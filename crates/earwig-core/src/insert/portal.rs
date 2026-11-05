//! Insertion through the XDG RemoteDesktop (version 2) and Clipboard portals
//! and libei (GNOME, KDE): the text is set as the clipboard through the
//! portal, then one paste chord goes out over libei.
//!
//! One session is opened at daemon start; the user approves it once and the
//! restore token in `<state_dir>/remote-desktop.token` brings it back without
//! a dialog. Every portal step before the dialog is time bounded, so a backend
//! that is advertised but cannot work fails fast instead of blocking.
//!
//! The portal and libei live on a dedicated thread with its own executor,
//! which also answers clipboard transfers and EIS pings between dictations.

use std::cell::Cell;
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
use futures_lite::{AsyncWriteExt as _, Stream, StreamExt as _, future, pin};
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use xkbcommon::xkb;

use self::focus::FocusWatch;
use super::{InsertError, Inserter, MIME_TYPES};
use crate::events::InsertMethod;

mod focus;

/// The id portals know earwig by. A desktop file with this basename must be
/// installed: GNOME rejects portal requests from host apps without one.
pub const APP_ID: &str = "io.github.adeci.earwig";

const TOKEN_FILE: &str = "remote-desktop.token";

/// How long [`PortalInserter`] waits for the portal thread to finish one job.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the EIS server may take to hand out a keyboard after `Start`.
const EIS_SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Portal steps without user interaction.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// `Start` while the consent dialog waits for the user.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Upper bound on [`PortalInserter::connect`]: every setup step in a row.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5 + 300 + 5 + 10);

/// GNOME shows a "remote control" indicator while the session is open.
const IDLE_CLOSE: Duration = Duration::from_secs(60);

/// Longest wait for the portal to confirm a new clipboard selection before
/// the paste chord goes out anyway.
const OWNER_TIMEOUT: Duration = Duration::from_millis(500);

/// evdev `KEY_V`, for layouts without a Latin `v`.
const KEY_V: u32 = 47;

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

/// Tell xdg-desktop-portal which app this process is. Since 1.21 host apps
/// without an app id are refused by GlobalShortcuts, and GNOME keys
/// RemoteDesktop restore data by it.
///
/// It has to be the first portal call on the process-wide D-Bus connection
/// and may happen only once, so every portal user calls this first.
pub(crate) fn register_app_id() {
    static REGISTERED: LazyLock<()> = LazyLock::new(|| {
        let result = async_io::block_on(bounded(SETUP_TIMEOUT, "portal registry", async {
            let app_id = ashpd::AppID::try_from(APP_ID)?;
            Ok(ashpd::register_host_app(app_id).await?)
        }));
        match result {
            Ok(()) => log::debug!("registered with the portal as {APP_ID}"),
            // Portals before 1.20 have no registry and identify host apps by
            // their systemd unit instead.
            Err(e) => log::warn!("could not register app id {APP_ID} with the portal: {e:#}"),
        }
    });
    LazyLock::force(&REGISTERED);
}

/// A RemoteDesktop session with a keyboard over libei, plus clipboard access.
/// It closes after [`IDLE_CLOSE`] without requests and is restored (no
/// dialog, ~80 ms) by the next one.
pub struct PortalInserter {
    requests: flume::Sender<Request>,
}

enum Job {
    Paste(String),
    Copy(String),
}

struct Request {
    job: Job,
    /// Dropped by a caller that timed out; the job is then skipped.
    reply: flume::Sender<Result<(), InsertError>>,
}

impl PortalInserter {
    /// Open (or restore) the portal session. Called at daemon start so the
    /// consent dialog never interrupts a dictation.
    pub fn connect(state_dir: &Path) -> anyhow::Result<Self> {
        register_app_id();
        let token_file = state_dir.join(TOKEN_FILE);
        let (ready_tx, ready_rx) = flume::bounded(1);
        let (requests, requests_rx) = flume::unbounded();
        thread::Builder::new()
            .name("earwig-portal".into())
            .spawn(move || async_io::block_on(run(&token_file, &ready_tx, &requests_rx)))
            .context("spawning the portal thread")?;
        async_io::block_on(bounded(CONNECT_TIMEOUT, "portal setup", async {
            ready_rx
                .recv_async()
                .await
                .context("the portal thread exited during setup")?
        }))?;
        Ok(Self { requests })
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

impl Inserter for PortalInserter {
    fn methods(&self) -> Vec<InsertMethod> {
        vec![InsertMethod::PortalPaste]
    }

    fn insert(&mut self, text: &str) -> Result<InsertMethod, InsertError> {
        self.call(Job::Paste(text.to_owned()))?;
        Ok(InsertMethod::PortalPaste)
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        self.call(Job::Copy(text.to_owned()))
    }
}

/// The portal thread: serve sessions while watching which widget has focus.
async fn run(
    token_file: &Path,
    ready: &flume::Sender<anyhow::Result<()>>,
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
            if let Some(focused) = watch.current().await {
                terminal.set(watch.is_terminal(&focused).await);
            }
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
    ready: &flume::Sender<anyhow::Result<()>>,
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
    if ready.send(Ok(())).is_err() {
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

/// Kept apart from [`Worker`] because the signal streams borrow it while the
/// worker changes.
struct Portal {
    session: Session<RemoteDesktop>,
    clipboard: Clipboard,
}

struct Worker {
    ei: reis::event::Connection,
    /// A second handle on the EIS socket to wait for room when it is full.
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

enum End {
    Shutdown,
    Idle,
    Lost(String),
}

/// A RemoteDesktop session with the keyboard selected (restored from `token`
/// when there is one) and clipboard access requested; not started yet.
async fn create_session(
    token: Option<&str>,
) -> anyhow::Result<(RemoteDesktop, Session<RemoteDesktop>, Clipboard)> {
    let remote = RemoteDesktop::new().await?;
    if remote.version() < 2 {
        bail!(
            "RemoteDesktop portal version {} has no ConnectToEIS",
            remote.version()
        );
    }
    let session = remote
        .create_session(ashpd::desktop::CreateSessionOptions::default())
        .await?;
    remote
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard))
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token),
        )
        .await?
        .response()?;
    // Clipboard access has to be requested before `Start`.
    let clipboard = Clipboard::new()
        .await
        .context("the Clipboard portal is not available")?;
    clipboard
        .request(
            &session,
            ashpd::desktop::clipboard::RequestClipboardOptions::default(),
        )
        .await
        .context("requesting clipboard access")?;
    Ok((remote, session, clipboard))
}

impl Worker {
    async fn open(
        token_file: &Path,
    ) -> anyhow::Result<(Portal, Self, reis::async_io::EiConvertEventStream)> {
        let token = fs::read_to_string(token_file)
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
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
        if !started.is_clipboard_enabled() {
            bail!("clipboard access was not granted");
        }
        match started.restore_token() {
            Some(token) => {
                if let Err(e) = save_token(token_file, token) {
                    log::warn!("saving {}: {e:#}", token_file.display());
                }
            }
            None => log::debug!("the portal returned no restore token"),
        }

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
        bounded(EIS_SETUP_TIMEOUT, "waiting for the EIS keyboard", async {
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
        })
        .await?;
        log::info!("RemoteDesktop portal ready");
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
        if let Some(request) = pending {
            self.handle(portal, request, terminal.get()).await;
        }
        // `None` once a source has ended.
        let mut events = Some(events);
        let mut transfers: Option<Pin<Box<dyn Stream<Item = u32> + '_>>> = match portal
            .clipboard
            .receive_selection_transfer::<RemoteDesktop>()
            .await
        {
            Ok(stream) => Some(Box::pin(stream.map(|(_session, _mime, serial)| serial))),
            Err(e) => {
                log::warn!("cannot serve clipboard requests: {e}");
                None
            }
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
                Input::Request(request) => {
                    self.handle(portal, request, terminal.get()).await;
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

    async fn handle(&mut self, portal: &Portal, request: Request, terminal: bool) {
        let Request { job, reply } = request;
        let result = match job {
            Job::Copy(text) => self.set_selection(portal, text, &reply).await,
            Job::Paste(text) => {
                async {
                    let chord = self.keyboard()?.chord.events(terminal);
                    self.set_selection(portal, text, &reply).await?;
                    if reply.is_disconnected() {
                        return Ok(());
                    }
                    self.send_keys(&chord).await?;
                    Ok(())
                }
                .await
            }
        };
        let _ = reply.send(result);
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

    /// Send key events in one burst: one frame per event, one write.
    async fn send_keys(&mut self, keys: &[(u32, bool)]) -> anyhow::Result<()> {
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        let keyboard = self.keyboard()?;
        let device = keyboard.device.device();
        let serial = self.ei.serial();
        device.start_emulating(serial, sequence);
        for &(key, down) in keys {
            let state = if down {
                ei::keyboard::KeyState::Press
            } else {
                ei::keyboard::KeyState::Released
            };
            keyboard.keys.key(key, state);
            device.frame(serial, now_us());
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

    /// Set the clipboard unless the caller of `reply` has given up.
    async fn set_selection(
        &mut self,
        portal: &Portal,
        text: String,
        reply: &flume::Sender<Result<(), InsertError>>,
    ) -> Result<(), InsertError> {
        if reply.is_disconnected() {
            return Ok(());
        }
        self.selection = Some(text);
        // The portal may answer before the compositor has made the selection
        // current (xdg-desktop-portal-kde hands it to KWin over data-control
        // without a roundtrip), and a paste chord sent then pastes the
        // previous clipboard. Wait until the portal reports this session as
        // the owner; subscribe first, as the signal can precede the reply.
        let owner_changes = portal
            .clipboard
            .receive_selection_owner_changed::<RemoteDesktop>()
            .await
            .inspect_err(|e| log::debug!("cannot watch the clipboard owner: {e}"))
            .ok();
        let started = std::time::Instant::now();
        portal
            .clipboard
            .set_selection(
                &portal.session,
                SetSelectionOptions::default().set_mime_types(MIME_TYPES),
            )
            .await
            .context("setting the clipboard through the portal")?;
        if let Some(owner_changes) = owner_changes {
            let owned = owner_changes.filter(|(_, change)| change.session_is_owner() == Some(true));
            pin!(owned);
            let confirmed = future::or(async { owned.next().await.is_some() }, async {
                Timer::after(OWNER_TIMEOUT).await;
                false
            })
            .await;
            if confirmed {
                log::debug!(
                    "the portal confirmed the clipboard after {} ms",
                    started.elapsed().as_millis()
                );
            } else {
                log::debug!("the portal did not confirm the clipboard within {OWNER_TIMEOUT:?}");
            }
        }
        Ok(())
    }

    /// An app pastes: write our selection into the pipe the portal hands us.
    async fn transfer(&self, portal: &Portal, serial: u32) -> anyhow::Result<()> {
        let clipboard = &portal.clipboard;
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

struct Keyboard {
    device: reis::event::Device,
    keys: ei::Keyboard,
    chord: ChordKeys,
    resumed: bool,
}

/// evdev codes (XKB keycode - 8) of the paste chord keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChordKeys {
    ctrl: u32,
    shift: u32,
    v: u32,
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
        Ok(Self {
            device,
            keys,
            chord: ChordKeys::new(&keymap)?,
            resumed: false,
        })
    }
}

impl ChordKeys {
    fn new(keymap: &xkb::Keymap) -> anyhow::Result<Self> {
        let evdev = |sym| find_key(keymap, sym).map(|code| code.raw() - 8);
        Ok(Self {
            ctrl: evdev(xkb::Keysym::Control_L).context("the EIS keymap has no Control key")?,
            shift: evdev(xkb::Keysym::Shift_L).context("the EIS keymap has no Shift key")?,
            v: evdev(xkb::Keysym::v).unwrap_or(KEY_V),
        })
    }

    /// Ctrl+V, or Ctrl+Shift+V for terminals.
    fn events(self, terminal: bool) -> Vec<(u32, bool)> {
        let held: &[u32] = if terminal {
            &[self.shift, self.ctrl]
        } else {
            &[self.ctrl]
        };
        let mut events: Vec<_> = held.iter().map(|&key| (key, true)).collect();
        events.extend([(self.v, true), (self.v, false)]);
        events.extend(held.iter().rev().map(|&key| (key, false)));
        events
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
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_DOT: u32 = 52;

    fn chord_keys(layout: &str, variant: &str) -> ChordKeys {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap =
            xkb::Keymap::new_from_names(&context, "evdev", "pc105", layout, variant, None, 0)
                .expect("compiling a keymap from the system's xkeyboard-config");
        ChordKeys::new(&keymap).unwrap()
    }

    #[test]
    fn the_chord_uses_the_layouts_v_and_wraps_it_in_modifiers() {
        let us = chord_keys("us", "");
        assert_eq!(
            us.events(true),
            [
                (KEY_LEFTSHIFT, true),
                (KEY_LEFTCTRL, true),
                (KEY_V, true),
                (KEY_V, false),
                (KEY_LEFTCTRL, false),
                (KEY_LEFTSHIFT, false),
            ]
        );
        assert_eq!(
            us.events(false),
            [
                (KEY_LEFTCTRL, true),
                (KEY_V, true),
                (KEY_V, false),
                (KEY_LEFTCTRL, false),
            ]
        );
        assert_eq!(chord_keys("us", "dvorak").v, KEY_DOT);
        assert_eq!(chord_keys("ru", "").v, KEY_V);
    }
}
