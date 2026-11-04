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

/// Parse a catalog file; exactly one entry must be the default.
pub fn parse_catalog(text: &str) -> anyhow::Result<Vec<CatalogEntry>> {
    let file: CatalogFile = toml::from_str(text)?;
    let defaults = file.model.iter().filter(|m| m.default).count();
    anyhow::ensure!(
        defaults == 1,
        "catalog must have exactly one default model, found {defaults}"
    );
    Ok(file.model)
}

/// Catalog, local store, search and downloads. Shared as `Arc<ModelManager>`.
#[derive(Debug)]
pub struct ModelManager {
    store: PathBuf,
    catalog: Vec<CatalogEntry>,
    hub: Hub,
}

impl ModelManager {
    /// Manager over `store` with the catalog built into this release, talking
    /// to `$EARWIG_HF_ENDPOINT` or `https://huggingface.co`.
    pub fn new(store: PathBuf) -> Self {
        let endpoint = std::env::var(ENDPOINT_ENV)
            .ok()
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| hub::DEFAULT_ENDPOINT.to_owned());
        Self::with_endpoint(store, &endpoint)
    }

    fn with_endpoint(store: PathBuf, endpoint: &str) -> Self {
        let catalog = parse_catalog(BUILTIN_CATALOG).expect("built-in models.toml is valid");
        Self {
            store,
            catalog,
            hub: Hub::new(endpoint),
        }
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

    /// The file for `spec` if it is on disk.
    pub fn resolve(&self, spec: &ModelSpec) -> Result<PathBuf, ModelError> {
        let path = self.path_for(spec)?;
        if path.is_file() {
            Ok(path)
        } else {
            Err(ModelError::NotInstalled(spec.clone()))
        }
    }

    /// Verified model files in the store, curated ones under their catalog
    /// id. Local `Path` models are not listed; they live wherever the config
    /// points.
    pub fn installed(&self) -> Vec<InstalledModel> {
        let mut found = Vec::new();
        if let Err(e) = walk(&self.store, &mut found) {
            log::warn!("listing {}: {e}", self.store.display());
        }
        found.sort();
        found
            .into_iter()
            .filter_map(|path| self.installed_model(path))
            .collect()
    }

    fn installed_model(&self, path: PathBuf) -> Option<InstalledModel> {
        let rel = path.strip_prefix(&self.store).ok()?.to_str()?;
        if !hub::is_gguf(rel) {
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
    /// files return at once; an earlier partial download resumes. The file
    /// is verified against the catalog's sha256 (curated) or the Hub's LFS
    /// sha256 (`hf:`) before it appears at its final path.
    pub fn download(
        &self,
        spec: &ModelSpec,
        progress: &mut dyn FnMut(DownloadProgress),
        cancel: &CancelToken,
    ) -> Result<PathBuf, ModelError> {
        if let ModelSpec::Path { .. } = spec {
            return Err(ModelError::LocalFile(spec.clone()));
        }
        let dest = self.path_for(spec)?;
        if let Ok(meta) = fs::metadata(&dest)
            && meta.is_file()
        {
            progress(DownloadProgress {
                spec: spec.clone(),
                downloaded: meta.len(),
                total: meta.len(),
            });
            return Ok(dest);
        }
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
        download::fetch(
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
        )?;
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
            return Err(ModelError::NotInstalled(spec.clone()));
        }
        // Drop directories left empty, up to the store itself.
        let mut dir = path.parent();
        while let Some(d) = dir.filter(|d| *d != self.store && d.starts_with(&self.store)) {
            if fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
        Ok(())
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

/// Every regular file under `dir`; a missing directory is empty.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            walk(&entry.path(), out)?;
        } else if ty.is_file() {
            out.push(entry.path());
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

    #[test]
    fn builtin_catalog_parses() {
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
