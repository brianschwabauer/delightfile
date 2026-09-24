//! delightfile's headless core.
//!
//! Everything a file manager does that is not painting lives here: reading and
//! sorting directories, deciding what a keystroke means, applying an operation
//! and recording how to take it back, and driving the worker pool that does the
//! slow parts. None of it may touch winit, egui or wgpu — the point of the
//! split (PLAN §1) is that this crate is exercised by `cargo test` on a machine
//! with no display, so the logic is proven before a pixel is involved.
//!
//! The modules below are placeholders for Phase 0; each one names the seam it
//! will fill so later work has an obvious home rather than a new file.

pub mod archive;
pub mod config;
pub mod du;
pub mod fs;
pub mod git;
pub mod input;
pub mod keymap;
/// Thread priority: how a background walk gets out of the UI thread's way.
pub mod ops;
pub mod preview;
pub mod state;
pub mod tasks;
/// The shared test fixtures (PLAN §9). `#[cfg(test)]` here, and a real `pub`
/// module for anyone who turns on the `test-support` feature — which is how
/// df-app's tests get the same `TempTree` instead of a second copy of it.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod text;
pub mod thread;
pub mod toml;
pub mod vfs;
pub mod zoxide;

/// Everything this crate can fail at.
///
/// One enum per crate (PLAN §1). Variants carry the path or the line they are
/// about, because "permission denied" without a name is not an error message a
/// person can act on — and every one of these ends up in a toast the user
/// reads, not in a log nobody opens.
#[derive(Debug, thiserror::Error)]
pub enum DfError {
    /// An io call failed against a specific path. `std::io::Error` alone loses
    /// the path, which is the only part the user recognizes.
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A config file said something that could not be understood. The line
    /// number is 1-based and the file keeps loading around it: PLAN §3's rule
    /// is that a bad line warns while the valid ones still apply.
    #[error("{file}:{line}: {message}")]
    Config {
        file: std::path::PathBuf,
        line: usize,
        message: String,
    },

    /// A binding could not be installed — an unparseable chord, or one of the
    /// reserved transport keys (`j k l [ ]`, PLAN §4.3) that a user keymap is
    /// not allowed to take over.
    #[error("keymap: {0}")]
    Keymap(String),

    /// A file operation could not be completed or could not be journalled, and
    /// so could not be undone.
    #[error("{0}")]
    Op(String),

    /// The operation was cancelled by the user. Not a failure — it is here so
    /// callers can tell "you stopped it" apart from "it broke" and stay quiet
    /// about the first.
    #[error("cancelled")]
    Cancelled,
}

impl DfError {
    /// Attach a path to an [`std::io::Error`], which is the shape almost every
    /// io failure in this crate arrives in.
    pub fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> DfError {
        DfError::Io {
            path: path.into(),
            source,
        }
    }
}

/// The crate-wide result alias. Spelled out at every public boundary so a
/// reader never has to guess which error type is in play.
pub type Result<T> = std::result::Result<T, DfError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_names_the_path() {
        let e = DfError::io(
            "/tmp/nope",
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        );
        let text = e.to_string();
        assert!(text.contains("/tmp/nope"), "{text}");
        assert!(text.contains("no such file"), "{text}");
    }
}
