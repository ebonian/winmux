//! End-to-end proof of sub-project 4's copy-mode and mouse features (Task
//! 10) through the REAL release-shaped binary: a copy-mode roundtrip (enter,
//! navigate into history, select, copy, exit, paste -- the pasted text is
//! echoed by the shell), a mouse click focusing a pane, and a mouse-wheel
//! scroll entering copy mode with history.
//!
//! Harness: same pattern as `tests/e2e.rs`/`tests/e2e_sessions.rs`/
//! `tests/e2e_config.rs` -- spawn the built `winmux.exe` under a test-owned
//! ConPTY (`winmux::pty::Pty`) and decode its output via winmux's own
//! `grid::Grid`; shared helpers live in `tests/common/mod.rs`. Every test
//! uses a unique `-L` socket (`common::unique_socket`) plus a `ServerGuard`
//! Drop teardown so a failing assertion never leaks a detached server
//! process.
//!
//! Design notes (see `.superpowers/sdd/task-10-report.md` for the full
//! writeup):
//!
//! - `copy_mode_roundtrip` deliberately never hardcodes which history line
//!   ends up under the copy cursor: it fills the pane with `L<N>`-tagged
//!   lines, scrolls up a fixed (generous) number of lines, then READS the
//!   resulting screen to learn the exact marker text under the cursor at
//!   selection time, and asserts that exact text (and only that text) shows
//!   up after paste. This is robust against the precise history/pane-height
//!   arithmetic rather than trying to predict it.
//! - Selection is driven by raw keystroke BYTES exactly as a real terminal
//!   would send them (`C-a`/`C-Space`/`C-e`/`C-w` = `0x01`/`0x00`/`0x05`/
//!   `0x17`), not by calling any internal API -- this is a pure keyboard-
//!   bytes proof per the task brief ("no excuse" for this one not to be
//!   fully e2e).
//! - The mouse click-focus test proves focus moved by reading the
//!   COMPOSED TERMINAL CURSOR position (`Grid::cursor()`), not border
//!   colors: `src/server.rs`'s `render_one` places the real cursor at
//!   `focused_pane.rect.{x,y} + pane_local_cursor`, and a pane's local
//!   cursor column can never exceed `rect.w - 1` -- so the cursor's ABSOLUTE
//!   column is structurally guaranteed to fall strictly left or right of the
//!   vertical border depending on which pane is focused, regardless of
//!   prompt length. A border-color assertion was considered and rejected: a
//!   plain 2-pane side-by-side split has exactly one border column, and that
//!   column is adjacent to BOTH panes, so `pane-active-border-style` paints
//!   it green regardless of which of the two is focused -- it can't
//!   distinguish "left focused" from "right focused" the way the cursor
//!   position can.

mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{
    has_vertical_border, pump, pump_raw, screen_text, spawn_winmux_pty, unique_socket, vertical_border_col,
    wait_until, ServerGuard, COLS, ROWS,
};
use winmux::grid::Grid;

/// Removes the backing file on drop so a temp `.tmux.conf` never outlives
/// the test that wrote it (including on a failing assertion, via unwind).
/// Mirrors `tests/e2e_config.rs`'s `TempConf` exactly (not shared via
/// `common` since it's config-file-loading test setup, not a core harness
/// primitive).
struct TempConf {
    path: std::path::PathBuf,
}

impl TempConf {
    fn write(name: &str, content: &str) -> Self {
        let path = std::env::temp_dir().join(format!("winmux-e2e-{name}-{}.tmux.conf", std::process::id()));
        std::fs::write(&path, content).expect("write temp .tmux.conf");
        TempConf { path }
    }

    fn path_str(&self) -> &str {
        self.path.to_str().expect("temp conf path is valid UTF-8")
    }
}

impl Drop for TempConf {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Parse a copy-mode position indicator (`render.rs`'s `[{scroll}/{history}]`,
/// right-aligned on the pane's top row) out of a rendered row string, if
/// present: `(scroll, history_len)`. Returns `None` for a row with no
/// bracket-slash-bracket pattern that parses as two numbers -- deliberately
/// strict (both halves must parse as `u32`) so ordinary pane content
/// (unlikely to contain a literal `[N/M]`) can't produce a false positive.
fn parse_indicator(row: &str) -> Option<(u32, u32)> {
    let start = row.rfind('[')?;
    let end = row[start..].find(']')? + start;
    let inner = &row[start + 1..end];
    let mut parts = inner.split('/');
    let a: u32 = parts.next()?.parse().ok()?;
    let b: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((a, b))
}

/// `copy_mode_roundtrip`: fill a pane with numbered lines beyond one screen,
/// enter copy mode (`prefix-[`), scroll up into history, select a line
/// (`C-Space` begin, `C-e` extend to end of line, `C-w` copy-and-cancel),
/// exit back to the live pane, paste (`prefix-]`), and assert the pasted
/// text is echoed by the shell on the command line. Pure keyboard bytes
/// throughout -- see the module doc comment for why the exact selected text
/// is discovered by reading the screen rather than predicted.
#[test]
fn copy_mode_roundtrip() {
    let socket = unique_socket("copy-mode-roundtrip");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, _proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", "-"]);
    let mut grid = Grid::new(COLS, ROWS, 0);
    let mut raw: Vec<u8> = Vec::new();

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Fill the pane with 80 uniquely-tagged lines ("L1".."L80"), far beyond
    // one screen (pane is ~23 rows), so real scrollback history accumulates.
    pty.write_input(b"1..80 | ForEach-Object { \"L$_\" }\r").expect("send fill loop");
    let deadline = Instant::now() + Duration::from_secs(20);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            let lines = screen_text(&grid);
            lines.iter().any(|l| l.contains("L80")) && lines.iter().any(|l| l.contains("PS "))
        }),
        "fill loop never completed (no 'L80' + fresh prompt); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Enter copy mode: prefix (Ctrl-b, 0x02) + '['. Confirm via the
    // "[0/N]" position indicator on the pane's top row (scroll starts at 0
    // for plain '[' entry, unlike 'PPage' which pre-scrolls one page).
    pty.write_input(b"\x02[").expect("send prefix-[ (enter copy mode)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            parse_indicator(&screen_text(&grid)[0]).map(|(s, _)| s) == Some(0)
        }),
        "copy-mode entry indicator '[0/...]' never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Scroll well into history: 60 Up-arrow presses (raw CSI bytes) is far
    // more than any plausible pane height, so the copy cursor is guaranteed
    // to land on view row 0 (top of the pane) with a comfortably nonzero
    // scroll offset, deep enough that the line now at row 0 is NOT one of
    // the ~23 lines still visible on the LIVE (post-copy-mode) screen.
    let ups: Vec<u8> = b"\x1b[A".repeat(60);
    pty.write_input(&ups).expect("send 60x Up (scroll into history)");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            grid.cursor().1 == 0
        }),
        "copy cursor never reached view row 0 after scrolling up; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    let (scroll, _history) = parse_indicator(&screen_text(&grid)[0])
        .unwrap_or_else(|| panic!("no indicator after scrolling; screen:\n{}", screen_text(&grid).join("\n")));
    assert!(scroll > 0, "expected to have scrolled into history (scroll > 0), got scroll={scroll}");

    // Read exactly what's under the cursor now (row 0): an "L<digits>"
    // marker from the fill loop.
    let row0 = screen_text(&grid)[0].clone();
    let content: String = row0.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    assert!(
        content.starts_with('L') && content.len() > 1 && content[1..].chars().all(|c| c.is_ascii_digit()),
        "row 0 did not start with an 'L<digits>' fill marker; row0={row0:?}"
    );

    // Select the whole line: Home (C-a, 0x01) resets the copy cursor's
    // column to 0, C-Space (0x00) begins selection at that anchor, C-e
    // (0x05) extends the copy cursor to end-of-line, C-w (0x17) copies the
    // selection (trailing blanks trimmed by `extract_selection_text`) into
    // the default paste buffer and exits copy mode back to Normal.
    pty.write_input(&[0x01]).expect("send C-a (start of line)");
    pty.write_input(&[0x00]).expect("send C-Space (begin selection)");
    pty.write_input(&[0x05]).expect("send C-e (extend to end of line)");
    pty.write_input(&[0x17]).expect("send C-w (copy selection and cancel)");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            parse_indicator(&screen_text(&grid)[0]).is_none()
        }),
        "copy-mode indicator did not disappear after copy-selection-and-cancel; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Sanity: the exact marker isn't already sitting somewhere on the live
    // (post-copy-mode) screen -- the 60-Up scroll depth guarantees this (see
    // the comment above), but assert it explicitly so a future regression in
    // that margin fails loudly here rather than via a false-positive paste
    // assertion below.
    pump_raw(&mut grid, &rx, &mut raw);
    assert!(
        !screen_text(&grid).iter().any(|l| l.contains(&content)),
        "'{content}' unexpectedly already visible on the live screen before paste; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Paste: prefix (0x02) + ']'. `paste-buffer -p` writes the buffer's raw
    // bytes into the focused pane's stdin (plain write, no trailing Enter),
    // so the marker appears typed on the current (empty) prompt line.
    pty.write_input(b"\x02]").expect("send prefix-] (paste-buffer)");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains(&content))
        }),
        "pasted text '{content}' never appeared on screen; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup: clear the pasted (unexecuted) line, then detach.
    let _ = pty.write_input(b"\x15"); // C-u: PowerShell's kill-whole-line
    let _ = pty.write_input(b"\x02d");
}

/// `mouse_click_focuses_pane_e2e`: with `mouse on` loaded from a config file
/// (mirroring `tests/e2e_config.rs`'s `-f`-based config pattern), split into
/// two panes, explicitly focus the left one, then inject a raw SGR mouse
/// click (press + release) landing inside the right pane and assert focus
/// moved there -- see the module doc comment for why this is asserted via
/// the composed terminal cursor position rather than border color.
#[test]
fn mouse_click_focuses_pane_e2e() {
    let conf = TempConf::write("mouse-click", "set -g mouse on\n");
    let socket = unique_socket("mouse-click-focus");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, _proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", conf.path_str()]);
    let mut grid = Grid::new(COLS, ROWS, 0);

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Split left/right: prefix (0x02) + '%'.
    pty.write_input(b"\x02%").expect("send prefix-% (split-window -h)");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            has_vertical_border(&grid)
        }),
        "vertical split border never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    let border_col = vertical_border_col(&grid).expect("vertical border column must be findable once has_vertical_border is true");

    // Explicitly focus the LEFT pane: prefix + Left arrow (select-pane -L).
    // Deterministic regardless of which pane a fresh split focuses by
    // default.
    pty.write_input(b"\x02\x1b[D").expect("send prefix-Left (select-pane -L)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            grid.cursor().0 < border_col
        }),
        "cursor did not land left of the border after select-pane -L; cursor={:?} border_col={border_col}; screen:\n{}",
        grid.cursor(),
        screen_text(&grid).join("\n")
    );
    let cursor_before = grid.cursor();

    // Click well inside the right pane: SGR press then release,
    // 1-based coordinates (border_col+6, 11) -- comfortably clear of the
    // border and any status row.
    let click_x = border_col + 6;
    let click_y: u16 = 10;
    let down = format!("\x1b[<0;{};{}M", click_x + 1, click_y + 1);
    let up = format!("\x1b[<0;{};{}m", click_x + 1, click_y + 1);
    pty.write_input(down.as_bytes()).expect("send SGR mouse-down");
    pty.write_input(up.as_bytes()).expect("send SGR mouse-up");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            grid.cursor().0 > border_col
        }),
        "cursor did not move right of the border after clicking the right pane; before={cursor_before:?} after={:?} border_col={border_col}; screen:\n{}",
        grid.cursor(),
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup.
    let _ = pty.write_input(b"\x02d");
}

/// `mouse_drag_select_copies_release_text` (Task 9, SP6 parity wave 2
/// closeout): with `mouse on`, on a LIVE (non-copy-mode) unsplit pane, a
/// press-drag-release across a known word (SGR press, then an SGR
/// button-1-motion "Drag" event, then release) must: (1) enter copy mode on
/// the very first `Drag` event (SP6 Task 6's `MouseDrag1Pane -> copy-mode
/// -M` on a live pane -- proven by the `[scroll/history]` position indicator
/// appearing after the drag but before the release), (2) copy the
/// dragged-over text and exit back to copy mode's cancelled/Normal state on
/// release (SP6 Task 6/7's `MouseDragEnd1Pane -> copy-selection-and-cancel`,
/// resolved against the RELEASE-time pane -- proven by the indicator
/// disappearing again), and (3) actually paste the exact dragged word
/// (`prefix-]`), proving the selected TEXT -- not just the mode transition
/// -- was captured correctly.
///
/// Discrimination (RED verified by hand before writing this comment, per
/// the task brief's "verify each assertion actually discriminates"):
/// - Sending the press+release WITHOUT any intervening `Drag` byte sequence
///   (a plain click, no motion) does NOT enter copy mode at all (SP6 Task 6:
///   `mouse_down`'s `PendingSelect{enter_copy:true}` only becomes real copy
///   mode on the first actual `Drag` event) -- so the step-1 indicator
///   assertion below genuinely requires the `Drag` byte, not just press+release.
/// - Releasing over a DIFFERENT pane/position than the drag's own pane (an
///   `Up` event whose coordinates don't hit-test back to the dragging pane)
///   does not copy (SP6 Task 6 part (b): `mouse_up` resolves
///   `MouseDragEnd1Pane` against the pane under the pointer AT RELEASE) --
///   this test's release coordinates deliberately match the drag's own
///   (single, unsplit) pane, so this exact path is exercised; a prior
///   manual run releasing outside the pane rect left the copy-mode indicator
///   still showing (no copy, no cancel) instead of disappearing, confirming
///   the indicator-disappearance assertion in step 2 discriminates release
///   targeting.
/// - Pasting BEFORE clearing the screen would trivially "pass" even with a
///   no-op copy (the original marker line is still sitting on screen from
///   the echo that printed it) -- this test explicitly clears the screen
///   (`cls`) and confirms the marker is GONE before pasting, exactly
///   mirroring `copy_mode_roundtrip`'s own distinctness-before-paste
///   safeguard.
#[test]
fn mouse_drag_select_copies_release_text() {
    let conf = TempConf::write("mouse-drag-select", "set -g mouse on\n");
    let socket = unique_socket("mouse-drag-select");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, _proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", conf.path_str()]);
    let mut grid = Grid::new(COLS, ROWS, 0);

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Print a distinctive, easy-to-locate word on its own output line (NOT
    // the typed command line, which would also contain the substring
    // prefixed by "echo ") so we can compute exact grid coordinates to drag
    // across.
    const MARKER: &str = "dragselectme";
    pty.write_input(format!("echo {MARKER}\r").as_bytes()).expect("send echo marker");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains(MARKER) && !l.contains("echo"))
        }),
        "echoed marker line never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    let lines = screen_text(&grid);
    let row = lines
        .iter()
        .position(|l| l.contains(MARKER) && !l.contains("echo"))
        .expect("marker output line must be present (checked by wait_until above)") as u16;
    let start_col = lines[row as usize].find(MARKER).expect("marker column must be findable") as u16;
    let end_col = start_col + MARKER.len() as u16 - 1;

    // SGR press (button 1) at the marker's first character, "Drag" motion
    // (Cb = 32: button 1 + the 0x20 motion bit) to its last character, then
    // release at that same position -- 1-based SGR coordinates.
    let down = format!("\x1b[<0;{};{}M", start_col + 1, row + 1);
    let drag = format!("\x1b[<32;{};{}M", end_col + 1, row + 1);
    let up = format!("\x1b[<0;{};{}m", end_col + 1, row + 1);

    pty.write_input(down.as_bytes()).expect("send SGR mouse-down");
    // A bare press with no Drag yet must NOT enter copy mode (SP6 Task 6: a
    // plain click on a live pane only focuses -- see this test's
    // discrimination note). Give the server a moment to process the press,
    // then check once before sending the drag.
    pump(&mut grid, &rx);
    thread::sleep(Duration::from_millis(150));
    pump(&mut grid, &rx);
    assert!(
        parse_indicator(&screen_text(&grid)[0]).is_none(),
        "copy mode entered on a bare press with no Drag event yet; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    pty.write_input(drag.as_bytes()).expect("send SGR drag (button-1 motion)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            parse_indicator(&screen_text(&grid)[0]).is_some()
        }),
        "drag over a live pane never entered copy mode (no '[scroll/history]' indicator); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    pty.write_input(up.as_bytes()).expect("send SGR mouse-up (release)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            parse_indicator(&screen_text(&grid)[0]).is_none()
        }),
        "copy-mode indicator did not disappear after release (copy-selection-and-cancel never fired); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Clear the screen so the ORIGINAL marker line is gone, then confirm
    // that -- otherwise a paste-assertion "pass" could be a false positive
    // from the original still being on screen (see the discrimination note
    // above).
    pty.write_input(b"cls\r").expect("send cls");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            !screen_text(&grid).iter().any(|l| l.contains(MARKER))
        }),
        "screen still showed '{MARKER}' after cls (cleanup/clear step); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Paste: prefix (0x02) + ']'. `paste-buffer -p` writes the buffer's raw
    // bytes into the focused pane's stdin (plain write, no trailing Enter),
    // so the marker appears typed on the current (empty) prompt line.
    pty.write_input(b"\x02]").expect("send prefix-] (paste-buffer)");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains(MARKER))
        }),
        "pasted text '{MARKER}' never appeared on screen after drag-select copy; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup: clear the pasted (unexecuted) line, then detach.
    let _ = pty.write_input(b"\x15"); // C-u: PowerShell's kill-whole-line
    let _ = pty.write_input(b"\x02d");
}

/// `mouse_wheel_scrolls_history_e2e`: with `mouse on`, fill a pane's
/// scrollback, then inject an SGR wheel-up event over the (single, unsplit)
/// pane and assert the copy-mode `[scroll/history]` position indicator
/// appears with a nonzero scroll -- proving the wheel event auto-entered
/// copy mode and scrolled into history (design spec `## 4. Mouse`: "WheelUp
/// on pane -> enter copy mode -e scrolled 5").
#[test]
fn mouse_wheel_scrolls_history_e2e() {
    let conf = TempConf::write("mouse-wheel", "set -g mouse on\n");
    let socket = unique_socket("mouse-wheel-scroll");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, _proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", conf.path_str()]);
    let mut grid = Grid::new(COLS, ROWS, 0);

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Fill scrollback beyond one screen.
    pty.write_input(b"1..80 | ForEach-Object { \"W$_\" }\r").expect("send fill loop");
    let deadline = Instant::now() + Duration::from_secs(20);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            let lines = screen_text(&grid);
            lines.iter().any(|l| l.contains("W80")) && lines.iter().any(|l| l.contains("PS "))
        }),
        "fill loop never completed (no 'W80' + fresh prompt); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // No copy-mode indicator yet (still a live, unsplit pane).
    pump(&mut grid, &rx);
    assert!(
        parse_indicator(&screen_text(&grid)[0]).is_none(),
        "copy-mode indicator present before any wheel event; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Inject one SGR wheel-up event (Cb=64) over the pane interior.
    let wheel_x = 40u16;
    let wheel_y = 10u16;
    let wheel = format!("\x1b[<64;{};{}M", wheel_x + 1, wheel_y + 1);
    pty.write_input(wheel.as_bytes()).expect("send SGR wheel-up");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            matches!(parse_indicator(&screen_text(&grid)[0]), Some((s, _)) if s > 0)
        }),
        "wheel-up never produced a nonzero-scroll copy-mode indicator; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup: exit copy mode, then detach.
    let _ = pty.write_input(b"q");
    let _ = pty.write_input(b"\x02d");
}

// `app_mouse_reporting_*_e2e` (SP7 Task 19 closeout): an application
// mouse-passthrough e2e test (follow-ups #35/#72) was ATTEMPTED here and
// deliberately WITHDRAWN after investigation confirmed it cannot be built
// reliably on this platform -- not merely "hard", genuinely infeasible with
// the current ConPTY-hosted-pane architecture. Documented per the task
// brief's explicit sanction ("skip if infeasible under ConPTY and document
// why") rather than left silent.
//
// Two approaches were tried, in order:
//
// 1. **Byte-level proof of the CHILD PROCESS's receipt of a forwarded
//    click.** The task brief's own suggestion: drive a pane app that emits
//    a mouse-mode DECSET and reads its own raw stdin to prove a forwarded
//    click's exact bytes arrive. Rejected before implementation: this
//    project's e2e harness always hosts real `powershell.exe` panes, whose
//    default `PSReadLine` module binds a literal `ESC` byte to `RevertLine`
//    (clear the current input) -- any forwarded mouse byte sequence (X10 or
//    SGR, both `ESC`-prefixed) delivered to an ordinary interactive prompt
//    would be intercepted by the shell's own line editor, not echoed back
//    verbatim, before a test could observe it.
// 2. **Prove the GATING decision changed instead** (a real pane requesting
//    mouse mode should divert wheel/drag events to forwarding rather than
//    winmux's own copy-mode entry) by having a real `powershell.exe` pane
//    emit `ESC[?1000h` on its own stdout (`Write-Host`/`[Console]::Out.Write`,
//    both tried), then sending a real SGR wheel event and checking copy mode
//    did NOT open. Implemented, run, and found to FAIL in a way that traces
//    to a genuine platform-level discovery, not a test bug: **Windows
//    ConPTY does not relay `CSI ?1000h`-class mouse-mode DECSET sequences
//    from a hosted process's output through to the reader of the
//    pseudoconsole's output pipe at all** -- confirmed with a minimal,
//    permanent regression-pin probe,
//    `tests/pty_smoke.rs::mouse_decset_private_mode_is_not_relayed_by_conpty`
//    (see that test's doc comment for the full writeup, the SGR-survives
//    control that rules out "the reader just isn't working", and why this
//    retroactively explains three OTHER previously-separate "investigated
//    and gave up" notes already in `docs/follow-ups.md`: the SP4
//    abandonment of `alt_screen_wheel_sends_arrows`, follow-up #35/#72's own
//    "no live-process byte-receipt e2e proof was attempted", and follow-up
//    #53's bracketed-paste positive path being "fundamentally unprovable
//    black-box"). Since `Grid::mouse_proto` (which parses exactly this
//    sequence) can never observe a real hosted app's mouse-mode request over
//    ConPTY on this platform, approach 2 cannot be built either -- there is
//    no way for a real child process in this harness to ever get the pane
//    INTO the state ("this pane's app requested mouse reporting") the test
//    needs to exercise.
//
// The underlying feature (`forward_mouse_to_pane`/`mouse_forward_eligible`,
// `src/server/dispatch.rs`) remains covered at the unit level (synthetic
// `Grid` byte-feed proving DECSET parsing, `mouse_forward_eligible_gates_
// by_proto_and_kind` proving the gating table, `forward_mouse_to_pane_
// writes_when_pane_owns_mouse_click` proving the re-encode-and-write path)
// -- exactly the residual gap follow-up #35/#72's resolution already
// disclosed honestly, now with a confirmed root cause rather than a
// suspected one. See `CLAUDE.md`'s "Hard-won platform gotchas" section for
// the one-line summary of the finding itself.
