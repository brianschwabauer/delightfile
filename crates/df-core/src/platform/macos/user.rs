//! macOS: the shared Unix uid, and uid/gid → names by asking the system
//! (`getpwuid_r`, `getgrgid_r`), each id once per process.
//!
//! Linux reads `/etc/passwd` and `/etc/group` and never asks libc, because a
//! lookup there can block on NSS in the paint loop. A Mac keeps its people in
//! Open Directory: `/etc/passwd` lists `root`, `daemon` and the `_` service
//! accounts and nobody who logs in, so the files would leave every row a
//! number. The lookups go through `opendirectoryd`, which answers a local
//! account from memory; a network account (a directory server at work) can
//! be slower, so each id is asked once and remembered, answer or not, and the
//! owner linemode pays for a new owner once rather than per row.
//!
//! Names live for the process ([`Box::leak`]), which is what lets the
//! answer be `&'static str` as on Linux: one short string per distinct owner
//! a session ever shows.

// Two libc calls, each in one function below that says what it trusts.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub use crate::platform::unix::user::*;

/// Whose name a cached answer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Id {
    User(u32),
    Group(u32),
}

/// Every id asked about so far, and what the system said.
fn names() -> MutexGuard<'static, HashMap<Id, Option<&'static str>>> {
    static NAMES: OnceLock<Mutex<HashMap<Id, Option<&'static str>>>> = OnceLock::new();
    match NAMES.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        Ok(guard) => guard,
        // A map of names is whole after any panic: every insert is one call.
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The cached answer for `id`, asking the system the first time. The lock is
/// not held while the system is asked, so a slow lookup holds up nobody else.
fn cached(id: Id) -> Option<&'static str> {
    if let Some(answer) = names().get(&id) {
        return *answer;
    }
    let answer = match id {
        Id::User(uid) => lookup_user(uid),
        Id::Group(gid) => lookup_group(gid),
    }
    .map(|name| &*Box::leak(name.into_boxed_str()));
    *names().entry(id).or_insert(answer)
}

/// The user's name, or `None` if the system knows no user by this uid.
pub fn user_name(uid: u32) -> Option<&'static str> {
    cached(Id::User(uid))
}

/// The group's name, or `None` if the system knows no group by this gid.
pub fn group_name(gid: u32) -> Option<&'static str> {
    cached(Id::Group(gid))
}

/// How big the buffer a record's strings are written into may grow: far
/// past any real record, short of an allocation a corrupt answer could ask
/// for.
const MAX_BUFFER: usize = 1 << 20;

/// Ask with a buffer that starts at 1 KiB and doubles on `ERANGE`. `call`
/// makes the lookup into the buffer and answers its return code and, on
/// success, the name.
fn with_buffer(
    mut call: impl FnMut(&mut [libc::c_char]) -> (i32, Option<String>),
) -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; 1024];
    loop {
        let (rc, name) = call(&mut buffer);
        if rc == libc::ERANGE && buffer.len() < MAX_BUFFER {
            let grown = buffer.len() * 2;
            buffer.resize(grown, 0);
            continue;
        }
        if rc != 0 {
            log::debug!(
                "owner lookup failed: {}",
                std::io::Error::from_raw_os_error(rc)
            );
        }
        return name;
    }
}

fn lookup_user(uid: u32) -> Option<String> {
    with_buffer(|buffer| {
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `record` is a correctly sized `passwd` for the call to fill,
        // `buffer` is live for exactly the length passed and holds the strings
        // the record points into, and `found` is a live out-pointer.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                record.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if rc != 0 || found.is_null() {
            return (rc, None);
        }
        // SAFETY: a zero return with `found` set means the record is filled,
        // and `pw_name` points at a NUL-terminated string inside `buffer`,
        // which is still live.
        let name = unsafe { CStr::from_ptr((*found).pw_name) };
        (0, Some(name.to_string_lossy().into_owned()))
    })
}

fn lookup_group(gid: u32) -> Option<String> {
    with_buffer(|buffer| {
        let mut record = std::mem::MaybeUninit::<libc::group>::uninit();
        let mut found: *mut libc::group = std::ptr::null_mut();
        // SAFETY: as `lookup_user`, for a `group` record.
        let rc = unsafe {
            libc::getgrgid_r(
                gid,
                record.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if rc != 0 || found.is_null() {
            return (rc, None);
        }
        // SAFETY: as `lookup_user`: `gr_name` points into the live buffer.
        let name = unsafe { CStr::from_ptr((*found).gr_name) };
        (0, Some(name.to_string_lossy().into_owned()))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    fn id(flag: &str) -> String {
        let out = std::process::Command::new("id").arg(flag).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// The runner's own user and group have the names `id` gives them — the
    /// account a Mac's `/etc/passwd` does not list.
    #[test]
    fn this_user_and_group_have_their_names() {
        assert_eq!(user_name(uid()), Some(id("-un").as_str()));
        // SAFETY: `getgid` takes nothing and cannot fail.
        let gid = unsafe { libc::getgid() };
        assert_eq!(group_name(gid), Some(id("-gn").as_str()));
        assert_eq!(user_name(0), Some("root"));
        // The second answer is the cached one, the same string.
        assert!(std::ptr::eq(
            user_name(uid()).unwrap(),
            user_name(uid()).unwrap()
        ));
    }

    /// An id nobody has is a number on the row, and asking twice asks once.
    #[test]
    fn an_unknown_id_has_no_name() {
        assert_eq!(user_name(3_999_999_999), None);
        assert_eq!(group_name(3_999_999_999), None);
        assert!(names().contains_key(&Id::User(3_999_999_999)));
    }
}
