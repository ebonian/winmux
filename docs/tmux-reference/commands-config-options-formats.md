# tmux behavioral reference: command parsing, configuration, options, styles, formats, key notation

**Source studied:** tmux master `db115c6` (2026-07-07), full source at the time of writing.
**Purpose:** authoritative spec for winmux parity work in this domain. All file:line references are into that tree. This is post-3.5 master — it includes theme colours (`themeblack`…), pane scrollbars, floating panes (`new-pane`), `switch-mode`, and an `escape-time` default of **10ms** (changed from the historical 500ms). Where master diverges from tmux 3.x conventions that users' configs assume, this is called out.

---

## 1. Config file processing

### 1.1 Default config file chain (`tmux.c`)

The compiled-in default is the `TMUX_CONF` macro. In the portable tmux build (`Makefile.am:14`):

```
TMUX_CONF = "$(sysconfdir)/tmux.conf:~/.tmux.conf:$XDG_CONFIG_HOME/tmux/tmux.conf:~/.config/tmux/tmux.conf"
```

(The OpenBSD-base fallback in `tmux.h:88` is just `/etc/tmux.conf:~/.tmux.conf`.)

At startup (`tmux.c:405`) `expand_paths(TMUX_CONF, &cfg_files, &cfg_nfiles, 1)` splits this on `:` and expands each element with `expand_path()` (`tmux.c:108-139`):

- `~/`-prefixed → `home + path+1`, where home comes from `find_home()` (the `HOME` environment variable, falling back to `getpwuid()`). A path that is exactly `~user/...` is **not** supported here (only in the command lexer, §2.6).
- `$VAR/...` → value of `VAR` from `global_environ` + rest. If the variable is unset the whole element is **silently dropped** (`expand_path` returns NULL → `expand_paths` skips it). So `$XDG_CONFIG_HOME/tmux/tmux.conf` simply vanishes from the chain when `XDG_CONFIG_HOME` is unset — but `~/.config/tmux/tmux.conf` is *also* in the chain unconditionally.
- Duplicate resolved paths are dropped (`tmux.c:171-179`).

**Every file in the chain is loaded, in order** — this is not first-match-wins. `~/.tmux.conf` and `~/.config/tmux/tmux.conf` both load if both exist, in that order, later files overriding earlier settings by normal command execution.

`-f file` (`tmux.c:424-434`): the **first** `-f` clears the whole default chain; each `-f` then appends (multiple `-f` allowed, loaded in order). Crucially it also sets `cfg_quiet = 0`.

### 1.2 Quiet flag and missing files (`cfg.c`)

`cfg_quiet` defaults to 1 (`cfg.c:35`). `start_cfg()` (`cfg.c:64-93`) passes `CMD_PARSE_QUIET` when quiet. In `load_cfg()` (`cfg.c:109-114`):

```c
if ((f = fopen(path, "rb")) == NULL) {
        if (errno == ENOENT && (flags & CMD_PARSE_QUIET))
                return (0);
        cfg_add_cause("%s: %s", path, strerror(errno));
        return (-1);
}
```

So: default-chain files that don't exist are silently skipped; an explicitly `-f`-given file that doesn't exist **is an error** (a cause is recorded), because `-f` cleared `cfg_quiet`. Note it is a *recorded cause*, not a hard startup abort — the server still starts.

There is no tmux equivalent of "`-f -` disables config"; the idiom is `-f /dev/null`. (winmux's `-f -` is an extension.)

### 1.3 Where config commands run, and error handling

`start_cfg()` runs config **without a client**: commands go onto the global command queue with `item->client == NULL`. The very first client is blocked by a `cfg_client_done` callback appended to its queue (`cfg.c:81-85`) so its own command runs only after config finishes; `cfg_done` (`cfg.c:47-62`) then flips `cfg_finished` and calls `cfg_show_causes(NULL)`.

**Two distinct error layers — get this exactly right:**

1. **Parse-time errors** (grammar error, *unknown command*, unknown flag, wrong argument count — anything that makes `cmd_parse()` fail): `cmd_parse_build_commands()` aborts on the first failure (`cmd-parse.y:915-920`), `load_cfg` records **one** cause `file:line: message` and **the entire file's commands are discarded** (`cfg.c:125-129`). tmux does *not* skip the bad line and run the rest — a config file with one misspelled command loads *nothing* from that file. Other files in the chain still load.
2. **Execution-time errors** (bad option value, no such session, etc.): each config *line* was built as its own command group (`cmd-parse.y:895-910`, "so the queue knows which ones to remove if a command fails"). When a command returns `CMD_RETURN_ERROR`, `cmdq_remove_group()` removes only the **rest of that group/line** (`cmd-queue.c:776-781`); subsequent lines still execute. The error text is routed by `cmdq_error()` (`cmd-queue.c:863-881`): with `item->client == NULL` it becomes `cfg_add_cause("%s:%u: %s", file, line, msg)`. So execution errors *are* collected and config *continues* — multiple causes accumulate.

**How causes are shown** (`cfg_show_causes`, `cfg.c:240-281`): once a session is attached, the active pane of the current window is switched into `window_view_mode` (a read-only copy-mode view) and each cause is added as a line. Until an attached session exists the causes are simply held (`s->attached == 0` → return, wait). Control-mode clients instead get `%config-error <cause>` lines. `cfg_print_causes` (`cfg.c:221-238`) is the variant used after `source-file`, printing causes through the *item* (status line / stdout) rather than the view pane.

### 1.4 `%if` / `%elif` / `%else` / `%endif` preprocessing

Implemented **in the parser grammar itself** (`cmd-parse.y:268-395`), not a separate preprocessor pass:

- Lexer: a word starting `%` is checked (`cmd-parse.y:1342-1377`); `%if`, `%elif`, `%else`, `%endif`, `%hidden` are keywords; any *other* `%word` is `ERROR` — unless the word consists entirely of `%` and digits, in which case it is an ordinary token (so `%%` and `%1` survive as prompt-template arguments).
- The argument of `%if`/`%elif` is a *format*: the `expanded` rule (`cmd-parse.y:197-219`) builds a format tree (with client/target defaults if available, `FORMAT_NOJOBS` — **`#()` never runs during config parsing**) and expands it; truth is `format_true()`: non-empty and not exactly `"0"` (`format.c:4502-4507`).
- After a `#` in condition position, `#{` opens a format token collected by `yylex_format()` (`cmd-parse.y:1400-1438`), brace-counting `#{`/`}` pairs; so `%if #{==:#{host},myhost}` works.
- Conditions nest (scope stack, `cmd-parse.y:42-45,87-88`); false branches are *parsed* but their command lists are discarded, and environment assignments inside false branches are suppressed (the `flag` check at `cmd-parse.y:241`).
- `%if` may also appear inline within a command sequence (the `condition1` rules) — i.e. between `;`-separated commands on one line.

### 1.5 Line continuation, comments, environment assignments

- **Line continuation:** handled in `yylex_getc()` (`cmd-parse.y:1210-1239`) — backslash-newline is eaten and the line counter incremented, *with correct handling of runs of backslashes* (`ps->escapes % 2` — `\\` + newline does **not** continue).
- **Comments:** an unquoted `#` not followed by `{`-in-condition consumes to end of line (`cmd-parse.y:1321-1340`). Inside quotes `#` is literal. Special case inside a quoted token spanning lines: after an embedded newline, leading whitespace is stripped and a `#` there starts a comment *unless* followed by one of `,#{}:`, which are the format escapes (`cmd-parse.y:1684-1698`).
- **`VAR=value` assignments:** a token matching `name=…` where `name` is `[A-Za-z_][A-Za-z0-9_]*` (no leading digit; `yylex_is_var`, `cmd-parse.y:1156-1164`) lexes as `EQUALS`. As the *first* word of a statement it sets the variable in **`global_environ`** (`cmd-parse.y:224-244`) — max length 16384. `%hidden VAR=value` sets it with `ENVIRON_HIDDEN` (kept out of child processes' environment but visible to config expansion). In *argument* position an `EQUALS` token is just an ordinary string argument (`cmd-parse.y:574-579`).

### 1.6 `source-file` (`cmd-source-file.c`)

```
source-file [-Fnqv] [-t target-pane] path ...        (alias: source)
```

- `-q`: sets `CMD_PARSE_QUIET` — a path that matches nothing is not an error; also silences nothing else (parse errors still reported).
- `-n`: `CMD_PARSE_PARSEONLY` — parse (syntax-check) but do not execute (`cfg.c:130-133`).
- `-F`: expand the *path* as a format first (`cmd-source-file.c:205-209`).
- `-v`: `CMD_PARSE_VERBOSE` — echo each parsed line (`cmd_parse_print_commands`, `cmd-parse.y:612-625`). Also inherited if the *calling* context was verbose.
- Multiple paths allowed; each may be a **glob**. Nesting depth limit `CMD_SOURCE_FILE_DEPTH_LIMIT 50` (`cmd-source-file.c:33`), per client (or global when clientless): "too many nested files".

**Path handling — the exact rules (`cmd-source-file.c:200-242`):**

1. `~` is *not* expanded here. It was already expanded at **lex time** when the command line containing `source-file ~/.tmux.conf` was tokenized (§2.6). By the time `cmd_source_file_exec` sees the argument it is `/home/user/.tmux.conf`. **This is the mechanism winmux is missing** — expansion belongs in the tokenizer, so it works identically for `source-file`, `new-session -c ~/src`, etc., and it happens when `bind r source-file ~/.tmux.conf` is *parsed* (binding stores the expanded path), not when the binding later fires.
2. Path `-` means "read stdin" and bypasses globbing.
3. A path not starting with `/` is made absolute against the **client's cwd** (`server_client_get_cwd`), which is first backslash-escaped for glob metacharacters (`cmd_source_file_quote_for_glob`, `cmd-source-file.c:144-157`: every non-alphanumeric ASCII char except `/` gets `\`).
4. The resulting pattern goes through **`glob(3)` with flags 0** — so `*?[]` work, `GLOB_TILDE` is *not* used (tilde really is purely the lexer's job), and a pattern with no matches yields `GLOB_NOMATCH` → error `"No such file or directory: <path>"` unless `-q`. Other glob failures map to ENOMEM/EINVAL strings (`cmd-source-file.c:221-236`).
5. All matches from all patterns are read asynchronously via `file_read` and each buffer is parsed with `load_cfg_from_buffer`; queued commands are inserted *after* the source-file item, chaining in order (`cmd-source-file.c:101-133`). After completion `cfg_print_causes` prints any collected causes to the invoking context.
6. Return value: error if a pattern matched nothing (without `-q`) or a file failed to load; note a *parse error inside* a sourced file marks `retval` error and (if the client is unattached, e.g. `tmux source ...` from a shell during startup) sets client exit code 1 (`cmd-source-file.c:86-93`).

---

## 2. Command grammar (`cmd-parse.y`)

### 2.1 Overall shape

The yacc grammar parses a file/string into `statements` → `statement` per line → `commands` separated by `;` → `command` = optional `VAR=val` prefix + `TOKEN` name + `arguments`. Arguments are strings (`TOKEN`/`EQUALS`) or **brace blocks**.

Terminators/structure characters at top level: newline, `;`, `{`, `}`, `#` (comment), `%` (directive) (`cmd-parse.y:1314-1319`). Everything else starts a token.

### 2.2 `;` command separation and `\;`

- In config files and strings, a bare unquoted `;` separates commands (lexer returns `';'`, grammar rule `commands ';' command`, `cmd-parse.y:407-430`).
- Escaped/quoted (`\;`, `';'`, `";"`) it is an ordinary character in a token.
- **From an already-split argv** (a bound key's command list given as separate arguments, or the shell CLI `tmux bind x cmd \; cmd2`): `cmd_parse_from_arguments()` (`cmd-parse.y:1062-1135`) implements the classic tmux rule — an argument whose **last** character is `;` ends a command there (the `;` is stripped); unless the `;` is preceded by `\`, in which case `\;` collapses to a literal `;` and does *not* split:

```c
if (size != 0 && copy[size - 1] == ';') {
        copy[--size] = '\0';
        if (size > 0 && copy[size - 1] == '\\')
                copy[size - 1] = ';';
        else
                end = 1;
}
```

### 2.3 Brace blocks `{ … }`

`{` begins a command-block **argument** (`argument: '{' argument_statements`, `cmd-parse.y:580-585`); the block contains full statements (own lines, `;`, nested `%if`, nested braces) and produces a `CMD_PARSE_COMMANDS` argument — kept as an unexpanded parsed command list, **not** a string. `bind X { cmd1 ; cmd2 }` therefore stores a real command list. When such a value must become a string (e.g. `args_print`) it renders as `{ cmd1 ; cmd2 }`. A `}` outside a block is a lex error (`cmd-parse.y:1717-1718`). Commands consuming an argument that may be commands-or-string declare it via their `args_parse` callback (`ARGS_PARSE_COMMANDS_OR_STRING`, e.g. bind-key, command-prompt, confirm-before, if-shell, run-shell -C, set-option value).

Semantics note (`cmd-parse.y:974-978`): when parsing a *string*, all commands go in one group — `{ a \n b }` ≡ `"a ; b"`.

### 2.4 Quoting and escapes (`yylex_token`, `cmd-parse.y:1635-1760`)

Three states: bare, `"…"`, `'…'`. Quotes may open/close mid-token (`fo"o b"ar` is one token `foo bar`).

- **Single quotes:** *everything* literal — no `\`, `$`, `~` processing. (`\` is literal inside `'…'`.)
- **Double quotes:** `\` escapes work; `$VAR`/`${VAR}` expands; `~` does **not** expand (only at token start — see condition below — and a `~` right after the opening quote *does* expand because state changed; precisely: `~` expands when `last != state && state != SINGLE_QUOTES`, i.e. at a quoting-state boundary — start of token or immediately after entering/leaving quotes, `cmd-parse.y:1707`).
- **Bare:** `\`, `$`, `~` all active; whitespace/`;`/`}`/newline end the token.
- **Formats `#{…}` are NOT expanded by the tokenizer** anywhere in ordinary commands — `#` only matters as comment-start (outside quotes) or `%if` condition. Format expansion happens later, per-command, only where a command explicitly expands (usage of `format_single…`, `-F` flags, status option rendering, etc.). Inside quotes `#` is literal and passes through to the command.

**Escape sequences** (`yylex_token_escape`, `cmd-parse.y:1440-1537`): `\ooo` octal (000–377, `\4xx`–`\7xx` invalid), `\a \b \e \f \s`(space)` \v \r \n \t`, `\uXXXX`, `\UXXXXXXXX` (hex → wchar → multibyte), and `\<anything else>` = that character (so `\;` `\#` `\\` `\ ` `\"` `\$` `\~`).

### 2.5 `$VAR` expansion (`yylex_token_variable`, `cmd-parse.y:1539-1589`)

`$NAME` or `${NAME}` — names `[A-Za-z_][A-Za-z0-9_]*` (max 1023). Looked up in **`global_environ`** (server's inherited + config-assigned env). Unset/hidden-value-NULL → expands to empty. `$` followed by a non-name char is a literal `$`. Unterminated `${…` is an error "invalid environment variable".

### 2.6 `~` expansion (`yylex_token_tilde`, `cmd-parse.y:1591-1633`) — the mechanism winmux needs

At a token/quote-state boundary (not in single quotes), `~` collects a user name up to `/`, whitespace, quote, or EOL:

```c
if (*name == '\0') {
        envent = environ_find(global_environ, "HOME");
        if (envent != NULL && envent->value != NULL && *envent->value != '\0')
                home = envent->value;
        else if ((pw = getpwuid(getuid())) != NULL)
                home = pw->pw_dir;
} else {
        if ((pw = getpwnam(name)) != NULL)
                home = pw->pw_dir;
}
if (home == NULL)
        return (0);            /* token error */
```

So `~` → `$HOME` (global_environ first, then passwd), `~alice` → alice's home dir; failure to resolve is a hard token error. Because this runs at parse time, **every** command argument gets it: `bind r source-file ~/.tmux.conf` bakes the absolute path into the stored binding.

### 2.7 Command-name resolution (`cmd_find`, `cmd.c:461-508`)

For each candidate entry, in table order:

1. **Exact alias match wins immediately** (`entry->alias`, `cmd.c:471-475`).
2. Otherwise prefix match against `entry->name`; multiple prefix matches → ambiguous, **unless** one of them is an exact name match (the `strcmp==0 → break` at `cmd.c:483-484`).
3. Errors: `unknown command: %s` / `ambiguous command: %s, could be: a, b, c`.

So `kill` is ambiguous, `killw` is the alias of kill-window, `set` is the alias of set-option (exact alias, wins over any prefix), `sourc` resolves to source-file by prefix.

**`command-alias` user aliases** (`cmd_get_alias`, `cmd.c:430-458` + `cmd_parse_expand_alias`, `cmd-parse.y:767-818`): before table lookup, the first word is looked up in the `command-alias` **array server option**, entries of form `name=replacement`. Defaults (`options-table.c:305-310`): `split-pane=split-window`, `splitp=split-window`, `server-info=show-messages -JT`, `info=show-messages -JT`, `choose-window=choose-tree -w`, `choose-session=choose-tree -s`. The replacement is *re-parsed as a command string* (may be multiple commands); the original invocation's remaining arguments are appended to the replacement's **last** command. Alias expansion is non-recursive (`CMD_PARSE_NOALIAS` set during the inner build). Alias lookup requires exact match on the alias name.

### 2.8 Flag/argument parsing (`arguments.c`)

Each command's `args` template is like `"aFgopqst:uUw"` (see appendix): a bare letter is a boolean flag, `x:` takes an argument, `x::` takes an **optional** argument.

`args_parse` (`arguments.c:255-346`), given the raw values (index 0 = command name):

- A value starting `-` and not exactly `-` or `--` is a flag clump: **boolean flags bundle** (`-dP`), and the first argument-taking flag consumes either the rest of the clump (`-tfoo`) or the next value (`-t foo`) (`args_parse_flags`, `arguments.c:205-252`).
- `--` terminates flag parsing (consumed); a lone `-` terminates flag parsing and is kept as a positional argument.
- `-?` → usage error; non-alphanumeric → `invalid flag -%c`; not in template → `unknown flag -%c`.
- Optional-argument (`::`) flags: a following value that looks like another flag (`-x` or `--…`) or end-of-args means "flag present, no value" (`arguments.c:184-193`) — used by e.g. `resize-pane -D` / `-D 5`.
- Repeated boolean flags count (`args_has` returns the **count** — `-r -r` etc.; `unbind -a` uses presence). Repeated argument flags accumulate values; `args_get` returns the **last** (`arguments.c:686-696`).
- After flags, positional values are checked against the command's `lower`/`upper` counts (−1 = unlimited): `too few arguments (need at least N)` / `too many arguments (need at most N)`. Positional type (string vs commands) is decided by the command's `args_parse` callback per index.
- A flag's argument must be a string, not a brace block (`-%c argument must be a string`).
- Numeric helpers: `args_strtonum` (strict, `value is <errstr>: <s>`), `args_percentage` (trailing `%` = percentage of current value, range-checked "too small"/"too large").
- **Argument re-quoting** (`args_escape`, `arguments.c:605-649`): used by list-keys/show-options display — values containing `` #';${}%``/space are double-quoted (with vis-encoding), values containing space/`"` single-quoted, single chars needing quoting are `\c`, leading `~` is escaped `\~` so the output re-parses identically.

### 2.9 `%%`/`%1`…`%9` template substitution (`cmd_template_replace`, `cmd.c:841-888`)

Used by command-prompt, confirm-before targets, choose-tree templates, and `cmd_list_copy` for bound commands with arguments. In a template, `%1`–`%9` are replaced by the corresponding argument, and the *first* `%%` acts as `%1` (later `%%` are literal). A doubled form `%%%`/`%N%` additionally backslash-escapes `"\$;~` in the substituted text (safe re-parse).

---

## 3. Options system

### 3.1 Storage model (`options.c`)

Four option trees: `global_options` (server), `global_s_options` (global session), `global_w_options` (global window), plus per-session `s->options` (parent = global_s), per-window `w->options` (parent = global_w), per-pane `wp->options` (parent = the window's options). `options_get` (`options.c:280-293`) walks up parents until found — this is the pane→window→global-window / session→global-session inheritance. The **master table** `options_table[]` (`options-table.c:282-1954`) defines every named option's type, scope bits (`SERVER`/`SESSION`/`WINDOW`/`PANE` — window and pane usually combined as `WINDOW|PANE`), defaults, limits, choices, separator, and flags (`IS_ARRAY`, `IS_STYLE`, `IS_COLOUR`, `IS_HOOK`).

Option **types**: `STRING`, `NUMBER` (min/max), `KEY` (key code, e.g. `prefix`), `COLOUR`, `FLAG` (on/off/1/0/yes/no; **no value toggles**), `CHOICE` (list; for 2-choice options, no value toggles — `options.c:1241-1247`), `COMMAND` (parsed command list, e.g. hooks, `default-client-command`).

### 3.2 Name lookup: prefix matching + old-name map

`options_match` (`options.c:798-842`): `@`-options are returned as-is (no table). Otherwise the name is first passed through `options_map_name` using `options_other_names[]` (`options-table.c:270-279`) — the US-spelling map: `display-panes-color`→`display-panes-colour`, `display-panes-active-color`, `clock-mode-color`, `cursor-color`, `prompt-cursor-color`, `prompt-command-cursor-color`, `pane-colors`→`pane-colours`. Then **exact match, else unambiguous-prefix match** over the whole table (same shape as command lookup). Failures produce (in cmd-set-option/cmd-show-options): `ambiguous option: %s` or `invalid option: %s`. (The distinct string `unknown option: %s` comes from `options_scope_from_name`, `options.c:1010-1012`, reached via set-hook paths.)

### 3.3 `set-option` (`cmd-set-option.c`) — flags and exact semantics

```
set-option [-aFgopqsuUw] [-t target-pane] option [value]     (alias: set)
set-window-option [-aFgoqu] [-t target-window] option [value] (alias: setw)
```

**`set-window-option`/`setw` is a real separate command entry** (`cmd-set-option.c:50-61`) sharing the same exec function with an implied "window" flag (`window = (cmd_get_entry(self) == &cmd_set_window_option_entry)`, line 187). It is *not* a config-level alias — winmux's `unknown command: setw` means the command entry (and its alias) are missing. Same for `show-window-options`/`showw` (`cmd-show-options.c:54-65`).

Execution order (`cmd_set_option_exec`, `cmd-set-option.c:172-339`):

1. Argument 0 (the option name) is **format-expanded** unconditionally (line 196).
2. `options_match` parses name + optional `[index]` array key; failure → error unless `-q`.
3. The value is argument 1; with `-F` it is format-expanded (`format_single_from_target`).
4. **Scope resolution** (`options_scope_from_name`, `options.c:991-1060`) — for *named* options **the table decides the scope**, flags only pick global-vs-local within it:
   - SERVER-scope option → always `global_options` (any `-g`/`-w` is harmless/ignored).
   - SESSION-scope → `-g` ⇒ `global_s_options`, else the target session's options (errors `no such session: %s` / `no current session`).
   - WINDOW|PANE-scope + `-p` ⇒ the target pane's options; WINDOW (or no `-p`) → `-g` ⇒ `global_w_options`, else target window's options.
   - So plain `set -g automatic-rename off` works even though it's a window option — **`set` and `setw` are interchangeable for named options**; `setw` merely changes the default `-t` target type (window vs pane) and drops `-p`/`-s`/`-U` from its template.
   - For **`@`-options**, scope comes purely from flags (`options_scope_from_flags`, `options.c:1062-1115`): `-s` server, `-p` pane, `-w` (or setw) window, default session; plus `-g` for the global tree.
5. `-o`: only set if not already set *in that tree* (array keys checked individually); if set → error `already set: %s` (suppressed to success by `-q`).
6. `-u` **unset**: with an array key, deletes that element. Otherwise `options_remove_or_default` (`options.c:1456-1473`): in a **global** tree, named options are *reset to their default* (globals must always exist); in a session/window/pane tree the entry is *removed* so inheritance resumes. Unset of something not locally set is a silent no-op (`o == NULL → goto out`). `-U` (set-option only): like `-u` but for a window option also clears it from **every pane** in the target window first (`cmd-set-option.c:265-277`).
7. `@`-option set: value required ("empty value"); stored as plain string; `-a` appends with **no separator**.
8. Named non-array set: `options_from_string` (`options.c:1254-1334`) by type — errors: `empty value` (NULL value for non-FLAG/CHOICE), `value is <too small/too large/invalid>: %s` (NUMBER), `bad key: %s`, `bad colour: %s`, `bad value: %s` (FLAG), `unknown value: %s` (CHOICE), command parse error text (COMMAND). STRING appends with `-a` (using the table's `separator` — e.g. `,` for style options — empty default), then *validates after setting* and rolls back on failure (`options.c:1282-1293`): `not a suitable shell: %s` (default-shell), `value is invalid: %s` (pattern-checked options like default-size), **`invalid style: %s`** (IS_STYLE options — only checked when the value contains no `#{`), `invalid colour: %s` (IS_COLOUR string options).
9. Array options (no key): without `-a` the array is **cleared** then `options_array_assign` splits the value on the table separator (default `" ,"`; `""` separator = one whole-string element appended, used by hooks) and appends elements at the next free numeric indices. With key: `options_array_set` (append supported per-element with `-a`). Errors: `bad array key: %s`, `not an array: %s` (key given for non-array or @), `wrong array type`.
10. On success `options_push_changes(name)` (`options.c:1336-1454`) applies live side effects (redraws, key-table rebuild, history trim, etc.).

**What `-q` suppresses** (exactly): unknown/invalid option name, ambiguous name, scope-resolution errors (no such session/window/pane), and the `-o` "already set" error (`cmd-set-option.c:208-215, 228-232, 257-259`). It does **not** suppress bad-value errors, array-key errors, or "empty value".

### 3.4 User options (`@name`)

- Allowed at **any** scope, selected by flags (§3.3.4). Always type STRING, never in the table (`options.c:1274-1279`: non-`@` without table entry → `bad option name`).
- Arrays are not supported on `@`-options (`not an array` if `[i]` used).
- `set -a @x val` appends with no separator. `set -u @x` removes it (silently if absent).
- Format visibility: window-tree `@`-options are exported into neighbour-window formats (`format.c:4926-4954`); panes get `PANE_STYLECHANGED` when any `@`-option changes (`options.c:1407-1410`).

### 3.5 `show-options` (`cmd-show-options.c`)

```
show-options [-AgHpqsvw] [-t target-pane] [option]     (alias: show)
show-window-options [-gv] [-t target-window] [option]  (alias: showw)
```

- No option argument → list the whole selected scope: first any local (non-table, i.e. `@`) entries, then table entries whose scope matches; hooks are skipped unless `-H`. `-A` includes values inherited from the parent, printed with a `*` suffix after the name (`%s* %s`); empty arrays print just the name.
- With option argument: name is format-expanded, matched (prefix/ambiguity as above); only the **local** tree is consulted (`options_get_only`) unless `-A` (fall back to inherited, print with `*`).
- Output format: `name value` with string values re-escaped by `args_escape`; `-v` prints the value only; array options print each element as `name[i] value`.
- **`show -gqv "@foo"` when unset:** prints nothing, returns success — the `o == NULL && *name == '@'` branch errors `invalid option: %s` *only without* `-q` (`cmd-show-options.c:142-147`). This is the canonical "read a user option, empty if unset" idiom (TPM uses it).
- FLAG values print `on`/`off`; CHOICE prints the choice string; KEY prints key notation; COLOUR prints colour name.

### 3.6 Array option details

Keys are numeric (`options_array_correct_key` canonicalises); `options_parse` accepts `name[123]` (single index, `]` must end the string). Assignment order for bare `set opt "a,b,c"`: split, each element appended at the first free index. Elements print in numeric order. The main array options: `command-alias`, `terminal-overrides`, `terminal-features`, `user-keys`, `codepoint-widths`, `status-format`, `update-environment`, `pane-colours` (COLOUR-typed array), and all hooks (COMMAND-typed, empty separator).

---

## 4. Styles (`style.c`) and colours (`colour.c`)

### 4.1 Style grammar (`style_parse`, `style.c:68-299`)

Terms are separated by **spaces, commas, or newlines** — `const char delimiters[] = " ,\n"` (`style.c:72`). The user's `fg=white bg=black bold` is fully legal tmux; winmux's comma-only splitting is the bug. Runs of delimiters are skipped; each term ≤255 chars. Terms are matched **case-insensitively**. On *any* bad term the whole style is reverted to its pre-parse value and −1 returned (`style.c:296-298`) — the caller then reports `bad style:`/`invalid style:` with the original string. `style_parse` *adds onto* an existing style (base = default cell for options).

Complete term list:

| Term | Effect (fields per `style.c` lines) |
|---|---|
| `default` | reset fg/bg/us/attr/flags to the *base* cell; clears link (96-102) |
| `fg=<colour>` / `bg=<colour>` | set colour; the literal colour value `default` (=8) resets to base (221-235). Prefix match is really "2nd/3rd chars are `g=`" so `Fg=`/`BG=` work |
| `us=<colour>` | underscore colour (236-242) |
| `none` | attr = 0 (243-244) |
| `no<attr>` | clear that attribute (245-255); also `nolink` (clear hyperlink) and `noattr` (sets GRID_ATTR_NOATTR — "ignore pane attrs" marker used by mode-style) |
| `<attr>` | OR in an attribute (285-289, via `attributes_fromstring`) |
| `ignore` / `noignore` | set/clear the ignore flag (103-105) |
| `push-default` / `pop-default` / `set-default` | default-stack ops for `#[…]` in status-format (107-112) |
| `list=on|focus|left-marker|right-marker` / `nolist` | status window-list markers (113-125) |
| `range=left` / `range=right` / `range=control\|0-9` / `range=pane\|%N` / `range=window\|N` / `range=session\|$N` / `range=user\|<string>` / `norange` | clickable-range annotations (126-196); note pane requires `%`-prefix, session `$`-prefix |
| `align=left|centre|right|absolute-centre` / `noalign` | (197-209) |
| `fill=<colour>` | background-fill colour (210-213) |
| `dim=N` or `dim=N%` | dim percentage 0-100 (214-220) |
| `width=N` / `width=N%` | fixed width or percentage (256-270; used by prompt/scrollbar styles) |
| `pad=N` | padding (271-275) |
| `link=<uri>` / `link=` | OSC-8 hyperlink; empty clears (276-284) |

Anything else → attribute lookup → error if unknown.

**Attributes** (`attributes.c:56-109`, separators for attribute *lists* are `" ,|"`): `acs`, `bright`, **`bold` (= alias of `bright`, same bit)**, `dim`, `underscore`, `blink`, `reverse`, `hidden`, `italics`, `strikethrough`, `double-underscore`, `curly-underscore`, `dotted-underscore`, `dashed-underscore`, `overline`. Plus `default`/`none` = 0. Note there is **no** `underline` alias (it's `underscore`) and no `italic` (it's `italics`).

**`#{…}` inside style option values:** validation at set time is skipped if the string contains `#{` (`options.c:1177-1182`); at render time `options_string_to_style` (`options.c:1117-1159`) expands the format first, then parses; results are cached only for format-free styles (`o->cached`).

`style_parse_colour` (`style.c:476-496`) is for IS_COLOUR string options: empty string → fg −1 (none); else a single colour name (stored in fg); `default` keeps base.

`style_apply`/`style_add` (`style.c:440-473`): applying a named style option onto a cell copies fg/bg/us only when ≠8 (default) and ORs attrs.

### 4.2 Colour grammar (`colour_fromstring`, `colour.c:386-462`)

Accepted forms, in order of checking:

1. `#rrggbb` (exactly 7 chars, hex) → RGB.
2. `colourN` / `colorN`, N 0–255 → 256-colour palette entry.
3. `default` → 8; `terminal` → 9.
4. Theme colours (master only): `themeblack`, `themewhite`, `themelightgrey`, `themedarkgrey`, `themegreen`, `themeyellow`, `themered`, `themeblue`, `themecyan`, `thememagenta` (from `colour_theme_table`, `colour.c:35+`) — resolved per client theme via `dark-theme-*`/`light-theme-*` options.
5. Named ANSI: `black red green yellow blue magenta cyan white` (0–7) and the exact digit strings `"0"`–`"7"`; `brightblack`…`brightwhite` (90–97) and `"90"`–`"97"`. (No `brightXXX` for arbitrary names; `bright` here is only these eight.)
6. Anything else → `colour_byname` (`colour.c:566+`): the X11 rgb.txt name list (`AliceBlue`, `DarkSlateGray4`, …) including `grey0`–`grey100`/`gray0`–`gray100`; unknown → −1 → "bad colour".

`colour_tostring` (`colour.c:226-291`) round-trips: RGB → `#rrggbb`, 256 → `colourN`, named as above, −1 → `none`.

### 4.3 `#[…]` in status strings

The format engine passes `#[` sequences through untouched (`format.c:6332-6355` — `#[` and `##[`+ are copied out, with `format_skip1` used to find the closing `]` so formats inside styles survive); `format_draw` (format-draw.c) later parses each `#[…]` with `style_parse` and paints. `push-default`/`pop-default` maintain a stack of "default" styles so `#[default]` inside e.g. `status-left` restores the pushed outer style; `range=` marks mouse-clickable regions (window tabs use `range=window|N`); `list=` marks the scrollable window-list region and its overflow markers; `align=` splits the line into left/centre/right segments. `##[` renders a literal `#[`.

---

## 5. Formats (`format.c`)

### 5.1 Expansion loop (`format_expand1`, `format.c:6230-6398`)

Scanning for `#`:

- `#(command)` — run shell command as a **job**, substituting its last output line; results are cached per-status-interval; disabled under `FORMAT_NOJOBS` (config `%if`, style options). Brackets nest by counting `(`/`)`.
- `#{…}` — variable/modifier replacement (below). Closing brace found with `format_skip1`, which brace-counts nested `#{` and skips `#,`-style escapes.
- `#[…]` (and `##[`, `###[`, …) — style: passed through for format-draw (§4.3).
- `##` → literal `#`; `#}` → literal `}`; `#,` → literal `,` (crucial inside conditional arms).
- `#<single letter>` shorthands (`format_upper`/`format_lower`, `format.c:213-270`): `#D`=pane_id, `#F`=window_flags, `#H`=host, `#I`=window_index, `#P`=pane_index, `#S`=session_name, `#T`=pane_title, `#W`=window_name, `#h`=host_short. Anything else after `#` is copied literally.
- With `FORMAT_EXPAND_TIME` (used by `status-left`/`status-right`/`display-message`/`set-titles-string` via `format_expand_time`): the whole string is passed through `strftime` **first**, so bare `%H:%M %d-%b-%y` works; `%%` guards a literal percent.
- Recursion/loop limit `FORMAT_LOOP_LIMIT` (100); time limit `FORMAT_EXPAND_TIME_LIMIT` aborts runaway expansion.

Truthiness everywhere: `format_true` (`format.c:4502-4507`) — true iff non-empty and not exactly `"0"`.

### 5.2 `#{?cond,true,false}` conditionals (`format.c:6028-6103`)

- Separators found with `format_skip1`, so nested `#{…}` and `#,` escapes don't split.
- The condition is first looked up **directly as a variable name** (with modifiers applied); if not found, it is *expanded* and, if expansion changed nothing, treated as false.
- Chained pairs are supported: `#{?c1,v1,c2,v2,fallback}` — evaluated pairwise; an unpaired trailing arg is the else-value; no match and no trailing arg → empty.
- Arms are themselves expanded on selection.

### 5.3 Modifiers (`format_build_modifiers`, `format.c:4546-4657`; dispatch `format.c:5590-5822`)

Syntax: `#{mod;mod;…:name}` — modifiers before a `:`, separated by `;`. Arg forms: bare (`=5:`), single unwrapped arg (`t/f:` style flags), or punctuation-wrapped multi-arg (`s/a/b/`, `m|p|t`). Wrapped args are format-expanded when built.

| Modifier | Meaning |
|---|---|
| `b:` / `d:` | basename / dirname of the value |
| `t:` | value is a Unix time → ctime-style string. Flags: `t/p:` pretty (age), `t/r:` relative, `t/f/<fmt>:` custom strftime format (`%` written `%%` inside) |
| `=N:` / `=/N/marker:` | truncate to width N (N<0: keep right end); optional marker string appended (prepended if right-truncating) when truncation happened |
| `p N` (`pN:` / `p/-N/`) | pad to width N (left-pad; negative = right-pad) |
| `l:` | literal — do not expand the content (unescape only) |
| `E:` | expand the *result* again (used for style options: `#{E:status-left-style}`) |
| `T:` | expand the result with strftime (time formats inside) |
| `S:`, `W:`, `P:`, `L:` | loop over sessions / windows in current session / panes in current window / clients, expanding the content for each and concatenating; `#{W:all,active}` form gives a different template for the active item. Optional sort-flags arg (`i` index, `n` name, `t` activity/time, `r` reverse; `P` also `z`). Loops define `loop_index`, `loop_last_flag`, window loops also `window_after_active`, `next_window_index`, etc. |
| `O:` / `V:` | loop over options / environment (master-only) |
| `N:` / `N/w:` / `N/s:` | does a window (or session) by that *name* exist → 1/0 |
| `C:` (`C/r`, `C/i`) | search pane content for string/regex → line number or 0 |
| `s/pat/repl/` (`s/pat/repl/i`) | ERE regex substitution, global; `\1`-style backrefs via `regsub` |
| `m/pat/str/` (flags `r` regex, `i` ci, `p`/`z` fuzzy) | match → 1/0; without `r` it is **fnmatch** glob |
| `<` / `>` | string comparison modifiers (with `:cmp` form below) |
| `q:` | quote shell metacharacters; `q/s:` single-quote-wrap; `q/e:`/`q/h:` double `#` (style-safe); `q/a:` argument-quote |
| `E`,`n:` | `n:` = length of value; `w:` = display width |
| `a:` | value (32–126) → that ASCII character |
| `c:` | colour name → `rrggbb` hex; `c/f:`/`c/b:` → SGR escape for fg/bg |
| `I/…` | client termcap (`c`), feature (`f`), or client-environ (`e`) lookup |
| `R:left,N` | repeat left string N times |
| `e|op|f|prec:x,y` | arithmetic: op ∈ `+ - * / % m == != < > <= >=`; `f` = float, prec digits (`format_replace_expression`, `format.c:5415-5555`) |
| `!:x` / `!!:x` | boolean NOT / double-NOT (normalise to 1/0) |
| `&&:a,b,…` / `\|\|:a,b,…` | n-ary boolean AND/OR over comma-separated operands (`format.c:4783-4819`) |
| `==:a,b` `!=:a,b` `<:a,b` `>:a,b` `<=:a,b` `>=:a,b` | **string** comparisons (both sides expanded) → 1/0 (`format.c:5993-6023`). Numeric comparison is the `e|…` form |

Order of post-processing after the base value is found (`format.c:6124-6199`): `E`/`T` re-expand → `s///` substitutions (in order) → `=` truncate → `p` pad → `n`/`w` length/width replacement.

If the content of `#{…}` isn't a known variable but contains `#{`, it is recursively expanded (`format.c:6108-6112`); unknown plain names expand to empty string.

### 5.4 Important format variables (from `format_table`, `format.c:3264+`, and defaults)

Core identity/state set winmux should support (variable — meaning):

- **Session:** `session_name` (#S), `session_id` ($N), `session_windows`, `session_attached`, `session_created`, `session_activity`, `session_last_attached`, `session_grouped`, `session_group`, `session_many_attached`, `session_marked`, `session_alerts`, `session_stack`, `session_path`, `session_format` (1 in session context).
- **Window:** `window_name` (#W), `window_index` (#I), `window_id` (@N), `window_flags` (#F: `*` current, `-` last, `#` activity, `!` bell, `~` silence, `Z` zoomed, `M` marked), `window_raw_flags`, `window_active`, `window_panes`, `window_width`, `window_height`, `window_layout`, `window_visible_layout`, `window_zoomed_flag`, `window_last_flag`, `window_bell_flag`, `window_activity_flag`, `window_silence_flag`, `window_linked`, `window_marked_flag`, `window_start_flag`, `window_end_flag`, `window_stack_index`, `window_format`, `window_activity`.
- **Pane:** `pane_id` (#D, `%N`), `pane_index` (#P), `pane_title` (#T), `pane_active`, `pane_width`, `pane_height`, `pane_left`/`pane_top`/`pane_right`/`pane_bottom`, `pane_at_left`/`pane_at_top`/`pane_at_right`/`pane_at_bottom`, `pane_current_command`, `pane_current_path`, `pane_start_command`, `pane_pid`, `pane_tty`, `pane_dead`, `pane_dead_status`, `pane_dead_signal`, `pane_dead_time`, `pane_in_mode`, `pane_mode`, `pane_synchronized`, `pane_marked`, `pane_marked_set`, `pane_last`, `pane_zoomed_flag` (via window), `pane_search_string`, `pane_fg`, `pane_bg`, `pane_flags`.
- **Client:** `client_name`, `client_tty`, `client_termname`, `client_width`, `client_height`, `client_pid`, `client_activity`, `client_created`, `client_prefix` (1 while prefix pending — status-left indicators!), `client_key_table`, `client_readonly`, `client_session`, `client_last_session`, `client_flags`, `client_utf8`, `client_theme`.
- **Server/global:** `host` (#H), `host_short` (#h), `pid`, `version`, `socket_path`, `start_time`, `config_files`, `uid`, `user`, `next_session_id`, `server_sessions`.
- **Mouse (valid during mouse key bindings):** `mouse_x`, `mouse_y`, `mouse_word`, `mouse_line`, `mouse_hyperlink`, `mouse_pane`, `mouse_status_line`, `mouse_status_range`, `mouse_any_flag`, `mouse_all_flag`, `mouse_button_flag`, `mouse_sgr_flag`, `mouse_standard_flag`.
- **Buffers:** `buffer_name`, `buffer_size`, `buffer_sample`, `buffer_created`.
- **Copy mode (mode formats):** `selection_present`, `selection_active`, `copy_cursor_x/y`, `copy_cursor_word`, `copy_cursor_line`, `search_present`, `search_match`, `scroll_position`, `history_size`, `history_limit`, `history_bytes` (some provided by the mode's `formats` callback, `window-copy.c`).
- **Loop-injected:** `loop_index`, `loop_last_flag`, `window_after_active`, `window_before_active`, `next_window_index`, `prev_window_index`, plus prefixed `@`-options of neighbours.
- **In command contexts:** `current_file` (during config load, `cfg.c:139`), `command`, `command_list_name`/`_alias`/`_usage` (list-commands), `hook`, `hook_session`… (hooks), numbered `%1`… are *not* formats (they're template substitution).

`#{format}` names may also be **any option name prefixed by its scope resolution**: `format_find` falls back to looking up options (`#{status-left}` yields the option's raw value; `E:` then expands it) and environment (session then global).

---

## 6. Key notation (`key-string.c`)

### 6.1 Named keys (`key_string_table`, `key-string.c:31-192`, matched case-insensitively)

- Function: `F1`–`F12`.
- Editing: `IC`/`Insert`, `DC`/`Delete`, `Home`, `End`, `NPage`/`PageDown`/`PgDn`, `PPage`/`PageUp`/`PgUp`, `BTab` (back-tab), `Space` (= 0x20), `BSpace`.
- C0 names: `Tab` (=C-i/0x09), `Enter` (=C-m/0x0d), `Escape` (=0x1b). Other C0 codes render as `[NUL]`, `[SOH]`, `[BEL]` etc. but are not intended as bindable names.
- Arrows: `Up`, `Down`, `Left`, `Right`.
- Keypad: `KP/ KP* KP- KP+ KP.` `KP0`–`KP9`, `KPEnter`.
- `None`, `Any` (special: `Any` binds a catch-all in a table).
- `User0`–`User999` (`KEYC_NUSER`) — mapped from `user-keys` array option.

Most named keys carry `KEYC_IMPLIED_META` in the table; when looked up *without* an explicit `M-` prefix that flag is stripped (`key-string.c:318-319`) — relevant only to output encoding.

### 6.2 Modifiers and literal keys (`key_string_lookup_string`, `key-string.c:242-323`)

- Prefixes `C-`, `M-`, `S-` in any order, **case-insensitive** (`c- m- s-` fine; `key-string.c:218-236`). Anything else before `-` → `KEYC_UNKNOWN`.
- Legacy `^x` = `C-x` (`^` + single char; with more chars, `^` acts as `C-` prefix) — lowercased.
- Single ASCII printable char (32–126) is itself; **case matters and is the whole story for shift**: bind `A` for shift-a. `S-a` is a *distinct* key (`a` + SHIFT bit) that terminals typically never send for plain letters — tmux keeps `S-` mainly for special keys (`S-Up`, `S-F5`). `S-Tab` = `BTab` conceptually but they are distinct codes; the table name is `BTab`.
- `0xNN` hex: codepoint (<32 allowed as raw control code).
- Any single UTF-8 character is a key (`é`, `ら`), with modifiers allowed.
- C0 via `C-` notation: `C-a`…, `C-?` (=0x7f), `C-Space`.
- Invalid → `KEYC_UNKNOWN` → `bind`: `unknown key: %s`.

`key_string_lookup_key` (`key-string.c:326-485`) is the inverse; special internal names it can emit: `FocusIn`, `FocusOut`, `PasteStart`, `PasteEnd`, `Mouse`, `Dragging`, `MouseMovePane`, `ReportDarkTheme`, etc.

### 6.3 Mouse key names (`tmux.h:236-290`, `key-string.c:126-191`)

Event kinds: `MouseDown`, `MouseUp`, `MouseDrag`, `MouseDragEnd`, `SecondClick`, `DoubleClick`, `TripleClick` — each with button number `1 2 3 6 7 8 9 10 11` (4/5 are the wheel) — plus `WheelUp`, `WheelDown` (no button number). Each combines with a **location suffix**: `Pane`, `Status`, `StatusLeft`, `StatusRight`, `StatusDefault`, `Border`, `ScrollbarUp`, `ScrollbarSlider`, `ScrollbarDown`, `Control0`–`Control9` (clickable `range=control|N` regions). Examples: `MouseDown1Pane`, `MouseDragEnd1Pane`, `WheelUpStatus`, `DoubleClick3Border`. Modifiers combine: `C-MouseDown1Pane`, `M-MouseDrag1Border`. Mouse keys are bound like any key (usually with `-n`/root, and in `copy-mode`/`copy-mode-vi` tables).

### 6.4 bind-key / unbind-key semantics

`bind-key [-nr] [-T table] [-N note] key [command …]` (`cmd-bind-key.c:35-107`):

- Table: `-T name`; `-n` = `-T root`; default `prefix`.
- **Binding into a table that doesn't exist yet creates it** — `key_bindings_add` → `key_bindings_get_table(name, create=1)` (`key-bindings.c:132-151, 225`). Arbitrary table names are legal (`bind -Tmove …` is even used by the defaults).
- With **no command**, `bind -N note key` just annotates an existing binding (and `-r` can add the repeat flag) — it does not error if absent, it's a no-op (`key-bindings.c:228-238`).
- One string argument → parsed as a command string; one brace block → used directly; multiple arguments → `cmd_parse_from_arguments` (so trailing-`;` splitting per §2.2 applies).
- Rebinding replaces the old binding wholesale.
- `-r`: repeat flag. After a repeatable binding fires, the client stays in the same key table for `repeat-time` ms (default 500; first press window `initial-repeat-time` if nonzero) so the key can be pressed again without the prefix; *any* key that is not bound-with-repeat in that table ends repeat mode. (Server-side in `server-client.c`; the flag is `KEY_BINDING_REPEAT`.)

`unbind-key [-anq] [-T table] key` (`cmd-unbind-key.c:42-104`):

- `-a`: remove **the whole table** (`key given with -a` if a key is also present). Errors `table %s doesn't exist` for unknown `-T` table — suppressed by `-q`.
- Single key: `-T` table must exist (`table %s doesn't exist`, suppressed by `-q`); `-n`/default tables (root/prefix) skip that existence check. Removing a key that isn't bound is a silent no-op.
- `-q` also silences `missing key` and `unknown key: %s`.
- **Why the user's `unbind -T copy-mode-vi MouseDragEnd1Pane` must work:** tmux's default binding set (`key_bindings_init`, `key-bindings.c:377-738` — the full default list, executed as real `bind` commands at server start, *before* config) populates `copy-mode` and `copy-mode-vi` tables; the tables therefore always exist by config time. winmux must either pre-create `copy-mode`/`copy-mode-vi` (correct fix: define defaults as bindings in those tables) or auto-create on unbind.
- After defaults are installed, a snapshot is stored as each table's `default_key_bindings` (`key_bindings_init_done`, `key-bindings.c:354-374`); `list-keys` uses it to mark changes, and "reset" restores from it. Removing the last binding of a table with no defaults frees the table (`key-bindings.c:281-286`).

Key tables at runtime: a client's current table is switched with `switch-client -T table` (root-table prefix-key handling sets `prefix` table; modes set `copy-mode`/`copy-mode-vi` via the mode's `key_table` callback resolved through `mode-keys`). The default lookup table for new keys is the session's `key-table` option (default `root`).

`list-keys [-1aN] [-P prefix-string] [-T table]`: prints `bind-key -T <table> <key> <command>` lines re-parseable as config; `-N` shows notes (only keys with notes); `-1` first match only; `-a` includes keys without notes when `-N`.

### 6.5 send-keys (`cmd-send-keys.c`)

`send-keys [-FHKlMRX] [-c client] [-N repeat] [-t pane] key …` (alias `send`):

- Each argument is first tried as **key notation** (`key_string_lookup_string`); if it parses, that key code is delivered; otherwise (or with `-l`) every UTF-8 character of the argument is sent individually (`cmd-send-keys.c:131-157`).
- `-l` literal (skip notation lookup); `-H` hex byte (`send-keys -H 1b`); `-N n` repeat count; `-R` reset pane terminal state; `-M` pass through the triggering mouse event; `-X` execute a **copy-mode command** (`send -X begin-selection` — this is how all copy-mode bindings work); `-K` treat keys as if typed at the client (goes through key bindings).
- `send-prefix [-2]` sends the key from the `prefix` (`prefix2`) option into the pane.

---

## 7. Prompts, confirmation, messages

### 7.1 `command-prompt` (`cmd-command-prompt.c`)

```
command-prompt [-1bCeFiklNP] [-I inputs] [-p prompts] [-t client] [-T type] [template]
```

- `template` may be a string or brace block; default template is **`%1`** (`args_make_commands_prepare(..., "%1", ...)`, line 110) — i.e. execute what was typed. `-F` expands the template as a format *before* `%`-substitution.
- `-p prompts`: comma-separated prompt list; each gets a trailing space appended. With no `-p`: prompt is `(command-name) ` derived from the template's first word, or `:` (no trailing space) when there's no template.
- `-I inputs`: comma-separated initial inline defaults, paired with prompts by index (missing → empty). This is how `,` pre-fills the window name: `command-prompt -I'#W' { rename-window -- '%%' }`. Inputs/prompts are format-expanded by the status-prompt layer.
- `-l`: single prompt, no comma-splitting of `-p`/`-I` (literal commas allowed).
- Multiple prompts: responses collected one at a time (callback re-arms with the next prompt, `cmd-command-prompt.c:216-225`); response i substitutes `%i`; first `%%` = `%1` (§2.9).
- Mode flags (mutually exclusive, first wins): `-1` `PROMPT_SINGLE` — first key press completes as the response; `-N` `PROMPT_NUMERIC` — like `-1` but digits only; `-i` `PROMPT_INCREMENTAL` — callback fires on **every** keystroke (search highlighting), commands get the partial text, prompt stays open until Enter/Escape; `-k` `PROMPT_KEY` — the pressed key's *name* (key notation) is the response (used by the default `/` describe-key binding). Also `-e` (Backspace on empty exits), `-C` (don't freeze client), `-b` (don't block the queue), `-P` (attach prompt to the pane, master-only).
- `-T type`: prompt history class — one of `command`, `search`, `target`, `window-target` (`prompt_type`); affects history bucket and completion (command names complete with Tab in `command` type).
- Cancelling (Escape/C-c/C-g or `q` in vi mode with empty line) yields response NULL → nothing executed.
- If the client already has a prompt, the command silently does nothing (`cmd-command-prompt.c:100-101`).
- Prompt line editing/history is `status.c` (`status_prompt_set`); history persists to `history-file` per prompt-type, limited by `prompt-history-limit`.

### 7.2 `confirm-before` (`cmd-confirm-before.c`)

```
confirm-before [-by] [-c confirm-key] [-p prompt] [-t client] command
```

- Command may be string or brace block; parsed **immediately** (`args_make_commands_now`, line 78) so syntax errors surface right away.
- Prompt: `-p` text gets a trailing space; default is `Confirm '<name>'? (y/n) ` where `<name>` is the first command's name and `y` is the confirm key (line 106-108).
- Runs a `PROMPT_SINGLE` prompt: one key. The command fires iff the key equals the confirm key (`-c X`, default `y`, must be a single printable char else `invalid confirm key`) — **or Enter when `-y`** (default-yes: `s[0] == '\r' && default_yes`, line 134). Anything else cancels.
- `-b`: don't block the queue while waiting. Blocking is the default: the invoking item waits (matters for `bind x confirm-before kill-pane` sequencing).
- On cancel with an unattached invoking client, client exit status is set to 1 (line 148-152).
- Note: prompt text is *not* format-expanded by confirm-before itself; the status-prompt layer expands `#{}`/`#S`-style content in prompts (status.c `status_prompt_set` formats the prompt string), which is why `confirm-before -p"kill-window #W? (y/n)"` shows the window name.

### 7.3 `display-message` (`cmd-display-message.c`)

```
display-message [-aCIlNpv] [-c target-client] [-d delay] [-F format] [-t pane] [message]
```

- Message template: the positional argument, else `-F`, else the default `[#{session_name}] #{window_index}:#{window_name}, current pane #{pane_index} - (%H:%M %d-%b-%y)`. Giving both `-F` and an argument is an error (`only one of -F or argument must be given`).
- Expansion is **`format_expand_time`** — strftime `%` codes work directly in the message (line 141).
- Output routing (lines 143-155): no client → error stream (during config: a cause); `-p` → print to stdout of the invoking client (cmdq_print); control client → `%message`; otherwise → status-line message for `-d` milliseconds (`-d 0` = until a key is pressed; default −1 = the `display-time` option, 750ms). `-N` = ignore keys while showing (don't let a keypress dismiss+consume); `-C` = don't clear the prompt. `-l` = literal, no expansion. `-a` = list **all** format variables as `name=value` lines. `-v` = verbose expansion trace. `-I` = forward stdin of the attached client into the pane.
- Styling: messages draw with `message-style` (+ `message-line` position, `message-format` template wrapping `#{message}`).

---

## 8. run-shell / if-shell / environment

### 8.1 Shell selection

Jobs run via `job_run` (`job.c:72-…`): shell = the session's (or global) **`default-shell` option**, validated by `checkshell` (`tmux.c:80-88`: must be absolute path, executable, and not tmux itself), else fallback `_PATH_BSHELL` (`/bin/sh`). Invoked as `execl(shell, argv0, "-c", cmd, NULL)` — always `sh -c` style, non-login. `default-shell`'s own default is initialised from `$SHELL`/passwd (`getshell`, `tmux.c:62-77`). During config (`!cfg_finished`) jobs don't get `TERM` set (`job.c:87-92`).

### 8.2 `run-shell` (`cmd-run-shell.c`)

```
run-shell [-bCE] [-c start-directory] [-d delay] [-t pane] [shell-command …]
```

- Default: **blocks** the command queue until the job exits (`CMD_RETURN_WAIT`); `-b` runs in background.
- The shell command is format-expanded (`format_expand` with target defaults, extra numbered formats `#{1}`… for extra arguments, lines 136-145).
- `-C`: the argument is a **tmux command** (string or braces), format-expanded and queued — no shell at all.
- `-d seconds` (float): delay before running; with no command, pure sleep.
- Output: each stdout line (stderr too with `-E`) is shown — in the target pane (`-t`) or the current pane — by switching it into `view` mode (a copy-mode view); when invoked from an attached command queue the lines go to cmdq_print instead (line 88-99).
- **Exit status:** nonzero exit appends a line `'<cmd>' returned <code>`; killed by signal → `'<cmd>' terminated by signal <n>` (lines 274-286). For an unattached client the exit status propagates to the client's return code.

### 8.3 `if-shell` (`cmd-if-shell.c`)

```
if-shell [-bF] [-t pane] shell-command command [command]
```

- Without `-F`: format-expand the first argument, run it as a shell job (same shell rules); **exit status 0 → first command, else second** (if given) (lines 148-152). Blocks unless `-b`.
- **`-F`: no shell at all** — the first argument is format-expanded and tested for truthiness with the *inline* rule `*s != '0' && *s != '\0'` (line 88; nonempty and not starting with `0` — note: subtly *different* from `format_true`, which only treats the exact string `"0"` as false; `if -F "01"` is FALSE here but `#{?01,…}` is true). Commands are parsed immediately and queued.
- Command arguments may be strings or brace blocks (indices 1 and 2 accept `COMMANDS_OR_STRING`).
- With no client/session context the target-derived cwd/format defaults still apply (config-time `if-shell` works; `#()`-free).

### 8.4 Environment (`environ.c`, `cmd-set-environment.c`) — brief

- Two layers: `global_environ` (server, seeded from the server process's environment, `tmux.c:400-403`) and per-session `s->environ`. Panes get merged global→session env (`environ_for_session`), minus `ENVIRON_HIDDEN` entries.
- `set-environment [-Fhgru] [-t session] name [value]` (alias `setenv`): `-g` global tree; `-h` hidden; `-u` unset; `-r` mark for **removal** from child environments; `-F` expand value. Errors: `empty variable name`, `variable name contains =`.
- `show-environment [-hgs] [name]` (alias `showenv`): `-s` emits Bourne-shell `export` syntax; removed-marked vars show as `-name`.
- `update-environment` (session option array) lists variables copied from the attaching client into the session env on attach (defaults include `DISPLAY SSH_AUTH_SOCK SSH_CONNECTION …`, `options-table.c:1223-1233`).
- Config-file `VAR=value` lines write the **global** environ (§1.5) — that's how TPM sets things pre-server.

---

## 9. Appendix A — complete command table (name, alias, args template, positional min/max)

From every `cmd-*.c` entry struct (master `db115c6`). "—" = no alias. This *is* the resolution table for §2.7.

| Command | Alias | Args template | Min | Max |
|---|---|---|---|---|
| attach-session | attach | `c:dEf:rt:x` | 0 | 0 |
| bind-key | bind | `nrN:T:` | 1 | −1 |
| break-pane | breakp | `abdPF:n:s:t:Wx:X:y:Y:` | 0 | 0 |
| capture-pane | capturep | `ab:CeE:FHJLMNpPqRS:Tt:` | 0 | 0 |
| choose-buffer | — | `F:f:K:kNO:rt:yZ` | 0 | 1 |
| choose-client | — | `F:f:hiK:kNO:rt:yZ` | 0 | 1 |
| choose-tree | — | `F:f:GhK:kNO:rst:wyZ` | 0 | 1 |
| clear-history | clearhist | `Ht:` | 0 | 0 |
| clear-prompt-history | clearphist | `T:` | 0 | 0 |
| clock-mode | — | `t:` | 0 | 0 |
| command-prompt | — | `1CbeFiklI:NPp:t:T:` | 0 | 1 |
| confirm-before | confirm | `bc:p:t:y` | 1 | 1 |
| copy-mode | — | `deHMqSs:t:u` | 0 | 0 |
| customize-mode | — | `F:f:kNt:yZ` | 0 | 0 |
| delete-buffer | deleteb | `b:` | 0 | 0 |
| detach-client | detach | `aE:s:t:P` | 0 | 0 |
| display-menu | menu | `b:c:C:H:s:S:MOt:T:x:y:` | 1 | −1 |
| display-message | display | `aCc:d:lINpt:F:v` | 0 | 1 |
| display-panes | displayp | `bd:Nt:` | 0 | 1 |
| display-popup | popup | `Bb:Cc:d:e:Eh:kNs:S:t:T:w:x:y:` | 0 | −1 |
| find-window | findw | `CiNrt:TZ` | 1 | 1 |
| has-session | has | `t:` | 0 | 0 |
| if-shell | if | `bFt:` | 2 | 3 |
| join-pane | joinp | `bdfhvp:l:s:t:` | 0 | 0 |
| kill-pane | killp | `af:t:` | 0 | 0 |
| kill-server | — | `` | 0 | 0 |
| kill-session | — | `aCgf:t:` | 0 | 0 |
| kill-window | killw | `af:t:` | 0 | 0 |
| last-pane | lastp | `det:Z` | 0 | 0 |
| last-window | last | `t:` | 0 | 0 |
| link-window | linkw | `abdks:t:` | 0 | 0 |
| list-buffers | lsb | `F:f:O:r` | 0 | 0 |
| list-clients | lsc | `F:f:O:rt:` | 0 | 0 |
| list-commands | lscm | `F:` | 0 | 1 |
| list-keys | lsk | `1aF:NO:P:rT:` | 0 | 1 |
| list-panes | lsp | `aF:f:O:rst:` | 0 | 0 |
| list-sessions | ls | `F:f:O:r` | 0 | 0 |
| list-windows | lsw | `aF:f:O:rt:` | 0 | 0 |
| load-buffer | loadb | `b:t:w` | 1 | 1 |
| lock-client | lockc | `t:` | 0 | 0 |
| lock-server | lock | `` | 0 | 0 |
| lock-session | locks | `t:` | 0 | 0 |
| move-pane | movep | `bdD::fhMvl:L::P:R::s:t:U::X:Y:z:` | 0 | 0 |
| move-window | movew | `abdkrs:t:` | 0 | 0 |
| new-pane | newp | `bB:c:de:EfF:hIkl:Lm:p:PR:s:S:t:T:vWx:X:y:Y:Z` | 0 | −1 |
| new-session | new | `Ac:dDe:EF:f:n:Ps:t:x:Xy:` | 0 | −1 |
| new-window | neww | `abc:de:EF:kn:PSt:` | 0 | −1 |
| next-layout | nextl | `t:` | 0 | 0 |
| next-window | next | `at:` | 0 | 0 |
| paste-buffer | pasteb | `db:prSs:t:` | 0 | 0 |
| pipe-pane | pipep | `IOot:` | 0 | 1 |
| previous-layout | prevl | `t:` | 0 | 0 |
| previous-window | prev | `at:` | 0 | 0 |
| refresh-client | refresh | `A:B:cC:Df:r:F:lLRSt:U` | 0 | 1 |
| rename-session | rename | `t:` | 1 | 1 |
| rename-window | renamew | `t:` | 1 | 1 |
| resize-pane | resizep | `D::L::MR::Tt:U::x:y:Z` | 0 | 1 |
| resize-window | resizew | `aADLRt:Ux:y:` | 0 | 1 |
| respawn-pane | respawnp | `c:e:Ekt:` | 0 | −1 |
| respawn-window | respawnw | `c:e:Ekt:` | 0 | −1 |
| rotate-window | rotatew | `Dt:UZ` | 0 | 0 |
| run-shell | run | `bd:Ct:Es:c:` | 0 | −1 |
| save-buffer | saveb | `ab:` | 1 | 1 |
| select-layout | selectl | `Enopt:` | 0 | 1 |
| select-pane | selectp | `DdegLlMmP:RT:t:UZ` | 0 | 0 |
| select-window | selectw | `lnpTt:` | 0 | 0 |
| send-keys | send | `c:FHKlMN:Rt:X` | 0 | −1 |
| send-prefix | — | `2t:` | 0 | 0 |
| server-access | — | `adglrw` | 0 | 1 |
| set-buffer | setb | `ab:t:n:w` | 0 | 1 |
| set-environment | setenv | `Fhgrt:u` | 1 | 2 |
| set-hook | — | `agpRt:uB:w` | 0 | 2 |
| set-option | set | `aFgopqst:uUw` | 1 | 2 |
| set-window-option | setw | `aFgoqt:u` | 1 | 2 |
| show-buffer | showb | `b:` | 0 | 0 |
| show-environment | showenv | `hgst:` | 0 | 1 |
| show-hooks | — | `Bgpt:w` | 0 | 1 |
| show-messages | showmsgs | `JTt:` | 0 | 0 |
| show-options | show | `AgHpqst:vw` | 0 | 1 |
| show-prompt-history | showphist | `T:` | 0 | 0 |
| show-window-options | showw | `gvt:` | 0 | 1 |
| source-file | source | `t:Fnqv` | 1 | −1 |
| split-window | splitw | `bc:de:EfF:hIkl:m:p:PR:s:S:t:T:vWZ` | 0 | −1 |
| start-server | start | `` | 0 | 0 |
| suspend-client | suspendc | `t:` | 0 | 0 |
| swap-pane | swapp | `dDs:t:UZ` | 0 | 0 |
| swap-window | swapw | `ds:t:` | 0 | 0 |
| switch-client | switchc | `c:EFlnO:pt:rT:Z` | 0 | 0 |
| switch-mode | — | `F:kst:wZ` | 0 | 1 |
| unbind-key | unbind | `anqT:` | 0 | 1 |
| unlink-window | unlinkw | `kt:` | 0 | 0 |
| wait-for | wait | `LSU` | 1 | 1 |

Plus the `command-alias` defaults: `split-pane`, `splitp` → split-window; `server-info`, `info` → `show-messages -JT`; `choose-window` → `choose-tree -w`; `choose-session` → `choose-tree -s`.

---

## 10. Appendix B — complete options table (`options-table.c:282-1954`)

Scope key: **Srv** = server, **Ses** = session, **Win** = window, **W/P** = window+pane. Type key: str, num(min–max), key, colour, flag, choice{…}, cmd. Flags: [A]=array, [St]=style-checked, [Co]=colour-string, [H]=hook.

### Server options

| Option | Type | Default |
|---|---|---|
| backspace | key | `C-?` (0177) |
| buffer-limit | num(1–INT_MAX) | 50 |
| command-alias | str[A] sep=`,` | `split-pane=split-window,splitp=split-window,server-info=show-messages -JT,info=show-messages -JT,choose-window=choose-tree -w,choose-session=choose-tree -s` |
| codepoint-widths | str[A] sep=`,` | "" |
| copy-command | str | "" |
| default-client-command | cmd | `new-session` |
| default-terminal | str | `screen`/`tmux-256color` (build TMUX_TERM) |
| editor | str | `_PATH_VI` |
| escape-time | num(0–INT_MAX) ms | **10** (was 500 in ≤3.4) |
| exit-empty | flag | on |
| exit-unattached | flag | off |
| extended-keys | choice{off,on,always} | off |
| extended-keys-format | choice{csi-u,xterm} | xterm |
| focus-events | flag | off |
| get-clipboard | choice{off,buffer,request,both} | buffer |
| history-file | str | "" |
| input-buffer-size | num(INPUT_BUF_DEFAULT_SIZE–UINT_MAX) | 1048576 |
| message-limit | num(0–INT_MAX) | 1000 |
| prefix-timeout | num(0–INT_MAX) ms | 0 (disabled) |
| prompt-history-limit | num(0–INT_MAX) | 100 |
| set-clipboard | choice{off,external,on} | external |
| terminal-overrides | str[A] sep=`,` | `linux*:AX@` |
| terminal-features | str[A] sep=`,` | `xterm*:clipboard:ccolour:cstyle:focus:title,screen*:title,rxvt*:ignorefkeys` |
| theme | choice{detect,terminal,light,dark} | detect |
| dark-theme-black/white/light-grey/dark-grey/green/yellow/red/blue/cyan/magenta | str[Co] | conditional formats (e.g. dark-theme-black = `#{?#{e|>=:#{client_colours},256},gray5,black}`) |
| light-theme-black/…/magenta | str[Co] | analogous |
| user-keys | str[A] sep=`,` | "" |
| variation-selector-always-wide | flag | on |

### Session options

| Option | Type | Default |
|---|---|---|
| activity-action | choice{none,any,current,other} | other |
| assume-paste-time | num(0–INT_MAX) ms | 1 |
| base-index | num(0–INT_MAX) | 0 |
| bell-action | choice{none,any,current,other} | any |
| default-command | str | "" |
| default-shell | str | `_PATH_BSHELL` (init from $SHELL) |
| default-size | str pattern `[0-9]*x[0-9]*` | `80x24` |
| destroy-unattached | choice{off,on,keep-last,keep-group} | off |
| detach-on-destroy | choice{off,on,no-detached,previous,next} | on |
| display-panes-active-colour | str[Co] | themered |
| display-panes-colour | str[Co] | themeblue |
| display-panes-format | str | `#[align=right]#{pane_width}x#{pane_height}` |
| display-panes-time | num(1–INT_MAX) ms | 1000 |
| display-time | num(0–INT_MAX) ms | 750 |
| focus-follows-mouse | flag | off |
| history-limit | num(0–INT_MAX) lines | 2000 |
| initial-repeat-time | num(0–2000000) ms | 0 |
| key-table | str | root |
| lock-after-time | num(0–INT_MAX) s | 0 |
| lock-command | str | `lock -np` (TMUX_LOCK_CMD) |
| message-command-style | str[St] sep=`,` | `bg=themeblack,fg=themeyellow,…fill=themeblack` |
| message-format | str | `#[#{?#{command_prompt},#{E:message-command-style},#{E:message-style}}]#{message}` |
| message-line | choice{0,1,2,3,4} | 0 |
| message-style | str[St] sep=`,` | `bg=themeyellow,fg=themeblack,…fill=themeyellow` |
| mouse | flag | off (TMUX_MOUSE) |
| prefix | key | `C-b` |
| prefix2 | key | None |
| renumber-windows | flag | off |
| repeat-time | num(0–2000000) ms | 500 |
| set-titles | flag | off |
| set-titles-string | str | `#S:#I:#W - "#T" #{session_alerts}` |
| silence-action | choice{none,any,current,other} | other |
| status | choice{off,on,2,3,4,5} | on |
| status-bg | colour | 8 (default; deprecated) |
| status-fg | colour | 8 (deprecated) |
| status-format | str[A] | 3-element default (window list / pane list / session list — see `options-table.c:119-239`) |
| status-interval | num(0–INT_MAX) s | 15 |
| status-justify | choice{left,centre,right,absolute-centre} | left |
| status-keys | choice{emacs,vi} | emacs |
| status-left | str | `[#{session_name}] ` |
| status-left-length | num(0–SHRT_MAX) | 10 |
| status-left-style | str[St] sep=`,` | default |
| status-position | choice{top,bottom} | bottom |
| status-right | str | `#{?window_bigger,[#{window_offset_x}#,#{window_offset_y}] ,}"#{=21:pane_title}" %H:%M %d-%b-%y` |
| status-right-length | num(0–SHRT_MAX) | 40 |
| status-right-style | str[St] sep=`,` | default |
| status-style | str[St] sep=`,` | `bg=themegreen,fg=themeblack` |
| prompt-cursor-colour / prompt-command-cursor-colour | str[Co] | "" |
| prompt-cursor-style / prompt-command-cursor-style | choice{default,blinking-block,block,blinking-underline,underline,blinking-bar,bar} | default |
| update-environment | str[A] | `DISPLAY KRB5CCNAME MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK SSH_AGENT_PID SSH_CONNECTION WAYLAND_DISPLAY WINDOWID XAUTHORITY XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE` |
| visual-activity | choice{off,on,both} | off |
| visual-bell | choice{off,on,both} | off |
| visual-silence | choice{off,on,both} | off |
| word-separators | str | `` !"#$%&'()*+,-./:;<=>?@[\]^`{|}~ `` |

### Window (and window+pane) options

| Option | Scope | Type | Default |
|---|---|---|---|
| aggressive-resize | Win | flag | off |
| allow-passthrough | W/P | choice{off,on,all} | off |
| allow-rename | W/P | flag | off |
| allow-set-title | W/P | flag | on |
| alternate-screen | W/P | flag | on |
| automatic-rename | Win | flag | on |
| automatic-rename-format | Win | str | `#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}` |
| clock-mode-colour | Win | str[Co] | themeblue |
| clock-mode-style | Win | choice{12,24,12-with-seconds,24-with-seconds} | 24 |
| copy-mode-match-style | Win | str[St] | `bg=themecyan,fg=themeblack` |
| copy-mode-current-match-style | Win | str[St] | `bg=thememagenta,fg=themeblack` |
| copy-mode-mark-style | Win | str[St] | `bg=themeyellow,fg=themeblack` |
| copy-mode-position-format | W/P | str | `#[align=right]#{t/p:top_line_time}…[#{copy_position}/#{copy_position_limit}]…` |
| copy-mode-position-style | Win | str[St] | `#{E:mode-style}` |
| copy-mode-selection-style | Win | str[St] | `#{E:mode-style}` |
| copy-mode-current-line-number-style | Win | str[St] | `fg=themeyellow` |
| copy-mode-line-number-style | Win | str[St] | `fg=themelightgrey,dim` |
| copy-mode-line-numbers | Win | choice{off,default,absolute,relative,hybrid} | off |
| cursor-colour | W/P | str[Co] | "" |
| cursor-style | W/P | choice{default,blinking-block,block,blinking-underline,underline,blinking-bar,bar} | default |
| fill-character | Win | str | "" |
| main-pane-height | Win | str | 24 (may be `N%`) |
| main-pane-width | Win | str | 80 (may be `N%`) |
| menu-style / menu-selected-style / menu-border-style | Win | str[St] | `bg=themedarkgrey,fg=themewhite` / `bg=themeyellow,fg=themeblack` / `bg=themedarkgrey,fg=themelightgrey` |
| menu-border-lines | Win | choice{single,double,heavy,simple,rounded,padded,none} | single |
| mode-keys | Win | choice{emacs,vi} | emacs |
| mode-style | Win | str[St] | `noattr,bg=themeyellow,fg=themeblack` |
| monitor-activity | Win | flag | off |
| monitor-bell | Win | flag | on |
| monitor-silence | Win | num(0–INT_MAX) | 0 |
| other-pane-height / other-pane-width | Win | str | 0 |
| pane-active-border-style | W/P | str[St] | `fg=#{?pane_marked,thememagenta,#{?synchronize-panes,themered,#{?pane_in_mode,themeyellow,themegreen}}}` |
| pane-base-index | Win | num(0–USHRT_MAX) | 0 |
| pane-border-format | W/P | str | `#{?pane_active,#[reverse],}#{pane_index}#[default] "#{pane_title}"…` |
| pane-border-indicators | Win | choice{off,colour,arrows,both} | colour |
| pane-border-lines | W/P | choice{single,double,heavy,simple,number,spaces,none} | single |
| pane-border-status | W/P | choice{off,top,bottom,top-floating,bottom-floating} | off |
| pane-border-style | W/P | str[St] | `fg=themelightgrey` |
| pane-colours | W/P | colour[A] | "" |
| pane-scrollbars | Win | choice{off,modal,on,auto-hide} | off |
| pane-scrollbars-timeout | Win | num(0–INT_MAX) ms | 500 |
| pane-scrollbars-style | W/P | str[St] | `bg=themedarkgrey,fg=themelightgrey,width=1,pad=0` |
| pane-scrollbars-position | Win | choice{right,left} | right |
| pane-status-current-style / pane-status-style | Win | str[St] | underscore / default |
| popup-style / popup-border-style | Win | str[St] | `bg=themedarkgrey,fg=themewhite` / `…,fg=themelightgrey` |
| popup-border-lines | Win | choice{single,double,heavy,simple,rounded,padded,none} | single |
| remain-on-exit | W/P | choice{off,on,failed,key} | off |
| remain-on-exit-format | W/P | str | `Pane is dead (…, #{t:pane_dead_time})` |
| scroll-on-clear | W/P | flag | on |
| session-status-current-style / session-status-style | Win | str[St] | underscore / default |
| switch-mode-match-style | W/P | str[St] | `bg=cyan fg=black` (note: space-separated default!) |
| synchronize-panes | W/P | flag | off |
| tiled-layout-max-columns | Win | num(0–USHRT_MAX) | 0 |
| tree-mode-border-style | Win | str[St] | `bg=themedarkgrey,fg=themelightgrey` |
| tree-mode-preview-format | W/P | str | `#{?pane_format,#{pane_index}:#{pane_title},#{window_index}:#{window_name}}` |
| tree-mode-preview-style | Win | str[St] | conditional themered/themeblue |
| tree-mode-selection-style | Win | str[St] | `#{E:mode-style}` |
| window-active-style | W/P | str[St] | default |
| window-pane-current-status-format / window-pane-status-format | Win | str | `#P:[#T]#{?pane_flags,#{pane_flags}, }` |
| window-size | Win | choice{largest,smallest,manual,latest} | latest |
| window-style | W/P | str[St] | default |
| window-status-activity-style | Win | str[St] | reverse |
| window-status-bell-style | Win | str[St] | reverse |
| window-status-current-format | Win | str | `#I:#W#{?window_flags,#{window_flags}, }` |
| window-status-current-style | Win | str[St] | underscore |
| window-status-format | Win | str | `#I:#W#{?window_flags,#{window_flags}, }` |
| window-status-last-style | Win | str[St] | default |
| window-status-separator | Win | str | ` ` (one space) |
| window-status-style | Win | str[St] | default |
| wrap-search | Win | flag | on |
| xterm-keys | Win | flag | on (no longer used) |

### Hook options (all cmd[A][H], empty default, empty separator)

Session-scope: `after-bind-key after-capture-pane after-copy-mode after-display-message after-display-panes after-kill-pane after-list-buffers after-list-clients after-list-keys after-list-panes after-list-sessions after-list-windows after-load-buffer after-lock-server after-new-session after-new-window after-paste-buffer after-pipe-pane after-queue after-refresh-client after-rename-session after-rename-window after-resize-pane after-resize-window after-save-buffer after-select-layout after-select-pane after-select-window after-send-keys after-set-buffer after-set-environment after-set-hook after-set-option after-show-environment after-show-messages after-show-options after-split-window after-unbind-key alert-activity alert-bell alert-silence client-active client-attached client-detached client-focus-in client-focus-out client-resized client-session-changed client-light-theme client-dark-theme command-error session-closed session-created session-renamed session-window-changed window-linked window-unlinked`. Pane-scope (W/P): `pane-died pane-exited pane-focus-in pane-focus-out pane-mode-changed pane-set-clipboard pane-title-changed`. Window-scope: `window-layout-changed window-pane-changed window-renamed window-resized`.

**Tmux-3.x options users' configs still set that are gone/renamed in master** (for error-message parity decisions): none of the user's failing options are gone — `visual-activity`, `visual-bell`, `visual-silence`, `bell-action`, `status-justify`, `status-right-style` are all present above; they failed in winmux only because winmux's table lacks them.

---

## 11. Windows/winmux applicability notes

1. **`~` expansion** belongs in the config/command tokenizer (§2.6), at a token/quote boundary, not inside single quotes. On Windows resolve bare `~` to the `HOME` entry of the server's global environment first, falling back to `%USERPROFILE%`; `~user` can reasonably fail (no getpwnam) — tmux errors the token when it can't resolve, so "user name is too long"/unresolvable-`~` as a parse error is faithful.
2. **Default config chain** for parity: `%USERPROFILE%\.tmux.conf` then `$XDG_CONFIG_HOME/tmux/tmux.conf` then `~/.config/tmux/tmux.conf` — all that exist load *in order* (not first-match). Silently skip missing files unless the path came from `-f`.
3. **A parse error must abort the entire file** (nothing from it runs) with a single `file:line: message` cause; an execution error must abort only the rest of that line's group and continue, collecting `file:line: message` causes. Causes surface on first attach (tmux uses a read-only view mode in the active pane; a winmux equivalent could be its copy-mode view or a message list).
4. **`setw`/`showw`** are separate command entries (`set-window-option`, `show-window-options`) whose behavior is `set`/`show` with the window flag pre-applied; for *named* options `set` alone already reaches window options because the table decides scope — implement scope-from-table, then `setw` is nearly free.
5. **`-q` on set/show** must swallow: unknown/invalid option, ambiguous option, missing target scope, `-o` already-set, and show-of-unset-@ — and nothing else.
6. **`@`-options**: accept at every scope, string-only, no table validation; `show -gqv "@foo"` unset ⇒ print nothing, exit 0.
7. **Style parsing**: split on space *and* comma (and newline), case-insensitive terms, `bold`≡`bright`, revert-on-error semantics, and don't validate at set time if the value contains `#{`.
8. **Key tables**: `bind -T anything` creates the table; ship default `copy-mode` and `copy-mode-vi` tables as *bindings in real tables* so `unbind -T copy-mode-vi X` and user rebinds work; `unbind` on a truly nonexistent table errors `table %s doesn't exist` unless `-q`.
9. **run-shell/if-shell shell**: tmux uses `default-shell -c 'cmd'`. On Windows the faithful analogue is honouring `default-shell` (default = the pane shell, e.g. `cmd.exe /c` or PowerShell `-Command`) — but note scripts in the wild assume POSIX `sh` semantics for exit codes; `if-shell -F` (format-only, no shell) works identically everywhere and should be preferred/implemented first. Exit-code truth: 0 = true. `if -F` truth: nonempty and not starting `0` (`cmd-if-shell.c:88`); `#{?}`/`%if` truth: nonempty and not exactly `"0"`.
10. **escape-time default is now 10ms** in master (`options-table.c:375`); winmux's 500ms matches tmux ≤3.4. Pick one and document; configs commonly `set -sg escape-time 0/10`. Also note `escape-time` is a **server** option (`-s`), and tmux accepts `set -g escape-time` anyway because the table wins — winmux must not error on the "wrong" scope flag.
11. **`%%` vs `%1`** substitution and its `%%%` quoted variant (§2.9) are needed for the default `,`/`$`/`'` prompt bindings to be faithful.
12. **glob for source-file**: on Windows implement `*?[]` matching manually or via a crate; no-match without `-q` = error `No such file or directory: <original arg>`; remember args are already tilde/env-expanded by the parser.
13. **Case sensitivity**: key names, style terms, colour names, and modifier prefixes are case-insensitive; command names, option names, and format variable names are case-sensitive.
