//! `earwig update`: through the running daemon, whose tray and settings follow
//! along, or on its own when none runs.

use earwig_core::events::{Command, Event, UpdateState};
use earwig_core::ipc;
use earwig_core::update::{self, Installed, UpdateError, Updates};

use crate::cli_models::ProgressLine;

pub fn run(check_only: bool) -> anyhow::Result<()> {
    match ipc::subscribe() {
        Ok(events) => with_daemon(events, check_only),
        Err(_) => alone(check_only),
    }
}

fn with_daemon(
    events: impl Iterator<Item = anyhow::Result<Event>>,
    check_only: bool,
) -> anyhow::Result<()> {
    let found = answer(&Command::CheckForUpdates)?;
    let installs = matches!(
        found,
        UpdateState::Available {
            installable: true,
            ..
        } | UpdateState::Failed { .. }
            | UpdateState::Downloading { .. }
            | UpdateState::Installing { .. }
    );
    if check_only || !installs {
        return report(&found, check_only);
    }
    let started = answer(&Command::Update)?;
    follow(events, &started)
}

/// The daemon's answer to an update request.
fn answer(cmd: &Command) -> anyhow::Result<UpdateState> {
    match ipc::request(cmd)? {
        Some(Event::Update { update }) => Ok(update),
        other => anyhow::bail!("unexpected reply: {other:?}"),
    }
}

/// What a check found; an error where `earwig update` can't install it.
fn report(state: &UpdateState, check_only: bool) -> anyhow::Result<()> {
    match state {
        UpdateState::None => println!("earwig {} is the newest", update::VERSION),
        UpdateState::Available {
            version,
            installable: true,
        } => println!("earwig {version} is out; `earwig update` installs it"),
        UpdateState::Available {
            version,
            installable: false,
        } => {
            let page = update::release_page(version);
            if !check_only {
                anyhow::bail!(
                    "earwig {version} is out, but this copy can't replace itself: {page}"
                );
            }
            println!("earwig {version} is out: {page} (this copy can't replace itself)");
        }
        UpdateState::Downloading { version, .. } => println!("downloading earwig {version}"),
        UpdateState::Installing { version } => println!("installing earwig {version}"),
        UpdateState::Failed { version, reason } => {
            println!("updating to earwig {version} failed: {reason}");
        }
    }
    Ok(())
}

/// The daemon's update, as it goes. On Windows this returns once the
/// installer runs: it replaces this program too, so it must have exited.
fn follow(
    events: impl Iterator<Item = anyhow::Result<Event>>,
    started: &UpdateState,
) -> anyhow::Result<()> {
    let mut line = ProgressLine::new();
    // Until this update shows, a failure is an older one's.
    let mut underway = matches!(
        started,
        UpdateState::Downloading { .. } | UpdateState::Installing { .. }
    );
    let mut installing = None;
    for event in events {
        let Event::Update { update } = event? else {
            continue;
        };
        match update {
            UpdateState::Downloading {
                version,
                downloaded,
                total,
            } => {
                underway = true;
                line.update(&format!("earwig {version}"), downloaded, total);
            }
            UpdateState::Installing { version } if underway => {
                line.finish();
                println!("installing earwig {version}");
                if cfg!(windows) {
                    println!("the installer stops earwig, replaces it and starts it again");
                    return Ok(());
                }
                installing = Some(version);
            }
            UpdateState::Failed { version, reason } if underway => {
                line.finish();
                anyhow::bail!("updating to earwig {version} failed: {reason}");
            }
            _ => {}
        }
    }
    line.finish();
    // The daemon quit to start the new app.
    match installing {
        Some(version) => {
            println!("earwig {version} is in place and starting");
            Ok(())
        }
        None => anyhow::bail!("earwig quit before the update was done"),
    }
}

/// No daemon: check, download and install right here. Nothing was running,
/// so nothing starts afterwards.
fn alone(check_only: bool) -> anyhow::Result<()> {
    let updates = Updates::detect().ok_or_else(|| anyhow::anyhow!(update::NOT_UPDATING))?;
    let Some(offer) = updates.check()? else {
        return report(&UpdateState::None, check_only);
    };
    let version = offer.version.to_string();
    if check_only || !updates.installable() {
        return report(
            &UpdateState::Available {
                version,
                installable: updates.installable(),
            },
            check_only,
        );
    }
    let lock = updates.lock()?.ok_or(UpdateError::Busy)?;
    let mut line = ProgressLine::new();
    let file = updates.download(&lock, &offer, &mut |downloaded, total| {
        line.update(&format!("earwig {version}"), downloaded, total);
    });
    line.finish();
    let file = file?;
    println!("installing earwig {version}");
    match updates.install(&lock, &offer, &file) {
        Ok(Installed::Relaunch(app)) => {
            updates.clear(&lock);
            println!("earwig {version} is in place: {}", app.display());
        }
        // It runs from the download, and replaces this program: let it be.
        Ok(Installed::Installer(_)) => println!("the installer runs in the background"),
        Err(e) => {
            updates.clear(&lock);
            return Err(e.into());
        }
    }
    Ok(())
}
