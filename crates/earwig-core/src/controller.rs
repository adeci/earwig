//! The daemon's heart: one thread that owns the dictation state and talks to
//! the microphone, engine and inserter; everything else holds a [`Handle`].
//!
//! Threads:
//! - `earwig-controller`: the [`Session`] state machine, commands, events,
//!   and the status snapshot. Never blocks on audio, decoding or insertion,
//!   so Release and Cancel are handled at once.
//! - `earwig-engine`: owns the loaded [`Model`]; loads, feeds, finishes.
//! - `earwig-output`: owns the [`Inserter`]; runs the `[hooks]` filter,
//!   inserts, appends history, runs `on_insert`. Between deliveries it
//!   replaces the inserter when the `[insert]` settings change.
//! - `earwig-audio`: the [`Capture`] worker.
//!
//! Every recording gets an id; results for an older (cancelled) recording
//! are dropped.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::audio::{AudioEvent, Capture, Source};
use crate::config::{AudioConfig, BackendSetting, Config, ConfigStore, InsertConfig};
use crate::engine::{Backend, EngineFactory, EngineOptions, Model, ModelInfo, Partial};
use crate::events::{Command, DictationState, Event, InsertMethod, ModelStatus, Status};
use crate::history::{History, HistoryEntry};
use crate::hooks;
use crate::insert::Inserter;
use crate::models::{ModelError, ModelManager, ModelSpec};
use crate::session::{Action, Input, Phase, Session};
use crate::spectrum::Feed;

/// Builds the inserter for the `[insert]` settings. It may block (a consent
/// dialog) and never fails: where no method works it returns a stand-in that
/// fails every insertion.
pub type InserterFactory = dyn Fn(&InsertConfig) -> Box<dyn Inserter> + Send;

pub struct Parts {
    pub config: ConfigStore,
    pub models: Arc<ModelManager>,
    pub history: Arc<History>,
    pub engine: Arc<dyn EngineFactory>,
    /// Called at start, then on the output thread whenever `[insert]`
    /// changes.
    pub inserter: Box<InserterFactory>,
    /// Names of the hotkey sources started for this session.
    pub hotkey_sources: Vec<String>,
}

/// Fan-out of [`Event`]s; subscribers that went away are dropped on the next
/// publish.
#[derive(Debug, Default)]
pub(crate) struct Bus {
    subscribers: Mutex<Vec<flume::Sender<Event>>>,
    closed: AtomicBool,
}

impl Bus {
    pub(crate) fn publish(&self, event: &Event) {
        self.lock().retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// A new subscriber that first receives `first()`. It runs under the
    /// subscriber lock, so no publish can slip between it and registration.
    /// After [`Bus::close`] the receiver disconnects right after it.
    fn subscribe(&self, first: impl FnOnce() -> Event) -> flume::Receiver<Event> {
        let (tx, rx) = flume::unbounded();
        let mut subscribers = self.lock();
        let _ = tx.send(first());
        if !self.closed.load(Ordering::Relaxed) {
            subscribers.push(tx);
        }
        rx
    }

    /// Front ends learn the daemon quit when their subscription disconnects.
    fn close(&self) {
        let mut subscribers = self.lock();
        self.closed.store(true, Ordering::Relaxed);
        subscribers.clear();
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

pub(crate) struct Shared {
    pub commands: flume::Sender<Command>,
    pub bus: Bus,
    /// The live spectrum of the current recording, for the overlay.
    pub spectrum: Feed,
    /// Written before the matching event is published.
    pub status: RwLock<Status>,
    pub config: Arc<RwLock<ConfigStore>>,
    pub models: Arc<ModelManager>,
    pub history: Arc<History>,
    pub engine: Arc<dyn EngineFactory>,
}

impl Handle {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
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
        self.shared.bus.subscribe(|| Event::Status {
            status: self.status(),
        })
    }

    /// The voice of the current recording as it is heard, far more often
    /// than [`Event::Level`].
    pub fn spectrum(&self) -> Feed {
        self.shared.spectrum.clone()
    }

    pub fn status(&self) -> Status {
        let mut status = self
            .shared
            .status
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // The user grants these in System Settings, not through earwig.
        status.permissions = crate::permissions::check();
        status
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

/// Start the controller with audio from `source`.
pub fn spawn_with_source(parts: Parts, source: Source) -> anyhow::Result<Handle> {
    let Parts {
        config,
        models,
        history,
        engine,
        inserter: build_inserter,
        hotkey_sources,
    } = parts;
    let settings = config.config().clone();
    let inserter = build_inserter(&settings.insert);
    let (commands_tx, commands) = flume::unbounded();
    let shared = Arc::new(Shared {
        commands: commands_tx,
        bus: Bus::default(),
        spectrum: Feed::default(),
        status: RwLock::new(Status {
            state: DictationState::Idle,
            model: ModelStatus::Missing {
                model: wanted_model(&models, &settings).spec,
            },
            insert_methods: inserter.methods(),
            hotkey_sources,
            permissions: Vec::new(),
        }),
        config: Arc::new(RwLock::new(config)),
        models,
        history,
        engine: engine.clone(),
    });

    let (audio_tx, audio) = flume::unbounded();
    let capture = Capture::spawn(
        source,
        settings.audio.clone(),
        audio_tx,
        shared.spectrum.clone(),
    )?;
    let (engine_tx, engine_jobs) = flume::unbounded();
    let (engine_events_tx, engine_events) = flume::unbounded();
    let engine_worker = thread::Builder::new()
        .name("earwig-engine".into())
        .spawn(move || engine_thread(engine.as_ref(), &engine_jobs, &engine_events_tx))?;
    let (output_tx, output_jobs) = flume::unbounded();
    let (output_events_tx, output_events) = flume::unbounded();
    let output_shared = shared.clone();
    let output_worker = thread::Builder::new()
        .name("earwig-output".into())
        .spawn(move || {
            output_thread(
                &*build_inserter,
                inserter,
                &output_shared,
                &output_jobs,
                &output_events_tx,
            );
        })?;

    let mut controller = Controller {
        shared: shared.clone(),
        session: Session::new(settings.hotkey.mode, settings.hotkey.hold_threshold_ms),
        audio_config: settings.audio.clone(),
        insert_config: settings.insert,
        capture: Some(capture),
        engine: engine_tx,
        output: output_tx,
        workers: vec![engine_worker, output_worker],
        recording: 0,
        started: Instant::now(),
        started_at_ms: 0,
        stopped: Instant::now(),
        duration_ms: 0,
        output_cancel: None,
        loaded: None,
        load_id: 0,
        reload_when_idle: false,
    };
    controller.ensure_model(true);
    thread::Builder::new()
        .name("earwig-controller".into())
        .spawn(move || controller.run(&commands, &audio, &engine_events, &output_events))?;
    Ok(Handle::new(shared))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelRequest {
    spec: ModelSpec,
    options: EngineOptions,
}

fn wanted_model(models: &ModelManager, config: &Config) -> ModelRequest {
    let spec = config
        .model
        .model
        .clone()
        .unwrap_or_else(|| ModelSpec::Curated {
            id: models.default_model().id.clone(),
        });
    let backend = match config.model.backend {
        BackendSetting::Auto => Backend::Auto,
        BackendSetting::Cpu => Backend::Cpu,
        BackendSetting::Gpu => Backend::Gpu,
    };
    ModelRequest {
        spec,
        options: EngineOptions {
            backend,
            language: config.model.language.clone(),
        },
    }
}

enum EngineJob {
    Unload,
    Load {
        load_id: u64,
        path: PathBuf,
        options: EngineOptions,
    },
    Begin {
        id: u64,
    },
    Feed {
        id: u64,
        pcm: Vec<f32>,
    },
    Finish {
        id: u64,
    },
    Abort {
        id: u64,
    },
}

enum EngineEvent {
    Loaded {
        load_id: u64,
        info: ModelInfo,
    },
    LoadFailed {
        load_id: u64,
        message: String,
    },
    Partial {
        id: u64,
        partial: Partial,
    },
    Finished {
        id: u64,
        result: Result<String, String>,
        took: Duration,
    },
}

fn load_model(
    factory: &dyn EngineFactory,
    load_id: u64,
    path: &std::path::Path,
    options: &EngineOptions,
) -> (Option<Box<dyn Model>>, EngineEvent) {
    let started = Instant::now();
    match factory.load(path, options) {
        Ok(model) => {
            let info = model.info().clone();
            log::info!(
                "loaded {} in {} ms ({}, {}, {})",
                path.display(),
                started.elapsed().as_millis(),
                info.architecture,
                info.backend,
                if info.streaming {
                    "streaming"
                } else {
                    "re-decoding"
                },
            );
            (Some(model), EngineEvent::Loaded { load_id, info })
        }
        Err(e) => {
            log::error!("loading {}: {e:#}", path.display());
            let message = format!("{e:#}");
            (None, EngineEvent::LoadFailed { load_id, message })
        }
    }
}

fn engine_thread(
    factory: &dyn EngineFactory,
    jobs: &flume::Receiver<EngineJob>,
    events: &flume::Sender<EngineEvent>,
) {
    let mut model: Option<Box<dyn Model>> = None;
    // The recording the model is working on, or why it could not begin.
    let mut active: Result<u64, String> = Err("no speech model loaded".into());
    let mut held = None;
    while let Some(job) = held.take().or_else(|| jobs.recv().ok()) {
        match job {
            EngineJob::Load {
                load_id,
                path,
                options,
            } => {
                // Free the old model first; two may not fit in VRAM.
                drop(model.take());
                active = Err("no speech model loaded".into());
                let (loaded, event) = load_model(factory, load_id, &path, &options);
                model = loaded;
                let _ = events.send(event);
            }
            EngineJob::Unload => {
                drop(model.take());
                active = Err("no speech model loaded".into());
            }
            EngineJob::Begin { id } => {
                active = match &mut model {
                    Some(model) => match model.begin() {
                        Ok(()) => Ok(id),
                        Err(e) => Err(format!("{e:#}")),
                    },
                    None => Err("no speech model loaded".into()),
                };
            }
            EngineJob::Feed { id, mut pcm } => {
                // Catch up in one call when decoding fell behind real time.
                while let Ok(next) = jobs.try_recv() {
                    match next {
                        EngineJob::Feed {
                            id: next_id,
                            pcm: more,
                        } if next_id == id => {
                            pcm.extend_from_slice(&more);
                        }
                        other => {
                            held = Some(other);
                            break;
                        }
                    }
                }
                if let (Some(model), Ok(active)) = (&mut model, &active)
                    && *active == id
                {
                    match model.feed(&pcm) {
                        Ok(Some(partial)) => {
                            let _ = events.send(EngineEvent::Partial { id, partial });
                        }
                        Ok(None) => {}
                        Err(e) => log::warn!("live transcription: {e:#}"),
                    }
                }
            }
            EngineJob::Finish { id } => {
                let started = Instant::now();
                let result = match (&mut model, &active) {
                    (Some(model), Ok(active)) if *active == id => {
                        model.finish().map_err(|e| format!("{e:#}"))
                    }
                    (_, Err(e)) => Err(e.clone()),
                    _ => Err("recording was not started".into()),
                };
                active = Err("no recording in progress".into());
                let _ = events.send(EngineEvent::Finished {
                    id,
                    result,
                    took: started.elapsed(),
                });
            }
            EngineJob::Abort { id } => {
                if let (Some(model), Ok(active_id)) = (&mut model, &active)
                    && *active_id == id
                {
                    model.abort();
                    active = Err("no recording in progress".into());
                }
            }
        }
    }
}

enum OutputJob {
    Deliver(Box<Delivery>),
    /// The `[insert]` settings changed: replace the inserter.
    Rebuild(InsertConfig),
}

struct Delivery {
    id: u64,
    text: String,
    duration_ms: u64,
    model: String,
    config: Config,
    cancel: Arc<AtomicBool>,
}

enum OutputEvent {
    /// The text of this recording was dealt with.
    Delivered(u64),
    /// A new inserter is in place, with these methods.
    Rebuilt(Vec<InsertMethod>),
}

fn output_thread(
    build: &InserterFactory,
    mut inserter: Box<dyn Inserter>,
    shared: &Shared,
    jobs: &flume::Receiver<OutputJob>,
    events: &flume::Sender<OutputEvent>,
) {
    for job in jobs {
        match job {
            OutputJob::Deliver(delivery) => {
                deliver(inserter.as_mut(), shared, &delivery);
                let _ = events.send(OutputEvent::Delivered(delivery.id));
            }
            OutputJob::Rebuild(config) => {
                // Only one input method may hold the seat: the old inserter
                // lets go before the new one connects.
                drop(inserter);
                inserter = build(&config);
                let _ = events.send(OutputEvent::Rebuilt(inserter.methods()));
            }
        }
    }
}

/// Filter, insert, record, announce.
fn deliver(inserter: &mut dyn Inserter, shared: &Shared, job: &Delivery) {
    let text = hooks::filter(&job.config.hooks, &job.text);
    if text.trim().is_empty() || job.cancel.load(Ordering::Relaxed) {
        return;
    }
    let result = inserter.insert(&text);
    if job.config.history.enabled {
        let entry = HistoryEntry {
            at_ms: unix_ms(),
            text: text.clone(),
            duration_ms: job.duration_ms,
            model: job.model.clone(),
            method: result.as_ref().ok().copied(),
        };
        if let Err(e) = shared.history.append(entry) {
            log::warn!("history: {e}");
        }
    }
    let event = match result {
        Ok(method) => {
            log::info!(
                "inserted {} characters via {method:?}",
                text.chars().count()
            );
            hooks::on_insert(&job.config.hooks, &text);
            Event::Inserted { text, method }
        }
        Err(e) => {
            let reason = e.to_string();
            log::warn!("insertion failed: {reason}");
            let on_clipboard = match inserter.copy_to_clipboard(&text) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("copying to the clipboard failed too: {e}");
                    false
                }
            };
            Event::InsertFailed {
                text,
                reason,
                on_clipboard,
            }
        }
    };
    shared.bus.publish(&event);
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

struct Controller {
    shared: Arc<Shared>,
    session: Session,
    audio_config: AudioConfig,
    /// The `[insert]` settings the output thread was last told to build for.
    insert_config: InsertConfig,
    capture: Option<Capture>,
    engine: flume::Sender<EngineJob>,
    output: flume::Sender<OutputJob>,
    workers: Vec<thread::JoinHandle<()>>,
    recording: u64,
    started: Instant,
    started_at_ms: u64,
    /// When the newest recording stopped, for latency logs.
    stopped: Instant,
    duration_ms: u64,
    /// Set to drop the result being inserted.
    output_cancel: Option<Arc<AtomicBool>>,
    /// The model last sent to the engine, with its load id.
    loaded: Option<ModelRequest>,
    load_id: u64,
    /// A model change waits until the current dictation is done.
    reload_when_idle: bool,
}

enum Wake {
    Command(Result<Command, flume::RecvError>),
    Audio(Result<AudioEvent, flume::RecvError>),
    Engine(Result<EngineEvent, flume::RecvError>),
    Output(Result<OutputEvent, flume::RecvError>),
}

impl Controller {
    fn run(
        mut self,
        commands: &flume::Receiver<Command>,
        audio: &flume::Receiver<AudioEvent>,
        engine: &flume::Receiver<EngineEvent>,
        output: &flume::Receiver<OutputEvent>,
    ) {
        loop {
            let wake = flume::Selector::new()
                .recv(commands, Wake::Command)
                .recv(audio, Wake::Audio)
                .recv(engine, Wake::Engine)
                .recv(output, Wake::Output)
                .wait();
            match wake {
                Wake::Command(Ok(Command::Quit) | Err(_)) => break,
                Wake::Command(Ok(cmd)) => self.command(cmd),
                Wake::Audio(Ok(event)) => self.audio(event),
                Wake::Engine(Ok(event)) => self.engine_event(event),
                Wake::Output(Ok(OutputEvent::Delivered(id))) => self.delivered(id),
                Wake::Output(Ok(OutputEvent::Rebuilt(methods))) => self.set_insert_methods(methods),
                Wake::Audio(Err(_)) | Wake::Engine(Err(_)) | Wake::Output(Err(_)) => {
                    log::error!("a worker thread died; stopping");
                    break;
                }
            }
        }
        log::info!("quitting");
        let Self {
            shared,
            capture,
            engine,
            output,
            workers,
            ..
        } = self;
        drop(capture);
        drop((engine, output));
        // The model must be freed before the process exits: GPU backends
        // crash in their static destructors while a model is still alive.
        for worker in workers {
            let _ = worker.join();
        }
        shared.bus.close();
    }

    fn publish(&self, event: &Event) {
        self.shared.bus.publish(event);
    }

    fn config(&self) -> Config {
        self.shared
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .config()
            .clone()
    }

    fn update_status(&self, edit: impl FnOnce(&mut Status)) {
        edit(
            &mut self
                .shared
                .status
                .write()
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    fn set_state(&self, state: DictationState) {
        self.update_status(|s| s.state = state);
        self.publish(&Event::State { state });
    }

    fn set_model(&self, model: ModelStatus) {
        self.update_status(|s| s.model = model.clone());
        self.publish(&Event::Model { model });
    }

    /// The insertion methods have no event of their own: front ends get a
    /// fresh snapshot.
    fn set_insert_methods(&self, methods: Vec<InsertMethod>) {
        self.update_status(|s| s.insert_methods = methods);
        let status = self
            .shared
            .status
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.publish(&Event::Status { status });
    }

    fn model_status(&self) -> ModelStatus {
        self.shared
            .status
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .model
            .clone()
    }

    fn error(&self, message: String) {
        self.publish(&Event::Error { message });
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Press => self.input(Input::Press),
            Command::Release => self.input(Input::Release),
            Command::Toggle => self.input(Input::Toggle),
            Command::Cancel => self.input(Input::Cancel),
            Command::Chord => self.input(Input::Chord),
            Command::ShowSettings => self.publish(&Event::ShowSettings),
            Command::ReloadConfig => self.reload_config(),
            Command::UseModel { model } => self.use_model(&model),
            // Answered by the socket server; nothing to do here.
            Command::Status | Command::Subscribe | Command::Quit => {}
        }
    }

    fn input(&mut self, input: Input) {
        match self.session.handle(input, Instant::now()) {
            Some(Action::Start { latched }) => self.start(latched),
            Some(Action::Latch) => self.set_state(DictationState::Recording {
                latched: true,
                started_at_ms: self.started_at_ms,
            }),
            Some(Action::Finish) => self.finish(),
            Some(Action::Discard) => self.discard(),
            None => {}
        }
    }

    fn start(&mut self, latched: bool) {
        if matches!(self.model_status(), ModelStatus::Missing { .. }) {
            // It may have been downloaded since.
            self.ensure_model(true);
        }
        match self.model_status() {
            ModelStatus::Missing { model } => {
                self.session.reset();
                self.error(format!("speech model {model} is not downloaded"));
                return;
            }
            ModelStatus::Failed { model, message } => {
                self.session.reset();
                self.error(format!("speech model {model} failed to load: {message}"));
                return;
            }
            ModelStatus::Loading { .. } | ModelStatus::Ready { .. } => {}
        }
        self.recording += 1;
        let id = self.recording;
        let _ = self.engine.send(EngineJob::Begin { id });
        if let Some(capture) = &self.capture {
            capture.start(id);
        }
        self.started = Instant::now();
        self.started_at_ms = unix_ms();
        log::debug!("recording {id} started");
        self.set_state(DictationState::Recording {
            latched,
            started_at_ms: self.started_at_ms,
        });
    }

    fn finish(&mut self) {
        self.stopped = Instant::now();
        self.duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(capture) = &self.capture {
            capture.stop(self.recording);
        }
        self.set_state(DictationState::Finalizing);
    }

    fn discard(&mut self) {
        let id = self.recording;
        if let Some(capture) = &self.capture {
            capture.stop(id);
        }
        let _ = self.engine.send(EngineJob::Abort { id });
        if let Some(cancel) = self.output_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        log::debug!("recording {id} discarded");
        self.idle();
    }

    fn idle(&mut self) {
        self.session.finished();
        self.set_state(DictationState::Idle);
        if std::mem::take(&mut self.reload_when_idle) {
            self.ensure_model(false);
        }
    }

    fn is_current(&self, id: u64) -> bool {
        id == self.recording && self.session.phase() != Phase::Idle
    }

    fn audio(&mut self, event: AudioEvent) {
        match event {
            AudioEvent::Samples { recording, pcm } if self.is_current(recording) => {
                let _ = self.engine.send(EngineJob::Feed { id: recording, pcm });
            }
            AudioEvent::Level { recording, level }
                if self.is_current(recording)
                    && matches!(self.session.phase(), Phase::Recording { .. }) =>
            {
                self.publish(&Event::Level { level });
            }
            AudioEvent::Stopped { recording }
                if self.is_current(recording) && self.session.phase() == Phase::Finalizing =>
            {
                let _ = self.engine.send(EngineJob::Finish { id: recording });
            }
            AudioEvent::Error(message) => self.error(message),
            _ => {}
        }
    }

    fn engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Loaded { load_id, info } if load_id == self.load_id => {
                if let Some(request) = &self.loaded {
                    self.set_model(ModelStatus::Ready {
                        model: request.spec.clone(),
                        architecture: info.architecture,
                        streaming: info.streaming,
                        backend: info.backend,
                    });
                }
            }
            EngineEvent::LoadFailed { load_id, message } if load_id == self.load_id => {
                if let Some(request) = &self.loaded {
                    self.set_model(ModelStatus::Failed {
                        model: request.spec.clone(),
                        message,
                    });
                }
            }
            EngineEvent::Partial { id, partial } if self.is_current(id) => {
                self.publish(&Event::Transcript {
                    committed: partial.committed,
                    tentative: partial.tentative,
                });
            }
            EngineEvent::Finished { id, result, took } if self.is_current(id) => {
                self.finished(id, result, took);
            }
            _ => {}
        }
    }

    fn finished(&mut self, id: u64, result: Result<String, String>, took: Duration) {
        let text = match result {
            Ok(text) => text.trim().to_owned(),
            Err(message) => {
                log::warn!("recording {id}: transcription failed: {message}");
                self.error(format!("transcription failed: {message}"));
                self.idle();
                return;
            }
        };
        log::info!(
            "recording {id}: {} ms of audio, {} words, final text {} ms after release \
             ({} ms to finish decoding)",
            self.duration_ms,
            text.split_whitespace().count(),
            self.stopped.elapsed().as_millis(),
            took.as_millis(),
        );
        log::debug!("recording {id}: {text:?}");
        self.publish(&Event::Transcript {
            committed: text.clone(),
            tentative: String::new(),
        });
        if text.is_empty() {
            self.idle();
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.output_cancel = Some(cancel.clone());
        let model = match self.model_status() {
            ModelStatus::Missing { model }
            | ModelStatus::Loading { model }
            | ModelStatus::Ready { model, .. }
            | ModelStatus::Failed { model, .. } => model.to_string(),
        };
        let _ = self.output.send(OutputJob::Deliver(Box::new(Delivery {
            id,
            text,
            duration_ms: self.duration_ms,
            model,
            config: self.config(),
            cancel,
        })));
    }

    fn delivered(&mut self, id: u64) {
        if self.is_current(id) && self.session.phase() == Phase::Finalizing {
            log::info!(
                "recording {id}: done {} ms after release",
                self.stopped.elapsed().as_millis()
            );
            self.output_cancel = None;
            self.idle();
        }
    }

    /// Load the configured model unless it is already loaded or loading.
    /// `retry` also retries a model that is missing or failed.
    fn ensure_model(&mut self, retry: bool) {
        let request = wanted_model(&self.shared.models, &self.config());
        let settled = !retry
            || matches!(
                self.model_status(),
                ModelStatus::Loading { .. } | ModelStatus::Ready { .. }
            );
        if self.loaded.as_ref() == Some(&request) && settled {
            return;
        }
        if self.session.phase() != Phase::Idle {
            self.reload_when_idle = true;
            return;
        }
        match self.shared.models.resolve(&request.spec) {
            Ok(path) => {
                self.load_id += 1;
                let _ = self.engine.send(EngineJob::Load {
                    load_id: self.load_id,
                    path,
                    options: request.options.clone(),
                });
                self.set_model(ModelStatus::Loading {
                    model: request.spec.clone(),
                });
                self.loaded = Some(request);
            }
            Err(e) => {
                if self.loaded.take().is_some() {
                    // Don't keep the old model in (GPU) memory.
                    let _ = self.engine.send(EngineJob::Unload);
                }
                self.set_model(match e {
                    ModelError::NotInstalled(model) => ModelStatus::Missing { model },
                    e => ModelStatus::Failed {
                        model: request.spec,
                        message: e.to_string(),
                    },
                });
            }
        }
    }

    fn reload_config(&mut self) {
        let paths = self
            .shared
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .paths()
            .clone();
        match ConfigStore::load_from(paths) {
            Ok(store) => {
                if let Some(e) = store.user_error() {
                    self.error(format!("config: {e}"));
                }
                *self
                    .shared
                    .config
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = store;
                self.apply_config();
            }
            Err(e) => self.error(format!("config: {e:#}")),
        }
    }

    fn apply_config(&mut self) {
        let config = self.config();
        self.session
            .configure(config.hotkey.mode, config.hotkey.hold_threshold_ms);
        if let Err(e) = crate::hotkey::configure(&config.hotkey) {
            self.error(format!("hotkey: {e}"));
        }
        if config.audio != self.audio_config {
            if let Some(capture) = &self.capture {
                capture.configure(config.audio.clone());
            }
            self.audio_config = config.audio.clone();
        }
        if config.insert != self.insert_config {
            // Built on the output thread between deliveries: connecting can
            // wait on a consent dialog.
            self.insert_config = config.insert.clone();
            let _ = self.output.send(OutputJob::Rebuild(config.insert));
        }
        if let Err(e) = self.shared.history.set_limit(config.history.limit) {
            log::warn!("history: {e}");
        }
        self.ensure_model(true);
    }

    fn use_model(&mut self, model: &ModelSpec) {
        let setting = self.shared.models.setting_for(model.clone());
        let updated = self
            .shared
            .config
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .update(|c| c.model.model = setting);
        if let Err(e) = updated {
            self.error(format!("saving the model choice: {e:#}"));
            return;
        }
        // The catalog's default is chosen by setting none, and a system file
        // may pin the same model: what counts is the model that results.
        if wanted_model(&self.shared.models, &self.config()).spec != *model {
            self.error("the model is set by the system configuration".into());
        }
        self.apply_config();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::config::{ConfigPaths, InsertSetting};
    use crate::insert::InsertError;

    const TEXT: &str = "hello from the fake engine";

    /// Counts the models alive.
    struct FakeEngine(Arc<AtomicUsize>);

    impl EngineFactory for FakeEngine {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn supports_architecture(&self, arch: &str) -> bool {
            arch == "fake"
        }
        fn load(&self, _path: &Path, _options: &EngineOptions) -> anyhow::Result<Box<dyn Model>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeModel {
                info: ModelInfo {
                    architecture: "fake".into(),
                    streaming: true,
                    backend: "cpu".into(),
                },
                samples: 0,
                live: self.0.clone(),
            }))
        }
    }

    struct FakeModel {
        info: ModelInfo,
        samples: usize,
        live: Arc<AtomicUsize>,
    }

    impl Drop for FakeModel {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Model for FakeModel {
        fn info(&self) -> &ModelInfo {
            &self.info
        }
        fn begin(&mut self) -> anyhow::Result<()> {
            self.samples = 0;
            Ok(())
        }
        fn feed(&mut self, pcm: &[f32]) -> anyhow::Result<Option<Partial>> {
            self.samples += pcm.len();
            Ok(Some(Partial {
                committed: String::new(),
                tentative: "hello".into(),
            }))
        }
        fn finish(&mut self) -> anyhow::Result<String> {
            // Text only when audio arrived, so insertion proves it flowed.
            Ok(if self.samples > 0 {
                TEXT.into()
            } else {
                String::new()
            })
        }
        fn abort(&mut self) {}
    }

    #[derive(Default)]
    struct Calls {
        /// The `[insert] method` of every inserter built.
        built: Vec<InsertSetting>,
        inserted: Vec<String>,
        clipboard: Vec<String>,
    }

    struct FakeInserter {
        method: InsertMethod,
        fail: bool,
        calls: Arc<Mutex<Calls>>,
    }

    impl Inserter for FakeInserter {
        fn methods(&self) -> Vec<InsertMethod> {
            vec![self.method]
        }
        fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
            if self.fail {
                return Err(InsertError::Failed(anyhow::anyhow!(
                    "no focused text field"
                )));
            }
            self.calls.lock().unwrap().inserted.push(text.into());
            Ok(self.method)
        }
        fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
            self.calls.lock().unwrap().clipboard.push(text.into());
            Ok(())
        }
    }

    struct Daemon {
        handle: Handle,
        events: flume::Receiver<Event>,
        calls: Arc<Mutex<Calls>>,
        live_models: Arc<AtomicUsize>,
        _dir: tempfile::TempDir,
    }

    impl Daemon {
        fn start(fail_insert: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let model = dir.path().join("fake.gguf");
            fs::write(&model, b"").unwrap();
            let system = dir.path().join("system.toml");
            fs::write(
                &system,
                format!(
                    "[hotkey]\nhold_threshold_ms = 150\n[model]\nmodel = '{}'\n",
                    model.display()
                ),
            )
            .unwrap();
            Self::spawn(dir, vec![system], fail_insert)
        }

        /// Start with `user.toml` in `dir` and the given system files, and
        /// wait until the model is ready.
        fn spawn(dir: tempfile::TempDir, system: Vec<PathBuf>, fail_insert: bool) -> Self {
            let config = ConfigStore::load_from(ConfigPaths {
                user: dir.path().join("user.toml"),
                system,
            })
            .unwrap();
            let calls = Arc::new(Mutex::new(Calls::default()));
            let live_models = Arc::new(AtomicUsize::new(0));
            let inserter_calls = calls.clone();
            let parts = Parts {
                config,
                models: Arc::new(ModelManager::new(dir.path().join("models"))),
                history: Arc::new(History::open(dir.path().join("history.jsonl"), 50).unwrap()),
                engine: Arc::new(FakeEngine(live_models.clone())),
                inserter: Box::new(move |config: &InsertConfig| -> Box<dyn Inserter> {
                    inserter_calls.lock().unwrap().built.push(config.method);
                    Box::new(FakeInserter {
                        method: match config.method {
                            InsertSetting::Auto | InsertSetting::InputMethod => {
                                InsertMethod::InputMethod
                            }
                            InsertSetting::Paste => InsertMethod::Paste,
                            InsertSetting::Portal => InsertMethod::PortalPaste,
                        },
                        fail: fail_insert,
                        calls: inserter_calls.clone(),
                    })
                }),
                hotkey_sources: Vec::new(),
            };
            let clip: Arc<[f32]> = vec![0.25; 1_600].into();
            let handle = spawn_with_source(parts, Source::Clip(clip)).unwrap();
            let events = handle.subscribe();
            let daemon = Self {
                handle,
                events,
                calls,
                live_models,
                _dir: dir,
            };
            daemon.wait_for(|e| matches!(e, Event::Model { model: ModelStatus::Ready { .. } }) || matches!(e, Event::Status { status } if matches!(status.model, ModelStatus::Ready { .. })));
            daemon
        }

        fn wait_for(&self, want: impl Fn(&Event) -> bool) -> Event {
            loop {
                let event = self
                    .events
                    .recv_timeout(Duration::from_secs(5))
                    .expect("event did not arrive");
                if want(&event) {
                    return event;
                }
            }
        }

        fn state(&self, want: DictationState) {
            self.wait_for(|e| *e == Event::State { state: want });
        }

        fn recording(&self, latched: bool) {
            self.wait_for(|e| {
                matches!(e, Event::State { state: DictationState::Recording { latched: l, .. } } if *l == latched)
            });
        }

        /// Events over the next `ms`, for asserting something did not happen.
        fn quiet(&self, ms: u64) -> Vec<Event> {
            let deadline = Instant::now() + Duration::from_millis(ms);
            let mut seen = Vec::new();
            while let Ok(event) = self.events.recv_deadline(deadline) {
                seen.push(event);
            }
            seen
        }
    }

    #[test]
    fn hold_inserts_the_engine_text() {
        let d = Daemon::start(false);
        d.handle.send(Command::Press);
        d.recording(false);
        d.wait_for(|e| matches!(e, Event::Level { .. }));
        d.wait_for(|e| matches!(e, Event::Transcript { tentative, .. } if tentative == "hello"));
        thread::sleep(Duration::from_millis(200));
        d.handle.send(Command::Release);
        d.state(DictationState::Finalizing);
        let inserted = d.wait_for(|e| matches!(e, Event::Inserted { .. }));
        assert_eq!(
            inserted,
            Event::Inserted {
                text: TEXT.into(),
                method: InsertMethod::InputMethod
            }
        );
        d.state(DictationState::Idle);
        assert_eq!(d.calls.lock().unwrap().inserted, [TEXT]);
        let history = d.handle.history().entries();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].method, Some(InsertMethod::InputMethod));
        assert_eq!(d.handle.status().state, DictationState::Idle);
    }

    #[test]
    fn tap_latches_until_the_next_press() {
        let d = Daemon::start(false);
        d.handle.send(Command::Press);
        d.handle.send(Command::Release);
        d.recording(true);
        thread::sleep(Duration::from_millis(100));
        d.handle.send(Command::Press);
        d.state(DictationState::Finalizing);
        d.wait_for(|e| matches!(e, Event::Inserted { .. }));
        d.state(DictationState::Idle);
        d.handle.send(Command::Release);
        let after = d.quiet(300);
        assert!(
            !after.iter().any(|e| matches!(e, Event::State { .. })),
            "{after:?}"
        );
    }

    #[test]
    fn cancel_inserts_nothing() {
        let d = Daemon::start(false);
        d.handle.send(Command::Press);
        d.recording(false);
        d.handle.send(Command::Cancel);
        d.state(DictationState::Idle);
        d.handle.send(Command::Release);
        let after = d.quiet(300);
        assert!(
            !after
                .iter()
                .any(|e| matches!(e, Event::Inserted { .. } | Event::InsertFailed { .. })),
            "{after:?}"
        );
        assert_eq!(d.calls.lock().unwrap().inserted, Vec::<String>::new());
        assert_eq!(d.handle.history().entries(), Vec::new());
    }

    #[test]
    fn failed_insert_lands_on_clipboard_and_in_history() {
        let d = Daemon::start(true);
        d.handle.send(Command::Toggle);
        d.recording(true);
        thread::sleep(Duration::from_millis(100));
        d.handle.send(Command::Toggle);
        let failed = d.wait_for(|e| matches!(e, Event::InsertFailed { .. }));
        let Event::InsertFailed {
            text,
            on_clipboard,
            reason,
        } = failed
        else {
            unreachable!()
        };
        assert_eq!(text, TEXT);
        assert!(on_clipboard);
        assert!(reason.contains("no focused text field"), "{reason}");
        assert_eq!(d.calls.lock().unwrap().clipboard, [TEXT]);
        let history = d.handle.history().entries();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].text, TEXT);
        assert_eq!(history[0].method, None);
    }

    #[test]
    fn quit_disconnects_subscribers() {
        let d = Daemon::start(false);
        d.handle.send(Command::ShowSettings);
        d.wait_for(|e| *e == Event::ShowSettings);
        d.handle.send(Command::Quit);
        let deadline = Instant::now() + Duration::from_secs(5);
        while d.events.recv_deadline(deadline).is_ok() {}
        assert!(Instant::now() < deadline, "subscription stayed open");
        let late = d.handle.subscribe();
        assert!(matches!(late.recv(), Ok(Event::Status { .. })));
        assert!(late.recv().is_err());
    }

    #[test]
    fn switching_to_a_missing_model_unloads_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("fake.gguf");
        fs::write(&model, b"").unwrap();
        let user = format!("[model]\nmodel = '{}'\n", model.display());
        fs::write(dir.path().join("user.toml"), user).unwrap();
        let gone = dir.path().join("gone.gguf");
        let d = Daemon::spawn(dir, Vec::new(), false);
        assert_eq!(d.live_models.load(Ordering::SeqCst), 1);
        d.handle.send(Command::UseModel {
            model: ModelSpec::Path { path: gone },
        });
        d.wait_for(|e| {
            matches!(
                e,
                Event::Model {
                    model: ModelStatus::Missing { .. } | ModelStatus::Failed { .. }
                }
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while d.live_models.load(Ordering::SeqCst) > 0 {
            assert!(Instant::now() < deadline, "the old model is still loaded");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn default_model(d: &Daemon) -> ModelSpec {
        ModelSpec::Curated {
            id: d.handle.models().default_model().id.clone(),
        }
    }

    #[test]
    fn using_the_default_model_leaves_the_choice_to_the_release() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("fake.gguf");
        fs::write(&model, b"").unwrap();
        let user = format!("[model]\nmodel = '{}'\n", model.display());
        fs::write(dir.path().join("user.toml"), user).unwrap();
        let d = Daemon::spawn(dir, Vec::new(), false);
        d.handle.send(Command::UseModel {
            model: default_model(&d),
        });
        // It switches without a word about the system configuration.
        let event = d.wait_for(|e| matches!(e, Event::Model { .. } | Event::Error { .. }));
        assert!(matches!(event, Event::Model { .. }), "{event:?}");
        assert_eq!(d.handle.config().read().unwrap().config().model.model, None);
    }

    #[test]
    fn a_model_the_system_pins_stays() {
        let d = Daemon::start(false);
        d.handle.send(Command::UseModel {
            model: default_model(&d),
        });
        let event = d.wait_for(|e| matches!(e, Event::Error { .. }));
        assert!(
            matches!(&event, Event::Error { message } if message.contains("system configuration")),
            "{event:?}"
        );
    }

    #[test]
    fn only_a_changed_insert_method_replaces_the_inserter() {
        let d = Daemon::start(false);
        let edit = |change: fn(&mut Config)| {
            d.handle.config().write().unwrap().update(change).unwrap();
            d.handle.send(Command::ReloadConfig);
        };
        edit(|c| c.insert.method = InsertSetting::Paste);
        d.wait_for(|e| {
            matches!(e, Event::Status { status } if status.insert_methods == [InsertMethod::Paste])
        });
        assert_eq!(d.handle.status().insert_methods, [InsertMethod::Paste]);

        // Other settings leave it alone. The output thread takes jobs in
        // order, so a rebuild would come before this dictation's insertion.
        edit(|c| c.history.limit = 20);
        d.handle.send(Command::Toggle);
        d.recording(true);
        thread::sleep(Duration::from_millis(100));
        d.handle.send(Command::Toggle);
        let inserted = d.wait_for(|e| matches!(e, Event::Inserted { .. }));
        assert_eq!(
            inserted,
            Event::Inserted {
                text: TEXT.into(),
                method: InsertMethod::Paste
            }
        );
        assert_eq!(
            d.calls.lock().unwrap().built,
            [InsertSetting::Auto, InsertSetting::Paste]
        );
    }
}
