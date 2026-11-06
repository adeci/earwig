//! Windows: not ported yet. No overlay (a normal window would take focus from
//! the app the text is meant for), no tray icon, no accent colour.

use earwig_core::controller::Handle;
use earwig_core::desktop::DesktopCaps;
use earwig_core::events::DictationState;
use gpui_kit::{App, Application, Hsla, Pixels, PlatformDisplay, Point, Window};

use crate::ui::{Local, OverlaySurface};

pub(crate) fn overlay_surface(_caps: &DesktopCaps) -> OverlaySurface {
    OverlaySurface::None
}

pub(crate) fn application(_surface: OverlaySurface, _caps: &DesktopCaps) -> Application {
    gpui_kit::application()
}

/// No popup can be moved on Windows yet.
pub(crate) enum Popup {}

impl Popup {
    pub(crate) fn new(_window: &Window) -> anyhow::Result<Self> {
        anyhow::bail!("the overlay cannot be moved on Windows yet")
    }

    pub(crate) fn pointer(&self, _scale: f32) -> Option<Point<Pixels>> {
        match *self {}
    }

    pub(crate) fn move_to(&mut self, _origin: Point<Pixels>, _scale: f32) {
        match *self {}
    }
}

/// There is no tray icon on Windows yet.
pub(crate) enum TrayHandle {}

impl TrayHandle {
    pub(crate) fn set_state(&self, _state: DictationState) {
        match *self {}
    }
}

pub(crate) fn spawn_tray(_handle: Handle, _local: flume::Sender<Local>) -> Option<TrayHandle> {
    log::info!("no tray icon on Windows yet");
    None
}

/// No accent colour on Windows yet: the feed ends at once.
pub(crate) fn follow_accent(_tx: &flume::Sender<Option<Hsla>>, _cx: &App) {}

pub(crate) fn home_display(cx: &App) -> Option<std::rc::Rc<dyn PlatformDisplay>> {
    cx.primary_display()
}
