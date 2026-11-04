//! earwig's core: everything except pixels.
//!
//! The daemon is a [`controller`] that owns the dictation state machine
//! (`session`), the microphone (`audio`), a loaded speech model
//! ([`engine`]), and an inserter ([`insert`]). Hotkeys arrive as
//! [`events::Command`]s from the control socket (`ipc`) or the portal
//! ([`hotkey`]); everything that happens leaves as [`events::Event`]s, which
//! the GPUI front end, `earwig events`, and status bars consume.

pub mod config;
pub mod controller;
pub mod desktop;
pub mod engine;
pub mod events;
pub mod history;
pub mod hotkey;
pub mod insert;
pub mod models;
pub mod paths;

/// Sample rate of all audio handed to an engine: 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;

// Still to come from the pipeline slice:
//
// session   pure tap-or-hold state machine; time is passed in
// audio     cpal capture resampled to SAMPLE_RATE, pre-roll ring buffer;
//           `fn input_devices() -> anyhow::Result<Vec<String>>`
// hooks     run `HooksConfig` commands
// ipc       control socket: one JSON `Command` per line in, `Event`s out
//   fn serve(handle: controller::Handle) -> anyhow::Result<()>   // own thread
//   fn request(cmd: &Command) -> anyhow::Result<Option<Event>>   // client
//   fn subscribe() -> anyhow::Result<impl Iterator<Item = anyhow::Result<Event>>>
// controller::spawn(parts) -> anyhow::Result<Handle>
