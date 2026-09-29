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

/// The longest path a socket can be bound at: `sun_path`, less the NUL that
/// ends it. 107 bytes on Linux and 103 on macOS, whose `sun_path` holds 104 —
/// which a path under macOS's `$TMPDIR` (`/var/folders/…/T/`, 49 bytes before
/// a name) runs out of fast.
pub const PATH_MAX: usize = std::mem::size_of::<libc::sockaddr_un>()
    - std::mem::offset_of!(libc::sockaddr_un, sun_path)
    - 1;

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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use std::os::unix::net::UnixListener;

    /// [`PATH_MAX`] is the kernel's own bound, byte for byte: a path of
    /// exactly that length binds, one byte more does not.
    #[test]
    fn the_longest_socket_path_is_the_platforms() {
        let dir = std::path::PathBuf::from("/tmp").join(format!("df-sun-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prefix = dir.as_os_str().len() + 1;
        let fits = dir.join("s".repeat(PATH_MAX - prefix));
        let over = dir.join("o".repeat(PATH_MAX - prefix + 1));
        assert_eq!(fits.as_os_str().len(), PATH_MAX);
        let bound = UnixListener::bind(&fits);
        let refused = UnixListener::bind(&over);
        let _ = std::fs::remove_dir_all(&dir);
        bound.unwrap();
        assert!(refused.is_err());
    }
}
