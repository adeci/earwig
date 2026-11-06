//! macOS: not ported yet. No overlay (a normal window would take focus from
//! the app the text is meant for), no accent colour; the menu-bar icon
//! (`tray-icon`).

use earwig_core::desktop::DesktopCaps;
use gpui_kit::{App, Application, Hsla, Pixels, PlatformDisplay, Point, Window};

use crate::ui::OverlaySurface;

pub(crate) use super::tray_icon::{TrayHandle, spawn as spawn_tray};

pub(crate) fn overlay_surface(_caps: &DesktopCaps) -> OverlaySurface {
    OverlaySurface::None
}

pub(crate) fn application(_surface: OverlaySurface, _caps: &DesktopCaps) -> Application {
    gpui_kit::application()
}

/// No popup can be moved on macOS yet.
pub(crate) enum Popup {}

impl Popup {
    pub(crate) fn new(_window: &Window) -> anyhow::Result<Self> {
        anyhow::bail!("the overlay cannot be moved on macOS yet")
    }

    pub(crate) fn pointer(&self, _scale: f32) -> Option<Point<Pixels>> {
        match *self {}
    }

    pub(crate) fn move_to(&mut self, _origin: Point<Pixels>, _scale: f32) {
        match *self {}
    }
}

/// No accent colour on macOS yet: the feed ends at once.
pub(crate) fn follow_accent(_tx: &flume::Sender<Option<Hsla>>, _cx: &App) {}

pub(crate) fn home_display(cx: &App) -> Option<std::rc::Rc<dyn PlatformDisplay>> {
    cx.primary_display()
}
