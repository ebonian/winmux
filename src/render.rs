use crate::geom::Rect;
use crate::grid::{Cell, Color, Grid, Style};
use crate::layout::PaneId;

/// Copy-mode rendering data for one pane (Task 2, sub-project 4): the pane's
/// content is read via `Grid::view_cell(scroll, ..)` instead of the live
/// `cell` when this is `Some`, and a `[scroll/history_len]` position
/// indicator is painted right-aligned on the pane's top row in
/// `Scene::mode_style`.
///
/// **LOCKED-CONTRACT AMENDMENT (SP7 Task 4 — follow-up #63):** the `cursor:
/// (u16, u16)` field this struct originally carried is REMOVED. It was dead:
/// `Renderer::compose_back` never read it (only `scroll` and `sel` are
/// consumed) — the actual terminal cursor placement during copy mode is
/// computed independently by `server::render_one`'s own `(cursor,
/// cursor_visible)` match on `client.mode`, which clamps the copy state's
/// `(cx, cy)` into the pane rect directly and was never routed through
/// `CopyView` at all. See `docs/specs/2026-07-06-mvp-interfaces.md`'s sibling
/// amendment for the `PaneView`/`CopyView` history.
pub struct CopyView {
    pub scroll: u32,
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
    /// SP7 Task 15 (closes #50's remainder; `docs/tmux-reference/
    /// choose-tree.md` `## 3.3`): `true` when this row is in the tagged
    /// set. Drawn as a fixed-width `"* "` tag slot (mirroring the marker
    /// slot's own fixed two-column width so sibling rows stay aligned)
    /// right after the marker slot, before the row's own text.
    pub tagged: bool,
}

/// The live preview box painted below choose-tree's row list (SP6 wave 2
/// Task 8; fix round 1 upgraded the chrome to the doc's full spec —
/// `docs/tmux-reference/choose-tree.md` `## 3.2`/`## 6`, tmux's
/// `screen_write_box`): a full 4-sided single-line box (`┌─┐│└┘`, in
/// `Scene::border`'s style) around the whole `rect`, with `title` embedded
/// over the top border starting at column `rect.x + 1`, then `content`
/// (already-composed filmstrip cells — dividers, per-slot labels, and each
/// slot's raw pane-cell copy — all pre-blitted by the SERVER, which is the
/// one place that holds every pane's `Grid`) blitted verbatim into the
/// interior at the doc's insets: 2 cells horizontal (`rect.x + 2`, usable
/// width `rect.w - 4`), 1 row vertical (`rect.y + 1`, usable height
/// `rect.h - 2` — the top and bottom border rows). `content` is
/// `content_w * content_h` cells, row-major; it may be LARGER than the
/// interior (the renderer truncates from the top-left corner, never scales)
/// or smaller (the renderer leaves the remainder as whatever the panel's
/// full-clear already put there — blank).
/// One row of a display-menu overlay (SP7 Task 16, closes #51). `text` is
/// the FULLY pre-formatted display string (item name, right-padded, then a
/// `" (key)"` shortcut hint if the item has one — already computed to fill
/// the box's content width, see `server::dispatch::build_menu_rows`) — the
/// renderer draws it verbatim, it does no padding/truncation of its own
/// (mirrors `TreeRowCell::text`'s own "server formats, renderer just
/// blits" split). `separator: true` draws a plain horizontal rule instead
/// (`text` is unused, always empty, for a separator row).
pub struct MenuRowCell {
    pub text: String,
    pub separator: bool,
    pub selected: bool,
}

/// A display-menu overlay (SP7 Task 16): a small floating bordered box —
/// UNLIKE [`Overlay::List`], this does NOT clear/replace the whole client
/// area; it paints only inside `rect`, leaving every pane/border/status
/// cell outside it untouched (matching real tmux's own menu, which floats
/// over the existing screen). `rect` is resolved once at open time by the
/// SERVER (`server::dispatch::open_menu`/`resolve_menu_axis`) — the
/// renderer treats it as already-clamped-to-fit and does no further
/// positioning of its own.
pub struct MenuOverlay {
    pub rect: Rect,
    pub title: String,
    pub rows: Vec<MenuRowCell>,
}

pub struct PreviewBlock {
    /// The full preview region, box borders included: row `rect.y` is the
    /// top border, row `rect.y + rect.h - 1` the bottom border, columns
    /// `rect.x`/`rect.x + rect.w - 1` the sides. Spans the panel's full
    /// width.
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
    /// `None` means no preview box is painted.
    pub preview: Option<PreviewBlock>,
    /// SP7 Task 15 (closes #73): the row list's own height in panel rows,
    /// from `Server::choose_tree_list_height` -- the AUTHORITATIVE cap on
    /// how many rows the list paints, REGARDLESS of whether `preview` is
    /// `Some` (list height == `preview.rect.y`, by construction) or `None`
    /// (either a legitimate full-height list, e.g. preview OFF, in which
    /// case this equals the panel height; OR a degenerate geometry where the
    /// preview box couldn't be painted at all, in which case this stays at
    /// the SMALL sizing-formula value and the rows below it are left blank
    /// -- see `dispatch::Server::choose_tree_preview_paintable`'s doc
    /// comment for why the list must NOT silently expand to fill that
    /// space).
    pub list_height: u16,
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
    /// clock-mode (Task 10, sub-project 6 wave 2, `prefix-t`): the bound
    /// pane's rect, its already-formatted time string (`server::
    /// format_clock` -- the render layer never touches wall-clock, matching
    /// `PaneDigits`' own "server resolves identity, render only paints"
    /// split), and the resolved `clock-mode-colour`. See
    /// [`Renderer::paint_clock`] for the exact drawing rule
    /// (`docs/tmux-reference/status-line-and-messages.md` `## 6. Clock
    /// mode`).
    Clock(Rect, String, Color),
    /// display-menu overlay (SP7 Task 16, closes #51): a small floating
    /// bordered box, painted OVER whatever else is on screen inside its own
    /// `rect` only — see [`MenuOverlay`]'s doc comment for how this differs
    /// from [`Overlay::List`]'s full-panel clear.
    Menu(MenuOverlay),
}

/// `pane-border-indicators` (Task 11, sub-project 6 wave 2 --
/// `docs/tmux-reference/panes-and-layout.md` §7.4): gates BOTH the
/// active-pane border COLOURING (`Colour`/`Both` -- see the half-border rule
/// documented on [`Scene::border_active`] and in `compose_back`'s border
/// pass) and the four active-pane ARROW glyphs (`Arrows`/`Both`, drawn just
/// inside each corner of the focused pane's own border, pointing at it).
/// `Off` = plain [`Scene::border`] everywhere and no arrows -- tmux's
/// literal "neither colour nor arrows indicate activity" (§7.4). Default
/// (tmux and winmux) is `Colour`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderIndicators {
    Off,
    Colour,
    Arrows,
    Both,
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
    /// Style used for border cells whose OWNER (see `compose_back`'s border
    /// pass) is the focused pane (`pane-active-border-style`, tmux default
    /// `fg=green`) -- gated by [`Scene::border_indicators`] being `Colour`
    /// or `Both`. Ownership of a border cell is normally "any pane it's
    /// orthogonally adjacent to that happens to be the focused one"
    /// (`docs/tmux-reference/panes-and-layout.md` §7.1's general per-cell
    /// adjacency rule), EXCEPT when the window has exactly two tiled panes:
    /// then the ONE shared divider between them is split cosmetically in
    /// half (`wy <= sy/2` owned by the left pane for a side-by-side split,
    /// `wx <= sx/2` owned by the top pane for a stacked split; the remainder
    /// owned by the other pane) instead of the whole divider reading as
    /// adjacent to both -- see the doc's two-pane special case, §7.1.
    pub border_active: Style,
    /// See [`BorderIndicators`].
    pub border_indicators: BorderIndicators,
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

            // General N-pane rule (`redraw_get_pane_for_border_style`'s "if
            // active adjacent, return active" branch, screen-redraw.c:1108-
            // 1131, per doc §7.1): a border cell's owner is the focused pane
            // if any of its 4 orthogonal neighbor cells falls inside the
            // focused pane's rect (ties among non-focused neighbors never
            // matter here since every non-focused pane shares one style).
            // This is the ONLY rule for windows that do NOT have exactly two
            // tiled panes -- unchanged from pre-Task-11 behavior, so a
            // 3+-pane window (e.g. a full-height left pane with the right
            // column split top/bottom, this task's target bug-report
            // layout) keeps its exact prior per-cell adjacency styling; see
            // `three_pane_left_tall_right_split_general_rule_unchanged`
            // below for the worked example.
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

            // Two-pane half-border rule (`redraw_check_two_pane_colours` +
            // `redraw_mark_two_pane_colours`, screen-redraw.c:404-420,788-
            // 829, doc §7.1's two-pane special case): ONLY when the window
            // has EXACTLY two tiled panes, the general rule above is
            // overridden -- otherwise it would colour the ENTIRE shared
            // divider active, since both panes are adjacent to every cell of
            // it (the user-visible bug this task fixes). Instead the single
            // shared divider is split cosmetically: for a side-by-side
            // (LEFTRIGHT) split, divider cells with `wy <= sy/2` (0-based row
            // offset from the pane pair's shared top edge; `sy` = their
            // shared height) belong to the LEFT pane, the rest to the RIGHT;
            // for a stacked (TOPBOTTOM) split, `wx <= sx/2` belongs to the
            // TOP pane, the rest to the BOTTOM. `half_rule` is `None` (general
            // rule applies) whenever there aren't exactly two panes, or the
            // two rects aren't axis-aligned the way a real tiled 2-pane split
            // always is (defensive; unreachable via any real layout).
            let half_rule: Option<(bool, u16, u16, Rect, Rect)> = if scene.panes.len() == 2 {
                let a = scene.panes[0].rect;
                let b = scene.panes[1].rect;
                if a.y == b.y && a.h == b.h {
                    // side-by-side: vertical divider between them.
                    let (left, right) = if a.x <= b.x { (a, b) } else { (b, a) };
                    Some((true, a.y, a.h, left, right))
                } else if a.x == b.x && a.w == b.w {
                    // stacked: horizontal divider between them.
                    let (top, bottom) = if a.y <= b.y { (a, b) } else { (b, a) };
                    Some((false, a.x, a.w, top, bottom))
                } else {
                    None
                }
            } else {
                None
            };
            let owner_is_focused = |x: i32, y: i32| -> bool {
                match &half_rule {
                    Some((vertical, origin, extent, first, second)) => {
                        let midpoint = extent / 2;
                        let owner_is_first =
                            if *vertical { (y as u16).saturating_sub(*origin) <= midpoint } else { (x as u16).saturating_sub(*origin) <= midpoint };
                        let owner = if owner_is_first { first } else { second };
                        focused_rect == Some(*owner)
                    }
                    None => touches_focused(x, y),
                }
            };

            let colour_enabled = matches!(scene.border_indicators, BorderIndicators::Colour | BorderIndicators::Both);
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
                    let style = if colour_enabled && owner_is_focused(x, y) { scene.border_active } else { scene.border };
                    self.set(x as u16, y as u16, Cell { ch, style });
                }
            }

            // Arrow-indicator pass (`pane-border-indicators` arrows/both,
            // doc §7.4, `redraw_mark_border_arrows`, screen-redraw.c:615-
            // 658): four glyphs placed just inside each corner of the
            // FOCUSED pane's own border -- one per side that actually HAS a
            // border cell there (a pane flush against the window edge has no
            // border on that side, so gets no arrow there, via the
            // `is_border` guard below). Each glyph points INTO the focused
            // pane (the direction from the border cell toward the pane),
            // reproducing tmux's `left_wp`/`right_wp`/`top_wp`/`bottom_wp
            // == active` ladder; per the doc's Windows note, glyphs are
            // U+2190 LEFTWARDS ARROW .. U+2193 DOWNWARDS ARROW rather than
            // the original ACS characters. Runs AFTER the colouring pass
            // above and reuses whatever style that pass already painted onto
            // the cell (so `arrows` alone -- colour disabled -- draws the
            // glyph on the plain `pane-border-style`, and `both` draws it on
            // the just-painted active colour); only the CHARACTER changes.
            let arrows_enabled = matches!(scene.border_indicators, BorderIndicators::Arrows | BorderIndicators::Both);
            if arrows_enabled {
                if let Some(fr) = focused_rect {
                    let sides: [(i32, i32, char); 4] = [
                        (fr.x as i32 + 1, fr.y as i32 - 1, '\u{2193}'), // top border: points down, into the pane
                        (fr.x as i32 + 1, fr.y as i32 + fr.h as i32, '\u{2191}'), // bottom border: points up
                        (fr.x as i32 - 1, fr.y as i32 + 1, '\u{2192}'), // left border: points right
                        (fr.x as i32 + fr.w as i32, fr.y as i32 + 1, '\u{2190}'), // right border: points left
                    ];
                    for (ax, ay, ch) in sides {
                        if is_border(ax, ay) {
                            let idx = ay as usize * w + ax as usize;
                            let style = self.back[idx].style;
                            self.set(ax as u16, ay as u16, Cell { ch, style });
                        }
                    }
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
                // SP7 Task 15 (closes #73): the row list is ALWAYS capped to
                // `list.list_height`, whether or not a preview box is
                // actually painted -- a `None` preview no longer implies
                // "list gets the whole panel" (see `ListOverlay::list_height`'s
                // doc comment for the degenerate-geometry case this fixes).
                let list_cap: u16 = list.list_height.min(rows);
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
                    let tag_slot = if row.tagged { "* " } else { "  " };
                    let line = format!("{indent}{marker_slot}{tag_slot}{}", row.text);
                    for (cx, ch) in line.chars().enumerate() {
                        if cx as u16 >= cols {
                            break;
                        }
                        self.set(cx as u16, yy, Cell { ch, style });
                    }
                }

                // Preview box (SP6 wave 2 Task 8; fix round 1): a full
                // 4-sided single-line box around the whole preview region
                // (tmux's `screen_write_box`, `docs/tmux-reference/
                // choose-tree.md` `## 3.2`) with the title embedded over
                // the top border starting at column `rect.x + 1`, then a
                // raw, truncate-never-scale blit of the pre-composed
                // filmstrip `content` into the interior at the doc's
                // insets: 2 cells horizontal (`rect.x + 2`, width
                // `rect.w - 4`), 1 row vertical (`rect.y + 1`, height
                // `rect.h - 2` -- the top and bottom border rows). A rect
                // under 2x2 can't form a box and paints nothing (never
                // reachable in production: `choose_tree_list_height`'s
                // box-size guard already drops the preview outright below
                // 5 columns / a 5-row region).
                if let Some(pv) = &list.preview {
                    if pv.rect.w >= 2 && pv.rect.h >= 2 && pv.rect.y < rows {
                        let bs = scene.border;
                        let x0 = pv.rect.x;
                        let y0 = pv.rect.y;
                        let x1 = pv.rect.x + pv.rect.w - 1;
                        let y1 = pv.rect.y + pv.rect.h - 1;
                        // Top + bottom fill, then sides, then corners
                        // (`self.set` already clips to the buffer).
                        for x in x0..=x1.min(cols.saturating_sub(1)) {
                            self.set(x, y0, Cell { ch: '─', style: bs });
                            self.set(x, y1, Cell { ch: '─', style: bs });
                        }
                        for y in (y0 + 1)..y1 {
                            self.set(x0, y, Cell { ch: '│', style: bs });
                            self.set(x1, y, Cell { ch: '│', style: bs });
                        }
                        self.set(x0, y0, Cell { ch: '┌', style: bs });
                        self.set(x1, y0, Cell { ch: '┐', style: bs });
                        self.set(x0, y1, Cell { ch: '└', style: bs });
                        self.set(x1, y1, Cell { ch: '┘', style: bs });
                        // Title over the top border, from column x0+1, never
                        // overwriting the right corner.
                        for (i, ch) in pv.title.chars().enumerate() {
                            let x = x0 + 1 + i as u16;
                            if x >= x1 {
                                break;
                            }
                            self.set(x, y0, Cell { ch, style: bs });
                        }
                        // Interior blit at inset (2, 1), truncated (never
                        // scaled) to the interior's `(rect.w-4) x (rect.h-2)`.
                        let interior_w = pv.rect.w.saturating_sub(4);
                        let interior_h = pv.rect.h.saturating_sub(2);
                        let copy_h = pv.content_h.min(interior_h);
                        let copy_w = pv.content_w.min(interior_w);
                        for cy in 0..copy_h {
                            let yy = y0 + 1 + cy;
                            if yy >= rows {
                                break;
                            }
                            for cx in 0..copy_w {
                                let x = x0 + 2 + cx;
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
            Some(Overlay::Clock(rect, text, colour)) => {
                self.paint_clock(*rect, text, *colour, cols, rows);
            }
            Some(Overlay::Menu(m)) => {
                self.paint_menu(m, scene.mode_style, scene.border, cols, rows);
            }
        }
    }

    /// Paint a display-menu overlay (SP7 Task 16, closes #51): a single-
    /// line box (`┌─┐│└┘`, in `border` style — same glyph set choose-tree's
    /// preview box uses) around `m.rect`, with `m.title` CENTERED over the
    /// top border (a documented simplification of real tmux's per-title
    /// `#[align=...]` support — winmux's `display-menu` doesn't parse
    /// inline style/alignment markers inside `-T`, so every menu title is
    /// centered unconditionally, matching what every one of winmux's own
    /// default menus' titles would render as anyway since they all pass
    /// `#[align=centre]` verbatim in real tmux). Interior rows start at
    /// `rect.y + 1`; each real item row fills its full interior width with
    /// `mode_style` (selected) or the default style, then draws `row.text`
    /// starting one column in from the left border (a further 1-column pad
    /// from the border, mirroring tmux's own item indentation) — a
    /// separator row instead fills the interior with `─`. Zero/degenerate
    /// rects (`w < 2 || h < 2`) paint nothing.
    fn paint_menu(&mut self, m: &MenuOverlay, mode_style: Style, border: Style, cols: u16, rows: u16) {
        let r = m.rect;
        if r.w < 2 || r.h < 2 {
            return;
        }
        let x0 = r.x;
        let y0 = r.y;
        let x1 = r.x + r.w - 1;
        let y1 = r.y + r.h - 1;

        // Interior clear (default style) — the box floats OVER whatever
        // was already composed there.
        for y in y0..=y1 {
            if y >= rows {
                break;
            }
            for x in x0..=x1 {
                if x >= cols {
                    break;
                }
                self.set(x, y, Cell { ch: ' ', style: Style::default() });
            }
        }

        // Border.
        for x in x0..=x1.min(cols.saturating_sub(1)) {
            self.set(x, y0, Cell { ch: '─', style: border });
            if y1 < rows {
                self.set(x, y1, Cell { ch: '─', style: border });
            }
        }
        for y in (y0 + 1)..y1 {
            if y >= rows {
                break;
            }
            self.set(x0, y, Cell { ch: '│', style: border });
            if x1 < cols {
                self.set(x1, y, Cell { ch: '│', style: border });
            }
        }
        self.set(x0, y0, Cell { ch: '┌', style: border });
        self.set(x1, y0, Cell { ch: '┐', style: border });
        if y1 < rows {
            self.set(x0, y1, Cell { ch: '└', style: border });
            self.set(x1, y1, Cell { ch: '┘', style: border });
        }

        // Title, centered on the top border.
        let interior_w = r.w.saturating_sub(2) as usize;
        if !m.title.is_empty() && interior_w > 0 {
            let title_chars: Vec<char> = m.title.chars().collect();
            let tlen = title_chars.len().min(interior_w);
            let pad = (interior_w - tlen) / 2;
            let start_x = x0 + 1 + pad as u16;
            for (i, ch) in title_chars[..tlen].iter().enumerate() {
                let x = start_x + i as u16;
                if x >= x1 || x >= cols {
                    break;
                }
                self.set(x, y0, Cell { ch: *ch, style: border });
            }
        }

        // Item rows.
        for (i, row) in m.rows.iter().enumerate() {
            let y = y0 + 1 + i as u16;
            if y >= y1 || y >= rows {
                break;
            }
            if row.separator {
                for x in (x0 + 1)..x1 {
                    if x >= cols {
                        break;
                    }
                    self.set(x, y, Cell { ch: '─', style: border });
                }
                continue;
            }
            let style = if row.selected { mode_style } else { Style::default() };
            for x in (x0 + 1)..x1 {
                if x >= cols {
                    break;
                }
                self.set(x, y, Cell { ch: ' ', style });
            }
            for (dx, ch) in row.text.chars().enumerate() {
                let x = x0 + 2 + dx as u16;
                if x >= x1 || x >= cols {
                    break;
                }
                self.set(x, y, Cell { ch, style });
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

    /// Paint clock-mode's (Task 10, `prefix-t`) big-digit time display into
    /// `rect`, per `docs/tmux-reference/status-line-and-messages.md`
    /// `## 6. Clock mode` (`window_clock_draw_screen`, `window-clock.c:222-
    /// 315`): the whole rect is first cleared to the default style
    /// (`clearscreen(8)` -- the mode replaces the pane's normal content,
    /// mirroring `Overlay::List`'s own full-panel clear, just scoped to one
    /// pane's rect instead of the whole client area), then EITHER:
    /// - a 5x5 block-glyph rendering of `text` (glyph pitch 6 columns,
    ///   needs `rect.w >= 6 * text.chars().count()` and `rect.h >= 6`, the
    ///   doc's exact big-digit-mode size guard), in `colour` used as BOTH fg
    ///   and bg for a solid block (reproduced here, like `paint_pane_digit`'s
    ///   own block font, as a blank cell with `bg` set -- a space glyph has
    ///   no visible foreground pixels either way, so this is visually
    ///   identical to "both fg and bg painted"); origin `ox = rect.x +
    ///   rect.w/2 - 3*len`, `oy = rect.y + rect.h/2 - 3` (the doc's exact
    ///   centering formula, safe from underflow given the size guard above);
    /// - OR, when too small for that but `text` still fits on one row, the
    ///   plain `text` centered in `colour` (fg only, doc's documented
    ///   fallback);
    /// - OR nothing at all (rect too small even for the fallback, or
    ///   zero-size) -- matching the doc's "nothing if even that doesn't
    ///   fit".
    fn paint_clock(&mut self, rect: Rect, text: &str, colour: Color, cols: u16, rows: u16) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        let default_style = Style::default();
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                if x < cols && y < rows {
                    self.set(x, y, Cell { ch: ' ', style: default_style });
                }
            }
        }
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len() as u16;
        if len == 0 {
            return;
        }
        if rect.w >= 6 * len && rect.h >= 6 {
            let block_style = Style { bg: colour, ..default_style };
            let ox = rect.x + rect.w / 2 - 3 * len;
            let oy = rect.y + rect.h / 2 - 3;
            for (i, ch) in chars.iter().enumerate() {
                let gx = ox + i as u16 * 6;
                for (dy, row) in clock_glyph_bitmap(*ch).iter().enumerate() {
                    for (dx, gc) in row.chars().enumerate() {
                        if gc != '#' {
                            continue;
                        }
                        let x = gx + dx as u16;
                        let y = oy + dy as u16;
                        if x < cols && y < rows {
                            self.set(x, y, Cell { ch: ' ', style: block_style });
                        }
                    }
                }
            }
        } else if rect.w >= len {
            let fg_style = Style { fg: colour, ..default_style };
            let ox = rect.x + (rect.w - len) / 2;
            let oy = rect.y + rect.h / 2;
            for (i, ch) in chars.iter().enumerate() {
                let x = ox + i as u16;
                if x < cols && oy < rows {
                    self.set(x, oy, Cell { ch: *ch, style: fg_style });
                }
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

/// clock-mode's (Task 10, sub-project 6 wave 2) 5x5 block-glyph font: real
/// tmux's clock and display-panes big-digit fonts are the SAME table
/// (`docs/tmux-reference/status-line-and-messages.md` `## 6. Clock mode`:
/// "This is the same 5x5 table display-panes uses for its big pane
/// numbers") -- winmux's own `digit_bitmap` above is already a from-scratch
/// substitute for tmux's exact bitmap (not a byte-for-byte port, see its own
/// doc comment), so this reproduces that SAME winmux-internal precedent:
/// digits `0`-`9` delegate straight to [`digit_bitmap`] (one font family,
/// shared between the two overlays, exactly like real tmux's), and `:`/`A`/
/// `P`/`M` (needed for the `12`-style `%l:%M ` + `AM`/`PM` display, which
/// `display-panes` never needed) are new winmux-original glyphs in the same
/// 5x5 style. Any other character (a literal space, from the `12`-style
/// format's `%l:%M ` separator) falls back to all-blank -- correct behavior
/// for that specific case, since the rect was already cleared to blank by
/// `paint_clock` before glyphs are painted.
fn clock_glyph_bitmap(ch: char) -> [&'static str; 5] {
    if let Some(d) = ch.to_digit(10) {
        return digit_bitmap(d);
    }
    match ch {
        ':' => [".....", "..#..", ".....", "..#..", "....."],
        'A' => ["..#..", ".#.#.", "#####", "#...#", "#...#"],
        'P' => ["#####", "#...#", "#####", "#....", "#...."],
        'M' => ["#...#", "##.##", "#.#.#", "#...#", "#...#"],
        _ => [".....", ".....", ".....", ".....", "....."],
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
    // status row at y=3. Right pane is focused. Task 11: with exactly two
    // tiled panes the half-border rule applies (sy=3, midpoint=3/2=1) --
    // wy<=1 (rows 0,1) owned by the LEFT pane (not focused -> default),
    // wy=2 (row 2) owned by the RIGHT pane (focused -> green). Superseded
    // by (but kept alongside, for the glyph/content assertions) the more
    // thorough `two_pane_vertical_divider_half_styled` below.
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
            border_indicators: BorderIndicators::Colour,
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
        // half-border rule (Task 11): rows 0,1 owned by the LEFT (inactive)
        // pane -> default fg; row 2 owned by the RIGHT (focused) pane ->
        // green fg = Idx(2). Pre-Task-11 this asserted the WHOLE column
        // green (the bug this task fixes: an inactive divider row read as
        // active) -- sanctioned inversion, see task report.
        assert_eq!(r.back_cell(3, 0).style.fg, Color::Default);
        assert_eq!(r.back_cell(3, 1).style.fg, Color::Default);
        assert_eq!(r.back_cell(3, 2).style.fg, Color::Idx(2));
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
            border_indicators: BorderIndicators::Colour,
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

    // ---- Task 11 (sub-project 6 wave 2): half-border active indication +
    // pane-border-indicators ----

    /// Builds the 7x4 two-pane-side-by-side scene from
    /// `two_panes_content_and_focused_border`, parameterized on which side
    /// is focused, so the focus-flip half of the required test is a plain
    /// re-invocation with the other side true.
    fn two_pane_vertical_scene<'a>(left: &'a Grid, right: &'a Grid, left_focused: bool, indicators: BorderIndicators) -> Scene<'a> {
        Scene {
            size: (7, 4),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 3 }, grid: left, focused: left_focused, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 3 }, grid: right, focused: !left_focused, dead: false, copy: None },
            ],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: indicators,
            overlay: None,
        }
    }

    // Side-by-side split, vertical divider column x=3, rows 0..=2 (sy=3,
    // rows shared by both panes, midpoint = sy/2 = 1 floor-divided): rows
    // 0,1 (wy<=1) are owned by the LEFT pane, row 2 (wy=2) by the RIGHT pane
    // (doc §7.1's two-pane special case, LEFTRIGHT: "top half...left pane's
    // border and the bottom half...right pane's"). Left focused -> rows 0,1
    // green, row 2 default; flipping focus to the right pane inverts EVERY
    // row's verdict (not just a recolor of the same cells -- the OWNER
    // assignment per row is fixed by geometry, only which owner counts as
    // "active" flips).
    #[test]
    fn two_pane_vertical_divider_half_styled() {
        let left = grid_with(3, 3, b"");
        let right = grid_with(3, 3, b"");

        let scene = two_pane_vertical_scene(&left, &right, true, BorderIndicators::Colour);
        let mut r = Renderer::new(7, 4);
        r.compose_back(&scene);
        assert_eq!(r.back_cell(3, 0).style.fg, Color::Idx(2)); // row 0: left owner, left focused -> green
        assert_eq!(r.back_cell(3, 1).style.fg, Color::Idx(2)); // row 1: left owner, left focused -> green
        assert_eq!(r.back_cell(3, 2).style.fg, Color::Default); // row 2: right owner, left focused -> default

        // focus-flip: right pane now active.
        let scene = two_pane_vertical_scene(&left, &right, false, BorderIndicators::Colour);
        let mut r = Renderer::new(7, 4);
        r.compose_back(&scene);
        assert_eq!(r.back_cell(3, 0).style.fg, Color::Default); // row 0: left owner, right focused -> default
        assert_eq!(r.back_cell(3, 1).style.fg, Color::Default); // row 1: left owner, right focused -> default
        assert_eq!(r.back_cell(3, 2).style.fg, Color::Idx(2)); // row 2: right owner, right focused -> green
    }

    /// Builds a 7x6 two-pane-stacked scene (top pane rows 0-1, horizontal
    /// divider row y=2, bottom pane rows 3-4, status row y=5), parameterized
    /// on which side is focused.
    fn two_pane_horizontal_scene<'a>(top: &'a Grid, bottom: &'a Grid, top_focused: bool, indicators: BorderIndicators) -> Scene<'a> {
        Scene {
            size: (7, 6),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 7, h: 2 }, grid: top, focused: top_focused, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 0, y: 3, w: 7, h: 2 }, grid: bottom, focused: !top_focused, dead: false, copy: None },
            ],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: indicators,
            overlay: None,
        }
    }

    // Stacked split, horizontal divider row y=2, columns 0..=6 (sx=7, width
    // shared by both panes, midpoint = sx/2 = 3 floor-divided): columns
    // 0-3 (wx<=3, 4 cols) owned by the TOP pane, columns 4-6 (3 cols) by the
    // BOTTOM pane (doc §7.1's two-pane special case, TOPBOTTOM: "left
    // half...top pane's border and...right...bottom pane's"). Top focused
    // -> cols 0-3 green, cols 4-6 default; flipping focus to the bottom pane
    // inverts every column's verdict, same as the vertical-divider test.
    #[test]
    fn two_pane_horizontal_divider_half_styled() {
        let top = grid_with(7, 2, b"");
        let bottom = grid_with(7, 2, b"");

        let scene = two_pane_horizontal_scene(&top, &bottom, true, BorderIndicators::Colour);
        let mut r = Renderer::new(7, 6);
        r.compose_back(&scene);
        for x in 0..=3u16 {
            assert_eq!(r.back_cell(x, 2).style.fg, Color::Idx(2), "col {x} top-owned, top focused -> green");
        }
        for x in 4..=6u16 {
            assert_eq!(r.back_cell(x, 2).style.fg, Color::Default, "col {x} bottom-owned, top focused -> default");
        }

        // focus-flip: bottom pane now active.
        let scene = two_pane_horizontal_scene(&top, &bottom, false, BorderIndicators::Colour);
        let mut r = Renderer::new(7, 6);
        r.compose_back(&scene);
        for x in 0..=3u16 {
            assert_eq!(r.back_cell(x, 2).style.fg, Color::Default, "col {x} top-owned, bottom focused -> default");
        }
        for x in 4..=6u16 {
            assert_eq!(r.back_cell(x, 2).style.fg, Color::Idx(2), "col {x} bottom-owned, bottom focused -> green");
        }
    }

    // `pane-border-indicators off`: no active colouring anywhere, even on a
    // two-pane divider where SOME row would otherwise be green (row 2,
    // right-owned, right focused, per `two_pane_vertical_divider_half_styled`
    // above) -- every divider cell stays the plain (default) border style.
    #[test]
    fn border_indicators_off_suppresses_active_styling() {
        let left = grid_with(3, 3, b"");
        let right = grid_with(3, 3, b"");
        let scene = two_pane_vertical_scene(&left, &right, false, BorderIndicators::Off);
        let mut r = Renderer::new(7, 4);
        r.compose_back(&scene);
        assert_eq!(r.back_cell(3, 0).style.fg, Color::Default);
        assert_eq!(r.back_cell(3, 1).style.fg, Color::Default);
        assert_eq!(r.back_cell(3, 2).style.fg, Color::Default);
    }

    // `pane-border-indicators arrows`: a 9x7 "plus" arrangement (no status
    // row) where the CENTER pane (focused) has a border on all four sides,
    // so all four arrow glyphs/positions/directions can be asserted in one
    // scene (doc §7.4, `redraw_mark_border_arrows`):
    //   top    Rect{x:3,y:0,w:3,h:1}   (row 0, cols 3-5)
    //   left   Rect{x:0,y:2,w:2,h:3}   (cols 0-1, rows 2-4)
    //   center Rect{x:3,y:2,w:3,h:3}   (cols 3-5, rows 2-4) -- FOCUSED
    //   right  Rect{x:7,y:2,w:2,h:3}   (cols 7-8, rows 2-4)
    //   bottom Rect{x:3,y:6,w:3,h:1}   (row 6, cols 3-5)
    // Border gaps: row 1 (between top and center) and row 5 (between center
    // and bottom) are fully uncovered across every column; column 2
    // (between left and center) and column 6 (between center and right) are
    // uncovered for rows 2-4. The doc's fixed spots relative to the
    // FOCUSED (center) pane's own xoff=3/yoff=2/sx=3/sy=3:
    //   top border:    (xoff+1, yoff-1)      = (4, 1) -> arrow points DOWN
    //   bottom border: (xoff+1, yoff+sy)     = (4, 5) -> arrow points UP
    //   left border:   (xoff-1, yoff+1)      = (2, 3) -> arrow points RIGHT
    //   right border:  (xoff+sx, yoff+1)     = (6, 3) -> arrow points LEFT
    // `arrows` alone (not `both`) also proves colouring stays OFF: none of
    // these cells (nor the general adjacency-active tee cells) turn green.
    #[test]
    fn border_indicators_arrows_draws_glyphs_at_active_corners() {
        let top = grid_with(3, 1, b"");
        let left = grid_with(2, 3, b"");
        let center = grid_with(3, 3, b"");
        let right = grid_with(2, 3, b"");
        let bottom = grid_with(3, 1, b"");
        let scene = Scene {
            size: (9, 7),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 3, y: 0, w: 3, h: 1 }, grid: &top, focused: false, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 0, y: 2, w: 2, h: 3 }, grid: &left, focused: false, dead: false, copy: None },
                PaneView { id: 3, rect: Rect { x: 3, y: 2, w: 3, h: 3 }, grid: &center, focused: true, dead: false, copy: None },
                PaneView { id: 4, rect: Rect { x: 7, y: 2, w: 2, h: 3 }, grid: &right, focused: false, dead: false, copy: None },
                PaneView { id: 5, rect: Rect { x: 3, y: 6, w: 3, h: 1 }, grid: &bottom, focused: false, dead: false, copy: None },
            ],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Arrows,
            overlay: None,
        };
        let mut r = Renderer::new(9, 7);
        r.compose_back(&scene);

        assert_eq!(r.back_cell(4, 1).ch, '\u{2193}'); // top border: down arrow
        assert_eq!(r.back_cell(4, 5).ch, '\u{2191}'); // bottom border: up arrow
        assert_eq!(r.back_cell(2, 3).ch, '\u{2192}'); // left border: right arrow
        assert_eq!(r.back_cell(6, 3).ch, '\u{2190}'); // right border: left arrow
        // colouring stayed off (mode is `arrows`, not `both`/`colour`):
        // every arrow cell (and, spot-checked, the tee cell (3,1) that
        // would read general-adjacency-active under `colour`) is plain.
        assert_eq!(r.back_cell(4, 1).style.fg, Color::Default);
        assert_eq!(r.back_cell(4, 5).style.fg, Color::Default);
        assert_eq!(r.back_cell(2, 3).style.fg, Color::Default);
        assert_eq!(r.back_cell(6, 3).style.fg, Color::Default);
        assert_eq!(r.back_cell(3, 1).style.fg, Color::Default);
    }

    // Worked example for the task's target bug-report layout: a full-height
    // left pane with the right column split top/bottom (identical geometry
    // to `border_tee_junction`/`border_style_applied` above) -- with THREE
    // tiled panes, the two-pane half-border rule does NOT apply (doc §7.1
    // scopes it to "exactly two (tiled) panes"), so every border cell keeps
    // the pre-Task-11 general per-cell adjacency rule unchanged. This test
    // exhaustively covers every border cell for BOTH focus configurations to
    // confirm the refactor didn't regress it (computed by hand):
    //   left pane:  Rect{x:0,y:0,w:3,h:4}  (id 1)
    //   right-top:  Rect{x:4,y:0,w:3,h:1}  (id 2)
    //   right-bot:  Rect{x:4,y:2,w:3,h:2}  (id 3)
    // Border cells: vertical column x=3 rows 0-3; horizontal arm row 1,
    // cols 4-6 (the tee at (3,1) is part of the vertical column).
    //   left focused:   column x=3 rows 0-3 ALL touch the left pane's left
    //     edge -> ALL active; the horizontal arm (4,1)/(5,1)/(6,1) touches
    //     neither right pane at that position -> ALL default.
    //   right-bottom focused: column rows 0,1 don't touch right-bot (which
    //     only spans y=2-3) -> default; rows 2,3 do -> active. The
    //     horizontal arm's DOWN neighbor (y=2) is inside right-bot's rect
    //     for every one of its columns -> ALL active.
    #[test]
    fn three_pane_left_tall_right_split_general_rule_unchanged() {
        let left = grid_with(3, 4, b"");
        let rt = grid_with(3, 1, b"");
        let rb = grid_with(3, 2, b"");
        let build = |left_focused: bool, rb_focused: bool| Scene {
            size: (7, 5),
            panes: vec![
                PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 3, h: 4 }, grid: &left, focused: left_focused, dead: false, copy: None },
                PaneView { id: 2, rect: Rect { x: 4, y: 0, w: 3, h: 1 }, grid: &rt, focused: false, dead: false, copy: None },
                PaneView { id: 3, rect: Rect { x: 4, y: 2, w: 3, h: 2 }, grid: &rb, focused: rb_focused, dead: false, copy: None },
            ],
            zoomed: false,
            status: Some(default_status(Vec::new(), "")),
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Colour,
            overlay: None,
        };

        // left pane focused
        let scene = build(true, false);
        let mut r = Renderer::new(7, 5);
        r.compose_back(&scene);
        for y in 0..=3u16 {
            assert_eq!(r.back_cell(3, y).style.fg, Color::Idx(2), "column row {y}: left focused -> active");
        }
        for x in 4..=6u16 {
            assert_eq!(r.back_cell(x, 1).style.fg, Color::Default, "arm col {x}: left focused -> default");
        }

        // right-bottom pane focused
        let scene = build(false, true);
        let mut r = Renderer::new(7, 5);
        r.compose_back(&scene);
        assert_eq!(r.back_cell(3, 0).style.fg, Color::Default);
        assert_eq!(r.back_cell(3, 1).style.fg, Color::Default);
        assert_eq!(r.back_cell(3, 2).style.fg, Color::Idx(2));
        assert_eq!(r.back_cell(3, 3).style.fg, Color::Idx(2));
        for x in 4..=6u16 {
            assert_eq!(r.back_cell(x, 1).style.fg, Color::Idx(2), "arm col {x}: right-bottom focused -> active");
        }
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
                border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
                border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
                border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: vec![
                    TreeRowCell { text: "row0".to_string(), depth: 0, marker: None, selected: false, tagged: false },
                    TreeRowCell { text: "row1".to_string(), depth: 0, marker: None, selected: true, tagged: false },
                ],
                top: 0,
                preview: None,
                list_height: 3,
            })),
        };
        let mut r = Renderer::new(10, 3);
        r.compose_back(&scene);

        // Row 0 (unselected): depth 0 / no marker -> a 2-space blank marker
        // slot, then (SP7 Task 15) a 2-space blank tag slot (untagged),
        // precede the text (no indent at depth 0), then the text, default
        // style.
        let row0: String = (0..8).map(|x| r.back_cell(x, 0).ch).collect();
        assert_eq!(row0, "    row0");
        assert_eq!(r.back_cell(0, 0).style, Style::default());
        // Padding past the text is still cleared to the row's style (full
        // client-area clear, not just the text run).
        assert_eq!(r.back_cell(9, 0).ch, ' ');
        assert_eq!(r.back_cell(9, 0).style, Style::default());

        // Row 1 (selected): text painted in mode_style, including padding.
        let row1: String = (0..8).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row1, "    row1");
        assert_eq!(r.back_cell(0, 1).style, mode_style);
        assert_eq!(r.back_cell(9, 1).ch, ' ');
        assert_eq!(r.back_cell(9, 1).style, mode_style);

        // The overlay fully replaced the pane's own content underneath.
        assert_eq!(r.back_cell(0, 2).ch, ' ');
    }

    // ---- display-menu (SP7 Task 16, closes #51) -----------------------------

    /// `Overlay::Menu` paints a single-line box at `rect` ONLY -- everything
    /// outside it stays untouched pane content, unlike `Overlay::List`'s
    /// full-panel clear. Exact-cell-by-cell: border glyphs, a centered
    /// title overwriting the top border, an unselected item row (default
    /// style), a separator row (a `─`-filled interior, no text), and a
    /// selected item row (`mode_style` fill, text drawn on top).
    #[test]
    fn overlay_menu_draws_box_title_rows_and_selection() {
        let content = "P".repeat(11 * 7);
        let g = grid_with(11, 7, content.as_bytes());
        let mode_style = Style { fg: Color::Idx(0), bg: Color::Idx(3), ..Style::default() };
        let border = Style { fg: Color::Idx(240), ..Style::default() };
        let scene = Scene {
            size: (11, 7),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 11, h: 7 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: true, // suppress border compositing so only pane + overlay are in play
            status: None,
            message: None,
            border,
            border_active: green_active(),
            mode_style,
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::Menu(MenuOverlay {
                rect: Rect { x: 1, y: 1, w: 9, h: 5 },
                title: "Menu".to_string(),
                rows: vec![
                    MenuRowCell { text: "Kill".to_string(), separator: false, selected: false },
                    MenuRowCell { text: String::new(), separator: true, selected: false },
                    MenuRowCell { text: "Zoom".to_string(), separator: false, selected: true },
                ],
            })),
        };
        let mut r = Renderer::new(11, 7);
        r.compose_back(&scene);

        let row = |y: u16| -> String { (0..11).map(|x| r.back_cell(x, y).ch).collect() };

        // Outside the box entirely: untouched pane content.
        assert_eq!(row(0), "PPPPPPPPPPP");
        assert_eq!(row(6), "PPPPPPPPPPP");
        assert_eq!(r.back_cell(0, 3).ch, 'P');
        assert_eq!(r.back_cell(10, 3).ch, 'P');

        // Top border row, title "Menu" centered (interior width 7, pad 1)
        // over the border, flanked by box corners.
        assert_eq!(row(1), "P┌─Menu──┐P");
        for x in 1..=9 {
            assert_eq!(r.back_cell(x, 1).style, border, "top border cell x={x} style");
        }

        // Row 2: unselected item "Kill" -- default style throughout the
        // interior (borders in `border` style).
        assert_eq!(row(2), "P│ Kill  │P");
        assert_eq!(r.back_cell(1, 2).style, border);
        assert_eq!(r.back_cell(9, 2).style, border);
        assert_eq!(r.back_cell(3, 2).style, Style::default());
        assert_eq!(r.back_cell(2, 2).style, Style::default());

        // Row 3: separator -- interior filled with a horizontal rule, no
        // text, sides still the box's vertical border.
        assert_eq!(row(3), "P│───────│P");

        // Row 4: selected item "Zoom" -- interior (including the padding
        // cells around the text) painted in `mode_style`, borders unchanged.
        assert_eq!(row(4), "P│ Zoom  │P");
        assert_eq!(r.back_cell(1, 4).style, border);
        assert_eq!(r.back_cell(2, 4).style, mode_style);
        assert_eq!(r.back_cell(3, 4).style, mode_style);
        assert_eq!(r.back_cell(8, 4).style, mode_style);
        assert_eq!(r.back_cell(9, 4).style, border);

        // Bottom border row.
        assert_eq!(row(5), "P└───────┘P");
    }

    /// A menu whose box wouldn't fit inside the client at all is the
    /// caller's (`server::dispatch::open_menu`) responsibility to refuse
    /// opening -- this is a defensive render-layer check that a
    /// degenerate `rect` (width/height under 2) simply paints nothing
    /// rather than panicking on the underflowing `x1 - 1`/`y1 - 1` math.
    #[test]
    fn overlay_menu_degenerate_rect_paints_nothing() {
        let g = grid_with(4, 2, b"PPPPPPPP");
        let scene = Scene {
            size: (4, 2),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 4, h: 2 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: true,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::Menu(MenuOverlay {
                rect: Rect { x: 0, y: 0, w: 1, h: 1 },
                title: "X".to_string(),
                rows: vec![MenuRowCell { text: "A".to_string(), separator: false, selected: false }],
            })),
        };
        let mut r = Renderer::new(4, 2);
        r.compose_back(&scene); // must not panic
        assert_eq!(r.back_cell(0, 0).ch, 'P');
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
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: vec![
                    TreeRowCell { text: "main: 2 windows".to_string(), depth: 0, marker: Some('-'), selected: false, tagged: false },
                    TreeRowCell { text: "0: bash*".to_string(), depth: 1, marker: None, selected: false, tagged: false },
                ],
                top: 0,
                preview: None,
                list_height: 2,
            })),
        };
        let mut r = Renderer::new(20, 2);
        r.compose_back(&scene);

        // "- " marker slot + "  " blank (untagged) tag slot (SP7 Task 15) + text.
        let expect0 = "-   main: 2 windows";
        let row0: String = (0..expect0.chars().count() as u16).map(|x| r.back_cell(x, 0).ch).collect();
        assert_eq!(row0, expect0);

        // depth 1 -> "  " indent + "  " blank marker slot + "  " blank tag slot.
        let expect1 = "      0: bash*";
        let row1: String = (0..expect1.chars().count() as u16).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row1, expect1);
    }

    /// SP7 Task 15 (closes #50's remainder): a TAGGED row's tag slot renders
    /// `"* "` instead of two blanks, right after the marker slot and before
    /// the row's own text (`## 3.3`: "then `*` if the item is tagged").
    #[test]
    fn overlay_tagged_row_shows_asterisk_marker() {
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
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: vec![
                    TreeRowCell { text: "main: 2 windows".to_string(), depth: 0, marker: Some('-'), selected: false, tagged: true },
                    TreeRowCell { text: "0: bash*".to_string(), depth: 1, marker: None, selected: false, tagged: false },
                ],
                top: 0,
                preview: None,
                list_height: 2,
            })),
        };
        let mut r = Renderer::new(20, 2);
        r.compose_back(&scene);

        // "- " marker slot + "* " tag slot (TAGGED) + text.
        let expect0 = "- * main: 2 windows";
        let row0: String = (0..expect0.chars().count() as u16).map(|x| r.back_cell(x, 0).ch).collect();
        assert_eq!(row0, expect0);

        // Untagged sibling still gets the blank tag slot.
        let expect1 = "      0: bash*";
        let row1: String = (0..expect1.chars().count() as u16).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row1, expect1);
    }

    /// The preview box (fix round 1: full 4-sided single-line box per
    /// `docs/tmux-reference/choose-tree.md` `## 3.2`, tmux's
    /// `screen_write_box`): `┌`/`┐`/`└`/`┘` corners, `─` top/bottom fill,
    /// `│` sides, all in `Scene::border`'s style; the title embedded over
    /// the top border starting at column `rect.x + 1`; then `content`
    /// blitted verbatim (raw cell-for-cell, character AND style, no
    /// recoloring) into the interior at the doc's insets -- 2 cells
    /// horizontal (`rect.x + 2`, usable width `rect.w - 4`), 1 row vertical
    /// (`rect.y + 1`, usable height `rect.h - 2`).
    /// `rect = (0,1,10,5)`: top border row 1, bottom border row 5, side
    /// borders columns 0 and 9 on rows 2-4; interior columns 2-7 (10-4 = 6
    /// wide), rows 2-4 (5-2 = 3 tall). `content` is `3 x 2` -- smaller than
    /// the 6x3 interior, so only interior columns 2-4, rows 2-3 are
    /// touched; the rest (e.g. (5,2)), and the 1-cell padding column
    /// between border and content (x=1), stay whatever the panel's
    /// full-clear left there (blank, default style).
    #[test]
    fn overlay_preview_blits_grid_cells() {
        let g = grid_with(10, 6, b"");
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
            size: (10, 6),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 10, h: 6 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border,
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: Vec::new(),
                top: 0,
                preview: Some(PreviewBlock { rect: Rect { x: 0, y: 1, w: 10, h: 5 }, title: "foo".to_string(), content_w: 3, content_h: 2, content }),
                list_height: 1,
            })),
        };
        let mut r = Renderer::new(10, 6);
        r.compose_back(&scene);

        // Top border row (y=1): '┌' corner, title "foo" from column 1, '─'
        // fill through column 8, '┐' at column 9 -- all in `border` style.
        assert_eq!(r.back_cell(0, 1).ch, '┌');
        assert_eq!(r.back_cell(0, 1).style, border);
        assert_eq!(r.back_cell(1, 1).ch, 'f');
        assert_eq!(r.back_cell(2, 1).ch, 'o');
        assert_eq!(r.back_cell(3, 1).ch, 'o');
        assert_eq!(r.back_cell(1, 1).style, border);
        assert_eq!(r.back_cell(4, 1).ch, '─');
        assert_eq!(r.back_cell(8, 1).ch, '─');
        assert_eq!(r.back_cell(9, 1).ch, '┐');
        assert_eq!(r.back_cell(9, 1).style, border);

        // Side borders: '│' at columns 0 and 9 on every row strictly
        // between the top (1) and bottom (5) border rows.
        for y in 2..=4u16 {
            assert_eq!(r.back_cell(0, y).ch, '│', "left border at y={y}");
            assert_eq!(r.back_cell(0, y).style, border);
            assert_eq!(r.back_cell(9, y).ch, '│', "right border at y={y}");
            assert_eq!(r.back_cell(9, y).style, border);
        }

        // Bottom border row (y=5): '└' + '─' fill + '┘'.
        assert_eq!(r.back_cell(0, 5).ch, '└');
        assert_eq!(r.back_cell(0, 5).style, border);
        assert_eq!(r.back_cell(4, 5).ch, '─');
        assert_eq!(r.back_cell(9, 5).ch, '┘');
        assert_eq!(r.back_cell(9, 5).style, border);

        // Interior row 0 (y = rect.y + 1 = 2, x from rect.x + 2 = 2):
        // content[0..3] verbatim; the padding column between border and
        // content (x=1) and the interior past the content (x=5) stay
        // untouched (blank, default style).
        assert_eq!(r.back_cell(1, 2).ch, ' ');
        assert_eq!(r.back_cell(1, 2).style, Style::default());
        assert_eq!(r.back_cell(2, 2), Cell { ch: 'A', style: s1 });
        assert_eq!(r.back_cell(3, 2), Cell { ch: 'B', style: s1 });
        assert_eq!(r.back_cell(4, 2), Cell { ch: 'C', style: s1 });
        assert_eq!(r.back_cell(5, 2).ch, ' ');
        assert_eq!(r.back_cell(5, 2).style, Style::default());

        // Interior row 1 (y=3): content[3..6] verbatim.
        assert_eq!(r.back_cell(2, 3), Cell { ch: 'D', style: s2 });
        assert_eq!(r.back_cell(3, 3), Cell { ch: 'E', style: s2 });
        assert_eq!(r.back_cell(4, 3), Cell { ch: 'F', style: s2 });
    }

    /// `content` (5 wide x 4 tall) is larger than the boxed preview's
    /// interior (`rect = (0,0,7,4)` -> interior is `rect.w - 4 = 3` wide by
    /// `rect.h - 2 = 2` tall, at inset `(2, 1)` per `## 3.2`): the renderer
    /// TRUNCATES to the top-left `3x2` corner rather than scaling --
    /// columns 3-4 (`'D'`/`'E'`, `'I'`/`'J'` of content's first two rows)
    /// and content rows 2-3 (`"KLMNO"`/`"PQRST"`) never appear anywhere on
    /// screen (row 3 is the box's own bottom border).
    #[test]
    fn overlay_preview_truncates_oversized_grid() {
        let g = grid_with(7, 4, b"");
        let content: Vec<Cell> = "ABCDEFGHIJKLMNOPQRST".chars().map(|ch| Cell { ch, style: Style::default() }).collect();
        let scene = Scene {
            size: (7, 4),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 7, h: 4 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::List(ListOverlay {
                title: String::new(),
                rows: Vec::new(),
                top: 0,
                preview: Some(PreviewBlock { rect: Rect { x: 0, y: 0, w: 7, h: 4 }, title: String::new(), content_w: 5, content_h: 4, content }),
                list_height: 0,
            })),
        };
        let mut r = Renderer::new(7, 4);
        r.compose_back(&scene);

        // Box chrome: top border row 0, bottom border row 3, sides at
        // columns 0 and 6.
        assert_eq!(r.back_cell(0, 0).ch, '┌');
        assert_eq!(r.back_cell(3, 0).ch, '─');
        assert_eq!(r.back_cell(6, 0).ch, '┐');
        assert_eq!(r.back_cell(0, 1).ch, '│');
        assert_eq!(r.back_cell(6, 2).ch, '│');
        assert_eq!(r.back_cell(0, 3).ch, '└');
        assert_eq!(r.back_cell(6, 3).ch, '┘');

        // Interior row 0 (y=1, x=2..4): only the first 3 of content's
        // 5-wide row 0 ("ABCDE") appear -- 'D'/'E' truncated. The padding
        // column (x=1) and the cell right of the copy (x=5) stay blank:
        // truncation, not overflow.
        let row1: String = (2..5).map(|x| r.back_cell(x, 1).ch).collect();
        assert_eq!(row1, "ABC");
        assert_eq!(r.back_cell(1, 1).ch, ' ');
        assert_eq!(r.back_cell(5, 1).ch, ' ');

        // Interior row 1 (y=2): content row 1 ("FGHIJ") truncated to "FGH".
        // Row 3 is the bottom border -- content rows 2-3 ("KLMNO"/"PQRST")
        // are structurally unreachable, proving the renderer never scales
        // down to fit them in.
        let row2: String = (2..5).map(|x| r.back_cell(x, 2).ch).collect();
        assert_eq!(row2, "FGH");
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
        let rows: Vec<TreeRowCell> =
            (0..15).map(|i| TreeRowCell { text: format!("r{i}"), depth: 0, marker: None, selected: false, tagged: false }).collect();
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
            border_indicators: BorderIndicators::Colour,
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
                list_height: 10,
            })),
        };
        let mut r = Renderer::new(20, 15);
        r.compose_back(&scene);

        // Rows 0-9: list text "    r0".."    r9" (marker slot + tag slot +
        // text, SP7 Task 15) -- all 10 rows the 2/3 split allots.
        for i in 0..10u16 {
            let line: String = (0..6).map(|x| r.back_cell(x, i).ch).collect();
            assert_eq!(line, format!("    r{i}"), "row {i} should still be a list row");
        }
        // Row 10 is the preview box's top border (its '┌' corner at column
        // 0, fix round 1), NOT list row 10's text -- proof the list was
        // capped to 10 rows (2/3 of 15), not the full 15. Row 14 (the
        // panel's last row) is the box's bottom border.
        assert_eq!(r.back_cell(0, 10).ch, '┌');
        assert_eq!(r.back_cell(1, 10).ch, '─');
        assert_eq!(r.back_cell(0, 14).ch, '└');
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
            border_indicators: BorderIndicators::Colour,
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
            border_indicators: BorderIndicators::Colour,
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

    /// clock-mode's (Task 10) big-digit block rendering, `text = "12:34"`
    /// (5 chars) in a rect sized exactly to the big-digit-mode threshold:
    /// `rect.w == 6 * 5 == 30`, `rect.h == 6`. Per `paint_clock`'s doc
    /// comment: `ox = 0 + 30/2 - 3*5 = 0`, `oy = 0 + 6/2 - 3 = 0`, glyph
    /// pitch 6 columns, so glyph `i` starts at column `i*6`.
    ///
    /// Glyph 0 ('1', `gx=0`) per `digit_bitmap(1)` = `["..#..", ".##..",
    /// "..#..", "..#..", "#####"]` -> on-cells `(2,0) (1,1) (2,1) (2,2)
    /// (2,3) (0,4) (1,4) (2,4) (3,4) (4,4)`.
    /// Glyph 2 (':', `gx=12`) per `clock_glyph_bitmap(':')` = `[".....",
    /// "..#..", ".....", "..#..", "....."]` -> on-cells `(14,1) (14,3)`
    /// (column `12+2`).
    /// All "on" cells are a blank space char in `colour` used as BOTH fg
    /// AND bg (a solid block -- `bg` is what actually renders it, per the
    /// doc's "both fg and bg set to the clock colour" rule for a
    /// space-glyph block).
    #[test]
    fn clock_overlay_draws_big_digits() {
        let g = grid_with(30, 6, b"");
        let blue = Color::Idx(4);
        let scene = Scene {
            size: (30, 6),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 30, h: 6 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::Clock(Rect { x: 0, y: 0, w: 30, h: 6 }, "12:34".to_string(), blue)),
        };
        let mut r = Renderer::new(30, 6);
        r.compose_back(&scene);

        let digit_one_on: &[(u16, u16)] = &[(2, 0), (1, 1), (2, 1), (2, 2), (2, 3), (0, 4), (1, 4), (2, 4), (3, 4), (4, 4)];
        for &(x, y) in digit_one_on {
            assert_eq!(r.back_cell(x, y).ch, ' ', "cell ({x},{y}) should be an 'on' block of the '1' glyph");
            assert_eq!(r.back_cell(x, y).style.bg, blue, "cell ({x},{y}) should be clock-mode-colour");
        }
        let colon_on: &[(u16, u16)] = &[(14, 1), (14, 3)];
        for &(x, y) in colon_on {
            assert_eq!(r.back_cell(x, y).ch, ' ', "cell ({x},{y}) should be an 'on' block of the ':' glyph");
            assert_eq!(r.back_cell(x, y).style.bg, blue, "cell ({x},{y}) should be clock-mode-colour");
        }
        // An "off" cell inside the '1' glyph's bounding box, and a cell
        // outside every glyph's pitch entirely, are both left at the
        // rect-clear default (blank, default style) -- not the block colour.
        assert_eq!(r.back_cell(0, 0).ch, ' ');
        assert_eq!(r.back_cell(0, 0).style, Style::default());
        assert_eq!(r.back_cell(29, 5).style, Style::default());
    }

    /// A rect too small for the big-digit threshold (below `6 * len` wide or
    /// `6` tall) falls back to the plain time string centered on one row, fg
    /// only (not a filled block) -- `docs/tmux-reference/status-line-and-
    /// messages.md` `## 6. Clock mode`'s documented fallback.
    #[test]
    fn clock_overlay_small_fallback_plain_text() {
        let g = grid_with(7, 3, b"");
        let blue = Color::Idx(4);
        let scene = Scene {
            size: (7, 3),
            panes: vec![PaneView { id: 1, rect: Rect { x: 0, y: 0, w: 7, h: 3 }, grid: &g, focused: true, dead: false, copy: None }],
            zoomed: false,
            status: None,
            message: None,
            border: Style::default(),
            border_active: green_active(),
            mode_style: Style::default(),
            display_panes_colour: Style::default(),
            display_panes_active_colour: Style::default(),
            border_indicators: BorderIndicators::Colour,
            overlay: Some(Overlay::Clock(Rect { x: 0, y: 0, w: 7, h: 3 }, "12:34".to_string(), blue)),
        };
        let mut r = Renderer::new(7, 3);
        r.compose_back(&scene);
        // ox = 0 + (7-5)/2 = 1, oy = 0 + 3/2 = 1
        let expected = "12:34";
        for (i, ch) in expected.chars().enumerate() {
            let cell = r.back_cell(1 + i as u16, 1);
            assert_eq!(cell.ch, ch, "column {i} of the fallback row");
            assert_eq!(cell.style.fg, blue, "fallback text is fg-only clock-mode-colour");
            assert_eq!(cell.style.bg, Color::Default, "fallback text must NOT be a filled block");
        }
        // Rect was still cleared first: an untouched cell above the text row.
        assert_eq!(r.back_cell(0, 0).ch, ' ');
        assert_eq!(r.back_cell(0, 0).style, Style::default());
    }
}
