//! The Linux backend against a uinput keyboard: keys as a real keyboard
//! sends them, through the kernel, plugged in after the listener started.
//! Needs write access to `/dev/uinput` (root in the `vm-keys` VM test);
//! without it the test says so and passes. With `EARWIG_KEYS_KEYD=1` keyd is
//! expected to hold the keyboards: the keys then come through its virtual
//! keyboard, and the hotkey is seen but cannot be swallowed.
#![cfg(target_os = "linux")]

use std::ffi::c_void;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use earwig_keys::{Blocking, Event, Listener, Options, Recorded, Status};
use rustix::fs::{Mode, OFlags};
use rustix::ioctl::{Direction, Ioctl, IoctlOutput, Opcode, opcode};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const KEY_ESC: u16 = 1;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_C: u16 = 46;
const KEY_SPACE: u16 = 57;
const KEY_F9: u16 = 67;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_LEFTMETA: u16 = 125;

struct Raw {
    opcode: Opcode,
    arg: *mut c_void,
}

// SAFETY: each use passes what its opcode expects.
unsafe impl Ioctl for Raw {
    type Output = IoctlOutput;
    const IS_MUTATING: bool = true;

    fn opcode(&self) -> Opcode {
        self.opcode
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.arg
    }

    unsafe fn output_from_ptr(out: IoctlOutput, _: *mut c_void) -> rustix::io::Result<IoctlOutput> {
        Ok(out)
    }
}

/// A uinput keyboard with a few keys.
struct Keyboard {
    fd: OwnedFd,
}

impl Keyboard {
    fn plug_in() -> Option<Self> {
        let flags = OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::open("/dev/uinput", flags, Mode::empty()).ok()?;
        let set = |number: u8, value: u16| {
            let opcode = opcode::write::<i32>(b'U', number);
            // SAFETY: UI_SET_*BIT take the bit as the argument itself.
            unsafe {
                rustix::ioctl::ioctl(
                    &fd,
                    Raw {
                        opcode,
                        arg: usize::from(value) as *mut c_void,
                    },
                )
            }
            .unwrap();
        };
        set(100, EV_KEY);
        for code in [
            KEY_ESC,
            KEY_LEFTCTRL,
            KEY_LEFTSHIFT,
            KEY_C,
            KEY_SPACE,
            KEY_F9,
            KEY_RIGHTCTRL,
            KEY_LEFTMETA,
        ] {
            set(101, code);
        }
        // struct uinput_setup: input_id, name[80], ff_effects_max.
        let mut setup = [0u8; 92];
        setup[0..2].copy_from_slice(&3u16.to_ne_bytes());
        setup[2..4].copy_from_slice(&0x1234u16.to_ne_bytes());
        let name = b"earwig-keys test keyboard";
        setup[8..8 + name.len()].copy_from_slice(name);
        let opcode = opcode::from_components(Direction::Write, b'U', 3, setup.len());
        // SAFETY: UI_DEV_SETUP reads a uinput_setup; UI_DEV_CREATE takes nothing.
        unsafe {
            rustix::ioctl::ioctl(
                &fd,
                Raw {
                    opcode,
                    arg: setup.as_mut_ptr().cast(),
                },
            )
            .unwrap();
            rustix::ioctl::ioctl(
                &fd,
                Raw {
                    opcode: opcode::none(b'U', 1),
                    arg: std::ptr::null_mut(),
                },
            )
            .unwrap();
        }
        // Long enough for udev and the listener (and keyd) to find it.
        thread::sleep(Duration::from_secs(2));
        Some(Self { fd })
    }

    fn key(&self, code: u16, down: bool) {
        let mut bytes = Vec::new();
        for (kind, code, value) in [(EV_KEY, code, i32::from(down)), (EV_SYN, 0, 0)] {
            bytes.extend_from_slice(&[0u8; 16]);
            bytes.extend_from_slice(&kind.to_ne_bytes());
            bytes.extend_from_slice(&code.to_ne_bytes());
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        rustix::io::write(&self.fd, &bytes).unwrap();
        thread::sleep(Duration::from_millis(30));
    }

    fn press(&self, codes: &[u16]) {
        for &code in codes {
            self.key(code, true);
        }
    }

    fn release(&self, codes: &[u16]) {
        for &code in codes.iter().rev() {
            self.key(code, false);
        }
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        // SAFETY: UI_DEV_DESTROY takes nothing.
        let _ = unsafe {
            rustix::ioctl::ioctl(
                &self.fd,
                Raw {
                    opcode: opcode::none(b'U', 2),
                    arg: std::ptr::null_mut(),
                },
            )
        };
    }
}

fn next<T>(rx: &Receiver<T>) -> Option<T> {
    rx.recv_timeout(Duration::from_secs(3)).ok()
}

fn nothing<T: std::fmt::Debug>(rx: &Receiver<T>) {
    let got = rx.recv_timeout(Duration::from_millis(300));
    assert!(got.is_err(), "unexpected {got:?}");
}

#[test]
fn a_uinput_keyboard_drives_the_hotkey_and_the_recorder() {
    if rustix::fs::access("/dev/uinput", rustix::fs::Access::WRITE_OK).is_err() {
        eprintln!("skipped: no write access to /dev/uinput");
        return;
    }
    let keyd = std::env::var_os("EARWIG_KEYS_KEYD").is_some();
    let listener = Listener::new(Options::default());
    listener.set_hotkey(Some("Super+Space".parse().unwrap()));
    let (tx, events) = mpsc::channel();
    let runner = {
        let listener = listener.clone();
        thread::spawn(move || listener.run(move |event| tx.send(event).is_ok()))
    };
    let keyboard = Keyboard::plug_in().unwrap();

    // A key with modifiers.
    keyboard.press(&[KEY_LEFTMETA, KEY_SPACE]);
    assert_eq!(next(&events), Some(Event::Pressed));
    keyboard.release(&[KEY_LEFTMETA, KEY_SPACE]);
    assert_eq!(next(&events), Some(Event::Released));
    keyboard.press(&[KEY_SPACE]);
    keyboard.release(&[KEY_SPACE]);
    nothing(&events);
    if keyd {
        assert_eq!(
            listener.status(),
            Status::Watching(Blocking::No("keyd holds the keyboard".into()))
        );
    } else {
        assert!(
            matches!(listener.status(), Status::Watching(_)),
            "{:?}",
            listener.status()
        );
    }

    // A modifier alone, held, and part of a shortcut.
    listener.set_hotkey(Some("RightCtrl".parse().unwrap()));
    keyboard.press(&[KEY_RIGHTCTRL]);
    assert_eq!(next(&events), Some(Event::Pressed));
    keyboard.release(&[KEY_RIGHTCTRL]);
    assert_eq!(next(&events), Some(Event::Released));
    keyboard.press(&[KEY_LEFTCTRL]);
    keyboard.release(&[KEY_LEFTCTRL]);
    nothing(&events);
    keyboard.press(&[KEY_RIGHTCTRL, KEY_C]);
    assert_eq!(next(&events), Some(Event::Pressed));
    assert_eq!(next(&events), Some(Event::Chord));
    keyboard.release(&[KEY_RIGHTCTRL, KEY_C]);
    nothing(&events);

    // Recording takes the keys instead of the hotkey.
    let (tx, recorded) = mpsc::channel();
    let record = |listener: &Listener| {
        let tx = tx.clone();
        listener.record(move |step| {
            if !matches!(step, Recorded::Holding(_)) {
                tx.send(step).unwrap();
            }
        });
    };
    record(&listener);
    keyboard.press(&[KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_F9]);
    assert_eq!(
        next(&recorded),
        Some(Recorded::Done("Ctrl+Shift+F9".parse().unwrap()))
    );
    keyboard.release(&[KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_F9]);
    record(&listener);
    keyboard.press(&[KEY_ESC]);
    keyboard.release(&[KEY_ESC]);
    assert_eq!(next(&recorded), Some(Recorded::Cancelled));
    record(&listener);
    keyboard.press(&[KEY_RIGHTCTRL]);
    keyboard.release(&[KEY_RIGHTCTRL]);
    assert_eq!(
        next(&recorded),
        Some(Recorded::Done("RightCtrl".parse().unwrap()))
    );
    nothing(&events);

    // A keyboard unplugged with the hotkey down lets go of it. Behind keyd,
    // the key stays down on keyd's keyboard until keyd lets go.
    if !keyd {
        keyboard.press(&[KEY_RIGHTCTRL]);
        assert_eq!(next(&events), Some(Event::Pressed));
        drop(keyboard);
        assert_eq!(next(&events), Some(Event::Released));
    }

    listener.stop();
    runner.join().unwrap().unwrap();
    assert_eq!(listener.status(), Status::Stopped);
}
