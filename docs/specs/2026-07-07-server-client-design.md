# Sub-project 2 — Server/client split, sessions, windows, detach/attach

Status: **Delivered** (2026-07-07, branch `feature/server-client-sessions`, 10
tasks, 208+ tests). Companion interface contract:
[`2026-07-07-server-client-interfaces.md`](2026-07-07-server-client-interfaces.md)
(created and extended task-by-task; same lock rules as the MVP contract).

## Goal

Turn the single-process MVP into tmux's real shape: a background **server**
process owning all sessions/windows/panes (ConPTY handles live only there), and
thin **clients** that attach over a named pipe, forward raw input, and write the
server-composed VT stream to their terminal. Sessions survive client
disconnect (including SSH drops). Multiple sessions and windows, tmux CLI
subset, tmux default keybindings for window/session management.

## Process model

One binary, `winmux.exe`, three roles selected by argv:

| Invocation | Role |
|---|---|
| `winmux [flags] [command ...]` | CLI: parse args, connect to pipe (auto-starting the server when the command creates state), then either run a one-shot control command or become an attached client. |
| `winmux __server --pipe <full-pipe-name>` | Hidden: run the server event loop. Spawned by the CLI with `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP` (no console, survives the client's console closing). |

## Transport

- Pipe name: `\\.\pipe\winmux-<username>-<socket-name>`; `<socket-name>`
  defaults to `default`, overridden by `-L <name>` (tmux's `-L`). Username from
  `GetUserNameW` (fallback `unknown`).
- Framing: `[type: u8][len: u32 LE][payload: len bytes]`, max len 1 MiB
  (reject larger = protocol error → drop connection).
- Byte-oriented; strings are UTF-8, u16/u32 are little-endian.

### Client → server frames

| type | name | payload |
|---|---|---|
| 0x01 | `Attach` | `mode: u8` (0 = attach existing, 1 = new session — error if duplicate, 2 = new session with auto-number name), `detach_others: u8` (0/1), `cols: u16`, `rows: u16`, `name_len: u16`, `name: utf8` (empty for mode 2) |
| 0x02 | `Stdin` | raw bytes from the client console |
| 0x03 | `Resize` | `cols: u16`, `rows: u16` |
| 0x04 | `Detach` | (empty) — client-initiated detach (console closing) |
| 0x05 | `Cli` | `argc: u16`, then per arg `len: u16` + utf8 — a one-shot control command |

### Server → client frames

| type | name | payload |
|---|---|---|
| 0x81 | `Output` | composed VT bytes; client writes them to stdout verbatim |
| 0x82 | `Exit` | `code: u8` (0 = success, 1 = error), `msg_len: u16`, `msg: utf8` — terminal frame; client restores console, prints msg (stdout if code 0 else stderr), exits with code |
| 0x83 | `CliDone` | `code: u8`, `out_len: u32` + utf8, `err_len: u32` + utf8 — reply to `Cli`; client prints and exits |

The server fully owns user-visible strings: `[detached (from session x)]`,
`[exited]`, `[server exited]`, `can't find session: x`, `duplicate session: x`.
The client prints what it is told. `no server running on <pipe>` is the one
client-side string (pipe connect failed and the command does not auto-start).

## Server architecture

Same single-threaded-state shape as the MVP loop, scaled up:

```
pane reader/waiter threads ─┐
client pipe reader threads ─┼→ mpsc<ServerEvent> → server main loop (owns ALL state)
accept thread ──────────────┘                          │
                                                       └→ per-client writer thread ← mpsc<Vec<u8>>
```

- **Accept thread**: loop `CreateNamedPipeW` (new instance per connection,
  `PIPE_ACCESS_DUPLEX`, byte mode) → `ConnectNamedPipe` (blocking) → send
  `ServerEvent::Connected(handle)`.
- **Per-client reader thread**: decodes frames → `ServerEvent::FromClient(id, frame)`;
  on error/EOF → `ServerEvent::ClientGone(id)`.
- **Per-client writer thread**: owns the write side; drains an unbounded
  `mpsc<Vec<u8>>` so a slow client never blocks the main loop.
- **Main loop** owns: `Registry` (sessions/windows), `HashMap<PaneId, PaneRuntime>`
  (`Pty` + `Grid` + `dead`), `HashMap<ClientId, ClientState>`
  (`Renderer`, `InputMachine`, session ref, size, prompt/confirm state, writer Sender).
- `recv_timeout(50ms)` tick refreshes the clock and coalesces; resize is now
  client-pushed (`Resize` frames), not polled, on the server side.
- Server process removes `PSModulePath` at startup (it is the pane parent now)
  and installs a panic hook that logs to `%LOCALAPPDATA%\winmux\server.log`
  (no console to print to).
- Server exits when the last session is destroyed (tmux `exit-empty` default on)
  and on `kill-server`.

## Data model (tmux semantics)

- `Registry` → `Vec<Session>`. Session: unique name (auto-name = lowest unused
  non-negative integer as string), creation time, windows, current + last window.
- `Window`: index (lowest unused ≥ 0 at creation, tmux `base-index` 0), name
  (default `powershell`; manual rename via prefix `,`), `Layout` (existing
  split tree — unchanged, one per window), zoom lives in `Layout`.
- `PaneId` stays a server-global monotonically increasing `u32`.
- Last pane in window dies/killed → window destroyed → last window destroyed →
  session destroyed → attached clients get `Exit "[exited]"` → last session
  destroyed → server exits.
- Window size = **smallest attached client** of the session (classic tmux
  behavior; simpler and safer than modern `latest`). No clients attached →
  keep last size (default 80x24).
- Client terminal larger than window size → window renders top-left, rest of
  screen left blank; status bar stays on the client's real bottom row.

## Input routing (server-side)

Each client has its own `InputMachine` (prefix state is per client). New
`Action` variants (contract update) bound per tmux defaults:

| key (after Ctrl-b) | Action | behavior |
|---|---|---|
| `c` | `NewWindow` | create + switch |
| `n` / `p` | `NextWindow` / `PrevWindow` | wraps |
| `l` | `LastWindow` | toggle to previously-current window |
| `0`-`9` | `SelectWindow(n)` | exact index; miss → status message `window not found: n` |
| `&` | `RequestKillWindow` | confirm prompt `kill-window <name>? (y/n)`; `y`/`Y`/Enter confirms |
| `,` | `RenameWindow` | status-line prompt `(rename-window) ` pre-filled with current name |
| `$` | `RenameSession` | prompt `(rename-session) ` pre-filled |
| `d` | `Detach` | `[detached (from session x)]`, client exits 0 |
| `(` / `)` | `SwitchClientPrev` / `SwitchClientNext` | move this client across sessions |
| `x` | (existing) | confirm text becomes tmux's `kill-pane <pane-index>? (y/n)` (index within window) |

Existing pane bindings (`%`, `"`, arrows, `o`, `;`, `z`, Ctrl-arrows, literal
Ctrl-b) unchanged. Confirm and prompt input is captured at the server layer
(line editor: printable append, BSpace/0x7f/0x08 delete, Enter commit,
Esc/Ctrl-c/Ctrl-g cancel) — `InputMachine` gets a capture mode so prompt
keystrokes are never interpreted as bindings. Deferred to sub-projects 3/4:
`w`/`s` choose-tree, `D`, `f`, `'`, `.`, `!`, `q`, Space layouts, `{`/`}`,
copy mode, mouse.

## Status bar (real state now)

`[session-name] 0:powershell* 1:foo- 2:bar        HH:MM DD-Mon-YY`

- left: `[#S] ` then window list in index order, each `#I:#W` + flag
  (`*` current — rendered underlined, tmux `window-status-current-style`
  `underscore`; `-` last; `Z` appended when that window is zoomed).
- right: clock as today (pane-title formats arrive with sub-project 3).
- messages/prompts overlay the row in yellow/black (existing message slot).

## CLI subset (this sub-project)

`new-session`/`new` `[-d] [-s name] [-x cols] [-y rows]` · `attach-session`/`attach` `[-d] [-t name]` ·
`detach-client` `[-s name]` · `list-sessions`/`ls` · `list-windows`/`lsw` ·
`has-session`/`has` `[-t name]` · `kill-session` `[-t name]` · `kill-server` ·
`rename-session` `[-t name] new` · `rename-window` `[-t name] new` ·
global `-L <socket-name>`. Bare `winmux` = `new-session`. Output strings match
tmux verbatim, e.g. `ls`:
`name: N windows (created Tue Jul  7 09:14:22 2026) (attached)` (always plural
`windows`, tmux quirk). Target `-t` resolution: exact match, then unambiguous
prefix (tmux rule); `=name` forces exact.

Auto-start: `new-session` (incl. bare) starts the server when the pipe is
absent; pure queries (`ls`, `has`, `attach`, `kill-*`) error with
`no server running on <pipe>` (exit 1).

## Testing strategy

Realized as planned:

1. Pure units as before: protocol codec roundtrips, model bookkeeping, status
   builder, input additions — TDD with exact expected values.
2. **Headless server integration** (`tests/server_proto.rs`, 33 tests): starts
   the server in-process on a unique `-L`, speaks raw protocol over the pipe
   (no console, no ConPTY host needed): attach, expect `Output` containing the
   status bar; `Stdin`, `Resize`, detach, kill-session, ls, prompts/confirms,
   window/session switching. This is the main behavioral test seam and where
   most of sub-project 2's coverage lives.
3. **Full e2e** (`tests/e2e.rs` + the new `tests/e2e_sessions.rs`, 6 tests
   total): drives `winmux.exe` (client role) under the test ConPTY exactly as
   the MVP did, with a unique `-L` per test and `kill-server` teardown: new →
   split → detach → `[detached]` → `ls` shows `(attached)`→`` → reattach →
   screen state persisted → new window → status bar flags → kill-session →
   server gone. Serialize with `-- --test-threads=1` when running e2e tests
   alongside each other — they spawn real detached server processes and bind
   real named pipes, so parallel runs can interfere.

## Follow-ups folded in (from docs/follow-ups.md)

#1 drop `Pty` at `Exited` (server task) · #2 confirm-race fixed structurally
(events dispatched one at a time against live confirm state) · #3 publish
`RESTORE` before first mutation (host task) · #4 coalesce pane `Output` events
per loop turn before rendering · #5 `saturating_add` in layout adjacency ·
#6 grid panic message with coordinates.
