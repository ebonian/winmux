# Sub-project 2 — Server/client split — Locked Interface Contract

**Status:** Locked, extended task-by-task. Every implementation task MUST
conform to these types and signatures exactly. If a signature must change
during implementation, the change must be applied consistently to every
consumer named here (same rule as the MVP contract).

**Parent spec:** [`2026-07-07-server-client-design.md`](2026-07-07-server-client-design.md)

## `protocol` — client/server frame codec (pure)

```rust
pub const MAX_FRAME: u32 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    Attach { mode: AttachMode, detach_others: bool, cols: u16, rows: u16, name: String },
    Stdin(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Detach,
    Cli(Vec<String>),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachMode { Existing = 0, NewNamed = 1, NewAuto = 2 }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    Output(Vec<u8>),
    Exit { code: u8, msg: String },
    CliDone { code: u8, out: String, err: String },
}

pub fn write_client_msg(w: &mut impl std::io::Write, m: &ClientMsg) -> std::io::Result<()>;
pub fn read_client_msg(r: &mut impl std::io::Read) -> std::io::Result<ClientMsg>;
pub fn write_server_msg(w: &mut impl std::io::Write, m: &ServerMsg) -> std::io::Result<()>;
pub fn read_server_msg(r: &mut impl std::io::Read) -> std::io::Result<ServerMsg>;
```

**Wire format** (byte-oriented; all multi-byte integers little-endian):

```
[type: u8][len: u32 LE][payload: len bytes]
```

`len` is the payload length only (not counting the 5-byte header). Reads use
`read_exact` throughout, so a short/absent underlying stream surfaces as
`io::ErrorKind::UnexpectedEof` — including a clean EOF before the type byte
(no bytes at all) and a payload that runs out mid-way. `len > MAX_FRAME` (1
MiB) is rejected as `io::ErrorKind::InvalidData` before the payload is read.
An unrecognized type byte, or a payload whose declared field lengths don't
fit the bytes actually present (including invalid UTF-8 in a string field),
is also `InvalidData`. Callers drop the connection on any such error — there
is no resynchronization.

Strings are UTF-8 with a `u16` length prefix, except `CliDone`'s `out`/`err`
fields, which use a `u32` length prefix (they carry arbitrary command output,
not short names).

### Client → server frame types

| type | name | payload |
|---|---|---|
| 0x01 | `Attach` | `mode: u8` (0=Existing, 1=NewNamed, 2=NewAuto) · `detach_others: u8` (0/1) · `cols: u16` · `rows: u16` · `name_len: u16` · `name: utf8` |
| 0x02 | `Stdin` | raw bytes (the entire payload, no further structure) |
| 0x03 | `Resize` | `cols: u16` · `rows: u16` |
| 0x04 | `Detach` | (empty) |
| 0x05 | `Cli` | `argc: u16`, then per arg: `len: u16` + `utf8` |

### Server → client frame types

| type | name | payload |
|---|---|---|
| 0x81 | `Output` | raw VT bytes (the entire payload) |
| 0x82 | `Exit` | `code: u8` · `msg_len: u16` · `msg: utf8` |
| 0x83 | `CliDone` | `code: u8` · `out_len: u32` + `utf8` · `err_len: u32` + `utf8` |

**Golden byte example** — `ClientMsg::Attach{ mode: Existing, detach_others:
false, cols: 80, rows: 24, name: "main" }` encodes to:

```
[0x01, 12,0,0,0, 0x00, 0x00, 0x50,0x00, 0x18,0x00, 0x04,0x00, b'm',b'a',b'i',b'n']
```

(type 0x01; payload length 12 = 1 mode + 1 detach_others + 2 cols + 2 rows +
2 name_len + 4 name bytes; cols=80=0x0050 LE, rows=24=0x0018 LE, name_len=4.)

**Implementation module:** `src/protocol.rs`, pure `std::io::{Read, Write}` —
no unsafe, no Windows APIs, no dependency on any other new module. Works
identically over a named pipe handle, a TCP stream, or an in-memory
`Vec<u8>`/`Cursor` (as the unit tests do).

## `pipe` — named-pipe transport (Windows)

```rust
pub fn pipe_name(socket_name: &str) -> String; // "\\.\pipe\winmux-<username>-<socket_name>"

pub struct PipeListener; // opaque: owns nothing between accepts
impl PipeListener {
    pub fn bind(full_name: &str) -> std::io::Result<PipeListener>;
    pub fn accept(&self) -> std::io::Result<PipeConn>;
}

pub struct PipeConn; // opaque: HANDLE wrapped in std::fs::File
impl PipeConn {
    pub fn connect(full_name: &str) -> std::io::Result<PipeConn>;
    pub fn try_clone(&self) -> std::io::Result<PipeConn>;
}
impl std::io::Read for PipeConn { ... }
impl std::io::Write for PipeConn { ... }
```

**Behavior:**

- `pipe_name` builds `\\.\pipe\winmux-<username>-<socket_name>`. `<username>`
  comes from `GetUserNameW` (`"unknown"` on failure); both `<username>` and
  `<socket_name>` are sanitized by keeping `[A-Za-z0-9_-]` and replacing every
  other character with `_`.
- `PipeListener::bind` calls `CreateNamedPipeW(name, PIPE_ACCESS_DUPLEX |
  FILE_FLAG_OVERLAPPED, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
  PIPE_UNLIMITED_INSTANCES, 64*1024, 64*1024, 0, None)` and **holds** the
  resulting instance — this proves the name is creatable before `bind`
  returns. The first `accept` consumes that held instance; every subsequent
  `accept` creates a fresh instance via the same `CreateNamedPipeW` call
  before waiting on it. Each `accept` blocks in `ConnectNamedPipe` (issued
  with an `OVERLAPPED` and waited out via `GetOverlappedResult(..., bWait:
  TRUE)` when it doesn't complete synchronously — required because the
  handle is overlapped, see below); a client that connects in the race
  window before `ConnectNamedPipe` is called surfaces as `ERROR_PIPE_CONNECTED`,
  which `accept` treats as success rather than an error.
- `PipeConn::connect` calls `CreateFileW(name, GENERIC_READ | GENERIC_WRITE,
  0, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED, None)`.
  `ERROR_FILE_NOT_FOUND` (no server bound to that name) maps to
  `io::ErrorKind::NotFound` — the CLI's "no server running" signal.
  `ERROR_PIPE_BUSY` (instance limit momentarily exhausted) triggers one
  `WaitNamedPipeW(name, 2000)` wait followed by exactly one retry of
  `CreateFileW`.
- **Every handle is opened with `FILE_FLAG_OVERLAPPED`** (added during Task
  6). `Read`/`Write` issue raw `ReadFile`/`WriteFile` calls against a
  private-per-`PipeConn` auto-reset event (bypassing `std::fs::File`'s own
  read/write, which cannot be used on a handle opened this way) and block
  via `GetOverlappedResult(..., bWait: TRUE)` until that specific call
  completes — so from the outside, `Read`/`Write` are still plain blocking
  calls. This is a correctness fix, not an optimization: a
  *non*-overlapped handle serializes ALL synchronous I/O against the
  underlying file object, including across `try_clone`'d duplicates of the
  same handle. A blocking read parked on one duplicate (e.g. a per-client
  reader thread waiting for the next frame) would stall a write on
  *another* duplicate of the very same connection (e.g. a writer thread
  sending a reply) forever — discovered as a real deadlock while building
  `src/server.rs`, whose architecture requires exactly that shape (one
  reader thread + one writer thread per client, each with its own cloned
  `PipeConn`). Every `PipeConn` (including each `try_clone`) gets its OWN
  event; sharing one across concurrently-used duplicates would let one
  operation's completion spuriously wake a wait on an unrelated operation.
  Regression coverage: `tests/pipe_smoke.rs::write_on_one_clone_does_not_block_behind_a_pending_read_on_another`.
- Reads map `ERROR_BROKEN_PIPE`/`ERROR_PIPE_NOT_CONNECTED` to `Ok(0)` (clean
  EOF), matching `src/pty.rs`'s `ERROR_BROKEN_PIPE` convention.
- Windows-rs wraps a failed call's `GetLastError()` code as an HRESULT
  (`HRESULT_FROM_WIN32`); `pipe.rs` unwraps that back to the raw Win32 code
  before constructing an `io::Error` so `.kind()` classification (e.g.
  `NotFound`) matches what calling `GetLastError()` directly would give.

**Cargo.toml:** requires the `Win32_System_WindowsProgramming` feature (for
`GetUserNameW`), alongside the already-enabled `Win32_System_Pipes`
(`CreateNamedPipeW`/`ConnectNamedPipe`/`WaitNamedPipeW`), `Win32_Storage_FileSystem`
(`CreateFileW`/`ReadFile`/`WriteFile`/`FILE_FLAG_OVERLAPPED`), and — added in
Task 6 for overlapped I/O — `Win32_System_IO` (`OVERLAPPED`,
`GetOverlappedResult`) and `Win32_Security` (required by the `CreateEventW`
binding used for each `PipeConn`'s completion event).

**Implementation module:** `src/pipe.rs`. Depends only on `windows` Win32
APIs and `std`; does not depend on `protocol` (frames flow over `PipeConn`
via its `Read`/`Write` impls, exercised together in
`tests/pipe_smoke.rs::roundtrip_client_server`).

## `model` — session/window registry (pure)

```rust
pub type WindowId = u32; // server-global, monotonic — NOT the tmux window index
pub struct Window {
    pub id: WindowId,
    pub index: u32,          // tmux window index (lowest unused >= 0 at creation)
    pub name: String,        // default "powershell"
    pub layout: crate::layout::Layout,
}
pub struct Session {
    pub name: String,
    pub created: std::time::SystemTime,
    pub windows: Vec<Window>,        // kept sorted by index
    pub current: WindowId,
    pub last: Option<WindowId>,
    pub size: (u16, u16),            // current window size (smallest attached client)
}
pub struct Registry { /* sessions: Vec<Session> in creation order, next_window_id */ }
impl Registry {
    pub fn new() -> Registry;
    pub fn create_session(&mut self, name: Option<&str>, first_pane: crate::layout::PaneId, size: (u16, u16))
        -> Result<&mut Session, String>;                    // Err("duplicate session: <name>")
    pub fn find(&mut self, target: &str) -> Result<&mut Session, String>; // tmux -t rules: "=x" exact only; else exact, then unambiguous prefix; Err("can't find session: <t>")
    pub fn kill_session(&mut self, name: &str) -> bool;
    pub fn sessions(&self) -> &[Session];
    pub fn session_mut(&mut self, name: &str) -> Option<&mut Session>;
    pub fn is_empty(&self) -> bool;
    pub fn auto_name(&self) -> String;                      // lowest unused non-negative integer as string
    pub fn neighbor_session(&self, current: &str, next: bool) -> Option<&str>; // for ( / ) switch-client, wraps
    pub fn mint_window_id(&mut self) -> WindowId;           // fresh id from the SAME counter create_session uses internally
}
impl Session {
    pub fn new_window(&mut self, id: WindowId, first_pane: crate::layout::PaneId) -> &mut Window; // index = lowest unused, becomes current, updates last
    pub fn kill_window(&mut self, id: WindowId) -> bool;    // removes; retargets current to last-or-nearest; false if it was the only window
    pub fn select_window(&mut self, index: u32) -> bool;    // exact index; updates current/last
    pub fn next_window(&mut self) / pub fn prev_window(&mut self); // wrap by index order
    pub fn last_window(&mut self) -> bool;
    pub fn current_window(&self) -> &Window; pub fn current_window_mut(&mut self) -> &mut Window;
    pub fn window_by_pane(&mut self, pane: crate::layout::PaneId) -> Option<&mut Window>;
}
```

**Behavior notes:**

- Session/window name validation: reject empty names and names containing
  `:` or `.` (tmux's target separators) → `Err("bad session name: <n>")`.
  Applied in `create_session`; there is no rename API yet (deferred to a
  later task alongside the `,`/`$` prefix bindings).
- `Registry` allocates `WindowId`s from a single internal `next_window_id`
  counter. `create_session` mints one internally for a session's initial
  window; `mint_window_id` (Task 7) exposes that SAME counter for any other
  caller adding a window to an EXISTING session (e.g. the `NewWindow`
  action) — `Session::new_window` itself only takes the id as a plain
  parameter and never mints its own, so the caller must mint via
  `Registry::mint_window_id` first. The two minting paths never collide
  since they share one counter.
- `find`'s `=name` form strips the sigil before matching and before building
  the error message, so `find("=foo")` that fails reports
  `can't find session: foo` (not `=foo`).
- Exact name match always wins over prefix matching, even when the exact
  name is itself a prefix of another session's name (e.g. sessions "foo"
  and "foobar": `find("foo")` returns "foo", not an ambiguity error).
- `kill_session`/`session_mut` are exact-name-only (no prefix rules, no `=`
  handling) — `find` is the only tmux-target-resolving entry point.
- `Session::kill_window` on the only remaining window is a no-op returning
  `false` (mirrors `Layout::remove`'s single-pane guard); the caller is
  expected to destroy the whole session instead, as for the last pane in a
  window. On removing a non-only window: if `last` pointed at the removed
  window, it's cleared to `None` first; if the *current* window was
  removed, it falls back to `last` (if still alive) else to the nearest
  window by index (highest index below the removed one, else lowest index
  above).
- `next_window`/`prev_window`/`select_window`/`new_window` maintain `last =
  <previous current>` on every change; re-selecting the already-current
  window via `select_window` is a no-op (doesn't disturb `last`).
  `next_window`/`prev_window` are no-ops with a single window.
- `last_window` swaps `current`/`last` (like `Layout::focus_last`); `false`
  if there is no `last` or it no longer exists.

**Implementation module:** `src/model.rs`, pure logic (no I/O, no Windows
APIs, no threads) — unit-tested the same way as `src/layout.rs`. Depends
only on `crate::layout` (`Layout`, `PaneId`) and `std`.

## `input` — window/session bindings and capture mode (Task 4 amendment)

`src/input.rs` (locked by
[`2026-07-06-mvp-interfaces.md`](2026-07-06-mvp-interfaces.md)) gained new
`Action` variants — `NewWindow`, `NextWindow`, `PrevWindow`, `LastWindow`,
`SelectWindow(u32)`, `RequestKillWindow`, `RenameWindow`, `RenameSession`,
`Detach`, `SwitchClientPrev`, `SwitchClientNext` — bound from the Prefixed
state (`c n p l 0-9 & , $ d ( )` respectively), a new `InputEvent::Captured
(Vec<u8>)` variant, and a new `InputMachine::set_capture(&mut self, on:
bool)` method for raw byte passthrough (status-line prompts: rename-window,
rename-session). Full details, including capture-vs-confirming precedence,
are in the amended `input` section of `2026-07-06-mvp-interfaces.md` — this
entry is only a cross-reference for readers starting from this document.
None of these new `Action` variants are dispatched by `src/app.rs` yet
(no-op arms with a `wired up by server (sub-project 2)` comment); a later
task wires them into `Registry`/`Session` (defined above) and the
server/client loop.

## `status` — status-line span builder (pure, Task 5)

```rust
// status.rs
pub struct WindowEntry {
    pub index: u32,
    pub name: String,
    pub current: bool,
    pub last: bool,
    pub zoomed: bool,
}

pub fn status_spans(session_name: &str, windows: &[WindowEntry]) -> Vec<StatusSpan>;
```

`StatusSpan { text: String, underline: bool }` is defined in `render.rs` (see
the **LOCKED-CONTRACT AMENDMENT** in `2026-07-06-mvp-interfaces.md`'s `render`
section: `Scene::status_left: String` → `Scene::status_spans:
Vec<StatusSpan>`). `status.rs` is pure bookkeeping with no dependency on
`model.rs`; a caller (future server/client wiring task) maps a `Session`'s
`Window`s to `WindowEntry`s (`current`/`last` from `Session::current`/`last`,
`zoomed` from `Window::layout.is_zoomed()`).

**Span composition** (index order as given in `windows`):
1. One span `"[<session_name>] "` (trailing space included), `underline: false`.
2. Per window, one span `"<index>:<name><flags>"`, `underline: true` iff
   `current`, else `false`.
3. Between window spans (not after the last one), a separate single-space
   span `" "`, `underline: false` — the separator itself is never underlined,
   even when the window before or after it is current.

**Flags string** for a window (exact rule — resolves the apparent ambiguity
between "else a literal space" and "else empty" phrasings that circulated
during design; empty is correct and is what's implemented/tested):
- `*` if `current`, else `-` if `last`, else nothing (empty).
- Then `Z` appended if `zoomed`.
- So: current+zoomed → `*Z`; last+zoomed → `-Z`; zoomed only (neither current
  nor last) → `Z` (e.g. window 2 named `logs` renders `2:logsZ`); no flags at
  all → bare `<index>:<name>` (e.g. `2:logs`).

**Render integration:** `render::compose_back`'s status-bar step draws
`status_spans` left-to-right from column 0 (each span's cells get the
status-row style — green bg/black fg, or yellow/black under a `message`
override — with that span's `underline` flag set on the style), then
`status_right` right-aligned as before; the "total left length" used for
right-truncation is the summed char count of all spans' text.

**Implementation module:** `src/status.rs`, pure (no I/O), unit-tested with
exact expected `Vec<(String, bool)>` span vectors (mirrors `render.rs`'s
exact-VT-bytes test style).

## `server` — headless multiplexer server (Task 6)

```rust
pub fn run(pipe_full_name: &str) -> Result<(), Box<dyn std::error::Error>>;
```

That is the ENTIRE public surface of `src/server.rs`; every type named below
(`ServerEvent`, `PaneRuntime`, `ClientState`, `ClientMode`, `Server`, and the
various free helper functions) is private to the module.

**Behavior:**

- `run` binds `pipe_full_name` via `pipe::PipeListener::bind`, spawns an
  accept thread (`while let Ok(conn) = listener.accept() { ... }`, sending
  `ServerEvent::Connected`), then loops: `recv_timeout(50ms)` on a single
  `mpsc<ServerEvent>` (falling back to a synthetic `Tick` on timeout), then
  drains every immediately-available further event via `try_recv` before
  rendering once (follow-up #4's coalescing). Every attached client is
  re-rendered on any dirty turn (see "Design choices" below) — not a
  per-session dirty set.
- `run` returns `Ok(())` once the registry has held at least one session
  AND is now empty (`had_session` flag guards against mistaking a
  never-yet-used server for exit-empty at startup). It does not touch the
  console and installs no panic hook (both remain `main.rs`'s job, Task 8).
- **Attach** (`ClientMsg::Attach`): `NewAuto` mints an auto session name and
  always succeeds (barring pane-spawn failure); `NewNamed` checks for a
  duplicate name up front (`Exit{1, "duplicate session: <n>"}`) before
  spawning a pane, and rolls the pane back if `Registry::create_session`
  still rejects the name (bad chars); `Existing` resolves the name via
  `Registry::find` (tmux `-t` prefix rules) and — when `detach_others` is
  set — sends every OTHER client currently attached to that session a plain
  `Exit{0, "[detached]"}` (distinct from the named `Detach`-action/-frame
  message) before attaching the new one. First window is always index 0,
  name `powershell`; shell is `powershell.exe -NoLogo`. A fresh `Renderer`
  is constructed and immediately `resize()`d to its own dimensions (forcing
  `force_full`) so the very first `compose()` is a guaranteed full repaint.
- **Session size**: `min(cols)` x `min(rows - 1)` over all clients attached
  to that session (the `- 1` reserves the client's own bottom row for its
  status bar, which is not part of the shared pane area); recomputed on
  attach, `Resize` frame, detach (frame or action), and client disconnect.
  No attached clients: keeps the last size. `apply_layout` (private,
  mirrors `app.rs`'s function of the same name but keyed by `HashMap`
  instead of a flat `Vec`) then resizes every pane whose rect changed.
- **Input routing**: `Stdin` frames are fed to the client's own
  `InputMachine`; the resulting events are dispatched one at a time against
  live `Registry`/pane state. `Split`/`Focus`/`FocusNext`/`FocusLast`/
  `ToggleZoom`/`Resize` mutate the session's current window's `Layout`
  directly. `RequestClose` arms `ClientMode::ConfirmKillPane(pane_id)` +
  `InputMachine::set_confirming(true)`; the render-time message text is
  `kill-pane <idx>? (y/n)` where `<idx>` is that pane's position in
  `layout.panes()` (recomputed at render time, not cached). Confirming
  removes the pane (or, if it was the window's only pane, destroys the
  whole session — same "last pane" logic as a natural process exit).
  `Action::Detach` and the `Detach` frame both end in the SAME per-client
  teardown: `Exit{0, "[detached (from session <name>)]"}`, size/layout
  recomputed for any clients left. Window/session `Action` variants
  (`NewWindow`, `NextWindow`, `PrevWindow`, `LastWindow`, `SelectWindow`,
  `RequestKillWindow`, `RenameWindow`, `RenameSession`, `SwitchClientPrev`,
  `SwitchClientNext`) and `InputEvent::Captured` are wired by Task 7 (below);
  `Action::Quit` remains a listed no-op arm (never emitted by
  `InputMachine::feed`, an MVP-only hook) — the match is still exhaustive,
  not a wildcard, so a new `Action` variant without a corresponding arm is a
  compile error.
- **Pane exit** (Task 7 rewrite — tmux `remain-on-exit off` parity): the
  waiter thread's `ServerEvent::Exited(pane_id)` drops that pane's `Pty`
  immediately (`PaneRuntime.pty = None`, follow-up #1) and marks it dead.
  `handle_exited` then finds which (session, window) owns the pane (any
  window, not just the current one). If another pane in that SAME window is
  still alive, the dead pane is removed outright — `Layout::remove` plus
  dropping its `PaneRuntime`/`last_rects` entry, same path as a confirmed
  `kill-pane` — instead of leaving a dead `[exited]` overlay; no window/
  session-level change. Otherwise (it was the window's last live pane): if
  the window was the session's only window, the whole session is destroyed
  (`destroy_session`, every attached client gets `Exit{0, "[exited]"}`);
  otherwise just that window is killed (`Session::kill_window` plus
  dropping all its panes) and the session lives on with its remaining
  windows. The `dead` flag on `PaneRuntime` stays in the struct — it is
  still read (briefly, between the `Exited` event and this handler running)
  by `render_one`'s dead-pane overlay and by `other_panes_alive`'s liveness
  check — even though a dead pane's `PaneRuntime` is now always removed
  within the same `handle_exited` call rather than lingering.
- **`Cli` frames** execute a real command set against the registry (Task 7)
  — see "CLI subset" below.
- **Window/session `Action`s** (Task 7): `NewWindow` mints a `PaneId` +
  spawns a pane sized to the session's current size, mints a `WindowId` via
  `Registry::mint_window_id`, and calls `Session::new_window`, becoming
  current; a spawn failure leaves the window uncreated and sets a transient
  message (`open terminal failed: <io error>`). `NextWindow`/`PrevWindow`/
  `LastWindow` call the matching `Session` method and reapply layout.
  `SelectWindow(n)` calls `Session::select_window`; a miss sets the
  transient message `window not found: <n>`. `RequestKillWindow` arms
  `ClientMode::ConfirmKillWindow(WindowId)` (`InputMachine::set_confirming
  (true)`); the render-time message is `kill-window <name>? (y/n)`.
  Confirming it removes every pane of that window and calls
  `Session::kill_window`, or — if it was the session's only window —
  destroys the whole session (same as a confirmed only-pane kill).
  `RenameWindow`/`RenameSession` arm `ClientMode::Prompt{label, buf, kind}`
  (`InputMachine::set_capture(true)`) with `label` `"(rename-window) "` /
  `"(rename-session) "` and `buf` pre-filled with the current name.
  `SwitchClientPrev`/`SwitchClientNext` move ONLY the acting client to the
  session adjacent in `Registry::neighbor_session` order (wraps; a single
  session is a no-op), force a full repaint via `Renderer::resize`
  (unconditionally sets `force_full`, regardless of whether the
  cols/rows actually changed), and trigger a resize/layout recompute of
  BOTH the old and new session once the client is back in `self.clients`.
- **Prompt line editor** (`ClientMode::Prompt`, Task 7): while
  `InputMachine::set_capture(true)` is active, `Stdin` frames decode to
  `InputEvent::Captured(Vec<u8>)`; each byte of a captured chunk is fed one
  at a time to `Server::feed_prompt_byte`: printable ASCII (0x20-0x7e)
  appends to `buf`; Backspace (0x7f or 0x08) removes the last char;
  Enter (`\r`/`\n`) commits; Esc/Ctrl-c/Ctrl-g (0x1b/0x03/0x07) cancels
  outright (discards `buf`, no validation). Either way, `set_capture(false)`
  and `mode` returns to `Normal`; any REMAINING bytes of the same captured
  chunk after the commit/cancel byte are then re-fed through
  `InputMachine::feed` and dispatched normally (capture is already off), so
  pasted input like `web\recho hi\r` commits the rename AND forwards the
  trailing `echo hi\r` to the pane instead of silently dropping it
  (post-review fix; test `prompt_commit_forwards_trailing_bytes`).
  On commit: `PromptKind::RenameWindow`
  validates `buf` (reject empty or containing `:`/`.`, mirroring
  `model.rs`'s session-name rule) and renames `session.current_window_mut()
  .name` directly (`Window::name` is a public field; the model has no
  rename API of its own). `PromptKind::RenameSession` additionally rejects
  a duplicate (any OTHER existing session already using `buf`); on success
  it mutates `Session::name` directly AND propagates the new name to every
  attached client's `ClientState::session` (`rename_session_everywhere`,
  since clients look their session up by name) plus the acting client's own
  `session`/local tracking variable, so mid-batch events after the commit
  (and any later Stdin frame) keep routing correctly. Either validation
  failure becomes a transient status message (see below); the prompt itself
  is still discarded (same as a cancel).
- **Transient status messages** (`ClientState::message: Option<(String,
  Instant)>`, Task 7): set by `window not found: <n>` (`SelectWindow` miss),
  `NewWindow` spawn failures, and prompt-commit validation errors. Shown in
  the message slot only while `mode` is `Normal` AND not `too_small`.
  Precedence matches the pre-Task-7 code exactly: `ConfirmKillPane`'s match
  arm had no guard, so it always won regardless of `too_small`; `Prompt`/
  `ConfirmKillWindow` are new sibling arms with the same unconditional
  priority (a confirm/prompt overlay doesn't depend on pane space, so it
  stays visible even on a too-small terminal) — `too_small` and the
  transient message only compete with each other, in `Normal` mode, and
  `too_small` wins there too. Cleared two ways: unconditionally at the top
  of `handle_stdin` for ANY `Stdin` frame
  from that client (so any keystroke dismisses it, whether or not it maps
  to an action), or after `MESSAGE_LIFETIME` (750ms, tmux's `display-time`
  default) elapses, checked on the 50ms `Tick` (which also now returns
  `dirty = true` whenever a message actually expires, in addition to its
  existing clock-changed check).
- **`ClientMode`** (Task 7 additions): `ConfirmKillWindow(WindowId)` (armed
  by `RequestKillWindow`, resolved by the SAME `InputEvent::ConfirmClose`
  event `ConfirmKillPane` uses — the two are now separate match arms over
  `client.mode` rather than a single `if let`) and `Prompt { label: String,
  buf: String, kind: PromptKind }` where `PromptKind::{RenameWindow,
  RenameSession}` (both server-private types).
- **Stale-confirm invalidation** (post-review fix): whenever a pane or
  window is removed by ANY path (natural exit in `handle_exited`, confirmed
  kill, window/session teardown), `cancel_stale_confirms` resets every
  client whose pending `ConfirmKillPane`/`ConfirmKillWindow` targets an id
  that no longer exists (mode → `Normal`, `set_confirming(false)`, message
  cleared, re-render). Independently, the confirm handlers re-verify their
  target still exists before acting and treat a missing target as a no-op
  cancel — NEVER as "last one → destroy session" (that mis-read previously
  let a stale `y` tear down a live session). Test:
  `stale_confirm_after_pane_exit_is_canceled`.
- **Confirm race (follow-up #2): left open, by design choice.** `Ctrl-b x`
  immediately followed by `y` in the SAME `Stdin` frame still races exactly
  as in the MVP — `InputMachine::feed` tokenizes the whole frame before this
  module gets a chance to call `set_confirming`, so the `y` is forwarded to
  the pane instead of confirming. This is one of the two options the task
  brief sanctioned (structurally fix vs. leave documented); chosen here to
  avoid a fiddly re-interpretation pass given the interactive impact is
  minor (an errant `y` reaching a shell prompt) and no test exercises the
  same-frame case.
- **Test thread lifecycle** (`tests/server_proto.rs`): each test uses a
  unique pipe name. Where the test's own flow naturally destroys every
  session (most of them, via `exit` in the last shell), the server thread
  is joined to prove clean exit-empty shutdown. `attach_missing_session_error`
  never creates a session, so its server thread is intentionally left
  running (detached) — safe because its pipe name is unique to that test
  and the process exits at the end of the test binary regardless.
- **Thread-shutdown note**: `run`'s accept thread is not joined or
  cancelled when `run` returns — it is left blocked in `PipeListener::accept`
  (or, in practice, simply abandoned) since nothing will ever connect to a
  now-unbound name. This is intentional: the only real caller (`main.rs`,
  Task 8) exits the whole process immediately after `run` returns, which
  reclaims the thread; there is currently no in-process "stop the server
  and keep the process alive" path.

**CLI subset** (Task 7): `handle_cli` looks up the calling connection's
writer channel (attached or still-`pending_writers`, i.e. never attached —
a bare CLI connection sends exactly one `Cli` frame and disconnects after
its `CliDone` reply) and replies with `execute_cli`'s `(code, out, err)`.
Argv parsing is a small hand-rolled `CliArgs` (`-t`/`-s` string, `-x`/`-y`
`u16`, `-d` bare flag, non-flag tokens collected as `positional` in order)
— no external arg-parsing crate. Unrecognized/empty command name:
`CliDone{1, "", "unknown command"}`. Any token starting with `-` that is
not a recognized flag FOR THAT COMMAND (each command declares its accepted
flag set) → `CliDone{1, "", "<usage line>"}` using that command's usage
constant (`usage: rename-session [-t target] new-name`, etc. — the same
usage strings listed per-command in the table below); flags are never
silently demoted to positionals (post-review fix; test
`cli_unknown_flag_is_usage_error`).

| command | args | success | failure |
|---|---|---|---|
| `list-sessions`/`ls` | (none) | `out` = one line per session, creation order: `"{name}: {n} windows (created {ctime}){attached}"` (`{n}` always plural, tmux quirk; `{attached}` = `" (attached)"` iff ≥1 client attached, else empty), each line `\n`-terminated | empty registry → `{1, "", "no sessions"}` |
| `has-session`/`has` | `-t name` (required) | `{0, "", ""}` | no `-t` → `{1,"","usage: has-session -t target"}`; not found → `{1,"","can't find session: <t>"}` (from `Registry::find`, tmux prefix rules) |
| `kill-session` | `[-t name]` | `{0,"",""}`, `destroy_session` (attached clients get `Exit{0,"[exited]"}`) | `-t` given but not found → `find`'s error; no `-t` and no sessions → `{1,"","no sessions"}`; default target (no `-t`) is the MOST RECENTLY CREATED session (`Registry::sessions().last()` — creation order) |
| `kill-server` | (none) | `{0,"",""}`; every attached client gets `Exit{0,"[server exited]"}`, every pane's `Pty` is dropped (`self.panes.clear()`), registry is cleared and `had_session` forced `true` so `run`'s exit-empty check fires this turn | (infallible) |
| `new-session`/`new` | `[-d] [-s name] [-x cols] [-y rows]` | `{0,"",""}`; spawns a pane sized `(x.unwrap_or(80), y.unwrap_or(24))` (both `.max(1)`) and calls `Registry::create_session` — `-d` is accepted but otherwise a no-op here (the `Cli` path never attaches a client regardless) | duplicate/bad name → `create_session`'s error (pane rolled back); spawn failure → `"failed to spawn shell: <io error>"` |
| `rename-session` | `[-t target] new-name` (both required) | `{0,"",""}`; renames `Session::name` directly and propagates to every attached client via `rename_session_everywhere` | missing `-t` or new-name → `"usage: rename-session [-t target] new-name"`; target not found → `find`'s error; bad new name → `"bad session name: <n>"`; duplicate → `"duplicate session: <n>"` |
| `rename-window` | `[-t target[:idx]] new-name` (both required) | `{0,"",""}`; renames `Window::name` directly | missing `-t` or new-name → `"usage: rename-window [-t target] new-name"`; session part not found → `find`'s error; `:idx` given but no such window index → `"window not found: <idx>"`; bad new name → `"bad window name: <n>"` |
| `list-windows`/`lsw` | `[-t name]` (default: most recently created session) | `out` = one line per window, index order: `"{index}: {name}{flag} ({n} panes) [{w}x{h}]{active}"` (`flag` = `*`/`-`/empty; `active` = `" (active)"` iff current), `\n`-terminated | target given but not found → `find`'s error; no sessions at all → `{1,"","no sessions"}` |
| `detach-client` | `-s name` (required) | `{0,"",""}`; every client attached to that session gets `Exit{0,"[detached (from session <name>)]"}`, then a size/layout recompute | no `-s` → `"usage: detach-client -s target"`; not found → `find`'s error |

`rename-window`'s `[-t target[:idx]]`: a `:` splits into session part +
window-index part (`idx` parsed as `u32`); no `:` means the whole target is
the session name and the RENAME applies to that session's CURRENT window
(no default target at all, i.e. no `-t`, is still an error — the CLI has
no "current client" context to fall back on, unlike the interactive
prefix-key binding).

`ls`/`lsw`'s `{ctime}` is `%a %b %e %H:%M:%S %Y` (C-locale weekday/month,
`%e` = space-padded day, e.g. `Tue Jul  7 09:14:22 2026`). Implementation
(`to_local_systemtime`/`format_ctime`): convert the session's `SystemTime`
(`created`, or `SYSTEMTIME`-equivalent) to a Win32 `FILETIME`, call
`FileTimeToLocalFileTime` (UTC → local, applying timezone/DST) then
`FileTimeToSystemTime` (→ `SYSTEMTIME`, which carries `wDayOfWeek` — the
only reason for this two-hop conversion instead of a manual epoch-to-date
calculation is getting the weekday without hand-rolling Zeller's
congruence). No extra state is stored per session for this — `created` is
converted on demand each time `ls`/`lsw` runs, so it stays correct across a
`rename-session` (which does not touch `created`). Requires the
`Win32_System_Time` Cargo feature (`FileTimeToLocalFileTime` itself is
`Win32_Storage_FileSystem`, already enabled).

**Internal shape** (private; matches the design spec's sketch with one
documented simplification): `ServerEvent::{Output(PaneId, Vec<u8>),
Exited(PaneId), Connected(PipeConn), FromClient(ClientId, ClientMsg),
ClientGone(ClientId), Tick}`; `PaneRuntime{pty: Option<Pty>, grid: Grid,
dead: bool}`; `ClientState{session: Option<String>, cols: u16, rows: u16,
renderer: Renderer, input: InputMachine, mode: ClientMode, message:
Option<(String, Instant)>, tx: Sender<Vec<u8>>}`. `ClientMode` is now
`{Normal, ConfirmKillPane(PaneId), ConfirmKillWindow(WindowId), Prompt{
label: String, buf: String, kind: PromptKind }}` (`PromptKind::{
RenameWindow, RenameSession}`) — see the CLI/prompt/transient-message
paragraphs above for Task 7's wiring of the last three variants.

**A bug found and fixed along the way:** `src/pipe.rs`'s handles were not
overlapped (`FILE_FLAG_OVERLAPPED`), which meant a pending blocking read on
one `try_clone`'d duplicate serialized against (and forever blocked) a write
on another duplicate of the same connection — a real deadlock hit while
implementing this module's mandatory reader-thread/writer-thread-per-client
shape. Fixed in `pipe.rs` itself (see its `## pipe` section above and the
module's own doc comment); `PipeListener`/`PipeConn`'s public signatures are
unchanged. Regression coverage:
`tests/pipe_smoke.rs::write_on_one_clone_does_not_block_behind_a_pending_read_on_another`.

**Implementation module:** `src/server.rs`. Consumes `protocol`, `pipe`,
`model`, `input`, `render`, `status`, `pty`, `grid`, `layout`; produces only
`pub fn run`. Integration-tested end to end (no console, no ConPTY on the
client side) by `tests/server_proto.rs`'s 28 tests (9 from Task 6 plus 19
added in Task 7: window ops, prompts, transient messages, switch-client,
pane-exit auto-close, and the CLI subset).
