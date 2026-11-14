//! How accurate and how fast a curated model is, as Handy measured it; the
//! grade and meter bands are Handy's too (`accuracyGrade` and
//! `speedMeterLevel` in @handy-computer/models 0.2.3).

use serde::{Deserialize, Serialize};

/// Steps on each meter.
pub const STEPS: u8 = 5;

/// The recording the speed is timed on, in seconds.
pub const DICTATION_SECONDS: f64 = 30.;

/// The most words wrong, in percent, for an A, B, C and D; more is an F.
pub const GRADE_CEILINGS: [f64; 4] = [5., 10., 20., 30.];

/// Seconds to transcribe [`DICTATION_SECONDS`] of speech: each one beaten
/// is a speed step.
pub const SPEED_CUTOFFS: [f64; 5] = [1.5, 3., 5., 10., 20.];

/// Handy's measurements of one model file.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Benchmark {
    /// Words wrong in English, in percent (FLEURS test set).
    pub english_error: f64,
    /// How many times faster than real time it transcribes on the benchmark
    /// laptop's graphics chip (Vulkan).
    pub gpu_speed: f64,
    /// The same on that laptop's processor alone.
    pub cpu_speed: f64,
}

/// What a model runs on, for its speed: a graphics chip or the processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Gpu,
    Cpu,
}

impl Benchmark {
    /// The English grade: A under 5% wrong, B under 10, C under 20, D under
    /// 30, F beyond. A to C carry a + below the band's ceiling divided by
    /// ∛2 twice, and a − from the ceiling divided by ∛2 once.
    pub fn grade(&self) -> &'static str {
        const SPLIT: [[&str; 3]; 3] = [["A+", "A", "A−"], ["B+", "B", "B−"], ["C+", "C", "C−"]];
        let error = self.english_error;
        let step = 2f64.cbrt();
        match band(error) {
            band @ 0..3 => {
                let ceiling = GRADE_CEILINGS[band];
                let sign = if error < ceiling / (step * step) {
                    0
                } else if error < ceiling / step {
                    1
                } else {
                    2
                };
                SPLIT[band][sign]
            }
            3 => "D",
            _ => "F",
        }
    }

    /// Accuracy on the meter: [`STEPS`] for an A, down to 1 for an F.
    pub fn accuracy_steps(&self) -> u8 {
        // Five bands: A to D, and F past the last ceiling.
        STEPS - u8::try_from(band(self.english_error)).unwrap_or(STEPS - 1)
    }

    /// Seconds [`DICTATION_SECONDS`] of speech take to transcribe on `tier`.
    pub fn seconds(&self, tier: Tier) -> f64 {
        let speed = match tier {
            Tier::Gpu => self.gpu_speed,
            Tier::Cpu => self.cpu_speed,
        };
        DICTATION_SECONDS / speed
    }

    /// Speed on the meter: how many of [`SPEED_CUTOFFS`] the transcription
    /// beats on `tier`, 0 to [`STEPS`].
    pub fn speed_steps(&self, tier: Tier) -> u8 {
        let seconds = self.seconds(tier);
        let beaten = SPEED_CUTOFFS.iter().filter(|&&cutoff| seconds < cutoff);
        u8::try_from(beaten.count()).unwrap_or(STEPS)
    }

    /// Whether these are numbers a benchmark can produce.
    pub(crate) fn is_valid(&self) -> bool {
        (0. ..=100.).contains(&self.english_error) && self.gpu_speed > 0. && self.cpu_speed > 0.
    }
}

/// The grade band of `error`: 0 for an A to 3 for a D, 4 for an F.
fn band(error: f64) -> usize {
    GRADE_CEILINGS
        .iter()
        .position(|&ceiling| error < ceiling)
        .unwrap_or(GRADE_CEILINGS.len())
}

#[cfg(test)]
mod tests {
    use super::{Benchmark, Tier};

    fn measured(english_error: f64, gpu_speed: f64) -> Benchmark {
        Benchmark {
            english_error,
            gpu_speed,
            cpu_speed: gpu_speed / 2.,
        }
    }

    #[test]
    fn grades_split_handys_bands() {
        let grade = |error| measured(error, 1.).grade();
        let grades = [3., 3.23, 3.99, 6.11, 7.9, 9.99, 15., 25., 30., 75.].map(grade);
        assert_eq!(
            grades,
            ["A+", "A", "A−", "B+", "B", "B−", "C", "D", "F", "F"]
        );
    }

    #[test]
    fn both_meters_have_five_steps() {
        let accuracy = [4.9, 9.9, 19.9, 29.9, 30.].map(|e| measured(e, 1.).accuracy_steps());
        assert_eq!(accuracy, [5, 4, 3, 2, 1]);
        // 30 s of speech in 1, 2, 4, 6, 15 and 30 s.
        let speed = [30., 15., 7.5, 5., 2., 1.].map(|x| measured(1., x).speed_steps(Tier::Gpu));
        assert_eq!(speed, [5, 4, 3, 2, 1, 0]);
        // At half the speed, 30 s take 2 s: under 3 s, not under 1.5.
        assert_eq!(measured(1., 30.).speed_steps(Tier::Cpu), 4);
    }

    #[test]
    fn impossible_numbers_are_refused() {
        assert!(measured(3., 20.).is_valid());
        assert!(!measured(-1., 20.).is_valid());
        assert!(!measured(101., 20.).is_valid());
        assert!(!measured(3., 0.).is_valid());
    }
}
