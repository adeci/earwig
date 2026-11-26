//! A dictation's pill from recording to closing, as pure state the overlay draws from.

use std::time::{Duration, Instant};

/// How long a notice stays up (it's a sentence to read).
pub const NOTICE: Duration = Duration::from_millis(1800);

/// Tone of an overlay notice's status badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeTone {
    /// Informational (e.g. nothing heard).
    Muted,
    /// Recoverable caution (e.g. silent mic).
    Warning,
    /// Failure (e.g. insertion failed, model not ready).
    Danger,
}

/// A short status and sentence shown in the overlay pill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub status: &'static str,
    pub line: String,
    pub tone: NoticeTone,
}

impl Notice {
    pub fn muted(status: &'static str, line: impl Into<String>) -> Self {
        Self {
            status,
            line: line.into(),
            tone: NoticeTone::Muted,
        }
    }

    pub fn warning(status: &'static str, line: impl Into<String>) -> Self {
        Self {
            status,
            line: line.into(),
            tone: NoticeTone::Warning,
        }
    }

    pub fn danger(status: &'static str, line: impl Into<String>) -> Self {
        Self {
            status,
            line: line.into(),
            tone: NoticeTone::Danger,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    Live,
    /// With `notice` it's the notice collapsing, else the live pill as it was.
    SwitchingOff {
        since: Instant,
        until: Instant,
        notice: Option<Notice>,
    },
    /// Gone; the empty, click-through window waits in case finalization reports a notice.
    Off,
    Notice {
        notice: Notice,
        until: Instant,
    },
    Closed,
}

#[derive(Debug)]
pub struct Lifecycle {
    stage: Stage,
    /// The daemon is done with the dictation.
    over: bool,
    /// A notice that came mid switch-off, shown once off: the animation always plays out
    /// with its own content untouched.
    queued: Option<Notice>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            stage: Stage::Live,
            over: false,
            queued: None,
        }
    }
}

impl Lifecycle {
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    pub fn recording(&mut self) {
        *self = Self::default();
    }

    /// Zero `switch_off` is instant.
    pub fn finish(&mut self, now: Instant, switch_off: Duration) {
        if self.stage == Stage::Live {
            self.stage = switching_off(now, switch_off, None);
            self.advance(now, switch_off);
        }
    }

    pub fn notice(&mut self, notice: Notice, now: Instant) {
        match self.stage {
            Stage::SwitchingOff { .. } => self.queued = Some(notice),
            Stage::Live | Stage::Off | Stage::Notice { .. } => {
                self.stage = Stage::Notice {
                    notice,
                    until: now + NOTICE,
                };
            }
            Stage::Closed => {}
        }
    }

    /// The dictation is over, whatever happened to its text.
    pub fn idle(&mut self) {
        match self.stage {
            // Cancelled while live, or already off: done.
            Stage::Live | Stage::Off => self.stage = Stage::Closed,
            Stage::SwitchingOff { .. } | Stage::Notice { .. } => self.over = true,
            Stage::Closed => {}
        }
    }

    /// Catch up with the clock; a notice that's been up long enough switches off.
    pub fn advance(&mut self, now: Instant, switch_off: Duration) {
        if matches!(&self.stage, Stage::Notice { until, .. } if now >= *until)
            && let Stage::Notice { notice, .. } = std::mem::replace(&mut self.stage, Stage::Off)
        {
            self.stage = switching_off(now, switch_off, Some(notice));
        }
        if matches!(&self.stage, Stage::SwitchingOff { until, .. } if now >= *until) {
            self.stage = match self.queued.take() {
                Some(notice) => Stage::Notice {
                    notice,
                    until: now + NOTICE,
                },
                None if self.over => Stage::Closed,
                None => Stage::Off,
            };
        }
    }

    /// When [`Lifecycle::advance`] next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        match self.stage {
            Stage::SwitchingOff { until, .. } | Stage::Notice { until, .. } => Some(until),
            Stage::Live | Stage::Off | Stage::Closed => None,
        }
    }
}

fn switching_off(now: Instant, duration: Duration, notice: Option<Notice>) -> Stage {
    Stage::SwitchingOff {
        since: now,
        until: now + duration,
        notice,
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
            *life.stage(),
            Stage::SwitchingOff {
                since: t.at(0),
                until: t.at(360),
                notice: None,
            }
        );
        assert_eq!(life.deadline(), Some(t.at(360)));
        life.advance(t.at(359), ANIMATED);
        assert!(matches!(life.stage(), Stage::SwitchingOff { .. }));
        // Off the screen though the text is still on its way...
        life.advance(t.at(360), ANIMATED);
        assert_eq!(*life.stage(), Stage::Off);
        assert_eq!(life.deadline(), None);
        // ...and the window goes once it has landed.
        life.idle();
        assert_eq!(*life.stage(), Stage::Closed);

        // Inserted before the pill was off: the animation plays out, then the window goes.
        let (mut life, t) = finished(ANIMATED);
        life.idle();
        assert!(matches!(life.stage(), Stage::SwitchingOff { .. }));
        life.advance(t.at(360), ANIMATED);
        assert_eq!(*life.stage(), Stage::Closed);
    }

    #[test]
    fn without_the_animation_the_pill_is_off_at_once() {
        let (mut life, _) = finished(Duration::ZERO);
        assert_eq!(*life.stage(), Stage::Off);
        life.idle();
        assert_eq!(*life.stage(), Stage::Closed);
    }

    #[test]
    fn a_recording_dropped_before_it_was_finished_closes_at_once() {
        let mut life = Lifecycle::default();
        life.idle();
        assert_eq!(*life.stage(), Stage::Closed);
    }

    #[test]
    fn a_notice_comes_back_to_say_so_then_switches_off() {
        let (mut life, t) = finished(ANIMATED);
        let msg = Notice::danger("Not inserted", "Text is on the clipboard.");
        life.advance(t.at(360), ANIMATED);
        life.notice(msg.clone(), t.at(900));
        let shown = Stage::Notice {
            notice: msg.clone(),
            until: t.at(900) + NOTICE,
        };
        assert_eq!(*life.stage(), shown);
        life.idle();
        assert_eq!(*life.stage(), shown, "the sentence stays up to be read");
        let read = t.at(900) + NOTICE;
        assert_eq!(life.deadline(), Some(read));
        life.advance(read, ANIMATED);
        // It switches off still saying so.
        assert_eq!(
            *life.stage(),
            Stage::SwitchingOff {
                since: read,
                until: read + ANIMATED,
                notice: Some(msg),
            }
        );
        life.advance(read + ANIMATED, ANIMATED);
        assert_eq!(*life.stage(), Stage::Closed);
    }

    #[test]
    fn a_notice_while_switching_off_shows_once_the_pill_is_off() {
        let (mut life, t) = finished(ANIMATED);
        let msg = Notice::warning("Silent mic", "No sound reached the microphone.");
        life.notice(msg.clone(), t.at(120));
        life.idle();
        assert!(matches!(
            life.stage(),
            Stage::SwitchingOff { notice: None, .. }
        ));
        life.advance(t.at(365), ANIMATED);
        assert_eq!(
            *life.stage(),
            Stage::Notice {
                notice: msg,
                until: t.at(365) + NOTICE,
            }
        );
        // Without the animation it is gone at once after the reading.
        life.advance(t.at(365) + NOTICE, Duration::ZERO);
        assert_eq!(*life.stage(), Stage::Closed);
    }

    #[test]
    fn a_new_notice_while_collapsing_an_earlier_notice_keeps_the_first_intact() {
        let clock = Clock(Instant::now());
        let first = Notice::danger("Not inserted", "First message.");
        let second = Notice::muted("Nothing heard", "Second message.");
        let mut life = Lifecycle::default();
        life.notice(first.clone(), clock.at(0));
        life.idle();
        let collapse_at = clock.at(0) + NOTICE;
        life.advance(collapse_at, ANIMATED);
        // Another notice arrives mid switch-off of the first.
        life.notice(second.clone(), collapse_at + Duration::from_millis(100));
        assert_eq!(
            *life.stage(),
            Stage::SwitchingOff {
                since: collapse_at,
                until: collapse_at + ANIMATED,
                notice: Some(first),
            },
            "the collapsing pill keeps its own notice until the animation ends"
        );
        life.advance(collapse_at + ANIMATED, ANIMATED);
        assert_eq!(
            *life.stage(),
            Stage::Notice {
                notice: second,
                until: collapse_at + ANIMATED + NOTICE,
            }
        );
    }

    #[test]
    fn a_new_dictation_while_switching_off_is_live_and_stays_up() {
        let (mut life, t) = finished(ANIMATED);
        life.idle();
        life.recording();
        assert_eq!(*life.stage(), Stage::Live);
        assert_eq!(life.deadline(), None);
        // The old switch-off's wake-up comes and goes.
        life.advance(t.at(400), ANIMATED);
        assert_eq!(*life.stage(), Stage::Live);
        // It finishes like any other.
        life.finish(t.at(2_000), ANIMATED);
        life.advance(t.at(2_360), ANIMATED);
        assert_eq!(*life.stage(), Stage::Off, "not closed by the last one");

        // Likewise over a notice being shown.
        let (mut life, t) = finished(Duration::ZERO);
        life.notice(
            Notice::muted("Nothing heard", "No speech detected."),
            t.at(50),
        );
        life.recording();
        assert_eq!(*life.stage(), Stage::Live);
        life.advance(t.at(50) + NOTICE, ANIMATED);
        assert_eq!(*life.stage(), Stage::Live);
    }
}
