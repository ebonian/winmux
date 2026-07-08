//! tmux-style paste buffers (Task 3, sub-project 4): server-global storage
//! for `copy-selection-and-cancel`'s automatic buffers and `set-buffer`'s
//! named buffers. Pure module: no I/O. See the `## buffers` section of
//! `docs/specs/2026-07-07-parity-polish-interfaces.md`.
//!
//! Storage is a single insertion-ordered `Vec` (oldest first, newest last) —
//! simple and fine at this scale (a handful to a few dozen buffers). Each
//! entry tracks whether it is `automatic` (an unnamed `buffer%u` from
//! `copy-selection-and-cancel`/bare `set-buffer`) or manual (`set-buffer -b
//! <name>`): `buffer-limit` eviction only ever touches automatic entries,
//! never manual ones (tmux behavior).

/// One stored buffer entry.
struct Entry {
    name: String,
    data: String,
    automatic: bool,
}

/// Server-global paste-buffer store. `add_automatic`'s numbering
/// (`buffer%u`) comes from a counter that NEVER resets, even across
/// deletions/evictions — matches tmux (a deleted `buffer3` is never reused).
pub struct Buffers {
    entries: Vec<Entry>,
    next_auto: u64,
}

impl Buffers {
    pub fn new() -> Self {
        Buffers { entries: Vec::new(), next_auto: 0 }
    }

    /// Insert a new AUTOMATIC buffer named `buffer<N>` (never-reset counter),
    /// evicting the oldest automatic entries first so the total automatic
    /// count stays under `limit` — eviction happens BEFORE the insert, so
    /// the newest buffer always survives even when `limit` is reached
    /// exactly (tmux `grid_collect_history`-style "make room first" order).
    /// Manual (named, `set_named`) entries are never evicted. Returns the
    /// new buffer's name.
    pub fn add_automatic(&mut self, data: String, limit: u32) -> String {
        self.evict_to_fit(limit);
        let name = format!("buffer{}", self.next_auto);
        self.next_auto += 1;
        self.entries.push(Entry { name: name.clone(), data, automatic: true });
        name
    }

    fn evict_to_fit(&mut self, limit: u32) {
        loop {
            let auto_count = self.entries.iter().filter(|e| e.automatic).count();
            if auto_count < limit as usize {
                return;
            }
            // Oldest automatic entry = the first one found scanning from the
            // front (insertion order == push order).
            match self.entries.iter().position(|e| e.automatic) {
                Some(idx) => {
                    self.entries.remove(idx);
                }
                None => return, // no automatic entries left to evict
            }
        }
    }

    /// Set (insert or overwrite) a MANUAL, named buffer — exempt from
    /// `buffer-limit` eviction regardless of how many automatic buffers
    /// exist. Overwriting an existing name (whether it was automatic or
    /// manual before) replaces its data in place and marks it manual.
    pub fn set_named(&mut self, name: &str, data: String) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.name == name) {
            e.data = data;
            e.automatic = false;
        } else {
            self.entries.push(Entry { name: name.to_string(), data, automatic: false });
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.iter().find(|e| e.name == name).map(|e| e.data.as_str())
    }

    /// The most recently inserted buffer (`set-buffer`, `set_named`, or
    /// `add_automatic` — whichever ran last), i.e. `paste-buffer`'s default
    /// target. `None` when empty.
    pub fn newest(&self) -> Option<(&str, &str)> {
        self.entries.last().map(|e| (e.name.as_str(), e.data.as_str()))
    }

    /// Remove a buffer by name. `true` if one was present.
    pub fn delete(&mut self, name: &str) -> bool {
        match self.entries.iter().position(|e| e.name == name) {
            Some(idx) => {
                self.entries.remove(idx);
                true
            }
            None => false,
        }
    }

    /// Remove and return the name of the newest buffer (`delete-buffer`'s
    /// default target, no `-b`). `None` when empty.
    pub fn delete_newest(&mut self) -> Option<String> {
        self.entries.pop().map(|e| e.name)
    }

    /// `(name, size_in_bytes, sample)` for every buffer, oldest first
    /// (`list-buffers` order). `sample` is the first 200 `char`s with every
    /// control character replaced by `?` (never echo raw ESC/OSC/CSI bytes
    /// back to a client's terminal — mirrors `options::sanitize_control_chars`).
    pub fn list(&self) -> Vec<(String, usize, String)> {
        self.entries.iter().map(|e| (e.name.clone(), e.data.len(), sample(&e.data))).collect()
    }
}

impl Default for Buffers {
    fn default() -> Self {
        Buffers::new()
    }
}

fn sample(s: &str) -> String {
    s.chars().take(200).map(|c| if c.is_control() { '?' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_automatic_names_never_reuse() {
        let mut b = Buffers::new();
        assert_eq!(b.add_automatic("a".to_string(), 50), "buffer0");
        assert_eq!(b.add_automatic("b".to_string(), 50), "buffer1");
        b.delete("buffer0");
        // Deleting buffer0 does NOT reset the counter -- next is buffer2,
        // never a reused buffer0.
        assert_eq!(b.add_automatic("c".to_string(), 50), "buffer2");
    }

    #[test]
    fn newest_and_get() {
        let mut b = Buffers::new();
        b.add_automatic("first".to_string(), 50);
        b.add_automatic("second".to_string(), 50);
        assert_eq!(b.newest(), Some(("buffer1", "second")));
        assert_eq!(b.get("buffer0"), Some("first"));
        assert_eq!(b.get("nope"), None);
    }

    #[test]
    fn eviction_is_automatic_only_and_evicts_before_insert() {
        let mut b = Buffers::new();
        // limit 2: fill with 2 automatic buffers, then a 3rd must evict the
        // oldest (buffer0) BEFORE inserting, so the newest never gets
        // evicted and the total stays at exactly `limit`.
        b.add_automatic("a".to_string(), 2);
        b.add_automatic("b".to_string(), 2);
        assert_eq!(b.add_automatic("c".to_string(), 2), "buffer2");
        assert_eq!(b.get("buffer0"), None, "oldest automatic must be evicted");
        assert_eq!(b.get("buffer1"), Some("b"));
        assert_eq!(b.get("buffer2"), Some("c"));
        assert_eq!(b.list().len(), 2);
    }

    #[test]
    fn manual_named_buffers_exempt_from_eviction() {
        let mut b = Buffers::new();
        b.set_named("keepme", "important".to_string());
        // Fill past the automatic limit; the manual buffer must survive.
        b.add_automatic("a".to_string(), 1);
        b.add_automatic("b".to_string(), 1);
        assert_eq!(b.get("keepme"), Some("important"));
        // Only one automatic buffer (the newest) should remain.
        let autos: Vec<_> = b.list().into_iter().filter(|(n, ..)| n != "keepme").collect();
        assert_eq!(autos.len(), 1);
    }

    #[test]
    fn set_named_overwrite_in_place() {
        let mut b = Buffers::new();
        b.set_named("x", "one".to_string());
        b.set_named("x", "two".to_string());
        assert_eq!(b.get("x"), Some("two"));
        assert_eq!(b.list().len(), 1);
    }

    #[test]
    fn delete_newest_and_delete_by_name() {
        let mut b = Buffers::new();
        b.add_automatic("a".to_string(), 50);
        b.add_automatic("b".to_string(), 50);
        assert_eq!(b.delete_newest(), Some("buffer1".to_string()));
        assert_eq!(b.get("buffer1"), None);
        assert!(b.delete("buffer0"));
        assert!(!b.delete("buffer0"));
        assert_eq!(b.delete_newest(), None);
    }

    #[test]
    fn list_order_and_sample_truncation_and_control_chars() {
        let mut b = Buffers::new();
        b.add_automatic("a".to_string(), 50);
        let long = "x".repeat(250);
        b.add_automatic(long.clone(), 50);
        b.add_automatic("with\x1b[control".to_string(), 50);
        let list = b.list();
        assert_eq!(list[0].0, "buffer0");
        assert_eq!(list[0].1, 1);
        assert_eq!(list[0].2, "a");
        assert_eq!(list[1].1, 250);
        assert_eq!(list[1].2.chars().count(), 200);
        assert_eq!(list[2].2, "with?[control");
    }

    #[test]
    fn evict_to_fit_limit_zero_evicts_all_automatic() {
        let mut b = Buffers::new();
        b.set_named("manual", "keep".to_string());
        b.add_automatic("a".to_string(), 0);
        b.add_automatic("b".to_string(), 0);
        let names: Vec<String> = b.list().into_iter().map(|(n, ..)| n).collect();
        // Only the most recent automatic buffer survives a from-scratch
        // insert against limit 0 (evict-before-insert still lets the
        // brand-new one in), plus the exempt manual buffer.
        assert!(names.contains(&"manual".to_string()));
        assert_eq!(names.iter().filter(|n| n.starts_with("buffer")).count(), 1);
    }
}
