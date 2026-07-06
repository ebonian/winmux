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
- `PipeListener::bind` calls `CreateNamedPipeW(name, PIPE_ACCESS_DUPLEX,
  PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES,
  64*1024, 64*1024, 0, None)` and **holds** the resulting instance — this
  proves the name is creatable before `bind` returns. The first `accept`
  consumes that held instance; every subsequent `accept` creates a fresh
  instance via the same `CreateNamedPipeW` call before waiting on it. Each
  `accept` blocks in `ConnectNamedPipe`; a client that connects in the race
  window before `ConnectNamedPipe` is called surfaces as `ERROR_PIPE_CONNECTED`,
  which `accept` treats as success rather than an error.
- `PipeConn::connect` calls `CreateFileW(name, GENERIC_READ | GENERIC_WRITE,
  0, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None)`. `ERROR_FILE_NOT_FOUND`
  (no server bound to that name) maps to `io::ErrorKind::NotFound` — the CLI's
  "no server running" signal. `ERROR_PIPE_BUSY` (instance limit momentarily
  exhausted) triggers one `WaitNamedPipeW(name, 2000)` wait followed by exactly
  one retry of `CreateFileW`.
- Reads map `ERROR_BROKEN_PIPE`/`ERROR_PIPE_NOT_CONNECTED` to `Ok(0)` (clean
  EOF), matching `src/pty.rs`'s `ERROR_BROKEN_PIPE` convention. Writes pass
  through `std::fs::File`'s `Write` impl unchanged.
- Windows-rs wraps a failed call's `GetLastError()` code as an HRESULT
  (`HRESULT_FROM_WIN32`); `pipe.rs` unwraps that back to the raw Win32 code
  before constructing an `io::Error` so `.kind()` classification (e.g.
  `NotFound`) matches what calling `GetLastError()` directly would give.

**Cargo.toml:** requires the `Win32_System_WindowsProgramming` feature (for
`GetUserNameW`), added alongside the already-enabled `Win32_System_Pipes`
(`CreateNamedPipeW`/`ConnectNamedPipe`/`WaitNamedPipeW`) and
`Win32_Storage_FileSystem` (`CreateFileW`).

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
- `Registry` allocates `WindowId`s itself only for a session's initial
  window (via `create_session`'s internal `next_window_id` counter).
  `Session::new_window` takes the id as a parameter — a future task that
  adds a "create window on session" entry point on `Registry` is
  responsible for minting that id from the same counter.
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
