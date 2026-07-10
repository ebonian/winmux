# winmux — a tmux alternative for Windows / PowerShell

> A native terminal multiplexer for Windows: split panes, manage sessions, detach
> and reattach (including over SSH), and reuse your existing `.tmux.conf`.

## Vision

winmux aims to be a full **tmux alternative for Windows**, working the way tmux
does: it runs **inside** your existing terminal (Windows Terminal or any
VT-capable console) and draws its panes, borders, styling, and status bar using
ANSI/VT escape sequences. It is **not** its own GUI terminal window.

Target parity includes:

- Multiplexing multiple terminals into panes within a window
- Managing multiple sessions and windows
- Detach / attach — sessions keep running in the background after you disconnect,
  including after an SSH session drops
- Reading real `.tmux.conf` configuration so existing tmux users can port their
  config, keybindings, and styling

## Core decisions

| Decision | Choice | Rationale |
|---|---|---|
| Intent | Serious, full-parity tool | A real tmux alternative, not a prototype/toy. |
| Language | **Rust** | Strong Windows/ConPTY bindings (`windows-rs`), mature VT parsing (`vte`), single self-contained `.exe`, no runtime. Same lineage as WezTerm/Alacritty. |
| Render model | **Draw into the host terminal** (true tmux model) | Runs inside any VT-capable console; draws panes/borders/status bar with ANSI. Required for the attach-over-SSH story; keeps it lightweight. |
| Config | **Core `.tmux.conf` parity** first | Parse real `.tmux.conf` and support the commonly-used subset; advanced formats/`if-shell`/hooks come later. |
| Platform primitive | **ConPTY** (Pseudo Console API) | The Windows pseudo-terminal API (Win10 1809+). The enabling technology for a native multiplexer. Confirmed available on this machine (Win11 build 26200). |

## Enabling technology: ConPTY

Historically Windows had no real pseudo-terminal, which is why tmux/screen never
ran natively. **ConPTY** (introduced in Windows 10 1809, 2018) provides exactly
what a multiplexer needs: spawn a shell attached to a pseudo-console, feed it
input, read its VT output as a stream, and resize it. It is the same API Windows
Terminal uses. winmux builds directly on it.

## Client / server architecture (whole project)

Like tmux, winmux separates a background **server** from thin **clients**:

- The **server** is a detached background process on the Windows host. It owns the
  ConPTY handles and all shell processes, so it survives client disconnect.
- A **client** attaches to the server over a **named pipe** (the Windows analogue
  of tmux's Unix domain socket), draws the UI, and forwards input.
- SSH story: you SSH into the Windows host and run a thin client that attaches to
  the already-running server. SSH drops → client dies → server + shells keep
  running → reconnect and reattach.

The MVP (sub-project 1) proved the hard rendering/ConPTY problems in a single
in-process form (no separate server). **Sub-project 2 has since delivered the
real server/client split**: one binary, `winmux.exe`, plays three roles
selected by argv — CLI (parse args, connect or autostart), attached client
(thin: forwards stdin, writes server-composed VT bytes to stdout), and hidden
`__server` (the background event loop, spawned detached so it survives the
launching console closing). Client and server talk over a Windows named pipe,
`\\.\pipe\winmux-<username>-<socket-name>`, where `<socket-name>` defaults to
`default` and is overridden by tmux's own `-L <name>` flag. This is the
Windows analogue of tmux's Unix domain socket, and gives the same SSH story:
SSH drops → client dies → server + shells keep running → reconnect and
reattach.

## Decomposition into sub-projects

This is too large for a single spec. It is decomposed into sequential
sub-projects, each with its own spec → plan → build cycle. Each builds on the
previous and is independently useful.

| # | Sub-project | Delivers |
|---|---|---|
| **1** | **Multiplexing MVP** — DELIVERED | ConPTY-spawned PowerShell panes, VT parsing, a split-tree layout, panes + borders + status bar drawn into the host terminal, prefix-key handling, split/switch/resize/close panes. **One session, one window, one attached client, no detach.** |
| **2** | **Server/client split + sessions + detach** — DELIVERED | Daemonized background server, named-pipe client↔server protocol, multiple sessions and windows, detach/attach, tmux CLI subset, survives SSH disconnect. |
| **3** | **Command layer + config compatibility** — DELIVERED | One command dispatcher (`cmd`/`options`/`keys`/`style`/`bindings` modules) powering all four entry points: keybindings, the `winmux <cmd>` CLI, the `prefix-:` command prompt, and a real `.tmux.conf`/`-f`/`.winmux.conf` config loader — `set-option`/`set`, `bind-key`/`bind`, `unbind-key`, styles/colors, `status-left`/`status-right` formatting, `base-index`/`pane-base-index`, prefix remapping, and option-driven status/border/message styling. |
| **4** | **Parity polish** — DELIVERED | Copy mode (emacs+vi tables, selection incl. rectangle, search), paste buffers, mouse (click/drag/wheel/status), layout presets + swap/rotate, break/move/find-window, choose-tree + display-panes overlays, escape-time, automatic-rename. |

**Build order:** sub-project 1 first (visible, motivating, proves the hardest
rendering/ConPTY problems), then 2 → 3 → 4.

## PROJECT COMPLETE

All four planned sub-projects are delivered as of sub-project 4's merge. The
originally scoped tmux-alternative feature set — multiplexing, server/client
split with detach/attach, real `.tmux.conf` config compatibility, and the
parity-polish long tail (copy mode, mouse, layouts, overlays) — is done.
`tests/e2e_copy_mouse.rs` is the full-stack proof for sub-project 4's two
biggest features (copy-mode roundtrip and mouse), driving the real binary
under a test-owned ConPTY exactly like the sub-project 1–3 e2e suites.
Known/deferred gaps (documented, non-blocking divergences from real tmux)
live in `docs/follow-ups.md`.

A further sub-project — TPM-style plugin support — has been researched but
is future work, not part of the planned scope above: see
[`docs/superpowers/plans/2026-07-08-tpm-plugin-support.md`](superpowers/plans/2026-07-08-tpm-plugin-support.md).

## SP6: tmux parity wave 2 — DELIVERED

A second, narrower parity-hardening pass over sub-project 4's delivered
surface — real-world tmux-config fidelity and mouse/focus fixes discovered
after the original four sub-projects shipped, not a new feature area, so it
extends the existing four interface-contract files rather than adding a
fifth. Plan:
[`docs/superpowers/plans/2026-07-10-sp6-tmux-parity-wave2.md`](superpowers/plans/2026-07-10-sp6-tmux-parity-wave2.md).

Delivered:

- Mouse drag-state lifecycle fixes — stale `MouseDrag` state surviving the
  overlay guard, the status-row short-circuit, and a border-miss early
  return (#64) — plus a fix for leftward/upward border drags, which had been
  a silent no-op (#66)
- Config-compatibility batch: `setw` alias, space-delimited style
  attributes, `@`-user options, `bind`/`unbind -T copy-mode`/`-vi`, `~`
  expansion in `source-file`, 13 new options — a real user `.tmux.conf`
  (`tests/fixtures/user.tmux.conf`) now loads with zero errors, proven both
  headlessly (`tests/server_proto.rs`'s `user_config_loads_clean`) and
  through the real binary under `-f` at startup
  (`tests/e2e_config.rs`'s `user_tmux_conf_loads_without_errors`)
- Directional-navigation edge wrap plus a real per-pane tmux `active_point`
  MRU for `select-pane -L/-R/-U/-D` (closes #65)
- Status-line rendering: `status-justify` (4 values), per-side
  `status-left`/`status-right` style, `window-status-format`/
  `-current-format` with inline `#[...]` styles, `window-status-separator`,
  a width-stable default window tab
- `swap-window` with relative wrapping targets and tmux's `-d` focus
  semantics
- Copy-mode mouse feel: click purity, release-pane targeting (resolved
  against the pane under the pointer AT release), drag-on-a-live-pane
  auto-entering copy mode, edge autoscroll, word/line drag extension with a
  3-class word model and a `word-separators` option
- `choose-tree`: a real session/window tree with expand/collapse, an
  active-item default selection, and a live preview box (tmux sizing/box
  chrome, `v` toggle between off/BIG/normal)
- `clock-mode` (`prefix-t`, `clock-mode-colour`/`-style`, any key or mouse
  exits)
- Half-border active-pane indication for exactly-two-pane windows, plus
  `pane-border-indicators`

New follow-up tickets from this wave: #67-#74. Resolved this wave: #64,
#65, #66. See `docs/follow-ups.md` for the full accounting.

## Specs

- [`specs/2026-07-06-multiplexing-mvp-design.md`](specs/2026-07-06-multiplexing-mvp-design.md) — sub-project 1 (delivered); companion interface contract [`specs/2026-07-06-mvp-interfaces.md`](specs/2026-07-06-mvp-interfaces.md)
- [`specs/2026-07-07-server-client-design.md`](specs/2026-07-07-server-client-design.md) — sub-project 2 (delivered); companion interface contract [`specs/2026-07-07-server-client-interfaces.md`](specs/2026-07-07-server-client-interfaces.md)
- [`specs/2026-07-07-command-config-design.md`](specs/2026-07-07-command-config-design.md) — sub-project 3 (delivered); companion interface contract [`specs/2026-07-07-command-config-interfaces.md`](specs/2026-07-07-command-config-interfaces.md)
- [`specs/2026-07-07-parity-polish-design.md`](specs/2026-07-07-parity-polish-design.md) — sub-project 4 (delivered); companion interface contract [`specs/2026-07-07-parity-polish-interfaces.md`](specs/2026-07-07-parity-polish-interfaces.md)
