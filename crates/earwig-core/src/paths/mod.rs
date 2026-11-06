//! Where earwig keeps things. Each OS module provides the base directories:
//! XDG on Linux (`$XDG_*` first), Application Support on macOS, the known
//! folders on Windows.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{fs, io};

const APP: &str = "earwig";

/// The user's settings file, e.g. `$XDG_CONFIG_HOME/earwig/config.toml`.
pub fn user_config() -> PathBuf {
    os::config_dir().join("config.toml")
}

/// Machine-wide settings files that exist, lowest precedence first (e.g.
/// `earwig/config.toml` under `$XDG_CONFIG_DIRS`, whose spec lists the most
/// important directory first).
pub fn system_configs() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = os::system_config_dirs()
        .into_iter()
        .map(|d| d.join("config.toml"))
        .filter(|p| p.is_file())
        .collect();
    files.reverse();
    files
}

/// Downloaded models.
pub fn models_dir() -> PathBuf {
    os::data_dir().join("models")
}

/// Read-only model stores, most important first (e.g. `earwig/models` in
/// each `$XDG_DATA_DIRS` entry).
pub fn system_model_dirs() -> Vec<PathBuf> {
    os::system_data_dirs()
        .into_iter()
        .map(|d| d.join("models"))
        .collect()
}

pub fn history_file() -> PathBuf {
    os::data_dir().join("history.jsonl")
}

/// Portal restore tokens and similar, e.g. `$XDG_STATE_HOME/earwig`.
pub fn state_dir() -> PathBuf {
    os::state_dir()
}

/// Where the daemon listens for [`crate::ipc`]: a Unix socket path, e.g.
/// `$XDG_RUNTIME_DIR/earwig.sock`, or a named pipe on Windows.
pub fn socket() -> PathBuf {
    os::socket()
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
