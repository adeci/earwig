#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::{current_platform, linux_platform};
#[cfg(feature = "wayland")]
pub use linux::{move_layer_surface, pointer_travel};
#[cfg(feature = "x11")]
pub use linux::x11_pointer_position;
