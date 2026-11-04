//! What the running desktop offers, probed once at startup.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCaps {
    /// `XDG_CURRENT_DESKTOP`, lowercased, e.g. `niri`, `gnome`, `kde`.
    pub desktop: String,
    pub wayland: bool,
    pub x11: bool,
    /// `zwlr_layer_shell_v1`: the overlay can float above windows without
    /// taking focus.
    pub layer_shell: bool,
    /// `zwp_input_method_manager_v2`.
    pub input_method: bool,
    /// `zwp_virtual_keyboard_manager_v1`.
    pub virtual_keyboard: bool,
    /// `ext_data_control_manager_v1` or `zwlr_data_control_manager_v1`.
    pub data_control: bool,
    /// `zwlr_foreign_toplevel_manager_v1` (focused app id for terminal
    /// detection).
    pub foreign_toplevel: bool,
    /// `org.freedesktop.portal.GlobalShortcuts` is available.
    pub portal_global_shortcuts: bool,
    /// `org.freedesktop.portal.RemoteDesktop` is available.
    pub portal_remote_desktop: bool,
}

// Implemented by the insertion slice:
//
//   pub fn probe() -> DesktopCaps
//
// Connects to Wayland once (globals only, no surfaces) and asks the portal
// frontend over D-Bus which interfaces it offers.
