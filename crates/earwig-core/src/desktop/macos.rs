//! macOS: nothing to probe; [`crate::permissions`] checks grants when asked.

use super::DesktopCaps;

pub(super) fn probe() -> DesktopCaps {
    DesktopCaps {
        desktop: "macos".into(),
    }
}
