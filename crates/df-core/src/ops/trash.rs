//! The trash: where `d` puts things so that `u` can bring them back.
//!
//! What a trash *is* belongs to the platform ([`crate::platform::trash`]):
//! the freedesktop.org spec's `files/` and `info/*.trashinfo` on Linux, and
//! nothing yet on macOS and Windows, where every operation says so rather than
//! deleting for good behind the user's back. This module is the name the rest
//! of the program already knew it by, and the collision suffix
//! ([`crate::fs::names`]) that grew up here before paste and the vfs needed it
//! too.

pub use crate::fs::names::{suffixed, MAX_NAME_BYTES, MAX_TRASH_COLLISIONS};
pub use crate::platform::trash::*;
