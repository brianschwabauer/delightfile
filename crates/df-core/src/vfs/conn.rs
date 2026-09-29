//! One connection: a child process, its three pipes, and every SFTP operation
//! delightfile performs over them.
//!
//! ## The child
//!
//! There is no ssh library here. [`Service::command`] builds `ssh -s <host>
//! sftp`, this file spawns it with all three streams piped, and the protocol is
//! spoken to its stdin and stdout. Everything hard about ssh — the key, the
//! agent, `known_hosts`, `~/.ssh/config`, `ProxyJump`, the FIDO token — belongs
//! to `ssh` and is inherited for free, exactly the way delightviewer inherited a
//! session bus by speaking D-Bus rather than linking a D-Bus library. The
//! failure mode this design does *not* have is the one every embedded ssh
//! client has: connecting successfully from the terminal and not from the app.
//!
//! ## Nothing blocks forever
//!
//! Every read and every write goes through [`crate::platform::pipe`] with a
//! deadline ([`super::CONNECT_TIMEOUT`] for the handshake, [`super::OP_TIMEOUT`]
//! for everything after). stderr is polled alongside stdout on every wait, so
//! `ssh`'s own diagnosis — "Permission denied (publickey)", "Host key
//! verification failed" — is already in hand at the moment stdout hits EOF, and
//! the user is told *why* the connection failed rather than that it did.
//!
//! ## Pipelining
//!
//! SFTP is request/response with a request id, so more than one request may be
//! outstanding. Lock-step transfers are latency-bound to uselessness: at a 30 ms
//! round trip and 32 KiB per request, one-at-a-time tops out near 1 MB/s no
//! matter how fast the link is.
//!
//! So a transfer keeps a **window** of [`READ_WINDOW`] requests in flight, each
//! for [`READ_CHUNK`] bytes: issue until the window is full, take one reply,
//! issue one more. Replies are matched by request id against a map of what was
//! asked for, and the data is written at *its own* offset with `write_all_at`
//! rather than appended — so out-of-order replies are correct rather than
//! merely unlikely. A reply carrying an id nobody is waiting for is a protocol
//! violation and hangs up the connection; it is the one failure a pipelined
//! client has that a lock-step one cannot, and it is checked rather than
//! assumed.
//!
//! Two subtleties the loop handles because the protocol permits them, not
//! because OpenSSH does them:
//!
//! - A `READ` may return **fewer bytes than asked for** without being at end of
//!   file. The remainder is re-queued as a new request rather than treated as
//!   EOF, which is how a short read becomes a slower transfer instead of a
//!   truncated file.
//! - `SSH_FX_EOF` arrives for whichever request first ran off the end, and the
//!   window may already hold requests *past* it. Their replies are consumed and
//!   discarded, and no new request is issued past the known end.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::config::Service;
use super::wire::{self, Attrs, Packet, Reply, Request, Status, StatusCode};
use super::{VfsError, VfsPath, CONNECT_TIMEOUT, OP_TIMEOUT};
use crate::platform::pipe;
use crate::tasks::TaskCtx;

/// Bytes per `READ`/`WRITE`.
///
/// 32 KiB, which is what OpenSSH's own sftp client uses over a plain connection
/// and comfortably under the 256 KiB the server will accept. Bigger chunks are
/// not obviously better: the win from pipelining comes from the *window*, and a
/// larger chunk only makes the tail of a small file wasteful and a cancel
/// slower to take effect, since a cancel lands between chunks.
pub const READ_CHUNK: u32 = 32 * 1024;

/// How many `READ`s are outstanding at once.
///
/// 16 × 32 KiB = 512 KiB in flight, which saturates a link up to roughly
/// (512 KiB / round-trip) — 17 MB/s at 30 ms, 170 MB/s at 3 ms. Past that the
/// bottleneck is the ssh transport's own window, not this one. The number is
/// bounded on the other side too: 512 KiB is the most this client can be holding
/// un-acknowledged, so a stalled server costs half a megabyte of memory and not
/// an unbounded queue.
pub const READ_WINDOW: usize = 16;

/// How many `WRITE`s are outstanding at once. Same arithmetic as
/// [`READ_WINDOW`], and the same number, because an upload is a download with
/// the arrows reversed.
pub const WRITE_WINDOW: usize = 16;

/// How many `STAT`s a listing will spend resolving symlinks.
///
/// A `READDIR` reply describes a symlink with the *link's* attributes, so a
/// listing cannot tell a link-to-directory from a link-to-file without one
/// extra round trip each. Those round trips are pipelined and cheap, but they
/// are not free, and a directory of ten thousand symlinks would spend the whole
/// listing on them.
///
/// 256 is several screenfuls' worth — far more links than any directory a
/// person is actually reading has. Past it, links are shown as links to files:
/// they still list, still sort, still open; `→` will not enter one without a
/// stat it did not do. Guessing "directory" would be worse, because entering a
/// thing that is not a directory is a visible failure and showing a file icon on
/// a directory link is not.
pub const MAX_LINK_RESOLVES: usize = 256;

/// The most of `ssh`'s stderr kept for the error message.
///
/// 8 KiB is dozens of lines of `ssh -v`, and the useful sentence is always in
/// the first few. It is capped at all because stderr is otherwise an unbounded
/// buffer fed by a process delightfile does not control.
pub const MAX_STDERR: usize = 8 * 1024;

/// One `read` off the child's stdout.
///
/// 64 KiB: two protocol chunks, so a full `DATA` packet usually arrives in one
/// or two reads, and one page-aligned allocation reused for the life of the
/// connection.
const SCRATCH: usize = 64 * 1024;

/// How long to wait for `ssh`'s parting words after its stdout dies.
///
/// When stdout hits EOF (or stdin breaks), the useful diagnosis is on stderr —
/// but it may not have *arrived* yet, because the two pipes race. 200 ms is far
/// longer than the gap between a process's last two writes and its exit, and
/// short enough that a connection failure still reports promptly. Bounded at
/// all because stderr belongs to a process delightfile does not control, and
/// an unbounded wait on it would turn "connection closed" into a hang.
const STDERR_GRACE: Duration = Duration::from_millis(200);

// ── Transport ───────────────────────────────────────────────────────────────

/// The child process and its pipes, framing packets and nothing else.
///
/// Split from [`Connection`] so that the byte-level rules — deadline on every
/// wait, length checked before allocation, stderr collected as it arrives — are
/// in one place and are the same rules for the handshake as for a bulk transfer.
struct Transport {
    service: String,
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
    /// Bytes read off stdout that are not yet a whole packet.
    inbuf: Vec<u8>,
    scratch: Vec<u8>,
    errbuf: Vec<u8>,
    /// The child's stderr reached EOF. Once it has, it is readable forever, so
    /// it stops being polled — otherwise every wait would return instantly and
    /// the deadline would be enforced by a busy loop.
    stderr_done: bool,
}

impl Transport {
    fn spawn(service: &Service) -> Result<Transport, VfsError> {
        let mut command = service.command();
        // Nothing here can wait on a pipe with a deadline (Windows, until
        // W4.20), so nothing is started: the connection fails with the
        // platform's refusal rather than with a session that could hang.
        if !pipe::AVAILABLE {
            return Err(VfsError::Spawn {
                service: service.name.clone(),
                program: command.get_program().to_string_lossy().into_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    crate::DfError::Unsupported("SFTP"),
                ),
            });
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|source| VfsError::Spawn {
            service: service.name.clone(),
            program: command.get_program().to_string_lossy().into_owned(),
            source,
        })?;

        // `take` on all three: the handles have to be owned here so that
        // dropping the transport closes them, which is what tells `ssh` to
        // exit. A `Child` that keeps its stdin open is a child that never
        // finishes.
        let missing = || VfsError::Disconnected {
            service: service.name.clone(),
            detail: "the child process had no pipes".to_string(),
        };
        let stdin = child.stdin.take().ok_or_else(missing)?;
        let stdout = child.stdout.take().ok_or_else(missing)?;
        let stderr = child.stderr.take().ok_or_else(missing)?;

        // Stdin and stderr, not stdout: see `pipe::set_nonblocking` for why
        // stdout stays blocking. Stderr must not block because
        // `drain_stderr` reads it *without* a preceding poll saying
        // "readable". A failure here is not fatal — a blocking pipe is still
        // correct, just capable of blocking past its deadline — so it is
        // logged rather than raised.
        for (name, fd) in [("stdin", pipe::fd(&stdin)), ("stderr", pipe::fd(&stderr))] {
            if let Err(e) = pipe::set_nonblocking(fd) {
                log::warn!("vfs {}: {name} stayed blocking: {e}", service.name);
            }
        }

        Ok(Transport {
            service: service.name.clone(),
            child,
            stdin,
            stdout,
            stderr,
            inbuf: Vec::with_capacity(SCRATCH),
            scratch: vec![0; SCRATCH],
            errbuf: Vec::new(),
            stderr_done: false,
        })
    }

    /// Whatever `ssh` has complained about, trimmed, or an empty string.
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.errbuf).trim().to_string()
    }

    fn disconnected(&self) -> VfsError {
        let detail = self.stderr_text();
        VfsError::Disconnected {
            service: self.service.clone(),
            detail: if detail.is_empty() {
                "the connection closed".to_string()
            } else {
                detail
            },
        }
    }

    fn timed_out(&self, op: &'static str, timeout: Duration) -> VfsError {
        VfsError::Timeout {
            service: self.service.clone(),
            op,
            timeout,
        }
    }

    fn io(&self, source: std::io::Error) -> VfsError {
        VfsError::Transport {
            service: self.service.clone(),
            source,
        }
    }

    /// Read whatever `ssh` has written to stderr right now, up to the cap.
    fn slurp_stderr(&mut self) {
        let mut buffer = [0u8; 4096];
        match self.stderr.read(&mut buffer) {
            Ok(0) => self.stderr_done = true,
            Ok(n) => {
                let room = MAX_STDERR.saturating_sub(self.errbuf.len());
                if room > 0 {
                    self.errbuf.extend_from_slice(&buffer[..n.min(room)]);
                }
            }
            // Nothing there after all, or a signal. Either way there is nothing
            // to do but carry on; stderr is diagnostic, never load-bearing.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock) => {}
            Err(_) => self.stderr_done = true,
        }
    }

    /// Give stderr a moment to deliver `ssh`'s diagnosis after the connection
    /// has died, then stop caring. Called on the EOF and broken-pipe paths,
    /// where the cause of death is racing down the other pipe.
    fn drain_stderr(&mut self) {
        let deadline = Instant::now() + STDERR_GRACE;
        while !self.stderr_done {
            let Some(left) = pipe::remaining(deadline) else {
                return;
            };
            match pipe::poll_read2(pipe::fd(&self.stderr), -1, left) {
                Ok((true, _)) => self.slurp_stderr(),
                Ok(_) => return, // the grace period elapsed with nothing there
                Err(_) => return,
            }
        }
    }

    /// Read until `inbuf` holds at least `want` bytes, or the deadline passes.
    fn fill(&mut self, want: usize, deadline: Instant, op: &'static str) -> Result<(), VfsError> {
        while self.inbuf.len() < want {
            let Some(left) = pipe::remaining(deadline) else {
                return Err(self.timed_out(op, OP_TIMEOUT));
            };
            // A negative fd is ignored by `poll`, which is how a finished
            // stderr stops waking the loop.
            let err_fd = if self.stderr_done {
                -1
            } else {
                pipe::fd(&self.stderr)
            };
            let (out_ready, err_ready) =
                pipe::poll_read2(pipe::fd(&self.stdout), err_fd, left).map_err(|e| self.io(e))?;
            if err_ready {
                self.slurp_stderr();
            }
            if !out_ready {
                continue;
            }
            match self.stdout.read(&mut self.scratch) {
                // EOF on stdout: the child is gone. Drain what it said on the
                // way out so the error names a cause.
                Ok(0) => {
                    self.drain_stderr();
                    return Err(self.disconnected());
                }
                Ok(n) => self.inbuf.extend_from_slice(&self.scratch[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(e) => return Err(self.io(e)),
            }
        }
        Ok(())
    }

    /// The next whole packet.
    fn read_packet(&mut self, deadline: Instant, op: &'static str) -> Result<Packet, VfsError> {
        self.fill(4, deadline, op)?;
        let len = u32::from_be_bytes([self.inbuf[0], self.inbuf[1], self.inbuf[2], self.inbuf[3]])
            as usize;
        // Both checks happen *before* `fill` is asked for the body, so a length
        // field claiming four gigabytes costs one refused packet rather than
        // one allocation and a very long wait.
        if len == 0 {
            return Err(self.protocol(wire::ProtocolError::Empty));
        }
        if len > wire::MAX_PACKET {
            return Err(self.protocol(wire::ProtocolError::TooLarge(len)));
        }
        self.fill(4 + len, deadline, op)?;
        let packet = Packet::from_body(&self.inbuf[4..4 + len]).map_err(|e| self.protocol(e))?;
        self.inbuf.drain(..4 + len);
        Ok(packet)
    }

    fn protocol(&self, source: wire::ProtocolError) -> VfsError {
        VfsError::Protocol {
            service: self.service.clone(),
            source,
        }
    }

    fn write_all(
        &mut self,
        mut bytes: &[u8],
        deadline: Instant,
        op: &'static str,
    ) -> Result<(), VfsError> {
        while !bytes.is_empty() {
            let Some(left) = pipe::remaining(deadline) else {
                return Err(self.timed_out(op, OP_TIMEOUT));
            };
            if !pipe::poll_write(pipe::fd(&self.stdin), left).map_err(|e| self.io(e))? {
                continue;
            }
            match self.stdin.write(bytes) {
                Ok(0) => return Err(self.disconnected()),
                Ok(n) => bytes = &bytes[n..],
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                // A broken pipe means the child exited; its stderr says why.
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    self.drain_stderr();
                    return Err(self.disconnected());
                }
                Err(e) => return Err(self.io(e)),
            }
        }
        Ok(())
    }
}

impl Drop for Transport {
    /// Kill the child and reap it.
    ///
    /// `ssh` would exit on its own once stdin closes, but "would" is doing a lot
    /// of work there: a session wedged on a dead network does not notice its
    /// stdin. Killing is unconditional and `wait` is unconditional after it,
    /// because the alternative — a zombie per reconnect, and an `ssh` per
    /// zombie — is a file manager that leaks processes for as long as it runs.
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

// ── Connection ──────────────────────────────────────────────────────────────

/// A live SFTP session on one child process.
pub(super) struct Connection {
    service: Arc<Service>,
    transport: Transport,
    next_id: u32,
    /// What `.` resolves to on this server — the login directory. Filled in on
    /// first use and kept, because it cannot change under a live session and a
    /// `REALPATH` per listing is a round trip spent on a constant.
    home: Option<String>,
}

impl Connection {
    /// Spawn the child and complete the `INIT`/`VERSION` handshake.
    ///
    /// The handshake gets [`CONNECT_TIMEOUT`] rather than [`OP_TIMEOUT`],
    /// because it is not one round trip: it is a TCP connect, a key exchange, an
    /// agent round trip and possibly a `ProxyJump` before a single SFTP byte
    /// moves.
    pub(super) fn connect(service: Arc<Service>) -> Result<Connection, VfsError> {
        let mut transport = Transport::spawn(&service)?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;

        let init = wire::init_packet().map_err(|e| transport.protocol(e))?;
        transport.write_all(&init, deadline, "connect")?;

        let packet = match transport.read_packet(deadline, "connect") {
            Ok(packet) => packet,
            // A connection that dies before its first reply is almost always an
            // authentication or host-key problem, and `ssh` has already said
            // which on stderr. Classifying it here is what turns "the
            // connection closed" into a sentence the user can act on.
            Err(VfsError::Disconnected { service: s, detail }) => {
                return Err(classify_ssh_failure(s, detail))
            }
            Err(e) => return Err(e),
        };
        let (_, reply) = wire::decode_reply(&packet).map_err(|e| transport.protocol(e))?;
        let Reply::Version { version, .. } = reply else {
            return Err(transport.protocol(wire::ProtocolError::UnexpectedType {
                kind: packet.kind,
                expected: "VERSION",
            }));
        };
        if version != wire::SFTP_VERSION {
            return Err(transport.protocol(wire::ProtocolError::Version(version)));
        }

        let mut connection = Connection {
            service,
            transport,
            // 1, not 0: `VERSION` reports id 0 and nothing may ever be waiting
            // on it.
            next_id: 1,
            home: None,
        };
        // Resolve the login (or configured `root`) directory now, not lazily:
        // every relative path in every later request resolves against it, so a
        // connection without it would quietly address the server's *cwd* —
        // fine over `ssh` (cwd is the login home) and wrong for a `program`
        // service pointed at a directory. One round trip, once per connection,
        // and a root that does not exist fails here, loudly, instead of as a
        // confusing NoSuchFile three operations later.
        connection.resolve_home()?;
        Ok(connection)
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Send one request, returning the id it was given.
    fn send(
        &mut self,
        request: &Request,
        deadline: Instant,
        op: &'static str,
    ) -> Result<u32, VfsError> {
        let id = self.alloc_id();
        let bytes = request.encode(id).map_err(|e| self.transport.protocol(e))?;
        self.transport.write_all(&bytes, deadline, op)?;
        Ok(id)
    }

    /// Take one reply, whatever it answers.
    fn recv(&mut self, deadline: Instant, op: &'static str) -> Result<(u32, Reply), VfsError> {
        let packet = self.transport.read_packet(deadline, op)?;
        wire::decode_reply(&packet).map_err(|e| self.transport.protocol(e))
    }

    /// One request, one reply, id checked.
    fn round_trip(&mut self, request: Request, op: &'static str) -> Result<Reply, VfsError> {
        let deadline = Instant::now() + OP_TIMEOUT;
        let id = self.send(&request, deadline, op)?;
        let (got, reply) = self.recv(deadline, op)?;
        if got != id {
            return Err(self
                .transport
                .protocol(wire::ProtocolError::IdMismatch { expected: id, got }));
        }
        Ok(reply)
    }

    /// The wire form of a [`VfsPath`] on this service.
    fn wire_path(&self, path: &VfsPath) -> String {
        let raw = path.path.trim();
        if raw.is_empty() || raw == "/" {
            return self
                .home
                .clone()
                .unwrap_or_else(|| self.service.root_path().to_string());
        }
        match (&self.home, raw.starts_with('/')) {
            // An absolute path is absolute; the root only ever supplies a
            // starting point, never a chroot. Pretending otherwise would make
            // `sftp://host//etc` unreachable, and a file manager that cannot
            // express a path is not one.
            (_, true) => raw.to_string(),
            (Some(home), false) => format!("{}/{raw}", home.trim_end_matches('/')),
            (None, false) => raw.to_string(),
        }
    }

    /// `REALPATH` the service root and remember it as `home`. Called once, at
    /// connect; see [`Connection::connect`] for why it is not lazy.
    fn resolve_home(&mut self) -> Result<(), VfsError> {
        let root = self.service.root_path().to_string();
        let reply = self.round_trip(
            Request::RealPath {
                path: root.clone().into_bytes(),
            },
            "realpath",
        )?;
        let home = match reply {
            Reply::Name(entries) => entries
                .first()
                .map(|e| e.name_lossy())
                // A `REALPATH` that resolves to nothing is not fatal: the
                // configured root is still a path the server accepts, it is
                // just not canonical.
                .unwrap_or(root),
            Reply::Status(status) => {
                return Err(self.status_error(&VfsPath::new(&self.service.name, root), status))
            }
            other => return Err(self.unexpected(&other, "NAME")),
        };
        self.home = Some(home);
        Ok(())
    }

    /// Resolve a path server-side. Returns the canonical absolute path.
    pub(super) fn realpath(&mut self, path: &VfsPath) -> Result<String, VfsError> {
        let wire = self.wire_path(path);
        let reply = self.round_trip(
            Request::RealPath {
                path: wire.clone().into_bytes(),
            },
            "realpath",
        )?;
        match reply {
            Reply::Name(entries) => Ok(entries.first().map(|e| e.name_lossy()).unwrap_or(wire)),
            // A path that does not exist has no canonical form; the caller's
            // own operation will produce the real error a moment later, so this
            // hands back what was asked for rather than failing early with a
            // less specific message.
            Reply::Status(_) => Ok(wire),
            other => Err(self.unexpected(&other, "NAME")),
        }
    }

    fn status_error(&self, path: &VfsPath, status: Status) -> VfsError {
        VfsError::Status {
            path: path.to_url(),
            status,
        }
    }

    fn unexpected(&self, reply: &Reply, expected: &'static str) -> VfsError {
        // A connection-fatal error, like every protocol violation: a reply of
        // the wrong type means the stream and this client disagree about where
        // they are, and there is no resynchronising a length-framed stream.
        VfsError::Unexpected {
            service: self.service.name.clone(),
            expected,
            got: reply.kind_name(),
        }
    }

    fn expect_ok(&self, path: &VfsPath, reply: Reply) -> Result<(), VfsError> {
        match reply {
            Reply::Status(status) if status.code == StatusCode::Ok => Ok(()),
            Reply::Status(status) => Err(self.status_error(path, status)),
            other => Err(self.unexpected(&other, "STATUS")),
        }
    }

    fn expect_handle(&self, path: &VfsPath, reply: Reply) -> Result<Vec<u8>, VfsError> {
        match reply {
            Reply::Handle(handle) => Ok(handle),
            Reply::Status(status) => Err(self.status_error(path, status)),
            other => Err(self.unexpected(&other, "HANDLE")),
        }
    }

    fn expect_attrs(&self, path: &VfsPath, reply: Reply) -> Result<Attrs, VfsError> {
        match reply {
            Reply::Attrs(attrs) => Ok(attrs),
            Reply::Status(status) => Err(self.status_error(path, status)),
            other => Err(self.unexpected(&other, "ATTRS")),
        }
    }

    // ── Metadata ────────────────────────────────────────────────────────────

    /// `STAT` (follows symlinks) or `LSTAT` (does not).
    pub(super) fn stat(&mut self, path: &VfsPath, follow: bool) -> Result<Attrs, VfsError> {
        let wire_path = self.wire_path(path).into_bytes();
        let request = if follow {
            Request::Stat { path: wire_path }
        } else {
            Request::LStat { path: wire_path }
        };
        let reply = self.round_trip(request, if follow { "stat" } else { "lstat" })?;
        self.expect_attrs(path, reply)
    }

    pub(super) fn mkdir(&mut self, path: &VfsPath) -> Result<(), VfsError> {
        let wire_path = self.wire_path(path).into_bytes();
        let reply = self.round_trip(
            Request::Mkdir {
                path: wire_path,
                attrs: Attrs::empty(),
            },
            "mkdir",
        )?;
        self.expect_ok(path, reply)
    }

    pub(super) fn rmdir(&mut self, path: &VfsPath) -> Result<(), VfsError> {
        let wire_path = self.wire_path(path).into_bytes();
        let reply = self.round_trip(Request::Rmdir { path: wire_path }, "rmdir")?;
        self.expect_ok(path, reply)
    }

    pub(super) fn remove(&mut self, path: &VfsPath) -> Result<(), VfsError> {
        let wire_path = self.wire_path(path).into_bytes();
        let reply = self.round_trip(Request::Remove { path: wire_path }, "remove")?;
        self.expect_ok(path, reply)
    }

    pub(super) fn rename(&mut self, from: &VfsPath, to: &VfsPath) -> Result<(), VfsError> {
        let old = self.wire_path(from).into_bytes();
        let new = self.wire_path(to).into_bytes();
        let reply = self.round_trip(Request::Rename { old, new }, "rename")?;
        self.expect_ok(from, reply)
    }

    pub(super) fn readlink(&mut self, path: &VfsPath) -> Result<String, VfsError> {
        let wire_path = self.wire_path(path).into_bytes();
        let reply = self.round_trip(Request::ReadLink { path: wire_path }, "readlink")?;
        match reply {
            Reply::Name(entries) => match entries.first() {
                Some(entry) => Ok(entry.name_lossy()),
                None => Err(self.status_error(
                    path,
                    Status {
                        code: StatusCode::Failure,
                        message: "the server returned no link target".to_string(),
                    },
                )),
            },
            Reply::Status(status) => Err(self.status_error(path, status)),
            other => Err(self.unexpected(&other, "NAME")),
        }
    }

    /// Create a symlink. `target` is what it points at, `link` is what is
    /// created — and they go on the wire in **that** order, which is OpenSSH's
    /// and not the draft's. See [`super::wire`].
    pub(super) fn symlink(&mut self, target: &str, link: &VfsPath) -> Result<(), VfsError> {
        let link_path = self.wire_path(link).into_bytes();
        let reply = self.round_trip(
            Request::Symlink {
                target: target.as_bytes().to_vec(),
                link: link_path,
            },
            "symlink",
        )?;
        self.expect_ok(link, reply)
    }

    /// `chmod`, as the `SETSTAT` it is.
    pub(super) fn chmod(&mut self, path: &VfsPath, mode: u32) -> Result<(), VfsError> {
        let wire_path = self.wire_path(path).into_bytes();
        let reply = self.round_trip(
            Request::SetStat {
                path: wire_path,
                // Permission bits only: sending the type bits back would ask
                // the server to change a file's type, which is not a thing.
                attrs: Attrs::permissions(mode & 0o7777),
            },
            "chmod",
        )?;
        self.expect_ok(path, reply)
    }

    // ── Listing ─────────────────────────────────────────────────────────────

    pub(super) fn opendir(&mut self, path: &VfsPath) -> Result<Vec<u8>, VfsError> {
        let wire_path = self.wire_path(path).into_bytes();
        let reply = self.round_trip(Request::OpenDir { path: wire_path }, "opendir")?;
        self.expect_handle(path, reply)
    }

    /// One `READDIR`. `Ok(None)` is the end of the directory.
    pub(super) fn readdir(
        &mut self,
        path: &VfsPath,
        handle: &[u8],
    ) -> Result<Option<Vec<wire::NameEntry>>, VfsError> {
        let reply = self.round_trip(
            Request::ReadDir {
                handle: handle.to_vec(),
            },
            "readdir",
        )?;
        match reply {
            Reply::Name(entries) => Ok(Some(entries)),
            Reply::Status(status) if status.code.is_eof() => Ok(None),
            Reply::Status(status) => Err(self.status_error(path, status)),
            other => Err(self.unexpected(&other, "NAME")),
        }
    }

    /// Close a handle, and say so in the log rather than to the user when it
    /// fails: a failed `CLOSE` after a successful read has already given the
    /// caller what it came for, and turning it into an error would fail
    /// operations that worked.
    pub(super) fn close_quietly(&mut self, handle: &[u8]) {
        let request = Request::Close {
            handle: handle.to_vec(),
        };
        if let Err(e) = self.round_trip(request, "close") {
            log::debug!("vfs {}: close failed: {e}", self.service.name);
        }
    }

    /// Follow a batch of symlinks with pipelined `STAT`s.
    ///
    /// Returns, per input path, whether the target is a directory — `None` where
    /// the link is broken or the budget ran out.
    pub(super) fn stat_many(&mut self, paths: &[VfsPath]) -> Result<Vec<Option<Attrs>>, VfsError> {
        let mut results: Vec<Option<Attrs>> = vec![None; paths.len()];
        let mut next = 0usize;
        let mut in_flight: HashMap<u32, usize> = HashMap::new();
        let deadline = Instant::now() + OP_TIMEOUT;

        while next < paths.len() || !in_flight.is_empty() {
            while in_flight.len() < READ_WINDOW && next < paths.len() {
                let request = Request::Stat {
                    path: self.wire_path(&paths[next]).into_bytes(),
                };
                let id = self.send(&request, deadline, "stat")?;
                in_flight.insert(id, next);
                next += 1;
            }
            let (id, reply) = self.recv(deadline, "stat")?;
            let Some(index) = in_flight.remove(&id) else {
                return Err(self.transport.protocol(wire::ProtocolError::IdMismatch {
                    expected: 0,
                    got: id,
                }));
            };
            match reply {
                Reply::Attrs(attrs) => results[index] = Some(attrs),
                // A broken link is not an error in a listing — it is a row.
                Reply::Status(_) => results[index] = None,
                other => return Err(self.unexpected(&other, "ATTRS")),
            }
        }
        Ok(results)
    }

    // ── Transfers ───────────────────────────────────────────────────────────

    /// Download `remote` to `local`, pipelined. Returns the byte count.
    pub(super) fn download(
        &mut self,
        remote: &VfsPath,
        local: &Path,
        ctx: &TaskCtx,
    ) -> Result<u64, VfsError> {
        let attrs = self.stat(remote, true)?;
        let size = attrs.size;
        ctx.set_total(size.unwrap_or(0), 1);

        let wire_path = self.wire_path(remote).into_bytes();
        let reply = self.round_trip(
            Request::Open {
                path: wire_path,
                pflags: wire::FXF_READ,
                attrs: Attrs::empty(),
            },
            "open",
        )?;
        let handle = self.expect_handle(remote, reply)?;

        let file = std::fs::File::create(local).map_err(|e| VfsError::Io {
            path: local.to_path_buf(),
            source: e,
        })?;

        let outcome = self.download_body(&handle, &file, remote, local, size, ctx);
        self.close_quietly(&handle);
        match outcome {
            Ok(written) => {
                file.sync_all().map_err(|e| VfsError::Io {
                    path: local.to_path_buf(),
                    source: e,
                })?;
                Ok(written)
            }
            Err(e) => {
                // A half-downloaded file is worse than none: the preview would
                // open it, the opener would launch on it. `ops::copy` removes
                // its partial destination for the same reason.
                drop(file);
                let _ignored = std::fs::remove_file(local);
                Err(e)
            }
        }
    }

    fn download_body(
        &mut self,
        handle: &[u8],
        file: &std::fs::File,
        remote: &VfsPath,
        local: &Path,
        size: Option<u64>,
        ctx: &TaskCtx,
    ) -> Result<u64, VfsError> {
        let mut in_flight: HashMap<u32, (u64, u32)> = HashMap::new();
        // Offsets whose earlier request came back short and need finishing.
        let mut requeued: VecDeque<(u64, u32)> = VecDeque::new();
        let mut next_offset = 0u64;
        let mut end: Option<u64> = None;
        let mut written = 0u64;
        let mut high_water = 0u64;

        loop {
            // Between chunks, not inside one: a cancel takes effect within one
            // 32 KiB round trip, and a pause holds the *connection* thread,
            // which is the thread actually moving the bytes.
            if let Err(crate::DfError::Cancelled) = ctx.checkpoint() {
                self.drain(in_flight.len())?;
                return Err(VfsError::Cancelled);
            }

            while in_flight.len() < READ_WINDOW {
                let next = if let Some(pair) = requeued.pop_front() {
                    pair
                } else if end.is_none() && size.is_none_or(|s| next_offset < s) {
                    let pair = (next_offset, READ_CHUNK);
                    next_offset += u64::from(READ_CHUNK);
                    pair
                } else {
                    break;
                };
                if end.is_some_and(|e| next.0 >= e) {
                    continue;
                }
                let deadline = Instant::now() + OP_TIMEOUT;
                let request = Request::Read {
                    handle: handle.to_vec(),
                    offset: next.0,
                    len: next.1,
                };
                let id = self.send(&request, deadline, "read")?;
                in_flight.insert(id, next);
            }

            if in_flight.is_empty() {
                // `written` counts every byte that landed, which for a
                // completed transfer is the file's true size — even when the
                // initial `STAT` lied (a file growing or shrinking mid-copy).
                log::debug!(
                    "vfs {}: downloaded {written} bytes (high water {high_water})",
                    self.service.name
                );
                return Ok(written);
            }

            let deadline = Instant::now() + OP_TIMEOUT;
            let (id, reply) = self.recv(deadline, "read")?;
            let Some((offset, len)) = in_flight.remove(&id) else {
                return Err(self.transport.protocol(wire::ProtocolError::IdMismatch {
                    expected: 0,
                    got: id,
                }));
            };
            match reply {
                Reply::Data(data) if data.is_empty() => {
                    // Zero bytes is not a legal `DATA` reply, but treating it as
                    // end-of-file rather than as an error is what keeps a
                    // server's bad day from becoming an infinite loop.
                    end = Some(end.unwrap_or(u64::MAX).min(offset));
                    requeued.retain(|(o, _)| *o < offset);
                }
                Reply::Data(data) => {
                    crate::platform::fs::write_all_at(file, &data, offset).map_err(|e| {
                        VfsError::Io {
                            path: local.to_path_buf(),
                            source: e,
                        }
                    })?;
                    let n = data.len() as u64;
                    written += n;
                    high_water = high_water.max(offset + n);
                    ctx.advance(n, 0);
                    // Short read: the rest of that chunk is still owed.
                    if data.len() < len as usize {
                        let rest_at = offset + n;
                        if end.is_none_or(|e| rest_at < e) {
                            requeued.push_back((rest_at, len - data.len() as u32));
                        }
                    }
                }
                Reply::Status(status) if status.code.is_eof() => {
                    end = Some(end.unwrap_or(u64::MAX).min(offset));
                    requeued.retain(|(o, _)| *o < offset);
                }
                Reply::Status(status) => {
                    self.drain(in_flight.len())?;
                    return Err(VfsError::Status {
                        path: remote.to_url(),
                        status,
                    });
                }
                other => return Err(self.unexpected(&other, "DATA")),
            }
        }
    }

    /// The sibling scratch name an upload writes to before it is renamed into
    /// place.
    ///
    /// A sibling, so the final rename is within one directory and therefore
    /// within one filesystem — the whole point of the dance is that the last
    /// step cannot half-succeed. Dotted, so it is hidden from a listing that
    /// catches it mid-flight, and stamped with a pid and a counter so two
    /// uploads of the same name from the same session do not collide.
    fn upload_temp(remote: &VfsPath) -> VfsPath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!(".{}.df-upload-{}-{n}", remote.name(), std::process::id());
        match remote.parent() {
            Some(parent) => parent.join(&name),
            None => VfsPath::new(&remote.service, name),
        }
    }

    /// Upload `local` to `remote`, pipelined. Returns the byte count.
    pub(super) fn upload(
        &mut self,
        local: &Path,
        remote: &VfsPath,
        ctx: &TaskCtx,
    ) -> Result<u64, VfsError> {
        let mut file = std::fs::File::open(local).map_err(|e| VfsError::Io {
            path: local.to_path_buf(),
            source: e,
        })?;
        let size = file
            .metadata()
            .map_err(|e| VfsError::Io {
                path: local.to_path_buf(),
                source: e,
            })?
            .len();
        ctx.set_total(size, 1);

        // Never `FXF_TRUNC` on the destination. Opening the real file that way
        // destroys whatever was there at byte one, so a cancel — or a dropped
        // connection — halfway through re-uploading a file leaves the user with
        // *neither* the old contents nor the new. The bytes go to a sibling
        // temporary instead, and the destination is only touched once there is a
        // whole file to put there. `FXF_EXCL` on the temp so two uploads
        // running at once cannot pick the same scratch name and interleave.
        let scratch = Self::upload_temp(remote);
        let wire_path = self.wire_path(&scratch).into_bytes();
        let reply = self.round_trip(
            Request::Open {
                path: wire_path,
                pflags: wire::FXF_WRITE | wire::FXF_CREAT | wire::FXF_EXCL,
                attrs: Attrs::empty(),
            },
            "open",
        )?;
        let handle = self.expect_handle(&scratch, reply)?;

        let outcome = self.upload_body(&handle, &mut file, local, &scratch, ctx);
        self.close_quietly(&handle);
        let sent = match outcome {
            Ok(sent) => sent,
            Err(e) => {
                // Only the scratch file goes; the destination was never opened.
                if let Err(cleanup) = self.remove(&scratch) {
                    log::warn!(
                        "vfs: partial upload {} left behind: {cleanup}",
                        scratch.to_url()
                    );
                }
                return Err(e);
            }
        };

        // SFTP v3 `rename` refuses an existing destination, so replacing one
        // means unlinking first. That window is unavoidable over this protocol —
        // but it is now a window of milliseconds between two complete files,
        // rather than the whole duration of the transfer.
        let _ignored = self.remove(remote);
        if let Err(e) = self.rename(&scratch, remote) {
            let _ignored = self.remove(&scratch);
            return Err(e);
        }
        Ok(sent)
    }

    fn upload_body(
        &mut self,
        handle: &[u8],
        file: &mut std::fs::File,
        local: &Path,
        remote: &VfsPath,
        ctx: &TaskCtx,
    ) -> Result<u64, VfsError> {
        let mut in_flight: HashMap<u32, u64> = HashMap::new();
        let mut offset = 0u64;
        let mut sent = 0u64;
        let mut eof = false;
        let mut buffer = vec![0u8; READ_CHUNK as usize];

        loop {
            if let Err(crate::DfError::Cancelled) = ctx.checkpoint() {
                self.drain(in_flight.len())?;
                return Err(VfsError::Cancelled);
            }

            while !eof && in_flight.len() < WRITE_WINDOW {
                let mut filled = 0usize;
                // `read` is allowed to return short; a short read here would
                // send a short chunk, which is legal but wasteful, so the buffer
                // is filled properly before it goes out.
                while filled < buffer.len() {
                    match file.read(&mut buffer[filled..]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            return Err(VfsError::Io {
                                path: local.to_path_buf(),
                                source: e,
                            })
                        }
                    }
                }
                if filled == 0 {
                    eof = true;
                    break;
                }
                let deadline = Instant::now() + OP_TIMEOUT;
                let request = Request::Write {
                    handle: handle.to_vec(),
                    offset,
                    data: buffer[..filled].to_vec(),
                };
                let id = self.send(&request, deadline, "write")?;
                in_flight.insert(id, filled as u64);
                offset += filled as u64;
            }

            if in_flight.is_empty() {
                return Ok(sent);
            }

            let deadline = Instant::now() + OP_TIMEOUT;
            let (id, reply) = self.recv(deadline, "write")?;
            let Some(chunk) = in_flight.remove(&id) else {
                return Err(self.transport.protocol(wire::ProtocolError::IdMismatch {
                    expected: 0,
                    got: id,
                }));
            };
            match reply {
                Reply::Status(status) if status.code == StatusCode::Ok => {
                    sent += chunk;
                    ctx.advance(chunk, 0);
                }
                Reply::Status(status) => {
                    self.drain(in_flight.len())?;
                    return Err(VfsError::Status {
                        path: remote.to_url(),
                        status,
                    });
                }
                other => return Err(self.unexpected(&other, "STATUS")),
            }
        }
    }

    /// Consume `count` replies and throw them away.
    ///
    /// The price of pipelining: a transfer that stops early still has requests
    /// the server is going to answer, and leaving them in the pipe would make
    /// the *next* operation on this connection read somebody else's reply. Every
    /// early exit either drains or tears the connection down; there is no third
    /// option that leaves the stream usable.
    fn drain(&mut self, count: usize) -> Result<(), VfsError> {
        let deadline = Instant::now() + OP_TIMEOUT;
        for _ in 0..count {
            self.recv(deadline, "drain")?;
        }
        Ok(())
    }
}

/// Turn `ssh`'s parting words into the right error.
///
/// The distinction that matters to the user: "delightfile cannot log in to this
/// machine" (fix your keys) versus "delightfile cannot reach this machine" (fix
/// your network). Both arrive here as an EOF on stdout, and only the text tells
/// them apart — so the text is what is matched, lowercased, against the phrases
/// OpenSSH actually emits.
pub(super) fn classify_ssh_failure(service: String, detail: String) -> VfsError {
    let lower = detail.to_lowercase();
    let is_auth = lower.contains("permission denied")
        || lower.contains("host key verification failed")
        || lower.contains("no supported authentication")
        || lower.contains("too many authentication failures")
        || lower.contains("batch mode");
    if is_auth {
        VfsError::Auth { service, detail }
    } else {
        VfsError::Disconnected { service, detail }
    }
}
