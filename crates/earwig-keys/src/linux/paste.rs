//! earwig's own keyboard for its paste chord, Shift+Insert, made through uinput once and kept: a
//! new device loses its first keys while the compositor takes it up. Without the digit row, so
//! keyd does not grab it; earwig's listener skips it as one of its own.

use std::io;

use rustix::fs::{Mode, OFlags};

use super::evdev::{self, EV_KEY, InputEvent, Uinput};

const KEY_LEFTSHIFT: u16 = 42;
const KEY_INSERT: u16 = 110;

/// Ctrl, Alt and Super, left and right: held by the user, they turn Shift+Insert into another
/// shortcut.
const MODIFIERS: [u16; 6] = [29, 97, 56, 100, 125, 126];

pub struct PasteKeyboard {
    uinput: Uinput,
}

impl PasteKeyboard {
    /// Fails without write access to `/dev/uinput`.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            uinput: Uinput::keyboard("earwig paste keyboard", [KEY_LEFTSHIFT, KEY_INSERT])?,
        })
    }

    /// Press Shift+Insert. Shift and Insert sit on the same keys in every layout.
    pub fn shift_insert(&self) -> io::Result<()> {
        let events = [
            (KEY_LEFTSHIFT, true),
            (KEY_INSERT, true),
            (KEY_INSERT, false),
            (KEY_LEFTSHIFT, false),
        ];
        for (code, down) in events {
            self.uinput.write(&[
                InputEvent::new(EV_KEY, code, i32::from(down)),
                InputEvent::syn(),
            ])?;
        }
        Ok(())
    }
}

/// Whether Ctrl, Alt or Super is down on a keyboard earwig can read.
pub fn modifier_held() -> bool {
    super::event_nodes().any(|path| {
        let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
        rustix::fs::open(&path, flags, Mode::empty())
            .ok()
            .and_then(|fd| evdev::keys_down(&fd).ok())
            .is_some_and(|down| MODIFIERS.iter().any(|&code| down.has(code)))
    })
}
