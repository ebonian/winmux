use crate::geom::Rect;
use crate::grid::{Cell, Color, Grid, Style};
use crate::layout::PaneId;

/// Copy-mode rendering data for one pane (Task 2, sub-project 4): the pane's
/// content is read via `Grid::view_cell(scroll, ..)` instead of the live
/// `cell` when this is `Some`, a `[scroll/history_len]` position indicator is
/// painted right-aligned on the pane's top row in `Scene::mode_style`, and
/// `cursor` (view coordinates) replaces the pane's live cursor for terminal
/// cursor placement.
pub struct CopyView {
    pub scroll: u32,
    pub cursor: (u16, u16),
    /// Selection highlight (Task 3, sub-project 4), precomputed by the
    /// SERVER in VIEW coordinates (`start_col, start_row, end_col, end_row,
    /// rect`) and already clamped into the visible pane rect -- `None` when
    /// there is no active selection, or the selection is wholly scrolled out
    /// of the current view. Linear (`rect == false`): `start_row`'s
    /// highlighted run is `start_col..`, `end_row`'s is `..=end_col`, every
    /// row strictly between is fully highlighted (standard "line-wrap"
    /// selection painting) -- a caller that clamped an off-screen endpoint
    /// widens `start_col`/`end_col` to 0/`cols-1` so this rule still paints
    /// correctly at the clamped edge. Rectangle (`rect == true`): every row
    /// in `start_row..=end_row` highlights exactly `start_col..=end_col`.
    pub sel: Option<(u16, u16, u16, u16, bool)>,
}

pub struct PaneView<'a> {
    pub id: PaneId,
    pub rect: Rect,
    pub grid: &'a Grid,
    pub focused: bool,
    pub dead: bool,
    /// `Some` when this pane is the one bound to some client's
    /// `ClientMode::Copy` (see module docs above); `None` for ordinary live
    /// rendering.
    pub copy: Option<CopyView>,
}

/// The status row's full drawing recipe (SP3 Task 8, superseding the old
/// `StatusSpan { text, underline }`): where the row sits, its base fill
/// style, left-aligned spans (each carrying a FULLY RESOLVED [`Style`] —
/// styling decisions live with the builder, `status::status_spans` + the
/// server's option table, not here), and the right-aligned text.
pub struct StatusRow {
    /// `true` = draw on row 0 (`status-position top`); `false` = the bottom
    /// row. Pane rects are computed by the SERVER to leave this row free —
    /// the renderer just paints where told.
    pub top: bool,
    /// Row fill style (`status-style` applied to the default style); padding
    /// cells between the spans and `right` are drawn with it.
    pub base: Style,
    /// Left-aligned runs with their resolved styles (window tabs get
    /// `window-status(-current)-style` layered over `base` upstream).
    pub spans: Vec<(String, Style)>,
    pub right: String,
    /// Style for `right` (`base` in SP3 — `#[]` inline styles are SP4).
    pub right_style: Style,
}

/// One row of a [`Overlay::List`] tree panel (choose-tree, Task 8; extended
/// SP6 wave 2 Task 8 for real tree structure). Tree furniture (per-depth
/// indentation and the `+`/`-` expand marker) is applied by the RENDERER
/// from `depth`/`marker` rather than baked into `text` by the caller, so
/// indentation is a pure, directly-testable render concern shared by every
/// row regardless of which view (`-s`/`-w`) built it. One indent level is
/// two spaces; the marker slot is always two columns wide (`"{marker} "` when
/// `Some`, two blank spaces when `None`) so sibling rows with and without a
/// marker still align. Built fresh by the SERVER on every render from live
/// registry state (never a stale snapshot -- see the `## overlays` contract
/// section).
pub struct TreeRowCell {
    pub text: String,
    /// Tree depth: `0` = root (session) row, `1` = child (window) row.
    pub depth: u8,
    /// `Some('+')` collapsed-with-children, `Some('-')` expanded-with-
    /// children, `None` for a leaf row with no expand affordance.
    pub marker: Option<char>,
    /// Painted in `Scene::mode_style` (reversed against the panel's default-
    /// style rows) when this is the current selection.
    pub selected: bool,
}

/// The live preview box painted below choose-tree's row list (SP6 wave 2
/// Task 8; `docs/tmux-reference/choose-tree.md` `## 3.2`/`## 6`): a single-
/// line horizontal border across `rect`'s top row (in `Scene::border`'s
/// style) with `title` embedded starting at column 1, then `content`
/// (already-composed filmstrip cells — dividers, per-slot labels, and each
/// slot's raw pane-cell copy — all pre-blitted by the SERVER, which is the
/// one place that holds every pane's `Grid`) blitted verbatim into the
/// interior (the rows below the border line, full width). `content` is
/// `content_w * content_h` cells, row-major; it may be LARGER than the
/// interior (the renderer truncates from the top-left corner, never scales)
/// or smaller (the renderer leaves the remainder as whatever the panel's
/// full-clear already put there — blank).
pub struct PreviewBlock {
    /// The full preview region: row `rect.y` is the border line, rows
    /// `rect.y + 1 .. rect.y + rect.h` are the interior. Spans the panel's
    /// full width.
    pub rect: Rect,
    pub title: String,
    pub content_w: u16,
    pub content_h: u16,
    pub content: Vec<Cell>,
}

/// A choose-tree panel (Task 8; extended SP6 wave 2 Task 8 for the tree rows
/// and preview box above). Built fresh by the SERVER on every render from
/// live registry state (never a stale snapshot -- see the `## overlays`
/// contract section).
pub struct ListOverlay {
    /// Optional header line, painted on the panel's first row in the
    /// default style (empty = no header row; the first `rows` entry starts
    /// at row 0 instead).
    pub title: String,
    pub rows: Vec<TreeRowCell>,
    /// Index into `rows` of the first row painted at the panel's top visible
    /// line (below the title, if any) -- how the panel scrolls when `rows`
    /// is longer than the available height.
    pub top: usize,
    /// `Some` when the `v`-cycled preview mode is BIG or NORMAL (never OFF)
    /// AND the panel is tall/wide enough per the sizing rule (`## 3.1`) --
    /// `None` means the row list gets the WHOLE panel height, exactly the
    /// pre-Task-8-wave-2 behavior.
    pub preview: Option<PreviewBlock>,
}

/// Everything [`Scene::overlay`] can paint OVER the already-composed frame
/// (Task 8, sub-project 4 — design spec `## 7. Overlays`): `List`
/// (choose-tree) clears the whole client area and paints a full-screen
/// panel; `PaneDigits` (display-panes) paints a 5x5 block-digit bitmap (or a
/// single-glyph fallback for an undersized pane) centered in each listed
/// pane's rect, without touching anything else on screen. The `(Rect, u32,
/// bool)` tuple is `(pane rect, digit 0-9, is the acting client's focused
/// pane)` — colour is resolved once via `Scene::display_panes_colour` /
/// `display_panes_active_colour`, not carried per-entry.
pub enum Overlay {
    List(ListOverlay),
    PaneDigits(Vec<(Rect, u32, bool)>),
}

pub struct Scene<'a> {
    pub size: (u16, u16),
    pub panes: Vec<PaneView<'a>>,
    pub zoomed: bool,
    /// `None` = `status off`: no status row is painted; panes may occupy
    /// every row.
    pub status: Option<StatusRow>,
    /// When `Some`, replaces the status row's content (same row, message
    /// style). With `status off`, the message overlays the BOTTOM row (tmux
    /// draws messages on the last line even without a status bar).
    pub message: Option<(String, Style)>,
    /// Border cell style (`pane-border-style` applied to the default style).
    pub border: Style,
    /// Border cells adjacent to the focused pane (`pane-active-border-style`,
    /// tmux default `fg=green`).
    pub border_active: Style,
    /// Copy mode's position-indicator (and, from Task 3, selection
    /// highlight) style (`mode-style` applied to the default style, tmux
    /// default `bg=yellow,fg=black`).
    pub mode_style: Style,
    /// display-panes (Task 8) digit-block colour for every pane EXCEPT the
    /// acting client's focused one (`display-panes-colour` applied to the
    /// default style, tmux default blue).
    pub display_panes_colour: Style,
    /// display-panes (Task 8) digit-block colour for the acting client's
    /// FOCUSED pane (`display-panes-active-colour`, tmux default red).
    pub display_panes_active_colour: Style,
    /// choose-tree / display-panes (Task 8): painted last, over everything
    /// else composed above. `None` = no overlay active.
    pub overlay: Option<Overlay>,
}

pub struct Renderer {
    cols: u16,
    rows: u16,
    front: Vec<Cell>,
    back: Vec<Cell>,
    force_full: bool,
}

impl Renderer {
    pub fn new(cols: u16, rows: u16) -> Self {
        let n = cols as usize * rows as usize;
        Renderer {
            cols,
            rows,
            front: vec![Cell::default(); n],
            back: vec![Cell::default(); n],
            force_full: false,
        }
    }

    fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if x >= self.cols || y >= self.rows {
            return;
        }
        let idx = y as usize * self.cols as usize + x as usize;
        self.back[idx] = cell;
    }

    /// Fill the back buffer from the scene: pane grids, junction-aware borders
    /// (unless zoomed), dead-pane overlays, then the status row (or message).
    fn compose_back(&mut self, scene: &Scene) {
        let cols = self.cols;
        let rows = self.rows;
        // Which row (if any) the status bar occupies. `status: None` frees
        // every row for panes.
        let status_y: Option<u16> = match &scene.status {
            Some(s) if rows > 0 => Some(if s.top { 0 } else { rows - 1 }),
            _ => None,
        };
        // A row panes/borders may draw on (i.e. not the status row).
        let in_band = |y: u16| -> bool { y < rows && Some(y) != status_y };

        // 0) clear back to default cells
        for c in self.back.iter_mut() {
            *c = Cell::default();
        }

        // 1) copy each pane's grid into its rect -- copy-mode panes read a
        // scrolled view (`view_cell`) instead of the live screen (`cell`).
        for pv in &scene.panes {
            let r = pv.rect;
            for dy in 0..r.h {
                let y = r.y + dy;
                if !in_band(y) {
                    continue;
                }
                for dx in 0..r.w {
                    let x = r.x + dx;
                    if x >= cols {
                        continue;
                    }
                    if dx < pv.grid.cols() && dy < pv.grid.rows() {
                        let cell = match &pv.copy {
                            Some(cv) => pv.grid.view_cell(cv.scroll, dx, dy),
                            None => pv.grid.cell(dx, dy),
                        };
                        self.set(x, y, cell);
                    }
                }
            }
        }

        // 1a) copy-mode selection highlight (Task 3, sub-project 4):
        // `mode_style`'s fg/bg painted ON TOP of whatever pass 1 already put
        // there (character and every OTHER style attribute -- bold,
        // underline, etc. -- preserved), for every cell inside the
        // precomputed (already view-clamped) selection rect.
        for pv in &scene.panes {
            let Some(cv) = &pv.copy else { continue };
            let Some((sc, sr, ec, er, rect)) = cv.sel else { continue };
            let r = pv.rect;
            for dy in sr..=er.min(r.h.saturating_sub(1)) {
                let y = r.y + dy;
                if !in_band(y) || dy >= pv.grid.rows() {
                    continue;
                }
                let (row_lo, row_hi) = if rect || (dy == sr && dy == er) {
                    (sc, ec)
                } else if dy == sr {
                    (sc, r.w.saturating_sub(1))
                } else if dy == er {
                    (0, ec)
                } else {
                    (0, r.w.saturating_sub(1))
                };
                for dx in row_lo..=row_hi.min(r.w.saturating_sub(1)) {
                    let x = r.x + dx;
                    if x >= cols || dx >= pv.grid.cols() {
                        continue;
                    }
                    let idx = y as usize * cols as usize + x as usize;
                    let existing = self.back[idx];
                    let style = Style { fg: scene.mode_style.fg, bg: scene.mode_style.bg, ..existing.style };
                    self.set(x, y, Cell { ch: existing.ch, style });
                }
            }
        }

        // 1b) copy-mode position indicator: `[scroll/history_len]`,
        // right-aligned on the pane's TOP row, in `mode_style`.
        for pv in &scene.panes {
            let Some(cv) = &pv.copy else { continue };
            let r = pv.rect;
            if !in_band(r.y) {
                continue;
            }
            let history_len = pv.grid.history_len();
            let indicator = format!("[{}/{}]", cv.scroll, history_len);
            let chars: Vec<char> = indicator.chars().collect();
            let ind_len = (chars.len() as u16).min(r.w);
            let start_x = r.x + r.w.saturating_sub(ind_len);
            let skip = chars.len() - ind_len as usize; // truncate from the left if wider than the pane
            for (i, ch) in chars[skip..].iter().enumerate() {
                let x = start_x + i as u16;
                if x < cols {
                    self.set(x, r.y, Cell { ch: *ch, style: scene.mode_style });
                }
            }
        }

        // 2) borders — only when not zoomed
        if !scene.zoomed {
            let w = cols as usize;
            let h = rows as usize;
            let mut covered = vec![false; w * h];
            for pv in &scene.panes {
                let r = pv.rect;
                for dy in 0..r.h {
                    let y = r.y + dy;
                    if !in_band(y) {
                        continue;
                    }
                    for dx in 0..r.w {
                        let x = r.x + dx;
                        if x >= cols {
                            continue;
                        }
                        covered[y as usize * w + x as usize] = true;
                    }
                }
            }
            let is_border = |x: i32, y: i32| -> bool {
                if x < 0 || y < 0 || x >= cols as i32 || y >= rows as i32 {
                    return false;
                }
                if !in_band(y as u16) {
                    return false;
                }
                !covered[y as usize * w + x as usize]
            };
            let focused_rect = scene.panes.iter().find(|p| p.focused).map(|p| p.rect);
            let touches_focused = |x: i32, y: i32| -> bool {
                match focused_rect {
                    Some(fr) => {
                        let inside = |xx: i32, yy: i32| {
                            xx >= fr.x as i32
                                && xx < (fr.x + fr.w) as i32
                                && yy >= fr.y as i32
                                && yy < (fr.y + fr.h) as i32
                        };
                        inside(x - 1, y) || inside(x + 1, y) || inside(x, y - 1) || inside(x, y + 1)
                    }
                    None => false,
                }
            };
            for y in 0..rows as i32 {
                if !in_band(y as u16) {
                    continue;
                }
                for x in 0..cols as i32 {
                    if !is_border(x, y) {
                        continue;
                    }
                    let ch = border_glyph(
                        is_border(x, y - 1),
                        is_border(x, y + 1),
                        is_border(x - 1, y),
                        is_border(x + 1, y),
                    );
                    let style = if touches_focused(x, y) { scene.border_active } else { scene.border };
                    self.set(x as u16, y as u16, Cell { ch, style });
                }
            }
        }

        // 3) dead-pane overlay: "[exited]" in reverse video at rect top-left
        for pv in &scene.panes {
            if !pv.dead {
                continue;
            }
            let y = pv.rect.y;
            if !in_band(y) {
                continue;
            }
            let style = Style { reverse: true, ..Style::default() };
            let x_end = pv.rect.x.saturating_add(pv.rect.w).min(cols);
            for (x, ch) in (pv.rect.x..x_end).zip("[exited]".chars()) {
                self.set(x, y, Cell { ch, style });
            }
        }

        // 4) status row / message overlay
        if rows == 0 {
            return;
        }
        let cols_u = cols as usize;
        if let Some((msg, style)) = &scene.message {
            // A message replaces the status row's content; with status off it
            // overlays the bottom row (tmux behavior: messages use the last
            // line even with no status bar).
            let y = status_y.unwrap_or(rows - 1);
            for x in 0..cols {
                self.set(x, y, Cell { ch: ' ', style: *style });
            }
            for (i, ch) in msg.chars().enumerate() {
                if i >= cols_u {
                    break;
                }
                self.set(i as u16, y, Cell { ch, style: *style });
            }
        } else if let Some(st) = &scene.status {
            let y = status_y.expect("status_y is Some whenever scene.status is (rows > 0)");
            // fill the row with base-styled spaces
            for x in 0..cols {
                self.set(x, y, Cell { ch: ' ', style: st.base });
            }
            let left_len_total: usize = st.spans.iter().map(|(t, _)| t.chars().count()).sum();
            let right: Vec<char> = st.right.chars().collect();

            let mut x = 0usize;
            'spans: for (text, style) in &st.spans {
                for ch in text.chars() {
                    if x >= cols_u {
                        break 'spans;
                    }
                    self.set(x as u16, y, Cell { ch, style: *style });
                    x += 1;
                }
            }
            let left_len = left_len_total.min(cols_u);
            let max_right = cols_u - left_len;
            let right_len = right.len().min(max_right); // truncate right first
            let start = cols_u - right_len;
            for (i, &ch) in right[..right_len].iter().enumerate() {
                self.set((start + i) as u16, y, Cell { ch, style: st.right_style });
            }
        }

        // 5) overlay (Task 8, sub-project 4): painted LAST, over everything
        // above. `List` (choose-tree) clears/replaces the whole client area
        // (including the status row just painted); `PaneDigits`
        // (display-panes) only touches cells inside the listed pane rects.
        match &scene.overlay {
            None => {}
            Some(Overlay::List(list)) => {
                for c in self.back.iter_mut() {
                    *c = Cell { ch: ' ', style: Style::default() };
                }
                let mut y: u16 = 0;
                if !list.title.is_empty() && y < rows {
                    for (i, ch) in list.title.chars().enumerate() {
                        if i as u16 >= cols {
                            break;
                        }
                        self.set(i as u16, y, Cell { ch, style: Style::default() });
                    }
                    y += 1;
                }
                // With a preview box present, the row list is already capped
                // to `preview.rect.y` by the sizing rule (`## 3.1`) -- the
                // message reservation below (pre-Task-8-wave-2 behavior) only
                // applies when there's no preview eating into the panel, same
                // as before this amendment.
                let list_cap: u16 = match &list.preview {
                    Some(pv) => pv.rect.y,
                    None => rows,
                };
                // A message (e.g. choose-tree's `x` kill-confirm prompt, see
                // `ClientMode::ChooseTree`'s `pending_kill`) takes the panel's
                // LAST row, same as it takes the status row outside the
                // overlay -- reserved BEFORE laying out the row list so the
                // two never collide, and painted AFTER the rows so it always
                // wins visually. With a preview shown, the message is instead
                // painted over the panel's bottom row unconditionally below
                // (rare/transient; simpler than re-deriving a preview-aware
                // reservation).
                let msg_reserved: u16 = if scene.message.is_some() && list.preview.is_none() && list_cap > y { 1 } else { 0 };
                let visible = list_cap.saturating_sub(y).saturating_sub(msg_reserved) as usize;
                let start = list.top.min(list.rows.len());
                let end = (start + visible).min(list.rows.len());
                for (i, row) in list.rows[start..end].iter().enumerate() {
                    let yy = y + i as u16;
                    let style = if row.selected { scene.mode_style } else { Style::default() };
                    for x in 0..cols {
                        self.set(x, yy, Cell { ch: ' ', style });
                    }
                    let indent = "  ".repeat(row.depth as usize);
                    let marker_slot = match row.marker {
                        Some(c) => format!("{c} "),
                        None => "  ".to_string(),
                    };
                    let line = format!("{indent}{marker_slot}{}", row.text);
                    for (cx, ch) in line.chars().enumerate() {
                        if cx as u16 >= cols {
                            break;
                        }
                        self.set(cx as u16, yy, Cell { ch, style });
                    }
                }

                // Preview box (SP6 wave 2 Task 8): single top-border line
                // with the title embedded, then a raw, truncate-never-scale
                // blit of the pre-composed filmstrip `content` below it.
                if let Some(pv) = &list.preview {
                    if pv.rect.w > 0 && pv.rect.h > 0 && pv.rect.y < rows {
                        let by = pv.rect.y;
                        let x_end = pv.rect.x.saturating_add(pv.rect.w).min(cols);
                        for x in pv.rect.x..x_end {
                            self.set(x, by, Cell { ch: '─', style: scene.border });
                        }
                        for (i, ch) in pv.title.chars().enumerate() {
                            let x = pv.rect.x + 1 + i as u16;
                            if x >= x_end {
                                break;
                            }
                            self.set(x, by, Cell { ch, style: scene.border });
                        }
                        let interior_y = by + 1;
                        let interior_h = pv.rect.h.saturating_sub(1);
                        let interior_w = pv.rect.w;
                        let copy_h = pv.content_h.min(interior_h);
                        let copy_w = pv.content_w.min(interior_w);
                        for cy in 0..copy_h {
                            let yy = interior_y + cy;
                            if yy >= rows {
                                break;
                            }
                            for cx in 0..copy_w {
                                let x = pv.rect.x + cx;
                                if x >= cols {
                                    break;
                                }
                                let idx = cy as usize * pv.content_w as usize + cx as usize;
                                if let Some(cell) = pv.content.get(idx) {
                                    self.set(x, yy, *cell);
                                }
                            }
                        }
                    }
                }

                if let Some((msg, style)) = &scene.message {
                    let msg_y = rows - 1;
                    for x in 0..cols {
                        self.set(x, msg_y, Cell { ch: ' ', style: *style });
                    }
                    for (i, ch) in msg.chars().enumerate() {
                        if i as u16 >= cols {
                            break;
                        }
                        self.set(i as u16, msg_y, Cell { ch, style: *style });
                    }
                }
            }
            Some(Overlay::PaneDigits(entries)) => {
                for (rect, digit, active) in entries {
                    let style = if *active { scene.display_panes_active_colour } else { scene.display_panes_colour };
                    self.paint_pane_digit(*rect, *digit, style, cols, rows);
                }
            }
        }
    }

    /// Paint one display-panes (Task 8) digit into `rect`: a 5x5 block
    /// bitmap (see [`digit_bitmap`]) centered in the rect when it's at least
    /// 6 cells wide (5 + a 1-cell margin) and 5 tall; otherwise a
    /// single-glyph "small-number fallback" (the design spec's own term) at
    /// the rect's center; a zero-size rect paints nothing.
    fn paint_pane_digit(&mut self, rect: Rect, digit: u32, style: Style, cols: u16, rows: u16) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        if rect.w >= 6 && rect.h >= 5 {
            let ox = rect.x + (rect.w - 5) / 2;
            let oy = rect.y + (rect.h - 5) / 2;
            for (dy, row) in digit_bitmap(digit).iter().enumerate() {
                for (dx, ch) in row.chars().enumerate() {
                    if ch != '#' {
                        continue;
                    }
                    let x = ox + dx as u16;
                    let y = oy + dy as u16;
                    if x < cols && y < rows {
                        self.set(x, y, Cell { ch: ' ', style });
                    }
                }
            }
        } else {
            let ch = char::from_digit(digit, 10).unwrap_or('?');
            let x = rect.x + rect.w / 2;
            let y = rect.y + rect.h / 2;
            if x < cols && y < rows {
                self.set(x, y, Cell { ch, style });
            }
        }
    }

    /// Reallocate both buffers to the new size and invalidate the front buffer
    /// so the next compose() emits a full repaint preceded by CSI 2J.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        let n = cols as usize * rows as usize;
        self.front = vec![Cell::default(); n];
        self.back = vec![Cell::default(); n];
        self.force_full = true;
    }

    /// Compose the scene into the back buffer, diff it against the front buffer,
    /// emit the minimal VT byte stream, swap buffers, and return the bytes.
    pub fn compose(
        &mut self,
        scene: &Scene,
        cursor: Option<(u16, u16)>,
        cursor_visible: bool,
    ) -> Vec<u8> {
        self.compose_back(scene);

        let mut out: Vec<u8> = Vec::new();
        if self.force_full {
            out.extend_from_slice(b"\x1b[2J");
        }

        let cols = self.cols as usize;
        let mut last_pos: Option<(u16, u16)> = None; // real cursor after last emit
        let mut cur_style: Option<Style> = None; // last SGR emitted

        for y in 0..self.rows {
            for x in 0..self.cols {
                let idx = y as usize * cols + x as usize;
                let b = self.back[idx];
                let f = self.front[idx];
                if !self.force_full && b == f {
                    continue;
                }
                // CUP only when not already positioned at (x, y)
                let need_move = !matches!(last_pos, Some((lx, ly)) if lx == x && ly == y);
                if need_move {
                    out.extend_from_slice(cup(x, y).as_bytes());
                }
                // SGR only on style change
                if cur_style != Some(b.style) {
                    out.extend_from_slice(sgr(&b.style).as_bytes());
                    cur_style = Some(b.style);
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(b.ch.encode_utf8(&mut buf).as_bytes());
                last_pos = Some((x + 1, y)); // advanced one column right
            }
        }

        // trailing reset if any style was emitted
        if cur_style.is_some() {
            out.extend_from_slice(b"\x1b[0m");
        }

        // cursor placement
        match cursor {
            Some((cx, cy)) if cursor_visible => {
                out.extend_from_slice(cup(cx, cy).as_bytes());
                out.extend_from_slice(b"\x1b[?25h");
            }
            _ => {
                out.extend_from_slice(b"\x1b[?25l");
            }
        }

        std::mem::swap(&mut self.front, &mut self.back);
        self.force_full = false;
        out
    }

    #[cfg(test)]
    fn back_cell(&self, x: u16, y: u16) -> Cell {
        self.back[y as usize * self.cols as usize + x as usize]
    }
}

/// Map the four orthogonal border-neighbor flags to a box-drawing glyph.
/// Degenerate cases (0 or 1 connection) resolve to a straight line so a border
/// that runs to the window edge renders as `│`/`─` rather than a stub.
fn border_glyph(up: bool, down: bool, left: bool, right: bool) -> char {
    match (up, down, left, right) {
        (true, true, true, true) => '┼',
        (true, true, true, false) => '┤',
        (true, true, false, true) => '├',
        (true, true, false, false) => '│',
        (true, false, true, true) => '┴',
        (false, true, true, true) => '┬',
        (true, false, true, false) => '┘',
        (true, false, false, true) => '└',
        (false, true, true, false) => '┐',
        (false, true, false, true) => '┌',
        (false, false, true, true) => '─',
        (true, false, false, false) => '│',
        (false, true, false, false) => '│',
        (false, false, true, false) => '─',
        (false, false, false, true) => '─',
        (false, false, false, false) => '─',
    }
}

/// display-panes (Task 8, sub-project 4): a 5-row x 5-column block-digit
/// bitmap for `0..=9` (`'#'` = painted cell, `'.'` = untouched). Not a real
/// tmux artifact -- tmux's own display-panes digit rendering isn't a
/// documented byte-for-byte spec, so this is winmux's own simple, legible
/// 5x5 font (the design spec only pins the CELL SIZE, not the exact glyph
/// shapes). `digit` outside `0..=9` (unreachable via the digit-key/pane-cap
/// path, which only ever mints 0-9) falls back to all-blank rather than
/// panicking.
fn digit_bitmap(digit: u32) -> [&'static str; 5] {
    match digit {
        0 => ["#####", "#...#", "#...#", "#...#", "#####"],
        1 => ["..#..", ".##..", "..#..", "..#..", "#####"],
        2 => ["#####", "....#", "#####", "#....", "#####"],
        3 => ["#####", "....#", "..###", "....#", "#####"],
        4 => ["#...#", "#...#", "#####", "....#", "....#"],
        5 => ["#####", "#....", "#####", "....#", "#####"],
        6 => ["#####", "#....", "#####", "#...#", "#####"],
        7 => ["#####", "....#", "...#.", "..#..", "..#.."],
        8 => ["#####", "#...#", "#####", "#...#", "#####"],
        9 => ["#####", "#...#", "#####", "....#", "#####"],
        _ => ["", "", "", "", ""],
    }
}

fn cup(x: u16, y: u16) -> String {
    format!("\x1b[{};{}H", y + 1, x + 1)
}

/// Build a single combined SGR sequence `\x1b[0;...m` from a Style.
fn sgr(s: &Style) -> String {
    let mut parts: Vec<String> = vec!["0".to_string()];
    if s.bold {
        parts.push("1".to_string());
    }
    if s.dim {
        parts.push("2".to_string());
    }
    if s.italic {
        parts.push("3".to_string());
    }
    if s.underline {
        parts.push("4".to_string());
    }
    if s.reverse {
        parts.push("7".to_string());
    }
    // foreground
    match s.fg {
        Color::Default => parts.push("39".to_string()),
        Color::Idx(n) if n < 8 => parts.push((30 + n as u16).to_string()),
        Color::Idx(n) if n < 16 => parts.push((90 + n as u16 - 8).to_string()),
        Color::Idx(n) => {
            parts.push("38".to_string());
            parts.push("5".to_string());
            parts.push(n.to_string());
        }
        Color::Rgb(r, g, b) => {
            parts.push("38".to_string());
            parts.push("2".to_string());
            parts.push(r.to_string());
            parts.push(g.to_string());
            parts.push(b.to_string());
        }
    }
    // background
    match s.bg {
        Color::Default => parts.push("49".to_string()),
        Color::Idx(n) if n < 8 => parts.push((40 + n as u16).to_string()),
        Color::Idx(n) if n < 16 => parts.push((100 + n as u16 - 8).to_string()),
        Color::Idx(n) => {
            parts.push("48".to_string());
            parts.push("5".to_string());
            parts.push(n.to_string());
        }
        Color::Rgb(r, g, b) => {
            parts.push("48".to_string());
            parts.push("2".to_string());
            parts.push(r.to_string());
            parts.push(g.to_string());
            parts.push(b.to_string());
        }
    }
    format!("\x1b[{}m", parts.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::grid::{Color, Grid};

    fn grid_with(cols: u16, rows: u16, bytes: &[u8]) -> Grid {
        let mut g = Grid::new(cols, rows, 0);
        g.feed(bytes);
        g
    }

    /// tmux default `status-style` resolved: `bg=green,fg=black`.
    fn status_base() -> Style {
        Style { fg: Color::Idx(0), bg: Color::Idx(2), ..Style::default() }
    }

    /// A bottom `StatusRow` with the default base style (what the server
    /// builds from default options) — keeps the pre-Task-8 tests' expected
    /// bytes unchanged.
    fn default_status(spans: Vec<(String, Style)>, right: &str) -> StatusRow {
        StatusRow { top: false, base: status_base(), spans, right: right.to_string(), right_style: status_base() }
    }

    /// tmux default `pane-active-border-style` resolved: `fg=green`.
    fn green_active() -> Style {
        Style { fg: Color::Idx(2), ..Style::default() }
    }

    /// tmux default `message-style` resolved: `bg=yellow,fg=black`.
    fn default_msg_style() -> Style {
        Style { fg: Color::Idx(0), bg: Color::Idx(3), ..Style::default() }
    }

    // 7x4 terminal: two panes side-by-side, vertical border column at x=3,
    // status row at y=3. Right pane is focused (its border is green).
    #[test]
    fn two_panes_content_and_focused_border() {
        let left = grid_with(3, 3, b"L");
        let right = grid_with(3, 3, b"R");
        let scene = Scene {
            size: (7, 4),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 3 }, grid: &left, focused: false, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 3 }, grid: &right, focused: true, dead: false, copy: None },
            ],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(7, 4);
        r.compose_back(&scene);

        // pane content copied into rects
        assert_eq!(r.back_cell(0, 0).ch, 'L');
        assert_eq!(r.back_cell(4, 0).ch, 'R');
        // vertical border column, all three rows
        assert_eq!(r.back_cell(3, 0).ch, '│');
        assert_eq!(r.back_cell(3, 1).ch, '│');
        assert_eq!(r.back_cell(3, 2).ch, '│');
        // border adjoins the focused (right) pane -> green fg = Idx(2)
        assert_eq!(r.back_cell(3, 0).style.fg, Color::Idx(2));
    }

    // 7x5 terminal: full-height left pane, right side split into top(1 row)
    // and bottom(2 rows). Produces a ├ junction where the horizontal border
    // meets the vertical border.
    #[test]
    fn border_tee_junction() {
        let left = grid_with(3, 4, b"");
        let rt = grid_with(3, 1, b"");
        let rb = grid_with(3, 2, b"");
        let scene = Scene {
            size: (7, 5),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 4 }, grid: &left, focused: false, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 1 }, grid: &rt, focused: false, dead: false, copy: None },
                PaneView { id: 3, rect: Rect { x: 4, y: 2, w: 3, h: 2 }, grid: &rb, focused: true, dead: false, copy: None },
            ],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(7, 5);
        r.compose_back(&scene);

        assert_eq!(r.back_cell(3, 0).ch, '│'); // vertical line top
        assert_eq!(r.back_cell(3, 1).ch, '├'); // junction (up,down,right)
        assert_eq!(r.back_cell(4, 1).ch, '─'); // horizontal arm
        assert_eq!(r.back_cell(6, 1).ch, '─'); // horizontal arm to edge
        // horizontal arm cell touches focused bottom-right pane -> green
        assert_eq!(r.back_cell(4, 1).style.fg, Color::Idx(2));
    }

    #[test]
    fn status_bar_layout() {
        let g = grid_with(10, 1, b"");
        let scene = Scene {
            size: (10, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 10, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(vec![("AB".to_string(), status_base())], "Z")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(10, 2);
        r.compose_back(&scene);

        assert_eq!(r.back_cell(0, 1).ch, 'A');
        assert_eq!(r.back_cell(1, 1).ch, 'B');
        assert_eq!(r.back_cell(9, 1).ch, 'Z'); // right-aligned
        assert_eq!(r.back_cell(5, 1).ch, ' '); // padded middle
        // bottom row is bg green (Idx 2) fg black (Idx 0)
        assert_eq!(r.back_cell(0, 1).style.bg, Color::Idx(2));
        assert_eq!(r.back_cell(0, 1).style.fg, Color::Idx(0));
        assert_eq!(r.back_cell(5, 1).style.bg, Color::Idx(2));
    }

    // Right part is truncated first when left+right do not fit.
    #[test]
    fn status_truncates_right_first() {
        let g = grid_with(6, 1, b"");
        let scene = Scene {
            size: (6, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 6, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(vec![("ab".to_string(), status_base())], "123456")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(6, 2);
        r.compose_back(&scene);
        // left kept intact, right cut to remaining 4 cells -> "ab1234"
        let row: String = (0..6).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row, "ab1234");
    }

    #[test]
    fn message_override() {
        let g = grid_with(5, 1, b"");
        let scene = Scene {
            size: (5, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 5, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(vec![("ignored".to_string(), status_base())], "ignored")),
            message: Some(("hey".to_string(), default_msg_style())),
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(5, 2);
        r.compose_back(&scene);
        assert_eq!(r.back_cell(0, 1).ch, 'h');
        assert_eq!(r.back_cell(2, 1).ch, 'y');
        assert_eq!(r.back_cell(4, 1).ch, ' ');
        // message-style: bg yellow (Idx 3) fg black (Idx 0)
        assert_eq!(r.back_cell(0, 1).style.bg, Color::Idx(3));
        assert_eq!(r.back_cell(0, 1).style.fg, Color::Idx(0));
        assert_eq!(r.back_cell(4, 1).style.bg, Color::Idx(3));
    }

    #[test]
    fn dead_pane_overlay() {
        let g = grid_with(10, 1, b"");
        let scene = Scene {
            size: (10, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 10, h: 1 }, grid: &g, focused: true, dead: true, copy: None }],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(10, 2);
        r.compose_back(&scene);
        // "[exited]" (8 chars) in reverse video at top-left
        let label: String = (0..8).map(|x| r.back_cell(x, 0).ch).collect();
        assert_eq!(label, "[exited]");
        assert!(r.back_cell(0, 0).style.reverse);
        assert!(r.back_cell(7, 0).style.reverse);
        assert_eq!(r.back_cell(8, 0).ch, ' '); // not overlaid
    }

    // Zoomed: the border pass is skipped even if a gap exists between rects.
    #[test]
    fn zoom_suppresses_borders() {
        let left = grid_with(3, 1, b"");
        let right = grid_with(3, 1, b"");
        let scene = Scene {
            size: (7, 2),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 1 }, grid: &left, focused: false, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 1 }, grid: &right, focused: true, dead: false, copy: None },
            ],
            zoomed: true,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(7, 2);
        r.compose_back(&scene);
        assert_eq!(r.back_cell(3, 0).ch, ' '); // gap left blank, no box char
    }

    // Helper: a fresh 4x2 renderer with one full-width pane over the top row
    // and an empty (green) status bar on the bottom row. Returns nothing;
    // callers build the Scene inline so the borrowed grid can be mutated
    // between composes.

    #[test]
    fn single_cell_change_emits_only_that_cell() {
        let mut g = grid_with(4, 1, b"ab");
        let mut r = Renderer::new(4, 2);

        // prime: front <- back (discard the output)
        {
            let scene = Scene {
                size: (4, 2),
                panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
                zoomed: false,
                status: Some(default_status(Vec::new(), "")),
                message: None,
                border: Style::default(),
                border_active: green_active(),
                mode_style: Style::default(),
                display_panes_colour: Style::default(),
                display_panes_active_colour: Style::default(),
                overlay: None,
            };
            let _ = r.compose(&scene, Some((0, 0)), true);
        }

        // change exactly cell (0,0): 'a' -> 'X'
        g.feed(b"\x1b[1;1HX");

        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let out = r.compose(&scene, Some((1, 0)), true);
        let got = String::from_utf8_lossy(&out);
        let want = "\x1b[1;1H\x1b[0;39;49mX\x1b[0m\x1b[1;2H\x1b[?25h";
        assert_eq!(got, want);
    }

    #[test]
    fn adjacent_changes_coalesce_one_cup_one_sgr() {
        let mut g = grid_with(4, 1, b"ab");
        let mut r = Renderer::new(4, 2);
        {
            let scene = Scene {
                size: (4, 2),
                panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
                zoomed: false,
                status: Some(default_status(Vec::new(), "")),
                message: None,
                border: Style::default(),
                border_active: green_active(),
                mode_style: Style::default(),
                display_panes_colour: Style::default(),
                display_panes_active_colour: Style::default(),
                overlay: None,
            };
            let _ = r.compose(&scene, Some((0, 0)), true);
        }

        // change (0,0) and (1,0): "ab" -> "XY"
        g.feed(b"\x1b[1;1HXY");

        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let out = r.compose(&scene, Some((2, 0)), true);
        let got = String::from_utf8_lossy(&out);
        // one CUP, one SGR, "XY" as a single run
        let want = "\x1b[1;1H\x1b[0;39;49mXY\x1b[0m\x1b[1;3H\x1b[?25h";
        assert_eq!(got, want);
    }

    #[test]
    fn hidden_cursor_when_not_visible_and_no_change() {
        let g = grid_with(4, 1, b"ab");
        let mut r = Renderer::new(4, 2);
        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let _ = r.compose(&scene, Some((0, 0)), true); // prime
        // identical recompose, cursor hidden -> no diff bytes, just hide
        let out = r.compose(&scene, Some((0, 0)), false);
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[?25l");
    }

    #[test]
    fn resize_forces_full_repaint() {
        let g = grid_with(4, 1, b"ab");
        let mut r = Renderer::new(4, 2);
        {
            let scene = Scene {
                size: (4, 2),
                panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
                zoomed: false,
                status: Some(default_status(Vec::new(), "")),
                message: None,
                border: Style::default(),
                border_active: green_active(),
                mode_style: Style::default(),
                display_panes_colour: Style::default(),
                display_panes_active_colour: Style::default(),
                overlay: None,
            };
            let _ = r.compose(&scene, Some((0, 0)), true); // prime
        }

        r.resize(4, 2); // invalidates front

        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let out = r.compose(&scene, Some((0, 0)), true);
        let got = String::from_utf8_lossy(&out);
        // full repaint: 2J, then every cell in row-major order.
        // row0 "ab  " default style; row1 "    " status style (green bg).
        let want = "\x1b[2J\
\x1b[1;1H\x1b[0;39;49mab  \
\x1b[2;1H\x1b[0;30;42m    \
\x1b[0m\x1b[1;1H\x1b[?25h";
        assert_eq!(got, want);
    }

    // Status spans: a non-underlined run ("AB") followed by the current
    // window's underlined span ("C") must emit SGR 4 only for the latter.
    #[test]
    fn underlined_span_emits_sgr4() {
        let g = grid_with(10, 1, b"");
        let scene = Scene {
            size: (10, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 10, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(
                vec![
                    ("AB".to_string(), status_base()),
                    ("C".to_string(), Style { underline: true, ..status_base() }),
                ],
                "",
            )),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(10, 2);
        let out = r.compose(&scene, None, false);
        let got = String::from_utf8_lossy(&out);
        // "AB" drawn with the plain status style (no "4" in the SGR).
        assert!(got.contains("\x1b[0;30;42mAB"));
        // "C" (current window, underlined) gets SGR 4 added.
        assert!(got.contains("\x1b[0;4;30;42mC"));
    }

    // ---- SP3 Task 8: option-driven styles/position ----

    // `status-position top`: the status row paints on row 0; the pane (whose
    // rect the server shifted down to y=1) paints below it. Exact bytes.
    #[test]
    fn status_top_row_zero() {
        let g = grid_with(4, 1, b"ab");
        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 1, w: 4, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(StatusRow {
                top: true,
                base: status_base(),
                spans: vec![("AB".to_string(), status_base())],
                right: String::new(),
                right_style: status_base(),
            }),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(4, 2);
        let out = r.compose(&scene, None, false);
        let got = String::from_utf8_lossy(&out);
        // row 0: "AB  " in status style (all 4 cells differ from the default
        // front); row 1: "ab" default style (trailing spaces match the
        // default front, so they are skipped).
        let want = "\x1b[1;1H\x1b[0;30;42mAB  \x1b[2;1H\x1b[0;39;49mab\x1b[0m\x1b[?25l";
        assert_eq!(got, want);
    }

    // `status off` (Scene.status None): no status bytes at all; the pane may
    // occupy every row including the bottom one.
    #[test]
    fn status_off_no_row() {
        let g = grid_with(4, 2, b"ab\r\ncd");
        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 2 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(4, 2);
        let out = r.compose(&scene, None, false);
        let got = String::from_utf8_lossy(&out);
        // Both rows are pane content in the default style; no 30;42 status
        // SGR appears anywhere.
        let want = "\x1b[1;1H\x1b[0;39;49mab\x1b[2;1Hcd\x1b[0m\x1b[?25l";
        assert_eq!(got, want);
    }

    // A span carrying a custom resolved style emits exactly that SGR.
    #[test]
    fn span_styles_emitted() {
        let g = grid_with(6, 1, b"");
        let custom = Style { fg: Color::Idx(7), bg: Color::Idx(4), ..Style::default() }; // white on blue
        let scene = Scene {
            size: (6, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 6, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(vec![("AB".to_string(), custom)], "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(6, 2);
        let out = r.compose(&scene, None, false);
        let got = String::from_utf8_lossy(&out);
        assert!(got.contains("\x1b[0;37;44mAB"), "got: {got:?}");
    }

    // Custom border + active-border styles: cells adjacent to the focused
    // pane use `border_active`, all other border cells use `border`.
    // Reuses the tee-junction layout with the LEFT pane focused so the
    // horizontal arm between the two right panes is a non-active border.
    #[test]
    fn border_style_applied() {
        let left = grid_with(3, 4, b"");
        let rt = grid_with(3, 1, b"");
        let rb = grid_with(3, 2, b"");
        let scene = Scene {
            size: (7, 5),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 4 }, grid: &left, focused: true, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 1 }, grid: &rt, focused: false, dead: false, copy: None },
                PaneView { id: 3, rect: Rect { x: 4, y: 2, w: 3, h: 2 }, grid: &rb, focused: false, dead: false, copy: None },
            ],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style { fg: Color::Idx(240), ..Style::default() },      // pane-border-style fg=colour240
            border_active: Style { fg: Color::Idx(1), ..Style::default() }, // pane-active-border-style fg=red
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(7, 5);
        r.compose_back(&scene);
        // vertical border column touches the focused left pane -> active red
        assert_eq!(r.back_cell(3, 0).ch, '│');
        assert_eq!(r.back_cell(3, 0).style.fg, Color::Idx(1));
        // the horizontal arm between rt and rb does NOT touch the focused
        // pane -> plain border style
        assert_eq!(r.back_cell(4, 1).ch, '─');
        assert_eq!(r.back_cell(4, 1).style.fg, Color::Idx(240));
    }

    // The message's own resolved style is emitted verbatim.
    #[test]
    fn message_style_applied() {
        let g = grid_with(5, 1, b"");
        let custom = Style { fg: Color::Idx(7), bg: Color::Idx(1), bold: true, ..Style::default() };
        let scene = Scene {
            size: (5, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 5, h: 1 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: Some(("hi".to_string(), custom)),
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: None,
        };
        let mut r = Renderer::new(5, 2);
        let out = r.compose(&scene, None, false);
        let got = String::from_utf8_lossy(&out);
        assert!(got.contains("\x1b[0;1;37;41mhi"), "got: {got:?}");
    }

    // ---- overlays (Task 8, sub-project 4) ----------------------------------

    /// `Overlay::List` clears the whole client area, paints each row's text
    /// left-aligned padded to full width, and paints the SELECTED row in
    /// `mode_style` while every other row stays the plain default style.
    #[test]
    fn overlay_list_paints_rows_and_selection() {
        let g = grid_with(10, 3, b"should be hidden");
        let mode_style = Style { fg: Color::Idx(0), bg: Color::Idx(3), ..Style::default() }; // yellow/black
        let scene = Scene {
            size: (10, 3),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 10, h: 3 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style,
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: vec![
                    TreeRowCell { text: "row0".to_string(), depth: 0, marker: None, selected: false },
                    TreeRowCell { text: "row1".to_string(), depth: 0, marker: None, selected: true },
                ],
                top: 0,
                preview: None,
            })),
        };
        let mut r = Renderer::new(10, 3);
        r.compose_back(&scene);

        // Row 0 (unselected): depth 0 / no marker -> a 2-space blank marker
        // slot precedes the text (no indent at depth 0), then the text,
        // default style.
        let row0: String = (0..6).map(|x| r.back_cell(x, 0).ch).collect();
        assert_eq!(row0, "  row0");
        assert_eq!(r.back_cell(0, 0).style, Style::default());
        // Padding past the text is still cleared to the row's style (full
        // client-area clear, not just the text run).
        assert_eq!(r.back_cell(9, 0).ch, ' ');
        assert_eq!(r.back_cell(9, 0).style, Style::default());

        // Row 1 (selected): text painted in mode_style, including padding.
        let row1: String = (0..6).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row1, "  row1");
        assert_eq!(r.back_cell(0, 1).style, mode_style);
        assert_eq!(r.back_cell(9, 1).ch, ' ');
        assert_eq!(r.back_cell(9, 1).style, mode_style);

        // The overlay fully replaced the pane's own content underneath.
        assert_eq!(r.back_cell(0, 2).ch, ' ');
    }

    // ---- tree rows + preview box (SP6 wave 2, Task 8) ----------------------

    /// Tree furniture: a root (session) row's marker occupies the first two
    /// columns (`"- "`, no indent at depth 0, per `TreeRowCell`'s doc
    /// comment); a child (window) row is indented one level (2 spaces) and
    /// THEN gets its own marker slot (blank, since leaf rows carry `marker:
    /// None`), so its text starts at column 4.
    #[test]
    fn overlay_tree_rows_indent_children() {
        let g = grid_with(20, 2, b"");
        let scene = Scene {
            size: (20, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 20, h: 2 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: vec![
                    TreeRowCell { text: "main: 2 windows".to_string(), depth: 0, marker: Some('-'), selected: false },
                    TreeRowCell { text: "0: bash*".to_string(), depth: 1, marker: None, selected: false },
                ],
                top: 0,
                preview: None,
            })),
        };
        let mut r = Renderer::new(20, 2);
        r.compose_back(&scene);

        let expect0 = "- main: 2 windows";
        let row0: String = (0..expect0.chars().count() as u16).map(|x| r.back_cell(x, 0).ch).collect();
        assert_eq!(row0, expect0);

        let expect1 = "    0: bash*"; // depth 1 -> "  " indent + "  " blank marker slot
        let row1: String = (0..expect1.chars().count() as u16).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row1, expect1);
    }

    /// The preview box paints a single top-border line (with the title
    /// embedded starting at column 1, in `Scene::border`'s style) then blits
    /// `content` verbatim into the interior (the rows below the border), raw
    /// cell-for-cell (character AND style), no color/attribute changes.
    /// `rect = (0,2,6,3)`: border row 2, interior rows 3-4 (`rect.h - 1 = 2`
    /// rows). `content` is `3 x 2` -- narrower than the 6-wide interior, so
    /// only the first 3 columns of each interior row are touched; the rest
    /// stay whatever the panel's full-clear already left them (blank,
    /// default style).
    #[test]
    fn overlay_preview_blits_grid_cells() {
        let g = grid_with(6, 5, b"");
        let s1 = Style { fg: Color::Idx(2), ..Style::default() };
        let s2 = Style { fg: Color::Idx(3), ..Style::default() };
        let content = vec![
            Cell { ch: 'A', style: s1 },
            Cell { ch: 'B', style: s1 },
            Cell { ch: 'C', style: s1 },
            Cell { ch: 'D', style: s2 },
            Cell { ch: 'E', style: s2 },
            Cell { ch: 'F', style: s2 },
        ];
        let border = Style { fg: Color::Idx(8), ..Style::default() };
        let scene = Scene {
            size: (6, 5),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 6, h: 5 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border,
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: Vec::new(),
                top: 0,
                preview: Some(PreviewBlock { rect: Rect { x: 0, y: 2, w: 6, h: 3 }, title: "foo".to_string(), content_w: 3, content_h: 2, content }),
            })),
        };
        let mut r = Renderer::new(6, 5);
        r.compose_back(&scene);

        // Border row (y=2): '─' fill in `border` style; title "foo"
        // overwrites starting at column 1 (also in `border` style).
        assert_eq!(r.back_cell(0, 2).ch, '─');
        assert_eq!(r.back_cell(0, 2).style, border);
        assert_eq!(r.back_cell(1, 2).ch, 'f');
        assert_eq!(r.back_cell(2, 2).ch, 'o');
        assert_eq!(r.back_cell(3, 2).ch, 'o');
        assert_eq!(r.back_cell(1, 2).style, border);
        assert_eq!(r.back_cell(4, 2).ch, '─');

        // Interior row 0 (y=3): content[0..3] verbatim; columns 3-5 untouched.
        assert_eq!(r.back_cell(0, 3), Cell { ch: 'A', style: s1 });
        assert_eq!(r.back_cell(1, 3), Cell { ch: 'B', style: s1 });
        assert_eq!(r.back_cell(2, 3), Cell { ch: 'C', style: s1 });
        assert_eq!(r.back_cell(3, 3).ch, ' ');
        assert_eq!(r.back_cell(3, 3).style, Style::default());

        // Interior row 1 (y=4): content[3..6] verbatim.
        assert_eq!(r.back_cell(0, 4), Cell { ch: 'D', style: s2 });
        assert_eq!(r.back_cell(1, 4), Cell { ch: 'E', style: s2 });
        assert_eq!(r.back_cell(2, 4), Cell { ch: 'F', style: s2 });
    }

    /// `content` (5 wide x 4 tall) is larger than the preview's interior
    /// (`rect = (0,0,4,3)` -> interior is `4 wide x (rect.h - 1) = 2` tall):
    /// the renderer TRUNCATES to the top-left `4x2` corner rather than
    /// scaling -- column 4 (`'E'`/`'J'` of content's first two rows) and
    /// content rows 2-3 (`"KLMNO"`/`"PQRST"`) never appear anywhere on
    /// screen (there isn't even room: a 3-row scene only has one row below
    /// the border).
    #[test]
    fn overlay_preview_truncates_oversized_grid() {
        let g = grid_with(4, 3, b"");
        let content: Vec<Cell> = "ABCDEFGHIJKLMNOPQRST".chars().map(|ch| Cell { ch, style: Style::default() }).collect();
        let scene = Scene {
            size: (4, 3),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 3 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: Vec::new(),
                top: 0,
                preview: Some(PreviewBlock { rect: Rect { x: 0, y: 0, w: 4, h: 3 }, title: String::new(), content_w: 5, content_h: 4, content }),
            })),
        };
        let mut r = Renderer::new(4, 3);
        r.compose_back(&scene);

        // Border row (y=0) is the top line, not content.
        assert_eq!(r.back_cell(0, 0).ch, '─');

        // Interior row 0 (y=1): only the first 4 of content's 5-wide row 0
        // ("ABCDE") appear -- 'E' is truncated.
        let row1: String = (0..4).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row1, "ABCD");

        // Interior row 1 (y=2): content row 1 ("FGHIJ"), again truncated to 4.
        // This is also the LAST row of the 3-row scene -- content rows 2-3
        // ("KLMNO"/"PQRST") are structurally unreachable, proving the
        // renderer never scales down to fit them in.
        let row2: String = (0..4).map(|x| r.back_cell(x, 2).ch).collect();
        assert_eq!(row2, "FGHI");
    }

    /// Sizing math per `docs/tmux-reference/choose-tree.md` `## 3.1`
    /// (`mode_tree_set_height`, NORMAL mode): `sy = 15` (panel height),
    /// `line_size = 15` (at or above the 2/3 split, so the "short list ->
    /// half" branch does NOT fire): `h = (15/3)*2 = 10`; `h(10) >
    /// line_size(15)`? no -> unchanged; `h(10) < 10`? no (equal) ->
    /// unchanged; `sy - h = 5 >= 2` -> the final drop-preview guard doesn't
    /// fire either. So the list gets exactly 10 rows and the preview gets
    /// the remaining 5 (row 10 = border, rows 11-14 = interior). The sizing
    /// FORMULA itself is dispatch.rs's responsibility (exercised end to end
    /// by server_proto's `choose_tree_v_toggles_preview`); this test
    /// constructs that already-computed 10-row split directly and asserts
    /// the RENDERER mechanically respects it: only rows 0-9 show list text,
    /// row 10 is the preview's border line, never list row 10's text.
    #[test]
    fn overlay_list_shrinks_to_two_thirds_when_preview_on() {
        let g = grid_with(20, 15, b"");
        let rows: Vec<TreeRowCell> = (0..15).map(|i| TreeRowCell { text: format!("r{i}"), depth: 0, marker: None, selected: false }).collect();
        let scene = Scene {
            size: (20, 15),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 20, h: 15 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows,
                top: 0,
                preview: Some(PreviewBlock {
                    rect: Rect { x: 0, y: 10, w: 20, h: 5 },
                    title: String::new(),
                    content_w: 0,
                    content_h: 0,
                    content: Vec::new(),
                }),
            })),
        };
        let mut r = Renderer::new(20, 15);
        r.compose_back(&scene);

        // Rows 0-9: list text "  r0".."  r9" (marker slot + text) -- all 10
        // rows the 2/3 split allots.
        for i in 0..10u16 {
            let line: String = (0..4).map(|x| r.back_cell(x, i).ch).collect();
            assert_eq!(line, format!("  r{i}"), "row {i} should still be a list row");
        }
        // Row 10 is the preview's border line, NOT list row 10's text --
        // proof the list was capped to 10 rows (2/3 of 15), not the full 15.
        assert_eq!(r.back_cell(0, 10).ch, '─');
    }

    /// display-panes' 5x5 block digit for `1`, exact cells, in a rect sized
    /// exactly to the bitmap's minimum (6 wide x 5 tall): per
    /// `digit_bitmap(1)` (`"..#..", ".##..", "..#..", "..#..", "#####"`),
    /// centering offsets `ox = (6-5)/2 = 0`, `oy = (5-5)/2 = 0`, so the
    /// bitmap occupies columns 0..5, rows 0..5 exactly with column 5 blank.
    /// `active: true` -> painted in `display_panes_active_colour` (bg red).
    #[test]
    fn overlay_digits_5x5() {
        let g = grid_with(6, 5, b"");
        let red = Style { bg: Color::Idx(1), ..Style::default() };
        let blue = Style { bg: Color::Idx(4), ..Style::default() };
        let scene = Scene {
            size: (6, 5),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 6, h: 5 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: blue,
            display_panes_active_colour: red,
            overlay: Some(Overlay::PaneDigits(vec![(Rect { x: 0, y: 0, w: 6, h: 5 }, 1, true)])),
        };
        let mut r = Renderer::new(6, 5);
        r.compose_back(&scene);

        // "on" cells per digit_bitmap(1), each a space char in the active
        // (red) style.
        let on_cells: &[(u16, u16)] = &[(2, 0), (1, 1), (2, 1), (2, 2), (2, 3), (0, 4), (1, 4), (2, 4), (3, 4), (4, 4)];
        for &(x, y) in on_cells {
            assert_eq!(r.back_cell(x, y).ch, ' ', "cell ({x},{y}) should be an 'on' block");
            assert_eq!(r.back_cell(x, y).style.bg, Color::Idx(1), "cell ({x},{y}) should be display_panes_active_colour (red)");
        }
        // An "off" cell within the bitmap's bounding box is left untouched
        // (still the pane's own default-styled blank content, not painted).
        assert_eq!(r.back_cell(0, 0).ch, ' ');
        assert_eq!(r.back_cell(0, 0).style, Style::default());
        // Column 5 (outside the 5-wide glyph, inside the 6-wide rect) is
        // also untouched.
        assert_eq!(r.back_cell(5, 0).style, Style::default());
    }

    /// A pane too small for the 5x5 block (below the 6x5 threshold) falls
    /// back to a single centered glyph in the resolved colour, not the block
    /// bitmap.
    #[test]
    fn overlay_digits_small_fallback() {
        let g = grid_with(3, 3, b"");
        let blue = Style { bg: Color::Idx(4), ..Style::default() };
        let scene = Scene {
            size: (3, 3),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 3 }, grid: &g, focused: false, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: blue,
            display_panes_active_colour: Style::default(),
            overlay: Some(Overlay::PaneDigits(vec![(Rect { x: 0, y: 0, w: 3, h: 3 }, 7, false)])),
        };
        let mut r = Renderer::new(3, 3);
        r.compose_back(&scene);
        // centered single glyph: x = 0 + 3/2 = 1, y = 0 + 3/2 = 1
        assert_eq!(r.back_cell(1, 1).ch, '7');
        assert_eq!(r.back_cell(1, 1).style.bg, Color::Idx(4));
        // nowhere else touched
        assert_eq!(r.back_cell(0, 0).ch, ' ');
        assert_eq!(r.back_cell(0, 0).style, Style::default());
    }
}
