//! earwig's core: everything except pixels. The [`controller`] owns the
//! dictation state; commands come in and [`events::Event`]s go out through
//! [`ipc`] and [`controller::Handle`].

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
