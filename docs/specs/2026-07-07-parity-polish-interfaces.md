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
