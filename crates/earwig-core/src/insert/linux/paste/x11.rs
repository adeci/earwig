//! The clipboard and the primary selection where the compositor offers no
//! data-control (GNOME): earwig owns the X11 CLIPBOARD and PRIMARY selections
//! from an unmapped window, and Mutter hands them to Wayland clients, with no
//! window of earwig's focused. Served from its own thread, so they outlive the
//! paste.

use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use x11rb::NONE;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Timestamp, Window,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::super::MIME_TYPES;

/// Longest wait for the compositor to fetch new text before the chord goes
/// out anyway; Mutter fetches it within milliseconds.
const FETCH_TIMEOUT: Duration = Duration::from_millis(500);
/// Longest wait for the X server.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct X11Clipboard {
    /// `None` only while dropping, so the thread sees the channel close.
    jobs: Option<flume::Sender<Job>>,
    wake: Arc<OwnedFd>,
}

struct Job {
    text: Arc<str>,
    primary: bool,
    done: flume::Sender<anyhow::Result<()>>,
}

impl X11Clipboard {
    /// Connect to `$DISPLAY` and serve the clipboard from a new thread.
    pub(super) fn connect() -> anyhow::Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("connecting to the X server")?;
        let owner = Owner::new(conn, screen)?;
        let wake = Arc::new(
            rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
                .context("eventfd")?,
        );
        let (jobs, jobs_rx) = flume::unbounded();
        let thread_wake = wake.clone();
        std::thread::Builder::new()
            .name("earwig-x11-clipboard".into())
            .spawn(move || {
                if let Err(err) = owner.serve(&jobs_rx, &thread_wake) {
                    log::warn!("the X11 clipboard stopped: {err:#}");
                }
            })
            .context("spawning the X11 clipboard thread")?;
        Ok(Self {
            jobs: Some(jobs),
            wake,
        })
    }

    /// Own the clipboard with `text`, and the primary selection too with
    /// `primary`; returns once the compositor fetched it.
    pub(super) fn set(&self, text: &str, primary: bool) -> anyhow::Result<()> {
        let (done, done_rx) = flume::bounded(1);
        self.jobs
            .as_ref()
            .expect("jobs is only taken on drop")
            .send(Job {
                text: text.into(),
                primary,
                done,
            })
            .map_err(|_| anyhow!("the X11 clipboard thread is gone"))?;
        self.wake();
        done_rx
            .recv_timeout(2 * REPLY_TIMEOUT + FETCH_TIMEOUT)
            .map_err(|err| match err {
                flume::RecvTimeoutError::Timeout => anyhow!("the X server did not answer"),
                flume::RecvTimeoutError::Disconnected => {
                    anyhow!("the X11 clipboard thread is gone")
                }
            })?
    }

    fn wake(&self) {
        // Only fails when the counter would overflow, which still wakes.
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
    }
}

impl Drop for X11Clipboard {
    fn drop(&mut self) {
        self.jobs = None;
        self.wake();
    }
}

struct Atoms {
    clipboard: Atom,
    primary: Atom,
    targets: Atom,
    timestamp: Atom,
    utf8_string: Atom,
    text: Atom,
    /// Appended to on our window for a server timestamp.
    stamp: Atom,
    /// [`MIME_TYPES`] in order.
    text_targets: Vec<Atom>,
}

/// The unmapped window that owns CLIPBOARD and PRIMARY, and what it serves.
struct Owner {
    conn: RustConnection,
    window: Window,
    atoms: Atoms,
    /// The text CLIPBOARD holds while the window owns it.
    clipboard: Option<Arc<str>>,
    /// The text PRIMARY holds while the window owns it.
    primary: Option<Arc<str>>,
    owned_at: Timestamp,
    /// Whether a client read the text since it was set.
    fetched: bool,
    /// Time of the last change to our window's stamp property.
    stamped: Option<Timestamp>,
}

impl Owner {
    fn new(conn: RustConnection, screen: usize) -> anyhow::Result<Self> {
        let intern = |name: &str| conn.intern_atom(false, name.as_bytes());
        let names = [
            "CLIPBOARD",
            "TARGETS",
            "TIMESTAMP",
            "UTF8_STRING",
            "TEXT",
            "EARWIG_TIMESTAMP",
        ];
        // Ask for every atom before waiting for the first.
        let cookies = names
            .iter()
            .chain(MIME_TYPES)
            .map(|&name| intern(name))
            .collect::<Result<Vec<_>, _>>()?;
        let mut atoms = cookies
            .into_iter()
            .map(|cookie| Ok(cookie.reply()?.atom))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("interning X11 atoms")?;
        let text_targets = atoms.split_off(names.len());
        let [clipboard, targets, timestamp, utf8_string, text, stamp] = atoms[..] else {
            unreachable!("one atom per name");
        };
        let root = conn
            .setup()
            .roots
            .get(screen)
            .context("the X server has no such screen")?
            .root;
        let window = conn.generate_id()?;
        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        conn.flush()?;
        Ok(Self {
            conn,
            window,
            atoms: Atoms {
                clipboard,
                primary: AtomEnum::PRIMARY.into(),
                targets,
                timestamp,
                utf8_string,
                text,
                stamp,
                text_targets,
            },
            clipboard: None,
            primary: None,
            owned_at: 0,
            fetched: false,
            stamped: None,
        })
    }

    fn serve(mut self, jobs: &flume::Receiver<Job>, wake: &OwnedFd) -> anyhow::Result<()> {
        loop {
            self.handle_events()?;
            loop {
                match jobs.try_recv() {
                    Ok(job) => {
                        let result = self.set(job.text, job.primary);
                        let _ = job.done.send(result);
                    }
                    Err(flume::TryRecvError::Empty) => break,
                    Err(flume::TryRecvError::Disconnected) => return Ok(()),
                }
            }
            self.conn.flush()?;
            if self.poll(Some(wake), None)? {
                let _ = rustix::io::read(wake, &mut [0u8; 8]);
            }
        }
    }

    fn set(&mut self, text: Arc<str>, primary: bool) -> anyhow::Result<()> {
        let time = self.server_time()?;
        self.owned_at = time;
        self.fetched = false;
        self.clipboard = Some(text.clone());
        self.own(self.atoms.clipboard, time)?;
        if primary {
            self.primary = Some(text);
            self.own(self.atoms.primary, time)?;
        }
        if !self.serve_until(|owner| owner.fetched, FETCH_TIMEOUT)? {
            log::debug!("nothing fetched the clipboard within {FETCH_TIMEOUT:?}");
        }
        Ok(())
    }

    fn own(&self, selection: Atom, time: Timestamp) -> anyhow::Result<()> {
        self.conn
            .set_selection_owner(self.window, selection, time)?;
        let owner = self.conn.get_selection_owner(selection)?.reply()?.owner;
        anyhow::ensure!(owner == self.window, "another program kept the selection");
        Ok(())
    }

    /// The X server's time now, from a change to our own window (ICCCM: no
    /// `CurrentTime` for selections).
    fn server_time(&mut self) -> anyhow::Result<Timestamp> {
        self.stamped = None;
        self.conn.change_property8(
            PropMode::APPEND,
            self.window,
            self.atoms.stamp,
            AtomEnum::STRING,
            &[],
        )?;
        self.serve_until(|owner| owner.stamped.is_some(), REPLY_TIMEOUT)?;
        self.stamped.context("the X server sent no timestamp")
    }

    /// Handle events until `done` holds or `limit` passed; whether it held.
    fn serve_until(
        &mut self,
        done: impl Fn(&Self) -> bool,
        limit: Duration,
    ) -> anyhow::Result<bool> {
        let deadline = Instant::now() + limit;
        loop {
            self.handle_events()?;
            if done(self) {
                return Ok(true);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(false);
            }
            self.conn.flush()?;
            self.poll(None, Some(left))?;
        }
    }

    /// Wait until the X connection (or `wake`) is readable or `limit` passed;
    /// whether `wake` was.
    fn poll(&self, wake: Option<&OwnedFd>, limit: Option<Duration>) -> anyhow::Result<bool> {
        let timeout = limit.map(|limit| Timespec {
            tv_sec: i64::try_from(limit.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(limit.subsec_nanos()),
        });
        let conn = self.conn.stream().as_fd();
        let (mut one, mut both);
        let fds: &mut [PollFd] = if let Some(wake) = wake {
            both = [
                PollFd::new(&conn, PollFlags::IN),
                PollFd::new(wake, PollFlags::IN),
            ];
            &mut both
        } else {
            one = [PollFd::new(&conn, PollFlags::IN)];
            &mut one
        };
        loop {
            match rustix::event::poll(fds, timeout.as_ref()) {
                Ok(_) => break,
                Err(Errno::INTR) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(fds.get(1).is_some_and(|fd| !fd.revents().is_empty()))
    }

    fn handle_events(&mut self) -> anyhow::Result<()> {
        while let Some(event) = self.conn.poll_for_event()? {
            match event {
                Event::PropertyNotify(event)
                    if event.window == self.window && event.atom == self.atoms.stamp =>
                {
                    self.stamped = Some(event.time);
                }
                Event::SelectionRequest(request) => self.answer(&request)?,
                Event::SelectionClear(event) if event.selection == self.atoms.clipboard => {
                    self.clipboard = None;
                }
                Event::SelectionClear(event) if event.selection == self.atoms.primary => {
                    self.primary = None;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Put what `request` asks for on its window, then tell it.
    fn answer(&mut self, request: &SelectionRequestEvent) -> anyhow::Result<()> {
        // Obsolete clients name no property.
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let text = if request.selection == self.atoms.clipboard {
            self.clipboard.clone()
        } else if request.selection == self.atoms.primary {
            self.primary.clone()
        } else {
            None
        };
        let served = text.is_some_and(|text| {
            match self.reply(&text, request.requestor, property, request.target) {
                Ok(served) => served,
                Err(err) => {
                    log::warn!("could not serve the clipboard: {err}");
                    false
                }
            }
        });
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { NONE },
        };
        self.conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Whether `target` of `text` could be written to `property` of `requestor`.
    fn reply(
        &mut self,
        text: &str,
        requestor: Window,
        property: Atom,
        target: Atom,
    ) -> Result<bool, x11rb::errors::ConnectionError> {
        let atoms = &self.atoms;
        if target == atoms.targets {
            let mut offered = vec![atoms.targets, atoms.timestamp];
            offered.extend(&atoms.text_targets);
            self.conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::ATOM,
                &offered,
            )?;
        } else if target == atoms.timestamp {
            self.conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::INTEGER,
                &[self.owned_at],
            )?;
        } else if atoms.text_targets.contains(&target) {
            // TEXT names a choice of encodings; the reply says which.
            let kind = if target == atoms.text {
                atoms.utf8_string
            } else {
                target
            };
            self.conn.change_property8(
                PropMode::REPLACE,
                requestor,
                property,
                kind,
                text.as_bytes(),
            )?;
            self.fetched = true;
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
