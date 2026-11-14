//! Linux backends, best first: `input_method` (Wayland commit_string; niri,
//! sway, Hyprland), `paste` (data-control clipboard plus one chord on a virtual
//! keyboard; Electron, XWayland) and `portal` (RemoteDesktop; GNOME, KDE).

use anyhow::Context as _;

use super::{InsertError, Inserter};
use crate::config::{InsertConfig, InsertSetting};
use crate::desktop::DesktopCaps;
use input_method::InputMethodInserter;
use paste::PasteInserter;
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

/// `auto` uses the portal only where the Wayland paste path is missing (it
/// asks for consent), and leaves out backends that fail to connect.
pub(super) fn backends(
    config: &InsertConfig,
    caps: &DesktopCaps,
) -> anyhow::Result<Vec<Box<dyn Inserter>>> {
    Ok(match config.method {
        InsertSetting::Auto => auto_backends(config, caps),
        InsertSetting::InputMethod => {
            let mut backends: Vec<Box<dyn Inserter>> = vec![Box::new(
                InputMethodInserter::connect().context("input method")?,
            )];
            // Text that could not be inserted still lands on the clipboard.
            if caps.data_control {
                backends.extend(connected("clipboard", PasteInserter::clipboard_only()));
            }
            backends
        }
        InsertSetting::Paste => vec![Box::new(
            PasteInserter::connect(&config.terminals).context("paste")?,
        )],
        InsertSetting::Portal => vec![Box::new(
            PortalInserter::connect(&crate::paths::state_dir()).context("RemoteDesktop portal")?,
        )],
    })
}

fn auto_backends(config: &InsertConfig, caps: &DesktopCaps) -> Vec<Box<dyn Inserter>> {
    let mut backends = Vec::new();
    if caps.input_method {
        backends.extend(connected("input method", InputMethodInserter::connect()));
    }
    let mut paste_chord = false;
    if caps.data_control && caps.virtual_keyboard {
        if let Some(paste) = connected("paste", PasteInserter::connect(&config.terminals)) {
            backends.push(paste);
            paste_chord = true;
        }
    } else if caps.data_control {
        backends.extend(connected("clipboard", PasteInserter::clipboard_only()));
    }
    if caps.portal_remote_desktop && !paste_chord {
        backends.extend(connected(
            "RemoteDesktop portal",
            PortalInserter::connect(&crate::paths::state_dir()),
        ));
    }
    backends
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
