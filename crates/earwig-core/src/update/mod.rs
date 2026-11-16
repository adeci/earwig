//! Updates for the Windows and macOS releases. The daemon asks GitHub once a
//! day for earwig's newest release; on request it downloads this platform's
//! file of it, checks it against the SHA-256 GitHub lists, and installs it
//! once it is signed like the running earwig: the installer on Windows (any
//! installer, when this earwig isn't signed), the app on macOS. Builds from
//! Nix, cargo or a package manager never check.

#[cfg(target_os = "macos")]
mod macos;
mod release;
mod service;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(windows)]
mod windows;

use std::fmt::Display;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use semver::Version;
use serde::de::DeserializeOwned;
use ureq::Agent;

use crate::models::download::{self, Source};
use crate::models::{CancelToken, ModelError};
#[cfg(target_os = "macos")]
pub use macos::relaunch;
pub use release::Offer;
use release::{Pick, Release};
pub(crate) use service::{Reply, Request, spawn};

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why a build that never updates itself won't.
pub const NOT_UPDATING: &str = "this build of earwig does not update itself: \
    whatever installed it (Nix, cargo, a package manager) does";

const REPO: &str = "adeci/earwig";

/// Set only when a test build is compiled: a server standing in for both of
/// GitHub's hosts, plain HTTP allowed. Every other build asks GitHub, over
/// HTTPS only.
const TEST_FEED: Option<&str> = option_env!("EARWIG_UPDATE_TEST_FEED");

/// Bounds GitHub's answer; a page of releases is far smaller.
const MAX_ANSWER: u64 = 8 << 20;

/// A download still going after this long is given up: a server dripping
/// bytes would hold off every later check.
const DOWNLOAD_LIMIT: Duration = Duration::from_mins(30);

/// The release page of `version`, which has all its files.
pub fn release_page(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
}

/// This platform's file of `version`, as the release workflows name it.
fn asset_name(version: &Version) -> String {
    if cfg!(windows) {
        format!("earwig-{version}-windows-x64-setup.exe")
    } else if cfg!(target_os = "macos") {
        format!("earwig-{version}-macos-arm64.dmg")
    } else {
        // Only test builds look on Linux, and never install.
        format!("earwig-{version}-linux-x64.tar.gz")
    }
}

/// What this copy of earwig does about a newer release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Say it is out; its release page has it.
    #[cfg_attr(target_os = "linux", allow(dead_code))] // Linux only tests
    Notify,
    /// Download and install it.
    Install,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// Asking GitHub failed, or its answer made no sense.
    #[error("{0}")]
    Check(String),
    #[error("{0}")]
    Download(String),
    #[error("the download does not match the SHA-256 GitHub lists for it")]
    Checksum,
    #[error("{0}")]
    Install(String),
    /// Another earwig (the daemon, or `earwig update` on its own) is
    /// updating.
    #[error("another earwig is installing an update")]
    Busy,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Held for a whole download and install, so that two earwigs (the daemon
/// and `earwig update` run on its own) never clear each other's.
pub struct UpdateLock {
    /// Locked while open.
    _file: fs::File,
}

/// What [`Updates::install`] leaves to its caller.
#[derive(Debug)]
pub enum Installed {
    /// macOS: the new app is where the running one was; start it once this
    /// one quit.
    Relaunch(PathBuf),
    /// Windows: the installer runs; it stops earwig, replaces it and starts
    /// it again.
    Installer(std::process::Child),
}

/// Where this copy of earwig finds newer releases, and what it may do with
/// them.
#[derive(Clone, Debug)]
pub struct Updates {
    mode: Mode,
    running: Version,
    /// `<api>/repos/adeci/earwig/releases`
    releases: String,
    /// `<github>/adeci/earwig/releases/download`, where release files are.
    downloads: String,
    /// The private folder downloads go to.
    dir: PathBuf,
    api: Agent,
    files: Agent,
    /// macOS: the running earwig.app.
    #[cfg(target_os = "macos")]
    app: PathBuf,
}

impl Updates {
    /// How this copy updates, or `None` when it never does: on Linux, in
    /// Windows builds other than the release package, in macOS builds
    /// without a Developer ID (Nix, cargo), and without a home directory.
    pub fn detect() -> Option<Self> {
        let (api, site) = TEST_FEED
            .map_or(("https://api.github.com", "https://github.com"), |feed| {
                (feed, feed)
            });
        let running = Version::parse(VERSION).expect("cargo versions are SemVer");
        #[cfg(target_os = "macos")]
        let (app, mode) = macos::detect()?;
        #[cfg(windows)]
        let mode = windows::detect()?;
        #[cfg(target_os = "linux")]
        let mode = TEST_FEED.map(|_| Mode::Install)?;
        let updates = Self::new(
            mode,
            running,
            api,
            site,
            crate::paths::update_dir()?,
            TEST_FEED.is_none(),
            #[cfg(target_os = "macos")]
            app,
        );
        log::info!(
            "updates from {}: {}",
            updates.releases,
            match mode {
                Mode::Install => "installed when asked",
                Mode::Notify => "only announced, this copy can't replace itself",
            }
        );
        Some(updates)
    }

    fn new(
        mode: Mode,
        running: Version,
        api: &str,
        site: &str,
        dir: PathBuf,
        https_only: bool,
        #[cfg(target_os = "macos")] app: PathBuf,
    ) -> Self {
        let user_agent = concat!("earwig/", env!("CARGO_PKG_VERSION"));
        let api_agent = Agent::config_builder()
            .user_agent(user_agent)
            .https_only(https_only)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        let files = Agent::config_builder()
            .user_agent(user_agent)
            .https_only(https_only)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .build()
            .new_agent();
        Self {
            mode,
            running,
            releases: format!("{api}/repos/{REPO}/releases"),
            downloads: format!("{site}/{REPO}/releases/download"),
            dir,
            api: api_agent,
            files,
            #[cfg(target_os = "macos")]
            app,
        }
    }

    /// Whether this copy installs updates, or only says they are out.
    pub fn installable(&self) -> bool {
        self.mode == Mode::Install
    }

    /// A newer release with a file for this platform, if GitHub has one.
    pub fn check(&self) -> Result<Option<Offer>, UpdateError> {
        let releases = if self.running.pre.is_empty() {
            self.get(&format!("{}/latest", self.releases), &[])?
                .into_iter()
                .collect()
        } else {
            self.get::<Vec<Release>>(&self.releases, &[("per_page", "30")])?
                .unwrap_or_default()
        };
        Ok(
            match release::pick(&self.running, &releases, &self.downloads, asset_name) {
                Pick::UpToDate => None,
                Pick::Offer(offer) => Some(offer),
                Pick::Unusable { version, why } => {
                    log::info!("earwig {version} is out, but not for this copy yet: {why}");
                    None
                }
            },
        )
    }

    /// GitHub's JSON at `url`; `None` for a 404 (no release yet).
    fn get<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<Option<T>, UpdateError> {
        let failed = |e: &dyn Display| UpdateError::Check(format!("asking GitHub ({url}): {e}"));
        let mut response = self
            .api
            .get(url)
            .query_pairs(query.iter().copied())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .map_err(|e| failed(&e))?;
        match response.status().as_u16() {
            200 => {}
            404 => return Ok(None),
            status => return Err(failed(&format!("HTTP {status}"))),
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER)
            .read_json()
            .map(Some)
            .map_err(|e| failed(&e))
    }

    /// The lock a download and install holds from start to end; `None` while
    /// another earwig holds it.
    pub fn lock(&self) -> Result<Option<UpdateLock>, UpdateError> {
        let path = self.dir.with_file_name("update.lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(UpdateLock { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// Download `offer` into the private folder, no more bytes than GitHub
    /// lists, within [`DOWNLOAD_LIMIT`], and checked against its SHA-256 and,
    /// on Windows, for this earwig's publisher's signature; nothing is left of
    /// a download that fails. `progress(downloaded, total)`.
    pub fn download(
        &self,
        lock: &UpdateLock,
        offer: &Offer,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<PathBuf, UpdateError> {
        self.clear(lock);
        create_private(&self.dir)?;
        let file = self.dir.join(asset_name(&offer.version));
        let source = Source {
            url: &offer.url,
            sha256: &offer.sha256,
            size: Some(offer.size),
        };
        // Cancels the download once the time is up, unless it ended first.
        let cancel = CancelToken::default();
        let (ended, end) = flume::bounded::<()>(0);
        let deadline = cancel.clone();
        thread::Builder::new()
            .name("earwig-update-deadline".into())
            .spawn(move || {
                if end.recv_timeout(DOWNLOAD_LIMIT) == Err(flume::RecvTimeoutError::Timeout) {
                    deadline.cancel();
                }
            })?;
        let fetched = download::fetch(&self.files, &source, &file, progress, &cancel);
        drop(ended);
        fetched.map_err(|e| {
            self.clear(lock);
            match e {
                ModelError::Checksum { .. } => UpdateError::Checksum,
                ModelError::Cancelled => UpdateError::Download(format!(
                    "the download took longer than {} minutes",
                    DOWNLOAD_LIMIT.as_secs() / 60
                )),
                ModelError::Http(message) => {
                    UpdateError::Download(format!("downloading {message}"))
                }
                ModelError::Io(e) => UpdateError::Io(e),
                other => UpdateError::Download(other.to_string()),
            }
        })?;
        #[cfg(windows)]
        if let Err(e) = windows::check_publisher(&file) {
            self.clear(lock);
            return Err(e);
        }
        Ok(file)
    }

    /// Install `file`, the checked download of `offer`.
    pub fn install(
        &self,
        _lock: &UpdateLock,
        offer: &Offer,
        file: &Path,
    ) -> Result<Installed, UpdateError> {
        if self.mode != Mode::Install {
            return Err(UpdateError::Install(
                "this copy of earwig can't replace itself".into(),
            ));
        }
        #[cfg(windows)]
        {
            let _ = offer;
            // The installer's log outlives the download, for when it fails.
            windows::launch(file, &self.dir.with_file_name("install.log"))
                .map(Installed::Installer)
                .map_err(|e| UpdateError::Install(format!("starting the installer: {e}")))
        }
        #[cfg(target_os = "macos")]
        {
            let running = macos::Running::this(&self.app, &self.running)?;
            macos::install(file, &offer.version, &running, &self.dir).map(Installed::Relaunch)
        }
        #[cfg(target_os = "linux")]
        {
            let _ = (offer, file);
            Err(UpdateError::Install(
                "earwig installs no updates on Linux".into(),
            ))
        }
    }

    /// Delete what an earlier update left behind, unless another earwig is
    /// updating right now.
    pub fn clean(&self) {
        match self.lock() {
            Ok(Some(lock)) => self.clear(&lock),
            Ok(None) => log::info!("another earwig is updating; its files stay"),
            Err(e) => log::warn!("cleaning up after updates: {e}"),
        }
    }

    /// Delete the downloads and, on macOS, a disk image still attached or an
    /// app still staged (putting back the old app if it is all there is).
    pub fn clear(&self, _lock: &UpdateLock) {
        #[cfg(target_os = "macos")]
        if !macos::clean(&self.dir, &self.app) {
            return;
        }
        match fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                log::warn!("removing {}: {e}", self.dir.display());
            }
            _ => {}
        }
    }
}

/// `dir` and its parents, only the user may look in it; refused if an
/// existing `dir` belongs to someone else or others may look in.
fn create_private(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = fs::metadata(dir)?;
        if meta.uid() != rustix::process::getuid().as_raw() || meta.mode() & 0o077 != 0 {
            return Err(io::Error::other(format!(
                "{} is not private to this user",
                dir.display()
            )));
        }
    }
    Ok(())
}

/// The app bundle `exe` runs from, `<app>/Contents/MacOS/<exe>`, and what it
/// may do: only a bundle signed with a Developer ID (a team) updates, and
/// only where it can replace itself (not translocated, its folder and
/// itself writable); one in the Nix store never does.
#[cfg(any(target_os = "macos", test))]
fn mac_mode(
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
#[cfg(any(target_os = "macos", all(test, unix)))]
fn swap(installed: &Path, staged: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    match renameat_with(CWD, staged, CWD, installed, RenameFlags::EXCHANGE) {
        Ok(()) => Ok(()),
        Err(e) if e == rustix::io::Errno::INVAL || e == rustix::io::Errno::NOTSUP => {
            swap_by_renames(installed, staged)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
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
#[cfg(any(target_os = "macos", all(test, unix)))]
fn aside(app: &Path) -> PathBuf {
    app.with_file_name(".earwig-old.app")
}

/// Where the new app is copied, next to the installed `app` so the swap
/// stays on one volume.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn staged(app: &Path) -> PathBuf {
    app.with_file_name(".earwig-update.app")
}

/// Tidy what a swap cut short left next to `app`: an app moved aside is put
/// back when it is all there is; the copies go only once `app` is there.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn settle(app: &Path) {
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
#[cfg(any(target_os = "macos", test))]
fn developer_id_requirement(requirement: &str, team: &str) -> bool {
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

/// Who signed a file, by an Authenticode signature Windows accepts.
#[cfg(any(windows, test))]
struct Publisher {
    /// The signing certificate's name.
    name: String,
    /// The signing certificate's extended key usages.
    ekus: Vec<String>,
    /// The SHA-256 of the root certificate its chain ends at, and its name.
    root: [u8; 32],
    root_name: String,
}

#[cfg(any(windows, test))]
impl Publisher {
    /// Its Artifact Signing Public Trust certificate profiles: the certificates
    /// of a profile, a new one every day, carry its own EKU right under
    /// 1.3.6.1.4.1.311.97, whose .1 holds the kinds of profile (.1.0 is Public
    /// Trust) and the other kinds' EKUs.
    fn profiles(&self) -> impl Iterator<Item = &str> {
        self.ekus.iter().map(String::as_str).filter(|eku| {
            eku.strip_prefix("1.3.6.1.4.1.311.97.")
                .is_some_and(|rest| !rest.is_empty() && rest.split('.').next() != Some("1"))
        })
    }

    fn root(&self) -> String {
        format!("{} ({})", self.root_name, hex::encode(&self.root[..8]))
    }
}

/// Why `new` isn't signed by the publisher of `running`, if it isn't: it must
/// chain to the same root and carry each of `running`'s certificate profiles,
/// which only the profile's own certificates from that root's authority do.
/// A subject is just a name that anyone can give their own certificate.
#[cfg(any(windows, test))]
fn same_publisher(running: &Publisher, new: &Publisher) -> Result<(), String> {
    let ours: Vec<&str> = running.profiles().collect();
    if ours.is_empty() {
        return Err(format!(
            "this earwig's signing certificate ({}) names no Artifact Signing certificate \
             profile, so it can't tell its publisher's installers from others",
            running.name
        ));
    }
    if new.root != running.root {
        return Err(format!(
            "the installer's signature ({}) chains to another root certificate, {}, not {}",
            new.name,
            new.root(),
            running.root()
        ));
    }
    let theirs: Vec<&str> = new.profiles().collect();
    match ours.iter().find(|profile| !theirs.contains(profile)) {
        Some(profile) => Err(format!(
            "the installer is signed by {} under another certificate profile ({}), \
             not this earwig's {profile}",
            new.name,
            if theirs.is_empty() {
                "none".to_owned()
            } else {
                theirs.join(", ")
            }
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Feed, data, releases};
    use super::*;

    #[test]
    fn a_stable_build_asks_for_the_latest_release_only() {
        let (data, sha256) = data();
        let feed = releases(data, sha256.clone(), 300_000);
        let dir = tempfile::tempdir().unwrap();
        let offer = feed.updates("0.1.0", dir.path()).check().unwrap().unwrap();
        assert_eq!(offer.version, Version::parse("0.2.0").unwrap());
        assert_eq!(offer.sha256, sha256);
        assert_eq!(
            *feed.asked.lock().unwrap(),
            [format!("/repos/{REPO}/releases/latest")]
        );
    }

    #[test]
    fn a_pre_release_build_asks_for_every_release() {
        let (data, sha256) = data();
        let feed = releases(data, sha256, 300_000);
        let dir = tempfile::tempdir().unwrap();
        let offer = feed
            .updates("0.2.0-rc.2", dir.path())
            .check()
            .unwrap()
            .unwrap();
        assert_eq!(offer.version, Version::parse("0.3.0-rc.1").unwrap());
        assert_eq!(
            *feed.asked.lock().unwrap(),
            [format!("/repos/{REPO}/releases?per_page=30")]
        );
    }

    #[test]
    fn no_release_yet_is_no_update() {
        let feed = Feed::start(|_| Vec::new());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(feed.updates("0.1.0", dir.path()).check().unwrap(), None);
    }

    #[test]
    fn a_download_matching_its_digest_lands_in_a_private_folder() {
        let (data, sha256) = data();
        let feed = releases(data.clone(), sha256, 300_000);
        let dir = tempfile::tempdir().unwrap();
        let updates = feed.updates("0.1.0", dir.path());
        let offer = updates.check().unwrap().unwrap();
        let lock = updates.lock().unwrap().unwrap();
        let mut last = (0, 0);
        let file = updates
            .download(&lock, &offer, &mut |done, total| last = (done, total))
            .unwrap();
        assert_eq!(fs::read(&file).unwrap(), data);
        assert_eq!(file.parent(), Some(dir.path().join("update").as_path()));
        assert_eq!(last, (300_000, 300_000));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(dir.path().join("update"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // Another earwig cleaning up meanwhile leaves it alone.
        assert!(updates.lock().unwrap().is_none(), "the lock is held");
        updates.clean();
        assert!(file.exists());
        drop(lock);
        // A child another test forks meanwhile may hold the lock a moment longer, until it execs.
        let update = dir.path().join("update");
        for _ in 0..100 {
            updates.clean();
            if !update.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!update.exists());
    }

    #[test]
    fn a_download_not_matching_its_digest_leaves_nothing() {
        let (data, _) = data();
        let feed = releases(data, "ab".repeat(32), 300_000);
        let dir = tempfile::tempdir().unwrap();
        let updates = feed.updates("0.1.0", dir.path());
        let offer = updates.check().unwrap().unwrap();
        let lock = updates.lock().unwrap().unwrap();
        let err = updates.download(&lock, &offer, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, UpdateError::Checksum), "{err}");
        assert!(!dir.path().join("update").exists());
    }

    #[test]
    fn a_download_longer_than_listed_is_refused() {
        let (data, sha256) = data();
        let feed = releases(data, sha256, 200_000);
        let dir = tempfile::tempdir().unwrap();
        let updates = feed.updates("0.1.0", dir.path());
        let offer = updates.check().unwrap().unwrap();
        let lock = updates.lock().unwrap().unwrap();
        let err = updates.download(&lock, &offer, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, UpdateError::Download(_)), "{err}");
        assert!(!dir.path().join("update").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_download_folder_others_may_look_in_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("update");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        let err = create_private(&shared).unwrap_err();
        assert!(err.to_string().contains("not private"), "{err}");
    }

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

    // The EKUs of a real Artifact Signing certificate (python.org's installer,
    // October 2026), and of one with Microsoft's example of another profile.
    const CODE_SIGNING: &str = "1.3.6.1.5.5.7.3.3";
    const PUBLIC_TRUST: &str = "1.3.6.1.4.1.311.97.1.0";
    const PROFILE: &str = "1.3.6.1.4.1.311.97.608394634.79987812.305991749.578777327";
    const OURS: &[&str] = &[PUBLIC_TRUST, CODE_SIGNING, PROFILE];
    const THEIRS: &[&str] = &[
        PUBLIC_TRUST,
        CODE_SIGNING,
        "1.3.6.1.4.1.311.97.990309390.766961637.194916062.941502583",
    ];

    fn publisher(name: &str, ekus: &[&str], root: u8) -> Publisher {
        Publisher {
            name: name.to_owned(),
            ekus: ekus.iter().map(|&eku| eku.to_owned()).collect(),
            root: [root; 32],
            root_name: format!("root {root}"),
        }
    }

    #[test]
    fn an_installer_from_the_same_certificate_profile_and_root_is_the_publishers() {
        let running = publisher("Alexander Decious", OURS, 1);
        // Another day's certificate: a new key and thumbprint, the profile's EKU.
        let next = publisher("Alexander Decious", &[CODE_SIGNING, PROFILE], 1);
        assert_eq!(same_publisher(&running, &next), Ok(()));
        // A name changed with a renewed identity validation, a marker added.
        let marked = [OURS, &["1.3.6.1.4.1.311.97.1.9"]].concat();
        let later = publisher("Alexander J. Decious", &marked, 1);
        assert_eq!(same_publisher(&running, &later), Ok(()));
    }

    #[test]
    fn an_installer_from_anyone_else_is_refused() {
        let running = publisher("Alexander Decious", OURS, 1);
        // Another certificate profile from the same authority: another
        // subscriber, or this one's profile deleted and made anew.
        let other = publisher("Someone Else", THEIRS, 1);
        let err = same_publisher(&running, &other).unwrap_err();
        assert!(err.contains("another certificate profile"), "{err}");
        // The same name and EKUs under another root: a look-alike, self-signed
        // or from another authority.
        let look_alike = publisher("Alexander Decious", OURS, 2);
        let err = same_publisher(&running, &look_alike).unwrap_err();
        assert!(err.contains("another root certificate"), "{err}");
        let plain = publisher("Alexander Decious", &[CODE_SIGNING], 1);
        let err = same_publisher(&running, &plain).unwrap_err();
        assert!(err.contains("another certificate profile (none)"), "{err}");
    }

    #[test]
    fn only_a_public_trust_profile_eku_names_a_publisher() {
        let installer = publisher("Alexander Decious", OURS, 1);
        for ekus in [
            &[CODE_SIGNING][..],
            &[CODE_SIGNING, PUBLIC_TRUST],
            &[CODE_SIGNING, "1.3.6.1.4.1.311.97.1.3.1.29433.35007"],
            &[CODE_SIGNING, "1.3.6.1.4.1.311.970.608394634"],
            &[CODE_SIGNING, "1.3.6.1.4.1.311.97."],
        ] {
            let running = publisher("Alexander Decious", ekus, 1);
            let err = same_publisher(&running, &installer).unwrap_err();
            assert!(
                err.contains("names no Artifact Signing certificate profile"),
                "{ekus:?}: {err}"
            );
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

    /// The Mac's update end to end, but for the program running it (a test,
    /// not an app): apps signed with throwaway identities and the hardened
    /// runtime like the release's, in disk images from hdiutil beside a link
    /// to Applications. On a Mac:
    /// `nix shell nixpkgs#rcodesign -c cargo test -p earwig-core mac_update -- --ignored`
    #[cfg(target_os = "macos")]
    mod mac {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;

        use sha2::{Digest as _, Sha256};

        use super::super::macos::{self, Running};
        use super::super::testing::release;
        use super::*;
        use crate::macos::{Requirement, check_app};

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
                let app =
                    macos::install(&download.unwrap(), &offer.version, &running, &work).unwrap();
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
}
