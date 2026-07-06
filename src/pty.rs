//! ConPTY wrapper: create pipes + pseudoconsole, spawn a child under it,
//! read/write its VT stream, resize it, and tear everything down on Drop.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_BROKEN_PIPE, HANDLE};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    TerminateProcess, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// Not exported as a named constant by windows 0.58; define it ourselves.
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;

/// Map a `windows::core::Error` into a `std::io::Error` (HRESULT as raw OS error).
fn win_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0)
}

pub struct Pty {
    hpcon: HPCON,
    process: HANDLE,
    pid: u32,
    /// Write end of the input pipe (our stdout -> child stdin). `File` is Send
    /// and closes the handle on drop.
    input: File,
    /// Read end of the output pipe (child stdout -> us). Moved out by
    /// `take_reader`; `None` afterwards.
    reader: Option<File>,
}

pub struct PtyReader {
    file: File,
}

impl Pty {
    pub fn spawn(cmdline: &str, cols: u16, rows: u16) -> io::Result<Pty> {
        unsafe {
            // 1. Two anonymous pipes. Child stdin = in_read; we write in_write.
            //    Child stdout = out_write; we read out_read.
            let mut in_read = HANDLE::default();
            let mut in_write = HANDLE::default();
            let mut out_read = HANDLE::default();
            let mut out_write = HANDLE::default();
            CreatePipe(&mut in_read, &mut in_write, None, 0).map_err(win_err)?;
            CreatePipe(&mut out_read, &mut out_write, None, 0).map_err(win_err)?;

            // 2. Create the pseudoconsole from the child's pipe ends.
            let size = COORD { X: cols as i16, Y: rows as i16 };
            let hpcon: HPCON =
                CreatePseudoConsole(size, in_read, out_write, 0).map_err(win_err)?;

            // 3. ConPTY now owns duplicates of in_read + out_write; close our
            //    local copies. We keep in_write (to child stdin) and out_read
            //    (from child stdout).
            let _ = CloseHandle(in_read);
            let _ = CloseHandle(out_write);

            // 4. Size the process/thread attribute list (two-call pattern: the
            //    first call is EXPECTED to fail with ERROR_INSUFFICIENT_BUFFER
            //    and only fills in `bytes_required`).
            let mut bytes_required: usize = 0;
            let _ = InitializeProcThreadAttributeList(
                LPPROC_THREAD_ATTRIBUTE_LIST(ptr::null_mut()),
                1,
                0,
                &mut bytes_required,
            );
            let mut attr_buf: Vec<u8> = vec![0u8; bytes_required];
            let attr_list =
                LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
            InitializeProcThreadAttributeList(attr_list, 1, 0, &mut bytes_required)
                .map_err(win_err)?;

            // 5. Attach the pseudoconsole to the attribute list.
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                Some(hpcon.0 as *const c_void),
                std::mem::size_of::<HPCON>(),
                None,
                None,
            )
            .map_err(win_err)?;

            // 6. STARTUPINFOEXW with cb = size of the extended struct and the
            //    attribute list attached.
            //
            //    IMPORTANT (deviation from the brief): also set
            //    STARTF_USESTDHANDLES with the std handle fields left null/zero
            //    (the default). Without this flag, when OUR OWN process's
            //    standard handles are themselves redirected (e.g. under a test
            //    harness, or `cargo test | tee`), Windows' legacy CreateProcess
            //    behavior duplicates those redirected handles into the child's
            //    standard handles even though `bInheritHandles` is FALSE and even
            //    though a pseudoconsole is attached via the proc-thread
            //    attribute. That causes the child's stdout to bypass the
            //    pseudoconsole pipes entirely and write to wherever our own
            //    stdout was redirected. Setting STARTF_USESTDHANDLES with null
            //    handles suppresses that legacy duplication so the child relies
            //    solely on the ConPTY attribute for its console. See
            //    https://github.com/microsoft/terminal/discussions/15814.
            let mut si_ex = STARTUPINFOEXW::default();
            si_ex.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            si_ex.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
            si_ex.lpAttributeList = attr_list;

            // 7. CreateProcessW may write to the command-line buffer, so it must
            //    be a mutable, NUL-terminated UTF-16 Vec wrapped as PWSTR.
            let mut cmd_utf16: Vec<u16> =
                cmdline.encode_utf16().chain(std::iter::once(0)).collect();

            let mut pi = PROCESS_INFORMATION::default();
            CreateProcessW(
                PCWSTR::null(),
                PWSTR(cmd_utf16.as_mut_ptr()),
                None,                          // process security attributes
                None,                          // thread security attributes
                false,                         // bInheritHandles
                EXTENDED_STARTUPINFO_PRESENT,  // dwCreationFlags
                None,                          // environment
                PCWSTR::null(),                // current directory
                &si_ex.StartupInfo,            // *const STARTUPINFOW (first field)
                &mut pi,
            )
            .map_err(win_err)?;

            // 8. Free the attribute list; close the child's thread handle (we do
            //    not need it) but KEEP the process handle for waiting.
            DeleteProcThreadAttributeList(attr_list);
            let _ = CloseHandle(pi.hThread);

            // 9. Wrap our pipe ends as `std::fs::File` (Send, RAII-closing) for
            //    cross-thread blocking I/O. HANDLE.0 is *mut c_void == RawHandle.
            let input = File::from_raw_handle(in_write.0 as RawHandle);
            let reader = File::from_raw_handle(out_read.0 as RawHandle);

            Ok(Pty {
                hpcon,
                process: pi.hProcess,
                pid: pi.dwProcessId,
                input,
                reader: Some(reader),
            })
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let size = COORD { X: cols as i16, Y: rows as i16 };
        unsafe { ResizePseudoConsole(self.hpcon, size).map_err(win_err) }
    }

    pub fn take_reader(&mut self) -> io::Result<PtyReader> {
        match self.reader.take() {
            Some(file) => Ok(PtyReader { file }),
            None => Err(io::Error::other("pty reader already taken")),
        }
    }

    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.input.write_all(bytes)?;
        self.input.flush()
    }

    /// Raw process HANDLE value for a waiter thread. The Pty retains ownership;
    /// the waiter only reads/waits on it (safe cross-thread on Windows).
    pub fn process_handle_raw(&self) -> isize {
        self.process.0 as isize
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buf) {
            Ok(n) => Ok(n),
            // ConPTY closes the output pipe on teardown; std surfaces the real
            // Win32 code (109) as raw_os_error. Map ERROR_BROKEN_PIPE to EOF.
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE.0 as i32) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Order matters: kill the child first, then close the pseudoconsole
        // (which unblocks any reader stuck in ReadFile on the output pipe), then
        // close the process handle. The `input` File field closes the input pipe
        // when it is dropped after this body runs. All errors are ignored.
        unsafe {
            let _ = TerminateProcess(self.process, 0);
            ClosePseudoConsole(self.hpcon);
            let _ = CloseHandle(self.process);
        }
    }
}
