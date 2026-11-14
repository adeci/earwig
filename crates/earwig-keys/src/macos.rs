//! macOS: a `CGEventTap` on the session's keys, which needs Accessibility; [`run`] keeps trying
//! until it's granted. The tap only swallows a key-with-modifiers hotkey.

use std::ffi::c_void;
use std::io;
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::key::{Held, Key, Modifier, Side};
use crate::listener::{Blocking, Shared, Status};
use crate::matcher::Edge;
use crate::{Event, SYNTHETIC};

type CGEventRef = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFStringRef = *const c_void;
type CGEventTapCallBack =
    unsafe extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef;

/// `kCGSessionEventTap`
const TAP_SESSION: u32 = 1;
/// `kCGHeadInsertEventTap`
const HEAD_INSERT: u32 = 0;
/// `kCGEventTapOptionDefault`: may swallow events.
const TAP_ACTIVE: u32 = 0;
/// `kCGEventTapOptionListenOnly`
const TAP_LISTEN_ONLY: u32 = 1;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
/// `kCGKeyboardEventAutorepeat`
const FIELD_AUTOREPEAT: u32 = 8;
/// `kCGKeyboardEventKeycode`
const FIELD_KEYCODE: u32 = 9;
/// `kCGEventSourceUserData`
const FIELD_SOURCE_USER_DATA: u32 = 42;

/// How often a tap that could not be made is tried again.
const RETRY: Duration = Duration::from_secs(2);

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    safe fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(cf: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    safe fn AXIsProcessTrusted() -> bool;
}

/// Stops the tap's run loop so [`run`] remakes the tap for a new hotkey, or ends.
#[derive(Default)]
pub(crate) struct Waker(Mutex<Option<RunLoop>>);

struct RunLoop(CFRunLoopRef);

// SAFETY: a CFRunLoop may be stopped from any thread.
unsafe impl Send for RunLoop {}

impl Waker {
    pub(crate) fn wake(&self) {
        if let Some(run_loop) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            // SAFETY: the run loop lives as long as its thread, which clears this first.
            unsafe { CFRunLoopStop(run_loop.0) };
        }
    }

    fn set(&self, run_loop: Option<RunLoop>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = run_loop;
    }
}

pub(crate) fn can_see_keys() -> bool {
    AXIsProcessTrusted() || CGPreflightPostEventAccess()
}

/// What the tap callback works with.
struct Context<'a> {
    shared: &'a Shared,
    events: &'a mut dyn FnMut(Event) -> bool,
    port: CFMachPortRef,
    /// `events` wants no more.
    done: bool,
}

pub(crate) fn run(shared: &Shared, events: &mut dyn FnMut(Event) -> bool) -> io::Result<()> {
    loop {
        let snapshot = shared.snapshot();
        if snapshot.stop {
            return Ok(());
        }
        let filters = snapshot
            .hotkey
            .as_ref()
            .is_some_and(|h| !h.is_modifier_only());
        // Without Accessibility a tap would make macOS ask for Input Monitoring too; ask for one.
        if !can_see_keys() {
            shared.set_status(Status::NoAccess);
            thread::sleep(RETRY);
            continue;
        }
        let mut context = Context {
            shared,
            events: &mut *events,
            port: std::ptr::null_mut(),
            done: false,
        };
        let interest = (1 << KEY_DOWN) | (1 << KEY_UP) | (1 << FLAGS_CHANGED);
        // SAFETY: `context` outlives the tap, which is invalidated before this iteration ends.
        let port = unsafe {
            CGEventTapCreate(
                TAP_SESSION,
                HEAD_INSERT,
                if filters { TAP_ACTIVE } else { TAP_LISTEN_ONLY },
                interest,
                callback,
                (&raw mut context).cast(),
            )
        };
        if port.is_null() {
            shared.set_status(Status::NoAccess);
            thread::sleep(RETRY);
            continue;
        }
        shared.set_status(Status::Watching(if filters {
            Blocking::Yes
        } else {
            Blocking::PassThrough
        }));
        context.port = port;
        // SAFETY: the port is valid until invalidated below; source and run loop are this thread's.
        unsafe {
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
            shared.waker.set(Some(RunLoop(run_loop)));
            // A change before the run loop was known.
            let now = shared.snapshot();
            if now.stop || now.hotkey.as_ref().is_some_and(|h| !h.is_modifier_only()) != filters {
                CFRunLoopStop(run_loop);
            }
            CGEventTapEnable(port, true);
            CFRunLoopRun();
            shared.waker.set(None);
            CFMachPortInvalidate(port);
            CFRelease(source.cast_const());
            CFRelease(port.cast_const());
        }
        if context.done {
            return Ok(());
        }
    }
}

unsafe extern "C" fn callback(
    _proxy: *mut c_void,
    kind: u32,
    event: CGEventRef,
    context: *mut c_void,
) -> CGEventRef {
    // SAFETY: the `Context` given to CGEventTapCreate, alive while the tap is.
    let context = unsafe { &mut *context.cast::<Context<'_>>() };
    if matches!(kind, TAP_DISABLED_BY_TIMEOUT | TAP_DISABLED_BY_USER_INPUT) {
        // SAFETY: the tap's own port.
        unsafe { CGEventTapEnable(context.port, true) };
        return event;
    }
    // SAFETY: a live event of a kind the tap asked for.
    let (code, flags, repeat, source) = unsafe {
        (
            CGEventGetIntegerValueField(event, FIELD_KEYCODE),
            CGEventGetFlags(event),
            CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) != 0,
            CGEventGetIntegerValueField(event, FIELD_SOURCE_USER_DATA),
        )
    };
    // The keys this program presses itself (the paste chord, which goes out
    // while a latched dictation's stopping press still holds the hotkey).
    if source == SYNTHETIC {
        return event;
    }
    let code = u16::try_from(code).unwrap_or(u16::MAX);
    let Some((key, edge)) = edge(kind, code, flags, repeat) else {
        return event;
    };
    let held = held(flags).besides(key);
    let action = context.shared.edge(key, edge, held);
    if let Some(hotkey_event) = action.event
        && !(context.events)(hotkey_event)
    {
        context.done = true;
        // SAFETY: this callback runs on the tap's run loop.
        unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
    }
    if action.swallow {
        std::ptr::null_mut()
    } else {
        event
    }
}

/// The key and edge of a tap event; a modifier's edge comes from its flag.
fn edge(kind: u32, code: u16, flags: u64, repeat: bool) -> Option<(Option<Key>, Edge)> {
    let key = key(code);
    match kind {
        KEY_DOWN if repeat => Some((key, Edge::Repeat)),
        KEY_DOWN => Some((key, Edge::Down)),
        KEY_UP => Some((key, Edge::Up)),
        FLAGS_CHANGED => {
            let down = MODIFIERS
                .iter()
                .find(|&&(c, _, _)| c == code)
                .map(|&(_, _, bit)| flags & bit != 0)
                .or_else(|| (key == Some(Key::CapsLock)).then_some(flags & ALPHA_SHIFT != 0))?;
            Some((key, if down { Edge::Down } else { Edge::Up }))
        }
        _ => None,
    }
}

/// `kCGEventFlagMaskAlphaShift`: Caps Lock is on.
const ALPHA_SHIFT: u64 = 0x1_0000;

/// Modifier keys: `kVK_*` code, key, and the device-dependent flag
/// (`NX_DEVICE*KEYMASK`) that says it is down.
const MODIFIERS: [(u16, Key, u64); 9] = [
    (59, Key::Modifier(Modifier::Ctrl, Side::Left), 0x1),
    (62, Key::Modifier(Modifier::Ctrl, Side::Right), 0x2000),
    (58, Key::Modifier(Modifier::Alt, Side::Left), 0x20),
    (61, Key::Modifier(Modifier::Alt, Side::Right), 0x40),
    (56, Key::Modifier(Modifier::Shift, Side::Left), 0x2),
    (60, Key::Modifier(Modifier::Shift, Side::Right), 0x4),
    (55, Key::Modifier(Modifier::Super, Side::Left), 0x8),
    (54, Key::Modifier(Modifier::Super, Side::Right), 0x10),
    (63, Key::Modifier(Modifier::Fn, Side::Either), 0x80_0000),
];

/// `kCGEventFlagMask*`: some sources set only these, which then count as the left key.
const FAMILY_MASKS: [(Modifier, u64); 4] = [
    (Modifier::Ctrl, 0x4_0000),
    (Modifier::Alt, 0x8_0000),
    (Modifier::Shift, 0x2_0000),
    (Modifier::Super, 0x10_0000),
];

/// The modifier keys `flags` say are down.
fn held(flags: u64) -> Held {
    let held = MODIFIERS
        .iter()
        .filter(|&&(_, _, bit)| flags & bit != 0)
        .fold(Held::default(), |held, &(_, key, _)| held.with(key));
    FAMILY_MASKS.iter().fold(held, |held, &(modifier, mask)| {
        if flags & mask != 0 && !held.has(modifier, Side::Either) {
            held.with(Key::Modifier(modifier, Side::Left))
        } else {
            held
        }
    })
}

/// `kVK_ANSI_A` … `kVK_ANSI_Z`.
const LETTERS: [u16; 26] = [
    0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,
];
/// `kVK_ANSI_0` … `kVK_ANSI_9`.
const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
/// `kVK_F1` … `kVK_F20`; Mac keyboards have no F21–F24.
const FUNCTION: [u16; 20] = [
    122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90,
];
/// `kVK_ANSI_Keypad0` … `kVK_ANSI_Keypad9`.
const NUMPAD: [u16; 10] = [82, 83, 84, 85, 86, 87, 88, 89, 91, 92];
const NAMED: [(u16, Key); 31] = [
    (49, Key::Space),
    (48, Key::Tab),
    (36, Key::Enter),
    (53, Key::Escape),
    (51, Key::Backspace),
    (117, Key::Delete),
    // kVK_Help, where PC keyboards have Insert.
    (114, Key::Insert),
    (115, Key::Home),
    (119, Key::End),
    (116, Key::PageUp),
    (121, Key::PageDown),
    (126, Key::Up),
    (125, Key::Down),
    (123, Key::Left),
    (124, Key::Right),
    (27, Key::Minus),
    (24, Key::Equal),
    (33, Key::BracketLeft),
    (30, Key::BracketRight),
    (42, Key::Backslash),
    (41, Key::Semicolon),
    (39, Key::Quote),
    (50, Key::Backquote),
    (43, Key::Comma),
    (47, Key::Period),
    (44, Key::Slash),
    (57, Key::CapsLock),
    (69, Key::NumpadAdd),
    (78, Key::NumpadSubtract),
    (67, Key::NumpadMultiply),
    (75, Key::NumpadDivide),
];
/// The rest of the number pad.
const NAMED_PAD: [(u16, Key); 2] = [(65, Key::NumpadDecimal), (76, Key::NumpadEnter)];

fn table() -> impl Iterator<Item = (u16, Key)> {
    let letters = ('A'..='Z')
        .zip(LETTERS)
        .map(|(c, code)| (code, Key::Letter(c)));
    let digits = (0..=9).zip(DIGITS).map(|(d, code)| (code, Key::Digit(d)));
    let function = (1..=20).zip(FUNCTION).map(|(n, code)| (code, Key::F(n)));
    let numpad = (0..=9).zip(NUMPAD).map(|(n, code)| (code, Key::Numpad(n)));
    let modifiers = MODIFIERS.iter().map(|&(code, key, _)| (code, key));
    letters
        .chain(digits)
        .chain(function)
        .chain(numpad)
        .chain(modifiers)
        .chain(NAMED)
        .chain(NAMED_PAD)
}

fn key(code: u16) -> Option<Key> {
    table().find(|&(c, _)| c == code).map(|(_, key)| key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::Hotkey;
    use crate::matcher::Matcher;

    const RIGHT_CMD: u16 = 54;
    const LEFT_CMD: u16 = 55;
    const LEFT_SHIFT: u16 = 56;
    const LEFT_CTRL: u16 = 59;
    const C: u16 = 8;
    const SPACE: u16 = 49;

    /// Runs tap events through `edge`, `held` and a matcher, like the callback.
    fn events(hotkey: &str, taps: &[(u32, u16, u64)]) -> Vec<Event> {
        let mut matcher = Matcher::new(hotkey.parse::<Hotkey>().unwrap());
        taps.iter()
            .filter_map(|&(kind, code, flags)| {
                let (key, edge) = edge(kind, code, flags, false)?;
                let held = held(flags).besides(key);
                matcher.edge(key, edge, held).event
            })
            .collect()
    }

    #[test]
    fn right_command_held_and_let_go() {
        let down = (FLAGS_CHANGED, RIGHT_CMD, 0x10_0000 | 0x10);
        let up = (FLAGS_CHANGED, RIGHT_CMD, 0);
        assert_eq!(
            events("RightCmd", &[down, up, down, up]),
            [
                Event::Pressed,
                Event::Released,
                Event::Pressed,
                Event::Released
            ]
        );
        let left = [
            (FLAGS_CHANGED, LEFT_CMD, 0x10_0000 | 0x8),
            (FLAGS_CHANGED, LEFT_CMD, 0),
        ];
        assert_eq!(events("RightCmd", &left), []);
    }

    #[test]
    fn a_shortcut_on_right_command_is_a_chord() {
        let taps = [
            (FLAGS_CHANGED, RIGHT_CMD, 0x10_0000 | 0x10),
            (KEY_DOWN, C, 0x10_0000 | 0x10),
            (KEY_UP, C, 0x10_0000 | 0x10),
            (FLAGS_CHANGED, RIGHT_CMD, 0),
        ];
        assert_eq!(events("RightCmd", &taps), [Event::Pressed, Event::Chord]);
        let shift_first = [
            (FLAGS_CHANGED, LEFT_SHIFT, 0x2_0000 | 0x2),
            (FLAGS_CHANGED, RIGHT_CMD, 0x10_0000 | 0x2_0000 | 0x12),
        ];
        assert_eq!(events("RightCmd", &shift_first), [], "Shift held");
    }

    #[test]
    fn option_space_needs_option_and_flags_without_sides_count() {
        let option = 0x8_0000 | 0x20;
        let taps = [(KEY_DOWN, SPACE, option), (KEY_UP, SPACE, option)];
        assert_eq!(
            events("Option+Space", &taps),
            [Event::Pressed, Event::Released]
        );
        assert_eq!(events("Option+Space", &[(KEY_DOWN, SPACE, 0)]), []);
        // Some sources set only the family's flag.
        assert_eq!(
            events("Ctrl+Space", &[(KEY_DOWN, SPACE, 0x4_0000)]),
            [Event::Pressed]
        );
        let ctrl = [(FLAGS_CHANGED, LEFT_CTRL, 0x4_0000 | 0x1)];
        assert_eq!(events("LeftCtrl", &ctrl), [Event::Pressed]);
    }

    #[test]
    fn every_key_but_the_pc_only_ones_has_a_mac_code_that_maps_back() {
        for key in Key::all() {
            let code = table().find(|&(_, k)| k == key).map(|(code, _)| code);
            let pc_only = matches!(
                key,
                Key::F(21..=24) | Key::PrintScreen | Key::ScrollLock | Key::Pause | Key::Menu
            );
            assert_eq!(code.is_none(), pc_only, "{key:?}");
            if let Some(code) = code {
                assert_eq!(super::key(code), Some(key));
            }
        }
    }
}
