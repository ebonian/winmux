//! CLI subset command executor (Task 7): `execute_cli`, its per-command
//! `cli_*` handlers, and the small hand-rolled argv parser they share.
//!
//! Split out of `server.rs` once that file passed ~1400 lines (flagged in
//! the task brief as the threshold to consider it). This is a private
//! submodule — `crate::server`'s only public item remains `pub fn run`;
//! everything here is reachable only via `impl Server` methods called from
//! `server.rs`'s `handle_cli`. See
//! `docs/specs/2026-07-07-server-client-interfaces.md`'s `## server`
//! "CLI subset" section for the exact command surface and output strings.

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::Time::FileTimeToSystemTime;

use std::time::SystemTime;

use crate::model::Registry;
use crate::protocol::ServerMsg;

use super::{send_msg, spawn_pane, ClientId, Server, MONTHS};

/// Abbreviated C-locale English weekday names, indexed by `SYSTEMTIME::wDayOfWeek`
/// (0 = Sunday .. 6 = Saturday).
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Convert a `SystemTime` to a local-time `SYSTEMTIME` (carrying
/// `wDayOfWeek`) for the CLI `ls` command's tmux-style creation-time
/// formatting. Two Win32 hops, both effectively infallible for any timestamp
/// actually produced by `SystemTime::now()`: `FileTimeToLocalFileTime` (UTC
/// FILETIME -> local FILETIME, applying the current timezone/DST bias) then
/// `FileTimeToSystemTime` (-> SYSTEMTIME, the only Win32 call that hands back
/// a weekday without a manual Zeller's-congruence calculation).
fn to_local_systemtime(t: SystemTime) -> SYSTEMTIME {
    let dur = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // FILETIME ticks are 100ns intervals since 1601-01-01; the Unix epoch
    // (1970-01-01) falls 116444736000000000 ticks after that fixed point.
    let ticks = dur.as_secs() * 10_000_000 + (dur.subsec_nanos() as u64) / 100 + 116_444_736_000_000_000;
    let utc_ft = FILETIME {
        dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut local_ft = FILETIME::default();
    let mut st = SYSTEMTIME::default();
    // SAFETY: both calls take plain-old-data in/out pointers to locals that
    // outlive the call; no other preconditions.
    unsafe {
        let _ = FileTimeToLocalFileTime(&utc_ft, &mut local_ft);
        let _ = FileTimeToSystemTime(&local_ft, &mut st);
    }
    st
}

/// tmux `ls`-style creation time: `%a %b %e %H:%M:%S %Y` (C-locale weekday
/// and month, `%e` = space-padded day), e.g. `Tue Jul  7 09:14:22 2026`.
fn format_ctime(t: SystemTime) -> String {
    let st = to_local_systemtime(t);
    let weekday = WEEKDAYS[(st.wDayOfWeek.min(6)) as usize];
    let month = MONTHS[(st.wMonth.clamp(1, 12) as usize) - 1];
    format!(
        "{weekday} {month} {:2} {:02}:{:02}:{:02} {}",
        st.wDay, st.wHour, st.wMinute, st.wSecond, st.wYear
    )
}

/// Thin re-export of `model.rs`'s `validate_name` (the SAME hardened rule
/// `Registry::create_session` applies: empty names, tmux's two target
/// separators, and — final-review fix, 2026-07-07 — any control character,
/// with the echoed name in the error sanitized). Used by the CLI
/// rename-session/rename-window handlers below AND by `server.rs`'s
/// `feed_prompt_byte` (rename prompt commit) — `pub(super)` so the parent
/// module can reuse it, since the model has no rename API of its own
/// (`Session`/`Window` `name` fields are public and mutated directly).
/// `noun` is `"session"` or `"window"`, matching tmux's `bad session name` /
/// analogous window-rename error text. Kept as a separate function (rather
/// than every call site importing `crate::model::validate_name` directly)
/// so both rename call sites and the create-session call site are visibly
/// going through one rule, not two that happen to agree today.
pub(super) fn validate_target_name(name: &str, noun: &str) -> Result<(), String> {
    crate::model::validate_name(name, noun)
}

/// Per-command usage lines, returned as the `err` of a `CliDone{1, ...}`
/// for BOTH a missing required argument and an unrecognized `-` flag
/// (an unknown flag is never silently treated as a positional —
/// `rename-session -t foo -q bar` must not rename the session to `-q`).
/// Recorded verbatim in the contract's CLI section.
const USAGE_LS: &str = "usage: list-sessions";
const USAGE_HAS: &str = "usage: has-session -t target";
const USAGE_KILL_SESSION: &str = "usage: kill-session [-t target]";
const USAGE_KILL_SERVER: &str = "usage: kill-server";
const USAGE_NEW: &str = "usage: new-session [-d] [-s name] [-x cols] [-y rows]";
const USAGE_RENAME_SESSION: &str = "usage: rename-session [-t target] new-name";
const USAGE_RENAME_WINDOW: &str = "usage: rename-window [-t target] new-name";
const USAGE_LSW: &str = "usage: list-windows [-t target]";
const USAGE_DETACH: &str = "usage: detach-client -s target";

/// The `(1, "", usage)` reply shape shared by every argv error.
fn usage_err(usage: &str) -> (u8, String, String) {
    (1, String::new(), usage.to_string())
}

/// Minimal hand-rolled flag parser for the CLI subset: `-t`/`-s` (string),
/// `-x`/`-y` (u16), `-d` (bare flag), everything else collected in order as
/// `positional` (e.g. `rename-*`'s trailing new-name).
#[derive(Default)]
struct CliArgs {
    t: Option<String>,
    s: Option<String>,
    x: Option<u16>,
    y: Option<u16>,
    positional: Vec<String>,
}

impl CliArgs {
    /// `allowed` lists the flags the calling command recognizes (a subset
    /// of `-t -s -x -y -d`). Any other `-`-prefixed token (length > 1, so
    /// a bare `-` still counts as a positional) is `Err(())`; the caller
    /// converts that into its usage line via [`usage_err`].
    fn parse(args: &[String], allowed: &[&str]) -> Result<CliArgs, ()> {
        let mut p = CliArgs::default();
        let mut i = 0;
        while i < args.len() {
            let tok = args[i].as_str();
            if tok.starts_with('-') && tok.len() > 1 {
                if !allowed.contains(&tok) {
                    return Err(());
                }
                match tok {
                    "-t" => {
                        i += 1;
                        p.t = args.get(i).cloned();
                    }
                    "-s" => {
                        i += 1;
                        p.s = args.get(i).cloned();
                    }
                    "-x" => {
                        i += 1;
                        p.x = args.get(i).and_then(|v| v.parse().ok());
                    }
                    "-y" => {
                        i += 1;
                        p.y = args.get(i).and_then(|v| v.parse().ok());
                    }
                    "-d" => {}
                    // Unreachable: `allowed` only ever names the five flags
                    // above, but keep the parser total over its input.
                    _ => return Err(()),
                }
            } else {
                p.positional.push(tok.to_string());
            }
            i += 1;
        }
        Ok(p)
    }
}

impl Server {
    /// Execute one CLI command (a `Cli` frame's argv) against live registry
    /// state. Returns `(exit code, stdout, stderr)` — see the `## server`
    /// contract section ("CLI subset") for the exact output strings.
    pub(super) fn execute_cli(&mut self, argv: &[String]) -> (u8, String, String) {
        let Some(cmd) = argv.first() else {
            return (1, String::new(), "unknown command".to_string());
        };
        let rest = &argv[1..];
        match cmd.as_str() {
            "list-sessions" | "ls" => self.cli_list_sessions(rest),
            "has-session" | "has" => self.cli_has_session(rest),
            "kill-session" => self.cli_kill_session(rest),
            "kill-server" => self.cli_kill_server(rest),
            "new-session" | "new" => self.cli_new_session(rest),
            "rename-session" => self.cli_rename_session(rest),
            "rename-window" => self.cli_rename_window(rest),
            "list-windows" | "lsw" => self.cli_list_windows(rest),
            "detach-client" => self.cli_detach_client(rest),
            _ => (1, String::new(), "unknown command".to_string()),
        }
    }

    fn cli_list_sessions(&mut self, args: &[String]) -> (u8, String, String) {
        if CliArgs::parse(args, &[]).is_err() {
            return usage_err(USAGE_LS);
        }
        if self.registry.is_empty() {
            return (1, String::new(), "no sessions".to_string());
        }
        let mut out = String::new();
        for s in self.registry.sessions() {
            let attached = if self.clients.values().any(|c| c.session.as_deref() == Some(s.name.as_str())) {
                " (attached)"
            } else {
                ""
            };
            out.push_str(&format!(
                "{}: {} windows (created {}){}\n",
                s.name,
                s.windows.len(),
                format_ctime(s.created),
                attached
            ));
        }
        (0, out, String::new())
    }

    fn cli_has_session(&mut self, args: &[String]) -> (u8, String, String) {
        let Ok(p) = CliArgs::parse(args, &["-t"]) else {
            return usage_err(USAGE_HAS);
        };
        let Some(target) = p.t else {
            return usage_err(USAGE_HAS);
        };
        match self.registry.find(&target) {
            Ok(_) => (0, String::new(), String::new()),
            Err(e) => (1, String::new(), e),
        }
    }

    fn cli_kill_session(&mut self, args: &[String]) -> (u8, String, String) {
        let Ok(p) = CliArgs::parse(args, &["-t"]) else {
            return usage_err(USAGE_KILL_SESSION);
        };
        let name = match p.t {
            Some(t) => match self.registry.find(&t) {
                Ok(s) => s.name.clone(),
                Err(e) => return (1, String::new(), e),
            },
            None => match self.registry.sessions().last() {
                Some(s) => s.name.clone(),
                None => return (1, String::new(), "no sessions".to_string()),
            },
        };
        self.destroy_session(&name);
        (0, String::new(), String::new())
    }

    /// Tear the whole server down: every attached client gets
    /// `Exit{0, "[server exited]"}`, every pane's ConPTY is dropped, and the
    /// registry is cleared so `run`'s exit-empty check fires this turn.
    fn cli_kill_server(&mut self, args: &[String]) -> (u8, String, String) {
        if CliArgs::parse(args, &[]).is_err() {
            return usage_err(USAGE_KILL_SERVER);
        }
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            if let Some(c) = self.clients.remove(&id) {
                send_msg(&c.tx, &ServerMsg::Exit { code: 0, msg: "[server exited]".to_string() });
            }
        }
        self.panes.clear();
        self.last_rects.clear();
        self.registry = Registry::new();
        self.had_session = true;
        (0, String::new(), String::new())
    }

    fn cli_new_session(&mut self, args: &[String]) -> (u8, String, String) {
        let Ok(p) = CliArgs::parse(args, &["-d", "-s", "-x", "-y"]) else {
            return usage_err(USAGE_NEW);
        };
        let size = (p.x.unwrap_or(80).max(1), p.y.unwrap_or(24).max(1));
        let pane_id = self.mint_pane_id();
        match spawn_pane(pane_id, size.0, size.1, &self.tx) {
            Ok(pr) => {
                self.panes.insert(pane_id, pr);
                match self.registry.create_session(p.s.as_deref(), pane_id, size) {
                    Ok(_) => {
                        self.had_session = true;
                        (0, String::new(), String::new())
                    }
                    Err(e) => {
                        self.panes.remove(&pane_id);
                        (1, String::new(), e)
                    }
                }
            }
            Err(e) => (1, String::new(), format!("failed to spawn shell: {e}")),
        }
    }

    fn cli_rename_session(&mut self, args: &[String]) -> (u8, String, String) {
        let Ok(p) = CliArgs::parse(args, &["-t"]) else {
            return usage_err(USAGE_RENAME_SESSION);
        };
        let (Some(target), Some(new_name)) = (p.t, p.positional.into_iter().next()) else {
            return usage_err(USAGE_RENAME_SESSION);
        };
        let old_name = match self.registry.find(&target) {
            Ok(s) => s.name.clone(),
            Err(e) => return (1, String::new(), e),
        };
        if let Err(e) = validate_target_name(&new_name, "session") {
            return (1, String::new(), e);
        }
        if new_name != old_name && self.registry.sessions().iter().any(|s| s.name == new_name) {
            return (1, String::new(), format!("duplicate session: {new_name}"));
        }
        if let Some(session) = self.registry.session_mut(&old_name) {
            session.name = new_name.clone();
        }
        self.rename_session_everywhere(&old_name, &new_name);
        (0, String::new(), String::new())
    }

    fn cli_rename_window(&mut self, args: &[String]) -> (u8, String, String) {
        let Ok(p) = CliArgs::parse(args, &["-t"]) else {
            return usage_err(USAGE_RENAME_WINDOW);
        };
        let (Some(target), Some(new_name)) = (p.t, p.positional.into_iter().next()) else {
            return usage_err(USAGE_RENAME_WINDOW);
        };
        let (sess_part, idx_part) = match target.split_once(':') {
            Some((s, i)) => (s.to_string(), Some(i.to_string())),
            None => (target.clone(), None),
        };
        let session = match self.registry.find(&sess_part) {
            Ok(s) => s,
            Err(e) => return (1, String::new(), e),
        };
        let wid = match idx_part {
            Some(idx_str) => {
                let Ok(idx) = idx_str.parse::<u32>() else {
                    return (1, String::new(), format!("window not found: {idx_str}"));
                };
                match session.windows.iter().find(|w| w.index == idx) {
                    Some(w) => w.id,
                    None => return (1, String::new(), format!("window not found: {idx}")),
                }
            }
            None => session.current,
        };
        if let Err(e) = validate_target_name(&new_name, "window") {
            return (1, String::new(), e);
        }
        if let Some(window) = session.windows.iter_mut().find(|w| w.id == wid) {
            window.name = new_name;
        }
        (0, String::new(), String::new())
    }

    fn cli_list_windows(&mut self, args: &[String]) -> (u8, String, String) {
        let Ok(p) = CliArgs::parse(args, &["-t"]) else {
            return usage_err(USAGE_LSW);
        };
        let name = match p.t {
            Some(t) => match self.registry.find(&t) {
                Ok(s) => s.name.clone(),
                Err(e) => return (1, String::new(), e),
            },
            None => match self.registry.sessions().last() {
                Some(s) => s.name.clone(),
                None => return (1, String::new(), "no sessions".to_string()),
            },
        };
        let Some(session) = self.registry.sessions().iter().find(|s| s.name == name) else {
            return (1, String::new(), format!("can't find session: {name}"));
        };
        let mut out = String::new();
        for w in &session.windows {
            let flag = if w.id == session.current {
                "*"
            } else if Some(w.id) == session.last {
                "-"
            } else {
                ""
            };
            let active = if w.id == session.current { " (active)" } else { "" };
            let panes = w.layout.panes().len();
            let (cols, rows) = session.size;
            out.push_str(&format!("{}: {}{} ({} panes) [{}x{}]{}\n", w.index, w.name, flag, panes, cols, rows, active));
        }
        (0, out, String::new())
    }

    fn cli_detach_client(&mut self, args: &[String]) -> (u8, String, String) {
        let Ok(p) = CliArgs::parse(args, &["-s"]) else {
            return usage_err(USAGE_DETACH);
        };
        let Some(target) = p.s else {
            return usage_err(USAGE_DETACH);
        };
        let name = match self.registry.find(&target) {
            Ok(s) => s.name.clone(),
            Err(e) => return (1, String::new(), e),
        };
        let ids: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session.as_deref() == Some(name.as_str()))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(c) = self.clients.remove(&id) {
                send_msg(&c.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {name})]") });
            }
        }
        self.recompute_session_size(&name);
        self.apply_layout_for_session(&name);
        (0, String::new(), String::new())
    }
}
