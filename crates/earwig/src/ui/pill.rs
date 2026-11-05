//! The overlay's pill: a rounded rectangle over a soft drop shadow that
//! shows the voice in one of two styles, both following the speech bands
//! mirrored from the centre outward, low bands in the middle. Wave: the
//! outline behaves like water the voice disturbs (see [`water`]), painted
//! as a path. Bars: thin accent bars above and below a still pill.

use std::f32::consts::TAU;
use std::time::Instant;

use earwig_core::config::{OverlayConfig, OverlayStyle};
use earwig_core::spectrum::{BANDS, Spectrum};
use gpui_kit::component::Theme;
use gpui_kit::{
    App, Bounds, BoxShadow, Corners, Edges, Hsla, PathBuilder, Pixels, Point, Rgba, Window, fill,
    point, px, size,
};

use crate::ui::water::{self, NODES, Ring};

/// Corner radius of the pill at rest.
const RADIUS: f32 = 24.;
/// Transparent room the pill needs around it: its shadow fades out (below a
/// visible step, 2.5 blur radii) before the surface ends, and the edge or
/// the bars at full reach stay a pixel clear of it.
pub const ROOM: f32 = larger(SHADOW_DROP + 2.5 * SHADOW_BLUR, MAX_REACH + 1.);

/// Furthest anything moves out from the pill at the widest motion setting.
pub const MAX_REACH: f32 = larger(WAVE_REACH, BARS_REACH);
/// The edge at its furthest...
const WAVE_REACH: f32 = OverlayConfig::MAX_MOTION * DEPTH;
/// ...or full bars beyond their gap.
const BARS_REACH: f32 = BAR_GAP + OverlayConfig::MAX_MOTION * BAR_LENGTH;
/// The ambient shadow's offset below the pill and its blur (one standard
/// deviation of the Gaussian GPUI draws).
const SHADOW_DROP: f32 = 10.;
const SHADOW_BLUR: f32 = 12.;

/// Pixels a full wave moves the edge at motion 1; the water's heights are
/// eased into this with `tanh`, so crests stay round and within reach.
const DEPTH: f32 = 10.;

/// Seconds colours take to settle (theme or accent changes).
const FADE: f32 = 0.18;
/// A stalled frame resumes calmly instead of jumping.
const MAX_STEP: f32 = 0.05;

/// Distance between outline samples, in pixels drawn: under a third of a
/// node of the water on the overlay's long runs, so crests stay round.
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
    /// Wave: the water the bands disturb.
    water: Ring,
}

impl Motion {
    pub fn new(style: OverlayStyle, intensity: f32) -> Self {
        Self {
            style,
            intensity,
            heights: [0.; BANDS],
            falling: [0.; BANDS],
            water: Ring::default(),
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
        let rises = self.follow(dt, voice);
        match self.style {
            OverlayStyle::Bars => Shape::Bars(Bars {
                heights: self.heights,
                length: BAR_LENGTH * self.intensity,
            }),
            OverlayStyle::Wave => {
                self.water.advance(dt, &self.heights, &rises);
                Shape::Wave(Edge::new(&self.water, DEPTH * self.intensity))
            }
        }
    }

    /// A band jumps up with the voice and falls under gravity, never below
    /// where the voice has it now. Returns how far each band rose.
    fn follow(&mut self, dt: f32, voice: Option<&Spectrum>) -> [f32; BANDS] {
        let mut rises = [0.; BANDS];
        for (band, (height, falling)) in self.heights.iter_mut().zip(&mut self.falling).enumerate()
        {
            let target = voice.map_or(0., |v| v.bands[band]);
            if target >= *height {
                rises[band] = target - *height;
                *height = target;
                *falling = 0.;
            } else {
                *falling += GRAVITY * dt;
                *height = (*height - *falling * dt).max(target);
            }
        }
        rises
    }
}

/// The fraction of the way to a target covered in `dt` seconds by a value
/// that closes on it with time constant `response`.
fn approach(dt: f32, response: f32) -> f32 {
    1. - (-dt / response).exp()
}

/// What the voice does to the pill at one instant. Built once a frame and
/// handed to paint by value; boxing the wave would allocate every frame.
#[derive(Clone, Copy, Debug)]
#[allow(clippy::large_enum_variant)]
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

/// The pill's outline at one instant: how far each node of the water stands
/// out from rest, in pixels.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    offsets: [f32; NODES],
    /// The furthest any node stands out or in, or is swinging to.
    reach: f32,
    /// How far the outline dips inside the pill at rest, at most.
    inward: f32,
}

impl Default for Edge {
    fn default() -> Self {
        Self {
            offsets: [0.; NODES],
            reach: 0.,
            inward: 0.,
        }
    }
}

impl Edge {
    /// `water` drawn with a full wave `depth` pixels deep.
    fn new(water: &Ring, depth: f32) -> Edge {
        if depth <= 0. || water.rests() {
            return Edge::default();
        }
        let offsets = water.surface().map(|h| depth * h.tanh());
        let inward = offsets.iter().fold(0_f32, |low, &o| low.min(o));
        Edge {
            offsets,
            reach: depth * water.amplitude().tanh(),
            inward: -inward,
        }
    }

    /// Outward displacement in pixels at `node`, a position along the ring
    /// of nodes (0..NODES), between the nodes on either side.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn offset(&self, node: f32) -> f32 {
        let below = node.floor();
        let at = |k: usize| self.offsets[(below as usize + k + NODES - 1) % NODES];
        catmull_rom([at(0), at(1), at(2), at(3)], node - below)
    }

    /// How far the edge is from the pill at rest right now, in pixels.
    pub fn reach(&self) -> f32 {
        self.reach
    }
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
        Shape::Wave(edge) if edge.reach() > 0. => {
            paint_shadow(bounds, edge.inward, look, scale, window);
            paint_wave(bounds, edge, look, scale, window);
        }
        Shape::Wave(_) => {
            paint_shadow(bounds, 0., look, scale, window);
            paint_body(bounds, look, scale, window);
        }
        Shape::Bars(bars) => {
            paint_shadow(bounds, 0., look, scale, window);
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

/// The outline of a pill at `bounds` with its edge moved out by `edge`.
fn wave_outline(bounds: Bounds<Pixels>, edge: &Edge, scale: f32) -> Vec<Point<Pixels>> {
    outline(bounds, RADIUS * scale)
        .map(|(at, normal, node)| {
            let offset = edge.offset(node) * scale;
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
/// element a pixel inside the body, or inside where the edge dips by
/// `inward` pixels (and spreading by as much), keeps the rim and any
/// trough shaded, while a translucent fill shows no shadow band beneath it.
fn paint_shadow(bounds: Bounds<Pixels>, inward: f32, look: Look, scale: f32, window: &mut Window) {
    let tuck = px((inward + 1.) * scale);
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
/// normal and where they fall on the ring of water nodes (0..NODES): each
/// straight run and corner spreads its nodes evenly over its length.
fn outline(
    bounds: Bounds<Pixels>,
    radius: f32,
) -> impl Iterator<Item = (Point<f32>, Point<f32>, f32)> {
    let (left, top) = (f32::from(bounds.left()), f32::from(bounds.top()));
    let (right, bottom) = (f32::from(bounds.right()), f32::from(bounds.bottom()));
    let r = corner_radius(bounds, radius);
    let straight_x = right - left - 2. * r;
    let straight_y = bottom - top - 2. * r;
    let arc = TAU / 4. * r;

    // Each side is a straight run followed by the corner that turns into the
    // next side; `angle` is the direction of the side's outward normal.
    let sides = [
        (point(left + r, top), straight_x, -TAU / 4., water::RUN),
        (point(right, top + r), straight_y, 0., water::SIDE),
        (point(right - r, bottom), straight_x, TAU / 4., water::RUN),
        (point(left, bottom - r), straight_y, TAU / 2., water::SIDE),
    ];
    let mut first_node = 0;
    sides
        .into_iter()
        .flat_map(move |(start, length, angle, nodes)| {
            let normal = point(angle.cos(), angle.sin());
            let along = point(-normal.y, normal.x);
            let corner = point(start.x + along.x * length, start.y + along.y * length);
            let centre = point(corner.x - normal.x * r, corner.y - normal.y * r);
            let count = samples(length + arc);
            let base = first_node;
            first_node += nodes + water::CORNER;
            #[allow(clippy::cast_precision_loss)]
            let (base, nodes, corner_nodes) = (base as f32, nodes as f32, water::CORNER as f32);
            (0..count).map(move |i| {
                #[allow(clippy::cast_precision_loss)]
                let s = (length + arc) * i as f32 / count as f32;
                if s < length {
                    (
                        point(start.x + along.x * s, start.y + along.y * s),
                        normal,
                        base + nodes * s / length,
                    )
                } else {
                    let round = (s - length) / r.max(f32::EPSILON);
                    let turned = angle + round;
                    let normal = point(turned.cos(), turned.sin());
                    (
                        point(centre.x + normal.x * r, centre.y + normal.y * r),
                        normal,
                        base + nodes + corner_nodes * round / (TAU / 4.),
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

    fn wave(shape: &Shape) -> Edge {
        match shape {
            Shape::Wave(edge) => *edge,
            Shape::Bars(_) => panic!("bars from a wave"),
        }
    }

    fn bars(shape: &Shape) -> Bars {
        match shape {
            Shape::Bars(bars) => *bars,
            Shape::Wave(_) => panic!("a wave from bars"),
        }
    }

    #[test]
    fn outline_hugs_the_rounded_rectangle() {
        let points: Vec<_> = outline(bounds(), RADIUS).collect();
        let (first, _, node) = points[0];
        assert_eq!((first.x, first.y, node), (10. + RADIUS, 20., 0.));
        for (at, normal, node) in &points {
            assert!((normal.x.hypot(normal.y) - 1.).abs() < 1e-4);
            assert!((9.99..=210.01).contains(&at.x) && (19.99..=100.01).contains(&at.y));
            // On a straight side, or on the arc of the nearest corner.
            let nearest = point(
                at.x.clamp(10. + RADIUS, 210. - RADIUS),
                at.y.clamp(20. + RADIUS, 100. - RADIUS),
            );
            let from_centre = (at.x - nearest.x).hypot(at.y - nearest.y);
            assert!((from_centre - RADIUS).abs() < 1e-3, "{at:?} off the pill");
            #[allow(clippy::cast_precision_loss)]
            let ring = 0. ..NODES as f32;
            assert!(ring.contains(node));
        }
        assert!(points.windows(2).all(|w| w[0].2 < w[1].2));
        // The top run spreads its nodes evenly from left to right.
        let run = 200. - 2. * RADIUS;
        let on_top_run = |(at, _, _): &&(Point<f32>, Point<f32>, f32)| (at.y - 20.).abs() < 1e-4;
        for (at, _, node) in points.iter().filter(on_top_run) {
            #[allow(clippy::cast_precision_loss)]
            let expected = water::RUN as f32 * (at.x - 10. - RADIUS) / run;
            assert!((node - expected).abs() < 1e-3, "{at:?} at node {node}");
        }
    }

    fn full_voice() -> Spectrum {
        Spectrum {
            level: 1.,
            bands: [1.; BANDS],
        }
    }

    /// Talking: syllables at 4.5 Hz with formants that drift.
    #[allow(clippy::cast_precision_loss)]
    fn talk(t: f32) -> Spectrum {
        let syllable = (TAU * 4.5 * t).sin().max(0.);
        Spectrum {
            level: 0.3 + 0.5 * syllable,
            bands: std::array::from_fn(|band| {
                syllable * (0.5 + 0.5 * (band as f32 * 0.5 + 1.3 * t).sin())
            }),
        }
    }

    fn rest_outline(bounds: Bounds<Pixels>) -> Vec<Point<Pixels>> {
        outline(bounds, RADIUS)
            .map(|(at, _, _)| point(px(at.x), px(at.y)))
            .collect()
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn speech_ripples_on_then_settles_to_the_exact_pill() {
        let still = wave(&Shape::still(OverlayStyle::Wave));
        assert_eq!(wave_outline(bounds(), &still, 1.), rest_outline(bounds()));

        let mut motion = Motion::new(OverlayStyle::Wave, OverlayConfig::MAX_MOTION);
        let mut speaking = 0_f32;
        for frame in 0..60 {
            let shape = motion.step(FRAME, Some(&talk(frame as f32 * FRAME)));
            speaking = speaking.max(shape.reach());
        }
        assert!(speaking > 4., "speech moves the edge: {speaking}");
        let after: Vec<f32> = (0..180)
            .map(|frame| {
                let voice = (frame < 60).then(Spectrum::default);
                motion.step(FRAME, voice.as_ref()).reach()
            })
            .collect();
        assert!(after[18] > 0.35, "still rippling 0.3 s after: {after:?}");
        let settled = after.iter().position(|&r| r == 0.).expect("settles");
        assert!(
            (45..=120).contains(&settled),
            "settles after {settled} frames"
        );
        assert!(after[settled..].iter().all(|&r| r == 0.));
        let edge = wave(&motion.step(FRAME, None));
        assert_eq!(wave_outline(bounds(), &edge, 1.), rest_outline(bounds()));
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn motion_setting_scales_the_wave_within_its_room() {
        let bounds = bounds();
        let quiet = Spectrum::default();
        let reaches: Vec<f32> = [0., 0.5, 1., 1.5, OverlayConfig::MAX_MOTION]
            .into_iter()
            .map(|intensity| {
                let mut motion = Motion::new(OverlayStyle::Wave, intensity);
                // Talking, then the loudest the voice can push: full bands
                // jumping in and out every other frame, then held.
                (0..240)
                    .map(|frame| {
                        let voice = match frame {
                            0..60 => talk(frame as f32 * FRAME),
                            60..180 if frame % 2 == 0 => quiet,
                            _ => full_voice(),
                        };
                        wave(&motion.step(FRAME, Some(&voice)))
                    })
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
    }

    #[test]
    fn silence_draws_no_bars() {
        let mut motion = Motion::new(OverlayStyle::Bars, OverlayConfig::MAX_MOTION);
        for voice in [Some(&Spectrum::default()), None] {
            let shape = motion.step(FRAME, voice);
            assert!(shape.reach().abs() < f32::EPSILON);
            assert_eq!(bar_layout(bounds(), &bars(&shape), 1.).count(), 0);
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
            heights.push(bars(&motion.step(FRAME, Some(&Spectrum::default()))).heights[5]);
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
        assert!((bars(&motion.step(FRAME, Some(&half))).heights[5] - 0.5).abs() < 1e-6);
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn bars_mirror_from_the_centre_outside_the_pill() {
        let mut motion = Motion::new(OverlayStyle::Bars, OverlayConfig::MAX_MOTION);
        let voice = Spectrum {
            bands: std::array::from_fn(|band| 1. - band as f32 / BANDS as f32),
            ..Spectrum::default()
        };
        let shape = bars(&motion.step(FRAME, Some(&voice)));
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
        let shape = bars(&motion.step(FRAME, Some(&voice)));
        assert_eq!(bar_layout(bounds(), &shape, 1.).count(), 0);
    }

    #[test]
    fn mix_ends_at_both_colours() {
        let (a, b) = (Hsla::black(), Hsla::white());
        assert_eq!(Rgba::from(mix(a, b, 0.)), Rgba::from(a));
        assert_eq!(Rgba::from(mix(a, b, 1.)), Rgba::from(b));
    }
}
