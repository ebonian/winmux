//! Shared e2e test harness: spawn the built `winmux.exe` under a test-owned
//! ConPTY (`winmux::pty::Pty`) and decode its output via `winmux::grid::Grid`
//! -- exactly the pattern `tests/e2e.rs` established (Task 8), now shared
//! with `tests/e2e_sessions.rs` (Task 9) so neither file duplicates it.
//!
//! IMPORTANT (Task 9 brief): these e2e tests spawn real ConPTY processes and
//! real named-pipe servers. Every test uses a unique `-L` socket name
//! (`unique_socket`) so `cargo test`'s default parallel runner never collides
//! two tests on the same pipe, but ConPTY/process load under full
//! parallelism can still make timing-sensitive assertions flaky under heavy
//! CI load -- when diagnosing flakiness, prefer:
//!   cargo test --test e2e --test e2e_sessions -- --test-threads=1
//! which serializes all e2e tests while keeping `cargo test` (no args, full
//! parallel) safe by construction (unique sockets + generous deadlines).

#![allow(dead_code)] // not every test file uses every helper

use std::io::Read;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use winmux::grid::Grid;
use winmux::pty::Pty;

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;

pub const COLS: u16 = 80;
pub const ROWS: u16 = 24;

/// Join each grid row's cell chars into a `String`, one entry per row.
pub fn screen_text(grid: &Grid) -> Vec<String> {
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
pub fn pump(grid: &mut Grid, rx: &Receiver<Vec<u8>>) {
    while let Ok(chunk) = rx.try_recv() {
        grid.feed(&chunk);
    }
}

/// Like [`pump`], but also appends every drained chunk (verbatim) to `raw` --
/// for tests that need to assert on the tail of the exact byte stream (e.g. a
/// detach/exit message printed to the restored normal screen AFTER the
/// client's `Host` is dropped), which the grid's cell matrix alone can't
/// distinguish from text drawn earlier inside the alt screen.
pub fn pump_raw(grid: &mut Grid, rx: &Receiver<Vec<u8>>, raw: &mut Vec<u8>) {
    while let Ok(chunk) = rx.try_recv() {
        raw.extend_from_slice(&chunk);
        grid.feed(&chunk);
    }
}

/// True if some interior column is a full column of `│` across the pane rows
/// (everything above the bottom status bar) -- i.e. a vertical split border.
pub fn has_vertical_border(grid: &Grid) -> bool {
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

/// Like [`has_vertical_border`], but returns the column index of the border
/// (rather than just whether one exists) -- used by mouse e2e tests (Task
/// 10) that need to click at a coordinate KNOWN to be on one side of a
/// vertical split, computed from the actual rendered layout rather than an
/// assumed 50/50 split column.
pub fn vertical_border_col(grid: &Grid) -> Option<u16> {
    let pane_rows = grid.rows().saturating_sub(1); // exclude status bar
    if pane_rows == 0 {
        return None;
    }
    for c in 1..grid.cols().saturating_sub(1) {
        if (0..pane_rows).all(|r| grid.cell(c, r).ch == '│') {
            return Some(c);
        }
    }
    None
}

/// Poll `cond` every 100ms until it is true or the deadline passes.
pub fn wait_until<F: FnMut() -> bool>(deadline: Instant, mut cond: F) -> bool {
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
pub fn process_exited(raw: isize) -> bool {
    // SAFETY: `raw` is winmux's live process HANDLE, owned by the still-alive
    // Pty; WaitForSingleObject with timeout 0 only queries its signaled state.
    unsafe { WaitForSingleObject(HANDLE(raw as *mut core::ffi::c_void), 0) == WAIT_OBJECT_0 }
}

/// Give a just-exited process's already-completed (but maybe not yet
/// drained-by-us) writes a little more time to show up on `rx` before
/// asserting on `raw`/`grid`. Every write in the exit path (console restore,
/// then the final message) is a synchronous `WriteFile` that completes
/// before the process actually terminates, so the bytes are already sitting
/// in the ConPTY pipe by the time `process_exited` is true -- this just
/// covers the reader thread's own scheduling latency.
///
/// Caution: ConPTY itself appends trailing housekeeping bytes (observed: a
/// buffer clear + cursor-home) to the SAME output stream once the hosted
/// process has fully exited, i.e. AFTER this drain runs. So the winmux
/// process's own final message is present in `raw` but is no longer
/// necessarily the last thing in it -- assert with `.contains(...)`, not
/// `.ends_with(...)`, on any text expected right before process exit.
pub fn drain_after_exit(grid: &mut Grid, rx: &Receiver<Vec<u8>>, raw: &mut Vec<u8>) {
    for _ in 0..10 {
        pump_raw(grid, rx, raw);
        thread::sleep(Duration::from_millis(50));
    }
}

/// Appends `-f -` (disable config loading) to `args` unless the caller
/// already passed an explicit `-f` -- a real `%USERPROFILE%\.tmux.conf` on a
/// dev/CI machine would otherwise contaminate every e2e test that doesn't
/// itself test config loading (custom prefix, styles, etc. from that file
/// would silently apply). Mirrors `tests/server_proto.rs`'s `start_server`,
/// which does the same isolation for its in-process server (Task 7 review
/// fix, commit bc04d45) -- this closes the same gap for the real-binary e2e
/// suites (`tests/e2e.rs`, `tests/e2e_sessions.rs`), which spawn
/// `winmux.exe` directly and never passed `-f` at all. Tests that DO want
/// config loading (`tests/e2e_config.rs`) already pass their own explicit
/// `-f <path>`/`-f -`, so they are left untouched by this default.
fn isolate_config<'a>(args: &[&'a str]) -> Vec<&'a str> {
    if args.contains(&"-f") {
        return args.to_vec();
    }
    let mut v = args.to_vec();
    v.push("-f");
    v.push("-");
    v
}

/// Spawn `winmux.exe` (`CARGO_BIN_EXE_winmux`) with `args` under a
/// `COLS`x`ROWS` ConPTY, plus a background reader thread pumping its output
/// into an mpsc channel -- ConPTY pipes don't reliably EOF (the original
/// `tests/e2e.rs` gotcha), so a blocking `read` loop on its own thread is
/// required rather than reading directly on the test thread. Config loading
/// is disabled by default (`-f -`, see `isolate_config`) unless `args`
/// already specifies `-f`.
pub fn spawn_winmux_pty(args: &[&str]) -> (Pty, isize, Receiver<Vec<u8>>) {
    let args = isolate_config(args);
    let mut cmdline = format!("\"{}\"", env!("CARGO_BIN_EXE_winmux"));
    for a in &args {
        cmdline.push(' ');
        cmdline.push_str(a);
    }
    let mut pty = Pty::spawn(&cmdline, COLS, ROWS).expect("spawn winmux under ConPTY");
    let proc_raw = pty.process_handle_raw();
    let mut reader = pty.take_reader().expect("take winmux output reader");

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

    (pty, proc_raw, rx)
}

/// Unique `-L` socket name for a test: `e2e-<test>-<pid>` -- distinct per
/// test NAME (not just pid) so tests within the same test binary never
/// collide when `cargo test`'s default parallel runner runs them
/// concurrently in the same process.
pub fn unique_socket(test_name: &str) -> String {
    format!("e2e-{test_name}-{}", std::process::id())
}

/// Kills any server left on `socket` when dropped (best-effort; tolerates
/// failure -- e.g. the test already killed it via `kill-server`/exit-empty,
/// or the server never actually started). winmux auto-starts a detached
/// server process on bare/`new-session` invocation; without this guard a
/// test failure partway through could leave that process running
/// indefinitely on a real named pipe.
pub struct ServerGuard {
    pub socket: String,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_winmux"))
            .args(["-L", &self.socket, "kill-server"])
            .status();
    }
}

/// Plain one-shot CLI invocation (`winmux -L <socket> <args...>`) -- NO
/// ConPTY. Commands like `ls`/`kill-session`/`kill-server` never call
/// `Host::enter`, so a plain `std::process::Command` with captured
/// stdout/stderr is sufficient and much cheaper than a ConPTY round trip.
/// Config loading is disabled by default (`-f -`, see `isolate_config`)
/// unless `args` already specifies `-f` -- most callers target an
/// already-running server (started via `spawn_winmux_pty`, itself isolated),
/// but some commands (`new-session`) autostart one, so this stays isolated
/// too rather than relying on caller discipline.
pub fn run_cli(socket: &str, args: &[&str]) -> std::process::Output {
    let args = isolate_config(args);
    std::process::Command::new(env!("CARGO_BIN_EXE_winmux"))
        .args(["-L", socket])
        .args(&args)
        .output()
        .expect("run winmux CLI one-shot")
}
