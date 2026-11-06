//! What the front end does differently per OS. Each OS module provides:
//!
//! - `overlay_surface(caps)`: how the overlay floats above other windows
//!   without taking focus, or [`OverlaySurface::None`](super::OverlaySurface).
//! - `application(surface, caps)`: the GPUI application to run.
//! - `Popup`: moves the overlay's [`OverlaySurface::Popup`](super::OverlaySurface)
//!   window during a drag and reports the pointer in screen coordinates
//!   (`new`, `pointer`, `move_to`).
//! - `spawn_tray(handle, local)` and `TrayHandle::set_state`.
//! - `follow_accent(tx, cx)`: sends the desktop's accent colour, and every
//!   change of it, to `tx`.

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

#[cfg(target_os = "linux")]
pub(crate) use os::usable_area;
pub(crate) use os::{Popup, TrayHandle, application, follow_accent, overlay_surface, spawn_tray};
