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
    /// Returns false if nothing changed.
    pub fn resize_focused(&mut self, dir: Direction, area: Rect, cells: u16) -> bool {
        let orient = match dir {
            Direction::Left | Direction::Right => SplitDir::Horizontal,
            Direction::Up | Direction::Down => SplitDir::Vertical,
        };
        // Right/Down grow the split's FIRST child (so the focus must live in
        // the first child); Left/Up grow the SECOND child.
        let want_first = matches!(dir, Direction::Right | Direction::Down);

        let path = match self.path_to(self.focused) {
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
}
