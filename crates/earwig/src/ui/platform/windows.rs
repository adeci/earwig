//! Windows: a never-activating topmost popup with hand cursors, a tray icon, the DWM accent.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use anyhow::{Context as _, bail};
use earwig_core::desktop::DesktopCaps;
use gpui_kit::{
    App, Application, Bounds, DisplayId, Hsla, Pixels, PlatformDisplay, Point, Rgba, Window, point,
    px,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use resvg::{tiny_skia, usvg};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, CreateRectRgn,
    DIB_RGB_COLORS, DeleteObject, MONITOR_DEFAULTTOPRIMARY, MonitorFromWindow, ScreenToClient,
    SetWindowRgn,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, KEY_SET_VALUE, REG_NOTIFY_CHANGE_LAST_SET,
    REG_SZ, RRF_RT_ANY, RRF_RT_REG_DWORD, RegCloseKey, RegDeleteValueW, RegGetValueW,
    RegNotifyChangeKeyValue, RegOpenKeyExW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetCapture;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GWL_EXSTYLE, GetCursor, GetCursorPos, GetForegroundWindow,
    GetWindowLongPtrW, HCURSOR, HTCLIENT, ICONINFO, IDC_ARROW, LoadCursorW, MA_NOACTIVATE,
    PostMessageW, SM_CXCURSOR, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetCursor,
    SetWindowLongPtrW, SetWindowPos, WM_APP, WM_MOUSEACTIVATE, WM_SETCURSOR, WS_EX_NOACTIVATE,
    WindowFromPoint,
};

use crate::ui::OverlaySurface;

pub(crate) use super::tray_icon::{TrayHandle, spawn as spawn_tray};

pub(crate) fn overlay_surface(_caps: &DesktopCaps) -> OverlaySurface {
    OverlaySurface::Popup
}

pub(crate) fn application(_surface: OverlaySurface, _caps: &DesktopCaps) -> Application {
    gpui_kit::application()
}

/// The display with the focused window.
pub(crate) fn home_display(cx: &App) -> Option<std::rc::Rc<dyn PlatformDisplay>> {
    // SAFETY: plain calls; a null window maps to the primary monitor.
    let monitor = unsafe { MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY) };
    // GPUI's display ids on Windows are monitor handles.
    let id = DisplayId::new(monitor as u64);
    cx.find_display(id).or_else(|| cx.primary_display())
}

pub(crate) struct Popup {
    hwnd: HWND,
    /// Origin last asked for, `None` while the window is where it opened.
    origin: Option<Point<Pixels>>,
}

impl Popup {
    /// Never take activation, and clip to nothing until the pill is painted.
    pub(crate) fn new(window: &Window) -> anyhow::Result<Self> {
        let hwnd = hwnd(window)?;
        // SAFETY: `hwnd` is the live window GPUI just created on this thread.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                style | isize::try_from(WS_EX_NOACTIVATE).unwrap_or(0),
            );
            if SetWindowSubclass(hwnd, Some(no_activate), 1, 0) == 0 {
                bail!("subclassing the overlay window failed");
            }
        }
        set_input_region(window, None);
        Ok(Self { hwnd, origin: None })
    }

    /// The pointer on the screen, in logical pixels.
    #[expect(clippy::unused_self, reason = "the same signature on every OS")]
    pub(crate) fn pointer(&self, scale: f32) -> Option<Point<Pixels>> {
        let mut pointer = POINT { x: 0, y: 0 };
        // SAFETY: `pointer` is written.
        if unsafe { GetCursorPos(&raw mut pointer) } == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some(point(
            px(pointer.x as f32 / scale),
            px(pointer.y as f32 / scale),
        ))
    }

    pub(crate) fn move_to(&mut self, origin: Point<Pixels>, scale: f32) {
        if self.origin == Some(origin) {
            return;
        }
        self.origin = Some(origin);
        #[allow(clippy::cast_possible_truncation)]
        let device = |value: Pixels| (f32::from(value) * scale).round() as i32;
        // WM_MOVE arrives at once, which GPUI can't take mid-render: post the move to ourselves.
        let packed =
            (i64::from(device(origin.y)) << 32) | i64::from(device(origin.x).cast_unsigned());
        // SAFETY: posting to a live window of this thread.
        #[allow(clippy::cast_possible_truncation)]
        let packed = packed as LPARAM;
        if unsafe { PostMessageW(self.hwnd, MOVE_TO, 0, packed) } == 0 {
            log::warn!("moving the overlay: {}", std::io::Error::last_os_error());
        }
    }
}

/// Clicks only land inside `area` (none for `None`). It clips drawing too.
pub(crate) fn set_input_region(window: &Window, area: Option<Bounds<Pixels>>) {
    let Ok(hwnd) = hwnd(window) else {
        return;
    };
    let scale = window.scale_factor();
    #[allow(clippy::cast_possible_truncation)]
    let device = |value: Pixels| (f32::from(value) * scale).round() as i32;
    let (left, top, right, bottom) = area.map_or((0, 0, 0, 0), |area| {
        (
            device(area.left()),
            device(area.top()),
            device(area.right()),
            device(area.bottom()),
        )
    });
    // SAFETY: the system owns the region once SetWindowRgn accepts it.
    unsafe { SetWindowRgn(hwnd, CreateRectRgn(left, top, right, bottom), 1) };
}

fn hwnd(window: &Window) -> anyhow::Result<HWND> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|e| anyhow::anyhow!("no window handle: {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as HWND),
        _ => bail!("the overlay is not a Win32 window"),
    }
}

/// How the pointer may take the pill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Grip {
    /// It cannot: the cursor is GPUI's.
    None,
    /// Grabbable over these bounds where GPUI shows an arrow (not over the ×).
    Open(Bounds<Pixels>),
    /// It holds the pill.
    Held,
}

thread_local! {
    static GRIP: Cell<Grip> = const { Cell::new(Grip::None) };
    /// The hand cursors by size in pixels: open, closed.
    static HANDS: RefCell<HashMap<i32, (HCURSOR, HCURSOR)>> = RefCell::new(HashMap::new());
}

/// The hand cursors GPUI shows as arrows here; applied at once if the pointer is on `window`.
pub(crate) fn set_grip(window: &Window, grip: Grip) {
    if GRIP.replace(grip) == grip {
        return;
    }
    let Ok(hwnd) = hwnd(window) else {
        return;
    };
    // SAFETY: plain calls on a live window of this thread.
    unsafe {
        let mut pointer = POINT { x: 0, y: 0 };
        let over = GetCapture() == hwnd
            || (GetCursorPos(&raw mut pointer) != 0 && WindowFromPoint(pointer) == hwnd);
        if !over {
            return;
        }
        match grip_cursor(hwnd) {
            Some(cursor) => SetCursor(cursor),
            // Back from a hand to GPUI's cursor, which the next move sets.
            None if is_hand(hwnd, GetCursor()) => {
                SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_ARROW))
            }
            None => GetCursor(),
        };
    }
}

/// The hand for the pointer on `hwnd` now, if any (GPUI has set its own cursor already).
unsafe fn grip_cursor(hwnd: HWND) -> Option<HCURSOR> {
    let (open, closed) = hands(hwnd)?;
    match GRIP.get() {
        Grip::None => None,
        Grip::Held => Some(closed),
        Grip::Open(pill) => {
            let mut pointer = POINT { x: 0, y: 0 };
            // SAFETY: `pointer` is written; `hwnd` is live.
            unsafe {
                if GetCursorPos(&raw mut pointer) == 0
                    || ScreenToClient(hwnd, &raw mut pointer) == 0
                {
                    return None;
                }
            }
            #[allow(clippy::cast_precision_loss)]
            let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.;
            #[allow(clippy::cast_precision_loss)]
            let at = point(px(pointer.x as f32 / scale), px(pointer.y as f32 / scale));
            // SAFETY: plain calls.
            let arrow = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
            // Not where GPUI shows a cursor of its own (the ×).
            let shown = unsafe { GetCursor() };
            (pill.contains(&at) && (shown == arrow || shown == open || shown == closed))
                .then_some(open)
        }
    }
}

fn is_hand(hwnd: HWND, cursor: HCURSOR) -> bool {
    hands(hwnd).is_some_and(|(open, closed)| cursor == open || cursor == closed)
}

/// The hand cursors at the system's cursor size for `hwnd`'s DPI.
fn hands(hwnd: HWND) -> Option<(HCURSOR, HCURSOR)> {
    // SAFETY: plain calls on a live window.
    let size = unsafe { GetSystemMetricsForDpi(SM_CXCURSOR, GetDpiForWindow(hwnd)) };
    let size = if size > 0 { size } else { 32 };
    HANDS.with_borrow_mut(|hands| {
        if let Some(&hands) = hands.get(&size) {
            return Some(hands);
        }
        let open = cursor(OPEN_HAND, size)?;
        let closed = cursor(CLOSED_HAND, size)?;
        hands.insert(size, (open, closed));
        Some((open, closed))
    })
}

const OPEN_HAND: &str = include_str!("../../../../../assets/cursors/open-hand.svg");
const CLOSED_HAND: &str = include_str!("../../../../../assets/cursors/closed-hand.svg");

/// `svg` as a `size`×`size` cursor, its hotspot in the middle (the palm).
fn cursor(svg: &str, size: i32) -> Option<HCURSOR> {
    let width = u32::try_from(size).ok()?;
    let pixels = bgra(svg, width);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).ok()?,
            biWidth: size,
            // Negative: rows top to bottom.
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: the DIB has room for the copied pixels; the cursor copies the bitmaps.
    unsafe {
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(
            std::ptr::null_mut(),
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast(), pixels.len());
        // The mask is unused with an alpha channel, but required.
        let mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let icon = ICONINFO {
            fIcon: 0,
            xHotspot: width / 2,
            yHotspot: width / 2,
            hbmMask: mask,
            hbmColor: color,
        };
        let cursor = CreateIconIndirect(&raw const icon);
        DeleteObject(color);
        DeleteObject(mask);
        (!cursor.is_null()).then_some(cursor)
    }
}

/// `svg` drawn `size`×`size`, BGRA with straight alpha, rows top to bottom.
#[allow(clippy::cast_precision_loss)]
fn bgra(svg: &str, size: u32) -> Vec<u8> {
    let tree =
        usvg::Tree::from_str(svg, &usvg::Options::default()).expect("the cursors are valid SVG");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("a cursor has pixels");
    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let pixel = pixel.demultiply();
            [pixel.blue(), pixel.green(), pixel.red(), pixel.alpha()]
        })
        .collect()
}

/// [`Popup::move_to`]'s request: `lparam` holds x (low half) and y.
const MOVE_TO: u32 = WM_APP + 1;

/// GPUI activates a window on every click; we don't, so the focused app keeps the keyboard.
unsafe extern "system" fn no_activate(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        return LRESULT::try_from(MA_NOACTIVATE).unwrap_or_default();
    }
    if msg == WM_SETCURSOR && (lparam & 0xffff) == LPARAM::try_from(HTCLIENT).unwrap_or_default() {
        // SAFETY: GPUI sets its cursor first; `hwnd` is this live window.
        unsafe {
            let handled = DefSubclassProc(hwnd, msg, wparam, lparam);
            if let Some(hand) = grip_cursor(hwnd) {
                SetCursor(hand);
                return 1;
            }
            return handled;
        }
    }
    if msg == MOVE_TO {
        #[allow(clippy::cast_possible_truncation)]
        let (x, y) = (lparam as i32, (lparam >> 32) as i32);
        // SAFETY: `hwnd` is this live window.
        let moved = unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
        if moved == 0 {
            log::warn!("moving the overlay: {}", std::io::Error::last_os_error());
        }
        return 0;
    }
    // SAFETY: the default for every other message.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// DWM's `AccentColor` (title bars, Start), now and on every change.
pub(crate) fn follow_accent(tx: &flume::Sender<Option<Hsla>>, cx: &App) {
    let tx = tx.clone();
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = watch_accent(&tx) {
                log::debug!("no accent colour from Windows: {e:#}");
            }
        })
        .detach();
}

const DWM_KEY: &str = r"Software\Microsoft\Windows\DWM";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "earwig";

/// A value in the per-user `Run` key.
pub(crate) fn autostart() -> bool {
    RegKey::open(RUN_KEY, KEY_READ).is_ok_and(|key| key.has_value(RUN_VALUE))
}

/// Prefers `earwigw.exe` beside us, which opens no console.
pub(crate) fn set_autostart(enabled: bool) -> anyhow::Result<()> {
    let key = RegKey::open(RUN_KEY, KEY_SET_VALUE)?;
    let name = wide(RUN_VALUE);
    let status = if enabled {
        let exe = std::env::current_exe().context("locating earwig")?;
        let quiet = exe.with_file_name("earwigw.exe");
        let command = if quiet.is_file() {
            format!("\"{}\"", quiet.display())
        } else {
            format!("\"{}\" daemon", exe.display())
        };
        let command = wide(&command);
        // SAFETY: a NUL-terminated REG_SZ of the byte length passed.
        unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                u32::try_from(command.len() * 2).unwrap_or(u32::MAX),
            )
        }
    } else {
        // SAFETY: plain call on an open key.
        match unsafe { RegDeleteValueW(key.0, name.as_ptr()) } {
            ERROR_FILE_NOT_FOUND => 0,
            status => status,
        }
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status.cast_signed()))
            .context("changing the sign-in entry");
    }
    Ok(())
}

fn watch_accent(tx: &flume::Sender<Option<Hsla>>) -> anyhow::Result<()> {
    let key = RegKey::open(DWM_KEY, KEY_READ | KEY_NOTIFY)?;
    // SAFETY: an unnamed auto-reset event, owned right away.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
    if event.is_null() {
        return Err(std::io::Error::last_os_error()).context("creating an event");
    }
    // SAFETY: a fresh handle nothing else owns.
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    loop {
        // Arm the notification before reading, so no change slips between.
        // SAFETY: both handles outlive the call; the event is signalled asynchronously.
        let armed = unsafe {
            RegNotifyChangeKeyValue(
                key.0,
                0,
                REG_NOTIFY_CHANGE_LAST_SET,
                event.as_raw_handle(),
                1,
            )
        };
        if armed != 0 {
            bail!(
                "watching the DWM settings: {}",
                std::io::Error::from_raw_os_error(armed.cast_signed())
            );
        }
        if tx.send(key.dword("AccentColor").map(accent)).is_err() {
            return Ok(());
        }
        // SAFETY: waits on the owned event.
        unsafe { WaitForSingleObject(event.as_raw_handle(), INFINITE) };
    }
}

/// `AccentColor` is 0xAABBGGRR; the accent itself is opaque.
fn accent(abgr: u32) -> Hsla {
    let [r, g, b, _] = abgr.to_le_bytes();
    Rgba {
        r: f32::from(r) / 255.,
        g: f32::from(g) / 255.,
        b: f32::from(b) / 255.,
        a: 1.,
    }
    .into()
}

/// An open registry key, closed on drop.
struct RegKey(HKEY);

impl RegKey {
    /// `HKEY_CURRENT_USER\<path>`.
    fn open(path: &str, access: u32) -> anyhow::Result<Self> {
        let wide_path = wide(path);
        let mut key = std::ptr::null_mut();
        // SAFETY: `wide_path` is NUL-terminated; `key` is written on success.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide_path.as_ptr(),
                0,
                access,
                &raw mut key,
            )
        };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status.cast_signed()))
                .with_context(|| format!("opening HKCU\\{path}"));
        }
        Ok(Self(key))
    }

    fn dword(&self, name: &str) -> Option<u32> {
        let name = wide(name);
        let mut value = 0u32;
        let mut size = u32::try_from(size_of::<u32>()).unwrap_or(0);
        // SAFETY: `value` has room for the DWORD asked for.
        let status = unsafe {
            RegGetValueW(
                self.0,
                std::ptr::null(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &raw mut size,
            )
        };
        (status == 0).then_some(value)
    }

    fn has_value(&self, name: &str) -> bool {
        let name = wide(name);
        // SAFETY: asks for no data, only whether the value exists.
        let status = unsafe {
            RegGetValueW(
                self.0,
                std::ptr::null(),
                name.as_ptr(),
                RRF_RT_ANY,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        status == 0
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: opened in `open`.
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hands_are_white_on_black_and_clear_around() {
        for svg in [OPEN_HAND, CLOSED_HAND] {
            let size = 32;
            let pixels = bgra(svg, size);
            assert_eq!(pixels.len(), 32 * 32 * 4);
            let opaque = |want: u8| {
                pixels
                    .chunks(4)
                    .any(|p| p[3] == 255 && p[..3].iter().all(|&c| c == want))
            };
            assert!(opaque(255) && opaque(0));
            assert_eq!(&pixels[..4], &[0; 4]);
        }
    }

    #[test]
    fn the_accent_is_stored_red_first() {
        // Windows' default blue, 0xFFD77800 in the registry.
        let blue = Rgba::from(accent(0xffd7_7800));
        assert!((blue.r - 0.).abs() < 1e-3);
        assert!((blue.g - 120. / 255.).abs() < 1e-3);
        assert!((blue.b - 215. / 255.).abs() < 1e-3);
        assert!((blue.a - 1.).abs() < 1e-3);
    }
}
