//! No owner names: stands in on Windows, whose owners are SIDs. The owner
//! linemode then shows the numbers. (macOS asks Open Directory,
//! `platform/macos/user.rs`.)

/// Not known here.
pub fn user_name(_uid: u32) -> Option<&'static str> {
    None
}

/// Not known here.
pub fn group_name(_gid: u32) -> Option<&'static str> {
    None
}
