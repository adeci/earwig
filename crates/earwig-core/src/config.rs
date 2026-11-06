//! Settings in three layers, later wins: built-in defaults, the user file
//! `$XDG_CONFIG_HOME/earwig/config.toml` (written by the settings window), and
//! `earwig/config.toml` under `$XDG_CONFIG_DIRS` (written by the NixOS module).
//! Keys set in a system file are locked: never written to the user file.

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
    pub appearance: AppearanceConfig,
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
            portal_trigger: "CTRL+space".into(),
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InsertConfig {
    pub method: InsertSetting,
    /// Extra app ids treated as terminals (Ctrl+Shift+V) on top of the
    /// built-in list.
    pub terminals: Vec<String>,
}

/// The colours of earwig's own windows (the settings window and the
/// overlay), whatever the desktop's light or dark preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeSetting {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearanceConfig {
    pub theme: ThemeSetting,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayPosition {
    Top,
    #[default]
    Bottom,
}

/// How the overlay pill shows the voice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayStyle {
    /// The pill's outline behaves like water the voice disturbs: speech
    /// sends ripples around it that ring on and settle after it.
    #[default]
    Wave,
    /// Sound bars above and below a still pill, one pair per speech band.
    Bars,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub position: OverlayPosition,
    /// Lines of live text shown before older lines scroll away.
    pub max_lines: u32,
    pub style: OverlayStyle,
    /// How far the voice moves the pill (bar height or wave amplitude): 0
    /// is still, up to [`OverlayConfig::MAX_MOTION`].
    #[serde(serialize_with = "short_float")]
    pub motion: f32,
    /// Opacity of the pill's fill (and the depth of its shadow), from
    /// [`OverlayConfig::MIN_OPACITY`] to 1; text, bars and border stay
    /// opaque.
    #[serde(serialize_with = "short_float")]
    pub opacity: f32,
    /// Width of the pill's accent border in pixels, 0 (none, the default)
    /// to [`OverlayConfig::MAX_BORDER`].
    #[serde(serialize_with = "short_float")]
    pub border: f32,
}

/// TOML floats are `f64`; widening through the shortest decimal writes 0.7
/// rather than 0.699999988079071.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` signature
fn short_float<S: serde::Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(value.to_string().parse().unwrap_or(f64::from(*value)))
}

impl OverlayConfig {
    pub const MAX_MOTION: f32 = 2.;
    pub const DEFAULT_MOTION: f32 = 0.8;
    pub const DEFAULT_OPACITY: f32 = 0.97;
    pub const MIN_OPACITY: f32 = 0.3;
    pub const MAX_BORDER: f32 = 3.;

    /// `motion` within its range; an unreadable value means the default.
    pub fn motion(&self) -> f32 {
        if self.motion.is_nan() {
            Self::DEFAULT_MOTION
        } else {
            self.motion.clamp(0., Self::MAX_MOTION)
        }
    }

    /// `opacity` within its range; an unreadable value means the default.
    pub fn opacity(&self) -> f32 {
        if self.opacity.is_nan() {
            Self::DEFAULT_OPACITY
        } else {
            self.opacity.clamp(Self::MIN_OPACITY, 1.)
        }
    }

    /// `border` within its range; an unreadable value means none.
    pub fn border(&self) -> f32 {
        if self.border.is_nan() {
            0.
        } else {
            self.border.clamp(0., Self::MAX_BORDER)
        }
    }
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position: OverlayPosition::Bottom,
            max_lines: 3,
            style: OverlayStyle::default(),
            motion: Self::DEFAULT_MOTION,
            opacity: Self::DEFAULT_OPACITY,
            border: 0.,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigPaths {
    pub user: PathBuf,
    /// System files that exist, lowest precedence first.
    pub system: Vec<PathBuf>,
}

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
        // Writing now would replace the user's broken file with defaults.
        if let Some(e) = &self.user_error {
            anyhow::bail!(
                "not saving: fix or remove {} first ({e})",
                self.paths.user.display()
            );
        }
        let mut next = self.config.clone();
        edit(&mut next);
        let defaults = toml::Table::try_from(Config::default())?;
        let mut user = toml::Table::try_from(&next)?;
        prune(&mut user, &defaults, "", &|key| self.is_locked(key));
        crate::paths::write_atomic(&self.paths.user, toml::to_string_pretty(&user)?.as_bytes())?;
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

    #[test]
    fn update_refuses_to_overwrite_a_broken_user_file() {
        let broken = "[audio]\nkeep_opne = true\n";
        let (_dir, mut store) = store(broken, "");
        let err = store.update(|c| c.history.limit = 10).unwrap_err();
        assert!(err.to_string().contains("config.toml"), "{err}");
        assert_eq!(fs::read_to_string(&store.paths().user).unwrap(), broken);
    }

    #[test]
    fn overlay_looks_default_round_trip_and_clamp() {
        let (_dir, mut store) = store("", "");
        let overlay = &store.config().overlay;
        assert_eq!(overlay.style, OverlayStyle::Wave);
        assert!((overlay.motion() - OverlayConfig::DEFAULT_MOTION).abs() < f32::EPSILON);
        assert!((overlay.opacity() - 0.97).abs() < f32::EPSILON);
        assert!(overlay.border().abs() < f32::EPSILON, "borderless");
        store
            .update(|c| {
                c.overlay.style = OverlayStyle::Bars;
                c.overlay.motion = 1.5;
                c.overlay.opacity = 0.7;
                c.overlay.border = 1.5;
            })
            .unwrap();
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert_eq!(
            written.trim(),
            "[overlay]\nborder = 1.5\nmotion = 1.5\nopacity = 0.7\nstyle = \"bars\""
        );
        let (_dir, reloaded) = self::store(&written, "");
        let overlay = &reloaded.config().overlay;
        assert_eq!(overlay.style, OverlayStyle::Bars);
        assert!((overlay.motion() - 1.5).abs() < f32::EPSILON);
        assert!((overlay.opacity() - 0.7).abs() < f32::EPSILON);
        assert!((overlay.border() - 1.5).abs() < f32::EPSILON);

        let over = OverlayConfig {
            motion: 7.,
            opacity: 3.,
            border: 10.,
            ..OverlayConfig::default()
        };
        assert!((over.motion() - OverlayConfig::MAX_MOTION).abs() < f32::EPSILON);
        assert!((over.opacity() - 1.).abs() < f32::EPSILON);
        assert!((over.border() - OverlayConfig::MAX_BORDER).abs() < f32::EPSILON);
        let under = OverlayConfig {
            motion: -1.,
            opacity: 0.,
            border: -2.,
            ..OverlayConfig::default()
        };
        assert!(under.motion().abs() < f32::EPSILON);
        assert!((under.opacity() - OverlayConfig::MIN_OPACITY).abs() < f32::EPSILON);
        assert!(under.border().abs() < f32::EPSILON);
        let unreadable = OverlayConfig {
            border: f32::NAN,
            ..OverlayConfig::default()
        };
        assert!(unreadable.border().abs() < f32::EPSILON);
    }

    #[test]
    fn theme_defaults_to_dark_round_trips_and_rejects_unknown() {
        let (_dir, mut store) = store("", "");
        assert_eq!(store.config().appearance.theme, ThemeSetting::Dark);
        store
            .update(|c| c.appearance.theme = ThemeSetting::Light)
            .unwrap();
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert_eq!(written.trim(), "[appearance]\ntheme = \"light\"");
        let (_dir, reloaded) = self::store(&written, "");
        assert_eq!(reloaded.config().appearance.theme, ThemeSetting::Light);

        let (_dir, store) = self::store("[appearance]\ntheme = \"system\"\n", "");
        assert!(store.user_error().is_some());
        assert_eq!(store.config().appearance.theme, ThemeSetting::Dark);
    }
}
