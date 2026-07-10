# winmux

[![CI](https://github.com/ebonian/winmux/actions/workflows/ci.yml/badge.svg)](https://github.com/ebonian/winmux/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/ebonian/winmux)](https://github.com/ebonian/winmux/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A native [tmux](https://github.com/tmux/tmux)-style terminal multiplexer for
Windows, written in Rust on top of ConPTY. tmux does not run natively on
Windows; winmux runs *inside* your existing terminal (like tmux does) and
gives you real tmux behavior — sessions that survive disconnects, windows,
panes, copy mode, mouse support, and `.tmux.conf` compatibility.

Guiding principle: **be exactly like tmux.** Wherever a design choice exists,
winmux matches tmux's real defaults, so tmux users are immediately at home.

## Install

Run this in PowerShell (no admin rights needed):

```powershell
irm https://raw.githubusercontent.com/ebonian/winmux/main/install.ps1 | iex
```

This downloads the latest release, verifies its checksum, installs to
`%LOCALAPPDATA%\Programs\winmux`, and adds it to your user PATH. Then:

```powershell
winmux
```

- **Update:** `winmux-update` (or re-run the install one-liner).
- **Uninstall:** `winmux kill-server; Remove-Item -Recurse "$env:LOCALAPPDATA\Programs\winmux"`,
  then remove the directory from your user PATH if you like.

Requirements: Windows 10 1809+ with a ConPTY-capable terminal
(Windows Terminal recommended).

## Features

- **Sessions, windows, panes** — a background server owns all state; clients
  attach, detach (`prefix d`), and reattach — your shells survive closing the
  terminal or an SSH disconnect. `winmux ls`, `winmux attach -t work`,
  `winmux kill-session`, and the rest of the tmux CLI subset work as you'd
  expect.
- **`.tmux.conf` compatibility** — winmux loads `~/.tmux.conf` at server
  startup (then `~/.winmux.conf` for overrides). `set-option`, `bind-key`,
  `unbind-key`, `send-keys`, `source-file`, per-session/per-window option
  scopes, `@`-user options, and a large option set all work — real-world tmux
  configs load clean.
- **Command layer** — one dispatcher powers keybindings, the `winmux <cmd>`
  CLI, the `prefix :` command prompt, and config files. Every binding
  (including the prefix itself) is rebindable.
- **Copy mode & paste buffers** — `prefix [`, emacs and vi key tables,
  scrollback history, search (`/`, `?`, `C-s`, `C-r`), linear and rectangle
  selection, named paste buffers, `copy-pipe`, and OSC 52 clipboard
  integration (`set-clipboard`).
- **Mouse** — `set -g mouse on`: click to focus, drag borders to resize,
  wheel to scroll into copy mode, drag to select, double/triple-click word and
  line selection, status-bar clicks, right-click context menus
  (`display-menu`), and application mouse passthrough. Mouse events are
  rebindable table entries (`bind -T root MouseDown1Pane ...`), like tmux.
- **Layouts & window ops** — the five tmux layout presets, `swap-pane` and
  `swap-window` (including cross-window/session), `rotate-window`,
  `break-pane`, `move-window`, `find-window` with regex.
- **Status line & formats** — a real tmux format engine (`#{...}` variables,
  conditionals, comparisons), `status-left`/`status-right`,
  `window-status-format`, styles, `status-justify`, alerts
  (bell/activity/silence monitoring with window flags).
- **Overlays** — `choose-tree` (`prefix w` / `prefix s`) with live preview,
  tagging, sorting, and filtering; `choose-buffer`, `choose-client`,
  `display-panes`, `clock-mode`.

Known, documented divergences from real tmux live in
[docs/follow-ups.md](docs/follow-ups.md).

## Quick reference

The prefix is `Ctrl-b`, exactly like tmux. A small sample:

| Key (after prefix) | Action |
|---|---|
| `%` / `"` | Split pane left/right / top/bottom |
| arrows | Move focus between panes |
| `z` | Toggle pane zoom |
| `x` | Kill pane (confirm) |
| `c` / `n` / `p` / `0`-`9` | New / next / previous / select window |
| `d` | Detach (session keeps running) |
| `[` | Enter copy mode (then `/` to search, Space/Enter to select/copy in vi mode) |
| `]` | Paste |
| `w` / `s` | choose-tree: this session's windows / all sessions |
| `:` | Command prompt (`rename-window foo`, `set -g mouse on`, ...) |

Every default tmux binding you'd reach for is there — see the
[docs](docs/overview.md) for the full tables, or `prefix :` then `list-keys`.

## Building from source

```powershell
cargo build --release    # binary at target\release\winmux.exe
cargo test               # unit + integration + e2e (Windows only: real ConPTY + named pipes)
cargo clippy --all-targets -- -D warnings
```

Note: a running winmux server locks `target\release\winmux.exe` — run
`winmux kill-server` before rebuilding release.

## Documentation

- [Changelog](CHANGELOG.md)
- [Project overview and roadmap](docs/overview.md)
- [Known issues / follow-ups](docs/follow-ups.md)
- Interface contracts and design specs: [docs/specs/](docs/specs/)

## License

[MIT](LICENSE)
