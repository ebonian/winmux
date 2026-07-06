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

/// tmux target names reject empty names and the two target separators.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains(':') || name.contains('.') {
        return Err(format!("bad session name: {name}"));
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
                validate_name(n)?;
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
    /// sigil already stripped).
    pub fn find(&mut self, target: &str) -> Result<&mut Session, String> {
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
