//! Windows Terminal VT console backend with guaranteed restoration on normal
//! exit, `Ctrl+C`, `CTRL_CLOSE_EVENT`, and panics (`panic = "unwind"`).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TermError {
    #[error(
        "virtual terminal mode is not supported by this console; run inside a modern UTF-8 terminal"
    )]
    VtUnsupported,
    #[error("console handle error: {0}")]
    Console(String),
    #[error("input error: {0}")]
    Input(String),
    #[error("a terminal guard is already active")]
    AlreadyActive,
    #[error("live terminal backend is not supported on this platform")]
    UnsupportedPlatform,
}

/// Escape sequence to enter alternate screen, hide cursor, and enable SGR mouse and focus tracking.
pub const ENTER_SEQUENCE: &str = "\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[?1004h";
/// Escape sequence to disable focus and mouse tracking, reset SGR, show cursor, and leave alternate screen.
pub const LEAVE_SEQUENCE: &str = "\x1b[?1004l\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l";

#[cfg(windows)]
mod win {
    use std::ptr;
    use std::sync::{Mutex, Once};

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS,
        ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE,
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
        GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
        SetConsoleOutputCP, WriteConsoleW,
    };

    use super::{ENTER_SEQUENCE, LEAVE_SEQUENCE, TermError};
    use crate::Size;

    const CP_UTF8: u32 = 65001;

    /// Computes raw Win32 console input mode for `crossterm::event::read()`.
    ///
    /// Clears `ENABLE_VIRTUAL_TERMINAL_INPUT` so `ConPTY` delivers native `FOCUS_EVENT_RECORD`
    /// and key/mouse records rather than queuing raw VT focus sequences (`ESC [ I` / `ESC [ O`)
    /// as printable keystrokes.
    const fn compute_input_mode(orig_in_mode: u32) -> u32 {
        (orig_in_mode
            & !(ENABLE_PROCESSED_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_QUICK_EDIT_MODE
                | ENABLE_VIRTUAL_TERMINAL_INPUT))
            | ENABLE_EXTENDED_FLAGS
            | ENABLE_MOUSE_INPUT
            | ENABLE_WINDOW_INPUT
    }

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
            let mut slot = match ACTIVE_STATE.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if slot.is_some() {
                return Err(TermError::AlreadyActive);
            }

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

            let desired_in_mode = compute_input_mode(orig_in_mode);
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
            *slot = Some(SavedConsoleState {
                stdout_handle: stdout as usize,
                stdin_handle: stdin as usize,
                orig_out_mode,
                orig_in_mode,
                orig_output_cp,
            });
            drop(slot);

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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn second_enter_is_rejected_without_clobbering_saved_state() {
            let sentinel = SavedConsoleState {
                stdout_handle: 1,
                stdin_handle: 2,
                orig_out_mode: 0x1111,
                orig_in_mode: 0x2222,
                orig_output_cp: 437,
            };
            {
                let mut slot = ACTIVE_STATE.lock().expect("lock active state");
                *slot = Some(sentinel);
            }

            let err = TerminalGuard::enter().expect_err("second guard must fail");
            assert!(matches!(err, TermError::AlreadyActive));

            let saved = ACTIVE_STATE
                .lock()
                .expect("lock active state")
                .take()
                .expect("original saved state must remain intact");
            assert_eq!(saved.orig_out_mode, 0x1111);
            assert_eq!(saved.orig_in_mode, 0x2222);
            assert_eq!(saved.orig_output_cp, 437);
        }

        #[test]
        fn compute_input_mode_clears_virtual_terminal_input_and_enables_raw_events() {
            let inherited = ENABLE_PROCESSED_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_QUICK_EDIT_MODE
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            let mode = compute_input_mode(inherited);
            assert_eq!(mode & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
            assert_eq!(mode & ENABLE_PROCESSED_INPUT, 0);
            assert_eq!(mode & ENABLE_LINE_INPUT, 0);
            assert_eq!(mode & ENABLE_ECHO_INPUT, 0);
            assert_eq!(mode & ENABLE_QUICK_EDIT_MODE, 0);
            assert_ne!(mode & ENABLE_EXTENDED_FLAGS, 0);
            assert_ne!(mode & ENABLE_MOUSE_INPUT, 0);
            assert_ne!(mode & ENABLE_WINDOW_INPUT, 0);
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::cell::UnsafeCell;
    use std::io::{self, IsTerminal, Write as _};
    use std::mem::MaybeUninit;
    use std::sync::Once;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

    use super::{ENTER_SEQUENCE, LEAVE_SEQUENCE, TermError};
    use crate::Size;

    const STATE_INACTIVE: u8 = 0;
    const STATE_ENTERING: u8 = 1;
    const STATE_ACTIVE: u8 = 2;
    const STATE_RESTORING: u8 = 3;
    const STATE_TERMINATED: u8 = 4;

    /// Sync wrapper for saved POSIX `termios`, written only while holding exclusive
    /// `STATE_ENTERING` and read only in `STATE_ACTIVE`, `STATE_RESTORING`, or
    /// `STATE_TERMINATED` (which permanently prevents any future `enter()` write).
    struct SavedTermiosSlot(UnsafeCell<MaybeUninit<libc::termios>>);

    // SAFETY: `STATE` enforces strict single-writer ownership during `STATE_ENTERING`
    // and immutable read-only access in `STATE_ACTIVE`, `STATE_RESTORING`, and
    // `STATE_TERMINATED` (which never transitions back to `STATE_INACTIVE`).
    unsafe impl Sync for SavedTermiosSlot {}

    static STATE: AtomicU8 = AtomicU8::new(STATE_INACTIVE);
    static TERMIOS_SAVED: AtomicBool = AtomicBool::new(false);
    static SAVED_TERMIOS: SavedTermiosSlot =
        SavedTermiosSlot(UnsafeCell::new(MaybeUninit::uninit()));
    static HOOKS_INSTALLED: Once = Once::new();

    fn write_all_stdout(text: &str) -> Result<(), TermError> {
        let mut out = io::stdout().lock();
        out.write_all(text.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|err| TermError::Console(format!("stdout write failed: {err}")))
    }

    /// Restores saved `termios` first (never blocks on a stalled PTY output queue), flushes any
    /// queued unsent frame bytes (`TCOFLUSH`) so `LEAVE_SEQUENCE` fits in the PTY buffer, then
    /// writes `LEAVE_SEQUENCE` if stdout becomes writable within a short bounded timeout.
    ///
    /// SAFETY: Uses only POSIX async-signal-safe functions (`tcsetattr`, `tcflush`, `poll`,
    /// `write`). Caller must ensure `SAVED_TERMIOS` is immutable (i.e. `STATE` is `ACTIVE`,
    /// `RESTORING`, or `TERMINATED`).
    unsafe fn restore_terminal_async_signal_safe() {
        // SAFETY: Caller guarantees `SAVED_TERMIOS` is immutable once `TERMIOS_SAVED` is true.
        unsafe {
            if TERMIOS_SAVED.load(Ordering::Acquire) {
                let saved = (&*SAVED_TERMIOS.0.get()).assume_init_ref();
                let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved);
            }
            // Discard stale frame output buffered in the PTY queue so the short `LEAVE_SEQUENCE`
            // can be written immediately without losing alt-screen / cursor restoration.
            let _ = libc::tcflush(libc::STDOUT_FILENO, libc::TCOFLUSH);
            let mut pfd = libc::pollfd {
                fd: libc::STDOUT_FILENO,
                events: libc::POLLOUT,
                revents: 0,
            };
            if libc::poll(&raw mut pfd, 1, 100) > 0 && (pfd.revents & libc::POLLOUT) != 0 {
                let bytes = LEAVE_SEQUENCE.as_bytes();
                let _ = libc::write(
                    libc::STDOUT_FILENO,
                    bytes.as_ptr().cast::<libc::c_void>(),
                    bytes.len(),
                );
            }
        }
    }

    /// RAII guard masking `SIGINT` and `SIGTERM` on the current thread across terminal setup so
    /// signals cannot race with `tcgetattr` / `enable_raw_mode` / `ENTER_SEQUENCE`.
    struct SigMaskGuard {
        prev: libc::sigset_t,
        active: bool,
    }

    impl SigMaskGuard {
        fn block_term_signals() -> Self {
            // SAFETY: Stack-allocated `sigset_t` structs initialized with `sigemptyset`.
            unsafe {
                let mut block: libc::sigset_t = std::mem::zeroed();
                let mut prev: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&raw mut block);
                libc::sigaddset(&raw mut block, libc::SIGINT);
                libc::sigaddset(&raw mut block, libc::SIGTERM);
                let ok =
                    libc::pthread_sigmask(libc::SIG_BLOCK, &raw const block, &raw mut prev) == 0;
                Self { prev, active: ok }
            }
        }
    }

    impl Drop for SigMaskGuard {
        fn drop(&mut self) {
            if self.active {
                // SAFETY: Restoring saved signal mask for the current thread.
                unsafe {
                    let _ = libc::pthread_sigmask(
                        libc::SIG_SETMASK,
                        &raw const self.prev,
                        std::ptr::null_mut(),
                    );
                }
            }
        }
    }

    /// Async-signal-safe terminal restore for `SIGINT` / `SIGTERM`, then re-raises default action.
    unsafe extern "C" fn unix_signal_handler(sig: libc::c_int) {
        let prev = STATE.swap(STATE_TERMINATED, Ordering::AcqRel);
        if prev == STATE_ACTIVE || prev == STATE_RESTORING {
            // SAFETY: `STATE_TERMINATED` permanently blocks `enter()` from ever writing
            // `SAVED_TERMIOS`, and `SAVED_TERMIOS` was fully initialized before `STATE_ACTIVE`.
            unsafe {
                restore_terminal_async_signal_safe();
            }
        }
        // SAFETY: Reset signal to default disposition and re-raise so exit status reflects signal.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = libc::SIG_DFL;
            sa.sa_flags = 0;
            libc::sigemptyset(&raw mut sa.sa_mask);
            let _ = libc::sigaction(sig, &raw const sa, std::ptr::null_mut());
            let _ = libc::raise(sig);
        }
    }

    fn install_global_restore_hooks() {
        HOOKS_INSTALLED.call_once(|| {
            let previous_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal_now();
                previous_hook(info);
            }));

            for sig in [libc::SIGINT, libc::SIGTERM] {
                // SAFETY: `unix_signal_handler` uses only async-signal-safe POSIX calls.
                unsafe {
                    let mut sa: libc::sigaction = std::mem::zeroed();
                    sa.sa_sigaction = unix_signal_handler as *const () as libc::sighandler_t;
                    sa.sa_flags = 0;
                    libc::sigemptyset(&raw mut sa.sa_mask);
                    let _ = libc::sigaction(sig, &raw const sa, std::ptr::null_mut());
                }
            }
        });
    }

    /// Restores the POSIX terminal if a `TerminalGuard` is currently active. Idempotent.
    pub fn restore_terminal_now() {
        if STATE
            .compare_exchange(
                STATE_ACTIVE,
                STATE_RESTORING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        let _ = write_all_stdout(LEAVE_SEQUENCE);
        if crossterm::terminal::disable_raw_mode().is_err() && TERMIOS_SAVED.load(Ordering::Acquire)
        {
            // SAFETY: `SAVED_TERMIOS` is immutable while in `STATE_RESTORING` or `STATE_TERMINATED`.
            unsafe {
                let saved = (&*SAVED_TERMIOS.0.get()).assume_init_ref();
                let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved);
            }
        }
        // Transition back to `STATE_INACTIVE` only if a signal did not latch `STATE_TERMINATED`.
        let _ = STATE.compare_exchange(
            STATE_RESTORING,
            STATE_INACTIVE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// RAII guard owning POSIX raw input mode, alternate screen, SGR mouse, and focus tracking.
    #[derive(Debug)]
    pub struct TerminalGuard {
        _private: (),
    }

    impl TerminalGuard {
        /// Enters POSIX raw mode and alternate screen, installing panic and signal restore hooks.
        pub fn enter() -> Result<Self, TermError> {
            if STATE
                .compare_exchange(
                    STATE_INACTIVE,
                    STATE_ENTERING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                return Err(TermError::AlreadyActive);
            }

            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                let _ = STATE.compare_exchange(
                    STATE_ENTERING,
                    STATE_INACTIVE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                return Err(TermError::VtUnsupported);
            }

            // Mask `SIGINT` and `SIGTERM` across the non-blocking ioctl setup (`tcgetattr`,
            // signal hook registration, and `enable_raw_mode`) so signals cannot race with mode
            // mutation, then unmask before writing `ENTER_SEQUENCE` so a full PTY never hangs
            // with termination signals blocked.
            {
                let _sig_mask = SigMaskGuard::block_term_signals();

                // Capture original termios while holding exclusive `STATE_ENTERING` before
                // installing signal hooks or mutating terminal modes.
                TERMIOS_SAVED.store(false, Ordering::Release);
                let mut orig: libc::termios = unsafe { std::mem::zeroed() };
                // SAFETY: `STDIN_FILENO` was just verified to be a terminal.
                if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &raw mut orig) } == 0 {
                    // SAFETY: `STATE_ENTERING` guarantees exclusive write access to `SAVED_TERMIOS`.
                    unsafe {
                        (*SAVED_TERMIOS.0.get()).write(orig);
                    }
                    TERMIOS_SAVED.store(true, Ordering::Release);
                }

                // Install panic and signal restore hooks and publish `STATE_ACTIVE` before
                // mutating raw mode so any signal after unmasking restores `termios`.
                install_global_restore_hooks();
                if STATE
                    .compare_exchange(
                        STATE_ENTERING,
                        STATE_ACTIVE,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    return Err(TermError::AlreadyActive);
                }

                if let Err(err) = crossterm::terminal::enable_raw_mode() {
                    restore_terminal_now();
                    return Err(TermError::Console(format!(
                        "failed to enable raw terminal mode: {err}"
                    )));
                }

                // Guard against a signal delivered to another thread while raw mode was applied.
                if STATE.load(Ordering::Acquire) == STATE_TERMINATED {
                    // SAFETY: `STATE_TERMINATED` guarantees `SAVED_TERMIOS` is immutable.
                    unsafe {
                        restore_terminal_async_signal_safe();
                    }
                    return Err(TermError::AlreadyActive);
                }
            }

            if let Err(err) = write_all_stdout(ENTER_SEQUENCE) {
                restore_terminal_now();
                return Err(err);
            }

            if STATE.load(Ordering::Acquire) == STATE_TERMINATED {
                // SAFETY: `STATE_TERMINATED` guarantees `SAVED_TERMIOS` is immutable.
                unsafe {
                    restore_terminal_async_signal_safe();
                }
                return Err(TermError::AlreadyActive);
            }

            Ok(Self { _private: () })
        }

        /// Queries current visible viewport dimensions from the POSIX terminal.
        pub fn size(&self) -> Result<Size, TermError> {
            let (width, height) = crossterm::terminal::size()
                .map_err(|err| TermError::Console(format!("terminal size query failed: {err}")))?;
            Ok(Size { width, height })
        }

        /// Writes an encoded ANSI/VT frame in a single stdout write + flush pass.
        pub fn write_frame(&mut self, frame: &str) -> Result<(), TermError> {
            write_all_stdout(frame)
        }
    }

    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            restore_terminal_now();
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Mutex;

        use super::*;

        static TEST_LOCK: Mutex<()> = Mutex::new(());

        #[test]
        fn second_enter_is_rejected_when_active_or_terminated() {
            let _guard = TEST_LOCK.lock().expect("lock unix backend tests");
            STATE.store(STATE_ACTIVE, Ordering::SeqCst);
            assert!(matches!(
                TerminalGuard::enter(),
                Err(TermError::AlreadyActive)
            ));

            STATE.store(STATE_TERMINATED, Ordering::SeqCst);
            assert!(matches!(
                TerminalGuard::enter(),
                Err(TermError::AlreadyActive)
            ));

            STATE.store(STATE_INACTIVE, Ordering::SeqCst);
        }

        #[test]
        fn restore_when_inactive_or_terminated_is_a_safe_noop() {
            let _guard = TEST_LOCK.lock().expect("lock unix backend tests");
            STATE.store(STATE_INACTIVE, Ordering::SeqCst);
            restore_terminal_now();
            assert_eq!(STATE.load(Ordering::Acquire), STATE_INACTIVE);

            STATE.store(STATE_TERMINATED, Ordering::SeqCst);
            restore_terminal_now();
            assert_eq!(STATE.load(Ordering::Acquire), STATE_TERMINATED);

            STATE.store(STATE_INACTIVE, Ordering::SeqCst);
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod stub {
    use super::TermError;
    use crate::Size;

    /// Fallback stub for unsupported non-Windows, non-Unix targets.
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

    impl Drop for TerminalGuard {
        fn drop(&mut self) {}
    }

    pub fn restore_terminal_now() {}
}

#[cfg(not(any(windows, unix)))]
pub use stub::{TerminalGuard, restore_terminal_now};
#[cfg(unix)]
pub use unix::{TerminalGuard, restore_terminal_now};
#[cfg(windows)]
pub use win::{TerminalGuard, restore_terminal_now};

#[cfg(test)]
mod sequence_tests {
    use super::{ENTER_SEQUENCE, LEAVE_SEQUENCE};

    #[test]
    fn enter_and_leave_sequences_toggle_alt_screen_cursor_mouse_and_focus() {
        for seq in [
            "\x1b[?1049h",
            "\x1b[?25l",
            "\x1b[?1000h",
            "\x1b[?1006h",
            "\x1b[?1004h",
        ] {
            assert!(
                ENTER_SEQUENCE.contains(seq),
                "ENTER_SEQUENCE missing {seq:?}"
            );
        }
        for seq in [
            "\x1b[?1004l",
            "\x1b[?1006l",
            "\x1b[?1000l",
            "\x1b[0m",
            "\x1b[?25h",
            "\x1b[?1049l",
        ] {
            assert!(
                LEAVE_SEQUENCE.contains(seq),
                "LEAVE_SEQUENCE missing {seq:?}"
            );
        }
    }
}
