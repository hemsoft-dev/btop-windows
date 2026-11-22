//! Windows Terminal VT console backend with guaranteed restoration on normal
//! exit, `Ctrl+C`, `CTRL_CLOSE_EVENT`, and panics (`panic = "unwind"`).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TermError {
    #[error(
        "virtual terminal mode is not supported by this console; run btop-windows in Windows Terminal on Windows 11"
    )]
    VtUnsupported,
    #[error("console handle error: {0}")]
    Console(String),
    #[error("input error: {0}")]
    Input(String),
    #[error("live terminal backend is only supported on Windows")]
    UnsupportedPlatform,
}

/// Escape sequence to enter alternate screen, hide cursor, and enable SGR mouse tracking.
pub const ENTER_SEQUENCE: &str = "\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h";
/// Escape sequence to disable mouse tracking, reset SGR, show cursor, and leave alternate screen.
pub const LEAVE_SEQUENCE: &str = "\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l";

#[cfg(windows)]
mod win {
    use std::ptr;
    use std::sync::{Mutex, Once};

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS,
        ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT, GetConsoleMode,
        GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP,
        WriteConsoleW,
    };

    use super::{ENTER_SEQUENCE, LEAVE_SEQUENCE, TermError};
    use crate::Size;

    const CP_UTF8: u32 = 65001;

    #[derive(Debug, Clone, Copy)]
    struct SavedConsoleState {
        stdout_handle: usize,
        stdin_handle: usize,
        orig_out_mode: u32,
        orig_in_mode: u32,
        orig_output_cp: u32,
    }

    static ACTIVE_STATE: Mutex<Option<SavedConsoleState>> = Mutex::new(None);
    static HOOKS_INSTALLED: Once = Once::new();

    /// Restores the console if a `TerminalGuard` is currently active. Idempotent.
    pub fn restore_terminal_now() {
        let saved = match ACTIVE_STATE.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(state) = saved else {
            return;
        };

        let stdout = state.stdout_handle as HANDLE;
        let stdin = state.stdin_handle as HANDLE;
        let _ = write_wide(stdout, LEAVE_SEQUENCE);
        // SAFETY: Handles and saved modes were validated when entering `TerminalGuard`.
        unsafe {
            let _ = SetConsoleMode(stdout, state.orig_out_mode);
            let _ = SetConsoleMode(stdin, state.orig_in_mode);
            let _ = SetConsoleOutputCP(state.orig_output_cp);
        }
    }

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
        match ctrl_type {
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT | CTRL_C_EVENT => {
                restore_terminal_now();
                // Return 0 (FALSE) so the default handler finishes process termination
                // on window close / logoff / shutdown after we restored console state.
                0
            }
            _ => 0,
        }
    }

    fn install_global_restore_hooks() {
        HOOKS_INSTALLED.call_once(|| {
            let previous_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal_now();
                previous_hook(info);
            }));

            // SAFETY: `console_ctrl_handler` matches the Win32 `PHANDLER_ROUTINE` signature.
            unsafe {
                let _ = SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
            }
        });
    }

    fn write_wide(handle: HANDLE, text: &str) -> Result<(), TermError> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut offset = 0;
        while offset < wide.len() {
            let chunk = &wide[offset..];
            let Ok(chunk_len) = u32::try_from(chunk.len()) else {
                return Err(TermError::Console(
                    "frame too large for WriteConsoleW".to_owned(),
                ));
            };
            let mut written = 0u32;
            // SAFETY: `chunk` points to valid UTF-16 code units for `chunk_len` elements.
            let ok = unsafe {
                WriteConsoleW(
                    handle,
                    chunk.as_ptr(),
                    chunk_len,
                    &raw mut written,
                    ptr::null(),
                )
            };
            if ok == 0 {
                return Err(TermError::Console("WriteConsoleW failed".to_owned()));
            }
            if written == 0 {
                break;
            }
            offset += written as usize;
        }
        Ok(())
    }

    /// RAII guard owning Windows Terminal VT output and raw input modes.
    #[derive(Debug)]
    pub struct TerminalGuard {
        stdout: HANDLE,
    }

    impl TerminalGuard {
        /// Configures Windows Terminal VT output, UTF-8 codepage, raw/mouse/resize input,
        /// alternate screen, and global panic/close restore hooks.
        pub fn enter() -> Result<Self, TermError> {
            // SAFETY: Retrieving standard Win32 console handles.
            let (stdout, stdin) = unsafe {
                (
                    GetStdHandle(STD_OUTPUT_HANDLE),
                    GetStdHandle(STD_INPUT_HANDLE),
                )
            };
            if stdout.is_null()
                || stdout == INVALID_HANDLE_VALUE
                || stdin.is_null()
                || stdin == INVALID_HANDLE_VALUE
            {
                return Err(TermError::Console(
                    "standard input/output is not attached to a console".to_owned(),
                ));
            }

            let mut orig_out_mode = 0u32;
            let mut orig_in_mode = 0u32;
            // SAFETY: Querying valid console handles.
            let (out_ok, in_ok, orig_output_cp) = unsafe {
                (
                    GetConsoleMode(stdout, &raw mut orig_out_mode),
                    GetConsoleMode(stdin, &raw mut orig_in_mode),
                    GetConsoleOutputCP(),
                )
            };
            if out_ok == 0 || in_ok == 0 {
                return Err(TermError::VtUnsupported);
            }

            let desired_out_mode =
                orig_out_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
            // SAFETY: Enabling VT processing on the validated stdout console handle.
            if unsafe { SetConsoleMode(stdout, desired_out_mode) } == 0 {
                return Err(TermError::VtUnsupported);
            }

            // SAFETY: Setting UTF-8 output codepage on the console.
            if unsafe { SetConsoleOutputCP(CP_UTF8) } == 0 {
                unsafe {
                    let _ = SetConsoleMode(stdout, orig_out_mode);
                }
                return Err(TermError::Console(
                    "failed to set console output codepage to UTF-8".to_owned(),
                ));
            }

            let desired_in_mode = (orig_in_mode
                & !(ENABLE_PROCESSED_INPUT
                    | ENABLE_LINE_INPUT
                    | ENABLE_ECHO_INPUT
                    | ENABLE_QUICK_EDIT_MODE))
                | ENABLE_EXTENDED_FLAGS
                | ENABLE_MOUSE_INPUT
                | ENABLE_WINDOW_INPUT;
            // SAFETY: Configuring raw/mouse/window input on the validated stdin handle.
            if unsafe { SetConsoleMode(stdin, desired_in_mode) } == 0 {
                unsafe {
                    let _ = SetConsoleMode(stdout, orig_out_mode);
                    let _ = SetConsoleOutputCP(orig_output_cp);
                }
                return Err(TermError::Console(
                    "failed to configure console input mode".to_owned(),
                ));
            }

            install_global_restore_hooks();
            {
                let mut slot = match ACTIVE_STATE.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                *slot = Some(SavedConsoleState {
                    stdout_handle: stdout as usize,
                    stdin_handle: stdin as usize,
                    orig_out_mode,
                    orig_in_mode,
                    orig_output_cp,
                });
            }

            if let Err(err) = write_wide(stdout, ENTER_SEQUENCE) {
                restore_terminal_now();
                return Err(err);
            }

            Ok(Self { stdout })
        }

        /// Queries the current visible viewport dimensions from `srWindow`.
        pub fn size(&self) -> Result<Size, TermError> {
            // SAFETY: `CONSOLE_SCREEN_BUFFER_INFO` is plain-old-data and filled by Win32.
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
            let ok = unsafe { GetConsoleScreenBufferInfo(self.stdout, &raw mut info) };
            if ok == 0 {
                return Err(TermError::Console(
                    "GetConsoleScreenBufferInfo failed".to_owned(),
                ));
            }
            let width = u16::try_from(info.srWindow.Right - info.srWindow.Left + 1).unwrap_or(0);
            let height = u16::try_from(info.srWindow.Bottom - info.srWindow.Top + 1).unwrap_or(0);
            Ok(Size { width, height })
        }

        /// Writes an encoded ANSI/VT frame in a single console write pass.
        pub fn write_frame(&mut self, frame: &str) -> Result<(), TermError> {
            write_wide(self.stdout, frame)
        }
    }

    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            restore_terminal_now();
        }
    }
}

#[cfg(not(windows))]
mod stub {
    use super::TermError;
    use crate::Size;

    /// Non-Windows stub allowing portable crates and tests to compile on Linux.
    #[derive(Debug)]
    pub struct TerminalGuard;

    impl TerminalGuard {
        pub fn enter() -> Result<Self, TermError> {
            Err(TermError::UnsupportedPlatform)
        }

        pub fn size(&self) -> Result<Size, TermError> {
            Err(TermError::UnsupportedPlatform)
        }

        pub fn write_frame(&mut self, _frame: &str) -> Result<(), TermError> {
            Err(TermError::UnsupportedPlatform)
        }
    }

    pub fn restore_terminal_now() {}
}

#[cfg(not(windows))]
pub use stub::{TerminalGuard, restore_terminal_now};
#[cfg(windows)]
pub use win::{TerminalGuard, restore_terminal_now};
