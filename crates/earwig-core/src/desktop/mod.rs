//! What the running desktop offers, probed once at startup. Each OS module
//! provides `probe()` and owns its own capability fields.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    target_os = "linux",
    expect(
        clippy::struct_excessive_bools,
        reason = "independent capability flags"
    )
)]
pub struct DesktopCaps {
    /// Linux: `XDG_CURRENT_DESKTOP`, lowercased, e.g. `niri`, `gnome`,
    /// `kde`. `macos` and `windows` elsewhere.
    pub desktop: String,
    #[cfg(target_os = "linux")]
    pub wayland: bool,
    #[cfg(target_os = "linux")]
    pub x11: bool,
    /// `zwlr_layer_shell_v1`: the overlay can float above windows without
    /// taking focus.
    #[cfg(target_os = "linux")]
    pub layer_shell: bool,
    /// `zwp_input_method_manager_v2`.
    #[cfg(target_os = "linux")]
    pub input_method: bool,
    /// `zwp_virtual_keyboard_manager_v1`.
    #[cfg(target_os = "linux")]
    pub virtual_keyboard: bool,
    /// `ext_data_control_manager_v1` or `zwlr_data_control_manager_v1`.
    #[cfg(target_os = "linux")]
    pub data_control: bool,
    #[cfg(target_os = "linux")]
    pub portal_global_shortcuts: bool,
    #[cfg(target_os = "linux")]
    pub portal_remote_desktop: bool,
}

/// Never fails: whatever cannot be reached counts as absent.
pub fn probe() -> DesktopCaps {
    os::probe()
}
