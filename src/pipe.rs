//! Named-pipe transport (Windows). `PipeListener` is the server side:
//! `bind` creates and holds the first named-pipe instance so the name is
//! provably creatable before returning, and each `accept` blocks until a
//! client connects — the first `accept` uses the instance `bind` created,
//! later ones create fresh instances (`CreateNamedPipeW` with
//! `PIPE_UNLIMITED_INSTANCES`). `PipeConn` is a HANDLE wrapped in
//! `std::fs::File` for RAII close-on-drop, on either end of the pipe.
//!
//! Windows-API style mirrors `src/pty.rs`: HANDLEs are wrapped in `File` via
//! `FromRawHandle` immediately so ownership/closing is RAII, and read-side
//! pipe-closed errors are mapped to `Ok(0)` (EOF) the same way.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::sync::Mutex;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_BROKEN_PIPE, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::WindowsProgramming::GetUserNameW;

const BUFFER_SIZE: u32 = 64 * 1024;

/// Wait up to 2s for a busy pipe's next instance to free up before retrying
/// `CreateFileW` once (`ERROR_PIPE_BUSY` handling, per Win32 named-pipe
/// client convention).
const PIPE_BUSY_WAIT_MS: u32 = 2000;

/// Build the full pipe path for a given logical socket name:
/// `\\.\pipe\winmux-<username>-<socket_name>`. Both the username and the
/// socket name are sanitized to characters legal in a pipe name.
pub fn pipe_name(socket_name: &str) -> String {
    format!(
        r"\\.\pipe\winmux-{}-{}",
        sanitize(&current_username()),
        sanitize(socket_name)
    )
}

/// Keep only `[A-Za-z0-9_-]`; replace anything else with `_`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Current Windows username via `GetUserNameW`; `"unknown"` if unavailable.
fn current_username() -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        match GetUserNameW(PWSTR(buf.as_mut_ptr()), &mut len) {
            // `len` on success includes the trailing NUL.
            Ok(()) => {
                let end = (len as usize).saturating_sub(1).min(buf.len());
                String::from_utf16_lossy(&buf[..end])
            }
            Err(_) => "unknown".to_string(),
        }
    }
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for the `*W` APIs.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// windows-rs wraps a `GetLastError()` code from a failed call as an HRESULT
/// (`HRESULT_FROM_WIN32`: low 16 bits = code, facility WIN32 in bits 16-26,
/// sign bit set). `io::Error::kind()` classification on Windows matches
/// against the *raw* Win32 code, so undo that encoding rather than passing
/// the HRESULT bits straight through (which would silently produce
/// `ErrorKind::Uncategorized` instead of e.g. `NotFound`).
fn win32_code_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

fn win_err(e: windows::core::Error) -> io::Error {
    win32_code_error((e.code().0 as u32) & 0xFFFF)
}

/// Create one named-pipe server instance (duplex, byte mode, blocking).
fn create_instance(wide_name: &[u16]) -> io::Result<HANDLE> {
    unsafe {
        let handle = CreateNamedPipeW(
            PCWSTR(wide_name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            None,
        );
        if handle.is_invalid() {
            Err(win32_code_error(GetLastError().0))
        } else {
            Ok(handle)
        }
    }
}

/// Server side of a named pipe. Owns nothing between `accept` calls beyond
/// the wide-encoded name and (until first use) the instance created by
/// `bind`.
pub struct PipeListener {
    wide_name: Vec<u16>,
    /// The instance `bind` created and held, to prove the name is
    /// creatable. Taken by the first `accept`; later `accept`s create fresh
    /// instances instead.
    first: Mutex<Option<File>>,
}

impl PipeListener {
    /// Create and hold the first named-pipe instance for `full_name`
    /// (`\\.\pipe\...`), proving the name is bindable before returning.
    pub fn bind(full_name: &str) -> io::Result<PipeListener> {
        let wide_name = to_wide(full_name);
        let handle = create_instance(&wide_name)?;
        let first = unsafe { File::from_raw_handle(handle.0 as RawHandle) };
        Ok(PipeListener {
            wide_name,
            first: Mutex::new(Some(first)),
        })
    }

    /// Block until a client connects to a pipe instance, returning the
    /// connection. The first call reuses the instance `bind` created; every
    /// call after that creates a fresh instance first.
    pub fn accept(&self) -> io::Result<PipeConn> {
        let file = {
            let mut guard = self.first.lock().unwrap_or_else(|e| e.into_inner());
            match guard.take() {
                Some(file) => file,
                None => {
                    let handle = create_instance(&self.wide_name)?;
                    unsafe { File::from_raw_handle(handle.0 as RawHandle) }
                }
            }
        };

        let handle = HANDLE(file.as_raw_handle());
        unsafe {
            if let Err(e) = ConnectNamedPipe(handle, None) {
                let io_err = win_err(e);
                // A client may already have connected in the window between
                // this instance's creation and this ConnectNamedPipe call;
                // that races as ERROR_PIPE_CONNECTED, not a real failure.
                if io_err.raw_os_error() != Some(ERROR_PIPE_CONNECTED.0 as i32) {
                    return Err(io_err);
                }
            }
        }
        Ok(PipeConn { file })
    }
}

/// One end of a named-pipe connection (client or an accepted server side).
#[derive(Debug)]
pub struct PipeConn {
    file: File,
}

impl PipeConn {
    /// Connect to an existing named pipe (`\\.\pipe\...`). A nonexistent
    /// pipe (no server bound to that name) maps to `ErrorKind::NotFound`.
    pub fn connect(full_name: &str) -> io::Result<PipeConn> {
        let wide_name = to_wide(full_name);
        let handle = open_with_retry(&wide_name)?;
        let file = unsafe { File::from_raw_handle(handle.0 as RawHandle) };
        Ok(PipeConn { file })
    }

    /// Clone the underlying handle (for separate reader/writer threads).
    pub fn try_clone(&self) -> io::Result<PipeConn> {
        Ok(PipeConn {
            file: self.file.try_clone()?,
        })
    }
}

fn open_pipe(wide_name: &[u16]) -> io::Result<HANDLE> {
    unsafe {
        CreateFileW(
            PCWSTR(wide_name.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
        .map_err(win_err)
    }
}

/// `CreateFileW` against a named pipe whose instance limit is momentarily
/// exhausted fails with `ERROR_PIPE_BUSY`; wait for an instance to free up
/// and retry exactly once.
fn open_with_retry(wide_name: &[u16]) -> io::Result<HANDLE> {
    match open_pipe(wide_name) {
        Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
            unsafe {
                let _ = WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), PIPE_BUSY_WAIT_MS);
            }
            open_pipe(wide_name)
        }
        other => other,
    }
}

impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buf) {
            Ok(n) => Ok(n),
            // The peer closed its end of the pipe: treat like `pty.rs`'s
            // ERROR_BROKEN_PIPE handling and surface a clean EOF.
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(c) if c == ERROR_BROKEN_PIPE.0 as i32
                        || c == ERROR_PIPE_NOT_CONNECTED.0 as i32
                ) =>
            {
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}

impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_format() {
        assert_eq!(
            pipe_name("main"),
            format!(r"\\.\pipe\winmux-{}-main", sanitize(&current_username()))
        );
    }

    #[test]
    fn sanitize_keeps_allowed_chars() {
        assert_eq!(sanitize("abc-XYZ_123"), "abc-XYZ_123");
    }

    #[test]
    fn sanitize_replaces_illegal_chars() {
        assert_eq!(sanitize("a b\\c/d:e"), "a_b_c_d_e");
    }
}
