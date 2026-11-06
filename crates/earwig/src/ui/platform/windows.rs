//! Windows: the overlay is GPUI's topmost tool-window popup, made
//! non-activating so clicking or dragging the pill never takes focus from
//! the app the text is meant for; a notification-area icon (`tray-icon`);
//! the accent colour from the DWM settings in the registry.

use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use anyhow::{Context as _, bail};
use earwig_core::desktop::DesktopCaps;
use gpui_kit::{
    App, Application, Bounds, DisplayId, Hsla, Pixels, PlatformDisplay, Point, Rgba, Window, point,
    px,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateRectRgn, MONITOR_DEFAULTTOPRIMARY, MonitorFromWindow, SetWindowRgn,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, KEY_SET_VALUE, REG_NOTIFY_CHANGE_LAST_SET,
    REG_SZ, RRF_RT_ANY, RRF_RT_REG_DWORD, RegCloseKey, RegDeleteValueW, RegGetValueW,
    RegNotifyChangeKeyValue, RegOpenKeyExW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WM_MOUSEACTIVATE,
    WS_EX_NOACTIVATE,
};

use crate::ui::OverlaySurface;

pub(crate) use super::tray_icon::{TrayHandle, spawn as spawn_tray};

pub(crate) fn overlay_surface(_caps: &DesktopCaps) -> OverlaySurface {
    OverlaySurface::Popup
}

pub(crate) fn application(_surface: OverlaySurface, _caps: &DesktopCaps) -> Application {
    gpui_kit::application()
}

/// The display the overlay opens on: the one with the focused window, where
/// the user is looking.
pub(crate) fn home_display(cx: &App) -> Option<std::rc::Rc<dyn PlatformDisplay>> {
    // SAFETY: plain calls; a null window maps to the primary monitor.
    let monitor = unsafe { MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY) };
    // GPUI's display ids on Windows are monitor handles.
    let id = DisplayId::new(monitor as u64);
    cx.find_display(id).or_else(|| cx.primary_display())
}

/// The overlay's popup window.
pub(crate) struct Popup {
    hwnd: HWND,
    /// Origin last asked for, `None` while the window is where it opened.
    origin: Option<Point<Pixels>>,
}

impl Popup {
    /// Make the popup GPUI opened (topmost, no taskbar entry) never take
    /// activation, and clip it to nothing until the pill is painted.
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

    /// Put the window's top-left corner at `origin` (logical pixels).
    pub(crate) fn move_to(&mut self, origin: Point<Pixels>, scale: f32) {
        if self.origin == Some(origin) {
            return;
        }
        self.origin = Some(origin);
        #[allow(clippy::cast_possible_truncation)]
        let device = |value: Pixels| (f32::from(value) * scale).round() as i32;
        // SAFETY: the window is alive while the overlay holds its popup.
        let moved = unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                device(origin.x),
                device(origin.y),
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
        if moved == 0 {
            log::warn!("moving the overlay: {}", std::io::Error::last_os_error());
        }
    }
}

/// Clicks reach the window only inside `area` (window coordinates, logical
/// pixels), and everywhere else whatever is below. `None` is nothing. The
/// region clips drawing as well, so `area` includes what is painted around
/// the pill.
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

/// GPUI activates a window on every click (`MA_ACTIVATE`); the overlay asks
/// Windows not to, so the focused app keeps the keyboard.
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
    // SAFETY: the default for every other message.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// The accent colour Windows draws title bars and Start with, and every
/// change of it (`HKCU\Software\Microsoft\Windows\DWM`, `AccentColor`).
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

/// Whether earwig starts when the user signs in (a value in the per-user
/// `Run` key).
pub(crate) fn autostart() -> bool {
    RegKey::open(RUN_KEY, KEY_READ).is_ok_and(|key| key.has_value(RUN_VALUE))
}

/// Start earwig at sign-in, through `earwigw.exe` when it sits next to this
/// program so no console window opens, or stop doing so.
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
        // SAFETY: both handles live for the call; the event is signalled
        // asynchronously.
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
    fn the_accent_is_stored_red_first() {
        // Windows' default blue, 0xFFD77800 in the registry.
        let blue = Rgba::from(accent(0xffd7_7800));
        assert!((blue.r - 0.).abs() < 1e-3);
        assert!((blue.g - 120. / 255.).abs() < 1e-3);
        assert!((blue.b - 215. / 255.).abs() < 1e-3);
        assert!((blue.a - 1.).abs() < 1e-3);
    }
}
