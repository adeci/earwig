//! The C and Objective-C calls earwig makes on macOS: synthetic keys, privacy
//! checks, the code signature and the pasteboard.

use std::ffi::c_void;
use std::path::Path;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool};
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
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        buffer: *const u8,
        length: isize,
        is_directory: u8,
    ) -> *const c_void;
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
    static kSecCodeInfoTeamIdentifier: *const c_void;
    static kSecCodeInfoPList: *const c_void;
    fn SecCodeCopySelf(flags: u32, code: *mut *const c_void) -> i32;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: u32,
        information: *mut *const c_void,
    ) -> i32;
    fn SecCodeCopyDesignatedRequirement(
        code: *const c_void,
        flags: u32,
        requirement: *mut *const c_void,
    ) -> i32;
    fn SecStaticCodeCreateWithPath(
        path: *const c_void,
        flags: u32,
        code: *mut *const c_void,
    ) -> i32;
    fn SecStaticCodeCheckValidity(
        code: *const c_void,
        flags: u32,
        requirement: *const c_void,
    ) -> i32;
    fn SecRequirementCopyString(
        requirement: *const c_void,
        flags: u32,
        text: *mut *const c_void,
    ) -> i32;
    fn SecCopyErrorMessageString(status: i32, reserved: *const c_void) -> *const c_void;
}

/// `kSecCSSigningInformation`: with the team, the Info.plist and more.
const SIGNING_INFORMATION: u32 = 1 << 1;
/// `codesign --verify --deep --strict`: every architecture
/// (`kSecCSCheckAllArchitectures`), nested code (`kSecCSCheckNestedCode`),
/// strictly (`kSecCSStrictValidate`), no symlinks out of the bundle
/// (`kSecCSRestrictSymlinks`) and no resource forks or other sideband data
/// (`kSecCSRestrictSidebandData`).
const STRICT: u32 = 1 | 1 << 3 | 1 << 4 | 1 << 7 | 1 << 9;

// `AVAudioApplication`, looked up by name.
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

// `NSPasteboard`, looked up by name.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// `AVAudioApplicationRecordPermission`: 'undt' and 'grnt' (and 'deny').
const RECORD_UNDETERMINED: isize = 0x756e_6474;
const RECORD_GRANTED: isize = 0x6772_6e74;

/// AVAudioApplication, which holds the microphone permission; macOS 14
/// brought it.
fn audio_application() -> anyhow::Result<&'static objc2::runtime::AnyClass> {
    objc2::runtime::AnyClass::get(c"AVAudioApplication")
        .ok_or_else(|| anyhow::anyhow!("earwig needs macOS 14 or later"))
}

/// Some answer, or "not asked yet" (`None`). AVAudioApplication reads it
/// live, and asks again once it is cleared.
pub fn microphone_allowed() -> anyhow::Result<Option<bool>> {
    let app = audio_application()?;
    // SAFETY: AVAudioApplication's own messages, with the types it declares.
    let permission: isize = autoreleasepool(|_| unsafe {
        let shared: *mut AnyObject = msg_send![app, sharedInstance];
        msg_send![shared, recordPermission]
    });
    Ok(match permission {
        RECORD_UNDETERMINED => None,
        RECORD_GRANTED => Some(true),
        _ => Some(false),
    })
}

/// Show the system's microphone prompt, if it was never answered.
pub fn request_microphone() {
    let Ok(app) = audio_application() else {
        return;
    };
    autoreleasepool(|_| {
        let done = block2::RcBlock::new(|_granted: Bool| {});
        // SAFETY: the method takes a block of a BOOL, and copies it.
        unsafe {
            let () = msg_send![app, requestRecordPermissionWithCompletionHandler: &*done];
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

/// `errSecCSInternalError`, for a call that went wrong without a status.
const INTERNAL: i32 = -67048;

/// A Core Foundation object of ours, released when dropped.
struct Owned(*const c_void);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: created or copied for us, released once.
        unsafe { CFRelease(self.0) };
    }
}

/// What a Core Foundation "copy" or "create" call put in its out pointer,
/// or its status.
fn copied(call: impl FnOnce(*mut *const c_void) -> i32) -> Result<Owned, i32> {
    let mut out = std::ptr::null();
    match call(&raw mut out) {
        0 if out.is_null() => Err(INTERNAL),
        0 => Ok(Owned(out)),
        status => Err(status),
    }
}

/// The running program's code.
fn own_code() -> Option<Owned> {
    // SAFETY: an out pointer for the running code.
    copied(|out| unsafe { SecCodeCopySelf(0, out) }).ok()
}

/// What the signature of `code` (running or on disk) says.
fn signing_info(code: &Owned, flags: u32) -> Option<Owned> {
    // SAFETY: a live code object, an out pointer for the dictionary.
    copied(|out| unsafe { SecCodeCopySigningInformation(code.0, flags, out) }).ok()
}

/// `value` as a string, if it is a CFString.
fn cf_string(value: *const c_void) -> Option<String> {
    // SAFETY: a live object; a CFString is an NSString.
    (!value.is_null() && unsafe { CFGetTypeID(value) == CFStringGetTypeID() })
        .then(|| autoreleasepool(|_| unsafe { &*value.cast::<NSString>() }.to_string()))
}

/// The string `dictionary` holds for `key`.
fn string_value(dictionary: &Owned, key: *const c_void) -> Option<String> {
    // SAFETY: a live dictionary; the value lives as long as it.
    cf_string(unsafe { CFDictionaryGetValue(dictionary.0, key) })
}

/// The hash of this build's code signature (its cdhash), which macOS keeps
/// an ad-hoc signed app's privacy grants for; `None` for unsigned code.
pub fn code_hash() -> Option<Vec<u8>> {
    let info = signing_info(&own_code()?, 0)?;
    // SAFETY: the dictionary, and with it its value for kSecCodeInfoUnique
    // (a CFData), is alive.
    unsafe {
        let unique = CFDictionaryGetValue(info.0, kSecCodeInfoUnique);
        if unique.is_null() {
            return None;
        }
        let len = usize::try_from(CFDataGetLength(unique))
            .ok()
            .filter(|&len| len > 0)?;
        Some(std::slice::from_raw_parts(CFDataGetBytePtr(unique), len).to_vec())
    }
}

/// The team of the Developer ID the running program is signed with; `None`
/// when it is signed ad hoc (Nix, cargo) or not at all.
pub(crate) fn own_team() -> Option<String> {
    let info = signing_info(&own_code()?, SIGNING_INFORMATION)?;
    // SAFETY: a constant of the framework.
    string_value(&info, unsafe { kSecCodeInfoTeamIdentifier }).filter(|team| !team.is_empty())
}

/// What a signature must be to stand for a program: for a Developer ID,
/// its identifier and its team.
pub(crate) struct Requirement(Owned);

impl Requirement {
    /// The one the running program's own signature designates.
    pub(crate) fn own() -> Option<Self> {
        designated(&own_code()?)
    }

    /// The one the app at `path` designates, as signed.
    #[cfg(test)]
    pub(crate) fn of(path: &Path) -> Option<Self> {
        designated(&static_code(path).ok()?)
    }

    /// As `codesign -d -r-` prints it.
    pub(crate) fn text(&self) -> String {
        // SAFETY: a live requirement, an out pointer for its text.
        copied(|out| unsafe { SecRequirementCopyString(self.0.0, 0, out) })
            .ok()
            .and_then(|text| cf_string(text.0))
            .unwrap_or_default()
    }
}

fn designated(code: &Owned) -> Option<Requirement> {
    // SAFETY: a live code object, an out pointer for the requirement.
    copied(|out| unsafe { SecCodeCopyDesignatedRequirement(code.0, 0, out) })
        .ok()
        .map(Requirement)
}

/// Check the app at `path` as `codesign --verify --deep --strict -R` does:
/// validly signed, all of it, and meeting `requirement`. Then its
/// `CFBundleShortVersionString`, from the Info.plist the signature seals.
pub(crate) fn check_app(path: &Path, requirement: &Requirement) -> Result<Option<String>, String> {
    let code = static_code(path).map_err(message)?;
    // SAFETY: live code and requirement objects.
    let status = unsafe { SecStaticCodeCheckValidity(code.0, STRICT, requirement.0.0) };
    if status != 0 {
        return Err(message(status));
    }
    let info = signing_info(&code, SIGNING_INFORMATION).ok_or_else(|| message(INTERNAL))?;
    // SAFETY: a constant of the framework; the plist lives as long as `info`.
    let plist = unsafe { CFDictionaryGetValue(info.0, kSecCodeInfoPList) };
    if plist.is_null() {
        return Ok(None);
    }
    let key = NSString::from_str("CFBundleShortVersionString");
    // SAFETY: a live dictionary and an NSString key (a CFString).
    Ok(cf_string(unsafe {
        CFDictionaryGetValue(plist, Retained::as_ptr(&key).cast())
    }))
}

/// The code at `path`, as signed on disk.
fn static_code(path: &Path) -> Result<Owned, i32> {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = path.as_os_str().as_bytes();
    let len = isize::try_from(bytes.len()).map_err(|_| INTERNAL)?;
    // SAFETY: `len` bytes of path; the URL is ours.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), len, 1)
    };
    if url.is_null() {
        return Err(INTERNAL);
    }
    let url = Owned(url);
    // SAFETY: a live URL, an out pointer for the code object.
    copied(|out| unsafe { SecStaticCodeCreateWithPath(url.0, 0, out) })
}

/// What Security says `status` means.
fn message(status: i32) -> String {
    // SAFETY: returns a string of ours, or null.
    let text = unsafe { SecCopyErrorMessageString(status, std::ptr::null()) };
    let text = (!text.is_null()).then(|| Owned(text));
    match text.and_then(|text| cf_string(text.0)) {
        Some(text) => format!("{text} ({status})"),
        None => format!("error {status}"),
    }
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
    // SAFETY: NSPasteboard's own messages, with the types it declares.
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
    // SAFETY: the source and the events are ours and released once; a null
    // source is allowed, a null event skipped.
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
