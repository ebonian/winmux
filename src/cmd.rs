//! tmux command tokenizer, command table, and typed commands.
//!
//! Pure module: no I/O, `std` only. This is the parsing layer shared by all
//! four SP3 command entry points (`.tmux.conf` lines, the `winmux` CLI
//! argv, the `prefix-:` command prompt, and key bindings) — see the `## cmd`
//! section of `docs/specs/2026-07-07-command-config-interfaces.md`.
//!
//! Pipeline: `join_continuations` (file loader: physical lines -> logical
//! lines) -> [`parse_line`] (a logical line -> one or more [`RawCmd`]s,
//! tmux config tokenization) -> [`resolve`] (a `RawCmd` -> a typed
//! [`ParsedCmd`] via the command table: full names + tmux aliases + flag
//! parsing). Execution against live server state is a later task (Task 6) —
//! this module only parses.

use crate::geom::Direction;

/// One untyped command: a name (already alias-resolved-or-not — that's
/// [`resolve`]'s job) plus its argument tokens, exactly as produced by
/// [`parse_line`]'s tokenizer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCmd {
    pub name: String,
    pub args: Vec<String>,
}

/// Tokenize one logical config/prompt/CLI line into zero or more [`RawCmd`]s.
///
/// Rules (tmux config-line tokenization):
/// - Whitespace (space/tab, and stray `\r`/`\n`) splits tokens and is
///   otherwise discarded.
/// - `'...'` is a literal run: no escapes recognized inside, including `#`
///   and `;` (both lose all special meaning between the quotes).
/// - `"..."` recognizes exactly two escapes: `\"` -> literal `"`, `\\` ->
///   literal `\`. Any other `\<c>` is passed through verbatim as BOTH
///   characters (the backslash is not an escape for anything else).
/// - A quoted segment can be adjacent to bare characters or another quoted
///   segment with no intervening whitespace — they concatenate into one
///   token (shell-style), e.g. `foo'bar'"baz"` is one token `foobarbaz`.
/// - `#` outside any quote starts a comment that runs to the end of the
///   line (even mid-token: `foo#bar` tokenizes `foo` then discards `#bar`).
/// - An unquoted, unescaped `;` is a command separator: it ends the token in
///   progress and the current command, and starts a new one. It is never
///   itself part of any token.
/// - `\;` outside quotes is an escaped semicolon: the backslash is consumed
///   and a literal `;` is appended to the token being built (this does
///   *not* end the command — see `resolve`'s tail-splitting for
///   `bind-key`/`confirm-before`, which is where a lone `\;` token, or an
///   equivalently-lone quoted `";"`, actually takes effect).
/// - Outside quotes, a bare `\` not followed by `;` is literal (no other
///   escape sequences are recognized at the top level).
///
/// Returns `Err("unterminated quote")` if a `'` or `"` is never closed
/// before the line ends. Empty commands (blank line, comment-only line, or
/// runs of bare `;`) are silently dropped, not emitted as empty `RawCmd`s.
pub fn parse_line(line: &str) -> Result<Vec<RawCmd>, String> {
    let mut commands: Vec<RawCmd> = Vec::new();
    let mut cur_tokens: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut chars = line.chars().peekable();

    fn push_token(token: &mut Option<String>, cur_tokens: &mut Vec<String>) {
        if let Some(t) = token.take() {
            cur_tokens.push(t);
        }
    }
    fn push_command(cur_tokens: &mut Vec<String>, commands: &mut Vec<RawCmd>) {
        if !cur_tokens.is_empty() {
            let name = cur_tokens.remove(0);
            commands.push(RawCmd { name, args: std::mem::take(cur_tokens) });
        }
    }

    while let Some(c) = chars.next() {
        match c {
            '#' => break,
            ';' => {
                push_token(&mut token, &mut cur_tokens);
                push_command(&mut cur_tokens, &mut commands);
            }
            ' ' | '\t' | '\r' | '\n' => {
                push_token(&mut token, &mut cur_tokens);
            }
            '\'' => {
                let buf = token.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => buf.push(ch),
                        None => return Err("unterminated quote".to_string()),
                    }
                }
            }
            '"' => {
                let buf = token.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('"') => buf.push('"'),
                            Some('\\') => buf.push('\\'),
                            Some(other) => {
                                buf.push('\\');
                                buf.push(other);
                            }
                            None => return Err("unterminated quote".to_string()),
                        },
                        Some(ch) => buf.push(ch),
                        None => return Err("unterminated quote".to_string()),
                    }
                }
            }
            '\\' if chars.peek() == Some(&';') => {
                chars.next();
                token.get_or_insert_with(String::new).push(';');
            }
            other => {
                token.get_or_insert_with(String::new).push(other);
            }
        }
    }
    push_token(&mut token, &mut cur_tokens);
    push_command(&mut cur_tokens, &mut commands);
    Ok(commands)
}

/// Join `\`-continued physical lines into logical lines for the config file
/// loader. A physical line whose last character is a single trailing `\`
/// (after stripping an optional trailing `\r`, so CRLF files work) is
/// joined directly (backslash removed, no separator inserted) with the next
/// physical line; this repeats across a chain of continuations. Returns
/// `(first_line_number, joined_text)` pairs, 1-based, one per logical line.
///
/// A trailing `\` on the very last physical line (nothing left to join with)
/// has no continuation to perform, so the backslash is put back rather than
/// silently dropped.
pub fn join_continuations<'a, I: Iterator<Item = &'a str>>(lines: I) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (idx, raw) in lines.enumerate() {
        let lineno = idx + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let (start, acc) = match pending.take() {
            Some((n, s)) => (n, s + line),
            None => (lineno, line.to_string()),
        };
        match acc.strip_suffix('\\') {
            Some(stripped) => pending = Some((start, stripped.to_string())),
            None => out.push((start, acc)),
        }
    }
    if let Some((n, s)) = pending {
        // Nothing followed to continue onto — the trailing backslash was
        // never actually a continuation, so keep it.
        out.push((n, format!("{s}\\")));
    }
    out
}

/// A fully typed, alias-resolved SP3 command. `bind-key`/`confirm-before`
/// store their bound/wrapped command(s) as unresolved [`RawCmd`]s (tmux does
/// late binding too — the tail is re-parsed against the table at execution
/// time, not here).
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedCmd {
    SplitWindow { horizontal: bool, target: Option<String> },
    SelectPane { dir: Option<Direction>, target: Option<String> },
    SelectWindow { target: String },
    NextWindow,
    PreviousWindow,
    LastWindow,
    LastPane,
    NewWindow { name: Option<String> },
    KillPane { target: Option<String> },
    KillWindow { target: Option<String> },
    ResizePane { dir: Option<Direction>, zoom: bool, count: i32 },
    RenameWindow { target: Option<String>, name: String },
    RenameSession { target: Option<String>, name: String },
    /// `detach-client [-s target]`. `target: None` = detach the ACTING
    /// client (tmux's bare `detach-client`); `Some(s)` = detach every client
    /// attached to session `s`. The Task 6 dispatcher rejects the bare form
    /// when there is no acting client (CLI/config context) with the SP2
    /// verbatim usage error -- `resolve` itself accepts both forms.
    DetachClient { target: Option<String> },
    SendKeys { literal: bool, target: Option<String>, keys: Vec<String> },
    SendPrefix,
    /// `switch-client -p`/`-n` (previous/next session). SP3 only supports
    /// these two flags (documented deviation: tmux's `-l` "last session" is
    /// not tracked in SP3 -- passing `-l` is a `usage:` error like any other
    /// unrecognized flag).
    SwitchClient { next: bool },
    DisplayMessage { text: Option<String> },
    ConfirmBefore { prompt: Option<String>, tail: Vec<RawCmd> },
    CommandPrompt { initial: Option<String> },
    SetOption { global: bool, window: bool, append: bool, unset: bool, name: String, value: Option<String> },
    /// `show-options`/`show`/`show-window-options`/`showw` (`-gwqv`, closes
    /// follow-up #68): `window` mirrors `SetOption`'s (true when `-w` was
    /// given OR the command word was `showw`/`show-window-options`); `quiet`
    /// (`-q`) suppresses the unknown-option error (only meaningful for the
    /// `@name` path today — see `Options::show_user_option`); `value_only`
    /// (`-v`) prints just the value, no `name ` prefix.
    ShowOptions { global: bool, window: bool, quiet: bool, value_only: bool, name: Option<String> },
    BindKey { table: String, repeat: bool, key: String, tail: Vec<RawCmd> },
    UnbindKey { all: bool, table: String, key: Option<String> },
    ListKeys,
    SourceFile { path: String },
    // SP2 CLI commands, folded into the same table.
    NewSession { detached: bool, name: Option<String>, cols: Option<u16>, rows: Option<u16> },
    AttachSession { target: Option<String>, detach_others: bool },
    ListSessions,
    ListWindows { target: Option<String> },
    HasSession { target: String },
    KillSession { target: Option<String> },
    KillServer,
    /// `copy-mode [-u] [-e]` (Task 2, sub-project 4): enter copy mode on the
    /// acting client's focused pane. `page_up` (`-u`) additionally scrolls up
    /// one page immediately (the `PPage` binding). `mouse` (`-e`) is stored
    /// but unused until the mouse task (SP4 §4) wires wheel-triggered entry.
    CopyMode { page_up: bool, mouse: bool },
    /// One internal `copy-*` movement/scroll/cancel/selection command (Task
    /// 2/3 scope). Dispatched only with an acting client currently in
    /// `ClientMode::Copy`; see the `## copy-mode` contract section. Also
    /// reachable via tmux's `send-keys -X <name>` spelling (`resolve`'s
    /// `send-keys` arm maps the `-X` name to this).
    CopyCmd(CopyAction),
    /// `paste-buffer|pasteb [-p] [-r] [-b name] [-t target-pane]` (Task 3):
    /// write a buffer's contents to a pane's pty. `name: None` = the newest
    /// buffer. `no_replace` (`-r`, tmux "do not replace LF with CR") default
    /// `false` -- the DEFAULT behavior replaces every `\n` in the buffer with
    /// `\r` before writing (tmux's own default; shells expect `\r` to submit
    /// a line). `-p` (bracketed-paste passthrough) is accepted and IGNORED
    /// (v1 simplification, documented in the design spec's deferrals list).
    PasteBuffer { name: Option<String>, target: Option<String>, no_replace: bool },
    /// `list-buffers|lsb` (Task 3): `<name>: <size> bytes: "<sample>"` lines,
    /// oldest first. Full multi-line text via the CLI/headless path;
    /// dispatched from a CLIENT (a key binding, or the `:` prompt) instead
    /// shows just the first line plus a `(N buffers)` suffix as a transient
    /// message (documented simplification -- tmux shows a pager).
    ListBuffers,
    /// `delete-buffer|deleteb [-b name]` (Task 3): `name: None` = the newest
    /// buffer.
    DeleteBuffer { name: Option<String> },
    /// `set-buffer|setb [-b name] data...` (Task 3): `name: None` creates a
    /// new AUTOMATIC buffer (same `buffer-limit` eviction as
    /// `copy-selection-and-cancel`); `Some(name)` sets/overwrites a MANUAL
    /// buffer (exempt from eviction).
    SetBuffer { name: Option<String>, data: String },
    /// `select-layout|selectl [-t target] [layout-name]` (Task 6, sub-project
    /// 4): rebuild the target window's split tree as one of the five preset
    /// layouts. `name: None` (bare `select-layout`) re-applies the window's
    /// current cycle position (tmux's "re-flow the current named layout"
    /// idiom) -- dispatch-time, since it needs `Window::last_layout`.
    /// `name: Some(n)` is validated against the five exact tmux layout names
    /// HERE (mirroring `bind-key -T`'s inline table-name validation just
    /// above) -- `Err("unknown layout: {n}")` for anything else.
    SelectLayout { target: Option<String>, name: Option<String> },
    /// `next-layout|nextl [-t target]` (Task 6): advance the target window's
    /// `next-layout` cycle by one (wrapping), per `layout::PRESET_CYCLE`.
    NextLayout { target: Option<String> },
    /// `swap-pane|swapp [-U] [-D] [-s src] [-t dst]` (Task 6): `dir: Some(Up
    /// | Down)` swaps the ACTING client's active pane with the
    /// previous/next pane in creation order (wrapping), focus following the
    /// active pane to its new position. `dir: None` uses the explicit
    /// `-s src`/`-t dst` pane targets instead (each `None` half defaults via
    /// the normal `resolve_pane_target` fallback -- the acting client's
    /// focused pane). Any other `Direction` is unreachable: `resolve`'s flag
    /// scanner only ever admits `-U`/`-D` for this command.
    SwapPane { dir: Option<Direction>, src: Option<String>, dst: Option<String> },
    /// `rotate-window|rotatew [-D] [-t target]` (Task 6): rotate every pane's
    /// content through the target window's leaf positions by one step.
    /// `down` is the `-D` flag; bare `rotate-window` (`down: false`, the
    /// `C-o` default binding) and `-D` (the `M-o` binding) rotate in opposite
    /// directions -- see `Layout::rotate`'s doc comment for the exact
    /// permutation each maps to.
    RotateWindow { down: bool, target: Option<String> },
    /// `break-pane|breakp [-d] [-n name]` (Task 7, sub-project 4): the
    /// target session/window's FOCUSED pane leaves its window and becomes a
    /// new window (next free index >= the session's `base-index`), which
    /// becomes current unless `-d` (`detached`) is given. `-n` names the new
    /// window. No pane target flag: winmux's break-pane always acts on the
    /// resolved current pane (matches the design spec's `## 6. Window ops`
    /// section, which omits a `-s`/`-t` pane selector entirely -- smaller,
    /// honest scope, same pattern as `swap-pane`'s own documented
    /// deviations).
    BreakPane { detached: bool, name: Option<String> },
    /// `move-window|movew [-k] -t index` (Task 7): re-index the CURRENT
    /// window (of the target session) to `target` (required -- there is
    /// nothing to do without one, unlike real tmux's fuller cross-session
    /// form). Occupied index -> `index in use: <n>` unless `-k` (`kill`)
    /// kills the occupant first. `target` is resolved at dispatch time as a
    /// bare/`:`-prefixed index within the SAME session (no cross-session
    /// move -- see the design spec's `## 6. Window ops` section).
    MoveWindow { kill: bool, target: String },
    /// `swap-window|swapw [-d] [-s src] -t dst` (SP6 Task 5): exchange the
    /// INDEX of the `src` window (default: the acting session's CURRENT
    /// window -- winmux has no `-s`-defaulting marked-pane concept) with the
    /// `dst` window's index, within the SAME session (no cross-session
    /// support -- same simplification `move-window` already documents).
    /// `dst` is REQUIRED (`resolve` rejects a missing `-t` with the usage
    /// error before ever constructing this variant, so `dst` is always
    /// `Some` in practice -- the `Option` shape mirrors `SwapPane`'s for
    /// consistency). Both `src`/`dst` accept the SAME grammar
    /// (`windows-and-sessions.md` §swap-window/§"Target resolution"):
    /// absent -> current window; a bare `+N`/`-N` (N optional, default 1) ->
    /// a relative winlink offset, WRAPPING; `:N` or a bare digit-string ->
    /// exact index in the current session; anything else -> exact-then-
    /// prefix window NAME match. `detach` (`-d`) governs whether the acting
    /// session's focus follows the INDEX (default: `false`, i.e. `-d`
    /// absent) or the WINDOW OBJECT (`true`, i.e. `-d` given) through the
    /// swap -- see `Server::exec_swap_window`'s doc comment for the exact
    /// current/last bookkeeping this implies.
    SwapWindow { src: Option<String>, dst: Option<String>, detach: bool },
    /// `find-window|findw <pattern>` (Task 7): case-insensitive substring
    /// search (v1, no regex) over window NAMES and every pane's CURRENTLY
    /// VISIBLE content (not scrollback) in the target session, in window-
    /// index order; jumps to the first match (current window counts too).
    /// No match -> `Ok` with a transient `no windows matching: <p>` message
    /// (not an `Err` -- matches tmux, which never treats "nothing found" as
    /// a command failure).
    FindWindow { pattern: String },
    /// `choose-tree|choosetree [-s] [-w]` (Task 8, sub-project 4): open the
    /// choose-tree overlay on the acting client. `sessions: true` (`-s`)
    /// lists every session (one collapsed row each); `false` (bare, or the
    /// real tmux `-w` flag) lists the acting client's CURRENT session's
    /// windows (a session header row + one indented row per window) — see
    /// the design spec's `## 7. Overlays` section for the exact row format
    /// and the documented "windows of the current session only" scope
    /// simplification. `-s`/`-w` together is a usage error.
    ChooseTree { sessions: bool },
    /// `display-panes|displayp [-d ms]` (Task 8): show a per-pane digit
    /// overlay on the acting client's current window; `ms: None` uses the
    /// `display-panes-time` option's current value (resolved at dispatch
    /// time, not here).
    DisplayPanes { ms: Option<u32> },
    /// `clock-mode` (Task 10, sub-project 6 wave 2): open the big-clock
    /// overlay on the acting client's current window, on the FOCUSED pane
    /// (mirrors `copy-mode`'s "binds to the pane focused at entry" rule —
    /// see the `server` amendment in the parity-polish contract doc).
    /// No flags: real tmux's `clock-mode [-t target-pane]` is not modeled
    /// here, following `display-panes`' own precedent of a client-scoped,
    /// no-target overlay command (`## overlays` design-spec section) —
    /// winmux's overlay lives in per-CLIENT state, not addressable by an
    /// arbitrary target pane.
    ClockMode,
}

/// The Task 2 (movement/scroll/cancel) subset of tmux copy-mode's internal
/// `send-keys -X` command set. See the design spec's `## 2. Copy mode`
/// section and the `## copy-mode` contract section for the exact per-name
/// mapping (`copy_action_name`/`copy_action_from_x_name` below).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyAction {
    CursorLeft,
    CursorRight,
    CursorUp,
    CursorDown,
    StartOfLine,
    EndOfLine,
    HistoryTop,
    HistoryBottom,
    TopLine,
    MiddleLine,
    BottomLine,
    ScrollUp,
    ScrollDown,
    HalfpageUp,
    HalfpageDown,
    PageUp,
    PageDown,
    NextWord,
    PreviousWord,
    NextWordEnd,
    Cancel,
    /// Task 3, sub-project 4 (selection): anchor := cursor, starting a new
    /// linear selection (or restarting one if a selection was already
    /// active).
    BeginSelection,
    /// Toggle rectangle mode on the CURRENT selection; a no-op (v1
    /// simplification, documented) when there is no active selection --
    /// tmux additionally sticks the toggled mode for the NEXT selection in
    /// the same copy-mode session, which winmux does not reproduce.
    RectangleToggle,
    /// Swap the anchor and cursor (including each one's own scroll offset).
    /// A no-op when there is no active selection.
    OtherEnd,
    /// Drop the current selection (if any); copy mode itself stays active.
    ClearSelection,
    /// Extract the current selection's text (if any) into a new automatic
    /// paste buffer, then exit copy mode (the "copy" action).
    SelectionAndCancel,
    /// Task 4, sub-project 4 (search): open the `/`/`C-s` "Search Down: "
    /// status-line prompt (forward = toward newer content / the live
    /// bottom). Committing dispatches the actual search; see the
    /// `## copy-mode` contract section's Task 4 amendment.
    SearchForward,
    /// Task 4: open the `?`/`C-r` "Search Up: " status-line prompt
    /// (backward = toward older content / history top).
    SearchBackward,
    /// Task 4: repeat the last committed search in the SAME direction (`n`).
    /// A no-op if no search has ever been committed in this copy-mode
    /// session.
    SearchAgain,
    /// Task 4: repeat the last committed search in the OPPOSITE direction
    /// (`N`). A no-op if no search has ever been committed in this copy-mode
    /// session.
    SearchReverse,
}

/// `copy-<action>` canonical command name for one [`CopyAction`] (bindings
/// table storage, `list-keys` output).
fn copy_action_name(a: CopyAction) -> &'static str {
    match a {
        CopyAction::CursorLeft => "copy-cursor-left",
        CopyAction::CursorRight => "copy-cursor-right",
        CopyAction::CursorUp => "copy-cursor-up",
        CopyAction::CursorDown => "copy-cursor-down",
        CopyAction::StartOfLine => "copy-start-of-line",
        CopyAction::EndOfLine => "copy-end-of-line",
        CopyAction::HistoryTop => "copy-history-top",
        CopyAction::HistoryBottom => "copy-history-bottom",
        CopyAction::TopLine => "copy-top-line",
        CopyAction::MiddleLine => "copy-middle-line",
        CopyAction::BottomLine => "copy-bottom-line",
        CopyAction::ScrollUp => "copy-scroll-up",
        CopyAction::ScrollDown => "copy-scroll-down",
        CopyAction::HalfpageUp => "copy-halfpage-up",
        CopyAction::HalfpageDown => "copy-halfpage-down",
        CopyAction::PageUp => "copy-page-up",
        CopyAction::PageDown => "copy-page-down",
        CopyAction::NextWord => "copy-next-word",
        CopyAction::PreviousWord => "copy-previous-word",
        CopyAction::NextWordEnd => "copy-next-word-end",
        CopyAction::Cancel => "copy-cancel",
        CopyAction::BeginSelection => "copy-begin-selection",
        CopyAction::RectangleToggle => "copy-rectangle-toggle",
        CopyAction::OtherEnd => "copy-other-end",
        CopyAction::ClearSelection => "copy-clear-selection",
        CopyAction::SelectionAndCancel => "copy-selection-and-cancel",
        CopyAction::SearchForward => "copy-search-forward",
        CopyAction::SearchBackward => "copy-search-backward",
        CopyAction::SearchAgain => "copy-search-again",
        CopyAction::SearchReverse => "copy-search-reverse",
    }
}

/// Every [`CopyAction`], for building the canonical-name table and iterating
/// in tests.
const COPY_ACTIONS: &[CopyAction] = &[
    CopyAction::CursorLeft,
    CopyAction::CursorRight,
    CopyAction::CursorUp,
    CopyAction::CursorDown,
    CopyAction::StartOfLine,
    CopyAction::EndOfLine,
    CopyAction::HistoryTop,
    CopyAction::HistoryBottom,
    CopyAction::TopLine,
    CopyAction::MiddleLine,
    CopyAction::BottomLine,
    CopyAction::ScrollUp,
    CopyAction::ScrollDown,
    CopyAction::HalfpageUp,
    CopyAction::HalfpageDown,
    CopyAction::PageUp,
    CopyAction::PageDown,
    CopyAction::NextWord,
    CopyAction::PreviousWord,
    CopyAction::NextWordEnd,
    CopyAction::Cancel,
    CopyAction::BeginSelection,
    CopyAction::RectangleToggle,
    CopyAction::OtherEnd,
    CopyAction::ClearSelection,
    CopyAction::SelectionAndCancel,
    CopyAction::SearchForward,
    CopyAction::SearchBackward,
    CopyAction::SearchAgain,
    CopyAction::SearchReverse,
];

fn copy_action_from_canonical(name: &str) -> Option<CopyAction> {
    COPY_ACTIONS.iter().copied().find(|a| copy_action_name(*a) == name)
}

/// `send-keys -X <name>` spelling -> [`CopyAction`] (tmux's copy-mode
/// command names, hyphenated without the `copy-` prefix).
fn copy_action_from_x_name(name: &str) -> Option<CopyAction> {
    Some(match name {
        "cancel" => CopyAction::Cancel,
        "cursor-left" => CopyAction::CursorLeft,
        "cursor-right" => CopyAction::CursorRight,
        "cursor-up" => CopyAction::CursorUp,
        "cursor-down" => CopyAction::CursorDown,
        "start-of-line" => CopyAction::StartOfLine,
        "end-of-line" => CopyAction::EndOfLine,
        "history-top" => CopyAction::HistoryTop,
        "history-bottom" => CopyAction::HistoryBottom,
        "top-line" => CopyAction::TopLine,
        "middle-line" => CopyAction::MiddleLine,
        "bottom-line" => CopyAction::BottomLine,
        "scroll-up" => CopyAction::ScrollUp,
        "scroll-down" => CopyAction::ScrollDown,
        "halfpage-up" => CopyAction::HalfpageUp,
        "halfpage-down" => CopyAction::HalfpageDown,
        "page-up" => CopyAction::PageUp,
        "page-down" => CopyAction::PageDown,
        "next-word" => CopyAction::NextWord,
        "previous-word" => CopyAction::PreviousWord,
        "next-word-end" => CopyAction::NextWordEnd,
        "begin-selection" => CopyAction::BeginSelection,
        "rectangle-toggle" => CopyAction::RectangleToggle,
        "other-end" => CopyAction::OtherEnd,
        "clear-selection" => CopyAction::ClearSelection,
        // tmux's own -X spelling for this one command retains the "copy-"
        // prefix (unlike every other -X name above) -- verified against
        // tmux master's `cmd-copy-mode.c`/`window-copy.c` command table.
        "copy-selection-and-cancel" => CopyAction::SelectionAndCancel,
        "search-forward" => CopyAction::SearchForward,
        "search-backward" => CopyAction::SearchBackward,
        "search-again" => CopyAction::SearchAgain,
        "search-reverse" => CopyAction::SearchReverse,
        _ => return None,
    })
}

/// Map a command name OR any tmux alias to its canonical full name. `None`
/// for an unrecognized name.
fn canonical(name: &str) -> Option<&'static str> {
    if name == "copy-mode" {
        return Some("copy-mode");
    }
    // Internal copy-* commands: bindable and resolvable but their canonical
    // name IS the alias (no separate short form).
    if let Some(a) = copy_action_from_canonical(name) {
        return Some(copy_action_name(a));
    }
    Some(match name {
        "split-window" | "splitw" => "split-window",
        "select-pane" | "selectp" => "select-pane",
        "select-window" | "selectw" => "select-window",
        "next-window" | "next" => "next-window",
        "previous-window" | "prev" => "previous-window",
        "last-window" | "last" => "last-window",
        "last-pane" | "lastp" => "last-pane",
        "new-window" | "neww" => "new-window",
        "kill-pane" | "killp" => "kill-pane",
        "kill-window" | "killw" => "kill-window",
        "resize-pane" | "resizep" => "resize-pane",
        "rename-window" | "renamew" => "rename-window",
        "rename-session" | "rename" => "rename-session",
        "detach-client" => "detach-client",
        "send-keys" | "send" => "send-keys",
        "send-prefix" => "send-prefix",
        "switch-client" | "switchc" => "switch-client",
        "display-message" | "display" => "display-message",
        "confirm-before" | "confirm" => "confirm-before",
        "command-prompt" => "command-prompt",
        "set-option" | "set" => "set-option",
        // `setw`/`set-window-option` is a real separate tmux command entry
        // (not a bare config-level alias) sharing `set-option`'s exec
        // function with an implied `-w` -- see the "set-option" resolve arm
        // below, which infers `window: true` from `raw.name` for these two
        // spellings even without an explicit `-w` flag.
        "setw" | "set-window-option" => "set-option",
        // `showw`/`show-window-options` mirrors `setw`'s real-separate-
        // command-entry relationship to `set-option`
        // (`commands-config-options-formats.md`: "Same for
        // `show-window-options`/`showw`" — `cmd-show-options.c:54-65`):
        // shares `show-options`'s exec function with an implied `-w`, see
        // the "show-options" resolve arm below.
        "show-options" | "show" | "show-window-options" | "showw" => "show-options",
        "bind-key" | "bind" => "bind-key",
        "unbind-key" | "unbind" => "unbind-key",
        "list-keys" | "lsk" => "list-keys",
        "source-file" | "source" => "source-file",
        "new-session" | "new" => "new-session",
        "attach-session" | "attach" | "a" => "attach-session",
        "list-sessions" | "ls" => "list-sessions",
        "list-windows" | "lsw" => "list-windows",
        "has-session" | "has" => "has-session",
        "kill-session" => "kill-session",
        "kill-server" => "kill-server",
        "paste-buffer" | "pasteb" => "paste-buffer",
        "list-buffers" | "lsb" => "list-buffers",
        "delete-buffer" | "deleteb" => "delete-buffer",
        "set-buffer" | "setb" => "set-buffer",
        "select-layout" | "selectl" => "select-layout",
        "next-layout" | "nextl" => "next-layout",
        "swap-pane" | "swapp" => "swap-pane",
        "rotate-window" | "rotatew" => "rotate-window",
        "break-pane" | "breakp" => "break-pane",
        "move-window" | "movew" => "move-window",
        "swap-window" | "swapw" => "swap-window",
        "find-window" | "findw" => "find-window",
        "choose-tree" | "choosetree" => "choose-tree",
        "display-panes" | "displayp" => "display-panes",
        "clock-mode" => "clock-mode",
        _ => return None,
    })
}

/// The `usage: ...` line for a command (looked up by full name or alias),
/// `None` if the name is unknown. For the commands SP2's `cli_exec.rs`
/// already implements (`new-session`, `has-session`, `kill-session`,
/// `kill-server`, `rename-session`, `rename-window`, `list-sessions`,
/// `list-windows`, `detach-client`) these strings are copied VERBATIM from
/// that module's `USAGE_*` constants so its existing tests keep passing
/// once the server rewires onto this table.
pub fn usage(name: &str) -> Option<&'static str> {
    let canon = canonical(name)?;
    if canon == "copy-mode" {
        return Some("usage: copy-mode [-u] [-e]");
    }
    if copy_action_from_canonical(canon).is_some() {
        return Some("usage: copy-<action> (no arguments)");
    }
    Some(match canon {
        "split-window" => "usage: split-window [-h] [-v] [-t target]",
        "select-pane" => "usage: select-pane [-L] [-R] [-U] [-D] [-t target]",
        "select-window" => "usage: select-window -t target",
        "next-window" => "usage: next-window",
        "previous-window" => "usage: previous-window",
        "last-window" => "usage: last-window",
        "last-pane" => "usage: last-pane",
        "new-window" => "usage: new-window [-n name]",
        "kill-pane" => "usage: kill-pane [-t target]",
        "kill-window" => "usage: kill-window [-t target]",
        "resize-pane" => "usage: resize-pane [-L] [-R] [-U] [-D] [-Z] [count]",
        "rename-window" => "usage: rename-window [-t target] new-name",
        "rename-session" => "usage: rename-session [-t target] new-name",
        "detach-client" => "usage: detach-client -s target",
        "send-keys" => "usage: send-keys [-l] [-t target] key ...",
        "send-prefix" => "usage: send-prefix",
        "switch-client" => "usage: switch-client [-p] [-n]",
        "display-message" => "usage: display-message [text]",
        "confirm-before" => "usage: confirm-before [-p prompt] command ...",
        "command-prompt" => "usage: command-prompt [-I initial]",
        "set-option" => "usage: set-option [-g] [-w] [-a] [-u] option [value]",
        "show-options" => "usage: show-options [-g] [-w] [-q] [-v] [option]",
        "bind-key" => "usage: bind-key [-n] [-r] [-T table] key command ...",
        "unbind-key" => "usage: unbind-key [-a] [-n] [-T table] [key]",
        "list-keys" => "usage: list-keys",
        "source-file" => "usage: source-file path",
        "new-session" => "usage: new-session [-d] [-s name] [-x cols] [-y rows]",
        "attach-session" => "usage: attach-session [-d] [-t target]",
        "list-sessions" => "usage: list-sessions",
        "list-windows" => "usage: list-windows [-t target]",
        "has-session" => "usage: has-session -t target",
        "kill-session" => "usage: kill-session [-t target]",
        "kill-server" => "usage: kill-server",
        "paste-buffer" => "usage: paste-buffer [-p] [-r] [-b name] [-t target-pane]",
        "list-buffers" => "usage: list-buffers",
        "delete-buffer" => "usage: delete-buffer [-b name]",
        "set-buffer" => "usage: set-buffer [-b name] data",
        "select-layout" => "usage: select-layout [-t target] [layout-name]",
        "next-layout" => "usage: next-layout [-t target]",
        "swap-pane" => "usage: swap-pane [-U] [-D] [-s src] [-t dst]",
        "rotate-window" => "usage: rotate-window [-D] [-t target]",
        "break-pane" => "usage: break-pane [-d] [-n name]",
        "move-window" => "usage: move-window [-k] -t index",
        "swap-window" => "usage: swap-window [-d] [-s src] -t dst",
        "find-window" => "usage: find-window pattern",
        "choose-tree" => "usage: choose-tree [-s] [-w]",
        "display-panes" => "usage: display-panes [-d ms]",
        "clock-mode" => "usage: clock-mode",
        _ => unreachable!("canonical() and usage() command lists diverged"),
    })
}

/// Single-pass flag scanner shared by most `resolve()` arms: `bools` lists
/// no-value flags (e.g. `-h`), `values` lists flags that consume the next
/// token as their value (e.g. `-t`). Any other `-`-prefixed token (length >
/// 1) is `Err(())`. Returns `(bool flags seen, (flag, value) pairs seen,
/// positional tokens in order)`.
type ScanResult = (Vec<String>, Vec<(String, String)>, Vec<String>);

fn scan_flags(args: &[String], bools: &[&str], values: &[&str]) -> Result<ScanResult, ()> {
    let mut bool_hits = Vec::new();
    let mut value_hits = Vec::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();
        if tok.len() > 1 && tok.starts_with('-') {
            if bools.contains(&tok) {
                bool_hits.push(tok.to_string());
                i += 1;
            } else if values.contains(&tok) {
                i += 1;
                let v = args.get(i).ok_or(())?.clone();
                value_hits.push((tok.to_string(), v));
                i += 1;
            } else {
                return Err(());
            }
        } else {
            positional.push(tok.to_string());
            i += 1;
        }
    }
    Ok((bool_hits, value_hits, positional))
}

fn has(hits: &[String], flag: &str) -> bool {
    hits.iter().any(|h| h == flag)
}

fn value_of(hits: &[(String, String)], flag: &str) -> Option<String> {
    hits.iter().find(|(f, _)| f == flag).map(|(_, v)| v.clone())
}

/// Split a `bind-key`/`confirm-before` tail token list into `RawCmd`s on
/// every exact `;` token (whether that token reached here via an escaped
/// `\;` or an equivalent quoted `";"`/`';'` — both are indistinguishable
/// bare `;` tokens by the time `parse_line` is done, and tmux treats them
/// the same way here). Empty runs between separators (leading/trailing/
/// doubled `;`) are dropped, not emitted as empty commands.
fn split_tail(tokens: &[String]) -> Vec<RawCmd> {
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for t in tokens {
        if t == ";" {
            if let Some(name) = cur.first().cloned() {
                out.push(RawCmd { name, args: cur[1..].to_vec() });
            }
            cur.clear();
        } else {
            cur.push(t.clone());
        }
    }
    if let Some(name) = cur.first().cloned() {
        out.push(RawCmd { name, args: cur[1..].to_vec() });
    }
    out
}

fn direction_of(hits: &[String]) -> Option<Direction> {
    if has(hits, "-L") {
        Some(Direction::Left)
    } else if has(hits, "-R") {
        Some(Direction::Right)
    } else if has(hits, "-U") {
        Some(Direction::Up)
    } else if has(hits, "-D") {
        Some(Direction::Down)
    } else {
        None
    }
}

/// Resolve one [`RawCmd`] (full name or alias) into a typed [`ParsedCmd`] via
/// the command table. `Err("unknown command: <name>")` for an unrecognized
/// name; `Err("usage: <usage line>")` for a bad/missing flag or argument —
/// see [`usage`] for the exact per-command strings (several are verbatim
/// copies of SP2's `cli_exec.rs` usage constants; see that fn's doc comment).
pub fn resolve(raw: &RawCmd) -> Result<ParsedCmd, String> {
    let Some(canon) = canonical(&raw.name) else {
        return Err(format!("unknown command: {}", raw.name));
    };
    let usage_str = usage(canon).expect("canonical() and usage() command lists must agree");
    let bad = || usage_str.to_string();

    if canon == "copy-mode" {
        let Ok((b, _, p)) = scan_flags(&raw.args, &["-u", "-e"], &[]) else { return Err(bad()) };
        if !p.is_empty() {
            return Err(bad());
        }
        return Ok(ParsedCmd::CopyMode { page_up: has(&b, "-u"), mouse: has(&b, "-e") });
    }
    if let Some(action) = copy_action_from_canonical(canon) {
        if !raw.args.is_empty() {
            return Err(bad());
        }
        return Ok(ParsedCmd::CopyCmd(action));
    }

    match canon {
        "split-window" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-h", "-v"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SplitWindow { horizontal: has(&b, "-h"), target: value_of(&v, "-t") })
        }
        "select-pane" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-L", "-R", "-U", "-D"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SelectPane { dir: direction_of(&b), target: value_of(&v, "-t") })
        }
        "select-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let Some(target) = value_of(&v, "-t") else { return Err(bad()) };
            Ok(ParsedCmd::SelectWindow { target })
        }
        "next-window" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::NextWindow)
        }
        "previous-window" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::PreviousWindow)
        }
        "last-window" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::LastWindow)
        }
        "last-pane" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::LastPane)
        }
        "new-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-n"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::NewWindow { name: value_of(&v, "-n") })
        }
        "kill-pane" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillPane { target: value_of(&v, "-t") })
        }
        "kill-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillWindow { target: value_of(&v, "-t") })
        }
        "resize-pane" => {
            let Ok((b, _, p)) = scan_flags(&raw.args, &["-L", "-R", "-U", "-D", "-Z"], &[]) else { return Err(bad()) };
            if p.len() > 1 {
                return Err(bad());
            }
            let count = match p.first() {
                Some(s) => s.parse::<i32>().map_err(|_| bad())?,
                None => 1,
            };
            Ok(ParsedCmd::ResizePane { dir: direction_of(&b), zoom: has(&b, "-Z"), count })
        }
        "rename-window" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if p.len() != 1 {
                return Err(bad());
            }
            Ok(ParsedCmd::RenameWindow { target: value_of(&v, "-t"), name: p[0].clone() })
        }
        "rename-session" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if p.len() != 1 {
                return Err(bad());
            }
            Ok(ParsedCmd::RenameSession { target: value_of(&v, "-t"), name: p[0].clone() })
        }
        "detach-client" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-s"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            // -s is optional at the parse level: bare `detach-client`
            // detaches the acting client (tmux behavior; the default
            // prefix-d binding relies on this). The Task 6 dispatcher -- not
            // resolve -- rejects the bare form when no client context
            // exists, with the SP2 verbatim usage error.
            Ok(ParsedCmd::DetachClient { target: value_of(&v, "-s") })
        }
        "send-keys" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-l", "-X"], &["-t"]) else { return Err(bad()) };
            if has(&b, "-X") {
                // tmux `send-keys -X <name>` spelling for a copy-mode
                // command: the first positional arg is the -X command name,
                // mapped to the internal `copy-*` command it aliases (see
                // `copy_action_from_x_name`). Whether the acting client is
                // actually IN copy mode is a dispatch-time (not parse-time)
                // concern — see the `## copy-mode` contract section.
                let Some(xname) = p.first() else { return Err(bad()) };
                let Some(action) = copy_action_from_x_name(xname) else {
                    return Err(format!("unknown -X command: {xname}"));
                };
                return Ok(ParsedCmd::CopyCmd(action));
            }
            if p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SendKeys { literal: has(&b, "-l"), target: value_of(&v, "-t"), keys: p })
        }
        "send-prefix" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SendPrefix)
        }
        "switch-client" => {
            let Ok((b, _, p)) = scan_flags(&raw.args, &["-p", "-n"], &[]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let has_p = has(&b, "-p");
            let has_n = has(&b, "-n");
            if has_p == has_n {
                // Neither given, or both given: SP3 requires exactly one.
                return Err(bad());
            }
            Ok(ParsedCmd::SwitchClient { next: has_n })
        }
        "display-message" => {
            if raw.args.is_empty() {
                Ok(ParsedCmd::DisplayMessage { text: None })
            } else {
                Ok(ParsedCmd::DisplayMessage { text: Some(raw.args.join(" ")) })
            }
        }
        "confirm-before" => {
            let mut prompt: Option<String> = None;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-p" => {
                        i += 1;
                        let Some(p) = raw.args.get(i) else { return Err(bad()) };
                        prompt = Some(p.clone());
                        i += 1;
                    }
                    _ => break,
                }
            }
            let tail_tokens = &raw.args[i..];
            if tail_tokens.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ConfirmBefore { prompt, tail: split_tail(tail_tokens) })
        }
        "command-prompt" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-I"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::CommandPrompt { initial: value_of(&v, "-I") })
        }
        "set-option" => {
            // `setw`/`set-window-option` imply `-w` even with no explicit
            // flag -- `canonical()` collapses both spellings onto
            // "set-option", so the original command word (`raw.name`, not
            // `canon`) is the only place left to recover that implication.
            let mut global = false;
            let mut window = matches!(raw.name.as_str(), "setw" | "set-window-option");
            let mut append = false;
            let mut unset = false;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-g" => {
                        global = true;
                        i += 1;
                    }
                    "-w" => {
                        window = true;
                        i += 1;
                    }
                    "-a" => {
                        append = true;
                        i += 1;
                    }
                    "-u" => {
                        unset = true;
                        i += 1;
                    }
                    _ => break,
                }
            }
            let rest = &raw.args[i..];
            let Some((name, value_tokens)) = rest.split_first() else { return Err(bad()) };
            let value = if value_tokens.is_empty() { None } else { Some(value_tokens.join(" ")) };
            Ok(ParsedCmd::SetOption { global, window, append, unset, name: name.clone(), value })
        }
        "show-options" => {
            // All four flags are boolean-only (no value-taking flag in this
            // command), so unlike `scan_flags` (shared by commands that DO
            // mix bool/value flags, and therefore never split a clump —
            // see its doc comment) this arm can safely support real tmux
            // clump bundling (`-gqv` == `-g -q -v`, `arguments.c:205-252`)
            // with a plain per-character scan: the exact TPM rung-1 idiom
            // `show -gqv "@foo"` (#68) parses. `-w` mirrors `set-option`'s
            // "table decides scope, flag only picks global-vs-local within
            // it" rule, and `showw`/`show-window-options` imply it exactly
            // like `setw`/`set-window-option` do for `set-option` (see that
            // arm's comment) -- recovered from `raw.name` since
            // `canonical()` already collapsed both spellings onto
            // "show-options".
            let mut global = false;
            let mut window = matches!(raw.name.as_str(), "showw" | "show-window-options");
            let mut quiet = false;
            let mut value_only = false;
            let mut positional = Vec::new();
            for tok in &raw.args {
                if tok.len() > 1 && tok.starts_with('-') {
                    for c in tok[1..].chars() {
                        match c {
                            'g' => global = true,
                            'w' => window = true,
                            'q' => quiet = true,
                            'v' => value_only = true,
                            _ => return Err(bad()),
                        }
                    }
                } else {
                    positional.push(tok.clone());
                }
            }
            if positional.len() > 1 {
                return Err(bad());
            }
            Ok(ParsedCmd::ShowOptions { global, window, quiet, value_only, name: positional.into_iter().next() })
        }
        "bind-key" => {
            let mut table: Option<String> = None;
            let mut repeat = false;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-n" => {
                        table = Some("root".to_string());
                        i += 1;
                    }
                    "-r" => {
                        repeat = true;
                        i += 1;
                    }
                    "-T" => {
                        i += 1;
                        let Some(t) = raw.args.get(i) else { return Err(bad()) };
                        if !matches!(t.as_str(), "root" | "prefix" | "copy-mode" | "copy-mode-vi") {
                            return Err(format!("unknown key table: {t}"));
                        }
                        table = Some(t.clone());
                        i += 1;
                    }
                    _ => break,
                }
            }
            let Some(key) = raw.args.get(i) else { return Err(bad()) };
            let tail_tokens = &raw.args[i + 1..];
            if tail_tokens.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::BindKey {
                table: table.unwrap_or_else(|| "prefix".to_string()),
                repeat,
                key: key.clone(),
                tail: split_tail(tail_tokens),
            })
        }
        "unbind-key" => {
            let mut all = false;
            let mut table: Option<String> = None;
            let mut i = 0;
            while i < raw.args.len() {
                match raw.args[i].as_str() {
                    "-a" => {
                        all = true;
                        i += 1;
                    }
                    "-n" => {
                        table = Some("root".to_string());
                        i += 1;
                    }
                    "-T" => {
                        i += 1;
                        let Some(t) = raw.args.get(i) else { return Err(bad()) };
                        if !matches!(t.as_str(), "root" | "prefix" | "copy-mode" | "copy-mode-vi") {
                            return Err(format!("unknown key table: {t}"));
                        }
                        table = Some(t.clone());
                        i += 1;
                    }
                    _ => break,
                }
            }
            let rest = &raw.args[i..];
            if rest.len() > 1 {
                return Err(bad());
            }
            let key = rest.first().cloned();
            if !all && key.is_none() {
                return Err(bad());
            }
            Ok(ParsedCmd::UnbindKey { all, table: table.unwrap_or_else(|| "prefix".to_string()), key })
        }
        "list-keys" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ListKeys)
        }
        "source-file" => match raw.args.as_slice() {
            [path] => Ok(ParsedCmd::SourceFile { path: path.clone() }),
            _ => Err(bad()),
        },
        "new-session" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-d"], &["-s", "-x", "-y"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let cols = match value_of(&v, "-x") {
                Some(s) => Some(s.parse::<u16>().map_err(|_| bad())?),
                None => None,
            };
            let rows = match value_of(&v, "-y") {
                Some(s) => Some(s.parse::<u16>().map_err(|_| bad())?),
                None => None,
            };
            Ok(ParsedCmd::NewSession { detached: has(&b, "-d"), name: value_of(&v, "-s"), cols, rows })
        }
        "attach-session" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-d"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::AttachSession { target: value_of(&v, "-t"), detach_others: has(&b, "-d") })
        }
        "list-sessions" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ListSessions)
        }
        "list-windows" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ListWindows { target: value_of(&v, "-t") })
        }
        "has-session" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let Some(target) = value_of(&v, "-t") else { return Err(bad()) };
            Ok(ParsedCmd::HasSession { target })
        }
        "kill-session" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillSession { target: value_of(&v, "-t") })
        }
        "kill-server" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::KillServer)
        }
        "paste-buffer" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-p", "-r"], &["-b", "-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::PasteBuffer { name: value_of(&v, "-b"), target: value_of(&v, "-t"), no_replace: has(&b, "-r") })
        }
        "list-buffers" => {
            if !raw.args.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ListBuffers)
        }
        "delete-buffer" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-b"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::DeleteBuffer { name: value_of(&v, "-b") })
        }
        "set-buffer" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-b"]) else { return Err(bad()) };
            if p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SetBuffer { name: value_of(&v, "-b"), data: p.join(" ") })
        }
        "select-layout" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if p.len() > 1 {
                return Err(bad());
            }
            let name = p.into_iter().next();
            if let Some(n) = &name {
                if !matches!(n.as_str(), "even-horizontal" | "even-vertical" | "main-horizontal" | "main-vertical" | "tiled") {
                    return Err(format!("unknown layout: {n}"));
                }
            }
            Ok(ParsedCmd::SelectLayout { target: value_of(&v, "-t"), name })
        }
        "next-layout" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::NextLayout { target: value_of(&v, "-t") })
        }
        "swap-pane" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-U", "-D"], &["-s", "-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::SwapPane { dir: direction_of(&b), src: value_of(&v, "-s"), dst: value_of(&v, "-t") })
        }
        "rotate-window" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-D"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::RotateWindow { down: has(&b, "-D"), target: value_of(&v, "-t") })
        }
        "break-pane" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-d"], &["-n"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::BreakPane { detached: has(&b, "-d"), name: value_of(&v, "-n") })
        }
        "move-window" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-k"], &["-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let Some(target) = value_of(&v, "-t") else { return Err(bad()) };
            Ok(ParsedCmd::MoveWindow { kill: has(&b, "-k"), target })
        }
        "swap-window" => {
            let Ok((b, v, p)) = scan_flags(&raw.args, &["-d"], &["-s", "-t"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let Some(dst) = value_of(&v, "-t") else { return Err(bad()) };
            Ok(ParsedCmd::SwapWindow { src: value_of(&v, "-s"), dst: Some(dst), detach: has(&b, "-d") })
        }
        "find-window" => {
            let Ok((_, _, p)) = scan_flags(&raw.args, &[], &[]) else { return Err(bad()) };
            if p.len() != 1 {
                return Err(bad());
            }
            Ok(ParsedCmd::FindWindow { pattern: p[0].clone() })
        }
        "choose-tree" => {
            let Ok((b, _, p)) = scan_flags(&raw.args, &["-s", "-w"], &[]) else { return Err(bad()) };
            if !p.is_empty() || (has(&b, "-s") && has(&b, "-w")) {
                return Err(bad());
            }
            Ok(ParsedCmd::ChooseTree { sessions: has(&b, "-s") })
        }
        "display-panes" => {
            let Ok((_, v, p)) = scan_flags(&raw.args, &[], &["-d"]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            let ms = match value_of(&v, "-d") {
                Some(s) => Some(s.parse::<u32>().map_err(|_| bad())?),
                None => None,
            };
            Ok(ParsedCmd::DisplayPanes { ms })
        }
        "clock-mode" => {
            let Ok((_, _, p)) = scan_flags(&raw.args, &[], &[]) else { return Err(bad()) };
            if !p.is_empty() {
                return Err(bad());
            }
            Ok(ParsedCmd::ClockMode)
        }
        _ => unreachable!("canonical() and resolve() command lists diverged"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, args: &[&str]) -> RawCmd {
        RawCmd { name: name.to_string(), args: args.iter().map(|s| s.to_string()).collect() }
    }

    // ---- tokenizer ----

    #[test]
    fn plain_split() {
        assert_eq!(parse_line("split-window -h -t work").unwrap(), vec![raw("split-window", &["-h", "-t", "work"])]);
    }

    #[test]
    fn single_quotes_literal() {
        // '#' and ';' inside single quotes are literal, not comment/separator.
        assert_eq!(parse_line("echo 'a # b ; c' rest").unwrap(), vec![raw("echo", &["a # b ; c", "rest"])]);
    }

    #[test]
    fn double_quotes_escapes() {
        assert_eq!(parse_line(r#"echo "a\"b\\c""#).unwrap(), vec![raw("echo", &[r#"a"b\c"#])]);
    }

    #[test]
    fn double_quotes_other_backslash_is_literal() {
        // \n inside double quotes is NOT a newline escape -- backslash and
        // the following char both survive verbatim (only \" and \\ are
        // recognized escapes).
        assert_eq!(parse_line(r#"echo "a\nb""#).unwrap(), vec![raw("echo", &[r"a\nb"])]);
    }

    #[test]
    fn comment_strips() {
        assert_eq!(parse_line("set -g status off # trailing").unwrap(), vec![raw("set", &["-g", "status", "off"])]);
    }

    #[test]
    fn comment_strips_mid_token() {
        assert_eq!(parse_line("foo#bar").unwrap(), vec![raw("foo", &[])]);
    }

    #[test]
    fn semicolon_splits_commands() {
        assert_eq!(
            parse_line("kill-pane ; display-message hi").unwrap(),
            vec![raw("kill-pane", &[]), raw("display-message", &["hi"])]
        );
    }

    #[test]
    fn escaped_semicolon_is_arg() {
        let cmds = parse_line(r"bind x kill-pane \; display ok").unwrap();
        assert_eq!(cmds, vec![raw("bind", &["x", "kill-pane", ";", "display", "ok"])]);
        // ... and resolve()'s tail-splitting turns it into 2 RawCmds.
        let ParsedCmd::BindKey { tail, .. } = resolve(&cmds[0]).unwrap() else { panic!("expected BindKey") };
        assert_eq!(tail, vec![raw("kill-pane", &[]), raw("display", &["ok"])]);
    }

    #[test]
    fn quoted_semicolon_also_splits_tail() {
        let cmds = parse_line(r#"bind x kill-pane ";" display ok"#).unwrap();
        let ParsedCmd::BindKey { tail, .. } = resolve(&cmds[0]).unwrap() else { panic!("expected BindKey") };
        assert_eq!(tail, vec![raw("kill-pane", &[]), raw("display", &["ok"])]);
    }

    #[test]
    fn unterminated_quote_err() {
        assert!(parse_line("\"foo").unwrap_err().contains("unterminated quote"));
        assert!(parse_line("'foo").unwrap_err().contains("unterminated quote"));
    }

    #[test]
    fn blank_and_comment_only_lines_are_empty() {
        assert_eq!(parse_line("").unwrap(), vec![]);
        assert_eq!(parse_line("   ").unwrap(), vec![]);
        assert_eq!(parse_line("# just a comment").unwrap(), vec![]);
        assert_eq!(parse_line("a ; ; b").unwrap(), vec![raw("a", &[]), raw("b", &[])]);
    }

    #[test]
    fn adjacent_quote_concatenation() {
        assert_eq!(parse_line(r#"foo'bar'"baz""#).unwrap(), vec![raw("foobarbaz", &[])]);
    }

    // ---- join_continuations ----

    #[test]
    fn join_continuations_passthrough() {
        assert_eq!(
            join_continuations(["a", "b"].into_iter()),
            vec![(1, "a".to_string()), (2, "b".to_string())]
        );
    }

    #[test]
    fn join_continuations_basic() {
        assert_eq!(join_continuations([r"foo \", "bar"].into_iter()), vec![(1, "foo bar".to_string())]);
    }

    #[test]
    fn join_continuations_chain_tracks_first_line_number() {
        assert_eq!(
            join_continuations(["x", r"a \", r"b \", "c"].into_iter()),
            vec![(1, "x".to_string()), (2, "a b c".to_string())]
        );
    }

    #[test]
    fn join_continuations_trailing_backslash_at_eof_kept() {
        assert_eq!(join_continuations([r"foo\"].into_iter()), vec![(1, r"foo\".to_string())]);
    }

    #[test]
    fn join_continuations_strips_crlf() {
        assert_eq!(join_continuations(["foo \\\r", "bar\r"].into_iter()), vec![(1, "foo bar".to_string())]);
    }

    // ---- resolve: aliases ----

    #[test]
    fn aliases_resolve() {
        assert_eq!(
            resolve(&raw("splitw", &["-h"])).unwrap(),
            resolve(&raw("split-window", &["-h"])).unwrap()
        );
        assert_eq!(
            resolve(&raw("set", &["-g", "status", "off"])).unwrap(),
            resolve(&raw("set-option", &["-g", "status", "off"])).unwrap()
        );
    }

    #[test]
    fn split_window_flags() {
        assert_eq!(
            resolve(&raw("split-window", &["-h", "-t", "work"])).unwrap(),
            ParsedCmd::SplitWindow { horizontal: true, target: Some("work".to_string()) }
        );
        assert_eq!(
            resolve(&raw("split-window", &["-v"])).unwrap(),
            ParsedCmd::SplitWindow { horizontal: false, target: None }
        );
    }

    #[test]
    fn send_keys_literal_flag() {
        assert_eq!(
            resolve(&raw("send-keys", &["-l", "-t", "work", "hello"])).unwrap(),
            ParsedCmd::SendKeys { literal: true, target: Some("work".to_string()), keys: vec!["hello".to_string()] }
        );
    }

    #[test]
    fn switch_client_prev_and_next() {
        assert_eq!(resolve(&raw("switch-client", &["-p"])).unwrap(), ParsedCmd::SwitchClient { next: false });
        assert_eq!(resolve(&raw("switchc", &["-n"])).unwrap(), ParsedCmd::SwitchClient { next: true });
    }

    #[test]
    fn switch_client_requires_exactly_one_of_p_n() {
        assert_eq!(resolve(&raw("switch-client", &[])), Err(usage("switch-client").unwrap().to_string()));
        assert_eq!(resolve(&raw("switch-client", &["-p", "-n"])), Err(usage("switch-client").unwrap().to_string()));
    }

    #[test]
    fn switch_client_dash_l_is_usage_error() {
        // SP3 deviation: -l ("last session") is not supported; it hits the
        // same usage: error as any other unrecognized flag.
        assert_eq!(resolve(&raw("switch-client", &["-l"])), Err(usage("switch-client").unwrap().to_string()));
    }

    #[test]
    fn send_keys_requires_a_key() {
        assert_eq!(resolve(&raw("send-keys", &["-t", "work"])).unwrap_err(), usage("send-keys").unwrap());
    }

    #[test]
    fn set_option_flags() {
        assert_eq!(
            resolve(&raw("set-option", &["-g", "-a", "status-left", "[#S] "])).unwrap(),
            ParsedCmd::SetOption {
                global: true,
                window: false,
                append: true,
                unset: false,
                name: "status-left".to_string(),
                value: Some("[#S] ".to_string()),
            }
        );
        // Multiple bare value tokens are joined with single spaces.
        assert_eq!(
            resolve(&raw("set", &["-g", "status-left", "a", "b", "c"])).unwrap(),
            ParsedCmd::SetOption {
                global: true,
                window: false,
                append: false,
                unset: false,
                name: "status-left".to_string(),
                value: Some("a b c".to_string()),
            }
        );
        // No value at all is allowed (semantics decided by `options` later).
        assert_eq!(
            resolve(&raw("set", &["-u", "mouse"])).unwrap(),
            ParsedCmd::SetOption {
                global: false,
                window: false,
                append: false,
                unset: true,
                name: "mouse".to_string(),
                value: None,
            }
        );
    }

    #[test]
    fn bind_key_full() {
        assert_eq!(
            resolve(&raw("bind", &["-r", "-T", "prefix", "C-Up", "resize-pane", "-U"])).unwrap(),
            ParsedCmd::BindKey {
                table: "prefix".to_string(),
                repeat: true,
                key: "C-Up".to_string(),
                tail: vec![raw("resize-pane", &["-U"])],
            }
        );
    }

    #[test]
    fn bind_key_dash_n_is_root_table() {
        let ParsedCmd::BindKey { table, .. } = resolve(&raw("bind", &["-n", "F5", "select-pane", "-U"])).unwrap()
        else {
            panic!("expected BindKey")
        };
        assert_eq!(table, "root");
    }

    #[test]
    fn bind_key_bad_table_errs() {
        assert_eq!(
            resolve(&raw("bind", &["-T", "other", "x", "kill-pane"])).unwrap_err(),
            "unknown key table: other"
        );
    }

    #[test]
    fn unbind_key_table_validation() {
        assert_eq!(
            resolve(&raw("unbind-key", &["-T", "custom", "x"])).unwrap_err(),
            "unknown key table: custom"
        );
        assert_eq!(
            resolve(&raw("unbind-key", &["-a"])).unwrap(),
            ParsedCmd::UnbindKey { all: true, table: "prefix".to_string(), key: None }
        );
        assert_eq!(resolve(&raw("unbind-key", &[])).unwrap_err(), usage("unbind-key").unwrap());
    }

    #[test]
    fn confirm_before_tail() {
        assert_eq!(
            resolve(&raw("confirm", &["-p", "kill-pane #P? (y/n)", "kill-pane"])).unwrap(),
            ParsedCmd::ConfirmBefore {
                prompt: Some("kill-pane #P? (y/n)".to_string()),
                tail: vec![raw("kill-pane", &[])],
            }
        );
    }

    #[test]
    fn resize_pane_defaults_and_errors() {
        assert_eq!(
            resolve(&raw("resize-pane", &["-U"])).unwrap(),
            ParsedCmd::ResizePane { dir: Some(Direction::Up), zoom: false, count: 1 }
        );
        assert_eq!(
            resolve(&raw("resize-pane", &["-L", "5"])).unwrap(),
            ParsedCmd::ResizePane { dir: Some(Direction::Left), zoom: false, count: 5 }
        );
        assert_eq!(resolve(&raw("resize-pane", &["-L", "abc"])).unwrap_err(), usage("resize-pane").unwrap());
    }

    // ---- layout presets, swap-pane, rotate-window (Task 6, sub-project 4) --

    #[test]
    fn select_layout_flags_and_name_validation() {
        assert_eq!(
            resolve(&raw("select-layout", &["main-vertical"])).unwrap(),
            ParsedCmd::SelectLayout { target: None, name: Some("main-vertical".to_string()) }
        );
        assert_eq!(
            resolve(&raw("selectl", &["-t", "work", "tiled"])).unwrap(),
            ParsedCmd::SelectLayout { target: Some("work".to_string()), name: Some("tiled".to_string()) }
        );
        assert_eq!(resolve(&raw("select-layout", &[])).unwrap(), ParsedCmd::SelectLayout { target: None, name: None });
        assert_eq!(
            resolve(&raw("select-layout", &["bogus"])).unwrap_err(),
            "unknown layout: bogus"
        );
        assert_eq!(resolve(&raw("select-layout", &["a", "b"])).unwrap_err(), usage("select-layout").unwrap());
    }

    #[test]
    fn next_layout_no_args() {
        assert_eq!(resolve(&raw("next-layout", &[])).unwrap(), ParsedCmd::NextLayout { target: None });
        assert_eq!(resolve(&raw("nextl", &["-t", "s"])).unwrap(), ParsedCmd::NextLayout { target: Some("s".to_string()) });
        assert_eq!(resolve(&raw("next-layout", &["extra"])).unwrap_err(), usage("next-layout").unwrap());
    }

    #[test]
    fn swap_pane_flags() {
        assert_eq!(
            resolve(&raw("swap-pane", &["-U"])).unwrap(),
            ParsedCmd::SwapPane { dir: Some(Direction::Up), src: None, dst: None }
        );
        assert_eq!(
            resolve(&raw("swapp", &["-D"])).unwrap(),
            ParsedCmd::SwapPane { dir: Some(Direction::Down), src: None, dst: None }
        );
        assert_eq!(
            resolve(&raw("swap-pane", &["-s", "0", "-t", "1"])).unwrap(),
            ParsedCmd::SwapPane { dir: None, src: Some("0".to_string()), dst: Some("1".to_string()) }
        );
        assert_eq!(resolve(&raw("swap-pane", &["-L"])).unwrap_err(), usage("swap-pane").unwrap());
    }

    #[test]
    fn rotate_window_flags() {
        assert_eq!(resolve(&raw("rotate-window", &[])).unwrap(), ParsedCmd::RotateWindow { down: false, target: None });
        assert_eq!(
            resolve(&raw("rotatew", &["-D"])).unwrap(),
            ParsedCmd::RotateWindow { down: true, target: None }
        );
        assert_eq!(
            resolve(&raw("rotate-window", &["-t", "work"])).unwrap(),
            ParsedCmd::RotateWindow { down: false, target: Some("work".to_string()) }
        );
    }

    // ---- window ops (Task 7, sub-project 4) --------------------------------

    #[test]
    fn break_pane_flags() {
        assert_eq!(resolve(&raw("break-pane", &[])).unwrap(), ParsedCmd::BreakPane { detached: false, name: None });
        assert_eq!(
            resolve(&raw("breakp", &["-d"])).unwrap(),
            ParsedCmd::BreakPane { detached: true, name: None }
        );
        assert_eq!(
            resolve(&raw("break-pane", &["-n", "logs"])).unwrap(),
            ParsedCmd::BreakPane { detached: false, name: Some("logs".to_string()) }
        );
        assert_eq!(
            resolve(&raw("break-pane", &["-d", "-n", "logs"])).unwrap(),
            ParsedCmd::BreakPane { detached: true, name: Some("logs".to_string()) }
        );
        assert_eq!(resolve(&raw("break-pane", &["extra"])).unwrap_err(), usage("break-pane").unwrap());
    }

    #[test]
    fn move_window_flags() {
        assert_eq!(
            resolve(&raw("move-window", &["-t", "5"])).unwrap(),
            ParsedCmd::MoveWindow { kill: false, target: "5".to_string() }
        );
        assert_eq!(
            resolve(&raw("movew", &["-k", "-t", "3"])).unwrap(),
            ParsedCmd::MoveWindow { kill: true, target: "3".to_string() }
        );
        // -t is required -- there's nothing for a bare move-window to do.
        assert_eq!(resolve(&raw("move-window", &[])).unwrap_err(), usage("move-window").unwrap());
    }

    #[test]
    fn swap_window_flags() {
        // The user's real config binding: `bind -r "<" swap-window -d -t -1`.
        assert_eq!(
            resolve(&raw("swap-window", &["-d", "-t", "-1"])).unwrap(),
            ParsedCmd::SwapWindow { src: None, dst: Some("-1".to_string()), detach: true }
        );
        // `bind -r ">" swap-window -d -t +1`.
        assert_eq!(
            resolve(&raw("swapw", &["-d", "-t", "+1"])).unwrap(),
            ParsedCmd::SwapWindow { src: None, dst: Some("+1".to_string()), detach: true }
        );
        // Explicit -s/-t absolute-index targets, no -d.
        assert_eq!(
            resolve(&raw("swap-window", &["-s", ":2", "-t", ":4"])).unwrap(),
            ParsedCmd::SwapWindow { src: Some(":2".to_string()), dst: Some(":4".to_string()), detach: false }
        );
        // -t is required -- there's nothing for a bare swap-window to do.
        assert_eq!(resolve(&raw("swap-window", &[])).unwrap_err(), usage("swap-window").unwrap());
        assert_eq!(resolve(&raw("swap-window", &["-s", "0"])).unwrap_err(), usage("swap-window").unwrap());
    }

    #[test]
    fn find_window_pattern() {
        assert_eq!(
            resolve(&raw("find-window", &["logs"])).unwrap(),
            ParsedCmd::FindWindow { pattern: "logs".to_string() }
        );
        assert_eq!(
            resolve(&raw("findw", &["with space"])).unwrap(),
            ParsedCmd::FindWindow { pattern: "with space".to_string() }
        );
        assert_eq!(resolve(&raw("find-window", &[])).unwrap_err(), usage("find-window").unwrap());
        assert_eq!(resolve(&raw("find-window", &["a", "b"])).unwrap_err(), usage("find-window").unwrap());
    }

    // ---- overlays (Task 8, sub-project 4) ----------------------------------

    #[test]
    fn choose_tree_flags() {
        assert_eq!(resolve(&raw("choose-tree", &[])).unwrap(), ParsedCmd::ChooseTree { sessions: false });
        assert_eq!(resolve(&raw("choose-tree", &["-w"])).unwrap(), ParsedCmd::ChooseTree { sessions: false });
        assert_eq!(resolve(&raw("choosetree", &["-s"])).unwrap(), ParsedCmd::ChooseTree { sessions: true });
        assert_eq!(resolve(&raw("choose-tree", &["-s", "-w"])).unwrap_err(), usage("choose-tree").unwrap());
        assert_eq!(resolve(&raw("choose-tree", &["extra"])).unwrap_err(), usage("choose-tree").unwrap());
    }

    #[test]
    fn display_panes_flags() {
        assert_eq!(resolve(&raw("display-panes", &[])).unwrap(), ParsedCmd::DisplayPanes { ms: None });
        assert_eq!(
            resolve(&raw("displayp", &["-d", "200"])).unwrap(),
            ParsedCmd::DisplayPanes { ms: Some(200) }
        );
        assert_eq!(resolve(&raw("display-panes", &["-d", "nope"])).unwrap_err(), usage("display-panes").unwrap());
        assert_eq!(resolve(&raw("display-panes", &["extra"])).unwrap_err(), usage("display-panes").unwrap());
    }

    /// Task 10 (clock-mode, sub-project 6 wave 2): bare `clock-mode` parses
    /// to the zero-field `ClockMode` variant; any argument (winmux does not
    /// implement real tmux's `-t target-pane`, see the variant's doc
    /// comment) is a usage error.
    #[test]
    fn clock_mode_no_args() {
        assert_eq!(resolve(&raw("clock-mode", &[])).unwrap(), ParsedCmd::ClockMode);
        assert_eq!(resolve(&raw("clock-mode", &["-t", "1"])).unwrap_err(), usage("clock-mode").unwrap());
        assert_eq!(resolve(&raw("clock-mode", &["extra"])).unwrap_err(), usage("clock-mode").unwrap());
        assert_eq!(usage("clock-mode").unwrap(), "usage: clock-mode");
    }

    #[test]
    fn unknown_command_err_exact() {
        assert_eq!(resolve(&raw("bogus-cmd", &[])).unwrap_err(), "unknown command: bogus-cmd");
    }

    #[test]
    fn usage_err_on_bad_flag() {
        assert_eq!(
            resolve(&raw("rename-session", &["-q", "foo"])).unwrap_err(),
            "usage: rename-session [-t target] new-name"
        );
    }

    #[test]
    fn sp2_commands_present() {
        assert_eq!(
            resolve(&raw("new-session", &["-s", "work"])).unwrap(),
            ParsedCmd::NewSession { detached: false, name: Some("work".to_string()), cols: None, rows: None }
        );
        assert_eq!(resolve(&raw("ls", &[])).unwrap(), ParsedCmd::ListSessions);
        assert_eq!(resolve(&raw("kill-server", &[])).unwrap(), ParsedCmd::KillServer);
        assert_eq!(
            resolve(&raw("has-session", &["-t", "work"])).unwrap(),
            ParsedCmd::HasSession { target: "work".to_string() }
        );
        assert_eq!(
            resolve(&raw("detach-client", &["-s", "work"])).unwrap(),
            ParsedCmd::DetachClient { target: Some("work".to_string()) }
        );
    }

    #[test]
    fn detach_client_bare_is_current_client() {
        // Bare `detach-client` (tmux: detach the acting client) resolves;
        // the "no client context" rejection is the dispatcher's job (Task
        // 6), not resolve's. The default prefix-d binding depends on this.
        assert_eq!(resolve(&raw("detach-client", &[])).unwrap(), ParsedCmd::DetachClient { target: None });
    }

    #[test]
    fn sp2_usage_strings_match_cli_exec_verbatim() {
        // These MUST stay byte-for-byte identical to src/server/cli_exec.rs's
        // USAGE_* constants (contract requirement, SP2 test parity).
        assert_eq!(usage("list-sessions").unwrap(), "usage: list-sessions");
        assert_eq!(usage("has-session").unwrap(), "usage: has-session -t target");
        assert_eq!(usage("kill-session").unwrap(), "usage: kill-session [-t target]");
        assert_eq!(usage("kill-server").unwrap(), "usage: kill-server");
        assert_eq!(usage("new-session").unwrap(), "usage: new-session [-d] [-s name] [-x cols] [-y rows]");
        assert_eq!(usage("rename-session").unwrap(), "usage: rename-session [-t target] new-name");
        assert_eq!(usage("rename-window").unwrap(), "usage: rename-window [-t target] new-name");
        assert_eq!(usage("list-windows").unwrap(), "usage: list-windows [-t target]");
        assert_eq!(usage("detach-client").unwrap(), "usage: detach-client -s target");
    }

    #[test]
    fn usage_lookup_by_alias_and_unknown() {
        assert_eq!(usage("splitw"), usage("split-window"));
        assert_eq!(usage("nonexistent-command"), None);
    }

    #[test]
    fn rename_window_requires_name() {
        assert_eq!(
            resolve(&raw("rename-window", &["-t", "foo"])).unwrap_err(),
            usage("rename-window").unwrap()
        );
        assert_eq!(
            resolve(&raw("renamew", &["newname"])).unwrap(),
            ParsedCmd::RenameWindow { target: None, name: "newname".to_string() }
        );
    }

    // ---- copy-mode (Task 2, sub-project 4) ----

    #[test]
    fn copy_mode_flags() {
        assert_eq!(resolve(&raw("copy-mode", &[])).unwrap(), ParsedCmd::CopyMode { page_up: false, mouse: false });
        assert_eq!(
            resolve(&raw("copy-mode", &["-u"])).unwrap(),
            ParsedCmd::CopyMode { page_up: true, mouse: false }
        );
        assert_eq!(
            resolve(&raw("copy-mode", &["-u", "-e"])).unwrap(),
            ParsedCmd::CopyMode { page_up: true, mouse: true }
        );
        assert_eq!(resolve(&raw("copy-mode", &["bogus"])).unwrap_err(), usage("copy-mode").unwrap());
    }

    #[test]
    fn copy_action_commands_resolve() {
        assert_eq!(resolve(&raw("copy-cursor-left", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::CursorLeft));
        assert_eq!(resolve(&raw("copy-cancel", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::Cancel));
        assert_eq!(
            resolve(&raw("copy-history-bottom", &[])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::HistoryBottom)
        );
        // No arguments accepted.
        assert_eq!(resolve(&raw("copy-cancel", &["x"])).unwrap_err(), usage("copy-cancel").unwrap());
    }

    #[test]
    fn send_keys_dash_x_maps_to_copy_action() {
        assert_eq!(resolve(&raw("send-keys", &["-X", "cancel"])).unwrap(), ParsedCmd::CopyCmd(CopyAction::Cancel));
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "cursor-left"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::CursorLeft)
        );
        assert_eq!(
            resolve(&raw("send", &["-X", "history-top"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::HistoryTop)
        );
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "bogus"])).unwrap_err(),
            "unknown -X command: bogus"
        );
    }

    #[test]
    fn bind_key_accepts_copy_mode_tables() {
        let ParsedCmd::BindKey { table, .. } =
            resolve(&raw("bind", &["-T", "copy-mode-vi", "h", "copy-cursor-left"])).unwrap()
        else {
            panic!("expected BindKey")
        };
        assert_eq!(table, "copy-mode-vi");
    }

    // ---- selection + paste buffers (Task 3, sub-project 4) ----

    #[test]
    fn copy_selection_action_commands_resolve() {
        assert_eq!(resolve(&raw("copy-begin-selection", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::BeginSelection));
        assert_eq!(resolve(&raw("copy-rectangle-toggle", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::RectangleToggle));
        assert_eq!(resolve(&raw("copy-other-end", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::OtherEnd));
        assert_eq!(resolve(&raw("copy-clear-selection", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::ClearSelection));
        assert_eq!(
            resolve(&raw("copy-selection-and-cancel", &[])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::SelectionAndCancel)
        );
    }

    #[test]
    fn send_keys_dash_x_maps_selection_names() {
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "begin-selection"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::BeginSelection)
        );
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "rectangle-toggle"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::RectangleToggle)
        );
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "other-end"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::OtherEnd)
        );
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "clear-selection"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::ClearSelection)
        );
        // The one -X name that keeps the "copy-" prefix.
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "copy-selection-and-cancel"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::SelectionAndCancel)
        );
    }

    // ---- search (Task 4, sub-project 4) ----

    #[test]
    fn copy_search_action_commands_resolve() {
        assert_eq!(resolve(&raw("copy-search-forward", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::SearchForward));
        assert_eq!(resolve(&raw("copy-search-backward", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::SearchBackward));
        assert_eq!(resolve(&raw("copy-search-again", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::SearchAgain));
        assert_eq!(resolve(&raw("copy-search-reverse", &[])).unwrap(), ParsedCmd::CopyCmd(CopyAction::SearchReverse));
        // No arguments accepted (same rule as every other copy-* command).
        assert_eq!(resolve(&raw("copy-search-forward", &["x"])).unwrap_err(), usage("copy-search-forward").unwrap());
    }

    #[test]
    fn send_keys_dash_x_maps_search_names() {
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "search-forward"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::SearchForward)
        );
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "search-backward"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::SearchBackward)
        );
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "search-again"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::SearchAgain)
        );
        assert_eq!(
            resolve(&raw("send-keys", &["-X", "search-reverse"])).unwrap(),
            ParsedCmd::CopyCmd(CopyAction::SearchReverse)
        );
    }

    #[test]
    fn paste_buffer_flags() {
        assert_eq!(
            resolve(&raw("paste-buffer", &[])).unwrap(),
            ParsedCmd::PasteBuffer { name: None, target: None, no_replace: false }
        );
        assert_eq!(
            resolve(&raw("pasteb", &["-p", "-b", "foo", "-t", "work"])).unwrap(),
            ParsedCmd::PasteBuffer { name: Some("foo".to_string()), target: Some("work".to_string()), no_replace: false }
        );
        assert_eq!(
            resolve(&raw("paste-buffer", &["-r"])).unwrap(),
            ParsedCmd::PasteBuffer { name: None, target: None, no_replace: true }
        );
        assert_eq!(resolve(&raw("paste-buffer", &["bogus"])).unwrap_err(), usage("paste-buffer").unwrap());
    }

    #[test]
    fn list_buffers_takes_no_args() {
        assert_eq!(resolve(&raw("lsb", &[])).unwrap(), ParsedCmd::ListBuffers);
        assert_eq!(resolve(&raw("list-buffers", &["x"])).unwrap_err(), usage("list-buffers").unwrap());
    }

    #[test]
    fn delete_buffer_flags() {
        assert_eq!(resolve(&raw("deleteb", &[])).unwrap(), ParsedCmd::DeleteBuffer { name: None });
        assert_eq!(
            resolve(&raw("delete-buffer", &["-b", "foo"])).unwrap(),
            ParsedCmd::DeleteBuffer { name: Some("foo".to_string()) }
        );
    }

    #[test]
    fn set_buffer_flags_and_requires_data() {
        assert_eq!(
            resolve(&raw("setb", &["hello", "world"])).unwrap(),
            ParsedCmd::SetBuffer { name: None, data: "hello world".to_string() }
        );
        assert_eq!(
            resolve(&raw("set-buffer", &["-b", "foo", "hi"])).unwrap(),
            ParsedCmd::SetBuffer { name: Some("foo".to_string()), data: "hi".to_string() }
        );
        assert_eq!(resolve(&raw("set-buffer", &["-b", "foo"])).unwrap_err(), usage("set-buffer").unwrap());
    }

    #[test]
    fn show_options_and_display_message() {
        assert_eq!(
            resolve(&raw("show", &["-g", "status-left"])).unwrap(),
            ParsedCmd::ShowOptions { global: true, window: false, quiet: false, value_only: false, name: Some("status-left".to_string()) }
        );
        assert_eq!(
            resolve(&raw("show", &[])).unwrap(),
            ParsedCmd::ShowOptions { global: false, window: false, quiet: false, value_only: false, name: None }
        );
        assert_eq!(
            resolve(&raw("display", &["hello", "world"])).unwrap(),
            ParsedCmd::DisplayMessage { text: Some("hello world".to_string()) }
        );
        assert_eq!(resolve(&raw("display-message", &[])).unwrap(), ParsedCmd::DisplayMessage { text: None });
    }

    /// #68: `show -gqv "@foo"` (the TPM rung-1 primitive) must parse `-q`
    /// and `-v` (bundled with `-g` in one clump, tmux's real boolean-flag
    /// bundling rule, `arguments.c:205-252`) alongside the pre-existing
    /// `-g`. Also covers `-w`/`showw`/`show-window-options` implying
    /// `window: true`, mirroring `setw_is_set_option_alias` above.
    #[test]
    fn show_options_parses_v_and_q_flags() {
        assert_eq!(
            resolve(&raw("show", &["-gqv", "@foo"])).unwrap(),
            ParsedCmd::ShowOptions { global: true, window: false, quiet: true, value_only: true, name: Some("@foo".to_string()) }
        );
        // Un-bundled, same result.
        assert_eq!(
            resolve(&raw("show-options", &["-g", "-q", "-v", "@foo"])).unwrap(),
            ParsedCmd::ShowOptions { global: true, window: false, quiet: true, value_only: true, name: Some("@foo".to_string()) }
        );
        // `-w` explicit.
        assert_eq!(
            resolve(&raw("show", &["-w", "mode-keys"])).unwrap(),
            ParsedCmd::ShowOptions { global: false, window: true, quiet: false, value_only: false, name: Some("mode-keys".to_string()) }
        );
        // `showw`/`show-window-options` imply `-w` with no explicit flag.
        let want_w = ParsedCmd::ShowOptions { global: false, window: true, quiet: false, value_only: false, name: None };
        assert_eq!(resolve(&raw("showw", &[])).unwrap(), want_w);
        assert_eq!(resolve(&raw("show-window-options", &[])).unwrap(), want_w);
    }

    /// SP6 Task 2: `setw`/`set-window-option` are real tmux command entries
    /// (not config-level aliases) sharing `set-option`'s exec function with
    /// an implied `-w`, per `commands-config-options-formats.md`'s
    /// `set-window-option`/`setw` note. `setw -g pane-base-index 1` must
    /// parse to the exact same `ParsedCmd` as `set -w -g pane-base-index 1`
    /// -- including `window: true` despite no explicit `-w` token.
    #[test]
    fn setw_is_set_option_alias() {
        let want = resolve(&raw("set", &["-w", "-g", "pane-base-index", "1"])).unwrap();
        assert_eq!(want, ParsedCmd::SetOption { global: true, window: true, append: false, unset: false, name: "pane-base-index".to_string(), value: Some("1".to_string()) });
        assert_eq!(resolve(&raw("setw", &["-g", "pane-base-index", "1"])).unwrap(), want);
        assert_eq!(resolve(&raw("set-window-option", &["-g", "pane-base-index", "1"])).unwrap(), want);
    }
}
