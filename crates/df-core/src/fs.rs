//! The directory model: reading a directory, watching it, and putting it in
//! order.
//!
//! Reads are async and results arrive on a channel, because a cold NFS mount or
//! a directory of 200k files must never be something the event loop waits on.
//! Sorting, `sort_dir_first`, the hidden-file toggle, filtering and find are
//! pure functions over the entry list (PLAN §2, §9) so they can be tested
//! against a fixture tree of gnarly names rather than by looking at the screen.
//! Later this is also where the archive and SFTP virtual filesystems attach —
//! one entry-list shape, several sources.
