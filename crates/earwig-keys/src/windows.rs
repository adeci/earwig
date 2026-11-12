//! Windows: a low-level keyboard hook (`WH_KEYBOARD_LL`). It sees every key
//! edge before any app does, which `RegisterHotKey` cannot (no releases, no
//! lone modifiers), and needs no permission.

use std::cell::Cell;
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN,
    WM_SYSKEYDOWN,
};

use crate::key::{Held, Key, Modifier, Side};
use crate::listener::{Blocking, Shared, Snapshot, Status};
use crate::matcher::{Edge, Mask};
use crate::{Event, SYNTHETIC};

/// An unassigned virtual key, sent between a swallowed key and the release
/// of Alt or Win so that release does not open the menu bar or Start.
const MASK: u16 = 0xe8;

/// Wakes [`run`]'s message loop: the hook thread's id, while it runs.
#[derive(Default)]
pub(crate) struct Waker(AtomicU32);

impl Waker {
    pub(crate) fn wake(&self) {
        let thread = self.0.load(Ordering::Acquire);
        if thread != 0 {
            // SAFETY: posting a message with no payload to a thread.
            unsafe { PostThreadMessageW(thread, WM_APP, 0, 0) };
        }
    }
}

pub(crate) fn can_see_keys() -> bool {
    true
}

/// What the hook works with, on the hook's thread.
struct Context<'a> {
    shared: &'a Shared,
    events: &'a mut dyn FnMut(Event) -> bool,
    /// Virtual keys down, to tell auto-repeat from a press.
    down: [u64; 4],
    done: bool,
}

thread_local! {
    /// The running hook's [`Context`].
    static CONTEXT: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

pub(crate) fn run(shared: &Shared, events: &mut dyn FnMut(Event) -> bool) -> io::Result<()> {
    let mut context = Context {
        shared,
        events,
        down: [0; 4],
        done: false,
    };
    CONTEXT.set((&raw mut context).cast());
    // SAFETY: plain thread id.
    shared
        .waker
        .0
        .store(unsafe { GetCurrentThreadId() }, Ordering::Release);
    // SAFETY: a global hook needs no module handle when the callback lives
    // in this process; it runs on this thread's message loop.
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), std::ptr::null_mut(), 0) };
    let result = if hook.is_null() {
        Err(io::Error::last_os_error())
    } else {
        loop {
            let snapshot = shared.snapshot();
            if snapshot.stop || context.done {
                break;
            }
            shared.set_status(status(&snapshot));
            // SAFETY: plain message loop; `msg` is written by GetMessageW.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                if GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) <= 0 {
                    break;
                }
                DispatchMessageW(&raw const msg);
            }
        }
        // SAFETY: the hook installed above.
        unsafe { UnhookWindowsHookEx(hook) };
        Ok(())
    };
    shared.waker.0.store(0, Ordering::Release);
    CONTEXT.set(std::ptr::null_mut());
    result
}

fn status(snapshot: &Snapshot) -> Status {
    Status::Watching(
        if snapshot
            .hotkey
            .as_ref()
            .is_some_and(|h| !h.is_modifier_only())
        {
            Blocking::Yes
        } else {
            Blocking::PassThrough
        },
    )
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let context = CONTEXT.get();
    // SAFETY: for HC_ACTION, lparam points at the event (WH_KEYBOARD_LL).
    let event = (code == HC_ACTION.cast_signed() && !context.is_null())
        .then(|| unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) });
    if let Some(event) = event.filter(|event| event.dwExtraInfo != SYNTHETIC)
        && let Ok(vk) = u16::try_from(event.vkCode)
    {
        // SAFETY: set by `run`, on this thread, for as long as the hook is in.
        let context = unsafe { &mut *context.cast::<Context<'_>>() };
        let down = matches!(u32::try_from(wparam), Ok(WM_KEYDOWN | WM_SYSKEYDOWN));
        if context.key(vk, event.flags & LLKHF_EXTENDED != 0, down) {
            return 1;
        }
    }
    // SAFETY: passing the event on, as every hook must.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

impl Context<'_> {
    /// One key edge; whether to swallow it.
    fn key(&mut self, vk: u16, extended: bool, down: bool) -> bool {
        let slot = usize::from(vk / 64) % 4;
        let bit = 1u64 << (vk % 64);
        let edge = match (down, self.down[slot] & bit != 0) {
            (true, true) => Edge::Repeat,
            (true, false) => Edge::Down,
            (false, _) => Edge::Up,
        };
        if down {
            self.down[slot] |= bit;
        } else {
            self.down[slot] &= !bit;
        }
        let key = key(vk, extended);
        let held = key.map_or_else(held, |key| held().without(key));
        let action = self.shared.edge(key, edge, held);
        if let Some(event) = action.event
            && !(self.events)(event)
        {
            self.done = true;
            // SAFETY: posting to this (the hook's) thread to end its loop.
            unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP, 0, 0) };
        }
        match action.mask {
            Mask::None => {}
            Mask::After => send(&[(MASK, false), (MASK, true)]),
            Mask::Release => send(&[(MASK, false), (MASK, true), (vk, true)]),
        }
        action.swallow
    }
}

/// The modifier keys down now (before the edge being hooked).
fn held() -> Held {
    MODIFIERS
        .iter()
        // SAFETY: no preconditions.
        .filter(|&&(vk, _)| unsafe { GetAsyncKeyState(i32::from(vk)) } < 0)
        .fold(Held::default(), |held, &(_, key)| held.with(key))
}

/// Send key edges marked as this program's own.
fn send(keys: &[(u16, bool)]) {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, up)| {
            let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
            // VK_RCONTROL, VK_RMENU, VK_LWIN, VK_RWIN
            if matches!(vk, 0xa3 | 0xa5 | 0x5b | 0x5c) {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        // Apps that read scan codes ignore keys without one.
                        // SAFETY: no preconditions.
                        wScan: u16::try_from(unsafe {
                            MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC)
                        })
                        .unwrap_or(0),
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: SYNTHETIC,
                    },
                },
            }
        })
        .collect();
    let size = i32::try_from(size_of::<INPUT>()).unwrap_or(i32::MAX);
    let count = u32::try_from(inputs.len()).unwrap_or(0);
    // SAFETY: `inputs` holds `count` initialised INPUTs.
    unsafe { SendInput(count, inputs.as_ptr(), size) };
}

/// `VK_LCONTROL` … `VK_RWIN`: hooks see each side's own virtual key.
const MODIFIERS: [(u16, Key); 8] = [
    (0xa2, Key::Modifier(Modifier::Ctrl, Side::Left)),
    (0xa3, Key::Modifier(Modifier::Ctrl, Side::Right)),
    (0xa4, Key::Modifier(Modifier::Alt, Side::Left)),
    (0xa5, Key::Modifier(Modifier::Alt, Side::Right)),
    (0xa0, Key::Modifier(Modifier::Shift, Side::Left)),
    (0xa1, Key::Modifier(Modifier::Shift, Side::Right)),
    (0x5b, Key::Modifier(Modifier::Super, Side::Left)),
    (0x5c, Key::Modifier(Modifier::Super, Side::Right)),
];

const NAMED: [(u16, Key); 35] = [
    (0x20, Key::Space),
    (0x09, Key::Tab),
    (0x0d, Key::Enter),
    (0x1b, Key::Escape),
    (0x08, Key::Backspace),
    (0x2e, Key::Delete),
    (0x2d, Key::Insert),
    (0x24, Key::Home),
    (0x23, Key::End),
    (0x21, Key::PageUp),
    (0x22, Key::PageDown),
    (0x26, Key::Up),
    (0x28, Key::Down),
    (0x25, Key::Left),
    (0x27, Key::Right),
    (0xbd, Key::Minus),
    (0xbb, Key::Equal),
    (0xdb, Key::BracketLeft),
    (0xdd, Key::BracketRight),
    (0xdc, Key::Backslash),
    (0xba, Key::Semicolon),
    (0xde, Key::Quote),
    (0xc0, Key::Backquote),
    (0xbc, Key::Comma),
    (0xbe, Key::Period),
    (0xbf, Key::Slash),
    (0x14, Key::CapsLock),
    (0x2c, Key::PrintScreen),
    (0x91, Key::ScrollLock),
    (0x13, Key::Pause),
    (0x5d, Key::Menu),
    (0x6b, Key::NumpadAdd),
    (0x6d, Key::NumpadSubtract),
    (0x6a, Key::NumpadMultiply),
    (0x6f, Key::NumpadDivide),
];
/// `VK_DECIMAL`.
const NUMPAD_DECIMAL: u16 = 0x6e;

fn table() -> impl Iterator<Item = (u16, Key)> {
    // VK_A … VK_Z are the letters' ASCII codes.
    let letters = (b'A'..=b'Z').map(|c| (u16::from(c), Key::Letter(char::from(c))));
    let digits = (0..=9u8).map(|d| (u16::from(b'0' + d), Key::Digit(d)));
    // VK_F1 is 0x70.
    let function = (1..=24u8).map(|n| (0x6f + u16::from(n), Key::F(n)));
    // VK_NUMPAD0 is 0x60.
    let numpad = (0..=9u8).map(|n| (0x60 + u16::from(n), Key::Numpad(n)));
    letters
        .chain(digits)
        .chain(function)
        .chain(numpad)
        .chain(MODIFIERS)
        .chain(NAMED)
        .chain([(NUMPAD_DECIMAL, Key::NumpadDecimal)])
}

/// The key of virtual key `vk`; Enter on the number pad is the extended one.
fn key(vk: u16, extended: bool) -> Option<Key> {
    let key = table().find(|&(v, _)| v == vk).map(|(_, key)| key)?;
    Some(if key == Key::Enter && extended {
        Key::NumpadEnter
    } else {
        key
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_virtual_key_that_maps_back() {
        for key in Key::all() {
            if key == Key::Modifier(Modifier::Fn, Side::Either) {
                continue;
            }
            let vk = if key == Key::NumpadEnter {
                Some(0x0d)
            } else {
                table().find(|&(_, k)| k == key).map(|(vk, _)| vk)
            };
            let vk = vk.unwrap_or_else(|| panic!("{key:?}"));
            assert_eq!(
                super::key(vk, key == Key::NumpadEnter),
                Some(key),
                "{key:?}"
            );
        }
        assert_eq!(super::key(u16::from(b'X'), false), Some(Key::Letter('X')));
        assert_eq!(super::key(0x87, false), Some(Key::F(24)));
    }
}
