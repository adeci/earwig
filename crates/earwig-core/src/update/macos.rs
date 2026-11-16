//! macOS: earwig.app replaces itself with the one in the release's disk
//! image, once that is signed so that it meets the running app's designated
//! requirement (same identifier, same Developer ID team) and is newer.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use semver::Version;

use super::{Mode, UpdateError, developer_id_requirement, mac_mode, settle, staged, swap};
use crate::macos::{self as mac, Requirement};

/// The running earwig.app and what it may do; `None` when it never updates.
pub(super) fn detect() -> Option<(PathBuf, Mode)> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    mac_mode(&exe, mac::own_team().as_deref(), |path| {
        rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok()
    })
}

/// Start the app an update put in place as LaunchServices starts apps, so
/// that it answers for its own privacy grants; a new instance, as this one
/// is still on its way out.
pub fn relaunch(app: &Path, headless: bool) {
    let mut open = Command::new("/usr/bin/open");
    open.arg("-n").arg(app);
    if headless {
        open.args(["--args", "daemon", "--headless"]);
    }
    match open.stdin(Stdio::null()).status() {
        Ok(status) if status.success() => log::info!("started {}", app.display()),
        Ok(status) => log::error!("starting {}: open stopped ({status})", app.display()),
        Err(e) => log::error!("starting {}: {e}", app.display()),
    }
}

/// The app an update replaces, and what the new one must live up to.
pub(super) struct Running<'a> {
    pub app: &'a Path,
    pub version: &'a Version,
    requirement: Requirement,
}

impl<'a> Running<'a> {
    /// This program, by the requirement its own signature designates, which
    /// must be the usual one for a Developer ID: Apple's anchor and its team.
    pub(super) fn this(app: &'a Path, version: &'a Version) -> Result<Self, UpdateError> {
        let refuse = |why: &str| UpdateError::Install(format!("this earwig's signature {why}"));
        let requirement = Requirement::own().ok_or_else(|| refuse("designates no requirement"))?;
        let team = mac::own_team().ok_or_else(|| refuse("names no team"))?;
        if !developer_id_requirement(&requirement.text(), &team) {
            return Err(refuse(&format!(
                "designates an unusual requirement: {}",
                requirement.text()
            )));
        }
        Ok(Self {
            app,
            version,
            requirement,
        })
    }

    /// The app at `app` as signed on disk: in tests, the program running is
    /// the test.
    #[cfg(test)]
    pub(super) fn on_disk(app: &'a Path, version: &'a Version) -> Self {
        Self {
            app,
            version,
            requirement: Requirement::of(app).expect("a signed app"),
        }
    }
}

/// Install the earwig.app in `dmg`, a release of `version`, over the running
/// app: the image attached read-only in `work`, its app checked, copied next
/// to the running one, checked again there and swapped in; the old app is
/// deleted. Returns where the new app is.
pub(super) fn install(
    dmg: &Path,
    version: &Version,
    running: &Running,
    work: &Path,
) -> Result<PathBuf, UpdateError> {
    log::info!(
        "installing {} if it meets {}",
        dmg.display(),
        running.requirement.text()
    );
    let mount = work.join("mount");
    fs::create_dir_all(&mount)?;
    let image = Image::attach(dmg, &mount)?;
    let app = mount.join("earwig.app");
    check(&app, version, running)?;
    let staged = staged(running.app);
    remove(&staged)?;
    if let Err(e) = run(Command::new("/usr/bin/ditto").arg(&app).arg(&staged)) {
        let _ = fs::remove_dir_all(&staged);
        return Err(UpdateError::Install(format!(
            "copying the new app next to this one: {e}"
        )));
    }
    drop(image);
    let swapped = check(&staged, version, running).and_then(|()| {
        swap(running.app, &staged)
            .map_err(|e| UpdateError::Install(format!("putting the new app in place: {e}")))
    });
    if let Err(e) = swapped {
        let _ = fs::remove_dir_all(&staged);
        return Err(e);
    }
    // The old app is where the new one was staged.
    if let Err(e) = remove(&staged) {
        log::warn!("removing the old app at {}: {e}", staged.display());
    }
    Ok(running.app.to_owned())
}

/// The app at `app` is a folder, not a link out of the image; validly
/// signed, all of it, strictly; meets the running app's designated
/// requirement; and is `version`, newer than the running one.
fn check(app: &Path, version: &Version, running: &Running) -> Result<(), UpdateError> {
    let refuse = |why: String| Err(UpdateError::Install(why));
    if !fs::symlink_metadata(app).is_ok_and(|meta| meta.is_dir()) {
        return refuse("the disk image has no earwig.app".into());
    }
    let signed = match mac::check_app(app, &running.requirement) {
        Ok(signed) => signed,
        Err(e) => return refuse(format!("the new app is not signed like this one: {e}")),
    };
    match signed.as_deref().map(Version::parse) {
        Some(Ok(found)) if found <= *running.version => refuse(format!(
            "the new app is earwig {found}, not newer than this one"
        )),
        Some(Ok(found)) if found != *version => {
            refuse(format!("the new app is earwig {found}, not {version}"))
        }
        Some(Ok(_)) => Ok(()),
        _ => refuse("the new app has no version".into()),
    }
}

/// A disk image attached read-only and hidden from the Finder, detached
/// when dropped.
struct Image(PathBuf);

impl Image {
    fn attach(dmg: &Path, mount: &Path) -> Result<Self, UpdateError> {
        run(Command::new("/usr/bin/hdiutil")
            .args([
                "attach",
                "-nobrowse",
                "-readonly",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(mount)
            .arg(dmg))
        .map_err(|e| UpdateError::Install(format!("attaching the disk image: {e}")))?;
        Ok(Self(mount.to_owned()))
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        detach(&self.0);
    }
}

/// Detach the image at `mount`, by force if it is busy; whether it went.
fn detach(mount: &Path) -> bool {
    for force in [false, true] {
        let mut hdiutil = Command::new("/usr/bin/hdiutil");
        hdiutil.arg("detach");
        if force {
            hdiutil.arg("-force");
        }
        match run(hdiutil.arg(mount)) {
            Ok(()) => return true,
            Err(e) => log::warn!("detaching {}: {e}", mount.display()),
        }
    }
    false
}

/// Run a system tool to the end; its complaint if it fails.
fn run(command: &mut Command) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} ({})",
            String::from_utf8_lossy(&output.stderr).trim(),
            output.status
        ))
    }
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Clear what an update cut short left: an app staged or moved aside next
/// to `app` (put back if it is the only copy), an image still attached in
/// `work`. False while that image can't be detached, so `work` must stay.
pub(super) fn clean(work: &Path, app: &Path) -> bool {
    settle(app);
    let mount = work.join("mount");
    !mounted(&mount) || detach(&mount)
}

/// Whether a volume is mounted on `dir`.
pub(super) fn mounted(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (fs::metadata(dir), dir.parent().map(fs::metadata)) {
        (Ok(dir), Some(Ok(parent))) => dir.dev() != parent.dev(),
        _ => false,
    }
}
