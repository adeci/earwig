//! Privacy permissions the user grants the OS by hand. On macOS those are
//! Microphone (to hear you) and Accessibility (to see the hotkey and paste;
//! an app trusted with Accessibility needs no Input Monitoring for its event
//! tap); other systems ask for none.
//!
//! macOS keeps the grants of an ad-hoc signed app, as earwig.app is, for
//! that exact build. After an update System Settings still shows the old
//! earwig ticked, and ticking it again allows only the old build. So each
//! build asks once for what it lacks, and asking first clears earwig's
//! stale entry, which puts the new build in the list.

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

/// Show the system's own prompt for each permission that is missing, once
/// for each build of earwig (remembered in `state_dir`): a new build starts
/// without the grants of the last, while a user who said no is not asked
/// again at every login. Returns what is missing.
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
    // Remembered before asking: a start that fails while asking (and is
    // started again) does not ask again.
    if let Err(e) = fs::create_dir_all(state_dir).and_then(|()| fs::write(&marker, build)) {
        log::warn!("remembering that permissions were asked for: {e}");
    }
    log::info!("asking for {missing:?}");
    for &permission in missing {
        ask(permission);
    }
}

/// This build of earwig as macOS tells builds apart: by the hash of its code
/// signature, which an ad-hoc signature ties the grants to.
fn build() -> String {
    #[cfg(target_os = "macos")]
    if let Some(hash) = crate::macos::code_hash() {
        return hex::encode(hash);
    }
    "unsigned".to_owned()
}

/// Show the system's prompt for `permission` unless it is granted. The
/// system asks only once, so first clear what would keep its prompt away:
/// a no to Microphone, or any Accessibility entry for earwig, such as the
/// one System Settings keeps ticked for an earlier build.
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

/// Clear earwig's entry for `permission` from the system's privacy database
/// (`tccutil reset`), so that the system asks again. Only from earwig.app,
/// whose entry it is, not from a bare `cargo run` build.
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

    #[test]
    fn having_asked_before_builds_were_told_apart_does_not_keep_a_build_from_asking() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(ASKED), "").unwrap();
        assert_eq!(asked(dir.path(), "a", &[Accessibility]), [Accessibility]);
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
