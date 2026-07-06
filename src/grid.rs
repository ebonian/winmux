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
}

impl TermState {
    fn new(cols: u16, rows: u16) -> Self {
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

    /// SGR — basic subset for Task 4 (Task 5 replaces this with the full set).
    fn apply_sgr(&mut self, params: &Params) {
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
                4 => self.style.underline = true,
                7 => self.style.reverse = true,
                30..=37 => self.style.fg = Color::Idx((flat[i] - 30) as u8),
                39 => self.style.fg = Color::Default,
                40..=47 => self.style.bg = Color::Idx((flat[i] - 40) as u8),
                49 => self.style.bg = Color::Default,
                _ => {}
            }
            i += 1;
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
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
            'm' => self.apply_sgr(params),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}

pub struct Grid {
    parser: Parser,
    state: TermState,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        Grid { parser: Parser::new(), state: TermState::new(cols, rows) }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.parser.advance(&mut self.state, b);
        }
    }

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
            "cell out of range"
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
    fn dectcem_visibility() {
        let mut g = Grid::new(5, 1);
        assert!(g.cursor_visible());
        g.feed(b"\x1b[?25l");
        assert!(!g.cursor_visible());
        g.feed(b"\x1b[?25h");
        assert!(g.cursor_visible());
    }
}
