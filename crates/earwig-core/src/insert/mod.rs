//! Getting the final text into whatever has keyboard focus, in one step.
//!
//! Backends, best first:
//! - `input_method`: Wayland `zwp_input_method_v2.commit_string` (niri, sway,
//!   Hyprland). Atomic, leaves the clipboard alone; needs the focused client
//!   to have text-input-v3 enabled.
//! - `paste`: clipboard via data-control plus one paste chord through
//!   `zwp_virtual_keyboard_v1`; Ctrl+Shift+V when the focused toplevel is a
//!   terminal. Covers clients without text-input (Electron, XWayland).
//! - `portal`: RemoteDesktop portal (GNOME, KDE): clipboard portal + paste
//!   chord, or a keystroke burst, over libei.
//!
//! [`Inserter::insert`] returns [`InsertError::NotApplicable`] when a backend
//! cannot handle the current focus, so the chain moves on; any other error is
//! a real failure.

use crate::events::InsertMethod;

pub trait Inserter: Send {
    /// Methods this inserter may use, best first (for [`crate::events::Status`]).
    fn methods(&self) -> Vec<InsertMethod>;

    /// Deliver `text` to the focused application.
    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError>;

    /// Put `text` on the clipboard only. Used when insertion failed so the
    /// user can paste by hand.
    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError>;
}

#[derive(Debug, thiserror::Error)]
pub enum InsertError {
    /// This backend cannot serve the current focus (no active text field, no
    /// protocol). Try the next one.
    #[error("not applicable: {0}")]
    NotApplicable(String),
    #[error(transparent)]
    Failed(#[from] anyhow::Error),
}

pub mod portal;

/// The inserter for this session: the configured method, or for `auto` every
/// usable backend best first.
// Insertion slice: replace the body with the real chain.
pub fn auto(
    _config: &crate::config::InsertConfig,
    _caps: &crate::desktop::DesktopCaps,
) -> anyhow::Result<Box<dyn Inserter>> {
    anyhow::bail!("no insertion backend implemented yet")
}
