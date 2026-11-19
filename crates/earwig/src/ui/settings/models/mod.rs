//! The Models page: yours, curated ones to download and Hugging Face results, in one table.

mod languages;
mod table;
mod view;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use earwig_core::events::Command;
use earwig_core::models::{CancelToken, DownloadProgress, InstalledModel, ModelError, ModelSpec};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{App, AppContext as _, Context, Entity, Subscription, Window};

use crate::ui::{UiState, edit_config, show_model_in_tray};
use crate::{human_size, human_size_of};
use table::{Place, Row, canonical, found_rows};

pub use view::page;

/// How often a running download moves its bar.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// How often the amount under the bar updates, so its digits don't flicker.
const TEXT_INTERVAL: Duration = Duration::from_secs(1);

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
    /// The amount the row says, refreshed every [`TEXT_INTERVAL`].
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

    /// The bar follows every update; the text every [`TEXT_INTERVAL`], or at once when complete.
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

    /// Every byte is in: the checksum is being verified.
    fn complete(&self) -> bool {
        self.total > 0 && self.downloaded >= self.total
    }

    /// No size known yet, or verifying.
    fn indeterminate(&self) -> bool {
        self.total == 0 || self.complete()
    }

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
        let (search_input, subscription) = search_box(window, cx);
        Self {
            installed: UiState::handle(cx).models().installed(),
            downloads: HashMap::new(),
            picks: Picks::default(),
            deleting: None,
            errors: HashMap::new(),
            search_input,
            query: String::new(),
            hub: HubSearch::default(),
            _subscriptions: vec![subscription],
        }
    }

    /// For a new settings window: the page afresh, but its downloads go on, with the model to
    /// switch to and why others failed.
    pub fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let old = std::mem::replace(self, Self::new(window, cx));
        self.downloads = old.downloads;
        self.picks = old.picks;
        self.errors = old.errors;
    }

    /// How far a running download of `spec` is: bytes so far and in all, 0 while unknown.
    pub fn downloading(&self, spec: &ModelSpec, cx: &App) -> Option<(u64, u64)> {
        let spec = canonical(spec.clone(), UiState::handle(cx).models().catalog());
        self.downloads.get(&spec).map(|d| (d.downloaded, d.total))
    }

    fn refresh_installed(&mut self, cx: &App) {
        self.installed = UiState::handle(cx).models().installed();
    }

    /// Use the row's model, or download it and use it then.
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
                        // The last byte always gets through: it starts the verifying.
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
        cx.defer(|cx| show_model_in_tray(cx));
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
                    cx.defer(|cx| show_model_in_tray(cx));
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
                cx.defer(|cx| show_model_in_tray(cx));
                match result {
                    Ok(()) => {
                        if self.picks.finished(&spec) {
                            use_model(spec, cx);
                        } else {
                            // The model in use, if it was the one missing, loads now.
                            UiState::handle(cx).send(Command::ReloadConfig);
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

    /// Filters the rows, and forgets a Hugging Face search for another query.
    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query == self.query {
            return;
        }
        self.hub.typed(&query);
        self.query = query;
        cx.notify();
    }

    /// Its row was clicked or Enter pressed.
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

/// Asked only on request, for two characters or more; a new query offers it again.
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

    /// The query to send, unless it's too short, already running or already found.
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

    fn running(&self, query: &str) -> bool {
        matches!(self, Self::Running(searched) if searched == query)
    }
}

/// Two characters or more.
fn searchable(query: &str) -> bool {
    query.chars().nth(1).is_some()
}

/// The search box of a settings window, filtering as it is typed in.
fn search_box(
    window: &mut Window,
    cx: &mut Context<ModelsPage>,
) -> (Entity<InputState>, Subscription) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Search models")
            .clean_on_escape()
    });
    let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| match event {
        InputEvent::Change => {
            let query = input.read(cx).value().trim().to_owned();
            this.set_query(query, cx);
        }
        InputEvent::PressEnter { .. } => this.search_hub(cx),
        InputEvent::Focus | InputEvent::Blur => {}
    });
    (input, subscription)
}

fn use_model(spec: ModelSpec, cx: &mut App) {
    let model = UiState::handle(cx).models().setting_for(spec);
    edit_config(cx, |c| c.model.model = model);
}

/// Where the clicked row's model stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clicked {
    /// On disk and in use.
    InUse,
    OnDisk,
    Downloading,
    /// Not on disk.
    Remote,
}

#[derive(Debug, PartialEq, Eq)]
enum Click {
    Use,
    /// Download it, then use it unless another row was clicked since.
    Download,
    Nothing,
}

/// What a finished download switches to: the last row clicked, if it wasn't on disk yet.
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

    /// Its download failed or was cancelled.
    fn dropped(&mut self, spec: &ModelSpec) {
        if self.is_target(spec) {
            self.target = None;
        }
    }

    fn is_target(&self, spec: &ModelSpec) -> bool {
        self.target.as_ref() == Some(spec)
    }
}

#[cfg(test)]
mod tests {
    use earwig_core::models::{CatalogEntry, ModelManager};

    use super::*;

    pub(super) fn catalog() -> Vec<CatalogEntry> {
        ModelManager::new(std::env::temp_dir()).catalog().to_vec()
    }

    pub(super) fn spec(id: &str) -> ModelSpec {
        ModelSpec::Curated { id: id.into() }
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
}
