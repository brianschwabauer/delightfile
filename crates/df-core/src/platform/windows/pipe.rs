//! No timed waits on a child's pipes yet: Windows pipes are not pollable
//! handles, and the design that replaces `poll` — a reader thread per pipe
//! feeding a channel — is W4.20.
//!
//! [`AVAILABLE`] is `false`, so the SFTP transport refuses before it starts
//! `ssh` ("SFTP is not available on this platform"); the functions below exist
//! so the transport compiles, and would refuse the same way if reached.

use std::io;
use std::time::{Duration, Instant};

/// Whether the child's pipes can be waited on with a deadline here: not yet.
pub const AVAILABLE: bool = false;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        crate::DfError::Unsupported("SFTP"),
    )
}

/// No descriptor numbers on Windows; `-1`, which the calls below never use.
pub fn fd<T>(_pipe: &T) -> i32 {
    -1
}

/// Time left until `deadline`, or `None` if it has passed.
pub fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

/// Refused (W4.20).
pub fn poll_read2(_stdout: i32, _stderr: i32, _timeout: Duration) -> io::Result<(bool, bool)> {
    Err(unsupported())
}

/// Refused (W4.20).
pub fn poll_write(_fd: i32, _timeout: Duration) -> io::Result<bool> {
    Err(unsupported())
}

/// Refused (W4.20).
pub fn set_nonblocking(_fd: i32) -> io::Result<()> {
    Err(unsupported())
}
