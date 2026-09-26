//! Just enough D-Bus to ask udisks2 about the disks, and to *be* a file-chooser
//! portal backend on the session bus.
//!
//! Ported from delightviewer's `dbus.rs`, which talks to the desktop portal on
//! the *session* bus; this one talks to udisks2 on the **system** bus. The wire
//! format is the same one implemented twice for the same reason it was
//! implemented once: `zbus` is a large tree with an async runtime in it, bought
//! here for four method calls and one signature.
//!
//! What is actually needed is: connect to a unix socket, do the one-line SASL
//! handshake, marshal a call, and read messages until the reply arrives. That is
//! the client half of this file. It implements the wire format those messages
//! use and nothing else; anything unexpected is an error, not a best guess.
//!
//! ## The service half
//!
//! `delightfile --portal` ([`crate::portal`]) is the other end of a call: it
//! owns a name, reads method calls off the socket and answers them. That needs
//! three things the client never did — marshalling *arbitrary* values (the
//! portal's options arrive as `a{sv}` holding `a(sa(us))` filters and `ay`
//! paths, and the answer goes back as `(ua{sv})`), building replies and errors
//! as well as calls, and a connection that splits into one reader and a writer
//! several threads share ([`Bus::into_service`]). [`Value`] and [`marshal`] are
//! that generic layer; the udisks2 client is one more user of it.
//!
//! ## Why the system bus is easier than the session bus
//!
//! There is no portal dance. udisks2 answers a method call with a method
//! return — no `Request` object, no `Response` signal, no connection that has to
//! outlive the call. So [`Bus::call`] is the whole client: send, read until the
//! serial comes back, hand over the body.
//!
//! What is *harder* is the reply. `ObjectManager.GetManagedObjects` returns
//! `a{oa{sa{sv}}}` — a dictionary of object paths to a dictionary of interface
//! names to a dictionary of property names to variants — and none of it can be
//! read by knowing the shape in advance, because which interfaces an object has
//! is the answer. So this file carries a real (small) unmarshaller: [`Value`]
//! and [`Reader::value`], which read *any* signature udisks2 can produce.
//!
//! ## Everything here blocks
//!
//! A mount can take seconds (fsck, network, a spinning disk waking up) and a
//! polkit prompt can take as long as the user does. So this runs on a worker
//! thread — [`crate::mounts::Mounts`] — and never on the event loop.
//!
//! ## Tested without a bus
//!
//! The wire format is hand-rolled, so it gets the treatment the SFTP module
//! gets: every encoder has a decoder and the tests are round trips. A message
//! this file builds is parsed back by this file, byte for byte, including the
//! alignment rules that are the whole difficulty of the format.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Little-endian, which is what every machine this runs on is.
const LE: u8 = b'l';

pub const MSG_METHOD_CALL: u8 = 1;
pub const MSG_METHOD_RETURN: u8 = 2;
pub const MSG_ERROR: u8 = 3;
/// A broadcast. udisks2 sends them unasked and [`Bus::call`] reads past them
/// to find its reply; the one this program subscribes to is the portal's
/// `SettingChanged`, which [`crate::appearance`] listens for.
pub const MSG_SIGNAL: u8 = 4;

/// The header flag a caller sets when it will not read the answer. A service
/// sends none then: the bus would deliver it to somebody not listening.
pub const FLAG_NO_REPLY_EXPECTED: u8 = 0x1;

/// `RequestName`'s "do not wait in line" flag. A second `--portal` started by
/// hand while the activated one runs should fail at once and say so, not sit
/// in the queue owning nothing.
const NAME_DO_NOT_QUEUE: u32 = 0x4;

/// The largest array the specification allows (64 MiB). A value this side
/// builds that is bigger is refused before the bus refuses it by hanging up.
const MAX_ARRAY: usize = 64 * 1024 * 1024;

/// How many method calls addressed to this connection a [`Bus::call`] keeps
/// for later while it waits for its own reply (see [`Bus::backlog`]). More
/// than a service could be sent in the moment between owning its name and
/// starting to read, and few enough that a peer spraying calls at a client
/// that never serves them cannot grow it without bound.
const MAX_BACKLOG: usize = 64;

// Header field codes.
const F_PATH: u8 = 1;
const F_INTERFACE: u8 = 2;
const F_MEMBER: u8 = 3;
const F_ERROR_NAME: u8 = 4;
const F_REPLY_SERIAL: u8 = 5;
const F_DESTINATION: u8 = 6;
const F_SENDER: u8 = 7;
const F_SIGNATURE: u8 = 8;

/// Where the system bus lives when the environment does not say.
///
/// The freedesktop default, and the path on every distribution this runs on. It
/// is a *fallback*: `DBUS_SYSTEM_BUS_ADDRESS` wins when it is set, because a
/// container or a test harness may have moved it.
pub const SYSTEM_BUS: &str = "/run/dbus/system_bus_socket";

/// How long a call may take before the client gives up.
///
/// Ninety seconds, and it is generous on purpose: mounting can mean waiting for
/// a spinning disk, for an fsck, or for the user to type a password into a
/// polkit prompt. A timeout that fired during the password dialog would be the
/// one failure the user cannot understand.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(90);

/// The largest message this client will accept.
pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;

/// The deepest a value may nest before it is refused.
///
/// The D-Bus specification's own limit, and the reason it has one: the reader
/// below descends a container by recursing, and a variant costs three bytes on
/// the wire per level. Without a ceiling, a peer sends twelve kilobytes and the
/// worker thread's stack is gone — a `SIGSEGV`, not something a `Result` can
/// carry. Real replies from udisks2 nest four or five deep.
pub const MAX_NESTING: u32 = 32;

/// A value on the wire, whatever its type — read off it, or about to go on it.
///
/// D-Bus is statically typed and this client mostly knows what it is asking
/// for — except for `GetManagedObjects`, whose whole answer is "here are the
/// types you did not know about". So there is one enum wide enough for anything
/// udisks2 puts in a property or a portal puts in its options, and the
/// accessors below are how a caller says what it expected.
///
/// Going *out*, the value is always paired with the signature it is sent as
/// ([`marshal`]): an empty `Array` could be `as` or `a{sv}`, and only the
/// signature can say which.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    U8(u8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F64(f64),
    Str(String),
    /// An object path. A string on the wire, and a different thing to a caller.
    Path(String),
    Signature(String),
    Array(Vec<Value>),
    /// `a{..}` — kept as pairs rather than a map, because D-Bus dictionaries
    /// are ordered arrays and a key may in principle repeat.
    Dict(Vec<(Value, Value)>),
    Struct(Vec<Value>),
    /// `v`: a value with its own signature beside it.
    ///
    /// Kept as a wrapper, not unwrapped on the way in, because a service has
    /// to *send* variants too — every value in the portal's `a{sv}` answer is
    /// one — and a variant holding an empty array has no element to infer its
    /// type from. The accessors look through it, so a reader of a property
    /// never has to care.
    Variant(String, Box<Value>),
}

impl Value {
    /// `value` as a variant of type `sig`.
    pub fn variant(sig: &str, value: Value) -> Value {
        Value::Variant(sig.to_string(), Box::new(value))
    }

    /// This value with any variant wrappers taken off: what a caller reading
    /// a property means by "the value".
    pub fn peeled(&self) -> &Value {
        let mut value = self;
        while let Value::Variant(_, inner) = value {
            value = inner;
        }
        value
    }

    /// [`Value::peeled`], by value.
    pub fn into_peeled(self) -> Value {
        let mut value = self;
        while let Value::Variant(_, inner) = value {
            value = *inner;
        }
        value
    }

    pub fn as_str(&self) -> Option<&str> {
        match self.peeled() {
            Value::Str(s) | Value::Path(s) | Value::Signature(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.peeled() {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Any unsigned integer, widened. udisks2 reports sizes as `t` and a few
    /// things as `u`, and no caller here cares which.
    pub fn as_u64(&self) -> Option<u64> {
        match self.peeled() {
            Value::U64(n) => Some(*n),
            Value::U32(n) => Some(*n as u64),
            Value::U16(n) => Some(*n as u64),
            Value::U8(n) => Some(*n as u64),
            Value::I64(n) if *n >= 0 => Some(*n as u64),
            Value::I32(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        }
    }

    /// A `ay`, exactly as sent — NULs and all. A unix path is bytes, and a
    /// caller that has to hand it back to the filesystem wants those bytes
    /// rather than a lossy decoding of them.
    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        let Value::Array(items) = self.peeled() else {
            return None;
        };
        items
            .iter()
            .map(|item| match item {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect()
    }

    /// A `ay` — udisks2's spelling for a device node or a mount point, which
    /// are byte arrays because a unix path is bytes and not text.
    ///
    /// The trailing NUL udisks2 includes is dropped, and the bytes are decoded
    /// lossily: a mount point with a non-UTF-8 name is still worth showing, and
    /// showing it wrong is better than not listing the disk.
    pub fn as_bytestring(&self) -> Option<String> {
        let mut bytes = self.as_bytes()?;
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// An `aay` — a list of byte strings, which is how mount points arrive.
    pub fn as_bytestrings(&self) -> Option<Vec<String>> {
        let Value::Array(items) = self.peeled() else {
            return None;
        };
        items.iter().map(Value::as_bytestring).collect()
    }

    /// A `ay` for `bytes`: how a path goes out.
    #[cfg(test)]
    pub fn bytes(bytes: &[u8]) -> Value {
        Value::Array(bytes.iter().copied().map(Value::U8).collect())
    }
}

/// One message, off the wire or about to go on it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Message {
    pub kind: u8,
    /// [`FLAG_NO_REPLY_EXPECTED`] and friends.
    pub flags: u8,
    /// This message's own serial: what a reply to it names as its
    /// `reply_serial`. Stamped by whoever sends it ([`Outbox::send`]).
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    /// The body's signature. `None` is an empty body.
    pub signature: Option<String>,
    pub body: Vec<u8>,
}

impl Message {
    /// The error's name and its human-readable text, for a toast.
    ///
    /// The name matters as much as the text: polkit refusals arrive as
    /// `org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain`, whose *text* is
    /// often empty, and "not authorized" is the only useful thing anyone can be
    /// told about it. See [`readable_error`].
    pub fn error_text(&self) -> String {
        let name = self.error_name.clone().unwrap_or_default();
        match Reader::new(&self.body).string() {
            Ok(text) if !text.is_empty() => readable_error(&name, &text),
            _ => readable_error(&name, ""),
        }
    }

    /// A call to `member` on `destination`, with no arguments yet.
    pub fn method_call(destination: &str, path: &str, interface: &str, member: &str) -> Message {
        Message {
            kind: MSG_METHOD_CALL,
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            destination: Some(destination.to_string()),
            ..Message::default()
        }
    }

    /// The (so far empty) answer to `call`, addressed to whoever made it.
    ///
    /// Flagged as wanting no reply of its own: nothing answers an answer, and
    /// saying so costs nothing.
    pub fn method_return(call: &Message) -> Message {
        Message {
            kind: MSG_METHOD_RETURN,
            flags: FLAG_NO_REPLY_EXPECTED,
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            ..Message::default()
        }
    }

    /// An error answering `call`: a D-Bus error name and a sentence.
    pub fn error(call: &Message, name: &str, text: &str) -> Message {
        let mut body = Vec::new();
        // A NUL cannot travel in a D-Bus string, and an error about a caller's
        // bad bytes may well quote them; the text is for a log, so it is
        // cleaned rather than refused.
        marshal_string(&mut body, &text.replace('\0', "\u{FFFD}"));
        Message {
            kind: MSG_ERROR,
            flags: FLAG_NO_REPLY_EXPECTED,
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            error_name: Some(name.to_string()),
            signature: Some("s".to_string()),
            body,
            ..Message::default()
        }
    }

    /// This message with `args` as its body, marshalled as `sig`.
    pub fn with_args(mut self, sig: &str, args: &[Value]) -> Result<Message, String> {
        self.body = marshal_body(sig, args)?;
        self.signature = (!sig.is_empty()).then(|| sig.to_string());
        Ok(self)
    }

    /// The body, read back as the values its signature says it holds.
    pub fn args(&self) -> Result<Vec<Value>, String> {
        match &self.signature {
            Some(sig) => Reader::new(&self.body).values(sig),
            None if self.body.is_empty() => Ok(Vec::new()),
            None => Err("a D-Bus body with no signature".into()),
        }
    }

    /// Whether whoever sent this call is waiting for an answer.
    pub fn wants_reply(&self) -> bool {
        self.kind == MSG_METHOD_CALL && self.flags & FLAG_NO_REPLY_EXPECTED == 0
    }

    /// The whole message as bytes: the fixed header and its field array —
    /// which is itself just the value `(yyyyuua(yv))` — then the body on its
    /// own 8-byte boundary.
    ///
    /// Every string that goes into the header is checked on the way (an
    /// object path's grammar, a signature's), because the bus answers a
    /// malformed message by dropping the connection, and for a service that
    /// is every dialog open at the time.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut fields = Vec::new();
        let mut field = |code: u8, sig: &str, value: Value| {
            fields.push(Value::Struct(vec![
                Value::U8(code),
                Value::variant(sig, value),
            ]));
        };
        if let Some(path) = &self.path {
            field(F_PATH, "o", Value::Path(path.clone()));
        }
        if let Some(interface) = &self.interface {
            field(F_INTERFACE, "s", Value::Str(interface.clone()));
        }
        if let Some(member) = &self.member {
            field(F_MEMBER, "s", Value::Str(member.clone()));
        }
        if let Some(name) = &self.error_name {
            field(F_ERROR_NAME, "s", Value::Str(name.clone()));
        }
        if let Some(serial) = self.reply_serial {
            field(F_REPLY_SERIAL, "u", Value::U32(serial));
        }
        if let Some(destination) = &self.destination {
            field(F_DESTINATION, "s", Value::Str(destination.clone()));
        }
        if let Some(sender) = &self.sender {
            field(F_SENDER, "s", Value::Str(sender.clone()));
        }
        if let Some(sig) = &self.signature {
            field(F_SIGNATURE, "g", Value::Signature(sig.clone()));
        }
        let header = Value::Struct(vec![
            Value::U8(LE),
            Value::U8(self.kind),
            Value::U8(self.flags),
            Value::U8(1), // protocol version
            Value::U32(self.body.len() as u32),
            Value::U32(self.serial),
            Value::Array(fields),
        ]);
        let mut out = Vec::with_capacity(128 + self.body.len());
        marshal(&mut out, "(yyyyuua(yv))", &header)?;
        pad_to(&mut out, 8);
        out.extend_from_slice(&self.body);
        if out.len() > MAX_MESSAGE {
            return Err("a D-Bus message too large to send".into());
        }
        Ok(out)
    }
}

/// Turn a D-Bus error into a sentence a person can act on.
///
/// The udisks2 errors a file manager actually meets are three: polkit said no,
/// polkit could have said yes if you had asked properly, and the device is
/// busy. Each has a next step and each is worth spelling out; everything else
/// falls back to whatever the service said, with its Java-style name trimmed off
/// the front.
pub fn readable_error(name: &str, text: &str) -> String {
    let short = name.rsplit('.').next().unwrap_or(name);
    match short {
        "NotAuthorized" => "not allowed — this needs an administrator".to_string(),
        "NotAuthorizedCanObtain" | "NotAuthorizedDismissed" => {
            "not allowed — the authorisation prompt was dismissed".to_string()
        }
        "DeviceBusy" => "the device is busy — something still has a file open on it".to_string(),
        "AlreadyMounted" => "already mounted".to_string(),
        "NotMounted" => "not mounted".to_string(),
        "ServiceUnknown" | "NameHasNoOwner" => "udisks2 is not running on this machine".to_string(),
        _ if !text.is_empty() => text.to_string(),
        _ if !short.is_empty() => short.to_string(),
        _ => "the call failed".to_string(),
    }
}

/// The name the bus itself answers to, and the only sender its own replies may
/// carry.
pub const BUS_DRIVER: &str = "org.freedesktop.DBus";

/// A connection to a bus: the system bus for udisks2, the session bus for the
/// portal.
pub struct Bus {
    sock: UnixStream,
    serial: u32,
    /// Anything read past the end of one message, kept for the next.
    buf: Vec<u8>,
    /// How long one call may wait for its reply, *in total*. A field rather
    /// than the constant so a test can ask for a deadline it can afford to
    /// wait for; nothing but a test ever changes it.
    timeout: Duration,
    /// Which bus this is, for the sentences its errors are.
    name: &'static str,
    /// Method calls that arrived while a [`Bus::call`] was waiting for its
    /// reply, oldest first, for the [`Inbox`] to hand out before anything
    /// else.
    ///
    /// **The call that started a service is one of them.** D-Bus activation
    /// queues the call that asked for the name, and delivers it the moment
    /// the name is owned — which is inside `RequestName`, before its reply.
    /// Read past like a stray signal, it was lost, and the first file dialog
    /// after every login waited out its timeout.
    backlog: std::collections::VecDeque<Message>,
}

impl Bus {
    /// Connect to the system bus, authenticate, and say Hello.
    pub fn system() -> Result<Bus, String> {
        let address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
            .unwrap_or_else(|_| format!("unix:path={SYSTEM_BUS}"));
        Bus::open(&address, "the system bus")
    }

    /// Connect to the session bus, authenticate, and say Hello.
    pub fn session() -> Result<Bus, String> {
        Bus::open(&session_address()?, "the session bus")
    }

    fn open(address: &str, name: &'static str) -> Result<Bus, String> {
        let socket = bus_socket(address)?;
        let sock = socket
            .connect()
            .map_err(|e| format!("connecting to {name} at {socket:?}: {e}"))?;
        let mut bus = Bus::on_socket(sock);
        bus.name = name;
        bus.authenticate()?;
        bus.hello()?;
        Ok(bus)
    }

    /// A client over an already-connected socket — the seam the tests use, and
    /// the only place the serial is seeded.
    ///
    /// The socket timeouts are the *per-read* ones; they are not the call's
    /// deadline, because a peer that sends one byte every eighty seconds resets
    /// them for ever. [`Bus::call`] and [`Bus::read_line`] carry the deadline
    /// (see [`Bus::deadline_read`]).
    fn on_socket(sock: UnixStream) -> Bus {
        sock.set_read_timeout(Some(CALL_TIMEOUT)).ok();
        sock.set_write_timeout(Some(Duration::from_secs(10))).ok();
        Bus {
            sock,
            serial: start_serial(),
            buf: Vec::new(),
            timeout: CALL_TIMEOUT,
            name: "the bus",
            backlog: std::collections::VecDeque::new(),
        }
    }

    /// Own `name`, or say who does.
    ///
    /// Refuses to queue ([`NAME_DO_NOT_QUEUE`]): a service that is second in
    /// line owns nothing, receives nothing, and would sit there looking alive.
    pub fn request_name(&mut self, name: &str) -> Result<(), String> {
        let body = marshal_body(
            "su",
            &[Value::Str(name.to_string()), Value::U32(NAME_DO_NOT_QUEUE)],
        )?;
        let reply = self.call(
            BUS_DRIVER,
            "/org/freedesktop/DBus",
            BUS_DRIVER,
            "RequestName",
            Some("su"),
            &body,
        )?;
        match Reader::new(&reply).u32()? {
            // Primary owner now, or already was.
            1 | 4 => Ok(()),
            3 => Err(format!("{name} is already owned by another process")),
            other => Err(format!("RequestName({name}) answered {other}")),
        }
    }

    /// Hand the connection over to a service: one [`Inbox`] for the single
    /// loop that reads calls, and one [`Outbox`] the request threads share to
    /// answer them.
    ///
    /// Two handles on one socket rather than one behind a lock, because the
    /// reader spends its life blocked in `read` — a lock it held there would
    /// stop every answer from going out until the next call came in.
    pub fn into_service(self) -> Result<(Inbox, Outbox), String> {
        let writer = self
            .sock
            .try_clone()
            .map_err(|e| format!("splitting the connection to {}: {e}", self.name))?;
        // A service is idle until somebody opens a dialog, which may be days:
        // the reader waits without a deadline. (The timeout is a property of
        // the socket, so this is also the writer's read timeout — which it
        // never reads.)
        self.sock.set_read_timeout(None).ok();
        Ok((
            Inbox {
                sock: self.sock,
                buf: self.buf,
                name: self.name,
                backlog: self.backlog,
            },
            Outbox {
                sock: writer,
                serial: self.serial,
                name: self.name,
            },
        ))
    }

    /// SASL EXTERNAL: the kernel already told the bus who we are, so the whole
    /// handshake is "here is my uid in hex" and a `BEGIN`.
    fn authenticate(&mut self) -> Result<(), String> {
        let hex = auth_line(uid());
        self.sock
            .write_all(hex.as_bytes())
            .map_err(|e| format!("bus auth: {e}"))?;
        let line = self.read_line()?;
        if !line.starts_with("OK") {
            return Err(format!("{} refused EXTERNAL auth: {line}", self.name));
        }
        self.sock
            .write_all(b"BEGIN\r\n")
            .map_err(|e| format!("bus auth: {e}"))?;
        Ok(())
    }

    /// One `\r\n`-terminated line of the SASL handshake.
    ///
    /// A byte at a time, because the handshake is text and the binary stream
    /// starts immediately after `OK` — reading ahead would swallow the first
    /// message. **Every one of those reads is under one deadline**: the length
    /// cap alone bounded this at four thousand reads, each of which could take
    /// the socket's own timeout, so a peer trickling one byte a minute held the
    /// disks card open for days.
    fn read_line(&mut self) -> Result<String, String> {
        let deadline = std::time::Instant::now() + self.timeout;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = self
                .deadline_read(&mut byte, deadline)
                .map_err(|e| format!("bus auth: {e}"))?;
            if n == 0 {
                return Err(format!("{} closed the connection during auth", self.name));
            }
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                line.truncate(line.len() - 2);
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            if line.len() > 4096 {
                return Err("bus auth: reply too long".into());
            }
        }
    }

    /// Read once, waiting no longer than `deadline`.
    ///
    /// The socket's own timeout is *per read*; this is the one that is about
    /// the whole wait. Both are needed: the socket timeout is what makes a
    /// silent peer return at all, and this is what stops a chatty one from
    /// resetting the clock for ever.
    fn deadline_read(
        &mut self,
        into: &mut [u8],
        deadline: std::time::Instant,
    ) -> Result<usize, String> {
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(self.gave_up());
            }
            // Never longer than what is left, so the read itself cannot outlive
            // the deadline it is being held to.
            self.sock.set_read_timeout(Some(left)).ok();
            return match self.sock.read(into) {
                Ok(n) => Ok(n),
                // The socket's own timeout firing *is* the deadline here, since
                // it was set to what was left of it — but a shorter one can
                // fire first (a signal, a coarse clock), so the loop asks the
                // clock rather than assuming.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(e) => Err(e.to_string()),
            };
        }
    }

    /// The sentence a wait that ran out says. One place, so the auth handshake
    /// and the call loop cannot describe the same deadline two ways.
    fn gave_up(&self) -> String {
        format!("{} did not answer within {:?}", self.name, self.timeout)
    }

    /// `org.freedesktop.DBus.Hello` — mandatory first call.
    fn hello(&mut self) -> Result<(), String> {
        self.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            None,
            &[],
        )
        .map(|_| ())
    }

    /// Make one method call and block until its reply comes back.
    ///
    /// Messages that are not this call's reply — signals udisks2 broadcasts
    /// about devices appearing, which this client does not subscribe to but may
    /// still be sent — are read past rather than queued: there is no second
    /// consumer to hand them to. Method calls *to* this connection are the
    /// exception: a service may yet serve them, so they go to
    /// [`Bus::backlog`].
    ///
    /// ## Two things stop a reply being taken from the wrong message
    ///
    /// This socket is not a private channel: on the system bus every local
    /// process can send this connection a message, and this client reads
    /// whatever arrives. So a reply is believed only when both of these hold.
    ///
    /// - **Its `reply_serial` is this call's serial.** Serials used to start at
    ///   0 and step by one, so the serial of the *next* call was the previous
    ///   one plus one from the outside — guessable, which is to say forgeable.
    ///   [`start_serial`] begins somewhere unpredictable instead.
    /// - **Its sender is who was called.** The bus stamps `sender` on every
    ///   message it routes and a peer cannot forge that field, so comparing it
    ///   against the destination's owner is what actually rejects a race. The
    ///   owner is resolved for this call rather than cached, because a service
    ///   that restarts gets a new unique name and a stale expectation would
    ///   drop the *real* reply.
    ///
    /// A message failing either check is discarded and the wait continues —
    /// under one deadline (`CALL_TIMEOUT`) rather than one per read, so a peer
    /// spraying signals cannot keep this loop alive for ever.
    pub fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        signature: Option<&str>,
        body: &[u8],
    ) -> Result<Vec<u8>, String> {
        let expected = self.expected_sender(destination);
        let serial = self.send_call(destination, path, interface, member, signature, body)?;
        let deadline = std::time::Instant::now() + self.timeout;
        loop {
            let msg = self.read_message(deadline)?;
            if msg.reply_serial != Some(serial) {
                self.keep(msg);
                continue;
            }
            if let Some(expected) = &expected {
                if msg.sender.as_deref() != Some(expected.as_str()) {
                    // Not an error: the real reply may still be behind it, and
                    // saying so out loud is how a forged one is noticed.
                    log::warn!(
                        "ignoring a reply to serial {serial} from {:?}, which is not {expected}",
                        msg.sender
                    );
                    continue;
                }
            }
            return match msg.kind {
                MSG_METHOD_RETURN => Ok(msg.body),
                MSG_ERROR => Err(msg.error_text()),
                _ => Err("the bus answered with something that is not a reply".into()),
            };
        }
    }

    /// Who a reply from `destination` must come from, when that can be known.
    ///
    /// The bus driver answers as itself. A unique name (`:1.42`) answers as
    /// itself. A well-known name is *owned* by a unique name, which only the
    /// bus can say — one `GetNameOwner`, whose own reply is checked against the
    /// driver by this same rule (one level of recursion, never two).
    ///
    /// `None` means "cannot be established" — a bus that will not answer
    /// `GetNameOwner`, or a service with no owner yet. The serial check still
    /// applies; this is defence in depth, not the only lock on the door.
    fn expected_sender(&mut self, destination: &str) -> Option<String> {
        if destination == BUS_DRIVER || destination.starts_with(':') {
            return Some(destination.to_string());
        }
        let mut body = Vec::new();
        marshal_string(&mut body, destination);
        let reply = self
            .call(
                BUS_DRIVER,
                "/org/freedesktop/DBus",
                BUS_DRIVER,
                "GetNameOwner",
                Some("s"),
                &body,
            )
            .map_err(|e| log::warn!("cannot resolve the owner of {destination}: {e}"))
            .ok()?;
        Reader::new(&reply).string().ok()
    }

    #[allow(clippy::too_many_arguments)]
    fn send_call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        signature: Option<&str>,
        body: &[u8],
    ) -> Result<u32, String> {
        self.serial = next_serial(self.serial);
        let msg = Message {
            serial: self.serial,
            signature: signature.map(str::to_string),
            body: body.to_vec(),
            ..Message::method_call(destination, path, interface, member)
        };
        self.sock
            .write_all(&msg.encode()?)
            .map_err(|e| format!("writing to {}: {e}", self.name))?;
        Ok(self.serial)
    }

    /// Hold on to a method call that arrived mid-call; drop anything else.
    fn keep(&mut self, msg: Message) {
        if msg.kind != MSG_METHOD_CALL {
            return;
        }
        if self.backlog.len() < MAX_BACKLOG {
            self.backlog.push_back(msg);
        } else {
            log::warn!(
                "dropping a call to {:?} that arrived while waiting for a reply",
                msg.member
            );
        }
    }

    /// Read exactly one message off the wire, no later than `deadline`.
    pub fn read_message(&mut self, deadline: std::time::Instant) -> Result<Message, String> {
        self.fill(16, deadline)?;
        let total = frame_len(&self.buf)?;
        self.fill(total, deadline)?;
        let msg = parse_message(&self.buf[..total])?;
        self.buf.drain(..total);
        Ok(msg)
    }

    fn fill(&mut self, want: usize, deadline: std::time::Instant) -> Result<(), String> {
        let mut chunk = [0u8; 8192];
        while self.buf.len() < want {
            let n = self
                .deadline_read(&mut chunk, deadline)
                .map_err(|e| format!("reading from {}: {e}", self.name))?;
            if n == 0 {
                return Err(format!("{} closed the connection", self.name));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        Ok(())
    }
}

/// The reading half of a service's connection ([`Bus::into_service`]).
pub struct Inbox {
    sock: UnixStream,
    buf: Vec<u8>,
    name: &'static str,
    /// Calls that came in before the service was reading ([`Bus::backlog`]).
    backlog: std::collections::VecDeque<Message>,
}

impl Inbox {
    /// The next message, however long it takes to come: first anything that
    /// arrived before the service was reading, then the socket.
    ///
    /// A message that frames but does not parse is skipped with a warning —
    /// one peer's odd bytes are not a reason to stop serving everyone else.
    /// An error is the connection itself: gone, or a stream this cannot frame
    /// (a big-endian peer), and either way the service is over.
    pub fn next(&mut self) -> Result<Message, String> {
        if let Some(msg) = self.backlog.pop_front() {
            return Ok(msg);
        }
        loop {
            self.fill(16)?;
            let total = frame_len(&self.buf)?;
            self.fill(total)?;
            let parsed = parse_message(&self.buf[..total]);
            self.buf.drain(..total);
            match parsed {
                Ok(msg) => return Ok(msg),
                Err(e) => log::warn!("skipping a message from {}: {e}", self.name),
            }
        }
    }

    fn fill(&mut self, want: usize) -> Result<(), String> {
        let mut chunk = [0u8; 8192];
        while self.buf.len() < want {
            let n = match self.sock.read(&mut chunk) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("reading from {}: {e}", self.name)),
            };
            if n == 0 {
                return Err(format!("{} closed the connection", self.name));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        Ok(())
    }
}

/// The writing half of a service's connection, shared behind a lock by every
/// thread that answers a call. One `send` is one whole message, so two answers
/// finishing at once cannot interleave their bytes.
pub struct Outbox {
    sock: UnixStream,
    serial: u32,
    name: &'static str,
}

impl Outbox {
    /// Stamp `msg` with the next serial and send it.
    pub fn send(&mut self, mut msg: Message) -> Result<u32, String> {
        self.serial = next_serial(self.serial);
        msg.serial = self.serial;
        self.sock
            .write_all(&msg.encode()?)
            .map_err(|e| format!("writing to {}: {e}", self.name))?;
        Ok(self.serial)
    }

    /// Close the connection under both halves.
    ///
    /// A shutdown is of the socket, not of this handle, so the [`Inbox`]
    /// blocked in `read` on the other half wakes to the end of the stream and
    /// its reader stops — the one way to end a thread that is waiting, with no
    /// deadline, for a message that may never come.
    pub fn hang_up(&self) {
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

/// The serial after `serial`: wrapping, and never 0 — the specification
/// reserves it, and a connection that lived long enough to wrap must not send
/// one.
fn next_serial(serial: u32) -> u32 {
    serial.wrapping_add(1).max(1)
}

/// Where a connection's serials start.
///
/// Not 1. A serial is the token a reply is matched by, and a client that always
/// began at 1 handed every local process on the system bus the serial of the
/// call it was about to make. This is not a secret — anything watching the
/// socket sees the number — but it stops a *blind* forgery, which is the only
/// kind that costs nothing to attempt.
///
/// A quarter of the space, so a long-lived connection has a billion calls of
/// headroom before it wraps (and wrapping is handled anyway). Time and pid,
/// mixed: no `rand` dependency for eight bytes of unpredictability (PLAN §1).
fn start_serial() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1);
    let mixed = nanos
        .wrapping_mul(6_364_136_223_846_793_005)
        .rotate_left(17)
        ^ (std::process::id() as u64).wrapping_mul(2_654_435_761);
    // 1..=0x3FFF_FFFF: never 0, never near the wrap.
    (mixed as u32 % 0x3FFF_FFFF) + 1
}

/// The SASL line, split out so the handshake is a test rather than a socket.
pub fn auth_line(uid: u32) -> String {
    let hex: String = uid
        .to_string()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect();
    // The leading NUL is part of the protocol, not part of the line.
    format!("\0AUTH EXTERNAL {hex}\r\n")
}

/// The process uid, without `libc` and without `unsafe`: `/proc/self` is owned
/// by the process that is asking.
fn uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(1000)
}

/// The socket a bus address names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusSocket {
    /// `unix:path=` — a socket file.
    Path(std::path::PathBuf),
    /// `unix:abstract=` — a name in Linux's abstract socket namespace, which
    /// is what a `dbus-daemon` started with `unix:tmpdir=` listens on.
    Abstract(Vec<u8>),
}

impl BusSocket {
    fn connect(&self) -> std::io::Result<UnixStream> {
        match self {
            BusSocket::Path(path) => UnixStream::connect(path),
            BusSocket::Abstract(name) => {
                // Safe std since 1.70: no raw `sockaddr_un` needed.
                use std::os::linux::net::SocketAddrExt;
                let addr = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
                UnixStream::connect_addr(&addr)
            }
        }
    }
}

/// The socket out of a bus address: the first `unix:` entry with a `path=` or
/// an `abstract=`. Other transports (`tcp:`, `unixexec:`) are skipped rather
/// than attempted — nothing a desktop session starts uses them.
pub fn bus_socket(address: &str) -> Result<BusSocket, String> {
    for part in address.split(';') {
        let Some(rest) = part.strip_prefix("unix:") else {
            continue;
        };
        for field in rest.split(',') {
            if let Some(path) = field.strip_prefix("path=") {
                let bytes = unescape_address(path)?;
                return Ok(BusSocket::Path(std::path::PathBuf::from(
                    std::ffi::OsString::from_vec(bytes),
                )));
            }
            if let Some(name) = field.strip_prefix("abstract=") {
                return Ok(BusSocket::Abstract(unescape_address(name)?));
            }
        }
    }
    Err(format!("cannot use bus address `{address}`"))
}

/// A bus address value with its `%xx` escapes undone. The specification lets
/// any byte be written that way, and a path with a space or a comma in it has
/// to be.
fn unescape_address(value: &str) -> Result<Vec<u8>, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| format!("bad escape in bus address value `{value}`"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// The session bus's address: the variable every session sets, then the one
/// the bus sets for a service it activated, then the socket systemd puts in
/// the runtime directory.
fn session_address() -> Result<String, String> {
    for var in ["DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS"] {
        match std::env::var(var) {
            Ok(address) if !address.is_empty() => return Ok(address),
            _ => {}
        }
    }
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => Ok(format!("unix:path={dir}/bus")),
        _ => Err("no session bus: DBUS_SESSION_BUS_ADDRESS and XDG_RUNTIME_DIR are unset".into()),
    }
}

// ── Framing ─────────────────────────────────────────────────────────────────

/// How many bytes the message starting at `head` occupies, from its first 16.
///
/// A big-endian message is *framed* even though it is never parsed
/// ([`parse_message`] refuses it): the bus forwards a message in its sender's
/// byte order, and a service that could not step over one would have to stop
/// reading altogether — one odd peer ending the service for everyone.
pub fn frame_len(head: &[u8]) -> Result<usize, String> {
    let big = match head.first() {
        Some(&LE) => false,
        Some(b'B') => true,
        _ => return Err("not a D-Bus message".into()),
    };
    let word = |at| u32_at(head, at).map(|n| if big { n.swap_bytes() } else { n });
    let body_len = word(4)? as usize;
    let fields_len = word(12)? as usize;
    let total = (16 + fields_len).next_multiple_of(8) + body_len;
    if total > MAX_MESSAGE {
        return Err("absurd message length from the bus".into());
    }
    Ok(total)
}

/// One complete message, understood. Split from the framing so a hand-built
/// message can be checked without a socket.
pub fn parse_message(bytes: &[u8]) -> Result<Message, String> {
    if bytes.first() != Some(&LE) {
        return Err("big-endian message from the bus (unsupported)".into());
    }
    let total = frame_len(bytes)?;
    if bytes.len() < total {
        return Err("truncated D-Bus message".into());
    }
    let fields_end = 16 + u32_at(bytes, 12)? as usize;
    if fields_end > bytes.len() {
        return Err("truncated D-Bus header".into());
    }
    let mut msg = Message {
        kind: bytes[1],
        flags: bytes[2],
        serial: u32_at(bytes, 8)?,
        body: bytes[fields_end.next_multiple_of(8)..total].to_vec(),
        ..Message::default()
    };
    parse_header_fields(&bytes[16..fields_end], &mut msg)?;
    Ok(msg)
}

/// The header's `a(yv)`: a field code and a variant, each struct 8-aligned.
fn parse_header_fields(bytes: &[u8], msg: &mut Message) -> Result<(), String> {
    let mut r = Reader::new(bytes);
    while r.pos < bytes.len() {
        r.align(8);
        if r.pos >= bytes.len() {
            break;
        }
        let code = r.u8()?;
        let sig = r.signature()?;
        match (code, sig.as_str()) {
            (F_PATH, "o") => msg.path = Some(r.string()?),
            (F_INTERFACE, "s") => msg.interface = Some(r.string()?),
            (F_MEMBER, "s") => msg.member = Some(r.string()?),
            (F_SENDER, "s") => msg.sender = Some(r.string()?),
            (F_ERROR_NAME, "s") => msg.error_name = Some(r.string()?),
            (F_REPLY_SERIAL, "u") => msg.reply_serial = Some(r.u32()?),
            (F_DESTINATION, "s") => msg.destination = Some(r.string()?),
            (F_SIGNATURE, "g") => msg.signature = Some(r.signature()?).filter(|s| !s.is_empty()),
            // Everything else is read past rather than understood — a field
            // this code does not use must not fail a message it could parse.
            (_, sig) => r.skip(sig)?,
        }
    }
    Ok(())
}

// ── Marshalling ─────────────────────────────────────────────────────────────

/// The fixed 16-byte header, the field array, and the body on its own 8-byte
/// boundary — built by hand, byte by byte.
///
/// Only the tests use it now ([`Message::encode`] is the real encoder): it is
/// a second, independent spelling of the framing, so the parser is checked
/// against something other than the encoder it would share a mistake with.
#[cfg(test)]
pub fn encode_message(kind: u8, flags: u8, serial: u32, fields: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + body.len());
    out.push(LE);
    out.push(kind);
    out.push(flags);
    out.push(1); // protocol version
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    out.extend_from_slice(fields);
    out.resize(out.len().next_multiple_of(8), 0);
    out.extend_from_slice(body);
    out
}

fn pad_to(out: &mut Vec<u8>, align: usize) {
    while !out.len().is_multiple_of(align) {
        out.push(0);
    }
}

pub fn marshal_string(out: &mut Vec<u8>, s: &str) {
    pad_to(out, 4);
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

fn marshal_signature(out: &mut Vec<u8>, s: &str) {
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Any array: the length word, the first element on its own boundary, the
/// contents, and the length back-patched once they are there.
///
/// The length counts the *contents* and nothing else — not itself, and not the
/// padding between it and the first element — which is why it can only be
/// written afterwards.
pub fn marshal_array(out: &mut Vec<u8>, elem_align: usize, contents: impl FnOnce(&mut Vec<u8>)) {
    pad_to(out, 4);
    let len_at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    pad_to(out, elem_align);
    let start = out.len();
    contents(out);
    let len = (out.len() - start) as u32;
    out[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
}

/// The empty `a{sv}` every udisks2 method takes as its options argument.
///
/// A function rather than a constant, because "an empty array" is still four
/// bytes of length word that have to land at the right alignment.
pub fn marshal_no_options(out: &mut Vec<u8>) {
    marshal_array(out, 8, |_| {});
}

/// Put `values` on the wire as a body of signature `sig`, one complete type
/// per value.
///
/// A body starts on an 8-byte boundary of the message, so marshalling it into
/// a fresh buffer — whose offset 0 *is* that boundary — puts every alignment
/// where the receiver expects it.
pub fn marshal_body(sig: &str, values: &[Value]) -> Result<Vec<u8>, String> {
    check_signature(sig)?;
    let types = split_types(sig);
    if types.len() != values.len() {
        return Err(format!(
            "signature `{sig}` has {} types for {} values",
            types.len(),
            values.len()
        ));
    }
    let mut out = Vec::new();
    for (one, value) in types.iter().zip(values) {
        marshal(&mut out, one, value)?;
    }
    Ok(out)
}

/// Put one value of the single complete type `sig` on the wire, aligned from
/// wherever `out` has got to.
///
/// Driven by the signature, not the value: the value only has to *fit* it. A
/// mismatch — a `Str` where `sig` says `u`, a struct with a field too few — is
/// an error before any byte of it could reach the bus, because the bus answers
/// a malformed message by dropping the connection.
pub fn marshal(out: &mut Vec<u8>, sig: &str, value: &Value) -> Result<(), String> {
    marshal_at(out, sig, value, 0)
}

fn marshal_at(out: &mut Vec<u8>, sig: &str, value: &Value, depth: u32) -> Result<(), String> {
    if depth > MAX_NESTING {
        return Err("D-Bus value is nested too deeply to send".into());
    }
    let depth = depth + 1;
    let head = sig.as_bytes().first().copied();
    match (head, value) {
        (Some(b'y'), Value::U8(n)) => out.push(*n),
        (Some(b'b'), Value::Bool(b)) => put_aligned(out, &u32::from(*b).to_le_bytes()),
        (Some(b'n'), Value::I16(n)) => put_aligned(out, &n.to_le_bytes()),
        (Some(b'q'), Value::U16(n)) => put_aligned(out, &n.to_le_bytes()),
        (Some(b'i'), Value::I32(n)) => put_aligned(out, &n.to_le_bytes()),
        (Some(b'u' | b'h'), Value::U32(n)) => put_aligned(out, &n.to_le_bytes()),
        (Some(b'x'), Value::I64(n)) => put_aligned(out, &n.to_le_bytes()),
        (Some(b't'), Value::U64(n)) => put_aligned(out, &n.to_le_bytes()),
        (Some(b'd'), Value::F64(f)) => put_aligned(out, &f.to_bits().to_le_bytes()),
        (Some(b's'), Value::Str(s)) => {
            if s.contains('\0') {
                return Err("a D-Bus string cannot hold a NUL".into());
            }
            marshal_string(out, s);
        }
        (Some(b'o'), Value::Path(path)) => {
            check_object_path(path)?;
            marshal_string(out, path);
        }
        (Some(b'g'), Value::Signature(s)) => {
            check_signature(s)?;
            marshal_signature(out, s);
        }
        (Some(b'v'), Value::Variant(inner_sig, inner)) => {
            check_signature(inner_sig)?;
            if split_types(inner_sig).len() != 1 {
                return Err(format!("variant signature `{inner_sig}` is not one type"));
            }
            marshal_signature(out, inner_sig);
            marshal_at(out, inner_sig, inner, depth)?;
        }
        (Some(b'a'), Value::Dict(pairs)) if sig[1..].starts_with('{') => {
            let entry = dict_entry(&sig[1..])?;
            let mut chars = entry.chars().peekable();
            let key_sig = take_one_type(&mut chars);
            let value_sig: String = chars.collect();
            marshal_array_of(out, 8, |out| {
                for (key, value) in pairs {
                    pad_to(out, 8);
                    marshal_at(out, &key_sig, key, depth)?;
                    marshal_at(out, &value_sig, value, depth)?;
                }
                Ok(())
            })?;
        }
        (Some(b'a'), Value::Array(items)) if !sig[1..].starts_with('{') => {
            let element = &sig[1..];
            if element.is_empty() {
                return Err("an array signature with no element type".into());
            }
            marshal_array_of(out, element_align(element), |out| {
                for item in items {
                    marshal_at(out, element, item, depth)?;
                }
                Ok(())
            })?;
        }
        (Some(b'('), Value::Struct(fields)) => {
            let inner = sig
                .strip_prefix('(')
                .and_then(|s| s.strip_suffix(')'))
                .ok_or_else(|| format!("unterminated struct signature `{sig}`"))?;
            let types = split_types(inner);
            if types.len() != fields.len() {
                return Err(format!(
                    "struct `{sig}` has {} fields, the value {}",
                    types.len(),
                    fields.len()
                ));
            }
            pad_to(out, 8);
            for (one, field) in types.iter().zip(fields) {
                marshal_at(out, one, field, depth)?;
            }
        }
        _ => return Err(format!("cannot send {} as `{sig}`", kind_of(value))),
    }
    Ok(())
}

/// A fixed-width number on its own natural boundary, which for every D-Bus
/// scalar is its own width.
fn put_aligned(out: &mut Vec<u8>, bytes: &[u8]) {
    pad_to(out, bytes.len());
    out.extend_from_slice(bytes);
}

/// [`marshal_array`] for contents that can fail, with the specification's
/// size ceiling checked once they are there.
fn marshal_array_of(
    out: &mut Vec<u8>,
    elem_align: usize,
    contents: impl FnOnce(&mut Vec<u8>) -> Result<(), String>,
) -> Result<(), String> {
    pad_to(out, 4);
    let len_at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    pad_to(out, elem_align);
    let start = out.len();
    contents(out)?;
    let len = out.len() - start;
    if len > MAX_ARRAY {
        return Err("a D-Bus array too large to send".into());
    }
    out[len_at..len_at + 4].copy_from_slice(&(len as u32).to_le_bytes());
    Ok(())
}

/// What kind of value this is, for an error that says what did not fit.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "a bool",
        Value::U8(_) => "a byte",
        Value::U16(_) | Value::I16(_) => "a 16-bit integer",
        Value::U32(_) | Value::I32(_) => "a 32-bit integer",
        Value::U64(_) | Value::I64(_) => "a 64-bit integer",
        Value::F64(_) => "a double",
        Value::Str(_) => "a string",
        Value::Path(_) => "an object path",
        Value::Signature(_) => "a signature",
        Value::Array(_) => "an array",
        Value::Dict(_) => "a dictionary",
        Value::Struct(_) => "a struct",
        Value::Variant(..) => "a variant",
    }
}

/// A signature split into its complete types: `sa{sv}(ub)` → `s`, `a{sv}`,
/// `(ub)`.
fn split_types(sig: &str) -> Vec<String> {
    let mut chars = sig.chars().peekable();
    std::iter::from_fn(|| Some(take_one_type(&mut chars)).filter(|one| !one.is_empty())).collect()
}

/// Whether `sig` is a signature the bus will accept: at most 255 bytes of
/// complete types, containers closed, dictionary keys basic, no empty struct.
pub fn check_signature(sig: &str) -> Result<(), String> {
    if sig.len() > 255 {
        return Err("a D-Bus signature longer than 255 bytes".into());
    }
    let bytes = sig.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        at = complete_type_end(bytes, at, 0)
            .map_err(|e| format!("bad D-Bus signature `{sig}`: {e}"))?;
    }
    Ok(())
}

/// Where the complete type starting at `bytes[at]` ends.
fn complete_type_end(bytes: &[u8], at: usize, depth: u32) -> Result<usize, String> {
    // The specification's own limits: 32 arrays and 32 structs deep.
    if depth > 2 * MAX_NESTING {
        return Err("nested too deeply".into());
    }
    const BASIC: &[u8] = b"ybnqiuxtdsogh";
    match bytes.get(at) {
        None => Err("ends in the middle of a type".into()),
        Some(c) if BASIC.contains(c) || *c == b'v' => Ok(at + 1),
        Some(b'a') if bytes.get(at + 1) == Some(&b'{') => {
            if !bytes.get(at + 2).is_some_and(|key| BASIC.contains(key)) {
                return Err("a dictionary key must be a basic type".into());
            }
            let end = complete_type_end(bytes, at + 3, depth + 1)?;
            if bytes.get(end) != Some(&b'}') {
                return Err("a dictionary entry is one key and one value".into());
            }
            Ok(end + 1)
        }
        Some(b'a') => complete_type_end(bytes, at + 1, depth + 1),
        Some(b'(') => {
            let mut pos = at + 1;
            if bytes.get(pos) == Some(&b')') {
                return Err("an empty struct".into());
            }
            while bytes.get(pos) != Some(&b')') {
                pos = complete_type_end(bytes, pos, depth + 1)?;
            }
            Ok(pos + 1)
        }
        Some(other) => Err(format!("`{}` is not a type", char::from(*other))),
    }
}

/// Whether `path` is an object path: `/`, or `/`-separated non-empty
/// elements of `[A-Za-z0-9_]` with no trailing slash.
pub fn check_object_path(path: &str) -> Result<(), String> {
    let valid = path == "/"
        || path.strip_prefix('/').is_some_and(|rest| {
            rest.split('/').all(|element| {
                !element.is_empty()
                    && element
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
        });
    if valid {
        Ok(())
    } else {
        Err(format!("`{path}` is not a D-Bus object path"))
    }
}

#[cfg(test)]
fn push_field(out: &mut Vec<u8>, code: u8, sig: char, value: &str) {
    pad_to(out, 8);
    out.push(code);
    marshal_signature(out, &sig.to_string());
    marshal_string(out, value);
}

#[cfg(test)]
fn push_signature_field(out: &mut Vec<u8>, sig: &str) {
    pad_to(out, 8);
    out.push(F_SIGNATURE);
    marshal_signature(out, "g");
    marshal_signature(out, sig);
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "truncated D-Bus message".to_string())
}

// ── Unmarshalling ───────────────────────────────────────────────────────────

pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, pos: 0 }
    }

    fn need(&self, n: usize) -> Result<(), String> {
        if self.pos.saturating_add(n) > self.bytes.len() {
            return Err("truncated D-Bus message".into());
        }
        Ok(())
    }

    fn align(&mut self, to: usize) {
        self.pos = self.pos.next_multiple_of(to);
    }

    fn u8(&mut self) -> Result<u8, String> {
        self.need(1)?;
        let b = self.bytes[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16, String> {
        self.align(2);
        self.need(2)?;
        let v = u16::from_le_bytes([self.bytes[self.pos], self.bytes[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        self.align(4);
        self.need(4)?;
        let v = u32_at(self.bytes, self.pos)?;
        self.pos += 4;
        Ok(v)
    }

    fn u64(&mut self) -> Result<u64, String> {
        self.align(8);
        self.need(8)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&self.bytes[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(u64::from_le_bytes(raw))
    }

    pub fn string(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        self.need(len + 1)?;
        let s = String::from_utf8_lossy(&self.bytes[self.pos..self.pos + len]).into_owned();
        self.pos += len + 1;
        Ok(s)
    }

    pub fn signature(&mut self) -> Result<String, String> {
        let len = self.u8()? as usize;
        self.need(len + 1)?;
        let raw = &self.bytes[self.pos..self.pos + len];
        // Rejected here rather than tolerated downstream: a signature is ASCII
        // type codes and nothing else, and `from_utf8_lossy` on arbitrary bytes
        // produces a `String` whose char boundaries are not its byte
        // boundaries — which the `sig[1..len - 1]` slicing below would then
        // panic on. One check at the door beats three at the windows.
        if !raw.is_ascii() {
            return Err("D-Bus signature is not ASCII".into());
        }
        let s = String::from_utf8_lossy(raw).into_owned();
        self.pos += len + 1;
        Ok(s)
    }

    /// Read one value of `sig`.
    ///
    /// The recursive half of the client, and the reason
    /// `GetManagedObjects` is readable at all: the signature says what to do at
    /// every step, so a reply describing interfaces this build has never heard
    /// of parses correctly and is then simply not looked at.
    pub fn value(&mut self, sig: &str) -> Result<Value, String> {
        self.value_at(sig, 0)
    }

    fn value_at(&mut self, sig: &str, depth: u32) -> Result<Value, String> {
        let mut chars = sig.chars().peekable();
        let one = take_one_type(&mut chars);
        if one.is_empty() {
            return Err("empty D-Bus signature".into());
        }
        self.one_at(&one, depth)
    }

    fn one(&mut self, sig: &str) -> Result<Value, String> {
        self.one_at(sig, 0)
    }

    /// `depth` is how many containers deep this value sits.
    ///
    /// It exists for one shape: a variant whose contents are a variant, whose
    /// contents are a variant. The signature of each is three bytes on the
    /// wire, and without a limit twelve kilobytes of body is enough recursion
    /// to overflow the thread's stack — which is an abort, not an error the
    /// worker can report. The spec's own ceiling is 32 nested containers, so
    /// nothing legitimate is refused.
    fn one_at(&mut self, sig: &str, depth: u32) -> Result<Value, String> {
        if depth > MAX_NESTING {
            return Err("D-Bus value is nested too deeply".into());
        }
        let depth = depth + 1;
        let mut chars = sig.chars();
        let Some(head) = chars.next() else {
            return Err("empty D-Bus signature".into());
        };
        match head {
            'y' => Ok(Value::U8(self.u8()?)),
            'b' => Ok(Value::Bool(self.u32()? != 0)),
            'n' => Ok(Value::I16(self.u16()? as i16)),
            'q' => Ok(Value::U16(self.u16()?)),
            'i' => Ok(Value::I32(self.u32()? as i32)),
            'u' => Ok(Value::U32(self.u32()?)),
            'x' => Ok(Value::I64(self.u64()? as i64)),
            't' => Ok(Value::U64(self.u64()?)),
            'd' => Ok(Value::F64(f64::from_bits(self.u64()?))),
            's' => Ok(Value::Str(self.string()?)),
            'o' => Ok(Value::Path(self.string()?)),
            'g' => Ok(Value::Signature(self.signature()?)),
            'h' => Ok(Value::U32(self.u32()?)),
            'v' => {
                let inner = self.signature()?;
                if split_types(&inner).len() != 1 {
                    return Err(format!("variant signature `{inner}` is not one type"));
                }
                let value = self.value_at(&inner, depth)?;
                Ok(Value::Variant(inner, Box::new(value)))
            }
            'a' => {
                let element = sig[1..].to_string();
                if element.is_empty() {
                    return Err("array with no element type".into());
                }
                let len = self.u32()? as usize;
                self.align(element_align(&element));
                let end = self
                    .pos
                    .checked_add(len)
                    .ok_or_else(|| "absurd array length".to_string())?;
                if end > self.bytes.len() {
                    return Err("truncated D-Bus array".into());
                }
                let dict = element.starts_with('{');
                let mut items = Vec::new();
                let mut pairs = Vec::new();
                while self.pos < end {
                    // Where the element started, so an element type that
                    // consumes nothing — `a()`, an array of empty structs — is
                    // a parse error rather than a loop that pushes `Value`s
                    // until the allocator gives up.
                    let before = self.pos;
                    if dict {
                        self.align(8);
                        if self.pos >= end {
                            break;
                        }
                        let inner = dict_entry(&element)?;
                        let mut chars = inner.chars().peekable();
                        let key_sig = take_one_type(&mut chars);
                        let value_sig: String = chars.collect();
                        let key = self.one_at(&key_sig, depth)?;
                        let value = self.one_at(&value_sig, depth)?;
                        pairs.push((key, value));
                    } else {
                        items.push(self.one_at(&element, depth)?);
                    }
                    if self.pos <= before {
                        return Err("D-Bus array element consumes no bytes".into());
                    }
                }
                // Trust the declared length over where the elements happened to
                // stop: a reader that ran short would desynchronise everything
                // after it.
                self.pos = end;
                Ok(if dict {
                    Value::Dict(pairs)
                } else {
                    Value::Array(items)
                })
            }
            '(' => {
                self.align(8);
                // `take_one_type` hands back an *unclosed* opener verbatim, so
                // `sig` can be a bare `(` — and `[1..len - 1]` on that is the
                // range `1..0`, which panics. An unterminated struct is a
                // malformed signature and says so.
                let inner = sig
                    .strip_prefix('(')
                    .and_then(|s| s.strip_suffix(')'))
                    .ok_or_else(|| "unterminated D-Bus struct signature".to_string())?;
                let mut chars = inner.chars().peekable();
                let mut fields = Vec::new();
                loop {
                    let one = take_one_type(&mut chars);
                    if one.is_empty() {
                        break;
                    }
                    fields.push(self.one_at(&one, depth)?);
                }
                Ok(Value::Struct(fields))
            }
            other => Err(format!("unhandled D-Bus type '{other}'")),
        }
    }

    /// Read every value a body of signature `sig` holds, in order.
    pub fn values(&mut self, sig: &str) -> Result<Vec<Value>, String> {
        let mut chars = sig.chars().peekable();
        let mut out = Vec::new();
        loop {
            let one = take_one_type(&mut chars);
            if one.is_empty() {
                return Ok(out);
            }
            out.push(self.one(&one)?);
        }
    }

    /// Step over a value of `sig` without interpreting it.
    pub fn skip(&mut self, sig: &str) -> Result<(), String> {
        let mut chars = sig.chars().peekable();
        loop {
            let one = take_one_type(&mut chars);
            if one.is_empty() {
                return Ok(());
            }
            self.one(&one)?;
        }
    }
}

/// The key-and-value signature inside a `{...}` dict entry.
///
/// The same trap `(` has: `take_one_type` returns an unclosed `{` as itself,
/// and slicing the braces off that is a backwards range.
fn dict_entry(element: &str) -> Result<&str, String> {
    element
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| "unterminated D-Bus dict entry signature".to_string())
}

/// The alignment of the first element of an array of `sig`.
fn element_align(sig: &str) -> usize {
    match sig.chars().next() {
        Some('y' | 'g' | 'v') => 1,
        Some('n' | 'q') => 2,
        Some('x' | 't' | 'd' | '(' | '{') => 8,
        _ => 4,
    }
}

/// Pull one complete type off the front of a signature — `s`, or `(us)`, or
/// `a{sv}` — so an array knows what its elements are.
fn take_one_type(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut out = String::new();
    loop {
        let Some(c) = chars.next() else { return out };
        out.push(c);
        match c {
            'a' => continue,
            '(' | '{' => {
                let mut depth = 1;
                for c in chars.by_ref() {
                    out.push(c);
                    match c {
                        '(' | '{' => depth += 1,
                        ')' | '}' => {
                            depth -= 1;
                            if depth == 0 {
                                return out;
                            }
                        }
                        _ => {}
                    }
                }
                return out;
            }
            _ => return out,
        }
    }
}

/// One object's interfaces, and each interface's properties.
pub type Interfaces = HashMap<String, HashMap<String, Value>>;

/// Parse an `ObjectManager.GetManagedObjects` reply — `a{oa{sa{sv}}}`.
///
/// Kept as its own function because it is the only shape this client has to
/// understand deeply, and because it is then a test over a hand-built body
/// rather than something that needs a machine with disks in it.
pub fn parse_managed_objects(body: &[u8]) -> Result<Vec<(String, Interfaces)>, String> {
    let mut reader = Reader::new(body);
    let value = reader.value("a{oa{sa{sv}}}")?;
    let Value::Dict(objects) = value else {
        return Err("GetManagedObjects did not answer with a dictionary".into());
    };
    let mut out = Vec::new();
    for (path, interfaces) in objects {
        let Some(path) = path.as_str().map(str::to_string) else {
            continue;
        };
        let Value::Dict(interfaces) = interfaces else {
            continue;
        };
        let mut map: Interfaces = HashMap::new();
        for (name, properties) in interfaces {
            let Some(name) = name.as_str().map(str::to_string) else {
                continue;
            };
            let Value::Dict(properties) = properties else {
                continue;
            };
            // Unwrapped here, once: a property *is* its value to every reader
            // in `mounts`, and the variant around it says nothing they use.
            let props = properties
                .into_iter()
                .filter_map(|(key, value)| Some((key.as_str()?.to_string(), value.into_peeled())))
                .collect();
            map.insert(name, props);
        }
        out.push((path, map));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;

    /// Build a body the way a service would, so the reader can be checked
    /// against something other than itself.
    fn by_hand(out: &mut Vec<u8>, value: &Value) {
        match value {
            Value::U8(n) => out.push(*n),
            Value::Bool(b) => {
                pad_to(out, 4);
                out.extend_from_slice(&u32::from(*b).to_le_bytes());
            }
            Value::U32(n) => {
                pad_to(out, 4);
                out.extend_from_slice(&n.to_le_bytes());
            }
            Value::U64(n) => {
                pad_to(out, 8);
                out.extend_from_slice(&n.to_le_bytes());
            }
            Value::F64(f) => {
                pad_to(out, 8);
                out.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            Value::Str(s) | Value::Path(s) => marshal_string(out, s),
            Value::Signature(s) => marshal_signature(out, s),
            other => panic!("the fixture does not marshal {other:?}"),
        }
    }

    /// A variant: its signature, then the value.
    fn marshal_variant(out: &mut Vec<u8>, sig: &str, value: &Value) {
        marshal_signature(out, sig);
        by_hand(out, value);
    }

    /// A `ay` byte string, the way udisks2 spells a path.
    fn marshal_bytestring(out: &mut Vec<u8>, text: &str) {
        marshal_array(out, 1, |out| {
            out.extend_from_slice(text.as_bytes());
            out.push(0);
        });
    }

    /// The header round trip: what this file writes, this file reads back —
    /// including the 8-byte boundary the body has to start on.
    #[test]
    fn a_method_call_survives_being_parsed_back() {
        let mut body = Vec::new();
        marshal_string(&mut body, "hello");
        let mut fields = Vec::new();
        push_field(&mut fields, F_PATH, 'o', "/org/freedesktop/UDisks2");
        push_field(&mut fields, F_DESTINATION, 's', "org.freedesktop.UDisks2");
        push_field(
            &mut fields,
            F_INTERFACE,
            's',
            "org.freedesktop.DBus.ObjectManager",
        );
        push_field(&mut fields, F_MEMBER, 's', "GetManagedObjects");
        push_signature_field(&mut fields, "s");

        let bytes = encode_message(MSG_METHOD_CALL, 0, 7, &fields, &body);
        assert_eq!(frame_len(&bytes).unwrap(), bytes.len());
        // The body always begins on an 8-byte boundary — the rule the whole
        // format hangs off.
        assert_eq!((bytes.len() - body.len()) % 8, 0);

        let msg = parse_message(&bytes).unwrap();
        assert_eq!(msg.kind, MSG_METHOD_CALL);
        assert_eq!(msg.serial, 7);
        assert_eq!(msg.path.as_deref(), Some("/org/freedesktop/UDisks2"));
        assert_eq!(msg.member.as_deref(), Some("GetManagedObjects"));
        assert_eq!(
            msg.interface.as_deref(),
            Some("org.freedesktop.DBus.ObjectManager")
        );
        assert_eq!(Reader::new(&msg.body).string().unwrap(), "hello");
    }

    /// Truncation at every length is an error, never a panic and never a
    /// half-read message.
    #[test]
    fn every_truncation_of_a_message_is_refused_rather_than_guessed() {
        let mut fields = Vec::new();
        push_field(&mut fields, F_PATH, 'o', "/x");
        push_field(&mut fields, F_MEMBER, 's', "Mount");
        let mut body = Vec::new();
        marshal_string(&mut body, "/run/media/brian/USB");
        let bytes = encode_message(MSG_METHOD_RETURN, 0, 3, &fields, &body);
        for cut in 0..bytes.len() {
            // Whatever it does, it must not panic.
            let _ = parse_message(&bytes[..cut]);
        }
        assert!(parse_message(&bytes).is_ok());
        // A big-endian message is refused rather than mis-read.
        let mut be = bytes.clone();
        be[0] = b'B';
        assert!(parse_message(&be).is_err());
    }

    /// Every scalar type the client can meet, read back as itself.
    #[test]
    fn scalars_round_trip_through_the_reader() {
        let cases: Vec<(&str, Value)> = vec![
            ("y", Value::U8(200)),
            ("b", Value::Bool(true)),
            ("u", Value::U32(4_000_000_000)),
            ("t", Value::U64(1 << 42)),
            ("d", Value::F64(1.5)),
            ("s", Value::Str("label".into())),
            (
                "o",
                Value::Path("/org/freedesktop/UDisks2/block_devices/sda1".into()),
            ),
        ];
        for (sig, value) in cases {
            let mut body = Vec::new();
            by_hand(&mut body, &value);
            let read = Reader::new(&body).value(sig).unwrap();
            assert_eq!(read, value, "signature {sig}");
        }
    }

    /// A byte string is a path, and the trailing NUL udisks2 sends is not part
    /// of it.
    #[test]
    fn byte_arrays_become_paths() {
        let mut body = Vec::new();
        marshal_bytestring(&mut body, "/dev/sda1");
        let value = Reader::new(&body).value("ay").unwrap();
        assert_eq!(value.as_bytestring().as_deref(), Some("/dev/sda1"));

        // `aay` — the mount points array, including the empty case, which is
        // what an unmounted filesystem reports and must not be an error.
        let mut body = Vec::new();
        marshal_array(&mut body, 4, |out| {
            marshal_bytestring(out, "/run/media/brian/USB");
            marshal_bytestring(out, "/mnt/other");
        });
        let value = Reader::new(&body).value("aay").unwrap();
        assert_eq!(
            value.as_bytestrings(),
            Some(vec![
                "/run/media/brian/USB".to_string(),
                "/mnt/other".to_string()
            ])
        );
        let mut empty = Vec::new();
        marshal_array(&mut empty, 4, |_| {});
        assert_eq!(
            Reader::new(&empty).value("aay").unwrap().as_bytestrings(),
            Some(Vec::new())
        );
    }

    /// The one shape that matters: a `GetManagedObjects` reply, hand-built the
    /// way udisks2 sends it, parsed back into objects and properties.
    #[test]
    fn a_managed_objects_reply_parses_into_objects_and_interfaces() {
        let mut body = Vec::new();
        // a{o a{s a{s v}}}
        marshal_array(&mut body, 8, |out| {
            for (path, label, size, mounted) in [
                (
                    "/org/freedesktop/UDisks2/block_devices/sda1",
                    "USB",
                    8_000_000_000u64,
                    true,
                ),
                (
                    "/org/freedesktop/UDisks2/block_devices/sdb1",
                    "Backup",
                    16u64,
                    false,
                ),
            ] {
                pad_to(out, 8);
                marshal_string(out, path);
                marshal_array(out, 8, |out| {
                    pad_to(out, 8);
                    marshal_string(out, "org.freedesktop.UDisks2.Block");
                    marshal_array(out, 8, |out| {
                        pad_to(out, 8);
                        marshal_string(out, "IdLabel");
                        marshal_variant(out, "s", &Value::Str(label.to_string()));
                        pad_to(out, 8);
                        marshal_string(out, "Size");
                        marshal_variant(out, "t", &Value::U64(size));
                        pad_to(out, 8);
                        marshal_string(out, "Device");
                        marshal_signature(out, "ay");
                        marshal_bytestring(out, "/dev/sda1");
                    });
                    pad_to(out, 8);
                    marshal_string(out, "org.freedesktop.UDisks2.Filesystem");
                    marshal_array(out, 8, |out| {
                        pad_to(out, 8);
                        marshal_string(out, "MountPoints");
                        marshal_signature(out, "aay");
                        marshal_array(out, 4, |out| {
                            if mounted {
                                marshal_bytestring(out, "/run/media/brian/USB");
                            }
                        });
                    });
                });
            }
        });

        let objects = parse_managed_objects(&body).unwrap();
        assert_eq!(objects.len(), 2);
        let (path, interfaces) = &objects[0];
        assert_eq!(path, "/org/freedesktop/UDisks2/block_devices/sda1");
        let block = &interfaces["org.freedesktop.UDisks2.Block"];
        assert_eq!(block["IdLabel"].as_str(), Some("USB"));
        assert_eq!(block["Size"].as_u64(), Some(8_000_000_000));
        assert_eq!(
            block["Device"].as_bytestring().as_deref(),
            Some("/dev/sda1")
        );
        let fs = &interfaces["org.freedesktop.UDisks2.Filesystem"];
        assert_eq!(
            fs["MountPoints"].as_bytestrings(),
            Some(vec!["/run/media/brian/USB".to_string()])
        );
        // The second device has the same interfaces and no mount point.
        let (_, second) = &objects[1];
        assert_eq!(
            second["org.freedesktop.UDisks2.Filesystem"]["MountPoints"].as_bytestrings(),
            Some(Vec::new())
        );
        assert_eq!(
            second["org.freedesktop.UDisks2.Block"]["IdLabel"].as_str(),
            Some("Backup")
        );
    }

    /// An empty reply is an answer, not a failure: a machine with no removable
    /// disks is a normal machine.
    #[test]
    fn an_empty_object_list_is_not_an_error() {
        let mut body = Vec::new();
        marshal_array(&mut body, 8, |_| {});
        assert!(parse_managed_objects(&body).unwrap().is_empty());
    }

    /// The options argument every udisks2 method takes.
    ///
    /// Eight bytes, not four: the length word is zero *and* the padding to the
    /// first element's 8-byte boundary is still required. The spec is explicit
    /// that an empty array carries its alignment padding anyway, and getting
    /// this wrong is a call that is refused with no explanation.
    #[test]
    fn the_empty_options_dictionary_carries_its_alignment_padding() {
        let mut body = Vec::new();
        marshal_no_options(&mut body);
        assert_eq!(body, vec![0; 8]);
        assert_eq!(
            Reader::new(&body).value("a{sv}").unwrap(),
            Value::Dict(vec![])
        );
    }

    /// The SASL line is the uid's decimal digits, hex-encoded — a fiddly little
    /// rule that is wrong in exactly one way and silent about it.
    #[test]
    fn the_auth_line_hex_encodes_the_decimal_uid() {
        assert_eq!(auth_line(1000), "\0AUTH EXTERNAL 31303030\r\n");
        assert_eq!(auth_line(0), "\0AUTH EXTERNAL 30\r\n");
    }

    #[test]
    fn bus_addresses_yield_their_socket() {
        let path = |p: &str| BusSocket::Path(std::path::PathBuf::from(p));
        assert_eq!(
            bus_socket("unix:path=/run/dbus/system_bus_socket").unwrap(),
            path("/run/dbus/system_bus_socket")
        );
        assert_eq!(
            bus_socket("unix:guid=abc,path=/tmp/bus").unwrap(),
            path("/tmp/bus")
        );
        // The abstract namespace, which `dbus-daemon --session` listens on
        // when its config says `unix:tmpdir=`.
        assert_eq!(
            bus_socket("unix:abstract=/tmp/dbus-XyZ,guid=0123").unwrap(),
            BusSocket::Abstract(b"/tmp/dbus-XyZ".to_vec())
        );
        // Escapes are undone, and a transport this client cannot use is
        // skipped for the next entry rather than failing the address.
        assert_eq!(
            bus_socket("tcp:host=localhost,port=1;unix:path=/run/user/1000/my%20bus").unwrap(),
            path("/run/user/1000/my bus")
        );
        assert!(bus_socket("tcp:host=localhost,port=1").is_err());
        assert!(bus_socket("unix:path=/tmp/bad%2").is_err());
    }

    /// The three refusals a file manager actually meets, each turned into a
    /// sentence with a next step in it.
    #[test]
    fn errors_become_sentences() {
        assert!(
            readable_error("org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain", "")
                .contains("not allowed")
        );
        assert!(
            readable_error("org.freedesktop.UDisks2.Error.DeviceBusy", "")
                .contains("still has a file open")
        );
        assert_eq!(
            readable_error("org.freedesktop.DBus.Error.ServiceUnknown", ""),
            "udisks2 is not running on this machine"
        );
        // Anything else keeps whatever the service said…
        assert_eq!(readable_error("some.Other.Error", "it broke"), "it broke");
        // …and falls back to the short name when it said nothing.
        assert_eq!(readable_error("some.Other.Weird", ""), "Weird");
        assert_eq!(readable_error("", ""), "the call failed");
    }

    /// A signature is walked one complete type at a time, nesting included.
    #[test]
    fn signatures_are_split_into_whole_types() {
        let mut chars = "sa{sv}(ub)".chars().peekable();
        assert_eq!(take_one_type(&mut chars), "s");
        assert_eq!(take_one_type(&mut chars), "a{sv}");
        assert_eq!(take_one_type(&mut chars), "(ub)");
        assert_eq!(take_one_type(&mut chars), "");
    }

    /// A variant whose contents this build has never heard of is read past
    /// rather than fatal — which is what lets a newer udisks2 add properties.
    #[test]
    fn unknown_values_are_skipped_not_fatal() {
        let mut body = Vec::new();
        marshal_array(&mut body, 8, |out| {
            pad_to(out, 8);
            marshal_string(out, "Known");
            marshal_variant(out, "s", &Value::Str("yes".into()));
            pad_to(out, 8);
            marshal_string(out, "Strange");
            marshal_signature(out, "(uus)");
            pad_to(out, 8);
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(&2u32.to_le_bytes());
            marshal_string(out, "three");
        });
        let Value::Dict(pairs) = Reader::new(&body).value("a{sv}").unwrap() else {
            panic!("not a dictionary");
        };
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].1.as_str(), Some("yes"));
        assert_eq!(
            *pairs[1].1.peeled(),
            Value::Struct(vec![
                Value::U32(1),
                Value::U32(2),
                Value::Str("three".into())
            ])
        );
    }

    /// Signals and errors are recognised for what they are.
    #[test]
    fn errors_and_signals_parse_as_themselves() {
        let mut fields = Vec::new();
        push_u32_field(&mut fields, F_REPLY_SERIAL, 9);
        push_field(
            &mut fields,
            F_ERROR_NAME,
            's',
            "org.freedesktop.UDisks2.Error.DeviceBusy",
        );
        push_signature_field(&mut fields, "s");
        let mut body = Vec::new();
        marshal_string(&mut body, "target is busy");
        let bytes = encode_message(MSG_ERROR, 0, 12, &fields, &body);
        let msg = parse_message(&bytes).unwrap();
        assert_eq!(msg.kind, MSG_ERROR);
        assert_eq!(msg.reply_serial, Some(9));
        assert!(msg.error_text().contains("still has a file open"));

        let mut fields = Vec::new();
        push_field(&mut fields, F_PATH, 'o', "/org/freedesktop/UDisks2");
        push_field(&mut fields, F_MEMBER, 's', "InterfacesAdded");
        let bytes = encode_message(MSG_SIGNAL, 0, 13, &fields, &[]);
        let msg = parse_message(&bytes).unwrap();
        assert_eq!(msg.kind, MSG_SIGNAL);
        assert_eq!(msg.member.as_deref(), Some("InterfacesAdded"));
        assert!(msg.reply_serial.is_none());
    }

    fn push_u32_field(out: &mut Vec<u8>, code: u8, value: u32) {
        pad_to(out, 8);
        out.push(code);
        marshal_signature(out, "u");
        pad_to(out, 4);
        out.extend_from_slice(&value.to_le_bytes());
    }

    // ── The two waits, and who is allowed to end them ───────────────────────

    /// One method return, as the bus would route it.
    fn reply_from(sender: &str, reply_serial: u32, text: &str) -> Vec<u8> {
        let mut fields = Vec::new();
        push_u32_field(&mut fields, F_REPLY_SERIAL, reply_serial);
        push_field(&mut fields, F_SENDER, 's', sender);
        push_signature_field(&mut fields, "s");
        let mut body = Vec::new();
        marshal_string(&mut body, text);
        encode_message(MSG_METHOD_RETURN, 0, 77, &fields, &body)
    }

    /// A broadcast nobody asked for — what udisks2 sends while a call is in
    /// flight, and what a hostile peer would send a great many of.
    fn a_signal() -> Vec<u8> {
        let mut fields = Vec::new();
        push_field(&mut fields, F_PATH, 'o', "/org/freedesktop/UDisks2");
        push_field(&mut fields, F_MEMBER, 's', "InterfacesAdded");
        push_field(&mut fields, F_SENDER, 's', ":1.9");
        encode_message(MSG_SIGNAL, 0, 5, &fields, &[])
    }

    /// Read one whole message off a socket, the way the client does.
    fn read_one(sock: &mut UnixStream) -> Vec<u8> {
        let mut head = [0u8; 16];
        sock.read_exact(&mut head).unwrap();
        let total = frame_len(&head).unwrap();
        let mut rest = vec![0u8; total - 16];
        sock.read_exact(&mut rest).unwrap();
        let mut all = head.to_vec();
        all.extend_from_slice(&rest);
        all
    }

    /// **The bug this fixes**: the reply this client accepted was "the first
    /// message carrying my serial", and the serial started at 0 and stepped by
    /// one — so the serial of the next call was 1, from the outside, and any
    /// local process could answer a call it had not been asked. The mount list
    /// drives no destructive operation on its own, but it *feeds device nodes
    /// to mount and unmount*, so a forged `GetManagedObjects` chooses which
    /// device the next keystroke acts on.
    ///
    /// Two locks now: an unguessable starting serial, and the `sender` the bus
    /// stamps — which a peer cannot forge, and which must be who was called.
    #[test]
    fn a_reply_is_believed_only_from_the_right_serial_and_the_right_sender() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut bus = Bus::on_socket(ours);
        bus.timeout = Duration::from_secs(5);
        // The serial this call will use. Not 1, which is the whole point.
        let serial = bus.serial.wrapping_add(1).max(1);
        assert!(serial > 1, "a fresh connection does not start at 1");

        let peer = std::thread::spawn(move || {
            let mut peer = theirs;
            let call = read_one(&mut peer);
            let sent = parse_message(&call).unwrap();
            assert_eq!(sent.member.as_deref(), Some("Hello"));
            assert_eq!(sent.serial, serial);
            // A blind forgery at the serial this client used to have.
            peer.write_all(&reply_from(BUS_DRIVER, 1, "forged-blind"))
                .unwrap();
            // A race: the right serial, from somebody who was not called.
            peer.write_all(&reply_from(":1.66", serial, "forged-race"))
                .unwrap();
            // Noise, which is legitimate and must not be mistaken for either.
            peer.write_all(&a_signal()).unwrap();
            // And the real one.
            peer.write_all(&reply_from(BUS_DRIVER, serial, "genuine"))
                .unwrap();
        });

        let body = bus
            .call(
                BUS_DRIVER,
                "/org/freedesktop/DBus",
                BUS_DRIVER,
                "Hello",
                None,
                &[],
            )
            .unwrap();
        assert_eq!(Reader::new(&body).string().unwrap(), "genuine");
        peer.join().unwrap();
    }

    /// **The bug this fixes**: the discard loop had no deadline of its own.
    /// The socket's read timeout is per read, so a peer that sends *anything* —
    /// a signal every twenty milliseconds — reset it for ever and the worker
    /// thread never came back. Ninety seconds is the promise; this checks the
    /// promise is kept when the peer is chatty rather than silent.
    #[test]
    fn a_call_the_peer_talks_over_gives_up_on_time() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut bus = Bus::on_socket(ours);
        bus.timeout = Duration::from_millis(200);

        let peer = std::thread::spawn(move || {
            let mut peer = theirs;
            let _call = read_one(&mut peer);
            // Never the reply. Just enough noise to keep a per-read timeout
            // alive for as long as anybody is listening.
            for _ in 0..200 {
                if peer.write_all(&a_signal()).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        let start = std::time::Instant::now();
        let err = bus
            .call(
                BUS_DRIVER,
                "/org/freedesktop/DBus",
                BUS_DRIVER,
                "Hello",
                None,
                &[],
            )
            .unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "it gave up when it said it would: {:?}",
            start.elapsed()
        );
        drop(bus);
        peer.join().unwrap();
    }

    /// The same deadline over the handshake, whose reads are one byte each.
    #[test]
    fn a_handshake_that_never_ends_is_not_waited_on_for_ever() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut bus = Bus::on_socket(ours);
        bus.timeout = Duration::from_millis(150);
        let peer = std::thread::spawn(move || {
            let mut peer = theirs;
            let mut byte = [0u8; 1];
            // Read the AUTH line, then trickle a reply that never terminates.
            let _ = peer.read(&mut byte);
            for _ in 0..100 {
                if peer.write_all(b"O").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let start = std::time::Instant::now();
        let err = bus.authenticate().unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(5));
        drop(bus);
        peer.join().unwrap();
    }

    /// The seed itself: in range, never the reserved 0, and never the constant
    /// a forger would try first.
    #[test]
    fn the_first_serial_is_not_a_number_anyone_can_guess() {
        for _ in 0..64 {
            let n = start_serial();
            assert!(n > 0, "0 is reserved by the specification");
            assert!(n <= 0x3FFF_FFFF, "room to count without wrapping");
        }
    }
}

/// A peer on the system bus is not trusted, and udisks2 is not the only thing
/// that can answer: any local process can send this connection a reply. So the
/// unmarshaller's failure mode on hostile bytes has to be an `Err`, never a
/// panic and never an abort.
#[cfg(test)]
mod hostile {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;

    /// A body of nested variants: three bytes per level, and before the depth
    /// limit this recursed until the thread's stack was gone — an abort the
    /// worker could not catch and the app could not survive.
    #[test]
    fn a_tower_of_variants_is_refused_rather_than_overflowing_the_stack() {
        let mut body = Vec::new();
        for _ in 0..100_000 {
            body.extend_from_slice(&[1, b'v', 0]);
        }
        body.extend_from_slice(&[1, b'y', 0, 0]);
        let err = Reader::new(&body).value("v").unwrap_err();
        assert!(err.contains("nested too deeply"), "{err}");

        // And the depth a real reply uses still parses.
        let mut ok = Vec::new();
        for _ in 0..8 {
            ok.extend_from_slice(&[1, b'v', 0]);
        }
        ok.extend_from_slice(&[1, b'y', 0, 7]);
        assert!(matches!(
            Reader::new(&ok).value("v").map(Value::into_peeled),
            Ok(Value::U8(7))
        ));
    }

    /// `take_one_type` hands an unterminated container back verbatim, and the
    /// braces used to be sliced off it with `[1..len - 1]` — a backwards range.
    #[test]
    fn an_unterminated_container_signature_is_an_error_not_a_panic() {
        assert!(Reader::new(&[]).value("(").is_err());
        assert!(Reader::new(&[]).value("{").is_err());
        // Reached the way a peer would reach it: through a variant.
        let body = [1, b'(', 0];
        assert!(Reader::new(&body).value("v").is_err());
        // `a{` — an array whose element type is an unclosed dict entry, with a
        // declared length big enough that the loop actually runs an element.
        let mut nested = vec![2, b'a', b'{', 0];
        nested.extend_from_slice(&8u32.to_le_bytes());
        nested.extend_from_slice(&[0u8; 8]);
        assert!(Reader::new(&nested).value("v").is_err());
    }

    /// A signature is ASCII type codes. Anything else used to become a
    /// `String` whose byte offsets are not char boundaries, and slicing it
    /// panicked.
    #[test]
    fn a_signature_that_is_not_ascii_is_refused() {
        let body = [3, b'(', 0xC3, 0xA9, 0];
        assert!(Reader::new(&body).value("v").is_err());
        let invalid = [2, b'(', 0xFF, 0];
        assert!(Reader::new(&invalid).value("v").is_err());
    }

    /// An array of empty structs: every element consumes nothing, so the loop
    /// never reached its end and pushed a `Value` per turn until the allocator
    /// gave up.
    #[test]
    fn an_array_of_zero_width_elements_terminates() {
        let mut body = Vec::new();
        body.extend_from_slice(&8u32.to_le_bytes());
        // Structs are 8-aligned, so the elements start on the next boundary.
        body.extend_from_slice(&[0u8; 4]);
        body.extend_from_slice(&[0u8; 8]);
        let err = Reader::new(&body).value("a()").unwrap_err();
        assert!(err.contains("consumes no bytes"), "{err}");
    }

    /// The same three shapes, arriving as a *header field* — the path that runs
    /// before any reply-serial filter, on every message the bus delivers.
    #[test]
    fn a_hostile_header_field_cannot_panic_the_parser() {
        for variant in [
            vec![1, b'(', 0],
            vec![1, b'{', 0],
            vec![3, b'(', 0xC3, 0xA9, 0],
        ] {
            let mut fields = Vec::new();
            pad_to(&mut fields, 8);
            fields.push(9u8); // a field code this client does not know
            fields.extend_from_slice(&variant);
            let msg = encode_message(MSG_METHOD_RETURN, 0, 1, &fields, &[]);
            assert!(parse_message(&msg).is_err());
        }
    }
}

/// The generic layer the portal service stands on: every signature the
/// file-chooser interface uses, marshalled and read back, and the framing in
/// both directions.
#[cfg(test)]
mod generic {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;

    fn s(text: &str) -> Value {
        Value::Str(text.to_string())
    }

    /// One `(sa(us))` filter: a name and its `(kind, pattern)` pairs.
    fn filter(name: &str, patterns: &[(u32, &str)]) -> Value {
        Value::Struct(vec![
            s(name),
            Value::Array(
                patterns
                    .iter()
                    .map(|(kind, pattern)| Value::Struct(vec![Value::U32(*kind), s(pattern)]))
                    .collect(),
            ),
        ])
    }

    /// The options a real `OpenFile` / `SaveFile` / `SaveFiles` call carries,
    /// every key the portal documents, each under its own signature.
    fn options() -> Value {
        let filters = Value::Array(vec![
            filter("Images", &[(0, "*.png"), (1, "image/*")]),
            filter("A", &[]),
            filter("Odd", &[(0, "*.x")]),
        ]);
        let choices = Value::Array(vec![Value::Struct(vec![
            s("encoding"),
            s("Encoding"),
            Value::Array(vec![
                Value::Struct(vec![s("utf8"), s("Unicode")]),
                Value::Struct(vec![s("latin15"), s("Western")]),
            ]),
            s("latin15"),
        ])]);
        let pairs = vec![
            ("accept_label", Value::variant("s", s("_Upload"))),
            ("modal", Value::variant("b", Value::Bool(true))),
            ("multiple", Value::variant("b", Value::Bool(false))),
            ("filters", Value::variant("a(sa(us))", filters)),
            (
                "current_filter",
                Value::variant("(sa(us))", filter("Images", &[(0, "*.png")])),
            ),
            ("choices", Value::variant("a(ssa(ss)s)", choices)),
            (
                "current_folder",
                Value::variant("ay", Value::bytes(b"/home/brian/Pictures\0")),
            ),
            (
                "files",
                Value::variant(
                    "aay",
                    Value::Array(vec![Value::bytes(b"a.png\0"), Value::bytes(b"b\0")]),
                ),
            ),
            ("empty", Value::variant("as", Value::Array(vec![]))),
        ];
        Value::Dict(pairs.into_iter().map(|(k, v)| (s(k), v)).collect())
    }

    /// Every signature the portal's calls and answers use.
    fn cases() -> Vec<(&'static str, Value)> {
        vec![
            ("y", Value::U8(0xFE)),
            ("b", Value::Bool(true)),
            ("u", Value::U32(2)),
            ("s", s("Open File")),
            ("s", s("")),
            (
                "o",
                Value::Path("/org/freedesktop/portal/desktop/request/1_42/t".into()),
            ),
            ("g", Value::Signature("a{sv}".into())),
            // A byte array with NULs inside it and at the end: the bytes of a
            // path, which are not text.
            ("ay", Value::bytes(b"/tmp/a\0b\0")),
            ("ay", Value::bytes(b"")),
            (
                "aay",
                Value::Array(vec![
                    Value::bytes(b"one\0"),
                    Value::bytes(b""),
                    Value::bytes(b"x"),
                ]),
            ),
            ("aay", Value::Array(vec![])),
            (
                "as",
                Value::Array(vec![s("file:///a"), s(""), s("file:///%C3%BC")]),
            ),
            ("as", Value::Array(vec![])),
            ("v", Value::variant("as", Value::Array(vec![]))),
            (
                "v",
                Value::variant("(sa(us))", filter("Odd", &[(1, "text/plain")])),
            ),
            (
                "a(sa(us))",
                Value::Array(vec![filter("abc", &[(0, "*.x")]), filter("de", &[])]),
            ),
            ("a(sa(us))", Value::Array(vec![])),
            (
                "(sa(us))",
                filter("Images", &[(0, "*.png"), (1, "image/*")]),
            ),
            (
                "a(ssa(ss)s)",
                Value::Array(vec![Value::Struct(vec![
                    s("k"),
                    s("Label"),
                    Value::Array(vec![]),
                    s("true"),
                ])]),
            ),
            ("a{sv}", options()),
            ("a{sv}", Value::Dict(vec![])),
            // Nested arrays, and a struct straight after an odd-length string.
            (
                "aas",
                Value::Array(vec![Value::Array(vec![s("a")]), Value::Array(vec![])]),
            ),
            (
                "(sy(us))",
                Value::Struct(vec![
                    s("odd"),
                    Value::U8(1),
                    Value::Struct(vec![Value::U32(9), s("z")]),
                ]),
            ),
        ]
    }

    /// Each signature alone, and again behind a byte and an odd-length
    /// string, so its alignment is exercised from an offset that is not
    /// already on its boundary.
    #[test]
    fn every_portal_signature_round_trips_at_every_alignment() {
        for (sig, value) in cases() {
            let mut out = Vec::new();
            marshal(&mut out, sig, &value).unwrap();
            let mut reader = Reader::new(&out);
            assert_eq!(reader.value(sig).unwrap(), value, "`{sig}` alone");
            assert_eq!(reader.pos, out.len(), "`{sig}` read every byte");

            let body_sig = format!("ys{sig}");
            let args = [Value::U8(7), s("odd"), value.clone()];
            let body = marshal_body(&body_sig, &args).unwrap();
            assert_eq!(
                Reader::new(&body).values(&body_sig).unwrap(),
                args,
                "`{sig}` behind a prefix"
            );
        }
    }

    /// The bytes themselves, worked out by hand from the specification — so
    /// the round trip above is not just the encoder agreeing with itself.
    #[test]
    fn a_filter_is_laid_out_the_way_the_specification_says() {
        let mut out = Vec::new();
        marshal(&mut out, "(sa(us))", &filter("abc", &[(0, "*.x")])).unwrap();
        #[rustfmt::skip]
        let expected = vec![
            3, 0, 0, 0, b'a', b'b', b'c', 0,    // "abc"
            12, 0, 0, 0,                        // the array: 12 bytes of content…
            0, 0, 0, 0,                         // …starting on the struct's 8
            0, 0, 0, 0,                         // u 0 (a glob)
            3, 0, 0, 0, b'*', b'.', b'x', 0,    // "*.x"
        ];
        assert_eq!(out, expected);

        // An empty array still pads to where its first element would have
        // been: four bytes of length and four of padding for 8-aligned
        // elements, just the length for 4-aligned ones.
        let mut empty = Vec::new();
        marshal(&mut empty, "a(us)", &Value::Array(vec![])).unwrap();
        assert_eq!(empty, vec![0; 8]);
        let mut empty = Vec::new();
        marshal(&mut empty, "aay", &Value::Array(vec![])).unwrap();
        assert_eq!(empty, vec![0; 4]);
        // A byte array after a byte: the length word waits for offset 4.
        let body = marshal_body("yay", &[Value::U8(7), Value::bytes(b"\0")]).unwrap();
        assert_eq!(body, vec![7, 0, 0, 0, 1, 0, 0, 0, 0]);
    }

    /// A value that does not fit its signature is refused before a byte of it
    /// reaches the bus, which would hang up on it.
    #[test]
    fn a_value_that_does_not_fit_is_refused() {
        let mut out = Vec::new();
        assert!(marshal(&mut out, "u", &s("2")).is_err());
        assert!(marshal(&mut out, "s", &s("a\0b")).is_err());
        assert!(marshal(&mut out, "o", &Value::Path("not/a/path".into())).is_err());
        assert!(marshal(&mut out, "o", &Value::Path("/trailing/".into())).is_err());
        assert!(marshal(&mut out, "o", &Value::Path("/a//b".into())).is_err());
        assert!(marshal(&mut out, "(us)", &Value::Struct(vec![Value::U32(1)])).is_err());
        assert!(marshal(&mut out, "a{sv}", &Value::Array(vec![])).is_err());
        assert!(marshal(&mut out, "as", &Value::Dict(vec![])).is_err());
        assert!(marshal(&mut out, "v", &Value::variant("ss", s("x"))).is_err());
        assert!(marshal(&mut out, "v", &Value::variant("u", s("x"))).is_err());
        assert!(marshal(&mut out, "a", &Value::Array(vec![])).is_err());
        assert!(marshal_body("su", &[s("one")]).is_err());
        assert!(marshal(&mut out, "o", &Value::Path("/".into())).is_ok());
    }

    #[test]
    fn signatures_are_checked_the_way_the_bus_checks_them() {
        for good in [
            "",
            "osssa{sv}",
            "ua{sv}",
            "a(sa(us))",
            "a(ssa(ss)s)",
            "aay",
            "a{oa{sa{sv}}}",
        ] {
            assert!(check_signature(good).is_ok(), "{good}");
        }
        for bad in [
            "a", "(s", "()", "a{vs}", "a{sss}", "a{s}", "z", "s)", "{sv}",
        ] {
            assert!(check_signature(bad).is_err(), "{bad}");
        }
        assert!(check_signature(&"u".repeat(256)).is_err());
    }

    /// A call, a return and an error, each encoded and parsed back into the
    /// message it was — flags, serial, every header field and the body.
    #[test]
    fn messages_survive_the_round_trip_in_both_directions() {
        let call = Message {
            serial: 41,
            sender: Some(":1.7".into()),
            ..Message::method_call(
                "org.freedesktop.impl.portal.desktop.delightfile",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.impl.portal.FileChooser",
                "OpenFile",
            )
        }
        .with_args(
            "osssa{sv}",
            &[
                Value::Path("/org/freedesktop/portal/desktop/request/1_7/t".into()),
                s("org.example.App"),
                s(""),
                s("Open"),
                options(),
            ],
        )
        .unwrap();
        let bytes = call.encode().unwrap();
        assert_eq!(frame_len(&bytes).unwrap(), bytes.len());
        let parsed = parse_message(&bytes).unwrap();
        assert_eq!(parsed, call);
        assert!(parsed.wants_reply());
        assert_eq!(parsed.args().unwrap()[4], options());

        let reply = Message {
            serial: 3,
            ..Message::method_return(&call)
        }
        .with_args(
            "ua{sv}",
            &[
                Value::U32(0),
                Value::Dict(vec![(
                    s("uris"),
                    Value::variant("as", Value::Array(vec![s("file:///a")])),
                )]),
            ],
        )
        .unwrap();
        let parsed = parse_message(&reply.encode().unwrap()).unwrap();
        assert_eq!(parsed, reply);
        assert_eq!(parsed.reply_serial, Some(41));
        assert_eq!(parsed.destination.as_deref(), Some(":1.7"));
        assert!(!parsed.wants_reply(), "nobody answers an answer");

        let error = Message {
            serial: 4,
            ..Message::error(
                &call,
                "org.freedesktop.DBus.Error.UnknownMethod",
                "no \0such",
            )
        };
        let parsed = parse_message(&error.encode().unwrap()).unwrap();
        assert_eq!(parsed.kind, MSG_ERROR);
        assert_eq!(parsed.args().unwrap(), vec![s("no \u{FFFD}such")]);

        // An empty body has no signature field at all.
        let empty = Message {
            serial: 5,
            ..Message::method_return(&call)
        };
        let parsed = parse_message(&empty.encode().unwrap()).unwrap();
        assert_eq!(parsed.signature, None);
        assert_eq!(parsed.args().unwrap(), Vec::<Value>::new());
    }

    /// The encoder and the hand-built fixture agree to the byte on a call —
    /// two spellings of the header that could not share a mistake.
    #[test]
    fn the_encoder_matches_the_header_built_by_hand() {
        let body = marshal_body("s", &[s("hello")]).unwrap();
        let mut fields = Vec::new();
        push_field(&mut fields, F_PATH, 'o', "/org/freedesktop/DBus");
        push_field(&mut fields, F_INTERFACE, 's', "org.freedesktop.DBus");
        push_field(&mut fields, F_MEMBER, 's', "GetNameOwner");
        push_field(&mut fields, F_DESTINATION, 's', "org.freedesktop.DBus");
        push_signature_field(&mut fields, "s");
        let by_hand = encode_message(MSG_METHOD_CALL, 0, 9, &fields, &body);
        let encoded = Message {
            serial: 9,
            ..Message::method_call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetNameOwner",
            )
        }
        .with_args("s", &[s("hello")])
        .unwrap()
        .encode()
        .unwrap();
        assert_eq!(encoded, by_hand);
    }

    /// A body whose bytes do not match its signature is an error from
    /// `args`, never a panic — the service answers it with `InvalidArgs`.
    #[test]
    fn a_body_that_lies_about_its_signature_is_an_error() {
        let mut msg = Message::method_call("a.b", "/", "a.b", "M");
        msg.signature = Some("osssa{sv}".into());
        msg.body = marshal_body("s", &[s("short")]).unwrap();
        assert!(msg.args().is_err());
        msg.signature = None;
        assert!(msg.args().is_err(), "bytes with no signature");
    }

    /// **The bug this fixes**: D-Bus activation delivers the call that
    /// started the service the moment the name is owned — inside
    /// `RequestName`, before its reply. `Bus::call` read past it like a stray
    /// signal, and the first dialog after every login timed out. Now a method
    /// call that arrives mid-call waits in the backlog for the inbox; signals
    /// are still dropped.
    #[test]
    fn a_call_that_arrives_during_request_name_is_served_afterwards() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut bus = Bus::on_socket(ours);
        bus.timeout = Duration::from_secs(5);
        let peer = std::thread::spawn(move || {
            let mut peer = theirs;
            let mut head = [0u8; 16];
            peer.read_exact(&mut head).unwrap();
            let mut rest = vec![0u8; frame_len(&head).unwrap() - 16];
            peer.read_exact(&mut rest).unwrap();
            let request = parse_message(&[&head[..], &rest[..]].concat()).unwrap();
            assert_eq!(request.member.as_deref(), Some("RequestName"));
            assert_eq!(
                request.args().unwrap(),
                vec![s("org.example.Service"), Value::U32(NAME_DO_NOT_QUEUE)]
            );
            // The activating call, queued by the bus until the name was owned…
            let activating = Message {
                serial: 70,
                sender: Some(":1.5".into()),
                ..Message::method_call("org.example.Service", "/x", "x.y", "Activate")
            };
            peer.write_all(&activating.encode().unwrap()).unwrap();
            // …the bus's own broadcast about the name…
            let acquired = Message {
                kind: MSG_SIGNAL,
                serial: 71,
                sender: Some(BUS_DRIVER.into()),
                path: Some("/org/freedesktop/DBus".into()),
                member: Some("NameAcquired".into()),
                ..Message::default()
            };
            peer.write_all(&acquired.encode().unwrap()).unwrap();
            // …and only then the reply: primary owner.
            let reply = Message {
                serial: 72,
                sender: Some(BUS_DRIVER.into()),
                ..Message::method_return(&request)
            }
            .with_args("u", &[Value::U32(1)])
            .unwrap();
            peer.write_all(&reply.encode().unwrap()).unwrap();
            peer
        });
        bus.request_name("org.example.Service").unwrap();
        // The peer hangs up, so a lost call is an error below, not a wait.
        drop(peer.join().unwrap());
        let (mut inbox, _outbox) = bus.into_service().unwrap();
        let first = inbox.next().unwrap();
        assert_eq!(first.member.as_deref(), Some("Activate"));
        assert_eq!(first.serial, 70);
        assert!(inbox.backlog.is_empty(), "the signal was not kept");
    }

    /// Hanging up the writing half ends a read blocked, with no deadline, on
    /// the other — which is how a listener with nothing coming is let go.
    #[test]
    fn hanging_up_wakes_a_waiting_inbox() {
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let (mut inbox, outbox) = Bus::on_socket(ours).into_service().unwrap();
        let reader = std::thread::spawn(move || inbox.next());
        // Long enough that the reader is almost surely in its `read`; the
        // test holds either way, since a shut socket reads as its end.
        std::thread::sleep(Duration::from_millis(30));
        outbox.hang_up();
        assert!(reader.join().unwrap().is_err());
    }

    /// A message the inbox can frame but not read — big-endian, or with a
    /// header this parser refuses — is stepped over, and the next one served.
    #[test]
    fn the_inbox_steps_over_a_message_it_cannot_read() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let (mut inbox, _outbox) = Bus::on_socket(ours).into_service().unwrap();

        let mut big_endian = Message {
            serial: 1,
            ..Message::method_call("x.y", "/a", "x.y", "Odd")
        }
        .encode()
        .unwrap();
        big_endian[0] = b'B';
        for at in [4, 8, 12] {
            big_endian[at..at + 4].reverse();
        }
        theirs.write_all(&big_endian).unwrap();

        let mut fields = Vec::new();
        pad_to(&mut fields, 8);
        fields.extend_from_slice(&[9, 1, b'(', 0]);
        theirs
            .write_all(&encode_message(MSG_METHOD_CALL, 0, 2, &fields, &[]))
            .unwrap();

        let fine = Message {
            serial: 3,
            ..Message::method_call("x.y", "/b", "x.y", "Fine")
        };
        theirs.write_all(&fine.encode().unwrap()).unwrap();
        assert_eq!(inbox.next().unwrap().member.as_deref(), Some("Fine"));
    }

    /// The service's two halves over a socket pair: what the outbox sends the
    /// inbox reads, a message at a time, however the bytes were split.
    #[test]
    fn the_inbox_reads_whole_messages_however_they_arrive() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (mut inbox, outbox) = Bus::on_socket(ours).into_service().unwrap();
        let (_, mut peer) = Bus::on_socket(theirs).into_service().unwrap();

        let first = Message {
            sender: Some(":1.1".into()),
            ..Message::method_call("x.y", "/a", "x.y", "One")
        };
        let second = Message::method_call("x.y", "/b", "x.y", "Two")
            .with_args("as", &[Value::Array(vec![s("z")])])
            .unwrap();
        let one = peer.send(first).unwrap();
        let two = peer.send(second).unwrap();
        assert_ne!(one, two, "each message gets its own serial");

        let got = inbox.next().unwrap();
        assert_eq!(got.member.as_deref(), Some("One"));
        assert_eq!(got.serial, one);
        let got = inbox.next().unwrap();
        assert_eq!(got.args().unwrap(), vec![Value::Array(vec![s("z")])]);
        assert_eq!(got.serial, two);

        // One message written in two halves still comes out as one.
        let bytes = Message {
            serial: 77,
            ..Message::method_call("x.y", "/c", "x.y", "Three")
        }
        .encode()
        .unwrap();
        let (left, right) = bytes.split_at(bytes.len() / 2);
        peer.sock.write_all(left).unwrap();
        let writer = {
            let mut sock = peer.sock.try_clone().unwrap();
            let right = right.to_vec();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                sock.write_all(&right).unwrap();
            })
        };
        assert_eq!(inbox.next().unwrap().member.as_deref(), Some("Three"));
        writer.join().unwrap();

        // And a connection closed at the other end is an error, not a hang.
        drop(peer);
        drop(outbox);
        assert!(inbox.next().is_err());
    }
}
