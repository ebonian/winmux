use crate::geom::Rect;
use crate::grid::{Cell, Color, Grid, Style};
use crate::layout::PaneId;

pub struct PaneView<'a> {
    pub id: PaneId,
    pub rect: Rect,
    pub grid: &'a Grid,
    pub focused: bool,
    pub dead: bool,
}

pub struct Scene<'a> {
    pub size: (u16, u16),
    pub panes: Vec<PaneView<'a>>,
    pub zoomed: bool,
    pub status_left: String,
    pub status_right: String,
    pub message: Option<String>,
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
    /// (unless zoomed), dead-pane overlays, then the status bar.
    fn compose_back(&mut self, scene: &Scene) {
        let cols = self.cols;
        let rows = self.rows;
        let pane_rows = rows.saturating_sub(1); // last row is the status bar

        // 0) clear back to default cells
        for c in self.back.iter_mut() {
            *c = Cell::default();
        }

        // 1) copy each pane's grid into its rect
        for pv in &scene.panes {
            let r = pv.rect;
            for dy in 0..r.h {
                let y = r.y + dy;
                if y >= pane_rows {
                    continue;
                }
                for dx in 0..r.w {
                    let x = r.x + dx;
                    if x >= cols {
                        continue;
                    }
                    if dx < pv.grid.cols() && dy < pv.grid.rows() {
                        let cell = pv.grid.cell(dx, dy);
                        self.set(x, y, cell);
                    }
                }
            }
        }

        // 2) borders — only when not zoomed
        if !scene.zoomed {
            let w = cols as usize;
            let h = pane_rows as usize;
            let mut covered = vec![false; w * h];
            for pv in &scene.panes {
                let r = pv.rect;
                for dy in 0..r.h {
                    let y = r.y + dy;
                    if y >= pane_rows {
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
                if x < 0 || y < 0 || x >= cols as i32 || y >= pane_rows as i32 {
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
            for y in 0..pane_rows as i32 {
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
                    let mut style = Style::default();
                    if touches_focused(x, y) {
                        style.fg = Color::Idx(2); // green
                    }
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
            if y >= pane_rows {
                continue;
            }
            let style = Style { reverse: true, ..Style::default() };
            let x_end = pv.rect.x.saturating_add(pv.rect.w).min(cols);
            for (x, ch) in (pv.rect.x..x_end).zip("[exited]".chars()) {
                self.set(x, y, Cell { ch, style });
            }
        }

        // 4) status bar on the bottom row
        if rows == 0 {
            return;
        }
        let y = rows - 1;
        let cols_u = cols as usize;
        let (style, message) = match &scene.message {
            Some(m) => {
                // black on yellow
                let s = Style { fg: Color::Idx(0), bg: Color::Idx(3), ..Style::default() };
                (s, Some(m.clone()))
            }
            None => {
                // black on green
                let s = Style { fg: Color::Idx(0), bg: Color::Idx(2), ..Style::default() };
                (s, None)
            }
        };
        // fill the row with styled spaces
        for x in 0..cols {
            self.set(x, y, Cell { ch: ' ', style });
        }
        if let Some(msg) = message {
            for (i, ch) in msg.chars().enumerate() {
                if i >= cols_u {
                    break;
                }
                self.set(i as u16, y, Cell { ch, style });
            }
        } else {
            let left: Vec<char> = scene.status_left.chars().collect();
            let right: Vec<char> = scene.status_right.chars().collect();
            for (i, &ch) in left.iter().enumerate() {
                if i >= cols_u {
                    break;
                }
                self.set(i as u16, y, Cell { ch, style });
            }
            let left_len = left.len().min(cols_u);
            let max_right = cols_u - left_len;
            let right_len = right.len().min(max_right); // truncate right first
            let start = cols_u - right_len;
            for (i, &ch) in right[..right_len].iter().enumerate() {
                self.set((start + i) as u16, y, Cell { ch, style });
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
        let mut g = Grid::new(cols, rows);
        g.feed(bytes);
        g
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
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 3 }, grid: &left, focused: false, dead: false },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 3 }, grid: &right, focused: true, dead: false },
            ],
            zoomed: false,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 4 }, grid: &left, focused: false, dead: false },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 1 }, grid: &rt, focused: false, dead: false },
                PaneView { id: 3, rect: Rect { x: 4, y: 2, w: 3, h: 2 }, grid: &rb, focused: true, dead: false },
            ],
            zoomed: false,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 10, h: 1 }, grid: &g, focused: true, dead: false }],
            zoomed: false,
            status_left: "AB".into(),
            status_right: "Z".into(),
            message: None,
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
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 6, h: 1 }, grid: &g, focused: true, dead: false }],
            zoomed: false,
            status_left: "ab".into(),
            status_right: "123456".into(),
            message: None,
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
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 5, h: 1 }, grid: &g, focused: true, dead: false }],
            zoomed: false,
            status_left: "ignored".into(),
            status_right: "ignored".into(),
            message: Some("hey".into()),
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
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 10, h: 1 }, grid: &g, focused: true, dead: true }],
            zoomed: false,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 1 }, grid: &left, focused: false, dead: false },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 1 }, grid: &right, focused: true, dead: false },
            ],
            zoomed: true,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
                panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false }],
                zoomed: false,
                status_left: String::new(),
                status_right: String::new(),
                message: None,
            };
            let _ = r.compose(&scene, Some((0, 0)), true);
        }

        // change exactly cell (0,0): 'a' -> 'X'
        g.feed(b"\x1b[1;1HX");

        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false }],
            zoomed: false,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
                panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false }],
                zoomed: false,
                status_left: String::new(),
                status_right: String::new(),
                message: None,
            };
            let _ = r.compose(&scene, Some((0, 0)), true);
        }

        // change (0,0) and (1,0): "ab" -> "XY"
        g.feed(b"\x1b[1;1HXY");

        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false }],
            zoomed: false,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false }],
            zoomed: false,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
                panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false }],
                zoomed: false,
                status_left: String::new(),
                status_right: String::new(),
                message: None,
            };
            let _ = r.compose(&scene, Some((0, 0)), true); // prime
        }

        r.resize(4, 2); // invalidates front

        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 1 }, grid: &g, focused: true, dead: false }],
            zoomed: false,
            status_left: String::new(),
            status_right: String::new(),
            message: None,
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
}
