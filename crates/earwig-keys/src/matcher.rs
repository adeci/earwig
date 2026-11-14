//! Turns key edges into [`Event`]s and says which edges to swallow, the same on every OS.
//! A key with modifiers fires when it goes down with exactly those held, and is swallowed;
//! modifiers alone pass through, and another key while they're held makes an [`Event::Chord`].

use crate::Event;
use crate::hotkey::Hotkey;
use crate::key::{Held, Key};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Down,
    Repeat,
    Up,
}

/// What a backend does with one key edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Action {
    pub event: Option<Event>,
    /// Keep the edge from other apps.
    pub swallow: bool,
    pub mask: Mask,
}

/// A lone Alt or Super release opens the menu bar, Start or a launcher, so after a swallowed key
/// (or with such a modifier alone) the backend taps a harmless key in between.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mask {
    #[default]
    None,
    /// Tap the mask key after this swallowed edge.
    After,
    /// The trigger's release was swallowed: tap the mask key, then send the release.
    Release,
}

#[derive(Debug)]
pub(crate) struct Matcher {
    hotkey: Hotkey,
    /// The trigger is down and [`Event::Pressed`] went out.
    active: bool,
    /// Another key went down while the lone modifiers were held.
    chorded: bool,
}

impl Matcher {
    pub(crate) fn new(hotkey: Hotkey) -> Self {
        Self {
            hotkey,
            active: false,
            chorded: false,
        }
    }

    pub(crate) fn hotkey(&self) -> &Hotkey {
        &self.hotkey
    }

    /// `key` is `None` for keys earwig can't name; `held` is the other modifiers down.
    pub(crate) fn edge(&mut self, key: Option<Key>, edge: Edge, held: Held) -> Action {
        if self.hotkey.is_modifier_only() {
            self.modifiers_alone(key, edge, held)
        } else {
            self.with_key(key, edge, held)
        }
    }

    fn modifiers_alone(&mut self, key: Option<Key>, edge: Edge, held: Held) -> Action {
        let trigger = key.is_some_and(|key| self.hotkey.is_trigger(key));
        match (edge, trigger) {
            (Edge::Down, true) if !self.active && self.hotkey.modifiers_match(held) => {
                self.active = true;
                self.chorded = false;
                event(Event::Pressed)
            }
            (Edge::Up, true) if self.active => {
                self.active = false;
                if self.chorded {
                    // The press was already taken back.
                    return Action::default();
                }
                let mut action = event(Event::Released);
                if self.hotkey.needs_mask() {
                    action.swallow = true;
                    action.mask = Mask::Release;
                }
                action
            }
            (Edge::Down | Edge::Repeat, false) if self.active && !self.chorded => {
                self.chorded = true;
                event(Event::Chord)
            }
            _ => Action::default(),
        }
    }

    fn with_key(&mut self, key: Option<Key>, edge: Edge, held: Held) -> Action {
        if key != Some(self.hotkey.trigger()) {
            return Action::default();
        }
        let swallow = Action {
            swallow: true,
            ..Action::default()
        };
        match (edge, self.active) {
            (Edge::Down, false) if self.hotkey.modifiers_match(held) => {
                self.active = true;
                Action {
                    event: Some(Event::Pressed),
                    swallow: true,
                    mask: if self.hotkey.needs_mask() {
                        Mask::After
                    } else {
                        Mask::None
                    },
                }
            }
            (Edge::Down | Edge::Repeat, true) => swallow,
            // The modifiers may go up first.
            (Edge::Up, true) => {
                self.active = false;
                Action {
                    event: Some(Event::Released),
                    ..swallow
                }
            }
            _ => Action::default(),
        }
    }
}

fn event(event: Event) -> Action {
    Action {
        event: Some(event),
        ..Action::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{Modifier, Side};

    const RIGHT_CTRL: Key = Key::Modifier(Modifier::Ctrl, Side::Right);
    const LEFT_CTRL: Key = Key::Modifier(Modifier::Ctrl, Side::Left);
    const LEFT_SHIFT: Key = Key::Modifier(Modifier::Shift, Side::Left);
    const RIGHT_ALT: Key = Key::Modifier(Modifier::Alt, Side::Right);
    const LEFT_SUPER: Key = Key::Modifier(Modifier::Super, Side::Left);

    /// Feeds edges like a keyboard, tracking what's held.
    struct Keyboard {
        matcher: Matcher,
        held: Held,
    }

    impl Keyboard {
        fn new(hotkey: &str) -> Self {
            Self {
                matcher: Matcher::new(hotkey.parse().unwrap()),
                held: Held::default(),
            }
        }

        fn edge(&mut self, key: Key, edge: Edge) -> Action {
            let held = self.held.without(key);
            let action = self.matcher.edge(Some(key), edge, held);
            self.held = if edge == Edge::Up {
                held
            } else {
                held.with(key)
            };
            action
        }

        fn down(&mut self, key: Key) -> Action {
            self.edge(key, Edge::Down)
        }

        fn up(&mut self, key: Key) -> Action {
            self.edge(key, Edge::Up)
        }

        fn events(&mut self, edges: &[(Key, Edge)]) -> Vec<Event> {
            edges
                .iter()
                .filter_map(|&(key, edge)| self.edge(key, edge).event)
                .collect()
        }
    }

    use Edge::{Down, Repeat, Up};

    #[test]
    fn a_lone_modifier_presses_releases_and_passes_through() {
        let mut keyboard = Keyboard::new("RightCtrl");
        let press = keyboard.down(RIGHT_CTRL);
        assert_eq!(press.event, Some(Event::Pressed));
        assert!(!press.swallow);
        assert_eq!(keyboard.edge(RIGHT_CTRL, Repeat), Action::default());
        assert_eq!(
            keyboard.edge(RIGHT_CTRL, Down),
            Action::default(),
            "repeat as a down"
        );
        let release = keyboard.up(RIGHT_CTRL);
        assert_eq!(release.event, Some(Event::Released));
        assert!(!release.swallow);
    }

    #[test]
    fn the_other_side_is_not_the_trigger_but_either_side_is_both() {
        let mut keyboard = Keyboard::new("RightCtrl");
        assert_eq!(keyboard.events(&[(LEFT_CTRL, Down), (LEFT_CTRL, Up)]), []);
        let mut keyboard = Keyboard::new("Ctrl");
        assert_eq!(
            keyboard.events(&[(LEFT_CTRL, Down), (LEFT_CTRL, Up), (RIGHT_CTRL, Down)]),
            [Event::Pressed, Event::Released, Event::Pressed]
        );
    }

    #[test]
    fn a_key_during_the_hold_makes_it_a_shortcut() {
        let mut keyboard = Keyboard::new("RightCtrl");
        let c = Key::Letter('C');
        let events = keyboard.events(&[
            (RIGHT_CTRL, Down),
            (c, Down),
            (c, Up),
            (Key::Letter('A'), Down),
            (RIGHT_CTRL, Up),
        ]);
        assert_eq!(events, [Event::Pressed, Event::Chord]);
        // A key earwig has no name for counts too.
        keyboard.down(RIGHT_CTRL);
        assert_eq!(
            keyboard
                .matcher
                .edge(None, Down, Held::default().with(RIGHT_CTRL))
                .event,
            Some(Event::Chord)
        );
    }

    #[test]
    fn another_modifier_going_down_is_a_chord_but_one_going_up_is_not() {
        let mut keyboard = Keyboard::new("RightCtrl");
        assert_eq!(
            keyboard.events(&[(LEFT_SHIFT, Down), (RIGHT_CTRL, Down)]),
            [],
            "Shift held"
        );
        let mut keyboard = Keyboard::new("RightCtrl");
        keyboard.down(LEFT_SHIFT);
        keyboard.up(LEFT_SHIFT);
        assert_eq!(
            keyboard.events(&[
                (RIGHT_CTRL, Down),
                (LEFT_SHIFT, Down),
                (LEFT_SHIFT, Up),
                (RIGHT_CTRL, Up)
            ]),
            [Event::Pressed, Event::Chord]
        );
    }

    #[test]
    fn a_lone_modifier_needs_its_companions_and_no_others() {
        let mut keyboard = Keyboard::new("Shift+RightAlt");
        assert_eq!(keyboard.events(&[(RIGHT_ALT, Down), (RIGHT_ALT, Up)]), []);
        assert_eq!(
            keyboard.events(&[(LEFT_SHIFT, Down), (RIGHT_ALT, Down)]),
            [Event::Pressed]
        );
        // Ctrl on the left does not hold off Right Ctrl.
        let mut keyboard = Keyboard::new("RightCtrl");
        assert_eq!(
            keyboard.events(&[(LEFT_CTRL, Down), (RIGHT_CTRL, Down)]),
            [Event::Pressed]
        );
    }

    #[test]
    fn releasing_a_lone_alt_or_super_is_masked_unless_it_was_a_shortcut() {
        let mut keyboard = Keyboard::new("RightAlt");
        keyboard.down(RIGHT_ALT);
        let release = keyboard.up(RIGHT_ALT);
        assert_eq!(release.event, Some(Event::Released));
        assert!(release.swallow);
        assert_eq!(release.mask, Mask::Release);
        keyboard.events(&[
            (RIGHT_ALT, Down),
            (Key::Letter('A'), Down),
            (Key::Letter('A'), Up),
        ]);
        assert_eq!(keyboard.up(RIGHT_ALT), Action::default());
    }

    #[test]
    fn a_key_with_modifiers_swallows_itself_and_its_repeats() {
        let mut keyboard = Keyboard::new("Ctrl+Space");
        assert_eq!(keyboard.down(Key::Space), Action::default(), "no Ctrl");
        keyboard.up(Key::Space);
        keyboard.down(LEFT_CTRL);
        let press = keyboard.down(Key::Space);
        assert_eq!(press.event, Some(Event::Pressed));
        assert!(press.swallow);
        assert_eq!(press.mask, Mask::None);
        let repeat = keyboard.edge(Key::Space, Repeat);
        assert_eq!(repeat.event, None);
        assert!(repeat.swallow);
        // Letting go of Ctrl first still ends on the key's release.
        keyboard.up(LEFT_CTRL);
        let release = keyboard.up(Key::Space);
        assert_eq!(release.event, Some(Event::Released));
        assert!(release.swallow);
        assert_eq!(keyboard.down(Key::Space), Action::default(), "Ctrl is up");
    }

    #[test]
    fn a_key_wants_exactly_its_modifiers() {
        let mut keyboard = Keyboard::new("Ctrl+Space");
        assert_eq!(
            keyboard.events(&[(LEFT_CTRL, Down), (LEFT_SHIFT, Down), (Key::Space, Down)]),
            []
        );
        let mut keyboard = Keyboard::new("Super+Space");
        keyboard.down(LEFT_SUPER);
        let press = keyboard.down(Key::Space);
        assert_eq!(press.event, Some(Event::Pressed));
        assert_eq!(
            press.mask,
            Mask::After,
            "Super is released after a swallowed key"
        );
    }

    #[test]
    fn other_keys_pass_a_key_hotkey_untouched() {
        let mut keyboard = Keyboard::new("Super+Space");
        keyboard.down(LEFT_SUPER);
        assert_eq!(keyboard.down(Key::Letter('E')), Action::default());
        assert_eq!(keyboard.up(LEFT_SUPER), Action::default());
    }
}
