# tmux reference: choose-tree and the mode-tree framework

Source of record: tmux master `db115c6` (2026-07-07), full source at the research
clone. All `file:line` references below are into that tree. This document is the
authoritative behavioral spec for winmux's chooser overlays; implementers should
not need to reopen the tmux source.

Primary files:

- `mode-tree.c` — the generic tree-chooser framework (list + preview + prompts +
  search/filter/tag/sort + mouse). All choosers below are thin clients of it.
- `window-tree.c` — "tree mode" (`choose-tree`), the prefix-s / prefix-w chooser.
- `window-client.c` — "client mode" (`choose-client`).
- `window-buffer.c` — "buffer mode" (`choose-buffer`).
- `cmd-choose-tree.c` — the command entry points for all of the above (plus
  `customize-mode` and `switch-mode`, which are out of scope here).
- `sort.c` — the shared sort machinery (`sort_criteria`, comparators, item
  collectors).
- `screen-write.c:936-988` (`screen_write_preview`) and `screen-write.c:665-718`
  (`screen_write_fast_copy`) — how the live preview is painted.

Note on this master vs. released tmux 3.5a: this master has grown several
features beyond 3.5a — `themeXXX` colours, the `tree-mode-*` style options, the
`sort.c` refactor, the in-mode prompt (`mtd->prompt`), the F1/C-h help overlay,
the `i` info view, `S-Up`/`S-Down` window swap, `m`/`M` mark, `-N -N` big
preview, `-y` skip-confirm, and the ACS tree-branch line prefix. The core
behaviors the user cares about (stable index sort, preview box sizing, preview
content, live rebuild) are identical in 3.5a; the extras are flagged inline
where relevant.

---

## 1. How a chooser starts

### 1.1 Command entry points (`cmd-choose-tree.c`)

One exec function serves five commands (`cmd_choose_tree_exec`,
cmd-choose-tree.c:107-139). It validates `-O` (`sort_order_from_string`; an
unrecognized value with `-O` present is the error "invalid sort order"), picks
the window mode, and calls `window_pane_set_mode(wp, NULL, mode, target, args)`
on the target pane. Guards:

- `choose-buffer`: returns silently (no error) if `paste_is_empty()`
  (cmd-choose-tree.c:122-125).
- `choose-client`: returns silently if no attached clients
  (cmd-choose-tree.c:126-129).
- All chooser commands "work only if at least one client is attached" (man).

Args specs (cmd-choose-tree.c:36, 50, 64):

| command | args | usage |
|---|---|---|
| `choose-tree` | `F:f:GhK:kNO:rst:wyZ` + 0-1 template | `[-GhkNrswZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]` |
| `choose-client` | `F:f:hiK:kNO:rt:yZ` + 0-1 template | `[-hikNrZ] ...` |
| `choose-buffer` | `F:f:K:kNO:rt:yZ` + 0-1 template | `[-kNrZ] ...` |

Flag semantics (choose-tree; window-tree.c:1111-1134, mode-tree.c:577-592,
man tmux.1):

- `-s` — session view: `data->type = WINDOW_TREE_SESSION`. Sessions start
  **collapsed**; initial selection is the client's current session.
- `-w` — window view: windows start collapsed (sessions expanded); initial
  selection is the client's current window.
- neither — pane view: everything expanded; initial selection is the current
  pane (or the current window when its window has only one pane,
  window-tree.c:468-473).
- `-F format` — per-item format (default `WINDOW_TREE_DEFAULT_FORMAT`, §5.3).
- `-K key-format` — per-item shortcut-key format (default §5.4).
- `-f filter` — initial filter format (§9.2).
- `-O sort-order` — initial sort field (§4). `-r` — start reversed.
- `-G` — do NOT squash session groups (default squashes: for the current
  session's group only the current session is listed, for other groups only
  the group's first session; window-tree.c:449-455).
- `-N` — start with the preview off; `-N -N` (twice) — start with the **big**
  preview (list shrunk to ~1/4, mode-tree.c:579-584). [big preview is
  master-only; 3.5a has just on/off]
- `-Z` — zoom the mode pane while the mode is open; unzoomed on exit only if
  the mode did the zoom (mode-tree.c:613-624, 706-709).
- `-y` — skip confirmation prompts (sets `PROMPT_ACCEPT`; the kill prompts
  auto-answer 'y', mode-tree.c:1086-1098, 1169-1172). [master-only]
- `-h` — hide the pane the mode is running in from the tree and previews
  (floating-pane helper; window-tree.c:1132, 366-368, 673-674).
- `-k` — kill the mode's pane when the mode exits (`wme->kill`,
  window.c:1417, honored in `window_pane_reset_mode`, window.c:1466-1467).
- `template` — command run on Enter; default `switch-client -Zt '%%'`
  (window-tree.c:37). [3.5a default lacks `-Z`]

### 1.2 Default key bindings (key-bindings.c)

```
bind s { choose-tree -Zs }      # key-bindings.c:432
bind w { choose-tree -Zw }      # key-bindings.c:436
bind = { choose-buffer -Z }     # key-bindings.c:412
bind D { choose-client -Z }     # key-bindings.c:414
```

So **prefix-s = `choose-tree -Zs`** and **prefix-w = `choose-tree -Zw`** — both
zoom the pane and differ only in the initial collapse level and initial
selection.

### 1.3 Mode plumbing (window.c:1391-1467)

`window_pane_set_mode` prepends a `window_mode_entry` to `wp->modes` and swaps
`wp->screen` to the mode's own screen. If the pane is *already* in that mode
the call is a no-op returning 1 (re-invoking prefix-w while open does
nothing). `window_pane_reset_mode` pops the entry, restores `wp->base` (or the
next stacked mode), and kills the pane if `-k` was given. Both fire
`layout_fix_panes`, redraw notifications, and the `pane-mode-changed` hook.

`mode_tree_start` (mode-tree.c:559-611) allocates the framework state and a
dedicated `struct screen` sized to the pane (`screen_init(...,
screen_size_x(&wp->base), screen_size_y(&wp->base), 0)`) with the cursor mode
bit cleared. Keys reach the mode via `window_pane_key` → `wme->mode->key`
(window.c:1690-1711); mouse-move events are dropped before the mode sees them
(window.c:1704-1705).

---

## 2. Data model and build cycle (mode-tree.c)

### 2.1 Structures

- `mode_tree_data` (mode-tree.c:60-110): owns the item tree (`children`), the
  flattened visible-line array (`line_list`/`line_size`), viewport (`offset`,
  `current`, `width`, `height`), the mode screen, sort criteria, preview state
  (`preview`: OFF / NORMAL / BIG), `search`, `filter`, and the in-mode prompt.
- `mode_tree_item` (mode-tree.c:112-134): `parent`, caller's `itemdata`, a
  **`uint64_t tag` used as the item's stable identity across rebuilds**,
  `name`, `text` (the -F expansion), `expanded`, `tagged`, per-line shortcut
  `key`, `children`.
- `mode_tree_line` (mode-tree.c:136-141): one visible row — `item`, `depth`,
  `last` (last sibling), `flat` (its sibling list has no children anywhere).

Identity tags used by window-tree are the kernel object pointers:
`(uint64_t)s` for a session, `(uint64_t)wl` for a winlink, `(uint64_t)wp` for a
pane (window-tree.c:296, 354, 410). Buffer mode uses the buffer's creation
order counter; client mode uses the client pointer.

### 2.2 `mode_tree_build` (mode-tree.c:656-694) — the rebuild contract

Every rebuild (initial, after O/r sort change, expand/collapse, filter change,
kill, external update):

1. Remember the current item's `tag` (or `UINT64_MAX` if no lines yet).
2. Move the whole existing tree to `mtd->saved`, then call the mode's
   `buildcb(modedata, &sort_crit, &tag, filter)` to repopulate `children`.
3. **State preservation:** every `mode_tree_add` looks up the same `tag` in
   `saved` and copies `expanded` and `tagged` from the old item
   (mode-tree.c:755-763). So expansion state, tag marks, and the selection all
   survive rebuilds; brand-new items use the caller's `expanded` argument
   (`-1` means default-expanded).
4. If the filtered build produced zero items, rebuild once more with
   `filter = NULL` and set `no_matches` (the title shows `(filter: no
   matches)`; an over-narrow filter is ignored rather than showing an empty
   list) (mode-tree.c:673-675).
5. Flatten expanded items depth-first into `line_list`
   (`mode_tree_build_lines`, mode-tree.c:296-350), assigning each visible line
   its shortcut key via `keycb` (window-tree supplies the `-K` format; without
   a keycb the fallback is `'0'+line` for lines 0-9 and `M-a`..`M-z` for
   10-35, mode-tree.c:323-333).
6. Restore the selection: `mode_tree_set_current(tag)` — if the tagged item
   still exists select it (scrolling it into view); otherwise clamp `current`
   to the last line (mode-tree.c:497-520).
7. Recompute `width` (= full screen width) and `height` (§3.1), and re-clamp
   the scroll offset (`mode_tree_check_selected`, mode-tree.c:277-286).

The builder may also *override* the selection by writing a tag into `*tag`:
window-tree does this only on the first build (using `data->type`, which is
reset to `WINDOW_TREE_NONE` immediately after `window_tree_init`'s first
build, window-tree.c:459-474, 1146) — that is what makes the chooser open with
the invoking session/window/pane selected.

### 2.3 window-tree's builder (window-tree.c:428-475)

```
sort_get_sessions()                 -- all sessions, sorted (§4)
  for each (group-squashed) session:
    window_tree_build_session       -- one root item, name = session name
      sort_get_winlinks_session()   -- session's winlinks, sorted
        window_tree_build_window    -- child item, name = winlink index ("%u")
          sort_get_panes_window()   -- window's panes, sorted
            window_tree_build_pane  -- grandchild, name = pane index ("%u")
```

Windows with zero panes after filtering are removed, and sessions whose
windows were all filtered out are removed (window-tree.c:370-376, 421-425).
Panes are filtered by evaluating the `-f` format per pane
(`window_tree_filter_pane`, window-tree.c:303-318). Single-pane windows still
get a pane child (it is filtered later only by `-h`); the item name for
windows/panes is right-aligned per depth (`mode_tree_align(mti, 1)`,
window-tree.c:300, 358; alignment width = longest name at that depth,
mode-tree.c:864-872).

Expansion defaults (window-tree.c:349-353, 406-409): with `-s` sessions AND
windows are collapsed; with `-w` windows are collapsed (sessions expanded);
with neither everything is expanded. Pane items pass `expanded = -1`
(irrelevant — they have no children).

---

## 3. Screen layout — the exact sizing rule

### 3.1 List height vs. preview height (`mode_tree_set_height`, mode-tree.c:626-654)

Let `sy` = the mode screen's height (the full pane height), `h` = the list
height (`mtd->height`), so the preview region is rows `h .. sy-1` (`sy - h`
rows including the box border). No mode in this family supplies a `heightcb`,
so the default rule always applies:

```c
if (mtd->preview == MODE_TREE_PREVIEW_NORMAL) {
        mtd->height = (screen_size_y(s) / 3) * 2;      /* list gets 2/3 */
        if (mtd->height > mtd->line_size)
                mtd->height = screen_size_y(s) / 2;    /* short list: 1/2 */
        if (mtd->height < 10)
                mtd->height = screen_size_y(s);        /* too small: no preview */
} else if (mtd->preview == MODE_TREE_PREVIEW_BIG) {
        mtd->height = screen_size_y(s) / 4;            /* list gets 1/4 */
        if (mtd->height > mtd->line_size)
                mtd->height = mtd->line_size;
        if (mtd->height < 2)
                mtd->height = 2;
} else
        mtd->height = screen_size_y(s);                /* preview off */
if (screen_size_y(s) - mtd->height < 2)
        mtd->height = screen_size_y(s);                /* preview must be >= 2 rows */
```

In words, for the **normal** preview:

- The list gets two thirds of the pane (integer `sy/3*2`), the preview the
  remaining third.
- If the list is *shorter* than two thirds (`line_size < h`), the list area
  shrinks to **half** the pane, giving the preview more room. (It does not
  shrink to exactly `line_size`; that keeps the layout stable.)
- If the resulting list height would be **< 10 rows**, the preview is dropped
  entirely (`h = sy`). Combined with the 2/3 rule this means the preview only
  exists at all when the pane is **≥ 15 rows** tall.
- Final guard: if fewer than 2 rows would remain for the preview, drop it.

Additionally `mode_tree_draw` refuses to paint the preview when
(mode-tree.c:980-981):

```c
sy = screen_size_y(s);
if (sy <= 4 || h < 2 || sy - h <= 4 || w <= 4)
        goto done;
```

i.e. the box is only drawn when the pane is ≥ 5 rows, the list kept ≥ 2 rows,
the preview region is ≥ 5 rows, and the pane is ≥ 5 columns. With `w == 0 ||
h == 0` nothing is drawn at all (mode-tree.c:837-840); with zero items the
whole draw returns immediately (mode-tree.c:834-835). If every item disappears
while open, the next key press exits the mode (`mode_tree_key` returns
"finished" when `line_size == 0`, mode-tree.c:1507-1510).

`v` cycles preview state at runtime: OFF→BIG→NORMAL→OFF... (mode-tree.c:
1792-1807; in 3.5a `v` is a simple on/off toggle). Rebuild recomputes heights;
the selection is re-clamped into view.

### 3.2 The preview box and its title (mode-tree.c:989-1027)

- Box: cursor to `(0, h)`, then `screen_write_box(ctx, w, sy - h,
  BOX_LINES_DEFAULT, &box_gc, NULL)` — full width, from the row right under
  the list to the bottom of the pane. `box_gc` = the
  **`tree-mode-border-style`** window option (default
  `bg=themedarkgrey,fg=themelightgrey`; 3.5a uses `mode-style` for the
  selection only and default cells for the box).
- Title text is written *over the top border* starting at column 1:

  ```
  " <item-name> (sort: <field>[, reversed])[ (view: preview|info)]"
  ```

  where `<item-name>` is the current item's name (session name / window index
  / pane index), `<field>` is `index`/`name`/`activity`/`z` etc.
  (mode-tree.c:992-1000; the `(view: ...)` part is master-only, fed by
  `mode_tree_view_name`). If a filter is active and space allows, it appends
  `" (filter: active) "` or `" (filter: no matches) "` (mode-tree.c:1005-1017).
  The whole title is skipped if it doesn't fit in `w - 2`.
- Preview interior: cursor to `(2, h + 1)`, size `box_x = w - 4` by `box_y =
  sy - h - 2`, then the mode's `drawcb` paints into it (mode-tree.c:1021-1027).
  So the content is inset one cell from the border on each side horizontally
  (2 from pane edge) and one row vertically.
- If the current line is a pane row being drawn "as parent"
  (`draw_as_parent`), the preview shows the parent item instead
  (mode-tree.c:984-987; not used by window-tree).

### 3.3 List rows (mode-tree.c:874-974)

Each visible row `i` (from `offset` to `offset + h - 1`):

- **Prefix** — `MODE_TREE_PREFIX_FORMAT` (mode-tree.c:42-54), a format string
  drawing: the shortcut key right-padded in parentheses (`(0)`, `(M-a)`,
  padded to the widest key + 3), then per-depth ACS tree branch characters
  (`│` continuation unless the parent was a last-sibling, `├─>`/`└─>` for
  branches), then a red `-` (expanded) or green `+` (collapsed) marker for
  items with children, or two spaces for leaf items in a non-flat tree.
  Format variables set per row: `mode_tree_key`, `mode_tree_key_width`,
  `mode_tree_selected`, `mode_tree_repeat` (depth-1), `mode_tree_branch`,
  `mode_tree_parent_last`, `mode_tree_has_children`, `mode_tree_last`,
  `mode_tree_expanded`, `mode_tree_flat` (mode-tree.c:884-910). [The ACS
  branch drawing is master-only; 3.5a indents with spaces and uses `+`/`-`
  markers only.]
- **Name** — right-aligned per depth for window/pane rows, then `*` if the
  item is tagged, then a grey `: ` separator, then the item's **text** (the
  `-F` format expansion) filling the rest of the line (mode-tree.c:916-930).
- Tagged rows are recoloured theme-cyan (mode-tree.c:932-935).
- The **current row** is painted with **`tree-mode-selection-style`**
  (default `#{E:mode-style}`, and `mode-style` defaults to
  `noattr,bg=themeyellow,fg=themeblack`) with clear-to-end-of-line in that
  background (mode-tree.c:952-965); all other rows use default cells.
- After drawing, the (hidden) cursor is parked on the current row
  (mode-tree.c:1034-1037); the screen's cursor mode is enabled only while an
  in-mode prompt is open.

Scrolling: `current`/`offset` maintain a classic scrolling window; Up at top
wraps to bottom (and vice versa) when invoked from the arrow keys
(`mode_tree_up/down` with `wrap=1`, mode-tree.c:363-398).

---

## 4. Ordering — default, stability, comparators

### 4.1 Sort criteria plumbing

`mode_tree_start` seeds `sort_crit.order = sort_order_from_string(args_get('O'))`
— which is `SORT_END` when `-O` is absent — and `reversed = args_has('r')`
(mode-tree.c:586-587). Before every build, the mode's `sortcb` installs its
order sequence and replaces `SORT_END` with the sequence's first entry
(mode-tree.c:670-671):

```c
static enum sort_order window_tree_order_seq[] = {   /* window-tree.c:137-143 */
        SORT_INDEX, SORT_NAME, SORT_ACTIVITY, SORT_Z, SORT_END,
};
static void window_tree_sort(struct sort_criteria *sort_crit) {
        sort_crit->order_seq = window_tree_order_seq;
        if (sort_crit->order == SORT_END)
                sort_crit->order = sort_crit->order_seq[0];   /* => SORT_INDEX */
}
```

**The default sort for choose-tree is `index`.** `O` steps to the next entry
in the sequence (wrapping) (`sort_next_order`, sort.c:334-354); `r` flips
`reversed` (mode-tree.c:1726-1732). Both trigger an immediate rebuild. The man
page documents `-O` values for choose-tree as `index`, `name`, `activity`,
`z`. (`sort_order_from_string` also accepts `creation`, `size`, `order`,
`modifier`, and aliases `key`→index, `title`→name — sort.c:356-380 — but only
the mode's sequence is reachable via `O` cycling; `-O` can force any value.)

### 4.2 What each field means (sort.c comparators)

Collectors iterate the primitive containers, then `qsort` **unless** the order
is `SORT_ORDER` (insertion order; only optionally reversed in place,
sort.c:28-51):

- Sessions (`sort_get_sessions`, sort.c:464-485): iterates the global
  `sessions` red-black tree — which is keyed by **session name**
  (session.c:41-45) — then sorts. `sort_session_cmp` (sort.c:142-193):
  - `SORT_INDEX`: `sa->id - sb->id` — **the session's numeric id ($N), i.e.
    creation order**, not a user-visible "index".
  - `SORT_CREATION`: `creation_time` ascending.
  - `SORT_ACTIVITY`: `activity_time` **descending** (most recent first).
  - `SORT_NAME`: `strcmp(name)`.
  - Tie-break for every field: `strcmp(name)`; `reversed` negates the final
    result.
- Winlinks (`sort_get_winlinks_session`, sort.c:597-619): iterates the
  session's winlink RB tree (keyed by index). `sort_winlink_cmp`
  (sort.c:241-296): `SORT_INDEX` = `wla->idx - wlb->idx` (the window index);
  `CREATION`/`ACTIVITY` use the window's times (activity descending);
  `NAME` = window name; `SIZE` = area `sx*sy`. Tie-break: window name.
- Panes (`sort_get_panes_window`, sort.c:547-569): iterates the window's pane
  list in pane order. `sort_pane_cmp` (sort.c:195-239): `SORT_INDEX` = pane
  index; `ACTIVITY` = `active_point` **ascending** (a monotonic counter bumped
  on focus — note: *not* reversed like the time-based ones); `CREATION` = pane
  id; `NAME` = pane title; `SORT_Z` = z-index (floating panes). Tie-break:
  pane title.

### 4.3 Stability — the answer to "does tmux re-sort under me?"

Under the default `index` order the comparison keys are session id, winlink
index, and pane index — none of which change from output, focus, or attach
activity. The list order therefore **never changes while the chooser is open**
except when the user changes it (`O`, `r`, `S-Up`/`S-Down` swap) or when items
are actually created/destroyed/renumbered elsewhere. There is no "bump the
active session to the top" behavior anywhere in tmux's chooser. A re-sort on
rebuild can reorder rows only if the user selected `activity` (times change
constantly) or `name` (renames). winmux's current dynamically re-sorting list
is a divergence; the fix is: default `index` order = creation order for
sessions, index order for windows/panes, and rebuilds that preserve order,
selection (by identity tag), expansion, and tag marks.

Also note `qsort` is unstable, but every comparator has a deterministic
name/title tie-break, so equal-keyed items still order deterministically.

### 4.4 `S-Up` / `S-Down` (K/J) — swapping windows [master-only]

`mode_tree_swap` (mode-tree.c:400-427) finds the adjacent line at the same
depth with the same parent and calls the mode's `swapcb`.
`window_tree_swap` (window-tree.c:1002-1051) only swaps **window items within
the same session**, and refuses when the current sort order would not show the
swap (`sort_would_window_tree_swap`, sort.c:404-412 — under `index` sort swaps
are always allowed; under other sorts only if the two compare equal). The swap
exchanges the two winlinks' `window` pointers (so the *indexes stay put and
the windows move*), fixes `curw`, synchronizes the session group, and
redraws. On success the selection follows the moved window.

---

## 5. Tree structure, navigation and formats

### 5.1 Expand / collapse

- `Left`/`h`/`-` (mode-tree.c:1734-1746): if the line is flat (no tree
  structure at this level) or already collapsed, move to the **parent** (or
  just move up if at root); otherwise collapse the current item.
- `Right`/`l`/`+` (mode-tree.c:1747-1756): if flat or already expanded, move
  **down** one line; otherwise expand.
- `M--` / `M-+`: collapse / expand **all root items** (sessions)
  (mode-tree.c:1757-1766).
- Every expansion change triggers `mode_tree_build` (state preserved per
  §2.2).

### 5.2 Selection movement

- `Up`/`k`/`C-p` and `Down`/`j`/`C-n`: move with wrap (mode-tree.c:1642-1653).
- `PPage`/`C-b`, `NPage`/`C-f`: move a full list-height page
  (mode-tree.c:1662-1677).
- `g`/`Home`: top; `G`/`End`: bottom (mode-tree.c:1678-1690).
- Per-line shortcut keys: any key equal to a visible line's shortcut selects
  that line and **immediately acts as Enter** (mode-tree.c:1615-1630).

### 5.3 `WINDOW_TREE_DEFAULT_FORMAT` (window-tree.c:39-55)

```
#{?pane_format,
    #{?pane_marked,#[fg=thememagenta],}#{?pane_floating_flag,#[underscore],}
    #{pane_current_command}#[fg=themelightgrey]#{pane_flags}
    #{?#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}},: "#{pane_title}",}
,window_format,
    #{?window_marked_flag,#[fg=thememagenta],}
    #{window_name}#[fg=themelightgrey]#{window_flags}
    #{?#{&&:#{==:#{window_panes},1},#{&&:#{pane_title},#{!=:#{pane_title},#{host_short}}}},: "#{pane_title}",}
,
    #[fg=themelightgrey]#{session_windows} windows
    #{?session_grouped, (group #{session_group}: #{session_group_list}),}
    #{?session_attached, (attached),}
}
```

One format serves all three row types, keyed off `pane_format` /
`window_format` (set by tmux when the format tree carries a pane / winlink):

- **Pane rows**: current command + pane flags (`*` active, `Z` zoomed, `M`
  marked...), plus `: "title"` when the pane title is set and differs from the
  short hostname. Marked pane is magenta, floating pane underscored.
- **Window rows**: window name + window flags (`*`/`-`/`#`/`!`/`~`/`Z`), plus
  `: "title"` for single-pane windows with a meaningful title. Marked window
  magenta.
- **Session rows**: `N windows`, `(group g: list)` when grouped,
  `(attached)` when attached.

The item *name* column that precedes this text is the session name / winlink
index / pane index respectively (§2.3). (3.5a's format is the same shape,
with `colour219`/`colour37` instead of theme colours and no floating flag.)

For pane rows the format is created with `FORMAT_PANE|wp->id`; for window rows
with the active pane's id; for session rows with the current window's active
pane's id (window-tree.c:290-292, 341-344, 399-402) — so `#{pane_...}`
variables resolve sensibly at every level.

### 5.4 Key format `WINDOW_TREE_DEFAULT_KEY_FORMAT` (window-tree.c:57-62)

```
#{?#{e|<:#{line},10},#{line},#{e|<:#{line},36},M-#{a:#{e|+:97,#{e|-:#{line},10}}}}
```

Line 0-9 → `0`..`9`; line 10-35 → `M-a`..`M-z`; beyond → no shortcut. Rendered
into the prefix as `(0)`, `(M-a)`... `window_tree_get_key`
(window-tree.c:973-1000) evaluates it with `#{line}` plus the item's
session/window/pane defaults.

---

## 6. The preview — content per item type

`window_tree_draw` (window-tree.c:879-912) dispatches on the current item
(after the `i` info toggle, §6.4):

### 6.1 Session item → strip of window miniatures (window-tree.c:515-654)

The preview area becomes a **horizontal filmstrip of the session's windows**,
each slot showing a live miniature of that window's **active pane** screen:

- `total` = number of winlinks. If `sx / total < 24` (slots would be narrower
  than 24 columns) then only `visible = sx / 24` windows (min 1) are shown;
  otherwise all.
- The visible range is centered on the session's **current window**
  (`start`/`end` window; exact rule window-tree.c:547-556), then shifted by
  the user's `<`/`>` offset (`data->offset`, clamped, window-tree.c:558-563).
- If windows are cut off on the left/right, a 3-column arrow gutter is drawn
  on that side: a full-height vertical line at `cx+2` / `cx+sx-3` with a `<` /
  `>` glyph at mid-height (window-tree.c:582-598). Both gutters are dropped
  when `sx <= 6` (both) or `<= 3` (one) (window-tree.c:565-568).
- Remaining width is divided evenly: `each = (sx - gutters) / visible`; each
  slot is `each - 1` wide with a vertical separator line, and the last slot
  absorbs the remainder (window-tree.c:569-579, 645-649). If `each == 0`
  nothing is drawn.
- Each slot: `screen_write_preview(ctx, &w->active->base, width, sy)` — a
  colour-true copy of the window's active pane (§6.5) — then a centered
  **label**: the `tree-mode-preview-format` option (default
  `#{window_index}:#{window_name}` for windows), drawn inside a 3-row mini box
  (`window_tree_draw_label`, window-tree.c:477-505: requires the slot ≥ 5x3,
  label trimmed to slot width - 4, box centered). Label style =
  `tree-mode-preview-style` (default: theme-red fg if this is the active
  window, theme-blue otherwise), on the border style's background. [In 3.5a
  the label is hardcoded `"%u:%s" (idx, name)` and the colours are
  red/blue literals.]

### 6.2 Window item → strip of pane miniatures (window-tree.c:656-802)

Identical algorithm over the window's panes (in pane order), centered on the
window's **active pane**, each slot a `screen_write_preview` of that pane's
`base` screen labelled `#{pane_index}:#{pane_title}` (per the same option
default). The mode's own pane is skipped when `-h` was given.

### 6.3 Pane item → single full preview (window-tree.c:907-910)

`screen_write_preview(ctx, &wp->base, sx, sy)` — the entire preview interior
is one live miniature of that pane.

### 6.4 The `i` info view [master-only] (window-tree.c:804-877, 1481-1487)

`i` toggles `preview_is_info`; the preview interior instead shows stacked
field tables — for a pane item: Pane/Title/Command/Path/TTY/Position/Mode/
Flags, then a horizontal rule, then the Window table
(Window/Size/Panes/Activity Time/Sessions/Flags), another rule, then the
Session table (Session/Created/Activity/Attached Time/Clients/Windows/Path/
Group/Flags); window items get Window+Session tables; session items just the
Session table. Flags render green when set, grey when not
(`WINDOW_TREE_FLAG`, window-tree.c:145-146). The box title's `(view:
preview|info)` reflects the toggle.

### 6.5 `screen_write_preview` — what a miniature actually is (screen-write.c:936-988)

```c
/* If the cursor is on, pick the area around the cursor, otherwise
   use the top left. */
```

- Chooses the source viewport: if the source screen has a visible cursor
  (`MODE_CURSOR`), the copied `nx * ny` window is positioned so the cursor
  sits about **one third** in from the left/top edges, clamped to the screen;
  otherwise the top-left corner.
- Copies with `screen_write_fast_copy(ctx, src, px, src->grid->hsize + py,
  nx, ny)` (screen-write.c:665-718): a raw cell-by-cell copy of the source
  grid **including all attributes and colours** — note the `hsize` offset,
  i.e. it reads the *visible screen*, never scrollback. Wide characters that
  would straddle the right edge stop the row copy.
- Then the cursor is made visible in the miniature: the cell under the source
  cursor is re-written with `GRID_ATTR_REVERSE` added
  (screen-write.c:981-987).

So: the preview is the pane's real screen content, truncated (never scaled)
to the preview rectangle, with colours, following the cursor, cursor shown as
a reverse-video cell.

### 6.6 Does the preview update live?

The preview repaints whenever `mode_tree_draw` runs, and the tree *rebuilds
and* repaints whenever the mode's `update` callback runs. The triggers:

1. **Every key/mouse event handled by the mode** ends with `mode_tree_draw` +
   `PANE_REDRAW` (window-tree.c:1553-1558).
2. **`window_tree_update`** (window-tree.c:1191-1199) = `mode_tree_build` +
   `mode_tree_draw` + `PANE_REDRAW`. It is invoked from
   `server_client_check_modes` (server-client.c:2215-2230) during the
   per-loop redraw check **for every pane of the client's current window whose
   client has `CLIENT_REDRAWSTATUS` set**. That flag is set by:
   - the status timer, once per `status-interval` (default **15** seconds;
     options-table.c:1055-1062, status.c:38-58);
   - `server_status_client` (server-fn.c:40-43), which fires on essentially
     every structural change — window created/killed/renamed, session
     created/destroyed/renamed, mode entered/left, pane killed, activity/bell
     alerts, key-table changes, etc.
3. Pane resize → `window_tree_resize` → `mode_tree_resize` (rebuild + draw,
   mode-tree.c:723-734).

**Plain pane output does not repaint the preview by itself** (pane output only
flags that pane, not the chooser's pane). In practice the preview looks live
because any structural event or status tick refreshes it, and every keystroke
inside the chooser redraws — but a shell printing output in another pane will
only show up in the preview at the next status tick / event / keypress. A
winmux implementation may choose to refresh on its 50ms tick instead; visually
that is a superset of tmux's behavior (divergence-note-worthy, not
user-hostile).

Kill/command operations queue `window_tree_command_done`
(window-tree.c:1254-1266) after the executed command, which rebuilds and
redraws so the list reflects e.g. a killed window immediately.

---

## 7. Complete key handling

Keys reach `window_tree_key` (window-tree.c:1430-1559), which first lets the
generic `mode_tree_key` (mode-tree.c:1494-1810) process the key; whatever the
framework does not consume falls through (possibly rewritten, e.g. a
double-click becomes `\r`) to the mode-specific switch.

### 7.1 Generic mode-tree keys (mode-tree.c:1632-1808)

| Key | Action |
|---|---|
| `q`, `Escape`, `C-[`(same), `C-g` | exit the mode (`mode_tree_key` returns finished; caller does `window_pane_reset_mode`) |
| `F1`, `C-h` | help overlay: centered box listing keys; any next key closes it [master-only] |
| `Up`/`k`/`C-p` | up, wraps |
| `Down`/`j`/`C-n` | down, wraps |
| `S-Up`/`K`, `S-Down`/`J` | swap current item with same-depth neighbor via `swapcb` (window swap, §4.4) [master-only] |
| `PPage`/`C-b`, `NPage`/`C-f` | page up / down |
| `g`/`Home`, `G`/`End` | top / bottom |
| `t` | toggle tag on current item (mutually exclusive with its parents/children: tagging an item untags all ancestors and descendants, mode-tree.c:1691-1710); items flagged `no_tag` refuse |
| `T` | untag everything |
| `C-t` | tag **all root items** (all sessions; precisely: parentless non-`no_tag` items, plus children of `no_tag` parents) |
| `O` | next sort field in the mode's sequence; rebuild |
| `r` | reverse sort; rebuild |
| `Left`/`h`/`-` | collapse current, or jump to parent (§5.1) |
| `Right`/`l`/`+` | expand current, or move down (§5.1) |
| `M--` / `M-+` | collapse / expand all roots |
| `/`, `?`, `C-s` | open the `(search)` prompt; on each change jumps to the next match **forward** (note `?` is also forward in tree mode — copy-mode's backward `?` does not apply here) |
| `n` / `N` | repeat search forward / backward |
| `f` | open the `(filter)` prompt, pre-filled with the current filter |
| `c` | clear the filter (and close any prompt) |
| `v` | cycle preview OFF → BIG → NORMAL → OFF [3.5a: on/off toggle] |
| line shortcut (`0`-`9`, `M-a`..`M-z`) | select that line and act as Enter |

Search semantics (mode-tree.c:1175-1316): matches by the mode's `searchcb`
— window-tree matches **session name** (session rows), **window name**
(window rows), or the pane's **running command name** via `osdep_get_name`
(pane rows) (window-tree.c:914-958). Case-insensitive iff the search string is
all-lowercase (smart-case, `mode_tree_is_lowercase`, mode-tree.c:228-237,
1309). The search wraps around the whole tree, traverses into *collapsed*
subtrees, and on a match expands all ancestors of the match and selects it
(mode-tree.c:1269-1293).

Filter semantics (mode-tree.c:1318-1352): the filter string is a **format**
evaluated per *pane* (window-tree.c:303-318); rows whose whole subtree fails
the filter disappear (§2.3). Empty input clears it. A filter matching nothing
is ignored and the title shows `(filter: no matches)`.

### 7.2 window-tree specific keys (window-tree.c:1459-1551, man)

| Key | Action |
|---|---|
| `Enter` (`\r`) | expand `%%`/`%1` in the template with the current item's target and run it; exit the mode. Default template `switch-client -Zt '%%'`. Targets: session `=name:`, window `=name:idx.`, pane `=name:idx.%%id` (window-tree.c:1201-1237 — note the `=` prefix for exact-name matching) |
| `x` | kill the **current** item with a confirm prompt: `Kill session <name>? ` / `Kill window <idx>? ` / `Kill pane <idx>? ` — single-key prompt; only `y`/`Y` proceeds. Kill = `session_destroy` / `server_kill_window` / `server_kill_pane` (window-tree.c:1296-1325), then `server_renumber_all()`. With `-y`, auto-answers `y` |
| `X` | same, for **all tagged** items: `Kill <N> tagged? ` (no-op when nothing is tagged) |
| `:` | command prompt, prompt text `(current) ` or `(<N> tagged) `; the entered command has `%%`/`%1` replaced by the target and is run **once per tagged item** (or once for the current item if none tagged), each with that item as the command's target context (window-tree.c:1239-1286, mode-tree.c:537-557) |
| `<` / `>` | scroll the preview filmstrip left / right (`data->offset--/++`) |
| `m` / `M` | set / clear the **marked pane** to the current item, rebuild [master-only] |
| `i` | toggle preview ↔ info view [master-only] |
| `H` | jump back to the starting pane: expands the invoking session + window and selects the invoking pane (falls back to the winlink) (window-tree.c:1466-1471) [master-only] |

When the selection changes for any reason the filmstrip offset resets to 0
(window-tree.c:1450-1453).

### 7.3 Prompts (search / filter / `:` / kill confirm)

This master hosts prompts **inside the mode screen** (`mtd->prompt`,
mode-tree.c:1041-1173): a one-line editor drawn on the mode's bottom row (top
row if `status-position top`), with the screen cursor enabled at the edit
position. While a prompt is open all keys go to the prompt editor
(mode-tree.c:1512-1558); a left-click on the prompt line repositions the
cursor. The kill confirms use `PROMPT_SINGLE` (first key is the answer) and
`-y` appends an automatic `y` (`mode_tree_prompt_accept`). [In 3.5a these
prompts are the regular *status-line* prompts (`status_prompt_set`) — same
texts and semantics, different rendering surface. For winmux, the existing
status-line prompt/confirm machinery is the right analogue.]

### 7.4 Mouse (mode-tree.c:1575-1610; window-tree.c:1369-1428)

Prereq: with `mouse on`, wheel/click on a pane in a mode is routed by the
default root bindings (`bind -n WheelUpPane { if -F
'#{||:#{alternate_on},#{pane_in_mode},#{mouse_any_flag}}' { send -M } {
copy-mode -e } }`, key-bindings.c:506; MouseDown1Pane analogues) as `send -M`
→ `window_pane_key` → the mode. With `mouse off`, tmux never sees wheel
events (most terminals' alternateScroll instead sends Up/Down arrow keys,
which work as normal movement).

Inside `mode_tree_key`'s mouse branch (coordinates via `cmd_mouse_at`):

- **Click (MouseDown1) on a list row**: select that row (no choose).
- **Double-click on a list row**: select + rewrite the key to `\r` → Enter
  (choose).
- **Right-click (MouseDown3) on a list row**: select + popup menu with the
  mode's items — for tree mode: Select(`\r`), Expand(Right), Mark(`m`),
  Tag(`t`), Tag All(`C-t`), Tag None(`T`), Kill(`x`), Kill Tagged(`X`),
  Cancel(`q`) (window-tree.c:64-79); choosing a menu entry replays that key
  with the clicked line current (mode-tree.c:1354-1418).
- **Right-click below the list** (preview area): generic menu — Scroll
  Left(`<`), Scroll Right(`>`), Cancel(`q`) (mode-tree.c:169-176).
- **Click in the preview area** (`window_tree_mouse`): clicking the `<`/`>`
  gutters scrolls the filmstrip; clicking a window/pane **miniature** expands
  the current item, selects the clicked child, and acts as Enter — i.e.
  click-through: clicking a window thumbnail in a session preview *chooses
  that window* (window-tree.c:1369-1428 → `'\r'`).
- **Wheel over the list**: consumed with no effect in this code — the mouse
  branch swallows every mouse key except the three click types before the
  key switch is reached, and every path `return`s (mode-tree.c:1592-1609), so
  the `KEYC_WHEELUP_PANE`/`KEYC_WHEELDOWN_PANE` cases at mode-tree.c:1644,
  1650 (one line up/down) are unreachable for real mouse events (they would
  require a wheel key delivered with no mouse event struct, which
  `window_pane_key` rejects, window.c:1695-1696). Practical upshot: wheel
  scrolling of the list works via terminal alternateScroll arrow translation
  when `mouse off`, and is a no-op with `mouse on`. A winmux implementation
  that maps wheel to `mode_tree_up/down(3 lines)` would be a (defensible)
  divergence; matching tmux exactly means click/double-click/right-click only.
- Mouse-move events never reach the mode (window.c:1704-1705); there is no
  drag behavior in choosers.

### 7.5 Exit paths

`q`/`Escape`/`C-g` (finished from `mode_tree_key`), Enter (after running the
template), or every item vanishing. All call `window_pane_reset_mode`
(window-tree.c:1553-1554); `-Z` unzoom and `-k` pane-kill fire there.

---

## 8. Live rebuild while open — exact triggers

Rebuild = `mode_tree_build` (§2.2 — preserves selection by identity tag, tag
marks, expansion; clamps selection when the item died).

1. `window_tree_update` via `server_client_check_modes`
   (server-client.c:2215-2230) whenever the owning client is flagged
   `CLIENT_REDRAWSTATUS`: every `status-interval` tick (default 15s) and any
   `server_status_client` event — session/window create/destroy/rename in any
   session, mode changes, alerts, etc. **This is how a window created or
   killed elsewhere appears/disappears in an open chooser.**
2. User actions inside the mode: `O`/`r`, expand/collapse, filter change,
   search jump (expands ancestors), `m`/`M`, swap, buffer delete / client
   detach (their handlers call `mode_tree_build` directly), and the queued
   `..._command_done` callback after `x`/`X`/`:` commands execute.
3. Resize (`mode_tree_resize`).

There is **no dedicated chooser timer** — the status-interval timer is the
only periodic driver.

---

## 9. choose-client (window-client.c) — brief

Same framework; flat list (no hierarchy), one root item per **attached,
non-control** client (`sort_get_clients` skips unattached, sort.c:437-462).

- Item name = client name (`c->name`, typically the tty path); identity tag =
  client pointer.
- Default line format: `#{t/p:client_activity}: session #{session_name}`
  (window-client.c:40-41; grey-themed on master).
- Sort sequence: **name** (default), size (tty area), creation, activity
  (window-client.c:185-191; man documents `-O name|size|creation|activity`).
- Default template: `detach-client -t '%%'` (window-client.c:38); `%%` is
  replaced by the client name. Enter runs it and exits.
- Preview (`window_client_draw`, window-client.c:295-349): what that client
  currently sees — `screen_write_preview` of the client's current window's
  **active pane** sized `sx x (sy - 2 - status_lines)`, then a horizontal
  border line, then a **`screen_write_fast_copy` of the client's real status
  line screen** (`c->status.screen`) at the bottom (or top when the client
  has `status-position top`). `-i` / `i` toggles an info sheet (client
  name/PID, session, times, TERM, size, features, key options)
  [info view master-only].
- Extra keys: `d`/`x` detach current (`x` = detach-and-kill: `MSG_DETACHKILL`,
  `z` suspend; `D`/`X`/`Z` = same over tagged items (window-client.c:505-561).
  No confirm prompts. Selection moves down before detaching the current row.
- No `searchcb` (search falls back to substring on the item **name**,
  mode-tree.c:1210-1215). No swap. Menu: Detach, Detach Tagged, Tag, Tag All,
  Tag None, Cancel (window-client.c:141-152).
- Mode exits automatically when the last client goes away
  (window-client.c:562-563).

## 10. choose-buffer (window-buffer.c) — brief

- One root item per paste buffer; item name = buffer name; identity tag = the
  buffer's monotonically-increasing order number (window-buffer.c:190).
- Default line format: `#{t/p:buffer_created}: #{buffer_sample}`
  (window-buffer.c:42-43).
- Sort sequence: **creation** (default; newest first — the comparator inverts
  on `order`, sort.c:67-73), name, size (window-buffer.c:110-115).
- Default template: `paste-buffer -p -b '%%'` (window-buffer.c:40).
- Preview (`window_buffer_draw`, window-buffer.c:198-232): the first `sy`
  lines of the buffer's content, each line clipped to `sx`, control
  characters/tabs made visible with `utf8_strvis(VIS_OCTAL|VIS_CSTYLE|
  VIS_TAB)`. Plain text — no colours, no cursor.
- Extra keys: `Enter`/`p` paste current (runs template, exits), `P` paste all
  tagged (exits), `d` delete current (selection steps down, or up at the
  end — window-buffer.c:447-468), `D` delete tagged, `e` open the buffer in
  the configured editor (spawns editor, shows a centered `WAITING FOR EDITOR`
  box, replaces the buffer content on save; window-buffer.c:488-614).
- Search matches buffer **name or content** (window-buffer.c:256-280).
- Mode exits automatically when no buffers remain (window-buffer.c:627-630,
  667).

(`customize-mode` and the fuzzy `switch-mode` also exist; `customize-mode` is
mode-tree based, `switch-mode` is a separate non-mode-tree fuzzy list —
neither is needed for prefix-s/w parity.)

---

## 11. Defaults table

| Thing | Default | Where |
|---|---|---|
| prefix-s | `choose-tree -Zs` | key-bindings.c:432 |
| prefix-w | `choose-tree -Zw` | key-bindings.c:436 |
| prefix-= | `choose-buffer -Z` | key-bindings.c:412 |
| prefix-D | `choose-client -Z` | key-bindings.c:414 |
| choose-tree template | `switch-client -Zt '%%'` (3.5a: no `-Z`) | window-tree.c:37 |
| choose-client template | `detach-client -t '%%'` | window-client.c:38 |
| choose-buffer template | `paste-buffer -p -b '%%'` | window-buffer.c:40 |
| choose-tree sort seq | index → name → activity → z (default **index**) | window-tree.c:137-143 |
| choose-client sort seq | name → size → creation → activity (default **name**) | window-client.c:185-191 |
| choose-buffer sort seq | creation → name → size (default **creation**, newest first) | window-buffer.c:110-115 |
| choose-tree `-F` | `WINDOW_TREE_DEFAULT_FORMAT` (§5.3) | window-tree.c:39-55 |
| `-K` key format (all) | 0-9 then M-a..M-z (§5.4) | window-tree.c:57-62 |
| list/preview split | list 2/3 (short list: 1/2); preview dropped if list < 10 rows or preview < 5 rows (§3.1) | mode-tree.c:626-654, 980-981 |
| preview interior | inset (2, 1) inside a full-width box; `w-4 x sy-h-2` | mode-tree.c:989-1027 |
| `status-interval` (update tick) | 15 seconds | options-table.c:1055-1062 |
| `tree-mode-border-style` [master] | `bg=themedarkgrey,fg=themelightgrey` | options-table.c:1714-1721 |
| `tree-mode-preview-format` [master] | `#{?pane_format,#{pane_index}:#{pane_title},#{window_index}:#{window_name}}` | options-table.c:1723-1730 |
| `tree-mode-preview-style` [master] | red fg when active window/pane, else blue | options-table.c:1732-1741 |
| `tree-mode-selection-style` [master] | `#{E:mode-style}` | options-table.c:1745-1752 |
| `mode-style` | `noattr,bg=themeyellow,fg=themeblack` (3.5a: `bg=yellow,fg=black`) | options-table.c:1464-1471 |
| filmstrip min slot width | 24 columns (else fewer slots + `<`/`>` gutters) | window-tree.c:533-538, 678-683 |
| kill confirm prompts | `Kill session <name>? ` / `Kill window <idx>? ` / `Kill pane <idx>? ` / `Kill <N> tagged? `, answer `y`/`Y` only | window-tree.c:1489-1531 |

---

## 12. Windows/winmux applicability notes

- **The two user-reported gaps have exact fixes here.** (a) Ordering: default
  to `index` — sessions by creation id, windows by index, panes by index —
  and never reorder on rebuild; preserve selection by identity (session
  id / window id / pane id as the `tag`), plus expansion + tag marks, exactly
  as `mode_tree_build`/`mode_tree_add` do (§2.2). (b) Preview: a bottom box
  sized by the §3.1 rule, painted from the selected item's grid.
- winmux's `grid` already holds per-pane cell matrices, so
  `screen_write_preview` translates directly: copy the visible cell window
  (cursor-following, one-third rule) from the source pane's grid into the
  overlay region with full SGR, then reverse-video the cursor cell. The
  session/window filmstrip is a loop of such copies with vertical separator
  columns, 24-col minimum slot width and `<`/`>` gutters.
- winmux's choose-tree lives in `server::dispatch` as an overlay composited by
  `render`; the mode-tree sizing rule (list 2/3, preview dropped when list
  < 10 rows or pane < 15 rows, box full-width) and the box-title string
  (` <name> (sort: index)`) should be reproduced verbatim for the tmux feel.
- Update cadence: tmux refreshes the open chooser on status ticks (15s) and
  structural events only — winmux's 50ms server tick can refresh the preview
  more often at zero extra architectural cost; that's a visible-but-benign
  divergence worth a line in `docs/follow-ups.md`. The essential part is that
  **rebuilds must not move the selection or reorder rows**.
- Prompt surfaces: winmux's existing status-line prompt/confirm machinery
  matches tmux 3.5a's chooser prompts (search `(search) `, filter
  `(filter) `, command `(current) `/`(N tagged) `, kill confirms with y/n);
  the in-mode bottom-row prompt is a master-only refinement.
- Theme colours (`themered` etc.) and the ACS branch-line prefix are
  master-only polish; 3.5a-compatible rendering (space indentation, `+`/`-`
  expand markers, `mode-style` selection bar, default-colour box) is the
  right first target for winmux.
- Wheel: to be "exactly like tmux" the list should *not* scroll on wheel when
  winmux's own mouse mode is on (clicks select, double-click chooses,
  right-click menu optional); winmux may still choose wheel-moves-selection
  as a marked divergence since ConPTY consoles rarely provide alternateScroll
  arrow translation.
- `osdep_get_name` (pane search by running command) has no cheap ConPTY
  analogue; searching pane rows by the OSC-captured title (which winmux
  already tracks for `automatic-rename`) is the practical substitute.
