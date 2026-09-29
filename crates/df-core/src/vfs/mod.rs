//! Remote filesystems over SFTP — PLAN §7.6's `g 1`/`g 2`, without an ssh
//! library — and cloud storage through rclone, without a cloud SDK.
//!
//! ## The design in one sentence
//!
//! delightfile spawns `ssh -s <host> sftp` and speaks SFTP **version 3**
//! (draft-ietf-secsh-filexfer-02, the one OpenSSH's `sftp-server` implements)
//! to the child's stdin and stdout — so authentication, `~/.ssh/config`, the
//! agent, `known_hosts`, `ProxyJump` and every other hard part of ssh belong to
//! `ssh`, exactly the way delightviewer inherited a session bus by hand-rolling
//! D-Bus instead of linking zbus. There is no ssh2, no libssh, and there never
//! will be: the failure mode every embedded ssh client has — "it connects from
//! my terminal but not from the app" — is structurally impossible here, because
//! the app's connection *is* the terminal's `ssh`.
//!
//! `BatchMode=yes` is passed unconditionally, so a host that wants a password
//! makes `ssh` fail immediately with a sentence on stderr instead of sitting on
//! a prompt nobody will ever see; that sentence is collected and becomes the
//! error the user reads.
//!
//! ## Cloud storage: the same design, with rclone in the place of ssh
//!
//! A service of `type = "rclone"` — or any remote in the user's
//! `rclone.conf`, which is read for them (see `config`) — is reached by
//! running `rclone rcd` on a private unix socket and asking it, in rclone's
//! remote-control API, to list, stat, copy and move. The app's connection *is*
//! the user's `rclone config`, so it never holds a token: Google Drive's OAuth,
//! Dropbox's refresh, S3's signing and every provider's quirks belong to
//! rclone, exactly as keys and `known_hosts` belong to `ssh`. Places on such a
//! service are `rclone://service/path`, they arrive in the same batches as
//! [`Entry`] rows, and the [`Vfs`] API is the same API; a worker simply holds
//! an rclone daemon where an sftp worker holds a session. See `rclone` for the
//! daemon, the jobs a transfer runs as, and what a cloud remote cannot do.
//!
//! ## The pieces
//!
//! - [`wire`] — the pure codec: bytes ↔ messages, every length checked, no I/O.
//! - [`crate::platform::pipe`] — timed waits on the child's pipes (`poll` on
//!   Unix; nothing yet on Windows, where SFTP says it is not available).
//! - `conn` — one child process and one live session: handshake, pipelined
//!   transfers, request-id matching, teardown.
//! - `config` — `vfs.toml`, read from yazi's file first and delightfile's
//!   second (per-service override), so the machines `g 1`/`g 2` already reach
//!   in yazi work on day one; then the remotes in `rclone.conf`.
//! - `rclone` — one `rclone rcd` child per cloud service and the calls made to
//!   it, over [`json`], `http` (one `POST` per call on a unix socket) and
//!   `rfc3339` (rclone's dates).
//! - `child` — the daemon's life tied to its worker thread's (a parent-death
//!   signal set between `fork` and `exec`) and its gentle `SIGTERM`.
//! - [`Vfs`] (this file) — the manager: one worker thread per service, lazy
//!   connect, reconnect after a drop, and the channel-and-token listing API.
//!
//! ## Shaped like `fs::Scanner` on purpose
//!
//! A remote listing arrives exactly the way a local one does: batches on a
//! crossbeam channel, a generation token to drop stale answers, and a
//! [`Notifier`] rung once per update so the event loop wakes instead of
//! polling. The batches carry [`crate::fs::Entry`] values whose `path` is the
//! `sftp://service/…` URL — so df-app can feed them through the very same
//! `DirState` sort/filter/cursor machinery that draws a local directory, and a
//! remote pane is not a second file manager.
//!
//! Operations (stat, mkdir, rename, download, upload, …) are blocking calls
//! against the [`Vfs`] handle. They are meant to be run from a
//! [`crate::tasks`] job — each takes a [`TaskCtx`] and honours pause/cancel
//! between chunks — while the vfs serialises them per service on that
//! service's one connection, which is the ordering one ssh session gives you
//! anyway.
//!
//! ## Nothing hangs, nothing leaks
//!
//! Every wait has a deadline ([`CONNECT_TIMEOUT`] for the handshake,
//! [`OP_TIMEOUT`] per operation), every length field from the wire is checked
//! before it is believed, a protocol violation tears the connection down
//! rather than guessing at resynchronisation, and dropping the [`Vfs`] joins
//! every worker, each of which kills and reaps its `ssh`. The next request
//! after a teardown reconnects lazily — a dropped wifi link costs one visible
//! error and one reconnect, not a restart.

/// Public for the app's one long-lived child of its own, the gvfs watcher:
/// see the module's note.
pub mod child;
mod config;
mod conn;
mod http;
/// Public for the same reason [`wire`] is: rclone's replies are documents in
/// this format, and the codec is a small, tested thing a reader may want.
pub mod json;
mod rclone;
mod rfc3339;
/// Public because the codec *is* a documented artifact: df-app never speaks
/// it, but the message-level types ([`wire::Request`], [`wire::Reply`]) and
/// the protocol constants are the reference for anyone reading a pcap or
/// extending the client, and the tests exercise both directions of it.
pub mod wire;

/// The daemon over its unix socket, with a real or a scripted rclone: Unix
/// until W4.32 gives Windows a transport.
#[cfg(all(test, unix))]
mod rclone_tests;
#[cfg(test)]
mod tests;

pub use config::{
    config_paths, load_rclone_conf, parse_rclone_conf, rclone_config_path, Service, ServiceKind,
    VfsConfig, DEFAULT_SSH_PORT,
};
pub use conn::{MAX_LINK_RESOLVES, READ_CHUNK, READ_WINDOW, WRITE_WINDOW};
pub use wire::{Attrs, ProtocolError, Status, StatusCode, MAX_PACKET};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::fs::{mime, Entry, Kind, LinkTarget, Notifier};
use crate::tasks::TaskCtx;
use crate::toml::ConfigWarning;
use conn::Connection;

/// How long the `INIT`/`VERSION` handshake may take, end to end.
///
/// 15 seconds, because "connect" is not one round trip: it is a TCP connect, a
/// key exchange, an agent (or FIDO token) round trip, and possibly a whole
/// `ProxyJump` chain before the first SFTP byte moves. `ssh` is also given its
/// own `ConnectTimeout` of the same length (see [`Service::command`]) so the
/// usual outcome of a dead host is ssh's *worded* failure, not this deadline.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long any single operation may go without the server answering.
///
/// 30 seconds. This is not "how long may a download take" — a transfer resets
/// its deadline per chunk — it is "how long may the server say *nothing*".
/// A link that cannot move one 32 KiB chunk in half a minute is dead in every
/// way that matters, and a stat that takes longer than this is a hung server.
/// Long enough for a spun-down disk on the far end; short enough that the user
/// learns the truth before they give up on the app.
pub const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// The URL scheme this module answers to. `sftp://service/path`, where
/// `service` is a `[services.<name>]` table in `vfs.toml` — **not** a
/// hostname. The config owns the mapping from name to machine.
pub const URL_SCHEME: &str = "sftp://";

/// The scheme of a place on an rclone service: `rclone://r2/bucket/photos`,
/// where `r2` is the service (usually a remote in `rclone.conf` of the same
/// name) and the rest is the path inside it.
pub const RCLONE_URL_SCHEME: &str = "rclone://";

// ── Addressing ──────────────────────────────────────────────────────────────

/// One place on one service: the vfs's `PathBuf`.
///
/// `path` is the part after `sftp://service`. Empty (or `/`) means the
/// service's root — the login home directory unless the config's `root` says
/// otherwise; an absolute path is absolute on the server; a relative one is
/// resolved against the login directory. The string is not canonicalised
/// here, because only the server knows what `..` means through its symlinks.
///
/// On an rclone service there is no login directory and no "absolute": every
/// path is inside the remote (and inside the service's `root`), so an
/// `rclone://` path is kept without its leading and trailing slashes —
/// `rclone://r2/a/b` and a row joined down to `a/b` from the root are the same
/// value, which is what lets a listing's rows and a typed URL agree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VfsPath {
    /// Which backend the service is, and so which scheme the URL has.
    pub kind: ServiceKind,
    /// The `[services.<name>]` name, never a hostname.
    pub service: String,
    pub path: String,
}

impl VfsPath {
    /// A place on an sftp service.
    pub fn new(service: impl Into<String>, path: impl Into<String>) -> VfsPath {
        VfsPath::of(ServiceKind::Sftp, service, path)
    }

    /// A place on an rclone service.
    pub fn rclone(service: impl Into<String>, path: impl Into<String>) -> VfsPath {
        VfsPath::of(ServiceKind::Rclone, service, path)
    }

    /// A place on `service`, whichever kind it is.
    pub fn for_service(service: &Service, path: impl Into<String>) -> VfsPath {
        VfsPath::of(service.kind, service.name.clone(), path)
    }

    /// The one constructor: the kind decides how `path` is kept (see the type's
    /// note on rclone paths).
    fn of(kind: ServiceKind, service: impl Into<String>, path: impl Into<String>) -> VfsPath {
        let path = path.into();
        let path = match kind {
            ServiceKind::Sftp => path,
            ServiceKind::Rclone => path.trim_matches('/').to_string(),
        };
        VfsPath {
            kind,
            service: service.into(),
            path,
        }
    }

    /// The service's root, on the same service.
    pub fn service_root(&self) -> VfsPath {
        VfsPath::of(self.kind, &self.service, "")
    }

    /// Parse an `sftp://service/path` or `rclone://service/path` URL. `None`
    /// if it is neither — which is how callers ask "is this path remote at
    /// all?".
    pub fn parse(url: &str) -> Option<VfsPath> {
        let (kind, rest) = if let Some(rest) = url.strip_prefix(URL_SCHEME) {
            (ServiceKind::Sftp, rest)
        } else {
            (ServiceKind::Rclone, url.strip_prefix(RCLONE_URL_SCHEME)?)
        };
        let (service, path) = match rest.find('/') {
            Some(slash) => (&rest[..slash], &rest[slash..]),
            None => (rest, ""),
        };
        if service.is_empty() {
            return None;
        }
        Some(VfsPath::of(
            kind,
            service,
            // A bare `/` is the root, same as no path at all; storing them
            // identically keeps `parse(to_url(p)) == p` honest.
            if path == "/" { "" } else { path },
        ))
    }

    /// The URL form, which is what [`Entry::path`] carries for a remote row.
    pub fn to_url(&self) -> String {
        let scheme = self.kind.scheme();
        let path = self.path.trim_end_matches('/');
        if path.is_empty() {
            format!("{scheme}{}", self.service)
        } else if path.starts_with('/') {
            format!("{scheme}{}{path}", self.service)
        } else {
            format!("{scheme}{}/{path}", self.service)
        }
    }

    /// A child of this path.
    pub fn join(&self, name: &str) -> VfsPath {
        let base = self.path.trim_end_matches('/');
        if base.is_empty() {
            // Children of the root are relative paths — resolved against the
            // login directory, which is what the root *is*.
            VfsPath::of(self.kind, &self.service, name)
        } else {
            VfsPath::of(self.kind, &self.service, format!("{base}/{name}"))
        }
    }

    /// The containing directory, or `None` at the service root.
    pub fn parent(&self) -> Option<VfsPath> {
        let trimmed = self.path.trim_end_matches('/');
        if trimmed.is_empty() {
            return None;
        }
        match trimmed.rsplit_once('/') {
            Some(("", _)) => Some(VfsPath::of(self.kind, &self.service, "/")),
            Some((parent, _)) => Some(VfsPath::of(self.kind, &self.service, parent)),
            None => Some(self.service_root()),
        }
    }

    /// The last component, or the service name at the root.
    pub fn name(&self) -> &str {
        let trimmed = self.path.trim_end_matches('/');
        match trimmed.rsplit_once('/') {
            Some((_, name)) => name,
            None if trimmed.is_empty() => &self.service,
            None => trimmed,
        }
    }
}

impl std::fmt::Display for VfsPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_url())
    }
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Everything the vfs can fail at, each with the words a toast needs.
///
/// The variants sort into two piles the manager cares about: connection-fatal
/// ones (the session is unusable — kill the child, reconnect on the next
/// request) and per-operation ones (`Status`, `Io`, `Cancelled` — the session
/// is fine, this request was not). [`VfsError::is_connection_fatal`] is that
/// sorting, in one place, so the reconnect rule cannot drift per call site.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    /// The URL named a service neither `vfs.toml` nor `rclone.conf` defines.
    #[error("no service named \"{service}\" in vfs.toml or rclone.conf")]
    UnknownService { service: String },

    /// `ssh`, `rclone` (or the configured program, or the worker thread
    /// itself) would not start at all. `program` is what was being started,
    /// in the words the sentence needs.
    #[error("{service}: could not start {program}: {source}")]
    Spawn {
        service: String,
        program: String,
        #[source]
        source: std::io::Error,
    },

    /// An operation this kind of service has no way to perform — a symlink
    /// in a Google Drive, a `chmod` on an S3 bucket. Not a failure of the
    /// connection, and not something a retry could change.
    #[error("{service}: {op} is not something rclone can do")]
    Unsupported { service: String, op: &'static str },

    /// `ssh` could not log in — keys, agent, host key. The detail is ssh's own
    /// stderr, which names the actual reason ("Permission denied (publickey)").
    #[error("{service}: cannot log in: {detail}")]
    Auth { service: String, detail: String },

    /// The connection ended. The detail is whatever ssh said on the way out,
    /// or "the connection closed" when it said nothing.
    #[error("{service}: {detail}")]
    Disconnected { service: String, detail: String },

    /// The server went silent past the deadline.
    #[error("{service}: {op} timed out after {}s", timeout.as_secs())]
    Timeout {
        service: String,
        op: &'static str,
        timeout: Duration,
    },

    /// An io error on the pipes themselves.
    #[error("{service}: {source}")]
    Transport {
        service: String,
        #[source]
        source: std::io::Error,
    },

    /// The server sent something that is not SFTP. Always a hang-up: once the
    /// framing has desynchronised there is nothing left to resynchronise by.
    #[error("{service}: protocol error: {source}")]
    Protocol {
        service: String,
        #[source]
        source: wire::ProtocolError,
    },

    /// A well-formed reply of a type that cannot answer what was asked.
    #[error("{service}: the server sent {got} where {expected} was expected")]
    Unexpected {
        service: String,
        expected: &'static str,
        got: &'static str,
    },

    /// The server said no. The status carries the server's own words when it
    /// sent any, which beat the code's category every time.
    #[error("{path}: {status}")]
    Status { path: String, status: wire::Status },

    /// A *local* io failure — the tempfile of a download, the source of an
    /// upload.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The user stopped it. Not a failure; callers stay quiet about it.
    #[error("cancelled")]
    Cancelled,

    /// The vfs is shutting down and the request has nowhere to run.
    #[error("the vfs is shutting down")]
    Closed,
}

impl VfsError {
    /// Whether the connection this error came off is still usable. See the
    /// enum's documentation for why this lives in one place.
    pub fn is_connection_fatal(&self) -> bool {
        !matches!(
            self,
            VfsError::Status { .. }
                | VfsError::Io { .. }
                | VfsError::Cancelled
                | VfsError::UnknownService { .. }
                | VfsError::Unsupported { .. }
        )
    }
}

/// The bridge into [`crate::tasks`]: a vfs job returns [`crate::Result`], so
/// the engine's cancel/retry classification keeps working. `Cancelled` maps to
/// `Cancelled` (the engine treats it as "you stopped it", not "it broke") and
/// a local io failure keeps its path; everything else becomes an op error
/// carrying the sentence the variant already composed.
impl From<VfsError> for crate::DfError {
    fn from(e: VfsError) -> crate::DfError {
        match e {
            VfsError::Cancelled => crate::DfError::Cancelled,
            VfsError::Io { path, source } => crate::DfError::Io { path, source },
            other => crate::DfError::Op(other.to_string()),
        }
    }
}

// ── Listing updates ─────────────────────────────────────────────────────────

/// Identifies one remote listing. Monotonic per [`Vfs`], never reused — the
/// same staleness contract as [`crate::fs::ScanToken`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VfsToken(pub u64);

/// What a listing sends back. The remote mirror of [`crate::fs::ScanUpdate`],
/// with a [`VfsPath`] where the local one has a `PathBuf`.
///
/// Batch size is the server's own `READDIR` granularity (OpenSSH sends up to
/// 100 entries per reply) rather than a re-buffered constant: each reply is
/// already one round trip's worth, which is the natural "paint something now"
/// unit on a link with real latency. rclone answers a listing whole, so an
/// rclone service cuts it into `rclone::LIST_BATCH`-row pieces instead.
#[derive(Debug)]
pub enum VfsUpdate {
    /// The directory opened on the server. Sent before any entries, so the
    /// pane can clear its old listing exactly when a new one is coming.
    Started { token: VfsToken, dir: VfsPath },
    /// Some entries, in server order (sorting is the model's job). `Entry.path`
    /// is the `sftp://…` or `rclone://…` URL of the row.
    Batch {
        token: VfsToken,
        dir: VfsPath,
        entries: Vec<Entry>,
    },
    /// Every entry has been sent.
    Done {
        token: VfsToken,
        dir: VfsPath,
        total: usize,
    },
    /// The listing failed — could not connect, could not open, lost the link.
    Failed {
        token: VfsToken,
        dir: VfsPath,
        error: VfsError,
    },
}

impl VfsUpdate {
    pub fn token(&self) -> VfsToken {
        match self {
            VfsUpdate::Started { token, .. }
            | VfsUpdate::Batch { token, .. }
            | VfsUpdate::Done { token, .. }
            | VfsUpdate::Failed { token, .. } => *token,
        }
    }

    pub fn dir(&self) -> &VfsPath {
        match self {
            VfsUpdate::Started { dir, .. }
            | VfsUpdate::Batch { dir, .. }
            | VfsUpdate::Done { dir, .. }
            | VfsUpdate::Failed { dir, .. } => dir,
        }
    }
}

// ── The manager ─────────────────────────────────────────────────────────────

/// What a worker is asked to do.
enum Cmd {
    List {
        token: VfsToken,
        dir: VfsPath,
    },
    Op {
        op: Op,
        ctx: TaskCtx,
        reply: Sender<Result<Outcome, VfsError>>,
    },
}

/// One blocking operation. The variants are exactly the SFTP verbs PLAN §7.6
/// needs; each maps onto one [`Connection`] method.
enum Op {
    Stat { path: VfsPath, follow: bool },
    RealPath { path: VfsPath },
    Mkdir { path: VfsPath },
    Rmdir { path: VfsPath },
    Remove { path: VfsPath },
    Rename { from: VfsPath, to: VfsPath },
    ReadLink { path: VfsPath },
    Symlink { target: String, link: VfsPath },
    Chmod { path: VfsPath, mode: u32 },
    Download { remote: VfsPath, local: PathBuf },
    Upload { local: PathBuf, remote: VfsPath },
}

enum Outcome {
    Unit,
    Attrs(wire::Attrs),
    Text(String),
    Bytes(u64),
}

/// Which listings are still wanted — the cancel set, shared with the workers.
type Live = Arc<Mutex<HashMap<VfsToken, VfsPath>>>;

struct Worker {
    sender: Sender<Cmd>,
    handle: std::thread::JoinHandle<()>,
}

/// The connection manager: services from `vfs.toml`, one worker thread (and
/// so one `ssh`) per service, spawned lazily on first use.
///
/// Dropping it closes every command channel and joins every worker; each
/// worker's connection drop kills and reaps its child. No zombies, ever.
pub struct Vfs {
    config: VfsConfig,
    warnings: Vec<ConfigWarning>,
    notify: Notifier,
    updates_tx: Sender<VfsUpdate>,
    updates_rx: Receiver<VfsUpdate>,
    live: Live,
    next_token: AtomicU64,
    workers: Mutex<HashMap<String, Worker>>,
}

impl Vfs {
    /// Load `vfs.toml` (yazi's, then delightfile's) and stand up the manager.
    /// No connection is made until a request needs one.
    pub fn start(notify: Notifier) -> Vfs {
        let (config, warnings) = VfsConfig::load();
        Vfs::with_config(config, warnings, notify)
    }

    /// A manager over an explicit config — the tests' front door, and the seam
    /// a picker that edits services live would use.
    pub fn with_config(config: VfsConfig, warnings: Vec<ConfigWarning>, notify: Notifier) -> Vfs {
        let (updates_tx, updates_rx) = unbounded();
        Vfs {
            config,
            warnings,
            notify,
            updates_tx,
            updates_rx,
            live: Arc::new(Mutex::new(HashMap::new())),
            next_token: AtomicU64::new(1),
            workers: Mutex::new(HashMap::new()),
        }
    }

    /// The configured services, in file order — what `g 1`/`g 2` index and
    /// what a service picker lists.
    pub fn services(&self) -> &[Service] {
        &self.config.services
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.config.service(name)
    }

    /// What `vfs.toml` said that could not be understood. Surfaced once at
    /// startup, the same way the main config's warnings are.
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    // ── Listing ─────────────────────────────────────────────────────────────

    /// Queue a listing and return its token. Any in-flight listing of the same
    /// directory is cancelled first — a rescan is exactly that.
    pub fn scan(&self, dir: VfsPath) -> VfsToken {
        let token = VfsToken(self.next_token.fetch_add(1, Ordering::Relaxed));
        {
            let mut live = lock(&self.live);
            live.retain(|_, d| *d != dir);
            live.insert(token, dir.clone());
        }
        if let Err(error) = self.dispatch(
            &dir.service.clone(),
            Cmd::List {
                token,
                dir: dir.clone(),
            },
        ) {
            lock(&self.live).remove(&token);
            let _ignored = self
                .updates_tx
                .send(VfsUpdate::Failed { token, dir, error });
            (self.notify)();
        }
        token
    }

    /// Stop a listing. Batches already in the channel are dropped by token;
    /// no further round trips are spent on it.
    pub fn cancel(&self, token: VfsToken) {
        lock(&self.live).remove(&token);
    }

    pub fn cancel_all(&self) {
        lock(&self.live).clear();
    }

    pub fn is_live(&self, token: VfsToken) -> bool {
        lock(&self.live).contains_key(&token)
    }

    /// The update channel, for a caller that selects on it.
    pub fn updates(&self) -> &Receiver<VfsUpdate> {
        &self.updates_rx
    }

    /// Everything that has arrived, without blocking — what the app calls when
    /// its `Wake` event fires.
    pub fn drain(&self) -> Vec<VfsUpdate> {
        self.updates_rx.try_iter().collect()
    }

    // ── Operations ──────────────────────────────────────────────────────────
    //
    // All blocking, all meant to run inside a task-engine job (each takes the
    // job's `TaskCtx` and honours cancel), all serialised per service behind
    // that service's single connection.

    /// `STAT` (follow symlinks) or `LSTAT` (do not).
    pub fn stat(&self, path: &VfsPath, follow: bool, ctx: &TaskCtx) -> Result<Attrs, VfsError> {
        match self.op(
            &path.service,
            Op::Stat {
                path: path.clone(),
                follow,
            },
            ctx,
        )? {
            Outcome::Attrs(attrs) => Ok(attrs),
            _ => Err(VfsError::Closed),
        }
    }

    /// The server-side canonical form of a path.
    pub fn realpath(&self, path: &VfsPath, ctx: &TaskCtx) -> Result<String, VfsError> {
        match self.op(&path.service, Op::RealPath { path: path.clone() }, ctx)? {
            Outcome::Text(text) => Ok(text),
            _ => Err(VfsError::Closed),
        }
    }

    pub fn mkdir(&self, path: &VfsPath, ctx: &TaskCtx) -> Result<(), VfsError> {
        self.unit(&path.service, Op::Mkdir { path: path.clone() }, ctx)
    }

    pub fn rmdir(&self, path: &VfsPath, ctx: &TaskCtx) -> Result<(), VfsError> {
        self.unit(&path.service, Op::Rmdir { path: path.clone() }, ctx)
    }

    pub fn remove(&self, path: &VfsPath, ctx: &TaskCtx) -> Result<(), VfsError> {
        self.unit(&path.service, Op::Remove { path: path.clone() }, ctx)
    }

    /// Rename within one service. Cross-service moves are a download plus an
    /// upload and belong to the caller, which is why `to.service` must match.
    pub fn rename(&self, from: &VfsPath, to: &VfsPath, ctx: &TaskCtx) -> Result<(), VfsError> {
        if from.service != to.service {
            return Err(VfsError::UnknownService {
                service: to.service.clone(),
            });
        }
        self.unit(
            &from.service,
            Op::Rename {
                from: from.clone(),
                to: to.clone(),
            },
            ctx,
        )
    }

    pub fn readlink(&self, path: &VfsPath, ctx: &TaskCtx) -> Result<String, VfsError> {
        match self.op(&path.service, Op::ReadLink { path: path.clone() }, ctx)? {
            Outcome::Text(text) => Ok(text),
            _ => Err(VfsError::Closed),
        }
    }

    /// Create `link` pointing at `target` (a server-side path, possibly
    /// relative — handed to the server verbatim).
    pub fn symlink(&self, target: &str, link: &VfsPath, ctx: &TaskCtx) -> Result<(), VfsError> {
        self.unit(
            &link.service,
            Op::Symlink {
                target: target.to_string(),
                link: link.clone(),
            },
            ctx,
        )
    }

    /// `chmod`, as the `SETSTAT` it is on the wire.
    pub fn chmod(&self, path: &VfsPath, mode: u32, ctx: &TaskCtx) -> Result<(), VfsError> {
        self.unit(
            &path.service,
            Op::Chmod {
                path: path.clone(),
                mode,
            },
            ctx,
        )
    }

    /// Download `remote` to `local`, pipelined, with progress on `ctx`.
    /// Returns the byte count. A failed or cancelled download removes its
    /// partial local file.
    pub fn download(&self, remote: &VfsPath, local: &Path, ctx: &TaskCtx) -> Result<u64, VfsError> {
        match self.op(
            &remote.service,
            Op::Download {
                remote: remote.clone(),
                local: local.to_path_buf(),
            },
            ctx,
        )? {
            Outcome::Bytes(n) => Ok(n),
            _ => Err(VfsError::Closed),
        }
    }

    /// Download to a fresh file under the temp directory — what
    /// download-on-open and remote previews use. Returns the local path.
    ///
    /// The name keeps the remote file's own name (uniquified by a counter),
    /// because the tempfile is about to be handed to an opener that sniffs
    /// extensions.
    pub fn download_to_temp(&self, remote: &VfsPath, ctx: &TaskCtx) -> Result<PathBuf, VfsError> {
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("delightfile-vfs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| VfsError::Io {
            path: dir.clone(),
            source: e,
        })?;
        let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let local = dir.join(format!("{unique}-{}", remote.name()));
        self.download(remote, &local, ctx)?;
        Ok(local)
    }

    /// Does `path` exist on the server?
    ///
    /// A `NO_SUCH_FILE` status is an *answer*, not a failure — everything else
    /// still is, so a permission-denied on the parent directory can never be
    /// read as "there is nothing there" by a caller about to write.
    pub fn exists(&self, path: &VfsPath, ctx: &TaskCtx) -> Result<bool, VfsError> {
        match self.stat(path, false, ctx) {
            Ok(_) => Ok(true),
            Err(VfsError::Status { status, .. }) if status.code == StatusCode::NoSuchFile => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// The first free `name`, `name_1`, `name_2`… beside `remote` — the
    /// server-side twin of [`crate::ops::paste::unique_name`].
    ///
    /// One `LSTAT` per candidate, which is the only way to ask: there is no
    /// atomic "create if absent" in SFTP version 3 that also gives us the
    /// pipelined write path, so this is a *ladder*, not a lock. The window it
    /// leaves is closed the only way it can be — by being re-run immediately
    /// before the transfer rather than once at plan time (see
    /// [`Vfs::upload_new`]).
    pub fn unique_name(&self, remote: &VfsPath, ctx: &TaskCtx) -> Result<VfsPath, VfsError> {
        if !self.exists(remote, ctx)? {
            return Ok(remote.clone());
        }
        let parent = remote.parent().unwrap_or_else(|| remote.service_root());
        let name = std::ffi::OsString::from(remote.name());
        for n in 1..crate::fs::names::MAX_TRASH_COLLISIONS {
            let candidate = parent.join(&crate::fs::names::suffixed(&name, n).to_string_lossy());
            if !self.exists(&candidate, ctx)? {
                return Ok(candidate);
            }
        }
        Err(VfsError::Status {
            path: remote.to_url(),
            status: wire::Status {
                code: StatusCode::Failure,
                message: format!("no free name beside {}", remote.to_url()),
            },
        })
    }

    /// Upload `local` beside `remote` **without ever replacing anything**, and
    /// say what it was actually called.
    ///
    /// The download half has always claimed its local name through
    /// [`crate::ops::paste::unique_name`] so that a download cannot silently
    /// overwrite (PLAN §5); this is the same promise pointing the other way. An
    /// upload that means to replace a file says so by calling [`Vfs::upload`]
    /// directly — which is one grep for every call site that can destroy
    /// somebody's file.
    pub fn upload_new(
        &self,
        local: &Path,
        remote: &VfsPath,
        ctx: &TaskCtx,
    ) -> Result<(u64, VfsPath), VfsError> {
        let target = self.unique_name(remote, ctx)?;
        let bytes = self.upload(local, &target, ctx)?;
        Ok((bytes, target))
    }

    /// Upload `local` to `remote`, pipelined, with progress on `ctx`. Returns
    /// the byte count.
    ///
    /// **This replaces whatever is at `remote`.** It is the deliberate
    /// overwrite; the answer to "is there something there?" belongs to the
    /// caller, and [`Vfs::upload_new`] is the one that refuses to destroy.
    ///
    /// The bytes go to a sibling scratch file and are renamed into place at the
    /// end, so a failed or cancelled upload removes only its own partial file
    /// and leaves whatever was at `remote` exactly as it was.
    pub fn upload(&self, local: &Path, remote: &VfsPath, ctx: &TaskCtx) -> Result<u64, VfsError> {
        match self.op(
            &remote.service,
            Op::Upload {
                local: local.to_path_buf(),
                remote: remote.clone(),
            },
            ctx,
        )? {
            Outcome::Bytes(n) => Ok(n),
            _ => Err(VfsError::Closed),
        }
    }

    // ── Plumbing ────────────────────────────────────────────────────────────

    fn unit(&self, service: &str, op: Op, ctx: &TaskCtx) -> Result<(), VfsError> {
        self.op(service, op, ctx).map(|_| ())
    }

    /// Send one operation to its service's worker and wait for the answer.
    fn op(&self, service: &str, op: Op, ctx: &TaskCtx) -> Result<Outcome, VfsError> {
        // Rendezvous of one: the worker's send never blocks, and a reply to a
        // caller that gave up (cannot happen today, but `recv` below is the
        // only wait) would be dropped rather than queued forever.
        let (reply, answer) = crossbeam_channel::bounded(1);
        self.dispatch(
            service,
            Cmd::Op {
                op,
                ctx: ctx.clone(),
                reply,
            },
        )?;
        // A dead worker dropped its receiver and every queued reply sender
        // with it; `Closed` is the honest name for that.
        answer.recv().map_err(|_| VfsError::Closed)?
    }

    /// Hand a command to the service's worker, spawning it on first use.
    fn dispatch(&self, service: &str, cmd: Cmd) -> Result<(), VfsError> {
        let Some(config) = self.config.service(service) else {
            return Err(VfsError::UnknownService {
                service: service.to_string(),
            });
        };
        let mut workers = lock(&self.workers);
        if let Some(worker) = workers.get(service) {
            if worker.sender.send(cmd).is_ok() {
                return Ok(());
            }
            // The worker's loop ended (it cannot panic out — every operation
            // returns its errors — but belt and braces): replace it.
            workers.remove(service);
            return Err(VfsError::Closed);
        }

        let service_config = Arc::new(config.clone());
        let (sender, receiver) = unbounded::<Cmd>();
        let updates = self.updates_tx.clone();
        let live = Arc::clone(&self.live);
        let notify = Arc::clone(&self.notify);
        let spawned = std::thread::Builder::new()
            .name(format!("df-vfs-{service}"))
            .spawn(move || worker_loop(service_config, receiver, updates, live, notify));
        match spawned {
            Ok(handle) => {
                let worker = Worker { sender, handle };
                let sent = worker.sender.send(cmd).is_ok();
                workers.insert(service.to_string(), worker);
                if sent {
                    Ok(())
                } else {
                    Err(VfsError::Closed)
                }
            }
            Err(e) => Err(VfsError::Spawn {
                service: service.to_string(),
                program: "its worker thread".to_string(),
                source: e,
            }),
        }
    }
}

impl Drop for Vfs {
    fn drop(&mut self) {
        self.cancel_all();
        let workers: Vec<Worker> = {
            let mut map = lock(&self.workers);
            map.drain().map(|(_, w)| w).collect()
        };
        // Two passes: close every channel first, then join, so the workers
        // wind down in parallel instead of one ssh teardown at a time.
        let handles: Vec<_> = workers
            .into_iter()
            .map(|w| {
                drop(w.sender);
                w.handle
            })
            .collect();
        for handle in handles {
            let _joined = handle.join();
        }
    }
}

/// Lock without panicking on poison — same reasoning as `fs::scan::lock`: the
/// data is a plain map with no invariant a panic could half-apply.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ── The worker ──────────────────────────────────────────────────────────────

/// What a worker holds while its service is reachable: an SFTP session on an
/// `ssh` child, or an `rclone rcd` child and the socket it answers on.
///
/// Both are "the connection" as far as the worker is concerned — made lazily,
/// dropped on a connection-fatal error, remade on the next command — and both
/// kill and reap their child when dropped.
enum Backend {
    Sftp(Connection),
    Rclone(rclone::Daemon),
}

/// One service's thread: owns the connection (and so the `ssh` or `rclone`
/// child), runs commands in arrival order, reconnects lazily after any
/// connection-fatal error. Ends when the command channel closes; ending drops
/// the connection, which kills and reaps the child.
fn worker_loop(
    service: Arc<Service>,
    commands: Receiver<Cmd>,
    updates: Sender<VfsUpdate>,
    live: Live,
    notify: Notifier,
) {
    let mut connection: Option<Backend> = None;

    for cmd in commands {
        match cmd {
            Cmd::List { token, dir } => {
                // Cancelled while queued — the common case when arrowing
                // through remote directories quickly.
                if !lock(&live).contains_key(&token) {
                    continue;
                }
                let conn = match ensure_connected(&mut connection, &service) {
                    Ok(conn) => conn,
                    Err(error) => {
                        lock(&live).remove(&token);
                        let sent = updates.send(VfsUpdate::Failed { token, dir, error });
                        if sent.is_ok() {
                            notify();
                        }
                        continue;
                    }
                };
                let fatal = match conn {
                    Backend::Sftp(conn) => run_list(conn, token, &dir, &updates, &live, &notify),
                    Backend::Rclone(daemon) => {
                        run_rclone_list(daemon, token, &dir, &updates, &live, &notify)
                    }
                };
                if fatal {
                    connection = None;
                }
            }
            Cmd::Op { op, ctx, reply } => {
                if ctx.is_cancelled() {
                    let _ignored = reply.send(Err(VfsError::Cancelled));
                    continue;
                }
                let outcome = match ensure_connected(&mut connection, &service) {
                    Ok(Backend::Sftp(conn)) => run_op(conn, op, &ctx),
                    Ok(Backend::Rclone(daemon)) => daemon.run_op(op, &ctx),
                    Err(e) => Err(e),
                };
                if outcome
                    .as_ref()
                    .err()
                    .is_some_and(VfsError::is_connection_fatal)
                {
                    // Tear down now rather than reuse a desynchronised stream;
                    // the next command reconnects.
                    connection = None;
                }
                let _ignored = reply.send(outcome);
            }
        }
    }
}

/// The lazy connect. On success the existing or fresh connection; on failure
/// the classified error (auth vs. unreachable — see `conn`; not installed vs.
/// would not start — see `rclone`).
fn ensure_connected<'a>(
    connection: &'a mut Option<Backend>,
    service: &Arc<Service>,
) -> Result<&'a mut Backend, VfsError> {
    if connection.is_none() {
        log::info!("vfs {}: connecting", service.name);
        *connection = Some(match service.kind {
            ServiceKind::Sftp => Backend::Sftp(Connection::connect(Arc::clone(service))?),
            ServiceKind::Rclone => {
                Backend::Rclone(rclone::Daemon::spawn(Arc::clone(service), &[])?)
            }
        });
    }
    match connection.as_mut() {
        Some(conn) => Ok(conn),
        // Unreachable — the line above filled it — but a panic here would take
        // the worker with it.
        None => Err(VfsError::Closed),
    }
}

fn run_op(conn: &mut Connection, op: Op, ctx: &TaskCtx) -> Result<Outcome, VfsError> {
    match op {
        Op::Stat { path, follow } => conn.stat(&path, follow).map(Outcome::Attrs),
        Op::RealPath { path } => conn.realpath(&path).map(Outcome::Text),
        Op::Mkdir { path } => conn.mkdir(&path).map(|()| Outcome::Unit),
        Op::Rmdir { path } => conn.rmdir(&path).map(|()| Outcome::Unit),
        Op::Remove { path } => conn.remove(&path).map(|()| Outcome::Unit),
        Op::Rename { from, to } => conn.rename(&from, &to).map(|()| Outcome::Unit),
        Op::ReadLink { path } => conn.readlink(&path).map(Outcome::Text),
        Op::Symlink { target, link } => conn.symlink(&target, &link).map(|()| Outcome::Unit),
        Op::Chmod { path, mode } => conn.chmod(&path, mode).map(|()| Outcome::Unit),
        Op::Download { remote, local } => conn.download(&remote, &local, ctx).map(Outcome::Bytes),
        Op::Upload { local, remote } => conn.upload(&local, &remote, ctx).map(Outcome::Bytes),
    }
}

/// Run one listing to completion (or cancellation, or failure). Returns
/// whether the connection must be torn down.
fn run_list(
    conn: &mut Connection,
    token: VfsToken,
    dir: &VfsPath,
    updates: &Sender<VfsUpdate>,
    live: &Live,
    notify: &Notifier,
) -> bool {
    let send = |update: VfsUpdate| {
        if updates.send(update).is_ok() {
            notify();
        }
    };
    let still_wanted = || lock(live).contains_key(&token);

    let handle = match conn.opendir(dir) {
        Ok(handle) => handle,
        Err(error) => {
            let fatal = error.is_connection_fatal();
            lock(live).remove(&token);
            send(VfsUpdate::Failed {
                token,
                dir: dir.clone(),
                error,
            });
            return fatal;
        }
    };

    send(VfsUpdate::Started {
        token,
        dir: dir.clone(),
    });

    let mut total = 0usize;
    // The stat budget for symlink targets, shared across the whole listing —
    // see `MAX_LINK_RESOLVES` for the arithmetic.
    let mut link_budget = MAX_LINK_RESOLVES;

    loop {
        if !still_wanted() {
            conn.close_quietly(&handle);
            return false;
        }
        let batch = match conn.readdir(dir, &handle) {
            Ok(Some(entries)) => entries,
            Ok(None) => break,
            Err(error) => {
                let fatal = error.is_connection_fatal();
                if !fatal {
                    conn.close_quietly(&handle);
                }
                lock(live).remove(&token);
                send(VfsUpdate::Failed {
                    token,
                    dir: dir.clone(),
                    error,
                });
                return fatal;
            }
        };

        let entries = match convert_batch(conn, dir, batch, &mut link_budget) {
            Ok(entries) => entries,
            Err(error) => {
                // `convert_batch` only fails on the pipelined stats, which are
                // connection-level failures.
                lock(live).remove(&token);
                send(VfsUpdate::Failed {
                    token,
                    dir: dir.clone(),
                    error,
                });
                return true;
            }
        };
        if entries.is_empty() {
            continue;
        }
        total += entries.len();
        send(VfsUpdate::Batch {
            token,
            dir: dir.clone(),
            entries,
        });
    }

    conn.close_quietly(&handle);
    lock(live).remove(&token);
    send(VfsUpdate::Done {
        token,
        dir: dir.clone(),
        total,
    });
    false
}

/// [`run_list`] for an rclone service. Returns whether the daemon must be
/// dropped.
///
/// rclone answers a listing in one reply rather than a `READDIR` at a time, so
/// the batching the pane expects is done here: [`rclone::LIST_BATCH`] rows per
/// update, with the same "is this still wanted?" check between batches that
/// the SFTP loop makes between round trips. `Started` goes out only once the
/// listing is in hand, so a directory that cannot be read ends in `Failed`
/// without ever having cleared the pane. The same check is made while rclone is
/// still paging the folder, on every poll of its job, so a listing abandoned
/// before rclone has finished is stopped rather than waited out.
fn run_rclone_list(
    daemon: &mut rclone::Daemon,
    token: VfsToken,
    dir: &VfsPath,
    updates: &Sender<VfsUpdate>,
    live: &Live,
    notify: &Notifier,
) -> bool {
    let send = |update: VfsUpdate| {
        if updates.send(update).is_ok() {
            notify();
        }
    };
    let still_wanted = || lock(live).contains_key(&token);

    let mut rest = match daemon.list(dir, &still_wanted) {
        Ok(Some(entries)) => entries,
        // Nobody wants it any more, and rclone has been told to stop: the
        // same quiet end the SFTP loop takes between round trips.
        Ok(None) => return false,
        Err(error) => {
            let fatal = error.is_connection_fatal();
            lock(live).remove(&token);
            send(VfsUpdate::Failed {
                token,
                dir: dir.clone(),
                error,
            });
            return fatal;
        }
    };

    send(VfsUpdate::Started {
        token,
        dir: dir.clone(),
    });
    let mut total = 0usize;
    while !rest.is_empty() {
        if !still_wanted() {
            return false;
        }
        let tail = rest.split_off(rest.len().min(rclone::LIST_BATCH));
        let entries = std::mem::replace(&mut rest, tail);
        total += entries.len();
        send(VfsUpdate::Batch {
            token,
            dir: dir.clone(),
            entries,
        });
    }
    lock(live).remove(&token);
    send(VfsUpdate::Done {
        token,
        dir: dir.clone(),
        total,
    });
    false
}

/// Turn one `READDIR` reply into rows, resolving symlink targets with
/// pipelined `STAT`s while the budget lasts.
fn convert_batch(
    conn: &mut Connection,
    dir: &VfsPath,
    batch: Vec<wire::NameEntry>,
    link_budget: &mut usize,
) -> Result<Vec<Entry>, VfsError> {
    // `.` and `..` are rows in SFTP and are not rows in delightfile — the
    // local scanner never sees them and the sort/cursor code must not either.
    let batch: Vec<wire::NameEntry> = batch
        .into_iter()
        .filter(|e| e.filename != b"." && e.filename != b"..")
        .collect();

    // Which rows are symlinks needing one extra round trip to learn what they
    // point at (a READDIR reply carries the *link's* attributes).
    let mut lookups: Vec<(usize, VfsPath)> = Vec::new();
    for (index, entry) in batch.iter().enumerate() {
        let is_link = entry.attrs.is_symlink()
            || (entry.attrs.permissions.is_none() && entry.longname_type() == Some('l'));
        if is_link && *link_budget > 0 {
            lookups.push((index, dir.join(&entry.name_lossy())));
            *link_budget -= 1;
        }
    }
    let mut resolved: HashMap<usize, Option<wire::Attrs>> = HashMap::new();
    if !lookups.is_empty() {
        let paths: Vec<VfsPath> = lookups.iter().map(|(_, p)| p.clone()).collect();
        let attrs = conn.stat_many(&paths)?;
        for ((index, _), target) in lookups.into_iter().zip(attrs) {
            resolved.insert(index, target);
        }
    }

    Ok(batch
        .iter()
        .enumerate()
        .map(|(index, entry)| remote_entry(dir, entry, resolved.get(&index)))
        .collect())
}

/// One `STAT`ed remote path as the [`Entry`] the panes and the dialogs draw.
///
/// The listing path builds its rows from `READDIR` replies; this is the same
/// mapping for a path that was asked about one at a time — what a conflict card
/// needs to say "this is what is already on the server", with the size, the
/// kind and the date coming off the wire rather than from a local `stat` that
/// would find nothing at all.
pub fn stat_entry(path: &VfsPath, attrs: Attrs) -> Entry {
    let parent = path.parent().unwrap_or_else(|| path.service_root());
    let entry = wire::NameEntry {
        filename: path.name().as_bytes().to_vec(),
        longname: Vec::new(),
        attrs,
    };
    remote_entry(&parent, &entry, None)
}

/// One remote row as the [`Entry`] the panes already know how to draw.
///
/// `resolved` is the symlink target's attrs: `Some(Some(_))` resolved,
/// `Some(None)` broken, `None` not a link (or the budget ran out, in which
/// case the link is shown as a link to a file — see [`MAX_LINK_RESOLVES`]).
fn remote_entry(dir: &VfsPath, entry: &wire::NameEntry, resolved: Option<&Option<Attrs>>) -> Entry {
    let name = entry.name_lossy();
    let attrs = entry.attrs;
    let is_link =
        attrs.is_symlink() || (attrs.permissions.is_none() && entry.longname_type() == Some('l'));
    // The longname's first column is the fallback for a server that sent no
    // permission bits — one character of `ls -l` beats guessing "file".
    let is_dir =
        attrs.is_dir() || (attrs.permissions.is_none() && entry.longname_type() == Some('d'));

    let kind = if is_link {
        let target = resolved.and_then(|r| r.as_ref()).map(|target_attrs| {
            if target_attrs.is_dir() {
                LinkTarget::Dir
            } else if target_attrs.is_file() {
                LinkTarget::File
            } else {
                LinkTarget::Other
            }
        });
        // No stat (budget, or not looked up): a link to a file — visible,
        // sortable, openable-with-a-stat-later; never a guessed directory.
        Kind::Symlink { target }
    } else if is_dir {
        Kind::Dir
    } else {
        Kind::File
    };

    // For a resolving symlink the row describes the target, same as the local
    // scanner's stat-through-the-link.
    let shown = match (&kind, resolved) {
        (Kind::Symlink { target: Some(_) }, Some(Some(target_attrs))) => *target_attrs,
        _ => attrs,
    };

    let is_dir_row = matches!(
        kind,
        Kind::Dir
            | Kind::Symlink {
                target: Some(LinkTarget::Dir)
            }
    );
    let mime = if is_dir_row {
        mime::DIR_MIME
    } else if matches!(kind, Kind::Symlink { target: None }) {
        mime::BROKEN_LINK_MIME
    } else {
        mime::hint_for_name(&name)
    };

    Entry {
        is_hidden: name.starts_with('.'),
        path: PathBuf::from(dir.join(&name).to_url()),
        kind,
        // Zero for directories, matching the local scanner's "not known yet"
        // convention that the du walk fills in.
        len: if is_dir_row {
            0
        } else {
            shown.size.unwrap_or(0)
        },
        mtime: shown.mtime_system(),
        // v3 has no birth time at all; every remote row honestly has a hole
        // here, which the btime linemode and sort already tolerate.
        btime: None,
        mode: shown.permissions.unwrap_or(0),
        // OpenSSH always sends uid/gid; a server that does not shows as
        // root-owned, which `owner_label` renders numerically anyway since the
        // remote uid is not in the local passwd file.
        uid: shown.uid.unwrap_or(0),
        gid: shown.gid.unwrap_or(0),
        mime,
        file_kind: crate::fs::classify(kind, &name, mime, shown.permissions.unwrap_or(0)),
        name,
        tags: Vec::new(),
    }
}
