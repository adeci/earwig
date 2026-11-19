//! Where the pill's window goes on the screen, and the pill in it: home, or where it was dragged
//! or left last time, always on screen.

use earwig_core::config::OverlayPosition;
use gpui_kit::{Bounds, Pixels, Point, Size, point, px, size};

use super::{
    EDGE_GAP, GAP, HEADER_HEIGHT, INSET, LINE_HEIGHT, MARGIN, PADDING_Y, WIDTH, WINDOW_MARGIN,
};

/// Room for the header, `max_lines` lines and the inset, in whole pixels; the pill shrinks to fit.
#[allow(clippy::cast_precision_loss)]
pub(super) fn window_size(max_lines: u32) -> Size<Pixels> {
    let height =
        2. * INSET + PADDING_Y * 2. + HEADER_HEIGHT + GAP + LINE_HEIGHT * max_lines.max(1) as f32;
    size(px((WIDTH + 2. * INSET).floor()), px(height.floor()))
}

/// Home: centred, `MARGIN` off its edge. A spot is the pill's left x and its hugged edge's y.
pub(super) fn home_spot(area: Bounds<Pixels>, position: OverlayPosition) -> Point<Pixels> {
    let x = area.left() + (area.size.width - px(WIDTH)) / 2.;
    let y = match position {
        OverlayPosition::Bottom => area.bottom() - px(MARGIN),
        OverlayPosition::Top => area.top() + px(MARGIN),
    };
    point(x, y)
}

/// Where layer-shell puts the home window: centred in whole pixels, margin truncated like GPUI.
pub(super) fn layer_home_origin(
    area: Size<Pixels>,
    window: Size<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let x = (area.width / 2.).floor() - (window.width / 2.).floor();
    let margin = px(WINDOW_MARGIN.trunc());
    let y = match position {
        OverlayPosition::Bottom => area.height - margin - window.height,
        OverlayPosition::Top => margin,
    };
    point(x, y)
}

/// The spot of a pill hugging its edge of the window at `origin`.
pub(super) fn hug(
    origin: Point<Pixels>,
    window: Size<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let y = match position {
        OverlayPosition::Bottom => origin.y + window.height - px(INSET),
        OverlayPosition::Top => origin.y + px(INSET),
    };
    point(origin.x + px(INSET), y)
}

/// Moves `spot` just enough to keep the pill `EDGE_GAP` inside `area` (top-left first if it can't).
pub(super) fn clamp_spot(
    spot: Point<Pixels>,
    pill: Size<Pixels>,
    area: Bounds<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let gap = px(EDGE_GAP);
    let fit = |value: Pixels, low: Pixels, high: Pixels| value.min(high).max(low);
    let x = fit(spot.x, area.left() + gap, area.right() - gap - pill.width);
    let y = match position {
        OverlayPosition::Bottom => fit(spot.y, area.top() + gap + pill.height, area.bottom() - gap),
        OverlayPosition::Top => fit(spot.y, area.top() + gap, area.bottom() - gap - pill.height),
    };
    point(x, y)
}

/// The spot `left` from an area's top-left on `area`, kept on it like a dragged pill's.
pub(super) fn remembered_spot(
    left: Point<Pixels>,
    area: Bounds<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    clamp_spot(area.origin + left, size(px(WIDTH), px(0.)), area, position)
}

/// Top-left of the window showing the pill at `spot`.
pub(super) fn popup_origin(
    spot: Point<Pixels>,
    window: Size<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let y = match position {
        OverlayPosition::Bottom => spot.y + px(INSET) - window.height,
        OverlayPosition::Top => spot.y - px(INSET),
    };
    point(spot.x - px(INSET), y)
}

/// [`popup_origin`] kept on `area` in whole pixels; near an edge the pill sits off-centre.
pub(super) fn window_origin(
    spot: Point<Pixels>,
    window: Size<Pixels>,
    area: Bounds<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let origin = popup_origin(spot, window, position);
    let fit = |value: Pixels, low: Pixels, high: Pixels| value.min(high).max(low).round();
    point(
        fit(origin.x, area.left(), area.right() - window.width),
        fit(origin.y, area.top(), area.bottom() - window.height),
    )
}

/// Where the pill sits in its window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Placement {
    pub(super) left: Pixels,
    /// From the window's hugged edge; gives way as the pill grows into `far`.
    pub(super) near: Pixels,
    /// Kept clear at the other end so the pill stays `EDGE_GAP` off the area's far edge.
    pub(super) far: Pixels,
}

pub(super) fn placement(
    spot: Point<Pixels>,
    origin: Point<Pixels>,
    window: Size<Pixels>,
    area: Bounds<Pixels>,
    position: OverlayPosition,
) -> Placement {
    let (near, beyond) = match position {
        OverlayPosition::Bottom => (origin.y + window.height - spot.y, origin.y - area.top()),
        OverlayPosition::Top => (spot.y - origin.y, area.bottom() - origin.y - window.height),
    };
    Placement {
        left: spot.x - origin.x,
        near: near.max(px(0.)),
        far: (px(EDGE_GAP) - beyond).max(px(0.)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::ui::overlay::layer_shell;

    const POSITIONS: [OverlayPosition; 2] = [OverlayPosition::Bottom, OverlayPosition::Top];

    fn screen() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(1000.), px(0.)),
            size: size(px(1920.), px(1080.)),
        }
    }

    /// The pill's rectangle at `spot`, `height` tall.
    fn pill_at(spot: Point<Pixels>, height: f32, position: OverlayPosition) -> Bounds<Pixels> {
        let top = match position {
            OverlayPosition::Bottom => spot.y - px(height),
            OverlayPosition::Top => spot.y,
        };
        Bounds::new(point(spot.x, top), size(px(WIDTH), px(height)))
    }

    /// `pill` is `gap` inside `area`, give or take float rounding.
    fn inside(pill: Bounds<Pixels>, area: Bounds<Pixels>, gap: f32) -> bool {
        let gap = px(gap - 1e-3);
        pill.left() >= area.left() + gap
            && pill.right() <= area.right() - gap
            && pill.top() >= area.top() + gap
            && pill.bottom() <= area.bottom() - gap
    }

    #[allow(clippy::cast_precision_loss)]
    fn drags() -> impl Iterator<Item = Point<Pixels>> {
        (-4000..=4000).step_by(250).flat_map(|dx| {
            (-3000..=3000)
                .step_by(125)
                .map(move |dy| point(px(dx as f32), px(dy as f32)))
        })
    }

    #[test]
    fn home_is_centred_margin_from_the_configured_edge() {
        let bottom = home_spot(screen(), OverlayPosition::Bottom);
        assert_eq!(
            bottom,
            point(px(1000. + (1920. - WIDTH) / 2.), px(1080. - MARGIN))
        );
        let top = home_spot(screen(), OverlayPosition::Top);
        assert_eq!(top, point(bottom.x, px(MARGIN)));
        for position in POSITIONS {
            // Home needs no clamping on a real screen...
            let home = home_spot(screen(), position);
            let pill = size(px(WIDTH), window_size(5).height - px(2. * INSET));
            assert_eq!(clamp_spot(home, pill, screen(), position), home);
            // ...the popup's home window shows the pill there...
            let window = window_size(3);
            assert_eq!(
                hug(popup_origin(home, window, position), window, position),
                home
            );
            // ...and so does layer-shell's, to the compositor's whole pixel.
            let area = Bounds::new(Point::default(), screen().size);
            let shown = hug(
                layer_home_origin(area.size, window, position),
                window,
                position,
            );
            let home = home_spot(area, position);
            assert!((shown.x - home.x).abs() < px(1.) && (shown.y - home.y).abs() < px(1.));
        }
    }

    #[test]
    fn a_dragged_pill_never_leaves_the_screen() {
        let screen = screen();
        for position in POSITIONS {
            let home = home_spot(screen, position);
            for height in [80., 124., 300.] {
                let pill = size(px(WIDTH), px(height));
                for delta in drags() {
                    let dragged = home + delta;
                    let spot = clamp_spot(dragged, pill, screen, position);
                    let at = pill_at(spot, height, position);
                    assert!(
                        inside(at, screen, EDGE_GAP),
                        "{position:?} {delta:?} {height}: {at:?}"
                    );
                    // Within reach, the pill goes exactly where it was dragged.
                    if inside(pill_at(dragged, height, position), screen, EDGE_GAP) {
                        assert_eq!(spot, dragged);
                    }
                }
            }
        }
    }

    #[test]
    fn corners_are_reachable() {
        let screen = screen();
        let pill = size(px(WIDTH), px(80.));
        let far = point(px(-1e4), px(-1e4));
        let bottom = clamp_spot(far, pill, screen, OverlayPosition::Bottom);
        assert_eq!(bottom, point(px(1000. + EDGE_GAP), px(EDGE_GAP + 80.)));
        let top = clamp_spot(far, pill, screen, OverlayPosition::Top);
        assert_eq!(top, point(px(1000. + EDGE_GAP), px(EDGE_GAP)));
        let far = point(px(1e4), px(1e4));
        let bottom = clamp_spot(far, pill, screen, OverlayPosition::Bottom);
        assert_eq!(
            bottom,
            point(px(2920. - EDGE_GAP - WIDTH), px(1080. - EDGE_GAP))
        );
    }

    #[test]
    fn a_remembered_spot_comes_back_on_any_screen_kept_on_it() {
        let screen = screen();
        let small = Bounds::new(Point::default(), size(px(800.), px(600.)));
        let pill = size(px(WIDTH), px(124.));
        for position in POSITIONS {
            let dragged = home_spot(screen, position) + point(px(-500.), px(-300.));
            let spot = clamp_spot(dragged, pill, screen, position);
            let left = spot - screen.origin;
            // Where it was left; on another screen as large, the same place there.
            assert_eq!(remembered_spot(left, screen, position), spot);
            let other = Bounds::new(point(px(-1920.), px(200.)), screen.size);
            assert_eq!(remembered_spot(left, other, position), other.origin + left);
            // A smaller screen (or a larger scale) keeps the pill on it.
            for left in drags() {
                let spot = remembered_spot(left, small, position);
                let at = pill_at(clamp_spot(spot, pill, small, position), 124., position);
                assert!(inside(at, small, EDGE_GAP), "{position:?} {left:?}: {at:?}");
            }
        }
        let bottom_right = point(px(1300.), px(1000.));
        assert_eq!(
            remembered_spot(bottom_right, small, OverlayPosition::Bottom),
            point(px(800. - EDGE_GAP - WIDTH), px(600. - EDGE_GAP))
        );
    }

    /// Where the flex column lays a pill `height` tall, in window coordinates.
    #[cfg(target_os = "linux")]
    fn laid_out(
        spot: Point<Pixels>,
        height: f32,
        origin: Point<Pixels>,
        window: Size<Pixels>,
        area: Bounds<Pixels>,
        position: OverlayPosition,
    ) -> Bounds<Pixels> {
        let placement = placement(spot, origin, window, area, position);
        let height = px(height);
        let near = placement.near.min(window.height - placement.far - height);
        let top = match position {
            OverlayPosition::Bottom => window.height - near - height,
            OverlayPosition::Top => near,
        };
        Bounds::new(point(placement.left, top), size(px(WIDTH), height))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_dragged_pill_fits_its_window_and_the_screen_as_the_text_grows() {
        let area = Bounds::new(Point::default(), screen().size);
        let window = window_size(3);
        let heights = [80., 102., 124.];
        for position in POSITIONS {
            let home = home_spot(area, position);
            for delta in drags() {
                // Dropped with one line, then the text grows (or the reverse).
                for (dropped, grown) in [(heights[0], heights[2]), (heights[2], heights[0])] {
                    let pill = size(px(WIDTH), px(dropped));
                    let spot = clamp_spot(home + delta, pill, area, position);
                    let origin = window_origin(spot, window, area, position);
                    let (_, margin) = layer_shell::moved(position, origin, window, area.size);
                    let (edge, left) = (margin.0.max(margin.2), margin.3);
                    assert!(edge >= px(0.) && left >= px(0.), "{position:?} {delta:?}");
                    for height in [dropped, grown] {
                        let spot =
                            clamp_spot(home + delta, size(px(WIDTH), px(height)), area, position);
                        let local = laid_out(spot, height, origin, window, area, position);
                        let whole = Bounds::new(Point::default(), window);
                        assert!(
                            inside(local, whole, 0.),
                            "{position:?} {delta:?} {height}: {local:?} in {window:?}"
                        );
                        let on_area = Bounds::new(local.origin + origin, local.size);
                        assert!(
                            inside(on_area, area, EDGE_GAP),
                            "{position:?} {delta:?} {height}: {on_area:?}"
                        );
                        assert_eq!(on_area.origin.x, spot.x);
                    }
                }
            }
        }
    }
}
