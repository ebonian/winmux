//! Prefix-key state machine: Normal / Prefixed / Repeat / Confirming.
//!
//! Pure logic, no I/O: `feed()` takes raw input bytes plus a monotonic
//! clock reading and returns the events they produce. Escape sequences
//! (arrows, Ctrl-arrows) may arrive split across multiple `feed()` calls;
//! in-progress sequences are buffered in `pending` between calls.

use std::time::Instant;

use crate::geom::Direction;
use crate::layout::SplitDir;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Split(SplitDir),
    Focus(Direction),
    FocusNext,         // prefix o
    FocusLast,         // prefix ;
    RequestClose,      // prefix x
    ToggleZoom,        // prefix z
    Resize(Direction), // prefix Ctrl-arrow, repeatable
    Quit,              // internal: not bound to a key in the MVP
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Forward(Vec<u8>),
    Action(Action),
    ConfirmClose(bool),
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
        }
    }

    pub fn set_confirming(&mut self, on: bool) {
        self.pending.clear();
        self.state = if on { State::Confirming } else { State::Normal };
    }

    pub fn feed(&mut self, bytes: &[u8], now: Instant) -> Vec<InputEvent> {
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
}
