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
