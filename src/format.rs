//! The general tmux format-expansion engine (SP7 Task 1, closes follow-ups
//! #27 and #70).
//!
//! Pure module: no I/O, `std` only, and no dependency on any other winmux
//! module in non-test code (the `#[cfg(test)]` module below reads
//! `crate::options::DEFAULT_WINDOW_STATUS_FORMAT` as a fixture constant for
//! one acceptance test — that's the only place this crate's other modules
//! are referenced). Replaces the fixed subset that used to
//! live in `src/options.rs` (`#S`/`#I`/three long-form variables/strftime
//! only, no conditionals or modifiers) with a recursive-descent expander
//! that implements the *documented core* of real tmux's format grammar —
//! see `docs/tmux-reference/commands-config-options-formats.md`'s `## 5.
//! Formats (format.c)` section, which this module's doc comments cite by
//! subsection.
//!
//! `options::expand_format`/`options::FormatCtx`/`options::SystemTimeParts`
//! now re-export this module's [`expand`]/[`FormatCtx`]/[`SystemTimeParts`]
//! for source compatibility — every existing caller (`status::status_spans`,
//! `server::render_one`, `server::dispatch`) keeps importing from
//! `crate::options` unchanged; only `options.rs`'s own `expand_format`
//! becomes a one-line delegate.
//!
//! ## Supported grammar (§5.1-§5.4 of the reference doc)
//!
//! - `#S #W #I #P #F #H #T` single-char aliases, `##`/`#,`/`#}` literal
//!   escapes (§5.1's `##`/`#,`/`#}` bullet — the SP3 engine only had `##`;
//!   `#,`/`#}` are new here, needed so a conditional arm can embed a literal
//!   comma/close-brace, e.g. `#{?cond,a#,b,c}`).
//! - `#[...]` inline style markers, passed through byte-for-byte (unchanged
//!   from the SP3/SP6 behavior — `status::styled_runs` parses these
//!   afterward, this module does text substitution only).
//! - `#{variable}` braced long-form variables — every field on [`FormatCtx`]
//!   plus the two derived-not-stored ones, `host` (alias for `hostname`) and
//!   `host_short` (leading dot-component of `hostname`). Unknown plain names
//!   expand to empty (§5.3, "unknown plain names expand to empty string").
//! - `#{?cond,true,false}` conditionals (§5.2), including chained pairs
//!   (`#{?c1,v1,c2,v2,fallback}`) and nested `#{...}`/`#,` inside any arm.
//!   `cond` truthiness: looked up FIRST as a direct variable name; if that
//!   fails, `cond` itself is format-expanded and, if the expansion changed
//!   nothing, treated as false (§5.2's exact two-step rule — this is what
//!   lets a bare variable name like `window_flags` work as `cond` without
//!   double-expanding it, while `#{==:...}`-shaped conditions still resolve
//!   through the normal recursive path).
//! - String comparisons `#{==:a,b}` `#{!=:a,b}` `#{<:a,b}` `#{>:a,b}`
//!   `#{<=:a,b}` `#{>=:a,b}` -> `"1"`/`"0"` (§5.3's comparison row lists all
//!   six as the same `OP:a,b` shape; the task brief's own examples only
//!   named `==`/`!=`, but implementing the other four costs nothing extra —
//!   same dispatch, same two-argument parse — so all six are in scope here;
//!   flagged in the task report as a deliberate doc-over-brief scope call).
//!   Both sides are format-expanded before comparing (§5.3).
//! - N-ary boolean `#{&&:a,b,...}` / `#{||:a,b,...}` (§5.3) — each
//!   comma-separated operand is evaluated with the SAME direct-lookup-else-
//!   expand truthiness rule as a conditional's `cond`.
//! - Length-limit modifier `#{=N:x}` (truncate to the left N chars) /
//!   `#{=-N:x}` (truncate to the right N chars) — no marker-string variant
//!   (`#{=/N/marker:x}`, only appended when truncation actually happened) is
//!   implemented; that's part of the documented non-supported remainder
//!   below. This is what makes tmux's REAL `status-right` default
//!   (`#{=21:pane_title}`) and REAL `window-status-format` default
//!   (`#I:#W#{?window_flags,#{window_flags}, }`) expand correctly for the
//!   first time (closes follow-ups #27/#70 — see `options::
//!   DEFAULT_WINDOW_STATUS_FORMAT`, which is now the literal tmux string).
//! - `%`-strftime passthrough, unchanged from the SP3 engine: `%H %M %S %d
//!   %m %Y %y %b %a %p %I %%`; any other `%<c>` is literal passthrough.
//! - Recursion/loop limit (§5.1's `FORMAT_LOOP_LIMIT` = 100, reference doc
//!   line ~332): expansion is capped at 100 levels of recursion (conditional
//!   nesting, `#{...}`-in-`#{...}` fallback expansion, and `eval_truth`'s
//!   own re-expand all count); past that depth, [`expand`] stops expanding
//!   and returns the remaining input text literally instead of recursing
//!   further, so a pathological/malicious format string degrades safely
//!   (truncated/partially-unexpanded output) instead of overflowing the
//!   stack.
//!
//! ## Documented non-supported remainder (§5.3's modifier table)
//!
//! Everything else `format_build_modifiers` documents is out of this task's
//! scope (no real winmux caller needs it yet — a future task can add these
//! incrementally without touching this module's locked public surface):
//! `b:`/`d:` (basename/dirname), `t:` (Unix-time formatting), `p`/`pN:`
//! (pad), `l:` (literal/no-expand), `E:`/`T:` (re-expand / re-expand+
//! strftime), `S:`/`W:`/`P:`/`L:` (loop over sessions/windows/panes/
//! clients), `O:`/`V:` (loop over options/environment), `N:` (window/session
//! existence check), `C:` (pane-content search), `s/pat/repl/` (regex
//! substitution), `m/pat/str/` (glob/regex match), `q:` (shell-quote),
//! `n:`/`w:` (length/display-width), `a:` (ASCII-code-to-char), `c:`
//! (colour-name-to-hex/SGR), `I/...` (client termcap/feature/environ
//! lookup), `R:` (string repeat), `e|op|f|prec:` (arithmetic), `!:`/`!!:`
//! (boolean NOT). `#(command)` shell jobs are also unsupported (no shell-job
//! subsystem exists in winmux at all, a pre-existing, much larger gap than
//! this task's scope). `client_*`/loop-injected/copy-mode/mouse/buffer
//! format variables are unsupported because [`FormatCtx`] carries none of
//! that data yet — adding them would require plumbing new fields through
//! `server::dispatch`'s two `FormatCtx` construction sites, which are
//! outside this task's file-scope restriction (only `server.rs`'s call
//! sites, not `server/dispatch.rs`, are in scope for this track).

/// Plain calendar/time facts for [`expand`]'s strftime subset. A plain
/// struct (no Windows types) so the module stays pure/testable; the server
/// fills one in from `GetLocalTime` (see `src/server.rs`'s `local_clock`,
/// which this format subset is designed to reproduce for `%H:%M %d-%b-%y`).
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

/// Everything [`expand`] needs beyond the format string itself. Unchanged
/// field set from the pre-SP7 `options::FormatCtx` (see this module's docs
/// for why `client_*`/other tmux format variables aren't represented here
/// yet): `session`, `window_index`, `window_name`, `window_flags`,
/// `pane_index`, `hostname`, `now`, `pane_title`.
pub struct FormatCtx<'a> {
    pub session: &'a str,
    pub window_index: u32,
    pub window_name: &'a str,
    pub window_flags: &'a str,
    pub pane_index: u32,
    pub hostname: &'a str,
    pub now: SystemTimeParts,
    /// `#T`/`#{pane_title}`: the focused pane's OSC 0/2 title
    /// (`server::PaneRuntime::title`), empty until the pane's program ever
    /// sets one. Documented divergence from real tmux (carried over from the
    /// pre-SP7 engine): tmux's `#T`/`#{pane_title}` falls back to the pane's
    /// running command name when no title has ever been set; here it falls
    /// back to an empty string (no foreground-process tracking exists in
    /// this codebase, only the ConPTY-surfaced console title).
    pub pane_title: &'a str,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn month_name(m: u8) -> &'static str {
    MONTHS[(m.clamp(1, 12) as usize) - 1]
}

fn weekday_name(w: u8) -> &'static str {
    WEEKDAYS[(w % 7) as usize]
}

/// `#{host_short}`: the leading dot-component of `hostname` (e.g. `HOST` for
/// `HOST.example.com`, or `HOST` unchanged if there's no `.` at all).
fn host_short(host: &str) -> &str {
    host.split('.').next().unwrap_or(host)
}

/// Direct lookup of a long-form `#{name}` variable. `None` means "not a
/// known variable name" — the caller (`resolve_braced`) then falls back to
/// recursive expansion (if `name` itself contains further `#{` structure) or
/// empty (§5.3's "unknown plain names expand to empty string").
fn lookup_variable(name: &str, ctx: &FormatCtx) -> Option<String> {
    match name {
        "session_name" => Some(ctx.session.to_string()),
        "window_index" => Some(ctx.window_index.to_string()),
        "window_name" => Some(ctx.window_name.to_string()),
        "window_flags" => Some(ctx.window_flags.to_string()),
        "pane_index" => Some(ctx.pane_index.to_string()),
        "pane_title" => Some(ctx.pane_title.to_string()),
        "host" => Some(ctx.hostname.to_string()),
        "host_short" => Some(host_short(ctx.hostname).to_string()),
        _ => None,
    }
}

/// `format_true` (§5.1): non-empty and not exactly `"0"`.
fn truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

/// The two-step condition-evaluation rule shared by `#{?cond,...}`'s `cond`
/// and every `&&:`/`||:` operand (§5.2): try `text` as a direct variable
/// name first; if that fails, format-expand `text` and, if the expansion
/// changed nothing (an unrecognized bare word expands to itself unchanged,
/// since `expand` only transforms `#`/`%` sequences), treat it as false —
/// otherwise apply `format_true` to the expanded result.
///
/// `depth` is the current recursion depth (see [`expand_depth`]'s doc for
/// the `FORMAT_LOOP_LIMIT` cap); the re-expand below goes one level deeper.
fn eval_truth(text: &str, ctx: &FormatCtx, depth: u32) -> bool {
    if let Some(v) = lookup_variable(text, ctx) {
        return truthy(&v);
    }
    let expanded = expand_depth(text, ctx, depth + 1);
    if expanded == text {
        false
    } else {
        truthy(&expanded)
    }
}

/// Find the index (into `chars`) of the `}` that matches the `#{` whose
/// content starts at `i` (i.e. `i` is the index right after that `{`).
/// Mirrors `format_skip1`'s brace-counting: a nested `#{` increments depth,
/// a bare `}` at depth 0 is the match, a bare `}` at depth>0 decrements.
/// Any other `#<c>` two-char unit (an escape like `#,`/`##`/`#}`, or a
/// single-char alias) is skipped over WITHOUT being decoded — decoding
/// happens once, at actual leaf expansion time in [`expand`], never during
/// structural scanning (decoding early would corrupt a later re-scan, e.g.
/// `##S` must stay "literal #, literal S", not collapse into the `#S`
/// shorthand). Returns `None` for an unterminated `#{` (caller treats this
/// as "expand to end-of-string, producing nothing further" — no well-formed
/// input hits this).
fn find_close(chars: &[char], mut i: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    while i < chars.len() {
        if chars[i] == '#' && i + 1 < chars.len() {
            if chars[i + 1] == '{' {
                depth += 1;
            }
            i += 2;
            continue;
        }
        if chars[i] == '}' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
            i += 1;
            continue;
        }
        i += 1;
    }
    None
}

/// Split `s` on top-level (depth-0) commas, the same brace/escape-aware
/// scan as [`find_close`] (§5.2: "Separators found with `format_skip1`, so
/// nested `#{…}` and `#,` escapes don't split"). Escape sequences and nested
/// `#{...}` blocks are copied into the returned parts VERBATIM, undecoded —
/// each part is expanded (once, leaf-level) later by the caller via
/// [`expand`], which is where `##`/`#,`/`#}` actually get decoded.
fn split_top_commas(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && i + 1 < chars.len() {
            if chars[i + 1] == '{' {
                depth += 1;
            }
            buf.push(chars[i]);
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if chars[i] == '}' {
            if depth > 0 {
                depth -= 1;
            }
            buf.push('}');
            i += 1;
            continue;
        }
        if chars[i] == ',' && depth == 0 {
            parts.push(std::mem::take(&mut buf));
            i += 1;
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    parts.push(buf);
    parts
}

/// `#{?cond,true,false}` (and chained `#{?c1,v1,c2,v2,fallback}` pairs),
/// `rest` being the content after the leading `?` has already been
/// stripped. Evaluated pairwise: the first cond that's truthy short-circuits
/// to its (recursively expanded) value; an unpaired trailing part is the
/// else-value (also expanded); no match and no trailing part -> empty
/// (§5.2). `depth` threads through to [`eval_truth`]/[`expand_depth`] for
/// the `FORMAT_LOOP_LIMIT` cap (see [`expand_depth`]).
fn resolve_conditional(rest: &str, ctx: &FormatCtx, depth: u32) -> String {
    let parts = split_top_commas(rest);
    let mut idx = 0;
    while idx + 1 < parts.len() {
        if eval_truth(&parts[idx], ctx, depth + 1) {
            return expand_depth(&parts[idx + 1], ctx, depth + 1);
        }
        idx += 2;
    }
    if idx < parts.len() {
        expand_depth(&parts[idx], ctx, depth + 1)
    } else {
        String::new()
    }
}

/// `#{=N:x}` / `#{=-N:x}`: `content` must be `=`, then an optional `-`, then
/// one or more digits, then `:`. Returns `(N, remainder)` on a match (`N`
/// keeps its sign — negative means "truncate from the left, keep the right
/// N chars").
fn parse_truncate(content: &str) -> Option<(i64, &str)> {
    let rest = content.strip_prefix('=')?;
    let bytes = rest.as_bytes();
    let mut idx = 0;
    if idx < bytes.len() && bytes[idx] == b'-' {
        idx += 1;
    }
    let digits_start = idx;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == digits_start {
        return None; // no digits -> not a truncate modifier
    }
    let n: i64 = rest[..idx].parse().ok()?;
    let remainder = rest[idx..].strip_prefix(':')?;
    Some((n, remainder))
}

/// `n >= 0`: keep the left `n` chars (truncate the right end). `n < 0`: keep
/// the right `n.abs()` chars (truncate the left end). No-op if `s` already
/// fits. Char-counted, not byte-counted (matches every other width
/// computation in this codebase, e.g. `server::truncate_chars`).
fn truncate_n(s: &str, n: i64) -> String {
    let chars: Vec<char> = s.chars().collect();
    if n >= 0 {
        let n = n as usize;
        if chars.len() <= n {
            s.to_string()
        } else {
            chars[..n].iter().collect()
        }
    } else {
        let n = (-n) as usize;
        if chars.len() <= n {
            s.to_string()
        } else {
            chars[chars.len() - n..].iter().collect()
        }
    }
}

/// String-comparison operators §5.3 documents together in one table row
/// (`==:a,b` `!=:a,b` `<:a,b` `>:a,b` `<=:a,b` `>=:a,b`) — the task brief's
/// own parenthetical examples named only `==`/`!=`, but all six share one
/// two-argument dispatch, so all six are implemented (see this module's top
/// doc comment).
type CmpFn = fn(&str, &str) -> bool;
const COMPARISON_PREFIXES: &[(&str, CmpFn)] = &[
    ("==:", |a, b| a == b),
    ("!=:", |a, b| a != b),
    ("<=:", |a, b| a <= b),
    (">=:", |a, b| a >= b),
    ("<:", |a, b| a < b),
    (">:", |a, b| a > b),
];

/// Resolve the content of one `#{...}` block (everything between the outer
/// braces, NOT including them) into its final substituted text. Dispatches,
/// in order: `?` conditional -> string comparison -> `&&`/`||` n-ary boolean
/// -> `=N:`/`=-N:` length-limit -> plain variable lookup (falling back to a
/// full recursive [`expand`] if the name itself contains further `#{`
/// structure, else empty — §5.3).
///
/// `depth` is the current recursion depth; past `FORMAT_LOOP_LIMIT` (100,
/// see [`expand_depth`]'s doc) this returns `content` literally instead of
/// recursing further — this function is itself a recursion site (the
/// `=N:`/`=-N:` branch below calls back into itself) independent of
/// [`expand_depth`]'s own guard, so it needs the same check at entry.
fn resolve_braced(content: &str, ctx: &FormatCtx, depth: u32) -> String {
    if depth > FORMAT_LOOP_LIMIT {
        return content.to_string();
    }
    if let Some(rest) = content.strip_prefix('?') {
        return resolve_conditional(rest, ctx, depth);
    }
    for (prefix, op) in COMPARISON_PREFIXES {
        if let Some(rest) = content.strip_prefix(prefix) {
            let parts = split_top_commas(rest);
            if parts.len() != 2 {
                return String::new(); // malformed -- no real config produces this
            }
            let a = expand_depth(&parts[0], ctx, depth + 1);
            let b = expand_depth(&parts[1], ctx, depth + 1);
            return if op(&a, &b) { "1" } else { "0" }.to_string();
        }
    }
    if let Some(rest) = content.strip_prefix("&&:") {
        let parts = split_top_commas(rest);
        let result = !parts.is_empty() && parts.iter().all(|p| eval_truth(p, ctx, depth + 1));
        return if result { "1" } else { "0" }.to_string();
    }
    if let Some(rest) = content.strip_prefix("||:") {
        let parts = split_top_commas(rest);
        let result = parts.iter().any(|p| eval_truth(p, ctx, depth + 1));
        return if result { "1" } else { "0" }.to_string();
    }
    if let Some((n, remainder)) = parse_truncate(content) {
        let base = resolve_braced(remainder, ctx, depth + 1);
        return truncate_n(&base, n);
    }
    if let Some(v) = lookup_variable(content, ctx) {
        return v;
    }
    if content.contains("#{") {
        return expand_depth(content, ctx, depth + 1);
    }
    String::new() // unknown plain name -> empty (§5.3)
}

/// `FORMAT_LOOP_LIMIT` (real tmux's `format.c` constant, §5.1 of the
/// reference doc, line ~332): the maximum recursion depth [`expand_depth`]
/// (and its mutually-recursive helpers `resolve_braced`/`resolve_conditional`/
/// `eval_truth`) will descend before giving up and returning the remaining
/// input literally rather than recursing further.
const FORMAT_LOOP_LIMIT: u32 = 100;

/// Expand a tmux format string against `ctx`. See the module docs for the
/// full grammar this implements. Never panics, never returns an error —
/// every unrecognized/malformed construct degrades to empty text or a
/// literal passthrough, matching tmux's own "anything else renders empty"
/// posture. Delegates to [`expand_depth`] at depth 0.
pub fn expand(fmt: &str, ctx: &FormatCtx) -> String {
    expand_depth(fmt, ctx, 0)
}

/// The actual expansion loop behind [`expand`], with an explicit recursion
/// `depth` threaded through every mutually-recursive helper (`resolve_braced`,
/// `resolve_conditional`, `eval_truth`) so nesting driven entirely by
/// (possibly adversarial) input text can't stack-overflow the process: once
/// `depth` exceeds [`FORMAT_LOOP_LIMIT`] (tmux's own `FORMAT_LOOP_LIMIT` =
/// 100, §5.1), expansion stops and `fmt` is returned unexpanded/literal —
/// safe degradation (truncated output), never a panic or an abort. Every
/// recursive call site below passes `depth + 1`.
fn expand_depth(fmt: &str, ctx: &FormatCtx, depth: u32) -> String {
    if depth > FORMAT_LOOP_LIMIT {
        return fmt.to_string();
    }
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::with_capacity(fmt.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            if i + 1 >= chars.len() {
                i += 1; // trailing lone '#' -> dropped
                continue;
            }
            match chars[i + 1] {
                '#' => {
                    out.push('#');
                    i += 2;
                }
                ',' => {
                    out.push(',');
                    i += 2;
                }
                '}' => {
                    out.push('}');
                    i += 2;
                }
                '[' => {
                    // Inline style marker: passed through verbatim, brackets
                    // included, up to and including the first `]` --
                    // `status::styled_runs` parses these afterward. An
                    // unterminated marker (no closing `]`) is copied to
                    // end-of-string rather than dropped.
                    out.push('#');
                    out.push('[');
                    i += 2;
                    while i < chars.len() {
                        out.push(chars[i]);
                        let done = chars[i] == ']';
                        i += 1;
                        if done {
                            break;
                        }
                    }
                }
                '{' => match find_close(&chars, i + 2) {
                    Some(close) => {
                        let content: String = chars[i + 2..close].iter().collect();
                        out.push_str(&resolve_braced(&content, ctx, depth + 1));
                        i = close + 1;
                    }
                    None => {
                        // Unterminated `#{...` -- consumed to end-of-string,
                        // producing nothing further (no well-formed input
                        // hits this).
                        i = chars.len();
                    }
                },
                'S' => {
                    out.push_str(ctx.session);
                    i += 2;
                }
                'W' => {
                    out.push_str(ctx.window_name);
                    i += 2;
                }
                'I' => {
                    out.push_str(&ctx.window_index.to_string());
                    i += 2;
                }
                'P' => {
                    out.push_str(&ctx.pane_index.to_string());
                    i += 2;
                }
                'F' => {
                    out.push_str(ctx.window_flags);
                    i += 2;
                }
                'H' => {
                    out.push_str(ctx.hostname);
                    i += 2;
                }
                'T' => {
                    out.push_str(ctx.pane_title);
                    i += 2;
                }
                _ => {
                    i += 2; // unrecognized short code -> empty
                }
            }
        } else if c == '%' {
            if i + 1 >= chars.len() {
                out.push('%'); // trailing lone '%'
                i += 1;
                continue;
            }
            match chars[i + 1] {
                '%' => {
                    out.push('%');
                    i += 2;
                }
                'H' => {
                    out.push_str(&format!("{:02}", ctx.now.hour));
                    i += 2;
                }
                'M' => {
                    out.push_str(&format!("{:02}", ctx.now.min));
                    i += 2;
                }
                'S' => {
                    out.push_str(&format!("{:02}", ctx.now.sec));
                    i += 2;
                }
                'd' => {
                    out.push_str(&format!("{:02}", ctx.now.day));
                    i += 2;
                }
                'm' => {
                    out.push_str(&format!("{:02}", ctx.now.month));
                    i += 2;
                }
                'Y' => {
                    out.push_str(&ctx.now.year.to_string());
                    i += 2;
                }
                'y' => {
                    out.push_str(&format!("{:02}", ctx.now.year.rem_euclid(100)));
                    i += 2;
                }
                'b' => {
                    out.push_str(month_name(ctx.now.month));
                    i += 2;
                }
                'a' => {
                    out.push_str(weekday_name(ctx.now.weekday));
                    i += 2;
                }
                'p' => {
                    out.push_str(if ctx.now.hour < 12 { "AM" } else { "PM" });
                    i += 2;
                }
                'I' => {
                    let h12 = ctx.now.hour % 12;
                    out.push_str(&format!("{:02}", if h12 == 0 { 12 } else { h12 }));
                    i += 2;
                }
                other => {
                    out.push('%');
                    out.push(other);
                    i += 2;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(session: &'a str, window_flags: &'a str, pane_title: &'a str, hostname: &'a str) -> FormatCtx<'a> {
        FormatCtx {
            session,
            window_index: 1,
            window_name: "bash",
            window_flags,
            pane_index: 0,
            hostname,
            now: SystemTimeParts { year: 2026, month: 7, day: 7, weekday: 2, hour: 14, min: 5, sec: 9 },
            pane_title,
        }
    }

    #[test]
    fn braced_variable_expands() {
        let c = ctx("work", "*", "vim", "HOST.example.com");
        // c.window_index = 1, c.pane_index = 0 (from the ctx() helper above).
        assert_eq!(
            expand(
                "#{session_name}/#{window_index}/#{window_name}/#{window_flags}/#{pane_index}/#{pane_title}/#{host}/#{host_short}",
                &c
            ),
            "work/1/bash/*/0/vim/HOST.example.com/HOST"
        );
    }

    #[test]
    fn undefined_variable_expands_empty() {
        let c = ctx("s", "", "", "H");
        assert_eq!(expand("<#{nonsense}>", &c), "<>");
        // unrecognized single-char alias also empty, unchanged from the SP3 engine.
        assert_eq!(expand("<#Z>", &c), "<>");
    }

    #[test]
    fn conditional_true_and_false_parts() {
        // cond = "pane_title" (a bare variable name -- direct lookup, no
        // recursive re-expand needed): truthy when non-empty.
        let with_title = ctx("s", "", "vim", "H");
        assert_eq!(expand("#{?pane_title,has:#{pane_title},none}", &with_title), "has:vim");
        let no_title = ctx("s", "", "", "H");
        assert_eq!(expand("#{?pane_title,has:#{pane_title},none}", &no_title), "none");
    }

    /// Nested `#{...}` inside a conditional arm, PLUS a `#,`-escaped literal
    /// comma inside that same arm (proves `split_top_commas`'s depth
    /// tracking correctly does not split on the escaped comma or the nested
    /// brace's internal `}`).
    ///
    /// content = "window_flags,flag=#{window_flags}#,ok,none" splits (at
    /// depth 0 only) into exactly 3 parts: "window_flags",
    /// "flag=#{window_flags}#,ok", "none". cond truthy (window_flags="*")
    /// -> expand the true arm: "flag=" + (#{window_flags} -> "*") + (#,
    /// -> literal ",") + "ok" = "flag=*,ok".
    #[test]
    fn conditional_nested_expansion() {
        let c = ctx("s", "*", "", "H");
        assert_eq!(expand("#{?window_flags,flag=#{window_flags}#,ok,none}", &c), "flag=*,ok");
    }

    #[test]
    fn comparison_eq_ne() {
        let c = ctx("work", "", "", "H");
        assert_eq!(expand("#{==:#{session_name},work}", &c), "1");
        assert_eq!(expand("#{==:#{session_name},other}", &c), "0");
        assert_eq!(expand("#{!=:#{session_name},other}", &c), "1");
        assert_eq!(expand("#{!=:#{session_name},work}", &c), "0");
    }

    #[test]
    fn length_limit_truncates_right_and_left() {
        let c = ctx("s", "", "abcdefgh", "H"); // pane_title is 8 chars
        assert_eq!(expand("#{=5:pane_title}", &c), "abcde"); // keep left 5
        assert_eq!(expand("#{=-5:pane_title}", &c), "defgh"); // keep right 5
        assert_eq!(expand("#{=20:pane_title}", &c), "abcdefgh"); // fits, no-op
    }

    #[test]
    fn hash_escape_literal() {
        let c = ctx("s", "", "", "H");
        assert_eq!(expand("before ## after", &c), "before # after");
        assert_eq!(expand("###", &c), "#" /* ## -> #, then lone # dropped */);
        assert_eq!(expand("a#,b", &c), "a,b");
        assert_eq!(expand("a#}b", &c), "a}b");
    }

    /// #70's acceptance test: tmux's REAL `window-status-format` default
    /// (`#I:#W#{?window_flags,#{window_flags}, }`, `options::
    /// DEFAULT_WINDOW_STATUS_FORMAT` as of this task) must now expand
    /// byte-identically to what the SP6 shim produced by hand: one literal
    /// space when the window has no flags, the flags string itself
    /// otherwise.
    #[test]
    fn real_default_window_status_format_matches_sp6_shim_output() {
        let fmt = crate::options::DEFAULT_WINDOW_STATUS_FORMAT;
        let flagged = ctx("s", "*", "", "H");
        assert_eq!(expand(fmt, &flagged), "1:bash*");
        let flagless = ctx("s", "", "", "H");
        assert_eq!(expand(fmt, &flagless), "1:bash "); // trailing space, not empty
    }

    #[test]
    fn strftime_passthrough_preserved() {
        let c = ctx("s", "", "", "H");
        assert_eq!(expand("%H:%M %d-%b-%y", &c), "14:05 07-Jul-26");
        assert_eq!(expand("%Y-%m-%d %a %I:%M%p", &c), "2026-07-07 Tue 02:05PM");
        assert_eq!(expand("%%", &c), "%");
        assert_eq!(expand("%x stays", &c), "%x stays");
    }

    /// Malformed input regression (code-review Important #2): an unterminated
    /// `#{` finds no matching close ([`find_close`] returns `None`), so
    /// [`expand_depth`] consumes to end-of-string producing nothing further
    /// (see its `'{' => match find_close(...) { None => ... }` arm) -- "a"
    /// (pushed before the `#{` was hit) is all that survives. Asserts this
    /// documented current behavior and, implicitly, that it doesn't panic.
    #[test]
    fn unclosed_brace_consumes_to_end_without_panic() {
        let c = ctx("s", "", "", "H");
        assert_eq!(expand("a#{session_name", &c), "a");
    }

    /// Malformed input regression (code-review Important #2): `#{}` -- empty
    /// braced content. `find_close` matches the `}` immediately (content =
    /// ""); `resolve_braced("")` fails every prefix check, `lookup_variable("")`
    /// is `None`, `"".contains("#{")` is false, so it falls through to the
    /// final `String::new()` arm (empty). "x" and "y" survive from outside
    /// the braces -> "xy".
    #[test]
    fn empty_braces_expand_empty() {
        let c = ctx("s", "", "", "H");
        assert_eq!(expand("x#{}y", &c), "xy");
    }

    /// FORMAT_LOOP_LIMIT regression (code-review Important #1): a
    /// pathologically deep nest must terminate (not stack-overflow the
    /// process) once recursion passes depth 100, per `docs/tmux-reference/
    /// commands-config-options-formats.md` §5.1's `FORMAT_LOOP_LIMIT` (100).
    ///
    /// `layers[k]` is built programmatically so the expected output can be
    /// sliced out exactly rather than hand-typed (a 500-deep string is not
    /// something anyone should hand-compute): `layers[0]` = "session_name"
    /// (the leaf, no `#` in it at all); `layers[k]` = `#{` + `layers[k-1]` +
    /// `}` (one more wrap). `layers[500]` is fed to `expand`.
    ///
    /// Tracing `expand_depth`/`resolve_braced`'s mutual recursion on
    /// `layers[500]`: `expand_depth(layers[500-k], depth=2k)` always calls
    /// `resolve_braced(layers[500-k-1], depth=2k+1)` (one `resolve_braced`
    /// call per unwrapped layer, `depth` climbing by 2 per layer: +1 for the
    /// `expand_depth -> resolve_braced` call, +1 for `resolve_braced`'s own
    /// `content.contains("#{") -> expand_depth` fallback call on the next
    /// layer down). The n-th `resolve_braced` call (1-indexed) runs at
    /// `depth = 2n-1` and processes `layers[500-n]`. The guard is `depth >
    /// FORMAT_LOOP_LIMIT` (100); the first odd depth exceeding 100 is 101,
    /// at `n = 51`, processing `layers[500-51] = layers[449]` -- that call's
    /// guard fires immediately and returns `layers[449]` back UNCHANGED
    /// (literal, still `#{`-wrapped, not further expanded), and every
    /// enclosing frame passes that same string straight back up (each
    /// wrapping layer's `fmt` is *exactly* one `#{...}` occupying the whole
    /// string, so there is no surrounding literal text to prepend/append at
    /// any level) -- so `expand(layers[500], ..)` returns `layers[449]`
    /// exactly: 51 layers were unwrapped (500 -> 449) before the cap bit,
    /// leaving the remaining 449-deep nest around `session_name` as
    /// unexpanded literal text.
    #[test]
    fn deep_nesting_hits_depth_limit_without_overflow() {
        let mut layers = vec!["session_name".to_string()];
        for _ in 0..500 {
            let prev = layers.last().unwrap();
            layers.push(format!("#{{{}}}", prev));
        }
        let c = ctx("s", "", "", "H");
        let result = expand(layers.last().unwrap(), &c);
        // Termination without a stack overflow/process abort IS the main
        // assertion here (a pre-fix build of this code would abort the test
        // process on this call rather than returning at all).
        assert_eq!(result, layers[449]);
        // Sanity: NOT the fully-expanded leaf value ("s") -- proves
        // expansion really did stop early rather than coincidentally
        // finishing at the same answer.
        assert_ne!(result, "s");
        assert!(result.contains("#{"), "expected an unexpanded literal tail, got: {result}");
    }

    /// Malformed/edge-case regression (code-review Important #2): a nested
    /// braced variable inside a length-limit modifier, `#{=5:#{session_name}}`
    /// -- proves `resolve_braced`'s `=N:` branch recurses into the inner
    /// `#{session_name}` (via its own `content.contains("#{") -> expand_depth`
    /// fallback) BEFORE truncating, not after truncating literal `"#{session_name}"`
    /// text. A 20-char session name truncated to the left 5 chars is
    /// "super".
    #[test]
    fn length_limit_wraps_nested_braced_variable() {
        let c = ctx("superlongsessionname", "", "", "H"); // 20 chars
        assert_eq!(expand("#{=5:#{session_name}}", &c), "super");
    }
}
