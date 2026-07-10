//! Headless multiplexer server: owns every session/window/pane, accepts
//! client connections over a named pipe, composes a per-client VT stream,
//! and routes input back to panes. See `docs/specs/2026-07-07-server-client-design.md`
//! ("Server architecture", "Data model", "Input routing", "Transport") and
//! the `## server` section of the sibling interfaces contract.
//!
//! Only [`run`] is public; everything below is the server's private state
//! machine.
//!
//! ## Design choices (see task-6-report.md for the full write-up)
//!
//! - **Confirm race** (follow-ups #2): NOT fixed here. `Ctrl-b x` and a
//!   following `y` arriving in the SAME `Stdin` frame still race exactly as
//!   in the MVP (the `y` gets forwarded to the pane instead of confirming),
//!   because `KeyMachine::feed` decodes the whole frame before the caller
//!   (this module) gets a chance to call `set_capture` to arm the confirm.
//!   This is one of the two sanctioned options in the task brief; documented
//!   here as still-open rather than half-fixed.
//! - **Render strategy**: every dirty turn (i.e. any event at all, after
//!   coalescing) re-renders ALL attached clients, not just those whose
//!   session actually changed. Simpler and correct at this scale; a
//!   per-session dirty set would cut redundant renders for unrelated
//!   sessions but isn't needed yet.
//! - **Test thread lifecycle**: `tests/server_proto.rs` gives every test a
//!   unique pipe name and, where the test's flow naturally destroys every
//!   session, joins the server thread to prove clean exit-empty shutdown.

mod dispatch;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
use windows::Win32::System::WindowsProgramming::GetComputerNameW;

use crate::bindings::Bindings;
use crate::buffers::Buffers;
use crate::cmd::RawCmd;
use crate::geom::Rect;
use crate::grid::{Cell, Grid, Style};
use crate::input::{KeyInputEvent, KeyMachine, WhichTable};
use crate::layout::{Layout, PaneId, MIN_PANE_H, MIN_PANE_W};
use crate::model::{Registry, Session, WindowId};
use crate::options::{expand_format, FormatCtx, Options, SystemTimeParts};
use crate::pipe::{PipeConn, PipeListener};
use crate::protocol::{self, read_client_msg, write_server_msg, AttachMode, ClientMsg, ServerMsg};
use crate::pty::Pty;
use crate::render::{CopyView, ListOverlay, Overlay, PaneView, PreviewBlock, Renderer, Scene, StatusRow, TreeRowCell};
use crate::status::{status_spans, strip_style_markers, WindowEntry};

/// Abbreviated month names for the status-bar clock (`DD-Mon-YY`) and the
/// CLI's `ls` creation-time format.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Transient status-line message lifetime (tmux `display-time` default).
const MESSAGE_LIFETIME: Duration = Duration::from_millis(750);

/// automatic-rename throttle (Task 9, sub-project 4): tmux `NAME_INTERVAL`
/// -- at most one automatic rename per window per this duration, even if its
/// active pane's title keeps changing faster than that.
const AUTO_RENAME_THROTTLE: Duration = Duration::from_millis(500);

/// Server-global, monotonically increasing client id (distinct id space
/// from `PaneId`/`WindowId`).
type ClientId = u32;

/// Messages funneled from every worker thread into the single-consumer main
/// loop. See the design spec's "Server architecture" diagram.
enum ServerEvent {
    /// ConPTY output for a pane (pane reader thread).
    Output(PaneId, Vec<u8>),
    /// A pane's child process exited (pane waiter thread).
    Exited(PaneId),
    /// A client connected (accept thread).
    Connected(PipeConn),
    /// A decoded frame from an attached (or not-yet-attached) client
    /// (per-client reader thread).
    FromClient(ClientId, ClientMsg),
    /// A client's connection closed/errored (per-client reader thread).
    ClientGone(ClientId),
    /// 50ms coalescing tick: refresh the clock.
    Tick,
}

/// One pane's live resources. `pty` is dropped (set to `None`) the moment
/// the child exits (follow-up #1) — this frees the pseudoconsole/conhost
/// immediately rather than waiting for the pane to be closed from the UI.
struct PaneRuntime {
    pty: Option<Pty>,
    grid: Grid,
    dead: bool,
    /// `#T`/`#{pane_title}` (Task 9, sub-project 4): a cached copy of
    /// `grid.title()`, refreshed whenever `grid.take_title_changed()` fires
    /// on an `Output` feed (see `Server::handle_event`'s `Output` arm) — kept
    /// as a plain `String` (default empty, not `Option`) so every format-ctx
    /// call site can hand out `&str` directly without an `unwrap_or("")` at
    /// every use.
    title: String,
    /// Per-pane writer channel (follow-up #14): the main loop's hot
    /// Forward/Key-forwarding path (`Server::process_client_events`) enqueues
    /// here instead of calling `pty.write_input` inline, so a pane whose
    /// child stops draining stdin (hung app, huge paste) only blocks the
    /// dedicated writer thread draining this channel — never the main loop,
    /// never any OTHER session/client. Mirrors the existing per-client
    /// writer design (`spawn_writer`) exactly. `spawn_pane` is the only
    /// producer of a real one (a `PtyWriter`-backed thread); a pane inserted
    /// directly by a unit test with `pty: None` gets a channel with no
    /// writer thread behind it, which is fine — sends into it just pile up
    /// harmlessly until the `Sender` drops with the `PaneRuntime`. Dropping
    /// `PaneRuntime` (pane removal — the ONLY way one is ever dropped) drops
    /// this field, which closes the channel and lets the writer thread's
    /// `recv()` loop end on its own; no explicit shutdown/join needed.
    input_tx: Sender<Vec<u8>>,
}

/// Which status-line prompt is in progress (`,` rename-window, `$`
/// rename-session, `.` move-window, `f` find-window, `'` index, or `:`
/// command-prompt) — determines the label text and what a commit does with
/// the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    RenameWindow,
    RenameSession,
    /// `.` prompt (Task 7, sub-project 4): commit dispatches `move-window -t
    /// <input>` (`-k` is never supplied by the prompt -- matches real
    /// tmux's own `.` binding, which has no way to type `-k` interactively
    /// either; use the `:` command-prompt for `move-window -k`).
    MoveWindow,
    /// `f` prompt (Task 7): commit dispatches `find-window <input>`.
    FindWindow,
    /// `'` prompt (Task 7): commit dispatches `select-window -t :<input>`.
    Index,
    /// `:` command-prompt (Task 6): commit parses the buffer as a command
    /// line and dispatches it instead of renaming anything.
    Command,
}

/// Per-client confirm/prompt state. `ConfirmCmd` (Task 6) generalizes the
/// legacy `ConfirmKillPane`/`ConfirmKillWindow` variants into a single
/// "confirm-before"-shaped mode: the wrapped command(s) to dispatch on
/// y/Y/Enter, plus a snapshot of the pane/window that was live when the
/// confirm was armed (staleness check — see `cancel_stale_confirms` and
/// `dispatch::Server::feed_confirm_byte`).
enum ClientMode {
    Normal,
    ConfirmCmd {
        prompt: String,
        cmds: Vec<RawCmd>,
        pane_snapshot: Option<PaneId>,
        window_snapshot: Option<WindowId>,
    },
    /// Status-line line editor (rename-window / rename-session / `:`
    /// command-prompt). `buf` is the live-edited text (pre-filled with the
    /// current name, or empty/initial for `:`); `label` is the fixed prefix.
    Prompt { label: String, buf: String, kind: PromptKind },
    /// Copy mode (Task 2, sub-project 4): scrollback navigation bound to the
    /// pane that was focused at entry. Per-CLIENT, not per-pane (tmux models
    /// copy mode per-pane; winmux's divergence is documented in the design
    /// spec's `## 2. Copy mode` section) — two clients can independently be
    /// in copy mode on the same or different panes.
    Copy(CopyState),
    /// choose-tree overlay (Task 8, sub-project 4; `w`/`s`, `choose-tree
    /// [-s|-w]`). Per-CLIENT, table-override key routing exactly like
    /// `Copy` (the exemplar this mode follows) — see `dispatch::
    /// resolve_choose_tree_key`/`dispatch_choose_tree_key`. Deliberately
    /// carries no snapshotted `rows`: `ChooseTreeState` only remembers WHICH
    /// view is showing and the selected INDEX into it; the actual row list
    /// (text + target identity) is rebuilt fresh from live registry state on
    /// every render AND every key that needs to resolve `sel` to a concrete
    /// target (`dispatch::Server::build_tree_rows`) — this is what makes the
    /// "stale row acts on the wrong/dead target" bug class structurally
    /// unreachable for navigation/commit (see the design brief's "multi-
    /// client and staleness" section). The one piece of state that DOES
    /// persist across renders, `pending_kill`, is re-validated against live
    /// state before acting (`Server::cancel_stale_choose_trees`) for exactly
    /// the same reason `cancel_stale_confirms` re-validates `ConfirmCmd`.
    ChooseTree(ChooseTreeState),
    /// display-panes overlay (Task 8; `q`, `display-panes [-d ms]`): a
    /// per-client TIMED overlay (`deadline`) showing a digit on every pane of
    /// the client's current window, auto-dismissing on `Tick` once expired.
    /// No pane-set snapshot either -- the digit-to-pane mapping is rebuilt
    /// fresh from the CURRENT window layout both when rendering
    /// (`Server::build_render_overlay`) and when resolving a digit keypress
    /// (`dispatch::Server::dispatch_display_panes_key`), so a pane that died
    /// mid-overlay simply stops being offered a digit rather than a stale
    /// key press acting on a dead `PaneId`.
    DisplayPanes(DisplayPanesState),
    /// clock-mode overlay (Task 10, sub-project 6 wave 2; `t`, `clock-mode`):
    /// a per-client mode bound to the pane that was FOCUSED at entry, same
    /// binding rule as `Copy` (`docs/tmux-reference/status-line-and-
    /// messages.md` `## 6. Clock mode`: "it is a window mode on the pane").
    /// Unlike `DisplayPanes` there is no auto-dismiss deadline -- real
    /// tmux's "any key exits" (`window_clock_key`/`window_pane_reset_mode`)
    /// is the only way out (see `dispatch::Server::dispatch_clock_key`).
    Clock(ClockState),
}

/// choose-tree's two views (Task 8): `Sessions` (`-s`) lists every session as
/// a real tree row (SP6 wave 2, Task 8: a `+`/`-` expand marker, collapsed by
/// default per `docs/tmux-reference/choose-tree.md` `## 1.1` -- "sessions
/// start collapsed"; `Right`/`+` reveals its windows as indented children);
/// `Windows` (`-w`, the default) lists the CURRENT session's windows (a
/// session header row + one indented row per window, unconditionally shown
/// -- see the design spec's `## 7. Overlays` section for the documented
/// "windows of the current session only" scope simplification, which this
/// task does not change: real tmux's `-w` shows the whole multi-session
/// tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChooseTreeView {
    Windows,
    Sessions,
}

/// choose-tree's live preview mode (SP6 wave 2, Task 8;
/// `docs/tmux-reference/choose-tree.md` `## 3.1`/`## 7.1`): `v` cycles
/// `Off -> Big -> Normal -> Off`. `Normal` (the tmux/winmux default) gives
/// the row list two thirds of the panel; `Big` gives it one quarter (a
/// bigger preview, smaller list); `Off` gives the list the whole panel.
/// [`dispatch::Server::choose_tree_list_height`] turns this (plus the panel
/// size and row count) into an actual row-count split every render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewMode {
    Off,
    Big,
    Normal,
}

impl PreviewMode {
    fn cycle(self) -> Self {
        match self {
            PreviewMode::Off => PreviewMode::Big,
            PreviewMode::Big => PreviewMode::Normal,
            PreviewMode::Normal => PreviewMode::Off,
        }
    }
}

/// choose-tree's per-client state (Task 8; review fix, Critical #1).
/// `selected` is the STABLE IDENTITY of the currently-selected row -- the
/// single source of truth for "what did the user actually select" -- and
/// `sel` is only a best-effort cache of its last-known display INDEX, used
/// solely as the fallback clamp point if that identity's row vanishes
/// entirely (see [`resolve_tree_sel`]). A plain array index is NOT a safe
/// primary key here: `dispatch::Server::build_tree_rows(session_name, view)`
/// is rebuilt fresh from live registry state on every render AND every
/// keypress, so "fresh" only means "current data," not "the same row the
/// user was looking at" -- if the row list shrinks (another client's
/// `kill-window`, or a pane exiting naturally) between the last render this
/// client saw and the keypress that commits/kills, a raw index can silently
/// resolve to a DIFFERENT, still-live row (reachable because the server's
/// event loop coalesces multiple queued events into one batch before
/// rendering). `Up`/`Down`/`Commit`/`Kill` all resolve through
/// [`resolve_tree_sel`] against the freshly rebuilt row list, never against
/// a bare index. `pending_kill` is `Some((target, prompt))` between pressing
/// `x` and answering y/n: the confirm PROMPT TEXT is precomputed at
/// `x`-press time (mirrors `ClientMode::ConfirmCmd`'s own `prompt` field,
/// deliberately NOT routed through `client.message` -- message is cleared on
/// every `Stdin` frame, which would make the prompt vanish before the user
/// could answer it). `pending_kill` was already identity-based before this
/// fix (`TreeTarget`, not an index) -- only `sel` needed the fix.
///
/// SP6 wave 2, Task 8 additions: `expanded` is the set of SESSION NAMES
/// currently expanded in the `Sessions` view (keyed by session identity, same
/// as `TreeTarget::Session`) -- sessions start collapsed (absent from the
/// set) per `docs/tmux-reference/choose-tree.md` `## 1.1`; `Windows` view row
/// generation ignores this set entirely (its window children are always
/// shown, matching that doc section's "-w: sessions expanded" default and
/// this project's pre-existing "current session only" scope simplification).
/// `preview` is the `v`-cycled preview mode (`## 3.1`/`## 7.1`), starting at
/// `Normal` (tmux's own default -- neither `-N` nor `-N -N` is ever passed by
/// winmux's `w`/`s` bindings).
struct ChooseTreeState {
    view: ChooseTreeView,
    sel: usize,
    selected: Option<TreeTarget>,
    pending_kill: Option<(TreeTarget, String)>,
    expanded: HashSet<String>,
    preview: PreviewMode,
}

/// Re-resolve choose-tree's selection identity to a display INDEX into a
/// freshly-rebuilt row list (Task 8 review fix, Critical #1): the single
/// seam every read of `ChooseTreeState.sel`/`selected` must go through,
/// shared by `Server::build_render_overlay` (render time) and
/// `dispatch::Server::dispatch_choose_tree_key` (Up/Down/Commit/Kill time).
/// If `selected`'s target is still present in `rows`, its CURRENT position
/// wins outright, regardless of what `fallback` says (this is the fix: a
/// row that moved because an EARLIER row was killed is still found by
/// identity, not lost to a stale positional index). Only when the
/// previously-selected row is genuinely gone does this fall back to
/// `fallback` (the last-known index), clamped into the new, possibly
/// shorter, row list -- mirroring how `cancel_stale_choose_trees` already
/// re-validates `pending_kill` against live state. An empty `rows` resolves
/// to `0` (the caller must check `rows.is_empty()` separately before
/// indexing).
fn resolve_tree_sel(rows: &[dispatch::TreeRow], selected: &Option<TreeTarget>, fallback: usize) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let clamped_fallback = fallback.min(rows.len() - 1);
    match selected {
        Some(target) => rows.iter().position(|r| &r.target == target).unwrap_or(clamped_fallback),
        None => clamped_fallback,
    }
}

/// A choose-tree row's underlying identity (Task 8): resolved fresh from the
/// registry at both render time and commit/kill time, never cached across a
/// render -- see `ClientMode::ChooseTree`'s doc comment.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TreeTarget {
    Session(String),
    /// Session name + window id (the session is always the acting client's
    /// current session in THIS project's simplified `Windows` view, but
    /// carried explicitly rather than assumed, for clarity at the exec
    /// sites).
    Window(String, WindowId),
}

/// display-panes' per-client state (Task 8): just the auto-dismiss deadline
/// (`Instant::now() + display-panes-time` at entry, or `+ -d ms` if given).
/// See `ClientMode::DisplayPanes`'s doc comment for why no pane-set snapshot
/// is carried either.
struct DisplayPanesState {
    deadline: Instant,
}

/// clock-mode's per-client state (Task 10): `pane` is the pane the mode was
/// entered on (mirrors `CopyState::pane` -- kept explicit rather than
/// implicitly "whatever's focused now", the correct DRY shape even though in
/// practice nothing inside clock mode can change focus before the mode
/// exits, since ANY key immediately does). `text` is the CURRENTLY DISPLAYED
/// already-formatted time string (`format_clock`, `clock-mode-style`-
/// governed) -- the render path (`Server::build_render_overlay`/
/// `render_one`) just draws it verbatim, which is the seam that lets the
/// render unit test inject a fixed string and never touch wall-clock. The
/// `Tick` handler recomputes the current formatted string every 50ms and
/// only marks the client dirty when it actually differs from `text`,
/// matching real tmux's own "redraw only if the time actually changed"
/// rule (`window-clock.c:146-168`).
struct ClockState {
    pane: PaneId,
    text: String,
}

/// Format the clock-mode display string per `clock-mode-style`
/// (`docs/tmux-reference/status-line-and-messages.md` `## 6. Clock mode`):
/// `style12 == false` (the `24` default) -> tmux's `%H:%M` (zero-padded);
/// `style12 == true` -> tmux's `%l:%M ` (`%l` = space-padded, NOT
/// zero-padded, 12-hour -- a POSIX strftime extension Windows lacks,
/// special-cased here per the doc's `## 10` "Windows/winmux applicability"
/// note) immediately followed by `AM`/`PM` with no extra space (matches
/// `window-clock.c`'s literal `strftime(..., "%l:%M ", tm)` +
/// `strcat(.., tm->tm_hour >= 12 ? "PM" : "AM")`). `hour24` is 0-23,
/// `minute` 0-59; both are the caller's job to keep in range (from
/// `system_time_parts`/`SYSTEMTIME`, always in range in practice).
fn format_clock(hour24: u8, minute: u8, style12: bool) -> String {
    if style12 {
        let hour12 = match hour24 % 12 {
            0 => 12,
            h => h,
        };
        let ampm = if hour24 < 12 { "AM" } else { "PM" };
        format!("{hour12:2}:{minute:02} {ampm}")
    } else {
        format!("{hour24:02}:{minute:02}")
    }
}

/// Task 8 (display-panes): pane-index -> digit mapping, in the window's
/// `layout.panes()` order (the SAME order the status bar's pane-index format
/// and `select-pane -t <n>` use), capped at the first 10 panes -- digits
/// 0-9 only (design spec `## 7. Overlays`: an 11th+ pane simply gets no
/// digit and is never offered as a display-panes target, a documented tmux-
/// parity simplification). Shared by both the render path
/// (`Server::build_render_overlay`) and the digit-keypress resolution path
/// (`dispatch::Server::dispatch_display_panes_key`) so they can never
/// disagree about which digit means which pane.
fn pane_digit_entries(window: &crate::model::Window) -> Vec<(PaneId, u32)> {
    window.layout.panes().into_iter().take(10).enumerate().map(|(i, id)| (id, i as u32)).collect()
}

/// Precomputed, render-ready overlay content for one client (Task 8),
/// resolved in `Server::build_render_overlay` -- a pass over `&self`
/// BEFORE `render_all`'s per-client `self.clients.values_mut()` loop begins,
/// because `ChooseTree`'s `Sessions` view needs the WHOLE registry (not just
/// the client's own session) and its "(attached)" suffix needs `self.
/// clients`, neither of which the per-client `render_one` (called while
/// `self.clients` is already mutably borrowed) can see directly.
enum RenderOverlay {
    /// Already-formatted row text (+ tree depth/expand-marker) in order,
    /// plus which index is selected; `render_one` turns this into a
    /// `render::Overlay::List` (padding/scrolling is a rendering concern,
    /// computed there with the client's own `rows`/`cols` in hand).
    /// `list_height` is the pre-sized row-list height (`Server::
    /// choose_tree_list_height`, SP6 wave 2 Task 8) -- equal to the full
    /// scene height whenever `preview` is `None` (preview OFF, or the panel
    /// too small per the sizing rule), reproducing the pre-Task-8-wave-2
    /// full-height list exactly in that case.
    Tree {
        rows: Vec<(String, u8, Option<char>)>,
        sel: usize,
        list_height: u16,
        preview: Option<TreePreviewData>,
    },
    /// The digit-to-pane mapping for the client's current window (see
    /// [`pane_digit_entries`]); `render_one` maps each `PaneId` to its
    /// current rect and active-ness.
    Digits(Vec<(PaneId, u32)>),
    /// clock-mode (Task 10): the bound pane's id (`render_one` resolves it
    /// to a current rect, same as `Digits` does per-entry) and the
    /// already-formatted time string (`ClockState::text`) -- `render_one`
    /// just hands both, plus `clock-mode-colour`, to `render::Overlay::Clock`.
    Clock { pane: PaneId, text: String },
}

/// The selected tree row's live preview content (SP6 wave 2, Task 8),
/// pre-composed by `dispatch::Server::build_tree_preview` from the raw
/// pane `Grid`s `self.panes` owns -- `render_one` only needs to place it (the
/// panel-relative rect is computed there from `list_height`/the client's own
/// `rows`/`cols`, since `build_render_overlay` runs before that's known) as
/// a `render::PreviewBlock`.
struct TreePreviewData {
    title: String,
    content_w: u16,
    content_h: u16,
    content: Vec<Cell>,
}

/// Copy mode's per-client state. `scroll` == tmux `oy` (lines scrolled up
/// from the live bottom, 0 = live screen); `cx`/`cy` are the copy cursor in
/// VIEW coordinates (0-based, within the pane's current `cols`/`rows`).
/// `scroll_exit` is a placeholder for the mouse task's scroll-past-bottom
/// auto-exit (SP4 §4) — unused until then. `sel` (Task 3, sub-project 4) is
/// the active selection, if any. `search`/`search_prompt` (Task 4) are the
/// stored repeatable search and the in-progress `/`/`?`/`C-s`/`C-r` line
/// edit, if any — see their own doc comments for why the search PROMPT is
/// tracked here rather than by switching `client.mode` to `ClientMode::Prompt`.
struct CopyState {
    pane: PaneId,
    scroll: u32,
    cx: u16,
    cy: u16,
    #[allow(dead_code)]
    scroll_exit: bool,
    sel: Option<SelState>,
    search: Option<SearchState>,
    search_prompt: Option<SearchPrompt>,
}

/// Task 4 (search): the last search a client COMMITTED in this copy-mode
/// session (pattern + direction), regardless of whether it matched — `n`/`N`
/// repeat against this, and even a failed search is worth remembering (the
/// user may scroll/resize and retry). `None` until the first commit.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchState {
    pattern: String,
    backward: bool,
}

/// Task 4 (search): the active `/`/`?`/`C-s`/`C-r` line edit, `Some` only
/// while the search prompt is open. Deliberately stored HERE, inside
/// `CopyState`, rather than by switching `client.mode` to the pre-existing
/// `ClientMode::Prompt` (which is how `:`/rename prompts work): `render_one`
/// only paints a pane's SCROLLED copy view, frozen cursor, and selection
/// highlight when `client.mode` is literally `ClientMode::Copy` (see its
/// `let copy = match &client.mode { ClientMode::Copy(cs) if ... }` below) —
/// switching away to `ClientMode::Prompt` while typing a search would drop
/// back to the pane's LIVE view/cursor for the duration of typing, an
/// observable regression from tmux (which keeps the copy-mode screen frozen
/// under the "Search Down:"/"Search Up:" prompt). Capture is armed the exact
/// same way (`client.key_machine.set_capture`) and the byte-level editing
/// rules are identical to `feed_prompt_byte` (printable append / BSpace
/// delete / Enter commit / Esc-Ctrl+c-Ctrl+g cancel, see
/// `Server::feed_copy_search_byte`) — no new capture MECHANISM, only a new
/// place for the in-progress buffer to live so the surrounding copy-mode
/// state survives the round trip.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchPrompt {
    backward: bool,
    buf: String,
}

/// A copy-mode selection's anchor + shape (Task 3, sub-project 4). The
/// anchor is pinned to CONTENT, not to the view (Task 3 review fix): its
/// position is captured as a view position (`anchor_scroll`/`anchor_x`/
/// `anchor_y`) PLUS the grid's monotonic `Grid::history_total()` reading at
/// capture time (`anchor_total`). At every use site (render precompute,
/// text extraction, `copy-other-end`) the anchor's CURRENT view position is
/// recomputed via [`anchor_key_now`], which shifts the capture-time key
/// down by one per line captured since — so new pane output arriving
/// mid-selection moves the anchor's highlight/extraction up in lockstep
/// with the content it was placed on, instead of the anchor staying glued
/// to a (now wrong) view row. The CURSOR endpoint deliberately stays
/// view-relative (`CopyState::scroll`/`cx`/`cy` are untouched by new
/// output — the copy cursor keeps its screen position while content moves
/// underneath, current behavior kept per the review guidance) and is
/// converted to the same key coordinate live at each use, so both
/// endpoints are always compared in one coherent frame. `rect` toggles
/// rectangle (column-bounding-box) selection vs. the default linear
/// (reading-order) selection. `kind` (Task 7, SP6 wave 2) is what unit the
/// MOVING end snaps to while dragging -- see [`SelKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelState {
    anchor_scroll: u32,
    anchor_x: u16,
    anchor_y: u16,
    /// `Grid::history_total()` at anchor time (see [`anchor_key_now`]).
    anchor_total: u64,
    rect: bool,
    kind: SelKind,
}

/// What unit a copy-mode selection's MOVING end snaps to while dragging
/// (Task 7, SP6 wave 2). `Char` is the default: keyboard `begin-selection`
/// and a plain-click-then-drag extend cell by cell. `Word`/`Line` are
/// installed by DoubleClick/TripleClick (`select_word_at`/`select_line_at`)
/// and make every subsequent `Drag` event snap the moving end to whole
/// word/line boundaries AND flip the fixed anchor end between the anchor
/// word/line's start and end, matching tmux's `SEL_WORD`/`SEL_LINE`
/// (`window_copy_synchronize_cursor_end`,
/// `docs/tmux-reference/mouse.md` :636-642 and
/// `docs/tmux-reference/copy-mode-and-buffers.md` :440-447): the selection
/// is always a whole number of words/lines that always includes the anchor
/// word/line. See `dispatch::move_drag_cursor`'s doc comment for the exact
/// snap rule and `dispatch::word_bounds_at` for word-boundary detection
/// (driven by the `word-separators` option, `Options::word_separators`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelKind {
    Char,
    Word,
    Line,
}

/// A copy-mode view position's ordering/delta key AT ONE INSTANT: for a
/// position at view row `row` under scroll offset `scroll`, `key = row -
/// scroll`. Two keys measured against the SAME grid state compare the same
/// way their absolute grid-line indices would, and `key + scroll` gives
/// that position's view row under a different scroll offset. NOTE keys are
/// NOT stable across grid mutations — every line captured into scrollback
/// since a key was taken shifts that content's current key down by one
/// (chunked eviction shifts nothing) — so a STORED anchor must go through
/// [`anchor_key_now`] rather than reusing its capture-time key directly
/// (Task 3 review fix: the original code did exactly that, and a selection
/// drifted onto unrelated text whenever the pane produced output
/// mid-selection).
fn sel_key(scroll: u32, row: u16) -> i64 {
    row as i64 - scroll as i64
}

/// The stored anchor's key in the grid's CURRENT frame: its capture-time
/// key shifted down by one per line captured since (`history_total -
/// anchor_total` — exact even across eviction, which lowers `history_len`
/// but never moves a surviving line's view position), then clamped to the
/// oldest retained history line (`key >= -history_len`): if the anchor's
/// line has been EVICTED, the endpoint degrades to the oldest content
/// still available instead of pointing off the buffer (no panic — and no
/// reliance on `Grid`'s own read-time clamping, which would silently read
/// the wrong row for keys below the clamp).
fn anchor_key_now(sel: &SelState, history_len: u32, history_total: u64) -> i64 {
    let raw = sel_key(sel.anchor_scroll, sel.anchor_y);
    let shifted = raw - history_total.saturating_sub(sel.anchor_total) as i64;
    shifted.max(-(history_len as i64))
}

/// Inverse of [`sel_key`] for `Grid::view_row_text`/`view_cell`-style
/// `(scroll_back, row)` queries: the smallest valid `scroll_back` (`Grid`
/// clamps to its actual `history_len` internally, so an over-large value
/// here is harmless) that reproduces `key` in view coordinates, with `row`
/// picked so it's always >= 0 regardless of how far `key` reaches into
/// history. `rows` clamps a below-the-live-screen `row` defensively (a
/// selection endpoint should never legitimately need this, but a resized
/// pane between selecting and extracting could otherwise index out of the
/// grid's current dimensions).
fn key_to_view(key: i64, rows: u16) -> (u32, u16) {
    if key <= 0 {
        ((-key) as u32, 0)
    } else {
        (0, (key as u16).min(rows.saturating_sub(1)))
    }
}

/// Precompute a copy-mode selection's render data in VIEW coordinates
/// (`## copy-mode` / render contract amendment, Task 3): `(start_col,
/// start_row, end_col, end_row, rect)`, both endpoints clamped into the
/// visible `rows`x`cols` view under the CURRENT `scroll`/`cx`/`cy`. `None`
/// when the selection is wholly scrolled out of view. `history_len`/
/// `history_total` are the pane grid's CURRENT readings, used to pin the
/// stored anchor to content via [`anchor_key_now`] (Task 3 review fix).
#[allow(clippy::too_many_arguments)]
fn compute_sel_view(
    sel: &SelState,
    cx: u16,
    cy: u16,
    scroll: u32,
    rows: u16,
    cols: u16,
    history_len: u32,
    history_total: u64,
) -> Option<(u16, u16, u16, u16, bool)> {
    let anchor_key = anchor_key_now(sel, history_len, history_total);
    let cursor_key = sel_key(scroll, cy);
    let (start_key, start_col, end_key, end_col) = if (anchor_key, sel.anchor_x) <= (cursor_key, cx) {
        (anchor_key, sel.anchor_x, cursor_key, cx)
    } else {
        (cursor_key, cx, anchor_key, sel.anchor_x)
    };

    let start_row_signed = start_key + scroll as i64;
    let end_row_signed = end_key + scroll as i64;
    if end_row_signed < 0 || start_row_signed >= rows as i64 {
        return None; // wholly above or wholly below the current view
    }
    let clipped_top = start_row_signed < 0;
    let clipped_bottom = end_row_signed >= rows as i64;
    let start_row = start_row_signed.max(0) as u16;
    let end_row = end_row_signed.min(rows as i64 - 1) as u16;

    let last_col = cols.saturating_sub(1);
    let (sc, ec) = if sel.rect {
        (start_col.min(end_col).min(last_col), start_col.max(end_col).min(last_col))
    } else {
        // A vertically-clipped endpoint's real column is off-screen too --
        // widen to the row edge so the render pass's "first row from
        // start_col" / "last row to end_col" rule paints that (now a
        // full-width MIDDLE row of the true selection) row correctly. See
        // `CopyView::sel`'s doc comment.
        let sc = if clipped_top { 0 } else { start_col.min(last_col) };
        let ec = if clipped_bottom { last_col } else { end_col.min(last_col) };
        (sc, ec)
    };
    Some((sc, start_row, ec, end_row, sel.rect))
}

/// Per-client attached state.
struct ClientState {
    session: Option<String>,
    cols: u16,
    rows: u16,
    renderer: Renderer,
    key_machine: KeyMachine,
    mode: ClientMode,
    /// A transient status-line message (e.g. `window not found: 5`) and when
    /// it was set; cleared on the next `Stdin` frame from this client OR
    /// after `MESSAGE_LIFETIME` elapses (checked on `Tick`). Shown only while
    /// `mode` is `Normal` (confirm/prompt overlays take priority).
    message: Option<(String, Instant)>,
    /// Feeds the client's writer thread (which owns the actual `Write` half
    /// of the pipe and drains this channel so a slow client never blocks
    /// the main loop).
    tx: Sender<Vec<u8>>,
    /// Mouse click/drag session state (Task 5, sub-project 4): double/
    /// triple-click detection and in-progress border-resize/selection
    /// dragging. See [`MouseClientState`].
    mouse: MouseClientState,
}

/// SGR mouse-mode enable sequence (normal tracking `?1000h` + button-event
/// tracking `?1002h` + SGR extended coordinates `?1006h`) and its `l`
/// (disable) counterpart. Sent to a client on attach (if `mouse` is already
/// on) and broadcast to every attached client whenever `set -g mouse`
/// changes (`dispatch::Server::exec_set_option`). See the design spec's
/// `## 4. Mouse` section.
const MOUSE_ENABLE_SEQ: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_DISABLE_SEQ: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1006l";

/// Double/triple-click detection window (tmux-style, ~500ms; the task brief
/// permits 300-500ms when the spec doesn't pin an exact value — 500ms
/// matches `input::REPEAT_TIME`, already the project's other user-facing
/// timing constant).
const MOUSE_CLICK_WINDOW: Duration = Duration::from_millis(500);

/// tmux `WheelUpPane`/`WheelDownPane`'s default step: 5 lines per wheel
/// event.
const MOUSE_WHEEL_STEP: u32 = 5;

/// Copy-mode drag autoscroll's repeat interval (Task 7, SP6 wave 2): while a
/// drag selection is held with the pointer on the pane's first/last row, the
/// view scrolls one line and the selection extends every time this much real
/// time elapses -- tmux's `WINDOW_COPY_DRAG_REPEAT_TIME` (`window-copy.c:351`,
/// documented in `docs/tmux-reference/mouse.md` §5.4/§8 as 50 000us / 20
/// rows per second), serviced by the same 50ms `Tick` the escape-time flush
/// already rides.
const MOUSE_DRAG_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// Per-client mouse session state (Task 5, sub-project 4): remembers the
/// last left-button click (for double/triple-click detection, same cell +
/// button within [`MOUSE_CLICK_WINDOW`]) and what an in-progress drag is
/// doing (nothing / resizing a pane border / extending a copy-mode
/// selection). `Default` starts idle.
#[derive(Default)]
struct MouseClientState {
    /// `(when, x, y, button, run_length)`; `run_length` saturates at 3 (tmux
    /// has no quadruple-click concept — a 4th same-cell click within the
    /// window is treated the same as a 3rd, i.e. line selection again).
    last_click: Option<(Instant, u16, u16, u8, u8)>,
    drag: MouseDrag,
    /// Armed while a drag selection's pointer sits on the pane's first/last
    /// row (Task 7, SP6 wave 2): `None` otherwise. Serviced by
    /// `Server::handle_event`'s `Tick` arm exactly like the escape-time
    /// flush -- see [`AutoscrollState`].
    autoscroll: Option<AutoscrollState>,
}

/// One armed copy-mode drag-autoscroll timer (Task 7, SP6 wave 2): `pane` is
/// the pane the bound `ClientMode::Copy` selection lives on, `top` is `true`
/// for the pane's FIRST row (scroll toward history) and `false` for its LAST
/// row (scroll toward the live bottom), `cursor_x` is the pointer's last
/// known pane-relative column (re-used every tick to re-evaluate a Word/Line
/// selection's snap, since no new `Drag` event arrives while the pointer sits
/// still), and `deadline` is when the next one-line scroll fires. Armed/
/// refreshed by `dispatch::service_drag_edge` on every `Drag` event whose
/// resulting cursor row is an edge row; cleared the moment a `Drag` lands
/// off an edge row, the drag ends (`Up`), or the bound pane/copy-mode goes
/// away -- matching tmux's "motion outside the pane is a no-op that stops
/// the timer; leaving the edge row stops it too" (`mouse.md` §5.4).
#[derive(Clone, Copy)]
struct AutoscrollState {
    pane: PaneId,
    top: bool,
    cursor_x: u16,
    deadline: Instant,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum MouseDrag {
    #[default]
    None,
    /// A `Down1` on a pane border armed a live resize: `pane` is the
    /// reference leaf `Layout::resize_from` resizes relative to, `vertical`
    /// is `true` for a column border (left|right panes) and `false` for a
    /// row border (top/bottom panes).
    Border { pane: PaneId, vertical: bool },
    /// A `Down1` (button 1) armed possible selection tracking, but nothing
    /// about the copy cursor/anchor/mode has been touched YET — real tmux
    /// (`mouse.md` §2.5/§5.3) classifies a drag START at the *press*
    /// position only once the first actual `Drag` event arrives, and a
    /// plain click (`Down` then `Up`, no `Drag` in between) never runs
    /// `begin-selection`/`copy-mode -M` at all, just `select-pane` (already
    /// done by `mouse_down`'s unconditional focus call). `press_x`/`press_y`
    /// are pane-relative, captured at `Down` time, matching tmux's `lx/ly`
    /// (the previous event's position it drag-START classifies against).
    /// `enter_copy` is `true` when the press landed on a pane NOT bound to
    /// this client's copy mode (root `MouseDrag1Pane -> copy-mode -M`: the
    /// first `Drag` must open copy mode on `pane` before installing the
    /// anchor) and `false` when the press landed inside the pane already
    /// bound to this client's copy mode (copy-mode table's own
    /// `MouseDrag1Pane -> begin-selection`: the anchor installs directly).
    PendingSelect { pane: PaneId, press_x: u16, press_y: u16, enter_copy: bool },
    /// A drag's anchor has been installed (either directly, for double/
    /// triple-click word/line selection, or via `PendingSelect`'s first
    /// `Drag`); each subsequent `Drag1` extends the selection's cursor
    /// endpoint and sets `moved`, and the eventual `Up1` copies it
    /// (`copy-selection-and-cancel`, matching tmux's `MouseDragEnd1Pane`
    /// default) ONLY if `moved` is true AND the release lands on the same
    /// pane the selection is bound to (tmux resolves `MouseDragEnd1Pane`
    /// against the pane under the pointer AT RELEASE, not the drag-origin
    /// pane — releasing elsewhere has no copy-mode binding there, so no
    /// copy). Real tmux's copy-mode binding table has no default for a bare
    /// `MouseUp1Pane` (release with no prior drag) — `Down` always arms
    /// `Selecting { moved: false }` (via double/triple-click) or
    /// `PendingSelect` (via a plain first click), since SGR button-event
    /// tracking guarantees an `Up` after every `Down` even with zero
    /// motion, and without this flag a plain click would always look like a
    /// (zero-width) completed drag.
    Selecting { moved: bool },
}

/// Advance `m`'s click-run-length tracker for a `Down` at `(x, y)` with
/// button `btn`, returning the new run length (1 for a fresh click, 2/3 for
/// a same-cell-same-button double/triple click within
/// [`MOUSE_CLICK_WINDOW`]). Updates `m.last_click` as a side effect so the
/// NEXT click chains off this one.
fn advance_click_run(m: &mut MouseClientState, now: Instant, x: u16, y: u16, btn: u8) -> u8 {
    let run = match m.last_click {
        Some((t, lx, ly, lb, r)) if lb == btn && lx == x && ly == y && now.duration_since(t) <= MOUSE_CLICK_WINDOW => {
            (r + 1).min(3)
        }
        _ => 1,
    };
    m.last_click = Some((now, x, y, btn, run));
    run
}

/// All server state, owned by the single main-loop thread — no locks.
struct Server {
    registry: Registry,
    panes: HashMap<PaneId, PaneRuntime>,
    /// Last rect applied to each pane (pty resize + grid resize), so
    /// `apply_layout` only touches panes whose rect actually changed.
    last_rects: HashMap<PaneId, Rect>,
    clients: HashMap<ClientId, ClientState>,
    /// Writer channels for connections that haven't completed `Attach` yet.
    pending_writers: HashMap<ClientId, Sender<Vec<u8>>>,
    next_pane_id: PaneId,
    next_client_id: ClientId,
    /// Set the first time any session is created; `run`'s exit-empty check
    /// only fires once this is true (an empty registry at STARTUP, before
    /// any client has attached, must not be mistaken for exit-empty).
    had_session: bool,
    clock: String,
    tx: Sender<ServerEvent>,
    /// Typed tmux option registry (Task 6): `prefix`, `default-command`,
    /// `renumber-windows`, styles, etc. One global instance (SP3 scope, no
    /// per-session/window overlays — documented deviation).
    options: Options,
    /// Mutable key-bindings table (`bind-key`/`unbind-key`/`list-keys`).
    /// `Bindings::default()` reproduces every legacy hardcoded binding.
    bindings: Bindings,
    /// Set by `run` after startup config loading IF at least one config
    /// error was collected (`config: N error(s), see server.log`); consumed
    /// (`Option::take`) by `finish_attach` the first time ANY client
    /// attaches, so only the first attach ever sees it (Task 7).
    pending_config_message: Option<String>,
    /// Computer name for the `#H` format code, queried once at startup
    /// (Task 8; `GetComputerNameW` — a hostname doesn't change under a
    /// running server).
    hostname: String,
    /// Server-global paste buffers (Task 3, sub-project 4): `copy-
    /// selection-and-cancel`'s automatic buffers and `set-buffer`'s named
    /// ones. One instance, shared by every session/client (tmux itself
    /// scopes buffers server-wide too, not per-session).
    buffers: Buffers,
    /// tmux's per-pane `active_point` counter (`window.c:593`; SP6 parity
    /// wave 2, Task 3), global across the whole server -- matching tmux,
    /// whose counter is meaningful across windows/sessions, not scoped to
    /// one. Stamped by [`Server::stamp_active`] at every
    /// `window_set_active_pane`-equivalent call site (see that method's doc
    /// for the full stamp/no-stamp map -- death handoffs deliberately do
    /// NOT stamp) so `Layout::focus_dir`'s MRU tie-break can rank
    /// candidates by real recency instead of the old single-slot
    /// `last_focused` approximation (closes follow-up #65). A pane with no
    /// entry here has never been focused; `next_active_point` starts at 1
    /// so that default (read as 0) never collides with a real stamp.
    /// Entries are pruned wherever panes are dropped (mirrors `last_rects`
    /// cleanup exactly).
    pane_activity: HashMap<PaneId, u64>,
    next_active_point: u64,
}

/// Local wall-clock time formatted `HH:MM DD-Mon-YY`. Duplicated privately
/// from `app.rs` (which dies in Task 8) rather than shared. Since SP3 Task 8
/// the status bar's right side is rendered via `expand_format` instead, but
/// this string is still the `Tick` handler's change detector: a re-render is
/// triggered whenever it changes (minute granularity — matching the default
/// `status-right`; a custom `%S`-bearing format only refreshes when the
/// minute flips, documented SP4 refinement alongside the stored-but-unused
/// `status-interval`).
fn local_clock() -> String {
    // SAFETY: no preconditions; windows 0.58 returns the SYSTEMTIME by value.
    let st = unsafe { GetLocalTime() };
    let month = MONTHS[(st.wMonth.clamp(1, 12) as usize) - 1];
    let (hh, mm, dd, yy) = (st.wHour, st.wMinute, st.wDay, st.wYear % 100);
    format!("{hh:02}:{mm:02} {dd:02}-{month}-{yy:02}")
}

/// Plain calendar/time facts for `expand_format`'s strftime subset, from
/// `GetLocalTime` (shared by `render_one`'s status-left/right expansion and
/// `dispatch.rs`'s `display-message`/`confirm-before -p` expansion).
fn system_time_parts() -> SystemTimeParts {
    // SAFETY: no preconditions; windows 0.58 returns the SYSTEMTIME by value.
    let st = unsafe { GetLocalTime() };
    SystemTimeParts {
        year: st.wYear as i32,
        month: st.wMonth as u8,
        day: st.wDay as u8,
        weekday: st.wDayOfWeek as u8,
        hour: st.wHour as u8,
        min: st.wMinute as u8,
        sec: st.wSecond as u8,
    }
}

/// Computer name for the `#H` format code (Task 8), queried once at server
/// startup via `GetComputerNameW`; falls back to the `COMPUTERNAME` env var
/// (empty string if neither works).
fn computer_name() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: `buf`/`len` outlive the call; `len` is in/out (chars written).
    let ok = unsafe { GetComputerNameW(windows::core::PWSTR(buf.as_mut_ptr()), &mut len) };
    if ok.is_ok() {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// Truncate to the first `max` chars (tmux `status-left-length` /
/// `status-right-length`). Applied while BUILDING the status strings; the
/// renderer's spatial right-truncation (when left + right don't fit the
/// terminal width) still applies on top of these caps.
fn truncate_chars(s: &str, max: u16) -> String {
    s.chars().take(max as usize).collect()
}

/// Encode and send one `ServerMsg` (small, never chunked: `Exit`/`CliDone`).
fn send_msg(tx: &Sender<Vec<u8>>, msg: &ServerMsg) {
    let mut buf = Vec::new();
    if write_server_msg(&mut buf, msg).is_ok() {
        let _ = tx.send(buf);
    }
}

/// Encode and send an `Output` payload, chunked so no single frame's
/// declared length reaches `MAX_FRAME` (the codec itself does not enforce
/// this on the write side — see the task brief).
fn send_output(tx: &Sender<Vec<u8>>, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    for chunk in bytes.chunks(protocol::MAX_FRAME as usize) {
        send_msg(tx, &ServerMsg::Output(chunk.to_vec()));
    }
}

/// Spawn a shell in a fresh ConPTY and wire its two worker threads (output
/// reader + process-exit waiter) into the shared event channel. `shell` is
/// the `default-command` option's current value (SP3 Task 6: configurable
/// per `set -g default-command`, replacing the old hardcoded `SHELL`
/// const).
fn spawn_pane(
    id: PaneId,
    cols: u16,
    rows: u16,
    tx: &Sender<ServerEvent>,
    shell: &str,
    history_limit: u32,
) -> std::io::Result<PaneRuntime> {
    let mut pty = Pty::spawn(shell, cols.max(1), rows.max(1))?;
    let mut reader = pty.take_reader()?;

    let out_tx = tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.send(ServerEvent::Output(id, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let wait_tx = tx.clone();
    let raw = pty.process_handle_raw();
    thread::spawn(move || {
        // SAFETY: `raw` is a live process HANDLE owned by the Pty, which the
        // main thread keeps alive until after this pane's Exited is handled.
        unsafe { WaitForSingleObject(HANDLE(raw as *mut core::ffi::c_void), INFINITE) };
        let _ = wait_tx.send(ServerEvent::Exited(id));
    });

    // Per-pane writer thread (follow-up #14): owns an independent duplicate
    // of the pty's input write handle and drains an unbounded channel of
    // pending writes, mirroring `spawn_writer`'s per-client design exactly —
    // see `PaneRuntime::input_tx`'s doc comment.
    let mut writer = pty.try_clone_writer()?;
    let (input_tx, input_rx) = channel::<Vec<u8>>();
    thread::spawn(move || {
        while let Ok(bytes) = input_rx.recv() {
            if writer.write(&bytes).is_err() {
                break;
            }
        }
    });

    let grid = Grid::new(cols.max(1), rows.max(1), history_limit);
    Ok(PaneRuntime { pty: Some(pty), grid, dead: false, title: String::new(), input_tx })
}

/// Recognized Windows executable/script suffixes stripped from an
/// automatic-rename candidate before it becomes a window name (fix round,
/// review finding 2) -- see `derive_auto_name`'s doc comment for why.
const KNOWN_TITLE_EXTENSIONS: [&str; 4] = [".exe", ".cmd", ".bat", ".ps1"];

/// Strip ONE trailing recognized extension (case-insensitive) if present;
/// otherwise return `token` unchanged. Never strips a token down to empty
/// (a token that's nothing BUT the extension, e.g. a literal `.exe`, is
/// left alone -- `derive_auto_name`'s later empty check then correctly
/// falls back to "keep the existing name" instead of manufacturing an
/// empty one from a pathological title).
fn strip_known_extension(token: &str) -> &str {
    for ext in KNOWN_TITLE_EXTENSIONS {
        if token.len() > ext.len() && token[token.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            return &token[..token.len() - ext.len()];
        }
    }
    token
}

/// Derive an automatic-rename candidate from a pane's OSC title (Task 9,
/// sub-project 4; design spec `## 9. automatic-rename`): strip a path prefix
/// down to its basename (last component split on either `\` or `/`), cut at
/// the first space (tmux's automatic-rename takes just the leading
/// "command" token, not the whole title), strip one recognized trailing
/// extension (`.exe`/`.cmd`/`.bat`/`.ps1`, case-insensitive), re-strip
/// control characters defensively (`grid::Grid`'s own OSC handler already
/// does this for the title it hands out, but this function doesn't assume
/// its caller is always that one guaranteed-clean source), sanitize any
/// residual `:`/`.`, and cap at 20 chars LAST (so the cap never truncates
/// mid-sanitization).
///
/// **Fix round, review finding 2**: the original implementation rejected
/// the WHOLE candidate via `model::validate_name` the instant it contained
/// a `:` or `.` -- but a bare Windows executable's stock, un-customized
/// console title is commonly its own exe name or full path, extension and
/// all (`cmd.exe`, `C:\Windows\system32\cmd.exe`; Windows defaults a fresh
/// console's title to the launched executable's path whenever the program
/// never calls `SetConsoleTitle` itself), so that gate silently no-op'd
/// automatic-rename for a large, common class of default titles --
/// including, concretely, the very first title `powershell.exe` itself
/// emits on pane startup.
///
/// The fix is EXTENSION-STRIPPING, not blanket character substitution:
/// `cmd.exe` -> `cmd`; `C:\Windows\system32\cmd.exe` -> `cmd` (path already
/// stripped by the basename step). Substituting `:`/`.` with `-` instead
/// (tried first, then reverted) technically also "worked" (`cmd.exe` ->
/// `cmd-exe`) but regressed several pre-existing tests
/// (`pane_title_updates_window_name` et al.): `powershell.exe` ->
/// `powershell-exe`, which does NOT equal the window's existing default
/// name `powershell`, so `maybe_auto_rename` treats it as a genuine change
/// and fires a VISIBLE rename on literally every fresh pane the instant it
/// attaches -- not what any test (or user) expects, and further from real
/// tmux's extension-less naming (`bash`/`zsh`) than stripping is.
/// Extension-stripping makes `powershell.exe` -> `powershell`, which DOES
/// equal the existing default, so that no-op check
/// (`window.name == name`) absorbs the startup title silently: no spurious
/// rename, no regression, and closer tmux parity for the common case. Any
/// OTHER residual `:`/`.` that survives extension-stripping (an
/// unrecognized extension, or a literal colon like `server:8080`) is still
/// sanitized -- replaced with `-` -- rather than rejected outright, so the
/// candidate stays useful instead of vanishing. Character substitution
/// (rather than simply widening `validate_name` to allow `:`/`.` for every
/// manually-typed name too) is deliberate: window names double as
/// `session:window`/`session.window` target syntax
/// (`split_session_prefix`/`resolve_window`'s window.pane split in
/// `src/server/dispatch.rs` both split on the FIRST occurrence of the
/// separator), so an embedded `:`/`.` in the name itself would be
/// ambiguous with the separator -- a materially larger, riskier change
/// than this fix round's scope.
///
/// Returns `None` only if the result is empty (caller keeps the window's
/// existing name) -- with `:`/`.` no longer able to trigger outright
/// rejection, `validate_name` here is now a defensive double-check (control
/// chars are already stripped above) rather than the primary gate.
fn derive_auto_name(title: &str) -> Option<String> {
    let basename = title.rsplit(['\\', '/']).next().unwrap_or(title);
    let first_token = basename.split(' ').next().unwrap_or("");
    let stripped = strip_known_extension(first_token);
    let cleaned: String = stripped
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == ':' || c == '.' { '-' } else { c })
        .take(20)
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    crate::model::validate_name(&cleaned, "window").ok()?;
    Some(cleaned)
}

/// Resize every pane whose computed rect changed (pty + grid), caching the
/// last applied rect per pane so unchanged panes are skipped. Same shape as
/// `app.rs`'s `apply_layout`, keyed by `HashMap` instead of a `Vec` (panes
/// now span every session/window, not just one flat list).
fn apply_layout(
    layout: &Layout,
    area: Rect,
    panes: &mut HashMap<PaneId, PaneRuntime>,
    last_rects: &mut HashMap<PaneId, Rect>,
) {
    for (id, rect) in layout.rects(area) {
        if last_rects.get(&id) == Some(&rect) {
            continue;
        }
        if let Some(p) = panes.get_mut(&id) {
            if let Some(pty) = p.pty.as_ref() {
                let _ = pty.resize(rect.w.max(1), rect.h.max(1));
            }
            p.grid.resize(rect.w.max(1), rect.h.max(1));
        }
        last_rects.insert(id, rect);
    }
}

/// Writer thread: owns the write half of the connection, drains an
/// unbounded channel of already-encoded frame bytes so a slow/blocked
/// client can never stall the main loop.
fn spawn_writer(mut conn: PipeConn) -> Sender<Vec<u8>> {
    let (tx, rx) = channel::<Vec<u8>>();
    thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            if conn.write_all(&bytes).is_err() {
                break;
            }
        }
    });
    tx
}

/// Reader thread: decodes client frames until EOF/error, forwarding each to
/// the main loop; a read error (including clean EOF) reports `ClientGone`.
fn spawn_client_reader(id: ClientId, mut conn: PipeConn, tx: Sender<ServerEvent>) {
    thread::spawn(move || loop {
        match read_client_msg(&mut conn) {
            Ok(msg) => {
                if tx.send(ServerEvent::FromClient(id, msg)).is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = tx.send(ServerEvent::ClientGone(id));
                break;
            }
        }
    });
}

/// `(` / `)` — move `client` to the session adjacent to `*session_name` in
/// registry creation order (wraps). Returns `None` (no-op) with a single
/// session or if the current session is somehow already gone. On an actual
/// switch, updates `client.session`/`*session_name` and forces a full
/// repaint (`Renderer::resize` unconditionally sets `force_full`), and
/// returns `(old_name, new_name)` so the caller can recompute both
/// sessions' sizes/layouts once `client` is back in `self.clients`.
fn switch_client_session(
    registry: &mut Registry,
    client: &mut ClientState,
    session_name: &mut String,
    next: bool,
) -> Option<(String, String)> {
    let neighbor = registry.neighbor_session(session_name, next)?.to_string();
    if neighbor == *session_name {
        return None;
    }
    let old = std::mem::replace(session_name, neighbor.clone());
    client.session = Some(neighbor.clone());
    client.renderer.resize(client.cols.max(1), client.rows.max(1));
    Some((old, neighbor))
}

/// One config file to attempt loading at server startup (Task 7):
/// `required` distinguishes an explicitly-requested file (`-f`/`--config`)
/// from a default-chain candidate. See `discover_config_files`.
struct ConfigCandidate {
    path: std::path::PathBuf,
    /// `true` for an explicit `--config`/`-f` file: a missing file is a
    /// collected error. `false` for a default-chain candidate (`.tmux.conf`/
    /// `.winmux.conf`): a MISSING file is silently skipped (tmux behavior —
    /// most users have no config at all); any OTHER open error (e.g.
    /// permissions) is still collected even for a non-required candidate.
    required: bool,
}

/// Pure discovery of which config file(s) `run` should try loading, in
/// order, and whether each is required. Existence is NOT checked here
/// (`Server::load_config_files` does that) — this only decides the
/// candidate list and the required/optional distinction, so it can be unit
/// tested without touching the filesystem or process environment (see the
/// task brief: mutating `std::env` in parallel tests is racy).
///
/// - `explicit` non-empty (server `--config <path>`, repeatable, forwarded
///   from the CLI's `-f`) REPLACES the default chain entirely, in the order
///   given, each `required: true`. The special value `-` (Task 7 review
///   fix; the tmux `-f /dev/null` idiom) is dropped from the candidate
///   list but still counts as "explicit was given" — so `--config -` alone
///   means NO config at all (no default chain, no candidates, no errors);
///   this is also the test suite's isolation seam (`tests/server_proto.rs`'s
///   `start_server` passes `["-"]` so a real `%USERPROFILE%\.tmux.conf` on
///   a dev/CI machine can never contaminate a test's server).
/// - Otherwise, the default chain: the first existing of
///   `$XDG_CONFIG_HOME/tmux/tmux.conf` (only when `xdg` is `Some` and
///   non-empty) or `%USERPROFILE%\.tmux.conf`, loaded FIRST; then
///   `%USERPROFILE%\.winmux.conf`, loaded SECOND (so winmux-specific
///   tweaks in a ported tmux config can be overridden by winmux-only
///   settings) — both `required: false`.
fn discover_config_files(xdg: Option<&str>, userprofile: Option<&str>, explicit: &[String]) -> Vec<ConfigCandidate> {
    if !explicit.is_empty() {
        return explicit
            .iter()
            .filter(|p| p.as_str() != "-")
            .map(|p| ConfigCandidate { path: std::path::PathBuf::from(p), required: true })
            .collect();
    }
    let mut out = Vec::new();
    let tmux_conf = match xdg.filter(|s| !s.is_empty()) {
        Some(x) => Some(std::path::PathBuf::from(x).join("tmux").join("tmux.conf")),
        None => userprofile.map(|u| std::path::PathBuf::from(u).join(".tmux.conf")),
    };
    if let Some(p) = tmux_conf {
        out.push(ConfigCandidate { path: p, required: false });
    }
    if let Some(u) = userprofile {
        out.push(ConfigCandidate { path: std::path::PathBuf::from(u).join(".winmux.conf"), required: false });
    }
    out
}

impl Server {
    fn new(tx: Sender<ServerEvent>) -> Self {
        Server {
            registry: Registry::new(),
            panes: HashMap::new(),
            last_rects: HashMap::new(),
            clients: HashMap::new(),
            pending_writers: HashMap::new(),
            next_pane_id: 1,
            next_client_id: 1,
            had_session: false,
            clock: local_clock(),
            tx,
            options: Options::new(),
            bindings: Bindings::default(),
            pending_config_message: None,
            hostname: computer_name(),
            buffers: Buffers::new(),
            pane_activity: HashMap::new(),
            next_active_point: 1,
        }
    }

    /// Stamp `id` as the most recently active pane (tmux's `active_point`,
    /// `window.c:593`; SP6 parity wave 2, Task 3). `saturating_add`: a u64
    /// counter wrapping after ~1.8e19 focus changes is not a real concern,
    /// but keeping the bump total (never panicking) matches this codebase's
    /// convention for counters that are conceptually "never overflows"
    /// (follow-up #5's spirit).
    ///
    /// Stamping happens ONLY where tmux calls `window_set_active_pane`
    /// (which is the sole place `active_point` is ever bumped):
    /// - explicit selection (`select-pane -t`, `exec_select_pane`'s target
    ///   branch) and directional navigation (`focus_dir` commit);
    /// - `exec_last_pane` (the `prefix ;` toggle);
    /// - mouse click focus / `display-panes` digit jump
    ///   (`mouse_focus_pane`);
    /// - `rotate-window` (`cmd-rotate-window.c:109` calls
    ///   `window_set_active_pane` -- `exec_rotate_window`);
    /// - non-detached pane/window SPAWN (`spawn.c`: `exec_split_window`,
    ///   `exec_new_window`, `exec_new_session` -- a freshly spawned pane
    ///   takes focus with a bump).
    ///
    /// NEVER on death handoffs: tmux's `window_lost_pane` (window.c)
    /// reassigns `w->active` directly (last_panes stack -> prev -> next)
    /// with NO `active_point` bump, so `kill_pane_by_id`,
    /// `exec_break_pane`'s source-window reassignment, and
    /// `handle_exited`'s natural-exit reassignment all deliberately leave
    /// the surviving pane's historical recency untouched (fix round 3,
    /// controller-verified against the tmux C source -- do not "fix" them
    /// by adding stamps).
    ///
    /// NEVER on break-pane's moved pane either (fix round 4): the classic
    /// break-pane path (cmd-break-pane.c:153-158) sets `w->active = wp` by
    /// DIRECT assignment, no bump -- tmux distinguishes a freshly SPAWNED
    /// pane (stamped) from break-pane's RECYCLED pane (not stamped; the
    /// `window_set_active_pane` at cmd-break-pane.c:80 belongs to the `-W`
    /// floating feature only, which winmux doesn't implement). So
    /// `exec_break_pane` stamps NOTHING, on either side.
    ///
    /// Also deliberately NOT stamped: `exec_select_window`/
    /// `exec_step_window`/`exec_last_window` -- switching windows changes
    /// the session's current *winlink* only, never any window's active
    /// pane, so tmux doesn't bump `active_point` there either.
    fn stamp_active(&mut self, id: PaneId) {
        let point = self.next_active_point;
        self.next_active_point = self.next_active_point.saturating_add(1);
        self.pane_activity.insert(id, point);
    }

    /// How many rows the status bar takes out of a client's contribution to
    /// its session's shared pane area (`status off` frees the row).
    fn status_rows(&self) -> u16 {
        if self.options.status_on() {
            1
        } else {
            0
        }
    }

    /// The y origin of every session's pane area: row 1 when the status bar
    /// sits on top, else row 0.
    fn pane_area_y(&self) -> u16 {
        if self.options.status_on() && self.options.status_position_top() {
            1
        } else {
            0
        }
    }

    fn mint_pane_id(&mut self) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    /// Dispatch one event; returns whether a render pass is warranted this
    /// turn (before coalescing — the caller ORs these across a whole batch).
    fn handle_event(&mut self, ev: ServerEvent) -> bool {
        match ev {
            ServerEvent::Output(id, bytes) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.grid.feed(&bytes);
                    // automatic-rename (Task 9, sub-project 4): OSC 0/2
                    // titles are edge-triggered (`take_title_changed`) --
                    // refresh the cached `#T` value and, if this pane is the
                    // ACTIVE pane of some window, consider renaming it.
                    if p.grid.take_title_changed() {
                        let title = p.grid.title().unwrap_or("").to_string();
                        p.title = title.clone();
                        self.maybe_auto_rename(id, &title);
                    }
                }
                true
            }
            ServerEvent::Exited(id) => self.handle_exited(id),
            ServerEvent::Connected(conn) => {
                self.handle_connected(conn);
                false
            }
            ServerEvent::FromClient(id, msg) => self.handle_client_msg(id, msg),
            ServerEvent::ClientGone(id) => {
                self.handle_client_gone(id);
                false
            }
            ServerEvent::Tick => {
                let now = local_clock();
                let mut dirty = if now != self.clock {
                    self.clock = now;
                    true
                } else {
                    false
                };
                let deadline = Instant::now();
                // escape-time (Task 9, sub-project 4): collected during the
                // borrow-checked `iter_mut` pass below (can't remove a
                // client from `self.clients` mid-iteration) and processed
                // just after it ends.
                let mut escape_flush: Vec<ClientId> = Vec::new();
                // Copy-mode drag autoscroll (Task 7, SP6 wave 2): collected
                // here (same borrow-checked reason as `escape_flush` --
                // servicing needs `self.panes`/`self.clients.get_mut`
                // together, which the ongoing `self.clients.iter_mut()`
                // above can't provide) and serviced just after this loop
                // ends.
                let mut autoscroll_due: Vec<ClientId> = Vec::new();
                // clock-mode (Task 10): the current formatted time string,
                // computed ONCE per tick (same wall time for every client)
                // rather than per-client -- compared against each Clock-mode
                // client's stored `text` below, only marking dirty (and
                // rebuilding) on an actual change, matching real tmux's own
                // "redraw only if the time actually changed" rule
                // (`window-clock.c:146-168`). Real time, therefore untested
                // at unit level beyond `format_clock` itself (the pure
                // formatting seam) -- see that function's test module.
                let clock_style12 = self.options.clock_mode_style_12();
                let clock_now = system_time_parts();
                for (cid, client) in self.clients.iter_mut() {
                    if let Some((_, set_at)) = client.message {
                        if deadline.duration_since(set_at) >= MESSAGE_LIFETIME {
                            client.message = None;
                            dirty = true;
                        }
                    }
                    // display-panes (Task 8): auto-dismiss once its deadline
                    // has passed -- this 50ms tick is the same mechanism
                    // `MESSAGE_LIFETIME` expiry already uses above.
                    let expired = matches!(&client.mode, ClientMode::DisplayPanes(s) if deadline >= s.deadline);
                    if expired {
                        client.mode = ClientMode::Normal;
                        dirty = true;
                    }
                    if let ClientMode::Clock(cs) = &mut client.mode {
                        let text = format_clock(clock_now.hour, clock_now.min, clock_style12);
                        if text != cs.text {
                            cs.text = text;
                            dirty = true;
                        }
                    }
                    if client.key_machine.escape_ready(deadline) {
                        escape_flush.push(*cid);
                    }
                    if let Some(a) = client.mouse.autoscroll {
                        if deadline >= a.deadline {
                            autoscroll_due.push(*cid);
                        }
                    }
                }
                // Autoscroll tick: one line of scroll (+ selection re-snap)
                // per due client, then re-arm for the next
                // `MOUSE_DRAG_AUTOSCROLL_INTERVAL` -- matches tmux's
                // `dragtimer` callback re-checking and re-arming itself each
                // time (`mouse.md` §5.4). `self` is fully free again here
                // (the `iter_mut` above has ended), so this can freely touch
                // `self.panes`/`self.options` alongside `self.clients`.
                for cid in autoscroll_due {
                    if self.service_autoscroll_tick(cid, deadline) {
                        dirty = true;
                    }
                }
                // escape-time flush: a lone/partial pending ESC older than
                // `escape-time` is force-drained through
                // `KeyMachine::flush_now` and its resulting events processed
                // through the SAME pipeline a live `Stdin` frame uses
                // (`process_key_events`) -- this is what finally delivers a
                // bare Escape keypress the decoder alone can never resolve
                // (see that method's doc comment and the design spec's `## 8.
                // escape-time` section).
                for cid in escape_flush {
                    let Some(mut client) = self.clients.remove(&cid) else { continue };
                    let Some(session_name) = client.session.clone() else {
                        self.clients.insert(cid, client);
                        continue;
                    };
                    let events = client.key_machine.flush_now(deadline);
                    if events.is_empty() {
                        self.clients.insert(cid, client);
                        continue;
                    }
                    dirty = true;
                    let queue: VecDeque<KeyInputEvent> = events.into();
                    self.process_key_events(cid, client, queue, session_name, deadline);
                }
                dirty
            }
        }
    }

    /// automatic-rename (Task 9, sub-project 4): if `pane_id` is the ACTIVE
    /// pane of some window (`window.layout.focused() == pane_id`), and both
    /// the global `automatic-rename` option and that window's own
    /// `auto_rename` flag are on, rename the window from `title` (via
    /// `derive_auto_name`), throttled to at most one rename per window per
    /// [`AUTO_RENAME_THROTTLE`]. A no-op if the pane isn't any window's
    /// active pane (a background pane's title changing never renames
    /// anything -- matches tmux, and naturally makes "switching active pane
    /// switches the tracked title source" fall out for free: the next
    /// title-change event on whichever pane IS active at the time is what
    /// gets read here). Does NOT touch `window.last_auto_rename` unless an
    /// actual rename happens, so a title that keeps re-deriving to the
    /// SAME name never gets throttled against a later, genuinely different
    /// one.
    fn maybe_auto_rename(&mut self, pane_id: PaneId, title: &str) {
        if !self.options.automatic_rename() {
            return;
        }
        let Some(name) = derive_auto_name(title) else { return };
        let target = self.registry.sessions().iter().find_map(|s| {
            s.windows
                .iter()
                .find(|w| w.layout.focused() == pane_id)
                .map(|w| (s.name.clone(), w.id))
        });
        let Some((session_name, wid)) = target else { return };
        let Some(session) = self.registry.session_mut(&session_name) else { return };
        let Some(window) = session.windows.iter_mut().find(|w| w.id == wid) else { return };
        if !window.auto_rename || window.name == name {
            return;
        }
        let now = Instant::now();
        if let Some(last) = window.last_auto_rename {
            if now.duration_since(last) < AUTO_RENAME_THROTTLE {
                return;
            }
        }
        window.name = name;
        window.last_auto_rename = Some(now);
    }

    fn handle_connected(&mut self, conn: PipeConn) {
        let id = self.next_client_id;
        self.next_client_id += 1;
        let reader_conn = match conn.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        };
        let writer_tx = spawn_writer(conn);
        spawn_client_reader(id, reader_conn, self.tx.clone());
        self.pending_writers.insert(id, writer_tx);
    }

    fn handle_client_gone(&mut self, id: ClientId) {
        self.pending_writers.remove(&id);
        if let Some(client) = self.clients.remove(&id) {
            if let Some(name) = client.session {
                self.recompute_session_size(&name);
                self.apply_layout_for_session(&name);
            }
        }
    }

    fn handle_client_msg(&mut self, id: ClientId, msg: ClientMsg) -> bool {
        match msg {
            ClientMsg::Attach { mode, detach_others, cols, rows, name } => {
                self.handle_attach(id, mode, detach_others, cols, rows, name);
            }
            ClientMsg::Stdin(bytes) => self.handle_stdin(id, bytes),
            ClientMsg::Resize { cols, rows } => self.handle_resize(id, cols, rows),
            ClientMsg::Detach => self.handle_detach_frame(id),
            ClientMsg::Cli(argv) => self.handle_cli(id, argv),
        }
        true
    }

    fn handle_attach(
        &mut self,
        id: ClientId,
        mode: AttachMode,
        detach_others: bool,
        cols: u16,
        rows: u16,
        name: String,
    ) {
        let writer_tx = match self.pending_writers.remove(&id) {
            Some(tx) => tx,
            None => return, // already attached, or unknown client id
        };
        let pane_rows = rows.saturating_sub(self.status_rows()).max(1);
        let size = (cols.max(1), pane_rows);

        match mode {
            AttachMode::NewAuto => {
                let pane_id = self.mint_pane_id();
                let shell = self.options.default_command().to_string();
                let history_limit = self.options.history_limit();
                match spawn_pane(pane_id, size.0, size.1, &self.tx, &shell, history_limit) {
                    Ok(pr) => {
                        self.panes.insert(pane_id, pr);
                        let base_index = self.options.base_index();
                        let session_name = self
                            .registry
                            .create_session(None, pane_id, size, base_index)
                            .expect("auto-assigned name never duplicates")
                            .name
                            .clone();
                        self.finish_attach(id, writer_tx, session_name, cols, rows);
                    }
                    Err(e) => {
                        send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: format!("failed to spawn shell: {e}") });
                    }
                }
            }
            AttachMode::NewNamed => {
                if self.registry.sessions().iter().any(|s| s.name == name) {
                    send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: format!("duplicate session: {name}") });
                    return;
                }
                let pane_id = self.mint_pane_id();
                let shell = self.options.default_command().to_string();
                let history_limit = self.options.history_limit();
                match spawn_pane(pane_id, size.0, size.1, &self.tx, &shell, history_limit) {
                    Ok(pr) => {
                        self.panes.insert(pane_id, pr);
                        let base_index = self.options.base_index();
                        match self.registry.create_session(Some(&name), pane_id, size, base_index) {
                            Ok(session) => {
                                let n = session.name.clone();
                                self.finish_attach(id, writer_tx, n, cols, rows);
                            }
                            Err(e) => {
                                // Roll back: drop the just-spawned pane (kills the shell).
                                self.panes.remove(&pane_id);
                                send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: e });
                            }
                        }
                    }
                    Err(e) => {
                        send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: format!("failed to spawn shell: {e}") });
                    }
                }
            }
            AttachMode::Existing => match self.registry.find(&name) {
                Ok(session) => {
                    let session_name = session.name.clone();
                    if detach_others {
                        self.detach_others(&session_name);
                    }
                    self.finish_attach(id, writer_tx, session_name, cols, rows);
                }
                Err(e) => send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: e }),
            },
        }
    }

    /// Common tail of a successful attach: register the client, then
    /// recompute the session's shared size and reapply its layout.
    fn finish_attach(&mut self, id: ClientId, tx: Sender<Vec<u8>>, session_name: String, cols: u16, rows: u16) {
        // Follow-up #21: `Renderer::new` starts with `force_full: false`, so
        // constructing at the real size and then immediately `resize`-ing to
        // that SAME size (as this used to do) allocated the front/back
        // buffers twice for no reason — `resize`'s only OTHER job, setting
        // `force_full: true` so the very first `compose()` is a guaranteed
        // full repaint (see module docs / task brief: "use a fresh Renderer
        // (or resize) at attach"), doesn't need a same-size `new` first.
        let mut renderer = Renderer::new(0, 0);
        renderer.resize(cols.max(1), rows.max(1));
        // First-attach-only config-error notice (Task 7): `take()` so a
        // SECOND client attaching later never sees it again.
        let message = self.pending_config_message.take().map(|m| (m, Instant::now()));
        // Task 5 (mouse): a newly-attaching client needs the enable sequence
        // too if `mouse` is already on (e.g. set in a `.tmux.conf`, or a
        // second client attaching after a first client turned it on) — sent
        // directly as its own Output frame rather than waiting for the next
        // composed render, matching how `exec_set_option`'s runtime toggle
        // broadcasts it.
        if self.options.mouse() {
            send_output(&tx, MOUSE_ENABLE_SEQ.to_vec());
        }
        let mut key_machine = KeyMachine::new(self.options.prefix());
        // Escape-time (Task 9, sub-project 4): seed from the CURRENT option
        // value (e.g. already set by a `.tmux.conf` loaded at startup) --
        // mirrors `prefix`'s existing at-creation seeding above. Unlike
        // `repeat-time` (a pre-existing gap: only re-synced by a runtime
        // `set`, never at attach time), escape-time getting this wrong would
        // silently break Escape-key delivery for a client that attaches
        // after config already changed it.
        key_machine.set_escape_time(self.options.escape_time());
        let client = ClientState {
            session: Some(session_name.clone()),
            cols,
            rows,
            renderer,
            key_machine,
            mode: ClientMode::Normal,
            message,
            tx,
            mouse: MouseClientState::default(),
        };
        self.clients.insert(id, client);
        self.had_session = true;
        self.recompute_session_size(&session_name);
        self.apply_layout_for_session(&session_name);
    }

    /// `detach_others`: every OTHER client currently attached to
    /// `session_name` gets `Exit{0, "[detached (from session <name>)]"}` —
    /// follow-up #17: previously a bare `[detached]` with no session name,
    /// inconsistent with every OTHER detach exit path in this module (the
    /// `d`-key/`Detach`-frame action and the `detach-client` CLI command both
    /// already name the session). Now identical text to those.
    fn detach_others(&mut self, session_name: &str) {
        let ids: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session.as_deref() == Some(session_name))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(c) = self.clients.remove(&id) {
                send_msg(&c.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {session_name})]") });
            }
        }
    }

    fn handle_resize(&mut self, id: ClientId, cols: u16, rows: u16) {
        let session_name = match self.clients.get_mut(&id) {
            Some(c) => {
                c.cols = cols;
                c.rows = rows;
                c.renderer.resize(cols.max(1), rows.max(1));
                c.session.clone()
            }
            None => return,
        };
        if let Some(name) = session_name {
            self.recompute_session_size(&name);
            self.apply_layout_for_session(&name);
        }
    }

    fn handle_detach_frame(&mut self, id: ClientId) {
        if let Some(client) = self.clients.remove(&id) {
            let name = client.session.clone().unwrap_or_default();
            send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {name})]") });
            self.recompute_session_size(&name);
            self.apply_layout_for_session(&name);
        }
    }

    fn handle_cli(&mut self, id: ClientId, argv: Vec<String>) {
        let tx = if let Some(c) = self.clients.get(&id) {
            c.tx.clone()
        } else if let Some(tx) = self.pending_writers.get(&id) {
            tx.clone()
        } else {
            return;
        };
        let (code, out, err) = self.execute_cli_argv(&argv);
        send_msg(&tx, &ServerMsg::CliDone { code, out, err });
    }

    /// Rename every attached client's `session` reference from `old` to
    /// `new` (a session's own `name` field is updated by the caller
    /// separately). Needed because clients look their session up by name.
    fn rename_session_everywhere(&mut self, old: &str, new: &str) {
        for c in self.clients.values_mut() {
            if c.session.as_deref() == Some(old) {
                c.session = Some(new.to_string());
            }
        }
    }

    /// Cancel any attached client's pending confirm-before whose snapshotted
    /// pane/window no longer exists (it exited naturally, or another client
    /// killed it, while the `(y/n)` prompt was up): reset the mode to
    /// `Normal`, drop capture (so the next key is normal input again, e.g. a
    /// `y` gets FORWARDED to the pane instead of confirming), and clear any
    /// transient message. `dispatch::Server::feed_confirm_byte` ALSO
    /// re-validates its target before acting (belt and braces — this method
    /// can't reach a client currently removed from the map mid-
    /// `handle_stdin`), but only this path clears a stale on-screen prompt
    /// without waiting for a keypress.
    fn cancel_stale_confirms(&mut self) {
        let mut live_panes: HashSet<PaneId> = HashSet::new();
        let mut live_windows: HashSet<WindowId> = HashSet::new();
        for s in self.registry.sessions() {
            for w in &s.windows {
                live_windows.insert(w.id);
                live_panes.extend(w.layout.panes());
            }
        }
        for client in self.clients.values_mut() {
            let stale = match &client.mode {
                ClientMode::ConfirmCmd { pane_snapshot, window_snapshot, .. } => {
                    pane_snapshot.is_some_and(|p| !live_panes.contains(&p))
                        || window_snapshot.is_some_and(|w| !live_windows.contains(&w))
                }
                _ => false,
            };
            if stale {
                client.mode = ClientMode::Normal;
                client.key_machine.set_capture(false);
                client.message = None;
            }
        }
    }

    /// Cancel any attached client's copy mode (Task 2) whose bound pane no
    /// longer exists (natural exit / killed while copy mode was active), OR
    /// whose ATTACHED SESSION's current window no longer contains that pane
    /// — the simplest robust way to implement "cancel on any window/session
    /// switch by that client" (design spec `## 2. Copy mode`): a
    /// `select-window`/`next-window`/`switch-client`/etc. dispatch always
    /// changes which window is `session.current`, so re-checking membership
    /// after every dispatch catches all of them uniformly without hooking
    /// each mutating path individually. Called from the same two sites as
    /// `cancel_stale_confirms` (natural pane exit, and once per `Stdin`
    /// frame after the client is back in `self.clients`).
    fn cancel_stale_copy_modes(&mut self) {
        let mut live_panes: HashSet<PaneId> = HashSet::new();
        for s in self.registry.sessions() {
            for w in &s.windows {
                live_panes.extend(w.layout.panes());
            }
        }
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            let stale = match self.clients.get(&id).map(|c| &c.mode) {
                Some(ClientMode::Copy(cs)) => {
                    if !live_panes.contains(&cs.pane) {
                        true
                    } else {
                        match self.clients.get(&id).and_then(|c| c.session.as_deref()) {
                            Some(session_name) => !self
                                .registry
                                .sessions()
                                .iter()
                                .find(|s| s.name == session_name)
                                .map(|s| s.current_window().layout.panes().contains(&cs.pane))
                                .unwrap_or(false),
                            None => true,
                        }
                    }
                }
                _ => false,
            };
            if stale {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.mode = ClientMode::Normal;
                    // Task 4 review fix: a client can be mid-search-prompt
                    // (capture armed via `set_capture(true)`, see
                    // `SearchPrompt`'s doc comment) when its pane dies out
                    // from under it. `cancel_stale_confirms` already turns
                    // capture back off for its own case (see that method) --
                    // do the same here, unconditionally (a harmless no-op
                    // when no search prompt was open), so the next keystroke
                    // routes as normal input again instead of being silently
                    // swallowed as a stray captured byte.
                    client.key_machine.set_capture(false);
                }
            }
        }
    }

    /// Cancel any attached client's clock mode (Task 10) whose bound pane no
    /// longer exists, or is no longer in that client's session's current
    /// window — the exact same staleness rule (and same two call sites) as
    /// [`Server::cancel_stale_copy_modes`], which this mirrors verbatim
    /// (clock mode has no capture/search-prompt state to reset, unlike copy
    /// mode, so this is the simpler half of that method).
    fn cancel_stale_clock_modes(&mut self) {
        let mut live_panes: HashSet<PaneId> = HashSet::new();
        for s in self.registry.sessions() {
            for w in &s.windows {
                live_panes.extend(w.layout.panes());
            }
        }
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            let stale = match self.clients.get(&id).map(|c| &c.mode) {
                Some(ClientMode::Clock(cs)) => {
                    if !live_panes.contains(&cs.pane) {
                        true
                    } else {
                        match self.clients.get(&id).and_then(|c| c.session.as_deref()) {
                            Some(session_name) => !self
                                .registry
                                .sessions()
                                .iter()
                                .find(|s| s.name == session_name)
                                .map(|s| s.current_window().layout.panes().contains(&cs.pane))
                                .unwrap_or(false),
                            None => true,
                        }
                    }
                }
                _ => false,
            };
            if stale {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.mode = ClientMode::Normal;
                }
            }
        }
    }

    /// Cancel any attached client's choose-tree (Task 8) `pending_kill`
    /// confirm whose snapshotted target no longer exists (killed by another
    /// client, or by this same client's own dispatch, while the `x` (y/n)
    /// prompt was up). Navigation/commit never go stale (see
    /// `ClientMode::ChooseTree`'s doc comment for why) — `pending_kill` is
    /// the one piece of state this mode carries across renders, so it is the
    /// only thing this sweep needs to re-validate. Called from the same two
    /// sites as `cancel_stale_confirms`/`cancel_stale_copy_modes`.
    fn cancel_stale_choose_trees(&mut self) {
        let live_sessions: HashSet<String> = self.registry.sessions().iter().map(|s| s.name.clone()).collect();
        let live_windows: HashSet<(String, WindowId)> =
            self.registry.sessions().iter().flat_map(|s| s.windows.iter().map(move |w| (s.name.clone(), w.id))).collect();
        for client in self.clients.values_mut() {
            if let ClientMode::ChooseTree(state) = &mut client.mode {
                if let Some((target, _)) = &state.pending_kill {
                    let alive = match target {
                        TreeTarget::Session(n) => live_sessions.contains(n),
                        TreeTarget::Window(sn, wid) => live_windows.contains(&(sn.clone(), *wid)),
                    };
                    if !alive {
                        state.pending_kill = None;
                    }
                }
            }
        }
    }

    /// Session's shared size = min over its attached clients of
    /// `(cols, rows - status_rows)` (the status row, when on, is not part of
    /// the pane area; `status off` gives panes the full height — Task 8).
    /// No attached clients: keep the last size.
    fn recompute_session_size(&mut self, name: &str) {
        let status_rows = self.status_rows();
        let mut min: Option<(u16, u16)> = None;
        for c in self.clients.values().filter(|c| c.session.as_deref() == Some(name)) {
            let contribution = (c.cols.max(1), c.rows.saturating_sub(status_rows).max(1));
            min = Some(match min {
                Some(m) => (m.0.min(contribution.0), m.1.min(contribution.1)),
                None => contribution,
            });
        }
        if let Some(size) = min {
            if let Some(session) = self.registry.session_mut(name) {
                session.size = size;
            }
        }
    }

    fn apply_layout_for_session(&mut self, name: &str) {
        let area_y = self.pane_area_y();
        let Some(session) = self.registry.session_mut(name) else { return };
        let size = session.size;
        let area = Rect { x: 0, y: area_y, w: size.0, h: size.1 };
        let window = session.current_window_mut();
        apply_layout(&window.layout, area, &mut self.panes, &mut self.last_rects);
    }

    /// Natural pane exit: tmux `remain-on-exit off` parity. If other panes in
    /// the SAME window are still alive, this pane is removed outright (same
    /// path as a confirmed kill) instead of leaving a dead `[exited]`
    /// overlay. If it was the window's last live pane, the whole window
    /// dies; if that was the session's last window, the session dies too
    /// (attached clients get `Exit{0, "[exited]"}`, same as a confirmed
    /// last-pane kill).
    fn handle_exited(&mut self, pane_id: PaneId) -> bool {
        if let Some(p) = self.panes.get_mut(&pane_id) {
            p.pty = None; // drop the Pty immediately (follow-up #1)
            p.dead = true;
        }

        let owner: Option<(String, WindowId)> = self.registry.sessions().iter().find_map(|s| {
            s.windows
                .iter()
                .find(|w| w.layout.panes().contains(&pane_id))
                .map(|w| (s.name.clone(), w.id))
        });
        let Some((session_name, window_id)) = owner else {
            return true;
        };

        let other_panes_alive = self
            .registry
            .sessions()
            .iter()
            .find(|s| s.name == session_name)
            .and_then(|s| s.windows.iter().find(|w| w.id == window_id))
            .map(|w| {
                w.layout
                    .panes()
                    .iter()
                    .any(|pid| *pid != pane_id && !self.panes.get(pid).map(|p| p.dead).unwrap_or(true))
            })
            .unwrap_or(false);

        if other_panes_alive {
            // NOTE (fix round 3, reverting round 2's stamp): `Layout::
            // remove` may hand focus to a surviving sibling here --
            // deliberately NOT stamped. Round 2 stamped it on the premise
            // that tmux routes pane death through `window_set_active_pane`;
            // the controller's direct source check disproved that: tmux's
            // `window_lost_pane` (window.c) reassigns `w->active` directly
            // (last_panes stack -> prev -> next) with NO `active_point`
            // bump, so the survivor keeps its historical recency. See
            // `Server::stamp_active`'s doc for the full stamp/no-stamp map.
            if let Some(session) = self.registry.session_mut(&session_name) {
                if let Some(window) = session.windows.iter_mut().find(|w| w.id == window_id) {
                    window.layout.remove(pane_id);
                }
            }
            self.panes.remove(&pane_id);
            self.last_rects.remove(&pane_id);
            self.pane_activity.remove(&pane_id); // Finding 2 (review): prune, mirrors last_rects
            self.apply_layout_for_session(&session_name);
        } else {
            let is_only_window = self
                .registry
                .sessions()
                .iter()
                .find(|s| s.name == session_name)
                .map(|s| s.windows.len() == 1)
                .unwrap_or(false);
            if is_only_window {
                self.destroy_session(&session_name);
            } else {
                let pane_ids: Vec<PaneId> = self
                    .registry
                    .sessions()
                    .iter()
                    .find(|s| s.name == session_name)
                    .and_then(|s| s.windows.iter().find(|w| w.id == window_id))
                    .map(|w| w.layout.panes())
                    .unwrap_or_default();
                if let Some(session) = self.registry.session_mut(&session_name) {
                    session.kill_window(window_id);
                }
                for pid in pane_ids {
                    self.panes.remove(&pid);
                    self.last_rects.remove(&pid);
                    self.pane_activity.remove(&pid); // Finding 2 (review): prune, mirrors last_rects
                }
                self.apply_layout_for_session(&session_name);
            }
        }
        // The removal above may have invalidated a client's pending confirm;
        // any confirm on it must be reset, or its `y` would act on stale state.
        self.cancel_stale_confirms();
        self.cancel_stale_copy_modes();
        self.cancel_stale_clock_modes();
        self.cancel_stale_choose_trees();
        true
    }

    /// Tear down a session: drop all its panes, remove it from the
    /// registry, and tell every attached client `Exit{0, "[exited]"}`.
    ///
    /// Follow-up #18: the loop below (dropping every pane) runs sequentially
    /// on the main-loop thread. Each pane drop runs a synchronous
    /// `TerminateProcess` plus `ClosePseudoConsole` plus `CloseHandle` (see
    /// `src/pty.rs`'s `Drop for Pty`). This is not a real scaling concern
    /// today. It is bounded by one session's pane count, typically a small
    /// number. Each `TerminateProcess` call is fast. And unlike follow-up
    /// #14's concern (a STALLED child that never drains its stdin), killing
    /// an already-alive process is not something a hung child can
    /// meaningfully stall. Worth revisiting only if a future workflow makes
    /// sessions with very many panes common.
    fn destroy_session(&mut self, name: &str) {
        if let Some(session) = self.registry.session_mut(name) {
            let pane_ids: Vec<PaneId> = session.windows.iter().flat_map(|w| w.layout.panes()).collect();
            for pid in pane_ids {
                self.panes.remove(&pid);
                self.last_rects.remove(&pid);
                self.pane_activity.remove(&pid); // Finding 2 (review): prune, mirrors last_rects
            }
        }
        self.registry.kill_session(name);
        let ids: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session.as_deref() == Some(name))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(client) = self.clients.remove(&id) {
                send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: "[exited]".to_string() });
            }
        }
    }

    /// Route one `Stdin` frame through the client's `KeyMachine` and hand the
    /// resulting events to [`Server::process_key_events`].
    fn handle_stdin(&mut self, id: ClientId, bytes: Vec<u8>) {
        let mut client = match self.clients.remove(&id) {
            Some(c) => c,
            None => return,
        };
        let session_name = match client.session.clone() {
            Some(n) => n,
            None => {
                self.clients.insert(id, client);
                return;
            }
        };
        let now = Instant::now();
        let queue: VecDeque<KeyInputEvent> = client.key_machine.feed(&bytes, now).into();
        self.process_key_events(id, client, queue, session_name, now);
    }

    /// Dispatch one batch of already-decoded key/mouse events against live
    /// state via the command dispatcher (`dispatch.rs`) and the mutable
    /// `bindings` table (see module docs re: the confirm race — NOT fixed
    /// here, same as before Task 6), then re-insert the client (unless it
    /// detached or its session was destroyed). Shared by `handle_stdin` (a
    /// live `Stdin` frame, decoded by `KeyMachine::feed`) and the `Tick`
    /// handler's escape-time flush (Task 9, sub-project 4: a lone pending
    /// ESC older than `escape-time`, decoded by `KeyMachine::flush_now`) —
    /// both just hand this method a `VecDeque<KeyInputEvent>` and the
    /// `Instant` that produced it.
    fn process_key_events(
        &mut self,
        id: ClientId,
        mut client: ClientState,
        mut queue: VecDeque<KeyInputEvent>,
        mut session_name: String,
        now: Instant,
    ) {
        // Any input byte (or flushed escape) from this client clears its
        // transient status message (the other clear path is 750ms elapsing,
        // on Tick).
        client.message = None;

        let mut detach = false;
        let mut destroy = false;
        let mut session_switched: Option<(String, String)> = None;

        'events: while let Some(ev) = queue.pop_front() {
            match ev {
                KeyInputEvent::Forward(data) => {
                    // Copy mode (Task 2): `KeyMachine` coalesces a whole run
                    // of PLAIN unmodified keys (bare letters, digits, Space,
                    // Enter, Tab, BSpace — most copy-mode bindings, e.g.
                    // `q`/`h`/`j`/`k`/`w`) into one `Forward` blob for
                    // throughput, entirely bypassing the `Key{table,..}`
                    // path this module's table-override lives on (see the
                    // `## input-v2` contract's documented deviation). While
                    // in copy mode those bytes must NOT reach the pane —
                    // re-decode the blob back into individual keys (a fresh
                    // `KeyDecoder` reproduces exactly the keys that were
                    // coalesced, since the blob is always a complete,
                    // self-contained run) and resolve each one against the
                    // copy table instead.
                    if matches!(client.mode, ClientMode::Copy(_)) {
                        let mut dec = crate::keys::KeyDecoder::new();
                        let mut decoded = dec.feed(&data);
                        decoded.extend(dec.flush());
                        let which = if self.options.mode_keys_vi() { WhichTable::CopyModeVi } else { WhichTable::CopyMode };
                        for item in decoded {
                            // A coalesced `Forward` blob is built ONLY from
                            // plain-forwardable KEYS (`is_plain_forwardable`
                            // in input.rs) -- a mouse event always reports
                            // its own `KeyInputEvent::Mouse` immediately
                            // instead (see that type's doc comment), so it
                            // can never end up inside a byte blob re-decoded
                            // here. Re-decoding that blob therefore always
                            // reproduces the exact same Key items that were
                            // originally coalesced into it.
                            let crate::keys::DecodedInput::Key(dk) = item else {
                                debug_assert!(false, "Forward blob decoded to a Mouse item");
                                continue;
                            };
                            let binding = self.bindings.lookup(which, &dk.key).cloned();
                            if let Some(b) = binding {
                                let outcome = self.dispatch_client(&b.cmds, &mut client, &mut session_name);
                                dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                                if detach || destroy {
                                    break 'events;
                                }
                            }
                            // Unbound in a copy table: swallowed, matching
                            // the `Key`-path rule.
                        }
                    } else if matches!(client.mode, ClientMode::ChooseTree(_)) {
                        // choose-tree (Task 8): same Forward-blob re-decode
                        // as copy mode above (`j`/`k`/`x`/`q`/digits are all
                        // plain-forwardable) — every decoded key is resolved
                        // against the HARDCODED choose-tree key table (not
                        // `self.bindings` — see `dispatch::
                        // resolve_choose_tree_key`'s doc comment), never
                        // forwarded to the pane underneath.
                        let mut dec = crate::keys::KeyDecoder::new();
                        let mut decoded = dec.feed(&data);
                        decoded.extend(dec.flush());
                        for item in decoded {
                            let crate::keys::DecodedInput::Key(dk) = item else { continue };
                            if let Some(outcome) = self.dispatch_choose_tree_key(&dk.key, &mut client, &mut session_name) {
                                dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                                if detach || destroy {
                                    break 'events;
                                }
                            }
                        }
                    } else if matches!(client.mode, ClientMode::DisplayPanes(_)) {
                        // display-panes (Task 8): only the FIRST decoded key
                        // of a coalesced blob matters (digit selects, any
                        // other key dismisses) — the rest of the blob is
                        // discarded, per the design spec's documented
                        // "not reprocessed" simplification.
                        let mut dec = crate::keys::KeyDecoder::new();
                        let decoded = dec.feed(&data);
                        if let Some(crate::keys::DecodedInput::Key(dk)) = decoded.into_iter().next() {
                            let outcome = self.dispatch_display_panes_key(&dk.key, &mut client, &session_name);
                            dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                            if detach || destroy {
                                break 'events;
                            }
                        }
                    } else if matches!(client.mode, ClientMode::Clock(_)) {
                        // clock-mode (Task 10): "any key exits"
                        // (`window-clock.c:213-219`) with no digit/non-digit
                        // split to make -- unlike display-panes there is
                        // nothing to decode at all, the whole blob is simply
                        // consumed and the mode closes.
                        let outcome = self.dispatch_clock_key(&mut client);
                        dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                        if detach || destroy {
                            break 'events;
                        }
                    } else if let Some(session) = self.registry.session_mut(&session_name) {
                        let fid = session.current_window().layout.focused();
                        if let Some(pane) = self.panes.get(&fid) {
                            // Follow-up #14: enqueue onto the pane's OWN
                            // writer thread instead of calling
                            // `pty.write_input` inline here — this is the
                            // server's single main-loop thread, which must
                            // never block on one pane's (possibly stalled)
                            // stdin.
                            let _ = pane.input_tx.send(data);
                        }
                    }
                }
                KeyInputEvent::Key { table, key, raw } => {
                    // choose-tree/display-panes (Task 8; review fix,
                    // Important #2): EVERY Key event is intercepted while
                    // one of these overlays is open, regardless of `table`
                    // -- INCLUDING a `Prefix`-table event, i.e. the
                    // completion of a prefix sequence typed WHILE THE
                    // OVERLAY WAS ALREADY OPEN (`KeyMachine` swallows the
                    // bare prefix keypress itself with no event at all --
                    // only the key that completes the sequence surfaces,
                    // tagged `Prefix` -- so this is the only seam available
                    // to catch it). Before this fix the interception was
                    // gated on `table == WhichTable::Root` only, so a
                    // completed prefix sequence fell through to ordinary
                    // prefix-binding dispatch below and ran an unrelated
                    // bound command UNDER the open overlay. This
                    // deliberately does NOT mirror copy mode's own
                    // "`Prefix`-table events pass through untouched" rule
                    // (`C-b c` still works from copy mode, a long-lived
                    // mode where that's desirable) -- choose-tree and
                    // display-panes are momentary, modal overlays, and the
                    // design spec's display-panes rule is explicit ("other
                    // key dismisses ... and is NOT reprocessed").
                    //
                    // choose-tree: `dispatch_choose_tree_key` already
                    // treats a key outside its hardcoded table as swallowed
                    // (`resolve_choose_tree_key` returns `None` for
                    // anything but Up/Down/`k`/`j`/Enter/`q`/Escape/`C-c`/
                    // `x`) -- so routing a `Prefix`-table event through it
                    // unconditionally reproduces tmux's own modal choose
                    // mode: a completed prefix sequence is either
                    // reinterpreted as a choose-tree action or silently
                    // ignored (overlay stays open either way), never as the
                    // prefix-bound command.
                    if matches!(client.mode, ClientMode::ChooseTree(_)) {
                        if let Some(outcome) = self.dispatch_choose_tree_key(&key, &mut client, &mut session_name) {
                            dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                            if detach || destroy {
                                break 'events;
                            }
                        }
                        continue;
                    }
                    // display-panes: per the design spec's "other key
                    // dismisses (and is NOT reprocessed)" rule -- a
                    // completed prefix sequence's second key dismisses the
                    // overlay exactly like any other non-digit key, and the
                    // prefix-bound command is never dispatched underneath
                    // it.
                    if matches!(client.mode, ClientMode::DisplayPanes(_)) {
                        let outcome = self.dispatch_display_panes_key(&key, &mut client, &session_name);
                        dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                        if detach || destroy {
                            break 'events;
                        }
                        continue;
                    }
                    // clock-mode (Task 10): same unconditional interception
                    // as display-panes above -- "any key exits" per
                    // `window-clock.c`'s own key handler, which has no key-
                    // table lookup at all, so a completed prefix sequence
                    // dismisses the overlay too, never running the bound
                    // command underneath it.
                    if matches!(client.mode, ClientMode::Clock(_)) {
                        let outcome = self.dispatch_clock_key(&mut client);
                        dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                        if detach || destroy {
                            break 'events;
                        }
                        continue;
                    }
                    // Copy mode (Task 2): a `Root`-table Key event while the
                    // acting client is in `ClientMode::Copy` is looked up
                    // against the copy table `mode-keys` selects instead —
                    // `KeyMachine` knows nothing of client modes, so this
                    // substitution is the server's job (see the
                    // `## copy-mode` contract section). A `Prefix`-table
                    // event is left alone: prefix bindings (e.g. `C-b c`)
                    // still fire from copy mode, matching tmux.
                    let table = if matches!(client.mode, ClientMode::Copy(_)) && table == WhichTable::Root {
                        if self.options.mode_keys_vi() {
                            WhichTable::CopyModeVi
                        } else {
                            WhichTable::CopyMode
                        }
                    } else {
                        table
                    };
                    let binding = self.bindings.lookup(table, &key).cloned();
                    match binding {
                        Some(b) => {
                            let outcome = self.dispatch_client(&b.cmds, &mut client, &mut session_name);
                            if b.repeat {
                                client.key_machine.arm_repeat(now);
                            }
                            dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                            if detach || destroy {
                                break 'events;
                            }
                        }
                        None => match table {
                            // Unbound in the root table: forward raw bytes
                            // (tmux behavior for `bind -n`-less keys).
                            WhichTable::Root => {
                                if let Some(session) = self.registry.session_mut(&session_name) {
                                    let fid = session.current_window().layout.focused();
                                    if let Some(pane) = self.panes.get(&fid) {
                                        // Follow-up #14: see the Forward-blob
                                        // arm above — same per-pane writer
                                        // channel, not an inline blocking
                                        // `pty.write_input`.
                                        let _ = pane.input_tx.send(raw);
                                    }
                                }
                            }
                            // Unbound in the prefix table: swallowed (tmux).
                            WhichTable::Prefix => {}
                            // Unbound in a copy table: swallowed (per the
                            // design spec — copy mode never leaks stray
                            // keystrokes to the pane underneath).
                            WhichTable::CopyMode | WhichTable::CopyModeVi => {}
                        },
                    }
                }
                KeyInputEvent::Captured(chunk) => {
                    let mut i = 0;
                    while i < chunk.len() {
                        let (ended, outcome) = self.feed_mode_byte(&mut client, &mut session_name, chunk[i]);
                        i += 1;
                        if let Some(outcome) = outcome {
                            dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                        }
                        if detach || destroy {
                            break 'events;
                        }
                        if ended {
                            // Commit/cancel mid-chunk: the rest of the chunk
                            // is normal input again (capture is off) — run it
                            // through the KeyMachine and process its events
                            // next, ahead of anything already queued.
                            if i < chunk.len() {
                                let more = client.key_machine.feed(&chunk[i..], now);
                                for e in more.into_iter().rev() {
                                    queue.push_front(e);
                                }
                            }
                            break;
                        }
                    }
                }
                KeyInputEvent::Mouse { event, .. } => {
                    // Task 5 (mouse): routed entirely outside the prefix/
                    // binding-table machinery — see `dispatch::dispatch_mouse`
                    // and the design spec's `## 4. Mouse` section.
                    let outcome = self.dispatch_mouse(event, &mut client, &session_name);
                    dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                    if detach || destroy {
                        break 'events;
                    }
                }
            }
        }

        if detach {
            send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {session_name})]") });
            self.recompute_session_size(&session_name);
            self.apply_layout_for_session(&session_name);
            return; // client dropped, not reinserted
        }
        if destroy {
            // `destroy_session` (and messaging every OTHER attached client)
            // has ALREADY run inside the dispatcher (`kill_pane_by_id`/
            // `kill_window_by_id`) — this client, removed from `self.clients`
            // at the top of this function, is the only one destroy_session
            // couldn't reach.
            send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: "[exited]".to_string() });
            return; // client dropped, not reinserted
        }
        self.clients.insert(id, client);
        // `(`/`)` switch-client: recompute both sessions' shared sizes/
        // layouts now that the client is back in `self.clients` (so it's
        // correctly counted toward the NEW session and no longer toward the
        // old one).
        if let Some((old, new)) = session_switched {
            self.recompute_session_size(&old);
            self.apply_layout_for_session(&old);
            self.recompute_session_size(&new);
            self.apply_layout_for_session(&new);
        }
        // This client may have just removed a pane/window that ANOTHER
        // client (same session) had a pending kill confirm armed on.
        self.cancel_stale_confirms();
        // Same idea for copy mode (Task 2): this client's own dispatch (or
        // another client's) may have changed a window's live panes, or this
        // client's own current window (a `[` copy-mode-then-prefix-`c`
        // sequence, etc.) — re-check every client's copy mode after every
        // Stdin-driven dispatch batch.
        self.cancel_stale_copy_modes();
        // Same idea for clock mode (Task 10): identical staleness rule to
        // copy mode above.
        self.cancel_stale_clock_modes();
        // Same idea for choose-tree's `pending_kill` (Task 8): a `y` from
        // this OR another client may have just removed the session/window
        // this client had a kill confirm armed on.
        self.cancel_stale_choose_trees();
    }

    /// Precompute per-client overlay render data (Task 8) for every
    /// currently attached client, BEFORE `render_all`'s mutable
    /// `self.clients.values_mut()` loop begins — see [`RenderOverlay`]'s doc
    /// comment for why this can't happen inside `render_one` itself.
    fn build_render_overlay(&self, client: &ClientState) -> Option<RenderOverlay> {
        let session_name = client.session.as_deref()?;
        match &client.mode {
            ClientMode::ChooseTree(state) => {
                let rows = self.build_tree_rows(session_name, state.view, &state.expanded);
                // Task 8 review fix, Critical #1: resolve by IDENTITY, not
                // a raw clamped index -- see `resolve_tree_sel`'s doc
                // comment. `build_render_overlay` runs with `&self` only
                // (before `render_all`'s mutable per-client loop), so this
                // is a read-only re-derivation, same as every other render
                // pass; the persisted `ChooseTreeState` is not mutated here.
                let sel = resolve_tree_sel(&rows, &state.selected, state.sel);
                // SP6 wave 2, Task 8: sizing (`## 3.1`) + the selected row's
                // live preview content, built here (not `render_one`) for the
                // same reason the rows themselves are -- this is the one pass
                // with `&self` (all panes' `Grid`s, the whole registry) in
                // hand before `render_all`'s mutable per-client loop begins.
                let sy = client.rows;
                let w = client.cols;
                let list_height = Server::choose_tree_list_height(sy, w, rows.len(), state.preview);
                let preview = if list_height < sy {
                    // Fix round 1 (`## 3.2`): the interior is inset inside
                    // the full 4-sided box -- 2 cells horizontal each side
                    // (`w - 4`), 1 row vertical each (top + bottom border:
                    // `(sy - list_height) - 2`). `choose_tree_list_height`'s
                    // box-size guard (`sy-h <= 4 || w <= 4` drops the
                    // preview) guarantees both are >= 1 whenever this branch
                    // is reached (region >= 5 rows -> interior >= 3 rows;
                    // panel >= 5 cols -> interior >= 1 col), so no extra
                    // "inset shrank the blit area below 1x1" drop rule is
                    // needed here.
                    let interior_h = sy.saturating_sub(list_height).saturating_sub(2);
                    let interior_w = w.saturating_sub(4);
                    rows.get(sel).map(|r| {
                        let (title, content_w, content_h, content) = self.build_tree_preview(&r.target, interior_w, interior_h);
                        TreePreviewData { title, content_w, content_h, content }
                    })
                } else {
                    None
                };
                Some(RenderOverlay::Tree {
                    rows: rows.into_iter().map(|r| (r.text, r.depth, r.marker)).collect(),
                    sel,
                    list_height,
                    preview,
                })
            }
            ClientMode::DisplayPanes(_) => {
                let session = self.registry.sessions().iter().find(|s| s.name == session_name)?;
                Some(RenderOverlay::Digits(pane_digit_entries(session.current_window())))
            }
            ClientMode::Clock(state) => Some(RenderOverlay::Clock { pane: state.pane, text: state.text.clone() }),
            _ => None,
        }
    }

    /// Render every attached client (see module docs: render-all, not
    /// per-session dirty tracking).
    fn render_all(&mut self) {
        let area_y = self.pane_area_y();
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        let mut overlays: HashMap<ClientId, RenderOverlay> = HashMap::new();
        for id in ids {
            if let Some(client) = self.clients.get(&id) {
                if let Some(ov) = self.build_render_overlay(client) {
                    overlays.insert(id, ov);
                }
            }
        }
        for (id, client) in self.clients.iter_mut() {
            let Some(name) = client.session.clone() else { continue };
            let Some(session) = self.registry.sessions().iter().find(|s| s.name == name) else { continue };
            render_one(client, session, &self.panes, &self.options, &self.hostname, area_y, overlays.get(id));
        }
    }
}

/// Compose and send one client's frame from shared session state. Styles,
/// status position/visibility, and the status-left/right strings all come
/// from the option table (Task 8): the defaults reproduce the SP2 output
/// byte for byte (`status-left "[#S] "` expands to the old `[<name>] `
/// prefix; `status-right "%H:%M %d-%b-%y"` expands to the old
/// `local_clock()` string). `area_y` is `Server::pane_area_y()` — the same
/// origin `apply_layout_for_session` used, so the drawn rects line up with
/// the pty/grid sizes.
fn render_one(
    client: &mut ClientState,
    session: &Session,
    panes: &HashMap<PaneId, PaneRuntime>,
    options: &Options,
    hostname: &str,
    area_y: u16,
    overlay_data: Option<&RenderOverlay>,
) {
    let window = session.current_window();
    let area = Rect { x: 0, y: area_y, w: session.size.0, h: session.size.1 };
    let focused = window.layout.focused();
    let zoomed = window.layout.is_zoomed();
    let rects = window.layout.rects(area);

    let too_small =
        area.w < MIN_PANE_W || area.h < MIN_PANE_H || rects.iter().any(|(_, r)| r.w < MIN_PANE_W || r.h < MIN_PANE_H);

    // Precedence matches the pre-Task-6 code exactly: `ConfirmCmd` (the
    // Task 6 generalization of the legacy `ConfirmKillPane`/`ConfirmKillWindow`
    // variants) had no match guard, so it (and `Prompt`) always wins
    // regardless of `too_small` — a confirm/prompt overlay doesn't depend on
    // pane space, so it stays visible even on a too-small terminal.
    // `too_small` only applies in `Normal` mode, where it additionally takes
    // priority over a transient status message.
    let default_style = Style::default();
    let msg_style = options.message_style().apply_to(default_style);
    let message = match &client.mode {
        ClientMode::ConfirmCmd { prompt, .. } => Some(prompt.clone()),
        ClientMode::Prompt { label, buf, .. } => Some(format!("{label}{buf}")),
        ClientMode::Normal if too_small => Some("terminal too small".to_string()),
        ClientMode::Copy(_) if too_small => Some("terminal too small".to_string()),
        ClientMode::Normal => client.message.as_ref().map(|(msg, _)| msg.clone()),
        // Copy mode usually has no message of its own (the position
        // indicator is painted directly on the pane, not the status row),
        // but a transient message (e.g. "no match: <pattern>") can still be
        // showing underneath it -- UNLESS a search prompt (Task 4) is
        // currently open, which takes over the status row exactly like
        // `ClientMode::Prompt` does (`"Search Down: "`/`"Search Up: "` +
        // the in-progress buffer), matching tmux's own prompt label text.
        ClientMode::Copy(cs) => match &cs.search_prompt {
            Some(sp) => {
                let label = if sp.backward { "Search Up: " } else { "Search Down: " };
                Some(format!("{label}{}", sp.buf))
            }
            None => client.message.as_ref().map(|(msg, _)| msg.clone()),
        },
        // choose-tree/display-panes (Task 8): `too_small` still wins first
        // (an overlay painted over a degenerate-size terminal is nonsensical
        // -- the `too_small` branch below returns early with no overlay at
        // all). Otherwise the pending kill-confirm prompt (precomputed at
        // `x`-press time, see `ChooseTreeState::pending_kill`'s doc comment)
        // takes priority over any ordinary transient message; display-panes
        // never has a message of its own beyond the digits themselves.
        ClientMode::ChooseTree(_) if too_small => Some("terminal too small".to_string()),
        ClientMode::DisplayPanes(_) if too_small => Some("terminal too small".to_string()),
        ClientMode::Clock(_) if too_small => Some("terminal too small".to_string()),
        ClientMode::ChooseTree(state) => match &state.pending_kill {
            Some((_, prompt)) => Some(prompt.clone()),
            None => client.message.as_ref().map(|(msg, _)| msg.clone()),
        },
        ClientMode::DisplayPanes(_) => client.message.as_ref().map(|(msg, _)| msg.clone()),
        // clock-mode (Task 10): no message of its own -- the time is drawn
        // directly on the pane, same as display-panes' digits.
        ClientMode::Clock(_) => client.message.as_ref().map(|(msg, _)| msg.clone()),
    }
    .map(|m| (m, msg_style));

    // Status row from the option table. status off -> None (no row painted;
    // the pane area already includes the freed row via
    // `recompute_session_size`).
    let status = if options.status_on() {
        let base = options.status_style().apply_to(default_style);
        // Format context from live state: the current window's index/name/
        // flags and the focused pane's position in `layout.panes()`.
        let mut window_flags = String::from("*");
        if window.layout.is_zoomed() {
            window_flags.push('Z');
        }
        let pane_index = window.layout.panes().iter().position(|p| *p == focused).unwrap_or(0) as u32;
        let pane_title = panes.get(&focused).map(|p| p.title.as_str()).unwrap_or("");
        let fctx = FormatCtx {
            session: &session.name,
            window_index: window.index,
            window_name: &window.name,
            window_flags: &window_flags,
            pane_index,
            hostname,
            now: system_time_parts(),
            pane_title,
        };
        // Option-length caps apply while building the strings (tmux
        // truncates left/right to status-left/right-length); the renderer's
        // spatial right-first truncation still applies on top when the
        // capped strings don't fit the terminal width. status-right is run
        // through `strip_style_markers` BEFORE the length cap: `render::
        // StatusRow::right` has only one style slot (no room for inline
        // `#[...]`-styled sub-runs the way the left/window-list spans have),
        // so any markers are dropped to plain text rather than leaking their
        // literal `#[...]` bytes onto the screen (SP6 Task 4).
        let left = truncate_chars(&expand_format(options.status_left(), &fctx), options.status_left_length());
        let right_expanded = strip_style_markers(&expand_format(options.status_right(), &fctx));
        let right = truncate_chars(&right_expanded, options.status_right_length());
        let entries: Vec<WindowEntry> = session
            .windows
            .iter()
            .map(|w| WindowEntry {
                index: w.index,
                name: w.name.clone(),
                current: w.id == session.current,
                last: Some(w.id) == session.last,
                zoomed: w.layout.is_zoomed(),
            })
            .collect();
        let spans = status_spans(
            &left,
            options.status_left_style(),
            &entries,
            &fctx,
            options.window_status_format(),
            options.window_status_current_format(),
            base,
            options.window_status_style(),
            options.window_status_current_style(),
            options.window_status_separator(),
            options.status_justify(),
            session.size.0,
            right.chars().count(),
        );
        Some(StatusRow {
            top: options.status_position_top(),
            base,
            spans,
            right,
            // status-right-style layered over base (SP6 Task 4; previously
            // always bare `base` until this task wired the option in).
            right_style: options.status_right_style().apply_to(base),
        })
    } else {
        None
    };

    let border = options.pane_border_style().apply_to(default_style);
    let border_active = options.pane_active_border_style().apply_to(default_style);
    let border_indicators = options.pane_border_indicators();
    let mode_style = options.mode_style().apply_to(default_style);
    let display_panes_colour = Style { bg: options.display_panes_colour(), ..default_style };
    let display_panes_active_colour = Style { bg: options.display_panes_active_colour(), ..default_style };
    let scene_size = (client.cols, client.rows);

    if too_small {
        let scene = Scene {
            size: scene_size,
            panes: Vec::new(),
            zoomed,
            status,
            message,
            border,
            border_active,
            border_indicators,
            mode_style,
            display_panes_colour,
            display_panes_active_colour,
            overlay: None,
        };
        let out = client.renderer.compose(&scene, None, false);
        send_output(&client.tx, out);
        return;
    }

    let mut views = Vec::with_capacity(rects.len());
    for (id, rect) in &rects {
        if let Some(p) = panes.get(id) {
            // Copy mode (Task 2): the pane bound to THIS client's
            // `ClientMode::Copy` (if any) renders its scrolled view; every
            // other pane (including one another client has zoomed/focused)
            // renders live, unaffected by this client's copy mode.
            let copy = match &client.mode {
                ClientMode::Copy(cs) if cs.pane == *id => {
                    let sel = cs.sel.as_ref().and_then(|sel| {
                        compute_sel_view(
                            sel,
                            cs.cx,
                            cs.cy,
                            cs.scroll,
                            rect.h,
                            rect.w,
                            p.grid.history_len(),
                            p.grid.history_total(),
                        )
                    });
                    Some(CopyView { scroll: cs.scroll, sel })
                }
                _ => None,
            };
            views.push(PaneView { id: *id, rect: *rect, grid: &p.grid, focused: *id == focused, dead: p.dead, copy });
        }
    }

    let (cursor, cursor_visible) = match &client.mode {
        ClientMode::Copy(cs) => match rects.iter().find(|(id, _)| *id == cs.pane).map(|(_, r)| *r) {
            Some(r) => {
                let cx = cs.cx.min(r.w.saturating_sub(1));
                let cy = cs.cy.min(r.h.saturating_sub(1));
                (Some((r.x + cx, r.y + cy)), message.is_none())
            }
            None => (None, false),
        },
        // choose-tree/display-panes (Task 8): both cover the pane area (a
        // full-screen panel, or per-pane digit blocks) — the real terminal
        // cursor has nothing sensible to sit on, so it's simply hidden
        // (same end effect `message.is_none()` gating already gives every
        // OTHER overlay/message case above).
        // clock-mode (Task 10): "Cursor is hidden" (`s->mode &= ~MODE_CURSOR`
        // in `window-clock.c`) -- same treatment as the other two overlays.
        ClientMode::ChooseTree(_) | ClientMode::DisplayPanes(_) | ClientMode::Clock(_) => (None, false),
        _ => match (rects.iter().find(|(id, _)| *id == focused).map(|(_, r)| *r), panes.get(&focused)) {
            (Some(r), Some(p)) => {
                let (cx, cy) = p.grid.cursor();
                let visible = p.grid.cursor_visible() && !p.dead && message.is_none();
                (Some((r.x + cx, r.y + cy)), visible)
            }
            _ => (None, false),
        },
    };

    let overlay = overlay_data.map(|ov| match ov {
        RenderOverlay::Tree { rows, sel, list_height, preview } => {
            // Task 8 review fix, Important #3: `compose_back`'s actual paint
            // pass reserves the panel's OWN last row for `scene.message`
            // (choose-tree's `x` kill-confirm prompt) whenever it's `Some`
            // AND there's no preview showing -- `msg_reserved`/`visible`
            // there, mirrored exactly here so `top`'s "keep `sel` on screen"
            // math never assumes one more paintable row than `compose_back`
            // will actually use. Without this, a scrolled selection at the
            // bottom of a long list with a just-armed kill-confirm showing
            // could be computed as "visible" here while `compose_back`
            // paints one row less, pushing the selected/prompted row
            // off-screen. SP6 wave 2, Task 8: the list's paintable height is
            // now `*list_height` (the preview-sizing rule, `## 3.1`) instead
            // of the full scene height whenever a preview is showing --
            // `*list_height == scene_size.1` in every other case, so this
            // subsumes the pre-Task-8-wave-2 math exactly.
            let msg_reserved = if message.is_some() && preview.is_none() { 1 } else { 0 };
            let visible = (*list_height as usize).saturating_sub(msg_reserved);
            let top = sel.saturating_sub(visible.saturating_sub(1));
            let preview_block = preview.as_ref().map(|p| PreviewBlock {
                rect: Rect { x: 0, y: *list_height, w: scene_size.0, h: scene_size.1.saturating_sub(*list_height) },
                title: p.title.clone(),
                content_w: p.content_w,
                content_h: p.content_h,
                content: p.content.clone(),
            });
            Overlay::List(ListOverlay {
                title: String::new(),
                rows: rows
                    .iter()
                    .enumerate()
                    .map(|(i, (text, depth, marker))| TreeRowCell { text: text.clone(), depth: *depth, marker: *marker, selected: i == *sel })
                    .collect(),
                top,
                preview: preview_block,
            })
        }
        RenderOverlay::Digits(entries) => {
            let mut v = Vec::with_capacity(entries.len());
            for (pane_id, digit) in entries {
                if let Some((_, rect)) = rects.iter().find(|(id, _)| id == pane_id) {
                    v.push((*rect, *digit, *pane_id == focused));
                }
            }
            Overlay::PaneDigits(v)
        }
        RenderOverlay::Clock { pane, text } => {
            // Falls back to a zero-size rect if the bound pane's rect can't
            // be found (unreachable in practice -- `cancel_stale_clock_modes`
            // already guarantees the pane is live and in the current window
            // by the time a render happens -- but zero-size rects are always
            // tolerated per the project's own "every consumer must tolerate
            // w==0/h==0" rule, so `paint_clock` simply no-ops on it).
            let rect = rects.iter().find(|(id, _)| id == pane).map(|(_, r)| *r).unwrap_or(Rect { x: 0, y: 0, w: 0, h: 0 });
            Overlay::Clock(rect, text.clone(), options.clock_mode_colour())
        }
    });

    let scene = Scene {
        size: scene_size,
        panes: views,
        zoomed,
        status,
        message,
        border,
        border_active,
        border_indicators,
        mode_style,
        display_panes_colour,
        display_panes_active_colour,
        overlay,
    };
    let out = client.renderer.compose(&scene, cursor, cursor_visible);
    send_output(&client.tx, out);
}

/// Run the multiplexer server: bind `pipe_full_name`, load startup config,
/// accept clients, and loop until every session has died (exit-empty). Does
/// not touch the console and installs no panic hook (both are `main.rs`'s
/// job, Task 8). `config_files` is the server role's `--config <path>` args
/// (repeatable, in order; forwarded from the CLI's `-f`, Task 7) — empty
/// means "use the default `.tmux.conf`/`.winmux.conf` discovery chain", a
/// non-empty slice REPLACES that chain entirely (see
/// `discover_config_files`).
pub fn run(pipe_full_name: &str, config_files: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let listener = PipeListener::bind(pipe_full_name)?;
    let (tx, rx) = channel::<ServerEvent>();

    {
        let accept_tx = tx.clone();
        thread::spawn(move || {
            while let Ok(conn) = listener.accept() {
                if accept_tx.send(ServerEvent::Connected(conn)).is_err() {
                    break;
                }
            }
        });
    }

    let mut server = Server::new(tx);

    // Startup config loading (Task 7): after the pipe is bound (so a client
    // racing to connect never sees "not found"), before any client attach is
    // served (the loop below). Errors don't stop the server from coming up
    // (tmux behavior) — they're logged AND surfaced to the first attach.
    {
        let xdg = std::env::var("XDG_CONFIG_HOME").ok();
        let userprofile = std::env::var("USERPROFILE").ok();
        let candidates = discover_config_files(xdg.as_deref(), userprofile.as_deref(), config_files);
        let errors = server.load_config_files(&candidates);
        if !errors.is_empty() {
            crate::logging::log_line(&format!("config: {} error(s)", errors.len()));
            for e in &errors {
                crate::logging::log_line(&format!("  {e}"));
            }
            server.pending_config_message = Some(format!("config: {} error(s), see server.log", errors.len()));
        }
    }

    loop {
        let first = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => ServerEvent::Tick,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut dirty = server.handle_event(first);
        while let Ok(ev) = rx.try_recv() {
            dirty |= server.handle_event(ev);
        }

        if dirty {
            server.render_all();
        }

        if server.had_session && server.registry.is_empty() {
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod config_discovery_tests {
    use super::discover_config_files;
    use std::path::PathBuf;

    #[test]
    fn explicit_replaces_default_chain() {
        let explicit = vec!["a.conf".to_string(), "b.conf".to_string()];
        let got = discover_config_files(Some("ignored"), Some(r"C:\Users\x"), &explicit);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, PathBuf::from("a.conf"));
        assert!(got[0].required);
        assert_eq!(got[1].path, PathBuf::from("b.conf"));
        assert!(got[1].required);
    }

    #[test]
    fn xdg_wins_over_userprofile_tmux_conf() {
        let got = discover_config_files(Some(r"C:\xdg"), Some(r"C:\Users\x"), &[]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, PathBuf::from(r"C:\xdg").join("tmux").join("tmux.conf"));
        assert!(!got[0].required);
        assert_eq!(got[1].path, PathBuf::from(r"C:\Users\x").join(".winmux.conf"));
        assert!(!got[1].required);
    }

    #[test]
    fn empty_xdg_falls_back_to_userprofile_tmux_conf() {
        let got = discover_config_files(Some(""), Some(r"C:\Users\x"), &[]);
        assert_eq!(got[0].path, PathBuf::from(r"C:\Users\x").join(".tmux.conf"));
        assert!(!got[0].required);
    }

    #[test]
    fn no_xdg_falls_back_to_userprofile_tmux_conf() {
        let got = discover_config_files(None, Some(r"C:\Users\x"), &[]);
        assert_eq!(got[0].path, PathBuf::from(r"C:\Users\x").join(".tmux.conf"));
        assert_eq!(got[1].path, PathBuf::from(r"C:\Users\x").join(".winmux.conf"));
    }

    #[test]
    fn no_userprofile_no_xdg_yields_no_candidates() {
        let got = discover_config_files(None, None, &[]);
        assert!(got.is_empty());
    }

    /// Task 7 review fix (Important): `--config -` (the tmux `-f /dev/null`
    /// idiom) disables config loading entirely — no default chain, no
    /// candidates, no errors. `-` entries are dropped but still count as
    /// "explicit was given" (the default chain stays replaced), so
    /// `--config - --config real.conf` loads only `real.conf`.
    #[test]
    fn dash_config_disables_defaults() {
        let got = discover_config_files(Some(r"C:\xdg"), Some(r"C:\Users\x"), &["-".to_string()]);
        assert!(got.is_empty());

        let got = discover_config_files(None, Some(r"C:\Users\x"), &["-".to_string(), "real.conf".to_string()]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, PathBuf::from("real.conf"));
        assert!(got[0].required);
    }
}

/// Task 10 (clock-mode): the pure formatting seam `format_clock` -- unit
/// tested directly (no wall-clock, no server/client plumbing) against the
/// exact strings pinned by `docs/tmux-reference/status-line-and-
/// messages.md` `## 6. Clock mode`.
#[cfg(test)]
mod format_clock_tests {
    use super::format_clock;

    #[test]
    fn style_24_zero_pads_both_fields() {
        assert_eq!(format_clock(9, 5, false), "09:05");
        assert_eq!(format_clock(0, 0, false), "00:00");
        assert_eq!(format_clock(23, 59, false), "23:59");
    }

    /// `%l:%M ` + `AM`/`PM`: hour is SPACE-padded (not zero-padded, tmux's
    /// `%l`), minute IS zero-padded, and AM/PM is appended directly after
    /// the trailing space baked into the `%l:%M ` format (no extra space).
    #[test]
    fn style_12_space_pads_hour_and_appends_am_pm() {
        assert_eq!(format_clock(0, 5, true), "12:05 AM"); // midnight -> 12 AM
        assert_eq!(format_clock(9, 5, true), " 9:05 AM"); // single digit -> space-padded
        assert_eq!(format_clock(12, 0, true), "12:00 PM"); // noon -> 12 PM
        assert_eq!(format_clock(13, 34, true), " 1:34 PM");
        assert_eq!(format_clock(23, 59, true), "11:59 PM");
    }
}
