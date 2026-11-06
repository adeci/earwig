//! macOS: nothing to probe; privacy permissions change while earwig runs,
//! so [`crate::permissions`] checks them when asked.

use super::DesktopCaps;

pub(super) fn probe() -> DesktopCaps {
    DesktopCaps {
        desktop: "macos".into(),
    }
}
