//! Following the desktop between light and dark: the half of `[flavor] mode =
//! "auto"` that has to ask somebody, in the words every platform answers in.
//!
//! Who is asked is the platform's ([`crate::platform::appearance`]): on Linux
//! the XDG desktop portal's `color-scheme`, read once and then heard, on a
//! thread of its own; on macOS the application's appearance, observed; on
//! Windows, for now, nobody. This module is
//! what the answer means once it is in, which is the same wherever it came
//! from.
//!
//! **Anything but a clear "light" is dark.** No preference, a key the portal
//! does not have, a portal that is not there, a platform that is not asked
//! yet: all of them leave the window on the dark side, which is what it was
//! before it knew how to be anything else.

use std::time::Duration;

use df_core::config::Appearance;

/// The least time between two starts of a watcher that were not asked for.
///
/// Ten seconds. A bus that went away does not come back within a frame, and
/// one that refuses outright refuses again at once — so a restart per frame
/// would be a thread a frame for nothing. Ten seconds of activity between
/// attempts costs one short-lived thread each, and a bus that has come back is
/// found by the first thing the hand does after it. `theme-auto`, which is
/// somebody asking, does not wait for it.
pub const RETRY: Duration = Duration::from_secs(10);

/// What the desktop prefers. How a platform's answer becomes one of these
/// is that platform's (on Linux, `Scheme::from_value` reads the portal's
/// number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// `0`, or anything the specification does not name, or no answer at all.
    NoPreference,
    Dark,
    Light,
}

impl Scheme {
    /// The side a window that follows the desktop is on: light only when light
    /// was asked for (see this module's header).
    pub fn appearance(self) -> Appearance {
        match self {
            Scheme::Light => Appearance::Light,
            Scheme::Dark | Scheme::NoPreference => Appearance::Dark,
        }
    }
}

/// Where a watcher is with the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Connecting, subscribing, asking.
    Starting,
    /// Subscribed and reading: a change on the desktop will be heard.
    Listening,
    /// The thread has ended — it could not connect, the bus refused the
    /// subscription, or the line dropped — and nothing more will be heard.
    Gone,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_clear_light_is_light() {
        assert_eq!(Scheme::Light.appearance(), Appearance::Light);
        assert_eq!(Scheme::Dark.appearance(), Appearance::Dark);
        assert_eq!(Scheme::NoPreference.appearance(), Appearance::Dark);
    }
}
