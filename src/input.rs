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

/// Default escape-time window (tmux default, `options::escape-time`'s
/// default too — sub-project 4, Task 9): a per-client `KeyMachine` starts
/// with this and can be reconfigured via `set_escape_time` (`set -g
/// escape-time`). See `escape_ready`/`flush_now`'s doc comments.
pub const ESCAPE_TIME: std::time::Duration = std::time::Duration::from_millis(500);

/// Which binding table a decoded key should be looked up against. The
/// server owns the actual [`crate::bindings::Bindings`] table and resolves
/// `KeyInputEvent::Key` events through it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WhichTable {
    Root,
    Prefix,
    /// Copy-mode (emacs `mode-keys`) key table. `KeyMachine` never produces
    /// this itself (it knows nothing of client modes) — the SERVER
    /// substitutes it for a `Root`-table `Key` event when the acting client
    /// is in `ClientMode::Copy` and `mode-keys` is `emacs` (see the
    /// `## copy-mode` contract section).
    CopyMode,
    /// Copy-mode (vi `mode-keys`) key table; same substitution rule as
    /// `CopyMode` but for `mode-keys vi`.
    CopyModeVi,
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
    /// A decoded mouse event (Task 5, sub-project 4). Unlike `Key`, this is
    /// NEVER routed through the prefix/table state machine or the repeat
    /// window — mouse "bindings" are hardcoded server-side (see the design
    /// spec's `## 4. Mouse` section), not looked up in
    /// `crate::bindings::Bindings`. `raw` is kept for symmetry with `Key`,
    /// still unused: SP7 Task 9 (closes follow-ups #35/#72) DOES now forward
    /// mouse events to a pane whose own app requested mouse reporting, but
    /// via `keys::encode_mouse` RE-ENCODING the already-decoded `event`
    /// (rebased to the pane's own origin, in the pane's own requested
    /// coordinate encoding) rather than replaying these CLIENT-terminal-
    /// relative raw bytes verbatim — `server::handle_event`'s
    /// `KeyInputEvent::Mouse { event, .. }` arm destructures and discards
    /// `raw` before it ever reaches `dispatch_mouse`.
    Mouse { event: keys::MouseEvent, raw: Vec<u8> },
}

/// Keys that report directly as `Forward` in `Normal` state with no repeat
/// window active, instead of `Key { table: Root, .. }` — a throughput
/// simplification: plain runs of printable input become one coalesced
/// `Forward` event instead of one `Key` event per keystroke. **This is a
/// coalescing-shape decision only, not the last word on bindability** (see
/// the `## input-v2` contract section): as of SP7's follow-up #30 fix,
/// `server.rs`'s `Forward`-blob consumption re-decodes and looks up every
/// key in this blob against the LIVE root table before forwarding it, so a
/// `bind -n` on a bare unmodified `Char`/`Enter`/`Tab`/`Space`/`BSpace` key
/// DOES fire (it dispatches and is swallowed instead of forwarded) — this
/// function no longer implies "never bindable," only "batched with its
/// neighbors instead of decoded as its own `Key` event here."
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
///   repeat entirely (mirrors the legacy machine's `set_capture`). Both
///   directions of the transition discard (do not re-emit) any incomplete
///   decoder buffer from before it. **Precise ON/OFF split (follow-up #20 —
///   the previous wording here was vaguer about which direction does what):**
///   turning capture ON leaves any already-armed `Prefixed`/repeat-window
///   state untouched (irrelevant while capturing, since dispatch is bypassed
///   entirely) — it is turning capture OFF that resets to `Normal` table
///   state and clears the repeat window, so a `Prefixed` state armed before
///   entering capture does NOT survive capture ending.
pub struct KeyMachine {
    decoder: keys::KeyDecoder,
    prefix: keys::Key,
    repeat_time: std::time::Duration,
    state: TableState,
    repeat_until: Option<Instant>,
    capturing: bool,
    /// Escape-time (sub-project 4, Task 9): how long an outstanding pending
    /// ESC (`keys::KeyDecoder::pending_starts_with_escape`) may sit
    /// unresolved before the server force-flushes it as a bare `Escape` key.
    /// tmux default 500ms; `set -g escape-time 0` flushes on the very next
    /// `Tick` (50ms server granularity — see `escape_ready`'s doc comment).
    escape_time: std::time::Duration,
    /// When the CURRENT pending-ESC buffer first appeared (`None` while no
    /// ESC-led sequence is outstanding). Set the first time a `feed()` call
    /// leaves `decoder.pending_starts_with_escape()` true, cleared the
    /// moment it goes false again (sequence completed or was force-flushed)
    /// — so it never "restarts the clock" just because more bytes of the
    /// SAME still-incomplete sequence keep arriving.
    pending_escape_since: Option<Instant>,
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
            escape_time: ESCAPE_TIME,
            pending_escape_since: None,
        }
    }

    pub fn set_prefix(&mut self, prefix: keys::Key) {
        self.prefix = prefix;
    }

    pub fn set_repeat_time(&mut self, d: std::time::Duration) {
        self.repeat_time = d;
    }

    /// `set -g escape-time <ms>` (sub-project 4, Task 9). Does NOT
    /// retroactively re-evaluate an already-outstanding pending ESC's age —
    /// the new duration applies to the next `escape_ready` check, same as
    /// `set_repeat_time`.
    pub fn set_escape_time(&mut self, d: std::time::Duration) {
        self.escape_time = d;
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
        self.pending_escape_since = None;
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
        for item in self.decoder.feed(bytes) {
            match item {
                keys::DecodedInput::Key(dk) => self.dispatch_key(dk, now, &mut fwd, &mut out),
                keys::DecodedInput::Mouse { event, raw } => {
                    // Bypasses prefix/table dispatch entirely (see
                    // `KeyInputEvent::Mouse`'s doc comment); still flushes
                    // any pending coalesced Forward run first so ordering
                    // relative to preceding plain keystrokes is preserved.
                    flush_key_forward(&mut fwd, &mut out);
                    out.push(KeyInputEvent::Mouse { event, raw });
                }
            }
        }
        flush_key_forward(&mut fwd, &mut out);
        self.update_pending_escape(now);
        out
    }

    /// Escape-time bookkeeping (sub-project 4, Task 9), run at the end of
    /// every `feed()`: start the clock the first time a pending ESC-led
    /// buffer appears, clear it the moment none remains.
    fn update_pending_escape(&mut self, now: Instant) {
        if self.decoder.pending_starts_with_escape() {
            if self.pending_escape_since.is_none() {
                self.pending_escape_since = Some(now);
            }
        } else {
            self.pending_escape_since = None;
        }
    }

    /// `true` once an outstanding pending ESC has aged past `escape_time` as
    /// of `now` (sub-project 4, Task 9). The server's `Tick` handler polls
    /// this per client (50ms granularity, matching the design spec's `## 8.
    /// escape-time` section) and calls [`KeyMachine::flush_now`] when it
    /// fires. `escape_time` 0 means "ready as soon as any tick observes the
    /// pending ESC" — there is no sub-tick immediate flush, so a burst CSI
    /// sequence that arrives split across `feed()` calls within the same
    /// 50ms tick still completes normally rather than racing the flush.
    pub fn escape_ready(&self, now: Instant) -> bool {
        matches!(
            self.pending_escape_since,
            Some(since) if now.saturating_duration_since(since) >= self.escape_time
        )
    }

    /// Force-drain any incomplete pending decoder buffer
    /// (`keys::KeyDecoder::flush`) through the SAME dispatch path `feed`
    /// uses, producing whatever `KeyInputEvent`s result — a lone pending ESC
    /// becomes one `Key` event carrying `KeyCode::Escape`; a truncated
    /// multi-byte sequence peels byte by byte, exactly like
    /// `KeyDecoder::flush`'s own doc comment describes (sub-project 4, Task
    /// 9). Called by the server's `Tick` handler once `escape_ready` reports
    /// the pending ESC is older than `escape-time`; also clears the
    /// pending-escape timer. A no-op (empty result) if nothing is pending.
    pub fn flush_now(&mut self, now: Instant) -> Vec<KeyInputEvent> {
        let mut out: Vec<KeyInputEvent> = Vec::new();
        let mut fwd: Vec<u8> = Vec::new();
        for item in self.decoder.flush() {
            match item {
                keys::DecodedInput::Key(dk) => self.dispatch_key(dk, now, &mut fwd, &mut out),
                keys::DecodedInput::Mouse { event, raw } => {
                    flush_key_forward(&mut fwd, &mut out);
                    out.push(KeyInputEvent::Mouse { event, raw });
                }
            }
        }
        flush_key_forward(&mut fwd, &mut out);
        self.pending_escape_since = None;
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

    /// Follow-up #20 (renamed from `capture_mode_clears_pending_prefix_state`,
    /// a name left over from the sub-project 2 `InputMachine` this module
    /// replaced — that exact name/test no longer exists post-rewrite, but the
    /// naming-precision gap it flagged is worth closing here too): the name
    /// now says exactly what's asserted — bypass DURING capture, plus OFF
    /// (not ON) is what clears armed `Prefixed` state.
    #[test]
    fn capture_bypasses_prefix_and_off_clears_prefix_state() {
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
    fn mouse_bypasses_prefix_and_repeat() {
        let now = Instant::now();
        let mut m = km();
        // Arm Prefixed, then feed a mouse sequence: it must report as Mouse
        // (not consumed as the awaited prefix-table key), and must NOT clear
        // the still-armed Prefixed state for the key that follows.
        assert_eq!(m.feed(b"\x02", now), vec![]);
        let mouse_bytes = b"\x1b[<0;6;11M";
        let events = m.feed(mouse_bytes, now);
        assert_eq!(events.len(), 1);
        match &events[0] {
            KeyInputEvent::Mouse { event, raw } => {
                assert_eq!(event.kind, keys::MouseKind::Down(1));
                assert_eq!((event.x, event.y), (5, 10));
                assert_eq!(raw, mouse_bytes);
            }
            other => panic!("expected Mouse, got {other:?}"),
        }
        // The still-armed prefix state resolves the NEXT key in the prefix
        // table, proving the mouse event didn't disturb it.
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
    fn mouse_flushes_pending_forward_first() {
        let now = Instant::now();
        let mut m = km();
        let mut bytes = b"hi".to_vec();
        bytes.extend_from_slice(b"\x1b[<0;1;1M");
        let events = m.feed(&bytes, now);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0], KeyInputEvent::Forward(b"hi".to_vec()));
        assert!(matches!(events[1], KeyInputEvent::Mouse { .. }));
    }

    #[test]
    fn lone_escape_flushes_after_escape_time() {
        let base = Instant::now();
        let mut m = km();
        m.set_escape_time(Duration::from_millis(200));

        // A lone ESC decodes to nothing yet -- it's buffered pending more
        // bytes (could be the start of a CSI/SS3/meta sequence).
        assert_eq!(m.feed(b"\x1b", base), vec![]);
        assert!(!m.escape_ready(base + Duration::from_millis(100)), "not aged past escape-time yet");
        assert!(m.escape_ready(base + Duration::from_millis(200)), "aged exactly to escape-time");

        // The Tick handler would call flush_now once escape_ready is true;
        // it force-resolves the pending ESC to a bare Escape key event.
        let flushed = m.flush_now(base + Duration::from_millis(200));
        assert_eq!(
            flushed,
            vec![KeyInputEvent::Key {
                table: WhichTable::Root,
                key: keys::Key { code: KeyCode::Escape, ctrl: false, meta: false, shift: false },
                raw: vec![0x1b],
            }]
        );
        // The pending-escape timer is cleared: no longer ready, and further
        // input decodes fresh (not stuck mid-sequence).
        assert!(!m.escape_ready(base + Duration::from_secs(10)));
        assert_eq!(m.feed(b"a", base), vec![KeyInputEvent::Forward(b"a".to_vec())]);
    }

    #[test]
    fn burst_csi_within_one_feed_never_reports_escape_ready() {
        // The common case (SSH sends an escape sequence as one write): a
        // complete CSI arriving in a SINGLE feed() call must decode as the
        // arrow key, never leaving a pending ESC behind for escape-time to
        // misfire on.
        let base = Instant::now();
        let mut m = km();
        m.set_escape_time(Duration::from_millis(0));
        assert_eq!(
            m.feed(b"\x1b[A", base),
            vec![KeyInputEvent::Key {
                table: WhichTable::Root,
                key: keys::Key { code: KeyCode::Up, ctrl: false, meta: false, shift: false },
                raw: b"\x1b[A".to_vec(),
            }]
        );
        assert!(!m.escape_ready(base + Duration::from_secs(1)));
    }

    #[test]
    fn escape_pending_resolved_by_later_bytes_before_ready() {
        // ESC arrives, then (before escape-time elapses) the REST of a CSI
        // arrow arrives in a later feed() call: it must complete normally as
        // Up, not get force-flushed as a bare Escape.
        let base = Instant::now();
        let mut m = km();
        m.set_escape_time(Duration::from_millis(200));
        assert_eq!(m.feed(b"\x1b", base), vec![]);
        let mid = base + Duration::from_millis(50);
        assert!(!m.escape_ready(mid));
        assert_eq!(
            m.feed(b"[A", mid),
            vec![KeyInputEvent::Key {
                table: WhichTable::Root,
                key: keys::Key { code: KeyCode::Up, ctrl: false, meta: false, shift: false },
                raw: b"\x1b[A".to_vec(),
            }]
        );
        assert!(!m.escape_ready(base + Duration::from_secs(1)), "sequence completed -- nothing pending");
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
