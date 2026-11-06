//! macOS: a `CGEventTap` watches for [`HotkeyConfig::key`], Right Command
//! by default. Seeing keys needs Accessibility access (which spares asking
//! for Input Monitoring); until the user grants it the tap cannot be made,
//! and the source keeps trying. The control socket works as well (`earwig
//! press` etc.).
//!
//! A lone modifier passes through to the apps (the tap only listens); if
//! another key goes down while it is held, the user meant a shortcut (Right
//! Command+C) and the source sends [`Command::Chord`]. A chord's key
//! (Ctrl+Space) is swallowed so the focused app never sees it, with a
//! filtering tap.

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use super::HotkeySource;
use super::trigger::{Key, Modifier, ParseError, Side, Trigger};
use crate::config::HotkeyConfig;
use crate::desktop::DesktopCaps;
use crate::events::Command;
use crate::macos::{self, CFRunLoopRef, CGEventRef};
use crate::permissions::{self, Permission};

/// How often a tap that could not be made is tried again.
const RETRY: Duration = Duration::from_secs(2);

/// The trigger the tap matches and its state; [`configure`] replaces it.
static WATCH: Mutex<Option<Matcher>> = Mutex::new(None);
/// The tap's run loop while it runs, stopped to rebuild the tap when the
/// trigger needs the other kind (listening or filtering).
static TAP: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    run_loop: CFRunLoopRef,
    filters: bool,
}

// SAFETY: a CFRunLoop may be stopped from any thread.
unsafe impl Send for Running {}

pub(super) fn sources(config: &HotkeyConfig, _caps: &DesktopCaps) -> Vec<Box<dyn HotkeySource>> {
    if let Err(e) = configure(config) {
        log::error!("hotkey: {e}");
    }
    vec![Box::new(EventTap)]
}

/// Watch for `config.key` from now on; a key that does not parse disables
/// the hotkey until it is fixed.
pub(super) fn configure(config: &HotkeyConfig) -> Result<(), ParseError> {
    let parsed = config
        .key
        .parse::<Trigger>()
        .and_then(|trigger| Binding::new(&trigger));
    let mut watch = WATCH.lock().unwrap_or_else(PoisonError::into_inner);
    let result = match parsed {
        Ok(binding) => {
            if watch
                .as_ref()
                .is_none_or(|matcher| matcher.binding != binding)
            {
                log::info!("hotkey: {}", config.key);
                *watch = Some(Matcher::new(binding));
            }
            Ok(())
        }
        Err(e) => {
            *watch = None;
            Err(e)
        }
    };
    let filters = watch.as_ref().is_some_and(Matcher::filters);
    drop(watch);
    if let Some(tap) = TAP.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        && tap.filters != filters
    {
        // SAFETY: the run loop lives as long as its thread, which clears
        // `TAP` before it ends.
        unsafe { macos::CFRunLoopStop(tap.run_loop) };
    }
    result
}

struct EventTap;

/// What the tap callback works with.
struct Context {
    commands: flume::Sender<Command>,
    port: macos::CFMachPortRef,
}

impl HotkeySource for EventTap {
    fn name(&self) -> &'static str {
        "event-tap"
    }

    fn run(self: Box<Self>, commands: flume::Sender<Command>) -> anyhow::Result<()> {
        let mut waiting = false;
        while !commands.is_disconnected() {
            let filters = WATCH
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(Matcher::filters);
            let context = Box::into_raw(Box::new(Context {
                commands: commands.clone(),
                port: std::ptr::null_mut(),
            }));
            let events =
                (1 << macos::KEY_DOWN) | (1 << macos::KEY_UP) | (1 << macos::FLAGS_CHANGED);
            // SAFETY: `context` outlives the tap: it is freed only after the
            // tap is invalidated below.
            let port = unsafe {
                macos::CGEventTapCreate(
                    macos::TAP_SESSION,
                    macos::HEAD_INSERT,
                    if filters {
                        macos::TAP_ACTIVE
                    } else {
                        macos::TAP_LISTEN_ONLY
                    },
                    events,
                    callback,
                    context.cast(),
                )
            };
            if port.is_null() {
                // SAFETY: never handed to a tap.
                drop(unsafe { Box::from_raw(context) });
                if !std::mem::replace(&mut waiting, true) {
                    log::warn!(
                        "hotkey: {}",
                        permissions::missing_message(Permission::Accessibility)
                    );
                }
                thread::sleep(RETRY);
                continue;
            }
            if std::mem::take(&mut waiting) {
                log::info!("hotkey: access granted, watching the key");
            }
            // SAFETY: the port is valid until invalidated below; the source
            // and run loop belong to this thread.
            unsafe {
                (*context).port = port;
                let source = macos::CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
                let run_loop = macos::CFRunLoopGetCurrent();
                macos::CFRunLoopAddSource(run_loop, source, macos::kCFRunLoopCommonModes);
                *TAP.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Running { run_loop, filters });
                macos::CGEventTapEnable(port, true);
                macos::CFRunLoopRun();
                TAP.lock().unwrap_or_else(PoisonError::into_inner).take();
                macos::CFMachPortInvalidate(port);
                macos::CFRelease(source.cast_const());
                macos::CFRelease(port.cast_const());
                drop(Box::from_raw(context));
            }
        }
        Ok(())
    }
}

unsafe extern "C" fn callback(
    _proxy: *mut c_void,
    kind: u32,
    event: CGEventRef,
    context: *mut c_void,
) -> CGEventRef {
    // SAFETY: the `Context` given to CGEventTapCreate, alive while the tap is.
    let context = unsafe { &*context.cast::<Context>() };
    if matches!(
        kind,
        macos::TAP_DISABLED_BY_TIMEOUT | macos::TAP_DISABLED_BY_USER_INPUT
    ) {
        // SAFETY: the tap's own port.
        unsafe { macos::CGEventTapEnable(context.port, true) };
        return event;
    }
    // SAFETY: a live event of a kind the tap asked for.
    let (code, flags, repeat, source) = unsafe {
        (
            macos::CGEventGetIntegerValueField(event, macos::FIELD_KEYCODE),
            macos::CGEventGetFlags(event),
            macos::CGEventGetIntegerValueField(event, macos::FIELD_AUTOREPEAT) != 0,
            macos::CGEventGetIntegerValueField(event, macos::FIELD_SOURCE_USER_DATA),
        )
    };
    let Some(edge) = edge(kind, repeat, source) else {
        return event;
    };
    let action = WATCH
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .map(|matcher| matcher.event(edge, u16::try_from(code).unwrap_or(u16::MAX), flags))
        .unwrap_or_default();
    if let Some(command) = action.command
        && context.commands.send(command).is_err()
    {
        // The daemon is going away.
        // SAFETY: this callback runs on the tap's run loop.
        unsafe { macos::CFRunLoopStop(macos::CFRunLoopGetCurrent()) };
    }
    if action.swallow {
        std::ptr::null_mut()
    } else {
        event
    }
}

/// The edge a tap event is, or `None` for the keys earwig presses itself
/// (the paste chord, which goes out while a latched dictation's stopping
/// press still holds the trigger).
fn edge(kind: u32, repeat: bool, source: i64) -> Option<Edge> {
    if source == macos::EARWIG_EVENT {
        return None;
    }
    Some(match kind {
        macos::KEY_DOWN if repeat => Edge::Repeat,
        macos::KEY_DOWN => Edge::Down,
        macos::KEY_UP => Edge::Up,
        _ => Edge::Flags,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Down,
    Repeat,
    Up,
    /// A modifier went down or up (`kCGEventFlagsChanged`).
    Flags,
}

/// `kCGEventFlagMask*` of each modifier family, and the key code and
/// device-dependent flag (`NX_DEVICE*KEYMASK`) of its left and right key.
struct Family {
    mask: u64,
    left: (u16, u64),
    right: (u16, u64),
}

const fn family(modifier: Modifier) -> Family {
    match modifier {
        Modifier::Ctrl => Family {
            mask: 0x4_0000,
            left: (59, 0x1),
            right: (62, 0x2000),
        },
        Modifier::Shift => Family {
            mask: 0x2_0000,
            left: (56, 0x2),
            right: (60, 0x4),
        },
        Modifier::Alt => Family {
            mask: 0x8_0000,
            left: (58, 0x20),
            right: (61, 0x40),
        },
        Modifier::Super => Family {
            mask: 0x10_0000,
            left: (55, 0x8),
            right: (54, 0x10),
        },
        Modifier::Fn => Family {
            mask: 0x80_0000,
            left: (63, 0x80_0000),
            right: (63, 0x80_0000),
        },
    }
}

/// Families that must not be held unless the trigger names them. Fn is
/// left out: macOS sets it on arrow and function keys by itself.
const FAMILIES: [Modifier; 4] = [
    Modifier::Ctrl,
    Modifier::Shift,
    Modifier::Alt,
    Modifier::Super,
];

/// One modifier of a trigger: either side, or just one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModKey {
    modifier: Modifier,
    side: Side,
}

impl ModKey {
    fn is_key(self, code: u16) -> bool {
        let family = family(self.modifier);
        match self.side {
            Side::Left => code == family.left.0,
            Side::Right => code == family.right.0,
            Side::Either => code == family.left.0 || code == family.right.0,
        }
    }

    fn is_held(self, flags: u64) -> bool {
        let family = family(self.modifier);
        let bits = match self.side {
            Side::Left => family.left.1,
            Side::Right => family.right.1,
            Side::Either => family.mask,
        };
        flags & bits != 0
    }
}

/// Whether the modifier key `code` is down after a flags change.
fn modifier_down(code: u16, flags: u64) -> Option<bool> {
    [
        Modifier::Ctrl,
        Modifier::Shift,
        Modifier::Alt,
        Modifier::Super,
        Modifier::Fn,
    ]
    .into_iter()
    .map(family)
    .find_map(|family| {
        [family.left, family.right]
            .into_iter()
            .find(|(key, _)| *key == code)
            .map(|(_, bit)| flags & bit != 0)
    })
}

/// A [`Trigger`] as macOS key codes and flags.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    /// Modifiers that must be held besides the trigger key.
    with: Vec<ModKey>,
    trigger: TriggerKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TriggerKey {
    /// A lone modifier: it passes through, the tap watches it.
    Modifier(ModKey),
    /// A key code, swallowed while it triggers.
    Key(u16),
}

impl Binding {
    fn new(trigger: &Trigger) -> Result<Self, ParseError> {
        let mut with: Vec<ModKey> = trigger
            .modifiers
            .iter()
            .map(|&(modifier, side)| ModKey { modifier, side })
            .collect();
        let trigger = match trigger.key {
            Some(key) => TriggerKey::Key(key_code(key)?),
            None => TriggerKey::Modifier(with.pop().ok_or(ParseError::Empty)?),
        };
        Ok(Self { with, trigger })
    }

    /// The required modifiers are held, and no other one (the trigger's
    /// own family aside).
    fn modifiers_match(&self, flags: u64) -> bool {
        let own = match self.trigger {
            TriggerKey::Modifier(key) => Some(key.modifier),
            TriggerKey::Key(_) => None,
        };
        self.with.iter().all(|key| key.is_held(flags))
            && FAMILIES.into_iter().all(|modifier| {
                own == Some(modifier)
                    || self.with.iter().any(|key| key.modifier == modifier)
                    || flags & family(modifier).mask == 0
            })
    }
}

/// `kVK_*` codes, as on an ANSI keyboard.
fn key_code(key: Key) -> Result<u16, ParseError> {
    const LETTERS: [u16; 26] = [
        0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16,
        6,
    ];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    const FUNCTION: [u16; 20] = [
        122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90,
    ];
    let code = match key {
        Key::Letter(c) => u32::from(c)
            .checked_sub(u32::from('A'))
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| LETTERS.get(i).copied()),
        Key::Digit(d) => DIGITS.get(usize::from(d)).copied(),
        Key::Space => Some(49),
        Key::Tab => Some(48),
        Key::Escape => Some(53),
        Key::Return => Some(36),
        Key::F(n) => FUNCTION.get(usize::from(n).wrapping_sub(1)).copied(),
    };
    code.ok_or_else(|| ParseError::Unknown(format!("{key:?} (no such key on a Mac)")))
}

/// What the tap does with one event.
#[derive(Debug, Default, PartialEq)]
struct Action {
    command: Option<Command>,
    /// Keep the event from the focused app.
    swallow: bool,
}

fn command(command: Command) -> Action {
    Action {
        command: Some(command),
        swallow: false,
    }
}

/// Turns the key events the tap sees into commands.
#[derive(Debug)]
struct Matcher {
    binding: Binding,
    /// The trigger is down and sent [`Command::Press`].
    active: bool,
    /// Another key went down while the lone modifier was held.
    chorded: bool,
}

impl Matcher {
    fn new(binding: Binding) -> Self {
        Self {
            binding,
            active: false,
            chorded: false,
        }
    }

    /// Whether the tap must be able to swallow events.
    fn filters(&self) -> bool {
        matches!(self.binding.trigger, TriggerKey::Key(_))
    }

    fn event(&mut self, edge: Edge, code: u16, flags: u64) -> Action {
        match self.binding.trigger {
            TriggerKey::Modifier(trigger) => self.modifier(trigger, edge, code, flags),
            TriggerKey::Key(key) => self.key(key, edge, code, flags),
        }
    }

    fn modifier(&mut self, trigger: ModKey, edge: Edge, code: u16, flags: u64) -> Action {
        match edge {
            Edge::Flags if trigger.is_key(code) => {
                let down = modifier_down(code, flags).unwrap_or(false);
                if down && !self.active && self.binding.modifiers_match(flags) {
                    self.active = true;
                    self.chorded = false;
                    command(Command::Press)
                } else if !down && self.active {
                    self.active = false;
                    if self.chorded {
                        // The session already let go of this press.
                        Action::default()
                    } else {
                        command(Command::Release)
                    }
                } else {
                    Action::default()
                }
            }
            Edge::Flags if modifier_down(code, flags) != Some(true) => Action::default(),
            Edge::Flags | Edge::Down | Edge::Repeat if self.active && !self.chorded => {
                self.chorded = true;
                command(Command::Chord)
            }
            _ => Action::default(),
        }
    }

    fn key(&mut self, key: u16, edge: Edge, code: u16, flags: u64) -> Action {
        if code != key {
            return Action::default();
        }
        let swallow = Action {
            command: None,
            swallow: true,
        };
        match (edge, self.active) {
            (Edge::Down, false) if self.binding.modifiers_match(flags) => {
                self.active = true;
                Action {
                    command: Some(Command::Press),
                    swallow: true,
                }
            }
            (Edge::Down | Edge::Repeat, true) => swallow,
            (Edge::Up, true) => {
                self.active = false;
                Action {
                    command: Some(Command::Release),
                    swallow: true,
                }
            }
            _ => Action::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT_CMD: u16 = 54;
    const LEFT_CMD: u16 = 55;
    const LEFT_SHIFT: u16 = 56;
    const C: u16 = 8;
    const V: u16 = 9;
    const SPACE: u16 = 49;
    const CMD: u64 = 0x10_0000;
    const CTRL: u64 = 0x4_0000;
    const SHIFT: u64 = 0x2_0000;

    fn matcher(key: &str) -> Matcher {
        Matcher::new(Binding::new(&key.parse().unwrap()).unwrap())
    }

    fn commands(matcher: &mut Matcher, events: &[(Edge, u16, u64)]) -> Vec<Command> {
        events
            .iter()
            .filter_map(|&(edge, code, flags)| matcher.event(edge, code, flags).command)
            .collect()
    }

    #[test]
    fn right_command_held_and_let_go() {
        let mut m = matcher("RightCmd");
        assert!(!m.filters());
        let down = (Edge::Flags, RIGHT_CMD, CMD | 0x10);
        let up = (Edge::Flags, RIGHT_CMD, 0);
        assert_eq!(
            commands(&mut m, &[down, up, down, up]),
            [
                Command::Press,
                Command::Release,
                Command::Press,
                Command::Release
            ]
        );
    }

    #[test]
    fn the_left_command_key_is_not_the_right_one() {
        let mut m = matcher("RightCmd");
        let events = [
            (Edge::Flags, LEFT_CMD, CMD | 0x8),
            (Edge::Flags, LEFT_CMD, 0),
        ];
        assert_eq!(commands(&mut m, &events), []);
    }

    #[test]
    fn a_shortcut_on_the_trigger_is_a_chord_and_passes_through() {
        let mut m = matcher("RightCmd");
        let events = [
            (Edge::Flags, RIGHT_CMD, CMD | 0x10),
            (Edge::Down, C, CMD | 0x10),
            (Edge::Up, C, CMD | 0x10),
            (Edge::Flags, RIGHT_CMD, 0),
        ];
        assert_eq!(commands(&mut m, &events), [Command::Press, Command::Chord]);
        assert!(!m.event(Edge::Down, C, CMD | 0x10).swallow);
    }

    #[test]
    fn another_modifier_going_down_is_a_chord_but_one_going_up_is_not() {
        let mut m = matcher("RightCmd");
        let events = [
            (Edge::Flags, LEFT_SHIFT, SHIFT | 0x2),
            (Edge::Flags, RIGHT_CMD, CMD | SHIFT | 0x12),
        ];
        // Shift already held: not this trigger.
        assert_eq!(commands(&mut m, &events), []);
        let mut m = matcher("RightCmd");
        let events = [
            (Edge::Flags, RIGHT_CMD, CMD | 0x10),
            (Edge::Flags, LEFT_SHIFT, CMD | SHIFT | 0x12),
            (Edge::Flags, LEFT_SHIFT, CMD | 0x10),
            (Edge::Flags, RIGHT_CMD, 0),
        ];
        assert_eq!(commands(&mut m, &events), [Command::Press, Command::Chord]);
    }

    #[test]
    fn a_key_trigger_needs_its_modifiers_and_is_swallowed() {
        let mut m = matcher("Ctrl+Space");
        assert!(m.filters());
        assert_eq!(m.event(Edge::Down, SPACE, 0), Action::default());
        assert_eq!(m.event(Edge::Up, SPACE, 0), Action::default());
        assert_eq!(m.event(Edge::Down, SPACE, CTRL | SHIFT).command, None);
        let press = m.event(Edge::Down, SPACE, CTRL | 0x1);
        assert_eq!(press.command, Some(Command::Press));
        assert!(press.swallow);
        let repeat = m.event(Edge::Repeat, SPACE, CTRL | 0x1);
        assert_eq!(repeat.command, None);
        assert!(repeat.swallow);
        // Ctrl may go up first.
        let release = m.event(Edge::Up, SPACE, 0);
        assert_eq!(release.command, Some(Command::Release));
        assert!(release.swallow);
    }

    #[test]
    fn the_paste_earwig_presses_while_the_trigger_is_held_is_no_chord() {
        let mut m = matcher("RightCmd");
        let mut feed = |kind, code, flags, source| {
            edge(kind, false, source).and_then(|edge| m.event(edge, code, flags).command)
        };
        let ours = macos::EARWIG_EVENT;
        let commands: Vec<Command> = [
            feed(macos::FLAGS_CHANGED, RIGHT_CMD, CMD | 0x10, 0),
            feed(macos::KEY_DOWN, V, CMD, ours),
            feed(macos::KEY_UP, V, CMD, ours),
            feed(macos::FLAGS_CHANGED, RIGHT_CMD, 0, 0),
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(commands, [Command::Press, Command::Release]);
        // The same keys from the keyboard are a shortcut.
        assert_eq!(
            feed(macos::FLAGS_CHANGED, RIGHT_CMD, CMD | 0x10, 0),
            Some(Command::Press)
        );
        assert_eq!(
            feed(macos::KEY_DOWN, V, CMD | 0x10, 0),
            Some(Command::Chord)
        );
    }

    #[test]
    fn every_parsable_key_has_a_mac_key_code() {
        for key in [
            "A", "Z", "0", "9", "F1", "F20", "Space", "Tab", "Escape", "Return",
        ] {
            assert!(Binding::new(&key.parse().unwrap()).is_ok(), "{key}");
        }
        assert!(Binding::new(&"F24".parse().unwrap()).is_err());
    }
}
