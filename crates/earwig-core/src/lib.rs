//! earwig's core: everything except pixels.
//!
//! The daemon is a [`controller`] that owns the dictation state machine
//! ([`session`]), the microphone ([`audio`]), a loaded speech model
//! ([`engine`]), and an inserter ([`insert`]). Hotkeys arrive as
//! [`events::Command`]s from the control socket ([`ipc`]) or the portal
//! ([`hotkey`]); everything that happens leaves as [`events::Event`]s, which
//! the GPUI front end, `earwig events`, and status bars consume. The final
//! text passes through the user's [`hooks`] on its way out.

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
pub mod models;
pub mod paths;
pub mod session;

/// Sample rate of all audio handed to an engine: 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;
