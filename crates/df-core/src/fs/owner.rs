//! uid/gid → names, for the owner linemode.
//!
//! Where the names come from is the platform's ([`crate::platform::user`]):
//! on Linux `/etc/passwd` and `/etc/group`, read once and parsed by
//! [`parse_id_table`] below; elsewhere nothing yet, so owners show as numbers.

use std::collections::HashMap;

/// Both files are `name:x:id:…`, colon-separated, one record per line. Lines
/// that do not parse are skipped rather than failing the file: a passwd with
/// one corrupt line still knows who you are.
///
/// Public, not crate-private, because only Linux's body reads the files: on a
/// platform that has no owner names the parser still stands, tested, for the
/// day one does.
pub fn parse_id_table(text: &str) -> HashMap<u32, String> {
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

/// The user's name, or `None` if this uid has none the platform can tell
/// (on Linux: not in `/etc/passwd`).
pub fn user_name(uid: u32) -> Option<&'static str> {
    crate::platform::user::user_name(uid)
}

/// The group's name, or `None` if this gid has none the platform can tell
/// (on Linux: not in `/etc/group`).
pub fn group_name(gid: u32) -> Option<&'static str> {
    crate::platform::user::group_name(gid)
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
