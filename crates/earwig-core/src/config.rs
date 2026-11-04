//! Settings.
//!
//! Three layers, later wins:
//! 1. built-in defaults (this file),
//! 2. the user file `$XDG_CONFIG_HOME/earwig/config.toml`, written by the
//!    settings window,
//! 3. the system file `earwig/config.toml` under `$XDG_CONFIG_DIRS`
//!    (`/etc/xdg` on NixOS, written by the module).
//!
//! Keys set in the system file are locked: the settings window shows them
//! read-only and never writes them to the user file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::ModelSpec;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    pub audio: AudioConfig,
    pub model: ModelConfig,
    pub insert: InsertConfig,
    pub overlay: OverlayConfig,
    pub history: HistoryConfig,
    pub hooks: HooksConfig,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivationMode {
    /// Hold to talk; a quick tap latches recording on until the next press.
    #[default]
    HoldOrToggle,
    /// Recording lasts exactly as long as the key is held.
    Hold,
    /// Every press flips recording on or off.
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub mode: ActivationMode,
    /// A release this soon after the press counts as a tap.
    pub hold_threshold_ms: u64,
    /// Register with the GlobalShortcuts portal when the desktop offers it.
    pub portal: bool,
    /// Trigger suggested to the portal, in XDG shortcut syntax.
    pub portal_trigger: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            mode: ActivationMode::HoldOrToggle,
            hold_threshold_ms: 300,
            portal: true,
            portal_trigger: "LOGO+space".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// Input device name as listed by `earwig devices`; `None` = system default.
    pub device: Option<String>,
    /// Keep the microphone open between dictations so speech that starts as
    /// the key goes down is not clipped. The desktop's mic indicator stays on.
    pub keep_open: bool,
    /// Audio from before the press that is included in each recording.
    pub preroll_ms: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: None,
            keep_open: true,
            preroll_ms: 300,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    /// `None` = the release's default catalog model.
    pub model: Option<ModelSpec>,
    /// `auto`, `cpu` or `gpu`.
    pub backend: BackendSetting,
    /// Language hint for multilingual models.
    pub language: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendSetting {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InsertSetting {
    /// Best available: input method, then paste, then portal.
    #[default]
    Auto,
    InputMethod,
    Paste,
    Portal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InsertConfig {
    pub method: InsertSetting,
    /// Extra app ids treated as terminals (Ctrl+Shift+V) on top of the
    /// built-in list.
    pub terminals: Vec<String>,
}

impl Default for InsertConfig {
    fn default() -> Self {
        Self {
            method: InsertSetting::Auto,
            terminals: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayPosition {
    Top,
    #[default]
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub position: OverlayPosition,
    /// Lines of live text shown before older lines scroll away.
    pub max_lines: u32,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position: OverlayPosition::Bottom,
            max_lines: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    pub enabled: bool,
    /// Entries kept on disk; older ones are dropped.
    pub limit: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            limit: 50,
        }
    }
}

/// External commands, the extension point for anything earwig does not do
/// itself (word replacements, LLM cleanup, logging, notifications).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HooksConfig {
    /// argv of a filter: gets the final text on stdin, its stdout is inserted
    /// instead. Non-zero exit or timeout inserts the unfiltered text.
    pub filter: Vec<String>,
    /// Filter timeout in milliseconds.
    pub filter_timeout_ms: Option<u64>,
    /// argv run after each insertion with the inserted text on stdin; not
    /// awaited.
    pub on_insert: Vec<String>,
}

/// Paths of the layers, for display and for writing the user file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigPaths {
    pub user: PathBuf,
    /// System files that exist, lowest precedence first.
    pub system: Vec<PathBuf>,
}

// Implemented by the pipeline slice:
//
//   pub struct ConfigStore { .. }
//   impl ConfigStore {
//       pub fn load() -> anyhow::Result<Self>               // XDG locations
//       pub fn load_from(paths: ConfigPaths) -> anyhow::Result<Self>
//       pub fn config(&self) -> &Config
//       pub fn paths(&self) -> &ConfigPaths
//       /// Dotted key such as `audio.keep_open`.
//       pub fn is_locked(&self, key: &str) -> bool
//       /// Apply `edit`, write the user layer (only keys that differ from
//       /// defaults and are not locked), and reload.
//       pub fn update(&mut self, edit: impl FnOnce(&mut Config)) -> anyhow::Result<()>
//   }
