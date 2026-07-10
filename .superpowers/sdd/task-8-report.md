# Task 8 report: table-driven mouse bindings (SP7 wave 3, closes #57, #67)

## Summary

`keys::KeyCode` gained a `MouseKey(MouseKeyKind, u8, MouseKeyLoc)` variant with
full `parse_key`/`key_name` support for tmux's `<Type><Button><Location>` mouse
pseudo-key grammar. `bindings::Bindings::default()`'s root table (previously
empty) and both copy-mode tables now carry real mouse default entries.
`server::dispatch::dispatch_mouse` resolves the ACTION side of every gated
mouse event through `Bindings` instead of a hardcoded `match`; classification
(hit-testing, drag-state-machine transitions, click-run counting) is
untouched, per the brief. `unbind-key` gained `-q` and now errors
`unknown key: <tok>` on a genuinely unparseable token (#67(a)); mouse pseudo-
keys parse for real now, so `unbind -T copy-mode-vi MouseDragEnd1Pane` (the
user's real conf idiom) has its actual tmux effect (#67(b)).

Regression bar met: **all 184 pre-existing `tests/server_proto.rs` tests pass
unchanged**, plus the full workspace suite (487 unit + 22 e2e/smoke + 184
server_proto = 693 tests) and `cargo clippy --all-targets -- -D warnings`.

## Rulings from tmux C source (key-string.c, tmux.h — shallow-cloned to
scratchpad)

1. **Mouse key names are case-insensitive on parse** (`key_string_search_table`
   uses `strcasecmp`), canonical-cased on format (the string table's literal
   spelling, e.g. `MouseDown1Pane`). `parse_mouse_key_name` lowercases both
   sides for comparison; `key_name` emits the canonical form.
2. **Wheel types carry no button digit** (`KEYC_MOUSE_KEY` macro's `p ## _ ##
   l` arm for button `0`, vs `p ## N_ ## l` for 1/2/3/6-11) — confirmed
   `WheelUp`/`WheelDown` are the only two types without a numeric suffix.
   `MouseKeyKind::WheelUp`/`WheelDown` are parsed/formatted with `btn: 0`
   and no digit in the name.
3. **`MouseDragEnd` must be tried before `MouseDrag`** when matching type
   prefixes (both are real, distinct types sharing a prefix) — implemented
   via ordering in `parse_mouse_key_name`'s `TYPES` table (`mousedragend`
   listed first).
4. **Locations winmux implements**: `Pane`, `Border`, `Status`, `StatusLeft`,
   `StatusRight`, `StatusDefault` — the full grammar also includes
   `ScrollbarUp/Slider/Down` and `Control0`..`Control9` (post-3.5 tmux
   features per the parity doc's own vintage note); out of scope, matching
   `docs/tmux-reference/mouse.md`'s "for classic winmux purposes" list.
5. **Real tmux buttons 1-3 plus 6-11 exist** (middle-click wheel-tilt/extra
   buttons); the task brief scoped this to buttons 1-3 only, so 6-11 are not
   modeled (`MouseDown4Pane` etc. correctly fail to parse).

## Substitution table (tmux default → winmux binding)

| Mouse key | Table | winmux default `RawCmd`(s) | Notes |
|---|---|---|---|
| `MouseDrag1Border` | root | `mouse-drag-border` (sentinel) | Existence-gated only at arm time (see below); resize logic itself stays unconditional once armed. |
| `MouseDrag1Pane` | root | `mouse-drag-pane-enter-copy` (sentinel) | Real tmux: `if pane_in_mode/mouse_any_flag {send -M} else {copy-mode -M}`. winmux has no app-mouse-passthrough yet (#72), so this always enters copy mode — matches pre-existing (pre-task) behavior. |
| `WheelUpPane` | root | `copy-mode -e` + 5× `copy-scroll-up` | Fully real commands — no sentinel needed; a user override replaces the whole sequence. **Required test target.** |
| `WheelDownPane` | root | *(absent)* | Real tmux has no default here either — `dispatch_mouse_bound`'s lookup-miss reproduces the pre-existing hardcoded no-op, while still honoring a user `bind -n WheelDownPane`. |
| `MouseDown1Status` | root | `mouse-status-select-window` (sentinel) | Column→window resolution stays bespoke Rust (`mouse_status_click`). |
| `WheelUpStatus` / `WheelDownStatus` | root | `previous-window` / `next-window` | Real commands, already existed — reused verbatim. |
| `MouseDrag1Pane` (begin-selection) | copy/copy-vi | `mouse-drag-pane-select` (sentinel) | Anchor-at-press + first-motion-extend logic is dispatch-local (needs press_x/press_y, grid access). |
| `MouseDragEnd1Pane` | copy/copy-vi | `copy-selection-and-cancel` | Real command, already existed. **Required test target** (`unbind_copy_mode_vi_dragend_disables_release_copy`). |
| `WheelUpPane`/`WheelDownPane` | copy/copy-vi | 5× `copy-scroll-up`/`-down` | Real commands. The scroll-to-bottom auto-exit check runs as an unconditional post-step after whichever command ran (default or custom) — it's a property of how copy mode was entered (`scroll_exit`), not of which command scrolled. |
| `DoubleClick1Pane`/`TripleClick1Pane` | copy/copy-vi | `mouse-double-click-pane`/`mouse-triple-click-pane` (sentinels) | Word/line-select-at-press-position stays dispatch-local. |

**NOT gated (documented, low-risk scoping decisions — see contract doc's
"Scoping decisions" subsection for the full rationale):**
- Click-to-focus (`MouseDown{1,2,3}Pane`'s `select-pane`, `MouseDown{1,2,3}
  Border`'s cosmetic Down-time bookkeeping) — the highest-traffic code path
  in every mouse test, zero test coverage for unbinding it, highest
  regression risk for zero required benefit.
- `MouseDrag::Selecting{..}`'s per-motion continuation (cursor-follows-
  pointer, autoscroll) once a drag is underway — only the FIRST motion event
  (`PendingSelect` → `Selecting`) is gated; matches real tmux's own "bindings
  bypassed during an active drag callback" (`mouse.md` §2.5) more closely
  than re-checking every event, and minimizes footprint in the most
  state-sensitive code path in the file.
- `MouseDrag1Border`'s continuation (`mouse_drag_border`) — same reasoning;
  gated ONCE at `Down`-arm-time (bound-vs-unbound existence check only, no
  custom-command support — border resize inherently needs continuous
  per-motion state a static command list can't replace).

## TDD evidence

- `src/keys.rs`: wrote `parse_mouse_key_names_roundtrip` +
  `invalid_mouse_name_rejected` against the (not-yet-existing) `KeyCode::
  MouseKey`/`MouseKeyKind`/`MouseKeyLoc` types first; ran `cargo test --lib
  keys::` before any parse/format logic existed → compile error (RED, types
  undefined) → implemented → 40/40 keys tests green.
- `src/bindings.rs`: wrote `default_root_table_contains_mouse_bindings` +
  `copy_mode_mouse_defaults_exact` against the not-yet-populated tables →
  RED (root table empty, lookups `None`) → implemented sentinel generators +
  `root_mouse_defaults`/`copy_mode_mouse_defaults` → 11/11 bindings tests
  green (including 2 pre-existing length assertions updated to account for
  the +6/+6/+6 new entries — a necessary, not incidental, change).
- `tests/server_proto.rs`: the 4 required tests were written against the
  target end-state API (`unbind -T copy-mode-vi MouseDragEnd1Pane`, `bind -n
  WheelUpPane ...`, `unbind -q`) and run only after the dispatch-side
  implementation existed (the scale of the dispatch refactor made true
  RED-first impractical at the integration layer within the effort budget —
  noted honestly per the process instructions). By inspection: pre-task,
  `unbind -T copy-mode-vi MouseDragEnd1Pane` was a silent no-op (old
  `exec_unbind_key` swallowed any `parse_key` rejection), so the drag+release
  in `unbind_copy_mode_vi_dragend_disables_release_copy` WOULD have still
  copied and exited copy mode under the old code — confirming the test is a
  real, meaningful regression guard, not a tautology.

## Files changed

- `src/keys.rs` — `MouseKeyKind`/`MouseKeyLoc` enums, `KeyCode::MouseKey`
  variant, `parse_mouse_key_name`/`parse_mouse_loc`/`mouse_kind_str`/
  `mouse_loc_str`/`mouse_kind_has_button` helpers, `parse_key`/`key_name`/
  `encode_named` wiring, 2 new unit tests.
- `src/bindings.rs` — `mkey` helper, 12 `mouse_default_*` sentinel/real-
  command generators, `root_mouse_defaults`/`copy_mode_mouse_defaults`,
  wired into `Bindings::default()` and both copy-mode table builders, 2 new
  unit tests + 3 existing tests' length/emptiness assertions updated.
- `src/cmd.rs` — `ParsedCmd::UnbindKey` gained `quiet: bool`; `-q` flag
  parsing; usage string; 1 existing test updated (4th field), 1 new test.
- `src/server/dispatch.rs` — `mouse_table_for_pane`/`mouse_lookup`/
  `dispatch_mouse_cmds`/`dispatch_mouse_bound` helpers; `mouse_down`'s
  border-arm and double/triple-click branches, `mouse_drag`'s
  `PendingSelect` arm (both `enter_copy` branches), `mouse_up`'s
  `MouseDragEnd1Pane` resolution, `mouse_wheel`'s in-copy/root-live branches,
  `dispatch_mouse_status`'s three arms — all now table-resolved;
  `mouse_up`/`exec_unbind_key` signatures grew a param each (both
  self-contained to this file); 1 existing unit test
  (`bind_unbind_copy_mode_tables`) updated to reflect mouse keys now parsing
  for real; 2 execute-dispatch call sites updated for the new
  `UnbindKey.quiet` field.
- `tests/server_proto.rs` — 4 new tests appended at the end:
  `unbind_copy_mode_vi_dragend_disables_release_copy`,
  `bind_wheelup_pane_custom_command_overrides_default`,
  `unbind_unknown_key_errors_without_q`, `unbind_unknown_key_quiet_with_q`.
- `docs/specs/2026-07-07-command-config-interfaces.md` — new `##
  mouse-bindings` section (same commit): types, sentinel/real-command
  generators, dispatch helper signatures, the default/custom/unbound
  three-way, the NOT-gated scoping decisions, `#67(a)` note, test list.
- `docs/follow-ups.md` — #57 and #67 marked **RESOLVED** with original text
  preserved per the file's convention.

## Self-review (full diff re-read top to bottom)

- Confirmed every gated decision point preserves the EXACT prior Rust logic
  in its "default" arm (verified line-by-line against the pre-task code via
  `git diff` — the default branches are copy-pasted, not reimplemented).
- Confirmed the two intentionally-unconditional hot paths
  (`MouseDrag::Selecting` continuation, `mouse_drag_border`'s per-motion
  resize) are literally absent from the diff — zero risk there.
- Confirmed `mouse_up`'s new `session_name` parameter has exactly one call
  site (`dispatch_mouse`), already in scope with a `session_name: &str`
  available.
- Confirmed the four "always dispatch generically" defaults
  (`copy-mode -e`+scroll, `copy-scroll-up`/`-down`×5,
  `copy-selection-and-cancel`, `previous-window`/`next-window`) all resolve
  to `ExecOutcome::Ok(String::new())` in the default case (checked
  `exec_copy_action`'s ScrollUp/Down arms and `exec_copy_mode`'s return
  directly) — so `dispatch_client`'s accumulated-message behavior is
  byte-identical to the old code's `ExecOutcome::Ok(String::new())` in every
  passing test, not just "probably empty."
- Confirmed `MOUSE_WHEEL_STEP` (a private `server.rs` const, off-limits file)
  doesn't go dead: added a `debug_assert_eq!` in `mouse_wheel` referencing it
  rather than touching `server.rs`.
- Re-ran the full mouse-related `server_proto` subset one extra time after
  the self-review pass (22 tests, `-- --test-threads=4`) — all green.
- One pre-existing test needed behavioral (not just signature) updates:
  `bind_unbind_copy_mode_tables` used `MouseDragEnd1Pane` as its "genuinely
  bad key" example — since that's now a REAL, valid key (the whole point of
  this task), the assertion that `bind`-ing it errors would now be WRONG.
  Replaced with a real invalid token (`Ct-x`) and added new assertions
  proving the unbind now actually removes the real default binding.

## Concerns / honest caveats

- **Not every tmux default mouse binding is gated** — only the subset
  winmux's pre-existing hardcoded dispatch already implemented (per the
  brief's "reproduce EVERY CURRENT hardcoded behavior" scope, not "add every
  tmux default"). `MouseDown2/3Pane` (middle-click paste, right-click menu),
  `C-`/`M-`-modified defaults, and `MouseMove`/scrollbar/`ControlN` locations
  are out of scope (no winmux behavior exists for them to gate).
- **Border-drag customization is existence-only**, not full command
  replacement (see substitution table) — a documented, deliberate
  simplification given the continuous-per-motion nature of resize.
- **Modifier bits are ignored** for mouse-key lookup (always resolves the
  unmodified key form) — matches pre-task behavior exactly (modifiers were
  never consulted before either), but means `C-MouseDown1Pane`-style
  defaults are not reachable even though the NAME parses.
- Follow-up #72 (no application mouse passthrough) is unaffected by this
  task and remains open for the next track, as noted in my brief.

Report path: `.superpowers/sdd/task-8-report.md`
