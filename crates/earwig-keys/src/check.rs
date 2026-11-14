//! Is a hotkey usable? Refuse keys that type or that the system already uses, warn about ones
//! that get in the way now and then, and say why in one line.

use crate::hotkey::Hotkey;
use crate::key::{Key, Modifier, Os, Side};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    Fine,
    /// Usable, but it gets in the way now and then.
    Warn(String),
    /// Unusable or harmful; earwig does not watch it.
    Reject(String),
}

/// Lower-case `XDG_CURRENT_DESKTOP`s where a lone Super opens the overview or a launcher.
const LAUNCHER_DESKTOPS: [&str; 9] = [
    "gnome", "kde", "cosmic", "pantheon", "budgie", "cinnamon", "unity", "deepin", "ukui",
];

impl Hotkey {
    /// `desktop` is `XDG_CURRENT_DESKTOP`, which decides what a lone Super does on Linux.
    pub fn check(&self, desktop: &str) -> Check {
        self.check_on(Os::CURRENT, desktop)
    }

    pub(crate) fn check_on(&self, os: Os, desktop: &str) -> Check {
        let label = self.label_on(os);
        if let Some(problem) = missing_key(self, os) {
            return Check::Reject(problem);
        }
        if self.trigger() == Key::CapsLock {
            return Check::Reject("Caps Lock turns capitals on and off; pick another key.".into());
        }
        if self.is_modifier_only() {
            return modifiers_alone(self, os, desktop);
        }
        if let Some(reason) = typing(self, os, &label) {
            return Check::Reject(reason);
        }
        if let Some(reason) = reserved(self, os) {
            return Check::Reject(format!("{label} {reason}."));
        }
        if os == Os::Mac && self.is(&[Modifier::Ctrl], Key::Space) {
            return Check::Warn(format!(
                "macOS also switches input sources with {label} when you have more than one."
            ));
        }
        if self.trigger() == Key::PrintScreen && self.modifiers().is_empty() {
            return Check::Warn(format!("{label} no longer takes screenshots."));
        }
        Check::Fine
    }

    /// Exactly these modifiers, on either side, and this key.
    fn is(&self, modifiers: &[Modifier], key: Key) -> bool {
        self.trigger() == key
            && self.modifiers().len() == modifiers.len()
            && self
                .modifiers()
                .iter()
                .all(|(modifier, _)| modifiers.contains(modifier))
    }
}

/// A key this OS's keyboards do not have, or cannot report.
fn missing_key(hotkey: &Hotkey, os: Os) -> Option<String> {
    let keys = hotkey
        .modifiers()
        .iter()
        .map(|&(modifier, side)| Key::Modifier(modifier, side))
        .chain([hotkey.trigger()]);
    for key in keys {
        let missing = match (key, os) {
            (Key::Modifier(Modifier::Fn, _), Os::Windows) => {
                Some("Windows never sees the Fn key".to_owned())
            }
            (
                Key::F(21..=24) | Key::PrintScreen | Key::ScrollLock | Key::Pause | Key::Menu,
                Os::Mac,
            ) => Some(format!("Mac keyboards have no {} key", key.label(os))),
            _ => None,
        };
        if missing.is_some() {
            return missing;
        }
    }
    None
}

/// Modifiers alone: Shift fires on every capital, a lone Super opens Start or a launcher, and
/// modifiers used in shortcuts start dictation for a moment each time.
fn modifiers_alone(hotkey: &Hotkey, os: Os, desktop: &str) -> Check {
    let label = hotkey.label_on(os);
    let Some((modifier, side)) = hotkey.trigger().modifier() else {
        return Check::Fine;
    };
    if !hotkey.modifiers().is_empty() {
        return Check::Fine;
    }
    let launcher = desktop
        .split(':')
        .any(|d| LAUNCHER_DESKTOPS.contains(&d.to_ascii_lowercase().as_str()));
    match (modifier, side, os) {
        (Modifier::Shift, Side::Either | Side::Left, _) => Check::Reject(format!(
            "{label} alone would start dictation with every capital letter you type."
        )),
        (Modifier::Shift, Side::Right, _) => Check::Warn(format!(
            "Typing a capital with {label} starts dictation for a moment."
        )),
        (Modifier::Super, _, Os::Windows) => {
            Check::Reject(format!("{label} alone opens the Start menu."))
        }
        (Modifier::Super, _, Os::Linux) if launcher => Check::Reject(format!(
            "{label} alone opens the overview or launcher on this desktop."
        )),
        (Modifier::Ctrl | Modifier::Alt, Side::Either | Side::Left, _) => Check::Warn(format!(
            "{label} is part of many shortcuts; each one starts dictation for a moment."
        )),
        (Modifier::Alt, Side::Right, Os::Linux | Os::Windows) => Check::Warn(format!(
            "{label} types special characters (AltGr) on many keyboard layouts."
        )),
        (Modifier::Fn, _, Os::Mac) => Check::Warn(format!(
            "macOS may act on {label} too: set System Settings → Keyboard → \"Press 🌐 key to\" \
             to Do Nothing."
        )),
        (Modifier::Fn, _, _) => {
            Check::Warn("Most keyboards handle Fn themselves and never report it.".to_owned())
        }
        _ => Check::Fine,
    }
}

/// Keys that type or edit text, alone or with Shift only.
fn typing(hotkey: &Hotkey, os: Os, label: &str) -> Option<String> {
    if hotkey
        .modifiers()
        .iter()
        .any(|&(modifier, _)| modifier != Modifier::Shift)
    {
        return None;
    }
    let shifted = !hotkey.modifiers().is_empty();
    let reason = match hotkey.trigger() {
        Key::Letter(_) if shifted => "types a capital letter",
        Key::Letter(_)
        | Key::Digit(_)
        | Key::Numpad(_)
        | Key::Minus
        | Key::Equal
        | Key::BracketLeft
        | Key::BracketRight
        | Key::Backslash
        | Key::Semicolon
        | Key::Quote
        | Key::Backquote
        | Key::Comma
        | Key::Period
        | Key::Slash
        | Key::NumpadAdd
        | Key::NumpadSubtract
        | Key::NumpadMultiply
        | Key::NumpadDivide
        | Key::NumpadDecimal => "types a character",
        Key::Space => "types a space",
        Key::Enter | Key::NumpadEnter | Key::Tab | Key::Backspace | Key::Delete => {
            "is needed for typing"
        }
        Key::Escape => "is how apps cancel",
        Key::Up
        | Key::Down
        | Key::Left
        | Key::Right
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown => "moves the cursor",
        _ => return None,
    };
    Some(format!(
        "{label} {reason}; add {}, {} or another modifier.",
        Modifier::Ctrl.label(os),
        Modifier::Alt.label(os)
    ))
}

/// What the system or most apps do with this shortcut, which earwig would take from them.
fn reserved(hotkey: &Hotkey, os: Os) -> Option<&'static str> {
    use Modifier::{Alt, Ctrl, Shift, Super};
    // Cmd on a Mac, Ctrl elsewhere.
    let app = if os == Os::Mac { Super } else { Ctrl };
    if let Key::Letter(letter) = hotkey.trigger()
        && hotkey.is(&[app], hotkey.trigger())
        && let Some(what) = app_shortcut(letter, os)
    {
        return Some(what);
    }
    let system: &[(&[Modifier], Key, &str)] = match os {
        Os::Mac => &[
            (&[Super], Key::Space, "opens Spotlight"),
            (&[Super, Alt], Key::Space, "opens a Finder search"),
            (&[Super], Key::Tab, "switches apps"),
            (&[Super], Key::Backquote, "switches windows"),
            (&[Super, Alt], Key::Escape, "opens Force Quit"),
            (&[Ctrl, Super], Key::Letter('Q'), "locks the screen"),
            (&[Super, Shift], Key::Digit(3), "takes a screenshot"),
            (&[Super, Shift], Key::Digit(4), "takes a screenshot"),
            (&[Super, Shift], Key::Digit(5), "takes a screenshot"),
        ],
        Os::Windows => &[
            (&[Alt], Key::Tab, "switches windows"),
            (&[Alt], Key::Escape, "switches windows"),
            (&[Alt], Key::F(4), "closes windows"),
            (&[Alt], Key::Space, "opens the window menu"),
            (&[Ctrl], Key::Escape, "opens the Start menu"),
            (&[Ctrl, Alt], Key::Delete, "is reserved by Windows"),
            (&[Ctrl, Shift], Key::Escape, "opens Task Manager"),
        ],
        Os::Linux => &[
            (&[Alt], Key::Tab, "switches windows"),
            (&[Super], Key::Tab, "switches windows"),
            (&[Alt], Key::F(4), "closes windows"),
            (&[Alt], Key::F(2), "opens the run dialog"),
            (&[Alt], Key::Space, "opens the window menu or launcher"),
            (&[Super], Key::Letter('L'), "locks the screen"),
            (&[Ctrl, Alt], Key::Delete, "is reserved by the system"),
            (&[Ctrl, Alt], Key::Backspace, "may end the session"),
        ],
    };
    if let Some(&(_, _, what)) = system
        .iter()
        .find(|(modifiers, key, _)| hotkey.is(modifiers, *key))
    {
        return Some(what);
    }
    if os == Os::Linux
        && hotkey.is(&[Ctrl, Alt], hotkey.trigger())
        && matches!(hotkey.trigger(), Key::F(1..=12))
    {
        return Some("switches to a text console");
    }
    if os == Os::Windows
        && hotkey.is(&[Super], hotkey.trigger())
        && matches!(
            hotkey.trigger(),
            Key::Letter(_) | Key::Digit(_) | Key::Space | Key::Tab | Key::Comma | Key::Period
        )
    {
        return Some("is one of Windows' own shortcuts");
    }
    None
}

/// What Ctrl (Command on a Mac) and `letter` do in most apps.
fn app_shortcut(letter: char, os: Os) -> Option<&'static str> {
    Some(match letter {
        'A' => "selects all in most apps",
        'C' => "copies in most apps",
        'V' => "pastes in most apps",
        'X' => "cuts in most apps",
        'Z' => "undoes in most apps",
        'Y' if os != Os::Mac => "redoes in most apps",
        'S' => "saves in most apps",
        'F' => "finds in most apps",
        'W' => "closes windows and tabs",
        'Q' => "quits apps",
        'T' => "opens a tab in most apps",
        'N' => "opens a window in most apps",
        'O' => "opens files in most apps",
        'P' => "prints in most apps",
        'H' | 'M' if os == Os::Mac => "hides or minimizes apps",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str, os: Os, desktop: &str) -> Check {
        text.parse::<Hotkey>().unwrap().check_on(os, desktop)
    }

    fn rejected(text: &str, os: Os, desktop: &str) -> String {
        match check(text, os, desktop) {
            Check::Reject(reason) => reason,
            other => panic!("{text} on {os:?} ({desktop}): {other:?}"),
        }
    }

    #[test]
    fn the_defaults_and_the_old_choices_are_fine() {
        assert_eq!(check("Super+Space", Os::Linux, "niri"), Check::Fine);
        assert_eq!(check("Super+Space", Os::Linux, "GNOME"), Check::Fine);
        assert_eq!(check("Option+Space", Os::Mac, ""), Check::Fine);
        assert_eq!(check("Ctrl+Space", Os::Windows, ""), Check::Fine);
        for key in ["RightCtrl", "Ctrl+Alt+Space"] {
            assert_eq!(check(key, Os::Windows, ""), Check::Fine, "{key}");
        }
        assert_eq!(check("RightCmd", Os::Mac, ""), Check::Fine);
        assert_eq!(check("RightAlt", Os::Mac, ""), Check::Fine);
        // Old choices with a catch stay usable.
        assert!(matches!(
            check("RightShift", Os::Windows, ""),
            Check::Warn(_)
        ));
        assert!(matches!(check("RightAlt", Os::Windows, ""), Check::Warn(_)));
        assert!(matches!(check("Ctrl+Space", Os::Mac, ""), Check::Warn(_)));
    }

    #[test]
    fn keys_that_type_are_refused_with_a_reason() {
        assert_eq!(
            rejected("A", Os::Linux, ""),
            "A types a character; add Ctrl, Alt or another modifier."
        );
        assert!(rejected("Shift+A", Os::Linux, "").contains("capital"));
        assert!(rejected("Space", Os::Windows, "").contains("types a space"));
        assert!(rejected("Shift+Space", Os::Windows, "").contains("types a space"));
        for key in [
            "Enter",
            "Tab",
            "Escape",
            "Backspace",
            "7",
            "Numpad7",
            "Slash",
            "Left",
        ] {
            rejected(key, Os::Linux, "");
        }
        assert!(rejected("Ctrl+CapsLock", Os::Linux, "").contains("Caps Lock"));
        assert_eq!(
            check("Ctrl+A", Os::Linux, ""),
            Check::Reject("Ctrl+A selects all in most apps.".into())
        );
        assert_eq!(check("F9", Os::Linux, ""), Check::Fine);
        assert_eq!(check("Ctrl+Alt+D", Os::Linux, ""), Check::Fine);
    }

    #[test]
    fn a_mac_names_the_keys_in_a_reason_by_their_symbols() {
        assert_eq!(
            rejected("Shift+A", Os::Mac, ""),
            "⇧ A types a capital letter; add ⌃, ⌥ or another modifier."
        );
        assert_eq!(
            check("Ctrl+Space", Os::Mac, ""),
            Check::Warn(
                "macOS also switches input sources with ⌃ Space when you have more than one."
                    .into()
            )
        );
        assert_eq!(
            rejected("LeftShift", Os::Mac, ""),
            "Left ⇧ alone would start dictation with every capital letter you type."
        );
    }

    #[test]
    fn system_shortcuts_are_refused() {
        assert_eq!(
            rejected("Cmd+Space", Os::Mac, ""),
            "⌘ Space opens Spotlight."
        );
        assert!(rejected("Cmd+Tab", Os::Mac, "").contains("switches apps"));
        assert!(rejected("Cmd+C", Os::Mac, "").contains("copies"));
        assert_eq!(check("Ctrl+C", Os::Mac, ""), Check::Fine);
        assert!(rejected("Alt+Tab", Os::Windows, "").contains("switches windows"));
        assert!(rejected("Ctrl+Alt+Delete", Os::Windows, "").contains("reserved"));
        assert!(rejected("Win+L", Os::Windows, "").contains("Windows' own"));
        assert!(rejected("Win+Space", Os::Windows, "").contains("Windows' own"));
        assert!(rejected("Ctrl+Alt+F3", Os::Linux, "").contains("text console"));
        assert!(rejected("Super+L", Os::Linux, "sway").contains("locks"));
        assert!(rejected("Alt+F4", Os::Linux, "").contains("closes"));
    }

    #[test]
    fn a_lone_super_is_refused_where_it_opens_something() {
        assert!(rejected("LeftSuper", Os::Linux, "ubuntu:GNOME").contains("launcher"));
        assert!(rejected("Super", Os::Linux, "KDE").contains("launcher"));
        assert_eq!(check("RightSuper", Os::Linux, "niri"), Check::Fine);
        assert!(rejected("RightWin", Os::Windows, "").contains("Start"));
        assert_eq!(check("RightCmd", Os::Mac, ""), Check::Fine);
    }

    #[test]
    fn modifiers_used_everywhere_alone_are_refused_or_warned_about() {
        assert!(rejected("Shift", Os::Linux, "").contains("capital"));
        assert!(matches!(check("LeftCtrl", Os::Linux, ""), Check::Warn(_)));
        assert!(matches!(check("Fn", Os::Mac, ""), Check::Warn(_)));
        assert_eq!(check("Ctrl+Shift", Os::Linux, ""), Check::Fine);
    }

    #[test]
    fn keys_a_keyboard_lacks_are_refused() {
        assert!(rejected("F24", Os::Mac, "").contains("no F24"));
        assert!(rejected("Fn", Os::Windows, "").contains("Fn"));
        assert_eq!(check("F24", Os::Linux, ""), Check::Fine);
        assert!(matches!(
            check("PrintScreen", Os::Linux, ""),
            Check::Warn(_)
        ));
    }
}
