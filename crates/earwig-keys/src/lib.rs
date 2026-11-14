//! One global hotkey: parse and check it, watch the keyboard for it, record a new one.
//! A key with modifiers (`Super+Space`) is swallowed; modifiers alone (`RightCtrl`) pass through.
//! Linux reads evdev directly, macOS uses an event tap, Windows a low-level keyboard hook.

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
    /// Modifiers alone turned out to be part of a shortcut: take the press back (no release).
    Chord,
}

/// `kCGEventSourceUserData` of key events the program posts itself.
#[cfg(target_os = "macos")]
pub const SYNTHETIC: i64 = 0x6561_7277;
/// `dwExtraInfo` of key events the program sends itself.
#[cfg(windows)]
pub const SYNTHETIC: usize = 0xea51_9000;

/// Can we see keys? Keyboard read access on Linux, Accessibility on macOS, always on Windows.
pub fn can_see_keys() -> bool {
    backend::can_see_keys()
}
