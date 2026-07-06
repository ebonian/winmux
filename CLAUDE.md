# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

winmux is a native tmux alternative for Windows, written in Rust on top of ConPTY. It runs *inside* the user's terminal (like tmux does) and draws panes/borders/status bar with VT escape sequences — it is not its own GUI window. Guiding principle: **be exactly like tmux** — wherever a design choice exists, match tmux's real defaults so tmux users are immediately at home.

Current state: sub-project 1 of 4 (Multiplexing MVP) is complete — one session, one window, multiple PowerShell panes, split/focus/resize/zoom/close, status bar. The roadmap (server/client split + detach, command layer + `.tmux.conf`, parity polish) is in `docs/overview.md`. Deferred known issues live in `docs/follow-ups.md`.

## Commands

`cargo` may not be on PATH in fresh shells — it lives at `C:\Users\poon\.cargo\bin`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"        # Bash
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"   # PowerShell
```

```bash
cargo build                                  # debug build
cargo test                                   # everything: unit + integration + e2e
cargo test layout::                          # one module's unit tests (also grid::, render::, input::)
cargo test --test pty_smoke                  # ConPTY integration tests (spawn real cmd.exe)
cargo test --test e2e                        # full e2e: drives the built winmux.exe under a ConPTY
cargo clippy --all-targets -- -D warnings    # lint gate — must stay clean
cargo build --release                        # binary at target\release\winmux.exe
cargo test host::tests -- --ignored          # manual host smoke test (needs a real console)
```

Windows-only: the integration/e2e tests spawn real processes through ConPTY and require Win10 1809+. A running `winmux.exe` locks `target\release\winmux.exe` — exit it before rebuilding release.

## Architecture

Core insight: **every pane is its own tiny terminal emulator.** ConPTY hands us the raw VT output stream of a shell; `grid` parses it into a cell matrix; `render` stitches all pane grids + borders + status bar into one frame diffed against the previous one; only changed cells are written to the host terminal.

Data flow (all state owned by the main thread — no locks on core state):

```
pane shell → ConPTY (pty) → reader thread ─┐
child exit → waiter thread ────────────────┼→ mpsc<Event> → app main loop → grid.feed / layout ops → render.compose → host.write
user keys  → stdin thread (host) ──────────┘                    ↑ input prefix-key state machine decides forward-to-pane vs action
```

Modules (`src/`): `geom` (Rect/Direction) · `layout` (binary split tree; pure) · `grid` (vte-driven emulator per pane) · `render` (double-buffered cell-diff compositor; pure) · `input` (Ctrl-b prefix state machine; pure) · `pty` (ConPTY wrapper) · `host` (raw mode, alt screen, restoration) · `app` (event loop wiring; the only untested-by-design module — the e2e test covers it).

The crate is **lib+bin** (`src/lib.rs` declares all modules) specifically so integration tests can `use winmux::pty` / `winmux::grid` — the e2e harness spawns the built `winmux.exe` inside a ConPTY via the project's own `pty` module and asserts on screen content by feeding the output through the project's own `grid` emulator.

### The locked interface contract

Public APIs of every module are governed by `docs/specs/2026-07-06-mvp-interfaces.md`. Do not add public surface or change signatures ad hoc: if a signature must change, update the contract file and every consumer in the same commit. Private helpers are unconstrained.

### Non-negotiable invariant: never wreck the user's terminal

Every exit path (normal exit, error, panic) must restore console modes, code pages, alt-screen, cursor, and SGR. This is implemented as an idempotent pair in `src/host.rs`: `Host`'s `Drop` plus a global panic hook, both reading a shared `RESTORE` snapshot (poison-recovering mutex). If you touch anything `Host::enter` mutates, you must also restore it in `apply_restore`.

## Hard-won platform gotchas (do not regress)

- **ConPTY + redirected stdio** (`src/pty.rs`): when the *parent's* std handles are not a console (e.g. under `cargo test`), `CreateProcessW` leaks them into the child past the pseudoconsole, so output bypasses the pipe. Fix is `STARTF_USESTDHANDLES` with null handles — applied *only* when `parent_stdio_is_redirected()`; setting it unconditionally is unnecessary interactively.
- **Console code pages** (`src/host.rs`): output must be forced to CP_UTF8 or box-drawing borders mangle under OEM code pages — and the original code pages must be saved and restored on exit.
- **PSModulePath** (`src/main.rs`): PowerShell 7 exports a `PSModulePath` that makes `powershell.exe` 5.1 panes resolve PSReadLine to PS7's script module, which execution policy blocks ("Cannot load PSReadline module"). winmux clears the variable at startup; panes rebuild their own default.
- **ConPTY exit protocol** (`src/pty.rs` / `src/app.rs`): the output pipe does not reliably EOF when the child exits. A waiter thread per pane does `WaitForSingleObject` on the process handle and sends `Event::Exited`; dropping the `Pty` (TerminateProcess → ClosePseudoConsole → CloseHandle, in that order) is what unblocks a reader stuck in `ReadFile`.
- **Zero-size geometry**: `layout::rects()` may return zero-size rects when the terminal is tiny; `grid` clamps to 1x1; every consumer must tolerate w==0/h==0 rects rather than assume drawability.

## Testing conventions

Pure modules (`layout`, `grid`, `render`, `input`) are unit-tested TDD-style with exact expected values computed in comments (grid tests feed literal VT byte strings like `b"\x1b[2J"` and assert resulting cells/cursor; render tests assert exact emitted VT byte strings with 1-based CUP coordinates). I/O edges (`pty`, `host`) get integration smoke tests against real Windows handles. Keep `cargo clippy --all-targets -- -D warnings` green — it is the project's lint gate.

## Keybindings (tmux defaults, hardcoded for MVP)

Prefix `Ctrl-b`, then: `%` split left/right · `"` split top/bottom · arrows focus · `o` cycle · `;` last pane · `x` kill (y/n confirm) · `z` zoom · `Ctrl-arrow` resize (repeatable, 500ms window) · `Ctrl-b` again sends literal Ctrl-b. The binding table lives in `src/input.rs` and is deliberately swappable for the future config-driven version (sub-project 3).
