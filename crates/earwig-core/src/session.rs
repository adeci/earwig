//! Tap-or-hold: turns hotkey edges into recording actions (see
//! [`ActivationMode`]). Pure state: the caller passes each edge's time. A
//! pressed key ignores further presses until its release, which absorbs key
//! repeat and the release that follows a stopping press.

use std::time::{Duration, Instant};

use crate::config::ActivationMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Press,
    Release,
    Toggle,
    Cancel,
    Chord,
}

/// What the caller should do after an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start {
        latched: bool,
    },
    /// The recording continues, now latched on until the next press.
    Latch,
    /// Stop recording and produce the final text. Call
    /// [`Session::finished`] once it is inserted.
    Finish,
    /// Drop the recording, or the result being finalized, without inserting.
    Discard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Recording {
        latched: bool,
        /// When the press that started it happened.
        since: Instant,
    },
    Finalizing,
}

#[derive(Clone, Debug)]
pub struct Session {
    mode: ActivationMode,
    hold_threshold: Duration,
    phase: Phase,
    /// A press was accepted and its release has not arrived yet.
    key_down: bool,
}

impl Session {
    pub fn new(mode: ActivationMode, hold_threshold_ms: u64) -> Self {
        Self {
            mode,
            hold_threshold: Duration::from_millis(hold_threshold_ms),
            phase: Phase::Idle,
            key_down: false,
        }
    }

    /// Change the activation rules; applies from the next input.
    pub fn configure(&mut self, mode: ActivationMode, hold_threshold_ms: u64) {
        self.mode = mode;
        self.hold_threshold = Duration::from_millis(hold_threshold_ms);
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn handle(&mut self, input: Input, now: Instant) -> Option<Action> {
        match input {
            Input::Press => self.press(now),
            Input::Release => self.release(now),
            Input::Toggle => self.toggle(now),
            Input::Cancel => self.cancel(),
            Input::Chord => self.chord(),
        }
    }

    /// The final text was inserted (or dropped): back to idle.
    pub fn finished(&mut self) {
        if self.phase == Phase::Finalizing {
            self.phase = Phase::Idle;
        }
    }

    /// Abandon whatever is going on without an action, e.g. when recording
    /// could not start.
    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
    }

    fn press(&mut self, now: Instant) -> Option<Action> {
        if self.key_down {
            return None;
        }
        self.key_down = true;
        match self.phase {
            Phase::Idle => {
                let latched = self.mode == ActivationMode::Toggle;
                self.phase = Phase::Recording {
                    latched,
                    since: now,
                };
                Some(Action::Start { latched })
            }
            Phase::Recording { latched: true, .. } => {
                self.phase = Phase::Finalizing;
                Some(Action::Finish)
            }
            Phase::Recording { latched: false, .. } | Phase::Finalizing => None,
        }
    }

    fn release(&mut self, now: Instant) -> Option<Action> {
        if !std::mem::replace(&mut self.key_down, false) {
            return None;
        }
        let Phase::Recording {
            latched: false,
            since,
        } = self.phase
        else {
            return None;
        };
        let tap = now.saturating_duration_since(since) < self.hold_threshold;
        if tap && self.mode == ActivationMode::HoldOrToggle {
            self.phase = Phase::Recording {
                latched: true,
                since,
            };
            Some(Action::Latch)
        } else {
            self.phase = Phase::Finalizing;
            Some(Action::Finish)
        }
    }

    fn toggle(&mut self, now: Instant) -> Option<Action> {
        match self.phase {
            Phase::Idle => {
                self.phase = Phase::Recording {
                    latched: true,
                    since: now,
                };
                Some(Action::Start { latched: true })
            }
            Phase::Recording { .. } => {
                self.phase = Phase::Finalizing;
                Some(Action::Finish)
            }
            Phase::Finalizing => None,
        }
    }

    /// While the key is down, a recording can only be one its press
    /// started: a press that stops a latched recording finalizes it.
    fn chord(&mut self) -> Option<Action> {
        if !std::mem::replace(&mut self.key_down, false) {
            return None;
        }
        match self.phase {
            Phase::Recording { .. } => {
                self.phase = Phase::Idle;
                Some(Action::Discard)
            }
            Phase::Idle | Phase::Finalizing => None,
        }
    }

    fn cancel(&mut self) -> Option<Action> {
        match self.phase {
            Phase::Idle => None,
            Phase::Recording { .. } | Phase::Finalizing => {
                self.phase = Phase::Idle;
                Some(Action::Discard)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: u64 = 300;

    struct Clock(Instant);

    impl Clock {
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    fn session(mode: ActivationMode) -> (Session, Clock) {
        (Session::new(mode, THRESHOLD), Clock(Instant::now()))
    }

    #[test]
    fn hold_past_threshold_finishes_on_release() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        assert_eq!(
            s.handle(Input::Press, t.at(0)),
            Some(Action::Start { latched: false })
        );
        assert_eq!(
            s.handle(Input::Release, t.at(THRESHOLD)),
            Some(Action::Finish)
        );
        assert_eq!(s.phase(), Phase::Finalizing);
        s.finished();
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn tap_latches_and_next_press_stops_ignoring_its_release() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        s.handle(Input::Press, t.at(0));
        assert_eq!(
            s.handle(Input::Release, t.at(THRESHOLD - 1)),
            Some(Action::Latch)
        );
        assert!(matches!(s.phase(), Phase::Recording { latched: true, .. }));
        assert_eq!(s.handle(Input::Press, t.at(5_000)), Some(Action::Finish));
        s.finished();
        assert_eq!(s.handle(Input::Release, t.at(6_000)), None);
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn key_repeat_while_held_is_ignored() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        s.handle(Input::Press, t.at(0));
        assert_eq!(s.handle(Input::Press, t.at(600)), None);
        assert_eq!(s.handle(Input::Press, t.at(630)), None);
        assert_eq!(s.handle(Input::Release, t.at(900)), Some(Action::Finish));
    }

    #[test]
    fn hold_mode_finishes_on_any_release() {
        let (mut s, t) = session(ActivationMode::Hold);
        assert_eq!(
            s.handle(Input::Press, t.at(0)),
            Some(Action::Start { latched: false })
        );
        assert_eq!(s.handle(Input::Release, t.at(10)), Some(Action::Finish));
    }

    #[test]
    fn toggle_mode_starts_and_stops_on_presses() {
        let (mut s, t) = session(ActivationMode::Toggle);
        assert_eq!(
            s.handle(Input::Press, t.at(0)),
            Some(Action::Start { latched: true })
        );
        assert_eq!(s.handle(Input::Release, t.at(2_000)), None);
        assert_eq!(s.handle(Input::Press, t.at(4_000)), Some(Action::Finish));
        assert_eq!(s.handle(Input::Release, t.at(4_050)), None);
    }

    #[test]
    fn toggle_input_works_in_every_mode() {
        for mode in [
            ActivationMode::HoldOrToggle,
            ActivationMode::Hold,
            ActivationMode::Toggle,
        ] {
            let (mut s, t) = session(mode);
            assert_eq!(
                s.handle(Input::Toggle, t.at(0)),
                Some(Action::Start { latched: true })
            );
            assert_eq!(s.handle(Input::Toggle, t.at(10)), Some(Action::Finish));
            assert_eq!(s.handle(Input::Toggle, t.at(20)), None, "{mode:?}");
        }
    }

    #[test]
    fn cancel_from_every_state() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        assert_eq!(s.handle(Input::Cancel, t.at(0)), None);

        // Held recording; the key's release afterwards does nothing.
        s.handle(Input::Press, t.at(0));
        assert_eq!(s.handle(Input::Cancel, t.at(100)), Some(Action::Discard));
        assert_eq!(s.phase(), Phase::Idle);
        assert_eq!(s.handle(Input::Release, t.at(200)), None);

        // Latched recording.
        s.handle(Input::Press, t.at(1_000));
        s.handle(Input::Release, t.at(1_050));
        assert_eq!(s.handle(Input::Cancel, t.at(1_100)), Some(Action::Discard));
        assert_eq!(s.phase(), Phase::Idle);

        // Finalizing.
        s.handle(Input::Press, t.at(2_000));
        s.handle(Input::Release, t.at(3_000));
        assert_eq!(s.handle(Input::Cancel, t.at(3_010)), Some(Action::Discard));
        assert_eq!(s.phase(), Phase::Idle);
    }

    #[test]
    fn a_chord_takes_back_the_press_that_started_recording() {
        for mode in [
            ActivationMode::HoldOrToggle,
            ActivationMode::Hold,
            ActivationMode::Toggle,
        ] {
            let (mut s, t) = session(mode);
            s.handle(Input::Press, t.at(0));
            assert_eq!(
                s.handle(Input::Chord, t.at(80)),
                Some(Action::Discard),
                "{mode:?}"
            );
            assert_eq!(s.handle(Input::Release, t.at(150)), None);
            assert_eq!(s.phase(), Phase::Idle);
        }
    }

    #[test]
    fn a_chord_after_a_stopping_press_keeps_the_dictation() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        s.handle(Input::Press, t.at(0));
        s.handle(Input::Release, t.at(50));
        assert_eq!(s.handle(Input::Press, t.at(5_000)), Some(Action::Finish));
        assert_eq!(s.handle(Input::Chord, t.at(5_080)), None);
        assert_eq!(s.phase(), Phase::Finalizing);
        s.finished();
        assert_eq!(s.handle(Input::Release, t.at(5_200)), None);
        assert_eq!(
            s.handle(Input::Press, t.at(6_000)),
            Some(Action::Start { latched: false })
        );
    }

    #[test]
    fn a_chord_without_a_held_key_is_ignored() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        assert_eq!(s.handle(Input::Chord, t.at(0)), None);
        s.handle(Input::Press, t.at(10));
        s.handle(Input::Release, t.at(20));
        assert_eq!(s.handle(Input::Chord, t.at(30)), None);
        assert!(matches!(s.phase(), Phase::Recording { latched: true, .. }));
    }

    #[test]
    fn release_without_press_is_ignored() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        assert_eq!(s.handle(Input::Release, t.at(0)), None);
        assert_eq!(s.phase(), Phase::Idle);
        assert_eq!(
            s.handle(Input::Press, t.at(10)),
            Some(Action::Start { latched: false })
        );
    }

    #[test]
    fn presses_while_finalizing_are_ignored() {
        let (mut s, t) = session(ActivationMode::HoldOrToggle);
        s.handle(Input::Press, t.at(0));
        s.handle(Input::Release, t.at(1_000));
        assert_eq!(s.handle(Input::Press, t.at(1_010)), None);
        assert_eq!(s.handle(Input::Release, t.at(1_020)), None);
        assert_eq!(s.phase(), Phase::Finalizing);
        s.finished();
        assert_eq!(
            s.handle(Input::Press, t.at(1_100)),
            Some(Action::Start { latched: false })
        );
    }
}
