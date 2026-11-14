//! The pill's edge as a ring of water: a damped wave equation kicked by the voice's onsets.

use std::collections::hash_map::RandomState;
use std::f32::consts::TAU;
use std::hash::BuildHasher;
use std::sync::LazyLock;

use earwig_core::spectrum::BANDS;

/// Nodes per long run, short side and corner, each spread evenly over its drawn length.
pub const RUN: usize = 96;
pub const SIDE: usize = 4;
pub const CORNER: usize = 12;
/// Top run and right side with their corners; the other half repeats it.
const HALF: usize = RUN + SIDE + 2 * CORNER;
pub const NODES: usize = 2 * HALF;

/// Fixed simulation step, whatever the frame rate.
const STEP: f32 = 1. / 240.;
/// Longest frame we simulate, so a stall doesn't spend ages catching up.
const MAX_ADVANCE: f32 = 0.1;

/// Wave speed in nodes/s (a node is about 5 px of the long runs).
const SPEED: f32 = 40.;
/// Pull back to rest (1/s²), so long swells settle instead of lingering.
const SPRING: f32 = 30.;
/// Velocity lost per second...
const DAMPING: f32 = 5.;
/// ...and more for short ripples (nodes²/s), which keeps crests round.
const VISCOSITY: f32 = 3.;
/// A typical swing rate (1/s): velocity over it is about how far a node will swing.
const SWING: f32 = 8.;

/// Velocity a full band rise kicks in, taken from beside it (see [`high_pass`]) so none grows.
const KICK: f32 = 70.;
/// Standard deviation, in nodes, of where a kick's water comes from.
const WIDE: f32 = 10.;
/// Whole-ring swell at full loudness, and its attack and release in seconds.
const SWELL: f32 = 0.25;
const SWELL_ATTACK: f32 = 0.05;
const SWELL_RELEASE: f32 = 0.35;
/// Band 0 kicks the middle of a run, the top band this far toward the corners (1)...
const BAND_END: f32 = 0.8;
/// ...each as a Gaussian this wide...
const SPREAD: f32 = 0.1;
/// ...under Siri's attenuation `(K / (K + x⁴))^K` with `x = FOCUS * across`...
const FOCUS: f32 = 1.4;
const FOCUS_K: f32 = 2.;
/// ...fading out toward the corners.
const TAPER: (f32, f32) = (0.7, 1.);
/// Below this (about a sixth of a pixel) an unpushed ring snaps to rest.
const REST: f32 = 0.01;
/// Kick gain wanders within 1 ± this around the ring, so no two syllables ripple alike.
const VARIETY: f32 = 0.6;
/// (times around the ring, drift in rad/s) of each of the gain's waves.
const VARIETY_WAVES: [(f32, f32); 4] = [(1., 1.5), (2., 2.3), (3., 3.4), (5., 4.7)];

/// Heights where 1 is a full wave, and their velocities.
#[derive(Clone)]
pub struct Ring {
    height: [f32; NODES],
    velocity: [f32; NODES],
    /// Whole-ring offset on top of the ripples.
    swell: f32,
    /// Seconds not yet simulated.
    pending: f32,
    variety: Variety,
}

impl Default for Ring {
    fn default() -> Self {
        Self::with(Variety::random())
    }
}

impl Ring {
    fn with(variety: Variety) -> Self {
        Self {
            height: [0.; NODES],
            velocity: [0.; NODES],
            swell: 0.,
            pending: 0.,
            variety,
        }
    }

    /// Deterministic kick variety, for tests.
    #[cfg(test)]
    pub fn seeded(seed: u64) -> Self {
        Self::with(Variety::seeded(seed))
    }

    /// `levels` are the bands now (0..1), `rises` how far each rose since the last frame.
    #[allow(clippy::cast_precision_loss)]
    pub fn advance(&mut self, dt: f32, levels: &[f32; BANDS], rises: &[f32; BANDS]) {
        if rises.iter().any(|&rise| rise > 0.) {
            let mut field = spread(rises);
            for (i, kick) in field.iter_mut().enumerate() {
                *kick *= self.variety.gain(i);
            }
            for (velocity, kick) in self.velocity.iter_mut().zip(high_pass(&field)) {
                *velocity += KICK * kick;
            }
        }
        self.variety.drift(dt);
        let loud = SWELL * levels.iter().sum::<f32>() / BANDS as f32;
        let response = if loud > self.swell {
            SWELL_ATTACK
        } else {
            SWELL_RELEASE
        };
        self.swell += (loud - self.swell) * (1. - (-dt / response).exp());

        self.pending = (self.pending + dt).min(MAX_ADVANCE);
        while self.pending >= STEP {
            self.pending -= STEP;
            self.step();
        }

        let pushed = levels.iter().chain(rises).any(|&band| band > 0.);
        let calm = |values: &[f32; NODES], below: f32| values.iter().all(|x| x.abs() < below);
        if !pushed
            && self.swell < REST
            && calm(&self.height, REST)
            && calm(&self.velocity, REST * SWING)
        {
            *self = Ring::with(self.variety);
        }
    }

    /// One semi-implicit Euler step of the damped wave equation.
    fn step(&mut self) {
        let (height, velocity) = (self.height, self.velocity);
        let around = |values: &[f32; NODES], i: usize| {
            values[(i + NODES - 1) % NODES] + values[(i + 1) % NODES] - 2. * values[i]
        };
        for (i, v) in self.velocity.iter_mut().enumerate() {
            let pull = SPEED * SPEED * around(&height, i) + VISCOSITY * around(&velocity, i)
                - DAMPING * velocity[i]
                - SPRING * height[i];
            *v += pull * STEP;
        }
        for (h, v) in self.height.iter_mut().zip(&self.velocity) {
            *h += v * STEP;
        }
    }

    pub fn surface(&self) -> [f32; NODES] {
        self.height.map(|h| h + self.swell)
    }

    /// Whether the ring is exactly at rest.
    pub fn rests(&self) -> bool {
        self.swell == 0. && self.height.iter().chain(&self.velocity).all(|&x| x == 0.)
    }

    /// Furthest any node stands out or is swinging to, so passing through rest isn't calm.
    pub fn amplitude(&self) -> f32 {
        let furthest = |values: &[f32; NODES]| values.iter().fold(0_f32, |m, x| m.max(x.abs()));
        self.swell + furthest(&self.height) + furthest(&self.velocity) / SWING
    }
}

/// Where kicks land stronger and weaker around the ring.
#[derive(Clone, Copy)]
struct Variety {
    /// Phase and signed drift of each of [`VARIETY_WAVES`].
    waves: [(f32, f32); VARIETY_WAVES.len()],
}

impl Variety {
    fn random() -> Self {
        Self::seeded(RandomState::new().hash_one(()))
    }

    #[allow(clippy::cast_precision_loss)]
    fn seeded(mut seed: u64) -> Self {
        // SplitMix64.
        let mut next = || {
            seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let z = (seed ^ (seed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            let z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        Self {
            waves: VARIETY_WAVES.map(|(_, speed)| {
                let draw = next();
                let phase = TAU * (draw >> 40) as f32 / (1_u64 << 24) as f32;
                (phase, if draw & 1 == 0 { speed } else { -speed })
            }),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn gain(&self, i: usize) -> f32 {
        let at = TAU * i as f32 / NODES as f32;
        let waves: f32 = VARIETY_WAVES
            .iter()
            .zip(&self.waves)
            .map(|((cycles, _), (phase, _))| (cycles * at + phase).sin())
            .sum();
        1. + VARIETY * waves / VARIETY_WAVES.len() as f32
    }

    fn drift(&mut self, dt: f32) {
        for (phase, speed) in &mut self.waves {
            *phase = (*phase + *speed * dt).rem_euclid(TAU);
        }
    }
}

/// Node `i`'s distance from the middle of its long run (0) to the corners (1), if on one.
#[allow(clippy::cast_precision_loss)]
fn across(i: usize) -> Option<f32> {
    let along = i % HALF;
    (along <= RUN).then(|| (2. * along as f32 / RUN as f32 - 1.).abs())
}

/// Per long-run node, each band's kick weight: mirrored Gaussians, normalised, focused, tapered.
static KICKS: LazyLock<Vec<(usize, [f32; BANDS])>> = LazyLock::new(|| {
    (0..NODES)
        .filter_map(|i| {
            let x = across(i)?;
            #[allow(clippy::cast_precision_loss)]
            let near: [f32; BANDS] = std::array::from_fn(|band| {
                let at = BAND_END * band as f32 / (BANDS - 1) as f32;
                let bump = |d: f32| (-0.5 * (d / SPREAD).powi(2)).exp();
                bump(x - at) + bump(x + at)
            });
            let total: f32 = near.iter().sum();
            let focus = (FOCUS_K / (FOCUS_K + (FOCUS * x).powi(4))).powf(FOCUS_K);
            let fade = 1. - smoothstep(TAPER.0, TAPER.1, x);
            Some((i, near.map(|w| focus * fade * w / total)))
        })
        .collect()
});

fn spread(rises: &[f32; BANDS]) -> [f32; NODES] {
    let mut nodes = [0.; NODES];
    for (i, weights) in KICKS.iter() {
        nodes[*i] = weights.iter().zip(rises).map(|(w, rise)| w * rise).sum();
    }
    nodes
}

/// `field` less its Gaussian-blurred local average, so a kick lifts water from beside it.
fn high_pass(field: &[f32; NODES]) -> [f32; NODES] {
    static BLUR: LazyLock<Vec<f32>> = LazyLock::new(|| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let reach = (3. * WIDE).ceil() as usize;
        #[allow(clippy::cast_precision_loss)]
        let taps: Vec<f32> = (0..=2 * reach)
            .map(|k| (-0.5 * ((k as f32 - reach as f32) / WIDE).powi(2)).exp())
            .collect();
        let total: f32 = taps.iter().sum();
        taps.into_iter().map(|t| t / total).collect()
    });
    let reach = BLUR.len() / 2;
    std::array::from_fn(|i| {
        let local: f32 = BLUR
            .iter()
            .enumerate()
            .map(|(k, w)| w * field[(i + NODES + k - reach) % NODES])
            .sum();
        field[i] - local
    })
}

/// 0 up to `from`, 1 from `to`, easing in and out between.
fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: [f32; BANDS] = [0.; BANDS];

    fn energy(ring: &Ring) -> f32 {
        let h = &ring.height;
        (0..NODES)
            .map(|i| {
                let slope = h[(i + 1) % NODES] - h[i];
                ring.velocity[i].powi(2) + (SPEED * slope).powi(2) + SPRING * h[i].powi(2)
            })
            .sum()
    }

    fn syllable() -> [f32; BANDS] {
        std::array::from_fn(|band| if (3..9).contains(&band) { 0.8 } else { 0.2 })
    }

    #[test]
    fn a_syllable_rings_on_then_comes_exactly_to_rest() {
        let mut ring = Ring::with(Variety::seeded(1));
        let voice = syllable();
        ring.advance(1. / 60., &voice, &voice);
        for _ in 0..12 {
            ring.advance(1. / 60., &voice, &QUIET);
        }
        let mut energies = Vec::new();
        let mut rested_at = None;
        for frame in 0..180 {
            ring.advance(1. / 60., &QUIET, &QUIET);
            energies.push(energy(&ring));
            if rested_at.is_none() && ring.rests() {
                rested_at = Some(frame);
            }
        }
        let peak = |from: usize, to: usize| energies[from..to].iter().copied().fold(0., f32::max);
        assert!(peak(18, 30) > 0., "still moving 0.3 s after: {energies:?}");
        for second in 0..5 {
            let (from, to) = (second * 12, (second + 1) * 12);
            assert!(
                peak(to, to + 12) <= peak(from, to),
                "energy only falls: {energies:?}"
            );
        }
        let rested_at = rested_at.expect("comes to rest");
        assert!(
            (40..=120).contains(&rested_at),
            "rests after {rested_at} frames"
        );
        assert!(ring.rests());
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn no_two_dictations_ripple_alike_yet_kicks_stay_centred() {
        let voice: [f32; BANDS] = std::array::from_fn(|band| 1. - band as f32 / BANDS as f32);
        let surface = |seed| {
            let mut ring = Ring::with(Variety::seeded(seed));
            ring.advance(1. / 60., &voice, &voice);
            for _ in 0..3 {
                ring.advance(1. / 60., &voice, &QUIET);
            }
            ring.surface()
        };
        let (a, b) = (surface(1), surface(2));
        let peak = a.iter().fold(0_f32, |m, h| m.max(h.abs()));
        let apart = a
            .iter()
            .zip(&b)
            .fold(0_f32, |m, (x, y)| m.max((x - y).abs()));
        assert!(apart > 0.1 * peak, "{apart} apart, peak {peak}");
        for h in [a, b] {
            for run in [0, HALF] {
                // Low bands, loudest here, kick each run's middle most; corners haven't felt it.
                let middle = h[run + RUN / 2];
                assert!(
                    middle > h[run + RUN / 4] && middle > 2. * h[run + RUN + CORNER],
                    "{h:?}"
                );
            }
        }
    }

    /// Talking: syllables at 4 Hz, as heights and rises per frame.
    #[allow(clippy::cast_precision_loss)]
    fn talk(t: f32) -> [f32; BANDS] {
        let syllable = (std::f32::consts::TAU * 4. * t).sin().max(0.);
        std::array::from_fn(|band| syllable * (0.5 + 0.5 * (band as f32 * 0.4 + t).sin()))
    }

    /// Furthest height per 0.1 s over 3 s of talk and 2 of silence, at `frame(n)`-second frames.
    fn run(frame: impl Fn(usize) -> f32) -> Vec<f32> {
        let mut ring = Ring::with(Variety::seeded(7));
        let (mut t, mut last, mut n) = (0., QUIET, 0);
        let mut furthest = vec![0_f32; 50];
        while t < 5. {
            let dt = frame(n);
            t += dt;
            n += 1;
            let now = if t < 3. { talk(t) } else { QUIET };
            let rises = std::array::from_fn(|b| (now[b] - last[b]).max(0.));
            ring.advance(dt, &now, &rises);
            last = now;
            assert!(
                ring.height
                    .iter()
                    .chain(&ring.velocity)
                    .all(|x| x.is_finite())
            );
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let slot = ((t * 10.) as usize).min(49);
            let high = ring.height.iter().fold(0_f32, |m, h| m.max(h.abs()));
            furthest[slot] = furthest[slot].max(high);
        }
        assert!(ring.rests(), "rests two seconds after the talking");
        furthest
    }

    #[test]
    fn behaves_the_same_at_any_frame_rate() {
        let reference = run(|_| 1. / 240.);
        let peak = reference.iter().copied().fold(0., f32::max);
        assert!(peak > 0.3 && peak < 2., "{reference:?}");
        let rates: [&dyn Fn(usize) -> f32; 5] = [
            &|_| 1. / 24.,
            &|_| 1. / 60.,
            &|_| 1. / 144.,
            &|_| 1. / 1000.,
            &|n| [0.004, 0.03, 0.011, 0.05, 0.0][n % 5],
        ];
        for (k, rate) in rates.iter().enumerate() {
            let furthest = run(rate);
            for (slot, (a, b)) in furthest.iter().zip(&reference).enumerate().take(30).skip(3) {
                assert!(
                    (a - b).abs() < 0.35 * peak,
                    "rate {k}, {slot}: {furthest:?} vs {reference:?}"
                );
            }
        }
    }
}
