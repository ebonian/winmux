//! Status-line span builder (pure).
//!
//! Turns a session name + window list into the ordered `StatusSpan` sequence
//! `render::compose_back` draws on the bottom row: `[<session>] ` followed by
//! each window's `<idx>:<name><flags>` text, space-separated, with the
//! *current* window's span underlined (tmux `window-status-current-style` =
//! `underscore` default). See `docs/specs/2026-07-07-server-client-interfaces.md`
//! "status" section for the exact flag-composition rule.

use crate::render::StatusSpan;

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

/// Build the status-bar spans: `[<session_name>] ` (not underlined), then per
/// window (index order as given) a `"<idx>:<name><flags>"` span (underlined
/// iff that window is current), with a separate non-underlined single-space
/// span between windows (not after the last one).
pub fn status_spans(session_name: &str, windows: &[WindowEntry]) -> Vec<StatusSpan> {
    let mut spans = Vec::with_capacity(1 + windows.len() * 2);
    spans.push(StatusSpan { text: format!("[{session_name}] "), underline: false });
    let last_idx = windows.len().saturating_sub(1);
    for (i, w) in windows.iter().enumerate() {
        let text = format!("{}:{}{}", w.index, w.name, flags(w));
        spans.push(StatusSpan { text, underline: w.current });
        if i != last_idx {
            spans.push(StatusSpan { text: " ".to_string(), underline: false });
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(index: u32, name: &str, current: bool, last: bool, zoomed: bool) -> WindowEntry {
        WindowEntry { index, name: name.to_string(), current, last, zoomed }
    }

    #[test]
    fn single_window_current() {
        let windows = vec![win(0, "powershell", true, false, false)];
        let spans = status_spans("0", &windows);
        let got: Vec<(String, bool)> =
            spans.into_iter().map(|s| (s.text, s.underline)).collect();
        assert_eq!(
            got,
            vec![("[0] ".to_string(), false), ("0:powershell*".to_string(), true)]
        );
    }

    #[test]
    fn flags_last_and_zoomed() {
        let windows = vec![
            win(0, "vim", false, true, true),    // last + zoomed -> "-Z"
            win(1, "logs", false, false, true),  // zoomed only -> "Z"
            win(2, "shell", true, false, false), // current -> "*"
        ];
        let spans = status_spans("s", &windows);
        let got: Vec<(String, bool)> =
            spans.into_iter().map(|s| (s.text, s.underline)).collect();
        assert_eq!(
            got,
            vec![
                ("[s] ".to_string(), false),
                ("0:vim-Z".to_string(), false),
                (" ".to_string(), false),
                ("1:logsZ".to_string(), false),
                (" ".to_string(), false),
                ("2:shell*".to_string(), true),
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
        let spans = status_spans("mysess", &windows);
        let got: Vec<(String, bool)> =
            spans.into_iter().map(|s| (s.text, s.underline)).collect();
        assert_eq!(
            got,
            vec![
                ("[mysess] ".to_string(), false),
                ("0:bash-".to_string(), false),
                (" ".to_string(), false),
                ("1:powershell*".to_string(), true),
                (" ".to_string(), false),
                ("2:logs".to_string(), false),
            ]
        );
    }
}
