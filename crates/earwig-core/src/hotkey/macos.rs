//! macOS: no global hotkey of earwig's own yet. `earwig press`, `release`
//! and `toggle` over the control socket work, so any hotkey tool that runs
//! commands can drive dictation.

use super::HotkeySource;
use crate::config::HotkeyConfig;
use crate::desktop::DesktopCaps;

pub(super) fn sources(_config: &HotkeyConfig, _caps: &DesktopCaps) -> Vec<Box<dyn HotkeySource>> {
    Vec::new()
}

#[expect(clippy::unnecessary_wraps, reason = "the same signature on every OS")]
pub(super) fn configure(_config: &HotkeyConfig) -> Result<(), super::trigger::ParseError> {
    Ok(())
}
