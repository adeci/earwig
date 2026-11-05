//! The small on-screen pill shown while dictating. The user can drag it out
//! of the way for the rest of the dictation; the next one starts at home.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use earwig_core::config::{OverlayConfig, OverlayPosition};
use earwig_core::desktop::DesktopCaps;
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Bounds, Context, CursorStyle, DispatchPhase, Div, FontFeatures, FontWeight,
    HighlightStyle, Hsla, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, PlatformDisplay, Point, Render,
    Size, Styled as _, StyledText, Subscription, WeakEntity, Window, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowKind, WindowOptions, canvas, div, point, px, size,
};

use crate::ui::pill::{self, Clock, Edge, Look, Motion};
use crate::ui::x11_popup::X11Popup;
use crate::ui::{OverlaySurface, accent, now_ms};

/// Width of the pill.
const WIDTH: f32 = 560.;
/// Gap between the pill and the screen edge it starts at.
const MARGIN: f32 = 56.;
/// Transparent room around the pill for its moving edge and its shadow.
const INSET: f32 = pill::ROOM;
/// Closest a dragged pill gets to a screen edge, so even its fullest swell
/// stays on screen.
const EDGE_GAP: f32 = pill::MAX_REACH;
const LINE_HEIGHT: f32 = 22.;
/// Frame pacing: smooth while the voice moves the edge, slow when it only
/// breathes, so the overlay never competes with CPU decoding at display rate.
const SPEAKING_FRAME: Duration = Duration::from_millis(16);
const QUIET_FRAME: Duration = Duration::from_millis(50);
const STILL_FRAME: Duration = Duration::from_millis(250);
/// Pixels the edge must be able to travel to be worth `SPEAKING_FRAME`.
const SPEAKING_REACH: f32 = 0.35;
const HEADER_HEIGHT: f32 = 22.;
const PADDING_X: f32 = 18.;
const PADDING_Y: f32 = 14.;
const GAP: f32 = 8.;

/// Window options for the overlay, the one place the platform choice is made.
///
/// With layer-shell the overlay is a transparent surface over the output the
/// compositor picks (the focused one), on the Overlay layer, that never takes
/// keyboard focus and reserves no space; only the pill takes pointer input.
/// Elsewhere it is a borderless popup just big enough for the pill, centred
/// on the primary display.
pub fn overlay_window_options(
    caps: &DesktopCaps,
    config: &OverlayConfig,
    display: Option<&dyn PlatformDisplay>,
) -> WindowOptions {
    let common = WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        app_id: Some("earwig-overlay".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        window_decorations: Some(WindowDecorations::Client),
        ..WindowOptions::default()
    };
    if caps.layer_shell {
        return WindowOptions {
            // Size zero on all four anchors: the compositor stretches the
            // surface over the output, less the bars' exclusive zones, so
            // the pill can be dragged anywhere on it.
            window_bounds: Some(WindowBounds::Windowed(Bounds::default())),
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "earwig".into(),
                layer: Layer::Overlay,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                exclusive_zone: None,
                exclusive_edge: None,
                margin: None,
                keyboard_interactivity: KeyboardInteractivity::None,
            }),
            ..common
        };
    }
    let window_size = popup_size(config.max_lines);
    let origin = display.map_or(point(px(0.), px(0.)), |display| {
        popup_origin(
            home_spot(display.bounds(), config.position),
            window_size,
            config.position,
        )
    });
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: window_size,
        })),
        kind: WindowKind::PopUp,
        display_id: display.map(PlatformDisplay::id),
        ..common
    }
}

/// Where the pill starts on `area`: centred, `MARGIN` from the edge at
/// `position`.
///
/// A spot is the pill's left edge and the y of the edge it hugs (its bottom
/// when it sits at the bottom), which stays put while the text grows.
fn home_spot(area: Bounds<Pixels>, position: OverlayPosition) -> Point<Pixels> {
    let x = area.left() + (area.size.width - px(WIDTH)) / 2.;
    let y = match position {
        OverlayPosition::Bottom => area.bottom() - px(MARGIN),
        OverlayPosition::Top => area.top() + px(MARGIN),
    };
    point(x, y)
}

/// `spot` moved just far enough that a pill of size `pill` keeps `EDGE_GAP`
/// inside `area`. On an area too small for it, its top-left corner stays on.
fn clamp_spot(
    spot: Point<Pixels>,
    pill: Size<Pixels>,
    area: Bounds<Pixels>,
    position: OverlayPosition,
) -> Point<Pixels> {
    let gap = px(EDGE_GAP);
    let fit = |value: Pixels, low: Pixels, high: Pixels| value.min(high).max(low);
    let x = fit(spot.x, area.left() + gap, area.right() - gap - pill.width);
    let y = match position {
        OverlayPosition::Bottom => fit(
            spot.y,
            area.top() + gap + pill.height,
            area.bottom() - gap,
        ),
        OverlayPosition::Top => fit(
            spot.y,
            area.top() + gap,
            area.bottom() - gap - pill.height,
        ),
    };
    point(x, y)
}

/// Top-left corner of the popup that shows the pill at `spot`: the pill
/// hugs the popup's edge at `position`, `INSET` inside it.
fn popup_origin(
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

/// The popup: room for the header, `max_lines` of text and the inset around
/// the pill, which shrinks to its content inside.
#[allow(clippy::cast_precision_loss)]
fn popup_size(max_lines: u32) -> Size<Pixels> {
    let height =
        2. * INSET + PADDING_Y * 2. + HEADER_HEIGHT + GAP + LINE_HEIGHT * max_lines.max(1) as f32;
    size(px(WIDTH + 2. * INSET), px(height))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Inserted,
    Failed { on_clipboard: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Recording,
    /// Recording stopped at `stopped_at_ms`; final text on its way.
    Finalizing,
    Done(Outcome),
}

pub struct OverlayView {
    max_lines: u32,
    position: OverlayPosition,
    phase: Phase,
    started_at_ms: u64,
    stopped_at_ms: Option<u64>,
    committed: String,
    tentative: String,
    /// Latest microphone level.
    level: f32,
    clock: Clock,
    motion: Motion,
    /// Fill opacity setting.
    opacity: f32,
    frame_pending: bool,
    /// The pill's colours as they fade toward the current phase's.
    look: Option<Look>,
    tabular_numbers: FontFeatures,
    surface: OverlaySurface,
    /// The X11 popup, when it can be moved.
    x11: Option<X11Popup>,
    /// Where the user dragged the pill this dictation (see [`home_spot`]),
    /// in the coordinates of [`Self::area`]; `None` keeps it at home.
    spot: Option<Point<Pixels>>,
    /// While dragging: the pointer's offset from the spot it grabbed.
    grab: Option<Point<Pixels>>,
    /// The pill's bounds as last painted, in window coordinates.
    painted: Rc<Cell<Option<Bounds<Pixels>>>>,
    _appearance: Subscription,
}

impl OverlayView {
    pub fn new(
        config: &OverlayConfig,
        surface: OverlaySurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
        });
        let x11 = match surface {
            OverlaySurface::X11Popup => X11Popup::new(window)
                .inspect_err(|e| log::warn!("the overlay stays where it is: {e:#}"))
                .ok(),
            _ => None,
        };
        Self {
            max_lines: config.max_lines.max(1),
            position: config.position,
            phase: Phase::Recording,
            started_at_ms: now_ms(),
            stopped_at_ms: None,
            committed: String::new(),
            tentative: String::new(),
            level: 0.,
            clock: Clock::default(),
            motion: Motion::new(config.motion()),
            opacity: config.opacity(),
            frame_pending: false,
            look: None,
            tabular_numbers: FontFeatures(Arc::new(vec![("tnum".into(), 1)])),
            surface,
            x11,
            spot: None,
            grab: None,
            painted: Rc::default(),
            _appearance: appearance,
        }
    }

    /// Recording (re)started. A different start time is a new recording: it
    /// clears the previous one's text and brings the pill back home.
    pub fn recording(&mut self, started_at_ms: u64) {
        if started_at_ms != self.started_at_ms || !self.is_recording() {
            self.committed.clear();
            self.tentative.clear();
            self.level = 0.;
            self.stopped_at_ms = None;
            self.started_at_ms = started_at_ms;
            self.spot = None;
            self.grab = None;
        }
        self.phase = Phase::Recording;
    }

    pub fn finalizing(&mut self) {
        self.phase = Phase::Finalizing;
        self.stopped_at_ms.get_or_insert_with(now_ms);
    }

    pub fn finish(&mut self, outcome: Outcome) {
        self.stopped_at_ms.get_or_insert_with(now_ms);
        self.phase = Phase::Done(outcome);
    }

    pub fn set_text(&mut self, committed: String, tentative: String) {
        self.committed = committed;
        self.tentative = tentative;
    }

    pub fn push_level(&mut self, level: f32) {
        self.level = level.clamp(0., 1.);
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording)
    }

    pub fn has_outcome(&self) -> bool {
        matches!(self.phase, Phase::Done(_))
    }

    fn elapsed(&self) -> Duration {
        let end = self.stopped_at_ms.unwrap_or_else(now_ms);
        Duration::from_millis(end.saturating_sub(self.started_at_ms))
    }

    fn header(&self, theme: &Theme) -> Div {
        let status = match self.phase {
            Phase::Done(Outcome::Failed { .. }) => Some("Not inserted"),
            _ => None,
        };
        let dot = theme.danger;
        let words = word_count(&self.committed) + word_count(&self.tentative);
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(HEADER_HEIGHT))
            .text_size(px(13.))
            .text_color(theme.muted_foreground)
            .child(div().size(px(8.)).rounded_full().bg(dot))
            .child(
                div()
                    .text_color(theme.foreground)
                    .font_weight(FontWeight::MEDIUM)
                    .font_features(self.tabular_numbers.clone())
                    .child(format_elapsed(self.elapsed())),
            )
            .child(format!(
                "{words} {}",
                if words == 1 { "word" } else { "words" }
            ))
            .when_some(status, |this, status| {
                this.child(div().text_color(dot).child(status))
            })
    }

    fn body(&self, theme: &Theme) -> Div {
        match self.phase {
            Phase::Done(Outcome::Failed { on_clipboard }) => {
                div().text_color(theme.foreground).child(if on_clipboard {
                    "Couldn't type into the app. The text is on the clipboard."
                } else {
                    "Couldn't insert the text. It is saved in History."
                })
            }
            _ if self.committed.is_empty() && self.tentative.is_empty() => div()
                .text_color(theme.muted_foreground)
                .child(if self.is_recording() {
                    "Listening…"
                } else {
                    ""
                }),
            _ => div().text_color(theme.foreground).child(live_text(
                &self.committed,
                &self.tentative,
                theme.muted_foreground,
            )),
        }
    }

    fn movable(&self) -> bool {
        self.surface == OverlaySurface::LayerShell || self.x11.is_some()
    }

    /// The area the pill moves on: the layer surface, which spans the
    /// output, or the X screen the popup sits on.
    fn area(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if self.x11.is_some() {
            window.display(cx).map(|display| display.bounds())
        } else {
            Some(Bounds::new(Point::default(), window.viewport_size()))
        }
    }

    /// The pointer in the coordinates of [`Self::area`], given an event at
    /// `local` in the window.
    fn pointer(&self, local: Point<Pixels>, window: &Window) -> Option<Point<Pixels>> {
        match &self.x11 {
            Some(popup) => popup.pointer(window.scale_factor()),
            None => Some(local),
        }
    }

    /// Where the pill is now: its spot kept on `area` at its current size.
    fn current_spot(&self, area: Bounds<Pixels>) -> Point<Pixels> {
        let pill = self
            .painted
            .get()
            .map_or(size(px(WIDTH), px(0.)), |painted| painted.size);
        let spot = self
            .spot
            .unwrap_or_else(|| home_spot(area, self.position));
        clamp_spot(spot, pill, area, self.position)
    }

    fn start_drag(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(pointer), Some(area)) = (self.pointer(event.position, window), self.area(window, cx))
        else {
            return;
        };
        self.grab = Some(pointer - self.current_spot(area));
        cx.notify();
    }

    fn drag(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(grab) = self.grab else {
            return;
        };
        // The button came up where we did not see it (the compositor ended
        // the grab): the drag is over.
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        if let Some(pointer) = self.pointer(event.position, window) {
            self.spot = Some(pointer - grab);
            cx.notify();
        }
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.grab.take().is_some() {
            cx.notify();
        }
    }

    /// Move the popup to the pill's spot once it was dragged (or back home
    /// for a new dictation).
    fn place_popup(&mut self, window: &Window, cx: &App) {
        let Some(popup) = &self.x11 else {
            return;
        };
        if self.spot.is_none() && !popup.is_moved() {
            return;
        }
        let Some(area) = self.area(window, cx) else {
            return;
        };
        let origin = popup_origin(
            self.current_spot(area),
            popup_size(self.max_lines),
            self.position,
        );
        if let Some(popup) = &mut self.x11 {
            popup.move_to(origin, window.scale_factor());
        }
    }

    /// Re-render after `interval`; the window closing drops the entity and
    /// ends the loop.
    fn schedule_frame(&mut self, interval: Duration, cx: &mut Context<Self>) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(interval).await;
            let _ = this.update(cx, |view, cx| {
                view.frame_pending = false;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dt = self.clock.tick(Instant::now());
        let edge = self
            .motion
            .step(dt, self.is_recording().then_some(self.level));
        let (edge, frame) = if pill::holds_still(window, cx) {
            (Edge::default(), STILL_FRAME)
        } else if edge.reach() > SPEAKING_REACH {
            (edge, SPEAKING_FRAME)
        } else {
            (edge, QUIET_FRAME)
        };
        self.schedule_frame(frame, cx);
        let accent = accent::get(cx);
        let theme = cx.theme();
        let target = Look::themed(theme, accent.unwrap_or(theme.primary), self.opacity);
        let look = self.look.map_or(target, |look| look.fade_to(target, dt));
        self.look = Some(look);
        let header = self.header(theme);
        let body = self.body(theme);

        let painted = self.painted.clone();
        let dragging = self.grab.is_some();
        let view = cx.entity().downgrade();
        let pill = div()
            .w(px(WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .when(self.movable(), |this| {
                this.cursor(CursorStyle::OpenHand)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::start_drag))
            })
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| {
                        pill::paint(bounds, edge, look, 1., window);
                        if painted.get() != Some(bounds) {
                            // Only the pill takes the pointer; everywhere
                            // else clicks reach whatever is below.
                            window.set_input_region(Some(&[bounds]));
                            if painted.get().is_some_and(|last| last.size != bounds.size) {
                                // Clamp the new size on screen right away.
                                window.refresh();
                            }
                            painted.set(Some(bounds));
                        }
                        if dragging {
                            follow_drag(view, window);
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(header)
            .child(
                // Only the last `max_lines` wrapped lines: anchor the text to
                // the bottom and clip what scrolls off the top.
                #[allow(clippy::cast_precision_loss)]
                div()
                    .max_h(px(LINE_HEIGHT * self.max_lines as f32))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .text_size(px(15.))
                    .line_height(px(LINE_HEIGHT))
                    .child(body),
            );

        if self.surface == OverlaySurface::LayerShell {
            // The surface spans the output and the pill sits at its spot.
            // Between the pill and the edge it hugs, a spacer that gives way
            // when growing text would push the pill past the far edge.
            let area = Bounds::new(Point::default(), window.viewport_size());
            let spot = self.current_spot(area);
            let near = match self.position {
                OverlayPosition::Bottom => area.bottom() - spot.y,
                OverlayPosition::Top => spot.y - area.top(),
            };
            return div()
                .size_full()
                .p(px(EDGE_GAP))
                .flex()
                .map(|this| match self.position {
                    OverlayPosition::Bottom => this.flex_col(),
                    OverlayPosition::Top => this.flex_col_reverse(),
                })
                .items_start()
                .child(div().flex_grow_1().flex_basis(px(0.)))
                .child(pill.ml(spot.x - px(EDGE_GAP)))
                .child(div().h((near - px(EDGE_GAP)).max(px(0.))));
        }

        // The popup is just big enough; the pill hugs its edge at `position`.
        self.place_popup(window, cx);
        div()
            .size_full()
            .p(px(INSET))
            .flex()
            .flex_col()
            .map(|this| match self.position {
                OverlayPosition::Bottom => this.justify_end(),
                OverlayPosition::Top => this.justify_start(),
            })
            .child(pill)
    }
}

/// While a drag lasts, follow the pointer anywhere in the window (the pill
/// trails it by a frame) and end on release. Registered each painted frame.
fn follow_drag(view: WeakEntity<OverlayView>, window: &mut Window) {
    window.set_window_cursor_style(CursorStyle::ClosedHand);
    let moved = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            let _ = moved.update(cx, |view, cx| view.drag(event, window, cx));
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
            let _ = view.update(cx, OverlayView::end_drag);
        }
    });
}

/// Committed text in full colour followed by the tentative tail dimmed.
fn live_text(committed: &str, tentative: &str, dim: Hsla) -> StyledText {
    let separator = if committed.is_empty()
        || tentative.is_empty()
        || committed.ends_with(char::is_whitespace)
        || tentative.starts_with(char::is_whitespace)
    {
        ""
    } else {
        " "
    };
    let text = format!("{committed}{separator}{tentative}");
    let tail = committed.len() + separator.len()..text.len();
    StyledText::new(text).with_highlights([(
        tail,
        HighlightStyle {
            color: Some(dim),
            ..HighlightStyle::default()
        },
    )])
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn elapsed_rolls_over_minutes() {
        assert_eq!(format_elapsed(Duration::from_millis(59_999)), "0:59");
        assert_eq!(format_elapsed(Duration::from_secs(61)), "1:01");
        assert_eq!(format_elapsed(Duration::from_secs(600)), "10:00");
    }

    #[test]
    fn home_is_centred_margin_from_the_configured_edge() {
        let bottom = home_spot(screen(), OverlayPosition::Bottom);
        assert_eq!(bottom, point(px(1000. + (1920. - WIDTH) / 2.), px(1080. - MARGIN)));
        let top = home_spot(screen(), OverlayPosition::Top);
        assert_eq!(top, point(bottom.x, px(MARGIN)));
        // Home needs no clamping on a real screen.
        for position in [OverlayPosition::Bottom, OverlayPosition::Top] {
            let home = home_spot(screen(), position);
            let pill = size(px(WIDTH), popup_size(5).height - px(2. * INSET));
            assert_eq!(clamp_spot(home, pill, screen(), position), home);
        }
    }

    #[test]
    fn popup_is_centred_and_keeps_its_margin_from_the_anchored_edge() {
        let window = popup_size(3);
        let spot = home_spot(screen(), OverlayPosition::Bottom);
        let bottom = popup_origin(spot, window, OverlayPosition::Bottom);
        assert_eq!(bottom.x + px(INSET), px(1000. + (1920. - WIDTH) / 2.));
        // The pill itself sits `MARGIN` from the edge, its inset inside that.
        assert_eq!(bottom.y + window.height - px(INSET), px(1080. - MARGIN));
        let spot = home_spot(screen(), OverlayPosition::Top);
        let top = popup_origin(spot, window, OverlayPosition::Top);
        assert_eq!(top, point(bottom.x, px(MARGIN - INSET)));
    }

    #[test]
    fn a_dragged_pill_never_leaves_the_screen() {
        let screen = screen();
        // A hair of slack for float rounding.
        let gap = px(EDGE_GAP - 1e-3);
        let on_screen = |pill: Bounds<Pixels>| {
            pill.left() >= screen.left() + gap
                && pill.right() <= screen.right() - gap
                && pill.top() >= screen.top() + gap
                && pill.bottom() <= screen.bottom() - gap
        };
        for position in [OverlayPosition::Bottom, OverlayPosition::Top] {
            let home = home_spot(screen, position);
            for height in [80., 124., 300.] {
                let pill = size(px(WIDTH), px(height));
                for dx in (-4000..=4000).step_by(250) {
                    for dy in (-3000..=3000).step_by(125) {
                        #[allow(clippy::cast_precision_loss)]
                        let dragged = home + point(px(dx as f32), px(dy as f32));
                        let spot = clamp_spot(dragged, pill, screen, position);
                        let at = pill_at(spot, height, position);
                        assert!(on_screen(at), "{position:?} {dx},{dy} {height}: {at:?}");
                        // Within reach, the pill goes exactly where it was dragged.
                        if on_screen(pill_at(dragged, height, position)) {
                            assert_eq!(spot, dragged);
                        }
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
        assert_eq!(bottom, point(px(2920. - EDGE_GAP - WIDTH), px(1080. - EDGE_GAP)));
    }
}
