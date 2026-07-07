//! End-to-end proof of sub-project 3's command/config layer through the
//! REAL release binary (Task 9): a `.tmux.conf` round trip (prefix
//! remapping, style options, a config-defined `bind`, `status-left`
//! formatting, `base-index`), the `prefix-:` command prompt, and the
//! `send-keys` CLI path.
//!
//! Harness: same pattern as `tests/e2e.rs`/`tests/e2e_sessions.rs` -- spawn
//! the built `winmux.exe` under a test-owned ConPTY (`winmux::pty::Pty`) and
//! decode its output via winmux's own `grid::Grid`; shared helpers live in
//! `tests/common/mod.rs`. Every test uses a unique `-L` socket
//! (`common::unique_socket`) plus a `ServerGuard` Drop teardown so a failing
//! assertion never leaks a detached server process.
//!
//! `e2e_tmux_conf_roundtrip`'s temp `.tmux.conf` is written with CRLF line
//! endings ON PURPOSE: real Windows text editors produce CRLF, and
//! `cmd::join_continuations` stripping a trailing `\r` per physical line
//! (Task 3) is otherwise only proven by unit tests feeding literal `\r`
//! bytes -- this is the full-stack proof that a real CRLF file loads
//! correctly through `-f`/autostart/dispatch end to end.

mod common;

use std::time::{Duration, Instant};

use common::{
    drain_after_exit, has_vertical_border, pump, pump_raw, process_exited, run_cli, screen_text,
    spawn_winmux_pty, unique_socket, wait_until, ServerGuard, COLS, ROWS,
};
use winmux::grid::{Color, Grid};

/// Removes the backing file on drop so a temp `.tmux.conf` never outlives
/// the test that wrote it (including on a failing assertion, via unwind).
struct TempConf {
    path: std::path::PathBuf,
}

impl TempConf {
    fn write(name: &str, content_crlf: &str) -> Self {
        let path = std::env::temp_dir().join(format!("winmux-e2e-{name}-{}.tmux.conf", std::process::id()));
        std::fs::write(&path, content_crlf).expect("write temp .tmux.conf");
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

/// `e2e_tmux_conf_roundtrip`: a real, CRLF-terminated `.tmux.conf` loaded via
/// `-f` at autostart -- proves `prefix` remapping (`C-a`), `status-style`
/// (bg=magenta, checked as an actual grid cell color), a config-defined
/// `bind` (`V` -> `split-window -h`), `base-index` (window numbering starts
/// at 1), and `status-left` formatting (`#S` expansion) all the way through
/// the real binary, plus detach/reattach persistence under the remapped
/// prefix.
#[test]
fn e2e_tmux_conf_roundtrip() {
    let conf = TempConf::write(
        "roundtrip",
        "set -g prefix C-a\r\n\
         set -g status-style bg=magenta\r\n\
         bind V split-window -h\r\n\
         set -g base-index 1\r\n\
         set -g status-left \"[cfg-#S] \"\r\n",
    );

    let socket = unique_socket("tmux-conf-roundtrip");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", conf.path_str()]);
    let mut grid = Grid::new(COLS, ROWS);
    let mut raw: Vec<u8> = Vec::new();

    // status-left "[cfg-#S] " expands #S to the auto-assigned session name
    // ("0", the lowest unused non-negative integer for a fresh socket) and
    // base-index 1 makes the first window "1:powershell*" instead of the
    // default "0:powershell*".
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("[cfg-0] 1:powershell*"))
        }),
        "custom status-left + base-index-1 window tab never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Pane ready: echo a marker we'll look for again after detach/reattach.
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    pty.write_input(b"echo cfgmark-77\r").expect("send echo");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("cfgmark-77"))
        }),
        "echoed 'cfgmark-77' never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // status-style bg=magenta -> every status-row cell's background is color
    // index 5 (the tmux 8-color palette slot for "magenta"; see
    // src/style.rs's named-color table). Checked on the actual grid cell,
    // not just the raw bytes, to prove the style actually reaches the
    // renderer's SGR output and gets parsed back correctly by our own vte
    // emulator.
    let status_row = ROWS - 1; // status-position defaults to "bottom" (not overridden)
    assert_eq!(
        grid.cell(0, status_row).style.bg,
        Color::Idx(5),
        "status row background was not magenta (Idx(5)); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Config-defined `bind V split-window -h` under the remapped prefix
    // C-a (0x01): a vertical border appears, same as the hardcoded `%`
    // binding would produce.
    pty.write_input(b"\x01V").expect("send C-a V (config bind)");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            has_vertical_border(&grid)
        }),
        "vertical split border never appeared after C-a V; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Detach under the remapped prefix: C-a d (0x01 'd').
    pty.write_input(b"\x01d").expect("send C-a d (detach)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            process_exited(proc_raw)
        }),
        "client did not exit within 10s after C-a d; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    drain_after_exit(&mut grid, &rx, &mut raw);

    let tail = String::from_utf8_lossy(&raw);
    assert!(
        tail.contains("[detached (from session 0)]"),
        "raw output did not contain the detach message; got:\n{tail:?}"
    );

    // Reattach: `winmux -L <sock> attach` (no `-t`, resolves to the sole
    // session) -- the pre-detach marker is still on screen.
    let (mut pty_b, proc_b, rx_b) = spawn_winmux_pty(&["-L", &socket, "attach"]);
    let mut grid_b = Grid::new(COLS, ROWS);
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid_b, &rx_b);
            screen_text(&grid_b).iter().any(|l| l.contains("cfgmark-77"))
        }),
        "reattached client did not show persisted 'cfgmark-77'; screen:\n{}",
        screen_text(&grid_b).join("\n")
    );

    // Best-effort cleanup (also under the remapped prefix).
    let _ = pty_b.write_input(b"\x01d");
    let _ = wait_until(Instant::now() + Duration::from_secs(10), || {
        pump(&mut grid_b, &rx_b);
        process_exited(proc_b)
    });
}

/// `e2e_command_prompt`: with config disabled (`-f -`, default prefix
/// `Ctrl-b`/base-index 0), `prefix-:` opens the `:` command-prompt line
/// editor (proven by the status row itself becoming the editor's `:` label),
/// and typing `rename-window meta` + Enter dispatches through the same
/// pipeline as a keybinding or the CLI, renaming the current window.
#[test]
fn e2e_command_prompt() {
    let socket = unique_socket("command-prompt");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", "-"]);
    let mut grid = Grid::new(COLS, ROWS);

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:powershell*"))
        }),
        "initial window tab never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Ctrl-b : opens the command prompt -- the status row's content becomes
    // exactly ":" (the prompt label, empty buffer) rather than the normal
    // window-list/status content.
    pty.write_input(b"\x02:").expect("send command-prompt open");
    let status_row = (ROWS - 1) as usize;
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid)[status_row].trim_end() == ":"
        }),
        "command-prompt ':' editor line never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    pty.write_input(b"rename-window meta\r").expect("send rename-window command");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("0:meta*"))
        }),
        "status row never showed '0:meta*' after command-prompt rename-window; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup.
    let _ = pty.write_input(b"\x02d");
    let _ = wait_until(Instant::now() + Duration::from_secs(10), || {
        pump(&mut grid, &rx);
        process_exited(proc_raw)
    });
}

/// `e2e_send_keys_cli`: a plain out-of-process `send-keys` CLI invocation
/// (no ConPTY -- exactly how a script would drive winmux) reaches the pane
/// the attached client is watching, proving the CLI-argv entry point into
/// the same command dispatcher exercised above via keybindings and the `:`
/// prompt.
#[test]
fn e2e_send_keys_cli() {
    let socket = unique_socket("send-keys-cli");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", "-"]);
    let mut grid = Grid::new(COLS, ROWS);

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // From OUTSIDE: a plain std::process::Command, no ConPTY at all.
    let out = run_cli(&socket, &["send-keys", "-t", "0", "echo e2e-ok", "Enter"]);
    assert_eq!(out.status.code(), Some(0), "send-keys CLI failed: {out:?}");

    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump(&mut grid, &rx);
            screen_text(&grid).iter().any(|l| l.contains("e2e-ok"))
        }),
        "attached screen never showed 'e2e-ok' after send-keys CLI; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup.
    let _ = pty.write_input(b"\x02d");
    let _ = wait_until(Instant::now() + Duration::from_secs(10), || {
        pump(&mut grid, &rx);
        process_exited(proc_raw)
    });
}
