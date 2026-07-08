# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

winmux is a native tmux alternative for Windows, written in Rust on top of ConPTY. It runs *inside* the user's terminal (like tmux does) and draws panes/borders/status bar with VT escape sequences — it is not its own GUI window. Guiding principle: **be exactly like tmux** — wherever a design choice exists, match tmux's real defaults so tmux users are immediately at home.

Current state: all four sub-projects are complete — **the planned project scope is done.**

- **Sub-project 1 (Multiplexing MVP):** one session, one window, multiple PowerShell panes, split/focus/resize/zoom/close, status bar.
- **Sub-project 2 (Server/client split):** a background server (daemonized, named-pipe transport) now owns all sessions/windows/panes; a thin client attaches, detaches, and reattaches — including surviving an SSH disconnect. Multiple sessions and windows are supported, along with a tmux CLI subset: `new-session`/`new`, `attach-session`/`attach`/`a`, `detach-client`, `list-sessions`/`ls`, `list-windows`/`lsw`, `has-session`/`has`, `kill-session`, `kill-server`, `rename-session`, `rename-window`, plus global `-L <socket-name>`. Bare `winmux` is `new-session`.
- **Sub-project 3 (Command layer + config compatibility):** one command dispatcher now powers all four entry points — keybindings, the `winmux <cmd>` CLI (any command from the CLI subset table, not just the SP2 handful), the `prefix-:` status-line command prompt, and real `.tmux.conf` config files. `set-option`/`set`, `bind-key`/`bind`, `unbind-key`, `send-keys`, `send-prefix`, `display-message`, `confirm-before`, `source-file`, `list-keys`, `show-options` all work at runtime and from a config file. Config loading: `-f <file>` (server-startup-only, tmux semantics — a `-f` against an already-running server is ignored), `-f -` disables config entirely, and with no `-f` the default discovery chain is `%USERPROFILE%\.tmux.conf` (or `$XDG_CONFIG_HOME/tmux/tmux.conf` if set) loaded first, then `%USERPROFILE%\.winmux.conf` second (so winmux-only tweaks can override a ported tmux config). Keybindings are table-driven (`src/bindings.rs`) and rebindable at runtime or via config; the prefix key itself is a `set -g prefix <key>` option, not hardcoded. Option-driven styling covers status-bar style/position/on-off, pane border style, message style, and a `status-left`/`status-right` format-string subset (`#S`/`#W`/`#I`/`#P`/`#F`/`#H`/strftime-style codes).
- **Sub-project 4 (Parity polish):** grid v2 (scrollback history, real alternate screen, OSC 0/2 title capture); copy mode (`prefix-[`, emacs + vi key tables, movement/scroll/search, the `[scroll/history]` indicator); selection + paste buffers (linear and rectangle selection, `paste-buffer`/`set-buffer`/`list-buffers`/`delete-buffer`, automatic + named buffers, `buffer-limit`); copy-mode search (`/`/`?`/`C-s`/`C-r`, case-insensitive, wraps); mouse (`set -g mouse on`: click-to-focus, border-drag resize, wheel-to-scroll/copy-mode entry, drag-to-select + click-release copy, double/triple-click word/line select, status-row click/wheel); five layout presets + `swap-pane`/`rotate-window`; window ops (`break-pane`, `move-window`, `find-window`, `'` index prompt); `choose-tree` (`w`/`s`) and `display-panes` (`q`) overlays; `escape-time`-governed Escape-vs-Alt disambiguation; `automatic-rename` from ConPTY OSC titles. Documented, non-blocking divergences from real tmux for all of the above live in `docs/follow-ups.md`.

The roadmap lives in `docs/overview.md` (see its "PROJECT COMPLETE" section). Known/deferred issues live in `docs/follow-ups.md`. A researched-but-not-started further sub-project (TPM-style plugin support) is documented at `docs/superpowers/plans/2026-07-08-tpm-plugin-support.md` but is future work, not part of the delivered scope.

## Commands

`cargo` may not be on PATH in fresh shells — it lives at `C:\Users\poon\.cargo\bin`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"        # Bash
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"   # PowerShell
```

```bash
cargo build                                  # debug build
cargo test                                   # everything: unit + integration + e2e
cargo test layout::                          # one module's unit tests (also grid::, render::, input::, model::, status::, protocol::, pipe::)
cargo test --test pty_smoke                  # ConPTY integration tests (spawn real cmd.exe)
cargo test --test pipe_smoke                 # named-pipe transport integration tests
cargo test --test server_proto               # headless server protocol tests — no console, no ConPTY host; speaks raw frames over the pipe
cargo test --test e2e                        # sub-project 1 style e2e: drives winmux.exe under a ConPTY (single session)
cargo test --test e2e_sessions               # sub-project 2 e2e: detach/reattach, windows, kill-session over real named pipes
cargo test --test e2e_config                 # sub-project 3 e2e: .tmux.conf round trip via -f, prefix-: command prompt, send-keys CLI
cargo test --test e2e_copy_mouse             # sub-project 4 e2e: copy-mode roundtrip (scroll into history, select, copy, paste), mouse click-focus, mouse-wheel scroll into copy mode
cargo test -- --test-threads=1               # serialize when diagnosing e2e flakiness (each e2e test uses a unique -L socket, so parallel `cargo test` is safe by construction, but real ConPTY/process load under full parallelism can still make timing assertions flaky)
cargo clippy --all-targets -- -D warnings    # lint gate — must stay clean
cargo build --release                        # binary at target\release\winmux.exe
cargo test host::tests -- --ignored          # manual host smoke test (needs a real console)
```

Windows-only: the integration/e2e tests spawn real processes through ConPTY and bind real named pipes; require Win10 1809+. A running `winmux.exe` **or a running background server** locks `target\release\winmux.exe` — exit any attached client and run `winmux -L <socket> kill-server` (or `target\release\winmux.exe kill-server` if not on PATH) before rebuilding release.

`tests/server_proto.rs` (130 tests, headless protocol-level) is fast but spins up a real named-pipe server per test; under `cargo test`'s default full-parallelism it has occasionally shown timing flakiness on a loaded machine (each test still uses its own isolated server/socket, so failures are timing, not cross-test interference). If it flakes, retry with a capped thread count rather than full serialization: `cargo test --test server_proto -- --test-threads=4`.

## Architecture

Core insight: **every pane is its own tiny terminal emulator.** ConPTY hands us the raw VT output stream of a shell; `grid` parses it into a cell matrix; `render` stitches all pane grids + borders + status bar into one frame per client, diffed against that client's previous frame; only changed cells are written out.

One binary, `winmux.exe`, plays three roles selected by argv (`src/cli.rs` parses this):

| Invocation | Role |
|---|---|
| `winmux [flags] [command ...]` | CLI: parse args, connect to the pipe (auto-starting the server only for commands that create state, e.g. `new-session`), then either run a one-shot control command or become an attached client. |
| *(same binary, becomes the attached client)* | Client: thin — forwards raw stdin, writes server-composed VT bytes to stdout verbatim, draws nothing itself. |
| `winmux __server --pipe <full-pipe-name>` | Hidden: the server event loop. Spawned detached (`DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP`) so it survives the launching console closing. |

Server-side data flow (all state owned by the server's main thread — no locks on core state):

```
pane reader/waiter threads ─┐
client pipe reader threads ─┼→ mpsc<ServerEvent> → server main loop (owns ALL state) → per-client writer thread ← mpsc<Vec<u8>>
accept thread ──────────────┘
```

The main loop owns the session/window `Registry`, every pane's `Pty` + `Grid` (dropped from the map once the pane exits), and every attached client's `Renderer` + `InputMachine` + prompt/confirm state. Each client has its own prefix-key state machine, so multiple clients can be mid-prefix independently. A `recv_timeout(50ms)` tick refreshes the clock and coalesces events (see follow-up #4); resize is client-pushed (`Resize` frames), not polled.

Modules (`src/`): `geom` (Rect/Direction) · `layout` (binary split tree; pure — sub-project 4 adds `apply_preset`/`swap_panes`/`rotate` for the five layout presets) · `grid` (vte-driven emulator per pane; sub-project 4 adds scrollback history, real alternate-screen save/restore, OSC 0/2 title capture, and the `view_cell`/`view_row_text` scrolled-read API copy mode reads from) · `render` (double-buffered cell-diff compositor; pure — sub-project 4 adds `CopyView`/selection-highlight/position-indicator painting and the `Overlay` (choose-tree list / display-panes digits) compositing pass) · `input` (table-driven `KeyMachine`: prefix/capture-mode state machine over `Key`/`Mouse` events, resolved through the mutable `Bindings` table; pure) · `status` (status-bar span builder; pure) · `model` (session/window registry, tmux naming/selection semantics; pure) · `protocol` (client↔server frame codec) · `pipe` (named-pipe listener/connection transport) · `pty` (ConPTY wrapper) · `host` (raw mode, alt screen, restoration; sub-project 4 adds unconditional mouse-mode-off escape sequences on exit) · `server` (the headless event loop — owns everything, the successor to the MVP's `app.rs`; its `dispatch` submodule is the one command executor shared by keybindings, the CLI, the `:` prompt, and config loading — sub-project 4 grows this submodule the most: copy-mode/selection/search actions, mouse hit-testing/routing, layout-preset/swap/rotate commands, window ops, and the choose-tree/display-panes overlays all live here) · `client` (thin attach client + detached-server autostart) · `cli` (pure argv parser for the tmux CLI subset) · `keys` (sub-project 3: tmux key notation ↔ `Key` ↔ VT input/output byte sequences; sub-project 4 adds SGR mouse-event decoding, `DecodedInput::Mouse`) · `style` (sub-project 3: tmux style-string grammar → `grid::Style`) · `cmd` (sub-project 3: config/CLI/prompt tokenizer + command table + typed `ParsedCmd`/`RawCmd`; sub-project 4 adds every copy-mode/buffer/layout/window-op/overlay command) · `options` (sub-project 3: typed global tmux option registry + the `expand_format` subset; sub-project 4 adds `mouse`, `mode-style`, `buffer-limit`, `main-pane-width`/`main-pane-height`, `display-panes-time`, `display-panes-colour`/`display-panes-active-colour`, and wires up previously-inert `history-limit`/`escape-time`/`automatic-rename`/`mode-keys` getters) · `bindings` (sub-project 3: the mutable key-binding table, `Bindings::default()` reproducing every prior hardcoded binding; sub-project 4 adds the `copy-mode`/`copy-mode-vi` tables plus every new default keybinding below) · `buffers` (sub-project 4: the server-global named/automatic paste-buffer store, `buffer-limit` eviction) · `logging` (sub-project 3: the headless server's panic-hook log sink, `%LOCALAPPDATA%\winmux\server.log`). `app.rs` (the MVP's single-process event loop) was deleted when `server`/`client` replaced it.

The crate is **lib+bin** (`src/lib.rs` declares all modules) specifically so integration tests can `use winmux::pty` / `winmux::grid` / etc. — the e2e harnesses spawn the built `winmux.exe` inside a test-owned ConPTY via the project's own `pty` module and assert on screen content by feeding the output through the project's own `grid` emulator; `tests/server_proto.rs` instead talks the raw `protocol` frames directly over a `pipe` connection, no console or ConPTY needed.

### The locked interface contract

Public APIs of every module are governed by three contract files: `docs/specs/2026-07-06-mvp-interfaces.md` (geom/layout/grid/render/pty/host), `docs/specs/2026-07-07-server-client-interfaces.md` (protocol/pipe/model/status/server/cli/client), and `docs/specs/2026-07-07-command-config-interfaces.md` (keys/style/cmd/options/bindings/server::dispatch). Do not add public surface or change signatures ad hoc: if a signature must change, update the relevant contract file and every consumer in the same commit. Private helpers are unconstrained.

### Non-negotiable invariant: never wreck the user's terminal

Every exit path (normal exit, error, panic) must restore console modes, code pages, alt-screen, cursor, and SGR. This is implemented as an idempotent pair in `src/host.rs`: `Host`'s `Drop` plus a global panic hook, both reading a shared `RESTORE` snapshot (poison-recovering mutex; published before the first mutation, follow-up #3). If you touch anything `Host::enter` mutates, you must also restore it in `apply_restore`. The server process is headless (no console to restore) and instead installs a panic hook that logs to `%LOCALAPPDATA%\winmux\server.log`.

## Hard-won platform gotchas (do not regress)

- **ConPTY + redirected stdio** (`src/pty.rs`): when the *parent's* std handles are not a console (e.g. under `cargo test`), `CreateProcessW` leaks them into the child past the pseudoconsole, so output bypasses the pipe. Fix is `STARTF_USESTDHANDLES` with null handles — applied *only* when `parent_stdio_is_redirected()`; setting it unconditionally is unnecessary interactively.
- **Console code pages** (`src/host.rs`): output must be forced to CP_UTF8 or box-drawing borders mangle under OEM code pages — and the original code pages must be saved and restored on exit.
- **PSModulePath** (`src/main.rs`): PowerShell 7 exports a `PSModulePath` that makes `powershell.exe` 5.1 panes resolve PSReadLine to PS7's script module, which execution policy blocks ("Cannot load PSReadline module"). winmux clears the variable at startup **in every role** (including the server, since it is the pane parent now) before any pane spawns; panes rebuild their own default.
- **ConPTY exit protocol** (`src/pty.rs` / `src/server.rs`): the output pipe does not reliably EOF when the child exits. A waiter thread per pane does `WaitForSingleObject` on the process handle and sends an exit event; dropping the `Pty` (TerminateProcess → ClosePseudoConsole → CloseHandle, in that order) is what unblocks a reader stuck in `ReadFile`.
- **Zero-size geometry**: `layout::rects()` may return zero-size rects when the terminal is tiny; `grid` clamps to 1x1; every consumer must tolerate w==0/h==0 rects rather than assume drawability.
- **Overlapped named-pipe I/O is mandatory, not cosmetic** (`src/pipe.rs`): every pipe handle is opened with `FILE_FLAG_OVERLAPPED`, and `Read`/`Write` issue raw `ReadFile`/`WriteFile` against a per-connection event instead of going through `File`'s synchronous path. Without this, a synchronous handle serializes I/O across its duplicated clones — a pending read on one clone would block a concurrent write on another, which the reader-thread/writer-thread split requires to work.
- **`FILE_FLAG_FIRST_PIPE_INSTANCE` prevents split-brain autostart** (`src/pipe.rs`): only the *initial* `CreateNamedPipeW` for a socket name sets this flag, so it fails with `ERROR_ACCESS_DENIED` if another process already owns the name — the loser of a double cold-start autostart race then exits instead of silently joining the winner's instance pool as a second, round-robined server on the same pipe name. Subsequent accept-time instances (added by the same server) must NOT set the flag.

## Testing conventions

Pure modules (`layout`, `grid`, `render`, `input`, `model`, `status`, `protocol`, `cli`, and sub-project 3's `keys`, `style`, `cmd`, `options`, `bindings`) are unit-tested TDD-style with exact expected values computed in comments (grid tests feed literal VT byte strings like `b"\x1b[2J"` and assert resulting cells/cursor; render tests assert exact emitted VT byte strings with 1-based CUP coordinates). I/O edges (`pty`, `host`, `pipe`) get integration smoke tests against real Windows handles. `tests/server_proto.rs` is the main behavioral seam for sub-projects 2, 3, and 4: it drives the server headlessly over a real pipe with no console/ConPTY host required, so it's fast and can assert deeply on protocol-level behavior (prompts, confirms, window/session switching, config loading, runtime `bind`/`set`, `send-keys`, copy-mode/selection/search, mouse routing, layouts, overlays). `tests/e2e.rs`, `tests/e2e_sessions.rs`, `tests/e2e_config.rs` (sub-project 3: `.tmux.conf` round trip via `-f`, the `prefix-:` command prompt, `send-keys` from a plain CLI invocation), and `tests/e2e_copy_mouse.rs` (sub-project 4: a full keyboard-bytes copy-mode roundtrip — scroll into scrollback history, select, copy, exit, paste, and the pasted text is echoed by the shell — plus SGR mouse-byte injection proving click-to-focus and wheel-scroll-into-copy-mode) are the full-stack proof, driving the actual `winmux.exe` client under a test-owned ConPTY; `tests/common/mod.rs` holds the shared harness (`unique_socket`, `drain_after_exit`'s 10×50ms poll after process exit, `vertical_border_col` for mouse-coordinate tests, etc.). Keep `cargo clippy --all-targets -- -D warnings` green — it is the project's lint gate.

## Keybindings (tmux defaults — table-driven and rebindable since sub-project 3)

These are the *default* bindings loaded into `src/bindings.rs`'s `Bindings::default()`; any of them (including the prefix key itself) can be changed at runtime (`bind-key`/`unbind-key`, or the `prefix-:` command prompt) or from a config file (`set -g prefix <key>`, `bind <key> <command...>`, `unbind <key>`). Prefix defaults to `Ctrl-b`, then:

- Panes (unchanged since the MVP): `%` split left/right · `"` split top/bottom · arrows focus · `o` cycle · `;` last pane · `x` kill (y/n confirm) · `z` zoom · `Ctrl-arrow` resize (repeatable, 500ms window) · the prefix key again sends it literally.
- Windows/sessions (since sub-project 2): `c` new window (create + switch) · `n`/`p` next/previous window (wraps) · `l` toggle to last window · `0`-`9` select window by index · `&` kill window (confirm prompt `kill-window <name>? (y/n)`) · `,` rename window (status-line prompt, pre-filled) · `$` rename session (prompt, pre-filled) · `d` detach (`[detached (from session x)]`, client exits 0) · `(`/`)` switch this client to the previous/next session.
- Command layer (sub-project 3): `:` opens the status-line `prefix-:` command prompt — anything from the CLI subset (`rename-window foo`, `split-window -h`, `set -g status off`, ...) can be typed and executed there, the same dispatcher a keybinding or the `winmux <cmd>` CLI goes through.
- Copy mode (sub-project 4): `[` enters copy mode on the focused pane; `PPage` enters pre-scrolled one page up. `mode-keys` (`emacs` default, or `vi`) selects which key table is active while in copy mode — arrows/`C-b`/`C-n`/`C-p`/`C-f`/`C-a`/`C-e`/`Home`/`End`/`M-<`/`M->`/`PPage`/`NPage`/Space page down in the emacs table; `hjkl`+arrows/`w`/`b`/`e`/`0`/`$`/`g`/`G`/`H`/`M`/`L`/`K`/`J`/`C-u`/`C-d`/`C-b`/`C-f` in the vi table. Both tables share: `q`/Escape cancel, `C-Space` (emacs) or Space (vi) begin selection, `R`/`v` rectangle-toggle, `o` other-end, `C-w`/`M-w` (emacs) or Enter (vi) copy-selection-and-cancel, `C-g` (emacs) or Escape (vi) clear-selection, `C-s`/`/` search-forward, `C-r`/`?` search-backward, `n`/`N` repeat/reverse search. The `[scroll/history]` position indicator is drawn top-right of the pane while in copy mode.
- Paste buffers (sub-project 4): `]` paste the newest buffer (`paste-buffer -p`) · `#` show a one-line buffer-list summary (`list-buffers`) · `-` delete the newest buffer.
- Layouts (sub-project 4): `Space` cycle through the five layout presets (`next-layout`) · `M-1`..`M-5` select a layout by name (even-horizontal/even-vertical/main-horizontal/main-vertical/tiled) · `{`/`}` `swap-pane -U`/`-D` · `C-o`/`M-o` `rotate-window` (forward/`-D`).
- Window ops (sub-project 4): `!` `break-pane` (focused pane out into a new window) · `.` prompt → `move-window -t <index>` · `f` prompt → `find-window <pattern>` (jumps to first match) · `'` prompt → `select-window -t :<index>`.
- Overlays (sub-project 4): `w` `choose-tree -w` (this session's windows) · `s` `choose-tree -s` (all sessions) — Up/Down/`j`/`k` move, Enter switches, `x` kills selected (confirm), `q`/Escape/`C-c` cancel · `q` `display-panes` (big digit overlay for `display-panes-time` ms or until a digit/other key; a digit key jumps to that pane).
- Mouse (sub-project 4, off by default — `set -g mouse on`): click focuses a pane; dragging a pane border live-resizes the split; click/wheel on a status-row window tab selects/cycles windows; wheel-up over a live pane enters copy mode scrolled 5 lines, wheel in copy mode scrolls 5 lines; drag in copy mode selects (release copies), double/triple-click select+copy a word/line.

Prompt/confirm input is captured at the server layer (a line editor: printable append, BSpace delete, Enter commit, Esc/Ctrl-c/Ctrl-g cancel) so prompt keystrokes are never misinterpreted as bindings.

### escape-time and automatic-rename (sub-project 4)

`escape-time` (default 500ms, `set -g escape-time <ms>`) governs how long the server waits after a lone `Escape` byte before treating it as a real Escape keypress rather than the start of an Alt/CSI sequence still in flight — driven by the server's 50ms `Tick` handler flushing any client whose pending decode buffer has aged past the configured window. `automatic-rename` (default `on`) renames a window to the first token of its active pane's ConPTY OSC-0/2 title whenever that title changes (throttled to once per window per 500ms); any manual `rename-window` (including the `,` prompt) permanently disables automatic renaming for that window until reversed by config/another explicit rename call.

### Config files

With no `-f`, the server (at startup) tries `%USERPROFILE%\.tmux.conf` (or `$XDG_CONFIG_HOME/tmux/tmux.conf` if that env var is set) first, then `%USERPROFILE%\.winmux.conf` second — both optional, missing files are silently skipped, later files can override earlier ones. `-f <file>` (top-level flag, e.g. `winmux -L mysock -f myconf.tmux.conf`) replaces that default chain with exactly the file(s) given (required — a missing explicit file is a startup error); `-f -` disables config loading entirely (tmux's `-f /dev/null` idiom). `-f` only has an effect on the invocation that actually autostarts the server — tmux semantics: config is read once at server start, so `-f` against an already-running server is a no-op.
