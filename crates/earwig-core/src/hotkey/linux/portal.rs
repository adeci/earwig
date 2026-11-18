//! The XDG GlobalShortcuts portal (GNOME 48+, KDE): `dictate`, suggesting
//! [`HotkeyConfig::portal_trigger`](crate::config::HotkeyConfig), and `cancel`.
//! Desktops remember the bindings per app id, hence [`register_app_id`] first.
//! GNOME and KDE have the user confirm them at start, which may take as long
//! as they like; after a no, GNOME asks again at the next start, and KDE keeps
//! the shortcut without a key for the user to give one in its settings.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::bail;
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use futures_lite::{StreamExt as _, future, pin};

use crate::events::Command;
use crate::hotkey::HotkeySource;
use crate::insert::linux::portal::{bounded, register_app_id};

const DICTATE: &str = "dictate";
const CANCEL: &str = "cancel";

/// Session setup involves no user interaction.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Dictating has a key and the portal sends its edges.
pub(super) static BOUND: AtomicBool = AtomicBool::new(false);

pub struct PortalShortcuts {
    trigger: String,
}

enum Input {
    /// An activation edge, and the command it is if any.
    Edge(Option<Command>),
    /// Whether dictating has a key now.
    Changed(bool),
    /// The portal stopped sending signals.
    Ended,
}

impl PortalShortcuts {
    /// `trigger` in XDG shortcut syntax (`LOGO+space`); empty for none.
    pub fn new(trigger: &str) -> Self {
        Self {
            trigger: trigger.to_owned(),
        }
    }

    async fn serve(self, commands: flume::Sender<Command>) -> anyhow::Result<()> {
        // A backend may advertise a portal it can't serve here (the GNOME one
        // outside GNOME); give up quietly, compositor binds and the socket work.
        let setup = bounded(SETUP_TIMEOUT, "GlobalShortcuts session setup", async {
            let portal = GlobalShortcuts::new().await?;
            let session = portal
                .create_session(ashpd::desktop::CreateSessionOptions::default())
                .await?;
            // Subscribe before binding so no activation is missed. Signals only
            // reach this connection, and this is its one session: no filtering.
            let activated = portal.receive_activated().await?;
            let deactivated = portal.receive_deactivated().await?;
            let changed = portal.receive_shortcuts_changed().await?;
            Ok((portal, session, activated, deactivated, changed))
        })
        .await;
        let (portal, session, activated, deactivated, changed) = match setup {
            Ok(setup) => setup,
            Err(e) => {
                log::info!("GlobalShortcuts portal unusable, not using it: {e:#}");
                return Ok(());
            }
        };
        pin!(activated, deactivated, changed);

        let trigger = Some(self.trigger.as_str()).filter(|t| !t.is_empty());
        let shortcuts = [
            NewShortcut::new(DICTATE, "Dictate: hold to talk, tap to start or stop")
                .preferred_trigger(trigger),
            NewShortcut::new(CANCEL, "Cancel dictation"),
        ];
        let bound = portal
            .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
            .await
            .and_then(|request| request.response());
        match bound {
            Ok(bound) => {
                for shortcut in bound.shortcuts() {
                    log::info!(
                        "portal shortcut {}: {}",
                        shortcut.id(),
                        shortcut.trigger_description()
                    );
                }
                keyed(has_key(
                    bound
                        .shortcuts()
                        .iter()
                        .map(|s| (s.id(), s.trigger_description())),
                ));
            }
            // A key given in the desktop's settings still arrives as a change.
            Err(e) => log::warn!("the desktop did not give earwig its shortcut: {e}"),
        }

        loop {
            let input = future::or(
                async {
                    activated.next().await.map_or(Input::Ended, |s| {
                        Input::Edge(command(s.shortcut_id(), true))
                    })
                },
                future::or(
                    async {
                        deactivated.next().await.map_or(Input::Ended, |s| {
                            Input::Edge(command(s.shortcut_id(), false))
                        })
                    },
                    async {
                        changed.next().await.map_or(Input::Ended, |changed| {
                            Input::Changed(has_key(
                                changed
                                    .shortcuts()
                                    .iter()
                                    .map(|s| (s.id(), s.trigger_description())),
                            ))
                        })
                    },
                ),
            )
            .await;
            match input {
                Input::Ended => bail!("the GlobalShortcuts portal stopped sending signals"),
                Input::Edge(None) => {}
                Input::Edge(Some(command)) => {
                    log::debug!("portal shortcut: {command:?}");
                    if commands.send(command).is_err() {
                        return Ok(());
                    }
                }
                Input::Changed(key) => keyed(key),
            }
        }
    }
}

impl HotkeySource for PortalShortcuts {
    fn name(&self) -> &'static str {
        "portal"
    }

    fn run(self: Box<Self>, commands: flume::Sender<Command>) -> anyhow::Result<()> {
        register_app_id();
        let result = async_io::block_on(self.serve(commands));
        BOUND.store(false, Ordering::Relaxed);
        result
    }
}

/// Dictating has a `key`, or none: KDE keeps a refused shortcut without one.
fn keyed(key: bool) {
    if !key {
        log::warn!("earwig's shortcut has no key");
    }
    BOUND.store(key, Ordering::Relaxed);
}

/// Whether `shortcuts` (id, trigger) give dictating a key. GNOME leaves one
/// without a key out.
fn has_key<'a>(shortcuts: impl IntoIterator<Item = (&'a str, &'a str)>) -> bool {
    shortcuts
        .into_iter()
        .any(|(id, trigger)| id == DICTATE && !trigger.trim().is_empty())
}

fn command(shortcut: &str, active: bool) -> Option<Command> {
    match (shortcut, active) {
        (DICTATE, true) => Some(Command::Press),
        (DICTATE, false) => Some(Command::Release),
        (CANCEL, true) => Some(Command::Cancel),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictating_needs_a_key_of_its_own() {
        assert!(has_key([(CANCEL, ""), (DICTATE, "Ctrl+Space")]));
        // KDE after a Cancel: both kept, neither with a key.
        assert!(!has_key([(CANCEL, ""), (DICTATE, "")]));
        // GNOME leaves a shortcut without a key out.
        assert!(!has_key([(CANCEL, "Press <ctrl>Escape")]));
        assert!(!has_key([]));
    }

    #[test]
    fn only_dictate_presses_and_releases() {
        assert_eq!(command(DICTATE, true), Some(Command::Press));
        assert_eq!(command(DICTATE, false), Some(Command::Release));
        assert_eq!(command(CANCEL, true), Some(Command::Cancel));
        assert_eq!(command(CANCEL, false), None);
    }
}
