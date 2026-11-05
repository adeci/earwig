//! Hugging Face Hub client: search, tree listings, GGUF header sniffing.
//!
//! Requests (all relative to the endpoint, `EARWIG_HF_ENDPOINT` or
//! `https://huggingface.co`):
//! - `GET /api/models?filter=transcribe.cpp&search=<q>&sort=downloads&limit=<n>`
//! - `GET /api/models/<repo>/revision/main` for the commit to pin to
//! - `GET /api/models/<repo>/tree/<commit>?recursive=true` for files, sizes
//!   and LFS sha256s
//! - `GET /<repo>/resolve/<commit>/<file>` with `Range` for headers and
//!   resumed downloads

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::Agent;

use super::gguf::{self, Scan};
use super::{ModelError, RemoteFile, SearchResult};

pub(crate) const DEFAULT_ENDPOINT: &str = "https://huggingface.co";
/// Repos fetched per search.
const SEARCH_LIMIT: usize = 20;
/// Concurrent requests during a search.
const WORKERS: usize = 8;
/// First header range; GGUF metadata usually puts the architecture first.
const FIRST_RANGE: usize = 64 * 1024;
/// Give up on a header that needs more than this to reach the key.
const MAX_HEADER: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct Hub {
    endpoint: String,
    /// JSON and header ranges: everything bounded by a total timeout.
    api: Agent,
    /// Model files: no total timeout, but a stalled connection fails.
    files: Agent,
    /// `general.architecture` by (repo, commit, file); `None` = no such key.
    archs: Mutex<HashMap<(String, String, String), Option<String>>>,
}

#[derive(Deserialize)]
struct RepoHit {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
}

#[derive(Deserialize)]
struct Revision {
    sha: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TreeEntry {
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub size: u64,
    pub lfs: Option<Lfs>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Lfs {
    /// sha256 of the file.
    pub oid: String,
}

impl Hub {
    pub fn new(endpoint: &str) -> Self {
        let user_agent = concat!("earwig/", env!("CARGO_PKG_VERSION"));
        let api = Agent::config_builder()
            .user_agent(user_agent)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        let files = Agent::config_builder()
            .user_agent(user_agent)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .build()
            .new_agent();
        Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            api,
            files,
            archs: Mutex::default(),
        }
    }

    pub fn files_agent(&self) -> &Agent {
        &self.files
    }

    pub fn resolve_url(&self, repo: &str, revision: &str, file: &str) -> String {
        format!(
            "{}/{}/resolve/{}/{}",
            self.endpoint,
            encode_path(repo),
            encode_path(revision),
            encode_path(file)
        )
    }

    fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ModelError> {
        let url = format!("{}{path}", self.endpoint);
        let mut resp = self
            .api
            .get(&url)
            .query_pairs(query.iter().copied())
            .call()
            .map_err(|e| http(&url, &e))?;
        if !resp.status().is_success() {
            return Err(ModelError::Http(format!("{url}: HTTP {}", resp.status())));
        }
        resp.body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_json()
            .map_err(|e| http(&url, &e))
    }

    /// Every file in `repo` at `revision`.
    pub fn tree(&self, repo: &str, revision: &str) -> Result<Vec<TreeEntry>, ModelError> {
        self.get_json(
            &format!(
                "/api/models/{}/tree/{}",
                encode_path(repo),
                encode_path(revision)
            ),
            &[("recursive", "true")],
        )
    }

    fn head_commit(&self, repo: &str) -> Result<String, ModelError> {
        let rev: Revision = self.get_json(
            &format!("/api/models/{}/revision/main", encode_path(repo)),
            &[],
        )?;
        Ok(rev.sha)
    }

    /// Repos tagged `transcribe.cpp` matching `query` (all of them, by
    /// downloads, when empty), each with its GGUF files checked by
    /// `supports`.
    pub fn search(
        &self,
        query: &str,
        supports: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<SearchResult>, ModelError> {
        let limit = SEARCH_LIMIT.to_string();
        let mut params = vec![
            ("filter", "transcribe.cpp"),
            ("sort", "downloads"),
            ("limit", limit.as_str()),
        ];
        let query = query.trim();
        if !query.is_empty() {
            params.push(("search", query));
        }
        let hits: Vec<RepoHit> = self.get_json("/api/models", &params)?;

        // Pin each repo to its current commit and list its GGUF files.
        let listings = parallel_map(&hits, WORKERS, |hit| {
            let sha = self.head_commit(&hit.id)?;
            let files: Vec<TreeEntry> = self
                .tree(&hit.id, &sha)?
                .into_iter()
                .filter(|e| e.kind == "file" && is_gguf(&e.path))
                .collect();
            Ok::<_, ModelError>((sha, files))
        });

        let mut jobs = Vec::new();
        let mut results = Vec::new();
        for (hit, listing) in hits.into_iter().zip(listings) {
            let (sha, files) = match listing {
                Ok(listing) => listing,
                Err(e) => {
                    log::warn!("skipping {}: {e}", hit.id);
                    continue;
                }
            };
            if files.is_empty() {
                continue;
            }
            for entry in &files {
                jobs.push((hit.id.clone(), sha.clone(), entry.path.clone()));
            }
            results.push(SearchResult {
                repo: hit.id,
                downloads: hit.downloads,
                likes: hit.likes,
                files: files
                    .into_iter()
                    .map(|e| RemoteFile {
                        file: e.path,
                        size: e.size,
                        sha256: e.lfs.map(|l| l.oid),
                        architecture: None,
                        compatible: false,
                        revision: sha.clone(),
                    })
                    .collect(),
            });
        }

        let archs = parallel_map(&jobs, WORKERS, |(repo, sha, file)| {
            self.architecture(repo, sha, file).unwrap_or_else(|e| {
                log::warn!("reading the header of {repo}/{file}: {e}");
                None
            })
        });
        let mut archs = archs.into_iter();
        for file in results.iter_mut().flat_map(|r| r.files.iter_mut()) {
            file.architecture = archs.next().flatten();
            file.compatible = file.architecture.as_deref().is_some_and(supports);
        }
        Ok(results)
    }

    /// `general.architecture` of a remote GGUF, cached per commit.
    fn architecture(
        &self,
        repo: &str,
        revision: &str,
        file: &str,
    ) -> Result<Option<String>, ModelError> {
        let key = (repo.to_owned(), revision.to_owned(), file.to_owned());
        if let Some(arch) = self.archs.lock().expect("cache lock").get(&key) {
            return Ok(arch.clone());
        }
        let arch = self.sniff(&self.resolve_url(repo, revision, file))?;
        self.archs
            .lock()
            .expect("cache lock")
            .insert(key, arch.clone());
        Ok(arch)
    }

    /// Read just enough of the file to find `general.architecture`, asking
    /// for more bytes while the header is longer than what we have.
    fn sniff(&self, url: &str) -> Result<Option<String>, ModelError> {
        let mut buf = Vec::new();
        let mut want = FIRST_RANGE;
        loop {
            let complete = self.read_prefix(url, &mut buf, want)?;
            match gguf::find_string(&buf, "general.architecture") {
                Scan::Found(arch) => return Ok(Some(arch)),
                Scan::Absent => return Ok(None),
                Scan::Invalid(why) => {
                    log::debug!("{url}: {why}");
                    return Ok(None);
                }
                Scan::NeedMore(need) => {
                    if complete || need > MAX_HEADER {
                        log::debug!("{url}: header ends early or is too large");
                        return Ok(None);
                    }
                    want = need.max(want * 4).min(MAX_HEADER);
                }
            }
        }
    }

    /// Extend `buf` (a prefix of the file) to `want` bytes with a range
    /// request. Returns whether the file ended before `want`.
    fn read_prefix(&self, url: &str, buf: &mut Vec<u8>, want: usize) -> Result<bool, ModelError> {
        let resp = self
            .api
            .get(url)
            .header("Accept-Encoding", "identity")
            .header("Range", format!("bytes={}-{}", buf.len(), want - 1))
            .call()
            .map_err(|e| http(url, &e))?;
        match resp.status().as_u16() {
            206 => {}
            // Range ignored: the body starts at byte 0.
            200 => buf.clear(),
            // Asked past the end: what we have is the whole file.
            416 => return Ok(true),
            status => return Err(ModelError::Http(format!("{url}: HTTP {status}"))),
        }
        let missing = want - buf.len();
        let read = resp
            .into_body()
            .into_reader()
            .take(missing as u64)
            .read_to_end(buf)
            .map_err(|e| ModelError::Http(format!("{url}: {e}")))?;
        Ok(read < missing)
    }
}

pub(crate) fn is_gguf(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gguf"))
}

fn http(url: &str, e: &ureq::Error) -> ModelError {
    ModelError::Http(format!("{url}: {e}"))
}

/// Percent-encode a URL path, keeping `/` separators.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `f` over `items` on up to `workers` threads, results in input order.
fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..workers.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    *slots[i].lock().expect("slot lock") = Some(f(item));
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("slot lock")
                .expect("every slot is filled")
        })
        .collect()
}
