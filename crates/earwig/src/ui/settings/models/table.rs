//! The table's rows: the models on disk, the catalog's, Hugging Face's, and which match a search.

use std::fmt::Write as _;
use std::path::Path;

use earwig_core::models::{Benchmark, CatalogEntry, InstalledModel, ModelSpec, Role, SearchResult};

use super::HubSearch;
use super::languages::language_name;

/// Where a row's model file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    Remote,
    /// On disk but not earwig's to delete: a system store or a local file.
    Kept,
    /// In the user's store, where the trash can delete it.
    Stored,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub(super) spec: ModelSpec,
    pub(super) name: String,
    /// The line under the name.
    pub(super) summary: String,
    pub(super) role: Option<Role>,
    /// Native streaming: live text keeps up with the speaker.
    pub(super) streaming: bool,
    /// Language codes it speaks; empty when unknown.
    pub(super) languages: Vec<String>,
    /// It takes a language hint (`[model] language`).
    pub(super) language_hint: bool,
    /// Bytes; 0 when unknown.
    pub(super) size: u64,
    pub(super) place: Place,
    /// Handy's measurements, for the meters; `None` for untested models.
    pub(super) benchmark: Option<Benchmark>,
}

impl Row {
    pub(super) fn curated(entry: &CatalogEntry) -> Self {
        Self {
            spec: curated(entry),
            name: entry.name.clone(),
            summary: entry.summary.clone(),
            role: entry.role,
            streaming: entry.streaming,
            languages: entry.languages.clone(),
            language_hint: entry.language_hint,
            size: entry.size,
            place: Place::Remote,
            benchmark: entry.benchmark,
        }
    }

    /// A file in a Hugging Face repo, named after the file.
    fn hub(repo: &str, file: &str, revision: &str, size: u64) -> Self {
        let spec = ModelSpec::HuggingFace {
            repo: repo.to_owned(),
            file: file.to_owned(),
            revision: revision.to_owned(),
        };
        Self::other(spec, stem(file), repo.to_owned(), size)
    }

    /// A file `config.toml` names by its path.
    fn local(path: &Path) -> Self {
        let size = std::fs::metadata(path).map_or(0, |m| m.len());
        let dir = path
            .parent()
            .map_or_else(String::new, |dir| dir.display().to_string());
        let spec = ModelSpec::Path {
            path: path.to_owned(),
        };
        let mut row = Self::other(spec, stem(&path.to_string_lossy()), dir, size);
        if path.is_file() {
            row.place = Place::Kept;
        }
        row
    }

    fn other(spec: ModelSpec, name: String, summary: String, size: u64) -> Self {
        Self {
            spec,
            name,
            summary,
            role: None,
            streaming: false,
            languages: Vec::new(),
            language_hint: false,
            size,
            place: Place::Remote,
            benchmark: None,
        }
    }

    pub(super) fn multilingual(&self) -> bool {
        self.languages.len() > 1
    }

    /// Every word of `query` is in the name, summary, id, tag, abilities or languages.
    fn matches(&self, query: &str) -> bool {
        let mut words = query.split_whitespace().peekable();
        if words.peek().is_none() {
            return true;
        }
        let mut haystack = String::with_capacity(
            self.name.len() + self.summary.len() + self.languages.len() * 10 + 64,
        );
        let _ = write!(haystack, "{} {} {}", self.name, self.summary, self.spec);
        if let Some(role) = self.role {
            haystack.push(' ');
            haystack.push_str(role_label(role));
        }
        if self.streaming {
            haystack.push_str(" streaming live");
        }
        if self.multilingual() {
            haystack.push_str(" multilingual");
        }
        for code in &self.languages {
            haystack.push(' ');
            haystack.push_str(language_name(code));
        }
        let haystack = haystack.to_lowercase();
        words.all(|word| haystack.contains(&word.to_lowercase()))
    }
}

/// `file`'s name without directories and the `.gguf` extension.
fn stem(file: &str) -> String {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let lower = name.to_ascii_lowercase();
    lower
        .strip_suffix(".gguf")
        .map_or(name, |stem| &name[..stem.len()])
        .to_owned()
}

fn curated(entry: &CatalogEntry) -> ModelSpec {
    ModelSpec::Curated {
        id: entry.id.clone(),
    }
}

/// `spec` named by its catalog id when it is a curated file in `hf:` form.
pub(super) fn canonical(spec: ModelSpec, catalog: &[CatalogEntry]) -> ModelSpec {
    if let ModelSpec::HuggingFace {
        repo,
        file,
        revision,
    } = &spec
        && let Some(entry) = catalog
            .iter()
            .find(|e| e.repo == *repo && e.file == *file && e.revision == *revision)
    {
        return curated(entry);
    }
    spec
}

/// Whether two specs are the same Hugging Face file, whatever the commit.
fn same_file(a: &ModelSpec, b: &ModelSpec) -> bool {
    match (a, b) {
        (
            ModelSpec::HuggingFace { repo, file, .. },
            ModelSpec::HuggingFace {
                repo: other_repo,
                file: other_file,
                ..
            },
        ) => repo == other_repo && file == other_file,
        _ => a == b,
    }
}

fn catalog_index(catalog: &[CatalogEntry], spec: &ModelSpec) -> Option<usize> {
    match spec {
        ModelSpec::Curated { id } => catalog.iter().position(|e| e.id == *id),
        _ => None,
    }
}

/// A row in Yours for the model in use when the store doesn't have it: a local file, a model to
/// download, or an unknown id.
pub(super) fn active_row(
    active: &ModelSpec,
    catalog: &[CatalogEntry],
    installed: &[InstalledModel],
) -> Option<Row> {
    if installed.iter().any(|m| m.spec == *active) {
        return None;
    }
    Some(match active {
        ModelSpec::Path { path } => Row::local(path),
        ModelSpec::HuggingFace {
            repo,
            file,
            revision,
        } => Row::hub(repo, file, revision, 0),
        ModelSpec::Curated { id } => match catalog.iter().find(|e| e.id == *id) {
            Some(entry) => Row::curated(entry),
            None => Row::other(
                active.clone(),
                id.clone(),
                "Not in this release's catalog".into(),
                0,
            ),
        },
    })
}

/// Search results the engine loads and the catalog doesn't list.
pub(super) fn found_rows(results: &[SearchResult], catalog: &[CatalogEntry]) -> Vec<Row> {
    results
        .iter()
        .flat_map(|result| {
            result
                .files
                .iter()
                .filter(|file| {
                    file.compatible
                        && !catalog
                            .iter()
                            .any(|e| e.repo == result.repo && e.file == file.file)
                })
                .map(|file| Row::hub(&result.repo, &file.file, &file.revision, file.size))
        })
        .collect()
}

/// What the search for `query` found, plus untested downloads that match it.
pub(super) fn untested_rows<'a>(
    query: &str,
    hub: &HubSearch,
    downloading: impl Iterator<Item = &'a Row>,
) -> Vec<Row> {
    let mut rows = match hub {
        HubSearch::Done(searched, Ok(rows)) if !query.is_empty() && searched == query => {
            rows.clone()
        }
        _ => Vec::new(),
    };
    for row in downloading {
        let untested = matches!(row.spec, ModelSpec::HuggingFace { .. });
        if untested && row.matches(query) && !rows.iter().any(|r| r.spec == row.spec) {
            rows.push(row.clone());
        }
    }
    rows
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Table {
    /// On disk, the one in use first, then curated ones in catalog order.
    pub(super) yours: Vec<Row>,
    /// Curated, not on disk.
    pub(super) available: Vec<Row>,
    /// Found on Hugging Face, not curated, not on disk.
    pub(super) untested: Vec<Row>,
}

/// The rows matching `query`; `extra` is [`active_row`].
pub(super) fn table(
    catalog: &[CatalogEntry],
    installed: &[InstalledModel],
    active: &ModelSpec,
    extra: Option<Row>,
    query: &str,
    untested: Vec<Row>,
) -> Table {
    let mut yours: Vec<Row> = installed
        .iter()
        .map(|model| {
            let entry = model
                .catalog_id
                .as_deref()
                .and_then(|id| catalog.iter().find(|e| e.id == id));
            let mut row = match (entry, &model.spec) {
                (Some(entry), _) => Row::curated(entry),
                (
                    None,
                    ModelSpec::HuggingFace {
                        repo,
                        file,
                        revision,
                    },
                ) => Row::hub(repo, file, revision, 0),
                (None, _) => Row::local(&model.path),
            };
            row.size = model.size;
            row.place = if model.system {
                Place::Kept
            } else {
                Place::Stored
            };
            row
        })
        .collect();
    yours.extend(extra);
    let rank = |row: &Row| {
        (
            row.spec != *active,
            catalog_index(catalog, &row.spec).unwrap_or(usize::MAX),
        )
    };
    yours.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));
    let available = catalog
        .iter()
        .map(Row::curated)
        .filter(|row| !yours.iter().any(|r| r.spec == row.spec) && row.matches(query))
        .collect();
    let untested = untested
        .into_iter()
        .filter(|row| !yours.iter().any(|r| same_file(&r.spec, &row.spec)))
        .collect();
    yours.retain(|row| row.matches(query));
    Table {
        yours,
        available,
        untested,
    }
}

/// The tag of one of Handy's picks, as Handy words it.
pub(super) fn role_label(role: Role) -> &'static str {
    match role {
        Role::Recommended => "Recommended",
        Role::Faster => "Faster",
        Role::Accurate => "More accurate",
    }
}

#[cfg(test)]
mod tests {
    use earwig_core::models::RemoteFile;

    use super::*;
    use crate::ui::settings::models::tests::{catalog, spec};

    fn installed(entry: &CatalogEntry) -> InstalledModel {
        InstalledModel {
            spec: curated(entry),
            path: format!("/store/{}/{}", entry.repo, entry.file).into(),
            size: entry.size,
            catalog_id: Some(entry.id.clone()),
            system: false,
        }
    }

    fn hub_file(repo: &str, file: &str) -> InstalledModel {
        InstalledModel {
            spec: ModelSpec::HuggingFace {
                repo: repo.into(),
                file: file.into(),
                revision: "c0".into(),
            },
            path: format!("/store/{repo}/c0/{file}").into(),
            size: 7,
            catalog_id: None,
            system: false,
        }
    }

    fn remote(file: &str, compatible: bool) -> RemoteFile {
        RemoteFile {
            file: file.into(),
            size: 9,
            architecture: Some("parakeet".into()),
            compatible,
            revision: "c1".into(),
        }
    }

    fn ids(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|row| row.spec.to_string()).collect()
    }

    #[test]
    fn yours_lists_the_model_in_use_first_then_in_catalog_order() {
        let catalog = catalog();
        let on_disk = [
            hub_file("someone/whisper-gguf", "whisper-Q8_0.gguf"),
            installed(&catalog[3]),
            installed(&catalog[1]),
            installed(&catalog[0]),
        ];
        let table = table(
            &catalog,
            &on_disk,
            &curated(&catalog[3]),
            None,
            "",
            Vec::new(),
        );
        assert_eq!(
            ids(&table.yours),
            [
                catalog[3].id.as_str(),
                &catalog[0].id,
                &catalog[1].id,
                "hf:someone/whisper-gguf/whisper-Q8_0.gguf@c0",
            ]
        );
        assert!(table.yours.iter().all(|row| row.place == Place::Stored));
        assert_eq!(table.yours[3].name, "whisper-Q8_0");
        // The rest of the catalog, in its order.
        assert_eq!(
            ids(&table.available),
            [catalog[2].id.clone(), catalog[4].id.clone()]
        );
        assert!(table.available.iter().all(|row| row.place == Place::Remote));
        assert_eq!(ids(&table.untested), Vec::<String>::new());
    }

    #[test]
    fn a_model_in_use_that_nothing_lists_still_gets_a_row() {
        let catalog = catalog();
        let local = ModelSpec::Path {
            path: "/nowhere/mine.gguf".into(),
        };
        let extra = active_row(&local, &catalog, &[]).unwrap();
        assert_eq!((extra.name.as_str(), extra.place), ("mine", Place::Remote));
        let on_disk = [installed(&catalog[0])];
        let table = table(&catalog, &on_disk, &local, Some(extra), "", Vec::new());
        assert_eq!(table.yours[0].spec, local);

        // A curated model in use but not downloaded heads Yours, not Available, to be downloaded.
        let wanted = curated(&catalog[2]);
        let extra = active_row(&wanted, &catalog, &[]);
        let listed = super::table(&catalog, &on_disk, &wanted, extra, "", Vec::new());
        assert_eq!(
            (&listed.yours[0].spec, listed.yours[0].place),
            (&wanted, Place::Remote)
        );
        assert!(!listed.available.iter().any(|row| row.spec == wanted));
        assert_eq!(
            active_row(&wanted, &catalog, &[installed(&catalog[2])]),
            None
        );
    }

    #[test]
    fn a_curated_file_in_hf_form_is_the_curated_model() {
        let catalog = catalog();
        let entry = &catalog[1];
        let hf = |revision: &str| ModelSpec::HuggingFace {
            repo: entry.repo.clone(),
            file: entry.file.clone(),
            revision: revision.into(),
        };
        assert_eq!(canonical(hf(&entry.revision), &catalog), curated(entry));
        assert_eq!(canonical(hf("older"), &catalog), hf("older"));
    }

    #[test]
    fn the_search_box_filters_by_name_tag_ability_and_language() {
        let catalog = catalog();
        let rows =
            |query| ids(&table(&catalog, &[], &spec("none"), None, query, Vec::new()).available);
        assert_eq!(rows("").len(), catalog.len());
        assert_eq!(rows("   ").len(), catalog.len());
        assert_eq!(rows("ULTRA"), ["parakeet-ultra"]);
        assert_eq!(rows("more accurate"), ["Qwen3-ASR-1.7B"]);
        assert_eq!(
            rows("japanese streaming"),
            ["nemotron-3.5-asr-streaming-0.6b"]
        );
        assert_eq!(rows("klingon"), Vec::<String>::new());
    }

    #[test]
    fn untested_models_show_only_while_searching() {
        let catalog = catalog();
        let entry = &catalog[0];
        let results = [SearchResult {
            repo: entry.repo.clone(),
            files: vec![
                // The catalog's own file is its row, not an untested one.
                remote(&entry.file, true),
                remote("parakeet-unified-en-0.6b-Q4_K_M.gguf", true),
                remote("parakeet-unified-en-0.6b-mmproj.gguf", false),
            ],
        }];
        let found = found_rows(&results, &catalog);
        assert_eq!(ids(&found).len(), 1);
        assert_eq!(
            (found[0].name.as_str(), found[0].size),
            ("parakeet-unified-en-0.6b-Q4_K_M", 9)
        );

        let downloading = found[0].clone();
        let search = HubSearch::Done("parakeet".to_owned(), Ok(found));
        let shown = |query, downloads: &[&Row]| {
            untested_rows(query, &search, downloads.iter().copied()).len()
        };
        assert_eq!(shown("parakeet", &[]), 1);
        assert_eq!(shown("", &[]), 0);
        assert_eq!(
            shown("parakeet unified", &[]),
            0,
            "that search was for another query"
        );
        // A running download stays listed while the box is empty.
        assert_eq!(shown("", &[&downloading]), 1);
        assert_eq!(shown("whisper", &[&downloading]), 0);

        // Once on disk, a file is Yours, at whatever commit it was found.
        let on_disk = [hub_file(
            &entry.repo,
            "parakeet-unified-en-0.6b-Q4_K_M.gguf",
        )];
        let table = table(
            &catalog,
            &on_disk,
            &spec("none"),
            None,
            "parakeet",
            vec![downloading],
        );
        assert_eq!(ids(&table.untested), Vec::<String>::new());
        assert_eq!(table.yours.len(), 1);
    }

    #[test]
    fn files_are_named_without_directories_or_extension() {
        assert_eq!(stem("sub/dir/model-Q8_0.gguf"), "model-Q8_0");
        assert_eq!(stem("MODEL.GGUF"), "MODEL");
        assert_eq!(stem(r"C:\models\mine.gguf"), "mine");
        assert_eq!(stem("notes.bin"), "notes.bin");
    }
}
