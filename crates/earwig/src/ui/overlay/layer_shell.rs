//! Window options for the overlay on layer-shell, where a surface cannot
//! move once mapped: the pill's home window, the window it is reopened in
//! where it was dropped, and the drag surface over the usable area.

use earwig_core::config::OverlayPosition;
use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui_kit::{Bounds, Pixels, Point, Size, WindowBounds, WindowKind, WindowOptions, point, px};

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

/// The window at `origin` on the usable `area`, where a pill was dropped.
pub(super) fn moved_window_options(
    position: OverlayPosition,
    origin: Point<Pixels>,
    window: Size<Pixels>,
    area: Size<Pixels>,
) -> WindowOptions {
    let edge = match position {
        OverlayPosition::Bottom => area.height - origin.y - window.height,
        OverlayPosition::Top => origin.y,
    };
    layer_options(
        edge_anchor(position) | Anchor::LEFT,
        margins(position, edge, origin.x),
        window,
    )
}

/// The surface over the whole usable `area` that shows the pill during a
/// drag; it takes no input.
pub(super) fn drag_window_options(position: OverlayPosition, area: Size<Pixels>) -> WindowOptions {
    // Anchored like the home window, so the two share the area's frame:
    // centred along the edge, the surface fills the area's width.
    let options = layer_options(
        edge_anchor(position),
        margins(position, px(0.), px(0.)),
        area,
    );
    // The pill follows the pointer at display rate: the surface never has
    // focus or input of its own, which GPUI would otherwise hold to 30 fps
    // (and to 30 fps in power-saver mode). Drawn with quads only, its
    // frames are cheap.
    WindowOptions {
        low_power_frame_interval: None,
        ..options
    }
}
