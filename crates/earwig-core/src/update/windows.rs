//! Windows: the installer's own install of earwig updates by running the new
//! installer silently, which stops earwig, replaces it and starts it again;
//! a copy anywhere else (the portable zip, a build) only says a release is
//! out.

use std::ffi::OsString;
use std::io;
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

use super::Mode;
use crate::win32;

/// Where the installer (`AppId` in packaging/windows/earwig.iss) records the
/// install.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{6A3C4F2E-8F1B-4C57-9E0D-2B7E5A1D9C41}_is1";

/// Set when packaging/windows/package.ps1 compiles a release package: only
/// those builds look for updates, not other cargo builds.
const RELEASE_PACKAGE: bool = option_env!("EARWIG_RELEASE_PACKAGE").is_some();

/// What a release package does: installs updates from the installer's own
/// install, says they are out elsewhere (the portable zip).
pub(super) fn detect() -> Option<Mode> {
    if !RELEASE_PACKAGE {
        return None;
    }
    let canonical = |dir: &Path| std::fs::canonicalize(dir).ok();
    let installed = win32::user_string(UNINSTALL_KEY, "InstallLocation")
        .and_then(|dir| canonical(Path::new(&dir)));
    let here = std::env::current_exe()
        .ok()
        .and_then(|exe| canonical(exe.parent()?));
    Some(if installed.is_some() && installed == here {
        Mode::Install
    } else {
        Mode::Notify
    })
}

/// Start `setup` silently in its own folder (the private download folder),
/// detached from earwig, which it stops (a silent upgrade leaves Start at
/// sign-in as it is); its log goes to `log`.
pub(super) fn launch(setup: &Path, log: &Path) -> io::Result<Child> {
    let mut log_arg = OsString::from("/LOG=");
    log_arg.push(log);
    let mut installer = Command::new(setup);
    if let Some(folder) = setup.parent() {
        installer.current_dir(folder);
    }
    installer
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .arg(log_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
}
