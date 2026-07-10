# SP7: tmux Parity Wave 3 + Debt Burn-down Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every remaining OPEN ticket in `docs/follow-ups.md` (#7-#74 minus the explicitly-accepted set below) — the big structural parity gaps (general format engine, option scopes, app mouse passthrough, table-driven mouse bindings, alerts subsystem, scrollback reflow), the medium parity gaps (clipboard/copy-pipe, choosers, menus, cross-window/session ops), the polish items, and the internal engineering debt.

**Architecture:** All tmux behavior is specified by the in-repo tmux reference (`docs/tmux-reference/*.md`, produced from tmux master db115c6) — it is the parity authority; when this plan and a reference doc disagree, the reference doc governs (flag the discrepancy in the task report). When a reference doc is silent or ambiguous, verify against the tmux C source directly (clone to scratchpad) before implementing — controller-verified rulings go in the ledger. New public surface: one new module (`src/format.rs`); everything else amends existing modules.

**Tech Stack:** Rust, existing winmux crate. Tests: unit (pure modules) + `tests/server_proto.rs` (headless protocol seam) + e2e where full-stack proof is needed.

## Global Constraints

- **Be exactly like tmux**: every behavior must match the cited `docs/tmux-reference/*.md` section; verify ambiguities against tmux C source (master db115c6).
- **Locked contracts**: public API changes to geom/layout/grid/render/pty/host (`docs/specs/2026-07-06-mvp-interfaces.md`), protocol/pipe/model/status/server/cli/client (`docs/specs/2026-07-07-server-client-interfaces.md`), keys/style/cmd/options/bindings/dispatch (`docs/specs/2026-07-07-command-config-interfaces.md`, `docs/specs/2026-07-07-parity-polish-interfaces.md`) must update the contract file **in the same commit**. `src/format.rs` is NEW public surface: document its interface in `2026-07-07-command-config-interfaces.md` (new `## format` section) in the commit that creates it. Private helpers are unconstrained.
- `cargo clippy --all-targets -- -D warnings` stays clean. cargo lives at `$HOME/.cargo/bin` (not on PATH in fresh shells): `export PATH="$HOME/.cargo/bin:$PATH"`.
- Tests MUST use unique `-L` socket names + kill-server teardown; NEVER the default socket. `server_proto` flakes at FULL parallelism only — retry at `--test-threads=4` before suspecting a regression (follow-up #58; Task 18 mitigates).
- Do not touch `target\release\winmux.exe` until Task 19 (a user server may hold the lock — check with `winmux -L <sock> kill-server` semantics first). Debug builds for all mid-plan verification.
- TDD: RED (run the failing test, record its failure) before GREEN. Frequent commits, at least one commit per task.
- Branch: integration branch `feature/sp7-parity-wave3` off `main`. Wave-1 parallel tracks each work in their own worktree branch (`sp7/track-a` … `sp7/track-d`) and are merged into the integration branch by the controller, serially, full-suite-verified after each merge.
- Real spacebar decodes as `Char(' ')` (see Task 5); ConPTY won't pass synthetic CSI ?1049h (alt-screen e2e infeasible) — do not write e2e tests that need it.
- Ledger: `.superpowers/sdd/progress.md` (gitignored) — every task appends its report there.

## Explicitly accepted, NOT implemented in SP7 (Task 19 documents these as final-state accepted debt)

- **#2** (confirm-race framing) — structural mitigation already in place; the ticket itself says keep it listed.
- **#4 / #13** (unbounded event/writer channels) — accepted design tradeoff per the server/client design doc; re-affirm, don't change.
- **#33** (config errors via log + first-attach transient message) — documented design-spec deviation; keep.
- **#40** (corner-cell border tie-break) — real tmux has the same ambiguity; keep.
- **#59** (`'` empty-commit silent no-op) — deliberate, comment-documented judgment call; keep.
- **TPM/plugins (SP5)** — separate researched plan (`docs/superpowers/plans/2026-07-08-tpm-plugin-support.md`); SP7 delivers its rung-1 prerequisites (#27 format engine, #68 `show -gqv`) but does NOT execute it.

## Parallelization map

**Wave 1 — four parallel tracks in isolated worktrees** (disjoint primary files; all add server_proto tests, which conflict append-only at file end — controller resolves at merge):

| Track | Tasks | Primary files | Branch |
|---|---|---|---|
| A | 1 | NEW `src/format.rs`, `src/options.rs` (expand_format delegation), `src/status.rs` (callers) | `sp7/track-a` |
| B | 2, 3 (serial within track) | `src/grid.rs`, `src/options.rs` (Task 3's `allow-rename` SPECS entry — **overlaps Track A's file**) | `sp7/track-b` |
| C | 4 | `src/pty.rs`, `src/pipe.rs`, `src/protocol.rs`, `src/client.rs`, `src/main.rs`, `src/render.rs` (one dead field), `src/server.rs` (writer threads, eviction msg) | `sp7/track-c` |
| D | 5 | `src/keys.rs`, `src/input.rs`, `src/bindings.rs` | `sp7/track-d` |

Merge order after Wave 1: C → D → B → A (smallest blast radius first; A merges LAST deliberately because Tracks A and B both edit `src/options.rs` — B's `allow-rename` SPECS/getter entry lands first, and A's merge must be conflict-checked against it by the controller as a REAL textual merge, not just a test-suite re-run). All tracks also append tests to `tests/server_proto.rs` (append-only conflicts, controller-resolved). Full `cargo test` + clippy after each merge.

**Waves 2-7 — serial on the integration branch** (every task touches `src/cmd.rs` / `src/server/dispatch.rs` / `src/options.rs`, the shared choke-point files):

- Wave 2: Task 6 (option scopes), Task 7 (status residuals).
- Wave 3 (mouse): Task 8 (table-driven mouse bindings) → Task 9 (app passthrough) → Task 10 (choose-tree mouse + status hit-test).
- Wave 4 (window/pane ops): Task 11 → Task 12.
- Wave 5 (copy/buffers): Task 13 → Task 14.
- Wave 6 (overlays/alerts): Task 15 → Task 16 → Task 17.
- Wave 7 (closeout): Task 18 (test debt) → Task 19 (e2e + docs + release).

Dependencies: Task 7 needs Task 1 (format engine) and Task 6 (window-scoped options for per-window formats). Task 9 needs Task 3 (DECSET tracking) and Task 8 (mouse key names). Task 16 needs Task 8 (MouseDown3 routing). Task 17 needs Task 3 (BEL hook) and Task 6 (window-scoped monitor options). Task 12's find-window multi-match needs nothing new (choose-tree exists).

---

### Task 1: General tmux format engine (`src/format.rs`) — closes #27, #70

**Files:**
- Create: `src/format.rs` (+ declare in `src/lib.rs`)
- Modify: `src/options.rs` (`expand_format` becomes a thin delegate to `format::expand`; `FormatCtx` moves to `format.rs`, re-exported from `options` for source compat or callers updated), `src/status.rs` + `src/server.rs` (call sites updated), remove the `status::status_spans` default-path flags-padding shim (#70's width-stability hack) once the real default expands correctly
- Modify: `docs/specs/2026-07-07-command-config-interfaces.md` (new `## format` section; `expand_format` delegation note) — same commit
- Test: `src/format.rs` unit tests (exact expected strings, computed in comments), `src/status.rs` regression suite must stay green unmodified except the shim-removal tests

**Interfaces:**
- Produces: `pub struct FormatCtx { ... }` (all fields the current `options::FormatCtx` has, plus `window_flags: String`, per-window `pane_index`/`pane_title` overrides — Task 7 consumes those), `pub fn expand(fmt: &str, ctx: &FormatCtx) -> String`.
- Consumers: `status::status_spans`, `server::render_one`, `options` (default values).

Spec: `docs/tmux-reference/commands-config-options-formats.md` — the formats section. Implement the documented core grammar, at minimum:
- `#{variable}` braced variables (every variable the current subset supports plus `window_flags`, `session_name`, `window_index`, `window_name`, `pane_index`, `pane_title`, `host`, `host_short`, `client_*` where winmux has the data — enumerate what the ctx can supply; undefined variables expand to empty like tmux).
- Conditionals `#{?cond,true-part,false-part}` with nested `#{}` inside both parts and comma-escaping via `#,` per the doc.
- Comparison/logic prefixes documented in the reference (`#{==:a,b}`, `#{!=:a,b}`, `#{&&:a,b}`, `#{||:a,b}`) — check the doc for the exact supported list; implement what it documents.
- Length-limit modifier `#{=N:variable}` and `#{=-N:variable}` (truncate left/right) — needed for tmux's real `status-right` default (`#{=21:pane_title}`).
- Single-char aliases `#S #W #I #P #F #H #T` and literal `##`; strftime `%`-codes passthrough preserved (existing behavior).
- tmux's REAL default `window-status-format` `#I:#W#{?window_flags,#{window_flags}, }` must now expand byte-identically to what the SP6 shim produced (flagless window → trailing space) — that is the #70 acceptance test.

- [ ] **Step 1: RED —** unit tests in `src/format.rs`: `braced_variable_expands`, `undefined_variable_expands_empty`, `conditional_true_and_false_parts`, `conditional_nested_expansion`, `comparison_eq_ne`, `length_limit_truncates_right_and_left`, `hash_escape_literal`, `real_default_window_status_format_matches_sp6_shim_output` (both flagged `*` and flagless cases — exact strings in comments), `strftime_passthrough_preserved`. Run: `cargo test format::` — expected: FAIL (module doesn't exist / functions undefined).
- [ ] **Step 2: Record RED output in the ledger.**
- [ ] **Step 3: GREEN —** recursive-descent expander in `src/format.rs` (parse `#{`…`}` with nesting depth tracking; modifiers as documented). Wire `options::expand_format` to delegate. Change `options::DEFAULT_WINDOW_STATUS_FORMAT` to tmux's real default string; delete the `status_spans` default-path padding shim and update/remove its shim-specific tests (sanctioned — the shim exists only because the engine didn't, see #70).
- [ ] **Step 4: Verify —** `cargo test format:: options:: status::` then full `cargo test` + `cargo test --test server_proto -- --test-threads=4` + clippy. The SP6 width-stability status tests must still pass (now via the real engine).
- [ ] **Step 5: Amend the command-config contract (`## format` section: FormatCtx fields, `expand` signature, supported grammar list, documented non-supported remainder). Commit** `feat(format): general tmux format engine — braced vars, conditionals, comparisons, length limits (closes follow-ups #27, #70)`.

### Task 2: Scrollback reflow on resize — closes #47

**Files:**
- Modify: `src/grid.rs` (resize path: reflow scrollback + primary screen to the new width)
- Modify: `docs/specs/2026-07-06-mvp-interfaces.md` (grid resize semantics note) — same commit
- Test: `src/grid.rs` unit tests

**Interfaces:** none change (resize signature stays; behavior documented in the contract's grid section).

Spec: `docs/tmux-reference/panes-and-layout.md` (resize/reflow section — tmux ≥1.9 reflows history to the new width: long lines wrap into multiple rows, short rows joined only if they were soft-wrapped). Key semantic: reflow requires tracking which rows are soft-wrapped continuations vs hard newlines. If `grid::Cell`/row storage has no wrapped-flag today, add a per-row `wrapped: bool` (set when the cursor auto-wraps at the right margin, cleared on explicit linefeed) — that is the standard terminal-emulator approach and what tmux's `GRID_LINE_WRAPPED` flag does. Alternate screen does NOT reflow (tmux clears/redraws it; keep current clip/pad behavior there).

- [ ] **Step 1: RED —** unit tests: `autowrap_sets_wrapped_flag_hard_newline_clears_it`; `narrow_resize_rewraps_long_line` (feed an 80-col grid a 100-char line — 1 wrapped pair — resize to 40, assert the text now spans 3 rows with identical concatenated content and correct cursor position, exact rows in comments); `widen_resize_rejoins_soft_wrapped_rows` (the pair rejoins into one row; a HARD-newline pair does NOT rejoin); `reflow_preserves_scrollback_content_across_shrink_and_grow` (round-trip 80→40→80 restores the original visible text); `alt_screen_resize_does_not_reflow`.
- [ ] **Step 2: Run `cargo test grid::`, record RED.**
- [ ] **Step 3: GREEN —** wrapped-flag plumbing in the write path; reflow algorithm on resize (concatenate soft-wrapped logical lines, re-split at the new width, rebuild scrollback + screen + cursor mapping — follow tmux's grid-reflow approach from the reference doc; verify cursor-mapping rules there).
- [ ] **Step 4: Full `cargo test` (copy-mode/selection suites exercise view_cell heavily — they must stay green) + clippy.**
- [ ] **Step 5: Amend the MVP contract's grid section (reflow semantics, wrapped flag). Commit** `feat(grid): reflow scrollback and screen on resize like tmux (closes follow-up #47)`.

### Task 3: Grid VT hooks — pane mouse-mode tracking, BEL, ESC k / allow-rename — closes #52; prereq for #72 (Task 9) and #74 (Task 17)

**Files:**
- Modify: `src/grid.rs` (DECSET/DECRST 9/1000/1002/1003/1005/1006 tracking → `pub fn mouse_proto(&self) -> MouseProto`; BEL (`\x07`) counter → `pub fn take_bell(&mut self) -> bool`; ESC k title parse feeding the existing OSC title slot)
- Modify: `src/options.rs` (`allow-rename` option, Flag, default **off** — verify default in the reference doc), `src/server.rs` (auto-rename gate consults `allow-rename` for ESC k-sourced titles)
- Modify: `docs/specs/2026-07-06-mvp-interfaces.md` (grid additions) + `docs/specs/2026-07-07-command-config-interfaces.md` (option) — same commit
- Test: `src/grid.rs` unit tests, `tests/server_proto.rs`

**Interfaces:**
- Produces: `pub enum MouseProto { Off, X10, Normal, Button, Any }` + `pub fn mouse_proto(&self) -> MouseProto` and `pub fn mouse_encoding(&self) -> MouseEncoding` (`Default` | `Utf8` | `Sgr`) on `Grid` (Task 9 consumes); `pub fn take_bell(&mut self) -> bool` (Task 17 consumes); ESC k sets the same title the OSC 0/2 path sets, but ONLY when `allow-rename` handling permits (the option gate lives server-side, so grid always captures; server decides).

Spec: `docs/tmux-reference/mouse.md` (pane mouse-mode flags: which DECSET numbers map to which MOUSE_* flags, and the SGR/UTF8 encoding toggles) and `docs/tmux-reference/commands-config-options-formats.md` (`allow-rename`, default off in modern tmux — verify). ESC k `<title>` ESC \ is the historical tmux title escape.

**ESC k implementation constraint (plan-review ruling, verified against vte 0.13's state table):** the vte crate has NO string-capturing path for `ESC k` — in the `Escape` state, byte `k` falls in the generic `0x60..=0x7e → (Ground, EscDispatch)` bucket, so `esc_dispatch` fires once and every subsequent title byte would be dispatched as `Print` (leaking the title text into the pane's visible cells). Do NOT try to hook `esc_dispatch`/OSC for this. Instead, pre-scan the raw byte stream in `Grid::feed` for `\x1bk … \x1b\\` (or BEL-terminated per tmux's tolerance — verify terminator rules in the tmux source if the doc is silent), strip the sequence out before handing bytes to `vte::Parser::advance`, and capture the title into the existing OSC-title slot. The pre-scan must handle the sequence split across `feed` chunk boundaries (keep a small pending-state, same class of problem as the input decoder's escape buffering).

- [ ] **Step 1: RED —** grid unit tests: `decset_1000_sets_normal_mouse_1006_sets_sgr_encoding`, `decrst_clears_mouse_mode`, `mode_1003_any_motion_wins_over_1000`, `bel_byte_sets_bell_flag_take_bell_clears`, `esc_k_sets_title`, `esc_k_title_bytes_do_not_leak_into_cells` (feed `\x1bkfoo\x1b\\` then `bar`; assert cells contain only `bar`), `esc_k_split_across_feed_chunks` (sequence split mid-title over two `feed` calls; title still captured, no leak). server_proto: `allow_rename_off_ignores_esc_k_title_rename` + `allow_rename_on_esc_k_renames_window` (mirror the existing automatic-rename OSC tests).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN —** DECSET/DECRST arms for the mode numbers in `csi_dispatch`'s existing `?`-intermediate match (modes 7/25/1049 already live there); BEL in the vte `execute` hook; ESC k via the pre-scan/strip in `Grid::feed` described above (NOT a vte hook). Option + server gate: ESC k-sourced titles participate in automatic-rename only when `allow-rename` is on (OSC 0/2 path unchanged).
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contracts; mark #52 resolved in follow-ups. Commit** `feat(grid): pane mouse-mode/encoding tracking, BEL surfacing, ESC k title + allow-rename (closes follow-up #52)`.

### Task 4: Plumbing debt batch — closes #7, #8, #9, #10, #14, #15, #16, #17, #18, #19, #20, #21, #24, #62, #63

**Files:**
- Modify: `src/pty.rs` (#7 win_err HRESULT unmask — port `pipe.rs`'s `raw_win32_code` approach), `src/pipe.rs` (#8 `UNLEN + 1` buffer; #15 explicit owner-only `SECURITY_ATTRIBUTES` DACL on `CreateNamedPipeW`; #21 dead `Ok(())` arm), `src/protocol.rs` (#9 `write_frame` returns an error for payloads > `MAX_FRAME`; #10 decoders error on trailing unconsumed payload bytes), `src/server.rs` (#14 per-pane writer thread + `mpsc<Vec<u8>>`, mirroring the per-client writer design so a stalled pane can't block the main loop; #17 steal-eviction message `[detached (from session <name>)]`; #18 comment; #21 redundant `Renderer::new`), `src/client.rs` + `src/main.rs` (#16 stdin-reader panic signals main loop exit non-zero; #19 kill-server race gets the cleaner `no server running on <pipe>` message where distinguishable), `src/input.rs` (#20 doc-comment fix + test rename), `src/model.rs` (#62 doc comments on `Window::name`/`Session::name` invariant), `src/render.rs` + `src/server.rs` (#63 delete dead `CopyView::cursor`), tests/pipe_smoke.rs or unit tests as fits (#24 test rename)
- Modify: `docs/specs/2026-07-06-mvp-interfaces.md` (#63 CopyView field removal) + `docs/specs/2026-07-07-parity-polish-interfaces.md` (#63) + `docs/specs/2026-07-07-server-client-interfaces.md` (#14 pane-writer architecture note, #17 message string) — same commits
- Test: unit tests per module; `tests/server_proto.rs` for #14 (pane input under a non-draining child still renders other panes) and #17 (steal eviction message text)

**Interfaces:** #63 removes a public field (contract amendment, two spec files). #14 is internal threading (documented in the server contract's architecture prose). Everything else private.

Notes per ticket: #14's design is already sketched in the ticket ("per-pane writer channel + thread, mirroring the existing per-client writer design") — writer thread owns the pty write handle clone; `InputEvent::Forward` enqueues; pane drop shuts the channel. #15: build a `SECURITY_DESCRIPTOR` granting GENERIC_ALL to the owner SID only (use `GetTokenInformation(TokenUser)`); keep behavior identical otherwise. #9: prefer a debug_assert + `Err` on oversize rather than silent chunking (callers already chunk). #10: strict length check behind the existing decode helpers; keep error text actionable.

- [ ] **Step 1: RED —** per-ticket failing tests where behavior changes: `protocol::tests::write_frame_rejects_oversized_payload`, `protocol::tests::decoder_rejects_trailing_bytes`, `pipe::current_username` buffer test (compile-level; assert constant), server_proto `stalled_pane_stdin_does_not_block_other_panes` (spawn a pane running a child that never reads stdin — e.g. `powershell -Command Start-Sleep 60` — flood `send-keys` at it, assert another pane still renders within timeout), server_proto `steal_attach_eviction_message_names_session`. Doc/comment/rename items (#18, #20, #24, #62) need no RED.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN — one commit per coherent group** (protocol strictness; pipe hardening; pty win_err; per-pane writer; client exit paths; cleanups+docs). Contract amendments ride the commit that changes the surface (#63, #14, #17).
- [ ] **Step 4: Full `cargo test` + `cargo test --test pipe_smoke --test pty_smoke` + clippy.**
- [ ] **Step 5: Mark all 15 tickets resolved in follow-ups (with one-line references). Final commit** `chore(debt): plumbing batch — protocol strictness, pipe DACL+UNLEN, pty win_err, per-pane writer thread, client exit paths, dead-code (closes #7-#10, #14-#21, #24, #62, #63)`.

### Task 5: Space-key normalization + `bind -n` printable verification — closes #34, #30

**Files:**
- Modify: `src/bindings.rs` (normalize `KeyCode::Space` ↔ `Char(' ')` at bind-time storage AND lookup — single canonical form internally, per #34's "normalizing wherever keys are looked up" fix sketch; decoder unchanged)
- Modify: `src/input.rs` if the KeyMachine's table lookups need the same canonicalization
- Test: `src/bindings.rs` + `src/input.rs` unit tests, `tests/server_proto.rs`

**Interfaces:** none change (canonicalization is internal to Bindings storage/lookup).

Spec: user-facing rule — `bind ... Space ...` in a config file must fire on a real spacebar press (which decodes as `Char(' ')`, see #34). tmux treats them as the same key. `is_plain_forwardable`'s plain-space-to-pane forwarding must be preserved for UNBOUND space. #30: real tmux `bind -n x <cmd>` shadows typing `x` entirely — verify winmux's root-table dispatch order does that for printables, add the missing test, fix if it doesn't.

- [ ] **Step 1: RED —** bindings unit: `bind_named_space_fires_on_char_space_lookup` (insert under `named("Space")`, look up `Char(' ')`, get the binding; and the reverse). server_proto: `config_bind_space_reachable_by_real_spacebar` (config `bind Space select-window -t 1` — press prefix then a literal `0x20` byte, assert the window switched), `bind_dash_n_printable_shadows_typing` (config `bind -n x kill-window`-style observable command; type `x` with NO prefix, assert the command ran and `x` did NOT reach the shell), `unbound_space_still_forwards_to_pane`.
- [ ] **Step 2: Run, record RED (the Space ones fail today per #34; the -n printable one may pass — if it passes, record that #30 was verification-only).**
- [ ] **Step 3: GREEN —** canonicalize in `Bindings::bind`/`unbind`/`lookup` (map `Char(' ')` → `Space` internally, or vice versa — pick one canonical form, document it in a comment); ensure the KeyMachine's root-table probe happens before plain-forwarding so `-n` printables shadow typing.
- [ ] **Step 4: Full verification + clippy (whole input/keys suite for regressions).**
- [ ] **Step 5: Mark #34/#30 resolved. Commit** `fix(keys): Space/Char(' ') binding equivalence; verify bind -n printable shadowing (closes #34, #30)`.

### Task 6: Per-session and per-window option scopes + `pane-base-index` + `show -gqv` — closes #26, #32, #68

**Files:**
- Modify: `src/options.rs` (scope layering: global + per-session + per-window stores; resolution = window → session → global for window-scoped names, session → global for session-scoped; SPECS entries gain a scope tag), `src/cmd.rs` (`set`/`show` parse `-s` (server— if the doc requires), `-g`, `-w`, `-p`?— follow the doc's scope flags; `show-options` gains `-v`/`-q` per #68), `src/server/dispatch.rs` (`exec_set_option`/`exec_show_options` thread scope + target resolution; every option READ site routes through a scope-resolving accessor with the acting session/window), `src/model.rs` (if per-entity option stores live on Session/Window structs)
- Modify: `docs/specs/2026-07-07-command-config-interfaces.md` + `docs/specs/2026-07-07-server-client-interfaces.md` (if model gains fields) — same commit
- Test: `src/options.rs` + `src/cmd.rs` unit tests, `tests/server_proto.rs`

**Interfaces:**
- Produces: a scope-resolving read API, recommended `Options::get_for(&self, name, sess: Option<SessionId>, win: Option<WindowId>) -> Value` or typed-getter variants taking a scope context — pick ONE pattern, apply it to every getter dispatch/status/render currently call, and document it. Task 7 and Task 17 consume window-scoped getters (`window-status-*`, `monitor-activity`).

Spec: `docs/tmux-reference/commands-config-options-formats.md` (options scopes section): window options settable per-window (`setw`, `set -w`) with a global-window level (`setw -g`); session options per-session with global level (`set -g`); unprefixed `set` inside a session context targets THAT session; `-t` targets; inheritance chain exactly as documented there. `pane-base-index` (#32): its existing getter must actually be consulted wherever pane indexes are user-visible (display-panes digits, `#P` expansion, kill-pane prompt text, pane targets `:.N`) — audit call sites, wire, and test. #68: `show -gqv @foo` prints value-only; `-q` suppresses the unknown-option error.

- [ ] **Step 1: RED —** options unit: `window_option_overrides_global_for_that_window_only`, `session_option_overrides_global`, `unset_window_option_falls_back_to_global`, `setw_g_sets_global_window_level`. cmd unit: `show_options_parses_v_and_q_flags`. server_proto: `setw_status_style_on_one_window_only_styles_that_window` (visible per-window divergence — e.g. `window-status-current-format` per-window), `set_without_g_targets_current_session`, `show_gqv_user_option_prints_value_only` (the TPM rung-1 primitive), `pane_base_index_shifts_display_panes_digits_and_hash_P`.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN —** scope stores + resolution; dispatch threading (the acting client's session/window is already resolved in dispatch context); `pane-base-index` wiring at the audited sites; `-v`/`-q` flags.
- [ ] **Step 4: Full verification + clippy (status/render suites prove no default-behavior regression).**
- [ ] **Step 5: Amend contracts; mark #26/#32/#68 resolved. Commit** `feat(options): per-session/per-window option scopes, pane-base-index wiring, show -gqv (closes #26, #32, #68)`.

### Task 7: Status residuals — closes #29, #69, #71; verifies/marks #31, #36, #37

**Files:**
- Modify: `src/status.rs` (window-list overflow scrolling with `<`/`>` markers per justify; per-window `FormatCtx` with that window's ACTIVE pane's index/title), `src/server.rs` (status refresh driven by `status-interval` via the Tick handler — re-render when `now - last_status_render >= status-interval`, not only on minute change; thread per-window active-pane data into `status::WindowEntry`), `src/options.rs` (`status-left` length cap measured on VISIBLE width after `#[...]` markers stripped — reuse `strip_style_markers` before `truncate_chars`, then re-apply markers correctly or cap on the stripped-and-restyled spans)
- Test: `src/status.rs` unit tests (exact span assertions), `tests/server_proto.rs`

**Interfaces:** `status::WindowEntry` gains per-window active-pane fields (status is in the server-client contract — amend same commit).

Spec: `docs/tmux-reference/status-line-and-messages.md` §1.4 (overflow: scroll the window list to keep the current window visible, draw `<`/`>` markers when content exists beyond either edge). #71: each tab's format ctx carries THAT window's active pane's `pane_index`/`pane_title`. #29: with Task 1's engine, `status-right` containing `%S` or `#{=21:pane_title}` must refresh on the `status-interval` cadence (default 15s; verify in doc). Also VERIFY-AND-MARK: #31 (inline `#[]` in status-left/right — SP6 shipped it; confirm with an existing/new test, then mark resolved), #36 (drag-enters-copy-mode — SP6 Task 6 shipped it; cite `mouse_drag_select_copies_release_text`), #37 (`word-separators` option — SP6 shipped it; cite its test).

- [ ] **Step 1: RED —** status unit: `window_list_scrolls_to_keep_current_visible_with_markers` (narrow width, 5 windows, current = #4 → list scrolled, `<` at left edge; exact spans in comments), `overflow_markers_absent_when_list_fits`, `per_tab_ctx_uses_that_windows_active_pane_title` (two windows, distinct pane titles, format `#T` → each tab shows its own), `status_left_length_cap_ignores_style_marker_bytes` (`#[fg=red]` + text longer than cap → cap counts visible chars only, marker not bisected). server_proto: `status_interval_refreshes_seconds_format` (set `status-interval 1`, `status-right "#{=5:pane_title}%S"`-class sub-minute content, advance ticks, assert re-render).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** per the spec sections above.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend the server-client contract (`## status`); mark #29/#69/#71 resolved and #31/#36/#37 verified-resolved with test citations. Commit** `feat(status): window-list overflow scrolling, per-window pane ctx, status-interval refresh, visible-width left cap (closes #29, #69, #71; marks #31/#36/#37)`.

### Task 8: Table-driven mouse bindings — closes #57, #67(a), #67(b)

**Files:**
- Modify: `src/keys.rs` (mouse pseudo-key names: parse/format `MouseDown1Pane`, `MouseUp1Pane`, `MouseDrag1Pane`, `MouseDragEnd1Pane`, `WheelUpPane`, `WheelDownPane`, `DoubleClick1Pane`, `TripleClick1Pane` … × buttons 1-3 × locations `Pane`/`Border`/`Status`/`StatusLeft`/`StatusRight`/`StatusDefault` per the doc's grammar — a `KeyCode::Mouse(MouseKey)` variant or parallel enum), `src/bindings.rs` (default ROOT/copy-mode table entries reproducing every current hardcoded mouse behavior as bound commands — see mapping below), `src/server/dispatch.rs` (`dispatch_mouse` becomes: classify event → synthesize mouse key name + location → resolve through `Bindings` for the client's current table → execute the bound command with the mouse context; the previous hardcoded behaviors become the DEFAULT bindings' command implementations), `src/cmd.rs` (commands the defaults need that don't exist yet as parseable commands, e.g. `copy-mode -M`, `select-pane -t = -M`-style mouse-target forms — follow the doc's default-binding command list), `exec_unbind_key` (#67a: error `unknown key: <tok>` unless `-q`, EXCEPT valid mouse names now parse for real)
- Modify: `docs/specs/2026-07-07-command-config-interfaces.md` (keys + bindings surface) — same commit
- Test: `src/keys.rs`/`src/bindings.rs` unit, `tests/server_proto.rs`

**Interfaces:**
- Produces: mouse key parse/format in `keys` (Task 9/16 consume — passthrough consults whether a binding consumed the event; menus bind `MouseDown3Pane`).

Spec: `docs/tmux-reference/mouse.md` — the default mouse bindings table (root: `MouseDown1Pane` → select-pane + forward semantics, `MouseDown1Status` → select-window, `MouseDrag1Border` → resize, `WheelUpPane` → copy-mode -e / scroll, `MouseDrag1Pane` → copy-mode -M + selection, `DoubleClick1Pane`/`TripleClick1Pane` → select word/line, copy-mode tables' `MouseDrag1Pane`/`MouseDragEnd1Pane`/`WheelUp`/`WheelDown`; use the doc's exact list) and the key-name grammar. Behavior MUST be regression-free: every existing server_proto mouse test stays green — the refactor moves dispatch from hardcoded `match` to table resolution with identical default outcomes. The user's real conf line `unbind -T copy-mode-vi MouseDragEnd1Pane` must now actually disable release-copy in vi copy mode.
Note: tmux's default bindings use commands/flags winmux may not fully model (`-M`, `send-keys -M`, `{ }` command blocks). Where the doc's default binding is a compound tmux idiom, bind an equivalent winmux-internal command that reproduces the same behavior, and document each such substitution in the contract amendment.

- [ ] **Step 1: RED —** keys unit: `parse_mouse_key_names_roundtrip` (every name × location), `invalid_mouse_name_rejected`. bindings unit: `default_root_table_contains_mouse_bindings`. server_proto: `unbind_copy_mode_vi_dragend_disables_release_copy` (the user's conf idiom: set `mode-keys vi`, unbind DragEnd, do a full SGR drag+release, assert NO buffer created and copy mode stays active — per tmux semantics), `bind_wheelup_pane_custom_command_overrides_default`, `unbind_unknown_key_errors_without_q` + `unbind_unknown_key_quiet_with_q` (#67a). Every EXISTING mouse test in server_proto is the regression net.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN —** key grammar; default-table entries; dispatch_mouse table resolution (classification logic — hit-testing, drag lifecycle, double/triple-click detection — STAYS in dispatch; only the action lookup moves to the table).
- [ ] **Step 4: Full verification + clippy — the entire SP4/SP6 mouse suite green is the bar.**
- [ ] **Step 5: Amend contract; mark #57/#67 resolved. Commit** `feat(mouse): table-driven mouse bindings with tmux default tables; unbind errors on unknown keys (closes #57, #67)`.

### Task 9: Application mouse passthrough — closes #72, #35

**Files:**
- Modify: `src/server/dispatch.rs` (before winmux's own routing: if the target pane's `grid.mouse_proto() != Off` AND the event is not a winmux-reserved gesture per the doc's precedence rules, re-encode the event in the pane's requested encoding and write it to the pane's pty instead), `src/keys.rs` (SGR/X10/UTF8 mouse re-ENCODING — the decode side exists)
- Modify: `docs/specs/2026-07-07-parity-polish-interfaces.md` (mouse routing precedence) — same commit
- Test: `tests/server_proto.rs` (feed a pane DECSET bytes via its output stream — the grid tracks it — then inject SGR mouse; assert the PANE's pty input received the re-encoded event and winmux did NOT act on it)

**Interfaces:** consumes Task 3's `Grid::mouse_proto()/mouse_encoding()` and Task 8's table routing.

Spec: `docs/tmux-reference/mouse.md` passthrough rules — precisely which events tmux forwards to a mouse-owning pane vs consumes itself (tmux consumes events its key tables bind when the binding exists — status-line clicks, border drags — and forwards pane-interior events to the app; wheel behavior per the doc: with the app in mouse mode, wheel forwards; copy-mode entry via wheel only happens when the pane is NOT mouse-owning; verify exact precedence in the doc, including the alternate-screen + `alternate-screen` option interaction if documented). Coordinates re-encode relative to the PANE's origin (subtract pane rect offsets), clamped per protocol limits; X10 encoding caps at 223.

- [ ] **Step 1: RED —** server_proto: `pane_in_mouse_mode_receives_sgr_click` (DECSET 1000;1006 via pane output, click inside pane → pane pty input gets `\x1b[<0;COL;ROWM`-form bytes with PANE-relative 1-based coords, focus may still change per doc — assert per doc), `wheel_forwards_to_mouse_owning_pane_instead_of_copy_mode`, `border_drag_still_resizes_when_pane_owns_mouse` (winmux keeps border events), `passthrough_stops_after_decrst`.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN —** encoding helpers in keys; precedence gate in dispatch ahead of table resolution, per the doc's rules.
- [ ] **Step 4: Full verification + clippy (all existing mouse tests unaffected — they never enable pane mouse mode).**
- [ ] **Step 5: Amend contract; mark #72 (and #35, same gap) resolved. Commit** `feat(mouse): forward mouse to panes whose app enabled mouse reporting (closes #35, #72)`.

### Task 10: Choose-tree mouse + status hit-test truncation — closes #61, #39

**Files:**
- Modify: `src/server/dispatch.rs` (choose-tree overlay: click on a row selects it, second click/double-click commits, wheel scrolls the list — per the doc; display-panes: click on a pane jumps to it if the doc says so; keep swallowing events for Prompt/Confirm), `mouse_status_click` (#39: replicate the renderer's final width truncation before hit-testing — share the truncation helper with `render::compose_back` rather than duplicating)
- Test: `tests/server_proto.rs`

**Interfaces:** none.

Spec: `docs/tmux-reference/choose-tree.md` (mouse section: MouseDown1 selects, DoubleClick1 commits, wheel moves selection/scrolls — use the doc's exact semantics) and `docs/tmux-reference/mouse.md` status-line rules.

- [ ] **Step 1: RED —** server_proto: `choose_tree_click_selects_row`, `choose_tree_double_click_commits_switch`, `choose_tree_wheel_scrolls_selection`, `status_click_past_truncation_is_noop` (terminal narrower than status content; click in the truncated zone must not select an invisible window).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** per docs.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Mark #61/#39 resolved. Commit** `feat(mouse): choose-tree click/wheel routing; status hit-test honors render truncation (closes #39, #61)`.

### Task 11: Cross-window/session structure ops — closes #41, #42, #44, #45

**Files:**
- Modify: `src/layout.rs` (leaf extract/insert primitives: remove a pane's leaf from one tree — collapsing its parent split — and insert a pane into another tree at a given split; needed by cross-window swap/move), `src/model.rs` (cross-session `move_window`), `src/server/dispatch.rs` (`exec_swap_pane` cross-window arm (#41) + `-U`/`-D`-with-`-s` (#42); `exec_break_pane` gains `-s`/`-t` (#44); `exec_move_window` cross-session (#45))
- Modify: `docs/specs/2026-07-06-mvp-interfaces.md` (layout primitives) + `docs/specs/2026-07-07-server-client-interfaces.md` (model) — same commit
- Test: `src/layout.rs`/`src/model.rs` unit, `tests/server_proto.rs`

**Interfaces:**
- Produces: `Layout::remove_leaf(pane) -> bool` / `Layout::insert_leaf_at(...)`-class primitives (exact shape per implementation, contract-documented; swap = coordinated relabel across two trees since PaneIds are global).

Spec: `docs/tmux-reference/panes-and-layout.md` (swap-pane full semantics incl. `-s`+direction resolution) and `docs/tmux-reference/windows-and-sessions.md` (move-window cross-session: window object moves, ids stable, destination index = explicit or `lowest_unused_index`; source session's focus falls per the doc; break-pane `-s` selects the source pane, `-t` the destination window index/session). Follow-up #45's fix sketch confirms ids are global so no re-minting.

- [ ] **Step 1: RED —** layout unit: `remove_leaf_collapses_parent_split`, `cross_tree_swap_preserves_geometry_slots`. model unit: `move_window_across_sessions_reindexes_destination`. server_proto: `swap_pane_between_windows_swaps_content` (marker text swap check), `swap_pane_dash_s_with_direction_resolves_relative_to_s`, `break_pane_dash_s_moves_named_pane`, `move_window_to_other_session_appears_there_and_leaves_source`.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** — layout primitives first (pure, unit-proven), then model, then dispatch arms.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contracts; mark #41/#42/#44/#45 resolved. Commit** `feat(panes): cross-window swap-pane, -s+direction, break-pane -s/-t, cross-session move-window (closes #41, #42, #44, #45)`.

### Task 12: find-window regex + multi-match picker; absolute main-pane sizing — closes #46, #54, #43

**Files:**
- Modify: `src/server/dispatch.rs` (`exec_find_window`: regex matching (`regex` crate — first external dep: justify in the commit message, or hand-roll the subset if the reference doc's matching is simple `fnmatch`-style — CHECK the doc first: tmux `find-window` uses fnmatch patterns by default and `-r` for regex; implement what the doc says), multi-match → route into the existing choose-tree overlay filtered to the matches), `src/layout.rs` + apply-sites (#43: store the configured absolute main-pane size at preset apply; re-derive the ratio from the CURRENT area on every resize — per the follow-up's fix sketch: a side-table or node variant remembering "first child wants N absolute cells")
- Modify: contracts for any layout surface change — same commit
- Test: unit + `tests/server_proto.rs`

**Interfaces:** layout may gain an absolute-size annotation (contract-documented).

Spec: `docs/tmux-reference/windows-and-sessions.md` §find-window (lines ~256-275): find-window is sugar that opens **choose-tree (window-tree) mode filtered to the matches** — it is NOT a direct jump, **even for a single match** (plan-review ruling: winmux's current single-match direct jump is a parity deviation and must be REPLACED, not preserved; selecting the entry in the tree performs the jump). `-C`/`-N`/`-T` content/name/title match targets, fnmatch by default, `-r` regex — implement per doc. Also `docs/tmux-reference/panes-and-layout.md` (main-pane-width/height are ABSOLUTE cell counts re-applied on every resize).

- [ ] **Step 1: RED —** server_proto: `find_window_multi_match_opens_choose_list`, `find_window_single_match_opens_choose_list_too` (tmux parity: one match still opens the filtered tree; Enter then jumps — the OLD direct-jump test, if one exists, is sanctioned to invert with a computed comment), `find_window_regex_flag_matches_anchor` (`-r '^foo'`). layout/server_proto: `main_pane_width_survives_window_resize` (apply main-vertical with `main-pane-width 20`, resize the terminal, assert the main pane is STILL exactly 20 cols, not proportionally scaled).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** per docs.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contracts if layout surface changed; mark #43/#46/#54 resolved. Commit** `feat(windows): find-window regex+multi-match picker; absolute main-pane sizing across resizes (closes #43, #46, #54)`.

### Task 13: Clipboard + copy-pipe + bracketed paste + emacs keys — closes #55, #53, #56

**Files:**
- Modify: `src/cmd.rs` (copy-mode command args: `copy-pipe`, `copy-pipe-and-cancel` with a shell command argument; note winmux models copy-mode commands as bindings-table actions — follow how `copy-selection-and-cancel` is modeled today), `src/server/dispatch.rs` (copy-pipe: spawn the command via `std::process::Command` with the selection on stdin, detached, no console window (CREATE_NO_WINDOW); OSC 52: when `set-clipboard` is `on`/`external`, emit `\x1b]52;c;<base64>\x07` to the ATTACHED CLIENT's output stream on every copy — add `set-clipboard` option), `src/options.rs` (`set-clipboard` Choice on/external/off, default per doc), `src/buffers.rs` (paste path: `-p` wraps in `\x1b[200~`…`\x1b[201~` when the target pane's grid has bracketed-paste mode (DECSET 2004) set — track 2004 in grid alongside Task 3's modes; tmux only brackets when the app requested it — verify in doc), `src/grid.rs` (DECSET 2004 tracking if Task 3 didn't include it), `src/bindings.rs` (emacs copy-mode: `C-k` copy-end-of-line-and-cancel, `M-m` back-to-indentation #56)
- Modify: contracts (cmd/options/bindings surfaces) — same commit
- Test: unit + `tests/server_proto.rs`

**Interfaces:** grid gains `bracketed_paste(&self) -> bool` if not present (contract note).

Spec: `docs/tmux-reference/copy-mode-and-buffers.md` (copy-pipe semantics: selection piped to the shell command AND still stored in a buffer; set-clipboard/OSC 52 rules — which value emits to clients; paste-buffer -p bracket rule) — verify each detail there.

Two plan-review decisions, made here so the implementer doesn't have to: (a) **OSC 52 capability gating** — ConPTY has no `Ms`-capability probe (per the doc's Windows notes), so winmux gates emission ONLY on the `set-clipboard` option value (`on`/`external` emit, `off` doesn't; per-value semantics per the doc); the "terminal may ignore or mangle it" risk is accepted and documented in the contract amendment (modern Windows Terminal supports OSC 52). (b) **base64**: hand-roll the encoder (~15 lines) in a private helper — do NOT add a dependency for it.

- [ ] **Step 1: RED —** server_proto: `copy_pipe_runs_command_with_selection_stdin` (pipe to `findstr`/`powershell -c "Set-Content"`-class command writing a scratch file; assert file contents == selection), `osc52_emitted_to_client_on_copy` (attached client's byte stream contains the base64 of the copied text), `paste_p_brackets_when_pane_requested_2004` + `paste_p_plain_when_not_requested`, `emacs_C_k_copies_to_eol_and_cancels`, `emacs_M_m_moves_to_first_nonblank`.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** per docs.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contracts; mark #53/#55/#56 resolved. Commit** `feat(copy): copy-pipe, OSC 52 set-clipboard, real bracketed paste, emacs C-k/M-m (closes #53, #55, #56)`.

### Task 14: choose-buffer + choose-client — closes #48, #49

**Files:**
- Modify: `src/cmd.rs` (`choose-buffer`, `choose-client` commands), `src/bindings.rs` (`=` → choose-buffer, `D` → choose-client, prefix table), `src/server/dispatch.rs` + `src/server.rs` (two new list-overlay modes reusing the choose-tree overlay machinery: buffer rows = name + size + first-line sample; client rows = client id/tty + session + attach time; Enter = paste that buffer / switch this client's view to that client's session? — NO: per doc, choose-client's Enter default is detach? — follow `docs/tmux-reference/choose-tree.md`'s choose-client/choose-buffer sections for row format and default Enter/x actions exactly), `src/render.rs` only if the shared overlay needs a variant
- Modify: contracts (cmd surface) — same commit
- Test: `tests/server_proto.rs`

**Interfaces:** none beyond ParsedCmd variants.

Spec: `docs/tmux-reference/choose-tree.md` (choose-buffer and choose-client are mode-tree siblings; row columns, default sort, Enter/x/tag behaviors per the doc — implement Enter + x + navigation; tagging arrives in Task 15 and should apply to these lists too if the shared machinery makes it free, else note).

- [ ] **Step 1: RED —** server_proto: `choose_buffer_lists_buffers_enter_pastes`, `choose_buffer_x_deletes_selected`, `choose_client_lists_attached_clients`, `choose_client_x_detaches_selected` (two attached clients over the pipe — the existing multi-client harness precedent is `two_clients_smallest_size_wins` in tests/server_proto.rs, ~line 443; follow its two-connection setup).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** reusing the ChooseTree overlay state machine (generalize its row model rather than duplicating it — the SP6 tree already carries typed targets).
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contract; mark #48/#49 resolved. Commit** `feat(choosers): choose-buffer (=) and choose-client (D) overlays (closes #48, #49)`.

### Task 15: choose-tree tagging, sort, filter + tiny-pane parity — closes #50 (remainder), #73

**Files:**
- Modify: `src/server.rs`/`src/server/dispatch.rs` (ChooseTree state: tagged-set (`t` toggles tag, `T` untags all, `C-t` tags all — per doc), `O` sort-key cycle + `r` reverse (index/name/time per doc — verify the key list), `/`-style filter prompt if the doc includes it for mode-tree (tmux uses `f` filter with format matching — implement a substring/format subset per doc and document any narrowing), kill (`x`) and other multi-target actions apply to TAGGED items when tags exist per the doc's `%%`/tagged rule), `choose_tree_list_height` (#73: keep the computed height and skip drawing the preview in degenerate geometry — matching mode-tree.c's paint-time guard — instead of expanding the list to full height)
- Modify: `src/render.rs` (tag marker `*` on rows, sort/filter indicator in the title line per doc)
- Test: render unit + `tests/server_proto.rs`

**Interfaces:** none public.

Spec: `docs/tmux-reference/choose-tree.md` — tagging keys/markers, sort keys for the tree mode, filter behavior, tagged-target application; mode-tree.c:980-981 degenerate guard per follow-up #73's exact citation.

- [ ] **Step 1: RED —** server_proto: `choose_tree_t_tags_row_and_x_kills_all_tagged` (tag two windows, x, confirm → both die), `choose_tree_O_cycles_sort_r_reverses` (create windows out of name order; assert row order changes per sort key), `choose_tree_filter_narrows_rows`, `choose_tree_tiny_pane_keeps_short_list_blank_remainder` (#73: degenerate-height pane in BIG preview → list rows ≤ computed h, remainder blank).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** per doc.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Mark #50 fully resolved (and #73). Commit** `feat(choose-tree): tagging, sort cycling, filtering; tmux tiny-pane guard parity (closes #50, #73)`.

### Task 16: display-menu + right-click default menus — closes #51

**Files:**
- Modify: `src/cmd.rs` (`display-menu [-t target] [-x pos] [-y pos] [-T title] name key command ...` triple-list grammar per doc — verify exact argv shape there), `src/render.rs` (menu overlay: bordered box, title, item rows with key hints, selected-row highlight — reuse overlay compositing), `src/server.rs`/`src/server/dispatch.rs` (menu state: open/navigate (Up/Down/digit/item key)/Enter runs the item's command/q-Escape closes; mouse: click item runs, click outside closes — per doc), `src/bindings.rs` (default ROOT bindings `MouseDown3Pane`/`MouseDown3Status`/`MouseDown3StatusLeft` etc. → the doc's default context menus, expressed via Task 8's mouse key names; menu item commands from the doc's defaults, substituting winmux equivalents where a tmux idiom isn't modeled — document substitutions)
- Modify: contracts (cmd + any render surface) — same commit
- Test: render unit (exact box cells) + `tests/server_proto.rs`

**Interfaces:** ParsedCmd::DisplayMenu (contract).

Spec (plan-review ruling — the reference docs do NOT fully cover menus): `docs/tmux-reference/mouse.md` §7.1 documents the default right-click menu CONTENTS, and `docs/tmux-reference/commands-config-options-formats.md` Appendix A has only the raw args template (`display-menu | menu | b:c:C:H:s:S:MOt:T:x:y: | 1 | −1` — flags, no grammar prose). The PRIMARY spec source for the `name key command ...` triple-list grammar, flag semantics, and navigation behavior is therefore the tmux C source itself: clone tmux (master db115c6) to the scratchpad, read `menu.c` + `cmd-display-menu.c` + the default menu definitions, record the rulings in the ledger, and implement those.

- [ ] **Step 1: RED —** cmd unit: `display_menu_parses_name_key_command_triples`. server_proto: `display_menu_opens_and_enter_runs_selected` (menu whose item runs `select-window -t 1`; navigate + Enter; assert window switched), `right_click_on_pane_opens_default_menu` (SGR button-3 press; assert menu box rendered), `menu_click_outside_closes_without_action`, `menu_escape_closes`.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** per doc/source ruling.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contracts; mark #51 resolved. Commit** `feat(menus): display-menu overlay + tmux default right-click menus (closes #51)`.

### Task 17: Alerts subsystem — closes #74

**Files:**
- Modify: `src/server.rs` (per-window alert flags: bell/activity/silence; detection: BEL via Task 3's `take_bell` in the pane-output path; activity = output arriving for a pane whose window is not the CLIENT-visible current window, gated by `monitor-activity`; silence = no output for `monitor-silence` seconds, checked on Tick), `src/options.rs` (`monitor-silence` (seconds, default 0=off), `monitor-bell` (default on — verify), `activity-action` if the doc pairs it with `bell-action` — follow the doc's option set; existing `visual-*`/`bell-action`/`monitor-activity` getters get consumers), `src/model.rs` (window alert-flag storage), `src/options.rs`/`src/format.rs` (`#F`/`window_flags` gains `!` bell, `#` activity, `~` silence per tmux flag chars; flags clear when the window becomes current per doc), `src/status.rs` (`window-status-bell-style` applied to flagged tabs; `window-status-activity-style` — add option if missing), visual-* reactions (status-line message vs bell passthrough per `visual-bell`/`visual-activity` values and `bell-action` scoping — exactly per doc)
- Modify: contracts (options/model/status surfaces) — same commit
- Test: unit + `tests/server_proto.rs`

**Interfaces:** model window flags (contract note); format `window_flags` extension.

Spec: `docs/tmux-reference/status-line-and-messages.md` (alerts section: flag chars, clear-on-visit, visual-vs-bell reactions, bell-action any/none/current/other semantics) — verify every semantic there; alerts-c behavior against tmux `alerts.c` if the doc is thin.

- [ ] **Step 1: RED —** server_proto: `bel_in_unfocused_window_sets_bang_flag_and_bell_style` (two windows; BEL bytes into the background one; assert `!` in its tab + bell style), `activity_flag_hash_when_monitor_activity_on`, `flags_clear_on_selecting_window`, `bell_action_none_suppresses`, `visual_bell_on_shows_message_instead_of_passthrough`, `monitor_silence_flags_after_interval` (set `monitor-silence 1`, produce nothing, advance ticks ≥1s, assert `~`).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** per doc.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contracts; mark #74 resolved. Commit** `feat(alerts): bell/activity/silence monitoring, window flags, visual-* and bell-action semantics (closes #74)`.

### Task 18: Test-debt batch — closes #11, #12, #22, #23, #58, #60

**Files:**
- Modify: `tests/server_proto.rs` (#11/#22: kill-last-pane-of-non-last-window-via-`x` cascade; rename_session propagation to OTHER clients; `SwitchClientPrev` single-session no-op; `list-windows` default-target; #60: `swap_pane_wraps_at_ends` for n≥3; #58: widen the two flaky tests' timing margins per the ticket's candidate fix), `tests/common/mod.rs` (#12: `drain_after_exit` → bounded condition-predicate wait), `src/cli.rs` tests (#23: assert full `Invocation`, pin `unknown_flag_err` exact text, rename the `-x 0` sentinel tests for clarity)
- Test: this task IS tests.

**Interfaces:** none.

- [ ] **Step 1: Write the missing tests; run each new test 3× and the two #58 tests 5× at full parallelism to demonstrate the margin fix (record pass counts).**
- [ ] **Step 2: Full `cargo test` + clippy.**
- [ ] **Step 3: Mark #11/#12/#22/#23/#58/#60 resolved. Commit** `test(debt): cascade/propagation/no-op coverage, condition-based drain, flaky margins, CLI assertion pinning (closes #11, #12, #22, #23, #58, #60)`.

### Task 19: e2e + docs closeout + release

**Files:**
- Modify: `tests/e2e_copy_mouse.rs` (passthrough e2e: a pane app that enables mouse mode receives a click — driven via a PowerShell script child that emits DECSET and reads stdin; skip if infeasible under ConPTY and document why), `tests/e2e_config.rs` (a config exercising SP7 surface: window-scoped `setw`, a `#{?...}` format, a mouse `bind`, `set-clipboard` — loads clean AND takes effect)
- Modify: `docs/follow-ups.md` (final sweep: every SP7-closed ticket marked with fix references; the accepted-set re-affirmed with SP7-review citations; new tickets for anything SP7 discovered), `CLAUDE.md` (SP7 section: scope, new module `format`, new options/commands/bindings, updated test counts), `docs/overview.md` (SP7 section)
- Verify: full `cargo test` (record counts), clippy, then — after confirming no live server holds the lock (`winmux -L <unique> kill-server` on test sockets; check the user's default-socket server is NOT killed — only rebuild if the file is writable) — `cargo build --release` + debug-binary smoke on a unique socket.

**Interfaces:** none.

- [ ] **Step 1: e2e additions (RED where behavior is new, then GREEN).**
- [ ] **Step 2: Docs sweep (follow-ups/CLAUDE.md/overview.md).**
- [ ] **Step 3: Full verification: `cargo test` all targets (server_proto at `--test-threads=4` if flaky), clippy, release build + smoke.**
- [ ] **Step 4: Commit** `test(e2e)+docs: SP7 closeout — passthrough/config e2e, follow-ups sweep, CLAUDE.md/overview` **then final whole-branch review before merge to main (controller dispatches; verdict gates the merge).**

---

## Self-review notes

- **Coverage:** every open follow-up ticket maps to a task: #7-#10/#14-#21/#24/#62/#63→T4; #11/#12/#22/#23/#58/#60→T18; #26/#32/#68→T6; #27/#70→T1; #29/#69/#71 (+#31/#36/#37 verify-mark)→T7; #30/#34→T5; #35/#72→T9; #39/#61→T10; #41/#42/#44/#45→T11; #43/#46/#54→T12; #47→T2; #48/#49→T14; #50/#73→T15; #51→T16; #52→T3; #53/#55/#56→T13; #57/#67→T8; #74→T17. Accepted set (#2, #4, #13, #33, #40, #59) documented in T19. TPM/SP5 explicitly out of scope with prerequisites delivered (T1, T6).
- **Order/deps validated:** T7 after T1+T6; T9 after T3+T8; T16 after T8; T17 after T3+T6. Wave 1's tracks are disjoint EXCEPT Tracks A and B both edit `src/options.rs` (B merges before A; controller conflict-checks A's merge as a real textual merge); server_proto.rs append conflicts are controller-resolved at merge.
- **Plan-review gate (2026-07-10, Sonnet, adversarial): APPROVE-WITH-FIXES — all 3 blocking fixes applied (Task 3 ESC k pre-scan ruling, Task 12 find-window always-opens-tree parity, Wave-1 options.rs overlap) plus 4 advisories (Task 14 test citation, Task 16 menu.c-as-primary-spec, Task 13 OSC 52 gating decision + hand-rolled base64).
- **Contract discipline:** T1 (new module section), T2/T3/T4(#63)/T11 (mvp/grid/layout), T4/T7/T11 (server-client), T3/T6/T8/T13/T14/T16/T17 (command-config) — each amends in-commit.
- **Sanctioned existing-test edits:** T1 removes the #70 shim tests (replaced by real-engine tests); T8 may need mechanical updates where mouse tests assert internal dispatch details (behavior must stay identical); everything else adds tests.
- **Known risk concentrations** (flagged for reviewers): T8's dispatch refactor (largest regression surface — the entire mouse suite is the net); T2's reflow (copy-mode view math interacts with scrollback indices); T6's scope threading (every option read site). These three get the strictest review gates.
