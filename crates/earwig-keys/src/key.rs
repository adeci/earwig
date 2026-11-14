//! Keys and their names in settings files (`RightCtrl`) and windows (`Right Ctrl`, `Right ⌘`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    #[cfg(target_os = "linux")]
    pub(crate) const CURRENT: Self = Self::Linux;
    #[cfg(target_os = "macos")]
    pub(crate) const CURRENT: Self = Self::Mac;
    #[cfg(windows)]
    pub(crate) const CURRENT: Self = Self::Windows;
}

/// Hotkeys list modifiers in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modifier {
    Ctrl,
    /// Option on a Mac.
    Alt,
    Shift,
    /// Cmd on a Mac, Win on Windows, Super (Meta, Logo) on Linux.
    Super,
    /// The Mac's Fn (Globe) key.
    Fn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Either,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Real keys have a side, except Fn; a hotkey may say `Side::Either`.
    Modifier(Modifier, Side),
    /// `A`–`Z`, by their place on a US keyboard.
    Letter(char),
    /// The number row.
    Digit(u8),
    F(u8),
    Numpad(u8),
    Space,
    Tab,
    Enter,
    Escape,
    Backspace,
    /// Forward delete.
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Backquote,
    Comma,
    Period,
    Slash,
    CapsLock,
    PrintScreen,
    ScrollLock,
    Pause,
    /// The context menu key.
    Menu,
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadDecimal,
    NumpadEnter,
}

/// (key, settings name, window label)
const NAMED: [(Key, &str, &str); 37] = [
    (Key::Space, "Space", "Space"),
    (Key::Tab, "Tab", "Tab"),
    (Key::Enter, "Enter", "Enter"),
    (Key::Escape, "Escape", "Esc"),
    (Key::Backspace, "Backspace", "Backspace"),
    (Key::Delete, "Delete", "Delete"),
    (Key::Insert, "Insert", "Insert"),
    (Key::Home, "Home", "Home"),
    (Key::End, "End", "End"),
    (Key::PageUp, "PageUp", "Page Up"),
    (Key::PageDown, "PageDown", "Page Down"),
    (Key::Up, "Up", "Up"),
    (Key::Down, "Down", "Down"),
    (Key::Left, "Left", "Left"),
    (Key::Right, "Right", "Right"),
    (Key::Minus, "Minus", "-"),
    (Key::Equal, "Equal", "="),
    (Key::BracketLeft, "BracketLeft", "["),
    (Key::BracketRight, "BracketRight", "]"),
    (Key::Backslash, "Backslash", "\\"),
    (Key::Semicolon, "Semicolon", ";"),
    (Key::Quote, "Quote", "'"),
    (Key::Backquote, "Backquote", "`"),
    (Key::Comma, "Comma", ","),
    (Key::Period, "Period", "."),
    (Key::Slash, "Slash", "/"),
    (Key::CapsLock, "CapsLock", "Caps Lock"),
    (Key::PrintScreen, "PrintScreen", "Print Screen"),
    (Key::ScrollLock, "ScrollLock", "Scroll Lock"),
    (Key::Pause, "Pause", "Pause"),
    (Key::Menu, "Menu", "Menu"),
    (Key::NumpadAdd, "NumpadAdd", "Num +"),
    (Key::NumpadSubtract, "NumpadSubtract", "Num -"),
    (Key::NumpadMultiply, "NumpadMultiply", "Num *"),
    (Key::NumpadDivide, "NumpadDivide", "Num /"),
    (Key::NumpadDecimal, "NumpadDecimal", "Num ."),
    (Key::NumpadEnter, "NumpadEnter", "Num Enter"),
];

/// Other spellings settings files may use, lower-case.
const ALIASES: [(&str, Key); 28] = [
    ("return", Key::Enter),
    ("esc", Key::Escape),
    ("del", Key::Delete),
    ("ins", Key::Insert),
    ("pgup", Key::PageUp),
    ("pgdn", Key::PageDown),
    ("arrowup", Key::Up),
    ("arrowdown", Key::Down),
    ("arrowleft", Key::Left),
    ("arrowright", Key::Right),
    ("-", Key::Minus),
    ("=", Key::Equal),
    ("[", Key::BracketLeft),
    ("]", Key::BracketRight),
    ("\\", Key::Backslash),
    (";", Key::Semicolon),
    ("'", Key::Quote),
    ("apostrophe", Key::Quote),
    ("`", Key::Backquote),
    ("grave", Key::Backquote),
    (",", Key::Comma),
    (".", Key::Period),
    ("/", Key::Slash),
    ("print", Key::PrintScreen),
    ("apps", Key::Menu),
    ("contextmenu", Key::Menu),
    ("numpadplus", Key::NumpadAdd),
    ("numpadminus", Key::NumpadSubtract),
];

/// How a Mac labels these, by the symbol on the key or in its menus.
const MAC_LABELS: [(Key, &str); 10] = [
    (Key::Enter, "↩"),
    (Key::Escape, "⎋"),
    (Key::Tab, "⇥"),
    (Key::Backspace, "⌫"),
    (Key::Delete, "⌦"),
    (Key::Up, "↑"),
    (Key::Down, "↓"),
    (Key::Left, "←"),
    (Key::Right, "→"),
    (Key::Insert, "Help"),
];

impl Modifier {
    /// Ctrl, Alt, Shift and Super: the modifiers every OS has.
    pub(crate) const COMMON: [Self; 4] = [Self::Ctrl, Self::Alt, Self::Shift, Self::Super];

    /// As settings files write it.
    pub(crate) fn name(self, os: Os) -> &'static str {
        match (self, os) {
            (Self::Ctrl, _) => "Ctrl",
            (Self::Alt, Os::Mac) => "Option",
            (Self::Alt, _) => "Alt",
            (Self::Shift, _) => "Shift",
            (Self::Super, Os::Mac) => "Cmd",
            (Self::Super, Os::Windows) => "Win",
            (Self::Super, Os::Linux) => "Super",
            (Self::Fn, _) => "Fn",
        }
    }

    /// As windows show it; a Mac uses the symbols on its keys.
    pub(crate) fn label(self, os: Os) -> &'static str {
        match (self, os) {
            (Self::Ctrl, Os::Mac) => "⌃",
            (Self::Alt, Os::Mac) => "⌥",
            (Self::Shift, Os::Mac) => "⇧",
            (Self::Super, Os::Mac) => "⌘",
            (Self::Fn, Os::Mac) => "fn",
            _ => self.name(os),
        }
    }

    /// Any of its names, lower-case.
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "ctrl" | "control" => Self::Ctrl,
            "alt" | "option" | "opt" => Self::Alt,
            "shift" => Self::Shift,
            "super" | "cmd" | "command" | "win" | "windows" | "meta" | "logo" => Self::Super,
            "fn" => Self::Fn,
            _ => return None,
        })
    }
}

impl Key {
    pub fn modifier(self) -> Option<(Modifier, Side)> {
        match self {
            Self::Modifier(modifier, side) => Some((modifier, side)),
            _ => None,
        }
    }

    /// A modifier part of a hotkey (`RightCtrl`, `option`), lower-case.
    pub(crate) fn parse_modifier(text: &str) -> Option<Self> {
        let (side, name) = if let Some(name) = text.strip_prefix("left") {
            (Side::Left, name)
        } else if let Some(name) = text.strip_prefix("right") {
            (Side::Right, name)
        } else {
            (Side::Either, text)
        };
        let modifier = Modifier::parse(name)?;
        (modifier != Modifier::Fn || side == Side::Either).then_some(Self::Modifier(modifier, side))
    }

    /// Any other key by any of its names, lower-case.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_lowercase() {
                return Some(Self::Letter(c.to_ascii_uppercase()));
            }
            if let Some(digit) = c.to_digit(10) {
                return u8::try_from(digit).ok().map(Self::Digit);
            }
        }
        let numbered = |prefix: &str, range: std::ops::RangeInclusive<u8>| {
            text.strip_prefix(prefix)
                .filter(|n| !n.starts_with('0') || n.len() == 1)
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| range.contains(n))
        };
        if let Some(n) = numbered("f", 1..=24) {
            return Some(Self::F(n));
        }
        if let Some(n) = numbered("numpad", 0..=9) {
            return Some(Self::Numpad(n));
        }
        NAMED
            .iter()
            .find(|(_, name, _)| name.eq_ignore_ascii_case(text))
            .map(|&(key, _, _)| key)
            .or_else(|| {
                ALIASES
                    .iter()
                    .find(|(alias, _)| *alias == text)
                    .map(|&(_, key)| key)
            })
    }

    /// As settings files write it.
    pub(crate) fn name(self, os: Os) -> String {
        match self {
            Self::Modifier(modifier, side) => format!("{}{}", side_name(side), modifier.name(os)),
            Self::Enter if os == Os::Mac => "Return".into(),
            _ => self.common_name(false),
        }
    }

    /// As windows show it.
    pub(crate) fn label(self, os: Os) -> String {
        match (self, os) {
            (Self::Modifier(modifier, Side::Either), _) => modifier.label(os).into(),
            (Self::Modifier(modifier, side), _) => {
                format!("{} {}", side_name(side), modifier.label(os))
            }
            (_, Os::Mac) => MAC_LABELS
                .iter()
                .find(|(key, _)| *key == self)
                .map_or_else(|| self.common_name(true), |&(_, label)| label.into()),
            _ => self.common_name(true),
        }
    }

    fn common_name(self, label: bool) -> String {
        match self {
            Self::Letter(c) => c.to_string(),
            Self::Digit(d) => d.to_string(),
            Self::F(n) => format!("F{n}"),
            Self::Numpad(n) if label => format!("Num {n}"),
            Self::Numpad(n) => format!("Numpad{n}"),
            _ => NAMED
                .iter()
                .find(|(key, _, _)| *key == self)
                .map(|&(_, name, shown)| if label { shown } else { name })
                .unwrap_or_default()
                .to_owned(),
        }
    }

    /// Every key a keyboard can have that earwig knows.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        let sides = Modifier::COMMON.into_iter().flat_map(|modifier| {
            [Side::Left, Side::Right].map(|side| Self::Modifier(modifier, side))
        });
        sides
            .chain([Self::Modifier(Modifier::Fn, Side::Either)])
            .chain(('A'..='Z').map(Self::Letter))
            .chain((0..=9).map(Self::Digit))
            .chain((1..=24).map(Self::F))
            .chain((0..=9).map(Self::Numpad))
            .chain(NAMED.iter().map(|&(key, _, _)| key))
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Either => "",
        Side::Left => "Left",
        Side::Right => "Right",
    }
}

/// Modifier keys held down, left and right apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Held(u16);

impl Held {
    fn bits(key: Key) -> u16 {
        let Key::Modifier(modifier, side) = key else {
            return 0;
        };
        let shift = match modifier {
            Modifier::Ctrl => 0,
            Modifier::Alt => 2,
            Modifier::Shift => 4,
            Modifier::Super => 6,
            Modifier::Fn => return 1 << 8,
        };
        match side {
            Side::Left => 1 << shift,
            Side::Right => 1 << (shift + 1),
            Side::Either => 0b11 << shift,
        }
    }

    #[must_use]
    pub(crate) fn with(self, key: Key) -> Self {
        Self(self.0 | Self::bits(key))
    }

    #[must_use]
    pub(crate) fn without(self, key: Key) -> Self {
        Self(self.0 & !Self::bits(key))
    }

    /// Without the key an edge is for, if any.
    #[must_use]
    pub(crate) fn besides(self, key: Option<Key>) -> Self {
        key.map_or(self, |key| self.without(key))
    }

    /// One of the keys `side` names is down.
    pub(crate) fn has(self, modifier: Modifier, side: Side) -> bool {
        self.0 & Self::bits(Key::Modifier(modifier, side)) != 0
    }

    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The physical keys held, in [`Modifier`] order.
    pub(crate) fn keys(self) -> impl Iterator<Item = Key> {
        Key::all()
            .take_while(|key| key.modifier().is_some())
            .filter(move |&key| self.0 & Self::bits(key) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_parses_back_from_its_name_on_every_os() {
        for os in [Os::Linux, Os::Mac, Os::Windows] {
            for key in Key::all() {
                let name = key.name(os).to_ascii_lowercase();
                let parsed = Key::parse_modifier(&name).or_else(|| Key::parse(&name));
                assert_eq!(parsed, Some(key), "{name} on {os:?}");
            }
        }
    }

    #[test]
    fn keys_have_names_people_know_on_each_os() {
        let right_cmd = Key::Modifier(Modifier::Super, Side::Right);
        assert_eq!(right_cmd.name(Os::Mac), "RightCmd");
        assert_eq!(right_cmd.label(Os::Windows), "Right Win");
        assert_eq!(right_cmd.label(Os::Linux), "Right Super");
        assert_eq!(Key::Enter.name(Os::Mac), "Return");
        assert_eq!(Key::Enter.label(Os::Windows), "Enter");
        assert_eq!(Key::PageUp.label(Os::Linux), "Page Up");
        assert_eq!(Key::Numpad(5).name(Os::Linux), "Numpad5");
        assert_eq!(Key::Numpad(5).label(Os::Linux), "Num 5");
    }

    #[test]
    fn aliases_and_odd_spellings() {
        assert_eq!(Key::parse("return"), Some(Key::Enter));
        assert_eq!(Key::parse("esc"), Some(Key::Escape));
        assert_eq!(Key::parse("f07"), None);
        assert_eq!(Key::parse("/"), Some(Key::Slash));
    }

    #[test]
    fn held_keys_keep_their_sides() {
        let held = Held::default()
            .with(Key::Modifier(Modifier::Ctrl, Side::Right))
            .with(Key::Modifier(Modifier::Shift, Side::Left));
        assert!(held.has(Modifier::Ctrl, Side::Either));
        assert!(held.has(Modifier::Ctrl, Side::Right));
        assert!(!held.has(Modifier::Ctrl, Side::Left));
        assert!(!held.has(Modifier::Alt, Side::Either));
        assert_eq!(
            held.keys().collect::<Vec<_>>(),
            [
                Key::Modifier(Modifier::Ctrl, Side::Right),
                Key::Modifier(Modifier::Shift, Side::Left)
            ]
        );
        assert!(
            held.without(Key::Modifier(Modifier::Ctrl, Side::Right))
                .without(Key::Modifier(Modifier::Shift, Side::Left))
                .is_empty()
        );
        assert!(Held::default().with(Key::Space).is_empty());
    }
}
