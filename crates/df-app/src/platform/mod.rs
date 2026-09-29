//! Everything in df-app that differs by operating system, behind one set of
//! names (`plans/other-platforms/00-ground-rules.md` §2).
//!
//! The rest of the program is written once and calls what this module
//! exports. Which body answers is decided here, by the `cfg` on the `mod`
//! declarations below, and nowhere else: code outside this directory never
//! names an operating system. There are no traits and no `dyn` — three
//! targets, one caller each, and the CI matrix compiling all three is what
//! keeps their shapes the same.
//!
//! `linux/` is the program as it has always run, moved here whole: the
//! Wayland data device, the D-Bus client, the file-chooser portal, udisks2
//! and gvfs. `macos/` and `windows/` are honest stand-ins until Phases 2 and
//! 4 give them bodies: a feature with no native body yet answers "not here" —
//! a refusal, an empty listing, `None` — and never panics or pretends to have
//! done something.
//!
//! ## The contract
//!
//! Every target exports exactly what this table lists, with these
//! signatures, and a row changes in the same change as its signature. The
//! Linux modules hold more than this — the portal and the mounts worker are
//! whole programs — but nothing outside `platform/` may use anything the
//! table does not name, because on the other targets nothing else is there.
//!
//! | Item | Signature | Linux | macOS, Windows | Task |
//! |---|---|---|---|---|
//!
//! The rows are the phase's checklist inside the code: each task of
//! `plans/other-platforms/01-platform-seam.md` §3 adds its own as it lands.

#[cfg(target_os = "linux")]
mod linux;
// Empty until the first row of the table lands.
#[allow(unused_imports)]
#[cfg(target_os = "linux")]
pub use linux::*;

/// Stands in on macOS until Phase 2 (`plans/other-platforms/02-macos.md`).
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

/// Stands in on Windows until Phase 4 (`plans/other-platforms/04-windows.md`).
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
