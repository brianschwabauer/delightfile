//! Following the desktop between light and dark: the half of `[flavor] mode =
//! "auto"` that has to ask somebody.
//!
//! ## Who is asked
//!
//! The XDG desktop portal, on the session bus: `org.freedesktop.portal.Settings`
//! at `/org/freedesktop/portal/desktop`, for `org.freedesktop.appearance`
//! `color-scheme`. That one key is the whole freedesktop vocabulary for this —
//! a `u` where `1` is "prefer dark", `2` is "prefer light" and `0` is "no
//! preference" — and GNOME, KDE and the gtk portal Hyprland sessions run all
//! serve it, so there is nothing desktop-specific to know.
//!
//! **Anything but a clear "light" is dark.** No preference, a key the portal
//! does not have, a portal that is not there, a session bus that is not there:
//! all of them leave the window on the dark side, which is what it was before
//! it knew how to be anything else, and none of them is worth a word to
//! anybody but the debug log. A file manager that toasted "could not reach the
//! desktop portal" at every start on a bare compositor would be complaining
//! about a setup that is working as intended.
//!
//! ## Asked once, then heard
//!
//! One thread, in the shape of every other worker here: it connects, subscribes
//! to `SettingChanged` for exactly this key (`AddMatch`, with the portal's name
//! as the sender, so the bus drops a forgery from anybody else before it is
//! sent here), asks `ReadOne` — or `Read`, which is what portals before version
//! 2 of the interface answer — and then blocks on the socket. Each answer goes
//! down a channel and rings the [`crate::Wake`] bell, and the app takes it on
//! its next pass through `poll_workers`. Nothing polls: a desktop that never
//! changes its mind costs one blocked thread and no frames.
//!
//! Subscribed *before* asking, so a change landing between the two is either
//! in the answer or after it — never lost in the gap.
//!
//! ## Letting go
//!
//! Dropping the [`Desktop`] hangs the connection up, which ends the read the
//! thread is blocked in and with it the thread. It is not joined: a bus that
//! never answered the first call would otherwise hold the window's exit for
//! the call's whole timeout.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::config::Appearance;
use df_core::fs::Notifier;

use crate::dbus::{marshal_body, Bus, Message, Outbox, Reader, BUS_DRIVER, MSG_SIGNAL};

/// The portal's well-known name, its object and the interface asked.
pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const SETTINGS: &str = "org.freedesktop.portal.Settings";

/// The setting: which namespace, which key.
pub const NAMESPACE: &str = "org.freedesktop.appearance";
pub const KEY: &str = "color-scheme";

/// What the desktop prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// `0`, or anything the specification does not name, or no answer at all.
    NoPreference,
    Dark,
    Light,
}

impl Scheme {
    /// The number on the wire, as the specification spells it.
    pub fn from_value(value: u64) -> Scheme {
        match value {
            1 => Scheme::Dark,
            2 => Scheme::Light,
            _ => Scheme::NoPreference,
        }
    }

    /// The side a window that follows the desktop is on: light only when light
    /// was asked for (see this module's header).
    pub fn appearance(self) -> Appearance {
        match self {
            Scheme::Light => Appearance::Light,
            Scheme::Dark | Scheme::NoPreference => Appearance::Dark,
        }
    }
}

/// The subscription: `SettingChanged` from the portal, for this key only.
///
/// `arg0` and `arg1` are the namespace and the key, which the bus compares
/// for us — so the thread is not woken for every font or accent-colour change
/// the desktop broadcasts. `sender` is the portal's well-known name, which the
/// bus resolves to its current owner: a signal from any other connection never
/// arrives.
pub fn match_rule() -> String {
    format!(
        "type='signal',sender='{PORTAL}',interface='{SETTINGS}',member='SettingChanged',\
         path='{PORTAL_PATH}',arg0='{NAMESPACE}',arg1='{KEY}'"
    )
}

/// The colour scheme a `SettingChanged` carries, or `None` for any message
/// that is not one about this key.
///
/// Checked here as well as by the match rule, because a connection is sent
/// more than it subscribed to — the bus driver's `NameAcquired`, for a start.
pub fn setting_changed(msg: &Message) -> Option<Scheme> {
    if msg.kind != MSG_SIGNAL
        || msg.interface.as_deref() != Some(SETTINGS)
        || msg.member.as_deref() != Some("SettingChanged")
        || msg.path.as_deref() != Some(PORTAL_PATH)
    {
        return None;
    }
    let args = msg.args().ok()?;
    let [namespace, key, value] = args.as_slice() else {
        return None;
    };
    if namespace.as_str() != Some(NAMESPACE) || key.as_str() != Some(KEY) {
        return None;
    }
    Some(
        value
            .as_u64()
            .map_or(Scheme::NoPreference, Scheme::from_value),
    )
}

/// The scheme in a `ReadOne` or `Read` reply.
///
/// `ReadOne` answers `v` holding the `u`; `Read` answers `v` holding a `v`
/// holding the `u`, a wrapping the interface's second version apologised for.
/// [`crate::dbus::Value::as_u64`] looks through any number of variants, so one
/// reader serves both.
pub fn read_reply(body: &[u8]) -> Result<Scheme, String> {
    let value = Reader::new(body).value("v")?;
    Ok(value
        .as_u64()
        .map_or(Scheme::NoPreference, Scheme::from_value))
}

/// The watcher: a thread talking to the portal, and the answers it has sent.
pub struct Desktop {
    answers: Receiver<Scheme>,
    /// Whether anything has come back yet, so [`Desktop::wait_first`] waits
    /// only for the first answer and never again.
    heard: bool,
    /// The connection's writing half once the thread is only listening, for
    /// [`Drop`] to hang up. See [`Line`].
    line: Arc<Mutex<Line>>,
}

/// What `Drop` and the thread share: whether the watcher has gone, and the
/// handle that ends the thread's read.
///
/// One lock around both, so the two cannot pass each other: either the thread
/// stores its handle before `Drop` looks (and `Drop` hangs it up), or it finds
/// `gone` already set (and stops without blocking). With two flags there is a
/// gap in which the thread checks, `Drop` finds nothing to hang up, and the
/// thread then blocks on a socket nobody will ever close.
#[derive(Default)]
struct Line {
    gone: bool,
    outbox: Option<Outbox>,
}

impl Desktop {
    /// Start asking. `notify` is rung once per answer.
    pub fn watch(notify: Notifier) -> Desktop {
        let (tx, rx) = unbounded::<Scheme>();
        let line = Arc::new(Mutex::new(Line::default()));
        let thread_line = Arc::clone(&line);
        let spawned = std::thread::Builder::new()
            .name("df-appearance".to_string())
            .spawn(move || run(&tx, &notify, &thread_line));
        if let Err(e) = spawned {
            // The same answer as no portal: the dark side.
            log::debug!("the appearance watcher did not start: {e}");
        }
        Desktop {
            answers: rx,
            heard: false,
            line,
        }
    }

    /// Everything that has arrived, as the one answer that matters — the
    /// newest. `None` when nothing has.
    pub fn drain(&mut self) -> Option<Scheme> {
        let newest = self.answers.try_iter().last();
        self.heard |= newest.is_some();
        newest
    }

    /// Wait up to `within` for the first answer, if it has not come yet.
    ///
    /// For the first frame only: the window should open on the side the
    /// desktop is on rather than open dark and turn light a moment later.
    /// Anything that arrived meanwhile is drained as well, newest wins.
    pub fn wait_first(&mut self, within: Duration) -> Option<Scheme> {
        if self.heard {
            return self.drain();
        }
        match self.answers.recv_timeout(within) {
            Ok(first) => {
                self.heard = true;
                Some(self.drain().unwrap_or(first))
            }
            Err(_) => None,
        }
    }

    /// A watcher with no thread and no bus, and the sender that stands in for
    /// the portal: what a test delivers its answers through.
    #[cfg(test)]
    pub fn fake() -> (Desktop, Sender<Scheme>) {
        let (tx, rx) = unbounded::<Scheme>();
        (
            Desktop {
                answers: rx,
                heard: false,
                line: Arc::new(Mutex::new(Line::default())),
            },
            tx,
        )
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let Ok(mut line) = self.line.lock() else {
            return;
        };
        line.gone = true;
        if let Some(outbox) = line.outbox.take() {
            outbox.hang_up();
        }
    }
}

/// The thread: connect, subscribe, ask, then listen until the line goes.
fn run(tx: &Sender<Scheme>, notify: &Notifier, line: &Arc<Mutex<Line>>) {
    // `false` once nobody is listening, which ends the thread.
    let tell = |scheme: Scheme| -> bool {
        if tx.send(scheme).is_err() {
            return false;
        }
        notify();
        true
    };
    let mut bus = match Bus::session() {
        Ok(bus) => bus,
        Err(e) => {
            log::debug!("no session bus for the colour scheme: {e}");
            tell(Scheme::NoPreference);
            return;
        }
    };
    let subscribed = match add_match(&mut bus) {
        Ok(()) => true,
        Err(e) => {
            // Asked once all the same: a window that cannot follow a change
            // can still open on the side the desktop is on now.
            log::debug!("cannot subscribe to the colour scheme: {e}");
            false
        }
    };
    let first = match ask(&mut bus) {
        Ok(scheme) => {
            log::info!("the desktop prefers {}", describe(scheme));
            scheme
        }
        Err(e) => {
            log::debug!("the portal did not say which colour scheme: {e}");
            Scheme::NoPreference
        }
    };
    if !tell(first) || !subscribed {
        return;
    }
    let (mut inbox, outbox) = match bus.into_service() {
        Ok(halves) => halves,
        Err(e) => {
            log::debug!("cannot listen for colour scheme changes: {e}");
            return;
        }
    };
    match line.lock() {
        Ok(mut line) if !line.gone => line.outbox = Some(outbox),
        // Dropped while this was connecting: nobody left to tell.
        _ => return,
    }
    loop {
        let msg = match inbox.next() {
            Ok(msg) => msg,
            // Hung up by `Drop`, or the bus went away: either way, done.
            Err(e) => {
                log::debug!("stopped listening for colour scheme changes: {e}");
                return;
            }
        };
        if let Some(scheme) = setting_changed(&msg) {
            log::info!("the desktop now prefers {}", describe(scheme));
            if !tell(scheme) {
                return;
            }
        }
    }
}

/// `org.freedesktop.DBus.AddMatch` with [`match_rule`].
fn add_match(bus: &mut Bus) -> Result<(), String> {
    let body = marshal_body("s", &[crate::dbus::Value::Str(match_rule())])?;
    bus.call(
        BUS_DRIVER,
        "/org/freedesktop/DBus",
        BUS_DRIVER,
        "AddMatch",
        Some("s"),
        &body,
    )
    .map(|_| ())
}

/// `ReadOne`, and `Read` when the portal is too old to have it.
fn ask(bus: &mut Bus) -> Result<Scheme, String> {
    let body = marshal_body(
        "ss",
        &[
            crate::dbus::Value::Str(NAMESPACE.to_string()),
            crate::dbus::Value::Str(KEY.to_string()),
        ],
    )?;
    let reply = match bus.call(PORTAL, PORTAL_PATH, SETTINGS, "ReadOne", Some("ss"), &body) {
        Ok(reply) => reply,
        Err(e) => {
            log::debug!("ReadOne: {e}; asking with Read");
            bus.call(PORTAL, PORTAL_PATH, SETTINGS, "Read", Some("ss"), &body)?
        }
    };
    read_reply(&reply)
}

/// A `SettingChanged` for the colour scheme, carrying `value`, as the portal
/// sends it — encoded and read back through this program's own wire code, so
/// a test can deliver the desktop's words without a bus.
#[cfg(test)]
pub fn scheme_signal(value: u32) -> Message {
    tests::signal(
        NAMESPACE,
        KEY,
        crate::dbus::Value::variant("u", crate::dbus::Value::U32(value)),
    )
}

fn describe(scheme: Scheme) -> &'static str {
    match scheme {
        Scheme::Dark => "dark",
        Scheme::Light => "light",
        Scheme::NoPreference => "neither (dark it is)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::{parse_message, Value};

    /// A `SettingChanged` as the portal sends it, built with this program's
    /// own encoder and read back through its own parser — the bytes a real
    /// bus would hand the thread, without needing one.
    pub(super) fn signal(namespace: &str, key: &str, value: Value) -> Message {
        let msg = Message {
            kind: MSG_SIGNAL,
            serial: 7,
            path: Some(PORTAL_PATH.to_string()),
            interface: Some(SETTINGS.to_string()),
            member: Some("SettingChanged".to_string()),
            sender: Some(":1.23".to_string()),
            ..Message::default()
        }
        .with_args(
            "ssv",
            &[
                Value::Str(namespace.to_string()),
                Value::Str(key.to_string()),
                value,
            ],
        )
        .expect("marshal the signal");
        parse_message(&msg.encode().expect("encode the signal")).expect("parse it back")
    }

    #[test]
    fn a_setting_changed_signal_says_which_scheme() {
        let light = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(2)));
        assert_eq!(setting_changed(&light), Some(Scheme::Light));
        let dark = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(1)));
        assert_eq!(setting_changed(&dark), Some(Scheme::Dark));
        let none = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(0)));
        assert_eq!(setting_changed(&none), Some(Scheme::NoPreference));
        // A number the specification has no word for is no preference.
        let odd = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(9)));
        assert_eq!(setting_changed(&odd), Some(Scheme::NoPreference));
    }

    /// Another key in the namespace, another namespace, another member, a
    /// method call: none of them is about the colour scheme.
    #[test]
    fn everything_else_on_the_line_is_ignored() {
        let accent = signal(
            NAMESPACE,
            "accent-color",
            Value::variant("(ddd)", Value::Struct(vec![Value::F64(0.1); 3])),
        );
        assert_eq!(setting_changed(&accent), None);
        let gtk = signal(
            "org.gnome.desktop.interface",
            KEY,
            Value::variant("u", Value::U32(2)),
        );
        assert_eq!(setting_changed(&gtk), None);

        let mut renamed = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(2)));
        renamed.member = Some("SettingsChanged".to_string());
        assert_eq!(setting_changed(&renamed), None);
        let mut called = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(2)));
        called.kind = crate::dbus::MSG_METHOD_CALL;
        assert_eq!(setting_changed(&called), None);
        let mut elsewhere = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(2)));
        elsewhere.path = Some("/org/freedesktop/portal/other".to_string());
        assert_eq!(setting_changed(&elsewhere), None);
    }

    /// `ReadOne`'s one variant and `Read`'s two both come to the number.
    #[test]
    fn both_read_replies_are_understood() {
        let one = marshal_body("v", &[Value::variant("u", Value::U32(2))]).expect("marshal");
        assert_eq!(read_reply(&one), Ok(Scheme::Light));
        let two = marshal_body(
            "v",
            &[Value::variant("v", Value::variant("u", Value::U32(1)))],
        )
        .expect("marshal");
        assert_eq!(read_reply(&two), Ok(Scheme::Dark));
        let odd =
            marshal_body("v", &[Value::variant("s", Value::Str("light".into()))]).expect("marshal");
        assert_eq!(read_reply(&odd), Ok(Scheme::NoPreference));
    }

    #[test]
    fn only_a_clear_light_is_light() {
        assert_eq!(Scheme::Light.appearance(), Appearance::Light);
        assert_eq!(Scheme::Dark.appearance(), Appearance::Dark);
        assert_eq!(Scheme::NoPreference.appearance(), Appearance::Dark);
    }

    /// The rule names the key, so the bus filters for us — and the portal, so
    /// it filters out everybody else.
    #[test]
    fn the_match_rule_is_this_key_from_the_portal() {
        let rule = match_rule();
        for part in [
            "type='signal'",
            "sender='org.freedesktop.portal.Desktop'",
            "member='SettingChanged'",
            "arg0='org.freedesktop.appearance'",
            "arg1='color-scheme'",
        ] {
            assert!(rule.contains(part), "{rule} lacks {part}");
        }
    }

    /// The first answer is waited for once; after that the watcher never
    /// blocks, and the newest of several answers wins.
    #[test]
    fn the_first_answer_is_waited_for_and_the_newest_wins() {
        let (mut desktop, portal) = Desktop::fake();
        assert_eq!(desktop.wait_first(Duration::from_millis(1)), None);
        portal.send(Scheme::Dark).expect("send");
        portal.send(Scheme::Light).expect("send");
        assert_eq!(
            desktop.wait_first(Duration::from_secs(1)),
            Some(Scheme::Light)
        );
        assert_eq!(desktop.drain(), None);
        portal.send(Scheme::Dark).expect("send");
        assert_eq!(desktop.drain(), Some(Scheme::Dark));
    }
}
