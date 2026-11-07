//! The pill powering on and switching off like an old CRT screen, in the
//! overlay's own calm palette. On: its first frame is a squashed bar at
//! full width that springs open while the text fades in. Off, once a
//! dictation is finished: the text fades, the pill squeezes to a thin line
//! across its middle and springs as it lands, the line shrinks to a dot in
//! the centre, and the dot closes like a droplet, releasing one faint
//! ripple. Pure curves of the time since the pill's first frame or the
//! finish; [`pill::paint_crt`](crate::ui::pill::paint_crt) draws them.

use std::time::Duration;

/// From the pill's first frame to it standing open.
pub const OPEN: Duration = Duration::from_millis(90);
/// The height it opens from, a share of its own...
const OPEN_FROM: f32 = 0.6;
/// ...on a spring (rad/s, damping ratio) that has nearly settled by then,
/// eased onto the full height over the last share of the opening.
const OPENING: (f32, f32) = (60., 0.75);
const SETTLE: f32 = 0.6;

/// From the finish to the window closing: a few frames past the ripple's
/// last trace at [`END`], so the window goes on an empty frame.
pub const DURATION: Duration = Duration::from_millis(400);
/// Seconds to the last trace of the ripple.
const END: f32 = 0.38;

/// Seconds the text and the × take to fade, gone before the pill they sit
/// on has squeezed past them.
const FADE: f32 = 0.05;
/// The squeeze is a spring from the whole pill to the line, from rest: its
/// natural frequency (rad/s) and damping ratio. Its overshoot thins the
/// line a moment before it settles.
const COLLAPSE: (f32, f32) = (32., 0.7);
/// How much thinner the overshoot makes the line, on a log scale of the
/// pill's height.
const SPRING_THIN: f32 = 1.5;

/// The power-on `t` seconds into the pill's first frame, for a pill `width`
/// by `height` pixels: the faint glow fades as it opens; `None` once open.
pub fn opening(t: f32, width: f32, height: f32) -> Option<Frame> {
    let open = OPEN.as_secs_f32();
    let t = t.max(0.);
    if t >= open {
        return None;
    }
    let spread = 1. - (1. - OPEN_FROM) * spring(t, OPENING);
    let share = spread + (1. - spread) * smoothstep(progress(t, SETTLE * open, open));
    Some(Frame {
        width,
        height: height * share,
        picture: share.min(1.),
        tint: 0.,
        glow: 1. - smoothstep(t / open),
        alpha: 1.,
        ripple: None,
    })
}

/// Opacity of the text and the × `t` seconds into powering on.
pub fn opening_content(t: f32) -> f32 {
    smoothstep(t / OPEN.as_secs_f32())
}
/// Thickness of the line and diameter of the dot, in pixels.
pub const LINE: f32 = 3.;
pub const DOT: f32 = LINE;
/// How much wider the pill grows as it flattens.
const STRETCH: f32 = 0.03;
/// Seconds since the finish over which the line shrinks to the dot.
const SHRINK: (f32, f32) = (0.13, 0.265);
/// The pill takes on the line's soft tint as it thins below this share of
/// its height.
const TINTS_FROM: f32 = 0.3;
/// The dot closes from the end of the shrink to this, like a droplet
/// drawn in.
const CLOSED: f32 = 0.315;
/// The ripple spreads from the whole dot as it starts to close, this wide
/// (pixels), this strong at most, that strong this soon (a share of its
/// life).
const RIPPLE_SIZE: f32 = 34.;
const RIPPLE_OPACITY: f32 = 0.55;
const RIPPLE_RISE: f32 = 0.15;

/// The pill `t` seconds into switching off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The pill's size in pixels, centred where it was.
    pub width: f32,
    pub height: f32,
    /// What is left of the pill as it was: 1 whole, 0 once it is a line.
    /// Its ripples, bars and wide shadow shrink with it.
    pub picture: f32,
    /// From the pill's own colours (0) to the line's soft tint (1).
    pub tint: f32,
    /// Strength of the faint halo around the pill, 0..1.
    pub glow: f32,
    /// Opacity of the pill, which goes as the dot closes.
    pub alpha: f32,
    /// The ripple the closing dot releases, once it has.
    pub ripple: Option<Ripple>,
}

/// A faint ring spreading from the centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ripple {
    pub diameter: f32,
    pub opacity: f32,
}

/// Opacity of the text and the × `t` seconds into switching off.
pub fn content(t: f32) -> f32 {
    1. - smoothstep(t / FADE)
}

/// The switch-off `t` seconds in for a pill `width` by `height` pixels;
/// `None` once nothing is left.
pub fn at(t: f32, width: f32, height: f32) -> Option<Frame> {
    let t = t.max(0.);
    if t >= END {
        return None;
    }
    let full = height.max(LINE);
    let squash = spring(t, COLLAPSE);
    let collapsed = if squash >= 0. {
        LINE + (full - LINE) * squash
    } else {
        LINE * (full / LINE).powf(SPRING_THIN * squash)
    };
    let picture = ((collapsed - LINE) / (full - LINE).max(f32::EPSILON)).clamp(0., 1.);

    let shrink = progress(t, SHRINK.0, SHRINK.1);
    let stretched = width * (1. + STRETCH * (1. - picture));
    let long = stretched + (DOT - stretched) * smoothstep(shrink);
    // Never taller than it is wide: the line keeps its thickness, and ends
    // as a dot as round as it is thick.
    let mut size = (long, collapsed.min(long));
    // The dot contracts evenly while it fades.
    let close = progress(t, SHRINK.1, CLOSED);
    if close > 0. {
        let diameter = DOT * (1. - 0.6 * close);
        size = (diameter, diameter);
    }
    let ripple = (t >= SHRINK.1).then(|| {
        let life = progress(t, SHRINK.1, END);
        Ripple {
            diameter: DOT + (RIPPLE_SIZE - DOT) * (1. - (1. - life).powf(2.5)),
            opacity: RIPPLE_OPACITY * smoothstep(life / RIPPLE_RISE) * (1. - life).powf(1.5),
        }
    });
    let tint = 1. - smoothstep(progress(collapsed, LINE, TINTS_FROM * full));
    Some(Frame {
        width: size.0,
        height: size.1,
        picture,
        tint,
        glow: tint,
        alpha: 1. - smoothstep(close),
        ripple,
    })
}

/// A spring from 1 toward 0, starting from rest, with natural frequency
/// `omega` (rad/s) and damping ratio `zeta`: under its overshoot it dips
/// below 0.
fn spring(t: f32, (omega, zeta): (f32, f32)) -> f32 {
    let damped = omega * (1. - zeta * zeta).sqrt();
    (-zeta * omega * t).exp() * ((damped * t).cos() + zeta * omega / damped * (damped * t).sin())
}

/// How far `x` is from `from` to `to`, within 0..1.
fn progress(x: f32, from: f32, to: f32) -> f32 {
    ((x - from) / (to - from)).clamp(0., 1.)
}

/// Eased in and out over 0..1.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pill with three lines of text.
    const PILL: (f32, f32) = (560., 132.);

    fn frame(t: f32) -> Frame {
        at(t, PILL.0, PILL.1).expect("still switching off")
    }

    /// Times from the finish to the end, every `step` seconds.
    #[allow(clippy::cast_precision_loss)]
    fn times(step: f32) -> impl Iterator<Item = f32> {
        (0..).map(move |k| k as f32 * step).take_while(|&t| t < END)
    }

    fn ripple(t: f32) -> Ripple {
        frame(t).ripple.unwrap_or(Ripple {
            diameter: DOT,
            opacity: 0.,
        })
    }

    #[test]
    fn starts_as_the_pill_and_ends_as_nothing() {
        let first = frame(0.);
        assert_eq!((first.width, first.height), PILL);
        assert_eq!(
            (content(0.), first.picture, first.alpha, first.tint),
            (1., 1., 1., 0.)
        );
        assert_eq!(first.ripple, None);
        assert!(at(END, PILL.0, PILL.1).is_none());
        let last = frame(END - 0.001);
        assert!(last.alpha < 0.01, "{last:?}");
        assert!(ripple(END - 0.001).opacity < 0.001, "{last:?}");
        assert!(DURATION.as_secs_f32() > END);
        assert!((0.3..=0.4).contains(&DURATION.as_secs_f32()));
    }

    #[test]
    fn nothing_jumps_from_one_millisecond_to_the_next() {
        // The line sweeps in at up to about 6 px/ms; a jump would be the
        // whole pill, or a whole step of colour, at once.
        let mut last = frame(0.);
        for (t, before) in times(0.001).skip(1).zip(times(0.001)) {
            let now = frame(t);
            let moved = (now.width - last.width)
                .abs()
                .max((now.height - last.height).abs())
                .max((ripple(t).diameter - ripple(before).diameter).abs());
            assert!(moved < 10., "{moved} px at {t}: {last:?} -> {now:?}");
            for (a, b) in [
                (content(before), content(t)),
                (last.tint, now.tint),
                (last.alpha, now.alpha),
                (last.picture, now.picture),
                (ripple(before).opacity, ripple(t).opacity),
            ] {
                assert!((a - b).abs() < 0.1, "at {t}: {last:?} -> {now:?}");
            }
            last = now;
        }
    }

    #[test]
    fn the_pill_squeezes_to_a_line_then_a_dot_that_closes() {
        // The text is gone before the pill squeezes past it.
        for t in times(0.001) {
            let f = frame(t);
            if f.height < 0.6 * PILL.1 {
                assert!(content(t) < 0.2, "{t}: {f:?}");
            }
        }
        let line = frame(SHRINK.0);
        assert!(
            line.height <= LINE + 0.5 && line.width >= PILL.0,
            "{line:?}"
        );
        assert!(line.tint > 0.99, "{line:?}");
        assert_eq!((line.picture, content(SHRINK.0)), (0., 0.));
        // From then on it only gets smaller: the line into the dot, the dot
        // to nothing.
        let ends: Vec<Frame> = times(0.001).filter(|&t| t >= SHRINK.0).map(frame).collect();
        assert!(ends.windows(2).all(|w| w[1].width <= w[0].width));
        // The whole tail: never taller than it is wide, no bead.
        for f in &ends {
            assert!(f.height <= f.width + 1e-3, "{f:?}");
        }
        let dot = frame(SHRINK.1);
        assert!((dot.width - DOT).abs() < 0.01 && (dot.height - DOT).abs() < 0.3);
        assert!(frame(CLOSED - 0.001).alpha < 0.01);
    }

    #[test]
    fn the_closing_dot_releases_one_faint_ripple_that_spreads_and_fades() {
        assert_eq!(frame(SHRINK.1 - 0.001).ripple, None);
        let rings: Vec<Ripple> = times(0.001)
            .filter(|&t| t >= SHRINK.1)
            .map(ripple)
            .collect();
        // From the whole dot, fading in.
        assert!(
            (rings[0].diameter - DOT).abs() < 0.01 && rings[0].opacity < 0.01,
            "{:?}",
            rings[0]
        );
        assert!(rings.windows(2).all(|w| w[1].diameter >= w[0].diameter));
        let strongest = rings.iter().map(|r| r.opacity).fold(0., f32::max);
        assert!((0.3..=RIPPLE_OPACITY).contains(&strongest), "{strongest}");
        let widest = rings.last().unwrap().diameter;
        assert!(widest > 3. * DOT && widest <= RIPPLE_SIZE, "{widest}");
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn powering_on_springs_a_squashed_pill_open_within_five_frames() {
        let open = OPEN.as_secs_f32();
        let first = opening(0., PILL.0, PILL.1).unwrap();
        // Already plainly the pill: full width, a little over half its
        // height, its own colours, no text yet, the faint glow on.
        assert_eq!((first.width, first.tint, first.glow), (PILL.0, 0., 1.));
        assert!(
            (0.55..=0.65).contains(&(first.height / PILL.1)),
            "{first:?}"
        );
        assert_eq!(opening_content(0.), 0.);
        assert!(opening(open, PILL.0, PILL.1).is_none());
        assert!(open <= 5.5 / 60.);
        let mut last = first;
        for k in 1..=90 {
            let t = k as f32 / 1000.;
            let Some(now) = opening(t, PILL.0, PILL.1) else {
                break;
            };
            // Springs a little past its height at most, and never jumps.
            assert!(now.height <= 1.02 * PILL.1, "{t}: {now:?}");
            assert!((now.height - last.height).abs() < 4., "{t}: {now:?}");
            assert!(now.glow <= last.glow, "{t}: {now:?}");
            last = now;
        }
        assert!(
            (last.height - PILL.1).abs() < 0.5 && last.glow < 0.01,
            "{last:?}"
        );
        assert!(opening_content(open - 0.001) > 0.99);
    }
}
