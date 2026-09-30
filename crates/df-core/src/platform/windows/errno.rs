//! What an `io::Error`'s OS code means, asked by name, in Win32's table.
//!
//! On Windows `std` reports `GetLastError` codes, not the C runtime's errno —
//! so `libc::EEXIST` (the CRT's 17) is the same number as
//! `ERROR_NOT_SAME_DEVICE`, and a check written against `libc` there reads a
//! cross-volume move as "something is in the way" and clears the destination.
//! The codes are named here from `winerror.h`: a handful of integers do not
//! need a bindings crate.
//!
//! One question needs more than the code: [`is_delete_pending`], whose
//! answer is in the NT status the thread's last failed call ended on, read
//! with `RtlGetLastNtStatus` — ntdll's, and not in `windows-sys`, so declared
//! here. It takes nothing and reads the calling thread's own record, which
//! is the whole of its `unsafe`.

#![allow(unsafe_code)] // RtlGetLastNtStatus, which reads this thread's record

use std::io;

const ERROR_TOO_MANY_OPEN_FILES: i32 = 4;
const ERROR_ACCESS_DENIED: i32 = 5;
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
            ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_TOO_MANY_OPEN_FILES | ERROR_BUSY
        )
    )
}

/// `STATUS_DELETE_PENDING`, from `ntstatus.h`: the file has been marked for
/// deletion by a handle that is still open, and will be gone when it closes.
const STATUS_DELETE_PENDING: i32 = 0xC000_0056_u32 as i32;
/// `STATUS_FILE_DELETED`: a call on a handle whose file has since been
/// deleted — a listing still open on a folder somebody removed.
const STATUS_FILE_DELETED: i32 = 0xC000_0123_u32 as i32;
/// `STATUS_OBJECT_NAME_NOT_FOUND`, `STATUS_OBJECT_PATH_NOT_FOUND`,
/// `STATUS_NO_SUCH_FILE`: the name is not there.
const STATUS_NOT_THERE: [i32; 3] = [
    0xC000_0034_u32 as i32,
    0xC000_003A_u32 as i32,
    0xC000_000F_u32 as i32,
];

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetLastNtStatus() -> i32;
}

/// Whether `e`, an `ERROR_ACCESS_DENIED`, is a name another deleter has
/// already marked for deletion — `STATUS_DELETE_PENDING` underneath, which
/// Win32 reports as access denied — and so is as good as gone. So is a call on
/// a folder deleted since it was opened (`STATUS_FILE_DELETED`: a listing still
/// open when the other deleter removed it). Also when the last status says
/// the name is not there at all: `std`'s `symlink_metadata`
/// answers a denied open by looking the name up in its directory, and returns
/// the first error when the name has gone meanwhile.
///
/// Asked straight after the call that failed, on the thread that made it:
/// the status is the thread's record of its last failure, which the next
/// failing call replaces.
pub fn is_delete_pending(e: &io::Error) -> bool {
    if e.raw_os_error() != Some(ERROR_ACCESS_DENIED) {
        return false;
    }
    // SAFETY: no arguments; it reads the calling thread's own environment
    // block, which exists for as long as the thread does.
    let status = unsafe { RtlGetLastNtStatus() };
    status == STATUS_DELETE_PENDING
        || status == STATUS_FILE_DELETED
        || STATUS_NOT_THERE.contains(&status)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

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

    /// A file somebody has opened with delete-on-close is delete-pending until
    /// they close it; opening it meanwhile is refused with access denied,
    /// which reads as gone. A real refusal does not.
    #[test]
    fn a_name_marked_for_deletion_is_as_good_as_gone() {
        use std::os::windows::fs::OpenOptionsExt;
        const DELETE: u32 = 0x0001_0000;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        const FILE_SHARE_ALL: u32 = 0x7;
        let t = crate::test_support::TempTree::new("win-delete-pending");
        let path = t.file("going.txt", b"x");
        let holder = std::fs::OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(&path)
            .expect("open for delete");
        // Marked, but the name lingers while `holder` is open — unless the
        // file system unlinks it at once (POSIX semantics), when the open
        // below says not found, which is gone too.
        let _ = std::fs::remove_file(&path);
        match std::fs::OpenOptions::new().read(true).open(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => assert!(is_delete_pending(&e), "{e:?}"),
            Ok(_) => panic!("a file marked for deletion opened"),
        }
        drop(holder);
        assert!(!path.exists());

        // Access denied for any other reason is not gone.
        let denied = std::fs::OpenOptions::new()
            .read(true)
            .open(r"C:\System Volume Information");
        if let Err(e) = denied {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                assert!(!is_delete_pending(&e), "{e:?}");
            }
        }
        assert!(!is_delete_pending(&code(ERROR_SHARING_VIOLATION)));
    }

    /// What Windows answers, call by call, about a folder that is being
    /// deleted by someone else — marked and still named, or already unlinked
    /// while a listing of it is open — and that each answer that is an error
    /// reads as gone. The codes and statuses are printed, since they are what
    /// this rule is made of.
    #[test]
    fn every_answer_about_a_folder_being_deleted_reads_as_gone() {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
        };
        const DELETE: u32 = 0x0001_0000;
        const FILE_SHARE_ALL: u32 = 0x7;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

        fn check(what: &str, result: io::Result<()>) {
            let Err(e) = result else {
                eprintln!("{what}: ok");
                return;
            };
            // SAFETY: as in `is_delete_pending`; asked straight after.
            let status = unsafe { RtlGetLastNtStatus() } as u32;
            let gone = e.kind() == io::ErrorKind::NotFound || is_delete_pending(&e);
            eprintln!("{what}: {e:?}, status {status:#010x}, gone {gone}");
            assert!(gone, "{what}: {e:?}, status {status:#010x}");
        }

        // A folder marked for deletion by a handle still open: the name
        // lingers until it closes.
        let t = crate::test_support::TempTree::new("win-folder-going");
        let marked = t.dir("marked");
        let holder = std::fs::OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&marked)
            .unwrap();
        let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        // SAFETY: the handle is `holder`'s, opened for delete; `info` is a
        // local of the class's struct and its size is passed.
        let ok = unsafe {
            SetFileInformationByHandle(
                holder.as_raw_handle() as _,
                FileDispositionInfo,
                std::ptr::addr_of!(info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        check(
            "lstat of a marked folder",
            std::fs::symlink_metadata(&marked).map(drop),
        );
        check(
            "listing a marked folder",
            std::fs::read_dir(&marked).map(drop),
        );
        check("removing a marked folder", std::fs::remove_dir(&marked));
        check(
            "a file in a marked folder",
            std::fs::write(marked.join("x"), b"x"),
        );
        drop(holder);

        // A folder deleted while a listing of it is open.
        let listed = t.dir("listed");
        for i in 0..40 {
            std::fs::write(listed.join(format!("{i:02}.txt")), b"x").unwrap();
        }
        let mut listing = std::fs::read_dir(&listed).unwrap();
        let _first = listing.next();
        for i in 0..40 {
            let _ = std::fs::remove_file(listed.join(format!("{i:02}.txt")));
        }
        if let Err(e) = std::fs::remove_dir(&listed) {
            // Not this rule's to judge: the listing's handle kept it.
            eprintln!("removing a folder being listed: {e:?}; nothing more to see");
            return;
        }
        for n in 0..100 {
            match listing.next() {
                None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    check(&format!("listing on, entry {n}"), Err(e));
                    break;
                }
            }
        }
    }
}
