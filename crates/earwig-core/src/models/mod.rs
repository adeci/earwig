//! Speech models: which one to use, where it lives, and how to get it.
//!
//! A model is named by a [`ModelSpec`]: a curated catalog id, an exact file in
//! a Hugging Face repo, or a local path. The catalog that ships with each
//! release is `models.toml` next to this crate; any GGUF the engine can load
//! works too, which is how users swap models without a code change.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// Which model to use.
///
/// String form (config files, CLI, socket):
/// - `parakeet-unified-en-0.6b`: curated catalog id
/// - `hf:owner/repo/path/to/file.gguf@<commit>`: a file in a Hugging Face
///   repo, pinned to a commit
/// - `/abs/path/model.gguf`: a local file (Nix store paths land here)
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum ModelSpec {
    Curated {
        id: String,
    },
    HuggingFace {
        repo: String,
        file: String,
        revision: String,
    },
    Path {
        path: PathBuf,
    },
}

impl fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Curated { id } => f.write_str(id),
            Self::HuggingFace {
                repo,
                file,
                revision,
            } => write!(f, "hf:{repo}/{file}@{revision}"),
            Self::Path { path } => write!(f, "{}", path.display()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid model spec {spec:?}: {reason}")]
pub struct ParseModelSpecError {
    pub spec: String,
    pub reason: &'static str,
}

impl FromStr for ModelSpec {
    type Err = ParseModelSpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason| ParseModelSpecError {
            spec: s.to_owned(),
            reason,
        };
        if let Some(rest) = s.strip_prefix("hf:") {
            let (path, revision) = rest
                .rsplit_once('@')
                .ok_or_else(|| err("missing @<commit>"))?;
            let mut parts = path.splitn(3, '/');
            let (Some(owner), Some(name), Some(file)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(err("expected hf:owner/repo/file@commit"));
            };
            if owner.is_empty() || name.is_empty() || file.is_empty() || revision.is_empty() {
                return Err(err("empty owner, repo, file or commit"));
            }
            // Each part becomes a directory under the model store.
            if !is_plain_relative(&format!("{owner}/{name}/{file}"))
                || !is_plain_relative(revision)
                || revision.contains('/')
            {
                return Err(err("empty, `.` or `..` path segment"));
            }
            return Ok(Self::HuggingFace {
                repo: format!("{owner}/{name}"),
                file: file.to_owned(),
                revision: revision.to_owned(),
            });
        }
        if s.starts_with('/') {
            return Ok(Self::Path { path: s.into() });
        }
        if s.is_empty() || s.contains(['/', ' ', ':']) {
            return Err(err("not a catalog id, hf: spec or absolute path"));
        }
        Ok(Self::Curated { id: s.to_owned() })
    }
}

/// `a/b/c` with no empty, `.` or `..` segments: safe to join under a
/// directory without escaping it.
pub(crate) fn is_plain_relative(path: &str) -> bool {
    path.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.contains('\\'))
}

impl TryFrom<String> for ModelSpec {
    type Error = ParseModelSpecError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<ModelSpec> for String {
    fn from(spec: ModelSpec) -> Self {
        spec.to_string()
    }
}

/// One entry of the curated catalog (`models.toml`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    /// Human name, e.g. "Parakeet Unified 0.6B (English)".
    pub name: String,
    /// One line on why you would pick it.
    pub summary: String,
    pub repo: String,
    pub file: String,
    /// Full commit hash; downloads resolve against it, never a branch.
    pub revision: String,
    pub sha256: String,
    pub size: u64,
    /// SPDX id or the name the model card uses.
    pub license: String,
    pub languages: Vec<String>,
    /// Native streaming (live text without re-decoding).
    pub streaming: bool,
    /// The release default. Exactly one entry has it.
    #[serde(default)]
    pub default: bool,
}

/// A Hugging Face search hit, already checked against the engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub repo: String,
    pub downloads: u64,
    pub likes: u64,
    pub files: Vec<RemoteFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    pub file: String,
    pub size: u64,
    /// LFS sha256 from the Hub tree listing.
    pub sha256: Option<String>,
    /// `general.architecture` read from the GGUF header with a range request.
    pub architecture: Option<String>,
    /// The engine can load this architecture.
    pub compatible: bool,
    /// Commit the listing was taken at; downloads pin to it.
    pub revision: String,
}

/// A model file on disk that earwig knows about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub spec: ModelSpec,
    pub path: PathBuf,
    pub size: u64,
    /// Catalog entry, when the file is a curated model.
    pub catalog_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub spec: ModelSpec,
    pub downloaded: u64,
    pub total: u64,
}

/// Cooperative cancellation for downloads. Cancelling keeps the partial file
/// so the next attempt resumes.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("unknown catalog model {0:?}")]
    UnknownCurated(String),
    #[error("model {0} is not downloaded")]
    NotInstalled(ModelSpec),
    #[error("{0} is a local file; earwig only downloads and deletes files in its model store")]
    LocalFile(ModelSpec),
    #[error("download cancelled")]
    Cancelled,
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("hugging face request failed: {0}")]
    Http(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

mod download;
mod gguf;
mod hub;
mod manager;
pub use manager::{ModelManager, parse_catalog};

#[cfg(test)]
mod tests {
    use super::ModelSpec;

    #[test]
    fn specs_round_trip_through_their_string_form() {
        for s in [
            "parakeet-unified-en-0.6b",
            "hf:owner/repo/model-Q8_0.gguf@0123456789abcdef0123456789abcdef01234567",
            "hf:owner/repo/sub/dir/model.gguf@main",
            "/nix/store/abc-model/model.gguf",
        ] {
            let spec: ModelSpec = s.parse().unwrap();
            assert_eq!(spec.to_string(), s);
            let json = serde_json::to_string(&spec).unwrap();
            assert_eq!(serde_json::from_str::<ModelSpec>(&json).unwrap(), spec);
        }
    }

    #[test]
    fn hf_spec_splits_repo_file_and_last_at() {
        assert_eq!(
            "hf:o/r/a@b/c.gguf@abc".parse::<ModelSpec>().unwrap(),
            ModelSpec::HuggingFace {
                repo: "o/r".into(),
                file: "a@b/c.gguf".into(),
                revision: "abc".into(),
            }
        );
    }

    #[test]
    fn malformed_specs_are_rejected() {
        for s in [
            "",
            "relative/path.gguf",
            "has space",
            "c:model",
            "hf:owner/repo/file.gguf",
            "hf:owner/file.gguf@abc",
            "hf:owner//file.gguf@abc",
            "hf:owner/repo/file.gguf@",
            "hf:owner/repo/../../../etc/passwd@abc",
            "hf:../repo/file.gguf@abc",
            "hf:owner/repo/./file.gguf@abc",
            "hf:owner/repo/dir//file.gguf@abc",
            "hf:owner/repo/file.gguf@..",
            "hf:owner/repo/file.gguf@a/b",
        ] {
            assert!(s.parse::<ModelSpec>().is_err(), "{s:?} parsed");
        }
    }
}
