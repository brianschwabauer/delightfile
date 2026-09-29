//! The Places card's worker on Windows: a stand-in until Phase 4's drives
//! card (`plans/other-platforms/04-windows.md` W4.18, W4.19).
//!
//! It lists nothing and does nothing, and says so: a listing is empty — so
//! the card opens on its Places, its cloud rows and its connect row, with the
//! Devices and Network sections saying there is nothing in them — and every
//! other request, a connection and a phone's mount answer "Not available on
//! this platform", which the card and the toasts already know how to say. No
//! `gio` is ever run, and there is no watcher to hear a phone arrive.

use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use df_core::fs::Notifier;

use crate::mounts::{Answer, Connected, Event, Gio, Reply, Request};

/// What every request that would act on something is answered.
const NOT_HERE: &str = "Not available on this platform";

/// No mount asks questions here, so there is no terminal to ask them in.
pub const TERMINAL_MOUNT: Option<&str> = None;

/// Nothing connects here, so no connect comes back unseen.
pub const CONNECT_UNSEEN: Option<&str> = None;

/// A `gio` that is never there: it answers every run with "unsupported".
/// Nothing on this platform runs it.
pub fn system_gio() -> Gio {
    Arc::new(|_args: &[&str]| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)))
}

/// The worker: an empty listing for [`Request::List`], and a refusal for
/// everything else, each rung once on `notify`.
pub fn run(requests: Receiver<Request>, replies: Sender<Answer>, notify: Notifier, _gio: Gio) {
    for request in requests {
        let reply = match request {
            Request::List => Reply::Listing {
                devices: Vec::new(),
                phones: Vec::new(),
                shares: Vec::new(),
            },
            _ => Reply::Failed(NOT_HERE.to_string()),
        };
        let _ = replies.send(Answer { to: request, reply });
        notify();
    }
}

/// Connecting to a server: not here.
pub fn connect(_url: &str, _gio: &Gio) -> Connected {
    Connected::Failed(NOT_HERE.to_string())
}

/// Mounting a phone or a camera: not here.
pub fn mount_gio(_root: &str, _gio: &Gio) -> Connected {
    Connected::Failed(NOT_HERE.to_string())
}

/// The watcher that hears a phone arrive. There is none here: no value of
/// this type exists, because [`Monitor::start`] never makes one.
pub enum Monitor {}

impl Monitor {
    pub fn start(_notify: Notifier) -> Option<Monitor> {
        None
    }

    pub fn gone(&mut self) -> bool {
        match *self {}
    }

    pub fn started(&self) -> Instant {
        match *self {}
    }

    pub fn drain(&self) -> Vec<Event> {
        match *self {}
    }
}
