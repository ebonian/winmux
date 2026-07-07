//! Unified command dispatcher (Task 6): resolves and executes [`RawCmd`]s
//! from all entry points — key bindings, the CLI, and the `:`/rename
//! status-line prompts — against live server state. See the design spec's
//! "Server dispatcher" section and the `## server-dispatch` contract
//! section.
//!
//! Two execution paths share almost all their logic:
//! - `execute_headless` — no acting client (CLI frames, `.tmux.conf`/
//!   `source-file` lines). Session/window/pane targets fall back to the
//!   most-recently-created session when no `-t`/`-s` is given.
//! - `execute_for_client` — an acting client exists (a key binding fired, or
//!   the `:` prompt committed). Targets fall back to the acting client's own
//!   session/focused window/pane; a handful of commands (confirm-before,
//!   command-prompt, switch-client, bare rename-*, detach-client with no
//!   `-s`) only make sense with a client and are errors headlessly.
//!
//! Both funnel into small per-command `exec_*` helpers that take an
//! `Option<&str>` "acting client's session name" rather than any client
//! object, so the resolution logic (`resolve_session_name`/
//! `resolve_window_target`/`resolve_pane_target`) is shared verbatim.

use std::time::{Instant, SystemTime};

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::Time::FileTimeToSystemTime;

use crate::bindings::Binding;
use crate::cmd::{self, CopyAction, ParsedCmd, RawCmd};
use crate::geom::{Direction, Rect};
use crate::input::WhichTable;
use crate::layout::{PaneId, SplitDir};
use crate::model::{Registry, Session, Window, WindowId};
use crate::options::FormatCtx;
use crate::protocol::ServerMsg;

use super::{
    key_to_view, sel_key, send_msg, spawn_pane, system_time_parts, ClientId, ClientMode, ClientState, ConfigCandidate,
    CopyState, PromptKind, SelState, Server, MONTHS,
};
use crate::grid::Grid;

/// Abbreviated C-locale English weekday names, indexed by
/// `SYSTEMTIME::wDayOfWeek` (0 = Sunday .. 6 = Saturday). Duplicated from the
/// deleted `cli_exec.rs` (single remaining user, `format_ctime`).
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// The result of executing one command (or a `;`-chained sequence) against
/// live state. Most commands only ever produce `Ok`/`Err`; the remaining
/// three variants are how the handful of client-mutating commands
/// (`detach-client`, `kill-pane`/`kill-window` when they destroy the whole
/// session, `switch-client`) signal a special outcome back up to
/// `handle_stdin`, which owns the acting client's lifecycle (it was removed
/// from `self.clients` before dispatch began, mirroring the pre-Task-6
/// confirm-handling code).
pub(super) enum ExecOutcome {
    /// Success; a non-empty string is a transient message (`display-message`,
    /// or an informational CLI-style output).
    Ok(String),
    Err(String),
    /// The acting client should be dropped with a `[detached (from session
    /// <name>)]` exit message (caller has `session_name` already).
    Detach,
    /// The acting client's session was destroyed as a side effect (last pane
    /// of the last window killed); `destroy_session` has ALREADY run and
    /// messaged every OTHER attached client — the caller only needs to
    /// message its own (already-removed-from-the-map) client and drop it.
    Destroy,
    /// `switch-client -p`/`-n` moved the acting client to a different
    /// session: `(old_name, new_name)`, both of which need their
    /// size/layout recomputed now that the client is back in `self.clients`.
    SwitchedSession(String, String),
}

fn wrap(r: Result<String, String>) -> ExecOutcome {
    match r {
        Ok(s) => ExecOutcome::Ok(s),
        Err(e) => ExecOutcome::Err(e),
    }
}

/// Route one [`ExecOutcome`] into `client`/the caller's local flags. Shared
/// by the `Key`-binding dispatch site and the prompt/confirm commit site in
/// `handle_stdin`, both of which loop over `KeyInputEvent`s the same way the
/// pre-Task-6 code looped over `InputEvent`s.
pub(super) fn route_outcome(
    outcome: ExecOutcome,
    client: &mut ClientState,
    detach: &mut bool,
    destroy: &mut bool,
    session_switched: &mut Option<(String, String)>,
) {
    match outcome {
        ExecOutcome::Ok(out) => {
            if !out.is_empty() {
                client.message = Some((out, Instant::now()));
            }
        }
        ExecOutcome::Err(e) => client.message = Some((e, Instant::now())),
        ExecOutcome::Detach => *detach = true,
        ExecOutcome::Destroy => *destroy = true,
        ExecOutcome::SwitchedSession(old, new) => *session_switched = Some((old, new)),
    }
}

// ---- target resolution (SP3 simplified `session:window.pane` grammar) ----

/// Split an optional leading `session:` prefix off a target string. Absent
/// or empty session part -> `None` (falls back to the acting client's own
/// session, or the most-recently-created one).
fn split_session_prefix(t: &str) -> (Option<&str>, &str) {
    match t.split_once(':') {
        Some((s, rest)) => (if s.is_empty() { None } else { Some(s) }, rest),
        None => (None, t),
    }
}

/// `true` if `s` (optionally `=`-prefixed) parses as a window/pane index --
/// i.e. keeps its TODAY meaning (index in the contextual session/window)
/// rather than falling back to session-name resolution for a bare token.
fn looks_like_index(s: &str) -> bool {
    s.strip_prefix('=').unwrap_or(s).parse::<u32>().is_ok()
}

/// Resolve a window spec (the part of a target after any `session:` prefix,
/// used whole for window-targeting commands): empty/absent -> the session's
/// current window; `=N` or a bare number -> exact index match; otherwise a
/// name, exact-then-unambiguous-prefix (mirrors `Registry::find`).
fn resolve_window(session: &Session, spec: Option<&str>) -> Result<WindowId, String> {
    let s = match spec {
        None => return Ok(session.current),
        Some("") => return Ok(session.current),
        Some(s) => s,
    };
    let (exact, s2) = match s.strip_prefix('=') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    if let Ok(idx) = s2.parse::<u32>() {
        return session
            .windows
            .iter()
            .find(|w| w.index == idx)
            .map(|w| w.id)
            .ok_or_else(|| format!("window not found: {idx}"));
    }
    if exact {
        return session
            .windows
            .iter()
            .find(|w| w.name == s2)
            .map(|w| w.id)
            .ok_or_else(|| format!("window not found: {s2}"));
    }
    let matches: Vec<WindowId> = session.windows.iter().filter(|w| w.name.starts_with(s2)).map(|w| w.id).collect();
    match matches.len() {
        1 => Ok(matches[0]),
        _ => Err(format!("window not found: {s}")),
    }
}

/// Resolve a pane spec (the part of a target after `window.`, or the whole
/// remainder when there's no `.`): empty/absent -> the window's focused
/// pane; `+`/`-` -> next/previous pane (cyclic) relative to focus; otherwise
/// a bare number -> position in `window.layout.panes()` order (leaf/tree
/// order), per the design spec's target grammar note.
fn resolve_pane(window: &Window, spec: Option<&str>) -> Result<PaneId, String> {
    let panes = window.layout.panes();
    let s = match spec {
        None => return Ok(window.layout.focused()),
        Some("") => return Ok(window.layout.focused()),
        Some(s) => s,
    };
    if s == "+" {
        let idx = panes.iter().position(|&p| p == window.layout.focused()).unwrap_or(0);
        return Ok(panes[(idx + 1) % panes.len()]);
    }
    if s == "-" {
        let idx = panes.iter().position(|&p| p == window.layout.focused()).unwrap_or(0);
        return Ok(panes[(idx + panes.len() - 1) % panes.len()]);
    }
    let idx: usize = s.parse().map_err(|_| format!("pane not found: {s}"))?;
    panes.get(idx).copied().ok_or_else(|| format!("pane not found: {s}"))
}

/// Convert a `SystemTime` to a local-time `SYSTEMTIME` (carrying
/// `wDayOfWeek`) for the CLI `ls` command's tmux-style creation-time
/// formatting. Moved verbatim from the deleted `cli_exec.rs`.
fn to_local_systemtime(t: SystemTime) -> SYSTEMTIME {
    let dur = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
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

/// tmux `ls`-style creation time: `%a %b %e %H:%M:%S %Y`. Moved verbatim
/// from the deleted `cli_exec.rs`.
fn format_ctime(t: SystemTime) -> String {
    let st = to_local_systemtime(t);
    let weekday = WEEKDAYS[(st.wDayOfWeek.min(6)) as usize];
    let month = MONTHS[(st.wMonth.clamp(1, 12) as usize) - 1];
    format!(
        "{weekday} {month} {:2} {:02}:{:02}:{:02} {}",
        st.wDay, st.wHour, st.wMinute, st.wSecond, st.wYear
    )
}

fn is_bare(raw: &RawCmd, names: &[&str]) -> bool {
    raw.args.is_empty() && names.contains(&raw.name.as_str())
}

/// Trim a trailing run of blank (space) characters — tmux copy-mode's "don't
/// carry trailing pad cells into the copied text" rule, applied per extracted
/// line.
fn trim_trailing_blanks(s: &str) -> String {
    s.trim_end_matches(' ').to_string()
}

/// Extract one selection's text from `grid` (Task 3, sub-project 4): `sel`'s
/// anchor and the current cursor (`cx`/`cy`/`scroll`) are converted to
/// `sel_key`-comparable positions so extraction is independent of
/// `history_len` drift while selecting. Linear (`sel.rect == false`):
/// reading-order between the two endpoints — a single row is a plain
/// substring; multiple rows join the first row's tail, every whole row in
/// between, and the last row's head with `\n` (tmux-style; NOT `\r\n`),
/// each trailing-blank-trimmed. Rectangle (`sel.rect == true`): every
/// spanned row's `[min_col..=max_col]` slice, same per-row trimming, `\n`-
/// joined.
fn extract_selection_text(grid: &Grid, sel: &SelState, cx: u16, cy: u16, scroll: u32) -> String {
    let rows = grid.rows();
    let anchor_key = sel_key(sel.anchor_scroll, sel.anchor_y);
    let cursor_key = sel_key(scroll, cy);

    let row_text = |key: i64| -> Vec<char> {
        let (sb, row) = key_to_view(key, rows);
        grid.view_row_text(sb, row).chars().collect()
    };

    if sel.rect {
        let min_col = sel.anchor_x.min(cx) as usize;
        let max_col = sel.anchor_x.max(cx) as usize;
        let (top, bottom) = if anchor_key <= cursor_key { (anchor_key, cursor_key) } else { (cursor_key, anchor_key) };
        let mut lines = Vec::new();
        for key in top..=bottom {
            let chars = row_text(key);
            let lo = min_col.min(chars.len());
            let hi = (max_col + 1).min(chars.len());
            let slice: String = chars[lo..hi].iter().collect();
            lines.push(trim_trailing_blanks(&slice));
        }
        return lines.join("\n");
    }

    let (start_key, start_col, end_key, end_col) = if (anchor_key, sel.anchor_x) <= (cursor_key, cx) {
        (anchor_key, sel.anchor_x as usize, cursor_key, cx as usize)
    } else {
        (cursor_key, cx as usize, anchor_key, sel.anchor_x as usize)
    };

    if start_key == end_key {
        let chars = row_text(start_key);
        let lo = start_col.min(chars.len());
        let hi = (end_col + 1).min(chars.len());
        let slice: String = chars[lo..hi].iter().collect();
        return trim_trailing_blanks(&slice);
    }

    let mut lines = Vec::new();
    {
        let chars = row_text(start_key);
        let lo = start_col.min(chars.len());
        let slice: String = chars[lo..].iter().collect();
        lines.push(trim_trailing_blanks(&slice));
    }
    for key in (start_key + 1)..end_key {
        let chars = row_text(key);
        let full: String = chars.iter().collect();
        lines.push(trim_trailing_blanks(&full));
    }
    {
        let chars = row_text(end_key);
        let hi = (end_col + 1).min(chars.len());
        let slice: String = chars[..hi].iter().collect();
        lines.push(trim_trailing_blanks(&slice));
    }
    lines.join("\n")
}

impl Server {
    // ---- resolution helpers ----

    fn resolve_session_name(&mut self, sess_part: Option<&str>, client_session: Option<&str>) -> Result<String, String> {
        if let Some(s) = sess_part {
            return self.registry.find(s).map(|s| s.name.clone());
        }
        if let Some(name) = client_session {
            return self
                .registry
                .session_mut(name)
                .map(|s| s.name.clone())
                .ok_or_else(|| format!("can't find session: {name}"));
        }
        self.registry.find("").map(|s| s.name.clone())
    }

    fn resolve_window_target(&mut self, client_session: Option<&str>, target: Option<&str>) -> Result<(String, WindowId), String> {
        let (sess_part, win_spec) = match target {
            Some(t) => {
                let (s, r) = split_session_prefix(t);
                (s, if r.is_empty() { None } else { Some(r) })
            }
            None => (None, None),
        };
        // Bare token, no `:` in the original target: tmux tries session name
        // FIRST for target-window. A number keeps today's meaning (window
        // index in the contextual session); a non-numeric token is a session
        // name (via `Registry::find`), yielding that session's CURRENT
        // window -- never a window-name lookup in the contextual session.
        if sess_part.is_none() {
            if let Some(w) = win_spec {
                if !looks_like_index(w) {
                    let session = self.registry.find(w)?;
                    return Ok((session.name.clone(), session.current));
                }
            }
        }
        let session_name = self.resolve_session_name(sess_part, client_session)?;
        let wid = {
            let session = self.registry.session_mut(&session_name).ok_or_else(|| format!("can't find session: {session_name}"))?;
            resolve_window(session, win_spec)?
        };
        Ok((session_name, wid))
    }

    fn resolve_pane_target(&mut self, client_session: Option<&str>, target: Option<&str>) -> Result<(String, WindowId, PaneId), String> {
        let (sess_part, rest) = match target {
            Some(t) => {
                let (s, r) = split_session_prefix(t);
                (s, Some(r))
            }
            None => (None, None),
        };
        let (win_spec, pane_spec, had_dot) = match rest {
            Some(r) => match r.split_once('.') {
                Some((w, p)) => (if w.is_empty() { None } else { Some(w) }, if p.is_empty() { None } else { Some(p) }, true),
                None => (None, if r.is_empty() { None } else { Some(r) }, false),
            },
            None => (None, None, false),
        };
        // Bare token, no `:` or `.` in the original target: same session-name
        // fallback as `resolve_window_target`, but yielding that session's
        // current window's FOCUSED pane. `+`/`-` (relative-to-focus) and
        // numeric indexes keep today's meaning (a pane position in the
        // contextual window); a non-numeric token is a session name.
        if sess_part.is_none() && !had_dot {
            if let Some(p) = pane_spec {
                if p != "+" && p != "-" && p.parse::<usize>().is_err() {
                    let session = self.registry.find(p)?;
                    let name = session.name.clone();
                    let wid = session.current;
                    let window = session.windows.iter().find(|w| w.id == wid).expect("session.current is a live window id");
                    let pid = window.layout.focused();
                    return Ok((name, wid, pid));
                }
            }
        }
        let session_name = self.resolve_session_name(sess_part, client_session)?;
        let (wid, pid) = {
            let session = self.registry.session_mut(&session_name).ok_or_else(|| format!("can't find session: {session_name}"))?;
            let wid = resolve_window(session, win_spec)?;
            let window = session.windows.iter().find(|w| w.id == wid).expect("resolve_window returned a live id");
            let pid = resolve_pane(window, pane_spec)?;
            (wid, pid)
        };
        Ok((session_name, wid, pid))
    }

    /// `(session_name, window_index, window_name, window_flags, pane_index)`
    /// for the acting client's (or most-recent, headlessly) focused pane —
    /// the context `expand_format` needs for `display-message`/
    /// `confirm-before -p`.
    fn format_values(&mut self, client_session: Option<&str>) -> Result<(String, u32, String, String, u32), String> {
        let (session_name, wid, pane_id) = self.resolve_pane_target(client_session, None)?;
        let session = self.registry.session_mut(&session_name).ok_or_else(|| format!("can't find session: {session_name}"))?;
        let window = session.windows.iter().find(|w| w.id == wid).ok_or_else(|| "window not found".to_string())?;
        let window_index = window.index;
        let window_name = window.name.clone();
        let flags = if wid == session.current {
            "*"
        } else if Some(wid) == session.last {
            "-"
        } else {
            ""
        }
        .to_string();
        let pane_index = window.layout.panes().iter().position(|p| *p == pane_id).unwrap_or(0) as u32;
        Ok((session_name, window_index, window_name, flags, pane_index))
    }

    fn expand_with_ctx(&mut self, fmt: &str, client_session: Option<&str>) -> String {
        match self.format_values(client_session) {
            Ok((session, window_index, window_name, window_flags, pane_index)) => {
                let fctx = FormatCtx {
                    session: &session,
                    window_index,
                    window_name: &window_name,
                    window_flags: &window_flags,
                    pane_index,
                    hostname: &self.hostname,
                    now: system_time_parts(),
                };
                crate::options::expand_format(fmt, &fctx)
            }
            Err(_) => fmt.to_string(),
        }
    }

    // ---- kill helpers (shared by kill-pane/kill-window, headless + client) ----

    /// Remove `pane_id` (owned by `session_name`). `Ok(true)` means the
    /// WHOLE session was destroyed (last pane of the last window) —
    /// `destroy_session` has already run and messaged every attached client.
    fn kill_pane_by_id(&mut self, session_name: &str, pane_id: PaneId) -> Result<bool, String> {
        let owner_wid = self.registry.session_mut(session_name).and_then(|s| s.window_by_pane(pane_id).map(|w| w.id));
        let Some(wid) = owner_wid else { return Err("pane not found".to_string()) };
        let removed = self
            .registry
            .session_mut(session_name)
            .and_then(|s| s.windows.iter_mut().find(|w| w.id == wid))
            .map(|w| w.layout.remove(pane_id))
            .unwrap_or(false);
        if removed {
            self.panes.remove(&pane_id);
            self.last_rects.remove(&pane_id);
            self.apply_layout_for_session(session_name);
            return Ok(false);
        }
        let only_window = self.registry.session_mut(session_name).map(|s| s.windows.len() == 1).unwrap_or(false);
        if only_window {
            self.destroy_session(session_name);
            return Ok(true);
        }
        let renumber = self.options.renumber_windows();
        if let Some(s) = self.registry.session_mut(session_name) {
            s.kill_window(wid);
            if renumber {
                s.renumber();
            }
        }
        self.panes.remove(&pane_id);
        self.last_rects.remove(&pane_id);
        self.apply_layout_for_session(session_name);
        Ok(false)
    }

    /// `Ok(true)` means the whole session was destroyed (only window).
    fn kill_window_by_id(&mut self, session_name: &str, wid: WindowId) -> Result<bool, String> {
        let exists = self.registry.session_mut(session_name).map(|s| s.windows.iter().any(|w| w.id == wid)).unwrap_or(false);
        if !exists {
            return Err("window not found".to_string());
        }
        let only_window = self.registry.session_mut(session_name).map(|s| s.windows.len() == 1).unwrap_or(false);
        if only_window {
            self.destroy_session(session_name);
            return Ok(true);
        }
        let pane_ids: Vec<PaneId> = self
            .registry
            .session_mut(session_name)
            .and_then(|s| s.windows.iter().find(|w| w.id == wid).map(|w| w.layout.panes()))
            .unwrap_or_default();
        let renumber = self.options.renumber_windows();
        let killed = self
            .registry
            .session_mut(session_name)
            .map(|s| {
                let k = s.kill_window(wid);
                if k && renumber {
                    s.renumber();
                }
                k
            })
            .unwrap_or(false);
        if killed {
            for pid in pane_ids {
                self.panes.remove(&pid);
                self.last_rects.remove(&pid);
            }
            self.apply_layout_for_session(session_name);
        }
        Ok(false)
    }

    // ---- per-command implementations (shared: take an Option<&str> "acting
    // client's session", not a client object) ----

    fn exec_split_window(&mut self, horizontal: bool, target: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let (session_name, _wid, pane_id) = self.resolve_pane_target(cs, target.as_deref())?;
        let size = self.registry.session_mut(&session_name).map(|s| s.size).ok_or_else(|| format!("can't find session: {session_name}"))?;
        let area = Rect { x: 0, y: self.pane_area_y(), w: size.0, h: size.1 };
        if let Some(session) = self.registry.session_mut(&session_name) {
            session.current_window_mut().layout.focus_pane(pane_id);
        }
        let dir = if horizontal { SplitDir::Horizontal } else { SplitDir::Vertical };
        let new_id = self.mint_pane_id();
        let split_ok = self
            .registry
            .session_mut(&session_name)
            .map(|s| s.current_window_mut().layout.split(dir, new_id, area).is_ok())
            .unwrap_or(false);
        if !split_ok {
            // Split refused (too small etc.): silent no-op, matches the
            // pre-Task-6 `Action::Split` behavior exactly.
            return Ok(String::new());
        }
        let rect = self
            .registry
            .session_mut(&session_name)
            .and_then(|s| s.current_window().layout.rects(area).into_iter().find(|(pid, _)| *pid == new_id))
            .map(|(_, r)| r)
            .unwrap_or(area);
        let shell = self.options.default_command().to_string();
        let history_limit = self.options.history_limit();
        match spawn_pane(new_id, rect.w.max(1), rect.h.max(1), &self.tx, &shell, history_limit) {
            Ok(pr) => {
                self.panes.insert(new_id, pr);
                self.apply_layout_for_session(&session_name);
            }
            Err(_) => {
                if let Some(s) = self.registry.session_mut(&session_name) {
                    s.current_window_mut().layout.remove(new_id);
                }
                self.apply_layout_for_session(&session_name);
            }
        }
        Ok(String::new())
    }

    fn exec_select_pane(&mut self, dir: Option<Direction>, target: Option<String>, cs: Option<&str>) -> Result<String, String> {
        if let Some(d) = dir {
            let session_name = self.resolve_session_name(None, cs)?;
            let area_y = self.pane_area_y();
            if let Some(session) = self.registry.session_mut(&session_name) {
                let size = session.size;
                let area = Rect { x: 0, y: area_y, w: size.0, h: size.1 };
                session.current_window_mut().layout.focus_dir(d, area);
            }
            return Ok(String::new());
        }
        let (session_name, wid, pane_id) = self.resolve_pane_target(cs, target.as_deref())?;
        if let Some(session) = self.registry.session_mut(&session_name) {
            if let Some(w) = session.windows.iter_mut().find(|w| w.id == wid) {
                w.layout.focus_pane(pane_id);
            }
        }
        Ok(String::new())
    }

    fn exec_select_window(&mut self, target: String, cs: Option<&str>) -> Result<String, String> {
        let (session_name, wid) = self.resolve_window_target(cs, Some(&target))?;
        if let Some(session) = self.registry.session_mut(&session_name) {
            if wid != session.current {
                session.last = Some(session.current);
                session.current = wid;
            }
        }
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    fn exec_step_window(&mut self, forward: bool, cs: Option<&str>) -> Result<String, String> {
        let session_name = self.resolve_session_name(None, cs)?;
        if let Some(session) = self.registry.session_mut(&session_name) {
            if forward {
                session.next_window();
            } else {
                session.prev_window();
            }
        }
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    fn exec_last_window(&mut self, cs: Option<&str>) -> Result<String, String> {
        let session_name = self.resolve_session_name(None, cs)?;
        if let Some(session) = self.registry.session_mut(&session_name) {
            session.last_window();
        }
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    fn exec_last_pane(&mut self, cs: Option<&str>) -> Result<String, String> {
        let session_name = self.resolve_session_name(None, cs)?;
        if let Some(session) = self.registry.session_mut(&session_name) {
            session.current_window_mut().layout.focus_last();
        }
        Ok(String::new())
    }

    fn exec_new_window(&mut self, name: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let session_name = self.resolve_session_name(None, cs)?;
        let size = self.registry.session_mut(&session_name).map(|s| s.size).ok_or_else(|| format!("can't find session: {session_name}"))?;
        let pane_id = self.mint_pane_id();
        let shell = self.options.default_command().to_string();
        let history_limit = self.options.history_limit();
        match spawn_pane(pane_id, size.0.max(1), size.1.max(1), &self.tx, &shell, history_limit) {
            Ok(pr) => {
                self.panes.insert(pane_id, pr);
                let wid = self.registry.mint_window_id();
                if let Some(session) = self.registry.session_mut(&session_name) {
                    let w = session.new_window(wid, pane_id);
                    if let Some(n) = name {
                        w.name = n;
                    }
                }
                self.apply_layout_for_session(&session_name);
                Ok(String::new())
            }
            Err(e) => Err(format!("open terminal failed: {e}")),
        }
    }

    fn exec_resize_pane(&mut self, dir: Option<Direction>, zoom: bool, count: i32, session_name: &str) -> Result<String, String> {
        if zoom {
            if let Some(session) = self.registry.session_mut(session_name) {
                session.current_window_mut().layout.toggle_zoom();
            }
            self.apply_layout_for_session(session_name);
            return Ok(String::new());
        }
        if let Some(d) = dir {
            let size = self.registry.session_mut(session_name).map(|s| s.size).ok_or_else(|| format!("can't find session: {session_name}"))?;
            let area = Rect { x: 0, y: self.pane_area_y(), w: size.0, h: size.1 };
            let cells = count.max(1) as u16;
            if let Some(session) = self.registry.session_mut(session_name) {
                session.current_window_mut().layout.resize_focused(d, area, cells);
            }
            self.apply_layout_for_session(session_name);
        }
        Ok(String::new())
    }

    fn exec_rename_window(&mut self, target: Option<String>, name: String, cs: Option<&str>) -> Result<String, String> {
        let (session_name, wid) = self.resolve_window_target(cs, target.as_deref())?;
        crate::model::validate_name(&name, "window")?;
        if let Some(session) = self.registry.session_mut(&session_name) {
            if let Some(w) = session.windows.iter_mut().find(|w| w.id == wid) {
                w.name = name;
            }
        }
        Ok(String::new())
    }

    fn exec_rename_session(&mut self, target: Option<String>, name: String, cs: Option<&str>) -> Result<String, String> {
        let session_name = self.resolve_session_name(target.as_deref(), cs)?;
        crate::model::validate_name(&name, "session")?;
        if name != session_name && self.registry.sessions().iter().any(|s| s.name == name) {
            return Err(format!("duplicate session: {name}"));
        }
        if let Some(session) = self.registry.session_mut(&session_name) {
            session.name = name.clone();
        }
        self.rename_session_everywhere(&session_name, &name);
        Ok(String::new())
    }

    fn exec_send_keys(&mut self, literal: bool, target: Option<String>, keys_arg: Vec<String>, cs: Option<&str>) -> Result<String, String> {
        let (_session, _wid, pane_id) = self.resolve_pane_target(cs, target.as_deref())?;
        let mut bytes = Vec::new();
        if literal {
            bytes.extend(keys_arg.join(" ").into_bytes());
        } else {
            for k in &keys_arg {
                // tmux: an arg that parses as a key name sends its encoded
                // bytes; anything else (including a multi-char arg like
                // "echo hi") is sent as literal text.
                let encoded = crate::keys::parse_key(k).and_then(|key| crate::keys::encode_key(&key));
                bytes.extend(encoded.unwrap_or_else(|| k.as_bytes().to_vec()));
            }
        }
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            if let Some(pty) = pane.pty.as_mut() {
                let _ = pty.write_input(&bytes);
            }
        }
        Ok(String::new())
    }

    fn exec_send_prefix(&mut self, cs: Option<&str>) -> Result<String, String> {
        let (_session, _wid, pane_id) = self.resolve_pane_target(cs, None)?;
        let bytes = crate::keys::encode_key(&self.options.prefix()).unwrap_or_default();
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            if let Some(pty) = pane.pty.as_mut() {
                let _ = pty.write_input(&bytes);
            }
        }
        Ok(String::new())
    }

    // ---- copy mode (Task 2, sub-project 4) ----

    /// `copy-mode [-u] [-e]`: enter copy mode on the acting client's
    /// CURRENTLY FOCUSED pane (the client's `ClientMode::Copy` then binds to
    /// that pane id for the duration — see the type's doc comment).
    /// `page_up` immediately scrolls up one page (the `PPage` binding);
    /// `mouse` is stored on... nothing yet (Task 2 has no selection/mouse
    /// state to carry it on) — accepted and validated but otherwise inert
    /// until the mouse task wires wheel-triggered entry (SP4 §4).
    fn exec_copy_mode(&mut self, page_up: bool, _mouse: bool, client: &mut ClientState, session_name: &str) -> ExecOutcome {
        let pane = match self.registry.session_mut(session_name) {
            Some(s) => s.current_window().layout.focused(),
            None => return ExecOutcome::Err(format!("can't find session: {session_name}")),
        };
        let Some(p) = self.panes.get(&pane) else {
            return ExecOutcome::Err("pane not found".to_string());
        };
        let (live_cx, live_cy) = p.grid.cursor();
        let history_len = p.grid.history_len();
        let rows = p.grid.rows();
        let (scroll, cx, cy) = if page_up {
            (u32::from(rows).min(history_len), live_cx, 0)
        } else {
            (0u32, live_cx, live_cy)
        };
        client.mode = ClientMode::Copy(CopyState { pane, scroll, cx, cy, scroll_exit: false, sel: None });
        ExecOutcome::Ok(String::new())
    }

    /// One internal `copy-*` movement/scroll/cancel command (Task 2 scope).
    /// `Err("not in a mode")` when the acting client isn't currently in copy
    /// mode — the same error a bare `send-keys -X <name>` hits outside copy
    /// mode (its `resolve()` maps straight onto this same `CopyCmd`, see the
    /// `## copy-mode` contract section), matching tmux's own wording.
    fn exec_copy_action(&mut self, action: CopyAction, client: &mut ClientState) -> ExecOutcome {
        let ClientMode::Copy(cs) = &mut client.mode else {
            return ExecOutcome::Err("not in a mode".to_string());
        };
        if action == CopyAction::Cancel {
            client.mode = ClientMode::Normal;
            return ExecOutcome::Ok(String::new());
        }
        let Some(p) = self.panes.get(&cs.pane) else {
            // The pane died between the keystroke's Stdin frame arriving and
            // this dispatch running (the normal case is caught by
            // `cancel_stale_copy_modes` right after, but belt-and-braces:
            // never index a gone pane).
            client.mode = ClientMode::Normal;
            return ExecOutcome::Ok(String::new());
        };
        let cols = p.grid.cols();
        let rows = p.grid.rows();
        let history_len = p.grid.history_len();

        match action {
            CopyAction::Cancel => unreachable!("handled above"),
            CopyAction::CursorLeft => cs.cx = cs.cx.saturating_sub(1),
            CopyAction::CursorRight => cs.cx = (cs.cx + 1).min(cols.saturating_sub(1)),
            CopyAction::CursorUp => {
                if cs.cy > 0 {
                    cs.cy -= 1;
                } else if cs.scroll < history_len {
                    cs.scroll += 1;
                }
            }
            CopyAction::CursorDown => {
                if cs.cy + 1 < rows {
                    cs.cy += 1;
                } else if cs.scroll > 0 {
                    cs.scroll -= 1;
                }
            }
            CopyAction::StartOfLine => cs.cx = 0,
            CopyAction::EndOfLine => cs.cx = cols.saturating_sub(1),
            CopyAction::HistoryTop => {
                cs.scroll = history_len;
                cs.cy = 0;
            }
            CopyAction::HistoryBottom => {
                cs.scroll = 0;
                cs.cy = rows.saturating_sub(1);
            }
            CopyAction::TopLine => cs.cy = 0,
            CopyAction::MiddleLine => cs.cy = rows / 2,
            CopyAction::BottomLine => cs.cy = rows.saturating_sub(1),
            CopyAction::ScrollUp => cs.scroll = (cs.scroll + 1).min(history_len),
            CopyAction::ScrollDown => cs.scroll = cs.scroll.saturating_sub(1),
            CopyAction::HalfpageUp => cs.scroll = (cs.scroll + u32::from(rows / 2).max(1)).min(history_len),
            CopyAction::HalfpageDown => cs.scroll = cs.scroll.saturating_sub(u32::from(rows / 2).max(1)),
            CopyAction::PageUp => cs.scroll = (cs.scroll + u32::from(rows)).min(history_len),
            CopyAction::PageDown => cs.scroll = cs.scroll.saturating_sub(u32::from(rows)),
            CopyAction::NextWord | CopyAction::PreviousWord | CopyAction::NextWordEnd => {
                // v1 word motion (documented simplification, design spec's
                // word-motion note): whitespace-delimited words, no line
                // wrapping — motion clamps at the current view row's edges
                // rather than continuing onto the next/previous line.
                let text = p.grid.view_row_text(cs.scroll, cs.cy);
                let chars: Vec<char> = text.chars().collect();
                let n = chars.len();
                match action {
                    CopyAction::NextWord => {
                        let mut i = (cs.cx as usize).min(n);
                        while i < n && !chars[i].is_whitespace() {
                            i += 1;
                        }
                        while i < n && chars[i].is_whitespace() {
                            i += 1;
                        }
                        cs.cx = i.min(cols.saturating_sub(1) as usize) as u16;
                    }
                    CopyAction::PreviousWord => {
                        let mut i = (cs.cx as usize).min(n);
                        i = i.saturating_sub(1);
                        while i > 0 && chars[i].is_whitespace() {
                            i -= 1;
                        }
                        while i > 0 && !chars[i - 1].is_whitespace() {
                            i -= 1;
                        }
                        cs.cx = i as u16;
                    }
                    CopyAction::NextWordEnd => {
                        let mut i = (cs.cx as usize).min(n);
                        if i < n {
                            i += 1;
                        }
                        while i < n && chars[i].is_whitespace() {
                            i += 1;
                        }
                        while i + 1 < n && !chars[i + 1].is_whitespace() {
                            i += 1;
                        }
                        cs.cx = i.min(n.saturating_sub(1)) as u16;
                    }
                    _ => unreachable!(),
                }
            }
            CopyAction::BeginSelection => {
                cs.sel = Some(SelState { anchor_scroll: cs.scroll, anchor_x: cs.cx, anchor_y: cs.cy, rect: false });
            }
            CopyAction::RectangleToggle => {
                // v1 simplification (documented in the design spec): a
                // no-op with no active selection, rather than tmux's
                // "sticks for the next selection too" behavior.
                if let Some(sel) = &mut cs.sel {
                    sel.rect = !sel.rect;
                }
            }
            CopyAction::OtherEnd => {
                // No-op with no active selection.
                if let Some(sel) = cs.sel {
                    let SelState { anchor_scroll, anchor_x, anchor_y, rect } = sel;
                    cs.sel = Some(SelState { anchor_scroll: cs.scroll, anchor_x: cs.cx, anchor_y: cs.cy, rect });
                    cs.scroll = anchor_scroll;
                    cs.cx = anchor_x;
                    cs.cy = anchor_y;
                }
            }
            CopyAction::ClearSelection => {
                cs.sel = None;
            }
            CopyAction::SelectionAndCancel => {
                let (sel_opt, ccx, ccy, cscroll) = (cs.sel, cs.cx, cs.cy, cs.scroll);
                let text = sel_opt.map(|sel| extract_selection_text(&p.grid, &sel, ccx, ccy, cscroll));
                client.mode = ClientMode::Normal;
                if let Some(t) = text {
                    if !t.is_empty() {
                        let limit = self.options.buffer_limit();
                        self.buffers.add_automatic(t, limit);
                    }
                }
                return ExecOutcome::Ok(String::new());
            }
        }
        ExecOutcome::Ok(String::new())
    }

    // ---- paste buffers (Task 3, sub-project 4) ----

    /// `paste-buffer` (client-aware like `send-keys`: `-t` resolves via
    /// `resolve_pane_target`, falling back to the acting client's focused
    /// pane, or erroring headlessly with no `-t`). Default `no_replace ==
    /// false` replaces every `\n` in the buffer with `\r` before writing —
    /// tmux's own default (`-r` disables it; see the `ParsedCmd::PasteBuffer`
    /// doc comment).
    fn exec_paste_buffer(&mut self, name: Option<String>, target: Option<String>, no_replace: bool, cs: Option<&str>) -> Result<String, String> {
        let (_session, _wid, pane_id) = self.resolve_pane_target(cs, target.as_deref())?;
        let data = match &name {
            Some(n) => self.buffers.get(n).ok_or_else(|| format!("buffer not found: {n}"))?.to_string(),
            None => self.buffers.newest().map(|(_, d)| d.to_string()).ok_or_else(|| "no buffer".to_string())?,
        };
        let bytes = if no_replace { data.into_bytes() } else { data.replace('\n', "\r").into_bytes() };
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            if let Some(pty) = pane.pty.as_mut() {
                let _ = pty.write_input(&bytes);
            }
        }
        Ok(String::new())
    }

    /// Full multi-line `list-buffers` text (CLI/headless path): one
    /// `<name>: <size> bytes: "<sample>"` line per buffer, oldest first.
    fn list_buffers_text(&self) -> String {
        self.buffers
            .list()
            .into_iter()
            .map(|(name, size, sample)| format!("{name}: {size} bytes: \"{sample}\""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn exec_list_buffers_headless(&mut self) -> Result<String, String> {
        let s = self.list_buffers_text();
        Ok(if s.is_empty() { s } else { format!("{s}\n") })
    }

    /// Dispatched from a CLIENT (key binding or the `:` prompt): a
    /// documented simplification of tmux's pager -- the first buffer's line
    /// plus a `(N buffers)` suffix when there's more than one, shown as a
    /// transient status-line message (which can only ever hold one line).
    fn exec_list_buffers_client(&mut self) -> ExecOutcome {
        let list = self.buffers.list();
        if list.is_empty() {
            return ExecOutcome::Ok("no buffers".to_string());
        }
        let (name, size, sample) = &list[0];
        let first_line = format!("{name}: {size} bytes: \"{sample}\"");
        let msg = if list.len() > 1 { format!("{first_line} ({} buffers)", list.len()) } else { first_line };
        ExecOutcome::Ok(msg)
    }

    fn exec_delete_buffer(&mut self, name: Option<String>) -> Result<String, String> {
        match name {
            Some(n) => {
                if self.buffers.delete(&n) {
                    Ok(String::new())
                } else {
                    Err(format!("buffer not found: {n}"))
                }
            }
            None => match self.buffers.delete_newest() {
                Some(_) => Ok(String::new()),
                None => Err("no buffer".to_string()),
            },
        }
    }

    fn exec_set_buffer(&mut self, name: Option<String>, data: String) -> Result<String, String> {
        match name {
            Some(n) => self.buffers.set_named(&n, data),
            None => {
                let limit = self.options.buffer_limit();
                self.buffers.add_automatic(data, limit);
            }
        }
        Ok(String::new())
    }

    fn exec_display_message(&mut self, text: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let fmt = text.unwrap_or_default();
        Ok(self.expand_with_ctx(&fmt, cs))
    }

    fn exec_show_options(&mut self, _global: bool, name: Option<String>) -> Result<String, String> {
        match name {
            Some(n) => match self.options.show(&n) {
                Some(v) => Ok(format!("{n} {v}\n")),
                None => Err(format!("unknown option: {n}")),
            },
            None => {
                let s = self.options.show_all();
                Ok(if s.is_empty() { s } else { format!("{s}\n") })
            }
        }
    }

    fn exec_set_option(&mut self, _global: bool, _window: bool, append: bool, unset: bool, name: String, value: Option<String>) -> Result<String, String> {
        let old_prefix = self.options.prefix();
        self.options.set(&name, value.as_deref(), append, unset)?;
        if name == "prefix" {
            let new_prefix = self.options.prefix();
            if new_prefix != old_prefix {
                self.bindings.unbind(WhichTable::Prefix, &old_prefix);
                self.bindings.bind(
                    WhichTable::Prefix,
                    new_prefix,
                    Binding { cmds: vec![RawCmd { name: "send-prefix".to_string(), args: vec![] }], repeat: false },
                );
                for c in self.clients.values_mut() {
                    c.key_machine.set_prefix(new_prefix);
                }
            }
        } else if name == "repeat-time" {
            let rt = self.options.repeat_time();
            for c in self.clients.values_mut() {
                c.key_machine.set_repeat_time(rt);
            }
        } else if matches!(name.as_str(), "status" | "status-position") {
            // The status row's on/off state and position change every
            // session's pane area (row count for `status`, y origin for
            // `status-position`): recompute the shared size and reapply the
            // layout — resizing ptys/grids — for every session, not just the
            // acting client's (options are global). The post-dispatch
            // re-render then draws the moved/removed bar.
            let names: Vec<String> = self.registry.sessions().iter().map(|s| s.name.clone()).collect();
            for n in names {
                self.recompute_session_size(&n);
                self.apply_layout_for_session(&n);
            }
        }
        Ok(String::new())
    }

    fn exec_bind_key(&mut self, table: String, repeat: bool, key: String, tail: Vec<RawCmd>) -> Result<String, String> {
        let which = match table.as_str() {
            "root" => WhichTable::Root,
            "prefix" => WhichTable::Prefix,
            _ => return Err(format!("unknown key table: {table}")),
        };
        let k = crate::keys::parse_key(&key).ok_or_else(|| format!("unknown key: {key}"))?;
        self.bindings.bind(which, k, Binding { cmds: tail, repeat });
        Ok(String::new())
    }

    fn exec_unbind_key(&mut self, all: bool, table: String, key: Option<String>) -> Result<String, String> {
        let which = match table.as_str() {
            "root" => WhichTable::Root,
            "prefix" => WhichTable::Prefix,
            _ => return Err(format!("unknown key table: {table}")),
        };
        if all {
            self.bindings.unbind_all(which);
            return Ok(String::new());
        }
        let key = key.expect("cmd::resolve guarantees a key unless -a is given");
        let k = crate::keys::parse_key(&key).ok_or_else(|| format!("unknown key: {key}"))?;
        self.bindings.unbind(which, &k);
        Ok(String::new())
    }

    /// Load and dispatch every config candidate in order, joining line
    /// continuations and collecting (not stopping at) every error — tmux
    /// behavior: a bad line, or an unreadable required file, doesn't stop
    /// the REST of that file, or any later file, from being applied. Shared
    /// by runtime `source-file` (one `required: true` candidate, below) and
    /// `run`'s startup config loading (`## config` contract section).
    pub(super) fn load_config_files(&mut self, candidates: &[ConfigCandidate]) -> Vec<String> {
        let mut errors = Vec::new();
        for c in candidates {
            let path = c.path.to_string_lossy().into_owned();
            match std::fs::read_to_string(&c.path) {
                Ok(content) => {
                    let joined = cmd::join_continuations(content.lines());
                    for (lineno, line) in joined {
                        match cmd::parse_line(&line) {
                            Ok(cmds) => {
                                for raw in cmds {
                                    match cmd::resolve(&raw) {
                                        Ok(parsed) => {
                                            if let Err(e) = self.execute_headless(parsed) {
                                                errors.push(format!("{path}:{lineno}: {e}"));
                                            }
                                        }
                                        Err(e) => errors.push(format!("{path}:{lineno}: {e}")),
                                    }
                                }
                            }
                            Err(e) => errors.push(format!("{path}:{lineno}: {e}")),
                        }
                    }
                }
                Err(e) => {
                    // A missing NON-required (default-chain) candidate is
                    // normal (most users have no config at all) and silently
                    // skipped; a required candidate (`-f`/`--config`, or
                    // runtime `source-file`'s single always-required entry)
                    // missing is an error, and so is ANY other open failure
                    // (permissions etc.) even on a non-required candidate.
                    if c.required || e.kind() != std::io::ErrorKind::NotFound {
                        errors.push(format!("{path}: {e}"));
                    }
                }
            }
        }
        errors
    }

    fn execute_source_file_headless(&mut self, path: &str) -> Result<String, String> {
        let candidate = ConfigCandidate { path: std::path::PathBuf::from(path), required: true };
        let errors = self.load_config_files(std::slice::from_ref(&candidate));
        if errors.is_empty() {
            Ok(String::new())
        } else {
            Err(errors.join("\n"))
        }
    }

    // ---- SP2 CLI commands (ported from the deleted `cli_exec.rs`) ----

    fn exec_new_session(&mut self, _detached: bool, name: Option<String>, cols: Option<u16>, rows: Option<u16>) -> Result<String, String> {
        let size = (cols.unwrap_or(80).max(1), rows.unwrap_or(24).max(1));
        let pane_id = self.mint_pane_id();
        let shell = self.options.default_command().to_string();
        let history_limit = self.options.history_limit();
        match spawn_pane(pane_id, size.0, size.1, &self.tx, &shell, history_limit) {
            Ok(pr) => {
                self.panes.insert(pane_id, pr);
                let base_index = self.options.base_index();
                match self.registry.create_session(name.as_deref(), pane_id, size, base_index) {
                    Ok(_) => {
                        self.had_session = true;
                        Ok(String::new())
                    }
                    Err(e) => {
                        self.panes.remove(&pane_id);
                        Err(e)
                    }
                }
            }
            Err(e) => Err(format!("failed to spawn shell: {e}")),
        }
    }

    fn exec_list_sessions(&mut self) -> Result<String, String> {
        if self.registry.is_empty() {
            return Err("no sessions".to_string());
        }
        let mut out = String::new();
        for s in self.registry.sessions() {
            let attached = if self.clients.values().any(|c| c.session.as_deref() == Some(s.name.as_str())) {
                " (attached)"
            } else {
                ""
            };
            out.push_str(&format!("{}: {} windows (created {}){}\n", s.name, s.windows.len(), format_ctime(s.created), attached));
        }
        Ok(out)
    }

    fn exec_has_session(&mut self, target: String) -> Result<String, String> {
        self.registry.find(&target).map(|_| String::new())
    }

    fn exec_kill_session(&mut self, target: Option<String>) -> Result<String, String> {
        let name = match target {
            Some(t) => self.registry.find(&t)?.name.clone(),
            None => self.registry.sessions().last().ok_or_else(|| "no sessions".to_string())?.name.clone(),
        };
        self.destroy_session(&name);
        Ok(String::new())
    }

    fn exec_kill_server(&mut self) -> Result<String, String> {
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
        Ok(String::new())
    }

    fn exec_list_windows(&mut self, target: Option<String>) -> Result<String, String> {
        let name = match target {
            Some(t) => self.registry.find(&t)?.name.clone(),
            None => self.registry.sessions().last().ok_or_else(|| "no sessions".to_string())?.name.clone(),
        };
        let session = self.registry.sessions().iter().find(|s| s.name == name).ok_or_else(|| format!("can't find session: {name}"))?;
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
        Ok(out)
    }

    // ---- kill-pane/kill-window: headless vs. client-aware wrappers ----

    fn exec_kill_pane_headless(&mut self, target: Option<String>) -> Result<String, String> {
        let (session_name, _wid, pane_id) = self.resolve_pane_target(None, target.as_deref())?;
        self.kill_pane_by_id(&session_name, pane_id)?;
        Ok(String::new())
    }

    fn exec_kill_pane_client(&mut self, target: Option<String>, cs: Option<&str>) -> ExecOutcome {
        let (session_name, _wid, pane_id) = match self.resolve_pane_target(cs, target.as_deref()) {
            Ok(v) => v,
            Err(e) => return ExecOutcome::Err(e),
        };
        match self.kill_pane_by_id(&session_name, pane_id) {
            // `Destroy` only when the ACTING client's own session died — a
            // foreign session's destruction (`-t other:...`) has already
            // notified ITS clients via `destroy_session`; the acting client
            // stays attached (Task 6 review fix).
            Ok(true) if cs == Some(session_name.as_str()) => ExecOutcome::Destroy,
            Ok(_) => ExecOutcome::Ok(String::new()),
            Err(e) => ExecOutcome::Err(e),
        }
    }

    fn exec_kill_window_headless(&mut self, target: Option<String>) -> Result<String, String> {
        let (session_name, wid) = self.resolve_window_target(None, target.as_deref())?;
        self.kill_window_by_id(&session_name, wid)?;
        Ok(String::new())
    }

    fn exec_kill_window_client(&mut self, target: Option<String>, cs: Option<&str>) -> ExecOutcome {
        let (session_name, wid) = match self.resolve_window_target(cs, target.as_deref()) {
            Ok(v) => v,
            Err(e) => return ExecOutcome::Err(e),
        };
        match self.kill_window_by_id(&session_name, wid) {
            // Same rule as `exec_kill_pane_client`: `Destroy` only for the
            // acting client's OWN session (Task 6 review fix).
            Ok(true) if cs == Some(session_name.as_str()) => ExecOutcome::Destroy,
            Ok(_) => ExecOutcome::Ok(String::new()),
            Err(e) => ExecOutcome::Err(e),
        }
    }

    fn exec_detach_client_headless(&mut self, target: Option<String>) -> Result<String, String> {
        match target {
            None => Err("usage: detach-client -s target".to_string()),
            Some(t) => {
                let name = self.registry.find(&t)?.name.clone();
                let ids: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.session.as_deref() == Some(name.as_str())).map(|(id, _)| *id).collect();
                for id in ids {
                    if let Some(c) = self.clients.remove(&id) {
                        send_msg(&c.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {name})]") });
                    }
                }
                self.recompute_session_size(&name);
                self.apply_layout_for_session(&name);
                Ok(String::new())
            }
        }
    }

    fn exec_detach_client_client(&mut self, target: Option<String>, session_name: &str) -> ExecOutcome {
        match target {
            None => ExecOutcome::Detach,
            Some(t) => {
                let name = match self.registry.find(&t) {
                    Ok(s) => s.name.clone(),
                    Err(e) => return ExecOutcome::Err(e),
                };
                let ids: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.session.as_deref() == Some(name.as_str())).map(|(id, _)| *id).collect();
                for id in ids {
                    if let Some(c) = self.clients.remove(&id) {
                        send_msg(&c.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {name})]") });
                    }
                }
                self.recompute_session_size(&name);
                self.apply_layout_for_session(&name);
                if session_name == name {
                    ExecOutcome::Detach
                } else {
                    ExecOutcome::Ok(String::new())
                }
            }
        }
    }

    // ---- top-level entry points ----

    /// CLI (`ClientMsg::Cli`) entry point. Preserves the pre-Task-6 exact
    /// `"unknown command"` (no name) error text for an unrecognized/empty
    /// argv[0] — a documented byte-for-byte compatibility shim: every OTHER
    /// error path (usage errors, and any error from resolved commands) now
    /// flows through the unified `cmd`/dispatch machinery unchanged, but
    /// `cmd::resolve`'s own `"unknown command: {name}"` message (used
    /// everywhere else — bindings, the `:` prompt, source-file) is
    /// translated back to the SP2-exact string here ONLY, so the pinned
    /// `cli_unknown_command_err` test keeps passing unmodified.
    pub(super) fn execute_cli_argv(&mut self, argv: &[String]) -> (u8, String, String) {
        let Some(name) = argv.first() else {
            return (1, String::new(), "unknown command".to_string());
        };
        let raw = RawCmd { name: name.clone(), args: argv[1..].to_vec() };
        match cmd::resolve(&raw) {
            Ok(parsed) => match self.execute_headless(parsed) {
                Ok(out) => (0, out, String::new()),
                Err(e) => (1, String::new(), e),
            },
            Err(e) => {
                if e.starts_with("unknown command:") {
                    (1, String::new(), "unknown command".to_string())
                } else {
                    (1, String::new(), e)
                }
            }
        }
    }

    fn execute_headless(&mut self, parsed: ParsedCmd) -> Result<String, String> {
        use ParsedCmd::*;
        match parsed {
            SplitWindow { horizontal, target } => self.exec_split_window(horizontal, target, None),
            SelectPane { dir, target } => self.exec_select_pane(dir, target, None),
            SelectWindow { target } => self.exec_select_window(target, None),
            NextWindow => self.exec_step_window(true, None),
            PreviousWindow => self.exec_step_window(false, None),
            LastWindow => self.exec_last_window(None),
            LastPane => self.exec_last_pane(None),
            NewWindow { name } => self.exec_new_window(name, None),
            KillPane { target } => self.exec_kill_pane_headless(target),
            KillWindow { target } => self.exec_kill_window_headless(target),
            ResizePane { .. } => Err("resize-pane: no client".to_string()),
            RenameWindow { target, name } => self.exec_rename_window(target, name, None),
            RenameSession { target, name } => self.exec_rename_session(target, name, None),
            DetachClient { target } => self.exec_detach_client_headless(target),
            SendKeys { literal, target, keys } => self.exec_send_keys(literal, target, keys, None),
            SendPrefix => self.exec_send_prefix(None),
            SwitchClient { .. } => Err("switch-client: only from a client connection".to_string()),
            DisplayMessage { text } => self.exec_display_message(text, None),
            ConfirmBefore { .. } => Err("confirm-before: only from a client connection".to_string()),
            CommandPrompt { .. } => Err("command-prompt: only from a client connection".to_string()),
            SetOption { global, window, append, unset, name, value } => self.exec_set_option(global, window, append, unset, name, value),
            ShowOptions { global, name } => self.exec_show_options(global, name),
            BindKey { table, repeat, key, tail } => self.exec_bind_key(table, repeat, key, tail),
            UnbindKey { all, table, key } => self.exec_unbind_key(all, table, key),
            ListKeys => Ok(self.bindings.list()),
            SourceFile { path } => self.execute_source_file_headless(&path),
            NewSession { detached, name, cols, rows } => self.exec_new_session(detached, name, cols, rows),
            AttachSession { .. } => Err("attach-session: only from a client connection".to_string()),
            ListSessions => self.exec_list_sessions(),
            ListWindows { target } => self.exec_list_windows(target),
            HasSession { target } => self.exec_has_session(target),
            KillSession { target } => self.exec_kill_session(target),
            KillServer => self.exec_kill_server(),
            CopyMode { .. } => Err("no current client".to_string()),
            CopyCmd(_) => Err("no current client".to_string()),
            PasteBuffer { name, target, no_replace } => self.exec_paste_buffer(name, target, no_replace, None),
            ListBuffers => self.exec_list_buffers_headless(),
            DeleteBuffer { name } => self.exec_delete_buffer(name),
            SetBuffer { name, data } => self.exec_set_buffer(name, data),
        }
    }

    /// Dispatch a `;`-chained (or single) command sequence with an acting
    /// client — a key binding fired, or the `:`/rename prompt committed.
    /// `client`/`session_name` are the caller's LOCALS with `client` already
    /// removed from `self.clients` (the established pre-Task-6 pattern —
    /// see `handle_stdin`), so `&mut self` and `&mut ClientState` never
    /// alias.
    pub(super) fn dispatch_client(&mut self, cmds: &[RawCmd], client: &mut ClientState, session_name: &mut String) -> ExecOutcome {
        let mut acc = String::new();
        for raw in cmds {
            // `,`/`$` bound bare (no args): tmux's real templated
            // command-prompt rename flow isn't implemented in SP3: a
            // NAMELESS rename-window/-session, with a client to show it to,
            // opens the interactive status-line prompt instead of hitting
            // `cmd::resolve`'s usage error (see `Bindings::default`'s doc
            // comment and the `## bindings` contract section).
            if is_bare(raw, &["rename-window", "renamew"]) {
                return self.open_rename_prompt(client, session_name, PromptKind::RenameWindow);
            }
            if is_bare(raw, &["rename-session", "rename"]) {
                return self.open_rename_prompt(client, session_name, PromptKind::RenameSession);
            }
            match cmd::resolve(raw) {
                Ok(parsed) => match self.execute_for_client(parsed, client, session_name) {
                    ExecOutcome::Ok(s) => {
                        if !s.is_empty() {
                            if !acc.is_empty() {
                                acc.push('\n');
                            }
                            acc.push_str(&s);
                        }
                    }
                    other => return other,
                },
                Err(e) => return ExecOutcome::Err(e),
            }
        }
        ExecOutcome::Ok(acc)
    }

    fn open_rename_prompt(&mut self, client: &mut ClientState, session_name: &str, kind: PromptKind) -> ExecOutcome {
        let current = match kind {
            PromptKind::RenameWindow => self.registry.session_mut(session_name).map(|s| s.current_window().name.clone()).unwrap_or_default(),
            PromptKind::RenameSession => session_name.to_string(),
            PromptKind::Command => unreachable!("open_rename_prompt only called for the two rename kinds"),
        };
        let label = match kind {
            PromptKind::RenameWindow => "(rename-window) ",
            PromptKind::RenameSession => "(rename-session) ",
            PromptKind::Command => ":",
        };
        client.mode = ClientMode::Prompt { label: label.to_string(), buf: current, kind };
        client.key_machine.set_capture(true);
        ExecOutcome::Ok(String::new())
    }

    fn execute_for_client(&mut self, parsed: ParsedCmd, client: &mut ClientState, session_name: &mut String) -> ExecOutcome {
        use ParsedCmd::*;
        match parsed {
            SplitWindow { horizontal, target } => wrap(self.exec_split_window(horizontal, target, Some(session_name.as_str()))),
            SelectPane { dir, target } => wrap(self.exec_select_pane(dir, target, Some(session_name.as_str()))),
            SelectWindow { target } => wrap(self.exec_select_window(target, Some(session_name.as_str()))),
            NextWindow => wrap(self.exec_step_window(true, Some(session_name.as_str()))),
            PreviousWindow => wrap(self.exec_step_window(false, Some(session_name.as_str()))),
            LastWindow => wrap(self.exec_last_window(Some(session_name.as_str()))),
            LastPane => wrap(self.exec_last_pane(Some(session_name.as_str()))),
            NewWindow { name } => wrap(self.exec_new_window(name, Some(session_name.as_str()))),
            KillPane { target } => self.exec_kill_pane_client(target, Some(session_name.as_str())),
            KillWindow { target } => self.exec_kill_window_client(target, Some(session_name.as_str())),
            ResizePane { dir, zoom, count } => wrap(self.exec_resize_pane(dir, zoom, count, &session_name.clone())),
            RenameWindow { target, name } => wrap(self.exec_rename_window(target, name, Some(session_name.as_str()))),
            RenameSession { target, name } => {
                // "Renaming self" must be determined from the RESOLVED old
                // name, not from `target.is_none()` — a client renaming its
                // OWN session via an explicit `-t <own-session>` (normal
                // tmux idiom) also needs its session reference re-synced,
                // or `render_all`'s find-by-name silently stops rendering
                // it (Task 6 review fix). Other clients on the session are
                // handled by `rename_session_everywhere` inside
                // `exec_rename_session`; only THIS client, removed from
                // `self.clients` for the duration of dispatch, needs the
                // manual update.
                let old = match self.resolve_session_name(target.as_deref(), Some(session_name.as_str())) {
                    Ok(o) => o,
                    Err(e) => return ExecOutcome::Err(e),
                };
                let renaming_self = old == *session_name;
                match self.exec_rename_session(target, name.clone(), Some(session_name.as_str())) {
                    Ok(s) => {
                        if renaming_self {
                            *session_name = name.clone();
                            client.session = Some(name);
                        }
                        ExecOutcome::Ok(s)
                    }
                    Err(e) => ExecOutcome::Err(e),
                }
            }
            DetachClient { target } => self.exec_detach_client_client(target, session_name),
            SendKeys { literal, target, keys } => wrap(self.exec_send_keys(literal, target, keys, Some(session_name.as_str()))),
            SendPrefix => wrap(self.exec_send_prefix(Some(session_name.as_str()))),
            SwitchClient { next } => match super::switch_client_session(&mut self.registry, client, session_name, next) {
                Some((old, new)) => ExecOutcome::SwitchedSession(old, new),
                None => ExecOutcome::Ok(String::new()),
            },
            DisplayMessage { text } => wrap(self.exec_display_message(text, Some(session_name.as_str()))),
            ConfirmBefore { prompt, tail } => self.exec_confirm_before_client(prompt, tail, client, session_name.as_str()),
            CommandPrompt { initial } => {
                client.mode = ClientMode::Prompt { label: ":".to_string(), buf: initial.unwrap_or_default(), kind: PromptKind::Command };
                client.key_machine.set_capture(true);
                ExecOutcome::Ok(String::new())
            }
            SetOption { global, window, append, unset, name, value } => wrap(self.exec_set_option(global, window, append, unset, name, value)),
            ShowOptions { global, name } => wrap(self.exec_show_options(global, name)),
            BindKey { table, repeat, key, tail } => wrap(self.exec_bind_key(table, repeat, key, tail)),
            UnbindKey { all, table, key } => wrap(self.exec_unbind_key(all, table, key)),
            ListKeys => ExecOutcome::Ok(self.bindings.list()),
            SourceFile { path } => wrap(self.execute_source_file_headless(&path)),
            NewSession { detached, name, cols, rows } => wrap(self.exec_new_session(detached, name, cols, rows)),
            AttachSession { .. } => ExecOutcome::Err("attach-session: only from a client connection".to_string()),
            ListSessions => wrap(self.exec_list_sessions()),
            ListWindows { target } => wrap(self.exec_list_windows(target)),
            HasSession { target } => wrap(self.exec_has_session(target)),
            KillSession { target } => wrap(self.exec_kill_session(target)),
            KillServer => wrap(self.exec_kill_server()),
            CopyMode { page_up, mouse } => self.exec_copy_mode(page_up, mouse, client, session_name),
            CopyCmd(action) => self.exec_copy_action(action, client),
            PasteBuffer { name, target, no_replace } => wrap(self.exec_paste_buffer(name, target, no_replace, Some(session_name.as_str()))),
            ListBuffers => self.exec_list_buffers_client(),
            DeleteBuffer { name } => wrap(self.exec_delete_buffer(name)),
            SetBuffer { name, data } => wrap(self.exec_set_buffer(name, data)),
        }
    }

    fn exec_confirm_before_client(&mut self, prompt: Option<String>, tail: Vec<RawCmd>, client: &mut ClientState, session_name: &str) -> ExecOutcome {
        let pane_snapshot = self.registry.session_mut(session_name).map(|s| s.current_window().layout.focused());
        let window_snapshot = self.registry.session_mut(session_name).map(|s| s.current);
        let fmt = prompt.unwrap_or_default();
        let expanded = self.expand_with_ctx(&fmt, Some(session_name));
        client.mode = ClientMode::ConfirmCmd { prompt: expanded, cmds: tail, pane_snapshot, window_snapshot };
        client.key_machine.set_capture(true);
        ExecOutcome::Ok(String::new())
    }

    // ---- prompt/confirm byte-level editors (status-line capture mode) ----

    /// Route one byte of a `KeyInputEvent::Captured` chunk to whichever
    /// interactive mode the client is in. Returns `(ended, outcome)`:
    /// `ended` mirrors the pre-Task-6 `feed_prompt_byte` contract (the
    /// caller re-feeds any remaining bytes of the chunk through
    /// `KeyMachine` once capture ends); `outcome` is `Some` only when a
    /// command was actually dispatched (confirm-yes, or a `:` commit),
    /// letting the caller route it through [`route_outcome`] exactly like a
    /// key-binding dispatch.
    pub(super) fn feed_mode_byte(&mut self, client: &mut ClientState, session_name: &mut String, b: u8) -> (bool, Option<ExecOutcome>) {
        match client.mode {
            ClientMode::ConfirmCmd { .. } => self.feed_confirm_byte(client, session_name, b),
            ClientMode::Prompt { .. } => self.feed_prompt_byte(client, session_name, b),
            // Copy mode (Task 2) never arms raw capture (`set_capture`) —
            // its keys flow through the normal `KeyInputEvent::Key` path
            // with a table override (see `handle_stdin`), not `Captured`
            // bytes. This arm exists only for match-exhaustiveness.
            ClientMode::Normal | ClientMode::Copy(_) => (true, None),
        }
    }

    fn feed_confirm_byte(&mut self, client: &mut ClientState, session_name: &mut String, b: u8) -> (bool, Option<ExecOutcome>) {
        client.key_machine.set_capture(false);
        let mode = std::mem::replace(&mut client.mode, ClientMode::Normal);
        let ClientMode::ConfirmCmd { cmds, pane_snapshot, window_snapshot, .. } = mode else {
            return (true, None);
        };
        let confirmed = matches!(b, b'y' | b'Y' | b'\r' | b'\n');
        if !confirmed {
            return (true, None);
        }
        // Re-validate staleness (belt-and-braces; `cancel_stale_confirms`
        // handles the common natural-pane/window-death case already, but
        // can't reach THIS client mid-`handle_stdin` — see its doc comment).
        if let Some(p) = pane_snapshot {
            let live = self.registry.sessions().iter().any(|s| s.windows.iter().any(|w| w.layout.panes().contains(&p)));
            if !live {
                return (true, None);
            }
        }
        if let Some(w) = window_snapshot {
            let live = self.registry.sessions().iter().any(|s| s.windows.iter().any(|win| win.id == w));
            if !live {
                return (true, None);
            }
        }
        let outcome = self.dispatch_client(&cmds, client, session_name);
        (true, Some(outcome))
    }

    fn feed_prompt_byte(&mut self, client: &mut ClientState, session_name: &mut String, b: u8) -> (bool, Option<ExecOutcome>) {
        let commit = matches!(b, b'\r' | b'\n');
        let cancel = matches!(b, 0x1b | 0x03 | 0x07);
        if !commit && !cancel {
            if let ClientMode::Prompt { buf, .. } = &mut client.mode {
                match b {
                    0x20..=0x7e => buf.push(b as char),
                    0x7f | 0x08 => {
                        buf.pop();
                    }
                    _ => {}
                }
            }
            return (false, None);
        }

        client.key_machine.set_capture(false);
        let mode = std::mem::replace(&mut client.mode, ClientMode::Normal);
        let ClientMode::Prompt { buf, kind, .. } = mode else {
            return (true, None);
        };
        if cancel {
            return (true, None);
        }
        match kind {
            PromptKind::RenameWindow => {
                match crate::model::validate_name(&buf, "window") {
                    Ok(()) => {
                        if let Some(session) = self.registry.session_mut(session_name) {
                            session.current_window_mut().name = buf;
                        }
                    }
                    Err(e) => client.message = Some((e, Instant::now())),
                }
                (true, None)
            }
            PromptKind::RenameSession => {
                if let Err(e) = crate::model::validate_name(&buf, "session") {
                    client.message = Some((e, Instant::now()));
                } else if buf != *session_name && self.registry.sessions().iter().any(|s| s.name == buf) {
                    client.message = Some((format!("duplicate session: {buf}"), Instant::now()));
                } else {
                    if let Some(session) = self.registry.session_mut(session_name) {
                        session.name = buf.clone();
                    }
                    self.rename_session_everywhere(session_name, &buf);
                    client.session = Some(buf.clone());
                    *session_name = buf;
                }
                (true, None)
            }
            PromptKind::Command => {
                if buf.trim().is_empty() {
                    return (true, None);
                }
                match cmd::parse_line(&buf) {
                    Ok(cmds) => (true, Some(self.dispatch_client(&cmds, client, session_name))),
                    Err(e) => {
                        client.message = Some((e, Instant::now()));
                        (true, None)
                    }
                }
            }
        }
    }
}
