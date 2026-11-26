//! Linux: layer-shell or an X11 popup, an SNI tray, the portal's accent colour.

mod accent;
mod layer_area;
mod tray;
mod x11_popup;

use std::rc::Rc;

use earwig_core::desktop::DesktopCaps;
use gpui_kit::{App, Application, Bounds, Pixels, PlatformDisplay, Point, WindowingModes};

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
        // No layer-shell (GNOME): an XWayland override-redirect popup floats without taking focus.
        gpui_kit::platform::linux(WindowingModes::X11)
    } else {
        gpui_kit::application()
    }
}

/// Where the X11 popup opens: the monitor containing the pointer, else the primary monitor.
/// The compositor places layer surfaces itself.
pub(crate) fn home_display(cx: &App) -> Option<Rc<dyn PlatformDisplay>> {
    let displays = cx.displays();
    pick_display(&displays, gpui_linux::x11_pointer_position()).or_else(|| cx.primary_display())
}

fn pick_display(
    displays: &[Rc<dyn PlatformDisplay>],
    pointer: Option<Point<Pixels>>,
) -> Option<Rc<dyn PlatformDisplay>> {
    let pointer = pointer?;
    let index = pick_display_index(displays.iter().map(|display| display.bounds()), pointer)?;
    displays.get(index).cloned()
}

fn pick_display_index(
    displays: impl IntoIterator<Item = Bounds<Pixels>>,
    pointer: Point<Pixels>,
) -> Option<usize> {
    displays
        .into_iter()
        .position(|bounds| bounds.contains(&pointer))
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, px, size};

    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn picks_the_monitor_containing_the_pointer_and_falls_back_when_offscreen() {
        let monitors = [rect(0., 0., 2560., 1440.), rect(2560., 0., 1920., 1080.)];
        assert_eq!(
            pick_display_index(monitors, point(px(100.), px(100.))),
            Some(0)
        );
        assert_eq!(
            pick_display_index(monitors, point(px(2560.), px(500.))),
            Some(1)
        );
        assert_eq!(
            pick_display_index(monitors, point(px(3500.), px(900.))),
            Some(1)
        );
        // Dead space below the shorter right monitor or stale coords after unplugging it.
        assert_eq!(
            pick_display_index(monitors, point(px(3000.), px(1200.))),
            None
        );
        assert_eq!(
            pick_display_index(monitors, point(px(5000.), px(200.))),
            None
        );
        // After unplugging the right monitor, stale pointer coords fall back to primary.
        let single = [rect(0., 0., 2560., 1440.)];
        assert_eq!(pick_display_index(single, point(px(3500.), px(500.))), None);
        assert_eq!(
            pick_display_index(single, point(px(1280.), px(720.))),
            Some(0)
        );
    }

    #[test]
    fn handles_negative_and_stacked_monitor_origins() {
        let monitors = [rect(-1920., 200., 1920., 1080.), rect(0., 0., 2560., 1440.)];
        assert_eq!(
            pick_display_index(monitors, point(px(-960.), px(600.))),
            Some(0)
        );
        assert_eq!(pick_display_index(monitors, point(px(0.), px(0.))), Some(1));
        assert_eq!(pick_display_index([], point(px(0.), px(0.))), None);
    }
}
