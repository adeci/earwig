//! Insertion through the RemoteDesktop portal and libei (GNOME, KDE).
//! (GNOME slice.)

use std::path::Path;

use super::{InsertError, Inserter};
use crate::events::InsertMethod;

/// A RemoteDesktop session with a keyboard over libei, plus clipboard access.
/// The user approves it once; the restore token in `state_dir` keeps it.
pub struct PortalInserter {
    _private: (),
}

impl PortalInserter {
    /// Open (or restore) the portal session. Called at daemon start so the
    /// consent dialog never interrupts a dictation.
    pub fn connect(_state_dir: &Path) -> anyhow::Result<Self> {
        anyhow::bail!("portal inserter not implemented yet")
    }
}

impl Inserter for PortalInserter {
    fn methods(&self) -> Vec<InsertMethod> {
        vec![InsertMethod::PortalPaste, InsertMethod::PortalType]
    }

    fn insert(&mut self, _text: &str) -> Result<InsertMethod, InsertError> {
        Err(InsertError::NotApplicable("portal inserter not implemented yet".into()))
    }

    fn copy_to_clipboard(&mut self, _text: &str) -> Result<(), InsertError> {
        Err(InsertError::NotApplicable("portal inserter not implemented yet".into()))
    }
}
