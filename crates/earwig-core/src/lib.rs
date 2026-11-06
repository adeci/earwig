//! earwig's core: everything except pixels. The [`controller`] owns the
//! dictation state; commands come in and [`events::Event`]s go out through
//! [`ipc`] and [`controller::Handle`].
//!
//! Each OS fills the same slots: [`desktop`] (`probe`), [`hotkey`]
//! (`sources`), [`insert`] (`backends`), [`paths`], [`ipc`] (transport) and
//! [`hooks`] (process tree), each with a `linux`, `macos` and `windows`
//! module behind one signature.

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

/// Sample rate of all audio handed to an engine: 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;

/// earwig's application id: the desktop entry's basename, the id XDG portals
/// know earwig by (GNOME rejects portal requests from host apps without an
/// installed desktop file of that name) and the settings window's app id.
pub const APP_ID: &str = "io.github.adeci.earwig";
