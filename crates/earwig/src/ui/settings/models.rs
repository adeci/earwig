//! Models page: one table under a search box. Yours (on disk, the one in use
//! first), then the curated models not downloaded yet, each with Handy's
//! accuracy and speed meters. Typing filters the table; a row at its end
//! searches Hugging Face for the query when clicked, and lists the untested
//! models it finds. Clicking a row uses that model, downloading it first if
//! needed; the last row clicked wins.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use earwig_core::config::{BackendSetting, Config, ModelConfig};
use earwig_core::events::ModelStatus;
use earwig_core::models::benchmark::{DICTATION_SECONDS, GRADE_CEILINGS, SPEED_CUTOFFS, STEPS};
use earwig_core::models::{
    Benchmark, CancelToken, CatalogEntry, DownloadProgress, InstalledModel, ModelError, ModelSpec,
    Role, SearchResult, Tier,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::progress::{Progress, ProgressCircle};
use gpui_kit::component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, AnyView, App, AppContext as _, Context, Entity, FontFeatures, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use super::{LOCKED_HINT, is_locked, spaced};
use crate::ui::{UiState, edit_config};
use crate::{human_size, human_size_of};

/// How often a running download moves its bar.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// How often the amount under a download's bar changes, so its digits
/// don't flicker.
const TEXT_INTERVAL: Duration = Duration::from_secs(1);
/// The Language choice that leaves the language to the model.
const DETECT_LANGUAGE: &str = "\u{0}detect";
/// A row's download mark and trash can follow the pointer on the row.
const ROW_GROUP: &str = "model-row";
/// What the sidebar search finds the page by.
const KEYWORDS: [&str; 4] = ["models", "downloads", "languages", "hugging face"];
/// Width of the accuracy, speed and size columns at a row's end.
const COLUMN: Pixels = px(56.);
/// Width of the cancel or delete button after them.
const SLOT: Pixels = px(24.);
/// A meter's dots, and the room between two.
const DOT: Pixels = px(6.);
const DOT_GAP: Pixels = px(3.);
/// Where a meter's or column's explanation wraps.
const TIP_WIDTH: Pixels = px(300.);
/// Digits of one width, so a changing amount doesn't jitter.
static TABULAR: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));

pub struct ModelsPage {
    installed: Vec<InstalledModel>,
    downloads: HashMap<ModelSpec, Download>,
    picks: Picks,
    /// The row asking whether to delete its model.
    deleting: Option<ModelSpec>,
    /// Why the last download or delete of a model failed, shown on its row.
    errors: HashMap<ModelSpec, String>,
    search_input: Entity<InputState>,
    /// The search box's text, trimmed.
    query: String,
    hub: HubSearch,
    _subscriptions: Vec<Subscription>,
}

struct Download {
    /// The row it started from, so an untested model stays listed.
    row: Row,
    downloaded: u64,
    /// 0 while unknown.
    total: u64,
    /// The amount the row says, `downloaded` as of [`TEXT_INTERVAL`] ago at
    /// most, and when it was taken.
    said: u64,
    said_at: Option<Instant>,
    cancel: CancelToken,
}

impl Download {
    fn new(row: Row, cancel: CancelToken) -> Self {
        Self {
            row,
            downloaded: 0,
            total: 0,
            said: 0,
            said_at: None,
            cancel,
        }
    }

    /// The bar follows every update; the amount said follows once a
    /// [`TEXT_INTERVAL`], and at once when the last byte is in.
    fn progress(&mut self, downloaded: u64, total: u64, now: Instant) {
        self.downloaded = downloaded;
        self.total = total;
        let due = self
            .said_at
            .is_none_or(|at| now.duration_since(at) >= TEXT_INTERVAL);
        if due || self.complete() {
            self.said = downloaded;
            self.said_at = Some(now);
        }
    }

    /// Every byte is in: the checksum is being verified before the file
    /// moves into place, which ends the download.
    fn complete(&self) -> bool {
        self.total > 0 && self.downloaded >= self.total
    }

    /// No share of a known size to show: nothing in yet, no size known, or
    /// verifying.
    fn indeterminate(&self) -> bool {
        self.total == 0 || self.complete()
    }

    /// What the row says under the bar.
    fn line(&self) -> String {
        if self.complete() {
            "Verifying the download".into()
        } else if self.total > 0 {
            human_size_of(self.said, self.total)
        } else if self.said > 0 {
            format!("{} so far", human_size(self.said))
        } else {
            "Starting the download".into()
        }
    }
}

enum DownloadMsg {
    Progress(DownloadProgress),
    Done(ModelSpec, Result<(), ModelError>),
}

impl ModelsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search models")
                .clean_on_escape()
        });
        let subscriptions = vec![cx.subscribe(
            &search_input,
            |this, input, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().trim().to_owned();
                    this.set_query(query, cx);
                }
                InputEvent::PressEnter { .. } => this.search_hub(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        )];
        Self {
            installed: UiState::handle(cx).models().installed(),
            downloads: HashMap::new(),
            picks: Picks::default(),
            deleting: None,
            errors: HashMap::new(),
            search_input,
            query: String::new(),
            hub: HubSearch::default(),
            _subscriptions: subscriptions,
        }
    }

    fn refresh_installed(&mut self, cx: &App) {
        self.installed = UiState::handle(cx).models().installed();
    }

    /// A row was clicked: use its model, or download it and use it then.
    fn click(&mut self, row: &Row, active: &ModelSpec, locked: bool, cx: &mut Context<Self>) {
        cx.notify();
        // A click anywhere answers an open "Delete?" with no.
        if self.deleting.take().is_some() {
            return;
        }
        self.errors.remove(&row.spec);
        let clicked = if self.downloads.contains_key(&row.spec) {
            Clicked::Downloading
        } else if row.place == Place::Remote {
            Clicked::Remote
        } else if row.spec == *active {
            Clicked::InUse
        } else {
            Clicked::OnDisk
        };
        match self.picks.click(&row.spec, clicked, locked) {
            Click::Use => use_model(row.spec.clone(), cx),
            Click::Download => self.download(row.clone(), cx),
            Click::Nothing => {}
        }
    }

    fn download(&mut self, row: Row, cx: &mut Context<Self>) {
        let spec = row.spec.clone();
        let cancel = CancelToken::default();
        self.downloads
            .insert(spec.clone(), Download::new(row, cancel.clone()));
        let models = UiState::handle(cx).models();
        let (tx, rx) = flume::unbounded();
        let thread_spec = spec.clone();
        let spawned = std::thread::Builder::new()
            .name("earwig-download".into())
            .spawn(move || {
                let mut last = Instant::now();
                let result = models.download(
                    &thread_spec,
                    &mut |progress| {
                        // The last byte always gets through: it starts the
                        // verifying.
                        let complete = progress.total > 0 && progress.downloaded >= progress.total;
                        if complete || last.elapsed() >= PROGRESS_INTERVAL {
                            last = Instant::now();
                            let _ = tx.send(DownloadMsg::Progress(progress));
                        }
                    },
                    &cancel,
                );
                let _ = tx.send(DownloadMsg::Done(thread_spec, result.map(drop)));
            });
        if let Err(e) = spawned {
            self.downloads.remove(&spec);
            self.picks.dropped(&spec);
            self.errors
                .insert(spec, format!("Could not start the download: {e}"));
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv_async().await {
                if this
                    .update(cx, |this, cx| this.on_download(msg, cx))
                    .is_err()
                {
                    return;
                }
            }
            // The channel closed without `Done`: the download thread panicked.
            let _ = this.update(cx, |this, cx| {
                if this.downloads.remove(&spec).is_some() {
                    this.picks.dropped(&spec);
                    this.errors
                        .insert(spec, "The download stopped unexpectedly.".into());
                    this.refresh_installed(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn on_download(&mut self, msg: DownloadMsg, cx: &mut Context<Self>) {
        match msg {
            DownloadMsg::Progress(progress) => {
                if let Some(download) = self.downloads.get_mut(&progress.spec) {
                    download.progress(progress.downloaded, progress.total, Instant::now());
                }
            }
            DownloadMsg::Done(spec, result) => {
                self.downloads.remove(&spec);
                self.refresh_installed(cx);
                match result {
                    Ok(()) => {
                        if self.picks.finished(&spec) {
                            use_model(spec, cx);
                        }
                    }
                    Err(ModelError::Cancelled) => self.picks.dropped(&spec),
                    Err(e) => {
                        self.picks.dropped(&spec);
                        self.errors.insert(spec, format!("Download failed: {e}"));
                    }
                }
            }
        }
        cx.notify();
    }

    fn cancel(&mut self, spec: &ModelSpec, cx: &mut Context<Self>) {
        if let Some(download) = self.downloads.get(spec) {
            download.cancel.cancel();
        }
        self.picks.dropped(spec);
        cx.notify();
    }

    fn delete(&mut self, spec: &ModelSpec, cx: &mut Context<Self>) {
        self.deleting = None;
        if let Err(e) = UiState::handle(cx).models().delete(spec) {
            self.errors
                .insert(spec.clone(), format!("Could not delete it: {e}"));
        }
        self.refresh_installed(cx);
        cx.notify();
    }

    /// The search box changed: the rows filter, and a Hugging Face search
    /// for anything else is forgotten.
    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query == self.query {
            return;
        }
        self.hub.typed(&query);
        self.query = query;
        cx.notify();
    }

    /// Search Hugging Face for the query in the box: its row at the end of
    /// the table was clicked, or Enter pressed.
    fn search_hub(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.hub.start(&self.query) else {
            return;
        };
        cx.notify();
        let handle = UiState::handle(cx);
        let models = handle.models();
        let engine = handle.engine();
        let (tx, rx) = flume::bounded(1);
        let thread_query = query.clone();
        let spawned = std::thread::Builder::new()
            .name("earwig-search".into())
            .spawn(move || {
                let supports = |arch: &str| engine.supports_architecture(arch);
                let _ = tx.send(models.search(&thread_query, &supports));
            });
        if let Err(e) = spawned {
            let failed = Err(format!("Could not start the search: {e}"));
            self.hub.finished(query, failed);
            return;
        }
        cx.spawn(async move |this, cx| {
            if let Ok(found) = rx.recv_async().await {
                let _ = this.update(cx, |this, cx| {
                    let models = UiState::handle(cx).models();
                    let rows = found
                        .map(|results| found_rows(&results, models.catalog()))
                        .map_err(|e| e.to_string());
                    this.hub.finished(query, rows);
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// The Hugging Face search at the end of the table. Typing only filters the
/// table; Hugging Face is asked when its row is clicked or Enter pressed,
/// for two characters or more, and a new query offers the search again.
#[derive(Debug, Default, PartialEq)]
enum HubSearch {
    /// Not asked for the query in the box.
    #[default]
    Offered,
    /// Running for this query.
    Running(String),
    /// What the search for this query found, or why it failed.
    Done(String, Result<Vec<Row>, String>),
}

impl HubSearch {
    /// The box holds `query` now: a search for any other is forgotten.
    fn typed(&mut self, query: &str) {
        if let Self::Running(searched) | Self::Done(searched, _) = self
            && searched != query
        {
            *self = Self::Offered;
        }
    }

    /// Asked to search for `query`: the query to send, unless it is too
    /// short, or its search is running or has found something already.
    fn start(&mut self, query: &str) -> Option<String> {
        if !searchable(query) {
            return None;
        }
        match self {
            Self::Running(searched) | Self::Done(searched, Ok(_)) if searched == query => None,
            _ => {
                *self = Self::Running(query.to_owned());
                Some(query.to_owned())
            }
        }
    }

    /// The search for `query` ended; dropped when the box moved on.
    fn finished(&mut self, query: String, found: Result<Vec<Row>, String>) {
        if matches!(self, Self::Running(searched) if *searched == query) {
            *self = Self::Done(query, found);
        }
    }

    /// Whether its search for `query` is running.
    fn running(&self, query: &str) -> bool {
        matches!(self, Self::Running(searched) if searched == query)
    }
}

/// Whether `query` is worth a Hugging Face search: two characters or more.
fn searchable(query: &str) -> bool {
    query.chars().nth(1).is_some()
}

/// Use `spec` from now on: the config names it and the daemon reloads.
fn use_model(spec: ModelSpec, cx: &mut App) {
    let model = UiState::handle(cx).models().setting_for(spec);
    edit_config(cx, |c| c.model.model = model);
}

/// A clicked row's model, as far as the click goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clicked {
    /// On disk and in use.
    InUse,
    OnDisk,
    Downloading,
    /// Not on disk.
    Remote,
}

/// What clicking a row does.
#[derive(Debug, PartialEq, Eq)]
enum Click {
    /// Use the model now.
    Use,
    /// Download it; it is used when that is done, unless another row was
    /// clicked since.
    Download,
    Nothing,
}

/// The model a finished download switches to: the last row clicked, as long
/// as that one was not on disk yet.
#[derive(Debug, Default)]
struct Picks {
    target: Option<ModelSpec>,
}

impl Picks {
    fn click(&mut self, spec: &ModelSpec, clicked: Clicked, locked: bool) -> Click {
        if locked {
            // The system's configuration chooses; downloads still work.
            return match clicked {
                Clicked::Remote => Click::Download,
                _ => Click::Nothing,
            };
        }
        match clicked {
            Clicked::InUse => {
                self.target = None;
                Click::Nothing
            }
            Clicked::OnDisk => {
                self.target = None;
                Click::Use
            }
            Clicked::Downloading => {
                self.target = Some(spec.clone());
                Click::Nothing
            }
            Clicked::Remote => {
                self.target = Some(spec.clone());
                Click::Download
            }
        }
    }

    /// `spec` finished downloading: whether to use it now.
    fn finished(&mut self, spec: &ModelSpec) -> bool {
        let wanted = self.is_target(spec);
        if wanted {
            self.target = None;
        }
        wanted
    }

    /// `spec`'s download failed or was cancelled: nothing to switch to.
    fn dropped(&mut self, spec: &ModelSpec) {
        if self.is_target(spec) {
            self.target = None;
        }
    }

    fn is_target(&self, spec: &ModelSpec) -> bool {
        self.target.as_ref() == Some(spec)
    }
}

/// Where a row's model file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Remote,
    /// On disk but not earwig's to delete: a system store or a local file.
    Kept,
    /// In the user's store, where the trash can delete it.
    Stored,
}

/// One model in the table.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    spec: ModelSpec,
    name: String,
    /// The line under the name.
    summary: String,
    role: Option<Role>,
    /// Native streaming: live text keeps up with the speaker.
    streaming: bool,
    /// Language codes it speaks; empty when unknown.
    languages: Vec<String>,
    /// It takes a language hint (`[model] language`).
    language_hint: bool,
    /// Bytes; 0 when unknown.
    size: u64,
    place: Place,
    /// Handy's measurements, for the meters; `None` for untested models.
    benchmark: Option<Benchmark>,
}

impl Row {
    fn curated(entry: &CatalogEntry) -> Self {
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

    fn multilingual(&self) -> bool {
        self.languages.len() > 1
    }

    /// Whether every word of `query` is in the row's name, summary, id, tag,
    /// abilities or language names, ignoring case.
    fn matches(&self, query: &str) -> bool {
        let mut haystack = format!("{} {} {}", self.name, self.summary, self.spec);
        if let Some(role) = self.role {
            haystack = format!("{haystack} {}", role_label(role));
        }
        if self.streaming {
            haystack.push_str(" streaming live");
        }
        if self.multilingual() {
            haystack.push_str(" multilingual");
        }
        for code in &self.languages {
            haystack = format!("{haystack} {}", language_name(code));
        }
        let haystack = haystack.to_lowercase();
        query
            .split_whitespace()
            .all(|word| haystack.contains(&word.to_lowercase()))
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
fn canonical(spec: ModelSpec, catalog: &[CatalogEntry]) -> ModelSpec {
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

/// The row of the model in use when nothing else lists it: a local file,
/// a Hugging Face file not on disk, or an id this release doesn't know.
fn active_row(
    active: &ModelSpec,
    catalog: &[CatalogEntry],
    installed: &[InstalledModel],
) -> Option<Row> {
    if installed.iter().any(|m| m.spec == *active) || catalog_index(catalog, active).is_some() {
        return None;
    }
    Some(match active {
        ModelSpec::Path { path } => Row::local(path),
        ModelSpec::HuggingFace {
            repo,
            file,
            revision,
        } => Row::hub(repo, file, revision, 0),
        ModelSpec::Curated { id } => Row::other(
            active.clone(),
            id.clone(),
            "Not in this release's catalog".into(),
            0,
        ),
    })
}

/// The untested rows of a search: files the engine loads that the catalog
/// doesn't list already.
fn found_rows(results: &[SearchResult], catalog: &[CatalogEntry]) -> Vec<Row> {
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

/// The Untested rows for `query`: what the search for it found, while it is
/// in the box, and untested models still downloading that match it.
fn untested_rows<'a>(
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

/// The table's rows, section by section.
#[derive(Debug, Default, PartialEq)]
struct Table {
    /// On disk, the one in use first, then curated ones in catalog order.
    yours: Vec<Row>,
    /// Curated, not on disk.
    available: Vec<Row>,
    /// Found on Hugging Face, not curated, not on disk.
    untested: Vec<Row>,
}

/// The rows matching `query`; `extra` is [`active_row`].
fn table(
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

pub fn page(view: &Entity<ModelsPage>, settings: &Config, cx: &App) -> SettingPage {
    let view = view.clone();
    let model = settings.model.clone();
    let locks = Locks {
        model: is_locked(cx, "model.model"),
        language: is_locked(cx, "model.language"),
    };
    SettingPage::new("Models")
        .icon(IconName::Cpu)
        .resettable(false)
        .groups(spaced(vec![
            SettingGroup::new().variant(GroupBoxVariant::Normal).item(
                SettingItem::render(move |_, _, cx| {
                    render(view.read(cx), &view, &model, locks, cx)
                })
                .keywords(KEYWORDS),
            ),
        ]))
}

/// Which model settings the system configuration sets.
#[derive(Clone, Copy)]
struct Locks {
    model: bool,
    language: bool,
}

/// What the rows of one render share.
struct Frame<'a> {
    page: &'a ModelsPage,
    view: &'a Entity<ModelsPage>,
    /// The model in use, by its catalog id when it has one.
    active: &'a ModelSpec,
    status: &'a ModelStatus,
    catalog: &'a [CatalogEntry],
    language: Option<&'a str>,
    locks: Locks,
    /// Which of Handy's speeds the speed dots show.
    tier: Tier,
}

fn render(
    page: &ModelsPage,
    view: &Entity<ModelsPage>,
    model: &ModelConfig,
    locks: Locks,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let models = UiState::handle(cx).models();
    let catalog = models.catalog();
    let configured = model
        .model
        .clone()
        .unwrap_or_else(|| curated(models.default_model()));
    let active = canonical(configured, catalog);
    let untested = untested_rows(
        &page.query,
        &page.hub,
        page.downloads.values().map(|d| &d.row),
    );
    let extra = active_row(&active, catalog, &page.installed);
    let table = table(
        catalog,
        &page.installed,
        &active,
        extra,
        &page.query,
        untested,
    );
    let status = &UiState::status(cx).model;
    let frame = Frame {
        page,
        view,
        active: &active,
        status,
        catalog,
        language: model.language.as_deref(),
        locks,
        tier: tier(status, model.backend),
    };
    let rows = |rows: &[Row]| -> Vec<AnyElement> {
        rows.iter().map(|row| render_row(&frame, row, cx)).collect()
    };
    let query = page.query.as_str();
    let searching = !query.is_empty();
    let muted = theme.muted_foreground;
    let show_yours = !table.yours.is_empty() || !searching;
    let yours = show_yours.then(|| {
        v_flex()
            .child(section_title("Yours", Some(frame.tier), cx))
            .when(table.yours.is_empty(), |this| {
                this.child(note(
                    "No models yet. Click one below to download it and use it.",
                    muted,
                ))
            })
            .children(rows(&table.yours))
    });
    let available = (!table.available.is_empty()).then(|| {
        let columns = (!show_yours).then_some(frame.tier);
        v_flex()
            .child(section_title("Available", columns, cx))
            .children(rows(&table.available))
    });
    let nothing = (searching && !show_yours && table.available.is_empty())
        .then(|| note(format!("Nothing here matches “{query}”."), muted));
    let found = matches!(&page.hub, HubSearch::Done(searched, Ok(_)) if searched == query);
    let untested = (!table.untested.is_empty() || searching && found).then(|| {
        v_flex()
            .child(section_title("Untested", None, cx))
            .child(note(
                untested_note(query, found, table.untested.is_empty()),
                muted,
            ))
            .children(rows(&table.untested))
    });
    v_flex()
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .child(search_box(page, cx))
        .when(locks.model, |this| {
            this.child(div().pt_2().child(note(LOCKED_HINT, muted)))
        })
        .child(
            v_flex()
                .w_full()
                .p_1()
                .children(yours)
                .children(available)
                .children(nothing)
                .children(hub_row(page, view, cx))
                .children(untested),
        )
        .into_any_element()
}

/// The search box across the table's top.
fn search_box(page: &ModelsPage, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(theme.border)
        .child(
            Input::new(&page.search_input)
                .appearance(false)
                .cleanable(true)
                .prefix(
                    Icon::new(IconName::Search)
                        .small()
                        .text_color(theme.muted_foreground),
                ),
        )
}

/// Which of Handy's speeds fits this computer: the backend the model in use
/// runs on, or before one is loaded, the configured one.
fn tier(status: &ModelStatus, backend: BackendSetting) -> Tier {
    match status {
        ModelStatus::Ready { backend, .. } if backend == "cpu" => Tier::Cpu,
        ModelStatus::Ready { .. } => Tier::Gpu,
        _ if backend == BackendSetting::Cpu => Tier::Cpu,
        _ => Tier::Gpu,
    }
}

/// The row at the end of the table while a query of two characters or more
/// is typed: search Hugging Face for it, or the search running. Gone once
/// the search found something; back, with why, when it failed.
fn hub_row(page: &ModelsPage, view: &Entity<ModelsPage>, cx: &App) -> Option<AnyElement> {
    let query = page.query.as_str();
    let failed = match &page.hub {
        HubSearch::Done(searched, found) if searched == query => found.as_ref().err(),
        _ => None,
    };
    let found = matches!(&page.hub, HubSearch::Done(searched, Ok(_)) if searched == query);
    if !searchable(query) || found {
        return None;
    }
    let theme = cx.theme();
    let running = page.hub.running(query);
    let (mark, text) = if running {
        (
            ProgressCircle::new("hub-search-running")
                .loading(true)
                .size(px(16.))
                .into_any_element(),
            format!("Searching Hugging Face for “{query}”…"),
        )
    } else {
        (
            Icon::new(IconName::Search)
                .size(px(14.))
                .text_color(theme.muted_foreground)
                .into_any_element(),
            format!("Search Hugging Face for “{query}”"),
        )
    };
    let hover = theme.list_hover;
    let row = h_flex()
        .id("hub-search")
        .w_full()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .when(!running, |this| {
            let view = view.clone();
            this.cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(move |_, _, cx| view.update(cx, ModelsPage::search_hub))
        })
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(mark),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(running, |this| this.text_color(theme.muted_foreground))
                .child(text),
        );
    Some(
        v_flex()
            .child(row)
            .when_some(failed, |this, e| {
                let why = format!("Hugging Face could not be searched: {e}");
                this.child(note(why, theme.danger))
            })
            .into_any_element(),
    )
}

/// The small print under the Untested title.
fn untested_note(query: &str, found: bool, empty: bool) -> SharedString {
    if !found {
        "Downloading from Hugging Face.".into()
    } else if empty {
        format!("Nothing on Hugging Face matches “{query}”.").into()
    } else {
        "From Hugging Face, not tested with earwig: they may be slower or make more mistakes."
            .into()
    }
}

/// A section's title. The first one also heads the accuracy and speed
/// columns, whose legends show on hover.
fn section_title(title: &'static str, columns: Option<Tier>, cx: &App) -> impl IntoElement {
    h_flex()
        .px_2()
        .pt_3()
        .pb_1()
        .gap_1()
        .text_xs()
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(div().flex_1().min_w_0().child(title))
        .when_some(columns, |this, tier| {
            this.child(column_head("accuracy-head", "Accuracy", accuracy_legend))
                .child(column_head("speed-head", "Speed", move || {
                    speed_legend(tier)
                }))
                .child(div().w(COLUMN).flex_shrink_0())
                .child(div().w(SLOT).flex_shrink_0())
        })
}

fn column_head(
    id: &'static str,
    label: &'static str,
    legend: impl Fn() -> String + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(COLUMN)
        .flex_shrink_0()
        .text_center()
        .child(label)
        .tooltip(move |window, cx| wrapped_tip(legend(), window, cx))
}

/// A tooltip for a sentence or two, wrapped at [`TIP_WIDTH`].
fn wrapped_tip(text: String, window: &mut Window, cx: &mut App) -> AnyView {
    Tooltip::element(move |_, _| div().max_w(TIP_WIDTH).child(text.clone())).build(window, cx)
}

/// What the accuracy dots stand for, by Handy's grades.
fn accuracy_legend() -> String {
    let bands: Vec<String> = GRADE_CEILINGS
        .iter()
        .zip(["A", "B", "C", "D"])
        .zip((2..=STEPS).rev())
        .map(|((ceiling, grade), dots)| format!("{dots} under {ceiling}% ({grade})"))
        .collect();
    format!(
        "Accuracy in English, from Handy's tests. Dots by the share of words it gets wrong: {}, 1 beyond (F).",
        bands.join(", ")
    )
}

/// What the speed dots stand for on `tier`.
fn speed_legend(tier: Tier) -> String {
    let bands: Vec<String> = SPEED_CUTOFFS
        .iter()
        .zip((1..=STEPS).rev())
        .map(|(cutoff, dots)| format!("{dots} under {cutoff} s"))
        .collect();
    format!(
        "Speed on {}, from Handy's tests. Dots by the time {DICTATION_SECONDS} s of speech take: {}. This computer may differ.",
        on(tier),
        bands.join(", ")
    )
}

/// The machine Handy timed a `tier`'s speed on.
fn on(tier: Tier) -> &'static str {
    match tier {
        Tier::Gpu => "a laptop's graphics chip",
        Tier::Cpu => "a laptop's processor",
    }
}

fn note(text: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div()
        .px_2()
        .pb_1()
        .text_xs()
        .text_color(color)
        .child(text.into())
}

/// The tag of one of Handy's picks, as Handy words it.
fn role_label(role: Role) -> &'static str {
    match role {
        Role::Recommended => "Recommended",
        Role::Faster => "Faster",
        Role::Accurate => "More accurate",
    }
}

fn element_id(kind: &str, spec: &ModelSpec) -> SharedString {
    format!("{kind}-{spec}").into()
}

fn render_row(frame: &Frame, row: &Row, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let in_use = row.place != Place::Remote && row.spec == *frame.active;
    let asking = frame.page.deleting.as_ref() == Some(&row.spec);
    let download = frame.page.downloads.get(&row.spec);
    let (line, line_color) = row_line(frame, row, asking, cx);
    let language = (in_use && row.language_hint && row.multilingual())
        .then(|| language_picker(&row.languages, frame.language, frame.locks.language));
    let hover = theme.list_hover;
    h_flex()
        .id(element_id("row", &row.spec))
        .group(ROW_GROUP)
        .w_full()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .when(in_use, |this| this.bg(theme.list_active))
        .when(!in_use && !asking, |this| {
            this.cursor_pointer().hover(move |style| style.bg(hover))
        })
        .on_click({
            let view = frame.view.clone();
            let row = row.clone();
            let active = frame.active.clone();
            let locked = frame.locks.model;
            move |_, _, cx| view.update(cx, |page, cx| page.click(&row, &active, locked, cx))
        })
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(row_mark(frame, row, in_use, cx)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(row_name(row, asking, cx))
                .children(download.map(|download| {
                    Progress::new(element_id("bar", &row.spec))
                        .value(percent(download.downloaded, download.total))
                        .loading(download.indeterminate())
                        .xsmall()
                        .my_1()
                }))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(line_color)
                        .when(download.is_some(), |this| {
                            this.font_features(TABULAR.clone())
                        })
                        .child(line),
                )
                .children(language),
        )
        .child(if asking {
            delete_question(frame.view, &row.spec)
        } else {
            row_values(frame, row, in_use, cx)
        })
        .into_any_element()
}

/// The line under a row's name: its summary, or what is happening to it.
fn row_line(frame: &Frame, row: &Row, asking: bool, cx: &App) -> (String, Hsla) {
    let theme = cx.theme();
    let page = frame.page;
    if asking {
        return (
            format!("Delete? It frees {}.", human_size(row.size)),
            theme.foreground,
        );
    }
    if let Some(error) = page.errors.get(&row.spec) {
        return (error.clone(), theme.danger);
    }
    if let Some(download) = page.downloads.get(&row.spec) {
        let then = if page.picks.is_target(&row.spec) {
            ", then switching to it"
        } else {
            ""
        };
        return (format!("{}{then}", download.line()), theme.muted_foreground);
    }
    if canonical(status_model(frame.status).clone(), frame.catalog) == row.spec {
        match frame.status {
            ModelStatus::Ready { .. } => {}
            ModelStatus::Loading { .. } => return ("Loading…".into(), theme.muted_foreground),
            ModelStatus::Missing { .. } => {
                return ("In use, but not downloaded yet.".into(), theme.warning);
            }
            ModelStatus::Failed { message, .. } => {
                return (format!("Could not load it: {message}"), theme.danger);
            }
        }
    }
    (row.summary.clone(), theme.muted_foreground)
}

fn status_model(status: &ModelStatus) -> &ModelSpec {
    match status {
        ModelStatus::Missing { model }
        | ModelStatus::Loading { model }
        | ModelStatus::Ready { model, .. }
        | ModelStatus::Failed { model, .. } => model,
    }
}

/// A spinning ring while downloading, else checked in use, empty on disk,
/// and a download mark when not on disk yet.
fn row_mark(frame: &Frame, row: &Row, in_use: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    if frame.page.downloads.contains_key(&row.spec) {
        // The bar under the name shows how far; the ring shows it's moving.
        return ProgressCircle::new(element_id("progress", &row.spec))
            .loading(true)
            .size(px(18.))
            .into_any_element();
    }
    let mark = div()
        .size(px(18.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center();
    if in_use {
        mark.bg(theme.primary)
            .child(
                Icon::new(IconName::Check)
                    .size(px(12.))
                    .text_color(theme.primary_foreground),
            )
            .into_any_element()
    } else if row.place == Place::Remote {
        let hovered = theme.foreground;
        mark.border_1()
            .border_dashed()
            .border_color(theme.muted_foreground)
            .text_color(theme.muted_foreground)
            .group_hover(ROW_GROUP, move |style| {
                style.border_color(hovered).text_color(hovered)
            })
            .child(Icon::new(IconName::ArrowDown).size(px(11.)))
            .into_any_element()
    } else {
        mark.border_1()
            .border_color(theme.muted_foreground)
            .into_any_element()
    }
}

/// The bold name, the pick's tag and the ability icons.
fn row_name(row: &Row, asking: bool, cx: &App) -> impl IntoElement {
    let streams =
        "Streams: the words show as you speak, and the text is ready the moment you stop.";
    h_flex()
        .min_w_0()
        .gap_1p5()
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_semibold()
                .child(row.name.clone()),
        )
        .when_some(row.role.filter(|_| !asking), |this, role| {
            this.child(
                div()
                    .flex_shrink_0()
                    .child(Tag::secondary().small().child(role_label(role))),
            )
        })
        .when(row.streaming, |this| {
            this.child(ability(
                element_id("streams", &row.spec),
                IconName::Radio,
                streams.into(),
                cx,
            ))
        })
        .when(row.multilingual(), |this| {
            this.child(ability(
                element_id("languages", &row.spec),
                IconName::Languages,
                format!("{} languages", row.languages.len()).into(),
                cx,
            ))
        })
}

/// An ability icon beside the name, explained by its tooltip.
fn ability(id: SharedString, icon: IconName, tip: SharedString, cx: &App) -> impl IntoElement {
    div()
        .id(id)
        .flex_shrink_0()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(icon).size(px(13.)))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

/// Handy's accuracy and speed dots, the size, then × for a running download
/// or the trash can (shown while the pointer is on the row) for a model in
/// the user's store.
fn row_values(frame: &Frame, row: &Row, in_use: bool, cx: &App) -> AnyElement {
    let view = frame.view;
    let spec = &row.spec;
    let slot = if frame.page.downloads.contains_key(spec) {
        Some(
            Button::new(element_id("cancel", spec))
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .tooltip("Stop downloading")
                .on_click(row_action(view, spec, ModelsPage::cancel))
                .into_any_element(),
        )
    } else if row.place == Place::Stored && !in_use {
        let trash = Button::new(element_id("trash", spec))
            .icon(IconName::Trash)
            .ghost()
            .xsmall()
            .tooltip("Delete")
            .on_click(row_action(view, spec, |page, spec, cx| {
                page.deleting = Some(spec.clone());
                cx.notify();
            }));
        Some(
            div()
                .opacity(0.)
                .group_hover(ROW_GROUP, |style| style.opacity(1.))
                .child(trash)
                .into_any_element(),
        )
    } else {
        None
    };
    let [accuracy, speed] = meters(row, frame.tier, cx);
    h_flex()
        .flex_shrink_0()
        .gap_1()
        .child(accuracy)
        .child(speed)
        .child(
            div()
                .w(COLUMN)
                .text_right()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .when(row.size > 0, |this| this.child(human_size(row.size))),
        )
        .child(div().size(SLOT).children(slot))
        .into_any_element()
}

/// Handy's accuracy and speed dots for `row`, or the room they take when
/// Handy hasn't measured it.
fn meters(row: &Row, tier: Tier, cx: &App) -> [AnyElement; 2] {
    let Some(benchmark) = row.benchmark else {
        return [(); 2].map(|()| div().w(COLUMN).flex_shrink_0().into_any_element());
    };
    [
        meter(
            element_id("accuracy", &row.spec),
            benchmark.accuracy_steps(),
            move || {
                format!(
                    "{} in English: {:.1}% of words wrong in Handy's tests",
                    benchmark.grade(),
                    benchmark.english_error
                )
            },
            cx,
        ),
        meter(
            element_id("speed", &row.spec),
            benchmark.speed_steps(tier),
            move || {
                format!(
                    "{DICTATION_SECONDS} s of speech in {:.1} s on {}, in Handy's tests",
                    benchmark.seconds(tier),
                    on(tier)
                )
            },
            cx,
        ),
    ]
}

/// `steps` of [`STEPS`] dots filled, explained by `tip` on hover.
fn meter(id: SharedString, steps: u8, tip: impl Fn() -> String + 'static, cx: &App) -> AnyElement {
    let filled = cx.theme().muted_foreground;
    let empty = filled.opacity(0.25);
    h_flex()
        .id(id)
        .w(COLUMN)
        .flex_shrink_0()
        .justify_center()
        .gap(DOT_GAP)
        .children((0..STEPS).map(|step| {
            div()
                .size(DOT)
                .rounded_full()
                .bg(if step < steps { filled } else { empty })
        }))
        .tooltip(move |window, cx| wrapped_tip(tip(), window, cx))
        .into_any_element()
}

/// "Delete?" answered: Cancel or Delete.
fn delete_question(view: &Entity<ModelsPage>, spec: &ModelSpec) -> AnyElement {
    h_flex()
        .flex_shrink_0()
        .gap_1()
        .child(
            Button::new(element_id("keep", spec))
                .label("Cancel")
                .ghost()
                .xsmall()
                .on_click(row_action(view, spec, |page, _, cx| {
                    page.deleting = None;
                    cx.notify();
                })),
        )
        .child(
            Button::new(element_id("delete", spec))
                .label("Delete")
                .danger()
                .xsmall()
                .on_click(row_action(view, spec, ModelsPage::delete)),
        )
        .into_any_element()
}

/// A click handler for a button on a row: runs `action` for the row's model
/// and keeps the click from reaching the row.
fn row_action(
    view: &Entity<ModelsPage>,
    spec: &ModelSpec,
    action: impl Fn(&mut ModelsPage, &ModelSpec, &mut Context<ModelsPage>) + 'static,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    let spec = spec.clone();
    move |_, _, cx| {
        cx.stop_propagation();
        view.update(cx, |page, cx| action(page, &spec, cx));
    }
}

/// `downloaded` of `total` in percent, 0 while the total is unknown.
#[allow(clippy::cast_precision_loss)]
fn percent(downloaded: u64, total: u64) -> f32 {
    if total == 0 {
        0.
    } else {
        downloaded as f32 * 100. / total as f32
    }
}

/// The Language line of the model in use, for a model that takes a hint.
fn language_picker(languages: &[String], configured: Option<&str>, locked: bool) -> AnyElement {
    let options = language_options(languages, configured);
    let current = SharedString::from(configured.unwrap_or(DETECT_LANGUAGE).to_owned());
    let label = options
        .iter()
        .find(|(value, _)| *value == current)
        .map_or_else(|| current.clone(), |(_, label)| label.clone());
    let menu_item = move |value: &SharedString, label: &SharedString, checked: bool| {
        let value = value.clone();
        PopupMenuItem::new(label.clone())
            .checked(checked)
            .on_click(move |_, _, cx| {
                let language = (value != DETECT_LANGUAGE).then(|| value.to_string());
                edit_config(cx, |c| c.model.language = language);
            })
    };
    h_flex()
        .pt_1()
        .gap_2()
        .text_xs()
        .child("Language")
        .child(
            Button::new("language")
                .label(label)
                .dropdown_caret(true)
                .outline()
                .xsmall()
                .disabled(locked)
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    options
                        .iter()
                        .fold(menu, |menu, (value, label)| {
                            menu.item(menu_item(value, label, *value == current))
                        })
                        .scrollable(true)
                }),
        )
        .into_any_element()
}

/// Detect automatically, then `languages` by name, then the `configured`
/// one when the model does not list it (the engine ignores it then).
fn language_options(
    languages: &[String],
    configured: Option<&str>,
) -> Vec<(SharedString, SharedString)> {
    let mut named: Vec<(SharedString, SharedString)> = languages
        .iter()
        .map(|code| (code.clone().into(), language_name(code).to_owned().into()))
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    let mut options = vec![(DETECT_LANGUAGE.into(), "Detect automatically".into())];
    options.extend(named);
    if let Some(code) = configured
        && !languages.iter().any(|language| language == code)
    {
        options.push((
            code.to_owned().into(),
            format!("{} (not for this model)", language_name(code)).into(),
        ));
    }
    options
}

/// The English name of a language code the catalog uses; any other code as
/// it is.
fn language_name(code: &str) -> &str {
    match code {
        "ar" => "Arabic",
        "bg" => "Bulgarian",
        "cs" => "Czech",
        "da" => "Danish",
        "de" => "German",
        "el" => "Greek",
        "en" => "English",
        "es" => "Spanish",
        "et" => "Estonian",
        "fa" => "Persian",
        "fi" => "Finnish",
        "fil" => "Filipino",
        "fr" => "French",
        "hi" => "Hindi",
        "hr" => "Croatian",
        "hu" => "Hungarian",
        "id" => "Indonesian",
        "it" => "Italian",
        "ja" => "Japanese",
        "ko" => "Korean",
        "lt" => "Lithuanian",
        "lv" => "Latvian",
        "mk" => "Macedonian",
        "ms" => "Malay",
        "mt" => "Maltese",
        "nb" => "Norwegian",
        "nl" => "Dutch",
        "pl" => "Polish",
        "pt" => "Portuguese",
        "ro" => "Romanian",
        "ru" => "Russian",
        "sk" => "Slovak",
        "sl" => "Slovenian",
        "sv" => "Swedish",
        "th" => "Thai",
        "tr" => "Turkish",
        "uk" => "Ukrainian",
        "vi" => "Vietnamese",
        "yue" => "Cantonese",
        "zh" => "Chinese",
        _ => code,
    }
}

#[cfg(test)]
mod tests {
    use earwig_core::models::{ModelManager, RemoteFile};

    use super::*;

    fn catalog() -> Vec<CatalogEntry> {
        ModelManager::new(std::env::temp_dir()).catalog().to_vec()
    }

    fn spec(id: &str) -> ModelSpec {
        ModelSpec::Curated { id: id.into() }
    }

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
        // Curated models are listed anyway.
        assert_eq!(active_row(&curated(&catalog[2]), &catalog, &[]), None);
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
    fn typing_only_filters_and_hugging_face_waits_for_a_click() {
        let mut hub = HubSearch::default();
        // Typing changes nothing until the search is asked for.
        hub.typed("p");
        hub.typed("parakeet");
        assert_eq!(hub, HubSearch::Offered);
        // One character is never searched for.
        assert_eq!(hub.start("p"), None);
        assert_eq!(hub, HubSearch::Offered);
        // The click (or Enter) sends one request, and only one.
        assert_eq!(hub.start("parakeet"), Some("parakeet".to_owned()));
        assert!(hub.running("parakeet"));
        assert_eq!(hub.start("parakeet"), None, "already running");
        hub.typed("parakeet");
        assert!(hub.running("parakeet"), "the same query keeps it");
        hub.finished("parakeet".to_owned(), Ok(Vec::new()));
        assert_eq!(hub, HubSearch::Done("parakeet".to_owned(), Ok(Vec::new())));
        assert_eq!(hub.start("parakeet"), None, "found already");

        // A new query offers the search again.
        hub.typed("parakeet u");
        assert_eq!(hub, HubSearch::Offered);
        hub.typed("parakeet");
        assert_eq!(hub, HubSearch::Offered, "nothing comes back by itself");
    }

    #[test]
    fn a_search_typed_over_is_dropped_and_a_failed_one_can_be_retried() {
        let mut hub = HubSearch::default();
        hub.start("whisper");
        hub.typed("whisp");
        hub.finished("whisper".to_owned(), Ok(Vec::new()));
        assert_eq!(hub, HubSearch::Offered, "its answer comes too late");

        hub.start("whisper");
        hub.finished("whisper".to_owned(), Err("offline".into()));
        assert_eq!(hub.start("whisper"), Some("whisper".to_owned()));
    }

    #[test]
    fn a_download_says_how_far_it_is_calmly() {
        let row = Row::curated(&catalog()[0]);
        let mut download = Download::new(row, CancelToken::default());
        let start = Instant::now();
        assert_eq!(download.line(), "Starting the download");
        assert!(download.indeterminate());

        download.progress(312_400_000, 740_000_000, start);
        assert_eq!(download.line(), "312 of 740 MB");
        assert!(!download.indeterminate());
        // The bar moves at once; the amount once a second.
        download.progress(330_000_000, 740_000_000, start + Duration::from_millis(400));
        assert_eq!(download.downloaded, 330_000_000);
        assert_eq!(download.line(), "312 of 740 MB");
        download.progress(380_000_000, 740_000_000, start + TEXT_INTERVAL);
        assert_eq!(download.line(), "380 of 740 MB");
        // The last byte shows at once: the checksum is verified next.
        download.progress(740_000_000, 740_000_000, start + TEXT_INTERVAL);
        assert_eq!(download.line(), "Verifying the download");
        assert!(download.indeterminate());

        // Without a size there is no share to show.
        let mut unsized_download =
            Download::new(Row::curated(&catalog()[1]), CancelToken::default());
        unsized_download.progress(12_300_000, 0, start);
        assert_eq!(unsized_download.line(), "12.3 MB so far");
        assert!(unsized_download.indeterminate());
    }

    #[test]
    fn speed_dots_follow_what_the_model_runs_on() {
        let model = spec("m");
        let ready = |backend: &str| ModelStatus::Ready {
            model: model.clone(),
            architecture: "parakeet".into(),
            streaming: false,
            backend: backend.into(),
        };
        assert_eq!(tier(&ready("vulkan"), BackendSetting::Cpu), Tier::Gpu);
        assert_eq!(tier(&ready("cpu"), BackendSetting::Auto), Tier::Cpu);
        let loading = ModelStatus::Loading {
            model: model.clone(),
        };
        assert_eq!(tier(&loading, BackendSetting::Auto), Tier::Gpu);
        assert_eq!(tier(&loading, BackendSetting::Cpu), Tier::Cpu);
    }

    #[test]
    fn the_legends_spell_out_handys_bands() {
        assert_eq!(
            accuracy_legend(),
            "Accuracy in English, from Handy's tests. Dots by the share of words it gets \
             wrong: 5 under 5% (A), 4 under 10% (B), 3 under 20% (C), 2 under 30% (D), 1 \
             beyond (F)."
        );
        assert_eq!(
            speed_legend(Tier::Cpu),
            "Speed on a laptop's processor, from Handy's tests. Dots by the time 30 s of \
             speech take: 5 under 1.5 s, 4 under 3 s, 3 under 5 s, 2 under 10 s, 1 under 20 \
             s. This computer may differ."
        );
        // Curated rows carry Handy's numbers; untested ones none.
        assert!(
            catalog()
                .iter()
                .map(Row::curated)
                .all(|row| row.benchmark.is_some())
        );
        assert_eq!(Row::hub("o/r", "m.gguf", "c", 1).benchmark, None);
    }

    #[test]
    fn the_last_row_clicked_wins() {
        let (a, b, c) = (spec("a"), spec("b"), spec("c"));
        let mut picks = Picks::default();
        assert_eq!(picks.click(&a, Clicked::Remote, false), Click::Download);
        assert_eq!(picks.click(&b, Clicked::Remote, false), Click::Download);
        assert!(!picks.finished(&a), "b was clicked after a");
        assert!(picks.finished(&b));
        assert!(!picks.finished(&b), "used once");

        // Clicking a downloading row picks it again without a second download.
        picks.click(&b, Clicked::Remote, false);
        assert_eq!(picks.click(&a, Clicked::Downloading, false), Click::Nothing);
        assert!(picks.finished(&a));

        // A model on disk is used at once and drops the pending switch.
        picks.click(&a, Clicked::Remote, false);
        assert_eq!(picks.click(&c, Clicked::OnDisk, false), Click::Use);
        assert!(!picks.finished(&a));
        assert_eq!(picks.click(&c, Clicked::InUse, false), Click::Nothing);

        // A cancelled download switches to nothing.
        picks.click(&a, Clicked::Remote, false);
        picks.dropped(&a);
        assert!(!picks.finished(&a));
    }

    #[test]
    fn a_locked_choice_still_downloads_but_never_switches() {
        let a = spec("a");
        let mut picks = Picks::default();
        assert_eq!(picks.click(&a, Clicked::Remote, true), Click::Download);
        assert!(!picks.finished(&a));
        assert_eq!(picks.click(&a, Clicked::OnDisk, true), Click::Nothing);
    }

    #[test]
    fn only_models_that_take_a_language_hint_offer_a_language() {
        let catalog = catalog();
        let hinted: Vec<&str> = catalog
            .iter()
            .filter(|entry| entry.language_hint)
            .map(|entry| entry.id.as_str())
            .collect();
        assert_eq!(
            hinted,
            ["Qwen3-ASR-1.7B", "nemotron-3.5-asr-streaming-0.6b"]
        );
        // Multilingual, but it detects the language and takes no hint.
        let ultra = catalog.iter().find(|entry| entry.id == "parakeet-ultra");
        let ultra = Row::curated(ultra.unwrap());
        assert!(ultra.multilingual() && !ultra.language_hint);
    }

    #[test]
    fn languages_are_listed_by_name_after_detection() {
        let languages = ["fr".to_owned(), "de".to_owned(), "en".to_owned()];
        let labels = |configured| {
            language_options(&languages, configured)
                .into_iter()
                .map(|(_, label)| label.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(None),
            ["Detect automatically", "English", "French", "German"]
        );
        assert_eq!(labels(Some("de")).len(), 4, "a listed choice adds nothing");
        assert_eq!(
            labels(Some("ja")).last().unwrap(),
            "Japanese (not for this model)"
        );
    }

    #[test]
    fn every_catalog_language_has_a_name() {
        for entry in catalog() {
            for code in &entry.languages {
                assert_ne!(language_name(code), code, "{} lists {code}", entry.id);
            }
        }
    }

    #[test]
    fn files_are_named_without_directories_or_extension() {
        assert_eq!(stem("sub/dir/model-Q8_0.gguf"), "model-Q8_0");
        assert_eq!(stem("MODEL.GGUF"), "MODEL");
        assert_eq!(stem(r"C:\models\mine.gguf"), "mine");
        assert_eq!(stem("notes.bin"), "notes.bin");
    }
}
