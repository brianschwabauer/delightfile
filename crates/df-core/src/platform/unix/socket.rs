//! The unix socket `rclone rcd` serves its remote control on: connecting to
//! it with the caller's timeout, and making the directory it lives in private
//! to this user, which is the socket's only authentication (`--rc-no-auth`).

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Whether the rclone daemon can be spoken to here: yes, over a unix socket.
pub const AVAILABLE: bool = true;

/// One connection to the daemon.
pub type Stream = UnixStream;

/// Connect to the socket at `socket`, with `timeout` on every read and write.
pub fn connect(socket: &Path, timeout: Duration) -> io::Result<Stream> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

/// Make `dir` if needed, and insist it is this user's and nobody else's.
///
/// The socket has no authentication of its own (`--rc-no-auth`); the
/// directory's mode is its authentication. A directory someone else owns is
/// refused rather than used, and one of ours that has been opened up is closed
/// again.
pub fn private_dir(dir: &Path, uid: u32) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not a directory this user owns",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
