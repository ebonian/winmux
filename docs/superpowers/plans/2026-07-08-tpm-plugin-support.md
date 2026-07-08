# TPM + tmux-plugin ecosystem support (future sub-project — SP5)

> **Status: PLANNED, not scheduled.** User directive (2026-07-08): explore and keep as a future
> plan; no implementation in the SP4 session. When SP5 starts, write a full TDD implementation
> plan (writing-plans skill) from this document against the then-current code.
>
> This document is the durable copy of the SP5 research (originally
> `.superpowers/sdd/tpm-research.md`, which is gitignored scratch). It maps, from the actual
> shell source of TPM and six popular plugins, the exact winmux surface needed — an 8-rung
> feature ladder, per-plugin requirement tables, and the honest Windows boundary.

## Roadmap summary

- **Rungs 1-4 (minimal — TPM installs/loads, tmux-sensible + tmux-prefix-highlight work):**
  `@`-prefixed user options with `show -gqv` semantics; `run-shell [-b]` via `sh -c` (Git Bash)
  plus a `tmux`-named shim (`-V` must digit-strip ≥ 1.9) and a `$TMUX`-style env var;
  `set-environment -g`/`show-environment -g`; format engine v2 (`#{?cond,a,b}` ternaries,
  `#{client_prefix}`/`#{pane_in_mode}`, inline `#[fg=..]` style tokens). No hooks needed.
- **Rungs 5-6 (medium — tmux-yank + vim-tmux-navigator):** `copy-pipe`/`copy-pipe-and-cancel`;
  `send-keys -N`; `display-message -p -F`; native `#{pane_current_path}`/`#{pane_current_command}`
  (Win32 APIs, not `ps`); `if-shell [-b] [-F]`; `select-pane -l`.
- **Rungs 7-8 (large — tmux-resurrect + tmux-continuum):** `list-panes/-windows/-sessions -F`
  (~12 format vars incl. `#{window_layout}` serialization) + `select-layout`;
  `capture-pane -epJ -S`; `-c <dir>` on new-session/new-window/split-window;
  `switch-client -t`; `move-window`; `select-pane -T`; per-window option scope;
  `#()` shell-out in status-right (cached, async) + `#{start_time}`. No `set-hook` needed.
- **Windows boundary (permanent):** `ps`-based tty introspection can never work under ConPTY
  (navigator's default `is_vim`, resurrect's `ps` save strategy) — mitigate with native
  `#{pane_current_command}` + documented plugin overrides; watch CRLF shebangs on plugin
  clones and MSYS-vs-Windows path dialect in paths plugins hand back.

---

# TPM + popular-plugin support research for winmux

Research date: 2026-07-07. Sources: actual shell source of tmux-plugins/tpm@master and each plugin's
repo (fetched raw), cross-checked against winmux's `src/cmd.rs`, `src/options.rs`, and a repo-wide
grep confirming zero occurrences of `run-shell`, `if-shell`, `set-environment`, `show-environment`,
`capture-pane`, `list-panes`, `set-hook`, `pane_current_path`, or inline `#[...]` style parsing in
`src/` today.

Scope note: winmux already has (relevant to this doc): the unified dispatcher (`server::dispatch`),
`.tmux.conf` loading, `set-option`/`show-options` over a **closed** typed registry
(`options.rs::SPECS`), `bind-key` with unresolved-`RawCmd` tails, `send-keys` (incl. `-X` copy
commands), `display-message` (status-line only), `source-file`, `command-prompt -I`, paste buffers
(`set-buffer`/`paste-buffer`/`list-buffers`/`delete-buffer`), copy mode with
`copy-selection-and-cancel` (buffer-only, no pipe), and the `expand_format` subset
(`#S #I #W #F #P #H ##`, three `#{long_forms}`, strftime).

---

## §1 TPM core requirements

TPM is ~10 small bash scripts. Every script is `#!/usr/bin/env bash` (README: "Requirements:
tmux 1.9+, git, bash"). It shells out to a binary literally named `tmux` on PATH.

### 1.1 Every tmux invocation in TPM's source

From `tpm` (root script):

```sh
local option_value="$(tmux show-option -gqv "$option")"        # get_tmux_option helper
tmux show-environment -g "$DEFAULT_TPM_ENV_VAR_NAME" >/dev/null 2>&1   # exit-code probe: is TMUX_PLUGIN_MANAGER_PATH set?
tmux set-environment -g "$DEFAULT_TPM_ENV_VAR_NAME" "$tpm_path"
tmux bind-key "$install_key" run-shell "$BINDINGS_DIR/install_plugins"   # default I
tmux bind-key "$update_key"  run-shell "$BINDINGS_DIR/update_plugins"    # default U
tmux bind-key "$clean_key"   run-shell "$BINDINGS_DIR/clean_plugins"     # default M-u
```

From `scripts/check_tmux_version.sh` (runs FIRST, before anything else):

```sh
tmux -V                                    # version string, digit-stripped, compared to "1.9"
local option_value=$(tmux show-option -gqv "$option")
tmux set-option -gq display-time "$display_duration"
tmux display-message "$message"
tmux set-option -gq display-time "$saved_display_time"
```

From `scripts/helpers/plugin_functions.sh`:

```sh
tmux start-server\; show-environment -g TMUX_PLUGIN_MANAGER_PATH | cut -f2 -d=
tmux start-server\; show-option -gqv "$tpm_plugins_variable_name"        # @tpm_plugins (legacy)
```

(note the chained `start-server \; <cmd>` — one invocation, tmux's own `;` separator; winmux's
`cmd::parse_line` already splits on `;` identically, but `start-server` is not a known command)

From `scripts/helpers/tmux_utils.sh`:

```sh
tmux source-file $(_get_user_tmux_conf) >/dev/null 2>&1    # reload_tmux_environment, run before AND after install
```

From `scripts/helpers/tmux_echo_functions.sh`:

```sh
tmux show -gw mode-keys | grep -q emacs     # `show` alias + combined -gw flags
tmux run-shell "echo '$message'"            # ALL user-facing output goes through run-shell echo
```

From `bindings/update_plugins`:

```sh
tmux command-prompt -p 'plugin update:' "run-shell '... update_plugin_prompt_handler.sh %1'"
```

(`command-prompt -p <prompt>` with a **template** whose `%1` is replaced by the committed input —
winmux's `CommandPrompt` has `-I` only, no `-p`, no template argument, no `%1` substitution)

### 1.2 Consolidated TPM command surface

| Command | Flags used | winmux today |
|---|---|---|
| `run-shell` / `run` | plain (never `-b` in TPM itself; plugins use `-b`) | **missing entirely** |
| `show-option` / `show` | `-g -q -v`, `-gw`, arbitrary `@name` | `show-options [-g] [name]` only; no `-q`/`-v`/`-w`; unknown name = error |
| `set-option` / `set` | `-g -q`, `@name` values | has `-g -w -a -u`; no `-q`; `@name` = `Err("unknown option")` |
| `set-environment` | `-g NAME VALUE` | **missing entirely** |
| `show-environment` | `-g NAME` (prints `NAME=value`; **exit code nonzero when unset** — TPM's idempotency check) | **missing entirely** |
| `bind-key` | `<key> run-shell <path>` | works once `run-shell` resolves (tail is late-bound `RawCmd`) |
| `display-message` | bare text | present |
| `source-file` | path | present |
| `start-server` | chained no-op | missing (trivial: accept + no-op, server already running) |
| `command-prompt` | `-p prompt` + `%1` template | has `-I` only |
| `tmux -V` | CLI version flag | **must digit-strip to ≥ 1.9** or `check_tmux_version.sh` aborts TPM with an error message |

### 1.3 Option/env syntaxes

- `set -g @plugin 'user/repo'` — **TPM never reads `@plugin` through tmux.** It awk-greps the raw
  `.tmux.conf` text: `awk '/^[ \t]*set(-option)? +-g +@plugin/ ...'`. winmux only needs `set -g
  @plugin ...` to be **accepted without error** during config load (today it hard-errors), not
  stored meaningfully.
- `@tpm_plugins` (legacy space-joined list) — read via `show-option -gqv`, so this one DOES need
  real user-option storage.
- `@tpm-install` / `@tpm-update` / `@tpm-clean` — keybinding overrides, read via `show-option -gqv`
  with empty→default fallback. The whole helper pattern is:
  `[ -z "$option_value" ] && echo "$default"` — i.e. **missing option must yield empty string +
  exit 0**, never an error line.
- `TMUX_PLUGIN_MANAGER_PATH` — the plugin install dir (default `$HOME/.tmux/plugins/`, or
  `$XDG_CONFIG_HOME/tmux/plugins/` when the XDG conf exists), plumbed exclusively through
  `set-environment -g` / `show-environment -g`.
- TPM self-locates via bash `BASH_SOURCE[0]` from the literal path in `run '~/.tmux/plugins/tpm/tpm'`
  — no tmux involvement; works under Git Bash (`~` = `$HOME` = `/c/Users/<u>` in MSYS).

### 1.4 run-shell semantics TPM depends on

- TPM never passes `-b`; real tmux's plain `run-shell` still executes the command **without
  blocking the server loop** (forked; stdout shown as a status message when it exits). winmux's
  `run-shell` must be spawn-and-return with a completion `ServerEvent` — a git clone over the
  network can take minutes and must not freeze the event loop.
- The spawned script then makes many ordinary synchronous `tmux <cmd>` client round-trips back into
  the server (over winmux's named pipe) — so `run-shell` children must inherit whatever env lets
  the `tmux` shim find the right server (socket name; see §4).
- `source_plugins.sh` executes each plugin's `*.tmux` file **directly** (`"$tmux_file" >/dev/null
  2>&1` relying on the `#!/usr/bin/env bash` shebang) — works under Git Bash provided LF endings
  (see §4 CRLF gotcha).

### 1.5 Minimal winmux surface for `run '~/.tmux/plugins/tpm/tpm'` to install/load plugins

1. `run-shell` command (async spawn via `sh -c`, Git Bash's `sh.exe` from PATH).
2. A `tmux`-named shim on PATH resolving to winmux, whose `-V` prints something like `tmux 3.4`
   (digit-strips ≥ 1.9), and which targets the correct server instance from the pane environment.
3. User options: `@`-prefixed names storable/retrievable; `show-option(s) -gqv` semantics
   (`-v` bare value, `-q` + missing → empty output, exit 0); `set-option -gq` accepted.
4. `set-environment -g` / `show-environment -g` global env table (with unset → nonzero exit).
5. `start-server` accepted as a no-op; `set -g @plugin ...` lines in config not erroring.
6. Already present: `bind-key`, `display-message`, `source-file`, `send-keys`.
7. For the `prefix-U` update flow only: `command-prompt -p <prompt> <template>` with `%1`.

### 1.6 Windows prior art

TPM's README claims "Tested and working on Linux, OSX, and Cygwin." Real tmux runs under
Cygwin/MSYS2 (users copy `tmux.exe` + `msys-event*.dll` into Git Bash), and TPM works there
unmodified — the blocker on Windows has historically been tmux itself, not TPM. Known failure
class: `core.autocrlf=true` breaking `*.tmux` shebang execution (TPM's own
`docs/tpm_not_working.md`; tpm#271). No prior art exists for TPM against a non-tmux
protocol-compatible server — winmux would be first.

---

## §2 Per-plugin requirement tables

### 2.1 tmux-sensible (`sensible.tmux`)

Everything is conditional: helpers `option_value_not_changed`/`key_binding_not_set` only override
still-at-default values (via `show-option -gv/-sv` and `list-keys | grep`).

| Needs | Detail | winmux gap |
|---|---|---|
| `show-option -gv <opt>` / `-sv <opt>` | value-only print | `-v` flag; `-s` (server scope) |
| `set-option -s escape-time 0` | **server scope `-s`** | no `-s`; alias to global is fine |
| `set-option -g history-limit 50000`, `display-time 4000`, `status-interval 5` | | present |
| `set-option -g status-keys emacs`, `focus-events on` | | options not in `SPECS` (add as accepted-inert) |
| `set-window-option -g aggressive-resize on` | **`setw`/`set-window-option` alias** | alias missing (option exists) |
| `list-keys` (grep'd) | | present |
| `unbind-key C-b`, `bind-key <prefix> send-prefix`, `bind-key C-p previous-window` etc. | | present |
| `bind-key R run-shell "tmux source-file ...; tmux display-message ..."` | reload binding | needs `run-shell` |

Hooks: none. `#()` status shell-out: none. POSIX tools: `uname` only (macOS gate). **Verdict:
portable; nearly works today** — gaps are `-v`/`-s` flags, `setw` alias, two inert options,
`run-shell` (for the optional R binding).

### 2.2 tmux-prefix-highlight (`prefix_highlight.tmux`)

Runs ONCE at load: reads `status-right`/`status-left` (`show-option -gqv`), string-substitutes the
literal placeholder `#{prefix_highlight}` with a prebuilt conditional format, writes back with
`set-option -gq`. **No hooks** (confirmed — zero `set-hook` in source). **No `#()` shell-out.**
Liveness comes entirely from the status bar re-evaluating the format each redraw.

The installed format is (schematically):

```
#{?client_prefix,#[fg=..]#[bg=..] ^B ,#{?pane_in_mode,..copy..,#{?synchronize-panes,..sync..,}}}#[default]
```

| Needs | winmux gap |
|---|---|
| `#{?cond,true,false}` ternary in format engine | **missing** — `expand_format` has no conditionals |
| `#{client_prefix}` format var | state exists per-client (prefix machine); not exposed |
| `#{pane_in_mode}` format var | derivable from `ClientMode::Copy`; not exposed |
| `#{synchronize-panes}` (option-as-format-var) | option doesn't exist; only if `show_sync_mode` on |
| Inline `#[fg=..,bg=..]`/`#[default]` style tokens inside status strings | **missing** — confirmed no `#[` parsing in `status.rs`/render path |
| `show-option -gqv` / `set-option -gq` on `@prefix_highlight_*` + `status-format[0]`/`[1]` | user options; indexed names must not hard-error |

POSIX tools: `sed`/`tr` at load only (Git Bash fine). **Verdict: portable; blocked purely on the
format engine** (ternary + two vars + inline styles).

### 2.3 tmux-yank (`yank.tmux`, `scripts/helpers.sh`, `copy_line.sh`, `copy_pane_pwd.sh`)

| Needs | winmux gap |
|---|---|
| `show-option -gqv @yank_*`, `show-option -gwv mode-keys` | user options, `-v`/`-w` flags |
| `bind-key -T copy-mode[-vi] y send-keys -X copy-pipe-and-cancel "<clipboard cmd>"` | **`copy-pipe`/`copy-pipe-and-cancel` CopyAction missing** — and it takes a *shell command argument*, unlike every existing arg-less `CopyAction` |
| `bind-key y run-shell -b "$SCRIPTS_DIR/copy_line.sh"` | `run-shell -b` |
| `tmux copy-mode`, `send -X begin-selection`, `send -X -N 150 cursor-down`, `end-of-line`, `previous-word`, `next-word-end` | present except **`-N <repeat>` on send-keys** |
| `tmux display -p -F "#{pane_current_path}"` | **`display-message -p -F`** (print-to-stdout mode) + `#{pane_current_path}` var — both missing |
| `tmux set-buffer "$payload"` | present |
| Clipboard tool | source already branches: `command_exists "clip.exe"` → `cat \| clip.exe` (WSL-labeled but fires on native Windows too — `C:\Windows\System32\clip.exe` is always on PATH) |

Hooks: none. **Verdict: portable with Git Bash** (clipboard solved by its own `clip.exe` branch);
blocked on `copy-pipe(-and-cancel)`, `run-shell -b`, `display -p -F`, `#{pane_current_path}`.

### 2.4 vim-tmux-navigator (README `.tmux.conf` snippet; `navigator.tmux` applies the same)

```tmux
is_vim="ps -o state= -o comm= -t '#{pane_tty}' | grep -iqE '^[^TXZ ]+ +...vim...$'"
bind-key -n 'C-h' if-shell "$is_vim" 'send-keys C-h' 'select-pane -L'   # + C-j/C-k/C-l, C-\
bind-key -T copy-mode-vi 'C-h' select-pane -L                            # + ... 'C-\' select-pane -l
if-shell -b '[ "$(echo "$tmux_version < 3.0" | bc)" = 1 ]' "bind-key ..."
```

| Needs | winmux gap |
|---|---|
| `if-shell [cond] [true-cmd] [false-cmd]` and `if-shell -b 'cond' 'cmd'` | **missing entirely** |
| `select-pane -l` (last pane) | flag missing; maps to existing `LastPane` |
| `bind-key -n`, `send-keys C-h`, `select-pane -L/-D/-U/-R` | present |
| `#{pane_tty}` format var + `ps -o state= -o comm= -t <tty>` | **POSIX wall** — no ConPTY tty path; MSYS `ps` sees only MSYS processes |
| `tmux -V` parsing (`bc`) | cosmetic |

Hooks: none. **Verdict: `if-shell` is implementable, but the default `is_vim` detection is
Unix-bound.** The honest path: winmux implements `#{pane_current_command}` **natively** (Win32
process-tree walk from the ConPTY child PID, no `ps`), and users set the plugin's supported
`@vim_navigator_check` override (or a documented winmux snippet) to test it via `if-shell -F
'#{m:*vim*,#{pane_current_command}}'` — turning a POSIX shell-out into a pure format match.

### 2.5 tmux-resurrect (save/restore command surface — the big one)

Save side (exact format strings, tab-delimited):

```sh
tmux list-panes -a -F "pane\t#{session_name}\t#{window_index}\t#{window_active}\t:#{window_flags}\t#{pane_index}\t#{pane_title}\t:#{pane_current_path}\t#{pane_active}\t#{pane_current_command}\t#{pane_pid}\t#{history_size}"
tmux list-windows -a -F "window\t#{session_name}\t#{window_index}\t:#{window_name}\t#{window_active}\t:#{window_flags}\t#{window_layout}"
tmux list-sessions -F "#{session_grouped}\t#{session_group}\t#{session_id}\t#{session_name}"
tmux display-message -p -F "state\t#{client_session}\t#{client_last_session}"
tmux capture-pane -pJ -t "$pane_id"                       # and: capture-pane -epJ -S "$start" -t ...
tmux display -p -t "$pane_id" -F "#{history_size}" / "#{cursor_y}"
tmux show-window-options -vt "$sess:$win" automatic-rename    # PER-WINDOW option scope
```

Restore side:

```sh
TMUX="" tmux -S "$(echo $TMUX | cut -d, -f1)" new-session -d -s NAME -c DIR   # socket re-derived from $TMUX!
tmux new-window -d -t S:W -c DIR [cmd]  ;  tmux split-window -t S:W -c DIR [cmd]
tmux select-layout -t S:W "$layout_string"  ;  tmux move-window -s S:old -t S:new
tmux rename-window / resize-pane -t .. -U 999 / resize-pane -Z / kill-pane / kill-session -t 0
tmux set-option [-u] -t S:W automatic-rename [VAL]  ;  tmux select-pane -t S:W.P -T "$title"
tmux switch-client -t TARGET  ;  tmux has-session -t NAME  ;  tmux send-keys -t T "cmd" Enter
```

Options: ~14 `@resurrect-*` user options via `show-option -gqv`; also reads `default-shell`,
`default-command`, `base-index`, `display-time`. Hooks: **none** (its "hooks" are
`@resurrect-hook-*` options eval'd by its own bash). Process detection: default strategy shells
`ps -ao "ppid,args" | grep ^$PANE_PID` (**POSIX wall**); `#{pane_current_command}` is saved
regardless and is the portable fallback. Save dir: `$HOME/.tmux/resurrect` (fine under Git Bash).

**winmux gaps (all missing today):** `capture-pane -e -p -J -S`, `list-panes -a/-t -F`,
`list-windows -a -F` (with formats), `list-sessions -F`, `display-message -p [-t] -F`,
`select-layout` (+ `#{window_layout}` serialization), `move-window`, `switch-client -t`,
`select-pane -T`, per-window `set-option -t` / `show-window-options -vt`, `-c <dir>` on
new-session/new-window/split-window, `-d` on new-window, `send-keys -t` targeting arbitrary
`sess:win.pane`, and format vars `pane_current_path`, `pane_pid`, `pane_current_command`,
`pane_title`, `history_size`, `cursor_y`, `window_layout`, `session_id`, `session_grouped`,
`client_session`, `client_last_session`. Plus a `$TMUX`-style env var in panes whose first
comma-field the shim accepts as `-S <socket>`.

### 2.6 tmux-continuum (`continuum.tmux`)

**No `set-hook` anywhere** (confirmed). Autosave = prepending the literal token
`#($CURRENT_DIR/scripts/continuum_save.sh)` to `status-right` via `set-option -gq`; tmux re-runs
`#(...)` on every status redraw (paced by `status-interval`); the script self-throttles against
`@continuum-save-interval` / `@continuum-save-last-timestamp`. Autorestore = plain background
process (`continuum_restore.sh &`) launched from the `.tmux` bootstrap at TPM load, gated on
`tmux display-message -p -F '#{start_time}'` vs `@continuum-restore-max-delay`. Version guard =
digit-stripped `tmux -V`.

**winmux gaps:** `#(shell-command)` shell-out support in the status format engine (the single
biggest ask — result caching + async refresh, never block the render loop), `#{start_time}` format
var, user options, `display -p -F`. `@continuum-boot` (systemd/launchd autostart) is honestly
out of scope on Windows. `/tmp` mkdir-lock in `continuum_save.sh` works under Git Bash.

---

## §3 The feature ladder (prioritized increments, with winmux gap placement)

Ordering principle: each rung is independently shippable; rungs 1–3 make TPM itself work; 4–6 make
the "easy" plugins work; 7–8 are the heavy tail.

### Rung 1 — User options + option-flag parity (`options.rs`, `cmd.rs`, `server/dispatch`)

- `options.rs`: alongside the closed `SPECS` table, add an open `BTreeMap<String, String>` for
  `@`-prefixed names (tmux: user options are untyped strings; `set -g @x val` stores, `set -gu @x`
  removes, `show -gqv @x` prints value or empty). Names with `[idx]` suffixes (`status-format[0]`)
  should store-or-ignore, never hard-error.
- `cmd.rs`: `set-option` gains `-q` (accepted; suppresses nothing fatal) and `-s` (accept, alias to
  global); `show-options` gains `-q`, `-v` (value-only output), `-w`, `-s`; add `show-option`
  (singular) and `set-window-option`/`setw`/`show-window-options`/`showw` as aliases.
- `server/dispatch`: `show -gqv <missing>` → empty output + success; without `-q`, unknown → error
  (tmux behavior). `set` on unknown non-`@` option with `-q` → silently ignore.
- **What breaks without it:** every plugin's very first `get_tmux_option` call errors; TPM's key
  overrides, all `@plugin` config lines error at config load. **Unlocks:** the config-file half of
  every plugin.

### Rung 2 — `run-shell [-b] <command>` + the `tmux` shim (`cmd.rs`, `server/dispatch`, `server.rs`, new `shim`/cli work)

- `cmd.rs`: `RunShell { background: bool, command: String }` (`run-shell`/`run`; also accept `-t`
  and ignore). `start-server` as an accepted no-op command.
- `server.rs`: execute by spawning `sh -c <command>` — resolve `sh` from PATH (Git Bash's
  `sh.exe`; document fallback order: `sh` on PATH → `%ProgramFiles%\Git\usr\bin\sh.exe` →
  error "run-shell requires sh (Git Bash) on PATH"). Spawn on a thread; completion posts a
  `ServerEvent` carrying exit status + captured stdout; non-`-b` shows stdout as a status message
  and (tmux behavior) reports nonzero exit as `'<cmd>' returned <code>`. Never block the main loop.
- Child env must carry the server identity: set `TMUX=<pipe-or-socket-name>,<pid>,<session-id>`
  (or at minimum `WINMUX_SOCKET`) in every pane AND every run-shell child, and make the CLI honor
  it so bare `tmux <cmd>` reaches the right server; accept `-S <name>` as a synonym for `-L`.
- Ship a `tmux`(.exe) shim (copy/hardlink of winmux.exe dispatching on argv[0], or a tiny wrapper)
  and make `winmux -V`/`tmux -V` print `tmux 3.4` (digit-strippable; TPM aborts below 1.9,
  version-gated plugins compare ints).
- **What breaks without it:** `run '~/.tmux/plugins/tpm/tpm'` is an unknown command — TPM cannot
  even start. **Unlocks:** TPM bootstrap + every `bind-key ... run-shell` plugin binding.

### Rung 3 — `set-environment` / `show-environment` (`cmd.rs`, new env table in `server.rs`)

- Global `BTreeMap<String,String>` env table on the server (separate store from `Options`, like
  tmux). `set-environment -g NAME VALUE`, `show-environment -g NAME` printing `NAME=value` with
  **nonzero exit / error when unset** (TPM's idempotency probe reads the exit code), bare
  `show-environment [-g]` listing all. Session-scoped env can wait.
- Fold the table into the env of newly spawned panes and run-shell children (that's what the table
  is FOR in tmux).
- **What breaks without it:** TPM can't record/find `TMUX_PLUGIN_MANAGER_PATH`; install dir
  resolution fails. **Unlocks (with 1+2): TPM installs, updates, cleans, and loads plugins
  end-to-end.** tmux-sensible now also substantially works (add `status-keys`/`focus-events` as
  accepted-inert SPECS rows while here).

### Rung 4 — Format engine v2: ternaries, state vars, inline styles (`options.rs::expand_format`, `status.rs`, render path)

- `#{?cond,true,false}` with nesting and comma/brace escaping; a variable/option name as cond
  (empty/`0` = false).
- Expose vars: `#{client_prefix}` (acting client's prefix-pending flag — per-client, so
  `FormatCtx` needs client state), `#{pane_in_mode}`, plus option-name lookup fallback inside
  `#{...}` (tmux resolves unknown `#{names}` against options — how `#{synchronize-panes}` works).
- Inline `#[fg=..,bg=..]`/`#[default]` style tokens: `status.rs` span builder must split expanded
  text into styled spans (reuse `style::parse_style` on the bracket contents).
- **What breaks without it:** prefix-highlight renders its raw `#{?client_prefix,...}` text
  literally (as empty, today). **Unlocks: tmux-prefix-highlight fully**; also big real-world
  `.tmux.conf` compatibility (nearly every themed status line uses `#[...]` + `#{?...}`).

### Rung 5 — `copy-pipe` family + `display -p -F` + first pane vars (`cmd.rs`, `server/dispatch`, `grid`/`pty`)

- `CopyAction` gains `copy-pipe`, `copy-pipe-and-cancel` **with a shell-command argument** (pipe
  selection bytes to `sh -c <cmd>` stdin — reuses rung 2's spawn machinery; also write the buffer
  as `copy-selection-and-cancel` does). `send-keys -X` arm must pass the trailing argument;
  `send-keys` gains `-N <count>` repeat.
- `display-message -p [-t target] [-F fmt]`: print expanded format to the CLI client's stdout
  instead of the status line (headless/one-shot path already exists for `list-*`).
- Format vars: `#{pane_current_path}` (Win32: query the pane child's CWD, or track via OSC 9;9 /
  shell integration — document precision limits), `#{pane_current_command}` (walk the ConPTY
  child's process tree, take the deepest/foreground child's exe name — native, no `ps`).
- **Unlocks: tmux-yank fully** (its `clip.exe` branch already fires on native Windows), and the
  documented navigator override path.

### Rung 6 — `if-shell [-b] [-F]` + `select-pane -l` (`cmd.rs`, `server/dispatch`)

- `if-shell [-b] [-F] cond true-cmd [false-cmd]`: without `-F`, run cond via rung-2 shell, exit 0
  = true, then dispatch the chosen command string (re-parse via `cmd::parse_line` → late binding,
  same as `bind-key` tails); with `-F`, expand cond as a format (no shell) and test
  non-empty/non-zero. `-b` = don't block (dispatch on completion event).
- `select-pane -l` → existing `LastPane`; also accept `-t`.
- **Unlocks: vim-tmux-navigator's bindings** (detection still needs the rung-5
  `pane_current_command` override — see §4), and a huge fraction of real `.tmux.conf` files
  (version-gated config is idiomatic).

### Rung 7 — Introspection + targeted-restore command surface (`cmd.rs`, `server/dispatch`, `model.rs`, `grid.rs`)

For resurrect's save half: `list-panes [-a|-t T] -F <fmt>`, `list-windows [-a] -F`,
`list-sessions -F` (generalize the existing fixed-format lists to take `-F` through the format
engine with per-pane/window/session `FormatCtx`), `capture-pane [-e] [-p] [-J] [-S n] -t T`
(read straight out of the pane's `Grid` + scrollback; `-e` re-emits SGR), and format vars
`pane_pid`, `pane_title`, `history_size`, `cursor_y`, `session_id`, `client_session`,
`window_layout` (requires a tmux-compatible layout serialization of the split tree — nontrivial;
`select-layout <string>` is its restore-side twin).

For the restore half: `-c <dir>` on `new-session`/`new-window`/`split-window` (ConPTY spawn cwd —
easy), `-d` on `new-window`, full `-t sess:win.pane` targeting on `send-keys`/`split-window`/
`select-pane`, `switch-client -t <session>`, `move-window`, `select-pane -T <title>`, per-window
option scope for `set-option -t`/`show-window-options` (the SP4 "option scopes" item). 
**Unlocks: tmux-resurrect** (with `@resurrect-save-command-strategy` limitations, §4).

### Rung 8 — `#()` status shell-out + `#{start_time}` (`options.rs`, `status.rs`, `server.rs`)

- `#(command)` in status-left/right: run via rung-2 shell **asynchronously with a cached result**
  (tmux caches per `status-interval`; never block rendering; render last value or empty while
  pending).
- `#{start_time}` (server start, `%s` epoch).
- **Unlocks: tmux-continuum** (autosave piggybacks on the status tick exactly as in tmux).

Cross-rung note: everything lands in existing modules — `cmd.rs` (parse), `server/dispatch`
(exec), `options.rs` (user options + format engine), `server.rs` (shell spawn, env table, `#()`
cache), `status.rs`/render (inline styles), `model.rs` (window-scoped options, layout serialize),
`grid.rs` (capture-pane), `pty.rs`/Win32 (pane pid/command/cwd). All are public-surface changes →
each rung must amend `docs/specs/2026-07-07-command-config-interfaces.md` per the contract rule.

---

## §4 The Windows boundary (honest limits)

**Hard walls (not fixable by winmux):**

1. **`ps`/tty-based process detection.** vim-tmux-navigator's default `is_vim` (`ps -o state= -o
   comm= -t '#{pane_tty}'`) and resurrect's default `@resurrect-save-command-strategy` (`ps -ao
   ppid,args`) assume a POSIX process table and real tty device paths. ConPTY has no tty path for
   `#{pane_tty}` to name, and Git Bash's MSYS `ps` sees only MSYS-spawned processes with synthetic
   PIDs — it can never see a native `vim.exe`/`nvim.exe`. Mitigation is substitution, not
   emulation: winmux implements `#{pane_current_command}`/`#{pane_pid}` natively via Win32 and
   documents override snippets (`@vim_navigator_check`-style / resurrect's
   `#{pane_current_command}` fallback). Plugins with NO override hook for their `ps` usage are out.
2. **`@continuum-boot`** (systemd/launchd autostart) — no equivalent implemented; document as
   unsupported (Task Scheduler is a user-side workaround, not plugin surface).
3. **Unix-domain-socket assumptions.** resurrect's `TMUX="" tmux -S "$(echo $TMUX|cut -d, -f1)"`
   treats field 1 of `$TMUX` as a filesystem socket path. winmux must publish a `$TMUX` whose first
   field the shim round-trips as `-S` (a pipe name is fine as an opaque token) — any plugin that
   instead *stats* the socket path as a file will misbehave.
4. **Anything execing xclip/xsel/pbcopy without a `clip.exe`/`powershell` branch** — per-plugin;
   the popular ones (yank) already branch to `clip.exe`, which is native. Paste-FROM-clipboard
   helpers that need `powershell Get-Clipboard` vary by plugin.

**Soft requirements (document, don't code around):**

- **Git Bash (or full MSYS2) + git are prerequisites**, exactly as bash+git are for tmux users.
  `run-shell`'s `sh` resolution is the load-bearing decision (rung 2).
- **CRLF**: plugins cloned with `core.autocrlf=true` get broken `#!/usr/bin/env bash` shebangs.
  TPM's own git-clone runs with the user's global git config; recommend documenting
  `git config --global core.autocrlf input` or having the shim docs call it out (TPM's
  `docs/tpm_not_working.md` failure class).
- **Path dialects**: `~/.tmux/plugins/...` resolves inside Git Bash (`/c/Users/...`) but winmux's
  Rust side sees Windows paths; any winmux code that consumes paths produced by plugin shell
  (e.g. `source-file` on a plugin-written file, `-c` dirs from `#{pane_current_path}`) should
  accept both spellings (or normalize MSYS `/c/...` → `C:\...`).
- **`default-shell` mismatch**: tmux runs `run-shell` commands via `/bin/sh`; winmux panes default
  to PowerShell. Keep those decoupled — run-shell/if-shell/`#()`/copy-pipe always use `sh`,
  regardless of `default-command` (matches tmux, where `run-shell` uses `_PATH_BSHELL`).

**Reachability summary:** TPM itself, tmux-sensible, tmux-prefix-highlight, tmux-yank, and
tmux-continuum are fully reachable on native Windows + Git Bash once the ladder is built.
vim-tmux-navigator is reachable only via its `@vim_navigator_check` override backed by a native
`#{pane_current_command}`. tmux-resurrect is reachable for layout/cwd/content save-restore, with
running-program restore degraded to `#{pane_current_command}`-level fidelity (its `ps` strategy —
full argv recovery — stays Unix-only).
