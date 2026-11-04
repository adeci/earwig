//! Insertion through the XDG RemoteDesktop (version 2) and Clipboard portals
//! and libei: any desktop whose portal backend implements them (GNOME, KDE).
//! No desktop names are checked; every portal step before a dialog is time
//! bounded, so a backend that is advertised but cannot work on this
//! compositor fails fast instead of blocking.
//!
//! One RemoteDesktop session with a keyboard and clipboard access is opened
//! when the daemon starts. The user approves it once; the restore token in
//! `<state_dir>/remote-desktop.token` brings it back without a dialog. Keys
//! go over libei (`ConnectToEIS`) rather than the portal's per-key D-Bus
//! calls, so a whole transcript leaves in one socket write.
//!
//! The portal and libei are async. They live on a dedicated thread with its
//! own executor, which also answers clipboard transfers and EIS pings between
//! dictations; [`PortalInserter`] talks to it over a channel and waits for
//! each answer with a timeout.
//!
//! Methods, in default order:
//! - [`InsertMethod::PortalPaste`]: clipboard set through the portal, then one
//!   paste chord. Atomic and covers every character; the text stays on the
//!   clipboard. The portal exposes no focused app id, so terminals are
//!   recognized over the accessibility bus ([`focus`]) and get Ctrl+Shift+V.
//! - [`InsertMethod::PortalType`]: every character as a key press, all in one
//!   burst. Used when clipboard access was not granted. Apps redraw per key,
//!   so long text visibly types out. Not applicable when a character is
//!   missing from the keyboard layout or Caps Lock is on.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::pin::Pin;
use std::sync::LazyLock;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use ashpd::desktop::clipboard::{Clipboard, SetSelectionOptions};
use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::{PersistMode, Session};
use async_io::{Async, Timer};
use futures_lite::{AsyncWriteExt as _, Stream, StreamExt as _, future};
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use xkbcommon::xkb;

use self::focus::FocusWatch;
use super::{InsertError, Inserter};
use crate::events::InsertMethod;

mod focus;

/// The id portals know earwig by. A desktop file with this basename must be
/// installed: GNOME rejects portal requests from host apps without one.
pub const APP_ID: &str = "io.github.adeci.earwig";

const TOKEN_FILE: &str = "remote-desktop.token";

/// Paste first. Measured in GNOME 50 (VM, GTK 4 text view, 500 characters):
/// paste complete in the app after ~45 ms, typing after ~2.5 s, with every
/// character drawn on its own; a burst into a ~2000 character paragraph made
/// the GTK client drop its Wayland connection.
const DEFAULT_ORDER: [InsertMethod; 2] = [InsertMethod::PortalPaste, InsertMethod::PortalType];

/// How long [`PortalInserter`] waits for the portal thread to finish one job.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the EIS server may take to hand out a keyboard after `Start`.
const EIS_SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Portal steps without user interaction.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// `Start` while the consent dialog waits for the user.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Close the session after this long without requests: GNOME shows a
/// "remote control" indicator while it is open.
const IDLE_CLOSE: Duration = Duration::from_secs(60);

const TEXT_MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// Run a portal step, failing after `limit`.
pub(crate) async fn bounded<T>(
    limit: Duration,
    what: &str,
    step: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    future::or(step, async {
        Timer::after(limit).await;
        Err(anyhow!("no answer within {limit:?}"))
    })
    .await
    .with_context(|| what.to_owned())
}

/// Tell xdg-desktop-portal which app this process is
/// (`org.freedesktop.host.portal.Registry.Register`). Since
/// xdg-desktop-portal 1.21 host apps without an app id are refused by
/// GlobalShortcuts, and GNOME keys RemoteDesktop restore data by it.
///
/// It has to be the first portal call on the process-wide D-Bus connection
/// and may happen only once, so every portal user calls this first; later
/// calls return immediately.
pub(crate) fn register_app_id() {
    static REGISTERED: LazyLock<()> = LazyLock::new(|| {
        let result = async_io::block_on(async {
            let app_id = ashpd::AppID::try_from(APP_ID)?;
            ashpd::register_host_app(app_id).await
        });
        match result {
            Ok(()) => log::debug!("registered with the portal as {APP_ID}"),
            // Portals before 1.20 have no registry and identify host apps by
            // their systemd unit instead.
            Err(e) => log::warn!("could not register app id {APP_ID} with the portal: {e}"),
        }
    });
    LazyLock::force(&REGISTERED);
}

/// A RemoteDesktop session with a keyboard over libei, plus clipboard access.
/// The user approves it once; the restore token in `state_dir` keeps it.
///
/// GNOME shows a "remote control" indicator while the session is open, so it
/// closes after [`IDLE_CLOSE`] without requests and is restored (no dialog,
/// ~80 ms) by the next one.
pub struct PortalInserter {
    requests: flume::Sender<Request>,
    order: Vec<InsertMethod>,
}

enum Job {
    Type(String),
    Paste(String),
    Copy(String),
}

struct Request {
    job: Job,
    reply: flume::Sender<Result<(), InsertError>>,
}

impl PortalInserter {
    /// Open (or restore) the portal session. Called at daemon start so the
    /// consent dialog never interrupts a dictation. Blocks until the user
    /// answers the dialog (if one is shown) and libei offers a keyboard.
    pub fn connect(state_dir: &Path) -> anyhow::Result<Self> {
        register_app_id();
        let token_file = state_dir.join(TOKEN_FILE);
        let (ready_tx, ready_rx) = flume::bounded(1);
        let (requests, requests_rx) = flume::unbounded();
        thread::Builder::new()
            .name("earwig-portal".into())
            .spawn(move || async_io::block_on(run(&token_file, &ready_tx, &requests_rx)))
            .context("spawning the portal thread")?;
        let clipboard = ready_rx
            .recv()
            .context("the portal thread exited during setup")??;
        if !clipboard {
            log::warn!("clipboard access not granted; portal paste is unavailable");
        }
        let order = DEFAULT_ORDER
            .into_iter()
            .filter(|m| clipboard || *m != InsertMethod::PortalPaste)
            .collect();
        Ok(Self { requests, order })
    }

    /// Insert with exactly `method`, without falling back to the other one.
    pub fn insert_with(&mut self, method: InsertMethod, text: &str) -> Result<(), InsertError> {
        let job = match method {
            InsertMethod::PortalType => Job::Type(text.to_owned()),
            InsertMethod::PortalPaste => Job::Paste(text.to_owned()),
            other => {
                return Err(InsertError::NotApplicable(format!(
                    "{other:?} is not a portal method"
                )));
            }
        };
        self.call(job)
    }

    fn call(&self, job: Job) -> Result<(), InsertError> {
        let (reply, reply_rx) = flume::bounded(1);
        self.requests
            .send(Request { job, reply })
            .map_err(|_| anyhow!("the portal thread is gone"))?;
        match reply_rx.recv_timeout(REPLY_TIMEOUT) {
            Ok(result) => result,
            Err(flume::RecvTimeoutError::Timeout) => Err(InsertError::Failed(anyhow!(
                "the portal did not answer within {REPLY_TIMEOUT:?}"
            ))),
            Err(flume::RecvTimeoutError::Disconnected) => {
                Err(InsertError::Failed(anyhow!("the portal thread is gone")))
            }
        }
    }
}

/// The portal thread: serve sessions while watching which widget has focus.
async fn run(
    token_file: &Path,
    ready: &flume::Sender<anyhow::Result<bool>>,
    requests: &flume::Receiver<Request>,
) {
    let terminal = Cell::new(false);
    future::or(
        sessions(token_file, ready, requests, &terminal),
        watch_focus(&terminal),
    )
    .await;
}

/// Keep `terminal` up to date; runs until the thread ends.
async fn watch_focus(terminal: &Cell<bool>) {
    match FocusWatch::connect().await {
        Ok(mut watch) => {
            while let Some(focused) = watch.next().await {
                terminal.set(watch.is_terminal(&focused).await);
            }
            log::info!("the accessibility bus went away; terminals now get Ctrl+V");
        }
        Err(e) => log::info!("no accessibility bus, terminals get Ctrl+V: {e:#}"),
    }
    terminal.set(false);
    future::pending::<()>().await;
}

/// Connect, serve requests, close when idle or lost, and reconnect for the
/// next request.
async fn sessions(
    token_file: &Path,
    ready: &flume::Sender<anyhow::Result<bool>>,
    requests: &flume::Receiver<Request>,
    terminal: &Cell<bool>,
) {
    let (mut portal, mut worker, mut events) = match Worker::open(token_file).await {
        Ok(opened) => opened,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    if ready.send(Ok(portal.clipboard.is_some())).is_err() {
        return;
    }
    let mut pending = None;
    loop {
        let end = worker
            .serve(&portal, events, requests, pending.take(), terminal)
            .await;
        let _ = portal.session.close().await;
        match end {
            End::Shutdown => return,
            End::Idle => log::debug!("RemoteDesktop session closed after {IDLE_CLOSE:?} idle"),
            End::Lost(reason) => log::warn!("RemoteDesktop session lost: {reason}"),
        }
        // Reconnect when the next request comes.
        loop {
            let Ok(request) = requests.recv_async().await else {
                return;
            };
            match Worker::open(token_file).await {
                Ok(opened) => {
                    (portal, worker, events) = opened;
                    pending = Some(request);
                    break;
                }
                Err(e) => {
                    let _ = request.reply.send(Err(InsertError::Failed(e)));
                }
            }
        }
    }
}

impl Inserter for PortalInserter {
    fn methods(&self) -> Vec<InsertMethod> {
        self.order.clone()
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        let mut reasons = Vec::new();
        for method in self.order.clone() {
            match self.insert_with(method, text) {
                Ok(()) => return Ok(method),
                Err(InsertError::NotApplicable(reason)) => {
                    log::debug!("{method:?} not applicable: {reason}");
                    reasons.push(reason);
                }
                Err(e) => return Err(e),
            }
        }
        Err(InsertError::NotApplicable(reasons.join("; ")))
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        self.call(Job::Copy(text.to_owned()))
    }
}

/// The portal objects. Kept apart from [`Worker`] because the signal streams
/// borrow them while the worker changes.
struct Portal {
    session: Session<RemoteDesktop>,
    /// `Some` when clipboard access was granted.
    clipboard: Option<Clipboard>,
}

/// What the portal thread owns besides [`Portal`].
struct Worker {
    ei: reis::event::Connection,
    /// A second handle on the EIS socket to wait for room when a long burst
    /// fills the socket buffer.
    socket: Async<OwnedFd>,
    keyboard: Option<Keyboard>,
    /// What we offer on the clipboard.
    selection: Option<String>,
    sequence: u32,
    /// Set when libei reports a disconnect.
    dead: Option<String>,
}

enum Input {
    Request(Request),
    Shutdown,
    Idle,
    Ei(Option<Result<EiEvent, reis::Error>>),
    Transfer(Option<u32>),
    Closed(Option<()>),
}

/// Why [`Worker::serve`] returned.
enum End {
    Shutdown,
    Idle,
    Lost(String),
}

/// Ask for clipboard access in the RemoteDesktop session. `None` when the
/// portal lacks the Clipboard interface; paste is then unavailable.
async fn request_clipboard(session: &Session<RemoteDesktop>) -> Option<Clipboard> {
    let clipboard = Clipboard::new()
        .await
        .inspect_err(|e| log::warn!("the Clipboard portal is not available: {e}"))
        .ok()?;
    clipboard
        .request(
            session,
            ashpd::desktop::clipboard::RequestClipboardOptions::default(),
        )
        .await
        .inspect_err(|e| log::warn!("requesting clipboard access failed: {e}"))
        .ok()?;
    Some(clipboard)
}

impl Worker {
    async fn open(
        token_file: &Path,
    ) -> anyhow::Result<(Portal, Self, reis::async_io::EiConvertEventStream)> {
        let token = fs::read_to_string(token_file)
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
        // No user interaction before `Start`: a backend that cannot work on
        // this compositor fails or hangs here, before any dialog.
        let (remote, session, clipboard) = bounded(
            SETUP_TIMEOUT,
            "RemoteDesktop setup",
            create_session(token.as_deref()),
        )
        .await?;

        // With a restore token there is no dialog.
        let start_timeout = if token.is_some() {
            SETUP_TIMEOUT
        } else {
            CONSENT_TIMEOUT
        };
        let started = bounded(start_timeout, "starting the RemoteDesktop session", async {
            Ok(remote
                .start(
                    &session,
                    None,
                    ashpd::desktop::remote_desktop::StartOptions::default(),
                )
                .await?
                .response()?)
        })
        .await
        .context("was the dialog declined?")?;
        if !started.devices().contains(DeviceType::Keyboard) {
            bail!("keyboard access was not granted");
        }
        match started.restore_token() {
            Some(token) => {
                if let Err(e) = save_token(token_file, token) {
                    log::warn!("saving {}: {e:#}", token_file.display());
                }
            }
            None => log::debug!("the portal returned no restore token"),
        }
        let clipboard = clipboard.filter(|_| started.is_clipboard_enabled());

        let (socket, ei, events) = bounded(SETUP_TIMEOUT, "connecting to EIS", async {
            let fd = remote
                .connect_to_eis(
                    &session,
                    ashpd::desktop::remote_desktop::ConnectToEISOptions::default(),
                )
                .await?;
            let socket = Async::new(fd.try_clone()?)?;
            let context = ei::Context::new(UnixStream::from(fd))?;
            let (ei, events) = context
                .handshake_async_io("earwig", ei::handshake::ContextType::Sender)
                .await?;
            Ok((socket, ei, events))
        })
        .await?;

        let mut worker = Worker {
            ei,
            socket,
            keyboard: None,
            selection: None,
            sequence: 0,
            dead: None,
        };
        let mut events = events;
        future::or(
            async {
                while !worker.keyboard.as_ref().is_some_and(|k| k.resumed) {
                    match events.next().await {
                        Some(Ok(event)) => worker.on_ei(event)?,
                        Some(Err(e)) => return Err(anyhow!(e).context("libei")),
                        None => bail!("the EIS connection closed during setup"),
                    }
                    if let Some(reason) = &worker.dead {
                        bail!("{reason}");
                    }
                }
                Ok(())
            },
            async {
                Timer::after(EIS_SETUP_TIMEOUT).await;
                Err(anyhow!(
                    "no keyboard from the EIS server within {EIS_SETUP_TIMEOUT:?}"
                ))
            },
        )
        .await?;
        log::info!(
            "RemoteDesktop portal ready (keyboard over libei, clipboard {})",
            if clipboard.is_some() {
                "granted"
            } else {
                "not granted"
            }
        );
        Ok((Portal { session, clipboard }, worker, events))
    }

    /// Serve requests (`pending` first) until shutdown, [`IDLE_CLOSE`] without
    /// requests, or the session ends.
    async fn serve(
        &mut self,
        portal: &Portal,
        events: reis::async_io::EiConvertEventStream,
        requests: &flume::Receiver<Request>,
        pending: Option<Request>,
        terminal: &Cell<bool>,
    ) -> End {
        if let Some(Request { job, reply }) = pending {
            let _ = reply.send(self.run(portal, job, terminal.get()).await);
        }
        // `None` once a source has ended.
        let mut events = Some(events);
        let mut transfers: Option<Pin<Box<dyn Stream<Item = u32> + '_>>> = match &portal.clipboard {
            Some(clipboard) => match clipboard
                .receive_selection_transfer::<RemoteDesktop>()
                .await
            {
                Ok(stream) => Some(Box::pin(stream.map(|(_session, _mime, serial)| serial))),
                Err(e) => {
                    log::warn!("cannot serve clipboard requests: {e}");
                    None
                }
            },
            None => None,
        };
        let mut closed: Option<Pin<Box<dyn Stream<Item = ()> + '_>>> =
            match portal.session.receive_closed().await {
                Ok(stream) => Some(Box::pin(stream.map(|_| ()))),
                Err(e) => {
                    log::warn!("cannot watch the RemoteDesktop session: {e}");
                    None
                }
            };
        let mut idle = Timer::after(IDLE_CLOSE);
        loop {
            let input = future::or(
                async {
                    requests
                        .recv_async()
                        .await
                        .map_or(Input::Shutdown, Input::Request)
                },
                future::or(
                    async { Input::Ei(next_or_pending(&mut events).await) },
                    future::or(
                        async { Input::Transfer(next_or_pending(&mut transfers).await) },
                        future::or(
                            async { Input::Closed(next_or_pending(&mut closed).await) },
                            async {
                                (&mut idle).await;
                                Input::Idle
                            },
                        ),
                    ),
                ),
            )
            .await;
            match input {
                Input::Request(Request { job, reply }) => {
                    let _ = reply.send(self.run(portal, job, terminal.get()).await);
                    idle.set_after(IDLE_CLOSE);
                }
                Input::Shutdown => return End::Shutdown,
                Input::Idle => return End::Idle,
                Input::Ei(Some(Ok(event))) => {
                    if let Err(e) = self.on_ei(event) {
                        log::warn!("libei: {e:#}");
                    }
                    if let Some(reason) = self.dead.take() {
                        return End::Lost(reason);
                    }
                }
                Input::Ei(Some(Err(e))) => return End::Lost(format!("libei failed: {e}")),
                Input::Ei(None) => return End::Lost("the EIS connection closed".into()),
                Input::Transfer(Some(serial)) => {
                    if let Err(e) = self.transfer(portal, serial).await {
                        log::warn!("clipboard transfer failed: {e:#}");
                    }
                    idle.set_after(IDLE_CLOSE);
                }
                Input::Transfer(None) => transfers = None,
                Input::Closed(Some(())) => return End::Lost("the session was closed".into()),
                Input::Closed(None) => closed = None,
            }
        }
    }

    async fn run(&mut self, portal: &Portal, job: Job, terminal: bool) -> Result<(), InsertError> {
        match job {
            Job::Type(text) => {
                let keyboard = self.keyboard()?;
                if keyboard.caps_lock {
                    return Err(InsertError::NotApplicable("Caps Lock is on".into()));
                }
                let strokes = text
                    .chars()
                    .map(|c| keyboard.map.stroke(c))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        InsertError::NotApplicable(
                            "the text has characters the keyboard layout cannot type".into(),
                        )
                    })?;
                self.send_keys(&strokes).await?;
                Ok(())
            }
            Job::Paste(text) => {
                let chord = self.keyboard()?.map.paste(terminal).ok_or_else(|| {
                    InsertError::NotApplicable("the keyboard layout has no Control key".into())
                })?;
                self.set_selection(portal, text).await?;
                self.send_keys(&[chord]).await?;
                Ok(())
            }
            Job::Copy(text) => self.set_selection(portal, text).await,
        }
    }

    fn keyboard(&self) -> Result<&Keyboard, InsertError> {
        match &self.keyboard {
            Some(keyboard) if keyboard.resumed => Ok(keyboard),
            Some(_) => Err(InsertError::Failed(anyhow!(
                "the compositor paused the portal keyboard"
            ))),
            None => Err(InsertError::Failed(anyhow!(
                "the compositor offers no portal keyboard"
            ))),
        }
    }

    /// Send all strokes in one burst: one frame per key event (a logical
    /// hardware event each), one write.
    async fn send_keys(&mut self, strokes: &[Stroke]) -> anyhow::Result<()> {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        let keyboard = self.keyboard()?;
        let device = keyboard.device.device();
        let serial = self.ei.serial();
        device.start_emulating(serial, sequence);
        let mut events = Vec::with_capacity(8);
        for stroke in strokes {
            events.clear();
            keyboard.map.key_events(*stroke, &mut events);
            for &(key, down) in &events {
                let state = if down {
                    ei::keyboard::KeyState::Press
                } else {
                    ei::keyboard::KeyState::Released
                };
                keyboard.keys.key(key, state);
                device.frame(serial, now_us());
            }
        }
        device.stop_emulating(serial);
        self.flush().await
    }

    async fn flush(&self) -> anyhow::Result<()> {
        loop {
            match self.ei.flush() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let e = io::Error::from(e);
                    if e.kind() != io::ErrorKind::WouldBlock {
                        return Err(anyhow!(e).context("writing to the EIS socket"));
                    }
                    self.socket.writable().await?;
                }
            }
        }
    }

    async fn set_selection(&mut self, portal: &Portal, text: String) -> Result<(), InsertError> {
        let Some(clipboard) = &portal.clipboard else {
            return Err(InsertError::NotApplicable(
                "clipboard access was not granted".into(),
            ));
        };
        self.selection = Some(text);
        clipboard
            .set_selection(
                &portal.session,
                SetSelectionOptions::default().set_mime_types(TEXT_MIME_TYPES),
            )
            .await
            .context("setting the clipboard through the portal")?;
        Ok(())
    }

    /// An app pastes: write our selection into the pipe the portal hands us.
    async fn transfer(&self, portal: &Portal, serial: u32) -> anyhow::Result<()> {
        let Some(clipboard) = &portal.clipboard else {
            return Ok(());
        };
        let text = self.selection.as_deref().unwrap_or_default();
        let fd = clipboard.selection_write(&portal.session, serial).await?;
        let written = async {
            let mut pipe = Async::new(fs::File::from(OwnedFd::from(fd)))?;
            pipe.write_all(text.as_bytes()).await?;
            pipe.flush().await
        }
        .await;
        clipboard
            .selection_write_done(&portal.session, serial, written.is_ok())
            .await?;
        written.context("writing the clipboard text")
    }

    fn on_ei(&mut self, event: EiEvent) -> anyhow::Result<()> {
        match event {
            EiEvent::SeatAdded(added) => {
                added
                    .seat
                    .bind_capabilities(DeviceCapability::Keyboard.into());
                self.ei.flush().map_err(io::Error::from)?;
            }
            EiEvent::DeviceAdded(added) => {
                if self.keyboard.is_none()
                    && added.device.has_capability(DeviceCapability::Keyboard)
                {
                    self.keyboard = Some(Keyboard::new(added.device)?);
                }
            }
            EiEvent::DeviceRemoved(removed) => {
                if self.is_keyboard(&removed.device) {
                    self.keyboard = None;
                }
            }
            EiEvent::DevicePaused(paused) => {
                if self.is_keyboard(&paused.device) {
                    self.keyboard.as_mut().expect("checked").resumed = false;
                }
            }
            EiEvent::DeviceResumed(resumed) => {
                if self.is_keyboard(&resumed.device) {
                    self.keyboard.as_mut().expect("checked").resumed = true;
                }
            }
            EiEvent::KeyboardModifiers(mods) => {
                if self.is_keyboard(&mods.device) {
                    let keyboard = self.keyboard.as_mut().expect("checked");
                    keyboard.caps_lock = mods.locked & keyboard.caps_mask != 0;
                    if mods.group != keyboard.map.group {
                        keyboard.map = CharMap::new(&keyboard.keymap, mods.group);
                    }
                }
            }
            EiEvent::Disconnected(disconnected) => {
                self.dead = Some(format!(
                    "the EIS server disconnected ({:?}{})",
                    disconnected.reason,
                    disconnected
                        .explanation
                        .map(|e| format!(": {e}"))
                        .unwrap_or_default()
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn is_keyboard(&self, device: &reis::event::Device) -> bool {
        self.keyboard.as_ref().is_some_and(|k| k.device == *device)
    }
}

async fn next_or_pending<S: Stream + Unpin>(stream: &mut Option<S>) -> Option<S::Item> {
    match stream {
        Some(stream) => stream.next().await,
        None => future::pending().await,
    }
}

fn save_token(path: &Path, token: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, token)?;
    Ok(())
}

/// libei timestamps: CLOCK_MONOTONIC in microseconds.
fn now_us() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(now.tv_sec).unwrap_or_default() * 1_000_000
        + u64::try_from(now.tv_nsec).unwrap_or_default() / 1_000
}

/// The libei keyboard and what its layout can type.
struct Keyboard {
    device: reis::event::Device,
    keys: ei::Keyboard,
    keymap: xkb::Keymap,
    map: CharMap,
    caps_mask: xkb::ModMask,
    caps_lock: bool,
    resumed: bool,
}

impl Keyboard {
    fn new(device: reis::event::Device) -> anyhow::Result<Self> {
        let keys = device
            .interface::<ei::Keyboard>()
            .context("keyboard device without ei_keyboard")?;
        let keymap = device.keymap().context("the EIS keyboard has no keymap")?;
        if keymap.type_ != ei::keyboard::KeymapType::Xkb {
            bail!("the EIS keyboard keymap is not XKB");
        }
        let fd = keymap.fd.try_clone()?;
        let size = usize::try_from(keymap.size)?;
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        // SAFETY: the EIS server hands out a keymap file of exactly `size`
        // bytes; libxkbcommon maps it read-only.
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(
                &context,
                fd,
                size,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        }?
        .context("compiling the EIS keymap")?;
        let map = CharMap::new(&keymap, 0);
        let caps_mask = 1 << keymap.mod_get_index(xkb::MOD_NAME_CAPS);
        Ok(Self {
            device,
            keys,
            keymap,
            map,
            caps_mask,
            caps_lock: false,
            resumed: false,
        })
    }
}

const SHIFT: u8 = 1;
const LEVEL3: u8 = 2;
const CONTROL: u8 = 4;

/// One key press with the modifiers held around it. Keys are evdev codes
/// (XKB keycode - 8), what libei sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stroke {
    key: u32,
    mods: u8,
}

/// Which key (and modifiers) types each character in one layout group.
struct CharMap {
    group: u32,
    chars: HashMap<char, Stroke>,
    shift: Option<u32>,
    level3: Option<u32>,
    control: Option<u32>,
}

/// evdev `KEY_V`, for Ctrl+V on layouts without a Latin `v`.
const KEY_V: u32 = 47;

impl CharMap {
    /// Simulate every key under no modifier, Shift, AltGr (level 3) and
    /// Shift+AltGr, and keep the first (fewest modifiers) way to get each
    /// character.
    fn new(keymap: &xkb::Keymap, group: u32) -> Self {
        let evdev = |code: xkb::Keycode| code.raw() - 8;
        let shift = find_key(keymap, xkb::Keysym::Shift_L);
        let level3 = find_key(keymap, xkb::Keysym::ISO_Level3_Shift);
        let control = find_key(keymap, xkb::Keysym::Control_L);
        let modifier_keys = [shift, level3, control];

        let mut chars = HashMap::new();
        for mods in [0, SHIFT, LEVEL3, SHIFT | LEVEL3] {
            let mut state = xkb::State::new(keymap);
            state.update_mask(0, 0, 0, 0, 0, group);
            let mut held = |bit: u8, key: Option<xkb::Keycode>| {
                if mods & bit == 0 {
                    return true;
                }
                key.is_some_and(|key| {
                    state.update_key(key, xkb::KeyDirection::Down);
                    true
                })
            };
            if !held(SHIFT, shift) || !held(LEVEL3, level3) {
                continue;
            }
            for raw in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
                let code = xkb::Keycode::new(raw);
                if raw < 8 || modifier_keys.contains(&Some(code)) {
                    continue;
                }
                let sym = state.key_get_one_sym(code);
                if sym == xkb::Keysym::NoSymbol || sym.is_keypad_key() {
                    continue;
                }
                let c = if sym == xkb::Keysym::Return {
                    '\n'
                } else {
                    match char::from_u32(state.key_get_utf32(code)) {
                        Some(c) if c == '\t' || !c.is_control() => c,
                        _ => continue,
                    }
                };
                chars.entry(c).or_insert(Stroke {
                    key: evdev(code),
                    mods,
                });
            }
        }
        Self {
            group,
            chars,
            shift: shift.map(evdev),
            level3: level3.map(evdev),
            control: control.map(evdev),
        }
    }

    fn stroke(&self, c: char) -> Option<Stroke> {
        self.chars.get(&c).copied()
    }

    /// Ctrl+V (Ctrl+Shift+V for terminals), on the key that types `v` when
    /// there is one.
    fn paste(&self, terminal: bool) -> Option<Stroke> {
        self.control?;
        let key = match self.stroke('v') {
            Some(Stroke { key, mods: 0 }) => key,
            _ => KEY_V,
        };
        let mods = if terminal { CONTROL | SHIFT } else { CONTROL };
        Some(Stroke { key, mods })
    }

    /// Press the modifiers, tap the key, release the modifiers.
    fn key_events(&self, stroke: Stroke, out: &mut Vec<(u32, bool)>) {
        let modifiers = [
            (SHIFT, self.shift),
            (LEVEL3, self.level3),
            (CONTROL, self.control),
        ];
        let held = modifiers
            .iter()
            .filter(|(bit, _)| stroke.mods & bit != 0)
            .filter_map(|(_, key)| *key);
        out.extend(held.clone().map(|key| (key, true)));
        out.push((stroke.key, true));
        out.push((stroke.key, false));
        let mut released: Vec<_> = held.map(|key| (key, false)).collect();
        released.reverse();
        out.extend(released);
    }
}

fn find_key(keymap: &xkb::Keymap, sym: xkb::Keysym) -> Option<xkb::Keycode> {
    (keymap.min_keycode().raw()..=keymap.max_keycode().raw())
        .map(xkb::Keycode::new)
        .find(|&code| {
            (0..keymap.num_layouts_for_key(code))
                .any(|layout| keymap.key_get_syms_by_level(code, layout, 0).contains(&sym))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // evdev codes from linux/input-event-codes.h
    const KEY_1: u32 = 2;
    const KEY_Q: u32 = 16;
    const KEY_Y: u32 = 21;
    const KEY_ENTER: u32 = 28;
    const KEY_A: u32 = 30;
    const KEY_APOSTROPHE: u32 = 40;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_SPACE: u32 = 57;
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_TAB: u32 = 15;

    fn keymap(layout: &str) -> xkb::Keymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(&context, "evdev", "pc105", layout, "", None, 0)
            .expect("compiling a keymap from the system's xkeyboard-config")
    }

    #[expect(clippy::unnecessary_wraps, reason = "compared with CharMap lookups")]
    fn on(key: u32, mods: u8) -> Option<Stroke> {
        Some(Stroke { key, mods })
    }

    #[test]
    fn us_layout_types_ascii_with_the_fewest_modifiers() {
        let map = CharMap::new(&keymap("us"), 0);
        assert_eq!(map.stroke('a'), on(KEY_A, 0));
        assert_eq!(map.stroke('A'), on(KEY_A, SHIFT));
        assert_eq!(map.stroke('!'), on(KEY_1, SHIFT));
        assert_eq!(map.stroke('1'), on(KEY_1, 0));
        assert_eq!(map.stroke(' '), on(KEY_SPACE, 0));
        assert_eq!(map.stroke('\n'), on(KEY_ENTER, 0));
        assert_eq!(map.stroke('\t'), on(KEY_TAB, 0));
    }

    #[test]
    fn characters_missing_from_the_layout_have_no_stroke() {
        let map = CharMap::new(&keymap("us"), 0);
        assert_eq!(map.stroke('é'), None);
        assert_eq!(map.stroke('ß'), None);
        assert_eq!(map.stroke('\r'), None);
    }

    #[test]
    fn altgr_levels_and_swapped_keys_follow_the_layout() {
        let map = CharMap::new(&keymap("de"), 0);
        assert_eq!(map.stroke('z'), on(KEY_Y, 0));
        assert_eq!(map.stroke('ä'), on(KEY_APOSTROPHE, 0));
        assert_eq!(map.stroke('Ä'), on(KEY_APOSTROPHE, SHIFT));
        assert_eq!(map.stroke('@'), on(KEY_Q, LEVEL3));
    }

    #[test]
    fn the_active_group_decides_the_characters() {
        let keymap = keymap("us,ru");
        let latin = CharMap::new(&keymap, 0);
        let cyrillic = CharMap::new(&keymap, 1);
        assert_eq!(latin.stroke('ф'), None);
        assert_eq!(cyrillic.stroke('ф'), on(KEY_A, 0));
        // Ctrl+V stays on the V key when the group has no Latin v.
        assert_eq!(cyrillic.paste(false), on(KEY_V, CONTROL));
        assert_eq!(latin.paste(false), on(KEY_V, CONTROL));
        assert_eq!(latin.paste(true), on(KEY_V, CONTROL | SHIFT));
    }

    #[test]
    fn modifiers_wrap_the_key_and_release_in_reverse() {
        let map = CharMap::new(&keymap("de"), 0);
        let level3 = map.level3.expect("de has a level 3 shift key");
        let mut events = Vec::new();
        map.key_events(
            Stroke {
                key: KEY_Q,
                mods: SHIFT | LEVEL3,
            },
            &mut events,
        );
        assert_eq!(
            events,
            [
                (KEY_LEFTSHIFT, true),
                (level3, true),
                (KEY_Q, true),
                (KEY_Q, false),
                (level3, false),
                (KEY_LEFTSHIFT, false),
            ]
        );
        events.clear();
        map.key_events(map.paste(true).expect("de has Control"), &mut events);
        assert_eq!(
            events,
            [
                (KEY_LEFTSHIFT, true),
                (KEY_LEFTCTRL, true),
                (KEY_V, true),
                (KEY_V, false),
                (KEY_LEFTCTRL, false),
                (KEY_LEFTSHIFT, false),
            ]
        );
    }
}
