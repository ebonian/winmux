//! Host terminal control: raw mode, alt-screen, size queries, frame writes,
//! and guaranteed restoration on every exit path (Drop + panic hook).

use std::ffi::c_void;
use std::io;
use std::sync::Mutex;

use windows::Win32::Foundation::{ERROR_BROKEN_PIPE, HANDLE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, SetConsoleMode,
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN,
    ENABLE_EXTENDED_FLAGS, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// Map a `windows::core::Error` into a `std::io::Error`. The stored HRESULT is
/// passed as the raw OS error; on Windows `io::Error`'s Display formats HRESULTs
/// correctly via FormatMessageW.
fn win_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0)
}

/// Snapshot needed to restore the console. Stored as plain integers (not raw
/// HANDLE pointers, which are neither Send nor Sync) so it can live in a static.
struct RestoreState {
    stdin: isize,
    stdout: isize,
    stdin_mode: u32,
    stdout_mode: u32,
}

/// Populated by `Host::enter`; read by both `Drop` and the panic hook so both
/// perform the identical restoration.
static RESTORE: Mutex<Option<RestoreState>> = Mutex::new(None);

/// Best-effort, infallible, idempotent restoration. Leaves alt-screen, shows
/// cursor, resets SGR, then restores the saved console modes. Every error is
/// ignored — restoration must never fail or panic.
unsafe fn apply_restore(
    stdin: HANDLE,
    stdout: HANDLE,
    stdin_mode: CONSOLE_MODE,
    stdout_mode: CONSOLE_MODE,
) {
    // CSI ?1049l = leave alt screen, CSI ?25h = show cursor, CSI 0m = reset SGR.
    let seq = b"\x1b[?1049l\x1b[?25h\x1b[0m";
    let mut written: u32 = 0;
    let _ = WriteFile(stdout, Some(seq), Some(&mut written), None);
    let _ = SetConsoleMode(stdout, stdout_mode);
    let _ = SetConsoleMode(stdin, stdin_mode);
}

pub struct Host {
    stdin: HANDLE,
    stdout: HANDLE,
    saved_stdin: CONSOLE_MODE,
    saved_stdout: CONSOLE_MODE,
}

impl Host {
    pub fn enter() -> io::Result<Host> {
        unsafe {
            let stdin = GetStdHandle(STD_INPUT_HANDLE).map_err(win_err)?;
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE).map_err(win_err)?;

            // Save the current modes so Drop / panic hook can restore them.
            let mut saved_stdin = CONSOLE_MODE::default();
            let mut saved_stdout = CONSOLE_MODE::default();
            GetConsoleMode(stdin, &mut saved_stdin).map_err(win_err)?;
            GetConsoleMode(stdout, &mut saved_stdout).map_err(win_err)?;

            // stdout: keep existing bits, add VT processing + suppress the
            // implicit CR that ConHost inserts when the cursor is at the last
            // column and an LF is written (DISABLE_NEWLINE_AUTO_RETURN).
            let new_stdout =
                saved_stdout | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
            SetConsoleMode(stdout, new_stdout).map_err(win_err)?;

            // stdin: full raw mode. We set the mode from scratch (not OR'd onto
            // the old value), so ENABLE_LINE_INPUT, ENABLE_ECHO_INPUT,
            // ENABLE_PROCESSED_INPUT and ENABLE_QUICK_EDIT_MODE are all OFF.
            //
            // IMPORTANT: with ENABLE_PROCESSED_INPUT cleared, Ctrl-C does NOT
            // raise a CTRL_C_EVENT signal — it is delivered inline as the raw
            // byte 0x03 in the input stream, exactly like a tty in raw mode.
            // The input state machine forwards / handles 0x03 like any other byte.
            let new_stdin = ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS;
            SetConsoleMode(stdin, new_stdin).map_err(win_err)?;

            // Publish the restore snapshot for Drop and the panic hook.
            *RESTORE.lock().unwrap() = Some(RestoreState {
                stdin: stdin.0 as isize,
                stdout: stdout.0 as isize,
                stdin_mode: saved_stdin.0,
                stdout_mode: saved_stdout.0,
            });

            let mut host = Host { stdin, stdout, saved_stdin, saved_stdout };
            // Enter alt screen, clear it, home the cursor.
            host.write(b"\x1b[?1049h\x1b[2J\x1b[H")?;
            Ok(host)
        }
    }

    pub fn size(&self) -> io::Result<(u16, u16)> {
        unsafe {
            let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
            GetConsoleScreenBufferInfo(self.stdout, &mut info).map_err(win_err)?;
            // Use the visible window rect, not the buffer, so scrollback height
            // does not inflate the row count.
            let cols = (info.srWindow.Right - info.srWindow.Left + 1) as u16;
            let rows = (info.srWindow.Bottom - info.srWindow.Top + 1) as u16;
            Ok((cols, rows))
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        // Console handle writes are unbuffered (WriteFile goes straight to the
        // console driver), so there is no user-space buffer to flush.
        let mut offset = 0usize;
        while offset < bytes.len() {
            let mut written: u32 = 0;
            unsafe {
                WriteFile(self.stdout, Some(&bytes[offset..]), Some(&mut written), None)
                    .map_err(win_err)?;
            }
            if written == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "WriteFile wrote 0 bytes"));
            }
            offset += written as usize;
        }
        Ok(())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Infallible: apply_restore ignores every error internally.
        unsafe {
            apply_restore(self.stdin, self.stdout, self.saved_stdin, self.saved_stdout);
        }
    }
}

/// Install a panic hook that restores the console (identical to Drop) before
/// delegating to the previously-installed hook. Call once from `main()` before
/// `Host::enter`. Safe to call once; restoration is idempotent so overlap with
/// Drop is harmless.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(guard) = RESTORE.lock() {
            if let Some(r) = guard.as_ref() {
                unsafe {
                    apply_restore(
                        HANDLE(r.stdin as *mut c_void),
                        HANDLE(r.stdout as *mut c_void),
                        CONSOLE_MODE(r.stdin_mode),
                        CONSOLE_MODE(r.stdout_mode),
                    );
                }
            }
        }
        previous(info);
    }));
}

/// Blocking read of raw bytes from the console input handle, for the stdin
/// thread. Returns Ok(0) only when the handle is closed (EOF / broken pipe).
pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE).map_err(win_err)? };
    let mut read: u32 = 0;
    unsafe {
        match ReadFile(stdin, Some(buf), Some(&mut read), None) {
            Ok(()) => Ok(read as usize),
            // A closed input handle surfaces as ERROR_BROKEN_PIPE; treat as EOF.
            Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
            Err(e) => Err(win_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual smoke test — requires a real interactive console. Run with:
    ///   cargo test -p winmux --lib host::tests::manual_enter_and_restore -- --ignored
    /// Watch the terminal: it should enter alt-screen, print the size line, then
    /// restore cleanly (cursor visible, normal screen, echo back on).
    #[test]
    #[ignore = "manual: requires a real attached console; run with --ignored"]
    fn manual_enter_and_restore() {
        let mut host = Host::enter().expect("enter raw mode");
        let (cols, rows) = host.size().expect("query size");
        assert!(cols > 0 && rows > 0, "console reported a zero dimension");
        host.write(format!("winmux host smoke: {cols}x{rows}\r\n").as_bytes())
            .expect("write to host");
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(host); // restoration runs here
    }
}
