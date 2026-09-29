//! What the trash view says about what it cannot see, on Linux: nothing.
//! The view reads the freedesktop trash itself, the one every other program
//! on the desktop writes, so it lists all of it and "Empty trash" empties
//! all of it.

/// The line under an empty trash view: none of its own, so the trash's
/// clock says its piece there.
pub const LISTED_NOTE: Option<&str> = None;

/// What "Empty trash" adds to its toast: nothing.
pub const EMPTIED_NOTE: Option<&str> = None;
