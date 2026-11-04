//! The control socket: `$XDG_RUNTIME_DIR/earwig.sock` ([`crate::paths::socket`]).
//!
//! Protocol: newline-delimited JSON. Each line a client writes is one
//! [`Command`]. Replies are [`Event`] lines:
//! - [`Command::Status`]: one [`Event::Status`].
//! - [`Command::Subscribe`]: an [`Event::Status`] snapshot, then every event
//!   until either side hangs up. Lines sent after it are ignored.
//! - every other command: no reply; it is queued for the controller.
//! - a line that is not a valid command: one [`Event::Error`].
//!
//! The daemon closes a connection when the client closes its write side, so
//! a client may send several commands, shut down writing, and read replies to
//! EOF. The socket is mode 0600 inside the per-user runtime directory.
//! Compositor binds use it through `earwig press|release|toggle|cancel`.

use std::fs;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::Context as _;

use crate::controller::Handle;
use crate::events::{Command, Event};

/// Serve [`crate::paths::socket`] for `handle` on a background thread.
pub fn serve(handle: Handle) -> anyhow::Result<()> {
    Server::bind(&crate::paths::socket())?.serve(handle)
}

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
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Bind `path`. Fails if another daemon answers there; a stale socket
    /// file left by a crash is replaced.
    pub fn bind(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            match UnixStream::connect(path) {
                Ok(_) => anyhow::bail!("earwig is already running (socket {})", path.display()),
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    log::info!("removing stale socket {}", path.display());
                    fs::remove_file(path)
                        .with_context(|| format!("removing stale socket {}", path.display()))?;
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("probing {}", path.display()));
                }
            }
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let listener =
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            path: path.to_owned(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept connections on a background thread, one thread per client.
    pub fn serve(self, handle: Handle) -> anyhow::Result<()> {
        thread::Builder::new()
            .name("earwig-ipc".into())
            .spawn(move || {
                for conn in self.listener.incoming() {
                    match conn {
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

fn serve_client(conn: &UnixStream, handle: &Handle) -> io::Result<()> {
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

fn connect(path: &Path) -> anyhow::Result<UnixStream> {
    UnixStream::connect(path)
        .with_context(|| format!("earwig is not running (no daemon at {})", path.display()))
}

fn send_line(mut conn: &UnixStream, cmd: &Command) -> anyhow::Result<()> {
    let mut line = serde_json::to_string(cmd)?;
    line.push('\n');
    conn.write_all(line.as_bytes())?;
    Ok(())
}

/// [`request`] against an explicit socket path.
pub fn request_at(path: &Path, cmd: &Command) -> anyhow::Result<Option<Event>> {
    let conn = connect(path)?;
    send_line(&conn, cmd)?;
    conn.shutdown(std::net::Shutdown::Write)?;
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
    use std::time::Duration;

    use super::*;
    use crate::config::{ConfigPaths, ConfigStore};
    use crate::controller::{Bus, Shared};
    use crate::engine::{EngineFactory, EngineOptions, Model};
    use crate::events::{DictationState, ModelStatus, Status};
    use crate::history::History;
    use crate::models::{ModelManager, ModelSpec};

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
            status: RwLock::new(status),
            config: Arc::new(RwLock::new(config)),
            models: Arc::new(ModelManager::new(dir.join("models"))),
            history: Arc::new(History::open(dir.join("history.jsonl"), 5).unwrap()),
            engine: Arc::new(NoEngine),
        });
        (Handle::new(shared.clone()), shared, rx)
    }

    #[test]
    fn commands_status_and_subscriptions_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (handle, shared, commands) = handle(dir.path());
        let path = dir.path().join("earwig.sock");
        Server::bind(&path).unwrap().serve(handle.clone()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

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
        // The server registers the subscriber before writing the snapshot,
        // so this publish cannot be missed.
        shared.bus.publish(&Event::Level { level: 0.5 });
        assert_eq!(events.next().unwrap().unwrap(), Event::Level { level: 0.5 });

        let mut raw = UnixStream::connect(&path).unwrap();
        raw.write_all(b"{\"cmd\":\"dance\"}\n").unwrap();
        raw.shutdown(std::net::Shutdown::Write).unwrap();
        let line = BufReader::new(&raw).lines().next().unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str(&line).unwrap(),
            Event::Error { .. }
        ));
    }

    #[test]
    fn live_daemon_is_kept_and_stale_socket_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("earwig.sock");
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let server = Server::bind(&path).unwrap();
        assert!(Server::bind(&path).is_err());
        drop(server);
    }
}
