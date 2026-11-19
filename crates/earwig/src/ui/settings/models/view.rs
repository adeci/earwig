//! The page as drawn: the search box, then Yours, Available and Untested, each row with its
//! meters and buttons.

use earwig_core::config::{Config, ModelConfig};
use earwig_core::engine::Backend;
use earwig_core::events::ModelStatus;
use earwig_core::models::benchmark::{DICTATION_SECONDS, GRADE_CEILINGS, SPEED_CUTOFFS, STEPS};
use earwig_core::models::{CatalogEntry, ModelSpec, Tier};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::input::Input;
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
    Anchor, AnyElement, AnyView, App, Context, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};

use super::languages::{DETECT_LANGUAGE, language_options};
use super::table::{Place, Row, active_row, canonical, role_label, table, untested_rows};
use super::{HubSearch, ModelsPage, searchable};
use crate::human_size;
use crate::ui::marquee::marquee;
use crate::ui::settings::{LOCKED_HINT, is_locked, percent, spaced};
use crate::ui::{UiState, edit_config, font};

/// Hover group for a row's download mark and trash can.
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
    let active = canonical(crate::configured_model(&models, model), catalog);
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
                this.child(note("Click a model below to download and use it.", muted))
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

/// The loaded model's backend, else the configured one.
fn tier(status: &ModelStatus, backend: Backend) -> Tier {
    match status {
        ModelStatus::Ready { backend, .. } if backend == "cpu" => Tier::Cpu,
        ModelStatus::Ready { .. } => Tier::Gpu,
        _ if backend == Backend::Cpu => Tier::Cpu,
        _ => Tier::Gpu,
    }
}

/// "Search Hugging Face" at the table's end; gone once that search found something.
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
            marquee("hub-search-text", text)
                .text_sm()
                .when(running, |this| this.text_color(theme.muted_foreground)),
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
        "From Hugging Face, not tested with earwig.".into()
    }
}

/// The first section's title also heads the accuracy and speed columns.
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

fn element_id(kind: &str, spec: &ModelSpec) -> SharedString {
    format!("{kind}-{spec}").into()
}

fn render_row(frame: &Frame, row: &Row, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let active = row.spec == *frame.active;
    let in_use = active && row.place != Place::Remote;
    let asking = frame.page.deleting.as_ref() == Some(&row.spec);
    let download = frame.page.downloads.get(&row.spec);
    let (line, line_color) = row_line(frame, row, asking, cx);
    let language = (in_use && row.language_hint && row.multilingual())
        .then(|| language_picker(&row.languages, frame.language, frame.locks.language));
    // The next step while the model in use is not on disk, unless it is a local file.
    let fetch = (active
        && row.place == Place::Remote
        && matches!(frame.status, ModelStatus::Missing { .. })
        && !matches!(row.spec, ModelSpec::Path { .. })
        && download.is_none()
        && !asking)
        .then(|| download_button(frame, row));
    let hover = theme.list_hover;
    h_flex()
        .id(element_id("row", &row.spec))
        .group(ROW_GROUP)
        .w_full()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .when(active, |this| this.bg(theme.list_active))
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
                    marquee("line", line)
                        .text_xs()
                        .text_color(line_color)
                        .when(download.is_some(), |this| {
                            this.font_features(font::TABULAR.clone())
                        }),
                )
                .children(language)
                .children(fetch),
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
            format!("Delete it to free {}?", human_size(row.size)),
            theme.foreground,
        );
    }
    if let Some(error) = page.errors.get(&row.spec) {
        return (error.clone(), theme.danger);
    }
    if let Some(download) = page.downloads.get(&row.spec) {
        let then = if page.picks.is_target(&row.spec) && row.spec != *frame.active {
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
            ModelStatus::Missing {
                model: ModelSpec::Path { .. },
            } => return ("In use, but its file is missing.".into(), theme.warning),
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

/// The next step for the model in use while it is not on disk.
fn download_button(frame: &Frame, row: &Row) -> AnyElement {
    let spec = row.spec.clone();
    let (row, active, locked) = (row.clone(), frame.active.clone(), frame.locks.model);
    h_flex()
        .pt_1()
        .child(
            Button::new(element_id("download", &spec))
                .icon(IconName::ArrowDown)
                .label("Download")
                .primary()
                .small()
                .on_click(row_action(frame.view, &spec, move |page, _, cx| {
                    page.click(&row, &active, locked, cx);
                })),
        )
        .into_any_element()
}

fn status_model(status: &ModelStatus) -> &ModelSpec {
    match status {
        ModelStatus::Missing { model }
        | ModelStatus::Loading { model }
        | ModelStatus::Ready { model, .. }
        | ModelStatus::Failed { model, .. } => model,
    }
}

/// A spinner while downloading, else a check in use, empty on disk, a download mark remote.
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
    let streams = "Streams the words as you speak.";
    h_flex()
        .min_w_0()
        .gap_1p5()
        .child(marquee("name", row.name.clone()).text_sm().font_semibold())
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

/// The meters and size, then × for a download or (on hover) a trash can for a stored model.
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

/// Handy's accuracy and speed dots, or their room when untested.
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

/// Runs `action` for the row's model without the click reaching the row.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::settings::models::tests::spec;

    #[test]
    fn speed_dots_follow_what_the_model_runs_on() {
        let model = spec("m");
        let ready = |backend: &str| ModelStatus::Ready {
            model: model.clone(),
            architecture: "parakeet".into(),
            streaming: false,
            backend: backend.into(),
        };
        assert_eq!(tier(&ready("vulkan"), Backend::Cpu), Tier::Gpu);
        assert_eq!(tier(&ready("cpu"), Backend::Auto), Tier::Cpu);
        let loading = ModelStatus::Loading {
            model: model.clone(),
        };
        assert_eq!(tier(&loading, Backend::Auto), Tier::Gpu);
        assert_eq!(tier(&loading, Backend::Cpu), Tier::Cpu);
    }
}
