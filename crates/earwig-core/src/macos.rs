//! The C and Objective-C entry points earwig needs on macOS: event taps and
//! synthetic keys (CoreGraphics), run loops (CoreFoundation), privacy
//! checks (ApplicationServices, AVFoundation) and the pasteboard.

use std::ffi::c_void;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::NSString;

pub type CGEventRef = *mut c_void;
pub type CFMachPortRef = *mut c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CFStringRef = *const c_void;
pub type CGEventTapCallBack =
    unsafe extern "C" fn(*mut c_void, u32, CGEventRef, *mut c_void) -> CGEventRef;

/// `kCGHIDEventTap`
pub const TAP_HID: u32 = 0;
/// `kCGSessionEventTap`
pub const TAP_SESSION: u32 = 1;
/// `kCGHeadInsertEventTap`
pub const HEAD_INSERT: u32 = 0;
/// `kCGEventTapOptionDefault`: may swallow events.
pub const TAP_ACTIVE: u32 = 0;
/// `kCGEventTapOptionListenOnly`
pub const TAP_LISTEN_ONLY: u32 = 1;

pub const KEY_DOWN: u32 = 10;
pub const KEY_UP: u32 = 11;
pub const FLAGS_CHANGED: u32 = 12;
pub const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
pub const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
/// `kCGKeyboardEventAutorepeat`
pub const FIELD_AUTOREPEAT: u32 = 8;
/// `kCGKeyboardEventKeycode`
pub const FIELD_KEYCODE: u32 = 9;
/// `kCGEventSourceUserData`
pub const FIELD_SOURCE_USER_DATA: u32 = 42;
/// What earwig writes into [`FIELD_SOURCE_USER_DATA`] of the keys it
/// presses, so its own hotkey tap can tell them from the keyboard's.
pub const EARWIG_EVENT: i64 = 0x6561_7277;

/// `kCGEventFlagMaskCommand`
pub const FLAG_COMMAND: u64 = 0x0010_0000;
/// `kVK_ANSI_V`
const KEYCODE_V: u16 = 9;
/// `kCGEventSourceStateHIDSystemState`
const SOURCE_HID: i32 = 1;

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
    fn CGEventSourceCreate(state: i32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, down: bool) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    safe fn CGPreflightPostEventAccess() -> bool;
    safe fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    pub fn CFMachPortInvalidate(port: CFMachPortRef);
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRun();
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRelease(cf: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    safe fn AXIsProcessTrusted() -> bool;
}

// `AVCaptureDevice`, looked up by name.
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

// `NSPasteboard`, looked up by name.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// Some answer, or "not asked yet" (`None`).
pub fn microphone_allowed() -> Option<bool> {
    // AVAuthorizationStatus: not determined, restricted, denied, authorized.
    let status: isize = autoreleasepool(|_| {
        let audio = NSString::from_str("soun");
        unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*audio] }
    });
    match status {
        0 => None,
        3 => Some(true),
        _ => Some(false),
    }
}

/// Show the system's microphone prompt, if it was never answered.
pub fn request_microphone() {
    autoreleasepool(|_| {
        let audio = NSString::from_str("soun");
        let done = block2::RcBlock::new(|_granted: Bool| {});
        unsafe {
            let () = msg_send![
                class!(AVCaptureDevice),
                requestAccessForMediaType: &*audio,
                completionHandler: &*done
            ];
        }
    });
}

/// Accessibility: needed to see the hotkey and to press Cmd+V in other
/// apps.
pub fn accessibility_allowed() -> bool {
    AXIsProcessTrusted() || CGPreflightPostEventAccess()
}

/// Show the system's Accessibility prompt.
pub fn request_accessibility() {
    CGRequestPostEventAccess();
}

/// Replace the general pasteboard's contents with `text`.
pub fn set_clipboard(text: &str) -> anyhow::Result<()> {
    let written = autoreleasepool(|_| unsafe {
        let board: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        anyhow::ensure!(!board.is_null(), "no general pasteboard");
        let _: isize = msg_send![board, clearContents];
        let text: Retained<NSString> = NSString::from_str(text);
        let kind = NSString::from_str("public.utf8-plain-text");
        let written: Bool = msg_send![board, setString: &*text, forType: &*kind];
        Ok(written.as_bool())
    })?;
    anyhow::ensure!(written, "the pasteboard refused the text");
    Ok(())
}

/// Press and release Cmd+V in the focused app.
pub fn press_paste() {
    unsafe {
        let source = CGEventSourceCreate(SOURCE_HID);
        for down in [true, false] {
            let event = CGEventCreateKeyboardEvent(source, KEYCODE_V, down);
            if event.is_null() {
                continue;
            }
            CGEventSetFlags(event, FLAG_COMMAND);
            CGEventSetIntegerValueField(event, FIELD_SOURCE_USER_DATA, EARWIG_EVENT);
            CGEventPost(TAP_HID, event);
            CFRelease(event.cast_const());
        }
        if !source.is_null() {
            CFRelease(source.cast_const());
        }
    }
}
