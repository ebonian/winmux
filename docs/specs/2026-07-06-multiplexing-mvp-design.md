# Sub-project 1 — Multiplexing MVP — Design

**Status:** Ready for review.

**Parent:** [`../overview.md`](../overview.md)

## Guiding principle

Be **exactly like tmux**. The project's reason for existing is that tmux users
cannot run tmux on Windows; winmux gives them tmux behavior natively. Wherever a
design choice exists, match tmux's real defaults and behavior so an existing tmux
user is immediately at home.

## Scope

Delivers a single session, single window, with multiple panes running PowerShell.
The user can split, switch focus, resize, and close panes, with borders and a
status bar drawn into their existing terminal.

**In scope:** one session, one window, multiple panes; ConPTY-hosted PowerShell;
VT parsing per pane; split-tree layout; compositing borders + status bar; prefix
key handling; split / switch / resize / close / zoom.

**Out of scope (later sub-projects):** detach/attach, multiple sessions, multiple
windows/tabs, `.tmux.conf` parsing, copy mode, mouse, scrollback history.

## Section 1: Architecture & modules

**Core insight:** every pane is its own tiny terminal emulator. ConPTY hands us
the raw VT output stream from a shell; we parse that into a grid of cells
(character + color/attributes); the compositor stitches all pane grids together
with borders and a status bar into one frame written to the host terminal.

### Modules

| Module | Responsibility | Depends on |
|---|---|---|
| `host` | Control host terminal: raw mode, alt-screen, read stdin bytes, query size, write frames, **restore on exit**. (Win32 console via `windows-rs`.) | — |
| `pty` | ConPTY wrapper: create pseudoconsole, spawn shell, read/write pipes, resize. | `windows-rs` |
| `grid` | A pane's terminal-emulator state: feed VT bytes (via `vte`), maintain cell grid + cursor + attributes. | `vte` |
| `layout` | Split tree (H/V splits, ratios, leaves = panes). Split, focus-navigate, resize, close, compute rects. **Pure logic, no I/O.** | — |
| `render` | Compose layout rects + pane grids + borders + status bar into a frame; diff against previous frame; emit only changed cells. | `grid`, `layout` |
| `input` | Prefix-key state machine; map keys to layout actions or forward to focused pane. | `layout` |
| `app` | Event loop wiring it all together. | all |

### Concurrency model

Threads + channels (not async). One reader thread per pane's ConPTY output and
one thread for host stdin, each sending messages into a single `mpsc` channel.
The main thread owns all state (layout, grids) and is the only thing that mutates
and renders — so no locks on core state. Simpler and less error-prone with raw
Windows handles than tokio, and fast enough.

### Data flow

- Pane output → reader thread → channel → main loop feeds bytes to that pane's
  `grid` → marks dirty → render.
- Keystroke → stdin thread → channel → main loop → prefix state machine → either
  write to focused pane's ConPTY input, or run a layout command → render.
- Resize event → main loop → recompute layout → resize each ConPTY + grid → render.

## Section 2: Layout model & UX / keybindings

**Layout is a binary split tree** (tmux's model). Internal nodes are splits
(horizontal or vertical), each with two children and a ratio (e.g. `0.5`). Leaves
are panes. Given the window's width/height, the tree computes each pane's rect
recursively. Arbitrary nested layouts fall out naturally; resize is "adjust the
ratio on one node." **Focus** is a pointer to the current leaf; navigation,
splitting, and closing operate relative to it.

**Keybindings — tmux exact defaults.** Hardcoded for the MVP, but `input` holds
them in a replaceable binding table so the config-driven version (sub-project 3)
swaps the table without touching the state machine.

| Key (after prefix) | Action |
|---|---|
| `Ctrl-b` | **Prefix** — all commands start here |
| `%` | Split focused pane **vertically** (left/right) |
| `"` | Split focused pane **horizontally** (top/bottom) |
| `←` `↑` `↓` `→` | Move focus to the adjacent pane in that direction |
| `o` | Cycle focus to the next pane |
| `;` | Toggle to the last-focused pane |
| `x` | Close focused pane (with a confirm prompt, as tmux does) |
| `z` | Toggle zoom (focused pane fills the window; toggle to restore) |
| `Ctrl-<arrow>` | Resize focused pane's split (repeatable, as tmux does) |
| `Ctrl-b` (again) | Send a literal `Ctrl-b` to the focused pane |

**Prefix state machine:** normal keystrokes forward straight to the focused
pane's ConPTY input. The prefix arms a "waiting for command" state; the next key
is interpreted as a command and consumed. Repeatable commands (resize) keep the
state armed briefly (tmux's `repeat-time`). Unrecognized command keys disarm.

**Minimum size:** panes have a floor (border + a small usable area). A split that
would violate the floor is refused rather than producing broken geometry.

## Section 3: Rendering & compositing

Double-buffered, cell-diffed rendering — flicker-free, like tmux:

- Maintain a **back buffer** (desired frame) and **front buffer** (on screen),
  both grids of styled cells sized to the terminal.
- Each frame, compose into the back buffer: for each pane, copy its `grid` cells
  into the pane's rect; draw split borders (single-line box-drawing chars, active
  pane's border highlighted, matching tmux); draw the status bar on the bottom
  row (session name, window list, clock — tmux's default layout).
- **Diff** back vs. front and emit VT only for changed cells (cursor-move + SGR +
  char); then swap buffers. Output is bounded to what actually changed.
- Position the **real terminal cursor** at the focused pane's cursor location and
  mirror its shown/hidden state, so the focused shell behaves normally.
- On host resize: recompute all pane rects from the tree, resize each ConPTY +
  grid, force a full repaint.

## Section 4: Error handling & terminal restoration

Non-negotiable: **never leave the user's terminal wrecked.** Raw mode,
alt-screen, and cursor visibility are torn down on *every* exit path.

- A `host` restoration guard (RAII `Drop`) plus a **panic hook** that restores
  console mode, leaves the alt-screen, and shows the cursor before the process
  dies. Same on `Ctrl-C` and normal exit.
- **Pane process exits:** mark the pane dead, show `[exited]` in it; when the last
  pane exits, winmux exits cleanly (single window/session behavior).
- **ConPTY / spawn failure:** surface a clear error and exit without corrupting
  the terminal.
- **Terminal too small:** clamp/refuse splits below the minimum; if the whole
  window is below the minimum, show a "terminal too small" message (as tmux does).

## Section 5: Testing strategy

Pure-logic modules are unit-tested TDD-style; I/O edges get thin wrappers plus
integration smoke tests.

- **`layout`** (pure): unit-test split / focus / resize / close geometry and rect
  computation exhaustively.
- **`grid`**: feed known VT byte sequences, assert resulting cell state (chars,
  colors, cursor) — the terminal-emulator correctness net.
- **`render`**: compose known grids + layout → assert diff output bytes.
- **`input`**: drive the prefix state machine with key sequences → assert emitted
  actions.
- **`pty` / `host`**: thin wrappers, hard to unit-test (real Windows handles);
  covered by an integration smoke test that spawns a real shell (e.g.
  `cmd /c echo`) through ConPTY and asserts output flows.

## Open items deferred to later sub-projects

Detach/attach, server/client split, multiple sessions and windows, `.tmux.conf`
parsing, the command dispatcher and `winmux <cmd>` CLI, copy mode, mouse,
scrollback, status-bar format strings.
