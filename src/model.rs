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

/// Lowest index >= 0 not already used by `windows`.
fn lowest_unused_index(windows: &[Window]) -> u32 {
    let mut i = 0u32;
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
    /// The new session gets one window (index 0, default name
    /// "powershell") wrapping `first_pane`; that window becomes current.
    pub fn create_session(
        &mut self,
        name: Option<&str>,
        first_pane: PaneId,
        size: (u16, u16),
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
            index: 0,
            name: "powershell".to_string(),
            layout: Layout::new(first_pane),
        };
        let session = Session {
            name,
            created: SystemTime::now(),
            windows: vec![window],
            current: id,
            last: None,
            size,
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
    /// Index = lowest unused >= 0. The new window becomes current
    /// (`last` <- previous current).
    pub fn new_window(&mut self, id: WindowId, first_pane: PaneId) -> &mut Window {
        let index = lowest_unused_index(&self.windows);
        let window = Window {
            id,
            index,
            name: "powershell".to_string(),
            layout: Layout::new(first_pane),
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

    /// tmux `renumber-windows` support (SP3 Task 6): reassign every window's
    /// `index` to its position in `self.windows` (0..N), preserving relative
    /// order. `self.windows` is already kept sorted by index by every
    /// mutator, so this simply closes any gaps left by `kill_window`.
    pub fn renumber(&mut self) {
        for (i, w) in self.windows.iter_mut().enumerate() {
            w.index = i as u32;
        }
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
        assert_eq!(r.create_session(None, 1, SZ).unwrap().name, "0");
        assert_eq!(r.auto_name(), "1");
        assert_eq!(r.create_session(None, 2, SZ).unwrap().name, "1");
        assert!(r.kill_session("0"));
        assert_eq!(r.auto_name(), "0");
        assert_eq!(r.create_session(None, 3, SZ).unwrap().name, "0");
    }

    #[test]
    fn duplicate_session_err_string() {
        let mut r = Registry::new();
        r.create_session(Some("x"), 1, SZ).unwrap();
        assert_eq!(
            r.create_session(Some("x"), 2, SZ).err().unwrap(),
            "duplicate session: x"
        );
    }

    #[test]
    fn find_exact_then_prefix() {
        let mut r = Registry::new();
        r.create_session(Some("foo"), 1, SZ).unwrap();
        r.create_session(Some("foobar"), 2, SZ).unwrap();
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
        r.create_session(Some("foo"), 1, SZ).unwrap();
        r.create_session(Some("bar"), 2, SZ).unwrap();
        r.create_session(Some("qux"), 3, SZ).unwrap();
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
        r.create_session(Some("foo"), 1, SZ).unwrap();
        r.create_session(Some("foobar"), 2, SZ).unwrap();
        assert_eq!(r.find("=foo").unwrap().name, "foo");
        // Would be an unambiguous prefix match for "foobar" without the `=`
        // sigil; `=` forces exact-only, so it's not found.
        assert_eq!(r.find("=foob").err().unwrap(), "can't find session: foob");
    }

    #[test]
    fn bad_names_rejected() {
        let mut r = Registry::new();
        assert_eq!(
            r.create_session(Some(""), 1, SZ).err().unwrap(),
            "bad session name: "
        );
        assert_eq!(
            r.create_session(Some("a:b"), 1, SZ).err().unwrap(),
            "bad session name: a:b"
        );
        assert_eq!(
            r.create_session(Some("a.b"), 1, SZ).err().unwrap(),
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
            r.create_session(Some("foo\nbar"), 1, SZ).err().unwrap(),
            "bad session name: foo?bar"
        );
        assert_eq!(
            r.create_session(Some("a\x1b[31mred"), 1, SZ).err().unwrap(),
            "bad session name: a?[31mred"
        );
        assert_eq!(
            r.create_session(Some("del\x7f"), 1, SZ).err().unwrap(),
            "bad session name: del?"
        );
    }

    #[test]
    fn window_index_lowest_unused() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
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

    #[test]
    fn kill_current_window_falls_back_to_last() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
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
        let s = r.create_session(Some("s"), 1, SZ).unwrap(); // id0 idx0
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
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
        let only = s.current;
        assert!(!s.kill_window(only));
        assert_eq!(s.windows.len(), 1);
    }

    #[test]
    fn last_window_toggles() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
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
        let s = r.create_session(Some("s"), 1, SZ).unwrap(); // id0 idx0
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
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
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
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
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
        r.create_session(Some("0"), 1, SZ).unwrap();
        r.create_session(Some("1"), 2, SZ).unwrap();
        r.create_session(Some("2"), 3, SZ).unwrap();
        assert_eq!(r.neighbor_session("0", true), Some("1"));
        assert_eq!(r.neighbor_session("2", true), Some("0")); // wraps forward
        assert_eq!(r.neighbor_session("0", false), Some("2")); // wraps backward
        assert_eq!(r.neighbor_session("missing", true), None);
    }

    #[test]
    fn select_window_exact_index() {
        let mut r = Registry::new();
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
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
        let s = r.create_session(Some("s"), 1, SZ).unwrap();
        assert_eq!(s.current_window().layout.panes(), vec![1]);
        s.new_window(2, 10);
        assert_eq!(s.current_window_mut().id, 2);
        assert_eq!(s.window_by_pane(1).unwrap().id, 0);
        assert_eq!(s.window_by_pane(10).unwrap().id, 2);
        assert!(s.window_by_pane(999).is_none());
    }

    #[test]
    fn registry_bookkeeping() {
        let mut r = Registry::new();
        assert!(r.is_empty());
        r.create_session(Some("a"), 1, SZ).unwrap();
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
