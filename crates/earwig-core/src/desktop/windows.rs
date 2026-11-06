//! Windows: nothing is probed yet.

use super::DesktopCaps;

pub(super) fn probe() -> DesktopCaps {
    DesktopCaps {
        desktop: "windows".into(),
    }
}
