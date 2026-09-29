//! What an `io::Error`'s OS code means, asked by name, in Win32's table.
//!
//! On Windows `std` reports `GetLastError` codes, not the C runtime's errno —
//! so `libc::EEXIST` (the CRT's 17) is the same number as
//! `ERROR_NOT_SAME_DEVICE`, and a check written against `libc` there reads a
//! cross-volume move as "something is in the way" and clears the destination.
//! The codes are named here from `winerror.h`: a handful of integers do not
//! need a bindings crate.

use std::io;

const ERROR_TOO_MANY_OPEN_FILES: i32 = 4;
const ERROR_NOT_SAME_DEVICE: i32 = 17;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;
const ERROR_FILE_EXISTS: i32 = 80;
const ERROR_INVALID_PARAMETER: i32 = 87;
const ERROR_DIR_NOT_EMPTY: i32 = 145;
const ERROR_BUSY: i32 = 170;
const ERROR_ALREADY_EXISTS: i32 = 183;
const ERROR_DIRECTORY: i32 = 267;

/// `ERROR_NOT_SAME_DEVICE`: a rename across volumes, which has to become a
/// copy.
pub fn is_cross_device(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE)
}

/// `ERROR_FILE_EXISTS` or `ERROR_ALREADY_EXISTS`: something already has the
/// name.
pub fn is_exists(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS)
    )
}

/// `ERROR_DIR_NOT_EMPTY`.
pub fn is_not_empty(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY)
}

/// `ERROR_DIRECTORY`: a name that had to be a directory is not one.
pub fn is_not_dir(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_DIRECTORY)
}

/// Never: Win32 has no code for "a directory where something else was
/// expected". Renaming a file over a directory answers
/// `ERROR_ACCESS_DENIED`, which also means a real permission refusal, so it is
/// not read as this.
pub fn is_dir(_e: &io::Error) -> bool {
    false
}

/// `ERROR_INVALID_PARAMETER`.
pub fn is_invalid(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_INVALID_PARAMETER)
}

/// "Not now" rather than "not ever": a sharing or lock violation (another
/// program has the file open), too many open files, a busy device. The codes
/// only; `Interrupted`, `WouldBlock` and `TimedOut` are the caller's to check.
pub fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(
            ERROR_SHARING_VIOLATION
                | ERROR_LOCK_VIOLATION
                | ERROR_TOO_MANY_OPEN_FILES
                | ERROR_BUSY
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(n: i32) -> io::Error {
        io::Error::from_raw_os_error(n)
    }

    /// The cross-volume move is not mistaken for a name in the way, which is
    /// the confusion the CRT's numbers would cause.
    #[test]
    fn a_cross_volume_move_is_not_a_name_in_the_way() {
        assert!(is_cross_device(&code(ERROR_NOT_SAME_DEVICE)));
        assert!(!is_exists(&code(ERROR_NOT_SAME_DEVICE)));
        assert!(is_exists(&code(ERROR_FILE_EXISTS)));
        assert!(is_exists(&code(ERROR_ALREADY_EXISTS)));
        assert!(is_not_empty(&code(ERROR_DIR_NOT_EMPTY)));
        assert!(is_not_dir(&code(ERROR_DIRECTORY)));
        assert!(!is_dir(&code(5)));
        assert!(is_invalid(&code(ERROR_INVALID_PARAMETER)));
    }

    #[test]
    fn the_transient_codes_are_the_four_and_only_those() {
        for n in [
            ERROR_SHARING_VIOLATION,
            ERROR_LOCK_VIOLATION,
            ERROR_TOO_MANY_OPEN_FILES,
            ERROR_BUSY,
        ] {
            assert!(is_transient(&code(n)), "{n}");
        }
        // Access denied and disk full are answers, not delays.
        assert!(!is_transient(&code(5)));
        assert!(!is_transient(&code(112)));
    }
}
