//! Integration smoke tests for the ConPTY wrapper. These spawn a real child
//! process, so they only run on Windows with ConPTY available (build 26200 has it).

use std::ffi::c_void;
use std::io::Read;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use winmux::pty::Pty;

// verify these import paths against windows 0.58 when compiling:
// WAIT_OBJECT_0 & HANDLE live in Win32::Foundation; WaitForSingleObject in
// Win32::System::Threading.
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;

/// Output written by the child must flow through the pseudoconsole to our reader.
#[test]
fn echo_output_flows_through_conpty() {
    let mut pty = Pty::spawn("cmd.exe /c echo winmux-smoke", 80, 24)
        .expect("spawn cmd.exe through ConPTY");
    let mut reader = pty.take_reader().expect("take reader once");

    // Read on a dedicated thread and stream chunks back; ConPTY's output pipe
    // does NOT reliably EOF when the child exits, so we must NOT wait for Ok(0).
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF once the main thread drops `pty`
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Collect until we observe the marker or hit a 10s deadline.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut collected: Vec<u8> = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        match rx.recv_timeout(remaining) {
            Ok(chunk) => {
                collected.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&collected).contains("winmux-smoke") {
                    // Success: dropping `pty` here closes the pseudoconsole,
                    // unblocking the reader thread so it can exit cleanly.
                    return;
                }
            }
            Err(_) => break, // timeout or sender gone
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    panic!(
        "did not observe 'winmux-smoke' in ConPTY output within 10s; got:\n{}",
        String::from_utf8_lossy(&collected)
    );
}

/// The exit-waiter protocol: a child that exits immediately must signal its
/// process handle so a waiter thread's WaitForSingleObject returns.
#[test]
fn child_exit_is_observable_via_wait() {
    let pty = Pty::spawn("cmd.exe /c exit 0", 80, 24).expect("spawn cmd.exe");
    let raw = pty.process_handle_raw();
    let status = unsafe { WaitForSingleObject(HANDLE(raw as *mut c_void), 10_000) };
    assert_eq!(
        status, WAIT_OBJECT_0,
        "process handle did not signal exit within 10s (got {status:?})"
    );
    // `pty` drops here: TerminateProcess on an already-dead process is a no-op,
    // ClosePseudoConsole + handle closes are harmless.
}
