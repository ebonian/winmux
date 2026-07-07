//! Headless multiplexer server: owns every session/window/pane, accepts
//! client connections over a named pipe, composes a per-client VT stream,
//! and routes input back to panes. See `docs/specs/2026-07-07-server-client-design.md`
//! ("Server architecture", "Data model", "Input routing", "Transport") and
//! the `## server` section of the sibling interfaces contract.
//!
//! Only [`run`] is public; everything below is the server's private state
//! machine.
//!
//! ## Design choices (see task-6-report.md for the full write-up)
//!
//! - **Confirm race** (follow-ups #2): NOT fixed here. `Ctrl-b x` and a
//!   following `y` arriving in the SAME `Stdin` frame still race exactly as
//!   in the MVP (the `y` gets forwarded to the pane instead of confirming),
//!   because `KeyMachine::feed` decodes the whole frame before the caller
//!   (this module) gets a chance to call `set_capture` to arm the confirm.
//!   This is one of the two sanctioned options in the task brief; documented
//!   here as still-open rather than half-fixed.
//! - **Render strategy**: every dirty turn (i.e. any event at all, after
//!   coalescing) re-renders ALL attached clients, not just those whose
//!   session actually changed. Simpler and correct at this scale; a
//!   per-session dirty set would cut redundant renders for unrelated
//!   sessions but isn't needed yet.
//! - **Test thread lifecycle**: `tests/server_proto.rs` gives every test a
//!   unique pipe name and, where the test's flow naturally destroys every
//!   session, joins the server thread to prove clean exit-empty shutdown.

mod dispatch;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};

use crate::bindings::Bindings;
use crate::cmd::RawCmd;
use crate::geom::Rect;
use crate::grid::Grid;
use crate::input::{KeyInputEvent, KeyMachine, WhichTable};
use crate::layout::{Layout, PaneId, MIN_PANE_H, MIN_PANE_W};
use crate::model::{Registry, Session, WindowId};
use crate::options::Options;
use crate::pipe::{PipeConn, PipeListener};
use crate::protocol::{self, read_client_msg, write_server_msg, AttachMode, ClientMsg, ServerMsg};
use crate::pty::Pty;
use crate::render::{PaneView, Renderer, Scene};
use crate::status::{status_spans, WindowEntry};

/// Abbreviated month names for the status-bar clock (`DD-Mon-YY`) and the
/// CLI's `ls` creation-time format.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Transient status-line message lifetime (tmux `display-time` default).
const MESSAGE_LIFETIME: Duration = Duration::from_millis(750);

/// Server-global, monotonically increasing client id (distinct id space
/// from `PaneId`/`WindowId`).
type ClientId = u32;

/// Messages funneled from every worker thread into the single-consumer main
/// loop. See the design spec's "Server architecture" diagram.
enum ServerEvent {
    /// ConPTY output for a pane (pane reader thread).
    Output(PaneId, Vec<u8>),
    /// A pane's child process exited (pane waiter thread).
    Exited(PaneId),
    /// A client connected (accept thread).
    Connected(PipeConn),
    /// A decoded frame from an attached (or not-yet-attached) client
    /// (per-client reader thread).
    FromClient(ClientId, ClientMsg),
    /// A client's connection closed/errored (per-client reader thread).
    ClientGone(ClientId),
    /// 50ms coalescing tick: refresh the clock.
    Tick,
}

/// One pane's live resources. `pty` is dropped (set to `None`) the moment
/// the child exits (follow-up #1) — this frees the pseudoconsole/conhost
/// immediately rather than waiting for the pane to be closed from the UI.
struct PaneRuntime {
    pty: Option<Pty>,
    grid: Grid,
    dead: bool,
}

/// Which status-line prompt is in progress (`,` rename-window, `$`
/// rename-session, or `:` command-prompt) — determines the label text and
/// what a commit does with the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    RenameWindow,
    RenameSession,
    /// `:` command-prompt (Task 6): commit parses the buffer as a command
    /// line and dispatches it instead of renaming anything.
    Command,
}

/// Per-client confirm/prompt state. `ConfirmCmd` (Task 6) generalizes the
/// legacy `ConfirmKillPane`/`ConfirmKillWindow` variants into a single
/// "confirm-before"-shaped mode: the wrapped command(s) to dispatch on
/// y/Y/Enter, plus a snapshot of the pane/window that was live when the
/// confirm was armed (staleness check — see `cancel_stale_confirms` and
/// `dispatch::Server::feed_confirm_byte`).
enum ClientMode {
    Normal,
    ConfirmCmd {
        prompt: String,
        cmds: Vec<RawCmd>,
        pane_snapshot: Option<PaneId>,
        window_snapshot: Option<WindowId>,
    },
    /// Status-line line editor (rename-window / rename-session / `:`
    /// command-prompt). `buf` is the live-edited text (pre-filled with the
    /// current name, or empty/initial for `:`); `label` is the fixed prefix.
    Prompt { label: String, buf: String, kind: PromptKind },
}

/// Per-client attached state.
struct ClientState {
    session: Option<String>,
    cols: u16,
    rows: u16,
    renderer: Renderer,
    key_machine: KeyMachine,
    mode: ClientMode,
    /// A transient status-line message (e.g. `window not found: 5`) and when
    /// it was set; cleared on the next `Stdin` frame from this client OR
    /// after `MESSAGE_LIFETIME` elapses (checked on `Tick`). Shown only while
    /// `mode` is `Normal` (confirm/prompt overlays take priority).
    message: Option<(String, Instant)>,
    /// Feeds the client's writer thread (which owns the actual `Write` half
    /// of the pipe and drains this channel so a slow client never blocks
    /// the main loop).
    tx: Sender<Vec<u8>>,
}

/// All server state, owned by the single main-loop thread — no locks.
struct Server {
    registry: Registry,
    panes: HashMap<PaneId, PaneRuntime>,
    /// Last rect applied to each pane (pty resize + grid resize), so
    /// `apply_layout` only touches panes whose rect actually changed.
    last_rects: HashMap<PaneId, Rect>,
    clients: HashMap<ClientId, ClientState>,
    /// Writer channels for connections that haven't completed `Attach` yet.
    pending_writers: HashMap<ClientId, Sender<Vec<u8>>>,
    next_pane_id: PaneId,
    next_client_id: ClientId,
    /// Set the first time any session is created; `run`'s exit-empty check
    /// only fires once this is true (an empty registry at STARTUP, before
    /// any client has attached, must not be mistaken for exit-empty).
    had_session: bool,
    clock: String,
    tx: Sender<ServerEvent>,
    /// Typed tmux option registry (Task 6): `prefix`, `default-command`,
    /// `renumber-windows`, styles, etc. One global instance (SP3 scope, no
    /// per-session/window overlays — documented deviation).
    options: Options,
    /// Mutable key-bindings table (`bind-key`/`unbind-key`/`list-keys`).
    /// `Bindings::default()` reproduces every legacy hardcoded binding.
    bindings: Bindings,
}

/// Local wall-clock time formatted `HH:MM DD-Mon-YY`. Duplicated privately
/// from `app.rs` (which dies in Task 8) rather than shared.
fn local_clock() -> String {
    // SAFETY: no preconditions; windows 0.58 returns the SYSTEMTIME by value.
    let st = unsafe { GetLocalTime() };
    let month = MONTHS[(st.wMonth.clamp(1, 12) as usize) - 1];
    let (hh, mm, dd, yy) = (st.wHour, st.wMinute, st.wDay, st.wYear % 100);
    format!("{hh:02}:{mm:02} {dd:02}-{month}-{yy:02}")
}

/// Encode and send one `ServerMsg` (small, never chunked: `Exit`/`CliDone`).
fn send_msg(tx: &Sender<Vec<u8>>, msg: &ServerMsg) {
    let mut buf = Vec::new();
    if write_server_msg(&mut buf, msg).is_ok() {
        let _ = tx.send(buf);
    }
}

/// Encode and send an `Output` payload, chunked so no single frame's
/// declared length reaches `MAX_FRAME` (the codec itself does not enforce
/// this on the write side — see the task brief).
fn send_output(tx: &Sender<Vec<u8>>, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    for chunk in bytes.chunks(protocol::MAX_FRAME as usize) {
        send_msg(tx, &ServerMsg::Output(chunk.to_vec()));
    }
}

/// Spawn a shell in a fresh ConPTY and wire its two worker threads (output
/// reader + process-exit waiter) into the shared event channel. `shell` is
/// the `default-command` option's current value (SP3 Task 6: configurable
/// per `set -g default-command`, replacing the old hardcoded `SHELL`
/// const).
fn spawn_pane(id: PaneId, cols: u16, rows: u16, tx: &Sender<ServerEvent>, shell: &str) -> std::io::Result<PaneRuntime> {
    let mut pty = Pty::spawn(shell, cols.max(1), rows.max(1))?;
    let mut reader = pty.take_reader()?;

    let out_tx = tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.send(ServerEvent::Output(id, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let wait_tx = tx.clone();
    let raw = pty.process_handle_raw();
    thread::spawn(move || {
        // SAFETY: `raw` is a live process HANDLE owned by the Pty, which the
        // main thread keeps alive until after this pane's Exited is handled.
        unsafe { WaitForSingleObject(HANDLE(raw as *mut core::ffi::c_void), INFINITE) };
        let _ = wait_tx.send(ServerEvent::Exited(id));
    });

    let grid = Grid::new(cols.max(1), rows.max(1));
    Ok(PaneRuntime { pty: Some(pty), grid, dead: false })
}

/// Resize every pane whose computed rect changed (pty + grid), caching the
/// last applied rect per pane so unchanged panes are skipped. Same shape as
/// `app.rs`'s `apply_layout`, keyed by `HashMap` instead of a `Vec` (panes
/// now span every session/window, not just one flat list).
fn apply_layout(
    layout: &Layout,
    area: Rect,
    panes: &mut HashMap<PaneId, PaneRuntime>,
    last_rects: &mut HashMap<PaneId, Rect>,
) {
    for (id, rect) in layout.rects(area) {
        if last_rects.get(&id) == Some(&rect) {
            continue;
        }
        if let Some(p) = panes.get_mut(&id) {
            if let Some(pty) = p.pty.as_ref() {
                let _ = pty.resize(rect.w.max(1), rect.h.max(1));
            }
            p.grid.resize(rect.w.max(1), rect.h.max(1));
        }
        last_rects.insert(id, rect);
    }
}

/// Writer thread: owns the write half of the connection, drains an
/// unbounded channel of already-encoded frame bytes so a slow/blocked
/// client can never stall the main loop.
fn spawn_writer(mut conn: PipeConn) -> Sender<Vec<u8>> {
    let (tx, rx) = channel::<Vec<u8>>();
    thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            if conn.write_all(&bytes).is_err() {
                break;
            }
        }
    });
    tx
}

/// Reader thread: decodes client frames until EOF/error, forwarding each to
/// the main loop; a read error (including clean EOF) reports `ClientGone`.
fn spawn_client_reader(id: ClientId, mut conn: PipeConn, tx: Sender<ServerEvent>) {
    thread::spawn(move || loop {
        match read_client_msg(&mut conn) {
            Ok(msg) => {
                if tx.send(ServerEvent::FromClient(id, msg)).is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = tx.send(ServerEvent::ClientGone(id));
                break;
            }
        }
    });
}

/// `(` / `)` — move `client` to the session adjacent to `*session_name` in
/// registry creation order (wraps). Returns `None` (no-op) with a single
/// session or if the current session is somehow already gone. On an actual
/// switch, updates `client.session`/`*session_name` and forces a full
/// repaint (`Renderer::resize` unconditionally sets `force_full`), and
/// returns `(old_name, new_name)` so the caller can recompute both
/// sessions' sizes/layouts once `client` is back in `self.clients`.
fn switch_client_session(
    registry: &mut Registry,
    client: &mut ClientState,
    session_name: &mut String,
    next: bool,
) -> Option<(String, String)> {
    let neighbor = registry.neighbor_session(session_name, next)?.to_string();
    if neighbor == *session_name {
        return None;
    }
    let old = std::mem::replace(session_name, neighbor.clone());
    client.session = Some(neighbor.clone());
    client.renderer.resize(client.cols.max(1), client.rows.max(1));
    Some((old, neighbor))
}

impl Server {
    fn new(tx: Sender<ServerEvent>) -> Self {
        Server {
            registry: Registry::new(),
            panes: HashMap::new(),
            last_rects: HashMap::new(),
            clients: HashMap::new(),
            pending_writers: HashMap::new(),
            next_pane_id: 1,
            next_client_id: 1,
            had_session: false,
            clock: local_clock(),
            tx,
            options: Options::new(),
            bindings: Bindings::default(),
        }
    }

    fn mint_pane_id(&mut self) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    /// Dispatch one event; returns whether a render pass is warranted this
    /// turn (before coalescing — the caller ORs these across a whole batch).
    fn handle_event(&mut self, ev: ServerEvent) -> bool {
        match ev {
            ServerEvent::Output(id, bytes) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.grid.feed(&bytes);
                }
                true
            }
            ServerEvent::Exited(id) => self.handle_exited(id),
            ServerEvent::Connected(conn) => {
                self.handle_connected(conn);
                false
            }
            ServerEvent::FromClient(id, msg) => self.handle_client_msg(id, msg),
            ServerEvent::ClientGone(id) => {
                self.handle_client_gone(id);
                false
            }
            ServerEvent::Tick => {
                let now = local_clock();
                let mut dirty = if now != self.clock {
                    self.clock = now;
                    true
                } else {
                    false
                };
                let deadline = Instant::now();
                for client in self.clients.values_mut() {
                    if let Some((_, set_at)) = client.message {
                        if deadline.duration_since(set_at) >= MESSAGE_LIFETIME {
                            client.message = None;
                            dirty = true;
                        }
                    }
                }
                dirty
            }
        }
    }

    fn handle_connected(&mut self, conn: PipeConn) {
        let id = self.next_client_id;
        self.next_client_id += 1;
        let reader_conn = match conn.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        };
        let writer_tx = spawn_writer(conn);
        spawn_client_reader(id, reader_conn, self.tx.clone());
        self.pending_writers.insert(id, writer_tx);
    }

    fn handle_client_gone(&mut self, id: ClientId) {
        self.pending_writers.remove(&id);
        if let Some(client) = self.clients.remove(&id) {
            if let Some(name) = client.session {
                self.recompute_session_size(&name);
                self.apply_layout_for_session(&name);
            }
        }
    }

    fn handle_client_msg(&mut self, id: ClientId, msg: ClientMsg) -> bool {
        match msg {
            ClientMsg::Attach { mode, detach_others, cols, rows, name } => {
                self.handle_attach(id, mode, detach_others, cols, rows, name);
            }
            ClientMsg::Stdin(bytes) => self.handle_stdin(id, bytes),
            ClientMsg::Resize { cols, rows } => self.handle_resize(id, cols, rows),
            ClientMsg::Detach => self.handle_detach_frame(id),
            ClientMsg::Cli(argv) => self.handle_cli(id, argv),
        }
        true
    }

    fn handle_attach(
        &mut self,
        id: ClientId,
        mode: AttachMode,
        detach_others: bool,
        cols: u16,
        rows: u16,
        name: String,
    ) {
        let writer_tx = match self.pending_writers.remove(&id) {
            Some(tx) => tx,
            None => return, // already attached, or unknown client id
        };
        let pane_rows = rows.saturating_sub(1).max(1);
        let size = (cols.max(1), pane_rows);

        match mode {
            AttachMode::NewAuto => {
                let pane_id = self.mint_pane_id();
                let shell = self.options.default_command().to_string();
                match spawn_pane(pane_id, size.0, size.1, &self.tx, &shell) {
                    Ok(pr) => {
                        self.panes.insert(pane_id, pr);
                        let session_name = self
                            .registry
                            .create_session(None, pane_id, size)
                            .expect("auto-assigned name never duplicates")
                            .name
                            .clone();
                        self.finish_attach(id, writer_tx, session_name, cols, rows);
                    }
                    Err(e) => {
                        send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: format!("failed to spawn shell: {e}") });
                    }
                }
            }
            AttachMode::NewNamed => {
                if self.registry.sessions().iter().any(|s| s.name == name) {
                    send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: format!("duplicate session: {name}") });
                    return;
                }
                let pane_id = self.mint_pane_id();
                let shell = self.options.default_command().to_string();
                match spawn_pane(pane_id, size.0, size.1, &self.tx, &shell) {
                    Ok(pr) => {
                        self.panes.insert(pane_id, pr);
                        match self.registry.create_session(Some(&name), pane_id, size) {
                            Ok(session) => {
                                let n = session.name.clone();
                                self.finish_attach(id, writer_tx, n, cols, rows);
                            }
                            Err(e) => {
                                // Roll back: drop the just-spawned pane (kills the shell).
                                self.panes.remove(&pane_id);
                                send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: e });
                            }
                        }
                    }
                    Err(e) => {
                        send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: format!("failed to spawn shell: {e}") });
                    }
                }
            }
            AttachMode::Existing => match self.registry.find(&name) {
                Ok(session) => {
                    let session_name = session.name.clone();
                    if detach_others {
                        self.detach_others(&session_name);
                    }
                    self.finish_attach(id, writer_tx, session_name, cols, rows);
                }
                Err(e) => send_msg(&writer_tx, &ServerMsg::Exit { code: 1, msg: e }),
            },
        }
    }

    /// Common tail of a successful attach: register the client, then
    /// recompute the session's shared size and reapply its layout.
    fn finish_attach(&mut self, id: ClientId, tx: Sender<Vec<u8>>, session_name: String, cols: u16, rows: u16) {
        let mut renderer = Renderer::new(cols.max(1), rows.max(1));
        // Force a full repaint on the very first compose (see module docs /
        // task brief: "use a fresh Renderer (or resize) at attach").
        renderer.resize(cols.max(1), rows.max(1));
        let client = ClientState {
            session: Some(session_name.clone()),
            cols,
            rows,
            renderer,
            key_machine: KeyMachine::new(self.options.prefix()),
            mode: ClientMode::Normal,
            message: None,
            tx,
        };
        self.clients.insert(id, client);
        self.had_session = true;
        self.recompute_session_size(&session_name);
        self.apply_layout_for_session(&session_name);
    }

    /// `detach_others`: every OTHER client currently attached to `session_name`
    /// gets a plain `[detached]` (distinct from the `Detach`-action/-frame
    /// message, which names the session).
    fn detach_others(&mut self, session_name: &str) {
        let ids: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session.as_deref() == Some(session_name))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(c) = self.clients.remove(&id) {
                send_msg(&c.tx, &ServerMsg::Exit { code: 0, msg: "[detached]".to_string() });
            }
        }
    }

    fn handle_resize(&mut self, id: ClientId, cols: u16, rows: u16) {
        let session_name = match self.clients.get_mut(&id) {
            Some(c) => {
                c.cols = cols;
                c.rows = rows;
                c.renderer.resize(cols.max(1), rows.max(1));
                c.session.clone()
            }
            None => return,
        };
        if let Some(name) = session_name {
            self.recompute_session_size(&name);
            self.apply_layout_for_session(&name);
        }
    }

    fn handle_detach_frame(&mut self, id: ClientId) {
        if let Some(client) = self.clients.remove(&id) {
            let name = client.session.clone().unwrap_or_default();
            send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {name})]") });
            self.recompute_session_size(&name);
            self.apply_layout_for_session(&name);
        }
    }

    fn handle_cli(&mut self, id: ClientId, argv: Vec<String>) {
        let tx = if let Some(c) = self.clients.get(&id) {
            c.tx.clone()
        } else if let Some(tx) = self.pending_writers.get(&id) {
            tx.clone()
        } else {
            return;
        };
        let (code, out, err) = self.execute_cli_argv(&argv);
        send_msg(&tx, &ServerMsg::CliDone { code, out, err });
    }

    /// Rename every attached client's `session` reference from `old` to
    /// `new` (a session's own `name` field is updated by the caller
    /// separately). Needed because clients look their session up by name.
    fn rename_session_everywhere(&mut self, old: &str, new: &str) {
        for c in self.clients.values_mut() {
            if c.session.as_deref() == Some(old) {
                c.session = Some(new.to_string());
            }
        }
    }

    /// Cancel any attached client's pending confirm-before whose snapshotted
    /// pane/window no longer exists (it exited naturally, or another client
    /// killed it, while the `(y/n)` prompt was up): reset the mode to
    /// `Normal`, drop capture (so the next key is normal input again, e.g. a
    /// `y` gets FORWARDED to the pane instead of confirming), and clear any
    /// transient message. `dispatch::Server::feed_confirm_byte` ALSO
    /// re-validates its target before acting (belt and braces — this method
    /// can't reach a client currently removed from the map mid-
    /// `handle_stdin`), but only this path clears a stale on-screen prompt
    /// without waiting for a keypress.
    fn cancel_stale_confirms(&mut self) {
        let mut live_panes: HashSet<PaneId> = HashSet::new();
        let mut live_windows: HashSet<WindowId> = HashSet::new();
        for s in self.registry.sessions() {
            for w in &s.windows {
                live_windows.insert(w.id);
                live_panes.extend(w.layout.panes());
            }
        }
        for client in self.clients.values_mut() {
            let stale = match &client.mode {
                ClientMode::ConfirmCmd { pane_snapshot, window_snapshot, .. } => {
                    pane_snapshot.is_some_and(|p| !live_panes.contains(&p))
                        || window_snapshot.is_some_and(|w| !live_windows.contains(&w))
                }
                _ => false,
            };
            if stale {
                client.mode = ClientMode::Normal;
                client.key_machine.set_capture(false);
                client.message = None;
            }
        }
    }

    /// Session's shared size = min over its attached clients of
    /// `(cols, rows - 1)` (the status row is not part of the pane area).
    /// No attached clients: keep the last size.
    fn recompute_session_size(&mut self, name: &str) {
        let mut min: Option<(u16, u16)> = None;
        for c in self.clients.values().filter(|c| c.session.as_deref() == Some(name)) {
            let contribution = (c.cols.max(1), c.rows.saturating_sub(1).max(1));
            min = Some(match min {
                Some(m) => (m.0.min(contribution.0), m.1.min(contribution.1)),
                None => contribution,
            });
        }
        if let Some(size) = min {
            if let Some(session) = self.registry.session_mut(name) {
                session.size = size;
            }
        }
    }

    fn apply_layout_for_session(&mut self, name: &str) {
        let Some(session) = self.registry.session_mut(name) else { return };
        let size = session.size;
        let area = Rect { x: 0, y: 0, w: size.0, h: size.1 };
        let window = session.current_window_mut();
        apply_layout(&window.layout, area, &mut self.panes, &mut self.last_rects);
    }

    /// Natural pane exit: tmux `remain-on-exit off` parity. If other panes in
    /// the SAME window are still alive, this pane is removed outright (same
    /// path as a confirmed kill) instead of leaving a dead `[exited]`
    /// overlay. If it was the window's last live pane, the whole window
    /// dies; if that was the session's last window, the session dies too
    /// (attached clients get `Exit{0, "[exited]"}`, same as a confirmed
    /// last-pane kill).
    fn handle_exited(&mut self, pane_id: PaneId) -> bool {
        if let Some(p) = self.panes.get_mut(&pane_id) {
            p.pty = None; // drop the Pty immediately (follow-up #1)
            p.dead = true;
        }

        let owner: Option<(String, WindowId)> = self.registry.sessions().iter().find_map(|s| {
            s.windows
                .iter()
                .find(|w| w.layout.panes().contains(&pane_id))
                .map(|w| (s.name.clone(), w.id))
        });
        let Some((session_name, window_id)) = owner else {
            return true;
        };

        let other_panes_alive = self
            .registry
            .sessions()
            .iter()
            .find(|s| s.name == session_name)
            .and_then(|s| s.windows.iter().find(|w| w.id == window_id))
            .map(|w| {
                w.layout
                    .panes()
                    .iter()
                    .any(|pid| *pid != pane_id && !self.panes.get(pid).map(|p| p.dead).unwrap_or(true))
            })
            .unwrap_or(false);

        if other_panes_alive {
            if let Some(session) = self.registry.session_mut(&session_name) {
                if let Some(window) = session.windows.iter_mut().find(|w| w.id == window_id) {
                    window.layout.remove(pane_id);
                }
            }
            self.panes.remove(&pane_id);
            self.last_rects.remove(&pane_id);
            self.apply_layout_for_session(&session_name);
        } else {
            let is_only_window = self
                .registry
                .sessions()
                .iter()
                .find(|s| s.name == session_name)
                .map(|s| s.windows.len() == 1)
                .unwrap_or(false);
            if is_only_window {
                self.destroy_session(&session_name);
            } else {
                let pane_ids: Vec<PaneId> = self
                    .registry
                    .sessions()
                    .iter()
                    .find(|s| s.name == session_name)
                    .and_then(|s| s.windows.iter().find(|w| w.id == window_id))
                    .map(|w| w.layout.panes())
                    .unwrap_or_default();
                if let Some(session) = self.registry.session_mut(&session_name) {
                    session.kill_window(window_id);
                }
                for pid in pane_ids {
                    self.panes.remove(&pid);
                    self.last_rects.remove(&pid);
                }
                self.apply_layout_for_session(&session_name);
            }
        }
        // The removal above may have invalidated a client's pending confirm;
        // any confirm on it must be reset, or its `y` would act on stale state.
        self.cancel_stale_confirms();
        true
    }

    /// Tear down a session: drop all its panes, remove it from the
    /// registry, and tell every attached client `Exit{0, "[exited]"}`.
    fn destroy_session(&mut self, name: &str) {
        if let Some(session) = self.registry.session_mut(name) {
            let pane_ids: Vec<PaneId> = session.windows.iter().flat_map(|w| w.layout.panes()).collect();
            for pid in pane_ids {
                self.panes.remove(&pid);
                self.last_rects.remove(&pid);
            }
        }
        self.registry.kill_session(name);
        let ids: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session.as_deref() == Some(name))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(client) = self.clients.remove(&id) {
                send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: "[exited]".to_string() });
            }
        }
    }

    /// Route one `Stdin` frame through the client's `KeyMachine` and
    /// dispatch the resulting events one at a time against live state via
    /// the command dispatcher (`dispatch.rs`) and the mutable `bindings`
    /// table (see module docs re: the confirm race — NOT fixed here, same
    /// as before Task 6).
    fn handle_stdin(&mut self, id: ClientId, bytes: Vec<u8>) {
        let mut client = match self.clients.remove(&id) {
            Some(c) => c,
            None => return,
        };
        let mut session_name = match client.session.clone() {
            Some(n) => n,
            None => {
                self.clients.insert(id, client);
                return;
            }
        };

        // Any input byte from this client clears its transient status
        // message (the other clear path is 750ms elapsing, on Tick).
        client.message = None;

        let now = Instant::now();
        // A queue (not a plain iterator) because a prompt/confirm commit
        // mid-`Captured`-chunk re-feeds the chunk's REMAINING bytes through
        // the KeyMachine and splices the resulting events in at the front
        // (they logically precede everything after the Captured event).
        let mut queue: VecDeque<KeyInputEvent> = client.key_machine.feed(&bytes, now).into();

        let mut detach = false;
        let mut destroy = false;
        let mut session_switched: Option<(String, String)> = None;

        'events: while let Some(ev) = queue.pop_front() {
            match ev {
                KeyInputEvent::Forward(data) => {
                    if let Some(session) = self.registry.session_mut(&session_name) {
                        let fid = session.current_window().layout.focused();
                        if let Some(pane) = self.panes.get_mut(&fid) {
                            if let Some(pty) = pane.pty.as_mut() {
                                let _ = pty.write_input(&data);
                            }
                        }
                    }
                }
                KeyInputEvent::Key { table, key, raw } => {
                    let binding = self.bindings.lookup(table, &key).cloned();
                    match binding {
                        Some(b) => {
                            let outcome = self.dispatch_client(&b.cmds, &mut client, &mut session_name);
                            if b.repeat {
                                client.key_machine.arm_repeat(now);
                            }
                            dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                            if detach || destroy {
                                break 'events;
                            }
                        }
                        None => match table {
                            // Unbound in the root table: forward raw bytes
                            // (tmux behavior for `bind -n`-less keys).
                            WhichTable::Root => {
                                if let Some(session) = self.registry.session_mut(&session_name) {
                                    let fid = session.current_window().layout.focused();
                                    if let Some(pane) = self.panes.get_mut(&fid) {
                                        if let Some(pty) = pane.pty.as_mut() {
                                            let _ = pty.write_input(&raw);
                                        }
                                    }
                                }
                            }
                            // Unbound in the prefix table: swallowed (tmux).
                            WhichTable::Prefix => {}
                        },
                    }
                }
                KeyInputEvent::Captured(chunk) => {
                    let mut i = 0;
                    while i < chunk.len() {
                        let (ended, outcome) = self.feed_mode_byte(&mut client, &mut session_name, chunk[i]);
                        i += 1;
                        if let Some(outcome) = outcome {
                            dispatch::route_outcome(outcome, &mut client, &mut detach, &mut destroy, &mut session_switched);
                        }
                        if detach || destroy {
                            break 'events;
                        }
                        if ended {
                            // Commit/cancel mid-chunk: the rest of the chunk
                            // is normal input again (capture is off) — run it
                            // through the KeyMachine and process its events
                            // next, ahead of anything already queued.
                            if i < chunk.len() {
                                let more = client.key_machine.feed(&chunk[i..], now);
                                for e in more.into_iter().rev() {
                                    queue.push_front(e);
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }

        if detach {
            send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: format!("[detached (from session {session_name})]") });
            self.recompute_session_size(&session_name);
            self.apply_layout_for_session(&session_name);
            return; // client dropped, not reinserted
        }
        if destroy {
            // `destroy_session` (and messaging every OTHER attached client)
            // has ALREADY run inside the dispatcher (`kill_pane_by_id`/
            // `kill_window_by_id`) — this client, removed from `self.clients`
            // at the top of this function, is the only one destroy_session
            // couldn't reach.
            send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: "[exited]".to_string() });
            return; // client dropped, not reinserted
        }
        self.clients.insert(id, client);
        // `(`/`)` switch-client: recompute both sessions' shared sizes/
        // layouts now that the client is back in `self.clients` (so it's
        // correctly counted toward the NEW session and no longer toward the
        // old one).
        if let Some((old, new)) = session_switched {
            self.recompute_session_size(&old);
            self.apply_layout_for_session(&old);
            self.recompute_session_size(&new);
            self.apply_layout_for_session(&new);
        }
        // This client may have just removed a pane/window that ANOTHER
        // client (same session) had a pending kill confirm armed on.
        self.cancel_stale_confirms();
    }

    /// Render every attached client (see module docs: render-all, not
    /// per-session dirty tracking).
    fn render_all(&mut self) {
        let clock = self.clock.clone();
        for client in self.clients.values_mut() {
            let Some(name) = client.session.clone() else { continue };
            let Some(session) = self.registry.sessions().iter().find(|s| s.name == name) else { continue };
            render_one(client, session, &self.panes, &clock);
        }
    }
}

/// Compose and send one client's frame from shared session state.
fn render_one(client: &mut ClientState, session: &Session, panes: &HashMap<PaneId, PaneRuntime>, clock: &str) {
    let window = session.current_window();
    let area = Rect { x: 0, y: 0, w: session.size.0, h: session.size.1 };
    let focused = window.layout.focused();
    let zoomed = window.layout.is_zoomed();
    let rects = window.layout.rects(area);

    let too_small =
        area.w < MIN_PANE_W || area.h < MIN_PANE_H || rects.iter().any(|(_, r)| r.w < MIN_PANE_W || r.h < MIN_PANE_H);

    // Precedence matches the pre-Task-6 code exactly: `ConfirmCmd` (the
    // Task 6 generalization of the legacy `ConfirmKillPane`/`ConfirmKillWindow`
    // variants) had no match guard, so it (and `Prompt`) always wins
    // regardless of `too_small` — a confirm/prompt overlay doesn't depend on
    // pane space, so it stays visible even on a too-small terminal.
    // `too_small` only applies in `Normal` mode, where it additionally takes
    // priority over a transient status message.
    let message = match &client.mode {
        ClientMode::ConfirmCmd { prompt, .. } => Some(prompt.clone()),
        ClientMode::Prompt { label, buf, .. } => Some(format!("{label}{buf}")),
        ClientMode::Normal if too_small => Some("terminal too small".to_string()),
        ClientMode::Normal => client.message.as_ref().map(|(msg, _)| msg.clone()),
    };

    let entries: Vec<WindowEntry> = session
        .windows
        .iter()
        .map(|w| WindowEntry {
            index: w.index,
            name: w.name.clone(),
            current: w.id == session.current,
            last: Some(w.id) == session.last,
            zoomed: w.layout.is_zoomed(),
        })
        .collect();
    let spans = status_spans(&session.name, &entries);
    let scene_size = (client.cols, client.rows);

    if too_small {
        let scene = Scene { size: scene_size, panes: Vec::new(), zoomed, status_spans: spans, status_right: clock.to_string(), message };
        let out = client.renderer.compose(&scene, None, false);
        send_output(&client.tx, out);
        return;
    }

    let mut views = Vec::with_capacity(rects.len());
    for (id, rect) in &rects {
        if let Some(p) = panes.get(id) {
            views.push(PaneView { id: *id, rect: *rect, grid: &p.grid, focused: *id == focused, dead: p.dead });
        }
    }

    let (cursor, cursor_visible) = match (rects.iter().find(|(id, _)| *id == focused).map(|(_, r)| *r), panes.get(&focused)) {
        (Some(r), Some(p)) => {
            let (cx, cy) = p.grid.cursor();
            let visible = p.grid.cursor_visible() && !p.dead && message.is_none();
            (Some((r.x + cx, r.y + cy)), visible)
        }
        _ => (None, false),
    };

    let scene = Scene { size: scene_size, panes: views, zoomed, status_spans: spans, status_right: clock.to_string(), message };
    let out = client.renderer.compose(&scene, cursor, cursor_visible);
    send_output(&client.tx, out);
}

/// Run the multiplexer server: bind `pipe_full_name`, accept clients, and
/// loop until every session has died (exit-empty). Does not touch the
/// console and installs no panic hook (both are `main.rs`'s job, Task 8).
pub fn run(pipe_full_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let listener = PipeListener::bind(pipe_full_name)?;
    let (tx, rx) = channel::<ServerEvent>();

    {
        let accept_tx = tx.clone();
        thread::spawn(move || {
            while let Ok(conn) = listener.accept() {
                if accept_tx.send(ServerEvent::Connected(conn)).is_err() {
                    break;
                }
            }
        });
    }

    let mut server = Server::new(tx);

    loop {
        let first = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => ServerEvent::Tick,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut dirty = server.handle_event(first);
        while let Ok(ev) = rx.try_recv() {
            dirty |= server.handle_event(ev);
        }

        if dirty {
            server.render_all();
        }

        if server.had_session && server.registry.is_empty() {
            break;
        }
    }

    Ok(())
}
