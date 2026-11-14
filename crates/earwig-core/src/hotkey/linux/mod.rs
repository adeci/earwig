//! Linux: the GlobalShortcuts portal where earwig cannot watch a key itself,
//! and the udev rule that lets it (keyboard access).

mod portal;

use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;

use anyhow::Context as _;

use super::HotkeySource;
use crate::config::HotkeyConfig;
use crate::desktop::DesktopCaps;

/// Lets the user at the seat read keyboards and make virtual ones; the
/// NixOS module installs the same file.
const UDEV_RULE: &str = include_str!("../../../../../assets/udev/70-earwig.rules");
const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/70-earwig.rules";

/// The GlobalShortcuts portal, when enabled and advertised and earwig cannot
/// watch a key itself; one that does not work ends quietly within seconds.
pub(super) fn sources(config: &HotkeyConfig, caps: &DesktopCaps) -> Vec<Box<dyn HotkeySource>> {
    let watching = !config.key.trim().is_empty() && earwig_keys::can_see_keys();
    if config.portal && caps.portal_global_shortcuts && !watching {
        vec![Box::new(portal::PortalShortcuts::new(
            &config.portal_trigger,
        ))]
    } else {
        Vec::new()
    }
}

/// The portal bound earwig's shortcut in this session.
pub(super) fn portal_bound() -> bool {
    portal::BOUND.load(Ordering::Relaxed)
}

/// How keyboard access is given on this system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessFix {
    /// NixOS: the `services.earwig` module installs the rule
    /// (`keyboardAccess`, on by default).
    NixosModule,
    /// Anywhere else: [`allow_keyboard_access`].
    Helper,
}

pub fn access_fix() -> AccessFix {
    if Path::new("/etc/NIXOS").exists() {
        AccessFix::NixosModule
    } else {
        AccessFix::Helper
    }
}

/// Install the udev rule as root (pkexec asks for the password) and apply it
/// to the keyboards plugged in now. Blocks until the user answered.
pub fn allow_keyboard_access() -> anyhow::Result<()> {
    let script = format!(
        "cat > {UDEV_RULE_PATH} <<'EOF'\n{UDEV_RULE}EOF\n\
         udevadm control --reload && \
         udevadm trigger --action=change --subsystem-match=input --subsystem-match=misc"
    );
    let status = Command::new("pkexec")
        .args(["/bin/sh", "-c", &script])
        .status()
        .context("running pkexec")?;
    match status.code() {
        Some(0) => Ok(()),
        // pkexec: the dialog was dismissed, or the password refused.
        Some(126 | 127) => anyhow::bail!("not allowed"),
        _ => anyhow::bail!("installing {UDEV_RULE_PATH} failed ({status})"),
    }
}
