//! What the front end does differently per OS. Each OS module provides:
//!
//! - `overlay_surface(caps)`: how the overlay floats above other windows
//!   without taking focus, or [`OverlaySurface::None`](super::OverlaySurface).
//! - `application(surface, caps)`: the GPUI application to run.
//! - `Popup`: moves the overlay's [`OverlaySurface::Popup`](super::OverlaySurface)
//!   window during a drag and reports the pointer in screen coordinates
//!   (`new`, `pointer`, `move_to`).
//! - `home_display(cx)`: the display the overlay opens on.
//! - `spawn_tray(handle, local)` (called inside the GPUI application) and
//!   `TrayHandle::set_state`.
//! - `follow_accent(tx, cx)`: sends the desktop's accent colour, and every
//!   change of it, to `tx`.
//!
//! macOS and Windows also have `set_input_region`, as GPUI does not
//! implement `Window::set_input_region` there; Windows has
//! `autostart`/`set_autostart`.

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
pub(crate) use os::{
    Popup, TrayHandle, application, follow_accent, home_display, overlay_surface, spawn_tray,
};
#[cfg(any(windows, target_os = "macos"))]
pub(crate) use os::set_input_region;
#[cfg(windows)]
pub(crate) use os::{autostart, set_autostart};
