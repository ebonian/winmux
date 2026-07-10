//! Typed tmux option registry. The general tmux format-expansion engine
//! (`#`/`%`/`#{...}`, conditionals, comparisons, length limits) lives in
//! [`crate::format`] as of SP7 Task 1 — [`FormatCtx`]/[`SystemTimeParts`]/
//! [`expand_format`] are re-exported/delegated here for source
//! compatibility (see below).
//!
//! Pure module: no I/O, `std` only. Depends on [`crate::keys`] (the `prefix`
//! option's `Key` type) and [`crate::style`] (every `*-style` option's
//! [`style::PartialStyle`]).
//!
//! [`Options`] holds one global table of tmux options (SP3 scope: no
//! per-session/window overlays — `-g`/`-w` on `set-option`/`show-options`
//! are both accepted but hit the same global table; documented deviation,
//! see the design spec's "Explicit deviations" section). [`Options::set`]
//! validates and stores a value by name; [`Options::show`]/[`show_all`]
//! print values back in tmux style; typed getters (`prefix()`,
//! `status_style()`, ...) are used by the server (Task 6) and renderer.
//!
//! `Str`-kind values (`status-left`/`status-right`/`default-command`/
//! `default-terminal`) reject control characters on `set` (including `-a`
//! append, validated against the appended RESULT) — see the review note on
//! the contract's `## options` section for why [`expand_format`]'s OUTPUT
//! needs no matching guard: its only live inputs are already
//! control-char-clean (`#S`/`#W` come from `model::validate_name`-guarded
//! session/window names, strftime output is fixed-format digits/month/
//! weekday abbreviations), so a clean `status-left`/`status-right` template
//! can only ever expand to a clean result — the SP7 format engine's wider
//! grammar (conditionals, comparisons, length limits) only rearranges/
//! selects among these same clean inputs, it never introduces a new
//! character class, so the guarantee still holds.
//!
//! [`expand_format`] delegates to [`crate::format::expand`], used by
//! `status-left`/`status-right`/`display-message`/window-status formats.

use crate::grid::Color;
use crate::keys::{self, Key};
use crate::style::{self, PartialStyle};
use std::collections::BTreeMap;
use std::time::Duration;

/// A parsed option value. Styles keep their original source string alongside
/// the parsed [`PartialStyle`] so [`Options::show`] can round-trip the
/// user's own text (defaults show their canonical default strings below)
/// instead of re-serializing the parsed struct.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Flag(bool),
    Number(u32),
    Key(Key),
    Choice(&'static str),
    Str(String),
    Style(String, PartialStyle),
}

/// Which shape an option's value must take; drives `set`'s parsing/validation
/// and `set -a`'s "string options only" rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Flag,
    Number,
    Key,
    Choice,
    Str,
    Style,
}

/// One row of the static option table: name, kind, and the default value
/// (built fresh by `default_value` — `Value` isn't `Copy` because of the
/// owned `String`/`PartialStyle` cases).
struct Spec {
    name: &'static str,
    kind: Kind,
    choices: &'static [&'static str],
}

/// The stored default for `window-status-format`/`window-status-current-format`
/// — tmux's REAL literal default string, unchanged since SP7 Task 1's
/// general format engine (`src/format.rs`) now evaluates its
/// `#{?window_flags,#{window_flags}, }` conditional correctly (closes
/// follow-up #70; superseded the SP6 Task 4 deviation, which stored the
/// conditional-free `#I:#W#F` plus a `status::status_spans`-side padding
/// shim because the old `expand_format` subset couldn't evaluate `#{?...}`
/// at all). Public so tests (and any future caller) can compare an
/// option's effective format against "the default" without duplicating the
/// literal string.
pub const DEFAULT_WINDOW_STATUS_FORMAT: &str = "#I:#W#{?window_flags,#{window_flags}, }";

const SPECS: &[Spec] = &[
    Spec { name: "prefix", kind: Kind::Key, choices: &[] },
    Spec { name: "base-index", kind: Kind::Number, choices: &[] },
    Spec { name: "pane-base-index", kind: Kind::Number, choices: &[] },
    Spec { name: "status", kind: Kind::Flag, choices: &[] },
    Spec { name: "status-position", kind: Kind::Choice, choices: &["top", "bottom"] },
    Spec { name: "status-interval", kind: Kind::Number, choices: &[] },
    Spec { name: "status-left", kind: Kind::Str, choices: &[] },
    Spec { name: "status-right", kind: Kind::Str, choices: &[] },
    Spec { name: "status-left-length", kind: Kind::Number, choices: &[] },
    Spec { name: "status-right-length", kind: Kind::Number, choices: &[] },
    Spec { name: "status-style", kind: Kind::Style, choices: &[] },
    Spec { name: "window-status-style", kind: Kind::Style, choices: &[] },
    Spec { name: "window-status-current-style", kind: Kind::Style, choices: &[] },
    Spec { name: "message-style", kind: Kind::Style, choices: &[] },
    Spec { name: "pane-border-style", kind: Kind::Style, choices: &[] },
    Spec { name: "pane-active-border-style", kind: Kind::Style, choices: &[] },
    // Active-pane border indication (Task 11, sub-project 6 wave 2):
    // `off`/`colour`/`arrows`/`both`, default `colour` -- see the
    // `pane_border_indicators` getter below.
    Spec { name: "pane-border-indicators", kind: Kind::Choice, choices: &["off", "colour", "arrows", "both"] },
    Spec { name: "display-time", kind: Kind::Number, choices: &[] },
    Spec { name: "repeat-time", kind: Kind::Number, choices: &[] },
    Spec { name: "default-command", kind: Kind::Str, choices: &[] },
    Spec { name: "renumber-windows", kind: Kind::Flag, choices: &[] },
    // `mouse` (Task 5, sub-project 4): now LIVE — see the `mouse()` getter's
    // doc comment. Accepted-inert options (SP4) follow below: typed,
    // validated, stored, shown; no getter beyond `show` since nothing
    // consumes them yet.
    Spec { name: "mouse", kind: Kind::Flag, choices: &[] },
    Spec { name: "history-limit", kind: Kind::Number, choices: &[] },
    Spec { name: "escape-time", kind: Kind::Number, choices: &[] },
    Spec { name: "automatic-rename", kind: Kind::Flag, choices: &[] },
    Spec { name: "allow-rename", kind: Kind::Flag, choices: &[] },
    Spec { name: "mode-keys", kind: Kind::Choice, choices: &["emacs", "vi"] },
    Spec { name: "default-terminal", kind: Kind::Str, choices: &[] },
    Spec { name: "exit-empty", kind: Kind::Flag, choices: &[] },
    Spec { name: "aggressive-resize", kind: Kind::Flag, choices: &[] },
    // Copy mode (Task 2, sub-project 4).
    Spec { name: "mode-style", kind: Kind::Style, choices: &[] },
    // Selection + paste buffers (Task 3, sub-project 4).
    Spec { name: "buffer-limit", kind: Kind::Number, choices: &[] },
    // Drag word/line selection extension (Task 7, SP6 wave 2): promoted from
    // a hardcoded constant (`" -_@"`, an inaccurate guess never verified
    // against real tmux) to a real, settable `Str` option -- see the
    // `word_separators()` getter's doc comment for the verified default.
    Spec { name: "word-separators", kind: Kind::Str, choices: &[] },
    // Layout presets (Task 6, sub-project 4): `main-horizontal`/
    // `main-vertical`'s main-pane size.
    Spec { name: "main-pane-width", kind: Kind::Number, choices: &[] },
    Spec { name: "main-pane-height", kind: Kind::Number, choices: &[] },
    // Overlays (Task 8, sub-project 4): choose-tree + display-panes.
    // `display-panes-colour`/`-active-colour` are plain BARE colours (not
    // full `fg=...`/`bg=...` style strings) -- stored as `Str` and parsed on
    // read via `style::parse_color` (see the two getters below), per the
    // design spec's `## 7. Overlays` section.
    Spec { name: "display-panes-time", kind: Kind::Number, choices: &[] },
    Spec { name: "display-panes-colour", kind: Kind::Str, choices: &[] },
    Spec { name: "display-panes-active-colour", kind: Kind::Str, choices: &[] },
    // SP6 Task 2 (config compatibility): every remaining option the user's
    // real `.tmux.conf` sets that winmux's table was missing entirely.
    // `visual-activity`/`visual-bell`/`visual-silence`/`bell-action`/
    // `monitor-activity`/`clock-mode-colour`/`window-status-bell-style` are
    // accepted-and-stored but INERT (no alerts/bell/clock-mode subsystem
    // exists yet -- same bucket as `mouse`/`history-limit` before Task 5/7
    // wired them up; see `.superpowers/sdd/sp6-gap-analysis.md` §A).
    // `status-justify`/`status-left-style`/`status-right-style`/
    // `window-status-format`/`window-status-current-format`/
    // `window-status-separator` are typed+stored here; the status-bar
    // RENDERING wiring for them (SP6 Task 4) lands in `src/status.rs`.
    Spec { name: "visual-activity", kind: Kind::Choice, choices: &["off", "on", "both"] },
    Spec { name: "visual-bell", kind: Kind::Choice, choices: &["off", "on", "both"] },
    Spec { name: "visual-silence", kind: Kind::Choice, choices: &["off", "on", "both"] },
    Spec { name: "bell-action", kind: Kind::Choice, choices: &["any", "none", "current", "other"] },
    Spec { name: "monitor-activity", kind: Kind::Flag, choices: &[] },
    // Bare colour token (like `display-panes-colour` above), not a full
    // style string -- stored as `Str`, parsed on read via `style::parse_color`.
    Spec { name: "clock-mode-colour", kind: Kind::Str, choices: &[] },
    // clock-mode (Task 10, sub-project 6 wave 2): 12-/24-hour format
    // selector. Real tmux's choice set is `12 | 24 | 12-with-seconds |
    // 24-with-seconds` (`docs/tmux-reference/status-line-and-messages.md`
    // `## 6. Clock mode`); winmux implements only the plain two, a
    // documented task-scope simplification (no seconds-resolution display).
    Spec { name: "clock-mode-style", kind: Kind::Choice, choices: &["12", "24"] },
    Spec { name: "window-status-bell-style", kind: Kind::Style, choices: &[] },
    Spec { name: "window-status-separator", kind: Kind::Str, choices: &[] },
    Spec { name: "status-justify", kind: Kind::Choice, choices: &["left", "centre", "right", "absolute-centre"] },
    Spec { name: "status-left-style", kind: Kind::Style, choices: &[] },
    Spec { name: "status-right-style", kind: Kind::Style, choices: &[] },
    Spec { name: "window-status-format", kind: Kind::Str, choices: &[] },
    Spec { name: "window-status-current-format", kind: Kind::Str, choices: &[] },
];

fn find_spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

/// Which [`Kind`] a concrete [`Value`] actually is -- used only by the
/// `specs_and_defaults_stay_in_sync` test to catch a `SPECS`/`default_value`
/// desync (a `Spec.kind` that doesn't match the `Value` variant
/// `default_value` builds for that name) at test time rather than as a
/// runtime `unreachable!` panic the first time a mismatched typed getter
/// (e.g. `str_ref` expecting `Value::Str`) is called against it.
#[cfg(test)]
fn value_kind(v: &Value) -> Kind {
    match v {
        Value::Flag(_) => Kind::Flag,
        Value::Number(_) => Kind::Number,
        Value::Key(_) => Kind::Key,
        Value::Choice(_) => Kind::Choice,
        Value::Str(_) => Kind::Str,
        Value::Style(_, _) => Kind::Style,
    }
}

/// Build the tmux-default `Value` for one option by name (must be a name
/// present in [`SPECS`]).
fn default_value(name: &str) -> Value {
    match name {
        "prefix" => Value::Key(keys::parse_key("C-b").expect("C-b parses")),
        "base-index" => Value::Number(0),
        "pane-base-index" => Value::Number(0),
        "status" => Value::Flag(true),
        "status-position" => Value::Choice("bottom"),
        "status-interval" => Value::Number(15),
        "status-left" => Value::Str("[#S] ".to_string()),
        // Deviation from tmux (documented in the design spec / contract):
        // tmux's real default embeds `#{=21:pane_title}`, which is outside
        // the SP3 format subset (would render as an empty-quoted prefix).
        // winmux's default is just the clock half, reproducing the SP2
        // status-right string exactly.
        "status-right" => Value::Str("%H:%M %d-%b-%y".to_string()),
        "status-left-length" => Value::Number(10),
        "status-right-length" => Value::Number(40),
        "status-style" => {
            let s = "bg=green,fg=black";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        "window-status-style" => Value::Style(String::new(), PartialStyle::default()),
        "window-status-current-style" => {
            let s = "underscore";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        "message-style" => {
            let s = "bg=yellow,fg=black";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        "pane-border-style" => Value::Style(String::new(), PartialStyle::default()),
        "pane-active-border-style" => {
            let s = "fg=green";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        "pane-border-indicators" => Value::Choice("colour"),
        "display-time" => Value::Number(750),
        "repeat-time" => Value::Number(500),
        "default-command" => Value::Str("powershell.exe -NoLogo".to_string()),
        "renumber-windows" => Value::Flag(false),
        "mouse" => Value::Flag(false),
        "history-limit" => Value::Number(2000),
        "escape-time" => Value::Number(500),
        "automatic-rename" => Value::Flag(true),
        "allow-rename" => Value::Flag(true),
        "mode-keys" => Value::Choice("emacs"),
        "default-terminal" => Value::Str("screen".to_string()),
        "exit-empty" => Value::Flag(true),
        "aggressive-resize" => Value::Flag(false),
        "mode-style" => {
            let s = "bg=yellow,fg=black";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        "buffer-limit" => Value::Number(50),
        // Verified against `docs/tmux-reference/copy-mode-and-buffers.md`
        // §6.4 / its options table (options-table.c:1262-1270): every
        // printable non-alphanumeric ASCII character EXCEPT underscore.
        // Plain space/tab are NOT in this string -- they're tmux's separate
        // `WHITESPACE` class (`"\t "`, tmux.h:662), handled by
        // `dispatch::char_class` as its own `CharClass::Whitespace` variant
        // rather than folded into `Separator` here.
        "word-separators" => Value::Str("!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~".to_string()),
        "main-pane-width" => Value::Number(80),
        "main-pane-height" => Value::Number(24),
        "display-panes-time" => Value::Number(1000),
        "display-panes-colour" => Value::Str("blue".to_string()),
        "display-panes-active-colour" => Value::Str("red".to_string()),
        "visual-activity" => Value::Choice("off"),
        "visual-bell" => Value::Choice("off"),
        "visual-silence" => Value::Choice("off"),
        "bell-action" => Value::Choice("any"),
        "monitor-activity" => Value::Flag(false),
        "clock-mode-colour" => Value::Str("blue".to_string()),
        // Verified against the reference doc's options appendix
        // (`## 9`/options-table.c:1342-1348): default is `24`, same as
        // classic tmux's narrower 12/24-only choice set.
        "clock-mode-style" => Value::Choice("24"),
        "window-status-bell-style" => {
            let s = "reverse";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        "window-status-separator" => Value::Str(" ".to_string()),
        "status-justify" => Value::Choice("left"),
        "status-left-style" => {
            let s = "default";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        "status-right-style" => {
            let s = "default";
            Value::Style(s.to_string(), style::parse_style(s).expect("valid default style"))
        }
        // SP7 Task 1: tmux's literal real default, verbatim (the general
        // format engine in `src/format.rs` evaluates its
        // `#{?window_flags,#{window_flags}, }` conditional correctly, so no
        // deviation/shim is needed anymore — see DEFAULT_WINDOW_STATUS_FORMAT's
        // doc comment above; closes docs/follow-ups.md #70).
        "window-status-format" => Value::Str(DEFAULT_WINDOW_STATUS_FORMAT.to_string()),
        "window-status-current-format" => Value::Str(DEFAULT_WINDOW_STATUS_FORMAT.to_string()),
        _ => unreachable!("default_value called with unknown option: {name}"),
    }
}

/// The global tmux option table. SP3 scope: one instance, no per-session/
/// window overlays (see module docs).
pub struct Options {
    values: BTreeMap<&'static str, Value>,
    /// SP6 Task 2: free-form user (`@name`) options -- tmux accepts ANY
    /// `@`-prefixed name at any scope, string-typed, no SPECS validation
    /// (`commands-config-options-formats.md` §3.4). Keyed WITHOUT the `@`
    /// (the prefix is stripped once at the `set`/`show` boundary). Starts
    /// empty: there is no "default" for a user option, only "never set".
    user_options: BTreeMap<String, String>,
}

impl Options {
    /// A fresh table populated with tmux defaults (per the design spec's
    /// option table).
    pub fn new() -> Options {
        let mut values = BTreeMap::new();
        for spec in SPECS {
            values.insert(spec.name, default_value(spec.name));
        }
        Options { values, user_options: BTreeMap::new() }
    }

    /// Set (or unset/append/toggle) one option.
    ///
    /// - `unset`: restore the option's default; `value` is ignored.
    /// - `append` (`-a`): only valid for `Str`-kind options — concatenates
    ///   `value` onto the current string. Any other kind -> `Err("bad
    ///   value: -a requires a string option")`.
    /// - `value: None` on a `Flag` option toggles it (tmux `set -g mouse`
    ///   with no value flips the current flag). On any other kind, a
    ///   missing value is `Err("bad value: <name> requires a value")`.
    /// - Unknown `name` -> `Err("unknown option: <name>")`.
    pub fn set(&mut self, name: &str, value: Option<&str>, append: bool, unset: bool) -> Result<(), String> {
        if let Some(uname) = name.strip_prefix('@') {
            return self.set_user_option(uname, value, append, unset);
        }
        let spec = find_spec(name).ok_or_else(|| format!("unknown option: {name}"))?;

        if unset {
            self.values.insert(spec.name, default_value(spec.name));
            return Ok(());
        }

        if append {
            if spec.kind != Kind::Str {
                return Err("bad value: -a requires a string option".to_string());
            }
            let addition = value.unwrap_or("");
            let mut current = match self.values.get(spec.name) {
                Some(Value::Str(s)) => s.clone(),
                _ => String::new(),
            };
            current.push_str(addition);
            // Validate the RESULT (existing + addition), not just the
            // addition alone -- an appended fragment that is clean on its
            // own could still complete a control sequence split across two
            // `set -a` calls. See the control-char rejection note on the
            // `Kind::Str` arm below.
            if has_control_chars(&current) {
                return Err(format!("bad value: {}", sanitize_control_chars(&current)));
            }
            self.values.insert(spec.name, Value::Str(current));
            return Ok(());
        }

        let value = match value {
            Some(v) => v,
            None => {
                if spec.kind == Kind::Flag {
                    let current = matches!(self.values.get(spec.name), Some(Value::Flag(true)));
                    self.values.insert(spec.name, Value::Flag(!current));
                    return Ok(());
                }
                return Err(format!("bad value: {name} requires a value"));
            }
        };

        let parsed = match spec.kind {
            Kind::Flag => Value::Flag(parse_on_off(value).ok_or_else(|| format!("bad value: {value}"))?),
            Kind::Number => Value::Number(value.parse::<u32>().map_err(|_| format!("bad value: {value}"))?),
            Kind::Key => Value::Key(keys::parse_key(value).ok_or_else(|| format!("bad value: {value}"))?),
            Kind::Choice => {
                let lower = value.to_ascii_lowercase();
                let matched = spec
                    .choices
                    .iter()
                    .find(|c| **c == lower)
                    .ok_or_else(|| format!("bad value: {value}"))?;
                Value::Choice(matched)
            }
            Kind::Str => {
                // `status-left`/`status-right` are settable at runtime by ANY
                // attached client (`:set -g status-left ...`) and the
                // composited status row goes to EVERY attached client's
                // terminal -- embedded ESC/OSC/CSI (title spoofing, OSC 52
                // clipboard) or bare \r\n could corrupt other clients'
                // terminals. Reject the same way `model::validate_name`
                // rejects control chars in session/window names: sanitize
                // control chars to `?` in the echoed error text so the
                // rejection message itself can never smuggle a control
                // sequence back to the caller's terminal. Covers
                // status-left/status-right/default-command/default-terminal
                // uniformly -- a control character in any of them is equally
                // bogus, not just the two status-bar options.
                if has_control_chars(value) {
                    return Err(format!("bad value: {}", sanitize_control_chars(value)));
                }
                Value::Str(value.to_string())
            }
            Kind::Style => {
                let parsed = style::parse_style(value)?;
                Value::Style(value.to_string(), parsed)
            }
        };
        self.values.insert(spec.name, parsed);
        Ok(())
    }

    /// `set`'s `@name` branch (SP6 Task 2): any `@`-prefixed name is a
    /// free-form user option -- string-typed, no SPECS validation, valid at
    /// any scope (`commands-config-options-formats.md` §3.4). `uname` is
    /// the name WITHOUT its `@` prefix (already stripped by the caller).
    /// Otherwise mirrors the `Str`-kind rules on the built-in path: `-u`
    /// removes it entirely (there is no default to fall back to -- an unset
    /// user option is "never set", not a stored empty string); `-a` appends
    /// onto the current value (or empty string if never set); control
    /// characters are rejected in the same way and for the same reason as
    /// every other `Str`-kind value (see the control-char rejection note on
    /// this module's docs).
    fn set_user_option(&mut self, uname: &str, value: Option<&str>, append: bool, unset: bool) -> Result<(), String> {
        if unset {
            self.user_options.remove(uname);
            return Ok(());
        }
        if append {
            let mut current = self.user_options.get(uname).cloned().unwrap_or_default();
            current.push_str(value.unwrap_or(""));
            if has_control_chars(&current) {
                return Err(format!("bad value: {}", sanitize_control_chars(&current)));
            }
            self.user_options.insert(uname.to_string(), current);
            return Ok(());
        }
        let value = value.ok_or_else(|| format!("bad value: @{uname} requires a value"))?;
        if has_control_chars(value) {
            return Err(format!("bad value: {}", sanitize_control_chars(value)));
        }
        self.user_options.insert(uname.to_string(), value.to_string());
        Ok(())
    }

    /// Format one option's current value back as tmux would print it
    /// (`show-options`/`show-options <name>`), or `None` for an unknown
    /// name. For an `@name` user option, `None` means "never set" -- the
    /// SAME signal an unknown BUILT-IN name gives here (this is the
    /// non-quiet-by-default tmux behavior: `show`'s caller, `server::
    /// dispatch::exec_show_options`, already turns a `None` into `Err("unknown
    /// option: {n}")` for ANY name with no further change needed). See
    /// [`Options::show_user_option`] for the `-q`-aware variant that
    /// distinguishes "never set" from "not a real option" with an
    /// explicit quiet flag (commands-config-options-formats.md:255).
    pub fn show(&self, name: &str) -> Option<String> {
        if let Some(uname) = name.strip_prefix('@') {
            return self.user_options.get(uname).cloned();
        }
        let value = self.values.get(name)?;
        Some(format_value(value))
    }

    /// `-q`-aware read of a user (`@name`) option, mirroring tmux's `show
    /// -gqv "@foo"` idiom -- the canonical "read a user option, empty if
    /// unset" pattern (commands-config-options-formats.md:255: "`show -gqv
    /// "@foo"` when unset: prints nothing, returns success -- the `o == NULL
    /// && *name == '@'` branch errors `invalid option: %s` only without
    /// `-q`."). `name` may be given with or without its leading `@`.
    /// `quiet`: `true` (tmux `-q`) -> an unset option is silently `Ok(None)`;
    /// `false` -> `Err("invalid option: @name")`, matching tmux's default
    /// (no `-q`) behavior. Not yet wired into `server::dispatch` (SP6 Task
    /// 2 scope is the `Options`-level semantics; CLI `-v`/`-q` flag parsing
    /// for `show-options` is future work).
    pub fn show_user_option(&self, name: &str, quiet: bool) -> Result<Option<String>, String> {
        let uname = name.strip_prefix('@').unwrap_or(name);
        match self.user_options.get(uname) {
            Some(v) => Ok(Some(v.clone())),
            None if quiet => Ok(None),
            None => Err(format!("invalid option: @{uname}")),
        }
    }

    /// All known options, one `name value` line per option, sorted by name
    /// (SP3 simplification, documented: real tmux `show-options` only lists
    /// non-default overrides unless `-g`; here `show_all` always lists
    /// every option).
    pub fn show_all(&self) -> String {
        let mut lines: Vec<String> = self
            .values
            .iter()
            .map(|(name, value)| format!("{name} {}", format_value(value)))
            .collect();
        // Only ACTUALLY-SET user options appear (there is no "default" row
        // for an `@name` the way built-in options always have one).
        for (uname, value) in &self.user_options {
            lines.push(format!("@{uname} {}", quote_if_needed(value)));
        }
        lines.sort();
        lines.join("\n")
    }

    pub fn prefix(&self) -> Key {
        match self.values.get("prefix") {
            Some(Value::Key(k)) => *k,
            _ => unreachable!("prefix is always Key"),
        }
    }

    pub fn base_index(&self) -> u32 {
        self.number("base-index")
    }

    pub fn pane_base_index(&self) -> u32 {
        self.number("pane-base-index")
    }

    pub fn status_on(&self) -> bool {
        self.flag("status")
    }

    pub fn status_position_top(&self) -> bool {
        matches!(self.values.get("status-position"), Some(Value::Choice("top")))
    }

    pub fn status_interval(&self) -> Duration {
        Duration::from_secs(self.number("status-interval") as u64)
    }

    pub fn status_left(&self) -> &str {
        self.str_ref("status-left")
    }

    pub fn status_right(&self) -> &str {
        self.str_ref("status-right")
    }

    pub fn status_left_length(&self) -> u16 {
        self.number("status-left-length") as u16
    }

    pub fn status_right_length(&self) -> u16 {
        self.number("status-right-length") as u16
    }

    pub fn status_style(&self) -> &PartialStyle {
        self.style_ref("status-style")
    }

    pub fn message_style(&self) -> &PartialStyle {
        self.style_ref("message-style")
    }

    pub fn window_status_style(&self) -> &PartialStyle {
        self.style_ref("window-status-style")
    }

    pub fn window_status_current_style(&self) -> &PartialStyle {
        self.style_ref("window-status-current-style")
    }

    pub fn pane_border_style(&self) -> &PartialStyle {
        self.style_ref("pane-border-style")
    }

    pub fn pane_active_border_style(&self) -> &PartialStyle {
        self.style_ref("pane-active-border-style")
    }

    /// `pane-border-indicators` (Task 11, sub-project 6 wave 2): how a
    /// border cell shows which pane is active -- `off` (no indication),
    /// `colour` (default; half-border cosmetic split on a 2-pane divider,
    /// general per-cell adjacency colouring otherwise), `arrows` (four
    /// glyphs on the active pane's own border, no colouring), `both` (both).
    /// See `render::BorderIndicators`, which this maps onto 1:1.
    pub fn pane_border_indicators(&self) -> crate::render::BorderIndicators {
        use crate::render::BorderIndicators;
        match self.values.get("pane-border-indicators") {
            Some(Value::Choice("off")) => BorderIndicators::Off,
            Some(Value::Choice("arrows")) => BorderIndicators::Arrows,
            Some(Value::Choice("both")) => BorderIndicators::Both,
            _ => BorderIndicators::Colour,
        }
    }

    pub fn display_time(&self) -> Duration {
        Duration::from_millis(self.number("display-time") as u64)
    }

    pub fn repeat_time(&self) -> Duration {
        Duration::from_millis(self.number("repeat-time") as u64)
    }

    pub fn default_command(&self) -> &str {
        self.str_ref("default-command")
    }

    pub fn renumber_windows(&self) -> bool {
        self.flag("renumber-windows")
    }

    pub fn history_limit(&self) -> u32 {
        self.number("history-limit")
    }

    /// `mouse` (Task 5, sub-project 4): global on/off for xterm mouse
    /// reporting. tmux default `off`. Reclassifies `mouse` from the
    /// accepted-inert group (SP4 options review) to a live, consumed option
    /// — `server::dispatch::exec_set_option` reacts to a change by
    /// broadcasting the SGR mouse-mode enable/disable escape sequences to
    /// every attached client, and `server::Server::finish_attach` sends the
    /// enable sequence to a newly-attaching client when this is already on.
    pub fn mouse(&self) -> bool {
        self.flag("mouse")
    }

    /// Copy mode's key table selector: `true` = `mode-keys vi`
    /// (`WhichTable::CopyModeVi`), `false` = the emacs default
    /// (`WhichTable::CopyMode`).
    pub fn mode_keys_vi(&self) -> bool {
        matches!(self.values.get("mode-keys"), Some(Value::Choice("vi")))
    }

    /// Copy mode's position-indicator/selection style (Task 2: only the
    /// position indicator uses this yet; selection highlighting is Task 3).
    pub fn mode_style(&self) -> &PartialStyle {
        self.style_ref("mode-style")
    }

    /// `word-separators` (Task 7, SP6 wave 2): the character class boundary
    /// double-click word selection (`select_word_at`) and continued-drag
    /// word-extension (`dispatch::move_drag_cursor`) expand to. tmux default
    /// (verified against `docs/tmux-reference/copy-mode-and-buffers.md`
    /// §6.4's options table, NOT the task brief's unverified `" -_@"`
    /// guess): every printable non-alphanumeric ASCII character except `_`
    /// -- so alphanumerics+`_` form one class ("word chars"), a run of this
    /// string's characters forms another (a run of separator punctuation is
    /// itself a selectable "word"), and plain space/tab are a third,
    /// separate whitespace class (tmux's `WHITESPACE`, never part of this
    /// option's value) -- see `dispatch::CharClass`/`char_class`.
    pub fn word_separators(&self) -> &str {
        self.str_ref("word-separators")
    }

    /// Max AUTOMATIC paste buffers (Task 3, sub-project 4) before
    /// `copy-selection-and-cancel`/bare `set-buffer` evicts the oldest;
    /// manual (`set-buffer -b`) buffers are exempt. tmux default 50.
    pub fn buffer_limit(&self) -> u32 {
        self.number("buffer-limit")
    }

    /// `main-pane-width`/`main-pane-height` (Task 6, sub-project 4): the
    /// `main-horizontal`/`main-vertical` layout presets' main-pane size,
    /// clamped at APPLICATION time by `layout::apply_preset` so the other
    /// panes never fall below `MIN_PANE_W`/`MIN_PANE_H`. tmux defaults: width
    /// 80, height 24.
    pub fn main_pane_width(&self) -> u16 {
        self.number("main-pane-width") as u16
    }

    pub fn main_pane_height(&self) -> u16 {
        self.number("main-pane-height") as u16
    }

    /// `display-panes-time` (Task 8, sub-project 4): how long the
    /// `display-panes` (`q`) overlay stays up before auto-dismissing, absent
    /// an explicit `-d ms` override on the command itself. tmux default
    /// 1000ms.
    pub fn display_panes_time(&self) -> Duration {
        Duration::from_millis(self.number("display-panes-time") as u64)
    }

    /// `display-panes-colour` (Task 8): the digit-overlay background colour
    /// for every pane EXCEPT the acting client's currently focused one. A
    /// stored value that no longer parses as a bare colour (only reachable
    /// by a future `set-option` bypassing today's `set`-time validation --
    /// there isn't one yet, `Kind::Str` accepts any control-char-free string)
    /// falls back to the compiled default rather than panicking.
    pub fn display_panes_colour(&self) -> Color {
        style::parse_color(&self.str_ref("display-panes-colour").to_ascii_lowercase()).unwrap_or(Color::Idx(4))
    }

    /// `display-panes-active-colour` (Task 8): same as
    /// [`Options::display_panes_colour`], for the focused pane's digit only.
    pub fn display_panes_active_colour(&self) -> Color {
        style::parse_color(&self.str_ref("display-panes-active-colour").to_ascii_lowercase()).unwrap_or(Color::Idx(1))
    }

    /// `escape-time` (Task 9, sub-project 4): how long a pending lone/
    /// partial ESC sequence (`input::KeyMachine::escape_ready`) may sit
    /// unresolved before the server force-flushes it as a bare `Escape` key.
    /// tmux default 500ms; reclassifies `escape-time` from the
    /// accepted-inert group to a live, consumed option (see the design
    /// spec's `## 8. escape-time` section).
    pub fn escape_time(&self) -> Duration {
        Duration::from_millis(self.number("escape-time") as u64)
    }

    /// `automatic-rename` (Task 9, sub-project 4): global on/off for
    /// tracking a window's name to its active pane's OSC title. tmux default
    /// on. ANDed with the per-window `model::Window::auto_rename` flag by
    /// `server::Server::maybe_auto_rename` — see that method's doc comment
    /// and the design spec's `## 9. automatic-rename` section.
    pub fn automatic_rename(&self) -> bool {
        self.flag("automatic-rename")
    }

    /// `visual-activity`/`visual-bell`/`visual-silence` (SP6 Task 2): how an
    /// activity/bell/silence alert is shown (`off`/`on`/`both` -- `both`
    /// means visual AND audible). Accepted-and-stored but INERT: no
    /// alerts/bell subsystem exists yet (`docs/follow-ups.md`-tracked; see
    /// `.superpowers/sdd/sp6-gap-analysis.md` §A).
    pub fn visual_activity(&self) -> &'static str {
        self.choice("visual-activity")
    }

    pub fn visual_bell(&self) -> &'static str {
        self.choice("visual-bell")
    }

    pub fn visual_silence(&self) -> &'static str {
        self.choice("visual-silence")
    }

    /// `bell-action` (SP6 Task 2): which window(s) a bell alert routes to
    /// (`any`/`none`/`current`/`other`). Accepted-and-stored but INERT, same
    /// bucket as the `visual-*` getters above.
    pub fn bell_action(&self) -> &'static str {
        self.choice("bell-action")
    }

    /// `monitor-activity` (SP6 Task 2): per-window activity monitoring
    /// on/off. Accepted-and-stored but INERT (no activity tracking exists
    /// yet).
    pub fn monitor_activity(&self) -> bool {
        self.flag("monitor-activity")
    }

    /// `clock-mode-colour` (SP6 Task 2): the big-clock overlay's colour.
    /// Accepted-and-stored but INERT (clock-mode itself does not exist).
    /// Same fallback-on-unparseable-stored-value pattern as
    /// [`Options::display_panes_colour`].
    pub fn clock_mode_colour(&self) -> Color {
        style::parse_color(&self.str_ref("clock-mode-colour").to_ascii_lowercase()).unwrap_or(Color::Idx(4))
    }

    /// `clock-mode-style` (Task 10, sub-project 6 wave 2): `true` = `12`
    /// (`%l:%M ` + `AM`/`PM`), `false` = the `24` default (`%H:%M`) -- see
    /// `server::format_clock`, which this selects between.
    pub fn clock_mode_style_12(&self) -> bool {
        matches!(self.values.get("clock-mode-style"), Some(Value::Choice("12")))
    }

    /// `window-status-bell-style` (SP6 Task 2): style for a window tab with
    /// an unseen bell. Accepted-and-stored but INERT (no bell-state tracking
    /// exists yet).
    pub fn window_status_bell_style(&self) -> &PartialStyle {
        self.style_ref("window-status-bell-style")
    }

    /// `window-status-separator` (SP6 Task 2): literal text between window
    /// tabs in the status bar. Typed+stored here; `status.rs`'s tab-join
    /// wiring is Task 4.
    pub fn window_status_separator(&self) -> &str {
        self.str_ref("window-status-separator")
    }

    /// `status-justify` (SP6 Task 2): window-list placement within the
    /// status bar (`left`/`centre`/`right`/`absolute-centre`). Typed+stored
    /// here; `status.rs`'s tab-placement wiring is Task 4.
    pub fn status_justify(&self) -> &'static str {
        self.choice("status-justify")
    }

    /// `status-left-style`/`status-right-style` (SP6 Task 2): per-side style
    /// layered over `status-style`. Typed+stored here; `status.rs`'s
    /// per-side layering wiring is Task 4.
    pub fn status_left_style(&self) -> &PartialStyle {
        self.style_ref("status-left-style")
    }

    pub fn status_right_style(&self) -> &PartialStyle {
        self.style_ref("status-right-style")
    }

    /// `window-status-format`/`window-status-current-format` (SP6 Task 2,
    /// rendering wired in Task 4): per-window tab format-string templates,
    /// expanded per window by `status::status_spans` via `expand_format`.
    /// Default is tmux's literal real string,
    /// `#I:#W#{?window_flags,#{window_flags}, }` (SP7 Task 1: see
    /// `DEFAULT_WINDOW_STATUS_FORMAT`'s doc comment).
    pub fn window_status_format(&self) -> &str {
        self.str_ref("window-status-format")
    }

    pub fn window_status_current_format(&self) -> &str {
        self.str_ref("window-status-current-format")
    }

    fn choice(&self, name: &str) -> &'static str {
        match self.values.get(name) {
            Some(Value::Choice(c)) => c,
            _ => unreachable!("{name} is always Choice"),
        }
    }

    fn number(&self, name: &str) -> u32 {
        match self.values.get(name) {
            Some(Value::Number(n)) => *n,
            _ => unreachable!("{name} is always Number"),
        }
    }

    fn flag(&self, name: &str) -> bool {
        match self.values.get(name) {
            Some(Value::Flag(b)) => *b,
            _ => unreachable!("{name} is always Flag"),
        }
    }

    fn str_ref(&self, name: &str) -> &str {
        match self.values.get(name) {
            Some(Value::Str(s)) => s.as_str(),
            _ => unreachable!("{name} is always Str"),
        }
    }

    fn style_ref(&self, name: &str) -> &PartialStyle {
        match self.values.get(name) {
            Some(Value::Style(_, s)) => s,
            _ => unreachable!("{name} is always Style"),
        }
    }
}

impl Default for Options {
    fn default() -> Options {
        Options::new()
    }
}

/// True if `s` contains any control character (mirrors
/// `model::validate_name`'s rule via `char::is_control`).
fn has_control_chars(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

/// Replace every control character with `?` for safe echo in an error
/// message (mirrors `model::validate_name`'s sanitized-echo approach) --
/// never echo raw ESC/OSC/CSI bytes back to a client's terminal.
fn sanitize_control_chars(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

fn parse_on_off(s: &str) -> Option<bool> {
    match s {
        "on" | "1" => Some(true),
        "off" | "0" => Some(false),
        _ => None,
    }
}

/// Render one stored value back in tmux `show-options` style: flags as
/// `on`/`off`, numbers as bare digits, the key option via [`keys::key_name`],
/// choices as their bare word, and strings quoted with `""` when they
/// contain a space or are empty (tmux quotes `status-left "[#S] "` because
/// of the trailing space; an unquoted bare word like `powershell.exe` prints
/// as-is). Styles print their ORIGINAL source string (round-trips the
/// user's own text; defaults print their canonical default strings above),
/// quoted by the same space/empty rule.
fn format_value(value: &Value) -> String {
    match value {
        Value::Flag(b) => if *b { "on" } else { "off" }.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Key(k) => keys::key_name(k),
        Value::Choice(c) => c.to_string(),
        Value::Str(s) => quote_if_needed(s),
        Value::Style(src, _) => quote_if_needed(src),
    }
}

fn quote_if_needed(s: &str) -> String {
    if s.is_empty() || s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// Re-exported from [`crate::format`] for source compatibility (SP7 Task 1:
/// the general format engine moved out of this module — see that module's
/// docs for the full grammar). [`expand_format`] below is now a one-line
/// delegate to [`crate::format::expand`].
pub use crate::format::{FormatCtx, SystemTimeParts};

/// Expand a tmux format string against `ctx` — thin delegate to
/// [`crate::format::expand`] (SP7 Task 1). Kept here, under its original
/// name, so every existing caller (`status::status_spans`,
/// `server::render_one`, `server::dispatch`) needs no import changes. See
/// `crate::format`'s module docs for the full supported grammar (braced
/// variables, `#{?cond,true,false}` conditionals, `==`/`!=`/`<`/`>`/`<=`/
/// `>=` string comparisons, `&&`/`||`, `#{=N:x}`/`#{=-N:x}` length limits,
/// single-char aliases, `##`/`#,`/`#}` escapes, `#[...]` style-marker
/// passthrough, `%`-strftime passthrough) and its documented non-supported
/// remainder.
pub fn expand_format(fmt: &str, ctx: &FormatCtx) -> String {
    crate::format::expand(fmt, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyCode;

    fn ctx<'a>(session: &'a str, now: SystemTimeParts) -> FormatCtx<'a> {
        FormatCtx {
            session,
            window_index: 1,
            window_name: "bash",
            window_flags: "*",
            pane_index: 0,
            hostname: "HOST",
            now,
            pane_title: "",
        }
    }

    fn sample_time() -> SystemTimeParts {
        // Tue 2026-07-07 14:05:09, matching the SP2 clock spot-check below.
        SystemTimeParts { year: 2026, month: 7, day: 7, weekday: 2, hour: 14, min: 5, sec: 9 }
    }

    // ---- defaults ----

    #[test]
    fn defaults_match_tmux() {
        let o = Options::new();
        assert_eq!(o.prefix(), Key { code: KeyCode::Char('b'), ctrl: true, meta: false, shift: false });
        assert_eq!(o.status_left(), "[#S] ");
        assert_eq!(o.repeat_time(), Duration::from_millis(500));
        assert_eq!(o.display_time(), Duration::from_millis(750));
        let msg = o.message_style();
        assert_eq!(msg.apply_to(crate::grid::Style::default()).fg, crate::grid::Color::Idx(0));
        assert_eq!(msg.apply_to(crate::grid::Style::default()).bg, crate::grid::Color::Idx(3));
    }

    // ---- set: typed options ----

    #[test]
    fn set_prefix_key() {
        let mut o = Options::new();
        o.set("prefix", Some("C-a"), false, false).unwrap();
        assert_eq!(o.prefix(), keys::parse_key("C-a").unwrap());
        assert_eq!(
            o.set("prefix", Some("NotAKey!!"), false, false),
            Err("bad value: NotAKey!!".to_string())
        );
    }

    #[test]
    fn set_style_validates() {
        let mut o = Options::new();
        assert_eq!(
            o.set("status-style", Some("fg=zzz"), false, false),
            Err("bad style: fg=zzz".to_string())
        );
    }

    #[test]
    fn append_string() {
        let mut o = Options::new();
        o.set("status-right", Some(" x"), true, false).unwrap();
        assert_eq!(o.status_right(), "%H:%M %d-%b-%y x");
        // append on a non-string option is rejected
        let mut o2 = Options::new();
        assert_eq!(
            o2.set("base-index", Some("1"), true, false),
            Err("bad value: -a requires a string option".to_string())
        );
    }

    /// `status-left`/`status-right` are settable at runtime by ANY attached
    /// client, and the composited status row is written to EVERY attached
    /// client's terminal -- a control character (ESC/OSC/CSI for title
    /// spoofing or OSC 52 clipboard injection, bare \r\n for line
    /// corruption) must be rejected rather than stored. Covers `set` on a
    /// plain `Str` value and `-a` append (validated against the appended
    /// RESULT, not just the addition in isolation).
    #[test]
    fn str_options_reject_control_chars() {
        let mut o = Options::new();
        assert_eq!(
            o.set("status-left", Some("a\x1bb"), false, false),
            Err("bad value: a?b".to_string())
        );
        // Rejected -- value left at its default, untouched.
        assert_eq!(o.status_left(), "[#S] ");

        // Append producing a CLEAN result is fine.
        o.set("status-right", Some(" clean"), true, false).unwrap();
        assert_eq!(o.status_right(), "%H:%M %d-%b-%y clean");

        // Append whose RESULT contains a control char is rejected, echoing
        // the sanitized RESULT (existing + addition), and the option is left
        // at its pre-append value.
        let before = o.status_right().to_string();
        assert_eq!(
            o.set("status-right", Some("\r\ninjected"), true, false),
            Err(format!("bad value: {before}??injected"))
        );
        assert_eq!(o.status_right(), before);

        // Same rule applies uniformly to every Str-kind option, not just the
        // two status-bar ones.
        assert_eq!(
            o.set("default-command", Some("evil\x1b]0;pwned\x07"), false, false),
            Err("bad value: evil?]0;pwned?".to_string())
        );
        assert_eq!(
            o.set("default-terminal", Some("scr\teen"), false, false),
            Err("bad value: scr?een".to_string())
        );
    }

    #[test]
    fn mouse_defaults_off_and_toggles() {
        let mut o = Options::new();
        assert!(!o.mouse(), "tmux default: mouse off");
        o.set("mouse", Some("on"), false, false).unwrap();
        assert!(o.mouse());
        o.set("mouse", None, false, false).unwrap(); // no value on a Flag toggles
        assert!(!o.mouse());
    }

    #[test]
    fn unset_restores_default() {
        let mut o = Options::new();
        o.set("base-index", Some("5"), false, false).unwrap();
        assert_eq!(o.base_index(), 5);
        o.set("base-index", None, false, true).unwrap();
        assert_eq!(o.base_index(), 0);
    }

    #[test]
    fn on_off_parsing() {
        let mut o = Options::new();
        o.set("status", Some("off"), false, false).unwrap();
        assert!(!o.status_on());
        o.set("status", Some("1"), false, false).unwrap();
        assert!(o.status_on());
        o.set("status", Some("0"), false, false).unwrap();
        assert!(!o.status_on());
        o.set("status", Some("on"), false, false).unwrap();
        assert!(o.status_on());
        assert_eq!(o.set("status", Some("sure"), false, false), Err("bad value: sure".to_string()));
    }

    #[test]
    fn flag_toggle_on_missing_value() {
        let mut o = Options::new();
        assert!(!o.renumber_windows()); // default off
        o.set("renumber-windows", None, false, false).unwrap();
        assert!(o.renumber_windows());
        o.set("renumber-windows", None, false, false).unwrap();
        assert!(!o.renumber_windows());
    }

    #[test]
    fn number_parsing() {
        let mut o = Options::new();
        o.set("base-index", Some("3"), false, false).unwrap();
        assert_eq!(o.base_index(), 3);
        assert_eq!(o.set("base-index", Some("nope"), false, false), Err("bad value: nope".to_string()));
        assert_eq!(o.set("base-index", None, false, false), Err("bad value: base-index requires a value".to_string()));
    }

    #[test]
    fn choice_parsing() {
        let mut o = Options::new();
        o.set("status-position", Some("top"), false, false).unwrap();
        assert!(o.status_position_top());
        assert_eq!(
            o.set("status-position", Some("middle"), false, false),
            Err("bad value: middle".to_string())
        );
    }

    #[test]
    fn unknown_option_err_exact() {
        let mut o = Options::new();
        assert_eq!(
            o.set("not-a-real-option", Some("x"), false, false),
            Err("unknown option: not-a-real-option".to_string())
        );
        assert_eq!(o.show("not-a-real-option"), None);
    }

    /// `SPECS` and `default_value` are two independent string-keyed tables;
    /// a desync between them (a `Spec.kind` that doesn't match the `Value`
    /// variant `default_value` actually builds for that name) is a
    /// server-wide panic waiting to happen the first time a typed getter is
    /// called against the mismatched option (every getter -- `number`,
    /// `flag`, `str_ref`, `style_ref` -- `unreachable!`s if the stored
    /// `Value` isn't the variant it expects). `Options::new()` iterating
    /// `SPECS` already guarantees every `SPECS` name HAS a `default_value`
    /// entry (a missing one is its own `unreachable!` in `default_value`);
    /// this test closes the other half -- that the entry's KIND agrees.
    #[test]
    fn specs_and_defaults_stay_in_sync() {
        for spec in SPECS {
            let kind = value_kind(&default_value(spec.name));
            assert_eq!(
                kind, spec.kind,
                "default_value(\"{}\") returned a {:?}-kind value but SPECS declares {:?}",
                spec.name, kind, spec.kind
            );
        }
    }

    /// Task 10 (clock-mode): `clock-mode-style`'s default and `set`
    /// round-trip. `Choice`-kind validation rejects an out-of-set value
    /// (`12-with-seconds` is a real tmux choice this task deliberately
    /// doesn't implement -- see the `SPECS` entry's doc comment).
    #[test]
    fn clock_mode_style_getter_and_roundtrip() {
        let mut o = Options::new();
        assert!(!o.clock_mode_style_12());
        o.set("clock-mode-style", Some("12"), false, false).unwrap();
        assert!(o.clock_mode_style_12());
        o.set("clock-mode-style", Some("24"), false, false).unwrap();
        assert!(!o.clock_mode_style_12());
        assert!(o.set("clock-mode-style", Some("12-with-seconds"), false, false).is_err());
    }

    #[test]
    fn copy_mode_getters() {
        let mut o = Options::new();
        assert!(!o.mode_keys_vi());
        let ms = o.mode_style();
        assert_eq!(ms.apply_to(crate::grid::Style::default()).fg, crate::grid::Color::Idx(0));
        assert_eq!(ms.apply_to(crate::grid::Style::default()).bg, crate::grid::Color::Idx(3));
        o.set("mode-keys", Some("vi"), false, false).unwrap();
        assert!(o.mode_keys_vi());
    }

    #[test]
    fn main_pane_size_getters() {
        let mut o = Options::new();
        assert_eq!(o.main_pane_width(), 80);
        assert_eq!(o.main_pane_height(), 24);
        o.set("main-pane-width", Some("30"), false, false).unwrap();
        o.set("main-pane-height", Some("10"), false, false).unwrap();
        assert_eq!(o.main_pane_width(), 30);
        assert_eq!(o.main_pane_height(), 10);
    }

    /// Task 8, sub-project 4: `display-panes-time` (Duration getter) and the
    /// two bare-colour getters, defaults AND after a `set-option` round trip.
    #[test]
    fn display_panes_getters() {
        let mut o = Options::new();
        assert_eq!(o.display_panes_time(), Duration::from_millis(1000));
        assert_eq!(o.display_panes_colour(), Color::Idx(4)); // blue
        assert_eq!(o.display_panes_active_colour(), Color::Idx(1)); // red

        o.set("display-panes-time", Some("200"), false, false).unwrap();
        o.set("display-panes-colour", Some("green"), false, false).unwrap();
        o.set("display-panes-active-colour", Some("colour208"), false, false).unwrap();
        assert_eq!(o.display_panes_time(), Duration::from_millis(200));
        assert_eq!(o.display_panes_colour(), Color::Idx(2));
        assert_eq!(o.display_panes_active_colour(), Color::Idx(208));
    }

    #[test]
    fn buffer_limit_getter() {
        let mut o = Options::new();
        assert_eq!(o.buffer_limit(), 50);
        o.set("buffer-limit", Some("10"), false, false).unwrap();
        assert_eq!(o.buffer_limit(), 10);
    }

    /// SP6 Task 2: `set -g @yank_action 'copy-pipe'` stores; `show`
    /// retrieves it; `-u` clears it back to "never set" (no default to fall
    /// back to). `show_user_option`'s `-q` semantics (doc citation:
    /// commands-config-options-formats.md:255): a never-set user option is
    /// silent (`Ok(None)`) when quiet, `Err("invalid option: ...")` when not.
    #[test]
    fn user_option_set_show_roundtrip() {
        let mut o = Options::new();
        // Unknown @-name before it's ever set: same "unknown" signal a
        // built-in unknown name gives (`show` returns `None`).
        assert_eq!(o.show("@yank_action"), None);

        o.set("@yank_action", Some("copy-pipe"), false, false).unwrap();
        assert_eq!(o.show("@yank_action"), Some("copy-pipe".to_string()));
        assert_eq!(o.show_user_option("@yank_action", false), Ok(Some("copy-pipe".to_string())));
        assert_eq!(o.show_user_option("yank_action", false), Ok(Some("copy-pipe".to_string())), "leading @ is optional");

        // Append.
        o.set("@yank_action", Some("-and-cancel"), true, false).unwrap();
        assert_eq!(o.show("@yank_action"), Some("copy-pipe-and-cancel".to_string()));

        // A DIFFERENT, never-set user option: quiet read is silent;
        // non-quiet read errors like tmux.
        assert_eq!(o.show_user_option("@unset_thing", true), Ok(None));
        assert_eq!(o.show_user_option("@unset_thing", false), Err("invalid option: @unset_thing".to_string()));

        // Control chars rejected, same rule as every other Str-kind value.
        assert_eq!(
            o.set("@evil", Some("a\x1bb"), false, false),
            Err("bad value: a?b".to_string())
        );

        // `-u` clears it back to "never set" -- NOT an empty string.
        o.set("@yank_action", None, false, true).unwrap();
        assert_eq!(o.show("@yank_action"), None);
        assert_eq!(o.show_user_option("@yank_action", true), Ok(None));
    }

    /// SP6 Task 2: every option added to close the user's real `.tmux.conf`
    /// gap list -- defaults verified against
    /// `docs/tmux-reference/commands-config-options-formats.md`'s options
    /// appendix, plus a basic set/get round trip for each.
    #[test]
    fn sp6_config_compat_options_defaults_and_roundtrip() {
        let mut o = Options::new();
        assert_eq!(o.visual_activity(), "off");
        assert_eq!(o.visual_bell(), "off");
        assert_eq!(o.visual_silence(), "off");
        assert_eq!(o.bell_action(), "any");
        assert!(!o.monitor_activity());
        assert_eq!(o.clock_mode_colour(), Color::Idx(4)); // blue
        let bell_style = o.window_status_bell_style().apply_to(crate::grid::Style::default());
        assert!(bell_style.reverse);
        assert_eq!(o.window_status_separator(), " ");
        assert_eq!(o.status_justify(), "left");
        // `status-left-style`/`status-right-style` default to the literal
        // tmux "default" style term -- a no-op that leaves the base cell
        // untouched (style.rs's `default_term_resets_everything`).
        let base = crate::grid::Style { fg: Color::Idx(3), ..crate::grid::Style::default() };
        assert_eq!(o.status_left_style().apply_to(base), base);
        assert_eq!(o.status_right_style().apply_to(base), base);
        // SP7 Task 1: tmux's literal real default, now expressible by the
        // general format engine's `#{?cond,a,b}` conditional support
        // (closes follow-ups #27/#70 -- see `DEFAULT_WINDOW_STATUS_FORMAT`'s
        // doc comment).
        assert_eq!(o.window_status_format(), crate::options::DEFAULT_WINDOW_STATUS_FORMAT);
        assert_eq!(o.window_status_current_format(), crate::options::DEFAULT_WINDOW_STATUS_FORMAT);

        o.set("visual-activity", Some("both"), false, false).unwrap();
        assert_eq!(o.visual_activity(), "both");
        o.set("bell-action", Some("current"), false, false).unwrap();
        assert_eq!(o.bell_action(), "current");
        o.set("monitor-activity", Some("on"), false, false).unwrap();
        assert!(o.monitor_activity());
        o.set("clock-mode-colour", Some("green"), false, false).unwrap();
        assert_eq!(o.clock_mode_colour(), Color::Idx(2));
        assert!(!o.clock_mode_style_12()); // default 24
        o.set("window-status-separator", Some("|"), false, false).unwrap();
        assert_eq!(o.window_status_separator(), "|");
        o.set("status-justify", Some("centre"), false, false).unwrap();
        assert_eq!(o.status_justify(), "centre");
        o.set("status-right-style", Some("fg=white bg=black"), false, false).unwrap();
        let applied = o.status_right_style().apply_to(crate::grid::Style::default());
        assert_eq!(applied.fg, Color::Idx(7));
        assert_eq!(applied.bg, Color::Idx(0));
    }

    #[test]
    fn accepted_inert_options_store() {
        let mut o = Options::new();
        o.set("mouse", Some("on"), false, false).unwrap();
        assert_eq!(o.show("mouse"), Some("on".to_string()));
        o.set("history-limit", Some("5000"), false, false).unwrap();
        assert_eq!(o.show("history-limit"), Some("5000".to_string()));
        o.set("mode-keys", Some("vi"), false, false).unwrap();
        assert_eq!(o.show("mode-keys"), Some("vi".to_string()));
        assert_eq!(o.set("mode-keys", Some("nope"), false, false), Err("bad value: nope".to_string()));
    }

    // ---- show / show_all ----

    #[test]
    fn show_quotes_when_needed() {
        let o = Options::new();
        // "[#S] " contains spaces -> quoted
        assert_eq!(o.show("status-left"), Some("\"[#S] \"".to_string()));
        // bare word, no space -> unquoted
        assert_eq!(o.show("default-command"), Some("\"powershell.exe -NoLogo\"".to_string()));
        assert_eq!(o.show("base-index"), Some("0".to_string()));
        assert_eq!(o.show("prefix"), Some("C-b".to_string()));
        // no spaces in this style string -> unquoted (valid style syntax
        // never contains internal whitespace, so style values are never
        // quoted in practice — the same `quote_if_needed` rule still
        // applies uniformly).
        assert_eq!(o.show("status-style"), Some("bg=green,fg=black".to_string()));
    }

    #[test]
    fn show_all_sorted() {
        let o = Options::new();
        let all = o.show_all();
        let names: Vec<&str> = all.lines().map(|l| l.split(' ').next().unwrap()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), SPECS.len());
    }

    // ---- format subset ----

    #[test]
    fn expand_basic() {
        let t = sample_time();
        let c = ctx("work", t);
        assert_eq!(expand_format("[#S] #I:#W#F", &c), "[work] 1:bash*");
        assert_eq!(expand_format("#{session_name}/#{window_index}/#{window_name}", &c), "work/1/bash");
        assert_eq!(expand_format("#P@#H", &c), "0@HOST");
    }

    #[test]
    fn expand_hash_escape() {
        let c = ctx("s", sample_time());
        assert_eq!(expand_format("before ## after", &c), "before # after");
        assert_eq!(expand_format("###", &c), "#" /* ## -> #, then lone # dropped */);
    }

    #[test]
    fn expand_unknown_long_empty() {
        let c = ctx("s", sample_time());
        assert_eq!(expand_format("<#{nonsense}>", &c), "<>");
        assert_eq!(expand_format("<#Z>", &c), "<>");
    }

    #[test]
    fn expand_pane_title() {
        // #T / #{pane_title} (Task 9, sub-project 4).
        let mut c = ctx("s", sample_time());
        c.pane_title = "vim";
        assert_eq!(expand_format("#T", &c), "vim");
        assert_eq!(expand_format("<#{pane_title}>", &c), "<vim>");
        // Empty (never-titled pane) is the common case -- expands to nothing,
        // same as an unset `#{...}` form, not a literal placeholder.
        let empty = ctx("s", sample_time());
        assert_eq!(expand_format("#T", &empty), "");
    }

    /// SP6 Task 4: `#[...]` inline style markers are text-substitution's
    /// business only in that they must NOT be interpreted/eaten here --
    /// they pass through byte-for-byte so `status::styled_runs` can parse
    /// them afterward into styled spans. An unterminated marker (no closing
    /// `]`) is copied verbatim to end-of-string rather than dropped.
    #[test]
    fn expand_inline_style_marker_passthrough() {
        let c = ctx("s", sample_time());
        assert_eq!(expand_format("#I #[fg=white]#W", &c), "1 #[fg=white]bash");
        assert_eq!(expand_format("#[fg=white", &c), "#[fg=white");
    }

    #[test]
    fn expand_strftime() {
        let c = ctx("s", sample_time());
        // reproduces SP2's `local_clock()` format exactly for this ctx
        assert_eq!(expand_format("%H:%M %d-%b-%y", &c), "14:05 07-Jul-26");
        assert_eq!(expand_format("%Y-%m-%d %a %I:%M%p", &c), "2026-07-07 Tue 02:05PM");
        assert_eq!(expand_format("%%", &c), "%");
        assert_eq!(expand_format("%x stays", &c), "%x stays");
    }
}
