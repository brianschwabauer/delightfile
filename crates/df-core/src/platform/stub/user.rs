//! No owner names: stands in on macOS until M2.4 (`getpwuid_r`, since macOS
//! keeps its users in Open Directory and `/etc/passwd` lists only system
//! accounts) and on Windows, whose owners are SIDs. The owner linemode then
//! shows the numbers.

/// Not known here.
pub fn user_name(_uid: u32) -> Option<&'static str> {
    None
}

/// Not known here.
pub fn group_name(_gid: u32) -> Option<&'static str> {
    None
}
