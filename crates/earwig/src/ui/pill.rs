//! The overlay's pill: a rounded rectangle over a soft drop shadow that
//! shows the voice in one of two styles. Bars: thin accent bars above and
//! below a still pill, mirrored from the centre outward, low bands in the
//! middle. Wave: the edge itself swells and ripples, painted as a path.

use std::f32::consts::TAU;
use std::time::Instant;

use earwig_core::config::{OverlayConfig, OverlayStyle};
use earwig_core::spectrum::{BANDS, Spectrum};
use gpui_kit::component::Theme;
use gpui_kit::{
    App, Bounds, BoxShadow, Corners, Edges, Hsla, PathBuilder, Pixels, Point, Rgba, Window, fill,
    point, px, size,
};

/// Corner radius of the pill at rest.
const RADIUS: f32 = 24.;
/// Transparent room the pill needs around it: its shadow fades out (below a
/// visible step, 2.5 blur radii) before the surface ends, even when the wave
/// carries it out to full reach. Bars leave the shadow where it is and fit
/// well within that.
pub const ROOM: f32 = SHADOW_DROP + 2.5 * SHADOW_BLUR + WAVE_REACH;
const _: () = assert!(BARS_REACH < ROOM);

/// Furthest anything moves out from the pill at the widest motion setting.
pub const MAX_REACH: f32 = larger(WAVE_REACH, BARS_REACH);
/// A full swell and ripple...
const WAVE_REACH: f32 = OverlayConfig::MAX_MOTION * (SWELL + RIPPLE);
/// ...or full bars beyond their gap.
const BARS_REACH: f32 = BAR_GAP + OverlayConfig::MAX_MOTION * BAR_LENGTH;
/// The ambient shadow's offset below the pill and its blur (one standard
/// deviation of the Gaussian GPUI draws).
const SHADOW_DROP: f32 = 10.;
const SHADOW_BLUR: f32 = 12.;

/// How far the whole pill grows at full voice and motion 1.
const SWELL: f32 = 3.;
/// Amplitude of the travelling ripple at full voice and motion 1.
const RIPPLE: f32 = 4.2;

/// Seconds the wave's voice energy takes to rise toward a louder level...
const ATTACK: f32 = 0.015;
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

/// Space between the pill's edge and the foot of its bars.
const BAR_GAP: f32 = 3.;
/// A bar's length at the top of its band and motion 1.
const BAR_LENGTH: f32 = 12.;
const BAR_WIDTH: f32 = 4.;
/// Closest bars may stand; a pill too narrow for one bar per band on each
/// side of the centre shows fewer, between the bands.
const BAR_PITCH: f32 = 8.;
/// Bars shorter than this many pixels are not drawn, so silence and
/// near-silence show a clean pill rather than dots.
const MIN_BAR: f32 = 1.5;
/// Seconds a full bar takes to fall to nothing once its band falls silent;
/// it gathers speed like a dropped object. Bars rise as fast as the
/// analysis does (a few milliseconds).
const DROP: f32 = 0.13;
const GRAVITY: f32 = 2. / (DROP * DROP);
/// Bar opacity from the shortest to the tallest.
const BAR_ALPHA: (f32, f32) = (0.55, 0.95);

const fn larger(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

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

/// How the pill follows the voice from frame to frame, in either style.
pub struct Motion {
    style: OverlayStyle,
    /// The motion setting: how far the voice moves the pill.
    intensity: f32,
    /// Wave: smoothed voice energy and the phases the edge animates with.
    energy: f32,
    phases: [f32; WAVES.len()],
    /// Bars: each band's height (0..1) and how fast it is falling.
    heights: [f32; BANDS],
    falling: [f32; BANDS],
}

impl Motion {
    pub fn new(style: OverlayStyle, intensity: f32) -> Self {
        Self {
            style,
            intensity,
            energy: 0.,
            phases: [0.; WAVES.len()],
            heights: [0.; BANDS],
            falling: [0.; BANDS],
        }
    }

    pub fn set_style(&mut self, style: OverlayStyle) {
        self.style = style;
    }

    pub fn set_intensity(&mut self, intensity: f32) {
        self.intensity = intensity;
    }

    /// Advance by `dt` seconds. `voice` is the latest reading while
    /// recording, `None` once recording stopped, which lets the pill settle.
    pub fn step(&mut self, dt: f32, voice: Option<&Spectrum>) -> Shape {
        match self.style {
            OverlayStyle::Bars => Shape::Bars(self.step_bars(dt, voice)),
            OverlayStyle::Wave => Shape::Wave(self.step_wave(dt, voice.map(|v| v.level))),
        }
    }

    /// A bar jumps up with its band and falls under gravity, never below
    /// where its band is now.
    fn step_bars(&mut self, dt: f32, voice: Option<&Spectrum>) -> Bars {
        for (band, (height, falling)) in self.heights.iter_mut().zip(&mut self.falling).enumerate()
        {
            let target = voice.map_or(0., |v| v.bands[band]);
            if target >= *height {
                *height = target;
                *falling = 0.;
            } else {
                *falling += GRAVITY * dt;
                *height = (*height - *falling * dt).max(target);
            }
        }
        Bars {
            heights: self.heights,
            length: BAR_LENGTH * self.intensity,
        }
    }

    fn step_wave(&mut self, dt: f32, level: Option<f32>) -> Edge {
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

/// What the voice does to the pill at one instant.
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Wave(Edge),
    Bars(Bars),
}

impl Shape {
    /// The pill of `style` in silence.
    pub fn still(style: OverlayStyle) -> Shape {
        match style {
            OverlayStyle::Bars => Shape::Bars(Bars::default()),
            OverlayStyle::Wave => Shape::Wave(Edge::default()),
        }
    }

    /// How many pixels the voice moves the pill by right now.
    pub fn reach(&self) -> f32 {
        match self {
            Shape::Wave(edge) => edge.reach(),
            Shape::Bars(bars) => bars.tallest(),
        }
    }
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

/// Band heights (0..1) and the length of a full bar in pixels.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bars {
    heights: [f32; BANDS],
    length: f32,
}

impl Bars {
    /// The longest bar in pixels.
    fn tallest(&self) -> f32 {
        self.heights.iter().copied().fold(0., f32::max) * self.length
    }

    /// Height (0..1) at `t`, 0 in the centre of the pill to 1 at the ends,
    /// between the bands on either side.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn height_at(&self, t: f32) -> f32 {
        let at = t.clamp(0., 1.) * (BANDS - 1) as f32;
        let below = (at.floor() as usize).min(BANDS - 2);
        let frac = at - below as f32;
        self.heights[below] * (1. - frac) + self.heights[below + 1] * frac
    }
}

/// Colours and border of the pill.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub fill: Hsla,
    pub accent: Hsla,
    pub dark: bool,
    /// Shadow strength, 1 at the default opacity.
    pub depth: f32,
    /// Border width in pixels; 0 draws none.
    pub border: f32,
}

impl Look {
    /// The overlay's look at fill `opacity`: Tokyo Night charcoal in dark
    /// mode, the theme's background in light mode, accent bars and border.
    /// The shadow fades with the fill; bars and border stay put.
    pub fn themed(theme: &Theme, accent: Hsla, opacity: f32, border: f32) -> Look {
        let dark = theme.is_dark();
        let fill: Hsla = if dark {
            // Tokyo Night's background: a blue-black charcoal.
            gpui_kit::rgb(0x001a_1b26).into()
        } else {
            theme.background
        };
        Look {
            fill: fill.opacity(opacity),
            accent,
            dark,
            depth: opacity / OverlayConfig::DEFAULT_OPACITY,
            border,
        }
    }

    /// `self` moved toward `target` over `dt` seconds.
    pub fn fade_to(self, target: Look, dt: f32) -> Look {
        let t = approach(dt, FADE);
        Look {
            fill: mix(self.fill, target.fill, t),
            accent: mix(self.accent, target.accent, t),
            dark: target.dark,
            depth: self.depth + (target.depth - self.depth) * t,
            border: target.border,
        }
    }

    /// The border's width at `scale`; a scaled-down border stays visible.
    fn border_width(&self, scale: f32) -> Option<Pixels> {
        (self.border > 0.).then(|| px((self.border * scale).max(1.)))
    }
}

/// Whether the pill should hold still: the desktop asks for reduced motion,
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

/// Paint the shadow, body, border and voice of a pill resting at `bounds`,
/// drawn at `scale`.
pub fn paint(bounds: Bounds<Pixels>, shape: &Shape, look: Look, scale: f32, window: &mut Window) {
    match shape {
        Shape::Wave(edge) => paint_wave(bounds, *edge, look, scale, window),
        Shape::Bars(bars) => {
            paint_shadow(bounds, Edge::default(), look, scale, window);
            let mut body = fill(bounds, look.fill).corner_radii(Corners::all(px(RADIUS * scale)));
            if let Some(width) = look.border_width(scale) {
                body = body
                    .border_widths(Edges::all(width))
                    .border_color(look.accent);
            }
            window.paint_quad(body);
            for (bar, height) in bar_layout(bounds, bars, scale) {
                let alpha = BAR_ALPHA.0 + (BAR_ALPHA.1 - BAR_ALPHA.0) * height;
                window.paint_quad(
                    fill(bar, look.accent.opacity(alpha))
                        .corner_radii(Corners::all(bar.size.width / 2.)),
                );
            }
        }
    }
}

fn paint_wave(bounds: Bounds<Pixels>, edge: Edge, look: Look, scale: f32, window: &mut Window) {
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
    if let Some(width) = look.border_width(scale) {
        let mut line = PathBuilder::stroke(width);
        line.add_polygon(&outline, true);
        if let Ok(path) = line.build() {
            window.paint_path(path, look.accent);
        }
    }
}

/// The bars of a pill at `bounds` that are long enough to draw, with their
/// height (0..1): mirrored pairs across the straight top and bottom runs,
/// low bands in the middle, each pair standing out from the edge it sits on.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn bar_layout(
    bounds: Bounds<Pixels>,
    bars: &Bars,
    scale: f32,
) -> impl Iterator<Item = (Bounds<Pixels>, f32)> {
    let (left, top) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let (right, bottom) = (f32::from(bounds.right()), f32::from(bounds.bottom()));
    let radius = (RADIUS * scale)
        .min((right - left) / 2.)
        .min((bottom - top) / 2.);
    let (width, gap) = (BAR_WIDTH * scale, BAR_GAP * scale);
    // Up to one bar per band on each side of the centre, spread evenly over
    // the run between the corners.
    let run = right - left - 2. * radius;
    let half = ((run / (BAR_PITCH * scale) / 2.).floor() as usize).clamp(1, BANDS);
    let pitch = run / (2 * half) as f32;
    let centre = f32::midpoint(left, right);
    let bars = *bars;
    (0..half).flat_map(move |k| {
        let height = bars.height_at(k as f32 / (half - 1).max(1) as f32);
        let length = height * bars.length * scale;
        let from_centre = (k as f32 + 0.5) * pitch;
        [centre - from_centre, centre + from_centre]
            .into_iter()
            .flat_map(move |x| {
                [top - gap - length, bottom + gap].map(|y| {
                    Bounds::new(
                        point(px(x - width / 2.), px(y)),
                        size(px(width), px(length)),
                    )
                })
            })
            .filter(move |_| length >= MIN_BAR)
            .map(move |bar| (bar, height))
    })
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
    use super::*;

    const FRAME: f32 = 1. / 60.;

    fn bounds() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(10.), px(20.)),
            size: size(px(200.), px(80.)),
        }
    }

    fn level(level: f32) -> Spectrum {
        Spectrum {
            level,
            ..Spectrum::default()
        }
    }

    fn wave(shape: Shape) -> Edge {
        match shape {
            Shape::Wave(edge) => edge,
            Shape::Bars(_) => panic!("bars from a wave"),
        }
    }

    fn bars(shape: Shape) -> Bars {
        match shape {
            Shape::Bars(bars) => bars,
            Shape::Wave(_) => panic!("a wave from bars"),
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
        let mut motion = Motion::new(OverlayStyle::Wave, 1.);
        let mut quiet = Edge::default();
        for _ in 0..120 {
            quiet = wave(motion.step(FRAME, Some(&level(0.2))));
        }
        assert!((0..100u8).all(|i| quiet.offset(f32::from(i) / 100.).abs() < 1e-3));

        let mut loud = quiet;
        for _ in 0..4 {
            loud = wave(motion.step(FRAME, Some(&level(0.8))));
        }
        let peak = motion.energy;
        assert!(peak > 0.95, "attack is fast: {peak}");
        let reach = furthest(&loud);
        assert!(reach > SWELL && reach <= loud.reach());

        for _ in 0..6 {
            motion.step(FRAME, None);
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
                let mut motion = Motion::new(OverlayStyle::Wave, intensity);
                let mut edge = Edge::default();
                for _ in 0..30 {
                    edge = wave(motion.step(FRAME, Some(&level(0.8))));
                }
                furthest(&edge)
            })
            .collect();
        assert!(reaches[0].abs() < 1e-6, "still at full voice: {reaches:?}");
        assert!(reaches.windows(2).all(|w| w[0] < w[1]), "{reaches:?}");
        assert!(reaches[4] <= MAX_REACH, "fits the room: {reaches:?}");
    }

    #[test]
    fn silence_draws_no_bars() {
        let mut motion = Motion::new(OverlayStyle::Bars, OverlayConfig::MAX_MOTION);
        for voice in [Some(&Spectrum::default()), None] {
            let shape = motion.step(FRAME, voice);
            assert!(shape.reach().abs() < f32::EPSILON);
            assert_eq!(bar_layout(bounds(), &bars(shape), 1.).count(), 0);
        }
    }

    #[test]
    fn bars_jump_with_the_voice_then_fall_within_the_drop_time() {
        let mut motion = Motion::new(OverlayStyle::Bars, 1.);
        let mut loud = Spectrum::default();
        loud.bands[5] = 1.;
        let shape = motion.step(FRAME, Some(&loud));
        assert!((shape.reach() - BAR_LENGTH).abs() < 1e-4, "no attack lag");

        let mut silent_for = 0.;
        let mut heights = Vec::new();
        while silent_for < 0.3 {
            heights.push(bars(motion.step(FRAME, Some(&Spectrum::default()))).heights[5]);
            silent_for += FRAME;
        }
        let at = |seconds: f32| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            heights[(seconds / FRAME).round() as usize - 1]
        };
        assert!(at(0.05) > 0.7, "a bar hangs a moment: {heights:?}");
        assert!(at(0.1) < 0.5, "then falls fast: {heights:?}");
        assert!(at(DROP + 2. * FRAME) < 1e-6, "and is gone: {heights:?}");
        assert!(heights.windows(2).all(|w| w[1] <= w[0]));

        // A band that only quietens stops its bar where it is now.
        let mut half = Spectrum::default();
        half.bands[5] = 0.5;
        motion.step(FRAME, Some(&loud));
        for _ in 0..30 {
            motion.step(FRAME, Some(&half));
        }
        assert!((bars(motion.step(FRAME, Some(&half))).heights[5] - 0.5).abs() < 1e-6);
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn bars_mirror_from_the_centre_outside_the_pill() {
        let mut motion = Motion::new(OverlayStyle::Bars, OverlayConfig::MAX_MOTION);
        let voice = Spectrum {
            bands: std::array::from_fn(|band| 1. - band as f32 / BANDS as f32),
            ..Spectrum::default()
        };
        let shape = bars(motion.step(FRAME, Some(&voice)));
        let bounds = bounds();
        let laid: Vec<_> = bar_layout(bounds, &shape, 1.).collect();
        assert_eq!(laid.len() % 4, 0, "pairs above and below");
        let centre = f32::from(bounds.center().x);
        let reach = MAX_REACH + 0.01;
        for (bar, _) in &laid {
            let (top, bottom) = (f32::from(bar.top()), f32::from(bar.bottom()));
            let above = bottom <= f32::from(bounds.top()) - BAR_GAP + 0.01;
            let below = top >= f32::from(bounds.bottom()) + BAR_GAP - 0.01;
            assert!(above || below, "{bar:?} overlaps the pill");
            assert!(top >= f32::from(bounds.top()) - reach);
            assert!(bottom <= f32::from(bounds.bottom()) + reach);
            assert!(f32::from(bar.left()) >= f32::from(bounds.left()) + RADIUS - BAR_PITCH);
            assert!(f32::from(bar.right()) <= f32::from(bounds.right()) - RADIUS + BAR_PITCH);
            // Its mirror image across the centre is just as tall.
            let mirror = 2. * centre - f32::from(bar.center().x);
            assert!(laid.iter().any(|(other, _)| {
                (f32::from(other.center().x) - mirror).abs() < 0.01
                    && (f32::from(other.size.height) - f32::from(bar.size.height)).abs() < 0.01
            }));
        }
        // Low bands, here the loudest, stand in the middle.
        let tallest = laid
            .iter()
            .max_by(|a, b| f32::from(a.0.size.height).total_cmp(&f32::from(b.0.size.height)))
            .unwrap();
        assert!((f32::from(tallest.0.center().x) - centre).abs() < BAR_PITCH);
    }

    #[test]
    fn motion_zero_draws_no_bars_even_at_full_voice() {
        let mut motion = Motion::new(OverlayStyle::Bars, 0.);
        let voice = Spectrum {
            bands: [1.; BANDS],
            ..Spectrum::default()
        };
        let shape = bars(motion.step(FRAME, Some(&voice)));
        assert_eq!(bar_layout(bounds(), &shape, 1.).count(), 0);
    }

    #[test]
    fn mix_ends_at_both_colours() {
        let (a, b) = (Hsla::black(), Hsla::white());
        assert_eq!(Rgba::from(mix(a, b, 0.)), Rgba::from(a));
        assert_eq!(Rgba::from(mix(a, b, 1.)), Rgba::from(b));
    }
}
