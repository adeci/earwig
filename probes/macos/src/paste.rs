//! Put text on the general pasteboard and press Cmd+V in the focused app.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::NSString;

use crate::ffi;

pub fn set_clipboard(text: &str) -> bool {
    unsafe {
        let board: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        let _: isize = msg_send![board, clearContents];
        let text: Retained<NSString> = NSString::from_str(text);
        let kind: Retained<NSString> = NSString::from_str("public.utf8-plain-text");
        let ok: Bool = msg_send![board, setString: &*text, forType: &*kind];
        ok.as_bool()
    }
}

pub fn clipboard() -> Option<String> {
    unsafe {
        let board: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        let kind: Retained<NSString> = NSString::from_str("public.utf8-plain-text");
        let text: Option<Retained<NSString>> = msg_send![board, stringForType: &*kind];
        text.map(|t| t.to_string())
    }
}

pub fn press_cmd_v() {
    unsafe {
        // kCGEventSourceStateHIDSystemState
        let source = ffi::CGEventSourceCreate(1);
        for down in [true, false] {
            let event = ffi::CGEventCreateKeyboardEvent(source, ffi::KEYCODE_V, down);
            ffi::CGEventSetFlags(event, ffi::FLAG_COMMAND);
            ffi::CGEventPost(ffi::TAP_HID, event);
            ffi::CFRelease(event.cast_const());
        }
        if !source.is_null() {
            ffi::CFRelease(source.cast_const());
        }
    }
}

pub fn paste(text: &str) {
    let set = set_clipboard(text);
    press_cmd_v();
    crate::log(&format!(
        "paste: pasteboard set={set}, Cmd+V posted (post-events allowed={}), pasteboard now holds the text: {}",
        ffi::CGPreflightPostEventAccess(),
        clipboard().as_deref() == Some(text)
    ));
}
