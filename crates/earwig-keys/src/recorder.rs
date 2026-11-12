//! Recording a hotkey: the user presses what they want.
//!
//! - A key with modifiers held is recorded as it goes down, the modifiers
//!   on either side: Left Ctrl and Space make `Ctrl+Space`.
//! - Modifiers alone are recorded when the first of them is let go, the
//!   last one pressed on its own side: Right Ctrl makes `RightCtrl`.
//! - Escape alone cancels.

use crate::hotkey::Hotkey;
use crate::key::{Held, Key, Modifier, Os, Side};
use crate::matcher::Edge;

/// What a recording has come to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Modifiers are held: as a window shows them, with a key yet to come
    /// (`Ctrl+…`, `⌃ …`); letting go records them alone.
    Holding(String),
    /// The user's hotkey. It may still fail [`Hotkey::check`].
    Done(Hotkey),
    /// Escape was pressed.
    Cancelled,
}

#[derive(Debug, Default)]
pub(crate) struct Recorder {
    /// Modifier keys pressed since the recording started, in order.
    pressed: Vec<Key>,
}

impl Recorder {
    /// One edge of `key` (`None`: a key earwig has no name for, which it
    /// skips); `held` is the modifier keys down besides it.
    pub(crate) fn edge(&mut self, key: Option<Key>, edge: Edge, held: Held) -> Option<Recorded> {
        let key = key?;
        match (key.modifier(), edge) {
            (_, Edge::Repeat) | (None, Edge::Up) => None,
            (Some(_), Edge::Down) => {
                if !self.pressed.contains(&key) {
                    self.pressed.push(key);
                }
                self.hotkey()
                    .map(|hotkey| Recorded::Holding(hotkey.label_held_on(Os::CURRENT)))
            }
            // Modifiers that were down before the recording started.
            (Some(_), Edge::Up) if !self.pressed.contains(&key) => None,
            (Some(_), Edge::Up) => self.hotkey().map(Recorded::Done),
            (None, Edge::Down) if key == Key::Escape && held.is_empty() => {
                Some(Recorded::Cancelled)
            }
            (None, Edge::Down) => {
                let modifiers = held
                    .keys()
                    .filter_map(Key::modifier)
                    // macOS sets Fn on arrow and function keys by itself.
                    .filter(|&(modifier, _)| modifier != Modifier::Fn)
                    .map(|(modifier, _)| (modifier, Side::Either));
                Some(Recorded::Done(Hotkey::new(modifiers, key)))
            }
        }
    }

    /// The modifiers pressed so far as a hotkey: the last one, with the
    /// others on either side.
    fn hotkey(&self) -> Option<Hotkey> {
        let (&trigger, others) = self.pressed.split_last()?;
        let modifiers = others
            .iter()
            .filter_map(|key| key.modifier())
            .map(|(modifier, _)| (modifier, Side::Either));
        Some(Hotkey::new(modifiers, trigger))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT_CTRL: Key = Key::Modifier(Modifier::Ctrl, Side::Left);
    const RIGHT_CTRL: Key = Key::Modifier(Modifier::Ctrl, Side::Right);
    const LEFT_SHIFT: Key = Key::Modifier(Modifier::Shift, Side::Left);
    const RIGHT_ALT: Key = Key::Modifier(Modifier::Alt, Side::Right);
    const LEFT_SUPER: Key = Key::Modifier(Modifier::Super, Side::Left);

    /// The recording's last answer to `edges`, keeping track of what is held.
    fn record(edges: &[(Key, Edge)]) -> Recorded {
        let mut recorder = Recorder::default();
        let mut held = Held::default();
        let mut last = None;
        for &(key, edge) in edges {
            let besides = held.without(key);
            if let Some(step) = recorder.edge(Some(key), edge, besides) {
                last = Some(step);
            }
            held = if edge == Edge::Up {
                besides
            } else {
                besides.with(key)
            };
        }
        last.expect("the recording answered")
    }

    fn done(text: &str) -> Recorded {
        Recorded::Done(text.parse().unwrap())
    }

    use Edge::{Down, Repeat, Up};

    #[test]
    fn a_key_with_modifiers_is_recorded_as_it_goes_down() {
        assert_eq!(
            record(&[(LEFT_SUPER, Down), (Key::Space, Down)]),
            done("Super+Space")
        );
        assert_eq!(
            record(&[(LEFT_CTRL, Down), (LEFT_SHIFT, Down), (Key::F(9), Down)]),
            done("Ctrl+Shift+F9")
        );
        assert_eq!(
            record(&[(Key::Letter('A'), Down)]),
            done("A"),
            "checked later"
        );
    }

    #[test]
    fn modifiers_alone_are_recorded_when_let_go_with_their_side() {
        assert_eq!(
            record(&[(RIGHT_CTRL, Down), (RIGHT_CTRL, Repeat), (RIGHT_CTRL, Up)]),
            done("RightCtrl")
        );
        assert_eq!(
            record(&[(LEFT_SHIFT, Down), (RIGHT_ALT, Down), (LEFT_SHIFT, Up)]),
            done("Shift+RightAlt")
        );
    }

    #[test]
    fn holding_modifiers_shows_them_waiting_for_a_key() {
        let mut recorder = Recorder::default();
        let shown = if cfg!(target_os = "macos") {
            "Right ⌃ …"
        } else {
            "Right Ctrl+…"
        };
        assert_eq!(
            recorder.edge(Some(RIGHT_CTRL), Down, Held::default()),
            Some(Recorded::Holding(shown.into()))
        );
    }

    #[test]
    fn escape_alone_cancels_but_escape_with_modifiers_is_a_hotkey() {
        assert_eq!(record(&[(Key::Escape, Down)]), Recorded::Cancelled);
        assert_eq!(
            record(&[(LEFT_CTRL, Down), (Key::Escape, Down)]),
            done("Ctrl+Escape")
        );
    }

    #[test]
    fn modifiers_held_from_before_are_not_recorded_alone() {
        let mut recorder = Recorder::default();
        assert_eq!(recorder.edge(Some(LEFT_CTRL), Up, Held::default()), None);
        assert_eq!(
            recorder.edge(None, Down, Held::default()),
            None,
            "an unnamed key"
        );
    }
}
