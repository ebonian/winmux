//! Table-driven key machine (`KeyMachine`): decodes raw input bytes into
//! events the server resolves against a mutable [`crate::bindings::Bindings`]
//! table. Pure logic, no I/O.
//!
//! This REPLACES the sub-project 2 hardcoded `InputMachine`/`Action`/
//! `InputEvent` machinery (deleted in sub-project 3 Task 6 once
//! `src/server.rs` was rewired onto this table-driven pipeline — see the
//! `## input-v2` section of
//! `docs/specs/2026-07-07-command-config-interfaces.md`).

use std::time::Instant;

use crate::keys;

/// Default repeat-time window (tmux default, `options::repeat-time`'s
/// default too); a per-client `KeyMachine` starts with this and can be
/// reconfigured via `set_repeat_time` (`set -g repeat-time`).
pub const REPEAT_TIME: std::time::Duration = std::time::Duration::from_millis(500);

/// Which binding table a decoded key should be looked up against. The
/// server owns the actual [`crate::bindings::Bindings`] table and resolves
/// `KeyInputEvent::Key` events through it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WhichTable {
    Root,
    Prefix,
}

/// Event produced by [`KeyMachine::feed`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyInputEvent {
    /// Bytes to forward to the focused pane verbatim. Coalesced across a
    /// whole run of plain (unmodified) forwardable keys decoded within one
    /// `feed()` call, for throughput (see `is_plain_forwardable`'s doc
    /// comment for the exact rule and the documented `bind -n` deviation it
    /// implies).
    Forward(Vec<u8>),
    /// A decoded key the server should resolve against `table`. `raw` is the
    /// exact input bytes (so an unbound `Root` key can still be forwarded).
    Key {
        table: WhichTable,
        key: keys::Key,
        raw: Vec<u8>,
    },
    /// Raw capture mode (status-line prompt line editing): uninterpreted
    /// bytes, coalesced per `feed()` call like `Forward`.
    Captured(Vec<u8>),
}

/// Keys that report directly as `Forward` in `Normal` state with no repeat
/// window active, instead of `Key { table: Root, .. }` — a throughput
/// simplification: plain runs of printable input become one coalesced
/// `Forward` event instead of one `Key` event per keystroke. **Documented
/// deviation** (see the `## input-v2` contract section): `bind -n` on a bare
/// unmodified `Char`/`Enter`/`Tab`/`Space`/`BSpace` key is accepted by
/// `cmd`/`bindings` but never fires in SP3 — only keys carrying a modifier,
/// or a named/special key outside this set, can be bound in the root table.
fn is_plain_forwardable(k: &keys::Key) -> bool {
    if k.ctrl || k.meta || k.shift {
        return false;
    }
    matches!(
        k.code,
        keys::KeyCode::Char(_)
            | keys::KeyCode::Enter
            | keys::KeyCode::Tab
            | keys::KeyCode::Space
            | keys::KeyCode::BSpace
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TableState {
    Normal,
    Prefixed,
}

fn flush_key_forward(fwd: &mut Vec<u8>, out: &mut Vec<KeyInputEvent>) {
    if !fwd.is_empty() {
        out.push(KeyInputEvent::Forward(std::mem::take(fwd)));
    }
}

/// Table-driven replacement for the sub-project 2 `InputMachine`. Decodes raw
/// input bytes via [`keys::KeyDecoder`] and reports which binding table (if
/// any) each decoded key resolves against; the server owns the actual
/// [`crate::bindings::Bindings`] table and dispatch logic.
///
/// Semantics:
/// - A decoded key equal to the configured prefix key, in `Normal` state,
///   is consumed (no event) and arms `Prefixed` for the very next decoded
///   key, which is reported as `Key { table: Prefix, .. }` unconditionally
///   (even if it is itself the prefix key again — tmux's "press prefix
///   twice" binds to `send-prefix` in the prefix table, like the legacy
///   double-Ctrl-b-forwards-literal behavior).
/// - `arm_repeat(now)`: the server calls this right after dispatching a
///   `-r` (repeatable) binding. Until `now + repeat_time`, decoded keys
///   report `Key { table: Prefix, .. }` WITHOUT a fresh prefix press
///   (matches the legacy `Ctrl-arrow` repeat window). A prefix press
///   arriving inside the window still arms `Prefixed` fresh (and clears the
///   window) rather than being swallowed by it.
/// - `set_capture(true)`: every byte, including the prefix byte and escape
///   sequences, passes through verbatim as `Captured`, bypassing prefix/
///   repeat entirely (mirrors the legacy machine's `set_capture`). Turning
///   capture on or off discards (does not re-emit) any incomplete decoder
///   buffer from before the transition; turning off also resets to `Normal`
///   table state.
pub struct KeyMachine {
    decoder: keys::KeyDecoder,
    prefix: keys::Key,
    repeat_time: std::time::Duration,
    state: TableState,
    repeat_until: Option<Instant>,
    capturing: bool,
}

impl KeyMachine {
    pub fn new(prefix: keys::Key) -> Self {
        KeyMachine {
            decoder: keys::KeyDecoder::new(),
            prefix,
            repeat_time: REPEAT_TIME,
            state: TableState::Normal,
            repeat_until: None,
            capturing: false,
        }
    }

    pub fn set_prefix(&mut self, prefix: keys::Key) {
        self.prefix = prefix;
    }

    pub fn set_repeat_time(&mut self, d: std::time::Duration) {
        self.repeat_time = d;
    }

    /// Arm the repeat window starting at `now`; see the type-level doc
    /// comment.
    pub fn arm_repeat(&mut self, now: Instant) {
        self.repeat_until = Some(now + self.repeat_time);
    }

    /// Turn raw capture mode on/off; see the type-level doc comment.
    pub fn set_capture(&mut self, on: bool) {
        // Discard (not re-emit) any incomplete decoder buffer on the
        // transition -- mirrors the legacy machine's `pending.clear()`.
        let _ = self.decoder.flush();
        self.capturing = on;
        if !on {
            self.state = TableState::Normal;
            self.repeat_until = None;
        }
    }

    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<KeyInputEvent> {
        if self.capturing {
            return if bytes.is_empty() {
                Vec::new()
            } else {
                vec![KeyInputEvent::Captured(bytes.to_vec())]
            };
        }

        let mut out: Vec<KeyInputEvent> = Vec::new();
        let mut fwd: Vec<u8> = Vec::new();
        for dk in self.decoder.feed(bytes) {
            self.dispatch_key(dk, now, &mut fwd, &mut out);
        }
        flush_key_forward(&mut fwd, &mut out);
        out
    }

    fn dispatch_key(
        &mut self,
        dk: keys::DecodedKey,
        now: Instant,
        fwd: &mut Vec<u8>,
        out: &mut Vec<KeyInputEvent>,
    ) {
        let keys::DecodedKey { key, raw } = dk;

        // Previous key armed Prefixed: this key resolves in the prefix
        // table no matter what it is.
        if self.state == TableState::Prefixed {
            self.state = TableState::Normal;
            flush_key_forward(fwd, out);
            out.push(KeyInputEvent::Key { table: WhichTable::Prefix, key, raw });
            return;
        }

        // A prefix press always arms Prefixed fresh, even inside an active
        // repeat window.
        if key == self.prefix {
            flush_key_forward(fwd, out);
            self.repeat_until = None;
            self.state = TableState::Prefixed;
            return;
        }

        if let Some(until) = self.repeat_until {
            if now < until {
                flush_key_forward(fwd, out);
                out.push(KeyInputEvent::Key { table: WhichTable::Prefix, key, raw });
                return;
            }
            self.repeat_until = None;
        }

        if is_plain_forwardable(&key) {
            fwd.extend_from_slice(&raw);
        } else {
            flush_key_forward(fwd, out);
            out.push(KeyInputEvent::Key { table: WhichTable::Root, key, raw });
        }
    }
}

#[cfg(test)]
mod key_machine_tests {
    use super::*;
    use crate::keys::{self, KeyCode};
    use std::time::{Duration, Instant};

    fn prefix_key() -> keys::Key {
        keys::parse_key("C-b").unwrap()
    }

    fn km() -> KeyMachine {
        KeyMachine::new(prefix_key())
    }

    #[test]
    fn plain_bytes_forward_coalesced() {
        let now = Instant::now();
        let mut m = km();
        assert_eq!(m.feed(b"hello", now), vec![KeyInputEvent::Forward(b"hello".to_vec())]);
    }

    #[test]
    fn prefix_then_key_reports_prefix_table() {
        let now = Instant::now();
        let mut m = km();
        assert_eq!(m.feed(b"\x02", now), vec![]);
        assert_eq!(
            m.feed(b"%", now),
            vec![KeyInputEvent::Key {
                table: WhichTable::Prefix,
                key: keys::Key { code: KeyCode::Char('%'), ctrl: false, meta: false, shift: false },
                raw: b"%".to_vec(),
            }]
        );
    }

    #[test]
    fn prefix_is_consumed_not_forwarded() {
        let now = Instant::now();
        let mut m = km();
        // The prefix byte alone produces no event at all -- not Forward,
        // not Key.
        assert_eq!(m.feed(b"\x02", now), vec![]);
    }

    #[test]
    fn double_prefix_reports_prefix_table_key() {
        // Legacy analog: Ctrl-b Ctrl-b forwarded a literal Ctrl-b. In the
        // table-driven machine the second prefix press reports as an
        // ordinary Key{Prefix, C-b} -- whose default binding is send-prefix,
        // reproducing the same end result through the bindings table.
        let now = Instant::now();
        let mut m = km();
        assert_eq!(
            m.feed(b"\x02\x02", now),
            vec![KeyInputEvent::Key {
                table: WhichTable::Prefix,
                key: keys::Key { code: KeyCode::Char('b'), ctrl: true, meta: false, shift: false },
                raw: b"\x02".to_vec(),
            }]
        );
    }

    #[test]
    fn root_table_keys_report_root() {
        let now = Instant::now();
        let mut m = km();
        // An arrow (no modifiers) is a decoded escape sequence, not in the
        // plain-forwardable set, so it reports Root even with no prefix.
        assert_eq!(
            m.feed(b"\x1b[A", now),
            vec![KeyInputEvent::Key {
                table: WhichTable::Root,
                key: keys::Key { code: KeyCode::Up, ctrl: false, meta: false, shift: false },
                raw: b"\x1b[A".to_vec(),
            }]
        );
    }

    #[test]
    fn arm_repeat_window() {
        let base = Instant::now();
        let mut m = km();
        m.arm_repeat(base);
        let cup = keys::Key { code: KeyCode::Up, ctrl: true, meta: false, shift: false };

        // Inside the 500ms window: reports Prefix without a prefix press.
        assert_eq!(
            m.feed(b"\x1b[1;5A", base + Duration::from_millis(400)),
            vec![KeyInputEvent::Key { table: WhichTable::Prefix, key: cup, raw: b"\x1b[1;5A".to_vec() }]
        );

        // After the window has elapsed: reports Root instead.
        assert_eq!(
            m.feed(b"\x1b[1;5A", base + Duration::from_millis(600)),
            vec![KeyInputEvent::Key { table: WhichTable::Root, key: cup, raw: b"\x1b[1;5A".to_vec() }]
        );
    }

    #[test]
    fn capture_bypasses_prefix() {
        let now = Instant::now();
        let mut m = km();
        // Arm Prefixed, then flip into capture mode mid-sequence.
        assert_eq!(m.feed(b"\x02", now), vec![]);
        m.set_capture(true);
        assert_eq!(m.feed(b"x", now), vec![KeyInputEvent::Captured(b"x".to_vec())]);
        // Turning capture off cleared the stale Prefixed state, so a plain
        // char now dispatches fresh (coalesced Forward), not Key{Prefix}.
        m.set_capture(false);
        assert_eq!(m.feed(b"x", now), vec![KeyInputEvent::Forward(b"x".to_vec())]);
    }

    #[test]
    fn configurable_prefix() {
        let now = Instant::now();
        let mut m = km();
        m.set_prefix(keys::parse_key("C-a").unwrap());

        // 0x01 (C-a) is now the prefix: consumed, no event.
        assert_eq!(m.feed(b"\x01", now), vec![]);

        // 0x02 (the OLD prefix, C-b) is no longer special: it is dispatched
        // as an ordinary decoded key. It carries ctrl, so it is not in the
        // plain-forwardable set -- it reports Key{Root}, NOT swallowed as a
        // prefix anymore.
        let mut m2 = km();
        m2.set_prefix(keys::parse_key("C-a").unwrap());
        assert_eq!(
            m2.feed(b"\x02", now),
            vec![KeyInputEvent::Key {
                table: WhichTable::Root,
                key: keys::Key { code: KeyCode::Char('b'), ctrl: true, meta: false, shift: false },
                raw: b"\x02".to_vec(),
            }]
        );
    }
}
