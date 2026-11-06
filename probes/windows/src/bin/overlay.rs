//! overlay [seconds]: a transparent, topmost, non-activating GPUI popup with an
//! animated pill; drag it, click × to quit. Logs the foreground window.
#![windows_subsystem = "windows"]

use std::time::{Duration, Instant};

use gpui_kit::*;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn log(line: String) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\earwig-build\overlay-probe.log")
        .unwrap();
    let _ = writeln!(f, "{line}");
}

fn foreground() -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(GetForegroundWindow(), buf.as_mut_ptr(), 256) };
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        return MA_NOACTIVATE as LRESULT;
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn hwnd(window: &Window) -> HWND {
    match HasWindowHandle::window_handle(window).unwrap().as_raw() {
        RawWindowHandle::Win32(h) => h.hwnd.get() as HWND,
        _ => unreachable!(),
    }
}

struct Pill {
    hwnd: HWND,
    start: Instant,
    grab: Option<(POINT, POINT)>,
    region: Option<Bounds<Pixels>>,
}

impl Pill {
    fn cursor() -> POINT {
        let mut p = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut p) };
        p
    }

    fn window_origin(&self) -> POINT {
        let mut r = unsafe { std::mem::zeroed() };
        unsafe { GetWindowRect(self.hwnd, &mut r) };
        POINT {
            x: r.left,
            y: r.top,
        }
    }
}

impl Render for Pill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.start.elapsed().as_secs_f32();
        window.request_animation_frame();
        let bars = (0..24).map(|i| {
            let h = 6. + 14. * (0.5 + 0.5 * (t * 6. + i as f32 * 0.5).sin());
            div().w(px(4.)).h(px(h)).rounded(px(2.)).bg(rgb(0x7aa2f7))
        });
        let scale = window.scale_factor();
        let hwnd = self.hwnd;
        div().size_full().flex().items_end().justify_center().child(
            div()
                .id("pill")
                .relative()
                .w(px(420.))
                .p(px(14.))
                .rounded(px(24.))
                .bg(rgba(0x1a1b26ee))
                .border_1()
                .border_color(rgb(0x7aa2f7))
                .text_color(rgb(0xc0caf5))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .gap(px(3.))
                        .items_center()
                        .h(px(22.))
                        .children(bars),
                )
                .child(format!(
                    "Listening… scale {scale} fg={} t={t:.1}",
                    foreground()
                ))
                .child(
                    div()
                        .id("close")
                        .absolute()
                        .top(px(8.))
                        .right(px(10.))
                        .px(px(6.))
                        .rounded(px(10.))
                        .hover(|s| s.bg(rgba(0xffffff22)))
                        .child("×")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            log(format!("× clicked, foreground={}", foreground()));
                            cx.stop_propagation();
                            cx.quit();
                        }),
                )
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, cx| {
                            let _ = cx;
                            let _ = window;
                            // Clip input (and drawing) to the pill.
                            let s = scale;
                            let rgn = unsafe {
                                CreateRectRgn(
                                    (f32::from(bounds.left()) * s) as i32,
                                    (f32::from(bounds.top()) * s) as i32,
                                    (f32::from(bounds.right()) * s) as i32,
                                    (f32::from(bounds.bottom()) * s) as i32,
                                )
                            };
                            unsafe { SetWindowRgn(hwnd, rgn, 1) };
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, _| {
                        this.grab = Some((Self::cursor(), this.window_origin()));
                        log(format!("grab, foreground={}", foreground()));
                    }),
                )
                .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, _| {
                    if let Some((at, origin)) = this.grab {
                        let now = Self::cursor();
                        unsafe {
                            SetWindowPos(
                                this.hwnd,
                                std::ptr::null_mut(),
                                origin.x + now.x - at.x,
                                origin.y + now.y - at.y,
                                0,
                                0,
                                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                            )
                        };
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, _| {
                        this.grab = None;
                        log(format!("drop, foreground={}", foreground()));
                    }),
                ),
        )
    }
}

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    log(format!("start, foreground={}", foreground()));
    gpui_kit::application().run(move |cx: &mut App| {
        let display = cx.primary_display();
        let size = size(px(480.), px(160.));
        let origin = display.as_ref().map_or(point(px(0.), px(0.)), |d| {
            let b = d.bounds();
            log(format!("primary display bounds {b:?}"));
            point(
                b.origin.x + (b.size.width - size.width) / 2.,
                b.bottom() - size.height - px(40.),
            )
        });
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size })),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: display.as_ref().map(|d| d.id()),
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Client),
            inactive_frame_interval: None,
            ..WindowOptions::default()
        };
        cx.open_window(options, |window, cx| {
            let hwnd = hwnd(window);
            unsafe {
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE as isize);
                SetWindowSubclass(hwnd, Some(subclass), 1, 0);
            }
            log(format!(
                "opened, scale {}, foreground={}",
                window.scale_factor(),
                foreground()
            ));
            cx.new(|_| Pill {
                hwnd,
                start: Instant::now(),
                grab: None,
                region: None,
            })
        })
        .unwrap();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            log(format!("timeout, foreground={}", foreground()));
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
