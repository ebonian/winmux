//! Per-pane terminal emulator: `Cell`/`Style`/`Color` types plus a vte-driven `Grid`.

use vte::{Params, Parser, Perform};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            reverse: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', style: Style::default() }
    }
}

#[derive(Clone, Copy)]
struct SavedCursor {
    col: u16,
    row: u16,
    style: Style,
    autowrap: bool,
}

/// Emulator state; the vte performer. Separate from the Parser so `feed`
/// can borrow the parser and this state disjointly.
struct TermState {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    cursor_col: u16,
    cursor_row: u16,
    style: Style,
    autowrap: bool,
    wrap_pending: bool,
    cursor_visible: bool,
    scroll_top: u16,
    scroll_bottom: u16,
    saved: Option<SavedCursor>,
}

impl TermState {
    fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        TermState {
            cols,
            rows,
            cells: vec![Cell::default(); cols as usize * rows as usize],
            cursor_col: 0,
            cursor_row: 0,
            style: Style::default(),
            autowrap: true,
            wrap_pending: false,
            cursor_visible: true,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            saved: None,
        }
    }

    fn idx(&self, col: u16, row: u16) -> usize {
        row as usize * self.cols as usize + col as usize
    }

    /// Scroll the region [scroll_top, scroll_bottom] up by `n`, blanking the
    /// vacated bottom rows.
    fn scroll_up(&mut self, n: u16) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;
        let cols = self.cols as usize;
        let n = n as usize;
        for row in top..=bottom {
            let src = row + n;
            if src <= bottom {
                for col in 0..cols {
                    self.cells[row * cols + col] = self.cells[src * cols + col];
                }
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
            }
        }
    }

    /// Scroll the region [scroll_top, scroll_bottom] down by `n`, blanking
    /// the vacated top rows.
    fn scroll_down(&mut self, n: u16) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;
        let cols = self.cols as usize;
        let n = n as usize;
        for row in (top..=bottom).rev() {
            if row >= top + n {
                let src = row - n;
                for col in 0..cols {
                    self.cells[row * cols + col] = self.cells[src * cols + col];
                }
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
            }
        }
    }

    /// RI: move up one row, scrolling the region down if at its top.
    fn reverse_index(&mut self) {
        if self.cursor_row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
        }
    }

    fn insert_chars(&mut self, n: u16) {
        let cols = self.cols as usize;
        let start = self.cursor_row as usize * cols;
        let col = self.cursor_col as usize;
        let n = (n as usize).min(cols - col);
        for c in (col..cols).rev() {
            if c >= col + n {
                self.cells[start + c] = self.cells[start + c - n];
            } else {
                self.cells[start + c] = Cell::default();
            }
        }
    }

    fn delete_chars(&mut self, n: u16) {
        let cols = self.cols as usize;
        let start = self.cursor_row as usize * cols;
        let col = self.cursor_col as usize;
        let n = (n as usize).min(cols - col);
        for c in col..cols {
            if c + n < cols {
                self.cells[start + c] = self.cells[start + c + n];
            } else {
                self.cells[start + c] = Cell::default();
            }
        }
    }

    fn erase_chars(&mut self, n: u16) {
        let cols = self.cols as usize;
        let start = self.cursor_row as usize * cols;
        let col = self.cursor_col as usize;
        let end = (col + n as usize).min(cols);
        for c in col..end {
            self.cells[start + c] = Cell::default();
        }
    }

    fn insert_lines(&mut self, n: u16) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }
        let cols = self.cols as usize;
        let top = self.cursor_row as usize;
        let bottom = self.scroll_bottom as usize;
        let n = (n as usize).min(bottom - top + 1);
        for row in (top..=bottom).rev() {
            if row >= top + n {
                let src = row - n;
                for col in 0..cols {
                    self.cells[row * cols + col] = self.cells[src * cols + col];
                }
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
            }
        }
    }

    fn delete_lines(&mut self, n: u16) {
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }
        let cols = self.cols as usize;
        let top = self.cursor_row as usize;
        let bottom = self.scroll_bottom as usize;
        let n = (n as usize).min(bottom - top + 1);
        for row in top..=bottom {
            let src = row + n;
            if src <= bottom {
                for col in 0..cols {
                    self.cells[row * cols + col] = self.cells[src * cols + col];
                }
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
            }
        }
    }

    fn save_cursor(&mut self) {
        self.saved = Some(SavedCursor {
            col: self.cursor_col,
            row: self.cursor_row,
            style: self.style,
            autowrap: self.autowrap,
        });
    }

    fn restore_cursor(&mut self) {
        if let Some(s) = self.saved {
            self.cursor_col = s.col.min(self.cols - 1);
            self.cursor_row = s.row.min(self.rows - 1);
            self.style = s.style;
            self.autowrap = s.autowrap;
        }
        self.wrap_pending = false;
    }

    /// LF: move down one row, scrolling the region up if at its bottom.
    fn line_feed(&mut self) {
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let total = self.cols as usize * self.rows as usize;
        let cur = self.idx(self.cursor_col, self.cursor_row);
        match mode {
            0 => {
                for i in cur..total {
                    self.cells[i] = Cell::default();
                }
            }
            1 => {
                for i in 0..=cur {
                    self.cells[i] = Cell::default();
                }
            }
            _ => {
                for i in 0..total {
                    self.cells[i] = Cell::default();
                }
            }
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let cols = self.cols as usize;
        let start = self.cursor_row as usize * cols;
        let col = self.cursor_col as usize;
        match mode {
            0 => {
                for c in col..cols {
                    self.cells[start + c] = Cell::default();
                }
            }
            1 => {
                for c in 0..=col {
                    self.cells[start + c] = Cell::default();
                }
            }
            _ => {
                for c in 0..cols {
                    self.cells[start + c] = Cell::default();
                }
            }
        }
    }

    fn apply_sgr(&mut self, params: &Params) {
        // Flatten every subparameter in order. Semicolon form "38;5;n" and
        // colon form "38:5:n" both flatten to [38,5,n], so one code path
        // serves both.
        let flat: Vec<u16> = params.iter().flat_map(|s| s.iter().copied()).collect();
        if flat.is_empty() {
            self.style = Style::default();
            return;
        }
        let mut i = 0;
        while i < flat.len() {
            match flat[i] {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                2 => self.style.dim = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                7 => self.style.reverse = true,
                22 => {
                    self.style.bold = false;
                    self.style.dim = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                27 => self.style.reverse = false,
                30..=37 => self.style.fg = Color::Idx((flat[i] - 30) as u8),
                39 => self.style.fg = Color::Default,
                40..=47 => self.style.bg = Color::Idx((flat[i] - 40) as u8),
                49 => self.style.bg = Color::Default,
                90..=97 => self.style.fg = Color::Idx((flat[i] - 90 + 8) as u8),
                100..=107 => self.style.bg = Color::Idx((flat[i] - 100 + 8) as u8),
                38 => {
                    // Extended fg color. A truncated or unknown-mode sequence
                    // makes the rest of the params unparseable: discard them
                    // (break) rather than reinterpret color args as SGR codes.
                    if i + 1 >= flat.len() {
                        break;
                    }
                    match flat[i + 1] {
                        5 => {
                            if i + 2 >= flat.len() {
                                break;
                            }
                            self.style.fg = Color::Idx(flat[i + 2] as u8);
                            i += 2;
                        }
                        2 => {
                            if i + 4 >= flat.len() {
                                break;
                            }
                            self.style.fg = Color::Rgb(
                                flat[i + 2] as u8,
                                flat[i + 3] as u8,
                                flat[i + 4] as u8,
                            );
                            i += 4;
                        }
                        _ => break,
                    }
                }
                48 => {
                    // Extended bg color; same discard-on-truncation rule.
                    if i + 1 >= flat.len() {
                        break;
                    }
                    match flat[i + 1] {
                        5 => {
                            if i + 2 >= flat.len() {
                                break;
                            }
                            self.style.bg = Color::Idx(flat[i + 2] as u8);
                            i += 2;
                        }
                        2 => {
                            if i + 4 >= flat.len() {
                                break;
                            }
                            self.style.bg = Color::Rgb(
                                flat[i + 2] as u8,
                                flat[i + 3] as u8,
                                flat[i + 4] as u8,
                            );
                            i += 4;
                        }
                        _ => break,
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut new_cells = vec![Cell::default(); cols as usize * rows as usize];
        let copy_cols = cols.min(self.cols) as usize;
        let copy_rows = rows.min(self.rows) as usize;
        for r in 0..copy_rows {
            for c in 0..copy_cols {
                new_cells[r * cols as usize + c] = self.cells[r * self.cols as usize + c];
            }
        }
        self.cells = new_cells;
        self.cols = cols;
        self.rows = rows;
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.wrap_pending = false;
    }
}

/// Read subparameter 0 of CSI param `idx`, or `default` if absent/empty.
/// Does NOT map an explicit 0 to the default — callers apply `.max(1)`
/// for movement commands.
fn param_or(params: &Params, idx: usize, default: u16) -> u16 {
    match params.iter().nth(idx) {
        Some(s) if !s.is_empty() => s[0],
        _ => default,
    }
}

impl Perform for TermState {
    fn print(&mut self, c: char) {
        if self.wrap_pending && self.autowrap {
            self.cursor_col = 0;
            self.line_feed();
        }
        self.wrap_pending = false;
        let i = self.idx(self.cursor_col, self.cursor_row);
        self.cells[i] = Cell { ch: c, style: self.style };
        if self.cursor_col + 1 >= self.cols {
            if self.autowrap {
                self.wrap_pending = true;
            }
        } else {
            self.cursor_col += 1;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                // BS
                self.wrap_pending = false;
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
            }
            0x09 => {
                // HT — next 8-col tab stop, clamped
                self.wrap_pending = false;
                let next = ((self.cursor_col / 8) + 1) * 8;
                self.cursor_col = next.min(self.cols.saturating_sub(1));
            }
            0x0A => {
                // LF
                self.wrap_pending = false;
                self.line_feed();
            }
            0x0D => {
                // CR
                self.wrap_pending = false;
                self.cursor_col = 0;
            }
            // BEL (0x07) and all other C0 are ignored.
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        // Private-marker sequences (CSI ? ...) carry b'?' in intermediates.
        let private = intermediates.first() == Some(&b'?');
        if private {
            if action == 'h' || action == 'l' {
                let set = action == 'h';
                for p in params.iter() {
                    match p.first().copied() {
                        Some(7) => self.autowrap = set,   // DECAWM
                        Some(25) => self.cursor_visible = set, // DECTCEM
                        Some(1049) => {
                            // Alt screen enter/leave: both clear + home (MVP).
                            self.erase_display(2);
                            self.cursor_col = 0;
                            self.cursor_row = 0;
                            self.wrap_pending = false;
                        }
                        _ => {}
                    }
                }
            }
            return;
        }
        match action {
            'A' => {
                self.wrap_pending = false;
                let n = param_or(params, 0, 1).max(1);
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                self.wrap_pending = false;
                let n = param_or(params, 0, 1).max(1);
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
            }
            'C' => {
                self.wrap_pending = false;
                let n = param_or(params, 0, 1).max(1);
                self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
            }
            'D' => {
                self.wrap_pending = false;
                let n = param_or(params, 0, 1).max(1);
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'E' => {
                self.wrap_pending = false;
                let n = param_or(params, 0, 1).max(1);
                self.cursor_col = 0;
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
            }
            'F' => {
                self.wrap_pending = false;
                let n = param_or(params, 0, 1).max(1);
                self.cursor_col = 0;
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'G' => {
                self.wrap_pending = false;
                let col = param_or(params, 0, 1).max(1) - 1;
                self.cursor_col = col.min(self.cols - 1);
            }
            'H' | 'f' => {
                self.wrap_pending = false;
                let row = param_or(params, 0, 1).max(1) - 1;
                let col = param_or(params, 1, 1).max(1) - 1;
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_col = col.min(self.cols - 1);
            }
            'J' => self.erase_display(param_or(params, 0, 0)),
            'K' => self.erase_line(param_or(params, 0, 0)),
            '@' => self.insert_chars(param_or(params, 0, 1).max(1)),
            'P' => self.delete_chars(param_or(params, 0, 1).max(1)),
            'X' => self.erase_chars(param_or(params, 0, 1).max(1)),
            'L' => self.insert_lines(param_or(params, 0, 1).max(1)),
            'M' => self.delete_lines(param_or(params, 0, 1).max(1)),
            'S' => self.scroll_up(param_or(params, 0, 1).max(1)),
            'T' => self.scroll_down(param_or(params, 0, 1).max(1)),
            'r' => {
                // DECSTBM: set scroll region (1-based, inclusive) and home.
                let top = param_or(params, 0, 1).max(1) - 1;
                let bottom = param_or(params, 1, self.rows).max(1) - 1;
                if top < bottom && bottom < self.rows {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                } else {
                    self.scroll_top = 0;
                    self.scroll_bottom = self.rows - 1;
                }
                self.cursor_col = 0;
                self.cursor_row = 0;
                self.wrap_pending = false;
            }
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            'm' => self.apply_sgr(params),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.save_cursor(),    // DECSC
            b'8' => self.restore_cursor(), // DECRC
            b'M' => {
                // RI (reverse index)
                self.wrap_pending = false;
                self.reverse_index();
            }
            _ => {}
        }
    }
}

pub struct Grid {
    parser: Parser,
    state: TermState,
}

impl Grid {
    /// Create a grid. Dimensions are clamped to a 1x1 minimum: a grid is
    /// never zero-sized.
    pub fn new(cols: u16, rows: u16) -> Self {
        Grid { parser: Parser::new(), state: TermState::new(cols, rows) }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.parser.advance(&mut self.state, b);
        }
    }

    /// Resize the grid, preserving the overlapping region. Dimensions are
    /// clamped to a 1x1 minimum: a grid is never zero-sized.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.state.resize(cols, rows);
    }

    pub fn cols(&self) -> u16 {
        self.state.cols
    }

    pub fn rows(&self) -> u16 {
        self.state.rows
    }

    pub fn cell(&self, col: u16, row: u16) -> Cell {
        assert!(
            col < self.state.cols && row < self.state.rows,
            "cell({col}, {row}) out of bounds {}x{}",
            self.state.cols,
            self.state.rows
        );
        self.state.cells[self.state.idx(col, row)]
    }

    pub fn cursor(&self) -> (u16, u16) {
        (self.state.cursor_col, self.state.cursor_row)
    }

    pub fn cursor_visible(&self) -> bool {
        self.state.cursor_visible
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect a whole row into a String for easy assertions.
    fn row_str(g: &Grid, row: u16) -> String {
        (0..g.cols()).map(|c| g.cell(c, row).ch).collect()
    }

    #[test]
    fn print_autowrap_deferred() {
        // 5 cols: "hello" fills the row; the last 'o' arms deferred wrap,
        // cursor stays parked at the last column until the NEXT printable char.
        let mut g = Grid::new(5, 2);
        g.feed(b"hello");
        assert_eq!(row_str(&g, 0), "hello");
        assert_eq!(g.cursor(), (4, 0));
        g.feed(b"!");
        assert_eq!(g.cursor(), (1, 1));
        assert_eq!(g.cell(0, 1).ch, '!');
        assert_eq!(row_str(&g, 1), "!    ");
    }

    #[test]
    fn autowrap_disabled() {
        // CSI ?7l turns DECAWM off: last-column prints overwrite in place.
        let mut g = Grid::new(5, 2);
        g.feed(b"\x1b[?7lhello!");
        assert_eq!(row_str(&g, 0), "hell!");
        assert_eq!(g.cursor(), (4, 0));
        assert_eq!(row_str(&g, 1), "     ");
    }

    #[test]
    fn backspace_overwrites() {
        // a,b -> BS moves back over b -> c overwrites it.
        let mut g = Grid::new(5, 2);
        g.feed(b"ab\x08c");
        assert_eq!(row_str(&g, 0), "ac   ");
        assert_eq!(g.cursor(), (2, 0));
    }

    #[test]
    fn cr_lf() {
        let mut g = Grid::new(5, 3);
        g.feed(b"abc\r");
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\n");
        assert_eq!(g.cursor(), (0, 1));
        assert_eq!(row_str(&g, 0), "abc  ");
    }

    #[test]
    fn horizontal_tab() {
        // 8-col tab stops, clamped to the last column.
        let mut g = Grid::new(20, 1);
        g.feed(b"\t");
        assert_eq!(g.cursor(), (8, 0));
        g.feed(b"\t");
        assert_eq!(g.cursor(), (16, 0));
        g.feed(b"\t");
        assert_eq!(g.cursor(), (19, 0));
    }

    #[test]
    fn line_feed_scrolls_at_bottom() {
        // Two CRLFs: the second is issued at the bottom row -> scroll up.
        let mut g = Grid::new(3, 2);
        g.feed(b"a\r\nb\r\n");
        assert_eq!(row_str(&g, 0), "b  ");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(g.cursor(), (0, 1));
        g.feed(b"c");
        assert_eq!(row_str(&g, 1), "c  ");
    }

    #[test]
    fn cursor_movement() {
        let mut g = Grid::new(10, 5);
        g.feed(b"\x1b[3;4H"); // CUP row3 col4 -> (3,2) 0-based
        assert_eq!(g.cursor(), (3, 2));
        g.feed(b"\x1b[2A");   // CUU
        assert_eq!(g.cursor(), (3, 0));
        g.feed(b"\x1b[1B");   // CUD
        assert_eq!(g.cursor(), (3, 1));
        g.feed(b"\x1b[2C");   // CUF
        assert_eq!(g.cursor(), (5, 1));
        g.feed(b"\x1b[3D");   // CUB
        assert_eq!(g.cursor(), (2, 1));
    }

    #[test]
    fn cup_and_hvp() {
        let mut g = Grid::new(10, 5);
        g.feed(b"\x1b[H");     // home
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\x1b[2;3f");  // HVP row2 col3 -> (2,1)
        assert_eq!(g.cursor(), (2, 1));
    }

    #[test]
    fn cnl_cpl_cha() {
        let mut g = Grid::new(10, 5);
        g.feed(b"\x1b[5;5H"); // (4,4)
        assert_eq!(g.cursor(), (4, 4));
        g.feed(b"\x1b[2F");   // CPL -> col0, up 2
        assert_eq!(g.cursor(), (0, 2));
        g.feed(b"\x1b[1E");   // CNL -> col0, down 1
        assert_eq!(g.cursor(), (0, 3));
        g.feed(b"\x1b[7G");   // CHA -> col7 (0-based 6)
        assert_eq!(g.cursor(), (6, 3));
    }

    #[test]
    fn erase_display_below() {
        let mut g = Grid::new(3, 3);
        g.feed(b"xxxxxxxxx");        // fills 3x3 via autowrap
        g.feed(b"\x1b[2;2H\x1b[0J"); // cursor (1,1); clear to end
        assert_eq!(row_str(&g, 0), "xxx");
        assert_eq!(row_str(&g, 1), "x  ");
        assert_eq!(row_str(&g, 2), "   ");
    }

    #[test]
    fn erase_display_above() {
        let mut g = Grid::new(3, 3);
        g.feed(b"xxxxxxxxx");
        g.feed(b"\x1b[2;2H\x1b[1J"); // clear start..=cursor
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(row_str(&g, 1), "  x");
        assert_eq!(row_str(&g, 2), "xxx");
    }

    #[test]
    fn erase_display_all() {
        let mut g = Grid::new(3, 3);
        g.feed(b"xxxxxxxxx");
        g.feed(b"\x1b[2J");
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(row_str(&g, 2), "   ");
    }

    #[test]
    fn erase_line_right() {
        let mut g = Grid::new(5, 2);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;3H\x1b[0K"); // cursor col3(0-based 2); clear to eol
        assert_eq!(row_str(&g, 0), "ab   ");
    }

    #[test]
    fn erase_line_left() {
        let mut g = Grid::new(5, 2);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;3H\x1b[1K"); // clear col0..=col2
        assert_eq!(row_str(&g, 0), "   de");
    }

    #[test]
    fn erase_line_all() {
        let mut g = Grid::new(5, 2);
        g.feed(b"abcde");
        g.feed(b"\x1b[2K");
        assert_eq!(row_str(&g, 0), "     ");
    }

    #[test]
    fn sgr_basic() {
        let mut g = Grid::new(5, 1);
        g.feed(b"\x1b[1;31mA");
        let a = g.cell(0, 0);
        assert_eq!(a.ch, 'A');
        assert!(a.style.bold);
        assert_eq!(a.style.fg, Color::Idx(1));
        g.feed(b"\x1b[0mB");
        let b = g.cell(1, 0);
        assert!(!b.style.bold);
        assert_eq!(b.style.fg, Color::Default);
    }

    #[test]
    fn sgr_attrs_and_bg() {
        let mut g = Grid::new(5, 1);
        g.feed(b"\x1b[4;7;42mX"); // underline, reverse, bg green(idx2)
        let x = g.cell(0, 0);
        assert!(x.style.underline);
        assert!(x.style.reverse);
        assert_eq!(x.style.bg, Color::Idx(2));
        g.feed(b"\x1b[39;49mY"); // fg/bg back to default
        let y = g.cell(1, 0);
        assert_eq!(y.style.fg, Color::Default);
        assert_eq!(y.style.bg, Color::Default);
    }

    #[test]
    #[should_panic(expected = "cell(90, 5) out of bounds 80x24")]
    fn cell_panic_message_includes_coordinates_and_dimensions() {
        // Follow-up #6: the panic message must include both the requested
        // coordinates AND the grid's actual dimensions.
        let g = Grid::new(80, 24);
        g.cell(90, 5);
    }

    #[test]
    fn zero_size_new_clamps_to_1x1() {
        let mut g = Grid::new(0, 0);
        assert_eq!(g.cols(), 1);
        assert_eq!(g.rows(), 1);
        g.feed(b"\x1b[5;5Hx"); // must not panic
        assert_eq!(g.cell(0, 0).ch, 'x');
    }

    #[test]
    fn zero_size_resize_clamps_to_1x1() {
        let mut g = Grid::new(5, 5);
        g.resize(0, 5);
        assert_eq!(g.cols(), 1);
        assert_eq!(g.rows(), 5);
        g.feed(b"\x1b[2;2HX\x1b[1;1C"); // must not panic
    }

    #[test]
    fn dectcem_visibility() {
        let mut g = Grid::new(5, 1);
        assert!(g.cursor_visible());
        g.feed(b"\x1b[?25l");
        assert!(!g.cursor_visible());
        g.feed(b"\x1b[?25h");
        assert!(g.cursor_visible());
    }

    #[test]
    fn insert_chars() {
        // "abcde", cursor at col1, ICH 2: 'a' | 2 blanks | 'b','c' (d,e drop off)
        let mut g = Grid::new(5, 1);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;2H\x1b[2@");
        assert_eq!(row_str(&g, 0), "a  bc");
    }

    #[test]
    fn delete_chars() {
        // "abcde", cursor at col1, DCH 2: 'a' + shift 'd','e' left, blanks fill
        let mut g = Grid::new(5, 1);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;2H\x1b[2P");
        assert_eq!(row_str(&g, 0), "ade  ");
    }

    #[test]
    fn erase_chars() {
        // "abcde", cursor at col1, ECH 2: blank col1,col2 in place (no shift)
        let mut g = Grid::new(5, 1);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;2H\x1b[2X");
        assert_eq!(row_str(&g, 0), "a  de");
    }

    #[test]
    fn insert_lines() {
        let mut g = Grid::new(3, 4);
        g.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        g.feed(b"\x1b[2;1H\x1b[L"); // cursor row1 (0-based); insert 1 blank line
        assert_eq!(row_str(&g, 0), "aaa");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(row_str(&g, 2), "bbb");
        assert_eq!(row_str(&g, 3), "ccc");
    }

    #[test]
    fn delete_lines() {
        let mut g = Grid::new(3, 4);
        g.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        g.feed(b"\x1b[2;1H\x1b[M"); // cursor row1; delete 1 line
        assert_eq!(row_str(&g, 0), "aaa");
        assert_eq!(row_str(&g, 1), "ccc");
        assert_eq!(row_str(&g, 2), "ddd");
        assert_eq!(row_str(&g, 3), "   ");
    }

    #[test]
    fn scroll_up_su() {
        let mut g = Grid::new(3, 3);
        g.feed(b"aaa\r\nbbb\r\nccc");
        g.feed(b"\x1b[S");
        assert_eq!(row_str(&g, 0), "bbb");
        assert_eq!(row_str(&g, 1), "ccc");
        assert_eq!(row_str(&g, 2), "   ");
    }

    #[test]
    fn scroll_down_sd() {
        let mut g = Grid::new(3, 3);
        g.feed(b"aaa\r\nbbb\r\nccc");
        g.feed(b"\x1b[T");
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(row_str(&g, 1), "aaa");
        assert_eq!(row_str(&g, 2), "bbb");
    }

    #[test]
    fn scroll_region_linefeed() {
        // Region rows 2..3 (1-based) => indices 1..2. LF at region bottom
        // scrolls only that region.
        let mut g = Grid::new(3, 4);
        g.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        g.feed(b"\x1b[2;3r"); // DECSTBM
        g.feed(b"\x1b[3;1H"); // cursor to index (0,2) = region bottom
        g.feed(b"\n");
        assert_eq!(row_str(&g, 0), "aaa");
        assert_eq!(row_str(&g, 1), "ccc");
        assert_eq!(row_str(&g, 2), "   ");
        assert_eq!(row_str(&g, 3), "ddd");
    }

    #[test]
    fn reverse_index_at_top() {
        // RI at region top scrolls the region down.
        let mut g = Grid::new(3, 4);
        g.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        g.feed(b"\x1b[2;3r"); // region indices 1..2
        g.feed(b"\x1b[2;1H"); // cursor to index (0,1) = region top
        g.feed(b"\x1bM");     // RI
        assert_eq!(row_str(&g, 0), "aaa");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(row_str(&g, 2), "bbb");
        assert_eq!(row_str(&g, 3), "ddd");
    }

    #[test]
    fn save_restore_cursor_esc() {
        let mut g = Grid::new(10, 5);
        g.feed(b"\x1b[3;4H\x1b7\x1b[H"); // to (3,2), save, home
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\x1b8");
        assert_eq!(g.cursor(), (3, 2));
    }

    #[test]
    fn save_restore_cursor_csi() {
        let mut g = Grid::new(10, 5);
        g.feed(b"\x1b[3;4H\x1b[s\x1b[H");
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\x1b[u");
        assert_eq!(g.cursor(), (3, 2));
    }

    #[test]
    fn sgr_extended_colors() {
        let mut g = Grid::new(5, 1);
        g.feed(b"\x1b[38;5;196mA");        // 256-color fg
        assert_eq!(g.cell(0, 0).style.fg, Color::Idx(196));
        g.feed(b"\x1b[48;2;10;20;30mB");   // truecolor bg
        assert_eq!(g.cell(1, 0).style.bg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn sgr_bright_and_reset_attrs() {
        let mut g = Grid::new(5, 1);
        g.feed(b"\x1b[90;103mA"); // bright fg -> Idx(8), bright bg -> Idx(11)
        let a = g.cell(0, 0);
        assert_eq!(a.style.fg, Color::Idx(8));
        assert_eq!(a.style.bg, Color::Idx(11));
        g.feed(b"\x1b[1;22mB");   // bold then clear bold
        assert!(!g.cell(1, 0).style.bold);
        g.feed(b"\x1b[3;23mC");   // italic then clear italic
        assert!(!g.cell(2, 0).style.italic);
        g.feed(b"\x1b[4;24;7;27mD"); // underline+clear, reverse+clear
        let d = g.cell(3, 0);
        assert!(!d.style.underline);
        assert!(!d.style.reverse);
    }

    #[test]
    fn sgr_truncated_extended_colors_ignored() {
        // Truncated extended-color sequences must be discarded, not
        // reinterpreted: [38,2,30] is NOT "dim + fg black".
        let mut g = Grid::new(5, 1);
        g.feed(b"\x1b[38;2;30mA"); // truecolor fg missing g,b
        let a = g.cell(0, 0);
        assert!(!a.style.dim);
        assert_eq!(a.style.fg, Color::Default);
        g.feed(b"\x1b[48;2mB"); // truecolor bg missing r,g,b
        let b = g.cell(1, 0);
        assert!(!b.style.dim);
        assert_eq!(b.style.fg, Color::Default);
        assert_eq!(b.style.bg, Color::Default);
        g.feed(b"\x1b[1;38;2mC"); // params before the truncated introducer apply
        let c = g.cell(2, 0);
        assert!(c.style.bold);
        assert_eq!(c.style.fg, Color::Default);
    }

    #[test]
    fn il_dl_noop_outside_scroll_region() {
        // IL/DL with the cursor outside the DECSTBM region must not move rows.
        let mut g = Grid::new(3, 4);
        g.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        g.feed(b"\x1b[2;3r"); // region indices 1..2
        g.feed(b"\x1b[4;1H"); // cursor row index 3, outside region
        g.feed(b"\x1b[L");
        g.feed(b"\x1b[M");
        assert_eq!(row_str(&g, 0), "aaa");
        assert_eq!(row_str(&g, 1), "bbb");
        assert_eq!(row_str(&g, 2), "ccc");
        assert_eq!(row_str(&g, 3), "ddd");
    }

    #[test]
    fn alt_screen_clears_and_homes() {
        let mut g = Grid::new(3, 3);
        g.feed(b"xxxxxxxxx");
        g.feed(b"\x1b[?1049h");
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(row_str(&g, 2), "   ");
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"yyy");
        g.feed(b"\x1b[?1049l"); // leave also clears + homes in MVP
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(g.cursor(), (0, 0));
    }

    #[test]
    fn osc_and_unknown_ignored() {
        let mut g = Grid::new(5, 1);
        g.feed(b"\x1b]0;my title\x07A"); // OSC set-title then print 'A'
        assert_eq!(g.cell(0, 0).ch, 'A');
        g.feed(b"\x1b[99;99Z");           // unknown CSI final byte -> ignored
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cursor(), (1, 0));
    }

    #[test]
    fn resize_clips_and_clamps() {
        let mut g = Grid::new(5, 3);
        g.feed(b"abc"); // cursor at (3,0)
        g.resize(2, 2);
        assert_eq!(g.cols(), 2);
        assert_eq!(g.rows(), 2);
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(1, 0).ch, 'b'); // 'c' clipped
        assert_eq!(g.cursor(), (1, 0));   // clamped from (3,0)
    }

    #[test]
    fn resize_grows_and_pads() {
        let mut g = Grid::new(2, 2);
        g.feed(b"ab");
        g.resize(4, 3);
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(1, 0).ch, 'b');
        assert_eq!(g.cell(3, 2).ch, ' '); // padded
    }
}
