//! uid/gid → names, by reading `/etc/passwd` and `/etc/group` once.
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

/// Both files are `name:x:id:…`, colon-separated, one record per line. Lines
/// that do not parse are skipped rather than failing the file: a passwd with
/// one corrupt line still knows who you are.
pub(crate) fn parse_id_table(text: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split(':');
        let (Some(name), Some(_), Some(id)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let Ok(id) = id.parse::<u32>() else { continue };
        // First record wins. `/etc/passwd` can legally list two names for one
        // id (`root` and `toor`); the first is the canonical one.
        map.entry(id).or_insert_with(|| name.to_string());
    }
    map
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

/// What the owner linemode draws: `brian brian`, falling back to the number for
/// whichever half is unknown.
pub fn owner_label(uid: u32, gid: u32) -> String {
    let user = user_name(uid)
        .map(str::to_string)
        .unwrap_or_else(|| uid.to_string());
    let group = group_name(gid)
        .map(str::to_string)
        .unwrap_or_else(|| gid.to_string());
    format!("{user} {group}")
}
