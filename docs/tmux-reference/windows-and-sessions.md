# tmux behavioral reference: Windows and Sessions

Source studied: tmux master `db115c6` (2026-07-07), full clone at the time of writing.
All `file:line` references are into that tree. This document is intended to be sufficient
to implement tmux-identical window/session behavior in winmux **without reopening the
tmux source**.

Data model recap (window.c:37–54):

- A **window** is a global object (`struct window`, id `@N`, `next_window_id` counter,
  never reused). Windows live in a global RB tree ordered by id (window.c:91–95).
- A **winlink** is a (session, index) → window link (`struct winlink`). A window may be
  linked into any number of sessions (or several times into one session). Winlinks are
  kept per-session in an RB tree ordered by **index** (window.c:97–101). The window keeps
  a back-list of all its winlinks (`w->winlinks`).
- A window holds a reference count; when the last winlink is removed the window is
  destroyed (window.c:415–423 `window_remove_ref`).
- A **session** (`struct session`, id `$N`, `next_session_id`, never reused) owns:
  `windows` (RB tree of winlinks), `curw` (current winlink), `lastw` (a **stack** of
  previously-current winlinks, most recent first), `attached` (count of attached
  clients, recomputed in resize.c:436–455), name, cwd, environ, options, activity/creation
  times. Sessions live in a global RB tree ordered by **name** (session.c:40–45
  `session_cmp` = `strcmp`).
- A **client** points at one session (`c->session`) plus `c->last_session`.

---

## 1. Window indexing

### base-index and the "first free slot"

- `base-index` (session option, default **0**, options-table.c:752–759) is the starting
  index for new windows.
- The generic allocator is `winlink_add(wwl, idx)` (window.c:176–192):
  - `idx >= 0`: fail if that index is taken (`NULL` return → "index in use").
  - `idx < 0`: treated as a request for "first free index at or after `-idx - 1`".
    `winlink_next_index` (window.c:146–161) scans upward from that start, **wrapping at
    `INT_MAX` back to 0**, and returns the first free index (or -1 if literally all
    `INT_MAX` indexes are used).
- `spawn_window` (spawn.c:145–166): when no index was given (`idx == -1`) it calls
  `winlink_add(&s->windows, -1 - options_get_number(s->options, "base-index"))`
  (spawn.c:147–148), i.e. **first free index ≥ base-index**. Note this fills gaps: with
  windows at 0 and 2, the next `new-window` gets index 1, not 3.
- When a target index **was** given (`new-window -t :5`): if that index exists and `-k`
  was not given → error `"index %d in use"`; with `-k` the existing winlink is unlinked
  first (spawn.c:122–143). If the killed winlink was the current window, `s->curw` is set
  to NULL and the new window is force-selected even if `-d` was given
  (spawn.c:138–141: `sc->flags &= ~SPAWN_DETACHED`).

### new-window -a / -b (insert after/before)

cmd-new-window.c:119–124:

```c
before = args_has(args, 'b');
if (args_has(args, 'a') || before) {
        idx = winlink_shuffle_up(s, wl, before);
        if (idx == -1)
                idx = target->idx;
}
```

`winlink_shuffle_up` (window.c:2099–2128): with `-a` the desired index is `wl->idx + 1`,
with `-b` it is `wl->idx` itself. It then finds the first free index at or above that
point and shifts the **contiguous run** of occupied indexes `[idx, firstfree)` up by one
(each existing winlink's `idx` is incremented in place — the winlink objects, alert
flags, lastw stack membership, and curw pointer all survive because only the index field
changes). Returns the freed index. So `-a`/`-b` never fail on collision; they make room.
The same helper is used by `move-window -a/-b` and `link-window -a/-b`
(cmd-move-window.c:93–101), where a missing `-t` winlink falls back to shuffling relative
to `dst->curw`.

### renumber-windows / session_renumber_windows

- `renumber-windows` (session option, flag, default **off**, options-table.c:978–984).
- `session_renumber_windows(s)` (session.c:692–746) rebuilds the whole winlink tree:
  - New indexes are assigned **sequentially starting at `base-index`**
    (session.c:704–721), in ascending old-index order.
  - Alert flags are carried over (`wl_new->flags |= wl->flags & WINLINK_ALERTFLAGS`).
  - The **lastw stack is rebuilt in the same recency order**, remapping each stack entry
    to the new winlink for the same window (session.c:723–733) — so last-window behavior
    is preserved across a renumber.
  - `s->curw` is remapped by remembering the old current's new index (session.c:735–741);
    the marked pane's winlink is also remapped.
- When it triggers automatically: `server_renumber_session` (server-fn.c:224–236) is
  called from `server_kill_window(w, renumber=1)` (server-fn.c:199–222) — i.e. whenever a
  window is killed or its last pane exits — but only if the option is on. If the session
  is in a group, **every session in the group** is renumbered. `kill-window -a` renumbers
  all sessions once at the end (cmd-kill-window.c:133 `server_renumber_all`).
  `move-window` renumbers the **source** session only (cmd-move-window.c:111–117), and
  skips that when `-s` was explicitly given? No — read carefully: it renumbers `src` when
  the option is on and `-s` was **not** given (`!sflag`, cmd-move-window.c:116).
- Manual trigger: `move-window -r [-t target-session]` does nothing but
  `session_renumber_windows(target.s)` + `recalculate_sizes()` + status refresh
  (cmd-move-window.c:72–82).

### move-window / link-window

Both share one exec (cmd-move-window.c:59–122). `link-window` = link into destination;
`move-window` = link + `server_unlink_window(src, wl)` afterwards (cmd-move-window.c:108–109).

Flags: `-s src-window`, `-t dst-window` (a *window index* target — may be nonexistent:
`CMD_FIND_WINDOW_INDEX`), `-k` kill destination index if occupied, `-d` do not select the
moved window in the destination, `-a`/`-b` insert after/before target, `-r` renumber-only
mode (see above).

Core: `server_link_window(src, srcwl, dst, dstidx, killflag, selectflag, cause)`
(server-fn.c:247–302):

- Error `"sessions are grouped"` if src ≠ dst but both are in the **same** session group.
- If the destination index is occupied:
  - same window at that index → error `"same index: %d"`;
  - with `-k`: destination winlink is removed **without** `session_detach` (so the
    session is not destroyed even if it becomes empty mid-operation); alert flags
    cleared, lastw stack entry removed, `"window-unlinked"` notified. If it was the
    destination session's current window, `dst->curw = NULL` and select is forced
    (server-fn.c:270–287).
  - without `-k`: `session_attach` → `winlink_add` fails → error `"index in use: %d"`.
- `dstidx == -1` (no `-t`) → `-1 - base-index` → first free slot ≥ base-index
  (server-fn.c:289–290).
- `selectflag` (= not `-d`) → `session_select(dst, dstwl->idx)`.
- move-window then unlinks from source: `server_unlink_window` (server-fn.c:304–311) =
  `session_detach`, and if that empties the source session the session (or its whole
  group) is destroyed (`server_destroy_session_group`).

### unlink-window

cmd-kill-window.c:75–83: `unlink-window [-k]` — without `-k` it refuses
(`"window only linked to one session"`) unless the window is linked to another session;
the check `session_is_linked` (session.c:377–385) compares the window's reference count
against 1, or against the group size when the session is grouped (links inside one group
don't count as "linked elsewhere"). With `-k` it unlinks regardless, killing the window
when this was its last link, possibly destroying the session.

### swap-window

cmd-swap-window.c:45–96. `-s` defaults to the **marked pane's window** if a mark exists,
else the current window (`CMD_FIND_DEFAULT_MARKED`, cmd-swap-window.c:38).

- Error if src ≠ dst sessions are in the same group.
- No-op (success) if both winlinks already point at the same window.
- Mechanism: the two winlinks stay at their indexes; their `->window` pointers are
  exchanged (cmd-swap-window.c:70–78). Alert flags/lastw membership stay with the
  *winlink* (i.e. with the index).
- **`-d` semantics** (cmd-swap-window.c:82–86): winlink identity means that *without*
  `-d`, `s->curw` is untouched — the client stays on the same **index**, which now shows
  the other window. *With* `-d`, `session_select(dst, wl_dst->idx)` (and
  `session_select(src, wl_src->idx)` when the sessions differ) is called: focus is moved
  to the index where each session's *swapped-in* window landed, i.e. **focus follows the
  window object**. So (matching the man page's "if -d is given, the new window does not
  become the current window"): if you are on the source window and swap it with index N,
  without `-d` you end up looking at the window that came *from* N; with `-d` you keep
  looking at your original window (now at index N).
- Ends with `session_group_synchronize_from` + full redraw for both sessions and
  `recalculate_sizes()`.

---

## 2. Window selection

### Target resolution for `-t target-window` (cmd-find.c)

A target string is split at the **first `:`** into session part and window part, then at
the **first `.`** into window and pane parts (cmd-find.c:1065–1113). With no separator the
string is interpreted according to the command's find type (session/window/pane), except
that a leading `$`/`@`/`%` forces session/window/pane id interpretation. A leading `=` on
the session or window part demands **exact match only** (cmd-find.c:1116–1123). Empty
parts are the current ones. Convenience aliases are mapped first
(cmd-find.c:51–58): `{start}`→`^`, `{last}`→`!`, `{end}`→`$`, `{next}`→`+`,
`{previous}`→`-`. Special whole-target tokens: empty/NULL → current;
`{mouse}`/`=` → mouse event target; `{marked}`/`~` → marked pane
(cmd-find.c:1006–1063).

**Session part** resolution order (`cmd_find_get_session`, cmd-find.c:263–324):
1. `$id`;
2. exact name match;
3. as a client name (that client's session);
4. unless exact-only: unique **prefix** of a session name (`strncmp`), error if the
   prefix is ambiguous;
5. unique `fnmatch` glob pattern, error if ambiguous.

**Window part** resolution order (`cmd_find_get_window_with_session`,
cmd-find.c:363–512), within a session:
1. `@id` (must be linked to the session);
2. unless exact-only: offset `+N`/`-N` relative to the current window (`+`/`-` alone
   mean 1). For commands that accept a *new* index (`CMD_FIND_WINDOW_INDEX`, e.g.
   new-window/move-window) the offset is plain arithmetic on `curw->idx`
   (cmd-find.c:396–407); otherwise it walks the winlink tree with **wraparound**
   (`winlink_next_by_number`/`winlink_previous_by_number`, window.c:232–252);
3. unless exact-only: `!` = top of the lastw stack (last window), `^` = lowest index,
   `$` = highest index (cmd-find.c:420–443);
4. numeric window **index** in this session; if not present and the command accepts an
   index, the bare number is returned as the new index (cmd-find.c:445–460);
5. exact window **name** match — error if two windows share the name
   (cmd-find.c:462–475);
6. unless exact-only: unique name **prefix**; then 7. unique **fnmatch glob** — each
   erroring on ambiguity (cmd-find.c:481–509).

When only a window part is given (`cmd_find_get_window`, cmd-find.c:327–357) it is tried
in the current session first; if that fails and the target didn't contain `:`, the same
string is retried **as a session name**, yielding that session's current window.

`select-window -t :N` therefore means "index N in the current session"; `-t :=N` means
exactly index N (no name fallback) — this is what the default `0`–`9` bindings use
(key-bindings.c:400–409).

### select-window

cmd-select-window.c:84–150. Flags `-l`/`-n`/`-p` make it behave as
last-/next-/previous-window. `-T`: if the target already **is** the current window,
switch to the last window instead (toggle behavior, cmd-select-window.c:127–138).
Otherwise `session_select(s, wl->idx)` (session.c:450–457 → `session_set_current`).
On success the session is fully redrawn; `s->curw->window->latest = c` records the
selecting client for the `latest` window-size rule; `recalculate_sizes()` runs.

### next-window / previous-window (and -a alert variants)

`session_next`/`session_previous` (session.c:398–447):

- next = RB-successor of `curw`; **wraps** to `RB_MIN` at the end. previous = RB
  predecessor, wraps to `RB_MAX`.
- With `alert` (the `-a` flag): from that starting point, keep walking (no additional
  wrap in the alert scan for `session_next` — it scans forward from the successor to the
  tree end, then retries once from `RB_MIN`; session.c:387–415) until a winlink with any
  alert flag (`WINLINK_BELL|ACTIVITY|SILENCE`) is found; if none exists the command
  errors (`"no next window"` / `"no previous window"`, cmd-select-window.c:107–121).
- Selecting the same window returns 1 (treated as success by callers; no redraw).

### last-window and the lastw stack

`s->lastw` is a TAILQ **stack of all previously-current winlinks in recency order**, not
a single slot. `session_set_current` (session.c:475–498) does:

```c
winlink_stack_remove(&s->lastw, wl);       /* remove new current from stack */
winlink_stack_push(&s->lastw, s->curw);    /* push old current on head */
s->curw = wl;
```

(`winlink_stack_push` itself first removes the winlink if already present, so each
winlink appears at most once — `WINLINK_VISITED` flag guards membership,
window.c:254–272.)

`session_last` (session.c:460–472) selects `TAILQ_FIRST(&s->lastw)`; error
(`"no last window"`) if the stack is empty; returns 1 (no-op) if the stack head *is* the
current window. Because it is a stack, when the last window is killed the *next* most
recent one becomes the new "last" — killing the current window also falls back through
the stack (see §6). Window destruction removes stack entries via
`winlink_stack_remove` in `session_detach` (session.c:350) and during renumber/group
sync.

`session_set_current` also (session.c:488–497): fires focus events if `focus-events` is
on, clears the new current window's alert flags (`winlink_clear_flags`), bumps window
activity, updates tty offsets, and notifies `session-window-changed`.

### find-window

cmd-find-window.c:44–116. `find-window [-CNTirZ] match-string` is sugar that opens
**window-tree (choose-tree) mode** on the current pane with a filter format, not a direct
jump. Flags:

- `-C` match visible pane **content**, `-N` match window **name**, `-T` match pane
  **title**; default when none given is all three (`C = N = T = 1`,
  cmd-find-window.c:68–69).
- `-r`: match-string is a **regular expression** (uses format `m/r:` matching and drops
  the implicit `*...*` glob wrapping — with `-r`, `star = ""`); without `-r` the string
  is used as a glob wrapped in `*` on both sides (substring glob match)
  (cmd-find-window.c:51–66).
- `-i`: case-insensitive (`/i` suffix; combines to `/ri` with -r).
- Name/title matching uses `#{m[/ri]:pattern,#{window_name}}`; content matching uses the
  `#{C[/ri]:pattern}` format (searches **visible** pane contents, not history).
- `-Z` keeps the tree zoomed.

The filter ORs the enabled criteria (cmd-find-window.c:74–105) and is passed to
`window_tree_mode`. Selecting an entry in the tree performs the jump.

---

## 3. Naming

### Name storage and validation

- Window names are arbitrary strings; `window_set_name` (window.c:444–455) passes the
  new name through `clean_name(name, untrusted)` and notifies `"window-renamed"`.
- `clean_name` (tmux.c:284–299): rejects invalid UTF-8 (returns NULL → rename silently
  ignored); when `untrusted` (names coming from the application escape sequence or OSC),
  any `#(` is rewritten to `_(` to neutralize command substitution in formats; the
  result is octal/C-style vis-escaped (tabs/newlines encoded).
- `check_name` (tmux.c:301–307) — used to validate `-n`/`-s` arguments and
  rename-session/rename-window arguments — only requires **valid UTF-8**. Current master
  has **no** ban on `:`, `.`, or empty names (the historical `session_check_name`
  restriction is gone from both code and man page); such names simply become awkward to
  target. rename-session additionally rejects duplicates
  (`"duplicate session: %s"`, cmd-rename-session.c:66–70) and no-ops on an unchanged
  name; the RB tree is re-inserted since it is keyed by name (cmd-rename-session.c:72–75)
  and `"session-renamed"` is notified.

### Session auto-naming

`session_create` (session.c:135–147): with no `-s` name, the name is the decimal session
id (`"%u"`), or `"<prefix>-%u"` when created inside a session group with a name prefix,
retried with fresh ids until unique.

### Window initial name (at spawn)

spawn.c:177–186:

```c
if (sc->name == NULL || *sc->name == '\0')
        w->name = default_window_name(w);
else {
        w->name = xstrdup(sc->name);
        options_set_number(w->options, "automatic-rename", 0);
}
```

So an **explicit `-n name` permanently disables automatic-rename for that window** (as a
window-local option). `default_window_name` (names.c:107–121) stringifies the pane's
argv (the command it was spawned with) or, if empty, the pane's shell path, and feeds it
through `parse_window_name` (names.c:141–174): strip surrounding `"`, strip a leading
`exec `, skip leading spaces/dashes, truncate at the first space, trim trailing
non-alnum/non-punct characters, `basename()` if it starts with `/`, then `clean_name`.

### automatic-rename (names.c)

- Window option `automatic-rename`, default **on** (options-table.c:1319–1324).
- Trigger: the server event loop calls `check_window_name(w)` for every window on each
  loop pass (server-client.c:1755). It does nothing unless the window's **active pane**
  has the `PANE_CHANGED` flag (set whenever the pane produces output, and when a pane
  becomes active — window.c:594), names.c:66–69.
- **Throttle**: renames happen at most once per `NAME_INTERVAL` = **500000 µs**
  (tmux.h:115) per window. If called earlier, a one-shot timer is queued for the
  remaining time (names.c:72–88); when it expires the event loop re-checks. On a real
  rename attempt `PANE_CHANGED` is cleared (names.c:93).
- The new name is **not** the OSC title: it is the expansion of the window option
  `automatic-rename-format`, default
  `#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}`
  (options-table.c:1326–1332), expanded with window+active-pane format defaults
  (names.c:123–139). `pane_current_command` is obtained from the OS by inspecting the
  process group leader on the pane's tty (osdep_*.c, `osdep_get_name`) — *not* from any
  escape sequence.
- If the expanded name differs from the current one, `window_set_name(w, name,
  untrusted=1)` + border/status redraw (names.c:95–101).
- Manual `rename-window` sets the name and then
  `options_set_number(wl->window->options, "automatic-rename", 0)`
  (cmd-rename-window.c:60–61) — i.e. it disables automatic renaming for that window only,
  by setting the window-local option, until someone re-enables it (`set -w
  automatic-rename on`, or the empty-rename escape below).

### allow-rename — what it actually gates

Window/pane option `allow-rename`, default **off** since tmux 2.6
(options-table.c:1295–1301; CHANGES:1858). It gates **only the application escape
sequence** `ESC k <name> ESC \` (the old screen "rename" sequence), handled in
`input_exit_rename` (input.c:2799–2830):

```c
if (!options_get_number(ictx->wp->options, "allow-rename"))
        return;
...
if (ictx->input_len == 0) {
        /* empty name: restore automatic-rename to default and clear name */
        o = options_get_only(w->options, "automatic-rename");
        if (o != NULL) options_remove_or_default(o, NULL, NULL);
        if (!options_get_number(w->options, "automatic-rename"))
                window_set_name(w, "", 1);
} else {
        options_set_number(w->options, "automatic-rename", 0);
        window_set_name(w, ictx->input_buf, 1);
}
```

Precision points:
- `allow-rename` does **not** affect `automatic-rename`, and does **not** affect OSC 0/2.
- OSC 0/2 set the **pane title** (`#{pane_title}`), gated by the separate `allow-set-title`
  option (input.c:2780 area; options-table.c:1303 region) — they never rename the window
  directly. They influence the window name only indirectly if the user puts
  `#{pane_title}` in `automatic-rename-format`.
- A non-empty ESC-k rename disables automatic-rename for the window (like a manual
  rename); an **empty** ESC-k rename un-sets the window-local automatic-rename override
  (reverting to the inherited value) and clears the name if automatic-rename is still
  off.
- Therefore a user config with `set -g allow-rename off` is merely restating the default;
  winmux must accept the option (window scope, flag) even if ESC-k is unimplemented.

---

## 4. Sessions

### new-session (cmd-new-session.c:67–383)

Args: `[-AdDEPX] [-c start-directory] [-e environment] [-F format] [-f client-flags]
[-n window-name] [-s session-name] [-t group-name] [-x width] [-y height]
[shell-command...]`.

Order of operations and exact semantics:

1. `-t` cannot be combined with a shell command or `-n` (error
   `"command or window name given with target"`, cmd-new-session.c:97–100).
2. `-n`/`-s` values are format-expanded, `check_name`-validated (error
   `"invalid window name: %s"` / `"invalid session name: %s"`), then `clean_name`d
   (cmd-new-session.c:102–121).
3. **`-A`**: if the named session (or the `-t` target session when `-s` is absent)
   exists, the command becomes `attach-session` with `-D` → its dflag, `-X` → its xflag,
   plus `-c`, `-E`, `-f` forwarded (cmd-new-session.c:122–136). `-D`/`-X` are meaningless
   without `-A`.
4. Duplicate name check: `"duplicate session: %s"` (cmd-new-session.c:137–140).
5. Session-group resolution for `-t` (cmd-new-session.c:142–162): the group is the
   target session's group, or an existing group with that name, or a new group named by
   the (validated) `-t` argument. The group name becomes the auto-name prefix.
6. `detached = args_has('d')`, forced on when there is no client
   (cmd-new-session.c:164–167).
7. cwd: `-c` (format-expanded) else the client's cwd (cmd-new-session.c:176–180).
8. If this attaches an existing terminal: `$TMUX` nesting check (error
   `"sessions should be nested with care, unset $TMUX to force"`), termios saved from
   the client fd into the session's saved tio (cmd-new-session.c:182–204), terminal
   opened (206–213).
9. **Size** (cmd-new-session.c:215–270): `-x`/`-y` accept 1..USHRT_MAX or the literal
   `-` meaning "client's current size". When attaching normally the initial window size
   is the client tty size minus 1 row when the status line is on. When detached (or a
   control client), size comes from the `default-size` session option (`"80x24"`
   default), with `-x`/`-y` overriding individual axes; final fallback 80×24, clamped
   ≥1. When `-x`/`-y` are given, the session's own `default-size` option is set to that
   size (cmd-new-session.c:274–280).
10. Environment: session environ starts from `update-environment` copying from the
    client unless `-E`; `-e VAR=value` entries applied on top (cmd-new-session.c:281–288).
11. `session_create` then `spawn_window` for the initial window (`idx = -1` → first free
    ≥ base-index; `-n` name; command argv). Failure destroys the just-made session with
    error `"create window failed: %s"` (cmd-new-session.c:289–310).
12. Group wiring: add to group, `session_group_synchronize_to` (copy the *other*
    session's windows into the new one), then select the lowest index
    (cmd-new-session.c:316–327).
13. If not detached: client flags applied (`-f`), `MSG_READY` sent, and if the client
    was already attached to another session, `c->last_session` is set before
    `server_client_set_session(c, s)` (cmd-new-session.c:334–345).
14. `-P` prints the format (`-F`, default `#{session_name}:`).

Session name rules: see §3 (valid UTF-8, non-duplicate; auto-name = id).

### attach-session (cmd-attach-session.c:50–184)

- Error `"no sessions"` if none exist.
- If the `-t` target contains `:` or `.` it is resolved as a **pane** target, and the
  attach also switches that session's current window and active pane
  (cmd-attach-session.c:80–101). Otherwise it is a session target resolved with
  `CMD_FIND_PREFER_UNATTACHED`.
- With **no `-t`**, "best" session = most recent `activity_time`, but any **unattached**
  session beats any attached one (`CMD_FIND_PREFER_UNATTACHED`, cmd-find.c:133–148,
  cmd_find_best_session cmd-find.c:163–188).
- `-c` replaces the session working directory. `-f` sets client flags. `-r` makes the
  client read-only + ignored for sizing (`CLIENT_READONLY|CLIENT_IGNORESIZE`).
- `-d` / `-x`: every **other** client on the same session is detached; `-x` uses
  `MSG_DETACHKILL` (the detached client, after exiting, sends `SIGHUP` to its parent
  process, client.c:417–422) instead of plain `MSG_DETACH` (cmd-attach-session.c:122–133,
  147–157). The attaching client itself always stays.
- `c->last_session = c->session` before switching (cmd-attach-session.c:121).
- Environment: `update-environment` variables copied from the client into the session
  unless `-E` (cmd-attach-session.c:134–135).
- Multiple clients on one session are fully supported; they share curw (a session has
  one current window — clients do not have per-client current windows).

### detach-client (cmd-detach-client.c:57–117)

- `-t` target client (default: the issuing client). `-s session`: detach **all** clients
  on that session. `-a`: detach all clients **except** the target. `-P`: use
  `MSG_DETACHKILL` (SIGHUP to the client's parent). `-E cmd`: instead of detaching, make
  the client's process `exec` the given command.
- `server_client_detach` (server-client.c:562–575) marks the client `CLIENT_EXIT`,
  records exit type detach + the session name (the client prints
  `[detached (from session X)]`).
- Detaching triggers (via `server_client_lost`/exit path) `recalculate_sizes` and
  `server_check_unattached` (destroy-unattached processing, server-client.c:513–515).

### switch-client (cmd-switch-client.c:48–163)

- `-t` containing `:`/`.`/`%` (or exactly `=`) is a pane target: switches that session's
  active pane/current window too (with `-Z` keeping zoom, cmd-switch-client.c:140–148);
  otherwise a session target with prefer-unattached.
- `-n`/`-p`: next/previous session from the **sorted session list**;
  `session_next_session`/`session_previous_session` (session.c:276–319) find the current
  session in the list and step with **wraparound**. Ordering: `-O
  activity|creation|index|name|size` (+`-r` reverse); with no `-O` the list keeps RB-tree
  order, i.e. **ascending session name** (sort.c:28–51: unknown/absent order leaves the
  RB_FOREACH order untouched; sessions RB tree is keyed by name). Errors:
  `"can't find next session"` / `"can't find previous session"`.
- `-l`: `tc->last_session` if still alive, else error `"can't find last session"`
  (cmd-switch-client.c:128–136). "Last session" = the session the client was attached to
  before the most recent switch: maintained in `server_client_set_session`
  (server-client.c:408–415 — set on switch when old ≠ new, cleared when the client is
  given no session) and in attach/new-session paths.
- `-r` toggles read-only+ignoresize; `-T table` sets the client's key table.
- Ends with environment update (unless `-E`) and `server_client_set_session`.

`server_client_set_session` (server-client.c:406–438) is the single choke point for a
client changing sessions: updates `last_session`, sets `CLIENT_FOCUSED`, updates focus
on old and new current windows, sets `s->curw->window->latest = c`,
`recalculate_sizes()`, bumps session activity, records `last_attached_time`, **clears the
new current winlink's alert flags**, re-checks alerts for the session, restarts the
status timer, notifies `client-session-changed`, full client redraw, then
`server_check_unattached()` + `server_update_socket()`.

### Session groups (brief but accurate)

- A group (`struct session_group`, session.c:31, 500–689) is a named set of sessions
  that share the **same set of windows**. Created via `new-session -t`.
- After any structural change (`session_attach`/`session_detach`/spawn/swap), the source
  session's window set is mirrored to all other group members by
  `session_group_synchronize_from` (session.c:613–626 → `session_group_synchronize1`,
  session.c:633–689): all winlinks in the peer are rebuilt at the **same indexes** as the
  target's, alert flags copied, each peer keeps its **own curw and lastw stack**
  (remapped by index; if a peer's current window vanished it moves last→previous→next
  first, session.c:645–649).
- `session_is_linked` treats intra-group links as not-linked (§1 unlink-window).
- Killing/destroying: `server_destroy_session_group` destroys **every** session in the
  group together (server-fn.c:390–405) — this is what runs when the last window of a
  grouped session dies. (`kill-session` without `-g` kills only the one session —
  its windows survive in the group peers; `kill-session -g` kills the whole group,
  cmd-kill-session.c:73–78.)
- `#{session_grouped}`, `#{session_group}`, group counting helpers:
  `session_group_count` / `session_group_attached_count` (session.c:568–591).

### destroy-unattached / detach-on-destroy

- `destroy-unattached` (session option, choice `off|on|keep-last|keep-group`, default
  **off**, options-table.c:792–799). Checked in `server_check_unattached`
  (server-fn.c:477–508) after every detach/session change: for each session with
  `attached == 0`: `on` → destroy; `keep-last` → destroy only if its group has >1
  member; `keep-group` → destroy unless it is the **sole** member of its group (i.e.
  with a group of 1 it survives; ungrouped sessions are destroyed).
- `detach-on-destroy` (session option, choice `off|on|no-detached|previous|next`,
  default **on** = 1, options-table.c:801–808). Resolved in `server_destroy_session`
  (server-fn.c:436–475) when a client's session is destroyed:
  - `on` (1): client detaches (exits).
  - `off` (0): switch the client to the most-recently-active other session.
  - `no-detached` (2): switch to the most-recently-active session **that has no attached
    clients**; if none, detach.
  - `previous` (3) / `next` (4): the alphabetically previous/next session
    (`session_previous_session`/`session_next_session` with default ordering).
  - Fallback: for `on`/`no-detached`, if a client has the internal
    `CLIENT_NO_DETACH_ON_DESTROY` flag it is switched to any newest session instead of
    exiting (server-fn.c:457–466).
  - Clients switched this way get `c->session = NULL; c->last_session = NULL` first, so
    last-session does not point at the dead session.

---

## 5. Alerts and activity (alerts.c)

### Sources

- **Bell**: BEL (0x07) in pane output → `alerts_queue(w, WINDOW_BELL)`
  (input.c:1299–1301).
- **Activity**: any pane output calls `window_update_activity` (input.c:1034 →
  window.c:298–303) which stamps `w->activity_time` and queues `WINDOW_ACTIVITY`.
  Selecting a window also bumps activity (session.c:494).
- **Silence**: `alerts_reset` (alerts.c:138–155) re-arms a per-window timer to
  `monitor-silence` seconds on **every** `alerts_queue` call (i.e. every output/bell
  resets it); when the timer fires, `alerts_queue(w, WINDOW_SILENCE)` (alerts.c:42–49).
  `monitor-silence 0` disables.

`alerts_queue` (alerts.c:157–180) sets the flag bits on the **window**, and if the
corresponding monitor option is enabled queues the window for a deferred (next event
loop pass) `alerts_callback`, which runs the three checks and then clears the window's
`WINDOW_ALERTFLAGS` (alerts.c:51–68) — the *winlink* flags are what persist.

### Monitor options → winlink flags → actions

For each alert type X ∈ {bell, activity, silence}, `alerts_check_X`
(alerts.c:182–290) runs only if the window flag is set **and** the window option
`monitor-X` is enabled. Then, for **every winlink of the window across all sessions**:

1. Set the winlink flag (`WINLINK_BELL` = shows `!`, `WINLINK_ACTIVITY` = `#`,
   `WINLINK_SILENCE` = `~`) and refresh that session's status line — **only if** the
   winlink is not the session's current window, or the session has no attached clients
   (`s->curw != wl || s->attached == 0`, alerts.c:202/238/274). Activity and silence
   skip winlinks already flagged; bell re-alerts even when already flagged
   (alerts.c:196–199 comment).
2. Consult `X-action` (`bell-action`/`activity-action`/`silence-action`; values
   `none|any|current|other`, alerts.c:70–89): `any` → always proceed; `current` → only
   if this winlink **is** the session's current window; `other` → only if it is not;
   `none` → stop (no notify, no message, no bell passthrough — but the status flag from
   step 1 is still set).
3. Fire the control notification (`alert-bell` etc.), then — once per session
   (guarded by the transient `SESSION_ALERTED` flag, alerts.c:194/210–212) —
   `alerts_set_message` (alerts.c:292–325): for each non-control client attached to the
   session, using `visual-X`:
   - `off`: send a terminal **bell** to the client (`TTYC_BEL`), no message;
   - `on`: show a status **message** only;
   - `both`: bell **and** message.
   Message text: `"Bell in current window"` (etc.) when the alerting winlink is that
   session's current window, else `"Bell in window %d"` with the **winlink index**.

### Clearing

Alert winlink flags clear when the window becomes the current window of any session:
`session_set_current` → `winlink_clear_flags(wl)` (session.c:493; window.c:2083–2096)
clears the window's flags and **every winlink of that window in every session**,
refreshing each affected session's status line. They are also cleared on
`session_detach` (session.c:348), on `kill-session -C` (clears all winlink+window alert
flags in the session, cmd-kill-session.c:65–70), when a client attaches
(`server_client_set_session` clears the current winlink's flags, server-client.c:428),
and are not copied when… (they **are** copied on group sync and renumber).

### Format flags

`#{window_flags}` (`#F`) via `window_printable_flags(wl, escape=1)` (window.c:1027–1053)
appends, in this exact order (multiple can appear at once):

| char | meaning | source |
|---|---|---|
| `#` | activity alert (doubled to `##` in `window_flags` so formats don't re-expand; raw single `#` in `#{window_raw_flags}`) | `WINLINK_ACTIVITY` |
| `!` | bell alert | `WINLINK_BELL` |
| `~` | silence alert | `WINLINK_SILENCE` |
| `*` | current window | `wl == s->curw` |
| `-` | last window | `wl == TAILQ_FIRST(&s->lastw)` |
| `M` | contains the marked pane | `marked_pane.wl` |
| `Z` | window is zoomed | `WINDOW_ZOOMED` |

(There is no precedence — they concatenate, e.g. `#!*Z`.) Individual boolean formats
also exist: `window_activity_flag`, `window_bell_flag`, `window_silence_flag`,
`window_active`, `window_last_flag`, `window_marked_flag`, `window_zoomed_flag`.
Status defaults `window-status-format` and `window-status-current-format` are both
`#I:#W#{?window_flags,#{window_flags}, }` (options-table.c:1817–1839) — note the
trailing space when there are no flags. `list-windows` uses `#{window_raw_flags}`
(cmd-list-windows.c:30–40). Pane flags (`window_pane_printable_flags`,
window.c:1055–1072): `*` active, `-` last pane, `Z` zoomed, `F` floating.

`#{session_attached}` = count of attached clients; `session_alerted` internal flag is
transient. `list-sessions` default template (cmd-list-sessions.c:31–36):
`#{session_name}: #{session_windows} windows (created #{t:session_created})`
`#{?session_grouped, (group ,}#{session_group}#{?session_grouped,),}`
`#{?session_attached, (attached),}`.

---

## 6. Window/session lifecycle

### Last pane in a window dies

`server_destroy_pane` (server-fn.c:313–388): consult pane option `remain-on-exit`
(`off|on|failed|key`, default **off**, options-table.c:1657–1665):

- `on` (1) / `key` (3): keep the dead pane; stamp `dead_time`, notify `pane-died`, draw
  the `remain-on-exit-format` message ("Pane is dead...") on the pane's last line, hide
  the cursor, mark for redraw. (`key`: any keypress later destroys it.)
- `failed` (2): keep only if the process exited non-zero; else fall through to destroy.
- `off` (0): notify `pane-exited`, unzoom, remove the pane from layout/window; **if the
  window now has no panes → `server_kill_window(w, renumber=1)`**, else redraw.

### Killing a window (kill-window, or last pane exited)

`server_kill_window(w, renumber)` (server-fn.c:199–222): for **every session** that has
the window: unzoom, then repeatedly `session_detach` each winlink of that window.
`session_detach` (session.c:340–358):

```c
if (s->curw == wl &&
    session_last(s) != 0 && session_previous(s, 0) != 0)
        session_next(s, 0);
```

**Focus handoff when the current window dies: last-used window first (top of the lastw
stack), then the previous (next lower index, with wrap), then the next.** Alert flags
are cleared, `window-unlinked` notified, the winlink removed from lastw and the tree.
If the session becomes empty, `session_detach` returns 1 and the whole session (or its
whole session group) is destroyed via `server_destroy_session_group`. Otherwise the
session group is redrawn and (if `renumber-windows` on) renumbered. Ends with
`recalculate_sizes()`.

`kill-window -a` kills all **other** windows (optionally filtered with `-f`), one at a
time, never touching the current window unless it appears at multiple indexes
(cmd-kill-window.c:92–135).

### Last window in a session / kill-session

Session destruction path is always: `server_destroy_session(s)` (redirect/detach every
client per `detach-on-destroy`, §4) **then** `session_destroy(s, 1, ...)`
(session.c:195–229: remove from tree, notify `session-closed`, drop lock timer, leave
its group, drain lastw, unlink every window — windows only die when their global
refcount hits zero, so windows shared with other sessions survive).

- `kill-session [-t]`: destroys the target session; each attached client either
  switches session or exits per `detach-on-destroy` (§4). `-a`: kill all **except**
  target (with optional `-f filter`); `-C`: don't kill anything, just clear all alert
  flags in the session; `-g`: kill the target's entire session group
  (cmd-kill-session.c:50–101).
- Which session becomes current for a client is exactly the `detach-on-destroy`
  resolution — there is no separate "focus handoff" concept for sessions.

### Server exit

When the last session dies the server exits (server.c checks `RB_EMPTY(&sessions)`;
`exit-empty` option, on by default). Not part of this domain but note the option exists.

---

## 7. Client size reconciliation (resize.c)

Constants: `WINDOW_MINIMUM` = `PANE_MINIMUM` = 1, `WINDOW_MAXIMUM` = 10000
(tmux.h:107–112).

### The window-size option

`window-size` (window option, choice `largest|smallest|manual|latest`, default
**latest**, options-table.c:1778–1790). A **window** (not a session, not a client) has
one size; every attached client viewing it renders that size, padding/clipping its own
terminal (`tty_update_window_offset`).

### recalculate_sizes / recalculate_size

`recalculate_sizes[_now]` (resize.c:419–460) runs after nearly every attach/detach/
resize/select event. It first recomputes each session's `attached` count from live
clients (skipping suspended/dead/exiting ones — `CLIENT_UNATTACHEDFLAGS`), sets
`CLIENT_STATUSOFF` for clients too small to show the status line, then calls
`recalculate_size(w, now)` for **every window** (resize.c:352–417):

- `type = window-size` (window option), `current = aggressive-resize` (window option).
- `clients_calculate_size` (resize.c:113–264) scans clients:
  - Ignored clients (`ignore_client_size`, resize.c:68–96): no session; dead/suspended/
    exiting; `CLIENT_IGNORESIZE` (e.g. read-only attach `-r`) **if** at least one
    non-flagged client exists; control clients that never reported a size.
  - Relevance filter (`recalculate_size_skip_client`, resize.c:336–350): with
    `aggressive-resize` **on** (`current=1`), only clients whose session's **current
    window is this window** count; with it **off**, any client whose session **contains**
    the window counts.
  - `largest`: max cx×cy over relevant clients; `smallest`: min; `latest`: only the
    client stored in `w->latest` counts when more than one client has the window
    (resize.c:144–170) — `w->latest` is updated on select-window, attach, and input.
    A client's contribution is `tty.sx` × `tty.sy - status_line_size(c)`
    (resize.c:181–188), or its per-window size if it set one
    (control-mode `refresh-client -C`).
  - `manual`: size = `w->manual_sx/sy` (set by `resize-window`; initialized to the
    creation size, window.c:329–330). Manual still gets clamped by any per-client
    window sizes (resize.c:217–248).
- If **no relevant client** exists (e.g. all sessions containing the window are
  detached) the window is **left at its current size** (`changed` false → only offsets
  updated, resize.c:250–263 return 0 → resize.c:394–398).
- On change: resize immediately if `now` or manual; otherwise latch
  `w->new_sx/new_sy` + `WINDOW_RESIZE` flag; the actual `resize_window` happens on the
  next redraw pass for the window (deferred resize; resize.c:400–416).
- `resize_window` (resize.c:25–66) clamps to [1,10000], unzooms, resizes the layout
  tree, never lets the window be smaller than its layout minimum, restores zoom, and
  notifies `window-layout-changed` + `window-resized`.

### default_window_size (new windows)

`default_window_size(c, s, w=NULL, ..., type=-1)` (resize.c:277–334), used by
`spawn_window` (spawn.c:153):

- type −1 → the **global** window option `window-size`.
- `latest` + a usable creating client → that client's size minus status line.
- Otherwise: run the same scan over clients whose session is `s` (w==NULL variant of
  the skip function, resize.c:266–275); if nothing found (detached creation) → parse
  the session's `default-size` option, fallback 80×24; clamp to [1,10000].
- A control client never contributes as the "given client" (resize.c:303–306).

Consequence for multiple clients at different sizes: with the default
`window-size latest`, the window tracks whichever client used it most recently;
`smallest` reproduces the classic tmux ≤2.9 behavior (and `aggressive-resize on` limits
that to clients actually looking at the window); `largest` lets small clients view a
clipped region.

---

## 8. Defaults table (every option touched above)

| option | scope | type / values | default | effect |
|---|---|---|---|---|
| `base-index` | session | number 0..INT_MAX | `0` | first index tried for new windows (options-table.c:752) |
| `renumber-windows` | session | flag | `off` | auto-renumber on window kill; also enables `move-window` src renumber (978) |
| `default-size` | session | string `NxN` | `80x24` | window size when no client available (784) |
| `destroy-unattached` | session | `off\|on\|keep-last\|keep-group` | `off` | destroy sessions with no attached clients (792) |
| `detach-on-destroy` | session | `off\|on\|no-detached\|previous\|next` | `on` | where a client goes when its session dies (801) |
| `bell-action` | session | `none\|any\|current\|other` | `any` | which windows' bells alert (761) |
| `activity-action` | session | same | `other` | which windows' activity alerts (733) |
| `silence-action` | session | same | `other` | which windows' silence alerts (1011) |
| `visual-bell` | session | `off\|on\|both` | `off` | off=bell passthrough, on=message, both=both (1244) |
| `visual-activity` | session | same | `off` | ditto for activity (1235) |
| `visual-silence` | session | same | `off` | ditto for silence (1253) |
| `monitor-bell` | window | flag | `on` | watch for bells (1480) |
| `monitor-activity` | window | flag | `off` | watch for output (1473) |
| `monitor-silence` | window | number (seconds) | `0` (off) | alert after N s of silence (1487) |
| `automatic-rename` | window | flag | `on` | rename window from format on output (1319) |
| `automatic-rename-format` | window | string | `#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}` | name source (1326) |
| `allow-rename` | window+pane | flag | `off` | permit ESC-k rename escape only (1295) |
| `aggressive-resize` | window | flag | `off` | size only from clients currently viewing the window (1274) |
| `window-size` | window | `largest\|smallest\|manual\|latest` | `latest` | how window size is chosen (1778) |
| `remain-on-exit` | window+pane | `off\|on\|failed\|key` | `off` | keep dead panes (1657) |
| `history-limit` | session | number | `2000` | scrollback lines per pane (861) |
| `default-command` | session | string | `""` (login shell) | command for new panes (769) |
| `default-shell` | session | string | `/bin/sh` (`_PATH_BSHELL`) | shell path (777) |
| `set-titles` | session | flag | `off` | set outer terminal title (997) |
| `window-status-format` | window | string | `#I:#W#{?window_flags,#{window_flags}, }` | status tab (1833) |
| `window-status-current-format` | window | string | same | current tab (1817) |

(`lock-after-time` also reads session activity — out of scope here.)

## Default prefix-table keybindings in this domain (key-bindings.c:388–456, 525–537)

| key | command |
|---|---|
| `$` | `command-prompt -I'#S' { rename-session -- '%%' }` |
| `&` | `confirm-before -p"kill-window #W? (y/n)" kill-window` |
| `'` | `command-prompt -pindex { select-window -t ':%%' }` |
| `(` | `switch-client -p` |
| `)` | `switch-client -n` |
| `,` | `command-prompt -I'#W' { rename-window -- '%%' }` |
| `.` | `command-prompt { move-window -t '%%' }` |
| `0`..`9` | `select-window -t:=0` .. `-t:=9` (exact index) |
| `L` | `switch-client -l` |
| `c` | `new-window` |
| `d` | `detach-client` |
| `f` | `command-prompt { find-window -Z -- '%%' }` |
| `l` | `last-window` |
| `n` | `next-window` |
| `p` | `previous-window` |
| `s` | `choose-tree -Zs` (sessions) |
| `w` | `choose-tree -Zw` (windows) |
| `M-n` | `next-window -a` |
| `M-p` | `previous-window -a` |
| root `MouseDown1Status` | `switch-client -t=` (click a status tab) |
| root `WheelDownStatus` / `WheelUpStatus` | `next-window` / `previous-window` |

(Root-table `D` is `choose-client`; `!` break-pane etc. are pane-domain.)

---

## Windows/winmux applicability notes

- **`pane_current_command` is Unix-specific.** tmux derives the automatic-rename name
  from the foreground process group on the pane tty (`osdep_get_name`), not from escape
  sequences. ConPTY has no tcgetpgrp; winmux's existing OSC-0/2-title-based
  automatic-rename is a reasonable substitute but is *behaviorally different*: tmux would
  rename to `pwsh`/`vim` (the running binary), and only uses the OSC title if the user
  puts `#{pane_title}` in `automatic-rename-format`. Document as a divergence; the
  500 ms throttle (winmux already has one) matches `NAME_INTERVAL`.
- **`allow-rename` gates ESC-k only.** Accepting `set -g allow-rename off` as a no-op is
  correct as long as winmux never implements the ESC-k rename escape; if it does, the
  option must gate it, default off, with the empty-name reset semantics of
  input.c:2818–2827.
- **Bell passthrough** (`visual-bell off` sends BEL to each attached client's terminal)
  works fine over ConPTY/VT — just forward `\a` to the client. Windows Terminal will
  flash/ding per its own settings.
- **`MSG_DETACHKILL` sends SIGHUP to the client's parent** (client.c:417–422) — meant to
  kill the user's login shell so a stolen session's old terminal doesn't sit at a
  prompt. No direct Windows equivalent; the honest mapping is "detach the other client
  and let its process exit", i.e. treat `-x` as `-d`.
- **termios capture** on new-session (saved tio reused for new panes) has no ConPTY
  analogue; ignore.
- **cwd semantics** (`-c`, client cwd inheritance) map to `CreateProcess`
  lpCurrentDirectory; the "client's cwd" concept requires the client to report it
  (tmux reads it from the client process; winmux client can send it in its hello frame).
- **fnmatch** for session/window name patterns needs a glob implementation on Windows
  (`*`, `?`, `[...]`); tmux uses it un-flagged (`fnmatch(pat, name, 0)`), and find-window
  uses format `m:`-style glob or POSIX regex with `-r`. Rust `regex` + a small glob
  translator suffices.
- **Session names sort by `strcmp`** (byte order) — switch-client -n/-p order and the
  sessions RB tree both rely on this; keep byte-wise ordering, not locale collation.
- Everything else in this domain (indexing, lastw stack, alerts state machine, resize
  arithmetic, target resolution grammar) is pure data-structure logic with no platform
  dependency and should be replicated exactly as specified above.
