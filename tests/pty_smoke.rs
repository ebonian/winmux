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

/// A spawn that fails (nonexistent executable) must return Err promptly and
/// release everything created up to the failure point (pseudoconsole, pipe
/// handles, attribute list). True leak detection is not practical in-test;
/// looping 50 times at least proves no panic, hang, or handle/process
/// exhaustion, and the RAII restructure in `spawn` is verified by review.
#[test]
fn spawn_failure_does_not_hang_or_leak() {
    for i in 0..50 {
        let result = Pty::spawn("definitely-not-a-real-executable-xyz.exe", 80, 24);
        assert!(
            result.is_err(),
            "iteration {i}: spawning a nonexistent executable must fail"
        );
    }
}

/// **Platform gotcha, verified here (SP7 Task 19 closeout, 2026-07-11):**
/// Windows ConPTY does NOT relay a hosted process's `CSI ?1000h`-class
/// mouse-mode DECSET private-mode sequence through to the reader of the
/// pseudoconsole's output pipe -- it is silently CONSUMED (not garbled, not
/// delayed, just entirely absent from the byte stream we read), unlike an
/// ordinary SGR sequence (`CSI 31 m`), which survives byte-for-byte. This
/// was discovered investigating a closeout e2e test for application mouse
/// passthrough (follow-ups #35/#72): a real `powershell.exe` pane emitting
/// `ESC[?1000h` via `Write-Host` or `[Console]::Out.Write` (both tried, same
/// result) never produces that byte sequence in `Pty`'s raw output, so
/// `Grid::mouse_proto` (which parses exactly this sequence) can never
/// observe a real hosted app's mouse-mode request over ConPTY on this
/// platform -- the feature is correctly implemented in software (unit-tested
/// end to end from a synthetic byte feed) but structurally unobservable via
/// a real child process's own escape-sequence emission in this project's e2e
/// harness. This explains, with a concrete confirmed mechanism, three
/// previously-separate "investigated and gave up" notes in
/// `docs/follow-ups.md`: the SP4 abandonment of `alt_screen_wheel_
/// sends_arrows` (a synthetic `?1049h` alt-screen CSI), follow-up #35/#72's
/// "no live-process byte-receipt e2e proof was attempted", and follow-up
/// #53's bracketed-paste (`?2004h`) positive path being "found to be
/// fundamentally unprovable black-box" -- all three were hitting the SAME
/// ConPTY behavior, not three unrelated obstacles. See
/// `docs/follow-ups.md`'s SP7 Task 19 closeout section for the full writeup
/// and `CLAUDE.md`'s "Hard-won platform gotchas" section for the one-line
/// summary. This test pins the finding (and its SGR-survives control) so a
/// future ConPTY/Windows update that changes this behavior is caught, not
/// silently assumed to still hold.
#[test]
fn mouse_decset_private_mode_is_not_relayed_by_conpty() {
    let mut pty =
        Pty::spawn("powershell.exe -NoProfile -NoLogo", 80, 24).expect("spawn powershell through ConPTY");
    let mut reader = pty.take_reader().expect("take reader once");

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut all: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        while let Ok(chunk) = rx.try_recv() {
            all.extend(chunk);
        }
        if String::from_utf8_lossy(&all).contains("PS ") {
            break;
        }
        assert!(Instant::now() < deadline, "prompt never appeared: {:?}", String::from_utf8_lossy(&all));
        thread::sleep(Duration::from_millis(50));
    }

    // One command emits BOTH a mouse-mode DECSET and an ordinary SGR color
    // sequence, so a single capture proves the discrimination: SGR survives,
    // DECSET does not (ruling out "the reader just isn't working").
    // The done marker is built by concatenation ('MARKER'+'DONE') so the
    // literal MARKERDONE can only appear in EXECUTED output — PSReadLine
    // echoes the typed command line back through ConPTY, and on a slow
    // machine that echo arrives a poll cycle before execution output, so a
    // marker readable from the echo breaks the wait loop too early.
    pty.write_input(
        b"[Console]::Out.Write([char]27 + '[?1000h' + [char]27 + '[31mRED' + [char]27 + '[0m'); \
          Write-Host ('MARKER' + 'DONE')\r",
    )
    .expect("send probe command");

    let mut all2: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        while let Ok(chunk) = rx.try_recv() {
            all2.extend(chunk);
        }
        if all2.windows(b"MARKERDONE".len()).any(|w| w == b"MARKERDONE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "'MARKERDONE' never appeared; got:\n{}",
            String::from_utf8_lossy(&all2)
        );
        thread::sleep(Duration::from_millis(50));
    }

    assert!(
        all2.windows(b"\x1b[31mRED".len()).any(|w| w == b"\x1b[31mRED"),
        "control failed: an ordinary SGR sequence did not survive the ConPTY round trip either \
         (reader/harness problem, not the DECSET-specific finding this test pins); got:\n{}",
        String::from_utf8_lossy(&all2)
    );
    assert!(
        !all2.windows(b"\x1b[?1000h".len()).any(|w| w == b"\x1b[?1000h"),
        "CSI ?1000h now DOES survive the ConPTY round trip -- the platform gotcha this test pins \
         (docs/follow-ups.md's SP7 Task 19 closeout entry, CLAUDE.md's platform gotchas section) \
         may no longer hold; if a genuine Windows/ConPTY behavior change, update both docs and \
         reconsider whether the app-mouse-passthrough e2e gap (follow-ups #35/#72) can now be \
         closed for real; got:\n{}",
        String::from_utf8_lossy(&all2)
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
