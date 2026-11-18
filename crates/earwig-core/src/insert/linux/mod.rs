//! Linux backends, best first: `input_method` (Wayland commit_string; niri,
//! sway, Hyprland, COSMIC), `paste` (the clipboard and the primary selection
//! set without focus, then Shift+Insert on the compositor's virtual keyboard
//! or earwig's own; Electron, XWayland, GNOME and KDE with keyboard access) and
//! `portal` (RemoteDesktop; GNOME and KDE without keyboard access).

use anyhow::Context as _;

use super::{InsertError, Inserter};
use crate::config::{InsertConfig, InsertSetting};
use crate::desktop::DesktopCaps;
use input_method::InputMethodInserter;
use paste::{PasteInserter, Selections};
use portal::PortalInserter;

pub mod input_method;
pub mod paste;
pub mod portal;
mod wayland;

/// Text targets offered on the clipboard, most specific first.
const MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "TEXT",
    "STRING",
];

/// `auto` uses the portal only where earwig can't press keys itself (it asks
/// for consent), and leaves out backends that fail to connect.
pub(super) fn backends(
    config: &InsertConfig,
    caps: &DesktopCaps,
) -> anyhow::Result<Vec<Box<dyn Inserter>>> {
    Ok(match config.method {
        InsertSetting::Auto => auto_backends(caps),
        InsertSetting::InputMethod => {
            let mut backends: Vec<Box<dyn Inserter>> = vec![Box::new(
                InputMethodInserter::connect().context("input method")?,
            )];
            // Text that could not be inserted still lands on the clipboard.
            backends.extend(connected("clipboard", Selections::connect(caps)));
            backends
        }
        InsertSetting::Paste => vec![Box::new(PasteInserter::connect(caps).context("paste")?)],
        InsertSetting::Portal => vec![Box::new(portal(caps).context("RemoteDesktop portal")?)],
    })
}

fn auto_backends(caps: &DesktopCaps) -> Vec<Box<dyn Inserter>> {
    let mut backends = Vec::new();
    if caps.input_method {
        backends.extend(connected("input method", InputMethodInserter::connect()));
    }
    match PasteInserter::connect(caps) {
        Ok(paste) => backends.push(Box::new(paste)),
        Err(err) => {
            log::warn!("paste insertion unavailable: {err:#}");
            if caps.portal_remote_desktop {
                backends.extend(connected("RemoteDesktop portal", portal(caps)));
            } else {
                // Text that could not be inserted still lands on the clipboard.
                backends.extend(connected("clipboard", Selections::connect(caps)));
            }
        }
    }
    backends
}

fn portal(caps: &DesktopCaps) -> anyhow::Result<PortalInserter> {
    PortalInserter::connect(&crate::paths::state_dir(), Selections::connect(caps)?)
}

fn connected<I: Inserter + 'static>(
    name: &str,
    inserter: anyhow::Result<I>,
) -> Option<Box<dyn Inserter>> {
    match inserter {
        Ok(inserter) => Some(Box::new(inserter)),
        Err(err) => {
            log::warn!("{name} insertion unavailable: {err:#}");
            None
        }
    }
}
