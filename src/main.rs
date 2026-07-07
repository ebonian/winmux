use std::env;
use std::io;

use winmux::cli::{self, Command};
use winmux::client;
use winmux::host;
use winmux::logging::log_line;
use winmux::pipe::{self, PipeConn};
use winmux::protocol::{self, AttachMode, ClientMsg, ServerMsg};
use winmux::server;

fn main() {
    // Drop PSModulePath from our environment so pane shells inherit a clean
    // one. When winmux is launched from PowerShell 7, pwsh exports a
    // PSModulePath whose PS7 module directories precede the Windows
    // PowerShell 5.1 ones; a powershell.exe pane then resolves PSReadLine to
    // PS7's script-based module, which the default execution policy refuses
    // to load, and the pane prints "Cannot load PSReadline module. Console is
    // running without PSReadline." With the variable absent, each PowerShell
    // edition reconstructs its own correct default module path. (Trade-off:
    // a user-customized PSModulePath is not forwarded to panes.)
    //
    // Must happen before any pane spawns (including server-spawned ones, so
    // this must run in every role, not just the attached-client path) and
    // while still single-threaded (mutating the environment is not
    // thread-safe on Windows).
    env::remove_var("PSModulePath");

    host::install_panic_hook();

    let args: Vec<String> = env::args().skip(1).collect();
    let invocation = match cli::parse(&args) {
        Ok(inv) => inv,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    };

    let code = match invocation.cmd {
        Command::Help => {
            print!("{}", cli::usage_text());
            0
        }
        Command::ServerRole { pipe, config } => run_server_role(&pipe, &config),
        Command::NewSession { name, detached, cols, rows } => {
            run_new_session(&invocation.socket, invocation.config.as_deref(), name, detached, cols, rows)
        }
        Command::Attach { target, detach_others } => {
            run_attach(&invocation.socket, target, detach_others)
        }
        Command::Control(argv) => run_control(&invocation.socket, argv),
    };
    std::process::exit(code);
}

/// Connect to `pipe`; if nothing is bound there yet (`NotFound`), autostart
/// the server on `socket` and wait for it to come up. Any other connect
/// error is propagated as-is. `config` is this invocation's `-f <file>`
/// (Task 7) — forwarded to the autostarted server's `--config` ONLY when
/// THIS invocation is the one actually starting it (tmux semantics: `-f`
/// against an already-running server is ignored, since config is read once
/// at server start).
fn ensure_server(pipe: &str, socket: &str, config: Option<&str>) -> io::Result<()> {
    match PipeConn::connect(pipe) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => client::autostart_server(socket, config),
        Err(e) => Err(e),
    }
}

/// stderr text for the fail-fast "no console" guard (Task 9 scope A) shared
/// by both attached-session entry points below.
const NO_CONSOLE_MSG: &str = "open terminal failed: not a console";

fn run_new_session(
    socket: &str,
    config: Option<&str>,
    name: Option<String>,
    detached: bool,
    cols: u16,
    rows: u16,
) -> i32 {
    let pipe_full_name = pipe::pipe_name(socket);

    // Fail fast, BEFORE autostart: an attached session needs a real console.
    // Probing here (rather than only inside `client::attach`, after
    // `ensure_server` may already have spawned a detached server) closes a
    // gap where redirected stdio (no console at all) would leave an idle
    // autostarted server behind forever -- it never gets a session, so
    // `run`'s exit-empty check (`had_session && registry.is_empty()`) never
    // fires because `had_session` never flips true. `NewSession{detached:
    // true}` has no console to probe (and needs none), so it's exempt.
    let console = if detached {
        None
    } else {
        match host::console_size() {
            Ok(sz) => Some(sz),
            Err(_) => {
                eprintln!("{NO_CONSOLE_MSG}");
                return 1;
            }
        }
    };

    if let Err(e) = ensure_server(&pipe_full_name, socket, config) {
        eprintln!("winmux: failed to start server: {e}");
        return 1;
    }

    if detached {
        let cols = if cols != 0 { cols } else { 80 };
        let rows = if rows != 0 { rows } else { 24 };
        let mut argv = vec!["new-session".to_string(), "-d".to_string()];
        if let Some(n) = &name {
            argv.push("-s".to_string());
            argv.push(n.clone());
        }
        argv.push("-x".to_string());
        argv.push(cols.to_string());
        argv.push("-y".to_string());
        argv.push(rows.to_string());
        return send_cli(&pipe_full_name, argv);
    }

    let (probe_cols, probe_rows) = console.unwrap_or((80, 24));
    let cols = if cols != 0 { cols } else { probe_cols };
    let rows = if rows != 0 { rows } else { probe_rows };
    let mode = if name.is_some() { AttachMode::NewNamed } else { AttachMode::NewAuto };
    let first = ClientMsg::Attach {
        mode,
        detach_others: false,
        cols,
        rows,
        name: name.unwrap_or_default(),
    };
    run_attach_client(&pipe_full_name, first)
}

fn run_attach(socket: &str, target: Option<String>, detach_others: bool) -> i32 {
    // Fail fast, BEFORE even probing/connecting: an attach with no console
    // must not touch the server at all (scope A) -- there is nothing a
    // console-less client could usefully do with an existing session either.
    let (cols, rows) = match host::console_size() {
        Ok(sz) => sz,
        Err(_) => {
            eprintln!("{NO_CONSOLE_MSG}");
            return 1;
        }
    };

    let pipe_full_name = pipe::pipe_name(socket);
    // NO autostart: a pure attach against a missing server is an error.
    if let Err(e) = probe_running(&pipe_full_name) {
        return report_connect_error(&pipe_full_name, e);
    }

    let first = ClientMsg::Attach {
        mode: AttachMode::Existing,
        detach_others,
        cols,
        rows,
        name: target.unwrap_or_default(),
    };
    run_attach_client(&pipe_full_name, first)
}

fn run_control(socket: &str, argv: Vec<String>) -> i32 {
    let pipe_full_name = pipe::pipe_name(socket);
    // NO autostart: a pure query/control command against a missing server
    // is an error (only `new-session`, including bare, auto-starts).
    send_cli(&pipe_full_name, argv)
}

/// `Ok(())` if a server is already listening on `pipe`; otherwise the
/// connect error (including `NotFound`) is returned for the caller to report.
fn probe_running(pipe: &str) -> io::Result<()> {
    PipeConn::connect(pipe).map(|_| ())
}

fn report_connect_error(pipe: &str, e: io::Error) -> i32 {
    if e.kind() == io::ErrorKind::NotFound {
        eprintln!("no server running on {pipe}");
    } else {
        eprintln!("winmux: {e}");
    }
    1
}

fn run_attach_client(pipe_full_name: &str, first: ClientMsg) -> i32 {
    match client::attach(pipe_full_name, first) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("winmux: {e}");
            1
        }
    }
}

/// One-shot control round trip: connect (no autostart), send `Cli(argv)`,
/// print the `CliDone` reply, exit with its code.
fn send_cli(pipe_full_name: &str, argv: Vec<String>) -> i32 {
    let mut conn = match PipeConn::connect(pipe_full_name) {
        Ok(c) => c,
        Err(e) => return report_connect_error(pipe_full_name, e),
    };
    if let Err(e) = protocol::write_client_msg(&mut conn, &ClientMsg::Cli(argv)) {
        eprintln!("winmux: {e}");
        return 1;
    }
    match protocol::read_server_msg(&mut conn) {
        Ok(ServerMsg::CliDone { code, out, err }) => {
            if !out.is_empty() {
                print!("{out}");
            }
            if !err.is_empty() {
                eprint!("{err}");
            }
            code as i32
        }
        Ok(other) => {
            eprintln!("winmux: unexpected server reply: {other:?}");
            1
        }
        Err(e) => {
            eprintln!("winmux: {e}");
            1
        }
    }
}

// ---- server role: headless, logs to a file instead of a console ----------

/// Chain a file-logging panic hook in front of whatever's already installed
/// (`host::install_panic_hook`, from `main`) — the server is headless, so
/// the console-restoration hook underneath is harmless but the panic must
/// also be recorded somewhere a developer can find it.
fn install_server_log_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log_line(&format!("panic: {info}"));
        previous(info);
    }));
}

/// `config` is the `__server --config <path>` args (repeatable, Task 7) —
/// forwarded verbatim to `server::run`, which treats an empty slice as "use
/// the default `.tmux.conf`/`.winmux.conf` discovery chain" and a non-empty
/// one as replacing that chain entirely.
fn run_server_role(pipe: &str, config: &[String]) -> i32 {
    log_line("server starting");
    install_server_log_panic_hook();
    match server::run(pipe, config) {
        Ok(()) => {
            log_line("server exited cleanly (exit-empty)");
            0
        }
        Err(e) => {
            log_line(&format!("server error: {e}"));
            1
        }
    }
}
