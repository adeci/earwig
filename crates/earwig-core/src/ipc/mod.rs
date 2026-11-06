//! The control socket: a Unix socket (mode 0600), e.g.
//! `$XDG_RUNTIME_DIR/earwig.sock`, or on Windows a named pipe only the user
//! may open (see [`crate::paths::socket`]).
//!
//! Newline-delimited JSON: each client line is one [`Command`]; replies are
//! [`Event`] lines.
//! - [`Command::Status`]: one [`Event::Status`].
//! - [`Command::Subscribe`]: an [`Event::Status`] snapshot, then every event
//!   until either side hangs up. Lines sent after it are ignored.
//! - every other command: no reply; it is queued for the controller.
//! - a line that is not a valid command: one [`Event::Error`].
//!
//! The daemon closes a connection when the client closes its write side, so
//! a client may send several commands, shut down writing, and read to EOF.
//! A pipe has no half-close; there the client sends an empty message instead.
//!
//! Each OS module provides the transport: `Listener::{bind, accept}`, a
//! connection that reads and writes through `&Conn`, `connect` and
//! `shutdown_write`.

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

use std::io::{self, BufRead as _, BufReader, Write as _};
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

/// A bound control socket that is not serving yet. Binding first lets the
/// daemon refuse to start twice before it opens the microphone or loads a
/// model.
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
    let mut out = conn;
    for line in BufReader::new(conn).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Command>(&line) {
            Ok(Command::Status) => write_event(
                &mut out,
                &Event::Status {
                    status: handle.status(),
                },
            )?,
            Ok(Command::Subscribe) => {
                for event in &handle.subscribe() {
                    write_event(&mut out, &event)?;
                }
                return Ok(());
            }
            Ok(cmd) => handle.send(cmd),
            Err(e) => write_event(
                &mut out,
                &Event::Error {
                    message: format!("invalid command: {e}"),
                },
            )?,
        }
    }
    Ok(())
}

fn write_event(out: &mut impl io::Write, event: &Event) -> io::Result<()> {
    let mut line = serde_json::to_string(event).map_err(io::Error::other)?;
    line.push('\n');
    out.write_all(line.as_bytes())
}

fn connect(path: &Path) -> anyhow::Result<os::Conn> {
    os::connect(path)
        .with_context(|| format!("earwig is not running (no daemon at {})", path.display()))
}

fn send_line(mut conn: &os::Conn, cmd: &Command) -> anyhow::Result<()> {
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    conn.write_all(line.as_bytes())?;
    Ok(())
}

/// [`request`] against an explicit socket path.
pub fn request_at(path: &Path, cmd: &Command) -> anyhow::Result<Option<Event>> {
    let conn = connect(path)?;
    send_line(&conn, cmd)?;
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
    send_line(&conn, &Command::Subscribe)?;
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
        fn name(&self) -> &'static str {
            "none"
        }
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
        (&raw).write_all(b"{\"cmd\":\"dance\"}\n").unwrap();
        os::shutdown_write(&raw).unwrap();
        let line = BufReader::new(&raw).lines().next().unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str(&line).unwrap(),
            Event::Error { .. }
        ));
    }

    #[test]
    fn a_live_daemon_keeps_its_address_until_it_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = address(dir.path());
        let server = Server::bind(&path).unwrap();
        assert!(Server::bind(&path).is_err());
        assert!(
            os::connect(&path).is_ok(),
            "the live daemon lost its address"
        );
        drop(server);
        Server::bind(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = address(dir.path());
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let _server = Server::bind(&path).unwrap();
        assert!(os::connect(&path).is_ok());
    }
}
