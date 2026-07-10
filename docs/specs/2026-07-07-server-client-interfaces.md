# Sub-project 2 — Server/client split — Locked Interface Contract

**Status:** Locked, extended task-by-task. Every implementation task MUST
conform to these types and signatures exactly. If a signature must change
during implementation, the change must be applied consistently to every
consumer named here (same rule as the MVP contract).

**Parent spec:** [`2026-07-07-server-client-design.md`](2026-07-07-server-client-design.md)

**SUPERSEDED input section (sub-project 3, Task 6):** every reference below
to `InputMachine`/`InputEvent`/`Action` (the `Server`/`ClientState` "Input
routing" narrative, `ClientMode::ConfirmKillPane`/`ConfirmKillWindow`, and
the CLI subset executed by the now-deleted `src/server/cli_exec.rs`)
describes sub-project 2 behavior ONLY. `src/server.rs` was rewired in Task 6
onto the table-driven `KeyMachine`/`Bindings`/command-dispatcher pipeline —
see `docs/specs/2026-07-07-command-config-interfaces.md`'s `## input-v2`,
`## bindings`, and `## server-dispatch` sections for the current (locked)
contract. `ClientMode::ConfirmKillPane(PaneId)`/`ConfirmKillWindow(WindowId)`
became the single `ConfirmCmd { prompt, cmds, pane_snapshot,
window_snapshot }` variant; the CLI subset's exact output strings/usage
lines are preserved byte-for-byte (see that file's `execute_cli_argv` and
`cmd.rs`'s `sp2_usage_strings_match_cli_exec_verbatim` test) but now flow
through `cmd::resolve` + the unified dispatcher instead of `cli_exec.rs`'s
hand-rolled `CliArgs` parser. This section is kept for historical reference.

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
  FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_TYPE_BYTE |
  PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES, 64*1024,
  64*1024, 0, None)` and **holds** the resulting instance — this proves the
  name is creatable before `bind` returns.
  **`FILE_FLAG_FIRST_PIPE_INSTANCE` (Task 8 amendment; bind-time call
  ONLY):** if ANY instance of the name already exists (another server owns
  it), the create fails with `ERROR_ACCESS_DENIED`, surfaced as
  `io::ErrorKind::PermissionDenied` (the natural raw-Win32-code mapping;
  chosen over remapping to `AlreadyExists` so `pipe.rs` keeps its uniform
  "raw code straight through `from_raw_os_error`" rule). Without the flag,
  `PIPE_UNLIMITED_INSTANCES` lets a SECOND process's `CreateNamedPipeW` on
  the same name succeed, silently joining the first server's instance pool
  — two servers would round-robin accepts on one name (split-brain). That
  is exactly the double-autostart race two concurrent cold-start clients
  produce (both find no pipe, both spawn a server): the loser's `bind` now
  fails, `server::run` returns `Err`, `main.rs`'s ServerRole logs it and
  exits 1, and the losing client's autostart connect-poll simply finds the
  winner's pipe. Regression coverage:
  `tests/pipe_smoke.rs::second_bind_same_name_fails` (second `bind` errors
  `PermissionDenied`; the first listener still accepts and serves).
  The first `accept` consumes the instance `bind` held; every subsequent
  `accept` creates a fresh instance via the same `CreateNamedPipeW` call —
  WITHOUT `FILE_FLAG_FIRST_PIPE_INSTANCE` (the name legitimately exists by
  then; it's the same server adding instances) — before waiting on it. Each `accept` blocks in `ConnectNamedPipe` (issued
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
    // AMENDED (SP7 Task 6, closes follow-up #26): this window's local
    // window-scoped option overrides (`setw`/`set -w`, no `-g`), starting
    // empty (inherits the global table). See the `## option-scopes` section
    // of 2026-07-07-command-config-interfaces.md for `options::Overlay`.
    pub window_options: crate::options::Overlay,
}
pub struct Session {
    pub name: String,
    pub created: std::time::SystemTime,
    pub windows: Vec<Window>,        // kept sorted by index
    pub current: WindowId,
    pub last: Option<WindowId>,
    pub size: (u16, u16),            // current window size (smallest attached client)
    // base_index: u32,              // NOT pub (Task 7 amendment, see below)
    // AMENDED (SP7 Task 6, closes follow-up #26): this session's local
    // session-scoped option overrides (unprefixed `set`, no `-g`),
    // starting empty. Same `options::Overlay` type as Window's.
    pub session_options: crate::options::Overlay,
}
pub struct Registry { /* sessions: Vec<Session> in creation order, next_window_id */ }
impl Registry {
    pub fn new() -> Registry;
    pub fn create_session(&mut self, name: Option<&str>, first_pane: crate::layout::PaneId, size: (u16, u16), base_index: u32)
        -> Result<&mut Session, String>;                    // Err("duplicate session: <name>")
    pub fn find(&mut self, target: &str) -> Result<&mut Session, String>; // tmux -t rules: "=x" exact only; else exact, then unambiguous prefix; Err("can't find session: <t>")
    pub fn kill_session(&mut self, name: &str) -> bool;
    pub fn sessions(&self) -> &[Session];
    pub fn session_mut(&mut self, name: &str) -> Option<&mut Session>;
    pub fn is_empty(&self) -> bool;
    pub fn auto_name(&self) -> String;                      // lowest unused non-negative integer as string
    pub fn neighbor_session(&self, current: &str, next: bool) -> Option<&str>; // for ( / ) switch-client, wraps
    pub fn mint_window_id(&mut self) -> WindowId;           // fresh id from the SAME counter create_session uses internally
    // Amendment (SP7 Task 11, cross-window/session structure ops) -- see below.
    pub fn move_window_to_session(&mut self, src_name: &str, id: WindowId, dst_name: &str, index: Option<u32>, kill: bool, select: bool) -> Result<(), String>;
    pub fn insert_new_window(&mut self, session_name: &str, id: WindowId, first_pane: crate::layout::PaneId, index: Option<u32>) -> Result<WindowId, String>;
}
impl Session {
    pub fn new_window(&mut self, id: WindowId, first_pane: crate::layout::PaneId) -> &mut Window; // index = lowest unused, becomes current, updates last
    pub fn kill_window(&mut self, id: WindowId) -> bool;    // removes; retargets current to last-or-nearest; false if it was the only window
    pub fn select_window(&mut self, index: u32) -> bool;    // exact index; updates current/last
    pub fn next_window(&mut self) / pub fn prev_window(&mut self); // wrap by index order
    pub fn last_window(&mut self) -> bool;
    pub fn current_window(&self) -> &Window; pub fn current_window_mut(&mut self) -> &mut Window;
    pub fn window_by_pane(&mut self, pane: crate::layout::PaneId) -> Option<&mut Window>;
    // Amendment (sub-project 6, Task 5, `swap-window`) -- see below.
    pub fn window_relative(&self, from: WindowId, offset: i64) -> Option<WindowId>;
    pub fn swap_windows(&mut self, src: WindowId, dst: WindowId, detach: bool) -> bool;
}
```

**Behavior notes:**

- **Amendment (Task 7, SP3 config loading):** `create_session` gains a
  `base_index: u32` parameter (tmux `base-index`; the caller — `server.rs`/
  `server/dispatch.rs` — samples `Options::base_index()` at each call site).
  The session's first window is created at `index: base_index` instead of a
  hardcoded `0`, and the value is stashed on a NEW private (not `pub`)
  `Session::base_index` field so every LATER `Session::new_window` on that
  same session also floors its "lowest unused index" search at `base_index`
  (a window killed and re-created never renumbers back down below it). Only
  the session's OWN `base_index`, fixed at creation time, is used — changing
  `set -g base-index` after a session already exists does not renumber its
  existing windows (tmux behavior). All three call sites (`server.rs`'s
  `NewAuto`/`NewNamed` attach paths, `server/dispatch.rs`'s
  `exec_new_session`) pass `self.options.base_index()`. `lowest_unused_index`
  (private free fn) gained a matching `base: u32` parameter (search starts
  at `base` instead of `0`). **`Session::renumber()` (Task 7 review fix,
  Critical)** also respects the same floor: it reassigns every window's
  `index` to `base_index + position` (in the already-index-sorted `windows`
  order), NOT `0 + position` — real tmux renumbers from `base-index`, so
  `base-index 1` + `renumber-windows on` never produces a window 0. The
  session's own stored `base_index` is used (no parameter needed at the
  `renumber` call sites in `server/dispatch.rs`, which is why storing the
  base on `Session` — rather than passing it through every call — was the
  cleaner wiring). Tests: `model.rs`'s
  `base_index_offsets_window_numbering` / `renumber_respects_base_index`;
  end to end, `tests/server_proto.rs::config_file_applies_at_startup`
  (`set -g base-index 1` at server startup -> the auto session's first
  window renders `1:powershell*`, not `0:powershell*`) and
  `renumber_windows_with_base_index` (base-index 1 + renumber-windows on
  from a config file; killing window 1 renumbers the survivor to 1, never
  0).
- Session/window name validation: reject empty names, names containing `:`
  or `.` (tmux's target separators), and — final-review fix, 2026-07-07 —
  any control character (C0 incl. `\n`/`\r`/ESC, plus 0x7f DEL) →
  `Err("bad session name: <n>")` / `Err("bad window name: <n>")` (`noun` is
  a parameter of the shared `validate_name(name, noun)` helper). Control
  chars are rejected because an unvalidated name reaches status-bar span
  text written raw to the host terminal (frame corruption) and breaks
  line-oriented `ls` output parsing; the interactive rename prompt already
  only appends printable ASCII 0x20-0x7e, but the CLI argv path
  (`new-session -s`, `rename-session`/`rename-window` positional arg) does
  not, so this is the only enforcement point for that path. The name echoed
  in the error is sanitized separately from the check itself (every control
  char -> `?`), so the rejection message can't re-inject the same bytes
  into stderr/status text. `create_session` calls `validate_name` directly;
  `crate::server::cli_exec::validate_target_name` (used by the CLI
  rename-session/rename-window handlers and by `server.rs`'s
  `feed_prompt_byte` rename-prompt commit) is a thin wrapper over the same
  function, so all three call sites share one rule. There is no rename API
  on `Registry`/`Session` itself yet — renames mutate the public `name`
  field directly from `server`/`cli_exec` after validating.
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
- **Amendment (Task 8):** an EMPTY `target` is a special case checked before
  anything else in `find` — it resolves to the most recently created
  session (`sessions().last()`, i.e. `Registry`'s creation-order `Vec`), or
  `Err("no sessions")` if the registry is empty. This is NOT the same as an
  always-matching empty-string prefix (which the old prefix-matching branch
  would otherwise produce when 2+ sessions exist — an ambiguity error).
  Added to support `attach-session`/`Attach{mode: Existing, ...}` with no
  `-t`/target: the CLI has no "current client" context to fall back on
  (unlike the interactive prefix-key bindings), so the server picks the
  newest session instead. Test: `Registry`'s own
  `find_empty_target_picks_most_recent` (multiple sessions; asserts
  creation-recency, not name order or ambiguity) /
  `find_empty_target_no_sessions_is_error` (`src/model.rs`), plus
  `tests/server_proto.rs::attach_empty_target_picks_most_recent` exercising
  it end-to-end over the wire.
  **Blast-radius audit (Task 8 review):** every other `find()` caller is a
  `cli_*` handler in `src/server/cli_exec.rs`, and an OMITTED flag never
  produces an empty-string call — `has-session`/`rename-session`/
  `rename-window`/`detach-client` require the flag (usage error before
  `find` is reached), and `kill-session`/`list-windows` default via
  `sessions().last()` directly. The only way `""` reaches `find` on the
  CLI path is an EXPLICITLY empty flag value (`-t ""`, `-s ""`, or the
  session part of `rename-window -t ":idx"`), which now resolves to the
  most recently created session — intentionally the SAME resolution as
  `kill-session`/`list-windows`'s documented no-`-t` default, giving one
  uniform rule: an empty target anywhere means "the most recently created
  session" (`"no sessions"` if none). Pinned by
  `tests/server_proto.rs::cli_empty_target_resolves_most_recent`
  (`has-session -t ""` and `kill-session -t ""` against two sessions).
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
- **Amendment (sub-project 6, Task 5, `swap-window`):** two new `Session`
  methods, both pure bookkeeping (no I/O), backing
  `server::dispatch::exec_swap_window` (`## server-dispatch` section of the
  sibling `command-config` contract). Full behavioral spec:
  `docs/tmux-reference/windows-and-sessions.md` §swap-window.
  - `window_relative(&self, from: WindowId, offset: i64) -> Option<WindowId>`:
    the window `offset` slots after (positive) / before (negative) `from` in
    INDEX order, WRAPPING at either end (`self.windows` is already
    index-sorted by every mutator, so this is a plain modular walk over the
    vector — the pure-data equivalent of tmux's `winlink_next_by_number`/
    `winlink_previous_by_number` winlink-tree walk). `None` if `from` isn't
    a live window in this session. A single-window session returns `from`
    itself for any offset (0-step wrap). This is `swap-window`'s `-1`/`+1`
    relative-target grammar (the user's real `bind -r "<" swap-window -d -t
    -1` binding).
  - `swap_windows(&mut self, src: WindowId, dst: WindowId, detach: bool) -> bool`:
    exchanges `src`'s and `dst`'s `index` values in place (`self.windows`
    stays sorted afterward) — each window OBJECT (id, name, layout,
    `last_layout`, `auto_rename` state) keeps its own identity; only which
    index it sits at trades places, mirroring tmux's actual mechanism
    exactly ("the two winlinks stay at their indexes; their `->window`
    pointers are exchanged") since winmux has no separate winlink type (the
    `index` field on the id-keyed `Window` doubles as that identity here).
    `false` (no-op, nothing changed) if `src == dst`, or either id isn't a
    live window in this session — mirrors tmux's own "no-op success if both
    winlinks already point at the same window" rule.
    Also resolves `current`/`last`, since winmux tracks both by `WindowId`
    (content) where tmux tracks them by winlink (slot/index) — the two only
    coincide when a window's index never changes, which the swap explicitly
    violates for `src`/`dst`. Let `flip(id)` map `src`↔`dst` and pass any
    other id through unchanged:
    - `detach == false` (no `-d`): tmux leaves `curw` (the SLOT pointer)
      untouched, so whatever the slot now displays is a different window —
      `current`/`last`, if they named `src` or `dst`, FLIP to the other id
      (same slot, new content); anything else is untouched.
    - `detach == true` (`-d` given): tmux calls `session_select(dst_session,
      wl_dst->idx)` — select BY THE FIXED INDEX that was `dst`'s, which
      post-swap is now occupied by `src`. When that reselect actually
      changes the current slot (i.e. pre-swap `current != dst`) this makes
      `src` the new `current`, and — mirroring `session_set_current`'s
      "push the OLD curw onto the lastw stack" step — sets `last` to
      `flip(current)` as it stood BEFORE this call (the same-slot post-swap
      content of whichever window was current going in). **EXCEPT** (review
      fix, Task 5 round 1): when pre-swap `current == dst`, the reselect
      target (dst's original slot, now showing `src`) IS the current slot,
      and `session_set_current` early-returns (`if (wl == s->curw) return
      1;`, session.c:475-498) without touching curw or lastw — the whole
      `-d` select is a no-op, so the bookkeeping degenerates to exactly the
      no-`-d` rule above (`current` flips dst → src via the same
      slot-content logic; `last` flips only if it named `src`/`dst`, and is
      otherwise left untouched — never overwritten).
    Tests: `model.rs`'s `swap_windows_exchanges_indices_keeps_ids` (pure
    index exchange; unrelated `current` untouched, related `last` flips),
    `swap_windows_without_detach_flips_current_to_other_window`,
    `swap_windows_with_detach_keeps_focus_on_source_window`,
    `swap_windows_detach_when_current_is_dst_preserves_last` /
    `swap_windows_detach_when_current_is_dst_flips_src_named_last` (the
    early-return case, round-1 review fix),
    `swap_windows_same_id_or_unknown_id_is_noop`, and `window_relative_wraps`;
    end to end, `tests/server_proto.rs`'s
    `swap_window_relative_target_moves_current_window` /
    `swap_window_without_d_keeps_focus_on_index`.
- **Amendment (SP7 Task 11, cross-window/session structure ops, closes
  follow-up #45):** `Registry` gains a cross-session `move-window`
  primitive, plus two supporting building blocks (one on `Registry`, two
  new PRIVATE `Session` methods that are NOT part of this locked contract
  — listed here only for context, since `move_window_to_session` is
  implemented in terms of them):

  ```rust
  impl Registry {
      /// Lift window `id` wholesale out of session `src_name` and insert it
      /// into session `dst_name` at `index` (explicit) or `dst_name`'s
      /// lowest free slot. The `Window` OBJECT (id, name, layout, every
      /// pane) moves untouched -- `WindowId`s are global, never re-minted.
      /// `select: true` makes the moved window `dst_name`'s new `current`
      /// (mirrors `move-window`'s "no -d" default; `last` becomes whatever
      /// WAS current there). `kill: true` removes an occupied `index`'s
      /// occupant first (same `-k` contract `Session::move_window` already
      /// has for the same-session case) -- the CALLER is responsible for
      /// cleaning up the killed occupant's pane runtime state (same
      /// pre-snapshot-then-remove pattern `server/dispatch.rs::exec_move_
      /// window` already follows for the same-session `-k` case).
      /// **Narrowing** (documented, follow-up #45's own honest-scope note):
      /// refuses (`"can't move the only window out of its session"`) rather
      /// than destroying an emptied source session the way real tmux does
      /// -- avoids this task also solving session-teardown client-eviction
      /// semantics for a case with no bearing on the required behavior.
      /// Errors: `"can't find session: <name>"` (either session missing),
      /// `"window not found"`, `"can't move the only window out of its
      /// session"`, `"index in use: <i>"` (explicit index occupied, `kill:
      /// false`), `"move-window: source and destination sessions are the
      /// same"` (route a same-session move through `Session::move_window`
      /// instead -- this method does not duplicate it).
      pub fn move_window_to_session(&mut self, src_name: &str, id: WindowId, dst_name: &str, index: Option<u32>, kill: bool, select: bool) -> Result<(), String>;

      /// Build a fresh single-pane `Window` (same defaults as `Session::
      /// new_window`) and insert it into session `session_name` at `index`
      /// (explicit) or its lowest free slot -- `break-pane -t
      /// <session[:index]>`'s primitive (closes follow-up #44), since
      /// `Session::new_window` itself only ever targets its OWN session and
      /// always forces focus onto the new window. Does NOT touch
      /// `current`/`last`. `id` must come from `mint_window_id`. Errors:
      /// `"can't find session: <session_name>"` / `"index in use: <i>"`.
      pub fn insert_new_window(&mut self, session_name: &str, id: WindowId, first_pane: crate::layout::PaneId, index: Option<u32>) -> Result<WindowId, String>;
  }
  ```

  Both delegate to two new PRIVATE `Session` methods (`insert_window`,
  `take_window`) plus a private `Session::build_window` associated
  function that `new_window` was refactored to share — `new_window`'s own
  signature/behavior is UNCHANGED (still index = lowest unused, becomes
  current, updates last). Full spec:
  `docs/tmux-reference/windows-and-sessions.md` §move-window/link-window,
  §break-pane. `server::dispatch`'s consumers
  (`exec_move_window`'s cross-session branch, `exec_break_pane`) are
  documented in the `## Cross-window/session structure ops` section of
  [`2026-07-07-command-config-interfaces.md`](2026-07-07-command-config-interfaces.md).
  Tests: `model.rs`'s `move_window_across_sessions_reindexes_destination`,
  `move_window_across_sessions_explicit_index`,
  `move_window_across_sessions_occupied_index_errors`,
  `move_window_across_sessions_kill_occupant`,
  `move_window_across_sessions_refuses_to_empty_source`,
  `move_window_across_sessions_unknown_session_or_window_errors`,
  `move_window_across_sessions_same_name_errors`; end to end,
  `tests/server_proto.rs`'s
  `move_window_to_other_session_appears_there_and_leaves_source`.

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

## `status` — status-line span builder (pure, Task 5; SIGNATURE AMENDED SP3 Task 8, SP6 Task 4, SP7 Task 7, SP7 parity wave 3 fix)

```rust
// status.rs
pub struct WindowEntry {
    pub index: u32,
    pub name: String,
    pub current: bool,
    pub last: bool,
    pub zoomed: bool,
    // AMENDED (SP7 Task 6, closes follow-up #26): per-window EFFECTIVE
    // window-status-format/-current-format and -style/-current-style,
    // resolved by the CALLER through that window's own options overlay
    // (`Options::window_status_*_for`). `None` falls back to the shared
    // `window_format`/`window_current_format`/`win_style`/
    // `win_current_style` arguments below — byte-identical to pre-SP7
    // behavior, which is what keeps every earlier status test green
    // unmodified. `server::render_one` always passes `Some(..)` (the
    // `_for` getters already fold in the global fallback).
    pub format_override: Option<String>,
    pub style_override: Option<crate::style::PartialStyle>,
    // AMENDED (SP7 Task 7, closes follow-up #71): THIS window's own active
    // pane's `#P`/`pane_index` value, already `pane-base-index`-shifted by
    // the caller (same rule `render_one` applies for the shared `ctx`
    // argument to `status_spans` below). Every pre-Task-7 caller/test gets
    // `0` here (the correct value for a lone default pane), so every
    // earlier test's expected spans are unchanged.
    pub pane_index: u32,
    // THIS window's own active pane's `#T`/`pane_title` value. Empty string
    // for every pre-Task-7 caller/test.
    pub pane_title: String,
}

// SP6 Task 4 signature (status-justify, per-side styles, per-window
// window-status-format/-current-format expansion, window-status-separator —
// see the amendment below):
pub fn status_spans(
    left: &str,                                      // pre-expanded, pre-length-capped status-left text
    left_style: &crate::style::PartialStyle,          // status-left-style
    windows: &[WindowEntry],
    ctx: &crate::options::FormatCtx,                  // session/pane/hostname/time; window_index/name/flags overridden per window
    window_format: &str,                              // window-status-format (raw, NOT pre-expanded)
    window_current_format: &str,                      // window-status-current-format (raw, NOT pre-expanded)
    base: crate::grid::Style,                         // status-style applied to Style::default()
    win_style: &crate::style::PartialStyle,           // window-status-style
    win_current_style: &crate::style::PartialStyle,   // window-status-current-style
    separator: &str,                                  // window-status-separator
    justify: &str,                                    // status-justify: "left"/"centre"/"right"/"absolute-centre"
    width: u16,                                        // terminal column count
    right_len: usize,                                  // char count of the (already capped, already stripped) status-right text
) -> Vec<(String, crate::grid::Style)>;

/// Strip `#[...]` inline style markers from `expand_format`-expanded text,
/// keeping only the literal text — used for `status-right`, whose
/// `render::StatusRow::right` field has only ONE style slot (no room for
/// multiple inline-styled sub-runs the way `status_spans`'s returned `Vec`
/// has for `left`/the window list).
pub fn strip_style_markers(text: &str) -> String;

/// NEW (SP7 Task 7, closes follow-up #69b). Truncate `text` (already
/// `expand_format`-expanded, so it may still carry literal `#[...]` markers)
/// to `max` VISIBLE characters: markers themselves count as zero width and
/// are never bisected (emitted whole if the visible budget isn't exhausted
/// yet when reached, otherwise dropped whole along with everything after).
/// Used for `status-left` (which — unlike `status-right` — keeps its
/// markers all the way through so `status_spans`'s `styled_runs` can still
/// split it into differently-styled runs); `status-right` keeps its
/// existing `strip_style_markers`-then-plain-char-count-cap treatment
/// unchanged (no markers survive to bisect by the time it's capped).
pub fn truncate_visible(text: &str, max: u16) -> String;

/// NEW (SP7 parity wave 3 fix, review of 128cfc0). One window's tab column
/// span in the FINAL rendered status row (0-based, `end` exclusive).
/// `window_pos` is the window's position in the `windows` slice passed to
/// `status_tab_columns`/`status_spans` — NOT its tmux `#I` index. A window
/// scrolled fully off-screen under overflow scrolling has no entry.
pub struct TabColumn {
    pub window_pos: usize,
    pub start: u16,
    pub end: u16,
}

/// NEW (SP7 parity wave 3 fix, review of 128cfc0, closes the `mouse_status_
/// click` hit-test regression). Column ranges a status-row click hit-test
/// can map through: mirrors `status_spans`'s layout exactly (same private
/// `window_tab_texts`/`plan_tab_layout` core, so it's byte-for-byte what's
/// actually drawn, including window-list overflow scrolling and its `<`/`>`
/// markers) but skips style resolution entirely, since a click doesn't care
/// what color a tab is.
pub fn status_tab_columns(
    left: &str,
    windows: &[WindowEntry],
    ctx: &crate::options::FormatCtx,
    window_format: &str,
    window_current_format: &str,
    separator: &str,
    justify: &str,
    width: u16,
    right_len: usize,
) -> Vec<TabColumn>;
```

**AMENDMENT (SP7 parity wave 3 fix — review of 128cfc0):** `mouse_status_click`
(`src/server/dispatch.rs`, see `## server-dispatch` below) used to
reconstruct the tab hit-boxes ITSELF — walking `session.windows` in original
order starting right after `status-left`, using its own hardcoded
`"{index}:{name}{flags}"` text-length guess — which predated (and was left
untouched by) SP7 Task 7's window-list overflow scrolling in `status_spans`.
Once a real window list actually scrolled, the two disagreed: a click on the
visually-current tab could resolve to the WRONG window. The fix factors the
LAYOUT MATH (not the styling) out of `status_spans` into two private
helpers — `window_tab_texts` (per-window expanded text + visible width,
format-override-aware) and `plan_tab_layout` (the justify/scroll/marker
decision, given only widths) — that `status_spans` and the new public
`status_tab_columns` both call, so there is exactly one place that decides
what's visible where. `status_spans`'s own behavior/output is byte-for-byte
unchanged (every pre-existing test still passes unmodified); only its
internal structure changed. Two new unit tests close a documented edge-case
gap: `overflow_boundary_exactly_fits_no_markers_no_scroll` (`list_width ==
list_avail` exactly must take the fit branch, not overflow) and
`single_window_wider_than_budget_overflows_with_both_markers` (a single tab
wider than the entire budget still overflows correctly with both markers
when the visible sliver lands strictly inside it). A third,
`status_tab_columns_matches_rendered_overflow_scroll`, proves the new
function's columns agree with `status_spans`'s rendered row for the exact
same fixture as `window_list_scrolls_to_keep_current_visible_with_markers`.

**AMENDMENT (SP3 Task 8, historical):** the original Task 5 signature was
`status_spans(session_name: &str, windows: &[WindowEntry]) ->
Vec<StatusSpan>`, hardcoding the `[<session>] ` prefix and an underline-only
current-window marker. SP3 Task 8 replaced it with the ALREADY-EXPANDED
`status-left` text plus `base`/`win_style`/`win_current_style`, returning
fully resolved styles per span.

**AMENDMENT (SP6 Task 4 — status-justify, side styles, window formats,
separator):** `status_spans` gained `left_style`, `ctx`, `window_format`,
`window_current_format`, `separator`, `justify`, `width`, `right_len`.
`status.rs` now depends on `crate::options` (`expand_format`/`FormatCtx`) —
still pure (no I/O), `expand_format` is pure too.

- **Per-window format expansion:** for each window, `window_current_format`
  (if `current`) else `window_format` is expanded via `options::
  expand_format` against a `FormatCtx` built from `ctx`'s
  session/hostname/now fields, with `window_index`/`window_name`/
  `window_flags` overridden to that window's own values (`window_flags`
  uses the SAME flags-string rule as before — see below, now fed through
  `#F` rather than hardcoded string concatenation) and, as of **SP7 Task 7**
  (closes follow-up #71), `pane_index`/`pane_title` ALSO overridden to that
  window's own `WindowEntry::pane_index`/`pane_title` rather than `ctx`'s
  (which only ever carried the acting client's focused pane in the CURRENT
  window — reused for every other window's tab before this fix, so `#P`/`#T`
  in a per-window format used to misrender for every non-focused window).
- **Inline `#[...]` style markers:** `expand_format` (SP6 Task 4 addition,
  see the command-config contract amendment) passes `#[...]` blocks through
  VERBATIM rather than interpreting them. `status.rs`'s private
  `styled_runs` then splits each window's (and `left`'s) expanded text on
  those markers into multiple `(text, Style)` sub-spans, additively layering
  each marker's `style::parse_style`-parsed style onto that section's base
  style (the tab's `win_style`/`win_current_style`-over-`base`, or `left`'s
  `left_style`-over-`base`). Text with no markers — the common case —
  yields exactly one span, byte-identical to the pre-Task-4 output. A
  malformed marker (no closing `]`, or content `parse_style` rejects) is a
  no-op/literal-text fallback, never a panic.
- **`status-justify` positioning (fits case):** when the window list fits
  its allotted budget (`list_width <= width - left_width - right_len`), the
  window-list group's start column is computed by a private `list_offset`
  helper per `docs/tmux-reference/status-line-and-messages.md` §1.4's
  closed-form offsets (winmux has no user-configurable centre/after content,
  so the general 8-screen trim-order engine collapses to `left`/`centre`/
  `right`/`absolute-centre` formulas keyed off `left`'s width, `right_len`,
  and the list's own total width). The gap between `left` and the list start
  is realized as a literal run of `base`-styled padding spaces inserted into
  the returned `Vec` — `render::compose_back` is UNCHANGED, it still just
  draws spans sequentially from column 0 (see `## render-styles`); this is
  why no `render.rs`/`Scene`/`StatusRow` signature changed for this task.
- **Window-list overflow scrolling (SP7 Task 7, closes follow-up #69a):**
  when the list does NOT fit (`list_width > list_avail`, where `list_avail =
  width - left_width - right_len`), `status_spans` no longer clamps to
  zero pad and overlaps/overruns. Instead it flattens the full unclipped
  tab+separator sequence to one `(char, Style)` per column, locates the
  CURRENT window's column range (`focus_start`/`focus_end`) within it,
  computes `focus_centre = focus_start + (focus_end - focus_start) / 2`, and
  scrolls a `content_w`-wide window (`content_w = list_avail` minus 1 column
  per marker actually needed) centered on `focus_centre`, clamped so it
  never runs past either end of the full list. Whether the left/right marker
  is needed is resolved by a capped (4-pass) fixed-point search — reserving
  a marker shrinks `content_w`, which can flip whether the OTHER end is
  still off-screen. `<` is prepended when content is scrolled off the left
  (`start > 0`); `>` is appended when content remains beyond the right
  (`start + content_w < list_width`); both markers are drawn in the row's
  plain `base` style. This is a documented simplification of tmux's own
  eight-screen `format_draw` model (winmux scrolls the whole
  left/list/right block as one fixed budget rather than reproducing every
  justify mode's own trim order for the overflow case specifically) — see
  the SP7 Task 7 report for the exact ruling. No padding is ever inserted in
  this branch: an overflowing list, by definition, consumes its entire
  allotted budget.
- **`window-status-separator`** replaces the old hardcoded `" "` between
  tabs (still omitted after the last one).
- **`status-right`'s style** is resolved by the SERVER directly
  (`options::status_right_style().apply_to(base)`, assigned to
  `render::StatusRow::right_style`) since `right` has only one style slot;
  any `#[...]` markers `expand_format` leaves in the expanded `status-right`
  text are removed via `strip_style_markers` before length-capping and
  assignment, rather than leaking literal `#[...]` bytes onto the screen.
- **`status-left`'s length cap (SP7 Task 7, closes follow-up #69b):**
  `server::render_one` caps `status-left` with the NEW `status::
  truncate_visible` instead of a plain char-count `truncate_chars` — it
  counts only characters outside a `#[...]` marker toward
  `status-left-length`'s budget and never bisects a marker (whole-marker
  in-or-out, never partial), so `status-left` (which keeps its markers,
  unlike `status-right`) can't have its visible-width budget miscounted by
  bytes that draw zero columns. `status-right` is unaffected — it strips
  markers BEFORE capping, so there's nothing left to bisect by the time its
  existing `truncate_chars` cap runs.

**Flags string** for a window (exact rule — resolves the apparent ambiguity
between "else a literal space" and "else empty" phrasings that circulated
during design; empty is correct and is what's implemented/tested):
- `*` if `current`, else `-` if `last`, else nothing (empty).
- Then `Z` appended if `zoomed`.
- So: current+zoomed → `*Z`; last+zoomed → `-Z`; zoomed only (neither current
  nor last) → `Z` (e.g. window 2 named `logs` renders `2:logsZ`); no flags at
  all → the flags string is empty.
- **Default-format padding shim (SP6 Task 4 fix round 1):** when the
  effective format for a tab IS `options::DEFAULT_WINDOW_STATUS_FORMAT`
  (`#I:#W#F`, a public const), an EMPTY flags string is padded to a single
  space before expansion — reproducing the `, }` else-branch of tmux's real
  default `#I:#W#{?window_flags,#{window_flags}, }`, so a flagless window
  renders `2:logs ` (trailing space) and every tab's width is stable across
  focus changes, byte-identical to real tmux's default rendering. CUSTOM
  formats are never padded — `#F` expands to the plain (possibly empty)
  flags string, exactly what tmux's `#{window_flags}` would substitute.
  Tests: `default_format_flagless_window_pads_one_space`,
  `custom_format_flagless_window_not_padded`.

**Render integration:** the server packs the returned spans into
`render::StatusRow` (base fill, right text/style, top flag from
`status-position`) — see the `## render-styles` section; `render::
compose_back` draws them left-to-right from column 0 with each span's own
resolved style, then the right text right-aligned; the "total left length"
used for right-truncation is the summed char count of all spans' text
(unchanged by this task — justify padding is just more span text).

**Implementation module:** `src/status.rs`, pure (no I/O, but now depends on
`crate::options`), unit-tested with exact expected `Vec<(String, Style)>`
span vectors (mirrors `render.rs`'s exact-VT-bytes test style), including
`custom_styles_layering` (layered-over-base, not over-win_style),
`status_justify_centre_positions_window_list`/`status_justify_right`/
`status_justify_absolute_centre` (exact offset math), `window_status_format_
expands_per_tab`/`window_status_current_format_used_for_current` (per-window
expansion + format selection), `side_styles_layer_over_status_style`,
`window_status_separator_respected`, and `inline_style_marker_in_window_
format` (SP6 Task 4). **SP7 Task 7** adds
`window_list_scrolls_to_keep_current_visible_with_markers`/
`overflow_markers_absent_when_list_fits` (window-list overflow, closes
#69a), `per_tab_ctx_uses_that_windows_active_pane_title` (per-window pane
context, closes #71), `status_left_length_cap_ignores_style_marker_bytes`
(`truncate_visible`, closes #69b), and
`status_left_inline_style_marker_splits_spans` (verify-and-mark evidence for
follow-up #31 — `status-left`'s own text, not just a window tab's, really
does split into multiple styled spans on an inline `#[...]` marker).
`tests/server_proto.rs::status_interval_refreshes_seconds_format` is the
end-to-end proof for `status-interval`-driven refresh (closes #29, in
`## server` below).

## `server` — headless multiplexer server (Task 6; `run` amended Task 7)

```rust
pub fn run(pipe_full_name: &str, config_files: &[String]) -> Result<(), Box<dyn std::error::Error>>;
```

**Amendment (Task 7, SP3 config loading):** `run` gained a `config_files:
&[String]` parameter — the server role's `--config <path>` args (repeatable,
in order; forwarded from the CLI's `-f`, see the `## config` section of the
sibling `2026-07-07-command-config-interfaces.md` contract for the full
discovery/loading design). An empty slice means "use the default
`.tmux.conf`/`.winmux.conf` discovery chain"; non-empty REPLACES that chain
entirely. `main.rs` is the only real caller
(`server::run(&pipe, &config)`); `tests/server_proto.rs`'s `start_server`
helper now calls `server::run(&name, &[])` (a separate
`start_server_with_config` helper is used by the Task 7 config tests).

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
- **`status-interval`-driven status refresh (SP7 Task 7, closes follow-up
  #29):** `Server` gained a `last_status_render: Instant` field (init
  `Instant::now()`). On every `Tick`, in addition to the pre-existing
  minute-granularity `clock`-changed check, `handle_event` now ALSO checks
  `options.status_interval() > Duration::ZERO && deadline.duration_since
  (last_status_render) >= options.status_interval()`; if true, it sets
  `last_status_render = deadline` and `dirty = true`. This is a periodic
  refresh independent of whether the status TEXT actually changed —
  matching real tmux's own per-client status timer
  (`docs/tmux-reference/status-line-and-messages.md` §8), so a custom
  `status-right` with sub-minute-sensitive content (`%S`, a fast-changing
  pane title, etc.) now re-renders on the configured cadence rather than
  only when the coarse `HH:MM` clock string happens to flip. One
  server-global timer, not per-session/per-client (`status-interval` has no
  `_for` scope-resolving getter yet, consistent with several other
  session-scoped options that stayed global-only through SP7 Task 6 — see
  follow-up #75/#76's framing). `status_interval() == 0` means "never
  re-arm" (tmux's documented "0 = no periodic refresh"), so it never sets
  `dirty` on its own in that case. Test:
  `tests/server_proto.rs::status_interval_refreshes_seconds_format`.
- **Startup config loading (Task 7, SP3 config loading):** after binding the
  pipe and spawning the accept thread, but BEFORE entering the event loop
  (so no attach is ever served against an unconfigured `Options`/`Bindings`),
  `run` discovers and loads `.tmux.conf`/`.winmux.conf`/`--config` files
  through the same headless command-dispatch path `source-file` uses. Any
  collected errors are logged (`crate::logging::log_line`, one summary line
  `config: N error(s)` plus one line per error) and stashed in
  `Server::pending_config_message` (`Option<String>`, the exact text `config:
  N error(s), see server.log`), which `finish_attach` `take()`s into the
  FIRST client's `ClientState::message` — so only that one attach (across
  ALL sessions/modes, not per-session) ever sees it. Full discovery/loading
  design (env-var chain, `required` vs. silently-skipped-if-missing,
  `discover_config_files`/`ConfigCandidate`/`Server::load_config_files`) is
  in the sibling `2026-07-07-command-config-interfaces.md` contract's `##
  config` section (that's also where `source-file`'s runtime re-use of the
  same loader lives, superseding this file's stale placeholder text below).
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
  set — sends every OTHER client currently attached to that session
  `Exit{0, "[detached (from session <name>)]"}` (follow-up #17, SP7 Task 4:
  previously a bare `[detached]` with no session name; now identical text to
  the named `Detach`-action/-frame message) before attaching the new one.
  First window is always index 0,
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
client side) by `tests/server_proto.rs`'s 32 tests: 9 from Task 6, 19 added
in Task 7 (window ops, prompts, transient messages, switch-client,
pane-exit auto-close, and the CLI subset) plus a few more added during
Task 7's review-fix passes, and Task 8's
`attach_empty_target_picks_most_recent` covering the empty-target
`Existing` attach amendment above.

**LOCKED-CONTRACT AMENDMENT (SP7 Task 4 — follow-up #14, per-pane writer
thread architecture note):** `PaneRuntime` (private; current shape per the
table-driven SP3+ rewrite, superseding the `{pty, grid, dead}` sketch in the
historical "Internal shape" paragraph above) gains one field:
`input_tx: Sender<Vec<u8>>`. `spawn_pane` now ALSO spawns a dedicated
per-pane writer thread — owning an independent duplicate of the pty's input
write handle via the new `Pty::try_clone_writer` (see
`2026-07-06-mvp-interfaces.md`'s sibling `## pty` amendment) — that drains
this channel, mirroring the existing per-client `spawn_writer` design
EXACTLY (same unbounded `mpsc<Vec<u8>>`-drained-by-a-dedicated-thread shape).
The server's own hot Forward/Key-forwarding path (`Server::
process_client_events`'s `KeyInputEvent::Forward`/unbound-`Root`-`Key` arms)
now enqueues onto `pane.input_tx` instead of calling `pty.write_input`
INLINE on the main-loop thread — closing the gap follow-up #14 tracked
(a pane whose child stops draining stdin, e.g. a hung app or a huge paste,
previously blocked `write_input`, which blocked the ENTIRE main loop —
rendering and input for every session, not just the stalled pane's).
`PaneRuntime` is dropped exactly the same way as before (pane removal is the
only way one is ever dropped); dropping it now ALSO drops `input_tx`, which
closes the channel and lets the writer thread's `recv()` loop end on its
own — no new explicit shutdown/join path needed. `src/server/dispatch.rs`'s
LOWER-volume write sites (send-keys, paste-buffer, mouse-drag forwarding)
are UNCHANGED — they still call `pty.write_input` directly; only the two
hottest, most frequently-hit call sites (ordinary keystroke/paste forwarding)
moved onto the new channel. Regression coverage:
`tests/server_proto.rs::stalled_pane_stdin_does_not_block_other_sessions`
(a session whose pane never reads stdin at all is flooded with raw `Stdin`
frames; a concurrent CLI round trip against a DIFFERENT session, sharing the
same main loop, must stay fast — reproduced RED against the pre-fix inline
`write_input` call, ~3.8s, GREEN after the fix, ~1s).

## `cli` — argv parser (pure, Task 8; amended Task 7 SP3 for `-f`)

```rust
pub struct Invocation {
    pub socket: String,          // -L, default "default"
    pub config: Option<String>,  // -f <config-file> (Task 7); None unless given
    pub cmd: Command,
}

pub enum Command {
    NewSession { name: Option<String>, detached: bool, cols: u16, rows: u16 },
    Attach { target: Option<String>, detach_others: bool },
    Control(Vec<String>),
    ServerRole { pipe: String, config: Vec<String> }, // config: --config <path>, repeatable (Task 7)
    Help,
}

pub fn parse(args: &[String]) -> Result<Invocation, String>; // Err = usage message
pub fn usage_text() -> &'static str; // printed by main.rs for `Help`, exit 0
```

**Amendment (Task 7, SP3 config loading):** `-f <config-file>` is extracted
from ANYWHERE in `args`, exactly like `-L` (same loop, same "no supported
subcommand has a flag of its own named `-f`" non-collision argument); given
more than once, the LAST occurrence wins (tmux's own `-f` takes a single
value — this just doesn't error on a repeat rather than replicating tmux's
own multi-`-f` behavior, which SP3 doesn't need). `-f` is accepted as a
GLOBAL flag regardless of `cmd` variant, but only has any effect via
`main.rs`'s routing when `cmd` is `NewSession` (the only autostart-capable
entry point — see the `## client`/`## config` amendments); it is silently
inert for `Attach`/`Control`/`Help`/`ServerRole`. The special value `-f -`
disables config loading entirely (Task 7 review fix; tmux `-f /dev/null`
idiom — the sentinel is interpreted by the server's discovery function, not
by `cli.rs`, which passes it through like any other value; see the `##
config` section of the sibling SP3 contract). `usage_text()`'s first line
gained `[-f config-file]`; the `Global:` line documents both flags and the
`-f -` sentinel.
`__server`'s hidden role (`parse_server_role`) now also accepts `--config
<path>`, repeatable, in any order relative to `--pipe` (still exactly one
`--pipe`, still required) — collected into `Command::ServerRole`'s new
`config: Vec<String>` field, forwarded verbatim to `server::run`. Tests:
`dash_f_parses`, `dash_f_anywhere`, `dash_f_repeated_last_wins`,
`server_role_config_args` (plus `bare_is_new_session`/`server_role_parse`
updated for the new struct fields).

**Behavior:**

- `-L <name>` is extracted from ANYWHERE in `args` (before or after the
  subcommand token), leaving the remaining tokens in order; none of the
  supported subcommands has a flag of its own named `-L`, so this can't
  collide with a passed-through `Control` argv. Missing value after `-L` is
  a usage error.
- Empty `args` (bare `winmux`) → `NewSession { name: None, detached: false,
  cols: 0, rows: 0 }`. `cols`/`rows` are `0` when not given on the command
  line (by `-x`/`-y` or the bare-defaults case) — NOT the real terminal
  size; `main.rs` fills that in afterward (console probe for an attached
  session, `80x24` for a detached one), per the "CLAMP dims: `-x`/`-y`
  override" rule in the design spec.
- `new-session`/`new [-d] [-s name] [-x cols] [-y rows]` → `NewSession`;
  `-x`/`-y` values that don't parse as `u16` are a usage error. Any token
  not matching one of the four flags is a usage error (flags are never
  silently demoted to positionals, mirroring `server.rs`'s own CLI-argv
  convention).
- `attach-session`/`attach`/`a [-d] [-t target]` → `Attach`. Note `-d` here
  means `detach_others` (tmux's attach `-d`, "detach every other client
  already on that session") — a DIFFERENT meaning from `new-session -d`'s
  "detached" (don't attach a client at all). Any token other than `-d`/`-t`
  is a usage error.
- `__server --pipe <full-pipe-name>` → `ServerRole { pipe }`; anything else
  after `__server` (missing `--pipe`, missing value, extra tokens) is a
  usage error. This is a hidden role — never advertised in `usage_text()`.
- `--help` / `-h` / `help` (as the first non-`-L` token) → `Help`.
- Any other token starting with `-` as the first non-`-L` token → a usage
  error (`Err`) — there is no bare top-level flag other than `-L`/`-h`.
- Everything else (first token doesn't match any of the above, and doesn't
  start with `-`) → `Control(rest)`, the ENTIRE remaining token list
  (including that first token) forwarded verbatim — `ls`, `list-sessions`,
  `has-session`/`has`, `kill-session`, `kill-server`, `rename-session`,
  `rename-window`, `list-windows`/`lsw`, `detach-client`, and any unknown
  command name are all routed here; `cli.rs` does not validate them further
  — `server.rs`'s `execute_cli`/`CliArgs` (see `## server` above) owns that,
  replying `CliDone{1, "", "unknown command"}` for anything it doesn't
  recognize.

**Implementation module:** `src/cli.rs`, pure (`&[String]` in, no I/O, no
Windows APIs) — unit-tested directly: `bare_is_new_session`, `new_flags`,
`ls_alias`, `attach_t`, `attach_dashd`, `dash_l_socket`, `server_role_parse`,
`unknown_flag_err`, `kill_session_passthrough`, `help_parses`.

## `client` — thin attach client + server autostart (Task 8; `autostart_server` amended Task 7)

```rust
pub fn attach(pipe_full_name: &str, first: protocol::ClientMsg) -> Result<i32, Box<dyn std::error::Error>>;
pub fn autostart_server(socket: &str, config: Option<&str>) -> std::io::Result<()>;
```

**Amendment (Task 7, SP3 config loading):** `autostart_server` gained a
`config: Option<&str>` parameter — the invocation's `-f <file>` (`cli::
Invocation::config`). When `Some`, the spawned `__server` argv gets
`--config <file>` appended after `--pipe <full-name>`; when `None`, no
`--config` flag is passed at all (the server falls back to its own default
`.tmux.conf`/`.winmux.conf` discovery). `main.rs`'s `run_new_session` is the
only caller that ever passes `Some` (the autostart-capable entry point);
`ensure_server`'s signature grew the same `config: Option<&str>` pass-
through parameter.

**`attach`:** `Host::enter()`s FIRST, then connects to `pipe_full_name` and
sends `first` (an `Attach` frame, already built by the caller with the
right `AttachMode`/size/name), and runs until the server sends a terminal
message. The enter-before-connect ordering is a Task 8 review fix: if the
terminal can't be entered at all (e.g. stdio is redirected — no console),
no `Attach` frame ever reaches the server, so no server-side session is
created for a client that can never use it (previously the frame went out
first; a failed `enter` then stranded a session — and, with exit-empty,
kept an autostarted server alive forever). A connect/write failure after
`enter` drops `host` (a local) on the `?` path, restoring the console
before `main.rs` prints the error. Loop behavior:

- A stdin-reader thread (its own `PipeConn::try_clone`) blocks in
  `host::read_stdin`, forwarding each chunk as a `Stdin` frame; on read
  failure or EOF (console closing) it sends a best-effort `Detach` frame
  before exiting. This thread is NEVER joined or cancelled — `main.rs`'s
  `std::process::exit` after `attach` returns is what reclaims it (see
  below).
- A second reader thread (another `try_clone`) decodes `ServerMsg` frames
  and relays them to the main loop over an `mpsc` channel, so the main loop
  can ALSO wake up on a 50ms tick (a plain blocking read on the main thread
  can't do both).
- **Follow-up #16 (stdin-reader panic):** the stdin thread's body runs inside
  `std::panic::catch_unwind`. A panic there previously left the main loop
  waiting on the reader channel forever (stdin forwarding silently dead, but
  nothing told the main loop) — now the stdin thread sends an `Err` through
  the SAME channel the reader thread uses (it holds its own clone of `tx`),
  which the main loop's existing fatal-error handling (below) already turns
  into a clean non-zero exit.
- Main loop: `recv_timeout(50ms)`. `Output` → `host.write`. `Exit{code,
  msg}` → drop `Host` FIRST (restores the console), THEN print `msg`
  (stdout if `code == 0`, else stderr), THEN return `code as i32` — this
  ordering is load-bearing, not cosmetic: the message must land on the
  restored normal screen, not the alt screen the pane content was drawn
  into. `CliDone` is ignored (not expected on an attached connection).
  Reader error/EOF without an `Exit`, or the reader thread hanging up
  (`RecvTimeoutError::Disconnected`) → drop `Host`, then (**follow-up #19,
  kill-server race**) print `"[lost server]"` to stderr IF at least one real
  `ServerMsg` was ever received on this connection, else print the cleaner
  `"no server running on <pipe>"` (same text `main.rs`'s
  `report_connect_error` uses) — a connection that raced a `kill-server`
  teardown window (connected, `Attach` sent, but the server tore itself down
  before serving even one reply) is indistinguishable in EFFECT from the pipe
  never having existed, so it now gets the same message a client connecting
  slightly later (once the pipe is fully gone) would see. Either way, return
  `1`. On `RecvTimeoutError::Timeout`: poll `host.size()`; if changed since
  the last known size, send a `Resize` frame over the ORIGINAL connection
  (not a clone — the reader/writer split only needs two of the three
  duplicates to be independent, since only the main thread ever writes
  `Resize`/the initial `Attach`, and only the stdin thread ever writes
  `Stdin`/`Detach`).
- **Documented caveat** (task brief, verified true): `host::read_stdin` has
  no clean cancellation — it blocks in `ReadFile` until the NEXT keystroke
  even after `attach` has returned. This is fine ONLY because every caller
  (`main.rs`) immediately calls `std::process::exit` with `attach`'s return
  code, which tears down the whole process (and that leaked thread) right
  away; `attach` must never be called from a context that expects to keep
  running afterward.

**`autostart_server`:** builds `pipe::pipe_name(socket)`, spawns
`std::env::current_exe()` as `<exe> __server --pipe <full-name>` with
`creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)` (`0x8 |
0x200`, via `std::os::windows::process::CommandExt`) so the server has no
console of its own and outlives the spawning client's console, then polls
`PipeConn::connect` on that same pipe every 50ms for up to 5s. Returns
`Ok(())` the first time a connect succeeds (the connection itself is
dropped immediately — this call is purely a readiness probe); the CALLER
does its own real `PipeConn::connect` afterward. Times out as the last
`NotFound` error if 5s elapses without success; any OTHER connect error
(not `NotFound`) is returned immediately without waiting out the timeout.

**`main.rs` routing** (not part of the locked interface, but the only
caller, so documented here for context): `ServerRole` installs a SECOND,
file-logging panic hook (chained in front of `host::install_panic_hook`'s
console-restoring one via `take_hook`/`set_hook`, appending to
`%LOCALAPPDATA%\winmux\server.log`) before calling `server::run(&pipe,
&config)`, since the server process has no console to print to. **Amendment
(Task 7):** the `log_line`/`server_log_dir` helpers moved out of `main.rs`
into a new small lib module, `src/logging.rs` (`pub fn log_line(&str)`),
since `server.rs`'s startup config loading now ALSO needs to log (config
errors) and `main.rs` (a bin, not part of the `winmux` lib) can't be `use`d
from `server.rs`. `main.rs`'s panic hook and exit-code logging now call
`winmux::logging::log_line` instead of a private duplicate. `NewSession{detached: false}`
FIRST probes `host::console_size()` — `Err` (stdio not a console) prints
`open terminal failed: not a console` to stderr and exits 1 BEFORE
`ensure_server`/autostart ever runs (Task 9 scope A) — then probes-then-
autostarts (`PipeConn::connect`; `NotFound` → `client::autostart_server(socket,
invocation.config.as_deref())`, forwarding `-f`'s file ONLY if this
invocation is the one that actually spawns the server — Task 7), sizes the
`Attach` frame from the already-probed size (fallback `80x24`, overridden by
nonzero `-x`/`-y`), and calls `client::attach`. Ordering
matters: without the upfront probe, a redirected-stdio invocation would
still autostart a detached server via `ensure_server`, then only fail later
inside `client::attach`'s own `Host::enter()` — stranding that autostarted
server alive forever, since it never gets a session and `run`'s exit-empty
check (`had_session && registry.is_empty()`) needs `had_session` to have
ever flipped true. `NewSession{detached: true}` has no console to probe (and
needs none — skips the guard entirely), probes-then-autostarts the same way,
then sends a one-shot `new-session -d [-s name] -x <cols> -y <rows>`
`Control` command (defaulting unset `-x`/`-y` to `80`/`24` directly, since
there's no console to probe for a session nobody is attaching to). `Attach`
ALSO probes `host::console_size()` first (same `open terminal failed`/exit 1
on failure, before even `probe_running`'s connect) — an attach with no
console must not touch the server at all — then connects WITHOUT
autostarting; `Control` connects WITHOUT autostarting either. Both of these
non-autostarting paths' `NotFound` prints `no server running on <pipe>` and
exits 1 — matching the design spec's "pure queries... error... auto-start:
new-session... starts the server" rule. Covered by
`tests/e2e_sessions.rs::e2e_no_console_fails_fast` (renamed from
`no_console_fails_fast`, follow-up #24).

**Implementation module:** `src/client.rs`. No unit tests (pure I/O glue:
threads, a live named pipe, a live console) — coverage is
`tests/e2e.rs::e2e_split_kill_exit` (drives the real attached-client path
through `client::attach` end to end under a ConPTY) plus manual runs.
