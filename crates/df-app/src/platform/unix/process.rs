//! The process's own console, on Linux and macOS: nothing to do. A program
//! started from a terminal writes to it and one started from a launcher
//! writes nowhere, with no help from the program, because there is one kind
//! of executable here and no console subsystem to opt out of.

/// Nothing: the standard streams are already whatever the parent gave.
pub fn attach_parent_console() {}
