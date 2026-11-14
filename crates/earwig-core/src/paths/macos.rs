//! macOS: everything under `~/Library/Application Support/earwig`; the
//! machine-wide counterpart is `/Library/Application Support/earwig`.

use std::env;
use std::path::PathBuf;

use super::APP;

const SYSTEM: &str = "/Library/Application Support";

/// `~/Library/Application Support/earwig`
fn app_support() -> PathBuf {
    dirs::data_dir().unwrap_or_else(env::temp_dir).join(APP)
}

pub(super) fn config_dir() -> PathBuf {
    app_support()
}

pub(super) fn system_config_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from(SYSTEM).join(APP)]
}

pub(super) fn data_dir() -> PathBuf {
    app_support()
}

pub(super) fn system_data_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from(SYSTEM).join(APP)]
}

/// `~/Library/Application Support/earwig/state`
pub(super) fn state_dir() -> PathBuf {
    app_support().join("state")
}

/// `$TMPDIR/earwig.sock`: macOS gives each user a private `$TMPDIR`.
pub(super) fn socket() -> PathBuf {
    env::temp_dir().join(format!("{APP}.sock"))
}
