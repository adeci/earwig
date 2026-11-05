//! Where earwig keeps things (XDG base directories).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{env, fs, io};

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
fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", dirs::data_dir).join(APP)
}

/// Downloaded models.
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

/// Read-only model stores: `earwig/models` in each `$XDG_DATA_DIRS` entry,
/// most important first (`/usr/local/share:/usr/share` when unset). Distro
/// packages and the NixOS module put pinned models here.
pub fn system_model_dirs() -> Vec<PathBuf> {
    let dirs = env::var("XDG_DATA_DIRS").unwrap_or_default();
    let dirs = if dirs.is_empty() {
        "/usr/local/share:/usr/share".to_owned()
    } else {
        dirs
    };
    dirs.split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| PathBuf::from(d).join(APP).join("models"))
        .collect()
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

/// Replace `path` with `contents` via a temp file in the same directory, so
/// readers see the old file or the new one, never a partial write.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&tmp, contents)
        .and_then(|()| fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
}
