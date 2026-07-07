# Sub-project 3 — Command layer + `.tmux.conf` — Locked Interface Contract

**Status:** Locked, extended task-by-task. Every implementation task MUST
conform to these types and signatures exactly. If a signature must change
during implementation, the change must be applied consistently to every
consumer named here (same rule as the MVP and SP2 contracts).

**Parent spec:** [`2026-07-07-command-config-design.md`](2026-07-07-command-config-design.md)

## `keys` — tmux key notation, VT encoding, incremental input decoder (Task 1)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KeyCode {
    Char(char), F(u8),
    Up, Down, Left, Right,
    Home, End, PPage, NPage, IC, DC,
    Enter, Escape, Space, Tab, BSpace, BTab,
}

pub fn parse_key(s: &str) -> Option<Key>;
pub fn key_name(k: &Key) -> String;
pub fn encode_key(k: &Key) -> Option<Vec<u8>>;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecodedKey { pub key: Key, pub raw: Vec<u8> }

pub struct KeyDecoder; // opaque; owns only an internal pending-byte buffer
impl KeyDecoder {
    pub fn new() -> KeyDecoder;
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<DecodedKey>;
    pub fn flush(&mut self) -> Vec<DecodedKey>;
}
```

Pure module: no I/O, no Windows APIs, `std` only. `Key`/`KeyCode` derive
`Clone, Copy, PartialEq, Eq, Hash, Debug` — `Key` is usable as a `HashMap`
key for the future bindings table (`Bindings { root: Map<Key, Binding>,
prefix: Map<Key, Binding> }`).

**Implementation module:** `src/keys.rs`. Depends on nothing but `std`.
Unit-tested with exact expected `Key`/byte-vector values (mirrors
`render.rs`'s exact-value test style): `parse_ctrl_letter`, `parse_meta`,
`parse_shift_fkey`, `parse_combined`, `parse_named`, `parse_plain_chars`,
`parse_invalid_none`, `key_name_roundtrip`, `encode_ctrl`, `encode_meta`,
`encode_named`, `encode_unsupported_combo_is_none`,
`encode_roundtrip_via_decoder`, `decode_plain_bytes`, `decode_ctrl_bytes`,
`decode_prefix_byte_is_ctrl_b`, `decode_csi_arrows`, `decode_csi_modified`,
`decode_ss3_fkeys`, `decode_csi_tilde_keys`, `decode_meta_char`,
`decode_split_sequence_across_feeds`, `decode_lone_escape_flush`,
`decode_utf8_multibyte_char` (24 tests total).

### Notation rules (`parse_key` / `key_name`)

- Modifier prefixes `C-`/`M-`/`S-` are combinable, in any order, each
  detected as a leading two-byte `<letter>-` pair and stripped iteratively
  (`"C-M-Left"` strips `C-` then `M-`, leaving `"Left"`). A prefix letter not
  immediately followed by `-` is not treated as a modifier (so a bare
  single-char token like `"C"` parses as the literal char `'C'`, not an
  empty ctrl-modified token).
- After prefix-stripping, the remaining token is matched **case-insensitively**
  against the named-key table below; on no match, it must be **exactly one
  Unicode scalar value** to parse as a literal `KeyCode::Char` (case-sensitive:
  `"V"` and `"v"` are distinct, unrelated chars — parsing never adds an
  implicit `shift` flag for a bare uppercase char). Anything else (empty
  string, multi-char non-named token, prefix with nothing after it) is
  `None`.
- **tmux ctrl-letter normalization:** when `ctrl` is set and the base token
  is a single ASCII letter, the letter is lowercased before building the
  `Key` — `parse_key("C-A") == parse_key("C-a")`. This normalization does
  NOT apply to non-letter chars (`C-%` keeps `%` as-is) or to named keys.
- Named-key table (case-insensitive on parse; canonical capitalization is
  what `key_name` emits): `Enter`, `Escape`, `Space`, `Tab`, `BSpace`,
  `BTab`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PPage`, `NPage`,
  `IC`, `DC`, and `F<n>` (`F1`.. any `u8`; `encode_key` only knows how to
  encode `F1`-`F12`, but `parse_key`/`key_name` accept any `F<n>`).
- `key_name` renders modifier prefixes in the fixed order `C-` then `M-`
  then `S-`, followed by the canonical body (named-key spelling above, or
  the literal char, or `F<n>`). Round-trips through `parse_key` for every
  `Key` value `parse_key`/`KeyDecoder` can themselves produce.

### `encode_key` (output direction: bytes to send to a pane)

| key | bytes |
|---|---|
| `Enter` | `\r` (0x0d) |
| `Tab` | `\t` (0x09) |
| `BSpace` | `0x7f` |
| `Escape` | `0x1b` |
| `Space` | `0x20` |
| `Char(c)` | `c`'s UTF-8 encoding |
| `Up`/`Down`/`Right`/`Left` (no mods) | `CSI A`/`B`/`C`/`D` |
| `Up`/`Down`/`Right`/`Left` (any of ctrl/meta/shift) | `CSI 1;<n><letter>`, `n = 1 + shift*1 + meta*2 + ctrl*4` |
| `Home` / `End` | `CSI H` / `CSI F` |
| `PPage` / `NPage` | `CSI 5~` / `CSI 6~` |
| `IC` / `DC` | `CSI 2~` / `CSI 3~` |
| `BTab` | `CSI Z` |
| `F1`-`F4` | SS3 `ESC O P`/`Q`/`R`/`S` |
| `F5`-`F12` | `CSI 15~ 17~ 18~ 19~ 20~ 21~ 23~ 24~` respectively |
| ctrl + ASCII-letter `Char` | `0x01`-`0x1a` (lowercased letter, `a`=1) |
| ctrl + `Space` (or `Char(' ')`) | `0x00` |
| meta + anything encodable | `ESC` prepended to that base encoding (recursion with `meta` cleared) |

Anything else — ctrl on a named key other than the arrow CSI-modifier form,
ctrl on a non-letter/non-space char, shift alone on a non-arrow named key,
or `F13`+ — returns `None` (documented simplification; arrows are the only
named keys with full CSI-modifier support in SP3).

### `KeyDecoder` (input direction: client keystroke bytes to `Key`)

Incremental: `feed()` pushes one byte at a time into an internal buffer and
attempts to classify the buffer as a complete token after every byte;
whatever completes is emitted (in order) as a `DecodedKey` with `raw` set
to the exact consumed bytes, and the buffer is cleared for the next token.
An in-progress escape sequence (or a partial UTF-8 multibyte sequence)
produces nothing until it completes — safe to split arbitrarily across
`feed()` calls.

**Bounded buffering** (fix pass, Task 1): the pending buffer is capped at
`MAX_PENDING` (32, private) bytes. A sequence that grows past the cap
without completing (e.g. a "CSI" that never receives a final byte) has its
head byte peeled off as a best-effort key (lone `ESC` -> `Escape`; anything
else -> its ordinary single-byte decoding) and the remainder reprocessed as
fresh tokens, in order, ahead of any not-yet-consumed input. A misbehaving
byte stream can therefore never stall the decoder or grow the buffer
unboundedly; the bound is also what keeps the CSI scan finite. Regression
test: `decode_runaway_csi_is_bounded`.

**`flush()` semantics** (fix pass, Task 1): end-of-input drain, equivalent
to an incremental re-feed of the leftover bytes — the SHORTEST classifiable
prefix of the buffer is emitted as a key whose `raw` is exactly its own
bytes; when the head token cannot complete (no more bytes are coming), the
head byte alone is peeled with the same best-effort rule as above, and
draining continues on the rest. **Raw-concatenation invariant:** the
concatenation of every emitted key's `raw` — across all `feed()` and
`flush()` calls — is exactly the input byte stream; no byte is ever dropped
or absorbed into a neighboring key's `raw`. (The original Task 1 flush
classified the whole remaining buffer as one token and stuffed ALL leftover
bytes into that single key's `raw` — a truncated `ESC [ 1 ; 5` flushed as
`Escape` + `Char('[')` with raw `[1;5`, mis-attributing three bytes.)
Regression tests: `flush_truncated_csi_peels_all_bytes` (that exact buffer
now flushes to five keys: `Escape`, `[`, `1`, `;`, `5`),
`flush_preserves_raw_concatenation` (property-style: raw concatenation ==
input over several truncated/hostile buffers).

Decoding table:

| input bytes | decodes to |
|---|---|
| `0x00` | ctrl + `Space` |
| `0x09` | `Tab` |
| `0x0d` | `Enter` |
| `0x7f` | `BSpace` |
| `0x01`-`0x1a` (excluding the three above) | ctrl + `Char(<letter>)`, `0x01`=`a` .. `0x1a`=`z` (e.g. `0x02` = `C-b`, the prefix byte) |
| `0x20`-`0x7e` | `Char(<that ASCII char>)` |
| UTF-8 multibyte lead (`0xc0`-`0xf7`) + continuation bytes | `Char(<decoded char>)` (buffered across `feed()` calls until the full sequence is present; invalid UTF-8 decodes as `U+FFFD`) |
| any other single byte (`0x1c`-`0x1f`, stray continuation bytes, `0xf8`-`0xff`) | `Char(<byte value as a Latin-1 code point>)` (fallback, not tmux notation, never produced by `parse_key`/`encode_key`) |
| `ESC` alone, no more bytes this call | buffered (nothing emitted); `flush()` resolves it to `Escape` |
| `ESC <token>` (first remainder byte is not `[` or `O`) | Meta + whatever the WHOLE remainder decodes to (fix pass, Task 1 — tmux applies Meta to the decoded key): `ESC x` → `M-x`; `ESC ESC [ A` → `M-Up` (one key, raw = all four bytes); `ESC` + UTF-8 char → `M-<char>`; nesting stacks onto the same `meta` flag (`ESC ESC [ 1;5 A` → `C-M-Up`). An incomplete remainder keeps buffering across `feed()` calls — it may complete later, or get peeled by `flush()`/the `MAX_PENDING` bound. (The original Task 1 code classified only the FIRST remainder byte, which returned "incomplete" forever when that byte was itself a multi-byte introducer — `ESC ESC [ A`, i.e. Escape-then-arrow, permanently stalled all input.) Regression tests: `decode_esc_then_arrow_is_meta_up`, `decode_esc_then_split_arrow_across_feeds`, `decode_esc_then_utf8`. |
| `ESC [ <params> <final>` (CSI) | see below |
| `ESC O P`/`Q`/`R`/`S` (SS3) | `F1`/`F2`/`F3`/`F4` |
| `ESC O <other byte>` (SS3, unrecognized third byte) | `Char(<that byte>)` (best-effort fallback, mirrors the unrecognized-CSI-final rule below; never blocks) |

CSI parsing buffers `ESC [` plus every subsequent byte until one lands in
`0x40`-`0x7e` (the final byte); everything between `[` and the final byte is
the parameter string, split on `;` into integers. Recognized forms:

| final byte | params | key |
|---|---|---|
| `A`/`B`/`C`/`D` | none, or `1;<mod>` | `Up`/`Down`/`Right`/`Left`, mods from `<mod>` below |
| `H` / `F` | none, or `1;<mod>` | `Home` / `End` |
| `Z` | none, or `1;<mod>` | `BTab` |
| `<n>~` | none, or `<n>;<mod>` | `n`=2→`IC`, 3→`DC`, 5→`PPage`, 6→`NPage`, 1 or 4→`Home`/`End`, 15/17/18/19/20/21/23/24→`F5`..`F12` |

`<mod>` decodes as `bits = mod_value - 1`; `shift = bits & 1`, `meta = bits
& 2`, `ctrl = bits & 4` (so `1;5A` = `C-Up`, `1;2A` = `S-Up`, `1;3A` =
`M-Up` — matches the bitmask `encode_key` builds for the same forms). An
unrecognized final byte, or a `~`-form with an unrecognized number, decodes
as `Char(<final byte>)` (best-effort fallback, not real tmux notation) once
the CSI sequence is structurally complete — decoding never blocks forever on
an unrecognized-but-well-formed sequence.

**Not implemented in SP3** (ticketed, matches the design spec's stated
deviations): no escape-time disambiguation timer — a lone `ESC` not
followed by anything in the same `feed()` call is simply left buffered
until the next `feed()` or a `flush()`; there is no way to distinguish "the
user pressed Escape" from "an escape sequence is still arriving" without
one. `encode_key`/`KeyDecoder` only cover the arrow CSI-modifier form for
combining modifiers with a named key — other named keys (Home/End/PPage/
NPage/IC/DC/F-keys) do not support ctrl/meta/shift combinations at all in
SP3, on either the encode or decode side, beyond what's in the tables above.

## `style` — tmux style-string grammar onto `grid::Style` (Task 2)

```rust
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PartialStyle { /* opaque: fg, bg, and per-attribute set/clear
                              state, all private */ }

impl PartialStyle {
    pub fn apply_to(&self, base: grid::Style) -> grid::Style;
    pub fn merge(&self, over: &PartialStyle) -> PartialStyle;
}

pub fn parse_style(input: &str) -> Result<PartialStyle, String>; // Err = "bad style: <input>"
```

Pure module: no I/O, `std` only. **Implementation module:** `src/style.rs`.
Depends only on `crate::grid::{Color, Style}`. `PartialStyle` is deliberately
opaque per the brief (no public fields) — every field is `Option<T>`
internally: `None` means "this style string didn't mention this", so
`apply_to`/`merge` can layer explicit overrides onto a base without
clobbering anything unmentioned.

Unit-tested with exact expected values (mirrors `keys.rs`'s style):
`named_colors`, `colour_indexed`, `hex_rgb`, `default_color_resets`,
`attrs_set`, `attr_synonyms`, `attrs_clear`, `accepted_ignored`,
`apply_layers_over_base`, `merge_precedence`, `bad_style_err_string`,
`empty_string_ok_noop`, `color_names_case_insensitive` (13 tests).

### Grammar (`parse_style`)

- Input is trimmed of surrounding whitespace first; empty after trim → `Ok`
  with a no-op `PartialStyle` (nothing mentioned, `apply_to` is identity).
  Otherwise split on `,` (components themselves are NOT individually
  trimmed — tmux has no internal whitespace tolerance); any empty component
  (leading/trailing/doubled comma) is a parse failure.
- Matching is **case-insensitive** throughout (fix pass, Task 2): each
  component is ASCII-lowercased before matching, so `FG=Red`, `BOLD`,
  `NONE`, `Bg=BrightRed` are all valid — tmux's `style_parse` and
  `colour_fromstring` are `strcasecmp`-based. The error string is built
  from the ORIGINAL input, so it echoes the user's own casing.
- `fg=<color>` / `bg=<color>` set the respective field; see the color
  grammar below.
- `none` / `noattr` reset all FIVE attribute fields (bold/dim/italic/
  underline/reverse) back to "unmentioned" for this style — this does
  **NOT** touch `fg`/`bg`, which are left as already parsed by earlier
  components in the same string (tmux behavior: `none` resets attributes
  only, not colors).
- Attribute set/clear pairs: `bold`/`nobold`, `dim`/`nodim`,
  `reverse`/`noreverse`; `italics`|`italic` / `noitalics`|`noitalic`
  (tmux's canonical word is `italics`, `italic` accepted as a synonym);
  `underscore`|`underline` / `nounderscore`|`nounderline` (tmux's canonical
  word is `underscore`, `underline` accepted as a synonym). Each maps onto
  `grid::Style`'s field of the same name (`italic`, `underline` — note NOT
  `italics`/`underscore`, matching `grid.rs`'s field names).
- Accepted-but-inert (parse OK, no-op — no corresponding `grid::Style`
  field): `blink`/`noblink`, `strikethrough`/`nostrikethrough`,
  `double-underscore`/`nodouble-underscore`,
  `curly-underscore`/`nocurly-underscore`,
  `dotted-underscore`/`nodotted-underscore`,
  `dashed-underscore`/`nodashed-underscore` (negated forms added in the
  Task 2 fix pass — a valid tmux config line using one must not abort the
  style parse).
- Anything else (unknown word, malformed `fg=`/`bg=` color) is a component
  failure; the whole call fails with `Err(format!("bad style: {input}"))` —
  `input` is the exact original (untrimmed) argument, not the trimmed or
  partially-parsed form.

### Color grammar (`fg=`/`bg=` value)

| form | maps to |
|---|---|
| `default` | `Color::Default` |
| `black` `red` `green` `yellow` `blue` `magenta` `cyan` `white` | `Color::Idx(0..=7)` |
| `bright<name>` (same 8 names) | `Color::Idx(8..=15)` (`brightred` → `Idx(9)`) |
| `colour<n>` / `color<n>`, `n` in `0..=255` | `Color::Idx(n)` (`colour256`+ → `Err`, out of `u8` range) |
| `#rrggbb` (exactly 6 hex digits, case-insensitive) | `Color::Rgb(r, g, b)` (wrong length or non-hex digit → `Err`) |

Every color form is case-insensitive (`fg=RED`, `fg=Colour208`,
`fg=DEFAULT`), like the rest of the grammar — the whole component is
lowercased before matching. (The original Task 2 code matched named
colors/`colour`/`default` case-sensitively; fixed in the Task 2 fix pass
to match tmux's `strcasecmp` behavior.)

### `apply_to` / `merge` semantics

- `apply_to(base)`: for each field, `Some(v)` (color explicitly set, or
  attribute explicitly set/cleared) overwrites `base`'s corresponding
  field; `None` (never mentioned) leaves `base`'s value untouched. An
  explicit `no<attr>` is `Some(false)` — just as "explicit" as setting the
  attribute — so it forces the base's flag off rather than leaving it
  alone.
- `merge(&self, over)`: per-field, `over`'s value wins if `Some`; otherwise
  falls back to `self`'s own value (which may itself be `None`). Used to
  layer e.g. `window-status-current-style` (`over`) on top of `status-style`
  (`self`) before a single `apply_to` call against the render base style.

## `cmd` — tmux command tokenizer, table, and typed commands (Task 3)

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawCmd { pub name: String, pub args: Vec<String> }

pub fn parse_line(line: &str) -> Result<Vec<RawCmd>, String>;
pub fn join_continuations<'a, I: Iterator<Item = &'a str>>(lines: I) -> Vec<(usize, String)>;

#[derive(Clone, Debug, PartialEq)]
pub enum ParsedCmd {
    SplitWindow { horizontal: bool, target: Option<String> },
    SelectPane { dir: Option<crate::geom::Direction>, target: Option<String> },
    SelectWindow { target: String },
    NextWindow, PreviousWindow, LastWindow, LastPane,
    NewWindow { name: Option<String> },
    KillPane { target: Option<String> },
    KillWindow { target: Option<String> },
    ResizePane { dir: Option<crate::geom::Direction>, zoom: bool, count: i32 },
    RenameWindow { target: Option<String>, name: String },
    RenameSession { target: Option<String>, name: String },
    DetachClient { target: String },
    SendKeys { literal: bool, target: Option<String>, keys: Vec<String> },
    SendPrefix,
    DisplayMessage { text: Option<String> },
    ConfirmBefore { prompt: Option<String>, tail: Vec<RawCmd> },
    CommandPrompt { initial: Option<String> },
    SetOption { global: bool, window: bool, append: bool, unset: bool, name: String, value: Option<String> },
    ShowOptions { global: bool, name: Option<String> },
    BindKey { table: String, repeat: bool, key: String, tail: Vec<RawCmd> },
    UnbindKey { all: bool, table: String, key: Option<String> },
    ListKeys,
    SourceFile { path: String },
    // SP2 CLI commands, folded into the same table.
    NewSession { detached: bool, name: Option<String>, cols: Option<u16>, rows: Option<u16> },
    AttachSession { target: Option<String>, detach_others: bool },
    ListSessions,
    ListWindows { target: Option<String> },
    HasSession { target: String },
    KillSession { target: Option<String> },
    KillServer,
}

pub fn resolve(raw: &RawCmd) -> Result<ParsedCmd, String>;
pub fn usage(name: &str) -> Option<&'static str>;
```

Pure module: no I/O, `std` only. **Implementation module:** `src/cmd.rs`.
Depends only on `crate::geom::Direction`. `bind-key`/`confirm-before` are
"store-don't-resolve": their bound/wrapped command tail is kept as
unresolved `Vec<RawCmd>` — tmux does late binding too, so the tail is
re-`resolve()`d against the table at execution time (Task 6), not here.

Unit-tested with exact expected values (mirrors `keys.rs`/`style.rs`'s
style): tokenizer — `plain_split`, `single_quotes_literal`,
`double_quotes_escapes`, `double_quotes_other_backslash_is_literal`,
`comment_strips`, `comment_strips_mid_token`, `semicolon_splits_commands`,
`escaped_semicolon_is_arg`, `quoted_semicolon_also_splits_tail`,
`unterminated_quote_err`, `blank_and_comment_only_lines_are_empty`,
`adjacent_quote_concatenation`; `join_continuations` —
`join_continuations_passthrough`, `join_continuations_basic`,
`join_continuations_chain_tracks_first_line_number`,
`join_continuations_trailing_backslash_at_eof_kept`,
`join_continuations_strips_crlf`; `resolve`/`usage` — `aliases_resolve`,
`split_window_flags`, `send_keys_literal_flag`, `send_keys_requires_a_key`,
`set_option_flags`, `bind_key_full`, `bind_key_dash_n_is_root_table`,
`bind_key_bad_table_errs`, `unbind_key_table_validation`,
`confirm_before_tail`, `resize_pane_defaults_and_errors`,
`unknown_command_err_exact`, `usage_err_on_bad_flag`, `sp2_commands_present`,
`sp2_usage_strings_match_cli_exec_verbatim`,
`usage_lookup_by_alias_and_unknown`, `rename_window_requires_name`,
`show_options_and_display_message` (35 tests total).

### Tokenizer (`parse_line`)

- Whitespace (space/tab, and stray `\r`/`\n`) splits tokens, discarded
  otherwise.
- `'...'`: fully literal — no escapes recognized inside, including `#` and
  `;` (both lose all special meaning between the quotes).
- `"..."`: recognizes exactly two escapes, `\"` -> `"` and `\\` -> `\`. Any
  other `\<c>` passes through as BOTH characters verbatim — the backslash is
  not a general escape inside double quotes either.
- A quoted segment concatenates with adjacent bare characters or another
  quoted segment with no intervening whitespace, shell-style:
  `foo'bar'"baz"` is one token `foobarbaz`.
- `#` outside any quote starts a comment running to end of line, even
  mid-token (`foo#bar` tokenizes `foo`, discards `#bar`).
- An unquoted, unescaped `;` is a command separator: ends the in-progress
  token and the current command, starts a new one; it is never itself part
  of a token, and consecutive/leading/trailing `;` never produce empty
  `RawCmd`s (silently dropped).
- `\;` outside quotes: the backslash is consumed and a literal `;` is
  appended to the token being built. This does **not** split the command at
  `parse_line`'s level — only `resolve()`'s tail-splitting for
  `bind-key`/`confirm-before` (see below) gives a lone `;` token special
  meaning.
- Outside quotes, a bare `\` not followed by `;` is literal (no other
  top-level escapes).
- `Err("unterminated quote")` if a `'`/`"` is never closed before the line
  ends.

**Design resolution — `\;` vs quoted `";"`:** both an escaped `\;` and an
equivalently-lone quoted token (`'";"'` or `"';'"`) produce the exact same
`String` content `";"` by the time tokenization is done — `parse_line` has
no way to (and does not) distinguish "this token happened to equal one
semicolon character" by origin. `resolve()`'s tail-splitting for
`bind-key`/`confirm-before` therefore splits on ANY tail token that is
exactly `";"`, regardless of whether it arrived via `\;` or a quoted form.
This matches tmux's real behavior for this case and is deliberately
documented rather than distinguished, per the task's design-decision notes.

### `join_continuations`

A physical line whose last character is `\` (after stripping one optional
trailing `\r`, so CRLF files work) is joined directly with the next
physical line — backslash removed, no separator inserted — repeating across
a chain. Returns `(first_line_number, joined_text)` pairs, 1-based. A
trailing `\` on the very last physical line has nothing to continue onto, so
the backslash is put back rather than silently dropped
(`join_continuations_trailing_backslash_at_eof_kept`).

### Command table: names, aliases, usage lines

`resolve()`/`usage()` accept either the full name or any alias below.
Usage-error messages are exactly `usage(name).unwrap()` (no extra
formatting) — the nine marked **verbatim** are copied byte-for-byte from
`src/server/cli_exec.rs`'s `USAGE_*` constants (SP2 test-parity
requirement); the rest are new tmux-style lines authored for SP3.

| command | alias(es) | usage string |
|---|---|---|
| `split-window` | `splitw` | `usage: split-window [-h] [-v] [-t target]` |
| `select-pane` | `selectp` | `usage: select-pane [-L] [-R] [-U] [-D] [-t target]` |
| `select-window` | `selectw` | `usage: select-window -t target` |
| `next-window` | `next` | `usage: next-window` |
| `previous-window` | `prev` | `usage: previous-window` |
| `last-window` | `last` | `usage: last-window` |
| `last-pane` | `lastp` | `usage: last-pane` |
| `new-window` | `neww` | `usage: new-window [-n name]` |
| `kill-pane` | `killp` | `usage: kill-pane [-t target]` |
| `kill-window` | `killw` | `usage: kill-window [-t target]` |
| `resize-pane` | `resizep` | `usage: resize-pane [-L] [-R] [-U] [-D] [-Z] [count]` |
| `rename-window` | `renamew` | `usage: rename-window [-t target] new-name` **(verbatim)** |
| `rename-session` | `rename` | `usage: rename-session [-t target] new-name` **(verbatim)** |
| `detach-client` | — | `usage: detach-client -s target` **(verbatim — note: `-s` is REQUIRED, not optional; SP2's actual `cli_exec.rs` behavior overrides the design doc's `[-s target]` bracket notation, per the task brief's verbatim-copy instruction)** |
| `send-keys` | `send` | `usage: send-keys [-l] [-t target] key ...` |
| `send-prefix` | — | `usage: send-prefix` |
| `display-message` | `display` | `usage: display-message [text]` |
| `confirm-before` | `confirm` | `usage: confirm-before [-p prompt] command ...` |
| `command-prompt` | — | `usage: command-prompt [-I initial]` |
| `set-option` | `set` | `usage: set-option [-g] [-w] [-a] [-u] option [value]` |
| `show-options` | `show` | `usage: show-options [-g] [option]` |
| `bind-key` | `bind` | `usage: bind-key [-n] [-r] [-T table] key command ...` |
| `unbind-key` | `unbind` | `usage: unbind-key [-a] [-n] [-T table] [key]` |
| `list-keys` | `lsk` | `usage: list-keys` |
| `source-file` | `source` | `usage: source-file path` |
| `new-session` | `new` | `usage: new-session [-d] [-s name] [-x cols] [-y rows]` **(verbatim)** |
| `attach-session` | `attach`, `a` | `usage: attach-session [-d] [-t target]` |
| `list-sessions` | `ls` | `usage: list-sessions` **(verbatim)** |
| `list-windows` | `lsw` | `usage: list-windows [-t target]` **(verbatim)** |
| `has-session` | `has` | `usage: has-session -t target` **(verbatim)** |
| `kill-session` | — | `usage: kill-session [-t target]` **(verbatim)** |
| `kill-server` | — | `usage: kill-server` **(verbatim)** |

### Flag/argument conventions

- Direction flags (`select-pane`/`resize-pane` `-L`/`-R`/`-U`/`-D`) map onto
  the existing `geom::Direction`; if more than one is given, a fixed priority
  `-L > -R > -U > -D` picks the winner — no error (documented simplification;
  no realistic config passes two direction flags to one command).
- `set-option`'s `value`: remaining tokens after `-g`/`-w`/`-a`/`-u` and the
  option name are joined with a single space each — a single token (e.g. one
  quoted string) is returned verbatim (join of one element is a no-op);
  multiple bare tokens are space-joined. Zero remaining tokens -> `value:
  None` (flags-only options like `set -g mouse`; on/off semantics are the
  `options` module's job, not this one's).
- `bind-key`/`unbind-key` `-T table`: SP3 only recognizes `root` and
  `prefix` (tmux itself allows arbitrary table names; broader table support
  is out of SP3 scope). Any other value is
  `Err(format!("unknown key table: {t}"))` — NOT the generic `usage:` error.
  `-n` is sugar for `-T root`. Default table (no `-n`/`-T` given) is
  `"prefix"`, matching tmux's default key table for both commands.
  `bind-key`'s key stores as a plain `String` (NOT parsed via
  `keys::parse_key` here) — the server resolves it through the `keys` module
  at execution/dispatch time, decoupling `cmd` from `keys`.
  `unbind-key`'s `key` is required unless `-a` is given.
  `resize-pane`'s optional trailing `count` defaults to `1`; a non-numeric
  value is a `usage:` error (not a separate "bad value" message).
- `bind-key`/`confirm-before` tail-splitting: after consuming the command's
  own flags (and, for `bind-key`, the key token), ALL remaining tokens are
  the tail; the tail is split into one `RawCmd` per run of tokens separated
  by an exact `";"` token (see the tokenizer section above). Both commands
  require a non-empty tail (`Err(usage)` if none).
- Commands with no positional/flag arguments at all (`next-window`,
  `previous-window`, `last-window`, `last-pane`, `send-prefix`, `list-keys`,
  `list-sessions`, `kill-server`) reject ANY leftover token (flag or
  positional) with the `usage:` error — a deliberate strictness beyond
  SP2's `cli_exec.rs`, which silently ignores stray positionals on some of
  these; since `cmd.rs` is a new parsing layer (not yet wired into
  `cli_exec.rs`'s execution), this does not break any existing SP2 test.
- `rename-window`/`rename-session` require EXACTLY one positional (the new
  name) after flags — zero or more-than-one is a `usage:` error (SP2's
  `cli_exec.rs` takes only the first positional and silently ignores extras;
  this module is stricter, same rationale as above).

### Command table implementation note

`resolve()` maps every alias to a canonical name via a private `canonical()`
match, then dispatches on the canonical name; `usage()` shares the same
`canonical()` lookup, so alias and full-name lookups always agree
(`usage_lookup_by_alias_and_unknown`). Unknown name -> `Err(format!("unknown
command: {name}"))` from `resolve()`, `None` from `usage()`.
