//! Just enough D-Bus to ask udisks2 about the disks.
//!
//! Ported from delightviewer's `dbus.rs`, which talks to the desktop portal on
//! the *session* bus; this one talks to udisks2 on the **system** bus. The wire
//! format is the same one implemented twice for the same reason it was
//! implemented once: `zbus` is a large tree with an async runtime in it, bought
//! here for four method calls and one signature.
//!
//! What is actually needed is: connect to a unix socket, do the one-line SASL
//! handshake, marshal a call, and read messages until the reply arrives. That is
//! this file. It implements the subset of the wire format those messages use and
//! nothing else; anything unexpected is an error, not a best guess.
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
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Little-endian, which is what every machine this runs on is.
const LE: u8 = b'l';

const MSG_METHOD_CALL: u8 = 1;
const MSG_METHOD_RETURN: u8 = 2;
const MSG_ERROR: u8 = 3;
/// A broadcast. This client subscribes to none, but udisks2 sends them anyway
/// and [`Bus::call`] has to read past them to find its reply — so the constant
/// exists to *name* what is being skipped, and the tests check one parses.
#[cfg_attr(not(test), allow(dead_code))]
const MSG_SIGNAL: u8 = 4;

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

/// A value read off the wire, whatever its type.
///
/// D-Bus is statically typed and this client mostly knows what it is asking
/// for — except for `GetManagedObjects`, whose whole answer is "here are the
/// types you did not know about". So there is one enum wide enough for anything
/// udisks2 puts in a property, and the accessors below are how a caller says
/// what it expected.
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
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Path(s) | Value::Signature(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Any unsigned integer, widened. udisks2 reports sizes as `t` and a few
    /// things as `u`, and no caller here cares which.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::U64(n) => Some(*n),
            Value::U32(n) => Some(*n as u64),
            Value::U16(n) => Some(*n as u64),
            Value::U8(n) => Some(*n as u64),
            Value::I64(n) if *n >= 0 => Some(*n as u64),
            Value::I32(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        }
    }

    /// A `ay` — udisks2's spelling for a device node or a mount point, which
    /// are byte arrays because a unix path is bytes and not text.
    ///
    /// The trailing NUL udisks2 includes is dropped, and the bytes are decoded
    /// lossily: a mount point with a non-UTF-8 name is still worth showing, and
    /// showing it wrong is better than not listing the disk.
    pub fn as_bytestring(&self) -> Option<String> {
        let Value::Array(items) = self else {
            return None;
        };
        let mut bytes: Vec<u8> = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Value::U8(b) => bytes.push(*b),
                _ => return None,
            }
        }
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// An `aay` — a list of byte strings, which is how mount points arrive.
    pub fn as_bytestrings(&self) -> Option<Vec<String>> {
        let Value::Array(items) = self else {
            return None;
        };
        items.iter().map(Value::as_bytestring).collect()
    }
}

/// One message off the wire.
#[derive(Debug, Default, Clone)]
pub struct Message {
    pub kind: u8,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub sender: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    /// This message's own serial. A client never needs it — nothing here
    /// answers a call — but a message without it is not a message, and reading
    /// it is what proves the fixed header was understood.
    #[cfg_attr(not(test), allow(dead_code))]
    pub serial: u32,
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
        "ServiceUnknown" | "NameHasNoOwner" => {
            "udisks2 is not running on this machine".to_string()
        }
        _ if !text.is_empty() => text.to_string(),
        _ if !short.is_empty() => short.to_string(),
        _ => "the call failed".to_string(),
    }
}

/// A connection to the system bus.
pub struct Bus {
    sock: UnixStream,
    serial: u32,
    /// Anything read past the end of one message, kept for the next.
    buf: Vec<u8>,
}

impl Bus {
    /// Connect, authenticate, and say Hello.
    pub fn connect() -> Result<Bus, String> {
        let path = match std::env::var("DBUS_SYSTEM_BUS_ADDRESS") {
            Ok(address) => socket_path(&address)?,
            Err(_) => SYSTEM_BUS.to_string(),
        };
        let sock = UnixStream::connect(&path)
            .map_err(|e| format!("connecting to the system bus at {path}: {e}"))?;
        sock.set_read_timeout(Some(CALL_TIMEOUT)).ok();
        sock.set_write_timeout(Some(Duration::from_secs(10))).ok();
        let mut bus = Bus {
            sock,
            serial: 0,
            buf: Vec::new(),
        };
        bus.authenticate()?;
        bus.hello()?;
        Ok(bus)
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
            return Err(format!("the system bus refused EXTERNAL auth: {line}"));
        }
        self.sock
            .write_all(b"BEGIN\r\n")
            .map_err(|e| format!("bus auth: {e}"))?;
        Ok(())
    }

    fn read_line(&mut self) -> Result<String, String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = self
                .sock
                .read(&mut byte)
                .map_err(|e| format!("bus auth: {e}"))?;
            if n == 0 {
                return Err("the system bus closed the connection during auth".into());
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
    /// consumer to hand them to.
    pub fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        signature: Option<&str>,
        body: &[u8],
    ) -> Result<Vec<u8>, String> {
        let serial = self.send_call(destination, path, interface, member, signature, body)?;
        loop {
            let msg = self.read_message()?;
            if msg.reply_serial != Some(serial) {
                continue;
            }
            return match msg.kind {
                MSG_METHOD_RETURN => Ok(msg.body),
                MSG_ERROR => Err(msg.error_text()),
                _ => Err("the bus answered with something that is not a reply".into()),
            };
        }
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
        self.serial += 1;
        let serial = self.serial;
        let mut fields = Vec::new();
        push_field(&mut fields, F_PATH, 'o', path);
        push_field(&mut fields, F_DESTINATION, 's', destination);
        push_field(&mut fields, F_INTERFACE, 's', interface);
        push_field(&mut fields, F_MEMBER, 's', member);
        if let Some(sig) = signature {
            push_signature_field(&mut fields, sig);
        }
        let out = encode_message(MSG_METHOD_CALL, 0, serial, &fields, body);
        self.sock
            .write_all(&out)
            .map_err(|e| format!("writing to the system bus: {e}"))?;
        Ok(serial)
    }

    /// Read exactly one message off the wire.
    pub fn read_message(&mut self) -> Result<Message, String> {
        self.fill(16)?;
        let total = frame_len(&self.buf)?;
        self.fill(total)?;
        let msg = parse_message(&self.buf[..total])?;
        self.buf.drain(..total);
        Ok(msg)
    }

    fn fill(&mut self, want: usize) -> Result<(), String> {
        let mut chunk = [0u8; 8192];
        while self.buf.len() < want {
            let n = self
                .sock
                .read(&mut chunk)
                .map_err(|e| format!("reading from the system bus: {e}"))?;
            if n == 0 {
                return Err("the system bus closed the connection".into());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        Ok(())
    }
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

/// The socket out of a bus address. Only `unix:path=` is handled — an abstract
/// socket would need a raw `sockaddr_un`, i.e. `unsafe`, and no system bus uses
/// one.
pub fn socket_path(address: &str) -> Result<String, String> {
    for part in address.split(';') {
        let Some(rest) = part.strip_prefix("unix:") else {
            continue;
        };
        for field in rest.split(',') {
            if let Some(path) = field.strip_prefix("path=") {
                return Ok(path.to_string());
            }
        }
    }
    Err(format!("cannot use bus address `{address}`"))
}

// ── Framing ─────────────────────────────────────────────────────────────────

/// How many bytes the message starting at `head` occupies, from its first 16.
pub fn frame_len(head: &[u8]) -> Result<usize, String> {
    if head.first() != Some(&LE) {
        return Err("big-endian message from the bus (unsupported)".into());
    }
    let body_len = u32_at(head, 4)? as usize;
    let fields_len = u32_at(head, 12)? as usize;
    let total = (16 + fields_len).next_multiple_of(8) + body_len;
    if total > MAX_MESSAGE {
        return Err("absurd message length from the bus".into());
    }
    Ok(total)
}

/// One complete message, understood. Split from the framing so a hand-built
/// message can be checked without a socket.
pub fn parse_message(bytes: &[u8]) -> Result<Message, String> {
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
            // Everything else is read past rather than understood — a field
            // this code does not use must not fail a message it could parse.
            (_, sig) => r.skip(sig)?,
        }
    }
    Ok(())
}

// ── Marshalling ─────────────────────────────────────────────────────────────

/// The fixed 16-byte header, the field array, and the body on its own 8-byte
/// boundary.
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

fn push_field(out: &mut Vec<u8>, code: u8, sig: char, value: &str) {
    pad_to(out, 8);
    out.push(code);
    marshal_signature(out, &sig.to_string());
    marshal_string(out, value);
}

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
        let s = String::from_utf8_lossy(&self.bytes[self.pos..self.pos + len]).into_owned();
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
        let mut chars = sig.chars().peekable();
        let one = take_one_type(&mut chars);
        if one.is_empty() {
            return Err("empty D-Bus signature".into());
        }
        self.one(&one)
    }

    fn one(&mut self, sig: &str) -> Result<Value, String> {
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
                self.value(&inner)
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
                    if dict {
                        self.align(8);
                        if self.pos >= end {
                            break;
                        }
                        let inner = &element[1..element.len() - 1];
                        let mut chars = inner.chars().peekable();
                        let key_sig = take_one_type(&mut chars);
                        let value_sig: String = chars.collect();
                        let key = self.one(&key_sig)?;
                        let value = self.one(&value_sig)?;
                        pairs.push((key, value));
                    } else {
                        items.push(self.one(&element)?);
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
                let inner = &sig[1..sig.len().saturating_sub(1)];
                let mut chars = inner.chars().peekable();
                let mut fields = Vec::new();
                loop {
                    let one = take_one_type(&mut chars);
                    if one.is_empty() {
                        break;
                    }
                    fields.push(self.one(&one)?);
                }
                Ok(Value::Struct(fields))
            }
            other => Err(format!("unhandled D-Bus type '{other}'")),
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
            let props = properties
                .into_iter()
                .filter_map(|(key, value)| Some((key.as_str()?.to_string(), value)))
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
    fn marshal(out: &mut Vec<u8>, value: &Value) {
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
        marshal(out, value);
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
        push_field(&mut fields, F_INTERFACE, 's', "org.freedesktop.DBus.ObjectManager");
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
            ("o", Value::Path("/org/freedesktop/UDisks2/block_devices/sda1".into())),
        ];
        for (sig, value) in cases {
            let mut body = Vec::new();
            marshal(&mut body, &value);
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
                ("/org/freedesktop/UDisks2/block_devices/sda1", "USB", 8_000_000_000u64, true),
                ("/org/freedesktop/UDisks2/block_devices/sdb1", "Backup", 16u64, false),
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
        assert_eq!(block["Device"].as_bytestring().as_deref(), Some("/dev/sda1"));
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
        assert_eq!(Reader::new(&body).value("a{sv}").unwrap(), Value::Dict(vec![]));
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
        assert_eq!(
            socket_path("unix:path=/run/dbus/system_bus_socket").unwrap(),
            "/run/dbus/system_bus_socket"
        );
        assert_eq!(
            socket_path("unix:guid=abc,path=/tmp/bus").unwrap(),
            "/tmp/bus"
        );
        assert!(socket_path("tcp:host=localhost,port=1").is_err());
    }

    /// The three refusals a file manager actually meets, each turned into a
    /// sentence with a next step in it.
    #[test]
    fn errors_become_sentences() {
        assert!(
            readable_error("org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain", "")
                .contains("not allowed")
        );
        assert!(readable_error("org.freedesktop.UDisks2.Error.DeviceBusy", "")
            .contains("still has a file open"));
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
            pairs[1].1,
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
}
