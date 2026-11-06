//! Linux: layer-shell where the compositor has it, else an override-redirect
//! X11 popup (GNOME, through XWayland); a StatusNotifierItem tray; the XDG
//! Settings portal's accent colour.

mod accent;
mod layer_area;
mod tray;
mod x11_popup;

use earwig_core::desktop::DesktopCaps;
use gpui_kit::{App, Application, PlatformDisplay, WindowingModes};

use crate::ui::OverlaySurface;

pub(crate) use accent::follow as follow_accent;
pub(crate) use layer_area::usable_area;
pub(crate) use tray::{TrayHandle, spawn as spawn_tray};
pub(crate) use x11_popup::X11Popup as Popup;

pub(crate) fn overlay_surface(caps: &DesktopCaps) -> OverlaySurface {
    if caps.layer_shell {
        OverlaySurface::LayerShell
    } else if caps.x11 {
        OverlaySurface::Popup
    } else {
        OverlaySurface::None
    }
}

pub(crate) fn application(surface: OverlaySurface, caps: &DesktopCaps) -> Application {
    if surface == OverlaySurface::Popup && caps.wayland {
        // No layer-shell (GNOME): run on XWayland, where an override-redirect
        // popup floats above other windows without taking focus. A Wayland
        // toplevel would be centred by the compositor and steal focus.
        gpui_kit::platform::linux(WindowingModes::X11)
    } else {
        gpui_kit::application()
    }
}

/// The primary display: the X11 popup opens there (layer-shell surfaces go
/// to the output the compositor picks).
pub(crate) fn home_display(cx: &App) -> Option<std::rc::Rc<dyn PlatformDisplay>> {
    cx.primary_display()
}
