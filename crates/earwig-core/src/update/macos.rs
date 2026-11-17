//! macOS: earwig.app replaces itself with the one in the release's disk
//! image, once that is signed so that it meets the running app's designated
//! requirement (same identifier, same Developer ID team) and is newer.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use semver::Version;

use super::bundle::{developer_id_requirement, mac_mode, settle, staged, swap};
use super::{Mode, UpdateError};
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
    fs::create_dir_all(&mount)
        .map_err(|e| UpdateError::io(e, format!("creating {}", mount.display())))?;
    let image = Image::attach(dmg, &mount)?;
    let app = mount.join("earwig.app");
    check(&app, version, running)?;
    let staged = staged(running.app);
    remove(&staged).map_err(|e| UpdateError::io(e, format!("removing {}", staged.display())))?;
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

/// The Mac's update end to end, but for the program running it (a test,
/// not an app): apps signed with throwaway identities and the hardened
/// runtime like the release's, in disk images from hdiutil beside a link
/// to Applications. On a Mac:
/// `nix shell nixpkgs#rcodesign -c cargo test -p earwig-core mac_update -- --ignored`
#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use sha2::{Digest as _, Sha256};

    use super::super::testing::{Feed, release};
    use super::super::{REPO, asset_name, macos};
    use super::*;
    use crate::macos::check_app;

    #[test]
    #[ignore = "signs apps with rcodesign and makes disk images with hdiutil"]
    fn mac_update_installs_only_a_newer_app_signed_like_the_running_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let ours = identity(root, "ours");
        let theirs = identity(root, "theirs");
        let installed = root.join("Applications/earwig.app");
        app(&installed, "0.1.0", &ours, |_| {});
        let requirement = Requirement::of(&installed).unwrap();
        let was = Version::parse("0.1.0").unwrap();
        let next = Version::parse("0.1.1").unwrap();
        let running = Running::on_disk(&installed, &was);
        let work = root.join("work");
        let intact = |version: &str| {
            assert_eq!(
                check_app(&installed, &requirement),
                Ok(Some(version.to_owned()))
            );
            let apps: Vec<_> = fs::read_dir(root.join("Applications"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(apps, ["earwig.app"]);
            assert!(
                !macos::mounted(&work.join("mount")),
                "the image is still attached"
            );
        };

        for (name, version, identity, why) in [
            // The same identifier, another identity.
            ("theirs", "0.1.1", &theirs, "not signed like this one"),
            ("older", "0.0.9", &ours, "not newer than this one"),
            ("other", "0.1.2", &ours, "not 0.1.1"),
        ] {
            let image = image(root, name, version, identity, |_| {});
            let err = macos::install(&image, &next, &running, &work).unwrap_err();
            assert!(err.to_string().contains(why), "{name}: {err}");
            intact("0.1.0");
        }
        let altered = image(root, "altered", "0.1.1", &ours, |app| {
            fs::write(app.join("Contents/MacOS/extra"), "after signing").unwrap();
        });
        let err = macos::install(&altered, &next, &running, &work).unwrap_err();
        assert!(
            err.to_string().contains("not signed like this one"),
            "{err}"
        );
        intact("0.1.0");

        // Through the feed: changed on the way it fails its digest; as
        // released it goes in, and the old app is gone.
        let good = fs::read(image(root, "good", "0.1.1", &ours, |_| {})).unwrap();
        let sha256 = hex::encode(Sha256::digest(&good));
        let mut tampered = good.clone();
        let middle = tampered.len() / 2;
        tampered[middle] ^= 1;
        for (served, as_released) in [(tampered, false), (good, true)] {
            let size = served.len();
            let feed = Feed::start(|url| {
                vec![
                    (
                        format!("/repos/{REPO}/releases/latest"),
                        release(url, "0.1.1", &sha256, size)
                            .to_string()
                            .into_bytes(),
                    ),
                    (
                        format!("/{REPO}/releases/download/v0.1.1/{}", asset_name(&next)),
                        served,
                    ),
                ]
            });
            let updates = feed.updates("0.1.0", root);
            let offer = updates.check().unwrap().unwrap();
            let lock = updates.lock().unwrap().unwrap();
            let download = updates.download(&lock, &offer, &mut |_, _| {});
            if !as_released {
                assert!(
                    matches!(download, Err(UpdateError::Checksum)),
                    "{download:?}"
                );
                intact("0.1.0");
                continue;
            }
            let app = macos::install(&download.unwrap(), &offer.version, &running, &work).unwrap();
            assert_eq!(app, installed);
            intact("0.1.1");
        }
    }

    /// A throwaway signing identity: its certificate and key.
    fn identity(root: &Path, name: &str) -> [PathBuf; 2] {
        let base = root.join(name);
        run(Command::new("rcodesign")
            .args(["generate-self-signed-certificate", "--person-name"])
            .arg(format!("earwig test {name}"))
            .arg("--pem-filename")
            .arg(&base));
        [base.with_extension("crt"), base.with_extension("key")]
    }

    /// earwig.app `version` at `path`, signed by `identity`, then `alter`ed.
    fn app(path: &Path, version: &str, identity: &[PathBuf; 2], alter: impl FnOnce(&Path)) {
        let exe = path.join("Contents/MacOS/earwig");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        // Any Mach-O does; this one is small.
        fs::copy("/usr/bin/true", &exe).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
                path.join("Contents/Info.plist"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.github.adeci.earwig</string>
<key>CFBundleExecutable</key><string>earwig</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{version}</string>
</dict></plist>
"#
                ),
            )
            .unwrap();
        run(Command::new("rcodesign")
            .arg("sign")
            .arg("--pem-file")
            .arg(&identity[0])
            .arg("--pem-file")
            .arg(&identity[1])
            .args(["--code-signature-flags", "runtime"])
            .arg(path));
        alter(path);
    }

    /// A disk image of `app(version, identity, alter)` beside a link to
    /// Applications, as releases are.
    fn image(
        root: &Path,
        name: &str,
        version: &str,
        identity: &[PathBuf; 2],
        alter: impl FnOnce(&Path),
    ) -> PathBuf {
        let folder = root.join(name);
        app(&folder.join("earwig.app"), version, identity, alter);
        std::os::unix::fs::symlink("/Applications", folder.join("Applications")).unwrap();
        let dmg = root.join(format!("{name}.dmg"));
        run(Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-volname", "earwig", "-fs", "HFS+"])
            .args(["-format", "UDZO", "-srcfolder"])
            .arg(&folder)
            .arg(&dmg));
        dmg
    }

    fn run(command: &mut Command) {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
