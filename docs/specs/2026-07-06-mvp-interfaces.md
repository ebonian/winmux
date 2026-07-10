# Sub-project 1 — Multiplexing MVP — Locked Interface Contract

**Status:** Locked. Every implementation task MUST conform to these types and
signatures exactly. If a signature must change during implementation, the change
must be applied consistently to every consumer named here.

**Amendment (sub-project 2, Task 4):** the `input` section below was extended
with new `Action` variants (window/session/detach bindings), a new
`InputEvent::Captured` variant, and a new `InputMachine::set_capture` method.
See [`2026-07-07-server-client-interfaces.md`](2026-07-07-server-client-interfaces.md)
for the session/window model these new actions dispatch into.

**Parent spec:** [`2026-07-06-multiplexing-mvp-design.md`](2026-07-06-multiplexing-mvp-design.md)

## Crate layout

Single binary crate `winmux`, Rust edition 2021.

```
src/main.rs      — entry point: install panic hook, call app::run(), map error to exit code
src/geom.rs      — Rect, Direction (shared geometry)
src/layout.rs    — split tree (pure logic, no I/O)
src/grid.rs      — per-pane terminal emulator state (vte-driven)
src/render.rs    — compositor + cell-diff renderer (pure logic, no I/O)
src/input.rs     — prefix-key state machine (pure logic, no I/O)
src/pty.rs       — ConPTY wrapper (windows-rs)
src/host.rs      — host terminal control (windows-rs)
src/app.rs       — event loop wiring
```

## Dependencies (Cargo.toml)

```toml
[dependencies]
vte = "0.13"
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_Console",
    "Win32_System_IO",
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
```

(`Win32_System_IO` was added during implementation: windows 0.58 gates
`ReadFile`/`WriteFile` behind it. The `app` task adds
`Win32_System_SystemInformation` for `GetLocalTime`.)

`vte` is pinned to 0.13: `Parser::advance(&mut performer, byte)` takes a single
byte. If a pinned version fails to resolve or compile, the implementer may bump
it but must fix all resulting API differences in the same task.

## `geom` — shared geometry

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect { pub x: u16, pub y: u16, pub w: u16, pub h: u16 }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction { Left, Right, Up, Down }
```

`Rect` coordinates are 0-based host-terminal cells; `x` grows rightward, `y`
grows downward.

## `layout` — split tree (pure)

**Amendment (sub-project 4, Task 5 — mouse):** `resize_focused` is now a thin
wrapper over a new, more general `resize_from(&mut self, pane: PaneId, dir:
Direction, area: Rect, cells: u16) -> bool`, which takes an explicit
reference leaf instead of always using `self.focused` — needed for mouse
border-drag resize, which must be able to move a border adjacent to a pane
that ISN'T currently focused, without changing focus. `resize_focused`'s own
signature/behavior is unchanged. See the `## mouse` section of
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md).

**Amendment (sub-project 4, Task 6 — layout presets, swap-pane,
rotate-window):** three additions, all still pure (no I/O):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutPreset {
    EvenHorizontal,
    EvenVertical,
    MainHorizontal,
    MainVertical,
    Tiled,
}

/// `next-layout`'s cycle order; also the canonical index (0..=4) stored in
/// `model::Window::last_layout`.
pub const PRESET_CYCLE: [LayoutPreset; 5] = [ /* the 5 variants above, in order */ ];

impl LayoutPreset {
    pub fn name(self) -> &'static str;             // "even-horizontal", ...
    pub fn from_name(s: &str) -> Option<LayoutPreset>; // exact match only
    pub fn cycle_index(self) -> u8;                 // position in PRESET_CYCLE
}

impl Layout {
    /// Rebuild the split tree from scratch as one of the five presets.
    /// `panes` is the CALLER-supplied pane order used for placement
    /// (position 0 is the "main" pane for MainHorizontal/MainVertical) --
    /// callers pass CREATION order (ascending PaneId), not `self.panes()`'s
    /// current tree order, so a preset re-applied after a swap/rotate stays
    /// pinned to the same placement regardless of how the tree got
    /// scrambled. `main_width`/`main_height` are the `main-pane-width`/
    /// `main-pane-height` option values, clamped internally so the main pane
    /// is >= MIN_PANE_W/H and the other panes are too. A single pane always
    /// just fills `area`. Focus is preserved if still present in `panes`
    /// (else falls back to `panes[0]`); zoom is cleared. No-op if `panes` is
    /// empty.
    pub fn apply_preset(&mut self, preset: LayoutPreset, panes: &[PaneId], area: Rect, main_width: u16, main_height: u16);

    /// Swap the CONTENTS of the two leaves holding `a` and `b` (each pane
    /// keeps its id; they trade tree/screen positions). `self.focused`
    /// stores a PaneId, so a focused pane that is one of `a`/`b`
    /// automatically "follows" to its new position -- no explicit focus
    /// bookkeeping. Clears zoom. `false` (no-op) if `a == b` or either id
    /// isn't a leaf of this layout.
    pub fn swap_panes(&mut self, a: PaneId, b: PaneId) -> bool;

    /// Rotate every pane's content through the tree's leaf positions by one
    /// step. `forward` shifts each position's content to what the PREVIOUS
    /// leaf position held (content moves one position later, wrapping last
    /// -> first); `!forward` is the mirror. Focus follows the SCREEN CELL
    /// (leaf position), not the pane -- whichever position was focused stays
    /// focused, now showing whichever pane rotated into it. Clears zoom.
    /// `false` (no-op) with 0 or 1 panes.
    pub fn rotate(&mut self, forward: bool) -> bool;
}
```

Rounding rule for every preset's even splits: remainder cells go to the
EARLIER (leftmost/topmost) entries first (`even_lengths`, a private helper).
`tiled`'s rows-first grid: `rows=cols=1; while r*c<n { r+=1; if r*c<n {
c+=1 } }`; a short final row's panes are spread evenly over the row's OWN
pane count (not the full `cols`), so the last pane in a short row ends up
wider than a normal column ("last short row spans"). Deviation from the SP4
design spec's `Layout::rotate(forward: bool)` signature: none — the
parameter name and meaning match; only the mapping from
`rotate-window`/`-D` to `forward` was a judgment call (see
`cmd`/`server::dispatch` amendments in
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md)'s
`## layout-presets` section) since neither the task brief nor the design
spec pin down the exact `-D`/bare direction-to-permutation mapping.

```rust
pub type PaneId = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    /// tmux `%`: children side-by-side (left | right); the split line is vertical.
    Horizontal,
    /// tmux `"`: children stacked (top / bottom); the split line is horizontal.
    Vertical,
}

/// tmux's PANE_MINIMUM: a pane must be at least this many cells in each axis.
pub const MIN_PANE_W: u16 = 2;
pub const MIN_PANE_H: u16 = 2;

#[derive(Debug, PartialEq, Eq)]
pub struct SplitRefused;

pub struct Layout { /* private */ }

impl Layout {
    pub fn new(first: PaneId) -> Self;

    /// Split the focused pane. The new pane takes the second half
    /// (right for Horizontal, bottom for Vertical) and RECEIVES FOCUS
    /// (tmux default). Returns Err(SplitRefused) if either resulting pane
    /// would fall below MIN_PANE_W/MIN_PANE_H given `area`.
    /// Splitting clears zoom first (tmux behavior).
    pub fn split(&mut self, dir: SplitDir, new_pane: PaneId, area: Rect)
        -> Result<(), SplitRefused>;

    pub fn focused(&self) -> PaneId;

    /// Geometric navigation: move focus to the pane adjacent in `dir`, per
    /// tmux `select-pane -L/-R/-U/-D` semantics
    /// (`window_pane_find_{left,right,up,down}`, window.c; see
    /// `docs/tmux-reference/panes-and-layout.md` §1.1). Two rules:
    ///
    /// 1. **Edge-flip wrap.** The search edge is normally the column/row
    ///    immediately touching the focused pane's near side; but if the
    ///    focused pane is already flush against that side of `area`, the
    ///    edge flips to one past the FAR side, so candidates become the
    ///    panes flush against the opposite edge -- navigation wraps (e.g.
    ///    Left from the leftmost pane reaches the rightmost), symmetric in
    ///    all four directions.
    /// 2. **MRU tie-break.** A candidate is any pane flush against the
    ///    (possibly wrapped) edge AND whose cross-axis range genuinely
    ///    OVERLAPS the focused pane's cross-axis range (a real
    ///    interval-overlap test, INCLUSIVE at the boundary: a candidate
    ///    whose near edge lands exactly on the focused pane's far boundary
    ///    still counts, per tmux's `window.c:1992-1998` one-past-edge
    ///    convention -- corner-touching-only candidates are not excluded;
    ///    2026-07-10 review fix, closes the pre-existing strict-`<` gap
    ///    from the 2026-07-08 hotfix). Among multiple candidates, the one with
    ///    the greatest `activity(pane)` value wins (tmux's real
    ///    `active_point` recency counter, `window_pane_choose_best`); ties
    ///    (e.g. every candidate still at its caller-supplied default)
    ///    resolve to whichever was seen FIRST in leaf/pane-index order,
    ///    since only a strictly-greater candidate ever replaces the running
    ///    best.
    ///
    /// `activity` is read-only and caller-supplied: `Layout` itself has no
    /// counter (its per-window `last_focused` field is retained ONLY for
    /// [`Self::focus_last`], the `prefix ;` toggle -- an unrelated feature).
    /// Real tmux's `active_point` counter is global across the whole
    /// server (meaningful across windows/sessions), so it is owned and
    /// stamped by the server (`Server::pane_activity` /
    /// `Server::stamp_active`, `src/server.rs`) at every
    /// `window_set_active_pane`-equivalent call site (explicit selection,
    /// directional navigation, last-pane toggle, mouse focus, rotate,
    /// spawn-time creation -- but NEVER on `window_lost_pane`-shaped
    /// death handoffs, nor on break-pane's recycled pane
    /// (cmd-break-pane.c:158 assigns `w->active` directly): tmux
    /// reassigns the active pane WITHOUT bumping the counter in both
    /// cases; see `Server::stamp_active`'s doc for the full stamp/
    /// no-stamp map), and threaded in here as a closure. This
    /// replaces the old single-slot `last_focused`-based MRU approximation
    /// (follow-up #65, now resolved).
    ///
    /// Returns false (no change) if there is no candidate in that direction.
    //
    // Hotfix note (2026-07-08): the original implementation tested only
    // whether the focused pane's cross-axis MIDPOINT fell inside a
    // candidate's range. When the focused pane spans the full cross-axis
    // length opposite a split column/row (e.g. a full-height pane next to a
    // top/bottom split), that midpoint could land exactly on the border
    // between two candidates and match neither, so directional navigation
    // silently no-op'd. Replaced with the real interval-overlap test kept
    // above (unaffected by the 2026-07-10 wrap/MRU signature change below).
    //
    // Signature change (2026-07-10, SP6 parity wave 2 Task 3): added the
    // `activity` parameter and the edge-flip wrap rule described above.
    // Every caller (`src/server/dispatch.rs`'s `exec_select_pane`) and every
    // unit test in `src/layout.rs` updated in the same commit.
    pub fn focus_dir(&mut self, dir: Direction, area: Rect, activity: &dyn Fn(PaneId) -> u64) -> bool;

    /// Cycle focus to the next pane in leaf (tree, left-to-right) order,
    /// wrapping.
    pub fn focus_next(&mut self);

    /// Toggle focus to the previously-focused pane, if it still exists.
    pub fn focus_last(&mut self);

    /// Remove pane `id`. Its sibling subtree absorbs the space. If the focused
    /// pane was removed, focus moves to the nearest remaining leaf of the
    /// sibling subtree. Clears zoom. Returns false (tree unchanged) if `id`
    /// is the only pane — the caller exits the app instead.
    pub fn remove(&mut self, id: PaneId) -> bool;

    /// Move the focused pane's nearest enclosing split edge in `dir` by
    /// `cells` cells (tmux Ctrl-arrow = 1 cell). Clamped so no pane violates
    /// minimums within `area`. Returns false if nothing changed.
    pub fn resize_focused(&mut self, dir: Direction, area: Rect, cells: u16) -> bool;

    /// Task 5 (mouse) addition: generalizes `resize_focused` to an explicit
    /// reference pane instead of `self.focused` (see the amendment note
    /// above). Never changes focus.
    pub fn resize_from(&mut self, pane: PaneId, dir: Direction, area: Rect, cells: u16) -> bool;

    /// Toggle zoom on the focused pane. Zoom auto-clears on split/remove.
    pub fn toggle_zoom(&mut self);
    pub fn is_zoomed(&self) -> bool;

    // Hardening note (follow-up #5, resolved sub-project-2 Task 10):
    // `focus_dir`'s Right/Down adjacency checks use `saturating_add` instead
    // of `+` so extreme-coordinate areas (near u16::MAX) can't overflow-panic
    // in debug builds. Behavior-preserving for all reachable terminal sizes.

    /// Compute pane rectangles within `area`. Exactly ONE border row/column
    /// separates siblings; rects EXCLUDE border cells. When zoomed, returns
    /// only [(focused, area)]. Split arithmetic: along the split axis with
    /// total length L, child1 gets round((L - 1) as f32 * ratio) cells,
    /// child2 gets L - 1 - child1 (the -1 is the border). Default ratio 0.5.
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)>;

    /// All pane ids in leaf order.
    pub fn panes(&self) -> Vec<PaneId>;

    pub fn len(&self) -> usize;
}
```

## `grid` — per-pane terminal emulator

**SUPERSEDED (sub-project 4, Task 1):** `Grid::new` gained a third
`history_limit: u32` parameter, and `Grid` gained `history_len`, `view_cell`,
`view_row_text`, `title`, and `take_title_changed`. Real scrollback capture
and a real (save/restore) alternate screen replaced the MVP's "no scrollback,
alt screen just clears" behavior; OSC 0/2 now capture the pane title instead
of being ignored. See the `## grid-v2` section of
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md)
for the full current contract. This section is kept for historical reference
only; everything below it describing scrollback-free/OSC-ignored MVP behavior
is superseded.

**FURTHER SUPERSEDED (SP7, Task 2 — closes follow-up #47):** `grid-v2`'s
"no reflow" divergence note (below, and mirrored in
`2026-07-07-parity-polish-interfaces.md`'s `## grid-v2` section) is itself
now superseded. `Grid::resize`'s signature is UNCHANGED, but on the primary
(non-alt) screen a column-WIDTH change now reflows scrollback + the live
screen to the new width like tmux ≥1.9 (`grid_reflow`): wrapped-chain rows
(tracked by a new private per-row `wrapped` flag — tmux's
`GRID_LINE_WRAPPED` — set only when the cursor auto-wraps off the right
margin, cleared on an explicit linefeed) are concatenated into logical
lines, then re-split at the new width (`ceil(len / new_cols)` rows per
logical line, all but the last marked wrapped); a row-COUNT-only change (or
any resize while showing the alternate screen — the alt screen still NEVER
reflows, tmux clears/redraws it instead) keeps the original clip/pad
behavior. Cursor mapping follows tmux's `grid_wrap_position`/
`grid_unwrap_position`: preserved by offset within its own logical line,
collapsing to "end of the line" if it sat past real content, and resetting
to `(0, 0)` if its line was itself evicted by the resize. See
`docs/specs/2026-07-07-parity-polish-interfaces.md`'s `## grid-v2` section
for the amended prose (amended in the same commit as this note, even though
that file sits outside this task's normal edit scope, because it — not this
superseded section — is the contract's actual current authority on `resize`
and `view_cell`'s history semantics, and leaving its "no reflow" claim
uncorrected there would misdocument the real behavior).

**Caveat (SP7 review fix):** `history_total()`'s "difference between two
readings = view rows shifted" invariant (see its own doc comment) holds only
for mutations that do NOT change the grid's width — `reflow_to_width` does
not bump `history_total` to match the (non-uniform) restructuring it
performs, so a coordinate pinned across a width-changing resize (e.g. a
copy-mode selection anchor) cannot be repaired by any single corrected shift
count. The server layer treats a width-changing resize of a pane bound to an
active copy-mode selection as invalidating that selection outright (clears
it) rather than attempting to remap it — see `Server::apply_layout_for_session`
in `src/server.rs`, which matches real tmux's `window_copy_size_changed`
(unconditional selection clear on ANY resize of the pane, width or height).

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color { Default, Idx(u8), Rgb(u8, u8, u8) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}
impl Default for Style { /* all flags false, both colors Color::Default */ }

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell { pub ch: char, pub style: Style }
impl Default for Cell { /* ch: ' ', style: Style::default() */ }

pub struct Grid { /* private; owns a vte::Parser + emulator state */ }

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self;
    /// Feed raw VT bytes from the pane's ConPTY output.
    pub fn feed(&mut self, bytes: &[u8]);
    /// Content is clipped (shrink) or padded with default cells (grow);
    /// cursor is clamped into range.
    pub fn resize(&mut self, cols: u16, rows: u16);
    pub fn cols(&self) -> u16;
    pub fn rows(&self) -> u16;
    pub fn cell(&self, col: u16, row: u16) -> Cell;   // panics out of range
    // Hardening note (follow-up #6, resolved sub-project-2 Task 10): the
    // panic message is `"cell({col}, {row}) out of bounds {cols}x{rows}"`,
    // including both the requested coordinates and the grid's dimensions.
    pub fn cursor(&self) -> (u16, u16);               // (col, row)
    pub fn cursor_visible(&self) -> bool;             // DECTCEM state, default true
}
```

**vte borrow pattern (required):** `Grid` holds `parser: vte::Parser` and a
separate `state: TermState` where `TermState: vte::Perform`. `feed` iterates
`self.parser.advance(&mut self.state, byte)` per byte. (A single struct cannot
be both the parser owner and the performer.)

**VT support scope** (what PowerShell + typical CLI tools emit):
- Printable chars with autowrap (DECAWM on by default, `CSI ?7 h/l` honored).
- C0: BS(0x08), HT(0x09, 8-col tab stops), LF(0x0A), CR(0x0D), BEL(ignored).
- CSI: CUU/CUD/CUF/CUB (`A B C D`), CNL/CPL (`E F`), CHA (`G`), CUP/HVP (`H f`),
  ED (`J` 0/1/2), EL (`K` 0/1/2), ICH (`@`), DCH (`P`), ECH (`X`), IL (`L`),
  DL (`M`), SU (`S`), SD (`T`), DECSTBM (`r`), save/restore cursor (`s`/`u`),
  DECTCEM (`?25 h/l`).
- SGR (`m`): 0, 1, 2, 3, 4, 7, 22, 23, 24, 27, 30–37, 39, 40–47, 49, 90–97,
  100–107, 38;5;n, 48;5;n, 38;2;r;g;b, 48;2;r;g;b.
- ESC: `7`/`8` (save/restore cursor), `M` (reverse index).
- Alt screen `CSI ?1049 h/l`: MVP treats enter as "clear screen, home cursor"
  and leave as "clear screen, home cursor" (no saved primary screen; no
  scrollback in MVP so nothing is lost).
- OSC (titles etc.): parsed and ignored. Unknown sequences: ignored, never panic.
- Lines scrolled off the top are dropped (no scrollback in MVP).

## `render` — compositor + differ (pure)

```rust
use crate::geom::Rect;
use crate::grid::Grid;
use crate::layout::PaneId;

pub struct PaneView<'a> {
    pub id: PaneId,
    pub rect: Rect,
    pub grid: &'a Grid,
    pub focused: bool,
    pub dead: bool,
}

/// **LOCKED-CONTRACT AMENDMENT (2026-07-07, SP2 Task 5, historical):**
/// `Scene::status_left: String` was first replaced by `Scene::status_spans:
/// Vec<StatusSpan>` (`StatusSpan { text: String, underline: bool }`) so the
/// status bar could render a real window list with the current window
/// underlined.
///
/// **LOCKED-CONTRACT AMENDMENT (2026-07-07, SP3 Task 8):** `StatusSpan` was
/// then DELETED and the status/message/border fields replaced wholesale so
/// every visual decision comes from the option table (`status-style`,
/// `window-status(-current)-style`, `message-style`,
/// `pane(-active)-border-style`, `status-position`, `status on|off`) instead
/// of hardcoded SGRs. Spans now carry FULLY RESOLVED `grid::Style`s (the old
/// per-span `underline` bool is subsumed); see the sibling SP3 contract's
/// `## render-styles` section for the server-side building rules and
/// `2026-07-07-server-client-interfaces.md` `## status` for the span
/// composition. With default options the emitted bytes are IDENTICAL to the
/// pre-amendment output (pinned by the untouched e2e suites).
pub struct StatusRow {
    /// true = row 0 (`status-position top`); false = the bottom row. Pane
    /// rects are computed by the server to leave this row free — the
    /// renderer just paints where told.
    pub top: bool,
    /// Row fill style (`status-style` applied to the default style).
    pub base: grid::Style,
    /// Left-aligned runs, each with its resolved style.
    pub spans: Vec<(String, grid::Style)>,
    pub right: String,
    /// Style for `right` (`base` in SP3; as of **SP6 Task 4** the server
    /// populates this with `status-right-style` layered over `base` — see
    /// that task's amendment in `2026-07-07-server-client-interfaces.md`'s
    /// `## status` section. Field TYPE is unchanged; only the VALUE the
    /// server assigns it changed, so no signature amendment was needed
    /// here. `right` never carries inline `#[...]`-styled sub-runs — there
    /// is only one style slot — any such markers are stripped to plain text
    /// before assignment (`status::strip_style_markers`).
    pub right_style: grid::Style,
}

/// **LOCKED-CONTRACT AMENDMENT (2026-07-08, SP4 Task 8 — overlays):**
/// `PaneView` gains `copy: Option<render::CopyView>` (SP4 Task 2, already in
/// effect — see the sibling SP4 contract's `## copy-mode` section) and
/// `Scene` gains `mode_style: grid::Style`, `display_panes_colour: grid::
/// Style`, `display_panes_active_colour: grid::Style`, and `overlay:
/// Option<render::Overlay>`:
///
/// ```rust
/// pub struct ListOverlay {
///     /// Optional header row (empty = none); painted in the default style.
///     pub title: String,
///     /// (already-formatted row text, is this row selected).
///     pub rows: Vec<(String, bool)>,
///     /// Index into `rows` of the first row shown below the title —
///     /// scrolling when `rows` is longer than the available height.
///     pub top: usize,
/// }
///
/// pub enum Overlay {
///     /// choose-tree: clears/replaces the WHOLE client area; selected row
///     /// painted in `Scene::mode_style`, everything else default style.
///     List(ListOverlay),
///     /// display-panes: `(pane rect, digit 0-9, is the focused pane)` —
///     /// colour comes from `Scene::display_panes_colour`/
///     /// `display_panes_active_colour`, not carried per-entry.
///     PaneDigits(Vec<(geom::Rect, u32, bool)>),
/// }
/// ```
///
/// Painted LAST, over everything else `compose_back` already composed
/// (panes/borders/status/message) — see the compositing rules below. Both
/// new colour fields resolve `display-panes-colour`/`-active-colour`
/// (design spec `## 7. Overlays`; defaults blue/red) applied as a `bg` onto
/// the default style. See `2026-07-07-parity-polish-interfaces.md`'s
/// `## overlays` section for the server-side building rules (`ClientMode::
/// ChooseTree`/`DisplayPanes`, the hardcoded key tables, the `cmd`/
/// `bindings`/`options` amendments).
///
/// **LOCKED-CONTRACT AMENDMENT (2026-07-10, sub-project 6 wave 2, Task 11 —
/// half-border active indication + `pane-border-indicators`):** `Scene`
/// gains `border_indicators: render::BorderIndicators`:
///
/// ```rust
/// pub enum BorderIndicators { Off, Colour, Arrows, Both }
/// ```
///
/// This REPLACES the border-styling rule stated in the compositing rules
/// below ("Border cells adjacent to the focused pane use
/// `Scene::border_active`; all other border cells use `Scene::border`") —
/// see the updated compositing rule further down for the exact per-cell
/// OWNER attribution (general adjacency vs. the two-pane half-border rule)
/// and the arrow-glyph pass, both gated by this new field. Maps 1:1 from
/// the new `pane-border-indicators` option
/// (`2026-07-07-command-config-interfaces.md`'s `## pane-border-indicators`
/// section, `Options::pane_border_indicators`); default `Colour` reproduces
/// tmux's own default and is the ONLY thing that changes the DEFAULT-byte
/// output of a two-pane window versus pre-Task-11 winmux (the whole shared
/// divider no longer reads uniformly active — this was the bug the task
/// fixes, a sanctioned, documented default-output change scoped to exactly
/// that layout shape; 1-pane and 3+-pane windows are byte-identical to
/// before).
pub struct Scene<'a> {
    /// Host terminal size (cols, rows).
    pub size: (u16, u16),
    pub panes: Vec<PaneView<'a>>,
    pub zoomed: bool,
    /// None = `status off`: no status row is painted; panes may occupy every
    /// row (the server's pane-area computation already freed the row).
    pub status: Option<StatusRow>,
    /// When Some, replaces the status row's content (confirm prompt,
    /// "terminal too small", transient messages), drawn with its own
    /// resolved style (`message-style`). With `status off` the message
    /// overlays the BOTTOM row (tmux draws messages on the last line even
    /// without a status bar). With an `Overlay::List` active, the message
    /// instead takes the PANEL's own last row (see the compositing rules).
    pub message: Option<(String, grid::Style)>,
    /// Border cell style (`pane-border-style` resolved; default = default
    /// style).
    pub border: grid::Style,
    /// Style for border cells whose OWNER is the focused pane (Task 11 —
    /// see `BorderIndicators` above for exactly what "owner" means and the
    /// updated compositing rule below for the full per-cell attribution;
    /// `pane-active-border-style` resolved, default fg green).
    pub border_active: grid::Style,
    /// See `BorderIndicators` above (Task 11).
    pub border_indicators: BorderIndicators,
    /// Copy mode's position-indicator/selection style (`mode-style`
    /// resolved; default `bg=yellow,fg=black`) — ALSO the choose-tree
    /// selected-row highlight style (SP4 Task 8).
    pub mode_style: grid::Style,
    /// display-panes digit-block colour for every pane except the focused
    /// one (SP4 Task 8; default blue).
    pub display_panes_colour: grid::Style,
    /// display-panes digit-block colour for the focused pane (SP4 Task 8;
    /// default red).
    pub display_panes_active_colour: grid::Style,
    /// choose-tree / display-panes overlay (SP4 Task 8); `None` = inactive.
    pub overlay: Option<Overlay>,
}

pub struct Renderer { /* private: front + back cell buffers */ }

impl Renderer {
    pub fn new(cols: u16, rows: u16) -> Self;
    /// Resizes both buffers and invalidates the front buffer so the next
    /// compose() emits a full repaint (preceded by CSI 2J).
    pub fn resize(&mut self, cols: u16, rows: u16);
    /// Compose the scene into the back buffer, diff against the front buffer,
    /// swap, and return the VT byte stream to write to the host terminal.
    /// `cursor` is the absolute host position for the real cursor
    /// (focused pane rect origin + its grid cursor), None or
    /// `cursor_visible == false` emits hide-cursor (CSI ?25l), otherwise the
    /// stream ends with CUP to `cursor` + CSI ?25h.
    pub fn compose(&mut self, scene: &Scene, cursor: Option<(u16, u16)>,
                   cursor_visible: bool) -> Vec<u8>;
}
```

**Compositing rules (amended SP3 Task 8 — option-driven styles; border rule
amended again by Task 11, sub-project 6 wave 2, below):**
- Pane cells copy from `grid.cell(...)` into the pane's rect; cells outside
  any pane rect (borders) are drawn with box chars `─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼`
  (junction-aware; glyph logic unchanged since the MVP). When zoomed, no
  borders. Panes and borders never draw on the status row (whichever row
  `StatusRow::top` selects); with `status: None` they may occupy every row,
  including the bottom one.
- **Border colouring (Task 11 — supersedes the pre-Task-11 "adjacent to the
  focused pane" rule stated by earlier revisions of this section):** gated
  by `Scene::border_indicators`. `Off`: every border cell uses
  `Scene::border`, no exceptions. `Colour`/`Both`: a border cell's OWNER
  determines `Scene::border_active` (owner is focused) vs. `Scene::border`
  (owner isn't) --- ownership is, by default, "any pane the cell is
  orthogonally adjacent to that happens to be focused" (the general rule,
  unchanged since SP3 Task 8, still governing 1-pane and 3+-pane windows
  exactly as before), EXCEPT when the window has exactly two tiled panes:
  then the ONE shared divider between them is split cosmetically instead —
  for a side-by-side split, divider cells with `wy <= sy/2` (0-based row
  offset from the pane pair's shared top edge; `sy` = their shared height)
  are owned by the LEFT pane, the rest by the RIGHT; for a stacked split,
  `wx <= sx/2` is owned by the TOP pane, the rest by the BOTTOM
  (`docs/tmux-reference/panes-and-layout.md` §7.1's two-pane special case —
  this is the fix for the reported bug where a 1:1 two-pane split painted
  the WHOLE shared divider active, since the general rule alone considers
  both panes adjacent to every cell of it). `Arrows`/`Off` never apply
  `border_active` — with `Arrows` the divider is always plain `border`, only
  the glyph pass below adds indication.
- **Border arrows (Task 11):** `Arrows`/`Both` additionally paint, AFTER the
  colouring pass, up to four glyphs just inside each corner of the focused
  pane's own border — one per side that actually has a border cell there (a
  pane flush against the window edge has no border on that side, so gets no
  arrow there). Position: top/bottom borders at `x = pane.rect.x + 1`;
  left/right borders at `y = pane.rect.y + 1`. Each glyph points INTO the
  focused pane: top border → `↓` (U+2193), bottom border → `↑` (U+2191),
  left border → `→` (U+2192), right border → `←` (U+2190) — the doc's
  Windows-note substitution for tmux's ACS arrow characters. The glyph
  reuses whatever style the colouring pass already painted onto that cell
  (only the character changes), so `Arrows` alone draws it on the plain
  `pane-border-style` and `Both` draws it on the active colour.
- Dead pane: its grid still renders; the string `[exited]` is overlaid in
  reverse video at the pane rect's top-left (skipped if the rect's top row
  IS the status row).
- Status row (`Scene::status: Some`): drawn on row 0 when `top`, else the
  bottom row; the row is filled with `base`-styled spaces, then `spans` drawn
  left-to-right starting at col 0, each span's cells carrying that span's own
  resolved style (SGR is always emitted as one combined `\x1b[0;...m`
  sequence per style change — see `sgr()`), then `right` right-aligned in
  `right_style`; middle padded with spaces; truncate right-first if too
  narrow (left length for this purpose is the sum of all spans' char counts).
  `Scene::status: None` (`status off`): no status bytes are emitted at all.
- Message (`Scene::message: Some((text, style))`): replaces the status row's
  content, filling that row with `style`; when `status` is `None` it overlays
  the BOTTOM row instead.
- **Overlay pass (SP4 Task 8), painted LAST:** `Overlay::List` clears the
  ENTIRE client area to the default style, paints an optional title row (row
  0, default style), then `rows` (each padded to full width: selected in
  `Scene::mode_style`, others default style), scrolled so `top`'s row is the
  first shown below the title. If `Scene::message` is also `Some` (e.g.
  choose-tree's `x` kill-confirm prompt), it takes the panel's own LAST row
  (reserved before laying out `rows`, painted after them so it always wins)
  instead of the ordinary status-row placement above — the panel would
  otherwise have already overwritten it. `Overlay::PaneDigits` paints, for
  each `(rect, digit, active)`: a centered 5x5 block-digit bitmap (space
  cells in `display_panes_colour`/`display_panes_active_colour`) when `rect`
  is at least 6 wide x 5 tall, else a single centered glyph in the same
  colour (a "small-number fallback", `rect.w == 0 || rect.h == 0` paints
  nothing) — touching only cells inside each listed pane's rect, leaving
  everything else (borders, status, other panes) untouched.
- Diff emission: for each changed cell, emit minimal CUP (skip if the cursor is
  already adjacent from the previous emitted cell) + SGR (only on style change)
  + the char. UTF-8 encode chars. Reset SGR (CSI 0m) at stream end.
- **Default-byte equivalence (Task 8 invariant):** with all options at their
  tmux defaults, `compose` emits byte-for-byte the same stream as before this
  amendment — pinned by the unchanged expected byte strings in `render.rs`'s
  default-styled unit tests and the untouched `tests/e2e.rs` /
  `tests/e2e_sessions.rs`.

## `input` — prefix state machine (pure)

**SUPERSEDED (sub-project 3, Task 6):** `Action`/`InputEvent`/`InputMachine`
below were DELETED from `src/input.rs` once `src/server.rs` was rewired onto
the table-driven `KeyMachine`/`KeyInputEvent`/`Bindings` pipeline — see the
`## input-v2` and `## bindings` sections of
`docs/specs/2026-07-07-command-config-interfaces.md` for the replacement
(locked) contract, and that same file's `## server-dispatch` section for how
the server resolves `KeyInputEvent::Key` against the mutable bindings table
and dispatches the resulting commands. This section is kept for historical
reference only — every behavior it describes (split/focus/resize/zoom/close/
window nav/rename/detach/switch-client) is reproduced exactly by
`crate::bindings::Bindings::default()`'s commands, unit-tested in
`bindings::tests::defaults_cover_current_behavior`.

```rust
use std::time::Instant;
use crate::geom::Direction;
use crate::layout::SplitDir;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Split(SplitDir),
    Focus(Direction),
    FocusNext,       // prefix o
    FocusLast,       // prefix ;
    RequestClose,    // prefix x  → app shows confirm prompt + calls set_confirming(true)
    ToggleZoom,      // prefix z
    Resize(Direction), // prefix Ctrl-arrow, repeatable
    Quit,            // internal: not bound to a key in MVP (app exits when last pane dies)
    // --- Added in sub-project 2 (see docs/specs/2026-07-07-server-client-interfaces.md
    // for the window/session model these dispatch into): ---
    NewWindow,          // prefix c
    NextWindow,         // prefix n
    PrevWindow,         // prefix p
    LastWindow,         // prefix l
    SelectWindow(u32),  // prefix 0-9 (digit value, not the ASCII byte)
    RequestKillWindow,  // prefix &
    RenameWindow,       // prefix ,
    RenameSession,      // prefix $
    Detach,             // prefix d
    SwitchClientPrev,   // prefix (
    SwitchClientNext,   // prefix )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Bytes to write verbatim to the focused pane's ConPTY input.
    Forward(Vec<u8>),
    Action(Action),
    /// Emitted while confirming a close: true = confirmed (y/Y), false = cancelled.
    ConfirmClose(bool),
    /// Added in sub-project 2. Emitted only while capture mode is on
    /// (`set_capture(true)`): raw, uninterpreted bytes for a status-line
    /// prompt (e.g. rename-window line editing), coalesced per `feed()`
    /// call like `Forward`.
    Captured(Vec<u8>),
}

pub struct InputMachine { /* private */ }

impl InputMachine {
    pub fn new() -> Self;
    /// Feed raw stdin bytes; `now` drives the resize repeat window
    /// (REPEAT_TIME below). May buffer an incomplete escape sequence
    /// internally across calls.
    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<InputEvent>;
    /// App arms/disarms confirm mode after Action::RequestClose.
    pub fn set_confirming(&mut self, on: bool);
    /// Added in sub-project 2. Turn raw capture mode on/off. While on,
    /// `feed()` bypasses all state-machine dispatch (Prefixed/Repeat/
    /// Confirming) and returns every byte verbatim as `InputEvent::Captured`
    /// — including the prefix byte and escape sequences (no parsing).
    /// Turning on clears any pending escape-sequence buffer and prefix
    /// state, mirroring `set_confirming`. Capture takes precedence over
    /// Confirming if both were somehow set (capture is checked first in
    /// `feed()`, independently of the underlying `state`). Turning off
    /// resumes Normal.
    pub fn set_capture(&mut self, on: bool);
}

pub const PREFIX: u8 = 0x02;                       // Ctrl-b
pub const REPEAT_TIME: std::time::Duration = std::time::Duration::from_millis(500);
```

**State machine semantics (tmux-exact):**
- Normal: bytes stream to `Forward` untouched, except `0x02` (consumed, arms
  Prefixed). Coalesce consecutive forwarded bytes into one `Forward`.
- Prefixed (next key is a command, consumed):
  - `%` → `Action(Split(SplitDir::Horizontal))` (left|right)
  - `"` → `Action(Split(SplitDir::Vertical))` (top/bottom)
  - Arrow keys (`ESC [ A/B/C/D` = Up/Down/Right/Left) → `Action(Focus(dir))`
  - `o` → `FocusNext`; `;` → `FocusLast`; `x` → `RequestClose`; `z` → `ToggleZoom`
  - Ctrl-arrows (`ESC [ 1;5A` etc.) → `Action(Resize(dir))`, then enter
    Repeat state until `now + REPEAT_TIME`.
  - `0x02` again → `Forward(vec![0x02])` (send literal Ctrl-b)
  - anything else → disarm silently (swallow the key).
  - An incomplete `ESC`-sequence tail is buffered until the next `feed`.
  - Added in sub-project 2, also consumed and returning to Normal: `c` →
    `NewWindow`; `n` → `NextWindow`; `p` → `PrevWindow`; `l` → `LastWindow`;
    `0`..=`9` → `SelectWindow(digit)` (u32 digit value, not the ASCII byte);
    `&` → `RequestKillWindow`; `,` → `RenameWindow`; `$` → `RenameSession`;
    `d` → `Detach`; `(` → `SwitchClientPrev`; `)` → `SwitchClientNext`.
- Repeat: a Ctrl-arrow within the window → another `Resize` and the window
  restarts. Any other input → leave Repeat, process that input as Normal.
- Confirming (set via `set_confirming(true)`): next key `y`/`Y` →
  `ConfirmClose(true)`; any other key → `ConfirmClose(false)`. Either way the
  machine returns to Normal (the app also calls `set_confirming(false)`).
  Keys in this mode are consumed, never forwarded.
- Capture (added in sub-project 2, set via `set_capture(true)`): every byte,
  regardless of what `state` would otherwise dispatch to — including the
  prefix byte and escape sequences — comes out as `Captured(bytes)`, raw and
  unparsed, coalesced per `feed()` call. This check happens before any
  `state` match, so capture wins even over Confirming if both flags were
  somehow set at once. `set_capture(false)` resumes Normal.

## `pty` — ConPTY wrapper

```rust
pub struct Pty { /* private: HPCON, process + thread handles, pipe handles */ }
pub struct PtyReader { /* private: owned dup of the output read handle */ }

impl Pty {
    /// Create pipes + CreatePseudoConsole(cols, rows) + spawn `cmdline` with
    /// PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE via CreateProcessW
    /// (EXTENDED_STARTUPINFO_PRESENT).
    pub fn spawn(cmdline: &str, cols: u16, rows: u16) -> std::io::Result<Pty>;
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()>;
    /// Reader for the dedicated reader thread. Blocking Read; returns Ok(0)
    /// or ERROR_BROKEN_PIPE (map to Ok(0)) at EOF.
    pub fn take_reader(&mut self) -> std::io::Result<PtyReader>;  // once; Err on second call
    pub fn write_input(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    /// Raw process HANDLE value (as isize) for a waiter thread to
    /// WaitForSingleObject on. The Pty retains ownership.
    pub fn process_handle_raw(&self) -> isize;
    pub fn pid(&self) -> u32;
}
impl std::io::Read for PtyReader { /* ... */ }
impl Drop for Pty {
    // TerminateProcess(child), ClosePseudoConsole, close handles — in that
    // order; ClosePseudoConsole unblocks any reader stuck in ReadFile.
}
```

**ConPTY exit protocol (important):** ConPTY's output pipe does NOT reliably
EOF when the child exits; a waiter thread per pane does
`WaitForSingleObject(process_handle)` and sends `Event::Exited(pane_id)`. The
app then marks the pane dead and drops the `Pty`, which closes the
pseudoconsole and unblocks the reader thread.

## `host` — host terminal control

**Amendment (sub-project 4, Task 5 — mouse):** the shared restore sequence
(`apply_restore`, run by both `Drop for Host` and the panic hook) now writes
`CSI ?1000l ?1002l ?1006l` (disable xterm mouse reporting: normal tracking +
button-motion + SGR extended coordinates) UNCONDITIONALLY, ahead of the
pre-existing `CSI ?1049l ?25h 0m` (leave alt screen, show cursor, reset SGR)
— i.e. every exit path (normal exit, error, panic) now ALSO disables mouse
reporting on the real terminal, regardless of whether the server ever told
this client to enable it. Terminal-restore invariant (extended): a
crashed/killed server, or a bug that forgot to send the `l` sequences before
a client detached, can never leave the user's real terminal with mouse
reporting stuck on — writing the disable sequences to a terminal that never
had them enabled is a harmless no-op (per the SGR mouse protocol, an `l` for
a mode that was never `h`'d does nothing). No signature change; `Host::enter`/
`Host::write`/`Drop`'s documented byte sequence is otherwise unchanged. See
the `## mouse` section of
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md)
for the enable-sequence (server) side of this feature.

**Amendment (sub-project 2, Task 8):**

- `Host::enter()`'s internal ordering changed (follow-up #3): every value
  needed to restore the console (code pages via `Get*`, console modes via
  `GetConsoleMode`) is now gathered FIRST, the `RESTORE` snapshot is
  published, and only THEN do the `Set*` mutations (UTF-8 code pages, VT
  processing / raw stdin mode) run. Previously the code pages were mutated
  before `RESTORE` was published, so a panic between those two steps would
  have left the panic hook/`Drop` restoring a snapshot that didn't yet
  reflect the just-mutated code pages. Observable behavior of `enter()`
  (final modes, alt-screen entry) is unchanged; only the failure-window
  ordering is tightened.
- New free function `pub fn console_size() -> std::io::Result<(u16, u16)>`:
  queries `GetConsoleScreenBufferInfo` against `STD_OUTPUT_HANDLE` directly
  (shares its `srWindow`-based calculation with `Host::size` via a private
  `query_size` helper), without constructing a `Host` — no mode changes, no
  alt-screen entry. Used by `main.rs` to size the initial `Attach` frame
  (and as an 80x24 fallback source) before deciding whether to become an
  attached client at all.

```rust
pub struct Host { /* private: saved stdin/stdout modes */ }

impl Host {
    /// Save current console modes, then: stdout += ENABLE_VIRTUAL_TERMINAL_PROCESSING
    /// | DISABLE_NEWLINE_AUTO_RETURN; stdin = ENABLE_VIRTUAL_TERMINAL_INPUT
    /// | ENABLE_EXTENDED_FLAGS (raw: LINE/ECHO/PROCESSED/QUICK_EDIT off, so
    /// Ctrl-C arrives as byte 0x03). Then write alt-screen enter
    /// (CSI ?1049h) + clear.
    pub fn enter() -> std::io::Result<Host>;
    pub fn size(&self) -> std::io::Result<(u16, u16)>;  // (cols, rows)
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>; // write + flush stdout
}
impl Drop for Host {
    // Disable xterm mouse reporting (CSI ?1000l ?1002l ?1006l — Task 5,
    // sub-project 4, unconditional), leave alt screen (CSI ?1049l), show
    // cursor (CSI ?25h), reset SGR, restore saved console modes. Must be
    // infallible (ignore errors).
}

/// Install a panic hook that performs the same restoration as Drop before
/// delegating to the previous hook. Safe to call once from main().
pub fn install_panic_hook();

/// Blocking read of raw bytes from the console input handle (for the stdin
/// thread). Returns Ok(0) only on handle closure.
pub fn read_stdin(buf: &mut [u8]) -> std::io::Result<usize>;
```

Host resize detection: no event in the byte stream — the app polls
`host.size()` on its tick (see below) and compares.

## `app` — event loop

**Superseded (sub-project 2, Task 8):** `src/app.rs` and its `pub mod app;`
declaration are DELETED. The single-process event loop described below was
replaced wholesale by the server/client split — `src/server.rs` owns the
loop shape server-side (headless, multi-session/window) and `src/client.rs`
+ `src/cli.rs` + `src/main.rs` are the thin client/CLI side. See
[`2026-07-07-server-client-interfaces.md`](2026-07-07-server-client-interfaces.md)'s
`## server`, `## cli`, and `## client` sections. This section is kept only
as a historical record of the MVP's shape.

```rust
pub enum Event {
    Output(crate::layout::PaneId, Vec<u8>), // reader thread
    Exited(crate::layout::PaneId),          // waiter thread
    Stdin(Vec<u8>),                         // stdin thread
}

pub fn run() -> Result<(), Box<dyn std::error::Error>>;
```

- One `std::sync::mpsc::channel::<Event>()`; reader/waiter threads per pane +
  one stdin thread all hold clones of the Sender.
- Main loop: `recv_timeout(Duration::from_millis(50))`. On timeout ("tick"):
  poll `host.size()` (resize → recompute layout, `pty.resize` + `grid.resize`
  each pane, `renderer.resize`), refresh the status clock string (re-render
  only if it changed).
- Shell command: `"powershell.exe -NoLogo"`.
- Pane area = full host size minus the bottom status row:
  `Rect { x: 0, y: 0, w: cols, h: rows - 1 }`.
- Status left: `"[winmux] 0:powershell*"`. Status right: local time
  `"HH:MM DD-Mon-YY"` (e.g. `"21:04 06-Jul-26"`).
- Confirm prompt message: `format!("kill-pane {}? (y/n)", pane_id)`.
- Terminal too small (area cannot fit the current tree at minimums): render
  `message: Some("terminal too small".into())` and blank panes; keep running.
- Dead pane: marked dead, still rendered with `[exited]` overlay; `x` (confirm)
  closes it. When the LAST pane dies or is closed, `run` returns Ok → clean exit.
- Zoom, split, close, resize all trigger `pty.resize` + `grid.resize` for every
  pane whose rect changed, then a render.
