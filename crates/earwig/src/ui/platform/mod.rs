//! What the front end does differently per OS, with the same signatures in each OS module.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, target_os = "macos"))]
mod tray_icon;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

#[cfg(target_os = "linux")]
pub(crate) use os::usable_area;
#[cfg(windows)]
pub(crate) use os::{Grip, autostart, set_autostart, set_grip, set_input_region};
pub(crate) use os::{
    Popup, TrayHandle, application, follow_accent, home_display, overlay_surface, spawn_tray,
};
#[cfg(target_os = "macos")]
pub(crate) use os::{autostart, set_autostart, set_input_region};
