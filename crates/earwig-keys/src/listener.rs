//! The handle programs hold, and the state the backends share with it.

use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::Event;
use crate::backend;
use crate::hotkey::Hotkey;
use crate::key::{Held, Key};
use crate::matcher::{Action, Edge, Mask, Matcher};
use crate::recorder::{Recorded, Recorder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Linux: grab the keyboards to keep a key-with-modifiers hotkey from apps (other keys go on
    /// through a uinput clone). macOS and Windows always keep it.
    pub block: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { block: true }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// [`Listener::run`] has not started, or it ended.
    Stopped,
    /// No Accessibility (macOS) or keyboard read access (Linux).
    NoAccess,
    /// Linux: no keyboard to watch.
    NoKeyboard,
    Watching(Blocking),
}

/// Whether the hotkey is kept from other apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocking {
    Yes,
    /// Modifiers alone pass through on purpose (or there's no hotkey).
    PassThrough,
    /// It should be but isn't (Linux), for this reason.
    No(String),
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => f.write_str("not watching the keyboard"),
            Self::NoAccess if cfg!(target_os = "macos") => {
                f.write_str("Accessibility is not allowed for earwig")
            }
            Self::NoAccess => f.write_str("no access to the keyboards (/dev/input)"),
            Self::NoKeyboard => f.write_str("no keyboard found"),
            Self::Watching(Blocking::No(reason)) => {
                write!(
                    f,
                    "watching the keyboard; other apps see the shortcut too: {reason}"
                )
            }
            Self::Watching(_) => f.write_str("watching the keyboard"),
        }
    }
}

/// Watches the keyboard for one hotkey; clones share it. [`Self::run`] blocks its thread, the rest
/// may be called from anywhere.
#[derive(Clone)]
pub struct Listener {
    shared: Arc<Shared>,
}

pub(crate) struct Shared {
    state: Mutex<State>,
    pub(crate) waker: backend::Waker,
}

struct State {
    matcher: Option<Matcher>,
    options: Options,
    recording: Option<Recording>,
    /// Bumped on each recording start/stop, so one that ended while its sink ran isn't put back.
    generation: u64,
    status: Status,
    /// The key that just finished a recording: its repeats and release are swallowed too.
    recorded_key: Option<Key>,
    stop: bool,
}

struct Recording {
    recorder: Recorder,
    sink: Box<dyn FnMut(Recorded) + Send>,
}

/// What a backend needs to know about the configuration.
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub hotkey: Option<Hotkey>,
    /// Only Linux can fail to keep the hotkey from apps, or choose not to.
    #[cfg(target_os = "linux")]
    pub options: Options,
    pub stop: bool,
}

impl Listener {
    pub fn new(options: Options) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    matcher: None,
                    options,
                    recording: None,
                    generation: 0,
                    status: Status::Stopped,
                    recorded_key: None,
                    stop: false,
                }),
                waker: backend::Waker::default(),
            }),
        }
    }

    /// `None` watches for nothing (a recording still works).
    pub fn set_hotkey(&self, hotkey: Option<Hotkey>) {
        let mut state = self.shared.lock();
        if state.matcher.as_ref().map(Matcher::hotkey) == hotkey.as_ref() {
            return;
        }
        state.matcher = hotkey.map(Matcher::new);
        drop(state);
        self.shared.waker.wake();
    }

    pub fn set_options(&self, options: Options) {
        let mut state = self.shared.lock();
        if state.options != options {
            state.options = options;
            drop(state);
            self.shared.waker.wake();
        }
    }

    pub fn status(&self) -> Status {
        self.shared.lock().status.clone()
    }

    /// Send keys to `sink` instead of the hotkey until it's [`Recorded::Done`] or
    /// [`Recorded::Cancelled`] or [`Self::stop_recording`]; keys still reach apps.
    pub fn record(&self, sink: impl FnMut(Recorded) + Send + 'static) {
        let mut state = self.shared.lock();
        state.generation += 1;
        state.recording = Some(Recording {
            recorder: Recorder::default(),
            sink: Box::new(sink),
        });
    }

    pub fn stop_recording(&self) {
        let mut state = self.shared.lock();
        state.generation += 1;
        state.recording = None;
    }

    pub fn is_recording(&self) -> bool {
        self.shared.lock().recording.is_some()
    }

    /// Watches on this thread until [`Self::stop`] or `events` returns false, waiting for access
    /// and keyboards as needed.
    pub fn run(&self, mut events: impl FnMut(Event) -> bool) -> io::Result<()> {
        self.shared.lock().stop = false;
        let result = backend::run(&self.shared, &mut events);
        self.shared.set_status(Status::Stopped);
        result
    }

    pub fn stop(&self) {
        self.shared.lock().stop = true;
        self.shared.waker.wake();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        Snapshot {
            hotkey: state.matcher.as_ref().map(|m| m.hotkey().clone()),
            #[cfg(target_os = "linux")]
            options: state.options,
            stop: state.stop,
        }
    }

    pub(crate) fn set_status(&self, status: Status) {
        let mut state = self.lock();
        if state.status != status {
            log::info!("hotkey: {status}");
            state.status = status;
        }
    }

    /// What to do with one key edge; a recording takes it instead of the hotkey.
    pub(crate) fn edge(&self, key: Option<Key>, edge: Edge, held: Held) -> Action {
        let swallow = Action {
            swallow: true,
            ..Action::default()
        };
        let mut state = self.lock();
        if key.is_some() && key == state.recorded_key && edge != Edge::Down {
            if edge == Edge::Up {
                state.recorded_key = None;
            }
            return swallow;
        }
        let Some(recording) = &mut state.recording else {
            return state
                .matcher
                .as_mut()
                .map(|matcher| matcher.edge(key, edge, held))
                .unwrap_or_default();
        };
        let Some(step) = recording.recorder.edge(key, edge, held) else {
            return Action::default();
        };
        let generation = state.generation;
        let mut recording = state.recording.take().expect("a recording is in progress");
        // The key that completed the shortcut is swallowed, as the hotkey's would be.
        let action = match &step {
            Recorded::Done(hotkey) if !hotkey.is_modifier_only() => {
                state.recorded_key = key;
                Action {
                    mask: if hotkey.needs_mask() {
                        Mask::After
                    } else {
                        Mask::None
                    },
                    ..swallow
                }
            }
            _ => Action::default(),
        };
        // The sink may call back into the listener.
        drop(state);
        let finished = !matches!(step, Recorded::Holding(_));
        (recording.sink)(step);
        let mut state = self.lock();
        if finished {
            state.generation += 1;
        } else if state.generation == generation && state.recording.is_none() {
            state.recording = Some(recording);
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::key::{Modifier, Side};

    #[test]
    fn a_recording_takes_the_keys_from_the_hotkey_until_it_is_done() {
        let listener = Listener::new(Options::default());
        listener.set_hotkey(Some("Super+Space".parse().unwrap()));
        let (tx, rx) = mpsc::channel();
        listener.record(move |step| tx.send(step).unwrap());
        let shared = &listener.shared;
        let left_super = Key::Modifier(Modifier::Super, Side::Left);
        let held = Held::default().with(left_super);
        assert_eq!(
            shared.edge(Some(left_super), Edge::Down, Held::default()),
            Action::default(),
            "modifiers pass"
        );
        let done = shared.edge(Some(Key::Space), Edge::Down, held);
        assert_eq!(
            (done.event, done.swallow),
            (None, true),
            "the recorder's key"
        );
        assert!(matches!(rx.try_recv(), Ok(Recorded::Holding(_))));
        assert_eq!(
            rx.try_recv(),
            Ok(Recorded::Done("Super+Space".parse().unwrap()))
        );
        assert!(!listener.is_recording());
        assert!(shared.edge(Some(Key::Space), Edge::Repeat, held).swallow);
        assert!(shared.edge(Some(Key::Space), Edge::Up, held).swallow);
        let press = shared.edge(Some(Key::Space), Edge::Down, held);
        assert_eq!(press.event, Some(Event::Pressed));
    }

    #[test]
    fn a_sink_may_stop_its_own_recording() {
        let listener = Listener::new(Options::default());
        let inner = listener.clone();
        listener.record(move |_| inner.stop_recording());
        let right_ctrl = Key::Modifier(Modifier::Ctrl, Side::Right);
        listener
            .shared
            .edge(Some(right_ctrl), Edge::Down, Held::default());
        assert!(!listener.is_recording());
    }
}
