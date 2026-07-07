//! End-to-end test: spawn the built winmux binary inside a ConPTY (using
//! winmux's OWN pty module), drive it via keystrokes, and assert on the
//! decoded screen by feeding its output into winmux's OWN grid emulator.
//!
//! Flow: wait for the status bar → split vertically (Ctrl-b %) → confirm a
//! border column appears → kill the new pane (Ctrl-b x, y) → confirm the
//! border disappears → `exit` the last shell → assert winmux exits cleanly.
//!
//! Harness helpers (`pump`, `screen_text`, `wait_until`, `has_vertical_border`,
//! `process_exited`, `spawn_winmux_pty`, `ServerGuard`, ...) live in
//! `tests/common/mod.rs`, shared with `tests/e2e_sessions.rs` (Task 9).

mod common;

use std::time::{Duration, Instant};

use common::{
    has_vertical_border, pump, process_exited, screen_text, spawn_winmux_pty, wait_until,
    ServerGuard,
};
use winmux::grid::Grid;

#[test]
fn e2e_split_kill_exit() {
    // Unique -L socket per test run so no server outlives this test and no
    // two runs collide on the default pipe name.
    let socket = format!("e2e-mvp-{}", std::process::id());
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, proc_raw, rx) = spawn_winmux_pty(&["-L", &socket]);

    let mut grid = Grid::new(common::COLS, common::ROWS);

    // 1. Status bar appears. Bare `winmux` now attaches to an auto-named
    //    session ("0", the lowest unused non-negative integer) instead of
    //    the MVP's hardcoded "[winmux]" — check for the window tab instead,
    //    which is stable regardless of the session name.
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:powershell*"))
        }),
        "status-bar window tab '0:powershell*' never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 1b. The pane's PowerShell prompt appears and PSReadLine loaded. winmux
    //     itself runs under THIS test's ConPTY, so winmux's own stdio IS a
    //     console and its panes take the no-STARTF_USESTDHANDLES path — the
    //     same path interactive use takes. If spawn nulled the pane's std
    //     handles, PSReadLine would fail its GetStdHandle probe and print
    //     "Cannot load PSReadline module. Console is running without
    //     PSReadline."
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt 'PS ' never appeared in the pane; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    assert!(
        !screen_text(&grid)
            .iter()
            .any(|l| l.to_lowercase().contains("psreadline")),
        "pane PowerShell reported a PSReadline failure; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 2. Split vertically: Ctrl-b %  → a `│` border column appears.
    pty.write_input(b"\x02%").expect("send split");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            has_vertical_border(&grid)
        }),
        "vertical split border '│' never appeared after Ctrl-b %; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 3. Kill the new (focused) pane: Ctrl-b x → wait for the confirm prompt →
    //    y. Waiting for the prompt guarantees winmux armed confirm mode before
    //    the `y` arrives, so `y` is consumed as confirmation, not forwarded.
    pty.write_input(b"\x02x").expect("send kill request");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("kill-pane"))
        }),
        "kill-pane confirm prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    pty.write_input(b"y").expect("send confirm");

    // 4. Border disappears once the pane is gone.
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            !has_vertical_border(&grid)
        }),
        "vertical split border '│' never disappeared after kill; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // 5. Exit the last remaining shell → winmux exits cleanly.
    pty.write_input(b"exit\r").expect("send exit");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx); // keep the failure screen-dump current
            process_exited(proc_raw)
        }),
        "winmux process did not exit within 15s after 'exit'; screen:\n{}",
        screen_text(&grid).join("\n")
    );
}
