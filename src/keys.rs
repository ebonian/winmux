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
//! - [`encode_mouse`]: `MouseEvent` -> VT bytes to FORWARD to a pane whose
//!   own application requested mouse reporting (SP7 Task 9) — the encoding
//!   half of [`KeyDecoder`]'s SGR mouse decoding.

use crate::grid::MouseEncoding;

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
    /// A synthesized tmux mouse pseudo-key name (`MouseDown1Pane`,
    /// `WheelUpStatus`, ...) -- Task 8, SP7 wave 3: table-driven mouse
    /// bindings (closes follow-ups #57, #67(a)/(b)). `0` is the conventional
    /// button placeholder for the two button-less kinds (`WheelUp`/
    /// `WheelDown` -- real tmux has no "WheelUp1", `docs/tmux-reference/
    /// mouse.md` §2.6); real button values are 1-3, matching the button
    /// numbering [`MouseKind`] already uses for press/drag/release. Carried
    /// in `Key.code` exactly like any other key so mouse pseudo-keys share
    /// `Key`'s existing modifier fields (`C-`/`M-`/`S-` prefixes parse/
    /// format identically, e.g. `C-MouseDown1Status`, matching real tmux's
    /// key-string.c grammar).
    MouseKey(MouseKeyKind, u8, MouseKeyLoc),
}

/// tmux's synthesized mouse pseudo-key TYPE (`docs/tmux-reference/mouse.md`
/// §2.7/§7). Only the types winmux's `server::dispatch` classification can
/// actually produce -- `MouseMove`/`SecondClick`/button numbers 6-11 are real
/// tmux key-string.c entries but never arise from winmux's SGR-1002 (button-
/// event, not all-motion) tracking, so they are not modeled.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MouseKeyKind {
    Down,
    Up,
    Drag,
    DragEnd,
    DoubleClick,
    TripleClick,
    WheelUp,
    WheelDown,
}

/// tmux's synthesized mouse pseudo-key LOCATION -- the subset winmux's
/// classification produces. The scrollbar/`ControlN` locations (post-3.5
/// tmux features per `docs/tmux-reference/mouse.md`'s vintage note) are out
/// of scope.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MouseKeyLoc {
    Pane,
    Border,
    Status,
    StatusLeft,
    StatusRight,
    StatusDefault,
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
    // Mouse pseudo-key names (`MouseDown1Pane`, `WheelUpStatus`, ...) are
    // multi-char tokens like the named keys above, so they must be tried
    // before the single-char `parse_char` fallback below (Task 8, SP7 wave
    // 3). Case-insensitive, matching real tmux's `strcasecmp`-based
    // `key_string_search_table` (`key-string.c`).
    let code = code.or_else(|| parse_mouse_key_name(rest));

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

/// Parse a tmux mouse pseudo-key name (`<Type><Button><Location>`, e.g.
/// `MouseDown1Pane`, `WheelUpStatus` -- no button digit for the two wheel
/// types). Case-insensitive. Longer/more-specific type prefixes are tried
/// first so `MouseDragEnd...` is never misparsed as `MouseDrag` + a bogus
/// `End...` location (`docs/tmux-reference/mouse.md` §2.7/§7 is the exact
/// name list this reproduces).
fn parse_mouse_key_name(s: &str) -> Option<KeyCode> {
    const TYPES: &[(&str, MouseKeyKind, bool)] = &[
        ("mousedragend", MouseKeyKind::DragEnd, true),
        ("mousedown", MouseKeyKind::Down, true),
        ("mouseup", MouseKeyKind::Up, true),
        ("mousedrag", MouseKeyKind::Drag, true),
        ("doubleclick", MouseKeyKind::DoubleClick, true),
        ("tripleclick", MouseKeyKind::TripleClick, true),
        ("wheelup", MouseKeyKind::WheelUp, false),
        ("wheeldown", MouseKeyKind::WheelDown, false),
    ];
    let lower = s.to_ascii_lowercase();
    for (prefix, kind, has_button) in TYPES {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let (button, loc_str) = if *has_button {
            let mut chars = rest.chars();
            let b = chars.next()?;
            if !('1'..='3').contains(&b) {
                return None; // this prefix is uniquely matched; a bad button
                             // digit is a malformed name, not "try the next
                             // candidate type" (no two type prefixes collide).
            }
            (b as u8 - b'0', &rest[1..])
        } else {
            (0u8, rest)
        };
        let loc = parse_mouse_loc(loc_str)?;
        return Some(KeyCode::MouseKey(*kind, button, loc));
    }
    None
}

fn parse_mouse_loc(s: &str) -> Option<MouseKeyLoc> {
    match s {
        "pane" => Some(MouseKeyLoc::Pane),
        "border" => Some(MouseKeyLoc::Border),
        "statusleft" => Some(MouseKeyLoc::StatusLeft),
        "statusright" => Some(MouseKeyLoc::StatusRight),
        "statusdefault" => Some(MouseKeyLoc::StatusDefault),
        "status" => Some(MouseKeyLoc::Status),
        _ => None,
    }
}

fn mouse_kind_str(k: MouseKeyKind) -> &'static str {
    match k {
        MouseKeyKind::Down => "MouseDown",
        MouseKeyKind::Up => "MouseUp",
        MouseKeyKind::Drag => "MouseDrag",
        MouseKeyKind::DragEnd => "MouseDragEnd",
        MouseKeyKind::DoubleClick => "DoubleClick",
        MouseKeyKind::TripleClick => "TripleClick",
        MouseKeyKind::WheelUp => "WheelUp",
        MouseKeyKind::WheelDown => "WheelDown",
    }
}

fn mouse_loc_str(l: MouseKeyLoc) -> &'static str {
    match l {
        MouseKeyLoc::Pane => "Pane",
        MouseKeyLoc::Border => "Border",
        MouseKeyLoc::Status => "Status",
        MouseKeyLoc::StatusLeft => "StatusLeft",
        MouseKeyLoc::StatusRight => "StatusRight",
        MouseKeyLoc::StatusDefault => "StatusDefault",
    }
}

/// `WheelUp`/`WheelDown` carry no button digit in tmux's naming (there is no
/// "WheelUp1") -- every other mouse-key type does.
fn mouse_kind_has_button(k: MouseKeyKind) -> bool {
    !matches!(k, MouseKeyKind::WheelUp | MouseKeyKind::WheelDown)
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
        KeyCode::MouseKey(kind, btn, loc) => {
            if mouse_kind_has_button(kind) {
                format!("{}{}{}", mouse_kind_str(kind), btn, mouse_loc_str(loc))
            } else {
                format!("{}{}", mouse_kind_str(kind), mouse_loc_str(loc))
            }
        }
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
        // A mouse pseudo-key isn't a byte sequence `send-keys` can write to a
        // pane -- same "unsupported combination" contract as ctrl-on-a-
        // named-key above.
        KeyCode::MouseKey(..) => None,
    }
}

/// One decoded key plus the exact raw bytes it came from (so an unbound key
/// can be forwarded to the pane verbatim).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecodedKey {
    pub key: Key,
    pub raw: Vec<u8>,
}

/// A decoded mouse event (Task 5, sub-project 4): SGR mouse reporting
/// (`CSI < Cb ; Cx ; Cy M` for press/drag/wheel, `CSI < Cb ; Cx ; Cy m` for
/// release), the only mouse protocol winmux enables (`\x1b[?1000h\x1b[?1002h
/// \x1b[?1006h` — normal + button-event tracking + SGR extended coordinates).
/// `x`/`y` are 0-based cell coordinates (SGR wire format is 1-based;
/// [`KeyDecoder`] converts on decode).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub x: u16,
    pub y: u16,
}

/// Which mouse action occurred, and which button (1 = left, 2 = middle,
/// 3 = right — SGR/xterm's 1-based button numbering, kept as-is rather than
/// 0-based to match wire values directly in tests/debug output).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Down(u8),
    Up(u8),
    /// Motion while a button is held (`?1002h` button-event tracking never
    /// reports motion with NO button down, so `Drag` always carries the
    /// held button number).
    Drag(u8),
    WheelUp,
    WheelDown,
}

/// One item [`KeyDecoder::feed`]/[`KeyDecoder::flush`] can produce: either a
/// decoded keystroke or a decoded mouse event, each carrying the exact raw
/// bytes it came from (contract change, Task 5 — see the `## input-v2`
/// section of `docs/specs/2026-07-07-command-config-interfaces.md`: this
/// generalizes the pre-Task-5 `Vec<DecodedKey>` return type. Minimal-churn
/// choice over a parallel `Vec<MouseEvent>` output, since callers already
/// process decoder output as one ordered stream of "things that happened",
/// and interleaving order between keys and mouse events matters (e.g. a
/// click followed immediately by a keystroke)).
///
/// RAW-BYTE FIDELITY / consume-always decision: any complete SGR mouse
/// sequence the decoder recognizes (`buf[2] == b'<'` right after `ESC [`) is
/// ALWAYS decoded as `Mouse`, regardless of whether winmux's `mouse` option
/// is currently on. The client only ever emits these bytes because winmux
/// itself sent the xterm mouse-mode enable sequences to it, so a decodable
/// mouse sequence arriving is never a coincidental byte collision with
/// something the user typed — dropping/ignoring a decoded `Mouse` event when
/// `mouse` is off is the SERVER's job (`server::dispatch::dispatch_mouse`),
/// not the decoder's; the decoder's contract is purely "what bytes did this
/// decode to", independent of any runtime option.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodedInput {
    Key(DecodedKey),
    Mouse { event: MouseEvent, raw: Vec<u8> },
}

/// Incremental VT-input decoder: turns a stream of client keystroke/mouse
/// bytes into [`DecodedInput`] values. Escape sequences (and partial UTF-8
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

    /// Feed more input bytes, returning every key/mouse item that became
    /// complete as a result (zero or more; an in-progress escape/UTF-8/mouse
    /// sequence produces none until it completes). Buffering is bounded: a
    /// sequence that grows past [`MAX_PENDING`] bytes without completing has
    /// its head byte peeled off as a best-effort key and the remainder
    /// reprocessed, so a misbehaving byte stream can never stall the decoder
    /// forever.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<DecodedInput> {
        let mut out = Vec::new();
        // Worklist rather than a plain iterator: a bound-exceeded peel
        // re-queues the buffered remainder at the FRONT so it is reprocessed
        // (in order) before any not-yet-consumed new input.
        let mut queue: std::collections::VecDeque<u8> = bytes.iter().copied().collect();
        while let Some(b) = queue.pop_front() {
            self.pending.push(b);
            if let Some(item) = classify(&self.pending) {
                out.push(finish(item, std::mem::take(&mut self.pending)));
            } else if self.pending.len() > MAX_PENDING {
                // Runaway sequence (e.g. a CSI that never gets a final
                // byte): peel the head byte as a best-effort key and
                // reprocess the rest as fresh tokens.
                let mut rest = std::mem::take(&mut self.pending);
                let first = rest.remove(0);
                out.push(DecodedInput::Key(DecodedKey {
                    key: peel_byte_key(first),
                    raw: vec![first],
                }));
                for &rb in rest.iter().rev() {
                    queue.push_front(rb);
                }
            }
        }
        out
    }

    /// Drain any incomplete trailing buffer as best-effort keys. Call this
    /// when no more input is coming (e.g. end of a read) so a lone `ESC` or
    /// a truncated escape sequence still produces something rather than
    /// silently vanishing.
    ///
    /// Works exactly like an incremental re-feed of the leftover bytes: the
    /// shortest classifiable prefix is emitted as an item (raw = exactly its
    /// own bytes); when the head byte can make no progress it is peeled
    /// alone (lone `ESC` -> `Escape`, anything else -> its single-byte
    /// classification). The concatenation of all emitted `raw`s — across
    /// `feed` and `flush` — is always exactly the input byte stream.
    pub fn flush(&mut self) -> Vec<DecodedInput> {
        let mut out = Vec::new();
        while !self.pending.is_empty() {
            // Shortest prefix that classifies == what feed() would have
            // emitted had these bytes arrived one at a time.
            let matched = (1..=self.pending.len())
                .find_map(|end| classify(&self.pending[..end]).map(|c| (c, end)));
            match matched {
                Some((item, end)) => {
                    let rest = self.pending.split_off(end);
                    let raw = std::mem::replace(&mut self.pending, rest);
                    out.push(finish(item, raw));
                }
                None => {
                    // Head token can't complete with no more bytes coming:
                    // peel exactly one byte and keep draining the rest.
                    let first = self.pending.remove(0);
                    out.push(DecodedInput::Key(DecodedKey {
                        key: peel_byte_key(first),
                        raw: vec![first],
                    }));
                }
            }
        }
        out
    }

    /// `true` iff there is a nonempty pending (not-yet-classified) buffer
    /// whose FIRST byte is `ESC` (0x1b) — an outstanding escape-introduced
    /// sequence (including a bare trailing `ESC` with nothing after it yet)
    /// that `feed()` hasn't been able to complete. Pure query, no clock: the
    /// decoder itself has no notion of time (per this module's docs) — this
    /// is the primitive `input::KeyMachine`'s escape-time flush logic
    /// (sub-project 4, Task 9) is built on: the CALLER records when this
    /// first became true and, once its own clock says `escape-time` has
    /// elapsed with no further bytes arriving, calls [`KeyDecoder::flush`]
    /// (indirectly, via `KeyMachine::flush_now`) to force-resolve it.
    pub fn pending_starts_with_escape(&self) -> bool {
        self.pending.first() == Some(&0x1b)
    }
}

/// Internal classification result before the raw bytes (known only to the
/// `feed`/`flush` callers, which own `self.pending`) are attached.
enum Classified {
    Key(Key),
    Mouse(MouseEvent),
}

fn finish(c: Classified, raw: Vec<u8>) -> DecodedInput {
    match c {
        Classified::Key(key) => DecodedInput::Key(DecodedKey { key, raw }),
        Classified::Mouse(event) => DecodedInput::Mouse { event, raw },
    }
}

/// Buffering bound for [`KeyDecoder`]: no real key sequence handled here is
/// anywhere near this long, so exceeding it means the stream is not a key
/// sequence at all and the buffer is force-drained byte by byte.
const MAX_PENDING: usize = 32;

/// Best-effort classification of a single peeled byte (bound-exceeded feed,
/// or flush of an incomplete tail): a lone ESC is the `Escape` key; anything
/// else gets its ordinary single-byte decoding.
fn peel_byte_key(b: u8) -> Key {
    if b == 0x1b {
        plain(KeyCode::Escape)
    } else {
        classify_single_byte(b)
    }
}

/// Try to decode a complete key or mouse event from `buf` (which always
/// starts at a fresh token boundary). `None` means "valid so far, need more
/// bytes".
fn classify(buf: &[u8]) -> Option<Classified> {
    match buf[0] {
        0x1b => classify_escape(buf),
        0xc0..=0xdf => classify_utf8(buf, 2).map(Classified::Key),
        0xe0..=0xef => classify_utf8(buf, 3).map(Classified::Key),
        0xf0..=0xf7 => classify_utf8(buf, 4).map(Classified::Key),
        b => Some(Classified::Key(classify_single_byte(b))),
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

fn classify_escape(buf: &[u8]) -> Option<Classified> {
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
                other => return Some(Classified::Key(plain(KeyCode::Char(other as char)))),
            };
            Some(Classified::Key(plain(code)))
        }
        _ => {
            // ESC <token>: Meta + whatever the WHOLE remainder decodes to
            // (tmux applies Meta to the decoded key: ESC ESC [ A = M-Up,
            // ESC + UTF-8 char = M-<char>). Classifying only the first
            // remainder byte would return None forever when that byte is
            // itself a multi-byte introducer (another ESC, a UTF-8 lead) —
            // permanently stalling the decoder. `None` here means the
            // remainder is still incomplete: keep buffering; it may
            // complete on a later feed() or get peeled by flush()/the
            // MAX_PENDING bound. A mouse sequence is never legitimately
            // ESC-prefixed on the wire, but Meta is applied uniformly for
            // defensiveness rather than dropping the event.
            classify(&buf[1..]).map(|c| match c {
                Classified::Key(mut k) => {
                    k.meta = true;
                    Classified::Key(k)
                }
                Classified::Mouse(mut m) => {
                    m.meta = true;
                    Classified::Mouse(m)
                }
            })
        }
    }
}

fn classify_csi(buf: &[u8]) -> Option<Classified> {
    // SGR mouse reporting: `CSI < Cb ; Cx ; Cy (M|m)`. Must be checked before
    // the generic final-byte scan below, since `<` (0x3C) is not itself a
    // CSI final byte -- without this special case the scan would run past it
    // looking for M/m and misparse the whole sequence as a bogus `Char('M')`/
    // `Char('m')` key carrying the entire mouse sequence as `raw`.
    if buf.len() >= 3 && buf[2] == b'<' {
        return classify_sgr_mouse(buf).map(Classified::Mouse);
    }
    let mut idx = 2;
    while idx < buf.len() {
        let b = buf[idx];
        if (0x40..=0x7e).contains(&b) {
            let params_str = std::str::from_utf8(&buf[2..idx]).unwrap_or("");
            return Some(Classified::Key(
                parse_csi(params_str, b).unwrap_or_else(|| plain(KeyCode::Char(b as char))),
            ));
        }
        idx += 1;
    }
    None // no final byte yet: still incomplete
}

/// Decode `CSI < Cb ; Cx ; Cy (M|m)` (buf[0..3] == `ESC [ <`) into a
/// [`MouseEvent`]. `None` while no `M`/`m` final byte has arrived yet (still
/// incomplete — keep buffering). A malformed body (non-digit/`;` byte before
/// a final byte, or unparseable numbers) degrades to `None` forever for that
/// exact buffer state; the caller's [`MAX_PENDING`] bound guarantees this
/// can't stall the decoder — a malformed sequence eventually gets peeled byte
/// by byte like any other runaway sequence.
fn classify_sgr_mouse(buf: &[u8]) -> Option<MouseEvent> {
    let mut idx = 3;
    while idx < buf.len() {
        let b = buf[idx];
        if b == b'M' || b == b'm' {
            let params = std::str::from_utf8(&buf[3..idx]).ok()?;
            let mut parts = params.split(';');
            let cb: i64 = parts.next()?.parse().ok()?;
            let cx: i64 = parts.next()?.parse().ok()?;
            let cy: i64 = parts.next()?.parse().ok()?;
            if parts.next().is_some() {
                return None; // unexpected extra field: malformed
            }
            let released = b == b'm';
            let kind = mouse_kind_from_cb(cb, released)?;
            return Some(MouseEvent {
                kind,
                shift: cb & 0x04 != 0,
                meta: cb & 0x08 != 0,
                ctrl: cb & 0x10 != 0,
                x: (cx - 1).max(0) as u16,
                y: (cy - 1).max(0) as u16,
            });
        }
        if !(b.is_ascii_digit() || b == b';') {
            return None; // malformed body: never completes from here
        }
        idx += 1;
    }
    None // no M/m yet: still incomplete
}

/// xterm/SGR `Cb` decoding: bit 0x40 marks a wheel event (low 2 bits pick
/// up/down); bit 0x20 marks motion (a held-button drag); otherwise low 2
/// bits + 1 give the 1-based button number, and press vs release comes from
/// the caller's `M`/`m` final byte.
fn mouse_kind_from_cb(cb: i64, released: bool) -> Option<MouseKind> {
    let low = (cb & 0x3) as u8;
    if cb & 0x40 != 0 {
        Some(if low == 0 { MouseKind::WheelUp } else { MouseKind::WheelDown })
    } else {
        let button = low + 1;
        if cb & 0x20 != 0 {
            Some(MouseKind::Drag(button))
        } else if released {
            Some(MouseKind::Up(button))
        } else {
            Some(MouseKind::Down(button))
        }
    }
}

fn mods_from_param(p: i64) -> (bool, bool, bool) {
    let bits = (p - 1).max(0) as u8;
    (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0) // (shift, meta, ctrl)
}

/// Reconstruct the xterm/SGR `Cb` button-word from a decoded [`MouseEvent`]
/// — the exact inverse of [`mouse_kind_from_cb`] plus the modifier-bit
/// packing `classify_sgr_mouse` reads: low 2 bits + bit 0x20 pick the
/// button/drag encoding (button number stored as `button - 1`, matching
/// `mouse_kind_from_cb`'s `low + 1`), bit 0x40 marks a wheel event (low bit
/// then picks up/down), and 0x04/0x08/0x10 carry shift/meta/ctrl — SAME
/// layout `classify_sgr_mouse` decodes, so `cb_for(classify_sgr_mouse(..))
/// == cb` round-trips for any legally-decoded event (verified by
/// `encode_roundtrip_via_decoder`-style tests below).
fn cb_for(ev: &MouseEvent) -> i64 {
    let mut cb: i64 = match ev.kind {
        MouseKind::Down(b) | MouseKind::Up(b) => (b.saturating_sub(1)) as i64,
        MouseKind::Drag(b) => (b.saturating_sub(1)) as i64 | 0x20,
        MouseKind::WheelUp => 0x40,
        MouseKind::WheelDown => 0x40 | 0x01,
    };
    if ev.shift {
        cb |= 0x04;
    }
    if ev.meta {
        cb |= 0x08;
    }
    if ev.ctrl {
        cb |= 0x10;
    }
    cb
}

/// Re-encode a decoded [`MouseEvent`] (coordinates already pane-relative and
/// 0-based — the caller's job, see `server::dispatch::forward_mouse_to_pane`)
/// into the wire bytes a pane application requesting `encoding` expects —
/// the forwarding half of Task 9 (SP7, closes follow-ups #35/#72): tmux's
/// `input_key_get_mouse` (`input-keys.c:713-793`) re-encodes per the pane's
/// own requested protocol rather than replaying the outer terminal's raw
/// bytes verbatim, since the pane's coordinate origin and requested wire
/// format can both differ from the client's.
pub fn encode_mouse(ev: &MouseEvent, encoding: MouseEncoding) -> Vec<u8> {
    match encoding {
        MouseEncoding::Sgr => encode_mouse_sgr(ev),
        MouseEncoding::Utf8 => encode_mouse_utf8(ev),
        MouseEncoding::Default => encode_mouse_x10(ev),
    }
}

/// `CSI < Cb ; Cx ; Cy (M|m)` — SGR (1006) encoding, unbounded coordinate
/// range (1-based, no clamp). `M` for every event except a release
/// (`MouseKind::Up`), which uses `m` — the exact inverse of
/// `classify_sgr_mouse`'s `released = b == b'm'` check.
fn encode_mouse_sgr(ev: &MouseEvent) -> Vec<u8> {
    let cb = cb_for(ev);
    let final_byte = if matches!(ev.kind, MouseKind::Up(_)) { 'm' } else { 'M' };
    format!("\x1b[<{cb};{};{}{final_byte}", ev.x as u32 + 1, ev.y as u32 + 1).into_bytes()
}

/// `CSI M <Cb+32> <Cx+32> <Cy+32>` — legacy X10/xterm-normal encoding
/// (DECSET 1000, the `MouseEncoding::Default` fallback when neither 1005
/// nor 1006 was requested): three raw bytes, 1-based coordinates offset by
/// 32 and clamped at 223 (byte value 255) since a single byte can't carry a
/// coordinate past `255 - 32`, per the task brief ("X10 encoding caps at
/// 223") and real xterm's own `MOUSE_PARAM_POS_OFF`/byte-width limit
/// (`docs/tmux-reference/mouse.md` §1's decode-side mirror: `x = byte -
/// 33`, i.e. encode is `byte = x + 33` — 32 for the offset, 1 more for the
/// 0-based-to-1-based conversion `classify_sgr_mouse`'s own `(cx - 1)`
/// undoes on decode). No release-button ambiguity here (unlike real X10,
/// which can't distinguish which button released) since [`MouseEvent`]
/// always carries the concrete kind/button already.
fn encode_mouse_x10(ev: &MouseEvent) -> Vec<u8> {
    let cb = cb_for(ev).clamp(0, 255 - 32) as u8;
    let cx = ((ev.x as u32 + 1).min(223)) as u8;
    let cy = ((ev.y as u32 + 1).min(223)) as u8;
    vec![0x1b, b'[', b'M', cb.wrapping_add(32), cx.wrapping_add(32), cy.wrapping_add(32)]
}

/// `CSI M <Cb+32> <Cx+32 as UTF-8> <Cy+32 as UTF-8>` — UTF-8 extended
/// coordinates (DECSET 1005): same button byte as legacy X10 (button/
/// modifier words never approach the 223 cap in practice), but `Cx+32`/
/// `Cy+32` are emitted as UTF-8-encoded codepoints instead of raw bytes,
/// extending the usable coordinate range well past 223 (capped here at
/// 2015 = 2047 - 32, xterm's own ctlseqs-documented 1005 limit) — matches
/// real xterm/tmux's 1005 extension, though no `docs/tmux-reference/*.md`
/// source line pins the exact byte-for-byte algorithm (1005 is a rare,
/// largely-superseded-by-1006 protocol; this is a best-effort, clearly
/// peripheral encoding, not exercised by winmux's own default `?1006h`-
/// only enable sequence).
fn encode_mouse_utf8(ev: &MouseEvent) -> Vec<u8> {
    let cb = cb_for(ev).clamp(0, 255 - 32) as u8;
    let cx = (ev.x as u32 + 1).min(2015) + 32;
    let cy = (ev.y as u32 + 1).min(2015) + 32;
    let mut out = vec![0x1b, b'[', b'M', cb.wrapping_add(32)];
    if let Some(c) = char::from_u32(cx) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    if let Some(c) = char::from_u32(cy) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    out
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

    // ---- mouse pseudo-key names (Task 8, SP7 wave 3) ----

    #[test]
    fn parse_mouse_key_names_roundtrip() {
        let button_kinds: &[MouseKeyKind] = &[
            MouseKeyKind::Down,
            MouseKeyKind::Up,
            MouseKeyKind::Drag,
            MouseKeyKind::DragEnd,
            MouseKeyKind::DoubleClick,
            MouseKeyKind::TripleClick,
        ];
        let wheel_kinds: &[MouseKeyKind] = &[MouseKeyKind::WheelUp, MouseKeyKind::WheelDown];
        let locs: &[MouseKeyLoc] = &[
            MouseKeyLoc::Pane,
            MouseKeyLoc::Border,
            MouseKeyLoc::Status,
            MouseKeyLoc::StatusLeft,
            MouseKeyLoc::StatusRight,
            MouseKeyLoc::StatusDefault,
        ];
        for &kind in button_kinds {
            for btn in 1..=3u8 {
                for &loc in locs {
                    let key = Key { code: KeyCode::MouseKey(kind, btn, loc), ctrl: false, meta: false, shift: false };
                    let name = key_name(&key);
                    assert_eq!(parse_key(&name), Some(key), "round-trip of {name:?}");
                }
            }
        }
        for &kind in wheel_kinds {
            for &loc in locs {
                let key = Key { code: KeyCode::MouseKey(kind, 0, loc), ctrl: false, meta: false, shift: false };
                let name = key_name(&key);
                assert_eq!(parse_key(&name), Some(key), "round-trip of {name:?}");
            }
        }

        // Exact canonical spellings.
        assert_eq!(
            key_name(&Key { code: KeyCode::MouseKey(MouseKeyKind::Down, 1, MouseKeyLoc::Pane), ctrl: false, meta: false, shift: false }),
            "MouseDown1Pane"
        );
        assert_eq!(
            key_name(&Key {
                code: KeyCode::MouseKey(MouseKeyKind::DragEnd, 1, MouseKeyLoc::Pane),
                ctrl: false,
                meta: false,
                shift: false
            }),
            "MouseDragEnd1Pane"
        );
        assert_eq!(
            key_name(&Key { code: KeyCode::MouseKey(MouseKeyKind::WheelUp, 0, MouseKeyLoc::Status), ctrl: false, meta: false, shift: false }),
            "WheelUpStatus"
        );

        // Case-insensitive parse (tmux's `strcasecmp`-based lookup).
        assert_eq!(
            parse_key("mousedown1pane"),
            Some(Key { code: KeyCode::MouseKey(MouseKeyKind::Down, 1, MouseKeyLoc::Pane), ctrl: false, meta: false, shift: false })
        );

        // Modifier prefixes combine with mouse keys exactly like any other key.
        assert_eq!(
            parse_key("C-MouseDown1Status"),
            Some(Key { code: KeyCode::MouseKey(MouseKeyKind::Down, 1, MouseKeyLoc::Status), ctrl: true, meta: false, shift: false })
        );
    }

    #[test]
    fn invalid_mouse_name_rejected() {
        assert_eq!(parse_key("MouseDown4Pane"), None, "button out of 1-3 range");
        assert_eq!(parse_key("MouseDown1Nowhere"), None, "unrecognized location");
        assert_eq!(parse_key("MouseClickPane"), None, "unrecognized type");
        assert_eq!(parse_key("WheelUp1Pane"), None, "wheel types carry no button digit");
        assert_eq!(parse_key("MouseDown"), None, "type with no button/location at all");
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
            match &decoded[0] {
                DecodedInput::Key(dk) => {
                    assert_eq!(dk.key, k, "round-trip of {k:?}");
                    assert_eq!(dk.raw, bytes);
                }
                other => panic!("expected a Key item, got {other:?}"),
            }
        }
    }

    // ---- KeyDecoder ----

    /// Helper: build the `DecodedInput::Key` wrapper most decoder tests
    /// assert against (raw bytes come along for the ride, so an unbound key
    /// can still be forwarded verbatim).
    fn dk(key: Key, raw: &[u8]) -> DecodedInput {
        DecodedInput::Key(DecodedKey { key, raw: raw.to_vec() })
    }

    #[test]
    fn decode_plain_bytes() {
        let mut dec = KeyDecoder::new();
        let out = dec.feed(b"ab");
        assert_eq!(
            out,
            vec![
                dk(plain(KeyCode::Char('a')), b"a"),
                dk(plain(KeyCode::Char('b')), b"b"),
            ]
        );
    }

    #[test]
    fn decode_ctrl_bytes() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(&[0x03]),
            vec![dk(Key { code: KeyCode::Char('c'), ctrl: true, meta: false, shift: false }, &[0x03])]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(&[0x0d]), vec![dk(plain(KeyCode::Enter), &[0x0d])]);
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(&[0x09]), vec![dk(plain(KeyCode::Tab), &[0x09])]);
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(&[0x7f]), vec![dk(plain(KeyCode::BSpace), &[0x7f])]);
    }

    #[test]
    fn decode_prefix_byte_is_ctrl_b() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(&[0x02]),
            vec![dk(Key { code: KeyCode::Char('b'), ctrl: true, meta: false, shift: false }, &[0x02])]
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
            assert_eq!(dec.feed(bytes), vec![dk(plain(code), bytes)], "bytes {bytes:?}");
        }
    }

    #[test]
    fn decode_csi_modified() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1b[1;5A"),
            vec![dk(Key { code: KeyCode::Up, ctrl: true, meta: false, shift: false }, b"\x1b[1;5A")]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1b[1;2A"),
            vec![dk(Key { code: KeyCode::Up, ctrl: false, meta: false, shift: true }, b"\x1b[1;2A")]
        );
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1b[1;3A"),
            vec![dk(Key { code: KeyCode::Up, ctrl: false, meta: true, shift: false }, b"\x1b[1;3A")]
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
            assert_eq!(dec.feed(bytes), vec![dk(plain(KeyCode::F(n)), bytes)]);
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
            assert_eq!(dec.feed(bytes), vec![dk(plain(code), bytes)], "bytes {bytes:?}");
        }
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[H"), vec![dk(plain(KeyCode::Home), b"\x1b[H")]);
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[F"), vec![dk(plain(KeyCode::End), b"\x1b[F")]);
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[Z"), vec![dk(plain(KeyCode::BTab), b"\x1b[Z")]);
    }

    #[test]
    fn decode_meta_char() {
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1bx"),
            vec![dk(Key { code: KeyCode::Char('x'), ctrl: false, meta: true, shift: false }, b"\x1bx")]
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
            vec![dk(Key { code: KeyCode::Up, ctrl: true, meta: false, shift: false }, b"\x1b[1;5A")]
        );
    }

    #[test]
    fn decode_lone_escape_flush() {
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b"), vec![]);
        assert_eq!(dec.flush(), vec![dk(plain(KeyCode::Escape), &[0x1b])]);
    }

    #[test]
    fn pending_starts_with_escape_tracks_buffer_state() {
        let mut dec = KeyDecoder::new();
        assert!(!dec.pending_starts_with_escape(), "fresh decoder has no pending buffer");
        assert_eq!(dec.feed(b"\x1b"), vec![]);
        assert!(dec.pending_starts_with_escape(), "lone ESC left pending");
        // A burst that completes within the SAME feed() call never leaves a
        // pending ESC behind.
        let mut dec2 = KeyDecoder::new();
        assert_eq!(dec2.feed(b"\x1b[A").len(), 1);
        assert!(!dec2.pending_starts_with_escape());
        // A non-ESC pending buffer (e.g. a still-buffering UTF-8 lead byte)
        // must not report true.
        let mut dec3 = KeyDecoder::new();
        assert_eq!(dec3.feed(&[0xc3]), vec![]);
        assert!(!dec3.pending_starts_with_escape());
        // flush() drains the pending buffer, clearing the flag.
        assert_eq!(dec.flush().len(), 1);
        assert!(!dec.pending_starts_with_escape());
    }

    #[test]
    fn decode_esc_then_arrow_is_meta_up() {
        // tmux decodes an ESC-prefixed sequence as Meta on the DECODED key:
        // ESC ESC [ A = M-Up (e.g. vim leave-insert immediately followed by
        // an arrow key). Must be ONE key with raw = all four bytes.
        let mut dec = KeyDecoder::new();
        assert_eq!(
            dec.feed(b"\x1b\x1b[A"),
            vec![dk(Key { code: KeyCode::Up, ctrl: false, meta: true, shift: false }, b"\x1b\x1b[A")]
        );
        // Decoder must be fully usable afterwards (regression: the old code
        // stalled forever after ESC ESC).
        assert_eq!(dec.feed(b"a"), vec![dk(plain(KeyCode::Char('a')), b"a")]);
    }

    #[test]
    fn decode_esc_then_split_arrow_across_feeds() {
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b\x1b"), vec![]); // incomplete: keep buffering
        assert_eq!(
            dec.feed(b"[A"),
            vec![dk(Key { code: KeyCode::Up, ctrl: false, meta: true, shift: false }, b"\x1b\x1b[A")]
        );
    }

    #[test]
    fn decode_esc_then_utf8() {
        let mut dec = KeyDecoder::new();
        let mut bytes = vec![0x1b];
        bytes.extend("é".as_bytes()); // 0xc3 0xa9
        assert_eq!(dec.feed(&bytes[..2]), vec![]); // ESC + UTF-8 lead: buffer
        assert_eq!(
            dec.feed(&bytes[2..]),
            vec![dk(Key { code: KeyCode::Char('é'), ctrl: false, meta: true, shift: false }, &bytes)]
        );
    }

    #[test]
    fn flush_truncated_csi_peels_all_bytes() {
        // A CSI truncated at end of stream must peel EVERY byte as its own
        // best-effort key — no byte silently swallowed into another key's raw.
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[1;5"), vec![]);
        assert_eq!(
            dec.flush(),
            vec![
                dk(plain(KeyCode::Escape), &[0x1b]),
                dk(plain(KeyCode::Char('[')), b"["),
                dk(plain(KeyCode::Char('1')), b"1"),
                dk(plain(KeyCode::Char(';')), b";"),
                dk(plain(KeyCode::Char('5')), b"5"),
            ]
        );
    }

    /// Extract the raw bytes of a [`DecodedInput`] item, regardless of
    /// variant — used by the raw-byte-fidelity property tests below.
    fn item_raw(item: &DecodedInput) -> &[u8] {
        match item {
            DecodedInput::Key(dk) => &dk.raw,
            DecodedInput::Mouse { raw, .. } => raw,
        }
    }

    #[test]
    fn flush_preserves_raw_concatenation() {
        // Property: for any input, the concatenation of every emitted item's
        // raw bytes (feed + flush) is exactly the input — nothing dropped.
        let nasty: &[&[u8]] = &[
            b"\x1b[1;5",        // truncated modified-CSI
            b"\x1b\x1b",        // double ESC
            b"\x1b[",           // bare CSI introducer
            b"\x1bO",           // bare SS3 introducer
            &[0x1b, 0xc3],      // ESC + partial UTF-8
            &[0xc3],            // lone partial UTF-8
            b"\x1b[1;5A\x1b[2", // complete key then truncated tail
            b"abc\x1b",         // text then lone ESC
            b"\x1b[<0;5;10M",   // complete SGR mouse press
            b"\x1b[<0;5",       // truncated SGR mouse
        ];
        for input in nasty {
            let mut dec = KeyDecoder::new();
            let mut items = dec.feed(input);
            items.extend(dec.flush());
            let concat: Vec<u8> = items.iter().flat_map(|i| item_raw(i).to_vec()).collect();
            assert_eq!(&concat, input, "raw bytes dropped for input {input:?}");
        }
    }

    #[test]
    fn decode_runaway_csi_is_bounded() {
        // A "CSI" that never terminates must not buffer forever: once the
        // pending buffer exceeds the bound, bytes are peeled as best-effort
        // keys and nothing is lost.
        let mut dec = KeyDecoder::new();
        let mut input = b"\x1b[".to_vec();
        input.extend(std::iter::repeat_n(b';', 64));
        let mut items = dec.feed(&input);
        items.extend(dec.flush());
        assert!(!items.is_empty(), "runaway CSI produced no items at all");
        let concat: Vec<u8> = items.iter().flat_map(|i| item_raw(i).to_vec()).collect();
        assert_eq!(concat, input, "runaway CSI dropped bytes");
    }

    #[test]
    fn decode_utf8_multibyte_char() {
        let mut dec = KeyDecoder::new();
        let bytes = "é".as_bytes();
        // Feed one byte at a time: no key until the sequence completes.
        assert_eq!(dec.feed(&bytes[..1]), vec![]);
        assert_eq!(dec.feed(&bytes[1..]), vec![dk(plain(KeyCode::Char('é')), bytes)]);
    }

    // ---- SGR mouse decoding (Task 5, sub-project 4) ----

    fn dm(kind: MouseKind, x: u16, y: u16, raw: &[u8]) -> DecodedInput {
        DecodedInput::Mouse {
            event: MouseEvent { kind, ctrl: false, meta: false, shift: false, x, y },
            raw: raw.to_vec(),
        }
    }

    #[test]
    fn decode_sgr_mouse_press() {
        // Button 1 (left) press at 1-based (6,11) -> 0-based (5,10).
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[<0;6;11M"), vec![dm(MouseKind::Down(1), 5, 10, b"\x1b[<0;6;11M")]);

        // Button 3 (right) press.
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[<2;1;1M"), vec![dm(MouseKind::Down(3), 0, 0, b"\x1b[<2;1;1M")]);
    }

    #[test]
    fn decode_sgr_mouse_release() {
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[<0;6;11m"), vec![dm(MouseKind::Up(1), 5, 10, b"\x1b[<0;6;11m")]);
    }

    #[test]
    fn decode_sgr_mouse_drag() {
        // Bit 0x20 set on top of button 1 (Cb = 32) marks button-motion.
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[<32;10;5M"), vec![dm(MouseKind::Drag(1), 9, 4, b"\x1b[<32;10;5M")]);
    }

    #[test]
    fn decode_sgr_mouse_wheel() {
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[<64;3;3M"), vec![dm(MouseKind::WheelUp, 2, 2, b"\x1b[<64;3;3M")]);
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[<65;3;3M"), vec![dm(MouseKind::WheelDown, 2, 2, b"\x1b[<65;3;3M")]);
    }

    #[test]
    fn decode_sgr_mouse_modifiers() {
        // Cb = button 1 (0) | shift(0x04) | meta(0x08) | ctrl(0x10) = 0x1c = 28.
        let mut dec = KeyDecoder::new();
        let got = dec.feed(b"\x1b[<28;1;1M");
        assert_eq!(
            got,
            vec![DecodedInput::Mouse {
                event: MouseEvent { kind: MouseKind::Down(1), ctrl: true, meta: true, shift: true, x: 0, y: 0 },
                raw: b"\x1b[<28;1;1M".to_vec(),
            }]
        );
    }

    #[test]
    fn decode_sgr_mouse_split_across_feeds() {
        let mut dec = KeyDecoder::new();
        assert_eq!(dec.feed(b"\x1b[<"), vec![]);
        assert_eq!(dec.feed(b"0;6;"), vec![]);
        assert_eq!(dec.feed(b"11"), vec![]);
        assert_eq!(dec.feed(b"M"), vec![dm(MouseKind::Down(1), 5, 10, b"\x1b[<0;6;11M")]);
    }

    #[test]
    fn decode_sgr_mouse_then_key_in_same_feed() {
        // A mouse sequence immediately followed by an ordinary key in the
        // same feed() call: both must decode, in order, as separate items.
        let mut dec = KeyDecoder::new();
        let mut input = b"\x1b[<0;6;11M".to_vec();
        input.push(b'a');
        assert_eq!(
            dec.feed(&input),
            vec![dm(MouseKind::Down(1), 5, 10, b"\x1b[<0;6;11M"), dk(plain(KeyCode::Char('a')), b"a")]
        );
    }

    // ---- encode_mouse (SP7 Task 9 -- closes follow-ups #35/#72) ----

    fn me(kind: MouseKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind, ctrl: false, meta: false, shift: false, x, y }
    }

    #[test]
    fn encode_mouse_sgr_press_matches_wire_format() {
        // 0-based (5, 10) pane-relative -> 1-based (6, 11) on the wire.
        let bytes = encode_mouse(&me(MouseKind::Down(1), 5, 10), MouseEncoding::Sgr);
        assert_eq!(bytes, b"\x1b[<0;6;11M");
    }

    #[test]
    fn encode_mouse_sgr_release_uses_lowercase_m() {
        let bytes = encode_mouse(&me(MouseKind::Up(1), 5, 10), MouseEncoding::Sgr);
        assert_eq!(bytes, b"\x1b[<0;6;11m");
    }

    #[test]
    fn encode_mouse_sgr_drag_sets_motion_bit() {
        let bytes = encode_mouse(&me(MouseKind::Drag(1), 9, 4), MouseEncoding::Sgr);
        assert_eq!(bytes, b"\x1b[<32;10;5M");
    }

    #[test]
    fn encode_mouse_sgr_wheel() {
        assert_eq!(encode_mouse(&me(MouseKind::WheelUp, 2, 2), MouseEncoding::Sgr), b"\x1b[<64;3;3M");
        assert_eq!(encode_mouse(&me(MouseKind::WheelDown, 2, 2), MouseEncoding::Sgr), b"\x1b[<65;3;3M");
    }

    #[test]
    fn encode_mouse_sgr_carries_modifiers() {
        let ev = MouseEvent { kind: MouseKind::Down(1), ctrl: true, meta: true, shift: true, x: 0, y: 0 };
        // cb = 0 (button 1) | 0x04 (shift) | 0x08 (meta) | 0x10 (ctrl) = 28.
        assert_eq!(encode_mouse(&ev, MouseEncoding::Sgr), b"\x1b[<28;1;1M");
    }

    #[test]
    fn encode_mouse_sgr_roundtrips_through_decoder() {
        // encode_mouse's SGR output, fed back into KeyDecoder, must decode to
        // the exact same MouseEvent it was built from -- the inverse-function
        // property `cb_for`'s doc comment claims.
        for ev in [
            me(MouseKind::Down(1), 5, 10),
            me(MouseKind::Down(3), 0, 0),
            me(MouseKind::Up(1), 5, 10),
            me(MouseKind::Drag(1), 9, 4),
            me(MouseKind::WheelUp, 2, 2),
            me(MouseKind::WheelDown, 2, 2),
        ] {
            let bytes = encode_mouse(&ev, MouseEncoding::Sgr);
            let mut dec = KeyDecoder::new();
            let items = dec.feed(&bytes);
            assert_eq!(items, vec![dm(ev.kind, ev.x, ev.y, &bytes)], "roundtrip failed for {ev:?}");
        }
    }

    #[test]
    fn encode_mouse_x10_offsets_by_32_and_caps_at_223() {
        // 0-based (5, 10) -> 1-based (6, 11) -> +32 offset -> bytes (38, 43).
        let bytes = encode_mouse(&me(MouseKind::Down(1), 5, 10), MouseEncoding::Default);
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 6 + 32, 11 + 32]);

        // A coordinate past the 223 cap clamps rather than overflowing the
        // single wire byte (task brief: "X10 encoding caps at 223").
        let bytes = encode_mouse(&me(MouseKind::Down(1), 500, 500), MouseEncoding::Default);
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 223 + 32, 223 + 32]);
    }

    #[test]
    fn encode_mouse_utf8_extends_range_past_x10_cap() {
        // A coordinate that would clamp under X10 (500 > 223) survives
        // uncapped under the UTF-8 (1005) encoding -- decode the emitted
        // UTF-8 codepoints back into 0-based coordinates and check they
        // match, rather than pinning exact byte sequences.
        let bytes = encode_mouse(&me(MouseKind::Down(1), 500, 500), MouseEncoding::Utf8);
        let s = std::str::from_utf8(&bytes[4..]).unwrap();
        let mut chars = s.chars();
        let cx = chars.next().unwrap() as u32 - 32 - 1;
        let cy = chars.next().unwrap() as u32 - 32 - 1;
        assert_eq!((cx, cy), (500, 500));
    }
}
