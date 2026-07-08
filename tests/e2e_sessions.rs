//! End-to-end tests for the full server/client session workflow (Task 9):
//! detach/reattach persistence, window create/rename/switch, an external
//! `kill-session` tearing down an attached client, plain CLI error paths,
//! and the fail-fast "no console" guard (Task 9 scope addition A).
//!
//! Harness: same pattern as `tests/e2e.rs` -- spawn the built `winmux.exe`
//! under a test-owned ConPTY (`winmux::pty::Pty`) and decode its output via
//! winmux's own `grid::Grid`; one-shot CLI commands (`ls`, `kill-session`,
//! `kill-server`) use a plain `std::process::Command` instead, since they
//! never call `Host::enter` and so need no console at all. Shared helpers
//! live in `tests/common/mod.rs`.
//!
//! IMPORTANT: these are real ConPTY + named-pipe integration tests. Every
//! test uses a unique `-L` socket (`common::unique_socket`) plus a
//! `ServerGuard` Drop teardown so a failing assertion never leaks a
//! detached server process. `cargo test` (default parallel) is safe by
//! construction; run `cargo test --test e2e --test e2e_sessions --
//! --test-threads=1` to serialize and rule out load-induced flakiness when
//! diagnosing a failure.

mod common;

use std::time::{Duration, Instant};

use common::{
    drain_after_exit, pump, pump_raw, process_exited, run_cli, screen_text, spawn_winmux_pty,
    unique_socket, wait_until, ServerGuard, COLS, ROWS,
};
use winmux::grid::Grid;

/// `e2e_detach_reattach_persists`: create a named session, produce visible
/// output in its pane, detach, verify the server-side `ls` view (no
/// `(attached)` suffix, still 1 window), then reattach a second client and
/// confirm the pane's prior output is still there.
#[test]
fn e2e_detach_reattach_persists() {
    let socket = unique_socket("detach-reattach");
    let _guard = ServerGuard { socket: socket.clone() };

    // Client A: `winmux -L <s> new -s work`.
    let (mut pty_a, proc_a, rx_a) = spawn_winmux_pty(&["-L", &socket, "new", "-s", "work"]);
    let mut grid_a = Grid::new(COLS, ROWS, 0);
    let mut raw_a: Vec<u8> = Vec::new();

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid_a, &rx_a, &mut raw_a);
            screen_text(&grid_a).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared in session 'work'; screen:\n{}",
        screen_text(&grid_a).join("\n")
    );

    pty_a.write_input(b"echo persist-42\r").expect("send echo");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid_a, &rx_a, &mut raw_a);
            screen_text(&grid_a).iter().any(|l| l.contains("persist-42"))
        }),
        "echoed 'persist-42' never appeared; screen:\n{}",
        screen_text(&grid_a).join("\n")
    );

    // Detach: Ctrl-b d.
    pty_a.write_input(b"\x02d").expect("send detach");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid_a, &rx_a, &mut raw_a);
            process_exited(proc_a)
        }),
        "client A did not exit within 10s after detach; screen:\n{}",
        screen_text(&grid_a).join("\n")
    );
    drain_after_exit(&mut grid_a, &rx_a, &mut raw_a);

    // NOT `ends_with`: once winmux.exe itself has exited, ConPTY emits its own
    // trailing housekeeping bytes (buffer clear + cursor-home) into the same
    // stream, so the exact detach message is present but no longer the last
    // thing in `raw_a`. `contains` still pins the exact message text.
    let tail = String::from_utf8_lossy(&raw_a);
    assert!(
        tail.contains("[detached (from session work)]"),
        "client A's raw output did not contain the detach message; got:\n{tail:?}"
    );

    // Server-side `ls` (plain Command, no ConPTY): still 1 window, no
    // trailing "(attached)" now that the only client has detached.
    let out = run_cli(&socket, &["ls"]);
    assert!(out.status.success(), "ls failed after detach: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().unwrap_or("");
    assert!(
        line.starts_with("work: 1 windows (created "),
        "unexpected ls line: {line:?}"
    );
    assert!(!line.contains("(attached)"), "ls line still shows attached: {line:?}");

    // Client B: `winmux -L <s> attach -t work` -- the prior output persists.
    let (mut pty_b, proc_b, rx_b) = spawn_winmux_pty(&["-L", &socket, "attach", "-t", "work"]);
    let mut grid_b = Grid::new(COLS, ROWS, 0);
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid_b, &rx_b);
            screen_text(&grid_b).iter().any(|l| l.contains("persist-42"))
        }),
        "reattached client B did not show persisted 'persist-42'; screen:\n{}",
        screen_text(&grid_b).join("\n")
    );

    // Best-effort cleanup: detach B so it doesn't linger past the test.
    let _ = pty_b.write_input(b"\x02d");
    let _ = wait_until(Instant::now() + Duration::from_secs(10), || {
        pump(&mut grid_b, &rx_b);
        process_exited(proc_b)
    });
}

/// `e2e_windows_roundtrip`: `Ctrl-b c` creates window 1 (status flags swap to
/// `0:powershell- 1:powershell*`), a rename commits on the *current* window
/// (1) before switching away, then `Ctrl-b p` moves back to window 0 and the
/// current/last flags swap again.
#[test]
fn e2e_windows_roundtrip() {
    let socket = unique_socket("windows-roundtrip");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, proc_raw, rx) = spawn_winmux_pty(&["-L", &socket]);
    let mut grid = Grid::new(COLS, ROWS, 0);

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:powershell*"))
        }),
        "initial window tab never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Ctrl-b c: new window -> "0:powershell- 1:powershell*".
    pty.write_input(b"\x02c").expect("send new-window");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:powershell- 1:powershell*"))
        }),
        "status row never showed '0:powershell- 1:powershell*' after Ctrl-b c; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Rename the CURRENT window (1) before switching away: Ctrl-b , then
    // clear the pre-filled "powershell" (10 chars) and type "edit".
    pty.write_input(b"\x02,").expect("send rename prompt");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("(rename-window) powershell"))
        }),
        "rename-window prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    pty.write_input(&[0x7f; 10]).expect("clear prefilled name");
    pty.write_input(b"edit\r").expect("commit rename");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:powershell- 1:edit*"))
        }),
        "status row never showed '0:powershell- 1:edit*' after rename; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Ctrl-b p: previous window -> flags swap.
    pty.write_input(b"\x02p").expect("send prev-window");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:powershell* 1:edit-"))
        }),
        "status row never showed '0:powershell* 1:edit-' after Ctrl-b p; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup.
    let _ = pty.write_input(b"\x02d");
    let _ = wait_until(Instant::now() + Duration::from_secs(10), || {
        pump(&mut grid, &rx);
        process_exited(proc_raw)
    });
}

/// `e2e_kill_session_exits_client`: an external `kill-session` (a separate,
/// non-ConPTY CLI connection) tears down the only session; the attached
/// client gets `Exit{0, "[exited]"}` and the server itself goes down
/// (exit-empty), so a subsequent `ls` reports no server at all.
#[test]
fn e2e_kill_session_exits_client() {
    let socket = unique_socket("kill-session");
    let _guard = ServerGuard { socket: socket.clone() };

    let (_pty, proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "new", "-s", "foo"]);
    let mut grid = Grid::new(COLS, ROWS, 0);
    let mut raw: Vec<u8> = Vec::new();

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // From outside: kill the session by name.
    let out = run_cli(&socket, &["kill-session", "-t", "foo"]);
    assert!(out.status.success(), "kill-session failed: {out:?}");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            process_exited(proc_raw)
        }),
        "client did not exit after external kill-session; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    drain_after_exit(&mut grid, &rx, &mut raw);

    // NOT `ends_with` -- see the matching note in `e2e_detach_reattach_persists`:
    // ConPTY appends its own trailing housekeeping bytes after the child
    // process (winmux.exe) has already exited.
    let tail = String::from_utf8_lossy(&raw);
    assert!(
        tail.contains("[exited]"),
        "client output did not contain '[exited]'; got:\n{tail:?}"
    );

    // exit-empty: the server had exactly one session, now zero -> it shuts
    // itself down, so `ls` against the same socket sees no server at all
    // (poll briefly: the server's own shutdown is not synchronous with the
    // kill-session reply reaching our CLI connection).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ls_out = run_cli(&socket, &["ls"]);
    assert!(
        wait_until(deadline, || {
            ls_out = run_cli(&socket, &["ls"]);
            !ls_out.status.success()
        }),
        "ls kept succeeding after kill-session (server did not shut down); {ls_out:?}"
    );
    let stderr = String::from_utf8_lossy(&ls_out.stderr);
    assert!(
        stderr.contains("no server running on"),
        "unexpected ls stderr after server shutdown: {stderr:?}"
    );
}

/// `e2e_no_server_error`: a plain CLI command against a socket nothing is
/// bound to fails fast with exit 1 and a `no server running on` stderr
/// message -- no ConPTY needed, `ls` never calls `Host::enter`.
#[test]
fn e2e_no_server_error() {
    let socket = format!("fresh-{}", std::process::id());
    let out = run_cli(&socket, &["ls"]);
    assert_eq!(out.status.code(), Some(1), "unexpected exit status: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no server running on"),
        "unexpected stderr: {stderr:?}"
    );
}

/// Task 9 scope addition B: with stdio fully redirected (no console at all,
/// NOT a ConPTY), a bare/`new-session` invocation must fail fast with exit 1
/// and an `open terminal failed` stderr message BEFORE it ever autostarts a
/// server -- otherwise redirected stdio (e.g. under a test harness, or a
/// misconfigured launcher) would leave an idle detached server process
/// behind forever (it never gets a session, so exit-empty never fires).
#[test]
fn no_console_fails_fast() {
    let socket = unique_socket("no-console");
    // Defensive: if the fail-fast guard regresses, don't leak a server.
    let _guard = ServerGuard { socket: socket.clone() };

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_winmux"))
        .args(["-L", &socket])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run winmux with redirected stdio");

    assert_eq!(out.status.code(), Some(1), "unexpected exit status: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("open terminal failed"),
        "unexpected stderr: {stderr:?}"
    );

    // No server left behind: the client must have exited before ever
    // autostarting one.
    let ls_out = run_cli(&socket, &["ls"]);
    assert!(!ls_out.status.success(), "server was left running: {ls_out:?}");
    let ls_stderr = String::from_utf8_lossy(&ls_out.stderr);
    assert!(
        ls_stderr.contains("no server running"),
        "unexpected ls stderr: {ls_stderr:?}"
    );
}
