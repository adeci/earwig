//! Insertion where earwig has no keyboard access (GNOME, KDE): earwig sets the
//! clipboard and the primary selection itself, as it does with keyboard
//! access, and the RemoteDesktop portal (v2) presses Shift+Insert over libei.
//! The desktop asks the user at start, for as long as they like; the restore
//! token in `<state_dir>/remote-desktop.token` brings the session back without
//! asking. A dictation while typing is not allowed fails, and the next one has
//! the desktop ask again.

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
use ashpd::desktop::remote_desktop::{
    DeviceType, RemoteDesktop, SelectDevicesOptions, SelectedDevices, StartOptions,
};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use async_io::{Async, Timer};
use futures_lite::{Stream, StreamExt as _, future, pin};
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};

use super::paste::Selections;
use crate::APP_ID;
use crate::events::InsertMethod;
use crate::insert::{InsertError, Inserter, NOT_ALLOWED};

const TOKEN_FILE: &str = "remote-desktop.token";

/// How long [`PortalInserter`] waits for the portal thread to press the keys.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the EIS server may take to hand out a keyboard after `Start`.
const EIS_SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Portal steps without user interaction.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a saved permission may take to come back (~80 ms) before the
/// desktop is taken to be asking the user again. Shorter than
/// [`REPLY_TIMEOUT`], so the dictation waiting for it learns why it failed.
const RESTORE_PATIENCE: Duration = Duration::from_secs(2);

/// GNOME shows a "remote control" indicator while the session is open.
const IDLE_CLOSE: Duration = Duration::from_secs(60);

/// Shift+Insert as evdev codes, which every layout keeps on the same keys.
const SHIFT_INSERT: [(u32, bool); 4] = [(42, true), (110, true), (110, false), (42, false)];

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

/// Tell xdg-desktop-portal which app this is (1.21+ refuses GlobalShortcuts
/// to host apps without an id; GNOME keys restore data by it). It must be the
/// first portal call on the process's D-Bus connection, and happen once.
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

/// The selections, then Shift+Insert through a RemoteDesktop session with a
/// keyboard over libei. The session closes after [`IDLE_CLOSE`] without
/// presses and is restored (no dialog, ~80 ms) by the next one; one allowed
/// without being remembered stays open instead.
pub struct PortalInserter {
    selections: Selections,
    presses: flume::Sender<Press>,
}

/// A request for Shift+Insert, skipped once the caller timed out.
struct Press {
    reply: flume::Sender<Result<(), InsertError>>,
}

impl Press {
    /// Fail it for want of the user's permission.
    fn refuse(self) {
        self.fail(anyhow!(NOT_ALLOWED));
    }

    fn fail(self, e: anyhow::Error) {
        let _ = self.reply.send(Err(InsertError::Failed(e)));
    }
}

impl PortalInserter {
    /// Start the portal thread, which has the desktop ask at once or brings
    /// a saved permission back, and return without waiting for it.
    pub(super) fn connect(state_dir: &Path, selections: Selections) -> anyhow::Result<Self> {
        register_app_id();
        let token_file = state_dir.join(TOKEN_FILE);
        let (presses, inbox) = flume::unbounded();
        thread::Builder::new()
            .name("earwig-portal".into())
            .spawn(move || async_io::block_on(sessions(&token_file, &inbox)))
            .context("spawning the portal thread")?;
        Ok(Self {
            selections,
            presses,
        })
    }

    fn press(&self) -> Result<(), InsertError> {
        let (reply, answer) = flume::bounded(1);
        self.presses
            .send(Press { reply })
            .map_err(|_| anyhow!("the portal thread is gone"))?;
        match answer.recv_timeout(REPLY_TIMEOUT) {
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
        self.selections.set(text, true)?;
        self.press()?;
        Ok(InsertMethod::PortalPaste)
    }

    fn copy_to_clipboard(&mut self, text: &str) -> Result<(), InsertError> {
        self.selections.set(text, false)
    }
}

/// The portal thread: a session at once, so the desktop asks before the first
/// dictation rather than during it; then again whenever a press finds none.
async fn sessions(token_file: &Path, presses: &flume::Receiver<Press>) {
    let mut pending = None;
    loop {
        match open(token_file, pending.take(), presses).await {
            Opened::Ready(ready) => {
                let Ready {
                    session,
                    mut worker,
                    events,
                    pending,
                    restorable,
                } = *ready;
                let end = worker
                    .serve(&session, events, presses, pending, restorable)
                    .await;
                let _ = session.close().await;
                match end {
                    End::Shutdown => return,
                    End::Idle => {
                        log::debug!("RemoteDesktop session closed after {IDLE_CLOSE:?} idle");
                    }
                    End::Lost(reason) => log::warn!("RemoteDesktop session lost: {reason}"),
                }
            }
            Opened::Failed => {}
            Opened::Shutdown => return,
        }
        let Ok(press) = presses.recv_async().await else {
            return;
        };
        pending = Some(press);
    }
}

enum Opened {
    Ready(Box<Ready>),
    /// Not open; whoever waited for it has been told why.
    Failed,
    Shutdown,
}

struct Ready {
    session: Session<RemoteDesktop>,
    worker: Worker,
    events: reis::async_io::EiConvertEventStream,
    /// The press the session was restored for.
    pending: Option<Press>,
    /// A restore token was saved: the session may close when idle.
    restorable: bool,
}

enum Wait {
    Answer(ashpd::Result<SelectedDevices>),
    Press(Result<Press, flume::RecvError>),
    Impatient,
}

/// Open a session: restored quietly with a saved permission, else allowed by
/// the user in the desktop's dialog, which may stay up as long as they like.
/// The dialog takes the focus from the app a press is for: a press waits
/// only for a restore.
async fn open(
    token_file: &Path,
    mut pending: Option<Press>,
    presses: &flume::Receiver<Press>,
) -> Opened {
    let token = saved_token(token_file);
    let created = bounded(
        SETUP_TIMEOUT,
        "RemoteDesktop setup",
        create_session(token.as_deref()),
    )
    .await;
    let (remote, session) = match created {
        Ok(created) => created,
        Err(e) => return failed(e, pending),
    };
    let mut asking = token.is_none();
    if asking && let Some(press) = pending.take() {
        press.refuse();
    }
    let answer = {
        let started = async {
            remote
                .start(&session, None, StartOptions::default())
                .await
                .and_then(|request| request.response())
        };
        pin!(started);
        let mut patience = if asking {
            Timer::never()
        } else {
            Timer::after(RESTORE_PATIENCE)
        };
        loop {
            let wait = future::or(
                async { Wait::Answer((&mut started).await) },
                future::or(async { Wait::Press(presses.recv_async().await) }, async {
                    (&mut patience).await;
                    Wait::Impatient
                }),
            )
            .await;
            match wait {
                Wait::Answer(answer) => break answer,
                // The desktop must be asking again: the press fails now, the
                // dialog stays.
                Wait::Impatient => {
                    patience = Timer::never();
                    asking = true;
                    if let Some(press) = pending.take() {
                        press.refuse();
                    }
                }
                Wait::Press(Ok(press)) if asking => press.refuse(),
                Wait::Press(Ok(press)) => pending = Some(press),
                Wait::Press(Err(_)) => {
                    let _ = session.close().await;
                    return Opened::Shutdown;
                }
            }
        }
    };
    let refusal = match answer {
        Ok(granted) if granted.devices().contains(DeviceType::Keyboard) => {
            return admit(&remote, session, &granted, token_file, pending).await;
        }
        // GNOME's Share with "Allow Remote Interaction" off.
        Ok(_) => "the keyboard was not shared",
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => "the dialog was cancelled",
        Err(ashpd::Error::Response(ResponseError::Other)) => "the desktop ended the request",
        Err(e) => {
            let _ = session.close().await;
            return failed(
                anyhow!(e).context("starting the RemoteDesktop session"),
                pending,
            );
        }
    };
    let _ = session.close().await;
    // What it restored, if anything, went with the answer.
    forget(token_file);
    log::warn!("typing was not allowed: {refusal}");
    if let Some(press) = pending {
        press.refuse();
    }
    Opened::Failed
}

/// The user said yes: remember it and connect the keyboard.
async fn admit(
    remote: &RemoteDesktop,
    session: Session<RemoteDesktop>,
    granted: &SelectedDevices,
    token_file: &Path,
    pending: Option<Press>,
) -> Opened {
    let restorable = if let Some(token) = granted.restore_token() {
        crate::paths::write_atomic(token_file, token.as_bytes())
            .inspect_err(|e| log::warn!("saving {}: {e}", token_file.display()))
            .is_ok()
    } else {
        // The user chose not to have it remembered.
        forget(token_file);
        false
    };
    match Worker::connect(remote, &session).await {
        Ok((worker, events)) => {
            log::info!("RemoteDesktop portal ready");
            Opened::Ready(Box::new(Ready {
                session,
                worker,
                events,
                pending,
                restorable,
            }))
        }
        Err(e) => {
            let _ = session.close().await;
            failed(e, pending)
        }
    }
}

/// The session could not be set up, which is no answer of the user's.
fn failed(e: anyhow::Error, pending: Option<Press>) -> Opened {
    log::warn!("RemoteDesktop portal: {e:#}");
    if let Some(press) = pending {
        press.fail(e);
    }
    Opened::Failed
}

fn saved_token(token_file: &Path) -> Option<String> {
    fs::read_to_string(token_file)
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

fn forget(token_file: &Path) {
    if let Err(e) = fs::remove_file(token_file)
        && e.kind() != io::ErrorKind::NotFound
    {
        log::warn!("removing {}: {e}", token_file.display());
    }
}

/// A RemoteDesktop session with the keyboard selected (restored from `token`
/// when there is one); not started yet.
async fn create_session(
    token: Option<&str>,
) -> anyhow::Result<(RemoteDesktop, Session<RemoteDesktop>)> {
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
    Ok((remote, session))
}

struct Worker {
    ei: reis::event::Connection,
    /// A second handle on the EIS socket to wait for room when it is full.
    socket: Async<OwnedFd>,
    keyboard: Option<Keyboard>,
    sequence: u32,
    /// Set when libei reports a disconnect.
    dead: Option<String>,
}

struct Keyboard {
    device: reis::event::Device,
    keys: ei::Keyboard,
    resumed: bool,
}

enum Input {
    Press(Press),
    Shutdown,
    Idle,
    Ei(Option<Result<EiEvent, reis::Error>>),
    Closed(Option<()>),
}

enum End {
    Shutdown,
    Idle,
    Lost(String),
}

impl Worker {
    /// The libei keyboard of a started session.
    async fn connect(
        remote: &RemoteDesktop,
        session: &Session<RemoteDesktop>,
    ) -> anyhow::Result<(Self, reis::async_io::EiConvertEventStream)> {
        let (socket, ei, events) = bounded(SETUP_TIMEOUT, "connecting to EIS", async {
            let fd = remote
                .connect_to_eis(
                    session,
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
        Ok((worker, events))
    }

    /// Serve presses (`pending` first) until shutdown, the session ends or,
    /// if it is `restorable`, [`IDLE_CLOSE`] without presses.
    async fn serve(
        &mut self,
        session: &Session<RemoteDesktop>,
        events: reis::async_io::EiConvertEventStream,
        presses: &flume::Receiver<Press>,
        pending: Option<Press>,
        restorable: bool,
    ) -> End {
        if let Some(press) = pending {
            self.handle(press).await;
        }
        // `None` once a source has ended.
        let mut events = Some(events);
        let mut closed: Option<Pin<Box<dyn Stream<Item = ()> + '_>>> =
            match session.receive_closed().await {
                Ok(stream) => Some(Box::pin(stream.map(|_| ()))),
                Err(e) => {
                    log::warn!("cannot watch the RemoteDesktop session: {e}");
                    None
                }
            };
        let rest = || {
            if restorable {
                Timer::after(IDLE_CLOSE)
            } else {
                Timer::never()
            }
        };
        let mut idle = rest();
        loop {
            let input = future::or(
                async {
                    presses
                        .recv_async()
                        .await
                        .map_or(Input::Shutdown, Input::Press)
                },
                future::or(
                    async { Input::Ei(next_or_pending(&mut events).await) },
                    future::or(
                        async { Input::Closed(next_or_pending(&mut closed).await) },
                        async {
                            (&mut idle).await;
                            Input::Idle
                        },
                    ),
                ),
            )
            .await;
            match input {
                Input::Press(press) => {
                    self.handle(press).await;
                    idle = rest();
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
                Input::Closed(Some(())) => return End::Lost("the session was closed".into()),
                Input::Closed(None) => closed = None,
            }
        }
    }

    async fn handle(&mut self, press: Press) {
        // Its caller gave up: the text may be going elsewhere by now.
        if press.reply.is_disconnected() {
            return;
        }
        let result = self.send_keys(&SHIFT_INSERT).await;
        let _ = press.reply.send(result.map_err(InsertError::Failed));
    }

    fn keyboard(&self) -> anyhow::Result<&Keyboard> {
        match &self.keyboard {
            Some(keyboard) if keyboard.resumed => Ok(keyboard),
            Some(_) => bail!("the compositor paused the portal keyboard"),
            None => bail!("the compositor offers no portal keyboard"),
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
                    let keys = added
                        .device
                        .interface::<ei::Keyboard>()
                        .context("keyboard device without ei_keyboard")?;
                    self.keyboard = Some(Keyboard {
                        device: added.device,
                        keys,
                        resumed: false,
                    });
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

/// libei timestamps: CLOCK_MONOTONIC in microseconds.
fn now_us() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(now.tv_sec).unwrap_or_default() * 1_000_000
        + u64::try_from(now.tv_nsec).unwrap_or_default() / 1_000
}
