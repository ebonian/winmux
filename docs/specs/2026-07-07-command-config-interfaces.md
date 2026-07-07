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
`feed()` calls. `flush()` is for end-of-input: it decodes whatever it can
from the leftover buffer, and for anything still ambiguous it peels one
byte at a time as a best-effort key (a lone `ESC` becomes an `Escape` key;
this is the only best-effort case exercised by tests, but the same
byte-peeling rule applies to any other stuck buffer, e.g. an unterminated
CSI sequence).

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
| `ESC <byte>` (byte is not `[` or `O`) | Meta + whatever that single byte alone decodes to (`ESC x` → `M-x`) |
| `ESC [ <params> <final>` (CSI) | see below |
| `ESC O P`/`Q`/`R`/`S` (SS3) | `F1`/`F2`/`F3`/`F4` |

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
