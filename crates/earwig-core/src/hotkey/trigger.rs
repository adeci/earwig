//! The hotkey the OS sources watch for, as written in `[hotkey] key`:
//! `+`-joined, case-insensitive, e.g. `RightCtrl`, `RightCmd`, `Ctrl+Space`,
//! `LeftAlt+Shift+F9`.
//!
//! Modifiers are `Ctrl`, `Alt` (`Option`), `Shift`, `Super` (`Cmd`,
//! `Command`, `Win`, `Meta`), each optionally `Left`/`Right`-prefixed, and
//! `Fn`. Keys are letters, digits, `Space`, `Tab`, `Escape`, `Return` and
//! `F1`–`F24`. Without a key the last modifier is the trigger itself
//! (modifier-only: `RightCtrl` alone, `Ctrl+RightAlt`).

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Super,
    Fn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
    Either,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// `A`–`Z`, stored upper-case.
    Letter(char),
    /// `0`–`9`.
    Digit(u8),
    Space,
    Tab,
    Escape,
    Return,
    /// `F1`–`F24`.
    F(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    /// In the order written; for a modifier-only trigger the last one is the
    /// key that is held.
    pub modifiers: Vec<(Modifier, Side)>,
    pub key: Option<Key>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("the hotkey is empty")]
    Empty,
    #[error("unknown key {0:?} in the hotkey")]
    Unknown(String),
    #[error("{0:?} must be the last part of the hotkey")]
    KeyNotLast(String),
    #[error("{0:?} appears twice in the hotkey")]
    Repeated(String),
}

impl Trigger {
    /// The modifier that is held as the trigger of a modifier-only hotkey.
    pub fn held_modifier(&self) -> Option<(Modifier, Side)> {
        if self.key.is_some() {
            None
        } else {
            self.modifiers.last().copied()
        }
    }
}

impl FromStr for Trigger {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Self, ParseError> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.iter().all(|part| part.is_empty()) {
            return Err(ParseError::Empty);
        }
        let mut modifiers: Vec<(Modifier, Side)> = Vec::new();
        let mut key = None;
        for (i, part) in parts.iter().enumerate() {
            let lower = part.to_ascii_lowercase();
            if let Some(modifier) = parse_modifier(&lower) {
                if key.is_some() {
                    return Err(ParseError::KeyNotLast(parts[i - 1].to_owned()));
                }
                if modifiers.iter().any(|(m, _)| *m == modifier.0) {
                    return Err(ParseError::Repeated((*part).to_owned()));
                }
                modifiers.push(modifier);
            } else if let Some(parsed) = parse_key(&lower) {
                if key.is_some() {
                    return Err(ParseError::KeyNotLast(parts[i - 1].to_owned()));
                }
                key = Some(parsed);
            } else {
                return Err(ParseError::Unknown((*part).to_owned()));
            }
        }
        Ok(Self { modifiers, key })
    }
}

fn parse_modifier(text: &str) -> Option<(Modifier, Side)> {
    let (side, name) = if let Some(rest) = text.strip_prefix("left") {
        (Side::Left, rest)
    } else if let Some(rest) = text.strip_prefix("right") {
        (Side::Right, rest)
    } else {
        (Side::Either, text)
    };
    let modifier = match name {
        "ctrl" | "control" => Modifier::Ctrl,
        "alt" | "option" => Modifier::Alt,
        "shift" => Modifier::Shift,
        "super" | "cmd" | "command" | "win" | "meta" => Modifier::Super,
        "fn" if side == Side::Either => Modifier::Fn,
        _ => return None,
    };
    Some((modifier, side))
}

fn parse_key(text: &str) -> Option<Key> {
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' => Some(Key::Letter(c.to_ascii_uppercase())),
            '0'..='9' => c
                .to_digit(10)
                .map(|d| Key::Digit(u8::try_from(d).unwrap_or(0))),
            _ => None,
        };
    }
    match text {
        "space" => Some(Key::Space),
        "tab" => Some(Key::Tab),
        "escape" | "esc" => Some(Key::Escape),
        "return" | "enter" => Some(Key::Return),
        _ => text
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=24).contains(n))
            .map(Key::F),
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        let mut sep = |f: &mut fmt::Formatter<'_>| {
            if !std::mem::take(&mut first) {
                f.write_str("+")?;
            }
            Ok::<(), fmt::Error>(())
        };
        for (modifier, side) in &self.modifiers {
            sep(f)?;
            f.write_str(match side {
                Side::Left => "Left",
                Side::Right => "Right",
                Side::Either => "",
            })?;
            f.write_str(match modifier {
                Modifier::Ctrl => "Ctrl",
                Modifier::Alt => "Alt",
                Modifier::Shift => "Shift",
                Modifier::Super => "Super",
                Modifier::Fn => "Fn",
            })?;
        }
        if let Some(key) = self.key {
            sep(f)?;
            match key {
                Key::Letter(c) => write!(f, "{c}")?,
                Key::Digit(d) => write!(f, "{d}")?,
                Key::Space => f.write_str("Space")?,
                Key::Tab => f.write_str("Tab")?,
                Key::Escape => f.write_str("Escape")?,
                Key::Return => f.write_str("Return")?,
                Key::F(n) => write!(f, "F{n}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Trigger, ParseError> {
        text.parse()
    }

    #[test]
    fn a_lone_modifier_is_held_as_the_trigger() {
        let trigger = parse("RightCtrl").unwrap();
        assert_eq!(trigger.key, None);
        assert_eq!(trigger.held_modifier(), Some((Modifier::Ctrl, Side::Right)));
    }

    #[test]
    fn names_are_case_insensitive_and_have_aliases() {
        assert_eq!(parse("rightcmd").unwrap(), parse("RIGHTSUPER").unwrap());
        assert_eq!(parse("Option+Space").unwrap(), parse("alt+space").unwrap());
        assert_eq!(parse("Win+Return").unwrap(), parse("meta + enter").unwrap());
    }

    #[test]
    fn a_chord_ends_in_its_key() {
        let trigger = parse("Ctrl+Shift+F9").unwrap();
        assert_eq!(
            trigger.modifiers,
            vec![
                (Modifier::Ctrl, Side::Either),
                (Modifier::Shift, Side::Either)
            ]
        );
        assert_eq!(trigger.key, Some(Key::F(9)));
        assert_eq!(trigger.held_modifier(), None);
        assert_eq!(parse("Alt+x").unwrap().key, Some(Key::Letter('X')));
        assert_eq!(parse("Ctrl+7").unwrap().key, Some(Key::Digit(7)));
    }

    #[test]
    fn display_round_trips() {
        for text in [
            "RightCtrl",
            "Ctrl+Space",
            "LeftAlt+Shift+F24",
            "Fn",
            "Super+A",
        ] {
            let trigger = parse(text).unwrap();
            assert_eq!(trigger.to_string(), text);
            assert_eq!(parse(&trigger.to_string()).unwrap(), trigger);
        }
    }

    #[test]
    fn malformed_hotkeys_are_rejected() {
        assert_eq!(parse(""), Err(ParseError::Empty));
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
}
