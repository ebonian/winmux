# tmux mouse handling — authoritative behavioral reference

Source studied: tmux master `db115c6` (2026-07-07), full clone. All `file:line` references
are into that tree. This document is intended to be sufficient to implement tmux-identical
mouse behavior in winmux without reopening the tmux source.

> Note on vintage: this master includes post-3.5 features (per-pane scrollbars, floating
> panes, `Control0..9` pane-border-format ranges, `copy-mode -H`). Those are flagged where
> they appear; everything else is long-standing tmux behavior.

---

## 0. The big picture

```
terminal bytes ──tty_keys_mouse (tty-keys.c:1187)──▶ struct mouse_event + key=KEYC_MOUSE
      KEYC_MOUSE ──server_client_key_callback (server-client.c:1315)
                        └─▶ server_client_check_mouse (server-client.c:808)
                              • classifies TYPE   (Down/Up/Drag/DragEnd/Wheel/Second/Double/Triple/Move)
                              • classifies WHERE  (Pane/Border/Status*/Scrollbar*/Control0-9/Nowhere)
                              • runs the DRAG STATE MACHINE (tty.mouse_drag_flag / _update / _release)
                              • returns a synthetic key like MouseDrag1Border
                        ├─ key == KEYC_DRAGGING  ─▶ c->tty.mouse_drag_update(c, m)  [bypasses bindings]
                        └─ else                  ─▶ normal key-table lookup (mode table → root)
                                                     unbound mouse key ─▶ forwarded to the pane app
                                                     (input_key_mouse, only if app enabled mouse)
```

Three layers, three vocabularies:

1. **Wire layer** (`tty-keys.c`): SGR-1006 / legacy X10 byte sequences → `struct mouse_event`.
2. **Classification layer** (`server_client_check_mouse`): mouse_event → one synthetic key
   code, e.g. `MouseDown1Status`, `WheelUpPane`, `MouseDrag1Border`, `MouseDragEnd1Pane`.
3. **Binding layer** (`key-bindings.c` defaults + `window-copy.c` / `cmd-resize-pane.c`
   callbacks): the synthetic key runs through the normal key tables — *except* while a drag
   callback is installed, when motion bypasses bindings entirely.

---

## 1. Wire decoding (`tty-keys.c:1187-1324` `tty_keys_mouse`)

Two formats are accepted (UTF-8/1005 extension is **not** accepted on input, tmux.h comment
at tty-keys.c:1222):

- **Legacy**: `ESC [ M b x y` — three raw bytes, each offset by 32 (`MOUSE_PARAM_BTN_OFF`
  0x20, `MOUSE_PARAM_POS_OFF` 0x21, tmux.h:698-701); coordinates become 0-based.
- **SGR (1006)**: `ESC [ < b ; x ; y (M|m)` — decimal; `x--`, `y--` to 0-based. Final `M` =
  press/motion, `m` = release.

Filled into `struct mouse_event` (tmux.h:1627-1653):

| field | meaning |
|---|---|
| `x,y,b` | this event's position and raw button word |
| `lx,ly,lb` | **previous** event's position/button (from `tty->mouse_last_*`, updated at tty-keys.c:1318-1321) |
| `sgr_type` | `'M'`/`'m'` if SGR, `' '` if legacy |
| `sgr_b` | raw SGR button word (only meaningful when `sgr_type != ' '`) |
| `ox,oy` | window view offset (filled later, server-client.c:992) |
| `statusat,statuslines` | status-line position/height (filled later) |
| `s,w,wp` | resolved session id / window id / pane id (−1 = unset; filled during classification) |
| `valid` | set to 1 only after classification succeeds (server-client.c:1360) |
| `ignore` | 1 only for the timer-synthesized DoubleClick event; stops re-forwarding to apps (input-keys.c:805) |
| `key` | the final synthetic key (server-client.c:1361) |

Key SGR conventions (tmux.h:1596-1624):

```c
#define MOUSE_MASK_BUTTONS 195      /* 0xC3: bits 0,1 + 64,128 = button space  */
#define MOUSE_MASK_SHIFT 4
#define MOUSE_MASK_META 8
#define MOUSE_MASK_CTRL 16
#define MOUSE_MASK_DRAG 32          /* motion flag                              */
#define MOUSE_WHEEL_UP 64           /* buttons 64/65 = wheel                    */
#define MOUSE_WHEEL_DOWN 65
#define MOUSE_BUTTON_1 0 /* 2→1, 3→2; 66/67 = btn 6/7; 128..131 = btn 8..11 */
#define MOUSE_BUTTONS(b)  ((b) & MOUSE_MASK_BUTTONS)
#define MOUSE_DRAG(b)     ((b) & MOUSE_MASK_DRAG)
#define MOUSE_RELEASE(b)  (((b) & MOUSE_MASK_BUTTONS) == 3)   /* "no button" */
```

Decoder details that matter:

- On an SGR `'m'` release, `b` is overwritten with `3` ("no button") but `sgr_b` keeps the
  real released button (tty-keys.c:1293-1295). This is how tmux knows *which* button ended
  a drag under SGR.
- **PuTTY quirk**: SGR `'m'` with a wheel button is discarded outright (tty-keys.c:1303-1304),
  return −2 → `tty_keys_next` maps it to "consume bytes, emit nothing" (tty-keys.c:842-844).
- A successful decode yields `key = KEYC_MOUSE` (tty-keys.c:836-839) — a placeholder;
  classification happens server-side.
- `tty->mouse_last_x/y/b` persist **across** all events (not just drags): every event's
  `lx/ly/lb` is simply the previous event.

Reset on client tty start (`tty.c:388-390`): `mouse_drag_flag = 0`,
`mouse_drag_update = NULL`, `mouse_drag_release = NULL`. Also `mouse_last_pane = -1`
(tty.c:116). When a pane is destroyed mid-drag, the server clears
`mouse_last_pane`, `mouse_drag_update`, `mouse_scrolling_flag` for any client dragging in
it (server-client.c:3006-3009).

### What tmux asks the outer terminal for (`server-client.c:2080-2099`)

When the `mouse` option is on, per redraw tmux picks the outer tracking mode:

```c
if (options_get_number(oo, "mouse")) {
        ...aggregate pane MODE_MOUSE_ALL...
        if (focus-follows-mouse || modal/autohide scrollbars)
                mode |= MODE_MOUSE_ALL;      /* 1003: all-motion */
        else if (~mode & MODE_MOUSE_ALL)
                mode |= MODE_MOUSE_BUTTON;   /* 1002: button-event (drag) tracking */
}
```

**Button-event tracking (1002) is mandatory for dragging** — the comment at
server-client.c:2080-2084 says exactly this. tmux also always requests SGR (1006) alongside.
If any inner pane app enabled 1003, tmux upgrades the outer terminal to 1003 so motion can
be relayed.

---

## 2. Event classification — `server_client_check_mouse` (server-client.c:808-1169)

Called from `server_client_key_callback` only for `key == KEYC_MOUSE || key ==
KEYC_DOUBLECLICK` (server-client.c:1353). Read-only clients never get mouse
(server-client.c:1354). Returns one synthetic key code, or `KEYC_UNKNOWN` (event dropped).

### 2.1 TYPE derivation (server-client.c:834-898) — exact order

Evaluated top to bottom; first match wins. `x,y,b` become the event's *effective* position
and button:

| # | condition | type | effective x,y,b |
|---|---|---|---|
| 1 | `event->key == KEYC_DOUBLECLICK` (timer-synthesized, §2.4) | DOUBLECLICK | `m->x, m->y, m->b`; `ignore=1` |
| 2 | motion with **no** button: SGR: `MOUSE_DRAG(sgr_b) && MOUSE_RELEASE(sgr_b)`; legacy: `MOUSE_DRAG(b) && MOUSE_RELEASE(b) && MOUSE_RELEASE(lb)` | MOUSEMOVE | `m->x, m->y, 0` |
| 3 | `MOUSE_DRAG(m->b)` (motion with a button held) | MOUSEDRAG | if `mouse_drag_flag` set (update): `m->x, m->y, m->b` — and if `x==lx && y==ly` **return KEYC_UNKNOWN** (dedupe); else (start): **`m->lx, m->ly, m->lb`** — the position/button of the *previous* event, i.e. where the button went down |
| 4 | `MOUSE_WHEEL(m->b)` | WHEELUP / WHEELDOWN | `m->x, m->y, m->b` |
| 5 | `MOUSE_RELEASE(m->b)` (b==3) | MOUSEUP | `m->x, m->y`, **`b = m->lb`**, or `b = m->sgr_b` when `sgr_type=='m'` (real released button) |
| 6 | otherwise it is a press: if `CLIENT_DOUBLECLICK` flag set → SECONDCLICK (sets `CLIENT_TRIPLECLICK`, restarts sequence); else if `CLIENT_TRIPLECLICK` set → TRIPLECLICK; else → MOUSEDOWN (sets `CLIENT_DOUBLECLICK`) | | `m->x, m->y, m->b` |

Then `m->s = session id`, `m->w = m->wp = -1`, `m->ignore = ignore`
(server-client.c:904-908).

### 2.2 WHERE derivation (server-client.c:910-1036)

**Status line first** (server-client.c:911-970). If
`statusat <= y < statusat + statuslines`, look up the style range at `(x, y - statusat)`
via `status_get_range` (status.c:142-149):

| range found | location | side effect |
|---|---|---|
| none | `STATUS_DEFAULT` | |
| `STYLE_RANGE_NONE` | — | **return KEYC_UNKNOWN** (dead zone) |
| `STYLE_RANGE_LEFT` | `STATUS_LEFT` | |
| `STYLE_RANGE_RIGHT` | `STATUS_RIGHT` | |
| `STYLE_RANGE_PANE` | `STATUS` | `m->wp = pane id` (invalid pane → UNKNOWN) |
| `STYLE_RANGE_WINDOW` | `STATUS` | `m->w = window id` (invalid index → UNKNOWN) |
| `STYLE_RANGE_SESSION` | `STATUS` | `m->s = session id` |
| `STYLE_RANGE_USER` | `STATUS` | |
| `STYLE_RANGE_CONTROL` n | `CONTROL0+n` | (pane-border-format `#[range=user\|...]`-style controls; newer master) |

Ranges come from `#[range=left]`, `#[range=window|<index>]`, `#[range=right]` markers in the
default `status-format[0]` (options-table.c:119-183). This is how "which window tab was
clicked" is resolved — by rendered range, not by column arithmetic.

**Not on status** (server-client.c:976-1034): compute window-relative `px,py`:

```c
px = x;
if (m->statusat == 0 && y >= m->statuslines) py = y - m->statuslines;   /* status on top */
else if (m->statusat > 0 && y >= statusat)   py = m->statusat - 1;      /* clamp to above */
else                                          py = y;
px += m->ox; py += m->oy;                     /* window view offset (larger-window case) */
```

Then pick the pane:

```c
if (type == KEYC_TYPE_MOUSEDRAG && lwp != NULL)
        wp = lwp;                              /* drag in progress: STICK to the drag pane */
else
        wp = window_get_active_at(w, px, py);  /* pane under pointer */
if (wp == NULL) return KEYC_UNKNOWN;
loc = server_client_check_mouse_in_pane(wp, px, py, &sl_mpos);
m->wp = wp->id; m->w = wp->window->id;
```

`lwp` is `window_pane_find_by_id(c->tty.mouse_last_pane)` (server-client.c:828-832) — the
pane captured when the drag started. **All drag-update events are attributed to the
drag-origin pane**, no matter where the pointer is.

`window_get_active_at` (window.c:667-729) — containment for tiled panes **includes the
pane's right border column and bottom border row** (`x <= xoff+sx`, `y <= yoff+sy`), so a
border point maps to the pane whose right/bottom edge it is; panes are scanned in z-order.

`server_client_check_mouse_in_pane` (server-client.c:660-804) then distinguishes:

- inside pane body → `PANE`;
- on a scrollbar → `SCROLLBAR_UP` / `SCROLLBAR_SLIDER` (also outputs grab offset
  `sl_mpos`) / `SCROLLBAR_DOWN` (scrollbars are a 3.6+ feature; skip if not implementing);
- otherwise scan **all** panes' border lines (`yoff-1` top, `yoff+sy` bottom, `xoff+sx`
  right; server-client.c:756-801) → `BORDER`; a border with a pane-border status range
  under it becomes `CONTROL0+n` (server-client.c:1019-1024);
- else `NOWHERE` → the final key is `...Nowhere`-located, which matches no binding.

For classic winmux purposes: **PANE, BORDER, STATUS, STATUS_LEFT, STATUS_RIGHT,
STATUS_DEFAULT, NOWHERE** are the locations that matter.

### 2.3 Click-sequence bookkeeping (server-client.c:1038-1064)

For MOUSEDOWN / SECONDCLICK / TRIPLECLICK:

- A SECONDCLICK/TRIPLECLICK is **downgraded to MOUSEDOWN** ("click sequence reset") if the
  button, the location class, or the pane differ from the stored click state
  (`c->click_button/click_loc/click_wp`).
- For MOUSEDOWN and SECONDCLICK, the full mouse_event is copied to `c->click_event`, click
  state stored, and the **click timer** is (re)armed to `KEYC_CLICK_TIMEOUT` = **300 ms**
  (tmux.h:229). TRIPLECLICK does not re-arm.

### 2.4 The click timer → the delayed DoubleClick (server-client.c:2127-2150)

```c
if (c->flags & CLIENT_TRIPLECLICK) {          /* 2 presses seen, no 3rd */
        event->key = KEYC_DOUBLECLICK;        /* replay saved click_event */
        server_client_handle_key(c, event);
}
c->flags &= ~(CLIENT_DOUBLECLICK|CLIENT_TRIPLECLICK);
```

So the physical press sequence produces:

| physical event | dispatched key | when |
|---|---|---|
| press 1 | `MouseDown1Pane` | immediately |
| release 1 | `MouseUp1Pane` | immediately |
| press 2 (≤300ms) | `SecondClick1Pane` | immediately (unbound by default → forwarded) |
| release 2 | `MouseUp1Pane` | immediately |
| *(no press 3 in 300ms)* | **`DoubleClick1Pane`** | **300 ms after press 2** (timer), with `ignore=1` so it is never re-forwarded to the app |
| press 3 (≤300ms) | `TripleClick1Pane` | immediately |

This is why tmux double-click actions feel slightly delayed, and why a triple click does
*not* also fire the double-click action.

### 2.5 Drag lifecycle — the exact state machine (zero ambiguity)

State lives per client in `struct tty` (tmux.h:1771-1778):

```c
int  mouse_drag_flag;       /* 0 = idle; else MOUSE_BUTTONS(b)+1 of the dragging button */
int  mouse_scrolling_flag;  /* dragging the scrollbar slider */
int  mouse_slider_mpos;     /* grab offset in slider, else -1 */
int  mouse_last_pane;       /* pane id the drag started over, else -1 */
void (*mouse_drag_update)(struct client *, struct mouse_event *);
void (*mouse_drag_release)(struct client *, struct mouse_event *);
```

**START** — a motion-with-button event (`MOUSE_DRAG(m->b)`) arrives while
`mouse_drag_flag == 0`:

1. TYPE = MOUSEDRAG with effective `x=lx, y=ly, b=lb` (server-client.c:857-860): the event
   is classified **at the button-press position with the pressed button**. This is why a
   fast diagonal drag still resizes the border you grabbed: the border hit-test uses where
   you pressed, not where the pointer is now.
2. Location computed normally (pointer pane via `window_get_active_at` at press position).
3. In the MOUSEDRAG block (server-client.c:1104-1130):
   `mouse_drag_flag = MOUSE_BUTTONS(b) + 1`; if `mouse_last_pane` was unset, it is set to
   the pane at `(px,py)`; scrollbar-slider grabs additionally set `mouse_scrolling_flag`
   and `mouse_slider_mpos`.
4. Since `mouse_drag_update` is still NULL, the key is synthesized normally →
   **`MouseDrag1Pane` / `MouseDrag1Border` / `MouseDrag1Status`…** and dispatched through
   the key tables. The bound command (e.g. `resize-pane -M`, or copy-mode's
   `begin-selection`) is what *installs* `mouse_drag_update` / `mouse_drag_release`.

**UPDATE** — subsequent motion events with the button held, `mouse_drag_flag != 0`:

- Same-cell motion is discarded (`x==lx && y==ly` → KEYC_UNKNOWN, server-client.c:854-855).
- If `mouse_drag_update != NULL`: `key = KEYC_DRAGGING` (server-client.c:1104-1106), and in
  the caller (server-client.c:1367-1370):

  ```c
  if ((key & KEYC_MASK_KEY) == KEYC_DRAGGING) {
          c->tty.mouse_drag_update(c, m);
          goto out;
  }
  ```

  **Key bindings are completely bypassed for every motion event during a callback drag.**
- If no callback was installed (binding didn't set one, or key was unbound), every motion
  re-synthesizes `MouseDragN<loc>` and dispatches it through the tables again — this is how
  `send -M` relays drag motion to applications.
- Either way the location logic pins the pane to `mouse_last_pane` (§2.2).

**END** — any event that is *not* MOUSEDRAG / WHEELUP / WHEELDOWN / DOUBLECLICK /
TRIPLECLICK arrives while `mouse_drag_flag != 0` (server-client.c:1068-1090). In practice
this is the button release (MOUSEUP), but a plain MouseDown or MouseMove would also end it:

```c
if (c->tty.mouse_drag_release != NULL)
        c->tty.mouse_drag_release(c, m);
c->tty.mouse_drag_update = NULL;
c->tty.mouse_drag_release = NULL;
c->tty.mouse_scrolling_flag = 0;
type = KEYC_TYPE_MOUSEDRAGEND;      /* re-tag this event */
c->tty.mouse_drag_flag = 0;
c->tty.mouse_slider_mpos = -1;
c->tty.mouse_last_pane = -1;
```

Then the event proceeds to key synthesis as **`MouseDragEndN<loc>`** where N is the button
(for MOUSEUP, `b` was already set to `lb`/`sgr_b`, so the DragEnd carries the button that
was dragging) and `<loc>` is **where the pointer is at release time** (location was
computed before this block, on the pane under the pointer — *not* pinned to the drag pane).
That key is dispatched through the key tables like any other
(`MouseDragEnd1Pane` → `copy-pipe-and-cancel` in the copy-mode tables; unbound for Border).

**RE-ARM** — there is no separate re-arm step. After END, `mouse_drag_flag == 0` and both
callbacks are NULL, so **the very next motion-with-button event is again a fresh START**:
it re-classifies at the new press position, re-dispatches `MouseDrag1Border` (or whatever),
and the binding re-installs the callback. Nothing persists between drags except
`tty->mouse_last_x/y/b` (which is exactly what makes the next start position correct).

> **winmux bug (a) checklist** ("can only drag once"): after release you must (1) clear the
> drag flag, (2) clear both callbacks, (3) clear the remembered drag pane, and (4) still
> synthesize+dispatch `MouseDragEnd1<loc>` for that same release event, and (5) *not*
> consume the following press/motion — the next `MouseDrag1Border` must go through binding
> dispatch again so `resize-pane -M` can reinstall the update callback. Also make sure a
> drag START is classified at `(lx,ly)` with `lb` — if you classify at the current motion
> position, a fast drag "escapes" the border and the second drag appears dead.

Special interactions while a drag is active:

- **Wheel during drag does not end the drag** (excluded from the END condition).
- The **timer-synthesized DoubleClick does not end a drag** either — this protects
  click-click-drag sequences.
- The dedupe rule means a press-release with zero motion in between never produces
  MouseDrag/MouseDragEnd at all; it is Down + Up.

### 2.6 Key synthesis and modifiers (server-client.c:1092-1168)

```c
key = KEYC_MAKE_MOUSE_KEY(type, bn, loc);   /* tmux.h:267-270:
                                               (type<<32) | (button<<8) | location */
if (b & MOUSE_MASK_META)  key |= KEYC_META;
if (b & MOUSE_MASK_CTRL)  key |= KEYC_CTRL;
if (b & MOUSE_MASK_SHIFT) key |= KEYC_SHIFT;
```

Button number `bn` mapping (server-client.c:1134-1154): raw 0/1/2 → 1/2/3, 66/67 → 6/7,
128..131 → 8..11; anything else → 0. Wheel keys use bn=0 (there is no "WheelUp1").

MOUSEMOVE handling (server-client.c:1093-1103): key = `MouseMovePane` etc.; additionally, if
`focus-follows-mouse` is on (session option, default off, options-table.c:854-859), moving
over a non-active pane activates it directly here. MouseMove keys never exit the prefix
table and, if unbound, are forwarded (server-client.c:1523-1529); modes drop them
(window.c:1704-1705).

### 2.7 Synthesized-name matrix (key-string.c:127-191)

Names are `<Type><Button><Location>`:

- Types: `MouseDown`, `MouseUp`, `MouseDrag`, `MouseDragEnd`, `SecondClick`, `DoubleClick`,
  `TripleClick` (with button 1,2,3,6..11), `WheelUp`, `WheelDown` (no button), `MouseMove`
  (no button).
- Locations: `Pane`, `Status`, `StatusLeft`, `StatusRight`, `StatusDefault`, `Border`,
  `ScrollbarUp`, `ScrollbarSlider`, `ScrollbarDown`, `Control0`..`Control9`.
- Examples: `MouseDown1Pane`, `MouseDrag1Border`, `MouseDragEnd1Pane`, `WheelUpStatus`,
  `TripleClick1Pane`, `C-MouseDown1Status` (modifier prefixes like keyboard keys).

---

## 3. Dispatch of the synthesized key (server-client.c:1351-1572)

Order in `server_client_key_callback`:

1. `m->valid = 1; m->key = key;` `KEYC_DRAGGING` short-circuit (§2.5).
2. Target resolution: `cmd_find_from_mouse(&fs, m)` — mouse keys execute against the pane
   in `m->wp` (this is what target `-t=` means in mouse bindings).
3. **`mouse` option off → skip bindings entirely and forward** the raw event to the pane
   app (server-client.c:1380-1381). ("Applications inside panes can use the mouse even when
   'off'" — options-table.c:961.)
4. Key table selection (server-client.c:1399-1406): if the client is in the default table
   and the target pane is in a mode with a key table (`copy-mode` / `copy-mode-vi` from
   `mode-keys`, window-copy.c:1230-1237), use the mode table; otherwise the client table.
   So *the same* `MouseDrag1Pane` key means "start copy-mode" at root but "begin selection"
   inside copy mode.
5. Unbound in mode table → retry root table; unbound everywhere → `forward_key`:
   `window_pane_key` (window.c:1690-1724) → `input_key_pane` → `input_key_mouse`
   (input-keys.c:797-815) → `input_key_get_mouse` (input-keys.c:713-793).

### 3.1 Forwarding rules — when the app actually receives the event

`input_key_get_mouse` (encode for the pane's screen mode):

- Pure **motion** (`MOUSE_DRAG(b)` set) needs the pane to have `MODE_MOUSE_BUTTON` (1002)
  or `MODE_MOUSE_ALL` (1003) (`MOTION_MOUSE_MODES`, tmux.h:693); no-button motion
  additionally requires `MODE_MOUSE_ALL` (input-keys.c:734-745).
- Anything else needs any of `MODE_MOUSE_STANDARD|BUTTON|ALL` (`ALL_MOUSE_MODES`).
- Encoding: SGR out only if the app requested SGR **and** the event arrived as SGR
  (a legacy release can't be converted to SGR because the released button is unknown —
  input-keys.c:747-755); else UTF-8 (1005) if requested and in range; else legacy X10 with
  coordinate clamping at 0xFF.
- Coordinates are rebased to the pane (`cmd_mouse_at`-style `x - wp->xoff`).
- `m->ignore` (the synthetic DoubleClick) is never forwarded (input-keys.c:805).

`send -M` (`cmd-send-keys.c:211-219`) does the same thing from a binding: resolve
`cmd_mouse_pane(m)` and call `window_pane_key(wp, ..., m->key, m)` — i.e. "give this mouse
event to the application in the pane under the mouse".

---

## 4. Border drag resize — the full path (winmux bug (a))

Default bindings (key-bindings.c:517-522):

```
bind -n MouseDown1Border { select-pane -M }        # clears the marked pane
bind -n MouseDrag1Border { resize-pane -M }
bind -n M-MouseDrag1Border { move-pane -M }        # floating panes; newer master
```

Sequence for one drag:

1. Press on a border: `MouseDown1Border` → `select-pane -M` (clear marked; cosmetic).
2. First motion with button held: START (§2.5) → key `MouseDrag1Border` (classified at
   press position) → binding runs `resize-pane -M` → `cmd_resize_pane_mouse_update`
   (cmd-resize-pane.c:191-220):

   ```c
   if (!event->m.valid) return;               /* -M from keyboard: no-op */
   wp = cmd_mouse_pane(&event->m, &s, NULL);
   ...
   c->tty.mouse_drag_update = cmd_resize_pane_mouse_resize_tiled;
   cmd_resize_pane_mouse_resize_tiled(c, &event->m);   /* first resize NOW */
   ```

   Note: **no `mouse_drag_release` is installed** for border resize — release cleanup is
   purely the generic §2.5 END block, and `MouseDragEnd1Border` is unbound (no-op).
3. Every further motion: `KEYC_DRAGGING` → `cmd_resize_pane_mouse_resize_tiled(c, m)`
   directly, live-resizing on each event.
4. Release: generic END; callbacks cleared; `MouseDragEnd1Border` dispatched (unbound);
   state fully idle → next border drag starts from scratch.

### 4.1 `cmd_resize_pane_mouse_resize_tiled` (cmd-resize-pane.c:354-418)

Works on **event deltas**, not absolute positions:

```c
y  = m->y + m->oy;   x = m->x + m->ox;      /* current, adjusted for status line */
ly = m->ly + m->oy; lx = m->lx + m->ox;     /* previous event                    */
static const int offsets[][2] = { {0,0},{0,1},{1,0},{0,-1},{-1,0} };
/* probe the PREVIOUS position and its 4 neighbours for a layout border */
for each offset: lc = layout_search_by_border(w->layout_root, lx+dx, ly+dy);  /* dedup */
for each found cell:
        if (y != ly && cell->parent->type == LAYOUT_TOPBOTTOM)
                layout_resize_layout(w, cell, LAYOUT_TOPBOTTOM, y - ly, 0);
        else if (x != lx && cell->parent->type == LAYOUT_LEFTRIGHT)
                layout_resize_layout(w, cell, LAYOUT_LEFTRIGHT, x - lx, 0);
if (resizes) server_redraw_window(w);
```

- `layout_search_by_border` (layout.c:159-196): recursive descent; if the point is inside a
  child cell, recurse; if it falls in the **gap between two siblings** (the border), return
  the earlier sibling (the cell whose right/bottom edge is being dragged).
- The **5-point probe** (point + up/down/left/right neighbours) is what makes an
  **intersection of borders resize both axes at once**: probing around a corner point finds
  both the LEFTRIGHT cell and the TOPBOTTOM cell, and each is resized along its own axis by
  that axis' delta. It also adds one cell of grab tolerance.
- Deltas are previous→current event (`x-lx`, `y-ly`), so resize tracks pointer velocity
  exactly and is idempotent for duplicate positions (which were already deduped anyway).
- `layout_resize_layout` (layout.c:936-961) grows/shrinks the cell by repeatedly stealing
  from/giving to following siblings (`layout_resize_pane_grow/shrink`), then
  `layout_fix_offsets` + `layout_fix_panes` + `window-layout-changed` notify.
- If the mouse leaves all borders mid-drag, probes find nothing → `ncells == 0` → nothing
  happens for that event, but the callback stays installed; moving back near the border
  resumes resizing (because deltas are computed per event pair, no drift).
- If the window disappears, the callback self-uninstalls
  (`c->tty.mouse_drag_update = NULL`, cmd-resize-pane.c:369).

(Floating panes, newer master: `cmd_resize_pane_mouse_resize_move_floating`
cmd-resize-pane.c:229-352 — grab corners/edges to resize, grab the top border to move.)

---

## 5. Copy-mode mouse — selection, drag, autoscroll (winmux bug (b))

### 5.1 Entering: what the root bindings do

```
bind -n MouseDrag1Pane   { if -F '#{||:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -M } }
bind -n WheelUpPane      { if -F '#{||:#{alternate_on},#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -e } }
bind -n DoubleClick1Pane { select-pane -t=; if -F '#{...same...}' { send -M } { copy-mode -H; send -X select-word; run -d0.3; send -X copy-pipe-and-cancel } }
bind -n TripleClick1Pane { ...same with select-line... }
```

(key-bindings.c:502, 506, 512, 515. In tmux ≤3.5 the double/triple bindings used
`copy-mode -M` and no `run -d0.3`/`-H`; the `-H`+delayed-copy form is newer master: it
briefly *shows* the word/line selection for 0.3 s, then copies and cancels.)

Format guards: `pane_in_mode` = pane already has a mode open; `mouse_any_flag` = the pane
app has enabled any mouse tracking (format.c:1935-1945: `wp->base.mode & ALL_MOUSE_MODES`);
`alternate_on` = pane is on the alternate screen (format.c:1411-1418). So: app owns the
mouse → relay; otherwise a left-drag on a live pane enters copy mode *and starts selecting
immediately*.

`copy-mode` flags (cmd-copy-mode.c:56-111):

- **`-M`**: resolve the pane from the mouse event (not from `-t`), require the client's
  session to match, and — after the mode is (already or newly) open — call
  **`window_copy_start_drag(c, &event->m)`** (cmd-copy-mode.c:95-96). That is the *only*
  thing `-M` adds; it does **not** set scroll-exit.
- **`-e`**: sets `data->scroll_exit = 1` (window-copy.c:615) — copy mode **auto-cancels when
  scrolled back to the very bottom** (`oy == 0` after a scroll-down/page-down;
  window-copy.c:2355, 975, 1750...). This is why wheel-up-then-wheel-down on a pane drops
  you cleanly back out of copy mode.
- **`-H`**: hides the top-right `[NN/MM]` position indicator (window-copy.c:616, 5211).
- **`-u`** page up on entry; **`-d`** page down (with `-e` semantics); **`-q`** exit
  copy mode; **`-S`** scrollbar-slider drag entry (newer master).
- Entering via any mouse key disables the (newer master) line-numbers feature
  (cmd-copy-mode.c:90-92).
- Entering by wheel does **not** itself scroll — the first notch only enters copy mode; the
  next wheel events hit the copy-mode table.

### 5.2 The copy-mode/copy-mode-vi mouse bindings (identical in both tables)

```
bind -Tcopy-mode    MouseDown1Pane    select-pane
bind -Tcopy-mode    MouseDrag1Pane    { select-pane; send -X begin-selection }
bind -Tcopy-mode    MouseDragEnd1Pane { send -X copy-pipe-and-cancel }
bind -Tcopy-mode    WheelUpPane       { select-pane; send -N5 -X scroll-up }
bind -Tcopy-mode    WheelDownPane     { select-pane; send -N5 -X scroll-down }
bind -Tcopy-mode    DoubleClick1Pane  { select-pane; send -X select-word; run -d0.3; send -X copy-pipe-and-cancel }
bind -Tcopy-mode    TripleClick1Pane  { select-pane; send -X select-line; run -d0.3; send -X copy-pipe-and-cancel }
```

(key-bindings.c:592-598 and 707-713; **emacs and vi are byte-identical for mouse** — mouse
release behaves the same in both: copy the selection and leave copy mode.)

- **Click without drag in copy mode**: `MouseDown1Pane` → `select-pane` only. The copy-mode
  **cursor does not move** and the selection is not cleared. (The corresponding
  `MouseUp1Pane` is unbound → no-op.)
- **Wheel in copy mode**: 5 lines per notch (`-N5` = command repeat count → `wme->prefix`,
  each iteration `window_copy_cursor_up/down(wme, 1)`; window-copy.c:2375-2383,
  2346-2358). With `scroll_exit` set (entered via `copy-mode -e`), a scroll-down that
  reaches `oy == 0` cancels the mode.
- **DragEnd** → `copy-pipe-and-cancel`: pipes/copies the selection into a new automatic
  paste buffer (and `copy-command` / OSC-52 clipboard as configured) and exits copy mode.
  If there is no selection, `window_copy_get_selection` fails and it just cancels.

### 5.3 `begin-selection` with a mouse event → `window_copy_start_drag`

`window_copy_cmd_begin_selection` (window-copy.c:1299-1315): if invoked with a valid mouse
event, it does **not** do the keyboard path — it calls `window_copy_start_drag(c, m)`
(window-copy.c:6714-6766):

```c
wp = cmd_mouse_pane(m, NULL, NULL);            /* the pane in m->wp            */
if pane not in copy/view mode: return;
if (cmd_mouse_at(wp, m, &x, &y, 1) != 0)       /* PRESS position (last=1: lx,ly)*/
        return;
c->tty.mouse_drag_update  = window_copy_drag_update;
c->tty.mouse_drag_release = window_copy_drag_release;

yg = screen_hsize(backing) + y - data->oy;     /* absolute grid line of press   */
if (x < data->selrx || x > data->endselrx || yg != data->selry)
        data->selflag = SEL_CHAR;              /* not on the word/line anchor →
                                                  plain char selection          */
switch (data->selflag) {
case SEL_WORD: /* extend an existing word anchor: snap cursor to word start */ ...
case SEL_LINE: window_copy_update_cursor(wme, 0, y); break;
case SEL_CHAR: window_copy_update_cursor(wme, x, y);
               window_copy_start_selection(wme); break;
}
window_copy_redraw_screen(wme);
window_copy_drag_update(c, m);                 /* process this event's position */
```

Key facts:

- The **anchor is the press position** (`cmd_mouse_at(..., 1)` reads `m->lx/m->ly`), so no
  motion is lost even though the first event you see is already a drag.
- `window_copy_start_selection` (window-copy.c:5474-5487) sets
  `selx/sely = endselx/endsely = cursor` (absolute grid coords, `sely = hsize + cy - oy`)
  and `cursordrag = CURSORDRAG_ENDSEL` — the cursor drags the selection *end*.
- The `SEL_WORD`/`SEL_LINE` branches implement **double-click-then-drag** and
  **triple-click-then-drag**: `select-word`/`select-line` (fired by DoubleClick/TripleClick)
  store the word/line anchor in `selrx/selry/endselrx/endselry` and set
  `selflag` (window-copy.c:2419-2495); if the subsequent drag starts on that same anchor,
  the selection extends by whole words/lines (see §5.5); if it starts elsewhere, it
  degrades to `SEL_CHAR`.

### 5.4 `window_copy_drag_update` (window-copy.c:6768-6812) — cursor follows pointer + autoscroll

```c
wp = cmd_mouse_pane(m, ...);                    /* m->wp is pinned to drag pane */
if pane no longer in copy/view mode: return;
evtimer_del(&data->dragtimer);                  /* any motion resets the timer  */
if (cmd_mouse_at(wp, m, &x, &y, 0) != 0)        /* pointer outside pane → no-op */
        return;
x = window_copy_cursor_unoffset(...);           /* line-number gutter, newer    */
old_cx = data->cx; old_cy = data->cy;
window_copy_update_cursor(wme, x, y);           /* clamps x to line length      */
if (window_copy_update_selection(wme, 1, 0))
        window_copy_redraw_selection(wme, old_cy);
if (old_cy != data->cy || old_cx == data->cx) {
        if (y == 0)                              { evtimer_add(&data->dragtimer, 50ms);
                                                   window_copy_cursor_up(wme, 1); }
        else if (y == screen_size_y - 1)         { evtimer_add(&data->dragtimer, 50ms);
                                                   window_copy_cursor_down(wme, 1); }
}
```

- **Cursor-follows-pointer math**: pane-relative `(x, y)`; `window_copy_update_cursor`
  (window-copy.c:5412-5471) clamps `x` to the line's content length (unless rectangle
  selection — rect may extend past EOL like vi virtualedit).
- **Autoscroll**: when the pointer reaches the pane's **top row (y==0)** or **bottom row**,
  each update scrolls one extra line *and* arms `dragtimer` for
  `WINDOW_COPY_DRAG_REPEAT_TIME` = **50 ms** (window-copy.c:351). The timer callback
  (window-copy.c:358-380) re-checks: if the cursor is still on the edge row, scroll one
  line and re-arm; so holding the pointer on the edge row scrolls **20 lines/sec** while
  the selection keeps extending (selection end is synchronized because it's stored in
  absolute grid coordinates and clipped to the visible screen for display,
  `window_copy_adjust_selection` window-copy.c:5489-5520).
- Pointer events **outside** the pane rectangle delete the timer and change nothing
  (`cmd_mouse_at` fails; cmd.c:764-791 — bounds test against `xoff/yoff/sx/sy`). tmux's
  autoscroll therefore only runs while the pointer is on the pane's first/last row (or
  parked there).
- The condition `old_cy != data->cy || old_cx == data->cx` suppresses re-scrolling on pure
  horizontal motion along the edge row.

### 5.5 Selection extension semantics (`window_copy_synchronize_cursor`, window-copy.c:5325-5410)

Every `window_copy_update_selection` first synchronizes the "moving end" with the cursor:

- `CURSORDRAG_ENDSEL` (normal drag): cursor writes `endselx/endsely`; `CURSORDRAG_SEL`
  (after `other-end`): cursor writes `selx/sely`.
- `SEL_CHAR`: the moving end is exactly the cursor cell.
- `SEL_WORD`: dragging **left/up of the anchor word** snaps the moving end to the start of
  the word under the cursor and restores the fixed end to the anchor word's end; dragging
  **right/down** snaps to the end of the word under the cursor and restores the fixed end
  to the anchor word's start (word boundaries from the `word-separators` option). I.e. the
  selection is always a whole number of words including the anchor word.
- `SEL_LINE`: same, with whole lines (left/up: `xx=0`; right/down:
  `xx = line length`, and `yy` at least the anchor's last line).
- The selection endpoints are stored in **absolute grid** coords (history included), then
  clipped per redraw to the visible region; if both ends are off-screen on the same side,
  the highlight is hidden but the selection persists (window-copy.c:5559-5564).

### 5.6 `window_copy_drag_release` (window-copy.c:6814-6837)

Just housekeeping: `evtimer_del(&data->dragtimer)` (kills autoscroll) — the actual copy is
done by the `MouseDragEnd1Pane` **binding** that the same release event dispatches
immediately afterwards (§2.5 END: release callback fires first, then the DragEnd key is
synthesized and dispatched → `copy-pipe-and-cancel`).

Release-position subtlety: the DragEnd key's location/`m->wp` are the pane **under the
pointer at release**. Releasing over a *different* pane makes `MouseDragEnd1Pane` resolve
to that other pane, whose key table is root (not in copy mode) → the DragEnd is unbound →
no copy happens, and the origin pane keeps its selection and stays in copy mode. Matching
tmux exactly means reproducing even this: **a drag that ends outside the pane does not
copy** (the user must release inside the pane).

### 5.7 Double/triple click — inside vs outside copy mode

- **Not in a mode** (root bindings, §5.1): enter copy mode (`-H`, hidden indicator), run
  `select-word` / `select-line`, wait 0.3 s (`run -d0.3`), then `copy-pipe-and-cancel` —
  net effect: flash the selection, copy it, back to normal. (tmux ≤3.5: `copy-mode -M;
  send -X select-word; send -X copy-pipe-and-cancel` with no delay.)
- **In copy mode**: `select-word`/`select-line` then delayed `copy-pipe-and-cancel` — same
  flash-copy-exit, from wherever you were scrolled.
- If the user starts dragging after the double/triple click before the delayed copy runs,
  the drag extends word/line-wise (§5.3 `selflag` mechanics) and the DragEnd does the copy.

---

## 6. Wheel — every context

| context | behavior | source |
|---|---|---|
| pane, no mode, app has no mouse, main screen | `WheelUpPane` → `copy-mode -e` (enter, scroll-exit armed; first notch does not scroll). `WheelDownPane` unbound at root → forwarded → dropped unless app enabled mouse. | key-bindings.c:506 |
| pane, app enabled any mouse tracking (`mouse_any_flag`) | `send -M`: the raw wheel event is re-encoded and written to the app. | key-bindings.c:506; input-keys.c:713 |
| pane on **alternate screen**, mouse not enabled by app (`alternate_on` guard) | `send -M` → `input_key_get_mouse` returns 0 → **wheel is swallowed**. tmux does **not** translate wheel to arrow keys anywhere in this tree — full-screen apps are expected to enable mouse themselves. Copy mode is *not* entered. | key-bindings.c:506; input-keys.c:722-726 |
| copy mode | 5 lines per notch (`send -N5 -X scroll-up/down`); wheel-down at bottom cancels iff entered with `-e`. | key-bindings.c:595-596 |
| status line | `WheelUpStatus` → `previous-window`; `WheelDownStatus` → `next-window`. | key-bindings.c:534-537 |
| during a drag | wheel does not terminate the drag (§2.5). | server-client.c:1069-1073 |
| scroll amount elsewhere | there is no global "wheel = N lines" option; each binding decides (`-N5` in copy mode; 1 event = 1 binding run). | — |

Wheel events have no Up/Release counterpart (a notch is a single press event; SGR wheel
releases are discarded at decode, tty-keys.c:1303).

---

## 7. Click semantics — every default mouse binding

### 7.1 Root table (key-bindings.c:497-554)

| key | command | effect |
|---|---|---|
| `MouseDown1Pane` | `select-pane -t=; send -M` | focus the pane under the mouse, then forward the press to the app (only actually written if the app enabled mouse; the matching MouseUp1Pane is unbound → forwarded too, so apps see press+release pairs) |
| `C-MouseDown1Pane` | `swap-pane -s@` | (newer master) swap marked pane here |
| `MouseDrag1Pane` | `if -F '#{\|\|:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -M }` | app owns mouse / pane in a mode → relay motion; else enter copy mode dragging (§5) |
| `M-MouseDrag1Pane` | `move-pane -M` | (newer master, floating panes) |
| `WheelUpPane` | `if -F '#{\|\|:#{alternate_on},#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -e }` | §6 |
| `MouseDown2Pane` | `select-pane -t=; if -F '#{\|\|:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { paste -p }` | middle-click paste (newest buffer, `-p` bracketed-paste aware) |
| `DoubleClick1Pane` | `select-pane -t=; if guard { send -M } { copy-mode -H; send -X select-word; run -d0.3; send -X copy-pipe-and-cancel }` | copy word under pointer (≤3.5: `copy-mode -M` + immediate copy) |
| `TripleClick1Pane` | same with `select-line` | copy line under pointer |
| `MouseDown1Border` | `select-pane -M` | clear the marked pane |
| `MouseDrag1Border` | `resize-pane -M` | live border resize (§4) |
| `M-MouseDrag1Border` | `move-pane -M` | (newer master) |
| `MouseDown1Status` | `switch-client -t=` | select the clicked window tab (target from the clicked `range=window\|N`) |
| `C-MouseDown1Status` | `swap-window -t@` | (newer master) |
| `WheelDownStatus` / `WheelUpStatus` | `next-window` / `previous-window` | |
| `MouseDown3StatusLeft` (+`M-` variant) | `display-menu -t= -xM -yW -T '#[align=centre]#{session_name}' <SESSION MENU>` | session context menu |
| `MouseDown3Status` (+`M-` variant) | `display-menu -t= -xW -yW -T '#{window_index}:#{window_name}' <WINDOW MENU>` | window context menu |
| `MouseDown3Pane` | `if -Ft= '#{\|\|:#{mouse_any_flag},#{&&:#{pane_in_mode},#{?#{m/r:(copy\|view)-mode,#{pane_mode}},0,1}}}' { select-pane -t=; send -M } { display-menu ... <PANE MENU> }` | right-click: relay to app if it owns the mouse (or a non-copy mode is open), else pane context menu |
| `M-MouseDown3Pane` | `display-menu ... <PANE MENU>` | menu even when app owns mouse |
| `MouseDown1Control7/8/9`, scrollbar bindings | newer master (pane-border controls: 9=kill w/ confirm menu, 8=zoom toggle, 7=float/tile; scrollbar page/track) | key-bindings.c:529-531, 551-554 |

Menu contents (key-bindings.c:27-100, abridged): **session menu** — switch-to entries for
up to 5 other sessions, Renumber, Rename, Detach, New Session, New Window; **window menu**
— Swap Left/Right, Swap Marked, Kill, Respawn, Mark/Unmark, Rename, New After, New At End;
**pane menu** — Go To Top/Bottom (in copy mode), Paste, Search/Type/Copy `#{mouse_word}`,
Copy `#{mouse_line}`, hyperlink entries, splits, Swap Up/Down, Kill, Respawn, Mark, Zoom.

### 7.2 Prefix table

**There are no default mouse bindings in the prefix table.** A mouse press while prefix is
active finds no binding, falls back to root (server-client.c:1536-1546), then (having
switched tables) is dropped rather than forwarded (server-client.c:1552-1556) — except
MouseMove keys, which are forwarded without leaving the prefix table
(server-client.c:1523-1529).

### 7.3 copy-mode and copy-mode-vi tables

See §5.2 — seven bindings each, byte-identical between emacs and vi:
`MouseDown1Pane`=select-pane, `MouseDrag1Pane`=select-pane+begin-selection,
`MouseDragEnd1Pane`=copy-pipe-and-cancel, `WheelUp/DownPane`=±5 lines,
`DoubleClick1Pane`=select-word+delayed copy-cancel,
`TripleClick1Pane`=select-line+delayed copy-cancel.

### 7.4 Passthrough decision summary ("who consumes a click?")

1. `mouse` option **off**: tmux never interprets; everything is forwarded to the app (which
   sees it only if it enabled mouse tracking itself; tmux still relays because the *app's*
   request turned on outer tracking via the pane screen mode).
2. `mouse` on, binding exists: binding runs. Bindings use format guards
   (`mouse_any_flag`/`pane_in_mode`/`alternate_on`) + `send -M` to hand precedence back to
   apps that requested the mouse. tmux **always keeps**: border drags, status-line clicks,
   Ctrl/Meta-modified defaults, and the initial `select-pane` focus click (the click both
   focuses *and* is forwarded).
3. `mouse` on, no binding: forwarded to the pane app (subject to §3.1 mode checks).

---

## 8. Options and constants

| name | kind | default | role |
|---|---|---|---|
| `mouse` | session flag | off (options-table.c:955-961) | master switch for tmux mouse handling; off = pure passthrough |
| `focus-follows-mouse` | session flag | off (options-table.c:854) | pane activation on MouseMove (needs outer 1003) |
| `word-separators` | session string | `!\"#$%&'()*+,-./:;<=>?@[\\]^`{\|}~` + space (tmux default) | word boundaries for select-word / double-click |
| `mode-keys` | window choice | emacs | selects `copy-mode` vs `copy-mode-vi` table (mouse defaults identical) |
| `copy-mode-selection-style`, `mode-style` | styles | | selection highlight (window-copy.c:5568) |
| `KEYC_CLICK_TIMEOUT` | compile-time | **300 ms** (tmux.h:229) | double/triple click window and DoubleClick synthesis delay; 0 disables the timer path |
| `WINDOW_COPY_DRAG_REPEAT_TIME` | compile-time | **50 000 µs** (window-copy.c:351) | copy-mode drag autoscroll repeat (20 rows/s) |
| `status-format[0]` ranges | | `range=left`, `range=window\|N`, `range=right` | status click hit-testing (§2.2) |

Bracketed paste does not interact with mouse: `KEYC_IS_PASTE` covers paste-start/end
function keys only; mouse keys are classified before the paste checks and
`server_client_is_bracket_paste` only reroutes non-mouse keys between the markers.

---

## 9. Windows / winmux applicability notes

- **Input format**: Windows Terminal, and conhost with `ENABLE_VIRTUAL_TERMINAL_INPUT`,
  deliver exactly the SGR-1006 sequences documented in §1 once the client emits
  `CSI ?1000h ?1002h ?1006h` (winmux already decodes SGR in `src/keys.rs`). Implement the
  legacy `ESC [ M` path only for exotic terminals; SGR is sufficient for ConPTY targets.
  Remember the PuTTY wheel-release discard and the `'m'`→`b=3` rewrite.
- **Emit `?1002h` (button-event tracking), not just `?1000h`** — without it the terminal
  sends no motion events while a button is held and drags can never update. Match tmux's
  policy: `?1002h` normally, upgrade to `?1003h` only for focus-follows-mouse or when a
  pane app requests 1003. Always pair with `?1006h`. And per winmux's own invariant, send
  the corresponding `l` sequences on every exit path.
- **Bug (a) — border drag works once**: implement §2.5 verbatim. The three essentials tmux
  relies on: (1) classification of a drag START uses the *previous* event
  (`lx,ly,lb` = press position/button); (2) the END path (triggered by the release event,
  i.e. SGR `'m'`, which decodes to `b=3` → MOUSEUP) must clear flag+callbacks+last-pane
  *and* still dispatch `MouseDragEnd1<loc>`; (3) nothing else persists — the next
  motion-with-button re-runs the `MouseDrag1Border` binding from scratch. If winmux keeps a
  "dragging" flag that is only cleared when the release lands on a border, or never
  re-dispatches the border-drag command after the first drag, this reproduces the reported
  symptom.
- **Bug (b) — selection feel**: the tmux feel decomposes into: anchor at press position
  (not first-motion position); cursor clamped to line length (no selecting phantom cells
  past EOL unless rect); selection endpoints in absolute history coordinates so scrolling
  during a drag never warps the anchor; 50 ms edge autoscroll while the pointer sits on the
  first/last pane row; motion outside the pane is a no-op (selection freezes, timer
  stops); release inside the pane = copy+exit via the `MouseDragEnd1Pane` binding; plain
  click = focus only, never moves the copy cursor; double/triple-click drags extend by
  whole words/lines via the `selflag` anchor logic.
- **Timing**: 300 ms click timeout, DoubleClick delivered 300 ms *after* the second press
  (via timer) — if winmux currently fires its double-click action on the second press it
  will feel different from tmux (and will also wrongly fire on triple clicks).
- **Coordinate spaces**: tmux has three — client (`m->x/y`), window
  (`+ox/oy`, minus status rows), pane (`- xoff/yoff`). winmux's equivalents must apply the
  status-bar offset exactly like server-client.c:984-990 (status at top vs bottom), or
  border hit-tests will be off by one row, which also presents as "drag doesn't work".
- **Per-client drag state**: `mouse_drag_flag/_update/_release/mouse_last_pane` are all
  per-client (`struct tty` inside `struct client`) — two attached clients can drag
  independently. winmux's per-client `InputMachine` is the right home.
- **No wheel→arrow translation**: modern tmux never converts wheel to arrow keys for
  alternate-screen apps (§6). If `less`/`vim` in winmux should scroll on wheel, the correct
  tmux-parity behavior is: relay the SGR wheel bytes if the app enabled mouse mode,
  otherwise swallow (alternate screen) or enter copy mode (main screen).
