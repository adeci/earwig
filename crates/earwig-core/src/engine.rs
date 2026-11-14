//! The speech engine boundary; another runtime is one crate implementing
//! these traits. Audio is 16 kHz mono `f32` in `[-1, 1]`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Loads models; shared by the daemon and the model browser.
pub trait EngineFactory: Send + Sync {
    /// Whether a model whose GGUF `general.architecture` is `arch` loads, so
    /// search results can be marked before anything is downloaded.
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

/// Also `[model] backend`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

    /// Add audio to the current recording; returns the live transcript when it
    /// changed. Must stay cheap enough for real time.
    fn feed(&mut self, pcm: &[f32]) -> anyhow::Result<Option<Partial>>;

    /// End the recording and return the final text.
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
