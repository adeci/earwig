//! A local HTTP server for tests: a handler gets each request and writes
//! the whole reply to the connection, which then closes.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};

/// What the server read of a request.
pub(crate) struct Request {
    /// With its query, as the client sent it.
    pub path: String,
    /// The `Range` header, if there is one.
    pub range: Option<String>,
}

/// Serve every connection, the `n`th from 0 with `answer(n, request, conn)`,
/// until the test ends; `handler(url)` makes `answer` once the server's
/// `http://127.0.0.1:<port>` is known, and that is returned.
pub(crate) fn serve<A>(handler: impl FnOnce(&str) -> A) -> String
where
    A: FnMut(usize, &Request, &mut TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut answer = handler(&url);
    std::thread::spawn(move || {
        for (n, conn) in listener.incoming().enumerate() {
            let Ok(mut conn) = conn else { return };
            let request = read(&conn);
            answer(n, &request, &mut conn);
        }
    });
    url
}

/// The request line and the headers, all of them, so that closing after the
/// reply does not reset the connection.
fn read(conn: &TcpStream) -> Request {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    let path = line.split(' ').nth(1).unwrap_or_default().to_owned();
    let mut range = None;
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            return Request { path, range };
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            range = Some(value.trim().to_owned());
        }
    }
}

/// `200 OK` with `body`, or `404 Not Found` without one.
pub(crate) fn reply(conn: &mut TcpStream, body: Option<&[u8]>) {
    let _ = match body {
        Some(body) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            conn.write_all(&[head.as_bytes(), body].concat())
        }
        None => conn
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    };
}
