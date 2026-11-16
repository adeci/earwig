//! Unix socket transport (Linux, macOS).

use std::fs;
use std::io;
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;

use super::WRITE_TIMEOUT;

pub(super) type Conn = UnixStream;

pub(super) struct Listener(UnixListener);

impl Listener {
    pub(super) fn bind(path: &Path) -> anyhow::Result<Self> {
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
        Ok(Self(listener))
    }

    pub(super) fn accept(&mut self) -> io::Result<Conn> {
        let (conn, _) = self.0.accept()?;
        conn.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(conn)
    }
}

/// A client end; reads and writes give up after `timeout`.
pub(super) fn connect(path: &Path, timeout: Option<Duration>) -> io::Result<Conn> {
    let conn = UnixStream::connect(path)?;
    conn.set_read_timeout(timeout)?;
    conn.set_write_timeout(timeout)?;
    Ok(conn)
}

pub(super) fn shutdown_write(conn: &Conn) -> io::Result<()> {
    conn.shutdown(Shutdown::Write)
}
