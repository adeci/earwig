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
/// listed here). The GlobalShortcuts portal runs where it exists, except on
/// compositors whose own binds are the norm (they also see key releases).
pub fn sources(
    config: &crate::config::HotkeyConfig,
    caps: &crate::desktop::DesktopCaps,
) -> Vec<Box<dyn HotkeySource>> {
    const BIND_COMPOSITORS: &[&str] = &["niri", "sway", "hyprland", "river"];
    let compositor_binds = caps
        .desktop
        .split(':')
        .any(|desktop| BIND_COMPOSITORS.contains(&desktop));
    if config.portal && caps.portal_global_shortcuts && !compositor_binds {
        vec![Box::new(portal::PortalShortcuts::new(
            &config.portal_trigger,
        ))]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyConfig;
    use crate::desktop::DesktopCaps;

    fn names(config: &HotkeyConfig, desktop: &str, portal: bool) -> Vec<&'static str> {
        let caps = DesktopCaps {
            desktop: desktop.into(),
            portal_global_shortcuts: portal,
            ..DesktopCaps::default()
        };
        sources(config, &caps).iter().map(|s| s.name()).collect()
    }

    #[test]
    fn portal_source_runs_on_desktops_without_compositor_binds() {
        let config = HotkeyConfig::default();
        assert_eq!(names(&config, "gnome", true), ["portal"]);
        assert_eq!(names(&config, "ubuntu:gnome", true), ["portal"]);
        assert_eq!(names(&config, "kde", true), ["portal"]);
    }

    #[test]
    fn portal_source_stays_off_when_unavailable_disabled_or_bound_by_compositor() {
        let config = HotkeyConfig::default();
        let none: [&str; 0] = [];
        assert_eq!(names(&config, "gnome", false), none);
        assert_eq!(names(&config, "niri", true), none);
        assert_eq!(names(&config, "sway:wlroots", true), none);
        let disabled = HotkeyConfig {
            portal: false,
            ..HotkeyConfig::default()
        };
        assert_eq!(names(&disabled, "gnome", true), none);
    }
}
