//! Windows: the text goes on the clipboard, where it stays, and one Ctrl+V
//! (`SendInput`) pastes it into the focused window. Windows Terminal and the
//! console host paste with Ctrl+V too; the terminals in [`TERMINALS`] and
//! `[insert] terminals` get Ctrl+Shift+V.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, bail};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GlobalFree, HANDLE, HWND};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_CONTROL, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::{InsertError, Inserter};
use crate::config::{InsertConfig, InsertSetting};
use crate::desktop::DesktopCaps;
use crate::events::InsertMethod;
use crate::hotkey::INJECTED;
use crate::win32::{is, owned, scan_code};

/// Executables (without `.exe`, compared ASCII case-insensitively) that
/// paste with Ctrl+Shift+V. `[insert] terminals` adds to this list.
const TERMINALS: &[&str] = &["alacritty", "rio", "wezterm-gui"];

/// How long to keep trying while another app has the clipboard open.
const CLIPBOARD_WAIT: Duration = Duration::from_millis(500);

pub(super) fn backends(
    config: &InsertConfig,
    _caps: &DesktopCaps,
) -> anyhow::Result<Vec<Box<dyn Inserter>>> {
    match config.method {
        InsertSetting::Auto | InsertSetting::Paste => Ok(vec![Box::new(Paste {
            terminals: config.terminals.clone(),
        })]),
        InsertSetting::InputMethod | InsertSetting::Portal => {
            bail!("insert method {:?} is Linux only; use paste", config.method)
        }
    }
}

struct Paste {
    terminals: Vec<String>,
}

impl Inserter for Paste {
    fn methods(&self) -> Vec<InsertMethod> {
        vec![InsertMethod::Paste]
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        // SAFETY: no preconditions.
        let window = unsafe { GetForegroundWindow() };
        if window.is_null() {
            return Err(InsertError::NotApplicable("no window has focus".into()));
        }
        let process = Process::of(window);
        if process.as_ref().is_some_and(Process::is_elevated) && !is_elevated(current()) {
            set_clipboard(text)?;
            return Err(InsertError::NotApplicable(
                "the focused app runs as administrator, which Windows keeps earwig from typing into"
                    .into(),
            ));
        }
        let terminal = process
            .as_ref()
            .and_then(|process| process.name.as_deref())
            .is_some_and(|name| is_terminal(name, &self.terminals));
        set_clipboard(text)?;
        log::debug!(
            "pasting into {:?} with {}",
            process.and_then(|process| process.name),
            if terminal { "Ctrl+Shift+V" } else { "Ctrl+V" }
        );
        paste(terminal)?;
        Ok(InsertMethod::Paste)
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        Ok(set_clipboard(text)?)
    }
}

fn is_terminal(name: &str, extra: &[String]) -> bool {
    TERMINALS
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .any(|terminal| terminal.eq_ignore_ascii_case(name))
}

/// Replace the clipboard's contents with `text`.
fn set_clipboard(text: &str) -> anyhow::Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let clipboard = Clipboard::open()?;
    // SAFETY: the clipboard is open; the memory handed to SetClipboardData
    // belongs to the system once it accepts it, and is freed here if not.
    unsafe {
        if EmptyClipboard() == 0 {
            return Err(std::io::Error::last_os_error()).context("emptying the clipboard");
        }
        let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if memory.is_null() {
            return Err(std::io::Error::last_os_error()).context("allocating clipboard memory");
        }
        let target = GlobalLock(memory).cast::<u16>();
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
        GlobalUnlock(memory);
        if SetClipboardData(u32::from(CF_UNICODETEXT), memory as HANDLE).is_null() {
            let err = std::io::Error::last_os_error();
            GlobalFree(memory);
            return Err(err).context("setting the clipboard");
        }
    }
    drop(clipboard);
    Ok(())
}

/// The open clipboard, closed on drop.
struct Clipboard;

impl Clipboard {
    fn open() -> anyhow::Result<Self> {
        let deadline = std::time::Instant::now() + CLIPBOARD_WAIT;
        loop {
            // SAFETY: no owner window; any thread may open the clipboard.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Self);
            }
            let err = std::io::Error::last_os_error();
            if std::time::Instant::now() >= deadline {
                return Err(err).context("another app keeps the clipboard open");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: opened in `open`.
        unsafe { CloseClipboard() };
    }
}

/// Press and release Ctrl+V, or Ctrl+Shift+V.
fn paste(shift: bool) -> anyhow::Result<()> {
    let mut keys = vec![(VK_CONTROL, false)];
    if shift {
        keys.push((VK_SHIFT, false));
    }
    keys.extend([(u16::from(b'V'), false), (u16::from(b'V'), true)]);
    if shift {
        keys.push((VK_SHIFT, true));
    }
    keys.push((VK_CONTROL, true));
    let inputs: Vec<INPUT> = keys
        .into_iter()
        .map(|(vk, up)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    // Apps that read scan codes (the edit control does)
                    // ignore keys without one.
                    wScan: scan_code(vk),
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: INJECTED,
                },
            },
        })
        .collect();
    let size = i32::try_from(size_of::<INPUT>()).unwrap_or(i32::MAX);
    let count = u32::try_from(inputs.len()).unwrap_or(0);
    // SAFETY: `inputs` holds `count` initialised INPUTs.
    let sent = unsafe { SendInput(count, inputs.as_ptr(), size) };
    if sent != count {
        return Err(std::io::Error::last_os_error()).context("sending the paste keys");
    }
    Ok(())
}

/// The process that owns a window.
struct Process {
    handle: std::os::windows::io::OwnedHandle,
    /// The executable's name without `.exe`.
    name: Option<String>,
}

impl Process {
    fn of(window: HWND) -> Option<Self> {
        let mut pid = 0;
        // SAFETY: `window` is a window handle; `pid` is written.
        unsafe { GetWindowThreadProcessId(window, &raw mut pid) };
        // SAFETY: plain call; the handle is owned right away.
        let handle = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
            .inspect_err(|e| log::debug!("opening the focused process: {e}"))
            .ok()?;
        let mut path = vec![0u16; 1024];
        let mut len = u32::try_from(path.len()).unwrap_or(0);
        // SAFETY: `path` holds `len` u16s; `len` is updated to the length.
        let named = unsafe {
            QueryFullProcessImageNameW(
                handle.as_raw_handle(),
                PROCESS_NAME_WIN32,
                path.as_mut_ptr(),
                &raw mut len,
            )
        } != 0;
        let name = named.then(|| {
            path.truncate(len as usize);
            let path = OsString::from_wide(&path);
            Path::new(&path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        });
        Some(Self {
            handle,
            name: name.flatten(),
        })
    }

    fn is_elevated(&self) -> bool {
        is_elevated(self.handle.as_raw_handle())
    }
}

fn current() -> HANDLE {
    // SAFETY: a pseudo handle, valid for the process's lifetime.
    unsafe { GetCurrentProcess() }
}

/// Whether `process` runs elevated. A token earwig may not even query
/// belongs to a process above it.
fn is_elevated(process: HANDLE) -> bool {
    let mut token = std::ptr::null_mut();
    // SAFETY: `token` is written on success.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return is(&std::io::Error::last_os_error(), ERROR_ACCESS_DENIED);
    }
    let Ok(token) = owned(token) else {
        return false;
    };
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0;
    // SAFETY: `elevation` has the size passed.
    let queried = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut elevation).cast(),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
            &raw mut len,
        )
    } != 0;
    queried && elevation.TokenIsElevated != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::DataExchange::GetClipboardData;
    use windows_sys::Win32::System::Memory::GlobalSize;

    fn clipboard_text() -> String {
        let _open = Clipboard::open().unwrap();
        // SAFETY: the clipboard is open; the data is read while locked,
        // within the size of its memory.
        unsafe {
            let data = GetClipboardData(u32::from(CF_UNICODETEXT));
            assert!(!data.is_null(), "no text on the clipboard");
            let units =
                std::slice::from_raw_parts(GlobalLock(data).cast::<u16>(), GlobalSize(data) / 2);
            let len = units
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(units.len());
            let text = String::from_utf16_lossy(&units[..len]);
            GlobalUnlock(data);
            text
        }
    }

    #[test]
    fn copied_text_stays_on_the_clipboard() {
        let mut paste = Paste {
            terminals: Vec::new(),
        };
        let text = "Grüße, 世界 👋 — earwig";
        paste.copy_to_clipboard(text).unwrap();
        assert_eq!(clipboard_text(), text);
    }

    #[test]
    fn terminals_match_case_insensitively_and_include_configured_ones() {
        let extra = ["MyTerm".to_owned()];
        assert!(is_terminal("Alacritty", &[]));
        assert!(is_terminal("wezterm-gui", &[]));
        assert!(!is_terminal("WindowsTerminal", &[]), "pastes with Ctrl+V");
        assert!(is_terminal("myterm", &extra));
    }
}
