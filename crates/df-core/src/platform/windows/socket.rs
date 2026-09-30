//! The unix socket `rclone rcd` serves its remote control on, over Windows'
//! own `AF_UNIX` (Windows 10 1803 and later), which `std` has no type for: a
//! WinSock stream socket connected to the path rclone bound, with the
//! caller's timeout on every read and write (W4.32).
//!
//! The timeout is a `select` before each `recv` and `send`, as Unix's
//! `poll` is before the SFTP pipes' reads: the runner showed an `AF_UNIX`
//! socket's `recv` waiting past its `SO_RCVTIMEO`, so the option a TCP
//! socket keeps is not asked. A `send` that `select` has let through can
//! still wait for room past the timeout; the requests rclone is sent are a
//! few hundred bytes, far inside a socket's buffer.
//!
//! The socket has no authentication of its own (`--rc-no-auth`), as on Unix:
//! the directory it is in is its authentication. That directory is
//! `%LOCALAPPDATA%\delightfile\run` ([`dirs`]), made with the ACL it inherits
//! there — the user's own, SYSTEM's and the administrators', as everywhere
//! under the profile — which is what mode 0700 is on Unix. [`private_dir`]
//! makes it and refuses one that is not a plain directory (a link, a
//! junction), and sets no ACL of its own.
//!
//! The path goes to WinSock as UTF-8, which is how rclone, a Go program,
//! hands it over too: the two ends spell the name the same way, so they
//! meet. A path that is not Unicode is refused.
//!
//! The `unsafe` is WinSock's `WSAStartup`, `WSASocketW`, `connect`,
//! `select`, `recv` and `send` (and, for the tests' listener, `bind`,
//! `listen`, `accept` and `SetHandleInformation`), written to the three
//! rules of the other islands:
//!
//! 1. Every socket is owned by exactly one `OwnedSocket` from the moment the
//!    call that made it returns, and closed by its drop (`closesocket`),
//!    once; nothing else closes it and no copy of the handle escapes.
//! 2. Every buffer and struct a call reads or writes is a local or a
//!    borrowed slice that outlives the call, its length passed beside it.
//! 3. Every return is checked, a failure read as `WSAGetLastError`'s code
//!    into an `io::Error`; nothing is assumed to succeed, and no `unsafe`
//!    escapes the file.

#![allow(unsafe_code)] // WinSock calls on sockets an OwnedSocket owns, and on locals

use std::io;
use std::os::windows::io::{AsRawSocket, FromRawSocket, OwnedSocket, RawSocket};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use windows_sys::Win32::Networking::WinSock as ws;

/// Whether the rclone daemon can be spoken to here: yes, over Windows' own
/// unix socket.
pub const AVAILABLE: bool = true;

/// The longest path a socket can be bound at: `sun_path`'s 108 bytes, as
/// Linux's, less the NUL that ends it.
pub const PATH_MAX: usize =
    std::mem::size_of::<ws::SOCKADDR_UN>() - std::mem::offset_of!(ws::SOCKADDR_UN, sun_path) - 1;

/// One connection to the daemon.
#[derive(Debug)]
pub struct Stream {
    socket: OwnedSocket,
    /// How long one read or write may wait.
    timeout: Duration,
}

/// Which way a [`Stream::wait`] waits.
#[derive(Clone, Copy)]
enum Ready {
    Read,
    Write,
}

impl Stream {
    fn raw(&self) -> ws::SOCKET {
        self.socket.as_raw_socket() as ws::SOCKET
    }

    /// Wait up to the stream's timeout for the socket to be readable (data,
    /// or the peer's close) or writable, and say `TimedOut` when it is not.
    fn wait(&self, ready: Ready) -> io::Result<()> {
        let mut set = ws::FD_SET {
            fd_count: 1,
            fd_array: [0; 64],
        };
        set.fd_array[0] = self.raw();
        let limit = ws::TIMEVAL {
            tv_sec: i32::try_from(self.timeout.as_secs()).unwrap_or(i32::MAX),
            tv_usec: self.timeout.subsec_micros() as i32,
        };
        let set_ptr = std::ptr::addr_of_mut!(set);
        let (read, write) = match ready {
            Ready::Read => (set_ptr, std::ptr::null_mut()),
            Ready::Write => (std::ptr::null_mut(), set_ptr),
        };
        // SAFETY: `set` and `limit` are locals that outlive the call; `set`
        // holds one socket, this stream's own and open, and is the one set
        // the call may write; the other set is null, as it allows.
        let n = unsafe { ws::select(0, read, write, std::ptr::null_mut(), &limit) };
        if checked(n)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no answer within the timeout",
            ));
        }
        Ok(())
    }
}

impl io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.wait(Ready::Read)?;
        let len = i32::try_from(buf.len()).unwrap_or(i32::MAX);
        // SAFETY: `buf` is a writable slice of at least `len` bytes that
        // outlives the call; the socket is this stream's own.
        let n = unsafe { ws::recv(self.raw(), buf.as_mut_ptr(), len, 0) };
        checked(n)
    }
}

impl io::Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.wait(Ready::Write)?;
        let len = i32::try_from(buf.len()).unwrap_or(i32::MAX);
        // SAFETY: `buf` is a readable slice of at least `len` bytes that
        // outlives the call; the socket is this stream's own.
        let n = unsafe { ws::send(self.raw(), buf.as_ptr(), len, 0) };
        checked(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Connect to the socket at `socket`, with `timeout` on every read and write.
pub fn connect(socket: &Path, timeout: Duration) -> io::Result<Stream> {
    let address = address_of(socket)?;
    let stream = Stream {
        socket: new_socket()?,
        timeout,
    };
    // SAFETY: the socket is the stream's own and open; `address` is a local
    // `SOCKADDR_UN` whose size is passed with it and which outlives the call,
    // which only reads it.
    let code = unsafe {
        ws::connect(
            stream.raw(),
            std::ptr::addr_of!(address).cast::<ws::SOCKADDR>(),
            std::mem::size_of::<ws::SOCKADDR_UN>() as i32,
        )
    };
    checked(code)?;
    Ok(stream)
}

/// Make `dir` if needed, with the ACL it inherits, and insist it is a plain
/// directory.
///
/// Under `%LOCALAPPDATA%` the inherited ACL is the user's own (see the module
/// note), which is the socket's authentication. A link or a junction in its
/// place — something reading as a directory that is somewhere else — is
/// refused rather than followed. `uid` is Unix's owner check, which the ACL
/// stands in for here.
pub fn private_dir(dir: &Path, _uid: u32) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    // `symlink_metadata`: a link or a junction is itself, and not a directory.
    if !std::fs::symlink_metadata(dir)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not a plain directory",
        ));
    }
    Ok(())
}

/// Where a socket may go when the caller names nowhere:
/// `%LOCALAPPDATA%\delightfile\run`, alone. There is no shorter place that is
/// the user's alone to fall back on, as `/tmp` is on Unix; a profile path long
/// enough to leave no room for the socket's name is refused by the caller in
/// words.
pub fn dirs(_uid: u32) -> Vec<PathBuf> {
    super::dirs::state_dir()
        .map(|local| local.join("delightfile").join("run"))
        .into_iter()
        .collect()
}

/// WinSock, started once for the process: `WSAStartup` asking for 2.2. Its
/// matching cleanup is never called, as `std`'s is not; the process's end
/// does it.
fn started() -> io::Result<()> {
    static STARTED: OnceLock<i32> = OnceLock::new();
    let code = *STARTED.get_or_init(|| {
        let mut data = std::mem::MaybeUninit::<ws::WSADATA>::uninit();
        // SAFETY: `data` is a local the call writes and nothing reads; it
        // outlives the call.
        unsafe { ws::WSAStartup(0x0202, data.as_mut_ptr()) }
    });
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code));
    }
    Ok(())
}

/// A new `AF_UNIX` stream socket, owned, which a child process does not
/// inherit — a daemon started while it is open must not hold its end.
fn new_socket() -> io::Result<OwnedSocket> {
    started()?;
    // SAFETY: plain values and a null protocol description, as the call
    // allows; what it returns is checked before it is owned.
    let raw = unsafe {
        ws::WSASocketW(
            i32::from(ws::AF_UNIX),
            ws::SOCK_STREAM,
            0,
            std::ptr::null(),
            0,
            ws::WSA_FLAG_NO_HANDLE_INHERIT,
        )
    };
    if raw == ws::INVALID_SOCKET {
        return Err(last_error());
    }
    // SAFETY: `raw` is a socket this call just made, open, and held by
    // nothing else; the `OwnedSocket` is its one owner from here.
    Ok(unsafe { OwnedSocket::from_raw_socket(raw as RawSocket) })
}

/// `path` as a `SOCKADDR_UN`: its UTF-8 bytes, NUL-terminated, refused when
/// it is not Unicode, holds a NUL or is longer than [`PATH_MAX`].
fn address_of(path: &Path) -> io::Result<ws::SOCKADDR_UN> {
    let Some(text) = path.to_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a socket's path must be Unicode",
        ));
    };
    let bytes = text.as_bytes();
    if bytes.len() > PATH_MAX || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a socket's path must be shorter than 108 bytes, with no NUL",
        ));
    }
    let mut address = ws::SOCKADDR_UN {
        sun_family: ws::AF_UNIX,
        sun_path: [0; 108],
    };
    address.sun_path[..bytes.len()].copy_from_slice(bytes);
    Ok(address)
}

/// A WinSock return: `SOCKET_ERROR` is the thread's last WinSock error, and
/// anything else is the count (or nothing) the call answered.
fn checked(code: i32) -> io::Result<usize> {
    if code == ws::SOCKET_ERROR {
        return Err(last_error());
    }
    Ok(usize::try_from(code).unwrap_or(0))
}

/// The calling thread's last WinSock error, which `std` reads with the codes
/// of the rest of Windows (`WSAETIMEDOUT` is `TimedOut`).
fn last_error() -> io::Error {
    // SAFETY: no arguments; reads the calling thread's error.
    io::Error::from_raw_os_error(unsafe { ws::WSAGetLastError() })
}

/// A socket bound for a test to serve on: the listening end [`connect`]
/// reaches, as `UnixListener` is on Unix.
#[cfg(test)]
pub struct Listener {
    socket: OwnedSocket,
}

#[cfg(test)]
impl Listener {
    /// Bind at `path`, which must not exist, and listen.
    pub fn bind(path: &Path) -> io::Result<Listener> {
        let address = address_of(path)?;
        let socket = new_socket()?;
        let raw = socket.as_raw_socket() as ws::SOCKET;
        // SAFETY: `raw` is the open socket `socket` owns; `address` is a
        // local whose size is passed with it and which outlives the call.
        checked(unsafe {
            ws::bind(
                raw,
                std::ptr::addr_of!(address).cast::<ws::SOCKADDR>(),
                std::mem::size_of::<ws::SOCKADDR_UN>() as i32,
            )
        })?;
        // SAFETY: the same open socket; a plain backlog.
        checked(unsafe { ws::listen(raw, 8) })?;
        Ok(Listener { socket })
    }

    /// The next connection, whose reads and writes wait up to ten seconds.
    ///
    /// Not inherited by a child: an accepted socket is, unlike one
    /// `WSASocketW` made with `WSA_FLAG_NO_HANDLE_INHERIT`, and a program
    /// another test starts meanwhile would hold the connection open past
    /// its close.
    pub fn accept(&self) -> io::Result<Stream> {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
        let raw = self.socket.as_raw_socket() as ws::SOCKET;
        // SAFETY: the listening socket this listener owns; no address is
        // asked for, so both out-pointers are null.
        let accepted = unsafe { ws::accept(raw, std::ptr::null_mut(), std::ptr::null_mut()) };
        if accepted == ws::INVALID_SOCKET {
            return Err(last_error());
        }
        // SAFETY: `accepted` is a socket the call just made, owned by
        // nothing else; the stream is its one owner from here.
        let socket = unsafe { OwnedSocket::from_raw_socket(accepted as RawSocket) };
        // SAFETY: a socket is a handle; this one is open and the stream's.
        if unsafe { SetHandleInformation(accepted as HANDLE, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Stream {
            socket,
            timeout: Duration::from_secs(10),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use std::io::{Read, Write};

    /// A scratch folder under the temp dir for one test's socket, removed on
    /// drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("df-sock-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Bytes go both ways over the socket, and the end of the connection
    /// reads as the end of the stream.
    #[test]
    fn a_connection_carries_bytes_both_ways() {
        let scratch = Scratch::new("both");
        let path = scratch.0.join("s.sock");
        let listener = Listener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let mut asked = [0u8; 4];
            stream.read_exact(&mut asked).unwrap();
            stream.write_all(b"pong").unwrap();
            asked
        });
        let mut client = connect(&path, Duration::from_secs(5)).unwrap();
        client.write_all(b"ping").unwrap();
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).unwrap();
        assert_eq!(server.join().unwrap(), *b"ping");
        assert_eq!(answer, b"pong");
    }

    /// A read nobody answers ends at the timeout as `TimedOut`, which the
    /// HTTP client reads as "no answer"; a socket that is not there is an
    /// error at once.
    #[test]
    fn a_silent_peer_times_out_and_a_missing_socket_is_refused() {
        let scratch = Scratch::new("silent");
        let path = scratch.0.join("s.sock");
        let listener = Listener::bind(&path).unwrap();
        let held = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(600));
            drop(stream);
        });
        let mut client = connect(&path, Duration::from_millis(150)).unwrap();
        let started = std::time::Instant::now();
        let got = client.read(&mut [0u8; 16]);
        let waited = started.elapsed();
        assert!(
            matches!(&got, Err(e) if e.kind() == io::ErrorKind::TimedOut),
            "after {waited:?}: {got:?}"
        );
        assert!(waited < Duration::from_millis(500), "{waited:?}");
        held.join().unwrap();
        let missing = connect(&scratch.0.join("nobody.sock"), Duration::from_secs(1));
        assert!(missing.is_err(), "{missing:?}");
    }

    /// [`PATH_MAX`] is what the system binds: a path of exactly that length
    /// binds, and one byte more is refused before the call.
    #[test]
    fn the_longest_socket_path_is_the_platforms() {
        let scratch = Scratch::new("sun");
        let prefix = scratch.0.as_os_str().len() + 1;
        let fits = scratch.0.join("s".repeat(PATH_MAX - prefix));
        let over = scratch.0.join("o".repeat(PATH_MAX - prefix + 1));
        assert_eq!(fits.as_os_str().len(), PATH_MAX);
        let bound = Listener::bind(&fits);
        assert!(Listener::bind(&over).is_err());
        drop(bound.unwrap());
    }

    /// The directory is made, with what it inherits, and a second call finds
    /// it; a file in its place is refused.
    #[test]
    fn the_socket_directory_is_made_and_must_be_a_directory() {
        let scratch = Scratch::new("dir");
        let run = scratch.0.join("delightfile").join("run");
        private_dir(&run, 0).unwrap();
        assert!(run.is_dir());
        private_dir(&run, 0).unwrap();
        let file = scratch.0.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        assert!(private_dir(&file, 0).is_err());
        let expected = super::super::dirs::state_dir().map(|l| l.join("delightfile").join("run"));
        assert_eq!(dirs(0).first(), expected.as_ref());
    }
}
