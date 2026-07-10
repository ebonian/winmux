//! Session/window registry (pure bookkeeping, no I/O).
//!
//! Sits above `layout::Layout`: a `Window` owns one `Layout` (its split
//! tree); a `Session` owns an ordered list of `Window`s plus current/last
//! tracking; a `Registry` owns all `Session`s in creation order. Implements
//! tmux's naming/targeting/window-cycling semantics exactly (see
//! docs/specs/2026-07-07-server-client-design.md "Data model" and the
//! `## model` section of the sibling interfaces contract).

use crate::layout::{Layout, PaneId};
use crate::options;
use std::time::{Instant, SystemTime};

/// Server-global, monotonically increasing window id — NOT the tmux window
/// index (`Window::index`), which is per-session and reused after gaps.
pub type WindowId = u32;

pub struct Window {
    pub id: WindowId,
    /// tmux window index (lowest unused >= 0 at creation).
    pub index: u32,
    /// Default "powershell"; renamed via tmux prefix `,` (future task).
    ///
    /// **Invariant (follow-up #62):** every write to this field MUST go
    /// through [`validate_name`] first (directly, or via a setter that
    /// already calls it). There is no type-level enforcement — a plain
    /// `String` accepts anything — the invariant holds today only because
    /// every call site that sets it (`exec_rename_window`,
    /// `derive_auto_name`'s caller, `Registry::create_session`/
    /// `Session::new_window`'s hardcoded initial `"powershell"`, which is
    /// itself a known-valid literal) happens to be gated by it. Choose-tree
    /// row rendering and the status bar interpolate this value into rendered
    /// VT output with no further escaping, trusting that transitively. A
    /// future direct-assignment call site that skips validation would
    /// silently reintroduce the terminal-corruption/control-character risk
    /// `validate_name` exists to close (see that function's own doc
    /// comment) — this note exists so the risk is documented at the FIELD,
    /// not only at today's call sites.
    pub name: String,
    pub layout: Layout,
    /// `next-layout`'s cycle position (Task 6, sub-project 4): the
    /// `layout::PRESET_CYCLE` index of the last preset APPLIED via
    /// `select-layout`/`next-layout` (`None` until the first one ever
    /// applied, or if the window's layout is currently a manual/custom tree
    /// -- manual splits/resizes never touch this field, so `next-layout`
    /// still resumes from wherever the cycle last landed, matching tmux).
    pub last_layout: Option<u8>,
    /// `automatic-rename` (Task 9, sub-project 4): `true` (tmux default)
    /// while this window's name should keep tracking its ACTIVE pane's OSC
    /// title. Any MANUAL naming — `rename-window`/the `,` prompt commit, or
    /// an explicit `-n`/name given to `new-window`/`break-pane` — sets this
    /// `false` PERMANENTLY for this window (tmux precedence: a later global
    /// `set -g automatic-rename on` resumes auto-renaming for windows that
    /// are still eligible, but never reactivates a window whose name was
    /// set explicitly — real tmux has a genuine per-window
    /// `set-window-option automatic-rename on` escape hatch for that; out of
    /// scope here since winmux has no per-window option overlays, see
    /// `docs/specs/2026-07-07-parity-polish-interfaces.md`'s `## naming`
    /// section). Whether a rename actually FIRES also requires the global
    /// `automatic-rename` option to be on (`server::Server::maybe_auto_rename`
    /// ANDs both).
    pub auto_rename: bool,
    /// Throttle bookkeeping for automatic-rename (tmux `NAME_INTERVAL`,
    /// 500ms): the last time this window's name was actually changed by the
    /// auto-rename path, so a chatty pane can't rename it more than once per
    /// throttle window. `None` until the first automatic rename.
    pub last_auto_rename: Option<Instant>,
    /// SP7 Task 6 (closes follow-up #26): this window's local overrides for
    /// window-scoped options (`setw`/`set -w`, no `-g`) -- see
    /// `options::Overlay`'s doc comment for why it lives HERE rather than
    /// in a keyed map inside `Options`. Starts empty (inherits the global
    /// table for everything), so a window that never runs `setw` behaves
    /// byte-identically to every pre-Task-6 window.
    pub window_options: options::Overlay,
    /// Alerts subsystem (SP7 Task 17, closes follow-up #74): tmux's
    /// `WINLINK_BELL`/`WINLINK_ACTIVITY`/`WINLINK_SILENCE` display flags,
    /// collapsed onto the window itself (winmux windows belong to exactly
    /// one session, unlike tmux's `winlink`, which can share a `window`
    /// across sessions -- so there is exactly one "winlink" per window
    /// here). Set by `server.rs`'s pane-output/Tick detection paths (see
    /// [`Window::mark_bell`]/[`Window::mark_activity`]/
    /// [`Window::mark_silence`]); cleared only when this window becomes its
    /// session's current window again (tmux's clear-on-visit,
    /// `winlink_clear_flags` — see [`Window::clear_alerts`] and every
    /// `Session` method that reassigns `current`). Consumed by
    /// `status.rs`'s `flags()` (`#`/`!`/`~` chars, tmux's fixed
    /// `window_printable_flags` order) and `server.rs`'s
    /// `window-status-bell-style`/`-activity-style` layering.
    pub alert_bell: bool,
    pub alert_activity: bool,
    pub alert_silence: bool,
    /// Edge-latch for the activity/silence REACTION (message/BEL/whatever
    /// `*-action` resolves to), separate from `alert_activity`/
    /// `alert_silence` (SP7 final-fix wave, correctness review SHOULD-FIX
    /// #4). `alert_activity`/`alert_silence` are deliberately only ever set
    /// `true` for a NON-current window (tmux doesn't draw the `#`/`~` flag
    /// for the window you're already looking at), which means their own
    /// `if self.alert_activity { return false }` edge-trigger guard never
    /// engages for the CURRENT window — so before this fix, `mark_activity`/
    /// `mark_silence` returned `true` on literally EVERY call for a
    /// focused, idle, monitor-silence-enabled window, causing
    /// `check_silence` to force a wasted render pass and (for `*-action
    /// any`/`current`) repeat the message/BEL reaction every single 50ms
    /// tick, forever. This latch fires once regardless of `is_current`, and
    /// is reset by [`Window::clear_alerts`] (window visited) for activity,
    /// or by [`Window::note_output`] (a fresh silence "episode" can start
    /// once output resumes) for silence — see those methods' doc comments.
    activity_fired: bool,
    silence_fired: bool,
    /// Last time this window had ANY pane output (tmux `window_update_
    /// activity`'s `activity_time`) — the silence-monitor clock
    /// `server.rs`'s Tick handler compares against `monitor-silence`
    /// seconds. Reset on every pane-output event routed to this window via
    /// [`Window::note_output`], regardless of `monitor-activity`/
    /// `monitor-silence` (mirrors tmux: `window_update_activity` runs
    /// unconditionally from the input parser).
    pub last_output: Instant,
}

impl Window {
    /// tmux clear-on-visit (`winlink_clear_flags`, window.c:2085-2096): all
    /// three alert flags reset when this window becomes its session's
    /// current window. Idempotent (a window with no flags set is a no-op).
    /// Also resets the activity reaction latch (SP7 final-fix wave) — see
    /// `activity_fired`'s doc comment; the silence latch is intentionally
    /// NOT reset here, only by [`Window::note_output`].
    pub fn clear_alerts(&mut self) {
        self.alert_bell = false;
        self.alert_activity = false;
        self.alert_silence = false;
        self.activity_fired = false;
    }

    /// Bell detection (tmux `alerts_check_bell`, alerts.c:182-218): sets
    /// the `!` flag when this window ISN'T its session's current window.
    /// UNCONDITIONAL — unlike activity/silence, a bell is "allowed even if
    /// there is an existing bell" (no edge-triggering skip guard); the
    /// caller (`server::Server::note_bell`) still separately evaluates the
    /// notify/visual-bell REACTION on every call, regardless of whether the
    /// flag here actually changed.
    pub fn mark_bell(&mut self, is_current: bool) {
        if !is_current {
            self.alert_bell = true;
        }
    }

    /// Activity detection (tmux `alerts_check_activity`, alerts.c:220-254):
    /// EDGE-TRIGGERED — once fired, every further call is a no-op (`false`)
    /// until the window is visited (`clear_alerts`); the caller only
    /// evaluates the activity-action/visual-activity reaction on a `true`
    /// return (tmux's `if (wl->flags & WINLINK_ACTIVITY) continue;`).
    /// `alert_activity` (the STATUS-BAR `#` flag) is still only ever set
    /// for a non-current window, unchanged — but the edge-trigger guard
    /// itself now runs on `activity_fired`, which fires for the CURRENT
    /// window too (SP7 final-fix wave, correctness review SHOULD-FIX #4:
    /// previously the guard was `self.alert_activity`, which never becomes
    /// true for the current window, so this returned `true` on literally
    /// every call for a focused window with `monitor-activity` on).
    pub fn mark_activity(&mut self, is_current: bool) -> bool {
        if self.activity_fired {
            return false;
        }
        self.activity_fired = true;
        if !is_current {
            self.alert_activity = true;
        }
        true
    }

    /// Silence detection (tmux `alerts_check_silence`, alerts.c:256-290):
    /// same edge-triggered shape as [`Window::mark_activity`] (see that
    /// method's doc comment for the SP7 final-fix wave's `_fired`-latch
    /// split), checked by `server::Server`'s Tick handler once `now -
    /// last_output` has crossed `monitor-silence` seconds. Unlike the
    /// activity latch, the silence latch resets on [`Window::note_output`]
    /// (fresh output), not on visit — a silence "episode" naturally ends
    /// when output resumes, so a LATER quiet period should be able to react
    /// again even if the window was never revisited in between.
    pub fn mark_silence(&mut self, is_current: bool) -> bool {
        if self.silence_fired {
            return false;
        }
        self.silence_fired = true;
        if !is_current {
            self.alert_silence = true;
        }
        true
    }

    /// Record fresh pane output for this window (tmux `window_update_
    /// activity`): refreshes `last_output` (the silence-monitor clock) and
    /// ends any active silence "episode" so a FUTURE quiet period can react
    /// again — see `mark_silence`'s doc comment. Called unconditionally on
    /// every pane-output event routed to this window (`server::Server::
    /// note_activity`), independent of `monitor-activity`/`monitor-silence`.
    pub fn note_output(&mut self) {
        self.last_output = Instant::now();
        self.silence_fired = false;
    }
}

pub struct Session {
    /// **Invariant (follow-up #62):** same rule as [`Window::name`] — every
    /// write MUST go through [`validate_name`] (directly, or via a setter
    /// that already calls it: `Registry::create_session`,
    /// `exec_rename_session`). No type-level guarantee; see `Window::name`'s
    /// doc comment for the full rationale (status bar / choose-tree render
    /// this into VT output untrusted-input-free only because every existing
    /// call site is gated).
    pub name: String,
    pub created: SystemTime,
    /// Kept sorted by `Window::index`.
    pub windows: Vec<Window>,
    pub current: WindowId,
    pub last: Option<WindowId>,
    /// Current window size (smallest attached client).
    pub size: (u16, u16),
    /// tmux `base-index` in effect for THIS session at creation time (Task 7,
    /// SP3 config-loading): the floor `new_window`'s "lowest unused index"
    /// search starts from, so a window killed later never reuses an index
    /// below it. Not `pub` — no consumer outside `model.rs` needs to read it
    /// directly (it's baked into every `Window::index` this session ever
    /// produces); `set -g base-index` only takes effect for sessions created
    /// AFTER the `set`, matching tmux (existing sessions keep their
    /// original numbering).
    base_index: u32,
    /// SP7 Task 6 (closes follow-up #26): this session's local overrides
    /// for session-scoped options (unprefixed `set`, no `-g`) -- see
    /// `options::Overlay`'s doc comment. Starts empty.
    pub session_options: options::Overlay,
}

#[derive(Default)]
pub struct Registry {
    /// Creation order (also display order for `list-sessions`).
    sessions: Vec<Session>,
    next_window_id: WindowId,
}

/// tmux target names reject empty names, the two target separators, AND
/// (final-review fix, 2026-07-07) any control character (C0 incl. `\n`
/// `\r` ESC, plus 0x7f DEL). Control chars are rejected because an
/// unfiltered name reaches the status-bar span text and is written raw to
/// the host terminal (frame corruption) and also breaks line-oriented `ls`
/// output parsing; unlike the interactive rename prompt (which only ever
/// appends printable ASCII 0x20-0x7e), the CLI path (`new-session -s`,
/// `rename-session`/`rename-window` argv) passes raw argv strings straight
/// through, so this check is the only thing standing between a hostile/
/// careless argv and terminal corruption.
///
/// `noun` is `"session"` or `"window"`, shared by both `Registry::
/// create_session` and the server dispatcher's rename paths
/// (`src/server/dispatch.rs`'s `exec_rename_window`/`exec_rename_session`
/// and the interactive rename-prompt commit) — the single hardened rule
/// every call site goes through, so the CLI/command rename paths and the
/// prompt commit path can never diverge.
///
/// The rejected name is echoed back in the error string (`bad session name:
/// <n>`) for operator-friendliness, but echoing the RAW name would
/// re-introduce the same injection into the error message itself (which is
/// also written to a terminal, via `CliDone.err`/status messages) — so the
/// echo is sanitized separately from the validity check: every control
/// char is replaced with `?` before formatting the error, regardless of
/// which rule (empty/separator/control) triggered the rejection.
pub(crate) fn validate_name(name: &str, noun: &str) -> Result<(), String> {
    if name.is_empty() || name.contains(':') || name.contains('.') || name.chars().any(|c| c.is_control()) {
        let sanitized: String = name.chars().map(|c| if c.is_control() { '?' } else { c }).collect();
        return Err(format!("bad {noun} name: {sanitized}"));
    }
    Ok(())
}

/// Lowest index >= `base` not already used by `windows` (tmux `base-index`;
/// `base` is 0 for the pre-Task-7 default).
fn lowest_unused_index(windows: &[Window], base: u32) -> u32 {
    let mut i = base;
    loop {
        if !windows.iter().any(|w| w.index == i) {
            return i;
        }
        i += 1;
    }
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// `name = None` auto-assigns the lowest unused non-negative integer.
    /// The new session gets one window (index `base_index`, default name
    /// "powershell") wrapping `first_pane`; that window becomes current.
    /// `base_index` is tmux's `base-index` option, sampled by the caller at
    /// creation time (Task 7) — every window this session EVER creates
    /// (including later `new_window` calls) numbers from this floor.
    pub fn create_session(
        &mut self,
        name: Option<&str>,
        first_pane: PaneId,
        size: (u16, u16),
        base_index: u32,
    ) -> Result<&mut Session, String> {
        let name = match name {
            Some(n) => {
                validate_name(n, "session")?;
                n.to_string()
            }
            None => self.auto_name(),
        };
        if self.sessions.iter().any(|s| s.name == name) {
            return Err(format!("duplicate session: {name}"));
        }

        let id = self.next_window_id;
        self.next_window_id += 1;
        let window = Window {
            id,
            index: base_index,
            name: "powershell".to_string(),
            layout: Layout::new(first_pane),
            last_layout: None,
            auto_rename: true,
            last_auto_rename: None,
            window_options: options::Overlay::new(),
            alert_bell: false,
            alert_activity: false,
            alert_silence: false,
            activity_fired: false,
            silence_fired: false,
            last_output: Instant::now(),
        };
        let session = Session {
            name,
            created: SystemTime::now(),
            windows: vec![window],
            current: id,
            last: None,
            size,
            base_index,
            session_options: options::Overlay::new(),
        };
        self.sessions.push(session);
        Ok(self.sessions.last_mut().expect("just pushed"))
    }

    /// tmux `-t` target resolution: `=name` matches only exactly; otherwise
    /// an exact match wins outright, then an unambiguous prefix match;
    /// anything else (no match, or an ambiguous prefix) is
    /// `Err("can't find session: <target>")` (using the target with any `=`
    /// sigil already stripped). An EMPTY target is a special case (Task 8
    /// amendment, for `attach-session` with no `-t`, i.e. no "current
    /// client" to fall back on the way the interactive prefix-key bindings
    /// do): it resolves to the most recently created session
    /// (`sessions().last()`), or `Err("no sessions")` if the registry is
    /// empty — never treated as an (always-matching) empty-string prefix.
    pub fn find(&mut self, target: &str) -> Result<&mut Session, String> {
        if target.is_empty() {
            return self.sessions.last_mut().ok_or_else(|| "no sessions".to_string());
        }
        if let Some(name) = target.strip_prefix('=') {
            return self
                .sessions
                .iter()
                .position(|s| s.name == name)
                .map(|i| &mut self.sessions[i])
                .ok_or_else(|| format!("can't find session: {name}"));
        }
        if let Some(i) = self.sessions.iter().position(|s| s.name == target) {
            return Ok(&mut self.sessions[i]);
        }
        let matches: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.name.starts_with(target))
            .map(|(i, _)| i)
            .collect();
        if matches.len() == 1 {
            return Ok(&mut self.sessions[matches[0]]);
        }
        Err(format!("can't find session: {target}"))
    }

    /// Exact-name removal (no prefix matching). `true` if a session was
    /// removed.
    pub fn kill_session(&mut self, name: &str) -> bool {
        match self.sessions.iter().position(|s| s.name == name) {
            Some(i) => {
                self.sessions.remove(i);
                true
            }
            None => false,
        }
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Mutable sibling of [`Registry::sessions`] (SP7 Task 17, closes
    /// follow-up #74): the alerts subsystem's Tick-driven silence check
    /// (`server::Server::check_silence`) needs to walk every session's
    /// every window and mutate per-window alert flags in one pass.
    pub fn sessions_mut(&mut self) -> &mut [Session] {
        &mut self.sessions
    }

    /// Exact-name lookup (no prefix matching, no `=` handling — see `find`
    /// for tmux `-t` target resolution).
    pub fn session_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.name == name)
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Lowest unused non-negative integer, as a string.
    pub fn auto_name(&self) -> String {
        let mut n: u64 = 0;
        loop {
            let candidate = n.to_string();
            if !self.sessions.iter().any(|s| s.name == candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// The session adjacent to `current` in creation order, for `(` / `)`
    /// switch-client; wraps at either end. `None` if `current` isn't found.
    pub fn neighbor_session(&self, current: &str, next: bool) -> Option<&str> {
        let idx = self.sessions.iter().position(|s| s.name == current)?;
        let len = self.sessions.len();
        let new_idx = if next { (idx + 1) % len } else { (idx + len - 1) % len };
        Some(self.sessions[new_idx].name.as_str())
    }

    /// Mint a fresh `WindowId` from the same monotonic counter
    /// `create_session` uses internally for a session's first window. Callers
    /// adding a window to an EXISTING session (`Session::new_window`, e.g.
    /// the `NewWindow` action) must mint the id here first, since
    /// `Session::new_window` takes the id as a plain parameter and does not
    /// mint its own.
    pub fn mint_window_id(&mut self) -> WindowId {
        let id = self.next_window_id;
        self.next_window_id += 1;
        id
    }

    /// Cross-session `move-window -t <session>[:<index>]` (SP7 Task 11,
    /// closes follow-up #45): lift window `id` wholesale out of session
    /// `src_name` and insert it into session `dst_name` at `index`
    /// (explicit) or `dst_name`'s lowest free slot. The `Window` OBJECT
    /// (id, name, layout, `last_layout`, `auto_rename` state, every pane it
    /// contains) moves untouched -- `WindowId`s are global and are never
    /// re-minted (`docs/tmux-reference/windows-and-sessions.md`
    /// §move-window/link-window: "the window keeps a back-list of all its
    /// winlinks... a window is a global object").
    ///
    /// `select`: when `true` (winmux's `move-window` has no `-d`, so
    /// dispatch always passes `true` today), the destination session's
    /// `current` becomes the moved window and `last` becomes whatever WAS
    /// current there, mirroring `new_window`'s own current/last shuffle.
    ///
    /// **Narrowing (documented, follow-up #45's own honest-scope note):**
    /// unlike real tmux (which destroys a source session emptied by the
    /// move, `server-fn.c:304-311`), this refuses outright
    /// (`"can't move the only window out of its session"`) if `id` is
    /// `src_name`'s ONLY window -- the same protective pattern
    /// `Session::kill_window` already applies to a session's last window,
    /// chosen here to avoid this task also having to solve session-teardown
    /// client-eviction semantics for a case that has no floor-level bearing
    /// on the required behavior (moving a window OUT of a multi-window
    /// session, leaving the source alive).
    ///
    /// Errors: `"can't find session: <src_name/dst_name>"` if either
    /// session doesn't exist; `"window not found"` if `id` isn't live in
    /// `src_name`; `"can't move the only window out of its session"` (see
    /// above); `"index in use: <i>"` if an explicit destination index
    /// collides and `kill` is `false` (`kill == true` removes the occupant
    /// first, same `-k` contract `Session::move_window`'s same-session path
    /// already has -- the CALLER is responsible for snapshotting/cleaning up
    /// the killed occupant's pane runtime state, same pattern
    /// `exec_move_window` already follows for the same-session case).
    /// `src_name == dst_name` is rejected too -- callers should route a
    /// same-session move through `Session::move_window` instead, which this
    /// method does not duplicate.
    pub fn move_window_to_session(
        &mut self,
        src_name: &str,
        id: WindowId,
        dst_name: &str,
        index: Option<u32>,
        kill: bool,
        select: bool,
    ) -> Result<(), String> {
        if src_name == dst_name {
            return Err("move-window: source and destination sessions are the same".to_string());
        }
        let src_i = self
            .sessions
            .iter()
            .position(|s| s.name == src_name)
            .ok_or_else(|| format!("can't find session: {src_name}"))?;
        let dst_i = self
            .sessions
            .iter()
            .position(|s| s.name == dst_name)
            .ok_or_else(|| format!("can't find session: {dst_name}"))?;
        if !self.sessions[src_i].windows.iter().any(|w| w.id == id) {
            return Err("window not found".to_string());
        }
        if self.sessions[src_i].windows.len() == 1 {
            return Err("can't move the only window out of its session".to_string());
        }
        if let Some(i) = index {
            let occupied = self.sessions[dst_i].windows.iter().any(|w| w.index == i);
            if occupied && !kill {
                return Err(format!("index in use: {i}"));
            }
        }
        let window = self.sessions[src_i]
            .take_window(id)
            .expect("presence just checked above");
        if let Some(i) = index {
            // NOTE: uses `take_window`, not `kill_window` -- `kill_window`
            // refuses to remove a session's ONLY window (a real UI-facing
            // guard: killing it would leave zero windows visible), but the
            // destination here is about to receive the incoming window
            // immediately below, so it is never actually left empty. The
            // detached `Window` (and its panes) is intentionally dropped --
            // the CALLER (`server/dispatch.rs::exec_move_window`) is
            // responsible for cleaning up its pane runtime state, same
            // pre-snapshot-then-remove pattern the SAME-session path already
            // follows for `Session::move_window`'s own `kill_window` call.
            if let Some(occ_id) = self.sessions[dst_i].windows.iter().find(|w| w.index == i).map(|w| w.id) {
                self.sessions[dst_i].take_window(occ_id);
            }
        }
        let dst = &mut self.sessions[dst_i];
        let dst_original_current = dst.current;
        let new_id = dst.insert_window(window, index).expect("occupancy resolved above");
        if select {
            dst.last = Some(dst_original_current);
            dst.current = new_id;
        }
        Ok(())
    }

    /// Build a fresh single-pane `Window` (same defaults as
    /// `Session::new_window`) and insert it into session `session_name` at
    /// `index` (explicit) or its lowest free slot -- the cross-session-
    /// capable primitive `break-pane -t <session[:index]>` (SP7 Task 11,
    /// closes follow-up #44) needs, since `Session::new_window` itself only
    /// ever targets its OWN session at the lowest free slot and always
    /// forces focus onto the new window. Does NOT touch `current`/`last` --
    /// the caller decides focus (mirrors `Session::insert_window`, which
    /// this delegates to after building the `Window` -- kept private to
    /// `model.rs` so `Window`-construction defaults stay in one place, not
    /// duplicated into `server/dispatch.rs`). `id` must come from
    /// `mint_window_id`. Errors `"can't find session: <session_name>"` /
    /// `"index in use: <i>"` (see `Session::insert_window`).
    pub fn insert_new_window(
        &mut self,
        session_name: &str,
        id: WindowId,
        first_pane: PaneId,
        index: Option<u32>,
    ) -> Result<WindowId, String> {
        let session = self
            .session_mut(session_name)
            .ok_or_else(|| format!("can't find session: {session_name}"))?;
        let window = Session::build_window(id, first_pane);
        session.insert_window(window, index)
    }
}

impl Session {
    /// tmux clear-on-visit (SP7 Task 17, closes follow-up #74): clears
    /// window `id`'s alert flags, if it exists in this session. Called from
    /// every method below that reassigns `self.current` to `id`, AND
    /// (`pub(crate)`, since several real current-changing dispatch paths --
    /// `exec_select_window`, `find-window`, the status-row window click,
    /// choose-tree's Enter commit, `break-pane -d` -- mutate `session.
    /// current`/`session.last` directly in `server/dispatch.rs` rather than
    /// going through one of this module's own methods) by every one of
    /// those call sites too.
    pub(crate) fn clear_alerts_for(&mut self, id: WindowId) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.clear_alerts();
        }
    }

    /// Build (but do not insert anywhere) a fresh single-pane `Window` with
    /// the same defaults `new_window`/`Registry::create_session` use (tmux
    /// default name "powershell", `auto_rename: true`, an empty option
    /// overlay). `index` is left `0` -- the caller (`insert_window`, or
    /// `Registry::insert_new_window` for a cross-session placement) always
    /// overwrites it once it knows which index space the window lands in.
    fn build_window(id: WindowId, first_pane: PaneId) -> Window {
        Window {
            id,
            index: 0,
            name: "powershell".to_string(),
            layout: Layout::new(first_pane),
            last_layout: None,
            auto_rename: true,
            last_auto_rename: None,
            window_options: options::Overlay::new(),
            alert_bell: false,
            alert_activity: false,
            alert_silence: false,
            activity_fired: false,
            silence_fired: false,
            last_output: Instant::now(),
        }
    }

    /// Index = lowest unused >= this session's `base_index`. The new window
    /// becomes current (`last` <- previous current).
    pub fn new_window(&mut self, id: WindowId, first_pane: PaneId) -> &mut Window {
        let window = Self::build_window(id, first_pane);
        let new_id = self.insert_window(window, None).expect("a None index never collides");
        self.last = Some(self.current);
        self.current = new_id;
        self.windows.iter_mut().find(|w| w.id == new_id).expect("just inserted")
    }

    /// Insert an already-built `Window` (SP7 Task 11: `break-pane -t
    /// <session[:index]>`/cross-session `move-window` both need to place a
    /// Window OBJECT that already exists -- either freshly built by
    /// `build_window`, or lifted wholesale from another session by
    /// `take_window` -- rather than building one in place the way
    /// `new_window` does) at `index` (explicit) or the lowest free slot >=
    /// this session's `base_index`. Errors `"index in use: {i}"` if `index`
    /// names an already-occupied slot (no kill/shuffle support -- matches
    /// `move_window`'s own honest same-session scope). Does NOT touch
    /// `current`/`last` -- every caller decides focus for itself (unlike
    /// `new_window`, which is always a direct "create and switch to" user
    /// action). `self.windows` stays sorted by index.
    fn insert_window(&mut self, mut window: Window, index: Option<u32>) -> Result<WindowId, String> {
        let idx = match index {
            Some(i) => {
                if self.windows.iter().any(|w| w.index == i) {
                    return Err(format!("index in use: {i}"));
                }
                i
            }
            None => lowest_unused_index(&self.windows, self.base_index),
        };
        window.index = idx;
        let id = window.id;
        let pos = self.windows.iter().position(|w| w.index > idx).unwrap_or(self.windows.len());
        self.windows.insert(pos, window);
        Ok(id)
    }

    /// Remove window `id` from this session and hand the `Window` OBJECT
    /// back to the caller, so it can be re-inserted elsewhere (SP7 Task 11:
    /// `Registry::move_window_to_session`'s cross-session
    /// `move-window`/`break-pane -t <session>` primitive). Unlike
    /// `kill_window` (which DESTROYS the window outright and refuses to
    /// remove a session's only window), this has no "only window" guard of
    /// its own -- the caller (`move_window_to_session`) is responsible for
    /// that decision, since whether emptying the session is acceptable
    /// depends on what the caller plans to do about it. `current`/`last`
    /// retargeting mirrors `kill_window`'s exactly (fall back to `last` if
    /// still alive, else the nearest remaining window by index), except it
    /// tolerates ending up with ZERO windows left (falls back to a
    /// placeholder `0` — the caller is expected to have already decided
    /// this session is being destroyed in that case, so the placeholder is
    /// never observed). `None` if `id` isn't found.
    fn take_window(&mut self, id: WindowId) -> Option<Window> {
        let pos = self.windows.iter().position(|w| w.id == id)?;
        let killed_index = self.windows[pos].index;
        let window = self.windows.remove(pos);

        if self.last == Some(id) {
            self.last = None;
        }
        if self.current == id {
            let fallback = self.last.take().filter(|&l| self.windows.iter().any(|w| w.id == l));
            self.current = fallback.unwrap_or_else(|| {
                self.windows
                    .iter()
                    .filter(|w| w.index < killed_index)
                    .max_by_key(|w| w.index)
                    .or_else(|| self.windows.iter().min_by_key(|w| w.index))
                    .map(|w| w.id)
                    .unwrap_or(0)
            });
        }
        Some(window)
    }

    /// Remove window `id`. If it was current, retarget current to `last`
    /// (if still alive) or else the nearest window by index. `false` (no
    /// change) if `id` is the only window, or isn't found.
    pub fn kill_window(&mut self, id: WindowId) -> bool {
        if self.windows.len() == 1 {
            return false;
        }
        let pos = match self.windows.iter().position(|w| w.id == id) {
            Some(p) => p,
            None => return false,
        };
        let killed_index = self.windows[pos].index;
        self.windows.remove(pos);

        if self.last == Some(id) {
            self.last = None;
        }
        if self.current == id {
            let fallback = self
                .last
                .take()
                .filter(|&l| self.windows.iter().any(|w| w.id == l));
            self.current = fallback.unwrap_or_else(|| self.nearest_by_index(killed_index));
            self.clear_alerts_for(self.current);
        }
        true
    }

    /// The window whose index is nearest `killed_index`: prefer the
    /// highest index below it, else the lowest index above it. Only called
    /// with at least one window remaining.
    fn nearest_by_index(&self, killed_index: u32) -> WindowId {
        self.windows
            .iter()
            .filter(|w| w.index < killed_index)
            .max_by_key(|w| w.index)
            .or_else(|| self.windows.iter().min_by_key(|w| w.index))
            .map(|w| w.id)
            .expect("caller guarantees at least one window remains")
    }

    /// Select by exact tmux window index. `false` (no change) if no window
    /// has that index.
    pub fn select_window(&mut self, index: u32) -> bool {
        match self.windows.iter().find(|w| w.index == index) {
            Some(w) => {
                let id = w.id;
                if id != self.current {
                    self.last = Some(self.current);
                    self.current = id;
                    self.clear_alerts_for(id);
                }
                true
            }
            None => false,
        }
    }

    /// Cycle current to the next/previous window by index order, wrapping.
    /// No-op with a single window.
    pub fn next_window(&mut self) {
        self.step_window(true);
    }

    pub fn prev_window(&mut self) {
        self.step_window(false);
    }

    fn step_window(&mut self, forward: bool) {
        if self.windows.len() <= 1 {
            return;
        }
        let pos = match self.windows.iter().position(|w| w.id == self.current) {
            Some(p) => p,
            None => return,
        };
        let len = self.windows.len();
        let new_pos = if forward { (pos + 1) % len } else { (pos + len - 1) % len };
        let new_id = self.windows[new_pos].id;
        self.last = Some(self.current);
        self.current = new_id;
        self.clear_alerts_for(new_id);
    }

    /// Toggle current <-> last, if `last` still exists. `false` if there is
    /// no last window (or it no longer exists).
    pub fn last_window(&mut self) -> bool {
        if let Some(l) = self.last {
            if self.windows.iter().any(|w| w.id == l) {
                let old = self.current;
                self.current = l;
                self.last = Some(old);
                self.clear_alerts_for(l);
                return true;
            }
        }
        false
    }

    pub fn current_window(&self) -> &Window {
        self.windows
            .iter()
            .find(|w| w.id == self.current)
            .expect("current always names a live window")
    }

    pub fn current_window_mut(&mut self) -> &mut Window {
        let current = self.current;
        self.windows
            .iter_mut()
            .find(|w| w.id == current)
            .expect("current always names a live window")
    }

    /// The window whose layout contains `pane`, if any.
    pub fn window_by_pane(&mut self, pane: PaneId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.layout.panes().contains(&pane))
    }

    /// tmux `renumber-windows` support (SP3 Task 6; Task 7 review fix):
    /// reassign every window's `index` to `base_index + position` in
    /// `self.windows` order, preserving relative order. `self.windows` is
    /// already kept sorted by index by every mutator, so this simply closes
    /// any gaps left by `kill_window` — starting FROM the session's
    /// creation-time `base_index` floor (real tmux renumbers from
    /// `base-index`; renumbering from a hardcoded 0 would violate the floor
    /// every window in this session was created under).
    pub fn renumber(&mut self) {
        let base = self.base_index;
        for (i, w) in self.windows.iter_mut().enumerate() {
            w.index = base + i as u32;
        }
    }

    /// tmux `move-window` (Task 7, sub-project 4): reassign window `id`'s
    /// index to `new_index` within THIS session (winmux's `move-window`
    /// simplification, per the design spec's `## 6. Window ops` section, is
    /// same-session re-indexing only -- no `-s`-to-a-different-session
    /// support). If `new_index` is already occupied by a DIFFERENT window:
    /// `kill == false` refuses, `false` (the caller already knows the index
    /// it tried, so it formats `index in use: <n>` itself -- matches
    /// `Session::kill_window`'s own bool-not-Result convention for a
    /// caller-formats-the-message refusal); `kill == true` removes the
    /// occupant first via [`Self::kill_window`] (so if the occupant
    /// happened to be `current`/`last` -- not possible for `move-window`'s
    /// sole caller, which always moves `current` itself, but this stays
    /// correct for any future caller -- that bookkeeping stays consistent)
    /// and then `id` takes `new_index`. Moving a window to the index it
    /// ALREADY occupies is a harmless no-op success: there is no "occupant"
    /// in the way (judgment call, undocumented by the design spec -- real
    /// tmux's own move-to-self-index behavior wasn't pinned down; a
    /// same-index move being anything other than a no-op would be a
    /// strange surprise for a command whose entire point is re-indexing).
    ///
    /// `self.windows` is kept sorted by index (the invariant every other
    /// mutator maintains) -- re-sorted here too.
    pub fn move_window(&mut self, id: WindowId, new_index: u32, kill: bool) -> bool {
        if self.windows.iter().any(|w| w.id == id && w.index == new_index) {
            return true;
        }
        let occupant = self.windows.iter().find(|w| w.index == new_index).map(|w| w.id);
        if let Some(occ) = occupant {
            if !kill {
                return false;
            }
            self.kill_window(occ);
        }
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.index = new_index;
        }
        self.windows.sort_by_key(|w| w.index);
        true
    }

    /// tmux relative window-target resolution (`+N`/`-N`, SP6 Task 5): the
    /// window `offset` slots after (positive) or before (negative) `from`
    /// in INDEX order, WRAPPING (`winlink_next_by_number`/
    /// `winlink_previous_by_number`, `windows-and-sessions.md` §"Target
    /// resolution", cmd-find.c:396-407 -- `self.windows` is already kept
    /// sorted by index by every mutator, so a plain wrapping walk over the
    /// vector reproduces the winlink-tree walk exactly). `None` if `from`
    /// isn't a live window in this session. A single-window session always
    /// returns `from` itself (a 0-step wrap), matching a one-entry RB tree's
    /// "next after the only entry is itself" wraparound.
    pub fn window_relative(&self, from: WindowId, offset: i64) -> Option<WindowId> {
        let len = self.windows.len() as i64;
        if len == 0 {
            return None;
        }
        let pos = self.windows.iter().position(|w| w.id == from)? as i64;
        let steps = offset.rem_euclid(len);
        let new_pos = ((pos + steps).rem_euclid(len)) as usize;
        Some(self.windows[new_pos].id)
    }

    /// tmux `swap-window` primitive (SP6 Task 5, `windows-and-sessions.md`
    /// §swap-window): exchange `src` and `dst`'s `index` values in place --
    /// each window OBJECT (id, name, layout, `last_layout`, `auto_rename`
    /// state -- everything but `index`) keeps its own identity; only which
    /// index it sits at trades places. This mirrors real tmux's actual
    /// mechanism exactly ("the two winlinks stay at their indexes; their
    /// `->window` pointers are exchanged") since winmux has no separate
    /// winlink type -- here the `index` field on the (id-keyed) `Window`
    /// struct doubles as that winlink identity. `self.windows` stays sorted
    /// by index afterward.
    ///
    /// Also resolves `current`/`last` bookkeeping, since winmux tracks both
    /// by `WindowId` (content) where tmux tracks them by winlink (slot/
    /// index) -- the two only coincide when a window's index never changes,
    /// which a swap explicitly violates for `src`/`dst`. Both branches are
    /// captured with `src`'s and `dst'`s PRE-swap `current`/`last` values,
    /// then:
    /// - **`detach == false`** (no `-d`): tmux leaves `curw` (the winlink /
    ///   slot pointer) UNTOUCHED, so whichever window is displayed at that
    ///   slot is free to change underneath it. Translated to WindowId
    ///   tracking: if `current` (or `last`) named `src` or `dst`, it FLIPS
    ///   to the other id (same slot, new content); anything else is
    ///   untouched.
    /// - **`detach == true`** (`-d` given): tmux calls
    ///   `session_select(dst_session, wl_dst->idx)` -- select BY THE FIXED
    ///   INDEX that was `dst`'s, which post-swap is now occupied by `src`.
    ///   That makes `src` the new `current` (regardless of what `current`
    ///   was beforehand), and — mirroring `session_set_current`'s "push the
    ///   OLD curw onto the lastw stack" step — sets `last` to whatever
    ///   `current` WOULD have flipped to under the no-`-d` rule above (i.e.
    ///   the same-slot post-swap content of the window that was current
    ///   before this call). **EXCEPT** (review fix, round 1) when the
    ///   pre-swap `current == dst`: the reselect target (dst's original
    ///   slot, now showing `src`) IS the current slot in that case, and
    ///   `session_set_current` early-returns (`if (wl == s->curw) return
    ///   1;`, session.c:475-498) without touching curw or lastw at all --
    ///   so the whole `-d` select is a no-op and the bookkeeping
    ///   degenerates to exactly the no-`-d` rule (`current` flips dst ->
    ///   src via the same slot-content logic; `last` flips only if it
    ///   named `src`/`dst`, and is otherwise untouched -- never
    ///   overwritten).
    ///
    /// `false` (no-op, nothing swapped) if `src == dst`, or either id isn't
    /// a live window in this session -- mirrors tmux's own "no-op success if
    /// both winlinks already point at the same window" rule (the `src ==
    /// dst` case; an unknown id is a winmux-specific defensive addition,
    /// since real tmux can't reach this primitive with an unresolved
    /// target at all).
    pub fn swap_windows(&mut self, src: WindowId, dst: WindowId, detach: bool) -> bool {
        if src == dst {
            return false;
        }
        let (isrc, idst) = match (
            self.windows.iter().position(|w| w.id == src),
            self.windows.iter().position(|w| w.id == dst),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };
        let idx_src = self.windows[isrc].index;
        let idx_dst = self.windows[idst].index;
        self.windows[isrc].index = idx_dst;
        self.windows[idst].index = idx_src;
        self.windows.sort_by_key(|w| w.index);

        let flip = |id: WindowId| if id == src { dst } else if id == dst { src } else { id };
        let flipped_current = flip(self.current);
        // `-d`'s reselect targets dst's original slot (post-swap content:
        // src); when the pre-swap current == dst, that slot IS the current
        // slot and tmux's `session_set_current` early-returns without
        // touching curw/lastw (see the doc comment) -- so only a select
        // that actually CHANGES the current slot takes the detach branch.
        if detach && self.current != dst {
            self.last = Some(flipped_current);
            self.current = src;
        } else {
            self.current = flipped_current;
            self.last = self.last.map(flip);
        }
        self.clear_alerts_for(self.current);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SZ: (u16, u16) = (80, 24);

    #[test]
    fn auto_name_fills_gaps() {
        let mut r = Registry::new();
        assert_eq!(r.auto_name(), "0");
        assert_eq!(r.create_session(None, 1, SZ, 0).unwrap().name, "0");
        assert_eq!(r.auto_name(), "1");
        assert_eq!(r.create_session(None, 2, SZ, 0).unwrap().name, "1");
        assert!(r.kill_session("0"));
        assert_eq!(r.auto_name(), "0");
        assert_eq!(r.create_session(None, 3, SZ, 0).unwrap().name, "0");
    }

    #[test]
    fn duplicate_session_err_string() {
        let mut r = Registry::new();
        r.create_session(Some("x"), 1, SZ, 0).unwrap();
        assert_eq!(
            r.create_session(Some("x"), 2, SZ, 0).err().unwrap(),
            "duplicate session: x"
        );
    }

    #[test]
    fn find_exact_then_prefix() {
        let mut r = Registry::new();
        r.create_session(Some("foo"), 1, SZ, 0).unwrap();
        r.create_session(Some("foobar"), 2, SZ, 0).unwrap();
        // Exact match wins even though "foo" is also a prefix of "foobar".
        assert_eq!(r.find("foo").unwrap().name, "foo");
        // Unambiguous prefix match.
        assert_eq!(r.find("foob").unwrap().name, "foobar");
        // Ambiguous prefix: matches both "foo" and "foobar".
        assert_eq!(r.find("fo").err().unwrap(), "can't find session: fo");
        // No match at all.
        assert_eq!(r.find("zzz").err().unwrap(), "can't find session: zzz");
    }

    #[test]
    fn find_empty_target_picks_most_recent() {
        let mut r = Registry::new();
        r.create_session(Some("foo"), 1, SZ, 0).unwrap();
        r.create_session(Some("bar"), 2, SZ, 0).unwrap();
        r.create_session(Some("qux"), 3, SZ, 0).unwrap();
        // Multiple sessions: "" is NOT an (always-matching, ambiguous)
        // empty prefix — it picks the most recently CREATED session.
        assert_eq!(r.find("").unwrap().name, "qux");
        // Still creation-recency (not name order) after the newest dies.
        assert!(r.kill_session("qux"));
        assert_eq!(r.find("").unwrap().name, "bar");
    }

    #[test]
    fn find_empty_target_no_sessions_is_error() {
        let mut r = Registry::new();
        assert_eq!(r.find("").err().unwrap(), "no sessions");
    }

    #[test]
    fn find_eq_forces_exact() {
        let mut r = Registry::new();
        r.create_session(Some("foo"), 1, SZ, 0).unwrap();
        r.create_session(Some("foobar"), 2, SZ, 0).unwrap();
        assert_eq!(r.find("=foo").unwrap().name, "foo");
        // Would be an unambiguous prefix match for "foobar" without the `=`
        // sigil; `=` forces exact-only, so it's not found.
        assert_eq!(r.find("=foob").err().unwrap(), "can't find session: foob");
    }

    #[test]
    fn bad_names_rejected() {
        let mut r = Registry::new();
        assert_eq!(
            r.create_session(Some(""), 1, SZ, 0).err().unwrap(),
            "bad session name: "
        );
        assert_eq!(
            r.create_session(Some("a:b"), 1, SZ, 0).err().unwrap(),
            "bad session name: a:b"
        );
        assert_eq!(
            r.create_session(Some("a.b"), 1, SZ, 0).err().unwrap(),
            "bad session name: a.b"
        );
    }

    /// Final-review fix (2026-07-07): control chars (C0 incl. `\n`/`\r`/ESC,
    /// plus 0x7f DEL) must be rejected — an unfiltered name reaches the
    /// status-bar span text and is written raw to the terminal. The
    /// rejected name is echoed back in the error, but sanitized (control
    /// chars -> `?`) so the echo itself can't re-inject the same bytes into
    /// stderr/status text.
    #[test]
    fn names_with_control_chars_rejected() {
        let mut r = Registry::new();
        assert_eq!(
            r.create_session(Some("foo\nbar"), 1, SZ, 0).err().unwrap(),
            "bad session name: foo?bar"
        );
        assert_eq!(
            r.create_session(Some("a\x1b[31mred"), 1, SZ, 0).err().unwrap(),
            "bad session name: a?[31mred"
        );
        assert_eq!(
            r.create_session(Some("del\x7f"), 1, SZ, 0).err().unwrap(),
            "bad session name: del?"
        );
    }

    #[test]
    fn window_index_lowest_unused() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        assert_eq!(s.windows[0].index, 0);
        s.new_window(2, 10); // index 1
        s.new_window(3, 20); // index 2
        assert_eq!(
            s.windows.iter().map(|w| w.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let mid_id = s.windows[1].id; // the window at index 1
        assert!(s.kill_window(mid_id));
        assert_eq!(
            s.windows.iter().map(|w| w.index).collect::<Vec<_>>(),
            vec![0, 2]
        );
        let w = s.new_window(4, 30);
        assert_eq!(w.index, 1); // lowest unused
    }

    /// tmux `base-index` (SP3 Task 7, wired through `set -g base-index`):
    /// the FIRST window is created at `base_index`, not 0, and every later
    /// `new_window` respects the same floor (killing the first window and
    /// creating a new one must not renumber back down to 0).
    #[test]
    fn base_index_offsets_window_numbering() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 1).unwrap();
        assert_eq!(s.windows[0].index, 1);
        s.new_window(2, 10);
        assert_eq!(s.windows.iter().map(|w| w.index).collect::<Vec<_>>(), vec![1, 2]);
        let first_id = s.windows[0].id;
        assert!(s.kill_window(first_id));
        let w = s.new_window(3, 20);
        assert_eq!(w.index, 1); // lowest unused >= base_index (1), never 0
    }

    /// Task 7 review fix (Critical): `renumber()` must close gaps starting
    /// FROM the session's base index, not from 0 — real tmux renumbers from
    /// `base-index`, so `base-index 1` + `renumber-windows on` never
    /// produces a window 0.
    #[test]
    fn renumber_respects_base_index() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 1).unwrap(); // idx 1
        s.new_window(2, 10); // idx 2
        s.new_window(3, 20); // idx 3
        let first_id = s.windows[0].id; // the window at index 1
        assert!(s.kill_window(first_id)); // survivors at 2, 3
        s.renumber();
        assert_eq!(
            s.windows.iter().map(|w| w.index).collect::<Vec<_>>(),
            vec![1, 2] // NOT 0, 1
        );
    }

    #[test]
    fn kill_current_window_falls_back_to_last() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        let w0 = s.current;
        s.new_window(2, 10); // current=2, last=w0
        s.new_window(3, 20); // current=3, last=2
        assert_eq!(s.current, 3);
        assert_eq!(s.last, Some(2));
        assert!(s.kill_window(3)); // last(2) alive -> falls back to it
        assert_eq!(s.current, 2);
        assert_eq!(s.last, None);
        assert_eq!(w0, 0);
    }

    #[test]
    fn kill_window_falls_back_to_nearest_when_last_dead() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // idx1, current=2, last=0
        s.new_window(3, 20); // idx2, current=3, last=2
        s.new_window(4, 30); // idx3, current=4, last=3

        // Kill window 3 (not current): last (3) is stale, cleared to None.
        assert!(s.kill_window(3));
        assert_eq!(s.current, 4);
        assert_eq!(s.last, None);

        // Kill current (4) with no last -> nearest by index among the
        // remaining [id0 idx0, id2 idx1]: highest index below the killed
        // window's index (3) is idx1 -> id2.
        assert!(s.kill_window(4));
        assert_eq!(s.current, 2);
    }

    #[test]
    fn kill_window_only_window_returns_false() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        let only = s.current;
        assert!(!s.kill_window(only));
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn last_window_toggles() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        assert!(!s.last_window()); // no last yet
        let w0 = s.current;
        s.new_window(2, 10); // current=2, last=w0
        assert!(s.last_window());
        assert_eq!(s.current, w0);
        assert_eq!(s.last, Some(2));
        assert!(s.last_window());
        assert_eq!(s.current, 2);
        assert_eq!(s.last, Some(w0));
    }

    #[test]
    fn next_prev_wrap() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // idx1
        s.new_window(3, 20); // idx2, current=3
        assert_eq!(s.current, 3);
        s.next_window(); // wraps past the end -> idx0
        assert_eq!(s.current, 0);
        s.prev_window(); // wraps back -> idx2
        assert_eq!(s.current, 3);
        s.prev_window(); // -> idx1
        assert_eq!(s.current, 2);
    }

    #[test]
    fn next_prev_noop_with_single_window() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        let only = s.current;
        s.next_window();
        assert_eq!(s.current, only);
        s.prev_window();
        assert_eq!(s.current, only);
    }

    #[test]
    fn mint_window_id_does_not_collide_with_create_session() {
        let mut r = Registry::new();
        // create_session mints id 0 internally for the session's first window.
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        assert_eq!(s.current, 0);
        // mint_window_id continues the SAME counter, so the next id is 1 —
        // never re-minting an id already claimed by an existing window.
        let minted = r.mint_window_id();
        assert_eq!(minted, 1);
        let s = r.session_mut("s").unwrap();
        let w = s.new_window(minted, 10);
        assert_eq!(w.id, 1);
        assert_eq!(w.index, 1);
        // Next mint continues from 2, still no collision.
        assert_eq!(r.mint_window_id(), 2);
    }

    #[test]
    fn neighbor_session_wraps() {
        let mut r = Registry::new();
        r.create_session(Some("0"), 1, SZ, 0).unwrap();
        r.create_session(Some("1"), 2, SZ, 0).unwrap();
        r.create_session(Some("2"), 3, SZ, 0).unwrap();
        assert_eq!(r.neighbor_session("0", true), Some("1"));
        assert_eq!(r.neighbor_session("2", true), Some("0")); // wraps forward
        assert_eq!(r.neighbor_session("0", false), Some("2")); // wraps backward
        assert_eq!(r.neighbor_session("missing", true), None);
    }

    #[test]
    fn select_window_exact_index() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        let w0 = s.current;
        s.new_window(2, 10); // idx1, current=2, last=w0
        s.new_window(3, 20); // idx2, current=3, last=2
        assert!(s.select_window(0));
        assert_eq!(s.current, w0);
        assert_eq!(s.last, Some(3));
        assert!(!s.select_window(99)); // no such index -> no change
        assert_eq!(s.current, w0);
    }

    #[test]
    fn current_and_window_by_pane() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap();
        assert_eq!(s.current_window().layout.panes(), vec![1]);
        s.new_window(2, 10);
        assert_eq!(s.current_window_mut().id, 2);
        assert_eq!(s.window_by_pane(1).unwrap().id, 0);
        assert_eq!(s.window_by_pane(10).unwrap().id, 2);
        assert!(s.window_by_pane(999).is_none());
    }

    /// `move_window` (Task 7): reassigns the index and keeps `windows`
    /// sorted; moving onto a free index is a plain success.
    #[test]
    fn move_window_reindexes() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1
        assert!(s.move_window(0, 5, false));
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(2, 1), (0, 5)] // re-sorted by index
        );
    }

    /// Moving onto an OCCUPIED index without `kill` refuses and changes
    /// nothing.
    #[test]
    fn move_window_occupied_errors_without_kill() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1
        assert!(!s.move_window(0, 1, false));
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(0, 0), (2, 1)] // unchanged
        );
    }

    /// `kill == true` removes the occupant and the mover takes its index.
    #[test]
    fn move_window_kill_occupant() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1
        assert!(s.move_window(0, 1, true));
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(0, 1)] // id2 (the occupant) is gone
        );
    }

    /// Moving a window to the index it already occupies is a harmless
    /// no-op success, not an "occupied" error (there is no OTHER window in
    /// the way).
    #[test]
    fn move_window_to_own_index_is_noop() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        assert!(s.move_window(0, 0, false));
        assert_eq!(s.windows[0].index, 0);
    }

    // ---- cross-session move-window (SP7 Task 11, closes #45) --------------

    /// The window OBJECT (id, name) moves untouched into the destination
    /// session, landing at the destination's LOWEST FREE index (no explicit
    /// index given) -- "reindexes the destination" in the sense that the
    /// window's `index` field is whatever the destination session assigns
    /// it, independent of whatever index it held in the source. The source
    /// session keeps its remaining window(s), current/last retargeted the
    /// same way `kill_window` would.
    #[test]
    fn move_window_across_sessions_reindexes_destination() {
        let mut r = Registry::new();
        r.create_session(Some("src"), 1, SZ, 0).unwrap(); // src: id0 idx0
        {
            let s = r.session_mut("src").unwrap();
            s.new_window(2, 10); // src: id2 idx1, current=2, last=Some(0)
        }
        let dst = r.create_session(Some("dst"), 100, SZ, 0).unwrap(); // dst: id_d idx0
        let dst_first_id = dst.current;
        dst.new_window(200, 20); // dst: id200 idx1, current=200, last=Some(dst_first_id)

        // Move src's window 0 (id0) into dst with no explicit index -> lands
        // at dst's lowest free slot (index 2, since 0 and 1 are taken).
        assert!(r.move_window_to_session("src", 0, "dst", None, false, true).is_ok());

        let dst = r.session_mut("dst").unwrap();
        assert_eq!(
            dst.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(dst_first_id, 0), (200, 1), (0, 2)]
        );
        // `select: true` -> the moved window becomes dst's current.
        assert_eq!(dst.current, 0);
        assert_eq!(dst.last, Some(200));

        // Source session survives with its remaining window; current falls
        // back the same way `kill_window` would (last, since it named the
        // survivor).
        let src = r.session_mut("src").unwrap();
        assert_eq!(src.windows.iter().map(|w| w.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(src.current, 2);
    }

    /// An explicit destination index is honored exactly (not just "lowest
    /// free").
    #[test]
    fn move_window_across_sessions_explicit_index() {
        let mut r = Registry::new();
        r.create_session(Some("src"), 1, SZ, 0).unwrap();
        {
            let s = r.session_mut("src").unwrap();
            s.new_window(2, 10);
        }
        r.create_session(Some("dst"), 100, SZ, 0).unwrap();
        assert!(r.move_window_to_session("src", 0, "dst", Some(7), false, true).is_ok());
        let dst = r.session_mut("dst").unwrap();
        assert!(dst.windows.iter().any(|w| w.id == 0 && w.index == 7));
    }

    /// An occupied explicit destination index without `kill` is an honest
    /// error and changes nothing on either side.
    #[test]
    fn move_window_across_sessions_occupied_index_errors() {
        let mut r = Registry::new();
        r.create_session(Some("src"), 1, SZ, 0).unwrap();
        {
            let s = r.session_mut("src").unwrap();
            s.new_window(2, 10);
        }
        r.create_session(Some("dst"), 100, SZ, 0).unwrap(); // dst: idx0 taken
        let err = r.move_window_to_session("src", 0, "dst", Some(0), false, true).unwrap_err();
        assert_eq!(err, "index in use: 0");
        // Source untouched: window 0 is still there.
        let src = r.session_mut("src").unwrap();
        assert_eq!(src.windows.iter().map(|w| w.id).collect::<Vec<_>>(), vec![0, 2]);
        // Destination's occupant is untouched too.
        let dst = r.session_mut("dst").unwrap();
        assert_eq!(dst.windows.len(), 1);
    }

    /// `kill: true` on an occupied explicit destination index removes the
    /// occupant and the mover takes its place (mirrors the same-session
    /// `move_window_kill_occupant` test's contract).
    #[test]
    fn move_window_across_sessions_kill_occupant() {
        let mut r = Registry::new();
        r.create_session(Some("src"), 1, SZ, 0).unwrap();
        {
            let s = r.session_mut("src").unwrap();
            s.new_window(2, 10);
        }
        r.create_session(Some("dst"), 100, SZ, 0).unwrap(); // dst: id100 idx0
        assert!(r.move_window_to_session("src", 0, "dst", Some(0), true, true).is_ok());
        let dst = r.session_mut("dst").unwrap();
        assert_eq!(dst.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(), vec![(0, 0)]);
    }

    /// Moving a session's ONLY window across sessions is refused outright
    /// (documented narrowing vs. real tmux, which destroys the emptied
    /// source session -- see `move_window_to_session`'s doc comment).
    #[test]
    fn move_window_across_sessions_refuses_to_empty_source() {
        let mut r = Registry::new();
        r.create_session(Some("src"), 1, SZ, 0).unwrap(); // src's ONLY window
        r.create_session(Some("dst"), 100, SZ, 0).unwrap();
        let err = r.move_window_to_session("src", 0, "dst", None, false, true).unwrap_err();
        assert_eq!(err, "can't move the only window out of its session");
        assert_eq!(r.session_mut("src").unwrap().windows.len(), 1);
    }

    #[test]
    fn move_window_across_sessions_unknown_session_or_window_errors() {
        let mut r = Registry::new();
        r.create_session(Some("src"), 1, SZ, 0).unwrap();
        r.create_session(Some("dst"), 100, SZ, 0).unwrap();
        assert_eq!(
            r.move_window_to_session("nope", 0, "dst", None, false, true).unwrap_err(),
            "can't find session: nope"
        );
        assert_eq!(
            r.move_window_to_session("src", 0, "nope", None, false, true).unwrap_err(),
            "can't find session: nope"
        );
        assert_eq!(
            r.move_window_to_session("src", 999, "dst", None, false, true).unwrap_err(),
            "window not found"
        );
    }

    #[test]
    fn move_window_across_sessions_same_name_errors() {
        let mut r = Registry::new();
        r.create_session(Some("s"), 1, SZ, 0).unwrap();
        assert_eq!(
            r.move_window_to_session("s", 0, "s", None, false, true).unwrap_err(),
            "move-window: source and destination sessions are the same"
        );
    }

    // ---- swap-window (SP6 Task 5) ------------------------------------------

    /// The pure index exchange: ids/content stay put, only `index` values
    /// trade places; `windows` stays sorted by index. `current` unrelated to
    /// either swapped window is untouched; `last`, if it named one of the
    /// swapped windows, FLIPS to the other id (winlink/slot membership
    /// travels with the index, not the content -- see `swap_windows`'s doc
    /// comment).
    #[test]
    fn swap_windows_exchanges_indices_keeps_ids() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1, current=2, last=Some(0)
        s.new_window(4, 11); // id4 idx2, current=4, last=Some(2)
        assert!(s.swap_windows(0, 2, false));
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(2, 0), (0, 1), (4, 2)]
        );
        // current (id4) named neither swapped window -> untouched.
        assert_eq!(s.current, 4);
        // last was Some(2) (== dst) -> flips to src (0): the slot that WAS
        // "last" now shows the other window's content.
        assert_eq!(s.last, Some(0));
    }

    /// Without `-d`: tmux leaves `curw` (the SLOT) untouched, so the client
    /// stays on the same index and sees whatever now occupies it -- in
    /// WindowId terms, `current`/`last` FLIP when they named `src`/`dst`.
    #[test]
    fn swap_windows_without_detach_flips_current_to_other_window() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1, current=2, last=Some(0)
        // Current is window 2 (index 1); swap it (src) with window 0 (dst,
        // index 0), no -d.
        assert!(s.swap_windows(2, 0, false));
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(2, 0), (0, 1)]
        );
        // Index 1 (where the client was looking) now shows window 0's
        // content -- current becomes dst (0), "the window that came from
        // N" per the doc.
        assert_eq!(s.current, 0);
        // last was Some(0) (== dst) -> flips to src (2).
        assert_eq!(s.last, Some(2));
    }

    /// With `-d`: focus follows the WINDOW OBJECT -- `session_select(dst,
    /// wl_dst->idx)` unconditionally selects whichever window now sits at
    /// dst's ORIGINAL index, which is always `src` post-swap; the OLD
    /// current (flipped to reflect its own post-swap slot content) becomes
    /// `last`, mirroring `session_set_current`'s push-onto-lastw step.
    #[test]
    fn swap_windows_with_detach_keeps_focus_on_source_window() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1, current=2, last=Some(0)
        assert!(s.swap_windows(2, 0, true)); // src = current window (2)
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(2, 0), (0, 1)]
        );
        // current stays on window 2 -- the user's ORIGINAL window, now
        // relocated to index 1 (dst's original index).
        assert_eq!(s.current, 2);
        // last = flip(old current = 2) = 0 (dst; the window that was
        // displaced).
        assert_eq!(s.last, Some(0));
    }

    /// Review fix (Task 5, round 1): `-d` when the pre-swap CURRENT window
    /// IS `dst` (reachable via explicit `-s`/`-t`, since `-t` resolves to
    /// the focused window). tmux's `session_select` -> `session_set_current`
    /// early-returns (`if (wl == s->curw) return 1;`, session.c:475-498)
    /// WITHOUT touching lastw when the reselect target is already the
    /// current winlink -- which is exactly this case: the `-d` reselect
    /// targets dst's ORIGINAL index/slot, and that slot IS the current slot
    /// when `current == dst`. So `last` must stay untouched (slot-wise);
    /// the buggy version unconditionally overwrote it with `Some(src)`.
    #[test]
    fn swap_windows_detach_when_current_is_dst_preserves_last() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1, current=2, last=Some(0)
        s.new_window(4, 11); // id4 idx2, current=4, last=Some(2)
        // Coordinator's scenario shape: current=idx1, last=idx0 -- select
        // idx0 then idx1.
        assert!(s.select_window(0)); // current=0, last=Some(4)
        assert!(s.select_window(1)); // current=2, last=Some(0)
        assert_eq!((s.current, s.last), (2, Some(0)));

        // swap -d with src = the third window (id4, idx2) and dst = the
        // CURRENT window (id2, idx1).
        assert!(s.swap_windows(4, 2, true));
        // Indices swap: id4 takes idx1, id2 takes idx2.
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(0, 0), (4, 1), (2, 2)]
        );
        // The -d reselect targets dst's original index (1), whose post-swap
        // content is src (id4) -- and since that slot IS the current slot,
        // tmux early-returns: `current` degenerates to the pure-swap flip
        // (the current slot idx1 now shows id4).
        assert_eq!(s.current, 4);
        // `last` (id0, a window unrelated to the swap) is UNTOUCHED -- the
        // early return never pushes anything onto lastw. The buggy version
        // set it to Some(4) (== current, doubly wrong).
        assert_eq!(s.last, Some(0));
    }

    /// Same early-return case, but with `last` naming `src`: the lastw SLOT
    /// is untouched by the early return, but its CONTENT changed with the
    /// swap -- in WindowId terms `last` flips src -> dst (the same rule as
    /// the non-detach branch, because `session_select` did nothing at all).
    #[test]
    fn swap_windows_detach_when_current_is_dst_flips_src_named_last() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1, current=2, last=Some(0)
        s.new_window(4, 11); // id4 idx2, current=4, last=Some(2)
        // current=idx1(id2), last=idx2(id4): re-selecting the already-
        // current id4 is a no-op (doesn't disturb last), then idx1.
        assert!(s.select_window(2)); // current=4, last=Some(2) (no-op)
        assert!(s.select_window(1)); // current=2, last=Some(4)
        assert_eq!((s.current, s.last), (2, Some(4)));

        // swap -d, src=id4 (== last), dst=id2 (== current).
        assert!(s.swap_windows(4, 2, true));
        assert_eq!(s.current, 4);
        // last's slot (idx2) now holds id2 -- last flips src(4) -> dst(2).
        // The buggy version overwrote it to Some(4) (== current).
        assert_eq!(s.last, Some(2));
    }

    /// Swapping a window with itself (or an id that isn't a live window in
    /// this session) is a no-op, mirroring tmux's own "no-op success if both
    /// winlinks already point at the same window" rule.
    #[test]
    fn swap_windows_same_id_or_unknown_id_is_noop() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1
        assert!(!s.swap_windows(0, 0, false));
        assert!(!s.swap_windows(0, 999, false));
        assert!(!s.swap_windows(999, 2, true));
        assert_eq!(
            s.windows.iter().map(|w| (w.id, w.index)).collect::<Vec<_>>(),
            vec![(0, 0), (2, 1)]
        );
    }

    /// `window_relative`: `+N`/`-N` winlink-offset resolution, wrapping at
    /// either end of the index-sorted window list.
    #[test]
    fn window_relative_wraps() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ, 0).unwrap(); // id0 idx0
        s.new_window(2, 10); // id2 idx1
        s.new_window(4, 11); // id4 idx2
        // From id0 (idx0): -1 wraps to the highest index (id4).
        assert_eq!(s.window_relative(0, -1), Some(4));
        // From id4 (idx2): +1 wraps to the lowest index (id0).
        assert_eq!(s.window_relative(4, 1), Some(0));
        // Plain forward/backward steps within range.
        assert_eq!(s.window_relative(0, 1), Some(2));
        assert_eq!(s.window_relative(4, -1), Some(2));
        // Multi-step offsets.
        assert_eq!(s.window_relative(0, 2), Some(4));
        assert_eq!(s.window_relative(0, -2), Some(2));
        // Unknown id -> None.
        assert_eq!(s.window_relative(999, 1), None);
    }

    #[test]
    fn registry_bookkeeping() {
        let mut r = Registry::new();
        assert!(r.is_empty());
        r.create_session(Some("a"), 1, SZ, 0).unwrap();
        assert!(!r.is_empty());
        assert_eq!(r.sessions().len(), 1);
        assert_eq!(r.sessions()[0].size, SZ);
        assert_eq!(r.sessions()[0].windows[0].name, "powershell");
        assert!(r.session_mut("a").is_some());
        assert!(r.session_mut("nope").is_none());
        assert!(!r.kill_session("nope"));
        assert!(r.kill_session("a"));
        assert!(r.is_empty());
    }
}
