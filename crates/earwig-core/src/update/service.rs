//! The daemon's updater thread: checks on a schedule while `[updates] check`
//! is on, keeps the [`UpdateState`] in the status, and installs on request
//! from a worker of its own.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use super::{Installed, Offer, UpdateError, Updates, VERSION, release_page};
use crate::controller::Shared;
use crate::events::{Command, Event, UpdateState};

/// The first check, once the daemon has settled; it also clears what an
/// earlier update left behind.
const FIRST_CHECK: Duration = Duration::from_mins(1);
const DAILY: Duration = Duration::from_hours(24);
/// After a check that failed (offline at login, say).
const RETRY: Duration = Duration::from_hours(1);
/// Waits are cut into pieces this long: the clock they run on stops while
/// the computer sleeps.
const WAKE: Duration = Duration::from_mins(10);

/// Where a socket client waits for the answer to its request.
pub(crate) type Reply = flume::Sender<Result<UpdateState, String>>;

pub(crate) enum Request {
    /// Check now, whatever `[updates] check` says.
    Check(Option<Reply>),
    /// Install what the last check found.
    Install(Option<Reply>),
    /// The settings changed; checks may have been switched on or off.
    Reconfigured,
    /// From the install worker.
    Progress {
        downloaded: u64,
        total: u64,
    },
    Installing,
    /// The new app to start, or why installing failed.
    Done(Result<PathBuf, String>),
}

pub(crate) fn spawn(
    updates: Updates,
    shared: Arc<Shared>,
    requests: flume::Sender<Request>,
    incoming: flume::Receiver<Request>,
) -> io::Result<()> {
    let service = Service {
        enabled: checks_enabled(&shared),
        updates,
        shared,
        requests,
        machine: Machine::default(),
        next: SystemTime::now() + FIRST_CHECK,
        cleaned: false,
    };
    thread::Builder::new()
        .name("earwig-update".into())
        .spawn(move || service.run(&incoming))?;
    Ok(())
}

fn checks_enabled(shared: &Shared) -> bool {
    shared
        .config
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .config()
        .updates
        .check
}

struct Service {
    updates: Updates,
    shared: Arc<Shared>,
    /// For the install worker's reports.
    requests: flume::Sender<Request>,
    machine: Machine,
    /// `[updates] check`.
    enabled: bool,
    next: SystemTime,
    cleaned: bool,
}

impl Service {
    fn run(mut self, incoming: &flume::Receiver<Request>) {
        loop {
            let wait = self
                .next
                .duration_since(SystemTime::now())
                .unwrap_or_default()
                .min(WAKE);
            match incoming.recv_timeout(wait) {
                Ok(request) => self.handle(request),
                Err(flume::RecvTimeoutError::Timeout) => self.tick(),
                Err(flume::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn tick(&mut self) {
        let now = SystemTime::now();
        if now < self.next {
            return;
        }
        self.next = now + RETRY;
        if self.machine.busy() {
            return;
        }
        if !self.cleaned {
            self.cleaned = true;
            self.updates.clean();
        }
        if self.enabled {
            let _ = self.check();
        } else {
            self.next = now + DAILY;
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Check(reply) => {
                let answer = if self.machine.busy() {
                    Ok(self.machine.state.clone())
                } else {
                    self.check()
                };
                answer_to(reply, answer);
            }
            Request::Install(reply) => {
                let answer = self.install();
                answer_to(reply, answer);
            }
            Request::Reconfigured => self.reconfigured(),
            Request::Progress { downloaded, total } => {
                if self.machine.progress(downloaded, total) {
                    self.publish();
                }
            }
            Request::Installing => {
                if self.machine.installing() {
                    self.publish();
                }
            }
            Request::Done(Ok(app)) => {
                log::info!("the update is in place; starting {}", app.display());
                *self
                    .shared
                    .relaunch
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(app);
                let _ = self.shared.commands.send(Command::Quit);
            }
            Request::Done(Err(reason)) => {
                log::warn!("updating failed: {reason}");
                if self.machine.failed(reason) {
                    self.publish();
                }
            }
        }
    }

    fn check(&mut self) -> Result<UpdateState, String> {
        match self.updates.check() {
            Ok(offer) => {
                self.next = SystemTime::now() + DAILY;
                match &offer {
                    Some(offer) => log::info!("earwig {} is out", offer.version),
                    None => log::info!("earwig {VERSION} is the newest"),
                }
                if self.machine.checked(offer, self.updates.installable()) {
                    self.publish();
                }
                Ok(self.machine.state.clone())
            }
            Err(e) => {
                log::warn!("checking for updates: {e}");
                Err(e.to_string())
            }
        }
    }

    fn install(&mut self) -> Result<UpdateState, String> {
        if self.machine.busy() {
            return Ok(self.machine.state.clone());
        }
        let offer = self.machine.start()?;
        self.publish();
        log::info!("updating to earwig {}", offer.version);
        let updates = self.updates.clone();
        let report = self.requests.clone();
        let spawned = thread::Builder::new()
            .name("earwig-update-install".into())
            .spawn(move || {
                let done = install(&updates, &offer, &report);
                let _ = report.send(Request::Done(done));
            });
        if let Err(e) = spawned {
            let reason = format!("starting the download: {e}");
            self.machine.failed(reason.clone());
            self.publish();
            return Err(reason);
        }
        Ok(self.machine.state.clone())
    }

    fn reconfigured(&mut self) {
        let enabled = checks_enabled(&self.shared);
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            self.next = SystemTime::now();
        } else if self.machine.forget() {
            self.publish();
        }
    }

    fn publish(&self) {
        let update = self.machine.state.clone();
        self.shared
            .status
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .update = Some(update.clone());
        self.shared.bus.publish(&Event::Update { update });
    }
}

fn answer_to(reply: Option<Reply>, answer: Result<UpdateState, String>) {
    if let Some(reply) = reply {
        let _ = reply.send(answer);
    }
}

/// Download, check and install `offer` under the update lock, reporting
/// along the way; the app to start next (macOS), or why it failed.
fn install(
    updates: &Updates,
    offer: &Offer,
    report: &flume::Sender<Request>,
) -> Result<PathBuf, String> {
    let lock = updates
        .lock()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| UpdateError::Busy.to_string())?;
    let file = updates
        .download(&lock, offer, &mut |downloaded, total| {
            let _ = report.send(Request::Progress { downloaded, total });
        })
        .map_err(|e| e.to_string())?;
    let _ = report.send(Request::Installing);
    let result = match updates.install(&lock, offer, &file) {
        Ok(Installed::Relaunch(app)) => Ok(app),
        // It stops earwig before it replaces it, so earwig still running
        // means it gave up.
        Ok(Installed::Installer(mut installer)) => Err(match installer.wait() {
            Ok(status) => format!("the installer stopped ({status}) without updating earwig"),
            Err(e) => format!("waiting for the installer: {e}"),
        }),
        Err(e) => Err(e.to_string()),
    };
    updates.clear(&lock);
    result
}

/// The update state and the release behind it.
#[derive(Default)]
struct Machine {
    state: UpdateState,
    offer: Option<Offer>,
}

impl Machine {
    fn busy(&self) -> bool {
        matches!(
            self.state,
            UpdateState::Downloading { .. } | UpdateState::Installing { .. }
        )
    }

    fn set(&mut self, state: UpdateState) -> bool {
        let changed = self.state != state;
        self.state = state;
        changed
    }

    /// What a check found. A failed update keeps its reason until another
    /// release, or none, comes along; one underway is left alone.
    fn checked(&mut self, offer: Option<Offer>, installable: bool) -> bool {
        if self.busy() {
            return false;
        }
        let state = match &offer {
            None => UpdateState::None,
            Some(offer) => match &self.state {
                UpdateState::Failed { version, .. } if *version == offer.version.to_string() => {
                    self.state.clone()
                }
                _ => UpdateState::Available {
                    version: offer.version.to_string(),
                    installable,
                },
            },
        };
        self.offer = offer;
        self.set(state)
    }

    /// Start on what the last check found; trying again after a failure too.
    fn start(&mut self) -> Result<Offer, String> {
        let offer = match (&self.state, &self.offer) {
            (
                UpdateState::Available {
                    installable: true, ..
                }
                | UpdateState::Failed { .. },
                Some(offer),
            ) => offer.clone(),
            (UpdateState::Available { version, .. }, _) => {
                return Err(format!(
                    "this copy of earwig can't replace itself; earwig {version} is at {}",
                    release_page(version)
                ));
            }
            _ => return Err(format!("earwig {VERSION} is the newest")),
        };
        self.set(UpdateState::Downloading {
            version: offer.version.to_string(),
            downloaded: 0,
            total: offer.size,
        });
        Ok(offer)
    }

    fn progress(&mut self, downloaded: u64, total: u64) -> bool {
        let UpdateState::Downloading { version, .. } = &self.state else {
            return false;
        };
        let version = version.clone();
        self.set(UpdateState::Downloading {
            version,
            downloaded,
            total,
        })
    }

    fn installing(&mut self) -> bool {
        let UpdateState::Downloading { version, .. } = &self.state else {
            return false;
        };
        let version = version.clone();
        self.set(UpdateState::Installing { version })
    }

    fn failed(&mut self, reason: String) -> bool {
        let version = match &self.state {
            UpdateState::Downloading { version, .. } | UpdateState::Installing { version } => {
                version.clone()
            }
            _ => return false,
        };
        self.set(UpdateState::Failed { version, reason })
    }

    /// Checks were switched off: nothing to show, unless an update is
    /// underway.
    fn forget(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        self.offer = None;
        self.set(UpdateState::None)
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;

    fn offer(version: &str) -> Offer {
        Offer {
            version: Version::parse(version).unwrap(),
            url: format!("https://github.com/adeci/earwig/releases/download/v{version}/x"),
            size: 1000,
            sha256: "0".repeat(64),
        }
    }

    fn available(version: &str) -> UpdateState {
        UpdateState::Available {
            version: version.into(),
            installable: true,
        }
    }

    #[test]
    fn a_check_finding_a_release_makes_it_available_and_none_clears_it() {
        let mut machine = Machine::default();
        assert!(!machine.checked(None, true), "nothing changed");
        assert!(machine.checked(Some(offer("0.2.0")), true));
        assert_eq!(machine.state, available("0.2.0"));
        assert!(!machine.checked(Some(offer("0.2.0")), true));
        assert!(machine.checked(Some(offer("0.3.0")), true));
        assert_eq!(machine.state, available("0.3.0"));
        assert!(machine.checked(None, true));
        assert_eq!(machine.state, UpdateState::None);
    }

    #[test]
    fn installing_goes_through_downloading_and_installing() {
        let mut machine = Machine::default();
        machine.checked(Some(offer("0.2.0")), true);
        assert_eq!(machine.start(), Ok(offer("0.2.0")));
        assert_eq!(
            machine.state,
            UpdateState::Downloading {
                version: "0.2.0".into(),
                downloaded: 0,
                total: 1000
            }
        );
        assert!(machine.progress(400, 1000));
        assert!(matches!(
            machine.state,
            UpdateState::Downloading {
                downloaded: 400,
                ..
            }
        ));
        // A check meanwhile changes nothing, not even to a newer release.
        assert!(!machine.checked(Some(offer("0.3.0")), true));
        assert!(!machine.checked(None, true));
        assert!(!machine.forget());
        assert!(machine.installing());
        assert_eq!(
            machine.state,
            UpdateState::Installing {
                version: "0.2.0".into()
            }
        );
        assert!(!machine.progress(1000, 1000), "the download is over");
    }

    #[test]
    fn a_failure_keeps_its_reason_until_retried_or_overtaken() {
        let mut machine = Machine::default();
        machine.checked(Some(offer("0.2.0")), true);
        machine.start().unwrap();
        assert!(machine.failed("the download does not match".into()));
        let failed = UpdateState::Failed {
            version: "0.2.0".into(),
            reason: "the download does not match".into(),
        };
        assert_eq!(machine.state, failed);
        // The daily check finds the same release: the reason stays.
        assert!(!machine.checked(Some(offer("0.2.0")), true));
        assert_eq!(machine.state, failed);
        // Trying again starts over.
        assert_eq!(machine.start(), Ok(offer("0.2.0")));
        assert!(machine.failed("again".into()));
        // A newer release replaces it.
        assert!(machine.checked(Some(offer("0.2.1")), true));
        assert_eq!(machine.state, available("0.2.1"));
    }

    #[test]
    fn nothing_starts_without_an_installable_release() {
        let mut machine = Machine::default();
        assert!(machine.start().unwrap_err().contains("is the newest"));
        machine.checked(Some(offer("0.2.0")), false);
        assert_eq!(
            machine.state,
            UpdateState::Available {
                version: "0.2.0".into(),
                installable: false
            }
        );
        let refused = machine.start().unwrap_err();
        assert!(
            refused.contains("https://github.com/adeci/earwig/releases/tag/v0.2.0"),
            "{refused}"
        );
        assert!(!machine.busy());
        assert!(!machine.failed("nothing was underway".into()));
    }

    #[test]
    fn switching_checks_off_forgets_the_release() {
        let mut machine = Machine::default();
        machine.checked(Some(offer("0.2.0")), true);
        assert!(machine.forget());
        assert_eq!(machine.state, UpdateState::None);
        assert!(machine.start().is_err());
    }
}
