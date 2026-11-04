//! `earwig models ...`: list, search, download, select and remove models.

use std::io::{IsTerminal as _, Write as _};
use std::time::Instant;

use anyhow::Context as _;
use clap::Subcommand;
use earwig_core::config::ConfigStore;
use earwig_core::events::Command;
use earwig_core::models::{CancelToken, DownloadProgress, ModelError, ModelManager, ModelSpec};

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// Curated and installed models; `*` marks the configured one.
    List,
    /// Search Hugging Face for transcribe.cpp models (most downloaded first).
    Search {
        /// Words to match in the repo name; empty lists the top models.
        query: Option<String>,
    },
    /// Download a model into the model store (resumes partial downloads).
    Download {
        /// Catalog id or `hf:owner/repo/file.gguf@<commit>`.
        spec: ModelSpec,
    },
    /// Use a model: writes `model.model` in the user config.
    Use {
        /// Catalog id, `hf:owner/repo/file.gguf@<commit>`, or an absolute path.
        spec: ModelSpec,
    },
    /// Delete a downloaded model from the model store.
    Remove {
        /// Catalog id or `hf:owner/repo/file.gguf@<commit>`.
        spec: ModelSpec,
    },
}

pub fn run(cmd: ModelsCmd) -> anyhow::Result<()> {
    let manager = ModelManager::new(earwig_core::paths::models_dir());
    match cmd {
        ModelsCmd::List => list(&manager),
        ModelsCmd::Search { query } => search(&manager, query.as_deref().unwrap_or_default()),
        ModelsCmd::Download { spec } => download(&manager, &spec),
        ModelsCmd::Use { spec } => select(&manager, &spec),
        ModelsCmd::Remove { spec } => {
            manager.delete(&spec)?;
            println!("removed {spec}");
            Ok(())
        }
    }
}

/// Stand-in until the engine slice lands; replace with
/// `earwig_transcribe::TranscribeEngine::new().supports_architecture(arch)`.
fn engine_supports(arch: &str) -> bool {
    ["parakeet", "nemotron", "whisper"]
        .iter()
        .any(|family| arch.starts_with(family))
}

fn configured(manager: &ModelManager) -> anyhow::Result<ModelSpec> {
    let store = ConfigStore::load()?;
    Ok(store
        .config()
        .model
        .model
        .clone()
        .unwrap_or_else(|| ModelSpec::Curated {
            id: manager.default_model().id.clone(),
        }))
}

fn list(manager: &ModelManager) -> anyhow::Result<()> {
    let current = configured(manager)?;
    let installed = manager.installed();
    let mark = |spec: &ModelSpec| if *spec == current { '*' } else { ' ' };

    println!("Curated models:");
    let id_width = manager
        .catalog()
        .iter()
        .map(|m| m.id.len())
        .max()
        .unwrap_or(0);
    for m in manager.catalog() {
        let spec = ModelSpec::Curated { id: m.id.clone() };
        let state = if installed.iter().any(|i| i.spec == spec) {
            "installed"
        } else {
            "-"
        };
        let default = if m.default { "default" } else { "" };
        println!(
            "{} {:id_width$}  {:>8}  {state:9}  {default:7}  {}",
            mark(&spec),
            m.id,
            human_size(m.size),
            m.name
        );
    }

    let others: Vec<_> = installed
        .iter()
        .filter(|i| i.catalog_id.is_none())
        .collect();
    let local = matches!(current, ModelSpec::Path { .. });
    if !others.is_empty() || local {
        println!("\nOther models:");
        for m in others {
            println!("{} {}  {:>8}", mark(&m.spec), m.spec, human_size(m.size));
        }
        if local {
            let state = if manager.resolve(&current).is_ok() {
                "local file"
            } else {
                "local file, missing"
            };
            println!("* {current}  ({state})");
        }
    }
    println!(
        "\n* = model.model in {}",
        ConfigStore::load()?.paths().user.display()
    );
    Ok(())
}

fn search(manager: &ModelManager, query: &str) -> anyhow::Result<()> {
    let results = manager.search(query, &engine_supports)?;
    if results.is_empty() {
        println!("no transcribe.cpp models match {query:?}");
        return Ok(());
    }
    let rows: Vec<[String; 5]> = results
        .iter()
        .flat_map(|r| {
            r.files.iter().map(|f| {
                [
                    r.repo.clone(),
                    f.file.clone(),
                    human_size(f.size),
                    f.architecture.clone().unwrap_or_else(|| "?".into()),
                    if f.compatible { "yes" } else { "no" }.into(),
                ]
            })
        })
        .collect();
    let header = ["REPO", "FILE", "SIZE", "ARCH", "COMPATIBLE"].map(String::from);
    let mut widths = [0; 5];
    for row in std::iter::once(&header).chain(&rows) {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        let [repo, file, size, arch, ok] = row;
        let [w0, w1, w2, w3, _] = widths;
        println!("{repo:w0$}  {file:w1$}  {size:>w2$}  {arch:w3$}  {ok}");
    }
    println!("\nDownload with `earwig models download hf:<REPO>/<FILE>@<COMMIT>`; commits:");
    for r in &results {
        if let Some(f) = r.files.first() {
            println!("  {}@{}", r.repo, f.revision);
        }
    }
    Ok(())
}

fn download(manager: &ModelManager, spec: &ModelSpec) -> anyhow::Result<()> {
    let cancel = CancelToken::default();
    let on_signal = cancel.clone();
    ctrlc::set_handler(move || on_signal.cancel()).context("installing the Ctrl-C handler")?;

    let mut line = ProgressLine::new();
    match manager.download(spec, &mut |p| line.update(&p), &cancel) {
        Ok(path) => {
            line.finish();
            println!("{}", path.display());
            Ok(())
        }
        Err(ModelError::Cancelled) => {
            line.finish();
            eprintln!("cancelled; the partial download is kept, run the same command to resume");
            std::process::exit(130);
        }
        Err(e) => {
            line.finish();
            Err(e.into())
        }
    }
}

fn select(manager: &ModelManager, spec: &ModelSpec) -> anyhow::Result<()> {
    if let ModelSpec::Curated { id } = spec
        && manager.curated(id).is_none()
    {
        let ids: Vec<_> = manager.catalog().iter().map(|m| m.id.as_str()).collect();
        anyhow::bail!("unknown catalog model {id:?}; known: {}", ids.join(", "));
    }
    let mut store = ConfigStore::load()?;
    if store.is_locked("model.model") {
        anyhow::bail!(
            "model.model is set by the system config ({}); change it there",
            store
                .paths()
                .system
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    store.update(|c| c.model.model = Some(spec.clone()))?;
    println!("model.model = {spec} in {}", store.paths().user.display());
    if manager.resolve(spec).is_ok() {
        // A running daemon switches now; without one there is nothing to do.
        if let Err(e) = crate::daemon::send(Command::ReloadConfig) {
            log::debug!("daemon not told to reload: {e}");
        }
    } else if matches!(spec, ModelSpec::Path { .. }) {
        println!("warning: {spec} does not exist");
    } else {
        println!("not downloaded yet; run `earwig models download {spec}`");
    }
    Ok(())
}

/// One self-overwriting progress line on a terminal, a line per 10%
/// otherwise (logs, CI).
struct ProgressLine {
    tty: bool,
    start: Instant,
    first: Option<u64>,
    printed_tenth: Option<u64>,
    dirty: bool,
}

impl ProgressLine {
    fn new() -> Self {
        Self {
            tty: std::io::stderr().is_terminal(),
            start: Instant::now(),
            first: None,
            printed_tenth: None,
            dirty: false,
        }
    }

    fn update(&mut self, p: &DownloadProgress) {
        let first = *self.first.get_or_insert(p.downloaded);
        let millis = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let rate = p.downloaded.saturating_sub(first) * 1000 / millis.max(1);
        let percent = (p.downloaded * 100).checked_div(p.total).unwrap_or(0);
        let text = format!(
            "{}: {} / {}  {percent}%  {}/s",
            p.spec,
            human_size(p.downloaded),
            human_size(p.total),
            human_size(rate),
        );
        let mut err = std::io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r\x1b[2K{text}");
            let _ = err.flush();
            self.dirty = true;
        } else if self.printed_tenth != Some(percent / 10) {
            self.printed_tenth = Some(percent / 10);
            let _ = writeln!(err, "{text}");
        }
    }

    fn finish(&mut self) {
        if self.dirty {
            eprintln!();
            self.dirty = false;
        }
    }
}

/// Decimal units, as the Hub shows them.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)] // display only
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
