//! evdev devices and their uinput clones, through raw ioctls (`linux/input.h`, `linux/uinput.h`).

use std::ffi::{CString, c_int, c_void};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use rustix::ioctl::{Ioctl, IoctlOutput, Opcode, opcode};

use super::codes;

pub(super) const EV_SYN: u16 = 0x00;
pub(super) const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
pub(super) const EV_MSC: u16 = 0x04;
pub(super) const EV_LED: u16 = 0x11;
pub(super) const SYN_REPORT: u16 = 0;
pub(super) const SYN_DROPPED: u16 = 3;
const MSC_SCAN: u16 = 4;
const REL_X: u16 = 0;
const ABS_X: u16 = 0;

/// `phys` of earwig's own uinput clones, so it never reads them back.
const OWN_PHYS: &str = "earwig-keys";

/// `struct input_event` on 64-bit Linux (`timeval` is two i64s).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct InputEvent {
    pub sec: i64,
    pub usec: i64,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

impl InputEvent {
    pub(super) fn new(kind: u16, code: u16, value: i32) -> Self {
        Self {
            kind,
            code,
            value,
            ..Self::default()
        }
    }

    pub(super) fn syn() -> Self {
        Self::new(EV_SYN, SYN_REPORT, 0)
    }

    /// `CLOCK_MONOTONIC` microseconds (see [`Device::open`]).
    pub(super) fn micros(&self) -> i64 {
        self.sec * 1_000_000 + self.usec
    }
}

/// `struct input_id`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct InputId {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

/// A bit array as evdev reports capabilities and key state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Bits<const N: usize>([u8; N]);

/// `KEY_MAX + 1` bits.
pub(super) type KeyBits = Bits<96>;

impl<const N: usize> Default for Bits<N> {
    fn default() -> Self {
        Self([0; N])
    }
}

impl<const N: usize> Bits<N> {
    pub(super) fn has(&self, bit: u16) -> bool {
        self.0
            .get(usize::from(bit / 8))
            .is_some_and(|byte| byte & (1 << (bit % 8)) != 0)
    }

    pub(super) fn set(&mut self, bit: u16, on: bool) {
        if let Some(byte) = self.0.get_mut(usize::from(bit / 8)) {
            if on {
                *byte |= 1 << (bit % 8);
            } else {
                *byte &= !(1 << (bit % 8));
            }
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.iter().all(|&byte| byte == 0)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        let bits = u16::try_from(N * 8).unwrap_or(u16::MAX);
        (0..bits).filter(|&bit| self.has(bit))
    }
}

/// An ioctl with an opcode known only at run time and a raw argument.
struct Raw {
    opcode: Opcode,
    arg: *mut c_void,
}

// SAFETY: every caller passes the argument its opcode expects.
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

/// # Safety
///
/// `arg` must be what `opcode` expects: a pointer to its argument, or the integer itself.
unsafe fn ioctl(fd: impl AsFd, opcode: Opcode, arg: *mut c_void) -> io::Result<IoctlOutput> {
    // SAFETY: the caller's.
    Ok(unsafe { rustix::ioctl::ioctl(fd, Raw { opcode, arg }) }?)
}

/// An evdev ioctl that reads into `buf` (`EVIOCGNAME`, `EVIOCGBIT`, …).
fn read_into(fd: impl AsFd, number: u8, buf: &mut [u8]) -> io::Result<usize> {
    let opcode = opcode::from_components(rustix::ioctl::Direction::Read, b'E', number, buf.len());
    // SAFETY: the kernel writes at most `buf.len()` bytes, the size in the opcode.
    let read = unsafe { ioctl(fd, opcode, buf.as_mut_ptr().cast()) }?;
    Ok(usize::try_from(read).unwrap_or(0))
}

fn read_string(fd: impl AsFd, number: u8) -> io::Result<String> {
    let mut buf = [0u8; 256];
    read_into(fd, number, &mut buf)?;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
}

fn read_bits<const N: usize>(fd: impl AsFd, kind: u16) -> io::Result<Bits<N>> {
    let mut bits = Bits::default();
    // EVIOCGBIT(kind, len)
    read_into(fd, 0x20 + u8::try_from(kind).unwrap_or(0), &mut bits.0)?;
    Ok(bits)
}

/// An ioctl that takes an `int` by value.
fn set_int(fd: impl AsFd, group: u8, number: u8, value: usize) -> io::Result<()> {
    // SAFETY: the argument is the integer itself.
    unsafe {
        ioctl(
            fd,
            opcode::write::<c_int>(group, number),
            value as *mut c_void,
        )
    }?;
    Ok(())
}

/// The keys of `fd` that are down now (`EVIOCGKEY`).
pub(super) fn keys_down(fd: impl AsFd) -> io::Result<KeyBits> {
    let mut bits = KeyBits::default();
    read_into(fd, 0x18, &mut bits.0)?;
    Ok(bits)
}

/// An input device that has keys earwig names.
#[expect(clippy::struct_excessive_bools, reason = "independent properties")]
pub(super) struct Device {
    pub path: PathBuf,
    pub fd: OwnedFd,
    pub name: String,
    id: InputId,
    pub keys: KeyBits,
    leds: Bits<2>,
    scan_codes: bool,
    /// A mouse or touchpad, which is never grabbed.
    pub pointer: bool,
    /// Made by a program through uinput (keyd, a test), not a keyboard.
    pub is_virtual: bool,
    /// Opened for writing too, to set its LEDs.
    writable: bool,
}

impl Device {
    /// `None` if it has no keys earwig names or is one of our clones.
    pub(super) fn open(path: &Path) -> io::Result<Option<Self>> {
        let flags = OFlags::NONBLOCK | OFlags::CLOEXEC;
        let (fd, writable) = match rustix::fs::open(path, flags | OFlags::RDWR, Mode::empty()) {
            Ok(fd) => (fd, true),
            Err(rustix::io::Errno::ACCESS) => (
                rustix::fs::open(path, flags | OFlags::RDONLY, Mode::empty())?,
                false,
            ),
            Err(e) => return Err(e.into()),
        };
        // Devices without one say ENOENT.
        if read_string(&fd, 0x07).unwrap_or_default() == OWN_PHYS {
            return Ok(None);
        }
        let events: Bits<4> = read_bits(&fd, 0)?;
        if !events.has(EV_KEY) {
            return Ok(None);
        }
        let keys: KeyBits = read_bits(&fd, EV_KEY)?;
        if !keys.iter().any(|code| codes::key(code).is_some()) {
            return Ok(None);
        }
        let pointer = (events.has(EV_REL) && read_bits::<2>(&fd, EV_REL)?.has(REL_X))
            || (events.has(EV_ABS) && read_bits::<8>(&fd, EV_ABS)?.has(ABS_X));
        let mut id = InputId::default();
        // SAFETY: EVIOCGID writes one `struct input_id`.
        unsafe {
            ioctl(
                &fd,
                opcode::read::<InputId>(b'E', 0x02),
                (&raw mut id).cast(),
            )?;
        }
        // Event times on CLOCK_MONOTONIC, comparable with the grab's.
        let mut clock: c_int = 1;
        // SAFETY: EVIOCSCLOCKID reads one `int`.
        unsafe {
            ioctl(
                &fd,
                opcode::write::<c_int>(b'E', 0xa0),
                (&raw mut clock).cast(),
            )?;
        }
        let is_virtual = path
            .file_name()
            .map(|name| Path::new("/sys/class/input").join(name))
            .and_then(|class| std::fs::canonicalize(class).ok())
            .is_some_and(|sys| sys.starts_with("/sys/devices/virtual"));
        Ok(Some(Self {
            path: path.to_owned(),
            name: read_string(&fd, 0x06).unwrap_or_default(),
            id,
            keys,
            leds: if events.has(EV_LED) {
                read_bits(&fd, EV_LED)?
            } else {
                Bits::default()
            },
            scan_codes: events.has(EV_MSC) && read_bits::<1>(&fd, EV_MSC)?.has(MSC_SCAN),
            pointer,
            is_virtual,
            writable,
            fd,
        }))
    }

    pub(super) fn read(&self, events: &mut [InputEvent]) -> io::Result<usize> {
        read_events(&self.fd, events)
    }

    /// Hold the device for this process alone (`EVIOCGRAB`), or let go.
    pub(super) fn grab(&self, on: bool) -> io::Result<()> {
        set_int(&self.fd, b'E', 0x90, usize::from(on))
    }

    /// Set an LED the compositor set on the clone.
    pub(super) fn set_led(&self, led: InputEvent) {
        if self.writable {
            let _ = write_events(&self.fd, &[led, InputEvent::syn()]);
        }
    }
}

/// A uinput clone of a [`Device`] that carries its keys on while it's grabbed; destroyed on drop.
pub(super) struct Uinput {
    pub fd: OwnedFd,
}

/// `struct uinput_setup`.
#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

/// A [`Uinput`] configured but not created yet: nothing has appeared.
pub(super) struct SetUp {
    fd: OwnedFd,
}

impl Uinput {
    /// Configure a clone of `device`; fails without access to `/dev/uinput`.
    pub(super) fn set_up(device: &Device) -> io::Result<SetUp> {
        let flags = OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::open("/dev/uinput", flags, Mode::empty())?;
        let ui = |number: u8, value: u16| set_int(&fd, b'U', number, usize::from(value));
        // UI_SET_EVBIT, UI_SET_KEYBIT
        ui(100, EV_KEY)?;
        for code in device.keys.iter().chain([codes::KEY_LEFTCTRL]) {
            ui(101, code)?;
        }
        if device.scan_codes {
            ui(100, EV_MSC)?;
            ui(104, MSC_SCAN)?;
        }
        if !device.leds.is_empty() {
            ui(100, EV_LED)?;
            for led in device.leds.iter() {
                ui(105, led)?;
            }
        }
        let phys = CString::new(OWN_PHYS).unwrap_or_default();
        // SAFETY: UI_SET_PHYS copies the NUL-terminated string before it returns.
        unsafe {
            ioctl(
                &fd,
                opcode::write::<*const u8>(b'U', 108),
                phys.as_ptr().cast_mut().cast(),
            )?;
        }
        // Same name and ids, so compositor settings for the keyboard still apply.
        let mut setup = UinputSetup {
            id: device.id,
            name: [0; 80],
            ff_effects_max: 0,
        };
        let name = device.name.as_bytes();
        let len = name.len().min(setup.name.len() - 1);
        setup.name[..len].copy_from_slice(&name[..len]);
        // SAFETY: UI_DEV_SETUP reads one `struct uinput_setup`.
        unsafe {
            ioctl(
                &fd,
                opcode::write::<UinputSetup>(b'U', 3),
                (&raw mut setup).cast(),
            )?;
        }
        Ok(SetUp { fd })
    }

    pub(super) fn write(&self, events: &[InputEvent]) -> io::Result<()> {
        write_events(&self.fd, events)
    }

    /// Events the compositor wrote to the clone (its LEDs).
    pub(super) fn read(&self, events: &mut [InputEvent]) -> io::Result<usize> {
        read_events(&self.fd, events)
    }
}

impl SetUp {
    /// Make the clone appear.
    pub(super) fn create(self) -> io::Result<Uinput> {
        // SAFETY: UI_DEV_CREATE takes no argument.
        unsafe { ioctl(&self.fd, opcode::none(b'U', 1), std::ptr::null_mut()) }?;
        Ok(Uinput { fd: self.fd })
    }
}

impl Drop for Uinput {
    fn drop(&mut self) {
        // SAFETY: UI_DEV_DESTROY takes no argument; the kernel releases keys still down.
        let _ = unsafe { ioctl(&self.fd, opcode::none(b'U', 2), std::ptr::null_mut()) };
    }
}

fn read_events(fd: impl AsFd, events: &mut [InputEvent]) -> io::Result<usize> {
    // SAFETY: `InputEvent` is plain data; the kernel writes whole events.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(events.as_mut_ptr().cast::<u8>(), size_of_val(events))
    };
    let read = rustix::io::read(fd, bytes)?;
    Ok(read / size_of::<InputEvent>())
}

fn write_events(fd: impl AsFd, events: &[InputEvent]) -> io::Result<()> {
    // SAFETY: `InputEvent` is plain data without padding.
    let bytes =
        unsafe { std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), size_of_val(events)) };
    let written = rustix::io::write(fd, bytes)?;
    if written != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short write of input events",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_events_are_the_kernels_size() {
        assert_eq!(size_of::<InputEvent>(), 24);
        assert_eq!(size_of::<UinputSetup>(), 92);
    }

    #[test]
    fn bits_set_and_list() {
        let mut bits = KeyBits::default();
        assert!(bits.is_empty());
        bits.set(57, true);
        bits.set(0x2ff, true);
        assert!(bits.has(57) && bits.has(0x2ff) && !bits.has(58));
        assert_eq!(bits.iter().collect::<Vec<_>>(), [57, 0x2ff]);
        bits.set(57, false);
        bits.set(0x2ff, false);
        assert!(bits.is_empty());
    }
}
