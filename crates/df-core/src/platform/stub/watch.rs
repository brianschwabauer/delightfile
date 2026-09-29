//! No directory watcher: stands in on macOS until M2.1 (kqueue) and on Windows
//! until W4.4 (`ReadDirectoryChangesW`).
//!
//! [`Backend::open`] fails, so [`crate::fs::Watcher::start`] logs the reason
//! and hands back its disabled watcher: every pane still loads and refreshes on
//! request, it just does not notice changes made behind its back.

use std::io;

use crossbeam_channel::{Receiver, Sender};

use crate::fs::watch::Control;
use crate::fs::{Notifier, WatchEvent};

/// A backend that cannot exist: [`Backend::open`] never makes one.
pub(crate) enum Backend {}

impl Backend {
    /// Always `Unsupported`.
    pub(crate) fn open(
        _control: Receiver<Control>,
        _events: Sender<WatchEvent>,
        _notify: Notifier,
    ) -> io::Result<(Backend, std::thread::JoinHandle<()>)> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub(crate) fn wake(&self) {
        match *self {}
    }
}
