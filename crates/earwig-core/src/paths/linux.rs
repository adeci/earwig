//! Linux: XDG base directories, `$XDG_*` first.

use std::env;
use std::path::PathBuf;

use super::APP;

fn xdg(var: &str, fallback: impl FnOnce() -> Option<PathBuf>) -> PathBuf {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(fallback)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$XDG_CONFIG_HOME/earwig`
pub(super) fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", dirs::config_dir).join(APP)
}

/// `earwig` in each `$XDG_CONFIG_DIRS` entry (`/etc/xdg` when unset); the
/// NixOS module writes there.
pub(super) fn system_config_dirs() -> Vec<PathBuf> {
    search_path("XDG_CONFIG_DIRS", "/etc/xdg")
}

/// `$XDG_DATA_HOME/earwig`
pub(super) fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", dirs::data_dir).join(APP)
}

/// `earwig` in each `$XDG_DATA_DIRS` entry (`/usr/local/share:/usr/share`
/// when unset). Distro packages and the NixOS module put pinned models here.
pub(super) fn system_data_dirs() -> Vec<PathBuf> {
    search_path("XDG_DATA_DIRS", "/usr/local/share:/usr/share")
}

/// `$XDG_STATE_HOME/earwig`
pub(super) fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", dirs::state_dir).join(APP)
}

/// `$XDG_RUNTIME_DIR/earwig.sock`
pub(super) fn socket() -> PathBuf {
    xdg("XDG_RUNTIME_DIR", dirs::runtime_dir).join(format!("{APP}.sock"))
}

/// `earwig` in each absolute entry of the colon-separated `var`, most
/// important first (the spec's order).
fn search_path(var: &str, default: &str) -> Vec<PathBuf> {
    let dirs = env::var(var).unwrap_or_default();
    let dirs = if dirs.is_empty() {
        default.to_owned()
    } else {
        dirs
    };
    dirs.split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| PathBuf::from(d).join(APP))
        .collect()
}
