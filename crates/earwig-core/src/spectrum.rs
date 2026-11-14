//! The voice as the overlay draws it: constant-Q band-pass filters with power
//! followers on the raw input (before resampling), shown only where they stand
//! above a learned noise floor, and handed over by a [`Feed`].

use std::f64::consts::TAU;
use std::sync::{Arc, Mutex, PoisonError};

/// Log-spaced speech bands from `LOWEST` to `HIGHEST` Hz.
pub const BANDS: usize = 24;
const LOWEST: f64 = 80.;
const HIGHEST: f64 = 5_000.;
/// Power followers settle in about this many periods of their band, but take
/// at least `MIN_FOLLOW` seconds so high bands don't track single cycles.
const FOLLOW_PERIODS: f64 = 1.5;
const MIN_FOLLOW: f64 = 0.003;
/// Time constant of the overall level, in seconds.
const LEVEL_FOLLOW: f64 = 0.01;
/// Speech loses energy up high: bands above `TILT_FROM` Hz get this many dB
/// per octave, so every band of a voice reaches a similar height.
const TILT_FROM: f64 = 500.;
const TILT_PER_OCTAVE: f64 = 4.;
/// A band shows from `BAND_FLOOR` dB (after the tilt), full at `+ BAND_SPAN`.
const BAND_FLOOR: f64 = -52.;
const BAND_SPAN: f64 = 32.;
/// Below this level (dBFS) nothing shows; bands fade in over `GATE_KNEE` dB.
const GATE: f64 = -45.;
const GATE_KNEE: f64 = 6.;
/// On a noisier mic the gate sits this far above the noise floor: the quietest
/// of the last `FLOOR_WINDOWS` level windows (3 s), which a voice pauses in.
const NOISE_MARGIN: f64 = 10.;
const FLOOR_WINDOWS: usize = 60;
/// The floor applies after this many windows (0.4 s) of sound, so a voice
/// there from the start has paused before any of it counts as noise.
const SEED_WINDOWS: usize = 8;
/// A window under the gate and within `ROOM` dB of the floor is the room alone:
/// each band's noise moves `BAND_NOISE_FOLLOW` of the way to its power, and
/// twice the noise is taken off the band.
const ROOM: f64 = 6.;
const BAND_NOISE_FOLLOW: f64 = 0.15;
const NOISE_OVERSUBTRACT: f64 = 2.;
/// Decaying filters go subnormal (slow) in digital silence; below this, zero.
const TINY: f64 = 1e-30;
/// Level windows per second, as [`crate::events::Event::Level`] reports them.
const LEVEL_HZ: u32 = 20;

/// The voice at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spectrum {
    /// Overall level on the scale of [`crate::events::Event::Level`]: 0..1
    /// over -60..0 dBFS.
    pub level: f32,
    /// How far each band of the voice stands above the noise, 0 (nothing to
    /// show) to 1, from low to high frequencies.
    pub bands: [f32; BANDS],
}

impl Spectrum {
    /// Whether no band shows.
    pub fn is_quiet(&self) -> bool {
        self.bands.iter().all(|&band| band == 0.)
    }
}

/// `dbfs` on the 0..1 scale of [`Spectrum::level`].
fn level_of(dbfs: f64) -> f64 {
    ((dbfs + 60.) / 60.).clamp(0., 1.)
}

/// Turns raw samples at one rate into [`Spectrum`] readings and the
/// windowed levels of [`crate::events::Event::Level`].
pub(crate) struct Analyzer {
    filters: [Bandpass; BANDS],
    /// Each band's smoothed power, and its follower's per-sample step.
    power: [f64; BANDS],
    follow: [f64; BANDS],
    tilt: [f64; BANDS],
    level_power: f64,
    level_follow: f64,
    window: Window,
    /// Power of the latest level windows (oldest at `next`), windows of sound
    /// heard (up to `SEED_WINDOWS`), and the level (dBFS) bands show above.
    recent: [f64; FLOOR_WINDOWS],
    next: usize,
    heard: usize,
    gate: f64,
    /// Each band's power while only the room sounds.
    noise: [f64; BANDS],
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
            recent: [f64::INFINITY; FLOOR_WINDOWS],
            next: 0,
            heard: 0,
            gate: GATE,
            noise: [0.; BANDS],
        }
    }

    /// Analyse `pcm`; `on_level` receives each completed level window.
    #[allow(clippy::cast_possible_truncation)]
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
            if let Some(power) = self.window.push(square) {
                self.end_window(power);
                on_level(level_of(decibels(power)) as f32);
            }
        }
    }

    /// A level window of mean `power` ended: follow the noise floor and each
    /// band's noise, and zero what decayed below `TINY`.
    fn end_window(&mut self, power: f64) {
        for filter in &mut self.filters {
            filter.flush();
        }
        for band in &mut self.power {
            flush(band);
        }
        flush(&mut self.level_power);
        // A new input may start with digital silence, which says nothing
        // about the room.
        if self.heard == 0 && power == 0. {
            return;
        }
        self.recent[self.next] = power;
        self.next = (self.next + 1) % FLOOR_WINDOWS;
        let floor = decibels(self.recent.iter().copied().fold(f64::INFINITY, f64::min));
        if self.heard < SEED_WINDOWS {
            self.heard += 1;
        } else {
            self.gate = (floor + NOISE_MARGIN).max(GATE);
        }
        if decibels(power) <= (floor + ROOM).min(self.gate) {
            for (noise, band) in self.noise.iter_mut().zip(&self.power) {
                *noise += (band - *noise) * BAND_NOISE_FOLLOW;
            }
        }
    }

    /// The voice as of the last sample pushed.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn spectrum(&self) -> Spectrum {
        let level_db = decibels(self.level_power);
        let gate = ((level_db - self.gate) / GATE_KNEE).clamp(0., 1.);
        let mut bands = [0.; BANDS];
        if gate > 0. {
            for (((band, power), noise), tilt) in bands
                .iter_mut()
                .zip(&self.power)
                .zip(&self.noise)
                .zip(&self.tilt)
            {
                let voice = power - NOISE_OVERSUBTRACT * noise;
                let height = (decibels(voice) + tilt - BAND_FLOOR) / BAND_SPAN;
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

fn flush(value: &mut f64) {
    if value.abs() < TINY {
        *value = 0.;
    }
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

    fn flush(&mut self) {
        flush(&mut self.z1);
        flush(&mut self.z2);
    }
}

/// Mean power over consecutive windows of 1/`LEVEL_HZ` s.
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

    #[allow(clippy::cast_precision_loss)]
    fn push(&mut self, square: f64) -> Option<f64> {
        self.sum += square;
        self.count += 1;
        if self.count < self.length {
            return None;
        }
        let power = self.sum / self.length as f64;
        self.sum = 0.;
        self.count = 0;
        Some(power)
    }
}

/// The latest [`Spectrum`] of the current recording, for the overlay.
/// [`Feed::wakes`] holds one wake-up at most, so a busy UI skips the backlog.
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
    use std::path::Path;

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

    /// `seconds` of steady room noise at `dbfs` RMS: hiss and a 120 Hz hum
    /// of equal power.
    #[allow(clippy::cast_possible_truncation)]
    fn room_noise(rate: u32, seconds: f64, dbfs: f64) -> Vec<f32> {
        let rms = 10_f64.powf(dbfs / 20.);
        let mut seed = 1_u32;
        tone(rate, 120., seconds, rms)
            .into_iter()
            .map(|hum| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                // Uniform over ±0.5: an RMS of 1/√12.
                let hiss = f64::from(seed >> 8) / f64::from(1_u32 << 24) - 0.5;
                hum + (hiss * 6_f64.sqrt() * rms) as f32
            })
            .collect()
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(a, b)| a + b).collect()
    }

    /// The readings of `pcm`, pushed in 10 ms buffers as a capture delivers
    /// it.
    fn readings(analyzer: &mut Analyzer, rate: u32, pcm: &[f32]) -> Vec<Spectrum> {
        pcm.chunks(rate as usize / 100)
            .map(|buffer| {
                analyzer.push(buffer, |_| {});
                analyzer.spectrum()
            })
            .collect()
    }

    /// The bands as they were before the analyzer learned noise floors: the
    /// absolute gate alone, nothing taken off.
    #[allow(clippy::cast_possible_truncation)]
    fn with_absolute_gate(analyzer: &Analyzer) -> [f32; BANDS] {
        let gate = ((decibels(analyzer.level_power) - GATE) / GATE_KNEE).clamp(0., 1.);
        std::array::from_fn(|band| {
            let height =
                (decibels(analyzer.power[band]) + analyzer.tilt[band] - BAND_FLOOR) / BAND_SPAN;
            (height.clamp(0., 1.) * gate) as f32
        })
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
        assert!(analyzer.spectrum().is_quiet());
        // A hum and some hiss well below speech.
        analyzer.push(&room_noise(RATE, 0.5, -50.), |_| {});
        let room = analyzer.spectrum();
        assert!(room.is_quiet(), "{room:?}");
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
    fn steady_noise_draws_nothing_once_heard() {
        for rate in [16_000, RATE] {
            for dbfs in [-40., -35.] {
                let mut analyzer = Analyzer::new(rate);
                let heard = readings(&mut analyzer, rate, &room_noise(rate, 10., dbfs));
                // Louder than the absolute gate, it shows at first...
                assert!(!heard[10].is_quiet(), "{rate} Hz, {dbfs} dBFS");
                // ...and draws nothing once half a second of it was heard.
                for (i, spectrum) in heard.iter().enumerate().skip(50) {
                    assert!(
                        spectrum.is_quiet(),
                        "{rate} Hz, {dbfs} dBFS, after {i}0 ms: {spectrum:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_voice_over_steady_noise_shows_at_once_and_alone() {
        for rate in [16_000, RATE] {
            // A voice at a typical level, -20 dBFS, for 100 ms.
            let voice = tone(rate, centre(12), 0.1, 0.14);
            let quiet = readings(&mut Analyzer::new(rate), rate, &voice);
            for dbfs in [-40., -35.] {
                let room = room_noise(rate, 2.1, dbfs);
                let (heard, rest) = room.split_at(2 * rate as usize);
                let mut analyzer = Analyzer::new(rate);
                analyzer.push(heard, |_| {});
                let noisy = readings(&mut analyzer, rate, &mix(&voice, rest));
                let context = |i: usize| {
                    format!(
                        "{rate} Hz, {dbfs} dBFS: {:?} against {:?}",
                        noisy[i], quiet[i]
                    )
                };
                // In the first buffer...
                assert!(noisy[0].bands[12] > 0.3, "{}", context(0));
                // ...and once it has settled as on a quiet mic, with neither
                // the hum nor the hiss standing up beside it.
                let last = noisy.len() - 1;
                assert!(
                    noisy[last].bands[12] > 0.8 * quiet[last].bands[12],
                    "{}",
                    context(last)
                );
                for (noisy, quiet) in noisy[last].bands.iter().zip(quiet[last].bands) {
                    assert!(*noisy <= quiet + 0.05, "{}", context(last));
                }
            }
        }
    }

    #[test]
    fn the_floor_falls_as_soon_as_the_noise_stops() {
        // A soft voice, -36 dBFS: a quiet mic shows it, a noisy one hides it.
        let soft = tone(RATE, centre(9), 0.01, 0.022);
        let noise = room_noise(RATE, 3.01, -35.);
        let (heard, rest) = noise.split_at(3 * RATE as usize);
        let mut analyzer = Analyzer::new(RATE);
        analyzer.push(heard, |_| {});
        analyzer.push(&mix(&soft, rest), |_| {});
        assert!(analyzer.spectrum().is_quiet());

        // The noise stops, leaving a quiet room.
        analyzer.push(&room_noise(RATE, 0.2, -65.), |_| {});
        analyzer.push(&soft, |_| {});
        let spectrum = analyzer.spectrum();
        assert_eq!(loudest(&spectrum), 9, "{spectrum:?}");
        assert!(spectrum.bands[9] > 0.3, "{spectrum:?}");
    }

    #[test]
    fn a_noise_that_starts_later_fades_within_seconds() {
        let mut analyzer = Analyzer::new(RATE);
        analyzer.push(&room_noise(RATE, 5., -65.), |_| {});
        let heard = readings(&mut analyzer, RATE, &room_noise(RATE, 6., -35.));
        // A new sound shows, as it may be a voice...
        assert!(!heard[..100].iter().any(Spectrum::is_quiet));
        // ...but a few seconds of it are the room.
        assert!(heard[400..].iter().all(Spectrum::is_quiet));
    }

    #[test]
    fn speech_never_lifts_the_floor() {
        let rate = crate::SAMPLE_RATE;
        for clip in ["quick-brown-fox.wav", "dictation-sentences.wav"] {
            let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
            let speech = crate::audio::read_wav(&fixtures.join(clip)).unwrap();
            let mut analyzer = Analyzer::new(rate);
            let (mut peaks, mut before) = ([0_f32; BANDS], [0_f32; BANDS]);
            for buffer in speech.chunks(rate as usize / 100) {
                analyzer.push(buffer, |_| {});
                for (((peak, old), now), then) in peaks
                    .iter_mut()
                    .zip(&mut before)
                    .zip(analyzer.spectrum().bands)
                    .zip(with_absolute_gate(&analyzer))
                {
                    *peak = peak.max(now);
                    *old = old.max(then);
                }
            }
            for (band, (peak, old)) in peaks.iter().zip(&before).enumerate() {
                assert!(
                    *peak >= 0.9 * old,
                    "{clip}, band {band}: {peaks:?} against {before:?}"
                );
            }
        }
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
