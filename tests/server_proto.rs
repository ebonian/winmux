//! Headless server integration tests (`src/server.rs`): spawn `server::run`
//! on a background thread with a unique named pipe, speak raw protocol
//! frames over `PipeConn::connect` (no console, no ConPTY host process
//! needed for the CLIENT side — the SERVER spawns real ConPTY panes).
//!
//! Test lifecycle: each test uses a unique pipe name
//! (`winmux-proto-<pid>-<n>`) so tests never collide even if a prior test's
//! server thread is still winding down. Where the test flow naturally kills
//! every session (via `exit` in the last shell), we join the server thread
//! to prove clean shutdown (exit-empty). Where a session is deliberately
//! left alive (e.g. `attach_missing_session_error`, which never creates a
//! session), the server thread is left running detached — the unique pipe
//! name means it never interferes with other tests, and the process exits
//! at the end of the test binary regardless of lingering threads.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use winmux::grid::{Color, Grid};
use winmux::pipe::PipeConn;
use winmux::protocol::{read_server_msg, write_client_msg, AttachMode, ClientMsg, ServerMsg};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique_pipe_name() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(r"\\.\pipe\winmux-proto-{}-{}", std::process::id(), n)
}

fn start_server(name: &str) -> JoinHandle<()> {
    // `["-"]` = "no config at all" (Task 7 review fix): an EMPTY slice would
    // make the server load the default `.tmux.conf`/`.winmux.conf` chain
    // from the REAL process environment, so a dev/CI machine with a real
    // `%USERPROFILE%\.tmux.conf` would silently contaminate every test.
    start_server_with_config(name, &["-".to_string()])
}

/// Like `start_server`, but forwarding explicit `--config <path>` files
/// (Task 7). `"-"` entries are dropped (disable-config sentinel); an empty
/// slice means the server's own default discovery chain (never wanted in a
/// test — use `start_server` for isolation).
fn start_server_with_config(name: &str, config_files: &[String]) -> JoinHandle<()> {
    let name = name.to_string();
    let config_files = config_files.to_vec();
    thread::spawn(move || {
        winmux::server::run(&name, &config_files).expect("server run");
    })
}

/// A unique temp-file path for a test's throwaway `.tmux.conf`-style fixture:
/// `<tmpdir>/winmux-test-<tag>-<pid>-<n>.conf`. Never created by this helper
/// — callers `std::fs::write` (or deliberately don't, to test a missing
/// file) and `std::fs::remove_file` (best effort) during teardown.
fn temp_conf_path(tag: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("winmux-test-{tag}-{}-{}.conf", std::process::id(), n))
}

/// Join each grid row's cell chars into a `String`, one entry per row
/// (mirrors `tests/e2e.rs`'s helper of the same name).
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

/// True if some interior column is a full column of `│` across the pane
/// rows (everything above the bottom status bar) — a vertical split border.
fn has_vertical_border(grid: &Grid) -> bool {
    let pane_rows = grid.rows().saturating_sub(1);
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

/// Validate the tmux `ls`/`lsw` creation-time text against
/// `%a %b %e %H:%M:%S %Y` (`%e` = space-padded day), e.g. `Tue Jul  7
/// 09:14:22 2026` — exactly 24 chars: `Www Mmm eD HH:MM:SS YYYY`.
fn assert_ctime_format(text: &str) {
    assert_eq!(text.len(), 24, "unexpected ctime length: {text:?}");
    let b = text.as_bytes();
    assert!(text[0..3].chars().all(|c| c.is_ascii_alphabetic()), "weekday: {text:?}");
    assert_eq!(b[3], b' ', "sep after weekday: {text:?}");
    assert!(text[4..7].chars().all(|c| c.is_ascii_alphabetic()), "month: {text:?}");
    assert_eq!(b[7], b' ', "sep after month: {text:?}");
    let day0 = b[8] as char;
    assert!(day0 == ' ' || day0.is_ascii_digit(), "day tens: {text:?}");
    assert!((b[9] as char).is_ascii_digit(), "day ones: {text:?}");
    assert_eq!(b[10], b' ', "sep after day: {text:?}");
    let time = &text[11..19];
    assert_eq!(time.as_bytes()[2], b':', "time: {text:?}");
    assert_eq!(time.as_bytes()[5], b':', "time: {text:?}");
    for (i, c) in time.chars().enumerate() {
        if i != 2 && i != 5 {
            assert!(c.is_ascii_digit(), "time: {text:?}");
        }
    }
    assert_eq!(b[19], b' ', "sep before year: {text:?}");
    assert!(text[20..24].chars().all(|c| c.is_ascii_digit()), "year: {text:?}");
}

/// A connected test client: writes `ClientMsg` frames directly, and reads
/// `ServerMsg` frames off a background reader thread via an mpsc channel so
/// `recv`/`recv_output_until` can apply a deadline (a blocking `PipeConn`
/// read has no built-in timeout).
struct Client {
    conn: PipeConn,
    rx: Receiver<ServerMsg>,
    _reader: JoinHandle<()>,
}

impl Client {
    /// Connect, retrying while the server hasn't bound the pipe yet.
    fn connect(name: &str) -> Self {
        let deadline = Instant::now() + Duration::from_secs(10);
        let conn = loop {
            match PipeConn::connect(name) {
                Ok(c) => break c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("client connect failed: {e}"),
            }
        };
        let reader_conn = conn.try_clone().expect("clone pipe conn for reader");
        let (tx, rx) = channel::<ServerMsg>();
        let reader = thread::spawn(move || {
            let mut c = reader_conn;
            while let Ok(msg) = read_server_msg(&mut c) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        Client { conn, rx, _reader: reader }
    }

    fn send(&mut self, msg: &ClientMsg) {
        write_client_msg(&mut self.conn, msg).expect("client send");
    }

    /// Receive one `ServerMsg` within a 10s deadline.
    fn recv(&self) -> ServerMsg {
        self.rx
            .recv_timeout(Duration::from_secs(10))
            .expect("timed out waiting for a server message")
    }

    /// Feed `Output` payloads into `grid` until `pred(&grid)` holds or a
    /// non-`Output` message arrives (returned to the caller) or 10s elapse.
    fn recv_output_until(&self, grid: &mut Grid, pred: impl Fn(&Grid) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if pred(grid) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!(
                    "timed out waiting for predicate; screen:\n{}",
                    screen_text(grid).join("\n")
                );
            }
            match self.rx.recv_timeout(remaining) {
                Ok(ServerMsg::Output(bytes)) => grid.feed(&bytes),
                Ok(other) => panic!("unexpected message while waiting for output: {other:?}"),
                Err(_) => panic!(
                    "timed out waiting for predicate; screen:\n{}",
                    screen_text(grid).join("\n")
                ),
            }
        }
    }

    /// Drain `Output` messages until the session-ending `Exit` arrives,
    /// asserting its code/message.
    fn expect_exit(&self, code: u8, msg: &str) {
        loop {
            match self.recv() {
                ServerMsg::Output(_) => continue,
                ServerMsg::Exit { code: c, msg: m } => {
                    assert_eq!(c, code, "exit code");
                    assert_eq!(m, msg, "exit message");
                    return;
                }
                other => panic!("unexpected message waiting for Exit: {other:?}"),
            }
        }
    }
}

fn attach(client: &mut Client, mode: AttachMode, name: &str, cols: u16, rows: u16) {
    client.send(&ClientMsg::Attach {
        mode,
        detach_others: false,
        cols,
        rows,
        name: name.to_string(),
    });
}

/// Attach `NewAuto`, wait for the status bar + prompt, then `exit\r` the
/// last shell so the session (and, since it's the only one, the server)
/// dies — used by tests that just need a throwaway auto-named session.
fn attach_auto_and_wait_prompt(client: &mut Client, cols: u16, rows: u16) -> Grid {
    attach(client, AttachMode::NewAuto, "", cols, rows);
    let mut grid = Grid::new(cols, rows, 0);
    client.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));
    grid
}

#[test]
fn attach_new_auto_shows_status() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut client = Client::connect(&name);

    let mut grid = attach_auto_and_wait_prompt(&mut client, 80, 24);
    client.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*"))
    });

    client.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    client.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn duplicate_named_session_is_error() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c1 = Client::connect(&name);
    attach(&mut c1, AttachMode::NewNamed, "x", 80, 24);
    match c1.recv() {
        ServerMsg::Output(_) => {}
        other => panic!("expected first attach to succeed with Output, got {other:?}"),
    }

    let mut c2 = Client::connect(&name);
    attach(&mut c2, AttachMode::NewNamed, "x", 80, 24);
    assert_eq!(
        c2.recv(),
        ServerMsg::Exit { code: 1, msg: "duplicate session: x".to_string() }
    );

    c1.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c1.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn attach_missing_session_error() {
    let name = unique_pipe_name();
    let _server = start_server(&name); // left running: no session is ever created
    let mut client = Client::connect(&name);
    attach(&mut client, AttachMode::Existing, "nope", 80, 24);
    assert_eq!(
        client.recv(),
        ServerMsg::Exit { code: 1, msg: "can't find session: nope".to_string() }
    );
}

/// `attach-session` with no `-t` sends `AttachMode::Existing` with an empty
/// name; the server (`Registry::find`, Task 8 amendment) resolves that to
/// the most recently created session, or `"no sessions"` if the registry is
/// empty — never an (always-matching) empty-string prefix.
#[test]
fn attach_empty_target_picks_most_recent() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    // No sessions yet: empty target is an error, not an ambiguous match.
    let mut probe = Client::connect(&name);
    attach(&mut probe, AttachMode::Existing, "", 80, 24);
    assert_eq!(probe.recv(), ServerMsg::Exit { code: 1, msg: "no sessions".to_string() });

    let mut c1 = Client::connect(&name);
    attach(&mut c1, AttachMode::NewNamed, "e1", 80, 24);
    let mut g1 = Grid::new(80, 24, 0);
    c1.recv_output_until(&mut g1, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut c2 = Client::connect(&name);
    attach(&mut c2, AttachMode::NewNamed, "e2", 80, 24);
    let mut g2 = Grid::new(80, 24, 0);
    c2.recv_output_until(&mut g2, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // Empty target attaches to "e2" (most recently created), not "e1".
    let mut c3 = Client::connect(&name);
    attach(&mut c3, AttachMode::Existing, "", 80, 24);
    let mut g3 = Grid::new(80, 24, 0);
    c3.recv_output_until(&mut g3, |g| screen_text(g).iter().any(|l| l.contains("[e2] ")));

    // Clean up via a one-shot CLI kill-server so the server thread exits.
    let mut cli = Client::connect(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".to_string()]));
    match cli.recv() {
        ServerMsg::CliDone { code, .. } => assert_eq!(code, 0),
        other => panic!("expected CliDone, got {other:?}"),
    }
    server.join().expect("server exits after kill-server");
}

#[test]
fn detach_frame_returns_message() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c1 = Client::connect(&name);
    attach(&mut c1, AttachMode::NewNamed, "s1", 80, 24);
    match c1.recv() {
        ServerMsg::Output(_) => {}
        other => panic!("expected attach success, got {other:?}"),
    }
    c1.send(&ClientMsg::Detach);
    assert_eq!(
        c1.recv(),
        ServerMsg::Exit { code: 0, msg: "[detached (from session s1)]".to_string() }
    );

    // Session s1 survives the detach; reattach and kill it so the server exits.
    let mut c2 = Client::connect(&name);
    attach(&mut c2, AttachMode::Existing, "s1", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c2.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));
    c2.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c2.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn session_survives_detach_and_reattaches() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c1 = Client::connect(&name);
    attach(&mut c1, AttachMode::NewNamed, "s2", 80, 24);
    let mut grid1 = Grid::new(80, 24, 0);
    c1.recv_output_until(&mut grid1, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    c1.send(&ClientMsg::Stdin(b"echo marker-123\r".to_vec()));
    c1.recv_output_until(&mut grid1, |g| {
        screen_text(g).iter().any(|l| l.contains("marker-123"))
    });

    c1.send(&ClientMsg::Detach);
    match c1.recv() {
        ServerMsg::Exit { code: 0, .. } => {}
        other => panic!("expected detach Exit, got {other:?}"),
    }

    let mut c2 = Client::connect(&name);
    attach(&mut c2, AttachMode::Existing, "s2", 80, 24);
    let mut grid2 = Grid::new(80, 24, 0);
    c2.recv_output_until(&mut grid2, |g| {
        screen_text(g).iter().any(|l| l.contains("marker-123"))
    });

    c2.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c2.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn prefix_d_detaches() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c1 = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c1, 80, 24);
    let _ = &grid; // used only to establish readiness

    c1.send(&ClientMsg::Stdin(vec![0x02, b'd']));
    match c1.recv() {
        ServerMsg::Exit { code: 0, msg } => {
            assert!(msg.starts_with("[detached (from session "), "unexpected msg: {msg}");
        }
        other => panic!("expected detach Exit, got {other:?}"),
    }

    // Auto-named session "0" survives; reattach and kill it so the server exits.
    let mut c2 = Client::connect(&name);
    attach(&mut c2, AttachMode::Existing, "0", 80, 24);
    grid = Grid::new(80, 24, 0);
    c2.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));
    c2.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c2.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn split_and_kill_pane_confirm() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    c.send(&ClientMsg::Stdin(vec![0x02, b'x']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("kill-pane 1? (y/n)"))
    });

    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn exit_last_shell_exits_session() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c = Client::connect(&name);
    let _grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies (exit-empty)");
}

#[test]
fn two_clients_smallest_size_wins() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut a = Client::connect(&name);
    attach(&mut a, AttachMode::NewNamed, "shared", 100, 40);
    let mut grid_a = Grid::new(100, 40, 0);
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut b = Client::connect(&name);
    attach(&mut b, AttachMode::Existing, "shared", 80, 24);
    let mut grid_b = Grid::new(80, 24, 0);
    b.recv_output_until(&mut grid_b, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // Once B (80x24) attaches, the shared session shrinks to the smaller
    // size. On A's larger (100x40) view: the status row spans the FULL
    // client width (green background reaches column 90), but pane content
    // is confined to the session's (smaller) width — column 90 on a
    // content row stays blank.
    a.recv_output_until(&mut grid_a, |g| {
        g.cell(90, 39).style.bg == Color::Idx(2) && g.cell(90, 0).ch == ' '
    });

    a.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    // Either client observing the session end is fine; both are attached.
    a.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

// ---- Task 7: window ops, prompts, CLI --------------------------------------

#[test]
fn new_window_updates_status() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    // Clean up: kill the new (current) window, falls back to window 0.
    c.send(&ClientMsg::Stdin(vec![0x02, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("kill-window powershell? (y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn next_prev_last_window_flags() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    c.send(&ClientMsg::Stdin(vec![0x02, b'n']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-"))
    });

    c.send(&ClientMsg::Stdin(vec![0x02, b'p']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    c.send(&ClientMsg::Stdin(vec![0x02, b'l']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-"))
    });

    // Clean up: current is window 0; killing its (only) pane removes just
    // that window (tmux remain-on-exit parity) and falls back to window 1
    // (still `last`), then a second exit ends the whole session.
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| {
        let lines = screen_text(g);
        lines.iter().any(|l| l.contains("1:powershell*")) && !lines.iter().any(|l| l.contains("0:powershell"))
    });
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn select_window_by_digit() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    c.send(&ClientMsg::Stdin(vec![0x02, b'0']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-"))
    });

    // Clean up: current (window 0) exits, falls back to window 1 (last),
    // second exit ends the session.
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("1:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn select_missing_window_shows_message() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'5']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("window not found: 5")));

    // Any further input clears the transient message.
    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| !screen_text(g).iter().any(|l| l.contains("window not found")));

    c.send(&ClientMsg::Stdin(b"\rexit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn kill_window_confirm_text() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    c.send(&ClientMsg::Stdin(vec![0x02, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("kill-window powershell? (y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    // Window list shrinks back to just window 0.
    c.recv_output_until(&mut grid, |g| {
        let lines = screen_text(g);
        lines.iter().any(|l| l.contains("[0] 0:powershell*")) && !lines.iter().any(|l| l.contains("1:powershell"))
    });

    // Killing the only remaining window destroys the session.
    c.send(&ClientMsg::Stdin(vec![0x02, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("kill-window powershell? (y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn kill_only_pane_confirm_destroys_session() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'x']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("kill-pane 0? (y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn pane_exit_autocloses_pane() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    // The split gives focus to the new (right) pane; exiting its shell
    // naturally removes just that pane (remain-on-exit off parity) instead
    // of leaving a dead overlay — the border disappears and the session
    // (and its other pane's shell) lives on.
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn rename_window_prompt_flow() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b',']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) powershell")));

    // "powershell" is 10 chars; wipe it, then type the new name.
    c.send(&ClientMsg::Stdin(vec![0x7f; 10]));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) ") && !l.contains("powershell")));
    c.send(&ClientMsg::Stdin(b"web".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) web")));
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0:web*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn rename_session_prompt_flow() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    attach(&mut c, AttachMode::NewNamed, "s1", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'$']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-session) s1")));

    // "s1" is 2 chars; wipe it, then type the new name.
    c.send(&ClientMsg::Stdin(vec![0x7f, 0x7f]));
    c.send(&ClientMsg::Stdin(b"mysess".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-session) mysess")));
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[mysess]")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn prompt_escape_cancels() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b',']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) powershell")));

    c.send(&ClientMsg::Stdin(vec![0x1b]));
    c.recv_output_until(&mut grid, |g| !screen_text(g).iter().any(|l| l.contains("(rename-window)")));
    // Name unchanged.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn switch_client_next_cycles_sessions() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut a = Client::connect(&name);
    attach(&mut a, AttachMode::NewNamed, "sA", 80, 24);
    let mut grid_a = Grid::new(80, 24, 0);
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut b = Client::connect(&name);
    attach(&mut b, AttachMode::NewNamed, "sB", 80, 24);
    let mut grid_b = Grid::new(80, 24, 0);
    b.recv_output_until(&mut grid_b, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // Creation order is [sA, sB]; `)` from sA moves client A to sB.
    a.send(&ClientMsg::Stdin(vec![0x02, b')']));
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("[sB]")));

    // Wraps: `)` again from sB (only 2 sessions) goes back to sA.
    a.send(&ClientMsg::Stdin(vec![0x02, b')']));
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("[sA]")));

    a.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    a.expect_exit(0, "[exited]");
    b.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    b.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

fn cli_client(name: &str) -> Client {
    Client::connect(name)
}

fn expect_cli_done(client: &Client, code: u8) -> (String, String) {
    match client.recv() {
        ServerMsg::CliDone { code: c, out, err } => {
            assert_eq!(c, code, "cli exit code (out={out:?} err={err:?})");
            (out, err)
        }
        other => panic!("expected CliDone, got {other:?}"),
    }
}

#[test]
fn cli_ls_format_exact() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "s1".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["ls".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    let line = out.trim_end_matches('\n');
    assert!(!line.contains('\n'), "expected exactly one ls line: {line:?}");
    let prefix = "s1: 1 windows (created ";
    assert!(line.starts_with(prefix), "line: {line:?}");
    let rest = &line[prefix.len()..];
    let rest = rest.strip_suffix(" (attached)").unwrap_or(rest);
    let rest = rest.strip_suffix(')').expect("missing closing paren");
    assert_ctime_format(rest);

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "s1".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_has_session_codes() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "hx".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["has-session".into(), "-t".into(), "hx".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["has-session".into(), "-t".into(), "nope".into()]));
    let (_, err) = expect_cli_done(&cli, 1);
    assert_eq!(err, "can't find session: nope");

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "hx".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_kill_session_notifies_attached() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c = Client::connect(&name);
    attach(&mut c, AttachMode::NewNamed, "ks1", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "ks1".into()]));
    expect_cli_done(&cli, 0);

    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_new_detached_then_attach() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "nd1".into()]));
    expect_cli_done(&cli, 0);

    let mut c = Client::connect(&name);
    attach(&mut c, AttachMode::Existing, "nd1", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_rename_session() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "rn1".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["rename-session".into(), "-t".into(), "rn1".into(), "rn2".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["has-session".into(), "-t".into(), "rn2".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["has-session".into(), "-t".into(), "rn1".into()]));
    let (_, err) = expect_cli_done(&cli, 1);
    assert_eq!(err, "can't find session: rn1");

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "rn2".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

/// Final-review fix (2026-07-07): an embedded control character (here `\n`)
/// in a `new-session -s` argv name must be rejected by the CLI path exactly
/// like `model.rs`'s `names_with_control_chars_rejected` unit test pins at
/// the `Registry` level — and the name echoed back in the error is
/// sanitized (control chars -> `?`) so the rejection itself doesn't write
/// the same control byte into the CLI's stderr text.
#[test]
fn cli_rejects_control_char_session_name() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        "foo\nbar".into(),
    ]));
    let (out, err) = expect_cli_done(&cli, 1);
    assert_eq!(out, "");
    assert_eq!(err, "bad session name: foo?bar");

    // No session was ever created, so the server won't auto-exit; shut it
    // down explicitly instead of joining on session-empty.
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Pins the Task 8 empty-target rule on the CLI path: an EXPLICITLY empty
/// flag value (`-t ""` / `-s ""`) reaches `Registry::find("")` and resolves
/// to the most recently created session — the same resolution as the
/// documented no-`-t` defaults of `kill-session`/`list-windows` — rather
/// than matching as an always-true (ambiguous) empty prefix. Omitted flags
/// are unaffected: `has-session` (no `-t`) is still a usage error.
#[test]
fn cli_empty_target_resolves_most_recent() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    // No sessions yet: empty target is "no sessions", not a usage error.
    cli.send(&ClientMsg::Cli(vec!["has-session".into(), "-t".into(), "".into()]));
    let (_, err) = expect_cli_done(&cli, 1);
    assert_eq!(err, "no sessions");

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "et1".into()]));
    expect_cli_done(&cli, 0);
    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "et2".into()]));
    expect_cli_done(&cli, 0);

    // With sessions present, an empty -t matches (the most recent one).
    cli.send(&ClientMsg::Cli(vec!["has-session".into(), "-t".into(), "".into()]));
    expect_cli_done(&cli, 0);

    // kill-session -t "" kills et2 (most recent), leaving only et1 — the
    // same session an omitted -t would have targeted.
    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "".into()]));
    expect_cli_done(&cli, 0);
    cli.send(&ClientMsg::Cli(vec!["list-sessions".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(out.starts_with("et1: "), "unexpected ls output: {out:?}");
    assert!(!out.contains("et2"), "et2 should be gone: {out:?}");

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "et1".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_list_windows_format() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "lw1".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["list-windows".into(), "-t".into(), "lw1".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    assert_eq!(out, "0: powershell* (1 panes) [80x24] (active)\n");

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "lw1".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_kill_server_exits_all() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut c = Client::connect(&name);
    let _grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);

    c.expect_exit(0, "[server exited]");
    server.join().expect("server exits after kill-server");
}

#[test]
fn cli_unknown_command_err() {
    let name = unique_pipe_name();
    let _server = start_server(&name); // never creates a session; left running
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["frobnicate".into()]));
    let (out, err) = expect_cli_done(&cli, 1);
    assert_eq!(out, "");
    assert_eq!(err, "unknown command");
}

#[test]
fn stale_confirm_after_pane_exit_is_canceled() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    // Two clients on the same session: A will arm a kill-pane confirm; B
    // (still in Normal mode, same shared layout/focus) will exit that
    // pane's shell out from under the pending prompt.
    let mut a = Client::connect(&name);
    attach(&mut a, AttachMode::NewNamed, "stale", 80, 24);
    let mut grid_a = Grid::new(80, 24, 0);
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut b = Client::connect(&name);
    attach(&mut b, AttachMode::Existing, "stale", 80, 24);
    let mut grid_b = Grid::new(80, 24, 0);
    b.recv_output_until(&mut grid_b, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // A splits; the new (right) pane takes focus for the whole session.
    a.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    a.recv_output_until(&mut grid_a, has_vertical_border);

    // A arms the kill-pane confirm on the focused (new) pane.
    a.send(&ClientMsg::Stdin(vec![0x02, b'x']));
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("kill-pane 1? (y/n)")));

    // B's input is forwarded to that same focused pane: exiting its shell
    // makes the pane die NATURALLY while A's confirm is still up. The
    // server must cancel A's now-stale confirm (prompt disappears along
    // with the border) instead of leaving a live trigger on a dead target.
    b.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    a.recv_output_until(&mut grid_a, |g| {
        !has_vertical_border(g) && !screen_text(g).iter().any(|l| l.contains("kill-pane"))
    });

    // A's `y` must now be FORWARDED to the surviving pane, not interpreted
    // as a confirm: erase it again (backspace), then prove the session (and
    // its surviving shell) is still alive end-to-end by running a command.
    // If the stale confirm had fired (the old bug destroyed the whole
    // session), an Exit frame would arrive here and recv_output_until
    // would panic on the unexpected message.
    a.send(&ClientMsg::Stdin(b"y".to_vec()));
    a.send(&ClientMsg::Stdin(b"\x08echo alive-42\r".to_vec()));
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("alive-42")));

    a.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    a.expect_exit(0, "[exited]");
    b.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn prompt_commit_forwards_trailing_bytes() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b',']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) powershell")));

    // One single frame: wipe the pre-filled "powershell" (10 backspaces),
    // type the new name, COMMIT, and then trailing bytes that must be
    // re-fed through the normal input path (not silently dropped): a shell
    // command whose echo proves it reached the pane.
    let mut frame = vec![0x7f; 10];
    frame.extend_from_slice(b"web\recho trailing-ok\r");
    c.send(&ClientMsg::Stdin(frame));

    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0:web*")));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("trailing-ok")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_unknown_flag_is_usage_error() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "uf1".into()]));
    expect_cli_done(&cli, 0);

    // Unknown flag: usage error, and CRITICALLY the `-q` is not treated as
    // the positional new-name.
    cli.send(&ClientMsg::Cli(vec![
        "rename-session".into(),
        "-t".into(),
        "uf1".into(),
        "-q".into(),
        "bar".into(),
    ]));
    let (out, err) = expect_cli_done(&cli, 1);
    assert_eq!(out, "");
    assert_eq!(err, "usage: rename-session [-t target] new-name");

    // Session name unchanged (neither renamed to "-q" nor to "bar").
    cli.send(&ClientMsg::Cli(vec!["has-session".into(), "-t".into(), "uf1".into()]));
    expect_cli_done(&cli, 0);

    // A couple more commands' unknown-flag paths (a flag that exists for
    // OTHER commands, like `-t` on new-session, is also unknown here).
    cli.send(&ClientMsg::Cli(vec!["list-sessions".into(), "-z".into()]));
    let (_, err) = expect_cli_done(&cli, 1);
    assert_eq!(err, "usage: list-sessions");
    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-t".into(), "x".into()]));
    let (_, err) = expect_cli_done(&cli, 1);
    assert_eq!(err, "usage: new-session [-d] [-s name] [-x cols] [-y rows]");

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "uf1".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

// ---- Task 6: unified command dispatcher (keys, CLI, `:` prompt) -----------

#[test]
fn cli_split_window_command() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["split-window".into(), "-h".into(), "-t".into(), "0".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, has_vertical_border);

    c.send(&ClientMsg::Stdin(vec![0x02, b'x']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_send_keys() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["send-keys".into(), "-t".into(), "0".into(), "echo send-keys-marker".into(), "Enter".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("send-keys-marker")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn cli_send_keys_literal() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    // -l: the whole arg is sent as literal bytes, not parsed key-by-key (in
    // particular "echo" must NOT be treated as an unrecognized key name and
    // dropped -- it must reach the shell verbatim).
    cli.send(&ClientMsg::Cli(vec!["send-keys".into(), "-l".into(), "-t".into(), "0".into(), "echo literal-marker".into()]));
    expect_cli_done(&cli, 0);
    // -l does not send a trailing Enter: the shell has only echoed the text
    // onto its input line so far.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("literal-marker")));

    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().filter(|l| l.contains("literal-marker")).count() >= 2);

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// tmux parity: a `-t` target that names only a SESSION (no `:`/`.`) resolves
/// to that session's current window's active pane -- the single most common
/// scripting idiom, `tmux send-keys -t mysession ...`. `demo` has no window
/// named/indexed "demo", so before the fix this fell through to "pane not
/// found: demo"; the practical rule now says a bare NON-NUMERIC token is a
/// session name via `Registry::find`, not a pane spec.
#[test]
fn send_keys_bare_session_target() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "demo".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec![
        "send-keys".into(),
        "-t".into(),
        "demo".into(),
        "echo bare-ok".into(),
        "Enter".into(),
    ]));
    expect_cli_done(&cli, 0);

    let mut c = Client::connect(&name);
    attach(&mut c, AttachMode::Existing, "demo", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("bare-ok")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// The same bare-token session-name fallback, when the name doesn't resolve
/// to any session at all, surfaces `Registry::find`'s own error rather than
/// a generic "pane not found" -- that's what the user actually meant by a
/// non-numeric `-t`.
#[test]
fn bare_nonnumeric_unknown_session_errors() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "onlysession".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec![
        "send-keys".into(),
        "-t".into(),
        "nosuch".into(),
        "echo nope".into(),
        "Enter".into(),
    ]));
    let (_, err) = expect_cli_done(&cli, 1);
    assert_eq!(err, "can't find session: nosuch");

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "onlysession".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

#[test]
fn command_prompt_executes() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"new-window\r".to_vec()));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    // Clean up: kill the new (current) window, falls back to window 0.
    c.send(&ClientMsg::Stdin(vec![0x02, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn command_prompt_error_message() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"badcmd\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("unknown command: badcmd")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn set_prefix_runtime() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "prefix".into(), "C-a".into()]));
    expect_cli_done(&cli, 0);

    // New prefix (0x01 = C-a) + c makes a new window.
    c.send(&ClientMsg::Stdin(vec![0x01, b'c']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    // The OLD prefix (0x02) is no longer special: 0x02 followed by "%" is
    // just forwarded (0x02 is swallowed by the shell as an ordinary control
    // byte; "%" types onto the prompt line) -- no split occurs.
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.send(&ClientMsg::Stdin(b"\recho old-prefix-marker\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("old-prefix-marker")));
    assert!(!has_vertical_border(&grid), "old prefix must no longer trigger split-window");

    // Clean up: kill the new (current) window, falls back to window 0.
    c.send(&ClientMsg::Stdin(vec![0x01, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn bind_custom_key() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["bind".into(), "V".into(), "split-window".into(), "-h".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(vec![0x02, b'V']));
    c.recv_output_until(&mut grid, has_vertical_border);

    c.send(&ClientMsg::Stdin(vec![0x02, b'x']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn unbind_default() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["unbind".into(), "%".into()]));
    expect_cli_done(&cli, 0);

    // `%` is no longer bound in the prefix table: swallowed (tmux behavior
    // for an unbound prefix-table key) -- no split, and nothing is forwarded
    // either (unlike an unbound ROOT-table key).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.send(&ClientMsg::Stdin(b"echo unbind-marker\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("unbind-marker")));
    assert!(!has_vertical_border(&grid), "unbound prefix key must not split");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn confirm_before_custom() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec![
        "bind".into(),
        "k".into(),
        "confirm-before".into(),
        "-p".into(),
        "sure? (y/n)".into(),
        "kill-pane".into(),
    ]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(vec![0x02, b'k']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("sure? (y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    // Only pane of the only window: killing it destroys the session.
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn list_keys_contains_defaults() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["list-keys".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    assert!(out.contains("bind-key -T prefix % split-window -h"), "out: {out:?}");
    assert!(out.contains("bind-key -r -T prefix C-Up resize-pane -U"), "out: {out:?}");

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn show_options_output() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["show-options".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    assert!(out.contains("prefix C-b"), "out: {out:?}");
    assert!(out.contains("repeat-time 500"), "out: {out:?}");

    cli.send(&ClientMsg::Cli(vec!["show".into(), "prefix".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert_eq!(out, "prefix C-b\n");

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn set_default_command() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "default-command".into(), "cmd.exe".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Microsoft Windows [Version")));

    // Clean up: kill the new (cmd.exe) window, falls back to window 0.
    c.send(&ClientMsg::Stdin(vec![0x02, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn renumber_windows_on() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "rnum".into()]));
    expect_cli_done(&cli, 0);
    // `new-window` has no `-t` target (see `cmd::ParsedCmd::NewWindow`): a
    // bare call falls back to the most-recently-created session, which is
    // `rnum` since this test creates no other session.
    cli.send(&ClientMsg::Cli(vec!["new-window".into()]));
    expect_cli_done(&cli, 0);
    cli.send(&ClientMsg::Cli(vec!["new-window".into()]));
    expect_cli_done(&cli, 0);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "renumber-windows".into(), "on".into()]));
    expect_cli_done(&cli, 0);

    // Indices are 0,1,2; kill the middle one (1) -> with renumbering on, the
    // survivors (0, 2) become (0, 1).
    cli.send(&ClientMsg::Cli(vec!["kill-window".into(), "-t".into(), "rnum:1".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["list-windows".into(), "-t".into(), "rnum".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(out.starts_with("0: "), "out: {out:?}");
    assert!(out.contains("1: "), "out: {out:?}");
    assert!(!out.contains("2: "), "out: {out:?}");

    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "rnum".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

/// Task 7 review fix (Critical): with `base-index 1` + `renumber-windows on`
/// loaded from a startup config file, killing a window renumbers the
/// survivors starting from 1 (the base), never producing a window 0.
#[test]
fn renumber_windows_with_base_index() {
    let name = unique_pipe_name();
    let conf_path = temp_conf_path("rnum-base");
    std::fs::write(&conf_path, "set -g base-index 1\nset -g renumber-windows on\n").expect("write temp conf");
    let config_files = vec![conf_path.to_string_lossy().into_owned()];

    let server = start_server_with_config(&name, &config_files);
    let mut cli = cli_client(&name);

    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "rnumb".into()]));
    expect_cli_done(&cli, 0);
    // base-index 1: first window is 1; new-window appends 2.
    cli.send(&ClientMsg::Cli(vec!["new-window".into()]));
    expect_cli_done(&cli, 0);

    // Kill window 1 -> the survivor (2) renumbers to 1, NOT 0.
    cli.send(&ClientMsg::Cli(vec!["kill-window".into(), "-t".into(), "rnumb:1".into()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["list-windows".into(), "-t".into(), "rnumb".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(out.starts_with("1: "), "out: {out:?}");
    assert!(!out.contains("0: "), "out: {out:?}");

    let _ = std::fs::remove_file(&conf_path);
    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "rnumb".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

#[test]
fn display_message_expands() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let _grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["display-message".into(), "#S:#W".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    assert_eq!(out, "0:powershell");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn kill_pane_via_command_targets() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["split-window".into(), "-h".into(), "-t".into(), "0".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, has_vertical_border);

    // -t 1: the second pane by position, addressed directly (no confirm).
    cli.send(&ClientMsg::Cli(vec!["kill-pane".into(), "-t".into(), "1".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Fix (Task 6 review, Important 1): a client renaming its OWN session via
/// an explicit `-t <own-session>` (normal tmux idiom) must keep the acting
/// client's session reference in sync — the old bug only synced the
/// `target: None` form, so the registry got the new name while the client
/// kept looking its session up by the old one, and `render_all`'s
/// find-by-name silently stopped rendering that client forever (appeared
/// hung).
#[test]
fn rename_session_dash_t_own_session_keeps_client_synced() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    attach(&mut c, AttachMode::NewNamed, "work", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"rename-session -t work dev\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[dev]")));

    // The client must still be rendering: a keystroke round-trips through
    // its (renamed) session's focused pane and back onto its screen.
    c.send(&ClientMsg::Stdin(b"echo sync-ok\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("sync-ok")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Fix (Task 6 review, Important 2): killing a FOREIGN session's last
/// window/pane via `-t other:...` destroys that session (its own attached
/// clients are notified by `destroy_session`) but must NOT exit the acting
/// client, which is attached to a different session.
#[test]
fn kill_foreign_session_pane_keeps_client_attached() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    attach(&mut c, AttachMode::NewNamed, "a", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "b".into()]));
    expect_cli_done(&cli, 0);

    // Kill session b's only window from client A (attached to "a"): b dies,
    // A must stay attached.
    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"kill-window -t b:0\r".to_vec()));

    // Session b eventually disappears from ls (the `:` commit is
    // asynchronous relative to the CLI connection).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cli.send(&ClientMsg::Cli(vec!["ls".into()]));
        let (out, _) = expect_cli_done(&cli, 0);
        if out.starts_with("a: ") && !out.contains("b: ") {
            break;
        }
        assert!(Instant::now() < deadline, "session b never died; ls: {out:?}");
        thread::sleep(Duration::from_millis(50));
    }

    // A must still be attached and rendering end-to-end.
    c.send(&ClientMsg::Stdin(b"echo still-here\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("still-here")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn source_file_runtime() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    let path = std::env::temp_dir().join(format!("winmux-test-source-{}-{}.conf", std::process::id(), unique_pipe_name().len()));
    std::fs::write(&path, "bind V split-window -h\nset -g base-index 5\n").expect("write temp conf");

    cli.send(&ClientMsg::Cli(vec!["source-file".into(), path.to_string_lossy().into_owned()]));
    expect_cli_done(&cli, 0);

    cli.send(&ClientMsg::Cli(vec!["list-keys".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(out.contains("bind-key -T prefix V split-window -h"), "out: {out:?}");

    cli.send(&ClientMsg::Cli(vec!["show".into(), "base-index".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert_eq!(out, "base-index 5\n");

    let _ = std::fs::remove_file(&path);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// SP6 Task 2 (config compatibility): the user's REAL `.tmux.conf`, copied
/// verbatim into `tests/fixtures/user.tmux.conf`, must load with zero
/// errors. Runtime `source-file` exercises the exact same
/// `load_config_files`/dispatch path startup config loading uses
/// (`execute_source_file_headless` wraps one `required: true` candidate
/// through `load_config_files`, joining every collected error into the
/// CLI's `err` field) -- a CLI exit code of 0 with an empty `err` is the
/// strictest "zero config errors" signal available over the protocol today
/// (there is no direct wire-level "error count" query; the transient
/// status-bar `config: N error(s)` notice is a STARTUP-only, 750ms-lifetime
/// side effect, and asserting its ABSENCE for a whole test would be racy --
/// see `config_errors_collected_and_continue` above for the POSITIVE use of
/// that same signal). Also spot-checks a few of the fixture's real effects:
/// the custom prefix, the `|` rebind, and `mouse on`.
#[test]
fn user_config_loads_clean() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut cli = cli_client(&name);

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/user.tmux.conf");
    cli.send(&ClientMsg::Cli(vec!["source-file".into(), fixture.to_string_lossy().into_owned()]));
    let (_out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "", "the user's .tmux.conf must load with zero errors");

    cli.send(&ClientMsg::Cli(vec!["show".into(), "prefix".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert_eq!(out, "prefix C-a\n");

    cli.send(&ClientMsg::Cli(vec!["show".into(), "mouse".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert_eq!(out, "mouse on\n");

    cli.send(&ClientMsg::Cli(vec!["list-keys".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(out.contains("bind-key -T prefix | split-window -h"), "out: {out:?}");

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

// ---- Task 7: startup config loading (`## config` contract section) ----

/// A `.tmux.conf`-style fixture loaded at STARTUP (via `--config`, the
/// server-role equivalent of the CLI's `-f`) applies before any client ever
/// attaches: a custom `prefix`, a custom prefix-table binding, and
/// `base-index` are all live from the very first attach.
#[test]
fn config_file_applies_at_startup() {
    let name = unique_pipe_name();
    let conf_path = temp_conf_path("startup");
    std::fs::write(&conf_path, "set -g prefix C-a\nbind V split-window -h\nset -g base-index 1\n")
        .expect("write temp conf");
    let config_files = vec![conf_path.to_string_lossy().into_owned()];

    let _server = start_server_with_config(&name, &config_files);
    let mut client = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut client, 80, 24);
    // base-index 1: the auto session's first window is index 1, not 0.
    client.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 1:powershell*")));

    // Custom prefix (C-a, 0x01) + the custom `V` binding splits vertically.
    // The DEFAULT prefix (C-b, 0x02) no longer means anything special here.
    client.send(&ClientMsg::Stdin(vec![0x01, b'V']));
    client.recv_output_until(&mut grid, has_vertical_border);

    let _ = std::fs::remove_file(&conf_path);
    // Two panes now (both real shells) — leave the server thread running
    // rather than juggling both exits, matching this file's convention for
    // tests that don't need to prove clean exit-empty shutdown.
}

/// A bad line between two good ones does not stop the good ones from
/// applying (tmux behavior: loading continues past an error), and the
/// FIRST client to attach gets a transient `config: N error(s)` notice.
#[test]
fn config_errors_collected_and_continue() {
    let name = unique_pipe_name();
    let conf_path = temp_conf_path("errors");
    std::fs::write(&conf_path, "set -g base-index 5\nset -g nonsense on\nbind V split-window -h\n")
        .expect("write temp conf");
    let config_files = vec![conf_path.to_string_lossy().into_owned()];

    let _server = start_server_with_config(&name, &config_files);
    let mut client = Client::connect(&name);
    attach(&mut client, AttachMode::NewAuto, "", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    // Check the transient config-error message FIRST, before waiting on
    // anything else: real ConPTY/shell spawn latency can easily exceed the
    // message's 750ms lifetime, so `attach_auto_and_wait_prompt` (which
    // consumes every frame up to the shell prompt) would race it away.
    client.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("config: 1 error(s)")));
    // Both good lines applied despite the bad one in between.
    client.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 5:powershell*")));
    client.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // The default prefix (C-b) is unaffected by the bad line; the good
    // `bind V split-window -h` still works.
    client.send(&ClientMsg::Stdin(vec![0x02, b'V']));
    client.recv_output_until(&mut grid, has_vertical_border);

    let _ = std::fs::remove_file(&conf_path);
}

/// The first-attach config-error notice is a ONE-TIME slot: a second client
/// attaching afterward never sees it.
#[test]
fn second_attach_no_config_message() {
    let name = unique_pipe_name();
    let conf_path = temp_conf_path("second-attach");
    std::fs::write(&conf_path, "set -g base-index 3\nset -g bogus-option x\n").expect("write temp conf");
    let config_files = vec![conf_path.to_string_lossy().into_owned()];

    let _server = start_server_with_config(&name, &config_files);

    let mut c1 = Client::connect(&name);
    attach(&mut c1, AttachMode::NewAuto, "", 80, 24);
    let mut grid1 = Grid::new(80, 24, 0);
    // Checked immediately after attach, before waiting on the shell prompt
    // (see `config_errors_collected_and_continue`'s comment: the message's
    // 750ms lifetime can race real shell-spawn latency).
    c1.recv_output_until(&mut grid1, |g| screen_text(g).iter().any(|l| l.contains("config: 1 error(s)")));

    let mut c2 = Client::connect(&name);
    attach(&mut c2, AttachMode::NewAuto, "", 80, 24);
    let mut grid2 = Grid::new(80, 24, 0);
    // base-index 3 (this test's only GOOD line) applies to c2's session too
    // — config loads once, before either client attaches.
    c2.recv_output_until(&mut grid2, |g| screen_text(g).iter().any(|l| l.contains("[1] 3:powershell*")));
    assert!(
        !screen_text(&grid2).iter().any(|l| l.contains("config:")),
        "second attach should not see the config-error notice; screen:\n{}",
        screen_text(&grid2).join("\n")
    );

    let _ = std::fs::remove_file(&conf_path);
}

/// An explicitly-requested config file (`--config`/`-f`) that doesn't exist
/// is a collected error (unlike a missing DEFAULT-chain file, which is
/// silently skipped) — the server still comes up and serves attaches.
#[test]
fn explicit_missing_config_is_error() {
    let name = unique_pipe_name();
    let missing_path = temp_conf_path("missing"); // deliberately never written
    let config_files = vec![missing_path.to_string_lossy().into_owned()];

    let _server = start_server_with_config(&name, &config_files);
    let mut client = Client::connect(&name);
    attach(&mut client, AttachMode::NewAuto, "", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    // Checked immediately after attach (see `config_errors_collected_and_continue`'s
    // comment: don't wait on the shell prompt first — the message's 750ms
    // lifetime can race real shell-spawn latency).
    client.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("config: 1 error(s)")));
}

/// Multiple explicit `--config` files are loaded in order; a later file
/// re-setting the same option wins (plain dispatch-order override, not any
/// special merge logic).
#[test]
fn two_explicit_configs_later_wins() {
    let name = unique_pipe_name();
    let a = temp_conf_path("two-a");
    let b = temp_conf_path("two-b");
    std::fs::write(&a, "set -g base-index 2\n").expect("write temp conf a");
    std::fs::write(&b, "set -g base-index 7\n").expect("write temp conf b");
    let config_files = vec![a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned()];

    let _server = start_server_with_config(&name, &config_files);
    let mut client = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut client, 80, 24);
    client.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 7:powershell*")));

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

// ---- Task 8: option-driven rendering ----------------------------------------

/// `set -g status-style bg=blue,fg=white` restyles the status row's cells at
/// runtime (asserted through the test Grid's per-cell styles).
#[test]
fn set_status_style_changes_bar() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Default first: bg green, fg black on the bottom row.
    c.recv_output_until(&mut grid, |g| {
        g.cell(0, 23).style.bg == Color::Idx(2) && g.cell(0, 23).style.fg == Color::Idx(0)
    });

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "status-style".into(), "bg=blue,fg=white".into()]));
    expect_cli_done(&cli, 0);

    c.recv_output_until(&mut grid, |g| {
        g.cell(0, 23).style.bg == Color::Idx(4) && g.cell(0, 23).style.fg == Color::Idx(7)
    });

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// A custom `status-left` format string replaces the default `[#S] ` prefix
/// and expands its `#S` against live state.
#[test]
fn set_status_left_format() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "status-left".into(), "[cfg-#S] ".into()]));
    expect_cli_done(&cli, 0);

    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[cfg-0] 0:powershell*"))
    });

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// A `set -g status-left` value containing a control character (title
/// spoofing / OSC 52 clipboard injection / \r\n corruption -- the composited
/// status row reaches EVERY attached client's terminal) is rejected by the
/// CLI with `bad value: <sanitized>`, and the status bar is left showing the
/// untouched default rather than any partially-applied value.
#[test]
fn set_status_left_rejects_control_chars() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Default status-left ("[#S] ", session "0") is showing.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec![
        "set".into(),
        "-g".into(),
        "status-left".into(),
        "a\x1bb".into(),
    ]));
    let (out, err) = expect_cli_done(&cli, 1);
    assert_eq!(out, "");
    assert_eq!(err, "bad value: a?b");

    // Status bar unchanged: still the default "[0] " prefix, never corrupted
    // by the rejected value.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `set -g status-position top` moves the bar to row 0 AND shifts/resizes
/// the pane area down: new shell output lands strictly below row 0.
#[test]
fn status_position_top_moves_bar() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "status-position".into(), "top".into()]));
    expect_cli_done(&cli, 0);

    // Bar now on row 0.
    c.recv_output_until(&mut grid, |g| screen_text(g)[0].contains("[0] 0:powershell*"));

    // Panes were re-laid-out below the bar: a fresh command's echo shows up
    // on some row BELOW row 0, and row 0 stays the status bar.
    c.send(&ClientMsg::Stdin(b"echo top-marker\r".to_vec()));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().skip(1).any(|l| l.contains("top-marker"))
    });
    assert!(
        screen_text(&grid)[0].contains("0:powershell*"),
        "row 0 must remain the status bar; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `set -g status off` removes the bar and gives the pane the full height:
/// the bottom row's cells lose the status background entirely.
#[test]
fn status_off_hides_bar() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Bar present first.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "status".into(), "off".into()]));
    expect_cli_done(&cli, 0);

    // Bar text gone AND the bottom row is now pane area (default background,
    // not the green status fill) — proving the pane grew to the full height.
    c.recv_output_until(&mut grid, |g| {
        !screen_text(g).iter().any(|l| l.contains("0:powershell*")) && g.cell(0, 23).style.bg == Color::Default
    });

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `set -g pane-active-border-style fg=red` restyles the border cells
/// adjacent to the focused pane at runtime.
#[test]
fn pane_active_border_style_runtime() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec![
        "set".into(),
        "-g".into(),
        "pane-active-border-style".into(),
        "fg=red".into(),
    ]));
    expect_cli_done(&cli, 0);

    // The split border column turns red (fg Idx(1)) on the half owned by
    // the focused right pane instead of the default green (Task 11:
    // exactly two tiled panes -> the divider is cosmetically split in half,
    // `docs/tmux-reference/panes-and-layout.md` §7.1 -- the RIGHT pane owns
    // the bottom half of a side-by-side divider, so row 0 specifically is
    // no longer guaranteed red; check across every row instead). Pre-Task
    // 11 the whole column read active/red at row 0 -- sanctioned inversion,
    // see the Task 11 report.
    c.recv_output_until(&mut grid, |g| {
        let pane_rows = g.rows().saturating_sub(1);
        (1..g.cols().saturating_sub(1)).any(|col| {
            (0..pane_rows).all(|r| g.cell(col, r).ch == '│')
                && (0..pane_rows).any(|r| g.cell(col, r).style.fg == Color::Idx(1))
        })
    });

    // Exit the focused (right) pane, wait for the border to disappear so the
    // next exit lands in the surviving pane, then exit it too.
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Coverage gap closed in the mouse-task fix round: the DEFAULT (no runtime
/// `set`) `pane-active-border-style` (fg=green) had protocol coverage only
/// for a runtime restyle (`pane_active_border_style_runtime`, above, sets
/// fg=red). This drives a 3-pane layout (`%` then `"`, so the vertical
/// border between the left pane and the right column has a T-junction where
/// the horizontal split meets it) and asserts the green segment tracks
/// whichever of the two right-hand panes is actually focused.
#[test]
fn pane_default_active_border_follows_focus() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Left pane (full height) | top-right / bottom-right (focused after the
    // 2nd split).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);

    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    // Wait for the T-junction glyph ('├': border continues up/down/right,
    // stops left) where the new horizontal split meets the vertical one.
    c.recv_output_until(&mut grid, |g| {
        (0..g.rows().saturating_sub(1)).any(|r| g.cell(border_x, r).ch == '├')
    });
    let split_row = (0..grid.rows().saturating_sub(1))
        .find(|&r| grid.cell(border_x, r).ch == '├')
        .expect("expected a T-junction on the vertical border after the second split");
    assert!(
        split_row >= 1 && split_row + 1 < grid.rows().saturating_sub(1),
        "split_row {split_row} too close to an edge to test both segments"
    );
    let top_row = split_row - 1;
    let bottom_row = split_row + 1;

    // The bottom-right pane is focused right after the 2nd split: the
    // border segment adjacent to it is green (default
    // `pane-active-border-style fg=green`); the segment adjacent to the
    // top-right (non-focused) pane keeps the default (non-green) fg.
    assert_eq!(
        grid.cell(border_x, bottom_row).style.fg,
        Color::Idx(2),
        "border adjacent to the focused (bottom-right) pane should be green"
    );
    assert_ne!(
        grid.cell(border_x, top_row).style.fg,
        Color::Idx(2),
        "border adjacent to the non-focused (top-right) pane should stay default"
    );

    // Move focus to the top-right pane (spatial `select-pane -U`) and
    // confirm the green segment FOLLOWS: it flips to the top segment, and
    // the formerly-green bottom segment reverts to default.
    let mut up = vec![0x02];
    up.extend_from_slice(b"\x1b[A");
    c.send(&ClientMsg::Stdin(up));
    c.recv_output_until(&mut grid, |g| g.cell(border_x, top_row).style.fg == Color::Idx(2));
    assert_ne!(
        grid.cell(border_x, bottom_row).style.fg,
        Color::Idx(2),
        "border adjacent to the now-non-focused (bottom-right) pane should revert to default"
    );

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// User-reported bug: `Layout::focus_dir` used to probe only the focused
/// pane's cross-axis MIDPOINT against each candidate's range. Left pane
/// (full height, `%` then `"` on the right pane) | top-right / bottom-right:
/// from the tall left pane, the midpoint of its own y-range (0..24 -> 12)
/// lands EXACTLY on the horizontal border between top-right (0..12) and
/// bottom-right (13..24), matching neither -- `prefix-Right` silently no-op'd.
/// Fixed to use a real interval-overlap test between the two panes' ranges,
/// with tmux's most-recently-used tie-break (approximated here via
/// `last_focused`, winmux's single "last pane" state) when both candidates
/// overlap.
#[test]
fn focus_right_into_split_column() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // H(1, V(2,3)): pane1 {0,0,40,24} (tall, full height); pane2
    // {41,0,39,12} (top-right); pane3 {41,13,39,11} (bottom-right, focused
    // right after the 2nd split).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40 && g.cursor().1 >= 13);

    // prefix-Left from pane3: single candidate (pane1) -- this direction
    // already worked before the fix.
    let mut left = vec![0x02];
    left.extend_from_slice(b"\x1b[D");
    c.send(&ClientMsg::Stdin(left));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < 40);

    // prefix-Right from the tall pane1: BEFORE the fix this was a no-op.
    // Candidates are pane2 and pane3; `last_focused` is pane3 (focused
    // immediately before the Left move above), so tmux's MRU tie-break
    // picks pane3 -- the cursor must land back in the bottom-right pane.
    let mut right = vec![0x02];
    right.extend_from_slice(b"\x1b[C");
    c.send(&ClientMsg::Stdin(right));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40 && g.cursor().1 >= 13);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Symmetric direction to `focus_right_into_split_column`: from the
/// top-right pane, `prefix-Left` must land back in the tall left pane. This
/// direction was never buggy (a single-pane target's range already contains
/// any midpoint drawn from a smaller source pane's range), but is pinned
/// here as a no-regression guard on the same layout.
#[test]
fn focus_left_from_split_column() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40 && g.cursor().1 >= 13);

    // prefix-Up: bottom-right (pane3) -> top-right (pane2).
    let mut up = vec![0x02];
    up.extend_from_slice(b"\x1b[A");
    c.send(&ClientMsg::Stdin(up));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40 && g.cursor().1 < 13);

    // prefix-Left from pane2 (top-right) -> pane1 (tall left pane).
    let mut left = vec![0x02];
    left.extend_from_slice(b"\x1b[D");
    c.send(&ClientMsg::Stdin(left));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < 40);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Vertical-axis variant of the same bug: a full-width tall pane on top of a
/// row that's been split left/right (`"` then `%` on the bottom pane).
/// `prefix-Down` from the full-width pane used to no-op for the same reason
/// -- the source pane's x-midpoint landing exactly on the vertical border
/// between the two candidates below.
#[test]
fn focus_down_into_split_row() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // V(1, H(2,3)): pane1 {0,0,80,12} (full-width top); pane2 {0,13,40,11}
    // (bottom-left); pane3 {41,13,39,11} (bottom-right, focused right after
    // the 2nd split).
    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    c.recv_output_until(&mut grid, has_horizontal_border);
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40 && g.cursor().1 >= 13);

    // prefix-Left: pane3 -> pane2 (single candidate, already worked).
    let mut left = vec![0x02];
    left.extend_from_slice(b"\x1b[D");
    c.send(&ClientMsg::Stdin(left));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < 40 && g.cursor().1 >= 13);

    // prefix-Up: pane2 -> pane1, the full-width tall pane (single
    // candidate, already worked).
    let mut up = vec![0x02];
    up.extend_from_slice(b"\x1b[A");
    c.send(&ClientMsg::Stdin(up));
    c.recv_output_until(&mut grid, |g| g.cursor().1 < 12);

    // prefix-Down from pane1: BEFORE the fix this was a no-op. Candidates
    // are pane2 and pane3; `last_focused` is pane2 (focused immediately
    // before the Up move above), so MRU picks pane2 -- cursor lands
    // bottom-left.
    let mut down = vec![0x02];
    down.extend_from_slice(b"\x1b[B");
    c.send(&ClientMsg::Stdin(down));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < 40 && g.cursor().1 >= 13);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// SP6 parity wave 2, Task 3: `Layout::focus_dir`'s edge-flip wrap rule
/// (`docs/tmux-reference/panes-and-layout.md` §1.1) -- directional
/// navigation off the near edge of the window wraps to the far edge,
/// mirroring `window_pane_find_left/right`. Pre-fix this was a silent
/// no-op (`focus_dir_two_pane_horizontal`'s inverted `false` assertions,
/// `src/layout.rs`).
#[test]
fn focus_wraps_at_window_edge() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // (1 | 2): focus starts on pane2 (right, cursor x > 40).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    let mut left = vec![0x02];
    left.extend_from_slice(b"\x1b[D");
    // prefix-Left: pane2 -> pane1 (leftmost, single candidate, unaffected
    // by the wrap fix).
    c.send(&ClientMsg::Stdin(left.clone()));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < 40);

    // prefix-Left AGAIN from the now-leftmost pane: pane1 is flush against
    // the window's left edge, so the search edge flips to one past the
    // right edge -- pane2 (flush right) is the sole candidate. Pre-fix this
    // was a no-op (cursor would stay in pane1); post-fix it wraps back to
    // pane2.
    c.send(&ClientMsg::Stdin(left));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// `set -g window-status-current-style fg=red,bold` restyles ONLY the
/// current window's tab; the non-current tab keeps the base style.
#[test]
fn window_status_current_style_override() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Second window: tabs "0:powershell- 1:powershell*".
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec![
        "set".into(),
        "-g".into(),
        "window-status-current-style".into(),
        "fg=red,bold".into(),
    ]));
    expect_cli_done(&cli, 0);

    c.recv_output_until(&mut grid, |g| {
        let row: String = (0..g.cols()).map(|x| g.cell(x, 23).ch).collect();
        match (row.find("1:powershell*"), row.find("0:powershell-")) {
            (Some(cur), Some(non)) => {
                let cur_style = g.cell(cur as u16, 23).style;
                let non_style = g.cell(non as u16, 23).style;
                // current tab: red + bold (layered over the green base bg);
                // non-current tab: untouched base (black fg, not bold).
                cur_style.fg == Color::Idx(1)
                    && cur_style.bold
                    && cur_style.bg == Color::Idx(2)
                    && non_style.fg == Color::Idx(0)
                    && !non_style.bold
            }
            _ => false,
        }
    });

    // Clean up: exit the current (window 1) shell — its window dies and
    // window 0 becomes current — then exit the last shell.
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")) && !screen_text(g).iter().any(|l| l.contains("1:powershell"))
    });
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

// ---- copy mode (Task 2, sub-project 4) ----

/// The pane's TOP row (row 0) — where the `[scroll/history]` position
/// indicator is painted, right-aligned, while a client is in copy mode.
fn row0(g: &Grid) -> String {
    (0..g.cols()).map(|x| g.cell(x, 0).ch).collect()
}

fn has_indicator(g: &Grid, prefix: &str) -> bool {
    let row = row0(g);
    row.contains(prefix) && row.trim_end().ends_with(']')
}

#[test]
fn copy_mode_enters_and_indicator() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // `q` cancels: the indicator disappears and normal typing resumes.
    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_mode_scroll_shows_history() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // Emit more numbered lines than fit in the pane so some scroll into
    // history, then wait for the last marker.
    c.send(&ClientMsg::Stdin(b"1..40 | ForEach-Object { \"scrollmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("scrollmark40")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // vi `K` x3 (dedicated scroll-up, distinct from `k` cursor-up which
    // only starts scrolling once the cursor reaches the pane's top row):
    // scroll indicator advances to [3/N].
    c.send(&ClientMsg::Stdin(b"KKK".to_vec()));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[3/"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[3/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_mode_page_keys() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(b"1..80 | ForEach-Object { \"pagemark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("pagemark80")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // PPage (page-up): scroll advances off zero.
    c.send(&ClientMsg::Stdin(b"\x1b[5~".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_indicator(g, "[0/"));

    // NPage (page-down) back to the bottom: scroll returns to zero.
    c.send(&ClientMsg::Stdin(b"\x1b[6~".to_vec()));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_mode_q_exits() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let before = screen_text(&grid);

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));
    assert_ne!(screen_text(&grid), before, "indicator must have changed row 0");

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g) == before);

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_mode_vi_vs_emacs_tables() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Emacs is the default: `C-p` (cursor-up, NOT the prefix key) moves the
    // copy cursor -- observable as the composed cursor row decreasing by
    // one (the local test Grid mirrors every CUP the server emits).
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));
    let entry_row = grid.cursor().1;
    c.send(&ClientMsg::Stdin(vec![0x10])); // C-p
    c.recv_output_until(&mut grid, |g| g.cursor().1 == entry_row - 1);

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));

    // Switch to vi: `C-p` is unbound there (swallowed), so the cursor must
    // NOT move even though it's the same raw byte.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));
    let entry_row_vi = grid.cursor().1;
    c.send(&ClientMsg::Stdin(vec![0x10])); // C-p again -- unbound in vi
    // No move to observe directly; instead confirm `h` (vi-only, cursor-left)
    // DOES work, proving the vi table is genuinely active and swallowing
    // C-p rather than e.g. still resolving the emacs table.
    c.send(&ClientMsg::Stdin(b"h".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cursor().1 == entry_row_vi);

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_mode_prefix_still_works() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // A prefix-table binding (`C-b c` -> new-window) still fires from copy
    // mode -- the KeyMachine's prefix interception is unconditional.
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*"))
    });

    // Switching windows canceled the old copy mode: the indicator is gone
    // from the new (window 1) pane's render.
    assert!(!row0(&grid).contains("[0/"), "copy mode must be canceled by the window switch");

    // Clean up: kill window 1 (falls back to window 0), then exit.
    c.send(&ClientMsg::Stdin(vec![0x02, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_mode_pane_death_cancels() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Split: the split gives focus to the NEW (right) pane.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["split-window".into(), "-h".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, has_vertical_border);

    // Enter copy mode: binds to the currently-focused (new, right) pane.
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0/")));

    // Headless `send-keys` with no `-t` defaults to the same focused pane
    // (the one copy mode is bound to) -- its shell exits naturally, which
    // is exactly the pane-death path `cancel_stale_copy_modes` hooks.
    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["send-keys".into(), "exit".into(), "Enter".into()]));
    expect_cli_done(&cli2, 0);

    // The window collapses to one pane and copy mode's indicator is gone --
    // no crash, no stale overlay left on the survivor.
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g) && !screen_text(g).iter().any(|l| l.contains("[0/")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

// ---- selection + paste buffers (Task 3, sub-project 4) ----

#[test]
fn copy_selection_to_buffer_and_paste() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(b"echo hello123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "hello123"));
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "hello123").unwrap() as u16;
    let baseline = screen_text(&grid).iter().filter(|l| l.contains("hello123")).count();

    // Enter copy mode, move to the start of the "hello123" line.
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));
    let entry_row = grid.cursor().1;
    let mut moves = vec![b'0'];
    moves.extend(std::iter::repeat_n(b'k', (entry_row - target_row) as usize));
    c.send(&ClientMsg::Stdin(moves));
    c.recv_output_until(&mut grid, |g| g.cursor().1 == target_row);

    // Select the whole line (Space=begin-selection, $=end-of-line -- trailing
    // blanks get trimmed at extraction time) and copy (Enter).
    c.send(&ClientMsg::Stdin(b" $\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));

    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli2, 0);
    assert!(out.contains("buffer0: 8 bytes: \"hello123\""), "unexpected list-buffers output: {out:?}");

    // `\x02]` (prefix `]`) pastes the newest buffer into the shell -- the
    // paste has no embedded newline, so it lands as unsubmitted text on the
    // current prompt line: one more line now contains "hello123" than before.
    c.send(&ClientMsg::Stdin(vec![0x02, b']']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().filter(|l| l.contains("hello123")).count() > baseline);

    // The pasted text is now pending, unsubmitted input on the prompt line
    // (no shell-specific line-editing assumptions needed for cleanup): kill
    // the server directly.
    let mut cli3 = cli_client(&name);
    cli3.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli3, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn rectangle_selection() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // One command, three consecutive output lines (no prompt lines between
    // them), so a rectangle spanning all three rows is unambiguous.
    c.send(&ClientMsg::Stdin(b"Write-Output \"ABCDEF`nGHIJKL`nMNOPQR\"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "MNOPQR"));
    let top_row = screen_text(&grid).iter().position(|l| l.trim_end() == "ABCDEF").unwrap() as u16;

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));
    let entry_row = grid.cursor().1;

    // Move to (col 1, top_row): `0` then `l` (col 0 -> col 1), then `k` up to
    // the top row.
    let mut moves = vec![b'0', b'l'];
    moves.extend(std::iter::repeat_n(b'k', (entry_row - top_row) as usize));
    c.send(&ClientMsg::Stdin(moves));
    c.recv_output_until(&mut grid, |g| g.cursor().1 == top_row && g.cursor().0 == 1);

    // Begin a selection, toggle rectangle, then extend down 2 rows and right
    // 2 columns (cols 1..=3): rows "ABCDEF"/"GHIJKL"/"MNOPQR" -> "BCD"/"HIJ"/"NOP".
    c.send(&ClientMsg::Stdin(b" v".to_vec()));
    c.send(&ClientMsg::Stdin(b"jjll".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cursor().1 == top_row + 2 && g.cursor().0 == 3);
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));

    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli2, 0);
    // 11 bytes = "BCD\nHIJ\nNOP"; the sample sanitizes control chars
    // (including embedded `\n`) to `?`.
    assert!(out.contains("buffer0: 11 bytes: \"BCD?HIJ?NOP\""), "unexpected list-buffers output: {out:?}");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task 3 review fix (Critical): a selection's ANCHOR is pinned to CONTENT,
/// not to its view row — new pane output arriving while a selection is
/// active scrolls the anchored content up, and both the highlight and the
/// copied text must follow it (the original implementation kept the anchor
/// at its capture-time view row, so it drifted onto unrelated text).
#[test]
fn selection_survives_concurrent_output() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // Fill past the pane height (so scrollback exists and later output
    // scrolls the screen), then print the line we'll anchor on.
    c.send(&ClientMsg::Stdin(b"1..30 | ForEach-Object { \"fillmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("fillmark30")));
    c.send(&ClientMsg::Stdin(b"echo anchor777\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "anchor777"));
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "anchor777").unwrap() as u16;

    // Copy mode; cursor to (col 0, anchor row); begin selection + extend to
    // end of line; highlight lands on the anchor row.
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));
    let entry_row = grid.cursor().1;
    let mut moves = vec![b'0'];
    moves.extend(std::iter::repeat_n(b'k', (entry_row - target_row) as usize));
    c.send(&ClientMsg::Stdin(moves));
    c.recv_output_until(&mut grid, |g| g.cursor() == (0, target_row));
    c.send(&ClientMsg::Stdin(b" $".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cell(0, target_row).style.bg == Color::Idx(3));

    // Concurrent output from a second connection: send-keys into the SAME
    // pane (headless, no -t = the focused pane) makes the shell echo the
    // command, print its output, and draw a fresh prompt — scrolling the
    // screen and capturing more scrollback while our selection is active.
    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["send-keys".into(), "echo extra999".into(), "Enter".into()]));
    expect_cli_done(&cli2, 0);

    // (a) The highlight must FOLLOW the anchored content to its new view
    // row: the row that now holds "anchor777" is highlighted from col 0
    // (it's the selection's first row), and it moved up from target_row.
    let find_anchor_row =
        |g: &Grid| screen_text(g).iter().position(|l| l.trim_end() == "anchor777").map(|r| r as u16);
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("extra999"))
            && match find_anchor_row(g) {
                Some(r) => r < target_row && g.cell(0, r).style.bg == Color::Idx(3),
                None => false,
            }
    });
    let new_row = find_anchor_row(&grid).unwrap();
    for x in 0..9u16 {
        assert_eq!(
            grid.cell(x, new_row).style.bg,
            Color::Idx(3),
            "col {x} of the moved anchor row must stay highlighted"
        );
    }

    // (b) The COPY extracts the anchored content, not whatever sits at the
    // stale view row: the buffer's first line is "anchor777" (the sample
    // sanitizes the joining \n to '?').
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    let mut cli3 = cli_client(&name);
    cli3.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli3, 0);
    assert!(out.contains("bytes: \"anchor777?"), "buffer must start with the anchored line: {out:?}");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Lighter variant of `selection_survives_concurrent_output` for
/// `copy-other-end`: after new output shifts the anchored content up, `o`
/// must jump the cursor to the anchor's NEW (content-pinned) view row, not
/// the stale view row it was captured at.
#[test]
fn other_end_survives_concurrent_output() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(b"1..30 | ForEach-Object { \"fillmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("fillmark30")));
    c.send(&ClientMsg::Stdin(b"echo anchor777\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "anchor777"));
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "anchor777").unwrap() as u16;

    // Anchor at (0, anchor row), cursor moved 3 right.
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));
    let entry_row = grid.cursor().1;
    let mut moves = vec![b'0'];
    moves.extend(std::iter::repeat_n(b'k', (entry_row - target_row) as usize));
    c.send(&ClientMsg::Stdin(moves));
    c.recv_output_until(&mut grid, |g| g.cursor() == (0, target_row));
    c.send(&ClientMsg::Stdin(b" lll".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cursor() == (3, target_row));

    // Concurrent output scrolls the anchored content up.
    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["send-keys".into(), "echo extra888".into(), "Enter".into()]));
    expect_cli_done(&cli2, 0);
    let find_anchor_row =
        |g: &Grid| screen_text(g).iter().position(|l| l.trim_end() == "anchor777").map(|r| r as u16);
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("extra888")) && find_anchor_row(g).is_some_and(|r| r < target_row)
    });
    let new_row = find_anchor_row(&grid).unwrap();

    // `o` jumps to the anchor's CURRENT (content-pinned) position — col 0
    // of the row "anchor777" moved to, not the stale (0, target_row).
    c.send(&ClientMsg::Stdin(b"o".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cursor() == (0, new_row));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn selection_highlight_styled() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    let row = grid.cursor().1;

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Emacs defaults: C-a start-of-line, C-Space begin-selection, C-f x3
    // cursor-right -> selection spans view columns 0..=3.
    c.send(&ClientMsg::Stdin(vec![0x01])); // C-a
    c.send(&ClientMsg::Stdin(vec![0x00])); // C-Space
    c.send(&ClientMsg::Stdin(vec![0x06, 0x06, 0x06])); // C-f C-f C-f
    c.recv_output_until(&mut grid, |g| {
        g.cell(3, row).style.bg == Color::Idx(3) && g.cell(3, row).style.fg == Color::Idx(0)
    });

    // Column 0..=3 highlighted in mode-style (default bg=yellow fg=black);
    // column 4 (just past the selection) is untouched.
    for x in 0..=3u16 {
        assert_eq!(grid.cell(x, row).style.bg, Color::Idx(3), "col {x} should be highlighted");
        assert_eq!(grid.cell(x, row).style.fg, Color::Idx(0), "col {x} should be highlighted");
    }
    assert_ne!(grid.cell(4, row).style.bg, Color::Idx(3), "col 4 must be outside the selection");

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn other_end_swaps() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    let row = grid.cursor().1;

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Anchor at col 0, cursor extended to col 3.
    c.send(&ClientMsg::Stdin(vec![0x01])); // C-a: start-of-line
    c.send(&ClientMsg::Stdin(vec![0x00])); // C-Space: begin-selection (anchor = col 0)
    c.send(&ClientMsg::Stdin(vec![0x06, 0x06, 0x06])); // C-f x3: cursor -> col 3
    c.recv_output_until(&mut grid, |g| g.cursor() == (3, row));

    // `o` (other-end): the LIVE cursor jumps back to the former anchor (col
    // 0) -- proof the anchor/cursor actually swapped, not a no-op.
    c.send(&ClientMsg::Stdin(b"o".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cursor() == (0, row));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn clear_selection_keeps_mode() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    let row = grid.cursor().1;

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(vec![0x01])); // C-a
    c.send(&ClientMsg::Stdin(vec![0x00])); // C-Space: begin-selection
    c.send(&ClientMsg::Stdin(vec![0x06, 0x06, 0x06])); // C-f x3
    c.recv_output_until(&mut grid, |g| g.cell(3, row).style.bg == Color::Idx(3));

    // C-g: clear-selection. The highlight disappears but copy mode itself
    // (the position indicator) stays up -- this command does NOT cancel.
    c.send(&ClientMsg::Stdin(vec![0x07]));
    c.recv_output_until(&mut grid, |g| g.cell(3, row).style.bg != Color::Idx(3));
    assert!(has_indicator(&grid, "[0/"), "copy mode must still be active after clear-selection");

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn list_buffers_format() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set-buffer".into(), "hello world".into()]));
    expect_cli_done(&cli, 0);

    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli2, 0);
    assert_eq!(out, "buffer0: 11 bytes: \"hello world\"\n");

    let mut cli3 = cli_client(&name);
    cli3.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli3, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn delete_buffer_newest() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set-buffer".into(), "first".into()]));
    expect_cli_done(&cli, 0);
    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["set-buffer".into(), "second".into()]));
    expect_cli_done(&cli2, 0);

    // No `-b`: deletes the NEWEST (buffer1, "second"), leaving buffer0.
    let mut cli3 = cli_client(&name);
    cli3.send(&ClientMsg::Cli(vec!["delete-buffer".into()]));
    expect_cli_done(&cli3, 0);

    let mut cli4 = cli_client(&name);
    cli4.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli4, 0);
    assert_eq!(out, "buffer0: 5 bytes: \"first\"\n");

    // Deleting an unknown named buffer is an error.
    let mut cli5 = cli_client(&name);
    cli5.send(&ClientMsg::Cli(vec!["delete-buffer".into(), "-b".into(), "nope".into()]));
    let (_, err) = expect_cli_done(&cli5, 1);
    assert_eq!(err, "buffer not found: nope");

    let mut cli6 = cli_client(&name);
    cli6.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli6, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn set_buffer_named_exempt_from_eviction() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "buffer-limit".into(), "1".into()]));
    expect_cli_done(&cli, 0);

    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["set-buffer".into(), "-b".into(), "keepme".into(), "important".into()]));
    expect_cli_done(&cli2, 0);

    // Two automatic buffers, limit 1: the first automatic is evicted, but
    // the manual "keepme" buffer must survive regardless.
    let mut cli3 = cli_client(&name);
    cli3.send(&ClientMsg::Cli(vec!["set-buffer".into(), "auto-one".into()]));
    expect_cli_done(&cli3, 0);
    let mut cli4 = cli_client(&name);
    cli4.send(&ClientMsg::Cli(vec!["set-buffer".into(), "auto-two".into()]));
    expect_cli_done(&cli4, 0);

    let mut cli5 = cli_client(&name);
    cli5.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli5, 0);
    assert!(out.contains("keepme: 9 bytes: \"important\""), "manual buffer must survive: {out:?}");
    assert!(out.contains("auto-two"), "newest automatic buffer must survive: {out:?}");
    assert!(!out.contains("auto-one"), "oldest automatic buffer must be evicted: {out:?}");

    let mut cli6 = cli_client(&name);
    cli6.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli6, 0);
    server.join().expect("server exits after kill-server");
}

// ---- copy-mode search (Task 4, sub-project 4) ----

/// After a search moves the copy cursor onto a match, the match always
/// starts at view column 0 in every test fixture below (each marker is
/// printed as the ONLY thing on its output line via a `'a'+'b'`-style
/// PowerShell concatenation expression, so the shell's own typed-command
/// echo never contains the search pattern as a contiguous substring and
/// can't accidentally provide a second, unwanted match). This checks the
/// cursor sits at col 0 of a row that STARTS WITH `text` -- a prefix check,
/// not full-line equality, because a match landing on view row 0 shares that
/// row with the `[scroll/history]` position indicator (right-aligned, see
/// `has_indicator`), which `trim_end()` alone would not strip away.
fn cursor_on_line(g: &Grid, text: &str) -> bool {
    let (cx, cy) = g.cursor();
    let want_len = text.chars().count();
    cx == 0
        && screen_text(g)
            .get(cy as usize)
            .map(|s| s.chars().take(want_len).collect::<String>())
            == Some(text.to_string())
}

/// True once the shell has drawn a fresh prompt on the row immediately
/// after `marker` -- i.e. the command that produced `marker` has FULLY
/// finished (no more trailing output, like the next prompt line, still in
/// flight). Entering copy mode as soon as `marker` merely APPEARS
/// (without this check) races: a trailing line arriving even one `Output`
/// frame later would shift every scrolled-back row up by one between when a
/// search computes its match and when the client renders it, since the
/// server's `p.grid` (and thus the row a stored `(scroll, cx, cy)` points
/// at) keeps moving until the shell is truly idle.
fn shell_idle_after(g: &Grid, marker: &str) -> bool {
    let lines = screen_text(g);
    let Some(pos) = lines.iter().position(|l| l.trim_end() == marker) else {
        return false;
    };
    lines.get(pos + 1).map(|l| l.contains("PS ")).unwrap_or(false)
}

#[test]
fn copy_search_finds_in_history() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // "needle$_" in the typed command template never collides with a
    // specific "needle9"-style search pattern (the digit only exists in the
    // INTERPOLATED output), and "needle9" is not a substring of any other
    // "needleNN" line in 1..40 (the two-digit lines all have a different
    // digit before the trailing one).
    c.send(&ClientMsg::Stdin(b"1..40 | ForEach-Object { \"needle$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| shell_idle_after(g, "needle40"));

    // Copy mode enters bound to the LIVE cursor position -- the very newest
    // spot in the whole buffer. A forward search from there must wrap all
    // the way around to the OLDEST retained line before finding anything,
    // so landing on "needle9" here proves the match was found up in
    // history, not just nearby on the live screen.
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // The mode-opening key and the subsequently-typed pattern must arrive
    // in SEPARATE `Stdin` frames (same constraint the existing rename-window
    // prompt tests already respect): capture only takes effect starting the
    // NEXT frame `KeyMachine::feed` decodes, not retroactively within the
    // frame that armed it.
    c.send(&ClientMsg::Stdin(b"/".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Search Down: ")));
    c.send(&ClientMsg::Stdin(b"needle9\r".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "needle9") && !has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_search_backward() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(b"1..40 | ForEach-Object { \"needle$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| shell_idle_after(g, "needle40"));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // `?` (vi backward search): ascends directly from the bottom to
    // "needle23" (unique -- no other 1..40 line contains it as a
    // substring), no wrap required.
    c.send(&ClientMsg::Stdin(b"?".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Search Up: ")));
    c.send(&ClientMsg::Stdin(b"needle23\r".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "needle23"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `n` (search-again) both wraps the buffer boundary AND advances past the
/// current match rather than re-finding it: with exactly two occurrences of
/// "wrapmark" -- one pushed into history, one still on the live screen --
/// a forward search from the very bottom must wrap all the way to the
/// OLDEST one first, and a following `n` must advance to the NEWER one
/// instead of staying put.
#[test]
fn copy_search_next_wraps() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // `'wrap'+'mark'` (string concatenation) prints exactly "wrapmark" as
    // the sole content of its output line; the TYPED command line
    // ("'wrap'+'mark'") never contains "wrapmark" contiguously (the `+` and
    // quotes break it up), so each invocation contributes exactly one match.
    c.send(&ClientMsg::Stdin(b"'wrap'+'mark'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "wrapmark"));

    // Push the first occurrence well into history before printing the
    // second (live) one.
    c.send(&ClientMsg::Stdin(b"1..35 | ForEach-Object { \"fillmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "fillmark35"));
    c.send(&ClientMsg::Stdin(b"'wrap'+'mark'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| shell_idle_after(g, "wrapmark"));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Forward from the very bottom wraps immediately to the OLDEST
    // occurrence (still in history: scroll != 0).
    c.send(&ClientMsg::Stdin(b"/".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Search Down: ")));
    c.send(&ClientMsg::Stdin(b"wrapmark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "wrapmark") && !has_indicator(g, "[0/"));

    // `n`: same direction (forward) again -- advances to the NEWER
    // occurrence (now back on the live screen: scroll == 0), proving it did
    // not just re-find the line it's already sitting on.
    c.send(&ClientMsg::Stdin(b"n".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "wrapmark") && has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Review regression (task-4 review, Critical finding #1): `n` (repeat, same
/// direction) after a BACKWARD search landed the cursor at view column 0
/// must still advance to the OLDER match, not silently re-find the same
/// position. Every match in this test harness lands at column 0 (see
/// `cursor_on_line`'s doc comment), which is exactly the condition that
/// tripped `find_last_in`'s `to_excl.saturating_sub(1)` bug: `to_excl == 0`
/// clamped to `0` (checking column 0 anyway) instead of signaling an empty
/// range, so a backward repeat from column 0 returned the SAME position.
#[test]
fn copy_search_backward_repeat_advances_from_col0() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // Two occurrences of "dupmark", the older pushed well into history by
    // filler output before the newer one is printed live. Both land at
    // column 0 (string-concatenation trick, same as the other search tests,
    // so the typed command echo never itself contains "dupmark").
    c.send(&ClientMsg::Stdin(b"'dup'+'mark'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "dupmark"));
    c.send(&ClientMsg::Stdin(b"1..35 | ForEach-Object { \"fillmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "fillmark35"));
    c.send(&ClientMsg::Stdin(b"'dup'+'mark'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| shell_idle_after(g, "dupmark"));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // `?`: backward from the bottom finds the NEARER (live) occurrence --
    // column 0, scroll == 0.
    c.send(&ClientMsg::Stdin(b"?".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Search Up: ")));
    c.send(&ClientMsg::Stdin(b"dupmark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "dupmark") && has_indicator(g, "[0/"));

    // `n`: same direction (backward) again, repeating from a cursor sitting
    // at column 0. Must advance to the OLDER, scrolled-into-history
    // occurrence (scroll != 0) -- the buggy code left the cursor and
    // `[scroll/history]` indicator completely unchanged instead.
    c.send(&ClientMsg::Stdin(b"n".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "dupmark") && !has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `N` repeats the last search in the OPPOSITE direction: backward first
/// finds the nearer (live) occurrence directly, then `N` reverses to
/// forward and must wrap to the farther (history) one.
#[test]
fn copy_search_capital_n_reverses_direction() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(b"'rev'+'mark'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "revmark"));
    c.send(&ClientMsg::Stdin(b"1..35 | ForEach-Object { \"fillmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "fillmark35"));
    c.send(&ClientMsg::Stdin(b"'rev'+'mark'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| shell_idle_after(g, "revmark"));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // `?`: backward from the bottom finds the NEARER (live) occurrence.
    c.send(&ClientMsg::Stdin(b"?".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Search Up: ")));
    c.send(&ClientMsg::Stdin(b"revmark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "revmark") && has_indicator(g, "[0/"));

    // `N`: reverses to forward, wrapping to the FARTHER (history) occurrence.
    c.send(&ClientMsg::Stdin(b"N".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "revmark") && !has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn copy_search_case_insensitive() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // Output is "MixedCase123"; the typed command never contains that
    // contiguous substring (broken by the `+`).
    c.send(&ClientMsg::Stdin(b"'Mixed'+'Case123'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| shell_idle_after(g, "MixedCase123"));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // All-lowercase pattern still finds the mixed-case line.
    c.send(&ClientMsg::Stdin(b"/".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Search Down: ")));
    c.send(&ClientMsg::Stdin(b"mixedcase123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| cursor_on_line(g, "MixedCase123"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `C-s` (emacs forward search) with no match anywhere: a transient
/// "no match: <pattern>" status message appears (a documented winmux
/// addition -- tmux itself gives no dedicated feedback here).
#[test]
fn copy_search_no_match_message() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Default mode-keys is emacs: C-s opens "Search Down: ".
    c.send(&ClientMsg::Stdin(vec![0x13]));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("Search Down: ")));

    c.send(&ClientMsg::Stdin(b"zzznomatchzzz\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("no match: zzznomatchzzz")));

    // Copy mode itself is still active underneath the message.
    assert!(has_indicator(&grid, "[0/"), "copy mode must still be active after a failed search");

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !screen_text(g).iter().any(|l| l.contains("no match:")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

// ---- mouse (Task 5, sub-project 4) ----

/// Build one SGR mouse frame: `CSI < Cb ; Cx ; Cy (M|m)`, `x`/`y` given
/// 0-based (converted to the wire's 1-based here).
fn sgr_mouse(cb: u8, x: u16, y: u16, release: bool) -> Vec<u8> {
    let final_byte = if release { 'm' } else { 'M' };
    format!("\x1b[<{};{};{}{}", cb, x + 1, y + 1, final_byte).into_bytes()
}

const CB_LEFT: u8 = 0; // button 1, plain press/release (no motion, no modifiers)
const CB_LEFT_DRAG: u8 = 0x20; // button 1 + motion bit
const CB_WHEEL_UP: u8 = 0x40;
const CB_WHEEL_DOWN: u8 = 0x41;

/// Drain any `Output` messages that arrive on `c` within `dur`, feeding them
/// into `grid`, WITHOUT panicking if none arrive -- unlike
/// `Client::recv_output_until`, which requires a predicate to eventually
/// become true. Used to prove a negative (nothing changed) after a bounded
/// wait, once the caller has otherwise synchronized (e.g. a CLI round trip)
/// that the action under test has already been fully processed server-side.
fn drain_briefly(c: &Client, grid: &mut Grid, dur: Duration) {
    let deadline = Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match c.rx.recv_timeout(remaining) {
            Ok(ServerMsg::Output(bytes)) => grid.feed(&bytes),
            Ok(_) | Err(_) => return,
        }
    }
}

fn enable_mouse(name: &str, c: &mut Client) {
    let mut cli = cli_client(name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mouse".into(), "on".into()]));
    expect_cli_done(&cli, 0);
    let _ = c.recv_output_bytes_until_contains(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");
}

/// The screen row a vertical `│` border occupies at column `col`, over every
/// row except the bottom (status) row.
fn has_vertical_border_at(g: &Grid, col: u16) -> bool {
    let pane_rows = g.rows().saturating_sub(1);
    pane_rows > 0 && (0..pane_rows).all(|r| g.cell(col, r).ch == '│')
}

fn find_vertical_border(g: &Grid) -> u16 {
    (1..g.cols().saturating_sub(1))
        .find(|&col| has_vertical_border_at(g, col))
        .expect("expected a vertical split border")
}

fn row_text(g: &Grid, row: u16) -> String {
    (0..g.cols()).map(|x| g.cell(x, row).ch).collect()
}

/// Piggy-back a `Cli` frame on `c` itself (rather than a separate
/// `cli_client`) to synchronize with whatever `Stdin` frames were already
/// sent on THIS SAME connection: a single connection's reader thread feeds
/// the server's event queue strictly in send order, so by the time the
/// returned `CliDone` arrives, every earlier frame on `c` is guaranteed to
/// have been fully processed server-side — no arbitrary sleep needed. Any
/// intervening `Output` frames (e.g. the periodic clock tick, or a mouse
/// event's own — possibly no-op — render) are drained into `grid` rather
/// than tripping up the wait. (SP6 Task 6: hoisted from
/// `mouse_plain_click_in_copy_mode_keeps_mode_and_buffers`, which pioneered
/// this pattern, for reuse by the click-purity/release-targeting tests.)
fn next_cli_done(c: &Client, grid: &mut Grid) -> (u8, String, String) {
    loop {
        match c.recv() {
            ServerMsg::Output(bytes) => grid.feed(&bytes),
            ServerMsg::CliDone { code, out, err } => return (code, out, err),
            other => panic!("unexpected message waiting for CliDone: {other:?}"),
        }
    }
}

impl Client {
    /// Accumulate raw `Output` payload bytes (across as many frames as
    /// needed) until the running buffer contains `needle` somewhere, or 10s
    /// elapse. Unlike `recv_output_until` (which feeds bytes through a
    /// `Grid` VT emulator for screen-content assertions), this is for
    /// asserting on the EXACT bytes the server sent — e.g. the raw mouse
    /// enable/disable escape sequences, which a `Grid` would just silently
    /// consume as unrecognized private-mode CSI sequences.
    fn recv_output_bytes_until_contains(&self, needle: &[u8]) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut buf = Vec::new();
        loop {
            if buf.windows(needle.len()).any(|w| w == needle) {
                return buf;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("timed out waiting for output containing {needle:?}; got {buf:?}");
            }
            match self.rx.recv_timeout(remaining) {
                Ok(ServerMsg::Output(bytes)) => buf.extend(bytes),
                Ok(other) => panic!("unexpected message while waiting for output bytes: {other:?}"),
                Err(_) => panic!("timed out waiting for output containing {needle:?}; got {buf:?}"),
            }
        }
    }
}

#[test]
fn mouse_option_emits_enable_sequences() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let _ = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mouse".into(), "on".into()]));
    expect_cli_done(&cli, 0);
    let _ = c.recv_output_bytes_until_contains(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");

    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mouse".into(), "off".into()]));
    expect_cli_done(&cli2, 0);
    let _ = c.recv_output_bytes_until_contains(b"\x1b[?1000l\x1b[?1002l\x1b[?1006l");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn mouse_click_focuses_pane() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);
    // split-window gives focus to the NEW (right) pane by default.
    assert!(grid.cursor().0 > border_x, "expected focus on the right pane right after split");

    // Left click (press + release) inside the LEFT pane.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, true)));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < border_x);

    // Cleanup: kill-server (two live panes).
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn mouse_wheel_enters_copy_mode() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // Enough scrollback that a 5-line scroll-up isn't clamped to 0.
    c.send(&ClientMsg::Stdin(b"1..40 | ForEach-Object { \"wheelmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("wheelmark40")));

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_WHEEL_UP, 5, 5, false)));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[5/"));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[5/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn mouse_drag_selects_and_release_copies() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(b"echo hello123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "hello123"));
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "hello123").unwrap() as u16;

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Drag-select "hello123" (columns 0..=7, 8 chars) and release.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, target_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 7, target_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 7, target_row, true)));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(out.contains("buffer0: 8 bytes: \"hello123\""), "unexpected list-buffers output: {out:?}");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Regression test for the fix-round Critical finding: a PLAIN click
/// (press then release, no `Drag` frame in between) inside a copy-mode pane
/// must NOT run `copy-selection-and-cancel` -- real tmux's copy-mode table
/// only binds `MouseDragEnd1Pane` (fires after an actual drag) to that
/// action; a bare `MouseUp1Pane` has no default binding at all. Before the
/// fix, SGR button-event tracking's guaranteed `Up` after every `Down`
/// meant EVERY plain click inside copy mode silently exited copy mode and
/// (landing on a non-blank cell) clobbered paste buffer 0 with a
/// 1-character entry.
#[test]
fn mouse_plain_click_in_copy_mode_keeps_mode_and_buffers() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // Two panes; split-window focuses the NEW (right) pane.
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    let mut cli_right_idx = cli_client(&name);
    cli_right_idx.send(&ClientMsg::Cli(vec!["display-message".into(), "#P".into()]));
    let (right_idx, _) = expect_cli_done(&cli_right_idx, 0);

    // Known non-blank content, in the (focused) right pane, to click on.
    c.send(&ClientMsg::Stdin(b"echo hello123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("hello123")));
    let (target_row, click_x) = {
        let text = screen_text(&grid);
        let row = text.iter().position(|l| l.contains("hello123")).unwrap() as u16;
        let col = text[row as usize].find("hello123").unwrap() as u16;
        (row, col)
    };

    // Enter copy mode bound to the RIGHT pane (still focused).
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Move REGISTRY focus away to the LEFT pane via a CLI command -- this
    // bypasses the attached client's own key routing entirely (which, while
    // in copy mode, captures arrow keys for cursor movement instead of
    // `select-pane`), so the plain click below is the ONLY thing that can
    // move focus back to the right pane.
    let mut cli_move = cli_client(&name);
    cli_move.send(&ClientMsg::Cli(vec!["select-pane".into(), "-L".into()]));
    expect_cli_done(&cli_move, 0);
    let mut cli_before = cli_client(&name);
    cli_before.send(&ClientMsg::Cli(vec!["display-message".into(), "#P".into()]));
    let (before_click_idx, _) = expect_cli_done(&cli_before, 0);
    assert_ne!(before_click_idx, right_idx, "select-pane -L must have moved focus off the right pane");

    // Plain click (press + release, NO drag frame at all) on a non-blank
    // cell of the copy-mode-bound right pane.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, click_x, target_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, click_x, target_row, true)));

    // Piggy-back the assertions as `Cli` frames on the SAME connection as
    // the click frames above -- see `next_cli_done`'s doc comment.
    c.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (code, out, _) = next_cli_done(&c, &mut grid);
    assert_eq!(code, 0);
    assert!(!out.contains("buffer0"), "plain click in copy mode must not create a paste buffer: {out:?}");

    // Copy mode must still be active -- the fix's core assertion.
    assert!(has_indicator(&grid, "[0/"), "plain click in copy mode must not exit copy mode");

    c.send(&ClientMsg::Cli(vec!["display-message".into(), "#P".into()]));
    let (code, after_click_idx, _) = next_cli_done(&c, &mut grid);
    assert_eq!(code, 0);
    assert_eq!(after_click_idx, right_idx, "plain click must still focus the clicked (right) pane");

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

// ---- SP6 Task 6: copy-mode mouse feel, part 1 (click purity, release
// targeting, drag-enters-copy-mode) ----

/// (a) `docs/tmux-reference/mouse.md:537-539`: a plain click (`Down` then
/// `Up`, no `Drag` frame between them) inside copy mode is `select-pane`
/// only -- the copy CURSOR must not move, and the click's target cell must
/// not become a new (zero-width) selection anchor. The click's position is
/// deliberately DIFFERENT from the cursor's post-entry position (moved there
/// first via keyboard) so a regression that reinstates "click writes
/// cs.cx/cs.cy unconditionally" is unambiguously caught.
#[test]
fn click_in_copy_mode_does_not_move_cursor() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Move the copy cursor to a known position via keyboard (emacs table:
    // plain arrows move the cursor in copy mode) so it's provably distinct
    // from the click target below.
    let before_move = grid.cursor();
    c.send(&ClientMsg::Stdin(b"\x1b[B\x1b[B\x1b[C\x1b[C\x1b[C".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cursor() != before_move);
    let moved_cursor = grid.cursor();
    assert_ne!(moved_cursor, (0, 0), "test setup: moved cursor must differ from the click target (0,0) below");

    // Plain click (press + release, NO drag frame) at a DIFFERENT cell.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, 0, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, 0, true)));

    // Synchronize on the SAME connection: the server processes a whole
    // batch of already-queued events (Down, Up, this Cli) before rendering,
    // and `CliDone` is sent synchronously DURING that batch while the
    // batch's own Output frame is only sent AFTER -- so `CliDone` can race
    // ahead of the click's own render. That's fine for checking SERVER
    // STATE read directly off the Cli response (buffer creation, below),
    // but NOT for `grid.cursor()`, which only reflects frames actually
    // received. Drain interim Output frames anyway (harmless).
    c.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (code, out, _) = next_cli_done(&c, &mut grid);
    assert_eq!(code, 0);
    assert!(!out.contains("buffer0"), "a plain click in copy mode must not create a paste buffer: {out:?}");

    // Force a genuine, predicate-observable render by making ONE further,
    // real cursor-moving keystroke (Right). If the click had (incorrectly)
    // moved the copy cursor to the clicked cell (0,0) and installed an
    // anchor there, this final position would be (1,0); if the click was
    // correctly a no-op, it's `moved_cursor` shifted right by one.
    c.send(&ClientMsg::Stdin(b"\x1b[C".to_vec()));
    c.recv_output_until(&mut grid, |g| g.cursor() != moved_cursor);
    let expected = (moved_cursor.0 + 1, moved_cursor.1);
    assert_eq!(
        grid.cursor(),
        expected,
        "a plain click in copy mode must not move the copy cursor (click's own no-op render may race a piggy-backed Cli's CliDone, so this checks a real subsequent keystroke's landing position instead)"
    );
    assert!(has_indicator(&grid, "[0/"), "a plain click in copy mode must not exit copy mode");

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_indicator(g, "[0/"));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// (a) The selection anchor a `Drag` installs is the PRESS position, not
/// wherever the first `Drag` event itself happens to report (a fast/coarse
/// physical drag can jump several cells before the terminal emits its first
/// motion frame). Press at col 0, but make the FIRST `Drag` frame already
/// report col 4 -- if the anchor were (incorrectly) taken from that first
/// `Drag` position instead of the remembered press position, the copied
/// text would start at col 4 ("o123") instead of col 0 ("hello123").
#[test]
fn drag_after_click_anchors_at_press_point() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(b"echo hello123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "hello123"));
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "hello123").unwrap() as u16;

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, target_row, false))); // press at col 0
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 4, target_row, false))); // first Drag jumps to col 4
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 7, target_row, false))); // further motion to col 7
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 7, target_row, true))); // release at col 7
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(
        out.contains("buffer0: 8 bytes: \"hello123\""),
        "selection must span the PRESS position (col 0), not the first Drag frame's position (col 4): {out:?}"
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// (b) `docs/tmux-reference/mouse.md:308-311,654-658`: `MouseDragEnd1Pane`
/// resolves against the pane under the pointer AT RELEASE, not the
/// drag-origin pane. Select in the (focused) right pane, but release over
/// the left pane -- no binding exists there for a non-copy-mode pane, so no
/// copy happens; the origin pane must keep its selection and stay in copy
/// mode.
#[test]
fn release_over_other_pane_does_not_copy() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // Two panes; split-window focuses the NEW (right) pane.
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);

    c.send(&ClientMsg::Stdin(b"echo hello123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("hello123")));
    let (target_row, click_x) = {
        let text = screen_text(&grid);
        let row = text.iter().position(|l| l.contains("hello123")).unwrap() as u16;
        let col = text[row as usize].find("hello123").unwrap() as u16;
        (row, col)
    };
    assert!(click_x > border_x, "test setup: click target must be in the RIGHT pane");

    // Enter copy mode bound to the RIGHT pane (still focused).
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Drag-select within the right pane, but RELEASE inside the left pane.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, click_x, target_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, click_x + 7, target_row, false)));
    let release_x = border_x.saturating_sub(2);
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, release_x, target_row, true)));

    // Synchronize on the SAME connection and drain any interim Output
    // frames into `grid`.
    c.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (code, out, _) = next_cli_done(&c, &mut grid);
    assert_eq!(code, 0);
    assert!(!out.contains("buffer0"), "releasing over a DIFFERENT pane must not copy the selection: {out:?}");

    assert!(has_indicator(&grid, "[0/"), "copy mode must remain active when the release lands on a different pane");

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// (c) `docs/tmux-reference/mouse.md:488,501`: the root table's
/// `MouseDrag1Pane -> copy-mode -M` -- a press+motion (button 1) on a LIVE
/// pane (not already in copy mode) enters copy mode immediately, anchored
/// at the press point, with the selection following the drag; the
/// subsequent release copies-and-cancels exactly like any copy-mode drag.
#[test]
fn drag_on_live_pane_enters_copy_mode_selecting() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(b"echo hello123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "hello123"));
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "hello123").unwrap() as u16;

    // NOT in copy mode. Press then drag (button 1) directly on the live
    // pane: the first Drag frame must enter copy mode, anchored at the
    // press point (col 0), with the selection already following the drag.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, target_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 7, target_row, false)));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 7, target_row, true)));
    c.recv_output_until(&mut grid, |g| !row0(g).contains("[0/"));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(
        out.contains("buffer0: 8 bytes: \"hello123\""),
        "drag on a live pane must enter copy mode selecting from the press point: {out:?}"
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

// ---- drag autoscroll + word/line drag extension (Task 7, SP6 wave 2) ----

/// `docs/tmux-reference/mouse.md` §5.4: while a drag selection is held with
/// the pointer on the pane's FIRST row, the view scrolls one line and the
/// selection extends every `MOUSE_DRAG_AUTOSCROLL_INTERVAL` (50ms) of real
/// time -- serviced by the server's own `Tick`, not by any further mouse
/// event from the client (matches the escape-time flush test's pattern:
/// hold still and let the server's real timer do the work). Presses then
/// drags to (0, 0) -- immediately the pane's top row -- and, having sent NO
/// further mouse events, waits for the `[scroll/history]` indicator to climb
/// to `[5/` purely from Tick-driven autoscroll (the Drag event itself only
/// accounts for ONE line of that). Releasing then must have copied a
/// selection spanning several distinct rows, not just the original
/// zero-width point.
#[test]
fn drag_at_top_row_autoscrolls_into_history() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // More than one page (23 pane rows on an 80x24 terminal) of scrollback.
    c.send(&ClientMsg::Stdin(b"1..150 | ForEach-Object { \"histmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("histmark150")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Press then drag to the pane's TOP row (0), col 0 -- the pointer parked
    // on the edge row. This Drag installs a zero-width Char-kind selection
    // anchored exactly here and, per `service_drag_edge`, fires ONE
    // immediate extra scroll line (scroll -> 1) in addition to arming the
    // autoscroll timer.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, 0, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 0, 0, false)));

    // No further mouse events at all: hold the pointer there and let the
    // server's real 50ms Tick drive the autoscroll timer the rest of the
    // way to scroll == 5 (four MORE lines beyond the Drag's own immediate
    // one).
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[5/"));

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, 0, true)));
    c.recv_output_until(&mut grid, |g| !row0(g).trim_end().ends_with(']'));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(out.contains("buffer0:"), "expected a copied selection: {out:?}");
    // Each autoscroll tick scrolls one MORE line and re-extends the
    // selection by one more row; a multi-row selection's embedded newlines
    // show up sanitized as `?` in the `list-buffers` sample
    // (`buffers::sample`) -- having reached scroll >= 5 before release, the
    // selection must span at least 5 distinct rows (>= 4 row breaks).
    let row_breaks = out.matches('?').count();
    assert!(
        row_breaks >= 4,
        "expected the drag-autoscroll selection to span several rows (>=5), only found {row_breaks} row breaks: {out:?}"
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task 7 fix round 1 (review, Moderate): `docs/tmux-reference/mouse.md`
/// §5.4 -- "motion outside the pane is a no-op that STOPS the timer". The
/// three early-exit guards in `dispatch_mouse` (overlay-open, status-row
/// diversion, outside-pane-area) reset `client.mouse.drag` but originally
/// did NOT clear `client.mouse.autoscroll`; since `service_autoscroll_tick`
/// only self-disarms when the client leaves copy mode or the pane vanishes,
/// a drag armed at a pane edge whose pointer then moved onto the status row
/// (adjacent to the pane's LAST row -- and reachable from the TOP row too
/// with `status-position top`, exercised here so the armed scroll direction
/// is the easy-to-assert into-history one) kept scrolling 1 line per 50ms
/// forever. Arms autoscroll at the pane's top row, proves it is advancing,
/// drags onto the status row, then samples the indicator's scroll offset
/// twice ~300ms apart (6+ tick intervals) and asserts it stopped.
#[test]
fn autoscroll_stops_when_drag_leaves_onto_status_row() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // Status bar on TOP: its row (0) is adjacent to the pane's FIRST row
    // (1), so an upward edge-drag can overshoot onto it -- and the armed
    // autoscroll direction is "into history", which the `[N/M]` indicator
    // makes directly observable. (The bottom-edge variant is the same guard
    // and the same bug; this construction just asserts more simply.)
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "status-position".into(), "top".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| row_text(g, 0).contains("[0]"));

    // Plenty of history so the runaway scroll (RED) has room to keep
    // visibly advancing at both sample points.
    c.send(&ClientMsg::Stdin(b"1..300 | ForEach-Object { \"histmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("histmark300")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    // With status on top the indicator is on the pane's own first row,
    // which is screen row 1 here -- `has_indicator`/`row0` read screen row
    // 0, so wait on the pane row directly.
    c.recv_output_until(&mut grid, |g| row_text(g, 1).contains("[0/"));

    // Press + drag to the pane's TOP row (screen y = 1): arms autoscroll
    // into history (one immediate line + the 50ms timer).
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 0, 1, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 0, 1, false)));
    // Prove the timer is genuinely running before the overshoot.
    c.recv_output_until(&mut grid, |g| row_text(g, 1).contains("[5/"));

    // Overshoot: drag onto the status row (screen y = 0). This hits
    // `dispatch_mouse`'s status-row guard, which must stop the timer.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 0, 0, false)));
    // Synchronize on the same connection so the Drag has been fully
    // processed server-side before sampling (`next_cli_done` pattern; no
    // drag-END was sent so no buffer exists yet, and `list-buffers` is a
    // clean exit-0 no-op either way).
    c.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (code, _, _) = next_cli_done(&c, &mut grid);
    assert_eq!(code, 0);

    // Two samples ~300ms apart (6+ autoscroll intervals), parsing the
    // `[N/M]` indicator's scroll offset from the pane's first row (screen
    // row 1 under `status-position top`).
    let sample = |g: &Grid| -> Option<u32> {
        let row = row_text(g, 1);
        let trimmed = row.trim_end();
        let open = trimmed.rfind('[')?;
        let rest = &trimmed[open + 1..];
        let slash = rest.find('/')?;
        rest[..slash].parse().ok()
    };
    drain_briefly(&c, &mut grid, Duration::from_millis(150));
    let first = sample(&grid).expect("copy-mode indicator visible after overshoot");
    drain_briefly(&c, &mut grid, Duration::from_millis(300));
    let second = sample(&grid).expect("copy-mode indicator still visible");
    assert_eq!(
        first, second,
        "autoscroll must STOP once the drag pointer leaves the pane onto the status row (mouse.md §5.4); scroll kept advancing {first} -> {second}"
    );

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// `docs/tmux-reference/mouse.md` :636-640 / `copy-mode-and-buffers.md`
/// :440-447: after DoubleClick installs a `SelKind::Word` anchor
/// (`select_word_at`), continuing to drag snaps the MOVING end to the whole
/// word under the cursor, not the raw cell. Double-clicks "beta" (columns
/// 6..=9 of "alpha beta gamma delta") then drags into the MIDDLE of "delta"
/// (column 19, mid-word) -- if the drag extended cell-by-cell instead of
/// snapping, the copied text would end mid-word ("...gamma del"); snapping
/// must capture the WHOLE trailing word instead.
#[test]
fn drag_after_double_click_extends_by_words() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(b"echo \"alpha beta gamma delta\"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "alpha beta gamma delta"));
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "alpha beta gamma delta").unwrap() as u16;

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Double-click "beta" (col 7, the 'e'): Down, Up, Down -- the second
    // Down reaches run==2, installing the word anchor immediately
    // (`mouse_down`'s `DoubleClick1Pane` -> `select-word` path).
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 7, target_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 7, target_row, true)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 7, target_row, false)));

    // Drag into the MIDDLE of "delta" (col 19, its 'l') and release there.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 19, target_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 19, target_row, true)));
    c.recv_output_until(&mut grid, |g| !row0(g).trim_end().ends_with(']'));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    assert!(
        out.contains("\"beta gamma delta\""),
        "drag after double-click must snap the moving end to the whole word under the cursor: {out:?}"
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `docs/tmux-reference/mouse.md` :636-640 (SEL_LINE branch) /
/// `copy-mode-and-buffers.md` :440-447: after TripleClick installs a
/// `SelKind::Line` anchor (`select_line_at`), continuing to drag snaps the
/// moving end to the WHOLE line under the cursor. Triple-clicks the
/// "firstrow" line then drags onto (the middle of) the "secondrow" line a
/// few rows below -- the copied selection must contain both lines in full,
/// not just up to the drag's column.
#[test]
fn drag_after_triple_click_extends_by_lines() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(b"echo firstrow\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "firstrow"));
    c.send(&ClientMsg::Stdin(b"echo secondrow\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.trim_end() == "secondrow"));

    let (row1, row2) = {
        let text = screen_text(&grid);
        let r1 = text.iter().position(|l| l.trim_end() == "firstrow").unwrap() as u16;
        let r2 = text.iter().position(|l| l.trim_end() == "secondrow").unwrap() as u16;
        (r1, r2)
    };
    assert!(row2 > row1, "test setup: secondrow must be below firstrow");

    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // Triple-click "firstrow" (Down, Up, Down, Up, Down -- run reaches 3).
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 2, row1, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 2, row1, true)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 2, row1, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 2, row1, true)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 2, row1, false)));

    // Drag down onto the middle of "secondrow"'s row and release there.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 3, row2, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 3, row2, true)));
    c.recv_output_until(&mut grid, |g| !row0(g).trim_end().ends_with(']'));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, _) = expect_cli_done(&cli, 0);
    // The copied sample's embedded newlines show up sanitized as `?`
    // (`buffers::sample`): "firstrow?" proves the FULL first line was
    // captured (not truncated mid-line) before the row break, and
    // "?secondrow" proves the second line starts fresh on its own row.
    assert!(out.contains("firstrow?"), "expected the whole first line captured: {out:?}");
    assert!(out.contains("?secondrow"), "expected the whole second line captured: {out:?}");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Attach, enable mouse, create a second window, and rename both windows to
/// fixed short names (`w0`/`w1`) so the status line's exact tab text is
/// deterministic for hit-testing — window 1 (`w1`) ends up current.
fn setup_two_named_windows(name: &str) -> (Client, Grid) {
    let mut c = Client::connect(name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(name, &mut c);

    let mut cli = cli_client(name);
    cli.send(&ClientMsg::Cli(vec!["rename-window".into(), "w0".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| row_text(g, g.rows() - 1).contains("0:w0*"));

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| row_text(g, g.rows() - 1).contains("1:"));

    let mut cli2 = cli_client(name);
    cli2.send(&ClientMsg::Cli(vec!["rename-window".into(), "w1".into()]));
    expect_cli_done(&cli2, 0);
    c.recv_output_until(&mut grid, |g| {
        let row = row_text(g, g.rows() - 1);
        row.contains("1:w1*") && row.contains("0:w0-")
    });

    (c, grid)
}

#[test]
fn mouse_status_click_selects_window() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let (mut c, mut grid) = setup_two_named_windows(&name);
    let status_row = grid.rows() - 1;
    let line = row_text(&grid, status_row);
    let tab0_col = line.find("0:w0-").expect("window0 tab must be on the status line") as u16;

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, tab0_col, status_row, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, tab0_col, status_row, true)));

    c.recv_output_until(&mut grid, |g| {
        let row = row_text(g, status_row);
        row.contains("0:w0*") && row.contains("1:w1-")
    });

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn mouse_wheel_status_cycles_windows() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let (mut c, mut grid) = setup_two_named_windows(&name);
    let status_row = grid.rows() - 1;

    // WheelUp on the status row -> previous-window (tmux default
    // `WheelUpStatus`): w1 (current) -> w0.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_WHEEL_UP, 0, status_row, false)));
    c.recv_output_until(&mut grid, |g| row_text(g, status_row).contains("0:w0*"));

    // WheelDown -> next-window (`WheelDownStatus`): back to w1.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_WHEEL_DOWN, 0, status_row, false)));
    c.recv_output_until(&mut grid, |g| row_text(g, status_row).contains("1:w1*"));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn mouse_border_drag_resizes() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);
    let target_x = border_x + 10;

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, border_x, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, target_x, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, target_x, 5, true)));

    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, target_x));
    assert!(!has_vertical_border_at(&grid, border_x), "border must have actually moved");

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Regression test for follow-up #66: dragging a vertical border LEFTWARD --
/// toward the LEFT pane's OWN edge, shrinking pane 1 (the split's first
/// child, which `mouse_down`'s `VBorder{ left }` hit-test always binds as
/// `mouse_drag_border`'s reference for the WHOLE gesture) and growing pane 2
/// -- must resize the split live, exactly like the rightward drag
/// `mouse_border_drag_resizes` above already covers (real tmux: a border
/// drag moves that border in EITHER direction). Pre-fix this is a silent
/// no-op: `Layout::resize_from` only accepts a first-child reference for
/// `Direction::Right`/`Down` (see `layout::tests::
/// resize_from_reference_pane_ignores_focus` and its sibling
/// `resize_from_first_child_reference_rejects_shrink_direction`), and
/// `mouse_drag_border` never re-resolves the reference pane per-direction --
/// confirmed empirically: the border never moves at all on a single
/// leftward-only drag, fresh state, no staleness involved. See
/// docs/follow-ups.md #66.
#[test]
fn mouse_border_drag_resizes_leftward() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);
    let target_x = border_x - 10;

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, border_x, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, target_x, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, target_x, 5, true)));

    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, target_x));
    assert!(!has_vertical_border_at(&grid, border_x), "border must have actually moved left");

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Same defect, horizontal-border leg (`HBorder{ top }` / `Direction::Up`):
/// dragging a horizontal border UPWARD -- toward the TOP pane's own edge,
/// shrinking pane 1 and growing pane 2 -- must resize live, mirroring
/// `mouse_border_drag_resizes_leftward` above. See docs/follow-ups.md #66.
#[test]
fn mouse_border_drag_resizes_upward() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    c.recv_output_until(&mut grid, has_horizontal_border);
    let border_y = find_horizontal_border(&grid);
    let target_y = border_y - 5;

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, border_y, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 5, target_y, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, target_y, true)));

    c.recv_output_until(&mut grid, |g| has_horizontal_border_at(g, target_y));
    assert!(!has_horizontal_border_at(&grid, border_y), "border must have actually moved up");

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Baseline non-regression coverage for the "border drag works once then
/// dies" bug (SP6 gap analysis §D): two consecutive, fully independent
/// clean press-drag-release cycles (both releasing inside the pane area, no
/// status-row/overlay interruption of either) must both actually resize --
/// the baseline `mouse_border_drag_resizes` above only ever exercises one.
///
/// Empirically this already passes on BOTH sides of the Task 1 fix (not a
/// RED test): `mouse_down`'s `VBorder`/`HBorder` arms unconditionally
/// overwrite `client.mouse.drag` on every `Down` that lands cleanly on a
/// real border, so a second, fully legitimate press always re-arms
/// correctly regardless of what staleness preceded it -- staleness only
/// bites when a `Drag`/`Up` arrives WITHOUT a fresh preceding `Down` (see
/// `mouse_border_drag_release_on_status_row_then_drag_again` below, and the
/// task report, for the actual RED/GREEN reproduction and the full
/// investigation). This test is still valuable as a regression guard on
/// the fix itself: an incorrect/overzealous drag-state reset could easily
/// have broken this exact "second clean drag" case, and this test would
/// catch that.
///
/// Both drags move the border further RIGHT (never left): `VBorder{ left }`
/// binds the LEFT pane (the split's first child) as `mouse_drag_border`'s
/// fixed resize-reference for the entire gesture, and `Layout::resize_from`
/// only accepts a first-child reference for `Direction::Right` (see
/// `layout::tests::resize_from_reference_pane_ignores_focus`) -- a LEFTWARD
/// drag would need the second-child (right) pane as reference instead, which
/// `mouse_drag_border` never resolves (confirmed empirically: even a SINGLE
/// leftward drag never moves the border). That direction/reference-pane
/// mismatch is a separate, always-reproducible pre-existing bug, out of
/// Task 1's drag-STATE-lifecycle scope (see this task's report); rightward
/// keeps this test isolated to the staleness bug this task addresses.
#[test]
fn mouse_border_drag_twice_resizes_twice() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);
    let target_x1 = border_x + 4;

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, border_x, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, target_x1, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, target_x1, 5, true)));
    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, target_x1));
    assert!(!has_vertical_border_at(&grid, border_x), "first drag must have actually moved the border");

    // Second, independent drag: 4 more columns right. This is the case that
    // fails today -- the first drag's stale `MouseDrag::Border` survives
    // (nothing in the fixed-vs-buggy input sequence differs from a real
    // user's second drag), and `mouse_drag_border` no-ops forever after.
    let target_x2 = target_x1 + 4;
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, target_x1, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, target_x2, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, target_x2, 5, true)));
    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, target_x2));
    assert!(!has_vertical_border_at(&grid, target_x1), "second drag must have actually moved the border again");

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Regression test for the status-row leg of the same bug (SP6 gap analysis
/// §D point 1): `dispatch_mouse`'s status-row short-circuit
/// (`dispatch.rs:1620-1624`) diverts Drag/Up events landing on the status
/// row to `dispatch_mouse_status`, which ignores them -- so a horizontal
/// border drag whose RELEASE overshoots onto the status row (very reachable:
/// the status bar sits immediately below the pane area) leaves
/// `client.mouse.drag` stuck at `Border`.
///
/// Construction note (deviates from the task brief's literal "press again,
/// motion, release inside the pane area" wording -- see this task's report
/// for the full empirical investigation): a SECOND, fully independent
/// press-drag-release cycle turns out NOT to reproduce a failure here, on
/// EITHER side of the fix, because `mouse_down`'s `VBorder`/`HBorder` arms
/// unconditionally overwrite `client.mouse.drag` on every `Down` that hits a
/// real border -- so a legitimate fresh press always re-arms correctly
/// regardless of what staleness came before. What the leftover
/// `MouseDrag::Border` actually enables is a Drag frame arriving WITHOUT an
/// intervening Down (a real-world possibility: buffered/coalesced motion
/// reports trailing a release, or simply a terminal quirk) being
/// misinterpreted as a live drag and moving the border using a reference
/// pane nobody currently pressed -- exactly the "revivable by a later
/// out-of-sequence Drag/Up frame with no intervening Down" failure mode
/// `docs/follow-ups.md` #64 describes for the sibling overlay guard. This
/// test reproduces that directly: after the status-row-swallowed release,
/// a bare `Drag` frame (no `Down`) must NOT move the border.
#[test]
fn mouse_border_drag_release_on_status_row_then_drag_again() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    c.recv_output_until(&mut grid, has_horizontal_border);
    let border_y = find_horizontal_border(&grid);
    let mid_y = border_y + 3;
    let status_row = grid.rows() - 1;

    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, border_y, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 5, mid_y, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, status_row, true)));
    c.recv_output_until(&mut grid, |g| has_horizontal_border_at(g, mid_y));
    assert!(!has_horizontal_border_at(&grid, border_y), "first drag must have actually moved the border");

    // Out-of-sequence: a `Drag` frame with NO preceding `Down`. Before the
    // fix, `client.mouse.drag` is still the stale `Border{ pane: top,
    // vertical: false }` left by the swallowed release above, so
    // `mouse_drag_border` runs and moves the border to `target_y` using
    // that leftover reference -- a spurious resize nobody pressed for.
    // After the fix, `client.mouse.drag` was reset to `None` by the
    // status-row guard, so a bare `Drag` is correctly inert.
    let target_y = mid_y + 3;
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT_DRAG, 5, target_y, false)));

    // Synchronize on a fresh CLI round trip (a separate connection, but the
    // server's single event loop processes messages in the order its
    // reader threads forward them, and this request is issued and its
    // response awaited well after the Drag frame above was sent) so the
    // Drag frame is guaranteed fully processed before checking state, then
    // drain whatever `Output` the Drag frame produced (if any) into `grid`.
    let mut sync_cli = cli_client(&name);
    sync_cli.send(&ClientMsg::Cli(vec!["list-windows".into()]));
    expect_cli_done(&sync_cli, 0);
    drain_briefly(&c, &mut grid, Duration::from_millis(500));

    assert!(
        has_horizontal_border_at(&grid, mid_y),
        "an out-of-sequence Drag with no Down must NOT move the border (stale drag state revived)"
    );
    assert!(!has_horizontal_border_at(&grid, target_y), "the border must still be at mid_y, unmoved");

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

// The overlay leg of the same bug (`docs/follow-ups.md` #64): the
// choose-tree/display-panes mouse guard in `dispatch_mouse` swallows
// Drag/Up events while an overlay is open but (before this fix) did not
// clear `client.mouse.drag`. Arms a border drag via a raw keyboard/config
// path that reaches `dispatch_mouse` at the unit level (not exercisable
// from a conformant SGR mouse stream through the e2e-style pipe harness,
// since real terminals never send Drag/Up without a preceding Down and the
// overlay guard sits ahead of the border/pane hit-test), asserting the
// guard now resets `MouseDrag::None` before returning. See
// `src/server/dispatch.rs`'s `mouse_dispatch_tests` module for the actual
// coverage (`mouse_drag_cleared_when_overlay_swallows_release`) -- unlike
// this file, that module constructs a `Server`/`ClientState` directly, so
// it can set `client.mouse.drag` to a known `Border` value and open an
// overlay without needing a real intervening Down event.

// `alt_screen_wheel_sends_arrows`: the task brief's suggested e2e approach
// (a PowerShell one-liner writes the raw `CSI ?1049h` bytes itself,
// `Write-Host -NoNewline "$([char]27)[?1049h"`) was tried here first and
// found to be exactly the "too flaky" case the brief anticipated -- real
// Windows ConPTY does not reliably pass a bare `Write-Host`-emitted
// `CSI ?1049h` through to the server's read side as the literal
// alt-screen-enter sequence (observed: the pane visibly cleared and
// PowerShell's prompt reprinted, consistent with SOME redraw happening, but
// the server pane's `Grid::alt_screen()` never actually flipped true, so a
// wheel event dispatched right after still entered copy mode instead of
// translating to arrows -- a ConPTY passthrough quirk for a synthetic/naive
// escape injection, not a winmux bug). Per the brief's own documented
// fallback, alt-screen wheel routing is instead covered at the server
// dispatch unit level: see `src/server/dispatch.rs`'s
// `mouse_dispatch_tests::alt_screen_wheel_does_not_enter_copy_mode` /
// `live_screen_wheel_enters_copy_mode`, which build a real `Server` +
// `Registry` session/pane directly and feed `\x1b[?1049h` straight into the
// pane's `Grid` (no ConPTY involved), exercising the exact same
// `p.grid.alt_screen()` check `dispatch::Server::mouse_wheel` branches on.
// `grid::tests::alt_screen_getter_tracks_mode` separately covers that the
// `Grid` itself correctly tracks alt-screen state end to end.

// ---- layout presets, swap-pane, rotate-window (Task 6, sub-project 4) -----

/// True if some row (excluding the bottom status row) is a full run of `─`
/// across every column -- a pure horizontal split border (no other border
/// crossing it, so no `┬`/`┴`/`┼` junction characters).
fn has_horizontal_border(g: &Grid) -> bool {
    let pane_rows = g.rows().saturating_sub(1);
    (0..pane_rows).any(|r| (0..g.cols()).all(|c| g.cell(c, r).ch == '─'))
}

/// True if row `row` (must be strictly above the bottom status row) is a
/// full run of `─` across every column -- mirrors `has_vertical_border_at`'s
/// pattern but for a horizontal split border.
fn has_horizontal_border_at(g: &Grid, row: u16) -> bool {
    let pane_rows = g.rows().saturating_sub(1);
    row < pane_rows && (0..g.cols()).all(|c| g.cell(c, row).ch == '─')
}

/// The row of the first full-width horizontal split border, or panics if
/// there isn't one -- mirrors `find_vertical_border`.
fn find_horizontal_border(g: &Grid) -> u16 {
    let pane_rows = g.rows().saturating_sub(1);
    (0..pane_rows).find(|&row| has_horizontal_border_at(g, row)).expect("expected a horizontal split border")
}

/// The column of the first occurrence of `marker` in any row, or `None` if
/// it isn't (yet) on screen anywhere.
fn marker_col(g: &Grid, marker: &str) -> Option<u16> {
    (0..g.rows()).find_map(|r| row_text(g, r).find(marker).map(|c| c as u16))
}

/// Like `marker_col`, but only searches rows in `[row_lo, row_hi)` -- used
/// to distinguish which of several same-side panes a marker landed in.
fn marker_col_in_rows(g: &Grid, marker: &str, row_lo: u16, row_hi: u16) -> Option<u16> {
    (row_lo..row_hi.min(g.rows())).find_map(|r| row_text(g, r).find(marker).map(|c| c as u16))
}

#[test]
fn space_cycles_layouts() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Two manual HORIZONTAL splits (prefix-% twice) build a plain 3-in-a-row
    // tree, but with SKEWED ratios (borders at columns 40 and 60) rather
    // than the preset's evenly-balanced columns (26 and 53) -- a manual
    // layout that's already topologically "even-horizontal"-shaped, but at
    // different border positions, so applying the preset is still visibly
    // distinguishable.
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, 60));
    assert!(has_vertical_border_at(&grid, 40), "expected the first split's border to remain at column 40");
    assert!(!has_vertical_border_at(&grid, 26), "borders must not already be at the preset's columns");

    // prefix-Space applies next-layout: from `last_layout == None`, the
    // first press lands on cycle index 0 (even-horizontal) -- three EVENLY
    // spread panes in one row, borders at columns 26 and 53.
    c.send(&ClientMsg::Stdin(vec![0x02, b' ']));
    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, 26) && has_vertical_border_at(g, 53));

    // A second press advances to index 1 (even-vertical): three panes
    // stacked, horizontal borders only (each spanning the FULL width, since
    // there's no more nested split), no vertical border anywhere.
    c.send(&ClientMsg::Stdin(vec![0x02, b' ']));
    c.recv_output_until(&mut grid, |g| has_horizontal_border(g) && !has_vertical_border(g));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn select_layout_by_name() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    // Default main-pane-width (80) exceeds an 80-col window, so it clamps:
    // total=80, MIN=2, max_main = 80-1-2 = 77 -> border at column 77.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["select-layout".into(), "main-vertical".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, 77));

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn main_pane_width_option_respected() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "main-pane-width".into(), "30".into()]));
    expect_cli_done(&cli, 0);
    cli.send(&ClientMsg::Cli(vec!["select-layout".into(), "main-vertical".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| has_vertical_border_at(g, 30));

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn swap_pane_braces() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // Split: pane1 (left, {0,0,40,24}) | pane2 (right, {41,0,39,24},
    // focused -- split-window gives the new pane focus).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);

    c.send(&ClientMsg::Stdin(b"echo right123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "right123").is_some());

    // Click into the left pane to focus pane1, then mark it too.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, true)));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < border_x);
    c.send(&ClientMsg::Stdin(b"echo left456\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "left456").is_some());

    // prefix-{ = swap-pane -U: with only two panes, this swaps them
    // outright. Focus follows the active pane (pane1, currently focused) to
    // its new position -- the RIGHT side -- so pane1's content ("left456")
    // ends up right of the border and pane2's ("right123") ends up left of
    // it; the border column itself is unchanged (swap relabels leaves, it
    // doesn't touch the tree's ratios).
    c.send(&ClientMsg::Stdin(vec![0x02, b'{']));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > border_x);
    assert!(has_vertical_border_at(&grid, border_x), "border column must not move on a swap");
    c.recv_output_until(&mut grid, |g| marker_col(g, "right123").map(|c| c < border_x).unwrap_or(false));
    c.recv_output_until(&mut grid, |g| marker_col(g, "left456").map(|c| c > border_x).unwrap_or(false));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn rotate_window_ctrl_o() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // H(1, V(2,3)): pane1 {0,0,40,24}, pane2 {41,0,39,12} (focused after the
    // first split), pane3 {41,13,39,11} (focused after the second split).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    c.send(&ClientMsg::Stdin(b"echo two222\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "two222").is_some());
    assert!(marker_col(&grid, "two222").unwrap() > 40, "pane2's marker must start out on the right");

    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    // pane3 (the new, focused pane after the vertical split) sits at
    // {41,13,39,11} -- wait for the cursor to land there rather than
    // `has_horizontal_border` (that helper requires a FULL-width border row,
    // but this nested split's border only spans the right half).
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40 && g.cursor().1 >= 13);

    // Click into pane1 (left) to focus it before rotating.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 5, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 5, true)));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < 40);

    // prefix-C-o = bare rotate-window (C-o is byte 0x0f). Per
    // `Layout::rotate`'s `forward=false` permutation (bare rotate-window
    // maps to `down: false`), leaf position 0 (pane1's old rect,
    // {0,0,40,24}) ends up showing pane2's content -- pane2's "two222"
    // marker moves from the right half of the screen to the left half.
    c.send(&ClientMsg::Stdin(vec![0x02, 0x0f]));
    c.recv_output_until(&mut grid, |g| marker_col(g, "two222").map(|c| c < 40).unwrap_or(false));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

// ---- Task 6 fix round: swap-pane review findings --------------------------

/// Review finding #1: cross-window `swap-pane -s`/`-t` used to silently
/// no-op (exit 0, nothing changed). Must now be an explicit, honest error --
/// winmux does not (yet) support moving a pane between windows via
/// `swap-pane`, unlike real tmux.
#[test]
fn swap_pane_cross_window_errors() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Window 0's only pane: mark it.
    c.send(&ClientMsg::Stdin(b"echo win0mark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "win0mark").is_some());

    // prefix-c creates window 1 and switches the client to it.
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));
    c.send(&ClientMsg::Stdin(b"echo win1mark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "win1mark").is_some());

    // `swap-pane -s 0.0 -t 1.0` targets window 0's pane and window 1's pane
    // -- a cross-window pair. Must fail loudly, not silently succeed.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["swap-pane".into(), "-s".into(), "0.0".into(), "-t".into(), "1.0".into()]));
    let (_, err) = expect_cli_done(&cli, 1);
    assert_eq!(err, "swap-pane: can only swap panes within the same window");

    // Both panes are untouched: window 1 (still current) still shows
    // win1mark, and switching back to window 0 still shows win0mark.
    assert!(marker_col(&grid, "win1mark").is_some(), "window 1's pane must be unchanged after the rejected swap");
    c.send(&ClientMsg::Stdin(vec![0x02, b'0']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));
    c.recv_output_until(&mut grid, |g| marker_col(g, "win0mark").is_some());

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Review Minor finding: the explicit `-s`/`-t` same-window path had zero
/// durable automated coverage (the reviewer's throwaway ad hoc test was
/// reverted). This is that permanent test.
#[test]
fn swap_pane_explicit_targets() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // Split: pane1 (left, {0,0,40,24}, leaf index 0) | pane2 (right,
    // {41,0,39,24}, leaf index 1, focused -- split-window gives the new
    // pane focus).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);

    c.send(&ClientMsg::Stdin(b"echo right123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "right123").is_some());

    // Click into the left pane to focus pane1, then mark it too.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, true)));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < border_x);
    c.send(&ClientMsg::Stdin(b"echo left456\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "left456").is_some());

    // Explicit `swap-pane -s 0 -t 1` (leaf-order indices: 0 = left, 1 =
    // right) -- the durable regression test for the self-caught
    // session-lookup bug fixed during Task 6 (see task-6-report.md). Focus
    // follows the acting (left, currently focused) pane to its new position
    // -- the RIGHT side, exactly like the `{` binding's `swap_pane_braces`
    // case, but driven via the explicit-target path instead of `-U`.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["swap-pane".into(), "-s".into(), "0".into(), "-t".into(), "1".into()]));
    expect_cli_done(&cli, 0);

    c.recv_output_until(&mut grid, |g| g.cursor().0 > border_x);
    assert!(has_vertical_border_at(&grid, border_x), "border column must not move on a swap");
    c.recv_output_until(&mut grid, |g| marker_col(g, "right123").map(|c| c < border_x).unwrap_or(false));
    c.recv_output_until(&mut grid, |g| marker_col(g, "left456").map(|c| c > border_x).unwrap_or(false));

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Review finding #3: `swap-pane -U`/`-D` used to silently ignore a
/// co-supplied `-t`, always operating on the acting client's ACTIVE pane.
/// Real tmux uses `-t` to select WHICH pane is swapped up/down (defaulting
/// to the active pane only when `-t` is absent). This test targets a
/// non-focused pane explicitly and proves the swap follows `-t`, not focus:
/// a 3-pane nested layout (H(1, V(2,3))), focus left on pane3
/// (bottom-right), then `swap-pane -D -t 0` (pane1, left, position 0).
/// `-D` swaps position 0 with the NEXT pane in creation order (position 1,
/// pane2/top-right) -- NOT with the active pane (pane3/position 2). Old
/// (ignoring -t) behavior would instead swap the active pane (position 2)
/// with position 0, moving the cursor to the left pane and putting pane3's
/// content there; the fixed behavior leaves the cursor exactly where it was
/// (position 2 is untouched) and swaps pane1<->pane2 instead.
#[test]
fn swap_pane_updown_with_target() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // H(1, V(2,3)): pane1 {0,0,40,24} (leaf 0), pane2 {41,0,39,12} (leaf 1),
    // pane3 {41,13,39,11} (leaf 2, focused after the second split).
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    c.send(&ClientMsg::Stdin(vec![0x02, b'"']));
    c.recv_output_until(&mut grid, |g| g.cursor().0 > 40 && g.cursor().1 >= 13);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["send-keys".into(), "-t".into(), "0".into(), "echo one111".into(), "Enter".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| marker_col(g, "one111").is_some());
    cli.send(&ClientMsg::Cli(vec!["send-keys".into(), "-t".into(), "1".into(), "echo two222".into(), "Enter".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| marker_col(g, "two222").is_some());
    cli.send(&ClientMsg::Cli(vec!["send-keys".into(), "-t".into(), "2".into(), "echo three333".into(), "Enter".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| marker_col(g, "three333").is_some());

    // Focus (pane3, position 2) must stay exactly where it is: the target
    // of this swap is position 0 (pane1), not the active pane.
    let cursor_before = grid.cursor();
    assert!(cursor_before.0 > 40 && cursor_before.1 >= 13, "sanity: cursor starts in pane3 (bottom-right)");

    cli.send(&ClientMsg::Cli(vec!["swap-pane".into(), "-D".into(), "-t".into(), "0".into()]));
    expect_cli_done(&cli, 0);

    // pane1 (position 0, left) now shows pane2's content ("two222");
    // pane2's old spot (position 1, top-right, rows 0..12) now shows pane1's
    // content ("one111") -- NOT pane3's spot (position 2, bottom-right, rows
    // 13..24), which is what the old (ignoring -t) behavior would have
    // produced. pane3's own content ("three333") and the cursor are both
    // untouched.
    c.recv_output_until(&mut grid, |g| marker_col(g, "two222").map(|c| c < 40).unwrap_or(false));
    c.recv_output_until(&mut grid, |g| marker_col_in_rows(g, "one111", 0, 12).is_some());
    assert!(marker_col_in_rows(&grid, "three333", 13, 24).is_some(), "pane3's content must stay in the bottom-right pane");
    assert!(marker_col_in_rows(&grid, "one111", 13, 24).is_none(), "pane1's content must NOT land in pane3's spot (that would mean -t was ignored)");
    assert_eq!(grid.cursor(), cursor_before, "focus/cursor must not move: -D targeted pane1, not the active pane");

    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

// ---- Task 7 (sub-project 4): window ops -- break-pane, move-window,
// find-window, `'` index prompt ------------------------------------------

/// `!` (break-pane): the focused (right) pane of a 2-pane window leaves and
/// becomes a new window, which becomes current; both windows are back down
/// to a single pane (no vertical border in either).
#[test]
fn break_pane_bang() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    c.send(&ClientMsg::Stdin(vec![0x02, b'!']));
    c.recv_output_until(&mut grid, |g| {
        screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")) && !has_vertical_border(g)
    });

    // Window 0 (the source) is also back to a single pane -- switch to it
    // and confirm no border survived there either.
    c.send(&ClientMsg::Stdin(vec![0x02, b'0']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));
    assert!(!has_vertical_border(&grid), "source window must be back to a single pane");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("1:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `!` refuses (`can't break with only one pane`, verbatim from the task
/// brief, itself quoting real tmux -- design spec's `## 6. Window ops`
/// section doesn't spell out a refusal string) whenever the SOURCE window
/// has only one pane -- both when it's the session's only window, AND when the
/// session has a second window (breaking a single-pane window is refused
/// regardless of how many other windows exist: a window can never be left
/// with zero panes).
#[test]
fn break_pane_last_pane_refused() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Case A: the only pane of the only window.
    c.send(&ClientMsg::Stdin(vec![0x02, b'!']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("can't break with only one pane")));
    assert!(!has_vertical_border(&grid));

    // Case B: a second window now exists, but the CURRENT window (1) still
    // has only its own single pane -- still refused.
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));
    c.send(&ClientMsg::Stdin(vec![0x02, b'!']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("can't break with only one pane")));
    // The transient message takes over the status line (message has
    // priority over the window list there); clear it with a keystroke and
    // confirm the window list is exactly as it was before the refused break.
    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));

    c.send(&ClientMsg::Stdin(b"\rexit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `.` opens the `(move-window) ` prompt; committing an index moves the
/// current window there.
#[test]
fn move_window_dot_prompt() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'.']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(move-window) ")));
    c.send(&ClientMsg::Stdin(b"5".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(move-window) 5")));
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 5:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `move-window -t <occupied index>` (without `-k`) errors `index in use:
/// <n>` verbatim and changes nothing.
#[test]
fn move_window_occupied_errors() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));

    // Switch back to window 0, then try to move it onto window 1's index.
    c.send(&ClientMsg::Stdin(vec![0x02, b'0']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'.']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(move-window) ")));
    c.send(&ClientMsg::Stdin(b"1\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("index in use: 1")));
    // The transient error message takes over the status line; clear it and
    // confirm the window list is exactly as it was before the refused move.
    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));

    c.send(&ClientMsg::Stdin(b"\rexit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("1:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `move-window -k -t <occupied index>` (only reachable via the explicit
/// CLI form -- the `.` prompt never supplies `-k`) kills the occupant and
/// takes its place.
#[test]
fn move_window_dash_k_kills() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Window 1 exists with a distinct marker, to prove it's really GONE
    // (not just renumbered) after the kill.
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));
    c.send(&ClientMsg::Stdin(b"echo w1mark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "w1mark").is_some());

    c.send(&ClientMsg::Stdin(vec![0x02, b'0']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));

    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"move-window -k -t 1\r".to_vec()));
    // Window 0 (current) now occupies index 1; the old window 1 (and its
    // "w1mark" pane) is gone -- only one window remains, at index 1.
    c.recv_output_until(&mut grid, |g| {
        let lines = screen_text(g);
        lines.iter().any(|l| l.contains("[0] 1:powershell*")) && !lines.iter().any(|l| l.contains("0:powershell"))
    });

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `swap-window -d -t -1` (SP6 Task 5, the user's real `bind -r "<"
/// swap-window -d -t -1` binding minus the key-binding indirection):
/// swaps the CURRENT window with the previous one by relative offset
/// (wrapping), and `-d` means focus FOLLOWS THE WINDOW OBJECT -- the client
/// keeps looking at its own pane content, which is now at the new (lower)
/// index. Proven two ways: (1) the status line's `*`/`-` flags land on the
/// swapped indexes, (2) the marked pane content stays visible (proof focus
/// followed the WINDOW, not the slot).
#[test]
fn swap_window_relative_target_moves_current_window() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // `c` creates window 1 (index 1, current); window 0 becomes "last".
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));
    c.send(&ClientMsg::Stdin(b"echo w1mark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "w1mark").is_some());

    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"swap-window -d -t -1\r".to_vec()));
    // Window 1 (marked, was current) swaps indexes with window 0: window 1
    // is now at index 0, window 0 is now at index 1. With -d, the client
    // stays on window 1 (its own window), which is now current at index 0.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));
    // Content proof: still looking at the SAME (marked) pane -- focus
    // followed the window object across the index change.
    assert!(marker_col(&grid, "w1mark").is_some());

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("1:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `swap-window -t -1` (no `-d`): the client's focus stays on the same
/// INDEX/slot, which now shows the OTHER window's content -- the opposite
/// of the `-d` case above. Proven by pane content: after the swap, window
/// 0's marker is visible again and window 1's marker is not, even though
/// no `select-window`-style command was ever issued.
#[test]
fn swap_window_without_d_keeps_focus_on_index() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Window 0: mark its pane before switching away from it.
    c.send(&ClientMsg::Stdin(b"echo w0mark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "w0mark").is_some());

    // `c` creates window 1 (index 1, current); window 0 becomes "last".
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));
    c.send(&ClientMsg::Stdin(b"echo w1mark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "w1mark").is_some());

    // Swap current (window 1) with the previous window (window 0), WITHOUT
    // -d.
    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"swap-window -t -1\r".to_vec()));
    // Focus stayed on the same slot (index 1) -- which now shows window 0's
    // content: "w0mark" is visible again, "w1mark" is not.
    c.recv_output_until(&mut grid, |g| marker_col(g, "w0mark").is_some() && marker_col(g, "w1mark").is_none());

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    // Window 0's shell (now at index 1, current) just exited, killing it;
    // fallback goes to `last` (window 1, still alive, now at index 0) --
    // the survivor renders as window 0.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")) && marker_col(g, "w1mark").is_some());
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `f` (find-window): matches by window NAME, matches by visible pane
/// CONTENT, and shows a transient `no windows matching: <p>` message (and
/// switches nothing) when neither matches.
#[test]
fn find_window_f_prompt() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Window 0: rename to "webby" (the NAME-match target).
    c.send(&ClientMsg::Stdin(vec![0x02, b',']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) powershell")));
    c.send(&ClientMsg::Stdin(vec![0x7f; 10]));
    c.send(&ClientMsg::Stdin(b"webby\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0:webby*")));

    // Window 1: the CONTENT-match target ("findme123" echoed into its pane).
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:webby- 1:powershell*")));
    c.send(&ClientMsg::Stdin(b"echo findme123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "findme123").is_some());

    // Currently on window 1: find-window by NAME jumps back to window 0.
    c.send(&ClientMsg::Stdin(vec![0x02, b'f']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(find-window) ")));
    c.send(&ClientMsg::Stdin(b"webby\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:webby* 1:powershell-")));

    // Now on window 0: find-window by CONTENT jumps to window 1.
    c.send(&ClientMsg::Stdin(vec![0x02, b'f']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(find-window) ")));
    c.send(&ClientMsg::Stdin(b"findme123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:webby- 1:powershell*")));

    // No match: transient message, nothing moves.
    c.send(&ClientMsg::Stdin(vec![0x02, b'f']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(find-window) ")));
    c.send(&ClientMsg::Stdin(b"zzznomatch\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("no windows matching: zzznomatch")));
    // The transient message takes over the status line; clear it and
    // confirm the no-match search didn't switch windows.
    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:webby- 1:powershell*")));

    c.send(&ClientMsg::Stdin(b"\rexit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0:webby*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `'` opens the `index` prompt (label verbatim per the design spec, no
/// parens/trailing space, unlike `.`/`f`'s "(name) " labels); committing an
/// existing index switches to it, and a nonexistent index errors `window
/// not found: <n>` (the same wording `resolve_window` already produces for
/// digit-key selection -- `'` reuses `select-window -t :<input>` verbatim).
#[test]
fn quote_prompt_selects_index() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'\'']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("index")));
    c.send(&ClientMsg::Stdin(b"0".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("index0")));
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));

    // Nonexistent index -> tmux-style error; selection unchanged.
    c.send(&ClientMsg::Stdin(vec![0x02, b'\'']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("index")));
    c.send(&ClientMsg::Stdin(b"9\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("window not found: 9")));
    // The transient error message takes over the status line; clear it and
    // confirm the selection didn't change.
    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));

    c.send(&ClientMsg::Stdin(b"\rexit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("1:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task-7 review, Important finding #1: the `'` index prompt must validate
/// the committed buffer is a well-formed window index (digits only) BEFORE
/// delegating to `select-window -t :<input>`. Before the fix, a non-numeric
/// buffer fell into `resolve_window_target`'s bare-token "try session name
/// first" branch: if the typed text happened to match an unrelated
/// session's name, the command silently resolved to THAT session's own
/// already-current window and did nothing visible on the acting client --
/// no error, no window change, no feedback at all. This test reproduces
/// exactly that scenario (a second session named `abc`, typed into the `'`
/// prompt on the main session) and asserts the fixed behavior: a `window
/// not found: abc` transient error (matching the wording the numeric-miss
/// case already produces) and the acting client's current window/session
/// unchanged.
#[test]
fn quote_prompt_rejects_non_numeric() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    // Second session, name chosen to be a plausible (but non-numeric)
    // window-index-prompt token -- this is what the bare-token session-name
    // fallback would otherwise silently match.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "abc".into()]));
    expect_cli_done(&cli, 0);

    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'\'']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("index")));
    c.send(&ClientMsg::Stdin(b"abc\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("window not found: abc")));
    // The transient error message takes over the status line; clear it and
    // confirm the acting client's own session/window is still unchanged --
    // NOT silently switched to (or left alone within) the unrelated `abc`
    // session.
    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    cli.send(&ClientMsg::Cli(vec!["kill-session".into(), "-t".into(), "abc".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after last session dies");
}

/// Final SP4 review, MUST-FIX #2: an all-ASCII-digit `'`-prompt input that
/// OVERFLOWS `u32` (e.g. 11 nines) must still get the numeric-index miss
/// wording (`window not found: <buf>`), not fall through to the bare-token
/// "try session name first" path and produce `can't find session: <buf>`.
/// Before the fix, `looks_like_index` used `parse::<u32>().is_ok()`, which
/// is `false` for an overflowing all-digit string, so
/// `resolve_window_target`'s bare-token fallback treated the digits as a
/// session name instead of a window index.
#[test]
fn quote_prompt_overflow_digits_window_not_found() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'\'']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("index")));
    c.send(&ClientMsg::Stdin(b"99999999999\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("window not found: 99999999999")));
    assert!(
        !screen_text(&grid).iter().any(|l| l.contains("can't find session")),
        "overflowing digits must not be treated as a session name"
    );

    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

// ---- overlays: choose-tree + display-panes (Task 8, sub-project 4) --------

/// `true` if any cell in `[x0,x1) x [y0,y1)` has background colour `bg` --
/// used to detect a display-panes digit block by its resolved colour
/// (`display-panes-colour`/`-active-colour`) rather than pinning exact
/// bitmap coordinates, which would over-couple the test to `render.rs`'s
/// private 5x5 font.
fn has_bg(g: &Grid, bg: Color, x0: u16, x1: u16, y0: u16, y1: u16) -> bool {
    (y0..y1.min(g.rows())).any(|y| (x0..x1.min(g.cols())).any(|x| g.cell(x, y).style.bg == bg))
}

#[test]
fn display_panes_q_shows_digits_and_selects() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Horizontal split: the RIGHT pane (digit 1) is focused immediately.
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);

    // Explicitly move focus to the LEFT pane (digit 0) first, so pressing
    // digit `1` later is an observable focus CHANGE rather than a no-op —
    // proven by a marker typed here landing left of the split.
    let mut left = vec![0x02];
    left.extend_from_slice(b"\x1b[D");
    c.send(&ClientMsg::Stdin(left));
    c.send(&ClientMsg::Stdin(b"leftmark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "leftmark").map(|col| col < 40).unwrap_or(false));

    // display-panes: a red (active, left pane) AND a blue (inactive, right
    // pane) digit block both appear.
    c.send(&ClientMsg::Stdin(vec![0x02, b'q']));
    c.recv_output_until(&mut grid, |g| has_bg(g, Color::Idx(1), 0, 80, 0, 23) && has_bg(g, Color::Idx(4), 0, 80, 0, 23));

    // Digit `1` selects the SECOND pane (the right one): focus moves there,
    // and the overlay closes on its own (no further key needed).
    c.send(&ClientMsg::Stdin(b"1".to_vec()));
    c.send(&ClientMsg::Stdin(b"rightmark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "rightmark").map(|col| col > 40).unwrap_or(false));

    // `-d 200`: a shorter-lived overlay auto-dismisses on its own (the 50ms
    // server tick, once its deadline has passed) with no keypress at all.
    c.send(&ClientMsg::Stdin(vec![0x02, b':']));
    c.send(&ClientMsg::Stdin(b"display-panes -d 200\r".to_vec()));
    c.recv_output_until(&mut grid, |g| has_bg(g, Color::Idx(1), 0, 80, 0, 23) || has_bg(g, Color::Idx(4), 0, 80, 0, 23));
    c.recv_output_until(&mut grid, |g| !has_bg(g, Color::Idx(1), 0, 80, 0, 23) && !has_bg(g, Color::Idx(4), 0, 80, 0, 23));

    // Clean up: exiting the (still-focused) right pane's shell autocloses
    // just that pane; the last remaining pane's exit destroys the session.
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task 10 (clock-mode, sub-project 6 wave 2): `prefix-t` opens the overlay
/// (big-digit blocks in `clock-mode-colour`, default blue `Color::Idx(4)`
/// -- an 80x23 pane comfortably clears the big-digit-mode threshold for the
/// default `24`-style 5-char `HH:MM` string, `w >= 6*5 == 30`, `h >= 6`);
/// per `docs/tmux-reference/status-line-and-messages.md` `## 6. Clock
/// mode`'s "any key exits" rule, a completed PREFIX SEQUENCE typed while
/// clock mode is open (`C-b c`, normally bound to `new-window`) both closes
/// the overlay AND must NOT run `new-window` underneath it -- the same
/// "other key dismisses, and is NOT reprocessed" interception display-panes
/// uses (`display_panes_prefix_sequence_dismisses_without_executing_bound_command`),
/// proving the exiting keystroke is swallowed by the overlay rather than
/// falling through to ordinary prefix-binding dispatch.
#[test]
fn clock_mode_opens_and_any_key_exits() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // prefix-t: the clock overlay opens, painting big-digit blocks in
    // clock-mode-colour (default blue).
    c.send(&ClientMsg::Stdin(vec![0x02, b't']));
    c.recv_output_until(&mut grid, |g| has_bg(g, Color::Idx(4), 0, 80, 0, 23));

    // A full prefix sequence typed WHILE clock mode is open (`C-b c`, bound
    // to new-window): the overlay must dismiss (blue blocks disappear,
    // pane content restored) AND `new-window` must NOT execute underneath
    // it.
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| !has_bg(g, Color::Idx(4), 0, 80, 0, 23));

    // Confirm from a fresh CLI connection (avoids racing this client's own
    // redraw): still exactly one window.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-windows".into(), "-t".into(), "0".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    assert_eq!(
        out.lines().filter(|l| !l.is_empty()).count(),
        1,
        "new-window bound under C-b c must NOT have executed while clock mode was open: {out:?}"
    );

    // Pane content is genuinely restored (not just "no more blue"): a
    // marker typed now round-trips through the shell normally.
    c.send(&ClientMsg::Stdin(b"restored\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "restored").is_some());

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task 10 fix round 1 (review Major): tmux's `window_clock_key`
/// (`window-clock.c:214-218`) calls `window_pane_reset_mode`
/// UNCONDITIONALLY -- its `key` and `mouse_event` parameters are both
/// `__unused` -- so ANY MOUSE EVENT exits clock mode too, exactly like any
/// key. And, same as the key path, the exiting event is CONSUMED by the
/// exit, not reprocessed: a click landing on a DIFFERENT (non-focused)
/// pane closes the overlay but must NOT focus that pane.
#[test]
fn clock_mode_exits_on_mouse() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    // Split; the new RIGHT pane is focused. Move focus to the LEFT pane so
    // the later click on the RIGHT pane would be an observable focus
    // change if it were (wrongly) reprocessed after the clock exit.
    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);
    let mut left = vec![0x02];
    left.extend_from_slice(b"\x1b[D");
    c.send(&ClientMsg::Stdin(left));
    c.recv_output_until(&mut grid, |g| g.cursor().0 < border_x);

    // prefix-t: clock mode opens on the focused LEFT pane -- blue big-digit
    // blocks appear left of the border.
    c.send(&ClientMsg::Stdin(vec![0x02, b't']));
    c.recv_output_until(&mut grid, |g| has_bg(g, Color::Idx(4), 0, border_x, 0, 23));

    // Click (press + release) well inside the RIGHT pane: the overlay must
    // close (any mouse event exits, `window-clock.c:214-218`)...
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, border_x + 5, 10, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, border_x + 5, 10, true)));
    c.recv_output_until(&mut grid, |g| !has_bg(g, Color::Idx(4), 0, 80, 0, 23));

    // ...and the click must have been CONSUMED by the exit, not reprocessed
    // as a click-to-focus: a marker typed now still lands in the LEFT pane.
    c.send(&ClientMsg::Stdin(b"staymark\r".to_vec()));
    c.recv_output_until(&mut grid, |g| marker_col(g, "staymark").map(|col| col < border_x).unwrap_or(false));

    // Cleanup: kill-server (two live panes).
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

#[test]
fn choose_tree_w_lists_and_switches() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| {
        let lines = screen_text(g);
        lines.iter().any(|l| l.contains("0: 2 windows (attached)"))
            && lines.iter().any(|l| l.contains("  0: powershell-"))
            && lines.iter().any(|l| l.contains("  1: powershell*"))
    });

    // SP6 wave 2, Task 8, `(b)`: the default selection is now the CURRENT
    // item, i.e. window 1's row (the just-created, now-current window) --
    // the last row, not the header. Up (window 1's row -> window 0's row) +
    // Enter switches to window 0.
    c.send(&ClientMsg::Stdin(b"\x1b[A".to_vec()));
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell* 1:powershell-")));

    // Clean up: kill the now-current window 0, falls back to window 1.
    c.send(&ClientMsg::Stdin(vec![0x02, b'&']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 1:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn choose_tree_s_sessions() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut a = Client::connect(&name);
    attach(&mut a, AttachMode::NewNamed, "sA", 80, 24);
    let mut grid_a = Grid::new(80, 24, 0);
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut b = Client::connect(&name);
    attach(&mut b, AttachMode::NewNamed, "sB", 80, 24);
    let mut grid_b = Grid::new(80, 24, 0);
    b.recv_output_until(&mut grid_b, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // `s` from sA: a collapsed row per session, both marked attached.
    a.send(&ClientMsg::Stdin(vec![0x02, b's']));
    a.recv_output_until(&mut grid_a, |g| {
        let lines = screen_text(g);
        lines.iter().any(|l| l.contains("sA: 1 windows (attached)")) && lines.iter().any(|l| l.contains("sB: 1 windows (attached)"))
    });

    // Down (sA -> sB) + Enter switches THIS client to sB.
    a.send(&ClientMsg::Stdin(b"\x1b[B".to_vec()));
    a.send(&ClientMsg::Stdin(b"\r".to_vec()));
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("[sB]")));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

/// Named `..._escape_cancels` per the task brief, but the concrete keypress
/// exercised is `q` (also bound to `ChooseTreeAction::Cancel`, alongside
/// `Escape`/`C-c`), not a literal bare ESC byte -- a lone `0x1b` with
/// nothing after it can NEVER decode through `KeyDecoder`'s normal (non-
/// capture) path within a single `feed()` call: `classify_escape` returns
/// `None` ("wait for more bytes, could be the start of a CSI/SS3/meta
/// sequence") and (at the time this test was written) `KeyMachine::feed`
/// never called `decoder.flush()` itself, so the byte sat in the decoder's
/// pending buffer forever absent an `escape-time` flush timer. That timer is
/// now implemented (sub-project 4, Task 9 -- see
/// `escape_key_reaches_pane_via_escape_time_flush` below, which exercises
/// the literal bare-ESC byte this comment used to say was impossible). This
/// test is kept as-is (using `q`) since it's still a valid, independent
/// regression check of the SAME `ChooseTreeAction::Cancel` dispatch path,
/// matching how the project's copy-mode test suite also keeps its `q`-based
/// `copy_mode_q_exits` alongside `Escape`-specific coverage elsewhere.
#[test]
fn choose_tree_escape_cancels() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: 1 windows (attached)")));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Final SP4 review, MUST-FIX NEW-1: mouse events must be swallowed while
/// an overlay (choose-tree or display-panes) is open, exactly like the
/// pre-existing `ConfirmCmd`/`Prompt` guard -- a click landing on a HIDDEN
/// pane underneath the overlay must never focus it. Splits the window so
/// the right pane is focused and a left pane exists to be mis-focused by a
/// leaking click; opens choose-tree (which draws full-screen, hiding the
/// split); clicks where the left pane would be; then dismisses the overlay
/// with `q` and asserts focus is STILL the right pane.
#[test]
fn mouse_ignored_under_choose_tree_overlay() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);
    assert!(grid.cursor().0 > border_x, "expected focus on the right pane right after split");

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: 1 windows (attached)")));

    // Click where the LEFT (unfocused, now-hidden) pane would be.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, true)));

    // Dismiss the overlay via the keyboard and confirm focus is unchanged --
    // the click must not have silently refocused the hidden left pane.
    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));
    assert!(grid.cursor().0 > border_x, "mouse click under choose-tree overlay must not move focus");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Display-panes variant of `mouse_ignored_under_choose_tree_overlay`
/// (cheap given the existing `has_bg` helper): a click under the
/// full-screen digit overlay must not focus the hidden left pane either.
#[test]
fn mouse_ignored_under_display_panes_overlay() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);
    enable_mouse(&name, &mut c);

    c.send(&ClientMsg::Stdin(vec![0x02, b'%']));
    c.recv_output_until(&mut grid, has_vertical_border);
    let border_x = find_vertical_border(&grid);
    assert!(grid.cursor().0 > border_x, "expected focus on the right pane right after split");

    c.send(&ClientMsg::Stdin(vec![0x02, b'q']));
    c.recv_output_until(&mut grid, |g| has_bg(g, Color::Idx(1), 0, 80, 0, 23) && has_bg(g, Color::Idx(4), 0, 80, 0, 23));

    // Click where the LEFT (unfocused, now-hidden) pane would be.
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, false)));
    c.send(&ClientMsg::Stdin(sgr_mouse(CB_LEFT, 5, 10, true)));

    // Dismiss with a non-digit key (space) and confirm focus is unchanged.
    c.send(&ClientMsg::Stdin(b" ".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_bg(g, Color::Idx(1), 0, 80, 0, 23) && !has_bg(g, Color::Idx(4), 0, 80, 0, 23));
    assert!(grid.cursor().0 > border_x, "mouse click under display-panes overlay must not move focus");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.recv_output_until(&mut grid, |g| !has_vertical_border(g));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task 9 (sub-project 4): escape-time disambiguation. A lone ESC byte with
/// nothing following it can never resolve through `KeyDecoder`'s normal
/// path within one `feed()` call -- only the `Tick`-driven escape-time
/// flush (design spec `## 8. escape-time`; `input::KeyMachine::flush_now`)
/// delivers it, once it has aged past the (here shrunk-for-the-test)
/// `escape-time` option. This test proves TWO things end to end, per the
/// task brief: (1) the flushed ESC actually reaches dispatch and has its
/// real effect (canceling the still-open choose-tree overlay -- a clean,
/// observable "Escape did something" signal); (2) the flush doesn't stall
/// or corrupt decoder state afterward -- the very next bytes (`[A`, sent
/// once the cancel has already been observed) decode as two ordinary
/// literal characters, not merged with the already-consumed ESC into some
/// leftover Meta-prefixed sequence (which would never show up as literal
/// text on the pane's line).
#[test]
fn escape_key_reaches_pane_via_escape_time_flush() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    // Shrink well below tmux's 500ms default (still well above the 50ms
    // Tick granularity) so the test doesn't need to block for the default.
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "escape-time".into(), "150".into()]));
    expect_cli_done(&cli, 0);

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: 1 windows (attached)")));

    // A lone ESC, nothing after it: buffered until the escape-time flush
    // resolves it on a later Tick.
    c.send(&ClientMsg::Stdin(vec![0x1b]));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));

    // The flush already ran (proven by the overlay closing above) and
    // cleared its own pending-escape state -- these bytes must decode
    // completely fresh.
    c.send(&ClientMsg::Stdin(b"[A".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[A")));

    // Submit (and discard) that literal "[A" as its own (invalid, harmless)
    // command line before cleanly exiting the shell.
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

#[test]
fn choose_tree_x_kills_with_confirm() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell- 1:powershell*")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("  0: powershell-")));

    // SP6 wave 2, Task 8, `(b)`: default selection is window 1's row (the
    // just-created, now-current window) -- Up selects window 0's row; `x`
    // arms the confirm, `y` commits it.
    c.send(&ClientMsg::Stdin(b"\x1b[A".to_vec()));
    c.send(&ClientMsg::Stdin(b"x".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("kill-window powershell? (y/n)")));

    c.send(&ClientMsg::Stdin(b"y".to_vec()));
    // Window 0 is gone; the overlay stays OPEN (tmux keeps choose-tree up)
    // with an updated row list -- only window 1's row remains.
    c.recv_output_until(&mut grid, |g| {
        let lines = screen_text(g);
        lines.iter().any(|l| l.contains("  1: powershell*")) && !lines.iter().any(|l| l.contains("  0: powershell"))
    });

    // `q` closes the still-open overlay (see `choose_tree_escape_cancels`'s
    // doc comment for why a bare Escape byte isn't used here); window 1 is
    // current.
    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 1:powershell*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task 8 review fix, Important #2: `KeyMachine` swallows the bare prefix
/// keypress itself with no event at all -- only the key that COMPLETES a
/// prefix sequence surfaces, tagged `WhichTable::Prefix`. Before this fix,
/// `handle_stdin`'s choose-tree/display-panes interception was gated on
/// `table == WhichTable::Root` only, so a completed prefix sequence typed
/// while the overlay was open fell through to ordinary prefix-binding
/// dispatch and ran the bound command (here, `C-b c` -> `new-window`) UNDER
/// the still-open overlay. Per the design spec's display-panes rule ("other
/// key dismisses ... and is NOT reprocessed"), the prefix keystroke counts
/// as "other key": the overlay must close AND `new-window` must NOT run.
#[test]
fn display_panes_prefix_sequence_dismisses_without_executing_bound_command() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    // Only one pane exists (no split), so its digit block is the ACTIVE
    // colour (red, `Color::Idx(1)`).
    c.send(&ClientMsg::Stdin(vec![0x02, b'q']));
    c.recv_output_until(&mut grid, |g| has_bg(g, Color::Idx(1), 0, 80, 0, 23));

    // A full prefix sequence typed WHILE display-panes is open (`C-b c`,
    // bound to new-window): the overlay must dismiss and `new-window` must
    // NOT execute underneath it.
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.recv_output_until(&mut grid, |g| !has_bg(g, Color::Idx(1), 0, 80, 0, 23) && !has_bg(g, Color::Idx(4), 0, 80, 0, 23));

    // Confirm from a fresh CLI connection (avoids racing this client's own
    // redraw): still exactly one window.
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["list-windows".into(), "-t".into(), "0".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    assert_eq!(
        out.lines().filter(|l| !l.is_empty()).count(),
        1,
        "new-window bound under C-b c must NOT have executed while display-panes was open: {out:?}"
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Same root cause as the display-panes test above, choose-tree side.
/// choose-tree's hardcoded key table (`resolve_choose_tree_key`) has no
/// entry for `c`, so per tmux's own modal choose-tree semantics (the
/// review's adjudicated reading of the design spec, which only documents
/// the "other key dismisses" rule for display-panes explicitly) the
/// completed `C-b c` sequence is silently IGNORED: the overlay stays open
/// and `new-window` does not run.
#[test]
fn choose_tree_prefix_sequence_ignored_overlay_stays_open() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: 1 windows (attached)")));

    // A full prefix sequence typed WHILE choose-tree is open (`C-b c`,
    // bound to new-window) must be swallowed by the overlay, not
    // dispatched as a prefix command underneath it. Immediately follow
    // with `q` (a REAL choose-tree action, Cancel) for a deterministic,
    // non-racy signal: with the bug, `new-window` would already have run
    // and switched focus to a brand-new window 1 before `q` cancels, so
    // the resulting status line reads "[0] 0:powershell- 1:powershell*"
    // (window 0 no longer current); with the fix, exactly one window
    // still exists and cancelling lands right back on it.
    c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));
    assert!(
        !screen_text(&grid).iter().any(|l| l.contains("1:powershell")),
        "new-window bound under C-b c must NOT have executed while choose-tree was open"
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// Task 8 review fix, Important #3: `render_one`'s choose-tree scroll `top`
/// used to be computed from the FULL client height, but `compose_back`'s
/// actual paint pass reserves one row for `scene.message` (choose-tree's
/// `x` kill-confirm prompt) whenever it's shown -- one fewer row than
/// `top`'s old math assumed. With a long, scrolled list and the selection
/// at the very bottom, arming the kill-confirm (`x`) could push the
/// selected/prompted row off the actually-painted area. 9 windows (10
/// total with the session's original one) overflow a 10-row terminal.
///
/// SP6 wave 2, Task 8, `(b)` update: the default selection is now the
/// CURRENT item -- window 9, the just-created, now-current window, is
/// already the LAST row -- so it (and the scroll needed to show it) is
/// immediate on opening, no `Down` presses required to reach it (the old
/// version of this test drove 20 `Down`s from the OLD row-0 default to get
/// there; that navigation is kept below, now purely as a defensive proof
/// that clamping still holds at the end of the list, not as how the
/// selection gets there). This 10-row terminal is also small enough that
/// `choose_tree_list_height`'s NORMAL-mode `h < 10` rule drops the preview
/// entirely (`sy=10`), so this test's msg_reserved/scrolling math is
/// exercised exactly as before this task's preview work.
#[test]
fn choose_tree_scrolls_long_list_with_confirm_message_shown() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 10);

    for _ in 0..9 {
        c.send(&ClientMsg::Stdin(vec![0x02, b'c']));
        c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));
    }

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("9: powershell*")));
    // The header (and windows 0-8's rows) are scrolled OFF the top of the
    // 10-row terminal instead, since window 9's row is the default
    // selection, not the header.
    assert!(!screen_text(&grid).iter().any(|l| l.contains("10 windows")), "test setup: header must be scrolled OFF (window 9 is the default selection)");

    // Downs past the last row are still safely clamped there (defensive --
    // the selection was already on window 9's row before this loop).
    for _ in 0..20 {
        c.send(&ClientMsg::Stdin(b"\x1b[B".to_vec()));
    }
    c.send(&ClientMsg::Stdin(b"x".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(y/n)")));

    let lines = screen_text(&grid);
    assert!(lines.iter().any(|l| l.contains("(y/n)")), "kill-confirm message must be showing");
    assert!(
        lines.iter().any(|l| l.contains("9: powershell*")),
        "the selected row (window 9, the last one) must still be visible alongside the kill-confirm message, not scrolled off by one row: {lines:?}"
    );

    // Clean up: cancel the confirm (anything but y/Y/Enter), close the
    // overlay, exit. (Not asserting on the post-`q` status line text here:
    // with 10 windows its "N:powershell<flag> " entries overflow an 80-col
    // status line long before reaching window 9's, so waiting for it to
    // appear there would hang regardless of this fix -- the pane prompt
    // reappearing is enough to prove the overlay actually closed.)
    c.send(&ClientMsg::Stdin(b"n".to_vec()));
    c.recv_output_until(&mut grid, |g| !screen_text(g).iter().any(|l| l.contains("(y/n)")));
    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // 10 windows survive -- `exit\r` on just the current one wouldn't end
    // the session, so clean up via `kill-server` from a fresh CLI
    // connection instead (same pattern `choose_tree_s_sessions` uses).
    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli, 0);
    server.join().expect("server exits after kill-server");
}

// ---- choose-tree: real tree view, default selection, preview (SP6 wave 2,
// Task 8) --------------------------------------------------------------

/// `(a)` tree structure: sessions are real tree PARENT rows with their
/// windows as indented CHILD rows -- collapsed by default (`docs/tmux-
/// reference/choose-tree.md` `## 1.1`: "sessions start collapsed"); `Right`
/// reveals the (default-selected, current) session's window(s) as child
/// rows underneath it.
#[test]
fn choose_tree_sessions_show_window_children() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b's']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: 1 windows (attached)")));
    // Collapsed by default: no window child row yet.
    assert!(!screen_text(&grid).iter().any(|l| l.contains("0: powershell*")), "test setup: session must start collapsed");

    // Right expands the (default-)selected session, revealing its window(s)
    // as indented child rows.
    c.send(&ClientMsg::Stdin(b"\x1b[C".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: powershell*")));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `(a)`: expand/collapse state survives a round trip -- `Right` reveals the
/// session's window child row, `Left` hides it again (jumps back up to the
/// parent per the doc's "flat, move to parent" rule would only apply to a
/// LEAF row; here the session row itself just collapses), and `Right` a
/// second time restores it -- the `expanded` set is mutated in place, not
/// reset on every rebuild.
#[test]
fn choose_tree_collapse_hides_children_expand_restores() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b's']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: 1 windows (attached)")));

    c.send(&ClientMsg::Stdin(b"\x1b[C".to_vec())); // Right: expand
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: powershell*")));

    c.send(&ClientMsg::Stdin(b"\x1b[D".to_vec())); // Left: collapse
    c.recv_output_until(&mut grid, |g| !screen_text(g).iter().any(|l| l.contains("0: powershell*")));

    c.send(&ClientMsg::Stdin(b"\x1b[C".to_vec())); // Right again: restored
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: powershell*")));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `(b)` default selection = current item: three sessions in creation order
/// (sA, sB, sC); the acting client is attached to the MIDDLE one (sB).
/// Opening `s` then immediately pressing Enter must be a no-op (committing
/// to the ALREADY-current session) -- if the default selection had instead
/// landed on row 0 (sA, the row-0-always behavior this task replaces),
/// Enter would actually SWITCH this client to sA.
#[test]
fn choose_tree_default_selects_current_session() {
    let name = unique_pipe_name();
    let server = start_server(&name);

    let mut cli_a = cli_client(&name);
    cli_a.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "sA".into()]));
    expect_cli_done(&cli_a, 0);

    let mut c = Client::connect(&name);
    attach(&mut c, AttachMode::NewNamed, "sB", 80, 24);
    let mut grid = Grid::new(80, 24, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[sB]")));

    let mut cli_c = cli_client(&name);
    cli_c.send(&ClientMsg::Cli(vec!["new-session".into(), "-d".into(), "-s".into(), "sC".into()]));
    expect_cli_done(&cli_c, 0);

    c.send(&ClientMsg::Stdin(vec![0x02, b's']));
    c.recv_output_until(&mut grid, |g| {
        let lines = screen_text(g);
        lines.iter().any(|l| l.contains("sA:")) && lines.iter().any(|l| l.contains("sB:")) && lines.iter().any(|l| l.contains("sC:"))
    });

    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    // Still on sB -- Enter on the default (current-session) selection was a
    // no-op, not a switch to row 0 (sA).
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[sB]")));

    let mut cli_kill = cli_client(&name);
    cli_kill.send(&ClientMsg::Cli(vec!["kill-server".into()]));
    expect_cli_done(&cli_kill, 0);
    server.join().expect("server exits after kill-server");
}

/// `(c)` preview box: the preview shows the SELECTED row's live pane
/// content -- a marker string printed in the current window's pane appears
/// inside the rendered preview region below the list. `Windows` view's
/// default selection is the current window (see `choose_tree_default_
/// selects_current_session`'s sibling test for the session-view half of
/// `(b)`), so the preview is already showing it with no navigation needed.
#[test]
fn choose_tree_preview_shows_selected_windows_content() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(b"echo PREVIEWMARK123\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PREVIEWMARK123")));

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    // The overlay clears the whole client area (the marker's OLD on-screen
    // position is wiped); it must reappear -- now inside the preview box --
    // for this to pass.
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("PREVIEWMARK123")));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `(c)`: `v` cycles the preview mode OFF -> BIG -> NORMAL -> OFF
/// (`docs/tmux-reference/choose-tree.md` `## 3.1`/`## 7.1`), observable as
/// the 0-based row the preview's top border line paints on. Starting state
/// is NORMAL (the tmux/winmux default), so the FIRST `v` press advances to
/// the state that follows NORMAL in that cycle, OFF -- not BIG (BIG comes
/// right before NORMAL in the stated sequence, so it's only reached on the
/// SECOND press). Worked out here for an 80x24 pane with a 2-row list
/// (session header + 1 window), `choose_tree_list_height`'s formula: NORMAL
/// -> `h = (24/3)*2 = 16`, `16 > line_size(2)` so `h = 24/2 = 12` (the
/// "short list" branch) -> border at row 12. OFF -> `h = sy = 24` -> no
/// preview at all (list fills the whole panel), so NO row contains a border
/// character anywhere. BIG -> `h = 24/4 = 6`, `6 > line_size(2)` so `h =
/// line_size = 2` -> border at row 2.
#[test]
fn choose_tree_v_toggles_preview() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b'w']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("0: 1 windows (attached)")));
    // Default preview mode is NORMAL: border row at 12.
    c.recv_output_until(&mut grid, |g| screen_text(g)[12].contains('─'));

    // v -> OFF: no border anywhere (the list now spans the whole panel).
    c.send(&ClientMsg::Stdin(b"v".to_vec()));
    c.recv_output_until(&mut grid, |g| !screen_text(g).iter().any(|l| l.contains('─')));

    // v -> BIG: border row at 2.
    c.send(&ClientMsg::Stdin(b"v".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g)[2].contains('─'));

    // v -> NORMAL again: border row at 12, same as the default.
    c.send(&ClientMsg::Stdin(b"v".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g)[12].contains('─'));

    c.send(&ClientMsg::Stdin(b"q".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:powershell*")));
    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// automatic-rename (Task 9, sub-project 4): PowerShell's
/// `$Host.UI.RawUI.WindowTitle` assignment round-trips through ConPTY as an
/// OSC 0/2 title, which `Grid` captures (`grid-v2`, Task 1) and the server
/// polls (`take_title_changed`) after every pane `Output` feed. The window
/// (auto-named "0" for its session, name "powershell" at creation, both
/// tmux/winmux defaults) tracks the title within the 500ms throttle window.
#[test]
fn pane_title_updates_window_name() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(b"$Host.UI.RawUI.WindowTitle='mytool'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:mytool*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// automatic-rename precedence (Task 9): a MANUAL `rename-window` (CLI here;
/// the `,` prompt commit funnels through the exact same
/// `exec_rename_window`, so this covers both call sites) permanently clears
/// the window's `auto_rename` flag -- a later OSC title change must NOT
/// override the manual name. The title-setting command and the
/// `done-manual-check` marker run as ONE PowerShell statement list so the
/// marker appearing on screen proves the OSC has already had its chance to
/// reach and be processed by the server before the assertion below runs.
#[test]
fn manual_rename_disables_auto() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["rename-window".into(), "manual".into()]));
    expect_cli_done(&cli, 0);
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:manual*")));

    c.send(&ClientMsg::Stdin(b"$Host.UI.RawUI.WindowTitle='mytool'; echo done-manual-check\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("done-manual-check")));
    let lines = screen_text(&grid);
    assert!(
        lines.iter().any(|l| l.contains("[0] 0:manual*")),
        "manual rename must survive a later title change; screen:\n{}",
        lines.join("\n")
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// automatic-rename precedence via the `,` PROMPT path (Task 9 fix round):
/// mirrors `manual_rename_disables_auto` exactly, except the rename is
/// committed via the interactive `,` status-line prompt (keystrokes lifted
/// from `rename_window_prompt_flow`) instead of the CLI. Regression test for
/// the review finding that `feed_prompt_byte`'s `PromptKind::RenameWindow`
/// arm was a separate, un-synced inline implementation that never cleared
/// `Window::auto_rename` -- a `,`-renamed window would silently revert to
/// the title-derived name on the pane's next OSC title change.
#[test]
fn comma_prompt_rename_disables_auto() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(vec![0x02, b',']));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) powershell")));
    // "powershell" is 10 chars; wipe it, then type the new name.
    c.send(&ClientMsg::Stdin(vec![0x7f; 10]));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) ") && !l.contains("powershell")));
    c.send(&ClientMsg::Stdin(b"manual".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("(rename-window) manual")));
    c.send(&ClientMsg::Stdin(b"\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:manual*")));

    c.send(&ClientMsg::Stdin(b"$Host.UI.RawUI.WindowTitle='mytool'; echo done-manual-check\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("done-manual-check")));
    let lines = screen_text(&grid);
    assert!(
        lines.iter().any(|l| l.contains("[0] 0:manual*")),
        "comma-prompt rename must survive a later title change; screen:\n{}",
        lines.join("\n")
    );

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// automatic-rename must produce a USEFUL name for the extremely common
/// Windows default console title shape -- a bare exe path, dot and all
/// (Task 9 fix round finding 2). Before the fix, `derive_auto_name` routed
/// the candidate through `model::validate_name`, which rejects any name
/// containing `:`/`.` (reserved for `session:window.pane` target syntax) --
/// so a title like `C:\Windows\system32\cmd.exe` silently no-op'd the
/// rename entirely, leaving the window stuck at its old name. The shipped
/// fix strips a recognized trailing extension (`.exe` here) instead of
/// rejecting: basename `cmd.exe` -> `cmd` (see `derive_auto_name`'s doc
/// comment for the full mapping, and why this was chosen over sanitizing
/// `:`/`.` to `-`, which would have produced `cmd-exe` but regressed the
/// pane-startup-title case in several other Task 9 tests).
#[test]
fn auto_rename_handles_exe_titles() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(b"$Host.UI.RawUI.WindowTitle='C:\\Windows\\system32\\cmd.exe'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:cmd*")));

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// `#T`/`display-message` (Task 9): the pane's FULL OSC title (not the
/// derived-and-truncated window name automatic-rename applies) is available
/// via the new `#T` format code. Using a multi-word title distinguishes the
/// two: the window name derives to just its first token ("mytool"), while
/// `#T` expands to the whole thing.
#[test]
fn pane_title_format_expands() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    c.send(&ClientMsg::Stdin(b"$Host.UI.RawUI.WindowTitle='mytool title here'\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("[0] 0:mytool*")));

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["display-message".into(), "#T".into()]));
    let (out, err) = expect_cli_done(&cli, 0);
    assert_eq!(err, "");
    assert_eq!(out, "mytool title here");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}

/// SP7 review fix: a width-changing resize while a copy-mode selection is
/// active must CLEAR the selection, not silently keep it pointing at
/// whatever content its (now-stale) anchor resolves to. `Grid`'s
/// `history_total()` shift invariant (used by `anchor_key_now` to keep a
/// selection anchor pinned to content) only holds across mutations that
/// don't change the grid's width -- `reflow_to_width` restructures rows
/// non-uniformly and never bumps `history_total` to match, so there is no
/// corrected shift count that could repair a stored anchor after such a
/// resize.
///
/// Verified against real tmux (`window-copy.c`): `window_pane_resize`
/// (`window.c:1362-1388`) calls the active mode's `resize` callback whenever
/// a pane's width OR height actually changes; for copy mode that's
/// `window_copy_resize` (`window-copy.c:1196-1227`), which unconditionally
/// ends by calling `window_copy_size_changed` (`window-copy.c:1174-1193`) --
/// and THAT unconditionally clears the selection
/// (`window_copy_clear_selection`, `window-copy.c:5914-5929`) regardless of
/// whether the resize actually reflowed anything (the cursor is separately
/// remapped/preserved via `grid_wrap_position`/`grid_unwrap_position`, only
/// when width changed, but the clear itself is unconditional). winmux's
/// `Server::apply_layout_for_session` mirrors this: it clears `cs.sel`
/// whenever a pane bound to an active copy-mode selection is actually
/// resized, but leaves copy mode itself active and the copy cursor
/// (`scroll`/`cx`/`cy`) untouched.
///
/// This test builds a selection over known non-blank text ("selmark4"),
/// resizes the client's terminal width (80 -> 60, rows unchanged -- a pure
/// width-changing resize, the exact case that breaks the anchor), then
/// tries to copy the selection (`Enter`, vi table's
/// `copy-selection-and-cancel`). If the selection had survived the resize
/// (the pre-fix bug), this would extract SOME text (right or wrong -- the
/// point of the bug is the anchor can't be trusted either way) into a new
/// automatic paste buffer; `list-buffers` reporting "no buffers" instead
/// proves the selection was cleared rather than silently carried across the
/// reflow.
#[test]
fn copy_mode_selection_clears_on_width_resize() {
    let name = unique_pipe_name();
    let server = start_server(&name);
    let mut c = Client::connect(&name);
    let mut grid = attach_auto_and_wait_prompt(&mut c, 80, 24);

    let mut cli = cli_client(&name);
    cli.send(&ClientMsg::Cli(vec!["set".into(), "-g".into(), "mode-keys".into(), "vi".into()]));
    expect_cli_done(&cli, 0);

    // Known non-blank marker lines to select against.
    c.send(&ClientMsg::Stdin(b"1..5 | ForEach-Object { \"selmark$_\" }\r".to_vec()));
    c.recv_output_until(&mut grid, |g| screen_text(g).iter().any(|l| l.contains("selmark5")));

    // Enter copy mode (cursor seeds from the live cursor -- the fresh
    // prompt row just below the marker lines).
    c.send(&ClientMsg::Stdin(vec![0x02, b'[']));
    c.recv_output_until(&mut grid, |g| has_indicator(g, "[0/"));

    // vi table: move up 2 rows onto a marker line ("selmark4"), jump to
    // start-of-line (`0`), begin a selection (`Space`), then extend right
    // 5 columns across the marker text (`l` x5) -- all in one Stdin frame,
    // same coalesced-Forward-blob pattern as the existing `KKK`/vi tests.
    c.send(&ClientMsg::Stdin(b"kk0 lllll".to_vec()));

    // Pure WIDTH-changing resize (rows unchanged) -- the exact case that
    // breaks the selection anchor. The local mirror grid is resized
    // in lockstep (a real terminal resizes itself instantly, independent of
    // the server's re-render), and frame ordering on this single connection
    // guarantees the server has fully processed the selection keys above
    // before it sees this Resize frame.
    c.send(&ClientMsg::Resize { cols: 60, rows: 24 });
    grid.resize(60, 24);

    // Try to copy the (should now be cleared) selection and exit copy mode.
    c.send(&ClientMsg::Stdin(vec![b'\r']));
    c.recv_output_until(&mut grid, |g| !has_indicator(g, ""));

    // No buffer should have been created: query via a separate headless CLI
    // connection (`list-buffers`'s headless path returns an EMPTY string
    // when there are no buffers -- see `exec_list_buffers_headless` -- a
    // deterministic assertion, unlike waiting on the attached client's
    // transient status-line message).
    let mut cli2 = cli_client(&name);
    cli2.send(&ClientMsg::Cli(vec!["list-buffers".into()]));
    let (out, err) = expect_cli_done(&cli2, 0);
    assert_eq!(err, "");
    assert_eq!(out, "", "a buffer was created from a selection that should have been cleared by the width resize");

    c.send(&ClientMsg::Stdin(b"exit\r".to_vec()));
    c.expect_exit(0, "[exited]");
    server.join().expect("server exits after last session dies");
}
