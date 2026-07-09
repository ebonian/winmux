# SP6: tmux Parity Wave 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the five user-reported parity failures — mouse border-drag dies after one use, `.tmux.conf` compatibility (17 startup errors + `~` reload failure), directional navigation doesn't wrap at window edges, mouse drag-select doesn't feel like tmux, and choose-tree lacks tmux's live preview box.

**Architecture:** All behavior is specified by the in-repo tmux reference (`docs/tmux-reference/*.md`, produced from tmux master db115c6) and the gap analysis (`.superpowers/sdd/sp6-gap-analysis.md` — read the relevant section before each task; it has exact file:line pointers). Changes live almost entirely in existing modules: `cmd`/`options`/`style` (config compat), `layout` + `server` (wrap + activity counter), `server/dispatch` (mouse state machine, copy-mode drag), `render` + `server` (choose-tree preview), `status` (justify/styles/formats), `model` (swap-window).

**Tech Stack:** Rust, existing winmux crate. Tests: unit (pure modules) + `tests/server_proto.rs` (headless protocol seam) + e2e where full-stack proof is needed.

## Global Constraints

- **Be exactly like tmux**: every behavior must match the cited `docs/tmux-reference/*.md` section. When this plan and the reference doc disagree, the reference doc governs — flag the discrepancy.
- **Locked contracts**: public API changes to geom/layout/grid/render/pty/host (`docs/specs/2026-07-06-mvp-interfaces.md`), protocol/pipe/model/status/server/cli/client (`2026-07-07-server-client-interfaces.md`), keys/style/cmd/options/bindings/dispatch (`2026-07-07-command-config-interfaces.md`, `2026-07-07-parity-polish-interfaces.md`) must update the contract file **in the same commit**. Private helpers are unconstrained.
- `cargo clippy --all-targets -- -D warnings` stays clean. cargo lives at `$HOME/.cargo/bin` (not on PATH in fresh shells).
- Tests MUST use unique `-L` socket names + kill-server teardown; NEVER the default socket (the user has a live server). Do not touch `target\release\winmux.exe` (locked by the user's running server) — debug builds only.
- TDD: RED (run the failing test, record its failure) before GREEN. Frequent commits, one commit per task minimum.
- Branch: all work on `feature/sp6-parity-wave2` off `main`.
- Reference for user's real config: a verbatim copy is at `tests/fixtures/user.tmux.conf` (Task 2 creates it).

---

### Task 1: Mouse drag-state lifecycle fix (drag-once bug)

**Files:**
- Modify: `src/server/dispatch.rs` (three reset sites)
- Modify: `docs/follow-ups.md` (#64 resolved)
- Test: `tests/server_proto.rs`

**Interfaces:** none change (all private/`pub(super)` bodies).

Root cause (gap analysis §D, verified): `ClientState.mouse.drag` is never reset on (1) the status-row short-circuit at `dispatch.rs:1620-1624` — Drag/Up diverted to `dispatch_mouse_status` which ignores them, so a border drag released over the status row leaves `drag = Border{..}` stale; (2) `mouse_down`'s `MouseHit::None` arm; (3) the overlay guard (follow-up #64). The stale `Border` state then makes `mouse_drag_border` (:1775) compute `delta == 0` against the already-resized rect and early-return forever.

tmux spec (`docs/tmux-reference/mouse.md`, drag lifecycle): drag state is cleared when the drag ends; the next motion-with-button is a fresh start. Stale drag state must never survive a button cycle.

- [ ] **Step 1: RED — two regression tests in tests/server_proto.rs** (follow the existing `mouse_border_drag_resizes` at ~:3518 as the harness template):
  - `mouse_border_drag_twice_resizes_twice`: split, drag the vertical border left 2 cols (press on border col, motion, release **inside the pane area**), assert resize applied; then drag again 2 more cols and assert the **second** resize also applied (this fails today: second drag no-ops).
  - `mouse_border_drag_release_on_status_row_then_drag_again`: drag a **horizontal** border downward and release with `ev.y == status row`; then perform a fresh border drag and assert it resizes (fails today).
- [ ] **Step 2: Run both, record the RED failure output.**
- [ ] **Step 3: GREEN — three resets in dispatch.rs:** (a) in `dispatch_mouse`, when the status-row guard matches and the event kind is Drag or Up, set `client.mouse.drag = MouseDrag::None` before delegating; (b) in `mouse_down`'s `MouseHit::None` arm, set `client.mouse.drag = MouseDrag::None`; (c) in the overlay mouse guard (the `ChooseTree(_)|DisplayPanes(_)|ConfirmCmd|Prompt` interception), reset drag state for Drag/Up events likewise (closes follow-up #64 — also add a `mouse_drag_cleared_when_overlay_swallows_release` test if cheaply constructible; if not, note why in the report).
- [ ] **Step 4: Run the new tests + `cargo test --test server_proto -- --test-threads=4` + `cargo test layout::` etc. unit sweep + clippy.**
- [ ] **Step 5: Mark follow-up #64 resolved in docs/follow-ups.md (reference this fix), commit** `fix(mouse): clear drag state on status-row/miss/overlay paths so border drag re-arms`.

### Task 2: Config compatibility — the user's .tmux.conf loads with zero errors

**Files:**
- Create: `tests/fixtures/user.tmux.conf` (verbatim copy of the 91-line conf reproduced in the task brief appendix — the gap analysis §A lists it error-by-error)
- Modify: `src/cmd.rs` (`canonical()` ~:522), `src/style.rs` (`parse_style` :111-123), `src/server/dispatch.rs` (`exec_bind_key`/`exec_unbind_key` :2145-2169; `execute_source_file_headless` :2219), `src/options.rs` (SPECS table, getters, user-option store)
- Modify: `docs/specs/2026-07-07-command-config-interfaces.md` (one consolidated amendment)
- Test: unit tests in each module + `tests/server_proto.rs` config-load test

**Interfaces:** `options.rs` gains new getters + a user-option store (contract amendment). `cmd`/`style`/dispatch signatures unchanged.

Spec: `docs/tmux-reference/commands-config-options-formats.md` — §setw aliasing, §style grammar (delimiters are space/comma/newline), §user options (`@name` always accepted, string-typed, any scope), §source-file path expansion (`~` → home). Gap analysis §A has the row-by-row current-vs-required table.

- [ ] **Step 1: RED — server_proto test `user_config_loads_clean`:** start a headless server with `-f tests/fixtures/user.tmux.conf`, assert the config error count is 0 (today: 17). Plus focused unit REDs:
  - `cmd::tests::setw_is_set_option_alias` (`setw -g pane-base-index 1` parses to the same ParsedCmd as `set -w -g ...`; also `set-window-option`).
  - `style::tests::space_separated_terms` (`fg=white bg=black bold` parses: fg white, bg black, bold attr — requires the split at style.rs:117 to treat `' '`, `','`, `'\n'` all as separators, empty runs skipped; also mixed `fg=red,bold dim`).
  - `options::tests::user_option_set_show_roundtrip` (`set -g @yank_action 'copy-pipe'` stores; `show -gv @yank_action` prints it; unset `@foo` with `-q` is silent, without `-q` errors like tmux).
  - dispatch test: `bind`/`unbind -T copy-mode-vi MouseDragEnd1Pane` succeeds at runtime (the `WhichTable` variants exist in `src/bindings.rs:39-44`; only the dispatch match arms are missing).
  - dispatch test: `source-file ~/xyz.conf` expands `~`/`~/` via `USERPROFILE`.
- [ ] **Step 2: Run all, record RED.**
- [ ] **Step 3: GREEN, per gap-analysis row:** setw/set-window-option alias arm in `canonical()`; style delimiter fix; copy-mode/copy-mode-vi arms in both exec_bind/exec_unbind matches; leading-`~` expansion in `execute_source_file_headless` (apply to the path argument before `PathBuf::from`); `@`-prefix branch in `Options::set`/`show`/unset storing to a new `BTreeMap<String,String>` bypassing SPECS; new SPECS entries + typed getters for: `visual-activity`, `visual-bell`, `visual-silence` (Choice on/off/both, default off), `bell-action` (Choice any/none/current/other, default any), `monitor-activity` (Flag, default off), `clock-mode-colour` (Colour, default blue), `window-status-bell-style` (Style), `window-status-separator` (Str, default `" "`), `status-justify` (Choice left/centre/right/absolute-centre, default left), `status-left-style`/`status-right-style` (Style, default `"default"`), `window-status-format` (Str, default `#I:#W#{?window_flags,#{window_flags}, }`), `window-status-current-format` (same default). Defaults must match `docs/tmux-reference/commands-config-options-formats.md`'s options appendix — verify each there. These options are **accepted and stored** in this task; rendering wiring for justify/side-styles/window-formats/separator lands in Task 4 (visual-*/bell/monitor/clock stay inert until an alerts/clock subsystem exists — ticket in Task 9).
- [ ] **Step 4: Full verification (unit + server_proto at --test-threads=4 + clippy).**
- [ ] **Step 5: Amend `docs/specs/2026-07-07-command-config-interfaces.md`** (new getters, user-option store, setw alias, style delimiter note) **in the same commit**: `feat(config): tmux-conf compatibility batch — setw, space-delimited styles, @-options, copy-mode bind tables, ~ expansion, missing options`.

### Task 3: Directional navigation wrap + activity-counter MRU

**Files:**
- Modify: `src/layout.rs` (`focus_dir` :317-382, `set_focus`), `src/server.rs` + `src/server/dispatch.rs` (activity stamping at every focus-change site), `docs/specs/2026-07-06-mvp-interfaces.md` (same commit)
- Test: `src/layout.rs` unit tests, `tests/server_proto.rs`

**Interfaces:** `Layout::focus_dir` signature changes (locked contract — amend `2026-07-06-mvp-interfaces.md` in the same commit). Recommended shape: `pub fn focus_dir(&mut self, dir: Direction, area: Rect, activity: &dyn Fn(PaneId) -> u64) -> bool` — caller (dispatch.rs:899) supplies a closure reading the server-side counter map. Keep `last_focused` field removal/retention decisions explicit in the contract text.

Spec: `docs/tmux-reference/panes-and-layout.md` §1.1 — (a) **edge rewrite**: when the focused pane is flush against the near window edge, the search edge flips to one past the far edge, so navigation wraps (Left from leftmost → candidates flush against the right edge; all four directions symmetric); candidate test = abut the edge exactly AND overlap the focused pane's perpendicular range (corner-touching counts — range extends through the border line); (b) **tie-break**: highest `active_point`, a monotonically-increasing counter stamped on every focus change (window.c:593/240) — replaces the single-slot MRU (follow-up #65).

- [ ] **Step 1: RED — unit tests:** `focus_dir_wraps_left_to_rightmost` (3-col layout, focus col 0, Left → rightmost col), `focus_dir_wraps_down_to_top`, `focus_dir_wrap_picks_most_recently_active_of_two_far_candidates` (right column split top/bottom; focus bottom-right, then left pane, then Left-wrap from... construct so wrap has 2 candidates and the more recently active wins), `focus_dir_three_candidates_ranked_by_activity` (the follow-up #65 case: 3 candidates, most-recent wins even when it isn't `last_focused`). Also **invert** the existing edge-case assertions that expect `false` at edges (`focus_dir_two_pane_horizontal` :1153-1162 asserts Right-at-edge → false; under wrap it becomes true and focus moves to the leftmost — update the expected values with computed comments). server_proto: `focus_wraps_at_window_edge` driving prefix-Left from the leftmost pane.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN:** rewrite the four adjacency arms with the edge-flip rule; add per-pane `active_point: u64` + `next_active_point: u64` on the server (stamp in every focus-change path: focus_dir commit, focus_next, focus_last, focus_pane/select-pane, mouse click focus, new-pane creation); tie-break = max activity among candidates (fallback: first in pane-index order when counters tie, e.g. all zero).
- [ ] **Step 4: Full unit + server_proto verification + clippy.**
- [ ] **Step 5: Amend the MVP contract (focus_dir signature + semantics; note follow-up #65 closed) and mark #65 resolved in docs/follow-ups.md. Commit** `feat(focus): edge-wrap directional navigation + tmux active_point MRU (closes follow-up #65)`.

### Task 4: Status-line parity rendering (justify, side styles, window formats, separator)

**Files:**
- Modify: `src/status.rs` (tab layout + spans), `src/options.rs` (`expand_format` window-scope additions if a used variable is missing)
- Test: `src/status.rs` unit tests (exact span assertions, existing style)

**Interfaces:** status.rs internals unconstrained; any new public getter already added in Task 2. If `expand_format` gains variables, note them in the command-config contract amendment (same commit).

Spec: `docs/tmux-reference/status-line-and-messages.md` — §status-justify positioning math (left/centre/right/absolute-centre; centre centres the window list in the space between left and right sections, absolute-centre centres in the full status width), §window-status-format/-current-format expansion per tab (with `#I`, `#W`, `#F` and `#[fg=..]` inline styles inside the format), §style layering (base `status-style`, then `status-left-style`/`status-right-style` per side, then per-tab window-status-style/current-style — later layers override), §window-status-separator between tabs (default single space).

- [ ] **Step 1: RED — unit tests with exact expected span values (computed in comments):** `status_justify_centre_positions_window_list`, `status_justify_right`, `status_justify_absolute_centre`, `window_status_format_expands_per_tab` (set format `' #I #W #F '`, assert the tab text for a 2-window session incl. current-window `*` flag), `window_status_current_format_used_for_current`, `side_styles_layer_over_status_style`, `window_status_separator_respected` (set to `"|"`).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN:** replace status.rs's hardcoded `#I:#W`-shaped tab rendering with per-window `expand_format` of window-status-format/-current-format (inline `#[..]` styles inside formats must style spans — reuse the existing status-left/right format+style machinery); apply justify offset math; layer side styles; join tabs with the separator option.
- [ ] **Step 4: Full verification + clippy** (status:: unit tests, server_proto smoke, visual sanity via existing render tests).
- [ ] **Step 5: Commit** `feat(status): status-justify, per-side styles, window-status-format/-current-format, separator`.

### Task 5: swap-window

**Files:**
- Modify: `src/cmd.rs` (ParsedCmd variant + table + resolve), `src/model.rs` (window-index swap primitive), `src/server/dispatch.rs` (handler), `docs/specs/2026-07-07-command-config-interfaces.md` + `2026-07-07-server-client-interfaces.md` if model surface changes (same commit)
- Test: `src/cmd.rs` + `src/model.rs` unit tests, `tests/server_proto.rs`

**Interfaces:** new `ParsedCmd::SwapWindow { src: Option<String>, dst: Option<String>, detach: bool }` (locked cmd contract — amend same commit). Model gains a swap primitive (amend server-client contract if public).

Spec: `docs/tmux-reference/windows-and-sessions.md` §swap-window — swaps the two winlinks' window objects; `-d` means focus **follows the window object**; without `-d`, focus stays on the index; `-s` defaults to current window, `-t` required target; target grammar supports relative `-1`/`+1` (wrapping) and `:N` index forms (the user binds `swap-window -d -t -1` / `-t +1`). Alert flags travel per spec.

- [ ] **Step 1: RED —** cmd parse tests (`swap-window -d -t -1`, `-t +1`, `-s :2 -t :4`, missing -t = usage error); model unit test for the swap primitive (windows keep ids, indices swap, current/last preserved per spec); server_proto `swap_window_relative_target_moves_current_window` + `swap_window_without_d_keeps_focus_on_index`.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN** (parse → dispatch → model), matching tmux's `-d` semantics exactly.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Amend contracts, commit** `feat(windows): swap-window with relative targets and -d focus semantics`.

### Task 6: Copy-mode mouse feel — part 1 (click purity, release targeting, drag-enters-copy-mode)

**Files:**
- Modify: `src/server/dispatch.rs` (`mouse_down` :1699-1736, `mouse_up` :1808-1816, root drag routing :1702-1705), `src/server.rs` (SelState if needed)
- Test: `tests/server_proto.rs`

**Interfaces:** none (private/`pub(super)`).

Spec: `docs/tmux-reference/mouse.md` — (a) plain click in copy mode = select-pane only, the copy cursor does NOT move and no selection anchor becomes visible (:537-539); anchor is installed at the **press position** only when the first Drag event arrives; (b) release dispatches DragEnd at the pane **under the pointer at release** — releasing over a different pane means no copy (:308-311, :654-658); (c) root-table `MouseDrag1Pane → copy-mode -M` (:488, :501): dragging on a live pane (not in copy mode, button 1) enters copy mode immediately with the anchor at the press point and selection following the drag; the subsequent release copies-and-cancels like any copy-mode drag (with `-M` semantics: entering this way, cancel returns to the live view).

- [ ] **Step 1: RED — server_proto tests:** `click_in_copy_mode_does_not_move_cursor` (enter copy mode, note cursor, click elsewhere in the pane, assert cursor unchanged + no selection highlight in the rendered frame, but pane focus follows); `drag_after_click_anchors_at_press_point` (existing selection assertions style); `release_over_other_pane_does_not_copy` (two panes; select in pane A by drag, release with coordinates inside pane B; assert paste buffer unchanged/absent); `drag_on_live_pane_enters_copy_mode_selecting` (no copy mode; press+motion in a pane with scrollback content; assert copy-mode indicator appears and a selection exists; release; assert buffer contains the dragged text and copy mode exited).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN:** defer cursor/anchor writes from `mouse_down` to the first `mouse_drag` motion (store pending press coords in the drag state instead); thread the release event's coordinates into `mouse_up` and hit-test — copy only when the release pane == the selecting pane; in the root path, btn-1 motion on a live pane starts copy-mode-with-selection (reuse the existing copy-mode entry + SelState machinery; anchor = press point).
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Commit** `fix(mouse): tmux click/release semantics in copy mode; drag on live pane enters copy mode`.

### Task 7: Copy-mode mouse feel — part 2 (edge autoscroll + word/line drag extension)

**Files:**
- Modify: `src/server.rs` (SelState kind + autoscroll timer state on CopyState/ClientState), `src/server/dispatch.rs` (`mouse_drag` :1753-1763, Tick handler, `select_word_at`/`select_line_at` :611-638)
- Test: `tests/server_proto.rs`

**Interfaces:** none.

Spec: `docs/tmux-reference/mouse.md` — autoscroll: while a drag is held with the pointer on the pane's first (or last) row, scroll 1 line per 50ms tick, extending the selection; motion outside the pane is a no-op and stops the timer (:590-624, :763). Word/line anchors: after DoubleClick/TripleClick sets a word/line selection, continued dragging snaps the moving end to word/line boundaries (SEL_WORD/SEL_LINE, :583-587, :636-640); word boundaries use the `word-separators` option default per `copy-mode-and-buffers.md`.

- [ ] **Step 1: RED:** `drag_at_top_row_autoscrolls_into_history` (pane with >1 page history at bottom; enter drag selection; move pointer to row 0 and hold; advance server Ticks; assert the view scrolled and the selection grew); `drag_after_double_click_extends_by_words` (double-click a word, drag into the middle of another word, assert the selection end snaps to that word's boundary); `drag_after_triple_click_extends_by_lines`.
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN:** add `SelKind { Char, Word, Line }` to SelState; snap logic in `mouse_drag`; edge-row detection arming an autoscroll deadline serviced by the existing 50ms `Tick` (same pattern as escape-time flushing).
- [ ] **Step 4: Full verification + clippy** (including the existing copy-mode/selection suite for regressions).
- [ ] **Step 5: Commit** `feat(mouse): drag autoscroll at pane edges; word/line drag extension after double/triple click`.

### Task 8: choose-tree — real tree view, current-item default selection, preview box

**Files:**
- Modify: `src/render.rs` (`ListOverlay`/`Overlay` extension: indented tree rows + preview blit + box chrome), `src/server.rs` (ChooseTreeState: tree model, expanded-set, preview mode), `src/server/dispatch.rs` (tree build, expand/collapse keys, default selection, preview source lookup + sizing; `v` key)
- Test: `src/render.rs` unit tests (exact cell assertions), `tests/server_proto.rs`

**Interfaces:** `Overlay`/`ListOverlay` are NOT in any locked contract (gap analysis §C) — free to extend; note the addition in the parity-polish spec's overlay section anyway for documentation parity (same commit, additive).

Spec: `docs/tmux-reference/choose-tree.md`. Three user-mandated behaviors, all per that doc:

**(a) Tree structure** (`prefix-s`): sessions are parent rows with their windows as indented child rows, tmux tree furniture (`+`/`-` expand markers, indentation); sessions start **expanded** per the doc's default-expansion rule (verify there — window children under each session); `Right`/`+` expands a collapsed session, `Left`/`-` collapses (per the doc's key table). `prefix-w` stays a window-list of the current session (its rows may show panes as children if the doc says so — follow the doc). Rows keep stable creation-order sorting (already correct).

**(b) Default selection = current item**: on open, the selection starts on the CURRENT session (`prefix-s`) / current window (`prefix-w`) — per mode_tree's current-item start behavior in the doc — not row 0.

**(c) Preview box**: sizing — NORMAL gives the list 2/3 of the pane height (or 1/2 when that's more than the item count needs), no preview when the remainder would be under the minimum; BIG gives the list 1/4 (min 2 rows, max item count); `v` cycles OFF→BIG→NORMAL→OFF (default NORMAL). Preview content: session item → filmstrip of that session's windows (each window's active pane blitted side-by-side, `│` dividers, each labeled); window item → filmstrip of its panes; raw cell copy (truncate, never scale), refreshed live each render tick. Box: single-line border across the top of the preview region with the selected item's title embedded.

Tagging and `O`/`r` sort-cycling remain deferred (Task 9 tickets them).

- [ ] **Step 1: RED — render unit tests with exact cell assertions:** `overlay_tree_rows_indent_children`; `overlay_preview_blits_grid_cells` (construct a small grid with known cells, a preview rect, assert composed cells + border row + title text); `overlay_preview_truncates_oversized_grid`; `overlay_list_shrinks_to_two_thirds_when_preview_on` (sizing math per the doc, computed in comments). server_proto: `choose_tree_sessions_show_window_children`; `choose_tree_default_selects_current_session` (create 3 sessions, attach to the middle one, open `s`, immediately Enter, assert we stayed on the current session — proving default selection); `choose_tree_collapse_hides_children_expand_restores`; `choose_tree_preview_shows_selected_windows_content` (marker string printed in a window's pane appears in the rendered preview region); `choose_tree_v_toggles_preview` (OFF→BIG→NORMAL observable via layout change).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN:** tree model on ChooseTreeState (rows carry depth + TreeTarget; expanded set keyed by session id, selection stays identity-stable via the existing `selected: Option<TreeTarget>` pattern); default-selection lookup at open; expand/collapse key handling; extend the overlay type with an optional preview block (cells snapshot + rect + title); server builds it each render pass from the selected row's target using the pass-1 blit pattern from `render.rs:171-194`; sizing math per spec; `v` cycles the mode.
- [ ] **Step 4: Full verification + clippy.**
- [ ] **Step 5: Commit** `feat(choose-tree): tree view with expand/collapse, current-item default selection, live preview box`.

### Task 9: e2e + docs closeout

**Files:**
- Modify: `tests/e2e_config.rs` (user-conf roundtrip), `tests/e2e_copy_mouse.rs` (drag-select e2e)
- Modify: `docs/follow-ups.md`, `CLAUDE.md`, `docs/overview.md`
- Test: full suite + fresh **debug** binary smoke (release exe is locked by the user's server)

**Interfaces:** none.

- [ ] **Step 1: e2e additions:** `user_tmux_conf_loads_without_errors` (drive winmux.exe with `-f tests/fixtures/user.tmux.conf`, assert no error banner and that `C-a` (the conf's prefix) + `|` splits — proving the conf actually applied); `mouse_drag_select_copies_release_text` (SGR byte injection: press-drag-release across known text, then paste and assert the shell echoes it — extends the existing e2e_copy_mouse harness).
- [ ] **Step 2: docs:** follow-ups — mark #64/#65 resolved; add new tickets: alerts subsystem (visual-*/bell-action/monitor-activity currently inert), clock-mode overlay (clock-mode-colour inert), choose-tree expand/collapse+tagging+`O`/`r` sort-cycling, choose-client/choose-buffer. CLAUDE.md — update the feature summary (SP6 wave: config compat batch, wrap navigation, mouse fixes, choose-tree preview, swap-window, status justify/formats) and the keybinding/option notes. overview.md — SP6 section.
- [ ] **Step 3: Full verification:** `cargo test` (all targets; `--test-threads=4` for server_proto if flaky), clippy, debug-binary smoke on a unique socket.
- [ ] **Step 4: Commit** `test(e2e)+docs: SP6 closeout — user-conf roundtrip, drag-select e2e, follow-ups/CLAUDE.md`.

### Task 10: clock mode (`prefix-t`)

**Files:**
- Modify: `src/cmd.rs` (`ParsedCmd::ClockMode` + table entry `clock-mode`, alias none, `-t` optional), `src/bindings.rs` (default `t` → `clock-mode` in the prefix table), `src/options.rs` (`clock-mode-style` Choice 12/24 default 24 — `clock-mode-colour` already exists from Task 2), `src/server.rs` + `src/server/dispatch.rs` (per-client clock overlay state + key handling), `src/render.rs` (clock overlay drawing)
- Modify: `docs/specs/2026-07-07-command-config-interfaces.md` (new ParsedCmd variant + getter, same commit)
- Test: cmd/render unit tests, `tests/server_proto.rs`

**Interfaces:** new `ParsedCmd` variant (locked cmd contract — amend same commit). Overlay types unconstrained.

Spec: `docs/tmux-reference/status-line-and-messages.md` §clock mode (window-clock.c): big-digit time centered in the pane, drawn in `clock-mode-colour`, format governed by `clock-mode-style` (12 → `%l:%M %p`, 24 → `%H:%M` — verify exact strings in the doc), refreshes on a timer (doc specifies the tick — winmux's 50ms/status tick is sufficient granularity; redraw when the displayed minute changes), any key exits (verify against the doc's exit rule; mouse per doc). Digit font: whatever the doc specifies (tmux's clock font table; winmux's display-panes 5x5 font may be reused only if the doc says they match — otherwise implement the doc's font).

- [ ] **Step 1: RED:** cmd parse test (`clock-mode` → variant; `t` in default bindings test); render unit test `clock_overlay_draws_big_digits` (fixed time injected, exact cell assertions for at least one digit + colon, computed in comments); server_proto `clock_mode_opens_and_any_key_exits` (send prefix-t, assert clock glyphs rendered; send a key, assert pane content restored).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN:** overlay state (like DisplayPanes), render pass, minute-change refresh via Tick, exit-on-any-key.
- [ ] **Step 4: Full verification + clippy.** Testability note: inject/freeze the displayed time through a seam (e.g. the overlay stores the formatted string at build time) so unit tests don't depend on wall-clock.
- [ ] **Step 5: Amend contract; commit** `feat(clock): clock-mode overlay with prefix-t, clock-mode-colour/style`.

### Task 11: active-pane border indication on shared dividers (half-border rule + pane-border-indicators)

**Files:**
- Modify: `src/render.rs` (border style attribution — the `touches_focused` adjacency logic at ~:243-272), `src/options.rs` (`pane-border-indicators` Choice off/colour/arrows/both, default colour)
- Modify: `docs/specs/2026-07-07-command-config-interfaces.md` (new getter, same commit)
- Test: `src/render.rs` unit tests (exact emitted-SGR assertions per existing style)

**Interfaces:** render internals unconstrained; new option getter → contract amendment.

Spec: `docs/tmux-reference/panes-and-layout.md` §7.1 (lines ~820-836) + §7.4. The user-visible bug: in a 1:1 two-pane split winmux paints the whole shared divider in the active style, indicating nothing. tmux splits a shared border **cosmetically**: for a LEFTRIGHT (side-by-side) split, divider cells with `wy <= sy/2` are styled as the LEFT pane's border and the rest as the RIGHT pane's; for TOPBOTTOM, the left half (`wx <= sx/2`) belongs to the TOP pane, the rest to the BOTTOM — so exactly half the divider is green, the half attributed to the active pane. `pane-border-indicators`: `off` = no activity indication (base style everywhere); `colour` (default) = the half-border colouring; `arrows`/`both` = four arrow glyphs on the active pane's border just inside each corner, pointing at the active pane (doc §7.4 has the exact glyph/position rules; use U+2190..U+2193 per the doc's Windows note). `both` = colour + arrows.

- [ ] **Step 1: RED — render unit tests with exact cell/SGR assertions (computed in comments):** `two_pane_vertical_divider_half_styled` (side-by-side split, left pane active → top half of divider green, bottom half default; focus right → inverted), `two_pane_horizontal_divider_half_styled` (analogous for top/bottom), `border_indicators_off_suppresses_active_styling`, `border_indicators_arrows_draws_glyphs_at_active_corners` (assert the four glyph positions/directions per doc).
- [ ] **Step 2: Run, record RED.**
- [ ] **Step 3: GREEN:** rework border-cell style attribution per §7.1 (per-cell: determine owning pane for shared segments via the half rule, style by whether the OWNER is the focused pane), gate by the option; arrows pass after borders.
- [ ] **Step 4: Full verification + clippy** — expect existing border tests asserting whole-divider-active to legitimately invert; update them with computed comments (sanctioned edit, mirrors tmux).
- [ ] **Step 5: Amend contract; commit** `feat(render): tmux half-border active indication + pane-border-indicators`.

---

## Self-review notes

- Spec coverage: gap §D→T1(+1b), §A→T2 (accept/store) + T4 (render) + T5 (swap-window), §B→T3, §E→T6+T7, §C→T8 (expanded per user: tree view + current-item default + preview), user-adhoc→T10 (clock) + T11 (half-border indication), deferrals→T9.
- Execution order: T1, T1b, T2, T3, T4, T5, T6, T7, T8, T10, T11, then T9 (closeout LAST — its docs/e2e cover everything before it).
- Contract discipline: T2/T5/T10/T11 (command-config), T3 (mvp) amend specs in-commit; T1/T6/T7 touch only private surface; T8 additive documentation.
- Existing-test-edit sanction: Task 3's edge-case assertions (wrap legitimately inverts them) and Task 11's whole-divider-active border tests (half rule legitimately inverts them); every other task adds tests.
