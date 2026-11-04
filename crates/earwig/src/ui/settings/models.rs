//! Models: the active one, the curated catalog, Hugging Face search, and
//! local files. Downloads and searches run on background threads and report
//! back over channels.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use earwig_core::events::ModelStatus;
use earwig_core::models::{
    CancelToken, CatalogEntry, DownloadProgress, InstalledModel, ModelError, ModelSpec,
    SearchResult,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::setting::{SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _,
    SharedString, Styled as _, Subscription, Window, div,
};

use crate::ui::{UiState, config, edit_config};

/// How often a running download repaints its progress bar.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub struct ModelsPage {
    installed: Vec<InstalledModel>,
    downloads: HashMap<ModelSpec, Download>,
    search_input: Entity<InputState>,
    search: Search,
    local_input: Entity<InputState>,
    /// Last failed action, shown at the top of the page.
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

struct Download {
    downloaded: u64,
    total: u64,
    cancel: CancelToken,
}

enum Search {
    Idle,
    Running(String),
    Done(Vec<SearchResult>),
    Failed(String),
}

enum DownloadMsg {
    Progress(DownloadProgress),
    Done(ModelSpec, Result<(), ModelError>),
}

impl ModelsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search Hugging Face, e.g. parakeet")
        });
        let local_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/model.gguf"));
        let subscriptions = vec![
            cx.subscribe(&search_input, |this, input, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let query = input.read(cx).value().trim().to_owned();
                    this.run_search(query, cx);
                }
            }),
            cx.subscribe(&local_input, |this, input, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let path = input.read(cx).value().trim().to_owned();
                    this.use_local(&path, cx);
                }
            }),
        ];
        Self {
            installed: UiState::handle(cx).models().installed(),
            downloads: HashMap::new(),
            search_input,
            search: Search::Idle,
            local_input,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn refresh_installed(&mut self, cx: &App) {
        self.installed = UiState::handle(cx).models().installed();
    }

    fn is_installed(&self, spec: &ModelSpec, cx: &App) -> bool {
        UiState::handle(cx).models().resolve(spec).is_ok()
    }

    fn download(&mut self, spec: ModelSpec, cx: &mut Context<Self>) {
        if self.downloads.contains_key(&spec) {
            return;
        }
        let cancel = CancelToken::default();
        self.downloads.insert(
            spec.clone(),
            Download {
                downloaded: 0,
                total: 0,
                cancel: cancel.clone(),
            },
        );
        self.error = None;
        let models = UiState::handle(cx).models();
        let (tx, rx) = flume::unbounded();
        std::thread::Builder::new()
            .name("earwig-download".into())
            .spawn(move || {
                let mut last = Instant::now();
                let result = models.download(
                    &spec,
                    &mut |progress| {
                        if last.elapsed() >= PROGRESS_INTERVAL {
                            last = Instant::now();
                            let _ = tx.send(DownloadMsg::Progress(progress));
                        }
                    },
                    &cancel,
                );
                let _ = tx.send(DownloadMsg::Done(spec, result.map(drop)));
            })
            .expect("spawning a thread");
        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv_async().await {
                if this.update(cx, |this, cx| this.on_download(msg, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn on_download(&mut self, msg: DownloadMsg, cx: &mut Context<Self>) {
        match msg {
            DownloadMsg::Progress(progress) => {
                if let Some(download) = self.downloads.get_mut(&progress.spec) {
                    download.downloaded = progress.downloaded;
                    download.total = progress.total;
                }
            }
            DownloadMsg::Done(spec, result) => {
                self.downloads.remove(&spec);
                match result {
                    Ok(()) | Err(ModelError::Cancelled) => {}
                    Err(e) => self.error = Some(format!("Downloading {spec}: {e}")),
                }
                self.refresh_installed(cx);
            }
        }
        cx.notify();
    }

    fn cancel(&mut self, spec: &ModelSpec) {
        if let Some(download) = self.downloads.get(spec) {
            download.cancel.cancel();
        }
    }

    fn delete(&mut self, spec: &ModelSpec, cx: &mut Context<Self>) {
        if let Err(e) = UiState::handle(cx).models().delete(spec) {
            self.error = Some(format!("Deleting {spec}: {e}"));
        }
        self.refresh_installed(cx);
        cx.notify();
    }

    fn use_model(&mut self, spec: ModelSpec, cx: &mut Context<Self>) {
        self.error = None;
        edit_config(cx, |c| c.model.model = Some(spec));
        cx.notify();
    }

    fn use_local(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            self.error = Some("Enter an absolute path to a .gguf file.".into());
        } else if !path.is_file() {
            self.error = Some(format!("{} is not a file.", path.display()));
        } else {
            self.use_model(ModelSpec::Path { path }, cx);
            return;
        }
        cx.notify();
    }

    fn run_search(&mut self, query: String, cx: &mut Context<Self>) {
        if query.is_empty() {
            return;
        }
        self.search = Search::Running(query.clone());
        let handle = UiState::handle(cx);
        let models = handle.models();
        let engine = handle.engine();
        let (tx, rx) = flume::bounded(1);
        std::thread::Builder::new()
            .name("earwig-search".into())
            .spawn(move || {
                let result = models.search(&query, &|arch| engine.supports_architecture(arch));
                let _ = tx.send((query, result));
            })
            .expect("spawning a thread");
        cx.spawn(async move |this, cx| {
            if let Ok((query, result)) = rx.recv_async().await {
                let _ = this.update(cx, |this, cx| {
                    // A newer search replaced this one.
                    if matches!(&this.search, Search::Running(q) if *q == query) {
                        this.search = match result {
                            Ok(results) => Search::Done(results),
                            Err(e) => Search::Failed(e.to_string()),
                        };
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        cx.notify();
    }
}

pub fn page(view: &Entity<ModelsPage>) -> SettingPage {
    let section = |render: fn(&ModelsPage, &Entity<ModelsPage>, &App) -> AnyElement| {
        let view = view.clone();
        SettingItem::render(move |_, _, cx| render(view.read(cx), &view, cx))
    };
    SettingPage::new("Models")
        .icon(IconName::Cpu)
        .resettable(false)
        .groups([
            SettingGroup::new()
                .title("Active model")
                .item(section(render_active)),
            SettingGroup::new()
                .title("Recommended")
                .item(section(render_catalog)),
            SettingGroup::new()
                .title("Downloaded")
                .item(section(render_installed)),
            SettingGroup::new()
                .title("Hugging Face")
                .description("Models published for transcribe.cpp.")
                .item(section(render_search)),
            SettingGroup::new()
                .title("Local file")
                .item(section(render_local)),
        ])
}

fn active_spec(cx: &App) -> ModelSpec {
    let handle = UiState::handle(cx);
    config(handle).model.model.unwrap_or_else(|| ModelSpec::Curated {
        id: handle.models().default_model().id.clone(),
    })
}

/// Whether two specs name the same file (a catalog id and its `hf:` form).
fn same_model(a: &ModelSpec, b: &ModelSpec, cx: &App) -> bool {
    let models = UiState::handle(cx).models();
    a == b || matches!((models.path_for(a), models.path_for(b)), (Ok(x), Ok(y)) if x == y)
}

fn model_locked(cx: &App) -> bool {
    UiState::handle(cx)
        .config()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .is_locked("model.model")
}

fn display_name(spec: &ModelSpec, cx: &App) -> String {
    match spec {
        ModelSpec::Curated { id } => UiState::handle(cx)
            .models()
            .curated(id)
            .map_or_else(|| id.clone(), |entry| entry.name.clone()),
        ModelSpec::HuggingFace { repo, file, .. } => format!("{repo} · {file}"),
        ModelSpec::Path { path } => path.display().to_string(),
    }
}

fn render_active(page: &ModelsPage, _: &Entity<ModelsPage>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (spec, state, color) = match &UiState::status(cx).model {
        ModelStatus::Missing { model } => (
            model.clone(),
            "Not downloaded yet. Download it below.".to_owned(),
            theme.warning,
        ),
        ModelStatus::Loading { model } => (model.clone(), "Loading…".to_owned(), theme.muted_foreground),
        ModelStatus::Ready {
            model,
            architecture,
            streaming,
            backend,
        } => (
            model.clone(),
            format!(
                "Ready · {architecture} on {backend} · {}",
                if *streaming {
                    "native streaming"
                } else {
                    "live text by re-decoding"
                }
            ),
            theme.success,
        ),
        ModelStatus::Failed { model, message } => {
            (model.clone(), format!("Failed to load: {message}"), theme.danger)
        }
    };
    v_flex()
        .w_full()
        .gap_1()
        .when_some(page.error.clone(), |this, error| {
            this.child(div().text_sm().text_color(theme.danger).child(error))
        })
        .child(div().font_semibold().child(display_name(&spec, cx)))
        .child(div().text_sm().text_color(color).child(state))
        .when(model_locked(cx), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Set by system configuration."),
            )
        })
        .into_any_element()
}

fn render_catalog(page: &ModelsPage, view: &Entity<ModelsPage>, cx: &App) -> AnyElement {
    let catalog: Vec<CatalogEntry> = UiState::handle(cx).models().catalog().to_vec();
    v_flex()
        .w_full()
        .gap_4()
        .children(catalog.into_iter().map(|entry| {
            let spec = ModelSpec::Curated {
                id: entry.id.clone(),
            };
            let languages = entry.languages.join(", ");
            let meta = format!(
                "{} · {} · {}{}",
                format_size(entry.size),
                languages,
                entry.license,
                if entry.streaming { " · streaming" } else { "" }
            );
            model_row(
                page,
                view,
                &spec,
                v_flex()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_semibold().child(entry.name.clone()))
                            .when(entry.default, |this| {
                                this.child(Tag::secondary().small().child("default"))
                            }),
                    )
                    .child(div().text_sm().child(entry.summary.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta),
                    ),
                cx,
            )
        }))
        .into_any_element()
}

fn render_installed(page: &ModelsPage, view: &Entity<ModelsPage>, cx: &App) -> AnyElement {
    let others: Vec<&InstalledModel> = page
        .installed
        .iter()
        .filter(|m| m.catalog_id.is_none())
        .collect();
    v_flex()
        .w_full()
        .gap_3()
        .when(others.is_empty(), |this| {
            this.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Models you download from Hugging Face show up here."),
            )
        })
        .children(others.into_iter().map(|model| {
            model_row(
                page,
                view,
                &model.spec,
                v_flex()
                    .gap_0p5()
                    .child(div().font_semibold().child(display_name(&model.spec, cx)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format_size(model.size)),
                    ),
                cx,
            )
        }))
        .into_any_element()
}

/// Description on the left; download progress or Download/Use/Delete on the
/// right.
fn model_row(
    page: &ModelsPage,
    view: &Entity<ModelsPage>,
    spec: &ModelSpec,
    description: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let id = SharedString::from(spec.to_string());
    let active = same_model(spec, &active_spec(cx), cx);
    let installed = page.is_installed(spec, cx);
    let locked = model_locked(cx);
    let actions = if let Some(download) = page.downloads.get(spec) {
        #[allow(clippy::cast_precision_loss)]
        let percent = if download.total == 0 {
            0.
        } else {
            download.downloaded as f32 * 100. / download.total as f32
        };
        h_flex()
            .gap_2()
            .w(gpui_kit::px(220.))
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .child(Progress::new(("progress", id.clone())).value(percent))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        format!(
                            "{} of {}",
                            format_size(download.downloaded),
                            format_size(download.total)
                        ),
                    )),
            )
            .child(
                Button::new(("cancel", id.clone()))
                    .label("Cancel")
                    .small()
                    .ghost()
                    .on_click({
                        let view = view.clone();
                        let spec = spec.clone();
                        move |_, _, cx| view.update(cx, |page, _| page.cancel(&spec))
                    }),
            )
    } else if installed {
        h_flex()
            .gap_2()
            .when(active, |this| this.child(Tag::success().small().child("in use")))
            .when(!active, |this| {
                this.child(
                    Button::new(("use", id.clone()))
                        .label("Use")
                        .small()
                        .primary()
                        .disabled(locked)
                        .on_click({
                            let view = view.clone();
                            let spec = spec.clone();
                            move |_, _, cx| {
                                view.update(cx, |page, cx| page.use_model(spec.clone(), cx));
                            }
                        }),
                )
            })
            .child(
                Button::new(("delete", id.clone()))
                    .icon(IconName::Delete)
                    .small()
                    .ghost()
                    .tooltip("Delete")
                    .disabled(active || matches!(spec, ModelSpec::Path { .. }))
                    .on_click({
                        let view = view.clone();
                        let spec = spec.clone();
                        move |_, _, cx| view.update(cx, |page, cx| page.delete(&spec, cx))
                    }),
            )
    } else {
        h_flex().gap_2().child(
            Button::new(("download", id.clone()))
                .label("Download")
                .small()
                .outline()
                .on_click({
                    let view = view.clone();
                    let spec = spec.clone();
                    move |_, _, cx| view.update(cx, |page, cx| page.download(spec.clone(), cx))
                }),
        )
    };
    h_flex()
        .w_full()
        .items_start()
        .justify_between()
        .gap_4()
        .child(div().flex_1().child(description))
        .child(actions)
        .into_any_element()
}

fn render_search(page: &ModelsPage, view: &Entity<ModelsPage>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let results: AnyElement = match &page.search {
        Search::Idle => div().into_any_element(),
        Search::Running(query) => div()
            .text_sm()
            .text_color(muted)
            .child(format!("Searching for “{query}”…"))
            .into_any_element(),
        Search::Failed(message) => div()
            .text_sm()
            .text_color(cx.theme().danger)
            .child(message.clone())
            .into_any_element(),
        Search::Done(results) if results.is_empty() => div()
            .text_sm()
            .text_color(muted)
            .child("No matching models.")
            .into_any_element(),
        Search::Done(results) => v_flex()
            .gap_4()
            .children(results.iter().map(|result| {
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().font_semibold().child(result.repo.clone()))
                            .child(div().text_xs().text_color(muted).child(format!(
                                "{} downloads · {} likes",
                                result.downloads, result.likes
                            ))),
                    )
                    .children(result.files.iter().map(|file| {
                        let spec = ModelSpec::HuggingFace {
                            repo: result.repo.clone(),
                            file: file.file.clone(),
                            revision: file.revision.clone(),
                        };
                        let arch = file.architecture.as_deref().unwrap_or("unknown architecture");
                        model_row(
                            page,
                            view,
                            &spec,
                            v_flex()
                                .gap_0p5()
                                .child(div().text_sm().child(file.file.clone()))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(format!("{} · {arch}", format_size(file.size)))
                                        .child(if file.compatible {
                                            Tag::success().small().child("compatible")
                                        } else {
                                            Tag::warning().small().child("not supported")
                                        }),
                                ),
                            cx,
                        )
                    }))
            }))
            .into_any_element(),
    };
    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(&page.search_input)))
                .child(
                    Button::new("search")
                        .icon(IconName::Search)
                        .label("Search")
                        .outline()
                        .loading(matches!(page.search, Search::Running(_)))
                        .on_click({
                            let view = view.clone();
                            move |_, _, cx| {
                                view.update(cx, |page, cx| {
                                    let query =
                                        page.search_input.read(cx).value().trim().to_owned();
                                    page.run_search(query, cx);
                                });
                            }
                        }),
                ),
        )
        .child(results)
        .into_any_element()
}

fn render_local(page: &ModelsPage, view: &Entity<ModelsPage>, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Any GGUF file transcribe.cpp can load. It is used where it is, not copied."),
        )
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(&page.local_input)))
                .child(
                    Button::new("use-local")
                        .label("Use")
                        .outline()
                        .disabled(model_locked(cx))
                        .on_click({
                            let view = view.clone();
                            move |_, _, cx| {
                                view.update(cx, |page, cx| {
                                    let path = page.local_input.read(cx).value().trim().to_owned();
                                    page.use_local(&path, cx);
                                });
                            }
                        }),
                ),
        )
        .into_any_element()
}

#[allow(clippy::cast_precision_loss)]
fn format_size(bytes: u64) -> String {
    const MB: f64 = 1024. * 1024.;
    let bytes = bytes as f64;
    if bytes >= 1024. * MB {
        format!("{:.1} GB", bytes / (1024. * MB))
    } else {
        format!("{:.0} MB", bytes / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn sizes_switch_to_gigabytes_at_one_gib() {
        assert_eq!(format_size(731_357_568), "697 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }
}
