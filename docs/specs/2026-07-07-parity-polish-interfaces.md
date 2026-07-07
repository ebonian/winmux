# Sub-project 4 — Parity polish — Locked Interface Contract

**Status:** Locked, extended task-by-task. Every implementation task MUST
conform to these types and signatures exactly. If a signature must change
during implementation, the change must be applied consistently to every
consumer named here (same rule as the MVP, SP2, and SP3 contracts).

**Parent spec:** [`2026-07-07-parity-polish-design.md`](2026-07-07-parity-polish-design.md)

## `grid-v2` — scrollback, real alternate screen, OSC titles (Task 1)

**Amends:** the MVP contract's `## grid` section
([`2026-07-06-mvp-interfaces.md`](2026-07-06-mvp-interfaces.md)) — see that
section's superseded-note. `Color`/`Style`/`Cell` are UNCHANGED.

```rust
pub struct Grid { /* private; owns a vte::Parser + emulator state */ }

impl Grid {
    /// Create a grid. Dimensions are clamped to a 1x1 minimum: a grid is
    /// never zero-sized. `history_limit` caps the scrollback line count;
    /// 0 disables scrollback entirely (nothing is ever captured).
    pub fn new(cols: u16, rows: u16, history_limit: u32) -> Self;
    /// Feed raw VT bytes from the pane's ConPTY output.
    pub fn feed(&mut self, bytes: &[u8]);
    /// Content is clipped (shrink) or padded with default cells (grow);
    /// cursor is clamped into range. While in alt-screen mode the saved
    /// primary buffer is ALSO clipped/padded in lockstep, so a subsequent
    /// leave-alt restores a primary screen consistent with the new size.
    pub fn resize(&mut self, cols: u16, rows: u16);
    pub fn cols(&self) -> u16;
    pub fn rows(&self) -> u16;
    pub fn cell(&self, col: u16, row: u16) -> Cell;   // panics out of range; live screen only
    pub fn cursor(&self) -> (u16, u16);               // (col, row)
    pub fn cursor_visible(&self) -> bool;             // DECTCEM state, default true

    /// Number of scrollback lines currently captured (<= the `history_limit`
    /// passed to `new`).
    pub fn history_len(&self) -> u32;
    /// Task 3 review-fix addition: monotonic count of lines EVER captured
    /// into scrollback — incremented on every actual capture, NEVER
    /// decremented by eviction (unlike `history_len`). The difference
    /// between two readings is exactly how many view rows the pane's
    /// content has shifted up between them (each capture shifts the view by
    /// one; chunked eviction shifts nothing) — the stable
    /// "lines-ever-captured" coordinate system copy-mode selection anchors
    /// are pinned to (see the `## copy-mode` Task 3 selection-math
    /// amendment). Stays 0 when `history_limit == 0` (nothing is ever
    /// captured).
    pub fn history_total(&self) -> u64;
    /// Look up a cell in view coordinates: `scroll_back` lines scrolled up
    /// from the live bottom (0 = live screen), clamped to `history_len()`.
    /// Out-of-range `row` (>= rows) or `col` (>= cols — checked against the
    /// CURRENT dimensions for history and live rows alike) returns a blank
    /// default-style cell; a history line's captured width may differ from
    /// the current `cols` (no reflow — see below), so a wider captured line
    /// is clipped to the current width and columns past a narrower captured
    /// line's width also read as blank.
    pub fn view_cell(&self, scroll_back: u32, col: u16, row: u16) -> Cell;
    /// Convenience: collect a whole view row into a `String` (e.g. for
    /// copy-mode search).
    pub fn view_row_text(&self, scroll_back: u32, row: u16) -> String;
    /// The pane's title as last captured via OSC 0/2, if any has ever been set.
    pub fn title(&self) -> Option<&str>;
    /// Edge-triggered: true the first time this is called after the title
    /// has changed, then false until it changes again. Intended to be
    /// polled by the server after each `feed`.
    pub fn take_title_changed(&mut self) -> bool;
}
```

**Scrollback capture rules:**
- Captured only from `scroll_up` (LF at the scroll-region bottom via
  `line_feed`, and `CSI S`), and only when the scroll region is the FULL
  screen (`scroll_top == 0 && scroll_bottom == rows - 1`, i.e. no partial
  `DECSTBM` region is in effect) AND the grid is NOT currently showing the
  alternate screen. Scrolling in ANY partial region — top-anchored
  (`bottom < rows - 1`) or interior (`top > 0`) — and all scrolling while
  in alt-screen mode, is never captured (tmux only captures full-screen
  scrolls into history).
- Each captured line is a `Vec<Cell>` exactly `cols` cells wide AT CAPTURE
  TIME. `view_cell`/`view_row_text` clip (extra columns read as blank) or
  pad (missing columns read as blank) a captured line lazily on read if the
  grid's width has since changed — **no reflow** (documented divergence from
  tmux ≥1.9, which does reflow; ticketed in `docs/follow-ups.md` at SP4
  closeout).
- Eviction: once a push brings the scrollback length to `>= history_limit`,
  the oldest `max(1, history_limit / 10)` lines are dropped in one chunk
  (mirrors tmux's `grid_collect_history` batch-eviction, not evict-one-at-
  hit-limit).
- `history_limit == 0` disables scrollback outright: nothing is ever pushed,
  `history_len()` stays 0, and any `scroll_back` clamps to 0 (always the
  live screen). Degenerate `history_limit == 1`: every push immediately
  hits the limit and evicts the line just pushed, so `history_len()` also
  stays 0 — effectively disabled (documented, not special-cased).

**View-coordinate mapping:** the combined buffer is history (oldest first)
followed by the live screen (`history_len` total history lines + `rows` live
lines). At a given `scroll_back` (clamped to `history_len`), view row `r`
(`0 <= r < rows`) reads combined index `history_len - scroll_back + r`: an
index `< history_len` reads `history[index]`; otherwise it reads live grid
row `index - history_len`.

**Real alternate screen (`CSI ?1049 h/l`):**
- Enter (`h`): the FIRST time (a redundant `?1049h` while already in alt
  mode does not re-save, so alt-screen content already drawn can't clobber
  the saved primary) saves the primary screen's cells AND the cursor state
  in DECSC/DECRC scope — position (col, row), SGR pen (`style`), and the
  autowrap flag — into an internal `saved_primary` (xterm documents 1049 as
  saving/restoring the cursor "as in DECSC"). Every enter (first or
  redundant) then clears the now-active alt buffer to blanks and homes the
  cursor — this is the MVP's original enter behavior, preserved exactly.
  `wrap_pending` is reset.
- Leave (`l`): if currently in alt mode, restores `cells`, the cursor
  position (clamped into the current, possibly-resized, dimensions), the
  SGR pen, and the autowrap flag from `saved_primary` EXACTLY — no
  clearing, and no pen-state leak from the alt-screen app into the primary
  screen. A spurious `?1049l` while not in alt mode is a no-op.
  `wrap_pending` is reset.
- The alt buffer accrues NO scrollback while active (see capture rules
  above). `1047`/`1048` are not separately implemented (documented; `1049`
  is the only alt-screen sequence winmux recognizes, matching what
  PowerShell/typical CLI tools emit).
- `resize` while in alt mode also clips/pads the saved primary buffer (and
  clamps its saved cursor) in lockstep with the active alt buffer, so a
  later leave restores a primary screen consistent with the grid's current
  dimensions.

**OSC title capture (`osc_dispatch`):**
- OSC `0` (icon + title) and OSC `2` (title) both set the title: OSC
  parameters 1..N re-joined with `;` (vte splits the OSC buffer on EVERY
  `;`, so a title containing semicolons spans multiple params — tmux and
  vte's own `ansi.rs` reference consumer both reconstruct the full title),
  UTF-8-decoded (lossy replacement of invalid sequences), with control
  characters stripped, capped at 256 `char`s. Either BEL or `ESC \` (ST) as
  the terminator produces identical results — `vte` already normalizes both
  into one `osc_dispatch` call, so no terminator-specific code is needed.
- OSC `1` (icon-only) and any other OSC command (or a malformed OSC with
  fewer than 2 parameters) leaves the title untouched and does not set the
  changed flag.
- `take_title_changed()` is edge-triggered and consumer-cleared: intended
  call site is the server polling once per pane after each `Output` feed
  (wired in a later SP4 task — automatic-rename, §9 of the design spec).

## Callers updated (Task 1)

- `src/server.rs`: `spawn_pane` gained a `history_limit: u32` parameter,
  threaded through from `self.options.history_limit()` at every call site
  (new-session auto/named attach paths in `src/server.rs`, plus split-pane,
  new-window, and CLI new-session paths in `src/server/dispatch.rs`).
- `src/options.rs`: added `Options::history_limit(&self) -> u32` (reads the
  pre-existing inert `history-limit` `Number` option, default 2000 — same
  getter pattern as `base_index`/`renumber_windows`). tmux semantics: read
  once at pane spawn; a later `set -g history-limit` affects only
  subsequently spawned panes, not already-running ones.
- `src/render.rs` (test helper `grid_with`), `tests/server_proto.rs`,
  `tests/e2e.rs`, `tests/e2e_sessions.rs`, `tests/e2e_config.rs`: every
  `Grid::new(cols, rows)` call site updated to `Grid::new(cols, rows, 0)`
  (no scrollback needed — these tests decode client/server output, not
  scrollback UI, which is a later SP4 task).

## `copy-mode` — scrollback navigation core (Task 2) + selection & paste buffers (Task 3)

Implements the design spec's `## 2. Copy mode` movement/scroll/cancel
subset (Task 2) PLUS selection (`copy-begin-selection`,
`copy-rectangle-toggle`, `copy-other-end`, `copy-clear-selection`,
`copy-selection-and-cancel`) and tmux paste buffers (Task 3, sub-project 4,
`## buffers` section below). Search (`copy-search-*`) remains OUT of scope
(Task 4) — no `ParsedCmd`/`CopyAction` variants or bindings exist for it yet.

### `input` amendment

```rust
pub enum WhichTable {
    Root,
    Prefix,
    CopyMode,    // NEW: emacs mode-keys copy table
    CopyModeVi,  // NEW: vi mode-keys copy table
}
```

`KeyMachine` itself is UNCHANGED — it still only ever produces `Root`/`Prefix`
table events (it knows nothing of client modes). The two new variants are
produced/consumed exclusively by the server (`src/server.rs`), which
substitutes them in per the rule below.

### `bindings` amendment

`Bindings` gains two more tables (`copy_mode`, `copy_mode_vi`), the same
`HashMap<Key, Binding>` shape as `root`/`prefix`. `table_name`/`table_mut`/
`table_ref`/`list()` all grow arms; `bind-key`/`unbind-key`'s `-T` validation
(`cmd::resolve`) accepts `"copy-mode"`/`"copy-mode-vi"` in addition to
`"root"`/`"prefix"`.

`Bindings::default()` additionally binds, in the PREFIX table:
`[` → `copy-mode`, `PPage` → `copy-mode -u` (both were deliberately left
unbound through sub-project 3).

Default `copy-mode` (emacs) table — movement/scroll/cancel subset only:

| Key(s) | Command |
|---|---|
| `Left`/`C-b`, `Right`/`C-f`, `Up`/`C-p`, `Down`/`C-n` | `copy-cursor-left`, `copy-cursor-right`, `copy-cursor-up`, `copy-cursor-down` |
| `C-a`/`Home` | `copy-start-of-line` |
| `C-e`/`End` | `copy-end-of-line` |
| `M-<` | `copy-history-top` |
| `M->` | `copy-history-bottom` |
| `M-v`/`C-v`/`PPage` | `copy-page-up` |
| `NPage` | `copy-page-down` |
| `Space` (the literal space CHARACTER — see the gotcha below) | `copy-page-down` |
| `q`/`Escape` | `copy-cancel` |

`H`/`M`/`L` (top/middle/bottom-line) are NOT bound in the emacs table — the
design spec flags them "unverified" for emacs; bound in vi only (documented
deviation). Search bindings (`C-s`/`C-r`, `n`/`N`) are Task 4 and absent.

**Task 3 amendment** — selection, added to the emacs table:

| Key(s) | Command |
|---|---|
| `C-Space` | `copy-begin-selection` |
| `C-w`, `M-w` | `copy-selection-and-cancel` |
| `R` | `copy-rectangle-toggle` |
| `C-g` | `copy-clear-selection` |
| `o` | `copy-other-end` |

Default `copy-mode-vi` table — movement/scroll/cancel subset only:

| Key(s) | Command |
|---|---|
| `h`/`l`/`k`/`j`, `Left`/`Right`/`Up`/`Down` | `copy-cursor-{left,right,up,down}` |
| `w`/`b`/`e` | `copy-next-word`/`copy-previous-word`/`copy-next-word-end` |
| `0`/`$`/`^` | `copy-start-of-line`/`copy-end-of-line`/`copy-start-of-line` (`^` simplified to start-of-line, documented) |
| `g`/`G` | `copy-history-top`/`copy-history-bottom` |
| `H`/`M`/`L` | `copy-top-line`/`copy-middle-line`/`copy-bottom-line` |
| `K`/`J` | `copy-scroll-up`/`copy-scroll-down` |
| `C-u`/`C-d` | `copy-halfpage-up`/`copy-halfpage-down` |
| `C-b`/`PPage` | `copy-page-up` |
| `C-f`/`NPage` | `copy-page-down` |
| `q` | `copy-cancel` |

`/`/`?`/`n`/`N` (search) are Task 4 and absent.

**Task 3 amendment** — selection, added to the vi table (`Escape`, left
UNBOUND through Task 2, is now bound):

| Key(s) | Command |
|---|---|
| `Space` (the literal space CHARACTER — see the gotcha below) | `copy-begin-selection` |
| `v` | `copy-rectangle-toggle` |
| `Enter` | `copy-selection-and-cancel` |
| `Escape` | `copy-clear-selection` |
| `o` | `copy-other-end` |

**Gotcha (discovered implementing this task, applies to both tables):** the
live decoder (`keys::classify_single_byte`) NEVER produces
`Key{code: KeyCode::Space}` for an actual spacebar press — byte `0x20`
decodes to `Key{code: KeyCode::Char(' ')}`; the `Space` code variant is only
ever produced for `Ctrl-Space` (byte `0x00`, explicitly special-cased) and
otherwise exists purely for `parse_key("Space")`/`send-keys Space` notation.
BOTH tables' `Space` defaults are therefore registered under `Char(' ')`,
NOT `named("Space")`, so a real keypress actually reaches them: the vi
table's `Space → copy-begin-selection` was written that way from the start,
and Task 2's emacs `Space → copy-page-down` (originally registered under
`named("Space")` and thus unreachable) was REBOUND under `Char(' ')` by the
Task 3 review fix. The deeper decoder-level `Char(' ')`/`Space`
normalization (which would also cover USER `bind ... Space ...` lines in
these tables) remains `docs/follow-ups.md` item 34.

### `cmd` amendment

`ParsedCmd` gains:

```rust
CopyMode { page_up: bool, mouse: bool },  // -u / -e
CopyCmd(CopyAction),
```

```rust
pub enum CopyAction {
    CursorLeft, CursorRight, CursorUp, CursorDown,
    StartOfLine, EndOfLine,
    HistoryTop, HistoryBottom,
    TopLine, MiddleLine, BottomLine,
    ScrollUp, ScrollDown,
    HalfpageUp, HalfpageDown,
    PageUp, PageDown,
    NextWord, PreviousWord, NextWordEnd,
    Cancel,
    // Task 3 additions (selection):
    BeginSelection, RectangleToggle, OtherEnd, ClearSelection, SelectionAndCancel,
}
```

`copy-mode [-u] [-e]` is a PUBLIC command (in `canonical()`/`usage()`
normally). Each `CopyAction` also has its own canonical command name
(`copy-cursor-left`, `copy-cancel`, ...) — INTERNAL (bindable/resolvable, but
not part of the discoverable "did you mean" surface; `usage()` returns a
generic `"usage: copy-<action> (no arguments)"` for all of them) — taking no
arguments. The Task 3 additions' canonical names are `copy-begin-selection`,
`copy-rectangle-toggle`, `copy-other-end`, `copy-clear-selection`,
`copy-selection-and-cancel`.

`send-keys` gains `-X <name>`: when present, `resolve` maps `<name>` (tmux's
copy-mode command spelling, e.g. `cancel`, `cursor-left`, `history-top`) via
a fixed table directly to `ParsedCmd::CopyCmd` (bypassing `SendKeys`
entirely) — `Err("unknown -X command: <name>")` for an unrecognized name.
Whether the acting client is actually in copy mode is a DISPATCH-time
concern, not `resolve`'s: `exec_copy_action` returns `Err("not in a
mode")` when it isn't (tmux's own wording), covering `send-keys -X` used
outside copy mode identically to a directly-bound `copy-*` command.

**Task 3 amendment** — the `-X` name table grows: `begin-selection`,
`rectangle-toggle`, `other-end`, `clear-selection` map onto their
same-named `CopyAction`s (tmux drops the `copy-` prefix for `-X`, as with
every Task 2 name); `copy-selection-and-cancel` is the ONE exception that
KEEPS the `copy-` prefix in its `-X` spelling too (verified against tmux
master's `window-copy.c` command table) — `resolve` maps the literal string
`"copy-selection-and-cancel"` (not `"selection-and-cancel"`) to
`CopyAction::SelectionAndCancel`.

`bind-key`/`unbind-key -T` accepts `"copy-mode"`/`"copy-mode-vi"` (see the
`bindings` amendment above).

### `options` amendment

Adds `mode-style` (`Style`, default `bg=yellow,fg=black`) and two getters:

```rust
pub fn mode_style(&self) -> &PartialStyle;
pub fn mode_keys_vi(&self) -> bool; // true iff mode-keys == "vi"
```

**Task 3 amendment** — adds `buffer-limit` (`Number`, default `50`; see the
`## buffers` section) and its getter:

```rust
pub fn buffer_limit(&self) -> u32;
```

### `server`/`server::dispatch` amendment

`ClientMode` gains `Copy(CopyState)`:

```rust
struct CopyState {
    pane: PaneId,
    scroll: u32,   // tmux `oy`; 0 = live screen
    cx: u16,       // view-coordinate cursor column
    cy: u16,       // view-coordinate cursor row
    scroll_exit: bool, // placeholder for the mouse task; unused in Task 2
    sel: Option<SelState>, // Task 3: the active selection, if any
}

/// Task 3 addition (amended by the Task 3 review fix). The anchor is
/// pinned to CONTENT, not to the view: its view position at capture time
/// (`anchor_scroll`/`anchor_x`/`anchor_y`) is stored together with the
/// grid's monotonic `Grid::history_total()` reading (`anchor_total`), and
/// every use site recomputes the anchor's CURRENT view position via
/// `anchor_key_now` (below) — never reusing the capture-time position
/// directly.
struct SelState {
    anchor_scroll: u32,
    anchor_x: u16,
    anchor_y: u16,
    anchor_total: u64, // Grid::history_total() at anchor time
    rect: bool, // rectangle (column bounding-box) vs. linear (reading-order)
}
```

winmux models copy mode PER-CLIENT bound to the pane focused at entry
(`pane`), not per-pane like real tmux (documented divergence, design spec
`## 2. Copy mode`): two clients can independently be in copy mode, even on
the same pane, with separate `scroll`/`cx`/`cy`.

**Entry**: `copy-mode [-u] [-e]`, dispatched only `execute_for_client`
(`execute_headless` — CLI/config with no client — errors `"no current
client"`). Binds to `session.current_window().layout.focused()` at the
moment of dispatch. `cx`/`cy` seed from the pane's LIVE cursor
(`Grid::cursor()`); `-u` additionally seeds `scroll = min(pane_rows,
history_len)` and `cy = 0` (page-up-on-entry, the `PPage` binding).

**Key routing** (`handle_stdin`): `KeyMachine` is unaware of client modes, so
the server performs TWO distinct substitutions while `client.mode` is
`Copy(_)`:
1. `KeyInputEvent::Key { table: Root, .. }` → the table is swapped to
   `CopyMode`/`CopyModeVi` (per `options.mode_keys_vi()`) BEFORE
   `bindings.lookup`. A `Prefix`-table event is left untouched (prefix
   bindings, e.g. `C-b c`, still fire from copy mode — tmux behavior).
2. **`KeyInputEvent::Forward(data)`** (CRITICAL, easy to miss): `KeyMachine`
   coalesces every PLAIN unmodified key (bare letters/digits, `Space`,
   `Enter`, `Tab`, `BSpace` — covering most copy-mode bindings, e.g. `q`,
   `h`/`j`/`k`/`l`, `w`/`b`/`e`, `g`/`G`, `H`/`M`/`L`, `K`/`J`) into ONE
   `Forward` blob for throughput (see the `## input-v2` contract's
   documented deviation) — this NEVER reaches the `Key{table,..}` arm at
   all. While in copy mode, `handle_stdin`'s `Forward` arm re-decodes the
   blob with a fresh `crate::keys::KeyDecoder` (reproducing exactly the keys
   that were coalesced, since the blob is always complete and
   self-contained) and resolves each decoded key against the copy table
   individually, instead of writing the bytes to the pane's pty.

Unbound key in EITHER copy table (via either routing path): swallowed (never
forwarded to the pane).

**Execution** (`server::dispatch`): `exec_copy_mode` (entry, above) and
`exec_copy_action` (movement/scroll/cancel, mutating `CopyState` in place).
Word motion (`NextWord`/`PreviousWord`/`NextWordEnd`) is a v1 simplification:
whitespace-delimited words via `Grid::view_row_text(scroll, cy)`, no line
wrapping (clamps at the current view row's edges) — documented, Task 3/4
territory for anything richer. All movement reads pane dimensions/history via
`self.panes.get(&cs.pane)`; a pane that's disappeared resets `client.mode` to
`Normal` defensively (belt-and-braces — `cancel_stale_copy_modes` is the
primary mechanism, see below).

**Stale invalidation**: `cancel_stale_copy_modes` (parallels
`cancel_stale_confirms`) resets any client's `Copy(cs)` to `Normal` when
EITHER `cs.pane` no longer exists in any live window, OR the client's
attached session's CURRENT window no longer contains `cs.pane` — the latter
is how "cancel on any window/session switch by that client" is implemented:
every window/session-changing dispatch (`select-window`, `next-window`,
`switch-client`, ...) changes `session.current`, so re-checking pane
membership in `session.current_window()` after every dispatch batch catches
all of them uniformly without hooking each mutating command individually.
Called from the same two sites as `cancel_stale_confirms`: `handle_exited`
(natural pane exit) and once per `Stdin` frame at the tail of `handle_stdin`
(after the client is back in `self.clients`).

**Task 3 amendment — selection commands** (`exec_copy_action`, same
`Err("not in a mode")`-outside-copy-mode rule as every other `CopyAction`):

- `BeginSelection`: `cs.sel = Some(SelState{anchor_scroll: cs.scroll,
  anchor_x: cs.cx, anchor_y: cs.cy, rect: false})` — always starts a NEW
  linear selection anchored at the current cursor (restarts one if already
  selecting).
- `RectangleToggle`: `sel.rect = !sel.rect` if `cs.sel.is_some()`, else a
  no-op (v1 simplification, documented deviation from tmux's "sticks for the
  next selection too").
- `OtherEnd`: swaps `(cs.scroll, cs.cx, cs.cy)` with the SelState's
  `(anchor_scroll, anchor_x, anchor_y)` if a selection is active, else a
  no-op.
- `ClearSelection`: `cs.sel = None`. Copy mode itself stays active (does
  NOT reset `client.mode`).
- `SelectionAndCancel`: if `cs.sel` is `Some`, extracts its text (below) via
  `extract_selection_text`; if the extracted text is non-empty, inserts it
  as a new automatic buffer (`self.buffers.add_automatic(text,
  self.options.buffer_limit())`); either way sets `client.mode =
  ClientMode::Normal` (exits copy mode) — matching the `Cancel` action's
  established "read `cs`'s Copy fields into locals before reassigning
  `client.mode`" NLL pattern.

**Task 3 amendment — position/ordering key** (`sel_key`, `anchor_key_now`,
`key_to_view`, `compute_sel_view`, private `fn`s in `src/server.rs`, used by
both `server::dispatch`'s extraction and `render_one`'s precompute below).
REWRITTEN by the Task 3 review fix — the original text claimed
`sel_key(scroll, row) = row - scroll` was history-drift-independent
("`history_len` cancels out"), which is FALSE: keys are only comparable
against the SAME grid state, and every line captured into scrollback after
a key is taken shifts that content's current key down by one. The corrected
model:

- `sel_key(scroll, row) = row as i64 - scroll as i64` — a view position's
  ordering/delta key AT ONE INSTANT. Two keys measured against the same
  grid state compare the way their absolute grid-line indices would, and
  `key + scroll` converts a current key to a view row under any scroll
  offset. NOT stable across grid mutations.
- `anchor_key_now(sel, history_len, history_total) -> i64` — the STORED
  anchor's key in the grid's CURRENT frame: `sel_key(anchor_scroll,
  anchor_y) - (history_total - anchor_total)`, then clamped to
  `>= -(history_len)` (the oldest retained history line). The
  `history_total` delta is exact even across chunked eviction — eviction
  lowers `history_len` but never moves a surviving line's view position,
  which is precisely why a plain `history_len` delta would under-correct
  and why `Grid::history_total()` (grid-v2 amendment above) exists. If the
  anchor's line has been EVICTED, the clamp degrades the endpoint to the
  oldest content still available (no panic, no reliance on `Grid`'s
  read-time clamping).
- **Endpoint semantics** (pragmatic resolution, per the review): the ANCHOR
  is pinned to content — new pane output mid-selection moves its
  highlight/extraction up in lockstep with the text it was placed on. The
  CURSOR endpoint stays view-relative (`CopyState::scroll`/`cx`/`cy` are
  untouched by new output; the copy cursor keeps its screen position while
  content moves underneath) and is converted to a key live at each use, so
  both endpoints are always compared in one coherent frame. `copy-other-end`
  jumps the cursor to the anchor's CURRENT (content-pinned) position,
  keeping the current scroll when that position is visible under it and
  scrolling minimally to reveal it otherwise.
- `key_to_view(key, rows)` — the `(scroll_back, row)` pair to pass to
  `Grid::view_row_text`/`view_cell` that reproduces a CURRENT key in view
  coordinates (`Grid` clamps `scroll_back` to its actual `history_len`
  internally, so an over-large value is harmless).

**Task 3 amendment — text extraction** (`extract_selection_text` in
`src/server/dispatch.rs`, a free `fn(grid: &Grid, sel: &SelState, cx: u16,
cy: u16, scroll: u32) -> String`): the stored anchor is converted to its
CURRENT view key via `anchor_key_now(sel, grid.history_len(),
grid.history_total())` (content-pinned; Task 3 review fix) and the live
cursor via `sel_key(scroll, cy)`, so both endpoints are compared in one
coherent frame regardless of output captured mid-selection.
- Linear (`sel.rect == false`): reading order between the two endpoints
  (whichever of anchor/cursor sorts first by `(key, col)`). A single-row
  selection is a plain `[start_col..=end_col]` substring. A multi-row
  selection joins the first row's `[start_col..]` tail, every whole row
  strictly between, and the last row's `[..=end_col]` head, with `\n`
  (never `\r\n`) — each line trailing-blank-trimmed (`trim_end_matches('
  ')`) independently.
- Rectangle (`sel.rect == true`): every row from `min(anchor_key,
  cursor_key)` to `max(...)` inclusive, each sliced to
  `[min_col..=max_col]` (columns from `min`/`max` of the anchor's and
  cursor's `x`, NOT sorted by which is anchor), same per-row trimming,
  `\n`-joined.

### `render` amendment

```rust
pub struct CopyView {
    pub scroll: u32,
    pub cursor: (u16, u16),
    // Task 3: precomputed by the server in VIEW coordinates, already
    // clamped into the pane's visible rows/cols; None = no active
    // selection, or it's wholly scrolled out of the current view.
    pub sel: Option<(u16, u16, u16, u16, bool)>, // (start_col, start_row, end_col, end_row, rect)
}

pub struct PaneView<'a> {
    // ...unchanged fields...
    pub copy: Option<CopyView>,  // NEW (Task 2)
}

pub struct Scene<'a> {
    // ...unchanged fields...
    pub mode_style: Style,  // NEW (Task 2)
}
```

`Renderer::compose_back`: pass 1 (pane content copy) reads
`grid.view_cell(cv.scroll, dx, dy)` instead of `grid.cell(dx, dy)` whenever
`PaneView::copy` is `Some`. A new pass 1b paints the position indicator
`[<scroll>/<history_len>]` right-aligned on that pane's TOP row
(`rect.y`), in `scene.mode_style`, truncating from the LEFT if the indicator
is wider than the pane. `history_len` comes from `pv.grid.history_len()` —
no separate field needed.

**Task 3 amendment — pass 1a, selection highlight** (runs BEFORE pass 1b,
so the position indicator paints on top of a highlighted cell if they
overlap): for every pane with `copy.sel == Some((sc, sr, ec, er, rect))`,
every cell in the shape below gets `scene.mode_style`'s `fg`/`bg` painted
ON TOP of whatever pass 1 already wrote there — the character and every
OTHER style attribute (bold, underline, ...) from pass 1 are PRESERVED, only
`fg`/`bg` are overridden. Shape: for `rect == true`, every row `sr..=er`
highlights `sc..=ec`. For `rect == false` (linear): row `sr` (if `sr != er`)
highlights `sc..`, row `er` (if `sr != er`) highlights `..=ec`, every row
strictly between highlights the full pane width, and if `sr == er` the
single row highlights exactly `sc..=ec`. (A `compute_sel_view`-clamped
endpoint whose true row is off-screen already arrives with `sc`/`ec` widened
to 0/`cols-1`, so this same shape logic paints it correctly as a full-width
"middle" row — see `compute_sel_view`'s doc comment in `src/server.rs`.)

`server::render_one`: when `client.mode` is `Copy(cs)`, the `PaneView` whose
`id == cs.pane` gets `copy: Some(CopyView{scroll: cs.scroll, cursor: (cs.cx,
cs.cy), sel: cs.sel.as_ref().and_then(|sel| compute_sel_view(sel, cs.cx,
cs.cy, cs.scroll, rect.h, rect.w, p.grid.history_len(),
p.grid.history_total()))})` (the two trailing grid readings are the Task 3
review fix's content-pinning inputs — see the position/ordering-key
amendment above; every other pane, including a DIFFERENT client's
focused/zoomed pane, renders live as before); the terminal cursor is placed
at `cs.pane`'s rect origin + `(cs.cx, cs.cy)` (clamped into the rect)
instead of the focused pane's live cursor, visible whenever there's no
overlay message.

## `buffers` — tmux paste buffers (Task 3)

New module, `src/buffers.rs`. Pure (no I/O). Insertion-ordered storage
(oldest first, newest last); `list()`'s row order is this same order.

```rust
pub struct Buffers { /* private */ }

impl Buffers {
    pub fn new() -> Self;

    /// Insert a new AUTOMATIC buffer named `buffer<N>` from a counter that
    /// NEVER resets (a deleted `buffer3` is never reused), evicting the
    /// oldest AUTOMATIC entries first so the automatic count stays under
    /// `limit` -- eviction happens BEFORE the insert, so the just-inserted
    /// buffer always survives even when `limit` is reached exactly. Manual
    /// (`set_named`) entries are NEVER evicted, regardless of `limit`.
    /// Returns the new buffer's name.
    pub fn add_automatic(&mut self, data: String, limit: u32) -> String;

    /// Insert or overwrite a MANUAL, named buffer -- exempt from
    /// `buffer-limit` eviction. Overwriting an existing name (automatic or
    /// manual) replaces its data and marks it manual.
    pub fn set_named(&mut self, name: &str, data: String);

    pub fn get(&self, name: &str) -> Option<&str>;
    /// The most recently inserted buffer (by any of the three insert paths)
    /// -- `paste-buffer`'s default target. `None` when empty.
    pub fn newest(&self) -> Option<(&str, &str)>;
    /// `true` if a buffer with that name existed and was removed.
    pub fn delete(&mut self, name: &str) -> bool;
    /// Remove and return the name of the newest buffer. `None` when empty.
    pub fn delete_newest(&mut self) -> Option<String>;
    /// `(name, size_in_bytes, sample)` per buffer, oldest first. `sample` is
    /// the first 200 `char`s with every control character (INCLUDING `\n`
    /// -- `char::is_control()` classifies it as one) replaced by `?`.
    pub fn list(&self) -> Vec<(String, usize, String)>;
}
```

**Deviation from the task brief's sketch**: `add_automatic` takes an
explicit `limit: u32` parameter (the brief's pseudocode omitted it). Since
`buffer-limit` eviction must happen somewhere with knowledge of the current
option value, and `Buffers` itself is deliberately option-registry-agnostic
(pure, no dependency on `crate::options`), the caller (`server::dispatch`)
passes `self.options.buffer_limit()` at each call site instead of `Buffers`
reading it from a stored config. This keeps `Buffers` unit-testable with
arbitrary limits per call (see `src/buffers.rs`'s own tests) without a
mutable "current limit" field to keep in sync with `set -g buffer-limit`.

### `options` amendment

`buffer-limit` (`Number`, default `50`) + `pub fn buffer_limit(&self) ->
u32` getter — see the `## copy-mode` section's `options amendment` above.

### `cmd` amendment

`ParsedCmd` gains:

```rust
PasteBuffer { name: Option<String>, target: Option<String>, no_replace: bool },
ListBuffers,
DeleteBuffer { name: Option<String> },
SetBuffer { name: Option<String>, data: String },
```

- `paste-buffer|pasteb [-p] [-r] [-b name] [-t target-pane]`: `name: None` =
  newest buffer; `target: None` resolves via the same `resolve_pane_target`
  grammar `send-keys` uses (acting client's focused pane, or a headless
  error with no `-t`). `no_replace` is `-r`'s value: DEFAULT (`false`)
  replaces every `\n` in the buffer with `\r` before writing to the pane's
  pty (tmux's own default -- tmux's `-r` flag means "do NOT replace LF with
  CR"; verified against tmux master's `paste.c`). `-p` (bracketed-paste
  passthrough) is accepted and IGNORED -- v1 simplification, documented in
  the design spec's deferrals list.
- `list-buffers|lsb`: no arguments. Full multi-line CLI/headless text (one
  `<name>: <size> bytes: "<sample>"` line per buffer, oldest first,
  newline-terminated) via `exec_list_buffers_headless`; dispatched from a
  CLIENT (a key binding, or the `:` prompt) instead goes through
  `exec_list_buffers_client`, which shows just the FIRST buffer's line plus
  a `(N buffers)` suffix (only when there's more than one) as a transient
  status-line message -- a status message can only ever hold one line, and
  this is a documented simplification of tmux's pager.
- `delete-buffer|deleteb [-b name]`: `name: None` = delete the newest
  buffer. `Err("no buffer")` if empty; `Err("buffer not found: <name>")` for
  an unknown name.
- `set-buffer|setb [-b name] data...`: `name: None` creates a new AUTOMATIC
  buffer (`self.buffers.add_automatic`, same eviction as
  `copy-selection-and-cancel`); `Some(name)` sets/overwrites a MANUAL buffer
  (`self.buffers.set_named`, exempt from eviction). `data` is the remaining
  positional tokens joined with single spaces (same convention as
  `set-option`'s value / `display-message`'s text).

Aliases: `pasteb`, `lsb`, `deleteb`, `setb`. All four are usable from EVERY
entry point (CLI, config, `:` prompt, key bindings) via the normal
`execute_headless`/`execute_for_client` dispatch, like every other SP3+
command -- `list-buffers` is the only one whose OUTPUT shape differs
between the two paths (see above), not its availability.

### `bindings` amendment

`Bindings::default()` additionally binds, in the PREFIX table: `]` →
`paste-buffer -p`, `#` → `list-buffers`, `-` → `delete-buffer`. tmux's `=`
(choose-buffer) is DEFERRED (documented in the design spec's deferrals
list; no winmux equivalent yet).

### `server` amendment

`Server` gains a `buffers: Buffers` field (`Buffers::new()` at
construction) -- one instance, shared by every session/client (tmux itself
scopes buffers server-wide too, not per-session). No wire-protocol change:
buffers are purely a dispatch-time server concern, never sent to clients
directly (their effects are observed through `paste-buffer`'s pty write and
`list-buffers`' text/message output).
