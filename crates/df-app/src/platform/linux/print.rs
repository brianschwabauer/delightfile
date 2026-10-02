//! Printing a PDF through the desktop's own print dialog.
//!
//! On Linux this is the XDG desktop portal's `org.freedesktop.portal.Print`
//! (version 4 here, served by `xdg-desktop-portal-gtk`): `PreparePrint` puts
//! up the GTK print dialog — printer, copies, pages, duplex, paper — and
//! answers a token; `Print` takes a PDF as a file descriptor and the token
//! and prints without a second dialog.
//!
//! ## One request, start to finish
//!
//! Both calls are portal *requests*. The method returns at once with the path
//! of a `Request` object, and the answer comes later as that object's
//! `Response` signal: a code — `0` done, `1` cancelled, `2` ended some other
//! way — and a dictionary of results. The path can be known before the call is
//! made: `/org/freedesktop/portal/desktop/request/SENDER/TOKEN`, where SENDER
//! is the connection's unique name without its colon and with its dots made
//! underscores, and TOKEN is a `handle_token` this side chose
//! ([`request_path`]). So [`request`] subscribes to that `Response` *before*
//! calling, because a portal may answer before its method has returned, and to
//! the portal's name changing hands; makes sure the portal is running, starting
//! it by name if not, as `appearance` does; learns its unique name, which every
//! reply and the `Response` must come from; and only then calls.
//!
//! ## No deadline
//!
//! A person may leave the dialog open as long as they like, so the wait for
//! the `Response` has no deadline, and is not [`Bus::call`], which gives up
//! after ninety seconds. Four things end it: the `Response`; an error
//! answering the call, from the portal or from the bus itself; the bus hanging
//! up; and the portal's name losing the owner the call was made to, since a
//! portal that has gone will not answer.
//!
//! ## Closing it from this side
//!
//! A fifth ends `prepare`'s wait: its `stop` flag, which the caller sets when
//! the task is cancelled or the PDF could not be made, so that quitting does
//! not wait on a dialog and a file that cannot print does not ask for a
//! printer first. The wait is [`Bus::next_unless`], which looks at the flag
//! every tenth of a second. Set, it sends `Request.Close` on the request's
//! path — on the same connection, since the portal takes a `Close` only from
//! the request's own sender — and the dialog goes. The answer is then `None`
//! once `Close` has been answered, or after [`CLOSE_GRACE`] when nothing comes;
//! unless the `Response` comes first, which wins: a dialog confirmed in the
//! same instant is confirmed, and the caller decides what that is worth.
//!
//! ## One connection for both halves
//!
//! A [`Session`] keeps the connection `prepare` was made on, and `print` makes
//! its call on the same one. The GTK backend files the dialog's answer under
//! the token *and the calling application*, which on one connection is
//! certainly the same application; each request's path is worked out from the
//! connection that makes it, so nothing here assumes the name stays put. The
//! connection is the kind that passes file descriptors
//! ([`Bus::session_passing_fds`]), because `Print` takes the PDF as an open,
//! read-only file and the backend reads it from that descriptor.
//!
//! The GTK backend keeps a token for five minutes and honours it once. A
//! session redeemed later than that puts the dialog up again, and `print`
//! returns that dialog's answer. A `0` from `Print` means the backend took the
//! job, not that paper came out: it does not report a printer's failure back.
//!
//! ## Not yet
//!
//! `parent_window` is empty, so the dialog is not attached to the window that
//! asked for it and `modal` attaches it to nothing. On Wayland a parent is an
//! `xdg_foreign` export of the window's surface (`wayland:HANDLE`), which is a
//! later refinement.

use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::platform::linux::appearance::{owner_rule, PORTAL, PORTAL_PATH};
use crate::platform::linux::dbus::{
    check_object_path, marshal_body, Bus, Message, Reader, Value, BUS_DRIVER, MSG_ERROR,
    MSG_METHOD_RETURN, MSG_SIGNAL,
};

/// Whether this platform can print at all: whether `builtin:print` is
/// offered and `Command::Print` is live.
pub const SUPPORTED: bool = true;

/// The portal's printing interface, and the one its requests answer on.
const PRINT: &str = "org.freedesktop.portal.Print";
const REQUEST: &str = "org.freedesktop.portal.Request";

/// Where a portal puts its request objects: under this, the caller's unique
/// name, then the caller's token.
const REQUESTS: &str = "/org/freedesktop/portal/desktop/request";

/// `Response`'s codes. Anything else, `2` in practice, is "ended some other
/// way".
const DONE: u32 = 0;
const CANCELLED: u32 = 1;

/// How long a dialog closed from this side waits for the portal to say it
/// has closed (or that it was answered just before) before it is let go.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// A print dialog that was confirmed: what the person chose, as the token
/// the portal hands back, to be redeemed by [`Session::print`] once the PDF
/// exists — on the connection it was handed back on.
pub struct Session {
    token: u32,
    /// The dialog's title, which `Print` takes as well.
    title: String,
    bus: Bus,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("token", &self.token)
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

/// Put up the system's print dialog titled `title` and wait for the person
/// to answer it. `Ok(None)` is the dialog cancelled. Blocks for as long as a
/// person takes, or until `stop` is set: then the dialog is closed from this
/// side and the answer is `None` too, unless the person confirmed it in the
/// same instant (see the module header). Never call this on the UI thread.
///
/// A `2` from the portal is `None` as well. The GTK backend answers `2` when
/// the dialog is closed with Escape or its close button rather than Cancel,
/// which is how most people dismiss a dialog, and an error for that would be
/// wrong every time. It is also the answer when the backend could not put the
/// dialog up at all, which is rare enough that the debug log saying so is
/// enough.
pub fn prepare(title: &str, stop: &AtomicBool) -> Result<Option<Session>, String> {
    if stop.load(Ordering::SeqCst) {
        return Ok(None);
    }
    prepare_on(Bus::session_passing_fds()?, title, stop)
}

/// [`prepare`] over a connection already made: the session bus, or a test's
/// make-believe one.
fn prepare_on(mut bus: Bus, title: &str, stop: &AtomicBool) -> Result<Option<Session>, String> {
    let handle_token = next_handle_token();
    let options = Value::Dict(vec![
        option("handle_token", "s", Value::Str(handle_token.clone())),
        option("modal", "b", Value::Bool(true)),
    ]);
    let args = [
        Value::Str(String::new()),
        Value::Str(title.to_string()),
        // No settings and no page setup to start from: the dialog's own
        // defaults, which are the printer's.
        Value::Dict(Vec::new()),
        Value::Dict(Vec::new()),
        options,
    ];
    let Some(answer) = request(
        &mut bus,
        &handle_token,
        "PreparePrint",
        "ssa{sv}a{sv}a{sv}",
        &args,
        &[],
        Some(stop),
    )?
    else {
        // Closed from this side.
        return Ok(None);
    };
    match answer.response {
        DONE => {
            let token = answer
                .token()
                .ok_or_else(|| "the print dialog answered without a token".to_string())?;
            Ok(Some(Session {
                token,
                title: title.to_string(),
                bus,
            }))
        }
        CANCELLED => Ok(None),
        code => {
            log::debug!(
                "the print dialog ended without an answer (response {code}, results {:?})",
                answer.results
            );
            Ok(None)
        }
    }
}

impl Session {
    /// Print `pdf` with the settings the dialog was answered with. Blocks
    /// until the portal has taken the job (not until the paper is out).
    ///
    /// A `2` from the portal is an error here, unlike in [`prepare`]: under a
    /// live token no dialog is shown, so the person is not in the loop and a
    /// `2` is the backend failing rather than somebody dismissing it.
    pub fn print(self, pdf: &Path) -> Result<(), String> {
        let file = std::fs::File::open(pdf)
            .map_err(|e| format!("cannot open {} to print it: {e}", pdf.display()))?;
        self.print_fd(file.as_fd())
    }

    /// [`Session::print`] of a file already open. The descriptor is lent,
    /// not given: the portal gets its own copy of it with the call.
    fn print_fd(mut self, pdf: BorrowedFd<'_>) -> Result<(), String> {
        let handle_token = next_handle_token();
        let options = Value::Dict(vec![
            option("handle_token", "s", Value::Str(handle_token.clone())),
            option("modal", "b", Value::Bool(true)),
            option("token", "u", Value::U32(self.token)),
        ]);
        let args = [
            Value::Str(String::new()),
            Value::Str(self.title.clone()),
            // The first, and only, descriptor sent with the call.
            Value::UnixFd(0),
            options,
        ];
        // No stop: under a live token there is no dialog to close, and the
        // portal answers at once.
        let Some(answer) = request(
            &mut self.bus,
            &handle_token,
            "Print",
            "ssha{sv}",
            &args,
            &[pdf],
            None,
        )?
        else {
            return Err("printing was stopped".to_string());
        };
        match answer.response {
            DONE => Ok(()),
            // Only when the token had expired and the dialog came back.
            CANCELLED => Err("printing was cancelled".to_string()),
            _ => Err("the print dialog closed without printing".to_string()),
        }
    }
}

/// What a request's `Response` said.
#[derive(Debug, Clone, PartialEq)]
struct Answer {
    response: u32,
    results: Vec<(Value, Value)>,
}

impl Answer {
    /// `PreparePrint`'s `token`, which `Print` is to be given. Its `settings`
    /// and `page-setup` are the backend's to remember and are not read.
    fn token(&self) -> Option<u32> {
        self.results
            .iter()
            .find(|(key, _)| key.as_str() == Some("token"))
            .and_then(|(_, value)| value.as_u64())
            .and_then(|token| u32::try_from(token).ok())
    }
}

/// One portal request on `bus`, start to finish: `member` of the print
/// interface with `args` (of signature `sig`) and `fds` beside them, made
/// under `handle_token`, and the `Response` it came to.
///
/// `None` when `stop` was set before the `Response` came and the request was
/// closed from this side ([`Bus::next_unless`], then `Request.Close`); with
/// no `stop`, the wait is [`Bus::next`]'s and has no end but the four the
/// module header names.
#[allow(clippy::too_many_arguments)]
fn request(
    bus: &mut Bus,
    handle_token: &str,
    member: &str,
    sig: &str,
    args: &[Value],
    fds: &[BorrowedFd<'_>],
    stop: Option<&AtomicBool>,
) -> Result<Option<Answer>, String> {
    let sender = bus
        .unique_name()
        .ok_or_else(|| "the session bus did not name this connection".to_string())?;
    let path = request_path(sender, handle_token)?;
    // Both subscriptions before anything that could be answered.
    add_match(bus, &response_rule(&path))?;
    add_match(bus, &owner_rule())?;
    start_portal(bus)?;
    let owner = portal_owner(bus)?;
    let call = Message::method_call(PORTAL, PORTAL_PATH, PRINT, member).with_args(sig, args)?;
    let serial = bus.send_with_fds(call, fds)?;
    let mut paths = vec![path];
    // Once the request has been closed from this side: the `Close` call's
    // serial, and how long its answer is waited for.
    let mut closing: Option<(u32, Instant)> = None;
    loop {
        let next = match (closing, stop) {
            (Some((_, until)), _) => bus.next_unless(|| Instant::now() >= until),
            (None, Some(stop)) => bus.next_unless(|| stop.load(Ordering::SeqCst)),
            (None, None) => bus.next().map(Some),
        };
        let msg = match next {
            Ok(Some(msg)) => msg,
            Ok(None) if closing.is_some() => {
                log::debug!("the portal did not answer Close within {CLOSE_GRACE:?}");
                return Ok(None);
            }
            Ok(None) => {
                // The newest path the request is known by: where the portal
                // said it put it, when it said somewhere else.
                let at = paths.last().map_or("", String::as_str);
                let close = Message::method_call(PORTAL, at, REQUEST, "Close");
                match bus.send(close) {
                    Ok(close_serial) => {
                        closing = Some((close_serial, Instant::now() + CLOSE_GRACE));
                        continue;
                    }
                    Err(e) => {
                        log::debug!("closing the print dialog: {e}");
                        return Ok(None);
                    }
                }
            }
            Err(e) if closing.is_some() => {
                log::debug!("closing the print dialog: {e}");
                return Ok(None);
            }
            Err(e) => return Err(format!("lost the session bus while printing: {e}")),
        };
        if let Some((close_serial, _)) = closing {
            if closed(&msg, close_serial, &owner) {
                return Ok(None);
            }
        }
        match heard(&msg, serial, &owner, &paths) {
            Heard::Answer(answer) => return Ok(Some(answer)),
            // Whatever else ends a request ends one being closed quietly.
            Heard::Refused(_) | Heard::PortalGone if closing.is_some() => return Ok(None),
            Heard::Refused(why) => return Err(why),
            Heard::PortalGone => {
                return Err("the desktop portal went away while printing".to_string())
            }
            Heard::Handle(handle) if !paths.contains(&handle) => {
                // A portal older than predictable request paths (0.9, 2018)
                // says where the request really is. Subscribed without
                // waiting for the bus to confirm, since a wait here would
                // read past the very `Response` it is for.
                log::debug!(
                    "the portal put the print request at {handle}, not {}",
                    paths[0]
                );
                let subscribe = Message::method_call(
                    BUS_DRIVER,
                    "/org/freedesktop/DBus",
                    BUS_DRIVER,
                    "AddMatch",
                )
                .with_args("s", &[Value::Str(response_rule(&handle))])?;
                bus.send(subscribe)?;
                paths.push(handle);
            }
            Heard::Handle(_) | Heard::Nothing => {}
        }
    }
}

/// What one message means to a request waiting for the answer to the call
/// `serial`, made to the portal's `owner`, whose `Response` will come at one
/// of `paths`.
#[derive(Debug, PartialEq)]
enum Heard {
    /// The call's return: where the request object is.
    Handle(String),
    /// An error answering the call, as a sentence.
    Refused(String),
    /// The request's `Response`.
    Answer(Answer),
    /// The portal's name lost the owner the call was made to.
    PortalGone,
    /// Anything else, which is read past.
    Nothing,
}

/// [`Heard`] for `msg`.
///
/// Every answer is held to who sent it, which the bus stamps and nobody can
/// forge: the call's return and the `Response` only from `owner`, the name's
/// changing hands only from the bus. An error answering the call may come
/// from either, because the bus is who says a call could not be delivered.
fn heard(msg: &Message, serial: u32, owner: &str, paths: &[String]) -> Heard {
    let from = msg.sender.as_deref();
    let answers_call = msg.reply_serial == Some(serial);
    match msg.kind {
        MSG_METHOD_RETURN if answers_call && from == Some(owner) => match msg.args().as_deref() {
            Ok([Value::Path(handle)]) => Heard::Handle(handle.clone()),
            // Not a handle: the `Response` is still what is waited for.
            _ => Heard::Nothing,
        },
        MSG_ERROR if answers_call && (from == Some(owner) || from == Some(BUS_DRIVER)) => {
            Heard::Refused(refusal(msg))
        }
        MSG_SIGNAL
            if from == Some(owner)
                && msg.interface.as_deref() == Some(REQUEST)
                && msg.member.as_deref() == Some("Response")
                && msg.path.as_ref().is_some_and(|path| paths.contains(path)) =>
        {
            match msg.args().as_deref() {
                Ok([Value::U32(response), Value::Dict(results)]) => Heard::Answer(Answer {
                    response: *response,
                    results: results.clone(),
                }),
                // The portal's one answer, unreadable: there will not be
                // another.
                _ => Heard::Refused("the desktop portal's answer could not be read".to_string()),
            }
        }
        MSG_SIGNAL
            if from == Some(BUS_DRIVER)
                && msg.interface.as_deref() == Some(BUS_DRIVER)
                && msg.member.as_deref() == Some("NameOwnerChanged") =>
        {
            match msg.args().as_deref() {
                Ok([name, old, _])
                    if name.as_str() == Some(PORTAL) && old.as_str() == Some(owner) =>
                {
                    Heard::PortalGone
                }
                _ => Heard::Nothing,
            }
        }
        _ => Heard::Nothing,
    }
}

/// Whether `msg` answers the `Request.Close` sent as `close_serial`: its
/// return from the portal's `owner`, or an error from the portal or the bus —
/// a request that had already ended, say — which ends the closing just the
/// same.
fn closed(msg: &Message, close_serial: u32, owner: &str) -> bool {
    let from = msg.sender.as_deref();
    if msg.reply_serial != Some(close_serial) {
        return false;
    }
    match msg.kind {
        MSG_METHOD_RETURN => from == Some(owner),
        MSG_ERROR if from == Some(owner) || from == Some(BUS_DRIVER) => {
            log::debug!("the portal answered Close with: {}", refusal(msg));
            true
        }
        _ => false,
    }
}

/// An error answering a print call, as a sentence.
///
/// Read here rather than by [`Message::error_text`], whose words are about
/// udisks2: a name with no owner is the portal missing, not the disks.
fn refusal(msg: &Message) -> String {
    let name = msg.error_name.as_deref().unwrap_or_default();
    let text = Reader::new(&msg.body).string().unwrap_or_default();
    match name.rsplit('.').next().unwrap_or(name) {
        "UnknownMethod" | "UnknownInterface" | "UnknownObject" => {
            "the desktop portal here cannot print (it has no org.freedesktop.portal.Print)"
                .to_string()
        }
        "ServiceUnknown" | "NameHasNoOwner" => "no desktop portal is running".to_string(),
        _ if !text.is_empty() => format!("the desktop portal could not print: {text}"),
        "" => "the desktop portal could not print".to_string(),
        short => format!("the desktop portal could not print ({short})"),
    }
}

/// Where the portal will put the request `handle_token` makes on the
/// connection named `unique_name`: the name without its colon, its dots made
/// underscores, then the token.
pub fn request_path(unique_name: &str, handle_token: &str) -> Result<String, String> {
    let sender = unique_name
        .strip_prefix(':')
        .unwrap_or(unique_name)
        .replace('.', "_");
    let path = format!("{REQUESTS}/{sender}/{handle_token}");
    check_object_path(&path)?;
    Ok(path)
}

/// A `handle_token` this process has not used before. A request's path is
/// its connection's name and this, and two requests at one path would be one
/// request to the portal, so it never repeats; and it is an element of an
/// object path, so it is letters, digits and underscores only.
fn next_handle_token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("delightfile_print_{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The subscription to the request at `path`'s `Response`, from the portal
/// only: the bus resolves the name to its owner and drops anybody else's.
fn response_rule(path: &str) -> String {
    format!("type='signal',sender='{PORTAL}',interface='{REQUEST}',member='Response',path='{path}'")
}

/// One entry of an `a{sv}` of options.
fn option(key: &str, sig: &str, value: Value) -> (Value, Value) {
    (Value::Str(key.to_string()), Value::variant(sig, value))
}

/// `org.freedesktop.DBus.AddMatch` with `rule`.
fn add_match(bus: &mut Bus, rule: &str) -> Result<(), String> {
    let body = marshal_body("s", &[Value::Str(rule.to_string())])?;
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

/// Make sure the portal is running before it is called, starting it by name
/// when it is not: `appearance`'s routine, and here for the same reason — the
/// owner its answers must come from can only be asked of a portal that is
/// there.
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
    .map_err(|e| {
        // The bus's words for this are `ServiceUnknown`, which the general
        // error reader spells as udisks2 missing.
        log::debug!("starting {PORTAL}: {e}");
        "no desktop portal is running, and none could be started".to_string()
    })
}

/// The portal's unique name: who its answers must come from.
fn portal_owner(bus: &mut Bus) -> Result<String, String> {
    let name = marshal_body("s", &[Value::Str(PORTAL.to_string())])?;
    let reply = bus
        .call(
            BUS_DRIVER,
            "/org/freedesktop/DBus",
            BUS_DRIVER,
            "GetNameOwner",
            Some("s"),
            &name,
        )
        .map_err(|e| {
            log::debug!("the owner of {PORTAL}: {e}");
            "no desktop portal is running".to_string()
        })?;
    Reader::new(&reply).string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{mpsc, Arc};

    use super::*;
    use crate::platform::linux::dbus::{parse_message, receive_with_fds};

    /// This connection's name on the make-believe bus, and the portal's.
    const US: &str = ":1.7";
    const OWNER: &str = ":1.50";

    fn s(text: &str) -> Value {
        Value::Str(text.to_string())
    }

    /// `msg` as it comes off the wire, with the sender the bus stamps.
    fn delivered(mut msg: Message, sender: &str) -> Message {
        msg.sender = Some(sender.to_string());
        msg.serial = 900;
        parse_message(&msg.encode().unwrap()).unwrap()
    }

    /// A `Response` at `path`, saying `response` with `results`.
    fn response(path: &str, response: u32, results: Vec<(Value, Value)>) -> Message {
        Message {
            kind: MSG_SIGNAL,
            path: Some(path.to_string()),
            interface: Some(REQUEST.to_string()),
            member: Some("Response".to_string()),
            ..Message::default()
        }
        .with_args("ua{sv}", &[Value::U32(response), Value::Dict(results)])
        .unwrap()
    }

    /// What `PreparePrint` answers when the person presses Print.
    fn chosen(token: u32) -> Vec<(Value, Value)> {
        vec![
            option(
                "settings",
                "a{sv}",
                Value::Dict(vec![option("n-copies", "s", s("2"))]),
            ),
            option(
                "page-setup",
                "a{sv}",
                Value::Dict(vec![option("PPDName", "s", s("A4"))]),
            ),
            option("token", "u", Value::U32(token)),
        ]
    }

    /// The bus saying the portal's name went from `old` to `new`.
    fn owner_changed(sender: &str, name: &str, old: &str, new: &str) -> Message {
        delivered(
            Message {
                kind: MSG_SIGNAL,
                path: Some("/org/freedesktop/DBus".to_string()),
                interface: Some(BUS_DRIVER.to_string()),
                member: Some("NameOwnerChanged".to_string()),
                ..Message::default()
            }
            .with_args("sss", &[s(name), s(old), s(new)])
            .unwrap(),
            sender,
        )
    }

    #[test]
    fn the_request_path_is_the_sender_and_the_token() {
        assert_eq!(
            request_path(":1.42", "delightfile_print_3").unwrap(),
            "/org/freedesktop/portal/desktop/request/1_42/delightfile_print_3"
        );
        assert_eq!(
            request_path(":1.4242", "t").unwrap(),
            "/org/freedesktop/portal/desktop/request/1_4242/t"
        );
        // Not an object path, so not a request this side could wait on.
        assert!(request_path(":1.42", "a-b").is_err());
        assert!(request_path(":1.42", "").is_err());

        let first = next_handle_token();
        let second = next_handle_token();
        assert_ne!(first, second, "a token is never used twice");
        assert!(request_path(":1.42", &first).is_ok(), "{first}");
    }

    #[test]
    fn a_confirmed_dialog_answers_with_its_token() {
        let path = request_path(US, "t").unwrap();
        let paths = [path.clone()];
        let msg = delivered(response(&path, 0, chosen(42)), OWNER);
        let Heard::Answer(answer) = heard(&msg, 5, OWNER, &paths) else {
            panic!("not an answer");
        };
        assert_eq!(answer.response, DONE);
        assert_eq!(answer.token(), Some(42));

        // A cancel carries no token, and a token that is not a `u` is none.
        let msg = delivered(response(&path, 1, Vec::new()), OWNER);
        let Heard::Answer(answer) = heard(&msg, 5, OWNER, &paths) else {
            panic!("not an answer");
        };
        assert_eq!((answer.response, answer.token()), (CANCELLED, None));
        let odd = vec![option("token", "s", s("42"))];
        let msg = delivered(response(&path, 0, odd), OWNER);
        let Heard::Answer(answer) = heard(&msg, 5, OWNER, &paths) else {
            panic!("not an answer");
        };
        assert_eq!(answer.token(), None);
    }

    /// A `Response` is believed only from the portal, at this request's path.
    #[test]
    fn a_response_from_anybody_else_or_anywhere_else_is_read_past() {
        let path = request_path(US, "t").unwrap();
        let paths = [path.clone()];
        let forged = delivered(response(&path, 0, chosen(1)), ":1.66");
        assert_eq!(heard(&forged, 5, OWNER, &paths), Heard::Nothing);
        let elsewhere = request_path(US, "other").unwrap();
        let stray = delivered(response(&elsewhere, 0, chosen(1)), OWNER);
        assert_eq!(heard(&stray, 5, OWNER, &paths), Heard::Nothing);
        let garbled = delivered(
            Message {
                kind: MSG_SIGNAL,
                path: Some(path.clone()),
                interface: Some(REQUEST.to_string()),
                member: Some("Response".to_string()),
                ..Message::default()
            }
            .with_args("s", &[s("0")])
            .unwrap(),
            OWNER,
        );
        assert!(matches!(
            heard(&garbled, 5, OWNER, &paths),
            Heard::Refused(_)
        ));
    }

    /// The call's own return, and errors from the portal or the bus.
    #[test]
    fn the_call_is_answered_with_a_handle_or_refused() {
        let paths = [request_path(US, "t").unwrap()];
        let call = Message {
            serial: 5,
            sender: Some(US.to_string()),
            ..Message::method_call(PORTAL, PORTAL_PATH, PRINT, "PreparePrint")
        };
        let handle = Message::method_return(&call)
            .with_args("o", &[Value::Path(paths[0].clone())])
            .unwrap();
        assert_eq!(
            heard(&delivered(handle.clone(), OWNER), 5, OWNER, &paths),
            Heard::Handle(paths[0].clone())
        );
        // Somebody else's return, or a return to another call, is not it.
        assert_eq!(
            heard(&delivered(handle.clone(), ":1.66"), 5, OWNER, &paths),
            Heard::Nothing
        );
        assert_eq!(
            heard(&delivered(handle, OWNER), 6, OWNER, &paths),
            Heard::Nothing
        );

        let no_print = Message::error(
            &call,
            "org.freedesktop.DBus.Error.UnknownMethod",
            "No such interface “org.freedesktop.portal.Print”",
        );
        let Heard::Refused(why) = heard(&delivered(no_print, OWNER), 5, OWNER, &paths) else {
            panic!("not refused");
        };
        assert!(why.contains("cannot print"), "{why}");
        // The bus speaks for a portal it could not reach, and is believed.
        let unreachable = Message::error(
            &call,
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "The name is not activatable",
        );
        assert_eq!(
            heard(
                &delivered(unreachable.clone(), BUS_DRIVER),
                5,
                OWNER,
                &paths
            ),
            Heard::Refused("no desktop portal is running".to_string())
        );
        assert_eq!(
            heard(&delivered(unreachable, ":1.66"), 5, OWNER, &paths),
            Heard::Nothing
        );
        let locked = Message::error(
            &call,
            "org.freedesktop.portal.Error.NotAllowed",
            "Printing disabled",
        );
        assert_eq!(
            heard(&delivered(locked, OWNER), 5, OWNER, &paths),
            Heard::Refused("the desktop portal could not print: Printing disabled".to_string())
        );
    }

    /// The portal's name losing the owner the call went to ends the wait;
    /// the name arriving, another name, or anybody but the bus saying so,
    /// does not.
    #[test]
    fn the_portal_leaving_ends_the_wait() {
        let paths = [request_path(US, "t").unwrap()];
        let gone = owner_changed(BUS_DRIVER, PORTAL, OWNER, "");
        assert_eq!(heard(&gone, 5, OWNER, &paths), Heard::PortalGone);
        let replaced = owner_changed(BUS_DRIVER, PORTAL, OWNER, ":1.51");
        assert_eq!(heard(&replaced, 5, OWNER, &paths), Heard::PortalGone);
        let arrived = owner_changed(BUS_DRIVER, PORTAL, "", OWNER);
        assert_eq!(heard(&arrived, 5, OWNER, &paths), Heard::Nothing);
        let other = owner_changed(BUS_DRIVER, "org.example.Other", OWNER, "");
        assert_eq!(heard(&other, 5, OWNER, &paths), Heard::Nothing);
        let forged = owner_changed(":1.66", PORTAL, OWNER, "");
        assert_eq!(heard(&forged, 5, OWNER, &paths), Heard::Nothing);
    }

    // ── A make-believe session bus ──────────────────────────────────────────

    /// How the make-believe portal answers a request.
    #[derive(Clone, Copy)]
    enum Dialog {
        /// `Response` with this code, and a token when it is `0`. Sent
        /// *before* the method's return, which a portal may do.
        Answers(u32),
        /// The portal's name loses its owner instead of answering.
        PortalLeaves,
        /// The bus hangs up instead.
        HangsUp,
        /// The dialog stays up, unanswered, until it is closed; `Close` is
        /// answered when this says so, and otherwise never.
        StaysUp { answers_close: bool },
        /// The person confirms in the instant the dialog is being closed:
        /// the stop flag is set, and the `Response` with its token sent,
        /// before the method has even returned.
        ConfirmedAsClosed,
    }

    /// What the make-believe bus saw of one request, or of a `Close`.
    #[derive(Debug, Default)]
    struct Seen {
        member: String,
        /// The object the call was made on.
        at: String,
        args: Vec<Value>,
        /// The match rules in place when the call came.
        rules: Vec<String>,
        /// The PDF, read through the descriptor that came with the call.
        pdf: Option<String>,
    }

    /// The token the make-believe dialog hands back.
    const TOKEN: u32 = 42;

    /// A bus on the far end of a socket pair: the bus driver for `AddMatch`,
    /// `NameHasOwner` and `GetNameOwner`, and the portal — at [`OWNER`] —
    /// for `PreparePrint`, `Print` and `Request.Close`, answering each request
    /// as the next of `script` says. What it saw of each goes down the
    /// channel; the flag is the `stop` to hand `prepare_on`, which the portal
    /// sets itself for [`Dialog::ConfirmedAsClosed`].
    fn make_believe(script: Vec<Dialog>) -> (Bus, mpsc::Receiver<Seen>, Arc<AtomicBool>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (seen_tx, seen) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let pull = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut sock = theirs;
            let mut rules = Vec::new();
            let mut script = script.into_iter();
            let mut answers_close = true;
            let mut serial = 1000;
            let mut stamp = |mut msg: Message, sender: &str| {
                serial += 1;
                msg.serial = serial;
                msg.sender = Some(sender.to_string());
                msg.encode().unwrap()
            };
            while let Some((call, fds)) = receive_with_fds(&mut sock) {
                let reply = Message::method_return(&call);
                let bytes = match call.member.as_deref() {
                    Some("AddMatch") => {
                        rules.push(call.args().unwrap()[0].as_str().unwrap().to_string());
                        stamp(reply, BUS_DRIVER)
                    }
                    Some("NameHasOwner") => stamp(
                        reply.with_args("b", &[Value::Bool(true)]).unwrap(),
                        BUS_DRIVER,
                    ),
                    Some("GetNameOwner") => {
                        stamp(reply.with_args("s", &[s(OWNER)]).unwrap(), BUS_DRIVER)
                    }
                    Some(member @ ("PreparePrint" | "Print")) => {
                        let args = call.args().unwrap();
                        let options = args.last().unwrap().clone();
                        let Value::Dict(options) = options else {
                            panic!("options are a dictionary");
                        };
                        let token = options
                            .iter()
                            .find(|(key, _)| key.as_str() == Some("handle_token"))
                            .and_then(|(_, value)| value.as_str())
                            .unwrap()
                            .to_string();
                        let path = request_path(US, &token).unwrap();
                        let pdf = fds.into_iter().next().map(|fd| {
                            let mut text = String::new();
                            std::fs::File::from(fd).read_to_string(&mut text).unwrap();
                            text
                        });
                        let _ = seen_tx.send(Seen {
                            member: member.to_string(),
                            at: call.path.clone().unwrap_or_default(),
                            args,
                            rules: rules.clone(),
                            pdf,
                        });
                        let handle = reply.with_args("o", &[Value::Path(path.clone())]).unwrap();
                        match script.next().unwrap() {
                            Dialog::Answers(code) => {
                                let results = if code == DONE && member == "PreparePrint" {
                                    chosen(TOKEN)
                                } else {
                                    Vec::new()
                                };
                                let answer = response(&path, code, results);
                                if sock.write_all(&stamp(answer, OWNER)).is_err() {
                                    return;
                                }
                                stamp(handle, OWNER)
                            }
                            Dialog::PortalLeaves => {
                                if sock.write_all(&stamp(handle, OWNER)).is_err() {
                                    return;
                                }
                                let gone = Message {
                                    kind: MSG_SIGNAL,
                                    path: Some("/org/freedesktop/DBus".to_string()),
                                    interface: Some(BUS_DRIVER.to_string()),
                                    member: Some("NameOwnerChanged".to_string()),
                                    ..Message::default()
                                }
                                .with_args("sss", &[s(PORTAL), s(OWNER), s("")])
                                .unwrap();
                                stamp(gone, BUS_DRIVER)
                            }
                            Dialog::HangsUp => return,
                            Dialog::StaysUp {
                                answers_close: answers,
                            } => {
                                answers_close = answers;
                                stamp(handle, OWNER)
                            }
                            Dialog::ConfirmedAsClosed => {
                                pull.store(true, Ordering::SeqCst);
                                let answer = response(&path, DONE, chosen(TOKEN));
                                if sock.write_all(&stamp(answer, OWNER)).is_err() {
                                    return;
                                }
                                stamp(handle, OWNER)
                            }
                        }
                    }
                    Some("Close") => {
                        assert_eq!(call.interface.as_deref(), Some(REQUEST));
                        let _ = seen_tx.send(Seen {
                            member: "Close".to_string(),
                            at: call.path.clone().unwrap_or_default(),
                            rules: rules.clone(),
                            ..Seen::default()
                        });
                        if !answers_close {
                            continue;
                        }
                        stamp(reply, OWNER)
                    }
                    other => panic!("the make-believe bus was not taught {other:?}"),
                };
                // A client that has its answer may hang up before the rest is
                // written: that is the end of the conversation, not a failure.
                if sock.write_all(&bytes).is_err() {
                    return;
                }
            }
        });
        (Bus::negotiated(ours, US), seen, stop)
    }

    /// The handle token a request was made under, out of its options.
    fn handle_token(seen: &Seen) -> String {
        let Some(Value::Dict(options)) = seen.args.last() else {
            panic!("no options");
        };
        options
            .iter()
            .find(|(key, _)| key.as_str() == Some("handle_token"))
            .and_then(|(_, value)| value.as_str())
            .unwrap()
            .to_string()
    }

    /// The whole of it over the make-believe bus: the dialog is put up with
    /// the title, subscribed to before it is called, answers before its
    /// method returns, and hands back a token; the PDF then goes to the
    /// portal as a descriptor, with that token, on the same connection.
    #[test]
    fn a_confirmed_dialog_prints_the_pdf_under_its_token() {
        let tree = df_core::test_support::TempTree::new("print-portal");
        let pdf = tree.join("report.pdf");
        std::fs::write(&pdf, "%PDF-1.7 make-believe").unwrap();

        let (bus, seen, stop) = make_believe(vec![Dialog::Answers(DONE), Dialog::Answers(DONE)]);
        let session = prepare_on(bus, "report.pdf", &stop).unwrap().unwrap();
        assert_eq!(session.token, TOKEN);

        let prepared = seen.recv().unwrap();
        assert_eq!(prepared.member, "PreparePrint");
        let [parent, title, settings, page_setup, Value::Dict(options)] = prepared.args.as_slice()
        else {
            panic!("PreparePrint's five arguments: {:?}", prepared.args);
        };
        assert_eq!(parent, &s(""));
        assert_eq!(title, &s("report.pdf"));
        assert_eq!(settings, &Value::Dict(Vec::new()));
        assert_eq!(page_setup, &Value::Dict(Vec::new()));
        assert!(options.contains(&option("modal", "b", Value::Bool(true))));
        let path = request_path(US, &handle_token(&prepared)).unwrap();
        assert!(
            prepared
                .rules
                .iter()
                .any(|rule| rule.contains(&format!("path='{path}'"))
                    && rule.contains("member='Response'")
                    && rule.contains("sender='org.freedesktop.portal.Desktop'")),
            "subscribed to the Response before calling: {:?}",
            prepared.rules
        );
        assert!(prepared.pdf.is_none(), "no descriptor with the dialog");

        session.print(&pdf).unwrap();
        let printed = seen.recv().unwrap();
        assert_eq!(printed.member, "Print");
        let [parent, title, fd, Value::Dict(options)] = printed.args.as_slice() else {
            panic!("Print's four arguments: {:?}", printed.args);
        };
        assert_eq!(parent, &s(""));
        assert_eq!(title, &s("report.pdf"));
        assert_eq!(fd, &Value::UnixFd(0));
        assert!(options.contains(&option("token", "u", Value::U32(TOKEN))));
        assert!(options.contains(&option("modal", "b", Value::Bool(true))));
        assert_ne!(handle_token(&printed), handle_token(&prepared));
        assert_eq!(printed.pdf.as_deref(), Some("%PDF-1.7 make-believe"));
    }

    /// Cancel is `None`, and so is the dialog ending any other way — `2`,
    /// which is Escape or the close button. Once a dialog has been answered,
    /// though, `Print` coming back with anything but done is an error: a
    /// cancel of the dialog it puts back up for an expired token, or a `2`,
    /// which with nobody in the loop is the backend failing.
    #[test]
    fn a_dialog_that_is_not_confirmed_prints_nothing() {
        let (bus, _seen, stop) = make_believe(vec![Dialog::Answers(CANCELLED)]);
        assert!(prepare_on(bus, "a.pdf", &stop).unwrap().is_none());

        let (bus, _seen, stop) = make_believe(vec![Dialog::Answers(2)]);
        assert!(prepare_on(bus, "a.pdf", &stop).unwrap().is_none());

        let tree = df_core::test_support::TempTree::new("print-portal-again");
        let pdf = tree.join("a.pdf");
        std::fs::write(&pdf, "%PDF").unwrap();
        let (bus, _seen, stop) =
            make_believe(vec![Dialog::Answers(DONE), Dialog::Answers(CANCELLED)]);
        let session = prepare_on(bus, "a.pdf", &stop).unwrap().unwrap();
        let err = session.print(&pdf).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");

        let (bus, _seen, stop) = make_believe(vec![Dialog::Answers(DONE), Dialog::Answers(2)]);
        let session = prepare_on(bus, "a.pdf", &stop).unwrap().unwrap();
        let err = session.print(&pdf).unwrap_err();
        assert!(err.contains("without printing"), "{err}");
    }

    /// The wait ends when the portal goes or the bus does, rather than
    /// lasting for ever.
    #[test]
    fn a_portal_or_a_bus_that_goes_ends_the_wait() {
        let (bus, _seen, stop) = make_believe(vec![Dialog::PortalLeaves]);
        let err = prepare_on(bus, "a.pdf", &stop).unwrap_err();
        assert!(err.contains("went away"), "{err}");

        let (bus, _seen, stop) = make_believe(vec![Dialog::HangsUp]);
        let err = prepare_on(bus, "a.pdf", &stop).unwrap_err();
        assert!(err.contains("lost the session bus"), "{err}");
    }

    /// A PDF that cannot be opened is an error before anything is sent.
    #[test]
    fn a_missing_pdf_is_not_sent() {
        let (bus, seen, stop) = make_believe(vec![Dialog::Answers(DONE)]);
        let session = prepare_on(bus, "gone.pdf", &stop).unwrap().unwrap();
        let _ = seen.recv().unwrap();
        let err = session
            .print(Path::new("/nonexistent/delightfile/gone.pdf"))
            .unwrap_err();
        assert!(err.contains("cannot open"), "{err}");
        assert!(seen.try_recv().is_err(), "no Print was made");
    }

    /// Stopped while the dialog is up, `prepare` closes it — `Request.Close`
    /// on the request's own path, on its own connection — and is `None` as
    /// soon as the portal says it has, without waiting out the grace.
    #[test]
    fn a_stop_while_the_dialog_is_up_closes_it() {
        let (bus, seen, stop) = make_believe(vec![Dialog::StaysUp {
            answers_close: true,
        }]);
        let start = Instant::now();
        let got = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(150));
                stop.store(true, Ordering::SeqCst);
            });
            prepare_on(bus, "a.pdf", &stop)
        });
        assert!(got.unwrap().is_none(), "closed is no answer");
        assert!(
            start.elapsed() < Duration::from_millis(150) + CLOSE_GRACE,
            "the Close's answer ended it, not the grace: {:?}",
            start.elapsed()
        );
        let prepared = seen.recv().unwrap();
        assert_eq!(prepared.member, "PreparePrint");
        let close = seen.recv().unwrap();
        assert_eq!(close.member, "Close");
        assert_eq!(
            close.at,
            request_path(US, &handle_token(&prepared)).unwrap(),
            "the request that was put up is the one closed"
        );
    }

    /// A portal that never answers the `Close` is let go after the grace.
    #[test]
    fn a_close_nobody_answers_is_given_up_on() {
        let (bus, seen, stop) = make_believe(vec![Dialog::StaysUp {
            answers_close: false,
        }]);
        let start = Instant::now();
        let got = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                stop.store(true, Ordering::SeqCst);
            });
            prepare_on(bus, "a.pdf", &stop)
        });
        assert!(got.unwrap().is_none());
        let took = start.elapsed();
        assert!(
            took >= CLOSE_GRACE && took < CLOSE_GRACE + Duration::from_secs(2),
            "the grace and no longer: {took:?}"
        );
        assert_eq!(seen.recv().unwrap().member, "PreparePrint");
        assert_eq!(seen.recv().unwrap().member, "Close");
    }

    /// A dialog confirmed in the instant it is being closed is confirmed:
    /// the `Response` wins over the stop, and the caller has its token.
    #[test]
    fn a_response_in_the_same_instant_as_a_stop_wins() {
        let (bus, _seen, stop) = make_believe(vec![Dialog::ConfirmedAsClosed]);
        let session = prepare_on(bus, "a.pdf", &stop)
            .unwrap()
            .expect("the confirmation still counts");
        assert!(stop.load(Ordering::SeqCst), "the stop was set first");
        assert_eq!(session.token, TOKEN);
    }
}
