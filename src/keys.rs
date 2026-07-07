//! tmux key notation: parsing, VT encoding, and an incremental input decoder.
//!
//! Pure module: no I/O, no Windows APIs, `std` only.
//!
//! - [`parse_key`] / [`key_name`]: tmux key notation (`C-a`, `M-x`, `S-F5`,
//!   `C-M-Left`) <-> [`Key`] values, for `.tmux.conf` `bind-key`/`unbind-key`
//!   lines and `list-keys` output.
//! - [`encode_key`]: `Key` -> VT bytes to SEND to a pane (`send-keys`).
//! - [`KeyDecoder`]: VT input bytes -> `Key` values (the future table-driven
//!   replacement for `src/input.rs`'s hand-rolled escape parsing).

/// A single logical key: a base [`KeyCode`] plus modifier flags.
///
/// `Char` codes are literal and case-sensitive (tmux treats `V` and `v` as
/// distinct keys with no implicit `shift` flag) except when combined with
/// `ctrl`, where tmux normalizes ASCII letters to lowercase (`C-A` ==
/// `C-a`) — [`parse_key`] and [`encode_key`] both apply this rule.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KeyCode {
    Char(char),
    F(u8),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PPage,
    NPage,
    IC,
    DC,
    Enter,
    Escape,
    Space,
    Tab,
    BSpace,
    BTab,
}

fn plain(code: KeyCode) -> Key {
    Key {
        code,
        ctrl: false,
        meta: false,
        shift: false,
    }
}

/// Parse tmux key notation: `C-`/`M-`/`S-` prefixes (combinable, any order),
/// case-insensitive named keys (`Enter`, `PPage`, `F5`, ...), or a single
/// literal character (case-sensitive). Returns `None` for anything else
/// (empty string, multi-char non-named token, unknown prefix letter without
/// a following `-`, etc).
pub fn parse_key(s: &str) -> Option<Key> {
    let mut ctrl = false;
    let mut meta = false;
    let mut shift = false;
    let mut rest = s;

    loop {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b'-' {
            match bytes[0] {
                b'C' => {
                    ctrl = true;
                    rest = &rest[2..];
                    continue;
                }
                b'M' => {
                    meta = true;
                    rest = &rest[2..];
                    continue;
                }
                b'S' => {
                    shift = true;
                    rest = &rest[2..];
                    continue;
                }
                _ => {}
            }
        }
        break;
    }

    if rest.is_empty() {
        return None;
    }

    let lower = rest.to_ascii_lowercase();
    let code = match lower.as_str() {
        "enter" => Some(KeyCode::Enter),
        "escape" => Some(KeyCode::Escape),
        "space" => Some(KeyCode::Space),
        "tab" => Some(KeyCode::Tab),
        "bspace" => Some(KeyCode::BSpace),
        "btab" => Some(KeyCode::BTab),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "ppage" => Some(KeyCode::PPage),
        "npage" => Some(KeyCode::NPage),
        "ic" => Some(KeyCode::IC),
        "dc" => Some(KeyCode::DC),
        _ => lower
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .map(KeyCode::F),
    };

    match code {
        Some(code) => Some(Key {
            code,
            ctrl,
            meta,
            shift,
        }),
        None => parse_char(rest, ctrl, meta, shift),
    }
}

fn parse_char(rest: &str, ctrl: bool, meta: bool, shift: bool) -> Option<Key> {
    let mut chars = rest.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None; // more than one char: not a valid literal-char token
    }
    // tmux lowercases ctrl-letter combos: C-A normalizes to C-a.
    let c = if ctrl && c.is_ascii_alphabetic() {
        c.to_ascii_lowercase()
    } else {
        c
    };
    Some(Key {
        code: KeyCode::Char(c),
        ctrl,
        meta,
        shift,
    })
}

/// Canonical tmux notation for a key (for `list-keys`). `C-` before `M-`
/// before `S-`; named keys use tmux's canonical capitalization. Round-trips
/// through [`parse_key`] for every key `parse_key`/`KeyDecoder` can produce.
pub fn key_name(k: &Key) -> String {
    let mut prefix = String::new();
    if k.ctrl {
        prefix.push_str("C-");
    }
    if k.meta {
        prefix.push_str("M-");
    }
    if k.shift {
        prefix.push_str("S-");
    }
    let body = match k.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PPage => "PPage".to_string(),
        KeyCode::NPage => "NPage".to_string(),
        KeyCode::IC => "IC".to_string(),
        KeyCode::DC => "DC".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Escape => "Escape".to_string(),
        KeyCode::Space => "Space".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BSpace => "BSpace".to_string(),
        KeyCode::BTab => "BTab".to_string(),
    };
    format!("{prefix}{body}")
}

/// Encode a key as bytes to send to a pane (`send-keys`). Returns `None` for
/// combinations winmux doesn't support encoding (e.g. ctrl on a named key
/// other than the arrow CSI-modifier form, or ctrl on a non-letter/space
/// char).
pub fn encode_key(k: &Key) -> Option<Vec<u8>> {
    // Arrows support the full CSI-modifier-parameter form; every other
    // modifier combination is handled below.
    if let Some(letter) = arrow_letter(k.code) {
        if !k.ctrl && !k.meta && !k.shift {
            return Some(format!("\x1b[{letter}").into_bytes());
        }
        let n = 1 + (k.shift as u8) + (k.meta as u8) * 2 + (k.ctrl as u8) * 4;
        return Some(format!("\x1b[1;{n}{letter}").into_bytes());
    }

    if k.ctrl {
        if let KeyCode::Char(c) = k.code {
            if c.is_ascii_alphabetic() {
                let byte = (c.to_ascii_lowercase() as u8) - b'a' + 1;
                return Some(wrap_meta(vec![byte], k.meta));
            } else if c == ' ' {
                return Some(wrap_meta(vec![0], k.meta));
            }
        }
        if let KeyCode::Space = k.code {
            return Some(wrap_meta(vec![0], k.meta));
        }
        return None; // unsupported ctrl combo (named key, non-letter char)
    }

    if k.meta {
        let base = encode_named(k.code)?;
        return Some(wrap_meta(base, true));
    }

    if k.shift && !matches!(k.code, KeyCode::Char(_)) {
        return None; // shift on a named key: only arrows support this
    }

    encode_named(k.code)
}

fn wrap_meta(base: Vec<u8>, meta: bool) -> Vec<u8> {
    if !meta {
        return base;
    }
    let mut out = vec![0x1b];
    out.extend(base);
    out
}

fn arrow_letter(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Up => Some('A'),
        KeyCode::Down => Some('B'),
        KeyCode::Right => Some('C'),
        KeyCode::Left => Some('D'),
        _ => None,
    }
}

/// Encode a key with no modifiers (arrows are handled separately by the
/// caller since they support the CSI-modifier form).
fn encode_named(code: KeyCode) -> Option<Vec<u8>> {
    match code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            Some(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::BSpace => Some(vec![0x7f]),
        KeyCode::Escape => Some(vec![0x1b]),
        KeyCode::Space => Some(vec![b' ']),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PPage => Some(b"\x1b[5~".to_vec()),
        KeyCode::NPage => Some(b"\x1b[6~".to_vec()),
        KeyCode::IC => Some(b"\x1b[2~".to_vec()),
        KeyCode::DC => Some(b"\x1b[3~".to_vec()),
        KeyCode::BTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::F(1) => Some(b"\x1bOP".to_vec()),
        KeyCode::F(2) => Some(b"\x1bOQ".to_vec()),
        KeyCode::F(3) => Some(b"\x1bOR".to_vec()),
        KeyCode::F(4) => Some(b"\x1bOS".to_vec()),
        KeyCode::F(5) => Some(b"\x1b[15~".to_vec()),
        KeyCode::F(6) => Some(b"\x1b[17~".to_vec()),
        KeyCode::F(7) => Some(b"\x1b[18~".to_vec()),
        KeyCode::F(8) => Some(b"\x1b[19~".to_vec()),
        KeyCode::F(9) => Some(b"\x1b[20~".to_vec()),
        KeyCode::F(10) => Some(b"\x1b[21~".to_vec()),
        KeyCode::F(11) => Some(b"\x1b[23~".to_vec()),
        KeyCode::F(12) => Some(b"\x1b[24~".to_vec()),
        KeyCode::F(_) => None,
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
            unreachable!("arrows are handled by encode_key before reaching encode_named")
        }
    }
}

/// One decoded key plus the exact raw bytes it came from (so an unbound key
/// can be forwarded to the pane verbatim).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecodedKey {
    pub key: Key,
    pub raw: Vec<u8>,
}

/// Incremental VT-input decoder: turns a stream of client keystroke bytes
/// into [`DecodedKey`] values. Escape sequences (and partial UTF-8
/// sequences) split across [`KeyDecoder::feed`] calls are buffered until
/// complete; [`KeyDecoder::flush`] drains an incomplete trailing buffer as
/// best-effort keys (a lone `ESC` becomes an `Escape` key).
pub struct KeyDecoder {
    pending: Vec<u8>,
}

impl Default for KeyDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyDecoder {
    pub fn new() -> Self {
        KeyDecoder {
            pending: Vec::new(),
        }
    }

    /// Feed more input bytes, returning every key that became complete as a
    /// result (zero or more; an in-progress escape/UTF-8 sequence produces
    /// none until it completes).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<DecodedKey> {
        let mut out = Vec::new();
        for &b in bytes {
            self.pending.push(b);
            if let Some(key) = classify(&self.pending) {
                out.push(DecodedKey {
                    key,
                    raw: std::mem::take(&mut self.pending),
                });
            }
        }
        out
    }

    /// Drain any incomplete trailing buffer as best-effort keys. Call this
    /// when no more input is coming (e.g. end of a read) so a lone `ESC` or
    /// a truncated escape sequence still produces something rather than
    /// silently vanishing.
    pub fn flush(&mut self) -> Vec<DecodedKey> {
        let mut out = Vec::new();
        while !self.pending.is_empty() {
            if let Some(key) = classify(&self.pending) {
                out.push(DecodedKey {
                    key,
                    raw: std::mem::take(&mut self.pending),
                });
                break;
            }
            // Still incomplete with no more bytes coming: peel off the
            // first byte as a best-effort key and keep draining the rest.
            let first = self.pending.remove(0);
            let key = if first == 0x1b {
                plain(KeyCode::Escape)
            } else {
                plain(KeyCode::Char(first as char))
            };
            out.push(DecodedKey {
                key,
                raw: vec![first],
            });
        }
        out
    }
}

/// Try to decode a complete key from `buf` (which always starts at a fresh
/// token boundary). `None` means "valid so far, need more bytes".
fn classify(buf: &[u8]) -> Option<Key> {
    match buf[0] {
        0x1b => classify_escape(buf),
        0xc0..=0xdf => classify_utf8(buf, 2),
        0xe0..=0xef => classify_utf8(buf, 3),
        0xf0..=0xf7 => classify_utf8(buf, 4),
        b => Some(classify_single_byte(b)),
    }
}

fn classify_single_byte(b: u8) -> Key {
    match b {
        0x00 => Key {
            code: KeyCode::Space,
            ctrl: true,
            meta: false,
            shift: false,
        },
        0x09 => plain(KeyCode::Tab),
        0x0d => plain(KeyCode::Enter),
        0x7f => plain(KeyCode::BSpace),
        1..=0x1a => Key {
            code: KeyCode::Char((b'a' + (b - 1)) as char),
            ctrl: true,
            meta: false,
            shift: false,
        },
        _ => plain(KeyCode::Char(b as char)),
    }
}

fn classify_utf8(buf: &[u8], needed: usize) -> Option<Key> {
    if buf.len() < needed {
        return None;
    }
    match std::str::from_utf8(buf) {
        Ok(s) => Some(plain(KeyCode::Char(s.chars().next().unwrap()))),
        Err(_) => Some(plain(KeyCode::Char(char::REPLACEMENT_CHARACTER))),
    }
}

fn classify_escape(buf: &[u8]) -> Option<Key> {
    if buf.len() == 1 {
        return None; // lone ESC so far: wait (or flush() resolves it)
    }
    match buf[1] {
        b'[' => classify_csi(buf),
        b'O' => {
            if buf.len() < 3 {
                return None;
            }
            let code = match buf[2] {
                b'P' => KeyCode::F(1),
                b'Q' => KeyCode::F(2),
                b'R' => KeyCode::F(3),
                b'S' => KeyCode::F(4),
                other => return Some(plain(KeyCode::Char(other as char))),
            };
            Some(plain(code))
        }
        other => {
            // ESC <byte>: Meta + whatever that single byte alone decodes to.
            classify(&[other]).map(|mut k| {
                k.meta = true;
                k
            })
        }
    }
}

fn classify_csi(buf: &[u8]) -> Option<Key> {
    let mut idx = 2;
    while idx < buf.len() {
        let b = buf[idx];
        if (0x40..=0x7e).contains(&b) {
            let params_str = std::str::from_utf8(&buf[2..idx]).unwrap_or("");
            return Some(
                parse_csi(params_str, b).unwrap_or_else(|| plain(KeyCode::Char(b as char))),
            );
        }
        idx += 1;
    }
    None // no final byte yet: still incomplete
}

fn mods_from_param(p: i64) -> (bool, bool, bool) {
    let bits = (p - 1).max(0) as u8;
    (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0) // (shift, meta, ctrl)
}

fn parse_csi(params_str: &str, final_byte: u8) -> Option<Key> {
    let parts: Vec<i64> = if params_str.is_empty() {
        Vec::new()
    } else {
        params_str.split(';').map(|s| s.parse().unwrap_or(0)).collect()
    };

    match final_byte {
        b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'Z' => {
            let code = match final_byte {
                b'A' => KeyCode::Up,
                b'B' => KeyCode::Down,
                b'C' => KeyCode::Right,
                b'D' => KeyCode::Left,
                b'H' => KeyCode::Home,
                b'F' => KeyCode::End,
                b'Z' => KeyCode::BTab,
                _ => unreachable!(),
            };
            let (shift, meta, ctrl) = if parts.len() == 2 {
                mods_from_param(parts[1])
            } else {
                (false, false, false)
            };
            Some(Key {
                code,
                ctrl,
                meta,
                shift,
            })
        }
        b'~' => {
            let n = *parts.first()?;
            let code = match n {
                2 => KeyCode::IC,
                3 => KeyCode::DC,
                5 => KeyCode::PPage,
                6 => KeyCode::NPage,
                15 => KeyCode::F(5),
                17 => KeyCode::F(6),
                18 => KeyCode::F(7),
                19 => KeyCode::F(8),
                20 => KeyCode::F(9),
                21 => KeyCode::F(10),
                23 => KeyCode::F(11),
                24 => KeyCode::F(12),
                1 => KeyCode::Home,
                4 => KeyCode::End,
                _ => return None,
            };
            let (shift, meta, ctrl) = if parts.len() == 2 {
                mods_from_param(parts[1])
            } else {
                (false, false, false)
            };
            Some(Key {
                code,
                ctrl,
                meta,
                shift,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_key ----

    #[test]
    fn parse_ctrl_letter() {
        assert_eq!(parse_key("C-a"), Some(Key { code: KeyCode::Char('a'), ctrl: true, meta: false, shift: false }));
        assert_eq!(parse_key("C-A"), parse_key("C-a"));
    }

    #[test]
    fn parse_meta() {
        assert_eq!(parse_key("M-x"), Some(Key { code: KeyCode::Char('x'), ctrl: false, meta: true, shift: false }));
    }

    #[test]
    fn parse_shift_fkey() {
        assert_eq!(parse_key("S-F5"), Some(Key { code: KeyCode::F(5), ctrl: false, meta: false, shift: true }));
    }

    #[test]
    fn parse_combined() {
        assert_eq!(
            parse_key("C-M-Left"),
            Some(Key { code: KeyCode::Left, ctrl: true, meta: true, shift: false })
        );
    }

    #[test]
    fn parse_named() {
        let cases: &[(&str, KeyCode)] = &[
            ("Enter", KeyCode::Enter),
            ("enter", KeyCode::Enter),
            ("Escape", KeyCode::Escape),
            ("ESCAPE", KeyCode::Escape),
            ("Space", KeyCode::Space),
            ("Tab", KeyCode::Tab),
            ("BSpace", KeyCode::BSpace),
            ("bspace", KeyCode::BSpace),
            ("Up", KeyCode::Up),
            ("PPage", KeyCode::PPage),
            ("ppage", KeyCode::PPage),
            ("IC", KeyCode::IC),
            ("ic", KeyCode::IC),
            ("BTab", KeyCode::BTab),
            ("btab", KeyCode::BTab),
        ];
        for (s, code) in cases {
            assert_eq!(
                parse_key(s),
                Some(Key { code: *code, ctrl: false, meta: false, shift: false }),
                "parsing {s:?}"
            );
        }
    }

    #[test]
    fn parse_plain_chars() {
        assert_eq!(parse_key("%"), Some(Key { code: KeyCode::Char('%'), ctrl: false, meta: false, shift: false }));
        assert_eq!(parse_key("\""), Some(Key { code: KeyCode::Char('"'), ctrl: false, meta: false, shift: false }));
        assert_eq!(parse_key("1"), Some(Key { code: KeyCode::Char('1'), ctrl: false, meta: false, shift: false }));
        assert_eq!(parse_key("é"), Some(Key { code: KeyCode::Char('é'), ctrl: false, meta: false, shift: false }));
    }

    #[test]
    fn parse_invalid_none() {
        assert_eq!(parse_key(""), None);
        assert_eq!(parse_key("C-"), None);
        assert_eq!(parse_key("Fxx"), None);
        assert_eq!(parse_key("ab"), None);
    }

    // ---- key_name / round-trip ----

    #[test]
    fn key_name_roundtrip() {
        let keys = [
            Key { code: KeyCode::Char('a'), ctrl: true, meta: false, shift: false },
            Key { code: KeyCode::Char('x'), ctrl: false, meta: true, shift: false },
            Key { code: KeyCode::F(5), ctrl: false, meta: false, shift: true },
            Key { code: KeyCode::Left, ctrl: true, meta: true, shift: false },
            Key { code: KeyCode::Enter, ctrl: false, meta: false, shift: false },
            Key { code: KeyCode::PPage, ctrl: false, meta: false, shift: false },
            Key { code: KeyCode::BSpace, ctrl: false, meta: false, shift: false },
        ];
        for k in keys {
            let name = key_name(&k);
            assert_eq!(parse_key(&name), Some(k), "round-trip of {name:?}");
        }
        assert_eq!(key_name(&Key { code: KeyCode::Char('a'), ctrl: true, meta: false, shift: false }), "C-a");
        assert_eq!(key_name(&Key { code: KeyCode::F(5), ctrl: false, meta: false, shift: true }), "S-F5");
        assert_eq!(
            key_name(&Key { code: KeyCode::Left, ctrl: true, meta: true, shift: false }),
            "C-M-Left"
        );
    }

    // ---- encode_key ----

    #[test]
    fn encode_ctrl() {
        assert_eq!(
            encode_key(&Key { code: KeyCode::Char('c'), ctrl: true, meta: false, shift: false }),
            Some(vec![0x03])
        );
    }

    #[test]
    fn encode_meta() {
        assert_eq!(
            encode_key(&Key { code: KeyCode::Char('x'), ctrl: false, meta: true, shift: false }),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn encode_named() {
        assert_eq!(encode_key(&plain(KeyCode::Enter)), Some(b"\r".to_vec()));
        assert_eq!(encode_key(&plain(KeyCode::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(&plain(KeyCode::F(5))), Some(b"\x1b[15~".to_vec()));
        assert_eq!(encode_key(&plain(KeyCode::Home)), Some(b"\x1b[H".to_vec()));
        assert_eq!(encode_key(&plain(KeyCode::DC)), Some(b"\x1b[3~".to_vec()));
    }

    #[test]
    fn encode_unsupported_combo_is_none() {
        // ctrl on a named key other than the arrow CSI-modifier form.
        assert_eq!(
            encode_key(&Key { code: KeyCode::F(5), ctrl: true, meta: false, shift: false }),
            None
        );
    }

    #[test]
    fn encode_roundtrip_via_decoder() {
        let keys = [
            plain(KeyCode::Enter),
            plain(KeyCode::Up),
            plain(KeyCode::Home),
            plain(KeyCode::F(5)),
            plain(KeyCode::DC),
            Key { code: KeyCode::Char('c'), ctrl: true, meta: false, shift: false },
            Key { code: KeyCode::Char('x'), ctrl: false, meta: true, shift: false },
            Key { code: KeyCode::Up, ctrl: true, meta: false, shift: false },
            Key { code: KeyCode::Up, ctrl: false, meta: false, shift: true },
            Key { code: KeyCode::Space, ctrl: true, meta: false, shift: false },
        ];
        for k in keys {
            let bytes = encode_key(&k).unwrap_or_else(|| panic!("no encoding for {k:?}"));
            let mut dec = KeyDecoder::new();
            let mut decoded = dec.feed(&bytes);
            decoded.extend(dec.flush());
            assert_eq!(decoded.len(), 1, "encoding of {k:?} -> {bytes:?} decoded to {decoded:?}");
            assert_eq!(decoded[0].key, k, "round-trip of {k:?}");
            assert_eq!(decoded[0].raw, bytes);
        }
    }

    // ---- KeyDecoder ----

    #[test]
    fn decode_plain_bytes() {
        let mut dec = KeyDecoder::new();
        let out = dec.feed(b"ab");
        assert_eq!(
            out,
            vec![
                DecodedKey { key: plain(KeyCode::Char('a')), raw: vec![b'a'] },
                DecodedKey { key: plain(KeyCode::Char('b')), raw: vec![b'b'] },
            ]
        );
    }

    #[test]
    fn decode_ctrl_bytes() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(&[0x03]),
            vec![DecodedKey {
                key: Key { code: KeyCode::Char('c'), ctrl: true, meta: false, shift: false },
                raw: vec![0x03],
            }]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(&[0x0d]),
            vec![DecodedKey { key: plain(KeyCode::Enter), raw: vec![0x0d] }]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(&[0x09]),
            vec![DecodedKey { key: plain(KeyCode::Tab), raw: vec![0x09] }]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(&[0x7f]),
            vec![DecodedKey { key: plain(KeyCode::BSpace), raw: vec![0x7f] }]
        );
    }

    #[test]
    fn decode_prefix_byte_is_ctrl_b() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(&[0x02]),
            vec![DecodedKey {
                key: Key { code: KeyCode::Char('b'), ctrl: true, meta: false, shift: false },
                raw: vec![0x02],
            }]
        );
    }

    #[test]
    fn decode_csi_arrows() {
        for (bytes, code) in [
            (&b"\x1b[A"[..], KeyCode::Up),
            (&b"\x1b[B"[..], KeyCode::Down),
            (&b"\x1b[C"[..], KeyCode::Right),
            (&b"\x1b[D"[..], KeyCode::Left),
        ] {
            let mut dec = KeyDecoder::new();
            assert_eq!(
                dec.feed(bytes),
                vec![DecodedKey { key: plain(code), raw: bytes.to_vec() }],
                "bytes {bytes:?}"
            );
        }
    }

    #[test]
    fn decode_csi_modified() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1b[1;5A"),
            vec![DecodedKey {
                key: Key { code: KeyCode::Up, ctrl: true, meta: false, shift: false },
                raw: b"\x1b[1;5A".to_vec(),
            }]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1b[1;2A"),
            vec![DecodedKey {
                key: Key { code: KeyCode::Up, ctrl: false, meta: false, shift: true },
                raw: b"\x1b[1;2A".to_vec(),
            }]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1b[1;3A"),
            vec![DecodedKey {
                key: Key { code: KeyCode::Up, ctrl: false, meta: true, shift: false },
                raw: b"\x1b[1;3A".to_vec(),
            }]
        );
    }

    #[test]
    fn decode_ss3_fkeys() {
        for (bytes, n) in [
            (&b"\x1bOP"[..], 1u8),
            (&b"\x1bOQ"[..], 2),
            (&b"\x1bOR"[..], 3),
            (&b"\x1bOS"[..], 4),
        ] {
            let mut dec = KeyDecoder::new();
            assert_eq!(
                dec.feed(bytes),
                vec![DecodedKey { key: plain(KeyCode::F(n)), raw: bytes.to_vec() }]
            );
        }
    }

    #[test]
    fn decode_csi_tilde_keys() {
        for (bytes, code) in [
            (&b"\x1b[2~"[..], KeyCode::IC),
            (&b"\x1b[3~"[..], KeyCode::DC),
            (&b"\x1b[5~"[..], KeyCode::PPage),
            (&b"\x1b[6~"[..], KeyCode::NPage),
            (&b"\x1b[15~"[..], KeyCode::F(5)),
            (&b"\x1b[17~"[..], KeyCode::F(6)),
            (&b"\x1b[18~"[..], KeyCode::F(7)),
            (&b"\x1b[19~"[..], KeyCode::F(8)),
            (&b"\x1b[20~"[..], KeyCode::F(9)),
            (&b"\x1b[21~"[..], KeyCode::F(10)),
            (&b"\x1b[23~"[..], KeyCode::F(11)),
            (&b"\x1b[24~"[..], KeyCode::F(12)),
            (&b"\x1b[1~"[..], KeyCode::Home),
            (&b"\x1b[4~"[..], KeyCode::End),
        ] {
            let mut dec = KeyDecoder::new();
            assert_eq!(
                dec.feed(bytes),
                vec![DecodedKey { key: plain(code), raw: bytes.to_vec() }],
                "bytes {bytes:?}"
            );
        }
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[H"), vec![DecodedKey { key: plain(KeyCode::Home), raw: b"\x1b[H".to_vec() }]);
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[F"), vec![DecodedKey { key: plain(KeyCode::End), raw: b"\x1b[F".to_vec() }]);
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[Z"), vec![DecodedKey { key: plain(KeyCode::BTab), raw: b"\x1b[Z".to_vec() }]);
    }

    #[test]
    fn decode_meta_char() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1bx"),
            vec![DecodedKey {
                key: Key { code: KeyCode::Char('x'), ctrl: false, meta: true, shift: false },
                raw: b"\x1bx".to_vec(),
            }]
        );
    }

    #[test]
    fn decode_split_sequence_across_feeds() {
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b"), vec![]);
        assert_eq!(dec.feed(b"["), vec![]);
        assert_eq!(dec.feed(b"1;5"), vec![]);
        assert_eq!(
            dec.feed(b"A"),
            vec![DecodedKey {
                key: Key { code: KeyCode::Up, ctrl: true, meta: false, shift: false },
                raw: b"\x1b[1;5A".to_vec(),
            }]
        );
    }

    #[test]
    fn decode_lone_escape_flush() {
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b"), vec![]);
        assert_eq!(
            dec.flush(),
            vec![DecodedKey { key: plain(KeyCode::Escape), raw: vec![0x1b] }]
        );
    }

    #[test]
    fn decode_utf8_multibyte_char() {
        let mut dec = KeyDecoder::new();
        let bytes = "é".as_bytes();
        // Feed one byte at a time: no key until the sequence completes.
        assert_eq!(dec.feed(&bytes[..1]), vec![]);
        assert_eq!(
            dec.feed(&bytes[1..]),
            vec![DecodedKey { key: plain(KeyCode::Char('é')), raw: bytes.to_vec() }]
        );
    }
}
