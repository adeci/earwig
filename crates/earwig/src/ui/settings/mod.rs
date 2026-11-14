//! The settings window: General, Models, History.

mod general;
mod history;
mod models;
mod pill_preview;
mod shortcut;

use std::iter;
use std::sync::PoisonError;

use gpui_kit::component::label::Label;
use gpui_kit::component::setting::{
    AnySettingField, SettingField, SettingGroup, SettingItem, Settings,
};
use gpui_kit::component::text::Text;
use gpui_kit::component::{ActiveTheme as _, TitleBar, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, Size, Styled as _, Window,
    WindowBounds, WindowOptions, div, px, relative, size,
};

use crate::ui::{UiState, config, theme};
use history::HistoryPage;
use models::ModelsPage;
use pill_preview::PillPreview;
use shortcut::ShortcutField;

const LOCKED_HINT: &str = "Set by system configuration.";
/// Just room for the search box's placeholder and the page names.
const SIDEBAR_WIDTH: Pixels = px(152.);
/// Smallest size at which every page still lays out as it does at the default size.
const MIN_SIZE: Size<Pixels> = size(px(760.), px(540.));

pub fn open(cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(900.), px(680.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(MIN_SIZE),
        app_id: Some(earwig_core::APP_ID.into()),
        focus: true,
        ..TitleBar::window_options()
    };
    let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("earwig settings");
        cx.new(|cx| SettingsView::new(window, cx))
    })?;
    follow_system(window, cx);
    Ok(window)
}

/// Re-render while open when permissions, keyboard access or the keyboards change.
fn follow_system(window: AnyWindowHandle, cx: &App) {
    const EVERY: std::time::Duration = std::time::Duration::from_secs(1);
    let check = || {
        (
            earwig_core::permissions::check(),
            earwig_core::hotkey::status(),
        )
    };
    cx.spawn(async move |cx| {
        let mut last = check();
        loop {
            cx.background_executor().timer(EVERY).await;
            let now = check();
            let open = if now == last {
                cx.update(|cx| window.update(cx, |_, _, _| ()).is_ok())
            } else {
                last = now;
                cx.update(|cx| window.update(cx, |_, window, _| window.refresh()).is_ok())
            };
            if !open {
                break;
            }
        }
    })
    .detach();
}

struct SettingsView {
    models: Entity<ModelsPage>,
    history: Entity<HistoryPage>,
    pill: Entity<PillPreview>,
    shortcut: Entity<ShortcutField>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let shortcut = cx.new(|_| ShortcutField::default());
        cx.observe(&shortcut, |_, _, cx| cx.notify()).detach();
        Self {
            models: cx.new(|cx| ModelsPage::new(window, cx)),
            history: cx.new(|_| HistoryPage::default()),
            pill: cx.new(PillPreview::new),
            shortcut,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = config(UiState::handle(cx));
        v_flex()
            .size_full()
            .bg(theme::page(cx))
            .text_color(cx.theme().foreground)
            // Blur on every press (fields take it back), or a focused field drags the page back.
            .capture_any_mouse_down(|_, window, cx| window.blur(cx))
            .child(TitleBar::new().child(div().text_sm().child("earwig settings")))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        Settings::new("earwig-settings")
                            .sidebar_width(SIDEBAR_WIDTH)
                            // A wider window widens the page, not the sidebar.
                            .sidebar_size_range(SIDEBAR_WIDTH..SIDEBAR_WIDTH)
                            .pages([
                                general::page(&self.pill, &self.shortcut, &settings, cx),
                                models::page(&self.models, &settings, cx),
                                history::page(&self.history, &settings, cx),
                            ]),
                    )
                    // Cover the fixed-width sidebar's resize handle (1 px line, 4 px either side).
                    .child(
                        div()
                            .id("sidebar-edge")
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(SIDEBAR_WIDTH - px(4.))
                            .w(px(9.))
                            .occlude(),
                    ),
            )
    }
}

/// A setting row; locked keys render disabled with a hint.
fn item<T: 'static>(
    cx: &App,
    key: &str,
    title: &'static str,
    description: &str,
    field: SettingField<T>,
) -> SettingItem
where
    SettingField<T>: AnySettingField,
{
    let locked = is_locked(cx, key);
    SettingItem::new(title, field)
        .description(hinted(description, locked))
        .disabled(locked)
}

fn hinted(description: &str, locked: bool) -> String {
    if locked {
        format!("{description} {LOCKED_HINT}")
    } else {
        description.to_owned()
    }
}

fn is_locked(cx: &App, key: &str) -> bool {
    UiState::handle(cx)
        .config()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .is_locked(key)
}

/// Room below the last group, which the page otherwise leaves too tight.
fn spaced(mut groups: Vec<SettingGroup>) -> Vec<SettingGroup> {
    if let Some(last) = groups.pop() {
        groups.push(last.mb_4());
    }
    groups
}

/// A group headed by its first row, not a title (which would list it in the sidebar).
fn section(
    title: &'static str,
    description: Option<&'static str>,
    items: impl IntoIterator<Item = SettingItem>,
) -> SettingGroup {
    let items: Vec<SettingItem> = items
        .into_iter()
        .map(|item| with_keyword(item, title))
        .collect();
    let words: Vec<SharedString> = items.iter().flat_map(search_words).collect();
    SettingGroup::new()
        .item(SettingItem::render(move |_, _, cx| heading(title, description, cx)).keywords(words))
        .items(items)
}

/// Drawn as gpui-kit draws a group's title and description.
fn heading(
    title: &'static str,
    description: Option<&'static str>,
    cx: &App,
) -> impl IntoElement + use<> {
    v_flex()
        .gap_1()
        .text_color(cx.theme().muted_foreground)
        .line_height(relative(1.25))
        .child(title)
        .when_some(description, |this, description| {
            this.child(
                Label::new(description)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
        })
}

fn with_keyword(item: SettingItem, word: &'static str) -> SettingItem {
    let (SettingItem::Item { keywords, .. } | SettingItem::Element { keywords, .. }) = &item;
    let keywords: Vec<SharedString> = keywords.iter().cloned().chain([word.into()]).collect();
    item.keywords(keywords)
}

/// What a search matches: title, plain-text description and keywords.
fn search_words(item: &SettingItem) -> Vec<SharedString> {
    match item {
        SettingItem::Item {
            title,
            description,
            keywords,
            ..
        } => {
            let description = match description {
                Some(Text::String(text)) => Some(text.clone()),
                _ => None,
            };
            iter::once(title.clone())
                .chain(description)
                .chain(keywords.iter().cloned())
                .collect()
        }
        SettingItem::Element { keywords, .. } => keywords.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heading_matches_every_search_its_rows_match() {
        let row = SettingItem::new(
            "Theme",
            SettingField::switch(|_: &App| true, |_, _: &mut App| {}),
        )
        .description("Colours of this window.")
        .keywords(["dark"]);
        let row = with_keyword(row, "Appearance");
        assert_eq!(
            search_words(&row),
            ["Theme", "Colours of this window.", "dark", "Appearance"]
        );
        let custom = with_keyword(SettingItem::render(|_, _, _| div()), "Hotkey");
        assert_eq!(search_words(&custom), ["Hotkey"]);
    }
}
