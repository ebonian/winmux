//! Split-tree layout (pure logic, no I/O).

use crate::geom::Rect;

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
/// round((L - 1) * ratio). Requires L >= 1 (callers guard this).
fn child_first(l: u16, ratio: f32) -> u16 {
    (((l - 1) as f32) * ratio).round() as u16
}

/// The two child rects of a split, EXCLUDING the single border row/column.
fn split_rects(dir: SplitDir, ratio: f32, area: Rect) -> (Rect, Rect) {
    match dir {
        SplitDir::Horizontal => {
            let c1 = child_first(area.w, ratio);
            let c2 = area.w - 1 - c1;
            (
                Rect { x: area.x, y: area.y, w: c1, h: area.h },
                Rect { x: area.x + c1 + 1, y: area.y, w: c2, h: area.h },
            )
        }
        SplitDir::Vertical => {
            let c1 = child_first(area.h, ratio);
            let c2 = area.h - 1 - c1;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

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
}
