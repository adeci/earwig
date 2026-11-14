//! Where key presses come from. A source only reports edges as
//! [`Command::Press`]/[`Command::Release`] ([`Command::Toggle`] without
//! releases); [`crate::session`] owns the timing. Sources: the keyboard
//! through [`earwig_keys`], the control socket ([`crate::ipc`]), and on Linux
//! the GlobalShortcuts portal.

#[cfg(target_os = "linux")]
pub mod linux;

use std::sync::{LazyLock, Mutex, PoisonError};

use earwig_keys::{Blocking, Event, Listener, Options, Status};
pub use earwig_keys::{Check, Hotkey, Recorded};

use crate::config::HotkeyConfig;
use crate::desktop::DesktopCaps;
use crate::events::{Command, HotkeyStatus, KeyboardState};

pub trait HotkeySource: Send {
    /// Name for [`crate::events::Status::hotkey_sources`].
    fn name(&self) -> &'static str;

    /// Run until the sender is dropped or the source fails; called on its own
    /// thread.
    fn run(self: Box<Self>, commands: flume::Sender<Command>) -> anyhow::Result<()>;
}

/// The one keyboard listener of the process.
static KEYBOARD: LazyLock<Listener> = LazyLock::new(|| Listener::new(Options::default()));

/// What [`configure`] last made of `[hotkey] key`.
static KEY: Mutex<Result<String, String>> = Mutex::new(Ok(String::new()));

/// The session's `XDG_CURRENT_DESKTOP`, which decides what a lone Super key
/// does.
static DESKTOP: Mutex<String> = Mutex::new(String::new());

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error(transparent)]
    Parse(#[from] earwig_keys::ParseError),
    #[error("{0}")]
    Refused(String),
}

/// Sources to start besides the always-on socket.
pub fn sources(config: &HotkeyConfig, caps: &DesktopCaps) -> Vec<Box<dyn HotkeySource>> {
    caps.desktop
        .clone_into(&mut DESKTOP.lock().unwrap_or_else(PoisonError::into_inner));
    if let Err(e) = configure(config) {
        log::error!("hotkey: {e}");
    }
    let keyboard: Box<dyn HotkeySource> = Box::new(Keyboard);
    #[cfg(target_os = "linux")]
    {
        std::iter::once(keyboard)
            .chain(linux::sources(config, caps))
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec![keyboard]
    }
}

/// Watch [`HotkeyConfig::key`] from now on. A key that does not parse, or
/// that [`Hotkey::check`] refuses, is not watched until it is fixed.
pub fn configure(config: &HotkeyConfig) -> Result<(), KeyError> {
    KEYBOARD.set_options(Options {
        block: config.block,
    });
    let parsed = parse(&config.key);
    let key = match &parsed {
        Ok(Some(hotkey)) => Ok(hotkey.to_string()),
        Ok(None) => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    };
    let changed = *KEY.lock().unwrap_or_else(PoisonError::into_inner) != key;
    if changed {
        log::info!("hotkey: {}", config.key);
    }
    *KEY.lock().unwrap_or_else(PoisonError::into_inner) = key;
    match parsed {
        Ok(hotkey) => {
            KEYBOARD.set_hotkey(hotkey);
            Ok(())
        }
        Err(e) => {
            KEYBOARD.set_hotkey(None);
            Err(e)
        }
    }
}

/// `key` as earwig watches it: `None` when empty; an error when it does not
/// parse or is unusable here. A key that is usable with a catch is logged.
pub fn parse(key: &str) -> Result<Option<Hotkey>, KeyError> {
    if key.trim().is_empty() {
        return Ok(None);
    }
    let hotkey: Hotkey = key.parse()?;
    match hotkey.check(&desktop()) {
        Check::Fine => {}
        Check::Warn(reason) => log::warn!("hotkey {hotkey}: {reason}"),
        Check::Reject(reason) => return Err(KeyError::Refused(reason)),
    }
    Ok(Some(hotkey))
}

/// The session's desktop, as [`Hotkey::check`] takes it.
pub fn desktop() -> String {
    DESKTOP
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// How the hotkey reaches earwig now, for [`crate::events::Status`].
pub fn status() -> HotkeyStatus {
    let key = KEY.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let keyboard = KEYBOARD.status();
    let (state, mut reason) = match &keyboard {
        Status::Stopped => (KeyboardState::Stopped, Some(keyboard.to_string())),
        Status::NoAccess => (
            KeyboardState::NoAccess,
            Some(format!("keyboard access missing: {keyboard}")),
        ),
        Status::NoKeyboard => (KeyboardState::NoKeyboard, Some(keyboard.to_string())),
        // With `blocking: false`, why other apps get the key too.
        Status::Watching(Blocking::No(why)) => (KeyboardState::Watching, Some(why.clone())),
        Status::Watching(_) => (KeyboardState::Watching, None),
    };
    let watched = match &key {
        Ok(key) if key.is_empty() => {
            reason = Some("no key set".into());
            false
        }
        Ok(_) => true,
        Err(e) => {
            reason = Some(e.clone());
            false
        }
    };
    let source = if watched && state == KeyboardState::Watching {
        "keyboard"
    } else if portal_bound() {
        "portal"
    } else {
        "socket"
    };
    HotkeyStatus {
        source: source.into(),
        key: key.unwrap_or_default(),
        keyboard: state,
        blocking: watched && keyboard == Status::Watching(Blocking::Yes),
        reason,
    }
}

fn portal_bound() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::portal_bound()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Record a new hotkey for the settings window: keys go to `sink` instead,
/// until it reports done or cancelled, or [`stop_recording`].
pub fn record(sink: impl FnMut(Recorded) + Send + 'static) {
    KEYBOARD.record(sink);
}

pub fn stop_recording() {
    KEYBOARD.stop_recording();
}

/// [`HotkeyConfig::key`], watched on the keyboard.
struct Keyboard;

impl HotkeySource for Keyboard {
    fn name(&self) -> &'static str {
        "keyboard"
    }

    fn run(self: Box<Self>, commands: flume::Sender<Command>) -> anyhow::Result<()> {
        KEYBOARD.run(|event| {
            let command = match event {
                Event::Pressed => Command::Press,
                Event::Released => Command::Release,
                Event::Chord => Command::Chord,
            };
            commands.send(command).is_ok()
        })?;
        Ok(())
    }
}
