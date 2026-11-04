//! The speech engine boundary.
//!
//! earwig ships one engine (`earwig-transcribe`, transcribe.cpp/GGUF), but the
//! daemon only sees these traits, so another runtime (ONNX, whisper.cpp, a
//! remote service) is one crate implementing them.
//!
//! Audio is always 16 kHz mono `f32` in `[-1, 1]` ([`crate::SAMPLE_RATE`]).

use std::path::Path;

/// Loads models. Shared by the daemon and the model browser.
pub trait EngineFactory: Send + Sync {
    /// Short name for logs and the UI, e.g. `transcribe.cpp`.
    fn name(&self) -> &str;

    /// Whether a model whose GGUF `general.architecture` is `arch` loads.
    /// The model browser uses this to mark search results compatible before
    /// downloading anything.
    fn supports_architecture(&self, arch: &str) -> bool;

    /// Load a model file and keep it warm for repeated dictations.
    fn load(&self, path: &Path, options: &EngineOptions) -> anyhow::Result<Box<dyn Model>>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineOptions {
    /// `auto` picks the best available accelerator; `cpu` forces CPU.
    pub backend: Backend,
    /// BCP-47 language hint for multilingual models; `None` lets the model
    /// detect or use its only language.
    pub language: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

/// A loaded model. Owned by the daemon's engine thread.
pub trait Model: Send {
    fn info(&self) -> &ModelInfo;

    /// Start a recording. Any previous recording is discarded.
    fn begin(&mut self) -> anyhow::Result<()>;

    /// Add audio to the current recording. Returns the live transcript when it
    /// changed. Models without native streaming may re-decode periodically
    /// here; they must keep this call cheap enough for real time.
    fn feed(&mut self, pcm: &[f32]) -> anyhow::Result<Option<Partial>>;

    /// End the recording and return the final text. With native streaming
    /// most audio is already decoded, so this only covers the tail.
    fn finish(&mut self) -> anyhow::Result<String>;

    /// Drop the current recording without decoding.
    fn abort(&mut self);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// GGUF `general.architecture`, e.g. `parakeet-unified`.
    pub architecture: String,
    pub streaming: bool,
    /// Compute backend actually in use, e.g. `vulkan`, `cpu`.
    pub backend: String,
}

/// Live transcript of the recording so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Partial {
    /// Stable prefix: only grows during a recording.
    pub committed: String,
    /// Tail that may still change.
    pub tentative: String,
}
