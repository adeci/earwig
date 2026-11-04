//! XDG GlobalShortcuts portal (any desktop whose portal backend implements
//! it, e.g. GNOME 48+, KDE).
//!
//! Binds two shortcuts: `dictate` (preferred trigger from
//! [`HotkeyConfig::portal_trigger`](crate::config::HotkeyConfig)) and
//! `cancel` (no default; the user picks one in the desktop's settings).
//! The desktop asks the user to confirm the bindings once and remembers them
//! per app id, which is why [`register_app_id`] runs first. When the portal
//! does not work, [`HotkeySource::run`] logs at info and returns `Ok`.

use std::time::Duration;

use anyhow::bail;
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use futures_lite::{StreamExt as _, future, pin};

use super::HotkeySource;
use crate::events::Command;
use crate::insert::portal::{bounded, register_app_id};

const DICTATE: &str = "dictate";
const CANCEL: &str = "cancel";

/// Session setup involves no user interaction.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Binding may wait for the user to confirm in a dialog.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

pub struct PortalShortcuts {
    trigger: String,
}

impl PortalShortcuts {
    /// `trigger` in XDG shortcut syntax (`LOGO+space`); empty for none.
    pub fn new(trigger: &str) -> Self {
        Self {
            trigger: trigger.to_owned(),
        }
    }

    async fn serve(self, commands: flume::Sender<Command>) -> anyhow::Result<()> {
        // A portal can be advertised by a backend that cannot work on this
        // compositor (xdg-desktop-portal-gnome outside GNOME); setup then
        // fails or never answers. Give up quietly: compositor binds and the
        // socket still work.
        let setup = bounded(SETUP_TIMEOUT, "GlobalShortcuts session setup", async {
            let portal = GlobalShortcuts::new().await?;
            let session = portal
                .create_session(ashpd::desktop::CreateSessionOptions::default())
                .await?;
            // Subscribe before binding so no activation is missed. Signals
            // reach only the connection that owns the session, and this
            // process has one GlobalShortcuts session, so they need no
            // filtering.
            let activated = portal.receive_activated().await?;
            let deactivated = portal.receive_deactivated().await?;
            Ok((portal, session, activated, deactivated))
        })
        .await;
        let (portal, session, activated, deactivated) = match setup {
            Ok(setup) => setup,
            Err(e) => {
                log::info!("GlobalShortcuts portal unusable, not using it: {e:#}");
                return Ok(());
            }
        };
        pin!(activated, deactivated);

        let trigger = Some(self.trigger.as_str()).filter(|t| !t.is_empty());
        let shortcuts = [
            NewShortcut::new(DICTATE, "Dictate: hold to talk, tap to start or stop")
                .preferred_trigger(trigger),
            NewShortcut::new(CANCEL, "Cancel dictation"),
        ];
        // The desktop may ask the user to confirm the bindings.
        let bound = bounded(CONSENT_TIMEOUT, "binding the shortcuts", async {
            Ok(portal
                .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
                .await?
                .response()?)
        })
        .await;
        let bound = match bound {
            Ok(bound) => bound,
            Err(e) => {
                log::info!("GlobalShortcuts not bound, not using the portal: {e:#}");
                return Ok(());
            }
        };
        for shortcut in bound.shortcuts() {
            log::info!(
                "portal shortcut {}: {}",
                shortcut.id(),
                shortcut.trigger_description()
            );
        }

        loop {
            let edge = future::or(
                async {
                    activated
                        .next()
                        .await
                        .map(|s| command(s.shortcut_id(), true))
                },
                async {
                    deactivated
                        .next()
                        .await
                        .map(|s| command(s.shortcut_id(), false))
                },
            )
            .await;
            let Some(edge) = edge else {
                bail!("the GlobalShortcuts portal stopped sending signals");
            };
            if let Some(command) = edge {
                log::debug!("portal shortcut: {command:?}");
                if commands.send(command).is_err() {
                    return Ok(());
                }
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
        async_io::block_on(self.serve(commands))
    }
}

fn command(shortcut: &str, active: bool) -> Option<Command> {
    match (shortcut, active) {
        (DICTATE, true) => Some(Command::Press),
        (DICTATE, false) => Some(Command::Release),
        (CANCEL, true) => Some(Command::Cancel),
        _ => None,
    }
}
