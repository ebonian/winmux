//! Thin attach client: connects to the server pipe, hands the terminal over
//! to `Host`, forwards stdin as `Stdin` frames, polls for terminal resizes,
//! and renders `Output` frames until the server sends a terminal `Exit`.
//!
//! Also owns `autostart_server`, which spawns the server role of this same
//! binary detached (no console, survives the client's console closing) when
//! `main.rs` finds no server bound to the target pipe.
//!
//! No unit tests: this module is pure I/O glue (threads, a live named pipe,
//! a live console) — its coverage is `tests/e2e.rs` (Task 9) plus manual
//! runs, per the task brief.

use std::error::Error;
use std::io;
use std::os::windows::process::CommandExt;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use crate::host::{self, Host};
use crate::pipe::{self, PipeConn};
use crate::protocol::{self, ClientMsg, ServerMsg};

/// `CreateProcess` creation flags: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
/// (0x00000008 | 0x00000200) — no console of its own, and not part of this
/// console's process group, so the server outlives the client's console.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// How long to poll for the freshly-spawned server to accept connections
/// before giving up.
const AUTOSTART_TIMEOUT: Duration = Duration::from_secs(5);
const AUTOSTART_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How often the main loop wakes up (to poll `host.size()` for a resize)
/// while otherwise waiting on server messages.
const TICK: Duration = Duration::from_millis(50);

/// Spawn `current_exe() __server --pipe <full-name> [--config <file>]`
/// detached, then poll `PipeConn::connect` on that same pipe up to
/// `AUTOSTART_TIMEOUT` until it succeeds. Returns `Ok(())` once the server is
/// accepting connections — callers do their own real `PipeConn::connect`
/// afterward. `config` is the invocation's `-f <file>` (Task 7), forwarded
/// as `--config <file>` ONLY when `Some` — omitted entirely means the
/// spawned server falls back to its own default `.tmux.conf`/`.winmux.conf`
/// discovery chain.
pub fn autostart_server(socket: &str, config: Option<&str>) -> io::Result<()> {
    let full_name = pipe::pipe_name(socket);
    let exe = std::env::current_exe()?;

    let mut command = std::process::Command::new(exe);
    command.args(["__server", "--pipe", &full_name]);
    if let Some(c) = config {
        command.args(["--config", c]);
    }
    command
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()?;

    let deadline = Instant::now() + AUTOSTART_TIMEOUT;
    loop {
        match PipeConn::connect(&full_name) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                thread::sleep(AUTOSTART_POLL_INTERVAL);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Connect to `pipe_full_name`, send `first` (an `Attach` frame), then run
/// as an attached client until the server sends a terminal `Exit` (or the
/// connection is lost). Returns the process exit code.
///
/// Terminal restoration ordering matters: on `Exit`, `Host` is dropped
/// (restoring the console) BEFORE the exit message is printed, so the
/// message lands on the normal screen, not the alt screen.
///
/// The stdin-reader thread spawned here blocks in `host::read_stdin` with no
/// clean way to cancel it. That's fine: every return path out of this
/// function is followed by `main.rs` calling `std::process::exit`, which
/// tears down the whole process (and that thread with it) immediately.
///
/// Ordering: `Host::enter()` runs BEFORE the pipe connect and the `Attach`
/// frame (Task 8 review fix). If the terminal can't be entered at all (e.g.
/// stdio is redirected — no console), no `Attach` ever reaches the server,
/// so no server-side session is created for a client that can never use it.
/// (Previously the frame went out first; a failed `enter` then stranded a
/// session — and, with exit-empty, kept an autostarted server alive
/// forever.) Every `?` after `enter` drops `host` (a local) on the way out,
/// restoring the console before `main.rs` prints the error.
pub fn attach(pipe_full_name: &str, first: ClientMsg) -> Result<i32, Box<dyn Error>> {
    let mut host = Host::enter()?;
    let (mut cols, mut rows) = host.size().unwrap_or((80, 24));

    let mut conn = PipeConn::connect(pipe_full_name)?;
    protocol::write_client_msg(&mut conn, &first)?;

    // Stdin thread: forwards raw console input as Stdin frames over its own
    // cloned connection. On read failure/EOF (console closing), it sends a
    // best-effort Detach frame before exiting.
    let mut stdin_conn = conn.try_clone()?;
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match host::read_stdin(&mut buf) {
                Ok(0) => {
                    let _ = protocol::write_client_msg(&mut stdin_conn, &ClientMsg::Detach);
                    break;
                }
                Ok(n) => {
                    let msg = ClientMsg::Stdin(buf[..n].to_vec());
                    if protocol::write_client_msg(&mut stdin_conn, &msg).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = protocol::write_client_msg(&mut stdin_conn, &ClientMsg::Detach);
                    break;
                }
            }
        }
    });

    // Reader thread: decodes ServerMsg frames off its own cloned connection
    // and relays them to the main loop, which also needs to wake up on a
    // tick (to poll for a terminal resize) — a plain blocking read on the
    // main thread couldn't do both.
    let mut reader_conn = conn.try_clone()?;
    let (tx, rx) = channel::<io::Result<ServerMsg>>();
    thread::spawn(move || loop {
        let msg = protocol::read_server_msg(&mut reader_conn);
        let is_err = msg.is_err();
        if tx.send(msg).is_err() || is_err {
            break;
        }
    });

    loop {
        match rx.recv_timeout(TICK) {
            Ok(Ok(ServerMsg::Output(bytes))) => {
                host.write(&bytes)?;
            }
            Ok(Ok(ServerMsg::Exit { code, msg })) => {
                // Restore the terminal FIRST, then print — the message must
                // land on the normal screen, not the alt screen.
                drop(host);
                if code == 0 {
                    println!("{msg}");
                } else {
                    eprintln!("{msg}");
                }
                return Ok(code as i32);
            }
            Ok(Ok(ServerMsg::CliDone { .. })) => {
                // Not expected on an attached connection; ignore.
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                drop(host);
                eprintln!("[lost server]");
                return Ok(1);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Ok((ncols, nrows)) = host.size() {
                    if (ncols, nrows) != (cols, rows) {
                        cols = ncols;
                        rows = nrows;
                        let _ = protocol::write_client_msg(
                            &mut conn,
                            &ClientMsg::Resize { cols, rows },
                        );
                    }
                }
            }
        }
    }
}
