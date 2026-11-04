use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{CatalogEntry, ModelError, ModelSpec};

const BUILTIN_CATALOG: &str = include_str!("../../models.toml");

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
}

impl ModelManager {
    /// Manager over `store` with the catalog built into this release.
    pub fn new(store: PathBuf) -> Self {
        let catalog = parse_catalog(BUILTIN_CATALOG).expect("built-in models.toml is valid");
        Self { store, catalog }
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
                Ok(self.hf_path(&entry.repo, &entry.revision, &entry.file))
            }
            ModelSpec::HuggingFace {
                repo,
                file,
                revision,
            } => Ok(self.hf_path(repo, revision, file)),
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

    fn hf_path(&self, repo: &str, revision: &str, file: &str) -> PathBuf {
        self.store.join(repo).join(revision).join(file)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_catalog_parses() {
        let catalog = super::parse_catalog(super::BUILTIN_CATALOG).unwrap();
        assert!(catalog.iter().all(|m| m.revision.len() == 40 && m.sha256.len() == 64));
    }
}
