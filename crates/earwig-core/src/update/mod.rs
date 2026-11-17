//! Updates for the Windows and macOS releases. The daemon asks GitHub once a
//! day for earwig's newest release; on request it downloads this platform's
//! file of it, checks it against the SHA-256 GitHub lists, and installs it
//! once it is signed like the running earwig: the installer on Windows (any
//! installer, when this earwig isn't signed), the app on macOS. Builds from
//! Nix, cargo or a package manager never check.

#[cfg(any(target_os = "macos", test))]
mod bundle;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod publisher;
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
    #[cfg_attr(target_os = "linux", allow(dead_code))] // on Linux only tests make one
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
    /// Something done to a local file or folder, and how it failed.
    #[error("{doing}: {source}")]
    Io { doing: String, source: io::Error },
}

impl UpdateError {
    fn io(source: io::Error, doing: String) -> Self {
        Self::Io { doing, source }
    }
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
        let (api_agent, files) = download::agents(https_only);
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
            fs::create_dir_all(parent)
                .map_err(|e| UpdateError::io(e, format!("creating {}", parent.display())))?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| UpdateError::io(e, format!("opening {}", path.display())))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(UpdateLock { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => {
                Err(UpdateError::io(e, format!("locking {}", path.display())))
            }
        }
    }

    /// Download `offer` into the private folder, no more bytes than GitHub
    /// lists, within `DOWNLOAD_LIMIT`, and checked against its SHA-256 and,
    /// on Windows, for this earwig's publisher's signature; nothing is left of
    /// a download that fails. `progress(downloaded, total)`.
    pub fn download(
        &self,
        lock: &UpdateLock,
        offer: &Offer,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<PathBuf, UpdateError> {
        self.clear(lock);
        create_private(&self.dir).map_err(|e| {
            UpdateError::io(e, format!("the download folder {}", self.dir.display()))
        })?;
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
            })
            .map_err(|e| UpdateError::io(e, "starting the download".into()))?;
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
                ModelError::Io(e) => {
                    UpdateError::io(e, format!("downloading to {}", file.display()))
                }
                other => UpdateError::Download(other.to_string()),
            }
        })?;
        #[cfg(windows)]
        if let Err(e) = windows::check_publisher(&file, &release_page(&offer.version.to_string())) {
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
    pub(crate) fn clean(&self) {
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
            return Err(io::Error::other("not private to this user"));
        }
    }
    Ok(())
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
}
