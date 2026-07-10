# Changelog

All notable changes to winmux are documented here. Entries are generated
from [Conventional Commits](https://www.conventionalcommits.org) by
[git-cliff](https://git-cliff.org).
## [0.1.0] - 2026-07-10

### Features
- **layout:** Geom types and split-tree new/split/rects
- **layout:** Focus, remove, resize, and zoom operations
- **grid:** Core VT emulation (print, autowrap, C0, cursor, ED/EL, SGR, DECTCEM)
- **grid:** Extended VT emulation (ICH/DCH/ECH/IL/DL/SU/SD, DECSTBM, save/restore, RI, full SGR, alt screen, resize)
- **input:** Prefix-key state machine with repeat + confirm modes
- **host:** Raw-mode console control with infallible restoration
- **pty:** ConPTY wrapper with spawn/resize/reader and exit-waiter smoke tests
- App event loop and local status-bar clock
- **protocol:** Client/server frame codec
- **pipe:** Named-pipe listener/connection transport
- **model:** Session/window registry with tmux naming semantics
- **input:** Tmux window/session/detach bindings and capture mode
- **status:** Model-driven status line with underlined current window
- **server:** Headless multiplexer server with attach/detach over named pipes
- **server:** Windows, sessions, prompts, and tmux CLI command execution
- **cli,client:** Tmux-style CLI, thin attach client, detached server autostart
- **keys:** Tmux key notation, VT encoding, and input decoder
- **style:** Tmux style-string grammar onto grid styles
- **cmd:** Tmux command tokenizer, table, and typed commands
- **options:** Typed tmux option registry with format subset
- **input:** Table-driven key machine and tmux default bindings (alongside legacy)
- **server:** Unified command dispatcher wiring keys, CLI, and prompt
- **config:** .tmux.conf loading, source-file, and -f
- **render:** Option-driven status and border styling
- **grid:** Scrollback, real alternate screen, OSC title capture
- **copy-mode:** Scrollback navigation with emacs/vi tables
- **copy-mode:** Selection, rectangle, and tmux paste buffers
- **copy-mode:** Search with / ? n N and C-s C-r
- **mouse:** SGR mouse decoding, routing, and mode management
- **layout:** Tmux preset layouts, swap-pane, rotate-window
- **window:** Break-pane, move-window, find-window, index prompt
- **overlay:** Display-panes and choose-tree
- **input,name:** Escape-time disambiguation and automatic-rename from ConPTY titles
- **config:** Tmux-conf compatibility batch — setw, space-delimited styles, @-options, copy-mode bind tables, ~ expansion, missing options
- **focus:** Edge-wrap directional navigation + tmux active_point MRU (closes follow-up #65)
- **status:** Status-justify, per-side styles, window-status-format/-current-format, separator
- **windows:** Swap-window with relative targets and -d focus semantics
- **mouse:** Drag autoscroll at pane edges; word/line drag extension after double/triple click
- **choose-tree:** Tree view with expand/collapse, current-item default selection, live preview box
- **clock:** Clock-mode overlay with prefix-t, clock-mode-colour/style
- **render:** Tmux half-border active indication + pane-border-indicators
- **server:** Per-pane writer thread, steal-eviction message, dead CopyView field
- **grid:** Reflow scrollback and screen on resize like tmux (closes follow-up #47)
- **grid:** Pane mouse-mode/encoding tracking, BEL surfacing, ESC k title + allow-rename (closes follow-up #52)
- **format:** General tmux format engine — braced vars, conditionals, comparisons, length limits (closes follow-ups #27, #70)
- **options:** Per-session/per-window option scopes, pane-base-index wiring, show -gqv (closes #26, #32, #68)
- **status:** Window-list overflow scrolling, per-window pane ctx, status-interval refresh, visible-width left cap (closes #29, #69, #71; marks #31/#36/#37)
- **mouse:** Table-driven mouse bindings with tmux default tables; unbind errors on unknown keys (closes #57, #67)
- **alerts:** Bell/activity/silence monitoring, window flags, visual-* and bell-action semantics (closes #74)
- **panes:** Cross-window swap-pane, -s+direction, break-pane -s/-t, cross-session move-window (closes #41, #42, #44, #45)
- **copy:** Copy-pipe, OSC 52 set-clipboard, real bracketed paste, emacs C-k/M-m (closes #53, #55, #56)
- **windows:** Find-window regex+multi-match picker; absolute main-pane sizing across resizes (closes #43, #46, #54)
- **mouse:** Forward mouse to panes whose app enabled mouse reporting (closes #35, #72)
- **mouse:** Choose-tree click/wheel routing; status hit-test honors render truncation (closes #39, #61)
- **overlays:** Choose-buffer (=) and choose-client (D) overlays (closes #48, #49)
- **choose-tree:** Tagging, sort cycling, filtering; tmux tiny-pane guard parity (closes #50, #73)
- **menus:** Display-menu overlay + tmux default right-click menus (closes #51)
- Open-source release pipeline, installer, and repo hygiene

### Bug Fixes
- **layout:** Total split geometry, no underflow on tiny areas
- **grid:** Clamp grid dimensions to 1x1 minimum, no zero-size panics
- **grid:** Discard truncated extended-SGR params instead of misparsing
- **host:** Recover from mutex poisoning in restoration paths
- **pty:** Release pseudoconsole and pipe handles on spawn error paths
- **host:** Save and restore original console code pages
- **pty:** Only null std handles when parent stdio is redirected; clean pane PSModulePath
- **pipe:** Overlapped I/O so duplicate handles support concurrent read+write
- **server:** Invalidate stale confirms, re-feed prompt trailing bytes, reject unknown CLI flags
- **client,pipe,server:** Review fixes — enter-before-attach, first-instance bind, empty-target pin
- **model:** Reject control characters in session/window names; ticket final-review debt
- **keys:** Decode ESC-prefixed sequences as Meta on the decoded key; make flush byte-exact
- **style:** Accept negated inert underline variants; case-insensitive matching
- **cmd:** Make detach-client -s optional so the default prefix-d binding resolves
- **server:** Sync client on -t self-rename; isolate foreign-session kills
- **config:** Renumber from base-index; -f - disables config loading
- **options:** Reject control chars in string options; test-suite config isolation
- **server:** Bare session-name pane/window targets resolve to active pane (tmux parity)
- **grid:** Rejoin semicolon-split OSC titles, full-screen-only scrollback capture, alt-screen pen restore
- **copy-mode:** Pin selection anchors to content; reachable emacs Space binding
- **copy-mode:** Backward search col-0 advance, Unicode-safe column mapping, shared prompt editor
- **mouse:** Plain click in copy mode focuses without copying; cover default active-border green
- **layout:** Error on cross-window swap-pane, honor -t with -U/-D, document main-pane ratio deviation
- **window:** Index prompt validates numeric input before dispatch
- **overlay:** Identity-stable choose-tree selection, overlay-first key routing, message-aware scroll
- **rename:** Unify comma-prompt rename path; sanitize exe-style titles for auto-rename
- Overlay mouse guard, index-prompt overflow wording, docs; ticket final-review follow-ups
- **focus:** Directional pane navigation across split columns (tmux candidate+MRU semantics)
- **mouse:** Clear drag state on status-row/miss/overlay paths so border drag re-arms
- **mouse:** Border drags toward pane's own left/top edge now resize (#66)
- **focus:** Stamp activity on kill/rotate/break focus handoffs, prune activity map, inclusive overlap per tmux
- **focus:** Stamp focus handoff on natural pane exit (handle_exited)
- **focus:** Death handoffs must not stamp activity (tmux window_lost_pane semantics)
- **focus:** Break-pane moved pane must not stamp activity (cmd-break-pane.c direct assignment)
- **status:** Width-stable default window-status-format; ticket per-window format ctx gap
- **windows:** Swap-window -d must not touch last-window when current is dst (session_set_current early return)
- **mouse:** Tmux click/release semantics in copy mode; drag on live pane enters copy mode
- **mouse:** Clear autoscroll state at dispatch early-exit guards (stop runaway edge scroll)
- **choose-tree:** Full 4-sided preview box with insets per tmux (screen_write_box)
- **clock:** Any mouse event exits clock mode (window_clock_key resets unconditionally)
- **protocol:** Write_frame rejects oversized payloads, decoders reject trailing bytes
- **pipe:** Owner-only DACL, UNLEN+1 username buffer, drop dead accept() arm
- **pty:** Unmask HRESULT in win_err; add per-pane writer-thread clone
- **client:** Stdin-reader panic exits non-zero, kill-server race gets cleaner message
- **server:** Acquire pty writer clone before spawning pane threads
- **keys:** Space/Char(' ') binding equivalence; verify bind -n printable shadowing (closes #34, #30)
- **grid:** Reconcile copy-mode state with width reflow per tmux window_copy_size_changed
- **grid:** Back-to-back ESC k state machine; allow-rename independent of automatic-rename per tmux
- **format:** FORMAT_LOOP_LIMIT depth cap + malformed-input regression tests
- **input:** Root-table printable bindings shadow typing (closes #30)
- **status:** Click hit-test shares span layout with renderer under overflow scrolling
- **sp7:** Final-gate consolidated fix wave — main-thread blocking, menu/buffer bugs, grid reflow staleness
- Satisfy new clippy::question_mark lint in Rust 1.97.0
- **test:** Retry pipe connect in two_sequential_clients like the real client
- **test:** Pty_smoke marker must not be readable from the command echo

### Documentation
- Winmux project overview and multiplexing MVP design (in progress)
- Finalize multiplexing MVP design spec
- Locked interface contract for multiplexing MVP
- Multiplexing MVP implementation plan (12 TDD tasks)
- Record Win32_System_IO feature in interface contract
- Follow-up tickets from MVP final review
- Add CLAUDE.md for future Claude Code sessions
- Design spec + implementation plan for server/client split (sub-project 2)
- Design spec + implementation plan for command layer + config (sub-project 3)
- **cmd:** Contract wording — repeated direction flags use fixed priority, not last-wins
- Design spec + implementation plan for parity polish (sub-project 4)
- TPM plugin-ecosystem support research and future plan (SP5)
- Ticket stale MouseDrag-under-overlay residual from final review (#64)
- Ticket single-slot MRU approximation from focus-nav hotfix review (#65)
- Add tmux behavior reference (deep source dive of tmux master db115c6)
- SP6 tmux-parity wave 2 implementation plan
- Ticket unbind-silence/mouse-pseudo-key gap (#67) and show-options -v/-q wiring (#68) from Task 2 review
- **plan:** SP6 amendments — choose-tree tree view + current-item selection, clock-mode task, half-border active indication task
- Ticket missing app mouse passthrough (#72) from Task 6 review
- Fix SP6 summary nits from final review (drop phantom pane-border-status; ticket range #67-#74)
- **plan:** SP7 parity wave 3 + debt burn-down implementation plan
- **plan:** Apply SP7 plan-review fixes (ESC k pre-scan, find-window tree parity, wave-1 overlap, advisories)
- **specs:** Correct ESC k/allow-rename contract prose to independent-rename semantics
- SP7 wave 3 sections + follow-ups final sweep (closes #26, #27, #38-partial, #47, #70)
- **sp7:** Final-gate review — contract fixes, new tickets, flakiness note
- Open-source release pipeline + install/update scripts design spec

### Testing
- **layout:** Cover resize step-back loop multi-iteration path
- End-to-end split/kill/exit harness; add README
- **e2e:** Detach/reattach, windows, kill-session full workflows
- **e2e:** .tmux.conf round-trip; docs for sub-project 3
- **e2e:** Copy-mode and mouse round-trips; docs for sub-project 4
- Test(e2e)+docs: SP6 closeout — user-conf roundtrip, drag-select e2e, follow-ups/CLAUDE.md

Adds tests/e2e_config.rs::user_tmux_conf_loads_without_errors (real user
.tmux.conf loads via -f at server startup with zero errors AND the conf's
remapped prefix + bind actually take effect) and
tests/e2e_copy_mouse.rs::mouse_drag_select_copies_release_text (SGR
press-drag-release on a live pane enters copy mode, copies against the
release-time pane, and the exact dragged text pastes back). Updates
docs/follow-ups.md (#66 header made consistent with #64/#65, #50 narrowed
now that SP6 Task 8 added preview/tree-shape, new #74 for the still-inert
alerts subsystem), CLAUDE.md (SP6 feature summary, keybindings, module
notes, test counts, a pre-existing 3-vs-4-contract-file staleness fix), and
docs/overview.md (new SP6 section) to reflect the wave's full delivered
scope, including Tasks 10/11 added after the original task brief was
written.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- **bindings:** Cross-notation Space unbind coverage; fix stale #34 comment
- **debt:** Cascade/propagation/no-op coverage, condition-based drain, flaky margins, CLI assertion pinning (closes #11, #12, #22, #23, #58, #60)
- **e2e:** SP7 closeout e2e additions
- Gate 13 host-sensitive server_proto tests off hosted CI (follow-up #90)
- Gate the follow-up #58 pair on hosted CI too (follow-up #90 addendum)

### CI/CD
- Run tests serially on hosted runners

### Miscellaneous
- Scaffold winmux lib+bin crate with module skeleton
- **lint:** Clear pre-existing clippy debt ahead of -D warnings gate
- **server:** Remove leftover TEMP-RED-CHECK debug marker
- Resolve MVP follow-ups, refresh docs for server/client architecture
- Doc/test-naming cleanups (input.rs, model.rs, e2e test rename)
- **debt:** Plumbing batch — protocol strictness, pipe DACL+UNLEN, pty win_err, per-pane writer thread, client exit paths, dead-code (closes #7-#10, #14-#21, #24, #62, #63)
- **options:** Drop unused scope getters contradicting #76 narrowing
- Untrack .superpowers/sdd/task-8-report.md (local-only SDD artifact, ignored by .superpowers/sdd/.gitignore)

### Other
- Back-buffer composition (panes, borders, status, overlays)
- Cell-diff compose(), cursor placement, resize repaint
- Initial commit
