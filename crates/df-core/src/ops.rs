//! File operations and the op journal that makes them reversible.
//!
//! Every mutation — move, rename, copy, trash, symlink, create — records its
//! inverse before it runs, and `u` replays it backwards (PLAN §5). Trash goes
//! through the freedesktop trash spec, hand-rolled, precisely so that `d` is
//! always undoable; permanent delete is the one operation with no inverse, and
//! it is the only one that gets a confirm dialog. Copies are reflink-first
//! (`FICLONE`) with a read/write fallback, so duplicating a 40 GB file on btrfs
//! is instant and on ext4 is merely a copy.
