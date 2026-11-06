//! Right Command hold/release through a listen-only `CGEventTap`.

use std::ffi::c_void;
use std::time::Instant;

use crate::ffi::{self, CFMachPortRef, CGEventRef};

#[derive(Clone, Copy, Debug)]
pub enum Edge {
    Press,
    Release,
    /// Another key went down while Right Command was held.
    Chord,
}

struct Tap {
    tx: flume::Sender<Edge>,
    port: CFMachPortRef,
    down: bool,
    chorded: bool,
    flags: u64,
    since: Instant,
}

impl Tap {
    fn send(&self, edge: Edge) {
        crate::log(&format!(
            "hotkey: {edge:?} at +{} ms",
            self.since.elapsed().as_millis()
        ));
        let _ = self.tx.send(edge);
    }
}

unsafe extern "C" fn callback(
    _proxy: *mut c_void,
    kind: u32,
    event: CGEventRef,
    info: *mut c_void,
) -> CGEventRef {
    let tap = unsafe { &mut *info.cast::<Tap>() };
    match kind {
        ffi::TAP_DISABLED_BY_TIMEOUT | ffi::TAP_DISABLED_BY_USER_INPUT => {
            crate::log(&format!("hotkey: tap disabled ({kind:#x}), re-enabling"));
            unsafe { ffi::CGEventTapEnable(tap.port, true) };
        }
        ffi::FLAGS_CHANGED => {
            let code = unsafe { ffi::CGEventGetIntegerValueField(event, ffi::FIELD_KEYCODE) };
            let flags = unsafe { ffi::CGEventGetFlags(event) };
            let pressed = flags & !tap.flags;
            tap.flags = flags;
            if code == ffi::KEYCODE_RIGHT_COMMAND {
                let down = flags & ffi::FLAG_RIGHT_COMMAND != 0;
                if down && !tap.down {
                    tap.down = true;
                    tap.chorded = false;
                    tap.send(Edge::Press);
                } else if !down && tap.down {
                    tap.down = false;
                    if !tap.chorded {
                        tap.send(Edge::Release);
                    }
                }
            } else if tap.down && !tap.chorded && pressed != 0 {
                tap.chorded = true;
                tap.send(Edge::Chord);
            }
        }
        ffi::KEY_DOWN => {
            if tap.down && !tap.chorded {
                tap.chorded = true;
                tap.send(Edge::Chord);
            }
        }
        _ => {}
    }
    event
}

/// Start the tap on its own thread with its own run loop.
pub fn spawn(tx: flume::Sender<Edge>) {
    std::thread::spawn(move || {
        let tap = Box::into_raw(Box::new(Tap {
            tx,
            port: std::ptr::null_mut(),
            down: false,
            chorded: false,
            flags: 0,
            since: Instant::now(),
        }));
        let mask = (1u64 << ffi::KEY_DOWN) | (1u64 << ffi::KEY_UP) | (1u64 << ffi::FLAGS_CHANGED);
        unsafe {
            let port = ffi::CGEventTapCreate(
                ffi::TAP_SESSION,
                ffi::HEAD_INSERT,
                ffi::LISTEN_ONLY,
                mask,
                callback,
                tap.cast(),
            );
            if port.is_null() {
                crate::log("hotkey: CGEventTapCreate failed (Input Monitoring not granted?)");
                return;
            }
            (*tap).port = port;
            let source = ffi::CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
            ffi::CFRunLoopAddSource(
                ffi::CFRunLoopGetCurrent(),
                source,
                ffi::kCFRunLoopCommonModes,
            );
            ffi::CGEventTapEnable(port, true);
            crate::log("hotkey: tap running; hold or tap Right Command");
            ffi::CFRunLoopRun();
        }
    });
}

/// Print edges for `secs` seconds.
pub fn run(secs: u64) {
    let (tx, rx) = flume::unbounded();
    spawn(tx);
    let deadline = Instant::now() + std::time::Duration::from_secs(secs);
    while let Ok(_edge) = rx.recv_deadline(deadline) {}
    crate::log("hotkey: done");
}
