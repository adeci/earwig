//! The overlay's pill: a rounded rectangle whose edge swells and ripples with
//! the voice, painted as a path over a soft drop shadow.

use std::f32::consts::TAU;
use std::time::Instant;

use earwig_core::config::OverlayConfig;
use gpui_kit::component::Theme;
use gpui_kit::{
    App, Bounds, BoxShadow, Corners, Hsla, PathBuilder, Pixels, Point, Rgba, Window, point, px,
};

/// Corner radius of the pill at rest.
const RADIUS: f32 = 24.;
/// Transparent room the pill needs around it: its shadow fades out (below a
/// visible step, 2.5 blur radii) before the surface ends, even at full reach.
pub const ROOM: f32 = SHADOW_DROP + 2.5 * SHADOW_BLUR + MAX_REACH;

/// Furthest the edge moves out from rest: swell plus a full ripple at the
/// wildest motion setting.
pub const MAX_REACH: f32 = OverlayConfig::MAX_MOTION * (SWELL + RIPPLE);
/// The ambient shadow's offset below the pill and its blur (one standard
/// deviation of the Gaussian GPUI draws).
const SHADOW_DROP: f32 = 10.;
const SHADOW_BLUR: f32 = 12.;
const BORDER: f32 = 1.5;

/// How far the whole pill grows at full voice and motion 1.
const SWELL: f32 = 3.;
/// Amplitude of the travelling ripple at full voice and motion 1.
const RIPPLE: f32 = 4.2;

/// Seconds the voice energy takes to rise toward a louder level...
const ATTACK: f32 = 0.05;
/// ...and to fall back, so the edge flows between syllables.
const RELEASE: f32 = 0.3;
/// Seconds colours take to settle (theme or accent changes).
const FADE: f32 = 0.18;
/// A stalled frame resumes calmly instead of jumping.
const MAX_STEP: f32 = 0.05;

/// Mic levels are dB mapped onto 0..1 over -60..0 dBFS; room noise sits
/// below `VOICE_FLOOR` and ordinary speech reaches the top of the span.
const VOICE_FLOOR: f32 = 0.35;
const VOICE_SPAN: f32 = 0.4;

/// Distance between outline samples; fine enough that corners stay round.
const SAMPLE_SPACING: f32 = 4.;

/// One travelling sine around the perimeter. Whole cycles keep the outline
/// closed; mixed speeds and directions keep it from looking mechanical.
struct Wave {
    cycles: f32,
    weight: f32,
    /// Radians per second at full voice.
    speed: f32,
}

const WAVES: [Wave; 3] = [
    Wave {
        cycles: 3.,
        weight: 0.3,
        speed: 2.,
    },
    Wave {
        cycles: 7.,
        weight: 0.4,
        speed: -3.4,
    },
    Wave {
        cycles: 12.,
        weight: 0.3,
        speed: 5.,
    },
];

/// Seconds between rendered frames.
#[derive(Default)]
pub struct Clock(Option<Instant>);

impl Clock {
    pub fn tick(&mut self, now: Instant) -> f32 {
        let dt = self
            .0
            .map_or(0., |last| now.duration_since(last).as_secs_f32());
        self.0 = Some(now);
        dt.min(MAX_STEP)
    }
}

/// Smoothed voice energy and the phases the edge animates with.
pub struct Motion {
    energy: f32,
    phases: [f32; WAVES.len()],
    /// The motion setting: how far the edge travels for a given voice.
    intensity: f32,
}

impl Motion {
    pub fn new(intensity: f32) -> Self {
        Self {
            energy: 0.,
            phases: [0.; WAVES.len()],
            intensity,
        }
    }

    pub fn set_intensity(&mut self, intensity: f32) {
        self.intensity = intensity;
    }

    /// Advance by `dt` seconds. `level` is the latest mic level while
    /// recording, `None` once recording stopped, which lets the edge settle.
    pub fn step(&mut self, dt: f32, level: Option<f32>) -> Edge {
        let target = level.map_or(0., voice);
        let response = if target > self.energy {
            ATTACK
        } else {
            RELEASE
        };
        self.energy += (target - self.energy) * approach(dt, response);
        for (phase, wave) in self.phases.iter_mut().zip(&WAVES) {
            *phase = (*phase + wave.speed * self.energy * dt).rem_euclid(TAU);
        }

        let travel = self.energy * self.intensity;
        Edge {
            swell: SWELL * travel,
            ripple: RIPPLE * travel,
            phases: self.phases,
        }
    }
}

/// The fraction of the way to a target covered in `dt` seconds by a value
/// that closes on it with time constant `response`.
fn approach(dt: f32, response: f32) -> f32 {
    1. - (-dt / response).exp()
}

/// How much of the voice range a mic level reaches, 0 in silence.
fn voice(level: f32) -> f32 {
    ((level - VOICE_FLOOR) / VOICE_SPAN).clamp(0., 1.)
}

/// The pill's outline at one instant.
#[derive(Clone, Copy, Debug, Default)]
pub struct Edge {
    swell: f32,
    ripple: f32,
    phases: [f32; WAVES.len()],
}

impl Edge {
    /// Outward displacement in pixels at `u`, the fraction of the way
    /// around the perimeter.
    fn offset(&self, u: f32) -> f32 {
        let ripple: f32 = WAVES
            .iter()
            .zip(self.phases)
            .map(|(wave, phase)| wave.weight * (TAU * wave.cycles * u + phase).sin())
            .sum();
        self.swell + self.ripple * ripple
    }

    /// The most the edge can stand out from rest right now, in pixels.
    pub fn reach(&self) -> f32 {
        self.swell + self.ripple
    }
}

/// Colours of the pill.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub fill: Hsla,
    pub border: Hsla,
    pub dark: bool,
    /// Shadow strength, 1 at the default opacity.
    pub depth: f32,
}

impl Look {
    /// The overlay's colours at fill `opacity`: Tokyo Night charcoal in dark
    /// mode, the theme's background in light mode, and an accent hairline.
    /// The shadow fades with the fill; the hairline stays put.
    pub fn themed(theme: &Theme, accent: Hsla, opacity: f32) -> Look {
        let dark = theme.is_dark();
        let fill: Hsla = if dark {
            // Tokyo Night's background: a blue-black charcoal.
            gpui_kit::rgb(0x001a_1b26).into()
        } else {
            theme.background
        };
        Look {
            fill: fill.opacity(opacity),
            border: accent.opacity(0.9),
            dark,
            depth: opacity / OverlayConfig::DEFAULT_OPACITY,
        }
    }

    /// `self` moved toward `target` over `dt` seconds.
    pub fn fade_to(self, target: Look, dt: f32) -> Look {
        let t = approach(dt, FADE);
        Look {
            fill: mix(self.fill, target.fill, t),
            border: mix(self.border, target.border, t),
            dark: target.dark,
            depth: self.depth + (target.depth - self.depth) * t,
        }
    }
}

/// Whether the edge should hold still: the desktop asks for reduced motion,
/// or there is no GPU and animating would take the CPU the decoder needs.
pub fn holds_still(window: &Window, cx: &App) -> bool {
    cx.reduce_motion() || software_rendered(window)
}

/// Whether GPUI draws on the CPU.
pub fn software_rendered(window: &Window) -> bool {
    window
        .gpu_specs()
        .is_some_and(|gpu| gpu.is_software_emulated)
}

/// Paint the shadow, body and hairline of a pill resting at `bounds`, with
/// its corners, motion and shadow drawn at `scale` (the hairline stays as
/// is, so a small preview keeps a crisp border).
pub fn paint(bounds: Bounds<Pixels>, edge: Edge, look: Look, scale: f32, window: &mut Window) {
    paint_shadow(bounds, edge, look, scale, window);
    let outline = outline(bounds, RADIUS * scale)
        .map(|(at, normal, u)| {
            let offset = edge.offset(u) * scale;
            point(px(at.x + normal.x * offset), px(at.y + normal.y * offset))
        })
        .collect::<Vec<_>>();
    let mut body = PathBuilder::fill();
    body.add_polygon(&outline, true);
    if let Ok(path) = body.build() {
        window.paint_path(path, look.fill);
    }
    let mut line = PathBuilder::stroke(px(BORDER));
    line.add_polygon(&outline, true);
    if let Ok(path) = line.build() {
        window.paint_path(path, look.border);
    }
}

/// A wide ambient shadow and a tight contact one. Shadows are drawn only
/// outside the element they belong to; tucking that element just inside the
/// smallest the edge gets right now (and spreading by as much) keeps the
/// troughs of a ripple shaded instead of revealing a hard rim, while a
/// translucent fill shows no shadow band beneath it.
fn paint_shadow(bounds: Bounds<Pixels>, edge: Edge, look: Look, scale: f32, window: &mut Window) {
    let tuck = px(((edge.ripple - edge.swell).max(0.) + 1.) * scale);
    let (ambient, contact) = if look.dark { (0.5, 0.3) } else { (0.16, 0.1) };
    let shadow = |alpha: f32, y: f32, blur: f32| BoxShadow {
        color: Hsla::black().opacity(alpha * look.depth),
        offset: point(px(0.), px(y * scale)),
        blur_radius: px(blur * scale),
        spread_radius: tuck,
        inset: false,
    };
    let swell = edge.swell * scale;
    let rest = bounds.dilate(px(swell));
    window.paint_drop_shadows(
        rest.dilate(-tuck),
        Corners::all(px(RADIUS * scale + swell)),
        &[
            shadow(ambient, SHADOW_DROP, SHADOW_BLUR),
            shadow(contact, 1., 3.),
        ],
    );
}

/// Evenly spaced points around the rounded rectangle, clockwise from the
/// start of the top edge, with their outward normal and how far around the
/// perimeter (0..1) each one is.
fn outline(
    bounds: Bounds<Pixels>,
    radius: f32,
) -> impl Iterator<Item = (Point<f32>, Point<f32>, f32)> {
    let (left, top) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let (right, bottom) = (f32::from(bounds.right()), f32::from(bounds.bottom()));
    let r = radius.min((right - left) / 2.).min((bottom - top) / 2.);
    let straight_x = right - left - 2. * r;
    let straight_y = bottom - top - 2. * r;
    let arc = TAU / 4. * r;
    let perimeter = 2. * (straight_x + straight_y) + 4. * arc;

    // Each side is a straight run followed by the corner that turns into the
    // next side; `angle` is the direction of the side's outward normal.
    let sides = [
        (point(left + r, top), straight_x, -TAU / 4.),
        (point(right, top + r), straight_y, 0.),
        (point(right - r, bottom), straight_x, TAU / 4.),
        (point(left, bottom - r), straight_y, TAU / 2.),
    ];
    let mut travelled = 0.;
    sides.into_iter().flat_map(move |(start, length, angle)| {
        let normal = point(angle.cos(), angle.sin());
        let along = point(-normal.y, normal.x);
        let corner = point(start.x + along.x * length, start.y + along.y * length);
        let centre = point(corner.x - normal.x * r, corner.y - normal.y * r);
        let from = travelled;
        travelled += length + arc;
        let count = samples(length + arc);
        (0..count).map(move |i| {
            #[allow(clippy::cast_precision_loss)]
            let s = (length + arc) * i as f32 / count as f32;
            let u = (from + s) / perimeter;
            if s < length {
                (
                    point(start.x + along.x * s, start.y + along.y * s),
                    normal,
                    u,
                )
            } else {
                let turned = angle + (s - length) / r.max(f32::EPSILON);
                let normal = point(turned.cos(), turned.sin());
                (
                    point(centre.x + normal.x * r, centre.y + normal.y * r),
                    normal,
                    u,
                )
            }
        })
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn samples(length: f32) -> usize {
    ((length / SAMPLE_SPACING).ceil() as usize).max(1)
}

/// `a` moved toward `b` by `t` (0..1), in sRGB so hues do not swing.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (Rgba::from(a), Rgba::from(b));
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use gpui_kit::size;

    use super::*;

    fn bounds() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(10.), px(20.)),
            size: size(px(200.), px(80.)),
        }
    }

    #[test]
    fn outline_hugs_the_rounded_rectangle() {
        let points: Vec<_> = outline(bounds(), RADIUS).collect();
        let (first, _, u0) = points[0];
        assert_eq!((first.x, first.y, u0), (10. + RADIUS, 20., 0.));
        for (at, normal, u) in &points {
            assert!((0. ..1.).contains(u));
            assert!((normal.x.hypot(normal.y) - 1.).abs() < 1e-4);
            assert!((9.99..=210.01).contains(&at.x) && (19.99..=100.01).contains(&at.y));
        }
        assert!(points.windows(2).all(|w| w[0].2 < w[1].2));
    }

    fn furthest(edge: &Edge) -> f32 {
        (0..100u8)
            .map(|i| edge.offset(f32::from(i) / 100.))
            .fold(0., f32::max)
    }

    #[test]
    fn silence_is_still_and_speech_moves_the_edge() {
        let frame = 1. / 60.;
        let mut motion = Motion::new(1.);
        let mut quiet = Edge::default();
        for _ in 0..120 {
            quiet = motion.step(frame, Some(0.2));
        }
        assert!((0..100u8).all(|i| quiet.offset(f32::from(i) / 100.).abs() < 1e-3));

        let mut loud = quiet;
        for _ in 0..30 {
            loud = motion.step(frame, Some(0.8));
        }
        let peak = motion.energy;
        assert!(peak > 0.95, "attack is fast: {peak}");
        let reach = furthest(&loud);
        assert!(reach > SWELL && reach <= loud.reach());

        for _ in 0..6 {
            motion.step(frame, None);
        }
        assert!(
            motion.energy > 0.5 && motion.energy < peak,
            "release is gradual: {}",
            motion.energy
        );
    }

    #[test]
    fn motion_setting_scales_how_far_the_voice_moves_the_edge() {
        let reaches: Vec<f32> = [0., 0.5, 1., 1.5, OverlayConfig::MAX_MOTION]
            .into_iter()
            .map(|intensity| {
                let mut motion = Motion::new(intensity);
                let mut edge = Edge::default();
                for _ in 0..30 {
                    edge = motion.step(1. / 60., Some(0.8));
                }
                furthest(&edge)
            })
            .collect();
        assert!(reaches[0].abs() < 1e-6, "still at full voice: {reaches:?}");
        assert!(reaches.windows(2).all(|w| w[0] < w[1]), "{reaches:?}");
        assert!(reaches[4] <= MAX_REACH, "fits the room: {reaches:?}");
    }

    #[test]
    fn mix_ends_at_both_colours() {
        let (a, b) = (Hsla::black(), Hsla::white());
        assert_eq!(Rgba::from(mix(a, b, 0.)), Rgba::from(a));
        assert_eq!(Rgba::from(mix(a, b, 1.)), Rgba::from(b));
    }
}
