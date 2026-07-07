# Sub-project 4 — Parity polish: copy mode, mouse, overlays, layouts, the long tail

Status: **Active design spec.** Companion contract:
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md)
(created/extended task-by-task, same lock rules). Behavioral ground truth
below was verified against tmux master source (2026-07-07); classic (≤3.5)
defaults chosen where master diverged.

## Scope

Deliver: grid scrollback + real alt screen + OSC titles · copy mode (emacs+vi
tables, selection incl. rectangle, search, position indicator) · paste
buffers · mouse (`mouse on`) · layout presets + swap/rotate · break/move/find
window + `'` prompt · choose-tree (`w`/`s`) · display-panes (`q`) ·
escape-time · automatic-rename. Documented deferrals at the end.

## 1. Grid: scrollback, alt screen, titles

- `Grid::new(cols, rows, history_limit: u32)` (contract change; 0 = none).
  Server passes `options.history_limit()` at pane spawn (read once, tmux
  semantics; later `set -g history-limit` affects only new panes).
- Scrollback `VecDeque<Vec<Cell>>` capturing lines dropped by `scroll_up`
  ONLY when the scroll region is the full screen top (`scroll_top == 0`) and
  the grid is NOT in alt-screen mode. Eviction: when len ≥ limit, drop oldest
  `max(1, limit/10)` in one chunk (tmux `grid_collect_history`).
- Real alternate screen (`CSI ? 1049 h/l`): save/restore the primary cells +
  cursor; alt buffer accrues NO scrollback; leaving restores the primary
  exactly. `1047`/`1048` not required (document).
- NO reflow on resize (tmux ≥1.9 reflows; documented winmux divergence,
  ticket). Scrollback lines are clipped/padded to the new width lazily on
  read.
- OSC 0/2 capture: `osc_dispatch` stores the title (UTF-8, control chars
  stripped, cap 256 chars); `Grid::title() -> Option<&str>` +
  `Grid::take_title_changed() -> bool` (edge-triggered flag polled by the
  server after `feed`).
- New read API for copy mode: `Grid::history_len() -> u32`,
  `Grid::line(offset_from_history_start) …` — concretely:
  `Grid::view_cell(scroll_back: u32, col: u16, row: u16) -> Cell` where
  `scroll_back` = lines scrolled up from live bottom (0 = live screen);
  plus `Grid::view_row_text(scroll_back, row) -> String` convenience for
  search. Clamp out-of-range to blank cells.

## 2. Copy mode

- Per-client state (`ClientMode::Copy(CopyState)`): `scroll: u32` (== tmux
  `oy`, 0 = bottom), `cx, cy: u16` (cursor in view coords), `sel: Option<SelState{
  anchor_scroll, anchor_x, anchor_y, rect: bool}>`, `scroll_exit: bool`,
  `search: Option<SearchState{pattern, backward}>`.
  NOTE tmux models copy mode per-PANE; winmux models it per-CLIENT bound to
  the focused pane at entry (`pane: PaneId`) — divergence documented (two
  clients can copy independently; pane death cancels copy mode via the
  existing stale-invalidation sweep).
- Entry: `copy-mode` command (`[` binding), `copy-mode -u` (`PPage` binding,
  enter + page up), `copy-mode -e` (wheel, Task: mouse). Alt-screen pane:
  works but history_len is 0.
- Key handling: NEW key tables `copy-mode` and `copy-mode-vi`
  (`WhichTable::CopyMode | CopyModeVi`). While `ClientMode::Copy`, the
  server overrides the table on `KeyInputEvent::Key` events to the copy
  table chosen by `mode-keys` BEFORE bindings lookup (prefix still works: a
  prefix-table Key event still resolves prefix bindings — tmux allows
  prefix commands while in copy mode). Unbound key in copy tables: swallow.
- Copy-mode commands are `send-keys -X <cmd>` in tmux; winmux implements
  them as first-class commands dispatched from bindings:
  `copy-cursor-{left,right,up,down}`, `copy-{start,end}-of-line`,
  `copy-history-{top,bottom}`, `copy-{top,middle,bottom}-line`,
  `copy-scroll-{up,down}` (1 line), `copy-halfpage-{up,down}`,
  `copy-page-{up,down}`, `copy-{next,previous}-word`, `copy-next-word-end`,
  `copy-begin-selection`, `copy-rectangle-toggle`, `copy-other-end`,
  `copy-clear-selection`, `copy-selection-and-cancel` (the "copy" action),
  `copy-cancel`, `copy-search-forward`, `copy-search-backward`,
  `copy-search-again`, `copy-search-reverse`. (Internal command names —
  hidden from `unknown command` suggestions but bindable; contract lists
  them. tmux's `send-keys -X` spelling accepted as alias syntax:
  `send-keys -X cancel` maps to `copy-cancel`, etc. — mapping table in
  contract.)
- Default tables (exact tmux subset):
  - emacs (`copy-mode`): arrows + `C-b/C-n/C-p/C-f` move; `C-a`/`C-e`/Home/
    End line; `M-<`/`M->` history top/bottom; `M-v`/`C-v`/PPage/NPage pages;
    `Space` page-down? NO — emacs Space = page-down in tmux master; keep
    tmux: `Space` → page-down? VERIFIED: emacs Space = `page-down`. But
    emacs begin-selection = `C-Space`. Copy = `C-w`/`M-w`; `C-k` copy to
    end of line and cancel (defer, not in v1 tables); `C-g` clear-selection;
    `q`/`Escape` cancel; `C-s`/`C-r` search fwd/back; `n`/`N` again/reverse;
    `R` rectangle-toggle; `o` other-end.
  - vi (`copy-mode-vi`): `hjkl`+arrows; `w`/`b`/`e` words; `0`/`$`/`^`;
    `g`/`G`; `H`/`M`/`L`; `K`/`J` scroll; `C-u`/`C-d` half pages;
    `C-b`/`C-f`/PPage/NPage pages; `Space` begin-selection; `v` rectangle-
    toggle; `Enter` copy-selection-and-cancel; `Escape` clear-selection;
    `q` cancel; `/` `?` search; `n`/`N`; `o` other-end.
- `mode-keys` option (exists, Choice emacs|vi, default emacs) selects the
  table at entry.
- Rendering (renderer amendments): the focused pane's `PaneView` gains
  `copy: Option<CopyView{scroll, cx, cy, sel_cells: fn?}>` — concretely the
  server precomputes and passes `sel: Option<(start_col,start_row,end_col,
  end_row, rect)>` in VIEW coordinates plus the scrolled grid view; render
  paints pane content from `view_cell(scroll,…)`, applies `mode_style`
  (options: `mode-style`, default `bg=yellow,fg=black` — ADD to options) to
  cells inside the selection, draws the position indicator `[scroll/history]`
  right-aligned on the pane's TOP row in `mode-style`, and places the
  terminal cursor at the copy cursor.
- Selection semantics: linear selection = between anchor and cursor in
  absolute line order (anchor stored with its own scroll offset so scrolling
  keeps it stable); rectangle = the col/row bounding box. `copy` extracts
  text from the grid view rows (trailing blank run per line trimmed, `\r\n`?
  NO — `\n` separators, tmux-style) into a new paste buffer.
- Exit: `q` (+ emacs Escape) / copy action / `copy-mode -e` scroll-past-
  bottom / pane death / window switch (cancel on any window/session switch
  by that client).

## 3. Paste buffers

- Server-global `Buffers`: named entries, automatic names `buffer%u` from a
  never-resetting counter; ordering by insertion; `buffer-limit` (ADD server
  option, Number, default 50) evicts oldest AUTOMATIC buffers only; manual
  names (`set-buffer -b`) exempt.
- Commands: `paste-buffer|pasteb [-p] [-b name] [-t target-pane]` (default
  newest; `-p` bracketed paste passthrough — v1: plain write, `-p` accepted
  and ignored, documented), `list-buffers|lsb` (lines
  `<name>: <size> bytes: "<sample>"`, sample = first 50 chars? tmux 200 —
  use 200, control chars escaped as `\ooo`? v1: replaced with `?`,
  documented), `delete-buffer|deleteb [-b name]` (default newest),
  `set-buffer|setb [-b name] data`. Bindings: `]` paste-buffer -p, `#`
  list-buffers (output → transient message first line? NO: multi-line
  output → CliDone for CLI; from a binding show first line as message —
  tmux shows a pager; documented simplification), `-` delete-buffer.
  `=` choose-buffer DEFERRED (documented).

## 4. Mouse

- `keys`: `MouseEvent { kind: MouseKind, mods {ctrl,meta,shift}, x: u16,
  y: u16 }` (0-based cells), `MouseKind::{Down(u8), Up(u8), Drag(u8),
  WheelUp, WheelDown}` (button 1/2/3). Decoder: SGR `CSI < Cb ; Cx ; Cy M|m`
  → `DecodedInput::Mouse(MouseEvent)`; `DecodedKey` generalizes to
  `DecodedInput::{Key(DecodedKey), Mouse{event, raw}}` (contract change) or
  a parallel vec — pick minimal churn, contract it.
- `input`: `KeyInputEvent::Mouse { event: MouseEvent, raw: Vec<u8> }` (never
  prefix-consumed, never repeat-gated; capture mode still swallows raw).
- Enable/disable: when `options.mouse()` (getter ADD) is on, the server
  appends `\x1b[?1000h\x1b[?1002h\x1b[?1006h` to the next composed output
  per client (and `l` variants when turned off / on client Exit — the
  client ALSO unconditionally writes the `l` sequences during terminal
  restore, host.rs amendment, so a crashed server can't leave mouse on).
- Server mouse routing (mouse on; mouse events with mouse off are dropped):
  hit-test against the client's last layout rects + status row:
  - Down1 on pane → focus that pane (+ forward SGR-re-encoded event to the
    pane app? v1: NOT forwarded, documented divergence).
  - Drag1 starting on a border → live resize (translate drag delta into
    layout ratio updates for the split whose border was grabbed).
  - Down1 on status window tab → select that window; wheel on status →
    next/previous window.
  - WheelUp on pane → enter copy mode `-e` scrolled 5; WheelDown → forward?
    v1: ignore when not in copy mode. In copy mode: wheel = scroll 5 lines;
    Drag1 = selection (begin at press cell, extend while dragging); release
    (Up1) = copy-selection-and-cancel; DoubleClick1 = select word + copy;
    TripleClick1 = select line + copy (double/triple detected server-side,
    500ms window, same cell).
  - Alt-screen pane + wheel → synthesize 3× Up/Down arrow key writes to the
    pane (tmux translation behavior).
- Mouse "bindings" are HARDCODED v1 (no MouseDown1Pane binding names;
  documented deviation — the bindings table stays keyboard-only).

## 5. Layout presets + swap/rotate

- `layout` additions (contract): `Layout::apply_preset(preset, panes:
  &[PaneId], area: Rect, main_width: u16, main_height: u16)` rebuilding
  `root` (binary encodings): even-horizontal = left-right chain w/ computed
  ratios for equal spread; even-vertical mirror; main-horizontal = TB split
  (main = full-width first pane at `main-pane-height`, clamped ≥MIN &
  leaving others ≥MIN) over an even LR row; main-vertical mirror w/
  `main-pane-width`; tiled = rows-first grid (`rows=cols=1; while r*c<n {
  r+=1; if r*c<n { c+=1 } }`), row-major, last short row spans. Focus
  preserved; zoom cleared. Panes order = current `panes()` order.
  `Layout::swap_panes(a, b)` (relabel two leaves; focus follows the pane
  object per tmux). `Layout::rotate(forward: bool)` = permute leaf ids;
  focus moves to keep the same SCREEN CELL focused (tmux rotate-window).
- Options ADD: `main-pane-width` (Number, 80), `main-pane-height` (Number,
  24).
- Commands: `select-layout|selectl <name>` (five classic names),
  `next-layout|nextl` (cycle even-h → even-v → main-h → main-v → tiled,
  per-window last-layout index stored on `Window`), `swap-pane|swapp
  [-U|-D]`, `rotate-window|rotatew [-D]`. Bindings: `Space` next-layout,
  `M-1`..`M-5` select-layout, `{`/`}` swap-pane -U/-D, `C-o`/`M-o`
  rotate-window.

## 6. Window ops

- `break-pane|breakp [-d] [-n name]`: focused pane out of a ≥2-pane window
  into a new window (next free index ≥ base), focus follows unless `-d`;
  `-n` sets name. Binding `!`.
- `move-window|movew [-k] [-t index]`: re-index current window; occupied →
  `index in use: <n>` unless `-k` (kill occupant). Binding `.` →
  command-prompt (label `(move-window) `) — implement as a new PromptKind
  executing `move-window -t <input>`.
- `find-window|findw <pattern>`: search window names + visible pane content
  (case-insensitive substring; v1 no regex); jump to first match; none →
  transient `no windows matching: <p>`. Binding `f` → prompt (label
  `(find-window) `).
- `'` binding → prompt (label `index`) → `select-window -t :<input>`.
- `D` choose-client: DEFERRED (documented).

## 7. Overlays: choose-tree + display-panes

- Renderer: `Scene.overlay: Option<Overlay>` where `Overlay::List(ListOverlay{
  title: String, rows: Vec<(String, bool /*selected*/)>, top: usize})` and
  `Overlay::PaneDigits(Vec<(Rect, u32, bool /*active*/)>)`. List paints a
  full-area panel (clears client area, one row per line, selected row in
  `mode-style` reverse); PaneDigits paints 5×5 block digits (space cells,
  bg = `display-panes-active-colour` fg-value for active / `display-panes-
  colour` otherwise — ADD both options, Colour kind? store as Str style-
  colors; defaults red/blue) centered per pane, small-number fallback when
  pane < 6*digits x 5.
- choose-tree (`w` = windows view, `s` = sessions view; command
  `choose-tree [-s|-w]`): `ClientMode::ChooseTree{rows: Vec<TreeRow>,
  sel: usize}` built flat: session lines `<name>: N windows (attached)`,
  window lines indented `  <idx>: <name><flags>`. Keys (hardcoded, capture
  routing like prompts): Up/Down/`k`/`j` move, Enter = switch client to
  that session/window, `q`/Escape/`C-c` cancel, `x` kill selected (reuses
  ConfirmCmd). No preview, no tagging (documented).
- display-panes (`q`; command `display-panes [-d ms]`): overlay for
  `display-panes-time` ms (ADD option, Number, 1000) or until keypress;
  digit key 0-9 → select that pane index; other key dismisses (and is NOT
  reprocessed — documented simplification).

## 8. escape-time

- Option exists (Number, default 500 — KEEP 500, the classic default;
  document master's 10ms change). Wiring: `KeyMachine` gains
  `flush_pending(now)` semantics — server: when a `Stdin` batch leaves the
  decoder with a pending buffer that starts with a lone ESC, record
  `(client, Instant)`; on `Tick` (50ms), if age ≥ escape-time, call
  `key_machine.flush_now()` (new method → decoder.flush() → events
  dispatched through the normal path). Timer granularity 50ms documented.

## 9. automatic-rename

- Server polls `grid.take_title_changed()` after each `Output` feed; title →
  `PaneRuntime.title: String` (exposed to formats as `#T`/`#{pane_title}`
  — ADD to expand_format; status-right default UNCHANGED).
- `automatic-rename` (exists, Flag, default on) + per-window `auto_rename:
  bool` (model): manual `rename-window` sets it false for that window
  (tmux precedence). When on and the ACTIVE pane's title changes, window
  name := first token of title (strip path → basename, cut at first space,
  cap 20 chars, control-stripped; empty → keep). Throttle: at most one
  rename per window per 500ms (tmux NAME_INTERVAL). `allow-rename`
  (ESC k) DEFERRED. Documented divergence: name derives from the console
  title (ConPTY surfaces SetConsoleTitle as OSC 0), not the foreground
  process.

## Options added in SP4

`mode-style` (Style, `bg=yellow,fg=black`) · `buffer-limit` (Number, 50) ·
`main-pane-width` (80) / `main-pane-height` (24) · `display-panes-time`
(1000) · `display-panes-colour` (`blue`) / `display-panes-active-colour`
(`red`). Getters added for previously-inert: `mouse`, `history-limit`,
`escape-time`, `automatic-rename`, `mode-keys`.

## Documented deferrals (ticket in follow-ups.md at closeout)

Scrollback reflow on resize · choose-buffer UI (`=`) · `D` choose-client ·
choose-tree preview/tag/filter/sort · right-click menus · mouse event
forwarding to pane apps (incl. re-encoding) · `allow-rename`/ESC k ·
bracketed-paste `-p` passthrough · regex find-window · copy-mode
`copy-pipe`/OSC 52 clipboard · emacs `C-k`/`M-m` niche bindings ·
mouse binding NAMES (MouseDown1Pane etc.) in bind-key.
