//! Linux: the shared Unix uid, and uid/gid → names by reading `/etc/passwd`
//! and `/etc/group` once.
//!
//! The owner linemode (`m o`, PLAN §4.1) needs a name per row, and a row that
//! says `1000` is a row that failed. The obvious call is `getpwuid_r`, which is
//! a libc call that may block on NSS — LDAP, `sssd`, a network that is having a
//! bad afternoon — and it would be called once per visible row, on the thread
//! that is painting. Parsing the two files is a few hundred microseconds, once
//! per process, and cannot hang on a network.
//!
//! What that trades away: users who exist only in NSS (LDAP, `systemd-homed`,
//! `nss-mymachines`) are not in the files, so they render as their number. On a
//! single-user workstation, which is what this program is for, the files are
//! the whole truth. If that ever stops being true the fix is a background
//! resolve that fills a cache — not a blocking call in the paint loop.
//!
//! Resolution is lazy in the strong sense: the files are not opened until
//! something actually asks for a name, so a session that never touches the
//! owner linemode never reads them at all.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::fs::owner::parse_id_table;

pub use crate::platform::unix::user::*;

/// `/etc/passwd`, cached. Empty if it could not be read — a container with no
/// passwd file is not an error, it is a place where everyone is a number.
fn users() -> &'static HashMap<u32, String> {
    static USERS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    USERS.get_or_init(|| parse_id_file("/etc/passwd"))
}

/// `/etc/group`, cached, same shape and same third field.
fn groups() -> &'static HashMap<u32, String> {
    static GROUPS: OnceLock<HashMap<u32, String>> = OnceLock::new();
    GROUPS.get_or_init(|| parse_id_file("/etc/group"))
}

fn parse_id_file(path: &str) -> HashMap<u32, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_id_table(&text),
        Err(e) => {
            log::debug!("{path}: {e} — owners will render as numbers");
            HashMap::new()
        }
    }
}

/// The user's name, or `None` if this uid is not in `/etc/passwd`.
pub fn user_name(uid: u32) -> Option<&'static str> {
    users().get(&uid).map(String::as_str)
}

/// The group's name, or `None` if this gid is not in `/etc/group`.
pub fn group_name(gid: u32) -> Option<&'static str> {
    groups().get(&gid).map(String::as_str)
}
