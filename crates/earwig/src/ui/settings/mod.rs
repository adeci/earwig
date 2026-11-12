//! The settings window: General, Models, History.

mod general;
mod history;
mod models;
mod pill_preview;

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
    IntoElement, ParentElement as _, Pixels, Render, SharedString, Styled as _, Window,
    WindowBounds, WindowOptions, div, px, relative, size,
};

use crate::ui::{UiState, config, theme};
use history::HistoryPage;
use models::ModelsPage;
use pill_preview::PillPreview;

const LOCKED_HINT: &str = "Set by system configuration.";
const SIDEBAR_WIDTH: Pixels = px(200.);

pub fn open(cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(900.), px(680.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(600.), px(420.))),
        app_id: Some(earwig_core::APP_ID.into()),
        focus: true,
        ..TitleBar::window_options()
    };
    let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("earwig settings");
        cx.new(|cx| SettingsView::new(window, cx))
    })?;
    #[cfg(target_os = "macos")]
    follow_permissions(window, cx);
    Ok(window)
}

/// Permissions are granted in System Settings, not here: re-render the
/// window when one changes, for as long as it is open.
#[cfg(target_os = "macos")]
fn follow_permissions(window: AnyWindowHandle, cx: &App) {
    const EVERY: std::time::Duration = std::time::Duration::from_secs(1);
    cx.spawn(async move |cx| {
        let mut last = earwig_core::permissions::check();
        loop {
            cx.background_executor().timer(EVERY).await;
            let now = earwig_core::permissions::check();
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
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            models: cx.new(|cx| ModelsPage::new(window, cx)),
            history: cx.new(|_| HistoryPage::default()),
            pill: cx.new(PillPreview::new),
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
            // Every press first clears focus, and a press in a text field
            // takes it again before anything redraws. Without this a field
            // kept focus wherever you clicked (gpui-component's buttons don't
            // take it), and a dropdown handed it back when its menu closed,
            // scrolling the page back to the field.
            .capture_any_mouse_down(|_, window, cx| window.blur(cx))
            .child(TitleBar::new().child(div().text_sm().child("earwig settings")))
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new("earwig-settings")
                        .sidebar_width(SIDEBAR_WIDTH)
                        // A wider window widens the page, not the sidebar.
                        .sidebar_size_range(SIDEBAR_WIDTH..SIDEBAR_WIDTH)
                        .pages([
                            general::page(&self.pill, &settings, cx),
                            models::page(&self.models, &settings, cx),
                            history::page(&self.history, &settings, cx),
                        ]),
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

/// `description`, saying so when its key is locked.
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

/// A page's groups with room below the last one: the page itself leaves
/// only that group's padding there, half the space between two groups.
fn spaced(mut groups: Vec<SettingGroup>) -> Vec<SettingGroup> {
    if let Some(last) = groups.pop() {
        groups.push(last.mb_4());
    }
    groups
}

/// A group under a heading drawn in the page as its first row. As the
/// group's title, the heading would also list the group in the sidebar,
/// where gpui-kit's `Settings` opens that list when its page is clicked but
/// closes it only from a small arrow; the sidebar names only the pages
/// instead. The heading shows whenever one of the rows does, and searching
/// for the heading finds the whole group.
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

/// A group's title, and its description below, as a group draws them.
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

/// `item`, which a search for `word` finds too.
fn with_keyword(item: SettingItem, word: &'static str) -> SettingItem {
    let (SettingItem::Item { keywords, .. } | SettingItem::Element { keywords, .. }) = &item;
    let keywords: Vec<SharedString> = keywords.iter().cloned().chain([word.into()]).collect();
    item.keywords(keywords)
}

/// Everything a search matches in `item`: its title, its description (when
/// plain text) and its keywords.
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
