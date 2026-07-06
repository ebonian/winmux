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
