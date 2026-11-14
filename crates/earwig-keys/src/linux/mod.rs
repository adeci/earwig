//! Linux: read the keyboards' evdev nodes directly (hot-plug via inotify), on any desktop.
//! Swallowing grabs each keyboard with the hotkey's key and passes other keys on through a uinput
//! clone (same name and ids, so compositor settings still apply). Grabs change only while no key
//! is down. Virtual keyboards are never grabbed: keyd grabs every new one, clones too, and the two
//! would feed each other.

mod codes;
mod evdev;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec};
use rustix::fs::inotify;

use self::evdev::{
    Device, EV_KEY, EV_LED, EV_SYN, InputEvent, KeyBits, SYN_DROPPED, SYN_REPORT, Uinput,
};
use crate::Event;
use crate::hotkey::Hotkey;
use crate::key::Held;
use crate::listener::{Blocking, Shared, Snapshot, Status};
use crate::matcher::{Edge, Mask};

const DEV_INPUT: &str = "/dev/input";
/// How long a refused grab waits to retry; a busy keyboard waits for devices to change instead.
const RETRY: Duration = Duration::from_secs(10);

/// Wakes [`run`] when the configuration changes: an eventfd.
pub(crate) struct Waker(Option<OwnedFd>);

impl Default for Waker {
    fn default() -> Self {
        let flags = EventfdFlags::NONBLOCK | EventfdFlags::CLOEXEC;
        Self(rustix::event::eventfd(0, flags).ok())
    }
}

impl Waker {
    pub(crate) fn wake(&self) {
        if let Some(fd) = &self.0 {
            let _ = rustix::io::write(fd, &1u64.to_ne_bytes());
        }
    }

    fn drain(&self) {
        if let Some(fd) = &self.0 {
            let _ = rustix::io::read(fd, &mut [0u8; 8]);
        }
    }
}

pub(crate) fn can_see_keys() -> bool {
    event_nodes().any(|path| matches!(Device::open(&path), Ok(Some(_))))
}

pub(crate) fn run(shared: &Shared, events: &mut dyn FnMut(Event) -> bool) -> io::Result<()> {
    let notify = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC)?;
    let watched =
        inotify::WatchFlags::CREATE | inotify::WatchFlags::ATTRIB | inotify::WatchFlags::DELETE;
    inotify::add_watch(&notify, DEV_INPUT, watched)?;
    let mut keyboards = Keyboards::default();
    for path in event_nodes() {
        keyboards.add(&path);
    }
    loop {
        let snapshot = shared.snapshot();
        if snapshot.stop {
            return Ok(());
        }
        keyboards.settle(&snapshot);
        shared.set_status(keyboards.status(&snapshot));
        let ready = keyboards.wait(&notify, &shared.waker)?;
        if ready.waker {
            shared.waker.drain();
        }
        if ready.notify {
            for (path, deleted) in changes(&notify) {
                if deleted {
                    keyboards.denied.remove(&path);
                } else {
                    keyboards.add(&path);
                }
            }
            // Permissions may have changed: /dev/uinput's with them.
            keyboards.retry_refused();
        }
        for i in ready.clones {
            keyboards.forward_leds(i);
        }
        // From the end, so removing one keeps the others' indices.
        for i in ready.keyboards.into_iter().rev() {
            if !keyboards.read(i, shared, events) {
                return Ok(());
            }
        }
    }
}

fn event_nodes() -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(DEV_INPUT)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_event_node(path))
}

fn is_event_node(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with("event"))
}

/// Event nodes created, changed (permissions) or deleted since last time.
fn changes(notify: &OwnedFd) -> Vec<(PathBuf, bool)> {
    let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
    let mut reader = inotify::Reader::new(notify, &mut buf);
    let mut changes = Vec::new();
    // Ends with EAGAIN once drained.
    while let Ok(event) = reader.next() {
        let Some(name) = event.file_name() else {
            continue;
        };
        let path = Path::new(DEV_INPUT).join(OsStr::from_bytes(name.to_bytes()));
        if is_event_node(&path) {
            let deleted = event.events().contains(inotify::ReadFlags::DELETE);
            changes.push((path, deleted));
        }
    }
    changes
}

/// Swallowed: a key with modifiers, or modifiers alone that need a mask key.
fn swallows(hotkey: &Hotkey) -> bool {
    !hotkey.is_modifier_only() || hotkey.needs_mask()
}

fn trigger_codes(hotkey: &Hotkey) -> Vec<u16> {
    codes::table()
        .filter(|&(_, key)| hotkey.is_trigger(key))
        .map(|(code, _)| code)
        .collect()
}

#[derive(Default)]
struct Keyboards {
    keyboards: Vec<Keyboard>,
    /// Event nodes this process may not read.
    denied: BTreeSet<PathBuf>,
}

/// What [`Keyboards::wait`] found readable.
#[derive(Default)]
struct Ready {
    waker: bool,
    notify: bool,
    keyboards: Vec<usize>,
    clones: Vec<usize>,
}

impl Keyboards {
    fn add(&mut self, path: &Path) {
        if self.keyboards.iter().any(|k| k.device.path == path) {
            return;
        }
        match Device::open(path) {
            Ok(Some(device)) => {
                self.denied.remove(path);
                log::info!("hotkey: reading {} ({})", device.name, path.display());
                self.keyboards.push(Keyboard::new(device));
            }
            Ok(None) => {
                self.denied.remove(path);
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                self.denied.insert(path.to_owned());
            }
            Err(e) => log::debug!("hotkey: {}: {e}", path.display()),
        }
    }

    /// Grab the keyboards the hotkey wants and let go of the rest, each once no key is down on it.
    fn settle(&mut self, snapshot: &Snapshot) {
        let codes = snapshot
            .hotkey
            .as_ref()
            .filter(|hotkey| snapshot.options.block && swallows(hotkey))
            .map(trigger_codes)
            .unwrap_or_default();
        for keyboard in &mut self.keyboards {
            let device = &keyboard.device;
            let wanted = !device.is_virtual
                && !device.pointer
                && codes.iter().any(|&code| device.keys.has(code));
            keyboard.settle(wanted);
        }
    }

    fn retry_refused(&mut self) {
        for keyboard in &mut self.keyboards {
            if matches!(keyboard.grab, Grab::Refused { .. }) {
                keyboard.grab = Grab::No;
            }
        }
    }

    fn status(&self, snapshot: &Snapshot) -> Status {
        if self.keyboards.is_empty() {
            return if self.denied.is_empty() {
                Status::NoKeyboard
            } else {
                Status::NoAccess
            };
        }
        let Some(hotkey) = snapshot.hotkey.as_ref().filter(|hotkey| swallows(hotkey)) else {
            return Status::Watching(Blocking::PassThrough);
        };
        if !snapshot.options.block {
            return Status::Watching(Blocking::No("swallowing it is off".into()));
        }
        let codes = trigger_codes(hotkey);
        let refused = self
            .keyboards
            .iter()
            .filter(|k| codes.iter().any(|&code| k.device.keys.has(code)))
            // Between a retry and its answer the last refusal still holds.
            .find_map(|k| match &k.grab {
                Grab::Held { .. } => None,
                _ => k.refused.as_ref(),
            });
        match refused {
            Some(Problem::Busy)
                if self
                    .keyboards
                    .iter()
                    .any(|k| k.device.name == "keyd virtual keyboard") =>
            {
                Status::Watching(Blocking::No("keyd holds the keyboard".into()))
            }
            Some(Problem::Busy) => {
                Status::Watching(Blocking::No("another program holds the keyboard".into()))
            }
            Some(Problem::NoUinput) => {
                Status::Watching(Blocking::No("no access to /dev/uinput".into()))
            }
            Some(Problem::Failed(e)) => Status::Watching(Blocking::No(e.clone())),
            None => Status::Watching(Blocking::Yes),
        }
    }

    /// Block until something is readable or a refused grab is due again.
    fn wait(&self, notify: &OwnedFd, waker: &Waker) -> io::Result<Ready> {
        #[derive(Clone, Copy)]
        enum Source {
            Waker,
            Notify,
            Keyboard(usize),
            Clone(usize),
        }
        let mut fds = Vec::new();
        let mut sources = Vec::new();
        if let Some(fd) = &waker.0 {
            fds.push(PollFd::new(fd, PollFlags::IN));
            sources.push(Source::Waker);
        }
        fds.push(PollFd::new(notify, PollFlags::IN));
        sources.push(Source::Notify);
        for (i, keyboard) in self.keyboards.iter().enumerate() {
            fds.push(PollFd::new(&keyboard.device.fd, PollFlags::IN));
            sources.push(Source::Keyboard(i));
            if let Grab::Held { clone, .. } = &keyboard.grab {
                fds.push(PollFd::new(&clone.fd, PollFlags::IN));
                sources.push(Source::Clone(i));
            }
        }
        // A busy keyboard is retried when devices change (keyd stopping drops its keyboard).
        let refused = self
            .keyboards
            .iter()
            .any(|k| matches!(&k.grab, Grab::Refused { problem, .. } if *problem != Problem::Busy));
        let timeout = Timespec {
            tv_sec: i64::try_from(RETRY.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: 0,
        };
        loop {
            match rustix::event::poll(&mut fds, refused.then_some(&timeout)) {
                Ok(_) => break,
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let mut ready = Ready::default();
        for (fd, source) in fds.iter().zip(sources) {
            if fd.revents().is_empty() {
                continue;
            }
            match source {
                Source::Waker => ready.waker = true,
                Source::Notify => ready.notify = true,
                Source::Keyboard(i) => ready.keyboards.push(i),
                Source::Clone(i) => ready.clones.push(i),
            }
        }
        Ok(ready)
    }

    /// Modifier keys down on every keyboard but `except`.
    fn held_except(&self, except: usize) -> Held {
        self.keyboards
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != except)
            .fold(Held::default(), |held, (_, keyboard)| keyboard.held(held))
    }

    /// Reads all keyboard `i` has (dropping it if gone); false once `events` wants no more.
    fn read(&mut self, i: usize, shared: &Shared, events: &mut dyn FnMut(Event) -> bool) -> bool {
        let others = self.held_except(i);
        let keyboard = &mut self.keyboards[i];
        let mut buf = [InputEvent::default(); 64];
        loop {
            let count = match keyboard.device.read(&mut buf) {
                Ok(0) => break,
                Ok(count) => count,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::info!("hotkey: {} is gone: {e}", keyboard.device.path.display());
                    let gone = self.keyboards.remove(i);
                    return gone.release_all(others, shared, events);
                }
            };
            for &event in &buf[..count] {
                if !keyboard.event(event, others, shared, events) {
                    return false;
                }
            }
        }
        if keyboard.lost_grab {
            keyboard.lost_grab = false;
            keyboard.ungrab();
        }
        true
    }

    fn forward_leds(&self, i: usize) {
        let keyboard = &self.keyboards[i];
        let Grab::Held { clone, .. } = &keyboard.grab else {
            return;
        };
        let mut buf = [InputEvent::default(); 16];
        while let Ok(count @ 1..) = clone.read(&mut buf) {
            for &event in buf[..count].iter().filter(|e| e.kind == EV_LED) {
                keyboard.device.set_led(event);
            }
        }
    }
}

/// Why a keyboard that should be grabbed is not.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Problem {
    NoUinput,
    /// Another program grabbed it.
    Busy,
    Failed(String),
}

enum Grab {
    No,
    Held {
        clone: Uinput,
        /// When the grab took hold (`CLOCK_MONOTONIC` µs); earlier events reached the compositor.
        since: i64,
    },
    Refused {
        problem: Problem,
        at: Instant,
    },
}

struct Keyboard {
    device: Device,
    /// Keys down as its events said, never the kernel's view: a keyboard keyd holds sends us
    /// nothing, so a kernel snapshot would leave Super held for good (every Space Super+Space).
    down: KeyBits,
    grab: Grab,
    /// Why the last grab was refused, logged once.
    refused: Option<Problem>,
    /// The frame being read (up to `SYN_REPORT`), to pass on while grabbed.
    frame: Vec<InputEvent>,
    /// A key event of the frame was swallowed.
    swallowed: bool,
    /// A mask key to tap when the frame ends, and the release it held back.
    mask: Mask,
    release: u16,
    /// Events were lost (`SYN_DROPPED`): skip to the next frame, then catch up.
    dropping: bool,
    /// The grab failed while in use: let go after this read.
    lost_grab: bool,
}

impl Keyboard {
    fn new(device: Device) -> Self {
        Self {
            down: KeyBits::default(),
            device,
            grab: Grab::No,
            refused: None,
            frame: Vec::new(),
            swallowed: false,
            mask: Mask::None,
            release: 0,
            dropping: false,
            lost_grab: false,
        }
    }

    /// `held` with this keyboard's modifier keys added.
    fn held(&self, held: Held) -> Held {
        codes::modifier_codes()
            .filter(|&(code, _)| self.down.has(code))
            .fold(held, |held, (_, key)| held.with(key))
    }

    fn settle(&mut self, wanted: bool) {
        match (&self.grab, wanted) {
            (Grab::No, true) => self.try_grab(),
            (Grab::Refused { problem, at }, true)
                if *problem != Problem::Busy && at.elapsed() >= RETRY =>
            {
                self.try_grab();
            }
            (Grab::Held { .. }, false) if self.down.is_empty() => self.ungrab(),
            (Grab::Refused { .. }, false) => self.grab = Grab::No,
            _ => {}
        }
    }

    fn try_grab(&mut self) {
        // A key down now would come up through the clone unbalanced: wait for its release.
        if !evdev::keys_down(&self.device.fd).is_ok_and(|down| down.is_empty()) {
            return;
        }
        self.grab = match self.grab_with_clone() {
            Ok(grab) => {
                log::info!(
                    "hotkey: holding {} to keep the shortcut from apps",
                    self.device.name
                );
                self.refused = None;
                grab
            }
            Err(problem) => {
                if self.refused.as_ref() != Some(&problem) {
                    log::info!("hotkey: not holding {}: {problem:?}", self.device.name);
                }
                if problem == Problem::Busy {
                    // Its keys go to the other program now, so ours would never come up.
                    self.down = KeyBits::default();
                }
                self.refused = Some(problem.clone());
                Grab::Refused {
                    problem,
                    at: Instant::now(),
                }
            }
        };
    }

    /// Set up the clone first so no grab happens without one; it only appears once the grab
    /// holds, so another grabber never sees it.
    fn grab_with_clone(&self) -> Result<Grab, Problem> {
        let set_up = Uinput::set_up(&self.device).map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound => Problem::NoUinput,
            _ => Problem::Failed(format!("creating a virtual keyboard: {e}")),
        })?;
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let since = now.tv_sec * 1_000_000 + now.tv_nsec / 1000;
        self.device.grab(true).map_err(|e| match e.raw_os_error() {
            Some(code) if code == rustix::io::Errno::BUSY.raw_os_error() => Problem::Busy,
            _ => Problem::Failed(format!("grabbing the keyboard: {e}")),
        })?;
        match set_up.create() {
            Ok(clone) => Ok(Grab::Held { clone, since }),
            Err(e) => {
                let _ = self.device.grab(false);
                Err(Problem::Failed(format!("creating a virtual keyboard: {e}")))
            }
        }
    }

    fn ungrab(&mut self) {
        if matches!(self.grab, Grab::Held { .. }) {
            let _ = self.device.grab(false);
            log::info!("hotkey: let go of {}", self.device.name);
        }
        // Dropping the clone destroys it.
        self.grab = Grab::No;
    }

    /// One event; false once `events` wants no more.
    fn event(
        &mut self,
        event: InputEvent,
        others: Held,
        shared: &Shared,
        events: &mut dyn FnMut(Event) -> bool,
    ) -> bool {
        match (event.kind, event.code) {
            (EV_SYN, SYN_DROPPED) => {
                self.dropping = true;
                self.reset_frame();
            }
            (EV_SYN, SYN_REPORT) if self.dropping => {
                self.dropping = false;
                return self.resync(others, shared, events);
            }
            (EV_SYN, SYN_REPORT) => self.end_frame(event),
            _ if self.dropping => {}
            (EV_KEY, _) => return self.key(event, others, shared, events),
            _ => self.frame.push(event),
        }
        true
    }

    fn key(
        &mut self,
        event: InputEvent,
        others: Held,
        shared: &Shared,
        events: &mut dyn FnMut(Event) -> bool,
    ) -> bool {
        let edge = match event.value {
            0 => Edge::Up,
            1 => Edge::Down,
            2 => Edge::Repeat,
            _ => {
                self.frame.push(event);
                return true;
            }
        };
        let key = codes::key(event.code);
        let held = self.held(others).besides(key);
        self.down.set(event.code, edge != Edge::Up);
        let action = shared.edge(key, edge, held);
        let running = action.event.is_none_or(&mut *events);
        if action.swallow && matches!(self.grab, Grab::Held { .. }) {
            self.swallowed = true;
            if action.mask != Mask::None {
                self.mask = action.mask;
                self.release = event.code;
            }
        } else {
            self.frame.push(event);
        }
        running
    }

    /// Pass the frame on through the clone, with a mask key if one is due.
    fn end_frame(&mut self, syn: InputEvent) {
        if let Grab::Held { clone, since } = &self.grab {
            if syn.micros() < *since {
                // The compositor saw this frame before the grab: a key that
                // went down then must come up through the keyboard too.
                if self.frame.iter().any(|e| e.kind == EV_KEY && e.value == 1) {
                    self.lost_grab = true;
                }
            } else {
                let out = passed_on(&self.frame, self.swallowed, self.mask, self.release, syn);
                if !out.is_empty()
                    && let Err(e) = clone.write(&out)
                {
                    log::warn!("hotkey: passing keys on from {}: {e}", self.device.name);
                    self.lost_grab = true;
                }
            }
        }
        self.reset_frame();
    }

    fn reset_frame(&mut self) {
        self.frame.clear();
        self.swallowed = false;
        self.mask = Mask::None;
    }

    /// After lost events, sync the keys down with the kernel and let the clone catch up.
    fn resync(
        &mut self,
        others: Held,
        shared: &Shared,
        events: &mut dyn FnMut(Event) -> bool,
    ) -> bool {
        let Ok(now) = evdev::keys_down(&self.device.fd) else {
            return true;
        };
        let mut running = true;
        let mut caught_up = Vec::new();
        let released: Vec<u16> = self.down.iter().filter(|&code| !now.has(code)).collect();
        for code in released {
            let key = codes::key(code);
            let held = self.held(others).besides(key);
            self.down.set(code, false);
            let action = shared.edge(key, Edge::Up, held);
            running &= action.event.is_none_or(&mut *events);
            caught_up.push(InputEvent::new(EV_KEY, code, 0));
        }
        for code in now.iter().filter(|&code| !self.down.has(code)) {
            caught_up.push(InputEvent::new(EV_KEY, code, 1));
        }
        self.down = now;
        if let Grab::Held { clone, .. } = &self.grab
            && !caught_up.is_empty()
        {
            caught_up.push(InputEvent::syn());
            if clone.write(&caught_up).is_err() {
                self.lost_grab = true;
            }
        }
        running
    }

    /// The keyboard is gone: whatever was down on it goes up.
    fn release_all(
        mut self,
        others: Held,
        shared: &Shared,
        events: &mut dyn FnMut(Event) -> bool,
    ) -> bool {
        let down: Vec<u16> = self.down.iter().collect();
        let mut running = true;
        for code in down {
            let key = codes::key(code);
            let held = self.held(others).besides(key);
            self.down.set(code, false);
            running &= shared
                .edge(key, Edge::Up, held)
                .event
                .is_none_or(&mut *events);
        }
        running
    }
}

/// What the clone sends for one frame: what's left of it, then any mask tap and held-back release.
fn passed_on(
    frame: &[InputEvent],
    swallowed: bool,
    mask: Mask,
    release: u16,
    syn: InputEvent,
) -> Vec<InputEvent> {
    let mut out = Vec::with_capacity(frame.len() + 7);
    // A frame that lost its key keeps nothing worth sending (a scan code).
    if !swallowed || frame.iter().any(|e| e.kind == EV_KEY) {
        out.extend_from_slice(frame);
        out.push(syn);
    }
    if mask != Mask::None {
        out.extend([
            InputEvent::new(EV_KEY, codes::KEY_LEFTCTRL, 1),
            InputEvent::syn(),
            InputEvent::new(EV_KEY, codes::KEY_LEFTCTRL, 0),
            InputEvent::syn(),
        ]);
    }
    if mask == Mask::Release {
        out.extend([InputEvent::new(EV_KEY, release, 0), InputEvent::syn()]);
    }
    out
}

#[cfg(test)]
mod tests;
