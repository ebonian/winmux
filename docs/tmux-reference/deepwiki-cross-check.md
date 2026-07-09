# DeepWiki cross-check: tmux architecture as seen by DeepWiki

**Purpose.** This document is an *independent* cross-check for the source-derived specs in
`docs/tmux-reference/`. It captures what DeepWiki's AI-generated wiki for the tmux repository
(https://deepwiki.com/tmux/tmux) says about the topics our specs cover, so that discrepancies
between our own reading of the tmux C source and DeepWiki's summarization can be caught.

**Fetch status (2026-07-08).** DeepWiki was fully fetchable over plain HTTPS. The wiki's table of
contents (38 pages) was retrieved from the overview page, and the following pages were fetched and
condensed below: 2.4 (Layout System), 3.1 (Command Definitions and Parsing), 3.2 (Key Bindings),
3.4 (Session and Window Lifecycle Commands), 5.1 (Format System), 5.2 (Options System), 5.3 (Style
System), 5.4 (Configuration Loading), 6.1 (Status Line, Messages, and Prompts), 6.2 (Copy Mode),
6.3 (Mouse Handling and Overlays), 6.4 (Mode Tree UI Framework), 7.4 (Paste Buffers and
Environment).

**Fidelity note.** Everything below is restricted to what the DeepWiki pages actually said.
Where a page was silent on a sub-topic we care about, that is called out explicitly as
"DeepWiki gap" — silence is itself a finding (it means our source-derived spec is the only
authority for that point and deserves extra care). Quoted fragments are DeepWiki's words.

**Big picture (from https://deepwiki.com/tmux/tmux/1-overview).** DeepWiki describes tmux as a
client-server system where "a single server process manages all state (sessions, windows, panes)"
over "a single-threaded event loop," with state in "global red-black trees," a four-level
hierarchy (session → window → pane → virtual terminal screen) with "winlink indirection"
permitting windows to exist in multiple sessions, and where "all user operations flow through the
command system, which parses, validates, queues, and executes commands sequentially." This matches
winmux's single-owner server main loop + one-dispatcher design.

---

## 1. Mouse input handling

Source: https://deepwiki.com/tmux/tmux/6.3-mouse-handling-and-overlays

### What DeepWiki says

- Mouse events go through a multi-stage pipeline; classification into event types happens in
  `server-client.c` (cited lines 718–780) based on:
  - **Button state**: current button (`m->b`) vs last button state (`m->lb`);
  - **Client flags**: `CLIENT_DOUBLECLICK` and `CLIENT_TRIPLECLICK` "track click sequences";
  - **Drag state**: `c->tty.mouse_drag_flag` "indicates active drag operations".
- Event types named: `MOUSEDOWN`, `MOUSEUP`, `MOUSEDRAG`, `WHEEL`.
- Key codes are generated on the pattern `KEYC_<EVENT_TYPE><BUTTON_NUM>_<LOCATION>` by the central
  processor `server_client_check_mouse` (`server-client.c` lines 691–1091), for dispatch through
  the key-binding system.
- Location detection: `server_client_check_mouse_in_pane` (`server-client.c` lines 612–688)
  determines where the event landed by "examining pane geometry, scrollbar dimensions, slider
  position, and window zoom state." Locations include `PANE`, `STATUS`, `BORDER`,
  `SCROLLBAR_SLIDER`. (Scrollbars are a newer tmux feature winmux does not implement.)
- Overlays: managed via `server_client_set_overlay` / `server_client_clear_overlay`
  (`server-client.c` lines 91–152); popups (`popup.c`) support dragging; menus (`menu.c`) are
  rendered by `menu_draw_cb`.

### DeepWiki gaps (explicitly probed, explicitly absent)

The page was re-queried specifically for the drag lifecycle and confirmed to contain **no detail**
on: drag start/update/end mechanics; `mouse_drag_update`/`mouse_drag_release` callbacks; any
synthetic drag-end key (MOUSEDRAGEND); how the next drag is re-armed after release; double/triple
click timers; wheel-event specifics; root-table vs pane fallthrough resolution order; and
**border-drag pane resizing** and **drag-to-select in copy mode** are not addressed at all. Our
source-derived mouse spec is the sole authority for all of these.

### VERIFY against source

- VERIFY: classification really keys off `m->b` vs `m->lb` plus `c->tty.mouse_drag_flag`, in
  `server_client_check_mouse` in `server-client.c` (DeepWiki's cited line ranges suggest one big
  function ~lines 691–1091 with classification at ~718–780).
- VERIFY: the generated mouse "keys" follow the `<TYPE><BUTTON>_<LOCATION>` naming (e.g.
  MouseDown1Pane, MouseDrag1Border) and are resolved through the normal key-binding tables.
- VERIFY: double/triple click detection is client-flag based (`CLIENT_DOUBLECLICK`,
  `CLIENT_TRIPLECLICK`) — confirm what timer arms/clears those flags, since DeepWiki names the
  flags but not the timing.
- VERIFY: locations are exactly the set {pane, border, status regions, scrollbar} and how the
  border location is attributed (DeepWiki gives no border hit-test detail).

## 2. Directional pane navigation (select-pane -L/-R/-U/-D)

Sources: https://deepwiki.com/tmux/tmux/2.4-layout-system and
https://deepwiki.com/tmux/tmux/3.4-session-and-window-lifecycle-commands

### What DeepWiki says

Very little. The Layout System page says only that commands like `select-pane -U/D/L/R` use
functions such as `window_pane_find_up` to "traverse the layout geometry and find the nearest
neighbor," and separately mentions `layout_search_by_border` ("finds a layout cell at a specific
(x, y) coordinate, used for mouse interaction"). The Lifecycle Commands page covers
creation/destruction (`session_create()`, `spawn_window()`, `spawn_pane()`, `server_kill_window()`
etc.) and was confirmed to be **missing** any coverage of `select-pane` directional flags,
adjacent-pane finding, wrap/cycle behavior, `select-window` specifics, `swap-pane`,
`rotate-window`, or `break-pane` mechanics.

### DeepWiki gaps

- **Wrap/cycle behavior at edges: entirely absent from DeepWiki.** The source-derived
  panes-and-layout spec is the only authority on whether/when directional movement wraps
  (tmux's `window_pane_find_*` in `window.c` and the `pane-*-of` semantics).
- No detail on tie-breaking when several panes border the moved-from pane ("nearest neighbor" is
  DeepWiki's entire description).

### VERIFY against source

- VERIFY: the neighbor-finding functions are `window_pane_find_up/down/left/right` (in
  `window.c`, not layout code) and operate on pane offsets/sizes ("layout geometry"), not the
  layout tree structure.
- VERIFY: wrap behavior — DeepWiki neither confirms nor denies that -L/-R/-U/-D wrap around the
  window edge; the source spec's claim here has no independent corroboration and should be
  double-checked directly in `window.c`.
- VERIFY: tie-breaking rule among multiple candidate neighbors (e.g. by cursor position /
  most-recent use) — undocumented on DeepWiki.

## 3. choose-tree / mode-tree

Source: https://deepwiki.com/tmux/tmux/6.4-mode-tree-ui-framework

### What DeepWiki says

- The mode tree renders into a `struct screen`, splitting space between the hierarchical list and
  an optional preview area; it "calculates the available space for the tree vs. the preview based
  on the `preview` setting," which has **three states: off, normal, and big**.
- Preview box: populated by a consumer-provided draw callback (`drawcb`); "when enabled, it
  occupies space at the bottom of the screen." For choose-tree the preview "draws a
  mini-representation of the selected window or pane"; customize mode shows "option descriptions,
  types, and inheritance."
- Tagging: toggle tag (`t`), tag all (`Ctrl-t`), untag all (`T`); tagged items can be operated on
  collectively.
- Search/filter: `/` and `?` search, `f` filter; search is "depth-first search in both forward and
  backward directions"; the buffer chooser has "custom search to look inside buffer contents."
- Key handling in `mode_tree_key()`: Up/Down (`j`/`k`), PageUp/Down, Home/End (`g`/`G`),
  expand/collapse (Left/Right/`h`/`l`), tagging keys, search/filter keys.
- Data structures: `struct mode_tree_data` holds `modedata` (consumer data), `children`
  (root-level items), `line_list` (flattened display array), `current`/`offset`
  (selection + scroll), `preview`. Nodes are `struct mode_tree_item`; `struct mode_tree_line`
  is the flattened rendering view. Consumers rebuild the tree in a "build callback" that
  "reconstructs expansion and tag state using tag identifiers" (i.e. stable item tags survive
  rebuilds).

### DeepWiki gaps

- **Sort order and sort stability: absent.** DeepWiki says nothing about window-tree sort fields
  (index/name/time) or `O`/`r` sort keys; the source spec is the only authority.
- No callback signatures, no preview sizing arithmetic (how many rows the "normal" vs "big"
  preview takes), no per-consumer (window-tree.c) layout details.

### VERIFY against source

- VERIFY: the preview setting genuinely has three states (off / normal / big) — winmux currently
  models choose-tree without a preview; if the source spec says on/off only, this three-state
  claim is a discrepancy to resolve in `mode-tree.c`.
- VERIFY: preview sits at the **bottom** of the screen with the list above it.
- VERIFY: rebuild-preserves-state via tag identifiers (expansion + tagging survive a build-callback
  rerun) — relevant if winmux ever refreshes the overlay while open.
- VERIFY: `mode_tree_key()` default key set matches our overlay bindings (notably `g`/`G` as
  Home/End and `h`/`l` as collapse/expand, which winmux does not currently implement).

## 4. Options system

Source: https://deepwiki.com/tmux/tmux/5.2-options-system

### What DeepWiki says

- Four scopes: `OPTIONS_TABLE_SERVER` (e.g. `default-terminal`, `escape-time`, `history-file`),
  `OPTIONS_TABLE_SESSION` (`base-index`, `status`, `prefix`), `OPTIONS_TABLE_WINDOW`
  (`aggressive-resize`, `window-status-format`), `OPTIONS_TABLE_PANE` (`remain-on-exit`,
  `cursor-colour`, `cursor-style`); some options carry multiple scope bits
  (e.g. `OPTIONS_TABLE_WINDOW|OPTIONS_TABLE_PANE`).
  - Note: DeepWiki lists `escape-time` as a **server**-scope option; winmux docs treat it as a
    plain global — worth confirming which scope the real table gives it.
- All built-ins live in the `options_table[]` array (`options-table.c`), each a
  `struct options_table_entry` with "name, type, scope, flags, minimum/maximum bounds, valid
  choices, and default values."
- Command aliasing: "`set`, `setw`, and `set-window-option` represent different scope-specific
  variants" of one implementation (`cmd-set-option.c`); flags `-g` (global), `-s` (server, per
  DeepWiki's list "session" — see VERIFY), `-w` (window), `-p` (pane), `-a` (append), `-u`
  (unset), `-F` (format-expand the value). `show-options` supports `-g`, `-A` ("show parent
  values marked with `*`"), `-v` (value only), `-H` (include hooks).
- Inheritance: "local options override inherited values"; `options_get()` does hierarchical
  lookup with recursive parent traversal, `options_get_only()` "checks only the local red-black
  tree without parent recursion"; "parent pointers allow for efficient inheritance without data
  duplication."
- User options: names prefixed `@` are user-defined, "always have type `OPTIONS_TABLE_STRING`
  (implicitly)," bypass table-entry lookup, have no default value, and "can be set at any scope."
  DeepWiki states unknown option names "are handled through these user option mechanisms" —
  i.e. it describes the @-path, but does not explicitly state what error a non-@ unknown name
  produces.
- Types: STRING, NUMBER (with range limits), KEY, COLOUR, FLAG, CHOICE, COMMAND, STYLE, plus
  "array options supporting indexed access."
- `options_push_changes()` "broadcasts option changes to trigger side-effects like redrawing or
  updating internal state."

### VERIFY against source

- VERIFY: flag meanings in `cmd-set-option.c` — DeepWiki's gloss "`-g` (global/server), `-s`
  (session)" looks swapped relative to real tmux (`-s` is the **server** options flag; `-g`
  selects the global session/window table). This is exactly the kind of AI-summary slip the
  cross-check exists to catch; trust the source spec.
- VERIFY: `set-window-option`/`setw` is a pure alias of `set-option -w` (same cmd entry) rather
  than a separate implementation.
- VERIFY: a non-`@` unknown option name is a hard error ("unknown option") while any `@name` is
  silently accepted as a string at any scope.
- VERIFY: `options_get` vs `options_get_only` parent-chain semantics (pane → window → global
  window; session → global session) — DeepWiki confirms the mechanism but not the exact chains.

## 5. Command / config parsing, key tables, bind -n/-r/-T

Sources: https://deepwiki.com/tmux/tmux/3.1-command-definitions-and-parsing,
https://deepwiki.com/tmux/tmux/3.2-key-bindings,
https://deepwiki.com/tmux/tmux/5.4-configuration-loading

### What DeepWiki says — cmd-parse grammar (3.1)

- Commands are defined by `struct cmd_entry`: `name`, `alias` (one secondary short name),
  `args.template` (getopt-style, e.g. `"hvp:t:"`, colon = flag takes an argument, double colon =
  optional argument), `exec` callback.
- `args_parse()` (in `arguments.c`) parses tokens against the template into a red-black
  `args_tree` of `struct args_entry` plus a positional array.
- The parser itself is **yacc-based** (`cmd-parse.y`) and handles config files: `%if` / `%elif` /
  `%else` / `%endif` conditionals "evaluated via `format_true()`"; `VARIABLE=value` assignments go
  through `environ_put()`; arguments are "expanded using format engine during parsing"; commands
  that accept command arguments (like `command-prompt`) use `ARGS_PARSE_COMMANDS_OR_STRING`
  (recursive parsing).
- Target resolution (`cmd-find.c` / `cmd_find_state`): sessions by id `$0`/name; windows by id
  `@0`/index/name/relative symbols `^ ! + -` (with `{start}` → `^`, `{last}` → `!` mappings);
  panes by id `%0`/index/directional strings. With no target, tmux uses "activity timers to find
  the best session or client" and can resolve from `TMUX_PANE`.
- Aliases: `command-alias` (a server option) expands via `cmd_get_alias()` **before** command
  lookup; if no exact command-name match, **prefix matching** is attempted and "ambiguous prefixes
  trigger errors."
- DeepWiki gap: semicolon-separated command sequences, tokenization mechanics (quoting), and parse
  error handling are *not* detailed on this page.

### What DeepWiki says — key bindings (3.2)

- Named key tables held in a global `key_tables` tree, "created on demand and reference-counted."
  Main tables: **root** ("bindings effective without a prefix (e.g., mouse events)"), **prefix**,
  and the **copy-mode** / **copy-mode-vi** tables.
- Each `key_table` maintains **two** red-black trees: "one for user-defined bindings and another
  for defaults that can be reset." Each `key_binding` holds a key code, a command list, and an
  optional note (`bind-key -N`).
- Functions: `key_bindings_get_table()`, `key_bindings_get()` (user bindings),
  `key_bindings_get_default()` (default bindings), `key_bindings_dispatch()`.
  `unbind-key -a` clears whole tables; `list-keys` iterates tables. Key notation parsing is via
  `key_string_lookup_string()` (named but not detailed).
- DeepWiki gap: **`bind-key -n`, `-r`, `-T` and repeat-time behavior are not covered at all.**
  Source spec is the only authority for `-n` ≡ `-T root`, `-r` repeat semantics, and custom
  tables via `-T`.

### What DeepWiki says — configuration loading (5.4)

- `start_cfg()` (in `cfg.c`): at server startup, assigns "the first client found in the `clients`
  list to `cfg_client`" and appends a `cfg_client_done` callback that returns `CMD_RETURN_WAIT`
  "until `cfg_finished` is set" — **the initial client is blocked until config finishes**. It then
  iterates the config files calling `load_cfg` for each, then appends a global `cfg_done`
  callback that sets `cfg_finished = 1`, triggers `cfg_show_causes`, and resumes the client.
- `load_cfg()`: opens the file in binary mode, builds a `struct cmd_parse_input`, calls
  `cmd_parse_from_file`; a `cmdq_state` carries the file path as format variable `current_file`;
  the parsed command list is queued on the global command queue. Files load in iteration order,
  each producing queued `cmdq_item`s, so config commands execute sequentially through the normal
  queue.
- Errors: collected globally — `cfg_add_cause()` appends "formatted error strings to the
  `cfg_causes` array"; displayed by `cfg_show_causes`: control-mode clients get immediate
  `%config-error` lines; attached sessions get the active pane switched to `window_view_mode`
  with "the error text [appended] to the pane's scrollback"; if no session exists yet the causes
  wait until one is available. Whether an error **aborts** loading is not stated.
- `source-file`: path supports **tilde and glob patterns** ("globbing uses the `glob()` system
  call"); with `-F` the path is format-expanded first via `format_single_from_target`; there is
  "recursion depth limiting" and async file reading (`file_read()` + `cmd_source_file_done`
  callback). The `-q`/`-n`/`-v` flags are *not* discussed.
- DeepWiki gap: default config paths (`/etc/tmux.conf`, `~/.tmux.conf`, XDG) and the `-f` flag
  are **not covered anywhere on the page**.

### VERIFY against source

- VERIFY: config parse errors do **not** abort loading (tmux continues past bad lines, collecting
  causes) — DeepWiki's cause-collection description strongly implies it, but explicitly declines
  to say; confirm in `cfg.c`/`cmd-parse.y`.
- VERIFY: `source-file` glob semantics — that a non-matching glob with `-q` is silent, without
  `-q` is an error, and that plain `~` expansion happens for non-glob paths too. (DeepWiki
  confirms tilde+glob exist and that `glob()` is used, nothing more.)
- VERIFY: per-table dual RB-trees (user vs resettable defaults) — winmux's single mutable
  `Bindings` table differs structurally; tmux's `unbind` semantics against a *default* binding
  may therefore differ (a default can be shadowed/restored rather than deleted).
- VERIFY: command-name **prefix matching** with ambiguity errors at lookup time — winmux's `cmd`
  table should match this (e.g. `killw` → error if ambiguous, unique prefix accepted).
- VERIFY: alias expansion (`command-alias`) happens before command lookup.
- VERIFY: `%if`/`%elif`/`%else`/`%endif` evaluated with `format_true()` at **parse** time, and
  `NAME=value` lines becoming environment entries — winmux doesn't implement these; the spec
  should at least record them as known-missing grammar.
- VERIFY: the initial-client-blocks-until-config-done behavior (`CMD_RETURN_WAIT` until
  `cfg_finished`) — relevant to winmux because winmux also loads config at server start before
  serving the first attach.

## 6. Status line internals

Sources: https://deepwiki.com/tmux/tmux/6.1-status-line-messages-and-prompts,
https://deepwiki.com/tmux/tmux/5.1-format-system,
https://deepwiki.com/tmux/tmux/5.3-style-system

### What DeepWiki says — status line (6.1)

- Rendering pipeline driven by `status_redraw()`: build a per-client `format_tree` → apply
  `status-style` → expand the **`status-format` array** through `format_expand_time()` → draw via
  `format_draw()` into the status screen. (I.e. the modern per-line `status-format[i]` array is
  the real source of the status line; `status-left`/`status-right`/window list are inputs to its
  default value — DeepWiki does not spell out that last relationship.)
- Messages: `status_message_set()` "pushes" the current status screen;
  `status_message_redraw()` renders using `message-style` **and `message-line`** (vertical
  placement of the message when the status area has multiple lines).
- Prompts: `struct prompt`; `status_prompt_set()` (init + push screen + set client's prompt
  pointer), `status_prompt_key()` (input, "emacs and vi editing modes"),
  `status_prompt_redraw()`, and `status_prompt_complete()` which "generates menus for commands,
  sessions, or windows" (tab completion).
- Screen stacking: messages/prompts overlay the status line via reference-counted
  `status_push_screen()` / `status_pop_screen()` (first push allocates a new `sl->active`
  screen; pop frees at zero).
- DeepWiki gap: **nothing** on `status-left`/`status-right` specifically, window-list
  construction, `window-status-format` / `window-status-current-*` styling, `status-justify`,
  multiple status lines (`status` option values 2–5), `display-time`, or prompt history.

### What DeepWiki says — formats (5.1)

- `format_tree` (in `format.c`): RB-tree of `format_entry`, scope type (SESSION/WINDOW/PANE),
  context pointers (`c`, `s`, `wl`, `w`, `wp`), flags (e.g. `FORMAT_VERBOSE`).
- Expansion: `format_expand1` recursively processes `#{...}` via `format_replace`; resolution
  order is (1) RB-tree lookup, (2) single-character aliases (via `format_upper`/`format_lower`
  tables — e.g. `#S` = session_name, `#T` = pane_title), (3) callback (`cb`) execution for
  dynamic values, (4) `#(shell-command)` spawning an async job.
- Conditionals: `#{?cond,t,f}` — "if cond is non-zero/non-empty, expand to t, otherwise f."
- Modifiers documented: `#{b:}` basename, `#{d:}` dirname, `#{T:}` strftime timestamp, `#{q:}`
  shell quote, `#{l:}` literal-length. **Not** documented: `#{=N:}` truncation, `#{t:}`,
  comparison operators, loop modifiers `#{W:}`/`#{S:}`/`#{P:}`.
- `#()` jobs: `struct format_job` (`cmd`, cached `out`, `job` pointer, `updated` flag), cached
  globally in an RB-tree `format_jobs` — i.e. shell segments are asynchronous and render the
  *last* cached output.
- Drawing: `format-draw.c` renders formats; `format_draw_none` / `format_draw_left` etc. handle
  the "list component" (the window list) with alignment regions LEFT, CENTRE, RIGHT,
  ABSOLUTE_CENTRE; `format_update_ranges()` fixes up `style_range` positions after expansion.

### What DeepWiki says — styles (5.3)

- Style grammar: comma-delimited `attr,attr,key=value` with `fg=`, `bg=`, `us=`, `fill=`;
  attributes `bright|bold`, `dim`, `underscore`, `blink`, `reverse`, `hidden`, `italics`,
  `strikethrough`, `double-/curly-/dotted-/dashed-underscore`, `overline`; negation `no<attr>`;
  `none` clears all.
- Layout directives *inside style strings*: `align=left|centre|right|absolute-centre`,
  `list=on|focus|left-marker|right-marker`, `range=<type>|<arg>` (interactive/clickable regions),
  `width=<n>`, `pad=<n>`, and default-stack ops `default`, `push-default`, `pop-default`,
  `set-default`.
- `struct style`: `gc.fg/gc.bg/gc.us`, `gc.attr` bitmask, plus `fill`, `align`, `list`,
  `range_type`, `width`, `pad`; global `style_default`.
- `style_parse()` is a state-machine tokenizer; `style_add()` merges additively,
  `style_apply()` resets then applies. Parsed styles are cached in `options_entry`, but "dynamic
  format expansions (`#{...}`) bypass caching."
- `format_draw()` processes `#[...]` mid-string style switches, maintaining **four alignment
  screens (left, centre, right, absolute-centre) plus a list screen**; `format_range` +
  `format_is_type()` power clickable window names.

### VERIFY against source

- VERIFY: the status line is really composed from the `status-format[]` array option (with
  `status-left`/`status-right`/window list appearing only inside its default value) — winmux
  implements status-left/right directly; behavioral differences (e.g. user-set
  `status-format[0]`) hinge on this.
- VERIFY: `#[...]` supports `align=`/`list=`/`range=`/`push-default`/`pop-default` directives
  (not just colors/attrs) and that window-list centring under `status-justify` is implemented as
  the format-draw "list" component with those alignment screens.
- VERIFY: `#()` segments are async cached jobs (never block a redraw; show last output).
- VERIFY: format resolution order (tree → one-char alias → callback → job) in `format_replace`.
- VERIFY: `message-line` option exists and picks the status row a message is drawn on.
- VERIFY: prompt tab-completion (`status_prompt_complete`) offers commands, session names, and
  window names via a menu.

## 7. Copy mode: selection, search, buffers

Sources: https://deepwiki.com/tmux/tmux/6.2-copy-mode and
https://deepwiki.com/tmux/tmux/7.4-paste-buffers-and-environment

### What DeepWiki says — copy mode (6.2)

- Entry: the `copy-mode` command; `-u` scrolls "up one page immediately upon entry"; `-M` enters
  the mode "and begin[s] a mouse drag (triggered by mouse events)". Entering creates a
  `window_mode_entry` and a `window_copy_mode_data`.
- `window_copy_mode_data` fields (as listed by DeepWiki): `screen` (the rendered copy-mode UI
  screen), `backing` (pointer to the pane's real screen, "the source of truth"), `oy` (lines
  scrolled back into history), `cx`/`cy` (cursor within viewport), `selx`/`sely` (selection
  start), `endselx`/`endsely` (selection end), `rectflag` (rectangle selection),
  `cursordrag` (`CURSORDRAG_NONE` / `CURSORDRAG_SEL` / ... — "tracks mouse selection state"),
  `separators` (word-boundary characters), `mx`/`my` (persistent mark for `jump-to-mark`).
- Selection model: `begin-selection` "sets selx/sely to the current cursor position and sets
  cursordrag to CURSORDRAG_ENDSEL"; `window_copy_update_selection` runs on cursor movement and
  updates `endselx/endsely` to follow the cursor; `rectflag` renders/captures a block;
  select-word/select-line "automatically expand the selection boundaries".
- Search: plain strings **or regular expressions**; a `searchmark` bitmask marks "every cell in
  the grid that matches the pattern"; visible matches render with `window-copy-search-style`
  (winmux calls this `copy-mode-match-style` territory — see VERIFY); a
  `WINDOW_COPY_SEARCH_TIMEOUT` guard "prevents server hangs during complex regex operations".
- Copy extraction (`window_copy_get_selection`): iterates grid lines `sely`..`endsely`; with
  `rectflag` "pads lines to maintain the rectangular shape," otherwise "joins lines, respecting
  the `GRID_LINE_WRAPPED` flag" (wrapped logical lines are joined without a newline); result goes
  to `paste_add` or `paste_set`; with `set-clipboard` enabled the text is *also* sent to the real
  terminal via OSC 52 (`tty_set_selection`). `copy-pipe` pipes the selection to a shell command
  while also saving a buffer; `capture-pane` snapshots a grid into a buffer without copy mode.
- Mouse: drag translates terminal coords to grid coords; a `window_copy_drag_timer` "triggers
  automatic scrolling to expand the selection into the history" when the drag reaches the
  viewport edge; scrollbar-slider drags recompute `oy` proportionally.
- DeepWiki gaps: exit commands, the full movement-command set, search-forward/backward and
  incremental-search specifics, and search wrapping are not covered.

### What DeepWiki says — paste buffers (7.4)

- `paste.c` keeps **two** red-black trees: `paste_by_name` (by unique name) and `paste_by_time`
  (by "an incrementing `order` value maintaining a strict 'most recent' sequence"). Buffers may
  contain NUL bytes (explicit sizes).
- Automatic buffers get generated names `buffer0`, `buffer1`, ... and are subject to
  `buffer-limit`: "when the limit is reached, the oldest **automatic** buffer is freed" — named
  buffers persist and are exempt. `paste_set` "adds or replaces a named buffer"; `paste_get_top`
  "retrieves the most recent automatic buffer".
- Commands: `load-buffer` (async file read), `save-buffer`/`show-buffer`, `paste-buffer` ("sends
  buffer content to a pane's input buffer," honoring bracketed paste with `-p`), `set-buffer`
  (with `-w` to also write the clipboard: base64 + `\033]52;c;...` via `tty_set_selection`).
- DeepWiki gaps: `paste-buffer -d` (delete after paste) and `-s` (separator), `delete-buffer`,
  `list-buffers` are not documented.

### VERIFY against source

- VERIFY: `buffer-limit` eviction frees the oldest **automatic** buffer only — explicitly named
  buffers are never evicted by the limit. (winmux's `buffers` module must match this asymmetry.)
- VERIFY: non-rectangle copy **joins wrapped lines** (no newline inserted at a soft wrap,
  per `GRID_LINE_WRAPPED`), and rectangle copy pads short lines to the block width.
- VERIFY: begin-selection puts the *moving* end at the cursor (`cursordrag = CURSORDRAG_ENDSEL`)
  and `other-end` swaps which end tracks the cursor.
- VERIFY: search populates a whole-grid `searchmark` (all matches highlighted, not just the
  current one) styled by the search style option, and that a regex/`timeout` guard exists.
- VERIFY: mouse drag-select in copy mode auto-scrolls at the viewport edge via a repeating drag
  timer (`window_copy_drag_timer`) — winmux currently has no drag auto-scroll; confirm whether
  the source spec flags this as a divergence.
- VERIFY: `paste_by_time` ordering is a monotonic counter (not wall-clock), so paste `-p` /
  `prefix-]` always takes the strictly most recent buffer.

---

## Overall assessment

DeepWiki proved strongest on **data structures and file/function inventory** (options system,
format/style systems, copy-mode state, paste-buffer trees, layout-cell tree, cmd_entry/cmd-parse,
cfg.c startup sequencing) and weakest on **behavioral edge cases** — exactly the things our
source-derived specs exist for: mouse drag lifecycle and border-drag resize, directional-navigation
wrap rules, choose-tree sort order, bind -n/-r/-T and repeat-time, status-left/right/justify
mechanics, and copy-mode movement/search key specifics all drew explicit blanks. Where DeepWiki
made a checkable claim that smells wrong, it is flagged above (most notably the apparent `-g`/`-s`
set-option flag swap in §4). No DeepWiki claim contradicting a hard architectural assumption of
winmux was found.
