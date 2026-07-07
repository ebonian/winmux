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
//!
//! ## Overlapped I/O (why, not just what)
//!
//! Every handle is opened with `FILE_FLAG_OVERLAPPED` and `Read`/`Write`
//! issue raw `ReadFile`/`WriteFile` calls against a per-`PipeConn` event
//! (bypassing `File`'s own synchronous read/write, which cannot be used on
//! a handle opened this way). This is required, not cosmetic: a
//! *non*-overlapped handle serializes ALL synchronous I/O against its
//! underlying file object, including across `try_clone`'d duplicates of the
//! same handle — a blocking `ReadFile` parked on one duplicate (e.g. a
//! per-client reader thread waiting for the next frame) stalls a
//! `WriteFile` on *another* duplicate of the very same connection (e.g. a
//! writer thread trying to send a reply) forever, even though the two
//! duplicates are different handle values. This was discovered as a real
//! deadlock while building `src/server.rs` (Task 6), whose architecture
//! requires exactly that shape (one reader thread + one writer thread per
//! client, each with its own cloned `PipeConn`). Making every handle
//! overlapped-capable at creation time — and giving every `PipeConn`
//! instance (including each `try_clone`) its own private auto-reset event —
//! lets duplicates proceed fully independently, while `Read`/`Write` still
//! present a plain *blocking* interface to callers (each call blocks via
//! `GetOverlappedResult(..., bWait: TRUE)` until its own operation
//! completes, never interfering with another duplicate's pending call).

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::sync::Mutex;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

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
    win32_code_error(raw_win32_code(&e))
}

/// Unwrap a `windows::core::Error`'s HRESULT back to the raw Win32 code (see
/// `win_err`'s doc comment) without constructing an `io::Error` — used to
/// compare against `ERROR_IO_PENDING`/`ERROR_PIPE_CONNECTED` etc. before
/// deciding how to handle an overlapped-I/O result.
fn raw_win32_code(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

/// RAII wrapper for a manual-object (in practice auto-reset) event HANDLE
/// used as one `PipeConn`'s private overlapped-I/O completion signal. Each
/// `PipeConn` (including every `try_clone`) gets its OWN event — sharing one
/// across concurrently-used duplicates would let one operation's completion
/// spuriously wake a wait on a different, unrelated operation.
struct OwnedEvent(HANDLE);

// SAFETY: a Win32 event HANDLE is just an opaque kernel-object identifier —
// safe to move to another thread (the same pattern `src/pty.rs` relies on
// for its process HANDLE). `PipeConn` is moved between threads, never
// shared by reference, so `Send` (not `Sync`) is all that's needed.
unsafe impl Send for OwnedEvent {}

impl OwnedEvent {
    /// Auto-reset (so a `GetOverlappedResult` wait implicitly clears it),
    /// initially unsignaled, unnamed.
    fn new() -> io::Result<Self> {
        unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map(OwnedEvent)
            .map_err(win_err)
    }
}

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Create one named-pipe server instance (duplex, byte mode, blocking from
/// the caller's point of view, overlapped-capable underneath).
///
/// `first` adds `FILE_FLAG_FIRST_PIPE_INSTANCE` — set ONLY by `bind`'s
/// initial instance. It makes the create fail with `ERROR_ACCESS_DENIED`
/// (surfacing as `io::ErrorKind::PermissionDenied`) if ANY instance of the
/// name already exists, i.e. if another server already owns the name.
/// Without it, `PIPE_UNLIMITED_INSTANCES` lets a second process's
/// `CreateNamedPipeW` on the same name SUCCEED, silently joining the first
/// server's instance pool — two servers would then round-robin accepts on
/// one name (split-brain). That is exactly the double-autostart race two
/// concurrent cold-start clients can produce: both spawn a server; the
/// loser's `bind` must FAIL so its process exits and the winner alone owns
/// the pipe. Subsequent `accept`-time instances must NOT set the flag (the
/// name legitimately exists then — it's the same server adding instances).
fn create_instance(wide_name: &[u16], first: bool) -> io::Result<HANDLE> {
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    unsafe {
        let handle = CreateNamedPipeW(
            PCWSTR(wide_name.as_ptr()),
            open_mode,
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
    /// Fails with `io::ErrorKind::PermissionDenied` (`ERROR_ACCESS_DENIED`,
    /// via `FILE_FLAG_FIRST_PIPE_INSTANCE`) if another process already owns
    /// the name — the losing side of a double-autostart race.
    pub fn bind(full_name: &str) -> io::Result<PipeListener> {
        let wide_name = to_wide(full_name);
        let handle = create_instance(&wide_name, true)?;
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
                    // NOT `first`: the name already exists (we own it) —
                    // this is the same server adding another instance.
                    let handle = create_instance(&self.wide_name, false)?;
                    unsafe { File::from_raw_handle(handle.0 as RawHandle) }
                }
            }
        };

        let handle = HANDLE(file.as_raw_handle());
        let event = OwnedEvent::new()?;
        let mut overlapped = OVERLAPPED { hEvent: event.0, ..Default::default() };
        unsafe {
            match ConnectNamedPipe(handle, Some(&mut overlapped)) {
                // A client may already have connected in the window between
                // this instance's creation and this call: reported
                // synchronously as ERROR_PIPE_CONNECTED, not a real failure.
                Ok(()) => {}
                Err(e) if raw_win32_code(&e) == ERROR_PIPE_CONNECTED.0 => {}
                Err(e) if raw_win32_code(&e) == ERROR_IO_PENDING.0 => {
                    let mut transferred = 0u32;
                    GetOverlappedResult(handle, &overlapped, &mut transferred, true).map_err(win_err)?;
                }
                Err(e) => return Err(win_err(e)),
            }
        }
        Ok(PipeConn { file, event })
    }
}

/// One end of a named-pipe connection (client or an accepted server side).
pub struct PipeConn {
    file: File,
    /// This instance's private overlapped-I/O completion event (see module
    /// docs). Not shared with clones — `try_clone` creates a fresh one.
    event: OwnedEvent,
}

impl std::fmt::Debug for PipeConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeConn").field("file", &self.file).finish()
    }
}

impl PipeConn {
    /// Connect to an existing named pipe (`\\.\pipe\...`). A nonexistent
    /// pipe (no server bound to that name) maps to `ErrorKind::NotFound`.
    pub fn connect(full_name: &str) -> io::Result<PipeConn> {
        let wide_name = to_wide(full_name);
        let handle = open_with_retry(&wide_name)?;
        let file = unsafe { File::from_raw_handle(handle.0 as RawHandle) };
        let event = OwnedEvent::new()?;
        Ok(PipeConn { file, event })
    }

    /// Clone the underlying handle (for separate reader/writer threads).
    /// The clone gets its OWN overlapped-I/O event (see module docs) so it
    /// can be used fully concurrently with `self` and any other clone.
    pub fn try_clone(&self) -> io::Result<PipeConn> {
        Ok(PipeConn {
            file: self.file.try_clone()?,
            event: OwnedEvent::new()?,
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
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
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

/// True for the read-side "peer closed / not connected" family, mapped to a
/// clean `Ok(0)` EOF (matches `src/pty.rs`'s `ERROR_BROKEN_PIPE` convention).
fn is_pipe_closed(e: &windows::core::Error) -> bool {
    let code = raw_win32_code(e);
    code == ERROR_BROKEN_PIPE.0 || code == ERROR_PIPE_NOT_CONNECTED.0
}

impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let handle = HANDLE(self.file.as_raw_handle());
        let mut overlapped = OVERLAPPED { hEvent: self.event.0, ..Default::default() };
        let mut n: u32 = 0;
        unsafe {
            match ReadFile(handle, Some(buf), Some(&mut n), Some(&mut overlapped)) {
                Ok(()) => Ok(n as usize),
                Err(e) if raw_win32_code(&e) == ERROR_IO_PENDING.0 => {
                    let mut transferred = 0u32;
                    match GetOverlappedResult(handle, &overlapped, &mut transferred, true) {
                        Ok(()) => Ok(transferred as usize),
                        Err(e) if is_pipe_closed(&e) => Ok(0),
                        Err(e) => Err(win_err(e)),
                    }
                }
                Err(e) if is_pipe_closed(&e) => Ok(0),
                Err(e) => Err(win_err(e)),
            }
        }
    }
}

impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let handle = HANDLE(self.file.as_raw_handle());
        let mut overlapped = OVERLAPPED { hEvent: self.event.0, ..Default::default() };
        let mut n: u32 = 0;
        unsafe {
            match WriteFile(handle, Some(buf), Some(&mut n), Some(&mut overlapped)) {
                Ok(()) => Ok(n as usize),
                Err(e) if raw_win32_code(&e) == ERROR_IO_PENDING.0 => {
                    let mut transferred = 0u32;
                    GetOverlappedResult(handle, &overlapped, &mut transferred, true)
                        .map(|()| transferred as usize)
                        .map_err(win_err)
                }
                Err(e) => Err(win_err(e)),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Every write above is a direct, already-completed WriteFile/
        // GetOverlappedResult round trip — nothing is buffered in this
        // process to flush.
        Ok(())
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
