//! Where key presses come from.
//!
//! The daemon owns tap-or-hold timing ([`crate::session`]); a source only
//! reports edges as [`Command::Press`] / [`Command::Release`] (or
//! [`Command::Toggle`] when it cannot see releases).
//!
//! - socket: `earwig press|release|toggle|cancel`, driven by compositor binds
//!   (niri `press {}`/`release {}`, sway `bindsym --release`, Hyprland
//!   `bindr`). Always on; lives in [`crate::ipc`].
//! - portal: XDG GlobalShortcuts (GNOME 48+, KDE). `Activated`/`Deactivated`
//!   map to press/release.

pub mod portal;

use crate::events::Command;

pub trait HotkeySource: Send {
    /// Name for [`crate::events::Status::hotkey_sources`].
    fn name(&self) -> &'static str;

    /// Run until the sender is dropped or the source fails; called on its own
    /// thread.
    fn run(self: Box<Self>, commands: flume::Sender<Command>) -> anyhow::Result<()>;
}

/// Hotkey sources to start for this session (the socket is always on and not
/// listed here): the GlobalShortcuts portal when enabled and advertised. A
/// portal that is advertised but does not work ends quietly within seconds
/// (see [`portal`]).
pub fn sources(
    config: &crate::config::HotkeyConfig,
    caps: &crate::desktop::DesktopCaps,
) -> Vec<Box<dyn HotkeySource>> {
    if config.portal && caps.portal_global_shortcuts {
        vec![Box::new(portal::PortalShortcuts::new(
            &config.portal_trigger,
        ))]
    } else {
        Vec::new()
    }
}
