//! Privacy permissions the user grants by hand: on macOS, Microphone and
//! Accessibility (which also covers the hotkey's event tap). macOS ties an
//! ad-hoc signed app's grants to that exact build, so each build asks once for
//! what it lacks, clearing earwig's stale entry first.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The file in the state directory that names the build which last asked.
const ASKED: &str = "permissions-asked";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    Microphone,
    Accessibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    Granted,
    Denied,
    /// The system has not asked the user yet.
    NotAsked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionStatus {
    pub permission: Permission,
    pub access: Access,
}

impl Permission {
    /// As System Settings names it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Microphone => "Microphone",
            Self::Accessibility => "Accessibility",
        }
    }

    /// What earwig cannot do without it.
    pub fn purpose(self) -> &'static str {
        match self {
            Self::Microphone => "hear you",
            Self::Accessibility => "see the hotkey and paste",
        }
    }
}

/// Every permission this system asks for, and whether it is granted.
pub fn check() -> Vec<PermissionStatus> {
    #[cfg(target_os = "macos")]
    {
        use crate::macos;
        let answered = |answer: Option<bool>| match answer {
            Some(true) => Access::Granted,
            Some(false) => Access::Denied,
            None => Access::NotAsked,
        };
        vec![
            PermissionStatus {
                permission: Permission::Microphone,
                access: answered(macos::microphone_allowed()),
            },
            PermissionStatus {
                permission: Permission::Accessibility,
                // The system keeps no "asked" state for this one.
                access: if macos::accessibility_allowed() {
                    Access::Granted
                } else {
                    Access::Denied
                },
            },
        ]
    }
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

/// Show the system's prompt for each missing permission, once per build
/// (remembered in `state_dir`) so a no isn't asked again at every login.
/// Returns what is missing.
pub fn ask_once(state_dir: &Path) -> Vec<Permission> {
    let missing: Vec<Permission> = check()
        .into_iter()
        .filter(|status| status.access != Access::Granted)
        .map(|status| status.permission)
        .collect();
    if !missing.is_empty() {
        ask_once_per_build(state_dir, &build(), &missing, ask);
    }
    missing
}

/// `ask` for each of `missing`, unless `build` has asked before.
fn ask_once_per_build(
    state_dir: &Path,
    build: &str,
    missing: &[Permission],
    mut ask: impl FnMut(Permission),
) {
    let marker = state_dir.join(ASKED);
    if fs::read_to_string(&marker).is_ok_and(|asked| asked == build) {
        return;
    }
    // Remembered first, so a start that fails while asking won't ask again.
    if let Err(e) = fs::create_dir_all(state_dir).and_then(|()| fs::write(&marker, build)) {
        log::warn!("remembering that permissions were asked for: {e}");
    }
    log::info!("asking for {missing:?}");
    for &permission in missing {
        ask(permission);
    }
}

/// This build as macOS tells builds apart: by its code signature's hash.
fn build() -> String {
    #[cfg(target_os = "macos")]
    if let Some(hash) = crate::macos::code_hash() {
        return hex::encode(hash);
    }
    "unsigned".to_owned()
}

/// Show the system's prompt for `permission` unless it is granted, first
/// clearing what keeps the prompt away (a no to Microphone, or an
/// Accessibility entry, such as one still ticked for an earlier build).
pub fn ask(permission: Permission) {
    #[cfg(target_os = "macos")]
    {
        use crate::macos;
        match permission {
            Permission::Microphone => match macos::microphone_allowed() {
                Some(true) => {}
                Some(false) => {
                    forget(permission);
                    macos::request_microphone();
                }
                None => macos::request_microphone(),
            },
            Permission::Accessibility => {
                if !macos::accessibility_allowed() {
                    forget(permission);
                    macos::request_accessibility();
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = permission;
}

/// Clear earwig's entry for `permission` (`tccutil reset`) so the system asks
/// again; only from earwig.app, whose entry it is.
#[cfg(target_os = "macos")]
fn forget(permission: Permission) {
    if crate::macos::bundle_id().as_deref() != Some(crate::APP_ID) {
        return;
    }
    let service = match permission {
        Permission::Microphone => "Microphone",
        Permission::Accessibility => "Accessibility",
    };
    match std::process::Command::new("/usr/bin/tccutil")
        .args(["reset", service, crate::APP_ID])
        .output()
    {
        Ok(output) if output.status.success() => log::info!("cleared earwig's {service} entry"),
        Ok(output) => log::warn!(
            "clearing earwig's {service} entry: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(e) => log::warn!("running tccutil: {e}"),
    }
}

/// Open the System Settings page where `permission` is granted.
pub fn open_settings(permission: Permission) {
    #[cfg(target_os = "macos")]
    {
        let anchor = match permission {
            Permission::Microphone => "Privacy_Microphone",
            Permission::Accessibility => "Privacy_Accessibility",
        };
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
        if let Err(e) = std::process::Command::new("/usr/bin/open")
            .arg(&url)
            .spawn()
        {
            log::warn!("opening {url}: {e}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = permission;
}

/// "earwig needs Microphone access to hear you: ...", for error messages.
pub fn missing_message(permission: Permission) -> String {
    format!(
        "earwig needs {} access to {}: allow it in System Settings → Privacy & Security → {}",
        permission.label(),
        permission.purpose(),
        permission.label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use Permission::{Accessibility, Microphone};

    const NOTHING: [Permission; 0] = [];

    /// What a start of `build` asks for while `missing` is.
    fn asked(state_dir: &Path, build: &str, missing: &[Permission]) -> Vec<Permission> {
        let mut asked = Vec::new();
        ask_once_per_build(state_dir, build, missing, |permission| {
            asked.push(permission);
        });
        asked
    }

    #[test]
    fn each_build_asks_once_for_what_it_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        assert_eq!(
            asked(&state, "a", &[Microphone, Accessibility]),
            [Microphone, Accessibility]
        );
        // The same build started again, at the next login say, does not.
        assert_eq!(asked(&state, "a", &[Microphone, Accessibility]), NOTHING);
        // An update, which macOS grants nothing yet, asks again; once.
        assert_eq!(asked(&state, "b", &[Accessibility]), [Accessibility]);
        assert_eq!(asked(&state, "b", &[Accessibility]), NOTHING);
        // Going back asks again too: asking for the update cleared what
        // the first build was granted.
        assert_eq!(asked(&state, "a", &[Accessibility]), [Accessibility]);
    }

    /// Test binaries are signed ad hoc on Apple silicon, as earwig.app is.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_build_is_known_by_the_hash_macos_keeps_its_grants_for() {
        let exe = std::env::current_exe().unwrap();
        let signature = std::process::Command::new("/usr/bin/codesign")
            .arg("-dvvv")
            .arg(&exe)
            .output()
            .unwrap();
        let signature = String::from_utf8_lossy(&signature.stderr);
        let cdhash = signature
            .lines()
            .find_map(|line| line.strip_prefix("CDHash="))
            .unwrap_or_else(|| panic!("no CDHash in:\n{signature}"));
        assert_eq!(build(), cdhash);
    }
}
