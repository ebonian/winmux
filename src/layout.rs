//! Split-tree layout (pure logic, no I/O).

use crate::geom::{Direction, Rect};

pub type PaneId = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    /// tmux `%`: children side-by-side (left | right); the split line is vertical.
    Horizontal,
    /// tmux `"`: children stacked (top / bottom); the split line is horizontal.
    Vertical,
}

/// tmux's PANE_MINIMUM: a pane must be at least this many cells in each axis.
pub const MIN_PANE_W: u16 = 2;
pub const MIN_PANE_H: u16 = 2;

#[derive(Debug, PartialEq, Eq)]
pub struct SplitRefused;

enum Node {
    Leaf(PaneId),
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

pub struct Layout {
    root: Node,
    focused: PaneId,
    last_focused: Option<PaneId>,
    zoomed: bool,
}

// ---- pure geometry helpers -------------------------------------------------

/// First child's length along the split axis, per the contract formula:
/// round((L - 1) * ratio), computed totally: saturates at 0 for L == 0 and
/// the result is clamped to the available length, so it never exceeds L - 1.
fn child_first(l: u16, ratio: f32) -> u16 {
    let avail = l.saturating_sub(1);
    ((avail as f32) * ratio).round().min(avail as f32) as u16
}

/// The two child rects of a split, EXCLUDING the single border row/column.
/// Total: never panics; on areas too small to hold both children plus the
/// border, the children degrade to zero-size rects (downstream minimum
/// checks handle those).
fn split_rects(dir: SplitDir, ratio: f32, area: Rect) -> (Rect, Rect) {
    match dir {
        SplitDir::Horizontal => {
            let c1 = child_first(area.w, ratio);
            let c2 = area.w.saturating_sub(1).saturating_sub(c1);
            (
                Rect { x: area.x, y: area.y, w: c1, h: area.h },
                Rect { x: area.x + c1 + 1, y: area.y, w: c2, h: area.h },
            )
        }
        SplitDir::Vertical => {
            let c1 = child_first(area.h, ratio);
            let c2 = area.h.saturating_sub(1).saturating_sub(c1);
            (
                Rect { x: area.x, y: area.y, w: area.w, h: c1 },
                Rect { x: area.x, y: area.y + c1 + 1, w: area.w, h: c2 },
            )
        }
    }
}

fn rects_of(node: &Node, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        Node::Leaf(pid) => out.push((*pid, area)),
        Node::Split { dir, ratio, first, second } => {
            let (r1, r2) = split_rects(*dir, *ratio, area);
            rects_of(first, r1, out);
            rects_of(second, r2, out);
        }
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(pid) => out.push(*pid),
        Node::Split { first, second, .. } => {
            collect_leaves(first, out);
            collect_leaves(second, out);
        }
    }
}

/// Replace the `Leaf(id)` node in the tree with `replacement` (consumed once).
fn replace_leaf(node: &mut Node, id: PaneId, replacement: &mut Option<Node>) {
    match node {
        Node::Leaf(pid) if *pid == id => {
            if let Some(r) = replacement.take() {
                *node = r;
            }
        }
        Node::Leaf(_) => {}
        Node::Split { first, second, .. } => {
            replace_leaf(first, id, replacement);
            if replacement.is_some() {
                replace_leaf(second, id, replacement);
            }
        }
    }
}

// ---- public API ------------------------------------------------------------

impl Layout {
    pub fn new(first: PaneId) -> Self {
        Layout {
            root: Node::Leaf(first),
            focused: first,
            last_focused: None,
            zoomed: false,
        }
    }

    /// Split the focused pane. The new pane takes the second half (right for
    /// Horizontal, bottom for Vertical) and RECEIVES FOCUS (tmux default).
    /// Returns Err(SplitRefused) if either resulting pane would fall below
    /// MIN_PANE_W/MIN_PANE_H given `area`. Splitting clears zoom first.
    pub fn split(&mut self, dir: SplitDir, new_pane: PaneId, area: Rect)
        -> Result<(), SplitRefused>
    {
        // Rect of the focused pane within `area` (unzoomed geometry).
        let fr = self
            .all_rects(area)
            .into_iter()
            .find(|(id, _)| *id == self.focused)
            .map(|(_, r)| r)
            .ok_or(SplitRefused)?;

        // Guard the split axis so child_first cannot underflow (needs L >= 1).
        let axis = match dir {
            SplitDir::Horizontal => fr.w,
            SplitDir::Vertical => fr.h,
        };
        if axis < 2 {
            return Err(SplitRefused);
        }

        let (r1, r2) = split_rects(dir, 0.5, fr);
        if r1.w < MIN_PANE_W
            || r1.h < MIN_PANE_H
            || r2.w < MIN_PANE_W
            || r2.h < MIN_PANE_H
        {
            return Err(SplitRefused);
        }

        self.zoomed = false;
        let focused = self.focused;
        let mut replacement = Some(Node::Split {
            dir,
            ratio: 0.5,
            first: Box::new(Node::Leaf(focused)),
            second: Box::new(Node::Leaf(new_pane)),
        });
        replace_leaf(&mut self.root, focused, &mut replacement);
        self.last_focused = Some(focused);
        self.focused = new_pane;
        Ok(())
    }

    pub fn focused(&self) -> PaneId {
        self.focused
    }

    /// Compute pane rectangles within `area`. Exactly ONE border row/column
    /// separates siblings; rects EXCLUDE border cells. When zoomed, returns
    /// only [(focused, area)].
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        if self.zoomed {
            return vec![(self.focused, area)];
        }
        self.all_rects(area)
    }

    /// All pane ids in leaf (tree, left-to-right) order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        collect_leaves(&self.root, &mut v);
        v
    }

    // is_empty is not part of the locked contract; a Layout is never empty.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.panes().len()
    }

    /// Rects ignoring zoom (internal to layout logic).
    fn all_rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        rects_of(&self.root, area, &mut out);
        out
    }
}

// ---- tree helpers (Task 3) ---------------------------------------------

/// First (left-to-right) leaf id of a subtree.
fn first_leaf(node: &Node) -> PaneId {
    match node {
        Node::Leaf(pid) => *pid,
        Node::Split { first, .. } => first_leaf(first),
    }
}

fn leaf_is(node: &Node, id: PaneId) -> bool {
    matches!(node, Node::Leaf(pid) if *pid == id)
}

/// Remove `id` from the tree. Returns the rebuilt tree and, when a removal
/// happened, `Some(fallback)` where `fallback` is the first leaf of the
/// sibling subtree that absorbed the space (the focus fallback).
fn remove_from(node: Node, id: PaneId) -> (Node, Option<PaneId>) {
    match node {
        Node::Leaf(pid) => (Node::Leaf(pid), None),
        Node::Split { dir, ratio, first, second } => {
            if leaf_is(&first, id) {
                let fallback = first_leaf(&second);
                return (*second, Some(fallback));
            }
            if leaf_is(&second, id) {
                let fallback = first_leaf(&first);
                return (*first, Some(fallback));
            }
            let (nf, rf) = remove_from(*first, id);
            if let Some(fallback) = rf {
                return (
                    Node::Split { dir, ratio, first: Box::new(nf), second },
                    Some(fallback),
                );
            }
            let (ns, rs) = remove_from(*second, id);
            (
                Node::Split { dir, ratio, first: Box::new(nf), second: Box::new(ns) },
                rs,
            )
        }
    }
}

/// Walk `path` (false = first child, true = second) from `root`, returning
/// the node reached.
fn node_at<'a>(root: &'a Node, path: &[bool]) -> &'a Node {
    let mut n = root;
    for &b in path {
        match n {
            Node::Split { first, second, .. } => {
                n = if b { &**second } else { &**first };
            }
            Node::Leaf(_) => break,
        }
    }
    n
}

fn node_at_mut<'a>(root: &'a mut Node, path: &[bool]) -> &'a mut Node {
    let mut n = root;
    for &b in path {
        match n {
            Node::Split { first, second, .. } => {
                n = if b { &mut **second } else { &mut **first };
            }
            Node::Leaf(_) => break,
        }
    }
    n
}

/// Area occupied by the node reached by following `path` from `root`.
fn area_at(root: &Node, path: &[bool], area: Rect) -> Rect {
    let mut n = root;
    let mut a = area;
    for &b in path {
        match n {
            Node::Split { dir, ratio, first, second } => {
                let (r1, r2) = split_rects(*dir, *ratio, a);
                if b {
                    n = &**second;
                    a = r2;
                } else {
                    n = &**first;
                    a = r1;
                }
            }
            Node::Leaf(_) => break,
        }
    }
    a
}

// ---- public API (Task 3) -------------------------------------------------

impl Layout {
    /// Geometric navigation: move focus to the pane adjacent in `dir` (the
    /// pane whose rect borders the focused rect in that direction, picking the
    /// one overlapping the focused pane's cross-axis midpoint). Returns false
    /// (no change) if there is no pane in that direction.
    pub fn focus_dir(&mut self, dir: Direction, area: Rect) -> bool {
        let rects = self.all_rects(area);
        let f = match rects.iter().find(|(id, _)| *id == self.focused) {
            Some((_, r)) => *r,
            None => return false,
        };
        let mut chosen: Option<PaneId> = None;
        for (id, r) in &rects {
            if *id == self.focused {
                continue;
            }
            // Adjacency accounts for the single border cell between siblings.
            let adjacent = match dir {
                // saturating_add: `f.x + f.w + 1` (and the symmetric Down
                // case below) is theoretically reachable near u16::MAX;
                // saturating keeps this a total comparison instead of a
                // debug-mode overflow panic (follow-up #5).
                Direction::Right => r.x == f.x.saturating_add(f.w).saturating_add(1),
                Direction::Left => f.x > 0 && r.x + r.w == f.x - 1,
                Direction::Down => r.y == f.y.saturating_add(f.h).saturating_add(1),
                Direction::Up => f.y > 0 && r.y + r.h == f.y - 1,
            };
            if !adjacent {
                continue;
            }
            let overlaps = match dir {
                Direction::Left | Direction::Right => {
                    let mid = f.y + f.h / 2;
                    r.y <= mid && mid < r.y + r.h
                }
                Direction::Up | Direction::Down => {
                    let mid = f.x + f.w / 2;
                    r.x <= mid && mid < r.x + r.w
                }
            };
            if overlaps {
                chosen = Some(*id);
                break;
            }
        }
        match chosen {
            Some(id) => {
                self.set_focus(id);
                true
            }
            None => false,
        }
    }

    /// Cycle focus to the next pane in leaf (tree, left-to-right) order,
    /// wrapping.
    pub fn focus_next(&mut self) {
        let panes = self.panes();
        if let Some(idx) = panes.iter().position(|&p| p == self.focused) {
            let next = panes[(idx + 1) % panes.len()];
            self.set_focus(next);
        }
    }

    /// Toggle focus to the previously-focused pane, if it still exists.
    pub fn focus_last(&mut self) {
        if let Some(last) = self.last_focused {
            if self.panes().contains(&last) {
                let current = self.focused;
                self.focused = last;
                self.last_focused = Some(current);
            }
        }
    }

    /// Remove pane `id`. Its sibling subtree absorbs the space. If the focused
    /// pane was removed, focus moves to the nearest remaining leaf of the
    /// sibling subtree. Clears zoom. Returns false (tree unchanged) if `id`
    /// is the only pane — the caller exits the app instead.
    pub fn remove(&mut self, id: PaneId) -> bool {
        if self.len() == 1 {
            return false;
        }
        let root = std::mem::replace(&mut self.root, Node::Leaf(0));
        let (new_root, removed) = remove_from(root, id);
        self.root = new_root;
        match removed {
            Some(fallback) => {
                self.zoomed = false;
                if self.focused == id {
                    self.focused = fallback;
                }
                if self.last_focused == Some(id) {
                    self.last_focused = None;
                }
                true
            }
            None => false,
        }
    }

    /// Move the focused pane's nearest enclosing split edge in `dir` by
    /// `cells` cells. Clamped so no pane violates minimums within `area`.
    /// Returns false if nothing changed. Thin wrapper over [`Self::resize_from`]
    /// using the currently focused pane as the reference leaf.
    pub fn resize_focused(&mut self, dir: Direction, area: Rect, cells: u16) -> bool {
        self.resize_from(self.focused, dir, area, cells)
    }

    /// Move `pane`'s nearest enclosing split edge (in `dir`'s orientation, on
    /// the side `pane` sits on) by `cells` cells. Clamped so no pane violates
    /// minimums within `area`. Returns false if nothing changed, or if `pane`
    /// isn't one of this layout's leaves. Generalizes [`Self::resize_focused`]
    /// to an arbitrary reference pane (Task 5, sub-project 4: mouse
    /// border-drag resize needs to move the split adjacent to whichever pane
    /// borders the dragged cell, independent of which pane currently has
    /// keyboard focus — unlike `resize_focused`, this never changes focus).
    pub fn resize_from(&mut self, pane: PaneId, dir: Direction, area: Rect, cells: u16) -> bool {
        let orient = match dir {
            Direction::Left | Direction::Right => SplitDir::Horizontal,
            Direction::Up | Direction::Down => SplitDir::Vertical,
        };
        // Right/Down grow the split's FIRST child (so `pane` must live in the
        // first child); Left/Up grow the SECOND child.
        let want_first = matches!(dir, Direction::Right | Direction::Down);

        let path = match self.path_to(pane) {
            Some(p) => p,
            None => return false,
        };

        // Deepest ancestor split of matching orientation on the correct side.
        // At depth i the chosen child is path[i]; focus-in-first-child means
        // path[i] == false, so the required bit is `!want_first`.
        let mut target: Option<usize> = None;
        {
            let mut node = &self.root;
            for (i, &step) in path.iter().enumerate() {
                if let Node::Split { dir: sd, first, second, .. } = node {
                    if *sd == orient && step != want_first {
                        target = Some(i);
                    }
                    node = if step { &**second } else { &**first };
                } else {
                    break;
                }
            }
        }
        let i = match target {
            Some(i) => i,
            None => return false, // at the edge: no matching ancestor
        };
        let prefix: Vec<bool> = path[..i].to_vec();
        let split_area = area_at(&self.root, &prefix, area);

        let (l, min) = match orient {
            SplitDir::Horizontal => (split_area.w, MIN_PANE_W),
            SplitDir::Vertical => (split_area.h, MIN_PANE_H),
        };
        // Need room for two panes plus the border, else nothing can move.
        if l < 2 * min + 1 {
            return false;
        }

        let ratio_old = match node_at(&self.root, &prefix) {
            Node::Split { ratio, .. } => *ratio,
            Node::Leaf(_) => return false,
        };
        let child1 = child_first(l, ratio_old) as i32;
        let sign: i32 = if want_first { 1 } else { -1 };
        let lo = min as i32;
        let hi = (l as i32 - 1) - min as i32;
        let mut c = (child1 + sign * cells as i32).clamp(lo, hi);
        if c == child1 {
            return false; // clamped straight back to where we started
        }
        // Apply, verifying full-tree minimums (nested splits shrink with their
        // parent); if a nested pane would be violated, step back toward the
        // original until valid, or give up unchanged.
        let step: i32 = if c > child1 { -1 } else { 1 };
        loop {
            let ratio = c as f32 / (l as f32 - 1.0);
            self.set_ratio(&prefix, ratio);
            if self.all_min_ok(area) {
                return true;
            }
            if c == child1 {
                self.set_ratio(&prefix, ratio_old);
                return false;
            }
            c += step;
        }
    }

    /// Toggle zoom on the focused pane. Zoom auto-clears on split/remove.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Move focus directly to `id` (SP3 Task 6: `select-pane -t <pane>` and
    /// other pane-targeted commands need to focus an arbitrary pane by id,
    /// not just a relative direction/next/last). `false` (no-op) if `id`
    /// isn't one of this layout's leaves.
    pub fn focus_pane(&mut self, id: PaneId) -> bool {
        if !self.panes().contains(&id) {
            return false;
        }
        self.set_focus(id);
        true
    }

    // ---- internal helpers (Task 3) ----

    /// Change focus, recording the previous focus as last-focused.
    fn set_focus(&mut self, id: PaneId) {
        if id != self.focused {
            self.last_focused = Some(self.focused);
            self.focused = id;
        }
    }

    /// Path (false = first child, true = second) from the root to `id`'s
    /// leaf, if present.
    fn path_to(&self, id: PaneId) -> Option<Vec<bool>> {
        fn go(node: &Node, id: PaneId, acc: &mut Vec<bool>) -> bool {
            match node {
                Node::Leaf(pid) => *pid == id,
                Node::Split { first, second, .. } => {
                    acc.push(false);
                    if go(first, id, acc) {
                        return true;
                    }
                    acc.pop();
                    acc.push(true);
                    if go(second, id, acc) {
                        return true;
                    }
                    acc.pop();
                    false
                }
            }
        }
        let mut acc = Vec::new();
        if go(&self.root, id, &mut acc) {
            Some(acc)
        } else {
            None
        }
    }

    /// Set the ratio of the split node reached by `path`.
    fn set_ratio(&mut self, path: &[bool], v: f32) {
        if let Node::Split { ratio, .. } = node_at_mut(&mut self.root, path) {
            *ratio = v;
        }
    }

    /// True if every pane's rect meets the minimums within `area`.
    fn all_min_ok(&self, area: Rect) -> bool {
        self.all_rects(area)
            .iter()
            .all(|(_, r)| r.w >= MIN_PANE_W && r.h >= MIN_PANE_H)
    }
}

// ---- layout presets, swap-pane, rotate-window (Task 6, sub-project 4) -----

/// tmux's five classic preset layouts (`select-layout`/`next-layout`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutPreset {
    EvenHorizontal,
    EvenVertical,
    MainHorizontal,
    MainVertical,
    Tiled,
}

/// `next-layout`'s cycle order, and the canonical index (0..=4) stored in
/// `Window::last_layout` to remember cycle position across calls.
pub const PRESET_CYCLE: [LayoutPreset; 5] = [
    LayoutPreset::EvenHorizontal,
    LayoutPreset::EvenVertical,
    LayoutPreset::MainHorizontal,
    LayoutPreset::MainVertical,
    LayoutPreset::Tiled,
];

impl LayoutPreset {
    /// The tmux `select-layout <name>` spelling.
    pub fn name(self) -> &'static str {
        match self {
            LayoutPreset::EvenHorizontal => "even-horizontal",
            LayoutPreset::EvenVertical => "even-vertical",
            LayoutPreset::MainHorizontal => "main-horizontal",
            LayoutPreset::MainVertical => "main-vertical",
            LayoutPreset::Tiled => "tiled",
        }
    }

    /// Parse one of the five exact tmux layout names. `None` for anything
    /// else (including abbreviations -- SP4 scope, documented: real tmux
    /// accepts unambiguous prefixes too).
    pub fn from_name(s: &str) -> Option<LayoutPreset> {
        PRESET_CYCLE.iter().copied().find(|p| p.name() == s)
    }

    /// Position in [`PRESET_CYCLE`] (0..=4) -- what `Window::last_layout`
    /// stores.
    pub fn cycle_index(self) -> u8 {
        PRESET_CYCLE
            .iter()
            .position(|&p| p == self)
            .expect("every LayoutPreset variant appears in PRESET_CYCLE") as u8
    }
}

/// First child's target length converted to the `child_first`/`split_rects`
/// ratio representation: solves `ratio` such that `child_first(area_len,
/// ratio) == target_first` exactly (for any `target_first <= area_len - 1`,
/// which every preset builder below guarantees by construction). `area_len ==
/// 0` degenerates to ratio 0.0 (irrelevant -- `child_first` also saturates to
/// 0 in that case).
fn ratio_for(target_first: u16, area_len: u16) -> f32 {
    let avail = area_len.saturating_sub(1);
    if avail == 0 {
        0.0
    } else {
        target_first as f32 / avail as f32
    }
}

/// Split `area_len` cells into `count` lengths separated by `count - 1`
/// single-cell borders, as evenly as possible. tmux's rounding rule: any
/// remainder cell goes to the EARLIER (leftmost/topmost) entries first. Total
/// function: saturates to all-zero lengths on an `area_len` too small to fit
/// even the borders (legal degenerate output, same convention as the rest of
/// this module).
fn even_lengths(area_len: u16, count: usize) -> Vec<u16> {
    if count == 0 {
        return Vec::new();
    }
    let borders = (count as u16).saturating_sub(1);
    let usable = area_len.saturating_sub(borders);
    let base = usable / count as u16;
    let rem = usable % count as u16;
    (0..count).map(|i| base + if (i as u16) < rem { 1 } else { 0 }).collect()
}

/// Chain `nodes` into a left-leaning Horizontal split tree so that, laid out
/// against an area whose width is `widths.iter().sum() + widths.len() - 1`
/// (one border per adjacent pair), each `nodes[i]` lands at exactly
/// `widths[i]` wide. `nodes.len() == widths.len()`, both `>= 1` (the caller
/// handles the single-pane case, which never calls this).
fn stack_horizontal(mut nodes: Vec<Node>, widths: &[u16]) -> Node {
    debug_assert_eq!(nodes.len(), widths.len());
    if nodes.len() == 1 {
        return nodes.pop().expect("len == 1 checked above");
    }
    let l: u16 = widths.iter().fold(0u16, |acc, &w| acc.saturating_add(w)).saturating_add(widths.len() as u16 - 1);
    let ratio = ratio_for(widths[0], l);
    let first = nodes.remove(0);
    let second = stack_horizontal(nodes, &widths[1..]);
    Node::Split { dir: SplitDir::Horizontal, ratio, first: Box::new(first), second: Box::new(second) }
}

/// Vertical-axis mirror of [`stack_horizontal`].
fn stack_vertical(mut nodes: Vec<Node>, heights: &[u16]) -> Node {
    debug_assert_eq!(nodes.len(), heights.len());
    if nodes.len() == 1 {
        return nodes.pop().expect("len == 1 checked above");
    }
    let l: u16 = heights.iter().fold(0u16, |acc, &h| acc.saturating_add(h)).saturating_add(heights.len() as u16 - 1);
    let ratio = ratio_for(heights[0], l);
    let first = nodes.remove(0);
    let second = stack_vertical(nodes, &heights[1..]);
    Node::Split { dir: SplitDir::Vertical, ratio, first: Box::new(first), second: Box::new(second) }
}

/// Clamp a requested `main-pane-width`/`main-pane-height` value so the main
/// pane is at least `min` and the space left for the other panes (`total -
/// requested - 1` border) is also at least `min`. When `total` is too small
/// to satisfy both minimums plus a border at all, degrades to `requested`
/// capped at `total - 1` (may legally violate MIN -- same tiny-area tolerance
/// as the rest of this module; there is no valid split otherwise).
fn clamp_main(requested: u16, total: u16, min: u16) -> u16 {
    if total < 2 * min + 1 {
        return requested.min(total.saturating_sub(1));
    }
    let max_main = total - 1 - min;
    requested.clamp(min, max_main)
}

/// tmux's `tiled` rows-first grid dimensions: grow rows first, then columns,
/// alternately, until `rows * cols >= n`.
fn tiled_dims(n: usize) -> (usize, usize) {
    let mut r = 1usize;
    let mut c = 1usize;
    while r * c < n {
        r += 1;
        if r * c < n {
            c += 1;
        }
    }
    (r, c)
}

/// Build the preset's split tree for exactly `panes` (already in the order
/// the caller wants used -- see [`Layout::apply_preset`]'s doc comment for
/// which order that is) against `area`, `main_width`/`main_height` being the
/// `main-pane-width`/`main-pane-height` option values (clamped internally).
fn build_preset_tree(preset: LayoutPreset, panes: &[PaneId], area: Rect, main_width: u16, main_height: u16) -> Node {
    if panes.len() == 1 {
        return Node::Leaf(panes[0]);
    }
    match preset {
        LayoutPreset::EvenHorizontal => {
            let widths = even_lengths(area.w, panes.len());
            stack_horizontal(panes.iter().map(|&p| Node::Leaf(p)).collect(), &widths)
        }
        LayoutPreset::EvenVertical => {
            let heights = even_lengths(area.h, panes.len());
            stack_vertical(panes.iter().map(|&p| Node::Leaf(p)).collect(), &heights)
        }
        LayoutPreset::MainHorizontal => {
            let main_id = panes[0];
            let others = &panes[1..];
            let main_h = clamp_main(main_height, area.h, MIN_PANE_H);
            let ratio = ratio_for(main_h, area.h);
            let widths = even_lengths(area.w, others.len());
            let bottom = stack_horizontal(others.iter().map(|&p| Node::Leaf(p)).collect(), &widths);
            Node::Split { dir: SplitDir::Vertical, ratio, first: Box::new(Node::Leaf(main_id)), second: Box::new(bottom) }
        }
        LayoutPreset::MainVertical => {
            let main_id = panes[0];
            let others = &panes[1..];
            let main_w = clamp_main(main_width, area.w, MIN_PANE_W);
            let ratio = ratio_for(main_w, area.w);
            let heights = even_lengths(area.h, others.len());
            let right = stack_vertical(others.iter().map(|&p| Node::Leaf(p)).collect(), &heights);
            Node::Split { dir: SplitDir::Horizontal, ratio, first: Box::new(Node::Leaf(main_id)), second: Box::new(right) }
        }
        LayoutPreset::Tiled => {
            let n = panes.len();
            let (_rows, cols) = tiled_dims(n);
            let rows_used = n.div_ceil(cols);
            let heights = even_lengths(area.h, rows_used);
            let mut row_nodes = Vec::with_capacity(rows_used);
            let mut idx = 0;
            for _ in 0..rows_used {
                let take = cols.min(n - idx);
                let row_panes = &panes[idx..idx + take];
                idx += take;
                // Last short row spans: its panes are spread evenly over the
                // FULL area width using only ITS OWN pane count, not `cols`
                // -- so a shorter final row's panes end up wider than a
                // normal row's, the last one absorbing the missing columns'
                // worth of space (tmux's "last short row spans" rule).
                let widths = even_lengths(area.w, row_panes.len());
                row_nodes.push(stack_horizontal(row_panes.iter().map(|&p| Node::Leaf(p)).collect(), &widths));
            }
            stack_vertical(row_nodes, &heights)
        }
    }
}

/// Overwrite every `Leaf`'s stored id, in leaf (tree, left-to-right) order,
/// from `ids` (`ids.len()` must equal the tree's leaf count).
fn assign_leaf_values_in_order(node: &mut Node, ids: &[PaneId], next: &mut usize) {
    match node {
        Node::Leaf(id) => {
            *id = ids[*next];
            *next += 1;
        }
        Node::Split { first, second, .. } => {
            assign_leaf_values_in_order(first, ids, next);
            assign_leaf_values_in_order(second, ids, next);
        }
    }
}

fn swap_leaf_values(node: &mut Node, a: PaneId, b: PaneId) {
    match node {
        Node::Leaf(id) => {
            if *id == a {
                *id = b;
            } else if *id == b {
                *id = a;
            }
        }
        Node::Split { first, second, .. } => {
            swap_leaf_values(first, a, b);
            swap_leaf_values(second, a, b);
        }
    }
}

impl Layout {
    /// Rebuild the split tree from scratch as one of tmux's five preset
    /// layouts. `panes` is the pane order the preset uses to place panes
    /// (position 0 is the "main" pane for `MainHorizontal`/`MainVertical`) --
    /// callers pass the window's pane CREATION order (ascending `PaneId`),
    /// not `self.panes()`'s current tree order, so a preset re-applied after
    /// a `swap-pane`/`rotate-window` reproduces the same layout regardless of
    /// how the tree got scrambled (task brief: "pane ordering is
    /// creation/index order, not tree position"). A single pane always just
    /// fills `area`, ignoring `preset`/`main_width`/`main_height`. Focus is
    /// preserved if the focused pane is still present (else falls back to
    /// `panes[0]`); zoom is cleared (matches `split`/`remove`). No-op if
    /// `panes` is empty (never happens in practice -- a window always has at
    /// least one pane).
    pub fn apply_preset(&mut self, preset: LayoutPreset, panes: &[PaneId], area: Rect, main_width: u16, main_height: u16) {
        if panes.is_empty() {
            return;
        }
        self.zoomed = false;
        let focused = if panes.contains(&self.focused) { self.focused } else { panes[0] };
        self.root = build_preset_tree(preset, panes, area, main_width, main_height);
        self.focused = focused;
    }

    /// Swap the CONTENTS of the two leaves holding `a` and `b` (each pane
    /// keeps its own id, but the two trade tree/screen positions). Since
    /// `self.focused` stores a `PaneId` (not a tree position), a focused pane
    /// that is one of `a`/`b` automatically "follows" to its new screen
    /// position -- no explicit focus bookkeeping needed (tmux swap-pane
    /// semantics, both the `-U`/`-D` and explicit `-s`/`-t` forms). Clears
    /// zoom. `false` (no-op, tree unchanged) if `a == b` or either id isn't a
    /// leaf of this layout.
    pub fn swap_panes(&mut self, a: PaneId, b: PaneId) -> bool {
        if a == b {
            return false;
        }
        let panes = self.panes();
        if !panes.contains(&a) || !panes.contains(&b) {
            return false;
        }
        self.zoomed = false;
        swap_leaf_values(&mut self.root, a, b);
        true
    }

    /// Rotate every pane's content through the tree's leaf positions by one
    /// step: `forward` shifts each position's content to what the PREVIOUS
    /// leaf position held (so, walked the other way, every pane's content
    /// moves one position later, wrapping last -> first); `!forward` is the
    /// mirror (content moves one position earlier, wrapping first -> last).
    /// Per the design spec, focus follows the SCREEN CELL, not the pane: the
    /// leaf POSITION that was focused stays focused, now showing whichever
    /// pane rotated into it. Clears zoom. `false` (no-op) with 0 or 1 panes.
    pub fn rotate(&mut self, forward: bool) -> bool {
        let ids = self.panes();
        let n = ids.len();
        if n <= 1 {
            return false;
        }
        self.zoomed = false;
        let focused_pos = ids.iter().position(|&p| p == self.focused);
        let new_ids: Vec<PaneId> = (0..n)
            .map(|i| if forward { ids[(i + n - 1) % n] } else { ids[(i + 1) % n] })
            .collect();
        let mut next = 0usize;
        assign_leaf_values_in_order(&mut self.root, &new_ids, &mut next);
        if let Some(pos) = focused_pos {
            self.focused = new_ids[pos];
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Direction, Rect};

    const A: Rect = Rect { x: 0, y: 0, w: 80, h: 24 };

    #[test]
    fn single_pane_gets_full_area() {
        let l = Layout::new(7);
        assert_eq!(l.focused(), 7);
        assert_eq!(l.len(), 1);
        assert_eq!(l.panes(), vec![7]);
        // One leaf → the whole area, no borders.
        assert_eq!(l.rects(A), vec![(7, Rect { x: 0, y: 0, w: 80, h: 24 })]);
    }

    #[test]
    fn horizontal_split_ratio_half() {
        // Split axis = area.w = 80.
        // child1 = round((80 - 1) * 0.5) = round(39.5) = 40
        // child2 = 80 - 1 - 40 = 39 ; the -1 is the border column at x = 40.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert_eq!(l.focused(), 2); // new pane receives focus (tmux default)
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 40, h: 24 }),
                (2, Rect { x: 41, y: 0, w: 39, h: 24 }),
            ]
        );
    }

    #[test]
    fn vertical_split_ratio_half() {
        // Split axis = area.h = 24.
        // child1 = round((24 - 1) * 0.5) = round(11.5) = 12
        // child2 = 24 - 1 - 12 = 11 ; border row at y = 12.
        let mut l = Layout::new(1);
        l.split(SplitDir::Vertical, 2, A).unwrap();
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 12 }),
                (2, Rect { x: 0, y: 13, w: 80, h: 11 }),
            ]
        );
    }

    #[test]
    fn nested_splits() {
        // 1 ; H-split -> (1 | 2) focus 2 ; V-split on 2 -> (2 over 3) focus 3.
        // Tree = H(Leaf1, V(Leaf2, Leaf3)).
        // Root H on w=80: child1 = 40 (pane1), border x=40, right area {41,0,39,24}.
        // Inner V on {41,0,39,24}: axis h=24, child1 = round(23*0.5)=12,
        //   child2 = 24-1-12 = 11 -> pane2 {41,0,39,12}, border y=12,
        //   pane3 {41,13,39,11}.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        l.split(SplitDir::Vertical, 3, A).unwrap();
        assert_eq!(l.focused(), 3);
        assert_eq!(l.panes(), vec![1, 2, 3]);
        assert_eq!(l.len(), 3);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 40, h: 24 }),
                (2, Rect { x: 41, y: 0, w: 39, h: 12 }),
                (3, Rect { x: 41, y: 13, w: 39, h: 11 }),
            ]
        );
    }

    #[test]
    fn split_refused_below_min_width() {
        // area.w = 4 ; H-split: child1 = round(3*0.5)=round(1.5)=2,
        // child2 = 4-1-2 = 1 -> width 1 < MIN_PANE_W(2) -> refused, tree unchanged.
        let mut l = Layout::new(1);
        let area = Rect { x: 0, y: 0, w: 4, h: 24 };
        assert_eq!(l.split(SplitDir::Horizontal, 2, area), Err(SplitRefused));
        assert_eq!(l.panes(), vec![1]);
        assert_eq!(l.focused(), 1);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn split_refused_below_min_height() {
        // area.h = 4 ; V-split: child1 = round(3*0.5)=2, child2 = 4-1-2 = 1
        // -> height 1 < MIN_PANE_H(2) -> refused.
        let mut l = Layout::new(1);
        let area = Rect { x: 0, y: 0, w: 80, h: 4 };
        assert_eq!(l.split(SplitDir::Vertical, 2, area), Err(SplitRefused));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn split_allowed_at_exact_min() {
        // area.w = 5 ; child1 = round(4*0.5)=2, child2 = 5-1-2 = 2 -> both == MIN.
        let mut l = Layout::new(1);
        let area = Rect { x: 0, y: 0, w: 5, h: 24 };
        assert!(l.split(SplitDir::Horizontal, 2, area).is_ok());
        assert_eq!(
            l.rects(area),
            vec![
                (1, Rect { x: 0, y: 0, w: 2, h: 24 }),
                (2, Rect { x: 3, y: 0, w: 2, h: 24 }),
            ]
        );
    }

    #[test]
    fn constants_match_contract() {
        assert_eq!(MIN_PANE_W, 2);
        assert_eq!(MIN_PANE_H, 2);
    }

    /// Every returned rect's w/h must fit within `area` (zero sizes allowed);
    /// rects with nonzero size must additionally lie fully inside `area`.
    fn assert_rects_fit(rects: &[(PaneId, Rect)], area: Rect) {
        for (id, r) in rects {
            assert!(
                r.w <= area.w && r.h <= area.h,
                "pane {id} rect {r:?} exceeds area {area:?} dimensions"
            );
            if r.w > 0 && r.h > 0 {
                assert!(
                    r.x >= area.x
                        && r.y >= area.y
                        && (r.x as u32 + r.w as u32) <= (area.x as u32 + area.w as u32)
                        && (r.y as u32 + r.h as u32) <= (area.y as u32 + area.h as u32),
                    "pane {id} rect {r:?} does not fit in area {area:?}"
                );
            }
        }
    }

    #[test]
    fn rects_do_not_panic_on_tiny_area() {
        // Build H(Leaf1, Leaf2) on a normal area, then ask for rects with
        // areas smaller than the tree's structural needs. The geometry must
        // be total: no underflow panic, rects degrade to zero size but stay
        // within the area.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();

        let zero_w = Rect { x: 0, y: 0, w: 0, h: 24 };
        let r = l.rects(zero_w);
        assert_eq!(r.len(), 2);
        assert_rects_fit(&r, zero_w);

        let one_by_one = Rect { x: 0, y: 0, w: 1, h: 1 };
        let r = l.rects(one_by_one);
        assert_eq!(r.len(), 2);
        assert_rects_fit(&r, one_by_one);
    }

    #[test]
    fn split_refused_on_tiny_area_tree_unchanged() {
        // Same tiny areas: split must refuse (not panic) and leave the tree
        // unchanged.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();

        let zero_w = Rect { x: 0, y: 0, w: 0, h: 24 };
        assert_eq!(l.split(SplitDir::Horizontal, 3, zero_w), Err(SplitRefused));
        let one_by_one = Rect { x: 0, y: 0, w: 1, h: 1 };
        assert_eq!(l.split(SplitDir::Vertical, 3, one_by_one), Err(SplitRefused));

        assert_eq!(l.panes(), vec![1, 2]);
        assert_eq!(l.focused(), 2);
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn zoom_returns_only_focused_full_area() {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap(); // focus 2
        assert!(!l.is_zoomed());
        l.toggle_zoom();
        assert!(l.is_zoomed());
        // Zoomed: only the focused pane, at the full area, no borders.
        assert_eq!(l.rects(A), vec![(2, Rect { x: 0, y: 0, w: 80, h: 24 })]);
        l.toggle_zoom();
        assert!(!l.is_zoomed());
        assert_eq!(l.rects(A).len(), 2);
    }

    #[test]
    fn split_clears_zoom() {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        l.toggle_zoom();
        assert!(l.is_zoomed());
        l.split(SplitDir::Vertical, 3, A).unwrap(); // splitting clears zoom
        assert!(!l.is_zoomed());
        assert_eq!(l.rects(A).len(), 3);
    }

    #[test]
    fn remove_gives_sibling_the_space() {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap(); // (1 | 2), focus 2
        assert!(l.remove(2)); // removing the focused pane returns true
        assert_eq!(l.len(), 1);
        assert_eq!(l.focused(), 1); // focus falls to the sibling leaf
        // Sibling absorbs the whole area, no border.
        assert_eq!(l.rects(A), vec![(1, Rect { x: 0, y: 0, w: 80, h: 24 })]);
    }

    #[test]
    fn remove_non_focused_keeps_focus() {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap(); // focus 2
        assert!(l.remove(1)); // remove the non-focused pane
        assert_eq!(l.focused(), 2);
        assert_eq!(l.rects(A), vec![(2, Rect { x: 0, y: 0, w: 80, h: 24 })]);
    }

    #[test]
    fn remove_last_pane_returns_false() {
        let mut l = Layout::new(9);
        assert!(!l.remove(9)); // only pane -> false; the caller exits the app
        assert_eq!(l.len(), 1);
        assert_eq!(l.panes(), vec![9]);
    }

    #[test]
    fn remove_clears_zoom() {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        l.toggle_zoom();
        assert!(l.is_zoomed());
        l.remove(1);
        assert!(!l.is_zoomed());
    }

    #[test]
    fn focus_dir_two_pane_horizontal() {
        // (1 | 2): pane1 {0,0,40,24}, pane2 {41,0,39,24}; focus starts on 2.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert_eq!(l.focused(), 2);
        // Left: pane1's right edge 0+40 == focused.x-1 (41-1=40); vertical
        // midpoint of pane2 = 0 + 24/2 = 12, inside pane1 y-range [0,24) -> 1.
        assert!(l.focus_dir(Direction::Left, A));
        assert_eq!(l.focused(), 1);
        // Right from 1: pane2.x 41 == 0+40+1; midpoint 12 inside pane2 -> 2.
        assert!(l.focus_dir(Direction::Right, A));
        assert_eq!(l.focused(), 2);
        // Right at the right edge: no neighbor -> false, focus unchanged.
        assert!(!l.focus_dir(Direction::Right, A));
        assert_eq!(l.focused(), 2);
        // No vertical neighbor either way.
        assert!(!l.focus_dir(Direction::Up, A));
        assert!(!l.focus_dir(Direction::Down, A));
        // Move to the left pane, then Left at the left edge -> false.
        assert!(l.focus_dir(Direction::Left, A));
        assert_eq!(l.focused(), 1);
        assert!(!l.focus_dir(Direction::Left, A));
    }

    #[test]
    fn focus_dir_nested_adjacency() {
        // Tree = H(Leaf1, V(Leaf2, Leaf3)):
        // pane1 {0,0,40,24}, pane2 {41,0,39,12}, pane3 {41,13,39,11}; focus 3.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        l.split(SplitDir::Vertical, 3, A).unwrap();
        assert_eq!(l.focused(), 3);
        // Up from 3: pane2 bottom edge 0+12 == 13-1; horizontal midpoint of
        // pane3 = 41 + 39/2 = 60, inside pane2 x-range [41,80) -> 2.
        assert!(l.focus_dir(Direction::Up, A));
        assert_eq!(l.focused(), 2);
        // Down from 2: pane3 top 13 == 0+12+1; midpoint 60 inside pane3 -> 3.
        assert!(l.focus_dir(Direction::Down, A));
        assert_eq!(l.focused(), 3);
        // Left from 3: pane1 right edge 0+40 == 41-1; vertical midpoint of
        // pane3 = 13 + 11/2 = 18, inside pane1 y-range [0,24) -> 1.
        assert!(l.focus_dir(Direction::Left, A));
        assert_eq!(l.focused(), 1);
    }

    #[test]
    fn focus_next_wraps() {
        // leaf order [1,2,3], focus 3.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        l.split(SplitDir::Vertical, 3, A).unwrap();
        assert_eq!(l.panes(), vec![1, 2, 3]);
        assert_eq!(l.focused(), 3);
        l.focus_next(); // index 2 -> (2+1)%3 = 0 -> pane 1 (wraps)
        assert_eq!(l.focused(), 1);
        l.focus_next(); // -> 2
        assert_eq!(l.focused(), 2);
        l.focus_next(); // -> 3
        assert_eq!(l.focused(), 3);
    }

    #[test]
    fn focus_last_toggles() {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap(); // focus 2, last-focused 1
        assert_eq!(l.focused(), 2);
        l.focus_last(); // -> 1 (last-focused becomes 2)
        assert_eq!(l.focused(), 1);
        l.focus_last(); // -> 2 again
        assert_eq!(l.focused(), 2);
    }

    #[test]
    fn focus_last_ignores_removed_pane() {
        // Focus history 1 -> 2 -> 3 makes last-focused == 2. Removing 2 must
        // drop it, so focus_last becomes a no-op.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap(); // focus 2, last 1
        l.split(SplitDir::Vertical, 3, A).unwrap();   // focus 3, last 2
        assert!(l.remove(2));       // 2 gone; focus (3) was not removed, stays
        assert_eq!(l.focused(), 3);
        l.focus_last();             // last-focused (2) no longer exists -> no-op
        assert_eq!(l.focused(), 3);
    }

    #[test]
    fn resize_right_at_edge_is_noop() {
        // (1 | 2), focus 2 is the right-most pane. Right needs a Horizontal
        // ancestor whose FIRST child holds the focus; none exists -> false.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert_eq!(l.focused(), 2);
        assert!(!l.resize_focused(Direction::Right, A, 1));
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 40, h: 24 }),
                (2, Rect { x: 41, y: 0, w: 39, h: 24 }),
            ]
        );
    }

    #[test]
    fn resize_left_grows_focused() {
        // (1 | 2), focus 2. child1 (pane1) starts at 40. Left moves the shared
        // border left by 1: child1 40 -> 39; pane2 width = 80-1-39 = 40.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert!(l.resize_focused(Direction::Left, A, 1));
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 39, h: 24 }),
                (2, Rect { x: 40, y: 0, w: 40, h: 24 }),
            ]
        );
    }

    #[test]
    fn resize_right_grows_focused_first_child() {
        // Move focus to pane1 (the FIRST child), then Right:
        // child1 40 -> 41; pane2 width = 80-1-41 = 38.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert!(l.focus_dir(Direction::Left, A));
        assert_eq!(l.focused(), 1);
        assert!(l.resize_focused(Direction::Right, A, 1));
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 41, h: 24 }),
                (2, Rect { x: 42, y: 0, w: 38, h: 24 }),
            ]
        );
    }

    #[test]
    fn resize_from_reference_pane_ignores_focus() {
        // (1 | 2), focus stays on 1 (the FIRST child) throughout, but
        // `resize_from` is told to resize relative to pane 2 (the SECOND
        // child) -- Task 5's mouse border-drag needs this: it must be able
        // to move a border adjacent to a pane that ISN'T focused, without
        // changing focus. `Direction::Left` grows the SECOND child (matches
        // resize_focused's own Left/Up-grows-second-child rule), so a call
        // relative to pane 2 with Left should shrink pane1/grow pane2 by 1 --
        // same net rect change as `resize_left_grows_focused` above, but
        // reached via pane 2 as the reference instead of the focused pane.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert_eq!(l.focused(), 2); // split() gives focus to the new pane
        l.focus_pane(1); // move focus OFF pane 2 before resizing relative to it
        assert_eq!(l.focused(), 1);
        assert!(l.resize_from(2, Direction::Left, A, 1));
        assert_eq!(l.focused(), 1, "resize_from must not change focus");
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 39, h: 24 }),
                (2, Rect { x: 40, y: 0, w: 40, h: 24 }),
            ]
        );
    }

    #[test]
    fn resize_from_unknown_pane_is_noop() {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert!(!l.resize_from(99, Direction::Left, A, 1));
    }

    #[test]
    fn resize_clamps_at_minimum_and_reports_no_change() {
        // width 5: pane1 w=2, pane2 w=2 (both at MIN). focus 2. Left would push
        // child1 (2) below MIN_PANE_W -> clamp keeps it at 2 -> false, unchanged.
        let mut l = Layout::new(1);
        let area = Rect { x: 0, y: 0, w: 5, h: 24 };
        l.split(SplitDir::Horizontal, 2, area).unwrap();
        assert_eq!(
            l.rects(area),
            vec![
                (1, Rect { x: 0, y: 0, w: 2, h: 24 }),
                (2, Rect { x: 3, y: 0, w: 2, h: 24 }),
            ]
        );
        assert!(!l.resize_focused(Direction::Left, area, 5));
        assert_eq!(
            l.rects(area),
            vec![
                (1, Rect { x: 0, y: 0, w: 2, h: 24 }),
                (2, Rect { x: 3, y: 0, w: 2, h: 24 }),
            ]
        );
    }

    #[test]
    fn resize_picks_deepest_matching_ancestor() {
        // Tree = V(Leaf1, H(Leaf2, Leaf3)); focus 3.
        // Outer V on 80x24: child1 = round(23*0.5)=12 -> pane1 {0,0,80,12},
        //   bottom area {0,13,80,11}. Inner H on that: child1 = round(79*0.5)=40
        //   -> pane2 {0,13,40,11}, pane3 {41,13,39,11}.
        let mut l = Layout::new(1);
        l.split(SplitDir::Vertical, 2, A).unwrap();   // (1 over 2), focus 2
        l.split(SplitDir::Horizontal, 3, A).unwrap(); // (2 | 3),    focus 3
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 12 }),
                (2, Rect { x: 0, y: 13, w: 40, h: 11 }),
                (3, Rect { x: 41, y: 13, w: 39, h: 11 }),
            ]
        );
        // Left acts on the INNER Horizontal split (focus 3 is its second child):
        // its child1 40 -> 39; pane3 width = 80-1-39 = 40. Outer V untouched.
        assert!(l.resize_focused(Direction::Left, A, 1));
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 12 }),
                (2, Rect { x: 0, y: 13, w: 39, h: 11 }),
                (3, Rect { x: 40, y: 13, w: 40, h: 11 }),
            ]
        );
        // Up acts on the OUTER Vertical split (focus 3 lives in its second
        // child): child1 (top height) 12 -> 11; bottom area height 24-1-11 = 12.
        assert!(l.resize_focused(Direction::Up, A, 1));
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 11 }),
                (2, Rect { x: 0, y: 12, w: 39, h: 12 }),
                (3, Rect { x: 40, y: 12, w: 40, h: 12 }),
            ]
        );
    }

    #[test]
    fn focus_dir_right_near_u16_max_does_not_overflow() {
        // Follow-up #5: the Right/Down adjacency checks in `focus_dir` compute
        // `f.x + f.w + 1`. Construct an area near u16::MAX so that sum
        // overflows a u16 (65500 + 36 = 65536 > 65535) — a Vertical split
        // keeps both panes at the same x/w (only y/h differ), so the
        // comparison against the sibling pane hits the overflowing add
        // without needing the split geometry itself to overflow.
        let area = Rect { x: 65500, y: 0, w: 36, h: 24 };
        let mut l = Layout::new(1);
        l.split(SplitDir::Vertical, 2, area).unwrap(); // focus 2 (bottom)
        // Must not panic ("attempt to add with overflow" in debug builds).
        l.focus_dir(Direction::Right, area);
    }

    #[test]
    fn focus_dir_down_near_u16_max_does_not_overflow() {
        // Symmetric case for the Down check `f.y + f.h + 1`: a Horizontal
        // split keeps both panes at the same y/h (only x/w differ).
        let area = Rect { x: 0, y: 65500, w: 24, h: 36 };
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, area).unwrap(); // focus 2 (right)
        l.focus_dir(Direction::Down, area);
    }

    #[test]
    fn resize_steps_back_when_nested_min_violated() {
        // Tree = H(H(Leaf1, Leaf3), Leaf2); focus 2 (outer SECOND child).
        // Build: split(H,2) -> H(1,2) focus 2; focus Left -> 1;
        // split(H,3) -> H(H(1,3),2) focus 3; focus Right -> 2.
        // Outer H on 80: child1 = round(79*0.5) = 40 -> inner area {0,0,40,24},
        //   pane2 {41,0,39,24}. Inner H on 40: child1 = round(39*0.5) = 20
        //   -> pane1 {0,0,20,24}, pane3 {21,0,19,24}.
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        assert!(l.focus_dir(Direction::Left, A));
        l.split(SplitDir::Horizontal, 3, A).unwrap();
        assert!(l.focus_dir(Direction::Right, A));
        assert_eq!(l.focused(), 2);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 20, h: 24 }),
                (3, Rect { x: 21, y: 0, w: 19, h: 24 }),
                (2, Rect { x: 41, y: 0, w: 39, h: 24 }),
            ]
        );
        // Left with cells=40 acts on the OUTER split (the only H ancestor with
        // the focus in its second child). l=80, child1=40, lo=MIN_PANE_W=2,
        // hi=79-2=77 -> c = clamp(40-40, 2, 77) = 2. The outer split's OWN
        // bounds accept c=2, but the shrinking first child is the nested
        // H(1,3), whose pane widths (inner child1 = round((c-1)*0.5),
        // c-1-child1) are:
        //   c=2 -> (1,0)  FAIL   (all_min_ok false, loop steps c by +1)
        //   c=3 -> (1,1)  FAIL
        //   c=4 -> (2,1)  FAIL
        //   c=5 -> (2,2)  OK     both inner panes exactly at MIN_PANE_W
        // -> 3 failed iterations before settling: a PARTIAL move (35 of the
        // 40 requested cells), returns true.
        assert!(l.resize_focused(Direction::Left, A, 40));
        // Final: outer child1=5 -> inner area {0,0,5,24}, border x=5,
        // pane2 {6,0,80-1-5=74,24}. Inner on 5: child1 = round(4*0.5) = 2
        // -> pane1 {0,0,2,24}, border x=2, pane3 {3,0,5-1-2=2,24}.
        // (A naive resize that applied the clamped c=2 without the step-back
        // would instead yield pane1 w=1, pane3 w=0, pane2 w=77.)
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 2, h: 24 }),
                (3, Rect { x: 3, y: 0, w: 2, h: 24 }),
                (2, Rect { x: 6, y: 0, w: 74, h: 24 }),
            ]
        );
    }

    // ---- layout presets (Task 6, sub-project 4) ----------------------------

    /// Build an n-pane layout via splits (content doesn't matter -- every
    /// preset test rebuilds the tree from scratch) and return a `Layout`
    /// whose `panes()` happen to already be `1..=n` in order; presets are
    /// then applied against an explicit `panes` slice per
    /// `apply_preset`'s contract (creation/index order), which for these
    /// tests is simply `[1, 2, ..., n]`.
    fn layout_with_n_panes(n: u32) -> Layout {
        let mut l = Layout::new(1);
        for id in 2..=n {
            // Alternate split direction so intermediate geometry never
            // matters -- apply_preset always rebuilds the tree from `panes`
            // and `area`, ignoring whatever tree shape existed before.
            let dir = if id % 2 == 0 { SplitDir::Horizontal } else { SplitDir::Vertical };
            l.split(dir, id, A).expect("room for n panes at 80x24");
        }
        l
    }

    fn ids(n: u32) -> Vec<PaneId> {
        (1..=n).collect()
    }

    #[test]
    fn preset_even_horizontal_2_3_5() {
        // n=2: even_lengths(80,2): borders=1,usable=79,base=39,rem=1 -> [40,39]
        // (identical arithmetic to `horizontal_split_ratio_half` above).
        let mut l = layout_with_n_panes(2);
        l.apply_preset(LayoutPreset::EvenHorizontal, &ids(2), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![(1, Rect { x: 0, y: 0, w: 40, h: 24 }), (2, Rect { x: 41, y: 0, w: 39, h: 24 })]
        );

        // n=3: even_lengths(80,3): borders=2,usable=78,base=26,rem=0 -> [26,26,26].
        let mut l = layout_with_n_panes(3);
        l.apply_preset(LayoutPreset::EvenHorizontal, &ids(3), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 26, h: 24 }),
                (2, Rect { x: 27, y: 0, w: 26, h: 24 }),
                (3, Rect { x: 54, y: 0, w: 26, h: 24 }),
            ]
        );

        // n=5: even_lengths(80,5): borders=4,usable=76,base=15,rem=1 -> [16,15,15,15,15].
        let mut l = layout_with_n_panes(5);
        l.apply_preset(LayoutPreset::EvenHorizontal, &ids(5), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 16, h: 24 }),
                (2, Rect { x: 17, y: 0, w: 15, h: 24 }),
                (3, Rect { x: 33, y: 0, w: 15, h: 24 }),
                (4, Rect { x: 49, y: 0, w: 15, h: 24 }),
                (5, Rect { x: 65, y: 0, w: 15, h: 24 }),
            ]
        );
    }

    #[test]
    fn preset_even_vertical_2_3_5() {
        // n=2: even_lengths(24,2): borders=1,usable=23,base=11,rem=1 -> [12,11]
        // (identical arithmetic to `vertical_split_ratio_half` above).
        let mut l = layout_with_n_panes(2);
        l.apply_preset(LayoutPreset::EvenVertical, &ids(2), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![(1, Rect { x: 0, y: 0, w: 80, h: 12 }), (2, Rect { x: 0, y: 13, w: 80, h: 11 })]
        );

        // n=3: even_lengths(24,3): borders=2,usable=22,base=7,rem=1 -> [8,7,7].
        let mut l = layout_with_n_panes(3);
        l.apply_preset(LayoutPreset::EvenVertical, &ids(3), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 8 }),
                (2, Rect { x: 0, y: 9, w: 80, h: 7 }),
                (3, Rect { x: 0, y: 17, w: 80, h: 7 }),
            ]
        );

        // n=5: even_lengths(24,5): borders=4,usable=20,base=4,rem=0 -> [4,4,4,4,4].
        let mut l = layout_with_n_panes(5);
        l.apply_preset(LayoutPreset::EvenVertical, &ids(5), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 4 }),
                (2, Rect { x: 0, y: 5, w: 80, h: 4 }),
                (3, Rect { x: 0, y: 10, w: 80, h: 4 }),
                (4, Rect { x: 0, y: 15, w: 80, h: 4 }),
                (5, Rect { x: 0, y: 20, w: 80, h: 4 }),
            ]
        );
    }

    #[test]
    fn preset_main_horizontal_2_3_5() {
        // main-pane-height=10 (unclamped: 24 >= 2*MIN+1=5, max_main=24-1-2=21,
        // 10 <= 21). Main pane (id 1) full width, height 10; ratio=10/23,
        // child_first(24,ratio)=round(23*10/23)=10, bottom area {0,11,80,13}.
        let mut l = layout_with_n_panes(2);
        l.apply_preset(LayoutPreset::MainHorizontal, &ids(2), A, 80, 10);
        assert_eq!(
            l.rects(A),
            vec![(1, Rect { x: 0, y: 0, w: 80, h: 10 }), (2, Rect { x: 0, y: 11, w: 80, h: 13 })]
        );

        // n=3: bottom area {0,11,80,13}; 2 others even_lengths(80,2)=[40,39].
        let mut l = layout_with_n_panes(3);
        l.apply_preset(LayoutPreset::MainHorizontal, &ids(3), A, 80, 10);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 10 }),
                (2, Rect { x: 0, y: 11, w: 40, h: 13 }),
                (3, Rect { x: 41, y: 11, w: 39, h: 13 }),
            ]
        );

        // n=5: bottom area {0,11,80,13}; 4 others even_lengths(80,4):
        // borders=3,usable=77,base=19,rem=1 -> [20,19,19,19].
        let mut l = layout_with_n_panes(5);
        l.apply_preset(LayoutPreset::MainHorizontal, &ids(5), A, 80, 10);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 80, h: 10 }),
                (2, Rect { x: 0, y: 11, w: 20, h: 13 }),
                (3, Rect { x: 21, y: 11, w: 19, h: 13 }),
                (4, Rect { x: 41, y: 11, w: 19, h: 13 }),
                (5, Rect { x: 61, y: 11, w: 19, h: 13 }),
            ]
        );
    }

    #[test]
    fn preset_main_vertical_2_3_5() {
        // main-pane-width=30 (unclamped: 80 >= 5, max_main=80-1-2=77). Main
        // pane (id 1) full height, width 30; right area {31,0,49,24}.
        let mut l = layout_with_n_panes(2);
        l.apply_preset(LayoutPreset::MainVertical, &ids(2), A, 30, 24);
        assert_eq!(
            l.rects(A),
            vec![(1, Rect { x: 0, y: 0, w: 30, h: 24 }), (2, Rect { x: 31, y: 0, w: 49, h: 24 })]
        );

        // n=3: right area {31,0,49,24}; 2 others even_lengths(24,2)=[12,11].
        let mut l = layout_with_n_panes(3);
        l.apply_preset(LayoutPreset::MainVertical, &ids(3), A, 30, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 30, h: 24 }),
                (2, Rect { x: 31, y: 0, w: 49, h: 12 }),
                (3, Rect { x: 31, y: 13, w: 49, h: 11 }),
            ]
        );

        // n=5: right area {31,0,49,24}; 4 others even_lengths(24,4):
        // borders=3,usable=21,base=5,rem=1 -> [6,5,5,5].
        let mut l = layout_with_n_panes(5);
        l.apply_preset(LayoutPreset::MainVertical, &ids(5), A, 30, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 30, h: 24 }),
                (2, Rect { x: 31, y: 0, w: 49, h: 6 }),
                (3, Rect { x: 31, y: 7, w: 49, h: 5 }),
                (4, Rect { x: 31, y: 13, w: 49, h: 5 }),
                (5, Rect { x: 31, y: 19, w: 49, h: 5 }),
            ]
        );
    }

    #[test]
    fn preset_tiled_rows_first_shape() {
        // n=2: tiled_dims(2) = (rows=2, cols=1) -- degenerates to a plain
        // vertical stack, identical to `preset_even_vertical_2_3_5`'s n=2 case.
        let mut l = layout_with_n_panes(2);
        l.apply_preset(LayoutPreset::Tiled, &ids(2), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![(1, Rect { x: 0, y: 0, w: 80, h: 12 }), (2, Rect { x: 0, y: 13, w: 80, h: 11 })]
        );

        // n=3: tiled_dims(3) = (rows=2, cols=2). rows_used=ceil(3/2)=2,
        // heights=even_lengths(24,2)=[12,11]. Row0 = panes[1,2] (2 panes,
        // widths=[40,39]) at {0,0,80,12}. Row1 = panes[3] alone (SHORT row:
        // spans the full width) at {0,13,80,11}.
        let mut l = layout_with_n_panes(3);
        l.apply_preset(LayoutPreset::Tiled, &ids(3), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 40, h: 12 }),
                (2, Rect { x: 41, y: 0, w: 39, h: 12 }),
                (3, Rect { x: 0, y: 13, w: 80, h: 11 }),
            ]
        );

        // n=5: tiled_dims(5) = (rows=3, cols=2). rows_used=ceil(5/2)=3,
        // heights=even_lengths(24,3)=[8,7,7]. Row0=[1,2] widths=[40,39] at
        // {0,0,80,8}. Row1=[3,4] widths=[40,39] at {0,9,80,7}. Row2=[5] alone
        // (short row spans) at {0,17,80,7}.
        let mut l = layout_with_n_panes(5);
        l.apply_preset(LayoutPreset::Tiled, &ids(5), A, 80, 24);
        assert_eq!(
            l.rects(A),
            vec![
                (1, Rect { x: 0, y: 0, w: 40, h: 8 }),
                (2, Rect { x: 41, y: 0, w: 39, h: 8 }),
                (3, Rect { x: 0, y: 9, w: 40, h: 7 }),
                (4, Rect { x: 41, y: 9, w: 39, h: 7 }),
                (5, Rect { x: 0, y: 17, w: 80, h: 7 }),
            ]
        );
    }

    #[test]
    fn preset_main_pane_height_clamped_and_min_respected() {
        // area 80x10, requested main-pane-height=100: total=10, MIN=2,
        // 2*MIN+1=5 <= 10, so max_main = 10-1-2 = 7 -> clamp(100,2,7)=7.
        // Main pane gets height 7 (not the requested 100); the other row
        // gets exactly MIN_PANE_H (2), never less.
        let area = Rect { x: 0, y: 0, w: 80, h: 10 };
        let mut l = layout_with_n_panes(2);
        l.apply_preset(LayoutPreset::MainHorizontal, &ids(2), area, 80, 100);
        let rects = l.rects(area);
        assert_eq!(rects[0], (1, Rect { x: 0, y: 0, w: 80, h: 7 }));
        assert_eq!(rects[1], (2, Rect { x: 0, y: 8, w: 80, h: 2 }));
        assert!(rects.iter().all(|(_, r)| r.h >= MIN_PANE_H));
    }

    #[test]
    fn preset_main_pane_width_clamped_and_min_respected() {
        // area 10x24, requested main-pane-width=100: total=10, MIN=2,
        // max_main = 10-1-2 = 7 -> clamp(100,2,7)=7; other column gets
        // exactly MIN_PANE_W (2).
        let area = Rect { x: 0, y: 0, w: 10, h: 24 };
        let mut l = layout_with_n_panes(2);
        l.apply_preset(LayoutPreset::MainVertical, &ids(2), area, 100, 24);
        let rects = l.rects(area);
        assert_eq!(rects[0], (1, Rect { x: 0, y: 0, w: 7, h: 24 }));
        assert_eq!(rects[1], (2, Rect { x: 8, y: 0, w: 2, h: 24 }));
        assert!(rects.iter().all(|(_, r)| r.w >= MIN_PANE_W));
    }

    #[test]
    fn preset_tiny_area_does_not_panic() {
        // 3x3 window, 5 panes: every preset must degrade to (legally)
        // zero-size rects rather than panic (follow the module's established
        // tiny-area tolerance, e.g. `rects_do_not_panic_on_tiny_area`).
        let area = Rect { x: 0, y: 0, w: 3, h: 3 };
        for preset in PRESET_CYCLE {
            let mut l = layout_with_n_panes(5);
            l.apply_preset(preset, &ids(5), area, 80, 24);
            let rects = l.rects(area);
            assert_eq!(rects.len(), 5);
            assert_rects_fit(&rects, area);
        }
    }

    #[test]
    fn preset_focus_preserved_or_falls_back() {
        // Focused pane still present after the preset: focus unchanged.
        let mut l = layout_with_n_panes(3); // focus ends on 3 after 2 splits
        assert_eq!(l.focused(), 3);
        l.apply_preset(LayoutPreset::Tiled, &ids(3), A, 80, 24);
        assert_eq!(l.focused(), 3);

        // Focused pane absent from `panes` (shouldn't normally happen -- the
        // caller always passes every current pane -- but the fallback must
        // still be total): falls back to panes[0].
        let mut l = layout_with_n_panes(3);
        l.apply_preset(LayoutPreset::Tiled, &[1, 2], A, 80, 24); // pane 3 excluded
        assert_eq!(l.focused(), 1);
    }

    #[test]
    fn preset_clears_zoom() {
        let mut l = layout_with_n_panes(2);
        l.toggle_zoom();
        assert!(l.is_zoomed());
        l.apply_preset(LayoutPreset::EvenHorizontal, &ids(2), A, 80, 24);
        assert!(!l.is_zoomed());
    }

    #[test]
    fn preset_name_roundtrip() {
        for preset in PRESET_CYCLE {
            assert_eq!(LayoutPreset::from_name(preset.name()), Some(preset));
        }
        assert_eq!(LayoutPreset::from_name("bogus"), None);
        assert_eq!(
            PRESET_CYCLE.map(LayoutPreset::cycle_index).to_vec(),
            vec![0, 1, 2, 3, 4]
        );
    }

    // ---- swap-pane / rotate-window (Task 6, sub-project 4) ------------------

    /// Shared 3-pane tree for swap/rotate tests: `H(Leaf1, V(Leaf2, Leaf3))`
    /// (same construction as `nested_splits` above), rects:
    /// pane1 {0,0,40,24} (leaf position 0), pane2 {41,0,39,12} (position 1),
    /// pane3 {41,13,39,11} (position 2); focus ends on 3.
    fn nested_3pane() -> Layout {
        let mut l = Layout::new(1);
        l.split(SplitDir::Horizontal, 2, A).unwrap();
        l.split(SplitDir::Vertical, 3, A).unwrap();
        l
    }

    #[test]
    fn swap_panes_relabels_leaves_focus_follows_pane() {
        let mut l = nested_3pane();
        assert_eq!(l.focused(), 3);
        assert!(l.swap_panes(1, 3));
        // Pane 3's content now occupies position 0 (pane 1's old rect); pane
        // 1 now occupies position 2 (pane 3's old rect); pane 2 untouched.
        assert_eq!(
            l.rects(A),
            vec![
                (3, Rect { x: 0, y: 0, w: 40, h: 24 }),
                (2, Rect { x: 41, y: 0, w: 39, h: 12 }),
                (1, Rect { x: 41, y: 13, w: 39, h: 11 }),
            ]
        );
        // Focus stays on pane id 3 -- it "followed" to its new position.
        assert_eq!(l.focused(), 3);
    }

    #[test]
    fn swap_panes_same_id_is_noop() {
        let mut l = nested_3pane();
        assert!(!l.swap_panes(2, 2));
        assert_eq!(l.panes(), vec![1, 2, 3]);
    }

    #[test]
    fn swap_panes_unknown_id_is_noop() {
        let mut l = nested_3pane();
        assert!(!l.swap_panes(1, 99));
        assert_eq!(l.panes(), vec![1, 2, 3]);
    }

    #[test]
    fn swap_panes_single_pane_layout_is_noop() {
        let mut l = Layout::new(1);
        assert!(!l.swap_panes(1, 1)); // only one pane exists -- a == b
    }

    #[test]
    fn swap_panes_clears_zoom() {
        let mut l = nested_3pane();
        l.toggle_zoom();
        assert!(l.is_zoomed());
        l.swap_panes(1, 2);
        assert!(!l.is_zoomed());
    }

    #[test]
    fn rotate_forward_permutes_and_focus_follows_screen_cell() {
        // Leaf order/positions: [1@pos0, 2@pos1, 3@pos2]; focus = 3 (pos2).
        // forward: new_ids[i] = old_ids[(i-1+n)%n] -> new_ids = [3, 1, 2].
        // Position 2 (the previously-focused SCREEN CELL) now holds pane 2,
        // so focus becomes 2.
        let mut l = nested_3pane();
        assert!(l.rotate(true));
        assert_eq!(
            l.rects(A),
            vec![
                (3, Rect { x: 0, y: 0, w: 40, h: 24 }),
                (1, Rect { x: 41, y: 0, w: 39, h: 12 }),
                (2, Rect { x: 41, y: 13, w: 39, h: 11 }),
            ]
        );
        assert_eq!(l.focused(), 2);
    }

    #[test]
    fn rotate_backward_permutes_and_focus_follows_screen_cell() {
        // !forward: new_ids[i] = old_ids[(i+1)%n] -> new_ids = [2, 3, 1].
        // Position 2 now holds pane 1, so focus becomes 1.
        let mut l = nested_3pane();
        assert!(l.rotate(false));
        assert_eq!(
            l.rects(A),
            vec![
                (2, Rect { x: 0, y: 0, w: 40, h: 24 }),
                (3, Rect { x: 41, y: 0, w: 39, h: 12 }),
                (1, Rect { x: 41, y: 13, w: 39, h: 11 }),
            ]
        );
        assert_eq!(l.focused(), 1);
    }

    #[test]
    fn rotate_single_pane_is_noop() {
        let mut l = Layout::new(1);
        assert!(!l.rotate(true));
        assert!(!l.rotate(false));
    }

    #[test]
    fn rotate_clears_zoom() {
        let mut l = nested_3pane();
        l.toggle_zoom();
        assert!(l.is_zoomed());
        l.rotate(true);
        assert!(!l.is_zoomed());
    }
}
