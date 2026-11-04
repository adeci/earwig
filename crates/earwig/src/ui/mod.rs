//! GPUI front end: overlay, settings window, tray (UI slice).

use earwig_core::controller::Handle;
use earwig_core::desktop::DesktopCaps;

/// Run the GPUI application on the main thread until the daemon quits.
pub fn run(_handle: Handle, _caps: DesktopCaps) -> anyhow::Result<()> {
    anyhow::bail!("ui: not implemented yet")
}
