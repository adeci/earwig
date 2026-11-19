//! The control socket's wire format (one JSON object per line) and the
//! in-process feed the UI renders from.

use serde::{Deserialize, Serialize};

use crate::models::ModelSpec;
use crate::permissions::PermissionStatus;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Command {
    Press,
    Release,
    /// Start if idle, otherwise stop and insert. For press-only bindings.
    Toggle,
    /// Drop the current recording without inserting anything.
    Cancel,
    /// The held hotkey was part of a key combination: a recording its press
    /// started is dropped (one it stopped still finishes), its release ignored.
    Chord,
    ShowSettings,
    /// Reply with one [`Event::Status`].
    Status,
    /// Keep the connection open and stream every [`Event`].
    Subscribe,
    /// Re-read the config files and apply changes (audio device, model, ...).
    ReloadConfig,
    /// Switch the active model; it must already be on disk.
    UseModel {
        model: ModelSpec,
    },
    /// Ask GitHub for a newer release now. The socket replies with one
    /// [`Event::Update`], or an [`Event::Error`] when that fails.
    CheckForUpdates,
    /// Download and install the newer release the last check found. The
    /// socket replies with one [`Event::Update`] once it is underway, or an
    /// [`Event::Error`] when there is nothing to install.
    Update,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// Full snapshot; the reply to [`Command::Status`], the first event of
    /// every subscription, and sent again when the insertion methods change.
    Status {
        status: Box<Status>,
    },
    State {
        state: DictationState,
    },
    /// Microphone level while recording, 0.0 (silence) to 1.0 (loud), 20 Hz.
    Level {
        level: f32,
    },
    /// Live transcript of the current recording. `committed` only grows within
    /// a recording; `tentative` is the tail that may still be rewritten.
    Transcript {
        committed: String,
        tentative: String,
    },
    Inserted {
        text: String,
        method: InsertMethod,
    },
    /// The final text could not be inserted. It was left on the clipboard when
    /// `on_clipboard` is true.
    InsertFailed {
        text: String,
        reason: String,
        on_clipboard: bool,
    },
    Model {
        model: ModelStatus,
    },
    /// A dictation was asked for while the model is missing or failed to
    /// load (`model` says which): nothing is recorded.
    ModelNotReady {
        model: ModelStatus,
    },
    /// A non-fatal problem worth showing to the user.
    Error {
        message: String,
    },
    /// The settings window was requested (tray, CLI, socket).
    ShowSettings,
    Update {
        update: UpdateState,
    },
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
    /// The clipboard and the primary selection set without focus, then
    /// Shift+Insert through the RemoteDesktop portal; GNOME and KDE without
    /// keyboard access, once the user allowed it in the desktop's dialog.
    PortalPaste,
    /// Clipboard set without focus, then one paste chord through earwig's own
    /// keyboard (uinput); GNOME and KDE with keyboard access.
    UinputPaste,
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
        /// GGUF `general.architecture`, e.g. `parakeet`.
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
    /// Insertion backends set up in this session, best first;
    /// `portal-paste` types once the user allowed it.
    pub insert_methods: Vec<InsertMethod>,
    /// Hotkey sources running in this session: `socket` always, `keyboard`
    /// (earwig watching the key itself), `portal`.
    pub hotkey_sources: Vec<String>,
    /// Which of them delivers the hotkey now, and why.
    #[serde(default)]
    pub hotkey: HotkeyStatus,
    /// Privacy permissions the user grants by hand (macOS), as of this
    /// snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PermissionStatus>,
    /// Whether a newer earwig is out, in builds that update themselves (the
    /// Windows and macOS releases); absent in the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateState>,
}

/// A newer release of earwig and how installing it goes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum UpdateState {
    /// Nothing newer, or not checked yet.
    #[default]
    None,
    /// `version` is out. `installable` is false where earwig can't replace
    /// itself (a portable copy, an app in a folder it can't write); its
    /// release page has it.
    Available { version: String, installable: bool },
    Downloading {
        version: String,
        downloaded: u64,
        total: u64,
    },
    /// Downloaded and checked; the new earwig is going in, then starts.
    Installing { version: String },
    /// Installing `version` failed; the installed earwig is as it was.
    Failed { version: String, reason: String },
}

/// How the hotkey reaches earwig.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyStatus {
    /// `keyboard` (earwig watches `key` itself), `portal` (the desktop's
    /// shortcut) or `socket` (only `earwig press` and the like).
    pub source: String,
    /// The key earwig watches, as `[hotkey] key` writes it; empty for none.
    pub key: String,
    /// The shortcut that throws a dictation away, as `[hotkey] discard`
    /// writes it; empty for none.
    #[serde(default)]
    pub discard: String,
    pub keyboard: KeyboardState,
    /// Other apps do not get the key.
    pub blocking: bool,
    /// Why the source is not the keyboard, or why other apps get the key too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Whether earwig sees the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyboardState {
    Watching,
    /// The system does not let earwig see keys: Accessibility (macOS),
    /// keyboard access (Linux, `/dev/input`).
    NoAccess,
    /// Linux: no keyboard is plugged in.
    NoKeyboard,
    /// Not watching (yet).
    #[default]
    Stopped,
}
