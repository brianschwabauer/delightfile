//! Bodies Linux and macOS share, because both are Unix and the code is the
//! same syscall or the same program on each. A target module re-exports what
//! it takes from here beside what it adds of its own; nothing outside
//! `platform/` names this module.

pub mod open;
pub mod process;
