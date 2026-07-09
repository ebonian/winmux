# tmux reference: status line, messages, and the command prompt UI

**Source studied:** tmux master `db115c6b97b06103a76934fb0e4abf588a6ae318` (2026-07-07).
All file:line references are into that tree.

**Scope:** the status line (layout, window list, styles, multi-line), status messages
(`display-message`, visual-* alerts), the status-line command prompt (line editor, history,
completion), clock mode, status-line mouse ranges, and the redraw discipline. This is the
authoritative behavioral spec for winmux; implementers should not need to reopen tmux source
for anything in this domain.

**Note on this tmux version:** this master has two structural changes vs. tmux ≤ 3.5 that do
not change user-visible behavior but move code around: (1) the prompt line editor was factored
out of `status.c` into `prompt.c` + `prompt-history.c` (prompts can now also be attached to a
*pane*, `PROMPT_ISPANE`, drawn over the pane's bottom row — `screen-redraw.c:1524-1578`); and
(2) default colours use theme palette names (`themegreen`, `themeyellow`, `themeblack`,
`themeblue`, `themered`) that resolve through the `theme` option's light/dark detection
(`colour.c:35-70`). In classic tmux these were plain `green`/`yellow`/`black`/`blue`/`red`;
for winmux purposes treat `themegreen` = `green` etc.

---

## 1. Status line anatomy and layout

### 1.1 Number of lines, position, and the cache

- `status` is a **choice** option: `off | on | 2 | 3 | 4 | 5` (`options-table.c:41-43`),
  default `on` (= 1 line). `STATUS_LINES_LIMIT` is 5 (`tmux.h:2010`).
- `status_update_cache()` (`status.c:86-96`) caches per session: `statuslines` = the choice
  value, and `statusat` = `-1` (off), `0` (`status-position top`), or `1` (bottom).
- `status_line_size(c)` (`status.c:112-122`) → 0 if the client has `CLIENT_STATUSOFF` or is a
  control client, else the session's `statuslines` (or the global `status` option if no
  session).
- `status_at_line(c)` (`status.c:99-109`) → the first terminal row of the status area:
  `0` for top, `c->tty.sy - status_line_size(c)` for bottom, `-1` if off.
- The status is drawn to the tty in `redraw_draw()` (`screen-redraw.c:1688-1699`): each of
  `lines` rows of `c->status.active` is copied to terminal row `y + i` where `y = 0` (top) or
  `y = tty.sy - lines` (bottom). **When a message or prompt is showing and `status` is off,
  `lines` is forced to 1** — the message borrows the bottom (or top) terminal row:

  ```c
  lines = dctx.status_lines;
  if (c->message_string != NULL || c->prompt != NULL)
          lines = (lines == 0 ? 1 : lines);
  ```

### 1.2 status-format: how the classic line is built

Every status line *i* is drawn by expanding `status-format[i]` with
`format_expand_time()` and painting it with `format_draw()` (`status.c:269-305`). If
`status-format` is unset the whole area is filled with spaces in the base style; if line *i*
has no array entry, that line is blank spaces (`status.c:274-281`).

The default `status-format` array has **three** entries (`options-table.c:234-239`):
index 0 is the classic line, index 1 a pane list line, index 2 a session list line (indices
1-2 only matter when `status` ≥ 2). The classic line (index 0,
`options-table.c:119-183`) is, verbatim (whitespace added):

```
#[align=left range=left #{E:status-left-style}]
#[push-default]
#{T;=/#{status-left-length}:status-left}
#[pop-default]
#[norange default]
#[list=on align=#{status-justify}]
#[list=left-marker]<#[list=right-marker]>#[list=on]
#{W:
    #[range=window|#{window_index} #{E:window-status-style}
      #{?#{&&:#{window_last_flag},#{!=:#{E:window-status-last-style},default}}, #{E:window-status-last-style},}
      #{?#{&&:#{window_bell_flag},#{!=:#{E:window-status-bell-style},default}}, #{E:window-status-bell-style},
         #{?#{&&:#{||:#{window_activity_flag},#{window_silence_flag}},#{!=:#{E:window-status-activity-style},default}}, #{E:window-status-activity-style},}}
    ]
    #[push-default]
    #{T:window-status-format}
    #[pop-default]
    #[norange default]
    #{?loop_last_flag,,#{E:window-status-separator}}
,
    #[range=window|#{window_index} list=focus
      #{?#{!=:#{E:window-status-current-style},default},#{E:window-status-current-style},#{E:window-status-style}}
      #{?#{&&:#{window_last_flag},#{!=:#{E:window-status-last-style},default}}, #{E:window-status-last-style},}
      #{?#{&&:#{window_bell_flag},#{!=:#{E:window-status-bell-style},default}}, #{E:window-status-bell-style},
         #{?#{&&:#{||:#{window_activity_flag},#{window_silence_flag}},#{!=:#{E:window-status-activity-style},default}}, #{E:window-status-activity-style},}}
    ]
    #[push-default]
    #{T:window-status-current-format}
    #[pop-default]
    #[norange list=on default]
    #{?loop_last_flag,,#{E:window-status-separator}}
}
#[nolist align=right range=right #{E:status-right-style}]
#[push-default]
#{T;=/#{status-right-length}:status-right}
#[pop-default]
#[norange default]
```

Everything about status-left/right/window-list behavior *follows from this format string plus
the `format_draw()` engine*. Key format modifiers used:

- `#{E:opt}` — fetch option `opt`'s value and expand it as a format (no strftime)
  (`format.c:5705-5706`, applied at `format.c:6126-6129`).
- `#{T:opt}` — same but strftime is applied too (`format.c:5708-5710`, `6130-6135`).
  **This is why `%Y-%m-%d %H:%M` works inside `status-right`, `status-left`,
  `window-status-format`.**
- `#{T;=/N:opt}` — modifiers chained with `;`. `=` with argument `N` (arg delimiter here is
  `/`) truncates the *expanded* value to width `N` using `format_trim_left`
  (`format.c:5619-5628`, applied `format.c:6149-6160`). A second `=` argument would be a
  truncation marker; the default status line passes none, so **status-left/right are cut
  silently, with no `>` marker**. A limit of `0` means *no truncation* (`if (limit > 0)`),
  and a negative limit trims from the right (keeps the rightmost cells, marker prepended).
- `#{W:fmt}` / `#{W:fmt,current-fmt}` — loop over windows (`format.c:4958-5036`): windows in
  index order; for each window a per-window format tree is built and `fmt` (or `current-fmt`
  for the session's current window) expanded. Loop variables: `loop_index`,
  `loop_last_flag` (1 on last window — used to suppress the trailing separator),
  `window_after_active`, `window_before_active`, plus `next_*`/`prev_*` neighbour data.

### 1.3 Expansion order: strftime vs. #{}

`format_expand_time()` sets `FORMAT_EXPAND_TIME`; in `format_expand1()`
(`format.c:6251-6264`) **strftime runs first over the whole template string** (only if it
contains a `%`), then `#{}`/`#()`/`#X` tokens are expanded:

```c
if ((es->flags & FORMAT_EXPAND_TIME) && strchr(fmt, '%') != NULL) {
        ...
        format_strftime(expanded, sizeof expanded, fmt, &es->tm);
        fmt = expanded;
}
```

This applies at *every* level that uses expand-time: the `status-format` string itself, and
each `#{T:...}` option value (status-left, status-right, window-status-format,
window-status-current-format, message-format, display-message templates). Consequence: a
literal `%` in those strings must be written `%%`. `#{E:...}` values (all the *-style options,
window-status-separator) get **no** strftime.

Single-character aliases (`format.c:213-241`, dispatched at `format.c:6366-6387`):
`#D`=pane_id, `#F`=window_flags, `#H`=host, `#I`=window_index, `#P`=pane_index,
`#S`=session_name, `#T`=pane_title, `#W`=window_name, `#h`=host_short. `##` is a literal `#`;
`#[` begins an inline style; inside a `#[...]` block alias expansion is suppressed
(`fmt > style_end` check, `format.c:6368`).

### 1.4 The format_draw engine: sections, justify math, truncation

`format_draw()` (`format-draw.c:689-1096`) parses the expanded string into **eight interim
screens**: `LEFT`, `CENTRE`, `RIGHT`, `ABSOLUTE_CENTRE`, `LIST`, `LIST_LEFT` (left marker),
`LIST_RIGHT` (right marker), `AFTER` (content after the list, same alignment section as the
list). Text goes to the screen selected by the current `#[align=...]` / `#[list=...]` state;
nonprintable characters are dropped; `#`-runs are halved (escaping) per
`format_leading_hashes` (`format-draw.c:646-674`, `760-788`).

The `list=focus` directive records `focus_start`/`focus_end` = the cell range of the focused
item within the LIST screen (the current window, per the default format).

After parsing, one of five arrangement functions draws the interim screens into the target,
selected by the alignment in force when `list=on` first appeared (i.e. `status-justify`):

- **No list** (`format_draw_none`, `format-draw.c:168-224`): trim order **centre → right →
  left** until `width_left+width_centre+width_right <= available`. Left at 0; right at
  `available - width_right` (right-trimmed content keeps its *rightmost* cells:
  source offset `right->cx - width_right`); centre centred between them; abs_centre in the
  true centre of the whole width.
- **List left** — `status-justify left`, the default (`format_draw_left`,
  `format-draw.c:227-327`): trim order **centre → list → right → after → left**. Left at 0,
  right at `available - width_right`, after at `width_left + width_list`, centre centred
  between `width_left+width_list+width_after` and `available-width_right`, list at offset
  `width_left`. If the list shrank to 0, falls back to the no-list layout. Focus default:
  left end.
- **List centre** — `status-justify centre` (`format_draw_centre`,
  `format-draw.c:330-435`): trim order **list → after → centre → right → left**.
  `middle = width_left + ((available - width_right) - width_left) / 2` (centre of the space
  *between* left and right, not of the terminal); centre at `middle - width_list/2 -
  width_centre`, after at `middle - width_list/2 + width_list`, list centred on `middle`.
  Focus default: list middle.
- **List right** — `status-justify right` (`format_draw_right`, `format-draw.c:438-542`):
  trim order **centre → list → right → after → left**. After at `available - width_after`,
  right at `available - width_right - width_list - width_after`, list between right and
  after. (With the default format, status-right is emitted *after* the list with
  `align=right`, so it lands in AFTER and stays at the right edge; the list sits directly
  left of it.)
- **absolute-centre** (`format_draw_absolute_centre`, `format-draw.c:544-644`): left/centre/
  right trimmed **centre → right → left** to fit; the list+after+abs_centre group is trimmed
  **list → after → abs_centre** independently and drawn *over* the rest, centred in the whole
  width: `abs_centre_offset = (available - width_list - width_abs_centre) / 2`.

**Window-list overflow markers.** When the LIST screen is wider than its allotted width,
`format_draw_put_list` (`format-draw.c:124-165`) scrolls it to keep the focus centre visible
(`start = focus_centre - width/2`, clamped) and draws the `LIST_LEFT` marker at the left edge
iff `start != 0`, and the `LIST_RIGHT` marker at the right edge iff `start + width <
list->cx`. In the default status-format those markers are the literal characters `<` and `>`
(`#[list=left-marker]<#[list=right-marker]>`). So: **`<`/`>` appear only for the window
list**, never for status-left/right (those are silently truncated by the `=` modifier before
drawing).

**Summary of "what gets cut first"** for the classic line with justify=left: the centre
section (unused by default), then the *window list* (which scrolls around the current window
with `<`/`>` markers), then status-right, then content after the list, and status-left only
as a last resort. Note status-left/right have *already* been hard-capped at
`status-left-length` (default 10) / `status-right-length` (default 40) during expansion.

### 1.5 status-left / status-right defaults

- `status-left` default `"[#{session_name}] "`, capped at `status-left-length` **10**.
- `status-right` default
  `#{?window_bigger,[#{window_offset_x}#,#{window_offset_y}] ,}"#{=21:pane_title}" %H:%M %d-%b-%y`,
  capped at `status-right-length` **40**.
- `status-left ''` is perfectly legal: the section is empty and the window list starts at
  column 0. (`status-left-length 10` with an empty left is a no-op.)
- `status-right '%Y-%m-%d %H:%M '` renders via strftime as described in §1.3.

### 1.6 Multi-line status

`status 2..5` gives 2-5 rows; `status-format[i]` supplies row *i*'s format. Rows share the
same base style (`status-style`). Default rows 1 and 2 (`options-table.c:184-233`) render a
pane list (`#{P:...}` with `window-pane-status-format` and `pane-status-style` etc.) and a
session list (`#{S:...}` with `session-status-style` etc.), each prefixed with a
space-run `#{R: ,#{n:#{session_name}}}P: ` that right-pads to align under `[name] `.
`message-line` (choice `0..4`, default 0) picks which row the message/prompt is drawn on;
`status_prompt_line_at` clamps it to `lines - 1` (`status.c:125-138`).

### 1.7 status_redraw: caching and change detection

`status_redraw()` (`status.c:214-314`):

1. Builds a format tree with `FORMAT_STATUS` (plus `FORMAT_FORCE` if the client has
   `CLIENT_STATUSFORCE`).
2. Base style: `style_apply(&gc, s->options, "status-style", ft)`, then the deprecated
   `status-fg`/`status-bg` (colour options, default 8 = "default") override fg/bg if set
   (`status.c:248-254`). If this resolved style differs from the cached one → `force = 1`.
3. Resizes the status screen if tty width or line count changed → `force = changed = 1`.
4. Per line: expands `status-format[i]` with `format_expand_time`; **if the expanded string
   is byte-identical to the cached `sle->expanded` and not forced, the line is skipped
   entirely** (`status.c:286-291`). Otherwise the line is cleared with the base style,
   `format_draw()` paints it and rebuilds that line's mouse ranges
   (`style_ranges_free` + `format_draw(..., &sle->ranges, 0)`), and the expansion is cached.
5. Returns `force || changed`; if 0, the caller (`redraw_draw`, `screen-redraw.c:1598-1609`)
   drops `REDRAW_STATUS` and possibly skips the redraw altogether.

---

## 2. Window list rendering

### 2.1 Formats and separator

- `window-status-format` default: `#I:#W#{?window_flags,#{window_flags}, }`
  (`options-table.c:1833-1839`). I.e. `index:name` then the flags string, or a single space
  if no flags (keeps widths stable).
- `window-status-current-format` default: identical string (`options-table.c:1817-1822`).
  The current window is distinguished by *style* (and by `*` in `#{window_flags}`), not by a
  different default format.
- `window-status-separator` default `" "` (one space, `options-table.c:1850-1855`); expanded
  with `#{E:...}` (formats allowed, no strftime); emitted after every entry **except the
  last** (`#{?loop_last_flag,,...}` — note: after the last *window*, not suppressed around
  the current window).
- Both formats are expanded with `#{T:...}` → strftime applies inside them too.

### 2.2 Style resolution order (who wins)

Per the default status-format (§1.2), each window entry's `#[...]` block concatenates style
strings in this order — later directives override earlier ones for the same attribute, and
attribute flags accumulate (`style_parse` is additive, `style.c:68-299`):

1. **Base:** non-current: `window-status-style` (default `default`).
   Current: `window-status-current-style` **if it is not the literal string `default`**,
   else `window-status-style`. (Default `window-status-current-style` in this tree is
   `underscore`; classic tmux ≤ 3.5 shipped `default` here.)
2. **Last window:** if `window_last_flag` and `window-status-last-style` ≠ `default`,
   append it (its default *is* `default`, so normally inert).
3. **Bell:** if `window_bell_flag` and `window-status-bell-style` ≠ `default`, append it
   (default `reverse`). **Bell beats activity/silence** — the activity check is in the
   *else* branch of the bell conditional.
4. **Activity/silence:** only if no bell styling applied: if (`window_activity_flag` or
   `window_silence_flag`) and `window-status-activity-style` ≠ `default`, append it
   (default `reverse`).

So a window that is both **current and has a bell** gets current-style *plus* bell-style
layered on top (bell's fg/bg/attrs win where they conflict). A window that is both
last and has activity gets last-style then activity-style. Silence and activity share
`window-status-activity-style`.

### 2.3 #{window_flags} characters and priority

`window_printable_flags()` (`window.c:1027-1053`) appends, in this fixed order:

| char | meaning | condition |
|---|---|---|
| `#` | activity alert | `WINLINK_ACTIVITY` |
| `!` | bell alert | `WINLINK_BELL` |
| `~` | silence alert | `WINLINK_SILENCE` |
| `*` | current window | `wl == s->curw` |
| `-` | last window | first in the session's lastw stack |
| `M` | marked pane in this window | `server_check_marked()` |
| `Z` | zoomed | `WINDOW_ZOOMED` |

All applicable flags are shown concatenated (e.g. `#*Z`) — there is no single-char priority
pick. In format contexts the `#` is escaped to `##` (the `escape` argument; the
`window_flags` format callback passes escape=1) so it renders literally rather than starting
a style. Pane flags for completeness (`window.c:1055-1072`): `*` active, `-` last, `Z`
zoomed, `F` floating.

---

## 3. Styles

### 3.1 Style options: defaults and coverage

| option | default (this tree) | classic default | covers |
|---|---|---|---|
| `status-style` | `bg=themegreen,fg=themeblack` | `bg=green,fg=black` | whole status area base cell (all lines) |
| `status-left-style` | `default` | `default` | status-left section (applied via `#[... #{E:status-left-style}]` + `push-default`) |
| `status-right-style` | `default` | `default` | status-right section |
| `window-status-style` | `default` | `default` | every window entry base |
| `window-status-current-style` | `underscore` | `default` | current window entry (only if ≠ `default`) |
| `window-status-last-style` | `default` | `default` | last-window entry (only if ≠ `default`) |
| `window-status-bell-style` | `reverse` | `reverse` | bell-flagged entry, beats activity |
| `window-status-activity-style` | `reverse` | `reverse` | activity- or silence-flagged entry |
| `message-style` | `bg=themeyellow,fg=themeblack,` + conditional `fill=themeyellow` | `bg=yellow,fg=black` | messages and the prompt |
| `message-command-style` | `bg=themeblack,fg=themeyellow,` + conditional `fill=themeblack` | `bg=black,fg=yellow` | prompt while in vi command mode |
| `mode-style` | `noattr,bg=themeyellow,fg=themeblack` | `bg=yellow,fg=black` | copy-mode selection/indicators, choose-tree highlighting (not status) |

Styles are strings parsed by `style_parse()` (`style.c:68-299`): comma/space-separated words:
`fg=`, `bg=`, `us=` colours (`=8`/`default` resets to base), attribute names
(`bright`/`bold`, `dim`, `underscore`, `blink`, `reverse`, `hidden`, `italics`,
`overline`, `strikethrough`, `double-underscore`, `curly-underscore`, `dotted-underscore`,
`dashed-underscore`), `no<attr>` to clear one, `none` to clear all, `default` to reset
fg/bg/us/attrs to the base cell. So the user's `message-style 'fg=white bg=black bold'`
(space-separated) is valid; **a style option's parser must accept spaces, commas, and
newlines as delimiters** (`const char delimiters[] = " ,\n"`, `style.c:72`).

Layout-only directives (meaningful inside format `#[...]` blocks, parsed by the same
function): `align=left|centre|right|absolute-centre` / `noalign`,
`list=on|focus|left-marker|right-marker` / `nolist`,
`range=left|right|window|N|pane|%N|session|$N|user|name|control|N` / `norange`,
`fill=<colour>`, `push-default` / `pop-default` / `set-default`, `ignore`/`noignore`,
`width=N`/`width=N%`, `pad=N`, `dim=N%`, `link=uri`/`nolink`.

`style_apply(gc, oo, name, ft)` (`style.c:466-473`) = start from `grid_default_cell`, then
overlay the option's fg/bg/us if ≠ 8 and OR in attrs. Option style *values are themselves
format-expanded* before parsing (that's what `#{E:...}` in the status-format does, and why
`message-style`'s default can embed a `#{?...}` conditional).

### 3.2 Inline `#[...]` styles vs. the base: push/pop-default

Within `format_draw()` the "default" that `#[default]` (and `fg=default` etc.) resets to is
`current_default`, initially the base cell passed in (e.g. the status-style cell). The
default status-format brackets each user-controlled fragment with:

```
#[range=... <section-style>]#[push-default] <user format> #[pop-default]#[norange default]
```

Semantics (`format-draw.c:856-871`):

- `push-default` — the style in force *before this `#[]` block* becomes the new
  `current_default`. So inside `status-left`, `#[default]` (or an unqualified `fg=...`
  reverting via `default`) restores *status-style + status-left-style*, not the bare
  status-style. It is a **one-slot save, not a real stack**.
- `pop-default` — restore `current_default` to the original base.
- `set-default` — permanently replace both base and current default with the pre-block style.

A user `#[fg=white]` inside `window-status-format` therefore changes fg but inherits
bg/attrs from the pushed default (window-status-style layered over status-style). This is
exactly what the motivating config relies on
(`window-status-format ' #I #[fg=white]#W #[fg=white]#F '`).

`fill=<colour>` (`format-draw.c:996-1002`): if any style in the string set `fill`, the whole
available width is first cleared to that background before the sections are drawn.

---

## 4. Messages (status_message_set)

### 4.1 API and timing

```c
void status_message_set(struct client *c, int delay, int ignore_styles,
    int ignore_keys, int no_freeze, const char *fmt, ...)   /* status.c:339-389 */
```

- `delay == -1` → use the `display-time` session option (default **750** ms).
- `delay == 0` → **no timer: the message stays until a key is pressed.**
- `delay > 0` → that many milliseconds.
- The message replaces any current message and prompt state? No — it clears any *previous
  message* (`status_message_clear`) and pushes the status screen (`status_push_screen`,
  §4.3). A message and a prompt can coexist structurally, but the redraw path prefers the
  message (`screen-redraw.c:1599-1604`: message → prompt → status).
- `ignore_keys` (`display-message -N`): while set, key input is swallowed and does *not*
  dismiss the message (`server-client.c:1624-1628`: `if (c->message_ignore_keys) return`).
  **Forced off when `delay == 0`** (`status.c:381-382`) — otherwise the message could never
  be dismissed.
- Without `ignore_keys`, *any* key press clears the message immediately and the key is then
  processed normally.
- `no_freeze` (`display-message -C`): normally a message sets `TTY_FREEZE` (pane output to
  the terminal is paused while the message shows) and `TTY_NOCURSOR`; `-C` skips the freeze.
- `ignore_styles`: `#` characters in the message text are doubled
  (`status_message_escape`, `status.c:317-336`) so `format_draw` shows them literally.
  The visual-* alert messages pass 1; `display-message` passes 0 (styles in user messages
  are honoured).
- Every message is also appended to the server message log shown by `show-messages`
  (`server_add_message`), capped by the server option `message-limit` (default 1000).
- Clearing (`status.c:392-406`) restores tty flags and sets `CLIENT_ALLREDRAWFLAGS` (a full
  redraw, since the screen was frozen and may be stale).

### 4.2 Rendering

`status_message_redraw()` (`status.c:461-524`):

1. Working screen = copy of the *frozen* base status screen (all lines), so on a multi-line
   status only the message's line is overwritten; the other lines stay visible (stale).
2. Line = `message-line` clamped (§1.6). Area x/width come from **message-style's `width`
   and `align` directives** (`status_message_area`, `status.c:412-449`): `width=N` or
   `width=N%` of tty columns (0 or too-big → full width); `align=centre|absolute-centre` →
   `x=(sx-w)/2`, `align=right` → `x=sx-w`, else 0.
3. Style = `style_apply(message-style)`. The text drawn is not the raw message: it is
   `message-format` (default `#[#{?#{command_prompt},#{E:message-command-style},#{E:message-style}}]#{message}`)
   expanded with `#{message}` = the message text and `command_prompt` = 0, then
   `format_draw`n into the area with the message-style cell as base. Truncation is therefore
   `format_draw_none`'s: overlong text is cut at the right edge (centre→right→left trim of
   that string's own sections; a plain message is all LEFT section, so simply clipped).
4. The result is grid-compared with the previous active screen; unchanged → no tty write.

### 4.3 The push/pop screen mechanism

`status_push_screen`/`status_pop_screen` (`status.c:152-175`): the first message/prompt
allocates a scratch `sl->active` screen (reference-counted), leaving `sl->screen` = the last
real status content frozen underneath; while any message/prompt is up, the status timer does
**not** mark `CLIENT_REDRAWSTATUS` (`status.c:49-50`), so the underlying status is not
recomputed until the message/prompt closes.

### 4.4 Bell vs. message: the visual-* options

`alerts_set_message()` (`alerts.c:292-325`), for `visual-bell`, `visual-activity`,
`visual-silence` (all choice `off|on|both`, default `off`):

- `off` → send a terminal BEL to each client on the window's session, no message.
- `on` → message only. `both` → BEL and message.
- Message text: `"Bell in current window"` / `"Bell in window %d"` (winlink index); same for
  `Activity` / `Silence`. Sent with `delay=-1` (display-time), `ignore_styles=1`,
  `ignore_keys=0`, `no_freeze=0`.
- Gating: the alert fires only if `monitor-bell`/`monitor-activity`/`monitor-silence` is on
  and `bell-action`/`activity-action`/`silence-action` (choice `none|any|current|other`,
  defaults `any`/`other`/`other`) applies to that window (`alerts.c:206-214, 242-250,
  278-286`).

---

## 5. The command prompt

### 5.1 Invocation and prompt string

`command-prompt` (`cmd-command-prompt.c`): flags `-1` single-key, `-N` numeric, `-i`
incremental, `-k` key-name, `-e` (`PROMPT_BSPACE_EXIT` — backspace on empty input closes the
prompt), `-C` no-freeze, `-b` don't block the queue, `-p prompt1,prompt2,...` prompts,
`-I input1,input2,...` initial texts, `-T command|search` prompt type, `-F` expand the
template up front. Default prompt when none given and no template: `":"` **with no trailing
space** (`cmd-command-prompt.c:119-121`); with a template: `"(template) "`; `-p` prompts get
a trailing space appended (`:141`). Multiple comma-separated prompts run sequentially,
collecting one `%%`/`%1`-substituted answer each (`cmd_command_prompt_callback`,
`:197-265`). The `prefix-:` binding is simply `bind : { command-prompt }`
(`key-bindings.c:410`).

`confirm-before` (`cmd-confirm-before.c`): prompt default `"Confirm 'cmd'? (y/n) "` (or `-p`
prompt + space), `PROMPT_SINGLE`; the confirm key is `y` or `-c <char>`; with `-y`, Enter
(`\r`) also confirms (`:88-101, 122-135`). Any other key cancels.

The prompt *message* (prefix) is drawn through `message-format` too: `prompt_expand()`
(`prompt.c:372-398`) expands `pr->string` with format vars `prompt_input` (current buffer
text), `prompt_flags`, `prompt_type`, then sets `#{message}` = that and `command_prompt` =
1/0 (1 only in vi command mode) and expands `message-format`. So the prompt inherits
message-style, and flips to **message-command-style** (plus `prompt-command-cursor-style`/
`-colour`) in vi command mode. Options are snapshotted at prompt creation
(`prompt_set_options`, `prompt.c:109-134`): message-style, message-command-style,
prompt-cursor-style/colour, prompt-command-cursor-style/colour, message-format,
`status-keys`, `word-separators`.

### 5.2 Drawing and the cursor

`status_prompt_redraw()` (`status.c:656-698`) mirrors the message path: copies the frozen
status screen, computes the same message-style width/align area, and calls `prompt_draw()`
(`prompt.c:458-527`):

- `start` = display width of the expanded prompt prefix (`format_width`, which understands
  `#[]`), clamped to the area width; the input buffer is drawn starting at `ax + start`.
- Horizontal scroll: `pcursor` = width of buffer up to the cursor index; if
  `pcursor >= left` (space remaining), draw with `offset = pcursor - left + 1` so the cursor
  sits at the right edge (`prompt.c:502-509`).
- Terminal cursor position = `ax + start + pcursor - offset`, exported via
  `status_prompt_cursor()` (`status.c:701-706`) and applied in
  `server_client_reset_state` (`server-client.c:2030-2032`). The message path hides the
  cursor (`TTY_NOCURSOR`); the prompt shows it, with `prompt-cursor-style` (choice
  `default|blinking-block|block|blinking-underline|underline|blinking-bar|bar`, default
  `default`) and `prompt-cursor-colour` (default empty = terminal default).
- Control characters in the buffer render as `^X` occupying 2 cells (`prompt.c:290-317`);
  `C-v` (quote-next) shows a `^` at the cursor while pending (`PROMPT_QUOTENEXT`).
- Pending tab-completion matches are drawn inline after the cursor, underlined, truncated to
  the area (`prompt_draw_complete`, `prompt.c:339-369`) — see §5.5.
- Prompt line = `message-line` (same as messages); screen row from
  `status_prompt_screen_line` (`status.c:641-653`).

Mouse: a MouseDown1 on the prompt line moves the cursor to the clicked character
(`prompt_mouse`, `prompt.c:530-584`; entry check in `status_prompt_key`,
`status.c:709-730`), or accepts a clicked completion candidate
(`prompt_mouse_complete`, `prompt.c:416-455`).

### 5.3 The line editor — emacs table (status-keys emacs, the default)

From `prompt_key()` (`prompt.c:1084-1409`). Keypad keys are first normalized to their ASCII
equivalents (`prompt_keypad_key`, `prompt.c:605-646`).

| key(s) | action |
|---|---|
| `Left` / `C-b` | cursor left |
| `Right` / `C-f` | cursor right |
| `Home` / `C-a` | start of line |
| `End` / `C-e` | end of line |
| `Tab` | complete word at cursor (§5.5) |
| `BSpace` / `C-h` | delete char before cursor; with `-e` flag and empty buffer, close prompt |
| `DC` / `C-d` | delete char under cursor |
| `C-u` | delete the **entire line** (not just to start; `pr->buffer[0].size = 0`) |
| `C-k` | delete from cursor to end |
| `C-w` | delete word before cursor (word-separators aware); **deleted text is saved to the prompt's local copy buffer** used by `C-y` |
| `C-Right` / `M-f` | forward word (emacs style: skip spaces, then to end of class run) |
| `C-Left` / `M-b` | backward word |
| `Up` / `C-p` | previous history entry (per prompt type) |
| `Down` / `C-n` | next history entry (empty string past the newest) |
| `C-y` | paste: the prompt-local `C-w` copy if any, else the **top tmux paste buffer**, truncated at the first control character (`prompt_paste`, `prompt.c:802-863`) |
| `C-t` | transpose the two chars before the cursor (cursor advances) |
| `Enter` / `C-j` | commit: non-empty input is appended to history; fire callback |
| `Escape` / `C-[` / `C-c` / `C-g` | cancel (callback fired with NULL) |
| `C-r` / `C-s` | only for **incremental** prompts (copy-mode search): re-fire the callback with `-`/`+` prefix (search again backward/forward); on an empty buffer, first recall the last search (`pr->last`) with `=` prefix |
| `C-v` | quote next key literally |
| anything else printable/Unicode | insert at cursor |

Word logic: `word-separators` (session option; default = all printable non-alphanumeric
ASCII except `_`: `!"#$%&'()*+,-./:;<=>?@[\]^`{|}~`) defines a second character class;
words are runs of a single class; spaces always separate
(`prompt_forward_word`/`prompt_end_word`/`prompt_backward_word`, `prompt.c:936-1041`).

### 5.4 The line editor — vi table (status-keys vi)

`prompt_translate_key()` (`prompt.c:654-800`). The prompt starts in **insert mode**:

- Insert mode: all the emacs control keys above still work (they're whitelisted and
  processed as emacs, `prompt.c:658-696`); other keys self-insert. `Escape`/`C-[` switches
  to command mode and moves the cursor one left (vi semantics), and flags a redraw (style
  flips to message-command-style).
- Command mode translations (each returns to the emacs handler with a mapped key):
  `A`→End+insert, `I`→Home+insert, `C`,`D`→kill-to-end (`C` also enters insert), `S`→kill
  line + insert, `s`→delete char + insert, `a`→Right+insert, `i`→insert,
  `$`→End, `0`,`^`→Home, `x`,`X`,`DC`→delete (X = backspace), `BSpace`,`h`→left, `l`→right,
  `k`/`Up`→history up, `j`/`Down`→history down, `b`/`B` back word (B: space-separated only),
  `w`/`W` forward word (vi style: land on next word start), `e`/`E` end of word,
  `d`→delete whole line, `p`→paste (`C-y`), `q`→cancel, `Enter`/`C-h`/`C-c` as emacs.
  `Escape` in command mode is ignored.

### 5.5 Tab completion (this master's behavior)

`prompt_complete()` (`prompt.c:1537-1576`) + `prompt_replace_complete()`
(`prompt.c:866-933`):

- Only for `PROMPT_TYPE_COMMAND` prompts, and only when the word under the cursor **starts
  at offset 0** of the buffer (i.e. the command name position), and is non-empty.
  **There is no `-t` target/session/window-name completion in this tree** (older tmux
  versions had a popup-menu target completion; it has been removed — the completion source
  is exactly command names + `command-alias` names, `prompt_complete_commands`,
  `prompt.c:1426-1461`).
- The word is the space-delimited token around the cursor.
- Unique match → replace the word with `"name "` (trailing space appended).
- Multiple matches → replace with the longest common prefix if it extends the word;
  otherwise store the sorted match list and display it **inline**: a space-separated,
  underlined list drawn right after the cursor (only shown while the cursor is at the end
  of the buffer), clipped to the prompt area (`prompt_draw_complete`). Any subsequent
  keypress clears the stored list (`prompt.c:1096-1100`); clicking a candidate inserts it
  (`prompt_mouse_complete`).

### 5.6 Prompt history

`prompt-history.c`; per-type lists (`PROMPT_NTYPES` = 2: `command`, `search`):

- `Up`/`Down` walk the list newest-first; index 0 = the empty "live" line; going down past
  the newest returns `""`.
- Committing a non-empty line appends it, **deduplicating only against the immediately
  previous entry** (`prompt_add_history`, `prompt-history.c:181-229`).
- Capacity: `prompt-history-limit` (server option, default **100**); overflow evicts oldest.
- Persistence: `history-file` (server option, default `""` = disabled; `~/...` expanded).
  Loaded once at config time (`cfg.c:59`), saved on server exit (`server.c:257`).
  File format: one entry per line, `type:text` (e.g. `command:kill-window`); untyped lines
  load as command history for backward compatibility.
- `show-prompt-history [-T type]` / `clear-prompt-history [-T type]` commands exist
  (`cmd-show-prompt-history.c`).
- History indices are per *prompt instance* (`pr->hindex[]`), reset when a multi-prompt
  sequence advances (`prompt_update`, `prompt.c:275`).

### 5.7 Prompt flags summary (tmux.h:2096-2110)

`PROMPT_SINGLE` (one key answers), `PROMPT_NUMERIC` (digits only; any other key closes; used
by `-N` repeat prompts), `PROMPT_INCREMENTAL` (`-i`, fire callback on every change with
`=`/`-`/`+` prefix; Up/Down/PPage/NPage become `PROMPT_KEY_MOVE` events passed to the caller
— used by incremental copy-mode search), `PROMPT_NOFORMAT` (initial input not
format-expanded), `PROMPT_KEY` (`-k`: report the key name), `PROMPT_ACCEPT`,
`PROMPT_QUOTENEXT`, `PROMPT_BSPACE_EXIT` (`-e`), `PROMPT_NOFREEZE` (`-C`),
`PROMPT_COMMANDMODE` (vi command mode active), `PROMPT_ISPANE`/`PROMPT_ISMODE` (pane-attached
prompts; also disable the `fill` in the default message-style via its `#{m/r:...}`
conditional on `prompt_flags`), `PROMPT_EDITARROWS` (incremental prompt keeps Left/Right as
editing keys instead of MOVE events).

Key-routing order for a client key (`server-client.c:1623-1673`): message (dismiss or
swallow) → overlay → status prompt (`status_prompt_key`; HANDLED/CLOSE consume the key) →
pane prompt → normal key bindings.

---

## 6. Clock mode (window-clock.c)

- Entered with `clock-mode` (default binding `prefix t`, `key-bindings.c:433`); it is a
  window *mode* on the pane. **Any key exits** (`window_clock_key` calls
  `window_pane_reset_mode`, `window-clock.c:213-219`). Cursor is hidden (`s->mode &=
  ~MODE_CURSOR`).
- `clock-mode-style`: choice `12 | 24 | 12-with-seconds | 24-with-seconds` (values 0-3),
  default **24** (`options-table.c:38-40, 1342-1348`; classic tmux had only 12/24). Time
  strings: `%l:%M ` (+`AM`/`PM`), `%l:%M:%S `+AM/PM, `%H:%M`, `%H:%M:%S`
  (`window-clock.c:247-261`).
- `clock-mode-colour`: default `themeblue` (classic `blue`), a colour-flavoured style option
  resolved via `style_apply` (fg used as the colour).
- Rendering (`window_clock_draw_screen`, `window-clock.c:222-315`): screen cleared to
  default bg (`clearscreen(8)`). **Big-digit mode** requires `sx >= 6*strlen(time)` and
  `sy >= 6`; each glyph is a 5×5 bitmap from `window_clock_table[14][5][5]`
  (`window-clock.c:53-124`: digits 0-9, `:`, `A`, `P`, `M`), drawn as `#` characters with
  **both fg and bg set to the clock colour** (solid blocks), glyph pitch 6 columns; origin
  `x = sx/2 - 3*len`, `y = sy/2 - 3`. This is **the same 5×5 table display-panes uses** for
  its big pane numbers (`cmd-display-panes.c:241`). Fallback when too small: the plain time
  string centred at `(sx/2 - len/2, sy/2)` in the clock colour (fg only); nothing if even
  that doesn't fit.
- Refresh: a timer aligned to the next wall-clock second boundary
  (`window_clock_start_timer`, `window-clock.c:126-144`); on fire, redraw only if the
  second actually changed, then re-arm (`window-clock.c:146-168`).

---

## 7. Status-line mouse ranges (short — mechanism only)

`format_draw` records, per drawn status line, the final screen-cell extents of every
`#[range=...]` region as `style_range` entries (`format-draw.c:958-979, 1044-1087`);
`status_redraw` stores them per line in `c->status.entries[y].ranges` (`status.c:298-299`).
On a mouse event landing in the status area (`server-client.c:910-970`),
`status_get_range(c, x, y - statusat)` (`status.c:141-149`) finds the range under the
pointer:

- no range → key location `StatusDefault`;
- `range=left` → `StatusLeft`; `range=right` → `StatusRight`;
- `range=window|N` → location `Status` with the mouse event's window resolved from winlink
  index N (click maps to that window regardless of pixel math — the *range* carries the
  identity); similarly `range=pane|%N` and `range=session|$N` set the target pane/session;
  `range=user|name` → `Status`; `range=control|N` (N<10) → special control locations.
- Invalid/vanished targets make the key `KEYC_UNKNOWN` (swallowed).

Relevant default bindings (`key-bindings.c:525-545`): `MouseDown1Status` → `switch-client
-t=` (selects the clicked window via the resolved target), `C-MouseDown1Status` →
`swap-window -t@`, `WheelUpStatus`/`WheelDownStatus` → `previous-window`/`next-window`,
`MouseDown3Status`/`M-MouseDown3Status` → window context menu, `MouseDown3StatusLeft` →
session menu.

Clicks on the prompt line while a prompt is open are consumed by the prompt first
(cursor motion / completion click, §5.2).

---

## 8. Redraw discipline

- **Timer:** each client has a status timer (`status.c:37-73`) firing every
  `status-interval` seconds (default **15**; `0` = never re-arm, i.e. no periodic refresh).
  On fire it sets `CLIENT_REDRAWSTATUS` **unless a message or prompt is showing**. The timer
  is (re)started on attach and when session options change (`status_timer_start_all`).
- **Event-driven:** `server_status_client/session/session_group/window` (`server-fn.c:40-134`)
  set `CLIENT_REDRAWSTATUS` immediately; they are called on renames, window flag changes
  (bell/activity), window add/remove, session switches, option changes, etc.
  `server_status_window` marks *every* session containing the window, not just where it is
  current.
- **Cheapness:** setting the flag is cheap because `status_redraw` (§1.7) caches the
  expanded string per line and skips repainting identical lines; and even after repainting,
  the tty write is a diff against the previous screen (`tty_draw_line`). Message/prompt
  redraws grid-compare before writing (§4.2, §5.2).
- `CLIENT_STATUSFORCE` (set by `refresh-client -S` and option changes) adds `FORMAT_FORCE`,
  which forces `#()` command jobs to re-run instead of serving cached output
  (`format.c:424-429`); otherwise a given `#()` job re-runs at most once per second and its
  cached output is reused; jobs used from a status format are tagged so their completion
  triggers a status redraw (`format.c:443-444`).
- The actual paint happens once per event-loop pass in `server_client_check_redraw`
  (`server-client.c:2249+`): if the tty has outstanding output, the redraw is **deferred**
  via a 1 ms timer rather than mixing with pending data. `CLIENT_REDRAWSTATUS` also triggers
  a `REDRAW_PANE_STATUS` pass (pane status bars share the cycle,
  `screen-redraw.c:1763-1764`), and window-mode `update` callbacks run on status redraws
  (`server_client_check_modes`, `server-client.c:2214-2230`).
- Redraw of messages/prompts: `CLIENT_REDRAWSTATUS` is set on message set/prompt set/every
  handled prompt key; clearing either sets `CLIENT_ALLREDRAWFLAGS` (full screen) because the
  tty was frozen.

---

## 9. Defaults table

Scope: server = one global value; session = per-session (global table `global_s_options`);
window = per-window (`global_w_options`). All can be set with `set -g`.

| option | scope | type | default |
|---|---|---|---|
| `status` | session | choice off/on/2/3/4/5 | `on` |
| `status-interval` | session | number (seconds, 0=off) | `15` |
| `status-position` | session | choice top/bottom | `bottom` |
| `status-justify` | session | choice left/centre/right/absolute-centre | `left` |
| `status-keys` | session | choice emacs/vi | `emacs` |
| `status-left` | session | string (format+strftime) | `"[#{session_name}] "` |
| `status-left-length` | session | number 0-32767 | `10` |
| `status-left-style` | session | style | `default` |
| `status-right` | session | string (format+strftime) | `#{?window_bigger,[#{window_offset_x}#,#{window_offset_y}] ,}"#{=21:pane_title}" %H:%M %d-%b-%y` |
| `status-right-length` | session | number 0-32767 | `40` |
| `status-right-style` | session | style | `default` |
| `status-style` | session | style | `bg=themegreen,fg=themeblack` (classic: `bg=green,fg=black`) |
| `status-format` | session | array[5] of format | 3 entries, see §1.2 (`options-table.c:119-239`) |
| `status-bg` / `status-fg` | session | colour (deprecated) | `8` (default = inert) |
| `window-status-format` | window | string (format+strftime) | `#I:#W#{?window_flags,#{window_flags}, }` |
| `window-status-current-format` | window | string (format+strftime) | `#I:#W#{?window_flags,#{window_flags}, }` |
| `window-status-separator` | window | string (format) | `" "` |
| `window-status-style` | window | style | `default` |
| `window-status-current-style` | window | style | `underscore` (classic: `default`) |
| `window-status-last-style` | window | style | `default` |
| `window-status-bell-style` | window | style | `reverse` |
| `window-status-activity-style` | window | style | `reverse` |
| `message-style` | session | style | `bg=themeyellow,fg=themeblack` + conditional `fill=themeyellow` (classic: `bg=yellow,fg=black`) |
| `message-command-style` | session | style | `bg=themeblack,fg=themeyellow` + conditional `fill=themeblack` (classic: `bg=black,fg=yellow`) |
| `message-format` | session | format | `#[#{?#{command_prompt},#{E:message-command-style},#{E:message-style}}]#{message}` |
| `message-line` | session | choice 0-4 | `0` |
| `message-limit` | server | number | `1000` |
| `display-time` | session | number ms (0 = until key) | `750` |
| `mode-style` | window | style | `noattr,bg=themeyellow,fg=themeblack` (classic: `bg=yellow,fg=black`) |
| `mode-keys` | window | choice emacs/vi | `emacs` |
| `clock-mode-colour` | window | colour | `themeblue` (classic: `blue`) |
| `clock-mode-style` | window | choice 12/24/12-with-seconds/24-with-seconds | `24` |
| `prompt-history-limit` | server | number | `100` |
| `history-file` | server | string (path) | `""` (disabled) |
| `prompt-cursor-style` | session | choice (cursor styles) | `default` |
| `prompt-cursor-colour` | session | colour | `""` (terminal default) |
| `prompt-command-cursor-style` | session | choice | `default` |
| `prompt-command-cursor-colour` | session | colour | `""` |
| `word-separators` | session | string | `!"#$%&'()*+,-./:;<=>?@[\]^`{|}~` |
| `command-alias` | server | array | `split-pane=split-window,splitp=split-window,server-info=show-messages -JT,info=show-messages -JT,choose-window=choose-tree -w,choose-session=choose-tree -s` |
| `visual-bell` / `visual-activity` / `visual-silence` | session | choice off/on/both | `off` |
| `bell-action` | session | choice none/any/current/other | `any` |
| `display-panes-time` | session | number ms | `1000` |
| `display-panes-colour` / `-active-colour` | session | colour | `themeblue` / `themered` (classic: `blue`/`red`) |
| `display-panes-format` | session | format | `#[align=right]#{pane_width}x#{pane_height}` |

The motivating user config is therefore entirely legal tmux: `status-justify left` (a no-op
vs. the default), `status-right-style`/`window-status-style`/`window-status-current-style`/
`window-status-bell-style` (style options, space- or comma-separated words),
`window-status-format ' #I #[fg=white]#W #[fg=white]#F '` (inline styles resolved against
the pushed section default, §3.2), `message-style 'fg=white bg=black bold'`, `status-left ''`
+ `status-left-length 10`, and `status-right '%Y-%m-%d %H:%M '` + `status-right-length 50`
(strftime first, then truncate-left to 50, §1.3/§1.5).

---

## 10. Windows/winmux applicability notes

- **Everything in this domain is platform-neutral** — it is pure string/grid manipulation.
  The only OS-touching pieces are strftime (identical enough on Windows CRT for the common
  `%Y %m %d %H %M %S %b %a %l` set — note `%l` (blank-padded 12-hour) is a POSIX extension
  that Windows strftime lacks; winmux's clock/status must special-case it), `history-file`
  path expansion (`~/` and absolute `/` checks in `prompt_find_history_file` need
  Windows-path equivalents: accept drive letters and `%USERPROFILE%`), and `#()` format
  jobs (spawning shell commands — winmux may stub these to empty, as `FORMAT_NOJOBS` does).
- winmux's current single-line status should be generalized in this order: (1) accept and
  correctly parse *all* the §9 options (the user's real config fails today on
  `status-justify`, `status-right-style`, `window-status-*` options — even a partial
  implementation must at minimum parse-and-store them); (2) implement the §1.4 section
  model (left/list/right + justify + trim order + `<`/`>` list markers); (3) the §2.2 style
  layering; (4) messages/prompt already exist in winmux — align the message timing rules
  (`display-time`, 0 = until keypress; keypress dismisses) and the §5.3 emacs editor keys.
- The status-format *engine* (eight-screen `format_draw`) is the faithful route, but winmux
  can reproduce classic behavior without user-visible `status-format` support by
  hard-coding the default format's semantics (sections, style layering, focus-following
  window list); that covers every config that doesn't set `status-format` explicitly —
  which is nearly all of them.
- The `themeX` colours in this tree are a 2026 addition; winmux should keep emitting classic
  `green/yellow/black/blue/red` defaults (matching tmux ≤ 3.5) unless it also implements
  the `theme` option.
- Multi-line status (`status 2..5`) and pane/session status lines (default formats 2/3) are
  low-value for parity; single-line plus correct `status off` message-borrows-a-row
  behavior (§1.1) covers real-world configs.
- The prompt's `C-y` pulls from the *tmux paste buffer store* (winmux `buffers` module), not
  the OS clipboard — winmux already has the right seam.
- Tab completion in current tmux master is command-name-only (§5.5) — winmux need not build
  target completion to be faithful to master (though tmux ≤ 3.5 had a target-completion
  popup menu; document whichever baseline winmux targets).
