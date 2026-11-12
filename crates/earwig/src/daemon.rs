//! Daemon assembly and the thin socket clients.
//!
//! `EARWIG_AUDIO_FILE=<wav>` replaces the microphone with that recording:
//! every dictation hears it from the start, in real time, then silence. The
//! VM tests use it to drive real transcriptions without a sound card.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use anyhow::Context as _;
use earwig_core::audio::{self, Source};
use earwig_core::config::{BackendSetting, ConfigStore, InsertConfig};
use earwig_core::controller::{self, Parts};
use earwig_core::engine::{Backend, EngineFactory, EngineOptions};
use earwig_core::events::{Command, Event, InsertMethod};
use earwig_core::history::History;
use earwig_core::insert::{self, InsertError, Inserter};
use earwig_core::models::{ModelManager, ModelSpec};
use earwig_core::{SAMPLE_RATE, desktop, hotkey, ipc, paths, permissions};
use earwig_transcribe::TranscribeEngine;

/// Open the settings window of the running daemon, or start the daemon with
/// it open (desktop entry, first launch without the user service).
pub fn settings() -> anyhow::Result<()> {
    if ipc::request(&Command::ShowSettings).is_ok() {
        return Ok(());
    }
    run(false, true)
}

pub fn run(headless: bool, open_settings: bool) -> anyhow::Result<()> {
    // Bind first so a second instance stops before touching mic or GPU.
    let server = ipc::Server::bind(&paths::socket())?;
    let socket = server.path().to_owned();
    let config = ConfigStore::load()?;
    if let Some(e) = config.user_error() {
        log::error!("settings file ignored, using defaults: {e}");
    }
    let settings = config.config().clone();
    let models = Arc::new(ModelManager::new(paths::models_dir()));
    let history = Arc::new(
        History::open(paths::history_file(), settings.history.limit)
            .with_context(|| format!("opening {}", paths::history_file().display()))?,
    );
    let engine: Arc<dyn EngineFactory> = Arc::new(TranscribeEngine::new());
    let caps = desktop::probe();
    let insert_caps = caps.clone();
    let inserter = Box::new(move |config: &InsertConfig| -> Box<dyn Inserter> {
        insert::auto(config, &insert_caps).unwrap_or_else(|e| {
            log::warn!("no way to insert text in this session ({e:#}); dictations go to history");
            Box::new(Unavailable(format!("{e:#}")))
        })
    });
    let sources = hotkey::sources(&settings.hotkey, &caps);
    let mut hotkey_sources = vec!["socket".to_owned()];
    hotkey_sources.extend(sources.iter().map(|s| s.name().to_owned()));
    let source = match std::env::var_os("EARWIG_AUDIO_FILE") {
        Some(path) => {
            let path = PathBuf::from(path);
            log::warn!(
                "EARWIG_AUDIO_FILE: dictations hear {} instead of the microphone",
                path.display()
            );
            Source::Clip(audio::read_wav(&path)?.into())
        }
        None => Source::Microphone,
    };

    let handle = controller::spawn_with_source(
        Parts {
            config,
            models,
            history,
            engine,
            inserter,
            hotkey_sources,
        },
        source,
    )?;
    for source in sources {
        let commands = handle.commands();
        let name = source.name();
        thread::Builder::new()
            .name(format!("earwig-hotkey-{name}"))
            .spawn(move || {
                if let Err(e) = source.run(commands) {
                    log::warn!("hotkey source {name} stopped: {e:#}");
                }
            })?;
    }
    server.serve(handle.clone())?;
    log::info!("listening on {}", socket.display());

    // Privacy permissions (macOS): the system asks for what is missing
    // once; until it is all granted, the settings window says what is.
    let missing = permissions::ask_once(&paths::state_dir());
    for permission in &missing {
        log::warn!("{}", permissions::missing_message(*permission));
    }
    if !headless {
        crate::ui::run(handle.clone(), caps, open_settings || !missing.is_empty());
    }
    // Headless: wait for a Quit command. Either way, return only once the
    // controller has freed the model; GPU backends crash at process exit
    // while a model is alive.
    let events = handle.subscribe();
    if !headless {
        handle.send(Command::Quit);
    }
    while events.recv().is_ok() {}
    // A pipe goes away with its last handle; a socket file stays behind.
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// Stand-in when no insertion backend works here: every dictation fails to
/// insert and is kept in history.
struct Unavailable(String);

impl Inserter for Unavailable {
    fn methods(&self) -> Vec<InsertMethod> {
        Vec::new()
    }

    fn insert(&mut self, _text: &str) -> Result<InsertMethod, InsertError> {
        Err(InsertError::Failed(anyhow::anyhow!(
            "no insertion backend: {}",
            self.0
        )))
    }

    fn copy_to_clipboard(&mut self, _text: &str) -> Result<(), InsertError> {
        Err(InsertError::NotApplicable(self.0.clone()))
    }
}

pub fn send(cmd: &Command) -> anyhow::Result<()> {
    ipc::request(cmd)?;
    Ok(())
}

pub fn status() -> anyhow::Result<()> {
    match ipc::request(&Command::Status)? {
        Some(Event::Status { status }) => {
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        other => anyhow::bail!("unexpected reply: {other:?}"),
    }
}

pub fn events() -> anyhow::Result<()> {
    let mut out = io::stdout().lock();
    for event in ipc::subscribe()? {
        let line = serde_json::to_string(&event?)?;
        match writeln!(out, "{line}").and_then(|()| out.flush()) {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            other => other?,
        }
    }
    Ok(())
}

/// Transcribe WAV files with the configured model, fed in 100 ms chunks as
/// live audio would be (but as fast as the model takes them). Text goes to
/// stdout, timings to stderr; "final" is the time from the last sample to
/// the final text, which is what a dictation waits for after release.
pub fn transcribe_files(wavs: &[PathBuf], backend: Option<&str>) -> anyhow::Result<()> {
    let config = ConfigStore::load()?;
    let settings = config.config();
    let models = ModelManager::new(paths::models_dir());
    let spec = settings
        .model
        .model
        .clone()
        .unwrap_or_else(|| ModelSpec::Curated {
            id: models.default_model().id.clone(),
        });
    let path = models.resolve(&spec)?;
    let backend = match backend {
        Some("cpu") => Backend::Cpu,
        Some("gpu") => Backend::Gpu,
        Some(_) => Backend::Auto,
        None => match settings.model.backend {
            BackendSetting::Auto => Backend::Auto,
            BackendSetting::Cpu => Backend::Cpu,
            BackendSetting::Gpu => Backend::Gpu,
        },
    };
    let options = EngineOptions {
        backend,
        language: settings.model.language.clone(),
    };
    let started = Instant::now();
    let mut model = TranscribeEngine::new().load(&path, &options)?;
    let info = model.info().clone();
    eprintln!(
        "{spec}: {} on {}, {}, loaded in {} ms",
        info.architecture,
        info.backend,
        if info.streaming {
            "streaming"
        } else {
            "re-decoding"
        },
        started.elapsed().as_millis()
    );
    for wav in wavs {
        transcribe_one(model.as_mut(), wav)?;
    }
    Ok(())
}

fn transcribe_one(model: &mut dyn earwig_core::engine::Model, wav: &Path) -> anyhow::Result<()> {
    let pcm = audio::read_wav(wav)?;
    let audio_ms = pcm.len() * 1000 / SAMPLE_RATE as usize;
    let started = Instant::now();
    model.begin()?;
    let mut updates = 0;
    for chunk in pcm.chunks(SAMPLE_RATE as usize / 10) {
        if model.feed(chunk)?.is_some() {
            updates += 1;
        }
    }
    let fed = started.elapsed();
    let finishing = Instant::now();
    let text = model.finish()?;
    let finish = finishing.elapsed();
    println!("{text}");
    #[allow(clippy::cast_precision_loss)]
    let rtf = started.elapsed().as_secs_f64() / (audio_ms as f64 / 1000.0);
    eprintln!(
        "{}: {audio_ms} ms of audio; fed in {} ms with {updates} live updates; \
         final text {} ms after the last sample; real-time factor {rtf:.3}",
        wav.display(),
        fed.as_millis(),
        finish.as_millis(),
    );
    Ok(())
}

/// One line per microphone: what `[audio] device` takes, then the name the
/// system shows when that differs.
pub fn devices() -> anyhow::Result<()> {
    let found = audio::input_devices(None)?;
    let width = found
        .devices
        .iter()
        .map(|device| device.id.chars().count())
        .max()
        .unwrap_or(0);
    for device in &found.devices {
        let mark = if found.default.as_ref() == Some(device) {
            "  (default)"
        } else {
            ""
        };
        if device.name == device.id {
            println!("{}{mark}", device.id);
        } else {
            println!("{:width$}  {}{mark}", device.id, device.name);
        }
    }
    Ok(())
}
