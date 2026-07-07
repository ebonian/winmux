//! Status-line span builder (pure).
//!
//! Turns a pre-expanded `status-left` string + window list + the status
//! styling options into the ordered `(text, Style)` span sequence
//! `render::compose_back` draws on the status row: the left string, followed
//! by each window's `<idx>:<name><flags>` tab, space-separated. Every span
//! carries a FULLY RESOLVED [`grid::Style`](crate::grid::Style) (SP3 Task 8,
//! superseding the old `StatusSpan { text, underline }` bool):
//!
//! - the left string and the single-space separators are drawn with `base`
//!   (the `status-style` option applied to the default style);
//! - a NON-current window tab is `window-status-style` layered over `base`;
//! - the CURRENT window tab is `window-status-current-style` layered over
//!   `base` — NOT over `window-status-style` (tmux layers the current style
//!   over `status-style` directly, so an fg set only in
//!   `window-status-style` does not leak into the current tab).
//!
//! With default options (`window-status-style` empty,
//! `window-status-current-style` = `underscore`) this reproduces the SP2
//! behavior exactly: every span equals `base` except the current tab, which
//! is `base` + underline. See `docs/specs/2026-07-07-server-client-interfaces.md`
//! "status" (flag-composition rule) and the SP3 contract's `## render-styles`
//! section.

use crate::grid::Style;
use crate::style::PartialStyle;

pub struct WindowEntry {
    pub index: u32,
    pub name: String,
    pub current: bool,
    pub last: bool,
    pub zoomed: bool,
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

/// Build the status-bar spans: `left` (the already-expanded, already-length-
/// capped `status-left` text) styled `base`, then per window (index order as
/// given) a `"<idx>:<name><flags>"` span styled per the layering rules in
/// the module docs, with a separate base-styled single-space span between
/// windows (not after the last one).
pub fn status_spans(
    left: &str,
    windows: &[WindowEntry],
    base: Style,
    win_style: &PartialStyle,
    win_current_style: &PartialStyle,
) -> Vec<(String, Style)> {
    let non_current = win_style.apply_to(base);
    let current = win_current_style.apply_to(base);
    let mut spans = Vec::with_capacity(1 + windows.len() * 2);
    spans.push((left.to_string(), base));
    let last_idx = windows.len().saturating_sub(1);
    for (i, w) in windows.iter().enumerate() {
        let text = format!("{}:{}{}", w.index, w.name, flags(w));
        spans.push((text, if w.current { current } else { non_current }));
        if i != last_idx {
            spans.push((" ".to_string(), base));
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Color;
    use crate::style::parse_style;

    fn win(index: u32, name: &str, current: bool, last: bool, zoomed: bool) -> WindowEntry {
        WindowEntry { index, name: name.to_string(), current, last, zoomed }
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

    #[test]
    fn single_window_current() {
        let windows = vec![win(0, "powershell", true, false, false)];
        let (ws, wcs) = default_partials();
        let spans = status_spans("[0] ", &windows, base(), &ws, &wcs);
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
        let spans = status_spans("[s] ", &windows, base(), &ws, &wcs);
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
        let spans = status_spans("[mysess] ", &windows, base(), &ws, &wcs);
        assert_eq!(
            spans,
            vec![
                ("[mysess] ".to_string(), base()),
                ("0:bash-".to_string(), base()),
                (" ".to_string(), base()),
                ("1:powershell*".to_string(), Style { underline: true, ..base() }),
                (" ".to_string(), base()),
                ("2:logs".to_string(), base()),
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
        let spans = status_spans("[x] ", &windows, base(), &ws, &wcs);
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
}
