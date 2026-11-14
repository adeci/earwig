//! earwig without the pixels: the [`controller`] owns the dictation state;
//! commands come in and [`events::Event`]s go out through [`ipc`] and
//! [`controller::Handle`].

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("earwig runs on Linux, macOS and Windows");

pub mod audio;
pub mod config;
pub mod controller;
pub mod desktop;
pub mod engine;
pub mod events;
pub mod history;
pub mod hooks;
pub mod hotkey;
pub mod insert;
pub mod ipc;
#[cfg(target_os = "macos")]
mod macos;
pub mod models;
pub mod paths;
pub mod permissions;
pub mod session;
pub mod spectrum;
#[cfg(windows)]
mod win32;

/// Audio handed to an engine is 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;

/// The desktop entry's basename and earwig's app id, for XDG portals and the
/// settings window. GNOME refuses portal calls from apps without that entry.
pub const APP_ID: &str = "io.github.adeci.earwig";
