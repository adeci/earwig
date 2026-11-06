//! Where a drag of the overlay pill stands, from the press on it to the
//! moment its window shows it again at the drop spot.
//!
//! On layer-shell the pill moves on a separate surface while it is held
//! (see `overlay.rs`), so two surfaces take turns showing it: the drag
//! surface covers for the resting window once it has drawn the pill, and
//! after the drop the resting window (reopened at the drop spot) takes it
//! back before the drag surface closes.

use gpui_kit::{MouseButton, Pixels, Point};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grab {
    /// The pointer's offset from the spot it grabbed.
    pub offset: Point<Pixels>,
    /// That spot.
    pub from: Point<Pixels>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum State {
    #[default]
    Still,
    Held(Grab),
    /// Let go; numbered so that a late signal never ends a later landing.
    Landing(u32),
}

#[derive(Debug, Default)]
pub struct Drag {
    state: State,
    landings: u32,
    /// The drag surface has shown the held pill, so its window leaves it out.
    covered: bool,
}

impl Drag {
    /// Take hold of the pill; refused while a drag lasts or lands.
    pub fn hold(&mut self, grab: Grab) -> bool {
        if self.state != State::Still {
            return false;
        }
        self.state = State::Held(grab);
        true
    }

    pub fn grab(&self) -> Option<Grab> {
        match self.state {
            State::Held(grab) => Some(grab),
            _ => None,
        }
    }

    pub fn is_held(&self) -> bool {
        self.grab().is_some()
    }

    /// The drag surface shows the held pill (layer-shell only).
    #[cfg(any(target_os = "linux", test))]
    pub fn cover(&mut self) {
        self.covered = self.is_held();
    }

    /// Whether the resting window leaves the pill to the drag surface.
    pub fn is_covered(&self) -> bool {
        self.covered
    }

    /// Let go of the pill: its grab and the landing that brings it back to
    /// its window. From now on that window shows it.
    pub fn let_go(&mut self) -> Option<(Grab, u32)> {
        let grab = self.grab()?;
        self.landings = self.landings.wrapping_add(1);
        self.state = State::Landing(self.landings);
        self.covered = false;
        Some((grab, self.landings))
    }

    pub fn landing(&self) -> Option<u32> {
        match self.state {
            State::Landing(landing) => Some(landing),
            _ => None,
        }
    }

    /// The pill is back in its window after `landing`; whether that ended
    /// it (rather than having ended already).
    pub fn land(&mut self, landing: u32) -> bool {
        if self.state != State::Landing(landing) {
            return false;
        }
        self.state = State::Still;
        true
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, px};

    use super::*;

    fn grab() -> Grab {
        Grab {
            offset: point(px(10.), px(-20.)),
            from: point(px(400.), px(1000.)),
        }
    }

    #[test]
    fn a_drag_holds_covers_lets_go_and_lands() {
        let mut drag = Drag::default();
        assert!(drag.hold(grab()));
        assert_eq!(drag.grab(), Some(grab()));
        assert!(
            !drag.is_covered(),
            "the resting window shows the pill until the surface does"
        );
        drag.cover();
        assert!(drag.is_covered());
        let (released, landing) = drag.let_go().unwrap();
        assert_eq!(released, grab());
        assert!(
            !drag.is_covered(),
            "the window at the drop spot shows the pill"
        );
        assert!(!drag.is_held());
        assert_eq!(drag.landing(), Some(landing));
        assert!(drag.land(landing));
        assert_eq!(drag.landing(), None);
        assert!(drag.hold(grab()), "and the pill can be taken again");
    }

    #[test]
    fn one_drag_at_a_time() {
        let mut drag = Drag::default();
        assert!(drag.hold(grab()));
        assert!(!drag.hold(grab()));
        let (_, landing) = drag.let_go().unwrap();
        assert!(!drag.hold(grab()), "not while landing");
        assert!(drag.let_go().is_none(), "nothing held");
        assert!(drag.land(landing));
        assert!(!drag.land(landing), "landed once");
    }

    #[test]
    fn a_late_signal_ends_no_later_landing_and_covers_nothing() {
        let mut drag = Drag::default();
        drag.hold(grab());
        let (_, first) = drag.let_go().unwrap();
        // The drag surface's first frame came after the drop.
        drag.cover();
        assert!(!drag.is_covered());
        assert!(drag.land(first));
        drag.hold(grab());
        let (_, second) = drag.let_go().unwrap();
        assert_ne!(first, second);
        assert!(!drag.land(first), "the first landing's fallback fires late");
        assert_eq!(drag.landing(), Some(second));
        assert!(drag.land(second));
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
