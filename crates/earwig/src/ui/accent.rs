//! The accent colour: earwig's blue, or the desktop's with `[appearance] accent = "system"`.

use earwig_core::config::AccentSetting;
use gpui_kit::{App, Global, Hsla};

use crate::ui::platform;

/// earwig's blue, the icons'. Over the 3:1 graphics need against both pills' fills.
pub(crate) const EARWIG: u32 = 0x0035_84e4;

#[derive(Default)]
struct Accent {
    setting: AccentSetting,
    /// The desktop's accent: `None` until it answers, or if it has none.
    system: Option<Hsla>,
}

impl Global for Accent {}

impl Accent {
    fn colour(&self) -> Hsla {
        match self.setting {
            AccentSetting::Earwig => earwig(),
            AccentSetting::System => self.system.unwrap_or_else(earwig),
        }
    }
}

fn earwig() -> Hsla {
    gpui_kit::rgb(EARWIG).into()
}

pub(crate) fn get(cx: &App) -> Hsla {
    cx.try_global::<Accent>()
        .map_or_else(earwig, Accent::colour)
}

pub(crate) fn set(setting: AccentSetting, cx: &mut App) {
    update(cx, |accent| accent.setting = setting);
}

/// Always follow the desktop's accent, so switching to it is instant.
pub(crate) fn watch(cx: &mut App) {
    let (tx, rx) = flume::unbounded();
    platform::follow_accent(&tx, cx);
    drop(tx);
    cx.spawn(async move |cx| {
        while let Ok(system) = rx.recv_async().await {
            cx.update(|cx| update(cx, |accent| accent.system = system));
        }
    })
    .detach();
}

/// Repaints only when the colour changes.
fn update(cx: &mut App, change: impl FnOnce(&mut Accent)) {
    let before = get(cx);
    change(cx.default_global::<Accent>());
    if get(cx) != before {
        cx.refresh_windows();
    }
}
