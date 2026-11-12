use super::evdev::{EV_KEY, EV_MSC, InputEvent};
use super::{codes, passed_on, swallows};
use crate::matcher::Mask;

const KEY_SPACE: u16 = 57;
const KEY_LEFTMETA: u16 = 125;

fn scan(code: i32) -> InputEvent {
    InputEvent::new(EV_MSC, 4, code)
}

fn key(code: u16, value: i32) -> InputEvent {
    InputEvent::new(EV_KEY, code, value)
}

#[test]
fn other_keys_pass_on_whole() {
    let frame = [scan(0x70004), key(30, 1)];
    let out = passed_on(&frame, false, Mask::None, 0, InputEvent::syn());
    assert_eq!(out, [scan(0x70004), key(30, 1), InputEvent::syn()]);
}

#[test]
fn a_swallowed_key_leaves_nothing_but_a_mask_tap_behind_super() {
    // Super went through earlier; Space, with its scan code, was swallowed.
    let frame = [scan(0x7002c)];
    let out = passed_on(&frame, true, Mask::After, 0, InputEvent::syn());
    let ctrl = codes::KEY_LEFTCTRL;
    assert_eq!(
        out,
        [
            key(ctrl, 1),
            InputEvent::syn(),
            key(ctrl, 0),
            InputEvent::syn()
        ]
    );
    assert_eq!(
        passed_on(&frame, true, Mask::None, 0, InputEvent::syn()),
        []
    );
}

#[test]
fn a_held_back_release_follows_its_mask_tap() {
    let out = passed_on(&[], true, Mask::Release, KEY_LEFTMETA, InputEvent::syn());
    assert_eq!(
        out.iter().filter(|e| e.kind == EV_KEY).collect::<Vec<_>>(),
        [
            &key(codes::KEY_LEFTCTRL, 1),
            &key(codes::KEY_LEFTCTRL, 0),
            &key(KEY_LEFTMETA, 0)
        ]
    );
}

#[test]
fn a_frame_with_another_key_keeps_it_when_the_hotkey_is_swallowed() {
    let frame = [key(30, 1)];
    let out = passed_on(&frame, true, Mask::None, 0, InputEvent::syn());
    assert_eq!(out, [key(30, 1), InputEvent::syn()]);
}

#[test]
fn only_keys_with_modifiers_and_masked_modifiers_take_a_grab() {
    assert!(swallows(&"Super+Space".parse().unwrap()));
    assert!(swallows(&"RightAlt".parse().unwrap()), "masked");
    assert!(!swallows(&"RightCtrl".parse().unwrap()));
    assert_eq!(codes::key(KEY_SPACE), Some(crate::Key::Space));
}
