//! What the trash view says about what it cannot see, on macOS.
//!
//! delightfile's trash here is Finder's Trash reached through
//! `NSFileManager`, with a journal of its own to say where each item came
//! from (`plans/other-platforms/02-macos.md` M2.8). The journal knows only
//! what delightfile put there, so the view lists only that, and "Empty
//! trash" empties only that: Finder's own items have no record to restore
//! them by, and emptying them unseen would be destroying what the view never
//! showed. Both places say so.

/// The line under an empty trash view.
pub const LISTED_NOTE: Option<&str> =
    Some("Only files trashed from delightfile are listed — Finder's Trash may hold more");

/// What "Empty trash" adds to its toast.
pub const EMPTIED_NOTE: Option<&str> =
    Some("only what delightfile trashed; Finder's Trash may hold more");
