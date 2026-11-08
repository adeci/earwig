//! The accent colour: earwig's own blue, the same on every computer, or
//! with `[appearance] accent = "system"` the desktop's, as the OS reports it
//! (see [`platform::follow_accent`]). Dark or light is earwig's own
//! `[appearance] theme`, not the desktop's colour scheme.

use earwig_core::config::AccentSetting;
use gpui_kit::{App, Global, Hsla};

use crate::ui::platform;

/// earwig's blue, the tray and app icon's. Both themes use it as it is: it
/// stands out from the light theme's white (3.8:1) and from the dark
/// pill's charcoal (4.5:1), past the 3:1 that graphics need.
pub(crate) const EARWIG: u32 = 0x0035_84e4;

#[derive(Default)]
struct Accent {
    setting: AccentSetting,
    /// The desktop's accent: `None` until it answers, and when it has none.
    system: Option<Hsla>,
}

impl Global for Accent {}

impl Accent {
    /// The desktop's accent if the settings ask for it and it has one,
    /// else earwig's.
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

/// The accent to paint with.
pub(crate) fn get(cx: &App) -> Hsla {
    cx.try_global::<Accent>()
        .map_or_else(earwig, Accent::colour)
}

/// Paint with the accent `setting` picks from now on.
pub(crate) fn set(setting: AccentSetting, cx: &mut App) {
    update(cx, |accent| accent.setting = setting);
}

/// Follow the desktop's accent off the main thread, whichever accent is in
/// use, so switching to it shows it at once.
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

/// Change the accent's state, repainting when that changes its colour.
fn update(cx: &mut App, change: impl FnOnce(&mut Accent)) {
    let before = get(cx);
    change(cx.default_global::<Accent>());
    if get(cx) != before {
        cx.refresh_windows();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_takes_the_desktops_accent_where_it_has_one() {
        let purple: Hsla = gpui_kit::rgb(0x0091_41ac).into();
        let accent = |setting, system| Accent { setting, system }.colour();
        assert_eq!(accent(AccentSetting::Earwig, Some(purple)), earwig());
        assert_eq!(accent(AccentSetting::System, Some(purple)), purple);
        assert_eq!(accent(AccentSetting::System, None), earwig());
    }
}
