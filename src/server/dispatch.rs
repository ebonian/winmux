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
use crate::keys::{Key, KeyCode, MouseEvent, MouseKind};
use crate::layout::{PaneId, SplitDir};
use crate::model::{Registry, Session, Window, WindowId};
use crate::options::FormatCtx;
use crate::protocol::ServerMsg;

use super::{
    advance_click_run, anchor_key_now, key_to_view, pane_digit_entries, resolve_tree_sel, sel_key, send_msg,
    spawn_pane, system_time_parts, ChooseTreeState, ChooseTreeView, ClientId, ClientMode, ClientState,
    ConfigCandidate, CopyState, DisplayPanesState, MouseDrag, PromptKind, SearchPrompt, SearchState, SelState,
    Server, TreeTarget, MONTHS, MOUSE_WHEEL_STEP,
};
use crate::grid::Grid;
use std::time::Duration;

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

/// `true` if `s` (optionally `=`-prefixed) LOOKS like a window/pane index --
/// i.e. keeps its TODAY meaning (index in the contextual session/window)
/// rather than falling back to session-name resolution for a bare token.
/// Final SP4 review, MUST-FIX #2: this is an all-ASCII-digit shape check,
/// NOT `parse::<u32>().is_ok()` -- an all-digit token that overflows `u32`
/// (e.g. 11 nines) must still be treated as an index attempt so it reaches
/// `resolve_window`'s numeric-miss path (`window not found: <buf>`) rather
/// than falling through to `Registry::find`'s session-name lookup
/// (`can't find session: <buf>`, the wrong wording for what is clearly a
/// numeric-looking token). `resolve_window`'s own `s2.parse::<u32>()` also
/// fails on the same overflowing string, but it degrades gracefully into
/// its name/prefix-match miss branch (`Err(format!("window not found:
/// {s}"))`) rather than panicking, so no change was needed there.
fn looks_like_index(s: &str) -> bool {
    let s = s.strip_prefix('=').unwrap_or(s);
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
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

/// SP6 Task 2: expand a leading `~` (bare, or `~/...`/`~\...`) to
/// `%USERPROFILE%`, mirroring tmux's parse-time tilde expansion
/// (`commands-config-options-formats.md` §2.6) for the one path winmux
/// resolves it for -- a runtime `source-file` argument (`execute_source_file_
/// headless`'s only caller). `~user` (a DIFFERENT user's home directory) is
/// deliberately NOT supported (no passwd database on Windows) and is left
/// untouched, same as any other non-`~`-prefixed path. If `USERPROFILE`
/// isn't set, the path is left untouched too (the subsequent file-open
/// simply fails with its own "not found"-shaped error, same as tmux's own
/// unresolvable-`~` token error).
fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return std::env::var("USERPROFILE").unwrap_or_else(|_| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Ok(home) = std::env::var("USERPROFILE") {
            return format!("{home}\\{rest}");
        }
    }
    path.to_string()
}

/// `find-window` (Task 7, sub-project 4) content-search predicate:
/// case-insensitive substring match against a pane's CURRENTLY VISIBLE
/// screen (not scrollback) -- `grid.rows()`/`grid.cols()` only ever cover
/// the live viewport. `needle` must already be lowercased by the caller
/// (avoids re-lowering it once per pane/row).
fn grid_contains(grid: &Grid, needle: &str) -> bool {
    for row in 0..grid.rows() {
        let mut line = String::with_capacity(grid.cols() as usize);
        for col in 0..grid.cols() {
            line.push(grid.cell(col, row).ch);
        }
        if line.to_lowercase().contains(needle) {
            return true;
        }
    }
    false
}

/// Trim a trailing run of blank (space) characters — tmux copy-mode's "don't
/// carry trailing pad cells into the copied text" rule, applied per extracted
/// line.
fn trim_trailing_blanks(s: &str) -> String {
    s.trim_end_matches(' ').to_string()
}

/// Extract one selection's text from `grid` (Task 3, sub-project 4): the
/// stored anchor is converted to its CURRENT view key via `anchor_key_now`
/// (content-pinned — output captured since the anchor was placed shifts it
/// up in lockstep; Task 3 review fix) and the live cursor
/// (`cx`/`cy`/`scroll`) via `sel_key`, so both endpoints are compared in
/// one coherent frame. Linear (`sel.rect == false`): reading-order between
/// the two endpoints — a single row is a plain substring; multiple rows
/// join the first row's tail, every whole row in between, and the last
/// row's head with `\n` (tmux-style; NOT `\r\n`), each
/// trailing-blank-trimmed. Rectangle (`sel.rect == true`): every spanned
/// row's `[min_col..=max_col]` slice, same per-row trimming, `\n`-joined.
fn extract_selection_text(grid: &Grid, sel: &SelState, cx: u16, cy: u16, scroll: u32) -> String {
    let rows = grid.rows();
    let anchor_key = anchor_key_now(sel, grid.history_len(), grid.history_total());
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

// ---- copy-mode search (Task 4, sub-project 4) ----

/// Case-fold one char to lowercase for copy-mode search, taking only the
/// FIRST char of its full Unicode lowercase mapping instead of the whole
/// (possibly multi-char) expansion `char::to_lowercase()` produces (task-4
/// review, Important finding #2). Some code points lowercase to MORE than
/// one char -- e.g. Turkish `İ` (U+0130) -> `"i\u{307}"` (`i` + combining dot
/// above, two chars) -- and `Grid::view_row_text` emits exactly one char per
/// screen cell, so folding a row with `.chars().flat_map(|c|
/// c.to_lowercase())` can make the folded `Vec<char>` LONGER than the row's
/// true cell count. Every char after such an expansion then sits one (or
/// more) index right of its true column, silently desyncing a match's
/// reported column from the actual screen position. Taking just the first
/// folded char keeps a strict 1:1 char-index<->column correspondence (a
/// simplified fold, not full Unicode case folding) while still matching
/// literal ASCII and the vast majority of real-world case pairs correctly.
fn fold_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Repeat the stored search (`cs.search`) in the SAME direction (`n`,
/// `reverse == false`) or the OPPOSITE direction (`N`, `reverse == true`).
/// `None` (silent no-op) when no search has ever been committed in this
/// copy-mode session. Otherwise delegates to `do_search`, whose return value
/// (`Some(message)` on no match, `None` on a move) is passed straight
/// through.
fn repeat_search(grid: &Grid, cs: &mut CopyState, reverse: bool) -> Option<String> {
    let state = cs.search.clone()?;
    let backward = if reverse { !state.backward } else { state.backward };
    do_search(grid, cs, &state.pattern, backward)
}

/// Perform one literal, case-insensitive, single-row copy-mode search (Task
/// 4): starting from the current cursor position EXCLUSIVE (a repeat must
/// advance past the current match, never re-find it), scans `backward`
/// (toward older content / history top) or forward (toward newer content /
/// live bottom), wrapping across the WHOLE buffer (history top <-> live
/// bottom, like tmux). Always records `(pattern, backward)` as the new
/// repeatable search FIRST -- even a failed search is worth remembering, so
/// `n`/`N` can retry it later (e.g. after more output arrives, or the pane
/// resizes). On a match, moves the cursor to the match's first column/row
/// and scrolls it into view (`key_to_view`) and returns `None`; searching
/// never touches `cs.sel` (an active selection's anchor is untouched -- the
/// cursor move alone extends it, same as any other copy-mode motion). On no
/// match, returns `Some("no match: <pattern>")` for the caller to show as a
/// transient status message -- tmux itself gives no dedicated "not found"
/// feedback for copy-mode search; this is a documented winmux addition (see
/// the task report). Multi-row matches and regex are both out of scope (v1
/// simplification, matching the task brief).
fn do_search(grid: &Grid, cs: &mut CopyState, pattern: &str, backward: bool) -> Option<String> {
    cs.search = Some(SearchState { pattern: pattern.to_string(), backward });
    let rows = grid.rows();
    let cur_key = sel_key(cs.scroll, cs.cy);
    let cur_col = cs.cx as usize;
    let pat: Vec<char> = pattern.chars().map(fold_char).collect();
    match find_search_match(grid, &pat, cur_key, cur_col, backward) {
        Some((key, col)) => {
            let (scroll, cy) = key_to_view(key, rows);
            cs.scroll = scroll;
            cs.cy = cy;
            cs.cx = col;
            None
        }
        None => Some(format!("no match: {pattern}")),
    }
}

/// The core buffer scan behind `do_search`. Visiting order (forward):
/// (1) the rest of the CURRENT row strictly after `cur_col`; (2) every OTHER
/// row in the buffer, nearest first, wrapping past the newest row back to
/// the oldest; (3) as a last resort, the current row's portion strictly
/// before `cur_col` (completing the wrap). Backward mirrors this exactly
/// (nearer/farther swapped, and each row's RIGHTMOST match is preferred over
/// its leftmost — the natural choice when scanning right-to-left). Returns
/// the match's `(key, col)` in the `sel_key`/`key_to_view` coordinate system,
/// or `None` if the pattern appears nowhere in the buffer.
fn find_search_match(grid: &Grid, pat: &[char], cur_key: i64, cur_col: usize, backward: bool) -> Option<(i64, u16)> {
    let rows = grid.rows();
    let history_len = grid.history_len();
    let min_key = -(history_len as i64);
    let max_key = rows as i64 - 1;
    let total = max_key - min_key + 1;
    if total <= 0 || pat.is_empty() {
        return None;
    }

    let row_at = |key: i64| -> Vec<char> {
        let (sb, r) = key_to_view(key, rows);
        grid.view_row_text(sb, r).chars().map(fold_char).collect()
    };
    let wrap = |k: i64| -> i64 { min_key + (k - min_key).rem_euclid(total) };
    let cur_row = row_at(cur_key);

    if backward {
        if let Some(c) = find_last_in(&cur_row, pat, None, Some(cur_col)) {
            return Some((cur_key, c as u16));
        }
        for off in 1..total {
            let key = wrap(cur_key - off);
            let row = row_at(key);
            if let Some(c) = find_last_in(&row, pat, None, None) {
                return Some((key, c as u16));
            }
        }
        if let Some(c) = find_last_in(&cur_row, pat, Some(cur_col + 1), None) {
            return Some((cur_key, c as u16));
        }
    } else {
        if let Some(c) = find_first_in(&cur_row, pat, cur_col + 1, None) {
            return Some((cur_key, c as u16));
        }
        for off in 1..total {
            let key = wrap(cur_key + off);
            let row = row_at(key);
            if let Some(c) = find_first_in(&row, pat, 0, None) {
                return Some((key, c as u16));
            }
        }
        if cur_col > 0 {
            if let Some(c) = find_first_in(&cur_row, pat, 0, Some(cur_col)) {
                return Some((cur_key, c as u16));
            }
        }
    }
    None
}

/// Leftmost match start column `>= from` (and `< to_excl` if given) in one
/// row. `None` if `pat` is empty, longer than `row`, or absent in range.
fn find_first_in(row: &[char], pat: &[char], from: usize, to_excl: Option<usize>) -> Option<usize> {
    if pat.is_empty() || pat.len() > row.len() {
        return None;
    }
    let last_start = row.len() - pat.len();
    let hi = match to_excl {
        Some(t) => t.checked_sub(1)?.min(last_start),
        None => last_start,
    };
    if from > hi {
        return None;
    }
    (from..=hi).find(|&s| &row[s..s + pat.len()] == pat)
}

/// Rightmost match start column `< to_excl` (and `>= from` if given) in one
/// row. `None` if `pat` is empty, longer than `row`, or absent in range --
/// including `to_excl == Some(0)`, which has NO valid start position (there
/// is no column `< 0`) and so must yield an empty range, not "check column 0
/// anyway" (task-4 review, Critical finding #1: the previous `usize`-typed
/// `to_excl` used `saturating_sub(1)`, which silently clamped `0 - 1` back to
/// `0` instead of signaling "empty"). `to_excl: Option<usize>` mirrors
/// `find_first_in`'s `Some(t) => t.checked_sub(1)?` shape so the same class
/// of off-by-one can't recur asymmetrically between the two functions.
fn find_last_in(row: &[char], pat: &[char], from: Option<usize>, to_excl: Option<usize>) -> Option<usize> {
    if pat.is_empty() || pat.len() > row.len() {
        return None;
    }
    let last_start = row.len() - pat.len();
    let hi = match to_excl {
        Some(t) => t.checked_sub(1)?.min(last_start),
        None => last_start,
    };
    let lo = from.unwrap_or(0);
    if lo > hi {
        return None;
    }
    (lo..=hi).rev().find(|&s| &row[s..s + pat.len()] == pat)
}

// ---- shared line-editor byte rules (status-line prompts + copy-mode search) ----

/// Outcome of feeding one raw byte to a captured line editor.
enum LineEdit {
    /// The byte was consumed as a printable-append/backspace edit (or
    /// silently ignored, for anything else); the buffer is still open.
    Editing,
    /// Enter/Ctrl+J: the line should be committed using `buf`'s current
    /// contents.
    Commit,
    /// Esc/Ctrl+C/Ctrl+G: the line should be discarded.
    Cancel,
}

/// Apply the byte-editing rules shared by every "capture raw bytes into a
/// line, then commit/cancel" input in this file -- the status-line prompt
/// (`Server::feed_prompt_byte`) and the copy-mode search prompt
/// (`Server::feed_copy_search_byte`). Printable ASCII (`0x20..=0x7e`)
/// appends to `buf`; Backspace/DEL (`0x7f`/`0x08`) removes the last char;
/// CR/LF commits; Esc/Ctrl+C/Ctrl+G cancels; anything else is ignored.
/// `buf` is mutated only for the `Editing` case -- callers read `buf`
/// themselves once `Commit` is returned. Extracted (task-4 review, Important
/// finding #3) so the two call sites' previously hand-duplicated rules can't
/// drift apart; both are limited to single-byte ASCII printable input
/// (neither handles true multibyte UTF-8 continuation bytes), which this
/// preserves unchanged.
fn edit_line_buf(buf: &mut String, b: u8) -> LineEdit {
    match b {
        b'\r' | b'\n' => LineEdit::Commit,
        0x1b | 0x03 | 0x07 => LineEdit::Cancel,
        0x20..=0x7e => {
            buf.push(b as char);
            LineEdit::Editing
        }
        0x7f | 0x08 => {
            buf.pop();
            LineEdit::Editing
        }
        _ => LineEdit::Editing,
    }
}

// ---- mouse (Task 5, sub-project 4) ----

/// tmux's default `word-separators` option value (` -_@`), hardcoded per the
/// task brief -- no `word-separators` option exists in the registry yet (a
/// documented v1 simplification, alongside the existing `copy-next-word`/
/// `copy-previous-word` motions' own whitespace-only word notion above,
/// which double-click word selection intentionally does NOT reuse: tmux's
/// real double-click uses `word-separators`, not the plain-whitespace rule
/// those cursor motions use).
const WORD_SEPARATORS: &str = " -_@";

fn is_word_sep(c: char) -> bool {
    WORD_SEPARATORS.contains(c)
}

/// Hit-test result for a mouse event's `(x, y)` against a window's current
/// pane rects.
enum MouseHit {
    Pane(PaneId),
    /// A vertical (column) border between two side-by-side panes; `left` is
    /// the pane whose RIGHT edge sits at the border column -- the
    /// `Layout::resize_from` reference leaf for a Left/Right resize.
    VBorder { left: PaneId },
    /// A horizontal (row) border between two stacked panes; `top` is the
    /// pane whose BOTTOM edge sits at the border row -- the
    /// `Layout::resize_from` reference leaf for an Up/Down resize.
    HBorder { top: PaneId },
    None,
}

/// Hit-test `(x, y)` against `rects` (a window's current pane rects, as
/// returned by `Layout::rects`): pane interior first, then a vertical
/// border, then a horizontal border. A cell that is simultaneously a valid
/// vertical- AND horizontal-border position (the single cell at a 4-way "+"
/// junction between four panes) resolves to the vertical border -- an
/// arbitrary but documented tie-break (Task 5 self-review note; real tmux
/// has the same kind of single-cell ambiguity at a "+" junction and doesn't
/// document a preference either). Zero-size rects (a too-small terminal)
/// simply never match any of these conditions, so they degrade to `None`
/// rather than panicking or matching spuriously.
fn hit_test(rects: &[(PaneId, Rect)], x: u16, y: u16) -> MouseHit {
    for (id, r) in rects {
        if x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h {
            return MouseHit::Pane(*id);
        }
    }
    for (id, r) in rects {
        if r.x + r.w == x && y >= r.y && y < r.y + r.h {
            return MouseHit::VBorder { left: *id };
        }
    }
    for (id, r) in rects {
        if r.y + r.h == y && x >= r.x && x < r.x + r.w {
            return MouseHit::HBorder { top: *id };
        }
    }
    MouseHit::None
}

/// Double-click word selection (`DoubleClick1Pane` -> `select-word`): expand
/// from the clicked cell to the maximal run of same-class characters (word
/// chars, or [`WORD_SEPARATORS`] chars) on that view row, using
/// [`WORD_SEPARATORS`] as the separator class -- NOT the plain-whitespace
/// rule `copy-next-word`/`copy-previous-word` use (see [`WORD_SEPARATORS`]'s
/// doc comment). A blank row (`n == 0`) clears any selection instead of
/// panicking on an out-of-range index.
fn select_word_at(cs: &mut CopyState, grid: &Grid, history_total: u64) {
    let text = grid.view_row_text(cs.scroll, cs.cy);
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        cs.sel = None;
        return;
    }
    let ci = (cs.cx as usize).min(n - 1);
    let sep = is_word_sep(chars[ci]);
    let mut start = ci;
    let mut end = ci;
    while start > 0 && is_word_sep(chars[start - 1]) == sep {
        start -= 1;
    }
    while end + 1 < n && is_word_sep(chars[end + 1]) == sep {
        end += 1;
    }
    cs.sel = Some(SelState { anchor_scroll: cs.scroll, anchor_x: start as u16, anchor_y: cs.cy, anchor_total: history_total, rect: false });
    cs.cx = end as u16;
}

/// Triple-click line selection (`TripleClick1Pane` -> `select-line`): the
/// whole clicked view row, column 0 through the last column.
fn select_line_at(cs: &mut CopyState, cols: u16, history_total: u64) {
    cs.sel = Some(SelState { anchor_scroll: cs.scroll, anchor_x: 0, anchor_y: cs.cy, anchor_total: history_total, rect: false });
    cs.cx = cols.saturating_sub(1);
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

    /// `(session_name, window_index, window_name, window_flags, pane_index,
    /// pane_title)` for the acting client's (or most-recent, headlessly)
    /// focused pane — the context `expand_format` needs for
    /// `display-message`/`confirm-before -p`.
    fn format_values(&mut self, client_session: Option<&str>) -> Result<(String, u32, String, String, u32, String), String> {
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
        let pane_title = self.panes.get(&pane_id).map(|p| p.title.clone()).unwrap_or_default();
        Ok((session_name, window_index, window_name, flags, pane_index, pane_title))
    }

    fn expand_with_ctx(&mut self, fmt: &str, client_session: Option<&str>) -> String {
        match self.format_values(client_session) {
            Ok((session, window_index, window_name, window_flags, pane_index, pane_title)) => {
                let fctx = FormatCtx {
                    session: &session,
                    window_index,
                    window_name: &window_name,
                    window_flags: &window_flags,
                    pane_index,
                    hostname: &self.hostname,
                    now: system_time_parts(),
                    pane_title: &pane_title,
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
        // Snapshot whether `pane_id` was this window's FOCUSED pane before
        // removal: `Layout::remove` silently reassigns `focused` internally
        // (to the nearest leaf of the sibling subtree) only in that case --
        // review Finding 1(a): that reassignment was never stamped, so
        // `focus_dir`'s MRU tie-break couldn't tell the handed-off pane was
        // just made active.
        let was_focused = self
            .registry
            .session_mut(session_name)
            .and_then(|s| s.windows.iter().find(|w| w.id == wid))
            .map(|w| w.layout.focused() == pane_id)
            .unwrap_or(false);
        let removed = self
            .registry
            .session_mut(session_name)
            .and_then(|s| s.windows.iter_mut().find(|w| w.id == wid))
            .map(|w| w.layout.remove(pane_id))
            .unwrap_or(false);
        if removed {
            self.panes.remove(&pane_id);
            self.last_rects.remove(&pane_id);
            self.pane_activity.remove(&pane_id); // Finding 2: prune, mirrors last_rects
            if was_focused {
                let new_focus = self
                    .registry
                    .session_mut(session_name)
                    .and_then(|s| s.windows.iter().find(|w| w.id == wid))
                    .map(|w| w.layout.focused());
                if let Some(id) = new_focus {
                    self.stamp_active(id);
                }
            }
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
        self.pane_activity.remove(&pane_id); // Finding 2: prune, mirrors last_rects
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
                self.pane_activity.remove(&pid); // Finding 2: prune, mirrors last_rects
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
                // Newly created panes take focus (tmux default) -- the most
                // recently active pane by construction.
                self.stamp_active(new_id);
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
            let mut newly_focused = None;
            {
                let activity = &self.pane_activity;
                let activity_fn = |id: PaneId| activity.get(&id).copied().unwrap_or(0);
                if let Some(session) = self.registry.session_mut(&session_name) {
                    let size = session.size;
                    let area = Rect { x: 0, y: area_y, w: size.0, h: size.1 };
                    let layout = &mut session.current_window_mut().layout;
                    if layout.focus_dir(d, area, &activity_fn) {
                        newly_focused = Some(layout.focused());
                    }
                }
            }
            if let Some(id) = newly_focused {
                self.stamp_active(id);
            }
            return Ok(String::new());
        }
        let (session_name, wid, pane_id) = self.resolve_pane_target(cs, target.as_deref())?;
        let mut changed = false;
        if let Some(session) = self.registry.session_mut(&session_name) {
            if let Some(w) = session.windows.iter_mut().find(|w| w.id == wid) {
                changed = w.layout.focus_pane(pane_id);
            }
        }
        if changed {
            self.stamp_active(pane_id);
        }
        Ok(String::new())
    }

    // Deliberately does NOT `stamp_active` (nor do `exec_step_window`/
    // `exec_last_window`): switching windows changes the session's current
    // winlink, not any window's active pane -- tmux only bumps
    // `active_point` in `window_set_active_pane`. See `stamp_active`'s doc.
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
        let mut newly_focused = None;
        if let Some(session) = self.registry.session_mut(&session_name) {
            let layout = &mut session.current_window_mut().layout;
            let before = layout.focused();
            layout.focus_last();
            let after = layout.focused();
            if after != before {
                newly_focused = Some(after);
            }
        }
        if let Some(id) = newly_focused {
            self.stamp_active(id);
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
                        // An explicit `-n name` at creation is manual naming
                        // too (Task 9) -- matches `exec_rename_window`.
                        w.auto_rename = false;
                    }
                }
                self.apply_layout_for_session(&session_name);
                // A new window's sole pane starts focused (tmux default).
                self.stamp_active(pane_id);
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
                // automatic-rename (Task 9, sub-project 4): ANY manual
                // rename -- CLI/config `rename-window`, and the `,` prompt
                // commit, which also funnels through this one function --
                // permanently stops this window's name from tracking its
                // active pane's title. See `model::Window::auto_rename`'s
                // doc comment for the "permanently" precedent.
                w.auto_rename = false;
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

    // ---- layout presets, swap-pane, rotate-window (Task 6, sub-project 4) --

    /// Current window's panes in CREATION order (ascending `PaneId`, since
    /// `Server::mint_pane_id` is a global monotonic counter) -- what
    /// `Layout::apply_preset` uses to place panes (position 0 = the "main"
    /// pane for `main-horizontal`/`main-vertical`). Deliberately NOT
    /// `layout.panes()`'s raw tree order: a prior `swap-pane`/`rotate-window`
    /// can have scrambled the tree's leaf order, and the task brief is
    /// explicit that preset placement must stay pinned to creation/index
    /// order regardless (matches tmux, which places panes by its window pane
    /// LIST, not by wherever they currently sit on screen).
    fn panes_in_creation_order(window: &Window) -> Vec<PaneId> {
        let mut ids = window.layout.panes();
        ids.sort_unstable();
        ids
    }

    fn exec_select_layout(&mut self, target: Option<String>, name: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let (session_name, wid) = self.resolve_window_target(cs, target.as_deref())?;
        let size = self.registry.session_mut(&session_name).map(|s| s.size).ok_or_else(|| format!("can't find session: {session_name}"))?;
        let area = Rect { x: 0, y: self.pane_area_y(), w: size.0, h: size.1 };
        let main_w = self.options.main_pane_width();
        let main_h = self.options.main_pane_height();
        let Some(session) = self.registry.session_mut(&session_name) else {
            return Err(format!("can't find session: {session_name}"));
        };
        let Some(window) = session.windows.iter_mut().find(|w| w.id == wid) else {
            return Err("window not found".to_string());
        };
        // Bare `select-layout` (no name): tmux re-flows the CURRENT named
        // layout. SP4 simplification, documented: winmux has no "custom vs.
        // named" tree classification beyond `last_layout`, so a bare
        // `select-layout` re-applies whichever cycle position `last_layout`
        // last recorded (or the first cycle entry if none has ever been
        // applied).
        let preset = match &name {
            Some(n) => crate::layout::LayoutPreset::from_name(n).expect("cmd::resolve already validated the layout name"),
            None => crate::layout::PRESET_CYCLE[window.last_layout.unwrap_or(0) as usize],
        };
        let panes = Self::panes_in_creation_order(window);
        window.layout.apply_preset(preset, &panes, area, main_w, main_h);
        window.last_layout = Some(preset.cycle_index());
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    fn exec_next_layout(&mut self, target: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let (session_name, wid) = self.resolve_window_target(cs, target.as_deref())?;
        let size = self.registry.session_mut(&session_name).map(|s| s.size).ok_or_else(|| format!("can't find session: {session_name}"))?;
        let area = Rect { x: 0, y: self.pane_area_y(), w: size.0, h: size.1 };
        let main_w = self.options.main_pane_width();
        let main_h = self.options.main_pane_height();
        let Some(session) = self.registry.session_mut(&session_name) else {
            return Err(format!("can't find session: {session_name}"));
        };
        let Some(window) = session.windows.iter_mut().find(|w| w.id == wid) else {
            return Err("window not found".to_string());
        };
        let next_idx = match window.last_layout {
            Some(i) => (i + 1) % crate::layout::PRESET_CYCLE.len() as u8,
            None => 0,
        };
        let preset = crate::layout::PRESET_CYCLE[next_idx as usize];
        let panes = Self::panes_in_creation_order(window);
        window.layout.apply_preset(preset, &panes, area, main_w, main_h);
        window.last_layout = Some(next_idx);
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    /// `swap-pane`. `-U`/`-D` (`dir: Some`) swap a target pane with the
    /// previous/next pane in creation order (wrapping), operating only on
    /// that pane's own window (matches the `{`/`}` bindings' intent; tmux
    /// itself scopes `-U`/`-D` to the target window's own pane list too).
    /// The target pane is `-t` if given (SP4 fix round: previously silently
    /// discarded alongside `-U`/`-D` -- see `docs/follow-ups.md`), else the
    /// acting client's active pane (tmux's own default when `-t` is
    /// omitted). A co-supplied `-s` is REJECTED with a usage error rather
    /// than silently ignored: real tmux additionally lets `-s` override the
    /// "which pane is `-U`/`-D` relative to" side of the swap, but winmux
    /// does not implement that fuller matrix yet -- smaller, honest scope
    /// chosen over a silent partial implementation.
    ///
    /// The explicit `-s`/`-t` form (`dir: None`) resolves two independent
    /// pane targets via the normal `resolve_pane_target` fallback chain and
    /// REQUIRES both to resolve to the same window (SP4 fix round: this used
    /// to silently no-op cross-window instead of erroring -- see
    /// `docs/follow-ups.md`). Real tmux supports moving a pane to a
    /// different window this way; winmux does not yet.
    fn exec_swap_pane(&mut self, dir: Option<Direction>, src: Option<String>, dst: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let (session_name, a, b, target_wid) = if let Some(d) = dir {
            if src.is_some() {
                return Err(cmd::usage("swap-pane").expect("swap-pane has a usage string").to_string());
            }
            let (session_name, wid, target) = self.resolve_pane_target(cs, dst.as_deref())?;
            let Some(session) = self.registry.session_mut(&session_name) else {
                return Err(format!("can't find session: {session_name}"));
            };
            let Some(window) = session.windows.iter().find(|w| w.id == wid) else {
                return Err("window not found".to_string());
            };
            let order = Self::panes_in_creation_order(window);
            let Some(pos) = order.iter().position(|&p| p == target) else {
                return Err("pane not found".to_string());
            };
            let n = order.len();
            let other = match d {
                Direction::Up => order[(pos + n - 1) % n],
                Direction::Down => order[(pos + 1) % n],
                // `resolve`'s flag scanner for swap-pane only ever admits
                // `-U`/`-D`; any other `Direction` is unreachable.
                _ => return Err(cmd::usage("swap-pane").expect("swap-pane has a usage string").to_string()),
            };
            (session_name, target, other, wid)
        } else {
            let (s1, w1, pa) = self.resolve_pane_target(cs, src.as_deref())?;
            let (_s2, w2, pb) = self.resolve_pane_target(cs, dst.as_deref())?;
            if w1 != w2 {
                return Err("swap-pane: can only swap panes within the same window".to_string());
            }
            (s1, pa, pb, w1)
        };
        if let Some(session) = self.registry.session_mut(&session_name) {
            if let Some(window) = session.windows.iter_mut().find(|w| w.id == target_wid) {
                window.layout.swap_panes(a, b);
            }
        }
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    fn exec_rotate_window(&mut self, down: bool, target: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let (session_name, wid) = self.resolve_window_target(cs, target.as_deref())?;
        // Finding 1(c): `Layout::rotate` keeps the same LEAF POSITION
        // focused, but a DIFFERENT PaneId now occupies it -- `self.focused`
        // is reassigned to a new pane on every successful rotate, which
        // must be stamped for the same reason as any other focus handoff.
        let mut newly_focused = None;
        if let Some(session) = self.registry.session_mut(&session_name) {
            if let Some(window) = session.windows.iter_mut().find(|w| w.id == wid) {
                if window.layout.rotate(down) {
                    newly_focused = Some(window.layout.focused());
                }
            }
        }
        if let Some(id) = newly_focused {
            self.stamp_active(id);
        }
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    // ---- window ops (Task 7, sub-project 4): break-pane, move-window,
    // find-window ----

    /// `break-pane|breakp [-d] [-n name]`: the resolved CURRENT pane leaves
    /// its window and becomes a new window (next free index, via
    /// `Session::new_window`'s existing `lowest_unused_index` floor). Errors
    /// `"can't break with only one pane"` (verbatim from the task brief,
    /// itself quoting real tmux's own message -- design spec `## 6. Window
    /// ops` doesn't spell out a refusal string) if the source window has
    /// only that one pane -- checked
    /// BEFORE any mutation, regardless of how many OTHER windows the
    /// session has (a window can never be left with zero panes; matches
    /// `Layout::remove`'s own "the only pane" refusal, which
    /// `kill_pane_by_id` relies on the same way). Focus follows the pane
    /// into the new window (`Session::new_window` makes it current) unless
    /// `-d`, which restores the source window as current and only updates
    /// `last`.
    fn exec_break_pane(&mut self, detached: bool, name: Option<String>, cs: Option<&str>) -> Result<String, String> {
        let (session_name, wid, pane_id) = self.resolve_pane_target(cs, None)?;
        let pane_count = self
            .registry
            .session_mut(&session_name)
            .and_then(|s| s.windows.iter().find(|w| w.id == wid))
            .map(|w| w.layout.len())
            .unwrap_or(0);
        if pane_count <= 1 {
            return Err("can't break with only one pane".to_string());
        }
        if let Some(n) = &name {
            crate::model::validate_name(n, "window")?;
        }
        // Finding 1(b), first half: snapshot whether `pane_id` was the
        // source window's FOCUSED pane before removal -- `Layout::remove`
        // only reassigns `focused` internally in that case, same as
        // `kill_pane_by_id` above.
        let was_focused = self
            .registry
            .session_mut(&session_name)
            .and_then(|s| s.windows.iter().find(|w| w.id == wid))
            .map(|w| w.layout.focused() == pane_id)
            .unwrap_or(false);
        let removed = self
            .registry
            .session_mut(&session_name)
            .and_then(|s| s.windows.iter_mut().find(|w| w.id == wid))
            .map(|w| w.layout.remove(pane_id))
            .unwrap_or(false);
        if !removed {
            // Defensive: `pane_count <= 1` above already guarantees
            // `Layout::remove` succeeds (it only ever refuses a
            // single-pane layout); unreachable in practice.
            return Err("can't break with only one pane".to_string());
        }
        if was_focused {
            let new_focus = self
                .registry
                .session_mut(&session_name)
                .and_then(|s| s.windows.iter().find(|w| w.id == wid))
                .map(|w| w.layout.focused());
            if let Some(id) = new_focus {
                self.stamp_active(id);
            }
        }
        let new_wid = self.registry.mint_window_id();
        if let Some(session) = self.registry.session_mut(&session_name) {
            let w = session.new_window(new_wid, pane_id);
            if let Some(n) = name {
                w.name = n;
                // Explicit naming at creation is manual naming too (Task 9)
                // -- matches `exec_rename_window`/`exec_new_window`.
                w.auto_rename = false;
            }
            // `new_window` always makes the new window current -- `-d`
            // (`detached`) means focus should stay in the SOURCE window
            // instead (tmux: the pane still moves, but the client's
            // displayed window doesn't follow it).
            if detached {
                session.current = wid;
                session.last = Some(new_wid);
            }
        }
        // Finding 1(b), second half: the moved pane is always the sole,
        // newly-focused pane of the brand-new window (`new_window`
        // constructs it that way unconditionally, `-d` only changes which
        // window the CLIENT displays, not which pane is focused inside the
        // new window) -- stamp unconditionally, same as
        // `exec_split_window`'s "new pane always takes focus" stamp.
        self.stamp_active(pane_id);
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    /// `move-window|movew [-k] -t index`: re-index the target session's
    /// CURRENT window to `target` (parsed as a bare/`:`-prefixed index --
    /// any `session:` prefix is accepted but IGNORED, since winmux's
    /// `move-window` only re-indexes within the SAME session, per the
    /// design spec). Occupied index -> `index in use: <n>` unless `kill`.
    fn exec_move_window(&mut self, kill: bool, target: String, cs: Option<&str>) -> Result<String, String> {
        let session_name = self.resolve_session_name(None, cs)?;
        let (_, win_spec) = split_session_prefix(&target);
        let idx: u32 = win_spec
            .strip_prefix('=')
            .unwrap_or(win_spec)
            .parse()
            .map_err(|_| cmd::usage("move-window").expect("move-window has a usage string").to_string())?;
        let Some(session) = self.registry.session_mut(&session_name) else {
            return Err(format!("can't find session: {session_name}"));
        };
        let wid = session.current;
        // Snapshot the occupant's panes (if any) BEFORE the move, so a
        // killed occupant's pane runtimes/rects can be cleaned up after --
        // once `Session::move_window` kills it, its `Window`/`Layout` is
        // gone from the registry.
        let occupant_panes: Vec<PaneId> = session
            .windows
            .iter()
            .find(|w| w.index == idx && w.id != wid)
            .map(|w| w.layout.panes())
            .unwrap_or_default();
        if !session.move_window(wid, idx, kill) {
            return Err(format!("index in use: {idx}"));
        }
        if self.options.renumber_windows() {
            if let Some(s) = self.registry.session_mut(&session_name) {
                s.renumber();
            }
        }
        for pid in occupant_panes {
            self.panes.remove(&pid);
            self.last_rects.remove(&pid);
            self.pane_activity.remove(&pid); // Finding 2: prune, mirrors last_rects
        }
        self.apply_layout_for_session(&session_name);
        Ok(String::new())
    }

    /// `find-window|findw <pattern>`: case-insensitive substring search
    /// (v1, no regex) over the target session's window NAMES first, then
    /// every pane's CURRENTLY VISIBLE content, in window-index order (the
    /// current window is a normal candidate, not excluded); jumps to the
    /// FIRST match. No match -> `Ok` carrying a transient `no windows
    /// matching: <p>` message (not an `Err` -- "nothing found" is not a
    /// command failure in tmux).
    fn exec_find_window(&mut self, pattern: String, cs: Option<&str>) -> Result<String, String> {
        let session_name = self.resolve_session_name(None, cs)?;
        let needle = pattern.to_lowercase();
        let snapshot: Vec<(WindowId, String, Vec<PaneId>)> = self
            .registry
            .session_mut(&session_name)
            .map(|s| s.windows.iter().map(|w| (w.id, w.name.clone(), w.layout.panes())).collect())
            .ok_or_else(|| format!("can't find session: {session_name}"))?;
        let mut found: Option<WindowId> = None;
        for (wid, wname, panes) in &snapshot {
            if wname.to_lowercase().contains(&needle) {
                found = Some(*wid);
                break;
            }
            let content_match = panes.iter().any(|pid| self.panes.get(pid).map(|pr| grid_contains(&pr.grid, &needle)).unwrap_or(false));
            if content_match {
                found = Some(*wid);
                break;
            }
        }
        match found {
            Some(wid) => {
                if let Some(session) = self.registry.session_mut(&session_name) {
                    if wid != session.current {
                        session.last = Some(session.current);
                        session.current = wid;
                    }
                }
                self.apply_layout_for_session(&session_name);
                Ok(String::new())
            }
            None => Ok(format!("no windows matching: {pattern}")),
        }
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
    /// `mouse` (SP4 fix round: now actually wired, previously accepted-but-
    /// ignored) becomes `CopyState::scroll_exit` directly — true both for an
    /// explicit `copy-mode -e` and for `mouse_wheel`'s wheel-triggered entry
    /// (which used to set the field by hand right after this call).
    fn exec_copy_mode(&mut self, page_up: bool, mouse: bool, client: &mut ClientState, session_name: &str) -> ExecOutcome {
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
        client.mode = ClientMode::Copy(CopyState { pane, scroll, cx, cy, scroll_exit: mouse, sel: None, search: None, search_prompt: None });
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
                cs.sel = Some(SelState {
                    anchor_scroll: cs.scroll,
                    anchor_x: cs.cx,
                    anchor_y: cs.cy,
                    anchor_total: p.grid.history_total(),
                    rect: false,
                });
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
                // No-op with no active selection. The old anchor's CURRENT
                // view position is recomputed content-pinned (Task 3 review
                // fix, `anchor_key_now`) — new output since the anchor was
                // placed moved its content up, so the cursor must jump to
                // where that content is NOW, not to the stale view row the
                // anchor was originally captured at. The view keeps its
                // current scroll when the anchor is visible under it, and
                // scrolls minimally to reveal it otherwise.
                if let Some(sel) = cs.sel {
                    let key = anchor_key_now(&sel, history_len, p.grid.history_total());
                    let anchor_x = sel.anchor_x;
                    cs.sel = Some(SelState {
                        anchor_scroll: cs.scroll,
                        anchor_x: cs.cx,
                        anchor_y: cs.cy,
                        anchor_total: p.grid.history_total(),
                        rect: sel.rect,
                    });
                    let row_under_current = key + cs.scroll as i64;
                    let (new_scroll, new_cy) = if (0..rows as i64).contains(&row_under_current) {
                        (cs.scroll, row_under_current as u16)
                    } else if key <= 0 {
                        // Above the current view: scroll so it lands on row 0.
                        (((-key) as u32).min(history_len), 0)
                    } else {
                        // Below the live view under scroll 0 -- reachable
                        // after a pane shrink, or after `copy-history-top`
                        // (`g`) then `copy-other-end` (`o`) scrolls far from
                        // the anchor with no shrink involved at all: clamp
                        // to the last live row.
                        (0, (key as u16).min(rows.saturating_sub(1)))
                    };
                    cs.scroll = new_scroll;
                    cs.cy = new_cy;
                    cs.cx = anchor_x.min(cols.saturating_sub(1));
                }
            }
            CopyAction::ClearSelection => {
                cs.sel = None;
            }
            CopyAction::SearchForward => {
                cs.search_prompt = Some(SearchPrompt { backward: false, buf: String::new() });
                client.key_machine.set_capture(true);
            }
            CopyAction::SearchBackward => {
                cs.search_prompt = Some(SearchPrompt { backward: true, buf: String::new() });
                client.key_machine.set_capture(true);
            }
            CopyAction::SearchAgain => {
                if let Some(msg) = repeat_search(&p.grid, cs, false) {
                    return ExecOutcome::Ok(msg);
                }
            }
            CopyAction::SearchReverse => {
                if let Some(msg) = repeat_search(&p.grid, cs, true) {
                    return ExecOutcome::Ok(msg);
                }
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

    // ---- mouse (Task 5, sub-project 4) ----

    /// Route one decoded [`MouseEvent`] for `client` (already resolved to
    /// `session_name`). Dropped entirely (a silent `Ok`) when the `mouse`
    /// option is off (design spec: "mouse events with mouse off are
    /// dropped"), or while `client` has an active confirm/prompt/choose-
    /// tree/display-panes overlay (Task 5 decision, undecided by the brief:
    /// real tmux's mouse-during-prompt behavior is a can of worms out of
    /// scope here -- winmux swallows mouse events in those modes so a stray
    /// click can never race a confirm's y/n capture or act on pane geometry
    /// the overlay is currently hiding; documented deviation, see
    /// `docs/follow-ups.md` #38). `ChooseTree`/`DisplayPanes` (Task 8,
    /// added later) joined this guard in the final SP4 review fix round --
    /// both draw full-screen, so the exact same "hidden pane geometry" risk
    /// applies: a click/drag/wheel would otherwise focus/resize/copy-mode a
    /// pane the user cannot currently see. Dismissal policy mirrors the
    /// keyboard policy documented in `## overlays` of
    /// `docs/specs/2026-07-07-parity-polish-interfaces.md`: mouse events
    /// never dismiss either overlay (unlike display-panes' "any non-digit
    /// KEY dismisses" rule) and never navigate/select a choose-tree row --
    /// they are swallowed outright, same as `ConfirmCmd`/`Prompt`. Real
    /// tmux-style mouse routing into choose-tree (click selects a row,
    /// wheel scrolls the list) is ticketed, `docs/follow-ups.md` #61.
    pub(super) fn dispatch_mouse(&mut self, ev: MouseEvent, client: &mut ClientState, session_name: &str) -> ExecOutcome {
        if !self.options.mouse() {
            return ExecOutcome::Ok(String::new());
        }
        if matches!(
            client.mode,
            ClientMode::ConfirmCmd { .. } | ClientMode::Prompt { .. } | ClientMode::ChooseTree(_) | ClientMode::DisplayPanes(_)
        ) {
            // #64: a drag armed before the overlay opened (keyboard-
            // triggered mid-drag) must not survive across the overlay's
            // lifetime -- clear it just like the sibling "outside pane
            // area"/status-row guards do, so a later out-of-sequence
            // Drag/Up can't revive stale Border/Selecting state.
            if matches!(ev.kind, MouseKind::Drag(_) | MouseKind::Up(_)) {
                client.mouse.drag = MouseDrag::None;
            }
            return ExecOutcome::Ok(String::new());
        }

        if let Some(sy) = self.mouse_status_row(client) {
            if ev.y == sy {
                // A border/selection drag that overshoots onto the status
                // row at release is diverted to `dispatch_mouse_status`,
                // which only handles Down(1)/Wheel -- Drag/Up would
                // otherwise fall through with no reset, leaving
                // `client.mouse.drag` stuck and making the NEXT drag a
                // silent no-op (see `mouse_drag_border`'s `delta == 0`
                // early return). Mirrors the "outside pane area" guard
                // below.
                if matches!(ev.kind, MouseKind::Drag(_) | MouseKind::Up(_)) {
                    client.mouse.drag = MouseDrag::None;
                }
                return self.dispatch_mouse_status(ev, client, session_name);
            }
        }

        let Some((area, rects)) = self.mouse_pane_rects(session_name) else {
            return ExecOutcome::Ok(String::new());
        };
        if ev.x >= area.w || ev.y < area.y || ev.y >= area.y + area.h {
            // Outside the pane area entirely (e.g. a blank gap row on a
            // client taller than the session's shared size): no-op, and end
            // any in-progress drag so it can't keep resizing/selecting based
            // on an off-screen position.
            client.mouse.drag = MouseDrag::None;
            return ExecOutcome::Ok(String::new());
        }

        match ev.kind {
            MouseKind::Down(btn) => self.mouse_down(ev, btn, &rects, client, session_name),
            MouseKind::Drag(_) => self.mouse_drag(ev, &rects, client, session_name),
            MouseKind::Up(_) => self.mouse_up(client),
            MouseKind::WheelUp => self.mouse_wheel(ev, true, &rects, client, session_name),
            MouseKind::WheelDown => self.mouse_wheel(ev, false, &rects, client, session_name),
        }
    }

    /// Status row's y coordinate on THIS client's own screen (mirrors
    /// `render_one`/`render::compose_back`'s `status_y` rule: row 0 if
    /// `status-position top`, else the client's own last row), or `None`
    /// when `status` is off.
    fn mouse_status_row(&self, client: &ClientState) -> Option<u16> {
        if !self.options.status_on() || client.rows == 0 {
            return None;
        }
        Some(if self.options.status_position_top() { 0 } else { client.rows - 1 })
    }

    /// The shared pane area rect and the acting session's CURRENT window's
    /// pane rects within it (mirrors `render_one`'s own area computation, so
    /// hit-testing always agrees with what was actually drawn last).
    /// `None` if `session_name` doesn't currently exist.
    fn mouse_pane_rects(&self, session_name: &str) -> Option<(Rect, Vec<(PaneId, Rect)>)> {
        let session = self.registry.sessions().iter().find(|s| s.name == session_name)?;
        let area = Rect { x: 0, y: self.pane_area_y(), w: session.size.0, h: session.size.1 };
        Some((area, session.current_window().layout.rects(area)))
    }

    fn mouse_focus_pane(&mut self, session_name: &str, pane_id: PaneId) {
        let mut changed = false;
        if let Some(session) = self.registry.session_mut(session_name) {
            changed = session.current_window_mut().layout.focus_pane(pane_id);
        }
        if changed {
            self.stamp_active(pane_id);
        }
    }

    /// `Down1`/`Down2`/`Down3` inside the pane area: a border press arms a
    /// live resize drag; a pane press always focuses that pane (tmux
    /// `select-pane`), and additionally arms a selection drag when it's a
    /// LEFT click landing inside the pane bound to `client`'s OWN copy mode
    /// (clicking on some other pane, or any click while not in copy mode at
    /// all, only focuses -- see the design spec's "Down1 on pane -> focus"
    /// bullet; forwarding the click to the pane's own mouse-reporting
    /// application is out of scope for v1, documented deferral).
    fn mouse_down(
        &mut self,
        ev: MouseEvent,
        btn: u8,
        rects: &[(PaneId, Rect)],
        client: &mut ClientState,
        session_name: &str,
    ) -> ExecOutcome {
        match hit_test(rects, ev.x, ev.y) {
            MouseHit::VBorder { left } => {
                client.mouse.drag = MouseDrag::Border { pane: left, vertical: true };
                ExecOutcome::Ok(String::new())
            }
            MouseHit::HBorder { top } => {
                client.mouse.drag = MouseDrag::Border { pane: top, vertical: false };
                ExecOutcome::Ok(String::new())
            }
            MouseHit::Pane(pane_id) => {
                self.mouse_focus_pane(session_name, pane_id);
                let in_copy_here = matches!(&client.mode, ClientMode::Copy(cs) if cs.pane == pane_id);
                if btn != 1 || !in_copy_here {
                    client.mouse.drag = MouseDrag::None;
                    return ExecOutcome::Ok(String::new());
                }
                let Some(rect) = rects.iter().find(|(id, _)| *id == pane_id).map(|(_, r)| *r) else {
                    return ExecOutcome::Ok(String::new());
                };
                let now = Instant::now();
                let run = advance_click_run(&mut client.mouse, now, ev.x, ev.y, btn);
                let cx = ev.x.saturating_sub(rect.x).min(rect.w.saturating_sub(1));
                let cy = ev.y.saturating_sub(rect.y).min(rect.h.saturating_sub(1));
                let Some(p) = self.panes.get(&pane_id) else {
                    return ExecOutcome::Ok(String::new());
                };
                let history_total = p.grid.history_total();
                let cols = p.grid.cols();
                if let ClientMode::Copy(cs) = &mut client.mode {
                    cs.cx = cx;
                    cs.cy = cy;
                    match run {
                        1 => {
                            cs.sel = Some(SelState {
                                anchor_scroll: cs.scroll,
                                anchor_x: cx,
                                anchor_y: cy,
                                anchor_total: history_total,
                                rect: false,
                            });
                        }
                        2 => select_word_at(cs, &p.grid, history_total),
                        _ => select_line_at(cs, cols, history_total),
                    }
                }
                client.mouse.drag = MouseDrag::Selecting { moved: false };
                ExecOutcome::Ok(String::new())
            }
            MouseHit::None => {
                // A press that misses every pane/border cell (off-by-one
                // vs. a just-moved border, a zero-size rect, ...) must not
                // leave a previously-armed drag stale -- every other arm
                // above overwrites `client.mouse.drag` unconditionally.
                client.mouse.drag = MouseDrag::None;
                ExecOutcome::Ok(String::new())
            }
        }
    }

    /// `Drag1`/`Drag2`/`Drag3` (button-motion tracking): extends whatever
    /// `client.mouse.drag` was armed to on the preceding `Down` (border
    /// resize or copy-mode selection); a no-op if no drag is in progress
    /// (e.g. the button went down outside the pane area, or on a border
    /// while `mouse` was toggled off mid-drag).
    fn mouse_drag(&mut self, ev: MouseEvent, rects: &[(PaneId, Rect)], client: &mut ClientState, session_name: &str) -> ExecOutcome {
        match client.mouse.drag {
            MouseDrag::Border { pane, vertical } => {
                self.mouse_drag_border(ev, pane, vertical, session_name);
                ExecOutcome::Ok(String::new())
            }
            MouseDrag::Selecting { .. } => {
                // An actual `Drag` event happened: mark `moved` so `mouse_up`
                // knows this is a real drag-select, not a plain click.
                client.mouse.drag = MouseDrag::Selecting { moved: true };
                if let ClientMode::Copy(cs) = &mut client.mode {
                    if let Some((_, rect)) = rects.iter().find(|(id, _)| *id == cs.pane) {
                        cs.cx = ev.x.saturating_sub(rect.x).min(rect.w.saturating_sub(1));
                        cs.cy = ev.y.saturating_sub(rect.y).min(rect.h.saturating_sub(1));
                    }
                }
                ExecOutcome::Ok(String::new())
            }
            MouseDrag::None => ExecOutcome::Ok(String::new()),
        }
    }

    /// Live-resize the border `pane` sits on (per `vertical`) so it tracks
    /// the drag position: re-reads the pane's CURRENT rect every call (not
    /// an accumulated delta since the drag started) so this is robust to
    /// clamping at layout minimums -- the border always ends up exactly at
    /// `ev.x`/`ev.y` if that position is reachable at all, rather than
    /// drifting from a stale accumulated offset.
    ///
    /// `pane` (bound once at `Down` by `VBorder{ left }`/`HBorder{ top }`) is
    /// always the FIRST-child-side pane of whichever split owns this border.
    /// `Layout::resize_from` only accepts a first-child reference for
    /// `Direction::Right`/`Down` (see its `want_first` doc comment and
    /// `layout::tests::resize_from_reference_pane_ignores_focus`) --
    /// `Direction::Left`/`Up` need the SECOND-child-side pane instead. Using
    /// `pane` unconditionally made every Left/Up drag (toward the
    /// first-child pane's own edge) a silent no-op (follow-up #66); resolve
    /// the correct reference pane fresh each call, per direction, by finding
    /// `pane`'s current neighbor across this exact border.
    fn mouse_drag_border(&mut self, ev: MouseEvent, pane: PaneId, vertical: bool, session_name: &str) {
        let Some((area, rects)) = self.mouse_pane_rects(session_name) else { return };
        let Some(rect) = rects.iter().find(|(id, _)| *id == pane).map(|(_, r)| *r) else { return };
        let (current, target, positive_dir, negative_dir) = if vertical {
            (rect.x + rect.w, ev.x, Direction::Right, Direction::Left)
        } else {
            (rect.y + rect.h, ev.y, Direction::Down, Direction::Up)
        };
        let delta = target as i32 - current as i32;
        if delta == 0 {
            return;
        }
        let dir = if delta > 0 { positive_dir } else { negative_dir };
        let cells = delta.unsigned_abs() as u16;
        let reference = if matches!(dir, Direction::Left | Direction::Up) {
            // The second-child neighbor starts one cell PAST the border
            // itself (panes are separated by the single border cell, e.g.
            // pane1 `x=0,w=40` / border at col 40 / pane2 `x=41` -- not
            // `x=40`), so its origin is `current + 1`, not `current`.
            rects
                .iter()
                .find(|(id, r)| {
                    *id != pane
                        && if vertical {
                            r.x == current + 1 && r.y < rect.y + rect.h && r.y + r.h > rect.y
                        } else {
                            r.y == current + 1 && r.x < rect.x + rect.w && r.x + r.w > rect.x
                        }
                })
                .map(|(id, _)| *id)
                .unwrap_or(pane)
        } else {
            pane
        };
        if let Some(session) = self.registry.session_mut(session_name) {
            session.current_window_mut().layout.resize_from(reference, dir, area, cells);
        }
        self.apply_layout_for_session(session_name);
    }

    /// `Up1`/`Up2`/`Up3`: ends whatever drag was in progress. A border-resize
    /// drag needs no further action (already applied live). A copy-mode
    /// selection drag that saw at least one `Drag` event copies the
    /// selection and exits copy mode -- tmux's `MouseDragEnd1Pane` default
    /// (`copy-selection-and-cancel`). A PLAIN click (no `Drag` event at all
    /// between this `Up` and the preceding `Down`, i.e. `moved == false`) is
    /// left alone entirely: no copy, no cancel, no selection/buffer touch --
    /// real tmux's copy-mode table has no default binding for a bare
    /// `MouseUp1Pane`, only `MouseDrag1Pane`/`MouseDragEnd1Pane` (both of
    /// which require actual motion). The click's `select-pane`
    /// (`mouse_down`'s unconditional focus) and, inside copy mode, its
    /// zero-width point-selection anchor / cursor reposition still stand --
    /// only the "release" side is a no-op.
    fn mouse_up(&mut self, client: &mut ClientState) -> ExecOutcome {
        let drag = std::mem::replace(&mut client.mouse.drag, MouseDrag::None);
        match drag {
            MouseDrag::Selecting { moved: true } if matches!(client.mode, ClientMode::Copy(_)) => {
                self.exec_copy_action(CopyAction::SelectionAndCancel, client)
            }
            _ => ExecOutcome::Ok(String::new()),
        }
    }

    /// `WheelUp`/`WheelDown` inside the pane area.
    fn mouse_wheel(
        &mut self,
        ev: MouseEvent,
        up: bool,
        rects: &[(PaneId, Rect)],
        client: &mut ClientState,
        session_name: &str,
    ) -> ExecOutcome {
        let Some(pane_id) = rects
            .iter()
            .find(|(_, r)| ev.x >= r.x && ev.x < r.x + r.w && ev.y >= r.y && ev.y < r.y + r.h)
            .map(|(id, _)| *id)
        else {
            return ExecOutcome::Ok(String::new());
        };
        let Some(p) = self.panes.get(&pane_id) else {
            return ExecOutcome::Ok(String::new());
        };
        if p.grid.alt_screen() {
            // tmux's alternate-screen wheel translation: an alt-screen app
            // (`less`, vim, ...) has its own scrollback/paging concept, not
            // winmux's, so each wheel event becomes 3 arrow-key presses sent
            // straight to the pane instead of entering copy mode.
            let arrow: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
            let mut data = Vec::with_capacity(arrow.len() * 3);
            for _ in 0..3 {
                data.extend_from_slice(arrow);
            }
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                if let Some(pty) = pane.pty.as_mut() {
                    let _ = pty.write_input(&data);
                }
            }
            return ExecOutcome::Ok(String::new());
        }

        let in_copy_here = matches!(&client.mode, ClientMode::Copy(cs) if cs.pane == pane_id);
        if in_copy_here {
            let action = if up { CopyAction::ScrollUp } else { CopyAction::ScrollDown };
            for _ in 0..MOUSE_WHEEL_STEP {
                self.exec_copy_action(action, client);
            }
            if !up {
                // tmux's scroll-to-bottom auto-exit: only when THIS copy-mode
                // session was entered by the wheel (`CopyState::scroll_exit`,
                // a Task 2 placeholder whose first consumer is Task 5).
                let should_exit = matches!(&client.mode, ClientMode::Copy(cs) if cs.scroll == 0 && cs.scroll_exit);
                if should_exit {
                    client.mode = ClientMode::Normal;
                }
            }
            return ExecOutcome::Ok(String::new());
        }

        if !up {
            // WheelDown on a live (non-copy-mode) pane: no-op (design spec's
            // documented v1 decision -- there is no "downward" scrollback
            // direction to enter copy mode from at the live bottom).
            return ExecOutcome::Ok(String::new());
        }

        // WheelUp on a live pane: enter copy mode scrolled MOUSE_WHEEL_STEP
        // lines (tmux's WheelUpPane default); `mouse: true` sets
        // `scroll_exit` (via `exec_copy_mode`'s wiring) so scrolling back
        // down to the live bottom by wheel auto-exits.
        let outcome = self.exec_copy_mode(false, true, client, session_name);
        if matches!(outcome, ExecOutcome::Err(_)) {
            return outcome;
        }
        for _ in 0..MOUSE_WHEEL_STEP {
            self.exec_copy_action(CopyAction::ScrollUp, client);
        }
        ExecOutcome::Ok(String::new())
    }

    /// A click or wheel event on the status row (tmux default status-table
    /// bindings: `MouseDown1Status` -> select the clicked window tab;
    /// `WheelUpStatus`/`WheelDownStatus` -> previous-window/next-window).
    fn dispatch_mouse_status(&mut self, ev: MouseEvent, _client: &mut ClientState, session_name: &str) -> ExecOutcome {
        // No client-mode state is needed for status-row routing today; the
        // parameter is kept (unused) so the call site in `dispatch_mouse`
        // stays symmetric with the pane-area dispatch methods.
        match ev.kind {
            MouseKind::Down(1) => self.mouse_status_click(ev.x, session_name),
            MouseKind::WheelUp => wrap(self.exec_step_window(false, Some(session_name))),
            MouseKind::WheelDown => wrap(self.exec_step_window(true, Some(session_name))),
            _ => ExecOutcome::Ok(String::new()),
        }
    }

    /// Left click on the status row at column `x`: select the window tab
    /// under it, if any. A click on the `status-left` prefix, a separator
    /// space, or past the last tab is a no-op (design spec: "Down-click on a
    /// status-line area with no window: no-op"). Rebuilds the SAME left-
    /// prefix-width-then-per-window-span layout `render_one`/`status_spans`
    /// draws (one space between tabs, none after the last) so hit-testing
    /// always agrees with what's actually on screen; deliberately does NOT
    /// replicate `render::compose_back`'s final spatial truncation when
    /// left+right don't fit the terminal width (a click past the truncation
    /// point on an extremely narrow terminal may resolve to a tab that isn't
    /// actually drawn there -- documented v1 gap, `docs/follow-ups.md`).
    fn mouse_status_click(&mut self, x: u16, session_name: &str) -> ExecOutcome {
        let Some(session) = self.registry.sessions().iter().find(|s| s.name == session_name) else {
            return ExecOutcome::Ok(String::new());
        };
        let window = session.current_window();
        let pane_index = window.layout.panes().iter().position(|p| *p == window.layout.focused()).unwrap_or(0) as u32;
        let mut window_flags = String::from("*");
        if window.layout.is_zoomed() {
            window_flags.push('Z');
        }
        let pane_title = self.panes.get(&window.layout.focused()).map(|p| p.title.clone()).unwrap_or_default();
        let fctx = FormatCtx {
            session: &session.name,
            window_index: window.index,
            window_name: &window.name,
            window_flags: &window_flags,
            pane_index,
            hostname: &self.hostname,
            now: system_time_parts(),
            pane_title: &pane_title,
        };
        let left = crate::options::expand_format(self.options.status_left(), &fctx);
        let left_len = left.chars().count().min(self.options.status_left_length() as usize) as u16;
        if x < left_len {
            return ExecOutcome::Ok(String::new());
        }

        let mut cursor = left_len;
        let last_idx = session.windows.len().saturating_sub(1);
        let mut target: Option<WindowId> = None;
        for (i, w) in session.windows.iter().enumerate() {
            let mut flags = String::new();
            if w.id == session.current {
                flags.push('*');
            } else if Some(w.id) == session.last {
                flags.push('-');
            }
            if w.layout.is_zoomed() {
                flags.push('Z');
            }
            let text_len = format!("{}:{}{}", w.index, w.name, flags).chars().count() as u16;
            if x >= cursor && x < cursor + text_len {
                target = Some(w.id);
                break;
            }
            cursor += text_len;
            if i != last_idx {
                cursor += 1; // separator space
            }
        }
        let Some(wid) = target else {
            return ExecOutcome::Ok(String::new());
        };
        if let Some(session) = self.registry.session_mut(session_name) {
            if wid != session.current {
                session.last = Some(session.current);
                session.current = wid;
            }
        }
        self.apply_layout_for_session(session_name);
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
        } else if name == "escape-time" {
            // Task 9: propagate to every attached client's KeyMachine
            // immediately (same pattern as `repeat-time` just above) -- a
            // pending ESC that's ALREADY outstanding is not retroactively
            // re-evaluated (see `KeyMachine::set_escape_time`'s doc
            // comment), only the duration used for the NEXT `escape_ready`
            // check changes.
            let et = self.options.escape_time();
            for c in self.clients.values_mut() {
                c.key_machine.set_escape_time(et);
            }
        } else if name == "mouse" {
            // Task 5: broadcast the SGR mouse-mode enable/disable escape
            // sequences to every CURRENTLY attached client immediately (a
            // raw Output frame, not waiting for the next composed render —
            // `mouse` is a global option so this affects every session, not
            // just the acting client's). A client attaching AFTER this point
            // gets the enable sequence from `finish_attach` instead. The
            // client's own terminal restore path (`host::apply_restore`)
            // unconditionally writes the disable sequence on exit regardless
            // of what the server ever sent, so a crashed/killed server can't
            // leave a client's real terminal with mouse reporting stuck on.
            let seq = if self.options.mouse() { super::MOUSE_ENABLE_SEQ } else { super::MOUSE_DISABLE_SEQ };
            for c in self.clients.values() {
                super::send_output(&c.tx, seq.to_vec());
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
            // SP6 Task 2: `cmd.rs`'s own `-T` validation has accepted these
            // two table names since sub-project 4 (`copy-mode`/
            // `copy-mode-vi` are real bindable tables, see `src/bindings.rs`
            // `WhichTable`), but this dispatch-time match never grew the
            // matching arms -- a parser/executor mismatch, not a missing
            // feature (`.superpowers/sdd/sp6-gap-analysis.md` §A).
            "copy-mode" => WhichTable::CopyMode,
            "copy-mode-vi" => WhichTable::CopyModeVi,
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
            "copy-mode" => WhichTable::CopyMode,
            "copy-mode-vi" => WhichTable::CopyModeVi,
            _ => return Err(format!("unknown key table: {table}")),
        };
        if all {
            self.bindings.unbind_all(which);
            return Ok(String::new());
        }
        let key = key.expect("cmd::resolve guarantees a key unless -a is given");
        // A key token that isn't valid winmux key notation (e.g. tmux's
        // mouse pseudo-keys like `MouseDragEnd1Pane` -- real tmux keys, but
        // winmux's mouse handling is hardcoded dispatch logic, not
        // table-driven via named pseudo-keys, so no such binding could ever
        // exist here to remove) is a silent no-op on UNBIND, not an error:
        // removing something that structurally can never be bound is at
        // least as harmless as removing something merely unbound (doc:
        // "Removing a key that isn't bound is a silent no-op"; an
        // unrecognized key is also one of the things real tmux's `-q`
        // suppresses). `bind-key` (above) still errors on a bad key --
        // CREATING a binding to a garbage key is a real mistake worth
        // reporting; removing a no-op binding is not.
        if let Some(k) = crate::keys::parse_key(&key) {
            self.bindings.unbind(which, &k);
        }
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
        let expanded = expand_tilde(path);
        let candidate = ConfigCandidate { path: std::path::PathBuf::from(expanded), required: true };
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
                        // A new session's sole pane starts focused.
                        self.stamp_active(pane_id);
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
            SelectLayout { target, name } => self.exec_select_layout(target, name, None),
            NextLayout { target } => self.exec_next_layout(target, None),
            SwapPane { dir, src, dst } => self.exec_swap_pane(dir, src, dst, None),
            RotateWindow { down, target } => self.exec_rotate_window(down, target, None),
            BreakPane { detached, name } => self.exec_break_pane(detached, name, None),
            MoveWindow { kill, target } => self.exec_move_window(kill, target, None),
            FindWindow { pattern } => self.exec_find_window(pattern, None),
            ChooseTree { .. } => Err("no current client".to_string()),
            DisplayPanes { .. } => Err("no current client".to_string()),
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
                return self.open_prompt(client, session_name, PromptKind::RenameWindow);
            }
            if is_bare(raw, &["rename-session", "rename"]) {
                return self.open_prompt(client, session_name, PromptKind::RenameSession);
            }
            // `.`/`f`/`'` (Task 7, sub-project 4): same "no-args means open
            // the interactive prompt" rule as the rename bindings above --
            // `.`/`f` bind bare to their REAL tmux command names
            // (move-window/find-window); `'` bares onto `select-window`
            // (there is no distinct "index-window" tmux command -- a bare
            // `select-window`, which would otherwise always be a usage
            // error since `-t` is normally required, is repurposed as the
            // trigger for the `'` binding's interactive index prompt, the
            // same "client-context bare form gets a special meaning" idiom
            // as the two rename commands above).
            if is_bare(raw, &["move-window", "movew"]) {
                return self.open_prompt(client, session_name, PromptKind::MoveWindow);
            }
            if is_bare(raw, &["find-window", "findw"]) {
                return self.open_prompt(client, session_name, PromptKind::FindWindow);
            }
            if is_bare(raw, &["select-window", "selectw"]) {
                return self.open_prompt(client, session_name, PromptKind::Index);
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

    /// Open one of the interactive status-line prompts with a client
    /// context: `,`/`$` (rename, pre-filled with the current name) and, as
    /// of Task 7 (sub-project 4), `.`/`f`/`'` (move-window/find-window/
    /// index, all pre-filled EMPTY -- matching real tmux, which never
    /// pre-fills these three). `PromptKind::Command` (`:`) does NOT go
    /// through here -- it's opened inline by `ParsedCmd::CommandPrompt`'s
    /// arm in `execute_for_client`, since it also handles the `-I initial`
    /// pre-fill text that `command-prompt` itself supports.
    fn open_prompt(&mut self, client: &mut ClientState, session_name: &str, kind: PromptKind) -> ExecOutcome {
        let current = match kind {
            PromptKind::RenameWindow => self.registry.session_mut(session_name).map(|s| s.current_window().name.clone()).unwrap_or_default(),
            PromptKind::RenameSession => session_name.to_string(),
            PromptKind::MoveWindow | PromptKind::FindWindow | PromptKind::Index => String::new(),
            PromptKind::Command => unreachable!("open_prompt is never called for PromptKind::Command"),
        };
        let label = match kind {
            PromptKind::RenameWindow => "(rename-window) ",
            PromptKind::RenameSession => "(rename-session) ",
            PromptKind::MoveWindow => "(move-window) ",
            PromptKind::FindWindow => "(find-window) ",
            // Verbatim per the design spec's `## 6. Window ops` section
            // (label `index`, no parens/trailing space unlike the two
            // above) -- deliberately kept exactly as specified rather than
            // "fixed up" to match the other two's "(name) " convention;
            // see the task report for the divergence-from-tmux note (real
            // tmux's own `'` binding supplies an explicit `-p index`,
            // which tmux itself likely renders as "index: " -- winmux's
            // `command-prompt` doesn't support `-p` labels at all yet, so
            // there's no established rendering convention to match against
            // beyond the spec's literal string).
            PromptKind::Index => "index",
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
            SelectLayout { target, name } => wrap(self.exec_select_layout(target, name, Some(session_name.as_str()))),
            NextLayout { target } => wrap(self.exec_next_layout(target, Some(session_name.as_str()))),
            SwapPane { dir, src, dst } => wrap(self.exec_swap_pane(dir, src, dst, Some(session_name.as_str()))),
            RotateWindow { down, target } => wrap(self.exec_rotate_window(down, target, Some(session_name.as_str()))),
            BreakPane { detached, name } => wrap(self.exec_break_pane(detached, name, Some(session_name.as_str()))),
            MoveWindow { kill, target } => wrap(self.exec_move_window(kill, target, Some(session_name.as_str()))),
            FindWindow { pattern } => wrap(self.exec_find_window(pattern, Some(session_name.as_str()))),
            ChooseTree { sessions } => self.exec_choose_tree_client(sessions, client, session_name.as_str()),
            DisplayPanes { ms } => self.exec_display_panes_client(ms, client),
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
        // Task 4 (search): a copy-mode client with an OPEN search prompt
        // (`CopyState::search_prompt`) arms capture the same way `Prompt`/
        // `ConfirmCmd` do, but deliberately stays in `ClientMode::Copy` (see
        // `SearchPrompt`'s doc comment) -- peek for that case first, since
        // it isn't one of the `client.mode` variants the match below knows
        // about. `cs` isn't read past the guard, so this borrow of
        // `client.mode` ends before the `Captured`/normal-routing path below
        // (which needs `&mut client.mode` again) runs.
        if let ClientMode::Copy(cs) = &client.mode {
            if cs.search_prompt.is_some() {
                return self.feed_copy_search_byte(client, b);
            }
        }
        match client.mode {
            ClientMode::ConfirmCmd { .. } => self.feed_confirm_byte(client, session_name, b),
            ClientMode::Prompt { .. } => self.feed_prompt_byte(client, session_name, b),
            // Copy mode (Task 2) without an open search prompt, and
            // choose-tree/display-panes (Task 8), never arm raw capture
            // (`set_capture`) — their keys flow through the normal
            // `KeyInputEvent::Key`/`Forward` path with a table override (see
            // `handle_stdin`), not `Captured` bytes. This arm exists only
            // for match-exhaustiveness.
            ClientMode::Normal | ClientMode::Copy(_) | ClientMode::ChooseTree(_) | ClientMode::DisplayPanes(_) => (true, None),
        }
    }

    /// Route one byte of a copy-mode search prompt's (Task 4) line edit:
    /// same commit/cancel/printable/backspace rules as `feed_prompt_byte`
    /// (the task brief's "reuse the existing capture machinery" instruction),
    /// via the shared `edit_line_buf` helper — only the STORAGE differs, see
    /// `SearchPrompt`'s doc comment.
    fn feed_copy_search_byte(&mut self, client: &mut ClientState, b: u8) -> (bool, Option<ExecOutcome>) {
        // No live search-prompt buffer to edit (defensive; `feed_mode_byte`
        // only calls this when one exists): fold into a throwaway scratch
        // buffer just to classify the byte, same as `edit_line_buf`'s only
        // other caller does for its own defensive case.
        let mut scratch = String::new();
        let buf = match &mut client.mode {
            ClientMode::Copy(cs) => cs.search_prompt.as_mut().map(|sp| &mut sp.buf).unwrap_or(&mut scratch),
            _ => &mut scratch,
        };
        let edit = edit_line_buf(buf, b);
        if matches!(edit, LineEdit::Editing) {
            return (false, None);
        }

        client.key_machine.set_capture(false);
        // The brief: "handle the client having left copy mode or the pane
        // having died between prompt open and commit (cancel silently)" --
        // `cancel_stale_copy_modes` is the primary mechanism (and now also
        // clears capture, see its Task 4 amendment), but this re-check is
        // belt-and-braces for the same reason `feed_confirm_byte` re-checks
        // its snapshot: this client was already removed from `self.clients`
        // for the duration of `handle_stdin`'s dispatch, unreachable by that
        // sweep until it's reinserted.
        let ClientMode::Copy(cs) = &mut client.mode else {
            return (true, None);
        };
        let Some(sp) = cs.search_prompt.take() else {
            return (true, None);
        };
        if matches!(edit, LineEdit::Cancel) {
            return (true, None);
        }
        let Some(p) = self.panes.get(&cs.pane) else {
            client.mode = ClientMode::Normal;
            return (true, None);
        };
        // Empty commit repeats the previous search (tmux behavior); with no
        // previous search stored, it's a silent no-op.
        let pattern = if sp.buf.is_empty() {
            match &cs.search {
                Some(s) => s.pattern.clone(),
                None => return (true, None),
            }
        } else {
            sp.buf
        };
        match do_search(&p.grid, cs, &pattern, sp.backward) {
            Some(msg) => (true, Some(ExecOutcome::Ok(msg))),
            None => (true, Some(ExecOutcome::Ok(String::new()))),
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
        let mut scratch = String::new();
        let buf = match &mut client.mode {
            ClientMode::Prompt { buf, .. } => buf,
            _ => &mut scratch,
        };
        let edit = edit_line_buf(buf, b);
        if matches!(edit, LineEdit::Editing) {
            return (false, None);
        }

        client.key_machine.set_capture(false);
        let mode = std::mem::replace(&mut client.mode, ClientMode::Normal);
        let ClientMode::Prompt { buf, kind, .. } = mode else {
            return (true, None);
        };
        if matches!(edit, LineEdit::Cancel) {
            return (true, None);
        }
        match kind {
            PromptKind::RenameWindow => {
                // Route through `exec_rename_window` -- the SAME function the
                // CLI/config `rename-window` command calls -- rather than
                // duplicating the rename inline, so this arm can't drift out
                // of sync with that path's semantics again (it previously
                // did: it skipped `auto_rename = false`, so a `,`-renamed
                // window would silently revert on the next OSC title change;
                // fixed by unifying on this one call). `target: None` means
                // "the client's own session's current window", matching this
                // prompt's prior direct-`current_window_mut()` behavior.
                if let Err(e) = self.exec_rename_window(None, buf, Some(session_name.as_str())) {
                    client.message = Some((e, Instant::now()));
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
            // `-k` is never supplied here -- see `PromptKind::MoveWindow`'s
            // doc comment; only the explicit `move-window -k ...` command
            // (via the `:` prompt or CLI) can kill an occupant.
            PromptKind::MoveWindow => {
                if let Err(e) = self.exec_move_window(false, buf, Some(session_name.as_str())) {
                    client.message = Some((e, Instant::now()));
                }
                (true, None)
            }
            PromptKind::FindWindow => {
                match self.exec_find_window(buf, Some(session_name.as_str())) {
                    Ok(msg) => {
                        if !msg.is_empty() {
                            client.message = Some((msg, Instant::now()));
                        }
                    }
                    Err(e) => client.message = Some((e, Instant::now())),
                }
                (true, None)
            }
            // Task-7 review, Important finding #1: `buf` is raw, unfiltered
            // prompt text (see `edit_line_buf`) -- without this check, a
            // non-numeric commit fell through `resolve_window_target`'s
            // bare-token "try session name first" fallback instead of
            // producing the informative miss a numeric-index prompt should
            // always give (silently no-opping against an unrelated session
            // when `buf` happened to match its name, or showing the wrong
            // error -- `can't find session: <buf>` -- otherwise). Empty
            // `buf` is left to `exec_select_window`'s existing "empty spec
            // -> current window" no-op (matches `PromptKind::Command`'s
            // empty-commit-is-silent-cancel precedent).
            PromptKind::Index => {
                if !buf.is_empty() && !buf.bytes().all(|b| b.is_ascii_digit()) {
                    client.message = Some((format!("window not found: {buf}"), Instant::now()));
                } else if let Err(e) = self.exec_select_window(format!(":{buf}"), Some(session_name.as_str())) {
                    client.message = Some((e, Instant::now()));
                }
                (true, None)
            }
        }
    }
}

// ---- overlays: choose-tree + display-panes (Task 8, sub-project 4) --------

/// One choose-tree row: its already-formatted display text and the
/// underlying session/window identity it acts on. Built fresh every time by
/// [`Server::build_tree_rows`] — never cached across a render or a keypress
/// (see `ClientMode::ChooseTree`'s doc comment for why).
pub(super) struct TreeRow {
    pub(super) text: String,
    pub(super) target: TreeTarget,
}

/// Hardcoded choose-tree key resolution (Task 8) — deliberately NOT routed
/// through the mutable `Bindings` table (the design spec's `## 7. Overlays`
/// section calls these out as hardcoded, same footing as the mouse
/// bindings), and NOT `set_capture`-based raw-byte capture either: capture
/// mode's `edit_line_buf` treats a lone `0x1b` as an immediate cancel byte,
/// which would make `Up`/`Down` (`\x1b[A`/`\x1b[B`) unusable — see the
/// `ClientMode::Copy`/copy-mode "table-override key routing" exemplar this
/// mode follows instead (`handle_stdin` intercepts already-DECODED `Key`
/// events). `None` = unbound: swallowed (choose-tree, like copy mode, never
/// leaks a keystroke to the pane underneath), overlay stays open.
enum ChooseTreeAction {
    Up,
    Down,
    Commit,
    Cancel,
    Kill,
}

fn resolve_choose_tree_key(key: &Key) -> Option<ChooseTreeAction> {
    if key.ctrl && matches!(key.code, KeyCode::Char('c')) {
        return Some(ChooseTreeAction::Cancel);
    }
    if key.ctrl || key.meta {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(ChooseTreeAction::Up),
        KeyCode::Down => Some(ChooseTreeAction::Down),
        KeyCode::Char('k') => Some(ChooseTreeAction::Up),
        KeyCode::Char('j') => Some(ChooseTreeAction::Down),
        KeyCode::Enter => Some(ChooseTreeAction::Commit),
        KeyCode::Char('q') => Some(ChooseTreeAction::Cancel),
        KeyCode::Escape => Some(ChooseTreeAction::Cancel),
        KeyCode::Char('x') => Some(ChooseTreeAction::Kill),
        _ => None,
    }
}

impl Server {
    /// Build choose-tree's row list fresh from LIVE registry state (Task 8)
    /// — the single source of truth both `render_one`'s overlay and every
    /// key that resolves `sel` to a concrete target go through, which is
    /// what makes stale-row bugs structurally unreachable (see
    /// `ClientMode::ChooseTree`'s doc comment).
    ///
    /// `Sessions`: one row per session, `<name>: N windows[ (attached)]`.
    /// `Windows`: the CURRENT session only — a header row in the same format
    /// as a `Sessions` row, followed by one indented row per window,
    /// `  <index>: <name><flags>` (`*` current, `-` last, else nothing) —
    /// see the design spec's `## 7. Overlays` section for the exact format
    /// and the documented "current session's windows only" scope
    /// simplification (real tmux's `-w` shows the whole tree).
    pub(super) fn build_tree_rows(&self, session_name: &str, view: ChooseTreeView) -> Vec<TreeRow> {
        let is_attached = |name: &str| self.clients.values().any(|c| c.session.as_deref() == Some(name));
        let session_row = |s: &Session| TreeRow {
            text: format!("{}: {} windows{}", s.name, s.windows.len(), if is_attached(&s.name) { " (attached)" } else { "" }),
            target: TreeTarget::Session(s.name.clone()),
        };
        match view {
            ChooseTreeView::Sessions => self.registry.sessions().iter().map(session_row).collect(),
            ChooseTreeView::Windows => {
                let Some(session) = self.registry.sessions().iter().find(|s| s.name == session_name) else {
                    return Vec::new();
                };
                let mut rows = vec![session_row(session)];
                for w in &session.windows {
                    let flag = if w.id == session.current {
                        "*"
                    } else if Some(w.id) == session.last {
                        "-"
                    } else {
                        ""
                    };
                    rows.push(TreeRow {
                        text: format!("  {}: {}{}", w.index, w.name, flag),
                        target: TreeTarget::Window(session.name.clone(), w.id),
                    });
                }
                rows
            }
        }
    }

    /// `kill-session <name>? (y/n)` / `kill-window <name>? (y/n)` for `x`
    /// (Task 8) — same prompt-string shape as the `&`/`x` prefix bindings'
    /// `confirm-before -p "kill-window #W? (y/n)" kill-window`, computed
    /// directly here instead since choose-tree's kill flow doesn't route
    /// through `ClientMode::ConfirmCmd` (see `ChooseTreeState::pending_kill`'s
    /// doc comment for why).
    fn tree_kill_prompt(&self, target: &TreeTarget) -> String {
        match target {
            TreeTarget::Session(name) => format!("kill-session {name}? (y/n)"),
            TreeTarget::Window(session_name, wid) => {
                let name = self
                    .registry
                    .sessions()
                    .iter()
                    .find(|s| s.name == *session_name)
                    .and_then(|s| s.windows.iter().find(|w| w.id == *wid))
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                format!("kill-window {name}? (y/n)")
            }
        }
    }

    /// Execute a confirmed choose-tree kill (Task 8): re-validates the
    /// target still exists (belt-and-braces — `cancel_stale_choose_trees`
    /// already clears a stale `pending_kill` before this can even be
    /// reached, same defense-in-depth as `feed_confirm_byte`'s own
    /// re-check), then reuses the SAME kill helpers `&`/`x` and `kill-
    /// session`/`kill-window` already go through. `Destroy` only when the
    /// killed session IS the acting client's own (same rule as
    /// `exec_kill_window_client`/`exec_kill_pane_client`) — the overlay
    /// simply closes along with the rest of that client's exit, matching a
    /// normal kill-your-own-session flow.
    fn exec_tree_kill(&mut self, target: TreeTarget, session_name: &str) -> ExecOutcome {
        match target {
            TreeTarget::Session(name) => {
                if self.registry.session_mut(&name).is_none() {
                    return ExecOutcome::Ok(String::new());
                }
                let acting = name == session_name;
                self.destroy_session(&name);
                if acting {
                    ExecOutcome::Destroy
                } else {
                    ExecOutcome::Ok(String::new())
                }
            }
            TreeTarget::Window(sname, wid) => {
                let exists = self.registry.sessions().iter().any(|s| s.name == sname && s.windows.iter().any(|w| w.id == wid));
                if !exists {
                    return ExecOutcome::Ok(String::new());
                }
                match self.kill_window_by_id(&sname, wid) {
                    Ok(true) if sname == session_name => ExecOutcome::Destroy,
                    Ok(_) => ExecOutcome::Ok(String::new()),
                    Err(e) => ExecOutcome::Err(e),
                }
            }
        }
    }

    /// Commit choose-tree's selection (Task 8, Enter): re-validates the
    /// target still exists (a stale row, e.g. killed by another client while
    /// this one was browsing, is a silent no-op rather than acting on a dead
    /// id) then switches this client to the session, or selects the window
    /// within its (always-current, per the `Windows` view's own scope) session
    /// — same underlying mutation as `switch-client -p/-n`/`select-window`.
    fn exec_tree_commit(&mut self, target: TreeTarget, client: &mut ClientState, session_name: &mut String) -> ExecOutcome {
        match target {
            TreeTarget::Session(name) => {
                if self.registry.session_mut(&name).is_none() || name == *session_name {
                    return ExecOutcome::Ok(String::new());
                }
                let old = std::mem::replace(session_name, name.clone());
                client.session = Some(name.clone());
                client.renderer.resize(client.cols.max(1), client.rows.max(1));
                ExecOutcome::SwitchedSession(old, name)
            }
            TreeTarget::Window(sname, wid) => {
                let exists = self.registry.sessions().iter().any(|s| s.name == sname && s.windows.iter().any(|w| w.id == wid));
                if !exists {
                    return ExecOutcome::Ok(String::new());
                }
                if let Some(session) = self.registry.session_mut(&sname) {
                    if wid != session.current {
                        session.last = Some(session.current);
                        session.current = wid;
                    }
                }
                self.apply_layout_for_session(&sname);
                ExecOutcome::Ok(String::new())
            }
        }
    }

    /// Route one decoded key to the acting client's choose-tree overlay
    /// (Task 8). `None` = the key was swallowed (unbound, or a navigation
    /// key while `pending_kill` absorbed it) with NO dispatch to report;
    /// `handle_stdin` only calls `route_outcome` on `Some`.
    pub(super) fn dispatch_choose_tree_key(&mut self, key: &Key, client: &mut ClientState, session_name: &mut String) -> Option<ExecOutcome> {
        // A pending kill-confirm (`x` was already pressed) absorbs the VERY
        // NEXT key as its y/n answer, taking priority over ordinary
        // navigation -- same y/Y/Enter-confirms, anything-else-cancels rule
        // as `feed_confirm_byte`.
        let pending = match &mut client.mode {
            ClientMode::ChooseTree(state) => state.pending_kill.take(),
            _ => return None,
        };
        if let Some((target, _prompt)) = pending {
            let confirmed = matches!(key.code, KeyCode::Enter) || matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
            return Some(if confirmed { self.exec_tree_kill(target, session_name) } else { ExecOutcome::Ok(String::new()) });
        }

        let action = resolve_choose_tree_key(key)?;
        let view = match &client.mode {
            ClientMode::ChooseTree(state) => state.view,
            _ => return None,
        };
        let rows = self.build_tree_rows(session_name, view);

        // Task 8 review fix, Critical #1: re-resolve the STORED SELECTION
        // IDENTITY against this freshly rebuilt `rows`, rather than trusting
        // `state.sel` as a raw array position. If a kill (another client, or
        // a pane exiting naturally) and this keypress land in the same
        // coalesced event batch, the row list can shift underneath a stale
        // index without an intervening render ever showing the user the
        // shift — re-resolving by identity is what makes Commit/Kill act on
        // what the user actually selected. See `resolve_tree_sel`'s doc
        // comment for the full mechanics.
        let sel = match &client.mode {
            ClientMode::ChooseTree(state) => resolve_tree_sel(&rows, &state.selected, state.sel),
            _ => return None,
        };

        match action {
            ChooseTreeAction::Up => {
                let new_sel = sel.saturating_sub(1);
                if let ClientMode::ChooseTree(state) = &mut client.mode {
                    state.sel = new_sel;
                    state.selected = rows.get(new_sel).map(|r| r.target.clone());
                }
                Some(ExecOutcome::Ok(String::new()))
            }
            ChooseTreeAction::Down => {
                let new_sel = (sel + 1).min(rows.len().saturating_sub(1));
                if let ClientMode::ChooseTree(state) = &mut client.mode {
                    state.sel = new_sel;
                    state.selected = rows.get(new_sel).map(|r| r.target.clone());
                }
                Some(ExecOutcome::Ok(String::new()))
            }
            ChooseTreeAction::Cancel => {
                client.mode = ClientMode::Normal;
                Some(ExecOutcome::Ok(String::new()))
            }
            ChooseTreeAction::Kill => {
                let Some(row) = rows.get(sel) else { return Some(ExecOutcome::Ok(String::new())) };
                let prompt = self.tree_kill_prompt(&row.target);
                if let ClientMode::ChooseTree(state) = &mut client.mode {
                    state.pending_kill = Some((row.target.clone(), prompt));
                }
                Some(ExecOutcome::Ok(String::new()))
            }
            ChooseTreeAction::Commit => {
                let Some(row) = rows.get(sel) else { return Some(ExecOutcome::Ok(String::new())) };
                let target = row.target.clone();
                client.mode = ClientMode::Normal;
                Some(self.exec_tree_commit(target, client, session_name))
            }
        }
    }

    /// Route one decoded key to the acting client's display-panes overlay
    /// (Task 8): a digit `0`-`9` focuses the matching pane (per the SAME
    /// digit-to-pane mapping the overlay was drawn with, [`pane_digit_entries`]
    /// recomputed fresh here rather than trusting anything stored); any
    /// other key just dismisses. Either way, exactly one key ever reaches
    /// this — the overlay closes unconditionally.
    pub(super) fn dispatch_display_panes_key(&mut self, key: &Key, client: &mut ClientState, session_name: &str) -> ExecOutcome {
        if !matches!(client.mode, ClientMode::DisplayPanes(_)) {
            return ExecOutcome::Ok(String::new());
        }
        client.mode = ClientMode::Normal;
        if let KeyCode::Char(c) = key.code {
            if !key.ctrl && !key.meta {
                if let Some(d) = c.to_digit(10) {
                    if let Some(session) = self.registry.sessions().iter().find(|s| s.name == session_name) {
                        let entries = pane_digit_entries(session.current_window());
                        if let Some((pane_id, _)) = entries.into_iter().find(|(_, dg)| *dg == d) {
                            self.mouse_focus_pane(session_name, pane_id);
                        }
                    }
                }
            }
        }
        ExecOutcome::Ok(String::new())
    }

    /// `choose-tree [-s|-w]` (Task 8): opens the overlay, replacing whatever
    /// mode the client was previously in (matches copy mode's own entry
    /// behavior — no special-casing for "already in copy mode"/"prompt open"
    /// etc., since a prompt/confirm keeps capture armed and therefore can
    /// never actually dispatch this command in the first place).
    fn exec_choose_tree_client(&mut self, sessions: bool, client: &mut ClientState, session_name: &str) -> ExecOutcome {
        let view = if sessions { ChooseTreeView::Sessions } else { ChooseTreeView::Windows };
        // Task 8 review fix, Critical #1: seed `selected` with row 0's
        // identity (not just index 0) so the very first Up/Down/Commit/Kill
        // has an identity to re-resolve, same as every subsequent keypress.
        let rows = self.build_tree_rows(session_name, view);
        let selected = rows.first().map(|r| r.target.clone());
        client.mode = ClientMode::ChooseTree(ChooseTreeState { view, sel: 0, selected, pending_kill: None });
        ExecOutcome::Ok(String::new())
    }

    /// `display-panes [-d ms]` (Task 8): `ms` overrides `display-panes-time`
    /// for this invocation only (the option itself is untouched).
    fn exec_display_panes_client(&mut self, ms: Option<u32>, client: &mut ClientState) -> ExecOutcome {
        let dur = ms.map(|m| Duration::from_millis(m as u64)).unwrap_or_else(|| self.options.display_panes_time());
        client.mode = ClientMode::DisplayPanes(DisplayPanesState { deadline: Instant::now() + dur });
        ExecOutcome::Ok(String::new())
    }
}

#[cfg(test)]
mod copy_search_tests {
    use super::*;
    use crate::grid::Grid;

    /// Task-4 review, Critical finding #1 (unit-level regression, mirrors
    /// the reviewer's own probe): `to_excl == Some(0)` has no valid start
    /// position (there is no column `< 0`), so it must be an empty range,
    /// not "check column 0 anyway". Before the fix (`to_excl:
    /// usize` + `saturating_sub(1)`), this returned `Some(0)`.
    #[test]
    fn find_last_in_to_excl_zero_is_empty_range() {
        let row: Vec<char> = "needlexxxx".chars().collect();
        let pat: Vec<char> = "needle".chars().collect();
        assert_eq!(find_last_in(&row, &pat, None, Some(0)), None);
    }

    /// `find_last_in` still finds a real match when `to_excl` legitimately
    /// permits it (sanity check alongside the `Some(0)` regression above).
    #[test]
    fn find_last_in_finds_within_range() {
        let row: Vec<char> = "needlexxxx".chars().collect();
        let pat: Vec<char> = "needle".chars().collect();
        assert_eq!(find_last_in(&row, &pat, None, Some(1)), Some(0));
        assert_eq!(find_last_in(&row, &pat, None, None), Some(0));
    }

    /// Task-4 review, Important finding #2: a char earlier in the row whose
    /// full Unicode lowercase mapping expands to more than one char (Turkish
    /// `İ`, U+0130 -> `i` + combining dot above, two chars) must not shift
    /// the reported column of a LATER ASCII match. `fold_char` takes only
    /// the first folded char per original char, preserving a strict 1:1
    /// index<->column mapping; the old `.chars().flat_map(|c|
    /// c.to_lowercase())` fold would have found "hello" at column 2 here
    /// (the naive lowered `Vec<char>` is one char longer than the row), not
    /// its true screen column, 1.
    #[test]
    fn unicode_lowercase_fold_preserves_column() {
        let mut grid = Grid::new(20, 1, 0);
        grid.feed("İhello".as_bytes());
        let pat: Vec<char> = "hello".chars().map(fold_char).collect();
        let got = find_search_match(&grid, &pat, 0, 0, false);
        assert_eq!(got, Some((0, 1)), "match column must equal the true screen column (1), not a naive-fold-shifted index");
    }
}

/// Unit-level coverage for the alt-screen wheel routing decision (Task 5).
///
/// The task brief's suggested e2e approach — have a real PowerShell pane
/// print the raw `CSI ?1049h` bytes itself (`Write-Host -NoNewline
/// "$([char]27)[?1049h"`) and drive a full `server::run` instance under
/// `tests/server_proto.rs` — was tried FIRST and found to be exactly the
/// "too flaky" case the brief anticipated: real Windows ConPTY does not
/// reliably pass a bare `Write-Host`-emitted `CSI ?1049h` through to the
/// server's read side as the literal alt-screen-enter sequence (observed
/// behavior: the pane visibly cleared and PowerShell's prompt reprinted —
/// consistent with SOME redraw happening — but the server pane's
/// `Grid::alt_screen()` never actually flipped true, so a wheel event
/// dispatched right after still entered copy mode instead of translating to
/// arrows). This is a ConPTY passthrough quirk for a synthetic/naive escape
/// injection, not a bug in winmux's own alt-screen tracking (which the
/// dedicated `grid::tests::alt_screen_getter_tracks_mode` test — driven by
/// feeding the escape DIRECTLY into a `Grid`, no ConPTY involved — already
/// covers) or in the routing logic under test here.
///
/// Per the brief's own documented fallback, this instead builds a real
/// `Server` + `Registry` session/pane directly (no ConPTY, no background
/// threads: `PaneRuntime.pty` is `None`, which is fine — `dispatch_mouse`'s
/// alt-screen branch only ever calls `pty.write_input`, gated behind an `if
/// let Some(pty) = ..`, so a `None` pty just makes the arrow-writes a silent
/// no-op instead of a panic) and feeds `\x1b[?1049h` straight into the
/// pane's `Grid` via its own public `feed` — exercising the EXACT same
/// `p.grid.alt_screen()` check `mouse_wheel` branches on, with no
/// ConPTY-passthrough uncertainty anywhere in the test.
#[cfg(test)]
mod mouse_dispatch_tests {
    use super::*;
    use crate::grid::Grid;
    use crate::keys::{MouseEvent, MouseKind};
    use crate::render::Renderer;
    use std::sync::mpsc::channel;

    /// A minimal but real `ClientState` — no writer thread needed since the
    /// test never reads off `tx`'s receiver, it just needs somewhere for
    /// `send`s to land harmlessly.
    fn test_client(cols: u16, rows: u16) -> ClientState {
        let (tx, _rx) = channel::<Vec<u8>>();
        ClientState {
            session: Some("0".to_string()),
            cols,
            rows,
            renderer: Renderer::new(cols, rows),
            key_machine: crate::input::KeyMachine::new(crate::keys::parse_key("C-b").unwrap()),
            mode: ClientMode::Normal,
            message: None,
            tx,
            mouse: super::super::MouseClientState::default(),
        }
    }

    /// Build a `Server` with one session/window/pane (`mouse` on), returning
    /// `(server, session_name, pane_id)`. `alt_screen`: whether to feed
    /// `\x1b[?1049h` into the pane's grid before returning.
    fn test_server_with_pane(alt_screen: bool) -> (Server, String, PaneId) {
        let (tx, _rx) = channel();
        let mut server = Server::new(tx);
        server.options.set("mouse", Some("on"), false, false).unwrap();
        let pane_id = server.mint_pane_id();
        let mut grid = Grid::new(20, 10, 100);
        if alt_screen {
            grid.feed(b"\x1b[?1049h");
            assert!(grid.alt_screen(), "test setup: grid must report alt_screen after CSI ?1049h");
        }
        server.panes.insert(pane_id, super::super::PaneRuntime { pty: None, grid, dead: false, title: String::new() });
        let session_name = server
            .registry
            .create_session(Some("0"), pane_id, (20, 10), 0)
            .expect("create_session")
            .name
            .clone();
        (server, session_name, pane_id)
    }

    fn wheel_up_at(x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind: MouseKind::WheelUp, ctrl: false, meta: false, shift: false, x, y }
    }

    #[test]
    fn alt_screen_wheel_does_not_enter_copy_mode() {
        let (mut server, session_name, _pane_id) = test_server_with_pane(true);
        let mut client = test_client(20, 10);

        let outcome = server.dispatch_mouse(wheel_up_at(5, 3), &mut client, &session_name);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));
        assert!(
            matches!(client.mode, ClientMode::Normal),
            "wheel over an alt-screen pane must NOT enter copy mode, got {:?}",
            match &client.mode {
                ClientMode::Normal => "Normal",
                ClientMode::Copy(_) => "Copy",
                ClientMode::Prompt { .. } => "Prompt",
                ClientMode::ConfirmCmd { .. } => "ConfirmCmd",
                ClientMode::ChooseTree(_) => "ChooseTree",
                ClientMode::DisplayPanes(_) => "DisplayPanes",
            }
        );
    }

    #[test]
    fn live_screen_wheel_enters_copy_mode() {
        // Same setup, but WITHOUT feeding the alt-screen escape: proves the
        // routing genuinely depends on `alt_screen()` rather than always
        // skipping copy-mode entry regardless of pane state.
        let (mut server, session_name, _pane_id) = test_server_with_pane(false);
        let mut client = test_client(20, 10);

        let outcome = server.dispatch_mouse(wheel_up_at(5, 3), &mut client, &session_name);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));
        assert!(matches!(client.mode, ClientMode::Copy(_)), "wheel over a LIVE pane must enter copy mode");
    }

    /// Fix-round Minor finding: `exec_copy_mode`'s `mouse` parameter (fed by
    /// `copy-mode -e` / the wheel-entry call site) must actually be wired to
    /// `CopyState::scroll_exit`, not silently ignored — this is the same
    /// flag `mouse_wheel` used to set by hand right after the call.
    #[test]
    fn exec_copy_mode_wires_mouse_flag_to_scroll_exit() {
        let (mut server, session_name, _pane_id) = test_server_with_pane(false);

        let mut client_mouse = test_client(20, 10);
        let outcome = server.exec_copy_mode(false, true, &mut client_mouse, &session_name);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));
        let ClientMode::Copy(cs) = &client_mouse.mode else { panic!("expected Copy mode after copy-mode -e") };
        assert!(cs.scroll_exit, "copy-mode -e (mouse=true) must set scroll_exit so wheel-down-to-bottom auto-exits");

        let mut client_plain = test_client(20, 10);
        let outcome = server.exec_copy_mode(false, false, &mut client_plain, &session_name);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));
        let ClientMode::Copy(cs) = &client_plain.mode else { panic!("expected Copy mode after plain copy-mode") };
        assert!(!cs.scroll_exit, "plain copy-mode entry (no -e) must not set scroll_exit");
    }

    /// Gap analysis §D point 2: `mouse_down`'s `MouseHit::None` arm didn't
    /// reset `client.mouse.drag`, unlike every other arm (`VBorder`/
    /// `HBorder`/`Pane`), which all overwrite it unconditionally. Real
    /// hit-testing can't actually produce `MouseHit::None` for coordinates
    /// inside a non-degenerate pane area on any practically-sized terminal
    /// (`hit_test`'s pane/border rects always fully tile the area), so this
    /// is exercised directly against `mouse_down` with an EMPTY `rects`
    /// slice -- the same "zero-size rects" degenerate case the root-cause
    /// doc comment on `hit_test` calls out, forced by hand rather than by
    /// shrinking a real terminal to a degenerate size.
    #[test]
    fn mouse_down_miss_clears_stale_drag() {
        let (mut server, session_name, pane_id) = test_server_with_pane(false);
        let mut client = test_client(20, 10);
        client.mouse.drag = super::super::MouseDrag::Border { pane: pane_id, vertical: true };

        let down = MouseEvent { kind: MouseKind::Down(1), ctrl: false, meta: false, shift: false, x: 5, y: 3 };
        let outcome = server.mouse_down(down, 1, &[], &mut client, &session_name);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));
        assert!(
            client.mouse.drag == super::super::MouseDrag::None,
            "a Down that misses every pane/border cell must clear stale drag state"
        );
    }

    /// `docs/follow-ups.md` #64: the choose-tree/display-panes mouse guard
    /// at the top of `dispatch_mouse` swallows Drag/Up events while an
    /// overlay is open, but (before this fix) never cleared
    /// `client.mouse.drag` -- so a drag armed before the overlay opened
    /// (e.g. a keyboard-triggered `display-panes` mid-drag) survived the
    /// overlay's lifetime, revivable by a later out-of-sequence `Drag`/`Up`
    /// with no intervening `Down`. Not reachable through a conformant SGR
    /// mouse stream (hence the unit-level construction here, mirroring
    /// `alt_screen_wheel_does_not_enter_copy_mode`'s direct `Server`+
    /// `ClientState` build instead of a pipe-driven e2e harness): a real
    /// terminal always sends `Down` before `Drag`/`Up`, and `Down`'s own
    /// arms already overwrite `client.mouse.drag` unconditionally, so this
    /// exact sequence can only be forced by hand.
    #[test]
    fn mouse_drag_cleared_when_overlay_swallows_release() {
        let (mut server, session_name, pane_id) = test_server_with_pane(false);
        let mut client = test_client(20, 10);
        client.mouse.drag = super::super::MouseDrag::Border { pane: pane_id, vertical: true };

        let outcome = server.exec_display_panes_client(None, &mut client);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));
        assert!(matches!(client.mode, ClientMode::DisplayPanes(_)), "test setup: overlay must be open");

        let up = MouseEvent { kind: MouseKind::Up(1), ctrl: false, meta: false, shift: false, x: 5, y: 3 };
        let outcome = server.dispatch_mouse(up, &mut client, &session_name);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));
        assert!(
            client.mouse.drag == super::super::MouseDrag::None,
            "overlay guard must clear stale drag state on a swallowed Up"
        );
    }
}

/// Task 8 review fix, Critical #1: unit-level regression coverage mirroring
/// the reviewer's own probe (no ConPTY, no background threads -- built the
/// same way `mouse_dispatch_tests` builds a real `Server`+`Registry` state
/// directly).
#[cfg(test)]
mod choose_tree_dispatch_tests {
    use super::*;
    use crate::grid::Grid;
    use crate::render::Renderer;
    use std::sync::mpsc::channel;

    fn test_client(cols: u16, rows: u16) -> ClientState {
        let (tx, _rx) = channel::<Vec<u8>>();
        ClientState {
            session: Some("0".to_string()),
            cols,
            rows,
            renderer: Renderer::new(cols, rows),
            key_machine: crate::input::KeyMachine::new(crate::keys::parse_key("C-b").unwrap()),
            mode: ClientMode::Normal,
            message: None,
            tx,
            mouse: super::super::MouseClientState::default(),
        }
    }

    fn insert_blank_pane(server: &mut Server) -> PaneId {
        let pane_id = server.mint_pane_id();
        server.panes.insert(pane_id, super::super::PaneRuntime { pty: None, grid: Grid::new(20, 10, 0), dead: false, title: String::new() });
        pane_id
    }

    /// One session ("0") with three windows named A, B, C, in that row
    /// order (A is the session's initial window; B and C added via
    /// `Session::new_window`). Window A is left CURRENT on return (which
    /// window is "current" is irrelevant to the bug under test -- only the
    /// row ORDER and which one is choose-tree-SELECTED matter).
    fn test_server_with_three_windows() -> (Server, String, [WindowId; 3]) {
        let (tx, _rx) = channel();
        let mut server = Server::new(tx);
        let pane_a = insert_blank_pane(&mut server);
        let session = server.registry.create_session(Some("0"), pane_a, (20, 10), 0).expect("create_session");
        let a_id = session.current;
        session.windows[0].name = "A".to_string();
        let session_name = session.name.clone();

        let b_id = server.registry.mint_window_id();
        let pane_b = insert_blank_pane(&mut server);
        server.registry.session_mut(&session_name).unwrap().new_window(b_id, pane_b).name = "B".to_string();

        let c_id = server.registry.mint_window_id();
        let pane_c = insert_blank_pane(&mut server);
        server.registry.session_mut(&session_name).unwrap().new_window(c_id, pane_c).name = "C".to_string();

        // `new_window` makes the new window current each time -- reset to A
        // so the row order (A, B, C) is the only thing this test relies on.
        server.registry.session_mut(&session_name).unwrap().current = a_id;

        (server, session_name, [a_id, b_id, c_id])
    }

    fn key(code: KeyCode) -> Key {
        Key { code, ctrl: false, meta: false, shift: false }
    }

    /// The review's Critical #1, reproduced: `ChooseTreeState.sel` used to be
    /// a raw array index into a row list rebuilt fresh every keypress -- not
    /// a stable target identity. Rows = `[header, A, B, C]`. Select row 2
    /// (window B) via real `Down` dispatches, then simulate a SAME-BATCH
    /// concurrent kill of window A (an EARLIER row) -- directly, bypassing
    /// this client's key machine and with NO intervening render, exactly
    /// the scenario the server's event-loop coalescing makes reachable
    /// (`server.rs`'s `run()` drains the whole channel before rendering).
    /// Rows are now `[header, B, C]`; the OLD raw index 2 would silently
    /// point at C instead of B. Enter must still commit to B.
    #[test]
    fn choose_tree_commit_targets_selected_row_after_concurrent_kill() {
        let (mut server, session_name, [a_id, b_id, _c_id]) = test_server_with_three_windows();
        let mut client = test_client(20, 10);
        let mut sname = session_name.clone();

        let outcome = server.exec_choose_tree_client(false, &mut client, &sname);
        assert!(matches!(outcome, ExecOutcome::Ok(_)));

        // Down, Down: header -> A's row -> B's row.
        server.dispatch_choose_tree_key(&key(KeyCode::Down), &mut client, &mut sname).expect("Down dispatches");
        server.dispatch_choose_tree_key(&key(KeyCode::Down), &mut client, &mut sname).expect("Down dispatches");
        match &client.mode {
            ClientMode::ChooseTree(state) => {
                assert_eq!(state.selected, Some(TreeTarget::Window(session_name.clone(), b_id)), "test setup: selection must be window B before the concurrent kill")
            }
            _ => panic!("expected ChooseTree mode"),
        }

        // Same-batch concurrent kill of window A (bypasses dispatch/render).
        server.kill_window_by_id(&session_name, a_id).expect("kill window A");

        let outcome = server.dispatch_choose_tree_key(&key(KeyCode::Enter), &mut client, &mut sname).expect("Enter dispatches");
        assert!(matches!(outcome, ExecOutcome::Ok(_)), "commit to a same-session window is a plain Ok, not SwitchedSession");
        let current = server.registry.session_mut(&session_name).unwrap().current;
        assert_eq!(
            current, b_id,
            "Enter must commit to what the user actually selected (window B), not whatever now sits at the stale index (window C)"
        );
    }

    /// Same root cause, `x` (Kill) variant: arming the kill-confirm must
    /// target the re-resolved identity too, not the stale index.
    #[test]
    fn choose_tree_kill_targets_selected_row_after_concurrent_kill() {
        let (mut server, session_name, [a_id, b_id, _c_id]) = test_server_with_three_windows();
        let mut client = test_client(20, 10);
        let mut sname = session_name.clone();

        server.exec_choose_tree_client(false, &mut client, &sname);
        server.dispatch_choose_tree_key(&key(KeyCode::Down), &mut client, &mut sname).expect("Down dispatches");
        server.dispatch_choose_tree_key(&key(KeyCode::Down), &mut client, &mut sname).expect("Down dispatches");

        server.kill_window_by_id(&session_name, a_id).expect("kill window A");

        server.dispatch_choose_tree_key(&key(KeyCode::Char('x')), &mut client, &mut sname).expect("x dispatches");
        match &client.mode {
            ClientMode::ChooseTree(state) => {
                let (target, _) = state.pending_kill.as_ref().expect("x must arm a pending kill");
                assert_eq!(
                    target,
                    &TreeTarget::Window(session_name.clone(), b_id),
                    "x must arm the kill-confirm on what the user actually selected (window B), not the stale index"
                );
            }
            _ => panic!("expected ChooseTree mode"),
        }
    }
}

/// SP6 Task 2 (config compatibility): the two dispatch-level gaps the user's
/// real `.tmux.conf` hit that neither `cmd.rs` parsing nor `options.rs`
/// alone can cover -- copy-mode/copy-mode-vi key-table routing, and
/// `source-file`'s `~` expansion.
#[cfg(test)]
mod sp6_config_compat_tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// `bind`/`unbind -T copy-mode-vi`: the `WhichTable::CopyMode`/
    /// `CopyModeVi` variants already existed (`src/bindings.rs:39-44`) and
    /// `cmd.rs`'s OWN `-T` validation already accepted these table names --
    /// only this dispatch-time match was missing the arms.
    #[test]
    fn bind_unbind_copy_mode_tables() {
        let (tx, _rx) = channel();
        let mut server = Server::new(tx);

        let tail = vec![RawCmd { name: "cancel".to_string(), args: vec![] }];
        server
            .exec_bind_key("copy-mode-vi".to_string(), false, "y".to_string(), tail)
            .expect("bind into copy-mode-vi");
        let y = crate::keys::parse_key("y").unwrap();
        assert!(server.bindings.lookup(WhichTable::CopyModeVi, &y).is_some());

        server
            .exec_unbind_key(false, "copy-mode-vi".to_string(), Some("y".to_string()))
            .expect("unbind from copy-mode-vi");
        assert!(server.bindings.lookup(WhichTable::CopyModeVi, &y).is_none());

        // The user's actual fixture line: `unbind -T copy-mode-vi
        // MouseDragEnd1Pane`. `MouseDragEnd1Pane` is a real tmux mouse
        // pseudo-key, but winmux's mouse handling is hardcoded dispatch
        // logic, not table-driven via named pseudo-keys, so `parse_key`
        // rejects it -- unbinding it must still succeed as a silent no-op
        // (nothing could ever have been bound there).
        server
            .exec_unbind_key(false, "copy-mode-vi".to_string(), Some("MouseDragEnd1Pane".to_string()))
            .expect("unbind of an unparseable pseudo-key is a silent no-op, not an error");

        // A copy-mode-vi bind still errors on a genuinely bad key.
        let tail2 = vec![RawCmd { name: "cancel".to_string(), args: vec![] }];
        assert!(server.exec_bind_key("copy-mode-vi".to_string(), false, "MouseDragEnd1Pane".to_string(), tail2).is_err());
    }

    /// `source-file ~/xyz.conf` expands the leading `~/` via `USERPROFILE`
    /// before opening the file (`commands-config-options-formats.md` §2.6).
    #[test]
    fn source_file_expands_tilde() {
        let (tx, _rx) = channel();
        let mut server = Server::new(tx);

        let home = std::env::var("USERPROFILE").expect("USERPROFILE must be set in the test environment");
        let filename = format!("winmux-test-tilde-{}.conf", std::process::id());
        let full_path = std::path::Path::new(&home).join(&filename);
        std::fs::write(&full_path, "set -g base-index 9\n").expect("write temp conf");

        let result = server.execute_source_file_headless(&format!("~/{filename}"));
        let _ = std::fs::remove_file(&full_path);
        result.expect("source-file with a leading ~/ expands via USERPROFILE");
        assert_eq!(server.options.base_index(), 9);
    }

    /// A bare `~` (no trailing path) also expands.
    #[test]
    fn expand_tilde_bare() {
        let home = std::env::var("USERPROFILE").expect("USERPROFILE must be set in the test environment");
        assert_eq!(super::expand_tilde("~"), home);
        assert_eq!(super::expand_tilde("no-tilde-here.conf"), "no-tilde-here.conf");
    }
}

/// Fix-round-1 review findings (Task 3 report addendum, 2026-07-10):
/// Finding 1 -- unstamped focus-handoff paths in `kill_pane_by_id`/
/// `exec_break_pane`/`exec_rotate_window`; Finding 2 -- `pane_activity`
/// never pruned on pane removal (unbounded leak). Both are inspected
/// directly via `Server::pane_activity`/`Server::stamp_active`, which
/// `dispatch.rs` (a child module of `server`) can already reach the same way
/// production code does (see `exec_select_pane`'s `activity` closure above).
#[cfg(test)]
mod focus_activity_fix_tests {
    use super::*;
    use crate::grid::Grid;
    use std::sync::mpsc::channel;

    fn insert_blank_pane(server: &mut Server) -> PaneId {
        let pane_id = server.mint_pane_id();
        server.panes.insert(pane_id, super::super::PaneRuntime { pty: None, grid: Grid::new(80, 24, 0), dead: false, title: String::new() });
        pane_id
    }

    /// One session/window with `n` panes, split HORIZONTALLY one after
    /// another directly via `Layout::split` (bypassing `spawn_pane`/real
    /// ConPTY, same pattern as `mouse_dispatch_tests::test_server_with_pane`
    /// and `choose_tree_dispatch_tests::test_server_with_three_windows`).
    /// Returns `(server, session_name, window_id, pane_ids_in_leaf_order)`.
    /// `pane_activity` is left EMPTY -- none of this setup goes through the
    /// production `exec_*` stamping call sites, so every pane reads as
    /// never-focused (activity 0) until a test stamps one explicitly.
    fn test_server_with_split_panes(n: usize) -> (Server, String, WindowId, Vec<PaneId>) {
        assert!(n >= 1);
        let (tx, _rx) = channel();
        let mut server = Server::new(tx);
        let first = insert_blank_pane(&mut server);
        let session = server.registry.create_session(Some("0"), first, (80, 24), 0).expect("create_session");
        let session_name = session.name.clone();
        let wid = session.current;
        let area = Rect { x: 0, y: 0, w: 80, h: 24 };
        let mut ids = vec![first];
        for _ in 1..n {
            let new_id = insert_blank_pane(&mut server);
            let ok = server
                .registry
                .session_mut(&session_name)
                .unwrap()
                .windows
                .iter_mut()
                .find(|w| w.id == wid)
                .unwrap()
                .layout
                .split(SplitDir::Horizontal, new_id, area)
                .is_ok();
            assert!(ok, "test setup: split must succeed");
            ids.push(new_id);
        }
        (server, session_name, wid, ids)
    }

    fn focused_pane(server: &mut Server, session_name: &str, wid: WindowId) -> PaneId {
        server.registry.session_mut(session_name).unwrap().windows.iter().find(|w| w.id == wid).unwrap().layout.focused()
    }

    fn set_focus(server: &mut Server, session_name: &str, wid: WindowId, pane: PaneId) {
        server.registry.session_mut(session_name).unwrap().windows.iter_mut().find(|w| w.id == wid).unwrap().layout.focus_pane(pane);
    }

    fn max_activity(server: &Server) -> u64 {
        server.pane_activity.values().copied().max().unwrap_or(0)
    }

    /// Finding 1(a): `kill_pane_by_id`'s `Layout::remove` reassigns focus
    /// internally (to the sibling subtree's nearest leaf) only when the
    /// KILLED pane was the window's focused one. That reassignment must get
    /// a fresh `stamp_active`, or `focus_dir`'s MRU tie-break can't tell the
    /// handed-off pane was just made active.
    #[test]
    fn kill_pane_by_id_stamps_focus_handoff() {
        // H(A, H(B, C)): kill focused A -> Layout::remove hands focus to
        // the sibling subtree's first leaf, B.
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        server.stamp_active(c);
        server.stamp_active(b);
        let before_max = max_activity(&server);
        set_focus(&mut server, &session_name, wid, a);

        server.kill_pane_by_id(&session_name, a).expect("kill A");

        let new_focus = focused_pane(&mut server, &session_name, wid);
        assert_eq!(new_focus, b, "test setup sanity: Layout::remove hands focus to the sibling subtree's first leaf");
        assert!(
            server.pane_activity.get(&new_focus).copied().unwrap_or(0) > before_max,
            "the pane focus is handed off to after kill-pane must get a FRESH activity stamp \
             (got {:?}, previous max was {before_max})",
            server.pane_activity.get(&new_focus)
        );
    }

    /// Fix round 2, coordinator-confirmed follow-on: `handle_exited` (a
    /// pane's shell exiting NATURALLY, the most common death path) runs the
    /// same `Layout::remove` focus reassignment as `kill_pane_by_id` -- and
    /// tmux routes both through the same `window_lost_pane` ->
    /// `window_set_active_pane` stamping -- so the handed-off pane must get
    /// a fresh stamp here too. Same construction as
    /// `kill_pane_by_id_stamps_focus_handoff`, but driving the pane-exit
    /// event handler instead of kill-pane.
    #[test]
    fn handle_exited_stamps_focus_handoff() {
        // H(A, H(B, C)): A's shell exits while A is focused ->
        // Layout::remove hands focus to the sibling subtree's first
        // leaf, B.
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        server.stamp_active(c);
        server.stamp_active(b);
        let before_max = max_activity(&server);
        set_focus(&mut server, &session_name, wid, a);

        assert!(server.handle_exited(a), "handle_exited must report the server should keep running");

        let new_focus = focused_pane(&mut server, &session_name, wid);
        assert_eq!(new_focus, b, "test setup sanity: Layout::remove hands focus to the sibling subtree's first leaf");
        assert!(
            server.pane_activity.get(&new_focus).copied().unwrap_or(0) > before_max,
            "the pane focus is handed off to after a natural pane exit must get a FRESH activity stamp \
             (got {:?}, previous max was {before_max})",
            server.pane_activity.get(&new_focus)
        );
        assert!(!server.pane_activity.contains_key(&a), "the exited pane's activity entry must be pruned (round-1 Finding 2 site)");
    }

    /// Guard, mirroring `kill_pane_by_id_does_not_stamp_when_focus_unchanged`:
    /// a NON-focused pane exiting naturally must not spuriously stamp the
    /// window's (unchanged) focus.
    #[test]
    fn handle_exited_does_not_stamp_when_focus_unchanged() {
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        set_focus(&mut server, &session_name, wid, c);
        server.stamp_active(c);

        assert!(server.handle_exited(a));

        assert_eq!(focused_pane(&mut server, &session_name, wid), c, "a non-focused pane exiting must not move focus");
        assert!(!server.pane_activity.contains_key(&b), "B was never focused and must not be spuriously stamped");
    }

    /// Guard: killing a NON-focused pane must NOT spuriously stamp the
    /// window's (unchanged) focus -- `Layout::remove` never touches
    /// `focused` in that case, so neither should `kill_pane_by_id`.
    #[test]
    fn kill_pane_by_id_does_not_stamp_when_focus_unchanged() {
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(3);
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        set_focus(&mut server, &session_name, wid, c);
        server.stamp_active(c); // c is focused and already stamped

        server.kill_pane_by_id(&session_name, a).expect("kill non-focused A");

        assert_eq!(focused_pane(&mut server, &session_name, wid), c, "killing a non-focused pane must not move focus");
        assert!(!server.pane_activity.contains_key(&b), "B was never focused and must not be spuriously stamped");
    }

    /// Finding 1(c): `Layout::rotate` keeps the same LEAF POSITION focused,
    /// but a DIFFERENT PaneId now occupies it -- `focused` is reassigned to
    /// a new pane on every successful rotate.
    #[test]
    fn exec_rotate_window_stamps_new_focus() {
        let (mut server, session_name, wid, _ids) = test_server_with_split_panes(3);
        let before_max = max_activity(&server);

        server.exec_rotate_window(false, None, Some(&session_name)).expect("rotate-window");

        let new_focus = focused_pane(&mut server, &session_name, wid);
        assert!(
            server.pane_activity.get(&new_focus).copied().unwrap_or(0) > before_max,
            "rotate-window's focus reassignment (Layout::rotate) must get a fresh stamp"
        );
    }

    /// Finding 1(b): `exec_break_pane` has TWO focus-handoff sites --
    /// (1) the source window's `Layout::remove` reassignment (same shape as
    /// `kill_pane_by_id`), and (2) the moved pane becoming the new window's
    /// sole, active pane.
    #[test]
    fn exec_break_pane_stamps_source_handoff_and_moved_pane() {
        // H(A, B): break B (focused) out; source window keeps only A, whose
        // Layout::remove-driven focus reassignment must be stamped, THEN
        // the moved pane B must be stamped as the new window's active pane
        // (in that order -- B's stamp must be the more recent of the two).
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(2);
        let (a, b) = (ids[0], ids[1]);
        server.stamp_active(a);
        let before_max = max_activity(&server);
        set_focus(&mut server, &session_name, wid, b);

        server.exec_break_pane(false, None, Some(&session_name)).expect("break-pane");

        let source_focus = focused_pane(&mut server, &session_name, wid);
        assert_eq!(source_focus, a, "test setup sanity: only A remains in the source window");
        let a_stamp = server.pane_activity.get(&a).copied().unwrap_or(0);
        assert!(a_stamp > before_max, "source window's focus handoff (to A) after break-pane must get a fresh stamp");

        let b_stamp = server.pane_activity.get(&b).copied().unwrap_or(0);
        assert!(b_stamp > a_stamp, "the moved pane (B), becoming the new window's active pane, must be stamped AFTER A's handoff stamp");
    }

    /// Finding 2: `pane_activity` must be pruned wherever a pane is removed
    /// from `self.panes`/`self.last_rects`, or it leaks unboundedly across
    /// the server's lifetime.
    #[test]
    fn kill_pane_by_id_prunes_pane_activity() {
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(2);
        let (a, b) = (ids[0], ids[1]);
        set_focus(&mut server, &session_name, wid, a);
        server.stamp_active(a);
        server.stamp_active(b);

        server.kill_pane_by_id(&session_name, a).expect("kill A");

        assert!(!server.pane_activity.contains_key(&a), "a killed pane's activity entry must be pruned");
        assert!(server.pane_activity.contains_key(&b), "the surviving pane's entry must be untouched");
    }

    #[test]
    fn kill_window_by_id_prunes_pane_activity_for_all_its_panes() {
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(2);
        let (a, b) = (ids[0], ids[1]);
        // A second window so the first can be killed outright (kill_window
        // refuses the session's only window).
        let other_pane = insert_blank_pane(&mut server);
        let other_wid = server.registry.mint_window_id();
        server.registry.session_mut(&session_name).unwrap().new_window(other_wid, other_pane);
        server.stamp_active(a);
        server.stamp_active(b);

        server.kill_window_by_id(&session_name, wid).expect("kill window");

        assert!(!server.pane_activity.contains_key(&a), "killed window's pane A must be pruned from pane_activity");
        assert!(!server.pane_activity.contains_key(&b), "killed window's pane B must be pruned from pane_activity");
    }

    #[test]
    fn exec_move_window_prunes_occupant_pane_activity() {
        // Two windows (idx0 = wid/A, idx1 = other_wid/B); move-window -k
        // onto idx1 kills B, its occupant, whose activity entry must be
        // pruned alongside its pane/rect cleanup.
        let (mut server, session_name, wid, ids) = test_server_with_split_panes(1);
        let a = ids[0];
        let other_pane = insert_blank_pane(&mut server);
        let other_wid = server.registry.mint_window_id();
        server.registry.session_mut(&session_name).unwrap().new_window(other_wid, other_pane);
        server.stamp_active(a);
        server.stamp_active(other_pane);
        server.registry.session_mut(&session_name).unwrap().current = wid;

        server.exec_move_window(true, "1".to_string(), Some(&session_name)).expect("move-window -k onto occupied index 1");

        assert!(!server.pane_activity.contains_key(&other_pane), "the killed occupant's activity entry must be pruned");
        assert!(server.pane_activity.contains_key(&a), "the mover's own pane activity must be untouched");
    }
}
