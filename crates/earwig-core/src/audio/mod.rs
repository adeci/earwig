//! Microphone capture as 16 kHz mono `f32`. The callback only downmixes; a
//! worker analyses the raw input for the overlay ([`crate::spectrum`]),
//! resamples, keeps the pre-roll, and reopens a failed input.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use rubato::Resampler as _;
use rubato::audioadapter_buffers::direct::InterleavedSlice;

use crate::SAMPLE_RATE;
use crate::config::AudioConfig;
use crate::spectrum::{Analyzer, Feed, Spectrum};

/// Starts every [`AudioEvent::Error`], setting mic failures apart from others.
pub const MIC_ERROR_PREFIX: &str = "microphone: ";

const REOPEN_DELAY: Duration = Duration::from_secs(2);
/// An open input that delivers nothing for this long counts as failed.
const STALL_TIMEOUT: Duration = Duration::from_secs(3);
/// How often a clip recording delivers audio, like a capture buffer.
const CLIP_TICK: Duration = Duration::from_millis(10);

/// A microphone that `[audio] device` can name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    /// What `[audio] device` holds; the same across runs.
    pub id: String,
    /// What people call it: the name the desktop's sound settings show.
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputDevices {
    pub devices: Vec<InputDevice>,
    /// What the system default records from, when that is known; not always
    /// one of `devices` (it can be a monitor of an output).
    pub default: Option<InputDevice>,
}

/// The microphones there are now, plus `configured` under its own name when
/// it is an older kind of name that still opens (an ALSA PCM on Linux).
pub fn input_devices(configured: Option<&str>) -> anyhow::Result<InputDevices> {
    os::input_devices(configured)
}

/// cpal's input devices that `device` describes, and the default one. Linux
/// lists the sound server's sources instead.
#[cfg(not(target_os = "linux"))]
fn cpal_devices(
    device: impl Fn(&cpal::Device) -> Option<InputDevice>,
) -> anyhow::Result<InputDevices> {
    let host = cpal::default_host();
    Ok(InputDevices {
        devices: host
            .input_devices()?
            .filter_map(|input| device(&input))
            .collect(),
        default: host.default_input_device().as_ref().and_then(&device),
    })
}

/// The cpal input device whose `id` is `wanted`, or the default one, and
/// its id.
fn cpal_input(
    wanted: Option<&str>,
    id: impl Fn(&cpal::Device) -> Option<String>,
) -> anyhow::Result<(cpal::Device, String)> {
    let host = cpal::default_host();
    let device = match wanted {
        None => host
            .default_input_device()
            .context("no default input device")?,
        Some(wanted) => host
            .input_devices()?
            .find(|d| id(d).as_deref() == Some(wanted))
            .with_context(|| format!("input device {wanted:?} not found"))?,
    };
    let id = id(&device).unwrap_or_else(|| "unknown".into());
    Ok((device, id))
}

/// cpal's id for `device`: an ALSA PCM name on Linux, a Core Audio UID on
/// macOS.
#[cfg(not(windows))]
fn cpal_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.id().to_owned())
}

/// Read a WAV file as 16 kHz mono `f32`, whatever its rate, channel count or
/// sample format.
pub fn read_wav(path: &Path) -> anyhow::Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / to_f32(1 << (spec.bits_per_sample - 1));
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| sample_f32(s) * scale))
                .collect::<Result<_, _>>()?
        }
    };
    let mono: Vec<f32> = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / to_f32(channels))
        .collect();
    resample_all(&mono, spec.sample_rate)
}

/// Resample a whole clip to [`SAMPLE_RATE`].
fn resample_all(mono: &[f32], rate: u32) -> anyhow::Result<Vec<f32>> {
    if rate == SAMPLE_RATE || mono.is_empty() {
        return Ok(mono.to_vec());
    }
    let mut resampler = rubato::Fft::<f32>::new(
        rate as usize,
        SAMPLE_RATE as usize,
        1024,
        1,
        rubato::FixedSync::Input,
    )?;
    let input = InterleavedSlice::new(mono, 1, mono.len())?;
    Ok(resampler.process_all(&input, mono.len(), None)?.take_data())
}

#[allow(clippy::cast_precision_loss)]
fn to_f32(n: usize) -> f32 {
    n as f32
}

#[allow(clippy::cast_precision_loss)]
fn sample_f32(s: i32) -> f32 {
    s as f32
}

#[derive(Clone, Debug)]
pub enum Source {
    /// The `[audio] device` input, or the system default.
    Microphone,
    /// A 16 kHz mono clip heard by every recording: it plays in real time
    /// from the start of each recording, followed by silence.
    Clip(Arc<[f32]>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioEvent {
    /// Audio of recording `recording`, in order. The first chunk after
    /// [`Capture::start`] is the pre-roll.
    Samples { recording: u64, pcm: Vec<f32> },
    /// Input level of the recording, 0.0 to 1.0, 20 times a second.
    Level { recording: u64, level: f32 },
    /// Every sample of `recording` has been delivered.
    Stopped { recording: u64 },
    /// The input failed; it is reopened in the background.
    Error(String),
}

enum Control {
    Start(u64),
    Stop(u64),
    Configure(AudioConfig),
}

/// Dropping it closes the input.
pub struct Capture {
    control: Option<flume::Sender<Control>>,
    worker: Option<JoinHandle<()>>,
}

impl Capture {
    /// Capture from `source`; `events` gets the audio and `feed` the
    /// overlay's live spectrum of each recording.
    pub fn spawn(
        source: Source,
        config: AudioConfig,
        events: flume::Sender<AudioEvent>,
        feed: Feed,
    ) -> anyhow::Result<Self> {
        let (control, control_rx) = flume::unbounded();
        let worker = thread::Builder::new()
            .name("earwig-audio".into())
            .spawn(move || Worker::new(source, config, events, feed).run(&control_rx))?;
        Ok(Self {
            control: Some(control),
            worker: Some(worker),
        })
    }

    /// Begin delivering audio for `recording`, pre-roll first.
    pub fn start(&self, recording: u64) {
        self.send(Control::Start(recording));
    }

    /// Stop delivering; [`AudioEvent::Stopped`] follows the last samples.
    pub fn stop(&self, recording: u64) {
        self.send(Control::Stop(recording));
    }

    pub fn configure(&self, config: AudioConfig) {
        self.send(Control::Configure(config));
    }

    fn send(&self, control: Control) {
        if let Some(tx) = &self.control {
            let _ = tx.send(control);
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.control.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Raw {
    Data(Vec<f32>),
    Error(String),
}

/// An input an OS module opened: its audio goes to the channel it was given
/// until `stream` is dropped.
struct Opened {
    stream: os::Stream,
    rate: u32,
    name: String,
}

struct Input {
    _stream: os::Stream,
    rx: flume::Receiver<Raw>,
    rate: u32,
    resampler: StreamResampler,
    last_data: Instant,
    name: String,
}

struct Recording {
    id: u64,
    started: Instant,
    clip_sent: usize,
}

struct Worker {
    source: Source,
    config: AudioConfig,
    events: flume::Sender<AudioEvent>,
    feed: Feed,
    input: Option<Input>,
    /// When to try opening the input next.
    reopen_at: Instant,
    /// The current failure was already reported.
    failing: bool,
    preroll: VecDeque<f32>,
    recording: Option<Recording>,
    /// Hears all input, recording or not, to know the noise floor before the
    /// first word; fresh for each new input, and each recording of a clip.
    analyzer: Option<Analyzer>,
}

enum Wake {
    Control(Result<Control, flume::RecvError>),
    Raw(Result<Raw, flume::RecvError>),
    Timeout,
}

impl Worker {
    fn new(
        source: Source,
        config: AudioConfig,
        events: flume::Sender<AudioEvent>,
        feed: Feed,
    ) -> Self {
        Self {
            source,
            config,
            events,
            feed,
            input: None,
            reopen_at: Instant::now(),
            failing: false,
            preroll: VecDeque::new(),
            recording: None,
            analyzer: None,
        }
    }

    fn run(mut self, control: &flume::Receiver<Control>) {
        loop {
            self.maintain_input();
            let timeout = self.next_timeout();
            let selector = flume::Selector::new().recv(control, Wake::Control);
            let wake = match &self.input {
                Some(input) => selector.recv(&input.rx, Wake::Raw).wait_timeout(timeout),
                None => selector.wait_timeout(timeout),
            }
            .unwrap_or(Wake::Timeout);
            match wake {
                Wake::Control(Ok(Control::Start(id))) => self.start(id),
                Wake::Control(Ok(Control::Stop(id))) => self.stop(id),
                Wake::Control(Ok(Control::Configure(config))) => self.configure(config),
                Wake::Control(Err(_)) => return,
                Wake::Raw(Ok(Raw::Data(pcm))) => self.process(&pcm),
                Wake::Raw(Ok(Raw::Error(e))) => self.fail(&e),
                Wake::Raw(Err(_)) => self.fail("audio stream ended"),
                Wake::Timeout => {}
            }
            self.play_clip();
        }
    }

    fn wants_input(&self) -> bool {
        matches!(self.source, Source::Microphone)
            && (self.config.keep_open || self.recording.is_some())
    }

    fn next_timeout(&self) -> Duration {
        if matches!(self.source, Source::Clip(_)) && self.recording.is_some() {
            return CLIP_TICK;
        }
        if self.wants_input() && self.input.is_none() {
            return self.reopen_at.saturating_duration_since(Instant::now());
        }
        Duration::from_secs(1)
    }

    /// Open, close, or reopen the input to match what is wanted.
    fn maintain_input(&mut self) {
        if !self.wants_input() {
            if let Some(input) = self.input.take() {
                log::debug!("closing audio input {}", input.name);
            }
            return;
        }
        if let Some(input) = &self.input {
            if input.last_data.elapsed() > STALL_TIMEOUT {
                self.fail("no audio from the input device");
            }
            return;
        }
        if Instant::now() < self.reopen_at {
            return;
        }
        match open_input(self.config.device.as_deref()) {
            Ok(input) => {
                self.failing = false;
                self.input = Some(input);
                self.analyzer = None;
            }
            Err(e) => self.fail(&format!("{e:#}")),
        }
    }

    fn fail(&mut self, message: &str) {
        self.input = None;
        self.reopen_at = Instant::now() + REOPEN_DELAY;
        if !std::mem::replace(&mut self.failing, true) {
            log::warn!("audio input failed, retrying: {message}");
            let _ = self
                .events
                .send(AudioEvent::Error(format!("{MIC_ERROR_PREFIX}{message}")));
        }
    }

    fn configure(&mut self, config: AudioConfig) {
        if config.device != self.config.device {
            self.input = None;
            self.failing = false;
            self.reopen_at = Instant::now();
        }
        self.config = config;
        self.trim_preroll();
    }

    fn start(&mut self, id: u64) {
        let preroll: Vec<f32> = if self.config.keep_open {
            self.preroll.drain(..).collect()
        } else {
            Vec::new()
        };
        self.preroll.clear();
        self.recording = Some(Recording {
            id,
            started: Instant::now(),
            clip_sent: 0,
        });
        if matches!(self.source, Source::Clip(_)) {
            self.analyzer = None;
        }
        self.feed.publish(Spectrum::default());
        if self.input.is_none() {
            self.reopen_at = Instant::now();
        }
        if !preroll.is_empty() {
            self.deliver(&preroll);
        }
    }

    fn stop(&mut self, id: u64) {
        if self.recording.as_ref().is_some_and(|r| r.id == id) {
            // Whatever the callback captured before the stop belongs to it.
            let pending: Vec<Raw> = self
                .input
                .as_ref()
                .map(|input| input.rx.try_iter().collect())
                .unwrap_or_default();
            for raw in pending {
                match raw {
                    Raw::Data(pcm) => self.process(&pcm),
                    Raw::Error(e) => self.fail(&e),
                }
            }
            self.play_clip();
            self.recording = None;
            self.feed.publish(Spectrum::default());
        }
        let _ = self.events.send(AudioEvent::Stopped { recording: id });
    }

    fn process(&mut self, raw: &[f32]) {
        let Some(input) = &mut self.input else {
            return;
        };
        input.last_data = Instant::now();
        let rate = input.rate;
        // The overlay hears the voice before the resampler waits for a
        // full chunk.
        self.analyze(rate, raw);
        let Some(input) = &mut self.input else {
            return;
        };
        let pcm = input.resampler.process(raw);
        if self.recording.is_some() {
            self.deliver(&pcm);
        } else if self.config.keep_open {
            self.preroll.extend(pcm);
            self.trim_preroll();
        }
    }

    /// The analyzer learns the noise floor from all input; while recording,
    /// levels go to the controller and the spectrum to the overlay.
    fn analyze(&mut self, rate: u32, pcm: &[f32]) {
        let analyzer = self.analyzer.get_or_insert_with(|| Analyzer::new(rate));
        let recording = self.recording.as_ref().map(|r| r.id);
        analyzer.push(pcm, |level| {
            if let Some(recording) = recording {
                let _ = self.events.send(AudioEvent::Level { recording, level });
            }
        });
        if recording.is_some() {
            self.feed.publish(analyzer.spectrum());
        }
    }

    fn trim_preroll(&mut self) {
        let keep = usize::try_from(self.config.preroll_ms).unwrap_or(usize::MAX)
            * (SAMPLE_RATE as usize / 1000);
        let excess = self.preroll.len().saturating_sub(keep);
        self.preroll.drain(..excess);
    }

    fn play_clip(&mut self) {
        let (Source::Clip(clip), Some(recording)) = (&self.source, &mut self.recording) else {
            return;
        };
        let due = usize::try_from(
            recording.started.elapsed().as_millis() * u128::from(SAMPLE_RATE) / 1000,
        )
        .unwrap_or(usize::MAX);
        if due <= recording.clip_sent {
            return;
        }
        let pcm: Vec<f32> = (recording.clip_sent..due)
            .map(|i| clip.get(i).copied().unwrap_or(0.0))
            .collect();
        recording.clip_sent = due;
        self.analyze(SAMPLE_RATE, &pcm);
        self.deliver(&pcm);
    }

    fn deliver(&self, pcm: &[f32]) {
        // The resampler returns nothing until it has a full chunk.
        let (Some(recording), false) = (&self.recording, pcm.is_empty()) else {
            return;
        };
        let _ = self.events.send(AudioEvent::Samples {
            recording: recording.id,
            pcm: pcm.to_vec(),
        });
    }
}

fn open_input(device: Option<&str>) -> anyhow::Result<Input> {
    let (tx, rx) = flume::unbounded();
    let opened = os::open(device, tx)?;
    Ok(Input {
        _stream: opened.stream,
        rx,
        rate: opened.rate,
        resampler: StreamResampler::new(opened.rate)?,
        last_data: Instant::now(),
        name: opened.name,
    })
}

/// Capture from `device`, called `name` in the log, in its own default
/// format, downmixed to mono, into `tx`; returns the stream and its rate.
fn open_cpal(
    device: &cpal::Device,
    name: &str,
    tx: flume::Sender<Raw>,
) -> anyhow::Result<(cpal::Stream, u32)> {
    let supported = device.default_input_config()?;
    let channels = usize::from(supported.channels());
    let rate = supported.sample_rate();
    let config = supported.config();
    // The device's own period, never a fixed one: cpal rings only two periods,
    // so one off PipeWire's quantum (480 vs 512) chops the voice every cycle.
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(device, config, channels, tx),
        cpal::SampleFormat::F64 => build::<f64>(device, config, channels, tx),
        cpal::SampleFormat::I16 => build::<i16>(device, config, channels, tx),
        cpal::SampleFormat::I32 => build::<i32>(device, config, channels, tx),
        cpal::SampleFormat::I8 => build::<i8>(device, config, channels, tx),
        cpal::SampleFormat::U16 => build::<u16>(device, config, channels, tx),
        cpal::SampleFormat::U8 => build::<u8>(device, config, channels, tx),
        other => anyhow::bail!("{name}: unsupported sample format {other}"),
    }
    .with_context(|| format!("opening {name}"))?;
    stream.play().with_context(|| format!("starting {name}"))?;
    let buffer = stream.buffer_size().map_or_else(
        |_| "unknown".to_owned(),
        |frames| {
            format!(
                "{frames} frames ({:.1} ms)",
                1000. * to_f32(frames as usize) / to_f32(rate as usize)
            )
        },
    );
    log::info!("audio input {name}: {rate} Hz, {channels} channel(s), buffer {buffer}");
    Ok((stream, rate))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    tx: flume::Sender<Raw>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let errors = tx.clone();
    let scale = 1.0 / to_f32(channels);
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            let mono = data
                .chunks_exact(channels)
                .map(|frame| {
                    frame
                        .iter()
                        .map(|&s| <f32 as cpal::FromSample<T>>::from_sample_(s))
                        .sum::<f32>()
                        * scale
                })
                .collect();
            let _ = tx.send(Raw::Data(mono));
        },
        move |e: cpal::Error| match e.kind() {
            // The stream keeps running through these.
            cpal::ErrorKind::DeviceChanged
            | cpal::ErrorKind::Xrun
            | cpal::ErrorKind::RealtimeDenied => log::debug!("audio input: {e}"),
            _ => {
                let _ = errors.send(Raw::Error(e.to_string()));
            }
        },
        None,
    )
}

struct StreamResampler {
    fft: Option<rubato::Fft<f32>>,
    pending: Vec<f32>,
    out: Vec<f32>,
}

impl StreamResampler {
    fn new(rate: u32) -> anyhow::Result<Self> {
        let fft = if rate == SAMPLE_RATE {
            None
        } else {
            // 20 ms chunks keep latency low.
            let chunk = (rate as usize / 50).max(1);
            Some(rubato::Fft::<f32>::new(
                rate as usize,
                SAMPLE_RATE as usize,
                chunk,
                1,
                rubato::FixedSync::Input,
            )?)
        };
        let out = vec![0.0; fft.as_ref().map_or(0, rubato::Resampler::output_frames_max)];
        Ok(Self {
            fft,
            pending: Vec::new(),
            out,
        })
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let Some(fft) = &mut self.fft else {
            return input.to_vec();
        };
        self.pending.extend_from_slice(input);
        let mut result = Vec::new();
        let mut used = 0;
        loop {
            let need = fft.input_frames_next();
            if self.pending.len() - used < need {
                break;
            }
            let frames_out = self.out.len();
            let chunk = &self.pending[used..used + need];
            let (Ok(input), Ok(mut output)) = (
                InterleavedSlice::new(chunk, 1, need),
                InterleavedSlice::new_mut(&mut self.out, 1, frames_out),
            ) else {
                break;
            };
            match fft.process_into_buffer(&input, &mut output, None) {
                Ok((read, written)) => {
                    used += read;
                    result.extend_from_slice(&self.out[..written]);
                }
                Err(e) => {
                    log::warn!("resampler: {e}");
                    used = self.pending.len();
                    break;
                }
            }
        }
        self.pending.drain(..used);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, n: usize, amplitude: f32) -> Vec<f32> {
        let step = 440.0 * std::f32::consts::TAU / to_f32(rate as usize);
        (0..n)
            .map(|i| amplitude * (to_f32(i) * step).sin())
            .collect()
    }

    #[test]
    fn stream_resampler_keeps_duration() {
        let mut r = StreamResampler::new(48_000).unwrap();
        let input = tone(48_000, 48_000, 0.5);
        let mut out = Vec::new();
        for chunk in input.chunks(441) {
            out.extend(r.process(chunk));
        }
        // Up to one 20 ms chunk stays buffered.
        assert!((15_600..=16_000).contains(&out.len()), "{}", out.len());
    }

    #[test]
    fn read_wav_converts_to_16k_mono() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for s in tone(44_100, 22_050, 0.5) {
            #[allow(clippy::cast_possible_truncation)]
            let v = (s * f32::from(i16::MAX)) as i16;
            writer.write_sample(v).unwrap();
            writer.write_sample(v).unwrap();
        }
        writer.finalize().unwrap();
        let pcm = read_wav(&path).unwrap();
        assert!((7_990..=8_010).contains(&pcm.len()), "{}", pcm.len());
        let peak = pcm.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((0.45..0.55).contains(&peak), "{peak}");
    }

    #[test]
    fn a_failure_is_reported_once_with_the_microphone_prefix() {
        let (tx, rx) = flume::unbounded();
        let config = AudioConfig::default();
        let mut worker = Worker::new(Source::Microphone, config, tx, Feed::default());
        worker.fail("no audio from the input device");
        worker.fail("no audio from the input device");
        let reported: Vec<AudioEvent> = rx.try_iter().collect();
        assert_eq!(
            reported,
            [AudioEvent::Error(format!(
                "{MIC_ERROR_PREFIX}no audio from the input device"
            ))]
        );
    }

    fn recv(rx: &flume::Receiver<AudioEvent>) -> AudioEvent {
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    #[allow(clippy::float_cmp)] // samples are copied from the clip verbatim
    fn clip_plays_per_recording_then_stops() {
        let clip: Arc<[f32]> = vec![0.5; 1_600].into();
        let (tx, rx) = flume::unbounded();
        let feed = Feed::default();
        let wakes = feed.wakes();
        let capture =
            Capture::spawn(Source::Clip(clip), AudioConfig::default(), tx, feed.clone()).unwrap();
        capture.start(1);
        assert!(
            wakes.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the overlay hears it"
        );
        thread::sleep(Duration::from_millis(250));
        capture.stop(1);
        let mut samples = Vec::new();
        let mut levels = 0;
        loop {
            match recv(&rx) {
                AudioEvent::Samples { recording: 1, pcm } => samples.extend(pcm),
                AudioEvent::Level { recording: 1, .. } => levels += 1,
                AudioEvent::Stopped { recording: 1 } => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(samples.len() >= 3_200, "{}", samples.len());
        assert!(samples[..1_600].iter().all(|&s| s == 0.5));
        assert!(samples[1_600..].iter().all(|&s| s == 0.0));
        assert!(levels >= 4, "{levels}");
        assert_eq!(feed.latest(), Spectrum::default(), "cleared once stopped");
        // Nothing arrives between recordings.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
