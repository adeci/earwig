//! Windows: nothing to probe.

use super::DesktopCaps;

pub(super) fn probe() -> DesktopCaps {
    DesktopCaps {
        desktop: "windows".into(),
    }
}
