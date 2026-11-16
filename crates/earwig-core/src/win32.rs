//! Small helpers for the Win32 calls.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{FromRawHandle as _, OwnedHandle};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};

/// Take ownership of a handle a Win32 call returned, or of its error.
pub(crate) fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// `s` as a NUL-terminated UTF-16 string.
pub(crate) fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// Whether `err` is the Win32 error `code`.
pub(crate) fn is(err: &io::Error, code: u32) -> bool {
    err.raw_os_error()
        .is_some_and(|raw| u32::try_from(raw).is_ok_and(|raw| raw == code))
}

/// The scan code of virtual key `vk` on the current keyboard layout.
pub(crate) fn scan_code(vk: u16) -> u16 {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};
    // SAFETY: no preconditions.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) };
    u16::try_from(scan).unwrap_or(0)
}

/// The string `name` under `HKEY_CURRENT_USER\<key>`.
pub(crate) fn user_string(key: &str, name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::RRF_RT_REG_SZ;
    let mut size = 0;
    if user_value(
        key,
        name,
        RRF_RT_REG_SZ,
        std::ptr::null_mut(),
        &raw mut size,
    ) != 0
    {
        return None;
    }
    let mut text = vec![0u16; usize::try_from(size).ok()?.div_ceil(2)];
    if user_value(
        key,
        name,
        RRF_RT_REG_SZ,
        text.as_mut_ptr().cast(),
        &raw mut size,
    ) != 0
    {
        return None;
    }
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    Some(String::from_utf16_lossy(&text[..end]))
}

/// `RegGetValueW` under `HKEY_CURRENT_USER`: `data` (`size` bytes) or null.
fn user_value(
    key: &str,
    name: &str,
    types: u32,
    data: *mut std::ffi::c_void,
    size: *mut u32,
) -> u32 {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RegGetValueW};
    let (key, name) = (wide(OsStr::new(key)), wide(OsStr::new(name)));
    // SAFETY: NUL-terminated names; `data` holds `*size` bytes, or both are
    // null (the size or the value's existence only).
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            types,
            std::ptr::null_mut(),
            data,
            size,
        )
    }
}
