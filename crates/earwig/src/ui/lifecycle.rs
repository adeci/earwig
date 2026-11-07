//! A dictation's pill from its recording to its window closing. Pure state,
//! fed the daemon's dictation states and the clock, so the rules are
//! testable without GPUI; the overlay keeps one and draws what it says, and
//! [`ui`](crate::ui) closes the window once it says so.
//!
//! Finishing (a release, a tap, `earwig release` or `toggle`) switches the
//! pill off at once; the final text and its insertion carry on unseen, and
//! the window closes once the pill is off and the dictation is over. A
//! failed insertion brings the pill back for a moment to say so. A
//! recording dropped before it was finished (`earwig cancel`, a chord)
//! closes it at once, as its × does.

use std::time::{Duration, Instant};

/// How long a failed insertion stays up: it carries a sentence to read.
pub const FAILURE: Duration = Duration::from_millis(1800);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Recording: the live pill.
    Live,
    /// Switching off from `since` until `until` (see [`crt`](crate::ui::crt)),
    /// the failure notice if `failure` (whether the text is on the
    /// clipboard), else the live pill as it was.
    SwitchingOff {
        since: Instant,
        until: Instant,
        failure: Option<bool>,
    },
    /// Gone from the screen; the window waits, empty and click-through, in
    /// case the insertion fails.
    Off,
    /// The insertion failed; the pill says so until `until`.
    Failed { on_clipboard: bool, until: Instant },
    /// Over: the window closes.
    Closed,
}

#[derive(Debug)]
pub struct Lifecycle {
    stage: Stage,
    /// The daemon is done with the dictation.
    over: bool,
    /// A failure reported while the pill was switching off, shown once it
    /// is off: the animation always plays out.
    reported: Option<bool>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            stage: Stage::Live,
            over: false,
            reported: None,
        }
    }
}

impl Lifecycle {
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// A recording: the live pill, whatever came before.
    pub fn recording(&mut self) {
        *self = Self::default();
    }

    /// The recording was finished: switch off for `switch_off` from `now`
    /// (zero is at once).
    pub fn finish(&mut self, now: Instant, switch_off: Duration) {
        if self.stage == Stage::Live {
            self.stage = switching_off(now, switch_off, None);
            self.advance(now, switch_off);
        }
    }

    /// The final text could not be inserted.
    pub fn failed(&mut self, on_clipboard: bool, now: Instant) {
        match self.stage {
            Stage::SwitchingOff { .. } => self.reported = Some(on_clipboard),
            Stage::Live | Stage::Off | Stage::Failed { .. } => {
                self.stage = Stage::Failed {
                    on_clipboard,
                    until: now + FAILURE,
                };
            }
            Stage::Closed => {}
        }
    }

    /// The daemon went idle: the dictation is over, whether its text was
    /// inserted, failed, came out empty or was dropped.
    pub fn idle(&mut self) {
        match self.stage {
            // Dropped before it was finished, or off with nothing more to
            // say: done.
            Stage::Live | Stage::Off => self.stage = Stage::Closed,
            Stage::SwitchingOff { .. } | Stage::Failed { .. } => self.over = true,
            Stage::Closed => {}
        }
    }

    /// Catch up with the clock: whatever was due by `now` has happened. A
    /// failure that has been up long enough switches off for `switch_off`.
    pub fn advance(&mut self, now: Instant, switch_off: Duration) {
        if let Stage::Failed {
            on_clipboard,
            until,
        } = self.stage
            && now >= until
        {
            self.stage = switching_off(now, switch_off, Some(on_clipboard));
        }
        if let Stage::SwitchingOff { until, .. } = self.stage
            && now >= until
        {
            self.stage = match self.reported.take() {
                Some(on_clipboard) => Stage::Failed {
                    on_clipboard,
                    until: now + FAILURE,
                },
                None if self.over => Stage::Closed,
                None => Stage::Off,
            };
        }
    }

    /// When [`Lifecycle::advance`] next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        match self.stage {
            Stage::SwitchingOff { until, .. } | Stage::Failed { until, .. } => Some(until),
            Stage::Live | Stage::Off | Stage::Closed => None,
        }
    }
}

fn switching_off(now: Instant, duration: Duration, failure: Option<bool>) -> Stage {
    Stage::SwitchingOff {
        since: now,
        until: now + duration,
        failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANIMATED: Duration = Duration::from_millis(360);

    struct Clock(Instant);

    impl Clock {
        fn at(&self, ms: u64) -> Instant {
            self.0 + Duration::from_millis(ms)
        }
    }

    fn finished(switch_off: Duration) -> (Lifecycle, Clock) {
        let clock = Clock(Instant::now());
        let mut life = Lifecycle::default();
        life.finish(clock.at(0), switch_off);
        (life, clock)
    }

    #[test]
    fn finishing_switches_off_at_once_and_closes_without_waiting_for_the_insertion() {
        let (mut life, t) = finished(ANIMATED);
        assert_eq!(
            life.stage(),
            Stage::SwitchingOff {
                since: t.at(0),
                until: t.at(360),
                failure: None,
            }
        );
        assert_eq!(life.deadline(), Some(t.at(360)));
        life.advance(t.at(359), ANIMATED);
        assert!(matches!(life.stage(), Stage::SwitchingOff { .. }));
        // Off the screen though the text is still on its way...
        life.advance(t.at(360), ANIMATED);
        assert_eq!(life.stage(), Stage::Off);
        assert_eq!(life.deadline(), None);
        // ...and the window goes once it has landed.
        life.idle();
        assert_eq!(life.stage(), Stage::Closed);

        // Inserted before the pill was off: the animation plays out, then
        // the window goes.
        let (mut life, t) = finished(ANIMATED);
        life.idle();
        assert!(matches!(life.stage(), Stage::SwitchingOff { .. }));
        life.advance(t.at(360), ANIMATED);
        assert_eq!(life.stage(), Stage::Closed);
    }

    #[test]
    fn without_the_animation_the_pill_is_off_at_once() {
        let (mut life, _) = finished(Duration::ZERO);
        assert_eq!(life.stage(), Stage::Off);
        life.idle();
        assert_eq!(life.stage(), Stage::Closed);
    }

    #[test]
    fn a_recording_dropped_before_it_was_finished_closes_at_once() {
        let mut life = Lifecycle::default();
        life.idle();
        assert_eq!(life.stage(), Stage::Closed);
    }

    #[test]
    fn a_failed_insertion_comes_back_to_say_so_then_switches_off() {
        let (mut life, t) = finished(ANIMATED);
        life.advance(t.at(360), ANIMATED);
        life.failed(true, t.at(900));
        let failed = Stage::Failed {
            on_clipboard: true,
            until: t.at(900) + FAILURE,
        };
        assert_eq!(life.stage(), failed);
        life.idle();
        assert_eq!(life.stage(), failed, "the sentence stays up to be read");
        let read = t.at(900) + FAILURE;
        assert_eq!(life.deadline(), Some(read));
        life.advance(read, ANIMATED);
        // It switches off still saying so.
        assert_eq!(
            life.stage(),
            Stage::SwitchingOff {
                since: read,
                until: read + ANIMATED,
                failure: Some(true),
            }
        );
        life.advance(read + ANIMATED, ANIMATED);
        assert_eq!(life.stage(), Stage::Closed);
    }

    #[test]
    fn a_failure_while_switching_off_shows_once_the_pill_is_off() {
        let (mut life, t) = finished(ANIMATED);
        life.failed(false, t.at(120));
        life.idle();
        assert!(matches!(life.stage(), Stage::SwitchingOff { .. }));
        life.advance(t.at(365), ANIMATED);
        assert_eq!(
            life.stage(),
            Stage::Failed {
                on_clipboard: false,
                until: t.at(365) + FAILURE
            }
        );
        // Without the animation it is gone at once after the reading.
        life.advance(t.at(365) + FAILURE, Duration::ZERO);
        assert_eq!(life.stage(), Stage::Closed);
    }

    #[test]
    fn a_new_dictation_while_switching_off_is_live_and_stays_up() {
        let (mut life, t) = finished(ANIMATED);
        life.idle();
        life.recording();
        assert_eq!(life.stage(), Stage::Live);
        assert_eq!(life.deadline(), None);
        // The old switch-off's wake-up comes and goes.
        life.advance(t.at(400), ANIMATED);
        assert_eq!(life.stage(), Stage::Live);
        // It finishes like any other.
        life.finish(t.at(2_000), ANIMATED);
        life.advance(t.at(2_360), ANIMATED);
        assert_eq!(life.stage(), Stage::Off, "not closed by the last one");

        // Likewise over a failure being shown.
        let (mut life, t) = finished(Duration::ZERO);
        life.failed(true, t.at(50));
        life.recording();
        assert_eq!(life.stage(), Stage::Live);
        life.advance(t.at(50) + FAILURE, ANIMATED);
        assert_eq!(life.stage(), Stage::Live);
    }
}
