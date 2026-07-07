//! Typed tmux option registry + the SP3 `#`/`%` format-string subset.
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
//! can only ever expand to a clean result.
//!
//! [`expand_format`] evaluates the SP3 format-string subset (`#S`, `#I`,
//! `#{session_name}`, `%H:%M`, ...) used by `status-left`/`status-right`/
//! `display-message`.

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
        _ => unreachable!("default_value called with unknown option: {name}"),
    }
}

/// The global tmux option table. SP3 scope: one instance, no per-session/
/// window overlays (see module docs).
pub struct Options {
    values: BTreeMap<&'static str, Value>,
}

impl Options {
    /// A fresh table populated with tmux defaults (per the design spec's
    /// option table).
    pub fn new() -> Options {
        let mut values = BTreeMap::new();
        for spec in SPECS {
            values.insert(spec.name, default_value(spec.name));
        }
        Options { values }
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

    /// Format one option's current value back as tmux would print it
    /// (`show-options`/`show-options <name>`), or `None` for an unknown
    /// name.
    pub fn show(&self, name: &str) -> Option<String> {
        let value = self.values.get(name)?;
        Some(format_value(value))
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

    /// Max AUTOMATIC paste buffers (Task 3, sub-project 4) before
    /// `copy-selection-and-cancel`/bare `set-buffer` evicts the oldest;
    /// manual (`set-buffer -b`) buffers are exempt. tmux default 50.
    pub fn buffer_limit(&self) -> u32 {
        self.number("buffer-limit")
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

/// Plain calendar/time facts for [`expand_format`]'s strftime subset. A
/// plain struct (no Windows types) so the module stays pure/testable; the
/// server fills one in from `GetLocalTime` (see `src/server.rs`'s
/// `local_clock`, which this format subset is designed to reproduce for
/// `%H:%M %d-%b-%y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemTimeParts {
    pub year: i32,
    /// 1-12.
    pub month: u8,
    pub day: u8,
    /// 0 = Sunday, matching Win32 `SYSTEMTIME.wDayOfWeek`.
    pub weekday: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
}

/// Everything [`expand_format`] needs beyond the format string itself.
pub struct FormatCtx<'a> {
    pub session: &'a str,
    pub window_index: u32,
    pub window_name: &'a str,
    pub window_flags: &'a str,
    pub pane_index: u32,
    pub hostname: &'a str,
    pub now: SystemTimeParts,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Expand the SP3 format-string subset used by `status-left`/`status-right`/
/// `display-message`:
///
/// - `#S` session name, `#I` window index, `#W` window name, `#F` window
///   flags, `#P` pane index, `#H` hostname, `##` literal `#`.
/// - `#{session_name}`, `#{window_index}`, `#{window_name}` (long forms of
///   the three most common `#`-codes); any other `#{...}` -> empty
///   (documented SP3 simplification — the full tmux format-expression
///   engine, conditionals, modifiers, etc. is out of scope until SP4).
/// - Any other `#<c>` (unrecognized short code) -> empty.
/// - `%`-strftime subset: `%H %M %S %d %m %Y %y %b %a %p %I %%`. Any other
///   `%<c>` is left as a literal two-character passthrough (`%x` stays
///   `%x`) rather than expanding or erroring.
pub fn expand_format(fmt: &str, ctx: &FormatCtx) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '#' => match chars.peek().copied() {
                Some('#') => {
                    chars.next();
                    out.push('#');
                }
                Some('S') => {
                    chars.next();
                    out.push_str(ctx.session);
                }
                Some('I') => {
                    chars.next();
                    out.push_str(&ctx.window_index.to_string());
                }
                Some('W') => {
                    chars.next();
                    out.push_str(ctx.window_name);
                }
                Some('F') => {
                    chars.next();
                    out.push_str(ctx.window_flags);
                }
                Some('P') => {
                    chars.next();
                    out.push_str(&ctx.pane_index.to_string());
                }
                Some('H') => {
                    chars.next();
                    out.push_str(ctx.hostname);
                }
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for c2 in chars.by_ref() {
                        if c2 == '}' {
                            closed = true;
                            break;
                        }
                        name.push(c2);
                    }
                    if closed {
                        match name.as_str() {
                            "session_name" => out.push_str(ctx.session),
                            "window_index" => out.push_str(&ctx.window_index.to_string()),
                            "window_name" => out.push_str(ctx.window_name),
                            _ => {} // unknown long form -> empty
                        }
                    }
                    // an unterminated `#{...` with no closing `}` is simply
                    // consumed to end-of-string, producing empty — no input
                    // is well-formed .tmux.conf either way.
                }
                Some(_) => {
                    chars.next(); // unrecognized short code -> empty
                }
                None => {} // trailing lone `#` -> dropped
            },
            '%' => match chars.peek().copied() {
                Some('%') => {
                    chars.next();
                    out.push('%');
                }
                Some('H') => {
                    chars.next();
                    out.push_str(&format!("{:02}", ctx.now.hour));
                }
                Some('M') => {
                    chars.next();
                    out.push_str(&format!("{:02}", ctx.now.min));
                }
                Some('S') => {
                    chars.next();
                    out.push_str(&format!("{:02}", ctx.now.sec));
                }
                Some('d') => {
                    chars.next();
                    out.push_str(&format!("{:02}", ctx.now.day));
                }
                Some('m') => {
                    chars.next();
                    out.push_str(&format!("{:02}", ctx.now.month));
                }
                Some('Y') => {
                    chars.next();
                    out.push_str(&ctx.now.year.to_string());
                }
                Some('y') => {
                    chars.next();
                    out.push_str(&format!("{:02}", ctx.now.year.rem_euclid(100)));
                }
                Some('b') => {
                    chars.next();
                    out.push_str(month_name(ctx.now.month));
                }
                Some('a') => {
                    chars.next();
                    out.push_str(weekday_name(ctx.now.weekday));
                }
                Some('p') => {
                    chars.next();
                    out.push_str(if ctx.now.hour < 12 { "AM" } else { "PM" });
                }
                Some('I') => {
                    chars.next();
                    let h12 = ctx.now.hour % 12;
                    out.push_str(&format!("{:02}", if h12 == 0 { 12 } else { h12 }));
                }
                Some(other) => {
                    // unrecognized strftime code -> literal passthrough
                    out.push('%');
                    out.push(other);
                    chars.next();
                }
                None => out.push('%'), // trailing lone `%`
            },
            _ => out.push(c),
        }
    }
    out
}

fn month_name(m: u8) -> &'static str {
    MONTHS[(m.clamp(1, 12) as usize) - 1]
}

fn weekday_name(w: u8) -> &'static str {
    WEEKDAYS[(w % 7) as usize]
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
    fn buffer_limit_getter() {
        let mut o = Options::new();
        assert_eq!(o.buffer_limit(), 50);
        o.set("buffer-limit", Some("10"), false, false).unwrap();
        assert_eq!(o.buffer_limit(), 10);
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
        assert_eq!(expand_format("<#{pane_title}>", &c), "<>");
        assert_eq!(expand_format("<#Z>", &c), "<>");
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
