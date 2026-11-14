//! Dragging the pill: where the pointer is while its window moves under it.

use std::cell::Cell;

use gpui_kit::{MouseButton, Pixels, Point};

/// Where things stood at the press.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grab {
    /// Pointer minus the pill's spot.
    pub offset: Point<Pixels>,
    /// The pointer on the area, and in the window then at `origin`.
    pub at: Point<Pixels>,
    pub local: Point<Pixels>,
    pub origin: Point<Pixels>,
    /// Summed relative motion, where the compositor reports it.
    pub travel: Option<Point<Pixels>>,
}

/// What pointer positions are relative to while the surface under a held button moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchoring {
    /// Where the surface was at the press (sway, niri).
    Press,
    /// Where it is now, as the protocol has it.
    Current,
}

thread_local! {
    /// This compositor's anchoring, once a drag has shown it.
    static ANCHORING: Cell<Option<Anchoring>> = const { Cell::new(None) };
}

/// Travel needed before a drag can tell the anchorings apart.
const TELLING: f64 = 24.;
/// The window keeps the pointer near where it pressed; drifting further means press anchoring.
const ADRIFT: f64 = 16.;

impl Grab {
    /// The pointer on the area: by relative travel if any (not tablets), else `local` as anchored.
    pub fn pointer(
        self,
        travel: Option<Point<Pixels>>,
        local: Point<Pixels>,
        origin: Point<Pixels>,
        unclamped: bool,
    ) -> Point<Pixels> {
        if let (Some(then), Some(now)) = (self.travel, travel)
            && now != then
        {
            let pointer = self.at + (now - then);
            self.learn(pointer, local, origin);
            return pointer;
        }
        if ANCHORING.get().is_none() && unclamped && (local - self.local).magnitude() > ADRIFT {
            learned(Anchoring::Press, "the pointer drifting in the window");
        }
        match ANCHORING.get() {
            Some(Anchoring::Press) => local + self.origin,
            _ => local + origin,
        }
    }

    /// Learns the anchoring that puts `local` where the travel says, give or take a late event.
    fn learn(self, pointer: Point<Pixels>, local: Point<Pixels>, origin: Point<Pixels>) {
        let travel = (pointer - self.at).magnitude();
        if ANCHORING.get().is_some()
            || travel < TELLING
            || (origin - self.origin).magnitude() < TELLING
        {
            return;
        }
        let near = |guess: Point<Pixels>| (guess - pointer).magnitude() < travel / 4.;
        match (near(local + self.origin), near(local + origin)) {
            (true, false) => learned(Anchoring::Press, "relative motion"),
            (false, true) => learned(Anchoring::Current, "relative motion"),
            _ => {}
        }
    }
}

fn learned(anchoring: Anchoring, by: &str) {
    log::debug!(
        "the compositor anchors pointer positions in a moving surface at {anchoring:?} (seen by {by})"
    );
    ANCHORING.set(Some(anchoring));
}

/// Read positions relative to where the window is now (see vendor/gpui-pre-linux/PATCH.md).
#[cfg(target_os = "linux")]
pub fn reads_current() -> bool {
    ANCHORING.get() != Some(Anchoring::Press)
}

/// A pointer event in the window holding the pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    Moved {
        pressed: Option<MouseButton>,
    },
    Pressed(MouseButton),
    Released(MouseButton),
    /// With a button down, the grab is gone (layer-shell only; X11 reports mid-grab crossings).
    Exited,
}

impl Pointer {
    /// The button came up (seen or not), another went down, or the grab was lost.
    pub fn ends_drag(self) -> bool {
        match self {
            Pointer::Moved { pressed } => pressed != Some(MouseButton::Left),
            Pointer::Pressed(_) | Pointer::Exited => true,
            Pointer::Released(button) => button == MouseButton::Left,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, px};

    use super::*;

    fn origin() -> Point<Pixels> {
        point(px(640.), px(860.))
    }

    fn grab(travel: bool) -> Grab {
        Grab {
            offset: point(px(10.), px(-20.)),
            at: origin() + point(px(300.), px(100.)),
            local: point(px(300.), px(100.)),
            origin: origin(),
            travel: travel.then(|| point(px(-30.), px(7.))),
        }
    }

    /// How far off the window ends up after `steps` 5 px moves right, anchored as `truth`.
    fn drive(truth: Anchoring, travel: bool, steps: usize) -> Point<Pixels> {
        let grab = grab(travel);
        let mut origin = self::origin();
        for step in 1..=steps {
            #[allow(clippy::cast_precision_loss)]
            let moved = point(px(5. * step as f32), px(0.));
            let local = grab.at + moved
                - match truth {
                    Anchoring::Press => self::origin(),
                    Anchoring::Current => origin,
                };
            let now = grab.travel.map(|then| then + moved);
            origin = grab.pointer(now, local, origin, true) - grab.local;
        }
        #[allow(clippy::cast_precision_loss)]
        let want = self::origin() + point(px(5. * steps as f32), px(0.));
        origin - want
    }

    #[test]
    fn the_window_follows_the_pointer_by_its_travel_and_learns_the_anchoring() {
        for truth in [Anchoring::Press, Anchoring::Current] {
            ANCHORING.set(None);
            for steps in [1, 3, 40] {
                let off = drive(truth, true, steps);
                assert!(off.magnitude() < 1e-3, "{truth:?} {steps}: {off:?}");
            }
            assert_eq!(ANCHORING.get(), Some(truth));
            // A tablet's drag (no relative motion) then follows too.
            let off = drive(truth, false, 40);
            assert!(off.magnitude() < 1e-3, "{truth:?} without travel: {off:?}");
        }
    }

    #[test]
    fn without_relative_motion_the_drag_tells_the_anchoring_itself() {
        ANCHORING.set(None);
        assert!(drive(Anchoring::Current, false, 40).magnitude() < 1e-3);
        assert_eq!(ANCHORING.get(), None, "as the protocol says");
        // Read as the protocol says, press-anchored positions run ahead; the drift shows it.
        assert!(drive(Anchoring::Press, false, 40).magnitude() < 1e-3);
        assert_eq!(ANCHORING.get(), Some(Anchoring::Press));
    }
}
