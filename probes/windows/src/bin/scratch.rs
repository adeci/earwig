//! scratch [out.txt]: a window titled "earwig scratch" holding one multi-line
//! edit control; every change of its text is written to out.txt (UTF-8).
#![windows_subsystem = "windows"]

use std::ptr::null;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontW, FW_NORMAL};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static OUT: OnceLock<String> = OnceLock::new();
static mut EDIT: HWND = std::ptr::null_mut();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                let (w, h) = ((lparam & 0xffff) as i32, ((lparam >> 16) & 0xffff) as i32);
                MoveWindow(EDIT, 0, 0, w, h, 1);
                0
            }
            WM_SETFOCUS => {
                SetFocus(EDIT);
                0
            }
            WM_COMMAND if (wparam >> 16) as u32 == EN_CHANGE => {
                let len = GetWindowTextLengthW(EDIT);
                let mut buf = vec![0u16; len as usize + 1];
                let n = GetWindowTextW(EDIT, buf.as_mut_ptr(), buf.len() as i32);
                let text = String::from_utf16_lossy(&buf[..n as usize]);
                let _ = std::fs::write(OUT.get().unwrap(), text);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn main() {
    OUT.set(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| r"C:\earwig-build\scratch.txt".into()),
    )
    .unwrap();
    let _ = std::fs::write(OUT.get().unwrap(), "");
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = wide("EarwigScratch");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (5 + 1) as _,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let title = wide("earwig scratch");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            200,
            200,
            900,
            500,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            null(),
        );
        let edit_class = wide("EDIT");
        EDIT = CreateWindowExW(
            0,
            edit_class.as_ptr(),
            null(),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
            0,
            0,
            900,
            500,
            hwnd,
            std::ptr::null_mut(),
            instance,
            null(),
        );
        let face = wide("Segoe UI");
        let font = CreateFontW(-28, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, 0, 0, 0, 0, 0, face.as_ptr());
        SendMessageW(EDIT, WM_SETFONT, font as usize, 1);
        SetForegroundWindow(hwnd);
        SetFocus(EDIT);
        let mut msg = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
