//! A global hotkey: parse it ([`Hotkey`]), check that it is usable
//! ([`Hotkey::check`]), watch the keyboard for it ([`Listener::run`]) and
//! record a new one ([`Listener::record`]).
//!
//! One hotkey at a time, either a key with modifiers (`Super+Space`,
//! swallowed so apps never see it) or modifiers alone (`RightCtrl`, passed
//! through; [`Event::Chord`] says when they turned out to be part of a
//! shortcut), with left and right modifiers apart. Edges come as
//! [`Event::Pressed`] and [`Event::Released`], so hold-to-talk and tapping
//! both work.
//!
//! - Linux: evdev (`/dev/input/event*`) directly, on any desktop, with
//!   hot-plug; needs read access to the keyboards. Swallowing grabs the
//!   keyboards and passes every other key on through a uinput clone of each;
//!   without `/dev/uinput` access, or where another program (keyd) holds
//!   them, the hotkey reaches apps too.
//! - macOS: a `CGEventTap`; needs Accessibility.
//! - Windows: a low-level keyboard hook.
//!
//! Keys the program sends itself, marked with [`SYNTHETIC`] (macOS,
//! Windows), are not seen; on Linux it sends none through the keyboards.

mod check;
mod hotkey;
mod key;
mod listener;
mod matcher;
mod recorder;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(windows)]
use windows as backend;

pub use check::Check;
pub use hotkey::{Hotkey, ParseError};
pub use key::{Key, Modifier, Side};
pub use listener::{Blocking, Listener, Options, Status};
pub use recorder::Recorded;

/// What the hotkey did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Pressed,
    Released,
    /// The modifiers of a modifiers-alone hotkey were part of a shortcut
    /// after all: take the press back. No release follows.
    Chord,
}

/// `kCGEventSourceUserData` of key events the program posts itself.
#[cfg(target_os = "macos")]
pub const SYNTHETIC: i64 = 0x6561_7277;
/// `dwExtraInfo` of key events the program sends itself.
#[cfg(windows)]
pub const SYNTHETIC: usize = 0xea51_9000;

/// Whether the system lets this program see keys now: read access to a
/// keyboard (Linux), Accessibility (macOS); always on Windows.
pub fn can_see_keys() -> bool {
    backend::can_see_keys()
}
