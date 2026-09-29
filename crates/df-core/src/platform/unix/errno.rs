//! What an `io::Error`'s OS code means, asked by name.
//!
//! `raw_os_error()` is a number from the platform's own table: `EAGAIN` is 11
//! on Linux and 35 on macOS, where 11 is `EDEADLK`, and on Windows the number
//! is a Win32 code from a different table altogether. A literal is right on
//! one platform and quietly wrong on the others — and a wrong "transient"
//! retries a failure that will never go away. So every question the crate asks
//! of an error code is one of these predicates, and each target answers it
//! from its own table. Here that table is `libc`'s, which is the right one for
//! Linux and for macOS by construction.

use std::io;

/// `EXDEV`: a rename across filesystems, which has to become a copy.
pub fn is_cross_device(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EXDEV)
}

/// `EEXIST`: something already has the name.
pub fn is_exists(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EEXIST)
}

/// `ENOTEMPTY`: a directory with something in it, where an empty one was
/// needed.
pub fn is_not_empty(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOTEMPTY)
}

/// `ENOTDIR`: a path that had to be a directory, or run through one, is not.
pub fn is_not_dir(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOTDIR)
}

/// `EISDIR`: a directory where something else was expected.
pub fn is_dir(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EISDIR)
}

/// `EINVAL`: the call does not apply to this object — what a filesystem that
/// cannot flush a directory answers `fsync` with.
pub fn is_invalid(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::EINVAL)
}

/// "Not now" rather than "not ever": `EAGAIN`, `EBUSY`, `ENFILE`, `EMFILE`,
/// `ETXTBSY`, `ESTALE`. The codes only; the `ErrorKind`s that mean the same
/// (`Interrupted`, `WouldBlock`, `TimedOut`) are the caller's to check, since
/// they are the same on every platform.
pub fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EAGAIN | libc::EBUSY | libc::ENFILE | libc::EMFILE | libc::ETXTBSY | libc::ESTALE)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(n: i32) -> io::Error {
        io::Error::from_raw_os_error(n)
    }

    /// Each predicate answers its own code and no other.
    #[test]
    fn each_code_is_known_by_its_name() {
        assert!(is_cross_device(&code(libc::EXDEV)));
        assert!(is_exists(&code(libc::EEXIST)));
        assert!(is_not_empty(&code(libc::ENOTEMPTY)));
        assert!(is_not_dir(&code(libc::ENOTDIR)));
        assert!(is_dir(&code(libc::EISDIR)));
        assert!(is_invalid(&code(libc::EINVAL)));
        assert!(!is_cross_device(&code(libc::EEXIST)));
        assert!(!is_exists(&code(libc::EXDEV)));
        assert!(!is_not_dir(&code(libc::EISDIR)));
        assert!(!is_dir(&io::Error::from(io::ErrorKind::NotFound)));
    }

    /// The six "not now" codes, by name — which is what makes the macOS
    /// numbers (`EAGAIN` 35, `ESTALE` 70) right without anyone writing them.
    #[test]
    fn the_transient_codes_are_the_six_and_only_those() {
        for n in [
            libc::EAGAIN,
            libc::EBUSY,
            libc::ENFILE,
            libc::EMFILE,
            libc::ETXTBSY,
            libc::ESTALE,
        ] {
            assert!(is_transient(&code(n)), "{n}");
        }
        for n in [libc::ENOSPC, libc::EACCES, libc::ENOENT, libc::EDEADLK] {
            assert!(!is_transient(&code(n)), "{n}");
        }
        assert!(!is_transient(&io::Error::from(io::ErrorKind::WouldBlock)));
    }
}
