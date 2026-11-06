//! macOS: the overlay is GPUI's pop-up panel made into a borderless,
//! non-activating overlay on every Space; earwig runs as a menu-bar app (no
//! Dock icon) and follows the system accent colour.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use earwig_core::desktop::DesktopCaps;
use gpui_kit::{
    App, Application, Bounds, Hsla, Pixels, Point, Rgba, Window, WindowId, WindowingRequest,
    point, px,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::ui::OverlaySurface;

/// `NSWindowStyleMaskTitled`
const TITLED: usize = 1;
/// `CanJoinAllSpaces | Stationary | IgnoresCycle | FullScreenAuxiliary`
const EVERY_SPACE: usize = 1 | 16 | 64 | 256;
/// How often the pointer is checked against the pill, which alone takes
/// clicks.
const POINTER_POLL: f64 = 1. / 30.;

pub(crate) fn overlay_surface(_caps: &DesktopCaps) -> OverlaySurface {
    OverlaySurface::Popup
}

/// A menu-bar app: no Dock icon, no menu bar, never activated by the
/// overlay.
pub(crate) fn application(_surface: OverlaySurface, _caps: &DesktopCaps) -> Application {
    gpui_kit::application().with_windowing(WindowingRequest::Headless)
}

thread_local! {
    /// The part of each popup that takes the pointer, in window coordinates.
    static INPUT_REGIONS: RefCell<HashMap<WindowId, Rc<RefCell<Option<Vec<Bounds<Pixels>>>>>>> =
        RefCell::default();
}

/// Like [`Window::set_input_region`], which GPUI leaves to the platform and
/// does nothing on macOS: the overlay's [`Popup`] applies it instead.
pub(crate) fn set_input_region(window: &mut Window, region: Option<&[Bounds<Pixels>]>) {
    let id = window.window_handle().window_id();
    INPUT_REGIONS.with_borrow(|regions| {
        if let Some(shared) = regions.get(&id) {
            *shared.borrow_mut() = region.map(<[_]>::to_vec);
        }
    });
}

/// The overlay's panel: kept off the key window and out of window cycling,
/// on every Space and over full-screen apps, moved by hand while dragged,
/// and transparent to clicks outside its input region.
pub(crate) struct Popup {
    panel: Retained<AnyObject>,
    window: WindowId,
    timer: Retained<AnyObject>,
}

impl Popup {
    pub(crate) fn new(window: &Window) -> anyhow::Result<Self> {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)?.as_raw()
        else {
            anyhow::bail!("the overlay is not an AppKit window");
        };
        let region = Rc::new(RefCell::new(None));
        let id = window.window_handle().window_id();
        INPUT_REGIONS.with_borrow_mut(|regions| regions.insert(id, region.clone()));
        unsafe {
            let view = handle.ns_view.as_ptr().cast::<AnyObject>();
            let panel: Option<Retained<AnyObject>> = msg_send![view, window];
            let panel = panel.ok_or_else(|| anyhow::anyhow!("the overlay has no NSWindow"))?;
            let mask: usize = msg_send![&*panel, styleMask];
            // Borderless, but still a non-activating panel: clicking it
            // leaves the app being typed in active.
            let () = msg_send![&*panel, setStyleMask: mask & !TITLED];
            let () = msg_send![&*panel, setBecomesKeyOnlyIfNeeded: Bool::YES];
            let () = msg_send![&*panel, setHidesOnDeactivate: Bool::NO];
            // The pill draws its own shadow.
            let () = msg_send![&*panel, setHasShadow: Bool::NO];
            let () = msg_send![&*panel, setCollectionBehavior: EVERY_SPACE];
            let () = msg_send![&*panel, setIgnoresMouseEvents: Bool::YES];
            let timer = poll_pointer(panel.clone(), region);
            Ok(Self {
                panel,
                window: id,
                timer,
            })
        }
    }

    /// The pointer on the panel's screen, in GPUI's coordinates there (top
    /// left origin, points).
    pub(crate) fn pointer(&self, _scale: f32) -> Option<Point<Pixels>> {
        let mouse: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        let screen = self.screen()?;
        #[allow(clippy::cast_possible_truncation)]
        Some(point(
            px((mouse.x - screen.origin.x) as f32),
            px((screen.origin.y + screen.size.height - mouse.y) as f32),
        ))
    }

    /// Put the panel's top-left corner at `origin` on its screen.
    pub(crate) fn move_to(&mut self, origin: Point<Pixels>, _scale: f32) {
        let Some(screen) = self.screen() else {
            return;
        };
        let top_left = NSPoint::new(
            screen.origin.x + f64::from(f32::from(origin.x)),
            screen.origin.y + screen.size.height - f64::from(f32::from(origin.y)),
        );
        unsafe {
            let () = msg_send![&*self.panel, setFrameTopLeftPoint: top_left];
        }
    }

    fn screen(&self) -> Option<NSRect> {
        unsafe {
            let screen: *mut AnyObject = msg_send![&*self.panel, screen];
            (!screen.is_null()).then(|| msg_send![screen, frame])
        }
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            let () = msg_send![&*self.timer, invalidate];
        }
        INPUT_REGIONS.with_borrow_mut(|regions| regions.remove(&self.window));
    }
}

/// Let clicks through the panel unless the pointer is on its input region.
/// Left alone while a button is down, so a drag keeps its window.
fn poll_pointer(
    panel: Retained<AnyObject>,
    region: Rc<RefCell<Option<Vec<Bounds<Pixels>>>>>,
) -> Retained<AnyObject> {
    let ignoring = std::cell::Cell::new(true);
    let tick = block2::RcBlock::new(move |_timer: *mut AnyObject| unsafe {
        let buttons: usize = msg_send![class!(NSEvent), pressedMouseButtons];
        if buttons != 0 {
            return;
        }
        let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let frame: NSRect = msg_send![&*panel, frame];
        #[allow(clippy::cast_possible_truncation)]
        let local = point(
            px((mouse.x - frame.origin.x) as f32),
            px((frame.origin.y + frame.size.height - mouse.y) as f32),
        );
        let inside = region
            .borrow()
            .as_ref()
            .is_none_or(|rects| rects.iter().any(|rect| rect.contains(&local)));
        if ignoring.get() == inside {
            ignoring.set(!inside);
            let () = msg_send![&*panel, setIgnoresMouseEvents: Bool::new(!inside)];
        }
    });
    unsafe {
        msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: POINTER_POLL,
            repeats: Bool::YES,
            block: &*tick
        ]
    }
}

/// Send `NSColor.controlAccentColor` now and whenever the system colours
/// change.
pub(crate) fn follow_accent(tx: &flume::Sender<Option<Hsla>>, _cx: &App) {
    let _ = tx.send(accent());
    let tx = tx.clone();
    let changed = block2::RcBlock::new(move |_note: *mut AnyObject| {
        let _ = tx.send(accent());
    });
    unsafe {
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let main: *mut AnyObject = msg_send![class!(NSOperationQueue), mainQueue];
        let name = NSString::from_str("NSSystemColorsDidChangeNotification");
        let observer: Option<Retained<AnyObject>> = msg_send![
            center,
            addObserverForName: &*name,
            object: std::ptr::null::<AnyObject>(),
            queue: main,
            usingBlock: &*changed
        ];
        // Observes for the life of the app.
        std::mem::forget(observer);
    }
}

fn accent() -> Option<Hsla> {
    unsafe {
        let color: *mut AnyObject = msg_send![class!(NSColor), controlAccentColor];
        let srgb: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let color: *mut AnyObject = msg_send![color, colorUsingColorSpace: srgb];
        if color.is_null() {
            return None;
        }
        let (r, g, b): (f64, f64, f64) = (
            msg_send![color, redComponent],
            msg_send![color, greenComponent],
            msg_send![color, blueComponent],
        );
        #[allow(clippy::cast_possible_truncation)]
        Some(
            Rgba {
                r: r as f32,
                g: g as f32,
                b: b as f32,
                a: 1.,
            }
            .into(),
        )
    }
}
