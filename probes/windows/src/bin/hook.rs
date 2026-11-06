//! hook [seconds]: print every key edge a WH_KEYBOARD_LL hook sees.
use std::time::Instant;

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static mut START: Option<Instant> = None;

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let edge = match wparam as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => "down",
            _ => "up",
        };
        #[allow(static_mut_refs)]
        let t = unsafe { START.unwrap().elapsed() };
        println!(
            "{:>8.3}s vk={:#04x} scan={:#04x} {edge} flags={:#x} extra={:#x}",
            t.as_secs_f32(),
            info.vkCode,
            info.scanCode,
            info.flags,
            info.dwExtraInfo
        );
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    unsafe {
        START = Some(Instant::now());
        let handle = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), std::ptr::null_mut(), 0);
        assert!(!handle.is_null(), "SetWindowsHookExW failed");
        let thread = GetCurrentThreadId();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            PostThreadMessageW(thread, WM_QUIT, 0, 0);
        });
        println!("hooked for {secs}s");
        let mut msg = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(handle);
    }
}
