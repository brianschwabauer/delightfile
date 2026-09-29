//! No socket to speak to `rclone rcd` on yet: `std` has no unix sockets on
//! Windows, and which transport replaces them (Windows' own `AF_UNIX`, or a
//! loopback port with rclone's password) is W4.32.
//!
//! [`AVAILABLE`] is `false`, so a cloud remote refuses before `rclone` is
//! started ("Cloud remotes is not available on this platform"), through the
//! vfs's spawn error as SFTP's refusal does.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Whether the rclone daemon can be spoken to here: not yet.
pub const AVAILABLE: bool = false;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        crate::DfError::Unsupported("Cloud remotes"),
    )
}

/// A connection that cannot exist: [`connect`] never makes one.
pub enum Stream {}

impl io::Read for Stream {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        match *self {}
    }
}

impl io::Write for Stream {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        match *self {}
    }

    fn flush(&mut self) -> io::Result<()> {
        match *self {}
    }
}

/// Refused (W4.32).
pub fn connect(_socket: &Path, _timeout: Duration) -> io::Result<Stream> {
    Err(unsupported())
}

/// Refused (W4.32): a directory's privacy is an ACL here, not a mode.
pub fn private_dir(_dir: &Path, _uid: u32) -> io::Result<()> {
    Err(unsupported())
}
