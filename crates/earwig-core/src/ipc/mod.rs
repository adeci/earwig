//! The control socket (a 0600 Unix socket, or a named pipe only the user may
//! open): one JSON [`Command`] per line in, [`Event`] lines out. `status` gets
//! one [`Event::Status`], `subscribe` a snapshot and then every event (later
//! lines are ignored), a bad line one [`Event::Error`], anything else no reply.
//! The daemon hangs up once the client shuts its write side (on a pipe: sends
//! an empty message).

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
use crate::events::{Command, Event};

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
                    status: handle.status(),
                },
            )?,
            Ok(Command::Subscribe) => {
                for event in &handle.subscribe() {
                    write_line(conn, &event)?;
                }
                return Ok(());
            }
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

fn write_line(mut out: impl io::Write, value: &impl serde::Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(value).map_err(io::Error::other)?;
    line.push('\n');
    out.write_all(line.as_bytes())
}

fn connect(path: &Path) -> anyhow::Result<os::Conn> {
    os::connect(path)
        .with_context(|| format!("earwig is not running (no daemon at {})", path.display()))
}

/// [`request`] against an explicit socket path.
pub fn request_at(path: &Path, cmd: &Command) -> anyhow::Result<Option<Event>> {
    let conn = connect(path)?;
    write_line(&conn, cmd)?;
    os::shutdown_write(&conn)?;
    let mut reply = None;
    for line in BufReader::new(&conn).lines() {
        let line = line?;
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
pub fn subscribe_at(
    path: &Path,
) -> anyhow::Result<impl Iterator<Item = anyhow::Result<Event>> + use<>> {
    let conn = connect(path)?;
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

    struct NoEngine;

    impl EngineFactory for NoEngine {
        fn supports_architecture(&self, _arch: &str) -> bool {
            false
        }
        fn load(&self, _path: &Path, _options: &EngineOptions) -> anyhow::Result<Box<dyn Model>> {
            anyhow::bail!("no engine")
        }
    }

    fn handle(dir: &Path) -> (Handle, Arc<Shared>, flume::Receiver<Command>) {
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
        let (handle, shared, commands) = handle(dir.path());
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
                status: handle.status()
            })
        );
        let mut events = subscribe_at(&path).unwrap();
        assert!(matches!(events.next(), Some(Ok(Event::Status { .. }))));
        // `subscribe` registers before the snapshot is written, so this
        // publish cannot be missed.
        shared.bus.publish(&Event::Level { level: 0.5 });
        assert_eq!(events.next().unwrap().unwrap(), Event::Level { level: 0.5 });

        let raw = os::connect(&path).unwrap();
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
            os::connect(&path).is_ok(),
            "the live daemon lost its address"
        );
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
        assert!(os::connect(&path).is_ok());
    }
}
