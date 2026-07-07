//! Pure argv parser for the tmux-style CLI subset (see
//! `docs/specs/2026-07-07-server-client-design.md`, "CLI subset", and the
//! `## cli` section of the sibling interfaces contract).
//!
//! No I/O, no Windows APIs — unit-tested directly on `&[String]`. `main.rs`
//! is the only caller; it turns a parsed `Invocation` into pipe connects,
//! server autostart, and either a one-shot `Cli` round trip or a full
//! attached client session.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// `-L <name>` socket name; defaults to `"default"`.
    pub socket: String,
    pub cmd: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Bare `winmux`, or `new-session`/`new [-d] [-s name] [-x cols] [-y rows]`.
    /// `cols`/`rows` are `0` when not given on the command line — `main.rs`
    /// fills in a real value (console probe or a headless default).
    NewSession {
        name: Option<String>,
        detached: bool,
        cols: u16,
        rows: u16,
    },
    /// `attach-session`/`attach`/`a [-d] [-t target]`. `target: None` means
    /// "no `-t`" — the server resolves that to the most recently created
    /// session. `-d` here means `detach_others` (tmux's attach `-d`, distinct
    /// from `new-session -d`'s "detached").
    Attach {
        target: Option<String>,
        detach_others: bool,
    },
    /// Every other recognized subcommand (`ls`, `list-sessions`,
    /// `has-session`, `kill-session`, `kill-server`, `rename-session`,
    /// `rename-window`, `list-windows`, `lsw`, `detach-client`, `has`, ...),
    /// forwarded verbatim as a `Cli` frame's argv — the server owns parsing
    /// and validation for these.
    Control(Vec<String>),
    /// `__server --pipe <full-pipe-name>` — hidden headless server role.
    ServerRole { pipe: String },
    /// `--help` / `-h` / `help`.
    Help,
}

const USAGE: &str = "usage: winmux [-L socket-name] [command [args]]\n\
Supported commands:\n\
  new-session|new [-d] [-s name] [-x cols] [-y rows]\n\
  attach-session|attach|a [-d] [-t target]\n\
  detach-client [-s name]\n\
  list-sessions|ls\n\
  list-windows|lsw [-t name]\n\
  has-session|has [-t name]\n\
  kill-session [-t name]\n\
  kill-server\n\
  rename-session [-t target] new-name\n\
  rename-window [-t target] new-name\n\
Global: -L socket-name\n\
Bare `winmux` (no command) is `new-session`.\n";

/// Usage text for `Help` (printed by `main.rs`, exit 0).
pub fn usage_text() -> &'static str {
    USAGE
}

fn usage_err() -> String {
    USAGE.to_string()
}

/// Consume the value following `flag` at `args[*i]` (which must equal
/// `flag`), advancing `*i` by 2. `Err` (usage) if the value is missing.
fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    match args.get(*i + 1) {
        Some(v) => {
            let v = v.clone();
            *i += 2;
            Ok(v)
        }
        None => Err(format!("usage: missing value for {flag}\n{USAGE}")),
    }
}

fn parse_u16_arg(s: &str, flag: &str) -> Result<u16, String> {
    s.parse::<u16>()
        .map_err(|_| format!("usage: {flag} expects a number, got '{s}'\n{USAGE}"))
}

fn parse_new_session(rest: &[String]) -> Result<Command, String> {
    let mut name = None;
    let mut detached = false;
    let mut cols: u16 = 0;
    let mut rows: u16 = 0;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-d" => {
                detached = true;
                i += 1;
            }
            "-s" => name = Some(take_value(rest, &mut i, "-s")?),
            "-x" => cols = parse_u16_arg(&take_value(rest, &mut i, "-x")?, "-x")?,
            "-y" => rows = parse_u16_arg(&take_value(rest, &mut i, "-y")?, "-y")?,
            _ => return Err(usage_err()),
        }
    }
    Ok(Command::NewSession { name, detached, cols, rows })
}

fn parse_attach(rest: &[String]) -> Result<Command, String> {
    let mut target = None;
    let mut detach_others = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-d" => {
                detach_others = true;
                i += 1;
            }
            "-t" => target = Some(take_value(rest, &mut i, "-t")?),
            _ => return Err(usage_err()),
        }
    }
    Ok(Command::Attach { target, detach_others })
}

fn parse_server_role(rest: &[String]) -> Result<Command, String> {
    if rest.len() == 2 && rest[0] == "--pipe" {
        Ok(Command::ServerRole { pipe: rest[1].clone() })
    } else {
        Err(format!("usage: __server --pipe <full-pipe-name>\n{USAGE}"))
    }
}

/// Parse `env::args().skip(1)`-style argv into an `Invocation`. `Err` is a
/// usage message meant to be printed to stderr with exit code 1.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    // Extract every top-level `-L <name>` pair, wherever it appears, leaving
    // the remaining tokens in order — "keep it simple: accept -L anywhere
    // top-level" (task brief). None of the supported subcommands has a flag
    // of its own named `-L`, so this can't collide with passthrough argv.
    let mut socket = "default".to_string();
    let mut rest: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-L" {
            socket = take_value(args, &mut i, "-L")?;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }

    if rest.is_empty() {
        return Ok(Invocation {
            socket,
            cmd: Command::NewSession { name: None, detached: false, cols: 0, rows: 0 },
        });
    }

    let cmd = match rest[0].as_str() {
        "-h" | "--help" | "help" => Command::Help,
        "__server" => parse_server_role(&rest[1..])?,
        "new-session" | "new" => parse_new_session(&rest[1..])?,
        "attach-session" | "attach" | "a" => parse_attach(&rest[1..])?,
        s if s.starts_with('-') => return Err(usage_err()),
        _ => Command::Control(rest),
    };
    Ok(Invocation { socket, cmd })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_is_new_session() {
        let inv = parse(&args(&[])).unwrap();
        assert_eq!(
            inv,
            Invocation {
                socket: "default".to_string(),
                cmd: Command::NewSession { name: None, detached: false, cols: 0, rows: 0 },
            }
        );
    }

    #[test]
    fn new_flags() {
        let inv = parse(&args(&["new", "-d", "-s", "x", "-x", "100", "-y", "30"])).unwrap();
        assert_eq!(
            inv.cmd,
            Command::NewSession {
                name: Some("x".to_string()),
                detached: true,
                cols: 100,
                rows: 30,
            }
        );
    }

    #[test]
    fn ls_alias() {
        let inv = parse(&args(&["ls"])).unwrap();
        assert_eq!(inv.cmd, Command::Control(vec!["ls".to_string()]));
    }

    #[test]
    fn attach_t() {
        let inv = parse(&args(&["attach", "-t", "foo"])).unwrap();
        assert_eq!(
            inv.cmd,
            Command::Attach { target: Some("foo".to_string()), detach_others: false }
        );
    }

    #[test]
    fn attach_dashd() {
        let inv = parse(&args(&["attach", "-d"])).unwrap();
        assert_eq!(inv.cmd, Command::Attach { target: None, detach_others: true });
    }

    #[test]
    fn dash_l_socket() {
        let inv = parse(&args(&["-L", "mysock", "ls"])).unwrap();
        assert_eq!(inv.socket, "mysock");
        assert_eq!(inv.cmd, Command::Control(vec!["ls".to_string()]));
    }

    #[test]
    fn server_role_parse() {
        let inv = parse(&args(&["__server", "--pipe", r"\\.\pipe\winmux-test"])).unwrap();
        assert_eq!(inv.cmd, Command::ServerRole { pipe: r"\\.\pipe\winmux-test".to_string() });
    }

    #[test]
    fn unknown_flag_err() {
        let err = parse(&args(&["-z"])).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn kill_session_passthrough() {
        let inv = parse(&args(&["kill-session", "-t", "foo"])).unwrap();
        assert_eq!(
            inv.cmd,
            Command::Control(vec!["kill-session".to_string(), "-t".to_string(), "foo".to_string()])
        );
    }

    #[test]
    fn help_parses() {
        assert_eq!(parse(&args(&["--help"])).unwrap().cmd, Command::Help);
        assert_eq!(parse(&args(&["-h"])).unwrap().cmd, Command::Help);
        assert_eq!(parse(&args(&["help"])).unwrap().cmd, Command::Help);
    }
}
