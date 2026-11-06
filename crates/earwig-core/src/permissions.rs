//! Privacy permissions the user grants the OS by hand. On macOS those are
//! Microphone (to hear you), Accessibility (to paste) and Input Monitoring
//! (to see the hotkey); other systems ask for none.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    Microphone,
    Accessibility,
    InputMonitoring,
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
            Self::InputMonitoring => "Input Monitoring",
        }
    }

    /// What earwig cannot do without it.
    pub fn purpose(self) -> &'static str {
        match self {
            Self::Microphone => "hear you",
            Self::Accessibility => "paste the text where you are typing",
            Self::InputMonitoring => "see the dictation key",
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
            PermissionStatus {
                permission: Permission::InputMonitoring,
                access: answered(macos::input_monitoring_allowed()),
            },
        ]
    }
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

/// Show the system's own prompt for each permission that is missing, the
/// first time earwig starts without them (remembered in `state_dir`), so a
/// user who said no is not asked at every login. Returns what is missing.
pub fn ask_once(state_dir: &Path) -> Vec<Permission> {
    let missing: Vec<Permission> = check()
        .into_iter()
        .filter(|status| status.access != Access::Granted)
        .map(|status| status.permission)
        .collect();
    let marker = state_dir.join("permissions-asked");
    if missing.is_empty() || marker.exists() {
        return missing;
    }
    log::info!("asking for {missing:?}");
    for permission in &missing {
        request(*permission);
    }
    if let Err(e) = fs::create_dir_all(state_dir).and_then(|()| fs::write(&marker, "")) {
        log::warn!("remembering that permissions were asked for: {e}");
    }
    missing
}

/// The system prompt for `permission` (it shows nothing once answered).
pub fn request(permission: Permission) {
    #[cfg(target_os = "macos")]
    {
        use crate::macos;
        match permission {
            Permission::Microphone => macos::request_microphone(),
            Permission::Accessibility => macos::request_accessibility(),
            Permission::InputMonitoring => macos::request_input_monitoring(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = permission;
}

/// Open the System Settings page where `permission` is granted.
pub fn open_settings(permission: Permission) {
    #[cfg(target_os = "macos")]
    {
        let anchor = match permission {
            Permission::Microphone => "Privacy_Microphone",
            Permission::Accessibility => "Privacy_Accessibility",
            Permission::InputMonitoring => "Privacy_ListenEvent",
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
