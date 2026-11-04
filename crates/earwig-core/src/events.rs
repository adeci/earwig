//! What the daemon says and what it accepts.
//!
//! These types are the wire protocol of the control socket (one JSON object
//! per line) and the in-process feed the UI renders from, so scripts, status
//! bars and the GPUI front end all see the same thing.

use serde::{Deserialize, Serialize};

use crate::models::ModelSpec;

/// A request to the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Command {
    /// Hotkey went down.
    Press,
    /// Hotkey went up.
    Release,
    /// Start if idle, otherwise stop and insert. For press-only bindings.
    Toggle,
    /// Drop the current recording without inserting anything.
    Cancel,
    /// Open (or raise) the settings window.
    ShowSettings,
    /// Reply with one [`Event::Status`].
    Status,
    /// Keep the connection open and stream every [`Event`].
    Subscribe,
    /// Re-read the config files and apply changes (audio device, model, ...).
    ReloadConfig,
    /// Switch the active model; it must already be on disk.
    UseModel { model: ModelSpec },
    /// Exit the daemon.
    Quit,
}

/// Something the daemon reports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// Full snapshot; the reply to [`Command::Status`] and the first event of
    /// every subscription.
    Status { status: Status },
    /// Dictation state changed.
    State { state: DictationState },
    /// Microphone level while recording, 0.0 (silence) to 1.0 (loud).
    Level { level: f32 },
    /// Live transcript of the current recording. `committed` only grows within
    /// a recording; `tentative` is the tail that may still be rewritten.
    Transcript {
        committed: String,
        tentative: String,
    },
    /// The final text reached the focused application.
    Inserted { text: String, method: InsertMethod },
    /// The final text could not be inserted. It was left on the clipboard when
    /// `on_clipboard` is true.
    InsertFailed {
        text: String,
        reason: String,
        on_clipboard: bool,
    },
    /// The active model changed state.
    Model { model: ModelStatus },
    /// A non-fatal problem worth showing to the user.
    Error { message: String },
    /// The settings window was requested (tray, CLI, socket).
    ShowSettings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DictationState {
    #[default]
    Idle,
    /// Capturing speech. `latched` is true once a quick tap locked recording
    /// on; the next press stops it. `started_at_ms` is Unix time in ms.
    Recording { latched: bool, started_at_ms: u64 },
    /// Recording stopped; producing the final text and inserting it.
    Finalizing,
}

/// How text reached the focused application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InsertMethod {
    /// Wayland input-method commit: one atomic string, clipboard untouched.
    InputMethod,
    /// Clipboard set, then one paste chord through a virtual keyboard.
    Paste,
    /// Clipboard set through the RemoteDesktop portal, paste chord over libei.
    PortalPaste,
    /// Keystrokes over libei (RemoteDesktop portal).
    PortalType,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ModelStatus {
    /// No model configured or the configured one is not on disk.
    Missing {
        model: ModelSpec,
    },
    Loading {
        model: ModelSpec,
    },
    Ready {
        model: ModelSpec,
        /// Architecture reported by the engine, e.g. `parakeet-unified`.
        architecture: String,
        /// Whether live text comes from native streaming (true) or periodic
        /// re-decoding (false).
        streaming: bool,
        /// Compute backend in use, e.g. `vulkan` or `cpu`.
        backend: String,
    },
    Failed {
        model: ModelSpec,
        message: String,
    },
}

/// Everything a fresh UI or `earwig status` needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: DictationState,
    pub model: ModelStatus,
    /// Insertion backends usable in this session, best first.
    pub insert_methods: Vec<InsertMethod>,
    /// Hotkey sources running in this session, e.g. `socket`, `portal`.
    pub hotkey_sources: Vec<String>,
}
