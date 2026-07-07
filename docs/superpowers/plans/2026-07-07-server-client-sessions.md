# Server/Client Split + Sessions + Detach Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split winmux into a detached background server (owning all sessions/windows/panes over ConPTY) and thin clients attaching over named pipes, with multiple sessions/windows, detach/attach, tmux-default keybindings and a tmux-compatible CLI subset.

**Architecture:** The server reuses the MVP event-loop shape — one mpsc channel, all state owned by the main thread — adding an accept thread, per-client reader/writer threads, and a `Registry` (sessions → windows → panes) above the existing `Layout`. Clients are dumb pipes: raw stdin bytes and resize messages in, server-composed VT bytes out. Full design + protocol: `docs/specs/2026-07-07-server-client-design.md`. Behavioral ground truth (exact tmux strings/semantics): same spec.

**Tech Stack:** Rust 2021, `vte 0.13`, `windows 0.58` (named pipes via existing `Win32_System_Pipes` feature). **No new dependencies** — hand-rolled framing, hand-rolled arg parsing.

## Global Constraints

- Interface contract discipline: public APIs are locked by `docs/specs/2026-07-06-mvp-interfaces.md` + new `docs/specs/2026-07-07-server-client-interfaces.md`. Any task adding/changing public surface MUST update the contract file in the same commit.
- `cargo clippy --all-targets -- -D warnings` stays green at every commit.
- `cargo` lives at `~/.cargo/bin` (may not be on PATH): `export PATH="$HOME/.cargo/bin:$PATH"` (bash).
- Never wreck the user's terminal: anything mutated by `Host::enter` must be restored in `apply_restore` on all exit paths.
- All tests must not collide with a real user server: every server-spawning test uses a unique `-L`/pipe name and kills its server in teardown.
- tmux parity: user-visible strings, keybindings, and defaults must match the design spec verbatim (they were verified against tmux source).
- Commit after every green task with a conventional-commit message.

---

### Task 1: Protocol module (frame codec)

**Files:**
- Create: `src/protocol.rs`
- Modify: `src/lib.rs` (add `pub mod protocol;`)
- Create: `docs/specs/2026-07-07-server-client-interfaces.md` (contract file with a `## protocol` section documenting everything below)

**Interfaces (Produces):**
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
Wire format is the design spec's table exactly: `[type u8][len u32 LE][payload]`; strings UTF-8 with u16 length prefixes (u32 for CliDone out/err); reads use `read_exact`; unknown type / oversize len / bad UTF-8 → `io::Error` of kind `InvalidData`; clean EOF before the type byte → `ErrorKind::UnexpectedEof`.

- [ ] **Step 1:** Write failing unit tests in `src/protocol.rs` `#[cfg(test)]`: roundtrip every variant through a `Vec<u8>` cursor (`attach_roundtrip`, `stdin_roundtrip`, `resize_roundtrip`, `detach_roundtrip`, `cli_roundtrip`, `output_roundtrip`, `exit_roundtrip`, `clidone_roundtrip`); byte-exact golden test `attach_wire_bytes` asserting the encoded bytes of `Attach{Existing, false, 80, 24, "main"}` == `[0x01, 11,0,0,0, 0x00, 0x00, 0x50,0x00, 0x18,0x00, 0x04,0x00, b'm',b'a',b'i',b'n']`; error tests `unknown_type_is_invalid_data`, `oversize_len_is_invalid_data`, `truncated_payload_is_eof`.
- [ ] **Step 2:** `cargo test protocol::` → all FAIL (module missing).
- [ ] **Step 3:** Implement the codec (pure std; no unsafe).
- [ ] **Step 4:** `cargo test protocol::` → PASS; `cargo clippy --all-targets -- -D warnings` clean.
- [ ] **Step 5:** Create the contract file section; commit `feat(protocol): client/server frame codec`.

### Task 2: Named-pipe transport

**Files:**
- Create: `src/pipe.rs`
- Modify: `src/lib.rs` (add `pub mod pipe;`)
- Create: `tests/pipe_smoke.rs`
- Modify: contract file (add `## pipe` section)

**Interfaces (Produces):**
```rust
pub fn pipe_name(socket_name: &str) -> String; // "\\.\pipe\winmux-<username>-<socket_name>"
pub struct PipeListener;   // opaque: owns nothing between accepts
impl PipeListener {
    pub fn bind(full_name: &str) -> std::io::Result<PipeListener>;
    pub fn accept(&self) -> std::io::Result<PipeConn>; // CreateNamedPipeW instance + ConnectNamedPipe (blocking)
}
pub struct PipeConn;       // opaque: HANDLE wrapped in std::fs::File
impl PipeConn {
    pub fn connect(full_name: &str) -> std::io::Result<PipeConn>; // CreateFileW; ERROR_FILE_NOT_FOUND -> ErrorKind::NotFound
    pub fn try_clone(&self) -> std::io::Result<PipeConn>;         // File::try_clone (reader/writer threads)
}
impl std::io::Read for PipeConn { ... }
impl std::io::Write for PipeConn { ... }
```
Implementation notes: `CreateNamedPipeW(name, PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES, 64*1024, 64*1024, 0, None)`. `bind` verifies the name is creatable by creating **and holding** the first instance (so `accept` #1 uses it, later accepts create new instances). `ERROR_PIPE_BUSY` on connect → `WaitNamedPipeW(2000)` + retry once. Client connect to a nonexistent pipe must map to `ErrorKind::NotFound` (the CLI's "no server running" signal). `ERROR_BROKEN_PIPE`/`ERROR_PIPE_NOT_CONNECTED` on read → `Ok(0)` EOF (match `pty.rs` convention). Username via `GetUserNameW` (`Win32_System_WindowsProgramming` feature may be needed — if so add it to Cargo.toml and note it in the contract), fallback `"unknown"`; strip characters illegal in pipe names (keep `[A-Za-z0-9_-]`, replace others with `_`).

- [ ] **Step 1:** Write `tests/pipe_smoke.rs`: `roundtrip_client_server` (bind unique name `winmux-test-pipe-<pid>`, thread accepts then echoes one protocol `ServerMsg::Output` for each received `ClientMsg::Stdin`; client connects, sends, asserts reply — exercises Task 1 codec over a real pipe), `connect_absent_pipe_is_not_found`, `two_sequential_clients` (accept loop serves two connections one after another).
- [ ] **Step 2:** `cargo test --test pipe_smoke` → FAIL (module missing).
- [ ] **Step 3:** Implement `src/pipe.rs`.
- [ ] **Step 4:** `cargo test --test pipe_smoke` → PASS; clippy clean.
- [ ] **Step 5:** Contract section; commit `feat(pipe): named-pipe listener/connection transport`.

### Task 3: Session/window model (pure)

**Files:**
- Create: `src/model.rs`
- Modify: `src/lib.rs` (add `pub mod model;`)
- Modify: contract file (add `## model` section)

**Interfaces (Produces):**
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
Session/window name validation: reject empty and names containing `:` or `.` (tmux target separators) → `Err("bad session name: <n>")`.

- [ ] **Step 1:** Failing unit tests (`model::` mod): `auto_name_fills_gaps` (create "0","1", kill "0", next auto is "0"), `duplicate_session_err_string` (exact `duplicate session: x`), `find_exact_then_prefix` (`mysess` matches `mysession` only when unambiguous; ambiguity → `can't find session:`), `find_eq_forces_exact`, `window_index_lowest_unused` (indexes 0,1,2; kill 1; next new window gets 1), `kill_current_window_falls_back_to_last`, `last_window_toggles`, `next_prev_wrap`, `neighbor_session_wraps`, `bad_names_rejected`.
- [ ] **Step 2:** `cargo test model::` → FAIL. **Step 3:** implement. **Step 4:** PASS + clippy. **Step 5:** contract; commit `feat(model): session/window registry with tmux naming semantics`.

### Task 4: Input machine — window/session actions

**Files:**
- Modify: `src/input.rs`
- Modify: `docs/specs/2026-07-06-mvp-interfaces.md` (input section: new variants) and new contract file (cross-reference)

**Interfaces (Produces — added `Action` variants):**
```rust
pub enum Action {
    /* existing: Split, Focus, FocusNext, FocusLast, RequestClose, ToggleZoom, Resize, Quit */
    NewWindow, NextWindow, PrevWindow, LastWindow, SelectWindow(u32),
    RequestKillWindow, RenameWindow, RenameSession, Detach,
    SwitchClientPrev, SwitchClientNext,
}
// InputMachine additions:
pub fn set_capture(&mut self, on: bool);   // while on, feed() emits only InputEvent::Captured(bytes) — raw, uninterpreted
// InputEvent addition:
pub enum InputEvent { Forward(Vec<u8>), Action(Action), ConfirmClose(bool), Captured(Vec<u8>) }
```
Bindings (Prefixed state): `c`→NewWindow, `n`→NextWindow, `p`→PrevWindow, `l`→LastWindow, `0`..=`9`→SelectWindow(d), `&`→RequestKillWindow, `,`→RenameWindow, `$`→RenameSession, `d`→Detach, `(`→SwitchClientPrev, `)`→SwitchClientNext. All → Normal after. `set_capture(true)` clears pending state (like `set_confirming`); capture takes precedence over Confirming.

- [ ] **Step 1:** Failing tests: one per new binding (`prefix_c_is_new_window`, ..., `prefix_digit_selects_window` for `0` and `9`), `capture_mode_passes_bytes_raw` (prefix byte inside capture is NOT interpreted, comes out as `Captured`), `capture_off_resumes_normal`.
- [ ] **Step 2:** `cargo test input::` → new tests FAIL. **Step 3:** implement. **Step 4:** all input tests PASS + clippy. **Step 5:** update both contract files; commit `feat(input): tmux window/session/detach bindings and capture mode`.

### Task 5: Status line builder + render spans

**Files:**
- Create: `src/status.rs`; Modify: `src/lib.rs`
- Modify: `src/render.rs` (Scene change), `src/app.rs` (adapt to new Scene shape — keep MVP behavior compiling; app.rs is deleted in Task 8)
- Modify: both contract files (render Scene change is a **locked-contract amendment**: `status_left: String` → `status_spans: Vec<StatusSpan>`)

**Interfaces (Produces):**
```rust
// render.rs
pub struct StatusSpan { pub text: String, pub underline: bool }
pub struct Scene<'a> { pub size: (u16,u16), pub panes: Vec<PaneView<'a>>, pub zoomed: bool,
    pub status_spans: Vec<StatusSpan>, pub status_right: String, pub message: Option<String> }
// status.rs (pure)
pub struct WindowEntry { pub index: u32, pub name: String, pub current: bool, pub last: bool, pub zoomed: bool }
pub fn status_spans(session_name: &str, windows: &[WindowEntry]) -> Vec<StatusSpan>;
```
`status_spans` output: span `[<session>] ` (underline false), then per window (index order) a span `"<idx>:<name><flag>"` where flag = `*` if current, `-` if last, else ` `, with `Z` appended after `*`/`-` when zoomed (e.g. `0:powershell*Z`), separated by a single space (attach the trailing space to each non-final span); the **current** window's span has `underline: true` (tmux `window-status-current-style` = underscore). Render draws spans left-to-right on the status row: green bg/black fg as today + SGR 4 for underline spans (and reset 24 after).

- [ ] **Step 1:** Failing tests: `status.rs` — `single_window_current` (exact spans for session `0`, window 0 `powershell` current → `[("[0] ", false), ("0:powershell*", true)]`), `flags_last_and_zoomed`, `three_windows_order_and_separators`; `render.rs` — adapt existing status-bar tests to spans and add `underlined_span_emits_sgr4` asserting the emitted VT contains `\x1b[…4…m` for the current-window cells and no underline for others.
- [ ] **Step 2:** FAIL → **Step 3:** implement (`render::compose_back` step 4 iterates spans; underline is a new bool on the status cells' `Style` — reuse `Style.underline`). Keep `app.rs` compiling by building `status_spans` from its hardcoded string. **Step 4:** whole `cargo test` PASS + clippy. **Step 5:** contracts; commit `feat(status): model-driven status line with underlined current window`.

### Task 6: Server core (attach, panes, render-per-client, detach)

**Files:**
- Create: `src/server.rs`; Modify: `src/lib.rs`
- Create: `tests/server_proto.rs`
- Modify: contract file (`## server` section: only `pub fn run(pipe_full_name: &str) -> Result<(), Box<dyn Error>>` is public)

**Interfaces:**
- Consumes: `protocol::*`, `pipe::{PipeListener, PipeConn}`, `model::Registry`, `input::{InputMachine, InputEvent, Action}`, `render::{Renderer, Scene, StatusSpan, PaneView}`, `status::status_spans`, `pty::Pty`, `grid::Grid`, `layout::*`.
- Produces: `pub fn run(pipe_full_name: &str) -> Result<(), Box<dyn std::error::Error>>` — blocks until server shutdown. Everything else private.

Internal shape (private, from the design spec):
```rust
enum ServerEvent { Output(PaneId, Vec<u8>), Exited(PaneId), Connected(PipeConn),
                   FromClient(ClientId, ClientMsg), ClientGone(ClientId), Tick }
struct PaneRuntime { pty: Option<Pty>, grid: Grid, dead: bool }   // pty dropped at Exited (follow-up #1)
struct ClientState { session: Option<String>, cols: u16, rows: u16, renderer: Renderer,
                     input: InputMachine, mode: ClientMode, tx: Sender<Vec<u8>> }
enum ClientMode { Normal, ConfirmKillPane(PaneId), ConfirmKillWindow(WindowId),
                  Prompt { label: String, buf: String, kind: PromptKind } }
```
Scope of THIS task (window ops are Task 7): accept loop; `Attach` all three modes (`NewAuto`, `NewNamed` incl. `duplicate session:` Exit, `Existing` incl. `can't find session:` Exit and `detach_others`); spawn panes (`powershell.exe -NoLogo`, reader+waiter threads exactly as MVP `spawn_pane`); per-client render (own `Renderer`, full repaint on attach; scene from the client's session's current window; status via Task 5; cursor only for focused pane); `Stdin` → `InputMachine` → pane actions (Split/Focus/FocusNext/FocusLast/RequestClose+confirm with tmux text `kill-pane <pane-index-in-window>? (y/n)` accepting `y`/`Y`/`\r`/`\n`, ToggleZoom, Resize) and `Detach` action + `Detach` frame → `Exit{0, "[detached (from session <name>)]"}`; `Resize` frames → session size = min over attached clients, resize ptys/grids/renderers; pane exit → drop Pty; last pane of last window → session destroyed → attached clients `Exit{0, "[exited]"}`; registry empty → `run()` returns (exit-empty); coalesce: drain all pending events before rendering once per loop turn (follow-up #4); multiple clients on one session each get their own composed stream; `PSModulePath` removal + panic hook + log file are `main.rs` concerns (Task 8) — `run()` itself must not touch the console. Ignore `Cli` frames with a `CliDone{1,"",“unknown command"}` stub (Task 7 fills it in).

- [ ] **Step 1:** Write `tests/server_proto.rs` (failing) — helpers: `start_server(name) -> JoinHandle` (spawns `server::run` on a thread with unique pipe name `winmux-proto-<pid>-<n>`), `Client` wrapper (connect + typed send/recv with 10s deadlines, `recv_output_until(pred)` accumulating `Output` payloads through a fresh `Grid` 80x24 for screen assertions). Tests:
  - `attach_new_auto_shows_status` — Attach NewAuto 80x24 → screen eventually contains `[0] 0:powershell*` on the bottom row and a `PS ` prompt.
  - `duplicate_named_session_is_error` — NewNamed "x" twice (2 conns) → second gets `Exit{1, "duplicate session: x"}`.
  - `attach_missing_session_error` — Existing "nope" → `Exit{1, "can't find session: nope"}`.
  - `detach_frame_returns_message` — Attach NewNamed "s1" → send `Detach` → `Exit{0, "[detached (from session s1)]"}`.
  - `session_survives_detach_and_reattaches` — attach "s2", send `Stdin("echo marker-123\r")`, wait for `marker-123` echo, Detach, new conn Attach Existing "s2" → full repaint contains `marker-123`.
  - `prefix_d_detaches` — Attach, send `Stdin([0x02, b'd'])` → Exit detached message.
  - `split_and_kill_pane_confirm` — `\x02%` → (screen gains `│` column) → `\x02x` → screen shows `kill-pane 1? (y/n)` → `y` → border gone.
  - `exit_last_shell_exits_session` — Attach NewAuto, `Stdin("exit\r")` → `Exit{0, "[exited]"}` and server thread joins (exit-empty).
  - `two_clients_smallest_size_wins` — client A 100x40 + B 80x24 on same session → A's screen shows blank area right of col 80 (probe: status row spans full 100 cols, pane content confined to 80).
- [ ] **Step 2:** `cargo test --test server_proto` → FAIL. **Step 3:** implement `src/server.rs` (largest single task, ~600-800 lines). **Step 4:** server_proto + whole suite PASS + clippy. **Step 5:** contract; commit `feat(server): headless multiplexer server with attach/detach over named pipes`.

### Task 7: Server window/session ops + CLI command execution

**Files:**
- Modify: `src/server.rs`, `tests/server_proto.rs`
- Modify: contract file (document CLI command surface + exact output strings)

**Interfaces:** consumes Task 4 actions + Task 3 model; extends the private server. `Cli(argv)` handling: execute against registry, reply `CliDone{code, out, err}`. Commands: `list-sessions|ls` (per line `"{name}: {n} windows (created {ctime}){attached}"` where ctime = `%a %b %e %H:%M:%S %Y` C-locale format e.g. `Tue Jul  7 09:14:22 2026`, attached = `" (attached)"` iff ≥1 client; empty registry → `CliDone{1,"","no sessions"}` — unreachable under exit-empty but required); `has-session|has -t x` (0/`can't find session: x`); `kill-session [-t x]` (default: most recent; clients get `Exit{0,"[exited]"}`); `kill-server` (all clients `Exit{0,"[server exited]"}`, then `run()` returns); `new-session -d [-s x] [-x c] [-y r]` (detached, default 80x24); `rename-session [-t x] new`, `rename-window [-t sess[:idx]] new`; `list-windows|lsw [-t x]` (per line `"{index}: {name}{flag} ({n} panes) [{w}x{h}]{active}"`, flag `*`/`-`/``, active = `" (active)"` iff current); `detach-client -s x`. Keybinding actions: NewWindow (new pane spawn + window, becomes current), Next/Prev/Last/SelectWindow (miss → transient status message `window not found: <n>`, yellow message slot, cleared on next keypress or ~750ms), RequestKillWindow → confirm `kill-window <name>? (y/n)`, RenameWindow/RenameSession → capture-mode prompt (`(rename-window) ` / `(rename-session) ` + editable initial text, cursor at end shown by trailing block? — no: just text; Enter commits with validation, Esc/`\x03`/`\x07` cancels, Backspace `\x7f`/`\x08` deletes), SwitchClientPrev/Next (wrap registry order; renderer full repaint). Status line + window list update on every change. Window kill cascades: only window → session destroyed → `[exited]`.

- [ ] **Step 1:** Failing tests added to `server_proto.rs`: `new_window_updates_status` (`\x02c` → `[0] 0:powershell- 1:powershell*`), `next_prev_last_window_flags`, `select_window_by_digit`, `select_missing_window_shows_message` (`window not found: 5`), `kill_window_confirm_text` (`kill-window powershell? (y/n)`; `y` kills; window list shrinks; killing only window → `[exited]`), `rename_window_prompt_flow` (`\x02,` → screen shows `(rename-window) powershell`; send `\x7f`*10 then `web` then `\r` → status shows `0:web*`), `rename_session_prompt_flow` (`$` → `[mysess]` in status), `prompt_escape_cancels`, `switch_client_next_cycles_sessions`, `cli_ls_format_exact` (regex `^s\d: 1 windows \(created \w{3} \w{3} [ \d]\d \d{2}:\d{2}:\d{2} \d{4}\)( \(attached\))?$`), `cli_has_session_codes`, `cli_kill_session_notifies_attached`, `cli_new_detached_then_attach`, `cli_rename_session`, `cli_list_windows_format`, `cli_kill_server_exits_all`, `cli_unknown_command_err`.
- [ ] **Step 2:** FAIL → **Step 3:** implement. **Step 4:** full suite PASS + clippy. **Step 5:** contract; commit `feat(server): windows, sessions, prompts, and tmux CLI command execution`.

### Task 8: Client + CLI + main (replace app.rs)

**Files:**
- Create: `src/client.rs`, `src/cli.rs`; Modify: `src/lib.rs`, `src/main.rs`
- Delete: `src/app.rs` (its loop is superseded; remove `pub mod app;`)
- Modify: `src/host.rs` (follow-up #3: publish `RESTORE` snapshot before the first mutation in `enter()`)
- Modify: both contract files (app removed; client/cli sections; host amendment)

**Interfaces (Produces):**
```rust
// cli.rs (pure — unit-testable)
pub struct Invocation { pub socket: String /*-L, default "default"*/, pub cmd: Command }
pub enum Command {
    NewSession { name: Option<String>, detached: bool, cols: u16, rows: u16 }, // bare winmux => this with all defaults
    Attach { target: Option<String>, detach_others: bool },
    Control(Vec<String>),      // everything forwarded verbatim to the server (ls, has-session, kill-session, ...)
    ServerRole { pipe: String }, // __server --pipe <name>
    Help,
}
pub fn parse(args: &[String]) -> Result<Invocation, String>; // Err = usage message, printed to stderr, exit 1
// client.rs
pub fn attach(pipe_full_name: &str, first: protocol::ClientMsg) -> Result<i32, Box<dyn std::error::Error>>;
//  - Host::enter, stdin thread -> Stdin frames, 50ms tick polls host.size() -> Resize frames on change,
//    reads ServerMsg: Output -> host.write; Exit{code,msg} -> drop Host (restore!) THEN print msg, return code.
//  - unexpected EOF from server -> restore, print "[lost server]", return 1.
pub fn autostart_server(socket: &str) -> std::io::Result<()>;
//  - spawn current_exe() as: winmux __server --pipe <full-name>, CreationFlags DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP,
//    then poll PipeConn::connect up to 5s (50ms interval).
```
`main.rs`: `remove_var("PSModulePath")` first (unchanged), `install_panic_hook`, parse argv. Routing: `ServerRole` → also redirect panics/errors to `%LOCALAPPDATA%\winmux\server.log`, run `server::run(&pipe)`; `NewSession{detached:false}` → connect-or-autostart, `attach` with `Attach{mode: NewNamed/NewAuto, ...}` sized from `Host`-free console probe (`GetConsoleScreenBufferInfo` via a small `host::console_size()` helper — add it, contract) falling back 80x24; `NewSession{detached:true}` → connect-or-autostart then `Control(["new-session","-d",...])`; `Attach` → connect (NO autostart; NotFound → `no server running on <pipe>` exit 1) then attach `Existing` (empty target → server picks most-recent session; add that rule to server find); `Control` → connect (no autostart) → send `Cli` → print out/err → exit code. Exit codes: server's. Ctrl-c handling: raw 0x03 already flows as a byte (no console handler needed).

- [ ] **Step 1:** Failing `cli::` unit tests: `bare_is_new_session`, `new_flags` (`new -d -s x -x 100 -y 30`), `ls_alias`, `attach_t`, `attach_dashd`, `dash_l_socket`, `server_role_parse`, `unknown_flag_err`, `kill_session_passthrough`.
- [ ] **Step 2:** FAIL → **Step 3:** implement cli.rs, client.rs, main.rs rewiring, host.rs RESTORE-ordering fix + `console_size()`. Delete app.rs. **Step 4:** `cargo test` (old `tests/e2e.rs` WILL break — update it minimally in this task: prepend `-L e2e-<pid>` args and `kill-server` teardown via a `Control` connection in a `Drop` guard; full new e2e is Task 9). `cargo build --release` OK; clippy clean. **Step 5:** contracts; commit `feat(cli,client): tmux-style CLI, thin attach client, detached server autostart`.

### Task 9: End-to-end tests (full tmux workflow)

**Files:**
- Modify: `tests/e2e.rs` (keep `e2e_split_kill_exit` green under new architecture)
- Create: `tests/e2e_sessions.rs`

**Interfaces:** consumes the e2e harness pattern (spawn `winmux.exe` under test-owned `Pty`, decode via `Grid`, `wait_until` polling); every test uses socket `-L e2e-<testname>-<pid>` + `KillServerGuard` (Drop: `winmux -L <sock> kill-server` via `std::process::Command`, tolerate failure).

- [ ] **Step 1:** Write failing tests:
  - `e2e_detach_reattach_persists`: spawn client A `winmux -L <s> new -s work`; wait `PS `; type `echo persist-42\r`; wait `persist-42`; send `\x02d`; A's process exits (10s); A's captured raw output ends with `[detached (from session work)]`; `winmux -L <s> ls` (plain Command, no ConPTY) stdout matches `^work: 1 windows .*$` without `(attached)`; spawn client B `winmux -L <s> attach -t work`; screen shows `persist-42` again.
  - `e2e_windows_roundtrip`: new session; `\x02c` → status row shows `0:powershell- 1:powershell*`; `\x02p` → flags swap; `\x02,` + `\x7f`×10 + `edit\r` → `1:edit` visible? (after `\x02p`, current is 0 — send rename before `\x02p`); assert exact status substrings.
  - `e2e_kill_session_exits_client`: attached client; `winmux kill-session -t <name>` from outside → client exits printing `[exited]`; subsequent `winmux ls` → stderr `no server running on` (exit-empty took the server down) OR `no sessions` if a second session held it — use single session, expect the former.
  - `e2e_no_server_error`: `winmux -L fresh-<pid> ls` → exit 1, stderr contains `no server running on`.
- [ ] **Step 2:** `cargo test --test e2e_sessions` → FAIL. **Step 3:** fix whatever they catch (this task is allowed to patch server/client/cli bugs). **Step 4:** ALL tests green: `cargo test` + `cargo test --test e2e --test e2e_sessions -- --test-threads=1` (serialize e2e to keep ConPTY load sane); clippy. **Step 5:** commit `test(e2e): detach/reattach, windows, kill-session full workflows`.

### Task 10: Remaining follow-ups + docs

**Files:**
- Modify: `src/layout.rs` (#5 `saturating_add` in Right/Down adjacency), `src/grid.rs` (#6 panic message with col/row/cols/rows), `docs/follow-ups.md` (mark 1-6 resolved with where), `docs/overview.md` (sub-project 2 delivered), `CLAUDE.md` (architecture/commands refresh: server/client, new modules, `-L` test convention, e2e files)

- [ ] **Step 1:** Failing tests: `layout::` adjacency overflow test (pane at extreme u16 coords — construct area near u16::MAX), `grid::` `#[should_panic(expected = "cell(90, 5) out of bounds 80x24")]`-style test.
- [ ] **Step 2:** FAIL → **Step 3:** fix. **Step 4:** full suite + clippy. **Step 5:** docs updates; commit `chore: resolve MVP follow-ups, refresh docs for server/client architecture`.

---

## Self-review notes

- Spec coverage: design-spec sections ↔ tasks: transport→1,2; model→3; input→4; status→5; server→6,7; CLI/client→8; tests→6,7,9; follow-ups→6 (#1,#2,#4), 8 (#3), 10 (#5,#6). Deferred intentionally (sub-projects 3/4): choose-tree (`w`/`s`), `q`/`!`/Space/`{}`, command prompt `:`, config, mouse, copy mode.
- Type consistency check: `PaneId=u32` (layout), `WindowId=u32` (model), `ClientId=u64` (server-private); `status_spans`/`StatusSpan` names consistent across render/status/server; `Exit{code,msg}` used by client for both stdout/stderr paths.
- The old `tests/e2e.rs` bridging happens in Task 8 (minimal) and Task 9 (full) — no task leaves the tree red at commit time.
