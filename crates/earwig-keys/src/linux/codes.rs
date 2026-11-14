//! evdev key codes (`linux/input-event-codes.h`) of the keys earwig names.

use crate::key::{Key, Modifier, Side};

pub(super) const KEY_LEFTCTRL: u16 = 29;

/// `KEY_A` … `KEY_Z`.
const LETTERS: [u16; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45,
    21, 44,
];
/// `KEY_KP0` … `KEY_KP9`.
const NUMPAD: [u16; 10] = [82, 79, 80, 81, 75, 76, 77, 71, 72, 73];

const NAMED: [(u16, Key); 48] = [
    (KEY_LEFTCTRL, Key::Modifier(Modifier::Ctrl, Side::Left)),
    (97, Key::Modifier(Modifier::Ctrl, Side::Right)),
    (56, Key::Modifier(Modifier::Alt, Side::Left)),
    (100, Key::Modifier(Modifier::Alt, Side::Right)),
    (42, Key::Modifier(Modifier::Shift, Side::Left)),
    (54, Key::Modifier(Modifier::Shift, Side::Right)),
    (125, Key::Modifier(Modifier::Super, Side::Left)),
    (126, Key::Modifier(Modifier::Super, Side::Right)),
    // KEY_FN: most keyboards keep it to themselves.
    (0x1d0, Key::Modifier(Modifier::Fn, Side::Either)),
    (1, Key::Escape),
    (12, Key::Minus),
    (13, Key::Equal),
    (14, Key::Backspace),
    (15, Key::Tab),
    (26, Key::BracketLeft),
    (27, Key::BracketRight),
    (28, Key::Enter),
    (39, Key::Semicolon),
    (40, Key::Quote),
    (41, Key::Backquote),
    (43, Key::Backslash),
    (51, Key::Comma),
    (52, Key::Period),
    (53, Key::Slash),
    (55, Key::NumpadMultiply),
    (57, Key::Space),
    (58, Key::CapsLock),
    (70, Key::ScrollLock),
    (74, Key::NumpadSubtract),
    (78, Key::NumpadAdd),
    (83, Key::NumpadDecimal),
    (96, Key::NumpadEnter),
    (98, Key::NumpadDivide),
    // KEY_SYSRQ and KEY_PRINT.
    (99, Key::PrintScreen),
    (210, Key::PrintScreen),
    (102, Key::Home),
    (103, Key::Up),
    (104, Key::PageUp),
    (105, Key::Left),
    (106, Key::Right),
    (107, Key::End),
    (108, Key::Down),
    (109, Key::PageDown),
    (110, Key::Insert),
    (111, Key::Delete),
    (119, Key::Pause),
    // KEY_COMPOSE is the PC keyboard's menu key; KEY_MENU some others'.
    (127, Key::Menu),
    (139, Key::Menu),
];

/// Every (code, key) pair, a key with several codes once per code.
pub(super) fn table() -> impl Iterator<Item = (u16, Key)> {
    let letters = ('A'..='Z')
        .zip(LETTERS)
        .map(|(c, code)| (code, Key::Letter(c)));
    // KEY_1 … KEY_9 are 2 … 10, KEY_0 is 11.
    let digits = (0..=9u8).map(|d| (if d == 0 { 11 } else { 1 + u16::from(d) }, Key::Digit(d)));
    // KEY_F1 … KEY_F10 are 59 … 68, KEY_F11 and KEY_F12 87 and 88, KEY_F13 … KEY_F24 183 … 194.
    let function = (1..=24u8).map(|n| {
        let code = match n {
            1..=10 => 58 + u16::from(n),
            11 | 12 => 76 + u16::from(n),
            _ => 170 + u16::from(n),
        };
        (code, Key::F(n))
    });
    let numpad = (0..=9u8)
        .zip(NUMPAD)
        .map(|(n, code)| (code, Key::Numpad(n)));
    letters
        .chain(digits)
        .chain(function)
        .chain(numpad)
        .chain(NAMED)
}

pub(super) fn key(code: u16) -> Option<Key> {
    table().find(|&(c, _)| c == code).map(|(_, key)| key)
}

/// The first nine named keys are the modifiers, which [`crate::key::Held`] tracks.
pub(super) fn modifier_codes() -> impl Iterator<Item = (u16, Key)> {
    NAMED.into_iter().take(9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_code_that_maps_back_to_it() {
        for key in Key::all() {
            let codes: Vec<u16> = table().filter(|&(_, k)| k == key).map(|(c, _)| c).collect();
            assert!(!codes.is_empty(), "{key:?}");
            for code in codes {
                assert_eq!(super::key(code), Some(key), "{code}");
            }
        }
    }

    #[test]
    fn known_codes() {
        assert_eq!(key(57), Some(Key::Space));
        assert_eq!(key(30), Some(Key::Letter('A')));
        assert_eq!(key(11), Some(Key::Digit(0)));
        assert_eq!(key(2), Some(Key::Digit(1)));
        assert_eq!(key(88), Some(Key::F(12)));
        assert_eq!(key(194), Some(Key::F(24)));
        assert_eq!(key(97), Some(Key::Modifier(Modifier::Ctrl, Side::Right)));
        assert_eq!(key(116), None, "KEY_POWER");
        assert!(modifier_codes().all(|(_, key)| key.modifier().is_some()));
    }
}
