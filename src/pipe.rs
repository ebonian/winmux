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

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::sync::Mutex;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_ALL, GENERIC_READ, GENERIC_WRITE,
    HANDLE,
};
use windows::Win32::Security::{
    AddAccessAllowedAce, InitializeAcl, InitializeSecurityDescriptor, SetSecurityDescriptorDacl,
    GetTokenInformation, TokenUser, ACL, ACL_REVISION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

const BUFFER_SIZE: u32 = 64 * 1024;

/// Wait up to 2s for a busy pipe's next instance to free up before retrying
/// `CreateFileW` once (`ERROR_PIPE_BUSY` handling, per Win32 named-pipe
/// client convention).
const PIPE_BUSY_WAIT_MS: u32 = 2000;

/// Windows' documented maximum username length (`UNLEN`, `lmcons.h`); the
/// buffer must be `UNLEN + 1` u16s to leave room for the trailing NUL
/// `GetUserNameW` writes even at the documented worst case (follow-up #8 —
/// the previous `256` was one `u16` short of that worst case; unreachable in
/// practice since real usernames are far shorter, but worth sizing to the
/// documented constant rather than a round number).
const UNLEN: usize = 256;

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
        let mut buf = [0u16; UNLEN + 1];
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

/// Owner-only DACL for `CreateNamedPipeW` (follow-up #15): grants the
/// CURRENT PROCESS's owner SID `GENERIC_ALL` and nothing else, in place of
/// relying on the platform default DACL. Low-risk even before this (the
/// default DACL grants Everyone read-only connect at most, and pipe names
/// are already per-username), but an explicit posture is more defensible.
/// Every buffer the built `SECURITY_DESCRIPTOR`/`ACL`/`SID` point into must
/// outlive the `CreateNamedPipeW` call that consumes `attrs()` — that's this
/// struct's only job; nothing here needs to survive past that one call.
struct OwnerDacl {
    _token_info: Vec<u8>,
    _acl: Vec<u8>,
    _sd: Vec<u8>,
    attrs: SECURITY_ATTRIBUTES,
}

impl OwnerDacl {
    fn build() -> io::Result<OwnerDacl> {
        unsafe {
            // 1. The current process's token, just long enough to query its
            //    owner SID.
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(win_err)?;
            let token_result: io::Result<Vec<u8>> = (|| {
                // Two-call pattern: the first call is EXPECTED to fail with
                // ERROR_INSUFFICIENT_BUFFER and only fills in the required
                // length.
                let mut len: u32 = 0;
                let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
                let mut buf = vec![0u8; len as usize];
                GetTokenInformation(
                    token,
                    TokenUser,
                    Some(buf.as_mut_ptr() as *mut c_void),
                    len,
                    &mut len,
                )
                .map_err(win_err)?;
                Ok(buf)
            })();
            let _ = CloseHandle(token);
            let token_info = token_result?;
            let sid = (*(token_info.as_ptr() as *const TOKEN_USER)).User.Sid;

            // 2. A one-ACE ACL: owner SID, GENERIC_ALL, nothing else. 256
            //    bytes comfortably covers the largest real SID plus the ACL/
            //    ACE headers.
            let mut acl = vec![0u8; 256];
            let acl_ptr = acl.as_mut_ptr() as *mut ACL;
            InitializeAcl(acl_ptr, acl.len() as u32, ACL_REVISION).map_err(win_err)?;
            AddAccessAllowedAce(acl_ptr, ACL_REVISION, GENERIC_ALL.0, sid).map_err(win_err)?;

            // 3. A self-relative-free SECURITY_DESCRIPTOR wrapping that ACL
            //    as its DACL (`bDaclPresent = TRUE`, not defaulted).
            let mut sd = vec![0u8; std::mem::size_of::<windows::Win32::Security::SECURITY_DESCRIPTOR>()];
            let psd = PSECURITY_DESCRIPTOR(sd.as_mut_ptr() as *mut c_void);
            InitializeSecurityDescriptor(psd, 1 /* SECURITY_DESCRIPTOR_REVISION */)
                .map_err(win_err)?;
            SetSecurityDescriptorDacl(psd, true, Some(acl_ptr as *const ACL), false)
                .map_err(win_err)?;

            let attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd.0,
                bInheritHandle: false.into(),
            };
            Ok(OwnerDacl { _token_info: token_info, _acl: acl, _sd: sd, attrs })
        }
    }

    fn attrs(&self) -> &SECURITY_ATTRIBUTES {
        &self.attrs
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
///
/// Every instance (this call, `first` or not) is created with an explicit
/// owner-only DACL (follow-up #15, `OwnerDacl`) rather than `None` (the
/// platform default) — see that type's doc comment.
fn create_instance(wide_name: &[u16], first: bool) -> io::Result<HANDLE> {
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let dacl = OwnerDacl::build()?;
    unsafe {
        let handle = CreateNamedPipeW(
            PCWSTR(wide_name.as_ptr()),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            Some(dacl.attrs() as *const SECURITY_ATTRIBUTES),
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
            // `handle` is always opened with `FILE_FLAG_OVERLAPPED`
            // (`create_instance`), and Win32's documented contract for
            // `ConnectNamedPipe` on an overlapped handle is that it NEVER
            // returns success synchronously — only `ERROR_IO_PENDING`
            // (connect still in flight) or `ERROR_PIPE_CONNECTED` (a client
            // raced in before this call) — so there is no `Ok(())` arm here
            // (follow-up #21: the old version had one, unreachable given this
            // module's exclusively-overlapped usage).
            match ConnectNamedPipe(handle, Some(&mut overlapped)) {
                // A client may already have connected in the window between
                // this instance's creation and this call: reported
                // synchronously as ERROR_PIPE_CONNECTED, not a real failure.
                Err(e) if raw_win32_code(&e) == ERROR_PIPE_CONNECTED.0 => {}
                Err(e) if raw_win32_code(&e) == ERROR_IO_PENDING.0 => {
                    let mut transferred = 0u32;
                    GetOverlappedResult(handle, &overlapped, &mut transferred, true).map_err(win_err)?;
                }
                Err(e) => return Err(win_err(e)),
                Ok(()) => unreachable!(
                    "ConnectNamedPipe returned Ok(()) synchronously on an overlapped handle"
                ),
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

    /// Follow-up #8: `current_username`'s buffer is sized to the documented
    /// `UNLEN + 1` (257 `u16`s — room for `UNLEN` characters plus the
    /// trailing NUL `GetUserNameW` writes), not an arbitrary round number.
    /// This is a compile-time/constant pin (the buffer size isn't otherwise
    /// observable from a real username, which is always far shorter) rather
    /// than a behavioral test.
    #[test]
    fn unlen_plus_one_is_257() {
        assert_eq!(UNLEN, 256);
        assert_eq!(UNLEN + 1, 257);
    }

    /// `current_username` never panics/truncates oddly regardless of the
    /// real environment's username length — a smoke check that the buffer
    /// resize didn't break the happy path.
    #[test]
    fn current_username_is_nonempty() {
        assert!(!current_username().is_empty());
    }
}
