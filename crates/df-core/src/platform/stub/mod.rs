//! Honest stubs, shared by every target that has no native body of a feature
//! yet — today macOS and Windows, which is why this module is selected with
//! `not(target_os = "linux")` rather than named after either.
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

pub mod fs;
pub mod thread;
pub mod trash;
pub mod user;
pub mod watch;
pub mod xattr;
