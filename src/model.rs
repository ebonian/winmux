//! Session/window registry (pure bookkeeping, no I/O).
//!
//! Sits above `layout::Layout`: a `Window` owns one `Layout` (its split
//! tree); a `Session` owns an ordered list of `Window`s plus current/last
//! tracking; a `Registry` owns all `Session`s in creation order. Implements
//! tmux's naming/targeting/window-cycling semantics exactly (see
//! docs/specs/2026-07-07-server-client-design.md "Data model" and the
//! `## model` section of the sibling interfaces contract).

use crate::layout::{Layout, PaneId};
use std::time::SystemTime;

/// Server-global, monotonically increasing window id — NOT the tmux window
/// index (`Window::index`), which is per-session and reused after gaps.
pub type WindowId = u32;

pub struct Window {
    pub id: WindowId,
    /// tmux window index (lowest unused >= 0 at creation).
    pub index: u32,
    /// Default "powershell"; renamed via tmux prefix `,` (future task).
    pub name: String,
    pub layout: Layout,
    /// `next-layout`'s cycle position (Task 6, sub-project 4): the
    /// `layout::PRESET_CYCLE` index of the last preset APPLIED via
    /// `select-layout`/`next-layout` (`None` until the first one ever
    /// applied, or if the window's layout is currently a manual/custom tree
    /// -- manual splits/resizes never touch this field, so `next-layout`
    /// still resumes from wherever the cycle last landed, matching tmux).
    pub last_layout: Option<u8>,
}

pub struct Session {
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
        };
        let session = Session {
            name,
            created: SystemTime::now(),
            windows: vec![window],
            current: id,
            last: None,
            size,
            base_index,
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
}

impl Session {
    /// Index = lowest unused >= this session's `base_index`. The new window
    /// becomes current (`last` <- previous current).
    pub fn new_window(&mut self, id: WindowId, first_pane: PaneId) -> &mut Window {
        let index = lowest_unused_index(&self.windows, self.base_index);
        let window = Window {
            id,
            index,
            name: "powershell".to_string(),
            layout: Layout::new(first_pane),
            last_layout: None,
        };
        let pos = self
            .windows
            .iter()
            .position(|w| w.index > index)
            .unwrap_or(self.windows.len());
        self.windows.insert(pos, window);

        self.last = Some(self.current);
        self.current = id;
        self.windows.iter_mut().find(|w| w.id == id).expect("just inserted")
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
    }

    /// Toggle current <-> last, if `last` still exists. `false` if there is
    /// no last window (or it no longer exists).
    pub fn last_window(&mut self) -> bool {
        if let Some(l) = self.last {
            if self.windows.iter().any(|w| w.id == l) {
                let old = self.current;
                self.current = l;
                self.last = Some(old);
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
