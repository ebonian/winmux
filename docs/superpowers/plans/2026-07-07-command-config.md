# Command Layer + .tmux.conf Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** tmux's command layer for winmux — one dispatcher powering keybindings, the CLI, the `prefix-:` prompt, and real `.tmux.conf` files (prefix, bind-key, set-option, styles, status strings).

**Architecture:** Four new pure modules (`keys`, `style`, `cmd`, `options`) + a server-side dispatcher all four entry points share; `InputMachine` becomes a Key-event decoder with a configurable prefix; bindings become a mutable table of stored commands seeded with tmux defaults. Full design: `docs/specs/2026-07-07-command-config-design.md` (READ IT — the module sections are the requirements; this plan adds task boundaries, files, and tests).

**Tech Stack:** Rust 2021, no new dependencies.

## Global Constraints

- Contract discipline: new contract file `docs/specs/2026-07-07-command-config-interfaces.md` created in Task 1, extended per task; amendments to the two prior contracts where their locked surfaces change (input, render, server, cli). Same-commit rule.
- `cargo clippy --all-targets -- -D warnings` green at every commit; full `cargo test` green at every commit (each task keeps the tree compiling — new input pipeline lands ALONGSIDE the old in Task 5, swap happens atomically in Task 6).
- `cargo` at `~/.cargo/bin` (`export PATH="$HOME/.cargo/bin:$PATH"`).
- tmux fidelity: key notation, command names/aliases, option names/defaults, style grammar, error-message shapes (`unknown command: x`, `usage: ...`, `bad style: x`, `unknown option: x`) per the design spec. Deviations only where the spec declares them.
- Tests spawning servers: unique `-L` sockets + kill-server teardown; never the default socket.
- Commit per green task, conventional message + `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: `keys` module

**Files:** Create `src/keys.rs`; modify `src/lib.rs`; create contract file with `## keys` section.

**Produces:** `Key { code: KeyCode, ctrl: bool, meta: bool, shift: bool }` (+ `KeyCode` per design spec), `parse_key(&str) -> Option<Key>`, `key_name(&Key) -> String` (canonical tmux notation, for list-keys), `encode_key(&Key) -> Option<Vec<u8>>`, `struct KeyDecoder { new(), feed(&mut self, bytes: &[u8]) -> Vec<DecodedKey>, flush(&mut self) -> Vec<DecodedKey> }`, `DecodedKey { key: Key, raw: Vec<u8> }`.

- [ ] Failing tests (exact values): `parse_ctrl_letter` (`C-a` → ctrl+Char('a'); `C-A` ≡ `C-a`), `parse_meta`, `parse_shift_fkey` (`S-F5`), `parse_combined` (`C-M-Left`), `parse_named` (Enter/Escape/Space/Tab/BSpace/Up/PPage/IC/BTab, case-insensitive), `parse_plain_chars` (`%`, `"`, `1`, unicode char), `parse_invalid_none`; `encode_ctrl` (C-c → [0x03]), `encode_meta` (M-x → ESC x), `encode_named` (Enter → \r, Up → CSI A, F5 → CSI 15~, Home → CSI H, DC → CSI 3~), `encode_roundtrip_via_decoder` (encode then decode → same Key for a representative set); decoder: `decode_plain_bytes` (letters → Char keys w/ raw preserved), `decode_ctrl_bytes` (0x03 → C-c, 0x0d → Enter, 0x09 → Tab, 0x7f → BSpace), `decode_csi_arrows`, `decode_csi_modified` (`ESC [1;5A` → C-Up), `decode_ss3_fkeys`, `decode_meta_char` (ESC x → M-x), `decode_split_sequence_across_feeds` (CSI split over two feed() calls buffers), `decode_lone_escape_flush` (ESC + flush → Escape key), `decode_prefix_byte_is_ctrl_b` (0x02 → C-b).
- [ ] RED → implement → GREEN (`cargo test keys::`) → full suite + clippy → contract → commit `feat(keys): tmux key notation, VT encoding, and input decoder`.

### Task 2: `style` module

**Files:** Create `src/style.rs`; modify `src/lib.rs`; contract `## style`.

**Produces:** `PartialStyle { fg: Option<grid::Color>, bg: Option<grid::Color>, set/clear attr flags... }` (opaque ok), `parse_style(&str) -> Result<PartialStyle, String>` (Err = `bad style: <input>`), `PartialStyle::apply_to(&self, base: grid::Style) -> grid::Style`, `PartialStyle::merge(&self, over: &PartialStyle) -> PartialStyle`.

- [ ] Failing tests: `named_colors` (fg=red → Idx(1); brightred → Idx(9)), `colour_indexed` (`colour208`/`color208` → Idx(208), `colour256` → Err), `hex_rgb` (#1e1e2e → Rgb), `default_color_resets` (fg=default → Color::Default), `attrs_set` (bold,underscore,reverse,dim,italics), `attrs_clear` (nobold; `none` clears all), `accepted_ignored` (blink, strikethrough parse OK, no-op on apply), `apply_layers_over_base` (base green/black + `fg=red,bold` → red/black bold), `merge_precedence`, `bad_style_err_string` (exact `bad style: fg=zzz`), `empty_string_ok_noop`.
- [ ] RED → implement → GREEN (`cargo test style::`) → full + clippy → contract → commit `feat(style): tmux style-string grammar onto grid styles`.

### Task 3: `cmd` module

**Files:** Create `src/cmd.rs`; modify `src/lib.rs`; contract `## cmd`.

**Produces:** `RawCmd { name: String, args: Vec<String> }`, `parse_line(&str) -> Result<Vec<RawCmd>, String>`, `join_continuations(lines_iter) -> Vec<(usize, String)>` (logical lines w/ original line numbers for config errors), `ParsedCmd` enum per the design-spec table, `resolve(&RawCmd) -> Result<ParsedCmd, String>` (errors `unknown command: <n>` / `usage: <usage>`), `usage(name) -> Option<&'static str>`. Store-don't-resolve types: `bind-key`/`confirm-before` carry their command tail as `Vec<RawCmd>` (re-parse the remaining tokens: for `bind ... command args...` the tail tokens form ONE RawCmd unless they contain `\;` separators — match tmux: the tail is parsed with the same separator rules).

- [ ] Failing tests: tokenizer — `plain_split`, `single_quotes_literal` (`'a b' # not comment` inside quotes), `double_quotes_escapes` (`"a\"b"`), `comment_strips` (`set -g status off # trailing`), `semicolon_splits_commands`, `escaped_semicolon_is_arg` (`bind x kill-pane \; display ok` → bind's tail = 2 RawCmds), `unterminated_quote_err`; resolve — `aliases_resolve` (`splitw` ≡ `split-window`; `set` ≡ `set-option`), `split_window_flags` (-h -v -t), `send_keys_literal_flag`, `set_option_flags` (-g -w -a -u + name + optional value incl. values with spaces), `bind_key_full` (`bind -r -T prefix C-Up resize-pane -U` → key parsed via keys::parse_key at resolve time? NO — store key as string in ParsedCmd::BindKey{key: String,...}, server parses via keys (decouples modules); document), `confirm_before_tail`, `unknown_command_err_exact`, `usage_err_on_bad_flag` (exact `usage:` line), `sp2_commands_present` (new-session/ls/kill-server resolve).
- [ ] RED → implement → GREEN (`cargo test cmd::`) → full + clippy → contract → commit `feat(cmd): tmux command tokenizer, table, and typed commands`.

### Task 4: `options` module

**Files:** Create `src/options.rs`; modify `src/lib.rs`; contract `## options`.

**Produces:** `Options::new()` (tmux defaults per design-spec table), `set(&mut self, name, value: Option<&str>, append: bool, unset: bool) -> Result<(), String>`, `show(&self, name) -> Option<String>`, `show_all(&self) -> String` (sorted `name value` lines), typed getters used by later tasks: `prefix() -> keys::Key`, `base_index() -> u32`, `status_on() -> bool`, `status_position_top() -> bool`, `status_interval() -> Duration`, `status_left()/status_right() -> &str`, `status_left_length()/status_right_length() -> u16`, `status_style()/message_style()/window_status_style()/window_status_current_style()/pane_border_style()/pane_active_border_style() -> &style::PartialStyle`, `display_time()/repeat_time() -> Duration`, `default_command() -> &str`, `renumber_windows() -> bool`. Plus `expand_format(fmt: &str, ctx: &FormatCtx) -> String` with `FormatCtx { session: &str, window_index: u32, window_name: &str, window_flags: &str, pane_index: u32, hostname: &str, now: SystemTimeParts }` — handles `#S #I #W #F #P #H ##`, `#{session_name}/#{window_index}/#{window_name}`, unknown `#{...}` → empty, `%`-strftime subset (%H %M %S %d %m %Y %y %b %a %p %I) — `SystemTimeParts` is a plain struct (year, month, day, weekday, hour, min, sec) so the module stays pure/testable; server fills it from GetLocalTime.

- [ ] Failing tests: `defaults_match_tmux` (spot: prefix C-b, status-left `"[#S] "`, repeat-time 500, message-style yellow/black, display-time 750), `set_prefix_key` (`set -g prefix C-a` → prefix()==C-a; bad key → `bad value: <v>`), `set_style_validates` (status-style fg=zzz → Err from style), `append_string` (`set -ga status-right " x"`), `unset_restores_default`, `on_off_parsing` (on/off/1/0), `number_parsing`, `unknown_option_err_exact`, `accepted_inert_options_store` (mouse on; history-limit 5000 → show returns them), `expand_basic` (`"[#S] #I:#W#F"` with ctx), `expand_hash_escape` (`##` → `#`), `expand_unknown_long_empty`, `expand_strftime` (`%H:%M %d-%b-%y` reproduces the SP2 clock string for a known ctx), `show_all_sorted`.
- [ ] RED → implement → GREEN (`cargo test options::`) → full + clippy → contract → commit `feat(options): typed tmux option registry with format subset`.

### Task 5: table-driven input machinery (alongside old)

**Files:** Modify `src/input.rs` (ADD new types, keep old API compiling — old is deleted in Task 6); create `src/bindings.rs`; modify `src/lib.rs`; contract `## input-v2` + `## bindings`.

**Produces (new, alongside old):**
```rust
// input.rs additions
pub enum WhichTable { Root, Prefix }
pub enum KeyInputEvent { Forward(Vec<u8>), Key { table: WhichTable, key: keys::Key, raw: Vec<u8> }, Captured(Vec<u8>) }
pub struct KeyMachine;  // new(prefix: keys::Key), feed(&mut self, bytes, now) -> Vec<KeyInputEvent>,
                        // set_prefix(Key), set_repeat_time(Duration), arm_repeat(&mut self, now),
                        // set_capture(bool)  — semantics per design spec (prefix consumed, next key = Prefix table;
                        // repeat window keeps Prefix table w/o prefix; capture bypasses everything; decoder = keys::KeyDecoder)
// bindings.rs
pub struct Binding { pub cmds: Vec<cmd::RawCmd>, pub repeat: bool }
pub struct Bindings; // default() = tmux default tables per design spec; bind(table, Key, Binding),
                     // unbind(table, Key) -> bool, unbind_all(table), lookup(table, &Key) -> Option<&Binding>,
                     // list() -> String  // list-keys format: "bind-key [-r] -T <table> <keyname> <command...>" lines, sorted
```
Defaults in `Bindings::default()`: every current hardcoded binding re-expressed as commands (design-spec Input pipeline section — x/& via confirm-before with tmux prompt texts, `:` → command-prompt, prefix-again → send-prefix, arrows select-pane -r? tmux binds arrows WITHOUT -r for select-pane and WITH -r for C-arrows resize — match tmux: plain arrows NOT repeatable, C-arrows repeatable).

- [ ] Failing tests: KeyMachine — `plain_bytes_forward_coalesced`, `prefix_then_key_reports_prefix_table` (0x02 then `%` → Key{Prefix, '%'}), `prefix_is_consumed_not_forwarded`, `root_table_keys_report_root` (every key not preceded by prefix, incl. escape sequences), `arm_repeat_window` (arm; C-Up within 500ms → Prefix table; after expiry → Root), `capture_bypasses_prefix`, `configurable_prefix` (set_prefix C-a; 0x01 arms, 0x02 forwards); Bindings — `defaults_cover_current_behavior` (lookup(Prefix, '%') → split-window -h; x → confirm-before tail kill-pane; C-Up → repeat:true resize-pane -U; ':' → command-prompt; arrows → select-pane non-repeat), `bind_unbind_roundtrip`, `unbind_all_table`, `list_keys_format_exact` (one known line byte-exact).
- [ ] RED → implement → GREEN → full + clippy (old input tests still green) → contract → commit `feat(input): table-driven key machine and tmux default bindings (alongside legacy)`.

### Task 6: server dispatcher + atomic rewire

**Files:** Modify `src/server.rs`, `src/server/cli_exec.rs` (likely becomes thin or absorbed into a new `src/server/dispatch.rs` private submodule), `src/input.rs` (DELETE legacy InputMachine/Action/InputEvent), `src/model.rs` (renumber-windows support: `Session::renumber()`), `src/main.rs`/`src/cli.rs` (CLI passthrough now resolves through cmd table for usage errors client-side? NO — keep server-side resolution, CLI stays verbatim passthrough), `tests/server_proto.rs`; contracts: `## server` amendment (dispatcher), input sections replaced, MVP contract input section superseded-note.

**Interfaces:** everything private except existing `pub fn run`. Internal: `dispatch(&mut self, cmds: &[RawCmd], ctx: DispatchCtx) -> DispatchResult` per design spec; per-client `KeyMachine` replaces `InputMachine`; `Bindings` + `Options` live on `Server`; ALL key handling goes key → lookup → dispatch; `Cli(argv)` frames: argv[0] resolved through `cmd::resolve` — ANY table command works from the CLI now (split-window, send-keys, set, bind...), CliDone carries dispatch output/error; `:` prompt (command-prompt) reuses the prompt editor, commit → parse_line + dispatch, output/error → transient message; confirm-before generalizes ClientMode confirm (prompt text = expand_format of -p arg with #P/#W available, wrapped cmds dispatched on y/Y/Enter); send-keys (-l literal bytes; else keys::parse_key per arg → encode_key, unknown key name → `unknown key: <k>`; target pane via -t else focused); display-message (expand_format → transient message); set-option → Options::set + immediate re-render (prefix changes propagate via key_machine.set_prefix on ALL clients; repeat-time likewise); bind/unbind/list-keys → Bindings; show-options; renumber-windows honored on kill_window when option on; default-command used by every pane spawn; source-file → Task 7 stub `unknown command: source-file` REPLACED in Task 7 (no — implement file loading in Task 7; here return `source-file: not yet available` error... better: implement source-file HERE by extracting a `load_config_lines` helper Task 7 reuses — decide: implement here, Task 7 adds discovery/startup/-f only). Also: `-t` target resolution for pane/window commands (`session:window.pane` grammar, pane index = position in layout.panes(), window by index or name, defaults = current/focused).

- [ ] Step 1: failing server_proto additions: `cli_split_window_command` (Cli ["split-window","-h","-t","0"] → border appears for attached client), `cli_send_keys` (send-keys -t 0 "echo hi" Enter → screen shows hi), `cli_send_keys_literal`, `command_prompt_executes` (`\x02:` then `new-window\r` → status shows 2 windows), `command_prompt_error_message` (`:badcmd\r` → `unknown command: badcmd` transient), `set_prefix_runtime` (Cli set -g prefix C-a → 0x01+c makes new window; 0x02 forwards), `bind_custom_key` (bind -T prefix V split-window -h → \x02V splits), `unbind_default` (unbind % → \x02% forwards to shell), `confirm_before_custom` (bind k confirm-before -p"sure? (y/n)" kill-pane → prompt text + y kills), `list_keys_contains_defaults`, `show_options_output`, `set_default_command` (set -g default-command + new-window spawns it — use cmd.exe /c … marker? spawn `cmd.exe` and assert its banner/prompt appears), `renumber_windows_on` , `display_message_expands` (`display-message "#S:#W"` → transient `0:powershell`), `kill_pane_via_command_targets` (-t addressing). Existing tests must stay green UNCHANGED (they pin behavior through the rewire — that's the point).
- [ ] RED → implement (this is the big one; ~500-700 lines churn) → GREEN → full + clippy → contracts → commit `feat(server): unified command dispatcher wiring keys, CLI, and prompt`.

### Task 7: config loading + source-file + `-f`

**Files:** Modify `src/server.rs` (startup load + source-file through the Task 6 helper), `src/cli.rs`/`src/main.rs` (`-f <file>` global flag, forwarded through autostart argv to `__server --pipe P --config F`... design: `__server` gains optional `--config <path>` args (repeatable) — hidden interface, contract note), `tests/server_proto.rs`; contract `## config`.

Discovery (design spec): `-f` replaces chain; else `.tmux.conf` (XDG first then `%USERPROFILE%\.tmux.conf`) loaded first, then `%USERPROFILE%\.winmux.conf` (winmux overrides). Continuations joined; errors collected as `<file>:<line>: <err>` → server.log + first-attach transient `config: N error(s), see server.log`; loading continues past errors. source-file at runtime dispatches through the same loader (errors → transient/CliDone).

- [ ] Failing tests: `config_file_applies_at_startup` (write temp conf: `set -g prefix C-a` + `bind V split-window -h` + `set -g base-index 1`; start server with it; attach → window index 1 in status; C-a V splits), `config_errors_collected_and_continue` (bad line between two good lines → both good applied; first attach sees `config: 1 error(s)` message), `source_file_runtime` (Cli source-file → applies), `winmux_conf_overrides_tmux_conf` (unit-level on the loader given two temp files), `continuation_lines_join`, `dash_f_cli_forwarded` (cli::parse test: `winmux -f x.conf new` → ServerRole receives... unit: Invocation carries config; autostart argv includes --config x.conf).
- [ ] RED → implement → GREEN → full + clippy → contract → commit `feat(config): .tmux.conf loading, source-file, and -f`.

### Task 8: styled rendering from options

**Files:** Modify `src/render.rs` (Scene carries styles + position), `src/status.rs` (spans carry PartialStyle-resolved Styles instead of bool underline — or a `style: grid::Style` per span), `src/server.rs` (scene building from Options incl. status-left/right expand_format with lengths, status off/top), `tests/server_proto.rs`; contract amendments (render/status/server).

Scene changes: `status_row: Option<StatusRow>` where `StatusRow { top: bool, base: grid::Style, spans: Vec<(String, grid::Style)>, right: String, right_style: grid::Style }`; message uses `message_style`. Border drawing takes `border: grid::Style` + `border_active: grid::Style` (replacing hardcoded green-on-active). status off → panes get the full height, no row. Defaults must reproduce SP2 output byte-for-byte (existing render tests updated mechanically but the e2e stays green untouched — proves visual stability).

- [ ] Failing tests: render — `status_top_row_zero` (panes shift down), `status_off_full_height`, `span_styles_emitted` (custom fg/bg SGR bytes exact), `border_style_applied` (non-default pane-border-style changes border SGR; active border style separate); server_proto — `set_status_style_changes_bar` (set -g status-style bg=blue,fg=white → cell style assertions via test Grid), `set_status_left_format` (set -g status-left "[#S!] " → literal in bar), `status_position_top_moves_bar`, `status_off_hides_bar`, `pane_active_border_style_runtime`, `window_status_current_style_override` (set … fg=red → tab cell fg). e2e untouched and green.
- [ ] RED → implement → GREEN → full + clippy → contracts → commit `feat(render): option-driven status and border styling`.

### Task 9: e2e + docs closeout

**Files:** Create `tests/e2e_config.rs`; modify `docs/overview.md`, `CLAUDE.md`, `docs/follow-ups.md`, design-spec status header; contract cross-check pass.

- [ ] Failing e2e: `e2e_tmux_conf_roundtrip` — temp conf (`set -g prefix C-a`, `set -g status-style bg=magenta`, `bind V split-window -h`, `set -g base-index 1`, `set -g status-left "[cfg-#S] "`); launch release binary `winmux -L <sock> -f <conf>`; assert: status shows `[cfg-0] 1:powershell*`; `C-a V` splits (border appears); `C-a d` detaches with message; reattach persists; kill-server guard. Plus `e2e_command_prompt` (`\x02:` `rename-window meta\r` → `1:meta*`), `e2e_send_keys_cli` (`winmux send-keys -t 0 "echo e2e-ok" Enter` from plain Command → attached screen shows `e2e-ok`).
- [ ] Docs: overview SP3 delivered; CLAUDE.md (command layer, config paths, new modules, `-f`, current keybindings note "defaults, rebindable"); follow-ups (escape-time, per-scope options, format engine, automatic-rename listed as SP3 deviations→SP4 items).
- [ ] Full suite + clippy + release build + release smoke (`winmux -f <tmpconf> -L smoke ls` path) → commit `test(e2e): .tmux.conf round-trip; docs for sub-project 3`.

---

## Self-review notes

- Coverage vs design spec: keys→1, style→2, cmd→3, options/format→4, input/bindings→5, dispatcher/all-commands→6, config→7, rendering→8, e2e/docs→9. Deviations documented in spec (global-only options, format subset, escape-time ticket, automatic-rename inert).
- Type-consistency: `RawCmd` stored in `Binding.cmds` and `confirm-before` tails; `keys::Key` used by options.prefix(), KeyMachine, Bindings maps (Key needs Hash+Eq — add derives in Task 1); `style::PartialStyle` returned by options getters, applied in render via `apply_to`.
- Task 6 deletes legacy input API atomically with the server rewire so no commit ships two active input paths.
