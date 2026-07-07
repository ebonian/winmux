# Sub-project 3 — Command layer + `.tmux.conf` configuration

Status: **Active design spec.** Companion contract:
[`2026-07-07-command-config-interfaces.md`](2026-07-07-command-config-interfaces.md)
(created/extended task-by-task, same lock rules as before).

## Goal

tmux's real control plane: **everything is a command**. One dispatcher powers
(1) keybindings, (2) the `winmux <cmd>` CLI, (3) the `prefix-:` command
prompt, and (4) `.tmux.conf` lines. Users port their real `.tmux.conf`:
`set -g prefix C-a`, `bind`/`unbind`, status/border/message styling, status
left/right strings, base-index, default-command, etc.

## Architecture

```
.tmux.conf lines ─┐
CLI argv ─────────┼→ cmd::parse_line / argv → Vec<ParsedCmd> → server dispatcher
prefix-: prompt ──┤       (pure)                        (executes against registry,
key bindings ─────┘  bindings table stores ParsedCmd     clients, options, bindings)
```

New pure modules: `keys` (tmux key notation ↔ `Key` values ↔ VT input byte
sequences), `style` (tmux style-string grammar → render styles), `cmd`
(tokenizer + command table + typed `ParsedCmd`), `options` (typed option
registry with tmux defaults). Server-side: a `dispatch(cmd, ctx)` executor
that all four entry points share; `input::InputMachine` becomes table-driven
(emits `Key` events; the server resolves them through a mutable bindings
table).

## `keys` module

- `Key { code: KeyCode, ctrl: bool, meta: bool, shift: bool }`;
  `KeyCode::{Char(char), F(u8), Up, Down, Left, Right, Home, End, PPage,
  NPage, IC, DC, Enter, Escape, Space, Tab, BSpace, BTab}`.
- `parse_key("C-a" | "M-x" | "S-F5" | "C-M-Left" | "%" | "Space" | ...) ->
  Option<Key>` — tmux notation, case rules per tmux (named keys
  case-insensitive, chars literal).
- `encode_key(&Key) -> Option<Vec<u8>>` — bytes to SEND to a pane
  (send-keys): Ctrl-letter → 0x01-0x1a, Meta → ESC prefix, named keys → VT
  sequences (arrows CSI A-D, F1-F4 SS3 P-S, F5+ CSI 15~ style, Home/End CSI
  H/F, PPage/NPage CSI 5~/6~, IC/DC CSI 2~/3~, Enter \r, Tab \t, BSpace 0x7f,
  Escape 0x1b, Space 0x20).
- `struct KeyDecoder` — incremental VT-input parser for the INPUT direction
  (client keystrokes): bytes → `Vec<DecodedKey { key: Key, raw: Vec<u8> }>`;
  `raw` preserved so unbound keys forward verbatim. Handles: plain bytes,
  0x01-0x1a as C-letter (0x0d Enter, 0x09 Tab, 0x1b Escape start), 0x7f
  BSpace, CSI sequences incl. modifier params (`1;5A` = C-Up etc.), SS3,
  ESC-prefixed byte = Meta (bounded buffering: incomplete sequences flush as
  raw Escape + rest after the feed ends — no timer in SP3; escape-time is
  ticketed).

## `style` module

`parse_style("fg=colour208,bg=#1e1e2e,bold,underscore") -> Result<PartialStyle, String>`
- fields: `fg: Option<Color>`, `bg: Option<Color>`, attr set/clear flags
  (bold, dim, italics, underscore, reverse, blink→ignored-but-accepted,
  strikethrough→accepted-ignored), `none`/`noattr` clears, `nobold` etc.
- Colors: named ANSI (black red green yellow blue magenta cyan white +
  bright* variants), `colour0`-`colour255`/`color*`, `#rrggbb`, `default`.
  Maps onto the existing `grid::Color::{Default, Idx(u8), Rgb(..)}`.
- `PartialStyle::apply_to(base: grid::Style) -> grid::Style`.
- Error: `bad style: <input>` (tmux-style message).

## `cmd` module

- Tokenizer (tmux config-line rules): whitespace split; `'...'` literal;
  `"..."` with `\"` `\\` escapes; `#` starts a comment outside quotes;
  trailing `\` continues to next line (handled by the file loader joining
  lines); unquoted `;` separates commands; `\;` is a literal `;` argument.
- `parse_line(line: &str) -> Result<Vec<RawCmd>, String>`;
  `RawCmd { name: String, args: Vec<String> }`.
- `resolve(raw: RawCmd) -> Result<ParsedCmd, String>` via the command table
  (names + tmux aliases + flag specs). Errors: `unknown command: <name>`,
  `usage: <usage line>` on bad flags (consistent with SP2's CLI errors).
- `ParsedCmd` (typed enum) — SP3 command set:

| command (alias) | args |
|---|---|
| `split-window` (`splitw`) | `[-h] [-v] [-t target]` |
| `select-pane` (`selectp`) | `[-L -R -U -D] [-t target]` |
| `select-window` (`selectw`) | `-t target` (window index/name) |
| `next-window` (`next`) / `previous-window` (`prev`) / `last-window` (`last`) | |
| `last-pane` (`lastp`) | |
| `new-window` (`neww`) | `[-n name]` |
| `kill-pane` (`killp`) / `kill-window` (`killw`) | `[-t target]` |
| `resize-pane` (`resizep`) | `[-L -R -U -D] [-Z] [count]` |
| `rename-window` (`renamew`) / `rename-session` (`rename`) | `[-t target] name` |
| `detach-client` | `[-s target]` |
| `send-keys` (`send`) | `[-l] [-t target] key...` |
| `send-prefix` | |
| `display-message` (`display`) | `[text]` |
| `confirm-before` (`confirm`) | `-p prompt command...` (command = rest, re-parsed) |
| `command-prompt` | (opens the `:` prompt; `-I initial` accepted) |
| `set-option` (`set`) | `[-g] [-w] [-a] [-u] option [value]` |
| `show-options` (`show`) | `[-g] [option]` |
| `bind-key` (`bind`) | `[-n] [-r] [-T table] key command...` |
| `unbind-key` (`unbind`) | `[-a] [-n] [-T table] [key]` |
| `list-keys` (`lsk`) | |
| `source-file` (`source`) | `path` |
| existing SP2 CLI commands | unchanged, folded into the same table (new-session, attach-session, list-sessions, list-windows, has-session, kill-session, kill-server) |

- `confirm-before`'s wrapped command and `bind-key`'s bound command are
  stored as `Vec<RawCmd>` re-resolved at execution time (tmux does late
  binding too).

## `options` module

`Options` struct: typed fields with tmux defaults + `set(name, value, append,
unset) -> Result<(), String>` + `get_printable(name)`. SP3 scope: one global
instance (per-session/window overlays are SP4; `-g`/`-w` accepted, both hit
the global table — documented deviation).

| option | type | default |
|---|---|---|
| `prefix` | key | `C-b` |
| `base-index` | number | 0 |
| `pane-base-index` | number | 0 |
| `status` | on/off | on |
| `status-position` | top/bottom | bottom |
| `status-interval` | seconds | 15 (clock redraw stays 1s-tick; interval gates strftime re-eval) |
| `status-left` / `status-right` | string | `"[#S] "` / `%H:%M %d-%b-%y` — **deviation from tmux** (revisit SP4): tmux's real default embeds `#{=21:pane_title}`, which is outside the SP3 format subset (would render as an empty-quoted prefix); winmux's default is just the clock half, reproducing SP2's `local_clock()` string exactly. Custom values support the SP3 format subset. |
| `status-left-length` / `status-right-length` | number | 10 / 40 |
| `status-style` | style | `bg=green,fg=black` |
| `window-status-style` / `window-status-current-style` | style | default / `underscore` |
| `message-style` | style | `bg=yellow,fg=black` |
| `pane-border-style` / `pane-active-border-style` | style | default(grey) / `fg=green` |
| `display-time` | ms | 750 |
| `repeat-time` | ms | 500 |
| `default-command` | string | `powershell.exe -NoLogo` (winmux default; lets users set pwsh) |
| `renumber-windows` | on/off | off (implemented) |
| `mouse`, `history-limit`, `escape-time`, `automatic-rename`, `allow-rename`, `mode-keys`, `default-terminal`, `exit-empty`, `aggressive-resize` | accepted + stored, functionality deferred (SP4) — setting them is NOT an error (config portability) |

Unknown option → `unknown option: <name>` (error, tmux behavior). Values
validated per type (`bad value: <v>` style errors).

**Format subset** (status-left/right, display-message): `#S` session name,
`#I` window index, `#W` window name, `#F` window flags, `#H` hostname, `#P`
pane index, `##` literal `#`, plus `strftime` `%`-codes (via the existing
GetLocalTime machinery). `#{...}` long forms: only `#{session_name}`,
`#{window_index}`, `#{window_name}` (the common ones); anything else renders
empty (documented). Re-evaluated per render; strftime granularity =
status-interval.

## Input pipeline (replaces hardcoded table)

- `InputMachine` (contract overhaul): configurable prefix key; emits
  `InputEvent::{Forward(Vec<u8>), Key { table: WhichTable, key: Key, raw: Vec<u8> }, Captured(Vec<u8>)}`
  where `WhichTable::{Root, Prefix}`. Prefix key press → next decoded key
  reports `Prefix` table. Double-prefix → the server's `send-prefix` default
  binding handles it (bound in the prefix table like tmux). Repeat: server
  calls `arm_repeat(now)` after dispatching a `-r` binding; keys arriving
  within `repeat-time` re-report table `Prefix` without a new prefix press.
  `set_capture` unchanged. `set_prefix(Key)`, `set_repeat_time(Duration)`.
- **Bindings table** (server-side, mutable): `Bindings { root: Map<Key, Binding>, prefix: Map<Key, Binding> }`,
  `Binding { cmds: Vec<RawCmd>, repeat: bool }`. `Bindings::default()`
  reproduces tmux defaults exactly as today's behavior (%, ", arrows o ; x z
  c n p l 0-9 & , $ d ( ) C-arrows -r, prefix-again → send-prefix, `:` →
  command-prompt) — `x`/`&` expressed as `confirm-before -p"kill-pane #P?
  (y/n)" kill-pane` etc., so rebinding works uniformly.
- Unbound key in Prefix table → swallowed (tmux). Unbound in Root → Forward
  raw bytes.

## Server dispatcher

`dispatch(&mut self, cmds: &[RawCmd], ctx: Ctx) -> Result<Option<String>, String>`
— ctx carries the acting client (for client-relative commands and message
routing) or None (config load / CLI without attach). All existing
Action-handling paths are REWIRED through commands (Action enum shrinks to
nothing / disappears — bindings produce commands). CLI `Cli(argv)` frames now
accept ANY command from the table (`winmux split-window -t work` works);
output rules unchanged (CliDone). `:` prompt executes through the same path;
errors and `display-message` output land in the transient message slot
(message-style, display-time).

## Config loading

- Server role: after bind, before accept loop: load, in order, the first
  existing of `%USERPROFILE%\.winmux.conf`, then also `$XDG_CONFIG_HOME/
  tmux/tmux.conf` or `%USERPROFILE%\.tmux.conf` (winmux file wins by loading
  LAST... tmux semantics: later lines override; we load `.tmux.conf` first,
  then `.winmux.conf` so winmux-specific tweaks override ported tmux config).
  `-f <file>` (CLI, forwarded through autostart argv) replaces the default
  chain entirely.
- Line continuation joined by the loader; each logical line → parse_line →
  dispatch with ctx=config. Errors collected (`<file>:<lineno>: <err>`),
  loading CONTINUES (tmux behavior), errors go to server.log AND the first
  client to attach gets a transient message `config: N error(s), see
  %LOCALAPPDATA%\winmux\server.log`.
- `source-file` re-enters the same loader at runtime.

## Rendering changes

Scene/renderer honor: status on/off (pane area grows), status-position
top/bottom, status-style (row base), window-status(-current)-style layered
over the base, message-style, pane-border-style / pane-active-border-style
(border cells get a Style instead of hardcoded SGR 32). Contract amendments
to `render`/`Scene` documented per task.

## Testing strategy

Pure TDD for keys/style/cmd/options (exact-value tests). server_proto
extensions: bind/unbind/set via Cli frames + `:` prompt bytes, custom prefix
end-to-end (C-a after `set -g prefix C-a`), styled status assertions
(cell-style checks through the test Grid), send-keys, confirm-before custom
binding, source-file, config-error surfacing. e2e: real `.tmux.conf`
fixture (custom prefix, bind, set status-style, base-index) loaded via
`-f`-style isolation (`WINMUX_CONF` env override or `-f` flag — decided in
the plan: `-f` flag, forwarded through autostart) driving the release binary.

## Explicit deviations from tmux (documented, revisit SP4)

- Options are global-only (`-g` optional); per-session/window scoping SP4.
- `#{...}` format engine: 3 names only; conditionals/modifiers SP4.
- escape-time not honored (no ESC-disambiguation timer); ticketed.
- automatic-rename accepted but inert (no fg-process tracking on Windows yet).
- Config errors surface via log + transient message, not tmux's error view.
- `status-right`'s default value omits tmux's `#{=21:pane_title}` prefix
  (outside the SP3 format subset); winmux's default is the clock half only.
