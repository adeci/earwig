use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::download::{self, Source};
use super::hub::{self, Hub};
use super::{
    CancelToken, CatalogEntry, DownloadProgress, InstalledModel, ModelError, ModelSpec,
    SearchResult, is_plain_relative,
};

const BUILTIN_CATALOG: &str = include_str!("../../models.toml");

/// Overrides the Hugging Face base URL for every request (mirrors, tests).
const ENDPOINT_ENV: &str = "EARWIG_HF_ENDPOINT";

#[derive(Deserialize)]
struct CatalogFile {
    model: Vec<CatalogEntry>,
}

/// Parse a catalog file; exactly one entry must be the default, and every
/// benchmark must hold numbers a benchmark can produce.
fn parse_catalog(text: &str) -> anyhow::Result<Vec<CatalogEntry>> {
    let file: CatalogFile = toml::from_str(text)?;
    let defaults = file.model.iter().filter(|m| m.default).count();
    anyhow::ensure!(
        defaults == 1,
        "catalog must have exactly one default model, found {defaults}"
    );
    if let Some(m) = file
        .model
        .iter()
        .find(|m| m.benchmark.is_some_and(|b| !b.is_valid()))
    {
        anyhow::bail!(
            "benchmark of {}: the error must be 0 to 100 and both speeds above 0",
            m.id
        );
    }
    Ok(file.model)
}

/// Catalog, local store, search and downloads. Shared as `Arc<ModelManager>`.
///
/// Models are looked up in the user's store first, then in read-only system
/// stores (`earwig/models` under each `$XDG_DATA_DIRS` entry) with the same
/// `<owner>/<repo>/<commit>/<file>` layout. NixOS installs pinned models
/// there, so they work offline without locking the model choice.
#[derive(Debug)]
pub struct ModelManager {
    store: PathBuf,
    system: Vec<PathBuf>,
    catalog: Vec<CatalogEntry>,
    hub: Hub,
}

impl ModelManager {
    /// Manager over `store` (plus the system stores) with the catalog built
    /// into this release, talking to `$EARWIG_HF_ENDPOINT` or
    /// `https://huggingface.co`.
    pub fn new(store: PathBuf) -> Self {
        let endpoint = std::env::var(ENDPOINT_ENV)
            .ok()
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| hub::DEFAULT_ENDPOINT.to_owned());
        Self::with_endpoint(store, &endpoint).with_system_stores(crate::paths::system_model_dirs())
    }

    fn with_endpoint(store: PathBuf, endpoint: &str) -> Self {
        let catalog = parse_catalog(BUILTIN_CATALOG).expect("built-in models.toml is valid");
        Self {
            store,
            system: Vec::new(),
            catalog,
            hub: Hub::new(endpoint),
        }
    }

    /// Read-only stores searched after the user's, in order.
    #[must_use]
    pub fn with_system_stores(mut self, stores: Vec<PathBuf>) -> Self {
        self.system = stores;
        self
    }

    pub fn store(&self) -> &Path {
        &self.store
    }

    pub fn catalog(&self) -> &[CatalogEntry] {
        &self.catalog
    }

    pub fn default_model(&self) -> &CatalogEntry {
        self.catalog
            .iter()
            .find(|m| m.default)
            .expect("catalog has a default")
    }

    pub fn curated(&self, id: &str) -> Option<&CatalogEntry> {
        self.catalog.iter().find(|m| m.id == id)
    }

    /// The `[model] model` setting that selects `spec`: none for the
    /// catalog's default, so that a later release's new default reaches the
    /// user too.
    pub fn setting_for(&self, spec: ModelSpec) -> Option<ModelSpec> {
        match &spec {
            ModelSpec::Curated { id } if *id == self.default_model().id => None,
            _ => Some(spec),
        }
    }

    /// Where a spec's file lives: `<store>/<owner>/<repo>/<commit>/<file>`
    /// for Hugging Face files (curated ids map to theirs), the path itself
    /// for local files. Says nothing about whether it exists.
    pub fn path_for(&self, spec: &ModelSpec) -> Result<PathBuf, ModelError> {
        match spec {
            ModelSpec::Curated { id } => {
                let entry = self
                    .curated(id)
                    .ok_or_else(|| ModelError::UnknownCurated(id.clone()))?;
                self.hf_path(&entry.repo, &entry.revision, &entry.file)
            }
            ModelSpec::HuggingFace {
                repo,
                file,
                revision,
            } => self.hf_path(repo, revision, file),
            ModelSpec::Path { path } => Ok(path.clone()),
        }
    }

    /// The file for `spec` if it is on disk: the user's store first, then the
    /// system stores.
    pub fn resolve(&self, spec: &ModelSpec) -> Result<PathBuf, ModelError> {
        let path = self.path_for(spec)?;
        if path.is_file() {
            return Ok(path);
        }
        if let Ok(rel) = path.strip_prefix(&self.store) {
            for root in &self.system {
                let candidate = root.join(rel);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err(ModelError::NotInstalled(spec.clone()))
    }

    /// Verified model files in the user's store and the system stores,
    /// curated ones under their catalog id; a model in both is listed once,
    /// from the user's store. Local `Path` models are not listed; they live
    /// wherever the config points.
    pub fn installed(&self) -> Vec<InstalledModel> {
        let mut models: Vec<InstalledModel> = Vec::new();
        let roots =
            std::iter::once((&self.store, false)).chain(self.system.iter().map(|r| (r, true)));
        for (root, system) in roots {
            let mut found = Vec::new();
            if let Err(e) = walk(root, &mut found) {
                log::warn!("listing {}: {e}", root.display());
            }
            found.sort();
            for model in found
                .into_iter()
                .filter_map(|path| self.installed_model(root, path, system))
            {
                if !models.iter().any(|m| m.spec == model.spec) {
                    models.push(model);
                }
            }
        }
        models
    }

    fn installed_model(&self, root: &Path, path: PathBuf, system: bool) -> Option<InstalledModel> {
        let rel = path.strip_prefix(root).ok()?.to_str()?;
        // Specs name files with `/` on every OS.
        let rel: std::borrow::Cow<str> = if cfg!(windows) {
            rel.replace('\\', "/").into()
        } else {
            rel.into()
        };
        if !hub::is_gguf(&rel) {
            return None;
        }
        let mut parts = rel.splitn(4, '/');
        let (owner, name, revision, file) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        let repo = format!("{owner}/{name}");
        let size = fs::metadata(&path).ok()?.len();
        let curated = self
            .catalog
            .iter()
            .find(|m| m.repo == repo && m.revision == revision && m.file == file);
        let spec = match curated {
            Some(entry) => ModelSpec::Curated {
                id: entry.id.clone(),
            },
            None => ModelSpec::HuggingFace {
                repo,
                file: file.to_owned(),
                revision: revision.to_owned(),
            },
        };
        Some(InstalledModel {
            spec,
            path,
            size,
            catalog_id: curated.map(|m| m.id.clone()),
            system,
        })
    }

    /// Search Hugging Face for repos tagged `transcribe.cpp`; an empty query
    /// lists the most downloaded. Each GGUF's architecture is read from its
    /// header and checked with `supports` (the engine's
    /// `supports_architecture`).
    pub fn search(
        &self,
        query: &str,
        supports: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<SearchResult>, ModelError> {
        self.hub.search(query, supports)
    }

    /// Download `spec` into the store and return its path. Already present
    /// files return at once; a failed download resumes where it stopped,
    /// and a cancelled one leaves nothing behind. The file is verified
    /// against the catalog's sha256 (curated) or the Hub's LFS sha256
    /// (`hf:`) before it appears at its final path.
    pub fn download(
        &self,
        spec: &ModelSpec,
        progress: &mut dyn FnMut(DownloadProgress),
        cancel: &CancelToken,
    ) -> Result<PathBuf, ModelError> {
        if let ModelSpec::Path { .. } = spec {
            return Err(ModelError::LocalFile(spec.clone()));
        }
        if let Ok(found) = self.resolve(spec) {
            let size = fs::metadata(&found).map_or(0, |m| m.len());
            progress(DownloadProgress {
                spec: spec.clone(),
                downloaded: size,
                total: size,
            });
            return Ok(found);
        }
        let dest = self.path_for(spec)?;
        let (repo, revision, file, sha256, size) = match spec {
            ModelSpec::Path { .. } => unreachable!("handled above"),
            ModelSpec::Curated { id } => {
                let m = self
                    .curated(id)
                    .ok_or_else(|| ModelError::UnknownCurated(id.clone()))?;
                (
                    m.repo.clone(),
                    m.revision.clone(),
                    m.file.clone(),
                    m.sha256.clone(),
                    m.size,
                )
            }
            ModelSpec::HuggingFace {
                repo,
                file,
                revision,
            } => {
                let entry = self
                    .hub
                    .tree(repo, revision)?
                    .into_iter()
                    .find(|e| e.kind == "file" && &e.path == file)
                    .ok_or_else(|| {
                        ModelError::Http(format!("{repo} has no file {file} at {revision}"))
                    })?;
                let lfs = entry.lfs.ok_or_else(|| {
                    ModelError::Http(format!("{repo}/{file} has no LFS sha256 to verify against"))
                })?;
                (
                    repo.clone(),
                    revision.clone(),
                    file.clone(),
                    lfs.oid,
                    entry.size,
                )
            }
        };
        let url = self.hub.resolve_url(&repo, &revision, &file);
        let src = Source {
            url: &url,
            sha256: &sha256,
            size: Some(size),
        };
        let fetched = download::fetch(
            self.hub.files_agent(),
            &src,
            &dest,
            &mut |downloaded, total| {
                progress(DownloadProgress {
                    spec: spec.clone(),
                    downloaded,
                    total,
                });
            },
            cancel,
        );
        if matches!(fetched, Err(ModelError::Cancelled)) {
            self.prune(&dest);
        }
        fetched?;
        log::info!("downloaded {spec} to {}", dest.display());
        Ok(dest)
    }

    /// Delete a downloaded model (and any partial download of it) from the
    /// store. Local `Path` models are never touched.
    pub fn delete(&self, spec: &ModelSpec) -> Result<(), ModelError> {
        if let ModelSpec::Path { .. } = spec {
            return Err(ModelError::LocalFile(spec.clone()));
        }
        let path = self.path_for(spec)?;
        let mut removed = false;
        for file in [path.clone(), download::part_path(&path)] {
            match fs::remove_file(&file) {
                Ok(()) => removed = true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if !removed {
            return Err(if self.resolve(spec).is_ok() {
                ModelError::ReadOnly(spec.clone())
            } else {
                ModelError::NotInstalled(spec.clone())
            });
        }
        self.prune(&path);
        Ok(())
    }

    /// Drop the directories above `file` left empty, up to the store itself.
    fn prune(&self, file: &Path) {
        let mut dir = file.parent();
        while let Some(d) = dir.filter(|d| *d != self.store && d.starts_with(&self.store)) {
            if fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }

    fn hf_path(&self, repo: &str, revision: &str, file: &str) -> Result<PathBuf, ModelError> {
        if !is_plain_relative(repo)
            || repo.split('/').count() != 2
            || !is_plain_relative(file)
            || !is_plain_relative(revision)
            || revision.contains('/')
        {
            return Err(ModelError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{repo}/{file}@{revision} does not name a file inside the model store"),
            )));
        }
        Ok(self.store.join(repo).join(revision).join(file))
    }
}

/// Every regular file under `dir`, following symlinks (system stores are
/// Nix link farms); a missing directory is empty.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        // A dangling link is skipped.
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, out)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::models::{Benchmark, Role, Tier};

    #[test]
    fn builtin_catalog_parses_with_roles() {
        let catalog = super::parse_catalog(super::BUILTIN_CATALOG).unwrap();
        assert!(
            catalog
                .iter()
                .all(|m| m.revision.len() == 40 && m.sha256.len() == 64)
        );
        let mut ids: Vec<_> = catalog.iter().map(|m| &m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), catalog.len(), "catalog ids are unique");
        // Each tag marks one model, and the default is the recommended one.
        for role in [Role::Recommended, Role::Faster, Role::Accurate] {
            let tagged = catalog.iter().filter(|m| m.role == Some(role)).count();
            assert_eq!(tagged, 1, "{role:?}");
        }
        let default = catalog.iter().find(|m| m.default).unwrap();
        assert_eq!(default.role, Some(Role::Recommended));
    }

    /// A catalog of one default model, with `extra` lines at its end.
    fn one_model(extra: &str) -> String {
        format!(
            r#"[[model]]
id = "m"
name = "M"
summary = "s"
repo = "o/r"
file = "m.gguf"
revision = "c"
sha256 = "x"
size = 1
license = "mit"
languages = ["en"]
streaming = false
default = true
{extra}"#
        )
    }

    #[test]
    fn roles_are_optional_and_spelled_out() {
        let role = |text: &str| super::parse_catalog(&one_model(text)).map(|c| c[0].role);
        assert_eq!(role("").unwrap(), None);
        assert_eq!(role(r#"role = "faster""#).unwrap(), Some(Role::Faster));
        assert_eq!(role(r#"role = "accurate""#).unwrap(), Some(Role::Accurate));
        assert!(role(r#"role = "best""#).is_err());
    }

    #[test]
    fn builtin_catalog_carries_handys_benchmarks() {
        let catalog = super::parse_catalog(super::BUILTIN_CATALOG).unwrap();
        let measured = |english_error, gpu_speed, cpu_speed| Benchmark {
            english_error,
            gpu_speed,
            cpu_speed,
        };
        // Handy's numbers (models.toml names the source), then the grade and
        // the accuracy, GPU speed and CPU speed steps they come to.
        let expected = [
            (measured(3.99, 25.28, 13.955), ("A−", 5, 5, 4)),
            (measured(6.11, 76.455, 50.03), ("B+", 4, 5, 5)),
            (measured(3.23, 4.085, 2.47), ("A", 5, 2, 1)),
            (measured(4.26, 23., 13.29), ("A−", 5, 5, 4)),
            (measured(7.9, 17.135, 10.59), ("B", 4, 4, 4)),
        ];
        let ids: Vec<&str> = catalog.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "parakeet-unified-en-0.6b",
                "parakeet-tdt-ctc-110m",
                "Qwen3-ASR-1.7B",
                "parakeet-ultra",
                "nemotron-3.5-asr-streaming-0.6b",
            ]
        );
        for (entry, (benchmark, graded)) in catalog.iter().zip(expected) {
            assert_eq!(entry.benchmark, Some(benchmark), "{}", entry.id);
            let shown = (
                benchmark.grade(),
                benchmark.accuracy_steps(),
                benchmark.speed_steps(Tier::Gpu),
                benchmark.speed_steps(Tier::Cpu),
            );
            assert_eq!(shown, graded, "{}", entry.id);
        }
    }

    #[test]
    fn benchmarks_are_optional_and_checked() {
        let benchmark = |text: &str| super::parse_catalog(&one_model(text)).map(|c| c[0].benchmark);
        assert_eq!(benchmark("").unwrap(), None);
        let table = "[model.benchmark]\nenglish_error = 3.5\ngpu_speed = 20\ncpu_speed = 10.5";
        assert_eq!(
            benchmark(table).unwrap(),
            Some(Benchmark {
                english_error: 3.5,
                gpu_speed: 20.,
                cpu_speed: 10.5,
            })
        );
        assert!(benchmark("[model.benchmark]\nenglish_error = 3.5\ngpu_speed = 20").is_err());
        assert!(benchmark(&table.replace("= 10.5", "= 0")).is_err());
    }

    #[test]
    fn choosing_the_default_model_sets_none() {
        let manager = ModelManager::with_endpoint("/nowhere".into(), "http://127.0.0.1:9");
        let curated = |m: &CatalogEntry| ModelSpec::Curated { id: m.id.clone() };
        assert_eq!(manager.setting_for(curated(manager.default_model())), None);
        let other = curated(manager.catalog().iter().find(|m| !m.default).unwrap());
        assert_eq!(manager.setting_for(other.clone()), Some(other));
        let file = ModelSpec::Path {
            path: "/models/m.gguf".into(),
        };
        assert_eq!(manager.setting_for(file.clone()), Some(file));
    }

    /// A one-repo fake Hub: the tree listing and the file itself.
    fn fake_hub(data: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let tree = serde_json::json!([
            {"type": "directory", "path": "sub", "size": 0},
            {"type": "file", "path": "sub/m.gguf", "size": data.len(),
             "lfs": {"oid": hex::encode(Sha256::digest(data)), "size": data.len()}},
        ])
        .to_string();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let mut conn = conn.unwrap();
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                // Headers are ignored, but read so closing does not reset.
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
                let body: &[u8] = if path == "/api/models/o/r/tree/c?recursive=true" {
                    tree.as_bytes()
                } else if path == "/o/r/resolve/c/sub/m.gguf" {
                    data
                } else {
                    let _ = conn.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                };
                let _ = write!(
                    conn,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(body);
            }
        });
        base
    }

    #[test]
    fn hf_download_lands_in_the_store_and_lists_and_deletes() {
        static DATA: &[u8] = b"GGUF\x03\0\0\0 not really a model";
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("models");
        let manager = ModelManager::with_endpoint(store.clone(), &fake_hub(DATA));
        let spec: ModelSpec = "hf:o/r/sub/m.gguf@c".parse().unwrap();

        let mut last = None;
        let path = manager
            .download(&spec, &mut |p| last = Some(p), &CancelToken::default())
            .unwrap();
        assert_eq!(path, store.join("o/r/c/sub/m.gguf"));
        assert_eq!(fs::read(&path).unwrap(), DATA);
        assert_eq!(last.unwrap().downloaded, DATA.len() as u64);
        assert_eq!(manager.resolve(&spec).unwrap(), path);

        // A curated model's path is reported under its id.
        let curated = manager.default_model().clone();
        let curated_path = manager
            .path_for(&ModelSpec::Curated {
                id: curated.id.clone(),
            })
            .unwrap();
        fs::create_dir_all(curated_path.parent().unwrap()).unwrap();
        fs::write(&curated_path, b"x").unwrap();
        fs::write(download::part_path(&curated_path), b"partial").unwrap();

        let installed = manager.installed();
        let specs: Vec<String> = installed.iter().map(|m| m.spec.to_string()).collect();
        assert_eq!(specs.len(), 2, "{specs:?}");
        assert!(specs.contains(&spec.to_string()));
        assert!(specs.contains(&curated.id));
        let curated_entry = installed.iter().find(|m| m.spec.to_string() == curated.id);
        assert_eq!(
            curated_entry.unwrap().catalog_id.as_deref(),
            Some(curated.id.as_str())
        );

        manager.delete(&spec).unwrap();
        assert!(!path.exists());
        assert!(!store.join("o").exists(), "empty directories are pruned");
        assert!(store.exists());
        assert!(matches!(
            manager.delete(&spec),
            Err(ModelError::NotInstalled(_))
        ));
    }

    #[test]
    fn a_cancelled_download_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("models");
        let manager = ModelManager::with_endpoint(store.clone(), &fake_hub(b"GGUF"));
        let spec: ModelSpec = "hf:o/r/sub/m.gguf@c".parse().unwrap();
        let cancel = CancelToken::default();
        cancel.cancel();
        let err = manager.download(&spec, &mut |_| {}, &cancel).unwrap_err();
        assert!(matches!(err, ModelError::Cancelled), "{err}");
        assert!(
            !store.join("o").exists(),
            "no partial file, no empty directories"
        );
    }

    #[test]
    fn system_stores_serve_models_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        let system = dir.path().join("system");
        // Nothing listens here: a system model must not need the network.
        let manager = ModelManager::with_endpoint(user.clone(), "http://127.0.0.1:9")
            .with_system_stores(vec![dir.path().join("missing"), system.clone()]);
        let default = manager.default_model().clone();
        let spec = ModelSpec::Curated {
            id: default.id.clone(),
        };
        let pinned = system
            .join(&default.repo)
            .join(&default.revision)
            .join(&default.file);
        fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        fs::write(&pinned, b"pinned").unwrap();

        assert_eq!(manager.resolve(&spec).unwrap(), pinned);
        let installed = manager.installed();
        assert_eq!(installed.len(), 1);
        assert!(installed[0].system);
        assert_eq!(
            installed[0].catalog_id.as_deref(),
            Some(default.id.as_str())
        );
        let got = manager
            .download(&spec, &mut |_| {}, &CancelToken::default())
            .unwrap();
        assert_eq!(got, pinned);
        assert!(matches!(
            manager.delete(&spec),
            Err(ModelError::ReadOnly(_))
        ));
        assert!(pinned.exists());

        // A user copy shadows the system one and is the one listed.
        let own = manager.path_for(&spec).unwrap();
        fs::create_dir_all(own.parent().unwrap()).unwrap();
        fs::write(&own, b"mine").unwrap();
        assert_eq!(manager.resolve(&spec).unwrap(), own);
        let installed = manager.installed();
        assert_eq!(installed.len(), 1);
        assert!(!installed[0].system);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_system_stores_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        let manager = ModelManager::with_endpoint(dir.path().join("user"), "http://127.0.0.1:9")
            .with_system_stores(vec![system.clone()]);
        let default = manager.default_model().clone();
        // Like a Nix link farm: a symlinked repo directory holding a
        // symlinked file.
        let target = dir.path().join("nix-store");
        fs::create_dir_all(target.join("files")).unwrap();
        fs::write(target.join("blob"), b"pinned").unwrap();
        let revision = target.join("files").join(&default.revision);
        fs::create_dir_all(revision.join(&default.file).parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target.join("blob"), revision.join(&default.file)).unwrap();
        let repo = system.join(&default.repo);
        fs::create_dir_all(repo.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target.join("files"), &repo).unwrap();

        let installed = manager.installed();
        assert_eq!(installed.len(), 1, "{installed:?}");
        assert!(installed[0].system);
        assert_eq!(
            installed[0].catalog_id.as_deref(),
            Some(default.id.as_str())
        );
        assert_eq!(installed[0].size, 6);
    }

    #[test]
    fn missing_remote_file_is_an_error_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let manager = ModelManager::with_endpoint(dir.path().into(), &fake_hub(b"x"));
        let spec: ModelSpec = "hf:o/r/other.gguf@c".parse().unwrap();
        let err = manager
            .download(&spec, &mut |_| {}, &CancelToken::default())
            .unwrap_err();
        assert!(matches!(err, ModelError::Http(_)), "{err}");
        assert_eq!(manager.installed(), []);
    }

    #[test]
    fn local_paths_are_never_downloaded_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("mine.gguf");
        fs::write(&outside, b"keep").unwrap();
        let manager = ModelManager::with_endpoint(dir.path().join("models"), "http://127.0.0.1:9");
        let spec = ModelSpec::Path {
            path: outside.clone(),
        };
        assert!(matches!(
            manager.delete(&spec),
            Err(ModelError::LocalFile(_))
        ));
        assert!(matches!(
            manager.download(&spec, &mut |_| {}, &CancelToken::default()),
            Err(ModelError::LocalFile(_))
        ));
        assert!(outside.exists());
    }

    #[test]
    fn specs_that_escape_the_store_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let manager = ModelManager::with_endpoint(dir.path().join("models"), "http://127.0.0.1:9");
        let spec = ModelSpec::HuggingFace {
            repo: "o/r".into(),
            file: "../../../x.gguf".into(),
            revision: "c".into(),
        };
        assert!(manager.path_for(&spec).is_err());
        assert!(manager.delete(&spec).is_err());
    }
}
