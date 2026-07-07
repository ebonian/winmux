//! End-to-end test: spawn the built winmux binary inside a ConPTY (using
//! winmux's OWN pty module), drive it via keystrokes, and assert on the
//! decoded screen by feeding its output into winmux's OWN grid emulator.
//!
//! Flow: wait for the status bar → split vertically (Ctrl-b %) → confirm a
//! border column appears → kill the new pane (Ctrl-b x, y) → confirm the
//! border disappears → `exit` the last shell → assert winmux exits cleanly.

use std::io::Read;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use winmux::grid::Grid;
use winmux::pty::Pty;

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Join each grid row's cell chars into a `String`, one entry per row.
fn screen_text(grid: &Grid) -> Vec<String> {
    let mut out = Vec::with_capacity(grid.rows() as usize);
    for r in 0..grid.rows() {
        let mut line = String::with_capacity(grid.cols() as usize);
        for c in 0..grid.cols() {
            line.push(grid.cell(c, r).ch);
        }
        out.push(line);
    }
    out
}

/// Drain all queued output chunks into the emulator.
fn pump(grid: &mut Grid, rx: &Receiver<Vec<u8>>) {
    while let Ok(chunk) = rx.try_recv() {
        grid.feed(&chunk);
    }
}

/// True if some interior column is a full column of `│` across the pane rows
/// (everything above the bottom status bar) — i.e. a vertical split border.
fn has_vertical_border(grid: &Grid) -> bool {
    let pane_rows = grid.rows().saturating_sub(1); // exclude status bar
    if pane_rows == 0 {
        return false;
    }
    for c in 1..grid.cols().saturating_sub(1) {
        if (0..pane_rows).all(|r| grid.cell(c, r).ch == '│') {
            return true;
        }
    }
    false
}

/// Poll `cond` every 100ms until it is true or the deadline passes.
fn wait_until<F: FnMut() -> bool>(deadline: Instant, mut cond: F) -> bool {
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Non-blocking check: has the process behind `raw` (an isize HANDLE) exited?
fn process_exited(raw: isize) -> bool {
    // SAFETY: `raw` is winmux's live process HANDLE, owned by the still-alive
    // Pty; WaitForSingleObject with timeout 0 only queries its signaled state.
    unsafe { WaitForSingleObject(HANDLE(raw as *mut core::ffi::c_void), 0) == WAIT_OBJECT_0 }
}

/// Kills any server left on `socket` when dropped (best-effort; tolerates
/// failure — e.g. the test already killed it via `kill-server`, or the
/// server never actually started). winmux now auto-starts a detached server
/// process on bare invocation (server/client split, sub-project 2); without
/// this guard a test failure partway through could leave that process
/// running indefinitely on a real pipe name.
struct ServerGuard {
    socket: String,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_winmux"))
            .args(["-L", &self.socket, "kill-server"])
            .status();
    }
}

#[test]
fn e2e_split_kill_exit() {
    // Unique -L socket per test run so no server outlives this test and no
    // two runs collide on the default pipe name.
    let socket = format!("e2e-mvp-{}", std::process::id());
    let _guard = ServerGuard { socket: socket.clone() };

    // Quote the exe path (it may contain spaces) for the ConPTY command line.
    let cmdline = format!("\"{}\" -L {}", env!("CARGO_BIN_EXE_winmux"), socket);
    let mut pty = Pty::spawn(&cmdline, COLS, ROWS).expect("spawn winmux under ConPTY");
    let proc_raw = pty.process_handle_raw();
    let mut reader = pty.take_reader().expect("take winmux output reader");

    // Reader thread → channel of raw output chunks (fed into the grid below).
    let (tx, rx) = channel::<Vec<u8>>();
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

    let mut grid = Grid::new(COLS, ROWS);

    // 1. Status bar appears. Bare `winmux` now attaches to an auto-named
    //    session ("0", the lowest unused non-negative integer) instead of
    //    the MVP's hardcoded "[winmux]" — check for the window tab instead,
    //    which is stable regardless of the session name.
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:powershell*"))
        }),
        "status-bar window tab '0:powershell*' never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 1b. The pane's PowerShell prompt appears and PSReadLine loaded. winmux
    //     itself runs under THIS test's ConPTY, so winmux's own stdio IS a
    //     console and its panes take the no-STARTF_USESTDHANDLES path — the
    //     same path interactive use takes. If spawn nulled the pane's std
    //     handles, PSReadLine would fail its GetStdHandle probe and print
    //     "Cannot load PSReadline module. Console is running without
    //     PSReadline."
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt 'PS ' never appeared in the pane; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    assert!(
        !screen_text(&grid)
            .iter()
            .any(|l| l.to_lowercase().contains("psreadline")),
        "pane PowerShell reported a PSReadline failure; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 2. Split vertically: Ctrl-b %  → a `│` border column appears.
    pty.write_input(b"\x02%").expect("send split");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            has_vertical_border(&grid)
        }),
        "vertical split border '│' never appeared after Ctrl-b %; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 3. Kill the new (focused) pane: Ctrl-b x → wait for the confirm prompt →
    //    y. Waiting for the prompt guarantees winmux armed confirm mode before
    //    the `y` arrives, so `y` is consumed as confirmation, not forwarded.
    pty.write_input(b"\x02x").expect("send kill request");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("kill-pane"))
        }),
        "kill-pane confirm prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    pty.write_input(b"y").expect("send confirm");

    // 4. Border disappears once the pane is gone.
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            !has_vertical_border(&grid)
        }),
        "vertical split border '│' never disappeared after kill; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 5. Exit the last remaining shell → winmux exits cleanly.
    pty.write_input(b"exit\r").expect("send exit");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx); // keep the failure screen-dump current
            process_exited(proc_raw)
        }),
        "winmux process did not exit within 15s after 'exit'; screen:\n{}",
        screen_text(&grid).join("\n")
    );
}
