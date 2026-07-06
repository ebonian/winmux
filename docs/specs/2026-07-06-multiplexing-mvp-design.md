# Sub-project 1 — Multiplexing MVP — Design

**Status:** In progress (brainstorming). Section 1 approved; remaining sections
being finalized in dialogue before this spec is locked and reviewed.

**Parent:** [`../overview.md`](../overview.md)

## Scope

Delivers a single session, single window, with multiple panes running PowerShell.
The user can split, switch focus, resize, and close panes, with borders and a
status bar drawn into their existing terminal.

**In scope:** one session, one window, multiple panes; ConPTY-hosted PowerShell;
VT parsing per pane; split-tree layout; compositing borders + status bar; prefix
key handling; split / switch / resize / close.

**Out of scope (later sub-projects):** detach/attach, multiple sessions, multiple
windows/tabs, `.tmux.conf` parsing, copy mode, mouse, scrollback history.

## Section 1: Architecture & modules — APPROVED

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

## Section 2: Layout model & UX / keybindings — TBD (in dialogue)

## Section 3: Rendering & compositing — TBD (in dialogue)

## Section 4: Error handling & terminal restoration — TBD (in dialogue)

## Section 5: Testing strategy & scope boundaries — TBD (in dialogue)
