//! The overlay's pill: a rounded rectangle whose edge ripples like water with the voice.

use std::f32::consts::TAU;
use std::time::Instant;

use earwig_core::config::OverlayConfig;
use earwig_core::spectrum::{BANDS, Spectrum};
use gpui_kit::component::Theme;
use gpui_kit::{
    App, Bounds, BoxShadow, Corners, Edges, Hsla, PathBuilder, Pixels, Point, Rgba, Window, fill,
    point, px, size,
};

use crate::ui::crt;
use crate::ui::water::{self, NODES, Ring};

const RADIUS: f32 = 24.;
/// Clear room around the pill, for the shadow to fade out and the edge's fullest swell.
pub const ROOM: f32 = (SHADOW_DROP + 2.5 * SHADOW_BLUR).max(DEPTH + 1.);
const SHADOW_DROP: f32 = 10.;
/// One standard deviation of GPUI's Gaussian.
const SHADOW_BLUR: f32 = 12.;

/// How far a full wave moves the edge; heights ease into it with `tanh`.
pub const DEPTH: f32 = 16.;
/// Troughs dip less than crests rise, so they don't crowd the text.
const TROUGH: f32 = 0.4;

/// Seconds colours take to settle after a theme or accent change.
const FADE: f32 = 0.18;
/// Cap on a frame's step, so a stall resumes calmly.
const MAX_STEP: f32 = 0.05;

/// Pixels between outline samples: under a third of a water node, so crests stay round.
const SAMPLE_SPACING: f32 = 1.5;

/// Seconds a full band takes to fall silent, accelerating like a dropped object.
const DROP: f32 = 0.13;
const GRAVITY: f32 = 2. / (DROP * DROP);

/// Switching off, the pill tints toward an ink: Tokyo Night's foreground on dark...
const INK_DARK: u32 = 0x00c0_caf5;
/// ...the accent at this share of its saturation on light.
const INK_SATURATION: f32 = 0.45;
/// How far toward the ink, (dark, light).
const INK: (f32, f32) = (0.62, 0.5);
const RIPPLE_STROKE: f32 = 1.5;
/// The halo around the line and dot: opacity (dark, light), blur, spread, saturation share.
const HALO: (f32, f32) = (0.22, 0.16);
const HALO_BLUR: f32 = 5.;
const HALO_SPREAD: f32 = 1.;
const HALO_SATURATION: f32 = 0.6;
/// Stands in for a grey accent.
const TOKYO_BLUE: u32 = 0x007a_a2f7;
/// Below this saturation an accent counts as grey.
const GREY: f32 = 0.15;

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

/// The voice's bands and the water they disturb.
#[derive(Default)]
pub struct Motion {
    /// Each band's height (0..1) and how fast it is falling.
    heights: [f32; BANDS],
    falling: [f32; BANDS],
    water: Ring,
}

impl Motion {
    /// Deterministic ripples, for tests.
    #[cfg(test)]
    pub fn seeded(seed: u64) -> Self {
        Self {
            heights: [0.; BANDS],
            falling: [0.; BANDS],
            water: Ring::seeded(seed),
        }
    }

    /// `voice` is `None` once recording stopped, which lets the pill settle.
    pub fn step(&mut self, dt: f32, voice: Option<&Spectrum>) -> Edge {
        let rises = self.follow(dt, voice);
        self.water.advance(dt, &self.heights, &rises);
        Edge::new(&self.water)
    }

    /// Bands jump up with the voice and fall under gravity; returns how far each rose.
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

/// Share of the way to a target covered in `dt` with time constant `response`.
fn approach(dt: f32, response: f32) -> f32 {
    1. - (-dt / response).exp()
}

/// How far each water node pushes the outline out, in pixels; the default is at rest.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    offsets: [f32; NODES],
    /// The furthest any node stands out or in, or is swinging to.
    reach: f32,
    /// Deepest dip inside the resting outline.
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
    fn new(water: &Ring) -> Edge {
        if water.rests() {
            return Edge::default();
        }
        let offsets = water.surface().map(|h| {
            let offset = DEPTH * h.tanh();
            if offset < 0. { offset * TROUGH } else { offset }
        });
        let inward = offsets.iter().fold(0_f32, |low, &o| low.min(o));
        Edge {
            offsets,
            reach: DEPTH * water.amplitude().tanh(),
            inward: -inward,
        }
    }

    /// Scaled down as the picture collapses.
    fn scaled(self, share: f32) -> Edge {
        Edge {
            offsets: self.offsets.map(|o| o * share),
            reach: self.reach * share,
            inward: self.inward * share,
        }
    }

    /// Displacement at a fractional `node`, between the nodes either side.
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

    pub fn reach(&self) -> f32 {
        self.reach
    }
}

/// Catmull-Rom between the middle two of four evenly spaced values.
fn catmull_rom([before, from, to, after]: [f32; 4], t: f32) -> f32 {
    let (t2, t3) = (t * t, t * t * t);
    0.5 * (2. * from
        + (to - before) * t
        + (2. * before - 5. * from + 4. * to - after) * t2
        + (3. * (from - to) + after - before) * t3)
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// Tokyo Night charcoal on dark, the theme's background on light; the shadow fades too.
    pub fn themed(theme: &Theme, accent: Hsla, opacity: f32, border: f32) -> Look {
        let dark = theme.is_dark();
        let fill: Hsla = if dark {
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

    /// At least a pixel, so a scaled-down border stays visible.
    fn border_width(&self, scale: f32) -> Option<Pixels> {
        (self.border > 0.).then(|| px((self.border * scale).max(1.)))
    }

    fn ink(&self) -> Hsla {
        if self.dark {
            gpui_kit::rgb(INK_DARK).into()
        } else {
            let mut ink = self.accent.opacity(1.);
            ink.s *= INK_SATURATION;
            ink
        }
    }

    /// The dot's own mid tone on dark; the ink on light, where the dot is too pale.
    fn ripple(&self) -> Hsla {
        if self.dark {
            mix(self.fill.opacity(1.), self.ink(), INK.0)
        } else {
            self.ink()
        }
    }

    fn halo(&self) -> Hsla {
        let mut halo = if self.accent.s < GREY {
            gpui_kit::rgb(TOKYO_BLUE).into()
        } else {
            self.accent.opacity(1.)
        };
        halo.s *= HALO_SATURATION;
        halo
    }

    /// Tinted toward the ink as the pill thins to the line, fading with the dot.
    fn switching_off(self, frame: &crt::Frame) -> Look {
        let ink = self.ink();
        let share = if self.dark { INK.0 } else { INK.1 };
        let tint = |colour: Hsla| {
            mix(colour, ink.opacity(colour.a), frame.tint * share).opacity(frame.alpha)
        };
        Look {
            fill: tint(self.fill),
            accent: tint(self.accent),
            dark: self.dark,
            depth: self.depth * frame.alpha,
            border: self.border,
        }
    }
}

/// Reduced motion, or no GPU (animating would starve the decoder).
pub fn holds_still(window: &Window, cx: &App) -> bool {
    cx.reduce_motion()
        || window
            .gpu_specs()
            .is_some_and(|gpu| gpu.is_software_emulated)
}

pub fn paint(bounds: Bounds<Pixels>, edge: &Edge, look: Look, scale: f32, window: &mut Window) {
    let depth = (look.depth, look.depth);
    paint_shadow(bounds, edge.inward, look, depth, scale, window);
    if edge.reach() > 0. {
        paint_wave(bounds, edge, look, scale, window);
    } else {
        paint_body(bounds, look, scale, window);
    }
}

/// The pill mid switch-off (see [`crt`]), then the ring the closing dot releases.
pub fn paint_crt(
    bounds: Bounds<Pixels>,
    frame: &crt::Frame,
    edge: &Edge,
    look: Look,
    window: &mut Window,
) {
    let centre = bounds.center();
    let pill = Bounds::centered_at(centre, size(px(frame.width), px(frame.height)));
    let edge = edge.scaled(frame.picture);
    let off = look.switching_off(frame);
    if frame.width > 0. && frame.alpha > 0. {
        let depth = (off.depth * frame.picture.powf(1.5), off.depth);
        paint_shadow(pill, edge.inward, off, depth, 1., window);
        paint_halo(pill, &look, frame, window);
        paint_wave(pill, &edge, off, 1., window);
    }
    if let Some(ripple) = frame.ripple {
        let ring = Bounds::centered_at(centre, size(px(ripple.diameter), px(ripple.diameter)));
        let mut line = PathBuilder::stroke(px(RIPPLE_STROKE));
        line.add_polygon(&wave_outline(ring, &Edge::default(), 1.), true);
        if let Ok(path) = line.build() {
            window.paint_path(path, look.ripple().opacity(ripple.opacity));
        }
    }
}

/// Tucked inside like the shadow, so it meets the line.
fn paint_halo(pill: Bounds<Pixels>, look: &Look, frame: &crt::Frame, window: &mut Window) {
    let opacity = frame.glow * frame.alpha * if look.dark { HALO.0 } else { HALO.1 };
    if opacity <= 0. {
        return;
    }
    let middle = f32::from(pill.size.width.min(pill.size.height)) / 2.;
    let tuck = px((middle - 0.25).clamp(0., 1.));
    let inner = pill.dilate(-tuck);
    window.paint_drop_shadows(
        inner,
        Corners::all(px(corner_radius(inner, RADIUS))),
        &[BoxShadow {
            color: look.halo().opacity(opacity),
            offset: point(px(0.), px(0.)),
            blur_radius: px(HALO_BLUR),
            spread_radius: tuck + px(HALO_SPREAD),
            inset: false,
        }],
    );
}

/// The pill at rest: an exact rounded rectangle.
fn paint_body(bounds: Bounds<Pixels>, look: Look, scale: f32, window: &mut Window) {
    let radius = corner_radius(bounds, RADIUS * scale);
    let mut body = fill(bounds, look.fill).corner_radii(Corners::all(px(radius)));
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

fn wave_outline(bounds: Bounds<Pixels>, edge: &Edge, scale: f32) -> Vec<Point<Pixels>> {
    outline(bounds, RADIUS * scale)
        .map(|(at, normal, node)| {
            let offset = edge.offset(node) * scale;
            point(px(at.x + normal.x * offset), px(at.y + normal.y * offset))
        })
        .collect()
}

/// GPUI only shadows outside a box, so tuck it inside the rim (and troughs) and spread back.
fn paint_shadow(
    bounds: Bounds<Pixels>,
    inward: f32,
    look: Look,
    (ambient_depth, contact_depth): (f32, f32),
    scale: f32,
    window: &mut Window,
) {
    let half = f32::from(bounds.size.width.min(bounds.size.height)) / 2.;
    let tuck = px(((inward + 1.) * scale).min(half));
    let (ambient, contact) = if look.dark { (0.5, 0.3) } else { (0.16, 0.1) };
    let shadow = |alpha: f32, y: f32, blur: f32| BoxShadow {
        color: Hsla::black().opacity(alpha),
        offset: point(px(0.), px(y * scale)),
        blur_radius: px(blur * scale),
        spread_radius: tuck,
        inset: false,
    };
    let inner = bounds.dilate(-tuck);
    window.paint_drop_shadows(
        inner,
        Corners::all(px(corner_radius(inner, RADIUS * scale))),
        &[
            shadow(ambient * ambient_depth, SHADOW_DROP, SHADOW_BLUR),
            shadow(contact * contact_depth, 1., 3.),
        ],
    );
}

/// `radius`, or less if the pill is too small for it.
fn corner_radius(bounds: Bounds<Pixels>, radius: f32) -> f32 {
    radius
        .min(f32::from(bounds.size.width) / 2.)
        .min(f32::from(bounds.size.height) / 2.)
}

/// Even points clockwise from the top-left: position, outward normal, place on the water ring.
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

    // Each side: a straight run, then the corner into the next; `angle` is its outward normal.
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

/// Lerp in sRGB, so hues don't swing.
fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
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
        assert_eq!(
            wave_outline(bounds(), &Edge::default(), 1.),
            rest_outline(bounds())
        );

        let mut motion = Motion::default();
        let mut speaking = 0_f32;
        for frame in 0..60 {
            let edge = motion.step(FRAME, Some(&talk(frame as f32 * FRAME)));
            speaking = speaking.max(edge.reach());
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
        let edge = motion.step(FRAME, None);
        assert_eq!(wave_outline(bounds(), &edge, 1.), rest_outline(bounds()));
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn the_loudest_voice_keeps_the_wave_within_its_room() {
        let bounds = bounds();
        let quiet = Spectrum::default();
        let mut motion = Motion::default();
        // Talking, then the loudest push: full bands flickering every other frame, then held.
        let reach = (0..240)
            .map(|frame| {
                let voice = match frame {
                    0..60 => talk(frame as f32 * FRAME),
                    60..180 if frame % 2 == 0 => quiet,
                    _ => full_voice(),
                };
                motion.step(FRAME, Some(&voice))
            })
            .flat_map(|edge| wave_outline(bounds, &edge, 1.))
            .map(|p| {
                let x = (f32::from(bounds.left()) - f32::from(p.x))
                    .max(f32::from(p.x) - f32::from(bounds.right()));
                let y = (f32::from(bounds.top()) - f32::from(p.y))
                    .max(f32::from(p.y) - f32::from(bounds.bottom()));
                x.max(y)
            })
            .fold(0., f32::max);
        assert!(reach > DEPTH / 2., "full voice swells the edge: {reach}");
        assert!(reach <= DEPTH + 1e-3, "{reach}");
        assert!(reach + 1. <= ROOM, "a pixel clear of the surface's edge");
    }

    #[test]
    fn bands_jump_with_the_voice_then_fall_within_the_drop_time() {
        let mut motion = Motion::default();
        let mut loud = Spectrum::default();
        loud.bands[5] = 1.;
        motion.step(FRAME, Some(&loud));
        assert!((motion.heights[5] - 1.).abs() < 1e-6, "no attack lag");

        let mut silent_for = 0.;
        let mut heights = Vec::new();
        while silent_for < 0.3 {
            motion.step(FRAME, Some(&Spectrum::default()));
            heights.push(motion.heights[5]);
            silent_for += FRAME;
        }
        let at = |seconds: f32| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            heights[(seconds / FRAME).round() as usize - 1]
        };
        assert!(at(0.05) > 0.7, "a band hangs a moment: {heights:?}");
        assert!(at(0.1) < 0.5, "then falls fast: {heights:?}");
        assert!(at(DROP + 2. * FRAME) < 1e-6, "and is gone: {heights:?}");
        assert!(heights.windows(2).all(|w| w[1] <= w[0]));

        // A band that only quietens stops where the voice is now.
        let mut half = Spectrum::default();
        half.bands[5] = 0.5;
        motion.step(FRAME, Some(&loud));
        for _ in 0..31 {
            motion.step(FRAME, Some(&half));
        }
        assert!((motion.heights[5] - 0.5).abs() < 1e-6);
    }
}
