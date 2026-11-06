//! Small helpers for the Win32 calls of [`crate::ipc`] and [`crate::hooks`].

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
