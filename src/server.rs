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
//!   because `InputMachine::feed` tokenizes the whole frame before the
//!   caller (this module) gets a chance to call `set_confirming`. This is
//!   one of the two sanctioned options in the task brief; documented here
//!   as still-open rather than half-fixed.
//! - **Render strategy**: every dirty turn (i.e. any event at all, after
//!   coalescing) re-renders ALL attached clients, not just those whose
//!   session actually changed. Simpler and correct at this scale; a
//!   per-session dirty set would cut redundant renders for unrelated
//!   sessions but isn't needed yet.
//! - **Test thread lifecycle**: `tests/server_proto.rs` gives every test a
//!   unique pipe name and, where the test's flow naturally destroys every
//!   session, joins the server thread to prove clean exit-empty shutdown.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};

use crate::geom::Rect;
use crate::grid::Grid;
use crate::input::{Action, InputEvent, InputMachine};
use crate::layout::{Layout, PaneId, MIN_PANE_H, MIN_PANE_W};
use crate::model::{Registry, Session};
use crate::pipe::{PipeConn, PipeListener};
use crate::protocol::{self, read_client_msg, write_server_msg, AttachMode, ClientMsg, ServerMsg};
use crate::pty::Pty;
use crate::render::{PaneView, Renderer, Scene};
use crate::status::{status_spans, WindowEntry};

/// Shell launched in every pane (matches the MVP; sub-project 3 makes this
/// configurable).
const SHELL: &str = "powershell.exe -NoLogo";

/// Abbreviated month names for the status-bar clock (`DD-Mon-YY`).
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

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

/// Per-client confirm/prompt state. Only `Normal` and `ConfirmKillPane` are
/// reachable from THIS task's scope (window ops — `ConfirmKillWindow`,
/// rename prompts — are Task 7's job); that variant is intentionally not
/// modeled here yet to avoid unused-variant dead code under `-D warnings`.
enum ClientMode {
    Normal,
    ConfirmKillPane(PaneId),
}

/// Per-client attached state.
struct ClientState {
    session: Option<String>,
    cols: u16,
    rows: u16,
    renderer: Renderer,
    input: InputMachine,
    mode: ClientMode,
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
/// reader + process-exit waiter) into the shared event channel. Copied from
/// `app.rs`'s `spawn_pane` (see module docs / CLAUDE.md: `app.rs` remains
/// the reference implementation for pane plumbing until Task 8).
fn spawn_pane(id: PaneId, cols: u16, rows: u16, tx: &Sender<ServerEvent>) -> std::io::Result<PaneRuntime> {
    let mut pty = Pty::spawn(SHELL, cols.max(1), rows.max(1))?;
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
                if now != self.clock {
                    self.clock = now;
                    true
                } else {
                    false
                }
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
            ClientMsg::Cli(_) => self.handle_cli(id),
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
                match spawn_pane(pane_id, size.0, size.1, &self.tx) {
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
                match spawn_pane(pane_id, size.0, size.1, &self.tx) {
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
            input: InputMachine::new(),
            mode: ClientMode::Normal,
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

    fn handle_cli(&mut self, id: ClientId) {
        let tx = if let Some(c) = self.clients.get(&id) {
            c.tx.clone()
        } else if let Some(tx) = self.pending_writers.get(&id) {
            tx.clone()
        } else {
            return;
        };
        send_msg(&tx, &ServerMsg::CliDone { code: 1, out: String::new(), err: "unknown command".to_string() });
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

    fn handle_exited(&mut self, pane_id: PaneId) -> bool {
        if let Some(p) = self.panes.get_mut(&pane_id) {
            p.pty = None; // drop the Pty immediately (follow-up #1)
            p.dead = true;
        }
        let owner = self
            .registry
            .sessions()
            .iter()
            .find(|s| s.windows.iter().any(|w| w.layout.panes().contains(&pane_id)))
            .map(|s| s.name.clone());
        if let Some(name) = owner {
            // Task 6 sessions only ever have one window (NewWindow is a
            // no-op until Task 7), so "last pane of last window" reduces to
            // "every pane of the current window is dead".
            let all_dead = self
                .registry
                .sessions()
                .iter()
                .find(|s| s.name == name)
                .map(|s| {
                    s.current_window()
                        .layout
                        .panes()
                        .iter()
                        .all(|pid| self.panes.get(pid).map(|p| p.dead).unwrap_or(true))
                })
                .unwrap_or(false);
            if all_dead {
                self.destroy_session(&name);
            }
        }
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

    /// Route one `Stdin` frame through the client's `InputMachine` and
    /// dispatch the resulting events one at a time against live state (see
    /// module docs re: the confirm race — NOT fixed here).
    fn handle_stdin(&mut self, id: ClientId, bytes: Vec<u8>) {
        let mut client = match self.clients.remove(&id) {
            Some(c) => c,
            None => return,
        };
        let session_name = match client.session.clone() {
            Some(n) => n,
            None => {
                self.clients.insert(id, client);
                return;
            }
        };

        let now = Instant::now();
        let events = client.input.feed(&bytes, now);

        let mut detach = false;
        let mut destroy = false;

        'events: for ev in events {
            match ev {
                InputEvent::Forward(data) => {
                    if let Some(session) = self.registry.session_mut(&session_name) {
                        let fid = session.current_window().layout.focused();
                        if let Some(pane) = self.panes.get_mut(&fid) {
                            if let Some(pty) = pane.pty.as_mut() {
                                let _ = pty.write_input(&data);
                            }
                        }
                    }
                }
                InputEvent::Action(action) => match action {
                    Action::Split(dir) => {
                        let size = self.registry.session_mut(&session_name).map(|s| s.size);
                        if let Some(size) = size {
                            let area = Rect { x: 0, y: 0, w: size.0, h: size.1 };
                            let new_id = self.mint_pane_id();
                            let split_ok = self
                                .registry
                                .session_mut(&session_name)
                                .map(|s| s.current_window_mut().layout.split(dir, new_id, area).is_ok())
                                .unwrap_or(false);
                            if split_ok {
                                let rect = self
                                    .registry
                                    .session_mut(&session_name)
                                    .and_then(|s| {
                                        s.current_window().layout.rects(area).into_iter().find(|(pid, _)| *pid == new_id)
                                    })
                                    .map(|(_, r)| r)
                                    .unwrap_or(area);
                                match spawn_pane(new_id, rect.w.max(1), rect.h.max(1), &self.tx) {
                                    Ok(pr) => {
                                        self.panes.insert(new_id, pr);
                                        self.apply_layout_for_session(&session_name);
                                    }
                                    Err(_) => {
                                        if let Some(s) = self.registry.session_mut(&session_name) {
                                            s.current_window_mut().layout.remove(new_id);
                                        }
                                        self.apply_layout_for_session(&session_name);
                                    }
                                }
                            }
                        }
                    }
                    Action::Focus(dir) => {
                        if let Some(session) = self.registry.session_mut(&session_name) {
                            let size = session.size;
                            let area = Rect { x: 0, y: 0, w: size.0, h: size.1 };
                            session.current_window_mut().layout.focus_dir(dir, area);
                        }
                    }
                    Action::FocusNext => {
                        if let Some(session) = self.registry.session_mut(&session_name) {
                            session.current_window_mut().layout.focus_next();
                        }
                    }
                    Action::FocusLast => {
                        if let Some(session) = self.registry.session_mut(&session_name) {
                            session.current_window_mut().layout.focus_last();
                        }
                    }
                    Action::RequestClose => {
                        if let Some(session) = self.registry.session_mut(&session_name) {
                            let focused = session.current_window().layout.focused();
                            client.mode = ClientMode::ConfirmKillPane(focused);
                            client.input.set_confirming(true);
                        }
                    }
                    Action::ToggleZoom => {
                        if let Some(session) = self.registry.session_mut(&session_name) {
                            session.current_window_mut().layout.toggle_zoom();
                        }
                        self.apply_layout_for_session(&session_name);
                    }
                    Action::Resize(dir) => {
                        if let Some(session) = self.registry.session_mut(&session_name) {
                            let size = session.size;
                            let area = Rect { x: 0, y: 0, w: size.0, h: size.1 };
                            session.current_window_mut().layout.resize_focused(dir, area, 1);
                        }
                        self.apply_layout_for_session(&session_name);
                    }
                    Action::Detach => {
                        detach = true;
                        break 'events;
                    }
                    Action::NewWindow
                    | Action::NextWindow
                    | Action::PrevWindow
                    | Action::LastWindow
                    | Action::SelectWindow(_)
                    | Action::RequestKillWindow
                    | Action::RenameWindow
                    | Action::RenameSession
                    | Action::SwitchClientPrev
                    | Action::SwitchClientNext
                    | Action::Quit => {
                        // Window/session actions: Task 7. `Quit` is never
                        // emitted by `InputMachine::feed` (MVP-only hook).
                    }
                },
                InputEvent::Captured(_) => {
                    // Raw capture mode (rename prompts): Task 7.
                }
                InputEvent::ConfirmClose(confirmed) => {
                    client.input.set_confirming(false);
                    let mode = std::mem::replace(&mut client.mode, ClientMode::Normal);
                    if let ClientMode::ConfirmKillPane(target) = mode {
                        if confirmed {
                            let removed = self
                                .registry
                                .session_mut(&session_name)
                                .map(|s| s.current_window_mut().layout.remove(target))
                                .unwrap_or(false);
                            if removed {
                                self.panes.remove(&target);
                                self.last_rects.remove(&target);
                                self.apply_layout_for_session(&session_name);
                            } else {
                                // Only pane in the window: the session ends.
                                destroy = true;
                                break 'events;
                            }
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
            send_msg(&client.tx, &ServerMsg::Exit { code: 0, msg: "[exited]".to_string() });
            self.destroy_session(&session_name); // handles any OTHER attached clients
            return; // client dropped, not reinserted
        }
        self.clients.insert(id, client);
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

    let message = match &client.mode {
        ClientMode::ConfirmKillPane(pid) => {
            let idx = window.layout.panes().iter().position(|p| p == pid).unwrap_or(0);
            Some(format!("kill-pane {idx}? (y/n)"))
        }
        _ if too_small => Some("terminal too small".to_string()),
        _ => None,
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
