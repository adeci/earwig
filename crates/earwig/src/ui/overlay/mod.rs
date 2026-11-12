//! The small on-screen pill shown while dictating. The user can drag it out
//! of the way for the rest of the dictation; the next one starts at home.
//! Its × throws the dictation away. Once the dictation is finished the pill
//! switches off where it is (see [`crt`](crate::ui::crt)) and its window
//! waits, empty and click-through, until the text has landed (see
//! [`lifecycle`](crate::ui::lifecycle)).
//!
//! The pill's window is just big enough for it: GPUI's path rendering costs
//! in proportion to the window's area, which makes an output-sized overlay
//! expensive (very much so on software rendering). A drag moves that window
//! with the pointer, the pill animated as ever: a layer surface through its
//! margins, a popup to its new place.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use earwig_core::config::{OverlayAnimation, OverlayConfig, OverlayPosition};
use earwig_core::spectrum::Feed;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, CursorStyle, DispatchPhase, Div,
    Entity, FontFeatures, FontWeight, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, PlatformDisplay, Point, Render, Size, Stateful, StatefulInteractiveElement as _,
    Styled as _, StyledText, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowId, WindowKind, WindowOptions, canvas, div, point, px, size,
};

use crate::ui::drag::{Drag, Grab, Pointer};
use crate::ui::lifecycle::{Lifecycle, Stage};
use crate::ui::pill::{self, Clock, Edge, Look, Motion};
use crate::ui::platform::Popup;
use crate::ui::{OverlaySurface, accent, crt, now_ms};

#[cfg(target_os = "linux")]
mod layer_shell;

/// Width of the pill.
pub(crate) const WIDTH: f32 = 560.;
/// Gap between the pill and the edge of the screen (less bars) it starts
/// at; at least `INSET`, so the window never reaches past that edge.
const MARGIN: f32 = 56.;
/// Transparent room around the pill for its moving edge and its shadow.
const INSET: f32 = pill::ROOM;
/// Gap between the window and the edge it starts at.
const WINDOW_MARGIN: f32 = MARGIN - INSET;
const _: () = assert!(WINDOW_MARGIN >= 0.);
/// Closest a dragged pill gets to an edge, so even its fullest swell stays
/// on screen.
const EDGE_GAP: f32 = pill::DEPTH;
/// Size and line height of the dictated text.
pub(crate) const TEXT_SIZE: f32 = 15.;
pub(crate) const LINE_HEIGHT: f32 = 22.;
/// Frame pacing: every display frame while the voice moves the pill, slow
/// otherwise, so the overlay never competes with CPU decoding at display
/// rate. A voice wakes a slow overlay at once (see [`OverlayView::hears`]).
const QUIET_FRAME: Duration = Duration::from_millis(50);
const STILL_FRAME: Duration = Duration::from_millis(250);
const DISCARD_SIZE: f32 = 24.;
/// Pixels the voice must move the pill by to be worth every frame.
const SPEAKING_REACH: f32 = 0.35;
const HEADER_HEIGHT: f32 = 22.;
/// The pill's padding, and the gap between its header and its text.
pub(crate) const PADDING_X: f32 = 18.;
pub(crate) const PADDING_Y: f32 = 18.;
pub(crate) const GAP: f32 = 8.;

/// Open the overlay's window at home.
pub fn open(
    config: &OverlayConfig,
    surface: OverlaySurface,
    voice: Feed,
    cx: &mut App,
) -> anyhow::Result<Entity<OverlayView>> {
    let display = crate::ui::platform::home_display(cx);
    let options = home_window_options(surface, config, display.as_deref());
    // No Base root: it paints an opaque background, and the overlay has no
    // inputs or popovers that need one.
    let mut view = None;
    let handle = cx.open_window(options, |window, cx| {
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
/// choice is made: a layer surface (see `layer_shell.rs`), or else a
/// borderless popup centred on the display the platform picks.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        unused_variables,
        reason = "only Linux has a surface besides the popup"
    )
)]
fn home_window_options(
    surface: OverlaySurface,
    config: &OverlayConfig,
    display: Option<&dyn PlatformDisplay>,
) -> WindowOptions {
    let window_size = window_size(config.max_lines);
    #[cfg(target_os = "linux")]
    if surface == OverlaySurface::LayerShell {
        return layer_shell::home_window_options(config.position, window_size);
    }
    let origin = display.map_or(point(px(0.), px(0.)), |display| {
        popup_origin(
            home_spot(display.visible_bounds(), config.position),
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

pub struct OverlayView {
    max_lines: u32,
    position: OverlayPosition,
    life: Lifecycle,
    started_at_ms: u64,
    stopped_at_ms: Option<u64>,
    committed: String,
    tentative: String,
    /// The voice of the current recording.
    voice: Feed,
    clock: Clock,
    motion: Motion,
    /// What the voice did to the pill's edge as of the last tick.
    edge: Edge,
    /// The pill holds still (reduced motion, software rendering).
    still: bool,
    /// How the pill comes and goes.
    animation: OverlayAnimation,
    /// Where the power-on of a new dictation's pill stands.
    power_on: PowerOn,
    /// Fill opacity and border width settings.
    opacity: f32,
    border: f32,
    frame_pending: bool,
    /// The pill's colours as they fade toward the theme's and the accent's.
    look: Option<Look>,
    /// The pill sits on a layer surface, which cannot move once mapped.
    layer_shell: bool,
    /// The popup, when it can be moved.
    popup: Option<Popup>,
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
    painted: Painted,
}

/// The pill's bounds as last painted in a resting window, and which one:
/// each new window needs its input region set.
type Painted = Rc<Cell<Option<(WindowId, Bounds<Pixels>)>>>;

/// A new dictation's pill powers on from its first frame (see
/// [`crt::opening`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PowerOn {
    /// On the next frame.
    Due,
    Since(Instant),
    /// Done, or not animated.
    Done,
}

impl OverlayView {
    #[cfg_attr(
        not(target_os = "linux"),
        expect(unused_variables, reason = "only layer-shell probes its area")
    )]
    pub fn new(
        config: &OverlayConfig,
        surface: OverlaySurface,
        voice: Feed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let popup = if surface == OverlaySurface::Popup {
            Popup::new(window)
                .inspect_err(|e| log::warn!("the overlay stays where it is: {e:#}"))
                .ok()
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let layer_shell = surface == OverlaySurface::LayerShell;
        #[cfg(not(target_os = "linux"))]
        let layer_shell = false;
        #[cfg(target_os = "linux")]
        if layer_shell {
            cx.spawn(async move |this, cx| {
                let area = cx
                    .background_executor()
                    .spawn(async { crate::ui::platform::usable_area() })
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
            life: Lifecycle::default(),
            started_at_ms: now_ms(),
            stopped_at_ms: None,
            committed: String::new(),
            tentative: String::new(),
            voice,
            clock: Clock::default(),
            motion: Motion::default(),
            edge: Edge::default(),
            still: false,
            animation: config.animation,
            power_on: PowerOn::Due,
            opacity: config.opacity(),
            border: config.border(),
            frame_pending: false,
            look: None,
            layer_shell,
            popup,
            usable: None,
            window: None,
            origin: None,
            spot: None,
            drag: Drag::default(),
            painted: Rc::default(),
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
            self.power_on = PowerOn::Due;
        }
        self.life.recording();
    }

    /// The recording was finished: switch the pill off at once, where it
    /// is (a pill held mid-drag is let go there). The final text and its
    /// insertion carry on unseen.
    pub fn finish(&mut self, now: Instant, cx: &mut Context<Self>) {
        self.stopped_at_ms.get_or_insert_with(now_ms);
        self.end_drag(cx);
        self.life.finish(now, self.switch_off());
    }

    /// The final text could not be inserted: say so.
    pub fn failed(&mut self, on_clipboard: bool, now: Instant) {
        self.life.failed(on_clipboard, now);
    }

    /// The daemon is done with the dictation.
    pub fn idle(&mut self) {
        self.life.idle();
    }

    /// Catch up with the clock (see [`OverlayView::deadline`]).
    pub fn advance(&mut self, now: Instant) {
        self.life.advance(now, self.switch_off());
    }

    /// When the pill next moves on by itself: a switch-off or a failure
    /// notice ending.
    pub fn deadline(&self) -> Option<Instant> {
        self.life.deadline()
    }

    /// Whether the overlay is done and its windows should close.
    pub fn is_closed(&self) -> bool {
        self.life.stage() == Stage::Closed
    }

    /// How long switching off takes: none when the pill holds still, which
    /// spares a CPU-drawn window the frames, or when the setting says so.
    fn switch_off(&self) -> Duration {
        if self.still || self.animation == OverlayAnimation::Instant {
            Duration::ZERO
        } else {
            crt::DURATION
        }
    }

    /// Seconds into powering on, while the pill does.
    fn powering_on(&self, now: Instant) -> Option<f32> {
        match self.power_on {
            PowerOn::Since(since) => Some(now.duration_since(since).as_secs_f32())
                .filter(|&t| t < crt::OPEN.as_secs_f32()),
            PowerOn::Due | PowerOn::Done => None,
        }
    }

    /// Seconds into switching off, while the pill does.
    fn switching_off(&self, now: Instant) -> Option<f32> {
        match self.life.stage() {
            Stage::SwitchingOff { since, .. } => Some(now.duration_since(since).as_secs_f32()),
            _ => None,
        }
    }

    pub fn set_text(&mut self, committed: String, tentative: String) {
        // A finished pill keeps the text it had, so it switches off as it was.
        if self.is_recording() {
            self.committed = committed;
            self.tentative = tentative;
        }
    }

    pub fn is_recording(&self) -> bool {
        self.life.stage() == Stage::Live
    }

    /// Whether a new reading of the voice should be drawn right away.
    pub fn hears(&self) -> bool {
        self.is_recording() && !self.still
    }

    /// Whether the pill takes the pointer: while recording, or saying the
    /// insertion failed; not while it switches off.
    fn takes_pointer(&self) -> bool {
        matches!(self.life.stage(), Stage::Live | Stage::Failed { .. })
    }

    /// The failure the pill says, or switches off saying: whether the text
    /// was left on the clipboard.
    fn failure(&self) -> Option<bool> {
        match self.life.stage() {
            Stage::Failed { on_clipboard, .. }
            | Stage::SwitchingOff {
                failure: Some(on_clipboard),
                ..
            } => Some(on_clipboard),
            _ => None,
        }
    }

    /// Whether the pill was dragged and recording `started_at_ms` is a new
    /// dictation, which starts at home in a fresh window.
    pub fn moved_before(&self, started_at_ms: u64) -> bool {
        self.spot.is_some() && (started_at_ms != self.started_at_ms || !self.is_recording())
    }

    /// Every window the overlay has open.
    pub fn windows(&self) -> Vec<AnyWindowHandle> {
        self.window.iter().copied().collect()
    }

    fn elapsed(&self) -> Duration {
        let end = self.stopped_at_ms.unwrap_or_else(now_ms);
        Duration::from_millis(end.saturating_sub(self.started_at_ms))
    }

    fn header(&self, theme: &Theme) -> Div {
        let status = self.failure().map(|_| "Not inserted");
        let words = word_count(&self.committed) + word_count(&self.tentative);
        header_row(theme, self.elapsed(), words, status)
    }

    fn body(&self, theme: &Theme) -> Div {
        match self.failure() {
            Some(on_clipboard) => div().text_color(theme.foreground).child(if on_clipboard {
                "Couldn't type into the app. The text is on the clipboard."
            } else {
                "Couldn't insert the text. It is saved in History."
            }),
            None if self.committed.is_empty() && self.tentative.is_empty() => {
                div().text_color(theme.muted_foreground).child("Listening…")
            }
            None => div().text_color(theme.foreground).child(live_text(
                &self.committed,
                &self.tentative,
                theme.muted_foreground,
            )),
        }
    }

    fn movable(&self) -> bool {
        self.layer_shell || self.popup.is_some()
    }

    /// The area the pill moves on: the output's usable area for layer-shell
    /// (the frame its window's margins are in), the screen less its panels
    /// for the popup.
    fn area(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if self.popup.is_some() {
            window.display(cx).map(|display| display.visible_bounds())
        } else {
            self.usable.map(|area| Bounds::new(Point::default(), area))
        }
    }

    /// Where the resting window sits on `area`.
    fn origin(&self, area: Bounds<Pixels>) -> Point<Pixels> {
        let window = window_size(self.max_lines);
        self.origin.unwrap_or_else(|| {
            if self.popup.is_some() {
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

    /// The pointer on `area`, given an event at `local` in the pill's window
    /// (which keeps the pointer while the button is down).
    fn pointer(
        &self,
        local: Point<Pixels>,
        area: Bounds<Pixels>,
        window: &Window,
    ) -> Option<Point<Pixels>> {
        let origin = self.origin(area);
        match (&self.popup, self.drag.grab()) {
            (Some(popup), _) => popup.pointer(window.scale_factor()),
            (None, Some(grab)) => Some(grab.pointer(
                self.pointer_travel(),
                local,
                origin,
                self.follows_freely(area),
            )),
            (None, None) => Some(local + origin),
        }
    }

    /// Whether the pill's window sits where the pill was dragged, not
    /// against an edge of `area`.
    fn follows_freely(&self, area: Bounds<Pixels>) -> bool {
        let Some(spot) = self.spot else {
            return true;
        };
        let size = window_size(self.max_lines);
        let held = self.current_spot(area, self.origin(area));
        held == spot
            && window_origin(spot, size, area, self.position)
                == popup_origin(spot, size, self.position).map(|v| v.round())
    }

    /// How far the pointer has travelled (see [`Grab::pointer`]), on
    /// layer-shell.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(clippy::unused_self, reason = "only layer-shell reports it")
    )]
    fn pointer_travel(&self) -> Option<Point<Pixels>> {
        #[cfg(target_os = "linux")]
        if self.layer_shell {
            return gpui_linux::pointer_travel();
        }
        None
    }

    fn start_drag(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) = self.area(window, cx) else {
            return;
        };
        let Some(pointer) = self.pointer(event.position, area, window) else {
            return;
        };
        let origin = self.origin(area);
        let from = self.current_spot(area, origin);
        if self.drag.hold(Grab {
            offset: pointer - from,
            at: pointer,
            local: event.position,
            origin,
            travel: self.pointer_travel(),
        }) {
            cx.notify();
        }
    }

    /// A pointer event in the pill's window while the pill is held, at
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
            self.end_drag(cx);
            return;
        }
        let Some(area) = self.area(window, cx) else {
            return;
        };
        if let Some(pointer) = self.pointer(local, area, window) {
            self.spot = Some(pointer - grab.offset);
            #[cfg(target_os = "linux")]
            self.move_layer(area, window);
            cx.notify();
        }
    }

    /// How the pointer may take the pill, for the hand cursors.
    #[cfg(windows)]
    fn grip(&self) -> crate::ui::platform::Grip {
        use crate::ui::platform::Grip;
        if self.drag.is_held() {
            return Grip::Held;
        }
        match self.painted.get() {
            Some((_, pill)) if self.movable() && self.takes_pointer() => Grip::Open(pill),
            _ => Grip::None,
        }
    }

    /// Let go of the pill where it is.
    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.let_go() {
            cx.notify();
        }
    }

    /// Move the layer surface to the pill's spot, at once rather than with
    /// the next frame: the compositor sends the pointer relative to it.
    #[cfg(target_os = "linux")]
    fn move_layer(&mut self, area: Bounds<Pixels>, window: &Window) {
        if !self.layer_shell {
            return;
        }
        let size = window_size(self.max_lines);
        let from = self.origin(area);
        let origin = window_origin(self.current_spot(area, from), size, area, self.position);
        if origin == from {
            return;
        }
        let shift = crate::ui::drag::reads_current();
        match layer_shell::move_window(
            window,
            self.position,
            (from, origin),
            size,
            area.size,
            shift,
        ) {
            Ok(()) => self.origin = Some(origin),
            Err(e) => {
                log::warn!("the overlay cannot be dragged: {e:#}");
                self.layer_shell = false;
                self.drag.let_go();
            }
        }
    }

    /// Move the popup to the pill's spot once it was dragged.
    fn place_popup(&mut self, area: Bounds<Pixels>, window: &Window) {
        if self.popup.is_none() || self.spot.is_none() {
            return;
        }
        let origin = window_origin(
            self.current_spot(area, self.origin(area)),
            window_size(self.max_lines),
            area,
            self.position,
        );
        self.origin = Some(origin);
        if let Some(popup) = &mut self.popup {
            popup.move_to(origin, window.scale_factor());
        }
    }

    /// Advance the clock, the pill's motion and the colours, and schedule
    /// the next frame.
    fn tick(&mut self, window: &Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let dt = self.clock.tick(now);
        let voice = self.is_recording().then(|| self.voice.latest());
        let edge = self.motion.step(dt, voice.as_ref());
        self.still = pill::holds_still(window, cx);
        if self.power_on == PowerOn::Due {
            // From this, its first frame: nothing waits for it.
            self.power_on = if self.switch_off() > Duration::ZERO {
                PowerOn::Since(now)
            } else {
                PowerOn::Done
            };
        }
        self.edge = if self.still {
            self.schedule_frame(STILL_FRAME, cx);
            Edge::default()
        } else {
            if edge.reach() > SPEAKING_REACH
                || self.switching_off(now).is_some()
                || self.powering_on(now).is_some()
            {
                window.request_animation_frame();
            } else {
                self.schedule_frame(QUIET_FRAME, cx);
            }
            edge
        };
        let target = Look::themed(cx.theme(), accent::get(cx), self.opacity, self.border);
        self.look = Some(self.look.map_or(target, |look| look.fade_to(target, dt)));
    }

    /// The pill: outline, header and text, or as far switched off as it
    /// is.
    fn pill(&self, theme: &Theme) -> Div {
        let edge = self.edge;
        let look = self.look;
        let painted = self.painted.clone();
        let now = Instant::now();
        let off = self.switching_off(now);
        let on = self.powering_on(now).filter(|_| off.is_none());
        let content = match (off, on) {
            (Some(t), _) => crt::content(t),
            (None, Some(t)) => crt::opening_content(t),
            (None, None) => 1.,
        };
        let takes_pointer = self.takes_pointer();
        div()
            .w(px(WIDTH))
            .flex_shrink_0()
            // No Base root sets the theme's font here (see `open`).
            .font_family(theme.font_family.clone())
            .flex()
            .flex_col()
            .gap(px(GAP))
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| {
                        match (look, off) {
                            (Some(look), None) => {
                                let size = bounds.size.map(f32::from);
                                match on.and_then(|t| crt::opening(t, size.width, size.height)) {
                                    Some(frame) => {
                                        pill::paint_crt(bounds, &frame, &edge, look, window);
                                    }
                                    None => pill::paint(bounds, &edge, look, 1., window),
                                }
                            }
                            (Some(look), Some(t)) => {
                                let size = bounds.size.map(f32::from);
                                if let Some(frame) = crt::at(t, size.width, size.height) {
                                    pill::paint_crt(bounds, &frame, &edge, look, window);
                                }
                            }
                            (None, _) => {}
                        }
                        let id = window.window_handle().window_id();
                        let last = painted.get();
                        if takes_pointer && last != Some((id, bounds)) {
                            // Only the pill takes the pointer; everywhere
                            // else clicks reach whatever is below.
                            set_pointer_region(window, Some(bounds));
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
            .child(self.header(theme).opacity(content))
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
                    .text_size(px(TEXT_SIZE))
                    .line_height(px(LINE_HEIGHT))
                    .opacity(content)
                    .child(self.body(theme)),
            )
            .when(self.failure().is_none(), |this| {
                this.child(discard_button(theme, self.is_recording()).opacity(content))
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
        if self.life.stage() == Stage::Off {
            // Gone from the screen: clicks go through until the pill comes
            // back to say the insertion failed, or the window closes.
            if self.painted.take().is_some() {
                set_pointer_region(window, None);
            }
            return div();
        }
        self.tick(window, cx);
        // The pointer's relative motion can come after its position: catch
        // up with it before drawing.
        #[cfg(target_os = "linux")]
        if let (Some(grab), Some(travel), Some(area)) = (
            self.drag.grab(),
            self.pointer_travel(),
            self.area(window, cx),
        ) && let Some(then) = grab.travel.filter(|&then| then != travel)
        {
            self.spot = Some(grab.at + (travel - then) - grab.offset);
            self.move_layer(area, window);
        }
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
            let exits = self.popup.is_none();
            canvas(
                |_, _, _| {},
                move |_, (), window, _| follow_drag(view, exits, window),
            )
            .absolute()
            .size_full()
        });
        #[cfg(windows)]
        crate::ui::platform::set_grip(window, self.grip());
        let pill = self
            .pill(cx.theme())
            .when(self.movable() && self.takes_pointer(), |this| {
                this.cursor(CursorStyle::OpenHand)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::start_drag))
            });
        self.layout(pill, spot, origin, viewport, area)
            .children(follow)
    }
}

/// Clicks reach the window only on the pill at `pill` (nowhere for `None`);
/// everywhere else they go to whatever is below.
fn set_pointer_region(window: &Window, pill: Option<Bounds<Pixels>>) {
    window.set_input_region(Some(pill.as_slice()));
    // With room for the shadow, the water and the glow, which the region
    // clips on Windows.
    #[cfg(windows)]
    crate::ui::platform::set_input_region(window, pill.map(|pill| pill.dilate(px(INSET))));
    #[cfg(target_os = "macos")]
    crate::ui::platform::set_input_region(window, pill);
}

/// The pill's header: the recording dot, the time recorded, the word count
/// and `status`, if any.
pub(crate) fn header_row(
    theme: &Theme,
    elapsed: Duration,
    words: usize,
    status: Option<&'static str>,
) -> Div {
    /// Figures of one width, so the time holds still as it counts.
    static TABULAR: LazyLock<FontFeatures> =
        LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
    let dot = theme.danger;
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
                .font_features(TABULAR.clone())
                .child(format_elapsed(elapsed)),
        )
        .child(format!(
            "{words} {}",
            if words == 1 { "word" } else { "words" }
        ))
        .when_some(status, |this, status| {
            this.child(div().text_color(dot).child(status))
        })
}

/// The × in the pill's top-right corner: throws the dictation away while
/// `live`; switching off, it only fades with the text.
fn discard_button(theme: &Theme, live: bool) -> Stateful<Div> {
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
        .child(Icon::new(IconName::Close).size(px(14.)))
        .when(live, |this| {
            this.cursor(CursorStyle::PointingHand)
                .hover(move |style| style.bg(highlight).text_color(bright))
                // A press here is no drag.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(|_, _, cx| cx.defer(crate::ui::discard))
        })
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
