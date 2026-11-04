//! The daemon's heart: one thread that owns the dictation state and talks to
//! the microphone, the engine and the inserter. Everything else holds a
//! cloneable [`Handle`].

use std::sync::{Arc, Mutex, PoisonError, RwLock};

use crate::config::ConfigStore;
use crate::desktop::DesktopCaps;
use crate::engine::EngineFactory;
use crate::events::{Command, Event, Status};
use crate::history::History;
use crate::insert::Inserter;
use crate::models::ModelManager;

/// What the controller is built from.
pub struct Parts {
    pub config: ConfigStore,
    pub models: Arc<ModelManager>,
    pub history: Arc<History>,
    pub engine: Arc<dyn EngineFactory>,
    pub inserter: Box<dyn Inserter>,
    pub caps: DesktopCaps,
    /// Names of the hotkey sources started for this session.
    pub hotkey_sources: Vec<String>,
}

/// Fan-out of [`Event`]s to any number of subscribers. Subscribers that went
/// away are dropped on the next publish.
#[derive(Debug, Default)]
pub struct Bus {
    subscribers: Mutex<Vec<flume::Sender<Event>>>,
}

impl Bus {
    pub fn publish(&self, event: &Event) {
        self.lock()
            .retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// A new subscriber that first receives `first`.
    pub fn subscribe(&self, first: Event) -> flume::Receiver<Event> {
        let (tx, rx) = flume::unbounded();
        let _ = tx.send(first);
        self.lock().push(tx);
        rx
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<flume::Sender<Event>>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Cloneable access to a running controller.
#[derive(Clone)]
pub struct Handle {
    shared: Arc<Shared>,
}

/// State the controller thread and its handles share.
pub struct Shared {
    pub commands: flume::Sender<Command>,
    pub bus: Bus,
    /// Latest snapshot, kept current by the controller thread.
    pub status: RwLock<Status>,
    pub config: Arc<RwLock<ConfigStore>>,
    pub models: Arc<ModelManager>,
    pub history: Arc<History>,
    pub engine: Arc<dyn EngineFactory>,
}

impl Handle {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Queue a command. Dropped silently if the controller has exited.
    pub fn send(&self, cmd: Command) {
        let _ = self.shared.commands.send(cmd);
    }

    pub fn commands(&self) -> flume::Sender<Command> {
        self.shared.commands.clone()
    }

    /// Every event from now on, starting with a [`Event::Status`] snapshot.
    pub fn subscribe(&self) -> flume::Receiver<Event> {
        self.shared.bus.subscribe(Event::Status {
            status: self.status(),
        })
    }

    pub fn status(&self) -> Status {
        self.shared
            .status
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Settings. Edit through [`ConfigStore::update`], then send
    /// [`Command::ReloadConfig`].
    pub fn config(&self) -> Arc<RwLock<ConfigStore>> {
        self.shared.config.clone()
    }

    pub fn models(&self) -> Arc<ModelManager> {
        self.shared.models.clone()
    }

    pub fn history(&self) -> Arc<History> {
        self.shared.history.clone()
    }

    pub fn engine(&self) -> Arc<dyn EngineFactory> {
        self.shared.engine.clone()
    }
}

// The controller thread itself (pipeline slice):
//
//   pub fn spawn(parts: Parts) -> anyhow::Result<Handle>
//
// Owns audio capture, the loaded model, the session state machine and the
// inserter; consumes `Shared::commands`; publishes on `Shared::bus` and keeps
// `Shared::status` current.
