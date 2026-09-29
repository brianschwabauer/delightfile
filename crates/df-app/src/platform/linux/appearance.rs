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
//! does not have, a portal that is not there: all of them leave the window on
//! the dark side, which is what it was before it knew how to be anything else,
//! and none of them is worth a word to anybody but the debug log at startup.
//! A file manager that toasted "could not reach the desktop portal" at every
//! start on a bare compositor would be complaining about a setup that is
//! working as intended. (Asked for by name — `theme-auto` — it does say so.)
//!
//! ## Asked once, then heard
//!
//! One thread, in the shape of every other worker here: it connects, subscribes
//! to `SettingChanged` for exactly this key (`AddMatch`, with the portal's name
//! as the sender, so the bus drops a forgery from anybody else before it is
//! sent here) and to the portal's name changing hands, starts the portal by
//! name if it is not running yet, asks `ReadOne` — or `Read`, which is what
//! portals before version 2 of the interface answer — and then blocks on the
//! socket. Each answer goes down a channel and rings the [`crate::Wake`] bell,
//! and the app takes it on its next pass through `poll_workers`. Nothing
//! polls: a desktop that never changes its mind costs one blocked thread and
//! no frames.
//!
//! Subscribed *before* asking, so a change landing between the two is either
//! in the answer or after it — never lost in the gap. And asked *again* when
//! the portal comes back after going away (`NameOwnerChanged`), because a
//! change made while it was down was never broadcast to anybody.
//!
//! ## When the line goes
//!
//! The thread ends when it cannot connect, when the bus refuses the
//! subscription, when the connection drops, or when it is let go — and every
//! one of those ends in the same place, [`Link::Gone`], with the bell rung so
//! the app notices. The app starts a new watcher then: at once when
//! `theme-auto` asks, and otherwise on the next frame something else brings
//! at least [`RETRY`](crate::appearance::RETRY) after the last start (see
//! `App::revive_desktop`). A new watcher asks afresh, so whatever changed
//! while nobody was listening is heard on reconnection.
//!
//! A line that drops says nothing about the desktop, so the window stays on
//! the side it was on. A portal that answers nothing is "no preference", and
//! dark, as at startup.
//!
//! ## Letting go
//!
//! Dropping the [`Desktop`] hangs the connection up, which ends the read the
//! thread is blocked in and with it the thread. It is not joined: a bus that
//! never answered the first call would otherwise hold the window's exit for
//! the call's whole timeout.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;

use crate::appearance::{Link, Scheme};
use crate::platform::linux::dbus::{
    marshal_body, Bus, Hangup, Message, Outbox, Reader, Value, BUS_DRIVER, MSG_ERROR,
    MSG_METHOD_RETURN, MSG_SIGNAL,
};

/// The portal's well-known name, its object and the interface asked.
pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const SETTINGS: &str = "org.freedesktop.portal.Settings";

/// The setting: which namespace, which key.
pub const NAMESPACE: &str = "org.freedesktop.appearance";
pub const KEY: &str = "color-scheme";

impl Scheme {
    /// The number on the wire, as the specification spells it.
    pub fn from_value(value: u64) -> Scheme {
        match value {
            1 => Scheme::Dark,
            2 => Scheme::Light,
            _ => Scheme::NoPreference,
        }
    }
}

/// How a watcher gets its connection: the session bus, or — in a test — one
/// end of a socket pair with a make-believe bus on the other.
pub type Connect = Arc<dyn Fn() -> Result<Bus, String> + Send + Sync>;

/// The session bus, which is where the portal is.
pub fn session() -> Connect {
    Arc::new(Bus::session)
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

/// The other subscription: the portal's name changing hands, from the bus
/// itself, so a portal that restarts is asked again.
pub fn owner_rule() -> String {
    format!(
        "type='signal',sender='{BUS_DRIVER}',interface='{BUS_DRIVER}',\
         member='NameOwnerChanged',arg0='{PORTAL}'"
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

/// The portal's new owner, when `msg` is the bus saying the portal has come
/// (back) — a unique name to hold the answer's sender to. `None` for anything
/// else, the portal going away included.
pub fn portal_returned(msg: &Message) -> Option<String> {
    if msg.kind != MSG_SIGNAL
        || msg.sender.as_deref() != Some(BUS_DRIVER)
        || msg.interface.as_deref() != Some(BUS_DRIVER)
        || msg.member.as_deref() != Some("NameOwnerChanged")
    {
        return None;
    }
    let args = msg.args().ok()?;
    let [name, _, owner] = args.as_slice() else {
        return None;
    };
    let owner = owner.as_str()?;
    (name.as_str() == Some(PORTAL) && !owner.is_empty()).then(|| owner.to_string())
}

/// The scheme in a `ReadOne` or `Read` reply.
///
/// `ReadOne` answers `v` holding the `u`; `Read` answers `v` holding a `v`
/// holding the `u`, a wrapping the interface's second version apologised for.
/// [`super::dbus::Value::as_u64`] looks through any number of variants, so one
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
    /// Where the thread has got to, and the handle that ends it. See [`Line`].
    line: Arc<Mutex<Line>>,
    /// When this watcher was started, for [`RETRY`](crate::appearance::RETRY).
    started: Instant,
}

/// What `Drop` and the thread share: whether the watcher has been let go,
/// where the thread has got to, and the handle that ends its read.
///
/// One lock around all three, so they cannot pass each other: either the
/// thread stores its handle before `Drop` looks (and `Drop` hangs it up), or
/// it finds `dropped` already set (and stops without blocking). With separate
/// flags there is a gap in which the thread checks, `Drop` finds nothing to
/// hang up, and the thread then blocks on a socket nobody will ever close.
struct Line {
    dropped: bool,
    link: Link,
    hangup: Option<Hangup>,
}

impl Desktop {
    /// Start asking whatever `connect` connects to — [`session`], outside a
    /// test. `notify` is rung once per answer, once when the thread is
    /// listening, and once more when it ends.
    pub fn watch_over(connect: Connect, notify: Notifier) -> Desktop {
        let (tx, rx) = unbounded::<Scheme>();
        let line = Arc::new(Mutex::new(Line {
            dropped: false,
            link: Link::Starting,
            hangup: None,
        }));
        let thread_line = Arc::clone(&line);
        let spawned = std::thread::Builder::new()
            .name("df-appearance".to_string())
            .spawn(move || run(&connect, &tx, &notify, &thread_line));
        if let Err(e) = spawned {
            // The same answer as no bus: nothing will be heard.
            log::debug!("the appearance watcher did not start: {e}");
            if let Ok(mut line) = line.lock() {
                line.link = Link::Gone;
            }
        }
        Desktop {
            answers: rx,
            heard: false,
            line,
            started: Instant::now(),
        }
    }

    /// Everything that has arrived, as the one answer that matters — the
    /// newest. `None` when nothing has.
    pub fn drain(&mut self) -> Option<Scheme> {
        let newest = self.answers.try_iter().last();
        self.heard |= newest.is_some();
        newest
    }

    /// Whether the desktop has said anything yet.
    pub fn heard(&self) -> bool {
        self.heard
    }

    /// Where the thread has got to.
    pub fn link(&self) -> Link {
        self.line.lock().map_or(Link::Gone, |line| line.link)
    }

    /// When this watcher was started.
    pub fn started(&self) -> Instant {
        self.started
    }

    /// Wait up to `within` for the first answer, if it has not come yet.
    ///
    /// For the first frame only: the window should open on the side the
    /// desktop is on rather than open dark and turn light a moment later.
    /// Anything that arrived meanwhile is drained as well, newest wins. A
    /// thread that ends without an answer ends the wait with it.
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

    /// A watcher with no thread and no bus, and the stand-in for the portal a
    /// test speaks through.
    #[cfg(test)]
    pub fn fake() -> (Desktop, FakePortal) {
        let (tx, rx) = unbounded::<Scheme>();
        let line = Arc::new(Mutex::new(Line {
            dropped: false,
            link: Link::Listening,
            hangup: None,
        }));
        (
            Desktop {
                answers: rx,
                heard: false,
                line: Arc::clone(&line),
                started: Instant::now(),
            },
            FakePortal { tx, line },
        )
    }
}

/// The far end of a [`Desktop::fake`].
#[cfg(test)]
pub struct FakePortal {
    tx: Sender<Scheme>,
    line: Arc<Mutex<Line>>,
}

#[cfg(test)]
impl FakePortal {
    /// The desktop says `scheme`.
    pub fn say(&self, scheme: Scheme) {
        self.tx.send(scheme).expect("the app is listening");
    }

    /// The line drops.
    pub fn hang_up(&self) {
        if let Ok(mut line) = self.line.lock() {
            line.link = Link::Gone;
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let Ok(mut line) = self.line.lock() else {
            return;
        };
        line.dropped = true;
        if let Some(hangup) = line.hangup.take() {
            hangup.hang_up();
        }
    }
}

/// Marks the thread [`Link::Gone`] however it ends, and rings the bell so the
/// app hears that it did — unless the app let it go, in which case there is
/// nobody to tell.
struct Ending<'a> {
    line: &'a Mutex<Line>,
    notify: &'a Notifier,
}

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        let dropped = match self.line.lock() {
            Ok(mut line) => {
                line.link = Link::Gone;
                line.hangup = None;
                line.dropped
            }
            Err(_) => true,
        };
        if !dropped {
            (self.notify)();
        }
    }
}

/// The thread: connect, subscribe, ask, then listen until the line goes.
fn run(connect: &Connect, tx: &Sender<Scheme>, notify: &Notifier, line: &Arc<Mutex<Line>>) {
    let _ending = Ending { line, notify };
    // `false` once nobody is listening, which ends the thread.
    let tell = |scheme: Scheme| -> bool {
        if tx.send(scheme).is_err() {
            return false;
        }
        notify();
        true
    };
    // A bus that cannot be reached says nothing about the desktop: no answer
    // is sent, the window stays where it is, and the link going says why.
    let mut bus = match connect() {
        Ok(bus) => bus,
        Err(e) => {
            log::debug!("no session bus for the colour scheme: {e}");
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
    let (mut inbox, mut outbox) = match bus.into_service() {
        Ok(halves) => halves,
        Err(e) => {
            log::debug!("cannot listen for colour scheme changes: {e}");
            return;
        }
    };
    let hangup = match outbox.hangup() {
        Ok(hangup) => hangup,
        Err(e) => {
            log::debug!("cannot listen for colour scheme changes: {e}");
            return;
        }
    };
    match line.lock() {
        Ok(mut line) if !line.dropped => {
            line.hangup = Some(hangup);
            line.link = Link::Listening;
        }
        // Let go while this was connecting: nobody left to tell.
        _ => return,
    }
    // Listening is news too: `theme-auto` waits for it to say what it is
    // following.
    notify();
    // A `ReadOne` sent from here, to a portal that has come back: its serial,
    // who must answer it, and whether it was `ReadOne` (so an error falls back
    // to `Read`) or `Read` already.
    let mut asking: Option<(u32, String, bool)> = None;
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
        } else if let Some(owner) = portal_returned(&msg) {
            // A change made while the portal was down was broadcast to
            // nobody: ask the one that is here now.
            asking = send_ask(&mut outbox, "ReadOne").map(|serial| (serial, owner, true));
        } else if let Some((serial, owner, one)) = asking.take() {
            if msg.reply_serial != Some(serial) || msg.sender.as_deref() != Some(owner.as_str()) {
                asking = Some((serial, owner, one));
                continue;
            }
            match msg.kind {
                MSG_METHOD_RETURN => {
                    let scheme = read_reply(&msg.body).unwrap_or(Scheme::NoPreference);
                    log::info!("the desktop, back, prefers {}", describe(scheme));
                    if !tell(scheme) {
                        return;
                    }
                }
                MSG_ERROR if one => {
                    asking = send_ask(&mut outbox, "Read").map(|serial| (serial, owner, false));
                }
                _ => {}
            }
        }
    }
}

/// `member` (`ReadOne` or `Read`) for the key, sent without waiting: the
/// answer comes back through the reader like everything else. `None` when it
/// could not be sent — the line is going, and the reader will say so.
fn send_ask(outbox: &mut Outbox, member: &str) -> Option<u32> {
    let msg = Message::method_call(PORTAL, PORTAL_PATH, SETTINGS, member)
        .with_args(
            "ss",
            &[
                Value::Str(NAMESPACE.to_string()),
                Value::Str(KEY.to_string()),
            ],
        )
        .ok()?;
    outbox.send(msg).ok()
}

/// `org.freedesktop.DBus.AddMatch` with [`match_rule`] and [`owner_rule`].
fn add_match(bus: &mut Bus) -> Result<(), String> {
    for rule in [match_rule(), owner_rule()] {
        let body = marshal_body("s", &[Value::Str(rule)])?;
        bus.call(
            BUS_DRIVER,
            "/org/freedesktop/DBus",
            BUS_DRIVER,
            "AddMatch",
            Some("s"),
            &body,
        )?;
    }
    Ok(())
}

/// Make sure the portal is running before it is called, starting it the way
/// the call itself would have, when it is not.
///
/// Asked first, by name and of the bus itself, because [`Bus::call`] checks a
/// reply's sender against the destination's owner and says out loud when it
/// cannot find one — which, for a session with no portal at all, would be a
/// warning in the log on every start about a setup that is working as
/// intended. Here that session is an error from `StartServiceByName`, and the
/// caller hears it as dark, quietly.
fn start_portal(bus: &mut Bus) -> Result<(), String> {
    let name = marshal_body("s", &[Value::Str(PORTAL.to_string())])?;
    let running = bus.call(
        BUS_DRIVER,
        "/org/freedesktop/DBus",
        BUS_DRIVER,
        "NameHasOwner",
        Some("s"),
        &name,
    )?;
    if Reader::new(&running).value("b")?.as_bool() == Some(true) {
        return Ok(());
    }
    let start = marshal_body("su", &[Value::Str(PORTAL.to_string()), Value::U32(0)])?;
    bus.call(
        BUS_DRIVER,
        "/org/freedesktop/DBus",
        BUS_DRIVER,
        "StartServiceByName",
        Some("su"),
        &start,
    )
    .map(|_| ())
}

/// `ReadOne`, and `Read` when the portal is too old to have it.
fn ask(bus: &mut Bus) -> Result<Scheme, String> {
    start_portal(bus)?;
    let body = marshal_body(
        "ss",
        &[
            Value::Str(NAMESPACE.to_string()),
            Value::Str(KEY.to_string()),
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

fn describe(scheme: Scheme) -> &'static str {
    match scheme {
        Scheme::Dark => "dark",
        Scheme::Light => "light",
        Scheme::NoPreference => "neither (dark it is)",
    }
}

/// A `SettingChanged` for the colour scheme, carrying `value`, as the portal
/// sends it — encoded and read back through this program's own wire code, so
/// a test can deliver the desktop's words without a bus.
#[cfg(test)]
pub fn scheme_signal(value: u32) -> Message {
    tests::signal(NAMESPACE, KEY, Value::variant("u", Value::U32(value)))
}

/// A make-believe session bus on the far end of a socket pair: the pair-socket
/// fixture `super::dbus`'s own tests use, taught just enough of the bus driver
/// and the portal to serve a watcher.
#[cfg(test)]
pub mod fake_bus {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::platform::linux::dbus::{frame_len, parse_message};

    /// The portal's unique name on this bus, and the one it has after
    /// [`Peer::portal_restarts`].
    pub const PORTAL_OWNER: &str = ":1.50";
    pub const PORTAL_OWNER_AGAIN: &str = ":1.51";

    /// What the test tells the bus to do next.
    pub enum Say {
        /// The portal broadcasts a change.
        Changed(u32),
        /// The portal goes away and comes back as [`PORTAL_OWNER_AGAIN`], its
        /// setting now this.
        Restarts(u32),
        /// The bus hangs up.
        Drop,
    }

    /// The test's end of it.
    pub struct Peer {
        orders: Sender<Say>,
        /// Each `ReadOne`/`Read` the bus was asked, as the member it was.
        pub asked: Receiver<String>,
    }

    impl Peer {
        pub fn say(&self, order: Say) {
            let _ = self.orders.send(order);
        }
    }

    fn reply(call: &Message, sender: &str, serial: u32, sig: &str, args: &[Value]) -> Vec<u8> {
        let mut msg = Message::method_return(call).with_args(sig, args).unwrap();
        msg.sender = Some(sender.to_string());
        msg.serial = serial;
        msg.encode().unwrap()
    }

    fn signal(sender: &str, path: &str, interface: &str, member: &str) -> Message {
        Message {
            kind: MSG_SIGNAL,
            serial: 900,
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            sender: Some(sender.to_string()),
            ..Message::default()
        }
    }

    fn read_one(sock: &mut UnixStream) -> Option<Message> {
        let mut head = [0u8; 16];
        sock.read_exact(&mut head).ok()?;
        let total = frame_len(&head).ok()?;
        let mut rest = vec![0u8; total - 16];
        sock.read_exact(&mut rest).ok()?;
        let mut all = head.to_vec();
        all.extend_from_slice(&rest);
        parse_message(&all).ok()
    }

    /// A bus whose portal prefers `scheme`: the watcher's end, and the test's.
    ///
    /// The bus answers `AddMatch`, `NameHasOwner` and `GetNameOwner` as the
    /// driver, and the portal answers `ReadOne` — and after that it does what
    /// the test says, in order, then waits for the watcher to hang up.
    pub fn bus(scheme: u32) -> (Bus, Peer) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (orders_tx, orders) = unbounded::<Say>();
        let (asked_tx, asked) = unbounded::<String>();
        std::thread::spawn(move || {
            let mut sock = theirs;
            let mut scheme = scheme;
            let mut owner = PORTAL_OWNER;
            let mut serial = 1000;
            let mut next = move || {
                serial += 1;
                serial
            };
            // The calls, until the watcher is listening: its first `ReadOne`.
            loop {
                let Some(call) = read_one(&mut sock) else {
                    return;
                };
                let answer = match call.member.as_deref() {
                    Some("AddMatch") => reply(&call, BUS_DRIVER, next(), "", &[]),
                    Some("NameHasOwner") => {
                        reply(&call, BUS_DRIVER, next(), "b", &[Value::Bool(true)])
                    }
                    Some("GetNameOwner") => reply(
                        &call,
                        BUS_DRIVER,
                        next(),
                        "s",
                        &[Value::Str(owner.to_string())],
                    ),
                    Some(member @ ("ReadOne" | "Read")) => {
                        let _ = asked_tx.send(member.to_string());
                        let value = Value::variant("u", Value::U32(scheme));
                        let bytes = reply(&call, owner, next(), "v", &[value]);
                        sock.write_all(&bytes).unwrap();
                        break;
                    }
                    other => panic!("the fake bus was not taught {other:?}"),
                };
                sock.write_all(&answer).unwrap();
            }
            for order in orders {
                match order {
                    Say::Changed(value) => {
                        let msg = signal(owner, PORTAL_PATH, SETTINGS, "SettingChanged")
                            .with_args(
                                "ssv",
                                &[
                                    Value::Str(NAMESPACE.to_string()),
                                    Value::Str(KEY.to_string()),
                                    Value::variant("u", Value::U32(value)),
                                ],
                            )
                            .unwrap();
                        sock.write_all(&msg.encode().unwrap()).unwrap();
                    }
                    Say::Restarts(value) => {
                        scheme = value;
                        let gone = signal(
                            BUS_DRIVER,
                            "/org/freedesktop/DBus",
                            BUS_DRIVER,
                            "NameOwnerChanged",
                        );
                        let names = |old: &str, new: &str| {
                            vec![
                                Value::Str(PORTAL.to_string()),
                                Value::Str(old.to_string()),
                                Value::Str(new.to_string()),
                            ]
                        };
                        let left = gone.clone().with_args("sss", &names(owner, "")).unwrap();
                        sock.write_all(&left.encode().unwrap()).unwrap();
                        owner = PORTAL_OWNER_AGAIN;
                        let back = gone.with_args("sss", &names("", owner)).unwrap();
                        sock.write_all(&back.encode().unwrap()).unwrap();
                        // The watcher asks the new portal.
                        let Some(call) = read_one(&mut sock) else {
                            return;
                        };
                        let member = call.member.clone().unwrap_or_default();
                        let _ = asked_tx.send(member);
                        let value = Value::variant("u", Value::U32(scheme));
                        let bytes = reply(&call, owner, next(), "v", &[value]);
                        sock.write_all(&bytes).unwrap();
                    }
                    Say::Drop => return,
                }
            }
        });
        (
            Bus::on_socket(ours),
            Peer {
                orders: orders_tx,
                asked,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::linux::dbus::{parse_message, Value};
    use fake_bus::Say;

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
        called.kind = crate::platform::linux::dbus::MSG_METHOD_CALL;
        assert_eq!(setting_changed(&called), None);
        let mut elsewhere = signal(NAMESPACE, KEY, Value::variant("u", Value::U32(2)));
        elsewhere.path = Some("/org/freedesktop/portal/other".to_string());
        assert_eq!(setting_changed(&elsewhere), None);
    }

    /// The bus saying the portal is back names its new owner; the portal
    /// leaving, another name, or anybody but the bus saying it, is nothing.
    #[test]
    fn the_portal_coming_back_is_read_off_the_bus() {
        let owner_changed = |sender: &str, name: &str, new: &str| {
            let msg = Message {
                kind: MSG_SIGNAL,
                serial: 3,
                path: Some("/org/freedesktop/DBus".to_string()),
                interface: Some(BUS_DRIVER.to_string()),
                member: Some("NameOwnerChanged".to_string()),
                sender: Some(sender.to_string()),
                ..Message::default()
            }
            .with_args(
                "sss",
                &[
                    Value::Str(name.to_string()),
                    Value::Str(String::new()),
                    Value::Str(new.to_string()),
                ],
            )
            .expect("marshal");
            parse_message(&msg.encode().expect("encode")).expect("parse")
        };
        assert_eq!(
            portal_returned(&owner_changed(BUS_DRIVER, PORTAL, ":1.9")),
            Some(":1.9".to_string())
        );
        assert_eq!(
            portal_returned(&owner_changed(BUS_DRIVER, PORTAL, "")),
            None
        );
        assert_eq!(
            portal_returned(&owner_changed(BUS_DRIVER, "org.example.Other", ":1.9")),
            None
        );
        assert_eq!(
            portal_returned(&owner_changed(":1.66", PORTAL, ":1.9")),
            None
        );
        assert!(owner_rule().contains("arg0='org.freedesktop.portal.Desktop'"));
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
        portal.say(Scheme::Dark);
        portal.say(Scheme::Light);
        assert_eq!(
            desktop.wait_first(Duration::from_secs(1)),
            Some(Scheme::Light)
        );
        assert_eq!(desktop.drain(), None);
        portal.say(Scheme::Dark);
        assert_eq!(desktop.drain(), Some(Scheme::Dark));
    }

    /// Wait, a few seconds at most, for `desktop` to be at `link`.
    fn until(desktop: &Desktop, link: Link) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while desktop.link() != link && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(desktop.link(), link);
    }

    /// The next answer, a few seconds at most.
    fn next_answer(desktop: &mut Desktop) -> Scheme {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(scheme) = desktop.drain() {
                return scheme;
            }
            assert!(Instant::now() < deadline, "the watcher never answered");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Over the pair-socket fixture: the watcher asks, listens, hears a
    /// change, asks again when the portal comes back, and — when the bus
    /// hangs up — ends as `Gone` with the bell rung; a watcher started after
    /// it connects afresh and asks afresh, so what changed while nobody was
    /// listening is heard.
    #[test]
    fn a_watcher_that_loses_the_bus_ends_and_a_new_one_hears_again() {
        let rung = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bell = {
            let rung = Arc::clone(&rung);
            Arc::new(move || {
                rung.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }) as Notifier
        };
        let (bus, peer) = fake_bus::bus(2);
        let slot = Arc::new(Mutex::new(Some(bus)));
        let connect: Connect = Arc::new(move || {
            slot.lock()
                .ok()
                .and_then(|mut bus| bus.take())
                .ok_or_else(|| "one connection per fixture".to_string())
        });

        let mut desktop = Desktop::watch_over(Arc::clone(&connect), Arc::clone(&bell));
        assert_eq!(
            desktop.wait_first(Duration::from_secs(5)),
            Some(Scheme::Light)
        );
        until(&desktop, Link::Listening);
        assert_eq!(peer.asked.recv().ok().as_deref(), Some("ReadOne"));

        peer.say(Say::Changed(1));
        assert_eq!(next_answer(&mut desktop), Scheme::Dark);

        // The portal restarts, and in the meantime the desktop went light: a
        // new `ReadOne`, sent from the listening thread, catches it.
        peer.say(Say::Restarts(2));
        assert_eq!(next_answer(&mut desktop), Scheme::Light);
        assert_eq!(peer.asked.recv().ok().as_deref(), Some("ReadOne"));

        // The bus goes. The thread ends, says so, and rings.
        let before = rung.load(std::sync::atomic::Ordering::SeqCst);
        peer.say(Say::Drop);
        until(&desktop, Link::Gone);
        assert!(rung.load(std::sync::atomic::Ordering::SeqCst) > before);
        assert_eq!(desktop.drain(), None, "a dropped line says nothing");

        // A second watcher over a second fixture, whose desktop went dark.
        let (bus, _peer) = fake_bus::bus(1);
        let slot = Arc::new(Mutex::new(Some(bus)));
        let again: Connect = Arc::new(move || {
            slot.lock()
                .ok()
                .and_then(|mut bus| bus.take())
                .ok_or_else(|| "one connection per fixture".to_string())
        });
        let mut desktop = Desktop::watch_over(again, bell);
        assert_eq!(
            desktop.wait_first(Duration::from_secs(5)),
            Some(Scheme::Dark)
        );
        until(&desktop, Link::Listening);

        // And a watcher that cannot connect at all is `Gone` with no answer,
        // and the first-frame wait ends with it rather than at its timeout.
        let nothing: Connect = Arc::new(|| Err("no bus here".to_string()));
        let mut desktop = Desktop::watch_over(nothing, Arc::new(|| {}));
        let start = Instant::now();
        assert_eq!(desktop.wait_first(Duration::from_secs(5)), None);
        assert!(start.elapsed() < Duration::from_secs(4));
        until(&desktop, Link::Gone);
    }
}
