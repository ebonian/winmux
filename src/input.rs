//! Prefix-key state machine: Normal / Prefixed / Repeat / Confirming.
//!
//! Pure logic, no I/O: `feed()` takes raw input bytes plus a monotonic
//! clock reading and returns the events they produce. Escape sequences
//! (arrows, Ctrl-arrows) may arrive split across multiple `feed()` calls;
//! in-progress sequences are buffered in `pending` between calls.

use std::time::Instant;

use crate::geom::Direction;
use crate::keys;
use crate::layout::SplitDir;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Split(SplitDir),
    Focus(Direction),
    FocusNext,             // prefix o
    FocusLast,             // prefix ;
    RequestClose,          // prefix x
    ToggleZoom,            // prefix z
    Resize(Direction),     // prefix Ctrl-arrow, repeatable
    Quit,                  // internal: not bound to a key in the MVP
    NewWindow,             // prefix c
    NextWindow,            // prefix n
    PrevWindow,            // prefix p
    LastWindow,            // prefix l
    SelectWindow(u32),     // prefix 0-9 (digit value, not the ASCII byte)
    RequestKillWindow,     // prefix &
    RenameWindow,          // prefix ,
    RenameSession,         // prefix $
    Detach,                // prefix d
    SwitchClientPrev,      // prefix (
    SwitchClientNext,      // prefix )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Forward(Vec<u8>),
    Action(Action),
    ConfirmClose(bool),
    /// Emitted only while capture mode is on (`set_capture(true)`): raw,
    /// uninterpreted bytes for a status-line prompt (e.g. rename-window
    /// line editing), coalesced per `feed()` call like `Forward`.
    Captured(Vec<u8>),
}

pub const PREFIX: u8 = 0x02; // Ctrl-b
pub const REPEAT_TIME: std::time::Duration = std::time::Duration::from_millis(500);

/// Private machine state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Normal,
    Prefixed,
    Repeat { until: Instant },
    Confirming,
}

pub struct InputMachine {
    state: State,
    /// Buffered bytes of an in-progress escape sequence (always begins with
    /// 0x1b). Used while Prefixed (waiting for an arrow / Ctrl-arrow command)
    /// and while in Repeat (matching a bare Ctrl-arrow). Empty otherwise.
    pending: Vec<u8>,
    /// Raw capture mode, orthogonal to `state`. Checked first in `feed()`,
    /// before any `state`-based dispatch, so capture wins even if `state`
    /// happens to still be `Confirming` underneath (e.g. a caller flips both
    /// flags) — see `set_capture`.
    capturing: bool,
}

/// Map an escape-sequence final byte to a direction.
/// A->Up, B->Down, C->Right, D->Left (shared by arrows and Ctrl-arrows).
fn arrow_dir(final_byte: u8) -> Option<Direction> {
    match final_byte {
        b'A' => Some(Direction::Up),
        b'B' => Some(Direction::Down),
        b'C' => Some(Direction::Right),
        b'D' => Some(Direction::Left),
        _ => None,
    }
}

/// Flush the coalesced Normal-forward accumulator as a single Forward event.
fn flush_forward(fwd: &mut Vec<u8>, out: &mut Vec<InputEvent>) {
    if !fwd.is_empty() {
        out.push(InputEvent::Forward(std::mem::take(fwd)));
    }
}

impl Default for InputMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl InputMachine {
    pub fn new() -> Self {
        InputMachine {
            state: State::Normal,
            pending: Vec::new(),
            capturing: false,
        }
    }

    pub fn set_confirming(&mut self, on: bool) {
        self.pending.clear();
        self.state = if on { State::Confirming } else { State::Normal };
    }

    /// Turn raw capture mode on/off. Turning on clears any pending
    /// escape-sequence buffer and prefix state, mirroring `set_confirming`;
    /// turning off resumes Normal. While on, `feed()` bypasses all
    /// state-machine dispatch (see the `capturing` field doc comment).
    pub fn set_capture(&mut self, on: bool) {
        self.pending.clear();
        self.capturing = on;
        if !on {
            self.state = State::Normal;
        }
    }

    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<InputEvent> {
        if self.capturing {
            // Raw capture mode: every byte — including the prefix byte and
            // escape sequences — passes through unparsed, coalesced into a
            // single Captured event per feed() call, exactly like Forward.
            return if bytes.is_empty() {
                Vec::new()
            } else {
                vec![InputEvent::Captured(bytes.to_vec())]
            };
        }

        let mut out: Vec<InputEvent> = Vec::new();
        let mut fwd: Vec<u8> = Vec::new();

        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            // Default: this byte is consumed. Repeat-state exits set this false
            // so the same byte is re-dispatched in Normal on the next iteration.
            let mut advance = true;

            match self.state {
                State::Normal => {
                    if b == PREFIX {
                        flush_forward(&mut fwd, &mut out);
                        self.pending.clear();
                        self.state = State::Prefixed;
                    } else {
                        fwd.push(b);
                    }
                }

                State::Prefixed => {
                    if self.pending.is_empty() {
                        // First key after the prefix.
                        match b {
                            b'%' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::Split(SplitDir::Horizontal)));
                                self.state = State::Normal;
                            }
                            b'"' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::Split(SplitDir::Vertical)));
                                self.state = State::Normal;
                            }
                            b'o' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::FocusNext));
                                self.state = State::Normal;
                            }
                            b';' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::FocusLast));
                                self.state = State::Normal;
                            }
                            b'x' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::RequestClose));
                                self.state = State::Normal;
                            }
                            b'z' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::ToggleZoom));
                                self.state = State::Normal;
                            }
                            b'c' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::NewWindow));
                                self.state = State::Normal;
                            }
                            b'n' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::NextWindow));
                                self.state = State::Normal;
                            }
                            b'p' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::PrevWindow));
                                self.state = State::Normal;
                            }
                            b'l' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::LastWindow));
                                self.state = State::Normal;
                            }
                            b'0'..=b'9' => {
                                flush_forward(&mut fwd, &mut out);
                                let digit = (b - b'0') as u32;
                                out.push(InputEvent::Action(Action::SelectWindow(digit)));
                                self.state = State::Normal;
                            }
                            b'&' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::RequestKillWindow));
                                self.state = State::Normal;
                            }
                            b',' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::RenameWindow));
                                self.state = State::Normal;
                            }
                            b'$' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::RenameSession));
                                self.state = State::Normal;
                            }
                            b'd' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::Detach));
                                self.state = State::Normal;
                            }
                            b'(' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::SwitchClientPrev));
                                self.state = State::Normal;
                            }
                            b')' => {
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Action(Action::SwitchClientNext));
                                self.state = State::Normal;
                            }
                            PREFIX => {
                                // Ctrl-b Ctrl-b: send a literal Ctrl-b.
                                flush_forward(&mut fwd, &mut out);
                                out.push(InputEvent::Forward(vec![PREFIX]));
                                self.state = State::Normal;
                            }
                            0x1b => {
                                // Begin buffering an escape sequence.
                                self.pending.push(b);
                            }
                            _ => {
                                // Unknown command key: disarm silently, swallow.
                                self.state = State::Normal;
                            }
                        }
                    } else {
                        // Continuing a buffered escape sequence (pending[0] == 0x1b).
                        match self.pending.len() {
                            1 => {
                                if b == 0x5b {
                                    self.pending.push(b);
                                } else {
                                    // ESC followed by something else: swallow + disarm.
                                    self.pending.clear();
                                    self.state = State::Normal;
                                }
                            }
                            2 => {
                                if let Some(dir) = arrow_dir(b) {
                                    flush_forward(&mut fwd, &mut out);
                                    out.push(InputEvent::Action(Action::Focus(dir)));
                                    self.pending.clear();
                                    self.state = State::Normal;
                                } else if b == 0x31 {
                                    // Possible Ctrl-arrow: ESC [ 1 ...
                                    self.pending.push(b);
                                } else {
                                    self.pending.clear();
                                    self.state = State::Normal;
                                }
                            }
                            3 => {
                                if b == 0x3b {
                                    self.pending.push(b);
                                } else {
                                    self.pending.clear();
                                    self.state = State::Normal;
                                }
                            }
                            4 => {
                                if b == 0x35 {
                                    self.pending.push(b);
                                } else {
                                    self.pending.clear();
                                    self.state = State::Normal;
                                }
                            }
                            5 => {
                                if let Some(dir) = arrow_dir(b) {
                                    flush_forward(&mut fwd, &mut out);
                                    out.push(InputEvent::Action(Action::Resize(dir)));
                                    self.pending.clear();
                                    self.state = State::Repeat { until: now + REPEAT_TIME };
                                } else {
                                    self.pending.clear();
                                    self.state = State::Normal;
                                }
                            }
                            _ => {
                                // Defensive: over-long buffer, discard + disarm.
                                self.pending.clear();
                                self.state = State::Normal;
                            }
                        }
                    }
                }

                State::Repeat { until } => {
                    if now >= until {
                        // Window elapsed: leave Repeat and re-dispatch this byte
                        // as Normal. Any buffered escape bytes become forwarded.
                        if !self.pending.is_empty() {
                            fwd.extend_from_slice(&self.pending);
                            self.pending.clear();
                        }
                        self.state = State::Normal;
                        advance = false;
                    } else {
                        // Inside the window: only a bare Ctrl-arrow keeps repeating.
                        match self.pending.len() {
                            0 => {
                                if b == 0x1b {
                                    self.pending.push(b);
                                } else {
                                    // Non-Ctrl-arrow: exit Repeat, reprocess as Normal.
                                    self.state = State::Normal;
                                    advance = false;
                                }
                            }
                            1 => {
                                if b == 0x5b {
                                    self.pending.push(b);
                                } else {
                                    fwd.extend_from_slice(&self.pending);
                                    self.pending.clear();
                                    self.state = State::Normal;
                                    advance = false;
                                }
                            }
                            2 => {
                                if b == 0x31 {
                                    self.pending.push(b);
                                } else {
                                    // Includes plain arrows (final A/B/C/D): forward raw.
                                    fwd.extend_from_slice(&self.pending);
                                    self.pending.clear();
                                    self.state = State::Normal;
                                    advance = false;
                                }
                            }
                            3 => {
                                if b == 0x3b {
                                    self.pending.push(b);
                                } else {
                                    fwd.extend_from_slice(&self.pending);
                                    self.pending.clear();
                                    self.state = State::Normal;
                                    advance = false;
                                }
                            }
                            4 => {
                                if b == 0x35 {
                                    self.pending.push(b);
                                } else {
                                    fwd.extend_from_slice(&self.pending);
                                    self.pending.clear();
                                    self.state = State::Normal;
                                    advance = false;
                                }
                            }
                            5 => {
                                if let Some(dir) = arrow_dir(b) {
                                    flush_forward(&mut fwd, &mut out);
                                    out.push(InputEvent::Action(Action::Resize(dir)));
                                    self.pending.clear();
                                    self.state = State::Repeat { until: now + REPEAT_TIME };
                                } else {
                                    fwd.extend_from_slice(&self.pending);
                                    self.pending.clear();
                                    self.state = State::Normal;
                                    advance = false;
                                }
                            }
                            _ => {
                                fwd.extend_from_slice(&self.pending);
                                self.pending.clear();
                                self.state = State::Normal;
                                advance = false;
                            }
                        }
                    }
                }

                State::Confirming => {
                    // Exactly one key decides; keys are consumed, never forwarded.
                    flush_forward(&mut fwd, &mut out); // defensive; normally empty
                    let confirmed = b == b'y' || b == b'Y';
                    out.push(InputEvent::ConfirmClose(confirmed));
                    self.state = State::Normal;
                }
            }

            if advance {
                i += 1;
            }
        }

        // Flush any trailing coalesced Normal bytes. (Incomplete escape tails
        // remain buffered in self.pending across calls while Prefixed/Repeat.)
        flush_forward(&mut fwd, &mut out);
        out
    }
}

// ---------------------------------------------------------------------
// input-v2: table-driven key machine (Task 5, sub-project 3).
//
// Lands ALONGSIDE the legacy `InputMachine`/`Action` machinery above (Task 6
// deletes the legacy path and rewires `src/server.rs` onto this). See the
// `## input-v2` section of
// `docs/specs/2026-07-07-command-config-interfaces.md`.
// ---------------------------------------------------------------------

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

/// Table-driven replacement for [`InputMachine`] (Task 6 deletes the legacy
/// type once the server is rewired onto this one). Decodes raw input bytes
/// via [`keys::KeyDecoder`] and reports which binding table (if any) each
/// decoded key resolves against; the server owns the actual
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
///   repeat entirely (mirrors `InputMachine::set_capture`). Turning capture
///   on or off discards (does not re-emit) any incomplete decoder buffer
///   from before the transition, matching the legacy machine's
///   `pending.clear()`; turning off also resets to `Normal` table state.
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
        // transition -- mirrors `InputMachine::set_capture`'s
        // `pending.clear()`.
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
mod tests {
    use super::*;
    use crate::geom::Direction;
    use crate::layout::SplitDir;
    use std::time::{Duration, Instant};

    fn m() -> InputMachine {
        InputMachine::new()
    }

    // ---- Normal-mode passthrough + coalescing ----

    #[test]
    fn normal_passthrough_coalesces_into_one_forward() {
        let now = Instant::now();
        let mut im = m();
        let ev = im.feed(b"hello", now);
        assert_eq!(ev, vec![InputEvent::Forward(b"hello".to_vec())]);
    }

    #[test]
    fn empty_input_yields_no_events() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"", now), vec![]);
    }

    // ---- Prefix consumed; bytes after a command continue in Normal ----

    #[test]
    fn prefix_split_then_continues_in_normal() {
        let now = Instant::now();
        let mut im = m();
        let ev = im.feed(b"ab\x02%cd", now);
        assert_eq!(
            ev,
            vec![
                InputEvent::Forward(b"ab".to_vec()),
                InputEvent::Action(Action::Split(SplitDir::Horizontal)),
                InputEvent::Forward(b"cd".to_vec()),
            ]
        );
    }

    // ---- Every command key ----

    #[test]
    fn command_key_split_vertical() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02\"", now),
            vec![InputEvent::Action(Action::Split(SplitDir::Vertical))]
        );
    }

    #[test]
    fn command_key_split_horizontal() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02%", now),
            vec![InputEvent::Action(Action::Split(SplitDir::Horizontal))]
        );
    }

    #[test]
    fn command_key_focus_next() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02o", now), vec![InputEvent::Action(Action::FocusNext)]);
    }

    #[test]
    fn command_key_focus_last() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02;", now), vec![InputEvent::Action(Action::FocusLast)]);
    }

    #[test]
    fn command_key_request_close() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02x", now), vec![InputEvent::Action(Action::RequestClose)]);
    }

    #[test]
    fn command_key_toggle_zoom() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02z", now), vec![InputEvent::Action(Action::ToggleZoom)]);
    }

    #[test]
    fn command_arrows_map_to_focus() {
        let now = Instant::now();
        for (bytes, dir) in [
            (&b"\x02\x1b[A"[..], Direction::Up),
            (&b"\x02\x1b[B"[..], Direction::Down),
            (&b"\x02\x1b[C"[..], Direction::Right),
            (&b"\x02\x1b[D"[..], Direction::Left),
        ] {
            let mut im = m();
            assert_eq!(
                im.feed(bytes, now),
                vec![InputEvent::Action(Action::Focus(dir))],
                "arrow {:?}",
                bytes
            );
        }
    }

    #[test]
    fn double_prefix_forwards_literal_ctrl_b() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02\x02", now), vec![InputEvent::Forward(vec![0x02])]);
    }

    #[test]
    fn unknown_command_key_is_swallowed_and_disarms() {
        let now = Instant::now();
        let mut im = m();
        // 'q' is not a bound command: no event, and the machine is back in Normal.
        assert_eq!(im.feed(b"\x02q", now), vec![]);
        assert_eq!(im.feed(b"hi", now), vec![InputEvent::Forward(b"hi".to_vec())]);
    }

    // ---- Escape sequence split across feed() calls while Prefixed ----

    #[test]
    fn arrow_sequence_split_across_feeds() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02", now), vec![]);      // arm Prefixed
        assert_eq!(im.feed(b"\x1b[", now), vec![]);     // buffer incomplete ESC seq
        assert_eq!(im.feed(b"A", now), vec![InputEvent::Action(Action::Focus(Direction::Up))]);
    }

    // ---- Ctrl-arrow -> Resize, then Repeat window ----

    #[test]
    fn ctrl_arrow_resizes_and_enters_repeat() {
        let base = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02\x1b[1;5A", base),
            vec![InputEvent::Action(Action::Resize(Direction::Up))]
        );
    }

    #[test]
    fn bare_ctrl_arrow_within_window_repeats_resize() {
        let base = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02\x1b[1;5A", base),
            vec![InputEvent::Action(Action::Resize(Direction::Up))]
        );
        // No prefix this time; still inside the 500ms window (400ms later).
        assert_eq!(
            im.feed(b"\x1b[1;5B", base + Duration::from_millis(400)),
            vec![InputEvent::Action(Action::Resize(Direction::Down))]
        );
    }

    #[test]
    fn ctrl_arrow_after_window_elapsed_is_forwarded_raw() {
        let base = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02\x1b[1;5A", base),
            vec![InputEvent::Action(Action::Resize(Direction::Up))]
        );
        // 600ms later: window (base+500ms) has elapsed -> NOT a Resize,
        // the raw bytes are forwarded verbatim.
        assert_eq!(
            im.feed(b"\x1b[1;5C", base + Duration::from_millis(600)),
            vec![InputEvent::Forward(vec![0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x43])]
        );
    }

    #[test]
    fn non_ctrl_arrow_input_exits_repeat_as_normal() {
        let base = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02\x1b[1;5A", base),
            vec![InputEvent::Action(Action::Resize(Direction::Up))]
        );
        // Ordinary text while in Repeat: exit Repeat, process as Normal.
        assert_eq!(
            im.feed(b"hello", base),
            vec![InputEvent::Forward(b"hello".to_vec())]
        );
    }

    // ---- Confirming mode ----

    #[test]
    fn confirming_y_lower_confirms() {
        let now = Instant::now();
        let mut im = m();
        im.set_confirming(true);
        assert_eq!(im.feed(b"y", now), vec![InputEvent::ConfirmClose(true)]);
        // Back to Normal afterwards.
        assert_eq!(im.feed(b"a", now), vec![InputEvent::Forward(b"a".to_vec())]);
    }

    #[test]
    fn confirming_y_upper_confirms() {
        let now = Instant::now();
        let mut im = m();
        im.set_confirming(true);
        assert_eq!(im.feed(b"Y", now), vec![InputEvent::ConfirmClose(true)]);
    }

    #[test]
    fn confirming_other_key_cancels() {
        let now = Instant::now();
        let mut im = m();
        im.set_confirming(true);
        assert_eq!(im.feed(b"n", now), vec![InputEvent::ConfirmClose(false)]);
    }

    #[test]
    fn confirming_escape_cancels_and_is_consumed() {
        let now = Instant::now();
        let mut im = m();
        im.set_confirming(true);
        assert_eq!(im.feed(b"\x1b", now), vec![InputEvent::ConfirmClose(false)]);
        // Consumed, not forwarded; machine is Normal again.
        assert_eq!(im.feed(b"z", now), vec![InputEvent::Forward(b"z".to_vec())]);
    }

    // ---- Window/session bindings (sub-project 2) ----

    #[test]
    fn prefix_c_is_new_window() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02c", now), vec![InputEvent::Action(Action::NewWindow)]);
        // Back to Normal afterwards.
        assert_eq!(im.feed(b"z", now), vec![InputEvent::Forward(b"z".to_vec())]);
    }

    #[test]
    fn prefix_n_is_next_window() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02n", now), vec![InputEvent::Action(Action::NextWindow)]);
    }

    #[test]
    fn prefix_p_is_prev_window() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02p", now), vec![InputEvent::Action(Action::PrevWindow)]);
    }

    #[test]
    fn prefix_l_is_last_window() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02l", now), vec![InputEvent::Action(Action::LastWindow)]);
    }

    #[test]
    fn prefix_digit_selects_window() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x020", now),
            vec![InputEvent::Action(Action::SelectWindow(0))]
        );
        let mut im = m();
        assert_eq!(
            im.feed(b"\x029", now),
            vec![InputEvent::Action(Action::SelectWindow(9))]
        );
    }

    #[test]
    fn prefix_ampersand_is_request_kill_window() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02&", now),
            vec![InputEvent::Action(Action::RequestKillWindow)]
        );
    }

    #[test]
    fn prefix_comma_is_rename_window() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02,", now), vec![InputEvent::Action(Action::RenameWindow)]);
    }

    #[test]
    fn prefix_dollar_is_rename_session() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02$", now), vec![InputEvent::Action(Action::RenameSession)]);
    }

    #[test]
    fn prefix_d_is_detach() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(im.feed(b"\x02d", now), vec![InputEvent::Action(Action::Detach)]);
    }

    #[test]
    fn prefix_open_paren_is_switch_client_prev() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02(", now),
            vec![InputEvent::Action(Action::SwitchClientPrev)]
        );
    }

    #[test]
    fn prefix_close_paren_is_switch_client_next() {
        let now = Instant::now();
        let mut im = m();
        assert_eq!(
            im.feed(b"\x02)", now),
            vec![InputEvent::Action(Action::SwitchClientNext)]
        );
    }

    // ---- Capture mode (sub-project 2: status-line prompts) ----

    #[test]
    fn capture_mode_passes_bytes_raw() {
        let now = Instant::now();
        let mut im = m();
        im.set_capture(true);
        // The prefix byte and an escape sequence both pass through
        // uninterpreted and coalesced, exactly as fed.
        assert_eq!(
            im.feed(b"hi\x02\x1b[Abye", now),
            vec![InputEvent::Captured(b"hi\x02\x1b[Abye".to_vec())]
        );
    }

    #[test]
    fn capture_mode_clears_pending_prefix_state() {
        let now = Instant::now();
        let mut im = m();
        // Arm Prefixed, then flip into capture mid-sequence.
        assert_eq!(im.feed(b"\x02", now), vec![]);
        im.set_capture(true);
        // The 'x' that would have been RequestClose is now just raw data.
        assert_eq!(im.feed(b"x", now), vec![InputEvent::Captured(b"x".to_vec())]);
    }

    #[test]
    fn capture_off_resumes_normal() {
        let now = Instant::now();
        let mut im = m();
        im.set_capture(true);
        assert_eq!(im.feed(b"raw", now), vec![InputEvent::Captured(b"raw".to_vec())]);
        im.set_capture(false);
        assert_eq!(im.feed(b"\x02x", now), vec![InputEvent::Action(Action::RequestClose)]);
    }

    #[test]
    fn capture_takes_precedence_over_confirming() {
        let now = Instant::now();
        let mut im = m();
        im.set_confirming(true);
        // Capture wins even though Confirming was armed underneath.
        im.set_capture(true);
        assert_eq!(im.feed(b"y", now), vec![InputEvent::Captured(b"y".to_vec())]);
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
