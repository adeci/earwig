//! The small on-screen pill shown while dictating. The user can drag it out
//! of the way for the rest of the dictation; the next one starts at home.
//! Hovering it shows a × that throws the dictation away.
//!
//! The pill's window is just big enough for it: GPUI's path rendering costs
//! in proportion to the window's area, which makes an output-sized overlay
//! expensive (very much so on software rendering). On layer-shell, where a
//! surface cannot move once mapped, a drag shows the pill on a transparent
//! surface over the output's usable area while the button is down, drawn
//! with quads only, then reopens the small window where it was dropped (see
//! [`drag`](crate::ui::drag) for the hand-over). On X11 the popup itself
//! follows the pointer.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use earwig_core::config::{OverlayConfig, OverlayPosition, OverlayStyle};
use earwig_core::desktop::DesktopCaps;
use earwig_core::spectrum::Feed;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Theme};
use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, CursorStyle, DispatchPhase, Div,
    Entity, FontFeatures, FontWeight, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, PlatformDisplay, Point, Render, Size, StatefulInteractiveElement as _, Styled as _,
    StyledText, Subscription, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowId, WindowKind, WindowOptions, canvas, div, point, px, size,
};

use crate::ui::drag::{Drag, Grab, Pointer};
use crate::ui::layer_area;
use crate::ui::pill::{self, Clock, Look, Motion, Shape};
use crate::ui::x11_popup::X11Popup;
use crate::ui::{OverlaySurface, accent, now_ms};

/// Width of the pill.
const WIDTH: f32 = 560.;
/// Gap between the pill and the edge of the screen (less bars) it starts
/// at; at least `INSET`, so the window never reaches past that edge.
const MARGIN: f32 = 56.;
/// Transparent room around the pill for its bars or moving edge and its
/// shadow.
const INSET: f32 = pill::ROOM;
/// Gap between the window and the edge it starts at.
const WINDOW_MARGIN: f32 = MARGIN - INSET;
const _: () = assert!(WINDOW_MARGIN >= 0.);
/// Closest a dragged pill gets to an edge, so even its fullest swell stays
/// on screen.
const EDGE_GAP: f32 = pill::MAX_REACH;
const LINE_HEIGHT: f32 = 22.;
/// Frame pacing: every display frame while the voice moves the pill, slow
/// otherwise, so the overlay never competes with CPU decoding at display
/// rate. A voice wakes a slow overlay at once (see [`OverlayView::hears`]).
const QUIET_FRAME: Duration = Duration::from_millis(50);
const STILL_FRAME: Duration = Duration::from_millis(250);
/// The drag surface's frame interval on software rendering, where every
/// frame of a surface that size costs real CPU.
const SOFTWARE_DRAG_FRAME: Duration = Duration::from_millis(100);
/// How long the drag surface waits, after a drop, for the pill's window to
/// show it before closing anyway.
const LANDING_TIMEOUT: Duration = Duration::from_millis(500);
/// Group name of the pill, whose hover shows the discard button.
const PILL: &str = "pill";
const DISCARD_SIZE: f32 = 24.;
/// Pixels the voice must move the pill by to be worth every frame.
const SPEAKING_REACH: f32 = 0.35;
const HEADER_HEIGHT: f32 = 22.;
const PADDING_X: f32 = 18.;
const PADDING_Y: f32 = 14.;
const GAP: f32 = 8.;

/// Open the overlay's window at home.
pub fn open(
    config: &OverlayConfig,
    caps: &DesktopCaps,
    surface: OverlaySurface,
    voice: Feed,
    cx: &mut App,
) -> anyhow::Result<Entity<OverlayView>> {
    let options = home_window_options(caps, config, cx.primary_display().as_deref());
    // No Base root: it paints an opaque background, and the overlay has no
    // inputs or popovers that need one.
    let mut view = None;
    let handle = cx.open_window(options, |window, cx| {
        Theme::sync_system_appearance(Some(window), cx);
        // Click-through until the pill is painted and takes its own bounds.
        window.set_input_region(Some(&[]));
        let root = cx.new(|cx| OverlayView::new(config, surface, voice, window, cx));
        view = Some(root.clone());
        root
    })?;
    let view = view.expect("the window was built");
    view.update(cx, |view, _| view.window = Some(handle.into()));
    Ok(view)
}

/// Window options for the overlay at home, the one place the platform
/// choice is made.
///
/// With layer-shell the pill floats on the Overlay layer of the output the
/// compositor picks (the focused one), never takes keyboard focus, and
/// reserves no space. Elsewhere it is a borderless popup centred on the
/// primary display.
fn home_window_options(
    caps: &DesktopCaps,
    config: &OverlayConfig,
    display: Option<&dyn PlatformDisplay>,
) -> WindowOptions {
    let window_size = window_size(config.max_lines);
    if caps.layer_shell {
        // One edge only: layer-shell centres the surface along it.
        return layer_options(
            edge_anchor(config.position),
            margins(config.position, px(WINDOW_MARGIN), px(0.)),
            window_size,
        );
    }
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
        ..base_options()
    }
}

fn base_options() -> WindowOptions {
    WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        app_id: Some("earwig-overlay".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        window_decorations: Some(WindowDecorations::Client),
        // The pill never has focus, and GPUI holds unfocused windows to 30
        // fps, which would also hold back the first frame of a voice by up
        // to 33 ms. The overlay paces itself.
        inactive_frame_interval: None,
        ..WindowOptions::default()
    }
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

/// The window at `origin` on the usable `area` (layer-shell), where a pill
/// was dropped.
fn moved_window_options(
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
/// drag (layer-shell); it takes no input.
fn drag_window_options(position: OverlayPosition, area: Size<Pixels>) -> WindowOptions {
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

/// Room for the header, `max_lines` of text and the inset around the pill,
/// in whole pixels as the compositor sizes it; the pill inside shrinks to
/// its content.
#[allow(clippy::cast_precision_loss)]
fn window_size(max_lines: u32) -> Size<Pixels> {
    let height =
        2. * INSET + PADDING_Y * 2. + HEADER_HEIGHT + GAP + LINE_HEIGHT * max_lines.max(1) as f32;
    size(px((WIDTH + 2. * INSET).floor()), px(height.floor()))
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

/// Where layer-shell puts the home window on a usable area of size `area`:
/// centred along the edge at `position` in whole pixels, `WINDOW_MARGIN`
/// (truncated, as GPUI sends it) from it.
fn layer_home_origin(
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

/// The spot of a pill that hugs the edge at `position` of a window at
/// `origin`, `INSET` inside it.
fn hug(origin: Point<Pixels>, window: Size<Pixels>, position: OverlayPosition) -> Point<Pixels> {
    let y = match position {
        OverlayPosition::Bottom => origin.y + window.height - px(INSET),
        OverlayPosition::Top => origin.y + px(INSET),
    };
    point(origin.x + px(INSET), y)
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
        OverlayPosition::Bottom => fit(spot.y, area.top() + gap + pill.height, area.bottom() - gap),
        OverlayPosition::Top => fit(spot.y, area.top() + gap, area.bottom() - gap - pill.height),
    };
    point(x, y)
}

/// Top-left corner of the window that shows the pill at `spot` hugging its
/// edge at `position`.
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

/// The window for a pill at `spot`: [`popup_origin`], kept on `area` in
/// whole pixels. Near an edge the pill sits off centre in it instead.
fn window_origin(
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

/// How a pill at `spot` lays out in a window at `origin`, both on `area`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    /// From the window's left edge to the pill's.
    left: Pixels,
    /// From the window's edge at `position` to the pill's, given way when
    /// the pill grows into `far`.
    near: Pixels,
    /// Room the pill leaves at the window's other end, so that it keeps
    /// `EDGE_GAP` from the area's far edge however tall the text grows.
    far: Pixels,
}

fn placement(
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
    /// The voice of the current recording.
    voice: Feed,
    clock: Clock,
    style: OverlayStyle,
    motion: Motion,
    /// What the voice did to the pill as of the last tick.
    shape: Shape,
    /// The pill holds still (reduced motion, software rendering).
    still: bool,
    /// Fill opacity and border width settings.
    opacity: f32,
    border: f32,
    frame_pending: bool,
    /// The pill's colours as they fade toward the current phase's.
    look: Option<Look>,
    tabular_numbers: FontFeatures,
    surface: OverlaySurface,
    /// The X11 popup, when it can be moved.
    x11: Option<X11Popup>,
    /// The output's usable area (layer-shell), once known; until then the
    /// pill stays put.
    usable: Option<Size<Pixels>>,
    /// The window the pill rests in, and where it sits on the area (`None`
    /// where it opened).
    window: Option<AnyWindowHandle>,
    origin: Option<Point<Pixels>>,
    /// Where the user dragged the pill this dictation (see [`home_spot`]);
    /// `None` keeps it at home.
    spot: Option<Point<Pixels>>,
    drag: Drag,
    /// When the pill last followed the pointer.
    moved_at: Option<Instant>,
    /// The surface showing the pill while a drag lasts and until its window
    /// shows it again (layer-shell).
    drag_window: Option<AnyWindowHandle>,
    /// The window the pill rested in before it was dropped elsewhere, which
    /// closes with the drag surface.
    left_behind: Option<AnyWindowHandle>,
    painted: Painted,
    /// Follows the system appearance through the resting window.
    appearance: Subscription,
}

/// The pill's bounds as last painted in a resting window, and which one:
/// each new window needs its input region set.
type Painted = Rc<Cell<Option<(WindowId, Bounds<Pixels>)>>>;

/// What a window draws the pill for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// The pill at rest: animated, takes the pointer.
    Rest,
    /// The pill on the drag surface: static quads, no input.
    Drag,
}

impl OverlayView {
    pub fn new(
        config: &OverlayConfig,
        surface: OverlaySurface,
        voice: Feed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let x11 = match surface {
            OverlaySurface::X11Popup => X11Popup::new(window)
                .inspect_err(|e| log::warn!("the overlay stays where it is: {e:#}"))
                .ok(),
            _ => None,
        };
        if surface == OverlaySurface::LayerShell {
            cx.spawn(async move |this, cx| {
                let area = cx
                    .background_executor()
                    .spawn(async { layer_area::usable_area() })
                    .await;
                match area {
                    #[allow(clippy::cast_precision_loss)]
                    Ok((width, height)) => {
                        let _ = this.update(cx, |view, _| {
                            view.usable = Some(size(px(width as f32), px(height as f32)));
                        });
                    }
                    Err(e) => log::warn!("the overlay cannot be dragged: {e:#}"),
                }
            })
            .detach();
        }
        Self {
            max_lines: config.max_lines.max(1),
            position: config.position,
            phase: Phase::Recording,
            started_at_ms: now_ms(),
            stopped_at_ms: None,
            committed: String::new(),
            tentative: String::new(),
            voice,
            clock: Clock::default(),
            style: config.style,
            motion: Motion::new(config.style, config.motion()),
            shape: Shape::still(config.style),
            still: false,
            opacity: config.opacity(),
            border: config.border(),
            frame_pending: false,
            look: None,
            tabular_numbers: FontFeatures(Arc::new(vec![("tnum".into(), 1)])),
            surface,
            x11,
            usable: None,
            window: None,
            origin: None,
            spot: None,
            drag: Drag::default(),
            moved_at: None,
            drag_window: None,
            left_behind: None,
            painted: Rc::default(),
            appearance: observe_appearance(window, cx),
        }
    }

    /// Recording (re)started. A different start time is a new recording and
    /// clears the previous one's text.
    pub fn recording(&mut self, started_at_ms: u64) {
        if started_at_ms != self.started_at_ms || !self.is_recording() {
            self.committed.clear();
            self.tentative.clear();
            self.stopped_at_ms = None;
            self.started_at_ms = started_at_ms;
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

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording)
    }

    /// Whether a new reading of the voice should be drawn right away.
    pub fn hears(&self) -> bool {
        self.is_recording() && !self.still
    }

    pub fn has_outcome(&self) -> bool {
        matches!(self.phase, Phase::Done(_))
    }

    /// Whether the pill was dragged and recording `started_at_ms` is a new
    /// dictation, which starts at home in a fresh window.
    pub fn moved_before(&self, started_at_ms: u64) -> bool {
        self.spot.is_some() && (started_at_ms != self.started_at_ms || !self.is_recording())
    }

    /// Every window the overlay has open.
    pub fn windows(&self) -> Vec<AnyWindowHandle> {
        self.window
            .iter()
            .chain(&self.drag_window)
            .chain(&self.left_behind)
            .copied()
            .collect()
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

    /// The area the pill moves on: the output's usable area for layer-shell
    /// (the frame of every overlay surface there), the X screen for the
    /// popup.
    fn area(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if self.x11.is_some() {
            window.display(cx).map(|display| display.bounds())
        } else {
            self.usable.map(|area| Bounds::new(Point::default(), area))
        }
    }

    /// Where the resting window sits on `area`.
    fn origin(&self, area: Bounds<Pixels>) -> Point<Pixels> {
        let window = window_size(self.max_lines);
        self.origin.unwrap_or_else(|| {
            if self.x11.is_some() {
                popup_origin(home_spot(area, self.position), window, self.position)
            } else {
                layer_home_origin(area.size, window, self.position)
            }
        })
    }

    /// Where the pill is now: its spot (at home, hugging the edge of the
    /// window at `origin`) kept on `area` at its current size.
    fn current_spot(&self, area: Bounds<Pixels>, origin: Point<Pixels>) -> Point<Pixels> {
        let pill = self
            .painted
            .get()
            .map_or(size(px(WIDTH), px(0.)), |(_, painted)| painted.size);
        let spot = self
            .spot
            .unwrap_or_else(|| hug(origin, window_size(self.max_lines), self.position));
        clamp_spot(spot, pill, area, self.position)
    }

    /// The pointer on `area`, given an event at `local` in the resting
    /// window (which keeps the pointer while the button is down).
    fn pointer(
        &self,
        local: Point<Pixels>,
        area: Bounds<Pixels>,
        window: &Window,
    ) -> Option<Point<Pixels>> {
        match &self.x11 {
            Some(popup) => popup.pointer(window.scale_factor()),
            None => Some(local + self.origin(area)),
        }
    }

    fn start_drag(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) = self.area(window, cx) else {
            return;
        };
        let Some(pointer) = self.pointer(event.position, area, window) else {
            return;
        };
        let from = self.current_spot(area, self.origin(area));
        let grab = Grab {
            offset: pointer - from,
            from,
        };
        if !self.drag.hold(grab) {
            return;
        }
        if self.x11.is_none() {
            self.open_drag_window(area.size, cx);
        }
        cx.notify();
    }

    fn open_drag_window(&mut self, area: Size<Pixels>, cx: &mut Context<Self>) {
        let options = drag_window_options(self.position, area);
        let overlay = cx.entity();
        // Opening renders the window, which reads this view: not while it
        // is being updated.
        cx.defer(move |cx| {
            if !overlay.read(cx).drag.is_held() {
                return;
            }
            let opened = cx.open_window(options, |window, cx| {
                window.set_input_region(Some(&[]));
                let overlay = overlay.clone();
                cx.new(|_| DragView { overlay })
            });
            match opened {
                Ok(handle) => overlay.update(cx, |view, _| view.drag_window = Some(handle.into())),
                Err(e) => log::warn!("opening the overlay's drag surface: {e:#}"),
            }
        });
    }

    /// A pointer event in the resting window while the pill is held, at
    /// `local` in it.
    fn on_pointer(
        &mut self,
        event: Pointer,
        local: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grab) = self.drag.grab() else {
            return;
        };
        if event.ends_drag() {
            if matches!(event, Pointer::Released(_)) {
                self.follow(local, grab, window, cx);
            }
            self.end_drag(cx);
            return;
        }
        // On software rendering each frame of the drag surface costs real
        // CPU: follow the pointer at a calm rate.
        if self.x11.is_none()
            && pill::software_rendered(window)
            && self
                .moved_at
                .is_some_and(|at| at.elapsed() < SOFTWARE_DRAG_FRAME)
        {
            return;
        }
        self.moved_at = Some(Instant::now());
        self.follow(local, grab, window, cx);
    }

    fn follow(
        &mut self,
        local: Point<Pixels>,
        grab: Grab,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.area(window, cx) else {
            return;
        };
        if let Some(pointer) = self.pointer(local, area, window) {
            self.spot = Some(pointer - grab.offset);
            cx.notify();
        }
    }

    /// Let go of the pill. On layer-shell, reopen the resting window where
    /// it was dropped (or let the old one show it again, when it did not
    /// move); the drag surface shows it until that window does.
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some((grab, landing)) = self.drag.let_go() else {
            return;
        };
        cx.notify();
        // The X11 popup already followed the pointer.
        let Some(area) = self
            .usable
            .filter(|_| self.x11.is_none())
            .map(|area| Bounds::new(Point::default(), area))
        else {
            self.land(landing, cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LANDING_TIMEOUT).await;
            let _ = this.update(cx, |view, cx| view.land(landing, cx));
        })
        .detach();
        let spot = self.current_spot(area, self.origin(area));
        if self.spot.is_none() || spot == grab.from {
            return;
        }
        self.spot = Some(spot);
        let window = window_size(self.max_lines);
        let origin = window_origin(spot, window, area, self.position);
        let previous_origin = self.origin.replace(origin);
        self.left_behind = self.window.take();
        let options = moved_window_options(self.position, origin, window, area.size);
        let overlay = cx.entity();
        cx.defer(move |cx| {
            let opened = cx.open_window(options, |window, cx| {
                window.set_input_region(Some(&[]));
                overlay.update(cx, |view, cx| {
                    view.appearance = observe_appearance(window, cx);
                });
                overlay.clone()
            });
            overlay.update(cx, |view, cx| match opened {
                Ok(handle) => view.window = Some(handle.into()),
                Err(e) => {
                    log::warn!("moving the overlay: {e:#}");
                    // Back to the old window, which never moved.
                    view.window = view.left_behind.take();
                    view.origin = previous_origin;
                    view.spot = Some(grab.from);
                    cx.notify();
                }
            });
        });
    }

    /// The pill's window shows it again after `landing`: close the drag
    /// surface and the window it was dropped out of.
    fn land(&mut self, landing: u32, cx: &mut Context<Self>) {
        if !self.drag.land(landing) {
            return;
        }
        for handle in self
            .drag_window
            .take()
            .into_iter()
            .chain(self.left_behind.take())
        {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// Move the X11 popup to the pill's spot once it was dragged.
    fn place_popup(&mut self, area: Bounds<Pixels>, window: &Window) {
        if self.x11.is_none() || self.spot.is_none() {
            return;
        }
        let origin = window_origin(
            self.current_spot(area, self.origin(area)),
            window_size(self.max_lines),
            area,
            self.position,
        );
        self.origin = Some(origin);
        if let Some(popup) = &mut self.x11 {
            popup.move_to(origin, window.scale_factor());
        }
    }

    /// Advance the clock, the pill's motion and the colours, and schedule
    /// the next frame.
    fn tick(&mut self, window: &Window, cx: &mut Context<Self>) {
        let dt = self.clock.tick(Instant::now());
        let voice = self.is_recording().then(|| self.voice.latest());
        let shape = self.motion.step(dt, voice.as_ref());
        self.still = pill::holds_still(window, cx);
        self.shape = if self.still {
            self.schedule_frame(STILL_FRAME, cx);
            Shape::still(self.style)
        } else {
            if shape.reach() > SPEAKING_REACH {
                window.request_animation_frame();
            } else {
                self.schedule_frame(QUIET_FRAME, cx);
            }
            shape
        };
        let accent = accent::get(cx);
        let theme = cx.theme();
        let target = Look::themed(
            theme,
            accent.unwrap_or(theme.primary),
            self.opacity,
            self.border,
        );
        self.look = Some(self.look.map_or(target, |look| look.fade_to(target, dt)));
    }

    /// The pill: outline, header and text. Only the resting window's pill
    /// takes the pointer and offers to discard the dictation.
    fn pill(&self, theme: &Theme, role: Role) -> Div {
        let rests = role == Role::Rest;
        let shape = if rests {
            self.shape
        } else {
            // Shadow and body quads, whatever the style: no paths on an
            // output-sized surface.
            Shape::still(OverlayStyle::Bars)
        };
        let look = self.look;
        let painted = self.painted.clone();
        div()
            .group(PILL)
            .w(px(WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| {
                        if let Some(look) = look {
                            pill::paint(bounds, &shape, look, 1., window);
                        }
                        let id = window.window_handle().window_id();
                        let last = painted.get();
                        if rests && last != Some((id, bounds)) {
                            // Only the pill takes the pointer; everywhere
                            // else clicks reach whatever is below.
                            window.set_input_region(Some(&[bounds]));
                            if last.is_some_and(|(_, last)| last.size != bounds.size) {
                                // Clamp the new size on screen right away.
                                window.refresh();
                            }
                            painted.set(Some((id, bounds)));
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(self.header(theme))
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
                    .child(self.body(theme)),
            )
            .when(rests && !self.has_outcome(), |this| {
                this.child(discard_button(theme))
            })
    }

    /// A window at `origin` on `area` with the pill at `spot` in it. A
    /// spacer between the pill and the edge it hugs gives way when growing
    /// text would push it past the far edge.
    fn layout(
        &self,
        pill: Div,
        spot: Point<Pixels>,
        origin: Point<Pixels>,
        window: Size<Pixels>,
        area: Bounds<Pixels>,
    ) -> Div {
        let placement = placement(spot, origin, window, area, self.position);
        div()
            .size_full()
            .flex()
            .map(|this| match self.position {
                OverlayPosition::Bottom => this.flex_col(),
                OverlayPosition::Top => this.flex_col_reverse(),
            })
            .items_start()
            .child(div().flex_grow_1().flex_basis(px(0.)).min_h(placement.far))
            .child(pill.ml(placement.left))
            .child(div().h(placement.near))
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
        if self.left_behind == Some(window.window_handle()) {
            // The pill was dropped elsewhere; closing once it shows there.
            return div();
        }
        if let Some(landing) = self.drag.landing() {
            // The pill is back in this window from this frame on.
            after_present(window, cx.entity().downgrade(), move |view, cx| {
                view.land(landing, cx);
            });
        }
        self.tick(window, cx);
        let viewport = window.viewport_size();
        let (area, origin) = match self.area(window, cx) {
            Some(area) => {
                self.place_popup(area, window);
                (area, self.origin(area))
            }
            // Not known yet: the pill stays home in its window.
            None => (Bounds::new(Point::default(), viewport), Point::default()),
        };
        let spot = self.current_spot(area, origin);
        let follow = self.drag.is_held().then(|| {
            let view = cx.entity().downgrade();
            let exits = self.x11.is_none();
            canvas(
                |_, _, _| {},
                move |_, (), window, _| follow_drag(view, exits, window),
            )
            .absolute()
            .size_full()
        });
        if self.drag.is_covered() {
            return div().size_full().children(follow);
        }
        let pill = self
            .pill(cx.theme(), Role::Rest)
            .when(self.movable(), |this| {
                this.cursor(CursorStyle::OpenHand)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::start_drag))
            });
        self.layout(pill, spot, origin, viewport, area)
            .children(follow)
    }
}

/// The × in the pill's top-right corner, shown while the pointer is on the
/// pill: throws the dictation away.
fn discard_button(theme: &Theme) -> impl IntoElement {
    // As far from the top and right edges as the header row's centre.
    let inset = px(PADDING_Y + (HEADER_HEIGHT - DISCARD_SIZE) / 2.);
    let (highlight, bright) = (theme.foreground.opacity(0.12), theme.foreground);
    div()
        .id("discard")
        .absolute()
        .top(inset)
        .right(inset)
        .size(px(DISCARD_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(theme.muted_foreground)
        .cursor(CursorStyle::PointingHand)
        .invisible()
        .group_hover(PILL, gpui_kit::StyleRefinement::visible)
        .hover(move |style| style.bg(highlight).text_color(bright))
        .child(Icon::new(IconName::Close).size(px(14.)))
        // A press here is no drag.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| cx.defer(crate::ui::discard))
}

/// While a drag lasts, follow the pointer anywhere (the resting window keeps
/// it while the button is down) and end on release, on a press of another
/// button, and, with `exits`, when the window loses the pointer. Registered
/// each frame.
fn follow_drag(view: WeakEntity<OverlayView>, exits: bool, window: &mut Window) {
    window.set_window_cursor_style(CursorStyle::ClosedHand);
    let on = |view: &WeakEntity<OverlayView>,
              event: Pointer,
              local: Point<Pixels>,
              window: &mut Window,
              cx: &mut App| {
        let _ = view.update(cx, |view, cx| view.on_pointer(event, local, window, cx));
    };
    let moved = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            let pressed = event.pressed_button;
            on(
                &moved,
                Pointer::Moved { pressed },
                event.position,
                window,
                cx,
            );
        }
    });
    let released = view.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            let pointer = Pointer::Released(event.button);
            on(&released, pointer, event.position, window, cx);
        }
    });
    let pressed = view.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            let pointer = Pointer::Pressed(event.button);
            on(&pressed, pointer, event.position, window, cx);
        }
    });
    if exits {
        window.on_mouse_event(move |event: &MouseExitEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture {
                on(&view, Pointer::Exited, event.position, window, cx);
            }
        });
    }
}

/// Run `f` on `view` once the frame `window` draws next has been presented:
/// its next-frame callbacks run before it draws, and a task spawned there
/// after that frame was handed to the compositor.
fn after_present<V: 'static>(
    window: &mut Window,
    view: WeakEntity<V>,
    f: impl FnOnce(&mut V, &mut Context<V>) + 'static,
) {
    window.on_next_frame(move |_, cx| {
        cx.spawn(async move |cx| {
            let _ = view.update(cx, f);
        })
        .detach();
    });
}

fn observe_appearance(window: &mut Window, cx: &mut Context<OverlayView>) -> Subscription {
    cx.observe_window_appearance(window, |_, window, cx| {
        Theme::sync_system_appearance(Some(window), cx);
    })
}

/// Root of the drag surface: the pill at its spot on the usable area, drawn
/// from the resting window's state (GPUI redraws it whenever that changes).
struct DragView {
    overlay: Entity<OverlayView>,
}

impl Render for DragView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let overlay = self.overlay.downgrade();
        self.overlay.update(cx, |view, cx| {
            if view.drag.is_held() && !view.drag.is_covered() {
                // Once this shows the pill, its window can leave it out.
                after_present(window, overlay, |view, cx| {
                    view.drag.cover();
                    cx.notify();
                });
            }
            let area = Bounds::new(Point::default(), view.usable.unwrap_or(viewport));
            let spot = view.current_spot(area, view.origin(area));
            let pill = view.pill(cx.theme(), Role::Drag);
            view.layout(pill, spot, Point::default(), area.size, area)
        })
    }
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

    /// Whether `pill` keeps `EDGE_GAP` inside `area`, give or take float
    /// rounding.
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
    fn elapsed_rolls_over_minutes() {
        assert_eq!(format_elapsed(Duration::from_millis(59_999)), "0:59");
        assert_eq!(format_elapsed(Duration::from_secs(61)), "1:01");
        assert_eq!(format_elapsed(Duration::from_secs(600)), "10:00");
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
            // ...and the layer-shell home window shows the pill there, to
            // the compositor's whole pixel.
            let area = Bounds::new(Point::default(), screen().size);
            let window = window_size(3);
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
    fn popup_is_centred_and_keeps_its_margin_from_the_anchored_edge() {
        let window = window_size(3);
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

    /// Lay a pill `height` tall out at `spot` in a window at `origin` the
    /// way the flex column does, in window coordinates.
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

    #[test]
    fn a_dropped_pill_fits_its_window_and_the_screen_as_the_text_grows() {
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
                    let (edge, left) =
                        match moved_window_options(position, origin, window, area.size).kind {
                            WindowKind::LayerShell(options) => {
                                let margin = options.margin.unwrap();
                                (margin.0.max(margin.2), margin.3)
                            }
                            _ => unreachable!(),
                        };
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

    #[test]
    fn the_drag_surface_shows_the_pill_where_it_is_on_the_area() {
        let area = Bounds::new(Point::default(), screen().size);
        for position in POSITIONS {
            let home = home_spot(area, position);
            for delta in drags() {
                for height in [80., 124.] {
                    let spot =
                        clamp_spot(home + delta, size(px(WIDTH), px(height)), area, position);
                    let at = laid_out(spot, height, Point::default(), area.size, area, position);
                    let want = pill_at(spot, height, position);
                    let off =
                        (at.origin.x - want.origin.x).abs() + (at.origin.y - want.origin.y).abs();
                    assert!(off < px(1e-3), "{position:?} {height}: {at:?} vs {want:?}");
                }
            }
        }
    }
}
