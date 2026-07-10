//! Per-pane terminal emulator: `Cell`/`Style`/`Color` types plus a vte-driven `Grid`.

use std::collections::VecDeque;
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

/// A pane application's requested mouse REPORTING protocol, tracked from
/// DECSET/DECRST 9 (X10 compatibility) / 1000 (VT200 "normal"/click-only) /
/// 1002 ("button-event"/drag) / 1003 ("any-event"/all motion) (SP7 Task 3;
/// `Task 9` consumes this to decide what to forward/re-encode to the pane
/// app). Verified against tmux's own pane-mode tracking
/// (`input_csi_dispatch_sm_private`/`_rm_private`, tmux `input.c`): SET of
/// ANY of 1000/1002/1003 first clears every other mouse-mode bit
/// (`ALL_MOUSE_MODES`) before setting its own, i.e. these modes are
/// mutually exclusive and the LAST one SET simply wins outright (not a
/// priority order among simultaneously-set bits — only one is ever set at
/// a time); RESET of any of them clears unconditionally to `Off`,
/// regardless of which mode number is named in the reset or which one was
/// actually active. Modern tmux has no `MODE_MOUSE_X10` bit at all (mode 9
/// is legacy/unimplemented pane-side in current tmux), but winmux tracks it
/// with the same mutual-exclusion rule as 1000/1002/1003 since a later
/// task's interface promise requires the `X10` variant to exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseProto {
    #[default]
    Off,
    X10,
    Normal,
    Button,
    Any,
}

/// A pane application's requested mouse COORDINATE ENCODING, tracked from
/// DECSET/DECRST 1005 (UTF-8) / 1006 (SGR) -- independent bits in real tmux
/// (`MODE_MOUSE_UTF8`/`MODE_MOUSE_SGR`, tmux.h), not mutually exclusive with
/// each other or with [`MouseProto`]. [`Grid::mouse_encoding`] resolves both
/// bits to tmux's own forwarding precedence (`input-keys.c`
/// `input_key_get_mouse`): SGR wins if both are set, else UTF-8, else the
/// legacy X10-style default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

/// Which escape sequence last set [`TermState::title`] -- OSC 0/2 (always
/// participates in automatic-rename) or the historical `ESC k <name> ESC \`
/// rename escape (participates only when `allow-rename` is on; see
/// `docs/tmux-reference/windows-and-sessions.md` "allow-rename -- what it
/// actually gates"). Read via [`Grid::title_from_esc_k`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TitleSource {
    Osc,
    EscK,
}

/// Pre-scan state for the historical `ESC k <name> (ESC \ | ESC)` rename
/// escape (SP7 Task 3). The `vte` crate has no string-capturing path for
/// `ESC k` -- in its `Escape` state, byte `k` falls in the generic
/// `0x60..=0x7e -> (Ground, EscDispatch)` bucket, so `esc_dispatch` fires
/// once and every subsequent title byte would `Print`-leak into the pane's
/// visible cells. `Grid::feed` therefore strips this sequence out of the
/// raw byte stream BEFORE it ever reaches `vte::Parser::advance`, committing
/// the captured title into the same slot `osc_dispatch` writes. This state
/// persists on `Grid` (not `TermState`) across `feed` calls so a sequence
/// split across chunk boundaries -- including a lone trailing `ESC` -- is
/// still captured correctly.
///
/// Verified against tmux's real state machine (`input.c`
/// `input_state_rename_string_table` reached via the `esc_enter` table's
/// `{0x6b,0x6b,NULL,&input_state_rename_string}` transition): the rename is
/// actually committed by `input_exit_rename`, the STATE's `exit` callback,
/// which `input_set_state` invokes the INSTANT the state changes away from
/// `rename_string` -- i.e. on the bare `ESC` byte itself, before the
/// following byte (the expected `\`) is even read. `PostTitle` below
/// reproduces this: the title is committed when `ESC` is seen, and the
/// following byte is only checked to decide among three outcomes: swallow
/// it as the completing `\` of the ST; re-enter `Title` if it's `k` (the
/// committing `ESC` was doing double duty as the opener of a SECOND,
/// back-to-back `ESC k` -- the realistic pattern under conhost, which eats
/// `ESC \`, per follow-up #52; SP7-B critical fix); or replay it (plus the
/// ESC) as ordinary input for anything else. Also per that same table:
/// `BEL` (0x07) inside the title is mapped to a no-op (0x00-0x17 ->
/// `NULL,NULL`), unlike OSC's dedicated
/// `{0x07,0x07,input_end_bel,&input_state_ground}` arm -- so BEL is
/// dropped, never accumulated and never a terminator, while inside a title.
enum EscKScan {
    Idle,
    Esc,
    Title(Vec<u8>),
    PostTitle,
}

#[derive(Clone, Copy)]
struct SavedCursor {
    col: u16,
    row: u16,
    style: Style,
    autowrap: bool,
}

/// One captured scrollback line: cells at the width in effect when last
/// (re)captured, plus whether it soft-wraps onto the line below it (tmux's
/// `GRID_LINE_WRAPPED`). See `TermState::row_wrapped` for how the flag is
/// set/cleared, and `TermState::reflow_to_width` for how chains of these are
/// rejoined/re-split on a column-width resize.
#[derive(Clone)]
struct HistLine {
    cells: Vec<Cell>,
    wrapped: bool,
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
    /// True while the alternate screen (`CSI ?1049h`) is active.
    alt_screen: bool,
    /// The primary screen's cells + cursor state (position, SGR pen,
    /// autowrap -- DECSC/DECRC scope, per xterm's documentation of 1049),
    /// saved on entering the alt screen and restored on leaving it.
    /// `None` when not in alt-screen mode. The saved primary's own
    /// per-row wrapped flags travel alongside it (second tuple element) so
    /// a later leave-alt restores a primary screen with correct wrap chains.
    saved_primary: Option<(Vec<Cell>, Vec<bool>, SavedCursor)>,
    /// Scrollback: oldest line at the front. Each line is exactly `cols`
    /// wide AT THE CURRENT WIDTH -- a column-width resize reflows every
    /// history line (and the live screen) to the new width via
    /// `TermState::reflow_to_width`, so lines are never stale-width between
    /// reflows; row-count-only resizes leave history untouched (see
    /// `resize`).
    history: VecDeque<HistLine>,
    /// 0 = scrollback disabled (nothing is ever captured).
    history_limit: u32,
    /// Per-live-screen-row soft-wrap flag, parallel to `cells` (indexed by
    /// row): `true` iff this row's content continues onto the row below it
    /// with no real newline in between (tmux's `GRID_LINE_WRAPPED`). Set
    /// only at the instant the cursor auto-wraps off the right margin
    /// (`Perform::print`, consuming `wrap_pending`); cleared on an explicit
    /// linefeed (`Perform::execute` on `0x0A`). Reflow (`reflow_to_width`)
    /// walks these chains to rejoin/re-split logical lines at a new width.
    row_wrapped: Vec<bool>,
    /// Monotonic count of lines EVER pushed into scrollback (never
    /// decremented by eviction) — the stable "lines-ever-captured"
    /// coordinate system copy-mode selection anchors are pinned to (Task 3
    /// review fix): `history_len()` alone can't measure how far content has
    /// shifted between two moments, because chunked eviction lowers it
    /// without moving any surviving line's view position.
    history_total: u64,
    /// Pane title captured from OSC 0/2 or `ESC k` (see [`TitleSource`]), if
    /// any has ever been set.
    title: Option<String>,
    /// Edge-triggered flag: set whenever `title` changes, cleared by
    /// `Grid::take_title_changed`.
    title_changed: bool,
    /// Which escape sequence last set `title` -- read via
    /// `Grid::title_from_esc_k` (SP7 Task 3).
    title_source: TitleSource,
    /// Mouse reporting protocol requested by the pane app via DECSET/DECRST
    /// 9/1000/1002/1003 (SP7 Task 3; `MouseProto` doc comment has the tmux
    /// mutual-exclusion ruling).
    mouse_proto: MouseProto,
    /// DECSET/DECRST 1005 (UTF-8 mouse coordinate encoding) -- independent
    /// of `mouse_sgr` and `mouse_proto` (SP7 Task 3).
    mouse_utf8: bool,
    /// DECSET/DECRST 1006 (SGR mouse coordinate encoding) -- independent of
    /// `mouse_utf8` and `mouse_proto` (SP7 Task 3).
    mouse_sgr: bool,
    /// Edge-triggered: set by a BEL (`\x07`) byte in `execute`, cleared by
    /// `Grid::take_bell` (SP7 Task 3; `Task 17` consumes this for bell
    /// alerts).
    bell: bool,
}

impl TermState {
    fn new(cols: u16, rows: u16, history_limit: u32) -> Self {
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
            alt_screen: false,
            saved_primary: None,
            history: VecDeque::new(),
            history_limit,
            row_wrapped: vec![false; rows as usize],
            history_total: 0,
            title: None,
            title_changed: false,
            title_source: TitleSource::Osc,
            mouse_proto: MouseProto::Off,
            mouse_utf8: false,
            mouse_sgr: false,
            bell: false,
        }
    }

    /// Commit a captured `ESC k <name>` rename-escape title into the same
    /// slot `osc_dispatch` writes (SP7 Task 3). Cleaning matches
    /// `osc_dispatch` exactly: UTF-8 (lossy), control characters stripped,
    /// capped at 256 chars. Always marks `title_changed`, same as OSC (the
    /// gate on whether this actually renames anything is the server's job,
    /// keyed off `title_source`).
    fn set_title_from_esc_k(&mut self, raw: &[u8]) {
        self.title = Some(clean_title(raw));
        self.title_source = TitleSource::EscK;
        self.title_changed = true;
    }

    fn idx(&self, col: u16, row: u16) -> usize {
        row as usize * self.cols as usize + col as usize
    }

    /// Push one scrolled-off line into the scrollback, evicting the oldest
    /// `max(1, history_limit/10)` lines in one chunk once the buffer reaches
    /// capacity (tmux `grid_collect_history`). No-op when scrollback is
    /// disabled (`history_limit == 0`). Degenerate `history_limit == 1`:
    /// every push immediately hits the limit and evicts the line just
    /// pushed, so `history_len()` stays 0 -- effectively disabled.
    fn push_history(&mut self, line: Vec<Cell>, wrapped: bool) {
        if self.history_limit == 0 {
            return;
        }
        self.history_total += 1;
        self.history.push_back(HistLine { cells: line, wrapped });
        if self.history.len() as u32 >= self.history_limit {
            let chunk = (self.history_limit / 10).max(1) as usize;
            for _ in 0..chunk.min(self.history.len()) {
                self.history.pop_front();
            }
        }
    }

    /// View-coordinate cell lookup: `scroll_back` lines scrolled up from the
    /// live bottom (0 = live screen), clamped to `history_len()`.
    /// Out-of-range `row`/`col` (against the CURRENT dimensions -- so a
    /// history line captured wider than the current width is clipped to it,
    /// and columns beyond a narrower captured line read as blank) returns a
    /// blank default cell.
    fn view_cell(&self, scroll_back: u32, col: u16, row: u16) -> Cell {
        if row >= self.rows || col >= self.cols {
            return Cell::default();
        }
        let history_len = self.history.len() as u32;
        let scroll_back = scroll_back.min(history_len);
        // Combined buffer = history (oldest first) followed by the live
        // screen; `combined_index` is where view row `row` lands in it.
        let combined_index = history_len - scroll_back + row as u32;
        if combined_index < history_len {
            self.history[combined_index as usize]
                .cells
                .get(col as usize)
                .copied()
                .unwrap_or_default()
        } else {
            let live_row = (combined_index - history_len) as u16;
            self.cells[self.idx(col, live_row)]
        }
    }

    /// Scroll the region [scroll_top, scroll_bottom] up by `n`, blanking the
    /// vacated bottom rows. Lines pushed off the top are captured into
    /// scrollback, but ONLY when the region is the FULL screen
    /// (`scroll_top == 0 && scroll_bottom == rows - 1` -- covering both
    /// LF-at-bottom via `line_feed` and `CSI S` with no DECSTBM region set;
    /// tmux never captures partial-region scrolls, even top-anchored ones)
    /// and the grid is not currently showing the alt screen.
    fn scroll_up(&mut self, n: u16) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;
        let cols = self.cols as usize;
        let n = n as usize;
        let full_screen = top == 0 && bottom == self.rows as usize - 1;
        if full_screen && !self.alt_screen && self.history_limit > 0 {
            let capture_n = n.min(bottom - top + 1);
            for row in 0..capture_n {
                let start = row * cols;
                let line = self.cells[start..start + cols].to_vec();
                let wrapped = self.row_wrapped[row];
                self.push_history(line, wrapped);
            }
        }
        for row in top..=bottom {
            let src = row + n;
            if src <= bottom {
                for col in 0..cols {
                    self.cells[row * cols + col] = self.cells[src * cols + col];
                }
                self.row_wrapped[row] = self.row_wrapped[src];
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
                self.row_wrapped[row] = false;
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
                self.row_wrapped[row] = self.row_wrapped[src];
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
                self.row_wrapped[row] = false;
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
                self.row_wrapped[row] = self.row_wrapped[src];
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
                self.row_wrapped[row] = false;
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
                self.row_wrapped[row] = self.row_wrapped[src];
            } else {
                for col in 0..cols {
                    self.cells[row * cols + col] = Cell::default();
                }
                self.row_wrapped[row] = false;
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

    /// Resize the active buffer. On the (non-alt-screen) PRIMARY screen, a
    /// column-WIDTH change reflows scrollback + the live screen to the new
    /// width like tmux >= 1.9 (`reflow_to_width`: long lines wrap into more
    /// rows, soft-wrapped pairs rejoin when there's room); a row-COUNT-only
    /// change keeps the original clip (shrink)/pad (grow) behavior,
    /// preserving the overlapping top-left region and leaving history
    /// untouched. The alternate screen NEVER reflows (tmux clears/redraws
    /// it) -- any resize while `alt_screen` is active keeps the original
    /// clip/pad behavior for both axes, and the saved primary buffer is
    /// ALSO clipped/padded in lockstep, so a subsequent leave-alt restores a
    /// primary screen consistent with the new dimensions.
    fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if self.alt_screen {
            self.cells = resize_cells(&self.cells, self.cols, self.rows, cols, rows);
            self.row_wrapped = resize_row_flags(&self.row_wrapped, self.rows, rows);
            if let Some((primary, primary_wrapped, saved)) = &mut self.saved_primary {
                *primary = resize_cells(primary, self.cols, self.rows, cols, rows);
                *primary_wrapped = resize_row_flags(primary_wrapped, self.rows, rows);
                saved.col = saved.col.min(cols.saturating_sub(1));
                saved.row = saved.row.min(rows.saturating_sub(1));
            }
            self.cols = cols;
            self.rows = rows;
        } else {
            if cols != self.cols {
                self.reflow_to_width(cols);
            }
            if rows != self.rows {
                self.cells = resize_cells(&self.cells, self.cols, self.rows, self.cols, rows);
                self.row_wrapped = resize_row_flags(&self.row_wrapped, self.rows, rows);
                self.rows = rows;
            }
        }
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.wrap_pending = false;
    }

    /// Reflow scrollback + the live screen to a new column width, tmux
    /// (`grid_reflow`) style. Rows are grouped into "logical lines" by
    /// following `row_wrapped`/`HistLine::wrapped` chains: a run of
    /// wrapped=true rows (always fully used at the OLD width -- a row is
    /// only ever marked wrapped at the instant it was filled edge-to-edge
    /// and the cursor auto-wrapped off it) followed by exactly one
    /// wrapped=false terminal row, whose trailing never-written cells are
    /// trimmed. Each logical line's content is then re-split at the new
    /// width into `ceil(len / new_cols)` rows (1 row for an empty line),
    /// every row but the last marked wrapped. Row COUNT (`self.rows`) is
    /// untouched here -- see the `resize` caller for the separate row-count
    /// axis.
    ///
    /// Cursor mapping follows tmux's `grid_wrap_position`/
    /// `grid_unwrap_position`: the cursor's offset within its OWN logical
    /// line is preserved; a cursor sitting past that row's real content (in
    /// trailing blank padding -- e.g. after `CUP` into blank space)
    /// collapses to "end of the logical line", matching tmux's own
    /// `UINT_MAX` sentinel. If the mapped position ends up scrolled into
    /// history (only possible when eviction drops the cursor's own line),
    /// the cursor resets to (0, 0) -- again matching tmux
    /// (`screen_resize_cursor`).
    fn reflow_to_width(&mut self, new_cols: u16) {
        let old_cols = self.cols as usize;
        let old_rows = self.rows as usize;
        let new_cols_usz = (new_cols as usize).max(1);

        // 1. Combined physical-row source: history (oldest first), then the
        //    live screen, each paired with its wrapped flag.
        let mut phys: Vec<(Vec<Cell>, bool)> = Vec::with_capacity(self.history.len() + old_rows);
        for h in &self.history {
            phys.push((h.cells.clone(), h.wrapped));
        }
        for r in 0..old_rows {
            let start = r * old_cols;
            phys.push((self.cells[start..start + old_cols].to_vec(), self.row_wrapped[r]));
        }

        // 2. Group into logical lines: concatenate a wrapped=true chain,
        //    trimming only the final (wrapped=false) row of each chain.
        struct Logical {
            content: Vec<Cell>,
            phys_start: usize,
            phys_end: usize, // exclusive
        }
        let mut logicals: Vec<Logical> = Vec::new();
        let mut i = 0;
        while i < phys.len() {
            let start = i;
            let mut content: Vec<Cell> = Vec::new();
            loop {
                let (row, wrapped) = &phys[i];
                if *wrapped {
                    content.extend_from_slice(row);
                } else {
                    let used = trimmed_len(row);
                    content.extend_from_slice(&row[..used]);
                }
                let was_wrapped = *wrapped;
                i += 1;
                if !was_wrapped || i >= phys.len() {
                    break;
                }
            }
            logicals.push(Logical { content, phys_start: start, phys_end: i });
        }

        // 3. Locate the cursor's logical line + its offset within it.
        let cursor_abs_row = self.history.len() + self.cursor_row as usize;
        let cursor_li = logicals
            .iter()
            .position(|l| cursor_abs_row >= l.phys_start && cursor_abs_row < l.phys_end)
            .unwrap_or(logicals.len() - 1);
        let cursor_line_len = logicals[cursor_li].content.len();
        let cursor_ax = {
            let l = &logicals[cursor_li];
            let offset_rows = cursor_abs_row - l.phys_start;
            let is_terminal_row = cursor_abs_row + 1 == l.phys_end;
            let accumulated = offset_rows * old_cols;
            let row_len =
                if is_terminal_row { trimmed_len(&phys[cursor_abs_row].0) } else { old_cols };
            let cursor_col = self.cursor_col as usize;
            if cursor_col >= row_len {
                cursor_line_len
            } else {
                accumulated + cursor_col
            }
        };

        // 4. Re-split every logical line's content at the new width,
        //    recording the cursor's new absolute (row, col) along the way.
        let mut new_phys: Vec<(Vec<Cell>, bool)> = Vec::new();
        let mut new_cursor_row_abs = 0usize;
        let mut new_cursor_col = 0u16;
        for (li, l) in logicals.iter().enumerate() {
            let len = l.content.len();
            let num_rows = if len == 0 { 1 } else { 1 + (len - 1) / new_cols_usz };
            let base = new_phys.len();
            for r in 0..num_rows {
                let s = r * new_cols_usz;
                let e = (s + new_cols_usz).min(len);
                let mut row = vec![Cell::default(); new_cols_usz];
                row[..e - s].copy_from_slice(&l.content[s..e]);
                new_phys.push((row, r + 1 < num_rows));
            }
            if li == cursor_li {
                let idx = cursor_ax.min(len);
                let mut row_local = idx / new_cols_usz;
                let mut col_local = idx % new_cols_usz;
                // Exact multiple of the new width: land on the LAST column
                // of the last existing row rather than the first column of
                // a row that doesn't exist.
                if col_local == 0 && idx == len && len > 0 && row_local > 0 {
                    row_local -= 1;
                    col_local = new_cols_usz - 1;
                }
                row_local = row_local.min(num_rows - 1);
                new_cursor_row_abs = base + row_local;
                new_cursor_col = col_local as u16;
            }
        }

        // 5. Pad with blank rows at the tail if reflow produced fewer total
        //    rows than the screen needs (tmux: `grid_reflow_add` at the end).
        while new_phys.len() < old_rows {
            new_phys.push((vec![Cell::default(); new_cols_usz], false));
        }
        let hsize = new_phys.len() - old_rows;
        let (new_history, new_screen) = new_phys.split_at(hsize);

        // 6. Store history (capped at history_limit, keeping the NEWEST
        //    entries; discarded entirely when scrollback is disabled, same
        //    as `push_history`).
        self.history.clear();
        if self.history_limit > 0 {
            let cap = self.history_limit as usize;
            let start = new_history.len().saturating_sub(cap);
            for (cells, wrapped) in &new_history[start..] {
                self.history.push_back(HistLine { cells: cells.clone(), wrapped: *wrapped });
            }
        }

        // 7. Store the live screen + its wrapped flags.
        self.cells = vec![Cell::default(); new_cols_usz * old_rows];
        self.row_wrapped = vec![false; old_rows];
        for (r, (cells, wrapped)) in new_screen.iter().enumerate() {
            let start = r * new_cols_usz;
            self.cells[start..start + new_cols_usz].copy_from_slice(cells);
            self.row_wrapped[r] = *wrapped;
        }

        // 8. Map the cursor: if it landed within the visible screen, carry
        //    its position across; if reflow pushed its own line out into
        //    history/eviction, reset to (0, 0) (tmux `screen_resize_cursor`).
        if new_cursor_row_abs >= hsize {
            self.cursor_row = (new_cursor_row_abs - hsize) as u16;
            self.cursor_col = new_cursor_col;
        } else {
            self.cursor_row = 0;
            self.cursor_col = 0;
        }

        self.cols = new_cols;
    }
}

/// Clip (shrink) or pad (grow) a `old_cols`x`old_rows` cell buffer into a new
/// `new_cols`x`new_rows` one, preserving the overlapping top-left region.
/// Shared by `TermState::resize` for both the active buffer and (while in
/// alt-screen mode) the saved primary buffer.
fn resize_cells(old: &[Cell], old_cols: u16, old_rows: u16, new_cols: u16, new_rows: u16) -> Vec<Cell> {
    let mut new_cells = vec![Cell::default(); new_cols as usize * new_rows as usize];
    let copy_cols = new_cols.min(old_cols) as usize;
    let copy_rows = new_rows.min(old_rows) as usize;
    for r in 0..copy_rows {
        for c in 0..copy_cols {
            new_cells[r * new_cols as usize + c] = old[r * old_cols as usize + c];
        }
    }
    new_cells
}

/// Crop/pad a per-row boolean flag vector -- the `resize_cells` analog for
/// `row_wrapped`/a saved primary's wrapped flags (one bool per row, not
/// `cols` cells per row). Used only on the row-COUNT axis and for
/// alt-screen resizes (the wrapped flags of newly-added rows are `false`);
/// width-driven changes go through `reflow_to_width` instead.
fn resize_row_flags(old: &[bool], old_rows: u16, new_rows: u16) -> Vec<bool> {
    let mut v = vec![false; new_rows as usize];
    let copy_rows = new_rows.min(old_rows) as usize;
    v[..copy_rows].copy_from_slice(&old[..copy_rows]);
    v
}

/// Length of `row` with trailing default (never-written) cells trimmed off
/// -- lets reflow tell real printed content on a logical line's terminal
/// row apart from unwritten padding. Non-terminal (wrapped) rows are never
/// trimmed: by construction they were filled edge-to-edge before the cursor
/// auto-wrapped off them.
fn trimmed_len(row: &[Cell]) -> usize {
    let mut n = row.len();
    while n > 0 && row[n - 1] == Cell::default() {
        n -= 1;
    }
    n
}

/// Shared title-cleaning rule for both the OSC 0/2 and `ESC k` capture
/// paths: UTF-8 (lossy), control characters stripped, capped at 256 chars.
fn clean_title(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.chars().filter(|c| !c.is_control()).take(256).collect()
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
            // The row we're leaving soft-wraps onto the row we're about to
            // land on (tmux `GRID_LINE_WRAPPED`) -- mark it BEFORE
            // `line_feed` moves `cursor_row` off it.
            self.row_wrapped[self.cursor_row as usize] = true;
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
                // LF: an explicit (hard) newline is never a soft wrap --
                // clear the outgoing row's wrapped flag even if it was
                // (stale-)true, so reflow never joins it to the next row.
                self.wrap_pending = false;
                self.row_wrapped[self.cursor_row as usize] = false;
                self.line_feed();
            }
            0x0D => {
                // CR
                self.wrap_pending = false;
                self.cursor_col = 0;
            }
            0x07 => {
                // BEL: never printed -- just an edge-triggered flag (SP7
                // Task 3) surfaced via `Grid::take_bell` for a later alerts
                // task. Does not affect cursor/wrap state.
                self.bell = true;
            }
            // All other C0 are ignored.
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}

    /// OSC 0 (icon+title) and OSC 2 (title) capture the pane title: UTF-8
    /// (lossy), control characters stripped, capped at 256 chars. OSC 1
    /// (icon-only) and any other OSC are ignored. The terminator (BEL vs
    /// `ESC \`) makes no difference here -- `vte` already normalizes both
    /// into this single callback. `vte` splits the OSC buffer on EVERY
    /// `;`, so a title containing semicolons arrives as params[1..N] and
    /// must be re-joined with `;` (tmux and vte's own ansi.rs reference
    /// consumer both do this), not truncated at params[1].
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.len() < 2 {
            return;
        }
        if params[0] != b"0" && params[0] != b"2" {
            return;
        }
        let joined: Vec<u8> = params[1..].join(&b';');
        self.title = Some(clean_title(&joined));
        self.title_source = TitleSource::Osc;
        self.title_changed = true;
    }

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
                            if set {
                                // Enter: save the primary screen (cells +
                                // cursor position, SGR pen, and autowrap --
                                // DECSC/DECRC scope per xterm's docs for
                                // 1049) the FIRST time only -- a redundant
                                // ?1049h while already in alt mode must not
                                // clobber the saved primary with alt-screen
                                // content. Either way, entering always
                                // clears the (now-active) alt buffer and
                                // homes the cursor (visible behavior
                                // preserved from the MVP).
                                if !self.alt_screen {
                                    self.saved_primary = Some((
                                        self.cells.clone(),
                                        self.row_wrapped.clone(),
                                        SavedCursor {
                                            col: self.cursor_col,
                                            row: self.cursor_row,
                                            style: self.style,
                                            autowrap: self.autowrap,
                                        },
                                    ));
                                    self.alt_screen = true;
                                }
                                self.erase_display(2);
                                self.row_wrapped = vec![false; self.rows as usize];
                                self.cursor_col = 0;
                                self.cursor_row = 0;
                                self.wrap_pending = false;
                            } else if self.alt_screen {
                                // Leave: restore the primary screen exactly
                                // (cells + cursor position/pen/autowrap),
                                // no clearing. A spurious ?1049l while not
                                // in alt mode is a no-op.
                                if let Some((primary, primary_wrapped, saved)) =
                                    self.saved_primary.take()
                                {
                                    self.cells = primary;
                                    self.row_wrapped = primary_wrapped;
                                    self.cursor_col = saved.col.min(self.cols.saturating_sub(1));
                                    self.cursor_row = saved.row.min(self.rows.saturating_sub(1));
                                    self.style = saved.style;
                                    self.autowrap = saved.autowrap;
                                }
                                self.alt_screen = false;
                                self.wrap_pending = false;
                            }
                        }
                        // Mouse reporting protocol (SP7 Task 3): 9/1000/
                        // 1002/1003 are mutually exclusive in real tmux --
                        // SET unconditionally overwrites to the new mode,
                        // RESET of any of the four unconditionally clears
                        // to `Off` (see `MouseProto`'s doc comment for the
                        // `input.c` source citation).
                        Some(9) => {
                            self.mouse_proto = if set { MouseProto::X10 } else { MouseProto::Off };
                        }
                        Some(1000) => {
                            self.mouse_proto =
                                if set { MouseProto::Normal } else { MouseProto::Off };
                        }
                        Some(1002) => {
                            self.mouse_proto =
                                if set { MouseProto::Button } else { MouseProto::Off };
                        }
                        Some(1003) => {
                            self.mouse_proto = if set { MouseProto::Any } else { MouseProto::Off };
                        }
                        // Mouse coordinate encoding (SP7 Task 3): 1005/1006
                        // are independent bits, both of each other and of
                        // the protocol mode above.
                        Some(1005) => self.mouse_utf8 = set,
                        Some(1006) => self.mouse_sgr = set,
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
    /// Cross-`feed`-call pre-scan state for the `ESC k` rename escape (SP7
    /// Task 3) -- see [`EscKScan`]'s doc comment.
    esck_scan: EscKScan,
}

impl Grid {
    /// Create a grid. Dimensions are clamped to a 1x1 minimum: a grid is
    /// never zero-sized. `history_limit` caps the scrollback line count;
    /// 0 disables scrollback entirely (nothing is ever captured).
    pub fn new(cols: u16, rows: u16, history_limit: u32) -> Self {
        Grid {
            parser: Parser::new(),
            state: TermState::new(cols, rows, history_limit),
            esck_scan: EscKScan::Idle,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let filtered = self.strip_esc_k(bytes);
        for b in filtered {
            self.parser.advance(&mut self.state, b);
        }
    }

    /// Pre-scan `bytes` for the historical `ESC k <name> (ESC \ | ESC)`
    /// rename escape and strip it out, returning everything else unchanged
    /// (byte-for-byte, in order) for `vte::Parser::advance`. See
    /// [`EscKScan`]'s doc comment for why this must happen before `vte` ever
    /// sees these bytes, and for the exact tmux-verified terminator rules
    /// this reproduces (title commits on the bare `ESC`; `BEL` is dropped,
    /// not a terminator, while inside a title).
    fn strip_esc_k(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            match std::mem::replace(&mut self.esck_scan, EscKScan::Idle) {
                EscKScan::Idle => {
                    if b == 0x1b {
                        self.esck_scan = EscKScan::Esc;
                    } else {
                        out.push(b);
                    }
                }
                EscKScan::Esc => {
                    if b == b'k' {
                        self.esck_scan = EscKScan::Title(Vec::new());
                    } else {
                        // Not `ESC k` -- replay both bytes untouched; state
                        // is already back to `Idle` via the `mem::replace`
                        // default above.
                        out.push(0x1b);
                        out.push(b);
                    }
                }
                EscKScan::Title(mut buf) => {
                    if b == 0x1b {
                        // Commit NOW, on the bare ESC -- matches tmux's
                        // `input_exit_rename` firing as the `rename_string`
                        // state's `exit` callback the instant the state
                        // changes away, before the following byte (the
                        // expected `\`) is even read.
                        self.state.set_title_from_esc_k(&buf);
                        self.esck_scan = EscKScan::PostTitle;
                    } else if b == 0x07 {
                        // BEL inside a title: dropped, not accumulated, not
                        // a terminator (tmux's rename_string state table has
                        // no BEL arm, unlike OSC's).
                        self.esck_scan = EscKScan::Title(buf);
                    } else {
                        buf.push(b);
                        self.esck_scan = EscKScan::Title(buf);
                    }
                }
                EscKScan::PostTitle => {
                    if b == b'\\' {
                        // ST fully consumed; the title already committed.
                        self.esck_scan = EscKScan::Idle;
                    } else if b == b'k' {
                        // The committing ESC was doing double duty: it also
                        // opens a SECOND `ESC k` capture (back-to-back
                        // unterminated titles -- the realistic pattern under
                        // conhost, which eats `ESC \`, per follow-up #52).
                        // Start capturing the next title instead of
                        // replaying `k...` as ordinary input.
                        self.esck_scan = EscKScan::Title(Vec::new());
                    } else if b == 0x1b {
                        // Another bare ESC instead of the expected `\` --
                        // keep waiting rather than replaying (the title is
                        // already committed either way; this only affects
                        // whether a stray non-`\` byte here leaks through).
                        self.esck_scan = EscKScan::PostTitle;
                    } else {
                        // Not `ESC \` (nor a new `ESC k`) after all --
                        // replay the consumed ESC plus this byte as
                        // ordinary input.
                        out.push(0x1b);
                        out.push(b);
                    }
                }
            }
        }
        out
    }

    /// Number of scrollback lines currently captured (<= the `history_limit`
    /// passed to `new`).
    pub fn history_len(&self) -> u32 {
        self.state.history.len() as u32
    }

    /// Monotonic count of lines EVER captured into scrollback — never
    /// decremented by eviction (unlike `history_len()`). The difference
    /// between two `history_total()` readings is exactly how many view rows
    /// the pane's content has shifted up between them (each capture shifts
    /// the view by one; eviction shifts nothing) — the coordinate system
    /// copy-mode selection anchors are pinned to (Task 3 review fix; see
    /// the `## grid-v2` contract amendment).
    ///
    /// **Caveat (SP7 review fix):** this invariant holds only across
    /// mutations that do NOT change the grid's WIDTH. `reflow_to_width`
    /// (called from `resize` on a column-width change) rewrites the
    /// combined history+screen buffer by re-splitting logical lines at the
    /// new width — a non-uniform restructuring — and does NOT go through
    /// `push_history`, so it never bumps `history_total` to match. A
    /// width-changing resize can therefore make `history_total`'s delta
    /// undercount (or simply not correspond to) how far content actually
    /// moved, and there is no single corrected shift count that could
    /// repair a coordinate pinned before such a resize (the reflow can
    /// split/merge lines, not just shift them). Consumers pinning
    /// coordinates across grid mutations (copy-mode selection anchors) must
    /// treat a width-changing resize of the bound pane as invalidating any
    /// stored anchor, not just re-shift it — see
    /// `Server::apply_layout_for_session` in `src/server.rs`, which clears
    /// a client's active copy-mode selection whenever its pane is actually
    /// resized (matching real tmux's `window_copy_size_changed`, which
    /// unconditionally clears the copy-mode selection on ANY resize of the
    /// pane, width or height).
    pub fn history_total(&self) -> u64 {
        self.state.history_total
    }

    /// Look up a cell in view coordinates: `scroll_back` lines scrolled up
    /// from the live bottom (0 = live screen, clamped to `history_len()`).
    /// Out-of-range `row`/`col` returns a blank default-style cell.
    pub fn view_cell(&self, scroll_back: u32, col: u16, row: u16) -> Cell {
        self.state.view_cell(scroll_back, col, row)
    }

    /// Convenience: collect a whole view row into a `String` (e.g. for
    /// copy-mode search).
    pub fn view_row_text(&self, scroll_back: u32, row: u16) -> String {
        (0..self.cols()).map(|c| self.view_cell(scroll_back, c, row).ch).collect()
    }

    /// The pane's title as last captured via OSC 0/2, if any.
    pub fn title(&self) -> Option<&str> {
        self.state.title.as_deref()
    }

    /// Edge-triggered: true the first time this is called after the title
    /// has changed, then false until it changes again.
    pub fn take_title_changed(&mut self) -> bool {
        let changed = self.state.title_changed;
        self.state.title_changed = false;
        changed
    }

    /// `true` if the pane's CURRENT `title()` was last set by the historical
    /// `ESC k <name> ESC \` rename escape rather than OSC 0/2 (SP7 Task 3).
    /// Not edge-triggered -- reflects the source of whatever `title()`
    /// currently holds. Lets the server gate ESC-k-sourced automatic-rename
    /// behind the `allow-rename` option while leaving the OSC 0/2 path
    /// unconditional, matching real tmux (`allow-rename` gates ONLY
    /// `ESC k` -- see `docs/tmux-reference/windows-and-sessions.md`
    /// "allow-rename -- what it actually gates").
    pub fn title_from_esc_k(&self) -> bool {
        matches!(self.state.title_source, TitleSource::EscK)
    }

    /// The pane app's requested mouse reporting protocol (SP7 Task 3), or
    /// `MouseProto::Off` if it has never sent a mouse-mode DECSET. See
    /// [`MouseProto`]'s doc comment for the tmux-verified mutual-exclusion
    /// rule among 9/1000/1002/1003.
    pub fn mouse_proto(&self) -> MouseProto {
        self.state.mouse_proto
    }

    /// The pane app's requested mouse coordinate encoding (SP7 Task 3):
    /// SGR (1006) wins if both SGR and UTF-8 (1005) are set, else UTF-8,
    /// else the legacy default -- matching tmux's own forwarding precedence
    /// (`input-keys.c` `input_key_get_mouse`; see [`MouseEncoding`]'s doc
    /// comment).
    pub fn mouse_encoding(&self) -> MouseEncoding {
        if self.state.mouse_sgr {
            MouseEncoding::Sgr
        } else if self.state.mouse_utf8 {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::Default
        }
    }

    /// Edge-triggered: true the first time this is called after a BEL
    /// (`\x07`) byte has been fed, then false until another BEL arrives
    /// (SP7 Task 3; a later alerts task consumes this).
    pub fn take_bell(&mut self) -> bool {
        let bell = self.state.bell;
        self.state.bell = false;
        bell
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

    /// `true` while the pane's application has switched to the alternate
    /// screen (`CSI ?1049h`/`?47h`/`?1047h`), `false` on the primary screen.
    /// Mouse wheel routing (Task 5, sub-project 4) uses this to decide
    /// whether a wheel event should scroll winmux's own scrollback/copy-mode
    /// (primary screen) or be translated into synthesized arrow-key presses
    /// sent to the pane (alt screen — tmux's own wheel-in-alt-screen
    /// behavior, since alt-screen apps like `less`/vim have no scrollback of
    /// their own to reveal).
    pub fn alt_screen(&self) -> bool {
        self.state.alt_screen
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
        let mut g = Grid::new(5, 2, 0);
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
        let mut g = Grid::new(5, 2, 0);
        g.feed(b"\x1b[?7lhello!");
        assert_eq!(row_str(&g, 0), "hell!");
        assert_eq!(g.cursor(), (4, 0));
        assert_eq!(row_str(&g, 1), "     ");
    }

    #[test]
    fn backspace_overwrites() {
        // a,b -> BS moves back over b -> c overwrites it.
        let mut g = Grid::new(5, 2, 0);
        g.feed(b"ab\x08c");
        assert_eq!(row_str(&g, 0), "ac   ");
        assert_eq!(g.cursor(), (2, 0));
    }

    #[test]
    fn cr_lf() {
        let mut g = Grid::new(5, 3, 0);
        g.feed(b"abc\r");
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\n");
        assert_eq!(g.cursor(), (0, 1));
        assert_eq!(row_str(&g, 0), "abc  ");
    }

    #[test]
    fn horizontal_tab() {
        // 8-col tab stops, clamped to the last column.
        let mut g = Grid::new(20, 1, 0);
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
        let mut g = Grid::new(3, 2, 0);
        g.feed(b"a\r\nb\r\n");
        assert_eq!(row_str(&g, 0), "b  ");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(g.cursor(), (0, 1));
        g.feed(b"c");
        assert_eq!(row_str(&g, 1), "c  ");
    }

    #[test]
    fn cursor_movement() {
        let mut g = Grid::new(10, 5, 0);
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
        let mut g = Grid::new(10, 5, 0);
        g.feed(b"\x1b[H");     // home
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\x1b[2;3f");  // HVP row2 col3 -> (2,1)
        assert_eq!(g.cursor(), (2, 1));
    }

    #[test]
    fn cnl_cpl_cha() {
        let mut g = Grid::new(10, 5, 0);
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
        let mut g = Grid::new(3, 3, 0);
        g.feed(b"xxxxxxxxx");        // fills 3x3 via autowrap
        g.feed(b"\x1b[2;2H\x1b[0J"); // cursor (1,1); clear to end
        assert_eq!(row_str(&g, 0), "xxx");
        assert_eq!(row_str(&g, 1), "x  ");
        assert_eq!(row_str(&g, 2), "   ");
    }

    #[test]
    fn erase_display_above() {
        let mut g = Grid::new(3, 3, 0);
        g.feed(b"xxxxxxxxx");
        g.feed(b"\x1b[2;2H\x1b[1J"); // clear start..=cursor
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(row_str(&g, 1), "  x");
        assert_eq!(row_str(&g, 2), "xxx");
    }

    #[test]
    fn erase_display_all() {
        let mut g = Grid::new(3, 3, 0);
        g.feed(b"xxxxxxxxx");
        g.feed(b"\x1b[2J");
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(row_str(&g, 2), "   ");
    }

    #[test]
    fn erase_line_right() {
        let mut g = Grid::new(5, 2, 0);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;3H\x1b[0K"); // cursor col3(0-based 2); clear to eol
        assert_eq!(row_str(&g, 0), "ab   ");
    }

    #[test]
    fn erase_line_left() {
        let mut g = Grid::new(5, 2, 0);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;3H\x1b[1K"); // clear col0..=col2
        assert_eq!(row_str(&g, 0), "   de");
    }

    #[test]
    fn erase_line_all() {
        let mut g = Grid::new(5, 2, 0);
        g.feed(b"abcde");
        g.feed(b"\x1b[2K");
        assert_eq!(row_str(&g, 0), "     ");
    }

    #[test]
    fn sgr_basic() {
        let mut g = Grid::new(5, 1, 0);
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
        let mut g = Grid::new(5, 1, 0);
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
        let g = Grid::new(80, 24, 0);
        g.cell(90, 5);
    }

    #[test]
    fn zero_size_new_clamps_to_1x1() {
        let mut g = Grid::new(0, 0, 0);
        assert_eq!(g.cols(), 1);
        assert_eq!(g.rows(), 1);
        g.feed(b"\x1b[5;5Hx"); // must not panic
        assert_eq!(g.cell(0, 0).ch, 'x');
    }

    #[test]
    fn zero_size_resize_clamps_to_1x1() {
        let mut g = Grid::new(5, 5, 0);
        g.resize(0, 5);
        assert_eq!(g.cols(), 1);
        assert_eq!(g.rows(), 5);
        g.feed(b"\x1b[2;2HX\x1b[1;1C"); // must not panic
    }

    #[test]
    fn dectcem_visibility() {
        let mut g = Grid::new(5, 1, 0);
        assert!(g.cursor_visible());
        g.feed(b"\x1b[?25l");
        assert!(!g.cursor_visible());
        g.feed(b"\x1b[?25h");
        assert!(g.cursor_visible());
    }

    #[test]
    fn insert_chars() {
        // "abcde", cursor at col1, ICH 2: 'a' | 2 blanks | 'b','c' (d,e drop off)
        let mut g = Grid::new(5, 1, 0);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;2H\x1b[2@");
        assert_eq!(row_str(&g, 0), "a  bc");
    }

    #[test]
    fn delete_chars() {
        // "abcde", cursor at col1, DCH 2: 'a' + shift 'd','e' left, blanks fill
        let mut g = Grid::new(5, 1, 0);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;2H\x1b[2P");
        assert_eq!(row_str(&g, 0), "ade  ");
    }

    #[test]
    fn erase_chars() {
        // "abcde", cursor at col1, ECH 2: blank col1,col2 in place (no shift)
        let mut g = Grid::new(5, 1, 0);
        g.feed(b"abcde");
        g.feed(b"\x1b[1;2H\x1b[2X");
        assert_eq!(row_str(&g, 0), "a  de");
    }

    #[test]
    fn insert_lines() {
        let mut g = Grid::new(3, 4, 0);
        g.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        g.feed(b"\x1b[2;1H\x1b[L"); // cursor row1 (0-based); insert 1 blank line
        assert_eq!(row_str(&g, 0), "aaa");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(row_str(&g, 2), "bbb");
        assert_eq!(row_str(&g, 3), "ccc");
    }

    #[test]
    fn delete_lines() {
        let mut g = Grid::new(3, 4, 0);
        g.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
        g.feed(b"\x1b[2;1H\x1b[M"); // cursor row1; delete 1 line
        assert_eq!(row_str(&g, 0), "aaa");
        assert_eq!(row_str(&g, 1), "ccc");
        assert_eq!(row_str(&g, 2), "ddd");
        assert_eq!(row_str(&g, 3), "   ");
    }

    #[test]
    fn scroll_up_su() {
        let mut g = Grid::new(3, 3, 0);
        g.feed(b"aaa\r\nbbb\r\nccc");
        g.feed(b"\x1b[S");
        assert_eq!(row_str(&g, 0), "bbb");
        assert_eq!(row_str(&g, 1), "ccc");
        assert_eq!(row_str(&g, 2), "   ");
    }

    #[test]
    fn scroll_down_sd() {
        let mut g = Grid::new(3, 3, 0);
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
        let mut g = Grid::new(3, 4, 0);
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
        let mut g = Grid::new(3, 4, 0);
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
        let mut g = Grid::new(10, 5, 0);
        g.feed(b"\x1b[3;4H\x1b7\x1b[H"); // to (3,2), save, home
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\x1b8");
        assert_eq!(g.cursor(), (3, 2));
    }

    #[test]
    fn save_restore_cursor_csi() {
        let mut g = Grid::new(10, 5, 0);
        g.feed(b"\x1b[3;4H\x1b[s\x1b[H");
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"\x1b[u");
        assert_eq!(g.cursor(), (3, 2));
    }

    #[test]
    fn sgr_extended_colors() {
        let mut g = Grid::new(5, 1, 0);
        g.feed(b"\x1b[38;5;196mA");        // 256-color fg
        assert_eq!(g.cell(0, 0).style.fg, Color::Idx(196));
        g.feed(b"\x1b[48;2;10;20;30mB");   // truecolor bg
        assert_eq!(g.cell(1, 0).style.bg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn sgr_bright_and_reset_attrs() {
        let mut g = Grid::new(5, 1, 0);
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
        let mut g = Grid::new(5, 1, 0);
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
        let mut g = Grid::new(3, 4, 0);
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
        // Real alt-screen save/restore (SP4): entering still clears + homes
        // (visible behavior preserved from the MVP); leaving now RESTORES
        // the primary screen's content and cursor exactly, rather than
        // clearing it a second time.
        let mut g = Grid::new(3, 3, 0);
        g.feed(b"xxxxxxxxx");
        g.feed(b"\x1b[2;2H"); // primary cursor -> (1,1) before entering alt
        g.feed(b"\x1b[?1049h");
        assert_eq!(row_str(&g, 0), "   ");
        assert_eq!(row_str(&g, 1), "   ");
        assert_eq!(row_str(&g, 2), "   ");
        assert_eq!(g.cursor(), (0, 0));
        g.feed(b"yyy");
        g.feed(b"\x1b[?1049l"); // leave: primary restored exactly, not cleared
        assert_eq!(row_str(&g, 0), "xxx");
        assert_eq!(row_str(&g, 1), "xxx");
        assert_eq!(row_str(&g, 2), "xxx");
        assert_eq!(g.cursor(), (1, 1));
    }

    #[test]
    fn alt_screen_getter_tracks_mode() {
        let mut g = Grid::new(3, 3, 0);
        assert!(!g.alt_screen());
        g.feed(b"\x1b[?1049h");
        assert!(g.alt_screen());
        g.feed(b"\x1b[?1049l");
        assert!(!g.alt_screen());
    }

    #[test]
    fn osc_title_captured() {
        let mut g = Grid::new(5, 1, 0);
        assert_eq!(g.title(), None);
        g.feed(b"\x1b]0;hello\x07"); // OSC 0 (icon+title), BEL-terminated
        assert_eq!(g.title(), Some("hello"));
        assert!(g.take_title_changed()); // edge-triggered: true once
        assert!(!g.take_title_changed()); // cleared on read
        g.feed(b"\x1b]2;world\x07"); // OSC 2 (title) also captured
        assert_eq!(g.title(), Some("world"));
        assert!(g.take_title_changed());
    }

    #[test]
    fn osc_title_with_semicolons() {
        // vte splits the OSC buffer on EVERY ';' -- a title containing
        // semicolons arrives as params[1..N] and must be re-joined, not
        // truncated at params[1] (tmux and vte's own ansi.rs reference
        // consumer both reconstruct the full title).
        let mut g = Grid::new(5, 1, 0);
        g.feed(b"\x1b]0;a;b;c\x07");
        assert_eq!(g.title(), Some("a;b;c"));
        assert!(g.take_title_changed());
    }

    #[test]
    fn region_scroll_top_anchored_not_captured() {
        // A top-anchored but PARTIAL scroll region (DECSTBM rows 1-10 on a
        // 24-row grid: top=0, bottom=9 < rows-1) must NOT capture scrolled
        // lines -- tmux only captures full-screen scrolls into history.
        let mut g = Grid::new(3, 24, 10);
        g.feed(b"\x1b[1;10r"); // region indices 0..=9, homes cursor
        g.feed(b"\x1b[10;1H"); // cursor to index (0,9) = region bottom
        g.feed(b"top\r\n"); // LF at region bottom scrolls the region only
        assert_eq!(g.history_len(), 0);
        // CSI S inside the same partial region: also not captured.
        g.feed(b"\x1b[S");
        assert_eq!(g.history_len(), 0);
        // Restoring the full-screen region re-enables capture.
        g.feed(b"\x1b[r\x1b[24;1H\n");
        assert_eq!(g.history_len(), 1);
    }

    #[test]
    fn alt_screen_restores_pen_state() {
        // xterm documents 1049 as save/restore "as in DECSC/DECRC": the SGR
        // pen and autowrap flag must be restored on leave, not leaked from
        // the alt-screen app into the primary screen.
        let mut g = Grid::new(5, 2, 0);
        g.feed(b"\x1b[?1049h");
        g.feed(b"\x1b[31m\x1b[?7l"); // alt app: red fg, autowrap off
        g.feed(b"\x1b[?1049l");
        g.feed(b"X");
        assert_eq!(g.cell(0, 0).style, Style::default()); // pen restored
        g.feed(b"YZAB!"); // 6th char on a 5-col row: wraps only if DECAWM is back on
        assert_eq!(g.cell(0, 1).ch, '!');
    }

    #[test]
    fn osc2_and_st_terminator() {
        // ST (`ESC \`) terminator behaves identically to BEL.
        let mut g = Grid::new(5, 1, 0);
        g.feed(b"\x1b]2;via-st\x1b\\");
        assert_eq!(g.title(), Some("via-st"));
        assert!(g.take_title_changed());
    }

    #[test]
    fn osc_and_unknown_ignored() {
        let mut g = Grid::new(5, 1, 0);
        g.feed(b"\x1b]1;icon only\x07A"); // OSC 1 (icon-only) NOT captured as title
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.title(), None);
        g.feed(b"\x1b[99;99Z"); // unknown CSI final byte -> ignored
        assert_eq!(g.cell(0, 0).ch, 'A');
        assert_eq!(g.cursor(), (1, 0));
    }

    #[test]
    fn decset_1000_sets_normal_mouse_1006_sets_sgr_encoding() {
        let mut g = Grid::new(10, 2, 0);
        assert_eq!(g.mouse_proto(), MouseProto::Off);
        assert_eq!(g.mouse_encoding(), MouseEncoding::Default);
        g.feed(b"\x1b[?1000h");
        assert_eq!(g.mouse_proto(), MouseProto::Normal);
        g.feed(b"\x1b[?1006h");
        assert_eq!(g.mouse_encoding(), MouseEncoding::Sgr);
        // Setting the encoding mode must not disturb the protocol mode.
        assert_eq!(g.mouse_proto(), MouseProto::Normal);
    }

    #[test]
    fn decrst_clears_mouse_mode() {
        let mut g = Grid::new(10, 2, 0);
        g.feed(b"\x1b[?1002h");
        assert_eq!(g.mouse_proto(), MouseProto::Button);
        g.feed(b"\x1b[?1002l");
        assert_eq!(g.mouse_proto(), MouseProto::Off);

        // A DECRST of any of the 4 protocol mode numbers clears unconditionally
        // to Off, even naming a DIFFERENT mode from the one actually active --
        // matches tmux's `input_csi_dispatch_rm_private` (case 1000/1002/1003
        // all clear ALL_MOUSE_MODES regardless of which is set).
        g.feed(b"\x1b[?1003h");
        assert_eq!(g.mouse_proto(), MouseProto::Any);
        g.feed(b"\x1b[?1000l");
        assert_eq!(g.mouse_proto(), MouseProto::Off);

        g.feed(b"\x1b[?1005h");
        assert_eq!(g.mouse_encoding(), MouseEncoding::Utf8);
        g.feed(b"\x1b[?1005l");
        assert_eq!(g.mouse_encoding(), MouseEncoding::Default);
    }

    #[test]
    fn mode_1003_any_motion_wins_over_1000() {
        let mut g = Grid::new(10, 2, 0);
        g.feed(b"\x1b[?1000h");
        assert_eq!(g.mouse_proto(), MouseProto::Normal);
        g.feed(b"\x1b[?1003h");
        assert_eq!(g.mouse_proto(), MouseProto::Any);
        // And the reverse also holds: these 4 modes are mutually exclusive
        // (last SET wins outright), not priority-ordered -- a later 1000
        // supersedes an active 1003 too.
        g.feed(b"\x1b[?1000h");
        assert_eq!(g.mouse_proto(), MouseProto::Normal);
    }

    #[test]
    fn bel_byte_sets_bell_flag_take_bell_clears() {
        let mut g = Grid::new(10, 2, 0);
        assert!(!g.take_bell());
        g.feed(b"abc\x07def");
        assert!(g.take_bell());
        assert!(!g.take_bell()); // edge-triggered: cleared on read
        // BEL must never print as a visible character.
        assert_eq!(row_str(&g, 0), "abcdef    ");
    }

    #[test]
    fn esc_k_sets_title() {
        let mut g = Grid::new(20, 1, 0);
        assert_eq!(g.title(), None);
        g.feed(b"\x1bkmy-title\x1b\\");
        assert_eq!(g.title(), Some("my-title"));
        assert!(g.take_title_changed());
        assert!(g.title_from_esc_k());
    }

    #[test]
    fn esc_k_title_bytes_do_not_leak_into_cells() {
        // The `vte` crate has no string-capturing path for `ESC k` -- without
        // the `Grid::feed` pre-scan/strip, every title byte after the first
        // would `Print`-leak into the pane's visible cells. Prove it doesn't.
        let mut g = Grid::new(20, 2, 0);
        g.feed(b"\x1bkfoo\x1b\\");
        g.feed(b"bar");
        assert_eq!(row_str(&g, 0).trim_end(), "bar");
        assert_eq!(g.title(), Some("foo"));
    }

    #[test]
    fn esc_k_split_across_feed_chunks() {
        // The sequence -- including the ESC/backslash terminator itself --
        // arrives split over several `feed` calls; the pre-scan state must
        // persist on `Grid` across calls, and (verified against tmux's
        // `input_exit_rename` firing as the `rename_string` state's `exit`
        // callback) the title commits on the bare ESC, one call before the
        // completing backslash even arrives.
        let mut g = Grid::new(20, 2, 0);
        g.feed(b"\x1bkhel");
        assert_eq!(g.title(), None);
        g.feed(b"lo");
        assert_eq!(g.title(), None);
        g.feed(b"\x1b");
        assert_eq!(g.title(), Some("hello"));
        assert!(g.take_title_changed());
        g.feed(b"\\");
        assert_eq!(g.title(), Some("hello")); // ST consumed; no further change
        assert!(!g.take_title_changed());
        // No title bytes leaked into the visible grid at any point.
        assert_eq!(row_str(&g, 0).trim_end(), "");
    }

    #[test]
    fn back_to_back_esc_k_titles_update_without_leak() {
        // Critical fix (SP7-B review of cae6af2): when the ESC that commits
        // a title is ALSO the opening ESC of a second `ESC k`, the old
        // `PostTitle` arm replayed the following `k` as ordinary input --
        // leaking `title2` into the visible grid and leaving `title()`
        // stuck on `title1`. The committing ESC must be recognized as
        // double duty: title-terminator AND next-title-opener.
        let mut g = Grid::new(30, 2, 0);
        g.feed(b"\x1bktitle1\x1bktitle2\x1b\\");
        assert_eq!(g.title(), Some("title2"));
        assert_eq!(row_str(&g, 0).trim_end(), "");
        assert_eq!(row_str(&g, 1).trim_end(), "");

        // Full chain: t1 -> t2 -> t3, ending on a real ST. Zero leaked cells
        // and the final title is the last one committed.
        let mut g2 = Grid::new(30, 2, 0);
        g2.feed(b"\x1bkt1\x1bkt2\x1bkt3\x1b\\");
        assert_eq!(g2.title(), Some("t3"));
        assert_eq!(row_str(&g2, 0).trim_end(), "");
        assert_eq!(row_str(&g2, 1).trim_end(), "");
    }

    #[test]
    fn pending_esc_then_non_k_escape_passes_through_intact() {
        // A lone trailing ESC at the end of one `feed` chunk, followed by a
        // CSI sequence (NOT `k`) starting the next chunk, must be replayed
        // byte-for-byte to `vte` rather than swallowed -- proving the
        // `Esc`-state passthrough survives a chunk boundary.
        let mut g = Grid::new(10, 2, 0);
        g.feed(b"ab\x1b");
        g.feed(b"[31mcd");
        assert_eq!(row_str(&g, 0).trim_end(), "abcd");
        assert_eq!(g.title(), None);
    }

    #[test]
    fn literal_k_inside_csi_or_osc_passes_through() {
        // A `k` byte that is NOT immediately preceded by a bare ESC (i.e.
        // it's a parameter/data byte inside some other escape sequence)
        // must never be mistaken for an `ESC k` opener.
        let mut g = Grid::new(20, 2, 0);
        // 'k' as an ordinary printable character after a CSI SGR sequence.
        g.feed(b"\x1b[31mk\x1b[0m");
        assert_eq!(row_str(&g, 0).trim_end(), "k");
        assert_eq!(g.title(), None);

        // 'k' inside an OSC 2 (set-title) string -- handled entirely by
        // `vte`'s own OSC accumulation, not the ESC-k pre-scan, since the
        // OSC's opening ESC is immediately followed by ']' not 'k'.
        let mut g2 = Grid::new(20, 2, 0);
        g2.feed(b"\x1b]2;kite\x07");
        assert_eq!(g2.title(), Some("kite"));
        assert_eq!(row_str(&g2, 0).trim_end(), "");
    }

    #[test]
    fn bel_mid_title_is_silent_noop() {
        // BEL inside an `ESC k` title must neither ring the bell nor leak
        // into the buffer -- tmux's `rename_string` state table has no BEL
        // arm (unlike OSC's dedicated bell-terminator arm).
        let mut g = Grid::new(20, 2, 0);
        assert!(!g.take_bell());
        g.feed(b"\x1bkfoo\x07bar\x1b\\");
        assert_eq!(g.title(), Some("foobar"));
        assert!(!g.take_bell());
        assert_eq!(row_str(&g, 0).trim_end(), "");
    }

    #[test]
    fn scrollback_captures_scrolled_lines() {
        // 3 cols x 2 rows: each of the three CRLFs but the first triggers a
        // full-screen (scroll_top == 0) scroll, capturing the row pushed
        // off the top before it's overwritten.
        let mut g = Grid::new(3, 2, 10);
        g.feed(b"aaa\r\nbbb\r\nccc\r\n");
        assert_eq!(g.history_len(), 2);
        assert_eq!(row_str(&g, 0), "ccc");
        assert_eq!(row_str(&g, 1), "   ");
        // scroll_back 1: one line up from the live bottom shows the state
        // just before the last scroll.
        assert_eq!(g.view_row_text(1, 0), "bbb");
        assert_eq!(g.view_row_text(1, 1), "ccc");
        // scroll_back == history_len: fully scrolled back to the earliest
        // captured state.
        assert_eq!(g.view_row_text(2, 0), "aaa");
        assert_eq!(g.view_row_text(2, 1), "bbb");
    }

    #[test]
    fn scrollback_eviction_chunked() {
        // rows=1 so every LF forces a scroll, capturing exactly one line of
        // history per iteration. history_limit=20 -> eviction chunk =
        // max(1, 20/10) = 2.
        let mut g = Grid::new(3, 1, 20);
        for i in 0..20 {
            let label = format!("{i:03}");
            g.feed(label.as_bytes());
            g.feed(b"\r\n");
        }
        // The 20th push hit the limit and evicted a full chunk of 2 in one
        // go (not just 1) -- len is 18, not 19.
        assert_eq!(g.history_len(), 18);
        // The two oldest lines ("000", "001") are gone; "002" now survives
        // as the oldest entry.
        assert_eq!(g.view_row_text(18, 0), "002");
    }

    /// Task 3 review fix: `history_total()` counts lines EVER captured,
    /// monotonically -- eviction lowers `history_len()` but never
    /// `history_total()`, making the latter a stable coordinate origin for
    /// copy-mode selection anchors.
    #[test]
    fn history_total_monotonic_across_eviction() {
        // Same setup as scrollback_eviction_chunked: 20 captures against
        // limit 20 evict a chunk of 2, so len (18) < total (20).
        let mut g = Grid::new(3, 1, 20);
        assert_eq!(g.history_total(), 0);
        for i in 0..20 {
            g.feed(format!("{i:03}\r\n").as_bytes());
        }
        assert_eq!(g.history_len(), 18);
        assert_eq!(g.history_total(), 20);
        // Further captures keep counting from 20, never reset by eviction.
        g.feed(b"x\r\n");
        assert_eq!(g.history_total(), 21);
        // history_limit == 0 never captures: total stays 0 too.
        let mut off = Grid::new(3, 1, 0);
        off.feed(b"a\r\nb\r\n");
        assert_eq!(off.history_total(), 0);
    }

    #[test]
    fn alt_screen_saves_and_restores_primary() {
        let mut g = Grid::new(4, 2, 0);
        g.feed(b"prim"); // row0 = "prim", fills the row
        g.feed(b"\x1b[2;2H"); // primary cursor -> (1,1)
        g.feed(b"\x1b[?1049h"); // enter alt: cleared + homed
        assert_eq!(row_str(&g, 0), "    ");
        g.feed(b"alt!"); // write into the alt buffer only
        assert_eq!(row_str(&g, 0), "alt!");
        g.feed(b"\x1b[?1049l"); // leave: primary restored exactly, alt content gone
        assert_eq!(row_str(&g, 0), "prim");
        assert_eq!(row_str(&g, 1), "    ");
        assert_eq!(g.cursor(), (1, 1));
    }

    #[test]
    fn alt_screen_no_history() {
        // Scrolling while in the alt screen must never capture scrollback,
        // even though it's a scroll_top == 0 full-screen scroll.
        let mut g = Grid::new(3, 1, 10);
        g.feed(b"\x1b[?1049h");
        for i in 0..5 {
            let label = format!("{i:03}");
            g.feed(label.as_bytes());
            g.feed(b"\r\n");
        }
        assert_eq!(g.history_len(), 0);
        g.feed(b"\x1b[?1049l");
        assert_eq!(g.history_len(), 0);
    }

    #[test]
    fn view_cell_clamps() {
        let mut g = Grid::new(3, 2, 10);
        g.feed(b"aaa\r\nbbb\r\nccc\r\n"); // history_len == 2, see scrollback_captures_scrolled_lines
        // scroll_back beyond history_len clamps to history_len.
        assert_eq!(g.view_row_text(999, 0), g.view_row_text(2, 0));
        assert_eq!(g.view_row_text(999, 1), g.view_row_text(2, 1));
        // Out-of-range row/col -> blank default cell.
        assert_eq!(g.view_cell(0, 0, 99), Cell::default());
        assert_eq!(g.view_cell(0, 99, 0), Cell::default());
    }

    #[test]
    fn history_limit_zero_disables() {
        let mut g = Grid::new(3, 2, 0);
        g.feed(b"aaa\r\nbbb\r\nccc\r\n");
        assert_eq!(g.history_len(), 0);
        // Any scroll_back clamps to 0 (no history) -> always the live screen.
        assert_eq!(g.view_row_text(5, 0), row_str(&g, 0));
        assert_eq!(g.view_row_text(5, 1), row_str(&g, 1));
    }

    #[test]
    fn resize_clips_and_clamps() {
        // A column-width change now REFLOWS (follow-up #47) instead of
        // clipping -- this test was originally written for the pre-reflow
        // clip/pad behavior and is updated here to the tmux-faithful
        // result, hand-derived:
        //
        // Grid::new(5, 3, 0): row0 = "abc  " (terminal, unwrapped,
        // trimmed_len=3), row1/row2 blank. history_limit=0 so no
        // scrollback capture ever happens.
        //
        // resize(2, 2): width 5->2 triggers reflow (using the OLD row
        // count, 3, before the row-count axis is touched):
        //   logical line0 = "abc" (len 3) -> at width 2: ceil(3/2)=2 rows:
        //     "ab" (wrapped=true), "c " (wrapped=false)
        //   logical line1 = ""  (len 0) -> 1 row ""
        //   logical line2 = ""  (len 0) -> 1 row ""
        //   total = 4 physical rows, old_rows(3) needed -> hsize = 4-3 = 1:
        //   the OLDEST row ("ab") overflows into history -- but
        //   history_limit==0 discards it (matches "scrollback disabled"
        //   semantics elsewhere: overflow that would scroll off is lost).
        //   Remaining screen (3 rows) = ["c ", "", ""].
        // Cursor was at (col=3, row=0); row0's cellused (terminal, trimmed)
        // is 3, so col(3) >= cellused -> "end of logical line" (tmux's
        // UINT_MAX sentinel) -> maps to local row 1 ("c " row), col 1 of
        // line0's own 2 new rows -> global row 1 -> screen-local row 0
        // (since hsize=1) -> post-reflow cursor = (col=1, row=0).
        //
        // Then the row-count axis (3->2) applies its OWN simple
        // (non-history-aware) top-left clip: keep rows 0,1 ("c ", "  "),
        // drop row2; cursor row 0 stays valid.
        let mut g = Grid::new(5, 3, 0);
        g.feed(b"abc"); // cursor at (3,0)
        g.resize(2, 2);
        assert_eq!(g.cols(), 2);
        assert_eq!(g.rows(), 2);
        assert_eq!(g.cell(0, 0).ch, 'c'); // "ab" scrolled off (history disabled: lost)
        assert_eq!(g.cell(1, 0).ch, ' ');
        assert_eq!(g.cursor(), (1, 0));
    }

    #[test]
    fn resize_grows_and_pads() {
        // Widening also goes through reflow now (width changed 2->4), but
        // "ab" never wrapped (row0 is a single terminal row, unwrapped) and
        // the reflowed content (1 row, "ab" + padding) still fits inside
        // the unchanged 2-row screen, so the visible result is identical to
        // plain clip/pad padding.
        let mut g = Grid::new(2, 2, 0);
        g.feed(b"ab");
        g.resize(4, 3);
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(1, 0).ch, 'b');
        assert_eq!(g.cell(3, 2).ch, ' '); // padded
    }

    /// Set only when the cursor auto-wraps at the right margin; cleared on
    /// an explicit linefeed (RED test per the SP7 Task 2 brief).
    #[test]
    fn autowrap_sets_wrapped_flag_hard_newline_clears_it() {
        let mut g = Grid::new(3, 3, 0);
        // Fill row0 exactly (3 cols) -- wrap_pending is set but NOT yet
        // consumed, so the flag isn't set until the NEXT print.
        g.feed(b"abc");
        assert!(!g.state.row_wrapped[0]);
        // One more printable char consumes the pending wrap: row0 becomes a
        // soft-wrap row, cursor lands on row1 col0.
        g.feed(b"d");
        assert!(g.state.row_wrapped[0]);
        assert_eq!(g.cursor(), (1, 1));
        assert_eq!(g.cell(0, 1).ch, 'd');

        // Now force row1 to also look wrapped (as if from earlier content),
        // then send a HARD newline from it: the flag must be cleared, not
        // left stale.
        g.state.row_wrapped[1] = true;
        g.feed(b"\r\n");
        assert!(!g.state.row_wrapped[1]);
        // row0's wrap (a different row, from a real auto-wrap) is untouched.
        assert!(g.state.row_wrapped[0]);
    }

    #[test]
    fn narrow_resize_rewraps_long_line() {
        // 80-col grid, one 100-char line: autowraps into exactly one
        // wrapped pair -- row0 (80 chars, wrapped=true), row1 (20 chars,
        // wrapped=false). 3 more blank rows below it (rows=5 total).
        let mut g = Grid::new(80, 5, 100);
        let line: String = (0..100).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        g.feed(line.as_bytes());
        assert_eq!(g.history_len(), 0);
        assert!(g.state.row_wrapped[0]);
        assert!(!g.state.row_wrapped[1]);
        assert_eq!(g.cursor(), (20, 1));

        // Resize to 40 cols: logical line len=100 -> ceil(100/40)=3 rows:
        // 40, 40, 20 chars (first 2 wrapped=true, last wrapped=false). But
        // the 5 original physical rows (2 real + 3 blank) can't hold the
        // now-3-row real content plus the 3 still-blank logical lines (6
        // rows needed > 5 available), so the OLDEST row -- the line's
        // first 40-char chunk -- overflows into scrollback history
        // (captured since history_limit=100 is large enough).
        g.resize(40, 5);
        assert_eq!(g.cols(), 40);
        assert_eq!(g.history_len(), 1);
        assert!(g.state.history[0].wrapped); // chunk0 -> chunk1
        assert!(g.state.row_wrapped[0]); // chunk1 (screen row0) -> chunk2
        assert!(!g.state.row_wrapped[1]); // chunk2 (screen row1), terminal

        let concatenated = g.view_row_text(1, 0).trim_end().to_string()
            + g.view_row_text(0, 0).trim_end()
            + g.view_row_text(0, 1).trim_end();
        assert_eq!(concatenated, line);
        // Cursor was at the end of the 100-char content -> still the end:
        // chunk2 (screen row1, since chunk0 is now in history), col 20
        // (100 - 2*40).
        assert_eq!(g.cursor(), (20, 1));
    }

    #[test]
    fn widen_resize_rejoins_soft_wrapped_rows() {
        // A soft-wrapped pair rejoins into one row when widened enough.
        let mut g = Grid::new(5, 4, 100);
        g.feed(b"abcdefg"); // "abcde" (wrapped) + "fg" (terminal)
        assert!(g.state.row_wrapped[0]);
        g.resize(10, 4);
        assert_eq!(row_str(&g, 0).trim_end(), "abcdefg");
        assert!(!g.state.row_wrapped[0]); // single row now, not wrapped

        // A HARD-newline pair must NOT rejoin, even though it also fits.
        let mut h = Grid::new(5, 4, 100);
        h.feed(b"abc\r\nde");
        assert!(!h.state.row_wrapped[0]);
        h.resize(10, 4);
        assert_eq!(row_str(&h, 0).trim_end(), "abc");
        assert_eq!(row_str(&h, 1).trim_end(), "de");
    }

    #[test]
    fn reflow_preserves_scrollback_content_across_shrink_and_grow() {
        // Round-trip 80 -> 40 -> 80 restores the original visible text
        // exactly. Hand-derived: at 40 cols the 100-char line needs 3 rows
        // instead of 2, overflowing its first 40-char chunk into
        // scrollback (history_limit=1000, so it's captured, not
        // discarded); widening back to 80 re-joins it into 2 rows again,
        // exactly filling the original 5-row screen with zero left over in
        // history -- narrowing then widening are exact inverses here.
        let mut g = Grid::new(80, 5, 1000);
        let line: String = (0..100).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        g.feed(line.as_bytes());
        g.feed(b"\r\nsecond line\r\nthird");

        g.resize(40, 5);
        assert_eq!(g.history_len(), 1); // the 100-char line's first chunk

        g.resize(80, 5);

        assert_eq!(g.cols(), 80);
        assert_eq!(g.history_len(), 0);
        assert_eq!(row_str(&g, 0), line[0..80]);
        assert_eq!(row_str(&g, 1).trim_end(), &line[80..100]);
        assert_eq!(row_str(&g, 2).trim_end(), "second line");
        assert_eq!(row_str(&g, 3).trim_end(), "third");
        assert_eq!(g.cursor(), (5, 3));
    }

    #[test]
    fn alt_screen_resize_does_not_reflow() {
        // While showing the alternate screen, a column-width resize keeps
        // the original clip/pad behavior (no reflow) -- content that would
        // otherwise wrap into another row is simply clipped.
        let mut g = Grid::new(5, 3, 100);
        g.feed(b"\x1b[?1049h"); // enter alt screen
        g.feed(b"abc");
        g.resize(2, 3);
        assert_eq!(g.cols(), 2);
        assert_eq!(g.cell(0, 0).ch, 'a');
        assert_eq!(g.cell(1, 0).ch, 'b'); // 'c' clipped, NOT reflowed to row1
        assert_eq!(g.cell(0, 1).ch, ' ');
        assert_eq!(g.cell(1, 1).ch, ' ');
    }
}
