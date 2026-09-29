//! Just enough HTTP/1.1 to `POST` a JSON body to `rclone rcd` over a unix
//! socket and read the answer.
//!
//! The same house rule as [`super::json`] and [`super::wire`]: the protocol is
//! small and fixed, so it is written here rather than borrowed. What is
//! written is the *client half of one exchange* and nothing else — no
//! keep-alive, no redirects, no TLS, no proxies, no cookies. Each call opens a
//! connection, sends one request with `Connection: close`, reads one response
//! and drops the socket. The socket is a local file (see `super::rclone`); a
//! connect costs microseconds, and one connection per call means no
//! connection state that could desynchronise between two calls.
//!
//! ## What a response may look like
//!
//! rclone's server is Go's `net/http`, which frames a body one of two ways:
//! `Content-Length` when the handler's whole reply fits the server's buffer,
//! and `Transfer-Encoding: chunked` when it does not — which, for rclone, is
//! any listing of more than a few dozen files. Both are read here, and so is
//! the third framing HTTP/1.1 allows, a body that runs to the close of the
//! connection. Header names are compared case-insensitively (RFC 9110 §5.1).
//!
//! ## Bounds
//!
//! The status line and every header line are capped at [`MAX_LINE`], the
//! header block at [`MAX_HEADERS`] lines, and the body at [`MAX_BODY`]. Each is
//! checked *before* the bytes are kept, so a server that claims a four
//! gigabyte body costs a refusal rather than an allocation. Every read and
//! write has the caller's timeout, which is how [`super::OP_TIMEOUT`] reaches
//! the socket.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::Duration;

/// The longest status or header line accepted, in bytes.
pub const MAX_LINE: usize = 16 * 1024;

/// How many header lines a response may carry.
pub const MAX_HEADERS: usize = 100;

/// The largest body accepted.
///
/// 256 MiB. The biggest thing rclone sends is a directory listing, at a few
/// hundred bytes per entry; this is a directory of about a million files,
/// which is past what a person browses in a file manager and short of what a
/// hostile or confused peer could make this process allocate.
pub const MAX_BODY: usize = 256 * 1024 * 1024;

/// One response: the status code and the body, with the framing removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Why an exchange did not produce a response.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Connecting, writing or reading failed at the socket.
    #[error("{0}")]
    Io(#[source] std::io::Error),
    /// Nothing arrived within the timeout.
    #[error("no answer within the timeout")]
    TimedOut,
    /// The connection closed before the response was complete.
    #[error("the connection closed mid-response")]
    Closed,
    /// The bytes were not an HTTP/1.x response.
    #[error("not an HTTP response: {0}")]
    Malformed(&'static str),
    /// The body is bigger than [`MAX_BODY`].
    #[error("the response is larger than {MAX_BODY} bytes")]
    TooLarge,
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> HttpError {
        match e.kind() {
            // What a read or write that ran into `set_read_timeout` returns.
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => HttpError::TimedOut,
            std::io::ErrorKind::UnexpectedEof => HttpError::Closed,
            _ => HttpError::Io(e),
        }
    }
}

/// `POST /<path>` with a JSON body over the unix socket at `socket`.
pub fn post(
    socket: &Path,
    path: &str,
    body: &str,
    timeout: Duration,
) -> Result<Response, HttpError> {
    let mut stream = crate::platform::socket::connect(socket, timeout).map_err(HttpError::Io)?;
    // One write for head and body: a request small enough to fit one socket
    // buffer goes out as one segment, and rclone's server never sees half a
    // header.
    let request = format!(
        "POST /{path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    read_response(stream)
}

/// Read one response off `reader`. Separate from [`post`] so the framing is a
/// test over bytes rather than something that needs a server.
pub fn read_response(reader: impl Read) -> Result<Response, HttpError> {
    let mut reader = BufReader::new(reader);

    let status_line = read_line(&mut reader)?.ok_or(HttpError::Closed)?;
    let status = parse_status(&status_line)?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut count = 0usize;
    loop {
        let line = read_line(&mut reader)?.ok_or(HttpError::Closed)?;
        if line.is_empty() {
            break;
        }
        count += 1;
        if count > MAX_HEADERS {
            return Err(HttpError::Malformed("too many header lines"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpError::Malformed("a header line with no `:`"));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            // Digits and nothing else (RFC 9110 §8.6): `parse` would also
            // take a leading `+`, and a length a strict peer reads differently
            // from this one is the start of a desynchronised stream.
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(HttpError::Malformed(
                    "a Content-Length that is not a number",
                ));
            }
            let length: usize = value
                .parse()
                .map_err(|_| HttpError::Malformed("a Content-Length that is not a number"))?;
            // Two different lengths is the request-smuggling shape; refusing it
            // is cheaper than deciding which one to believe.
            if content_length.is_some_and(|earlier| earlier != length) {
                return Err(HttpError::Malformed("two different Content-Lengths"));
            }
            content_length = Some(length);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            // The last coding is the one that frames the body (RFC 9112 §6.1).
            let last = value.rsplit(',').next().unwrap_or("").trim();
            if last.eq_ignore_ascii_case("chunked") {
                chunked = true;
            } else {
                return Err(HttpError::Malformed("a transfer coding other than chunked"));
            }
        }
    }

    // A 1xx, 204 or 304 has no body whatever the headers say; rclone sends
    // none of them, but a reader that waited for one would wait out its
    // timeout on a response that was already complete.
    if (100..200).contains(&status) || status == 204 || status == 304 {
        return Ok(Response {
            status,
            body: Vec::new(),
        });
    }

    // Chunked wins over a length when a server sends both (RFC 9112 §6.3).
    let body = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(length) = content_length {
        if length > MAX_BODY {
            return Err(HttpError::TooLarge);
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
        body
    } else {
        // No framing at all: the body is everything until the server closes,
        // which `Connection: close` has asked it to do.
        let mut body = Vec::new();
        let read = reader
            .by_ref()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut body)?;
        if read > MAX_BODY {
            return Err(HttpError::TooLarge);
        }
        body
    };
    Ok(Response { status, body })
}

/// `HTTP/1.1 200 OK` → 200.
fn parse_status(line: &str) -> Result<u16, HttpError> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(HttpError::Malformed("the status line is not HTTP/1.x"));
    }
    let code = parts.next().unwrap_or("");
    if code.len() != 3 {
        return Err(HttpError::Malformed("the status code is not three digits"));
    }
    code.parse::<u16>()
        .ok()
        .filter(|c| (100..600).contains(c))
        .ok_or(HttpError::Malformed("the status code is not three digits"))
}

/// A chunked body, trailers skipped.
fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    loop {
        let line = read_line(reader)?.ok_or(HttpError::Closed)?;
        // `1a2b;name=value` — chunk extensions are allowed and meaningless here.
        let size_text = line.split(';').next().unwrap_or("").trim();
        // Hex digits and nothing else (RFC 9112 §7.1): `from_str_radix` would
        // also take a leading `+`.
        if size_text.is_empty()
            || size_text.len() > 16
            || !size_text.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(HttpError::Malformed("a chunk size that is not hex"));
        }
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| HttpError::Malformed("a chunk size that is not hex"))?;
        if size == 0 {
            // Trailer fields, if any, then the blank line that ends them.
            let mut trailers = 0usize;
            loop {
                let line = read_line(reader)?.ok_or(HttpError::Closed)?;
                if line.is_empty() {
                    return Ok(body);
                }
                trailers += 1;
                if trailers > MAX_HEADERS {
                    return Err(HttpError::Malformed("too many trailer lines"));
                }
            }
        }
        if body.len() + size > MAX_BODY {
            return Err(HttpError::TooLarge);
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        // Every chunk's data is followed by its own CRLF.
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf)?;
        if &crlf != b"\r\n" {
            return Err(HttpError::Malformed("a chunk that does not end in CRLF"));
        }
    }
}

/// One line without its line ending, or `None` at a clean end of stream.
///
/// CRLF is what HTTP sends; a bare LF is accepted too (RFC 9112 §2.2 lets a
/// recipient), and a line longer than [`MAX_LINE`] is refused before the rest
/// of it is read.
fn read_line(reader: &mut impl BufRead) -> Result<Option<String>, HttpError> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(if read > MAX_LINE {
            HttpError::Malformed("a header line longer than the limit")
        } else {
            HttpError::Closed
        });
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| HttpError::Malformed("a header line that is not UTF-8"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on a bad fixture is the point

    use super::*;
    use std::io::Cursor;

    fn response(bytes: &[u8]) -> Result<Response, HttpError> {
        read_response(Cursor::new(bytes.to_vec()))
    }

    #[test]
    fn a_content_length_body_is_read_exactly() {
        let r = response(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\n\t\"a\": 1\n}",
        )
        .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"{\n\t\"a\": 1\n}");
    }

    #[test]
    fn a_chunked_body_is_reassembled_with_extensions_and_trailers() {
        let r = response(
            b"HTTP/1.1 500 Internal Server Error\r\n\
              transfer-encoding: chunked\r\n\r\n\
              5;ext=1\r\nhello\r\n\
              1\r\n \r\n\
              A\r\n0123456789\r\n\
              0\r\nX-Trailer: yes\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.status, 500);
        assert_eq!(r.body, b"hello 0123456789");
    }

    #[test]
    fn header_names_are_case_insensitive_and_bare_lf_is_accepted() {
        let r = response(b"HTTP/1.1 404 Not Found\nCONTENT-LENGTH: 2\n\nok").unwrap();
        assert_eq!(r.status, 404);
        assert_eq!(r.body, b"ok");
    }

    #[test]
    fn an_unframed_body_runs_to_the_close() {
        let r = response(b"HTTP/1.0 200 OK\r\n\r\nall of it").unwrap();
        assert_eq!(r.body, b"all of it");
    }

    #[test]
    fn a_204_has_no_body() {
        let r = response(b"HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n").unwrap();
        assert_eq!(r.status, 204);
        assert!(r.body.is_empty());
    }

    #[test]
    fn broken_responses_are_refused_not_guessed_at() {
        for (bytes, what) in [
            (&b""[..], "empty"),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
                "short body",
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n",
                "headers cut off",
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel",
                "chunk cut off",
            ),
        ] {
            assert!(
                matches!(response(bytes), Err(HttpError::Closed)),
                "{what}: {:?}",
                response(bytes)
            );
        }
        for (bytes, what) in [
            (&b"SSH-2.0-OpenSSH\r\n\r\n"[..], "not http"),
            (b"HTTP/1.1 20 OK\r\n\r\n", "two-digit status"),
            (b"HTTP/1.1 abc OK\r\n\r\n", "non-numeric status"),
            (b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n", "bad header"),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n",
                "bad length",
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: +5\r\n\r\nhello",
                "a length with a sign",
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 5 5\r\n\r\nhello",
                "a length with a space in it",
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+a\r\n0123456789\r\n0\r\n\r\n",
                "a chunk size with a sign",
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
                "two lengths",
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n",
                "unknown coding",
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
                "bad chunk size",
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabXX0\r\n\r\n",
                "chunk without CRLF",
            ),
        ] {
            assert!(
                matches!(response(bytes), Err(HttpError::Malformed(_))),
                "{what}: {:?}",
                response(bytes)
            );
        }
    }

    #[test]
    fn a_huge_length_claim_is_refused_before_allocating() {
        let claim = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(matches!(
            response(claim.as_bytes()),
            Err(HttpError::TooLarge)
        ));
        let chunk = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
            MAX_BODY + 1
        );
        assert!(matches!(
            response(chunk.as_bytes()),
            Err(HttpError::TooLarge)
        ));
        let long_line = format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(MAX_LINE));
        assert!(matches!(
            response(long_line.as_bytes()),
            Err(HttpError::Malformed(_))
        ));
    }

    /// The exchange over a real socket, which is a unix socket until W4.32.
    #[cfg(unix)]
    mod over_a_socket {
        use super::*;
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicU32, Ordering};

        /// A private scratch directory for one test's socket, removed on drop —
        /// under the temp dir, never the user's runtime directory — and a socket
        /// path in it short enough for `sun_path`'s 108 bytes.
        struct Scratch {
            dir: std::path::PathBuf,
        }

        impl Scratch {
            fn new(tag: &str) -> Scratch {
                static COUNTER: AtomicU32 = AtomicU32::new(0);
                let name = format!(
                    "df-http-{tag}-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                );
                // `/tmp` when `$TMPDIR` is too deep for a socket inside it.
                let mut dir = std::env::temp_dir().join(&name);
                if dir.as_os_str().len() > 90 {
                    dir = std::path::PathBuf::from("/tmp").join(&name);
                }
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                Scratch { dir }
            }

            fn socket(&self) -> std::path::PathBuf {
                self.dir.join("s.sock")
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        /// A one-shot server: accept one connection, read the request's head and
        /// body, answer with `reply`, hand back what was asked.
        fn serve_once(socket: &Path, reply: &'static [u8]) -> std::thread::JoinHandle<String> {
            let _ = std::fs::remove_file(socket);
            let listener = UnixListener::bind(socket).unwrap();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut head = String::new();
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    head.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                let mut stream = reader.into_inner();
                stream.write_all(reply).unwrap();
                head + &String::from_utf8(body).unwrap()
            })
        }

        #[test]
        fn a_post_over_a_unix_socket_round_trips_both_framings() {
            for reply in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{\"a\":\"1\"}\n"[..],
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n6\r\n:\"1\"}\n\r\n0\r\n\r\n",
        ] {
            let scratch = Scratch::new("post");
            let socket = scratch.socket();
            let server = serve_once(&socket, reply);
            let response = post(&socket, "rc/noop", r#"{"a":"1"}"#, Duration::from_secs(5)).unwrap();
            let request = server.join().unwrap();
            assert_eq!(response.status, 200);
            assert_eq!(response.body, b"{\"a\":\"1\"}\n");
            assert!(request.starts_with("POST /rc/noop HTTP/1.1\r\n"), "{request}");
            assert!(request.contains("Host: localhost\r\n"), "{request}");
            assert!(request.contains("Content-Type: application/json\r\n"));
            assert!(request.contains("Content-Length: 9\r\n"), "{request}");
            assert!(request.contains("Connection: close\r\n"));
            assert!(request.ends_with("\r\n\r\n{\"a\":\"1\"}"), "{request}");
        }
        }

        #[test]
        fn a_silent_server_times_out_and_a_missing_one_is_an_io_error() {
            let scratch = Scratch::new("silent");
            let socket = scratch.socket();
            let listener = UnixListener::bind(&socket).unwrap();
            // Accept and then say nothing, holding the connection open.
            let held = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                std::thread::sleep(Duration::from_millis(600));
                drop(stream);
            });
            let error = post(&socket, "rc/noop", "{}", Duration::from_millis(150)).unwrap_err();
            assert!(matches!(error, HttpError::TimedOut), "{error}");
            held.join().unwrap();
            let _ = std::fs::remove_file(&socket);

            let error = post(&socket, "rc/noop", "{}", Duration::from_millis(150)).unwrap_err();
            assert!(matches!(error, HttpError::Io(_)), "{error}");
        }
    }
}
