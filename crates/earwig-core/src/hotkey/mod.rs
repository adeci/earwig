//! Where key presses come from.
//!
//! The daemon owns tap-or-hold timing ([`crate::session`]); a source only
//! reports edges as [`Command::Press`] / [`Command::Release`] (or
//! [`Command::Toggle`] when it cannot see releases).
//!
//! - socket: `earwig press|release|toggle|cancel`, on every OS. Always on;
//!   lives in [`crate::ipc`]. On Linux compositor binds drive it (niri
//!   `press {}`/`release {}`, sway `bindsym --release`, Hyprland `bindr`).
//! - Linux: the XDG GlobalShortcuts portal (GNOME 48+, KDE).
//!   `Activated`/`Deactivated` map to press/release.
//! - Windows: a low-level keyboard hook watching [`HotkeyConfig::key`].
//! - macOS: no global hotkey of its own yet; the socket is the only source.
//!
//! Each OS module provides `sources(config, caps)` and `configure(config)`.

pub mod trigger;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::INJECTED;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

use crate::config::HotkeyConfig;
use crate::desktop::DesktopCaps;
use crate::events::Command;

pub trait HotkeySource: Send {
    /// Name for [`crate::events::Status::hotkey_sources`].
    fn name(&self) -> &'static str;

    /// Run until the sender is dropped or the source fails; called on its own
    /// thread.
    fn run(self: Box<Self>, commands: flume::Sender<Command>) -> anyhow::Result<()>;
}

/// Sources to start besides the always-on socket.
pub fn sources(config: &HotkeyConfig, caps: &DesktopCaps) -> Vec<Box<dyn HotkeySource>> {
    os::sources(config, caps)
}

/// Apply a changed [`HotkeyConfig`] to the running sources. Only
/// [`HotkeyConfig::key`] changes while they run; a key that does not parse
/// turns the OS's own hotkey off until it does.
pub fn configure(config: &HotkeyConfig) -> Result<(), trigger::ParseError> {
    os::configure(config)
}
