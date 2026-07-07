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

## `copy-mode` — scrollback navigation core (Task 2)

Implements the design spec's `## 2. Copy mode` movement/scroll/cancel
subset ONLY — selection (`copy-begin-selection`, `copy-rectangle-toggle`,
`copy-selection-and-cancel`, `copy-other-end`, `copy-clear-selection`) and
search (`copy-search-*`) are explicitly OUT of scope (Tasks 3/4) and neither
their `ParsedCmd`/`CopyAction` variants nor their bindings exist yet.

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
| `Space` | `copy-page-down` |
| `q`/`Escape` | `copy-cancel` |

`H`/`M`/`L` (top/middle/bottom-line) are NOT bound in the emacs table — the
design spec flags them "unverified" for emacs; bound in vi only (documented
deviation). Selection/search bindings (`C-Space`, `C-w`/`M-w`, `C-g`,
`C-s`/`C-r`, `n`/`N`, `R`, `o`) are Tasks 3/4 and absent.

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

`Escape` is deliberately UNBOUND in vi (tmux binds it to `clear-selection`,
Task 3; until then an unbound copy-table key swallows — a documented no-op).
`Space`/`v`/`Enter`/`/`/`?`/`n`/`N`/`o` (selection/search) are Tasks 3/4 and
absent.

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
}
```

`copy-mode [-u] [-e]` is a PUBLIC command (in `canonical()`/`usage()`
normally). Each `CopyAction` also has its own canonical command name
(`copy-cursor-left`, `copy-cancel`, ...) — INTERNAL (bindable/resolvable, but
not part of the discoverable "did you mean" surface; `usage()` returns a
generic `"usage: copy-<action> (no arguments)"` for all of them) — taking no
arguments.

`send-keys` gains `-X <name>`: when present, `resolve` maps `<name>` (tmux's
copy-mode command spelling, e.g. `cancel`, `cursor-left`, `history-top`) via
a fixed table directly to `ParsedCmd::CopyCmd` (bypassing `SendKeys`
entirely) — `Err("unknown -X command: <name>")` for an unrecognized name.
Whether the acting client is actually in copy mode is a DISPATCH-time
concern, not `resolve`'s: `exec_copy_action` returns `Err("not in a
mode")` when it isn't (tmux's own wording), covering `send-keys -X` used
outside copy mode identically to a directly-bound `copy-*` command.

`bind-key`/`unbind-key -T` accepts `"copy-mode"`/`"copy-mode-vi"` (see the
`bindings` amendment above).

### `options` amendment

Adds `mode-style` (`Style`, default `bg=yellow,fg=black`) and two getters:

```rust
pub fn mode_style(&self) -> &PartialStyle;
pub fn mode_keys_vi(&self) -> bool; // true iff mode-keys == "vi"
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

### `render` amendment

```rust
pub struct CopyView { pub scroll: u32, pub cursor: (u16, u16) }

pub struct PaneView<'a> {
    // ...unchanged fields...
    pub copy: Option<CopyView>,  // NEW
}

pub struct Scene<'a> {
    // ...unchanged fields...
    pub mode_style: Style,  // NEW
}
```

`Renderer::compose_back`: pass 1 (pane content copy) reads
`grid.view_cell(cv.scroll, dx, dy)` instead of `grid.cell(dx, dy)` whenever
`PaneView::copy` is `Some`. A new pass 1b paints the position indicator
`[<scroll>/<history_len>]` right-aligned on that pane's TOP row
(`rect.y`), in `scene.mode_style`, truncating from the LEFT if the indicator
is wider than the pane. `history_len` comes from `pv.grid.history_len()` —
no separate field needed.

`server::render_one`: when `client.mode` is `Copy(cs)`, the `PaneView` whose
`id == cs.pane` gets `copy: Some(CopyView{scroll: cs.scroll, cursor: (cs.cx,
cs.cy)})` (every other pane, including a DIFFERENT client's focused/zoomed
pane, renders live as before); the terminal cursor is placed at
`cs.pane`'s rect origin + `(cs.cx, cs.cy)` (clamped into the rect) instead of
the focused pane's live cursor, visible whenever there's no overlay message.
