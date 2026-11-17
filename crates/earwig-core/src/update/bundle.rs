//! earwig.app's side of an update, apart from the macOS calls so that every
//! OS tests it: whether a copy may replace itself, what its signature must
//! designate, and how a new app takes the old one's place.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{fs, io};

use super::Mode;

/// The app bundle `exe` runs from, `<app>/Contents/MacOS/<exe>`, and what it
/// may do: only a bundle signed with a Developer ID (a team) updates, and
/// only where it can replace itself (not translocated, its folder and
/// itself writable); one in the Nix store never does.
pub(super) fn mac_mode(
    exe: &Path,
    team: Option<&str>,
    writable: impl Fn(&Path) -> bool,
) -> Option<(PathBuf, Mode)> {
    team.filter(|team| !team.is_empty())?;
    let contents = exe
        .parent()
        .filter(|dir| dir.ends_with("MacOS"))?
        .parent()?;
    let app = contents
        .parent()
        .filter(|_| contents.ends_with("Contents"))?;
    if app.extension().is_none_or(|ext| ext != "app") || app.starts_with("/nix/store") {
        return None;
    }
    // Gatekeeper runs a quarantined app it hasn't seen moved from a
    // read-only copy at a random path.
    let translocated = app
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation");
    let replaceable = !translocated && app.parent().is_some_and(&writable) && writable(app);
    let mode = if replaceable {
        Mode::Install
    } else {
        Mode::Notify
    };
    Some((app.to_owned(), mode))
}

/// Puts `staged` where `installed` is in one step, rolled back if that
/// fails; on success the old app is at `staged`. Where the file system
/// can't swap two names at once (HFS+, exFAT): moved aside, the new one in,
/// the old one back if that failed.
#[cfg(unix)]
pub(super) fn swap(installed: &Path, staged: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    match renameat_with(CWD, staged, CWD, installed, RenameFlags::EXCHANGE) {
        Ok(()) => Ok(()),
        Err(e) if e == rustix::io::Errno::INVAL || e == rustix::io::Errno::NOTSUP => {
            swap_by_renames(installed, staged)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
fn swap_by_renames(installed: &Path, staged: &Path) -> io::Result<()> {
    let aside = aside(installed);
    fs::rename(installed, &aside)?;
    if let Err(e) = fs::rename(staged, installed) {
        if let Err(back) = fs::rename(&aside, installed) {
            log::error!(
                "earwig is at {} now: moving it back to {} failed: {back}",
                aside.display(),
                installed.display()
            );
        }
        return Err(e);
    }
    // The new app is in: the old one only has to go somewhere.
    if let Err(e) = fs::rename(&aside, staged) {
        log::warn!("moving the old app to {}: {e}", staged.display());
    }
    Ok(())
}

/// Where the swap without an atomic rename moves the old app meanwhile.
#[cfg(unix)]
fn aside(app: &Path) -> PathBuf {
    app.with_file_name(".earwig-old.app")
}

/// Where the new app is copied, next to the installed `app` so the swap
/// stays on one volume.
#[cfg(unix)]
pub(super) fn staged(app: &Path) -> PathBuf {
    app.with_file_name(".earwig-update.app")
}

/// Tidy what a swap cut short left next to `app`: an app moved aside is put
/// back when it is all there is; the copies go only once `app` is there.
#[cfg(unix)]
pub(super) fn settle(app: &Path) {
    let aside = aside(app);
    if !app.exists() && aside.exists() {
        match fs::rename(&aside, app) {
            Ok(()) => log::warn!(
                "put earwig back at {} from {}",
                app.display(),
                aside.display()
            ),
            Err(e) => {
                log::error!(
                    "earwig is at {}: moving it back failed: {e}",
                    aside.display()
                );
                return;
            }
        }
    }
    if !app.exists() {
        return;
    }
    for leftover in [staged(app), aside] {
        match fs::remove_dir_all(&leftover) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                log::warn!("removing {}: {e}", leftover.display());
            }
            _ => {}
        }
    }
}

/// Whether `requirement` is what a Developer ID of `team` designates:
/// Apple's anchor, and that team (quoted or not) as the leaf's OU.
pub(super) fn developer_id_requirement(requirement: &str, team: &str) -> bool {
    const OU: &str = "certificate leaf[subject.OU] = ";
    !team.is_empty()
        && requirement.contains("anchor apple generic")
        && requirement.match_indices(OU).any(|(at, _)| {
            let value = &requirement[at + OU.len()..];
            let value = value.strip_prefix('"').unwrap_or(value);
            value
                .strip_prefix(team)
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_developer_id_requirement_is_trusted() {
        let quoted = r#"identifier "io.github.adeci.earwig" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = "ABCDE12345""#;
        assert!(developer_id_requirement(quoted, "ABCDE12345"));
        let bare = quoted.replace(r#""ABCDE12345""#, "ABCDE12345");
        assert!(developer_id_requirement(&bare, "ABCDE12345"));
        assert!(!developer_id_requirement(quoted, "OTHERTEAM1"));
        assert!(
            !developer_id_requirement(quoted, "ABCDE"),
            "a prefix of the team"
        );
        assert!(!developer_id_requirement(quoted, ""));
        for odd in [
            r#"cdhash H"0123456789abcdef0123456789abcdef01234567""#,
            r#"identifier "io.github.adeci.earwig" and certificate root = H"4c96ea84b4946b554e63ad9daa79ed4749474980""#,
            r#"identifier "io.github.adeci.earwig" and certificate leaf[subject.OU] = "ABCDE12345""#,
        ] {
            assert!(!developer_id_requirement(odd, "ABCDE12345"), "{odd}");
        }
    }

    fn writable_except<'a>(read_only: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |path| !read_only.iter().any(|ro| path == Path::new(ro))
    }

    const TEAM: Option<&str> = Some("ABCDE12345");

    #[test]
    fn a_mac_app_updates_where_it_may_replace_itself() {
        let exe = Path::new("/Applications/earwig.app/Contents/MacOS/earwig");
        let app = PathBuf::from("/Applications/earwig.app");
        assert_eq!(
            mac_mode(exe, TEAM, writable_except(&[])),
            Some((app.clone(), Mode::Install))
        );
        // A standard user can't write to /Applications, nor into an admin's app.
        assert_eq!(
            mac_mode(exe, TEAM, writable_except(&["/Applications"])),
            Some((app.clone(), Mode::Notify))
        );
        assert_eq!(
            mac_mode(exe, TEAM, writable_except(&["/Applications/earwig.app"])),
            Some((app, Mode::Notify))
        );
        let translocated = Path::new(
            "/private/var/folders/xy/T/AppTranslocation/0F1E2D3C/d/earwig.app/Contents/MacOS/earwig",
        );
        assert!(matches!(
            mac_mode(translocated, TEAM, writable_except(&[])),
            Some((_, Mode::Notify))
        ));
        let mounted = Path::new("/Volumes/earwig/earwig.app/Contents/MacOS/earwig");
        assert!(matches!(
            mac_mode(mounted, TEAM, writable_except(&["/Volumes/earwig"])),
            Some((_, Mode::Notify))
        ));
    }

    #[test]
    fn mac_builds_without_a_developer_id_or_a_bundle_never_update() {
        let exe = Path::new("/Applications/earwig.app/Contents/MacOS/earwig");
        // Ad hoc signed: nix-darwin's app, cargo builds.
        assert_eq!(mac_mode(exe, None, writable_except(&[])), None);
        assert_eq!(mac_mode(exe, Some(""), writable_except(&[])), None);
        let store =
            Path::new("/nix/store/abc-earwig/Applications/earwig.app/Contents/MacOS/earwig");
        assert_eq!(mac_mode(store, TEAM, writable_except(&[])), None);
        for bare in [
            "/Users/me/.cargo/bin/earwig",
            "/Applications/earwig/Contents/MacOS/earwig",
            "/Applications/earwig.app/MacOS/earwig",
        ] {
            assert_eq!(
                mac_mode(Path::new(bare), TEAM, writable_except(&[])),
                None,
                "{bare}"
            );
        }
    }

    /// An app folder that says `version`.
    #[cfg(unix)]
    fn app(path: &Path, version: &str) {
        fs::create_dir_all(path.join("Contents")).unwrap();
        fs::write(path.join("Contents/version"), version).unwrap();
    }

    #[cfg(unix)]
    fn version(path: &Path) -> String {
        fs::read_to_string(path.join("Contents/version")).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn the_swap_puts_the_new_app_in_and_the_old_one_where_it_was_staged() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("earwig.app");
        let staged = dir.path().join(".earwig-update.app");
        for swap in [swap, swap_by_renames] {
            app(&installed, "old");
            app(&staged, "new");
            swap(&installed, &staged).unwrap();
            assert_eq!(version(&installed), "new");
            assert_eq!(version(&staged), "old");
            assert!(!aside(&installed).exists());
            fs::remove_dir_all(&installed).unwrap();
            fs::remove_dir_all(&staged).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_swap_leaves_the_installed_app() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("earwig.app");
        app(&installed, "old");
        let missing = dir.path().join(".earwig-update.app");
        for swap in [swap, swap_by_renames] {
            assert!(swap(&installed, &missing).is_err());
            assert_eq!(version(&installed), "old");
            assert!(!aside(&installed).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_app_moved_aside_and_left_alone_is_put_back() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("earwig.app");
        // A swap cut short between its renames: the old app aside, the new
        // one still staged.
        app(&aside(&installed), "old");
        app(&staged(&installed), "new");
        settle(&installed);
        assert_eq!(version(&installed), "old");
        assert!(!aside(&installed).exists());
        assert!(!staged(&installed).exists());
        // With the app in place, leftovers just go.
        app(&aside(&installed), "older");
        app(&staged(&installed), "newer");
        settle(&installed);
        assert_eq!(version(&installed), "old");
        assert!(!aside(&installed).exists());
        assert!(!staged(&installed).exists());
        // No app and nothing aside: a staged copy is all there is, and stays.
        fs::remove_dir_all(&installed).unwrap();
        app(&staged(&installed), "new");
        settle(&installed);
        assert_eq!(version(&staged(&installed)), "new");
    }
}
