//! Integration tests spawning disposable Windows child processes to verify:
//! - Priority Class change (`SetPriorityClass` / `GetPriorityClass`)
//! - Graceful Close (`EnumWindows` + `PostMessageW(WM_CLOSE)`)
//! - Guarded Terminate (`IsProcessCritical` + `CreateTime` + `TerminateProcess`)
//! - `CreateTime`-mismatch safe abort (PID reuse protection)

#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use btop_sys::{
    PriorityClass, ProcActionError, graceful_close_process, query_process_information,
    query_process_priority, set_process_priority, terminate_process,
};

const CHILD_MODE_ENV: &str = "BTOP_SYS_PROC_ACTION_CHILD_MODE";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn find_create_time(pid: u32) -> u64 {
    let mut scratch = Vec::with_capacity(256 * 1024);
    let procs = query_process_information(&mut scratch).expect("query processes");
    procs
        .into_iter()
        .find(|p| p.pid == pid)
        .expect("spawned child must appear in SystemProcessInformation")
        .create_time_100ns
}

fn spawn_helper_child(mode: &str) -> ChildGuard {
    let exe = std::env::current_exe().expect("current test executable");
    let child = Command::new(exe)
        .arg("--exact")
        .arg("child_helper_entrypoint")
        .arg("--nocapture")
        .env(CHILD_MODE_ENV, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn disposable test child");
    ChildGuard(child)
}

/// Self-invoked helper test used as the disposable child process for live action tests.
#[test]
fn child_helper_entrypoint() {
    let Ok(mode) = std::env::var(CHILD_MODE_ENV) else {
        return;
    };
    match mode.as_str() {
        "sleep" => {
            println!("BTOP_CHILD_READY");
            std::thread::sleep(Duration::from_secs(30));
        }
        "window" => run_closable_window_loop(),
        _ => {}
    }
}

fn run_closable_window_loop() {
    use core::ptr;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
        PostQuitMessage, RegisterClassW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WM_CLOSE,
        WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CLOSE => {
                // SAFETY: Valid HWND received from Win32 window procedure.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                0
            }
            WM_DESTROY => {
                // SAFETY: Terminates the helper thread's message loop cleanly with exit code 0.
                unsafe {
                    PostQuitMessage(0);
                }
                0
            }
            // SAFETY: Forwarding unhandled messages to standard default window procedure.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    let class_name: Vec<u16> = "BtopWindowsTestClosableClass\0".encode_utf16().collect();
    let title: Vec<u16> = "btop-windows-test-window\0".encode_utf16().collect();
    // SAFETY: Querying current module instance and registering a minimal top-level window class.
    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        let _ = RegisterClassW(&raw const wc);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            160,
            120,
            ptr::null_mut(),
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        );
        assert!(!hwnd.is_null(), "CreateWindowExW failed in test helper");
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        println!("BTOP_CHILD_READY");

        let mut msg: MSG = core::mem::zeroed();
        while GetMessageW(&raw mut msg, ptr::null_mut(), 0, 0) > 0 {
            let _ = TranslateMessage(&raw const msg);
            let _ = DispatchMessageW(&raw const msg);
        }
    }
}

fn wait_for_ready(child: &mut Child) -> BufReader<std::process::ChildStdout> {
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        if line.contains("BTOP_CHILD_READY") {
            return reader;
        }
        line.clear();
    }
    panic!("child exited before signaling BTOP_CHILD_READY");
}

#[test]
fn live_windows_priority_mismatch_and_terminate_on_disposable_child() {
    let mut child = spawn_helper_child("sleep");
    let _stdout = wait_for_ready(&mut child.0);
    let pid = child.0.id();
    let create_time = find_create_time(pid);

    // 1. CreateTime mismatch must abort cleanly across all actions without killing child.
    let wrong_create = create_time.saturating_add(1);
    assert_eq!(
        set_process_priority(pid, wrong_create, PriorityClass::BelowNormal),
        Err(ProcActionError::IdentityMismatch { pid })
    );
    assert_eq!(
        terminate_process(pid, wrong_create),
        Err(ProcActionError::IdentityMismatch { pid })
    );
    assert_eq!(
        graceful_close_process(pid, wrong_create),
        Err(ProcActionError::IdentityMismatch { pid })
    );
    assert!(
        child.0.try_wait().expect("try_wait").is_none(),
        "child must still be alive after CreateTime-mismatch abort"
    );

    // 2. Priority change with verified CreateTime succeeds and reads back cleanly.
    let updated = set_process_priority(pid, create_time, PriorityClass::BelowNormal)
        .expect("set BelowNormal priority");
    assert_eq!(updated.applied, PriorityClass::BelowNormal);
    assert_eq!(
        query_process_priority(pid, create_time).expect("query priority"),
        PriorityClass::BelowNormal
    );

    // 3. Guarded terminate with verified CreateTime cleanly terminates the disposable child.
    terminate_process(pid, create_time).expect("terminate disposable child");
    let status = child.0.wait().expect("wait terminated child");
    assert!(!status.success(), "terminated child should exit non-zero");
}

#[test]
fn live_windows_graceful_wm_close_exits_windowed_child_cleanly() {
    let mut child = spawn_helper_child("window");
    let _stdout = wait_for_ready(&mut child.0);
    let pid = child.0.id();
    let create_time = find_create_time(pid);

    let notified = graceful_close_process(pid, create_time).expect("post WM_CLOSE to child window");
    assert!(notified >= 1, "expected at least one top-level window");

    let deadline = Instant::now() + Duration::from_secs(5);
    let exited = loop {
        if let Some(status) = child.0.try_wait().expect("try_wait windowed child") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let status = exited.expect("windowed child should exit after WM_CLOSE");
    assert!(
        status.success(),
        "windowed child should exit cleanly (code 0) after WM_CLOSE, got {status}"
    );
}
