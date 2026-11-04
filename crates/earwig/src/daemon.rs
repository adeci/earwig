//! Daemon assembly and the thin socket clients.
//!
//! `EARWIG_AUDIO_FILE=<wav>` replaces the microphone with that recording:
//! every dictation hears it from the start, in real time, then silence. The
//! VM tests use it to drive real transcriptions without a sound card.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use anyhow::Context as _;
use earwig_core::audio::{self, Source};
use earwig_core::config::{BackendSetting, ConfigStore};
use earwig_core::controller::{self, Handle, Parts};
use earwig_core::engine::{Backend, EngineFactory, EngineOptions};
use earwig_core::events::{Command, Event, InsertMethod};
use earwig_core::history::History;
use earwig_core::insert::{self, InsertError, Inserter};
use earwig_core::models::{ModelManager, ModelSpec};
use earwig_core::{SAMPLE_RATE, desktop, hotkey, ipc, paths};
use earwig_transcribe::TranscribeEngine;

pub fn run(headless: bool) -> anyhow::Result<()> {
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
    log::debug!("desktop: {caps:?}");
    let inserter = insert::auto(&settings.insert, &caps).unwrap_or_else(|e| {
        log::warn!("no way to insert text in this session ({e:#}); dictations go to history");
        Box::new(Unavailable(format!("{e:#}")))
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
            caps: caps.clone(),
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

    let result = if headless {
        wait_for_quit(&handle);
        Ok(())
    } else {
        crate::ui::run(handle.clone(), caps)
    };
    handle.send(Command::Quit);
    let _ = fs::remove_file(&socket);
    result
}

/// Block until the controller exits (it closes every subscription).
fn wait_for_quit(handle: &Handle) {
    let events = handle.subscribe();
    while events.recv().is_ok() {}
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

pub fn devices() -> anyhow::Result<()> {
    let default = audio::default_input_device();
    for name in audio::input_devices()? {
        let mark = if default.as_ref() == Some(&name) {
            "  (default)"
        } else {
            ""
        };
        println!("{name}{mark}");
    }
    Ok(())
}
