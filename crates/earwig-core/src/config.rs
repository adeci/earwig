//! Settings in layers, later wins: built-in defaults, the user file (written
//! by the settings window), then the system files (the NixOS module's), whose
//! keys are locked and never written to the user file. See [`crate::paths`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::engine::Backend;
use crate::models::ModelSpec;
use crate::replacements;

const NO_PHRASE: &str = "[replacements] has an entry without a heard phrase";

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
    /// What earwig writes (the value) wherever the final text says a phrase
    /// (the key, as [`replacements::normalize`] spells it), before
    /// `hooks.filter` gets it. See [`replacements::apply`].
    pub replacements: BTreeMap<String, String>,
    pub hooks: HooksConfig,
    pub updates: UpdatesConfig,
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
    /// The key earwig watches itself (`Super+Space`, or modifiers alone such
    /// as `RightCtrl`); empty for none (compositor binds or the portal only).
    pub key: String,
    /// Linux: keep the key from other apps and the compositor by grabbing the
    /// keyboards that have it. macOS and Windows always do.
    pub block: bool,
    /// Register with the GlobalShortcuts portal when the desktop offers it
    /// and earwig cannot watch the key itself.
    pub portal: bool,
    /// Trigger suggested to the portal, in XDG shortcut syntax.
    pub portal_trigger: String,
}

impl HotkeyConfig {
    /// With the keyboards grabbed the compositor never sees it (GNOME switches
    /// input sources with it, sway's sample config toggles floating focus).
    #[cfg(target_os = "linux")]
    pub const DEFAULT_KEY: &str = "Super+Space";
    /// Option+Space: macOS keeps Command+Space for Spotlight and
    /// Control+Space for input sources.
    #[cfg(target_os = "macos")]
    pub const DEFAULT_KEY: &str = "Option+Space";
    #[cfg(windows)]
    pub const DEFAULT_KEY: &str = "Ctrl+Space";
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            mode: ActivationMode::HoldOrToggle,
            hold_threshold_ms: 300,
            key: Self::DEFAULT_KEY.into(),
            block: true,
            portal: true,
            portal_trigger: "CTRL+space".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// As `earwig devices` lists it (a sound server source on Linux, a device
    /// name on Windows); `None` = system default.
    pub device: Option<String>,
    /// Keep the microphone open between dictations so speech that starts as
    /// the key goes down is not clipped. The desktop's mic indicator stays on,
    /// which macOS shows in the menu bar and Windows in the taskbar: off there
    /// by default.
    pub keep_open: bool,
    /// Audio from before the press that is included in each recording.
    pub preroll_ms: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: None,
            keep_open: cfg!(target_os = "linux"),
            preroll_ms: 300,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    /// `None` = the release's default catalog model.
    pub model: Option<ModelSpec>,
    pub backend: Backend,
    /// Language hint for multilingual models.
    pub language: Option<String>,
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
    /// Extra executables treated as terminals (Ctrl+Shift+V) on top of the
    /// built-in list, on Windows; Linux pastes with Shift+Insert everywhere
    /// and ignores them.
    pub terminals: Vec<String>,
}

/// The colours of earwig's own windows, whatever the desktop prefers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeSetting {
    #[default]
    Dark,
    Light,
}

/// The accent of the overlay's pill (its border and glow).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccentSetting {
    /// earwig's own blue, the icon's.
    #[default]
    Earwig,
    /// The desktop's accent colour, live; earwig's blue where there is none.
    System,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppearanceConfig {
    pub theme: ThemeSetting,
    pub accent: AccentSetting,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayPosition {
    Top,
    #[default]
    Bottom,
}

/// How the overlay pill comes and goes; one that holds still (reduced motion,
/// software rendering) comes and goes at once either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayAnimation {
    /// Like an old CRT: springs open from a squashed bar, switches off to a
    /// line that shrinks to a dot.
    #[default]
    Crt,
    /// It appears and is gone at once.
    Instant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub position: OverlayPosition,
    /// Lines of live text shown before older lines scroll away.
    pub max_lines: u32,
    /// Opacity of the pill's fill (and the depth of its shadow), from
    /// [`OverlayConfig::MIN_OPACITY`] to 1; text and border stay opaque.
    #[serde(serialize_with = "short_float")]
    pub opacity: f32,
    /// Width of the pill's accent border in pixels, 0 (none, the default)
    /// to [`OverlayConfig::MAX_BORDER`].
    #[serde(serialize_with = "short_float")]
    pub border: f32,
    pub animation: OverlayAnimation,
}

/// TOML floats are `f64`; widening through the shortest decimal writes 0.7
/// rather than 0.699999988079071.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` signature
fn short_float<S: serde::Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(value.to_string().parse().unwrap_or(f64::from(*value)))
}

impl OverlayConfig {
    pub const DEFAULT_OPACITY: f32 = 0.97;
    pub const MIN_OPACITY: f32 = 0.3;
    pub const MAX_BORDER: f32 = 3.;

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
            opacity: Self::DEFAULT_OPACITY,
            border: 0.,
            animation: OverlayAnimation::default(),
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
/// itself (LLM cleanup, logging, notifications).
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

/// The Windows and macOS releases look for a newer release on GitHub; other
/// builds come from Nix, cargo or a package manager and never do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    /// Ask GitHub once a day whether a newer release is out.
    pub check: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { check: true }
    }
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
    /// The path to every leaf key set by a system file.
    locked: BTreeSet<Vec<String>>,
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

    /// A broken system file is an error (fix it where it is generated); a
    /// broken user file is skipped and reported by [`ConfigStore::user_error`].
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
            leaf_keys(&table, &mut Vec::new(), &mut locked);
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
        self.locks(&key.split('.'))
    }

    /// Whether a system file sets the replacement for `heard`, a phrase as
    /// [`replacements::normalize`] spells it.
    pub fn is_replacement_locked(&self, heard: &str) -> bool {
        self.locks(&["replacements", heard].into_iter())
    }

    /// Whether a locked key is at `path` or above it. A phrase is one step
    /// of a path, dots and all.
    fn locks<'a>(&self, path: &(impl Iterator<Item = &'a str> + Clone)) -> bool {
        self.locked.iter().any(|leaf| {
            let mut path = path.clone();
            leaf.iter().all(|key| path.next() == Some(key.as_str()))
        })
    }

    /// Apply `edit` and rewrite the user file with only the unlocked values
    /// that differ from the defaults, so new defaults still reach everyone else.
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
        // In the user file it would make that a broken one.
        anyhow::ensure!(
            next.replacements
                .keys()
                .all(|heard| !heard.trim().is_empty()),
            NO_PHRASE
        );
        let defaults = toml::Table::try_from(Config::default())?;
        let mut user = toml::Table::try_from(&next)?;
        prune(&mut user, &defaults, &mut Vec::new(), &|path| {
            self.locks(&path.iter().map(String::as_str))
        });
        crate::paths::write_atomic(&self.paths.user, toml::to_string_pretty(&user)?.as_bytes())?;
        *self = Self::load_from(self.paths.clone())?;
        Ok(())
    }
}

fn read_table(path: &Path) -> anyhow::Result<Option<toml::Table>> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let mut table: toml::Table = text.parse()?;
            key_phrases(&mut table)?;
            Ok(Some(table))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Key `[replacements]` by [`replacements::normalize`], so a phrase is one
/// entry whichever file sets it and however it is spelled there.
fn key_phrases(table: &mut toml::Table) -> anyhow::Result<()> {
    let Some(toml::Value::Table(entries)) = table.get_mut("replacements") else {
        return Ok(());
    };
    for (heard, written) in std::mem::take(entries) {
        let phrase = replacements::normalize(&heard);
        anyhow::ensure!(!phrase.is_empty(), NO_PHRASE);
        entries.insert(phrase, written);
    }
    Ok(())
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

fn leaf_keys(table: &toml::Table, path: &mut Vec<String>, out: &mut BTreeSet<Vec<String>>) {
    for (key, value) in table {
        path.push(key.clone());
        match value {
            toml::Value::Table(t) => leaf_keys(t, path, out),
            _ => {
                out.insert(path.clone());
            }
        }
        path.pop();
    }
}

/// Drop entries equal to `defaults` or locked; drop tables left empty.
fn prune(
    table: &mut toml::Table,
    defaults: &toml::Table,
    path: &mut Vec<String>,
    locked: &dyn Fn(&[String]) -> bool,
) {
    table.retain(|key, value| {
        path.push(key.to_owned());
        let keep = !locked(path)
            && match (value, defaults.get(key)) {
                (toml::Value::Table(t), Some(toml::Value::Table(d))) => {
                    prune(t, d, path, locked);
                    !t.is_empty()
                }
                (value, Some(default)) => value != default,
                (_, None) => true,
            };
        path.pop();
        keep
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
    fn a_setting_set_back_to_its_default_leaves_the_user_file() {
        let (_dir, mut store) = store("", "");
        store.update(|c| c.hotkey.key = "RightCtrl".into()).unwrap();
        store
            .update(|c| c.hotkey.key = HotkeyConfig::DEFAULT_KEY.into())
            .unwrap();
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert_eq!(written.trim(), "");
        assert_eq!(store.config().hotkey.key, HotkeyConfig::DEFAULT_KEY);
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
    fn overlay_looks_round_trip_and_clamp() {
        let (_dir, mut store) = store("", "");
        store
            .update(|c| {
                c.overlay.opacity = 0.7;
                c.overlay.border = 1.5;
            })
            .unwrap();
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert_eq!(written.trim(), "[overlay]\nborder = 1.5\nopacity = 0.7");
        let (_dir, reloaded) = self::store(&written, "");
        let overlay = &reloaded.config().overlay;
        assert!((overlay.opacity() - 0.7).abs() < f32::EPSILON);
        assert!((overlay.border() - 1.5).abs() < f32::EPSILON);

        let over = OverlayConfig {
            opacity: 3.,
            border: 10.,
            ..OverlayConfig::default()
        };
        assert!((over.opacity() - 1.).abs() < f32::EPSILON);
        assert!((over.border() - OverlayConfig::MAX_BORDER).abs() < f32::EPSILON);
        let under = OverlayConfig {
            opacity: 0.,
            border: -2.,
            ..OverlayConfig::default()
        };
        assert!((under.opacity() - OverlayConfig::MIN_OPACITY).abs() < f32::EPSILON);
        assert!(under.border().abs() < f32::EPSILON);
        let unreadable = OverlayConfig {
            border: f32::NAN,
            ..OverlayConfig::default()
        };
        assert!(unreadable.border().abs() < f32::EPSILON);
    }

    fn table(store: &ConfigStore) -> Vec<(&str, &str)> {
        store
            .config()
            .replacements
            .iter()
            .map(|(heard, written)| (heard.as_str(), written.as_str()))
            .collect()
    }

    #[test]
    fn a_system_replacement_is_locked_beside_the_users_own() {
        let (_dir, mut store) = store(
            "[replacements]\n'get hub' = 'GitHub'\n",
            "[replacements]\n'nix os' = 'NixOS'\n",
        );
        assert_eq!(table(&store), [("get hub", "GitHub"), ("nix os", "NixOS")]);
        assert!(store.is_replacement_locked("nix os"));
        assert!(!store.is_replacement_locked("get hub"));

        // The user's come and go; the system's stays, and out of the user file.
        store
            .update(|c| {
                c.replacements.insert("jujitsu".into(), "jujutsu".into());
                c.replacements.remove("get hub");
                c.replacements.remove("nix os");
            })
            .unwrap();
        assert_eq!(table(&store), [("jujitsu", "jujutsu"), ("nix os", "NixOS")]);
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert_eq!(written.trim(), "[replacements]\njujitsu = \"jujutsu\"");
    }

    #[test]
    fn the_system_file_wins_a_phrase_both_files_spell_differently() {
        let (_dir, mut store) = store(
            "[replacements]\n'Nix  OS' = 'nix-os'\n'get hub' = 'GitHub'\n",
            "[replacements]\n'nix os' = 'NixOS'\n",
        );
        assert_eq!(table(&store), [("get hub", "GitHub"), ("nix os", "NixOS")]);
        assert!(store.is_replacement_locked("nix os"));

        // The next save drops the user's entry for it.
        store.update(|c| c.history.limit = 10).unwrap();
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert!(written.contains("\"get hub\" = \"GitHub\""), "{written}");
        assert!(!written.to_lowercase().contains("nix"), "{written}");
    }

    #[test]
    fn a_phrase_with_a_dot_is_not_locked_by_a_shorter_one() {
        let (_dir, mut store) = store(
            "[replacements]\n'node.js' = 'Node.js'\n",
            "[replacements]\nnode = 'Node'\n",
        );
        assert!(store.is_replacement_locked("node"));
        assert!(!store.is_replacement_locked("node.js"));

        store.update(|c| c.history.limit = 10).unwrap();
        assert_eq!(table(&store), [("node", "Node"), ("node.js", "Node.js")]);
        let written = fs::read_to_string(&store.paths().user).unwrap();
        assert!(written.contains("\"node.js\" = \"Node.js\""), "{written}");
    }

    #[test]
    fn a_replacement_without_a_heard_phrase_is_refused() {
        let phraseless = "[replacements]\n' ' = 'x'\n";
        let (dir, store) = store(phraseless, "");
        let error = store.user_error().unwrap_or_default();
        assert!(error.contains("heard phrase"), "{error}");
        assert_eq!(store.config(), &Config::default());

        let system = dir.path().join("system.toml");
        fs::write(&system, phraseless).unwrap();
        let error = ConfigStore::load_from(ConfigPaths {
            user: dir.path().join("none.toml"),
            system: vec![system],
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("heard phrase"), "{error:#}");

        let (_dir, mut store) = self::store("", "");
        let error = store
            .update(|c| {
                c.replacements.insert(" ".into(), "x".into());
            })
            .unwrap_err();
        assert!(error.to_string().contains("heard phrase"), "{error}");
        assert!(!store.paths().user.exists());
    }
}
