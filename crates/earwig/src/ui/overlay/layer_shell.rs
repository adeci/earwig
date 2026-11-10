//! The overlay's window on layer-shell: where it opens, and moving it while
//! the pill is dragged (through the patched GPUI in `vendor/`, as GPUI only
//! places a layer surface when it opens it).

use anyhow::bail;
use earwig_core::config::OverlayPosition;
use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui_kit::{
    Bounds, Pixels, Point, Size, Window, WindowBounds, WindowKind, WindowOptions, point, px,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{WINDOW_MARGIN, base_options};

/// The pill floats on the Overlay layer of the output the compositor picks
/// (the focused one), never takes keyboard focus, and reserves no space.
pub(super) fn home_window_options(position: OverlayPosition, size: Size<Pixels>) -> WindowOptions {
    // One edge only: layer-shell centres the surface along it.
    layer_options(
        edge_anchor(position),
        margins(position, px(WINDOW_MARGIN), px(0.)),
        size,
    )
}

/// A transparent Overlay-layer surface that never takes keyboard focus.
fn layer_options(
    anchor: Anchor,
    margin: (Pixels, Pixels, Pixels, Pixels),
    size: Size<Pixels>,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size,
        })),
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: "earwig".into(),
            layer: Layer::Overlay,
            anchor,
            exclusive_zone: None,
            exclusive_edge: None,
            margin: Some(margin),
            keyboard_interactivity: KeyboardInteractivity::None,
        }),
        ..base_options()
    }
}

fn edge_anchor(position: OverlayPosition) -> Anchor {
    match position {
        OverlayPosition::Bottom => Anchor::BOTTOM,
        OverlayPosition::Top => Anchor::TOP,
    }
}

/// Layer-shell margins (top, right, bottom, left): `edge` from the edge at
/// `position`, `left` from the left one.
fn margins(
    position: OverlayPosition,
    edge: Pixels,
    left: Pixels,
) -> (Pixels, Pixels, Pixels, Pixels) {
    match position {
        OverlayPosition::Bottom => (px(0.), px(0.), edge, left),
        OverlayPosition::Top => (edge, px(0.), px(0.), left),
    }
}

/// Move the window of size `size` from one origin to another on the usable
/// `area` (see [`moved`]); with `shift`, pointer positions are in its new
/// frame at once (see `vendor/gpui-pre-linux/PATCH.md`).
pub(super) fn move_window(
    window: &Window,
    position: OverlayPosition,
    (from, origin): (Point<Pixels>, Point<Pixels>),
    size: Size<Pixels>,
    area: Size<Pixels>,
    shift: bool,
) -> anyhow::Result<()> {
    let RawWindowHandle::Wayland(handle) = HasWindowHandle::window_handle(window)?.as_raw() else {
        bail!("the overlay is not a Wayland window");
    };
    let (anchor, margin) = moved(position, origin, size, area);
    if !gpui_linux::move_layer_surface(handle.surface, anchor, margin, origin - from, shift) {
        bail!("the overlay is not a layer surface");
    }
    Ok(())
}

/// Anchor and margins of a window of size `size` at `origin` on the usable
/// `area`: from the edge at `position` and the left one.
pub(super) fn moved(
    position: OverlayPosition,
    origin: Point<Pixels>,
    size: Size<Pixels>,
    area: Size<Pixels>,
) -> (Anchor, (Pixels, Pixels, Pixels, Pixels)) {
    let edge = match position {
        OverlayPosition::Bottom => area.height - origin.y - size.height,
        OverlayPosition::Top => origin.y,
    };
    (
        edge_anchor(position) | Anchor::LEFT,
        margins(position, edge, origin.x),
    )
}
