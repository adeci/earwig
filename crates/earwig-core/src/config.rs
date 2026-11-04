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

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

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

/// The merged settings plus where each layer lives.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    paths: ConfigPaths,
    config: Config,
    /// Dotted leaf keys set by a system file.
    locked: BTreeSet<String>,
    /// Why the user file was ignored, if it was.
    user_error: Option<String>,
}

impl ConfigStore {
    /// Load from the XDG locations.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(ConfigPaths {
            user: crate::paths::user_config(),
            system: crate::paths::system_configs(),
        })
    }

    /// Load from explicit paths. A broken system file is an error (it is
    /// managed, so it must be fixed where it is generated); a broken user file
    /// is skipped and reported through [`ConfigStore::user_error`] so the
    /// daemon still starts.
    pub fn load_from(paths: ConfigPaths) -> anyhow::Result<Self> {
        let mut merged = toml::Table::try_from(Config::default())?;
        let mut user_error = None;
        match read_table(&paths.user) {
            Ok(Some(user)) => {
                let mut candidate = merged.clone();
                merge(&mut candidate, user);
                match candidate.clone().try_into::<Config>() {
                    Ok(_) => merged = candidate,
                    Err(e) => user_error = Some(format!("{}: {e}", paths.user.display())),
                }
            }
            Ok(None) => {}
            Err(e) => user_error = Some(format!("{}: {e}", paths.user.display())),
        }
        if let Some(e) = &user_error {
            log::warn!("ignoring user config: {e}");
        }
        let mut locked = BTreeSet::new();
        for path in &paths.system {
            let table = read_table(path)
                .with_context(|| format!("reading {}", path.display()))?
                .unwrap_or_default();
            leaf_keys(&table, "", &mut locked);
            merge(&mut merged, table);
        }
        let config = merged.try_into::<Config>().with_context(|| {
            format!(
                "system config {}",
                paths
                    .system
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        Ok(Self {
            paths,
            config,
            locked,
            user_error,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn paths(&self) -> &ConfigPaths {
        &self.paths
    }

    pub fn user_error(&self) -> Option<&str> {
        self.user_error.as_deref()
    }

    /// Whether a dotted key such as `audio.keep_open` (or a parent of it) is
    /// set by a system file and therefore read-only here.
    pub fn is_locked(&self, key: &str) -> bool {
        let mut prefix = key;
        loop {
            if self.locked.contains(prefix) {
                return true;
            }
            match prefix.rsplit_once('.') {
                Some((parent, _)) => prefix = parent,
                None => return false,
            }
        }
    }

    /// Apply `edit`, write the user file, and reload every layer.
    ///
    /// The user file keeps only values that differ from the built-in
    /// defaults and are not locked, so a later release's new defaults still
    /// reach users who never touched a setting.
    pub fn update(&mut self, edit: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
        let mut next = self.config.clone();
        edit(&mut next);
        let defaults = toml::Table::try_from(Config::default())?;
        let mut user = toml::Table::try_from(&next)?;
        prune(&mut user, &defaults, "", &|key| self.is_locked(key));
        write_atomic(&self.paths.user, &toml::to_string_pretty(&user)?)?;
        *self = Self::load_from(self.paths.clone())?;
        Ok(())
    }
}

fn read_table(path: &Path) -> anyhow::Result<Option<toml::Table>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.parse()?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Deep-merge `top` into `base`; tables merge, everything else replaces.
fn merge(base: &mut toml::Table, top: toml::Table) {
    for (key, value) in top {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(t)) => merge(b, t),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn leaf_keys(table: &toml::Table, prefix: &str, out: &mut BTreeSet<String>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(t) => leaf_keys(t, &path, out),
            _ => {
                out.insert(path);
            }
        }
    }
}

/// Drop entries equal to `defaults` or locked; drop tables left empty.
fn prune(
    table: &mut toml::Table,
    defaults: &toml::Table,
    prefix: &str,
    locked: &dyn Fn(&str) -> bool,
) {
    table.retain(|key, value| {
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };
        if locked(&path) {
            return false;
        }
        match (value, defaults.get(key)) {
            (toml::Value::Table(t), Some(toml::Value::Table(d))) => {
                prune(t, d, &path, locked);
                !t.is_empty()
            }
            (value, Some(default)) => value != default,
            (_, None) => true,
        }
    });
}

fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(user: &str, system: &str) -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let user_path = dir.path().join("user/config.toml");
        let system_path = dir.path().join("system.toml");
        if !user.is_empty() {
            fs::create_dir_all(user_path.parent().unwrap()).unwrap();
            fs::write(&user_path, user).unwrap();
        }
        fs::write(&system_path, system).unwrap();
        let store = ConfigStore::load_from(ConfigPaths {
            user: user_path,
            system: vec![system_path],
        })
        .unwrap();
        (dir, store)
    }

    #[test]
    fn system_layer_wins_and_locks() {
        let (_dir, store) = store(
            "[audio]\npreroll_ms = 100\nkeep_open = false\n",
            "[audio]\nkeep_open = true\n",
        );
        assert_eq!(store.config().audio.preroll_ms, 100);
        assert!(store.config().audio.keep_open);
        assert!(store.is_locked("audio.keep_open"));
        assert!(!store.is_locked("audio.preroll_ms"));
    }

    #[test]
    fn update_writes_only_unlocked_changes() {
        let (_dir, mut store) = store("", "[overlay]\nposition = \"top\"\n");
        store
            .update(|c| {
                c.history.limit = 10;
                c.overlay.position = OverlayPosition::Bottom;
            })
            .unwrap();
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert_eq!(written.trim(), "[history]\nlimit = 10");
        assert_eq!(store.config().history.limit, 10);
        assert_eq!(store.config().overlay.position, OverlayPosition::Top);
    }

    #[test]
    fn broken_user_file_is_skipped() {
        let (_dir, store) = store("[audio]\nkeep_opne = true\n", "");
        assert!(store.user_error().is_some());
        assert_eq!(store.config(), &Config::default());
    }
}
