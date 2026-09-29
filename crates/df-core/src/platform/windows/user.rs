//! Who this process runs as, in the numbers the rest of the program was
//! written against. Windows identifies users by SID, not by a small integer.

/// No numeric uid on Windows: `0`, which is also what yazi uses there.
pub fn uid() -> u32 {
    0
}

/// What follows `yazi-` in the thumbnail cache's directory name: `0`, because
/// yazi builds the name from `uid_or_zero()` on every platform
/// (`yazi-fs/src/xdg.rs`, `Xdg::load_temp_dir`), and its Windows uid is zero.
/// `%TEMP%\yazi-0` is the directory both programs share.
pub fn cache_suffix() -> String {
    "0".to_string()
}
