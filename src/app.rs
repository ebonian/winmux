//! Event loop: wires host + pty + grid + layout + render + input together.
//!
//! This is the I/O composition root. It owns all core state (layout, grids)
//! on the main thread and is the only thing that mutates and renders, so no
//! locks are needed. It has NO unit tests — correctness is proven end-to-end
//! by `tests/e2e.rs`.

use std::collections::HashMap;
use std::io::Read;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};

use crate::geom::Rect;
use crate::grid::Grid;
use crate::host::{self, Host};
use crate::input::{Action, InputEvent, InputMachine};
use crate::layout::{Layout, PaneId, MIN_PANE_H, MIN_PANE_W};
use crate::pty::Pty;
use crate::render::{PaneView, Renderer, Scene};

/// Shell launched in every pane (single window/session MVP).
const SHELL: &str = "powershell.exe -NoLogo";

/// Abbreviated month names for the status-bar clock (`DD-Mon-YY`).
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Messages funneled from the worker threads into the single-consumer main loop.
pub enum Event {
    /// ConPTY output for a pane (reader thread).
    Output(PaneId, Vec<u8>),
    /// A pane's child process exited (waiter thread).
    Exited(PaneId),
    /// Raw bytes read from the host console (stdin thread).
    Stdin(Vec<u8>),
}

struct Pane {
    id: PaneId,
    pty: Pty,
    grid: Grid,
    dead: bool,
}

/// Local wall-clock time formatted `HH:MM DD-Mon-YY` (e.g. `21:04 06-Jul-26`).
///
/// `GetLocalTime` returns pre-computed local calendar fields, so no
/// days-since-epoch date math is required and UTC is avoided entirely.
fn local_clock() -> String {
    // SAFETY: no preconditions; windows 0.58 returns the SYSTEMTIME by value
    // (the brief's `GetLocalTime(&mut st)` out-param form does not match this
    // crate version's signature).
    let st = unsafe { GetLocalTime() };
    let month = MONTHS[(st.wMonth.clamp(1, 12) as usize) - 1];
    let (hh, mm, dd, yy) = (st.wHour, st.wMinute, st.wDay, st.wYear % 100);
    format!("{hh:02}:{mm:02} {dd:02}-{month}-{yy:02}")
}

/// Spawn a shell in a fresh ConPTY and wire its two worker threads (output
/// reader + process-exit waiter) into the shared event channel.
fn spawn_pane(id: PaneId, cols: u16, rows: u16, tx: &Sender<Event>) -> std::io::Result<Pane> {
    let mut pty = Pty::spawn(SHELL, cols.max(1), rows.max(1))?;
    let mut reader = pty.take_reader()?;

    // Reader thread: pump ConPTY output into Event::Output until EOF (Ok(0)).
    let out_tx = tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.send(Event::Output(id, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Waiter thread: block on the child process handle; on exit signal Exited.
    let wait_tx = tx.clone();
    let raw = pty.process_handle_raw();
    thread::spawn(move || {
        // SAFETY: `raw` is a live process HANDLE owned by the Pty, which the
        // main thread keeps alive until after this pane's Exited is handled.
        unsafe { WaitForSingleObject(HANDLE(raw as *mut core::ffi::c_void), INFINITE) };
        let _ = wait_tx.send(Event::Exited(id));
    });

    let grid = Grid::new(cols.max(1), rows.max(1));
    Ok(Pane { id, pty, grid, dead: false })
}

/// Resize every pane whose computed rect changed (pty + grid), caching the
/// last applied rect per pane so unchanged panes are skipped.
fn apply_layout(
    layout: &Layout,
    area: Rect,
    panes: &mut [Pane],
    last_rects: &mut HashMap<PaneId, Rect>,
) {
    for (id, rect) in layout.rects(area) {
        if last_rects.get(&id) == Some(&rect) {
            continue;
        }
        if let Some(p) = panes.iter_mut().find(|p| p.id == id) {
            if !p.dead {
                let _ = p.pty.resize(rect.w.max(1), rect.h.max(1));
            }
            p.grid.resize(rect.w.max(1), rect.h.max(1));
        }
        last_rects.insert(id, rect);
    }
}

/// Compose the current state into a frame and write it to the host terminal.
#[allow(clippy::too_many_arguments)]
fn render(
    host: &mut Host,
    renderer: &mut Renderer,
    layout: &Layout,
    panes: &[Pane],
    area: Rect,
    size: (u16, u16),
    clock: &str,
    confirm_pane: Option<PaneId>,
) -> std::io::Result<()> {
    let focused = layout.focused();
    let zoomed = layout.is_zoomed();

    let too_small = area.w < MIN_PANE_W
        || area.h < MIN_PANE_H
        || layout
            .rects(area)
            .iter()
            .any(|(_, r)| r.w < MIN_PANE_W || r.h < MIN_PANE_H);

    let message = if let Some(id) = confirm_pane {
        Some(format!("kill-pane {id}? (y/n)"))
    } else if too_small {
        Some("terminal too small".to_string())
    } else {
        None
    };

    let status_left = "[winmux] 0:powershell*".to_string();

    // Terminal too small: blank panes, message override, no cursor.
    if too_small {
        let scene = Scene {
            size,
            panes: Vec::new(),
            zoomed,
            status_left,
            status_right: clock.to_string(),
            message,
        };
        let out = renderer.compose(&scene, None, false);
        return host.write(&out);
    }

    let rects = layout.rects(area);
    let mut views = Vec::with_capacity(rects.len());
    for (id, rect) in &rects {
        if let Some(p) = panes.iter().find(|p| p.id == *id) {
            views.push(PaneView {
                id: *id,
                rect: *rect,
                grid: &p.grid,
                focused: *id == focused,
                dead: p.dead,
            });
        }
    }

    // Real cursor: focused pane rect origin + its grid cursor. Hidden while a
    // message is shown or the focused pane is dead.
    let (cursor, cursor_visible) = match (
        rects.iter().find(|(id, _)| *id == focused).map(|(_, r)| *r),
        panes.iter().find(|p| p.id == focused),
    ) {
        (Some(r), Some(p)) => {
            let (cx, cy) = p.grid.cursor();
            let visible = p.grid.cursor_visible() && !p.dead && message.is_none();
            (Some((r.x + cx, r.y + cy)), visible)
        }
        _ => (None, false),
    };

    let scene = Scene {
        size,
        panes: views,
        zoomed,
        status_left,
        status_right: clock.to_string(),
        message,
    };
    let out = renderer.compose(&scene, cursor, cursor_visible);
    host.write(&out)
}

/// Run the multiplexer. Returns `Ok(())` on clean exit (last pane gone).
/// `Host` is a local here, so it is dropped (terminal restored) before this
/// function returns on ANY path, including the `?` error paths.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::enter()?;
    let (mut cols, mut rows) = host.size()?;
    let mut area = Rect { x: 0, y: 0, w: cols, h: rows.saturating_sub(1) };

    let (tx, rx) = channel::<Event>();

    // stdin reader thread.
    {
        let stdin_tx = tx.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match host::read_stdin(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if stdin_tx.send(Event::Stdin(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let first_id: PaneId = 1;
    let mut next_id: PaneId = 2;
    let mut layout = Layout::new(first_id);
    let mut panes: Vec<Pane> = vec![spawn_pane(first_id, area.w, area.h, &tx)?];
    let mut last_rects: HashMap<PaneId, Rect> = HashMap::new();
    apply_layout(&layout, area, &mut panes, &mut last_rects);

    let mut renderer = Renderer::new(cols, rows);
    let mut input = InputMachine::new();
    let mut confirm_pane: Option<PaneId> = None;
    let mut clock = local_clock();

    render(&mut host, &mut renderer, &layout, &panes, area, (cols, rows), &clock, confirm_pane)?;

    let mut exit = false;
    while !exit {
        let mut dirty = false;
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Event::Output(id, bytes)) => {
                if let Some(p) = panes.iter_mut().find(|p| p.id == id) {
                    p.grid.feed(&bytes);
                }
                dirty = true;
            }
            Ok(Event::Exited(id)) => {
                if let Some(p) = panes.iter_mut().find(|p| p.id == id) {
                    p.dead = true;
                }
                // When every pane is dead, the window/session is over.
                if panes.iter().all(|p| p.dead) {
                    exit = true;
                }
                dirty = true;
            }
            Ok(Event::Stdin(bytes)) => {
                for ev in input.feed(&bytes, Instant::now()) {
                    match ev {
                        InputEvent::Forward(data) => {
                            let fid = layout.focused();
                            if let Some(p) = panes.iter_mut().find(|p| p.id == fid) {
                                if !p.dead {
                                    let _ = p.pty.write_input(&data);
                                }
                                // Dead pane: input is discarded.
                            }
                        }
                        InputEvent::Action(action) => match action {
                            Action::Split(dir) => {
                                let new_id = next_id;
                                if layout.split(dir, new_id, area).is_ok() {
                                    next_id += 1;
                                    let new_rect = layout
                                        .rects(area)
                                        .into_iter()
                                        .find(|(id, _)| *id == new_id)
                                        .map(|(_, r)| r)
                                        .unwrap_or(area);
                                    match spawn_pane(new_id, new_rect.w, new_rect.h, &tx) {
                                        Ok(pane) => {
                                            panes.push(pane);
                                            apply_layout(
                                                &layout, area, &mut panes, &mut last_rects,
                                            );
                                        }
                                        Err(_) => {
                                            // Spawn failed: roll the split back.
                                            if layout.remove(new_id) {
                                                apply_layout(
                                                    &layout, area, &mut panes, &mut last_rects,
                                                );
                                            }
                                        }
                                    }
                                }
                                // Err(SplitRefused): too small — ignored.
                            }
                            Action::Focus(dir) => {
                                layout.focus_dir(dir, area);
                            }
                            Action::FocusNext => layout.focus_next(),
                            Action::FocusLast => layout.focus_last(),
                            Action::RequestClose => {
                                confirm_pane = Some(layout.focused());
                                input.set_confirming(true);
                            }
                            Action::ToggleZoom => {
                                layout.toggle_zoom();
                                apply_layout(&layout, area, &mut panes, &mut last_rects);
                            }
                            Action::Resize(dir) => {
                                if layout.resize_focused(dir, area, 1) {
                                    apply_layout(&layout, area, &mut panes, &mut last_rects);
                                }
                            }
                            Action::Quit => exit = true,
                        },
                        InputEvent::ConfirmClose(confirmed) => {
                            input.set_confirming(false);
                            let target = confirm_pane.take();
                            if confirmed {
                                if let Some(id) = target {
                                    if layout.remove(id) {
                                        // Dropping the Pane closes its ConPTY.
                                        panes.retain(|p| p.id != id);
                                        apply_layout(
                                            &layout, area, &mut panes, &mut last_rects,
                                        );
                                    } else {
                                        // Last pane — exit the app.
                                        exit = true;
                                    }
                                }
                            }
                        }
                    }
                }
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {
                // Tick: poll host size for resize, then refresh the clock.
                if let Ok((ncols, nrows)) = host.size() {
                    if (ncols, nrows) != (cols, rows) {
                        cols = ncols;
                        rows = nrows;
                        area = Rect { x: 0, y: 0, w: cols, h: rows.saturating_sub(1) };
                        renderer.resize(cols, rows);
                        last_rects.clear();
                        apply_layout(&layout, area, &mut panes, &mut last_rects);
                        dirty = true;
                    }
                }
                let now = local_clock();
                if now != clock {
                    clock = now;
                    dirty = true;
                }
            }
            Err(RecvTimeoutError::Disconnected) => exit = true,
        }

        if dirty && !exit {
            render(
                &mut host, &mut renderer, &layout, &panes, area, (cols, rows), &clock,
                confirm_pane,
            )?;
        }
    }

    Ok(())
    // `host` drops here → terminal restored.
}
