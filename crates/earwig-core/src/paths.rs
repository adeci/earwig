//! Where earwig keeps things (XDG base directories).

use std::env;
use std::path::PathBuf;

const APP: &str = "earwig";

fn xdg(var: &str, fallback: impl FnOnce() -> Option<PathBuf>) -> PathBuf {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(fallback)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$XDG_CONFIG_HOME/earwig/config.toml`
pub fn user_config() -> PathBuf {
    xdg("XDG_CONFIG_HOME", dirs::config_dir)
        .join(APP)
        .join("config.toml")
}

/// `earwig/config.toml` in each `$XDG_CONFIG_DIRS` entry that exists, lowest
/// precedence first (the spec lists the most important directory first).
pub fn system_configs() -> Vec<PathBuf> {
    let dirs = env::var("XDG_CONFIG_DIRS").unwrap_or_default();
    let dirs = if dirs.is_empty() {
        "/etc/xdg".to_owned()
    } else {
        dirs
    };
    let mut files: Vec<PathBuf> = dirs
        .split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| PathBuf::from(d).join(APP).join("config.toml"))
        .filter(|p| p.is_file())
        .collect();
    files.reverse();
    files
}

/// `$XDG_DATA_HOME/earwig`
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", dirs::data_dir).join(APP)
}

/// Downloaded models.
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn history_file() -> PathBuf {
    data_dir().join("history.jsonl")
}

/// `$XDG_STATE_HOME/earwig`: portal restore tokens and similar.
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", dirs::state_dir).join(APP)
}

/// `$XDG_RUNTIME_DIR/earwig.sock`
pub fn socket() -> PathBuf {
    xdg("XDG_RUNTIME_DIR", dirs::runtime_dir).join(format!("{APP}.sock"))
}
