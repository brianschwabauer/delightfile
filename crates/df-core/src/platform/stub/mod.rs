//! Honest stubs, shared by every target that has no native body of a feature
//! yet — which is why this module is selected with `not(target_os = "linux")`
//! rather than named after a target. Today that is Windows alone: macOS has a
//! body of its own for every one (Phase 2).
//!
//! One copy rather than two identical ones: `macos` and `windows` each re-export
//! the stubs they stand behind, so a native body lands by replacing that one
//! re-export (`pub use super::stub::watch;` becomes `pub mod watch;`) and the
//! other target is untouched. Every stub says in its doc comment which task
//! replaces it on which target.
//!
//! What a stub does is fixed by `plans/other-platforms/00-ground-rules.md` §2:
//! an operation fails with [`crate::DfError::Unsupported`], a query answers
//! "nothing", a service comes up inert. Never a panic, never a silent success.
//!
//! A stub only Windows still stands behind is compiled only there
//! (`#[cfg(windows)]` on its line below), so it is not dead code on macOS,
//! which has its own body.

#[cfg(windows)]
pub mod fs;
#[cfg(windows)]
pub mod nofollow;
#[cfg(windows)]
pub mod thread;
#[cfg(windows)]
pub mod user;
#[cfg(windows)]
pub mod xattr;
