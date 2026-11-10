//! Where a drag of the overlay pill stands: held from the press on it until
//! the button comes up, another one goes down, or the window loses the
//! pointer. The pill's own window follows the pointer meanwhile, still
//! animated (see `overlay`).

use std::cell::Cell;

use gpui_kit::{MouseButton, Pixels, Point};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grab {
    /// The pointer's offset from the spot it grabbed.
    pub offset: Point<Pixels>,
    /// The pointer on the area at the press, in the window then at
    /// `origin`, and how far it had travelled by then (relative motions
    /// summed, where the compositor reports them).
    pub at: Point<Pixels>,
    pub local: Point<Pixels>,
    pub origin: Point<Pixels>,
    pub travel: Option<Point<Pixels>>,
}

/// What a compositor gives the pointer's position in a surface relative to
/// while that surface moves under a held button.
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

/// Pointer travel over which a drag tells the anchorings apart.
const TELLING: f64 = 24.;
/// How far from where it pressed the pointer may seem to be in the window,
/// read as the protocol has it, before that is taken as positions anchored
/// at the press: following the pointer, the window keeps it within one
/// motion event of there.
const ADRIFT: f64 = 16.;

impl Grab {
    /// The pointer on the area now, from its travel since the press where
    /// the compositor reports relative motion (it does not for absolute
    /// devices such as tablets), else from `local` in the window at
    /// `origin` as this compositor anchors it: as the protocol says until a
    /// drag has shown otherwise. `free`: the window follows the pointer
    /// rather than resting against an edge of the area.
    pub fn pointer(
        self,
        travel: Option<Point<Pixels>>,
        local: Point<Pixels>,
        origin: Point<Pixels>,
        free: bool,
    ) -> Point<Pixels> {
        if let (Some(then), Some(now)) = (self.travel, travel)
            && now != then
        {
            let pointer = self.at + (now - then);
            self.learn(pointer, local, origin);
            return pointer;
        }
        if ANCHORING.get().is_none() && free && (local - self.local).magnitude() > ADRIFT {
            learned(Anchoring::Press, "the pointer drifting in the window");
        }
        match ANCHORING.get() {
            Some(Anchoring::Press) => local + self.origin,
            _ => local + origin,
        }
    }

    /// Note which anchoring puts `local` where the travel says the pointer
    /// is (to within a quarter of the way, as relative motion may come an
    /// event late); the other one is off by as far as the window went.
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

/// Whether positions are read as relative to where the window is now, so
/// the window shifts those the compositor sent before it caught up with a
/// move (see `vendor/gpui-pre-linux/PATCH.md`).
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
    /// The pointer left the window. While a button is down the compositor
    /// keeps the pointer on the window it went down in, so this means the
    /// grab is gone (layer-shell only: X11 reports crossings during one).
    Exited,
}

impl Pointer {
    /// Whether the event ends a drag: the button came up (seen or not),
    /// another one went down, or the grab was lost.
    pub fn ends_drag(self) -> bool {
        match self {
            Pointer::Moved { pressed } => pressed != Some(MouseButton::Left),
            Pointer::Pressed(_) | Pointer::Exited => true,
            Pointer::Released(button) => button == MouseButton::Left,
        }
    }
}

#[derive(Debug, Default)]
pub struct Drag(Option<Grab>);

impl Drag {
    /// Take hold of the pill; refused while a drag lasts.
    pub fn hold(&mut self, grab: Grab) -> bool {
        if self.0.is_some() {
            return false;
        }
        self.0 = Some(grab);
        true
    }

    pub fn grab(&self) -> Option<Grab> {
        self.0
    }

    pub fn is_held(&self) -> bool {
        self.0.is_some()
    }

    /// Let go of the pill, where it is; whether it was held.
    pub fn let_go(&mut self) -> bool {
        self.0.take().is_some()
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

    /// Where the window ends up behind a pointer that goes `steps` 5 px to
    /// the right, relative to where it should be, on a compositor that
    /// anchors positions as `truth`, with the pointer's travel known or not.
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
        // Read as the protocol says, positions anchored at the press run
        // ahead; the drift gives it away within a few events.
        assert!(drive(Anchoring::Press, false, 40).magnitude() < 1e-3);
        assert_eq!(ANCHORING.get(), Some(Anchoring::Press));
    }

    #[test]
    fn a_drag_holds_and_lets_go_any_number_of_times() {
        let mut drag = Drag::default();
        for _ in 0..5 {
            assert!(drag.hold(grab(true)));
            assert_eq!(drag.grab(), Some(grab(true)));
            assert!(drag.let_go());
            assert!(!drag.is_held());
        }
    }

    #[test]
    fn one_drag_at_a_time() {
        let mut drag = Drag::default();
        assert!(drag.hold(grab(true)));
        assert!(!drag.hold(grab(false)));
        assert!(drag.let_go());
        assert!(!drag.let_go(), "nothing held");
    }

    #[test]
    fn what_ends_a_drag() {
        let left = MouseButton::Left;
        assert!(
            !Pointer::Moved {
                pressed: Some(left)
            }
            .ends_drag()
        );
        // The release went elsewhere: the next move says so.
        assert!(Pointer::Moved { pressed: None }.ends_drag());
        assert!(
            Pointer::Moved {
                pressed: Some(MouseButton::Right)
            }
            .ends_drag()
        );
        assert!(Pointer::Released(left).ends_drag());
        assert!(!Pointer::Released(MouseButton::Right).ends_drag());
        assert!(Pointer::Pressed(MouseButton::Right).ends_drag());
        assert!(Pointer::Pressed(left).ends_drag());
        assert!(Pointer::Exited.ends_drag());
    }
}
