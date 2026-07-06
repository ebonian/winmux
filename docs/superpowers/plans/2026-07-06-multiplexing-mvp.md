# winmux Multiplexing MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working tmux-style terminal multiplexer for Windows: multiple ConPTY-hosted PowerShell panes with tmux-default keybindings, borders, and a status bar, drawn into the host terminal — one session, one window, tested end-to-end.

**Architecture:** Every pane is its own tiny terminal emulator (vte-parsed grid fed by ConPTY output); a binary split tree computes pane rects; a double-buffered cell-diff compositor draws panes + borders + status bar into the host terminal. Threads + mpsc channels (one reader per pane, one waiter per pane, one stdin thread); the main thread owns all state and renders.

**Tech Stack:** Rust (edition 2021), `vte 0.13` (VT parsing), `windows 0.58` (ConPTY + Win32 console). Crate is lib+bin: `src/lib.rs` exposes all modules so integration tests can drive them.

**Spec:** `docs/specs/2026-07-06-multiplexing-mvp-design.md`
**Locked interface contract (read before every task):** `docs/specs/2026-07-06-mvp-interfaces.md`

## Global Constraints

- Public APIs MUST match `docs/specs/2026-07-06-mvp-interfaces.md` exactly; private helpers are free. If a signature must change to compile, update the contract file and every consumer in the same task.
- Dependencies: exactly `vte = "0.13"` and `windows = "0.58"` (features listed in the contract; Task 11 adds `Win32_System_SystemInformation`). No other crates without an explicit new decision.
- All modules are declared once in `src/lib.rs` (`pub mod ...`) by Task 1; no task adds `mod` lines to `src/main.rs`.
- tmux-exact behavior everywhere a choice exists: prefix `Ctrl-b` (0x02), `%`/`"` split direction meanings, new pane gets focus, repeat window 500 ms, confirm-before on kill, green active border, green status bar.
- Non-negotiable: never leave the user's terminal wrecked — every exit path (Drop, panic hook, error) restores console modes, leaves the alt screen, shows the cursor.
- Windows-only: build and test on this machine (Windows 11 build 26200). `cargo test` must be green after every task; `cargo clippy --all-targets -- -D warnings` clean at Tasks 11 and 12.
- Commit after every task with a conventional message ending in `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---
### Task 1: Project scaffold

**Files:**
- Create: `C:\Users\poon\developments\winmux\Cargo.toml`
- Create: `C:\Users\poon\developments\winmux\.gitignore`
- Create: `C:\Users\poon\developments\winmux\src\lib.rs`
- Create: `C:\Users\poon\developments\winmux\src\main.rs`
- Create: `C:\Users\poon\developments\winmux\src\geom.rs`
- Create: `C:\Users\poon\developments\winmux\src\layout.rs`
- Create: `C:\Users\poon\developments\winmux\src\grid.rs`
- Create: `C:\Users\poon\developments\winmux\src\render.rs`
- Create: `C:\Users\poon\developments\winmux\src\input.rs`
- Create: `C:\Users\poon\developments\winmux\src\pty.rs`
- Create: `C:\Users\poon\developments\winmux\src\host.rs`
- Create: `C:\Users\poon\developments\winmux\src\app.rs`

**Interfaces:**
- Consumes: nothing (fresh crate).
- Produces: a compiling `winmux` crate that is **lib + bin from the start** (integration tests in `tests/` can only link a library). `src/lib.rs` declares the 8 public modules (`app, geom, grid, host, input, layout, pty, render`); `src/main.rs` is the final entry-point shape from the contract (panic hook + `app::run()` + exit-code mapping), which compiles now because `app.rs` and `host.rs` carry minimal stubs. All other module files are empty placeholders filled by later tasks.

Steps:

- [ ] **Step 1: Write `Cargo.toml`** with the pinned deps from the contract (exact features).
```toml
[package]
name = "winmux"
version = "0.1.0"
edition = "2021"

[dependencies]
vte = "0.13"
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Security",
    "Win32_Storage_FileSystem",
    "Win32_System_Console",
    "Win32_System_Pipes",
    "Win32_System_Threading",
] }
```
  (No explicit `[lib]`/`[[bin]]` sections needed: Cargo auto-detects `src/lib.rs` as the library and `src/main.rs` as the `winmux` binary.)

- [ ] **Step 2: Write `.gitignore`.**
```gitignore
/target
```

- [ ] **Step 3: Write `src/lib.rs`** declaring every module (exact contents):
```rust
pub mod app;
pub mod geom;
pub mod grid;
pub mod host;
pub mod input;
pub mod layout;
pub mod pty;
pub mod render;
```

- [ ] **Step 4: Write `src/main.rs`** (exact contents — this is already the final entry point per the contract: install panic hook, call `app::run()`, map error to exit code):
```rust
use winmux::{app, host};

fn main() {
    host::install_panic_hook();
    if let Err(e) = app::run() {
        eprintln!("winmux: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 5: Write the 8 module files.** `app.rs` and `host.rs` need minimal compilable stubs (they are called from `main.rs`); the other six are empty placeholders (a doc comment so the file is not literally blank):

  `src/app.rs`
  ```rust
  //! Event loop wiring (implemented in a later task).

  pub fn run() -> Result<(), Box<dyn std::error::Error>> {
      Ok(())
  }
  ```
  `src/host.rs`
  ```rust
  //! Host terminal control (implemented in a later task).

  pub fn install_panic_hook() {}
  ```
  `src/geom.rs`
  ```rust
  //! Shared geometry types (implemented in a later task).
  ```
  `src/layout.rs`
  ```rust
  //! Split-tree layout (implemented in a later task).
  ```
  `src/grid.rs`
  ```rust
  //! Per-pane terminal emulator (implemented in a later task).
  ```
  `src/render.rs`
  ```rust
  //! Compositor + differ (implemented in a later task).
  ```
  `src/input.rs`
  ```rust
  //! Prefix-key state machine (implemented in a later task).
  ```
  `src/pty.rs`
  ```rust
  //! ConPTY wrapper (implemented in a later task).
  ```

- [ ] **Step 6: Build and sanity-run.** Run:
  ```
  cargo build
  ```
  Expected: `Finished dev [unoptimized + debuginfo] target(s)` (first run downloads and compiles `vte` + `windows`; success, no errors — both the `winmux` lib and the `winmux` bin build). Then:
  ```
  cargo run
  ```
  Expected: exits immediately with code 0 and no output (the stub `app::run()` returns `Ok(())`).

- [ ] **Step 7: Commit.**
  ```
  git add Cargo.toml Cargo.lock .gitignore src
  git commit -m "chore: scaffold winmux lib+bin crate with module skeleton"
  ```

---

### Task 2: `geom` + `layout` core (new / split / rects)

**Files:**
- Modify: `C:\Users\poon\developments\winmux\src\geom.rs`
- Modify: `C:\Users\poon\developments\winmux\src\layout.rs`
- Test: unit tests in `#[cfg(test)] mod tests` inside `src\layout.rs`

**Interfaces:**
- Consumes: `crate::geom::Rect` (from this task's `geom.rs`; `crate` = the `winmux` lib).
- Produces (public, must match the locked contract exactly):
  ```rust
  // geom.rs
  pub struct Rect { pub x: u16, pub y: u16, pub w: u16, pub h: u16 }   // Clone,Copy,PartialEq,Eq,Debug
  pub enum Direction { Left, Right, Up, Down }                          // Clone,Copy,PartialEq,Eq,Debug

  // layout.rs
  pub type PaneId = u32;
  pub enum SplitDir { Horizontal, Vertical }                           // Clone,Copy,PartialEq,Eq,Debug
  pub const MIN_PANE_W: u16 = 2;
  pub const MIN_PANE_H: u16 = 2;
  pub struct SplitRefused;                                             // Debug,PartialEq,Eq
  impl Layout {
      pub fn new(first: PaneId) -> Self;
      pub fn split(&mut self, dir: SplitDir, new_pane: PaneId, area: Rect) -> Result<(), SplitRefused>;
      pub fn focused(&self) -> PaneId;
      pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)>;
      pub fn panes(&self) -> Vec<PaneId>;
      pub fn len(&self) -> usize;
  }
  ```
- Private (this task, per the required representation hint): `enum Node { Leaf(PaneId), Split { dir: SplitDir, ratio: f32, first: Box<Node>, second: Box<Node> } }`; `Layout` fields `root: Node, focused: PaneId, last_focused: Option<PaneId>, zoomed: bool`; helper fns `child_first`, `split_rects`, `rects_of`, `collect_leaves`, `replace_leaf`, method `all_rects`.

Steps:

- [ ] **Step 1: Write `geom.rs` fully** (finished in this task; no later changes needed):
```rust
//! Shared geometry types (pure, no I/O).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}
```

- [ ] **Step 2: Write the failing tests.** Replace `src/layout.rs` with just the placeholder doc comment plus this `mod tests` block appended (the types it references don't exist yet — that is the RED state). Every expected number is computed in a comment.
```rust
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
```

- [ ] **Step 3: Run the tests, watch them fail.**
  ```
  cargo test layout:: -- --nocapture
  ```
  Expected: compilation fails (RED), e.g.
  ```
  error[E0433]: failed to resolve: use of undeclared type `Layout`
  error[E0433]: failed to resolve: use of undeclared type `SplitDir`
  error[E0425]: cannot find value `MIN_PANE_W` in this scope
  ```

- [ ] **Step 4: Write the implementation** — insert all of this into `src/layout.rs` above the `mod tests` block, replacing the placeholder doc comment. Complete code, no elisions:
```rust
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
```

- [ ] **Step 5: Run the tests, watch them pass.**
  ```
  cargo test layout:: -- --nocapture
  ```
  Expected: `test result: ok. 8 passed; 0 failed`.

- [ ] **Step 6: Commit.**
  ```
  git add src/geom.rs src/layout.rs
  git commit -m "feat(layout): geom types and split-tree new/split/rects"
  ```

---

### Task 3: `layout` operations (focus navigation, remove, resize, zoom)

**Files:**
- Modify: `C:\Users\poon\developments\winmux\src\layout.rs`
- Test: additional tests in the existing `#[cfg(test)] mod tests` in `src\layout.rs`

**Interfaces:**
- Consumes: `crate::geom::{Direction, Rect}`, plus Task 2's private `Node` and helpers (`split_rects`, `child_first`, `all_rects`) and public `panes`/`len`.
- Produces (public, must match the locked contract exactly):
  ```rust
  impl Layout {
      pub fn focus_dir(&mut self, dir: Direction, area: Rect) -> bool;
      pub fn focus_next(&mut self);
      pub fn focus_last(&mut self);
      pub fn remove(&mut self, id: PaneId) -> bool;
      pub fn resize_focused(&mut self, dir: Direction, area: Rect, cells: u16) -> bool;
      pub fn toggle_zoom(&mut self);
      pub fn is_zoomed(&self) -> bool;
  }
  ```
- Private (this task): free fns `first_leaf`, `leaf_is`, `remove_from`, `node_at`, `node_at_mut`, `area_at`; methods `set_focus`, `path_to`, `set_ratio`, `all_min_ok`.

Steps:

- [ ] **Step 1: Widen the `geom` import.** In `src/layout.rs`, change:
```rust
use crate::geom::Rect;
```
to:
```rust
use crate::geom::{Direction, Rect};
```

- [ ] **Step 2: Write the failing tests.** Add these test functions inside the existing `mod tests` in `src/layout.rs`, and change its inner import `use crate::geom::Rect;` to `use crate::geom::{Direction, Rect};`. All expected numbers are computed in comments.
```rust
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
```

- [ ] **Step 3: Run the tests, watch them fail.**
  ```
  cargo test layout:: -- --nocapture
  ```
  Expected: compilation fails (RED), e.g.
  ```
  error[E0599]: no method named `is_zoomed` found for struct `Layout` in the current scope
  error[E0599]: no method named `toggle_zoom` found for struct `Layout` ...
  error[E0599]: no method named `remove` found for struct `Layout` ...
  error[E0599]: no method named `focus_dir` found for struct `Layout` ...
  error[E0599]: no method named `resize_focused` found for struct `Layout` ...
  ```

- [ ] **Step 4: Write the implementation** — add these free functions and a second `impl Layout` block to `src/layout.rs`, after the Task 2 `impl Layout` block and before `mod tests`. Complete code, no elisions:
```rust
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
                Direction::Right => r.x == f.x + f.w + 1,
                Direction::Left => f.x > 0 && r.x + r.w == f.x - 1,
                Direction::Down => r.y == f.y + f.h + 1,
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
            for i in 0..path.len() {
                if let Node::Split { dir: sd, first, second, .. } = node {
                    if *sd == orient && path[i] == !want_first {
                        target = Some(i);
                    }
                    node = if path[i] { &**second } else { &**first };
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
```

- [ ] **Step 5: Run the tests, watch them pass.**
  ```
  cargo test layout:: -- --nocapture
  ```
  Expected: `test result: ok. 23 passed; 0 failed` (8 from Task 2 + 15 added here).

- [ ] **Step 6: Run the full suite to confirm nothing else broke.**
  ```
  cargo test
  ```
  Expected: `test result: ok.` with 0 failures across all targets.

- [ ] **Step 7: Commit.**
  ```
  git add src/layout.rs
  git commit -m "feat(layout): focus, remove, resize, and zoom operations"
  ```
### Task 4: `grid` core emulation

Implements the `grid` module's public contract types (`Color`, `Style`, `Cell`, `Grid`) and the core vte-driven emulator: printing with proper deferred autowrap, the C0 controls, cursor positioning, erase-display/erase-line, the basic SGR set, and DECTCEM cursor visibility. Task 5 extends the same file.

**Files:**
- Modify: `C:\Users\poon\developments\winmux\src\grid.rs` (Task 1 scaffolds it empty; `src/lib.rs` already declares `pub mod grid;` — the crate is lib+bin, all modules are declared in src/lib.rs by Task 1)
- Test: `C:\Users\poon\developments\winmux\src\grid.rs` (unit tests in `#[cfg(test)] mod tests`)
- (No `Cargo.toml` change — `vte = "0.13"` was added in Task 1.)

**Interfaces:**
- Consumes: `vte::{Parser, Perform, Params}` (vte 0.13). `Parser::new()`; `parser.advance(&mut performer, byte: u8)`; `Perform` methods `print/execute/hook/put/unhook/osc_dispatch/csi_dispatch/esc_dispatch`; `params.iter()` yields `&[u16]` subparam slices.
- Produces (exactly the locked contract — no more public API):
  ```rust
  pub enum Color { Default, Idx(u8), Rgb(u8, u8, u8) }           // Clone,Copy,PartialEq,Eq,Debug
  pub struct Style { pub fg: Color, pub bg: Color, pub bold: bool,
      pub dim: bool, pub italic: bool, pub underline: bool, pub reverse: bool } // + Default
  pub struct Cell { pub ch: char, pub style: Style }             // Clone,Copy,PartialEq,Debug + Default
  pub struct Grid { /* private: parser + state */ }
  impl Grid {
      pub fn new(cols: u16, rows: u16) -> Self;
      pub fn feed(&mut self, bytes: &[u8]);
      pub fn resize(&mut self, cols: u16, rows: u16);
      pub fn cols(&self) -> u16;
      pub fn rows(&self) -> u16;
      pub fn cell(&self, col: u16, row: u16) -> Cell;   // panics out of range
      pub fn cursor(&self) -> (u16, u16);               // (col, row)
      pub fn cursor_visible(&self) -> bool;             // default true
  }
  ```
  Internally: `Grid { parser: vte::Parser, state: TermState }` where `TermState: vte::Perform` (the vte borrow-split the contract mandates).

---

- [ ] **Step 1: Write the failing core tests.** Append this test module to `src/grid.rs`. It references types/methods that do not exist yet, so it will not compile — that is the intended first failure.

  ```rust
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
  ```

- [ ] **Step 2: Run tests to confirm they fail.** Run exactly:

  `cargo test grid:: `

  Expected: compilation failure — `cannot find type Grid in this scope` / `cannot find type Color` (the module has no implementation yet). This is the red state.

- [ ] **Step 3: Write the implementation.** Put this at the TOP of `src/grid.rs`, above the `#[cfg(test)] mod tests` block. Complete, no elisions.

  ```rust
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
  ```

- [ ] **Step 4: Run tests to confirm they pass.** Run exactly:

  `cargo test grid:: `

  Expected: all 18 Task-4 tests pass (`test result: ok. 18 passed; 0 failed`). If vte 0.13 fails to resolve/compile, bump the version per the contract note and fix any API drift in this same task.

- [ ] **Step 5: Commit.** Run:

  ```
  git add src/grid.rs
  git commit -m "feat(grid): core VT emulation (print, autowrap, C0, cursor, ED/EL, SGR, DECTCEM)"
  ```

---

### Task 5: `grid` extended emulation + resize

Extends `src/grid.rs` with the editing/scrolling CSIs (ICH, DCH, ECH, IL, DL, SU, SD), scroll regions (DECSTBM + region-aware LF/RI), cursor save/restore (`ESC 7`/`8`, `CSI s`/`u`), reverse index (`ESC M`), the full SGR set including 256-color and truecolor, the alt-screen toggle, and validated resize behavior. This task edits the file from Task 4.

**Files:**
- Modify: `C:\Users\poon\developments\winmux\src\grid.rs`
- Test: `C:\Users\poon\developments\winmux\src\grid.rs` (add tests to the existing `mod tests`)

**Interfaces:**
- Consumes: same vte 0.13 surface as Task 4, plus `esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8)` (the final byte identifies `ESC 7`/`8`/`M`).
- Produces: no new public API. Same locked `Grid` surface; only internal behavior grows. SGR now covers `2,3,22,23,24,27,90–97,100–107, 38;5;n, 48;5;n, 38;2;r;g;b, 48;2;r;g;b` (semicolon form; colon subparameter form falls out for free because all subparams are flattened in order). Alt screen `CSI ?1049 h/l` both clear+home (contract).

---

- [ ] **Step 1: Write the failing extended tests.** Add these test functions inside the existing `#[cfg(test)] mod tests` block in `src/grid.rs` (the `row_str` helper is already defined there).

  ```rust
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
  ```

- [ ] **Step 2: Run tests to confirm they fail.** Run exactly:

  `cargo test grid:: `

  Expected: the new tests fail. The `@ P X L M S T r s u` sequences fall through Task 4's `_ => {}` arm (cursor/content unchanged), `ESC M`/`7`/`8` are no-ops, and `38;5;n`/bright SGR are dropped — so assertions like `insert_chars`, `scroll_up_su`, `save_restore_cursor_esc`, and `sgr_extended_colors` fail with mismatched cells/cursor (`assertion 'left == right' failed`). Task-4 tests still pass.

- [ ] **Step 3: Add the SavedCursor type and struct field.** Insert the `SavedCursor` struct just above `struct TermState`:

  ```rust
  #[derive(Clone, Copy)]
  struct SavedCursor {
      col: u16,
      row: u16,
      style: Style,
      autowrap: bool,
  }
  ```

  Add a field to `struct TermState` (after `scroll_bottom: u16,`):

  ```rust
      saved: Option<SavedCursor>,
  ```

  And initialize it in `TermState::new` (after `scroll_bottom: rows.saturating_sub(1),`):

  ```rust
              saved: None,
  ```

- [ ] **Step 4: Add the new helper methods.** Insert these methods inside `impl TermState` (e.g. right after `scroll_up`):

  ```rust
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
  ```

- [ ] **Step 5: Replace `apply_sgr` with the full version.** Replace the entire Task-4 `apply_sgr` method body with this (handles 256-color/truecolor via flattened subparams, which also covers the colon form):

  ```rust
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
                      if i + 1 < flat.len() {
                          match flat[i + 1] {
                              5 => {
                                  if i + 2 < flat.len() {
                                      self.style.fg = Color::Idx(flat[i + 2] as u8);
                                      i += 2;
                                  }
                              }
                              2 => {
                                  if i + 4 < flat.len() {
                                      self.style.fg = Color::Rgb(
                                          flat[i + 2] as u8,
                                          flat[i + 3] as u8,
                                          flat[i + 4] as u8,
                                      );
                                      i += 4;
                                  }
                              }
                              _ => {}
                          }
                      }
                  }
                  48 => {
                      if i + 1 < flat.len() {
                          match flat[i + 1] {
                              5 => {
                                  if i + 2 < flat.len() {
                                      self.style.bg = Color::Idx(flat[i + 2] as u8);
                                      i += 2;
                                  }
                              }
                              2 => {
                                  if i + 4 < flat.len() {
                                      self.style.bg = Color::Rgb(
                                          flat[i + 2] as u8,
                                          flat[i + 3] as u8,
                                          flat[i + 4] as u8,
                                      );
                                      i += 4;
                                  }
                              }
                              _ => {}
                          }
                      }
                  }
                  _ => {}
              }
              i += 1;
          }
      }
  ```

- [ ] **Step 6: Extend `csi_dispatch`.** In the private-marker branch, add `1049` handling. Replace the private block's inner match:

  ```rust
                  for p in params.iter() {
                      match p.first().copied() {
                          Some(7) => self.autowrap = set,   // DECAWM
                          Some(25) => self.cursor_visible = set, // DECTCEM
                          _ => {}
                      }
                  }
  ```

  with:

  ```rust
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
  ```

  Then, in the non-private `match action { ... }`, add these arms just before the final `'m' => self.apply_sgr(params),` line:

  ```rust
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
  ```

- [ ] **Step 7: Replace `esc_dispatch`.** Replace the empty Task-4 `esc_dispatch` with:

  ```rust
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
  ```

- [ ] **Step 8: Run tests to confirm they pass.** Run exactly:

  `cargo test grid:: `

  Expected: all Task-4 and Task-5 grid tests pass (`test result: ok. 35 passed; 0 failed`).

- [ ] **Step 9: Commit.** Run:

  ```
  git add src/grid.rs
  git commit -m "feat(grid): extended VT emulation (ICH/DCH/ECH/IL/DL/SU/SD, DECSTBM, save/restore, RI, full SGR, alt screen, resize)"
  ```
### Task 6: `render` composition (back buffer)

**Files:**
- `src/render.rs` (new — created by this task)

**Prerequisite:** `src/geom.rs` (`Rect`), `src/grid.rs` (`Grid`, `Cell`, `Style`, `Color`), and `src/layout.rs` (`PaneId`) must already be present and compiling (other tasks). `render` only consumes them. The crate is lib+bin: `src/lib.rs` already declares `pub mod render;` (done in Task 1) — no module-declaration edit is needed in this task.

**Interfaces:**

Consumes (exact signatures — do not redefine):
```rust
// geom.rs
pub struct Rect { pub x: u16, pub y: u16, pub w: u16, pub h: u16 }
// grid.rs
pub enum Color { Default, Idx(u8), Rgb(u8, u8, u8) }
pub struct Style { pub fg: Color, pub bg: Color, pub bold: bool, pub dim: bool,
                   pub italic: bool, pub underline: bool, pub reverse: bool }
pub struct Cell { pub ch: char, pub style: Style }   // Default => ch ' ', Style::default()
impl Grid { pub fn new(cols: u16, rows: u16) -> Self;
            pub fn feed(&mut self, bytes: &[u8]);
            pub fn cols(&self) -> u16; pub fn rows(&self) -> u16;
            pub fn cell(&self, col: u16, row: u16) -> Cell; }
// layout.rs
pub type PaneId = u32;
```

Produces (exact contract signatures — public surface):
```rust
pub struct PaneView<'a> { pub id: PaneId, pub rect: Rect, pub grid: &'a Grid,
                          pub focused: bool, pub dead: bool }
pub struct Scene<'a> { pub size: (u16, u16), pub panes: Vec<PaneView<'a>>,
                       pub zoomed: bool, pub status_left: String,
                       pub status_right: String, pub message: Option<String> }
pub struct Renderer { /* private */ }
impl Renderer { pub fn new(cols: u16, rows: u16) -> Self; }
```

Internal-only surface introduced by this task (NOT public): a private method `fn compose_back(&mut self, scene: &Scene)` that fills the back buffer, a private `fn set(&mut self, x: u16, y: u16, cell: Cell)`, free fn `fn border_glyph(...) -> char`, and a **`#[cfg(test)]` accessor `fn back_cell(&self, x: u16, y: u16) -> crate::grid::Cell`**. A `#[cfg(test)]` method is explicitly allowed and is the preferred way to assert composition here; Task 6's tests drive `compose_back` directly and read cells back through `back_cell`. No other public API is added. (The `front` and `force_full` fields are allocated now but only wired up in Task 7; a temporary `field is never read` warning is expected and acceptable — `cargo test` still passes.)

---

- [ ] **Step 1: Confirm the module declaration.** `src/lib.rs` already declares `pub mod render;` (added by Task 1 alongside `pub mod geom;`, `pub mod grid;`, `pub mod layout;`). Do not edit `src/lib.rs` or `src/main.rs` in this task — just verify the declaration exists.

- [ ] **Step 2: Write the failing composition tests.** Create `src/render.rs` containing ONLY the imports, the public type definitions, and the `#[cfg(test)] mod tests` block below. The `Renderer`/`compose_back`/`back_cell` items do not exist yet, so this fails to compile — that is the intended red state.

```rust
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
}
```

- [ ] **Step 3: Run the tests and confirm failure.** Run: `cargo test render::`. Expected: compilation error — `cannot find type Renderer` / `no method named compose_back` / `no method named back_cell`. This is the red state.

- [ ] **Step 4: Implement `Renderer::new`, buffers, `compose_back`, `set`, `border_glyph`, and the test accessor.** Insert the following ABOVE the `#[cfg(test)] mod tests` block in `src/render.rs` (keep the imports and public types from Step 2):

```rust
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
            let mut style = Style::default();
            style.reverse = true;
            let mut x = pv.rect.x;
            let x_end = pv.rect.x.saturating_add(pv.rect.w);
            for ch in "[exited]".chars() {
                if x >= x_end || x >= cols {
                    break;
                }
                self.set(x, y, Cell { ch, style });
                x += 1;
            }
        }

        // 4) status bar on the bottom row
        if rows == 0 {
            return;
        }
        let y = rows - 1;
        let cols_u = cols as usize;
        let (mut style, message) = match &scene.message {
            Some(m) => {
                let mut s = Style::default();
                s.fg = Color::Idx(0); // black
                s.bg = Color::Idx(3); // yellow
                (s, Some(m.clone()))
            }
            None => {
                let mut s = Style::default();
                s.fg = Color::Idx(0); // black
                s.bg = Color::Idx(2); // green
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
        // avoid an unused-assignment warning on `style` reuse
        let _ = &mut style;
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
```

- [ ] **Step 5: Run the tests and confirm they pass.** Run: `cargo test render::`. Expected: all seven composition tests pass (`two_panes_content_and_focused_border`, `border_tee_junction`, `status_bar_layout`, `status_truncates_right_first`, `message_override`, `dead_pane_overlay`, `zoom_suppresses_borders`). A `field is never read: front` / `force_full` warning may appear — ignore, it is resolved in Task 7.

- [ ] **Step 6: Commit.**
```
git add src/render.rs
git commit -m "render: back-buffer composition (panes, borders, status, overlays)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: `render` diffing, cursor, resize

**Files:**
- `src/render.rs` (edit — adds `compose`, `resize`, `cup`, `sgr`; extends `#[cfg(test)] mod tests`)

**Interfaces:**

Consumes (from Task 6, same module): `Renderer` private fields `cols/rows/front/back/force_full`, `Renderer::compose_back`, `crate::grid::{Cell, Color, Style}`.

Produces (exact contract signatures — public surface):
```rust
impl Renderer {
    pub fn resize(&mut self, cols: u16, rows: u16);
    pub fn compose(&mut self, scene: &Scene, cursor: Option<(u16, u16)>,
                   cursor_visible: bool) -> Vec<u8>;
}
```

Internal-only helpers introduced: free fns `fn cup(x: u16, y: u16) -> String` (emits 1-based `\x1b[{y+1};{x+1}H`) and `fn sgr(s: &Style) -> String` (emits the combined `\x1b[0;...m`). No public API beyond the two contract methods.

**VT emission contract (used to compute every expected string below):**
- CUP: `\x1b[{row+1};{col+1}H` (1-based).
- Default style SGR: `\x1b[0;39;49m` (`0`, fg Default→`39`, bg Default→`49`).
- Status style SGR: `\x1b[0;30;42m` (fg Idx(0)→`30`, bg Idx(2)→`42`).
- Reset: `\x1b[0m`; clear: `\x1b[2J`; show/hide cursor: `\x1b[?25h` / `\x1b[?25l`.

---

- [ ] **Step 1: Write the failing diff/cursor/resize tests.** Append the following test functions INSIDE the existing `#[cfg(test)] mod tests` block in `src/render.rs` (after the Task 6 tests). They reference `compose` and `resize`, which do not exist yet — this fails to compile (red state).

```rust
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
```

- [ ] **Step 2: Run the tests and confirm failure.** Run: `cargo test render::`. Expected: compilation error — `no method named compose` / `no method named resize` on `Renderer`.

- [ ] **Step 3: Implement `compose`, `resize`, `cup`, and `sgr`.** Add the two methods inside `impl Renderer` (below `compose_back`, above the `#[cfg(test)]` accessor) and the two free fns near `border_glyph`:

```rust
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
```

```rust
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
```

- [ ] **Step 4: Run the tests and confirm they pass.** Run: `cargo test render::`. Expected: all Task 6 and Task 7 tests pass (`single_cell_change_emits_only_that_cell`, `adjacent_changes_coalesce_one_cup_one_sgr`, `hidden_cursor_when_not_visible_and_no_change`, `resize_forces_full_repaint`). The earlier `front`/`force_full` dead-code warning is now gone.

- [ ] **Step 5: Commit.**
```
git add src/render.rs
git commit -m "render: cell-diff compose(), cursor placement, resize repaint

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
### Task 8: `input` prefix-key state machine

**Files:**
- Modify: `src/input.rs` (create the module body; tests live inline in `#[cfg(test)] mod tests` at the bottom of the same file)

**Interfaces:**
- Consumes:
  - `crate::geom::Direction` (from `src/geom.rs` — variants `Left, Right, Up, Down`) — provided by the geom task.
  - `crate::layout::SplitDir` (from `src/layout.rs` — variants `Horizontal, Vertical`) — provided by the layout task.
- Produces (public API — must match the locked contract EXACTLY, add nothing beyond it):
  ```rust
  pub enum Action {
      Split(SplitDir),
      Focus(Direction),
      FocusNext,
      FocusLast,
      RequestClose,
      ToggleZoom,
      Resize(Direction),
      Quit,
  }
  pub enum InputEvent {
      Forward(Vec<u8>),
      Action(Action),
      ConfirmClose(bool),
  }
  pub struct InputMachine { /* private */ }
  impl InputMachine {
      pub fn new() -> Self;
      pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<InputEvent>;
      pub fn set_confirming(&mut self, on: bool);
  }
  pub const PREFIX: u8 = 0x02;
  pub const REPEAT_TIME: std::time::Duration = std::time::Duration::from_millis(500);
  ```
  Everything else (`State`, `pending` buffer, helper fns) is private and MUST NOT be exported.

**Byte-level facts the implementation depends on (memorise these):**
- `PREFIX` = `0x02` (Ctrl-b).
- Arrow key = 3 bytes: `0x1b 0x5b <final>` where final ∈ `A/B/C/D` (`0x41/0x42/0x43/0x44`).
- Ctrl-arrow = 6 bytes: `0x1b 0x5b 0x31 0x3b 0x35 <final>` (i.e. `ESC [ 1 ; 5 A/B/C/D`).
- Direction mapping for BOTH arrows and Ctrl-arrows: `A → Up`, `B → Down`, `C → Right`, `D → Left`.

---

- [ ] **Step 1: Confirm the module is wired for compilation.**
  The crate is lib+bin: `src/lib.rs` (created by Task 1) already declares `pub mod input;` alongside the other modules. Do not add any `mod` declarations yourself — just verify `pub mod input;` exists in `src/lib.rs`. (`pub mod geom;` and `pub mod layout;` are owned by their tasks; if they are missing the crate will not compile — that is a cross-task precondition, not something to fix here.)

- [ ] **Step 2: Write the COMPLETE failing test module first (TDD — no implementation yet).**
  Put this at the bottom of `src/input.rs`. At this point the file has no types above it, so it will fail to compile — that is the expected first red.

  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::geom::Direction;
      use crate::layout::SplitDir;
      use std::time::{Duration, Instant};

      fn m() -> InputMachine {
          InputMachine::new()
      }

      // ---- Normal-mode passthrough + coalescing ----

      #[test]
      fn normal_passthrough_coalesces_into_one_forward() {
          let now = Instant::now();
          let mut im = m();
          let ev = im.feed(b"hello", now);
          assert_eq!(ev, vec![InputEvent::Forward(b"hello".to_vec())]);
      }

      #[test]
      fn empty_input_yields_no_events() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(im.feed(b"", now), vec![]);
      }

      // ---- Prefix consumed; bytes after a command continue in Normal ----

      #[test]
      fn prefix_split_then_continues_in_normal() {
          let now = Instant::now();
          let mut im = m();
          let ev = im.feed(b"ab\x02%cd", now);
          assert_eq!(
              ev,
              vec![
                  InputEvent::Forward(b"ab".to_vec()),
                  InputEvent::Action(Action::Split(SplitDir::Horizontal)),
                  InputEvent::Forward(b"cd".to_vec()),
              ]
          );
      }

      // ---- Every command key ----

      #[test]
      fn command_key_split_vertical() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(
              im.feed(b"\x02\"", now),
              vec![InputEvent::Action(Action::Split(SplitDir::Vertical))]
          );
      }

      #[test]
      fn command_key_split_horizontal() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(
              im.feed(b"\x02%", now),
              vec![InputEvent::Action(Action::Split(SplitDir::Horizontal))]
          );
      }

      #[test]
      fn command_key_focus_next() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(im.feed(b"\x02o", now), vec![InputEvent::Action(Action::FocusNext)]);
      }

      #[test]
      fn command_key_focus_last() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(im.feed(b"\x02;", now), vec![InputEvent::Action(Action::FocusLast)]);
      }

      #[test]
      fn command_key_request_close() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(im.feed(b"\x02x", now), vec![InputEvent::Action(Action::RequestClose)]);
      }

      #[test]
      fn command_key_toggle_zoom() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(im.feed(b"\x02z", now), vec![InputEvent::Action(Action::ToggleZoom)]);
      }

      #[test]
      fn command_arrows_map_to_focus() {
          let now = Instant::now();
          for (bytes, dir) in [
              (&b"\x02\x1b[A"[..], Direction::Up),
              (&b"\x02\x1b[B"[..], Direction::Down),
              (&b"\x02\x1b[C"[..], Direction::Right),
              (&b"\x02\x1b[D"[..], Direction::Left),
          ] {
              let mut im = m();
              assert_eq!(
                  im.feed(bytes, now),
                  vec![InputEvent::Action(Action::Focus(dir))],
                  "arrow {:?}",
                  bytes
              );
          }
      }

      #[test]
      fn double_prefix_forwards_literal_ctrl_b() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(im.feed(b"\x02\x02", now), vec![InputEvent::Forward(vec![0x02])]);
      }

      #[test]
      fn unknown_command_key_is_swallowed_and_disarms() {
          let now = Instant::now();
          let mut im = m();
          // 'q' is not a bound command: no event, and the machine is back in Normal.
          assert_eq!(im.feed(b"\x02q", now), vec![]);
          assert_eq!(im.feed(b"hi", now), vec![InputEvent::Forward(b"hi".to_vec())]);
      }

      // ---- Escape sequence split across feed() calls while Prefixed ----

      #[test]
      fn arrow_sequence_split_across_feeds() {
          let now = Instant::now();
          let mut im = m();
          assert_eq!(im.feed(b"\x02", now), vec![]);      // arm Prefixed
          assert_eq!(im.feed(b"\x1b[", now), vec![]);     // buffer incomplete ESC seq
          assert_eq!(im.feed(b"A", now), vec![InputEvent::Action(Action::Focus(Direction::Up))]);
      }

      // ---- Ctrl-arrow -> Resize, then Repeat window ----

      #[test]
      fn ctrl_arrow_resizes_and_enters_repeat() {
          let base = Instant::now();
          let mut im = m();
          assert_eq!(
              im.feed(b"\x02\x1b[1;5A", base),
              vec![InputEvent::Action(Action::Resize(Direction::Up))]
          );
      }

      #[test]
      fn bare_ctrl_arrow_within_window_repeats_resize() {
          let base = Instant::now();
          let mut im = m();
          assert_eq!(
              im.feed(b"\x02\x1b[1;5A", base),
              vec![InputEvent::Action(Action::Resize(Direction::Up))]
          );
          // No prefix this time; still inside the 500ms window (400ms later).
          assert_eq!(
              im.feed(b"\x1b[1;5B", base + Duration::from_millis(400)),
              vec![InputEvent::Action(Action::Resize(Direction::Down))]
          );
      }

      #[test]
      fn ctrl_arrow_after_window_elapsed_is_forwarded_raw() {
          let base = Instant::now();
          let mut im = m();
          assert_eq!(
              im.feed(b"\x02\x1b[1;5A", base),
              vec![InputEvent::Action(Action::Resize(Direction::Up))]
          );
          // 600ms later: window (base+500ms) has elapsed -> NOT a Resize,
          // the raw bytes are forwarded verbatim.
          assert_eq!(
              im.feed(b"\x1b[1;5C", base + Duration::from_millis(600)),
              vec![InputEvent::Forward(vec![0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x43])]
          );
      }

      #[test]
      fn non_ctrl_arrow_input_exits_repeat_as_normal() {
          let base = Instant::now();
          let mut im = m();
          assert_eq!(
              im.feed(b"\x02\x1b[1;5A", base),
              vec![InputEvent::Action(Action::Resize(Direction::Up))]
          );
          // Ordinary text while in Repeat: exit Repeat, process as Normal.
          assert_eq!(
              im.feed(b"hello", base),
              vec![InputEvent::Forward(b"hello".to_vec())]
          );
      }

      // ---- Confirming mode ----

      #[test]
      fn confirming_y_lower_confirms() {
          let now = Instant::now();
          let mut im = m();
          im.set_confirming(true);
          assert_eq!(im.feed(b"y", now), vec![InputEvent::ConfirmClose(true)]);
          // Back to Normal afterwards.
          assert_eq!(im.feed(b"a", now), vec![InputEvent::Forward(b"a".to_vec())]);
      }

      #[test]
      fn confirming_y_upper_confirms() {
          let now = Instant::now();
          let mut im = m();
          im.set_confirming(true);
          assert_eq!(im.feed(b"Y", now), vec![InputEvent::ConfirmClose(true)]);
      }

      #[test]
      fn confirming_other_key_cancels() {
          let now = Instant::now();
          let mut im = m();
          im.set_confirming(true);
          assert_eq!(im.feed(b"n", now), vec![InputEvent::ConfirmClose(false)]);
      }

      #[test]
      fn confirming_escape_cancels_and_is_consumed() {
          let now = Instant::now();
          let mut im = m();
          im.set_confirming(true);
          assert_eq!(im.feed(b"\x1b", now), vec![InputEvent::ConfirmClose(false)]);
          // Consumed, not forwarded; machine is Normal again.
          assert_eq!(im.feed(b"z", now), vec![InputEvent::Forward(b"z".to_vec())]);
      }
  }
  ```

- [ ] **Step 3: Run the tests and confirm the expected RED.**
  Command:
  ```
  cargo test input::
  ```
  Expected: a compile error (`cannot find type/struct InputMachine`, `Action`, `InputEvent`, etc. in module `input`), because no implementation exists yet. This is the intended failing state — do not proceed until you see it fail to compile for exactly this reason.

- [ ] **Step 4: Write the COMPLETE implementation above the test module (no TODOs, no elisions, every match arm written).**
  Place this at the TOP of `src/input.rs` (imports first, then the public types, then the private state and helpers, then `impl InputMachine`). The full state machine:

  ```rust
  use std::time::Instant;

  use crate::geom::Direction;
  use crate::layout::SplitDir;

  #[derive(Clone, Debug, PartialEq, Eq)]
  pub enum Action {
      Split(SplitDir),
      Focus(Direction),
      FocusNext,       // prefix o
      FocusLast,       // prefix ;
      RequestClose,    // prefix x
      ToggleZoom,      // prefix z
      Resize(Direction), // prefix Ctrl-arrow, repeatable
      Quit,            // internal: not bound to a key in the MVP
  }

  #[derive(Clone, Debug, PartialEq, Eq)]
  pub enum InputEvent {
      Forward(Vec<u8>),
      Action(Action),
      ConfirmClose(bool),
  }

  pub const PREFIX: u8 = 0x02; // Ctrl-b
  pub const REPEAT_TIME: std::time::Duration = std::time::Duration::from_millis(500);

  /// Private machine state.
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  enum State {
      Normal,
      Prefixed,
      Repeat { until: Instant },
      Confirming,
  }

  pub struct InputMachine {
      state: State,
      /// Buffered bytes of an in-progress escape sequence (always begins with
      /// 0x1b). Used while Prefixed (waiting for an arrow / Ctrl-arrow command)
      /// and while in Repeat (matching a bare Ctrl-arrow). Empty otherwise.
      pending: Vec<u8>,
  }

  /// Map an escape-sequence final byte to a direction.
  /// A->Up, B->Down, C->Right, D->Left (shared by arrows and Ctrl-arrows).
  fn arrow_dir(final_byte: u8) -> Option<Direction> {
      match final_byte {
          b'A' => Some(Direction::Up),
          b'B' => Some(Direction::Down),
          b'C' => Some(Direction::Right),
          b'D' => Some(Direction::Left),
          _ => None,
      }
  }

  /// Flush the coalesced Normal-forward accumulator as a single Forward event.
  fn flush_forward(fwd: &mut Vec<u8>, out: &mut Vec<InputEvent>) {
      if !fwd.is_empty() {
          out.push(InputEvent::Forward(std::mem::take(fwd)));
      }
  }

  impl InputMachine {
      pub fn new() -> Self {
          InputMachine {
              state: State::Normal,
              pending: Vec::new(),
          }
      }

      pub fn set_confirming(&mut self, on: bool) {
          self.pending.clear();
          self.state = if on { State::Confirming } else { State::Normal };
      }

      pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<InputEvent> {
          let mut out: Vec<InputEvent> = Vec::new();
          let mut fwd: Vec<u8> = Vec::new();

          let mut i = 0;
          while i < bytes.len() {
              let b = bytes[i];
              // Default: this byte is consumed. Repeat-state exits set this false
              // so the same byte is re-dispatched in Normal on the next iteration.
              let mut advance = true;

              match self.state {
                  State::Normal => {
                      if b == PREFIX {
                          flush_forward(&mut fwd, &mut out);
                          self.pending.clear();
                          self.state = State::Prefixed;
                      } else {
                          fwd.push(b);
                      }
                  }

                  State::Prefixed => {
                      if self.pending.is_empty() {
                          // First key after the prefix.
                          match b {
                              b'%' => {
                                  flush_forward(&mut fwd, &mut out);
                                  out.push(InputEvent::Action(Action::Split(SplitDir::Horizontal)));
                                  self.state = State::Normal;
                              }
                              b'"' => {
                                  flush_forward(&mut fwd, &mut out);
                                  out.push(InputEvent::Action(Action::Split(SplitDir::Vertical)));
                                  self.state = State::Normal;
                              }
                              b'o' => {
                                  flush_forward(&mut fwd, &mut out);
                                  out.push(InputEvent::Action(Action::FocusNext));
                                  self.state = State::Normal;
                              }
                              b';' => {
                                  flush_forward(&mut fwd, &mut out);
                                  out.push(InputEvent::Action(Action::FocusLast));
                                  self.state = State::Normal;
                              }
                              b'x' => {
                                  flush_forward(&mut fwd, &mut out);
                                  out.push(InputEvent::Action(Action::RequestClose));
                                  self.state = State::Normal;
                              }
                              b'z' => {
                                  flush_forward(&mut fwd, &mut out);
                                  out.push(InputEvent::Action(Action::ToggleZoom));
                                  self.state = State::Normal;
                              }
                              PREFIX => {
                                  // Ctrl-b Ctrl-b: send a literal Ctrl-b.
                                  flush_forward(&mut fwd, &mut out);
                                  out.push(InputEvent::Forward(vec![PREFIX]));
                                  self.state = State::Normal;
                              }
                              0x1b => {
                                  // Begin buffering an escape sequence.
                                  self.pending.push(b);
                              }
                              _ => {
                                  // Unknown command key: disarm silently, swallow.
                                  self.state = State::Normal;
                              }
                          }
                      } else {
                          // Continuing a buffered escape sequence (pending[0] == 0x1b).
                          match self.pending.len() {
                              1 => {
                                  if b == 0x5b {
                                      self.pending.push(b);
                                  } else {
                                      // ESC followed by something else: swallow + disarm.
                                      self.pending.clear();
                                      self.state = State::Normal;
                                  }
                              }
                              2 => {
                                  if let Some(dir) = arrow_dir(b) {
                                      flush_forward(&mut fwd, &mut out);
                                      out.push(InputEvent::Action(Action::Focus(dir)));
                                      self.pending.clear();
                                      self.state = State::Normal;
                                  } else if b == 0x31 {
                                      // Possible Ctrl-arrow: ESC [ 1 ...
                                      self.pending.push(b);
                                  } else {
                                      self.pending.clear();
                                      self.state = State::Normal;
                                  }
                              }
                              3 => {
                                  if b == 0x3b {
                                      self.pending.push(b);
                                  } else {
                                      self.pending.clear();
                                      self.state = State::Normal;
                                  }
                              }
                              4 => {
                                  if b == 0x35 {
                                      self.pending.push(b);
                                  } else {
                                      self.pending.clear();
                                      self.state = State::Normal;
                                  }
                              }
                              5 => {
                                  if let Some(dir) = arrow_dir(b) {
                                      flush_forward(&mut fwd, &mut out);
                                      out.push(InputEvent::Action(Action::Resize(dir)));
                                      self.pending.clear();
                                      self.state = State::Repeat { until: now + REPEAT_TIME };
                                  } else {
                                      self.pending.clear();
                                      self.state = State::Normal;
                                  }
                              }
                              _ => {
                                  // Defensive: over-long buffer, discard + disarm.
                                  self.pending.clear();
                                  self.state = State::Normal;
                              }
                          }
                      }
                  }

                  State::Repeat { until } => {
                      if now >= until {
                          // Window elapsed: leave Repeat and re-dispatch this byte
                          // as Normal. Any buffered escape bytes become forwarded.
                          if !self.pending.is_empty() {
                              fwd.extend_from_slice(&self.pending);
                              self.pending.clear();
                          }
                          self.state = State::Normal;
                          advance = false;
                      } else {
                          // Inside the window: only a bare Ctrl-arrow keeps repeating.
                          match self.pending.len() {
                              0 => {
                                  if b == 0x1b {
                                      self.pending.push(b);
                                  } else {
                                      // Non-Ctrl-arrow: exit Repeat, reprocess as Normal.
                                      self.state = State::Normal;
                                      advance = false;
                                  }
                              }
                              1 => {
                                  if b == 0x5b {
                                      self.pending.push(b);
                                  } else {
                                      fwd.extend_from_slice(&self.pending);
                                      self.pending.clear();
                                      self.state = State::Normal;
                                      advance = false;
                                  }
                              }
                              2 => {
                                  if b == 0x31 {
                                      self.pending.push(b);
                                  } else {
                                      // Includes plain arrows (final A/B/C/D): forward raw.
                                      fwd.extend_from_slice(&self.pending);
                                      self.pending.clear();
                                      self.state = State::Normal;
                                      advance = false;
                                  }
                              }
                              3 => {
                                  if b == 0x3b {
                                      self.pending.push(b);
                                  } else {
                                      fwd.extend_from_slice(&self.pending);
                                      self.pending.clear();
                                      self.state = State::Normal;
                                      advance = false;
                                  }
                              }
                              4 => {
                                  if b == 0x35 {
                                      self.pending.push(b);
                                  } else {
                                      fwd.extend_from_slice(&self.pending);
                                      self.pending.clear();
                                      self.state = State::Normal;
                                      advance = false;
                                  }
                              }
                              5 => {
                                  if let Some(dir) = arrow_dir(b) {
                                      flush_forward(&mut fwd, &mut out);
                                      out.push(InputEvent::Action(Action::Resize(dir)));
                                      self.pending.clear();
                                      self.state = State::Repeat { until: now + REPEAT_TIME };
                                  } else {
                                      fwd.extend_from_slice(&self.pending);
                                      self.pending.clear();
                                      self.state = State::Normal;
                                      advance = false;
                                  }
                              }
                              _ => {
                                  fwd.extend_from_slice(&self.pending);
                                  self.pending.clear();
                                  self.state = State::Normal;
                                  advance = false;
                              }
                          }
                      }
                  }

                  State::Confirming => {
                      // Exactly one key decides; keys are consumed, never forwarded.
                      flush_forward(&mut fwd, &mut out); // defensive; normally empty
                      let confirmed = b == b'y' || b == b'Y';
                      out.push(InputEvent::ConfirmClose(confirmed));
                      self.state = State::Normal;
                  }
              }

              if advance {
                  i += 1;
              }
          }

          // Flush any trailing coalesced Normal bytes. (Incomplete escape tails
          // remain buffered in self.pending across calls while Prefixed/Repeat.)
          flush_forward(&mut fwd, &mut out);
          out
      }
  }
  ```

  Notes for the implementer (already reflected above, do not deviate):
  - `advance = false` re-dispatches the current byte in `Normal` on the next loop turn. Every such branch also sets `state = State::Normal`, so the loop cannot spin (the next turn is `Normal`, which always advances).
  - Never push an `Action`/`ConfirmClose` without first `flush_forward`-ing, so event ordering (Forward before Action, as in the `ab\x02%cd` test) is preserved.
  - `Action::Quit` is defined by the contract but intentionally never emitted here.

- [ ] **Step 5: Run the tests and confirm GREEN.**
  Command:
  ```
  cargo test input::
  ```
  Expected: all tests in `input::tests` pass (every test listed in Step 2). If `geom`/`layout` are not yet present in the tree, the crate will fail to compile for an unrelated reason — coordinate with those tasks; do not stub their types here.

- [ ] **Step 6: Commit.**
  ```
  git add src/input.rs
  git commit -m "feat(input): prefix-key state machine with repeat + confirm modes"
  ```
### Task 9: `host` terminal control

**Files:**
- `src/host.rs` (implement) — the entire `host` module
- `src/lib.rs` (verify only) — src/lib.rs already declares the module (Task 1)

**Interfaces:**

Produces (must match the locked contract in `2026-07-06-mvp-interfaces.md` exactly):
```rust
pub struct Host { /* private: saved stdin/stdout modes + handles */ }
impl Host {
    pub fn enter() -> std::io::Result<Host>;
    pub fn size(&self) -> std::io::Result<(u16, u16)>;          // (cols, rows)
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>; // write + flush stdout
}
impl Drop for Host { /* infallible restoration */ }
pub fn install_panic_hook();
pub fn read_stdin(buf: &mut [u8]) -> std::io::Result<usize>;
```
Consumes: nothing (leaf module). Win32 via `windows` 0.58.

**Rationale for the test step:** every function here is a thin wrapper over Win32 console I/O against the *real* attached console (`GetStdHandle` returns the process's console handles). There is no console attached in a headless CI/agent run, so `Host::enter`/`size`/`write` cannot be exercised by an automated unit test. The correctness net for this module is therefore (a) `cargo build` proving the Win32 bindings and types resolve, plus (b) a single `#[test] #[ignore]` **manual smoke test** a human runs in a real terminal with `cargo test --ignored`. No non-ignored unit tests are added for this module — this is intentional and expected.

- [ ] **Step 1: Write the full `host` module.**

  ```rust
  //! Host terminal control: raw mode, alt-screen, size queries, frame writes,
  //! and guaranteed restoration on every exit path (Drop + panic hook).

  use std::ffi::c_void;
  use std::io;
  use std::sync::Mutex;

  use windows::Win32::Foundation::{ERROR_BROKEN_PIPE, HANDLE};
  use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
  use windows::Win32::System::Console::{
      GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, SetConsoleMode,
      CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN,
      ENABLE_EXTENDED_FLAGS, ENABLE_VIRTUAL_TERMINAL_INPUT,
      ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
  };
  // verify exact import paths against the windows 0.58 docs when compiling:
  // GetStdHandle / console mode flags live in Win32::System::Console; the mode
  // flag constants are values of type CONSOLE_MODE.

  /// Map a `windows::core::Error` into a `std::io::Error`. The stored HRESULT is
  /// passed as the raw OS error; on Windows `io::Error`'s Display formats HRESULTs
  /// correctly via FormatMessageW.
  fn win_err(e: windows::core::Error) -> io::Error {
      io::Error::from_raw_os_error(e.code().0)
  }

  /// Snapshot needed to restore the console. Stored as plain integers (not raw
  /// HANDLE pointers, which are neither Send nor Sync) so it can live in a static.
  struct RestoreState {
      stdin: isize,
      stdout: isize,
      stdin_mode: u32,
      stdout_mode: u32,
  }

  /// Populated by `Host::enter`; read by both `Drop` and the panic hook so both
  /// perform the identical restoration.
  static RESTORE: Mutex<Option<RestoreState>> = Mutex::new(None);

  /// Best-effort, infallible, idempotent restoration. Leaves alt-screen, shows
  /// cursor, resets SGR, then restores the saved console modes. Every error is
  /// ignored — restoration must never fail or panic.
  unsafe fn apply_restore(
      stdin: HANDLE,
      stdout: HANDLE,
      stdin_mode: CONSOLE_MODE,
      stdout_mode: CONSOLE_MODE,
  ) {
      // CSI ?1049l = leave alt screen, CSI ?25h = show cursor, CSI 0m = reset SGR.
      let seq = b"\x1b[?1049l\x1b[?25h\x1b[0m";
      let mut written: u32 = 0;
      let _ = WriteFile(stdout, Some(seq), Some(&mut written), None);
      let _ = SetConsoleMode(stdout, stdout_mode);
      let _ = SetConsoleMode(stdin, stdin_mode);
  }

  pub struct Host {
      stdin: HANDLE,
      stdout: HANDLE,
      saved_stdin: CONSOLE_MODE,
      saved_stdout: CONSOLE_MODE,
  }

  impl Host {
      pub fn enter() -> io::Result<Host> {
          unsafe {
              let stdin = GetStdHandle(STD_INPUT_HANDLE).map_err(win_err)?;
              let stdout = GetStdHandle(STD_OUTPUT_HANDLE).map_err(win_err)?;

              // Save the current modes so Drop / panic hook can restore them.
              let mut saved_stdin = CONSOLE_MODE::default();
              let mut saved_stdout = CONSOLE_MODE::default();
              GetConsoleMode(stdin, &mut saved_stdin).map_err(win_err)?;
              GetConsoleMode(stdout, &mut saved_stdout).map_err(win_err)?;

              // stdout: keep existing bits, add VT processing + suppress the
              // implicit CR that ConHost inserts when the cursor is at the last
              // column and an LF is written (DISABLE_NEWLINE_AUTO_RETURN).
              let new_stdout =
                  saved_stdout | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
              SetConsoleMode(stdout, new_stdout).map_err(win_err)?;

              // stdin: full raw mode. We set the mode from scratch (not OR'd onto
              // the old value), so ENABLE_LINE_INPUT, ENABLE_ECHO_INPUT,
              // ENABLE_PROCESSED_INPUT and ENABLE_QUICK_EDIT_MODE are all OFF.
              //
              // IMPORTANT: with ENABLE_PROCESSED_INPUT cleared, Ctrl-C does NOT
              // raise a CTRL_C_EVENT signal — it is delivered inline as the raw
              // byte 0x03 in the input stream, exactly like a tty in raw mode.
              // The input state machine forwards / handles 0x03 like any other byte.
              let new_stdin = ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS;
              SetConsoleMode(stdin, new_stdin).map_err(win_err)?;

              // Publish the restore snapshot for Drop and the panic hook.
              *RESTORE.lock().unwrap() = Some(RestoreState {
                  stdin: stdin.0 as isize,
                  stdout: stdout.0 as isize,
                  stdin_mode: saved_stdin.0,
                  stdout_mode: saved_stdout.0,
              });

              let mut host = Host { stdin, stdout, saved_stdin, saved_stdout };
              // Enter alt screen, clear it, home the cursor.
              host.write(b"\x1b[?1049h\x1b[2J\x1b[H")?;
              Ok(host)
          }
      }

      pub fn size(&self) -> io::Result<(u16, u16)> {
          unsafe {
              let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
              GetConsoleScreenBufferInfo(self.stdout, &mut info).map_err(win_err)?;
              // Use the visible window rect, not the buffer, so scrollback height
              // does not inflate the row count.
              let cols = (info.srWindow.Right - info.srWindow.Left + 1) as u16;
              let rows = (info.srWindow.Bottom - info.srWindow.Top + 1) as u16;
              Ok((cols, rows))
          }
      }

      pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
          // Console handle writes are unbuffered (WriteFile goes straight to the
          // console driver), so there is no user-space buffer to flush.
          let mut offset = 0usize;
          while offset < bytes.len() {
              let mut written: u32 = 0;
              unsafe {
                  WriteFile(self.stdout, Some(&bytes[offset..]), Some(&mut written), None)
                      .map_err(win_err)?;
              }
              if written == 0 {
                  return Err(io::Error::new(io::ErrorKind::WriteZero, "WriteFile wrote 0 bytes"));
              }
              offset += written as usize;
          }
          Ok(())
      }
  }

  impl Drop for Host {
      fn drop(&mut self) {
          // Infallible: apply_restore ignores every error internally.
          unsafe {
              apply_restore(self.stdin, self.stdout, self.saved_stdin, self.saved_stdout);
          }
      }
  }

  /// Install a panic hook that restores the console (identical to Drop) before
  /// delegating to the previously-installed hook. Call once from `main()` before
  /// `Host::enter`. Safe to call once; restoration is idempotent so overlap with
  /// Drop is harmless.
  pub fn install_panic_hook() {
      let previous = std::panic::take_hook();
      std::panic::set_hook(Box::new(move |info| {
          if let Ok(guard) = RESTORE.lock() {
              if let Some(r) = guard.as_ref() {
                  unsafe {
                      apply_restore(
                          HANDLE(r.stdin as *mut c_void),
                          HANDLE(r.stdout as *mut c_void),
                          CONSOLE_MODE(r.stdin_mode),
                          CONSOLE_MODE(r.stdout_mode),
                      );
                  }
              }
          }
          previous(info);
      }));
  }

  /// Blocking read of raw bytes from the console input handle, for the stdin
  /// thread. Returns Ok(0) only when the handle is closed (EOF / broken pipe).
  pub fn read_stdin(buf: &mut [u8]) -> io::Result<usize> {
      let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE).map_err(win_err)? };
      let mut read: u32 = 0;
      unsafe {
          match ReadFile(stdin, Some(buf), Some(&mut read), None) {
              Ok(()) => Ok(read as usize),
              // A closed input handle surfaces as ERROR_BROKEN_PIPE; treat as EOF.
              // verify: WIN32_ERROR::to_hresult() exists in windows 0.58.
              Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
              Err(e) => Err(win_err(e)),
          }
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      /// Manual smoke test — requires a real interactive console. Run with:
      ///   cargo test -p winmux --lib host::tests::manual_enter_and_restore -- --ignored
      /// Watch the terminal: it should enter alt-screen, print the size line, then
      /// restore cleanly (cursor visible, normal screen, echo back on).
      #[test]
      #[ignore = "manual: requires a real attached console; run with --ignored"]
      fn manual_enter_and_restore() {
          let mut host = Host::enter().expect("enter raw mode");
          let (cols, rows) = host.size().expect("query size");
          assert!(cols > 0 && rows > 0, "console reported a zero dimension");
          host.write(format!("winmux host smoke: {cols}x{rows}\r\n").as_bytes())
              .expect("write to host");
          std::thread::sleep(std::time::Duration::from_millis(500));
          drop(host); // restoration runs here
      }
  }
  ```

- [ ] **Step 2: Build clean.** The only automated gate for this module is compilation.
  ```
  cargo build
  ```
  Expected: builds with no errors. If a console-mode constant or a Win32 function fails to resolve, fix the import path per the windows 0.58 docs (the flags are `CONSOLE_MODE` values under `Win32::System::Console`; `ReadFile`/`WriteFile` are under `Win32::Storage::FileSystem`). Do **not** proceed until `cargo build` is green.

- [ ] **Step 3: (Optional, human) run the manual smoke test** in a real terminal to eyeball restoration:
  ```
  cargo test host::tests::manual_enter_and_restore -- --ignored
  ```
  Not part of the CI gate; documented here so the behavior can be verified by hand.

- [ ] **Step 4: Commit.**
  ```
  git add src/host.rs src/lib.rs
  git commit -m "feat(host): raw-mode console control with infallible restoration"
  ```

---

### Task 10: `pty` ConPTY wrapper + integration smoke test

**Files:**
- Test: `tests/pty_smoke.rs` (write first — TDD)
- `src/pty.rs` (implement) — the entire `pty` module
- `src/lib.rs` (verify only) — src/lib.rs already declares the module (Task 1). The library target is what lets `tests/pty_smoke.rs` do `use winmux::pty::Pty`.

**Interfaces:**

Produces (must match the locked contract exactly):
```rust
pub struct Pty { /* private: HPCON, process + thread handles, pipe handles */ }
pub struct PtyReader { /* private: owned dup of the output read handle */ }
impl Pty {
    pub fn spawn(cmdline: &str, cols: u16, rows: u16) -> std::io::Result<Pty>;
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()>;
    pub fn take_reader(&mut self) -> std::io::Result<PtyReader>; // once; Err on second call
    pub fn write_input(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    pub fn process_handle_raw(&self) -> isize;
    pub fn pid(&self) -> u32;
}
impl std::io::Read for PtyReader { /* ERROR_BROKEN_PIPE -> Ok(0) */ }
impl Drop for Pty { /* TerminateProcess -> ClosePseudoConsole -> CloseHandle */ }
```
Consumes: `windows` 0.58 (a normal dependency; normal dependencies are available to integration tests, so `tests/pty_smoke.rs` may `use windows::...` directly. verify: if that import fails to resolve in the test target, mirror the `Win32_Foundation` + `Win32_System_Threading` features into `[dev-dependencies].windows`).

**Thread-safety notes (contract-driven):**
- `PtyReader` owns only a `std::fs::File` (the output read pipe end), which is `Send`. So `PtyReader: Send` is derived automatically — it can move to the dedicated reader thread. No `unsafe impl` needed.
- `Pty` holds raw `HPCON`/`HANDLE` pointers, so it is `!Send` and stays on the main thread. The waiter thread never touches the `Pty`; it gets the process handle value via `process_handle_raw() -> isize` and reconstructs a `HANDLE` locally. A Windows process `HANDLE` is safe to `WaitForSingleObject` on from another thread while the owner keeps it open — waiting does not mutate or consume the handle.

- [ ] **Step 1: Write the integration test FIRST (`tests/pty_smoke.rs`).** It will not compile until `pty` exists — that is the intended red state.

  ```rust
  //! Integration smoke tests for the ConPTY wrapper. These spawn a real child
  //! process, so they only run on Windows with ConPTY available (build 26200 has it).

  use std::ffi::c_void;
  use std::io::Read;
  use std::sync::mpsc;
  use std::thread;
  use std::time::{Duration, Instant};

  use winmux::pty::Pty;

  // verify these import paths against windows 0.58 when compiling:
  // WAIT_OBJECT_0 & HANDLE live in Win32::Foundation; WaitForSingleObject in
  // Win32::System::Threading.
  use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
  use windows::Win32::System::Threading::WaitForSingleObject;

  /// Output written by the child must flow through the pseudoconsole to our reader.
  #[test]
  fn echo_output_flows_through_conpty() {
      let mut pty = Pty::spawn("cmd.exe /c echo winmux-smoke", 80, 24)
          .expect("spawn cmd.exe through ConPTY");
      let mut reader = pty.take_reader().expect("take reader once");

      // Read on a dedicated thread and stream chunks back; ConPTY's output pipe
      // does NOT reliably EOF when the child exits, so we must NOT wait for Ok(0).
      let (tx, rx) = mpsc::channel::<Vec<u8>>();
      thread::spawn(move || {
          let mut buf = [0u8; 4096];
          loop {
              match reader.read(&mut buf) {
                  Ok(0) => break, // EOF once the main thread drops `pty`
                  Ok(n) => {
                      if tx.send(buf[..n].to_vec()).is_err() {
                          break;
                      }
                  }
                  Err(_) => break,
              }
          }
      });

      // Collect until we observe the marker or hit a 10s deadline.
      let deadline = Instant::now() + Duration::from_secs(10);
      let mut collected: Vec<u8> = Vec::new();
      loop {
          let remaining = deadline
              .checked_duration_since(Instant::now())
              .unwrap_or(Duration::ZERO);
          match rx.recv_timeout(remaining) {
              Ok(chunk) => {
                  collected.extend_from_slice(&chunk);
                  if String::from_utf8_lossy(&collected).contains("winmux-smoke") {
                      // Success: dropping `pty` here closes the pseudoconsole,
                      // unblocking the reader thread so it can exit cleanly.
                      return;
                  }
              }
              Err(_) => break, // timeout or sender gone
          }
          if Instant::now() >= deadline {
              break;
          }
      }

      panic!(
          "did not observe 'winmux-smoke' in ConPTY output within 10s; got:\n{}",
          String::from_utf8_lossy(&collected)
      );
  }

  /// The exit-waiter protocol: a child that exits immediately must signal its
  /// process handle so a waiter thread's WaitForSingleObject returns.
  #[test]
  fn child_exit_is_observable_via_wait() {
      let pty = Pty::spawn("cmd.exe /c exit 0", 80, 24).expect("spawn cmd.exe");
      let raw = pty.process_handle_raw();
      let status = unsafe { WaitForSingleObject(HANDLE(raw as *mut c_void), 10_000) };
      assert_eq!(
          status, WAIT_OBJECT_0,
          "process handle did not signal exit within 10s (got {status:?})"
      );
      // `pty` drops here: TerminateProcess on an already-dead process is a no-op,
      // ClosePseudoConsole + handle closes are harmless.
  }
  ```

- [ ] **Step 2: Run the test to confirm it fails to compile (red).**
  ```
  cargo test --test pty_smoke
  ```
  Expected: compile error — `unresolved import winmux::pty` / `Pty` not found. This confirms the test targets the not-yet-written API. Do not implement until you have seen this failure.

- [ ] **Step 3: Implement the full `pty` module (`src/pty.rs`).**

  ```rust
  //! ConPTY wrapper: create pipes + pseudoconsole, spawn a child under it,
  //! read/write its VT stream, resize it, and tear everything down on Drop.

  use std::ffi::c_void;
  use std::fs::File;
  use std::io::{self, Read, Write};
  use std::os::windows::io::{FromRawHandle, RawHandle};
  use std::ptr;

  use windows::core::{PCWSTR, PWSTR};
  use windows::Win32::Foundation::{CloseHandle, ERROR_BROKEN_PIPE, HANDLE};
  use windows::Win32::System::Console::{
      ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
  };
  use windows::Win32::System::Pipes::CreatePipe;
  use windows::Win32::System::Threading::{
      CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
      TerminateProcess, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT,
      LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, STARTUPINFOEXW,
  };
  // verify exact import paths against windows 0.58 docs when compiling:
  // CreatePipe is under Win32::System::Pipes; CreatePseudoConsole / HPCON / COORD
  // under Win32::System::Console; the ProcThreadAttribute + CreateProcessW family
  // under Win32::System::Threading.

  /// Not exported as a named constant by windows 0.58; define it ourselves.
  const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;

  /// Map a `windows::core::Error` into a `std::io::Error` (HRESULT as raw OS error).
  fn win_err(e: windows::core::Error) -> io::Error {
      io::Error::from_raw_os_error(e.code().0)
  }

  pub struct Pty {
      hpcon: HPCON,
      process: HANDLE,
      pid: u32,
      /// Write end of the input pipe (our stdout -> child stdin). `File` is Send
      /// and closes the handle on drop.
      input: File,
      /// Read end of the output pipe (child stdout -> us). Moved out by
      /// `take_reader`; `None` afterwards.
      reader: Option<File>,
  }

  pub struct PtyReader {
      file: File,
  }

  impl Pty {
      pub fn spawn(cmdline: &str, cols: u16, rows: u16) -> io::Result<Pty> {
          unsafe {
              // 1. Two anonymous pipes. Child stdin = in_read; we write in_write.
              //    Child stdout = out_write; we read out_read.
              let mut in_read = HANDLE::default();
              let mut in_write = HANDLE::default();
              let mut out_read = HANDLE::default();
              let mut out_write = HANDLE::default();
              CreatePipe(&mut in_read, &mut in_write, None, 0).map_err(win_err)?;
              CreatePipe(&mut out_read, &mut out_write, None, 0).map_err(win_err)?;

              // 2. Create the pseudoconsole from the child's pipe ends.
              let size = COORD { X: cols as i16, Y: rows as i16 };
              let hpcon: HPCON =
                  CreatePseudoConsole(size, in_read, out_write, 0).map_err(win_err)?;

              // 3. ConPTY now owns duplicates of in_read + out_write; close our
              //    local copies. We keep in_write (to child stdin) and out_read
              //    (from child stdout).
              let _ = CloseHandle(in_read);
              let _ = CloseHandle(out_write);

              // 4. Size the process/thread attribute list (two-call pattern: the
              //    first call is EXPECTED to fail with ERROR_INSUFFICIENT_BUFFER
              //    and only fills in `bytes_required`).
              let mut bytes_required: usize = 0;
              let _ = InitializeProcThreadAttributeList(
                  LPPROC_THREAD_ATTRIBUTE_LIST(ptr::null_mut()),
                  1,
                  0,
                  &mut bytes_required,
              );
              let mut attr_buf: Vec<u8> = vec![0u8; bytes_required];
              let attr_list =
                  LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
              InitializeProcThreadAttributeList(attr_list, 1, 0, &mut bytes_required)
                  .map_err(win_err)?;

              // 5. Attach the pseudoconsole to the attribute list.
              //    verify: in windows 0.58 the trailing out params of
              //    UpdateProcThreadAttribute may be `Option<*mut ...>` instead of
              //    raw pointers; if so pass `None, None` instead of null pointers.
              UpdateProcThreadAttribute(
                  attr_list,
                  0,
                  PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                  hpcon.0 as *const c_void,
                  std::mem::size_of::<HPCON>(),
                  ptr::null_mut(),
                  ptr::null_mut(),
              )
              .map_err(win_err)?;

              // 6. STARTUPINFOEXW with cb = size of the extended struct and the
              //    attribute list attached.
              let mut si_ex = STARTUPINFOEXW::default();
              si_ex.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
              si_ex.lpAttributeList = attr_list;

              // 7. CreateProcessW may write to the command-line buffer, so it must
              //    be a mutable, NUL-terminated UTF-16 Vec wrapped as PWSTR.
              let mut cmd_utf16: Vec<u16> =
                  cmdline.encode_utf16().chain(std::iter::once(0)).collect();

              let mut pi = PROCESS_INFORMATION::default();
              CreateProcessW(
                  PCWSTR::null(),
                  PWSTR(cmd_utf16.as_mut_ptr()),
                  None,                          // process security attributes
                  None,                          // thread security attributes
                  false,                         // bInheritHandles
                  EXTENDED_STARTUPINFO_PRESENT,  // dwCreationFlags
                  None,                          // environment
                  PCWSTR::null(),                // current directory
                  &si_ex.StartupInfo,            // *const STARTUPINFOW (first field)
                  &mut pi,
              )
              .map_err(win_err)?;
              // verify: in windows 0.58 the bInheritHandles BOOL param may require
              // `false.into()` or `BOOL(0)` instead of a bare `false`.

              // 8. Free the attribute list; close the child's thread handle (we do
              //    not need it) but KEEP the process handle for waiting.
              DeleteProcThreadAttributeList(attr_list);
              let _ = CloseHandle(pi.hThread);

              // 9. Wrap our pipe ends as `std::fs::File` (Send, RAII-closing) for
              //    cross-thread blocking I/O. HANDLE.0 is *mut c_void == RawHandle.
              let input = File::from_raw_handle(in_write.0 as RawHandle);
              let reader = File::from_raw_handle(out_read.0 as RawHandle);

              Ok(Pty {
                  hpcon,
                  process: pi.hProcess,
                  pid: pi.dwProcessId,
                  input,
                  reader: Some(reader),
              })
          }
      }

      pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
          let size = COORD { X: cols as i16, Y: rows as i16 };
          unsafe { ResizePseudoConsole(self.hpcon, size).map_err(win_err) }
      }

      pub fn take_reader(&mut self) -> io::Result<PtyReader> {
          match self.reader.take() {
              Some(file) => Ok(PtyReader { file }),
              None => Err(io::Error::new(
                  io::ErrorKind::Other,
                  "pty reader already taken",
              )),
          }
      }

      pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<()> {
          self.input.write_all(bytes)?;
          self.input.flush()
      }

      /// Raw process HANDLE value for a waiter thread. The Pty retains ownership;
      /// the waiter only reads/waits on it (safe cross-thread on Windows).
      pub fn process_handle_raw(&self) -> isize {
          self.process.0 as isize
      }

      pub fn pid(&self) -> u32 {
          self.pid
      }
  }

  impl Read for PtyReader {
      fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
          match self.file.read(buf) {
              Ok(n) => Ok(n),
              // ConPTY closes the output pipe on teardown; std surfaces the real
              // Win32 code (109) as raw_os_error. Map ERROR_BROKEN_PIPE to EOF.
              Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE.0 as i32) => Ok(0),
              Err(e) => Err(e),
          }
      }
  }

  impl Drop for Pty {
      fn drop(&mut self) {
          // Order matters: kill the child first, then close the pseudoconsole
          // (which unblocks any reader stuck in ReadFile on the output pipe), then
          // close the process handle. The `input` File field closes the input pipe
          // when it is dropped after this body runs. All errors are ignored.
          unsafe {
              let _ = TerminateProcess(self.process, 0);
              ClosePseudoConsole(self.hpcon);
              let _ = CloseHandle(self.process);
          }
      }
  }
  ```

- [ ] **Step 4: Run the integration test — expect green.**
  ```
  cargo test --test pty_smoke
  ```
  Expected: both `echo_output_flows_through_conpty` and `child_exit_is_observable_via_wait` pass on Windows 11 build 26200. If `echo_...` times out, the most likely cause is a botched pipe/handle wiring in `spawn` (child stdout not routed to `out_write`) — re-check steps 1–3 of `spawn`. If it fails to compile, resolve the `verify:` notes (BOOL param, UpdateProcThreadAttribute out-params) against the windows 0.58 docs.

- [ ] **Step 5: Confirm the full build is still clean** (the `app`/reader-thread consumers link against this API):
  ```
  cargo build
  ```

- [ ] **Step 6: Commit.**
  ```
  git add src/pty.rs src/lib.rs tests/pty_smoke.rs
  git commit -m "feat(pty): ConPTY wrapper with spawn/resize/reader and exit-waiter smoke tests"
  ```
### Task 11: `app` event loop + `main`

**Files:**
- Modify: `C:\Users\poon\developments\winmux\src\app.rs` (replace the Task 1 stub `pub fn run()` with the full implementation)
- Modify: `C:\Users\poon\developments\winmux\Cargo.toml` (add one `windows` feature)

**Interfaces (Consumes — exact signatures from the locked contract):**
- `geom`: `struct Rect { x, y, w, h: u16 }`, `enum Direction`
- `layout`: `Layout::new(PaneId)`, `split(&mut, SplitDir, PaneId, Rect) -> Result<(), SplitRefused>`, `focused() -> PaneId`, `focus_dir(&mut, Direction, Rect) -> bool`, `focus_next(&mut)`, `focus_last(&mut)`, `remove(&mut, PaneId) -> bool`, `resize_focused(&mut, Direction, Rect, u16) -> bool`, `toggle_zoom(&mut)`, `is_zoomed() -> bool`, `rects(&self, Rect) -> Vec<(PaneId, Rect)>`; consts `MIN_PANE_W`, `MIN_PANE_H`; `type PaneId = u32`
- `grid`: `Grid::new(u16, u16)`, `feed(&mut, &[u8])`, `resize(&mut, u16, u16)`, `cursor() -> (u16, u16)`, `cursor_visible() -> bool`
- `render`: `Renderer::new(u16, u16)`, `resize(&mut, u16, u16)`, `compose(&mut, &Scene, Option<(u16,u16)>, bool) -> Vec<u8>`; `struct PaneView<'a>`, `struct Scene<'a>`
- `input`: `InputMachine::new()`, `feed(&mut, &[u8], Instant) -> Vec<InputEvent>`, `set_confirming(&mut, bool)`; `enum Action`, `enum InputEvent`
- `pty`: `Pty::spawn(&str, u16, u16) -> io::Result<Pty>`, `resize(&self, u16, u16) -> io::Result<()>`, `take_reader(&mut) -> io::Result<PtyReader>`, `write_input(&mut, &[u8]) -> io::Result<()>`, `process_handle_raw(&self) -> isize`; `impl Read for PtyReader`
- `host`: `Host::enter() -> io::Result<Host>`, `size(&self) -> io::Result<(u16,u16)>`, `write(&mut, &[u8]) -> io::Result<()>`, `install_panic_hook()`, `read_stdin(&mut [u8]) -> io::Result<usize>`
- Win32: `GetLocalTime(*mut SYSTEMTIME)` (feature `Win32_System_SystemInformation`), `WaitForSingleObject(HANDLE, u32)`, `INFINITE`

> Note on this task: `app::run()` is the I/O composition root — it owns real Windows handles and threads and cannot be meaningfully unit-tested. **It gets NO unit tests by design.** Its correctness is proven end-to-end by Task 12's `tests/e2e.rs`.

> Note on the clock: the contract's status-right is local time `"HH:MM DD-Mon-YY"`. Win32 `GetLocalTime` already returns a broken-down local `SYSTEMTIME` (year/month/day/hour/minute), so no days-since-epoch → y/m/d conversion (e.g. Howard Hinnant's `civil_from_days`) is needed — adding one would be **unused dead code and fail `cargo clippy -- -D warnings`**. UTC via `std::time::SystemTime` is deliberately rejected (a clock must be local). This is the honest minimal implementation and keeps the clippy gate green.

> Note on `src/main.rs`: it already contains the final entry point from Task 1 (`use winmux::{app, host};` → `host::install_panic_hook();` → `app::run()` → on `Err` print to stderr and exit 1 — the terminal is already restored because `Host` is dropped inside `run` before it returns). Do NOT touch it in this task.

- [ ] **Step 1: Add the `Win32_System_SystemInformation` feature to `Cargo.toml`.**
  Edit the `windows` dependency's `features` list so it reads exactly (keep all other keys/versions Tasks 1–10 set):
  ```toml
  [dependencies]
  vte = "0.13"
  windows = { version = "0.58", features = [
      "Win32_Foundation",
      "Win32_Security",
      "Win32_Storage_FileSystem",
      "Win32_System_Console",
      "Win32_System_Pipes",
      "Win32_System_SystemInformation",
      "Win32_System_Threading",
  ] }
  ```

- [ ] **Step 2: Replace `src/app.rs` with the full implementation.**
  ```rust
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

  use windows::Win32::Foundation::{HANDLE, SYSTEMTIME};
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
      let mut st = SYSTEMTIME::default();
      // SAFETY: `st` is a valid owned SYSTEMTIME; GetLocalTime only writes to it.
      unsafe { GetLocalTime(&mut st) };
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
  ```

- [ ] **Step 3: Build.**
  Run:
  ```
  cargo build
  ```
  Expected final line:
  ```
      Finished `dev` profile [unoptimized + debuginfo] target(s) in <N>s
  ```

- [ ] **Step 4: Lint (deny all warnings) and fix.**
  Run:
  ```
  cargo clippy --all-targets -- -D warnings
  ```
  Expected final line (zero warnings):
  ```
      Finished `dev` profile [unoptimized + debuginfo] target(s) in <N>s
  ```
  If clippy reports anything, fix it and re-run until clean.

- [ ] **Step 5: Commit.**
  ```
  git add Cargo.toml src/app.rs
  git commit -m "feat: app event loop and local status-bar clock

  Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
  ```

---

### Task 12: End-to-end test + README

**Files:**
- Create: `C:\Users\poon\developments\winmux\tests\e2e.rs`
- Create: `C:\Users\poon\developments\winmux\README.md`

**Interfaces (Consumes — exact signatures from the locked contract):**
- `winmux::pty`: `Pty::spawn(&str, u16, u16) -> io::Result<Pty>`, `take_reader(&mut) -> io::Result<PtyReader>`, `write_input(&mut, &[u8]) -> io::Result<()>`, `process_handle_raw(&self) -> isize`; `impl Read for PtyReader`
- `winmux::grid`: `Grid::new(u16, u16)`, `feed(&mut, &[u8])`, `cols() -> u16`, `rows() -> u16`, `cell(u16, u16) -> Cell` (field `ch: char`)
- Win32 (via the `windows` dep, available to integration tests): `WaitForSingleObject(HANDLE, u32)`, `WAIT_OBJECT_0`
- Cargo: `env!("CARGO_BIN_EXE_winmux")` — absolute path to the built binary, set by Cargo before integration tests run.

> Rationale: the crate is lib+bin (from Task 1: `src/lib.rs` declares all modules `pub mod`), so the integration test can use the crate's OWN `pty` module to spawn the real `winmux.exe` inside a ConPTY and its OWN `grid::Grid` emulator to decode the output. The raw output buffer is append-only VT soup and not directly assertable; feeding it through `Grid` and asserting on decoded screen CONTENTS is the honest way.

- [ ] **Step 1: Write `tests/e2e.rs` in full.**
  ```rust
  //! End-to-end test: spawn the built winmux binary inside a ConPTY (using
  //! winmux's OWN pty module), drive it via keystrokes, and assert on the
  //! decoded screen by feeding its output into winmux's OWN grid emulator.
  //!
  //! Flow: wait for the status bar → split vertically (Ctrl-b %) → confirm a
  //! border column appears → kill the new pane (Ctrl-b x, y) → confirm the
  //! border disappears → `exit` the last shell → assert winmux exits cleanly.

  use std::io::Read;
  use std::sync::mpsc::{channel, Receiver};
  use std::thread;
  use std::time::{Duration, Instant};

  use winmux::grid::Grid;
  use winmux::pty::Pty;

  use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
  use windows::Win32::System::Threading::WaitForSingleObject;

  const COLS: u16 = 80;
  const ROWS: u16 = 24;

  /// Join each grid row's cell chars into a `String`, one entry per row.
  fn screen_text(grid: &Grid) -> Vec<String> {
      let mut out = Vec::with_capacity(grid.rows() as usize);
      for r in 0..grid.rows() {
          let mut line = String::with_capacity(grid.cols() as usize);
          for c in 0..grid.cols() {
              line.push(grid.cell(c, r).ch);
          }
          out.push(line);
      }
      out
  }

  /// Drain all queued output chunks into the emulator.
  fn pump(grid: &mut Grid, rx: &Receiver<Vec<u8>>) {
      while let Ok(chunk) = rx.try_recv() {
          grid.feed(&chunk);
      }
  }

  /// True if some interior column is a full column of `│` across the pane rows
  /// (everything above the bottom status bar) — i.e. a vertical split border.
  fn has_vertical_border(grid: &Grid) -> bool {
      let pane_rows = grid.rows().saturating_sub(1); // exclude status bar
      if pane_rows == 0 {
          return false;
      }
      for c in 1..grid.cols().saturating_sub(1) {
          if (0..pane_rows).all(|r| grid.cell(c, r).ch == '│') {
              return true;
          }
      }
      false
  }

  /// Poll `cond` every 100ms until it is true or the deadline passes.
  fn wait_until<F: FnMut() -> bool>(deadline: Instant, mut cond: F) -> bool {
      loop {
          if cond() {
              return true;
          }
          if Instant::now() >= deadline {
              return false;
          }
          thread::sleep(Duration::from_millis(100));
      }
  }

  /// Non-blocking check: has the process behind `raw` (an isize HANDLE) exited?
  fn process_exited(raw: isize) -> bool {
      // SAFETY: `raw` is winmux's live process HANDLE, owned by the still-alive
      // Pty; WaitForSingleObject with timeout 0 only queries its signaled state.
      unsafe { WaitForSingleObject(HANDLE(raw as *mut core::ffi::c_void), 0) == WAIT_OBJECT_0 }
  }

  #[test]
  fn e2e_split_kill_exit() {
      // Quote the exe path (it may contain spaces) for the ConPTY command line.
      let cmdline = format!("\"{}\"", env!("CARGO_BIN_EXE_winmux"));
      let mut pty = Pty::spawn(&cmdline, COLS, ROWS).expect("spawn winmux under ConPTY");
      let proc_raw = pty.process_handle_raw();
      let mut reader = pty.take_reader().expect("take winmux output reader");

      // Reader thread → channel of raw output chunks (fed into the grid below).
      let (tx, rx) = channel::<Vec<u8>>();
      thread::spawn(move || {
          let mut buf = [0u8; 4096];
          loop {
              match reader.read(&mut buf) {
                  Ok(0) => break,
                  Ok(n) => {
                      if tx.send(buf[..n].to_vec()).is_err() {
                          break;
                      }
                  }
                  Err(_) => break,
              }
          }
      });

      let mut grid = Grid::new(COLS, ROWS);

      // 1. Status bar appears.
      let deadline = Instant::now() + Duration::from_secs(15);
      assert!(
          wait_until(deadline, || {
              pump(&mut grid, &rx);
              screen_text(&grid).iter().any(|l| l.contains("[winmux]"))
          }),
          "status-bar marker '[winmux]' never appeared"
      );

      // 2. Split vertically: Ctrl-b %  → a `│` border column appears.
      pty.write_input(b"\x02%").expect("send split");
      let deadline = Instant::now() + Duration::from_secs(15);
      assert!(
          wait_until(deadline, || {
              pump(&mut grid, &rx);
              has_vertical_border(&grid)
          }),
          "vertical split border '│' never appeared after Ctrl-b %"
      );

      // 3. Kill the new (focused) pane: Ctrl-b x → wait for the confirm prompt →
      //    y. Waiting for the prompt guarantees winmux armed confirm mode before
      //    the `y` arrives, so `y` is consumed as confirmation, not forwarded.
      pty.write_input(b"\x02x").expect("send kill request");
      let deadline = Instant::now() + Duration::from_secs(15);
      assert!(
          wait_until(deadline, || {
              pump(&mut grid, &rx);
              screen_text(&grid).iter().any(|l| l.contains("kill-pane"))
          }),
          "kill-pane confirm prompt never appeared"
      );
      pty.write_input(b"y").expect("send confirm");

      // 4. Border disappears once the pane is gone.
      let deadline = Instant::now() + Duration::from_secs(15);
      assert!(
          wait_until(deadline, || {
              pump(&mut grid, &rx);
              !has_vertical_border(&grid)
          }),
          "vertical split border '│' never disappeared after kill"
      );

      // 5. Exit the last remaining shell → winmux exits cleanly.
      pty.write_input(b"exit\r").expect("send exit");
      let deadline = Instant::now() + Duration::from_secs(15);
      assert!(
          wait_until(deadline, || process_exited(proc_raw)),
          "winmux process did not exit within 15s after 'exit'"
      );
  }
  ```

- [ ] **Step 2: Write `README.md` in full.**
  ```markdown
  # winmux

  A [tmux](https://github.com/tmux/tmux)-style terminal multiplexer for Windows,
  written in Rust. tmux does not run natively on Windows; winmux gives Windows
  users tmux behavior — splits, focus, resize, zoom, close, a status bar — in
  their existing terminal, matching tmux's real defaults so tmux users are
  immediately at home.

  ## Status

  **Multiplexing MVP.** Single session, single window, multiple PowerShell panes
  hosted via ConPTY, each its own VT emulator, composited with borders and a
  status bar into the host terminal.

  Not yet implemented (planned for later sub-projects): detach/attach, multiple
  sessions/windows, `.tmux.conf`, copy mode, mouse, scrollback.

  ## Requirements

  - Windows 10/11 with a ConPTY-capable terminal (Windows Terminal recommended).
  - Rust (edition 2021) toolchain.

  ## Build

  ```
  cargo build --release
  ```

  The binary is produced at `target/release/winmux.exe`.

  ## Run

  Launch it from Windows Terminal:

  ```
  winmux
  ```

  You get one PowerShell pane. Use the keybindings below to split and manage
  panes. When the last pane's shell exits, winmux exits and restores your
  terminal.

  ## Keybindings

  All commands start with the prefix `Ctrl-b`, exactly like tmux.

  | Key (after prefix) | Action |
  |---|---|
  | `Ctrl-b` | **Prefix** — all commands start here |
  | `%` | Split focused pane **vertically** (left/right) |
  | `"` | Split focused pane **horizontally** (top/bottom) |
  | `←` `↑` `↓` `→` | Move focus to the adjacent pane in that direction |
  | `o` | Cycle focus to the next pane |
  | `;` | Toggle to the last-focused pane |
  | `x` | Close focused pane (with a `y`/`n` confirm prompt) |
  | `z` | Toggle zoom (focused pane fills the window; toggle to restore) |
  | `Ctrl-<arrow>` | Resize the focused pane's split (repeatable) |
  | `Ctrl-b` (again) | Send a literal `Ctrl-b` to the focused pane |

  ## Documentation

  - [Multiplexing MVP — Design](docs/specs/2026-07-06-multiplexing-mvp-design.md)
  - [Multiplexing MVP — Locked Interface Contract](docs/specs/2026-07-06-mvp-interfaces.md)
  ```

- [ ] **Step 3: Run the e2e test alone (it builds the binary first).**
  Run:
  ```
  cargo test --test e2e
  ```
  Expected:
  ```
  running 1 test
  test e2e_split_kill_exit ... ok

  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
  ```

- [ ] **Step 4: Run the full test suite (all module unit tests + e2e).**
  Run:
  ```
  cargo test
  ```
  Expected: every test binary ends with `test result: ok.` and `0 failed`, including `e2e_split_kill_exit ... ok`.

- [ ] **Step 5: Lint everything (deny all warnings) and fix.**
  Run:
  ```
  cargo clippy --all-targets -- -D warnings
  ```
  Expected final line (zero warnings):
  ```
      Finished `dev` profile [unoptimized + debuginfo] target(s) in <N>s
  ```

- [ ] **Step 6: Commit.**
  ```
  git add tests/e2e.rs README.md
  git commit -m "test: end-to-end split/kill/exit harness; add README

  Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
  ```
