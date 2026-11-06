//! The few C entry points the probes need from CoreGraphics, CoreFoundation,
//! ApplicationServices and IOKit.

use std::ffi::c_void;

pub type CGEventRef = *mut c_void;
pub type CFMachPortRef = *mut c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CFStringRef = *const c_void;
pub type CGEventTapCallBack =
    unsafe extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef;

pub const TAP_SESSION: u32 = 1;
pub const TAP_HID: u32 = 0;
pub const HEAD_INSERT: u32 = 0;
pub const LISTEN_ONLY: u32 = 1;

pub const KEY_DOWN: u32 = 10;
pub const KEY_UP: u32 = 11;
pub const FLAGS_CHANGED: u32 = 12;
pub const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
pub const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
pub const FIELD_KEYCODE: u32 = 9;
pub const FIELD_AUTOREPEAT: u32 = 8;

pub const FLAG_COMMAND: u64 = 0x0010_0000;
/// `NX_DEVICERCMDKEYMASK`: the right Command key is down.
pub const FLAG_RIGHT_COMMAND: u64 = 0x10;

pub const KEYCODE_RIGHT_COMMAND: i64 = 0x36;
pub const KEYCODE_V: u16 = 9;

pub const LISTEN_EVENT: u32 = 1;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventSourceCreate(state: i32) -> *mut c_void;
    pub fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, down: bool) -> CGEventRef;
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventPost(tap: u32, event: CGEventRef);
    pub safe fn CGPreflightListenEventAccess() -> bool;
    pub safe fn CGRequestListenEventAccess() -> bool;
    pub safe fn CGPreflightPostEventAccess() -> bool;
    pub safe fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRun();
    pub fn CFRelease(cf: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub safe fn AXIsProcessTrusted() -> bool;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    pub safe fn IOHIDCheckAccess(request: u32) -> u32;
    pub safe fn IOHIDRequestAccess(request: u32) -> bool;
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
