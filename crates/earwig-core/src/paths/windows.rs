//! Windows: settings in `%APPDATA%\earwig` (they roam), models, history and
//! state in `%LOCALAPPDATA%\earwig` (they stay on the machine), machine-wide
//! files in `%ProgramData%\earwig`.

use std::env;
use std::path::PathBuf;

use super::APP;

/// `%APPDATA%\earwig`
pub(super) fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(env::temp_dir).join(APP)
}

pub(super) fn system_config_dirs() -> Vec<PathBuf> {
    vec![program_data()]
}

/// `%LOCALAPPDATA%\earwig`
pub(super) fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(env::temp_dir)
        .join(APP)
}

pub(super) fn system_data_dirs() -> Vec<PathBuf> {
    vec![program_data()]
}

/// `%LOCALAPPDATA%\earwig\state`
pub(super) fn state_dir() -> PathBuf {
    data_dir().join("state")
}

/// `\\.\pipe\earwig-<user SID>`: pipe names are machine-wide, so each user
/// gets their own (and only that user may open it, see [`crate::ipc`]).
pub(super) fn socket() -> PathBuf {
    match crate::ipc::user_sid() {
        Ok(sid) => PathBuf::from(format!(r"\\.\pipe\{APP}-{sid}")),
        Err(e) => {
            log::warn!("no user SID for the pipe name: {e}");
            PathBuf::from(format!(r"\\.\pipe\{APP}"))
        }
    }
}

/// `%ProgramData%\earwig`
fn program_data() -> PathBuf {
    env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
        .join(APP)
}

#[cfg(test)]
mod tests {
    #[test]
    fn settings_roam_and_models_stay_local() {
        let config = super::super::user_config();
        assert!(
            config.ends_with(r"AppData\Roaming\earwig\config.toml"),
            "{}",
            config.display()
        );
        let models = super::super::models_dir();
        assert!(
            models.ends_with(r"AppData\Local\earwig\models"),
            "{}",
            models.display()
        );
        let pipe = super::super::socket();
        let pipe = pipe.to_str().unwrap();
        assert!(pipe.starts_with(r"\\.\pipe\earwig-S-1-5-"), "{pipe}");
    }
}
