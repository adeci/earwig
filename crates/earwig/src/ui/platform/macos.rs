//! macOS: a non-activating panel overlay on every Space, a menu-bar app, the system accent.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use earwig_core::desktop::DesktopCaps;
use gpui_kit::{
    App, Application, Bounds, DisplayId, Hsla, Pixels, PlatformDisplay, Point, Rgba, Window,
    WindowId, WindowingRequest, point, px,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::ui::OverlaySurface;

pub(crate) use super::tray_icon::{TrayHandle, spawn as spawn_tray};

/// `NSWindowStyleMaskTitled`
const TITLED: usize = 1;
/// `CanJoinAllSpaces | Stationary | IgnoresCycle | FullScreenAuxiliary`
const EVERY_SPACE: usize = 1 | 16 | 64 | 256;
/// How often we check whether the pointer is over the pill, the only part taking clicks.
const POINTER_POLL: f64 = 1. / 30.;

pub(crate) fn overlay_surface(_caps: &DesktopCaps) -> OverlaySurface {
    OverlaySurface::Popup
}

/// A menu-bar app: no Dock icon, no menu bar, never activated by the overlay.
pub(crate) fn application(_surface: OverlaySurface, _caps: &DesktopCaps) -> Application {
    gpui_kit::application().with_windowing(WindowingRequest::Headless)
}

/// The display with the key window, where the user is looking.
pub(crate) fn home_display(cx: &App) -> Option<Rc<dyn PlatformDisplay>> {
    let id: Option<u32> = unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            None
        } else {
            let description: *mut AnyObject = msg_send![screen, deviceDescription];
            let key = NSString::from_str("NSScreenNumber");
            let number: *mut AnyObject = msg_send![description, objectForKey: &*key];
            (!number.is_null()).then(|| msg_send![number, unsignedIntValue])
        }
    };
    id.and_then(|id| cx.find_display(DisplayId::new(u64::from(id))))
        .or_else(|| cx.primary_display())
}

type Region = Rc<Cell<Option<Bounds<Pixels>>>>;

thread_local! {
    /// The part of each popup that takes the pointer, in window coordinates.
    static INPUT_REGIONS: RefCell<HashMap<WindowId, Region>> = RefCell::default();
}

/// GPUI's `set_input_region` does nothing on macOS, so the window's [`Popup`] applies it.
pub(crate) fn set_input_region(window: &Window, area: Option<Bounds<Pixels>>) {
    let id = window.window_handle().window_id();
    INPUT_REGIONS.with_borrow(|regions| {
        if let Some(region) = regions.get(&id) {
            region.set(area);
        }
    });
}

/// The overlay's panel: never key, on every Space, click-through outside its input region.
pub(crate) struct Popup {
    panel: Retained<AnyObject>,
    window: WindowId,
    timer: Retained<AnyObject>,
}

impl Popup {
    pub(crate) fn new(window: &Window) -> anyhow::Result<Self> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|e| anyhow::anyhow!("no window handle: {e}"))?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            anyhow::bail!("the overlay is not an AppKit window");
        };
        let region = Region::default();
        let id = window.window_handle().window_id();
        INPUT_REGIONS.with_borrow_mut(|regions| regions.insert(id, region.clone()));
        unsafe {
            let view = handle.ns_view.as_ptr().cast::<AnyObject>();
            let panel: Option<Retained<AnyObject>> = msg_send![view, window];
            let panel = panel.ok_or_else(|| anyhow::anyhow!("the overlay has no NSWindow"))?;
            // Borderless, still non-activating. Restyling resizes, which GPUI can't take mid-build.
            let restyled = panel.clone();
            let borderless = block2::RcBlock::new(move || {
                let mask: usize = msg_send![&*restyled, styleMask];
                let () = msg_send![&*restyled, setStyleMask: mask & !TITLED];
            });
            let main: *mut AnyObject = msg_send![class!(NSOperationQueue), mainQueue];
            let () = msg_send![main, addOperationWithBlock: &*borderless];
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

    /// On the panel's screen, in points from the top left.
    pub(crate) fn pointer(&self, _scale: f32) -> Option<Point<Pixels>> {
        let mouse: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        let screen = self.screen()?;
        #[allow(clippy::cast_possible_truncation)]
        Some(point(
            px((mouse.x - screen.origin.x) as f32),
            px((screen.origin.y + screen.size.height - mouse.y) as f32),
        ))
    }

    pub(crate) fn move_to(&mut self, origin: Point<Pixels>, _scale: f32) {
        let Some(screen) = self.screen() else {
            return;
        };
        let top_left = NSPoint::new(
            screen.origin.x + f64::from(f32::from(origin.x)),
            screen.origin.y + screen.size.height - f64::from(f32::from(origin.y)),
        );
        // AppKit reports the move at once, which GPUI can't take mid-render: defer a turn.
        let panel = self.panel.clone();
        let moved = block2::RcBlock::new(move || unsafe {
            let () = msg_send![&*panel, setFrameTopLeftPoint: top_left];
        });
        unsafe {
            let main: *mut AnyObject = msg_send![class!(NSOperationQueue), mainQueue];
            let () = msg_send![main, addOperationWithBlock: &*moved];
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

/// Click-through unless the pointer is on the input region; left alone mid-drag.
fn poll_pointer(panel: Retained<AnyObject>, region: Region) -> Retained<AnyObject> {
    let ignoring = Cell::new(true);
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
        let inside = region.get().is_some_and(|area| area.contains(&local));
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

/// Sends `NSColor.controlAccentColor` now and whenever the system colours change.
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

fn launch_agent() -> Option<std::path::PathBuf> {
    Some(
        dirs::home_dir()?
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", earwig_core::APP_ID)),
    )
}

pub(crate) fn autostart() -> bool {
    launch_agent().is_some_and(|path| path.is_file())
}

/// A launch agent running this binary's daemon, from the next login.
pub(crate) fn set_autostart(enabled: bool) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let path = launch_agent().context("no home directory")?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().context("locating earwig")?;
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{id}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>daemon</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
"#,
        id = earwig_core::APP_ID,
        exe = escape(&exe.to_string_lossy()),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&path, plist).with_context(|| format!("writing {}", path.display()))
}
