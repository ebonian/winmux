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

/// The four key tables (`root`/`prefix`/`copy-mode`/`copy-mode-vi`), matching
/// tmux's `bind-key -T`. The two copy-mode tables (Task 2, sub-project 4) are
/// only ever consulted by the server after it substitutes them in for `Root`
/// while the acting client is in `ClientMode::Copy` — see the
/// `## copy-mode` contract section.
pub struct Bindings {
    root: HashMap<Key, Binding>,
    prefix: HashMap<Key, Binding>,
    copy_mode: HashMap<Key, Binding>,
    copy_mode_vi: HashMap<Key, Binding>,
}

fn table_name(t: WhichTable) -> &'static str {
    match t {
        WhichTable::Root => "root",
        WhichTable::Prefix => "prefix",
        WhichTable::CopyMode => "copy-mode",
        WhichTable::CopyModeVi => "copy-mode-vi",
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

        // Copy mode entry (Task 2, sub-project 4): `[` and `PPage` were
        // deliberately left unbound in SP3 (see that task's report) pending
        // copy mode existing at all.
        b(char_key('['), vec![cmd1("copy-mode", &[])], false);
        b(named("PPage"), vec![cmd1("copy-mode", &["-u"])], false);

        // Paste buffers (Task 3, sub-project 4): `]` paste (bracketed-paste
        // flag accepted-ignored, see the `## buffers` contract section),
        // `#` list (a binding shows a one-line summary, not the full
        // multi-line CLI output -- see `exec_list_buffers`), `-` delete the
        // newest buffer.
        b(char_key(']'), vec![cmd1("paste-buffer", &["-p"])], false);
        b(char_key('#'), vec![cmd1("list-buffers", &[])], false);
        b(char_key('-'), vec![cmd1("delete-buffer", &[])], false);

        // Layout presets + swap/rotate (Task 6, sub-project 4). `Space` is
        // bound under the literal space CHARACTER, not `named("Space")` --
        // same project-wide gotcha as copy mode's spacebar bindings (a real
        // spacebar press decodes as `Key{Char(' ')}`, never `KeyCode::Space`).
        b(char_key(' '), vec![cmd1("next-layout", &[])], false);
        b(named("M-1"), vec![cmd1("select-layout", &["even-horizontal"])], false);
        b(named("M-2"), vec![cmd1("select-layout", &["even-vertical"])], false);
        b(named("M-3"), vec![cmd1("select-layout", &["main-horizontal"])], false);
        b(named("M-4"), vec![cmd1("select-layout", &["main-vertical"])], false);
        b(named("M-5"), vec![cmd1("select-layout", &["tiled"])], false);
        b(char_key('{'), vec![cmd1("swap-pane", &["-U"])], false);
        b(char_key('}'), vec![cmd1("swap-pane", &["-D"])], false);
        // tmux: `C-o` rotate-window (bare, no `-D`), `M-o` rotate-window -D.
        b(named("C-o"), vec![cmd1("rotate-window", &[])], false);
        b(named("M-o"), vec![cmd1("rotate-window", &["-D"])], false);

        // Window ops (Task 7, sub-project 4): break-pane, and three
        // prompt-opening bindings. `!` dispatches `break-pane` directly (no
        // prompt, matches real tmux). `.`/`f`/`'` bind BARE to their real
        // tmux command names (`.`/`f`) or, for `'` (no distinct
        // "index-window" tmux command exists), to `select-window` bare --
        // `dispatch_client`'s `is_bare` special-casing (same established
        // "no-args-with-a-client-context opens the interactive prompt"
        // pattern as `,`/`$`'s rename bindings) intercepts all three before
        // `cmd::resolve` would otherwise error on a missing required arg.
        b(char_key('!'), vec![cmd1("break-pane", &[])], false);
        b(char_key('.'), vec![cmd1("move-window", &[])], false);
        b(char_key('f'), vec![cmd1("find-window", &[])], false);
        b(char_key('\''), vec![cmd1("select-window", &[])], false);

        Bindings { root: HashMap::new(), prefix, copy_mode: copy_mode_emacs_defaults(), copy_mode_vi: copy_mode_vi_defaults() }
    }
}

/// Default `copy-mode` (emacs `mode-keys`) table: movement/scroll/cancel
/// subset only (Task 2 scope — selection/search bindings are Tasks 3/4).
/// `H`/`M`/`L` (top/middle/bottom line) are tmux emacs-table bindings too,
/// but the design spec flags them as unverified for the emacs table; they
/// are bound in the vi table only here (documented deviation).
fn copy_mode_emacs_defaults() -> HashMap<Key, Binding> {
    let mut t: HashMap<Key, Binding> = HashMap::new();
    let mut b = |k: Key, name: &str, args: &[&str]| {
        t.insert(k, Binding { cmds: vec![cmd1(name, args)], repeat: false });
    };

    b(named("Left"), "copy-cursor-left", &[]);
    b(named("Right"), "copy-cursor-right", &[]);
    b(named("Up"), "copy-cursor-up", &[]);
    b(named("Down"), "copy-cursor-down", &[]);
    b(named("C-b"), "copy-cursor-left", &[]);
    b(named("C-f"), "copy-cursor-right", &[]);
    b(named("C-p"), "copy-cursor-up", &[]);
    b(named("C-n"), "copy-cursor-down", &[]);

    b(named("C-a"), "copy-start-of-line", &[]);
    b(named("Home"), "copy-start-of-line", &[]);
    b(named("C-e"), "copy-end-of-line", &[]);
    b(named("End"), "copy-end-of-line", &[]);

    b(named("M-<"), "copy-history-top", &[]);
    b(named("M->"), "copy-history-bottom", &[]);

    b(named("M-v"), "copy-page-up", &[]);
    b(named("C-v"), "copy-page-down", &[]);
    b(named("PPage"), "copy-page-up", &[]);
    b(named("NPage"), "copy-page-down", &[]);
    // Bound under the literal space CHARACTER, not `named("Space")` (Task 3
    // review fix): a real spacebar press decodes as `Key{Char(' ')}` -- the
    // decoder only ever produces `KeyCode::Space` for `Ctrl-Space` (byte
    // 0x00), so Task 2's original `named("Space")` registration was
    // unreachable by an actual keypress. Same rule as the vi table's
    // `Space -> copy-begin-selection`; decoder-level `Char(' ')`/`Space`
    // normalization stays follow-up #34.
    b(char_key(' '), "copy-page-down", &[]);

    b(char_key('q'), "copy-cancel", &[]);
    b(named("Escape"), "copy-cancel", &[]);

    // Selection (Task 3, sub-project 4).
    b(named("C-Space"), "copy-begin-selection", &[]);
    b(named("C-w"), "copy-selection-and-cancel", &[]);
    b(named("M-w"), "copy-selection-and-cancel", &[]);
    b(char_key('R'), "copy-rectangle-toggle", &[]);
    b(named("C-g"), "copy-clear-selection", &[]);
    b(char_key('o'), "copy-other-end", &[]);

    // Search (Task 4, sub-project 4).
    b(named("C-s"), "copy-search-forward", &[]);
    b(named("C-r"), "copy-search-backward", &[]);
    b(char_key('n'), "copy-search-again", &[]);
    b(char_key('N'), "copy-search-reverse", &[]);

    t
}

/// Default `copy-mode-vi` table: movement/scroll/cancel subset (Task 2) plus
/// selection (Task 3, sub-project 4): `Escape` -- left UNBOUND through Task
/// 2 -- is now bound to `clear-selection`, matching tmux.
fn copy_mode_vi_defaults() -> HashMap<Key, Binding> {
    let mut t: HashMap<Key, Binding> = HashMap::new();
    let mut b = |k: Key, name: &str, args: &[&str]| {
        t.insert(k, Binding { cmds: vec![cmd1(name, args)], repeat: false });
    };

    b(char_key('h'), "copy-cursor-left", &[]);
    b(char_key('l'), "copy-cursor-right", &[]);
    b(char_key('k'), "copy-cursor-up", &[]);
    b(char_key('j'), "copy-cursor-down", &[]);
    b(named("Left"), "copy-cursor-left", &[]);
    b(named("Right"), "copy-cursor-right", &[]);
    b(named("Up"), "copy-cursor-up", &[]);
    b(named("Down"), "copy-cursor-down", &[]);

    b(char_key('w'), "copy-next-word", &[]);
    b(char_key('b'), "copy-previous-word", &[]);
    b(char_key('e'), "copy-next-word-end", &[]);

    b(char_key('0'), "copy-start-of-line", &[]);
    b(char_key('$'), "copy-end-of-line", &[]);
    // `^` (first non-blank) is simplified to start-of-line in v1 (documented).
    b(char_key('^'), "copy-start-of-line", &[]);

    b(char_key('g'), "copy-history-top", &[]);
    b(char_key('G'), "copy-history-bottom", &[]);

    b(char_key('H'), "copy-top-line", &[]);
    b(char_key('M'), "copy-middle-line", &[]);
    b(char_key('L'), "copy-bottom-line", &[]);

    b(char_key('K'), "copy-scroll-up", &[]);
    b(char_key('J'), "copy-scroll-down", &[]);

    b(named("C-u"), "copy-halfpage-up", &[]);
    b(named("C-d"), "copy-halfpage-down", &[]);

    b(named("C-b"), "copy-page-up", &[]);
    b(named("C-f"), "copy-page-down", &[]);
    b(named("PPage"), "copy-page-up", &[]);
    b(named("NPage"), "copy-page-down", &[]);

    b(char_key('q'), "copy-cancel", &[]);

    // Selection (Task 3, sub-project 4). NOTE: a real spacebar press decodes
    // as `Key{Char(' ')}`, NEVER `Key{code: KeyCode::Space}` -- the decoder
    // only ever produces the `Space` code variant for `Ctrl-Space` (byte
    // 0x00, see `keys::classify_single_byte`); `KeyCode::Space` otherwise
    // exists purely for `parse_key("Space")`/`send-keys Space` notation. So
    // this binds the literal space CHARACTER, matching what a keypress
    // actually decodes to (the same latent gap affects the pre-existing
    // Task 2 emacs `Space -> copy-page-down` default, which is therefore
    // ALSO unreachable by a real spacebar press -- left as-is here since
    // fixing it is out of this task's scope; noted in the report).
    b(char_key(' '), "copy-begin-selection", &[]);
    b(char_key('v'), "copy-rectangle-toggle", &[]);
    b(named("Enter"), "copy-selection-and-cancel", &[]);
    b(named("Escape"), "copy-clear-selection", &[]);
    b(char_key('o'), "copy-other-end", &[]);

    // Search (Task 4, sub-project 4).
    b(char_key('/'), "copy-search-forward", &[]);
    b(char_key('?'), "copy-search-backward", &[]);
    b(char_key('n'), "copy-search-again", &[]);
    b(char_key('N'), "copy-search-reverse", &[]);

    t
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
            WhichTable::CopyMode => &mut self.copy_mode,
            WhichTable::CopyModeVi => &mut self.copy_mode_vi,
        }
    }

    fn table_ref(&self, table: WhichTable) -> &HashMap<Key, Binding> {
        match table {
            WhichTable::Root => &self.root,
            WhichTable::Prefix => &self.prefix,
            WhichTable::CopyMode => &self.copy_mode,
            WhichTable::CopyModeVi => &self.copy_mode_vi,
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
        for (k, v) in &self.copy_mode {
            entries.push((WhichTable::CopyMode, *k, v));
        }
        for (k, v) in &self.copy_mode_vi {
            entries.push((WhichTable::CopyModeVi, *k, v));
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

    /// The equivalence contract Task 6 leans on: EVERY default binding is
    /// asserted with its exact RawCmd name+args and repeat flag, matching
    /// the legacy hardcoded `InputMachine` behavior 1:1 — plus "nothing
    /// else": the prefix table is exactly this set and the root table is
    /// empty.
    #[test]
    fn defaults_cover_current_behavior() {
        let b = Bindings::default();

        // (key, expected cmd name, expected args, repeat)
        let expected: &[(&str, &str, &[&str], bool)] = &[
            ("%", "split-window", &["-h"], false),
            ("\"", "split-window", &["-v"], false),
            ("Up", "select-pane", &["-U"], false),
            ("Down", "select-pane", &["-D"], false),
            ("Left", "select-pane", &["-L"], false),
            ("Right", "select-pane", &["-R"], false),
            ("o", "select-pane", &["-t", ":.+"], false),
            (";", "last-pane", &[], false),
            ("x", "confirm-before", &["-p", "kill-pane #P? (y/n)", "kill-pane"], false),
            ("z", "resize-pane", &["-Z"], false),
            ("C-Up", "resize-pane", &["-U"], true),
            ("C-Down", "resize-pane", &["-D"], true),
            ("C-Left", "resize-pane", &["-L"], true),
            ("C-Right", "resize-pane", &["-R"], true),
            ("c", "new-window", &[], false),
            ("n", "next-window", &[], false),
            ("p", "previous-window", &[], false),
            ("l", "last-window", &[], false),
            ("0", "select-window", &["-t", ":=0"], false),
            ("1", "select-window", &["-t", ":=1"], false),
            ("2", "select-window", &["-t", ":=2"], false),
            ("3", "select-window", &["-t", ":=3"], false),
            ("4", "select-window", &["-t", ":=4"], false),
            ("5", "select-window", &["-t", ":=5"], false),
            ("6", "select-window", &["-t", ":=6"], false),
            ("7", "select-window", &["-t", ":=7"], false),
            ("8", "select-window", &["-t", ":=8"], false),
            ("9", "select-window", &["-t", ":=9"], false),
            ("&", "confirm-before", &["-p", "kill-window #W? (y/n)", "kill-window"], false),
            (",", "rename-window", &[], false),
            ("$", "rename-session", &[], false),
            ("d", "detach-client", &[], false),
            ("(", "switch-client", &["-p"], false),
            (")", "switch-client", &["-n"], false),
            ("C-b", "send-prefix", &[], false),
            (":", "command-prompt", &[], false),
            ("[", "copy-mode", &[], false),
            ("PPage", "copy-mode", &["-u"], false),
            ("]", "paste-buffer", &["-p"], false),
            ("#", "list-buffers", &[], false),
            ("-", "delete-buffer", &[], false),
            (" ", "next-layout", &[], false),
            ("M-1", "select-layout", &["even-horizontal"], false),
            ("M-2", "select-layout", &["even-vertical"], false),
            ("M-3", "select-layout", &["main-horizontal"], false),
            ("M-4", "select-layout", &["main-vertical"], false),
            ("M-5", "select-layout", &["tiled"], false),
            ("{", "swap-pane", &["-U"], false),
            ("}", "swap-pane", &["-D"], false),
            ("C-o", "rotate-window", &[], false),
            ("M-o", "rotate-window", &["-D"], false),
            ("!", "break-pane", &[], false),
            (".", "move-window", &[], false),
            ("f", "find-window", &[], false),
            ("'", "select-window", &[], false),
        ];

        for (k, name, args, repeat) in expected {
            let binding = b
                .lookup(WhichTable::Prefix, &key(k))
                .unwrap_or_else(|| panic!("default binding missing for prefix-{k}"));
            assert_eq!(
                binding.cmds,
                vec![RawCmd {
                    name: name.to_string(),
                    args: args.iter().map(|s| s.to_string()).collect(),
                }],
                "wrong command for prefix-{k}"
            );
            assert_eq!(binding.repeat, *repeat, "wrong repeat flag for prefix-{k}");
        }

        // ... and nothing else: the prefix table is exactly this set.
        assert_eq!(b.prefix.len(), expected.len());

        // Root table has no defaults in SP3.
        assert!(b.root.is_empty());
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

    /// Copy-mode (emacs) default table: the exact movement/scroll/cancel
    /// subset per the design spec, and nothing else (no selection/search
    /// bindings — Tasks 3/4).
    #[test]
    fn copy_mode_emacs_defaults_exact() {
        let b = Bindings::default();
        let expected: &[(&str, &str, &[&str])] = &[
            ("Left", "copy-cursor-left", &[]),
            ("Right", "copy-cursor-right", &[]),
            ("Up", "copy-cursor-up", &[]),
            ("Down", "copy-cursor-down", &[]),
            ("C-b", "copy-cursor-left", &[]),
            ("C-f", "copy-cursor-right", &[]),
            ("C-p", "copy-cursor-up", &[]),
            ("C-n", "copy-cursor-down", &[]),
            ("C-a", "copy-start-of-line", &[]),
            ("Home", "copy-start-of-line", &[]),
            ("C-e", "copy-end-of-line", &[]),
            ("End", "copy-end-of-line", &[]),
            ("M-<", "copy-history-top", &[]),
            ("M->", "copy-history-bottom", &[]),
            ("M-v", "copy-page-up", &[]),
            ("C-v", "copy-page-down", &[]),
            ("PPage", "copy-page-up", &[]),
            ("NPage", "copy-page-down", &[]),
            // The literal space char, NOT the "Space" key-name notation --
            // Task 3 review fix; see copy_mode_emacs_defaults' comment.
            (" ", "copy-page-down", &[]),
            ("q", "copy-cancel", &[]),
            ("Escape", "copy-cancel", &[]),
            ("C-Space", "copy-begin-selection", &[]),
            ("C-w", "copy-selection-and-cancel", &[]),
            ("M-w", "copy-selection-and-cancel", &[]),
            ("R", "copy-rectangle-toggle", &[]),
            ("C-g", "copy-clear-selection", &[]),
            ("o", "copy-other-end", &[]),
            ("C-s", "copy-search-forward", &[]),
            ("C-r", "copy-search-backward", &[]),
            ("n", "copy-search-again", &[]),
            ("N", "copy-search-reverse", &[]),
        ];
        for (k, name, args) in expected {
            let binding = b
                .lookup(WhichTable::CopyMode, &key(k))
                .unwrap_or_else(|| panic!("default copy-mode binding missing for {k}"));
            assert_eq!(
                binding.cmds,
                vec![RawCmd { name: name.to_string(), args: args.iter().map(|s| s.to_string()).collect() }],
                "wrong command for copy-mode {k}"
            );
            assert!(!binding.repeat);
        }
        assert_eq!(b.copy_mode.len(), expected.len());
    }

    /// Copy-mode-vi default table: same exactness check.
    #[test]
    fn copy_mode_vi_defaults_exact() {
        let b = Bindings::default();
        let expected: &[(&str, &str, &[&str])] = &[
            ("h", "copy-cursor-left", &[]),
            ("l", "copy-cursor-right", &[]),
            ("k", "copy-cursor-up", &[]),
            ("j", "copy-cursor-down", &[]),
            ("Left", "copy-cursor-left", &[]),
            ("Right", "copy-cursor-right", &[]),
            ("Up", "copy-cursor-up", &[]),
            ("Down", "copy-cursor-down", &[]),
            ("w", "copy-next-word", &[]),
            ("b", "copy-previous-word", &[]),
            ("e", "copy-next-word-end", &[]),
            ("0", "copy-start-of-line", &[]),
            ("$", "copy-end-of-line", &[]),
            ("^", "copy-start-of-line", &[]),
            ("g", "copy-history-top", &[]),
            ("G", "copy-history-bottom", &[]),
            ("H", "copy-top-line", &[]),
            ("M", "copy-middle-line", &[]),
            ("L", "copy-bottom-line", &[]),
            ("K", "copy-scroll-up", &[]),
            ("J", "copy-scroll-down", &[]),
            ("C-u", "copy-halfpage-up", &[]),
            ("C-d", "copy-halfpage-down", &[]),
            ("C-b", "copy-page-up", &[]),
            ("C-f", "copy-page-down", &[]),
            ("PPage", "copy-page-up", &[]),
            ("NPage", "copy-page-down", &[]),
            ("q", "copy-cancel", &[]),
            (" ", "copy-begin-selection", &[]),
            ("v", "copy-rectangle-toggle", &[]),
            ("Enter", "copy-selection-and-cancel", &[]),
            ("Escape", "copy-clear-selection", &[]),
            ("o", "copy-other-end", &[]),
            ("/", "copy-search-forward", &[]),
            ("?", "copy-search-backward", &[]),
            ("n", "copy-search-again", &[]),
            ("N", "copy-search-reverse", &[]),
        ];
        for (k, name, args) in expected {
            let binding = b
                .lookup(WhichTable::CopyModeVi, &key(k))
                .unwrap_or_else(|| panic!("default copy-mode-vi binding missing for {k}"));
            assert_eq!(
                binding.cmds,
                vec![RawCmd { name: name.to_string(), args: args.iter().map(|s| s.to_string()).collect() }],
                "wrong command for copy-mode-vi {k}"
            );
        }
        assert_eq!(b.copy_mode_vi.len(), expected.len());
    }

    #[test]
    fn list_keys_format_exact() {
        let mut b = Bindings::default();
        b.unbind_all(WhichTable::Prefix);
        b.unbind_all(WhichTable::Root);
        b.unbind_all(WhichTable::CopyMode);
        b.unbind_all(WhichTable::CopyModeVi);
        b.bind(
            WhichTable::Prefix,
            key("C-Up"),
            Binding { cmds: vec![RawCmd { name: "resize-pane".to_string(), args: vec!["-U".to_string()] }], repeat: true },
        );
        assert_eq!(b.list(), "bind-key -r -T prefix C-Up resize-pane -U");
    }
}
