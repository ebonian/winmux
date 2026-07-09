# tmux behavioral reference: Panes and layout

Source studied: tmux master `db115c6` (2026-07-07), full source at the time of writing.
All `file:line` references are into that tree. Short C excerpts are quoted where the logic is
subtle. This document is intended to be sufficient to implement tmux-identical pane/layout
behavior **without reopening the tmux source**.

> **Note on this snapshot.** This master includes two features newer than classic tmux 3.x:
> **floating panes** (`new-pane`, `LAYOUT_CELL_FLOATING`, `break-pane -W`, `move-pane -P/-z`,
> z-index list) and **pane scrollbars** (`pane-scrollbars`). Sections below describe the core
> (classic) tiled behavior first and flag floating/scrollbar interactions explicitly as
> *[floating]* / *[scrollbar]*. winmux can ignore those flags entirely and still be an exact
> clone of tmux ≤3.5; every "tiled" code path is unchanged in spirit from 3.x except where
> noted (notably `PANE_MINIMUM`, see §2).

---

## 0. Data model

- **Layout tree** (`layout.c:27-43` comment): each window owns a tree of `layout_cell`s.
  A cell is one of `LAYOUT_LEFTRIGHT` (horizontal row of children, split by vertical lines),
  `LAYOUT_TOPBOTTOM` (vertical stack of children), or `LAYOUT_WINDOWPANE` (leaf holding one
  pane). `w->layout_root` points at the root; `wp->layout_cell` points back at the leaf;
  every cell has `parent`. Cell geometry `lc->g = {sx, sy, xoff, yoff}` is in *window*
  coordinates (0-based, borders excluded — a border line lies *between* cells, so siblings
  in a LEFTRIGHT node satisfy `next.xoff == prev.xoff + prev.sx + 1`).
- **Pane list**: `w->panes` is a TAILQ in *creation/index order*. Pane numbering
  (`window_pane_index`, `window.c:960`) is simply position in this list plus
  `pane-base-index`. Splits insert the new pane immediately **after** the target pane in
  this list (before, with `-b`) — `window_add_pane`, `window.c:851-883`; full-size (`-f`)
  splits insert at the list head (`-b -f`) or tail (`-f`).
- **Last-pane stack**: `w->last_panes` is a most-recently-used stack of previously active
  panes (`window_pane_stack_push/remove`, `window.c:2064-2081`, flag `PANE_VISITED`).
  `window_set_active_pane` pushes the outgoing active pane onto it (`window.c:589-590`).
- **Recency stamp**: every time a pane becomes active it gets
  `wp->active_point = next_active_point++` (`window.c:593`) — a global monotonically
  increasing counter. This is **the tie-breaker for directional navigation** (§1).
- *[floating]* `w->z_index` is a second TAILQ ordering panes front-to-back for drawing.

### Constants (`tmux.h:107-111`)

```c
#define PANE_MINIMUM 1
#define PANE_MAXIMUM 10000
#define WINDOW_MINIMUM PANE_MINIMUM
```

**Warning:** in tmux ≤3.5a `PANE_MINIMUM` was **2**. This master lowered it to 1. winmux
should choose one value and use it consistently everywhere `PANE_MINIMUM` appears below
(split space check, resize floor, preset layouts). Matching released tmux 3.x means 2;
matching this master means 1.

---

## 1. Directional pane navigation (`select-pane -L/-R/-U/-D`)

Implemented by `window_pane_find_left/right/up/down` (`window.c:1960/2012/1838/1899`),
called from `cmd_select_pane_exec` (`cmd-select-pane.c:185-201`). All four share one
algorithm:

1. Compute the current pane's full rectangle (`window_pane_full_size_offset`,
   `window.c:1811-1831`; without scrollbars this is just `wp->xoff/yoff/sx/sy`).
2. Compute the **edge** coordinate the candidate must touch, **wrapping it to the far side
   of the window if the current pane is already on that edge**.
3. Scan **every pane in the window** (`TAILQ_FOREACH(next, &w->panes, entry)`); a pane is a
   candidate iff (a) it is flush against the edge and (b) its perpendicular extent
   *overlaps* the current pane's extent.
4. Of all candidates pick the one with the **greatest `active_point`** (most recently
   active) — `window_pane_choose_best`, `window.c:1790-1805`:

```c
best = list[0];
for (i = 1; i < size; i++) {
        next = list[i];
        if (next->active_point > best->active_point)
                best = next;
}
```

If no candidate exists the function returns NULL and `select-pane` silently does nothing
(`cmd-select-pane.c:202-203` — `if (wp == NULL) return (CMD_RETURN_NORMAL);`).

### 1.1 The wrap/cycle rule (exact code)

**Left** (`window_pane_find_left`, `window.c:1977-1979`): the edge is the column to the left
of the pane; if the pane touches the window's left edge (`xoff == 0`), the edge becomes one
past the window's right edge, so panes flush against the right edge match:

```c
edge = xoff;
if (edge == 0)
        edge = (int)w->sx + 1;
...
TAILQ_FOREACH(next, &w->panes, entry) {
        ...
        if (xoff + (int)sx + 1 != edge)   /* candidate's right border must abut edge */
                continue;
```

A candidate matches when `candidate.xoff + candidate.sx + 1 == edge`. In the wrapped case
`edge == w->sx + 1`, i.e. candidates are exactly the panes whose right side touches the
window's right edge (`xoff + sx == w->sx`). **So pressing Left from a leftmost pane wraps
to the rightmost pane(s) overlapping it vertically; ties go to the most recently active.**

**Right** (`window.c:2029-2031`):

```c
edge = xoff + (int)sx + 1;
if (edge >= (int)w->sx)
        edge = 0;
...
        if (xoff != edge)                 /* candidate's left column == edge */
                continue;
```

Wrap: from a rightmost pane, candidates are panes with `xoff == 0` (leftmost).

**Up** (`window.c:1856-1866`) — the vertical cases must account for `pane-border-status`
occupying a row at the top or bottom of the window:

```c
edge = yoff;
if (status == PANE_STATUS_TOP) {
        if (edge == 1)
                edge = (int)w->sy + 1;
} else if (status == PANE_STATUS_BOTTOM) {
        if (edge == 0)
                edge = (int)w->sy;
} else {
        if (edge == 0)
                edge = (int)w->sy + 1;
}
...
        if (yoff + (int)sy + 1 != edge)   /* candidate's bottom border abuts edge */
                continue;
```

With `pane-border-status off`: topmost pane (`yoff==0`) wraps to panes whose bottom edge
touches the window bottom (`yoff + sy == w->sy`). With `top`, panes start at `yoff==1`; with
`bottom`, the bottommost pane's `yoff + sy == w->sy - 1`, hence the adjusted wrap targets.

**Down** (`window.c:1917-1927`):

```c
edge = yoff + (int)sy + 1;
if (status == PANE_STATUS_TOP) {
        if (edge >= (int)w->sy)
                edge = 1;
} else if (status == PANE_STATUS_BOTTOM) {
        if (edge >= (int)w->sy - 1)
                edge = 0;
} else {
        if (edge >= (int)w->sy)
                edge = 0;
}
...
        if (yoff != edge)
                continue;
```

### 1.2 The overlap test (identical shape in all four, e.g. `window.c:1992-1998` for left)

For left/right the perpendicular axis is vertical; `top = yoff`, `bottom = yoff + sy` of the
*current* pane (note: `bottom`/`right` are one **past** the last cell, i.e. the border row):

```c
end = yoff + (int)sy - 1;         /* candidate's last row */
found = 0;
if (yoff < top && end > bottom)   /* candidate strictly contains current */
        found = 1;
else if (yoff >= top && yoff <= bottom)   /* candidate's top inside current range */
        found = 1;
else if (end >= top && end <= bottom)     /* candidate's bottom inside current range */
        found = 1;
```

Because the range extends to `bottom = yoff + sy` (the border line), a candidate that only
touches diagonally-adjacent at the shared border corner still counts as overlapping. Up/down
use `left = xoff`, `right = xoff + sx` and the candidate's `end = xoff + sx - 1`
(`window.c:1868-1885`). Note a quirk: `find_up` computes left/right from the *full size
offset* (scrollbar-inclusive), while `find_down`/`find_left`/`find_right` compute the
current pane's perpendicular range from raw `wp->xoff/wp->yoff` (`window.c:1929-1930`,
`2033-2034`) — without scrollbars these are identical.

### 1.3 Zoom interaction

Each directional lookup is bracketed by `window_push_zoom(w, 0, 1)` … find …
`window_pop_zoom(w)` (`cmd-select-pane.c:185-201`): the window is temporarily unzoomed so
real geometry is used, then re-zoomed. The actual pane *switch* then runs
`window_push_zoom(w, 0, args_has('Z'))` (`cmd-select-pane.c:235`): with `-Z` the zoom is
re-applied to the **new** pane after switching (`window_pop_zoom` re-zooms `w->active`,
`window.c:841-848`); without `-Z`, `window_set_active_pane` itself unzooms
(`window.c:585-586`) and the window stays unzoomed. `window_push_zoom(w, always, flag)`
(`window.c:829-838`) sets `WINDOW_WASZOOMED` iff `flag && (always || currently zoomed)`
and always unzooms.

### 1.4 select-pane flags (all: `cmd-select-pane.c`)

Args spec: `"DdegLlMmP:RT:t:UZ"` (`-P` and `-g` deprecated) — `cmd-select-pane.c:36`.

- **`-t target`**: any pane target; without direction flags simply makes that pane active.
  If the target already is the active pane, returns without doing anything
  (`cmd-select-pane.c:233-234`). Afterwards fires the `after-select-pane` hook and redraws
  borders only (full redraw only if the window is bigger than the client —
  `cmd_select_pane_redraw`, `cmd-select-pane.c:58-81`).
- **`-l` / `last-pane`** (`cmd-select-pane.c:100-135`): target is the head of
  `w->last_panes`. Fallback: if the stack is empty **and the window has exactly 2 panes**,
  use the pane before (else after) the active one in the pane list (`window.c` comment:
  covers `split-window -d` where the other pane was never visited). If still none: error
  `"no last pane"`. `-e`/`-d` combined with `-l` enable/disable input on the last pane
  *without switching*. Otherwise switches with the same push/pop zoom dance (`-Z`
  supported).
- **`-e` / `-d`** (`cmd-select-pane.c:205-216`): clear / set the `PANE_INPUTOFF` flag on the
  target pane (input enable/disable), redraw borders + status; do **not** change the active
  pane.
- **`-m` / `-M`** (`cmd-select-pane.c:137-168`): `-m` sets the *marked pane* (a single
  global `marked_pane` = session+winlink+pane); `-m` on the currently marked pane toggles it
  off (`server_is_marked` check); `-M` clears any mark. Both redraw the borders of the
  previously and newly marked panes. `-m` on a pane that is not visible (zoomed away) is a
  no-op. The marked pane is the default `-s` source for `swap-pane`/`join-pane`
  (`CMD_FIND_DEFAULT_MARKED`), and the marked pane's border is drawn with reverse video
  (`screen-redraw.c:1191-1194`: `gc.attr ^= GRID_ATTR_REVERSE`).
- **`-T title`** (`cmd-select-pane.c:218-227`): format-expand the argument and set the
  pane's title (`screen_set_title`); fires `pane-title-changed`.
- **`-P style` / `-g`** (deprecated, `cmd-select-pane.c:170-183`): set/print the pane's
  `window-style` (and `window-active-style`).
- **Nothing else given**: plain activation via `window_set_active_pane(w, wp, 1)`.

### 1.5 `window_set_active_pane` (`window.c:576-607`) — canonical activation

```c
if (wp == w->active)
        return (0);
if (w->flags & WINDOW_ZOOMED)
        window_unzoom(w, 1);
lastwp = w->active;
window_pane_stack_remove(&w->last_panes, wp);
window_pane_stack_push(&w->last_panes, lastwp);
w->active = wp;
w->active->active_point = next_active_point++;
w->active->flags |= PANE_CHANGED;
```

Every activation: (1) implicit unzoom, (2) update last-panes stack, (3) bump
`active_point`, (4) full window redraw, (5) `window-pane-changed` notify (if `notify`).

Other index-based helpers: `window_pane_at_index` (`window.c:922`, walks the list counting
from `pane-base-index`), `window_pane_next_by_number`/`previous_by_number`
(`window.c:937/948`, wrap around the list ends) — these back `-t .+`/`.-` style targets
(default binding `o` is `select-pane -t:.+`, i.e. next pane by index, wrapping).

---

## 2. Splitting (`split-window`, `cmd-split-window.c` + `layout.c`)

Args spec (`cmd-split-window.c:60`): `"bc:de:EfF:hIkl:m:p:PR:s:S:t:T:vWZ"`.

Flag semantics:

| Flag | Meaning |
|---|---|
| `-h` | horizontal split → `LAYOUT_LEFTRIGHT` (panes side by side). Default (or `-v`) is `LAYOUT_TOPBOTTOM` (`layout.c:1611-1613`). |
| `-b` | new pane goes **before** (left of / above) the target: `SPAWN_BEFORE`. |
| `-f` | full window width/height: `SPAWN_FULLSIZE` — splits the **root** cell instead of the target pane's cell. |
| `-l size` | absolute size in cells, **or** `N%` (percentage — `args_percentage_and_expand`, `layout.c:1629-1631`). Applies to the *new* pane. |
| `-p pct` | (older form) percentage 0-100 of the current value: `size = curval * p / 100` (`layout.c:1632-1637`). |
| `-d` | do not make the new pane the active pane (`SPAWN_DETACHED`). |
| `-c cwd` | start directory (format-expanded). |
| `-e VAR=val` | extra environment (repeatable). |
| `-Z` | zoom the new pane after splitting (`SPAWN_ZOOM`). |
| `-P` / `-F fmt` | print info about the new pane; default template `#{session_name}:#{window_index}.#{pane_index}` (`cmd-split-window.c:33`). |
| `-I`/`-E`/`-k`/`-m`/`-W`/`-s`/`-S`/`-R`/`-T` | stdin-fed empty pane / empty pane / remain-on-exit + message / wait-for-exit / per-pane styles / title. Secondary for winmux. |

The percentage/size base `curval` (`layout.c:1615-1627`): with `-f` it is the *window's*
`sy` (for `-v`) or `sx` (for `-h`); otherwise the *target pane's* `wp->sy`/`wp->sx`.

### 2.1 Space check (`layout_split_check_space`, `layout.c:1255-1291`)

```c
case LAYOUT_LEFTRIGHT:
        minimum = PANE_MINIMUM * 2 + 1;      /* (scrollbars variant omitted) */
        if (sx < minimum) return (0);
case LAYOUT_TOPBOTTOM:
        if (layout_add_horizontal_border(...))   /* pane-border-status row applies */
                minimum = PANE_MINIMUM * 2 + 2;
        else
                minimum = PANE_MINIMUM * 2 + 1;
        if (sy < minimum) return (0);
```

Failure ⇒ `layout_split_pane` returns NULL ⇒ error **"no space for a new pane"**
(`layout.c:1650-1652`), command fails, nothing spawned.

### 2.2 Size math (`layout_split_sizes`, `layout.c:1293-1320`)

`ss` = the split dimension of the cell being split; `size` = requested new-pane size or -1;
`size2` = size of the **new** pane (when not `-b`), `size1` = remaining old-pane side:

```c
if (size < 0)
        s2 = ((ss + 1) / 2) - 1;         /* default: even split, new pane may be 1 smaller */
else if (before)
        s2 = ss - size - 1;
else
        s2 = size;
if (s2 < PANE_MINIMUM)
        s2 = PANE_MINIMUM;
else if (s2 > sx - 2)                    /* NB: compares against sx even for -v (upstream quirk) */
        s2 = ss - 2;
s1 = ss - 1 - s2;
```

So for `ss=80`: default split gives s2=39, s1=40 (old pane keeps the extra cell; 1 cell for
the border). With `-b`, `size1` (top/left) becomes the new cell and `size2` the old, so the
requested size still applies to the new pane.

### 2.3 Tree surgery (`layout_split_pane`, `layout.c:1326-1453`)

Three cases:

1. **Parent is same type** (`layout.c:1371-1381`): create a new sibling cell inserted
   directly after (or before with `-b`) the split cell in the parent's child list. This is
   why repeatedly pressing `%` produces flat N-way LEFTRIGHT nodes, not nested pairs.
2. **`-f` full-size and root is same type** (`layout.c:1382-1410`): shrink all existing
   children proportionally (`layout_resize_child_cells`) to make room, then append/prepend
   the new cell to the root.
3. **Otherwise** (`layout.c:1411-1425`): replace the split cell with a new node of the split
   type (`layout_replace_with_node`, `layout.c:1233-1252`), re-insert the old cell as its
   first child, and add the new cell after/before it.

Then geometry is assigned (`layout.c:1438-1444`):

```c
layout_set_size(lc1, size1, sy, xoff, yoff);
layout_set_size(lc2, size2, sy, xoff + lc1->g.sx + 1, yoff);   /* LEFTRIGHT */
```

For `-f`, `layout_resize_child_cells` + `layout_fix_offsets` finish the job. The pane is
attached with `layout_assign_pane` and pane rectangles are recomputed by
`layout_fix_panes` (`layout.c:432-499`), which subtracts the pane-border-status row for
top/bottom edge panes (§7.3) and clamps to the layout cell geometry.

### 2.4 Zoom + focus after split

`layout_get_tiled_cell` calls `window_push_zoom(wp->window, 1, args_has('Z'))`
(`layout.c:1649`) — **always unzooms** before splitting (the `always=1` argument means: with
`-Z`, remember zoom state even if not currently zoomed, so `-Z` zooms afterwards).
After spawn, `cmd_split_window_exec` calls `window_pop_zoom` (`cmd-split-window.c:239`).

Focus: `spawn_pane` (`spawn.c:527-532`) —

```c
if ((~sc->flags & SPAWN_DETACHED) || w->active == NULL) {
        window_set_active_pane(w, new_wp, 1);
}
```

i.e. the **new pane becomes active unless `-d`**. With `SPAWN_ZOOM`,
`layout_assign_pane(sc->lc, new_wp, 1)` skips the pane resize (`do_not_resize`,
`layout.c:1063-1072`) since the zoom will re-layout anyway.

`join-pane` reuses `layout_get_tiled_cell` and thus supports `-h/-v/-b/-f/-l/-p` with
identical math (§5.4).

---

## 3. Layout engine

### 3.1 Resize bookkeeping primitives

- **`layout_resize_check(w, lc, type)`** (`layout.c:522-570`): how much can `lc` shrink in
  direction `type`? Leaf: `available = size - PANE_MINIMUM` (plus 1 for a border-status row
  in TOPBOTTOM; floored at 0). Node of same type: **sum** of children. Node of other type:
  **min** of children.
- **`layout_resize_adjust(w, lc, type, change)`** (`layout.c:576-637`): apply a delta to a
  cell. Leaf: just adjust `g.sx/g.sy`. Node of other type: apply same delta to every child.
  Node of same type: distribute **one cell at a time round-robin across children** — grow
  gives +1 to each child in list order repeatedly; shrink takes −1 only from children with
  `layout_resize_check > 0`, looping until the delta is exhausted or nothing changed.
- **`layout_resize_set_size`** (`layout.c:639-651`): absolute wrapper over adjust.
- **`layout_fix_offsets`** (`layout.c:323-371`): recompute all `xoff/yoff` from sizes,
  walking each node and accumulating `+ child_size + 1` (border) per sibling.
- **`layout_fix_panes(w, skip)`** (`layout.c:432-499`): copy cell geometry to panes, apply
  the pane-border-status edge row (top: `yoff++; sy--`) and *[scrollbar]* reservation, and
  call `window_pane_resize`.

### 3.2 Whole-window resize (`layout_resize`, `layout.c:777-828`)

Called from `resize_window` (`resize.c:26-66`). For each axis:

```c
xchange = sx - lc->g.sx;
xlimit = layout_resize_check(w, lc, LAYOUT_LEFTRIGHT);
if (xchange < 0 && xchange < -xlimit)
        xchange = -xlimit;                 /* don't shrink below minimum */
if (xlimit == 0) {                         /* already at minimum */
        if (sx <= lc->g.sx) xchange = 0;   /* keep layout larger than window */
        else xchange = sx - lc->g.sx;
}
if (xchange != 0)
        layout_resize_adjust(w, lc, LAYOUT_LEFTRIGHT, xchange);
```

Key consequence: the layout may end up **larger than the window** (window shows a clipped
view); `resize_window` then clamps the window size up to the layout size
(`resize.c:49-52`). A zoomed window is unzoomed before resizing and re-zoomed after
(`resize.c:41-43,57-59`).

### 3.3 Single-pane resize (`layout_resize_pane`, `layout.c:963-987`)

Find the nearest ancestor cell whose **parent** is of the requested type (walking up); if
that cell is the **last** sibling, step back one sibling (so resizing the rightmost pane's
right edge actually moves its left border):

```c
lcparent = lc->parent;
while (lcparent != NULL && lcparent->type != type) {
        lc = lcparent;
        lcparent = lc->parent;
}
if (lcparent == NULL)
        return;                            /* nothing to resize in that direction */
if (lc == TAILQ_LAST(&lcparent->cells, layout_cells))
        lc = TAILQ_PREV(lc, layout_cells, entry);
layout_resize_layout(wp->window, lc, type, change, opposite);
```

`layout_resize_layout` (`layout.c:934-961`) loops grow/shrink one step at a time until the
change is satisfied or impossible, then fixes offsets/panes and notifies
`window-layout-changed`.

- **Grow** (`layout_resize_pane_grow`, `layout.c:989-1028`): add to `lc`; take the space
  from the **first following sibling** with available space; if none and `opposite` is set
  (true for the `resize-pane` command), take from the first **preceding** sibling.
- **Shrink** (`layout_resize_pane_shrink`, `layout.c:1030-1060`): walk from `lc` backwards
  to find a sibling (including itself) with available space to remove; give the space to the
  sibling immediately after `lc`. If `lc` is last (can't happen here due to the step-back
  above) return 0.

**Absolute resize** (`layout_resize_pane_to`, `layout.c:830-861`, used by
`resize-pane -x/-y`): same ancestor walk; if the cell is the *last* sibling the delta is
inverted (`change = size - new_size`) because the resize moves its top/left border.

### 3.4 Spread (`select-layout -E`)

`layout_spread_cell` (`layout.c:1509-1573`): within one parent node, give each child
`each = (size - (n-1)) / n` cells (`size` excludes one border-status row if applicable),
distributing the `remainder` one extra cell to the first `remainder` children.
`layout_spread_out` (`layout.c:1575-1593`) walks up from the active pane's parent until a
level actually changes.

### 3.5 The preset layouts (`layout-set.c`)

Table (`layout-set.c:39-50`) — order matters for `next-layout`:

```
0 even-horizontal   1 even-vertical    2 main-horizontal   3 main-horizontal-mirrored
4 main-vertical     5 main-vertical-mirrored               6 tiled
```

(Classic tmux ≤3.1 had only 5: even-h, even-v, main-h, main-v, tiled — the mirrored
variants were added in 3.2/3.3; winmux's five-preset cycle matches the classic list.)

`layout_set_lookup` (`layout-set.c:52-71`): exact match first, then unique-prefix match
(ambiguous prefix ⇒ -1). `layout_set_select/next/previous` (`layout-set.c:73-124`) apply
by index and store `w->lastlayout`. `next-layout` cycles `lastlayout+1` mod N starting from
0 if never set; `previous-layout` cycles backwards starting from N-1.

All preset builders share this pattern: skip if `n = window_count_panes(w) <= 1`; free only
the tree's *node* cells (`layout_free(w, 1)` keeps leaves attached to panes); build a fresh
root; re-link every pane's existing leaf cell in **pane-list order**; fix offsets/panes;
`window_resize` to the (possibly grown) layout size; notify + redraw.

- **even-horizontal / even-vertical** (`layout_set_even`, `layout-set.c:138-188`): one
  LEFTRIGHT (resp. TOPBOTTOM) root; the needed size is
  `n * (PANE_MINIMUM + 1) - 1` and the root takes `max(that, w->sx)`; every child is
  initialised to the full window size and then `layout_spread_cell` evens them out.
- **main-horizontal** (`layout_set_main_h`, `layout-set.c:202-297`): TOPBOTTOM root =
  main pane on top (full width), LEFTRIGHT row of the others below.
  Height math (`layout-set.c:218-246`):
  - available `sy = w->sy - 1` (one border line);
  - `mainh` = `main-pane-height` option (string; supports `%` of `sy`); parse failure ⇒ 24;
  - if `mainh + PANE_MINIMUM >= sy`: `mainh = sy - PANE_MINIMUM` (or `PANE_MINIMUM` if the
    window is tiny), `otherh = PANE_MINIMUM`;
  - else `otherh` = `other-pane-height` option (default "0" ⇒ `otherh = sy - mainh`); if
    set and it fits, **the main pane grows to take the slack**: `mainh = sy - otherh`.
  - Required width `sx = max(n*(PANE_MINIMUM+1)-1, w->sx)` for the bottom row.
  - The main pane is the **first pane in the pane list** (`layout_set_first_tiled`,
    `layout-set.c:126-136`), not the active pane.
  - If only one other pane, it is placed directly under the root (no LEFTRIGHT node).
  - Others get `PANE_MINIMUM` width then `layout_spread_cell` evens the row.
- **main-horizontal-mirrored** (`layout-set.c:299-394`): identical, but the others-row is
  inserted at the **head** — i.e. the row of others is on *top*, main pane at the bottom.
- **main-vertical** (`layout-set.c:396-491`): transpose of main-horizontal: LEFTRIGHT root,
  main pane (width `main-pane-width`, default 80, fallback rules identical with
  `other-pane-width`) on the left, TOPBOTTOM stack of others on the right.
- **main-vertical-mirrored** (`layout-set.c:493-589`): others stack on the left.
- **tiled** (`layout_set_tiled`, `layout-set.c:591-711`): grid. Rows/columns
  (`layout-set.c:611-617`, with `max_columns` = `tiled-layout-max-columns`, 0 = unlimited):

  ```c
  rows = columns = 1;
  while (rows * columns < n) {
          rows++;
          if (rows * columns < n && (max_columns == 0 || columns < max_columns))
                  columns++;
  }
  ```

  (n=2 → 2x1... actually rows=2,cols=1 → 2 rows; n=3 → rows=2,cols=2; n=5 → rows=3,cols=2;
  i.e. rows ≥ columns, growing rows first.)
  Cell size: `width = (w->sx - (columns-1)) / columns`, `height = (w->sy - (rows-1)) / rows`
  each floored at `PANE_MINIMUM`. Root is TOPBOTTOM of row cells; each row is a LEFTRIGHT of
  up to `columns` panes, except a row holding a single remaining pane is inserted directly.
  Leftover horizontal space goes to the **last cell of each row**
  (`layout-set.c:686-693`), leftover vertical space to the **last row**
  (`layout-set.c:696-701`). Panes fill the grid row-major in pane-list order.

Preset layouts ignore `main-pane-*`/`other-pane-*` percentages relative to anything but the
current window size at application time; layouts are one-shot (subsequent window resizes go
through the generic §3.2 scaling, not the preset).

### 3.6 `select-layout` (`cmd-select-layout.c:71-149`)

- `server_unzoom_window(w)` first — **selecting any layout unzooms**.
- Saves `w->old_layout = layout_dump(current tree)` before changing; `select-layout -o`
  applies that saved string — i.e. **`-o` undoes the last layout change** (one level).
  On error the old value is restored (`cmd-select-layout.c:145-148`).
- No argument: re-applies `w->lastlayout` (the most recent preset) if any.
- Named argument: preset name/prefix (`layout_set_lookup`), else treated as a **custom
  layout string** (`layout_parse`).
- `-n`/`-p` behave as `next-layout`/`previous-layout` (also standalone commands);
  `-E` = spread (§3.4).

### 3.7 Custom layout strings (`layout-custom.c`)

Format = what `#{window_layout}` prints: `csum,cell` where `csum` is 4 lowercase hex digits.

- **Cell syntax**: `SXxSY,XOFF,YOFF` then optionally `,PANEID` (leaf; the pane id digits
  are only consumed if not followed by `x`, `layout-custom.c:351-358`), or children:
  `{...}` = LEFTRIGHT, `[...]` = TOPBOTTOM, children comma-separated
  (`layout_construct`, `layout-custom.c:375-428`).
- **Checksum** (`layout_checksum`, `layout-custom.c:46-57`) over everything after the comma:

  ```c
  csum = 0;
  for (; *layout != '\0'; layout++) {
          csum = (csum >> 1) + ((csum & 1) << 15);   /* 16-bit rotate right */
          csum += *layout;
  }
  ```

- **Parsing/applying** (`layout_parse`, `layout-custom.c:173-296`): verify
  `sscanf("%hx,")` consumed exactly 5 chars and checksum matches, else "invalid layout".
  Pane count vs leaf count: more panes than cells ⇒ error `"have %u panes but need %u"`;
  fewer ⇒ repeatedly destroy the **bottom-right** leaf (`layout_find_bottomright`, last
  child recursively, `layout-custom.c:35-43`) until counts match. Old-version fixup: if the
  root size disagrees with the sum of its children, correct the root. Then `layout_check`
  verifies internal consistency (children of LEFTRIGHT: same `sy`, widths sum+borders equal
  parent; mirrored for TOPBOTTOM; `layout-custom.c:136-170`). The window is resized to the
  layout size, panes are assigned to leaves **in pane-list order** (`layout_assign`,
  depth-first, `layout-custom.c:298-319`) — pane ids in the string are *ignored* for
  assignment.

### 3.8 Closing a pane (`layout_close_pane` / `layout_destroy_cell`, `layout.c:697-755`)

The freed space goes to the **nearest sibling**: prefer the *next* sibling unless the cell
is the last child, then the previous (`layout_cell_get_neighbour`, `layout.c:677-694`);
that neighbour is grown by `size + 1` (the border line). If the parent is left with one
child, the parent is dissolved and replaced by that child (`layout.c:739-754`).

---

## 4. resize-pane (`cmd-resize-pane.c`)

Args (`cmd-resize-pane.c:44`): `"D::L::MR::Tt:U::x:y:Z"` plus one optional positional.

- **`-U/-D/-L/-R [adjust]`** (`cmd-resize-pane.c:142-180`): adjust defaults to 1 (or the
  positional argument). `L`/`R` → LAYOUT_LEFTRIGHT, `U`/`D` → LAYOUT_TOPBOTTOM; `L`/`U`
  negate the delta. Calls `layout_resize_pane(wp, type, adjust, 1)` (opposite=1: may steal
  space from siblings on the other side when the preferred side is exhausted). Processing
  order when multiple direction flags are given: U, D, L, R (the `flags[]` array).
- **`-x width` / `-y height`** (`cmd-resize-pane.c:96-140`): absolute; supports `N%` of the
  window dimension (`args_percentage`), bounded `0..PANE_MAXIMUM`. `-y` is bumped by one if
  a pane-border-status row overlaps the pane's edge (top status & `yoff==1`, or bottom
  status & pane touching the bottom, `cmd-resize-pane.c:120-130`). Uses
  `layout_resize_pane_to` (§3.3).
- **`-Z`** (`cmd-resize-pane.c:86-93`): toggle zoom — if `WINDOW_ZOOMED` unzoom, else
  `window_zoom(wp)`. All other resize forms first call `server_unzoom_window(w)`
  (`cmd-resize-pane.c:94`) — **resizing implicitly unzooms**.
- **`-T`** (`cmd-resize-pane.c:71-81`): unrelated trim — pulls lines below the cursor back
  into history ("trim below cursor").
- **`-M`** (`cmd-resize-pane.c:191-220` + `355-418`): mouse resize, bound by default to
  `MouseDrag1Border`. `cmd_resize_pane_mouse_resize_tiled` finds the border cell(s) under
  the drag start point via `layout_search_by_border` (`layout.c:157-196`, which locates the
  cell whose gap contains the coordinate) probing 5 offsets `{0,0},{0,1},{1,0},{0,-1},{-1,0}`
  to catch both sides of a junction; for each found cell whose parent runs in the drag
  direction, applies `layout_resize_layout(w, cell, type, delta, 0)` where delta is the
  mouse movement since the last event. It stays installed as `mouse_drag_update` so the
  resize is live during the drag.
- **Repeat**: the default bindings use `bind -r` (C-Up etc. adjust 1, M-Up etc. adjust 5);
  repeat window is the `repeat-time` session option, default **500 ms**
  (`options-table.c:986-994`).

### 4.1 Zoom (`window_zoom`/`window_unzoom`, `window.c:772-826`) — exact behavior

```c
window_zoom(wp):
        if (w->flags & WINDOW_ZOOMED) return -1;
        if (window_count_panes(w, 1) == 1) return -1;   /* can't zoom a lone pane */
        if (w->active != wp) window_set_active_pane(w, wp, 1);
        wp->flags |= PANE_ZOOMED;
        for each pane: saved_layout_cell = layout_cell; layout_cell = NULL;
        w->saved_layout_root = w->layout_root;
        layout_init(w, wp);                              /* fresh 1-pane layout */
        w->flags |= WINDOW_ZOOMED;
```

Unzoom restores `saved_layout_root` and every pane's `saved_layout_cell`, then
`layout_fix_panes`. Both notify `window-layout-changed`.

**What implicitly unzooms:** any `window_set_active_pane` to a different pane
(`window.c:585-586`); `select-layout`/`next-layout`/`previous-layout`
(`cmd-select-layout.c:83`); `resize-pane` non-`-Z` (`cmd-resize-pane.c:94`);
`break-pane` (`cmd-break-pane.c:123`); `join-pane`/`move-pane` (both src and dst windows,
`cmd-join-pane.c:427,452`); `kill-pane -a` (`cmd-kill-pane.c:81`); `kill-pane` via
`server_kill_pane` (`server-fn.c:191`); a window resize (temporarily, §3.2); display-panes
digit selection (`cmd-display-panes.c:330`). Splits/swaps/rotates *keep or restore* zoom
per the push/pop mechanism, and their `-Z` flag re-zooms afterwards; without `-Z` those
commands leave the window unzoomed (push_zoom's flag is `args_has('Z')`).

The status-line window flag for a zoomed window is `Z` (elsewhere; `window_printable_flags`).

---

## 5. Pane lifecycle & rearrangement

### 5.1 swap-pane (`cmd-swap-pane.c`)

Args: `"dDs:t:UZ"`. Target `-t` is the *destination*; source `-s` defaults to the **marked
pane** (`CMD_FIND_DEFAULT_MARKED`, `cmd-swap-pane.c:38`), else the current pane.

- **`-D`** (`cmd-swap-pane.c:80-91`): source = next pane after target in the pane list,
  wrapping to the list head. **`-U`**: previous, wrapping to the tail. (Skipping
  *[floating]* panes.)
- Both windows get `window_push_zoom(w, 0, args_has('Z'))` (`cmd-swap-pane.c:77,106`) and
  `window_pop_zoom` at the end — so swap works while zoomed; with `-Z` zoom is retained.
- The panes swap **positions in the pane list** (so their indexes swap), **layout cells**,
  and geometry (`cmd-swap-pane.c:115-154`); when the windows differ they also swap window
  membership and option parents.
- **Focus** (`cmd-swap-pane.c:156-169`): without `-d`, the destination-window's active pane
  becomes `src_wp` (the pane that moved into it) — i.e. **focus follows the swapped-in
  content**: after `swap-pane -U` the active pane is still the same *content*, now in the
  new position (`window_set_active_pane(src_w, dst_wp)` for same-window swaps... precisely:
  same-window case activates `dst_wp`? No — for same-window `tmp_wp = dst_wp;
  window_set_active_pane(src_w, tmp_wp, 1)` activates the *target* pane, which for -U/-D is
  the pane the user was on before... note `dst_wp` is the `-t` target = current pane; so the
  current pane *stays* active in its new location). With `-d`, activity is only adjusted if
  an active pane moved away (each window re-activates the pane now in the formerly-active
  slot).
- Swapping a pane with itself is a silent no-op (`cmd-swap-pane.c:109-110`).

### 5.2 rotate-window (`cmd-rotate-window.c`)

Args: `"Dt:UZ"`. `window_push_zoom(w, 0, -Z)` / `window_pop_zoom` bracket it.

- **`-U`** (default, `cmd-rotate-window.c:82-107`): the *first* pane moves to the tail of
  the pane list, and every pane takes the layout cell + geometry of its predecessor —
  i.e. pane contents rotate "up/forward": each pane's content moves to the previous pane's
  position.
- **`-D`** (`cmd-rotate-window.c:57-81`): the *last* pane moves to the head; contents rotate
  the other way.
- **Focus** (`cmd-rotate-window.c:80-81,105-106`): after `-U` the new active pane is
  `TAILQ_NEXT(old_active)` (wrapping to first); after `-D` it is `TAILQ_PREV(old_active)`
  (wrapping to last) — the effect is that **focus follows the same content** to its new
  location.

### 5.3 break-pane (`cmd-break-pane.c`)

Args: `"abdPF:n:s:t:Wx:X:y:Y:"`. Source `-s` pane; target `-t` a *window index*
(`CMD_FIND_WINDOW_INDEX`).

- `server_unzoom_window(w)` first (`cmd-break-pane.c:123`).
- If the pane is the **only** pane in its window (`window_count_panes == 1`,
  `cmd-break-pane.c:125-141`): the window is *linked* to the destination index and unlinked
  from its old position (a pure move; `-n` renames and disables `automatic-rename`).
- Otherwise (`cmd-break-pane.c:147-180`): remove the pane from its window
  (`window_lost_pane` + `layout_close_pane` — old window refocuses per §5.6 and the space
  is redistributed per §3.8), create a brand-new window of the same size with this pane as
  its only pane (`layout_init`), name it (`-n` or `default_window_name`), and attach at the
  target index (`-a`/`-b`: after/before the target, via `winlink_shuffle_up`; default: the
  first free index from `base-index`). Duplicate destination index ⇒ error `"index in use"`.
- **Focus**: without `-d` the new window becomes the session's current window
  (`session_select`, `cmd-break-pane.c:177-180`); with `-d` it stays in the background.
- `-P`/`-F` print the new location; default template
  `#{session_name}:#{window_index}.#{pane_index}` (`cmd-break-pane.c:29`).
- *[floating]* `-W` floats the pane in place instead (`cmd_break_pane_float`).

### 5.4 join-pane / move-pane (`cmd-join-pane.c`)

`join-pane` args: `"bdfhvp:l:s:t:"`. Moves the **source pane** (default: marked pane) out
of its window and splits it into the **target pane's** cell, using exactly the split
machinery: `layout_get_tiled_cell(item, args, dst_w, dst_wp, flags, &cause)`
(`cmd-join-pane.c:461`) — so `-h/-v/-b/-f/-l/-p` mean the same as for `split-window`
(**note:** `-v`/default is TOPBOTTOM; historic `join-pane` before tmux 3.x defaulted the
same way). Differences from split:

- Both windows are unzoomed (`cmd-join-pane.c:427,452`).
- Joining a pane to itself is an error `"source and target panes must be different"`
  (`cmd-join-pane.c:454-459`).
- The source pane is removed from its window (`layout_close_pane`, `window_lost_pane`,
  list removal, `cmd-join-pane.c:468-473`), re-parented, inserted after (before with `-b`)
  the target pane in the destination pane list, and assigned to the new cell
  (`layout_assign_pane`).
- **Focus**: without `-d`, the moved pane becomes the destination window's active pane and
  the destination window becomes current in its session (`cmd-join-pane.c:493-497`).
- If the source window is left with zero panes it is killed (`cmd-join-pane.c:501-502`).

`move-pane` is the same command entry; in classic tmux it is an exact alias of `join-pane`.
*[floating]* In this master `move-pane` additionally handles floating panes
(`-P position`, `-z z-index`, `-X/-Y/-U/-D/-L/-R` offsets, `-M` mouse drag), and
`join-pane` on a floating pane re-tiles it.

### 5.5 kill-pane (`cmd-kill-pane.c`, `server-fn.c:183-197`)

Args: `"af:t:"`.

- Plain: `server_kill_pane(wp)` — if it is the window's only pane, kill the whole window;
  otherwise unzoom, remove the pane, `layout_close_pane` (space redistribution §3.8),
  redraw.
- `-a`: kill **all panes except** the target (optionally filtered by `-f '#{...}'`
  format that must evaluate true); `-f` without `-a` is an error.

### 5.6 Focus after a pane dies (`window_lost_pane`, `window.c:885-909`)

```c
if (wp == w->active) {
        w->active = TAILQ_FIRST(&w->last_panes);      /* most recently active other pane */
        if (w->active == NULL) {
                w->active = TAILQ_PREV(wp, ...);      /* else previous by index */
                if (w->active == NULL)
                        w->active = TAILQ_NEXT(wp, ...);   /* else next by index */
        }
}
```

Priority: **last-panes stack → previous pane in list → next pane in list.** The chosen pane
is removed from the stack, flagged `PANE_CHANGED`, and `window-pane-changed` fires. A dead
marked pane also clears the mark (`window.c:890-891`).

### 5.7 respawn-pane (`cmd-respawn-pane.c`)

`respawn-pane [-k] [-c dir] [-e VAR=val] [-t pane] [cmd...]`: re-runs a command in an
existing pane via `spawn_pane` with `SPAWN_RESPAWN` (+`SPAWN_KILL` for `-k`). Without `-k`
it fails if the pane's process is still alive (spawn.c checks). Layout/focus are untouched;
the pane is just reset and redrawn (`cmd-respawn-pane.c:92-94`).

---

## 6. display-panes (`cmd-display-panes.c`)

Command: `display-panes [-bN] [-d duration] [-t target-client] [template]`
(`cmd-display-panes.c:39-40`).

- **Overlay**: installs a client overlay (`server_client_set_overlay`,
  `server-client.c:91-125`) with a timer of `-d duration` ms, defaulting to the
  `display-panes-time` option (default **1000** ms). While active the client's tty is
  frozen for normal drawing and the cursor hidden.
- **`-b`**: don't block — without `-b` the command returns `CMD_RETURN_WAIT` and the
  invoking queue item is only continued when the overlay closes
  (`cmd_display_panes_free`, `cmd-display-panes.c:293-302`).
- **`-N`**: install **no key handler** — keys behave normally, digits do not jump
  (`cmd-display-panes.c:380-384`).
- **Rendering** (`cmd_display_panes_draw_pane`, `cmd-display-panes.c:124-263`), per visible
  pane:
  - The pane's index (from `window_pane_index`, i.e. `pane-base-index`-relative) is drawn
    as **big 5x5 digits** using the shared `window_clock_table` glyphs, centred:
    `px = sx/2 - len*3; py = sy/2 - 2`; each digit cell is a space drawn with bg = the
    indicator colour, digits advance `px += 6`.
  - Colours: active pane uses `display-panes-active-colour`, others
    `display-panes-colour` (`cmd-display-panes.c:195-198`); (this master routes them
    through theme styles, defaults `themered`/`themeblue`; classic tmux: `red`/`blue`).
  - **Small panes** (`sx < len*6 || sy < 5`, `cmd-display-panes.c:211-228`): fall back to
    drawing the plain number (in the indicator fg colour) at the pane centre.
  - Below the digits (if `sy > 6`) a line is drawn from `display-panes-format`
    (default `#[align=right]#{pane_width}x#{pane_height}`), format-expanded per pane
    (`cmd_display_panes_draw_format`, `cmd-display-panes.c:88-122`).
  - Panes 10-34 additionally show a letter `a`+`(index-10)` next to the number
    (`cmd-display-panes.c:206-209`) — because keys can only select ≤ 36 panes.
- **Key handling** (`cmd_display_panes_key`, `cmd-display-panes.c:304-348`): `0`-`9` map to
  indexes 0-9; unmodified `a`-`z` map to 10-35; **any other key returns -1**, which per the
  overlay dispatch (`server-client.c:1630-1640`) means: *the overlay is closed and the key
  is then processed normally* (it is not swallowed). A digit for a nonexistent pane index
  returns 1 (overlay closes, key consumed). A valid selection: `window_unzoom(w, 1)` then
  build the command from the **template** with every `%%` replaced by `%<pane-id>`
  (prepared as `args_make_commands_prepare(self, item, 0, "select-pane -t \"%%%\"", ...)`,
  `cmd-display-panes.c:377-378`) — i.e. the default template is `select-pane -t '%%'`, so a
  digit jumps to that pane; a custom template (e.g. `display-panes 'kill-pane -t %%'`)
  runs that instead. The command is queued and the overlay closes (return 1).
- The overlay also closes by itself when the timer fires; the timer is the *only* thing
  that limits duration (keys pressed do not restart it).

---

## 7. Pane borders

### 7.1 Which style colours a border cell

Every border cell knows which pane(s) it borders on each side (top/bottom/left/right
owners; `redraw_mark_border_cell`, `screen-redraw.c:515-573`). Style resolution per span
(`redraw_get_pane_for_border_style`, `screen-redraw.c:1108-1131`):

```c
if (span->data.b.style_wp != NULL)  return style_wp;     /* two-pane special case */
if (active adjacent)                return active;
return first of top_wp / bottom_wp / left_wp / right_wp;
```

Then `window_pane_get_border_style` (`window-border.c:86-113`): if the chosen pane **is the
client's active pane** use `pane-active-border-style` (default in classic tmux: `fg=green`;
this master: a conditional theme style that turns magenta when marked, red when
`synchronize-panes`, yellow in a mode, else green), otherwise `pane-border-style` (classic
default: `default`; this master `fg=themelightgrey`). Both are window-or-pane scoped
options, format-expanded (so `#{...}` conditionals work), cached per pane until styles
change. **The active-pane test is `wp == server_client_get_pane(c)`** — per client, so two
clients on different active panes each see their own green border.

A border adjacent to the **marked pane** gets `attr ^= GRID_ATTR_REVERSE`
(`screen-redraw.c:1191-1194`).

**Two-pane windows** (`redraw_check_two_pane_colours` + `redraw_mark_two_pane_colours`,
`screen-redraw.c:404-420,788-829`): when a window has exactly two (tiled) panes, the single
shared border is split cosmetically — for a LEFTRIGHT split the top half
(`wy <= w->sy / 2`) is coloured as the left pane's border and the bottom half as the right
pane's; for TOPBOTTOM the left half by the top pane, right half by the bottom pane. Effect:
with an active green style, only *half* the divider is green in a 2-pane window, indicating
which side is active.

### 7.2 pane-border-lines / character sets

`pane-border-lines` choices (`options-table.c:78-80`): `single` (default), `double`,
`heavy`, `simple`, `number` (plus `spaces`, `none` in this master — new). Border cell types
(`tmux.h:820-833`): CELL_UD `│`, CELL_LR `─`, corners RD `┌` LD `┐` RU `└` LU `┘`, tees
LRD `┬` LRU `┴` URD `├` ULD `┤`, cross LRUD `┼`, CELL_NONE `·`.

- **single**: ACS charset string `CELL_BORDERS = " xqlkmjwvtun~"` (`tmux.h:836`) — drawn
  with `GRID_ATTR_CHARSET` (DEC line drawing; `x`=│, `q`=─, `l`=┌, `k`=┐, `m`=└, `j`=┘,
  `w`=┬, `v`=┴, `t`=├, `u`=┤, `n`=┼, `~`=·).
- **double**: U+2550-family (║ ═ ╔ ╗ ╚ ╝ ╦ ╩ ╠ ╣ ╬, none=·) — `tty-acs.c:114-128`.
- **heavy**: U+2501-family (┃ ━ ┏ ┓ ┗ ┛ ┳ ┻ ┣ ┫ ╋, none=·) — `tty-acs.c:131-145`.
  (Terminals without UTF-8 fall back to single — handled in tty layer.)
- **simple**: ASCII `SIMPLE_BORDERS = " |-+++++++++."` (`tmux.h:837`).
- **number**: every border cell shows the adjacent pane's index digit
  (`'0' + idx % 10`; `*` if unresolvable) — `window-border.c:39-50`.
- The cell type is computed from which directions the border continues
  (`redraw_get_cell_type`, `screen-redraw.c:370-402`), so junctions between three/four
  panes get the right tee/cross glyph.

### 7.3 pane-border-status / pane-border-format

`pane-border-status` ∈ `off` (default) / `top` / `bottom` (`options-table.c:72-74`,
+floating variants new). When on:

- The layout reserves **one extra row** for panes at the affected window edge:
  `layout_add_horizontal_border` (`layout.c:420-429`) returns true for cells that are
  topmost (status top) or bottommost (status bottom) — determined by walking the tree
  (`layout_cell_is_top/bottom`, `layout.c:374-414`). `layout_fix_panes` then shifts/shrinks
  the pane by one row (`layout.c:457-463`); minimum sizes and spread math gain +1 in the
  same places (`layout.c:544-547`, `1280`, `1527-1529,1558-1561`).
- Non-edge panes show their status line *on* their existing top/bottom border row.
- Content (`window_make_pane_status`, `window-border.c:116-171`): a 1-row screen of width =
  the pane's border width, pre-filled with border line characters, then
  `pane-border-format` (window/pane option) is format-expanded (with time formats) and
  drawn over it in the pane's border style. Default format (classic tmux):
  `#{?pane_active,#[reverse],}#{pane_index}#[default] "#{pane_title}"` (this master appends
  mouse-clickable `[t/f]`/`[z/u]`/`[x]` range buttons when `mouse` is on,
  `options-table.c:1534-1551`). The status text starts 2 columns in from the pane's left
  border (`sx = wp->xoff + 2`, `screen-redraw.c:595`).
- With status top, `select-pane -U/-D` wrap edges shift by one row (§1.1) and
  `window_get_active_at` treats the row above a pane as belonging to it
  (`window.c:675-692`).

### 7.4 pane-border-indicators

`pane-border-indicators` ∈ `off` / `colour` (default) / `arrows` / `both`
(`options-table.c:75-77`, default `PANE_BORDER_COLOUR`).

- `colour`/`both`: the active-style colouring of §7.1 (including the two-pane half-border
  trick, which is *only* applied for colour/both — `screen-redraw.c:797-798`).
- `arrows`/`both` (`redraw_mark_border_arrows`, `screen-redraw.c:615-658` + draw at
  `screen-redraw.c:1133-1159`): four arrow glyphs are drawn on the **active pane's** border
  at fixed spots — on the top and bottom borders at `x = wp->xoff + 1`, and on the left and
  right borders at `y = wp->yoff + 1`. Glyphs are ACS characters chosen by which side of
  the border the active pane is on:

  ```c
  if (left_wp == active)  ch = ',';   /* arrow pointing left  (ACS_LARROW)  */
  else if (right_wp == active) ch = '+';  /* pointing right (ACS_RARROW) */
  else if (top_wp == active)   ch = '-';  /* pointing up    (ACS_UARROW) */
  else if (bottom_wp == active) ch = '.'; /* pointing down  (ACS_DARROW) */
  ```

  i.e. the arrows sit just inside each corner of the active pane and point *at* the active
  pane. When `off`, neither colour nor arrows indicate activity (border style still applies
  as the inactive style everywhere).
- With `pane-border-lines none` on the *window* fill character or `fill-character` set,
  CELL_NONE cells outside any pane use that fill (`window-border.c:33-36`).

---

## 8. Sizing options: window-size, aggressive-resize (`resize.c`)

`window-size` ∈ `largest` / `smallest` / `manual` / `latest` (default **latest**;
`options-table.c:1778-1788`). A window's size is recomputed by `recalculate_size`
(`resize.c:352-417`) whenever clients attach/detach/resize or sessions change
(`recalculate_sizes`, `resize.c:419-460`):

- Candidate clients are all attached, non-control clients (control clients count only if
  they pushed an explicit size; clients flagged read-only/ignore-size are skipped —
  `ignore_client_size`, `resize.c:68-96`).
- Per `window-size` type (`clients_calculate_size`, `resize.c:113-264`):
  - **largest**: max client width × max client height (dimensions considered
    independently) over clients whose session contains the window.
  - **smallest**: min × min.
  - **latest**: if more than one candidate client shows the window, only `w->latest` (the
    client that most recently had it as current window and provided input) contributes;
    with a single client it degrades to smallest (`resize.c:162-170`).
  - **manual**: `w->manual_sx/sy` (set by `resize-window -x/-y`; `default-size` seeds new
    windows, default `80x24`).
- A client's contribution is `tty.sx × (tty.sy - status_line_size)` (`resize.c:186-188`).
- **aggressive-resize** (window option, default off, `options-table.c:1274-1282`): passed
  as `current` into the skip callback (`recalculate_size_skip_client`, `resize.c:336-350`):
  when on, only clients for which this window **is the current window** are considered
  (instead of all clients whose session merely contains it). So an `on` window snaps to the
  size of whoever is actually looking at it; the man-page wording "when window-size is
  smallest…largest" reflects that it matters for those modes (latest already tracks one
  client).
- If no suitable client exists, `default-size` is used for new windows
  (`default_window_size`, `resize.c:277-334`); existing windows just keep their size
  (`changed == 0` path).
- The resize is normally **deferred**: `w->new_sx/new_sy` + `WINDOW_RESIZE` flag, applied
  later by the server loop (after a resize-throttle interval), except manual/`now` which
  resize immediately (`resize.c:400-416`). The actual `resize_window` clamps to
  `WINDOW_MINIMUM..WINDOW_MAXIMUM`, unzooms/rezooms, and runs §3.2.
- Clients viewing a window **smaller than the window size** see a clipped view that
  follows the cursor (`tty_update_window_offset`); clients larger see filler around it.
  That machinery is out of scope here, but note `cmd_select_pane_redraw`
  (`cmd-select-pane.c:58-81`) forces a full client redraw when the window is bigger than
  the client because the visible offset may move on pane switch.

---

## 9. Option defaults table (all options touched by this domain)

| Option | Scope | Default | Effect |
|---|---|---|---|
| `pane-base-index` | window | `0` | First pane index (`window_pane_at_index`/`window_pane_index`). |
| `main-pane-width` | window | `"80"` | main-vertical main pane width; supports `N%`. |
| `main-pane-height` | window | `"24"` | main-horizontal main pane height; supports `N%`. |
| `other-pane-width` | window | `"0"` | main-vertical others' width; `0` = whatever remains; if set, main pane takes the slack. |
| `other-pane-height` | window | `"0"` | main-horizontal others' height; same rules. |
| `tiled-layout-max-columns` | window | `0` | Max columns in tiled layout; 0 = unlimited. *(newer option)* |
| `pane-border-style` | window+pane | classic: `default`; this master: `fg=themelightgrey` | Style of non-active pane borders (also the base for border-status lines). |
| `pane-active-border-style` | window+pane | classic: `fg=green`; this master: themed conditional (marked→magenta, sync→red, mode→yellow, else green) | Style of the client-active pane's border. |
| `pane-border-status` | window(+pane) | `off` | `off`/`top`/`bottom`: per-pane status line on the border; reserves one row at the window edge. |
| `pane-border-format` | window+pane | `#{?pane_active,#[reverse],}#{pane_index}#[default] "#{pane_title}"` (+ mouse buttons in this master) | Content of the border status line. |
| `pane-border-lines` | window(+pane) | `single` | Border glyph set: single/double/heavy/simple/number (+spaces/none new). |
| `pane-border-indicators` | window | `colour` | `off`/`colour`/`arrows`/`both` — how the active pane is marked on borders. |
| `display-panes-time` | session | `1000` ms | Overlay duration for display-panes. |
| `display-panes-colour` | session | classic `blue`; master `themeblue` | Digit colour for inactive panes. |
| `display-panes-active-colour` | session | classic `red`; master `themered` | Digit colour for the active pane. |
| `display-panes-format` | session | `#[align=right]#{pane_width}x#{pane_height}` | Text under the big digits. *(newer; classic tmux had no format line)* |
| `window-size` | window | `latest` | How the window size is derived from clients. |
| `aggressive-resize` | window | `off` | Consider only clients where the window is *current*. |
| `default-size` | session | `80x24` | Fallback/new-window size when no client informs sizing. |
| `repeat-time` | session | `500` ms | Repeat window for `bind -r` (resize/select-pane arrows). |
| `pane-scrollbars` | window | `off` | *(new)* off/modal/on/auto-hide. |

---

## 10. Default key bindings in this domain (`key-bindings.c:382-489`, prefix table unless noted)

| Key | Command |
|---|---|
| `%` | `split-window -h` |
| `"` | `split-window` (vertical) |
| `!` | `break-pane` |
| `;` | `last-pane` |
| `o` | `select-pane -t:.+` (next pane by index, wraps) |
| `Up`/`Down`/`Left`/`Right` | `select-pane -U/-D/-L/-R` — **bound with `-r` (repeatable)** |
| `C-Up/C-Down/C-Left/C-Right` | `resize-pane -U/-D/-L/-R` (`-r`, adjust 1) |
| `M-Up/M-Down/M-Left/M-Right` | `resize-pane -U/-D/-L/-R 5` (`-r`) |
| `z` | `resize-pane -Z` |
| `x` | `confirm-before -p"kill-pane #P? (y/n)" kill-pane` |
| `{` / `}` | `swap-pane -U` / `swap-pane -D` |
| `C-o` / `M-o` | `rotate-window` / `rotate-window -D` |
| `Space` | `next-layout` |
| `M-1`..`M-5` | `select-layout even-horizontal` / `even-vertical` / `main-horizontal` / `main-vertical` / `tiled` |
| `M-6`/`M-7` | `select-layout main-horizontal-mirrored` / `main-vertical-mirrored` *(3.2+)* |
| `E` | `select-layout -E` (spread) |
| `q` | `display-panes` |
| `m` / `M` | `select-pane -m` (toggle mark) / `select-pane -M` (clear mark) |
| `*` / `@` | *(new, floating)* `new-pane` / float-toggle |
| Mouse `MouseDown1Pane` (root) | `select-pane -t=; send -M` (click focuses pane, then forwards) |
| Mouse `MouseDown1Border` | `select-pane -M` *(note: on borders this is effectively a no-op mark-clear click-catcher)* |
| Mouse `MouseDrag1Border` | `resize-pane -M` (live border drag) |

(`join-pane`/`move-pane`/`respawn-pane`/`select-layout <string>` have no default prefix
bindings; they appear in the right-click pane menu — `key-bindings.c:43-100`.)

---

## 11. Windows/winmux applicability notes

- **Nothing in the navigation/layout core is Unix-specific.** The layout tree, the
  directional-find algorithms (incl. wrap + `active_point` tie-break), split math, preset
  layouts, custom layout strings/checksum, swap/rotate/break/join semantics, and the
  focus-after-close priority chain are pure data-structure logic and port 1:1.
- **`active_point` is the piece winmux most likely lacks**: winmux needs a per-pane
  monotonically increasing "last activated" counter to reproduce tmux's tie-break, plus a
  per-window `last_panes` MRU stack for `last-pane` and death-refocus (winmux's `;` pane
  behavior should use the stack, not a single "last pane" pointer, to match tmux exactly).
- **The user-reported winmux gap — no wrap on directional navigation — is settled by
  §1.1:** tmux always wraps (edge test rewritten to the far window edge when the pane is
  flush against the near edge); candidates must overlap perpendicular range; ties by
  recency; silent no-op only when the window has a single pane (the pane itself is skipped
  and its own far edge match makes it its own candidate — actually with one pane the scan
  skips `next == wp`, so no candidate ⇒ no-op).
- **spawn/process bits** (`spawn_pane` fork/exec, `respawn-pane`'s process check, `-c` cwd
  semantics via `/proc`, `-I` stdin panes) are Unix; winmux replaces them with ConPTY
  spawning — only the *layout/focus* side documented here needs matching.
- **display-panes** uses a client "overlay" that freezes normal drawing and swallows keys;
  winmux's overlay layer maps directly. The `window_clock_table` 5×5 digit font is a static
  table (in `window-clock.c`) and trivially portable. Key rule to copy: digits/letters
  select (consume, close), unknown keys close **and are then processed normally**.
- **Border drawing** with DEC ACS (`GRID_ATTR_CHARSET`) vs UTF-8: on Windows/ConPTY winmux
  already emits UTF-8 box drawing; map `single`→U+2500 set, `double`/`heavy` per the tables
  in §7.2, `simple`→ASCII. The arrow indicator glyphs `,`/`+`/`-`/`.` are ACS
  larrow/rarrow/uarrow/darrow — use ←/→/↑/↓ (U+2190..U+2193) or the VT ACS equivalents.
- **window-size/aggressive-resize** logic is transport-agnostic; winmux's client-pushed
  Resize frames provide exactly the `tty.sx/sy` inputs `clients_calculate_size` needs.
  Remember `latest` needs a per-window `latest` client pointer updated on input focus.
- **PANE_MINIMUM**: pick 2 (released tmux) or 1 (this master) — affects split refusal
  ("no space for a new pane"), resize floors, and preset-layout minimum math.
- Percent parsing (`-l 30%`, `main-pane-height 25%`) and the `%%` template substitution in
  display-panes are plain string handling; no platform issues.
