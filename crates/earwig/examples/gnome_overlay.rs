//! Experiment (GNOME slice), not part of earwig: can GPUI float a small
//! overlay above the focused window on GNOME, where Mutter offers no
//! layer-shell?
//!
//! ```text
//! gnome_overlay popup [secs]     WindowKind::PopUp: an override-redirect X11
//!                                window when run with WAYLAND_DISPLAY unset
//!                                (XWayland), a toplevel on Wayland
//! gnome_overlay toplevel [secs]  plain WindowKind::Normal toplevel
//! ```
//!
//! Both ask for 420x90 px at the bottom center of the primary display,
//! without focus. Prints the bounds GPUI reports, quits after `secs`
//! (default 20).

use std::time::Duration;

use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, Context, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, application, div, point, px, rgb, size,
};

struct Overlay {
    label: SharedString,
}

impl Render for Overlay {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x00_20_60))
            .text_color(rgb(0xff_ff_ff))
            .text_xl()
            .child(self.label.clone())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (kind, label) = match args.first().map(String::as_str) {
        Some("popup") => (WindowKind::PopUp, "EARWIG POPUP OVERLAY"),
        Some("toplevel") => (WindowKind::Normal, "EARWIG TOPLEVEL OVERLAY"),
        _ => {
            eprintln!("usage: gnome_overlay popup|toplevel [secs]");
            std::process::exit(2);
        }
    };
    let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let backend = if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty()) {
        "wayland"
    } else {
        "x11"
    };

    application().run(move |cx: &mut App| {
        // GPUI on Wayland may not know the outputs yet; assume the VM's
        // 1280x800 then (a toplevel cannot pick its position there anyway).
        let display = cx.primary_display();
        let screen = display.as_ref().map_or_else(
            || Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(1280.), px(800.)),
            },
            |d| d.bounds(),
        );
        let overlay = size(px(420.), px(90.));
        let origin = point(
            screen.origin.x + (screen.size.width - overlay.width) / 2.,
            screen.origin.y + screen.size.height - overlay.height - px(48.),
        );
        let bounds = Bounds {
            origin,
            size: overlay,
        };
        println!("backend={backend} kind={kind:?} screen={screen:?} requested={bounds:?}");
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            display_id: display.map(|d| d.id()),
            titlebar: None,
            focus: false,
            show: true,
            kind,
            is_movable: false,
            is_resizable: false,
            app_id: Some("io.github.adeci.earwig.overlay".into()),
            window_background: WindowBackgroundAppearance::Opaque,
            ..WindowOptions::default()
        };
        cx.open_window(options, |window, cx| {
            println!("opened: bounds={:?}", window.bounds());
            cx.new(|_| Overlay {
                label: label.into(),
            })
        })
        .expect("opening the overlay window");
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
