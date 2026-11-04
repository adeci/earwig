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

use anyhow::Context as _;

use crate::config::{InsertConfig, InsertSetting};
use crate::desktop::DesktopCaps;
use crate::events::InsertMethod;
use input_method::InputMethodInserter;
use paste::PasteInserter;
use portal::PortalInserter;

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

pub mod input_method;
pub mod paste;
pub mod portal;
mod wayland;

/// The inserter for this session: the configured method, or for `auto` every
/// usable backend best first.
///
/// `auto` chains the input method (when offered), clipboard plus paste chord
/// (data-control and a virtual keyboard), and the RemoteDesktop portal only
/// where that Wayland paste path is missing (GNOME, KDE), since the portal
/// asks for consent. A backend that fails to connect is logged and left out;
/// with none left, `methods()` is empty and every insert reports why. A
/// configured method that cannot connect is an error.
pub fn auto(config: &InsertConfig, caps: &DesktopCaps) -> anyhow::Result<Box<dyn Inserter>> {
    let backends: Vec<Box<dyn Inserter>> = match config.method {
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
    };
    let chain = Chain { backends };
    log::info!("insertion methods: {:?}", chain.methods());
    Ok(Box::new(chain))
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

/// Backends best first: the first one that can serve the focus inserts.
struct Chain {
    backends: Vec<Box<dyn Inserter>>,
}

impl Inserter for Chain {
    fn methods(&self) -> Vec<InsertMethod> {
        self.backends
            .iter()
            .flat_map(|backend| backend.methods())
            .collect()
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        let mut skipped = Vec::new();
        for backend in &mut self.backends {
            match backend.insert(text) {
                Err(InsertError::NotApplicable(reason)) => skipped.push(reason),
                // A failure may have delivered part of the text; trying the
                // next backend could insert it twice.
                done => return done,
            }
        }
        Err(InsertError::NotApplicable(reasons(
            &skipped,
            "no insertion backend is available",
        )))
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        let mut skipped = Vec::new();
        let mut failed = None;
        for backend in &mut self.backends {
            match backend.copy_to_clipboard(text) {
                Ok(()) => return Ok(()),
                Err(InsertError::NotApplicable(reason)) => skipped.push(reason),
                Err(InsertError::Failed(err)) => {
                    log::warn!("copying to the clipboard failed: {err:#}");
                    failed.get_or_insert(err);
                }
            }
        }
        Err(match failed {
            Some(err) => InsertError::Failed(err),
            None => {
                InsertError::NotApplicable(reasons(&skipped, "no clipboard backend is available"))
            }
        })
    }
}

fn reasons(skipped: &[String], none: &str) -> String {
    if skipped.is_empty() {
        none.to_owned()
    } else {
        skipped.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Copy)]
    enum Outcome {
        Done,
        Skip,
        Fail,
    }

    /// A backend with fixed outcomes that records which backends were asked.
    struct Backend {
        name: &'static str,
        method: InsertMethod,
        insert: Outcome,
        copy: Outcome,
        asked: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Backend {
        fn result(&self, outcome: Outcome) -> Result<(), InsertError> {
            self.asked.lock().unwrap().push(self.name);
            match outcome {
                Outcome::Done => Ok(()),
                Outcome::Skip => Err(InsertError::NotApplicable(format!("{} skipped", self.name))),
                Outcome::Fail => Err(InsertError::Failed(anyhow::anyhow!("{} failed", self.name))),
            }
        }
    }

    impl Inserter for Backend {
        fn methods(&self) -> Vec<InsertMethod> {
            vec![self.method]
        }

        fn insert(&mut self, _text: &str) -> Result<InsertMethod, InsertError> {
            self.result(self.insert).map(|()| self.method)
        }

        fn copy_to_clipboard(&mut self, _text: &str) -> Result<(), InsertError> {
            self.result(self.copy)
        }
    }

    fn build(outcomes: &[(Outcome, Outcome)]) -> (Chain, Arc<Mutex<Vec<&'static str>>>) {
        const NAMES: [(&str, InsertMethod); 3] = [
            ("im", InsertMethod::InputMethod),
            ("paste", InsertMethod::Paste),
            ("portal", InsertMethod::PortalPaste),
        ];
        let asked = Arc::new(Mutex::new(Vec::new()));
        let backends = outcomes
            .iter()
            .zip(NAMES)
            .map(|(&(insert, copy), (name, method))| {
                Box::new(Backend {
                    name,
                    method,
                    insert,
                    copy,
                    asked: asked.clone(),
                }) as Box<dyn Inserter>
            })
            .collect();
        (Chain { backends }, asked)
    }

    #[test]
    fn not_applicable_moves_on_and_the_first_success_wins() {
        use Outcome::{Done, Skip};
        let (mut chain, asked) = build(&[(Skip, Skip), (Done, Done), (Done, Done)]);
        assert_eq!(chain.insert("x").unwrap(), InsertMethod::Paste);
        assert_eq!(*asked.lock().unwrap(), ["im", "paste"]);
        assert_eq!(
            chain.methods(),
            [
                InsertMethod::InputMethod,
                InsertMethod::Paste,
                InsertMethod::PortalPaste
            ]
        );
    }

    #[test]
    fn a_failure_stops_the_chain() {
        use Outcome::{Done, Fail};
        let (mut chain, asked) = build(&[(Fail, Done), (Done, Done)]);
        assert!(matches!(chain.insert("x"), Err(InsertError::Failed(_))));
        assert_eq!(*asked.lock().unwrap(), ["im"]);
    }

    #[test]
    fn nothing_applicable_reports_every_reason() {
        use Outcome::Skip;
        let (mut chain, _) = build(&[(Skip, Skip), (Skip, Skip)]);
        let Err(InsertError::NotApplicable(reason)) = chain.insert("x") else {
            panic!("expected not applicable");
        };
        assert_eq!(reason, "im skipped; paste skipped");

        let (mut empty, _) = build(&[]);
        assert_eq!(empty.methods(), []);
        assert!(matches!(
            empty.insert("x"),
            Err(InsertError::NotApplicable(_))
        ));
    }

    #[test]
    fn copy_uses_the_first_backend_that_can() {
        use Outcome::{Done, Fail, Skip};
        let (mut chain, asked) = build(&[(Done, Skip), (Done, Fail), (Done, Done)]);
        chain.copy_to_clipboard("x").unwrap();
        assert_eq!(*asked.lock().unwrap(), ["im", "paste", "portal"]);

        // With no taker, a real failure outranks "not applicable".
        let (mut chain, _) = build(&[(Done, Skip), (Done, Fail)]);
        assert!(matches!(
            chain.copy_to_clipboard("x"),
            Err(InsertError::Failed(_))
        ));
    }
}
