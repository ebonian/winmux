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
    let name = name.to_string();
    thread::spawn(move || {
        winmux::server::run(&name).expect("server run");
    })
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
    let mut grid = Grid::new(cols, rows);
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
    let mut g1 = Grid::new(80, 24);
    c1.recv_output_until(&mut g1, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut c2 = Client::connect(&name);
    attach(&mut c2, AttachMode::NewNamed, "e2", 80, 24);
    let mut g2 = Grid::new(80, 24);
    c2.recv_output_until(&mut g2, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    // Empty target attaches to "e2" (most recently created), not "e1".
    let mut c3 = Client::connect(&name);
    attach(&mut c3, AttachMode::Existing, "", 80, 24);
    let mut g3 = Grid::new(80, 24);
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
    let mut grid = Grid::new(80, 24);
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
    let mut grid1 = Grid::new(80, 24);
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
    let mut grid2 = Grid::new(80, 24);
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
    grid = Grid::new(80, 24);
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
    let mut grid_a = Grid::new(100, 40);
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut b = Client::connect(&name);
    attach(&mut b, AttachMode::Existing, "shared", 80, 24);
    let mut grid_b = Grid::new(80, 24);
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
    let mut grid = Grid::new(80, 24);
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
    let mut grid_a = Grid::new(80, 24);
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut b = Client::connect(&name);
    attach(&mut b, AttachMode::NewNamed, "sB", 80, 24);
    let mut grid_b = Grid::new(80, 24);
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
    let mut grid = Grid::new(80, 24);
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
    let mut grid = Grid::new(80, 24);
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
    let mut grid_a = Grid::new(80, 24);
    a.recv_output_until(&mut grid_a, |g| screen_text(g).iter().any(|l| l.contains("PS ")));

    let mut b = Client::connect(&name);
    attach(&mut b, AttachMode::Existing, "stale", 80, 24);
    let mut grid_b = Grid::new(80, 24);
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
    let mut grid = Grid::new(80, 24);
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
    let mut grid = Grid::new(80, 24);
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
