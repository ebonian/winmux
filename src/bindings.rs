//! tmux default key bindings table (Task 5, sub-project 3): [`Binding`]/
//! [`Bindings`], keyed by [`crate::keys::Key`] per [`crate::input::WhichTable`].
//!
//! `Bindings::default()` reproduces every hardcoded binding from the legacy
//! `src/input.rs`/`Action` machinery as [`crate::cmd::RawCmd`]s (store-don't-
//! resolve: `cmd::resolve` re-parses at execution time -- tmux does late
//! binding too), so rebinding (`bind-key`/`unbind-key`) works uniformly once
//! Task 6 wires the server's dispatcher onto this table. See the
//! `## bindings` section of
//! `docs/specs/2026-07-07-command-config-interfaces.md`.

use std::collections::HashMap;

use crate::cmd::RawCmd;
use crate::input::WhichTable;
use crate::keys::{self, Key, KeyCode};

/// One bound command: the command(s) to run when the binding fires (stored
/// unresolved) and whether the binding is repeatable (`-r`; matches tmux's
/// `bind-key -r`). Only the `C-arrow` resize bindings default to `true`.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub cmds: Vec<RawCmd>,
    pub repeat: bool,
}

/// The two key tables (`root`/`prefix`), matching tmux's `bind-key -T`.
pub struct Bindings {
    root: HashMap<Key, Binding>,
    prefix: HashMap<Key, Binding>,
}

fn table_name(t: WhichTable) -> &'static str {
    match t {
        WhichTable::Root => "root",
        WhichTable::Prefix => "prefix",
    }
}

fn cmd1(name: &str, args: &[&str]) -> RawCmd {
    RawCmd {
        name: name.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    }
}

fn char_key(c: char) -> Key {
    Key { code: KeyCode::Char(c), ctrl: false, meta: false, shift: false }
}

fn named(s: &str) -> Key {
    keys::parse_key(s).unwrap_or_else(|| panic!("bad default-binding key notation: {s}"))
}

impl Default for Bindings {
    /// tmux default bindings, matching the legacy hardcoded `InputMachine`
    /// exactly, re-expressed as commands: `%`/`"` split, arrows/`o`/`;`
    /// select/last pane (arrows NOT repeatable, matching tmux), `x`/`z`
    /// kill/zoom, `C-arrow` resize (repeatable), `c`/`n`/`p`/`l`/digits
    /// window nav, `&` kill-window, `,`/`$` rename (see the documented
    /// "no-name-argument means open the interactive prompt" rule below),
    /// `d` detach, `(`/`)` switch-client, the prefix key itself ->
    /// `send-prefix`, `:` -> `command-prompt`. The root table starts empty
    /// (no default `bind -n` bindings in SP3).
    ///
    /// `,`/`$` deviation: real tmux binds these to
    /// `command-prompt -I'#W' { rename-window '%%' }`-style templating that
    /// SP3's `cmd`/`command-prompt` don't support. Instead these bind
    /// directly to `rename-window`/`rename-session` with NO name argument;
    /// Task 6's dispatcher treats a rename command with no name argument,
    /// executed with a client context, as "open the interactive rename
    /// prompt" (matches sub-project 2's behavior).
    fn default() -> Bindings {
        let mut prefix: HashMap<Key, Binding> = HashMap::new();

        let mut b = |k: Key, cmds: Vec<RawCmd>, repeat: bool| {
            prefix.insert(k, Binding { cmds, repeat });
        };

        b(char_key('%'), vec![cmd1("split-window", &["-h"])], false);
        b(char_key('"'), vec![cmd1("split-window", &["-v"])], false);

        b(named("Up"), vec![cmd1("select-pane", &["-U"])], false);
        b(named("Down"), vec![cmd1("select-pane", &["-D"])], false);
        b(named("Left"), vec![cmd1("select-pane", &["-L"])], false);
        b(named("Right"), vec![cmd1("select-pane", &["-R"])], false);

        b(char_key('o'), vec![cmd1("select-pane", &["-t", ":.+"])], false);
        b(char_key(';'), vec![cmd1("last-pane", &[])], false);

        b(
            char_key('x'),
            vec![cmd1("confirm-before", &["-p", "kill-pane #P? (y/n)", "kill-pane"])],
            false,
        );
        b(char_key('z'), vec![cmd1("resize-pane", &["-Z"])], false);

        b(named("C-Up"), vec![cmd1("resize-pane", &["-U"])], true);
        b(named("C-Down"), vec![cmd1("resize-pane", &["-D"])], true);
        b(named("C-Left"), vec![cmd1("resize-pane", &["-L"])], true);
        b(named("C-Right"), vec![cmd1("resize-pane", &["-R"])], true);

        b(char_key('c'), vec![cmd1("new-window", &[])], false);
        b(char_key('n'), vec![cmd1("next-window", &[])], false);
        b(char_key('p'), vec![cmd1("previous-window", &[])], false);
        b(char_key('l'), vec![cmd1("last-window", &[])], false);

        for d in 0..=9u32 {
            let c = char::from_digit(d, 10).expect("0..=9 always yields a digit char");
            b(char_key(c), vec![cmd1("select-window", &["-t", &format!(":={d}")])], false);
        }

        b(
            char_key('&'),
            vec![cmd1("confirm-before", &["-p", "kill-window #W? (y/n)", "kill-window"])],
            false,
        );
        b(char_key(','), vec![cmd1("rename-window", &[])], false);
        b(char_key('$'), vec![cmd1("rename-session", &[])], false);
        b(char_key('d'), vec![cmd1("detach-client", &[])], false);
        b(char_key('('), vec![cmd1("switch-client", &["-p"])], false);
        b(char_key(')'), vec![cmd1("switch-client", &["-n"])], false);

        // Prefix pressed again: send a literal prefix byte (tmux binds the
        // prefix key itself, in the prefix table, to send-prefix).
        b(named("C-b"), vec![cmd1("send-prefix", &[])], false);

        b(char_key(':'), vec![cmd1("command-prompt", &[])], false);

        Bindings { root: HashMap::new(), prefix }
    }
}

impl Bindings {
    pub fn bind(&mut self, table: WhichTable, key: Key, binding: Binding) {
        self.table_mut(table).insert(key, binding);
    }

    /// Remove a binding; `true` if one was present.
    pub fn unbind(&mut self, table: WhichTable, key: &Key) -> bool {
        self.table_mut(table).remove(key).is_some()
    }

    pub fn unbind_all(&mut self, table: WhichTable) {
        self.table_mut(table).clear();
    }

    pub fn lookup(&self, table: WhichTable, key: &Key) -> Option<&Binding> {
        self.table_ref(table).get(key)
    }

    fn table_mut(&mut self, table: WhichTable) -> &mut HashMap<Key, Binding> {
        match table {
            WhichTable::Root => &mut self.root,
            WhichTable::Prefix => &mut self.prefix,
        }
    }

    fn table_ref(&self, table: WhichTable) -> &HashMap<Key, Binding> {
        match table {
            WhichTable::Root => &self.root,
            WhichTable::Prefix => &self.prefix,
        }
    }

    /// `list-keys` output: one `bind-key [-r] -T <table> <keyname>
    /// <command...>` line per binding, sorted by table then key name.
    pub fn list(&self) -> String {
        let mut entries: Vec<(WhichTable, Key, &Binding)> = Vec::new();
        for (k, v) in &self.prefix {
            entries.push((WhichTable::Prefix, *k, v));
        }
        for (k, v) in &self.root {
            entries.push((WhichTable::Root, *k, v));
        }
        entries.sort_by(|a, b| {
            table_name(a.0).cmp(table_name(b.0)).then_with(|| keys::key_name(&a.1).cmp(&keys::key_name(&b.1)))
        });
        entries
            .iter()
            .map(|(t, k, binding)| {
                let repeat = if binding.repeat { "-r " } else { "" };
                format!("bind-key {repeat}-T {} {} {}", table_name(*t), keys::key_name(k), render_cmds(&binding.cmds))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn render_cmd(c: &RawCmd) -> String {
    let mut parts = vec![c.name.clone()];
    for a in &c.args {
        if a.contains(' ') {
            parts.push(format!("\"{a}\""));
        } else {
            parts.push(a.clone());
        }
    }
    parts.join(" ")
}

fn render_cmds(cmds: &[RawCmd]) -> String {
    cmds.iter().map(render_cmd).collect::<Vec<_>>().join(" ; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Key {
        keys::parse_key(s).unwrap()
    }

    #[test]
    fn defaults_cover_current_behavior() {
        let b = Bindings::default();

        let split_h = b.lookup(WhichTable::Prefix, &key("%")).unwrap();
        assert_eq!(split_h.cmds, vec![RawCmd { name: "split-window".to_string(), args: vec!["-h".to_string()] }]);
        assert!(!split_h.repeat);

        let x = b.lookup(WhichTable::Prefix, &key("x")).unwrap();
        assert_eq!(
            x.cmds,
            vec![RawCmd {
                name: "confirm-before".to_string(),
                args: vec!["-p".to_string(), "kill-pane #P? (y/n)".to_string(), "kill-pane".to_string()],
            }]
        );
        assert!(!x.repeat);

        let cup = b.lookup(WhichTable::Prefix, &key("C-Up")).unwrap();
        assert_eq!(cup.cmds, vec![RawCmd { name: "resize-pane".to_string(), args: vec!["-U".to_string()] }]);
        assert!(cup.repeat);

        let colon = b.lookup(WhichTable::Prefix, &key(":")).unwrap();
        assert_eq!(colon.cmds, vec![RawCmd { name: "command-prompt".to_string(), args: vec![] }]);

        for (name, flag) in [("Up", "-U"), ("Down", "-D"), ("Left", "-L"), ("Right", "-R")] {
            let arrow = b.lookup(WhichTable::Prefix, &key(name)).unwrap();
            assert_eq!(arrow.cmds, vec![RawCmd { name: "select-pane".to_string(), args: vec![flag.to_string()] }]);
            assert!(!arrow.repeat, "{name} must not be repeatable (only C-arrows are)");
        }

        // Root table has no defaults in SP3.
        assert!(b.lookup(WhichTable::Root, &key("%")).is_none());
    }

    #[test]
    fn bind_unbind_roundtrip() {
        let mut b = Bindings::default();
        let k = key("M-q");
        assert!(b.lookup(WhichTable::Root, &k).is_none());

        b.bind(
            WhichTable::Root,
            k,
            Binding { cmds: vec![RawCmd { name: "detach-client".to_string(), args: vec![] }], repeat: false },
        );
        assert!(b.lookup(WhichTable::Root, &k).is_some());

        assert!(b.unbind(WhichTable::Root, &k));
        assert!(b.lookup(WhichTable::Root, &k).is_none());
        // Removing an already-absent binding reports false, not a panic.
        assert!(!b.unbind(WhichTable::Root, &k));
    }

    #[test]
    fn unbind_all_table() {
        let mut b = Bindings::default();
        assert!(b.lookup(WhichTable::Prefix, &key("%")).is_some());
        assert!(b.lookup(WhichTable::Prefix, &key(":")).is_some());

        b.unbind_all(WhichTable::Prefix);

        assert!(b.lookup(WhichTable::Prefix, &key("%")).is_none());
        assert!(b.lookup(WhichTable::Prefix, &key(":")).is_none());
    }

    #[test]
    fn list_keys_format_exact() {
        let mut b = Bindings::default();
        b.unbind_all(WhichTable::Prefix);
        b.unbind_all(WhichTable::Root);
        b.bind(
            WhichTable::Prefix,
            key("C-Up"),
            Binding { cmds: vec![RawCmd { name: "resize-pane".to_string(), args: vec!["-U".to_string()] }], repeat: true },
        );
        assert_eq!(b.list(), "bind-key -r -T prefix C-Up resize-pane -U");
    }
}
