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
use crate::keys::{self, Key, KeyCode, MouseKeyKind, MouseKeyLoc};

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

fn mkey(kind: MouseKeyKind, btn: u8, loc: MouseKeyLoc) -> Key {
    Key { code: KeyCode::MouseKey(kind, btn, loc), ctrl: false, meta: false, shift: false }
}

// ---- mouse default-action sentinels (Task 8, SP7 wave 3: table-driven
// mouse bindings, closes follow-ups #57, #67(a)/(b)) ----
//
// Some tmux mouse defaults need dispatch-local context (which pane is under
// the pointer, the drag anchor position, click-run state) that can't be
// expressed as static `RawCmd` args -- `server::dispatch::dispatch_mouse`
// compares a resolved binding's `cmds` against the exact values these
// functions return to decide "run the built-in Rust default logic" (the
// SAME code that ran unconditionally before this task) vs "the user rebound
// this key, execute whatever they bound generically". The sentinel command
// names below (`mouse-drag-border` etc.) are not real `cmd::resolve`
// commands -- they exist purely as unique, stable markers.
//
// The other four mouse defaults below (`mouse_default_wheel_up_pane_copy`
// and friends) need NO bespoke Rust logic at all: they're expressed as real,
// generically-executable command lists (`copy-mode -e`, `copy-scroll-up`,
// `copy-selection-and-cancel`, `previous-window`, `next-window` -- all
// pre-existing commands), so `dispatch_mouse` always runs whatever's bound
// for these through the normal command pipeline, default or overridden
// alike, with no separate comparison. See the `## mouse` contract section
// amendment for the full tmux-default -> winmux-substitution table.
pub(crate) fn mouse_default_drag_border() -> Vec<RawCmd> {
    vec![cmd1("mouse-drag-border", &[])]
}
pub(crate) fn mouse_default_drag_pane_enter_copy() -> Vec<RawCmd> {
    vec![cmd1("mouse-drag-pane-enter-copy", &[])]
}
pub(crate) fn mouse_default_drag_pane_select() -> Vec<RawCmd> {
    vec![cmd1("mouse-drag-pane-select", &[])]
}
pub(crate) fn mouse_default_double_click_pane() -> Vec<RawCmd> {
    vec![cmd1("mouse-double-click-pane", &[])]
}
pub(crate) fn mouse_default_triple_click_pane() -> Vec<RawCmd> {
    vec![cmd1("mouse-triple-click-pane", &[])]
}
pub(crate) fn mouse_default_status_select_window() -> Vec<RawCmd> {
    vec![cmd1("mouse-status-select-window", &[])]
}

/// `copy-mode -e` (enters copy mode with `scroll_exit` armed) then 5x
/// `copy-scroll-up` -- `5` mirrors `server::MOUSE_WHEEL_STEP`, a private
/// `server.rs` const this module can't reach across the crate's privacy
/// boundary; keep the two in sync by hand if either changes.
pub(crate) fn mouse_default_wheel_up_pane_root() -> Vec<RawCmd> {
    let mut v = vec![cmd1("copy-mode", &["-e"])];
    v.extend(std::iter::repeat_with(|| cmd1("copy-scroll-up", &[])).take(5));
    v
}
pub(crate) fn mouse_default_wheel_up_pane_copy() -> Vec<RawCmd> {
    std::iter::repeat_with(|| cmd1("copy-scroll-up", &[])).take(5).collect()
}
pub(crate) fn mouse_default_wheel_down_pane_copy() -> Vec<RawCmd> {
    std::iter::repeat_with(|| cmd1("copy-scroll-down", &[])).take(5).collect()
}
pub(crate) fn mouse_default_drag_end_pane_copy() -> Vec<RawCmd> {
    vec![cmd1("copy-selection-and-cancel", &[])]
}
pub(crate) fn mouse_default_wheel_up_status() -> Vec<RawCmd> {
    vec![cmd1("previous-window", &[])]
}
pub(crate) fn mouse_default_wheel_down_status() -> Vec<RawCmd> {
    vec![cmd1("next-window", &[])]
}

/// Root-table mouse defaults (`docs/tmux-reference/mouse.md` §7.1, the
/// subset winmux's dispatch classification actually reproduces -- see the
/// task report's substitution table for what's intentionally NOT bindable
/// yet, e.g. plain click-to-focus, which stays unconditional).
fn root_mouse_defaults() -> HashMap<Key, Binding> {
    let mut t: HashMap<Key, Binding> = HashMap::new();
    let mut b = |k: Key, cmds: Vec<RawCmd>| {
        t.insert(canonical_key(k), Binding { cmds, repeat: false });
    };
    b(mkey(MouseKeyKind::Drag, 1, MouseKeyLoc::Border), mouse_default_drag_border());
    b(mkey(MouseKeyKind::Drag, 1, MouseKeyLoc::Pane), mouse_default_drag_pane_enter_copy());
    b(mkey(MouseKeyKind::WheelUp, 0, MouseKeyLoc::Pane), mouse_default_wheel_up_pane_root());
    b(mkey(MouseKeyKind::Down, 1, MouseKeyLoc::Status), mouse_default_status_select_window());
    b(mkey(MouseKeyKind::WheelUp, 0, MouseKeyLoc::Status), mouse_default_wheel_up_status());
    b(mkey(MouseKeyKind::WheelDown, 0, MouseKeyLoc::Status), mouse_default_wheel_down_status());
    t
}

/// Copy-mode mouse defaults, shared verbatim between the emacs and vi tables
/// -- real tmux's mouse bindings are "byte-identical" between the two
/// (`docs/tmux-reference/mouse.md` §7.3).
fn copy_mode_mouse_defaults() -> HashMap<Key, Binding> {
    let mut t: HashMap<Key, Binding> = HashMap::new();
    let mut b = |k: Key, cmds: Vec<RawCmd>| {
        t.insert(canonical_key(k), Binding { cmds, repeat: false });
    };
    b(mkey(MouseKeyKind::Drag, 1, MouseKeyLoc::Pane), mouse_default_drag_pane_select());
    b(mkey(MouseKeyKind::DragEnd, 1, MouseKeyLoc::Pane), mouse_default_drag_end_pane_copy());
    b(mkey(MouseKeyKind::WheelUp, 0, MouseKeyLoc::Pane), mouse_default_wheel_up_pane_copy());
    b(mkey(MouseKeyKind::WheelDown, 0, MouseKeyLoc::Pane), mouse_default_wheel_down_pane_copy());
    b(mkey(MouseKeyKind::DoubleClick, 1, MouseKeyLoc::Pane), mouse_default_double_click_pane());
    b(mkey(MouseKeyKind::TripleClick, 1, MouseKeyLoc::Pane), mouse_default_triple_click_pane());
    t
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
            prefix.insert(canonical_key(k), Binding { cmds, repeat });
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

        // Overlays (Task 8, sub-project 4): `w` choose-tree (windows of the
        // current session), `s` choose-tree (sessions, collapsed), `q`
        // display-panes. See the design spec's `## 7. Overlays` section.
        b(char_key('w'), vec![cmd1("choose-tree", &["-w"])], false);
        b(char_key('s'), vec![cmd1("choose-tree", &["-s"])], false);
        b(char_key('q'), vec![cmd1("display-panes", &[])], false);
        // SP7 Task 14 (closes #48/#49): `=` choose-buffer, `D` choose-client
        // -- real tmux's own defaults (`key-bindings.c:412,414`).
        b(char_key('='), vec![cmd1("choose-buffer", &[])], false);
        b(char_key('D'), vec![cmd1("choose-client", &[])], false);

        // Clock mode (Task 10, sub-project 6 wave 2): `t`, matching real
        // tmux's default `prefix t` (`key-bindings.c:433`).
        b(char_key('t'), vec![cmd1("clock-mode", &[])], false);

        Bindings { root: root_mouse_defaults(), prefix, copy_mode: copy_mode_emacs_defaults(), copy_mode_vi: copy_mode_vi_defaults() }
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
        t.insert(canonical_key(k), Binding { cmds: vec![cmd1(name, args)], repeat: false });
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
    // Bound under the literal space CHARACTER, not `named("Space")` -- follows
    // up #34, now resolved via Bindings-layer `canonical_key` normalization.
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

    // SP7 Task 13 (closes follow-up #56): `C-k` copy-end-of-line-and-cancel,
    // `M-m` back-to-indentation. See `CopyAction::EndOfLineAndCancel`'s doc
    // comment (`src/cmd.rs`) for why `C-k` is winmux's own
    // `copy-end-of-line-and-cancel` rather than the tmux master branch's
    // pipe-always `copy-pipe-end-of-line-and-cancel`.
    b(named("C-k"), "copy-end-of-line-and-cancel", &[]);
    b(named("M-m"), "copy-back-to-indentation", &[]);

    // Search (Task 4, sub-project 4).
    b(named("C-s"), "copy-search-forward", &[]);
    b(named("C-r"), "copy-search-backward", &[]);
    b(char_key('n'), "copy-search-again", &[]);
    b(char_key('N'), "copy-search-reverse", &[]);

    t.extend(copy_mode_mouse_defaults());
    t
}

/// Default `copy-mode-vi` table: movement/scroll/cancel subset (Task 2) plus
/// selection (Task 3, sub-project 4): `Escape` -- left UNBOUND through Task
/// 2 -- is now bound to `clear-selection`, matching tmux.
fn copy_mode_vi_defaults() -> HashMap<Key, Binding> {
    let mut t: HashMap<Key, Binding> = HashMap::new();
    let mut b = |k: Key, name: &str, args: &[&str]| {
        t.insert(canonical_key(k), Binding { cmds: vec![cmd1(name, args)], repeat: false });
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

    t.extend(copy_mode_mouse_defaults());
    t
}

/// #34: `keys::classify_single_byte` (the live input decoder) never
/// produces `KeyCode::Space` for a real bare spacebar press (byte `0x20`
/// decodes as `KeyCode::Char(' ')`) -- `KeyCode::Space` only ever comes from
/// `keys::parse_key("Space")` (config/notation) or a real Ctrl-Space
/// keypress (byte `0x00`, which the decoder DOES special-case to
/// `KeyCode::Space` with `ctrl: true`). So a config/runtime
/// `bind ... Space ...` line stores a `Key{code: Space, ..}` that a real
/// spacebar keypress's decoded `Key{code: Char(' '), ..}` would never match
/// in a plain `HashMap` lookup.
///
/// Fix: canonicalize `KeyCode::Space` to `KeyCode::Char(' ')` -- preserving
/// `ctrl`/`meta`/`shift` as-is -- at every point a `Key` enters or leaves the
/// table (`bind`, `unbind`, `lookup`). This makes `Char(' ')` the single
/// canonical internal representation for the space key in every table,
/// reachable equally from `named("Space")` notation and a real keypress,
/// while a real Ctrl-Space press (`Key{code: Space, ctrl: true}`) still
/// canonicalizes to a DISTINCT key (`Key{code: Char(' '), ctrl: true}`) from
/// plain Space (`ctrl: false`) -- the two remain independently bindable
/// (see `ctrl_space_and_plain_space_remain_distinct`).
fn canonical_key(mut key: Key) -> Key {
    if key.code == KeyCode::Space {
        key.code = KeyCode::Char(' ');
    }
    key
}

impl Bindings {
    pub fn bind(&mut self, table: WhichTable, key: Key, binding: Binding) {
        self.table_mut(table).insert(canonical_key(key), binding);
    }

    /// Remove a binding; `true` if one was present.
    pub fn unbind(&mut self, table: WhichTable, key: &Key) -> bool {
        self.table_mut(table).remove(&canonical_key(*key)).is_some()
    }

    pub fn unbind_all(&mut self, table: WhichTable) {
        self.table_mut(table).clear();
    }

    pub fn lookup(&self, table: WhichTable, key: &Key) -> Option<&Binding> {
        self.table_ref(table).get(&canonical_key(*key))
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
            ("w", "choose-tree", &["-w"], false),
            ("s", "choose-tree", &["-s"], false),
            ("=", "choose-buffer", &[], false),
            ("D", "choose-client", &[], false),
            ("q", "display-panes", &[], false),
            ("t", "clock-mode", &[], false),
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

        // Root table: SP3 had none; Task 8 (SP7 wave 3) adds the mouse
        // defaults asserted in detail by `default_root_table_contains_mouse_bindings`
        // below -- and nothing else (no keyboard `bind -n` defaults exist).
        assert_eq!(b.root.len(), 6);
    }

    /// Task 8 (SP7 wave 3, closes #57/#67): the root table's mouse defaults,
    /// reproducing exactly the tmux-default behaviors winmux's dispatch
    /// classification wires up (`docs/tmux-reference/mouse.md` §7.1
    /// subset -- see the task report's substitution table for the rest).
    #[test]
    fn default_root_table_contains_mouse_bindings() {
        let b = Bindings::default();
        let expected: &[(&str, Vec<RawCmd>)] = &[
            ("MouseDrag1Border", mouse_default_drag_border()),
            ("MouseDrag1Pane", mouse_default_drag_pane_enter_copy()),
            ("WheelUpPane", mouse_default_wheel_up_pane_root()),
            ("MouseDown1Status", mouse_default_status_select_window()),
            ("WheelUpStatus", mouse_default_wheel_up_status()),
            ("WheelDownStatus", mouse_default_wheel_down_status()),
        ];
        for (k, cmds) in expected {
            let binding = b.lookup(WhichTable::Root, &key(k)).unwrap_or_else(|| panic!("default root mouse binding missing for {k}"));
            assert_eq!(&binding.cmds, cmds, "wrong command for root {k}");
            assert!(!binding.repeat);
        }
        assert_eq!(b.root.len(), expected.len());
        // `WheelDownPane` is deliberately unbound at root (real tmux has no
        // default there either -- see `docs/tmux-reference/mouse.md` §6).
        assert!(b.lookup(WhichTable::Root, &key("WheelDownPane")).is_none());
    }

    /// #34 (Space/`Char(' ')` binding equivalence): a binding registered
    /// under the `named("Space")` key-notation form must be reachable when
    /// looked up under `Char(' ')` -- the shape a REAL spacebar keypress
    /// actually decodes to (`keys::classify_single_byte` never produces
    /// `KeyCode::Space` for a bare 0x20 byte; see follow-up #34). And the
    /// reverse: a binding stored under `Char(' ')` is reachable via a
    /// `named("Space")` lookup key too -- both directions canonicalize to
    /// the same internal HashMap key.
    #[test]
    fn bind_named_space_fires_on_char_space_lookup() {
        let mut b = Bindings::default();
        b.bind(
            WhichTable::Root,
            key("Space"),
            Binding { cmds: vec![cmd1("next-layout", &[])], repeat: false },
        );
        let found = b
            .lookup(WhichTable::Root, &char_key(' '))
            .expect("Space-registered binding must be reachable via Char(' ') lookup");
        assert_eq!(found.cmds, vec![cmd1("next-layout", &[])]);

        // Reverse: store under Char(' '), look up via named("Space").
        let mut b2 = Bindings::default();
        b2.bind(WhichTable::Root, char_key(' '), Binding { cmds: vec![cmd1("select-pane", &["-U"])], repeat: false });
        let found2 = b2
            .lookup(WhichTable::Root, &key("Space"))
            .expect("Char(' ')-registered binding must be reachable via named(\"Space\") lookup");
        assert_eq!(found2.cmds, vec![cmd1("select-pane", &["-U"])]);
    }

    /// Cross-notation unbind: when a binding is stored under one Space
    /// notation (named or Char), unbind must work with the other notation too.
    /// Direction 1: bind via `named("Space")`, unbind via `Char(' ')`.
    /// Direction 2: bind via `Char(' ')`, unbind via `named("Space")`.
    #[test]
    fn unbind_space_across_notations_removes_binding() {
        // Direction 1: bind under named("Space"), unbind using Char(' ')
        let mut b = Bindings::default();
        b.bind(
            WhichTable::Root,
            key("Space"),
            Binding { cmds: vec![cmd1("next-layout", &[])], repeat: false },
        );
        assert!(b.lookup(WhichTable::Root, &key("Space")).is_some());
        // Now unbind using the Char(' ') form
        assert!(b.unbind(WhichTable::Root, &char_key(' ')));
        assert!(b.lookup(WhichTable::Root, &key("Space")).is_none());

        // Direction 2: bind under Char(' '), unbind via named("Space")
        let mut b2 = Bindings::default();
        b2.bind(
            WhichTable::Root,
            char_key(' '),
            Binding { cmds: vec![cmd1("select-pane", &["-U"])], repeat: false },
        );
        assert!(b2.lookup(WhichTable::Root, &char_key(' ')).is_some());
        // Now unbind using the named("Space") form
        assert!(b2.unbind(WhichTable::Root, &key("Space")));
        assert!(b2.lookup(WhichTable::Root, &char_key(' ')).is_none());
    }

    /// A real Ctrl-Space keypress decodes as `Key{code: Space, ctrl: true}`
    /// (`keys::classify_single_byte`'s byte-0x00 special case) -- distinct
    /// from a bare spacebar. The Space/`Char(' ')` canonicalization must not
    /// collide the two: `C-Space` and plain `Space` remain independently
    /// bindable, matching the pre-existing `copy-mode` emacs defaults
    /// (`C-Space` -> copy-begin-selection, plain `Space` -> copy-page-down).
    #[test]
    fn ctrl_space_and_plain_space_remain_distinct() {
        let b = Bindings::default();
        let plain = b.lookup(WhichTable::CopyMode, &char_key(' ')).expect("plain Space binding");
        assert_eq!(plain.cmds, vec![cmd1("copy-page-down", &[])]);
        let ctrl = b.lookup(WhichTable::CopyMode, &key("C-Space")).expect("C-Space binding");
        assert_eq!(ctrl.cmds, vec![cmd1("copy-begin-selection", &[])]);
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
            // SP7 Task 13, closes follow-up #56.
            ("C-k", "copy-end-of-line-and-cancel", &[]),
            ("M-m", "copy-back-to-indentation", &[]),
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
        // + 6: the copy-mode mouse defaults (Task 8, SP7 wave 3), asserted
        // in detail by `copy_mode_mouse_defaults_exact` below.
        assert_eq!(b.copy_mode.len(), expected.len() + 6);
    }

    /// Task 8 (SP7 wave 3): the copy-mode/copy-mode-vi mouse defaults are
    /// "byte-identical" in real tmux (`docs/tmux-reference/mouse.md` §7.3) --
    /// asserted once here for both tables via the shared sentinel
    /// generators.
    #[test]
    fn copy_mode_mouse_defaults_exact() {
        let b = Bindings::default();
        let expected: &[(&str, Vec<RawCmd>)] = &[
            ("MouseDrag1Pane", mouse_default_drag_pane_select()),
            ("MouseDragEnd1Pane", mouse_default_drag_end_pane_copy()),
            ("WheelUpPane", mouse_default_wheel_up_pane_copy()),
            ("WheelDownPane", mouse_default_wheel_down_pane_copy()),
            ("DoubleClick1Pane", mouse_default_double_click_pane()),
            ("TripleClick1Pane", mouse_default_triple_click_pane()),
        ];
        for table in [WhichTable::CopyMode, WhichTable::CopyModeVi] {
            for (k, cmds) in expected {
                let binding = b.lookup(table, &key(k)).unwrap_or_else(|| panic!("default {table:?} mouse binding missing for {k}"));
                assert_eq!(&binding.cmds, cmds, "wrong command for {table:?} {k}");
            }
        }
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
        // + 6: the copy-mode-vi mouse defaults (Task 8, SP7 wave 3),
        // asserted in detail by `copy_mode_mouse_defaults_exact` above.
        assert_eq!(b.copy_mode_vi.len(), expected.len() + 6);
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
