//! Who this process runs as: the uid that names a user's own directories
//! (`/run/user/<uid>`, `$topdir/.Trash-<uid>`, a private socket directory) and
//! the thumbnail cache delightfile shares with yazi.

/// The process's user id. `getuid` cannot fail and touches nothing; std simply
/// does not expose it.
pub fn uid() -> u32 {
    #[allow(unsafe_code)]
    unsafe {
        libc::getuid()
    }
}

/// What follows `yazi-` in the thumbnail cache's directory name under the
/// temp directory: the uid, as yazi names it (`yazi-fs` `Xdg::temp_dir`), so
/// the two programs find each other's thumbnails.
pub fn cache_suffix() -> String {
    uid().to_string()
}
