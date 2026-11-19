//! Turns key edges into [`Event`]s and says which edges to swallow, the same on every OS.
//! A key with modifiers fires when it goes down with exactly those held, and is swallowed;
//! modifiers alone pass through, and another key while they're held makes an [`Event::Chord`].
//! The discard shortcut is matched the same way, but only while a dictation is in progress; a
//! modifier added to the held hotkey (Shift to `Super+Space`) can make it too.

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
    /// The discard shortcut's own matcher, which gets the edges the hotkey leaves.
    discard: Option<Box<Self>>,
    /// The trigger is down and [`Event::Pressed`] went out.
    active: bool,
    /// Another key went down while the lone modifiers were held.
    chorded: bool,
}

impl Matcher {
    pub(crate) fn new(hotkey: Hotkey, discard: Option<Hotkey>) -> Self {
        Self {
            hotkey,
            discard: discard.map(|discard| Box::new(Self::new(discard, None))),
            active: false,
            chorded: false,
        }
    }

    /// The hotkey and the discard shortcut.
    pub(crate) fn hotkeys(&self) -> (&Hotkey, Option<&Hotkey>) {
        let discard = self.discard.as_ref().map(|discard| &discard.hotkey);
        (&self.hotkey, discard)
    }

    /// `key` is `None` for keys earwig can't name; `held` is the other modifiers down. The discard
    /// shortcut counts while `dictating`.
    pub(crate) fn edge(
        &mut self,
        key: Option<Key>,
        edge: Edge,
        held: Held,
        dictating: bool,
    ) -> Action {
        let action = if self.hotkey.is_modifier_only() {
            self.modifiers_alone(key, edge, held)
        } else {
            self.with_key(key, edge, held)
        };
        let Some(discard) = self
            .discard
            .as_mut()
            .filter(|_| action == Action::default())
        else {
            return action;
        };
        let added = key.filter(|key| key.modifier().is_some() && edge == Edge::Down);
        if let Some(added) = added
            && self.active
            && !self.hotkey.is_modifier_only()
            && discard.hotkey.trigger() == self.hotkey.trigger()
            && discard.hotkey.modifiers_match(held.with(added))
        {
            return event(Event::Discard);
        }
        if !dictating && !discard.active {
            return action;
        }
        let action = discard.edge(key, edge, held, false);
        Action {
            event: (action.event == Some(Event::Pressed)).then_some(Event::Discard),
            ..action
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
        dictating: bool,
    }

    impl Keyboard {
        fn new(hotkey: &str) -> Self {
            Self {
                matcher: Matcher::new(hotkey.parse().unwrap(), None),
                held: Held::default(),
                dictating: false,
            }
        }

        fn discarding(hotkey: &str, discard: &str) -> Self {
            Self {
                matcher: Matcher::new(hotkey.parse().unwrap(), Some(discard.parse().unwrap())),
                ..Self::new(hotkey)
            }
        }

        fn edge(&mut self, key: Key, edge: Edge) -> Action {
            let held = self.held.without(key);
            let action = self.matcher.edge(Some(key), edge, held, self.dictating);
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
                .edge(None, Down, Held::default().with(RIGHT_CTRL), false)
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

    const SWALLOW: Action = Action {
        event: None,
        swallow: true,
        mask: Mask::None,
    };

    #[test]
    fn shift_added_to_the_held_hotkey_discards_and_its_release_ends_the_hold() {
        for shift_first in [true, false] {
            let mut keyboard = Keyboard::discarding("Super+Space", "Super+Shift+Space");
            let hold = [(LEFT_SUPER, Down), (Key::Space, Down)];
            assert_eq!(keyboard.events(&hold), [Event::Pressed]);
            assert_eq!(
                keyboard.down(LEFT_SHIFT),
                event(Event::Discard),
                "Shift reaches apps"
            );
            if shift_first {
                assert_eq!(keyboard.up(LEFT_SHIFT), Action::default());
            }
            let release = keyboard.up(Key::Space);
            assert_eq!(
                (release.event, release.swallow),
                (Some(Event::Released), true)
            );
            if !shift_first {
                assert_eq!(keyboard.up(LEFT_SHIFT), Action::default());
            }
            assert_eq!(keyboard.up(LEFT_SUPER), Action::default());
        }
    }

    #[test]
    fn the_discard_shortcut_while_dictating_is_kept_from_apps_until_its_release() {
        let mut keyboard = Keyboard::discarding("Super+Space", "Super+Shift+Space");
        let tap = [(LEFT_SUPER, Down), (Key::Space, Down), (Key::Space, Up)];
        assert_eq!(keyboard.events(&tap), [Event::Pressed, Event::Released]);
        keyboard.dictating = true;
        keyboard.down(LEFT_SHIFT);
        let discard = keyboard.down(Key::Space);
        assert_eq!(
            discard,
            Action {
                event: Some(Event::Discard),
                swallow: true,
                mask: Mask::After
            }
        );
        // The dictation is gone; its modifiers go up first.
        keyboard.dictating = false;
        assert_eq!(keyboard.edge(Key::Space, Repeat), SWALLOW);
        assert_eq!(keyboard.events(&[(LEFT_SUPER, Up), (LEFT_SHIFT, Up)]), []);
        assert_eq!(keyboard.up(Key::Space), SWALLOW);
        let again = [(LEFT_SUPER, Down), (Key::Space, Down)];
        assert_eq!(keyboard.events(&again), [Event::Pressed]);
    }

    #[test]
    fn the_discard_shortcut_reaches_apps_while_no_dictation_is_in_progress() {
        let mut keyboard = Keyboard::discarding("Super+Space", "Super+Shift+Space");
        assert_eq!(
            keyboard.events(&[(LEFT_SUPER, Down), (LEFT_SHIFT, Down)]),
            []
        );
        assert_eq!(keyboard.down(Key::Space), Action::default());
        assert_eq!(keyboard.edge(Key::Space, Repeat), Action::default());
        assert_eq!(keyboard.up(Key::Space), Action::default());
    }

    #[test]
    fn a_hotkey_with_shift_works_as_ever_beside_its_discard_shortcut() {
        let mut keyboard = Keyboard::discarding("Ctrl+Shift+Space", "Ctrl+Space");
        let hold = [
            (LEFT_CTRL, Down),
            (LEFT_SHIFT, Down),
            (Key::Space, Down),
            (LEFT_SHIFT, Up),
            (Key::Space, Up),
        ];
        assert_eq!(keyboard.events(&hold), [Event::Pressed, Event::Released]);
        keyboard.dictating = true;
        let discard = [(Key::Space, Down), (Key::Space, Up)];
        assert_eq!(keyboard.events(&discard), [Event::Discard]);
    }

    #[test]
    fn modifiers_alone_discard_with_shift_first_and_make_a_chord_with_shift_after() {
        let mut keyboard = Keyboard::discarding("RightCtrl", "Shift+RightCtrl");
        keyboard.dictating = true;
        let shift_first = [
            (LEFT_SHIFT, Down),
            (RIGHT_CTRL, Down),
            (RIGHT_CTRL, Up),
            (LEFT_SHIFT, Up),
        ];
        assert_eq!(keyboard.events(&shift_first), [Event::Discard]);
        let shift_after = [
            (RIGHT_CTRL, Down),
            (LEFT_SHIFT, Down),
            (LEFT_SHIFT, Up),
            (RIGHT_CTRL, Up),
        ];
        assert_eq!(
            keyboard.events(&shift_after),
            [Event::Pressed, Event::Chord]
        );
    }
}
