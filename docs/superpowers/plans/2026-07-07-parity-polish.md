# Parity Polish Implementation Plan (sub-project 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** tmux's remaining core parity: scrollback + copy mode + paste buffers, mouse, layout presets, break/swap/rotate/move/find window, choose-tree, display-panes, escape-time, automatic-rename.

**Architecture:** Design spec `docs/specs/2026-07-07-parity-polish-design.md` is the requirement of record — each task below names its spec sections. Contract file `docs/specs/2026-07-07-parity-polish-interfaces.md` created in Task 1, extended per task; amendments to prior contracts where locked surfaces change (grid, layout, render, input/keys/bindings, options, server, host).

**Tech Stack:** Rust 2021, no new dependencies.

## Global Constraints

- All existing tests stay green UNCHANGED at every commit (they pin SP1-3 behavior); `cargo clippy --all-targets -- -D warnings` clean; full `cargo test` green (server_proto may need `--test-threads=4` under full parallel load — pre-existing, documented).
- `cargo` at `~/.cargo/bin` (`export PATH="$HOME/.cargo/bin:$PATH"`).
- tmux fidelity per the design spec's verified tables; deviations only where the spec declares them.
- Tests spawning servers: unique `-L` sockets, `-f -` isolation, kill-server teardown; never the default socket.
- Never wreck the user's terminal: mouse-mode disable sequences must be part of client terminal restore (Task 5 amends host restore).
- Commit per green task, conventional message + `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: Grid scrollback, real alt screen, OSC titles — spec §1

**Files:** Modify `src/grid.rs`, `src/server.rs` (spawn sites pass `options.history_limit()`; add the getter to `src/options.rs`), tests inline; Create contract file with `## grid-v2` section; amend MVP contract grid section (superseded note).
**Produces:** `Grid::new(cols, rows, history_limit: u32)`; `history_len()`, `view_cell(scroll_back, col, row) -> Cell`, `view_row_text(scroll_back, row) -> String`, `title() -> Option<&str>`, `take_title_changed() -> bool`. All existing `Grid::new(c, r)` callers updated (tests may use a `Grid::new(c, r, 0)`-style explicit 0). Alt-screen save/restore per spec; scrollback capture rules per spec (full-region top only, not in alt); chunked eviction max(1, limit/10).
- [ ] TDD: failing tests `scrollback_captures_scrolled_lines` (feed >rows lines, assert view_cell(1,..) shows the line above), `scrollback_eviction_chunked`, `alt_screen_saves_and_restores_primary`, `alt_screen_no_history`, `osc_title_captured` (`\x1b]0;hello\x07` → title Some("hello"), take_title_changed true once), `osc2_and_st_terminator`, `view_cell_clamps`, `history_limit_zero_disables`. Existing `alt_screen_clears_and_homes` + `osc_and_unknown_ignored` tests UPDATED (behavior legitimately changes — the ONLY sanctioned existing-test edits in this plan; keep assertions equivalent-or-stronger).
- [ ] Implement → green → full suite + clippy → contracts → commit `feat(grid): scrollback, real alternate screen, OSC title capture`.

### Task 2: Copy mode core — spec §2 (movement/scroll/render; selection is Task 3)

**Files:** Modify `src/input.rs` (WhichTable::{CopyMode, CopyModeVi}), `src/bindings.rs` (two new default tables — movement/scroll/cancel subset), `src/cmd.rs` (copy-mode command + `send-keys -X` alias mapping + internal copy-* commands), `src/server.rs`/`src/server/dispatch.rs` (ClientMode::Copy(CopyState), table override on Key events, copy-* execution, entry/exit incl. stale-invalidation on pane death/window switch), `src/render.rs` + `src/status.rs`? (PaneView copy view: scrolled content via view_cell, position indicator top-right in mode-style, cursor at copy cursor), `src/options.rs` (`mode-style` option + `mode_keys_vi()` getter), `tests/server_proto.rs`.
**Scope:** enter `[`/`copy-mode`/PPage `-u`; all cursor movement + scroll + page/halfpage + top/bottom/H/M/L commands for BOTH tables per spec; `q`(+emacs Escape) cancel; position indicator `[scroll/history]`; `-e` flag stored (used by mouse later; keyboard `-u` entry tested). NO selection yet.
- [ ] TDD server_proto: `copy_mode_enters_and_indicator` (`\x02[` → indicator `[0/N]` top-right of pane in yellow bg), `copy_mode_scroll_shows_history` (fill >24 lines with numbered output, `\x02[` then vi `k`*3 → older lines visible, indicator `[3/N]`), `copy_mode_page_keys`, `copy_mode_q_exits` (live screen restored), `copy_mode_vi_vs_emacs_tables` (`set -g mode-keys vi` → `h` moves; emacs default → `C-b` moves), `copy_mode_prefix_still_works` (`\x02[` then `\x02c` creates window, copy mode canceled on switch), `copy_mode_pane_death_cancels`. Unit: bindings default-table content test per table.
- [ ] Implement → green → contracts (`## copy-mode` + input/bindings/cmd/server amendments) → commit `feat(copy-mode): scrollback navigation with emacs/vi tables`.

### Task 3: Selection + paste buffers — spec §2 selection + §3

**Files:** Modify `src/server/dispatch.rs` (+`src/server.rs`), `src/render.rs` (selection highlight pass via mode-style), `src/cmd.rs` (paste-buffer/list-buffers/delete-buffer/set-buffer + aliases), new `src/buffers.rs` (pure Buffers store), `src/bindings.rs` (Space/v/Enter/Escape/o vi + C-Space/C-w/M-w/R/C-g/o emacs + `]` `#` `-` prefix bindings), `src/options.rs` (`buffer-limit`), `tests/server_proto.rs`.
- [ ] TDD: unit buffers.rs (auto-naming buffer%u never reuses, eviction automatic-only, manual exempt); server_proto `copy_selection_to_buffer_and_paste` (copy-mode, begin-selection, move, Enter → buffer created; `\x02]` pastes into the shell — screen echoes), `rectangle_selection`, `selection_highlight_styled` (cells in selection have mode-style bg), `list_buffers_format` (`buffer0: N bytes: "..."`), `delete_buffer_newest`, `set_buffer_named_exempt_from_eviction`, `other_end_swaps`, `clear_selection_keeps_mode`.
- [ ] Implement → green → contracts → commit `feat(copy-mode): selection, rectangle, and tmux paste buffers`.

### Task 4: Copy-mode search — spec §2 search

**Files:** Modify server/dispatch (search prompt via existing Prompt machinery — PromptKind::CopySearch{backward}), copy-* search commands, `src/bindings.rs` (`/` `?` `n` `N` vi; `C-s` `C-r` `n` `N` emacs), `tests/server_proto.rs`.
**Semantics:** literal case-insensitive substring over view rows from cursor (wrapping through history); found → move cursor + scroll to line; not found → transient `no match: <p>`? tmux shows nothing special — use transient message (documented). `n`/`N` repeat with stored pattern.
- [ ] TDD: `copy_search_finds_in_history`, `copy_search_backward`, `copy_search_next_wraps`, `copy_search_no_match_message`.
- [ ] Implement → green → contracts → commit `feat(copy-mode): search`.

### Task 5: Mouse — spec §4

**Files:** Modify `src/keys.rs` (SGR decode → mouse variant; contract-documented DecodedInput change), `src/input.rs` (KeyInputEvent::Mouse), `src/server.rs` (mode-enable bytes on option toggle+attach; routing/hit-testing incl. border drag resize, status clicks, wheel, copy-mode drag/double/triple click, alt-screen wheel→arrows), `src/client.rs`/`src/host.rs` (restore writes mouse-off sequences — host contract amendment), `src/options.rs` (`mouse()` getter), `tests/server_proto.rs` + keys unit tests.
- [ ] TDD: keys unit `decode_sgr_mouse_press/release/drag/wheel` (exact events incl. mods); server_proto `mouse_option_emits_enable_sequences` (set -g mouse on → next Output contains `\x1b[?1000h\x1b[?1002h\x1b[?1006h`; off → `l`), `mouse_click_focuses_pane` (split, click in second pane's cell → cursor/focus moves — assert via border active-style change), `mouse_wheel_enters_copy_mode` (indicator appears, scrolled 5), `mouse_drag_selects_and_release_copies` (buffer created), `mouse_status_click_selects_window`, `mouse_wheel_status_cycles_windows`, `mouse_border_drag_resizes` (border column moved), `alt_screen_wheel_sends_arrows` (hard to fake alt-screen via shell — feed a pane `\x1b[?1049h` via a command? use `printf`-equivalent PowerShell escape output; if too flaky, cover via unit-level routing test and note it).
- [ ] Implement → green → contracts → commit `feat(mouse): SGR mouse decoding, routing, and mode management`.

### Task 6: Layout presets + swap/rotate — spec §5

**Files:** Modify `src/layout.rs` (apply_preset, swap_panes, rotate — contract amendments), `src/model.rs` (Window.last_layout: Option<u8> for next-layout cycle), `src/cmd.rs` + dispatch (select-layout/next-layout/swap-pane/rotate-window), `src/bindings.rs` (Space, M-1..M-5, `{`,`}`, C-o, M-o), `src/options.rs` (main-pane-width/height), tests.
- [ ] TDD: layout unit tests per preset (exact rects for 2/3/5 panes at 80x24 incl. tiled rows-first shape, main-pane clamps, MIN respected), swap/rotate leaf-permutation + focus rules; server_proto `space_cycles_layouts` (border pattern changes even-h → even-v), `select_layout_by_name`, `swap_pane_braces`, `rotate_window_ctrl_o`, `main_pane_width_option_respected`.
- [ ] Implement → green → contracts → commit `feat(layout): tmux preset layouts, swap-pane, rotate-window`.

### Task 7: Window ops: break/move/find/' — spec §6

**Files:** dispatch + cmd (break-pane/move-window/find-window), new PromptKinds (move-window `.`, find-window `f`, index `'`), bindings (`!`, `.`, `f`, `'`), model (window re-index helper for move-window), tests.
- [ ] TDD server_proto: `break_pane_bang` (split, `\x02!` → 2 windows, new one current, pane count back to 1 each), `break_pane_last_pane_refused`? (tmux: breaking the only pane of the only window = error `can't break with only one pane`; if >1 window it moves — implement + test both), `move_window_dot_prompt` (`\x02.` `5\r` → index 5 in status), `move_window_occupied_errors` (`index in use: <n>`), `move_window_dash_k_kills`, `find_window_f_prompt` (match by name + by content; no match message), `quote_prompt_selects_index`.
- [ ] Implement → green → contracts → commit `feat(window): break-pane, move-window, find-window, index prompt`.

### Task 8: Overlays: display-panes + choose-tree — spec §7

**Files:** `src/render.rs` (Scene.overlay: Option<Overlay> — List + PaneDigits painting incl. 5x5 digit bitmaps), server/dispatch (ClientMode::ChooseTree + display-panes timed overlay + digit-key handling; choose-tree byte routing like prompts), cmd (`choose-tree [-s|-w]`, `display-panes [-d]`), bindings (`w`, `s`, `q`), options (display-panes-time/-colour/-active-colour), tests.
- [ ] TDD: render unit `overlay_list_paints_rows_and_selection`, `overlay_digits_5x5` (exact block cells for digit 1 in a known rect, bg red for active); server_proto `display_panes_q_shows_digits_and_selects` (split, `\x02q`, digit `1` → focus second pane; timeout auto-dismiss with -d 200 tested via wait), `choose_tree_w_lists_and_switches` (2 windows: `\x02w` → rows visible, Down+Enter switches window), `choose_tree_s_sessions` (2 sessions), `choose_tree_escape_cancels`, `choose_tree_x_kills_with_confirm`.
- [ ] Implement → green → contracts → commit `feat(overlay): display-panes and choose-tree`.

### Task 9: escape-time + automatic-rename — spec §8-9

**Files:** input.rs/keys.rs (pending-ESC age + flush_now — contract), server (Tick check per client; title polling → auto-rename with 500ms throttle; `#T`/`#{pane_title}` in expand_format ctx), model (Window.auto_rename flag; manual rename clears it — verify existing rename paths), options (escape_time()/automatic_rename() getters), tests.
- [ ] TDD: unit KeyMachine `lone_escape_flushes_after_escape_time`; server_proto `escape_key_reaches_pane` (send `\x1b` alone; after escape-time the pane receives it — assert via PowerShell? hard; use a unit-level KeyMachine test + a server test that lone ESC followed 600ms later by `[A` yields Escape THEN the CSI as separate — assert no stall), `pane_title_updates_window_name` (PowerShell `$Host.UI.RawUI.WindowTitle='mytool'` → within throttle window status shows `0:mytool*`), `manual_rename_disables_auto` (rename then title change → name stays), `pane_title_format_expands` (`display-message '#T'`).
- [ ] Implement → green → contracts → commit `feat(input,name): escape-time disambiguation and automatic-rename from ConPTY titles`.

### Task 10: e2e + docs closeout

**Files:** Create `tests/e2e_copy_mouse.rs` (e2e: copy-mode roundtrip through the real binary — fill history, `[`, navigate, select, copy, paste, assert echoed; mouse SGR bytes injected through the test ConPTY stdin — click focuses pane), docs: overview (SP4 delivered — PROJECT COMPLETE), design-spec header, CLAUDE.md (final state: full feature list, copy-mode/mouse notes, new modules/options), follow-ups.md (SP4 deferrals list from the spec).
- [ ] TDD e2e; full verification incl. release build + smoke; commit `test(e2e): copy-mode and mouse round-trips; docs for sub-project 4`.

---

## Self-review notes

- Spec coverage: §1→T1, §2→T2-4, §3→T3, §4→T5, §5→T6, §6→T7, §7→T8, §8-9→T9, deferrals→T10 docs. 
- Order rationale: grid first (everything reads it); copy-mode before mouse (mouse reuses copy machinery); overlays after prompts-pattern maturity; escape-time last among features (touches the decoder everyone else uses — minimize rebase pain).
- Existing-test-edit sanction: ONLY Task 1's two grid tests (alt-screen/OSC no-op pins) — every other task adds.
