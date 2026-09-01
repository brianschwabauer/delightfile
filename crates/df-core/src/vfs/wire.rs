//! The SFTP wire format, version 3, as a pure codec.
//!
//! Nothing in this file does I/O. It turns bytes into messages and messages
//! into bytes, and every function in it is a pure one — which is the entire
//! reason the protocol lives in its own module. A hand-rolled wire protocol is
//! only trustworthy if the parsing half can be hammered by tests without a
//! server, a socket or a network, and that is what `super::tests` does: every
//! message type goes out through the encoder and comes back through the decoder
//! and has to be the same thing on the other side.
//!
//! ## Which SFTP
//!
//! SFTP is not one protocol. The IETF drafts run from `-00` to `-13` and they
//! disagree about attributes, about filenames, about text mode. What is
//! *deployed* is **version 3** — `draft-ietf-secsh-filexfer-02` — because that
//! is what OpenSSH's `sftp-server` implements and OpenSSH is what is running on
//! the other end of every host in `vfs.toml`. So this speaks 3, offers 3 in the
//! handshake, and refuses anything else rather than guessing.
//!
//! ## The frame
//!
//! ```text
//!   uint32  length          (of everything after this field)
//!   byte    type
//!   uint32  request-id      (every message except INIT and VERSION)
//!   ...     type-specific payload
//! ```
//!
//! Strings are `uint32 length` + that many bytes, and they are **bytes**, not
//! text: a v3 filename has no declared encoding. They are carried as `Vec<u8>`
//! on the wire and only lossily converted to a `String` at the point a name is
//! shown to a person, so a file whose name is not UTF-8 lists instead of
//! failing the whole directory.
//!
//! ## Two deliberate incompatibilities with the draft
//!
//! 1. **`SSH_FXP_SYMLINK` has its arguments backwards.** The draft says
//!    `linkpath` then `targetpath`. OpenSSH's server reads `oldpath` (the
//!    target) then `newpath` (the link) — an ancient bug that is now the de
//!    facto protocol, because both OpenSSH's client and its server have agreed
//!    on it for two decades. [`crate::vfs::wire::Request::Symlink`] emits OpenSSH's order. Sending the
//!    draft's order to `sftp-server` silently creates the symlink the wrong way
//!    round, which is worse than an error.
//! 2. **`SSH_FXP_STATUS` may stop after the code.** The draft requires a
//!    message and a language tag; several servers (and older OpenSSH) send
//!    neither. A missing tail is treated as an empty message rather than as a
//!    truncated packet, because refusing a status is refusing to hear "no".
//!
//! ## Bounds
//!
//! Every length field on the wire is attacker-controlled as far as this code is
//! concerned — a compromised or merely broken server should not be able to make
//! delightfile allocate a gigabyte or read past a buffer. So: a packet longer
//! than [`MAX_PACKET`] is refused before a byte of it is read, a string longer
//! than its enclosing packet is refused, and every decoder returns
//! [`ProtocolError`] rather than panicking. There is no `unwrap` and no slice
//! index in this file that is not bounds-checked first.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Message types ───────────────────────────────────────────────────────────

pub const FXP_INIT: u8 = 1;
pub const FXP_VERSION: u8 = 2;
pub const FXP_OPEN: u8 = 3;
pub const FXP_CLOSE: u8 = 4;
pub const FXP_READ: u8 = 5;
pub const FXP_WRITE: u8 = 6;
pub const FXP_LSTAT: u8 = 7;
pub const FXP_FSTAT: u8 = 8;
pub const FXP_SETSTAT: u8 = 9;
pub const FXP_FSETSTAT: u8 = 10;
pub const FXP_OPENDIR: u8 = 11;
pub const FXP_READDIR: u8 = 12;
pub const FXP_REMOVE: u8 = 13;
pub const FXP_MKDIR: u8 = 14;
pub const FXP_RMDIR: u8 = 15;
pub const FXP_REALPATH: u8 = 16;
pub const FXP_STAT: u8 = 17;
pub const FXP_RENAME: u8 = 18;
pub const FXP_READLINK: u8 = 19;
pub const FXP_SYMLINK: u8 = 20;

pub const FXP_STATUS: u8 = 101;
pub const FXP_HANDLE: u8 = 102;
pub const FXP_DATA: u8 = 103;
pub const FXP_NAME: u8 = 104;
pub const FXP_ATTRS: u8 = 105;

/// The only version this client speaks. See the module header for why 3 is not
/// a conservative choice but the *correct* one.
pub const SFTP_VERSION: u32 = 3;

/// The largest packet this client will send or accept, in bytes.
///
/// 256 KiB, which is OpenSSH `sftp-server`'s own ceiling
/// (`SFTP_MAX_MSG_LENGTH`), so agreeing with it costs nothing and disagreeing
/// with it could only ever mean accepting something the server would not.
/// Sized against the largest legitimate packet: a `READ` reply of
/// [`super::conn::READ_CHUNK`] bytes plus its header, with two orders of
/// magnitude of headroom for a `NAME` reply carrying a few hundred long
/// filenames.
///
/// It is checked against the length field *before* the body is read, so a
/// server that claims 4 GB costs one refused packet and not one allocation.
pub const MAX_PACKET: usize = 256 * 1024;

// ── Open flags (`pflags`) ───────────────────────────────────────────────────

pub const FXF_READ: u32 = 0x0000_0001;
pub const FXF_WRITE: u32 = 0x0000_0002;
pub const FXF_APPEND: u32 = 0x0000_0004;
pub const FXF_CREAT: u32 = 0x0000_0008;
pub const FXF_TRUNC: u32 = 0x0000_0010;
pub const FXF_EXCL: u32 = 0x0000_0020;

// ── Attribute flags ─────────────────────────────────────────────────────────

pub const ATTR_SIZE: u32 = 0x0000_0001;
pub const ATTR_UIDGID: u32 = 0x0000_0002;
pub const ATTR_PERMISSIONS: u32 = 0x0000_0004;
pub const ATTR_ACMODTIME: u32 = 0x0000_0008;
pub const ATTR_EXTENDED: u32 = 0x8000_0000;

/// How many `string type, string data` pairs an `ATTRS` extension block may
/// carry before the packet is called nonsense.
///
/// Nothing delightfile talks to sends any, so any value above zero is already
/// generous; 64 is small enough that the count field cannot be turned into an
/// allocation and large enough that a server with opinions still parses.
pub const MAX_ATTR_EXTENSIONS: u32 = 64;

/// The `st_mode` type mask and the three types a listing distinguishes.
/// Spelled out rather than taken from `libc` because these are *wire* values —
/// they mean the same thing whatever the client is running on, and reading them
/// out of the local `libc` would quietly make the protocol platform-dependent.
pub const S_IFMT: u32 = 0o170_000;
pub const S_IFDIR: u32 = 0o040_000;
pub const S_IFLNK: u32 = 0o120_000;
pub const S_IFREG: u32 = 0o100_000;

// ── Errors ──────────────────────────────────────────────────────────────────

/// The server said something that is not SFTP.
///
/// Every variant is a reason to hang up: once a stream has desynchronized there
/// is no way to resynchronize it, because the framing is the only structure
/// there is. The connection layer treats any of these as "kill the child and
/// reconnect on the next request" — never as something to retry in place, and
/// never as a panic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// The packet ended in the middle of a field.
    #[error("truncated packet: needed {want} more bytes, {have} remained")]
    Truncated { want: usize, have: usize },

    /// The length prefix was bigger than [`MAX_PACKET`].
    #[error("packet of {0} bytes is over the {MAX_PACKET}-byte limit")]
    TooLarge(usize),

    /// A length prefix of zero. Every packet has at least a type byte.
    #[error("zero-length packet")]
    Empty,

    /// A string's own length ran past the end of the packet holding it.
    #[error("string of {want} bytes does not fit in the {have} bytes left")]
    StringOverrun { want: usize, have: usize },

    /// A reply of a type that cannot answer the request that was sent.
    #[error("unexpected packet type {kind} (expected {expected})")]
    UnexpectedType { kind: u8, expected: &'static str },

    /// A reply carrying somebody else's request id — the one failure mode that
    /// a pipelined client has and a lock-step one does not, and the reason
    /// every response is matched rather than merely counted.
    #[error("reply for request {got}, expected {expected}")]
    IdMismatch { expected: u32, got: u32 },

    /// The server does not speak version 3.
    #[error("server speaks SFTP version {0}; this client speaks 3")]
    Version(u32),

    /// A count field that would mean an implausible allocation.
    #[error("{what} count of {count} is not plausible")]
    ImplausibleCount { what: &'static str, count: u32 },

    /// Bytes left over after a packet decoded completely. Harmless in
    /// principle; a symptom of a decoder/encoder disagreement in practice, so
    /// it is reported rather than ignored.
    #[error("{0} bytes of trailing junk after a complete packet")]
    Trailing(usize),
}

/// A `SSH_FXP_STATUS` code.
///
/// `Other` exists because a server is allowed to invent codes and an unknown
/// number must still produce a sentence a person can read, not a parse failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    Eof,
    NoSuchFile,
    PermissionDenied,
    Failure,
    BadMessage,
    NoConnection,
    ConnectionLost,
    OpUnsupported,
    Other(u32),
}

impl StatusCode {
    pub fn from_u32(code: u32) -> StatusCode {
        match code {
            0 => StatusCode::Ok,
            1 => StatusCode::Eof,
            2 => StatusCode::NoSuchFile,
            3 => StatusCode::PermissionDenied,
            4 => StatusCode::Failure,
            5 => StatusCode::BadMessage,
            6 => StatusCode::NoConnection,
            7 => StatusCode::ConnectionLost,
            8 => StatusCode::OpUnsupported,
            other => StatusCode::Other(other),
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            StatusCode::Ok => 0,
            StatusCode::Eof => 1,
            StatusCode::NoSuchFile => 2,
            StatusCode::PermissionDenied => 3,
            StatusCode::Failure => 4,
            StatusCode::BadMessage => 5,
            StatusCode::NoConnection => 6,
            StatusCode::ConnectionLost => 7,
            StatusCode::OpUnsupported => 8,
            StatusCode::Other(code) => code,
        }
    }

    /// The sentence that ends up in a toast.
    ///
    /// Written the way a person would say it, because these are the words the
    /// user reads when a remote operation fails — "SSH_FX_NO_SUCH_FILE" is a
    /// constant name, not an explanation.
    pub fn describe(self) -> &'static str {
        match self {
            StatusCode::Ok => "succeeded",
            StatusCode::Eof => "end of file",
            StatusCode::NoSuchFile => "no such file or directory",
            StatusCode::PermissionDenied => "permission denied",
            StatusCode::Failure => "the server refused the operation",
            StatusCode::BadMessage => "the server could not understand the request",
            StatusCode::NoConnection => "not connected",
            StatusCode::ConnectionLost => "the connection was lost",
            StatusCode::OpUnsupported => "the server does not support that operation",
            StatusCode::Other(_) => "the server reported an error",
        }
    }

    /// Whether this is the not-a-failure one. `Eof` ends a `READDIR` loop and
    /// ends a `READ` at the end of a file; treating it as an error would make
    /// every successful listing fail on its last packet.
    pub fn is_eof(self) -> bool {
        matches!(self, StatusCode::Eof)
    }
}

/// A decoded `SSH_FXP_STATUS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub code: StatusCode,
    /// The server's own words, when it sent any. Preferred over
    /// [`StatusCode::describe`] in messages, because "Permission denied" from
    /// the server names *its* reason and the code only names the category.
    pub message: String,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.trim().is_empty() {
            f.write_str(self.code.describe())
        } else {
            f.write_str(self.message.trim())
        }
    }
}

// ── Attributes ──────────────────────────────────────────────────────────────

/// A v3 `ATTRS` block: everything the server chose to tell us, and nothing it
/// did not.
///
/// Every field is an `Option` because the flags word says which are present,
/// and a missing field is genuinely missing — a directory listing off a server
/// that does not report uid/gid must show a blank owner, not user 0. Collapsing
/// "absent" into "zero" is how a listing ends up claiming every file belongs to
/// root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attrs {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// The full `st_mode` — type bits and permission bits together, the same
    /// number [`crate::fs::Entry::mode`] holds.
    pub permissions: Option<u32>,
    /// Seconds since the epoch. v3 has no sub-second resolution and no 2038
    /// escape hatch; both are the protocol's, not this code's.
    pub atime: Option<u32>,
    pub mtime: Option<u32>,
}

impl Attrs {
    /// The empty attribute block, which is what a request that is not trying to
    /// set anything sends (`OPEN` for reading, `MKDIR` with default modes).
    pub fn empty() -> Attrs {
        Attrs::default()
    }

    /// Just a permission word — the `SETSTAT` that is `chmod`.
    pub fn permissions(mode: u32) -> Attrs {
        Attrs {
            permissions: Some(mode),
            ..Attrs::default()
        }
    }

    pub fn flags(&self) -> u32 {
        let mut flags = 0;
        if self.size.is_some() {
            flags |= ATTR_SIZE;
        }
        // v3 packs uid and gid into one flag, so an `Attrs` with only one of
        // them set has to send both; the missing half goes out as 0, which is
        // the only thing the encoding permits.
        if self.uid.is_some() || self.gid.is_some() {
            flags |= ATTR_UIDGID;
        }
        if self.permissions.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        if self.atime.is_some() || self.mtime.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        flags
    }

    pub fn encode(&self, out: &mut Encoder) {
        out.put_u32(self.flags());
        if let Some(size) = self.size {
            out.put_u64(size);
        }
        if self.uid.is_some() || self.gid.is_some() {
            out.put_u32(self.uid.unwrap_or(0));
            out.put_u32(self.gid.unwrap_or(0));
        }
        if let Some(permissions) = self.permissions {
            out.put_u32(permissions);
        }
        if self.atime.is_some() || self.mtime.is_some() {
            out.put_u32(self.atime.unwrap_or(0));
            out.put_u32(self.mtime.unwrap_or(0));
        }
    }

    pub fn decode(input: &mut Decoder<'_>) -> Result<Attrs, ProtocolError> {
        let flags = input.get_u32()?;
        let mut attrs = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            attrs.size = Some(input.get_u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            attrs.uid = Some(input.get_u32()?);
            attrs.gid = Some(input.get_u32()?);
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attrs.permissions = Some(input.get_u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            attrs.atime = Some(input.get_u32()?);
            attrs.mtime = Some(input.get_u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            let count = input.get_u32()?;
            if count > MAX_ATTR_EXTENSIONS {
                return Err(ProtocolError::ImplausibleCount {
                    what: "attribute extension",
                    count,
                });
            }
            // Read and discard: nothing delightfile does depends on a vendor
            // extension, but the bytes still have to be stepped over or every
            // field after them is misread.
            for _ in 0..count {
                input.get_bytes()?;
                input.get_bytes()?;
            }
        }
        Ok(attrs)
    }

    /// The `S_IFMT` half of the mode, when the server sent one.
    pub fn file_type(&self) -> Option<u32> {
        self.permissions.map(|mode| mode & S_IFMT)
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == Some(S_IFDIR)
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == Some(S_IFLNK)
    }

    pub fn is_file(&self) -> bool {
        self.file_type() == Some(S_IFREG)
    }

    /// `mtime` as a [`SystemTime`], for [`crate::fs::Entry`].
    pub fn mtime_system(&self) -> Option<SystemTime> {
        self.mtime
            .map(|secs| UNIX_EPOCH + Duration::from_secs(u64::from(secs)))
    }
}

/// One entry of a `SSH_FXP_NAME` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    /// The bare filename in a `READDIR` reply; a full path in a `REALPATH` or
    /// `READLINK` one. Bytes, because v3 does not say what encoding it is.
    pub filename: Vec<u8>,
    /// The `ls -l` line the server composed. Kept because it is the only place
    /// a *name* for the owner and group appears — the attrs carry numeric ids
    /// and a remote uid means nothing in the local password file.
    pub longname: Vec<u8>,
    pub attrs: Attrs,
}

impl NameEntry {
    /// The filename as text, with invalid bytes replaced. Lossy on purpose: a
    /// name that is not UTF-8 must still appear in the listing.
    pub fn name_lossy(&self) -> String {
        String::from_utf8_lossy(&self.filename).into_owned()
    }

    /// The type character `ls -l` puts first (`d`, `-`, `l`, …), when the
    /// longname looks like an `ls -l` line at all.
    ///
    /// The fallback for a server that sends a `NAME` reply with no permission
    /// bits in the attrs: the longname is not structured data and parsing it is
    /// a last resort, but one character at a known offset is a cheap one.
    pub fn longname_type(&self) -> Option<char> {
        let first = *self.longname.first()? as char;
        matches!(first, 'd' | '-' | 'l' | 'b' | 'c' | 'p' | 's').then_some(first)
    }

    /// The owner and group names out of the longname's third and fourth
    /// whitespace-separated columns, when they are there.
    pub fn longname_owner(&self) -> Option<(String, String)> {
        let text = String::from_utf8_lossy(&self.longname);
        let mut columns = text.split_whitespace();
        let (_perms, _links) = (columns.next()?, columns.next()?);
        let user = columns.next()?.to_string();
        let group = columns.next()?.to_string();
        Some((user, group))
    }
}

// ── Encoder ─────────────────────────────────────────────────────────────────

/// Builds one packet, length prefix and all.
///
/// The length is written last, by [`Encoder::finish`], from the actual byte
/// count — so it cannot disagree with the body. Every other design (compute the
/// length up front from the fields you are about to write) has the same bug
/// waiting in it: add a field, forget the arithmetic, desynchronize the stream.
#[derive(Debug, Default)]
pub struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// Start a packet of `kind` carrying `id`.
    pub fn request(kind: u8, id: u32) -> Encoder {
        let mut e = Encoder::raw(kind);
        e.put_u32(id);
        e
    }

    /// Start a packet of `kind` with no request id — `INIT` and `VERSION`, the
    /// only two.
    pub fn raw(kind: u8) -> Encoder {
        // Four zero bytes reserved for the length, filled in by `finish`.
        let mut bytes = vec![0, 0, 0, 0];
        bytes.push(kind);
        Encoder { bytes }
    }

    pub fn put_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn put_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn put_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// A `uint32`-prefixed byte string.
    pub fn put_bytes(&mut self, value: &[u8]) {
        // A string longer than `u32::MAX` cannot be expressed; nothing in this
        // crate produces one (paths are bounded by `PATH_MAX`, writes by
        // `READ_CHUNK`), and truncating is the only lossless-looking option
        // that is actually a corruption. So it saturates and the packet-length
        // check in `finish` refuses the result.
        let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
        self.put_u32(len);
        self.bytes.extend_from_slice(value);
    }

    pub fn put_str(&mut self, value: &str) {
        self.put_bytes(value.as_bytes());
    }

    /// Fill in the length prefix and hand over the finished packet, or refuse
    /// it for being over [`MAX_PACKET`].
    pub fn finish(self) -> Result<Vec<u8>, ProtocolError> {
        let body = self.bytes.len() - 4;
        if body > MAX_PACKET {
            return Err(ProtocolError::TooLarge(body));
        }
        let mut bytes = self.bytes;
        let prefix = (body as u32).to_be_bytes();
        bytes[..4].copy_from_slice(&prefix);
        Ok(bytes)
    }
}

// ── Decoder ─────────────────────────────────────────────────────────────────

/// Reads fields out of a packet body, refusing to run off the end.
///
/// Holds a cursor rather than reslicing so that a decode failure can say how
/// much was left, which is the difference between a bug report and a shrug.
#[derive(Debug)]
pub struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Decoder<'a> {
        Decoder { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.remaining() < n {
            return Err(ProtocolError::Truncated {
                want: n,
                have: self.remaining(),
            });
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn get_u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    pub fn get_u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn get_u64(&mut self) -> Result<u64, ProtocolError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// A `uint32`-prefixed byte string. The length is checked against what is
    /// *actually left*, not against a constant, so a string can never be used
    /// to read another packet's bytes.
    pub fn get_bytes(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.get_u32()? as usize;
        if len > self.remaining() {
            return Err(ProtocolError::StringOverrun {
                want: len,
                have: self.remaining(),
            });
        }
        self.take(len)
    }

    /// A string, lossily as text. For the fields that are words rather than
    /// filenames — status messages, language tags, extension names.
    pub fn get_string_lossy(&mut self) -> Result<String, ProtocolError> {
        Ok(String::from_utf8_lossy(self.get_bytes()?).into_owned())
    }

    /// Everything not yet read.
    pub fn rest(&mut self) -> &'a [u8] {
        let slice = &self.data[self.pos..];
        self.pos = self.data.len();
        slice
    }

    /// Fail if anything is left. Called by the decoders that know exactly how
    /// long their message is.
    pub fn finish(self) -> Result<(), ProtocolError> {
        match self.remaining() {
            0 => Ok(()),
            n => Err(ProtocolError::Trailing(n)),
        }
    }
}

// ── Packets ─────────────────────────────────────────────────────────────────

/// One framed message: the type byte and everything after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub kind: u8,
    pub body: Vec<u8>,
}

impl Packet {
    /// Split a complete framed packet — length prefix included — apart.
    ///
    /// Used by the tests and by anything holding a whole packet already; the
    /// transport reads the prefix separately because it has to know how much to
    /// wait for before it has the rest.
    pub fn parse(frame: &[u8]) -> Result<Packet, ProtocolError> {
        if frame.len() < 4 {
            return Err(ProtocolError::Truncated {
                want: 4,
                have: frame.len(),
            });
        }
        let len = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        if len == 0 {
            return Err(ProtocolError::Empty);
        }
        if len > MAX_PACKET {
            return Err(ProtocolError::TooLarge(len));
        }
        let body = frame.get(4..4 + len).ok_or(ProtocolError::Truncated {
            want: 4 + len,
            have: frame.len(),
        })?;
        Packet::from_body(body)
    }

    /// Build from the bytes *after* the length prefix.
    pub fn from_body(body: &[u8]) -> Result<Packet, ProtocolError> {
        let (kind, rest) = body.split_first().ok_or(ProtocolError::Empty)?;
        Ok(Packet {
            kind: *kind,
            body: rest.to_vec(),
        })
    }

    pub fn decoder(&self) -> Decoder<'_> {
        Decoder::new(&self.body)
    }
}

// ── Requests ────────────────────────────────────────────────────────────────

/// A message this client sends.
///
/// The enum exists so the codec is symmetric: [`Request::encode`] and
/// [`decode_request`] are inverses, and the tests prove it for every variant.
/// A protocol whose encoder has no decoder is a protocol tested by hoping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Init {
        version: u32,
    },
    Open {
        path: Vec<u8>,
        pflags: u32,
        attrs: Attrs,
    },
    Close {
        handle: Vec<u8>,
    },
    Read {
        handle: Vec<u8>,
        offset: u64,
        len: u32,
    },
    Write {
        handle: Vec<u8>,
        offset: u64,
        data: Vec<u8>,
    },
    Stat {
        path: Vec<u8>,
    },
    LStat {
        path: Vec<u8>,
    },
    FStat {
        handle: Vec<u8>,
    },
    SetStat {
        path: Vec<u8>,
        attrs: Attrs,
    },
    OpenDir {
        path: Vec<u8>,
    },
    ReadDir {
        handle: Vec<u8>,
    },
    Remove {
        path: Vec<u8>,
    },
    Mkdir {
        path: Vec<u8>,
        attrs: Attrs,
    },
    Rmdir {
        path: Vec<u8>,
    },
    RealPath {
        path: Vec<u8>,
    },
    Rename {
        old: Vec<u8>,
        new: Vec<u8>,
    },
    ReadLink {
        path: Vec<u8>,
    },
    /// **OpenSSH argument order**: `target` first, then the link to create. See
    /// the module header.
    Symlink {
        target: Vec<u8>,
        link: Vec<u8>,
    },
}

impl Request {
    pub fn kind(&self) -> u8 {
        match self {
            Request::Init { .. } => FXP_INIT,
            Request::Open { .. } => FXP_OPEN,
            Request::Close { .. } => FXP_CLOSE,
            Request::Read { .. } => FXP_READ,
            Request::Write { .. } => FXP_WRITE,
            Request::Stat { .. } => FXP_STAT,
            Request::LStat { .. } => FXP_LSTAT,
            Request::FStat { .. } => FXP_FSTAT,
            Request::SetStat { .. } => FXP_SETSTAT,
            Request::OpenDir { .. } => FXP_OPENDIR,
            Request::ReadDir { .. } => FXP_READDIR,
            Request::Remove { .. } => FXP_REMOVE,
            Request::Mkdir { .. } => FXP_MKDIR,
            Request::Rmdir { .. } => FXP_RMDIR,
            Request::RealPath { .. } => FXP_REALPATH,
            Request::Rename { .. } => FXP_RENAME,
            Request::ReadLink { .. } => FXP_READLINK,
            Request::Symlink { .. } => FXP_SYMLINK,
        }
    }

    /// Frame this request. `id` is ignored for [`Request::Init`], which is the
    /// one message with no id — a fact the encoder enforces rather than trusts
    /// the caller with.
    pub fn encode(&self, id: u32) -> Result<Vec<u8>, ProtocolError> {
        if let Request::Init { version } = self {
            let mut e = Encoder::raw(FXP_INIT);
            e.put_u32(*version);
            return e.finish();
        }
        let mut e = Encoder::request(self.kind(), id);
        match self {
            Request::Init { .. } => unreachable!("handled above"),
            Request::Open {
                path,
                pflags,
                attrs,
            } => {
                e.put_bytes(path);
                e.put_u32(*pflags);
                attrs.encode(&mut e);
            }
            Request::Close { handle } | Request::ReadDir { handle } | Request::FStat { handle } => {
                e.put_bytes(handle)
            }
            Request::Read {
                handle,
                offset,
                len,
            } => {
                e.put_bytes(handle);
                e.put_u64(*offset);
                e.put_u32(*len);
            }
            Request::Write {
                handle,
                offset,
                data,
            } => {
                e.put_bytes(handle);
                e.put_u64(*offset);
                e.put_bytes(data);
            }
            Request::Stat { path }
            | Request::LStat { path }
            | Request::OpenDir { path }
            | Request::Remove { path }
            | Request::Rmdir { path }
            | Request::RealPath { path }
            | Request::ReadLink { path } => e.put_bytes(path),
            Request::SetStat { path, attrs } | Request::Mkdir { path, attrs } => {
                e.put_bytes(path);
                attrs.encode(&mut e);
            }
            Request::Rename { old, new } => {
                e.put_bytes(old);
                e.put_bytes(new);
            }
            Request::Symlink { target, link } => {
                e.put_bytes(target);
                e.put_bytes(link);
            }
        }
        e.finish()
    }
}

/// Decode a request. The inverse of [`Request::encode`]; returns the request id
/// too, which is `0` for `INIT` because `INIT` does not have one.
pub fn decode_request(packet: &Packet) -> Result<(u32, Request), ProtocolError> {
    let mut d = packet.decoder();
    if packet.kind == FXP_INIT {
        let version = d.get_u32()?;
        // The server's extension list follows in a `VERSION`; an `INIT` from a
        // client may also carry extensions, and ignoring them is correct.
        d.rest();
        return Ok((0, Request::Init { version }));
    }
    let id = d.get_u32()?;
    let request = match packet.kind {
        FXP_OPEN => {
            let path = d.get_bytes()?.to_vec();
            let pflags = d.get_u32()?;
            let attrs = Attrs::decode(&mut d)?;
            Request::Open {
                path,
                pflags,
                attrs,
            }
        }
        FXP_CLOSE => Request::Close {
            handle: d.get_bytes()?.to_vec(),
        },
        FXP_READDIR => Request::ReadDir {
            handle: d.get_bytes()?.to_vec(),
        },
        FXP_FSTAT => Request::FStat {
            handle: d.get_bytes()?.to_vec(),
        },
        FXP_READ => {
            let handle = d.get_bytes()?.to_vec();
            let offset = d.get_u64()?;
            let len = d.get_u32()?;
            Request::Read {
                handle,
                offset,
                len,
            }
        }
        FXP_WRITE => {
            let handle = d.get_bytes()?.to_vec();
            let offset = d.get_u64()?;
            let data = d.get_bytes()?.to_vec();
            Request::Write {
                handle,
                offset,
                data,
            }
        }
        FXP_STAT => Request::Stat {
            path: d.get_bytes()?.to_vec(),
        },
        FXP_LSTAT => Request::LStat {
            path: d.get_bytes()?.to_vec(),
        },
        FXP_OPENDIR => Request::OpenDir {
            path: d.get_bytes()?.to_vec(),
        },
        FXP_REMOVE => Request::Remove {
            path: d.get_bytes()?.to_vec(),
        },
        FXP_RMDIR => Request::Rmdir {
            path: d.get_bytes()?.to_vec(),
        },
        FXP_REALPATH => Request::RealPath {
            path: d.get_bytes()?.to_vec(),
        },
        FXP_READLINK => Request::ReadLink {
            path: d.get_bytes()?.to_vec(),
        },
        FXP_SETSTAT => {
            let path = d.get_bytes()?.to_vec();
            let attrs = Attrs::decode(&mut d)?;
            Request::SetStat { path, attrs }
        }
        FXP_MKDIR => {
            let path = d.get_bytes()?.to_vec();
            let attrs = Attrs::decode(&mut d)?;
            Request::Mkdir { path, attrs }
        }
        FXP_RENAME => {
            let old = d.get_bytes()?.to_vec();
            let new = d.get_bytes()?.to_vec();
            Request::Rename { old, new }
        }
        FXP_SYMLINK => {
            let target = d.get_bytes()?.to_vec();
            let link = d.get_bytes()?.to_vec();
            Request::Symlink { target, link }
        }
        kind => {
            return Err(ProtocolError::UnexpectedType {
                kind,
                expected: "a request",
            })
        }
    };
    d.finish()?;
    Ok((id, request))
}

// ── Replies ─────────────────────────────────────────────────────────────────

/// A message the server sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Version {
        version: u32,
        extensions: Vec<(String, String)>,
    },
    Status(Status),
    Handle(Vec<u8>),
    Data(Vec<u8>),
    Name(Vec<NameEntry>),
    Attrs(Attrs),
}

impl Reply {
    /// A one-word name for the "expected X, got Y" message.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Reply::Version { .. } => "VERSION",
            Reply::Status(_) => "STATUS",
            Reply::Handle(_) => "HANDLE",
            Reply::Data(_) => "DATA",
            Reply::Name(_) => "NAME",
            Reply::Attrs(_) => "ATTRS",
        }
    }

    pub fn encode(&self, id: u32) -> Result<Vec<u8>, ProtocolError> {
        match self {
            Reply::Version {
                version,
                extensions,
            } => {
                let mut e = Encoder::raw(FXP_VERSION);
                e.put_u32(*version);
                for (name, data) in extensions {
                    e.put_str(name);
                    e.put_str(data);
                }
                e.finish()
            }
            Reply::Status(status) => {
                let mut e = Encoder::request(FXP_STATUS, id);
                e.put_u32(status.code.as_u32());
                e.put_str(&status.message);
                // The language tag. Always empty: it exists in the draft and
                // means nothing to anyone.
                e.put_str("");
                e.finish()
            }
            Reply::Handle(handle) => {
                let mut e = Encoder::request(FXP_HANDLE, id);
                e.put_bytes(handle);
                e.finish()
            }
            Reply::Data(data) => {
                let mut e = Encoder::request(FXP_DATA, id);
                e.put_bytes(data);
                e.finish()
            }
            Reply::Name(entries) => {
                let mut e = Encoder::request(FXP_NAME, id);
                e.put_u32(u32::try_from(entries.len()).unwrap_or(u32::MAX));
                for entry in entries {
                    e.put_bytes(&entry.filename);
                    e.put_bytes(&entry.longname);
                    entry.attrs.encode(&mut e);
                }
                e.finish()
            }
            Reply::Attrs(attrs) => {
                let mut e = Encoder::request(FXP_ATTRS, id);
                attrs.encode(&mut e);
                e.finish()
            }
        }
    }
}

/// How many entries one `SSH_FXP_NAME` may claim.
///
/// OpenSSH sends at most 100 per `READDIR`; a server is free to send more, and
/// a *directory* is free to be enormous — but one packet is capped at
/// [`MAX_PACKET`] and an entry cannot be smaller than about a dozen bytes, so
/// anything past this is a count field lying about a packet that could not
/// possibly contain it. Checking it before the loop turns a hostile 4-billion
/// count into one refused packet instead of one enormous `Vec::with_capacity`.
pub const MAX_NAME_ENTRIES: u32 = (MAX_PACKET / 12) as u32;

/// Decode a reply, returning the request id it answers.
///
/// `VERSION` has no id and reports `0`; nothing waits on id 0 because
/// `super::conn` starts its counter at 1.
pub fn decode_reply(packet: &Packet) -> Result<(u32, Reply), ProtocolError> {
    let mut d = packet.decoder();
    match packet.kind {
        FXP_VERSION => {
            let version = d.get_u32()?;
            let mut extensions = Vec::new();
            // Extensions run to the end of the packet with no count. A pair
            // that does not decode ends the list rather than failing the
            // handshake: the version number is the part that matters, and no
            // extension this client cares about exists.
            while !d.is_empty() {
                let Ok(name) = d.get_string_lossy() else {
                    break;
                };
                let Ok(data) = d.get_string_lossy() else {
                    break;
                };
                extensions.push((name, data));
            }
            Ok((
                0,
                Reply::Version {
                    version,
                    extensions,
                },
            ))
        }
        FXP_STATUS => {
            let id = d.get_u32()?;
            let code = StatusCode::from_u32(d.get_u32()?);
            // Message and language tag are optional in practice — see the
            // module header's second incompatibility.
            let message = d.get_string_lossy().unwrap_or_default();
            Ok((id, Reply::Status(Status { code, message })))
        }
        FXP_HANDLE => {
            let id = d.get_u32()?;
            let handle = d.get_bytes()?.to_vec();
            Ok((id, Reply::Handle(handle)))
        }
        FXP_DATA => {
            let id = d.get_u32()?;
            let data = d.get_bytes()?.to_vec();
            Ok((id, Reply::Data(data)))
        }
        FXP_NAME => {
            let id = d.get_u32()?;
            let count = d.get_u32()?;
            if count > MAX_NAME_ENTRIES {
                return Err(ProtocolError::ImplausibleCount {
                    what: "NAME entry",
                    count,
                });
            }
            let mut entries = Vec::new();
            for _ in 0..count {
                let filename = d.get_bytes()?.to_vec();
                let longname = d.get_bytes()?.to_vec();
                let attrs = Attrs::decode(&mut d)?;
                entries.push(NameEntry {
                    filename,
                    longname,
                    attrs,
                });
            }
            Ok((id, Reply::Name(entries)))
        }
        FXP_ATTRS => {
            let id = d.get_u32()?;
            let attrs = Attrs::decode(&mut d)?;
            Ok((id, Reply::Attrs(attrs)))
        }
        kind => Err(ProtocolError::UnexpectedType {
            kind,
            expected: "a reply",
        }),
    }
}

// ── Convenience builders ────────────────────────────────────────────────────

/// The first thing on the wire: `INIT` with the version this client speaks.
pub fn init_packet() -> Result<Vec<u8>, ProtocolError> {
    Request::Init {
        version: SFTP_VERSION,
    }
    .encode(0)
}
