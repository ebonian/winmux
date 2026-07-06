# winmux

A [tmux](https://github.com/tmux/tmux)-style terminal multiplexer for Windows,
written in Rust. tmux does not run natively on Windows; winmux gives Windows
users tmux behavior — splits, focus, resize, zoom, close, a status bar — in
their existing terminal, matching tmux's real defaults so tmux users are
immediately at home.

## Status

**Multiplexing MVP.** Single session, single window, multiple PowerShell panes
hosted via ConPTY, each its own VT emulator, composited with borders and a
status bar into the host terminal.

Not yet implemented (planned for later sub-projects): detach/attach, multiple
sessions/windows, `.tmux.conf`, copy mode, mouse, scrollback.

## Requirements

- Windows 10/11 with a ConPTY-capable terminal (Windows Terminal recommended).
- Rust (edition 2021) toolchain.

## Build

```
cargo build --release
```

The binary is produced at `target/release/winmux.exe`.

## Run

Launch it from Windows Terminal:

```
winmux
```

You get one PowerShell pane. Use the keybindings below to split and manage
panes. When the last pane's shell exits, winmux exits and restores your
terminal.

## Keybindings

All commands start with the prefix `Ctrl-b`, exactly like tmux.

| Key (after prefix) | Action |
|---|---|
| `Ctrl-b` | **Prefix** — all commands start here |
| `%` | Split focused pane **vertically** (left/right) |
| `"` | Split focused pane **horizontally** (top/bottom) |
| `←` `↑` `↓` `→` | Move focus to the adjacent pane in that direction |
| `o` | Cycle focus to the next pane |
| `;` | Toggle to the last-focused pane |
| `x` | Close focused pane (with a `y`/`n` confirm prompt) |
| `z` | Toggle zoom (focused pane fills the window; toggle to restore) |
| `Ctrl-<arrow>` | Resize the focused pane's split (repeatable) |
| `Ctrl-b` (again) | Send a literal `Ctrl-b` to the focused pane |

## Documentation

- [Multiplexing MVP — Design](docs/specs/2026-07-06-multiplexing-mvp-design.md)
- [Multiplexing MVP — Locked Interface Contract](docs/specs/2026-07-06-mvp-interfaces.md)
