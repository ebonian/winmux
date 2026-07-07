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
`empty_string_ok_noop` (12 tests).

### Grammar (`parse_style`)

- Input is trimmed of surrounding whitespace first; empty after trim → `Ok`
  with a no-op `PartialStyle` (nothing mentioned, `apply_to` is identity).
  Otherwise split on `,` (components themselves are NOT individually
  trimmed — tmux has no internal whitespace tolerance); any empty component
  (leading/trailing/doubled comma) is a parse failure.
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
  field): `blink`, `noblink`, `strikethrough`, `nostrikethrough`,
  `double-underscore`, `curly-underscore`, `dotted-underscore`,
  `dashed-underscore`.
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

Named colors, `colour`/`color` prefixes, and `default` are matched
case-sensitively (lowercase, tmux config convention); hex digits after `#`
are the only case-insensitive part of the grammar.

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
