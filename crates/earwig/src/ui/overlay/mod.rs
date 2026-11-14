//! The dictation pill, in a window just its size (GPUI's path rendering costs per window area).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use earwig_core::config::{OverlayAnimation, OverlayConfig, OverlayPosition};
use earwig_core::spectrum::Feed;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, CursorStyle, DispatchPhase, Div,
    Entity, FontWeight, HighlightStyle, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels,
    PlatformDisplay, Point, Render, Size, Stateful, StatefulInteractiveElement as _, Styled as _,
    StyledText, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowId, WindowKind, WindowOptions, canvas, div, point, px, size,
};

use crate::ui::drag::{Grab, Pointer};
use crate::ui::lifecycle::{Lifecycle, Stage};
use crate::ui::pill::{self, Clock, Edge, Look, Motion};
use crate::ui::platform::Popup;
use crate::ui::{OverlaySurface, accent, crt, font, now_ms};

#[cfg(target_os = "linux")]
mod layer_shell;

pub(crate) const WIDTH: f32 = 560.;
/// From the pill to the screen edge it starts at.
const MARGIN: f32 = 56.;
/// Room around the pill for its moving edge and shadow.
const INSET: f32 = pill::ROOM;
const WINDOW_MARGIN: f32 = MARGIN - INSET;
const _: () = assert!(WINDOW_MARGIN >= 0.);
/// Closest a dragged pill gets to an edge, so its fullest swell stays on screen.
const EDGE_GAP: f32 = pill::DEPTH;
pub(crate) const TEXT_SIZE: f32 = 15.;
pub(crate) const LINE_HEIGHT: f32 = 22.;
/// Pacing while the voice isn't moving the pill, so we don't compete with CPU decoding.
const QUIET_FRAME: Duration = Duration::from_millis(50);
const STILL_FRAME: Duration = Duration::from_millis(250);
const DISCARD_SIZE: f32 = 24.;
/// Pixels the voice must move the edge by to be worth every frame.
const SPEAKING_REACH: f32 = 0.35;
const HEADER_HEIGHT: f32 = 22.;
pub(crate) const PADDING_X: f32 = 18.;
pub(crate) const PADDING_Y: f32 = 18.;
/// Between the header and the text.
pub(crate) const GAP: f32 = 8.;

pub fn open(
    config: &OverlayConfig,
    surface: OverlaySurface,
    voice: Feed,
    cx: &mut App,
) -> anyhow::Result<Entity<OverlayView>> {
    let display = crate::ui::platform::home_display(cx);
    let options = home_window_options(surface, config, display.as_deref());
    // No gpui-kit Root: it paints an opaque background, and we have no inputs or popovers.
    let mut view = None;
    let handle = cx.open_window(options, |window, cx| {
        // Click-through until the pill is painted.
        window.set_input_region(Some(&[]));
        let root = cx.new(|cx| OverlayView::new(config, surface, voice, window, cx));
        view = Some(root.clone());
        root
    })?;
    let view = view.expect("the window was built");
    view.update(cx, |view, _| view.window = Some(handle.into()));
    Ok(view)
}

/// A layer surface, or a borderless popup centred on the platform's display.
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
        // GPUI caps unfocused windows at 30 fps, delaying a voice's first frame; we pace ourselves.
        inactive_frame_interval: None,
        ..WindowOptions::default()
    }
}

/// Room for the header, `max_lines` lines and the inset, in whole pixels; the pill shrinks to fit.
#[allow(clippy::cast_precision_loss)]
fn window_size(max_lines: u32) -> Size<Pixels> {
    let height =
        2. * INSET + PADDING_Y * 2. + HEADER_HEIGHT + GAP + LINE_HEIGHT * max_lines.max(1) as f32;
    size(px((WIDTH + 2. * INSET).floor()), px(height.floor()))
}

/// Home: centred, `MARGIN` off its edge. A spot is the pill's left x and its hugged edge's y.
fn home_spot(area: Bounds<Pixels>, position: OverlayPosition) -> Point<Pixels> {
    let x = area.left() + (area.size.width - px(WIDTH)) / 2.;
    let y = match position {
        OverlayPosition::Bottom => area.bottom() - px(MARGIN),
        OverlayPosition::Top => area.top() + px(MARGIN),
    };
    point(x, y)
}

/// Where layer-shell puts the home window: centred in whole pixels, margin truncated like GPUI.
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

/// The spot of a pill hugging its edge of the window at `origin`.
fn hug(origin: Point<Pixels>, window: Size<Pixels>, position: OverlayPosition) -> Point<Pixels> {
    let y = match position {
        OverlayPosition::Bottom => origin.y + window.height - px(INSET),
        OverlayPosition::Top => origin.y + px(INSET),
    };
    point(origin.x + px(INSET), y)
}

/// Moves `spot` just enough to keep the pill `EDGE_GAP` inside `area` (top-left first if it can't).
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

/// Top-left of the window showing the pill at `spot`.
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

/// [`popup_origin`] kept on `area` in whole pixels; near an edge the pill sits off-centre.
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

/// Where the pill sits in its window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    left: Pixels,
    /// From the window's hugged edge; gives way as the pill grows into `far`.
    near: Pixels,
    /// Kept clear at the other end so the pill stays `EDGE_GAP` off the area's far edge.
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
    voice: Feed,
    clock: Clock,
    motion: Motion,
    edge: Edge,
    /// Reduced motion or software rendering.
    still: bool,
    animation: OverlayAnimation,
    power_on: PowerOn,
    opacity: f32,
    border: f32,
    frame_pending: bool,
    /// Fades toward the theme's and the accent's colours.
    look: Option<Look>,
    layer_shell: bool,
    popup: Option<Popup>,
    /// The output's usable area (layer-shell), once probed.
    usable: Option<Size<Pixels>>,
    window: Option<AnyWindowHandle>,
    /// Where the window sits on the area; `None` where it opened.
    origin: Option<Point<Pixels>>,
    /// Where the pill was dragged this dictation; `None` is home.
    spot: Option<Point<Pixels>>,
    drag: Option<Grab>,
    painted: Painted,
}

/// The pill's last painted bounds and window, to set each new window's input region.
type Painted = Rc<Cell<Option<(WindowId, Bounds<Pixels>)>>>;

/// The CRT power-on, from the pill's first frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PowerOn {
    /// Starts on the next frame.
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
            drag: None,
            painted: Rc::default(),
        }
    }

    /// A new start time is a new recording, which clears the text.
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

    /// Switch the pill off right where it is, letting go of any drag.
    pub fn finish(&mut self, now: Instant, cx: &mut Context<Self>) {
        self.stopped_at_ms.get_or_insert_with(now_ms);
        self.end_drag(cx);
        self.life.finish(now, self.switch_off());
    }

    pub fn failed(&mut self, on_clipboard: bool, now: Instant) {
        self.life.failed(on_clipboard, now);
    }

    pub fn idle(&mut self) {
        self.life.idle();
    }

    pub fn advance(&mut self, now: Instant) {
        self.life.advance(now, self.switch_off());
    }

    /// When the pill next changes by itself.
    pub fn deadline(&self) -> Option<Instant> {
        self.life.deadline()
    }

    pub fn is_closed(&self) -> bool {
        self.life.stage() == Stage::Closed
    }

    /// Instant when the pill holds still (spares a CPU-drawn window) or the setting says so.
    fn switch_off(&self) -> Duration {
        if self.still || self.animation == OverlayAnimation::Instant {
            Duration::ZERO
        } else {
            crt::DURATION
        }
    }

    /// Seconds into the power-on, while it lasts.
    fn powering_on(&self, now: Instant) -> Option<f32> {
        match self.power_on {
            PowerOn::Since(since) => Some(now.duration_since(since).as_secs_f32())
                .filter(|&t| t < crt::OPEN.as_secs_f32()),
            PowerOn::Due | PowerOn::Done => None,
        }
    }

    /// Seconds into the switch-off, while it lasts.
    fn switching_off(&self, now: Instant) -> Option<f32> {
        match self.life.stage() {
            Stage::SwitchingOff { since, .. } => Some(now.duration_since(since).as_secs_f32()),
            _ => None,
        }
    }

    pub fn set_text(&mut self, committed: String, tentative: String) {
        // A finished pill keeps its text while it switches off.
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

    /// Live or showing a failure; never while switching off.
    fn takes_pointer(&self) -> bool {
        matches!(self.life.stage(), Stage::Live | Stage::Failed { .. })
    }

    /// The failure on show (or switching off): whether the text is on the clipboard.
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

    /// Dragged, and `started_at_ms` is a new dictation, which starts at home in a fresh window.
    pub fn moved_before(&self, started_at_ms: u64) -> bool {
        self.spot.is_some() && (started_at_ms != self.started_at_ms || !self.is_recording())
    }

    pub fn window(&self) -> Option<AnyWindowHandle> {
        self.window
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

    /// What the pill moves on: the output's usable area (layer-shell) or the screen less panels.
    fn area(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if self.popup.is_some() {
            window.display(cx).map(|display| display.visible_bounds())
        } else {
            self.usable.map(|area| Bounds::new(Point::default(), area))
        }
    }

    /// Where the window sits on `area`.
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

    /// The pill's spot now (home unless dragged), kept on `area` at its painted size.
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

    /// The pointer on `area`, from an event at `local` in the pill's window.
    fn pointer(
        &self,
        local: Point<Pixels>,
        area: Bounds<Pixels>,
        window: &Window,
    ) -> Option<Point<Pixels>> {
        let origin = self.origin(area);
        match (&self.popup, self.drag) {
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

    /// The window sits where the pill was dragged, not pushed against an edge.
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

    /// Summed relative pointer motion, on layer-shell.
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
        let grab = Grab {
            offset: pointer - from,
            at: pointer,
            local: event.position,
            origin,
            travel: self.pointer_travel(),
        };
        if self.drag.is_none() {
            self.drag = Some(grab);
            cx.notify();
        }
    }

    fn on_pointer(
        &mut self,
        event: Pointer,
        local: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(grab) = self.drag else {
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

    /// For the hand cursors.
    #[cfg(windows)]
    fn grip(&self) -> crate::ui::platform::Grip {
        use crate::ui::platform::Grip;
        if self.drag.is_some() {
            return Grip::Held;
        }
        match self.painted.get() {
            Some((_, pill)) if self.movable() && self.takes_pointer() => Grip::Open(pill),
            _ => Grip::None,
        }
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    /// Moves the layer surface right away: pointer positions come relative to it.
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
                self.drag = None;
            }
        }
    }

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

    /// Steps the clock, motion and colours, and schedules the next frame.
    fn tick(&mut self, window: &Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let dt = self.clock.tick(now);
        let voice = self.is_recording().then(|| self.voice.latest());
        let edge = self.motion.step(dt, voice.as_ref());
        self.still = pill::holds_still(window, cx);
        if self.power_on == PowerOn::Due {
            // Starts on this, its first frame.
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
            // No Root sets the theme's font for us (see `open`).
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
                            // Only the pill takes clicks; elsewhere they fall through.
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
                // The last `max_lines` lines: bottom-anchored, clipped at the top.
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

    /// Lays the pill out at `spot`; the spacer by the hugged edge gives way as text grows.
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

    /// Re-renders after `interval`, unless the window has closed.
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
            // Off screen: click-through until a failure brings the pill back or the window closes.
            if self.painted.take().is_some() {
                set_pointer_region(window, None);
            }
            return div();
        }
        self.tick(window, cx);
        // Relative motion can arrive after the position: catch up before drawing.
        #[cfg(target_os = "linux")]
        if let (Some(grab), Some(travel), Some(area)) =
            (self.drag, self.pointer_travel(), self.area(window, cx))
            && let Some(then) = grab.travel.filter(|&then| then != travel)
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
        let follow = self.drag.is_some().then(|| {
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

/// Clicks only land on `pill` (nowhere for `None`); elsewhere they fall through.
fn set_pointer_region(window: &Window, pill: Option<Bounds<Pixels>>) {
    window.set_input_region(Some(pill.as_slice()));
    // Windows clips drawing to the region too: leave room for the shadow, water and glow.
    #[cfg(windows)]
    crate::ui::platform::set_input_region(window, pill.map(|pill| pill.dilate(px(INSET))));
    #[cfg(target_os = "macos")]
    crate::ui::platform::set_input_region(window, pill);
}

/// Recording dot, elapsed time, word count and an optional status.
pub(crate) fn header_row(
    theme: &Theme,
    elapsed: Duration,
    words: usize,
    status: Option<&'static str>,
) -> Div {
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
                .font_features(font::TABULAR.clone())
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

/// The × in the corner: discards while `live`, else just fades with the text.
fn discard_button(theme: &Theme, live: bool) -> Stateful<Div> {
    // Same inset from the top and right, centred on the header row.
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
                // Not a drag.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(|_, _, cx| cx.defer(crate::ui::discard))
        })
}

/// Each drag frame: follow the pointer; end on release, another press or (`exits`) leaving.
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
