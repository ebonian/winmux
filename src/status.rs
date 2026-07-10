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
//!
//! **Fix (review of 128cfc0, SP7 parity wave 3):** the window-list overflow
//! scrolling added by SP7 Task 7 below (item (1)) was a `status_spans`-only
//! change — `server::dispatch::mouse_status_click`'s status-row click
//! hit-test kept its OWN, unrelated reconstruction of the tab layout
//! (unscrolled, hardcoded `"{index}:{name}{flags}"` text), so the two could
//! disagree the moment a real list actually scrolled: a click on the
//! visually-current tab could resolve to the wrong window. The layout MATH
//! (not the styling) is now factored into two private helpers —
//! [`window_tab_texts`] (per-window expanded text + visible width, format-
//! override-aware) and `plan_tab_layout` (the justify/scroll/marker
//! decision, given only widths) — that `status_spans` and the new public
//! [`status_tab_columns`] both call, so there is exactly one place that
//! decides what's visible where; `mouse_status_click` now maps its click
//! column through [`status_tab_columns`]'s [`TabColumn`] ranges instead of
//! reconstructing anything itself.
//!
//! **SP7 Task 7 (status-line residuals):** three more gaps closed. (1)
//! **Window-list overflow scrolling** (closes #69a): when `left` + the
//! window list + `right` together exceed the terminal width, `status_spans`
//! now scrolls the list to keep the CURRENT window visible and draws `<`/`>`
//! markers wherever content still exists off-screen, per
//! `docs/tmux-reference/status-line-and-messages.md` §1.4 — see the private
//! `group_runs` helper and the overflow branch inside `status_spans` for the
//! exact cell-granularity algorithm (a documented simplification from
//! tmux's own eight-screen `format_draw` model, not a byte-for-byte port).
//! (2) **Per-window active-pane context** (closes #71): [`WindowEntry`]
//! gained `pane_index`/`pane_title`, and each tab's `per_window_ctx` now
//! reads THOSE fields instead of the shared `ctx`'s (which only ever held
//! the acting client's focused pane in the CURRENT window) — `#P`/`#T`
//! inside a `window-status(-current)-format` now shows the RIGHT window's
//! own active pane. (3) **Visible-width `status-left` cap** (closes #69b):
//! [`truncate_visible`] counts only characters outside a `#[...]` marker
//! toward `status-left-length`'s budget and never bisects one, bringing
//! `status-left` in line with `status-right`'s existing
//! strip-then-cap treatment.

use crate::grid::Style;
use crate::options::{expand_format, FormatCtx};
use crate::style::{self, PartialStyle};

pub struct WindowEntry {
    pub index: u32,
    pub name: String,
    pub current: bool,
    pub last: bool,
    pub zoomed: bool,
    /// Alerts subsystem (SP7 Task 17, closes follow-up #74): tmux's
    /// `WINLINK_ACTIVITY`/`WINLINK_BELL`/`WINLINK_SILENCE` display flags
    /// (`model::Window::alert_activity`/`alert_bell`/`alert_silence`,
    /// resolved by the caller). Feed [`flags`]'s `#`/`!`/`~` chars; every
    /// pre-Task-17 caller/test gets `false` here (via the `win()` test
    /// helper), so no earlier expected flags string changes.
    pub activity: bool,
    pub bell: bool,
    pub silence: bool,
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
    /// SP7 Task 7 (closes follow-up #71): THIS window's own active pane's
    /// `#P`/`pane_index` value (already `pane-base-index`-shifted by the
    /// caller, same rule `render_one` already applies for the shared `ctx`).
    /// Every pre-Task-7 caller/test gets `0` here, which is the correct
    /// value for a lone default pane and keeps every earlier test's expected
    /// spans unchanged (none of them expand `#P` in a window format).
    pub pane_index: u32,
    /// THIS window's own active pane's `#T`/`pane_title` value. Empty string
    /// for every pre-Task-7 caller/test (same "no earlier test exercises
    /// `#T`" reasoning as `pane_index`).
    pub pane_title: String,
}

/// Flags string for one window, tmux's fixed `window_printable_flags` order
/// (`docs/tmux-reference/status-line-and-messages.md` §2.3): `#` activity,
/// `!` bell, `~` silence, then `*` if current else `-` if last, then `Z` if
/// zoomed (`M` marked-pane is not implemented -- winmux has no
/// `server_check_marked` equivalent, documented narrowing). E.g. `!*Z`,
/// `#-`, `~`, `*Z`, or empty.
fn flags(w: &WindowEntry) -> String {
    let mut f = String::new();
    if w.activity {
        f.push('#');
    }
    if w.bell {
        f.push('!');
    }
    if w.silence {
        f.push('~');
    }
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

/// Truncate `text` (already `expand_format`-expanded, so it may still carry
/// literal `#[...]` inline style markers) to `max` VISIBLE characters --
/// closes follow-up #69b. Counts only characters OUTSIDE a `#[...]` marker
/// toward the budget (a marker draws zero columns) and never bisects one: a
/// marker is emitted whole if the budget isn't exhausted yet when it's
/// reached, otherwise it (and everything after it) is dropped whole. Used
/// for `status-left`, which -- unlike `status-right` (`strip_style_markers`
/// then a plain char-count cap, since `render::StatusRow::right` has only
/// one style slot to begin with) -- keeps its markers all the way through so
/// `status_spans`'s `styled_runs` can still split it into differently-styled
/// runs. Text with no markers at all behaves exactly like a plain
/// `s.chars().take(max)` cap.
pub fn truncate_visible(text: &str, max: u16) -> String {
    let max = max as usize;
    let mut out = String::new();
    let mut visible = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek() == Some(&'[') {
            let mut marker = String::from("#[");
            chars.next(); // consume '['
            for c2 in chars.by_ref() {
                marker.push(c2);
                if c2 == ']' {
                    break;
                }
            }
            if visible < max {
                out.push_str(&marker);
            }
            continue;
        }
        if visible >= max {
            break;
        }
        out.push(c);
        visible += 1;
    }
    out
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

/// Expand every window's tab text ONCE (format-override-aware) and measure
/// its visible width -- shared by [`status_spans`] (drawing) and
/// [`status_tab_columns`] (hit-testing) so there is exactly one place that
/// decides what a tab's text is, instead of two parallel reimplementations
/// that can silently drift apart (the bug this fixes: `mouse_status_click`
/// used to reimplement this loop with its own hardcoded format guess).
/// Returns `(expanded text per window -- `#[...]` markers still verbatim,
/// for [`styled_runs`] to split; VISIBLE width per window -- chars outside
/// any marker, a marker draws zero columns; the `windows` slice position of
/// the current window, if any)`.
fn window_tab_texts(
    windows: &[WindowEntry],
    ctx: &FormatCtx,
    window_format: &str,
    window_current_format: &str,
) -> (Vec<String>, Vec<usize>, Option<usize>) {
    let mut texts = Vec::with_capacity(windows.len());
    let mut widths = Vec::with_capacity(windows.len());
    let mut current_idx = None;
    for (i, w) in windows.iter().enumerate() {
        if w.current {
            current_idx = Some(i);
        }
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
        // SP7 Task 7 (closes follow-up #71): `#P`/`#T` inside a per-window
        // format must show THAT window's own active pane, not the acting
        // client's focused pane in the CURRENT window (`ctx.pane_index`/
        // `ctx.pane_title`) -- those two fields are only correct for the
        // one window that happens to be current, and were wrongly reused
        // for every OTHER window's tab before this fix.
        let per_window_ctx = FormatCtx {
            session: ctx.session,
            window_index: w.index,
            window_name: &w.name,
            window_flags: &flags_str,
            pane_index: w.pane_index,
            hostname: ctx.hostname,
            now: ctx.now,
            pane_title: &w.pane_title,
        };
        let text = expand_format(fmt, &per_window_ctx);
        widths.push(strip_style_markers(&text).chars().count());
        texts.push(text);
    }
    (texts, widths, current_idx)
}

/// Pure layout decision shared by [`status_spans`] and
/// [`status_tab_columns`] -- fix for a review finding on 128cfc0 (see the
/// module docs' "Fix" note): given each tab's visible width, decide where
/// the window-list content begins, whether it's clipped/scrolled, and (if
/// so) which end marker(s) are needed. This is the ONE place the SP7 Task 7
/// overflow-scrolling algorithm (§1.4's cell-granularity scroll-to-keep-
/// current-visible model) lives; both callers derive their answer from the
/// SAME [`TabLayout`], which is what makes a status-row click hit-test
/// agree with what's actually drawn.
struct TabLayout {
    /// Absolute column (0-based, in the final status row) where the tab
    /// list's VISIBLE content begins: right after `left` + justify padding
    /// (fit case), or right after `left` + a leading `<` marker (overflow
    /// case, which never pads).
    content_start_col: usize,
    /// `Some((start, end))` -- the RAW (unclipped) flattened tab+separator
    /// coordinate range that's actually visible -- when the list had to be
    /// scrolled/cropped; `None` when everything fit as-is (nothing to clip).
    clip: Option<(usize, usize)>,
    marker_left: bool,
    marker_right: bool,
    /// Per-window (slice-index-aligned) `[start, end)` span in the RAW
    /// (unclipped) flattened tab+separator coordinate space.
    raw_spans: Vec<(usize, usize)>,
}

fn plan_tab_layout(
    tab_widths: &[usize],
    current_idx: Option<usize>,
    sep_width: usize,
    left_width: usize,
    right_len: usize,
    justify: &str,
    width: u16,
) -> TabLayout {
    let mut raw_spans = Vec::with_capacity(tab_widths.len());
    let mut cursor = 0usize;
    let last_idx = tab_widths.len().saturating_sub(1);
    for (i, w) in tab_widths.iter().enumerate() {
        let start = cursor;
        cursor += w;
        raw_spans.push((start, cursor));
        if i != last_idx {
            cursor += sep_width;
        }
    }
    let list_width = cursor;
    let list_avail = (width as usize).saturating_sub(left_width).saturating_sub(right_len);

    if list_width <= list_avail {
        // Fits (or nothing to show): the justify math decides a start
        // column; the caller realizes the gap as literal padding spaces, no
        // markers.
        let offset = list_offset(justify, width as usize, left_width, right_len, list_width);
        let content_start_col = offset.max(left_width);
        return TabLayout { content_start_col, clip: None, marker_left: false, marker_right: false, raw_spans };
    }

    // Overflow (SP7 Task 7, closes follow-up #69a): §1.4's
    // `format_draw_put_list` scrolls the window list at CELL granularity to
    // keep the CURRENT window's centre visible, drawing `<`/`>` markers
    // wherever content still exists beyond that edge. No padding is ever
    // used in this branch: an overflowing list, by definition, consumes its
    // entire allotted budget with nothing left over for any justify's gap.
    let (focus_start, focus_end) = current_idx.and_then(|i| raw_spans.get(i).copied()).unwrap_or((0, 0));
    let focus_centre = focus_start + (focus_end.saturating_sub(focus_start)) / 2;

    // Fixed-point search for which end marker(s) are actually needed:
    // reserving a marker's column shrinks the visible content window, which
    // can flip whether the OTHER end is still off-screen (and vice versa).
    // This converges because `reserved` (and hence every value derived from
    // it below) is a PURE function of `(marker_left, marker_right)`, which
    // only ever takes one of 3 values (`reserved` ∈ {0, 1, 2} -- `true,true`
    // and `false,false` both aren't reachable as a STARTING guess here, but
    // the search still only ever visits a handful of the 4 boolean-pair
    // states before repeating one) -- capped at 4 passes as a documented
    // upper bound on that convergence, not a heuristic guess.
    let mut marker_left = true;
    let mut marker_right = true;
    let mut start = 0usize;
    let mut content_w = 0usize;
    for _ in 0..4 {
        let reserved = marker_left as usize + marker_right as usize;
        content_w = list_avail.saturating_sub(reserved);
        start = if content_w == 0 {
            focus_centre.min(list_width)
        } else {
            let half = content_w / 2;
            let raw_start = focus_centre.saturating_sub(half);
            let max_start = list_width.saturating_sub(content_w);
            raw_start.min(max_start)
        };
        let end = (start + content_w).min(list_width);
        let new_left = start > 0;
        let new_right = end < list_width;
        if new_left == marker_left && new_right == marker_right {
            break;
        }
        marker_left = new_left;
        marker_right = new_right;
    }
    let end = (start + content_w).min(list_width);
    // Defensive final-consistency check (review finding on 128cfc0): the
    // loop only `break`s when the flags it just recomputed from THIS pass's
    // `start`/`end` already match the flags that pass assumed, so
    // `marker_left`/`marker_right` and `start`/`content_w`/`end` can never
    // end up paired across two different passes (a "knife-edge" 2-cycle
    // would instead just never satisfy the `break` condition and run the
    // full 4 passes, still ending on a self-consistent pair). Assert the
    // invariant rather than trust it silently, so a future edit that
    // reorders/shortcuts this loop trips a debug build instead of quietly
    // emitting a mismatched marker.
    debug_assert_eq!(marker_left, start > 0, "TabLayout: marker_left must match final start");
    debug_assert_eq!(marker_right, end < list_width, "TabLayout: marker_right must match final end");

    let content_start_col = left_width + marker_left as usize;
    TabLayout { content_start_col, clip: Some((start, end)), marker_left, marker_right, raw_spans }
}

/// One window's tab column span in the FINAL rendered status row (0-based,
/// end exclusive). Returned by [`status_tab_columns`], the single source of
/// truth a status-row click hit-test (`server::dispatch::
/// mouse_status_click`) maps through — see the module docs' "Fix" note for
/// why this replaced an ad hoc reconstruction that broke under window-list
/// overflow scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabColumn {
    /// This window's position in the `windows` slice passed to
    /// [`status_tab_columns`]/[`status_spans`] -- NOT its tmux `#I` index. A
    /// caller that built `windows` from (e.g.) `session.windows` in order
    /// can index back into that same slice to recover the `WindowId`.
    pub window_pos: usize,
    /// First visible column this tab's text occupies.
    pub start: u16,
    /// One past the last visible column (exclusive). May be narrower than
    /// the tab's full expanded width if the overflow branch's scroll window
    /// clips one edge of it; a window scrolled fully off-screen has no
    /// entry in the returned `Vec` at all.
    pub end: u16,
}

/// Column ranges a status-row click hit-test can map through -- see
/// [`TabColumn`]'s doc comment. Mirrors [`status_spans`]'s layout exactly
/// (same [`window_tab_texts`]/`plan_tab_layout` core, so it's byte-for-byte
/// what's actually drawn, including window-list overflow scrolling and its
/// `<`/`>` markers) but skips style resolution entirely, since a click
/// doesn't care what color a tab is.
#[allow(clippy::too_many_arguments)]
pub fn status_tab_columns(
    left: &str,
    windows: &[WindowEntry],
    ctx: &FormatCtx,
    window_format: &str,
    window_current_format: &str,
    separator: &str,
    justify: &str,
    width: u16,
    right_len: usize,
) -> Vec<TabColumn> {
    let left_width = strip_style_markers(left).chars().count();
    let sep_width = separator.chars().count();
    let (_texts, tab_widths, current_idx) = window_tab_texts(windows, ctx, window_format, window_current_format);
    let layout = plan_tab_layout(&tab_widths, current_idx, sep_width, left_width, right_len, justify, width);
    let base_offset = layout.clip.map(|(s, _)| s).unwrap_or(0);

    let mut out = Vec::with_capacity(windows.len());
    for (i, &(raw_start, raw_end)) in layout.raw_spans.iter().enumerate() {
        let (vis_start, vis_end) = match layout.clip {
            None => (raw_start, raw_end),
            Some((cstart, cend)) => {
                let s = raw_start.max(cstart);
                let e = raw_end.min(cend);
                if s >= e {
                    continue; // scrolled fully off-screen: no hit box
                }
                (s, e)
            }
        };
        let abs_start = layout.content_start_col + (vis_start - base_offset);
        let abs_end = layout.content_start_col + (vis_end - base_offset);
        out.push(TabColumn { window_pos: i, start: abs_start as u16, end: abs_end as u16 });
    }
    out
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

    let (tab_texts, tab_widths, current_idx) = window_tab_texts(windows, ctx, window_format, window_current_format);
    let tab_spans: Vec<Vec<(String, Style)>> = windows
        .iter()
        .zip(tab_texts.iter())
        .map(|(w, text)| {
            let tab_base = match &w.style_override {
                Some(s) => s.apply_to(base),
                None if w.current => current,
                None => non_current,
            };
            styled_runs(text, tab_base)
        })
        .collect();

    let sep_width = separator.chars().count();
    let layout = plan_tab_layout(&tab_widths, current_idx, sep_width, left_width, right_len, justify, width);

    let mut spans = Vec::with_capacity(left_spans.len() + tab_spans.len() * 2);
    spans.append(&mut left_spans);

    match layout.clip {
        None => {
            // Fits (or nothing to show): unchanged pre-Task-7 behavior -- the
            // justify math decides a start column, realized as literal
            // padding spaces, no markers.
            let pad = layout.content_start_col.saturating_sub(left_width);
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
        }
        Some((start, end)) => {
            // Flatten the full (unclipped) tab+separator sequence into one
            // per-char `(char, Style)` run so `plan_tab_layout`'s scroll
            // window can land mid-tab exactly like real tmux's (a documented
            // simplification from tmux's own eight-screen model is that
            // winmux scrolls the WHOLE left/list/right block as a single
            // fixed budget rather than tmux's justify-specific trim order --
            // see the status-line report for the ruling).
            let mut flat: Vec<(char, Style)> = Vec::with_capacity(tab_widths.iter().sum());
            let last_idx = tab_spans.len().saturating_sub(1);
            for (i, ts) in tab_spans.into_iter().enumerate() {
                for (text, style) in &ts {
                    flat.extend(text.chars().map(|c| (c, *style)));
                }
                if i != last_idx {
                    flat.extend(separator.chars().map(|c| (c, base)));
                }
            }
            if layout.marker_left {
                spans.push(("<".to_string(), base));
            }
            spans.extend(group_runs(&flat[start..end]));
            if layout.marker_right {
                spans.push((">".to_string(), base));
            }
        }
    }
    spans
}

/// Merge consecutive same-`Style` characters from a per-char slice back into
/// `(String, Style)` spans -- the inverse of flattening, used by the
/// window-list overflow branch above after it slices `flat` to the visible
/// column range.
fn group_runs(chars: &[(char, Style)]) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    for (c, style) in chars {
        match out.last_mut() {
            Some((text, last_style)) if last_style == style => text.push(*c),
            _ => out.push((c.to_string(), *style)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Color;
    use crate::options::SystemTimeParts;
    use crate::style::parse_style;

    fn win(index: u32, name: &str, current: bool, last: bool, zoomed: bool) -> WindowEntry {
        WindowEntry {
            index,
            name: name.to_string(),
            current,
            last,
            zoomed,
            activity: false,
            bell: false,
            silence: false,
            format_override: None,
            style_override: None,
            pane_index: 0,
            pane_title: String::new(),
        }
    }

    /// SP7 Task 17 (closes follow-up #74): same as [`win`] but with the
    /// three alert flags settable, for `flags()`-ordering coverage.
    fn win_alert(index: u32, name: &str, current: bool, activity: bool, bell: bool, silence: bool) -> WindowEntry {
        let mut w = win(index, name, current, false, false);
        w.activity = activity;
        w.bell = bell;
        w.silence = silence;
        w
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

    /// SP7 Task 17 (closes follow-up #74): `#`/`!`/`~` render in tmux's
    /// fixed `window_printable_flags` order, ahead of `*`/`-`/`Z`
    /// (`docs/tmux-reference/status-line-and-messages.md` §2.3).
    #[test]
    fn flags_bell_activity_silence_order() {
        let windows = vec![
            win_alert(0, "bg", false, true, true, true), // all three + non-current -> "#!~"
            win_alert(1, "cur", true, false, false, false), // current, no alerts (clear-on-visit) -> "*"
        ];
        let (ws, wcs) = default_partials();
        let spans = spans_default("[a] ", &windows, base(), &ws, &wcs);
        assert_eq!(
            spans,
            vec![
                ("[a] ".to_string(), base()),
                ("0:bg#!~".to_string(), base()),
                (" ".to_string(), base()),
                ("1:cur*".to_string(), Style { underline: true, ..base() }),
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

    /// Verify-and-mark evidence for follow-up #31 ("inline `#[...]`
    /// per-segment style overrides are not parsed" in `status-left`/
    /// `-right`): `status-left`'s OWN text (not just a window format) is
    /// run through `styled_runs` exactly like every window tab, so a
    /// literal `#[...]` marker embedded in the ALREADY-EXPANDED `left`
    /// argument splits it into multiple differently-styled spans, additively
    /// layered on top of `status-left-style`-over-`base`. `status-right`
    /// deliberately does NOT do this (`render::StatusRow::right` has only
    /// one style slot, so `server::render_one` strips its markers via
    /// `strip_style_markers` before assignment -- a documented, intentional
    /// single-style-slot constraint, not an unimplemented gap).
    #[test]
    fn status_left_inline_style_marker_splits_spans() {
        // No windows: isolates `left`'s own span-splitting from the window
        // list entirely (`list_width` = 0, always the "fits" branch, no
        // padding since `left_width` == the "left" justify offset).
        let windows: Vec<WindowEntry> = vec![];
        let base_style = base();
        // status-left-style = fg=cyan, layered under BOTH inline markers
        // below (never overridden -- neither marker mentions `fg=cyan`
        // specifically, but the second marker's `bg=blue` accumulates
        // ADDITIVELY on top of the first marker's `fg=red`, per
        // `styled_runs`' doc comment: "a later `#[bg=black]` after an
        // earlier `#[fg=white]` keeps both").
        let left_style = parse_style("fg=cyan").unwrap();
        let spans = status_spans(
            "#[fg=red]ERR#[bg=blue]ok",
            &left_style,
            &windows,
            &ctx0(),
            "#I",
            "#I",
            base_style,
            &PartialStyle::default(),
            &PartialStyle::default(),
            " ",
            "left",
            200,
            0,
        );
        assert_eq!(
            spans,
            vec![
                // "ERR": fg=red (overriding left_style's fg=cyan) over base
                // (bg stays base's green -- bg was never mentioned).
                ("ERR".to_string(), Style { fg: Color::Idx(1), ..base_style }),
                // "ok": fg=red STILL applies (additive marker state) plus
                // bg=blue now layered on top.
                ("ok".to_string(), Style { fg: Color::Idx(1), bg: Color::Idx(4), ..base_style }),
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

    // ---- window-list overflow scrolling + markers (SP7 Task 7, closes follow-up #69a) ----

    /// List fits (`overflow_markers_absent_when_list_fits`): 3 windows,
    /// bare `#I` format (each tab 1 char wide), separator " ", left="",
    /// right_len=0, width=10. list_width = 1+1+1+1+1 = 5 (three tabs + two
    /// separators) <= list_avail (10-0-0=10) -> the pre-Task-7 fit branch,
    /// no `<`/`>` anywhere.
    #[test]
    fn overflow_markers_absent_when_list_fits() {
        let windows = vec![
            win(0, "a", false, false, false),
            win(1, "b", true, false, false),
            win(2, "c", false, false, false),
        ];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I",
            "#I",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "left",
            10,
            0,
        );
        let joined: String = spans.iter().map(|(t, _)| t.as_str()).collect();
        assert!(!joined.contains('<') && !joined.contains('>'), "no overflow markers when the list fits: {joined:?}");
        assert_eq!(joined, "0 1 2");
    }

    /// `window_list_scrolls_to_keep_current_visible_with_markers`: 5
    /// windows, bare `#I` format (index 0-4, one char each), separator " ",
    /// left="", right_len=0, width=5 (narrow). current = window at array
    /// position 4 (the LAST window).
    ///
    /// Unclipped list (concatenated, one char per column):
    ///   col: 0123456789
    ///   text: 0 1 2 3 4
    /// (9 columns total: 5 tabs + 4 separators.) `list_avail` =
    /// width(5) - left_width(0) - right_len(0) = 5, and 9 > 5 -> overflow.
    ///
    /// Window 4's tab is the single char "4" at column 8 -> focus_start=8,
    /// focus_end=9, focus_centre = 8 + (9-8)/2 = 8.
    ///
    /// Fixed-point marker search (list_avail=5):
    ///   pass 1 (assume both markers): reserved=2, content_w=3, half=1,
    ///     raw_start=8-1=7, max_start=9-3=6, start=min(7,6)=6,
    ///     end=min(9,9)=9 -> new_left=(6>0)=true, new_right=(9<9)=false.
    ///     Differs from the (true,true) assumption -> iterate again.
    ///   pass 2 (left only): reserved=1, content_w=4, half=2,
    ///     raw_start=8-2=6, max_start=9-4=5, start=min(6,5)=5,
    ///     end=min(9,9)=9 -> new_left=(5>0)=true, new_right=(9<9)=false.
    ///     Matches (true,false) -> converged: start=5, content_w=4, end=9.
    ///
    /// Visible slice = columns [5,9) = " 3 4" (position5=' ' separator,
    /// 6='3' from the non-current window-3 tab, 7=' ' separator, 8='4' from
    /// the CURRENT window-4 tab). Adjacent same-style chars merge: the
    /// separator/'3'/separator run is all plain `base` (win_style is the
    /// default empty PartialStyle, so non-current == base) -> one span
    /// " 3 "; '4' is styled with `win_current_style` (underscore) -> its
    /// own span. `marker_left` is true (`<` prepended); `marker_right` is
    /// false (nothing appended).
    #[test]
    fn window_list_scrolls_to_keep_current_visible_with_markers() {
        let windows = vec![
            win(0, "a", false, false, false),
            win(1, "b", false, false, false),
            win(2, "c", false, false, false),
            win(3, "d", false, false, false),
            win(4, "e", true, false, false),
        ];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I",
            "#I",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "left",
            5,
            0,
        );
        assert_eq!(
            spans,
            vec![
                (String::new(), base_style),
                ("<".to_string(), base_style),
                (" 3 ".to_string(), base_style),
                ("4".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    // ---- per-window active-pane context (SP7 Task 7, closes follow-up #71) ----

    /// Two windows with DISTINCT active-pane titles; format `#T` (pane_title
    /// alias) must show each tab's OWN window's pane_title, not the shared
    /// `ctx.pane_title` (which neither window's entry even sets here --
    /// `ctx0()` leaves it empty -- proving the value really comes from
    /// `WindowEntry::pane_title`, not the caller's ctx).
    #[test]
    fn per_tab_ctx_uses_that_windows_active_pane_title() {
        let mut w0 = win(0, "a", false, false, false);
        w0.pane_title = "alpha".to_string();
        let mut w1 = win(1, "b", true, false, false);
        w1.pane_title = "beta".to_string();
        let windows = vec![w0, w1];
        let (ws, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#T",
            "#T",
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
                ("alpha".to_string(), base_style),
                (" ".to_string(), base_style),
                ("beta".to_string(), Style { underline: true, ..base_style }),
            ]
        );
    }

    // ---- status-left visible-width length cap (SP7 Task 7, closes follow-up #69b) ----

    /// `#[fg=red]` (9 raw chars) draws zero visible columns; a cap of 3 must
    /// count only the literal text that follows, and must never split the
    /// marker itself.
    #[test]
    fn status_left_length_cap_ignores_style_marker_bytes() {
        assert_eq!(truncate_visible("#[fg=red]abcdef", 3), "#[fg=red]abc");
        // No markers at all: behaves exactly like a plain char-count cap.
        assert_eq!(truncate_visible("abcdef", 3), "abc");
        // The whole text fits under the cap already: returned unchanged,
        // marker included.
        assert_eq!(truncate_visible("#[fg=red]ab", 5), "#[fg=red]ab");
    }

    // ---- edge cases for the overflow/fit boundary (review of 128cfc0, Minor 2) ----

    /// Exactly-fits boundary: `list_width == list_avail` must take the FIT
    /// branch (`list_width <= list_avail`), not the overflow branch -- no
    /// `<`/`>` markers, no scrolling, even though there is exactly zero
    /// slack. 2 windows, bare `#I` format (1 char each), separator " " (1
    /// char): `list_width` = 1+1+1 = 3 (two tabs + one separator). left=""
    /// (width 0), right_len=0, width=3 -> `list_avail` = 3-0-0 = 3 ==
    /// `list_width`.
    #[test]
    fn overflow_boundary_exactly_fits_no_markers_no_scroll() {
        let windows = vec![win(0, "a", false, false, false), win(1, "b", true, false, false)];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "#I",
            "#I",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "left",
            3,
            0,
        );
        let joined: String = spans.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "0 1", "exact-fit boundary must not scroll or draw markers: {joined:?}");
        assert!(!joined.contains('<') && !joined.contains('>'));
    }

    /// A SINGLE window whose own tab is wider than the entire list budget
    /// still overflows correctly, including BOTH markers when the visible
    /// sliver lands strictly inside the tab (neither its start nor its end
    /// column). left="" (width 0), right_len=0, width=3; one (current)
    /// window whose format is the literal `WIDE` (4 chars, width > budget).
    ///
    /// `list_width` = 4, `list_avail` = 3-0-0 = 3, 4 > 3 -> overflow.
    /// `focus_start`=0, `focus_end`=4, `focus_centre` = 0 + (4-0)/2 = 2.
    /// Pass 1 (both markers assumed): `reserved`=2, `content_w`=3-2=1,
    /// `half`=0, `raw_start`=2-0=2, `max_start`=4-1=3, `start`=min(2,3)=2,
    /// `end`=min(2+1,4)=3 -> `new_left`=(2>0)=true, `new_right`=(3<4)=true
    /// -- matches the (true,true) assumption -> converged on pass 1:
    /// `start`=2, `content_w`=1, `end`=3.
    /// Visible slice = raw column [2,3) = `"WIDE"`'s 3rd char, `'D'`. Both
    /// markers drawn (content is strictly inside the tab) -> `"<D>"` (3
    /// columns, exactly `width`).
    #[test]
    fn single_window_wider_than_budget_overflows_with_both_markers() {
        let windows = vec![win(0, "w", true, false, false)];
        let (_, wcs) = default_partials();
        let base_style = base();
        let spans = status_spans(
            "",
            &PartialStyle::default(),
            &windows,
            &ctx0(),
            "WIDE",
            "WIDE",
            base_style,
            &PartialStyle::default(),
            &wcs,
            " ",
            "left",
            3,
            0,
        );
        assert_eq!(
            spans,
            vec![
                (String::new(), base_style),
                ("<".to_string(), base_style),
                ("D".to_string(), Style { underline: true, ..base_style }),
                (">".to_string(), base_style),
            ]
        );
    }

    // ---- status_tab_columns agrees with status_spans under overflow (review of 128cfc0) ----

    /// Same fixture as `window_list_scrolls_to_keep_current_visible_with_
    /// markers` above (5 windows, bare `#I`, width 5, current = array
    /// position 4): that test already establishes the rendered row is
    /// `"< 3 4"`-shaped -- visible raw columns [5,9), windows 0/1/2 entirely
    /// scrolled off (window 2's raw span (4,5) ends exactly at the clip
    /// start, zero overlap), window 3's `'3'` at raw col 6, window 4's `'4'`
    /// at raw col 8. `content_start_col` equals `left_width`(0) plus
    /// `marker_left`(1), i.e. 1, so `status_tab_columns` must report window
    /// 3 at absolute column `1+(6-5)=2` and window 4 at `1+(8-5)=4`, and
    /// nothing at all for windows 0/1/2, proving a click can never resolve
    /// to a scrolled-off window.
    #[test]
    fn status_tab_columns_matches_rendered_overflow_scroll() {
        let windows = vec![
            win(0, "a", false, false, false),
            win(1, "b", false, false, false),
            win(2, "c", false, false, false),
            win(3, "d", false, false, false),
            win(4, "e", true, false, false),
        ];
        let cols = status_tab_columns("", &windows, &ctx0(), "#I", "#I", " ", "left", 5, 0);
        assert_eq!(
            cols,
            vec![
                TabColumn { window_pos: 3, start: 2, end: 3 },
                TabColumn { window_pos: 4, start: 4, end: 5 },
            ]
        );
    }
}
