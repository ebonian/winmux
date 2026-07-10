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
    let mut grid = Grid::new(COLS, ROWS, 0);
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
    let mut grid_b = Grid::new(COLS, ROWS, 0);
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

/// `user_tmux_conf_loads_without_errors` (Task 9, SP6 parity wave 2 closeout):
/// the exact `.tmux.conf` a real winmux user brought over from tmux
/// (`tests/fixtures/user.tmux.conf` -- remapped prefix, `|`/`-` split
/// rebinds, mouse on, custom status styling, `@`-user options, a `-T
/// copy-mode-vi` unbind, etc.) loads via `-f` at server-startup autostart
/// with ZERO config errors, AND the loaded config actually TAKES EFFECT end
/// to end: the fixture's remapped prefix (`C-a`, not the default `C-b`)
/// combined with its `bind | split-window -h` rebind together produce a
/// vertical split border -- proof the whole file applied, not just that it
/// parsed. `tests/server_proto.rs`'s `user_config_loads_clean` already
/// proves the zero-errors half via the headless `source-file` runtime path;
/// this is the full-stack, real-binary, `-f`-at-startup proof, plus the
/// "did it actually take effect" half that a parse-only check can't cover.
///
/// Discrimination: run this test against a build with the fixture's `bind
/// C-a` line REMOVED (i.e. only `set -g prefix C-a` without the
/// `unbind C-a` / `bind C-a send-prefix` lines still present, prefix still
/// remaps) -- verified by hand that sending the DEFAULT prefix (`C-b`,
/// `0x02`) instead of `C-a` here does NOT produce a split border (the
/// config's remapped prefix wins, so `0x02` is just forwarded to the shell
/// as an ordinary keystroke) -- i.e. the split-border assertion below
/// genuinely requires BOTH the remapped prefix AND the config's `|` rebind,
/// not just one or the other.
#[test]
fn user_tmux_conf_loads_without_errors() {
    let socket = unique_socket("user-conf");
    let _guard = ServerGuard { socket: socket.clone() };

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/user.tmux.conf");
    let fixture_str = fixture.to_str().expect("fixture path is valid UTF-8");

    let (mut pty, _proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", fixture_str]);
    let mut grid = Grid::new(COLS, ROWS, 0);
    let mut raw: Vec<u8> = Vec::new();

    // Poll for the initial status row's normal window-tab content
    // ("powershell", from the fixture's custom window-status-format ' #I #W
    // #F ') to appear -- while asserting, on every poll, that no line on
    // screen ever contains "error" (case-insensitive). A config-load error
    // would replace the ENTIRE status-row content with a one-shot
    // "config: N error(s), see server.log" transient message
    // (`src/server.rs`'s `finish_attach`/`pending_config_message`), which
    // would win this race against the normal status content appearing --
    // this loop fails FAST (inside the closure) the moment any such banner
    // is seen, rather than waiting out the full timeout.
    let mut saw_normal_status = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            let lines = screen_text(&grid);
            assert!(
                !lines.iter().any(|l| l.to_lowercase().contains("error")),
                "a config-error banner appeared on screen while loading the user's .tmux.conf; screen:\n{}",
                lines.join("\n")
            );
            if lines.iter().any(|l| l.contains("powershell")) {
                saw_normal_status = true;
            }
            saw_normal_status
        }),
        "normal status-row content ('powershell' window tab) never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    // Belt-and-braces: also check the raw byte stream (covers any error text
    // that scrolled off the visible grid before a poll caught it).
    let raw_text = String::from_utf8_lossy(&raw);
    assert!(
        !raw_text.to_lowercase().contains("error"),
        "raw output stream contained \"error\" while loading the user's .tmux.conf; got:\n{raw_text:?}"
    );

    // Pane ready.
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // The fixture's remapped prefix (C-a, 0x01) plus its `bind | split-window
    // -h` rebind: sending C-a | must produce a vertical split border. Under
    // the DEFAULT prefix (C-b) this exact byte sequence would just forward
    // "C-a" then a literal "|" character to the shell -- no border -- so
    // this assertion genuinely discriminates the remapped-prefix config
    // having applied (see the discrimination note in this test's doc
    // comment).
    pty.write_input(b"\x01|").expect("send C-a | (config bind: split-window -h)");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            has_vertical_border(&grid)
        }),
        "vertical split border never appeared after C-a | (config prefix + bind); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Best-effort cleanup: detach under the remapped prefix (the fixture has
    // no explicit `d` rebind, so the default `d` -> detach still applies).
    let _ = pty.write_input(b"\x01d");
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
    let mut grid = Grid::new(COLS, ROWS, 0);

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

/// True if any pane row shows a `[N/M]` copy-mode position indicator
/// (mirrors `tests/e2e_copy_mouse.rs`'s `parse_indicator`, duplicated here
/// rather than shared since it's a small, self-contained string check and
/// the two files don't otherwise share test-specific helpers).
fn has_scroll_indicator(grid: &Grid) -> bool {
    screen_text(grid).iter().any(|l| {
        let Some(start) = l.rfind('[') else { return false };
        let Some(end) = l[start..].find(']').map(|i| i + start) else { return false };
        let inner = &l[start + 1..end];
        let mut parts = inner.split('/');
        let a = parts.next().and_then(|p| p.parse::<u32>().ok());
        let b = parts.next().and_then(|p| p.parse::<u32>().ok());
        a.is_some() && b.is_some() && parts.next().is_none()
    })
}

/// `config_sp7_surface_takes_effect` (SP7 Task 19 closeout): a single
/// `.tmux.conf`, loaded via `-f` at real server startup, exercising several
/// distinct SP7 surface additions together -- proving each loads clean AND
/// actually takes effect through the real ConPTY client, not just that it
/// parses:
///
/// - **`setw` window-scoped `set`** (closes follow-up #26 for the write
///   side), proven in TWO steps because a CONFIG-loaded `setw` and a
///   RUNTIME one resolve differently by design (follow-up #26: "headless
///   \[CLI/config\] calls with no acting session fall back to the global
///   table"): (1) the config's own `setw pane-base-index 5` (no `-g`, no
///   acting window at load time) lands as a GLOBAL default -- proven by
///   window 0 showing `P5` immediately after load; (2) a SECOND `setw
///   pane-base-index 9` issued at RUNTIME via the `:` command prompt (which
///   DOES have an acting client bound to window 0) lands as a LOCAL
///   override on window 0 ONLY -- proven by window 0 flipping to `P9` while
///   a brand new window (`prefix c`, created afterward) shows `P5` (the
///   untouched global default from the config), not `P9`.
/// - **The general `#{?...}` format engine + `#{==:...}` string comparison**
///   (closes follow-ups #27/#70): `status-right` uses
///   `#{?#{==:#F,*},NOZ,ZOOMED}` -- a conditional whose condition is itself
///   a nested string-comparison expression, evaluated against LIVE data
///   (`#F`/`window_flags`) that changes at runtime (`prefix z` toggles
///   zoom), proven by the rendered status text actually flipping from
///   `NOZ` to `ZOOMED` after a real zoom keystroke, not just rendering some
///   static text once.
/// - **A config `bind` on a mouse pseudo-key name** (closes follow-up #57):
///   `bind -T root WheelUpPane display-message "WU-OK"` replaces the
///   default wheel-up-enters-copy-mode action -- proven two ways at once: a
///   real SGR wheel-up event shows the `WU-OK` message (the custom command
///   ran) AND does NOT show the `[N/M]` copy-mode indicator (the default
///   action it replaced did NOT also run).
/// - **`set-clipboard`** (closes follow-up #55): `set -g set-clipboard on`
///   from the CONFIG FILE (not just a runtime `set`, which
///   `tests/server_proto.rs::osc52_emitted_to_client_on_copy` already
///   covers) makes a real copy-mode copy emit the OSC 52 sequence on the
///   raw byte stream this ConPTY client actually receives.
#[test]
fn config_sp7_surface_takes_effect() {
    let conf = TempConf::write(
        "sp7-surface",
        "set -g mouse on\n\
         set -g set-clipboard on\n\
         setw pane-base-index 5\n\
         set -g status-right \"#{?#{==:#F,*},NOZ,ZOOMED}-P#P\"\n\
         bind -T root WheelUpPane display-message \"WU-OK\"\n",
    );
    let socket = unique_socket("sp7-surface");
    let _guard = ServerGuard { socket: socket.clone() };

    let (mut pty, proc_raw, rx) = spawn_winmux_pty(&["-L", &socket, "-f", conf.path_str()]);
    let mut grid = Grid::new(COLS, ROWS, 0);
    let mut raw: Vec<u8> = Vec::new();

    // Initial render: unzoomed ("NOZ") + window-scoped pane-base-index 5
    // ("P5") on window 0's sole pane -- proves the conditional/comparison
    // engine AND the setw write both took effect from the config alone.
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("NOZ-P5"))
        }),
        "expected status-right 'NOZ-P5' (conditional + setw pane-base-index) never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Zoom the sole pane (prefix z): the SAME format now reads "ZOOMED"
    // instead of "NOZ" -- the conditional re-evaluates against LIVE
    // window_flags, not a one-shot render-time snapshot.
    pty.write_input(b"\x02z").expect("send prefix-z (resize-pane -Z)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("ZOOMED-P5"))
        }),
        "status-right did not flip to 'ZOOMED-P5' after prefix-z; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    // Unzoom again for the rest of the test.
    pty.write_input(b"\x02z").expect("send prefix-z (un-zoom)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("NOZ-P5"))
        }),
        "status-right did not flip back to 'NOZ-P5' after un-zoom; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // A CONFIG-loaded `setw` (no acting client/window at load time) falls
    // back to the GLOBAL table (documented follow-up #26 behavior: headless
    // calls with no acting session/window preserve pre-Task-6 global-only
    // semantics) -- so `P5` above is currently a GLOBAL default, not yet a
    // per-window override. To prove genuine window scoping, run `setw`
    // again at RUNTIME, through the `:` command prompt, which DOES have a
    // real acting client bound to window 0 -- this time it must land as a
    // LOCAL override on window 0 only.
    pty.write_input(b"\x02:").expect("send prefix-: (open command prompt)");
    let status_row = (ROWS - 1) as usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid)[status_row].trim_end() == ":"
        }),
        "command-prompt ':' editor line never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    pty.write_input(b"setw pane-base-index 9\r").expect("send runtime setw pane-base-index 9");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("NOZ-P9"))
        }),
        "window 0's status-right did not pick up the runtime 'setw pane-base-index 9' local \
         override (still expected to show 'NOZ-P9'); screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // New window (prefix c): its OWN pane starts at the GLOBAL value (5,
    // from the config -- see above), NOT window 0's fresh LOCAL override
    // (9) -- proves the runtime `setw` really was window-scoped to window
    // 0 alone, not a global write in disguise.
    pty.write_input(b"\x02c").expect("send prefix-c (new-window)");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("NOZ-P5"))
        }),
        "new window's status-right never showed 'NOZ-P5' (global pane-base-index, NOT window 0's \
         local override of 9); screen:\n{}",
        screen_text(&grid).join("\n")
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("PS "))
        }),
        "PowerShell prompt never appeared in the new window; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // Config-bound mouse action: SGR wheel-up over this LIVE pane runs the
    // config's `display-message "WU-OK"` INSTEAD OF the default
    // wheel-up-enters-copy-mode action.
    let wheel = "\x1b[<64;40;10M";
    pty.write_input(wheel.as_bytes()).expect("send SGR wheel-up");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.contains("WU-OK"))
        }),
        "config-bound WheelUpPane -> display-message 'WU-OK' never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    assert!(
        !has_scroll_indicator(&grid),
        "copy-mode indicator appeared even though WheelUpPane was rebound away from the default \
         copy-mode-entry action; screen:\n{}",
        screen_text(&grid).join("\n")
    );

    // set-clipboard: a real copy-mode copy of a known line emits OSC 52 on
    // the raw byte stream (same payload `tests/server_proto.rs`'s
    // `osc52_emitted_to_client_on_copy` proves at the headless/runtime-`set`
    // level; this proves the SAME option, set from a CONFIG FILE, reaches a
    // real attached ConPTY client).
    pty.write_input(b"echo hello123\r").expect("send echo hello123");
    let deadline = Instant::now() + Duration::from_secs(15);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            screen_text(&grid).iter().any(|l| l.trim_end() == "hello123")
        }),
        "echoed 'hello123' line never appeared; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    let target_row = screen_text(&grid).iter().position(|l| l.trim_end() == "hello123").unwrap() as u16;

    pty.write_input(b"\x02[").expect("send prefix-[ (enter copy mode)");
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            has_scroll_indicator(&grid)
        }),
        "copy-mode indicator never appeared after prefix-[; screen:\n{}",
        screen_text(&grid).join("\n")
    );
    pump_raw(&mut grid, &rx, &mut raw);
    let entry_row = grid.cursor().1;
    if entry_row > target_row {
        pty.write_input(&vec![0x10u8; (entry_row - target_row) as usize]).expect("send C-p x n (cursor-up)");
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(
            wait_until(deadline, || {
                pump_raw(&mut grid, &rx, &mut raw);
                grid.cursor().1 == target_row
            }),
            "cursor never reached the 'hello123' row; screen:\n{}",
            screen_text(&grid).join("\n")
        );
    }
    pty.write_input(&[0x01]).expect("send C-a (start-of-line)");
    pty.write_input(&[0x00]).expect("send C-Space (begin-selection)");
    pty.write_input(&[0x05]).expect("send C-e (end-of-line)");
    pty.write_input(&[0x17]).expect("send C-w (copy-selection-and-cancel)");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_until(deadline, || {
            pump_raw(&mut grid, &rx, &mut raw);
            raw.windows(b"\x1b]52;c;aGVsbG8xMjM=\x07".len()).any(|w| w == b"\x1b]52;c;aGVsbG8xMjM=\x07")
        }),
        "OSC 52 clipboard sequence for 'hello123' never appeared in the raw output stream; \
         raw tail: {:?}",
        String::from_utf8_lossy(&raw[raw.len().saturating_sub(400)..])
    );

    // Best-effort cleanup.
    let _ = pty.write_input(b"\x02d");
    let _ = wait_until(Instant::now() + Duration::from_secs(10), || {
        pump_raw(&mut grid, &rx, &mut raw);
        process_exited(proc_raw)
    });
}
