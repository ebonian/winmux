//! Status-line span builder (pure).
//!
//! Turns the pre-expanded `status-left` text, the live window list, and the
//! status-bar option table into the ordered `(text, Style)` span sequence
//! `render::compose_back` draws left-to-right starting at column 0 (the
//! status-right text is drawn separately, right-aligned, by the server —
//! see `render::StatusRow::right`/`right_style`). Every span carries a
//! FULLY RESOLVED [`grid::Style`](crate::grid::Style).
//!
//! **SP6 Task 4 (status-justify / per-side styles / window formats /
//! separator):** `status_spans` grew from a hardcoded `#I:#W<flags>`-shaped
//! tab renderer into a full per-tab `window-status-format`/
//! `-current-format` expander (via [`crate::options::expand_format`]) with:
//!
//! - **justify** (`status-justify`): the window-list group's start column is
//!   computed per `docs/tmux-reference/status-line-and-messages.md` §1.4's
//!   positioning math (`left`/`centre`/`right`/`absolute-centre`) and
//!   realized as a literal run of `base`-styled padding spaces between the
//!   left section and the list — `render::compose_back` still just draws
//!   spans sequentially from column 0, so no renderer change was needed.
//! - **side styles**: `left` is styled `status-left-style` layered over
//!   `base` (previously always bare `base`); `status-right-style` is
//!   layered over `base` by the SERVER directly (`render::StatusRow::right`
//!   has only one style slot — see [`strip_style_markers`] for why
//!   status-right can't carry multiple inline-styled runs).
//! - **window formats**: each tab's text is `window-status-current-format`
//!   (current window) or `window-status-format` (every other window)
//!   expanded via `expand_format` against a per-window
//!   [`crate::options::FormatCtx`] (session/pane/hostname/time carried over
//!   from the caller's context; `window_index`/`window_name`/`window_flags`
//!   overridden per window). `expand_format` passes `#[...]` inline style
//!   markers through VERBATIM (SP6 Task 4 addition — see that function's
//!   doc comment); [`styled_runs`] (private) then splits the expanded text
//!   on those markers into styled sub-spans, additively layering each
//!   marker's parsed style onto the tab's base
//!   (`window-status(-current)-style` over `status-style`, unchanged
//!   layering rule from SP3). Text with no markers at all — the common case
//!   — yields exactly one span, byte-identical to the pre-Task-4 output.
//! - **separator**: `window-status-separator` (default a single space)
//!   replaces the old hardcoded `" "` between tabs (still omitted after the
//!   last tab).
//!
//! The CURRENT window's style is still `window-status-current-style`
//! layered over `base` directly — NOT over `window-status-style` (tmux
//! layers `window-status-current-style` over `status-style` directly, so an
//! fg set only in `window-status-style` never leaks into the current tab).
//!
//! **SP7 Task 1 (general format engine):** `expand_format` now delegates to
//! [`crate::format::expand`], a real recursive-descent engine (braced
//! variables, `#{?cond,true,false}` conditionals, comparisons, length
//! limits — see that module's docs). This removed the SP6 Task 4 "fix round
//! 1" flagless-padding shim that used to live in the per-window loop below:
//! `options::DEFAULT_WINDOW_STATUS_FORMAT` is now tmux's literal
//! `#I:#W#{?window_flags,#{window_flags}, }`, whose conditional the engine
//! evaluates directly, so the width-stable one-space-when-flagless behavior
//! falls out of the format string itself (closes follow-ups #27/#70).

use crate::grid::Style;
use crate::options::{expand_format, FormatCtx};
use crate::style::{self, PartialStyle};

pub struct WindowEntry {
    pub index: u32,
    pub name: String,
    pub current: bool,
    pub last: bool,
    pub zoomed: bool,
    /// SP7 Task 6 (closes follow-up #26): this window's own EFFECTIVE
    /// `window-status-format`/`-current-format` (resolved by the caller
    /// through this window's `window_options` overlay via `Options::
    /// window_status_format_for`/`window_status_current_format_for` --
    /// `window-status-format`/`-current-format` ARE window-scoped, so two
    /// windows in the same session can legitimately show different tab
    /// text/styling). `None` falls back to `status_spans`'s shared
    /// `window_format`/`window_current_format` argument -- what every
    /// pre-Task-6 caller/test still gets (byte-identical default
    /// behavior). A real caller with a live `Options`/overlay always
    /// resolves and passes `Some(..)`, since the `_for` getter already
    /// folds in the "no local override -> global default" fallback, so
    /// `Some` is correct whether or not this ONE window actually has a
    /// local override.
    pub format_override: Option<String>,
    /// Same idea as `format_override`, for `window-status-style`/
    /// `-current-style`.
    pub style_override: Option<PartialStyle>,
}

/// Flags string for one window: `*` if current, else `-` if last, else empty,
/// then `Z` appended when zoomed (e.g. `*Z`, `-Z`, `Z`, `*`, `-`, or empty).
fn flags(w: &WindowEntry) -> String {
    let mut f = String::new();
    if w.current {
        f.push('*');
    } else if w.last {
        f.push('-');
    }
    if w.zoomed {
        f.push('Z');
    }
    f
}

/// Split text already expanded by [`expand_format`] (which passes `#[...]`
/// inline style markers through verbatim) into styled runs: literal text
/// between markers, each carrying `default_style` layered with every
/// `#[...]` style seen so far (tmux's `style_parse` is additive — a later
/// `#[bg=black]` after an earlier `#[fg=white]` keeps both; a marker whose
/// content fails to parse, or one with no closing `]`, is a no-op/treated as
/// literal text — defensive, not expected from real configs). Text with no
/// markers at all yields exactly one span equal to `default_style` — this
/// is what keeps every pre-Task-4 test's expected values unchanged.
fn styled_runs(text: &str, default_style: Style) -> Vec<(String, Style)> {
    let mut spans = Vec::new();
    let mut current = PartialStyle::default();
    let mut buf = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            let mut inner = String::new();
            let mut closed = false;
            for c2 in chars.by_ref() {
                if c2 == ']' {
                    closed = true;
                    break;
                }
                inner.push(c2);
            }
            if closed {
                if !buf.is_empty() {
                    spans.push((std::mem::take(&mut buf), current.apply_to(default_style)));
                }
                if let Ok(parsed) = style::parse_style(&inner) {
                    current = current.merge(&parsed);
                }
            } else {
                // Unterminated marker: no real config produces this: treat
                // the `#[` + whatever followed as literal text rather than
                // silently dropping it.
                buf.push('#');
                buf.push('[');
                buf.push_str(&inner);
            }
        } else {
            buf.push(c);
        }
    }
    spans.push((buf, current.apply_to(default_style)));
    spans
}

/// Strip `#[...]` inline style markers from already-`expand_format`-expanded
/// text, keeping only the literal text. Used for `status-right`: unlike
/// `left` (folded into `status_spans`'s returned `Vec`, which can carry any
/// number of differently-styled runs) `render::StatusRow::right` is a single
/// `String` with one `right_style` — there is no slot for multiple styled
/// runs on that side, so any inline markers in `status-right` are dropped
/// rather than leaking their literal `#[...]` text onto the screen.
pub fn strip_style_markers(text: &str) -> String {
    styled_runs(text, Style::default()).into_iter().map(|(t, _)| t).collect()
}

/// The window-list group's starting column per `status-justify`'s
/// positioning rules (`docs/tmux-reference/status-line-and-messages.md`
/// §1.4; winmux has no `status-format`-configurable centre/after content, so
/// the general 8-screen trim-order engine collapses to these closed-form
/// offsets for the fixed left/list/right layout):
///
/// - `left` (default): list starts immediately after `left` — offset =
///   `left_width`.
/// - `centre`: list is centered in the gap BETWEEN left and right —
///   `middle = left_width + ((width - right_width) - left_width) / 2`,
///   list starts at `middle - list_width / 2`.
/// - `right`: list sits immediately before the right section — offset =
///   `width - right_width - list_width`.
/// - `absolute-centre`: list is centered in the FULL width, independent of
///   left/right — offset = `(width - list_width) / 2`.
///
/// All arithmetic saturates at 0 rather than panicking when a section is
/// wider than its allotted space (an overflowing list simply overlaps/abuts
/// its neighbour rather than scrolling with tmux's `<`/`>` markers — a
/// documented simplification, see the report).
fn list_offset(justify: &str, width: usize, left_width: usize, right_width: usize, list_width: usize) -> usize {
    match justify {
        "centre" => {
            let gap_end = width.saturating_sub(right_width);
            let middle = left_width + gap_end.saturating_sub(left_width) / 2;
            middle.saturating_sub(list_width / 2)
        }
        "right" => width.saturating_sub(right_width).saturating_sub(list_width),
        "absolute-centre" => width.saturating_sub(list_width) / 2,
        _ => left_width, // "left" and any unrecognized value
    }
}

/// Build the status-bar spans, ready for `render::compose_back` to draw
/// left-to-right from column 0 (see the module docs for the full SP6 Task 4
/// design). `ctx` supplies every format-expansion field EXCEPT
/// `window_index`/`window_name`/`window_flags`, which are overridden per
/// window in the loop below (`ctx`'s own values for those three fields, if
/// any, are ignored). `width` is the terminal's column count; `right_len` is
/// the char count of the (already length-capped, already
/// `strip_style_markers`-cleaned) `status-right` text the caller will draw
/// right-aligned — needed for the `centre`/`right`/`absolute-centre` offset
/// math but otherwise unused here.
#[allow(clippy::too_many_arguments)]
pub fn status_spans(
    left: &str,
    left_style: &PartialStyle,
    windows: &[WindowEntry],
    ctx: &FormatCtx,
    window_format: &str,
    window_current_format: &str,
    base: Style,
    win_style: &PartialStyle,
    win_current_style: &PartialStyle,
    separator: &str,
    justify: &str,
    width: u16,
    right_len: usize,
) -> Vec<(String, Style)> {
    let left_base = left_style.apply_to(base);
    let mut left_spans = styled_runs(left, left_base);
    let left_width: usize = left_spans.iter().map(|(t, _)| t.chars().count()).sum();

    let non_current = win_style.apply_to(base);
    let current = win_current_style.apply_to(base);

    let mut tab_spans: Vec<Vec<(String, Style)>> = Vec::with_capacity(windows.len());
    let mut tab_widths: Vec<usize> = Vec::with_capacity(windows.len());
    for w in windows {
        let flags_str = flags(w);
        let fmt = w
            .format_override
            .as_deref()
            .unwrap_or(if w.current { window_current_format } else { window_format });
        // SP7 Task 1: no shim needed anymore. tmux's real default
        // (`#I:#W#{?window_flags,#{window_flags}, }`,
        // `options::DEFAULT_WINDOW_STATUS_FORMAT`) is now stored VERBATIM
        // and the general format engine (`crate::format`) evaluates its
        // `#{?cond,a,b}` conditional correctly on its own: an empty
        // `window_flags` is falsy, so the conditional's else-branch (a
        // single literal space) renders directly, keeping a tab's width
        // stable across focus changes with no special-casing here (closes
        // follow-ups #27/#70; superseded the SP6 Task 4 padding shim that
        // used to live in this loop).
        let per_window_ctx = FormatCtx {
            session: ctx.session,
            window_index: w.index,
            window_name: &w.name,
            window_flags: &flags_str,
            pane_index: ctx.pane_index,
            hostname: ctx.hostname,
            now: ctx.now,
            pane_title: ctx.pane_title,
        };
        let text = expand_format(fmt, &per_window_ctx);
        let tab_base = match &w.style_override {
            Some(s) => s.apply_to(base),
            None if w.current => current,
            None => non_current,
        };
        let spans = styled_runs(&text, tab_base);
        let width: usize = spans.iter().map(|(t, _)| t.chars().count()).sum();
        tab_spans.push(spans);
        tab_widths.push(width);
    }
    let sep_width = separator.chars().count();
    let list_width: usize =
        tab_widths.iter().sum::<usize>() + sep_width.saturating_mul(windows.len().saturating_sub(1));

    let offset = list_offset(justify, width as usize, left_width, right_len, list_width);
    let pad = offset.saturating_sub(left_width);

    let mut spans = Vec::with_capacity(left_spans.len() + tab_spans.len() * 2);
    spans.append(&mut left_spans);
    if pad > 0 {
        spans.push((" ".repeat(pad), base));
    }
    let last_idx = windows.len().saturating_sub(1);
    for (i, ts) in tab_spans.into_iter().enumerate() {
        spans.extend(ts);
        if i != last_idx {
            spans.push((separator.to_string(), base));
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Color;
    use crate::options::SystemTimeParts;
    use crate::style::parse_style;

    fn win(index: u32, name: &str, current: bool, last: bool, zoomed: bool) -> WindowEntry {
        WindowEntry { index, name: name.to_string(), current, last, zoomed, format_override: None, style_override: None }
    }

    /// tmux default `status-style` resolved: `bg=green,fg=black`.
    fn base() -> Style {
        Style { fg: Color::Idx(0), bg: Color::Idx(2), ..Style::default() }
    }

    /// Default option pair: `window-status-style` mentions nothing,
    /// `window-status-current-style` = `underscore`.
    fn default_partials() -> (PartialStyle, PartialStyle) {
        (PartialStyle::default(), parse_style("underscore").unwrap())
    }

    /// A neutral `FormatCtx` for tests that don't exercise time/pane/host
    /// fields (`window_index`/`window_name`/`window_flags` are overridden
    /// per window by `status_spans` regardless of what's set here).
    fn ctx0() -> FormatCtx<'static> {
        FormatCtx {
            session: "s",
            window_index: 0,
            window_name: "",
            window_flags: "",
            pane_index: 0,
            hostname: "",
            now: SystemTimeParts { year: 2026, month: 1, day: 1, weekday: 0, hour: 0, min: 0, sec: 0 },
            pane_title: "",
        }
    }

    /// tmux's real default (`#I:#W#{?window_flags,#{window_flags}, }`) is
    /// stored verbatim as of SP7 Task 1 — the general format engine
    /// (`crate::format`) evaluates its `#{?...}` conditional directly, no
    /// shim needed (closes follow-ups #27/#70). Tests below alias the real
    /// constant so they exercise the REAL default path end to end.
    const DEFAULT_FMT: &str = crate::options::DEFAULT_WINDOW_STATUS_FORMAT;

    #[allow(clippy::too_many_arguments)]
    fn spans_default(
        left: &str,
        windows: &[WindowEntry],
        base: Style,
        win_style: &PartialStyle,
        win_current_style: &PartialStyle,
    ) -> Vec<(String, Style)> {
        status_spans(
            left,
            &PartialStyle::default(),
            windows,
            &ctx0(),
            DEFAULT_FMT,
            DEFAULT_FMT,
            base,
            win_style,
            win_current_style,
            " ",
            "left",
            200, // plenty of room: never pads, never truncates
            0,
        )
    }

    #[test]
    fn single_window_current() {
        let windows = vec![win(0, "powershell", true, false, false)];
        let (ws, wcs) = default_partials();
        let spans = spans_default("[0] ", &windows, base(), &ws, &wcs);
        // Defaults reproduce the old underline-only behavior: every span is
        // `base` except the current tab, which is `base` + underline.
        assert_eq!(
            spans,
            vec![
                ("[0] ".to_string(), base()),
                ("0:powershell*".to_string(), Style { underline: true, ..base() }),
            ]
        );
    }

    #[test]
    fn flags_last_and_zoomed() {
        let windows = vec![
            win(0, "vim", false, true, true),    // last + zoomed -> "-Z"
            win(1, "logs", false, false, true),  // zoomed only -> "Z"
            win(2, "shell", true, false, false), // current -> "*"
        ];
        let (ws, wcs) = default_partials();
        let spans = spans_default("[s] ", &windows, base(), &ws, &wcs);
        assert_eq!(
            spans,
            vec![
                ("[s] ".to_string(), base()),
                ("0:vim-Z".to_string(), base()),
                (" ".to_string(), base()),
                ("1:logsZ".to_string(), base()),
                (" ".to_string(), base()),
                ("2:shell*".to_string(), Style { underline: true, ..base() }),
            ]
        );
    }

    #[test]
    fn three_windows_order_and_separators() {
        let windows = vec![
            win(0, "bash", false, true, false),
            win(1, "powershell", true, false, false),
            win(2, "logs", false, false, false),
        ];
        let (ws, wcs) = default_partials();
        let spans = spans_default("[mysess] ", &windows, base(), &ws, &wcs);
        assert_eq!(
            spans,
            vec![
                ("[mysess] ".to_string(), base()),
                ("0:bash-".to_string(), base()),
                (" ".to_string(), base()),
                ("1:powershell*".to_string(), Style { underline: true, ..base() }),
                (" ".to_string(), base()),
                // Flagless window on the DEFAULT-format path: one padding
                // space (tmux's real default's `#{?window_flags,
                // #{window_flags}, }` else-branch, evaluated directly by the
                // format engine — SP7 Task 1).
                ("2:logs ".to_string(), base()),
            ]
        );
    }

    // Layering rules with CUSTOM partial styles: non-current tabs get
    // `window-status-style` over base; the current tab gets
    // `window-status-current-style` over base — NOT over
    // `window-status-style` (the blue fg from win_style must not leak into
    // the current tab, and its unmentioned bg must fall through to base's).
    #[test]
    fn custom_styles_layering() {
        let windows = vec![win(0, "a", false, true, false), win(1, "b", true, false, false)];
        let ws = parse_style("fg=blue").unwrap();
        let wcs = parse_style("fg=red,bold").unwrap();
        let spans = spans_default("[x] ", &windows, base(), &ws, &wcs);
        assert_eq!(
            spans,
            vec![
                // left prefix + separators stay pure base
                ("[x] ".to_string(), base()),
                // non-current: blue fg over base (bg stays green)
                ("0:a-".to_string(), Style { fg: Color::Idx(4), ..base() }),
                (" ".to_string(), base()),
                // current: red+bold over BASE (no blue anywhere; no
                // underline since wcs doesn't mention it)
                ("1:b*".to_string(), Style { fg: Color::Idx(1), bold: true, ..base() }),
            ]
        );
    }

    // ---- SP6 Task 4: status-justify, side styles, window formats, separator ----

    /// justify=centre: list centered in the gap BETWEEN left and right.
    /// left="AB" (width 2), right_len=4, width=20, one window whose format
    /// is bare `#I` and index 42 -> tab text "42" (width 2, no separator
    /// needed with a single window).
    /// middle = 2 + ((20-4)-2)/2 = 2 + 14/2 = 2+7 = 9
    /// offset = middle - list_width/2 = 9 - 2/2 = 9-1 = 8
    /// pad = offset - left_width = 8-2 = 6
    #[test]
    fn status_justify_centre_positions_window_list() {
        let windows = vec![win(42, "w", true, false, false)];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "AB",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I",
            "#I",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "centre",
            20,
            4,
        );
        assert_eq!(
            spans,
            vec![
                ("AB".to_string(), base_style),
                ("      ".to_string(), base_style), // 6 spaces of padding
                ("42".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    /// justify=right: list sits immediately before the right section.
    /// left="L" (width 1), right_len=5, width=20, one window `#I` index 7 ->
    /// "7" (width 1).
    /// offset = width - right_len - list_width = 20-5-1 = 14
    /// pad = offset - left_width = 14-1 = 13
    #[test]
    fn status_justify_right() {
        let windows = vec![win(7, "w", true, false, false)];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "L",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I",
            "#I",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "right",
            20,
            5,
        );
        assert_eq!(
            spans,
            vec![
                ("L".to_string(), base_style),
                (" ".repeat(13), base_style),
                ("7".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    /// justify=absolute-centre: list centered in the FULL width, ignoring
    /// left/right entirely (right_len is passed but unused by this branch).
    /// left="LEFT" (width 4), width=30, one window `#I` index 12 -> "12"
    /// (width 2).
    /// offset = (width - list_width)/2 = (30-2)/2 = 28/2 = 14
    /// pad = offset - left_width = 14-4 = 10
    #[test]
    fn status_justify_absolute_centre() {
        let windows = vec![win(12, "w", true, false, false)];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "LEFT",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I",
            "#I",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "absolute-centre",
            30,
            10,
        );
        assert_eq!(
            spans,
            vec![
                ("LEFT".to_string(), base_style),
                (" ".repeat(10), base_style),
                ("12".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    /// A custom `window-status-format` (used for BOTH current and
    /// non-current here, to isolate pure substitution from the
    /// current-vs-non-current format SELECTION tested separately below)
    /// expands `#I`/`#W`/`#F` per tab, literal spaces preserved.
    /// window 0: index=0 name="bash", non-current+last -> flags "-" ->
    ///   " 0 bash - "
    /// window 1: index=1 name="zsh", current -> flags "*" -> " 1 zsh * "
    #[test]
    fn window_status_format_expands_per_tab() {
        let windows = vec![win(0, "bash", false, true, false), win(1, "zsh", true, false, false)];
        let (ws, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "X",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            " #I #W #F ",
            " #I #W #F ",
            base_style,
            &ws,
            &wcs,
            " ",
            "left",
            200,
            0,
        );
        assert_eq!(
            spans,
            vec![
                ("X".to_string(), base_style),
                (" 0 bash - ".to_string(), base_style),
                (" ".to_string(), base_style),
                (" 1 zsh * ".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    /// `window-status-current-format` is used ONLY for the current window;
    /// every other window uses `window-status-format` — distinct wrapper
    /// strings make the selection observable.
    #[test]
    fn window_status_current_format_used_for_current() {
        let windows = vec![win(0, "a", false, false, false), win(1, "b", true, false, false)];
        let (ws, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I:#W",
            "[#I:#W]",
            base_style,
            &ws,
            &wcs,
            " ",
            "left",
            200,
            0,
        );
        assert_eq!(
            spans,
            vec![
                (String::new(), base_style),
                ("0:a".to_string(), base_style),
                (" ".to_string(), base_style),
                ("[1:b]".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    /// `status-left-style` layers over `status-style` for the left span,
    /// exactly like `window-status(-current)-style` layers over base for
    /// tabs — the two sides never leak into each other (the left's cyan fg
    /// doesn't affect the tab's style, and vice versa).
    #[test]
    fn side_styles_layer_over_status_style() {
        let windows = vec![win(0, "w", true, false, false)];
        let (_, wcs) = default_partials();
        let base_style = base();
        let left_style = parse_style("fg=cyan").unwrap();
        let spans = status_spans(
            "hi",
            &left_style,
            &windows,
            &ctx0(),
            DEFAULT_FMT,
            DEFAULT_FMT,
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "left",
            200,
            0,
        );
        assert_eq!(
            spans,
            vec![
                // fg=cyan over base (bg stays green, from base())
                ("hi".to_string(), Style { fg: Color::Idx(6), ..base_style }),
                // tab style untouched by left_style
                ("0:w*".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    /// `window-status-separator` set to `"|"` replaces the default single
    /// space between tabs (still omitted after the last one).
    #[test]
    fn window_status_separator_respected() {
        let windows = vec![
            win(0, "a", false, false, false),
            win(1, "b", false, false, false),
            win(2, "c", true, false, false),
        ];
        let (ws, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            DEFAULT_FMT,
            DEFAULT_FMT,
            base_style,
            &ws,
            &wcs,
            "|",
            "left",
            200,
            0,
        );
        assert_eq!(
            spans,
            vec![
                (String::new(), base_style),
                // Windows 0/1 are flagless on the DEFAULT-format path ->
                // one padding space each (the conditional's else-branch,
                // evaluated directly by the format engine).
                ("0:a ".to_string(), base_style),
                ("|".to_string(), base_style),
                ("1:b ".to_string(), base_style),
                ("|".to_string(), base_style),
                ("2:c*".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    // ---- width-stable default format (flagless padding, via the real conditional) ----

    /// tmux's real default `#I:#W#{?window_flags,#{window_flags}, }` emits
    /// ONE padding space when a window has NO flags, so a tab's width is
    /// stable across the most common transition (a window gaining/losing
    /// `*`/`-` on focus change) -- the format engine evaluates this
    /// conditional directly (SP7 Task 1), no special-casing in this module.
    /// windows: 0 "aa" current  -> flags "*" -> "0:aa*" (5 chars)
    ///          1 "bb" flagless -> flags " " -> "1:bb " (5 chars, SAME width)
    #[test]
    fn default_format_flagless_window_pads_one_space() {
        let (ws, wcs) = default_partials();
        let windows = vec![win(0, "aa", true, false, false), win(1, "bb", false, false, false)];
        let spans = spans_default("", &windows, base(), &ws, &wcs);
        assert_eq!(
            spans,
            vec![
                (String::new(), base()),
                ("0:aa*".to_string(), Style { underline: true, ..base() }),
                (" ".to_string(), base()),
                ("1:bb ".to_string(), base()),
            ]
        );
        // Width stability: flip which window is current -- every span's
        // char width must be unchanged (`*` and the padding space are both
        // exactly 1 column), so a focus change never reflows the status row.
        let flipped = vec![win(0, "aa", false, false, false), win(1, "bb", true, false, false)];
        let spans2 = spans_default("", &flipped, base(), &ws, &wcs);
        let widths: Vec<usize> = spans.iter().map(|(t, _)| t.chars().count()).collect();
        let widths2: Vec<usize> = spans2.iter().map(|(t, _)| t.chars().count()).collect();
        assert_eq!(widths, widths2);
        assert_eq!(spans2[1].0, "0:aa ");
        assert_eq!(spans2[3].0, "1:bb*");
    }

    /// The one-space padding lives entirely in the DEFAULT format string's
    /// own `#{?window_flags,#{window_flags}, }` conditional -- a CUSTOM
    /// format using the plain `#F` short code (not the conditional) gets the
    /// plain empty flags string, exactly what real tmux's `#{window_flags}`/
    /// `#F` substitutes, no invisible extra characters the user didn't
    /// write.
    #[test]
    fn custom_format_flagless_window_not_padded() {
        let (ws, wcs) = default_partials();
        let windows = vec![win(0, "aa", false, false, false)];
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "<#I:#W#F>",
            "<#I:#W#F>",
            base(),
            &ws,
            &wcs,
            " ",
            "left",
            200,
            0,
        );
        assert_eq!(spans[1].0, "<0:aa>"); // no padding space before `>`
    }

    // ---- inline `#[...]` style markers within a window format ----

    #[test]
    fn inline_style_marker_in_window_format() {
        let windows = vec![win(3, "vim", true, false, false)];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I #[fg=white]#W",
            "#I #[fg=white]#W",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "left",
            200,
            0,
        );
        assert_eq!(
            spans,
            vec![
                (String::new(), base_style),
                // "3 " stays the current-tab style (underline), "vim" gets
                // fg=white layered ON TOP of the current-tab style.
                ("3 ".to_string(), Style { underline: true, ..base_style }),
                (
                    "vim".to_string(),
                    Style { fg: Color::Idx(7), underline: true, ..base_style }
                ),
            ]
        );
    }

    // ---- per-window format/style override (SP7 Task 6, closes follow-up #26) ----

    /// A per-window `format_override`/`style_override` (what the caller
    /// sets when a window has its OWN `window-status-format`/`-style`
    /// override, resolved through THAT window's overlay) wins for that ONE
    /// window; every other window with `None` still uses the shared
    /// `window_format`/`win_style` arguments -- proving one window's
    /// `setw` never leaks onto its neighbours.
    #[test]
    fn per_window_override_wins_for_that_window_only() {
        let (ws, wcs) = default_partials();
        let base_style = base();
        let mut overridden = win(0, "special", false, false, false);
        overridden.format_override = Some("CUSTOM:#W".to_string());
        overridden.style_override = Some(parse_style("fg=red").unwrap());
        let plain = win(1, "plain", true, false, false);
        let windows = vec![overridden, plain];
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I:#W",
            "[#I:#W]",
            base_style,
            &ws,
            &wcs,
            " ",
            "left",
            200,
            0,
        );
        assert_eq!(
            spans,
            vec![
                (String::new(), base_style),
                // window 0: override wins over BOTH the shared format
                // (which would've been "#I:#W" since it's non-current)
                // AND the shared non-current style.
                ("CUSTOM:special".to_string(), Style { fg: Color::Idx(1), ..base_style }),
                (" ".to_string(), base_style),
                // window 1: no override -> shared current format/style.
                ("[1:plain]".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }
}
