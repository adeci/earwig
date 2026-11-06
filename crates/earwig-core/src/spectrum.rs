//! The voice as the overlay draws it: speech bands measured on the raw input
//! as it arrives, before any resampling, and handed to the UI through a
//! [`Feed`] that only ever holds the latest reading.
//!
//! Each band is a constant-Q band-pass filter followed by a power follower
//! whose time constant is about a period and a half of the band, so a sound
//! shows within a few milliseconds and low bands do not flicker. The work is
//! per sample and allocation-free; readings are taken whenever a buffer has
//! been analysed.

use std::f64::consts::TAU;
use std::sync::{Arc, Mutex, PoisonError};

/// Number of speech bands, log-spaced from `LOWEST` to `HIGHEST` Hz.
pub const BANDS: usize = 24;
const LOWEST: f64 = 80.;
const HIGHEST: f64 = 5_000.;
/// Power followers settle within about this many periods of their band...
const FOLLOW_PERIODS: f64 = 1.5;
/// ...but never take less than this many seconds, which keeps high bands
/// from tracking single cycles.
const MIN_FOLLOW: f64 = 0.003;
/// The overall level follows the voice this quickly, in seconds.
const LEVEL_FOLLOW: f64 = 0.01;
/// Speech loses energy toward high frequencies; lifting the bands above
/// this frequency by `TILT_PER_OCTAVE` dB per octave lets every band of a
/// voice reach a similar height.
const TILT_FROM: f64 = 500.;
const TILT_PER_OCTAVE: f64 = 4.;
/// A band at `BAND_FLOOR` dB (after the tilt) shows nothing, one at
/// `BAND_FLOOR + BAND_SPAN` reaches the top.
const BAND_FLOOR: f64 = -52.;
const BAND_SPAN: f64 = 32.;
/// Below this overall level (dBFS) every band is zero, so room noise draws
/// nothing; bands fade in over `GATE_KNEE` dB above it.
const GATE: f64 = -45.;
const GATE_KNEE: f64 = 6.;
/// [`crate::events::Event::Level`] is the RMS of this many windows a second.
const LEVEL_HZ: u32 = 20;

/// The voice at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spectrum {
    /// Overall level on the scale of [`crate::events::Event::Level`]: 0..1
    /// over -60..0 dBFS.
    pub level: f32,
    /// How far each band stands above the voice floor, 0 (nothing to show)
    /// to 1, from low to high frequencies.
    pub bands: [f32; BANDS],
}

impl Spectrum {
    /// Whether the level is below the gate: every band is zero.
    pub fn is_quiet(&self) -> bool {
        f64::from(self.level) <= level_of(GATE)
    }
}

/// `dbfs` on the 0..1 scale of [`Spectrum::level`].
fn level_of(dbfs: f64) -> f64 {
    ((dbfs + 60.) / 60.).clamp(0., 1.)
}

/// Turns raw samples at one rate into [`Spectrum`] readings and the
/// windowed levels of [`crate::events::Event::Level`].
pub(crate) struct Analyzer {
    rate: u32,
    filters: [Bandpass; BANDS],
    /// Smoothed power of each band's output, and how much of the way to a
    /// new sample's power it moves.
    power: [f64; BANDS],
    follow: [f64; BANDS],
    tilt: [f64; BANDS],
    level_power: f64,
    level_follow: f64,
    window: Window,
}

/// Centre frequency of `band`, in Hz.
#[allow(clippy::cast_precision_loss)]
fn centre(band: usize) -> f64 {
    LOWEST * (HIGHEST / LOWEST).powf(band as f64 / (BANDS - 1) as f64)
}

impl Analyzer {
    pub(crate) fn new(rate: u32) -> Self {
        let fs = f64::from(rate.max(1));
        // Neighbouring bands meet where each is 3 dB down.
        let ratio = centre(1) / centre(0);
        let q = 1. / (ratio.sqrt() - 1. / ratio.sqrt());
        Self {
            rate,
            filters: std::array::from_fn(|band| Bandpass::new(centre(band), q, fs)),
            power: [0.; BANDS],
            follow: std::array::from_fn(|band| {
                follow_rate((FOLLOW_PERIODS / centre(band)).max(MIN_FOLLOW), fs)
            }),
            tilt: std::array::from_fn(|band| {
                TILT_PER_OCTAVE * (centre(band) / TILT_FROM).log2().max(0.)
            }),
            level_power: 0.,
            level_follow: follow_rate(LEVEL_FOLLOW, fs),
            window: Window::new(rate),
        }
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    /// Analyse `pcm`; `on_level` receives each completed level window.
    pub(crate) fn push(&mut self, pcm: &[f32], mut on_level: impl FnMut(f32)) {
        for &sample in pcm {
            let x = f64::from(sample);
            let square = x * x;
            for ((filter, power), follow) in self
                .filters
                .iter_mut()
                .zip(&mut self.power)
                .zip(&self.follow)
            {
                let y = filter.process(x);
                *power += (y * y - *power) * follow;
            }
            self.level_power += (square - self.level_power) * self.level_follow;
            if let Some(level) = self.window.push(square) {
                on_level(level);
            }
        }
    }

    /// The voice as of the last sample pushed.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn spectrum(&self) -> Spectrum {
        let level_db = decibels(self.level_power);
        let gate = ((level_db - GATE) / GATE_KNEE).clamp(0., 1.);
        let mut bands = [0.; BANDS];
        if gate > 0. {
            for ((band, power), tilt) in bands.iter_mut().zip(&self.power).zip(&self.tilt) {
                let height = (decibels(*power) + tilt - BAND_FLOOR) / BAND_SPAN;
                *band = (height.clamp(0., 1.) * gate) as f32;
            }
        }
        Spectrum {
            level: level_of(level_db) as f32,
            bands,
        }
    }
}

fn decibels(power: f64) -> f64 {
    10. * power.max(1e-12).log10()
}

/// The per-sample step of a one-pole follower with time constant `seconds`.
fn follow_rate(seconds: f64, fs: f64) -> f64 {
    1. - (-1. / (seconds * fs)).exp()
}

/// A band-pass biquad with 0 dB gain at its centre (RBJ cookbook), in
/// transposed direct form II. A band too close to Nyquist passes nothing.
#[derive(Clone, Copy)]
struct Bandpass {
    b0: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Bandpass {
    fn new(centre: f64, q: f64, fs: f64) -> Self {
        if centre >= 0.45 * fs {
            return Self {
                b0: 0.,
                a1: 0.,
                a2: 0.,
                z1: 0.,
                z2: 0.,
            };
        }
        let w0 = TAU * centre / fs;
        let alpha = w0.sin() / (2. * q);
        let a0 = 1. + alpha;
        Self {
            b0: alpha / a0,
            a1: -2. * w0.cos() / a0,
            a2: (1. - alpha) / a0,
            z1: 0.,
            z2: 0.,
        }
    }

    /// `b1` is 0 and `b2` is `-b0`.
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.z2 - self.a1 * y;
        self.z2 = -self.b0 * x - self.a2 * y;
        y
    }
}

/// RMS over consecutive windows of 1/`LEVEL_HZ` s, mapped like
/// [`Spectrum::level`].
struct Window {
    length: usize,
    sum: f64,
    count: usize,
}

impl Window {
    fn new(rate: u32) -> Self {
        Self {
            length: (rate / LEVEL_HZ).max(1) as usize,
            sum: 0.,
            count: 0,
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn push(&mut self, square: f64) -> Option<f32> {
        self.sum += square;
        self.count += 1;
        if self.count < self.length {
            return None;
        }
        let level = level_of(decibels(self.sum / self.length as f64)) as f32;
        self.sum = 0.;
        self.count = 0;
        Some(level)
    }
}

/// The latest [`Spectrum`] of the current recording, for the overlay. The
/// audio thread overwrites it; [`Feed::wakes`] fires when there is a new
/// one worth drawing and never holds more than one wake-up, so a busy UI
/// reads the newest reading instead of working through a backlog.
#[derive(Clone)]
pub struct Feed {
    shared: Arc<Shared>,
}

struct Shared {
    latest: Mutex<Spectrum>,
    wake: flume::Sender<()>,
    wakes: flume::Receiver<()>,
}

impl Default for Feed {
    fn default() -> Self {
        let (wake, wakes) = flume::bounded(1);
        Self {
            shared: Arc::new(Shared {
                latest: Mutex::new(Spectrum::default()),
                wake,
                wakes,
            }),
        }
    }
}

impl Feed {
    pub fn latest(&self) -> Spectrum {
        *self.lock()
    }

    /// Ready once a reading arrived that is not quiet, or that ends a run
    /// of ones that were not.
    pub fn wakes(&self) -> flume::Receiver<()> {
        self.shared.wakes.clone()
    }

    pub(crate) fn publish(&self, spectrum: Spectrum) {
        let previous = std::mem::replace(&mut *self.lock(), spectrum);
        if !(spectrum.is_quiet() && previous.is_quiet()) {
            let _ = self.shared.wake.try_send(());
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Spectrum> {
        self.shared
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn tone(rate: u32, hz: f64, seconds: f64, amplitude: f64) -> Vec<f32> {
        let n = (f64::from(rate) * seconds) as usize;
        (0..n)
            .map(|i| (amplitude * (TAU * hz * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    fn loudest(spectrum: &Spectrum) -> usize {
        (0..BANDS)
            .max_by(|&a, &b| spectrum.bands[a].total_cmp(&spectrum.bands[b]))
            .unwrap()
    }

    #[test]
    fn a_pure_tone_lights_its_own_band() {
        for rate in [16_000, 44_100, RATE] {
            for band in [0, 4, 9, 14, BANDS - 1] {
                let mut analyzer = Analyzer::new(rate);
                // About -33 dBFS RMS: a voice, without saturating any band.
                analyzer.push(&tone(rate, centre(band), 0.3, 0.03), |_| {});
                let spectrum = analyzer.spectrum();
                assert_eq!(loudest(&spectrum), band, "{rate} Hz: {spectrum:?}");
                assert!(spectrum.bands[band] > 0.5, "{rate} Hz: {spectrum:?}");
                // Two bands away there is little left.
                for other in [band.checked_sub(2), Some(band + 2)].into_iter().flatten() {
                    if other < BANDS {
                        assert!(
                            spectrum.bands[other] < spectrum.bands[band] * 0.7,
                            "{rate} Hz, band {band} vs {other}: {spectrum:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn silence_and_room_noise_show_nothing() {
        let mut analyzer = Analyzer::new(RATE);
        analyzer.push(&vec![0.; RATE as usize / 2], |_| {});
        let silent = analyzer.spectrum();
        assert_eq!(silent.bands, [0.; BANDS]);
        assert!(silent.is_quiet());

        // A hum and some hiss well below speech.
        let mut noise = tone(RATE, 120., 0.5, 0.004);
        let mut seed = 1_u32;
        for s in &mut noise {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            #[allow(clippy::cast_precision_loss)]
            let white = (seed >> 8) as f32 / (1 << 24) as f32 - 0.5;
            *s += white * 0.004;
        }
        analyzer.push(&noise, |_| {});
        let room = analyzer.spectrum();
        assert_eq!(room.bands, [0.; BANDS], "{room:?}");
        assert!(room.is_quiet());
    }

    #[test]
    fn a_voice_shows_within_a_few_milliseconds() {
        let mut analyzer = Analyzer::new(RATE);
        analyzer.push(&vec![0.; RATE as usize / 10], |_| {});
        // 5 ms of a 300 Hz tone at speech level.
        analyzer.push(&tone(RATE, 300., 0.005, 0.05), |_| {});
        let spectrum = analyzer.spectrum();
        assert!(!spectrum.is_quiet());
        assert!(spectrum.bands.iter().any(|&b| b > 0.3), "{spectrum:?}");
    }

    #[test]
    fn bands_above_nyquist_stay_dark() {
        let mut analyzer = Analyzer::new(8_000);
        analyzer.push(&tone(8_000, 3_000., 0.2, 0.2), |_| {});
        let spectrum = analyzer.spectrum();
        assert!(spectrum.bands.iter().all(|b| b.is_finite()));
        assert_eq!(spectrum.bands[BANDS - 1], 0., "{spectrum:?}");
    }

    #[test]
    fn level_windows_track_loudness() {
        let rate = crate::SAMPLE_RATE;
        let mut analyzer = Analyzer::new(rate);
        let mut levels = Vec::new();
        analyzer.push(&vec![0.; rate as usize / 20], |l| levels.push(l));
        analyzer.push(&tone(rate, 440., 0.05, 0.9), |l| levels.push(l));
        analyzer.push(&tone(rate, 440., 0.05, 0.01), |l| levels.push(l));
        assert_eq!(levels.len(), 3, "{levels:?}");
        assert!(levels[0].abs() < f32::EPSILON, "{levels:?}");
        assert!(levels[1] > 0.9, "{levels:?}");
        assert!(levels[2] > 0.2 && levels[2] < 0.5, "{levels:?}");
    }

    #[test]
    fn the_feed_keeps_the_latest_and_wakes_once() {
        let feed = Feed::default();
        let wakes = feed.wakes();
        let quiet = Spectrum::default();
        feed.publish(quiet);
        assert!(wakes.try_recv().is_err(), "quiet after quiet wakes nobody");

        let mut loud = Spectrum {
            level: 0.8,
            ..Spectrum::default()
        };
        for value in [0.2, 0.5, 0.9] {
            loud.bands[3] = value;
            feed.publish(loud);
        }
        assert_eq!(feed.latest(), loud);
        assert!(wakes.try_recv().is_ok());
        assert!(wakes.try_recv().is_err(), "wake-ups do not queue");

        feed.publish(quiet);
        assert!(wakes.try_recv().is_ok(), "falling silent is news");
        assert_eq!(feed.latest(), quiet);
    }
}
