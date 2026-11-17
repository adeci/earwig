//! The control socket (a 0600 Unix socket, or a named pipe only the user may
//! open): one JSON [`Command`] per line in, [`Event`] lines out. `status` gets
//! one [`Event::Status`], `check-for-updates` and `update` one
//! [`Event::Update`] (or [`Event::Error`]), `subscribe` a snapshot and then
//! every event (later lines are ignored), a bad line one [`Event::Error`],
//! anything else no reply. The daemon hangs up once the client shuts its
//! write side (on a pipe: sends an empty message).

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as os;
#[cfg(windows)]
use windows as os;

#[cfg(windows)]
pub(crate) use windows::user_sid;

use std::io::{self, BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::Context as _;

use crate::controller::Handle;
use crate::events::{Command, Event, UpdateState};

/// A client that stops reading for this long is dropped, so it cannot pin a
/// thread and an ever-growing event queue.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Send one command to the running daemon; returns its reply, if the
/// command has one.
pub fn request(cmd: &Command) -> anyhow::Result<Option<Event>> {
    request_at(&crate::paths::socket(), cmd)
}

/// Stream the daemon's events, starting with a status snapshot.
pub fn subscribe() -> anyhow::Result<impl Iterator<Item = anyhow::Result<Event>>> {
    subscribe_at(&crate::paths::socket())
}

/// Bound but not serving yet, so a second daemon stops before it opens the
/// microphone or loads a model.
pub struct Server {
    listener: os::Listener,
    path: PathBuf,
}

impl Server {
    /// Fails if another daemon answers at `path`; a stale socket file left by
    /// a crash is replaced.
    pub fn bind(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            listener: os::Listener::bind(path)?,
            path: path.to_owned(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept connections on a background thread, one thread per client.
    pub fn serve(self, handle: Handle) -> anyhow::Result<()> {
        let mut listener = self.listener;
        thread::Builder::new()
            .name("earwig-ipc".into())
            .spawn(move || {
                loop {
                    match listener.accept() {
                        Ok(conn) => {
                            let handle = handle.clone();
                            let spawned = thread::Builder::new()
                                .name("earwig-ipc-client".into())
                                .spawn(move || {
                                    if let Err(e) = serve_client(&conn, &handle) {
                                        log::debug!("control client: {e}");
                                    }
                                });
                            if let Err(e) = spawned {
                                log::warn!("control socket: {e}");
                            }
                        }
                        Err(e) => log::warn!("control socket accept: {e}"),
                    }
                }
            })?;
        Ok(())
    }
}

fn serve_client(conn: &os::Conn, handle: &Handle) -> io::Result<()> {
    for line in BufReader::new(conn).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Command>(&line) {
            Ok(Command::Status) => write_line(
                conn,
                &Event::Status {
                    status: Box::new(handle.status()),
                },
            )?,
            Ok(Command::Subscribe) => {
                for event in &handle.subscribe() {
                    write_line(conn, &event)?;
                }
                return Ok(());
            }
            Ok(Command::CheckForUpdates) => {
                write_line(conn, &update_reply(handle.check_for_updates()))?;
            }
            Ok(Command::Update) => write_line(conn, &update_reply(handle.install_update()))?,
            Ok(cmd) => handle.send(cmd),
            Err(e) => write_line(
                conn,
                &Event::Error {
                    message: format!("invalid command: {e}"),
                },
            )?,
        }
    }
    Ok(())
}

fn update_reply(answer: Result<UpdateState, String>) -> Event {
    match answer {
        Ok(update) => Event::Update { update },
        Err(message) => Event::Error { message },
    }
}

fn write_line(mut out: impl io::Write, value: &impl serde::Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(value).map_err(io::Error::other)?;
    line.push('\n');
    out.write_all(line.as_bytes())
}

/// How long a client waits for the daemon to answer a command: one that took
/// the connection and never answers must not hang `earwig quit` or `status`,
/// which the installer runs and waits for. The updater answers
/// `check-for-updates` and `update` once it is free, behind a check of GitHub
/// it may be in and, for a check, after one of its own: up to 30 s each.
fn reply_timeout(cmd: &Command) -> Duration {
    if matches!(cmd, Command::CheckForUpdates | Command::Update) {
        Duration::from_secs(90)
    } else {
        Duration::from_secs(5)
    }
}

fn connect(path: &Path, timeout: Option<Duration>) -> anyhow::Result<os::Conn> {
    os::connect(path, timeout)
        .with_context(|| format!("earwig is not running (no daemon at {})", path.display()))
}

/// [`request`] against an explicit socket path.
fn request_at(path: &Path, cmd: &Command) -> anyhow::Result<Option<Event>> {
    request_within(path, cmd, reply_timeout(cmd))
}

fn request_within(path: &Path, cmd: &Command, timeout: Duration) -> anyhow::Result<Option<Event>> {
    let conn = connect(path, Some(timeout))?;
    write_line(&conn, cmd)?;
    os::shutdown_write(&conn)?;
    let mut reply = None;
    for line in BufReader::new(&conn).lines() {
        let line = line.map_err(|e| match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                anyhow::anyhow!("earwig did not answer within {timeout:?}")
            }
            _ => e.into(),
        })?;
        if !line.trim().is_empty() {
            reply = Some(serde_json::from_str(&line)?);
            break;
        }
    }
    if let Some(Event::Error { message }) = &reply {
        anyhow::bail!("daemon: {message}");
    }
    Ok(reply)
}

/// [`subscribe`] against an explicit socket path.
fn subscribe_at(
    path: &Path,
) -> anyhow::Result<impl Iterator<Item = anyhow::Result<Event>> + use<>> {
    let conn = connect(path, None)?;
    write_line(&conn, &Command::Subscribe)?;
    Ok(BufReader::new(conn)
        .lines()
        .filter(|line| !line.as_ref().is_ok_and(|l| l.trim().is_empty()))
        .map(|line| Ok(serde_json::from_str(&line?)?)))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use super::*;
    use crate::config::{ConfigPaths, ConfigStore};
    use crate::controller::{Bus, Shared};
    use crate::engine::{EngineFactory, EngineOptions, Model};
    use crate::events::{DictationState, ModelStatus, Status};
    use crate::history::History;
    use crate::models::{ModelManager, ModelSpec};
    use crate::spectrum::Feed;
    use crate::update;

    struct NoEngine;

    impl EngineFactory for NoEngine {
        fn supports_architecture(&self, _arch: &str) -> bool {
            false
        }
        fn load(&self, _path: &Path, _options: &EngineOptions) -> anyhow::Result<Box<dyn Model>> {
            anyhow::bail!("no engine")
        }
    }

    fn handle(
        dir: &Path,
        updates: Option<flume::Sender<update::Request>>,
    ) -> (Handle, Arc<Shared>, flume::Receiver<Command>) {
        let (tx, rx) = flume::unbounded();
        let config = ConfigStore::load_from(ConfigPaths {
            user: dir.join("config.toml"),
            system: Vec::new(),
        })
        .unwrap();
        let status = Status {
            state: DictationState::Idle,
            model: ModelStatus::Missing {
                model: ModelSpec::Curated { id: "m".into() },
            },
            insert_methods: Vec::new(),
            hotkey_sources: vec!["socket".into()],
            hotkey: crate::events::HotkeyStatus::default(),
            permissions: Vec::new(),
            update: updates.as_ref().map(|_| UpdateState::None),
        };
        let shared = Arc::new(Shared {
            commands: tx,
            bus: Bus::default(),
            spectrum: Feed::default(),
            status: RwLock::new(status),
            config: Arc::new(RwLock::new(config)),
            models: Arc::new(ModelManager::new(dir.join("models"))),
            history: Arc::new(History::open(dir.join("history.jsonl"), 5).unwrap()),
            engine: Arc::new(NoEngine),
            updates,
            relaunch: std::sync::Mutex::new(None),
        });
        (Handle::new(shared.clone()), shared, rx)
    }

    /// A fresh address for a test server: a socket in `dir`, or a pipe named
    /// after it.
    fn address(dir: &Path) -> PathBuf {
        #[cfg(unix)]
        {
            dir.join("earwig.sock")
        }
        #[cfg(windows)]
        {
            let name = dir.file_name().unwrap().to_string_lossy();
            PathBuf::from(format!(r"\\.\pipe\earwig-test-{name}"))
        }
    }

    #[test]
    fn commands_status_and_subscriptions_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, shared, commands) = handle(dir.path(), None);
        let path = address(dir.path());
        Server::bind(&path).unwrap().serve(handle.clone()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        assert_eq!(request_at(&path, &Command::Press).unwrap(), None);
        assert_eq!(
            commands.recv_timeout(Duration::from_secs(5)).unwrap(),
            Command::Press
        );

        let reply = request_at(&path, &Command::Status).unwrap();
        assert_eq!(
            reply,
            Some(Event::Status {
                status: Box::new(handle.status())
            })
        );
        let mut events = subscribe_at(&path).unwrap();
        assert!(matches!(events.next(), Some(Ok(Event::Status { .. }))));
        // `subscribe` registers before the snapshot is written, so this
        // publish cannot be missed.
        shared.bus.publish(&Event::Level { level: 0.5 });
        assert_eq!(events.next().unwrap().unwrap(), Event::Level { level: 0.5 });

        let raw = os::connect(&path, None).unwrap();
        write_line(&raw, &serde_json::json!({"cmd": "dance"})).unwrap();
        os::shutdown_write(&raw).unwrap();
        let line = BufReader::new(&raw).lines().next().unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str(&line).unwrap(),
            Event::Error { .. }
        ));
    }

    #[test]
    fn a_second_daemon_leaves_the_live_ones_address_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = address(dir.path());
        let _server = Server::bind(&path).unwrap();
        assert!(Server::bind(&path).is_err());
        assert!(
            os::connect(&path, None).is_ok(),
            "the live daemon lost its address"
        );
    }

    #[test]
    fn a_daemon_that_never_answers_fails_the_request_in_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = address(dir.path());
        let mut listener = os::Listener::bind(&path).unwrap();
        // Takes the connection and keeps it, answering nothing.
        thread::spawn(move || {
            let _conn = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(30));
        });
        let started = std::time::Instant::now();
        let err = request_within(&path, &Command::Status, Duration::from_millis(300)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "{err:#}");
        assert!(err.to_string().contains("did not answer"), "{err:#}");
    }

    #[test]
    fn update_requests_get_the_updaters_answer() {
        let dir = tempfile::tempdir().unwrap();
        let (updater, requests) = flume::unbounded();
        let (handle, _, _) = handle(dir.path(), Some(updater));
        let path = address(dir.path());
        Server::bind(&path).unwrap().serve(handle).unwrap();
        let available = UpdateState::Available {
            version: "0.2.0".into(),
            installable: true,
        };
        let found = available.clone();
        // Stands in for the updater: a check finds 0.2.0, an install won't start.
        std::thread::spawn(move || {
            for request in requests {
                match request {
                    update::Request::Check(Some(reply)) => {
                        let _ = reply.send(Ok(found.clone()));
                    }
                    update::Request::Install(Some(reply)) => {
                        let _ = reply.send(Err("the disk is full".into()));
                    }
                    _ => {}
                }
            }
        });
        assert_eq!(
            request_at(&path, &Command::CheckForUpdates).unwrap(),
            Some(Event::Update { update: available })
        );
        let refused = request_at(&path, &Command::Update).unwrap_err();
        assert!(
            refused.to_string().contains("the disk is full"),
            "{refused}"
        );
    }

    #[test]
    fn a_build_that_does_not_update_itself_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, _, _) = handle(dir.path(), None);
        let path = address(dir.path());
        Server::bind(&path).unwrap().serve(handle).unwrap();
        for cmd in [Command::CheckForUpdates, Command::Update] {
            let refused = request_at(&path, &cmd).unwrap_err();
            assert!(
                refused.to_string().contains(update::NOT_UPDATING),
                "{refused}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_socket_is_replaced() {
        use rustix::net::{AddressFamily, SocketAddrUnix, SocketType, bind, socket};
        let dir = tempfile::tempdir().unwrap();
        let path = address(dir.path());
        // A socket nothing listens on, as a crash leaves it. Not a dropped
        // listener: a child another test forks meanwhile holds that until exec.
        let stale = socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
        bind(&stale, &SocketAddrUnix::new(&path).unwrap()).unwrap();
        let _server = Server::bind(&path).unwrap();
        assert!(os::connect(&path, None).is_ok());
    }
}
