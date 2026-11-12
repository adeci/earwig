//! A line of text that ends in an ellipsis when its room is too narrow and,
//! while the pointer rests on it, scrolls sideways to show the rest, like an
//! old information display: it holds still a moment, eases into a steady
//! pace, pauses at the end, comes back, and starts over until the pointer
//! leaves. Under reduced motion it holds still and a tooltip shows the whole
//! text instead. Either way it takes the room the ellipsised line would.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    App, AvailableSpace, Bounds, ContentMask, DispatchPhase, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, InspectorElementId, Interactivity, IntoElement, LayoutId,
    MouseExitEvent, MouseMoveEvent, ParentElement as _, Pixels, Role, ShapedLine, SharedString,
    StyleRefinement, Styled, TextAlign, TextStyle, TruncateFrom, Window, accesskit, div, point, px,
    size,
};

/// How long the line holds still at its start: once the pointer is on it,
/// and between passes.
const START_PAUSE: f32 = 0.8;
/// How long it holds still at its end before coming back.
const END_PAUSE: f32 = 1.2;
/// The steady pace, in pixels per second.
const SPEED: f32 = 50.;
/// How long it takes to reach that pace, and to stop from it.
const RAMP: f32 = 0.4;
const ELLIPSIS: &str = "…";
/// Where the reduced-motion tooltip wraps a long text.
const TIP_WIDTH: Pixels = px(360.);

/// One line of `text`, as wide as its room allows; `id` keeps its motion
/// from frame to frame.
pub fn marquee(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Marquee {
    let mut text: SharedString = text.into();
    if text.contains('\n') {
        text = text.replace('\n', " ").into();
    }
    let mut interactivity = Interactivity::new();
    interactivity.element_id = Some(id.into());
    Marquee {
        interactivity,
        text,
    }
    .min_w_0()
}

pub struct Marquee {
    interactivity: Interactivity,
    text: SharedString,
}

/// What a frame paints.
pub struct Lines {
    hitbox: Hitbox,
    height: Pixels,
    /// The whole text on one line.
    whole: ShapedLine,
    /// What fits: the whole text, or its start and an ellipsis.
    fitted: ShapedLine,
}

/// What a marquee keeps from one frame to the next.
#[derive(Default)]
struct Motion {
    /// When the pointer came to rest on the line while it was cut short.
    since: Option<Instant>,
    /// Whether the last frame cut the line short.
    truncated: bool,
    /// Whether the pointer left the window, which moves nothing in it.
    away: Rc<Cell<bool>>,
}

/// The current text style, for one line.
struct Line {
    style: TextStyle,
    font_size: Pixels,
    height: Pixels,
}

impl Line {
    fn current(window: &Window) -> Self {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let height = window.pixel_snap(
            style
                .line_height
                .to_pixels(font_size.into(), window.rem_size()),
        );
        Self {
            style,
            font_size,
            height,
        }
    }

    fn whole(&self, text: &SharedString, window: &Window) -> ShapedLine {
        let runs = [self.style.to_run(text.len())];
        window
            .text_system()
            .shape_line(text.clone(), self.font_size, &runs, None)
    }

    /// `whole` as it fits in `room`: as it is, or the start of `text` and
    /// an ellipsis, the way GPUI truncates text.
    fn fit(
        &self,
        text: &SharedString,
        whole: ShapedLine,
        room: Option<Pixels>,
        window: &Window,
        cx: &App,
    ) -> ShapedLine {
        let Some(room) = room.filter(|room| whole.width > *room) else {
            return whole;
        };
        let runs = [self.style.to_run(text.len())];
        let mut wrapper = cx
            .text_system()
            .line_wrapper(self.style.font(), self.font_size);
        let (cut, runs) =
            wrapper.truncate_line(text.clone(), room, ELLIPSIS, &runs, TruncateFrom::End);
        window
            .text_system()
            .shape_line(cut, self.font_size, &runs, None)
    }
}

impl Element for Marquee {
    type RequestLayoutState = ();
    type PrepaintState = Lines;

    fn id(&self) -> Option<ElementId> {
        self.interactivity.element_id.clone()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.interactivity.source_location()
    }

    fn a11y_role(&self) -> Option<Role> {
        Some(Role::Label)
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        node.set_value(self.text.to_string());
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let truncated = id.is_some_and(|id| {
            window.with_element_state(id, |motion: Option<Motion>, _| {
                let motion = motion.unwrap_or_default();
                (motion.truncated, motion)
            })
        });
        if truncated && cx.reduce_motion() {
            let text = self.text.clone();
            self.interactivity.tooltip(move |window, cx| {
                let text = text.clone();
                Tooltip::element(move |_, _| div().max_w(TIP_WIDTH).child(text.clone()))
                    .build(window, cx)
            });
        }
        let text = self.text.clone();
        let layout_id =
            self.interactivity
                .request_layout(id, inspector_id, window, cx, |style, window, _| {
                    let line = window.with_text_style(style.text_style().cloned(), |window| {
                        Line::current(window)
                    });
                    window.request_measured_layout(style, move |known, available, window, cx| {
                        let width = known.width.unwrap_or_else(|| {
                            let room = match available.width {
                                AvailableSpace::Definite(room) => Some(room),
                                AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
                            };
                            let whole = line.whole(&text, window);
                            line.fit(&text, whole, room, window, cx).width.ceil()
                        });
                        size(width, line.height)
                    })
                });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Lines {
        let text = self.text.clone();
        self.interactivity.prepaint(
            id,
            inspector_id,
            bounds,
            bounds.size,
            window,
            cx,
            |_, _, hitbox, window, cx| {
                let line = Line::current(window);
                let whole = line.whole(&text, window);
                let fitted = line.fit(&text, whole.clone(), Some(bounds.size.width), window, cx);
                Lines {
                    hitbox: hitbox
                        .unwrap_or_else(|| window.insert_hitbox(bounds, HitboxBehavior::Normal)),
                    height: line.height,
                    whole,
                    fitted,
                }
            },
        )
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        lines: &mut Lines,
        window: &mut Window,
        cx: &mut App,
    ) {
        let moves = !cx.reduce_motion();
        self.interactivity.paint(
            id,
            inspector_id,
            bounds,
            Some(&lines.hitbox),
            window,
            cx,
            |_, window, cx| {
                let overflow = lines.whole.width - bounds.size.width;
                let scrolls = moves && overflow > px(0.);
                let now = Instant::now();
                let hitbox = lines.hitbox.clone();
                let mut hovered = false;
                let mut offset = px(0.);
                let mut away = Rc::default();
                if let Some(id) = id {
                    window.with_element_state(id, |motion: Option<Motion>, window| {
                        let mut motion = motion.unwrap_or_default();
                        hovered = hitbox.is_hovered(window) && !motion.away.get();
                        motion.truncated = overflow > px(0.);
                        motion.since = (hovered && scrolls).then(|| motion.since.unwrap_or(now));
                        if let Some(since) = motion.since {
                            offset = px(self::offset(now - since, overflow.as_f32()));
                        }
                        away = motion.away.clone();
                        ((), motion)
                    });
                }
                if offset > px(0.) {
                    let origin = point(bounds.origin.x - offset, bounds.origin.y);
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        let _ = lines.whole.paint(
                            origin,
                            lines.height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    });
                } else {
                    let _ = lines.fitted.paint(
                        bounds.origin,
                        lines.height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                if !scrolls {
                    return;
                }
                if hovered {
                    window.request_animation_frame();
                }
                // Draw again when the pointer comes or goes.
                let view = window.current_view();
                let left = Rc::clone(&away);
                window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        away.set(false);
                        if hitbox.is_hovered(window) != hovered {
                            cx.notify(view);
                        }
                    }
                });
                window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture && hovered {
                        left.set(true);
                        cx.notify(view);
                    }
                });
            },
        );
    }
}

impl IntoElement for Marquee {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for Marquee {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.interactivity.base_style
    }
}

/// How far a line whose end lies `overflow` pixels past its room has
/// scrolled `elapsed` after the pointer came to rest on it: still at the
/// start, over to the end, still there, and back, over and over.
fn offset(elapsed: Duration, overflow: f32) -> f32 {
    if overflow <= 0. {
        return 0.;
    }
    let travel = travel_time(overflow);
    let t = elapsed.as_secs_f32() % (START_PAUSE + travel + END_PAUSE + travel);
    if t < START_PAUSE {
        0.
    } else if t < START_PAUSE + travel {
        eased(t - START_PAUSE, overflow)
    } else if t < START_PAUSE + travel + END_PAUSE {
        overflow
    } else {
        overflow - eased(t - START_PAUSE - travel - END_PAUSE, overflow)
    }
}

/// How long one pass over `distance` takes.
fn travel_time(distance: f32) -> f32 {
    // Speeding up to SPEED and slowing down again covers this much.
    let ramps = SPEED * RAMP;
    if distance >= ramps {
        2. * RAMP + (distance - ramps) / SPEED
    } else {
        2. * (distance * RAMP / SPEED).sqrt()
    }
}

/// How far a pass over `distance` has come `t` seconds in: it speeds up
/// evenly, holds `SPEED` (when the distance is long enough to reach it),
/// and slows down evenly to a stop.
fn eased(t: f32, distance: f32) -> f32 {
    let accel = SPEED / RAMP;
    let total = travel_time(distance);
    let ramp = (total / 2.).min(RAMP);
    let t = t.clamp(0., total);
    if t < ramp {
        accel * t * t / 2.
    } else if t > total - ramp {
        let left = total - t;
        distance - accel * left * left / 2.
    } else {
        accel * ramp * ramp / 2. + (t - ramp) * SPEED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: f32, overflow: f32) -> f32 {
        offset(Duration::from_secs_f32(seconds), overflow)
    }

    #[test]
    fn a_line_that_fits_never_moves() {
        for t in [0., 1., 2.5, 10.] {
            assert!(at(t, 0.).abs() < f32::EPSILON);
            assert!(at(t, -12.).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn it_waits_before_moving() {
        assert!(at(0., 200.).abs() < f32::EPSILON);
        assert!(at(START_PAUSE - 0.01, 200.).abs() < f32::EPSILON);
        assert!(at(START_PAUSE + 0.05, 200.) > 0.);
    }

    #[test]
    fn it_eases_into_a_steady_pace() {
        let start = START_PAUSE;
        // Slower than the steady pace while speeding up.
        assert!(at(start + 0.1, 200.) < SPEED * 0.1);
        // The steady pace once there.
        let moved = at(start + 2., 200.) - at(start + 1., 200.);
        assert!((moved - SPEED).abs() < 0.01, "{moved}");
    }

    #[test]
    fn it_stops_at_the_end_then_comes_back() {
        let overflow = 120.;
        let travel = travel_time(overflow);
        let end = START_PAUSE + travel;
        assert!((at(end, overflow) - overflow).abs() < 0.01);
        assert!((at(end + END_PAUSE - 0.01, overflow) - overflow).abs() < f32::EPSILON);
        let back = at(end + END_PAUSE + travel / 2., overflow);
        assert!(back > 0. && back < overflow);
        let cycle = end + END_PAUSE + travel;
        assert!(at(cycle - 0.001, overflow) < 0.01);
        // And over again.
        assert!(
            (at(cycle + START_PAUSE + 0.3, overflow) - at(START_PAUSE + 0.3, overflow)).abs()
                < 0.01
        );
    }

    #[test]
    fn a_short_overflow_moves_smoothly_without_overshooting() {
        let overflow = 6.;
        let travel = travel_time(overflow);
        let mut last = 0.;
        for step in 0_u8..=100 {
            let now = at(START_PAUSE + travel * f32::from(step) / 100., overflow);
            assert!(now >= last - 1e-4 && now <= overflow + 1e-4, "{now}");
            assert!(now - last < 0.5, "jumped {last} -> {now}");
            last = now;
        }
        assert!((last - overflow).abs() < 0.01);
    }
}
