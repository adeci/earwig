//! A stand-in for GitHub in tests: its API and release files from a local
//! server.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

use semver::Version;
use sha2::{Digest as _, Sha256};

use super::{Mode, REPO, Updates, asset_name};

/// GitHub's API and files, as routes (path to body); the paths asked for
/// land in `asked`.
pub(crate) struct Feed {
    pub url: String,
    pub asked: Arc<Mutex<Vec<String>>>,
}

impl Feed {
    /// Serves what `routes(url)` makes, 404 for anything else.
    pub fn start(routes: impl FnOnce(&str) -> Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes = routes(&url);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                answer(conn, &routes, &log);
            }
        });
        Self { url, asked }
    }

    /// A build of `running` that downloads into `dir/update` (and on macOS
    /// is `dir/earwig.app`).
    pub fn updates(&self, running: &str, dir: &Path) -> Updates {
        Updates::new(
            Mode::Install,
            Version::parse(running).unwrap(),
            &self.url,
            &self.url,
            dir.join("update"),
            false,
            #[cfg(target_os = "macos")]
            dir.join("earwig.app"),
        )
    }
}

fn answer(mut conn: TcpStream, routes: &[(String, Vec<u8>)], log: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).unwrap();
    let mut header = String::new();
    while reader
        .read_line(&mut header)
        .is_ok_and(|_| !header.trim().is_empty())
    {
        header.clear();
    }
    let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
    let route = path.split('?').next().unwrap_or_default().to_owned();
    log.lock().unwrap().push(path);
    let reply = match routes.iter().find(|(known, _)| *known == route) {
        Some((_, body)) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            [head.as_bytes(), body].concat()
        }
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    let _ = conn.write_all(&reply);
}

/// GitHub's JSON for release `version` with this platform's file, listed
/// with `sha256` and `size`.
pub(crate) fn release(url: &str, version: &str, sha256: &str, size: usize) -> serde_json::Value {
    let name = asset_name(&Version::parse(version).unwrap());
    serde_json::json!({
        "tag_name": format!("v{version}"),
        "draft": false,
        "prerelease": version.contains('-'),
        "assets": [{
            "name": name,
            "size": size,
            "digest": format!("sha256:{sha256}"),
            "state": "uploaded",
            "browser_download_url": format!("{url}/{REPO}/releases/download/v{version}/{name}"),
        }],
    })
}

/// A release file and its SHA-256.
pub(crate) fn data() -> (Vec<u8>, String) {
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 13 % 251) as u8).collect();
    let sha256 = hex::encode(Sha256::digest(&data));
    (data, sha256)
}

/// The newest stable release is 0.2.0, its file `data` listed with `sha256`
/// and `size`; 0.3.0-rc.1 is the newest of all.
pub(crate) fn releases(data: Vec<u8>, sha256: String, size: usize) -> Feed {
    Feed::start(move |url| {
        let stable = release(url, "0.2.0", &sha256, size);
        let next = release(url, "0.3.0-rc.1", &sha256, size);
        let name = asset_name(&Version::parse("0.2.0").unwrap());
        vec![
            (
                format!("/repos/{REPO}/releases/latest"),
                stable.to_string().into_bytes(),
            ),
            (
                format!("/repos/{REPO}/releases"),
                serde_json::json!([next, stable]).to_string().into_bytes(),
            ),
            (format!("/{REPO}/releases/download/v0.2.0/{name}"), data),
        ]
    })
}
