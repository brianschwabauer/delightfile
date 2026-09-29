//! The desktop's light or dark, on macOS: not asked yet.
//!
//! macOS knows which side it is on and tells a window when that changes, but
//! reading it is Phase 2's (`plans/other-platforms/02-macos.md` M2.30). Until
//! then this watcher starts no thread and hears nothing, which `[flavor]
//! mode = "auto"` takes as dark — what a Linux session with no portal gets —
//! and it says at once that nothing is coming ([`Link::Gone`]), so
//! `theme-auto` says it could not reach the setting rather than claiming to
//! follow it.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use df_core::fs::Notifier;

use crate::appearance::{Link, Scheme};

/// How a watcher reaches the desktop. There is nothing here to reach, so no
/// connection is ever made.
pub type Connect = Arc<dyn Fn() -> Result<Infallible, String> + Send + Sync>;

/// The connection a watcher would ask through, which is never made.
pub fn session() -> Connect {
    Arc::new(|| Err("the desktop's colour scheme is not read on this platform".to_string()))
}

/// A watcher that is never listening.
pub struct Desktop {
    started: Instant,
}

impl Desktop {
    /// A watcher that has already gone: no thread, no answer, and `notify`
    /// is never rung.
    pub fn watch_over(_connect: Connect, _notify: Notifier) -> Desktop {
        Desktop {
            started: Instant::now(),
        }
    }

    /// Nothing has arrived, and nothing will.
    pub fn drain(&mut self) -> Option<Scheme> {
        None
    }

    pub fn heard(&self) -> bool {
        false
    }

    pub fn link(&self) -> Link {
        Link::Gone
    }

    pub fn started(&self) -> Instant {
        self.started
    }

    /// No wait: there is no first answer to wait for.
    pub fn wait_first(&mut self, _within: Duration) -> Option<Scheme> {
        None
    }
}
