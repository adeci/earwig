//! The C and Objective-C calls earwig makes on macOS: synthetic keys, privacy
//! checks, the code signature and the pasteboard.

use std::ffi::c_void;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSBundle, NSDictionary, NSNumber, NSString};

type CGEventRef = *mut c_void;

/// `kCGHIDEventTap`
const TAP_HID: u32 = 0;
/// `kCGEventSourceUserData`, where the keys earwig presses carry
/// [`earwig_keys::SYNTHETIC`] so its own hotkey tap passes them by.
const FIELD_SOURCE_USER_DATA: u32 = 42;
/// `kCGEventFlagMaskCommand`
const FLAG_COMMAND: u64 = 0x0010_0000;
/// `kVK_ANSI_V`
const KEYCODE_V: u16 = 9;
/// `kCGEventSourceStateHIDSystemState`
const SOURCE_HID: i32 = 1;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state: i32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, down: bool) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    safe fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: *const c_void;
    safe fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecCodeInfoUnique: *const c_void;
    fn SecCodeCopySelf(flags: u32, code: *mut *const c_void) -> i32;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        information: *mut *const c_void,
    ) -> i32;
}

// `AVAudioApplication` and `AVCaptureDevice`, looked up by name.
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

// `NSPasteboard`, looked up by name.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// `AVAudioApplicationRecordPermission`: 'undt' and 'grnt' (and 'deny').
const RECORD_UNDETERMINED: isize = 0x756e_6474;
const RECORD_GRANTED: isize = 0x6772_6e74;

/// AVAudioApplication (macOS 14+) reads the mic answer live and asks again
/// once it is cleared; AVCaptureDevice (all macOS 13 has) caches the first.
fn audio_application() -> Option<&'static AnyClass> {
    AnyClass::get(c"AVAudioApplication")
}

/// Some answer, or "not asked yet" (`None`).
pub fn microphone_allowed() -> Option<bool> {
    autoreleasepool(|_| {
        if let Some(class) = audio_application() {
            let permission: isize = unsafe {
                let app: *mut AnyObject = msg_send![class, sharedInstance];
                msg_send![app, recordPermission]
            };
            return match permission {
                RECORD_UNDETERMINED => None,
                RECORD_GRANTED => Some(true),
                _ => Some(false),
            };
        }
        // AVAuthorizationStatus: not determined, restricted, denied, authorized.
        let audio = NSString::from_str("soun");
        let status: isize =
            unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*audio] };
        match status {
            0 => None,
            3 => Some(true),
            _ => Some(false),
        }
    })
}

/// Show the system's microphone prompt, if it was never answered.
pub fn request_microphone() {
    autoreleasepool(|_| {
        let done = block2::RcBlock::new(|_granted: Bool| {});
        if let Some(class) = audio_application() {
            unsafe {
                let () = msg_send![class, requestRecordPermissionWithCompletionHandler: &*done];
            }
        } else {
            let audio = NSString::from_str("soun");
            unsafe {
                let () = msg_send![
                    class!(AVCaptureDevice),
                    requestAccessForMediaType: &*audio,
                    completionHandler: &*done
                ];
            }
        }
    });
}

/// Accessibility: needed to see the hotkey and to press Cmd+V in other
/// apps.
pub fn accessibility_allowed() -> bool {
    AXIsProcessTrusted() || CGPreflightPostEventAccess()
}

/// Show the system's Accessibility prompt, which also lists earwig in System
/// Settings, switched off until the user switches it on.
pub fn request_accessibility() {
    autoreleasepool(|_| {
        // SAFETY: a CFString constant, which is an NSString.
        let prompt = unsafe { &*kAXTrustedCheckOptionPrompt.cast::<NSString>() };
        let options = NSDictionary::from_slices(&[prompt], &[&*NSNumber::new_bool(true)]);
        // SAFETY: a dictionary of the options it takes.
        unsafe { AXIsProcessTrustedWithOptions(Retained::as_ptr(&options).cast()) };
    });
}

/// The hash of this build's code signature (its cdhash), which macOS keeps
/// an ad-hoc signed app's privacy grants for; `None` for unsigned code.
pub fn code_hash() -> Option<Vec<u8>> {
    let mut code = std::ptr::null();
    // SAFETY: an out pointer for the running code, released below.
    if unsafe { SecCodeCopySelf(0, &raw mut code) } != 0 || code.is_null() {
        return None;
    }
    let mut info = std::ptr::null();
    // SAFETY: `code` is alive; the dictionary is released below.
    let status = unsafe { SecCodeCopySigningInformation(code, 0, &raw mut info) };
    // SAFETY: copied above.
    unsafe { CFRelease(code) };
    if status != 0 || info.is_null() {
        return None;
    }
    // SAFETY: the dictionary, and with it its value for kSecCodeInfoUnique
    // (a CFData), lives until released below.
    let hash = unsafe {
        let unique = CFDictionaryGetValue(info, kSecCodeInfoUnique);
        let len = if unique.is_null() {
            0
        } else {
            CFDataGetLength(unique)
        };
        match usize::try_from(len) {
            Ok(len) if len > 0 => {
                Some(std::slice::from_raw_parts(CFDataGetBytePtr(unique), len).to_vec())
            }
            _ => None,
        }
    };
    // SAFETY: copied above.
    unsafe { CFRelease(info) };
    hash
}

/// The identifier of the app bundle this program runs from, if any.
pub fn bundle_id() -> Option<String> {
    autoreleasepool(|_| {
        NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|id| id.to_string())
    })
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
            CGEventSetIntegerValueField(event, FIELD_SOURCE_USER_DATA, earwig_keys::SYNTHETIC);
            CGEventPost(TAP_HID, event);
            CFRelease(event.cast_const());
        }
        if !source.is_null() {
            CFRelease(source.cast_const());
        }
    }
}
