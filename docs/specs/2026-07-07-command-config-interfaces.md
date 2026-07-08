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
    /// Sub-project 4, Task 9 (escape-time). Pure query, no clock (see
    /// "Not implemented in SP3" below, now superseded by the `## input-v2`
    /// section's Task 9 amendment for the actual timing decision).
    pub fn pending_starts_with_escape(&self) -> bool;
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
`decode_utf8_multibyte_char` (24 tests total; sub-project 4 adds mouse-
decoding tests documented in the `## mouse` section of
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md),
plus Task 9's `pending_starts_with_escape_tracks_buffer_state`).

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

**Amendment (sub-project 4, Task 5 — mouse):** `KeyDecoder::feed`/`flush` now
return `Vec<DecodedInput>` instead of `Vec<DecodedKey>` —
`DecodedInput::{Key(DecodedKey), Mouse { event: keys::MouseEvent, raw:
Vec<u8> }}` — so the same incremental byte stream can carry decoded SGR mouse
events (`CSI < Cb ; Cx ; Cy M|m`) alongside decoded keys, in the arrival
order both were seen. This is the minimal-churn option this section's
original text called out ("`DecodedKey` generalizes to
`DecodedInput::{Key(DecodedKey), Mouse{event, raw}}`... pick minimal churn,
contract it"). Every table/example below that still says "`DecodedKey`" is
unchanged in its own right — a plain key decodes exactly as documented, just
wrapped in `DecodedInput::Key(..)` — this amendment only adds the sibling
`Mouse` variant and a new SGR-recognition branch in `classify_csi` (checked
BEFORE the generic CSI final-byte scan, since a mouse sequence's `<`
introducer byte is not itself a valid CSI final byte). Full mouse decoding
contract (the `MouseEvent`/`MouseKind` shapes, the SGR wire-format decode
table, and the RAW-BYTE FIDELITY "always consume when decodable, regardless
of the `mouse` option" decision) lives in the `## mouse` section of
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md),
alongside the rest of Task 5's cross-module surface, rather than duplicated
here.

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

**Amendment (sub-project 4, Task 9 — escape-time):** `KeyDecoder` gains
`pending_starts_with_escape(&self) -> bool`, a pure query (no clock — this
module stays clock-free per its own docs) reporting whether the current
pending buffer's first byte is `ESC` (0x1b): an outstanding escape-
introduced sequence, including a bare trailing `ESC` with nothing after it
yet, that `feed()` hasn't been able to complete. This is the primitive the
CALLER (`input::KeyMachine`, which already threads an `Instant` through
`feed()`) builds its own escape-time timer on top of — see the `## input-v2`
section's matching Task 9 amendment for `flush_now`/`escape_ready`. What was
previously "not implemented in SP3" (below) is now implemented, one layer
up: the decoder itself is still clock-free; `KeyMachine` is what decides
"has enough time passed to force-flush this."

**Historical note (was "Not implemented in SP3", now resolved by the Task 9
amendment above):** SP3 shipped with no escape-time disambiguation timer — a
lone `ESC` not followed by anything in the same `feed()` call was simply
left buffered until the next `feed()` or a `flush()`; there was no way to
distinguish "the user pressed Escape" from "an escape sequence is still
arriving" without one. `encode_key`/`KeyDecoder` only cover the arrow
CSI-modifier form for combining modifiers with a named key — other named
keys (Home/End/PPage/NPage/IC/DC/F-keys) do not support ctrl/meta/shift
combinations at all, on either the encode or decode side, beyond what's in
the tables above (still true; unrelated to escape-time).

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

**Amendment (SP4 Task 8 — overlays):** the private `parse_color(s: &str) ->
Result<grid::Color, ()>` helper `parse_style` already used internally is now
`pub(crate)`, so `options.rs` can parse `display-panes-colour`/`-active-
colour` (plain bare colours, not full `fg=...`/`bg=...` style strings)
directly against the SAME colour grammar without going through a whole style
string. `parse_style` itself is unchanged; this only widens `parse_color`'s
visibility (still not `pub` — no consumer outside this crate). Case-folding
is still the CALLER's job (`parse_style` lowercases before calling it;
`options.rs`'s two getters do the same).

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
    DetachClient { target: Option<String> },
    SendKeys { literal: bool, target: Option<String>, keys: Vec<String> },
    SendPrefix,
    SwitchClient { next: bool },
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
`show_options_and_display_message`, `switch_client_prev_and_next`,
`switch_client_requires_exactly_one_of_p_n`,
`switch_client_dash_l_is_usage_error`,
`detach_client_bare_is_current_client` (Task 5 cross-module additions: 39
tests total).

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
| `detach-client` | — | `usage: detach-client -s target` **(verbatim usage TEXT — but as of Task 5, `-s` is optional at the `resolve()` level: `DetachClient { target: Option<String> }`. tmux's bare `detach-client` detaches the CURRENT client — the default `prefix-d` binding depends on this. Dispatch rule (Task 6): client context + no `-s` = detach the acting client; CLI/config context (no attached client) + no `-s` = the DISPATCHER emits this verbatim SP2 usage error, not `resolve()`. SP2's `cli_exec.rs` keeps enforcing `-s` itself until Task 6 absorbs it.)** |
| `send-keys` | `send` | `usage: send-keys [-l] [-t target] key ...` |
| `send-prefix` | — | `usage: send-prefix` |
| `switch-client` | `switchc` | `usage: switch-client [-p] [-n]` |
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
- `detach-client` (amended in the Task 5 fix pass): `-s target` is optional
  in `resolve()` — bare `detach-client` resolves to `DetachClient { target:
  None }`, meaning "detach the acting client" (tmux behavior; required for
  the default `prefix-d` binding to be resolvable at all). The "no `-s` and
  no client context" error is the dispatcher's responsibility (Task 6), and
  its message is the SP2 verbatim usage line — the usage TEXT is unchanged,
  only who enforces it moved. See `detach_client_bare_is_current_client`.
- `switch-client` (Task 5 cross-module addition, sanctioned by the Task 5
  brief): only `-p` (previous session) and `-n` (next session) are parsed;
  exactly one of the two must be given — neither, or both, is a `usage:`
  error. Real tmux's `-l` ("last session") is a **documented deviation**:
  SP3 does not track a per-client last-session pointer, so `-l` is simply an
  unrecognized flag and hits the same `usage:` error as any other bad flag
  (not a special "unsupported" message) — see
  `switch_client_dash_l_is_usage_error`.

### Command table implementation note

`resolve()` maps every alias to a canonical name via a private `canonical()`
match, then dispatches on the canonical name; `usage()` shares the same
`canonical()` lookup, so alias and full-name lookups always agree
(`usage_lookup_by_alias_and_unknown`). Unknown name -> `Err(format!("unknown
command: {name}"))` from `resolve()`, `None` from `usage()`.

## `options` — typed tmux option registry + format subset (Task 4)

```rust
pub struct Options { /* opaque: private BTreeMap<&'static str, Value> */ }

impl Options {
    pub fn new() -> Options; // tmux defaults, see table below
    pub fn set(&mut self, name: &str, value: Option<&str>, append: bool, unset: bool) -> Result<(), String>;
    pub fn show(&self, name: &str) -> Option<String>;
    pub fn show_all(&self) -> String; // sorted `name value` lines, ALL known options

    pub fn prefix(&self) -> crate::keys::Key;
    pub fn base_index(&self) -> u32;
    pub fn pane_base_index(&self) -> u32;
    pub fn status_on(&self) -> bool;
    pub fn status_position_top(&self) -> bool;
    pub fn status_interval(&self) -> std::time::Duration;
    pub fn status_left(&self) -> &str;
    pub fn status_right(&self) -> &str;
    pub fn status_left_length(&self) -> u16;
    pub fn status_right_length(&self) -> u16;
    pub fn status_style(&self) -> &crate::style::PartialStyle;
    pub fn message_style(&self) -> &crate::style::PartialStyle;
    pub fn window_status_style(&self) -> &crate::style::PartialStyle;
    pub fn window_status_current_style(&self) -> &crate::style::PartialStyle;
    pub fn pane_border_style(&self) -> &crate::style::PartialStyle;
    pub fn pane_active_border_style(&self) -> &crate::style::PartialStyle;
    pub fn display_time(&self) -> std::time::Duration;
    pub fn repeat_time(&self) -> std::time::Duration;
    pub fn default_command(&self) -> &str;
    pub fn renumber_windows(&self) -> bool;
}

impl Default for Options { fn default() -> Options; } // == Options::new()

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemTimeParts {
    pub year: i32,   // full year, e.g. 2026
    pub month: u8,   // 1-12
    pub day: u8,
    pub weekday: u8, // 0 = Sunday, matches Win32 SYSTEMTIME.wDayOfWeek
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
}

pub struct FormatCtx<'a> {
    pub session: &'a str,
    pub window_index: u32,
    pub window_name: &'a str,
    pub window_flags: &'a str,
    pub pane_index: u32,
    pub hostname: &'a str,
    pub now: SystemTimeParts,
}

pub fn expand_format(fmt: &str, ctx: &FormatCtx) -> String;
```

Pure module: no I/O, `std` only. **Implementation module:** `src/options.rs`.
Depends on `crate::keys` (`Key`/`parse_key`/`key_name`, the `prefix` option's
type) and `crate::style` (`PartialStyle`/`parse_style`, every `*-style`
option's type). SP3 scope: one global `Options` instance — `-g`/`-w` on
`set-option`/`show-options` (from `cmd::ParsedCmd::SetOption`/`ShowOptions`)
both hit this same table; per-session/window overlays are SP4 (documented
deviation, matches the design spec).

Unit-tested with exact expected values (mirrors `keys.rs`/`style.rs`'s
style): `defaults_match_tmux`, `set_prefix_key`, `set_style_validates`,
`append_string`, `str_options_reject_control_chars`, `unset_restores_default`,
`on_off_parsing`, `flag_toggle_on_missing_value`, `number_parsing`,
`choice_parsing`, `unknown_option_err_exact`, `specs_and_defaults_stay_in_sync`,
`accepted_inert_options_store`, `show_quotes_when_needed`, `show_all_sorted`,
`expand_basic`, `expand_hash_escape`, `expand_unknown_long_empty`,
`expand_strftime` (19 tests total).

**Control-char rejection (final whole-branch review, 2026-07-07):**
`status-left`/`status-right` are settable at runtime by ANY attached client
(`:set -g status-left ...`), and the composited status row is written to
EVERY attached client's terminal — an embedded ESC/OSC/CSI sequence (title
spoofing, OSC 52 clipboard injection) or bare `\r\n` could corrupt other
clients' terminals. `Options::set` on a `Str`-kind option (`status-left`,
`status-right`, `default-command`, `default-terminal`) therefore rejects any
value containing a control character (`char::is_control`, the same rule
`model::validate_name` already applies to session/window names) with
`Err("bad value: <v>")`, where `<v>` sanitizes control chars to `?` (same
echo rule as `model::validate_name`) — this is checked BEFORE the value is
stored, `-a` append included (validated against the appended RESULT, i.e.
existing + addition, not the addition alone, since a clean fragment can
still complete a control sequence split across two `set -a` calls).
`expand_format`'s OUTPUT needs no matching second guard: its only live
inputs are already control-char-clean by construction — `#S`/`#W` come from
`model::validate_name`-guarded session/window names, and every strftime-style
`%`-code produces fixed-format digits or a month/weekday abbreviation from
the `MONTHS`/`WEEKDAYS` tables — so a clean `status-left`/`status-right`
template can only ever expand to a clean result.

### Supported options (exactly this table; anything else -> `unknown option: <name>`)

| option | type | default |
|---|---|---|
| `prefix` | key | `C-b` |
| `base-index` | number | 0 |
| `status` | on/off | on |
| `status-position` | choice (`top`/`bottom`) | bottom |
| `status-interval` | number (seconds) | 15 |
| `status-left` | string | `[#S] ` |
| `status-right` | string | `%H:%M %d-%b-%y` — **deviation from tmux**: real tmux embeds `#{=21:pane_title}`, outside the SP3 format subset; this default reproduces SP2's `local_clock()` string exactly (documented in the design spec) |
| `status-left-length` | number | 10 |
| `status-right-length` | number | 40 |
| `status-style` | style | `bg=green,fg=black` |
| `window-status-style` | style | (no-op default: nothing mentioned) |
| `window-status-current-style` | style | `underscore` |
| `message-style` | style | `bg=yellow,fg=black` |
| `pane-border-style` | style | (no-op default: nothing mentioned) |
| `pane-active-border-style` | style | `fg=green` |
| `display-time` | number (ms) | 750 |
| `repeat-time` | number (ms) | 500 |
| `default-command` | string | `powershell.exe -NoLogo` |
| `renumber-windows` | on/off | off |
| `mouse` | on/off (inert) | off |
| `history-limit` | number (inert) | 2000 |
| `escape-time` | number (inert) | 500 |
| `automatic-rename` | on/off (inert) | on |
| `allow-rename` | on/off (inert) | on |
| `mode-keys` | choice (`emacs`/`vi`, inert) | emacs |
| `default-terminal` | string (inert) | `screen` |
| `exit-empty` | on/off (inert) | on |
| `aggressive-resize` | on/off (inert) | off |
| `pane-base-index` | number (inert) | 0 |

"Inert" options are typed, validated, stored, and shown, but have no getter
beyond `show` — nothing reads them yet (SP4 functionality). Exception:
`pane-base-index` DOES have a typed getter (`Options::pane_base_index`), but
the getter itself is unused — pane indexes in kill-pane prompts/targets are
position-based from 0 regardless of this option (final review, 2026-07-07;
tracked in `docs/follow-ups.md`'s SP3-deferred list).

### `set` semantics

- `unset` (`-u`): restores the option's tmux default; `value` is ignored.
  Checked before `append`/`value` (an unset always wins over any other flag
  combination passed alongside it).
- `append` (`-a`): valid ONLY on `Str`-kind options (`status-left`,
  `status-right`, `default-command`, `default-terminal`) — concatenates
  `value` (or empty string if `value` is `None`) onto the current stored
  string, THEN validates the RESULT for control characters (see below) —
  not just the appended fragment in isolation — before storing. Any other
  option kind -> `Err("bad value: -a requires a string option")` (a
  deliberate SP3 simplification over tmux's real per-type append behavior,
  documented here rather than in-code only).
- `value: None` (no value token, e.g. `set -g mouse`):
  - On a `Flag`-kind option: **toggles** the current flag (tmux's real
    `set -g mouse` with no value flips it).
  - On any other kind: `Err(format!("bad value: {name} requires a value"))`.
- Unknown `name` -> `Err(format!("unknown option: {name}"))` (checked first,
  before unset/append/value handling).
- Per-kind value parsing (all applied case-sensitively to `name`, the value
  itself where noted is matched case-insensitively/leniently):
  - `Flag`: accepts exactly `on`, `off`, `1`, `0` (case-sensitive as
    written); anything else -> `Err(format!("bad value: {v}"))`.
  - `Number`: `str::parse::<u32>`; failure -> `Err(format!("bad value: {v}"))`.
  - `Key`: `crate::keys::parse_key(v)`; `None` -> `Err(format!("bad value:
    {v}"))`.
  - `Choice`: value ASCII-lowercased, matched against the option's fixed
    choice list (`status-position`: `top`/`bottom`; `mode-keys`:
    `emacs`/`vi`); no match -> `Err(format!("bad value: {v}"))`.
  - `Str`: rejected if it contains a control character (`char::is_control`,
    final whole-branch review, 2026-07-07 — see the control-char rejection
    note above) -> `Err(format!("bad value: {sanitized}"))`, where
    `sanitized` replaces every control char with `?` (same echo rule as
    `model::validate_name`); otherwise stored verbatim.
  - `Style`: `crate::style::parse_style(v)` — its own `Err("bad style:
    {v}")` is propagated as-is (NOT rewrapped into a `bad value:` message);
    the ORIGINAL source string `v` is stored alongside the parsed
    `PartialStyle` so `show`/`show_all` round-trip the user's exact text.

### `show` / `show_all` formatting

- `Flag` -> `on`/`off`. `Number` -> bare digits. `Key` ->
  `crate::keys::key_name`. `Choice` -> the bare matched word. `Str` and
  `Style` -> the stored string (for `Style`, the ORIGINAL source string, not
  a re-serialization of the parsed `PartialStyle`), quoted with `"..."` if
  it is empty or contains a literal space character, else printed bare
  (mirrors tmux's `show-options` quoting a value like `status-left "[#S] "`
  because of its trailing space).
- `show(name)` -> `None` for an unknown name (no panic).
- `show_all()` -> **every** known option (not just non-default overrides —
  a documented SP3 simplification vs. real tmux, which only lists
  non-default options unless `-g` is given), one `"{name} {value}"` line
  per option, sorted alphabetically by name, joined with `\n` (no trailing
  newline).

### `expand_format` (format-string subset)

Evaluated left-to-right over the input string, `#` and `%` are the only
two meta characters (everything else copies through verbatim):

| sequence | expands to |
|---|---|
| `##` | literal `#` |
| `#S` | `ctx.session` |
| `#I` | `ctx.window_index` (decimal) |
| `#W` | `ctx.window_name` |
| `#F` | `ctx.window_flags` |
| `#P` | `ctx.pane_index` (decimal) |
| `#H` | `ctx.hostname` |
| `#{session_name}` | `ctx.session` |
| `#{window_index}` | `ctx.window_index` (decimal) |
| `#{window_name}` | `ctx.window_name` |
| `#{<anything else>}` | empty (documented SP3 simplification — no
  conditionals/modifiers, full tmux format-expression engine is SP4) |
| `#<any other char>`, trailing lone `#` | empty (unrecognized short code
  consumes the one following character; a `#` with nothing after it is
  dropped) |
| `%%` | literal `%` |
| `%H` `%M` `%S` `%d` `%m` | zero-padded 2-digit hour/min/sec/day/month
  from `ctx.now` |
| `%Y` | full 4-digit year (`ctx.now.year`, not zero-padded/truncated) |
| `%y` | 2-digit year (`ctx.now.year.rem_euclid(100)`, zero-padded) |
| `%b` | 3-letter English month abbreviation (`Jan`..`Dec`) |
| `%a` | 3-letter English weekday abbreviation (`Sun`..`Sat`, `ctx.now.weekday % 7` indexes the table) |
| `%p` | `AM` if `hour < 12` else `PM` |
| `%I` | 12-hour zero-padded hour (`0` hour -> `12`, otherwise `hour % 12`) |
| `%<any other char>` | literal passthrough, BOTH characters kept (`%x`
  stays `%x` — not an error, not expanded) |
| trailing lone `%` | literal `%` |

`%H:%M %d-%b-%y` reproduces `src/server.rs`'s `local_clock()` string exactly
for equivalent inputs (verified by `expand_strftime`), which is why it was
chosen as `status-right`'s default value (see the design spec deviation
note). Unknown `#{...}` forms and unrecognized `#<c>` codes are silent
(empty), matching the design spec's "anything else renders empty"
directive — `expand_format` never returns an `Err`.

## `input-v2` — table-driven key machine (Task 5, sub-project 3)

**Amendment (sub-project 4, Task 9 — escape-time):** `KeyMachine` gains an
`escape_time: Duration` field (default `input::ESCAPE_TIME`, 500ms — mirrors
`repeat_time`'s existing shape) plus three methods:

```rust
impl KeyMachine {
    pub fn set_escape_time(&mut self, d: std::time::Duration);
    /// `true` once an outstanding pending ESC (tracked internally via
    /// `keys::KeyDecoder::pending_starts_with_escape`, checked at the end
    /// of every `feed()`) has aged past `escape_time` as of `now`.
    pub fn escape_ready(&self, now: std::time::Instant) -> bool;
    /// Force-drains the decoder's pending buffer (`keys::KeyDecoder::flush`)
    /// through the SAME `dispatch_key` path `feed` uses, producing whatever
    /// `KeyInputEvent`s result; clears the pending-escape timer.
    pub fn flush_now(&mut self, now: std::time::Instant) -> Vec<KeyInputEvent>;
}
```

`feed()` itself is unchanged in signature; internally, after decoding, it
now also updates a private `pending_escape_since: Option<Instant>` — set the
first time `decoder.pending_starts_with_escape()` goes true, cleared the
moment it goes false (sequence completed, OR force-flushed by `flush_now`)
— so the "clock" never restarts just because more bytes of the SAME
still-incomplete sequence keep arriving. `set_capture` (both directions)
also clears `pending_escape_since`, matching its existing "discard any
incomplete decoder buffer on the transition" rule.

**Server wiring** (`src/server.rs`, not part of this module's own contract
but documented here since it's the ONLY consumer): the `Tick` handler (every
50ms, per the design spec's `## 8. escape-time` section) calls
`escape_ready(Instant::now())` on every attached client's `KeyMachine`;
for each one that fires, `flush_now` is called and its resulting events are
run through `Server::process_key_events` — the SAME dispatch path
`handle_stdin` uses for a live `Stdin` frame (extracted into that shared
method by this task specifically so the two callers — a real `Stdin` frame,
and a `Tick`-triggered flush — don't duplicate the ~250-line event-dispatch
loop). A freshly-attaching client's `KeyMachine` seeds `escape_time` from
`Options::escape_time()` at `finish_attach` time (mirrors how `prefix` is
already seeded there); `set -g escape-time <ms>` at runtime additionally
broadcasts `set_escape_time` to every ALREADY-attached client's
`KeyMachine` (mirrors the existing `repeat-time` runtime-propagation
branch in `server/dispatch.rs::exec_set_option`).

Tests: `input::key_machine_tests::lone_escape_flushes_after_escape_time`,
`burst_csi_within_one_feed_never_reports_escape_ready`,
`escape_pending_resolved_by_later_bytes_before_ready`; end to end,
`tests/server_proto.rs::escape_key_reaches_pane_via_escape_time_flush`
(opens choose-tree, cancels it with a literal bare ESC byte after a
shrunk `escape-time`, then proves the flush didn't stall/merge decoder
state by typing `[A` immediately after and confirming it lands as two
ordinary literal characters).

**Amendment (sub-project 4, Task 5 — mouse):** `KeyInputEvent` gains a
`Mouse { event: keys::MouseEvent, raw: Vec<u8> }` variant. Unlike `Key`, a
`Mouse` event is NEVER routed through the prefix/table state machine or the
repeat window — it bypasses `dispatch_key` entirely (`KeyMachine::feed`
matches `keys::DecodedInput::Mouse { .. }` before it would ever reach the
prefix-armed / repeat-window / plain-forwardable checks) — mouse "bindings"
are hardcoded server-side (`server::dispatch::dispatch_mouse`), not looked up
in `crate::bindings::Bindings`. A pending coalesced `Forward` run is flushed
first so ordering relative to preceding plain keystrokes is preserved. During
`set_capture(true)` (a prompt/confirm/copy-mode-search line editor), the
capture short-circuit at the top of `feed()` still applies unchanged: a mouse
sequence arriving mid-capture is swallowed into the raw `Captured` blob like
any other byte (capture never decodes at all, mouse or otherwise). See the
`## mouse` section of
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md)
for the full server-side routing this feeds into.

```rust
// src/input.rs additions -- land ALONGSIDE the legacy InputMachine/Action/
// InputEvent above (unmodified; Task 6 deletes the legacy types once
// src/server.rs is rewired onto this one).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WhichTable { Root, Prefix }

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyInputEvent {
    Forward(Vec<u8>),
    Key { table: WhichTable, key: crate::keys::Key, raw: Vec<u8> },
    Captured(Vec<u8>),
}

pub struct KeyMachine { /* opaque: KeyDecoder + prefix + repeat/capture/escape-time state */ }
impl KeyMachine {
    pub fn new(prefix: crate::keys::Key) -> Self;
    pub fn feed(&mut self, bytes: &[u8], now: std::time::Instant) -> Vec<KeyInputEvent>;
    pub fn set_prefix(&mut self, prefix: crate::keys::Key);
    pub fn set_repeat_time(&mut self, d: std::time::Duration);
    pub fn arm_repeat(&mut self, now: std::time::Instant);
    pub fn set_capture(&mut self, on: bool);
    // Task 9, sub-project 4 (escape-time) -- see this section's matching
    // amendment note above for the full contract.
    pub fn set_escape_time(&mut self, d: std::time::Duration);
    pub fn escape_ready(&self, now: std::time::Instant) -> bool;
    pub fn flush_now(&mut self, now: std::time::Instant) -> Vec<KeyInputEvent>;
}
```

Pure logic, no I/O (same as the legacy machine). **Implementation:** the
`## input-v2` section of `src/input.rs`, immediately after the legacy
`impl InputMachine` block. Depends on `crate::keys` (`Key`/`KeyCode`/
`KeyDecoder`) for decoding; the server resolves `Key` events against a
mutable `crate::bindings::Bindings` table (Task 6) — `KeyMachine` itself
knows nothing about bindings or commands.

Unit-tested (`input::key_machine_tests`, mirrors the legacy module's exact-
value style): `plain_bytes_forward_coalesced`,
`prefix_then_key_reports_prefix_table`, `prefix_is_consumed_not_forwarded`,
`double_prefix_reports_prefix_table_key`, `root_table_keys_report_root`,
`arm_repeat_window`, `capture_bypasses_prefix`, `configurable_prefix`,
plus mouse tests (documented in the `## mouse` section of
[`2026-07-07-parity-polish-interfaces.md`](2026-07-07-parity-polish-interfaces.md))
and Task 9's `lone_escape_flushes_after_escape_time`,
`burst_csi_within_one_feed_never_reports_escape_ready`,
`escape_pending_resolved_by_later_bytes_before_ready`.

### Semantics

- **Decoding:** every `feed()` call runs `bytes` through an internal
  `keys::KeyDecoder`, then dispatches each resulting `DecodedKey` in order;
  incomplete trailing sequences stay buffered in the decoder across `feed()`
  calls (no timer, same deviation as `keys::KeyDecoder` itself).
- **Prefix:** a decoded key equal (full `Key` comparison, all fields) to the
  configured prefix key, while in `Normal` table state, is consumed — no
  event at all, not even `Forward` — and arms `Prefixed` state for the very
  next decoded key. That next key is reported as `Key { table: Prefix, .. }`
  **unconditionally**, even if it is itself the prefix key again (tmux
  binds the prefix key, in the prefix table, to `send-prefix` — matching the
  legacy double-Ctrl-b-sends-literal-Ctrl-b behavior, now expressed as an
  ordinary binding instead of hardcoded logic).
- **Repeat window:** `arm_repeat(now)` is called by the server immediately
  after it dispatches a binding with `repeat: true`. Until `now +
  repeat_time` (`set_repeat_time`, default 500ms via the existing
  `input::REPEAT_TIME` constant), every decoded key reports `Key { table:
  Prefix, .. }` **without** requiring a fresh prefix press — this
  reproduces the legacy `Ctrl-arrow` repeat behavior exactly, just
  generalized to any repeatable binding. A prefix press arriving inside an
  active window still arms `Prefixed` fresh (and clears the window) rather
  than being swallowed by the window check — checked in that order
  (`key == prefix` before the repeat-window check) so `arm_repeat` never
  makes the prefix key itself unavailable. Once `now` has passed the
  window's expiry, the window is cleared and the key falls through to
  ordinary Normal-state dispatch (Root/Forward) — it does NOT report
  `Prefix` for a stale, already-expired window.
- **Root-table throughput simplification (documented deviation):** in
  `Normal` state, with no repeat window active and not the prefix key, a
  decoded key that is a **plain, unmodified** (`ctrl == meta == shift ==
  false`) `Char`/`Enter`/`Tab`/`Space`/`BSpace` key is emitted directly as
  `Forward(raw)` — coalesced with any adjacent such keys within the same
  `feed()` call into a single `Forward` event, exactly like the legacy
  machine's plain-byte passthrough. **Every other** decoded key (any
  modifier flag set, or a named/special key outside that five-way set, e.g.
  arrows/F-keys/Escape/BTab/Home/End/PPage/NPage/IC/DC) is emitted as `Key {
  table: Root, .. }` instead, even though nothing preceded it — the server
  looks it up in the root table and forwards it raw itself if unbound.
  **Consequence (documented, matches the design spec's `bind -n`
  deviation):** `bind -n` on a bare unmodified printable char, Enter, Tab,
  Space, or BSpace is accepted by `cmd::resolve`/`bindings::Bindings::bind`
  but can **never fire** in SP3, because `KeyMachine` never emits a `Key`
  event for such a key in the first place — only keys carrying a modifier,
  or a named/special key outside this set, can be bound in the root table.
  Revisit if SP4 needs full `bind -n` coverage (would require dropping the
  coalescing optimization or making it conditional on whether the root
  table currently has any such binding).
- **Capture mode (`set_capture`):** while on, `feed()` bypasses ALL of the
  above — every byte, including the raw prefix byte and escape sequences,
  passes through verbatim as `Captured(bytes)`, coalesced per `feed()` call
  exactly like `Forward` (mirrors the legacy `InputMachine::set_capture`).
  **Mode-transition cleanup:** every call to `set_capture` (turning it on
  OR off) first calls `self.decoder.flush()` and **discards** the result —
  this clears the decoder's internal pending-byte buffer without emitting
  it as anything, mirroring the legacy machine's `pending.clear()` on its
  own `set_capture`/`set_confirming` transitions (an in-flight partial
  escape sequence at the moment of a mode flip is simply dropped, not
  forwarded or captured). Turning capture OFF additionally resets table
  state to `Normal` and clears any active repeat window, matching the
  legacy machine's "resumes Normal" behavior.

## `bindings` — tmux default key bindings table (Task 5)

```rust
// src/bindings.rs (new module; declared in src/lib.rs)

#[derive(Clone, Debug, PartialEq)]
pub struct Binding { pub cmds: Vec<crate::cmd::RawCmd>, pub repeat: bool }

pub struct Bindings { /* opaque: HashMap<Key, Binding> per WhichTable */ }
impl Default for Bindings { fn default() -> Bindings; } // tmux defaults, see table below
impl Bindings {
    pub fn bind(&mut self, table: crate::input::WhichTable, key: crate::keys::Key, binding: Binding);
    pub fn unbind(&mut self, table: crate::input::WhichTable, key: &crate::keys::Key) -> bool;
    pub fn unbind_all(&mut self, table: crate::input::WhichTable);
    pub fn lookup(&self, table: crate::input::WhichTable, key: &crate::keys::Key) -> Option<&Binding>;
    pub fn list(&self) -> String; // list-keys format, see below
}
```

Pure module: no I/O, `std` only (`HashMap`). **Implementation module:**
`src/bindings.rs`. Depends on `crate::keys` (`Key`/`KeyCode`/`parse_key`/
`key_name`), `crate::cmd::RawCmd`, and `crate::input::WhichTable`. Note the
call surface is `Bindings::default()` (the standard `Default` trait, not a
bespoke inherent `default()`) — chosen so `Bindings::default()` reads
idiomatically and clippy's `should_implement_trait` lint has no complaint.
`bind`/`unbind`/`unbind_all` mutate in place; there is deliberately no
"resolve" step here — `Binding.cmds` stays unresolved `RawCmd`s (tmux does
late binding too; `cmd::resolve` re-parses at dispatch time, Task 6).

Unit-tested (`bindings::tests`, mirrors `cmd.rs`'s exact-value style):
`defaults_cover_current_behavior`, `bind_unbind_roundtrip`,
`unbind_all_table`, `list_keys_format_exact` (4 tests).

### `Bindings::default()` — tmux defaults (root table starts empty)

Every entry below lives in the **prefix** table unless noted; each
reproduces the legacy hardcoded `InputMachine`/`Action` binding exactly, as
one or more `RawCmd`s:

| key | `Binding.cmds` | repeat |
|---|---|---|
| `%` | `split-window -h` | no |
| `"` | `split-window -v` | no |
| `Up`/`Down`/`Left`/`Right` | `select-pane -U`/`-D`/`-L`/`-R` | no (matches tmux: plain arrows are NOT repeatable, only `C-`arrows are) |
| `o` | `select-pane -t :.+` | no |
| `;` | `last-pane` | no |
| `x` | `confirm-before -p "kill-pane #P? (y/n)" kill-pane` | no |
| `z` | `resize-pane -Z` | no |
| `C-Up`/`C-Down`/`C-Left`/`C-Right` | `resize-pane -U`/`-D`/`-L`/`-R` | **yes** |
| `c` | `new-window` | no |
| `n` | `next-window` | no |
| `p` | `previous-window` | no |
| `l` | `last-window` | no |
| `0`-`9` | `select-window -t :=<d>` | no |
| `&` | `confirm-before -p "kill-window #W? (y/n)" kill-window` | no |
| `,` | `rename-window` (no name argument — see deviation below) | no |
| `$` | `rename-session` (no name argument — see deviation below) | no |
| `d` | `detach-client` | no |
| `(` | `switch-client -p` | no |
| `)` | `switch-client -n` | no |
| `C-b` (the default prefix key itself) | `send-prefix` | no |
| `:` | `command-prompt` | no |

**`,`/`$` deviation (documented):** real tmux binds these to
`command-prompt -I'#W' { rename-window '%%' }`-style templating that SP3's
`cmd`/`command-prompt` do not implement (no `{ }` command blocks, no `%%`
substitution). Instead `Bindings::default()` binds them directly to
`rename-window`/`rename-session` with **no name argument** at all (`args:
[]`) — `cmd::resolve` would itself reject that with a `usage:` error if
resolved as-is, but Task 6's dispatcher special-cases this: a
`rename-window`/`rename-session` command with no name argument, executed
with a live client context, opens the interactive status-line rename
prompt instead of calling `resolve` (matches sub-project 2's actual rename
flow, which was never a plain command in the first place).

**`send-prefix` binding note:** `Bindings::default()` hardcodes the prefix
key as `C-b` (the `options` module's own default), independent of whatever
prefix `KeyMachine` is separately configured with — if a `.tmux.conf`
changes `prefix` via `set -g prefix`, Task 6's config loader is responsible
for re-binding `send-prefix` onto the *new* prefix key (unbinding the old
one), the same way tmux's own `set -g prefix` handling does. `Bindings`
itself has no notion of "the current prefix"; it only stores whatever keys
`bind`/`default()` put in its maps.

### `list()` format (`list-keys`)

One line per binding: `bind-key [-r] -T <table> <keyname> <command...>` —
`-r ` (trailing space) present only when `binding.repeat` is `true`; table
is the literal `"root"`/`"prefix"`; `<keyname>` is `crate::keys::key_name`;
`<command...>` is every `RawCmd` in `Binding.cmds` rendered as `name arg1
arg2 ...` (a bare arg containing a literal space is re-quoted as `"arg"`)
and joined across multiple `RawCmd`s with `" ; "`. Lines are sorted by
table name first (`"prefix"` < `"root"` ASCII-betically), then by
`key_name` within a table, then joined with `\n` (no trailing newline).
Example (`list_keys_format_exact`): a `Bindings` with only `C-Up ->
resize-pane -U` (repeat: true) bound in the prefix table renders to exactly
`"bind-key -r -T prefix C-Up resize-pane -U"`.

## `server-dispatch` — unified command dispatcher (Task 6)

Everything in this section is **private** (`src/server.rs` / `src/server/
dispatch.rs`; `Server`'s only public item remains `pub fn run`), so it
documents BEHAVIOR RULES rather than locked types/signatures — the shapes
below (`ExecOutcome`, `dispatch_client`, `execute_headless`, ...) are the
actual implementation but are free to be refactored as long as every rule
here still holds and the `server_proto` tests it's pinned by stay green.

### Two execution paths, one command table

`src/server/dispatch.rs` adds `impl Server` methods reached from three entry
points:

- **CLI** (`ClientMsg::Cli(argv)`): `execute_cli_argv(&mut self, argv)` ->
  `argv[0]`/`argv[1..]` become one `RawCmd`, resolved via `cmd::resolve`,
  executed via `execute_headless`. No acting client -- `-t`/`-s`-less
  targets fall back to `Registry::find("")` (the most-recently-created
  session, same rule Task 5/8's empty-target amendments already
  established for the CLI subset).
- **Key bindings**: `handle_stdin` resolves a `KeyInputEvent::Key{table,
  key, raw}` via `bindings.lookup(table, &key)`; a hit's `Binding.cmds` go
  through `dispatch_client`, which loops `cmd::resolve` + `execute_for_client`
  per `RawCmd` (`;`-chains supported, matching `bind-key`/`confirm-before`'s
  tail semantics). A miss in `Root` forwards `raw`; a miss in `Prefix` is
  swallowed (no fallback to CLI-style errors -- matches the design spec).
- **`:` command-prompt / rename prompts**: share one status-line line editor
  (`ClientMode::Prompt { label, buf, kind: PromptKind::{RenameWindow,
  RenameSession, Command} }`); a `Command`-kind commit runs `cmd::parse_line`
  then `dispatch_client` on the parsed commands.

Both `execute_headless` and `execute_for_client` are big matches over
`cmd::ParsedCmd` that call small `exec_*` helpers; almost every helper takes
an `Option<&str>` "acting client's session name" (`None` headlessly) rather
than a client object, so session/window/pane target resolution
(`resolve_session_name`/`resolve_window_target`/`resolve_pane_target`) is
shared verbatim between both paths. A handful of commands only make sense
with a real client (`confirm-before`, `command-prompt`, `switch-client`, a
bare `detach-client`) and return a fixed error string headlessly (e.g.
`"switch-client: only from a client connection"` -- not exercised by any
CLI test, so not contract-locked, just documented here).

### `-t`/`-s` target grammar (SP3 simplification, documented deviation)

A target string is `[session:][window][.pane]`:

- An optional `session:` prefix is resolved via `Registry::find` (exact,
  then unambiguous prefix -- same rule as everywhere else `-t session`
  appears). Absent -> the acting client's own session, or (headlessly, or
  with no client) the most-recently-created session. Note this generic
  fallback means a CLI `rename-session <name>` with no `-t` now targets the
  most-recently-created session (an intentional, consistent behavior change
  from SP2's `cli_exec.rs`, which made `-t` mandatory for rename-session;
  the usage TEXT is unchanged, and a `-t`-less rename was previously just a
  usage error, so nothing depended on the old behavior).
- A client-context command's client-mutating side effects
  (`rename-session` re-syncing `client.session`; `kill-pane`/`kill-window`
  producing `ExecOutcome::Destroy` when the kill destroyed a whole session)
  apply ONLY when the RESOLVED target session is the acting client's own
  session -- determined by comparing resolved names, never by whether `-t`
  was syntactically present. A foreign session's destruction notifies that
  session's own clients via `destroy_session` and leaves the acting client
  attached (Task 6 review fixes; pinned by
  `rename_session_dash_t_own_session_keeps_client_synced` and
  `kill_foreign_session_pane_keeps_client_attached`).
- The window part: empty/absent -> the session's current window; `=N` or a
  bare number -> **exact** index match (`"window not found: <n>"` on miss,
  matching the pre-Task-6 `select-window`/digit-binding message text
  exactly); otherwise a name, exact-then-unambiguous-prefix.
- The pane part (only for pane-targeting commands: `split-window`,
  `select-pane`, `kill-pane`, `send-keys`; NOT `select-window`/
  `kill-window`/`rename-window`, whose target is a window, and NOT
  `rename-session`/`kill-session`/`detach-client -s`, whose target is a
  session): empty/absent -> the window's focused pane; `+`/`-` -> next/
  previous pane (cyclic); a bare number -> **position** in
  `Layout::panes()` order (leaf/tree order), per the design spec's own
  "pane index = position in layout.panes()" note.
- **Bare-token session-name fallback (tmux parity, post-Task-6 fix):** a
  target with no `:` and (for pane-targeting commands) no `.` either is a
  "bare token". Real tmux tries the session name FIRST for
  target-session/target-window, and the full pane-resolution rules are more
  involved than this codebase implements; this project adopts one
  PRACTICAL rule instead of the full tmux algorithm: a bare token that
  PARSES AS A NUMBER (optionally `=`-prefixed) keeps the meaning above
  unchanged -- a window index (window contexts) or a pane position in the
  contextual window (pane contexts), both scoped to the contextual session
  (`cli_split_window_command`/`kill_pane_via_command_targets`/
  `select_missing_window_shows_message` pin this). A bare token that is
  NON-NUMERIC is instead resolved as a SESSION NAME via `Registry::find`
  (exact, then unambiguous prefix) -- **not** a window-name lookup in the
  contextual session -- yielding that session's CURRENT window (window
  contexts: `resolve_window_target`) or that window's FOCUSED pane (pane
  contexts: `resolve_pane_target`). This is what makes `send-keys -t
  mysession ...` (a target naming only a session) work, the single most
  common scripting idiom (`send_keys_bare_session_target`). `+`/`-`
  (relative-to-focus) pane specs are exempted from this fallback -- they're
  never treated as session names. If `Registry::find` fails, its own error
  (`"can't find session: <t>"`) is surfaced as-is, rather than a generic
  `"pane not found: <t>"`/`"window not found: <t>"`, since a non-numeric
  bare token can now only ever mean "session name"
  (`bare_nonnumeric_unknown_session_errors`). Session-name-with-colon forms
  (`demo:1`, `demo:1.0`, `demo:`) are unaffected -- the `session:` prefix
  already resolves via `Registry::find`, and the window/pane parts after it
  keep their ordinary index-or-name resolution.
- Cross-session targeting for pane/window commands is NOT implemented
  beyond the `session:` prefix picking which session's registry state to
  read -- `split-window -t othersession:2.1` resolves fully; there is no
  further per-session-window overlay concern here (that's the `options`
  module's own documented SP3 simplification, unrelated).

### `ExecOutcome` (the dispatcher's internal return type)

`Ok(String)` (a transient message / CLI stdout -- routed to `client.message`
or `CliDone.out` by the caller), `Err(String)` (ditto for stderr/message),
`Detach` (the acting client should be dropped with a `[detached (from
session <name>)]` exit -- `handle_stdin` already removed it from
`self.clients` before dispatch, mirroring the pre-Task-6 confirm-handling
pattern), `Destroy` (the acting client's whole session died as a side
effect of `kill-pane`/`kill-window` -- `destroy_session` has ALREADY run
and messaged every OTHER attached client inside the `exec_kill_*_client`
helper; the caller only sends `Exit{0,"[exited]"}` to its own removed
client), and `SwitchedSession(String, String)` (`switch-client -p`/`-n`
moved the acting client -- both sessions' size/layout are recomputed once
the client is back in `self.clients`). Headlessly (CLI), any `Detach`/
`Destroy`/`SwitchedSession` collapses to a plain success with empty output
-- there's no "acting client" to signal for.

### `confirm-before` (replaces `ConfirmKillPane`/`ConfirmKillWindow`)

`ClientMode::ConfirmCmd { prompt: String, cmds: Vec<RawCmd>, pane_snapshot:
Option<PaneId>, window_snapshot: Option<WindowId> }` -- `prompt` is the `-p`
argument, ALREADY `expand_format`-expanded at arm-time (so `#P`/`#W`/etc.
reflect state as of the keypress, not the eventual `y`); `pane_snapshot`/
`window_snapshot` are the focused pane / current window at arm-time, used
ONLY for staleness invalidation (both `cancel_stale_confirms`, on every
pane/window death, and `feed_confirm_byte`, belt-and-braces at `y`-time,
treat a vanished snapshot as a silent no-op cancel -- this is what the
pre-Task-6 `stale_confirm_*` tests pin, now generalized). Confirming
(`y`/`Y`/`Enter`) re-dispatches the stored `cmds` fresh via
`dispatch_client` -- note this means a wrapped command with no explicit
target (the default `x`/`&` bindings: `kill-pane`/`kill-window`, no `-t`)
resolves against whatever is CURRENTLY focused/current at `y`-time, not a
pinned target id; this only differs observably from "pin the exact
original pane/window" if some OTHER client moved focus in the same session
between arm and confirm, which no test exercises (documented
simplification, matches tmux's own late-binding philosophy for
`confirm-before`'s wrapped command).

### Bare `rename-window`/`rename-session` (default `,`/`$` bindings)

`dispatch_client` special-cases a `RawCmd` named `rename-window`/`renamew`
or `rename-session`/`rename` with ZERO args (exactly what
`Bindings::default()`'s `,`/`$` entries carry) BEFORE calling `cmd::resolve`
(which would otherwise reject it with a `usage:` error, since `resolve`
requires exactly one positional): it opens the interactive status-line
prompt directly (pre-filled with the current window/session name), matching
the design spec's documented `,`/`$` deviation. Any OTHER invocation
(including `-t foo` with no name) still goes through `cmd::resolve` and
hits its usage error normally. This special-case only fires with a client
context; headlessly (CLI/`source-file`), a bare `rename-window`/
`rename-session` is a plain `cmd::resolve` usage error (no prompt to open).

### `default-command` / `renumber-windows` / `prefix` / `repeat-time` wiring

Every pane spawn (`split-window`, `new-window`, `new-session`, and the
initial `Attach`) reads `self.options.default_command()` at spawn time
(replacing the sub-project 1/2 hardcoded `SHELL` const). `kill-pane`/
`kill-window`'s shared helpers (`kill_pane_by_id`/`kill_window_by_id`) call
`Session::renumber()` (new in `model.rs`: reassigns every window's `index`
to `base_index + position` in the (index-sorted) `windows` vec — Task 7
review fix: renumbering starts from the session's creation-time
`base_index` floor, not 0, matching real tmux; see the `## model`
amendment in the sibling SP2 contract) after a successful
`kill_window` IF `options.renumber_windows()` is on at that moment.
`set-option prefix`: on an actual value change, unbinds the OLD prefix
key's `send-prefix` entry in the prefix table and rebinds `send-prefix`
onto the NEW prefix key, then calls `KeyMachine::set_prefix` on every
CURRENTLY ATTACHED client (a client that attaches AFTER the `set` sees the
new prefix from `finish_attach`'s `KeyMachine::new(self.options.prefix())`
automatically). `set-option repeat-time`: same broadcast pattern via
`KeyMachine::set_repeat_time`.

### CLI `"unknown command"` compatibility shim

`execute_cli_argv` is the ONLY place that still emits the sub-project 2
exact string `"unknown command"` (no colon, no name) for an unrecognized
`argv[0]` -- every other entry point (bindings, `:` prompt, `source-file`)
surfaces `cmd::resolve`'s real `"unknown command: <name>"`. This is a
deliberate, narrow compatibility shim: the pinned `cli_unknown_command_err`
test asserts the SP2-exact string, and the task brief requires it stay
green UNCHANGED, so `execute_cli_argv` pattern-matches on
`e.starts_with("unknown command:")` and substitutes the legacy string for
that one call site only.

### `send-keys` key/literal resolution

Non-`-l`: each argument is tried through `keys::parse_key` then
`keys::encode_key`; on success its encoded bytes are sent, on EITHER
failure (unrecognized token, or a recognized-but-unencodable key) the
argument's own UTF-8 bytes are sent literally instead (no error) -- this
matches tmux's real behavior of treating a multi-char argument like `"echo
hi"` as literal text while still letting `Enter`/`C-c`/etc. resolve as key
presses in the same command. `-l`: every argument is joined with single
spaces and sent as one literal run (no key parsing at all, no trailing
Enter).

## `config` - startup `.tmux.conf`/`.winmux.conf` loading, `source-file`, and `-f` (Task 7)

No new public module: this section documents private additions to
`src/server.rs`/`src/server/dispatch.rs`, plus the amended public surface of
`server::run`, `cli::{Invocation, Command::ServerRole}`, and
`client::autostart_server` (full signatures/diffs are in the sibling
`2026-07-07-server-client-interfaces.md` contract's `## server`/`## cli`/
`## client`/`## model` sections - this section is the design writeup; that
one is the exact-signature source of truth).

```rust
// src/server.rs (private)
struct ConfigCandidate { path: std::path::PathBuf, required: bool }
fn discover_config_files(xdg: Option<&str>, userprofile: Option<&str>, explicit: &[String]) -> Vec<ConfigCandidate>;

// src/server/dispatch.rs, impl Server (pub(super): called from server.rs::run)
pub(super) fn load_config_files(&mut self, candidates: &[ConfigCandidate]) -> Vec<String>; // returns collected error strings

// src/logging.rs (new lib module)
pub fn log_line(msg: &str); // best-effort append to %LOCALAPPDATA%\winmux\server.log
```

### Discovery (`discover_config_files`, pure)

- `explicit` non-empty (server `--config <path>`, repeatable, forwarded from
  the CLI's `-f`) REPLACES the default chain entirely, in the order given,
  each `required: true`.
- **`-` sentinel (Task 7 review fix, Important):** the special explicit
  value `-` (i.e. `winmux -f -` / `__server --config -`) is DROPPED from
  the candidate list but still counts as "explicit was given" — so
  `--config -` alone yields an empty candidate list: no default chain, no
  files, no errors (the tmux `-f /dev/null` idiom). A mixed
  `--config - --config real.conf` loads only `real.conf`. This doubles as
  the test suite's isolation seam: `tests/server_proto.rs`'s `start_server`
  helper passes `["-"]` (NOT `[]`) so a real `%USERPROFILE%\.tmux.conf` on
  a dev/CI machine can never contaminate a test's server through the
  default chain; only `start_server_with_config` with explicit temp-file
  paths loads anything. `usage_text()` mentions it (`-f - disables
  config`).
- Otherwise (`explicit` empty), the default chain: first
  `$XDG_CONFIG_HOME/tmux/tmux.conf` (only when the env var is `Some` AND
  non-empty) else `%USERPROFILE%\.tmux.conf`, loaded FIRST (`required:
  false`); then `%USERPROFILE%\.winmux.conf`, loaded SECOND (`required:
  false`) - so a ported tmux config's settings can be overridden by
  winmux-specific tweaks. Neither candidate is pushed if `userprofile` (for
  the `.tmux.conf` fallback / the `.winmux.conf` entry) is `None`.
- Existence is NOT checked here - deliberately pure and file-system-free so
  it's unit-testable without touching `std::env`/the filesystem (mutating
  process env in parallel tests is racy; see `server::config_discovery_tests`:
  `explicit_replaces_default_chain`, `xdg_wins_over_userprofile_tmux_conf`,
  `empty_xdg_falls_back_to_userprofile_tmux_conf`,
  `no_xdg_falls_back_to_userprofile_tmux_conf`,
  `no_userprofile_no_xdg_yields_no_candidates`,
  `dash_config_disables_defaults`).

### Loading (`Server::load_config_files`)

Shared by BOTH startup config loading (`run`) and runtime `source-file`
(`execute_source_file_headless`, now a thin wrapper: one `required: true`
candidate, `Ok(String::new())`/`Err(errors.join("\n"))` from the returned
error list - preserves its pre-Task-7 external behavior/error text exactly).
For each candidate, in order:

- File opens: `cmd::join_continuations` joins backslash-continued lines,
  then each logical line goes through `cmd::parse_line` -> `cmd::resolve` ->
  `Server::execute_headless` (the SAME headless dispatch path a CLI frame
  uses) - a failure at ANY of those three steps is collected as
  `"<path>:<lineno>: <err>"`; loading CONTINUES to the next line (tmux
  behavior: one bad line doesn't stop the rest of the file).
- File fails to open: a `required` candidate collects `"<path>: <io error>"`
  regardless of the error kind. A NON-required candidate collects nothing
  (silently skipped) IF the error is `NotFound` - a user with no config
  files at all is normal, not an error; any OTHER open failure (permissions,
  etc.) is still collected even for a non-required candidate, since that's
  not "the file doesn't exist," it's something worth surfacing.
- Loading NEVER stops at the first error, file or line - every candidate is
  attempted, and every error from every candidate is collected into one
  `Vec<String>` returned to the caller.

### Startup wiring (`server::run`)

After `PipeListener::bind` and spawning the accept thread, but BEFORE the
event loop's first iteration (so no attach is ever served against a
not-yet-configured `Options`/`Bindings`): `run` reads `XDG_CONFIG_HOME`/
`USERPROFILE` from the process environment, calls `discover_config_files`
with the server role's `config_files` argument as `explicit`, and calls
`server.load_config_files(&candidates)`. If the returned error list is
non-empty: `crate::logging::log_line` writes a `config: N error(s)` summary
line plus one indented line per error, and `Server::pending_config_message`
(`Option<String>`, new field) is set to the exact text `"config: N
error(s), see server.log"`.

### First-attach transient message

`Server::finish_attach` (shared tail of every successful `Attach`, all three
`AttachMode`s) does `self.pending_config_message.take().map(|m| (m,
Instant::now()))` and uses that as the new `ClientState`'s initial
`message` (same `(String, Instant)` transient-message slot `display-message`/
`window not found: <n>` etc. already use - same 750ms `MESSAGE_LIFETIME`
expiry, same "any Stdin frame from that client clears it" rule). Because
`Option::take` consumes it, this can only ever fire once, for whichever
client happens to attach first - REGARDLESS of session/mode (`NewAuto`,
`NewNamed`, or `Existing` all funnel through `finish_attach`). A second (or
later) attach - even to a brand new session - never sees it. Tests:
`config_file_applies_at_startup` (no errors: no message, but `base-index`/
custom `prefix`/custom binding are all live from the very first attach -
proving config loaded before that attach was served),
`config_errors_collected_and_continue` (one bad line between two good ones:
both good lines still applied, first attach sees the exact message text),
`second_attach_no_config_message`, `explicit_missing_config_is_error`
(a `--config` file that doesn't exist is 1 collected error, server still
comes up and serves attaches), `two_explicit_configs_later_wins` (plain
dispatch-order override: two `--config` files re-setting the same option,
the second wins - no special merge logic).

**Test timing note:** the transient message's 750ms lifetime can race real
ConPTY/shell-spawn latency if a test waits for the shell prompt (`"PS "`)
BEFORE checking for the message - `attach_auto_and_wait_prompt` drains every
intermediate frame into the test's `Grid`, and if shell boot takes longer
than 750ms the message has already been cleared (and overwritten in a later
frame's cell-diff) by the time `"PS "` appears. All four config-error tests
above attach directly (`attach()`, not `attach_auto_and_wait_prompt`) and
check the message FIRST, before waiting on anything else.

### `logging` - shared server-process log file (new lib module)

`src/logging.rs`: `server_log_dir()` (private, `%LOCALAPPDATA%\winmux`) and
`pub fn log_line(msg: &str)` (best-effort append-newline to `server.log`,
silently swallowing any failure - the server has no console to report a
logging failure to). Moved out of `main.rs` (previously private
`server_log_dir`/`log_line` functions, used only by the panic hook and
startup/exit lines) because `server.rs`'s startup config loading now ALSO
needs to log, and `main.rs` (a bin crate target) cannot be `use`d from
`server.rs` (a lib module) - `logging` is the shared home for the one thing
both sides of that boundary need. `main.rs` now calls
`winmux::logging::log_line` instead of a private duplicate. No unit tests
(thin I/O glue over a real file); covered indirectly by every
`tests/server_proto.rs` config-error test (which assert on the transient
attach message, not the log file itself) and manual runs.

### `-f` / `--config` CLI plumbing

- `cli::Invocation` gains `pub config: Option<String>`; `parse` extracts
  `-f <file>` from anywhere in `args` exactly like `-L` (last occurrence
  wins if repeated). `usage_text()`'s first line gained `[-f config-file]`.
- `cli::Command::ServerRole` gains `config: Vec<String>`; the hidden
  `__server` role parser now accepts `--config <path>`, repeatable, in any
  order relative to the still-required-exactly-once `--pipe`.
- `main.rs`: `Command::ServerRole { pipe, config } => run_server_role(&pipe,
  &config)` forwards straight to `server::run(pipe, config)`.
  `Command::NewSession { .. } => run_new_session(&invocation.socket,
  invocation.config.as_deref(), ...)` - `run_new_session`'s new `config:
  Option<&str>` parameter flows into `ensure_server(pipe, socket, config)`,
  which forwards to `client::autostart_server(socket, config)` ONLY on the
  `NotFound` (need-to-autostart) path. `Attach`/`Control`/`Help` never
  autostart, so `invocation.config` is simply unused there - matching tmux
  semantics: `-f` only matters when THIS invocation is the one starting the
  server; against an already-running server it's silently ignored (config is
  read once, at server start).
- `client::autostart_server(socket: &str, config: Option<&str>)`: appends
  `--config <file>` to the spawned `__server --pipe <name>` argv only when
  `config` is `Some`.
- Tests: `cli.rs`'s `dash_f_parses`, `dash_f_anywhere`,
  `dash_f_repeated_last_wins`, `server_role_config_args` (plus
  `bare_is_new_session`/`server_role_parse` updated for the new struct
  fields). No e2e coverage in Task 7 (Task 9's `e2e_tmux_conf_roundtrip`
  drives the real `-f`-flagged release binary end to end).

## `render-styles` — option-driven rendering (Task 8)

Locked type changes live in the two sibling contracts (amended in the same
commit): `render::StatusRow`/the new `Scene` shape in
`2026-07-06-mvp-interfaces.md`'s `render` section, and `status.rs`'s styled
`status_spans` signature in `2026-07-07-server-client-interfaces.md`'s
`## status` section. This section is the SP3-side design record: how the
server builds a `Scene` from the option table, plus the documented
simplifications.

### Scene building (`src/server.rs::render_one`, private)

- **status on/off:** `Options::status_on()` gates `Scene::status`
  (`None` = no row painted). The pane-area computation is status-aware:
  `recompute_session_size` contributes `(cols, rows - status_rows)` per
  client where `status_rows` is 1 when `status` is on, 0 when off
  (`Server::status_rows()`); the initial `Attach` sizing in `handle_attach`
  uses the same rule (so `set -g status off` in a startup config yields a
  full-height first session).
- **status position:** `StatusRow::top = Options::status_position_top()`.
  The pane area's y origin is `Server::pane_area_y()` — row 1 when the bar
  is on top, else row 0 — used consistently by `apply_layout_for_session`,
  `render_one`'s `layout.rects(area)`, and `dispatch.rs`'s layout-mutating
  areas (`split-window`/`select-pane -LRUD`/`resize-pane`), so drawn rects
  always line up with the pty/grid sizes.
- **set-option relayout:** `exec_set_option` on `status` or
  `status-position` recomputes every session's shared size and reapplies its
  layout (pty + grid resizes) — options are global, so ALL sessions move,
  not just the acting client's. The ordinary post-dispatch re-render then
  repaints. Pinned by `status_position_top_moves_bar` (bar on row 0 AND a
  subsequent command echoes below row 0) and `status_off_hides_bar` (bar
  text gone AND the bottom row's bg is Default, i.e. pane area).
- **Styles:** `base = status_style.apply_to(Style::default())`; message
  style, border, and active-border resolve the same way from their options.
  `StatusRow::right_style = base` (status-right `#[]` inline styles are
  SP4). Window-tab styles are layered by `status::status_spans` (see the
  amended `## status` section: current tab = `window-status-current-style`
  over BASE, not over `window-status-style`).
- **status-left/right:** both are `options::expand_format`-expanded per
  render against a live `FormatCtx` — session name, current window
  index/name, the current window's flags string (same chars as its status
  tab: `*` plus `Z` when zoomed), `pane_index` = the focused pane's position
  in `layout.panes()`, hostname, and `GetLocalTime` via a shared
  `system_time_parts()` (moved from `dispatch.rs` to `server.rs`; the
  dispatcher's `display-message`/`confirm-before -p` expansion now imports
  it). The defaults reproduce SP2 exactly: `"[#S] "` expands to the old
  `[<name>] ` prefix and `"%H:%M %d-%b-%y"` to the old `local_clock()`
  string — **default options => byte-identical output** (e2e suites
  untouched).
- **hostname (`#H`):** queried ONCE at server startup via `GetComputerNameW`
  (`Server::hostname`, private `computer_name()` helper; `COMPUTERNAME` env
  fallback). Requires no new Cargo feature
  (`Win32_System_WindowsProgramming`, already enabled for `GetUserNameW`).
- **Truncation interplay:** `status-left-length`/`status-right-length` cap
  the BUILT strings (first N chars, `truncate_chars`); the renderer's
  spatial truncation (right-first when left + right exceed the terminal
  width) still applies on top. Window tabs are not length-capped in SP3
  (tmux's per-tab `window-status-format` widths are SP4 territory).
- **message row placement:** with `status off`, a message/confirm/prompt
  overlays the BOTTOM row (over pane content), matching tmux's
  message-on-last-line behavior; with the bar on top, messages replace the
  TOP row's content.

### Documented simplifications (revisit SP4)

- `status-interval` is stored but unused: the status refresh remains the
  50ms `Tick`'s `local_clock()` change detector (minute granularity), so a
  custom `%S`-bearing `status-right` only refreshes when the minute flips.
- `right_style` is always `base` — no `#[fg=...]` inline style parsing.
- Window tabs have no length caps or `window-status-format` templating.

### Tests

- `render.rs` units: `status_top_row_zero`, `status_off_no_row`,
  `span_styles_emitted`, `border_style_applied`, `message_style_applied`;
  every pre-existing default-styled test updated mechanically to the new
  `Scene` shape with its expected byte strings UNCHANGED (default-byte
  equivalence pinned at the unit level).
- `status.rs` units: the three Task 5 tests re-expressed with styles
  (defaults = base + underline-on-current) plus `custom_styles_layering`.
- `tests/server_proto.rs`: `set_status_style_changes_bar`,
  `set_status_left_format`, `status_position_top_moves_bar`,
  `status_off_hides_bar`, `pane_active_border_style_runtime`,
  `window_status_current_style_override`.
- `tests/e2e.rs` / `tests/e2e_sessions.rs`: untouched and green (they assert
  default-styled output through the real binary — the visual-stability
  proof).
