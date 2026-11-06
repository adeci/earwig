//! Windows: a low-level keyboard hook (`WH_KEYBOARD_LL`) watches for
//! [`HotkeyConfig::key`]. It sees every key edge before any app does, which
//! `RegisterHotKey` cannot (no releases, no lone modifiers), and needs no
//! permission. The control pipe works as well (`earwig press` etc.).
//!
//! A lone modifier (the default, Right Ctrl) passes through to the apps; if
//! another key goes down while it is held, the user meant a shortcut
//! (Right Ctrl+C) and the source sends [`Command::Chord`]. A chord's key
//! (Ctrl+Space) is swallowed so the focused app never sees it.

use std::sync::{Mutex, PoisonError};

use anyhow::Context as _;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, SendInput, VK_ESCAPE, VK_F1, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SPACE, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_QUIT, WM_SYSKEYDOWN,
};

use super::HotkeySource;
use super::trigger::{Key, Modifier, ParseError, Side, Trigger};
use crate::config::HotkeyConfig;
use crate::desktop::DesktopCaps;
use crate::events::Command;

/// `dwExtraInfo` of the keys earwig sends itself (the paste chord, the masks
/// below), which the hook lets pass unseen.
pub(crate) const INJECTED: usize = 0xea51_9000;

/// An unassigned virtual key. Sent between a swallowed key and the release
/// of Alt or Win so that release does not open the menu bar or Start.
const MASK: u16 = 0xe8;

/// The trigger the hook matches and its state; [`configure`] replaces it.
static WATCH: Mutex<Option<Matcher>> = Mutex::new(None);
/// Where the hook sends commands, while it runs.
static COMMANDS: Mutex<Option<flume::Sender<Command>>> = Mutex::new(None);

pub(super) fn sources(config: &HotkeyConfig, _caps: &DesktopCaps) -> Vec<Box<dyn HotkeySource>> {
    if let Err(e) = configure(config) {
        log::error!("hotkey: {e}");
    }
    vec![Box::new(KeyboardHook)]
}

/// Watch for `config.key` from now on; a key that does not parse disables
/// the hotkey until it is fixed.
pub(super) fn configure(config: &HotkeyConfig) -> Result<(), ParseError> {
    let parsed = config
        .key
        .parse::<Trigger>()
        .and_then(|trigger| Keys::new(&trigger));
    let mut watch = WATCH.lock().unwrap_or_else(PoisonError::into_inner);
    match parsed {
        Ok(keys) => {
            if watch.as_ref().is_none_or(|matcher| matcher.keys != keys) {
                log::info!("hotkey: {}", config.key);
                *watch = Some(Matcher::new(keys));
            }
            Ok(())
        }
        Err(e) => {
            *watch = None;
            Err(e)
        }
    }
}

struct KeyboardHook;

impl HotkeySource for KeyboardHook {
    fn name(&self) -> &'static str {
        "keyboard-hook"
    }

    fn run(self: Box<Self>, commands: flume::Sender<Command>) -> anyhow::Result<()> {
        *COMMANDS.lock().unwrap_or_else(PoisonError::into_inner) = Some(commands);
        // SAFETY: a global hook needs no module handle when the callback
        // lives in this process; it runs on this thread's message loop.
        let hook =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), std::ptr::null_mut(), 0) };
        if hook.is_null() {
            return Err(std::io::Error::last_os_error()).context("installing the keyboard hook");
        }
        // SAFETY: plain message loop; `msg` is written by GetMessageW.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&raw const msg);
            }
            UnhookWindowsHookEx(hook);
        }
        COMMANDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Ok(())
    }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for HC_ACTION, lparam points at the event (WH_KEYBOARD_LL).
    let event =
        (code == HC_ACTION.cast_signed()).then(|| unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) });
    if let Some(event) = event.filter(|event| event.dwExtraInfo != INJECTED)
        && let Ok(vk) = u16::try_from(event.vkCode)
    {
        let down = matches!(u32::try_from(wparam), Ok(WM_KEYDOWN | WM_SYSKEYDOWN));
        let action = WATCH
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .map(|matcher| matcher.key(vk, down, held))
            .unwrap_or_default();
        if let Some(command) = action.command {
            let sent = COMMANDS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(|commands| commands.send(command).is_ok());
            if !sent {
                // The daemon is gone: end the message loop.
                // SAFETY: posting to this (the hook's) thread.
                unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_QUIT, 0, 0) };
            }
        }
        match action.mask {
            Mask::None => {}
            Mask::After => send(&[(MASK, false), (MASK, true)]),
            Mask::Release(vk) => send(&[(MASK, false), (MASK, true), (vk, true)]),
        }
        if action.swallow {
            return 1;
        }
    }
    // SAFETY: passing the event on, as every hook must.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Whether `vk` is down right now (before the event being hooked).
fn held(vk: u16) -> bool {
    // SAFETY: no preconditions.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    state < 0
}

/// Send key edges marked as earwig's own.
fn send(keys: &[(u16, bool)]) {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, up)| {
            let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
            if matches!(vk, VK_RCONTROL | VK_RMENU | VK_LWIN | VK_RWIN) {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED,
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

/// One modifier as virtual keys: either side, or just one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModKeys {
    left: u16,
    right: u16,
    side: Side,
}

impl ModKeys {
    fn new(modifier: Modifier, side: Side) -> Result<Self, ParseError> {
        let (left, right) = match modifier {
            Modifier::Ctrl => (VK_LCONTROL, VK_RCONTROL),
            Modifier::Alt => (VK_LMENU, VK_RMENU),
            Modifier::Shift => (VK_LSHIFT, VK_RSHIFT),
            Modifier::Super => (VK_LWIN, VK_RWIN),
            Modifier::Fn => return Err(ParseError::Unknown("Fn".into())),
        };
        Ok(Self { left, right, side })
    }

    fn matches(self, vk: u16) -> bool {
        match self.side {
            Side::Left => vk == self.left,
            Side::Right => vk == self.right,
            Side::Either => vk == self.left || vk == self.right,
        }
    }

    fn is_held(self, held: impl Fn(u16) -> bool) -> bool {
        match self.side {
            Side::Left => held(self.left),
            Side::Right => held(self.right),
            Side::Either => held(self.left) || held(self.right),
        }
    }

    /// Releasing Alt or Win on its own opens the menu bar or Start.
    fn needs_mask(self) -> bool {
        matches!(self.left, VK_LMENU | VK_LWIN)
    }
}

/// Every modifier family, to check that no unwanted one is held.
const FAMILIES: [(u16, u16); 4] = [
    (VK_LCONTROL, VK_RCONTROL),
    (VK_LMENU, VK_RMENU),
    (VK_LSHIFT, VK_RSHIFT),
    (VK_LWIN, VK_RWIN),
];

/// A [`Trigger`] as virtual keys.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Keys {
    /// Modifiers that must be held besides the trigger key.
    with: Vec<ModKeys>,
    trigger: TriggerKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriggerKey {
    /// A lone modifier: it passes through, the hotkey watches it.
    Modifier(ModKeys),
    /// A key, swallowed while it triggers.
    Key(u16),
}

impl Keys {
    fn new(trigger: &Trigger) -> Result<Self, ParseError> {
        let mut with = trigger
            .modifiers
            .iter()
            .map(|&(modifier, side)| ModKeys::new(modifier, side))
            .collect::<Result<Vec<_>, _>>()?;
        let trigger = match trigger.key {
            Some(key) => TriggerKey::Key(vk(key)),
            None => TriggerKey::Modifier(with.pop().ok_or(ParseError::Empty)?),
        };
        Ok(Self { with, trigger })
    }

    /// The required modifiers are held, and no other one (the trigger's
    /// own family aside).
    fn modifiers_match(&self, held: impl Fn(u16) -> bool + Copy) -> bool {
        let own = match self.trigger {
            TriggerKey::Modifier(keys) => Some(keys),
            TriggerKey::Key(_) => None,
        };
        self.with.iter().all(|keys| keys.is_held(held))
            && FAMILIES.iter().all(|&(left, right)| {
                let wanted = self
                    .with
                    .iter()
                    .chain(own.as_ref())
                    .any(|keys| keys.left == left);
                wanted || !(held(left) || held(right))
            })
    }

    fn needs_mask(&self) -> bool {
        self.with
            .iter()
            .chain(match &self.trigger {
                TriggerKey::Modifier(keys) => Some(keys),
                TriggerKey::Key(_) => None,
            })
            .any(|keys| keys.needs_mask())
    }
}

fn vk(key: Key) -> u16 {
    match key {
        Key::Letter(c) => u16::try_from(u32::from(c)).unwrap_or(0),
        Key::Digit(d) => u16::from(b'0' + d),
        Key::Space => VK_SPACE,
        Key::Tab => VK_TAB,
        Key::Escape => VK_ESCAPE,
        Key::Return => VK_RETURN,
        Key::F(n) => VK_F1 + u16::from(n) - 1,
    }
}

/// What the hook does with one key edge.
#[derive(Debug, Default, PartialEq)]
struct Action {
    command: Option<Command>,
    /// Keep the edge from the focused app.
    swallow: bool,
    mask: Mask,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum Mask {
    #[default]
    None,
    /// Send the mask key now.
    After,
    /// The edge, a release of `vk`, was swallowed: send the mask key, then
    /// the release.
    Release(u16),
}

/// Turns the key edges the hook sees into commands.
#[derive(Debug)]
struct Matcher {
    keys: Keys,
    /// The trigger is down and sent [`Command::Press`].
    active: bool,
    /// Another key went down while the lone modifier was held.
    chorded: bool,
}

impl Matcher {
    fn new(keys: Keys) -> Self {
        Self {
            keys,
            active: false,
            chorded: false,
        }
    }

    /// `held` reports keys that are down, not counting this edge.
    fn key(&mut self, vk: u16, down: bool, held: impl Fn(u16) -> bool + Copy) -> Action {
        match self.keys.trigger {
            TriggerKey::Modifier(trigger) => self.modifier(trigger, vk, down, held),
            TriggerKey::Key(key) => self.chord(key, vk, down, held),
        }
    }

    fn modifier(
        &mut self,
        trigger: ModKeys,
        vk: u16,
        down: bool,
        held: impl Fn(u16) -> bool + Copy,
    ) -> Action {
        let is_trigger = trigger.matches(vk);
        match (self.active, down) {
            (false, true) if is_trigger && self.keys.modifiers_match(held) => {
                self.active = true;
                self.chorded = false;
                command(Command::Press)
            }
            // Auto-repeat.
            (true, true) if is_trigger => Action::default(),
            (true, true) if !self.chorded => {
                self.chorded = true;
                command(Command::Chord)
            }
            (true, false) if is_trigger => {
                self.active = false;
                if self.chorded {
                    // The session already let go of this press.
                    return Action::default();
                }
                let mut action = command(Command::Release);
                if !self.keys.needs_mask() {
                    return action;
                }
                action.swallow = true;
                action.mask = Mask::Release(vk);
                action
            }
            _ => Action::default(),
        }
    }

    fn chord(
        &mut self,
        key: u16,
        vk: u16,
        down: bool,
        held: impl Fn(u16) -> bool + Copy,
    ) -> Action {
        if vk != key {
            return Action::default();
        }
        match (self.active, down) {
            (false, true) if self.keys.modifiers_match(held) => {
                self.active = true;
                Action {
                    command: Some(Command::Press),
                    swallow: true,
                    mask: if self.keys.needs_mask() {
                        Mask::After
                    } else {
                        Mask::None
                    },
                }
            }
            (true, true) => Action {
                swallow: true,
                ..Action::default()
            },
            (true, false) => {
                self.active = false;
                Action {
                    command: Some(Command::Release),
                    swallow: true,
                    mask: Mask::None,
                }
            }
            _ => Action::default(),
        }
    }
}

fn command(command: Command) -> Action {
    Action {
        command: Some(command),
        ..Action::default()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Feeds edges the way the hook sees them, keeping track of what is held.
    struct Keyboard {
        matcher: Matcher,
        down: RefCell<Vec<u16>>,
    }

    impl Keyboard {
        fn new(key: &str) -> Self {
            let trigger = key.parse().unwrap();
            Self {
                matcher: Matcher::new(Keys::new(&trigger).unwrap()),
                down: RefCell::default(),
            }
        }

        fn edge(&mut self, vk: u16, down: bool) -> Action {
            let held = self.down.clone().into_inner();
            let action = self.matcher.key(vk, down, |k| held.contains(&k));
            let mut keys = self.down.borrow_mut();
            keys.retain(|&k| k != vk);
            if down {
                keys.push(vk);
            }
            action
        }

        /// Commands sent for a sequence of edges.
        fn commands(&mut self, edges: &[(u16, bool)]) -> Vec<Command> {
            edges
                .iter()
                .filter_map(|&(vk, down)| self.edge(vk, down).command)
                .collect()
        }
    }

    const A: u16 = b'A' as u16;
    const C: u16 = b'C' as u16;

    #[test]
    fn a_lone_modifier_presses_and_releases_and_passes_through() {
        let mut keyboard = Keyboard::new("RightCtrl");
        let press = keyboard.edge(VK_RCONTROL, true);
        assert_eq!(press.command, Some(Command::Press));
        assert!(!press.swallow);
        assert_eq!(
            keyboard.edge(VK_RCONTROL, true),
            Action::default(),
            "repeat"
        );
        let release = keyboard.edge(VK_RCONTROL, false);
        assert_eq!(release.command, Some(Command::Release));
        assert!(!release.swallow);
    }

    #[test]
    fn the_other_side_is_not_the_trigger() {
        let mut keyboard = Keyboard::new("RightCtrl");
        assert_eq!(
            keyboard.commands(&[(VK_LCONTROL, true), (VK_LCONTROL, false)]),
            []
        );
    }

    #[test]
    fn a_key_during_the_hold_makes_it_a_shortcut() {
        let mut keyboard = Keyboard::new("RightCtrl");
        let commands = keyboard.commands(&[
            (VK_RCONTROL, true),
            (C, true),
            (C, false),
            (A, true),
            (A, false),
            (VK_RCONTROL, false),
        ]);
        assert_eq!(commands, [Command::Press, Command::Chord]);
    }

    #[test]
    fn a_lone_modifier_needs_its_companions_and_no_others() {
        let mut keyboard = Keyboard::new("Shift+RightAlt");
        assert_eq!(
            keyboard.commands(&[(VK_RMENU, true), (VK_RMENU, false)]),
            []
        );
        assert_eq!(
            keyboard.commands(&[(VK_LSHIFT, true), (VK_RMENU, true)]),
            [Command::Press]
        );
        let mut keyboard = Keyboard::new("RightCtrl");
        assert_eq!(
            keyboard.commands(&[(VK_LSHIFT, true), (VK_RCONTROL, true)]),
            []
        );
        // Ctrl on the left does not hold off Right Ctrl.
        let mut keyboard = Keyboard::new("RightCtrl");
        assert_eq!(
            keyboard.commands(&[(VK_LCONTROL, true), (VK_RCONTROL, true)]),
            [Command::Press]
        );
    }

    #[test]
    fn releasing_a_lone_alt_or_win_is_masked() {
        let mut keyboard = Keyboard::new("RightAlt");
        keyboard.edge(VK_RMENU, true);
        let release = keyboard.edge(VK_RMENU, false);
        assert_eq!(release.command, Some(Command::Release));
        assert!(release.swallow);
        assert_eq!(release.mask, Mask::Release(VK_RMENU));
        // After a shortcut the other key already did that, and the session
        // let go of the press.
        keyboard.commands(&[(VK_RMENU, true), (A, true), (A, false)]);
        assert_eq!(keyboard.edge(VK_RMENU, false), Action::default());
    }

    #[test]
    fn a_chord_swallows_its_key_and_its_repeats() {
        let mut keyboard = Keyboard::new("Ctrl+Space");
        assert_eq!(keyboard.edge(VK_SPACE, true), Action::default(), "no Ctrl");
        keyboard.edge(VK_SPACE, false);
        keyboard.edge(VK_LCONTROL, true);
        let press = keyboard.edge(VK_SPACE, true);
        assert_eq!(press.command, Some(Command::Press));
        assert!(press.swallow);
        assert_eq!(press.mask, Mask::None);
        let repeat = keyboard.edge(VK_SPACE, true);
        assert_eq!(repeat.command, None);
        assert!(repeat.swallow);
        // Letting go of Ctrl first still ends on the key's release.
        keyboard.edge(VK_LCONTROL, false);
        let release = keyboard.edge(VK_SPACE, false);
        assert_eq!(release.command, Some(Command::Release));
        assert!(release.swallow);
    }

    #[test]
    fn a_chord_wants_exactly_its_modifiers() {
        let mut keyboard = Keyboard::new("Ctrl+Space");
        assert_eq!(
            keyboard.commands(&[(VK_LCONTROL, true), (VK_LSHIFT, true), (VK_SPACE, true)]),
            []
        );
        let mut keyboard = Keyboard::new("Alt+Shift+F9");
        let edges = [(VK_RMENU, true), (VK_LSHIFT, true)];
        keyboard.commands(&edges);
        let press = keyboard.edge(VK_F1 + 8, true);
        assert_eq!(press.command, Some(Command::Press));
        assert_eq!(
            press.mask,
            Mask::After,
            "Alt is released after a swallowed key"
        );
    }

    #[test]
    fn fn_is_not_a_windows_key() {
        let trigger = "Fn".parse().unwrap();
        assert!(Keys::new(&trigger).is_err());
    }

    #[test]
    fn letters_and_digits_are_their_virtual_keys() {
        assert_eq!(vk(Key::Letter('X')), u16::from(b'X'));
        assert_eq!(vk(Key::Digit(0)), u16::from(b'0'));
        assert_eq!(vk(Key::F(24)), VK_F1 + 23);
    }
}
