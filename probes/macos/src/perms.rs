//! Privacy permissions: what is granted, and asking for each.

use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

use crate::ffi;

/// `AVAuthorizationStatus`: 0 not determined, 1 restricted, 2 denied, 3
/// authorized.
pub fn microphone() -> isize {
    let audio: Retained<NSString> = NSString::from_str("soun");
    unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*audio] }
}

fn mic_name(status: isize) -> &'static str {
    match status {
        0 => "not determined",
        1 => "restricted",
        2 => "denied",
        3 => "authorized",
        _ => "unknown",
    }
}

fn hid_name(status: u32) -> &'static str {
    match status {
        0 => "granted",
        1 => "denied",
        _ => "unknown",
    }
}

pub fn report() {
    crate::log(&format!(
        "perms: microphone={} accessibility(AXIsProcessTrusted)={} post-events(CGPreflightPostEventAccess)={} listen-events(CGPreflightListenEventAccess)={} input-monitoring(IOHIDCheckAccess)={}",
        mic_name(microphone()),
        ffi::AXIsProcessTrusted(),
        ffi::CGPreflightPostEventAccess(),
        ffi::CGPreflightListenEventAccess(),
        hid_name(ffi::IOHIDCheckAccess(ffi::LISTEN_EVENT)),
    ));
}

/// Ask for one permission the way the daemon would, then report.
pub fn request(which: &str) {
    match which {
        "microphone" => {
            let (tx, rx) = flume::bounded(1);
            let audio: Retained<NSString> = NSString::from_str("soun");
            let block = block2::RcBlock::new(move |granted: Bool| {
                let _ = tx.send(granted.as_bool());
            });
            unsafe {
                let () = msg_send![
                    class!(AVCaptureDevice),
                    requestAccessForMediaType: &*audio,
                    completionHandler: &*block
                ];
            }
            match rx.recv_timeout(Duration::from_secs(120)) {
                Ok(granted) => crate::log(&format!("request microphone: granted={granted}")),
                Err(_) => crate::log("request microphone: no answer within 120 s"),
            }
        }
        "accessibility" => crate::log(&format!(
            "request accessibility: CGRequestPostEventAccess={}",
            ffi::CGRequestPostEventAccess()
        )),
        "input-monitoring" => crate::log(&format!(
            "request input-monitoring: CGRequestListenEventAccess={} IOHIDRequestAccess={}",
            ffi::CGRequestListenEventAccess(),
            ffi::IOHIDRequestAccess(ffi::LISTEN_EVENT)
        )),
        other => crate::log(&format!("unknown permission {other}")),
    }
    report();
}

/// Open the System Settings pane for one permission.
pub fn open_pane(which: &str) {
    let anchor = match which {
        "microphone" => "Privacy_Microphone",
        "accessibility" => "Privacy_Accessibility",
        "input-monitoring" => "Privacy_ListenEvent",
        other => {
            crate::log(&format!("unknown pane {other}"));
            return;
        }
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    let status = std::process::Command::new("/usr/bin/open")
        .arg(&url)
        .status();
    crate::log(&format!("open {url}: {status:?}"));
}
