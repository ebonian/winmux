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

    /// Geometric navigation: move focus to the pane adjacent in `dir`
    /// (the pane whose rect borders the focused rect in that direction,
    /// picking the one overlapping the focused pane's cursor axis midpoint).
    /// Returns false (no change) if there is no pane in that direction.
    pub fn focus_dir(&mut self, dir: Direction, area: Rect) -> bool;

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

    /// Toggle zoom on the focused pane. Zoom auto-clears on split/remove.
    pub fn toggle_zoom(&mut self);
    pub fn is_zoomed(&self) -> bool;

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

pub struct Scene<'a> {
    /// Host terminal size (cols, rows). The status bar is the bottom row;
    /// panes live in rows 0..rows-1.
    pub size: (u16, u16),
    pub panes: Vec<PaneView<'a>>,
    pub zoomed: bool,
    pub status_left: String,   // e.g. "[winmux] 0:powershell*"
    pub status_right: String,  // e.g. "21:04 06-Jul-26"
    /// When Some, the status row shows this message instead (confirm prompt,
    /// "terminal too small"), styled bg yellow(SGR 43) fg black(30) like tmux
    /// message-style.
    pub message: Option<String>,
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

**Compositing rules:**
- Pane cells copy from `grid.cell(...)` into the pane's rect; cells outside any
  pane rect (borders) are drawn with box chars `─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼`
  (junction-aware). Active pane's border: fg green (SGR 32); others fg default
  (tmux default `pane-active-border-style fg=green`). When zoomed, no borders.
- Dead pane: its grid still renders; the string `[exited]` is overlaid in
  reverse video at the pane rect's top-left.
- Status bar (bottom row): bg green (42) fg black (30), tmux default.
  `status_left` at col 0, `status_right` right-aligned; middle padded with
  spaces; truncate right-first if too narrow.
- Diff emission: for each changed cell, emit minimal CUP (skip if the cursor is
  already adjacent from the previous emitted cell) + SGR (only on style change)
  + the char. UTF-8 encode chars. Reset SGR (CSI 0m) at stream end.

## `input` — prefix state machine (pure)

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
    // Leave alt screen (CSI ?1049l), show cursor (CSI ?25h), reset SGR,
    // restore saved console modes. Must be infallible (ignore errors).
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
