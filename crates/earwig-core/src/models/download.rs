//! Resumable, verified downloads: bytes go to `<file>.part`, hashed as they
//! are written, and a later attempt re-hashes the part and asks for the rest
//! with `Range`. Only a file whose SHA-256 matches is renamed into place.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use ureq::Agent;

use super::{CancelToken, ModelError};

const CHUNK: usize = 256 * 1024;
/// Transfers that fail mid-way are resumed this many times per call.
const ATTEMPTS: u32 = 4;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

pub(crate) struct Source<'a> {
    pub url: &'a str,
    /// Expected SHA-256, hex.
    pub sha256: &'a str,
    /// Expected size, when known up front.
    pub size: Option<u64>,
}

/// `<dest>.part`
pub(crate) fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_owned();
    name.push(".part");
    dest.with_file_name(name)
}

/// Download `src` to `dest`. `progress(downloaded, total)` runs at most every
/// 100 ms and once at the end; `total` is 0 while unknown.
pub(crate) fn fetch(
    agent: &Agent,
    src: &Source<'_>,
    dest: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &CancelToken,
) -> Result<(), ModelError> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let part_path = part_path(dest);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&part_path)?;
    // One writer per part, across processes (settings window and CLI).
    file.try_lock().map_err(|e| match e {
        TryLockError::WouldBlock => {
            io::Error::other(format!("{} is already being downloaded", dest.display()))
        }
        TryLockError::Error(e) => e,
    })?;

    let mut part = Part {
        file,
        hasher: Sha256::new(),
        len: 0,
        total: src.size.unwrap_or(0),
        last_report: None,
        progress,
    };
    match part.complete(agent, src, cancel) {
        Ok(()) => {}
        // Deleted while still locked, so no other download of it can start
        // on the part first.
        Err(ModelError::Cancelled) => {
            if let Err(e) = fs::remove_file(&part_path) {
                log::warn!("removing {}: {e}", part_path.display());
            }
            return Err(ModelError::Cancelled);
        }
        Err(e) => return Err(e),
    }

    let actual = hex::encode(part.hasher.finalize_reset());
    if !actual.eq_ignore_ascii_case(src.sha256) {
        drop(part.file);
        fs::remove_file(&part_path)?;
        return Err(ModelError::Checksum {
            file: dest
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            expected: src.sha256.to_owned(),
            actual,
        });
    }
    part.file.sync_all()?;
    fs::rename(&part_path, dest)?;
    Ok(())
}

enum Failure {
    /// The connection broke; resuming may work.
    Retry(ModelError),
    Fatal(ModelError),
}

impl From<io::Error> for Failure {
    /// Local I/O (disk full, permissions) is not fixed by retrying.
    fn from(e: io::Error) -> Self {
        Self::Fatal(e.into())
    }
}

struct Part<'p> {
    file: File,
    hasher: Sha256,
    len: u64,
    /// Expected full size; 0 while unknown.
    total: u64,
    last_report: Option<Instant>,
    progress: &'p mut dyn FnMut(u64, u64),
}

impl Part<'_> {
    /// Hash what an earlier attempt left behind, then fetch the rest,
    /// resuming a broken transfer up to [`ATTEMPTS`] times.
    fn complete(
        &mut self,
        agent: &Agent,
        src: &Source<'_>,
        cancel: &CancelToken,
    ) -> Result<(), ModelError> {
        self.rehash(cancel)?;
        if src.size.is_some_and(|size| self.len > size) {
            self.restart()?;
        }
        self.report(false);
        let mut attempt = 1;
        while src.size != Some(self.len) {
            match self.transfer(agent, src, cancel) {
                Ok(()) => break,
                Err(Failure::Fatal(e)) => return Err(e),
                Err(Failure::Retry(e)) if attempt >= ATTEMPTS => return Err(e),
                Err(Failure::Retry(e)) => {
                    log::warn!("download interrupted at {} bytes ({e}); resuming", self.len);
                    std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
                    if cancel.is_cancelled() {
                        return Err(ModelError::Cancelled);
                    }
                    attempt += 1;
                }
            }
        }
        self.report(true);
        Ok(())
    }

    /// Hash what an earlier attempt left behind and position at its end.
    fn rehash(&mut self, cancel: &CancelToken) -> Result<(), ModelError> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut buf = vec![0; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let n = self.file.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            self.hasher.update(&buf[..n]);
            self.len += n as u64;
        }
    }

    fn restart(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.hasher = Sha256::new();
        self.len = 0;
        Ok(())
    }

    fn report(&mut self, force: bool) {
        let now = Instant::now();
        if force || self.last_report.is_none_or(|t| now - t >= PROGRESS_EVERY) {
            self.last_report = Some(now);
            (self.progress)(self.len, self.total);
        }
    }

    /// One request: from the current length to the end.
    fn transfer(
        &mut self,
        agent: &Agent,
        src: &Source<'_>,
        cancel: &CancelToken,
    ) -> Result<(), Failure> {
        let url = src.url;
        let net = |e: &dyn std::fmt::Display| ModelError::Http(format!("{url}: {e}"));
        let mut req = agent.get(url).header("Accept-Encoding", "identity");
        if self.len > 0 {
            req = req.header("Range", format!("bytes={}-", self.len));
        }
        let resp = req.call().map_err(|e| Failure::Retry(net(&e)))?;
        let status = resp.status().as_u16();
        let remaining = match status {
            206 if self.len > 0 => {
                let range = resp
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range);
                match range {
                    Some((start, total)) if start == self.len => {
                        total.map(|t| t.saturating_sub(start))
                    }
                    _ => {
                        log::warn!("{url}: unexpected Content-Range; restarting");
                        self.restart()?;
                        return Err(Failure::Retry(net(&"unexpected Content-Range")));
                    }
                }
            }
            200 => {
                if self.len > 0 {
                    log::info!("{url}: server ignored Range; restarting from 0");
                    self.restart()?;
                }
                resp.body().content_length()
            }
            416 if self.len > 0 => {
                // The part is longer than the remote file.
                self.restart()?;
                return Err(Failure::Retry(net(&"range not satisfiable")));
            }
            408 | 429 | 500..=599 => return Err(Failure::Retry(net(&format!("HTTP {status}")))),
            _ => return Err(Failure::Fatal(net(&format!("HTTP {status}")))),
        };
        let end = remaining.map(|r| self.len + r);
        if let (Some(end), Some(size)) = (end, src.size)
            && end != size
        {
            return Err(Failure::Fatal(net(&format!(
                "server has {end} bytes, expected {size}"
            ))));
        }
        if self.total == 0 {
            self.total = end.unwrap_or(0);
        }

        let mut body = resp.into_body().into_reader();
        let mut buf = vec![0; CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(Failure::Fatal(ModelError::Cancelled));
            }
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Failure::Retry(net(&e))),
            };
            self.file.write_all(&buf[..n])?;
            self.hasher.update(&buf[..n]);
            self.len += n as u64;
            self.report(false);
        }
        // Without a length header a short body only shows against the
        // catalog or Hub size.
        match end.or(src.size) {
            Some(end) if self.len < end => Err(Failure::Retry(net(&"connection closed early"))),
            _ => Ok(()),
        }
    }
}

/// `bytes <start>-<end>/<total|*>` -> (start, total)
fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let (range, total) = v.strip_prefix("bytes ")?.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    Some((start.trim().parse().ok()?, total))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    use super::*;

    /// How the fake server answers request number `n` (0-based).
    #[derive(Clone, Copy)]
    enum Reply {
        /// Honor `Range`; send at most `limit` body bytes, then hang up.
        Ranged { limit: Option<usize> },
        /// Ignore `Range`, always 200 with the whole file.
        Full,
        /// Honor `Range`; pause after `after` body bytes before the rest.
        Stall { after: usize },
        /// 200 without `Content-Length`; send `limit` bytes, then hang up.
        Unsized { limit: usize },
    }

    struct Server {
        url: String,
        /// `Range` header of each request (None = no Range).
        ranges: Arc<Mutex<Vec<Option<String>>>>,
    }

    fn serve(data: Vec<u8>, replies: Vec<Reply>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/o/r/resolve/c/model.gguf",
            listener.local_addr().unwrap()
        );
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen = ranges.clone();
        std::thread::spawn(move || {
            for (n, conn) in listener.incoming().enumerate() {
                let Ok(conn) = conn else { return };
                let reply = replies
                    .get(n)
                    .copied()
                    .unwrap_or(Reply::Ranged { limit: None });
                let range = answer(conn, &data, reply);
                seen.lock().unwrap().push(range);
            }
        });
        Server { url, ranges }
    }

    fn answer(mut conn: TcpStream, data: &[u8], reply: Reply) -> Option<String> {
        let mut range = None;
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("range")
            {
                range = Some(v.trim().to_owned());
            }
        }
        let start = range
            .as_deref()
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.strip_suffix('-'))
            .map(|s| s.parse::<usize>().unwrap());
        let (head, body) = match (reply, start) {
            (Reply::Ranged { .. } | Reply::Stall { .. }, Some(start)) => (
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{}\r\n",
                    data.len() - start,
                    data.len() - 1,
                    data.len()
                ),
                &data[start..],
            ),
            _ => (
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n", data.len()),
                data,
            ),
        };
        if let Reply::Unsized { limit } = reply {
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
            let _ = conn.write_all(&data[..limit]);
            return range;
        }
        let _ = conn.write_all(format!("{head}Connection: close\r\n\r\n").as_bytes());
        match reply {
            Reply::Ranged { limit } => {
                let _ = conn.write_all(&body[..body.len().min(limit.unwrap_or(usize::MAX))]);
            }
            Reply::Full => {
                let _ = conn.write_all(body);
            }
            Reply::Stall { after } => {
                let _ = conn.write_all(&body[..after]);
                std::thread::sleep(Duration::from_millis(300));
                let _ = conn.write_all(&body[after..]);
            }
            Reply::Unsized { .. } => unreachable!("answered above"),
        }
        range
    }

    fn data() -> (Vec<u8>, String) {
        let data: Vec<u8> = (0..1_500_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let sha = hex::encode(Sha256::digest(&data));
        (data, sha)
    }

    fn agent() -> Agent {
        Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent()
    }

    fn run(
        server: &Server,
        sha: &str,
        size: Option<u64>,
        dest: &Path,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), ModelError> {
        let src = Source {
            url: &server.url,
            sha256: sha,
            size,
        };
        fetch(&agent(), &src, dest, on_progress, cancel)
    }

    #[test]
    fn resumes_after_the_connection_is_cut() {
        let (data, sha) = data();
        let server = serve(
            data.clone(),
            vec![Reply::Ranged {
                limit: Some(600_000),
            }],
        );
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("o/r/c/model.gguf");
        let mut last = (0, 0);
        run(
            &server,
            &sha,
            Some(data.len() as u64),
            &dest,
            &CancelToken::default(),
            &mut |d, t| {
                last = (d, t);
            },
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists());
        assert_eq!(last, (data.len() as u64, data.len() as u64));
        assert_eq!(
            *server.ranges.lock().unwrap(),
            [None, Some("bytes=600000-".to_owned())]
        );
    }

    #[test]
    fn a_short_body_without_length_resumes_against_the_known_size() {
        let (data, sha) = data();
        let server = serve(data.clone(), vec![Reply::Unsized { limit: 600_000 }]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        run(
            &server,
            &sha,
            Some(data.len() as u64),
            &dest,
            &CancelToken::default(),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert_eq!(
            *server.ranges.lock().unwrap(),
            [None, Some("bytes=600000-".to_owned())]
        );
    }

    #[test]
    fn restarts_when_the_server_ignores_range() {
        let (data, sha) = data();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        // A stale part with wrong bytes: only a full restart can verify.
        fs::write(part_path(&dest), vec![0xAA; 1000]).unwrap();
        let server = serve(data.clone(), vec![Reply::Full]);
        run(
            &server,
            &sha,
            None,
            &dest,
            &CancelToken::default(),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert_eq!(
            *server.ranges.lock().unwrap(),
            [Some("bytes=1000-".to_owned())]
        );
    }

    #[test]
    fn checksum_mismatch_deletes_the_part() {
        let (data, _) = data();
        let server = serve(data, vec![]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        let wrong = "0".repeat(64);
        let err = run(
            &server,
            &wrong,
            None,
            &dest,
            &CancelToken::default(),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, ModelError::Checksum { .. }), "{err}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn cancel_deletes_the_part() {
        let (data, sha) = data();
        let server = serve(data.clone(), vec![Reply::Stall { after: 600_000 }]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        let cancel = CancelToken::default();
        let err = run(
            &server,
            &sha,
            Some(data.len() as u64),
            &dest,
            &cancel,
            &mut |d, _| {
                if d > 0 {
                    cancel.cancel();
                }
            },
        )
        .unwrap_err();
        assert!(matches!(err, ModelError::Cancelled), "{err}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn a_part_left_by_an_earlier_call_resumes() {
        let (data, sha) = data();
        let server = serve(data.clone(), vec![]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        fs::write(part_path(&dest), &data[..600_000]).unwrap();
        let mut first = None;
        run(
            &server,
            &sha,
            Some(data.len() as u64),
            &dest,
            &CancelToken::default(),
            &mut |d, _| {
                first.get_or_insert(d);
            },
        )
        .unwrap();
        assert_eq!(
            first,
            Some(600_000),
            "progress starts at the resumed offset"
        );
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert_eq!(
            *server.ranges.lock().unwrap(),
            [Some("bytes=600000-".to_owned())]
        );
    }

    #[test]
    fn content_range_is_parsed() {
        assert_eq!(parse_content_range("bytes 10-19/20"), Some((10, Some(20))));
        assert_eq!(parse_content_range("bytes 10-19/*"), Some((10, None)));
        assert_eq!(parse_content_range("items 1-2/3"), None);
    }
}
