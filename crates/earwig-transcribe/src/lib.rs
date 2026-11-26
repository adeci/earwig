//! The speech engine: transcribe.cpp with the CPU and one GPU backend (Metal
//! on macOS, Vulkan elsewhere). Streaming models decode as you speak, falling
//! back to one decode of the whole recording; others re-decode about once a
//! second. Each model's session lives on a worker thread: transcribe.cpp
//! allows one compute at a time and a stream borrows the session.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use earwig_core::engine::{Backend, EngineFactory, EngineOptions, Model, ModelInfo, Partial};
use transcribe_cpp as tc;

/// `general.architecture` values transcribe.cpp 0.4.1 transcribes: its
/// family table without the diarization (`sortformer`), language-ID
/// (`ecapa_tdnn`) and VAD (`silero_vad`) families. Update with the crate.
const ASR_ARCHITECTURES: &[&str] = &[
    "canary",
    "canary_qwen",
    "cohere_asr",
    "funasr_nano",
    "gigaam",
    "granite_speech",
    "granite_speech5_ctc",
    "granite_speech_nar",
    "medasr",
    "moonshine",
    "moonshine_streaming",
    "moss",
    "parakeet",
    "qwen3_asr",
    "sensevoice",
    "voxtral",
    "voxtral_realtime",
    "whisper",
];

/// Live text for non-streaming models is refreshed at most this often.
const PREVIEW_INTERVAL: Duration = Duration::from_secs(1);

/// Peak amplitude below -60 dBFS (matching `earwig_core::spectrum`'s silence
/// floor): muted or digitally silent audio that cannot contain speech.
const SILENCE_PEAK: f32 = 1e-3;

/// Cheap to create: nothing is initialized until a model loads.
#[derive(Debug, Default)]
pub struct TranscribeEngine;

impl TranscribeEngine {
    pub fn new() -> Self {
        Self
    }
}

impl EngineFactory for TranscribeEngine {
    fn supports_architecture(&self, arch: &str) -> bool {
        ASR_ARCHITECTURES.contains(&arch)
    }

    fn load(&self, path: &Path, options: &EngineOptions) -> anyhow::Result<Box<dyn Model>> {
        static ENGINE: LazyLock<Result<(), String>> = LazyLock::new(|| {
            tc::init_logging();
            // The backend modules beside libtranscribe (`dynamic-backends`);
            // nothing to do where they are linked in.
            tc::init_backends_default().map_err(|e| e.to_string())
        });
        check_cpu(&missing_cpu_features())?;
        if let Err(e) = &*ENGINE {
            anyhow::bail!("starting the speech engine: {e}");
        }
        Ok(Box::new(TranscribeModel::load(path, options)?))
    }
}

/// The x86 features the build pinned transcribe.cpp to (`pinned_avx2`, see
/// build.rs) that this CPU lacks; a native build tunes to its own CPU.
fn missing_cpu_features() -> Vec<&'static str> {
    #[cfg(all(pinned_avx2, target_arch = "x86_64"))]
    {
        [
            ("SSE4.2", is_x86_feature_detected!("sse4.2")),
            ("AVX", is_x86_feature_detected!("avx")),
            ("AVX2", is_x86_feature_detected!("avx2")),
            ("BMI2", is_x86_feature_detected!("bmi2")),
            ("FMA", is_x86_feature_detected!("fma")),
            ("F16C", is_x86_feature_detected!("f16c")),
        ]
        .into_iter()
        .filter_map(|(name, present)| (!present).then_some(name))
        .collect()
    }
    #[cfg(not(all(pinned_avx2, target_arch = "x86_64")))]
    {
        Vec::new()
    }
}

/// An error naming each feature in `missing`, if there are any.
fn check_cpu(missing: &[&str]) -> anyhow::Result<()> {
    let names = match missing {
        [] => return Ok(()),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    anyhow::bail!("this CPU lacks {names}, which this build of earwig's speech engine needs")
}

fn backend_request(backend: Backend) -> anyhow::Result<tc::Backend> {
    Ok(match backend {
        Backend::Auto => tc::Backend::Auto,
        Backend::Cpu => tc::Backend::Cpu,
        Backend::Gpu => [
            tc::Backend::Vulkan,
            tc::Backend::Cuda,
            tc::Backend::Rocm,
            tc::Backend::Metal,
        ]
        .into_iter()
        .find(|b| tc::backend_available(*b))
        .context(if cfg!(target_os = "macos") {
            "no GPU backend is available (Metal found no device)"
        } else {
            "no GPU backend is available (is a Vulkan driver installed?)"
        })?,
    })
}

/// The language hint as the model lists it: as given, or for a bare code the
/// model only lists with a region (Nemotron has `en-US`, the catalog says `en`)
/// the first such tag. `None` when the model lists languages but not this one.
fn listed_language(lang: &str, listed: &[String]) -> Option<String> {
    if listed.is_empty() || listed.iter().any(|tag| tag == lang) {
        return Some(lang.to_owned());
    }
    let regional = listed.iter().find(|tag| {
        tag.split_once('-')
            .is_some_and(|(base, _)| base.eq_ignore_ascii_case(lang))
    });
    if regional.is_none() {
        log::warn!(
            "the model does not list language {lang:?} ({}); ignoring the hint",
            listed.join(", ")
        );
    }
    regional.cloned()
}

enum Job {
    Begin {
        generation: u64,
    },
    Feed {
        generation: u64,
        pcm: Vec<f32>,
    },
    /// End the stream, or decode `recording` when there is none (or the
    /// stream came up empty).
    Finish {
        generation: u64,
        recording: Vec<f32>,
    },
    /// Decode a snapshot for live text.
    Preview {
        generation: u64,
        pcm: Vec<f32>,
    },
    Abort,
}

enum Reply {
    Begun {
        generation: u64,
        result: Result<(), String>,
    },
    Fed {
        generation: u64,
        partial: Option<Partial>,
    },
    Finished {
        generation: u64,
        text: Result<String, String>,
    },
    Preview {
        generation: u64,
        text: Result<String, String>,
        took: Duration,
    },
}

impl Reply {
    fn generation(&self) -> u64 {
        match self {
            Self::Begun { generation, .. }
            | Self::Fed { generation, .. }
            | Self::Finished { generation, .. }
            | Self::Preview { generation, .. } => *generation,
        }
    }
}

struct TranscribeModel {
    info: ModelInfo,
    jobs: Option<flume::Sender<Job>>,
    replies: flume::Receiver<Reply>,
    worker: Option<JoinHandle<()>>,
    /// Aborts an in-flight preview decode when the final one is wanted.
    cancel: tc::CancelToken,
    /// Generation currently permitted to start a preview decode (`0` = none).
    allow_preview: Arc<AtomicU64>,
    /// Longest audio the model accepts in one run (0 = no limit), in samples.
    max_samples: usize,
    generation: u64,
    active: bool,
    /// The current recording has a live native stream.
    streaming_now: bool,
    /// Everything fed since `begin`: batch decodes and stream fallbacks.
    recording: Vec<f32>,
    preview_in_flight: bool,
    next_preview: Instant,
}

impl TranscribeModel {
    fn load(path: &Path, options: &EngineOptions) -> anyhow::Result<Self> {
        let backend = backend_request(options.backend)?;
        let load = |backend| {
            tc::Model::load_with(
                path,
                &tc::ModelOptions {
                    backend,
                    ..tc::ModelOptions::default()
                },
            )
        };
        let model = match load(backend) {
            // Automatic: a GPU that fails (driver trouble, too little
            // memory) leaves the CPU, which every build has.
            Err(e) if backend == tc::Backend::Auto => {
                log::warn!("loading on the GPU failed ({e}); using the CPU");
                load(tc::Backend::Cpu)
            }
            loaded => loaded,
        }
        .with_context(|| format!("loading {}", path.display()))?;
        let caps = model
            .capabilities()
            .context("the model does not transcribe speech")?;
        let language = options
            .language
            .as_deref()
            .and_then(|lang| listed_language(lang, &caps.languages));
        let info = ModelInfo {
            architecture: model.arch(),
            streaming: caps.supports_streaming,
            backend: model
                .device()
                .map_or_else(|_| model.backend().to_lowercase(), |d| d.kind),
        };
        let max_samples = usize::try_from(caps.max_audio_ms)
            .unwrap_or(0)
            .saturating_mul(earwig_core::SAMPLE_RATE as usize / 1000);
        let mut session = model.session().context("creating a session")?;
        let run = tc::RunOptions {
            language,
            ..tc::RunOptions::default()
        };
        // A first decode builds the GPU's pipelines, which can take seconds the
        // first time a driver sees them: better now than in the first dictation.
        let warm = Instant::now();
        match session.run(&vec![0.; earwig_core::SAMPLE_RATE as usize], &run) {
            Ok(_) => log::debug!("warm-up decode took {:?}", warm.elapsed()),
            Err(e) => log::debug!("warm-up decode: {e}"),
        }
        let cancel = tc::CancelToken::new();
        session.set_cancel_token(&cancel);
        let allow_preview = Arc::new(AtomicU64::new(0));
        let (jobs, jobs_rx) = flume::unbounded();
        let (replies_tx, replies) = flume::unbounded();
        let worker_cancel = cancel.clone();
        let worker_allow = allow_preview.clone();
        let worker = thread::Builder::new()
            .name("earwig-transcribe".into())
            .spawn(move || {
                worker(
                    session,
                    &run,
                    &worker_cancel,
                    &worker_allow,
                    &jobs_rx,
                    &replies_tx,
                );
            })?;
        Ok(Self {
            info,
            jobs: Some(jobs),
            replies,
            worker: Some(worker),
            cancel,
            allow_preview,
            max_samples,
            generation: 0,
            active: false,
            streaming_now: false,
            recording: Vec::new(),
            preview_in_flight: false,
            next_preview: Instant::now(),
        })
    }

    fn send(&self, job: Job) -> anyhow::Result<()> {
        self.jobs
            .as_ref()
            .and_then(|jobs| jobs.send(job).ok())
            .context("the transcription worker stopped")
    }

    /// The next reply for the current recording that `want` accepts.
    /// Finished previews (including stale ones) are noted on the way.
    fn wait(&mut self, want: impl Fn(&Reply) -> bool) -> anyhow::Result<Reply> {
        loop {
            let reply = self
                .replies
                .recv()
                .ok()
                .context("the transcription worker stopped")?;
            if matches!(reply, Reply::Preview { .. }) {
                self.preview_in_flight = false;
            }
            if reply.generation() != self.generation {
                continue;
            }
            if want(&reply) {
                return Ok(reply);
            }
        }
    }

    /// Collect a finished preview and start the next one when due.
    fn preview(&mut self) -> Option<Partial> {
        let mut partial = None;
        while let Ok(reply) = self.replies.try_recv() {
            if let Reply::Preview {
                generation,
                text,
                took,
            } = reply
            {
                self.preview_in_flight = false;
                if generation == self.generation {
                    // A slow decode pushes the next one out: never queue decodes
                    // faster than they finish.
                    self.next_preview = Instant::now() + PREVIEW_INTERVAL.max(took);
                    match text {
                        Ok(text) => {
                            partial = Some(Partial {
                                committed: String::new(),
                                tentative: text.trim().to_owned(),
                            });
                        }
                        Err(e) => log::debug!("live preview: {e}"),
                    }
                }
            }
        }
        if !self.preview_in_flight
            && Instant::now() >= self.next_preview
            && !self.recording.is_empty()
        {
            let start = if self.max_samples > 0 {
                self.recording.len().saturating_sub(self.max_samples)
            } else {
                0
            };
            let job = Job::Preview {
                generation: self.generation,
                pcm: self.recording[start..].to_vec(),
            };
            self.preview_in_flight = self.send(job).is_ok();
        }
        partial
    }
}

impl Model for TranscribeModel {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn begin(&mut self) -> anyhow::Result<()> {
        self.abort();
        self.generation += 1;
        self.recording.clear();
        self.next_preview = Instant::now() + PREVIEW_INTERVAL;
        // Keep `preview_in_flight` and `cancel` intact: an aborted preview from
        // the prior take may still be running on the worker, which resets
        // `cancel` itself before starting a new decode.
        self.allow_preview.store(self.generation, Ordering::SeqCst);
        self.active = true;
        if self.info.streaming {
            self.send(Job::Begin {
                generation: self.generation,
            })?;
            match self.wait(|r| matches!(r, Reply::Begun { .. }))? {
                Reply::Begun { result: Ok(()), .. } => self.streaming_now = true,
                Reply::Begun { result: Err(e), .. } => {
                    log::warn!("streaming unavailable, decoding at the end instead: {e}");
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    }

    fn feed(&mut self, pcm: &[f32]) -> anyhow::Result<Option<Partial>> {
        // transcribe.cpp rejects empty feeds as invalid.
        if !self.active || pcm.is_empty() {
            return Ok(None);
        }
        self.recording.extend_from_slice(pcm);
        if self.streaming_now {
            self.send(Job::Feed {
                generation: self.generation,
                pcm: pcm.to_vec(),
            })?;
            return match self.wait(|r| matches!(r, Reply::Fed { .. }))? {
                Reply::Fed { partial, .. } => Ok(partial),
                _ => unreachable!(),
            };
        }
        if self.info.streaming {
            // The stream failed to begin; the batch decode at the end covers it.
            return Ok(None);
        }
        Ok(self.preview())
    }

    fn finish(&mut self) -> anyhow::Result<String> {
        anyhow::ensure!(self.active, "no recording in progress");
        self.active = false;
        self.streaming_now = false;
        let recording = std::mem::take(&mut self.recording);
        self.allow_preview.store(0, Ordering::SeqCst);
        if self.preview_in_flight {
            // The worker resets the token before the final decode.
            self.cancel.cancel();
        }
        self.send(Job::Finish {
            generation: self.generation,
            recording,
        })?;
        match self.wait(|r| matches!(r, Reply::Finished { .. }))? {
            Reply::Finished { text, .. } => text
                .map(|t| t.trim().to_owned())
                .map_err(anyhow::Error::msg),
            _ => unreachable!(),
        }
    }

    fn abort(&mut self) {
        if std::mem::take(&mut self.active) {
            self.recording.clear();
            self.allow_preview.store(0, Ordering::SeqCst);
            if self.preview_in_flight {
                self.cancel.cancel();
            }
            if std::mem::take(&mut self.streaming_now) {
                let _ = self.send(Job::Abort);
            }
        }
    }
}

impl Drop for TranscribeModel {
    fn drop(&mut self) {
        self.allow_preview.store(0, Ordering::SeqCst);
        self.cancel.cancel();
        self.jobs.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum StreamEnd {
    Done,
    /// Decode this recording in one run instead.
    Fallback(Vec<f32>),
    /// A job that does not belong to the stream; handle it next.
    Next(Job),
    Closed,
}

fn worker(
    mut session: tc::Session,
    run: &tc::RunOptions,
    cancel: &tc::CancelToken,
    allow_preview: &AtomicU64,
    jobs: &flume::Receiver<Job>,
    replies: &flume::Sender<Reply>,
) {
    let mut next = None;
    while let Some(job) = next.take().or_else(|| jobs.recv().ok()) {
        match job {
            Job::Begin { generation } => {
                let end = match session.stream(run, &tc::StreamOptions::default()) {
                    Ok(mut stream) => {
                        let _ = replies.send(Reply::Begun {
                            generation,
                            result: Ok(()),
                        });
                        serve_stream(&mut stream, generation, jobs, replies)
                    }
                    Err(e) => {
                        let _ = replies.send(Reply::Begun {
                            generation,
                            result: Err(e.to_string()),
                        });
                        StreamEnd::Done
                    }
                };
                match end {
                    StreamEnd::Done => {}
                    StreamEnd::Fallback(recording) => {
                        cancel.reset();
                        let text = decode(&mut session, run, &recording);
                        let _ = replies.send(Reply::Finished { generation, text });
                    }
                    StreamEnd::Next(job) => next = Some(job),
                    StreamEnd::Closed => return,
                }
            }
            Job::Finish {
                generation,
                recording,
            } => {
                cancel.reset();
                let text = decode(&mut session, run, &recording);
                let _ = replies.send(Reply::Finished { generation, text });
            }
            Job::Preview { generation, pcm } => {
                let started = Instant::now();
                let text = run_preview(generation, allow_preview, cancel, || {
                    decode(&mut session, run, &pcm)
                });
                let _ = replies.send(Reply::Preview {
                    generation,
                    text,
                    took: started.elapsed(),
                });
            }
            Job::Feed { generation, .. } => {
                let _ = replies.send(Reply::Fed {
                    generation,
                    partial: None,
                });
            }
            Job::Abort => {}
        }
    }
}

/// Run a preview decode only while `generation` is still allowed, resetting
/// `cancel` on the worker thread with a SeqCst check so `abort()`/`finish()`
/// never race `reset()`.
fn run_preview(
    generation: u64,
    allow_preview: &AtomicU64,
    cancel: &tc::CancelToken,
    decode: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    if allow_preview.load(Ordering::SeqCst) != generation {
        return Err("preview cancelled".into());
    }
    cancel.reset();
    if allow_preview.load(Ordering::SeqCst) != generation {
        return Err("preview cancelled".into());
    }
    decode()
}

fn is_silent(pcm: &[f32]) -> bool {
    pcm.iter().all(|s| s.abs() < SILENCE_PEAK)
}

/// Whether an empty streaming finalize warrants a full-recording batch fallback.
fn should_fallback_decode(stream_text: &str, recording: &[f32]) -> bool {
    stream_text.trim().is_empty() && !is_silent(recording)
}

fn serve_stream(
    stream: &mut tc::Stream<'_>,
    generation: u64,
    jobs: &flume::Receiver<Job>,
    replies: &flume::Sender<Reply>,
) -> StreamEnd {
    let mut failed = false;
    let started = Instant::now();
    let mut compute = Duration::ZERO;
    loop {
        let Ok(job) = jobs.recv() else {
            return StreamEnd::Closed;
        };
        match job {
            Job::Feed { generation: g, pcm } if g == generation => {
                let mut partial = None;
                if !failed {
                    let t = Instant::now();
                    match stream.feed(&pcm) {
                        Ok(update) => {
                            if update.committed_changed || update.tentative_changed {
                                let text = stream.text();
                                partial = Some(Partial {
                                    committed: text.committed,
                                    tentative: text.tentative,
                                });
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "stream feed failed; decoding the recording at the end: {e}"
                            );
                            failed = true;
                        }
                    }
                    compute += t.elapsed();
                }
                let _ = replies.send(Reply::Fed {
                    generation,
                    partial,
                });
            }
            Job::Finish {
                generation: g,
                recording,
            } if g == generation => {
                if !failed {
                    let t = Instant::now();
                    match stream.finalize() {
                        Ok(_) => {
                            let text = stream.text().display();
                            log::debug!(
                                "stream: {} ms of audio, {} ms decoding while recording, \
                                 finalize {} ms",
                                recording.len() / (earwig_core::SAMPLE_RATE as usize / 1000),
                                compute.as_millis(),
                                t.elapsed().as_millis(),
                            );
                            if !should_fallback_decode(&text, &recording) {
                                let _ = replies.send(Reply::Finished {
                                    generation,
                                    text: Ok(text),
                                });
                                return StreamEnd::Done;
                            }
                            log::info!("stream produced no text; decoding the recording");
                        }
                        Err(e) => {
                            log::warn!("stream finalize failed; decoding the recording: {e}");
                        }
                    }
                }
                log::debug!("stream lived {} ms", started.elapsed().as_millis());
                if is_silent(&recording) {
                    let _ = replies.send(Reply::Finished {
                        generation,
                        text: Ok(String::new()),
                    });
                    return StreamEnd::Done;
                }
                return StreamEnd::Fallback(recording);
            }
            Job::Abort => return StreamEnd::Done,
            other => return StreamEnd::Next(other),
        }
    }
}

/// One batch run. Truncated or repeating output still yields its text.
fn decode(session: &mut tc::Session, run: &tc::RunOptions, pcm: &[f32]) -> Result<String, String> {
    if pcm.is_empty() {
        return Ok(String::new());
    }
    match session.run(pcm, run) {
        Ok(transcript) => Ok(transcript.text),
        Err(
            tc::Error::OutputTruncated {
                partial: Some(partial),
                ..
            }
            | tc::Error::OutputRepetition {
                partial: Some(partial),
                ..
            },
        ) => {
            log::warn!("decode stopped early; using the partial transcript");
            Ok(partial.text)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cpu_short_of_the_build_is_told_each_feature_it_lacks() {
        assert!(check_cpu(&[]).is_ok());
        let refused = |missing: &[&str]| check_cpu(missing).unwrap_err().to_string();
        let needs = ", which this build of earwig's speech engine needs";
        assert_eq!(refused(&["F16C"]), format!("this CPU lacks F16C{needs}"));
        assert_eq!(
            refused(&["AVX2", "FMA"]),
            format!("this CPU lacks AVX2 and FMA{needs}")
        );
        assert_eq!(
            refused(&["AVX2", "BMI2", "FMA"]),
            format!("this CPU lacks AVX2, BMI2 and FMA{needs}")
        );
    }

    #[test]
    fn a_bare_language_code_finds_the_models_regional_tag() {
        let listed = ["en-US", "en-GB", "ja-JP", "fr"].map(String::from);
        let hint = |lang| listed_language(lang, &listed);
        assert_eq!(hint("en-GB").as_deref(), Some("en-GB"));
        assert_eq!(hint("en").as_deref(), Some("en-US"));
        assert_eq!(hint("ja").as_deref(), Some("ja-JP"));
        assert_eq!(hint("fr").as_deref(), Some("fr"));
        assert_eq!(hint("de"), None);
        // A model that lists no languages gets the hint as it is.
        assert_eq!(listed_language("de", &[]).as_deref(), Some("de"));
    }

    #[test]
    fn silent_streaming_takes_skip_the_batch_fallback() {
        assert!(!should_fallback_decode("", &[]));
        assert!(!should_fallback_decode("  ", &vec![0.0; 16_000]));
        assert!(!should_fallback_decode("", &vec![5e-4; 16_000]));
        assert!(should_fallback_decode("", &vec![0.05; 16_000]));
        assert!(!should_fallback_decode("hello", &vec![0.05; 16_000]));
    }

    #[test]
    fn beginning_a_new_take_never_uncancels_an_aborted_preview() {
        let cancel = tc::CancelToken::new();
        let allow_preview = Arc::new(AtomicU64::new(0));
        let (jobs_tx, jobs_rx) = flume::unbounded();
        let (replies_tx, replies_rx) = flume::unbounded();
        let (preview_started_tx, preview_started_rx) = flume::bounded(1);
        let (release_preview_tx, release_preview_rx) = flume::bounded(1);

        let worker_cancel = cancel.clone();
        let worker_allow = allow_preview.clone();
        let worker = thread::spawn(move || {
            while let Ok(job) = jobs_rx.recv() {
                match job {
                    Job::Preview { generation, .. } => {
                        let started = Instant::now();
                        let text = run_preview(generation, &worker_allow, &worker_cancel, || {
                            let _ = preview_started_tx.send(generation);
                            let _ = release_preview_rx.recv();
                            if worker_cancel.is_cancelled() {
                                Err("aborted".into())
                            } else {
                                Ok("stale preview".into())
                            }
                        });
                        let _ = replies_tx.send(Reply::Preview {
                            generation,
                            text,
                            took: started.elapsed(),
                        });
                    }
                    Job::Finish {
                        generation,
                        recording,
                    } => {
                        worker_cancel.reset();
                        let _ = replies_tx.send(Reply::Finished {
                            generation,
                            text: Ok(format!("samples={}", recording.len())),
                        });
                    }
                    _ => {}
                }
            }
        });

        let mut model = TranscribeModel {
            info: ModelInfo {
                architecture: "fake".into(),
                streaming: false,
                backend: "cpu".into(),
            },
            jobs: Some(jobs_tx),
            replies: replies_rx,
            worker: Some(worker),
            cancel: cancel.clone(),
            allow_preview,
            max_samples: 0,
            generation: 0,
            active: false,
            streaming_now: false,
            recording: Vec::new(),
            preview_in_flight: false,
            next_preview: Instant::now(),
        };

        // Take 1 starts a preview decode.
        model.begin().unwrap();
        model.next_preview = Instant::now();
        assert!(model.feed(&[0.2; 160]).unwrap().is_none());
        assert_eq!(
            preview_started_rx.recv_timeout(Duration::from_secs(5)),
            Ok(1)
        );
        assert!(model.preview_in_flight);

        // Abort take 1 and immediately start take 2 while take 1's preview is still running.
        model.abort();
        assert!(cancel.is_cancelled());
        model.begin().unwrap();
        assert!(
            cancel.is_cancelled(),
            "begin() must not reset cancel while take 1's preview is still in flight"
        );
        assert!(
            model.preview_in_flight,
            "begin() must keep tracking the in-flight preview until the worker replies"
        );

        // Unblock take 1's preview: it sees cancellation, and take 2 finishes cleanly.
        release_preview_tx.send(()).unwrap();
        model.feed(&[0.3; 320]).unwrap();
        assert_eq!(model.finish().unwrap(), "samples=320");
        assert!(!model.preview_in_flight);
    }
}
