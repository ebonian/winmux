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

## SP7: tmux parity wave 3 + debt burn-down — DELIVERED

A third parity pass, broader than SP6: closes essentially every open
follow-up ticket accumulated across sub-projects 2-4 and SP6 (#7 through
#74), except a six-item accepted-debt set explicitly re-affirmed rather
than implemented (#2, #4, #13, #33, #40, #59 — deliberate design tradeoffs
or documented-not-a-bug items, see `docs/follow-ups.md`). Like SP6, no new
interface-contract file: every SP7 change, including the brand-new `format`
module, amends one or more of the existing four contract files in the same
commit as its code change. Plan:
[`docs/superpowers/plans/2026-07-10-sp7-parity-wave3.md`](superpowers/plans/2026-07-10-sp7-parity-wave3.md).
Execution: 19 tasks across 7 waves — Wave 1 ran 4 tasks in parallel
worktrees, Waves 3-5 similarly parallelized independent tracks, all merged
serially with full-suite verification at each merge; Wave 6 (this document's
own closeout task) is Task 19.

Delivered:

- **General tmux format engine** (`src/format.rs`, new module): `#{variable}`
  braced long-form vars, `#{?cond,a,b}` conditionals (incl. chained
  `#{?c1,v1,c2,v2,fallback}` and nested expansion), the six `#{==/!=/</>/<=/
  >=:a,b}` string comparisons, N-ary `#{&&:...}`/`#{||:...}`, `#{=N:x}`/
  `#{=-N:x}` length limits, a `FORMAT_LOOP_LIMIT`=100 recursion cap —
  replacing the old fixed `#S`/`#W`/`#I`/`#P`/`#F`/`#H`/strftime-only
  subset. Both `status-right`'s and `window-status-format`'s real tmux
  defaults now expand correctly for the first time (closes #27, #70).
- **Scrollback reflow on resize** (`Grid::reflow_to_width`, tmux
  `grid_reflow`-style logical-line regrouping and cursor remapping; closes
  #47). A copy-mode selection now unconditionally clears on any actual pane
  width/height change, matching tmux's own `window_copy_size_changed`
  behavior (verified against the tmux C source).
- **`ESC k` / `allow-rename`** (closes #52): the legacy pane-title-rename
  escape is pre-scanned/stripped out of a pane's raw VT stream before
  `vte::Parser` ever sees it (no string-capture path for it exists in the
  `vte` crate), including across back-to-back unterminated sequences — a
  realistic pattern on this platform, since Windows conhost silently drops
  the literal `ESC \` terminator. Gated by a new `allow-rename` window
  option, INDEPENDENT of `automatic-rename` (matches real tmux exactly).
  `Grid` also gains `mouse_proto`/`mouse_encoding` DECSET tracking and
  edge-triggered `take_bell` surfacing, prerequisites for later tasks.
- **Real per-session/per-window option scopes** (`options::Overlay`, closes
  #26, #32, #68): `set`/`setw`/`-g` all resolve to the correct scope for the
  first time, with a documented, deliberate headless-call fallback to global
  (see `CLAUDE.md`'s "Option scopes (SP7)" section).
- **Status-line residuals** (closes #29, #69, #71): window-list overflow
  scrolling with `<`/`>` markers, a marker-aware visible-width
  `status-left-length` cap, per-window `#P`/`#T` in each tab's own format
  expansion, and a `status-interval`-driven refresh timer.
- **Table-driven mouse bindings** (`KeyCode::MouseKey`, closes #57, #67) —
  `bind-key -T root MouseDown1Pane <command>` now works — plus
  **application mouse passthrough** (closes #35, #72): a pane whose own app
  requested mouse reporting gets clicks/drags/wheel forwarded instead of
  consumed for winmux's own copy-mode/drag handling, and **choose-tree
  mouse routing** (click-to-select, double-click-to-commit; closes #61).
- **Cross-window/session structure ops** (closes #41, #42, #44, #45):
  `swap-pane -s`/`-t` across windows/sessions and `-U`/`-D` relative to an
  explicit `-s`; `break-pane -s`/`-t`; `move-window -t session:index` across
  sessions.
- **Alerts subsystem** (closes #74): real bell/activity/silence monitoring
  and window flags (`#`/`!`/`~`), `visual-*`/`bell-action`/`monitor-*`/
  `*-action` options, `window-status-bell-style`/`-activity-style`.
- **`find-window` regex/multi-match + absolute main-pane sizing** (closes
  #43, #46, #54): `-C`/`-N`/`-T`/`-i`/`-r` flags (real regex via the new
  `regex` crate dependency — the project's first external dependency beyond
  `vte`/`windows`); `find-window` now ALWAYS opens the filtered `choose-tree`
  overlay, even for one match, matching real tmux's sugar exactly;
  `main-pane-width`/`-height` gain a percent form and survive a later window
  resize instead of rescaling.
- **`copy-pipe`/OSC 52 clipboard/bracketed paste** (closes #53, #55, #56):
  a `set-clipboard` option, hand-rolled base64 OSC 52 emission on every copy
  destination, real `ESC[200~`/`ESC[201~` bracketed-paste wrapping (gated on
  the TARGET pane's own DECSET 2004 state), and two new emacs copy-mode
  bindings (`C-k`, `M-m`).
- **`choose-buffer` (`=`) / `choose-client` (`D`)** overlays plus
  **tagging/sort/filter** across all four choose-overlay views, generalized
  onto the same tree-overlay machinery rather than duplicated (closes #48,
  #49, #50, #73).
- **`display-menu`/`menu`** (closes #51): a floating bordered overlay,
  right-click on a pane/window-tab/session-name opens winmux's own default
  PANE/WINDOW/SESSION context menu. Spec authority is the cloned tmux C
  source (`menu.c`/`cmd-display-menu.c`), since the project's own
  `docs/tmux-reference/` docs cover the default menus' item contents but not
  the grammar/mechanics.
- **Internal debt burn-down** (closes #7-#10, #14-#21, #24, #58, #60, #62,
  #63, #30 among others): per-pane writer threads (mirroring the existing
  per-client design, so a stalled pane's stdin can no longer block every
  session), named-pipe DACLs, protocol trailing-byte/oversized-payload
  strictness, `pty`/`pipe` error-code consistency, and a `bind -n` fix so a
  plain-printable-key root-table binding actually shadows typing.

New follow-up tickets from this wave: #75-#79 (option-scope narrowings,
`find-window -Z` no-op) plus #80-#84 from the Task 19 closeout itself (four
small self-found Wave-2/Wave-3 notes formalized as tickets, and one platform
ceiling: **Windows ConPTY does not relay a hosted process's mouse-mode
DECSET sequence to a winmux pane's reader at all** — confirmed empirically
and pinned as a permanent regression test
(`tests/pty_smoke.rs::mouse_decset_private_mode_is_not_relayed_by_conpty`);
see `CLAUDE.md`'s "Hard-won platform gotchas" section and
`docs/follow-ups.md` #84 for the full writeup, including why this
retroactively explains three previously-separate "couldn't build this e2e
test" notes from SP4 through SP7). The six-item accepted-debt set (#2, #4,
#13, #33, #40, #59) is explicitly re-affirmed, not implemented — each is a
deliberate design tradeoff or a documented non-bug, not an oversight.

## Specs

- [`specs/2026-07-06-multiplexing-mvp-design.md`](specs/2026-07-06-multiplexing-mvp-design.md) — sub-project 1 (delivered); companion interface contract [`specs/2026-07-06-mvp-interfaces.md`](specs/2026-07-06-mvp-interfaces.md)
- [`specs/2026-07-07-server-client-design.md`](specs/2026-07-07-server-client-design.md) — sub-project 2 (delivered); companion interface contract [`specs/2026-07-07-server-client-interfaces.md`](specs/2026-07-07-server-client-interfaces.md)
- [`specs/2026-07-07-command-config-design.md`](specs/2026-07-07-command-config-design.md) — sub-project 3 (delivered); companion interface contract [`specs/2026-07-07-command-config-interfaces.md`](specs/2026-07-07-command-config-interfaces.md)
- [`specs/2026-07-07-parity-polish-design.md`](specs/2026-07-07-parity-polish-design.md) — sub-project 4 (delivered); companion interface contract [`specs/2026-07-07-parity-polish-interfaces.md`](specs/2026-07-07-parity-polish-interfaces.md)
