//! A hotkey as settings files write it: `+`-joined, case-insensitive, modifiers first
//! (`Super+Space`, `Ctrl+Shift+F9`). With no other key the last modifier fires (`RightCtrl`).

use std::fmt;
use std::str::FromStr;

use crate::key::{Held, Key, Modifier, Os, Side};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    /// In [`Modifier`] order, one per modifier, never the trigger's own.
    modifiers: Vec<(Modifier, Side)>,
    /// Any key; for modifiers alone, the one held last.
    trigger: Key,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("the shortcut is empty")]
    Empty,
    #[error("there is no key {0:?}")]
    Unknown(String),
    #[error("{0:?} must be the last part of the shortcut")]
    KeyNotLast(String),
    #[error("{0:?} appears twice in the shortcut")]
    Repeated(String),
}

impl Hotkey {
    /// A modifier of the trigger's own family is dropped.
    pub(crate) fn new(modifiers: impl IntoIterator<Item = (Modifier, Side)>, trigger: Key) -> Self {
        let own = trigger.modifier().map(|(modifier, _)| modifier);
        let mut modifiers: Vec<(Modifier, Side)> = modifiers
            .into_iter()
            .filter(|&(modifier, _)| Some(modifier) != own)
            .collect();
        modifiers.sort_by_key(|&(modifier, _)| modifier);
        modifiers.dedup_by_key(|&mut (modifier, _)| modifier);
        Self { modifiers, trigger }
    }

    pub fn modifiers(&self) -> &[(Modifier, Side)] {
        &self.modifiers
    }

    pub fn trigger(&self) -> Key {
        self.trigger
    }

    /// Modifiers alone (`RightCtrl`) pass through to apps; another key while they're held makes
    /// them a shortcut instead ([`crate::Event::Chord`]).
    pub fn is_modifier_only(&self) -> bool {
        self.trigger.modifier().is_some()
    }

    /// As windows show it: `Right Ctrl`, `Ctrl+Space`; on a Mac `Right ⌘`, `⌥ Space`.
    pub fn label(&self) -> String {
        self.label_on(Os::CURRENT)
    }

    /// A Mac puts fn first and runs ⌃⌥⇧⌘ together like its menus: `⌃⌥⇧⌘ F9`, `fn ⌃ ←`.
    pub(crate) fn label_on(&self, os: Os) -> String {
        let mut modifiers: Vec<Key> = self
            .modifiers
            .iter()
            .map(|&(modifier, side)| Key::Modifier(modifier, side))
            .collect();
        if os == Os::Mac {
            // Stable, so the rest keep ⌃⌥⇧⌘ order.
            modifiers.sort_by_key(|&key| key != Key::Modifier(Modifier::Fn, Side::Either));
        }
        let mut label = String::new();
        let mut after_symbol = false;
        for key in modifiers.into_iter().chain([self.trigger]) {
            let symbol = os == Os::Mac
                && matches!(key, Key::Modifier(modifier, Side::Either) if modifier != Modifier::Fn);
            if !label.is_empty() {
                label.push_str(if os != Os::Mac {
                    "+"
                } else if after_symbol && symbol {
                    ""
                } else {
                    " "
                });
            }
            label.push_str(&key.label(os));
            after_symbol = symbol;
        }
        label
    }

    /// What a recording shows while only modifiers are held: `Ctrl+…`, `⌃ …`.
    pub(crate) fn label_held_on(&self, os: Os) -> String {
        let gap = if os == Os::Mac { ' ' } else { '+' };
        format!("{}{gap}…", self.label_on(os))
    }

    /// As settings files write it on `os`.
    pub(crate) fn name_on(&self, os: Os) -> String {
        self.modifiers
            .iter()
            .map(|&(modifier, side)| Key::Modifier(modifier, side).name(os))
            .chain([self.trigger.name(os)])
            .collect::<Vec<_>>()
            .join("+")
    }

    pub(crate) fn is_trigger(&self, key: Key) -> bool {
        match (self.trigger, key) {
            (Key::Modifier(wanted, Side::Either), Key::Modifier(modifier, _)) => wanted == modifier,
            (trigger, key) => trigger == key,
        }
    }

    /// Exactly the wanted modifiers are held. The trigger's own family doesn't count (Left Ctrl
    /// held doesn't block Right Ctrl), nor does Fn, which macOS sets on arrows and F-keys.
    pub(crate) fn modifiers_match(&self, held: Held) -> bool {
        let own = self.trigger.modifier().map(|(modifier, _)| modifier);
        self.modifiers
            .iter()
            .all(|&(modifier, side)| held.has(modifier, side))
            && Modifier::COMMON.into_iter().all(|modifier| {
                Some(modifier) == own
                    || self.modifiers.iter().any(|&(wanted, _)| wanted == modifier)
                    || !held.has(modifier, Side::Either)
            })
    }

    /// Alt or Super is in it: a lone release opens menus (see [`crate::matcher::Mask`]).
    pub(crate) fn needs_mask(&self) -> bool {
        self.modifiers
            .iter()
            .map(|&(modifier, _)| modifier)
            .chain(self.trigger.modifier().map(|(modifier, _)| modifier))
            .any(|modifier| matches!(modifier, Modifier::Alt | Modifier::Super))
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name_on(Os::CURRENT))
    }
}

impl FromStr for Hotkey {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Self, ParseError> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.iter().all(|part| part.is_empty()) {
            return Err(ParseError::Empty);
        }
        let mut modifiers: Vec<Key> = Vec::new();
        let mut key: Option<Key> = None;
        for (i, part) in parts.iter().enumerate() {
            let lower = part.to_ascii_lowercase();
            let parsed = Key::parse_modifier(&lower)
                .map(Ok)
                .or_else(|| Key::parse(&lower).map(Err))
                .ok_or_else(|| ParseError::Unknown((*part).to_owned()))?;
            if key.is_some() {
                return Err(ParseError::KeyNotLast(parts[i - 1].to_owned()));
            }
            match parsed {
                Ok(modifier) => {
                    let family = modifier.modifier().map(|(family, _)| family);
                    if modifiers
                        .iter()
                        .any(|m| m.modifier().map(|(f, _)| f) == family)
                    {
                        return Err(ParseError::Repeated((*part).to_owned()));
                    }
                    modifiers.push(modifier);
                }
                Err(other) => key = Some(other),
            }
        }
        let trigger = match key {
            Some(key) => key,
            None => modifiers.pop().ok_or(ParseError::Empty)?,
        };
        Ok(Self::new(
            modifiers.into_iter().filter_map(Key::modifier),
            trigger,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Hotkey, ParseError> {
        text.parse()
    }

    #[test]
    fn a_lone_modifier_is_the_trigger() {
        let hotkey = parse("RightCtrl").unwrap();
        assert!(hotkey.is_modifier_only());
        assert_eq!(hotkey.trigger(), Key::Modifier(Modifier::Ctrl, Side::Right));
        assert_eq!(hotkey.modifiers(), []);
        let hotkey = parse("Shift+RightAlt").unwrap();
        assert_eq!(hotkey.trigger(), Key::Modifier(Modifier::Alt, Side::Right));
        assert_eq!(hotkey.modifiers(), [(Modifier::Shift, Side::Either)]);
    }

    #[test]
    fn names_are_case_insensitive_and_have_aliases() {
        assert_eq!(parse("rightcmd").unwrap(), parse("RIGHTSUPER").unwrap());
        assert_eq!(parse("RightCommand").unwrap(), parse("rightcmd").unwrap());
        assert_eq!(parse("Option+Space").unwrap(), parse("alt+space").unwrap());
        assert_eq!(parse("Win+Return").unwrap(), parse("meta + enter").unwrap());
        assert_eq!(
            parse("Shift+Ctrl+A").unwrap(),
            parse("Ctrl+Shift+A").unwrap()
        );
    }

    #[test]
    fn each_os_writes_its_own_names() {
        let hotkey = parse("Alt+Space").unwrap();
        assert_eq!(hotkey.name_on(Os::Mac), "Option+Space");
        assert_eq!(hotkey.name_on(Os::Linux), "Alt+Space");
        let hotkey = parse("Super+Space").unwrap();
        assert_eq!(hotkey.name_on(Os::Linux), "Super+Space");
        assert_eq!(hotkey.name_on(Os::Windows), "Win+Space");
        assert_eq!(hotkey.name_on(Os::Mac), "Cmd+Space");
        assert_eq!(
            parse("RightCtrl").unwrap().label_on(Os::Windows),
            "Right Ctrl"
        );
        assert_eq!(
            parse("Super+Shift+PageDown").unwrap().label_on(Os::Linux),
            "Shift+Super+Page Down"
        );
    }

    #[test]
    fn a_mac_shows_its_symbols_in_its_own_order() {
        let shown = |text: &str| parse(text).unwrap().label_on(Os::Mac);
        assert_eq!(shown("Option+Space"), "⌥ Space");
        assert_eq!(shown("RightCmd"), "Right ⌘");
        assert_eq!(shown("Cmd+Shift+Option+Ctrl+A"), "⌃⌥⇧⌘ A");
        assert_eq!(shown("Cmd+Shift+Return"), "⇧⌘ ↩");
        assert_eq!(shown("Ctrl+Fn+Left"), "fn ⌃ ←");
        assert_eq!(shown("Shift+RightAlt"), "⇧ Right ⌥");
        assert_eq!(shown("Ctrl+Fn"), "⌃ fn", "the key held last stays last");
        assert_eq!(shown("Alt+Escape"), "⌥ ⎋");
        assert_eq!(shown("Ctrl+Alt+Tab"), "⌃⌥ ⇥");
        assert_eq!(shown("Cmd+Backspace"), "⌘ ⌫");
    }

    #[test]
    fn held_modifiers_wait_for_a_key() {
        let hotkey = parse("Ctrl+RightAlt").unwrap();
        assert_eq!(hotkey.label_held_on(Os::Linux), "Ctrl+Right Alt+…");
        assert_eq!(hotkey.label_held_on(Os::Mac), "⌃ Right ⌥ …");
        assert_eq!(parse("Alt").unwrap().label_held_on(Os::Mac), "⌥ …");
    }

    #[test]
    fn names_round_trip_on_every_os() {
        for text in [
            "RightCtrl",
            "Ctrl+Space",
            "LeftAlt+Shift+F24",
            "Fn",
            "Super+A",
            "Ctrl+Alt+PageDown",
            "Shift+RightAlt",
            "Numpad7",
            "Ctrl+Slash",
        ] {
            let hotkey = parse(text).unwrap();
            for os in [Os::Linux, Os::Mac, Os::Windows] {
                assert_eq!(
                    parse(&hotkey.name_on(os)).unwrap(),
                    hotkey,
                    "{text} on {os:?}"
                );
            }
        }
        assert_eq!(
            parse("ctrl+shift+f9").unwrap().name_on(Os::Linux),
            "Ctrl+Shift+F9"
        );
    }

    #[test]
    fn malformed_shortcuts_are_rejected() {
        assert_eq!(parse(""), Err(ParseError::Empty));
        assert_eq!(parse(" + "), Err(ParseError::Empty));
        assert_eq!(
            parse("Ctrl+Hyper"),
            Err(ParseError::Unknown("Hyper".into()))
        );
        assert_eq!(
            parse("Space+Ctrl"),
            Err(ParseError::KeyNotLast("Space".into()))
        );
        assert_eq!(parse("A+B"), Err(ParseError::KeyNotLast("A".into())));
        assert_eq!(
            parse("Ctrl+RightCtrl"),
            Err(ParseError::Repeated("RightCtrl".into()))
        );
        assert_eq!(parse("F25"), Err(ParseError::Unknown("F25".into())));
        assert_eq!(parse("RightFn"), Err(ParseError::Unknown("RightFn".into())));
    }

    #[test]
    fn exact_modifiers_match_and_the_triggers_own_other_side_does_not_count() {
        let left_ctrl = Key::Modifier(Modifier::Ctrl, Side::Left);
        let left_shift = Key::Modifier(Modifier::Shift, Side::Left);
        let held = Held::default();
        let ctrl_space = parse("Ctrl+Space").unwrap();
        assert!(!ctrl_space.modifiers_match(held));
        assert!(ctrl_space.modifiers_match(held.with(left_ctrl)));
        assert!(!ctrl_space.modifiers_match(held.with(left_ctrl).with(left_shift)));
        let right_ctrl = parse("RightCtrl").unwrap();
        assert!(right_ctrl.modifiers_match(held.with(left_ctrl)));
        assert!(!right_ctrl.modifiers_match(held.with(left_shift)));
        assert!(right_ctrl.modifiers_match(held.with(Key::Modifier(Modifier::Fn, Side::Either))));
    }
}
