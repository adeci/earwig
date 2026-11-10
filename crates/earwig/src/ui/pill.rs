//! The overlay's pill: a rounded rectangle over a soft drop shadow whose
//! outline behaves like water the voice disturbs (see [`water`]), following
//! the speech bands mirrored from the centre outward, low bands in the
//! middle, and painted as a path. Once the dictation is finished it
//! switches off like an old CRT screen (see [`crt`]).

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

/// Corner radius of the pill at rest.
const RADIUS: f32 = 24.;
/// Transparent room the pill needs around it: its shadow fades out (below a
/// visible step, 2.5 blur radii) before the surface ends, and the edge at
/// its furthest stays a pixel clear of it.
pub const ROOM: f32 = larger(SHADOW_DROP + 2.5 * SHADOW_BLUR, DEPTH + 1.);
/// The ambient shadow's offset below the pill and its blur (one standard
/// deviation of the Gaussian GPUI draws).
const SHADOW_DROP: f32 = 10.;
const SHADOW_BLUR: f32 = 12.;

/// Pixels a full wave moves the edge, and so the furthest it goes: the
/// water's heights are eased into this with `tanh`, so crests stay round.
pub const DEPTH: f32 = 16.;
/// Troughs sink this fraction as deep as crests rise, so a dip never
/// crowds the text inside the pill.
const TROUGH: f32 = 0.4;

/// Seconds colours take to settle (theme or accent changes).
const FADE: f32 = 0.18;
/// A stalled frame resumes calmly instead of jumping.
const MAX_STEP: f32 = 0.05;

/// Distance between outline samples, in pixels drawn: under a third of a
/// node of the water on the overlay's long runs, so crests stay round.
const SAMPLE_SPACING: f32 = 1.5;

/// Seconds a full band takes to fall to nothing once it falls silent; it
/// gathers speed like a dropped object. Bands rise as fast as the analysis
/// does (a few milliseconds).
const DROP: f32 = 0.13;
const GRAVITY: f32 = 2. / (DROP * DROP);

/// Switching off, the line, the dot and the ripple ease toward a soft ink
/// in the overlay's palette: Tokyo Night's foreground on the dark theme...
const INK_DARK: u32 = 0x00c0_caf5;
/// ...the accent with this share of its saturation on the light one.
const INK_SATURATION: f32 = 0.45;
/// How far toward the ink the line goes, on the dark theme and the light
/// one: a gentle tint, never a light of its own.
const INK: (f32, f32) = (0.62, 0.5);
/// Width of the ripple's ring, in pixels.
const RIPPLE_STROKE: f32 = 1.5;
/// A very faint halo around the line and the dot: its opacity on the dark
/// theme and the light one, its blur and spread in pixels, and the share
/// of the accent's saturation it keeps (Tokyo Night's blue stands in for
/// an accent that is grey).
const HALO: (f32, f32) = (0.22, 0.16);
const HALO_BLUR: f32 = 5.;
const HALO_SPREAD: f32 = 1.;
const HALO_SATURATION: f32 = 0.6;
const TOKYO_BLUE: u32 = 0x007a_a2f7;
/// Below this saturation an accent counts as grey.
const GREY: f32 = 0.15;

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

/// How the pill follows the voice from frame to frame.
#[derive(Default)]
pub struct Motion {
    /// Each band's height (0..1) and how fast it is falling.
    heights: [f32; BANDS],
    falling: [f32; BANDS],
    /// The water the bands disturb.
    water: Ring,
}

impl Motion {
    /// Advance by `dt` seconds. `voice` is the latest reading while
    /// recording, `None` once recording stopped, which lets the pill settle.
    pub fn step(&mut self, dt: f32, voice: Option<&Spectrum>) -> Edge {
        let rises = self.follow(dt, voice);
        self.water.advance(dt, &self.heights, &rises);
        Edge::new(&self.water)
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

/// The pill's outline at one instant: how far each node of the water stands
/// out from rest, in pixels. The default is the pill at rest.
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
    /// `water` drawn with a full wave [`DEPTH`] pixels deep.
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

    /// The voice's effect cut to `share` of it, as the picture collapses.
    fn scaled(self, share: f32) -> Edge {
        Edge {
            offsets: self.offsets.map(|o| o * share),
            reach: self.reach * share,
            inward: self.inward * share,
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

/// Colours and border of the pill.
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
    /// The overlay's look at fill `opacity`: Tokyo Night charcoal in the
    /// dark theme (the default), the theme's background in the light one,
    /// and an accent border.
    /// The shadow fades with the fill; the border stays put.
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

    /// The soft colour the line, the dot and the ripple ease toward.
    fn ink(&self) -> Hsla {
        if self.dark {
            gpui_kit::rgb(INK_DARK).into()
        } else {
            let mut ink = self.accent.opacity(1.);
            ink.s *= INK_SATURATION;
            ink
        }
    }

    /// The ripple's colour: the dot's own on the dark theme, a mid tone
    /// that shows on dark and light backgrounds alike; the ink itself on
    /// the light theme, where the dot is too pale for a white background.
    fn ripple(&self) -> Hsla {
        if self.dark {
            mix(self.fill.opacity(1.), self.ink(), INK.0)
        } else {
            self.ink()
        }
    }

    /// The halo's colour: a muted accent.
    fn halo(&self) -> Hsla {
        let mut halo = if self.accent.s < GREY {
            gpui_kit::rgb(TOKYO_BLUE).into()
        } else {
            self.accent.opacity(1.)
        };
        halo.s *= HALO_SATURATION;
        halo
    }

    /// `self` as far switched off as `frame`: the pill's own fill and
    /// border, gently tinted toward the ink as it thins to the line, going
    /// with the dot.
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

/// Paint the shadow, body and border of a pill resting at `bounds`, its
/// edge as the voice moved it, drawn at `scale`.
pub fn paint(bounds: Bounds<Pixels>, edge: &Edge, look: Look, scale: f32, window: &mut Window) {
    let depth = (look.depth, look.depth);
    paint_shadow(bounds, edge.inward, look, depth, scale, window);
    if edge.reach() > 0. {
        paint_wave(bounds, edge, look, scale, window);
    } else {
        paint_body(bounds, look, scale, window);
    }
}

/// The pill at `bounds` switched off as far as `frame` (see [`crt`]): the
/// pill itself squeezing, its ripples and wide shadow shrinking with it
/// while the tight one stays under the line and the dot, then the ring the
/// closing dot releases.
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

/// A very faint halo around the line and the dot at `pill`, coming in with
/// their tint. Tucked inside, as the shadow is, so it meets the line.
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

/// The outline of a pill at `bounds` with its edge moved out by `edge`.
fn wave_outline(bounds: Bounds<Pixels>, edge: &Edge, scale: f32) -> Vec<Point<Pixels>> {
    outline(bounds, RADIUS * scale)
        .map(|(at, normal, node)| {
            let offset = edge.offset(node) * scale;
            point(px(at.x + normal.x * offset), px(at.y + normal.y * offset))
        })
        .collect()
}

/// A wide ambient shadow and a tight contact one, cast by the pill at rest,
/// at `depth` (ambient, contact) of their strength. Shadows are drawn only
/// outside the element they belong to; tucking that element a pixel inside
/// the body, or inside where the edge dips by `inward` pixels (and
/// spreading by as much), keeps the rim and any trough shaded, while a
/// translucent fill shows no shadow band beneath it.
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
        // Talking, then the loudest the voice can push: full bands jumping
        // in and out every other frame, then held.
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

    #[test]
    fn mix_ends_at_both_colours() {
        let (a, b) = (Hsla::black(), Hsla::white());
        assert_eq!(Rgba::from(mix(a, b, 0.)), Rgba::from(a));
        assert_eq!(Rgba::from(mix(a, b, 1.)), Rgba::from(b));
    }
}
