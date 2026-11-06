//! The overlay as a GPUI pop-up panel driven by the Right Command tap, a
//! menu-bar icon, and a settings-like window that takes keyboard focus.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use gpui_kit::{
    App, AppContext as _, Bounds, Context, FocusHandle, Global, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseMoveEvent, ParentElement as _, PathBuilder,
    Pixels, QuitMode, Render, StatefulInteractiveElement as _, Styled as _, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
    WindowingRequest, canvas, div, hsla, point, px, rgb, size,
};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::hotkey::{self, Edge};

const WIDTH: f32 = 560.;
const HEIGHT: f32 = 96.;
const INSET: f32 = 24.;
const MARGIN: f32 = 56.;

struct Probe {
    text: String,
    overlay: Option<WindowHandle<Pill>>,
    settings: Option<WindowHandle<Settings>>,
    dictations: u32,
    _tray: Option<TrayIcon>,
    toggle: MenuItem,
}

impl Global for Probe {}

enum Ui {
    Edge(Edge),
    Menu(String),
}

/// `show_for`: open the overlay at once and quit after that long (CI
/// screenshots), instead of waiting for the hotkey.
pub fn run(text: String, show_for: Option<Duration>) {
    let (tx, rx) = flume::unbounded();
    if show_for.is_none() {
        let (edges_tx, edges_rx) = flume::unbounded();
        hotkey::spawn(edges_tx);
        let tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(edge) = edges_rx.recv() {
                let _ = tx.send(Ui::Edge(edge));
            }
        });
    }
    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = menu_tx.send(Ui::Menu(event.id.0));
    }));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_windowing(WindowingRequest::Headless)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            crate::log(&format!("accent: {}", accent()));
            let toggle = MenuItem::with_id("toggle", "Start dictation", true, None);
            let tray = tray(&toggle);
            cx.set_global(Probe {
                text,
                overlay: None,
                settings: None,
                dictations: 0,
                _tray: tray,
                toggle,
            });
            cx.spawn(async move |cx| {
                while let Ok(event) = rx.recv_async().await {
                    cx.update(|cx| match event {
                        Ui::Edge(edge) => on_edge(edge, cx),
                        Ui::Menu(id) => on_menu(&id, cx),
                    });
                }
            })
            .detach();
            if let Some(after) = show_for {
                open_overlay(cx);
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(after).await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
        });
}

fn tray(toggle: &MenuItem) -> Option<TrayIcon> {
    let menu = Menu::new();
    let settings = MenuItem::with_id("settings", "Settings…", true, None);
    let quit = MenuItem::with_id("quit", "Quit earwig probe", true, None);
    if let Err(e) = menu.append_items(&[&settings, toggle, &PredefinedMenuItem::separator(), &quit])
    {
        crate::log(&format!("tray: menu: {e}"));
    }
    let built = Icon::from_rgba(icon(36), 36, 36)
        .map_err(|e| e.to_string())
        .and_then(|icon| {
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_icon(icon)
                .with_tooltip("earwig probe")
                .build()
                .map_err(|e| e.to_string())
        });
    match built {
        Ok(tray) => {
            crate::log("tray: menu-bar icon added");
            Some(tray)
        }
        Err(e) => {
            crate::log(&format!("tray: {e}"));
            None
        }
    }
}

fn on_menu(id: &str, cx: &mut App) {
    crate::log(&format!("tray: menu item {id}"));
    match id {
        "settings" => open_settings(cx),
        "toggle" => {
            if cx.global::<Probe>().overlay.is_some() {
                finish(cx, true);
            } else {
                open_overlay(cx);
            }
        }
        "quit" => cx.quit(),
        _ => {}
    }
}

fn on_edge(edge: Edge, cx: &mut App) {
    match edge {
        Edge::Press => {
            if cx.global::<Probe>().overlay.is_some() {
                finish(cx, true);
            } else {
                open_overlay(cx);
            }
        }
        Edge::Release => {
            let held = cx
                .global::<Probe>()
                .overlay
                .and_then(|w| w.read_with(cx, |pill, _| pill.started.elapsed()).ok());
            match held {
                Some(held) if held >= Duration::from_millis(300) => finish(cx, true),
                Some(held) => crate::log(&format!(
                    "overlay: tap ({held:?}): latched until next press"
                )),
                None => {}
            }
        }
        Edge::Chord => finish(cx, false),
    }
}

fn finish(cx: &mut App, insert: bool) {
    let Some(window) = cx.global_mut::<Probe>().overlay.take() else {
        return;
    };
    let panel = window
        .read_with(cx, |pill, _| pill.panel)
        .unwrap_or(std::ptr::null_mut());
    crate::log(&format!("overlay: closing; {}", focus_state(panel)));
    let _ = window.update(cx, |_, window, _| window.remove_window());
    let _ = cx.global::<Probe>().toggle.set_text("Start dictation");
    if insert {
        let probe = cx.global_mut::<Probe>();
        probe.dictations += 1;
        let text = format!("{} #{} ", probe.text, probe.dictations);
        crate::paste::paste(&text);
    } else {
        crate::log("overlay: discarded, nothing inserted");
    }
}

fn open_overlay(cx: &mut App) {
    let display = cx.primary_display();
    let origin = display.as_ref().map_or(point(px(0.), px(0.)), |d| {
        let b = d.bounds();
        point(
            b.left() + (b.size.width - px(WIDTH + 2. * INSET)) / 2.,
            b.bottom() - px(MARGIN) - px(HEIGHT + INSET),
        )
    });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: size(px(WIDTH + 2. * INSET), px(HEIGHT + 2. * INSET)),
        })),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: display.map(|d| d.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        inactive_frame_interval: None,
        ..WindowOptions::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        let panel = ns_window(window);
        configure(panel);
        crate::log(&format!("overlay: opened; {}", focus_state(panel)));
        cx.new(|cx| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(30))
                        .await;
                    if this
                        .update(cx, |pill: &mut Pill, _| pill.click_through())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Pill {
                started: Instant::now(),
                panel,
                grab: None,
                ignoring: None,
            }
        })
    });
    match opened {
        Ok(handle) => {
            cx.global_mut::<Probe>().overlay = Some(handle);
            let _ = cx.global::<Probe>().toggle.set_text("Stop dictation");
        }
        Err(e) => crate::log(&format!("overlay: open failed: {e:#}")),
    }
}

fn ns_window(window: &Window) -> *mut AnyObject {
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::AppKit(h)) => unsafe {
            let view = h.ns_view.as_ptr().cast::<AnyObject>();
            msg_send![view, window]
        },
        _ => std::ptr::null_mut(),
    }
}

/// Make GPUI's pop-up panel an overlay: borderless, never key, no shadow,
/// on every Space and over full-screen apps, skipped by window cycling.
fn configure(panel: *mut AnyObject) {
    if panel.is_null() {
        crate::log("overlay: no NSWindow");
        return;
    }
    unsafe {
        let mask: usize = msg_send![panel, styleMask];
        let level: isize = msg_send![panel, level];
        let behavior: usize = msg_send![panel, collectionBehavior];
        crate::log(&format!(
            "overlay: GPUI panel styleMask={mask:#x} level={level} collectionBehavior={behavior:#x}"
        ));
        if std::env::var_os("PROBE_TITLED").is_none() {
            // NSWindowStyleMaskTitled
            let () = msg_send![panel, setStyleMask: mask & !1];
        }
        let () = msg_send![panel, setBecomesKeyOnlyIfNeeded: Bool::YES];
        let () = msg_send![panel, setHidesOnDeactivate: Bool::NO];
        let () = msg_send![panel, setHasShadow: Bool::NO];
        // CanJoinAllSpaces | Stationary | IgnoresCycle | FullScreenAuxiliary
        let () = msg_send![panel, setCollectionBehavior: 1usize | 16 | 64 | 256];
        let mask: usize = msg_send![panel, styleMask];
        crate::log(&format!("overlay: now styleMask={mask:#x}"));
    }
}

fn focus_state(panel: *mut AnyObject) -> String {
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let front: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let pid: i32 = if front.is_null() {
            -1
        } else {
            msg_send![front, processIdentifier]
        };
        let textedit = !front.is_null() && {
            let id: Option<objc2::rc::Retained<objc2_foundation::NSString>> =
                msg_send![front, bundleIdentifier];
            id.is_some_and(|id| id.to_string() == "com.apple.TextEdit")
        };
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let active: Bool = msg_send![app, isActive];
        let key = !panel.is_null() && {
            let key: Bool = msg_send![panel, isKeyWindow];
            key.as_bool()
        };
        format!(
            "frontmost-is-self={} frontmost-is-textedit={textedit} probe-app-active={} panel-is-key={key}",
            u32::try_from(pid).is_ok_and(|pid| pid == std::process::id()),
            active.as_bool()
        )
    }
}

fn mouse() -> NSPoint {
    unsafe { msg_send![class!(NSEvent), mouseLocation] }
}

fn accent() -> String {
    unsafe {
        let color: *mut AnyObject = msg_send![class!(NSColor), controlAccentColor];
        let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let rgb: *mut AnyObject = msg_send![color, colorUsingColorSpace: space];
        if rgb.is_null() {
            return "none".into();
        }
        let (r, g, b): (f64, f64, f64) = (
            msg_send![rgb, redComponent],
            msg_send![rgb, greenComponent],
            msg_send![rgb, blueComponent],
        );
        format!("controlAccentColor sRGB ({r:.3}, {g:.3}, {b:.3})")
    }
}

struct Pill {
    started: Instant,
    panel: *mut AnyObject,
    /// Pointer minus the window's top-left corner, in screen points.
    grab: Option<(f64, f64)>,
    ignoring: Option<bool>,
}

impl Pill {
    fn frame(&self) -> NSRect {
        unsafe { msg_send![self.panel, frame] }
    }

    /// Clicks pass through everywhere but the pill.
    fn click_through(&mut self) {
        if self.panel.is_null() || self.grab.is_some() {
            return;
        }
        let (m, f) = (mouse(), self.frame());
        let inset = f64::from(INSET);
        let inside = m.x >= f.origin.x + inset
            && m.x <= f.origin.x + f.size.width - inset
            && m.y >= f.origin.y + inset
            && m.y <= f.origin.y + f.size.height - inset;
        if self.ignoring != Some(!inside) {
            self.ignoring = Some(!inside);
            unsafe {
                let () = msg_send![self.panel, setIgnoresMouseEvents: Bool::new(!inside)];
            }
        }
    }

    fn start_drag(&mut self) {
        if self.panel.is_null() {
            return;
        }
        let (m, f) = (mouse(), self.frame());
        self.grab = Some((m.x - f.origin.x, m.y - (f.origin.y + f.size.height)));
    }

    fn drag(&mut self) {
        let Some((dx, dy)) = self.grab else {
            return;
        };
        let m = mouse();
        unsafe {
            let () = msg_send![self.panel, setFrameTopLeftPoint: NSPoint::new(m.x - dx, m.y - dy)];
        }
    }

    fn end_drag(&mut self) {
        if self.grab.take().is_some() {
            crate::log(&format!("overlay: dropped; {}", focus_state(self.panel)));
        }
    }
}

impl Render for Pill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        let t = self.started.elapsed().as_secs_f32();
        div().size_full().p(px(INSET)).child(
            div()
                .id("pill")
                .group("pill")
                .relative()
                .size_full()
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, (), window, _| wave(bounds, t, window),
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(22.))
                        .top(px(18.))
                        .text_size(px(15.))
                        .text_color(rgb(0xc0ca_f5))
                        .child(format!("earwig probe · {t:.1} s · TextEdit keeps focus")),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(22.))
                        .top(px(46.))
                        .text_size(px(13.))
                        .text_color(rgb(0x7aa2_f7))
                        .child("drag me · × discards"),
                )
                .child(
                    div()
                        .id("discard")
                        .absolute()
                        .top(px(12.))
                        .right(px(14.))
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_color(rgb(0xc0ca_f5))
                        .invisible()
                        .group_hover("pill", gpui_kit::StyleRefinement::visible)
                        .hover(|s| s.bg(hsla(0., 0., 1., 0.12)))
                        .child("×")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, _, cx| {
                            cx.defer(|cx| {
                                crate::log("overlay: × clicked");
                                finish(cx, false);
                            });
                        }),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.start_drag()),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.end_drag()),
                ),
        )
    }
}

/// A rounded pill whose outline ripples.
fn wave(bounds: Bounds<Pixels>, t: f32, window: &mut Window) {
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let r = h / 2.;
    let straight = w - 2. * r;
    let perimeter = 2. * straight + TAU * r;
    let n = 240;
    let mut outline = Vec::with_capacity(n);
    #[allow(clippy::cast_precision_loss)]
    for i in 0..n {
        let s = perimeter * i as f32 / n as f32;
        // Point and outward normal on the stadium at arc length `s`.
        let (px_, py_, nx, ny) = if s < straight {
            (x0 + r + s, y0, 0., -1.)
        } else if s < straight + TAU * r / 2. {
            let a = (s - straight) / r - TAU / 4.;
            (
                x0 + w - r + r * a.cos(),
                y0 + r + r * a.sin(),
                a.cos(),
                a.sin(),
            )
        } else if s < 2. * straight + TAU * r / 2. {
            let d = s - straight - TAU * r / 2.;
            (x0 + w - r - d, y0 + h, 0., 1.)
        } else {
            let a = (s - 2. * straight - TAU * r / 2.) / r + TAU / 4.;
            (x0 + r + r * a.cos(), y0 + r + r * a.sin(), a.cos(), a.sin())
        };
        let ripple = 3.5 * (s / 38. - t * 3.).sin() * (0.6 + 0.4 * (t * 1.7).sin()) - 4.;
        outline.push(point(px(px_ + nx * ripple), px(py_ + ny * ripple)));
    }
    let mut body = PathBuilder::fill();
    body.add_polygon(&outline, true);
    if let Ok(path) = body.build() {
        window.paint_path(path, hsla(235. / 360., 0.22, 0.13, 0.97));
    }
    let mut line = PathBuilder::stroke(px(1.5));
    line.add_polygon(&outline, true);
    if let Ok(path) = line.build() {
        window.paint_path(path, Hsla::from(rgb(0x7aa2_f7)));
    }
}

struct Settings {
    focus: FocusHandle,
    typed: String,
}

fn open_settings(cx: &mut App) {
    if let Some(window) = cx.global::<Probe>().settings {
        if window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.activate(true);
            return;
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(480.), px(240.)), cx)),
        focus: true,
        ..WindowOptions::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Settings {
                focus,
                typed: String::new(),
            }
        })
    });
    cx.activate(true);
    match opened {
        Ok(handle) => cx.global_mut::<Probe>().settings = Some(handle),
        Err(e) => crate::log(&format!("settings: open failed: {e:#}")),
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .size_full()
            .p(px(24.))
            .bg(rgb(0x1a1b_26))
            .text_color(rgb(0xc0ca_f5))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child("earwig probe settings: type here")
            .child(format!("typed: {}", self.typed))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if let Some(c) = &event.keystroke.key_char {
                    this.typed.push_str(c);
                    crate::log("settings: key typed into the settings window");
                    cx.notify();
                }
            }))
    }
}

/// earwig's icon: a blue disc with three white bars, RGBA.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn icon(size: u32) -> Vec<u8> {
    let s = size as f32;
    let (center, radius, unit) = (s / 2., s / 2. - 0.5, s / 24.);
    let bars = [(8.0_f32, 3.5_f32), (12., 6.5), (16., 3.5)];
    let half_width = 1.25 * unit;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px_, py_) = (x as f32 + 0.5, y as f32 + 0.5);
            let dist = ((px_ - center).powi(2) + (py_ - center).powi(2)).sqrt();
            let coverage = (radius - dist + 0.5).clamp(0., 1.);
            let bar = bars
                .iter()
                .map(|&(bx, half)| {
                    let dy = (py_ - center).abs() - half * unit;
                    let edge = if dy > 0. {
                        half_width - (dy.powi(2) + (px_ - bx * unit).powi(2)).sqrt()
                    } else {
                        half_width - (px_ - bx * unit).abs()
                    };
                    (edge + 0.5).clamp(0., 1.)
                })
                .fold(0., f32::max);
            let mix = |c: u8| (f32::from(c) * (1. - bar) + 255. * bar).round() as u8;
            data.extend_from_slice(&[
                mix(0x2b),
                mix(0x6c),
                mix(0xb0),
                (coverage * 255.).round() as u8,
            ]);
        }
    }
    data
}
