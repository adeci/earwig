//! The overlay's pill: a rounded rectangle over a soft drop shadow that
//! shows the voice in one of two styles, both following the same speech
//! bands, mirrored from the centre outward with low bands in the middle.
//! Wave: the top and bottom edges rise and fall with the bands, painted as
//! a path. Bars: thin accent bars above and below a still pill.

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
/// visible step, 2.5 blur radii) before the surface ends, and the edge or
/// the bars at full reach stay a pixel clear of it.
pub const ROOM: f32 = larger(SHADOW_DROP + 2.5 * SHADOW_BLUR, MAX_REACH + 1.);

/// Furthest anything moves out from the pill at the widest motion setting.
pub const MAX_REACH: f32 = larger(WAVE_REACH, BARS_REACH);
/// The edge at its peak...
const WAVE_REACH: f32 = OverlayConfig::MAX_MOTION * PEAK;
/// ...or full bars beyond their gap.
const BARS_REACH: f32 = BAR_GAP + OverlayConfig::MAX_MOTION * BAR_LENGTH;
/// The ambient shadow's offset below the pill and its blur (one standard
/// deviation of the Gaussian GPUI draws).
const SHADOW_DROP: f32 = 10.;
const SHADOW_BLUR: f32 = 12.;

/// How far the edge stands out where a band is at the top, at motion 1.
const PEAK: f32 = 18.;
/// Each band stands out from the mean of its neighbours by this many times
/// its difference from them (an unsharp mask), so formants peak instead of
/// merging into a plateau.
const CONTRAST: f32 = 2.;
/// The band profile is raised to this power, which narrows its peaks and
/// keeps quieter bands low while staying smooth.
const SHARPNESS: f32 = 1.8;
/// Fraction of the way from the centre to a corner where the highest band
/// stands; the bands spread evenly up to it.
const BAND_END: f32 = 0.85;
/// Siri's attenuation `(K / (K + x⁴))^K` with `x = FOCUS` times the way to
/// the corners: the edge moves most in the middle and calms toward the ends
/// (to 57% halfway, 10% at four fifths).
const FOCUS: f32 = 1.8;
const FOCUS_K: f32 = 2.;
/// The edge comes to rest over this span of the way to the corners (0 at
/// the centre, 1 where the corners begin), so corners and ends stay round.
const TAPER: (f32, f32) = (0.82, 1.);
/// A fine ripple travelling outward from the centre takes up to this share
/// off the edge's rise, so held sounds still shimmer...
const SHIMMER: f32 = 0.2;
/// ...with this many crests between the centre and a corner...
const SHIMMER_CYCLES: f32 = 10.;
/// ...passing at this many radians per second.
const SHIMMER_SPEED: f32 = 2. * TAU;

/// Seconds colours take to settle (theme or accent changes).
const FADE: f32 = 0.18;
/// A stalled frame resumes calmly instead of jumping.
const MAX_STEP: f32 = 0.05;

/// Distance between outline samples, in pixels drawn: a sixth of a band's
/// span on the overlay's pill, so peaks stay round.
const SAMPLE_SPACING: f32 = 1.5;

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
/// Seconds a full band takes to fall to nothing once it falls silent; it
/// gathers speed like a dropped object. Bands rise as fast as the analysis
/// does (a few milliseconds).
const DROP: f32 = 0.13;
const GRAVITY: f32 = 2. / (DROP * DROP);
/// Bar opacity from the shortest to the tallest.
const BAR_ALPHA: (f32, f32) = (0.55, 0.95);

const fn larger(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

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
    /// Each band's height (0..1) and how fast it is falling.
    heights: [f32; BANDS],
    falling: [f32; BANDS],
    /// Wave: phase of the shimmer.
    shimmer: f32,
}

impl Motion {
    pub fn new(style: OverlayStyle, intensity: f32) -> Self {
        Self {
            style,
            intensity,
            heights: [0.; BANDS],
            falling: [0.; BANDS],
            shimmer: 0.,
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
        self.follow(dt, voice);
        match self.style {
            OverlayStyle::Bars => Shape::Bars(Bars {
                heights: self.heights,
                length: BAR_LENGTH * self.intensity,
            }),
            OverlayStyle::Wave => {
                self.shimmer = (self.shimmer + SHIMMER_SPEED * dt).rem_euclid(TAU);
                Shape::Wave(Edge::new(
                    &self.heights,
                    PEAK * self.intensity,
                    self.shimmer,
                ))
            }
        }
    }

    /// A band jumps up with the voice and falls under gravity, never below
    /// where the voice has it now.
    fn follow(&mut self, dt: f32, voice: Option<&Spectrum>) {
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
    }
}

/// The fraction of the way to a target covered in `dt` seconds by a value
/// that closes on it with time constant `response`.
fn approach(dt: f32, response: f32) -> f32 {
    1. - (-dt / response).exp()
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

/// The pill's top and bottom edges at one instant: a smooth profile through
/// the band heights, mirrored from the centre to either corner.
#[derive(Clone, Copy, Debug, Default)]
pub struct Edge {
    /// Band heights (0..1) with their contrast raised.
    heights: [f32; BANDS],
    /// Pixels the edge stands out where a band is at the top.
    peak: f32,
    shimmer: f32,
}

impl Edge {
    #[allow(clippy::cast_possible_wrap)]
    fn new(heights: &[f32; BANDS], peak: f32, shimmer: f32) -> Edge {
        Edge {
            heights: std::array::from_fn(|band| {
                let [below, at, above] = [-1, 0, 1].map(|d| mirrored(heights, band as isize + d));
                (at + CONTRAST * (at - f32::midpoint(below, above))).clamp(0., 1.)
            }),
            peak,
            shimmer,
        }
    }

    /// Outward displacement in pixels at `from_centre`, the distance from
    /// the pill's middle as a fraction of the way to where its corners begin.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn offset(&self, from_centre: f32) -> f32 {
        let calm = smoothstep(TAPER.0, TAPER.1, from_centre);
        if self.peak <= 0. || calm >= 1. {
            return 0.;
        }
        let at = from_centre / BAND_END * (BANDS - 1) as f32;
        let below = at.floor();
        let band = below as isize;
        let height = catmull_rom(
            [band - 1, band, band + 1, band + 2].map(|b| mirrored(&self.heights, b)),
            at - below,
        )
        .clamp(0., 1.)
        .powf(SHARPNESS);
        let focus = (FOCUS_K / (FOCUS_K + (FOCUS * from_centre).powi(4))).powf(FOCUS_K);
        let shimmer =
            SHIMMER * 0.5 * (1. + (TAU * SHIMMER_CYCLES * from_centre - self.shimmer).sin());
        self.peak * focus * (1. - calm) * height * (1. - shimmer)
    }

    /// How far the edge stands out at the tallest band right now, in pixels.
    pub fn reach(&self) -> f32 {
        self.heights
            .iter()
            .copied()
            .fold(0., f32::max)
            .powf(SHARPNESS)
            * self.peak
    }
}

/// Band `band` of `heights`, mirrored across the centre and held beyond the
/// highest band.
fn mirrored(heights: &[f32; BANDS], band: isize) -> f32 {
    heights[band.unsigned_abs().min(BANDS - 1)]
}

/// The Catmull-Rom spline through four evenly spaced values at `t` (0..1)
/// of the way between the middle two: smooth, and through every value.
fn catmull_rom([before, from, to, after]: [f32; 4], t: f32) -> f32 {
    let (t2, t3) = (t * t, t * t * t);
    0.5 * (2. * from
        + (to - before) * t
        + (2. * before - 5. * from + 4. * to - after) * t2
        + (3. * (from - to) + after - before) * t3)
}

/// 0 up to `from`, 1 from `to`, easing in and out between.
fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0., 1.);
    t * t * (3. - 2. * t)
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
    paint_shadow(bounds, look, scale, window);
    match shape {
        Shape::Wave(edge) if edge.reach() > 0. => paint_wave(bounds, edge, look, scale, window),
        Shape::Wave(_) => paint_body(bounds, look, scale, window),
        Shape::Bars(bars) => {
            paint_body(bounds, look, scale, window);
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

/// The pill at rest: an exact rounded rectangle.
fn paint_body(bounds: Bounds<Pixels>, look: Look, scale: f32, window: &mut Window) {
    let mut body = fill(bounds, look.fill).corner_radii(Corners::all(px(RADIUS * scale)));
    if let Some(width) = look.border_width(scale) {
        body = body
            .border_widths(Edges::all(width))
            .border_color(look.accent);
    }
    window.paint_quad(body);
}

fn paint_wave(bounds: Bounds<Pixels>, edge: &Edge, look: Look, scale: f32, window: &mut Window) {
    let outline = wave_outline(bounds, edge, scale);
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

/// The outline of a pill at `bounds` with its edges moved out by `edge`.
fn wave_outline(bounds: Bounds<Pixels>, edge: &Edge, scale: f32) -> Vec<Point<Pixels>> {
    let radius = corner_radius(bounds, RADIUS * scale);
    let centre = f32::from(bounds.center().x);
    let half_run = (f32::from(bounds.size.width) / 2. - radius).max(f32::EPSILON);
    outline(bounds, radius)
        .map(|(at, normal)| {
            let offset = edge.offset((at.x - centre).abs() / half_run) * scale;
            point(px(at.x + normal.x * offset), px(at.y + normal.y * offset))
        })
        .collect()
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

/// A wide ambient shadow and a tight contact one, cast by the pill at rest.
/// Shadows are drawn only outside the element they belong to; tucking that
/// element a pixel inside the body (and spreading by as much) keeps its
/// anti-aliased rim shaded, while a translucent fill shows no shadow band
/// beneath it.
fn paint_shadow(bounds: Bounds<Pixels>, look: Look, scale: f32, window: &mut Window) {
    let tuck = px(scale);
    let (ambient, contact) = if look.dark { (0.5, 0.3) } else { (0.16, 0.1) };
    let shadow = |alpha: f32, y: f32, blur: f32| BoxShadow {
        color: Hsla::black().opacity(alpha * look.depth),
        offset: point(px(0.), px(y * scale)),
        blur_radius: px(blur * scale),
        spread_radius: tuck,
        inset: false,
    };
    window.paint_drop_shadows(
        bounds.dilate(-tuck),
        Corners::all(px(RADIUS * scale)),
        &[
            shadow(ambient, SHADOW_DROP, SHADOW_BLUR),
            shadow(contact, 1., 3.),
        ],
    );
}

/// The corner radius of a pill at `bounds`: `radius`, or less where the
/// pill is too small for it.
fn corner_radius(bounds: Bounds<Pixels>, radius: f32) -> f32 {
    radius
        .min(f32::from(bounds.size.width) / 2.)
        .min(f32::from(bounds.size.height) / 2.)
}

/// Evenly spaced points around the rounded rectangle with corner radius
/// `radius`, clockwise from the start of the top edge, with their outward
/// normal.
fn outline(bounds: Bounds<Pixels>, radius: f32) -> impl Iterator<Item = (Point<f32>, Point<f32>)> {
    let (left, top) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let (right, bottom) = (f32::from(bounds.right()), f32::from(bounds.bottom()));
    let r = corner_radius(bounds, radius);
    let straight_x = right - left - 2. * r;
    let straight_y = bottom - top - 2. * r;
    let arc = TAU / 4. * r;

    // Each side is a straight run followed by the corner that turns into the
    // next side; `angle` is the direction of the side's outward normal.
    let sides = [
        (point(left + r, top), straight_x, -TAU / 4.),
        (point(right, top + r), straight_y, 0.),
        (point(right - r, bottom), straight_x, TAU / 4.),
        (point(left, bottom - r), straight_y, TAU / 2.),
    ];
    sides.into_iter().flat_map(move |(start, length, angle)| {
        let normal = point(angle.cos(), angle.sin());
        let along = point(-normal.y, normal.x);
        let corner = point(start.x + along.x * length, start.y + along.y * length);
        let centre = point(corner.x - normal.x * r, corner.y - normal.y * r);
        let count = samples(length + arc);
        (0..count).map(move |i| {
            #[allow(clippy::cast_precision_loss)]
            let s = (length + arc) * i as f32 / count as f32;
            if s < length {
                (point(start.x + along.x * s, start.y + along.y * s), normal)
            } else {
                let turned = angle + (s - length) / r.max(f32::EPSILON);
                let normal = point(turned.cos(), turned.sin());
                (
                    point(centre.x + normal.x * r, centre.y + normal.y * r),
                    normal,
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

    /// The overlay's pill: wide enough that a band spans several samples.
    fn overlay_bounds() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(10.), px(20.)),
            size: size(px(560.), px(80.)),
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
        let (first, _) = points[0];
        assert_eq!((first.x, first.y), (10. + RADIUS, 20.));
        for (at, normal) in &points {
            assert!((normal.x.hypot(normal.y) - 1.).abs() < 1e-4);
            assert!((9.99..=210.01).contains(&at.x) && (19.99..=100.01).contains(&at.y));
            // On a straight side, or on the arc of the nearest corner.
            let nearest = point(
                at.x.clamp(10. + RADIUS, 210. - RADIUS),
                at.y.clamp(20. + RADIUS, 100. - RADIUS),
            );
            let from_centre = (at.x - nearest.x).hypot(at.y - nearest.y);
            assert!((from_centre - RADIUS).abs() < 1e-3, "{at:?} off the pill");
        }
    }

    fn full_voice() -> Spectrum {
        Spectrum {
            level: 1.,
            bands: [1.; BANDS],
        }
    }

    fn one_band(band: usize) -> Spectrum {
        let mut voice = Spectrum::default();
        voice.bands[band] = 1.;
        voice
    }

    #[allow(clippy::cast_precision_loss)]
    fn along(steps: u16) -> impl Iterator<Item = f32> {
        (0..=steps).map(move |i| 1.2 * f32::from(i) / f32::from(steps))
    }

    #[test]
    fn silence_is_the_exact_pill() {
        let mut motion = Motion::new(OverlayStyle::Wave, OverlayConfig::MAX_MOTION);
        motion.step(FRAME, Some(&full_voice()));
        let mut shapes = vec![Shape::still(OverlayStyle::Wave)];
        for _ in 0..30 {
            shapes.push(motion.step(FRAME, Some(&Spectrum::default())));
        }
        shapes.push(motion.step(FRAME, None));
        for &shape in shapes[..1].iter().chain(&shapes[16..]) {
            let edge = wave(shape);
            assert!(edge.reach().abs() < f32::EPSILON, "settled: {edge:?}");
            assert!(along(240).all(|d| edge.offset(d) == 0.));
            let rest: Vec<_> = outline(bounds(), RADIUS)
                .map(|(at, _)| point(px(at.x), px(at.y)))
                .collect();
            assert_eq!(wave_outline(bounds(), &edge, 1.), rest);
        }
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn one_band_lifts_its_own_stretch_of_the_edge_mirrored() {
        let spacing = BAND_END / (BANDS - 1) as f32;
        let mut peaks = Vec::new();
        for band in [2, 9, 17] {
            let mut motion = Motion::new(OverlayStyle::Wave, 1.);
            let edge = wave(motion.step(FRAME, Some(&one_band(band))));
            let home = band as f32 * spacing;
            let (peak_at, peak) = along(2400)
                .map(|d| (d, edge.offset(d)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            assert!(
                (peak_at - home).abs() < spacing / 2.,
                "band {band} peaks at {peak_at}"
            );
            assert!(peak > 0.15 * PEAK, "band {band}: {peak}");
            peaks.push(peak);
            for d in along(2400).filter(|d| (d - home).abs() >= 2. * spacing) {
                assert!(edge.offset(d) == 0., "band {band} moves the edge at {d}");
            }

            // The same stretch rises on either side of the centre, above and
            // below; the corners and ends stay put.
            let bounds = overlay_bounds();
            let (centre, middle) = (f32::from(bounds.center().x), f32::from(bounds.center().y));
            let half_run = f32::from(bounds.size.width) / 2. - RADIUS;
            let mut quadrants = [0_f32; 4];
            for (p, (at, _)) in wave_outline(bounds, &edge, 1.)
                .iter()
                .zip(outline(bounds, RADIUS))
            {
                let lift = (f32::from(p.x) - at.x).hypot(f32::from(p.y) - at.y);
                if lift == 0. {
                    continue;
                }
                let from_centre = (at.x - centre).abs() / half_run;
                assert!((from_centre - home).abs() < 2. * spacing, "{at:?} moved");
                let quadrant = usize::from(at.x > centre) + 2 * usize::from(at.y > middle);
                quadrants[quadrant] = quadrants[quadrant].max(lift);
            }
            for lift in quadrants {
                assert!(
                    (lift - peak).abs() < 0.1 * peak,
                    "band {band}: {quadrants:?} vs {peak}"
                );
            }
        }
        assert!(
            peaks.windows(2).all(|w| w[0] > w[1]),
            "calmer outward: {peaks:?}"
        );
    }

    #[test]
    fn the_wave_jumps_with_the_voice_then_falls_within_the_drop_time() {
        let mut motion = Motion::new(OverlayStyle::Wave, 1.);
        let shape = motion.step(FRAME, Some(&one_band(5)));
        assert!((shape.reach() - PEAK).abs() < 1e-4, "no attack lag");
        let mut silent_for = 0.;
        let mut reaches = Vec::new();
        while silent_for < DROP + 2. * FRAME {
            reaches.push(motion.step(FRAME, Some(&Spectrum::default())).reach());
            silent_for += FRAME;
        }
        assert!(reaches[2] > 0.5 * PEAK, "hangs a moment: {reaches:?}");
        assert!(
            reaches.last().unwrap().abs() < f32::EPSILON,
            "and settles: {reaches:?}"
        );
    }

    #[test]
    fn motion_setting_scales_the_wave_within_its_room() {
        let bounds = bounds();
        let reaches: Vec<f32> = [0., 0.5, 1., 1.5, OverlayConfig::MAX_MOTION]
            .into_iter()
            .map(|intensity| {
                let mut motion = Motion::new(OverlayStyle::Wave, intensity);
                (0..30)
                    .map(|_| wave(motion.step(FRAME, Some(&full_voice()))))
                    .flat_map(|edge| wave_outline(bounds, &edge, 1.))
                    .map(|p| {
                        let x = (f32::from(bounds.left()) - f32::from(p.x))
                            .max(f32::from(p.x) - f32::from(bounds.right()));
                        let y = (f32::from(bounds.top()) - f32::from(p.y))
                            .max(f32::from(p.y) - f32::from(bounds.bottom()));
                        x.max(y)
                    })
                    .fold(0., f32::max)
            })
            .collect();
        assert!(reaches[0].abs() < 1e-3, "still at full voice: {reaches:?}");
        assert!(reaches.windows(2).all(|w| w[0] < w[1]), "{reaches:?}");
        assert!(reaches[4] <= WAVE_REACH + 1e-3, "{reaches:?}");
        assert!(
            reaches[4] + 1. <= ROOM,
            "a pixel clear of the surface's edge"
        );
        assert!(reaches[4] > 0.9 * WAVE_REACH, "{reaches:?}");
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
