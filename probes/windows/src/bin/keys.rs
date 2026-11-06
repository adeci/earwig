//! Drives the interactive desktop for tests:
//!   keys hold <vk> <ms>      press, wait, release (vk: rctrl, lctrl, ralt, space, or a number)
//!   keys tap <vk>
//!   keys combo <vk>... (pressed in order, released in reverse)
//!   keys click <x> <y>       left click at physical screen coordinates
//!   keys move <x> <y>
//!   keys drag <x0> <y0> <x1> <y1>
//!   keys rclick <x> <y>
//!   keys front <title>       bring the window whose title contains <title> to the front
//!   keys fg                  print the foreground window's title
//!   keys find <title>        print the window's rect (physical)
//!   keys sleep <ms>
//! Several commands can be chained with `--`.
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn vk(name: &str) -> u16 {
    match name {
        "rctrl" => VK_RCONTROL,
        "lctrl" | "ctrl" => VK_LCONTROL,
        "ralt" => VK_RMENU,
        "lalt" | "alt" => VK_LMENU,
        "shift" => VK_LSHIFT,
        "lwin" => VK_LWIN,
        "space" => VK_SPACE,
        "esc" => VK_ESCAPE,
        "enter" => VK_RETURN,
        "v" => u16::from(b'V'),
        other if other.len() == 1 => u16::from(other.as_bytes()[0].to_ascii_uppercase()),
        other => other.parse().expect("vk"),
    }
}

fn key(vk: u16, up: bool) {
    let extended = matches!(vk, VK_RCONTROL | VK_RMENU);
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    assert_eq!(sent, 1, "SendInput");
}

fn mouse(flags: u32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
}

fn move_to(x: i32, y: i32) {
    unsafe { SetCursorPos(x, y) };
    sleep(30);
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n as usize])
}

fn find(query: &str) -> Option<HWND> {
    struct Search<'a>(&'a str, Option<HWND>);
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> i32 {
        let search = unsafe { &mut *(data as *mut Search) };
        if unsafe { IsWindowVisible(hwnd) } != 0 && title(hwnd).contains(search.0) {
            search.1 = Some(hwnd);
            return 0;
        }
        1
    }
    let mut search = Search(query, None);
    unsafe { EnumWindows(Some(each), &mut search as *mut Search as LPARAM) };
    search.1
}

fn main() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    for command in args.split(|a| a == "--") {
        let int = |i: usize| command[i].parse::<i32>().expect("number");
        match command[0].as_str() {
            "hold" => {
                let vk = vk(&command[1]);
                key(vk, false);
                sleep(int(2) as u64);
                key(vk, true);
            }
            "tap" => {
                let vk = vk(&command[1]);
                key(vk, false);
                sleep(40);
                key(vk, true);
            }
            "combo" => {
                let keys: Vec<u16> = command[1..].iter().map(|k| vk(k)).collect();
                for &k in &keys {
                    key(k, false);
                    sleep(20);
                }
                for &k in keys.iter().rev() {
                    key(k, true);
                    sleep(20);
                }
            }
            "move" => move_to(int(1), int(2)),
            "click" | "rclick" => {
                let (down, up) = if command[0] == "click" {
                    (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
                } else {
                    (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
                };
                move_to(int(1), int(2));
                mouse(down);
                sleep(60);
                mouse(up);
            }
            "drag" => {
                move_to(int(1), int(2));
                mouse(MOUSEEVENTF_LEFTDOWN);
                sleep(100);
                let (x0, y0, x1, y1) = (int(1), int(2), int(3), int(4));
                for step in 1..=20 {
                    move_to(x0 + (x1 - x0) * step / 20, y0 + (y1 - y0) * step / 20);
                    mouse(MOUSEEVENTF_MOVE);
                }
                sleep(100);
                mouse(MOUSEEVENTF_LEFTUP);
            }
            "front" => {
                let hwnd = find(&command[1]).expect("no such window");
                unsafe {
                    // Alt lets a process that is not in front take the foreground.
                    key(VK_LMENU, false);
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                    key(VK_LMENU, true);
                }
                sleep(200);
            }
            "fg" => println!("foreground: {}", title(unsafe { GetForegroundWindow() })),
            "find" => {
                let hwnd = find(&command[1]).expect("no such window");
                let mut rect = RECT::default();
                unsafe { GetWindowRect(hwnd, &mut rect) };
                println!("{} {} {} {}", rect.left, rect.top, rect.right, rect.bottom);
            }
            "sleep" => sleep(int(1) as u64),
            "close" => {
                let hwnd = find(&command[1]).expect("no such window");
                unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            }
            "list" => {
                unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> i32 {
                    if unsafe { IsWindowVisible(hwnd) } != 0 {
                        let mut rect: RECT = unsafe { std::mem::zeroed() };
                        unsafe { GetWindowRect(hwnd, &mut rect) };
                        let mut pid = 0;
                        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
                        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
                        println!(
                            "{hwnd:?} pid={pid} ex={ex:#x} [{} {} {} {}] {:?}",
                            rect.left,
                            rect.top,
                            rect.right,
                            rect.bottom,
                            title(hwnd)
                        );
                    }
                    1
                }
                unsafe { EnumWindows(Some(each), 0) };
            }
            other => panic!("unknown command {other}"),
        }
    }
}
