//! paste <text> [shift]: put text on the clipboard and send Ctrl+V (Ctrl+Shift+V
//! with `shift`) to whatever has focus; then read the clipboard back.
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CF_UNICODETEXT: u32 = 13;

fn set_clipboard(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        assert!(OpenClipboard(std::ptr::null_mut()) != 0, "OpenClipboard");
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        let ptr = GlobalLock(mem).cast::<u16>();
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        GlobalUnlock(mem);
        if SetClipboardData(CF_UNICODETEXT, mem as HANDLE).is_null() {
            GlobalFree(mem);
            panic!("SetClipboardData");
        }
        CloseClipboard();
    }
}

fn get_clipboard() -> String {
    unsafe {
        assert!(OpenClipboard(std::ptr::null_mut()) != 0, "OpenClipboard");
        let data = GetClipboardData(CF_UNICODETEXT);
        let text = if data.is_null() {
            String::new()
        } else {
            let ptr = GlobalLock(data as _).cast::<u16>();
            let len = (0..).take_while(|&i| *ptr.add(i) != 0).count();
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            GlobalUnlock(data as _);
            text
        };
        CloseClipboard();
        text
    }
}

fn input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0xea51,
            },
        },
    }
}

fn main() {
    let text = std::env::args().nth(1).expect("text");
    let shift = std::env::args().nth(2).is_some();
    set_clipboard(&text);
    let mut keys = vec![input(VK_CONTROL, false)];
    if shift {
        keys.push(input(VK_SHIFT, false));
    }
    keys.push(input(u16::from(b'V'), false));
    keys.push(input(u16::from(b'V'), true));
    if shift {
        keys.push(input(VK_SHIFT, true));
    }
    keys.push(input(VK_CONTROL, true));
    let fg = unsafe { GetForegroundWindow() };
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(fg, buf.as_mut_ptr(), 256) };
    println!("foreground: {}", String::from_utf16_lossy(&buf[..n as usize]));
    let sent = unsafe { SendInput(keys.len() as u32, keys.as_ptr(), size_of::<INPUT>() as i32) };
    println!("sent {sent}/{}", keys.len());
    std::thread::sleep(std::time::Duration::from_millis(300));
    println!("clipboard now: {:?}", get_clipboard());
}
