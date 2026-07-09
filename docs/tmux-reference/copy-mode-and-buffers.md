# tmux behavioral reference: copy mode, scrollback, and paste buffers

Source studied: tmux master `db115c6` (2026-07-07), full source. All file:line references
are into that tree. This is a *behavioral spec* — implementers should not need to reopen
the tmux source for anything in this domain.

> **Version note.** This master is newer than tmux 3.5. Features marked **[new]** did not
> exist in 3.x-era tmux and are optional for parity with "classic" tmux: copy-mode line
> numbers, `copy-mode-position-format`/`-style` options, the live-refresh timer
> (`refresh-on/off/toggle`), `recentre-top-bottom`, `cursor-centre-*`, `toggle-position`,
> `scroll-exit-*`, `selection-mode`, `next/previous-prompt`, scrollbar commands
> (`scroll-to-mouse`), and `theme*` colour names in style defaults. Everything else below
> is longstanding tmux behavior.

---

## 1. Architecture of copy mode (`window-copy.c`)

Copy mode is a *window mode* (`window_copy_mode`, window-copy.c:171) stacked on a pane.
A second mode, `window_view_mode` (:184), shares all the code and is used for command
output (`show-*`/`list-*` piped into a pane via `window_copy_add`); the differences are
noted where relevant.

Key state (`struct window_copy_mode_data`, window-copy.c:258):

- `screen` — the *visible* composed screen that is drawn to the pane.
- `backing` — a full **clone** of the pane's screen incl. its entire scrollback
  (`window_copy_clone_screen`, :382). Copy mode never reads the live pane grid directly;
  it reads the clone. When entered on the same pane, trailing blank lines are *not*
  trimmed; when entered with `-s src-pane` on a different pane they are (`trim` arg, :384,
  :602-603).
- `oy` — number of lines scrolled up from the bottom (0 = live bottom;
  max = `screen_hsize(backing)`).
- `cx`, `cy` — cursor position on the *visible* screen (cy in 0..sy-1).
  The cursor's absolute grid row is always computed as
  `py = hsize + cy - oy` — this idiom appears everywhere.
- `selx/sely`, `endselx/endsely` — selection endpoints in **absolute grid coordinates**
  (y counts from the top of history). `cursordrag` says which endpoint follows the cursor
  (`CURSORDRAG_NONE/ENDSEL/SEL`, :279).
- `lastcx`/`lastsx` — "sticky column" memory for vertical movement (§6.3).
- `mx/my/showmark` — the mark (`set-mark`/`jump-to-mark`).
- `searchtype/searchdirection/searchregex/searchstr/searchmark/...` — search state (§7).
- `modekeys` — snapshot of `mode-keys` taken at mode entry (:583); but note most
  per-command checks re-read the option live (e.g. :4509, :5957).
- `wme->prefix` — the repeat count for the next command (default 1; see §3.3).

### 1.1 Backing refresh while in copy mode [new]

Classic tmux froze the backing snapshot for the life of copy mode. This master adds a
50 ms refresh timer (`WINDOW_COPY_REFRESH_INTERVAL`, :354; `window_copy_refresh_timer`,
:2973) which re-syncs the backing screen from the live pane **only when**:

- the pane has unseen changes (`PANE_UNSEENCHANGES`),
- there is **no selection** (`data->screen.sel == NULL`) and no cursor drag,
- the mode is not view-mode and not a `-s` view of another pane (:3010).

If the cursor sits at the very bottom (`oy == 0 && cy == last row`) the view *follows*
new output; otherwise the same absolute lines are kept on screen (:2943-2953). Sync is
incremental when the grid's monotonic scroll counters line up
(`window_copy_sync_backing`, :464 — `scroll_added`/`scroll_collected`/
`scroll_generation` deltas), else it falls back to a full re-clone. The refresh can be
toggled with the `refresh-on/off/toggle` copy-mode commands (`r` in both key tables).
**For a clone targeting classic behavior, a frozen snapshot is acceptable; the
observable classic rule is: content visible at entry stays put, live output does not
move your view.**

---

## 2. Entering copy mode (`cmd-copy-mode.c`)

```
copy-mode [-deHMqSu] [-s src-pane] [-t target-pane]
```

Flags (cmd-copy-mode.c:33-36, exec at :57):

| Flag | Behavior |
|---|---|
| (none) | Enter copy mode on the target pane. If the pane is already in copy mode, this is a no-op (mode not re-initialised). |
| `-u` | After entering, scroll one **page** up (`window_copy_pageup(wp, 0)`, :101). This is the `PPage` root binding. |
| `-d` | After entering, scroll one page **down** (:104) — combined with `-e` it can exit immediately if already at the bottom. Used by the scrollbar down-arrow binding. |
| `-e` | Sets `data->scroll_exit = 1` (window-copy.c:615): copy mode **cancels itself when the view returns to the bottom** (`oy == 0`) via any scroll-down/page-down/cursor-down path. This is what the `WheelUpPane` root binding uses, so wheel-down past the bottom leaves copy mode. |
| `-H` | Sets `data->hide_position = 1` (:616): the position indicator is not drawn. Used by the double/triple-click bindings so the flash of copy mode is invisible. |
| `-M` | Mouse-initiated: resolves the pane from the mouse event, requires the client's session to match, and after entering starts a drag selection (`window_copy_start_drag`, :97). Used by `MouseDrag1Pane`. |
| `-q` | Exit: `window_pane_reset_mode_all(wp)` — cancels any mode on the pane, then returns (:71). |
| `-S` | **[new]** Scrollbar drag entry (`window_copy_scroll`), positions the view from the scrollbar slider. |
| `-s src-pane` | View another pane's content inside this pane (`wme->swp != wme->wp`). Backing is cloned from the source with trailing blank lines trimmed; the refresh timer never runs for such views (:3010). |

Entry state (`window_copy_init`, window-copy.c:591):

- The clone preserves the pane's cursor; `data->cx/cy` start at the pane's cursor
  position (reflow-adjusted if width differs), `oy = 0` (:606-613).
- The mark `mx/my` is initialised to the cursor position, `showmark = 0` (:625-627).
- If the pane has a previous search (`wp->searchstr`), it is inherited with
  `searchtype = SEARCHUP` so `n` works immediately (:565-569).
- `-e`/`-H` per above. Line numbers **[new]** are enabled for keyboard entry, disabled
  for mouse entry (cmd-copy-mode.c:92-94), but actually drawing them requires
  `copy-mode-line-numbers` ≠ off (default off).

### 2.1 Default bindings that enter copy mode (key-bindings.c)

- prefix `[` → `copy-mode` (:418)
- prefix `PPage` → `copy-mode -u` (:442)
- `WheelUpPane` (no prefix) → `if #{||:#{alternate_on},#{pane_in_mode},#{mouse_any_flag}} { send -M } { copy-mode -e }` (:506)
- `MouseDrag1Pane` → `if #{||:#{pane_in_mode},#{mouse_any_flag}} { send -M } { copy-mode -M }` (:502)
- `DoubleClick1Pane` → `select-pane -t=; if ... { send -M } { copy-mode -H; send -X select-word; run -d0.3; send -X copy-pipe-and-cancel }` (:512)
- `TripleClick1Pane` → same with `select-line` (:515)
- `MouseDown1ScrollbarUp/Down`, `MouseDrag1ScrollbarSlider` → `copy-mode -u` / `-d` / `-S` **[new]** (:552-554)

Note the wheel binding: with an **alternate-screen** app active (`alternate_on`), wheel
events are sent to the app instead of entering copy mode.

### 2.2 The key table switch

`window_copy_key_table` (window-copy.c:1229): returns `"copy-mode-vi"` if the window's
`mode-keys` option is `vi`, else `"copy-mode"`. Checked per keypress, so changing
`mode-keys` takes effect immediately.

### 2.3 Exiting

All exit paths:

1. `cancel` command → `WINDOW_COPY_CMD_CANCEL` → `window_pane_reset_mode(wp)` (:3763).
   Bound: emacs `q`, `Escape`, `C-[`, `C-c`; vi `q`, `C-c` (vi `Escape` is
   `clear-selection`, **not** cancel).
2. Any `*-and-cancel` command after doing its work (copy-selection-and-cancel, etc.).
3. `scroll_exit` (`-e`): `page-down`, `halfpage-down`, `scroll-down`, and the internal
   pagedown path return CANCEL when `oy` hits 0 (:975, :2355).
4. `cursor-down-and-cancel`: cancels when cursor could not move down and `oy == 0`
   (:1595).
5. `copy-mode -q` from outside.
6. Killing the pane, `clear-history` (resets modes first, cmd-capture-pane.c:407).

On cancel tmux restores the live pane screen; the pane's `searchstr` is retained on the
pane struct so a later copy mode can repeat it.

### 2.4 The position indicator

Drawn by `window_copy_write_line` only on screen row 0, only when the screen has >1 row
(`s->rupper < s->rlower`) and `hide_position` is false (window-copy.c:5211). Content is
the window option `copy-mode-position-format` **[new — classic tmux hardcoded
`[%u/%u]`]**, default:

```
#[align=right]#{t/p:top_line_time}#{?#{e|>:#{top_line_time},0}, ,}
[#{copy_position}/#{copy_position_limit}]
#{?search_timed_out, (timed out),#{?search_count, (#{search_count}#{?search_count_partial,+,} results),}}
```

with `copy_position = data->oy` (lines scrolled up) and `copy_position_limit = hsize`
(window-copy.c:1105-1114). So the classic rendering is right-aligned `[oy/hsize]`, plus
`(N results)` / `(N+ results)` / `(timed out)` after a search. Styled with
`copy-mode-position-style` (default `#{E:mode-style}` → the `mode-style` option). The
indicator row is *overlaid* on the top line of content. It is repainted after every
command — even "nothing" commands redraw line 0 to refresh it (:3767-3775).

---

## 3. Complete default key tables (key-bindings.c:556-723)

`send -X <cmd>` abbreviated to just the command. `command-prompt` args shown compactly.
`-P` on command-prompt = show the prompt at the pane **[new]**; `-1` = single-key input;
`-N` = numeric-only input; `-i` = incremental (fires callback on every keystroke);
`-T search` = search prompt type (own history).

### 3.1 `copy-mode` table (emacs)

| Key | Command |
|---|---|
| `C-Space` | `begin-selection` |
| `C-a` | `start-of-line` |
| `C-b` | `cursor-left` |
| `C-c` | `cancel` |
| `C-e` | `end-of-line` |
| `C-f` | `cursor-right` |
| `C-g` | `clear-selection` |
| `C-k` | `copy-pipe-end-of-line-and-cancel` |
| `C-l` | `recentre-top-bottom` **[new]** |
| `C-n` | `cursor-down` |
| `C-p` | `cursor-up` |
| `C-r` | `command-prompt -P -T search -ip'(search up)' -I'#{pane_search_string}' { send -X search-backward-incremental -- '%%' }` |
| `C-s` | `command-prompt -P -T search -ip'(search down)' -I'#{pane_search_string}' { send -X search-forward-incremental -- '%%' }` |
| `C-v` | `page-down` |
| `C-w` | `copy-pipe-and-cancel` |
| `Escape`, `C-[` | `cancel` |
| `Space` | `page-down` |
| `,` | `jump-reverse` |
| `;` | `jump-again` |
| `F` | `command-prompt -P -1p'(jump backward)' { send -X jump-backward -- '%%' }` |
| `N` | `search-reverse` |
| `P` | `toggle-position` **[new]** |
| `R` | `rectangle-toggle` |
| `T` | `command-prompt -P -1p'(jump to backward)' { send -X jump-to-backward -- '%%' }` |
| `X` | `set-mark` |
| `f` | `command-prompt -P -1p'(jump forward)' { send -X jump-forward -- '%%' }` |
| `g` | `command-prompt -P -p'(goto line)' { send -X goto-line -- '%%' }` |
| `n` | `search-again` |
| `q` | `cancel` |
| `r` | `refresh-toggle` **[new]** |
| `t` | `command-prompt -P -1p'(jump to forward)' { send -X jump-to-forward -- '%%' }` |
| `Home` | `start-of-line` |
| `End` | `end-of-line` |
| `NPage` | `page-down` |
| `PPage` | `page-up` |
| `Up`/`Down`/`Left`/`Right` | `cursor-up`/`cursor-down`/`cursor-left`/`cursor-right` |
| `M-1`..`M-9` | `command-prompt -P -Np'(repeat)' -I<digit> { send -N '%%' }` |
| `M-<` | `history-top` |
| `M->` | `history-bottom` |
| `M-R` | `top-line` |
| `M-b` | `previous-word` |
| `C-M-b` | `previous-matching-bracket` |
| `M-f` | `next-word-end` |
| `C-M-f` | `next-matching-bracket` |
| `M-l` | `cursor-centre-horizontal` **[new]** |
| `M-m` | `back-to-indentation` |
| `M-r` | `middle-line` |
| `M-v` | `page-up` |
| `M-w` | `copy-pipe-and-cancel` |
| `M-x` | `jump-to-mark` |
| `M-{` / `M-}` | `previous-paragraph` / `next-paragraph` |
| `M-Up` / `M-Down` | `halfpage-up` / `halfpage-down` |
| `C-Up` / `C-Down` | `scroll-up` / `scroll-down` |
| `M-C-Up` / `M-C-Down` | `previous-prompt` / `next-prompt` **[new]** |
| `MouseDown1Pane` | `select-pane` |
| `MouseDrag1Pane` | `select-pane; send -X begin-selection` |
| `MouseDragEnd1Pane` | `copy-pipe-and-cancel` |
| `WheelUpPane` | `select-pane; send -N5 -X scroll-up` |
| `WheelDownPane` | `select-pane; send -N5 -X scroll-down` |
| `DoubleClick1Pane` | `select-pane; send -X select-word; run -d0.3; send -X copy-pipe-and-cancel` |
| `TripleClick1Pane` | `select-pane; send -X select-line; run -d0.3; send -X copy-pipe-and-cancel` |

Note: classic tmux ≤3.5 additionally bound emacs `C-k` to `copy-end-of-line` (no pipe);
this master pipes. Historic `C-w`/`M-w` were `copy-selection-and-cancel` before
`copy-pipe-and-cancel` became the default (3.2+); the pipe variants behave identically
when `copy-command` is empty.

### 3.2 `copy-mode-vi` table

| Key | Command |
|---|---|
| `#` | `send -FX search-backward -- '#{copy_cursor_word}'` (search word under cursor, up) |
| `*` | `send -FX search-forward -- '#{copy_cursor_word}'` |
| `C-b` / `C-f` | `page-up` / `page-down` |
| `C-c` | `cancel` |
| `C-d` / `C-u` | `halfpage-down` / `halfpage-up` |
| `C-e` / `C-y` | `scroll-down` / `scroll-up` |
| `C-h` | `cursor-left` |
| `C-j`, `Enter` | `copy-pipe-and-cancel` |
| `C-v` | `rectangle-toggle` |
| `Escape`, `C-[` | `clear-selection` (**not** cancel) |
| `Space` | `begin-selection` |
| `$` | `end-of-line` |
| `,` / `;` | `jump-reverse` / `jump-again` |
| `/` | `command-prompt -P -T search -p'(search down)' { send -X search-forward -- '%%' }` |
| `?` | `command-prompt -P -T search -p'(search up)' { send -X search-backward -- '%%' }` |
| `0` | `start-of-line` |
| `1`..`9` | `command-prompt -P -Np'(repeat)' -I<digit> { send -N '%%' }` |
| `:` | `command-prompt -P -p'(goto line)' { send -X goto-line -- '%%' }` |
| `A` | `append-selection-and-cancel` |
| `B` | `previous-space` |
| `D` | `copy-pipe-end-of-line-and-cancel` |
| `E` | `next-space-end` |
| `F` | `command-prompt -P -1p'(jump backward)' { send -X jump-backward -- '%%' }` |
| `G` | `history-bottom` |
| `H` / `M` / `L` | `top-line` / `middle-line` / `bottom-line` |
| `J` / `K` | `scroll-down` / `scroll-up` |
| `N` | `search-reverse` |
| `P` | `toggle-position` **[new]** |
| `T` | `command-prompt -P -1p'(jump to backward)' { send -X jump-to-backward -- '%%' }` |
| `V` | `select-line` |
| `W` | `next-space` |
| `X` | `set-mark` |
| `^` | `back-to-indentation` |
| `b` | `previous-word` |
| `e` | `next-word-end` |
| `f` | `command-prompt -P -1p'(jump forward)' { send -X jump-forward -- '%%' }` |
| `g` | `history-top` |
| `h`/`j`/`k`/`l` | `cursor-left`/`cursor-down`/`cursor-up`/`cursor-right` |
| `n` | `search-again` |
| `o` | `other-end` |
| `q` | `cancel` |
| `r` | `refresh-toggle` **[new]** |
| `t` | `command-prompt -P -1p'(jump to forward)' { send -X jump-to-forward -- '%%' }` |
| `v` | `rectangle-toggle` |
| `w` | `next-word` |
| `z` | `scroll-middle` |
| `{` / `}` | `previous-paragraph` / `next-paragraph` |
| `%` | `next-matching-bracket` |
| `Home` / `End` | `start-of-line` / `end-of-line` |
| `BSpace` | `cursor-left` |
| `NPage` / `PPage` | `page-down` / `page-up` |
| `Up`/`Down`/`Left`/`Right` | cursor movement |
| `M-x` | `jump-to-mark` |
| `C-Up` / `C-Down` | `scroll-up` / `scroll-down` |
| Mouse (`MouseDown1Pane`, `MouseDrag1Pane`, `MouseDragEnd1Pane`, `WheelUp/DownPane` `-N5`, Double/TripleClick) | identical to the emacs table |

### 3.3 Repeat counts (`-N`)

`wme->prefix` starts at 1. `send-keys -N <n> [-X cmd]` sets it
(cmd-send-keys.c:183-198); when `-X` follows (or no keys are given) the count applies to
the next mode command. `window_copy_command` resets `wme->prefix = 1` after **every**
command (window-copy.c:3761). The digit bindings (emacs `M-1..9`, vi `1..9`) open a
numeric prompt `(repeat)` pre-filled with the digit; on Enter it runs `send -N <typed>`,
so e.g. `5j` in vi is typed as `5`, Enter is implicit only if you keep typing digits then
press a movement key — actually the prompt accepts more digits, and the *next* key after
Enter gets the count. Commands that honor the count do so by looping `np` times (cursor
moves, words, jumps, pages, search-again, scroll). `other-end` uses `np % 2` (:2183).
`copy-end-of-line`/`copy-line` extend `np-1` lines down (:1390, :1478).

### 3.4 Prompts inside copy mode

All prompt-using bindings run `command-prompt` variants; the resulting command is
dispatched back into the mode with `send -X`. Consequences for a clone:

- goto line: free-text numeric prompt `(goto line)` → `goto-line <s>`.
- jump: single-char prompts `(jump forward)` etc. → `jump-forward <c>`.
- repeat: numeric prompt `(repeat)` → `send -N <n>`.
- vi `/` `?`: normal prompts `(search down)`/`(search up)` → `search-forward <s>` (regex).
- emacs `C-s` `C-r`: **incremental** prompts pre-filled with `#{pane_search_string}` →
  `search-forward-incremental '%%'` fired on every keystroke with a prefix character:
  `=` (unchanged direction/first), `+` (C-s pressed inside prompt = flip to down),
  `-` (C-r inside prompt = flip to up) — see prompt.c:1087,1330-1345 and the consumer
  window-copy.c:2837-2859/2894-2915. Empty text clears the marks. While the prompt is
  open, C-r/C-s with an empty buffer recall the previous search (`pr->last`).

---

## 4. The copy-mode command set (window-copy.c:3107-3686)

Dispatch: `window_copy_command` (:3688). Every command returns one of
`NOTHING`/`REDRAW`/`CANCEL` (:213). Each table entry also has a `clear` policy for
search-match highlighting (:219):

- `CLEAR_ALWAYS` — running this command removes search highlights.
- `CLEAR_EMACS_ONLY` — removes them only when `mode-keys` is emacs (vi keeps highlights
  while moving; emacs clears them on any cursor motion).
- `CLEAR_NEVER` — keeps highlights.

The clear step is skipped entirely for commands whose name starts with `search-`
(:3749). When highlights are cleared this way, `searchx/y` are invalidated (:3756).

Full command list (args in `[]`; count = accepted positional args):

| Command | Args | Clear | Effect |
|---|---|---|---|
| `append-selection` | | ALWAYS | Append selection to top automatic buffer, clear selection (§5.4) |
| `append-selection-and-cancel` | | ALWAYS | ... and exit copy mode |
| `back-to-indentation` | | ALWAYS | First non-blank of (wrapped) line |
| `begin-selection` | | ALWAYS | Start selection at cursor; with mouse event, starts drag |
| `bottom-line` | | EMACS_ONLY | Move to bottom visible line (cx=0) |
| `cancel` | | ALWAYS | Exit copy mode |
| `clear-selection` | | ALWAYS | Drop selection (stays in mode) |
| `copy-end-of-line[-and-cancel]` | `[-CP] [prefix]` | ALWAYS | Copy cursor→EOL (+count-1 lines) |
| `copy-pipe-end-of-line[-and-cancel]` | `[-CP] [command] [prefix]` | ALWAYS | Same, also pipe |
| `copy-line[-and-cancel]` | `[-CP] [prefix]` | ALWAYS | Copy whole line(s) |
| `copy-pipe-line[-and-cancel]` | `[-CP] [command] [prefix]` | ALWAYS | Same, also pipe |
| `copy-selection` / `-no-clear` / `-and-cancel` | `[-CP] [prefix]` | ALWAYS / NEVER / ALWAYS | §5.3 |
| `copy-pipe` / `-no-clear` / `-and-cancel` | `[-CP] [command] [prefix]` | ALWAYS / NEVER / ALWAYS | §5.3 |
| `pipe` / `-no-clear` / `-and-cancel` | `[command]` | ALWAYS / NEVER / ALWAYS | Pipe only, no buffer |
| `cursor-up/-down/-left/-right` | | EMACS_ONLY | Repeatable cursor motion |
| `cursor-down-and-cancel` | | ALWAYS | Down; cancel if pinned at bottom with oy=0 |
| `cursor-centre-vertical/-horizontal` | | EMACS_ONLY | **[new]** centre cursor on screen |
| `end-of-line` / `start-of-line` | | EMACS_ONLY | §6.2 |
| `goto-line` | `<line>` | EMACS_ONLY | `oy = clamp(line, 0, hsize)` (:4824-4853); i.e. the argument is "lines back into history"; 0 = bottom |
| `halfpage-up` / `halfpage-down[-and-cancel]` | | EMACS_ONLY / ALWAYS | §6.5 |
| `history-top` / `history-bottom` | | EMACS_ONLY | oy=hsize, cx=cy=0 / oy=0, bottom line, cx=line end (:1792-1836) |
| `jump-forward/backward/to-forward/to-backward` | `<char>` | EMACS_ONLY | §6.7 |
| `jump-again` / `jump-reverse` | | EMACS_ONLY | Repeat / reverse last jump |
| `jump-to-mark` | | ALWAYS | Swap cursor and mark (§6.8) |
| `middle-line` / `top-line` / `bottom-line` | | EMACS_ONLY | cx=0, cy = (sy-1)/2 / 0 / sy-1 |
| `next-matching-bracket` / `previous-matching-bracket` | | ALWAYS | `{[( )]}` matching, vi/emacs nuances (:1908-2112) |
| `next-paragraph` / `previous-paragraph` | | EMACS_ONLY | §6.6 |
| `next-word` / `next-word-end` / `previous-word` | | EMACS_ONLY | word-separators option (§6.4) |
| `next-space` / `next-space-end` / `previous-space` | | EMACS_ONLY | same with separators = "" |
| `next-prompt` / `previous-prompt` | `[-o]` | ALWAYS | **[new]** OSC-133 prompt lines; `-o` = start-of-output lines |
| `other-end` | | EMACS_ONLY | Swap dragged end (count%2) |
| `page-up` / `page-down[-and-cancel]` | | EMACS_ONLY / ALWAYS | §6.5 |
| `rectangle-on/off/toggle` | | ALWAYS | §5.2 |
| `refresh-on/off/toggle` | | NEVER | **[new]** live-refresh timer |
| `recentre-top-bottom` | | ALWAYS | **[new]** emacs C-l cycle middle→top→bottom |
| `scroll-up` / `scroll-down[-and-cancel]` | | EMACS_ONLY / ALWAYS | one-line view scroll (§6.5) |
| `scroll-top/middle/bottom` | | ALWAYS | scroll cursor line to top/middle/bottom (vi `z` = scroll-middle) |
| `scroll-exit-on/off/toggle` | | ALWAYS | **[new]** runtime toggle of `-e` behavior |
| `scroll-to-mouse` | `[-e]` | EMACS_ONLY | **[new]** scrollbar |
| `search-forward` / `search-backward` | `[<for>]` | ALWAYS | regex search (§7) |
| `search-forward-text` / `search-backward-text` | `[<for>]` | ALWAYS | plain-text search |
| `search-forward-incremental` / `search-backward-incremental` | `<for>` | ALWAYS | incremental (text) search |
| `search-again` / `search-reverse` | | ALWAYS | repeat / repeat-flipped |
| `select-line` / `select-word` | | ALWAYS | §5.2 |
| `selection-mode` | `[char|word|line]` | — | **[new]** set selflag directly |
| `set-mark` | | ALWAYS | mark := cursor, showmark=1 |
| `stop-selection` | | ALWAYS | cursordrag=NONE, keep selection frozen |
| `toggle-position` | | NEVER | **[new]** show/hide the position indicator |

Commands taking `-C` suppress setting the system clipboard, `-P` suppresses creating a
paste buffer (:1372-1373 and everywhere the pattern repeats). `search-*` accept `-F` to
format-expand the argument first (used by vi `*`/`#`, :1250).

If the client is read-only, only commands flagged `WINDOW_COPY_CMD_FLAG_READONLY`
(purely navigational ones) are permitted (:3722-3729).

`NOTHING` still redraws screen line 0 so the position indicator stays current (:3767).

---

## 5. Selection

### 5.1 Model

`begin-selection` (`window_copy_start_selection`, :5473): sets `selx/sely` :=
cursor (absolute coords), `endsel := sel`, `cursordrag = CURSORDRAG_ENDSEL`. From then
on **every cursor movement** calls `window_copy_update_selection` →
`window_copy_synchronize_cursor` (:5395), which copies the cursor into whichever end
`cursordrag` says is live. `stop-selection` freezes both ends (`CURSORDRAG_NONE`) —
selection remains, cursor roams free. `other-end` (:6027) toggles
ENDSEL↔SEL and teleports the cursor to the *other* endpoint, scrolling the view if that
endpoint is off-screen (above → `oy = hsize - sely`, below → pin to bottom, :6064-6072).

The selection endpoints are **absolute** (history-anchored), so scrolling does not move
them. Rendering clips them to the visible screen per frame
(`window_copy_adjust_selection`, :5489: rows above the view clamp to row 0 col 0, rows
below clamp to the last row/col — for rect selection only the row is clamped). If both
ends fall off the same side, the selection is hidden that frame (:5560-5563).

Because the backing snapshot never mutates while a selection exists (§1.1: refresh is
suppressed when `sel != NULL`), live output cannot shift a selection in this master.
In classic tmux the backing was always frozen, same net effect.

### 5.2 Character / rectangle / word / line

- **Character-wise** (default, `selflag = SEL_CHAR`).
- **Rectangle** (`rectangle-toggle`, `window_copy_rectangle_set` :6672): flips
  `rectflag`, clears line-select mode, clamps cx to the line, re-renders. Geometry
  (screen.c `screen_check_selection` :520): rows between the two ends inclusive; the
  included columns are `[min(selx,cx) .. max(selx,cx)]`, but the **cursor column itself
  is excluded in emacs mode** and **included in vi mode** — implemented both in the
  highlight (`sel->modekeys`, screen.c:578-616) and in the copied text
  (window-copy.c:5668-5695: emacs `lastex = data->cx`, vi `data->cx + 1`). While a
  rectangle selection is active the cursor may sit one past EOL ("virtualedit", :5427).
- **Word select** (`select-word`, :2448; double-click): `selflag=SEL_WORD`,
  `lineflag=LINE_SEL_LEFT_RIGHT`; anchor word = word under cursor computed with
  `previous-word` + `next-word-end` using the `word-separators` *session* option;
  single-character words handled specially (:2470-2485). Dragging (or moving the
  cursor) extends by whole words on either side: `window_copy_synchronize_cursor_end`
  SEL_WORD branch (:5333-5358) — moving above/left of the anchor extends to the start of
  the word under the cursor (right-to-left), otherwise to the end of the word under the
  cursor.
- **Line select** (`select-line`, :2419; triple-click): `selflag=SEL_LINE`; selects the
  whole (unwrapped) display line; count extends downward; dragging extends by whole
  lines (:5360-5381). Cursor vertical movement in line mode snaps x to line
  start/end (`lineflag` handling in cursor_up/down, :6186-6202/:6258-6274).
- **`selection-mode char|word|line`** **[new]** sets `selflag` directly.

`clear-selection` also resets `selflag=SEL_CHAR`, `lineflag=NONE`, clamps the cursor
back inside the line for vi (:5913-5929).

### 5.3 What gets copied (`window_copy_get_selection`, :5607)

- If there is **no selection**, the copy commands copy the **search match under the
  cursor** if any (`window_copy_match_at_cursor`, :4874; one position after the match
  also counts, :4890-4893). Otherwise nothing.
- Linear selection: line 1 from `firstsx`, middle lines full width, last line to
  `lastex`; **emacs excludes the bottom-right-most cell, vi includes it**
  (:5661-5704).
- Per line (`window_copy_copy_line`, :5850): trailing whitespace is stripped
  (`grid_line_length` trims trailing spaces, grid.c:1643) **unless the line is wrapped**
  (wrapped lines use `cellsize` and get **no** `\n` appended → hard-wrapped lines are
  joined back together, :5866-5910). Padding cells (wide-char tails) are skipped; tab
  cells emit `\t`; ACS chars are translated to their Unicode strings.
- The final trailing `\n` is removed in emacs mode, or in vi when the selection does not
  extend past the last line's content (:5719-5724).
- Rectangle: per-row `[restsx..restex)` slices per §5.2.

### 5.4 Copy destinations (`window_copy_copy_buffer`, :5729)

1. If `set-clipboard` ≠ off **and** the command didn't pass `-C`: write OSC 52
   (`screen_write_setselection` → each attached client's tty if it has the `Ms`
   capability, tty.c:2129-2148) and fire the `pane-set-clipboard` notification.
2. Unless the command passed `-P`: `paste_add(prefix, buf, len)` — creates a new
   **automatic** paste buffer. `prefix` is the optional positional arg of the copy
   command (format-expanded); buffer is named `<prefix><n>` (default prefix `buffer`).
3. `copy-pipe*` additionally spawns `<command>` (or, if empty/absent, the
   `copy-command` server option; if both empty, no pipe) as a job and writes the
   selection to its stdin (`window_copy_pipe_run`, :5758-5775). Piping happens **in
   addition to** buffer/clipboard, not instead.
4. `append-selection` (:5816): takes the **newest automatic buffer** (`paste_get_top`),
   concatenates old-data + selection, and `paste_set`s it back under the same name.
   Subtle upstream quirk: `paste_set` marks the result **non-automatic** (paste.c:303),
   so after one append the buffer no longer counts against `buffer-limit` and is no
   longer found by `paste_get_top` (a second `append-selection` therefore appends to a
   *different, newer* automatic buffer if one exists, else creates `buffer<n>`). It
   still sets the clipboard when `set-clipboard` is on.

---

## 6. Movement semantics

Most motions go through `struct grid_reader` (grid-reader.c) operating on the backing
grid in absolute coordinates, then re-acquire the on-screen cursor with
`window_copy_acquire_cursor_up/down` (:6862-6915) which scrolls if the target row is
off-screen.

### 6.1 Cursor limits and padding

- vi mode: cursor max column is `len-1` (on the last char); emacs: `len` (one past)
  — `window_copy_cursor_limit` (:5948-5962). Rectangle selection lifts this
  ("allow_onemore").
- Left/right skip wide-character padding cells (grid-reader.c:66-96).
- `cursor-left` at column 0 wraps to the end of the previous line **only if** that
  previous line is wrapped into this one (grid-reader.c:89-93 with wrap=1 from
  window-copy.c:6096 — actually copy mode passes wrap=1, so it always wraps up onto the
  previous row); `cursor-right` at line end wraps to column 0 of the next row
  (grid-reader.c:63-65).

### 6.2 Start/end of line, indentation

- `start-of-line`: moves to column 0 of the **logical** line — walks up through
  wrapped predecessors first (grid_reader_cursor_start_of_line wrap=1,
  grid-reader.c:132-141).
- `end-of-line`: walks down through wrap continuations, then to `grid_line_length`
  (trailing-space-trimmed); with an active rectangle selection it goes to the full grid
  width instead (window-copy.c:6016-6019). In vi the result is clamped to len-1 via
  cursor_limit.
- `back-to-indentation`: start of logical line, then first cell that is not space, tab,
  or padding, scanning across wraps (grid-reader.c:414-441).

### 6.3 Vertical movement and the sticky column

`cursor-up`/`cursor-down` (:6125-6275) remember `lastcx` when the cursor is not at the
line end. After moving, if the cursor was at/past its remembered content width or past
the new line's length, it snaps to the new line's end (:6174-6184). Net effect (same as
vim): a cursor placed at EOL rides the line ends; a mid-line cursor keeps its column,
clamping on shorter lines and restoring on longer ones. At the top/bottom screen edge
they scroll the view by one instead (`window_copy_scroll_down/up(wme,1)`).

`scroll-up`/`scroll-down` (scroll_only=1 path): move the *view* one line; in **vi**
mode the cursor first shifts to keep pointing at the same absolute line when possible
(:6145-6148, :6225-6228); the cursor is pulled along only when it would leave the
screen.

### 6.4 Words

- `WHITESPACE` is `"\t "` (tmux.h:662). Tabs count with their display width when
  skipping (grid_in_set returns the tab's width, grid.c:1664-1684).
- `word-separators` (session option) default:
  `!"#$%&'()*+,-./:;<=>?@[\]^`{|}~` — every printable non-alphanumeric ASCII char
  **except underscore** (options-table.c:1262-1270). So a "word" is a run of
  alphanumerics+`_`, and a run of separator punctuation is also a word (vim-like
  3-class model: whitespace / separators / other), implemented in
  `grid_reader_cursor_next_word{,_end}` and `_previous_word`
  (grid-reader.c:192-339).
- `next-space`/`next-space-end`/`previous-space` run the same code with separators = ""
  → whitespace-delimited WORDs (vi `W`/`E`/`B`).
- `next-word-end` differs by mode (window-copy.c:6407-6413/6437-6443): **vi** moves
  right one first (if not on whitespace), finds the word end, then steps **left one** so
  the cursor lands *on* the last character (vim `e`); **emacs** (`M-f`) lands one
  *past* the end.
- `previous-word`: emacs stops at an end-of-line boundary if the previous line ends in
  whitespace (`stop_at_eol=1`, :6482-6485); vi crosses lines freely. Movement across a
  line boundary only continues into the previous line if... (grid-reader.c:283-339:
  always crosses up, but the "beginning of word" backtrack only crosses wrapped
  boundaries).
- Words never break across *wrapped* line boundaries: wrapped lines are treated as one
  logical line (`xx = gd->sx - 1` when wrapped, grid-reader.c:198-201).

### 6.5 Paging and scrolling amounts

`window_copy_pageup1`/`pagedown1` (:875-983):

- full page `n = sy - 2` (screen rows minus two, min 1 when sy ≤ 2);
- half page `n = sy / 2`;
- `oy` clamps to `[0, hsize]`; when clamped at the top, the *cursor* moves up the
  remaining rows instead (:899-906).
- The sticky-column rule of §6.3 applies (lastcx restore + EOL snap).
- After any scroll, search marks are recomputed for the visible region (:916-917) and
  the selection re-synced.
- `scroll-up`/`scroll-down` = 1 line each (wheel bindings send `-N5` for 5 lines/notch).
- `scroll-top/middle/bottom` reposition the view so the cursor row lands on
  top/middle/bottom without moving the cursor within its line (:1626-1698).

### 6.6 Paragraphs

`previous-paragraph` (:985): walk up over blank lines, then up over non-blank lines
— lands on the blank line above the paragraph (cx=0). `next-paragraph` (:1002): walk
down over blank lines, then over non-blank lines; lands at the end (`ox = line length`)
of the first blank line after the paragraph. "Blank" = `grid_line_length == 0`.

### 6.7 Jump to character (f/F/t/T ; ,)

State: `jumptype` + `jumpchar` (UTF-8, so multibyte chars work). Set by
`jump-forward <c>` (f), `jump-backward` (F), `jump-to-forward` (t: stops one column
*before* the char), `jump-to-backward` (T: one column after). `jump-again` (`;`)
repeats with the same type/char; `jump-reverse` (`,`) runs the opposite type
(:1838-1892). The scan is confined to the current **logical** line: it crosses wrapped
row boundaries but stops at a hard line end (grid_reader_cursor_jump/-_back,
grid-reader.c:357-410). `t` starts scanning at cx+2 so repeated `;` makes progress
(:6328); `T` symmetric with two lefts (:6359-6360). All honor repeat counts.

### 6.8 Mark

`set-mark` (X): `mx/my := cursor` (absolute), `showmark=1` — the marked *line* is
restyled with `copy-mode-mark-style` and the marked *cell* is shown reverse
(:4934-4946). `jump-to-mark` (M-x): swaps cursor and mark (:6839-6860), so M-x M-x
returns.

### 6.9 Matching bracket (`%` vi, C-M-f/C-M-b emacs)

Sets: open `{[(`, close `}])` (:1914, :2000). `previous-matching-bracket`: if on a
closing bracket, walk back to its match; in emacs, if not on one, try one cell left,
else behave like `previous-word` over the close set. `next-matching-bracket` in vi: if
on a *closing* bracket first tries `previous-matching-bracket` (returning to start on
failure) — this makes vi `%` toggle between the pair; scans forward to EOL for a
bracket otherwise; emacs behaves like `next-word-end` when not on a bracket.

---

## 7. Search

### 7.1 Commands and state

- `search-forward [<re>]` / `search-backward [<re>]` — **regex** (POSIX extended)
  search down/up. If the string contains none of `^$*+()?[].\` it silently degrades to
  plain-text (:4476-4477).
- `search-forward-text` / `search-backward-text` — plain text.
- `search-forward-incremental` / `search-backward-incremental` — plain text, driven by
  an incremental prompt; argument arrives with a `=`/`+`/`-` prefix char selecting the
  direction (see §3.4; `+` = down, `-` = up, `=` = keep the prompt's base direction:
  :2837-2859, :2894-2915). While the prompt is open, a changed string restarts from the
  pre-search cursor (`searchx/y/o` saved on first keystroke, restored on change,
  :2821-2832). Empty string clears highlighting.
- `search-again` (n) repeats in `searchtype` direction; `search-reverse` (N) runs the
  opposite direction *without* changing the stored direction (:2386-2417). Because vi
  `/`=down `?`=up set the direction, vi n/N behave exactly like vim. Both honor counts.
- The last search string is stored on the **pane** (`wp->searchstr`, :4492) so
  re-entering copy mode keeps `n` working and `#{pane_search_string}` pre-fills the
  emacs incremental prompt.

### 7.2 Case sensitivity

`cis = window_copy_is_lowercase(str)` (:4305): if the search string is entirely
lowercase, the search is case-insensitive (regex: `REG_ICASE`); any uppercase char makes
it exact. (= vim "smartcase", always on; no option.)

### 7.3 Mechanics, wrapping, and cursor placement

Text search compares cell-by-cell with wide-char and tab awareness
(`window_copy_search_lr/rl`, :3838-3924); regex search stringifies whole logical
(wrap-joined) lines up to `WINDOW_COPY_SEARCH_MAX_LINE = 2000` cells (:345) and maps
byte offsets back to cells (`window_copy_cstrtocellpos`, :4177). Matches spanning
wrapped rows work; backward regex searches extend across earlier wrapped rows to find
the longest overlapping match (`window_copy_search_back_overlap`, :4320).

Search start position (:4496-4536): forward searches in **vi** start one cell right of
the cursor (or after the current match if the cursor is inside one); backward searches
start one cell left. `wrap-search` (window option, default on) wraps at grid
top/bottom and rescans once (:4427-4432).

Cursor placement on a hit (:4563-4597): **vi** — cursor at the match **start** for both
directions. **emacs** — forward search leaves the cursor just **after** the match end;
backward search at the match start.

### 7.4 Highlighting and the match count

After a successful jump, `window_copy_search_marks` (:4672) fills `searchmark`, a
byte-array of `sx*sy` covering **only the visible screen**; every match cell gets a
generation byte so adjacent matches remain distinct (:4641-4670). Marks are recomputed
on every scroll/reposition while a search is live (e.g. :916, :6561).

The first search scans the **entire history** to count matches, but with time limits
(:4716-4790): total budget `WINDOW_COPY_SEARCH_ALL_TIMEOUT = 200`ms for the full-grid
pass — if exceeded, it falls back to marking just the visible region and *rounds* the
count: >1000 → "1000+", >100 → "100+", >10 → "10+", else no count
(`searchcount`, `searchmore` → formats `search_count`, `search_count_partial`). A
10-second overall budget (`WINDOW_COPY_SEARCH_TIMEOUT`) sets `data->timeout`, shows
"(timed out)" and disables further marking until the next explicit search.

Styling (`window_copy_update_style`, :4923): all match cells get
`copy-mode-match-style`; the **current** match — the one containing the cursor (in
emacs, when searching forward, the cell *left* of the cursor counts because the cursor
sits after the match, :4959-4967) — gets `copy-mode-current-match-style`. The count
indicator is rendered by the position-indicator format (§2.4).

Every non-`search-*` command clears the highlight according to its `clear` policy
(§4) — in vi, plain motions keep the highlight; in emacs they clear it.

`copy_cursor_word`/`copy_cursor_line`/`search_match` formats expose the word/line/match
at the cursor (:1157-1162); vi `*`/`#` are built on `copy_cursor_word`.

---

## 8. Scrollback / history

### 8.1 history-limit

- `history-limit` is a **session** option (default **2000**, options-table.c:861); it is
  baked into a pane's grid as `gd->hlimit` **at pane creation** (spawn.c:258). Changing
  the option live triggers `session_update_history` (options.c:1442 →
  session.c:764-787): every pane's `hlimit` is updated and history over the limit is
  trimmed immediately (`grid_collect_history(gd, 1)`).
- Trimming during normal output (`grid_collect_history`, grid.c:427-453): whenever a
  line is about to scroll into history and `hsize >= hlimit`, tmux frees a **batch** of
  the oldest `hlimit/10` lines (min 1) — history oscillates between `hlimit` and
  ~`hlimit*0.9`, it is not trimmed one-by-one. Called from the scroll paths
  (grid-view.c:85,110).
- `hscrolled` counts lines scrolled into history since last clear (used by scrollbars
  and clamped when history is collected).
- Monotonic counters `scroll_added` / `scroll_collected` / `scroll_generation` **[new]**
  exist solely so copy mode can incrementally sync its backing clone (§1.1).

### 8.2 clear-history

`clear-history [-H] [-t pane]` is implemented in cmd-capture-pane.c:406-413: it first
**resets all modes on the pane** (kicking out any copy-mode viewer), then
`grid_clear_history` (grid.c:492-503: frees all history lines, `hsize = 0`,
`hscrolled = 0`, bumps `scroll_generation`). `-H` also clears stored hyperlinks.

### 8.3 scroll-on-clear

Window/pane option, default **on** (options-table.c:1680). When the application clears
the *whole* screen (ED 2 `\e[2J`, and the clear-from-cursor variant when it covers the
whole screen) on a history-enabled grid, the visible contents are first **scrolled into
history** instead of destroyed (`grid_view_clear_history`, screen-write.c:1921-1929 and
:2073-2078). With the option off, clearing discards the content. (`clear` in a shell
therefore doesn't lose scrollback by default.)

### 8.4 Alternate screen interaction

`screen_alternate_on` (screen.c:678-710): saves the visible grid to `saved_grid`,
clears the view, and **clears the GRID_HISTORY flag** — while an app is on the alternate
screen, nothing new is added to history, but the *existing* primary-screen scrollback
remains in the grid. Consequences:

- You **can** enter copy mode over an alt-screen app (prefix-`[` works); the snapshot
  shows the alt screen's current contents with the old primary scrollback above it.
- The mouse wheel does **not** enter copy mode over an alt-screen app — the root
  `WheelUpPane` binding checks `#{alternate_on}` and forwards the event to the app
  instead (key-bindings.c:506).
- `capture-pane -a` captures the saved (primary) screen while the alternate screen is
  active (`wp->base.saved_grid`, cmd-capture-pane.c:258-266); errors without `-q` if no
  alternate screen.
- `screen_alternate_off` restores the saved grid and re-enables history.

### 8.5 view-mode

`window_view_mode` backs `show-messages`, `list-keys` etc. Content is *written* into a
fresh backing screen with `hlimit = UINT_MAX` via `window_copy_vadd` (:702-751);
`scroll_exit` semantics do not apply, but `cursor-down-and-cancel` (used when a pager
reaches the bottom) does. Line numbers are disabled (:653).

---

## 9. Paste buffers (`paste.c`)

### 9.1 Data model

Global (server-wide) set of buffers, in two red-black trees: by **name** and by
**time** (really by `order`, a monotonically increasing insertion counter — newest =
smallest in the time tree, paste.c:52-60). Buffer data is a byte array + length —
**not** NUL-terminated, may contain any bytes.

Each buffer: `name`, `data/size`, `automatic` flag, `order`, `created` (unix time).

- **Automatic buffers** are created by copy-mode copies, `set-buffer` without a name,
  `capture-pane` without `-b`, and OSC 52 from applications. Named ("assigned")
  buffers come from `set-buffer -b`, `set-buffer -n` renames, or `load-buffer -b`.
- `paste_get_top` (paste.c:108-121): the newest **automatic** buffer — named buffers
  are *skipped*. This is what `paste-buffer`/`delete-buffer`/`show-buffer` without
  `-b`, `append-selection`, and `set-buffer -n/-a` without `-b` operate on.
- `paste_get_name`: exact-name lookup.

### 9.2 Automatic naming and eviction

`paste_add(prefix, data, size)` (paste.c:156-199):

- Zero-size data is discarded silently.
- Name = `<prefix><index>` where prefix defaults to `"buffer"` and `index` is a global
  monotonically increasing counter (`paste_next_index`), skipping names that already
  exist. So: `buffer0`, `buffer1`, ... (no zero-padding). The counter never resets
  while the server lives, and is shared across prefixes (copying with a prefix `foo`
  after `buffer4` yields `foo5`).
- **Eviction first**: while `paste_num_automatic >= buffer-limit` (server option,
  default **50**), walk the time tree from the **oldest** end and free automatic
  buffers (`RB_FOREACH_REVERSE`, :171-176). **Named buffers are exempt** and never
  counted against the limit.
- The new buffer is marked automatic and gets the next `order`.

### 9.3 set-buffer / delete-buffer (cmd-set-buffer.c)

```
set-buffer    [-aw] [-b buffer-name] [-n new-buffer-name] [-t target-client] data
delete-buffer [-b buffer-name]
```

- `delete-buffer`: with `-b`, that buffer (error `unknown buffer: X` if missing);
  without, the newest automatic buffer (error `no buffer` if none). (:70-85)
- `set-buffer -n new`: renames `-b` (or top automatic). Renaming **clears the automatic
  flag** (paste.c:250-252) — a renamed buffer becomes named and eviction-exempt.
  Renaming onto an existing name frees the old holder (paste.c:242-243).
- `set-buffer -a`: append. Appending only happens when `-b` names an **existing**
  buffer; `-a` without `-b`, or with a new name, just creates a fresh buffer with the
  data (:113-117 — `pb` is only looked up from `-b`).
- With data and `-b name`: create/replace that named buffer (`paste_set`, paste.c:266:
  replaces any same-named buffer, marks it **non-automatic**). With data and no `-b`:
  `paste_add` → automatic buffer.
- `-w`: also push the data to the target client's clipboard via OSC 52 (:127-128).
- Empty data string is a silent no-op (:110-111).
- Buffer names go through `clean_name` — invalid names error (`invalid buffer name`).

### 9.4 paste-buffer (cmd-paste-buffer.c)

```
paste-buffer [-dprS] [-s separator] [-b buffer-name] [-t target-pane]
```

Behavior (:57-132):

1. Buffer: `-b name` (error `no buffer X` if missing) else newest automatic buffer.
   No buffer at all → silently does nothing (no error).
2. Nothing is written if the pane's input is off (`PANE_INPUTOFF`).
3. **Line-ending translation**: the buffer is split at `\n`; each segment is written
   followed by the separator. Separator = `-s` value if given; else `"\n"` with `-r`;
   else `"\r"`. I.e. **by default every LF is rewritten to CR** (what Enter sends);
   `-r` disables the translation. A trailing `\n` in the buffer produces a trailing
   separator.
4. Each segment is **sanitised** through `utf8_stravisx(VIS_SAFE|VIS_NOSLASH)`
   (dangerous control bytes are octal-escaped) unless `-S` is given, which writes the
   raw bytes **[new flag]** (:108-111, :118-121).
5. `-p`: **bracketed paste** — the whole write is wrapped in `\e[200~` ... `\e[201~`
   **only if the target pane's screen has `MODE_BRACKETPASTE` set** (i.e. the
   application enabled DECSET 2004). Otherwise `-p` is a silent no-op wrapper
   (:97-98, :124-125). The default `]` binding is `paste-buffer -p`.
6. `-d`: delete the buffer after pasting (frees it even if the pane was input-off).

The root `MouseDown2Pane` binding pastes with `paste -p` (middle-click paste,
key-bindings.c:509).

### 9.5 list-buffers / choose-buffer / show-buffer

- `list-buffers [-F format] [-f filter] [-O order] [-r]` (cmd-list-buffers.c): prints
  one line per buffer, default template
  `#{buffer_name}: #{buffer_size} bytes: "#{buffer_sample}"`, sorted (default:
  creation/time order, newest first; `-O` name/size/creation **[new]**).
  `buffer_sample` (paste_make_sample, paste.c:331-348) is the first 200 cells,
  vis-escaped (octal/C-style, incl. tabs & newlines), with `...` appended when
  truncated.
- `choose-buffer` (prefix `=`, `choose-buffer -Z`) opens **buffer-mode**
  (window-buffer.c): a mode-tree list, default display format
  `#{t/p:buffer_created}: #{buffer_sample}`, sortable by time/name/size. Keys: Enter
  or `p` = paste selected (`paste-buffer -p -b '%%'`, :40), `d` = delete, `D` = delete
  tagged, `P` = paste tagged, `t`/`T`/`C-t` tag controls, `e` = open the buffer in an
  editor **[new]**, `q` = cancel. Empty buffer list → the command fails
  (`no buffers`).
- `show-buffer [-b name]` prints buffer contents (not studied in detail — trivial).

### 9.6 capture-pane (cmd-capture-pane.c)

```
capture-pane [-aCeFHJLMNpPqRT] [-b buffer-name] [-E end] [-S start] [-t pane]
```

- Destination: `-p` prints to stdout (control clients get it inline), else stores in a
  buffer: `-b name` → named buffer via `paste_set`, no `-b` → automatic buffer.
- Range: `-S`/`-E` line numbers relative to the visible screen (0 = top visible line;
  negative = into history; `-S -` = very start of history, `-E -` = very end). Values
  are clamped; swapped if reversed (:282-320).
- Source grid: default = pane's primary screen+history; `-a` = the **saved alternate
  screen** (error unless `-q`); `-M` **[new]** = the screen of the active mode (so you
  can capture what copy mode is showing).
- `-J` joins wrapped lines (no `\n` after lines with `GRID_LINE_WRAPPED`) **and**
  preserves trailing spaces; `-N` preserves trailing spaces without joining; `-T`
  strips trailing "empty cell" runs; `-e` includes SGR escape sequences; `-C` escapes
  non-printables as octal; `-L`/`-F` **[new]** prefix line numbers / line flags; `-H`
  **[new]** dumps hyperlinks; `-R` **[new]** dumps the raw grid metadata; `-P` captures
  only the *pending* (unparsed) input.

### 9.7 OSC 52 in both directions

- **Outgoing** (tmux → terminal clipboard): every copy writes OSC 52 to attached
  clients when `set-clipboard` is `on` (2) or `external` (1) — the check in copy paths
  is simply `!= 0` (window-copy.c:5738) — and the client's terminfo advertises `Ms`.
- **Incoming** (application inside a pane sets clipboard): `input_osc_52`
  (input.c:3268) — **only when `set-clipboard` is exactly `on`** (value 2,
  input.c:3231). The base64 payload is decoded, forwarded to client ttys, **and added
  as an automatic paste buffer** (`paste_add(NULL, ...)`, input.c:3286/3293). An OSC 52
  query (`?`) replies with the top paste buffer if `set-clipboard` is on.

---

## 10. Option defaults summary

| Option | Scope | Default | Notes (file:line in options-table.c) |
|---|---|---|---|
| `mode-keys` | window | `emacs` | choices emacs/vi (:1456) |
| `mode-style` | window | `noattr,bg=themeyellow,fg=themeblack` | classic tmux: `bg=yellow,fg=black`; the `theme*` names are **[new]** (:1464) |
| `word-separators` | session | ``!"#$%&'()*+,-./:;<=>?@[\]^`{|}~`` | punctuation minus `_` (:1262) |
| `history-limit` | session | `2000` | lines per pane, applied at pane creation; live change trims (:861) |
| `buffer-limit` | server | `50` | max **automatic** buffers (:291) |
| `copy-command` | server | `""` | fallback pipe command for `copy-pipe*` (:325) |
| `set-clipboard` | server | `external` (1) | off/external/on; `on` additionally lets apps' OSC 52 create buffers (:513) |
| `wrap-search` | window | `on` | (:1867) |
| `scroll-on-clear` | window+pane | `on` | full-screen clears push content into history (:1680) |
| `copy-mode-match-style` | window | `bg=themecyan,fg=themeblack` | classic: `bg=cyan,fg=black` (:1350) |
| `copy-mode-current-match-style` | window | `bg=thememagenta,fg=themeblack` | classic: `bg=magenta,fg=black` (:1359) |
| `copy-mode-mark-style` | window | `bg=themeyellow,fg=themeblack` | (:1368) |
| `copy-mode-selection-style` | window | `#{E:mode-style}` | **[new]** — classic used mode-style directly (:1398) |
| `copy-mode-position-style` | window | `#{E:mode-style}` | **[new]** (:1389) |
| `copy-mode-position-format` | window+pane | see §2.4 | **[new]** (:1377) |
| `copy-mode-line-numbers` | window | `off` | **[new]** off/default/absolute/relative/hybrid (:1425) |
| `copy-mode-(current-)line-number-style` | window | `fg=themelightgrey,dim` / `fg=themeyellow` | **[new]** (:1407,:1416) |
| `prompt-history-limit` | server | `100` | search/goto prompt history (:504) |

Style resolution: all `copy-mode-*-style` values are applied per redraw with
`style_apply` (window-copy.c:5173-5186), so `set -w copy-mode-match-style ...` takes
effect immediately; a `style_changed` mode hook re-renders the selection (:5314).

---

## 11. Windows / winmux applicability notes

- **OSC 52 / `Ms`**: Windows Terminal ≥ 1.11 supports OSC 52 writes, so the
  `set-clipboard` outgoing path is viable — winmux can emit
  `\e]52;;<base64>\a` to the attached client verbatim. The terminfo `Ms` gate has no
  ConPTY equivalent; a capability whitelist or a winmux option is the pragmatic
  substitute. Incoming OSC 52 (apps creating buffers) requires winmux's per-pane grid
  parser to intercept OSC 52 rather than forwarding it — gate on `set-clipboard on`
  exactly as tmux does.
- **`copy-command` / `copy-pipe`**: tmux runs the command through the job system
  (`/bin/sh -c`). On Windows the natural mapping is `cmd.exe /c` or the user's shell;
  piping to `clip.exe` is the idiomatic Windows clipboard integration and makes
  `copy-command "clip"` an easy documented recipe.
- **Line-ending translation in paste** matters *more* on Windows: PowerShell/cmd expect
  CR for "Enter", so tmux's default LF→CR rewrite (§9.4) is exactly right; keep `-r`
  (raw LF) and `-s` overrides.
- **Bracketed paste**: ConPTY forwards DECSET 2004 from apps; winmux's grid already
  tracks modes, so `MODE_BRACKETPASTE` gating is implementable 1:1.
- **`regex.h`**: tmux uses POSIX EREs. Rust's `regex` crate is a reasonable stand-in;
  the auto-downgrade to literal search when the pattern has no special characters
  (§7.1) and the all-lowercase ⇒ case-insensitive rule are the behaviors to match, not
  the regex dialect corner cases. Note tmux's search *timeouts* (200 ms count pass,
  10 s hard cap) are part of observable behavior (`(N+ results)`, `(timed out)`).
- **Alternate screen**: winmux's grid v2 already models real alt-screen; replicate the
  two tmux rules: wheel-up over an alt-screen app goes to the app, and prefix-`[` still
  enters copy mode showing alt content above old primary scrollback (history is frozen
  while alt is active).
- **Batch history trim** (`hlimit/10`, §8.1) is a performance behavior worth copying —
  per-line trims thrash.
- **Frozen vs live backing**: winmux currently reads live grid state in copy mode;
  classic-tmux equivalence needs snapshot semantics or at minimum the two invariants of
  §1.1 (no view shift from live output; never refresh while a selection exists).
- Not applicable / skippable on Windows: terminfo `Ms` detection, `/bin/sh` job
  semantics, vis(3) octal escaping specifics (any control-byte sanitisation on paste
  suffices), sixel image save/restore in alt-screen.
