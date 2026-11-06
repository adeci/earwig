//! Linux: the XDG GlobalShortcuts portal; compositor binds use the socket.

mod portal;

use super::HotkeySource;
use crate::config::HotkeyConfig;
use crate::desktop::DesktopCaps;

/// The GlobalShortcuts portal when enabled and advertised. An advertised
/// portal that does not work ends quietly within seconds.
pub(super) fn sources(config: &HotkeyConfig, caps: &DesktopCaps) -> Vec<Box<dyn HotkeySource>> {
    if config.portal && caps.portal_global_shortcuts {
        vec![Box::new(portal::PortalShortcuts::new(
            &config.portal_trigger,
        ))]
    } else {
        Vec::new()
    }
}

/// The portal's binding lives in the desktop's settings, not in earwig's.
#[expect(clippy::unnecessary_wraps, reason = "the same signature on every OS")]
pub(super) fn configure(_config: &HotkeyConfig) -> Result<(), super::trigger::ParseError> {
    Ok(())
}
