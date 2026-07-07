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
    /// Task 5 (mouse) addition: `true` while the pane is showing the
    /// alternate screen (`CSI ?1049h` seen more recently than a matching
    /// `?1049l`). Consumed by `server::dispatch::mouse_wheel` to decide
    /// whether a wheel event scrolls winmux's own copy-mode/scrollback
    /// (primary screen) or is translated into 3 synthesized arrow-key
    /// presses sent to the pane (alt screen — tmux's own alt-screen wheel
    /// translation, since alt-screen apps like `less`/vim have their own
    /// paging, not winmux's). See the `## mouse` section below.
    pub fn alt_screen(&self) -> bool;
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

## `copy-mode` — scrollback navigation core (Task 2) + selection & paste buffers (Task 3) + search (Task 4)

Implements the design spec's `## 2. Copy mode` movement/scroll/cancel
subset (Task 2) PLUS selection (`copy-begin-selection`,
`copy-rectangle-toggle`, `copy-other-end`, `copy-clear-selection`,
`copy-selection-and-cancel`), tmux paste buffers (Task 3, sub-project 4,
`## buffers` section below), and search (`copy-search-*`, Task 4, below).

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
deviation).

**Task 3 amendment** — selection, added to the emacs table:

| Key(s) | Command |
|---|---|
| `C-Space` | `copy-begin-selection` |
| `C-w`, `M-w` | `copy-selection-and-cancel` |
| `R` | `copy-rectangle-toggle` |
| `C-g` | `copy-clear-selection` |
| `o` | `copy-other-end` |

**Task 4 amendment** — search, added to the emacs table:

| Key(s) | Command |
|---|---|
| `C-s` | `copy-search-forward` |
| `C-r` | `copy-search-backward` |
| `n` | `copy-search-again` |
| `N` | `copy-search-reverse` |

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

**Task 3 amendment** — selection, added to the vi table (`Escape`, left
UNBOUND through Task 2, is now bound):

| Key(s) | Command |
|---|---|
| `Space` (the literal space CHARACTER — see the gotcha below) | `copy-begin-selection` |
| `v` | `copy-rectangle-toggle` |
| `Enter` | `copy-selection-and-cancel` |
| `Escape` | `copy-clear-selection` |
| `o` | `copy-other-end` |

**Task 4 amendment** — search, added to the vi table:

| Key(s) | Command |
|---|---|
| `/` | `copy-search-forward` |
| `?` | `copy-search-backward` |
| `n` | `copy-search-again` |
| `N` | `copy-search-reverse` |

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
    // Task 4 additions (search):
    SearchForward, SearchBackward, SearchAgain, SearchReverse,
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

**Task 4 amendment** — search. Canonical names `copy-search-forward`,
`copy-search-backward`, `copy-search-again`, `copy-search-reverse` (all four
follow the generic `copy-<action>`/no-arguments/`Err("not in a mode")`
pattern above — no special-casing was needed in `canonical()`/`usage()`/
`resolve()`, unlike `copy-mode` itself). The `-X` name table grows
`search-forward`, `search-backward`, `search-again`, `search-reverse`
(dropping the `copy-` prefix, the normal rule). `SearchForward`/
`SearchBackward` do NOT take an inline pattern argument (tmux's real
`search-forward <text>` does) — v1 simplification, prompt-driven only
(documented deviation, see the task report); `SearchAgain`/`SearchReverse`
take none either way (repeat the STORED pattern).

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
    search: Option<SearchState>,       // Task 4: last committed search, for n/N
    search_prompt: Option<SearchPrompt>, // Task 4: the open `/`?`/C-s`/C-r line edit, if any
}

// Task 4 additions -- see the "Task 4 amendment -- search" subsection below
// for the full rationale (in particular WHY these live inside `CopyState`
// instead of `ClientMode::Prompt`).
struct SearchState {
    pattern: String,
    backward: bool,
}
struct SearchPrompt {
    backward: bool,
    buf: String,
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

**Task 4 amendment — search** (`src/server.rs` / `src/server/dispatch.rs`).
`CopyState` gains `search: Option<SearchState>` (the last COMMITTED search —
pattern + direction — for `n`/`N` to repeat; set on every commit, even a
failed one, so a retry later can still find something) and `search_prompt:
Option<SearchPrompt>` (the in-progress `/`/`?`/`C-s`/`C-r` line edit, `Some`
only while typing).

**Why `search_prompt` lives inside `CopyState` instead of switching
`client.mode` to the existing `ClientMode::Prompt`** (the brief's suggested
starting point): `render_one` only paints a pane's SCROLLED copy view,
frozen cursor, and selection highlight when `client.mode` is literally
`ClientMode::Copy` (`let copy = match &client.mode { ClientMode::Copy(cs) if
... }`). Switching to `ClientMode::Prompt` while typing a search would drop
back to the pane's LIVE view/cursor for the duration of typing — an
observable regression from tmux, which keeps the copy-mode screen frozen
under the "Search Down:"/"Search Up:" prompt. The capture MECHANISM is
identical either way (`client.key_machine.set_capture`, the same printable-
append/BSpace/Enter-commit/Esc-C-c-C-g-cancel rules as `feed_prompt_byte`) —
only the storage location differs, so this is not "new capture machinery" in
the sense the brief was steering away from.

- `exec_copy_action` (Err("not in a mode") outside copy mode, same as every
  other `CopyAction`):
  - `SearchForward`/`SearchBackward`: `cs.search_prompt = Some(SearchPrompt{
    backward, buf: String::new()})` then `client.key_machine.set_capture(true)`.
    No message; copy mode's normal rendering continues underneath.
  - `SearchAgain`/`SearchReverse`: delegate to a free `fn repeat_search(grid:
    &Grid, cs: &mut CopyState, reverse: bool) -> Option<String>` — `None`
    (silent no-op) if `cs.search` is `None`; otherwise re-runs the stored
    pattern in the same (`SearchAgain`) or flipped (`SearchReverse`)
    direction via `do_search` below. A `Some(message)` return is surfaced as
    `ExecOutcome::Ok(message)`.
- `Server::feed_mode_byte` peeks `client.mode` for `ClientMode::Copy(cs)` with
  `cs.search_prompt.is_some()` FIRST (this borrow ends before the existing
  match, which still handles `ConfirmCmd`/`Prompt`/everything else) and
  routes to a new `feed_copy_search_byte(client, b)` method when true — same
  commit/cancel/printable/backspace byte rules as `feed_prompt_byte`. On
  commit: `client.key_machine.set_capture(false)`; re-checks `client.mode` is
  still `Copy` and `cs.pane` still exists (belt-and-braces — "handle the
  client having left copy mode or the pane having died between prompt open
  and commit" per the brief; `cancel_stale_copy_modes` below is the primary
  mechanism) — either failure cancels silently. An EMPTY committed buffer
  repeats `cs.search`'s pattern (in THIS prompt's direction, `sp.backward` —
  not necessarily the stored search's original direction, matching vim's
  `/<Enter>`/`?<Enter>`) if one exists, else is a silent no-op. Otherwise
  dispatches to `do_search`.
- `fn do_search(grid: &Grid, cs: &mut CopyState, pattern: &str, backward:
  bool) -> Option<String>` (free fn, `src/server/dispatch.rs`): records
  `cs.search = Some(SearchState{pattern, backward})` FIRST (even on a miss —
  worth remembering for a later retry), then delegates to `fn
  find_search_match(grid, pat: &[char], cur_key: i64, cur_col: usize,
  backward: bool) -> Option<(i64, u16)>` (lowercased literal single-row
  match, `sel_key`/`key_to_view` coordinates — the same "combined
  history+live buffer, one linear key" system Task 3's selection math
  uses). On a match: `cs.scroll`/`cs.cy` set via `key_to_view(key, rows)`,
  `cs.cx` set to the match column, returns `None`. On no match: returns
  `Some("no match: <pattern>")` (a documented winmux addition — tmux itself
  gives no dedicated "not found" feedback for copy-mode search) without
  moving the cursor. Never touches `cs.sel` — an active selection's anchor is
  untouched by a search the same as by any other copy-mode motion.
  - Visiting order (forward): the rest of the CURRENT row strictly after
    `cur_col`; then every OTHER row, nearest first, wrapping past the newest
    row back to the oldest; then, as a last resort, the current row's
    portion strictly before `cur_col` (completing the wrap) — this is what
    makes the search EXCLUSIVE of the current position (a repeat cannot
    re-find the cell the cursor is already on) while still covering the
    whole buffer if nothing else matches. Backward mirrors this (nearer/
    farther swapped); each row's RIGHTMOST match is preferred over its
    leftmost when scanning right-to-left. Multi-row matches and regex are
    both out of scope (v1 simplification, matching the task brief).
- `Server::cancel_stale_copy_modes` amendment: now also calls
  `client.key_machine.set_capture(false)` when resetting a stale client's
  mode to `Normal` (previously only `cancel_stale_confirms` did this) —
  covers the pane-dies-while-the-search-prompt-is-open case: without this,
  capture would stay armed after `client.mode` reverts to `Normal`, so the
  next keystroke would be silently swallowed as a stray captured byte
  instead of routing as normal input.
- `render_one`'s `message` computation: the `ClientMode::Copy(cs)` arm now
  checks `cs.search_prompt` first — `Some(sp)` renders `"Search Down: "`
  (`sp.backward == false`) or `"Search Up: "` (`sp.backward == true`) plus
  `sp.buf` in the status row (exactly like `ClientMode::Prompt`'s `{label}
  {buf}`, including hiding the pane cursor the same way, since
  `cursor_visible` is still `message.is_none()`); `None` falls through to the
  pre-Task-4 behavior (any transient `client.message`, e.g. a "no match:
  ..." result, showing underneath).

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

## `mouse` — SGR mouse decoding, routing, and mode management (Task 5)

Implements the design spec's `## 4. Mouse` section (`set -g mouse on/off`,
click-to-focus, border-drag resize, wheel-to-copy-mode with alt-screen
translation, copy-mode click/drag/double/triple-click selection, status-line
click/wheel). Mouse "bindings" are HARDCODED — there is no `MouseDown1Pane`-
style binding table; `server::dispatch::dispatch_mouse` and its helpers ARE
the routing table. Cross-module amendments already documented elsewhere and
only cross-referenced here: `keys`/`input` (`## keys`/`## input-v2` sections
of
[`2026-07-07-command-config-interfaces.md`](2026-07-07-command-config-interfaces.md)),
`grid::alt_screen()` (`## grid-v2` section above), `layout::resize_from`
(`## layout` section of
[`2026-07-06-mvp-interfaces.md`](2026-07-06-mvp-interfaces.md)), and the
`host` restore-path mouse-off amendment (`## host` section of the same file).

### `keys` amendment (full mouse decoding contract)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub x: u16, // 0-based cell column (wire is 1-based; KeyDecoder converts)
    pub y: u16, // 0-based cell row
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Down(u8),  // 1 = left, 2 = middle, 3 = right (SGR/xterm 1-based numbering)
    Up(u8),
    Drag(u8),  // motion while `u8`'s button is held (?1002h button-event tracking)
    WheelUp,
    WheelDown,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodedInput {
    Key(DecodedKey),
    Mouse { event: MouseEvent, raw: Vec<u8> },
}
```

`KeyDecoder::feed`/`flush` return `Vec<DecodedInput>` (see the `##
input-v2`/`KeyDecoder` amendment in `2026-07-07-command-config-interfaces.md`
for the full decoder-shape change). SGR mouse recognition: `classify_csi`
checks `buf[2] == b'<'` FIRST (before its generic CSI final-byte scan, since
`<` is not itself a valid CSI final byte and the generic scan would
otherwise misparse the whole sequence as a bogus `Char('M')`/`Char('m')` key
carrying the entire sequence's bytes as `raw`) and hands off to
`classify_sgr_mouse`, which scans for an `M` (press/drag/wheel) or `m`
(release) final byte, parsing the `Cb;Cx;Cy` parameter string once found;
`None` (incomplete, keep buffering) while no `M`/`m` byte has arrived yet, OR
while a malformed body byte (not an ASCII digit or `;`) is seen before one —
the latter never completes for THAT exact buffer state, but the decoder's
existing `MAX_PENDING`-bound peel (unrelated to this feature, pre-existing)
still guarantees the stream can't stall forever on a malformed/never-
terminating sequence.

`Cb` decoding (`mouse_kind_from_cb`): bit `0x40` marks a wheel event (low 2
bits: `0` = up, else down); bit `0x20` marks motion (a held-button drag,
requiring `?1002h`, which winmux always enables alongside `?1000h`); otherwise
`(low 2 bits) + 1` gives the 1-based button number, and press vs release
comes from the `M`/`m` final byte, not `Cb`. Modifier bits: `shift = Cb &
0x04`, `meta = Cb & 0x08`, `ctrl = Cb & 0x10` (same bit-shape as
`keys::mods_from_param`'s CSI-modifier decoding elsewhere in this module,
different base — not reused, since the two encodings' bit MEANINGS happen to
overlap but their SOURCE parameter is different (`Cb` vs the CSI `<mod>`
field), and conflating them would be a coincidence-driven false
abstraction).

**Consume-always decision (RAW-BYTE FIDELITY invariant):** a complete,
recognized SGR mouse sequence ALWAYS decodes as `DecodedInput::Mouse`,
regardless of whether the `mouse` option is currently on. The client only
ever emits these bytes because winmux itself sent the enable sequence to it
(`MOUSE_ENABLE_SEQ`, below) — a decodable mouse sequence arriving is never a
coincidental collision with literal typed text. Dropping a decoded `Mouse`
event when `mouse` is off is `dispatch_mouse`'s job (a silent `Ok`), not the
decoder's — the decoder's contract is "what did these bytes decode to",
independent of any runtime option.

Unit tests (`keys::tests`): `decode_sgr_mouse_press`, `_release`, `_drag`,
`_wheel`, `_modifiers`, `_split_across_feeds` (incremental delivery, same
pattern as the existing CSI-arrow split test), `_then_key_in_same_feed`
(ordering with an adjacent decoded key in one `feed()` call);
`flush_preserves_raw_concatenation`/`decode_runaway_csi_is_bounded` extended
with SGR-mouse cases (via a new `item_raw` helper covering both
`DecodedInput` variants).

### `input` amendment

```rust
pub enum KeyInputEvent {
    Forward(Vec<u8>),
    Key { table: WhichTable, key: crate::keys::Key, raw: Vec<u8> },
    Captured(Vec<u8>),
    Mouse { event: crate::keys::MouseEvent, raw: Vec<u8> }, // NEW
}
```

See the `## input-v2` amendment in `2026-07-07-command-config-interfaces.md`
for the bypass/ordering/capture-mode semantics. Unit tests
(`input::key_machine_tests`): `mouse_bypasses_prefix_and_repeat` (a mouse
event arriving with `Prefixed` state armed reports `Mouse` immediately AND
leaves the armed state intact for the NEXT key), `mouse_flushes_pending_
forward_first` (ordering: `Forward` then `Mouse`, not merged/reordered).

### `options` amendment

`mouse` (`Flag`, default `off`) already existed as an SP4-accepted-inert
option (SPECS/default_value unchanged); this task adds its first consumer, a
typed getter:

```rust
pub fn mouse(&self) -> bool;
```

### `server`/`server::dispatch` amendment

```rust
// src/server.rs

const MOUSE_ENABLE_SEQ: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_DISABLE_SEQ: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1006l";
const MOUSE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);
const MOUSE_WHEEL_STEP: u32 = 5; // tmux WheelUpPane/WheelDownPane default

struct ClientState {
    // ...existing fields unchanged...
    mouse: MouseClientState, // NEW
}

#[derive(Default)]
struct MouseClientState {
    last_click: Option<(std::time::Instant, u16, u16, u8, u8)>, // (when, x, y, button, run_length 1..=3)
    drag: MouseDrag,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum MouseDrag {
    #[default]
    None,
    Border { pane: layout::PaneId, vertical: bool },
    // `moved` (fix round, see the `Up` bullet below): starts `false` on
    // `Down`, flips to `true` on the first `Drag`. Distinguishes a plain
    // click (no `Drag` event at all before the matching `Up`) from an
    // actual drag-select.
    Selecting { moved: bool },
}
```

**Enable/disable sequence delivery** (design spec: "the server appends
`\x1b[?1000h...` to the next composed output per client... and `l` variants
when turned off / on client Exit"): implemented as DIRECT raw `Output`
frames (`send_output`), not woven into `render::Scene`/`Renderer::compose` —
simpler, and satisfies the same observable contract (the client's next
`Output` frame(s) contain the sequence) without adding a scene field.
- `Server::finish_attach`: if `options.mouse()` is already on, sends
  `MOUSE_ENABLE_SEQ` to the newly-attaching client's `tx` before inserting it
  into `self.clients`.
- `dispatch::Server::exec_set_option`, `name == "mouse"` branch: broadcasts
  `MOUSE_ENABLE_SEQ`/`MOUSE_DISABLE_SEQ` (whichever the new value implies) to
  EVERY currently-attached client (mouse is a global option).
- Client-side unconditional disable-on-exit is `host::apply_restore`'s job
  (see the `## host` amendment in `2026-07-06-mvp-interfaces.md`), not
  server-messaged per-exit — simpler and strictly stronger (covers a crashed
  server too).

**Routing (`dispatch::Server::dispatch_mouse`, `pub(super)`, called from
`server::handle_stdin`'s `KeyInputEvent::Mouse` arm):**
1. `!options.mouse()` → dropped (`Ok`, no-op) — see the `keys` amendment's
   "consume-always" note for why this drop happens here, not at decode time.
2. `client.mode` is `ConfirmCmd`/`Prompt` → dropped (documented deviation:
   real tmux's mouse-during-prompt behavior was left undecided by the task
   brief; winmux swallows mouse events during these overlays so a stray
   click/drag can never race a confirm's y/n capture or act on pane geometry
   the overlay is hiding).
3. `y` equals the status row for THIS client (`mouse_status_row`: row 0 if
   `status-position top` else `client.rows - 1`, `None` if `status` off) →
   `dispatch_mouse_status`.
4. Otherwise hit-tested against the shared pane area (`mouse_pane_rects`:
   `Rect { x: 0, y: pane_area_y(), w: session.size.0, h: session.size.1 }`,
   THIS session's CURRENT window's `Layout::rects`) — outside that area
   (including a blank gap row on a client taller than the session's shared
   size) is a no-op that also clears any in-progress drag.

**Hit-testing (`hit_test`, pure, tested indirectly via the `tests/
server_proto.rs` mouse suite):** pane interior first; then a vertical
(column) border (`r.x + r.w == x`, `y` within `r`'s row range) — `left` is
the `resize_from` reference leaf for a Left/Right resize; then a horizontal
(row) border (`r.y + r.h == y`, `x` within `r`'s column range) — `top` is the
reference leaf for an Up/Down resize. A cell that is simultaneously a valid
vertical- and horizontal-border position (a 4-way "+" junction) resolves to
the vertical border — documented, arbitrary tie-break. Zero-size rects never
match any branch (degrade to `None`), tolerating a too-small terminal.

**Pane-area routing:**
- `Down` on a border → arms `MouseDrag::Border { pane, vertical }` (no other
  action).
- `Down` on a pane → ALWAYS focuses it (`Layout::focus_pane`), any button.
  Additionally, when it's button 1 landing inside the pane bound to this
  CLIENT's OWN `ClientMode::Copy` (clicking a DIFFERENT pane, or clicking
  while not in copy mode at all, only focuses — documented v1 scope,
  matches the design spec's bulleted overview): `advance_click_run` (500ms
  same-cell-same-button window, capped at run length 3) decides
  single/double/triple-click semantics — 1 = `BeginSelection` at the clicked
  view cell; 2 = `select_word_at` (expands to the maximal run of same-class
  characters using tmux's hardcoded default `word-separators` = `" -_@"`,
  NOT the plain-whitespace rule `copy-next-word`/`copy-previous-word` use);
  3 = `select_line_at` (the whole clicked view row) — then arms
  `MouseDrag::Selecting { moved: false }`.
- `Drag` → `MouseDrag::Border` re-reads the reference pane's CURRENT rect
  every call (not an accumulated delta since the drag started — robust to
  layout-minimum clamping: the border always ends up exactly at the drag
  position if reachable at all) and calls `Layout::resize_from` with the
  sign/axis implied by the drag delta, then `apply_layout_for_session`.
  `MouseDrag::Selecting { .. }` sets `moved = true` (see `Up` below) and
  updates the bound `CopyState`'s `cx`/`cy` to the new cell (clamped into
  the pane's rect).
- `Up` → `MouseDrag::Border` needs no further action (already applied live).
  `MouseDrag::Selecting { moved: true }` (while still in copy mode) calls
  `exec_copy_action(CopyAction::SelectionAndCancel, ..)` — tmux's
  `MouseDragEnd1Pane` default, which fires only after an actual drag.
  **Click/drag/release semantics (fix round; matches real tmux's copy-mode
  binding table exactly):** SGR button-event tracking sends an `Up` after
  every `Down`, even with zero motion in between — a bare click, no less
  than a drag, always produces a `Down` immediately followed by an `Up`. A
  PLAIN click (`Selecting { moved: false }`, i.e. no `Drag` event was ever
  seen between this `Down` and this `Up`) is therefore explicitly EXCLUDED
  from `SelectionAndCancel`: it is a no-op on `Up` — no copy, no cancel, no
  buffer write, copy mode stays entered — because real tmux's copy-mode
  table has no default binding for a bare `MouseUp1Pane` at all, only
  `MouseDrag1Pane` (extends selection) and `MouseDragEnd1Pane` (copies and
  exits), both of which require actual motion; only `MouseDown1Pane`
  (`select-pane`, focus-only) fires for a plain click. The click's focus-on-
  `Down` and its zero-width point-selection anchor / cursor reposition still
  happen regardless — only the `Up`-side copy/cancel is gated on `moved`.
  Any other `Up` combination (border drag, or `Selecting` outside copy mode)
  is a no-op.
- `WheelUp`/`WheelDown` over a pane whose `Grid::alt_screen()` is true →
  ALWAYS 3x synthesized arrow-key presses (`\x1b[A`/`\x1b[B`) written
  straight to the pane via `pty.write_input`, regardless of copy-mode state
  (tmux's alt-screen wheel translation). Over a LIVE pane bound to this
  client's OWN copy mode → `MOUSE_WHEEL_STEP` (5) `ScrollUp`/`ScrollDown`
  `CopyAction`s; a `WheelDown` that lands exactly on `scroll == 0` AND
  `CopyState::scroll_exit` (set true only when copy mode was entered BY a
  wheel — the Task 2 placeholder field's first real consumer) exits copy
  mode back to `Normal`. Over a live pane NOT in copy mode: `WheelUp` enters
  copy mode (`exec_copy_mode(page_up: false, mouse: true, ..)`, whose
  `mouse` parameter IS `scroll_exit` directly — fix round: previously
  accepted-but-ignored [`copy-mode -e`'s `mouse` flag was a documented
  no-op], now the one place `CopyState::scroll_exit` is actually set — then
  applies `MOUSE_WHEEL_STEP` `ScrollUp`s);
  `WheelDown` is a no-op (documented v1 decision — there is no "downward"
  scrollback direction to enter copy mode from at the live bottom).

**Status-row routing (`dispatch_mouse_status`):** `Down(1)` →
`mouse_status_click`, which rebuilds the SAME left-prefix-width-then-per-
window-span layout `render_one`/`status::status_spans` draws (one separator
space between tabs, none after the last) to hit-test which window tab (if
any) column `x` falls in — a click on the `status-left` prefix, a separator,
or past the last tab is a no-op (design spec: "Down-click on a status-line
area with no window: no-op"); a hit selects that window
(`session.current`/`session.last` updated directly, then
`apply_layout_for_session`). Does NOT replicate `render::compose_back`'s
final spatial right-truncation (when left+right don't fit the terminal
width) — documented v1 gap, `docs/follow-ups.md`. `WheelUp`/`WheelDown` on
the status row → `exec_step_window(false, ..)` / `exec_step_window(true,
..)` (previous-window / next-window — tmux's default `WheelUpStatus`/
`WheelDownStatus` bindings).

**Explicit deferrals (v1 scope, documented in `docs/follow-ups.md`):**
forwarding a click/drag to the pane's own mouse-reporting application (the
design spec's "v1: NOT forwarded" note); drag-to-select on a LIVE (non-
copy-mode) pane (real tmux implicitly enters copy mode on such a drag; the
brief's own bullet list scopes "Drag1 = selection" to "In copy mode:" only);
`word-separators` as a real, settable option (hardcoded tmux default
`" -_@"` instead).

Integration tests (`tests/server_proto.rs`): `mouse_option_emits_enable_
sequences`, `mouse_click_focuses_pane`, `mouse_wheel_enters_copy_mode`,
`mouse_drag_selects_and_release_copies`, `mouse_status_click_selects_
window`, `mouse_wheel_status_cycles_windows`, `mouse_border_drag_resizes`,
`mouse_plain_click_in_copy_mode_keeps_mode_and_buffers` (fix round: the
Critical regression test — plain click in copy mode must not copy/cancel,
and must still focus its pane), `pane_default_active_border_follows_focus`
(fix round: coverage gap closed — the DEFAULT `pane-active-border-style`
green segment on a 3-pane T-junction border follows focus, not just the
runtime-restyled case `pane_active_border_style_runtime` already covered).
Unit tests (`src/server/dispatch.rs::mouse_dispatch_tests`):
`exec_copy_mode_wires_mouse_flag_to_scroll_exit` (fix round: the Minor
finding — `exec_copy_mode`'s `mouse` parameter is genuinely read now).
`alt_screen_wheel_sends_arrows` was ATTEMPTED per the task brief's suggested
approach (a real PowerShell pane self-emitting `CSI ?1049h` via `Write-Host
-NoNewline "$([char]27)[?1049h"`) and found to be exactly the "too flaky"
case the brief anticipated: real Windows ConPTY does not reliably pass a
bare `Write-Host`-emitted `?1049h` through to the server's read side as the
literal alt-screen-enter sequence (the pane visibly cleared and PowerShell's
prompt reprinted — consistent with SOME redraw happening — but the server
pane's `Grid::alt_screen()` never actually flipped true, so a wheel event
dispatched right after still entered copy mode instead of translating to
arrows; a ConPTY passthrough quirk for a synthetic escape injection, not a
winmux bug). Per the brief's documented fallback, alt-screen wheel routing
is instead covered by two unit-level tests in `src/server/dispatch.rs`
(`mouse_dispatch_tests::alt_screen_wheel_does_not_enter_copy_mode` /
`live_screen_wheel_enters_copy_mode`) that build a real `Server` + one
session/pane directly (`PaneRuntime.pty: None` — fine, since the alt-screen
branch's `pty.write_input` call is already `if let Some(pty) = ..`-gated)
and feed `\x1b[?1049h` straight into the pane's `Grid`, no ConPTY involved —
exercising the exact same `p.grid.alt_screen()` check with no passthrough
uncertainty. `grid::tests::alt_screen_getter_tracks_mode` separately covers
that `Grid` itself correctly tracks alt-screen state end to end.

## `layout-presets` — layout presets, swap-pane, rotate-window (Task 6)

Implements the design spec's `## 5. Layout presets + swap/rotate` section.
The `layout` amendment itself (`LayoutPreset`, `PRESET_CYCLE`,
`Layout::apply_preset`/`swap_panes`/`rotate`) is documented in the `## layout`
section of
[`2026-07-06-mvp-interfaces.md`](2026-07-06-mvp-interfaces.md) (that file
owns `layout`'s public surface); this section covers everything ABOVE it:
`model::Window`, `options`, `cmd`, `bindings`, and `server::dispatch`.

### `model` amendment

`Window` gains one field:

```rust
pub struct Window {
    // ...unchanged...
    /// `next-layout`'s cycle position: the `layout::PRESET_CYCLE` index of
    /// the last preset APPLIED via `select-layout`/`next-layout` (`None`
    /// until the first one ever applied). Manual splits/resizes never touch
    /// this -- `next-layout` still resumes from wherever the cycle last
    /// landed, matching tmux.
    pub last_layout: Option<u8>,
}
```

Both `Window`-constructing call sites (`Registry::create_session`'s first
window, `Session::new_window`) initialize it to `None`.

### `options` amendment

Two new `Number` options, both with typed getters returning `u16`:

```rust
// SPECS additions: Spec { name: "main-pane-width", kind: Kind::Number, .. },
//                  Spec { name: "main-pane-height", kind: Kind::Number, .. }
// defaults: main-pane-width = 80, main-pane-height = 24 (tmux defaults)

impl Options {
    pub fn main_pane_width(&self) -> u16;
    pub fn main_pane_height(&self) -> u16;
}
```

**Documented deviation: ratio-baked, not absolute, across later resizes.**
`Layout`'s tree only ever stores `f32` split ratios (no absolute-size node
variant), so `apply_preset` converts `main-pane-width`/`-height`'s absolute
cell count into a ratio via `ratio_for(target, area_len)` ONCE, at
`select-layout`/`next-layout` apply-time. The FIRST render after applying the
preset reproduces the exact configured cell count, but a LATER window
resize scales the main pane proportionally along with everything else,
rather than re-deriving the same absolute width/height the way real tmux
does (tmux recomputes the absolute size on every resize). Functionally
acceptable given the architecture and the Task 6 brief's test scope (exact
rects at a fixed area); tracked as `docs/follow-ups.md` #43 for eventual
absolute-size preservation.

### `cmd` amendment

`ParsedCmd` gains:

```rust
pub enum ParsedCmd {
    // ...
    /// `select-layout|selectl [-t target] [layout-name]`. `name: None`
    /// (bare) re-applies the target window's current cycle position
    /// (dispatch-time, needs `Window::last_layout`). `name: Some(n)` is
    /// validated against the five exact tmux layout names IN `resolve`
    /// itself (mirrors `bind-key -T`'s inline table-name validation) --
    /// `Err("unknown layout: {n}")` for anything else.
    SelectLayout { target: Option<String>, name: Option<String> },
    /// `next-layout|nextl [-t target]`: advance the target window's
    /// `next-layout` cycle by one (wrapping), per `layout::PRESET_CYCLE`.
    NextLayout { target: Option<String> },
    /// `swap-pane|swapp [-U] [-D] [-s src] [-t dst]`. `dir: Some(Up | Down)`
    /// (`-U`/`-D`) swaps a TARGET pane (`-t` if given, else the acting
    /// client's active pane -- tmux's own default) with the previous/next
    /// pane in creation order, wrapping, within the target's own window;
    /// `resolve`'s flag scanner only ever admits `-U`/`-D` for this command,
    /// so any other `Direction` reaching dispatch is unreachable. A
    /// co-supplied `-s` alongside `-U`/`-D` is a dispatch-time usage error
    /// (Task 6 fix round: winmux does not implement tmux's fuller `-s`-as-
    /// reference-override semantics for the directional form). `dir: None`
    /// uses the explicit `-s src`/`-t dst` pane targets instead (each
    /// resolved via the normal `resolve_pane_target` fallback chain); both
    /// MUST resolve to the same window, or dispatch errors (`"swap-pane: can
    /// only swap panes within the same window"` -- Task 6 fix round: this
    /// used to silently no-op cross-window instead).
    SwapPane { dir: Option<Direction>, src: Option<String>, dst: Option<String> },
    /// `rotate-window|rotatew [-D] [-t target]`. `down` is the `-D` flag;
    /// bare `rotate-window` (`down: false`) and `-D` (`down: true`) rotate
    /// in opposite directions -- see `Layout::rotate`'s doc comment.
    RotateWindow { down: bool, target: Option<String> },
}
```

Canonical names/aliases: `select-layout`/`selectl`, `next-layout`/`nextl`,
`swap-pane`/`swapp`, `rotate-window`/`rotatew`. Usage strings: `usage:
select-layout [-t target] [layout-name]`, `usage: next-layout [-t target]`,
`usage: swap-pane [-U] [-D] [-s src] [-t dst]`, `usage: rotate-window [-D]
[-t target]`.

### `bindings` amendment

Six new prefix-table defaults (`Bindings::default()`):

| Key | Command |
|---|---|
| `Space` (bound under `char_key(' ')`, NOT `named("Space")` -- same real-keypress gotcha as copy mode's spacebar bindings) | `next-layout` |
| `M-1`..`M-5` | `select-layout even-horizontal` / `even-vertical` / `main-horizontal` / `main-vertical` / `tiled` (tmux's real default order) |
| `{` | `swap-pane -U` |
| `}` | `swap-pane -D` |
| `C-o` | `rotate-window` (bare) |
| `M-o` | `rotate-window -D` |

The task brief specified `C-o`/`M-o`'s tmux semantics ("C-o = rotate-window
(upward), M-o = rotate-window -D") but neither the brief nor the design spec
pin down the EXACT permutation "upward"/"-D" maps to at the `Layout::rotate`
level. Judgment call, documented here: bare `rotate-window` (`down: false`)
calls `layout.rotate(forward: false)`; `-D` (`down: true`) calls
`layout.rotate(forward: true)`.

### `server::dispatch` amendment

Four new `exec_*` helpers, wired into both `execute_headless` (CLI/config,
acting client `None`) and `execute_for_client` (key binding / `:` prompt) —
same shared-helper pattern as every other Task 6-era command:

- `exec_select_layout`/`exec_next_layout`: resolve the target window
  (`resolve_window_target`), compute `area` from `session.size` (same
  `Rect { x: 0, y: pane_area_y(), w, h }` convention as
  `exec_split_window`/`apply_layout_for_session`), read `main-pane-width`/
  `-height` from `self.options`, call `Layout::apply_preset` with the
  window's panes in CREATION order (`panes_in_creation_order`: `layout
  .panes()` sorted ascending by `PaneId`, NOT raw tree order — see the
  `## layout` section's rationale), set `Window::last_layout`, then
  `apply_layout_for_session` to resize every pane's ConPTY to the new rects.
- `exec_swap_pane` (Task 6 fix round: reworked to close two review findings):
  `-U`/`-D` resolves the previous/next pane in creation order relative to a
  TARGET pane -- `-t` if given (resolved via `resolve_pane_target`, so it can
  name any pane, defaulting to the acting client's current window's active
  pane when `-t` is absent, matching tmux) -- within that target's own
  window (`Direction::Left`/`Right` are unreachable here — `resolve`'s flag
  scanner for `swap-pane` only ever admits `-U`/`-D`). A co-supplied `-s`
  alongside `-U`/`-D` returns the `swap-pane` usage-string error instead of
  being silently discarded (winmux does not implement tmux's fuller
  `-s`-as-reference-override matrix for the directional form — see
  `docs/follow-ups.md` #42). The explicit `-s`/`-t` form (`dir: None`)
  resolves two independent pane targets via `resolve_pane_target` and now
  REQUIRES both to resolve to the same `WindowId` (window ids are minted from
  a single global monotonic counter — `Registry::mint_window_id` — so a
  plain `!=` comparison also catches cross-session pairs): a mismatch returns
  `Err("swap-pane: can only swap panes within the same window")` instead of
  the old silent no-op. Real tmux allows moving a pane to a different window
  this way; winmux does not yet (`docs/follow-ups.md` #41 tracks it).
- `exec_rotate_window`: resolves the target window, calls `Layout::rotate`.

All four call `apply_layout_for_session` unconditionally at the end (same
established pattern as `kill_window_by_id` etc.) — harmless even when the
target window wasn't the session's current one; the geometry change is
picked up whenever that window next becomes current.

## `window-ops` — break-pane, move-window, find-window, `'` index prompt (Task 7)

Implements the design spec's `## 6. Window ops` section (`D` choose-client
stays deferred, documented there). Covers `model`, `cmd`, `server` (the
`PromptKind` enum), and `server::dispatch`.

### `model` amendment

`Session` gains one method:

```rust
impl Session {
    /// Reassign window `id`'s index to `new_index` WITHIN this session
    /// (winmux's `move-window` is same-session re-indexing only -- no
    /// `-s`-to-a-different-session support). `new_index` occupied by a
    /// DIFFERENT window: `kill == false` -> `false` (caller formats `index
    /// in use: <n>`); `kill == true` -> the occupant is removed via
    /// `Self::kill_window` first, then `id` takes `new_index`. Moving a
    /// window to the index it ALREADY occupies is a no-op success (no
    /// "occupant" in the way -- judgment call, the design spec doesn't pin
    /// this case down). `self.windows` stays sorted by index (re-sorted
    /// here too).
    pub fn move_window(&mut self, id: WindowId, new_index: u32, kill: bool) -> bool;
}
```

### `cmd` amendment

`ParsedCmd` gains three variants:

```rust
pub enum ParsedCmd {
    // ...
    /// `break-pane|breakp [-d] [-n name]`: the resolved CURRENT pane
    /// leaves its window and becomes a new window (next free index),
    /// which becomes current unless `-d`. `-n` names the new window. No
    /// pane-target flag -- winmux's break-pane always acts on the
    /// resolved current pane (the design spec's signature has no `-s`/`-t`
    /// pane selector either -- smaller, honest scope, same pattern as
    /// `swap-pane`'s own documented deviations).
    BreakPane { detached: bool, name: Option<String> },
    /// `move-window|movew [-k] -t index`: re-index the CURRENT window (of
    /// the target session) to `target` (REQUIRED -- there is nothing to do
    /// without one). Occupied index -> `index in use: <n>` unless `-k`
    /// (`kill`) kills the occupant first. `target` is resolved at dispatch
    /// time as a bare/`:`-prefixed index within the SAME session; any
    /// `session:` prefix is accepted but ignored (no cross-session move).
    MoveWindow { kill: bool, target: String },
    /// `find-window|findw <pattern>`: case-insensitive substring search
    /// (v1, no regex) over window NAMES and every pane's CURRENTLY VISIBLE
    /// content (not scrollback) in the target session, in window-index
    /// order (the current window counts too); jumps to the FIRST match.
    /// No match -> `Ok` carrying a transient `no windows matching: <p>`
    /// message (not an `Err`).
    FindWindow { pattern: String },
}
```

Canonical names/aliases: `break-pane`/`breakp`, `move-window`/`movew`,
`find-window`/`findw`. Usage strings: `usage: break-pane [-d] [-n name]`,
`usage: move-window [-k] -t index`, `usage: find-window pattern`.

### `server` (`PromptKind`) amendment

Three new variants, alongside the existing `RenameWindow`/`RenameSession`/
`Command`:

```rust
enum PromptKind {
    // ...
    /// `.` prompt: commit dispatches `move-window -t <input>` (`-k` is
    /// never supplied by the prompt -- only the explicit CLI/`:`-prompt
    /// form of `move-window -k` can kill an occupant).
    MoveWindow,
    /// `f` prompt: commit dispatches `find-window <input>`.
    FindWindow,
    /// `'` prompt: commit dispatches `select-window -t :<input>`.
    Index,
}
```

All three are pre-filled EMPTY (unlike `RenameWindow`/`RenameSession`,
which pre-fill the current name) and opened via the renamed
`Server::open_prompt` (was `open_rename_prompt` through Task 6 — private
helper, no contract impact from the rename). Labels: `"(move-window) "`,
`"(find-window) "`, and, verbatim per the design spec's `## 6. Window ops`
section, `"index"` (no parens/trailing space, unlike the other two —
deliberately kept exactly as the spec's literal string; see the Task 7
report for the tmux-rendering divergence this likely represents).

### `server::dispatch` amendment

Three new `exec_*` helpers, wired into both `execute` (headless) and
`execute_for_client`, same shared-helper pattern as every Task 6/7 command:

- `exec_break_pane`: resolves the current pane (`resolve_pane_target(cs,
  None)` — no pane-target flag), errors `"can't break with only one pane"`
  if the source window's `layout.len() <= 1` (checked BEFORE any mutation,
  independent of how many other windows the session has — a window can
  never be left with zero panes), then `layout.remove` + `mint_window_id` +
  `Session::new_window` (which makes the new window current); `-d`
  reverses that back onto the source window (`session.current = wid;
  session.last = Some(new_wid)`). Zoom-clearing and "focus falls back to
  the sibling subtree's first leaf" both come for free from
  `Layout::remove`, already exercised by `kill_pane_by_id`.
- `exec_move_window`: parses `target` (stripping any `session:` prefix and
  a leading `=`) as a `u32`, snapshots the occupant's pane ids (if any)
  BEFORE calling `Session::move_window` (needed for `self.panes`/
  `last_rects` cleanup if `-k` kills it — the occupant's `Window`/`Layout`
  is gone from the registry afterward), then respects
  `Options::renumber_windows()` same as every other structural op.
- `exec_find_window`: snapshots `(WindowId, name, pane_ids)` for every
  window in the target session, then for each in index order checks the
  name (case-insensitive substring) then every listed pane's grid via the
  new free function `grid_contains(grid: &Grid, needle_lowercased: &str) ->
  bool` (walks `0..grid.rows()` × `0..grid.cols()`, i.e. the CURRENTLY
  VISIBLE screen only, not scrollback); first match wins, `None` returns
  the `no windows matching:` message as `Ok`, not `Err`.

`'`'s dispatch has no dedicated `exec_*` — its `PromptKind::Index` commit
validates `buf` is empty or all-ASCII-digits BEFORE delegating (Task-7
review, Important finding #1: `resolve_window_target`'s bare-token "try
session name first" fallback otherwise mis-resolves a non-numeric `buf`,
either erroring `can't find session: <buf>` or, worse, silently no-opping
against an unrelated session whose name happens to match); a non-numeric,
non-empty `buf` produces `window not found: <buf>` directly (matching
`resolve_window`'s own miss wording) without calling `exec_select_window`
at all. An empty or all-digit `buf` calls the PRE-EXISTING
`exec_select_window(format!(":{buf}"), Some(session_name.as_str()))`
directly (no new command, no new dispatch table entry): `resolve_window`'s
existing bare-numeric-index handling already produces the exact required
`window not found: <n>` wording for a numeric miss, and an empty `buf`
resolves to the session's current window (a no-op).

### `bindings` amendment

Four new prefix-table defaults:

| Key | Command |
|---|---|
| `!` | `break-pane` (bare — direct dispatch, no prompt) |
| `.` | `move-window` (bare — `dispatch_client`'s `is_bare` special-casing opens the `(move-window) ` prompt with a client context, same "no-args-means-open-the-prompt" idiom as `,`/`$`) |
| `f` | `find-window` (bare — same idiom, opens the `(find-window) ` prompt) |
| `'` | `select-window` (bare — no distinct "index-window" tmux command exists, so the `'` binding repurposes a bare `select-window`, which would otherwise always be a usage error since `-t` is normally required, as the trigger for the `index` prompt) |
