//! The settings window: General, Models, History.

mod general;
mod history;
mod models;
mod pill_preview;

use gpui_kit::component::setting::Settings;
use gpui_kit::component::{ActiveTheme as _, TitleBar, v_flex};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement,
    ParentElement as _, Render, Styled as _, Window, WindowBounds, WindowOptions, div, px, size,
};

use models::ModelsPage;
use pill_preview::PillPreview;

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
    pill: Entity<PillPreview>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            models: cx.new(|cx| ModelsPage::new(window, cx)),
            pill: cx.new(PillPreview::new),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(div().text_sm().child("earwig settings")))
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new("earwig-settings")
                        .sidebar_width(px(200.))
                        .pages([
                            general::page(&self.pill, cx),
                            models::page(&self.models),
                            history::page(),
                        ]),
                ),
            )
    }
}
