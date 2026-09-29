//! Cloud storage through `rclone rcd`: one daemon per service, spoken to over a
//! unix socket in rclone's remote-control API.
//!
//! ## The design in one sentence
//!
//! delightfile runs `rclone rcd` on a private socket and asks it to list, stat,
//! copy and move — so every provider rclone speaks (Google Drive, Dropbox, S3,
//! R2, OneDrive, WebDAV and forty more) is reachable, and every hard part of
//! reaching one — the OAuth dance, the token refresh, the multipart upload, the
//! provider's rate limits — belongs to rclone and to the remote the user set up
//! with `rclone config`. The app's connection *is* the user's `rclone config`,
//! so the app never holds a token, never sees a secret, and never has a cloud
//! SDK linked into it: the same bargain [`super`] strikes with `ssh`.
//!
//! ## The daemon
//!
//! [`Daemon::spawn`] runs
//!
//! ```text
//! rclone rcd --rc-addr unix://$XDG_RUNTIME_DIR/delightfile/rclone-<pid>-<service>-<n>.sock
//!            --rc-no-auth --rc-job-expire-duration 10m --ask-password=false
//! ```
//!
//! and waits for `rc/noop` to answer. The socket lives in a directory only this
//! user can enter (0700), which is what makes `--rc-no-auth` safe: the socket
//! *is* the authentication, the way a session bus's is. `--ask-password=false`
//! is `ssh`'s `BatchMode=yes` again — an encrypted `rclone.conf` with no
//! `RCLONE_CONFIG_PASS` fails at once with a sentence, rather than waiting on a
//! password prompt nobody will ever see. The finished-job expiry is raised
//! from rclone's one minute to ten so a transfer's final status is still there
//! to be read however long the app took to ask.
//!
//! stderr is read on a thread for the life of the daemon — rclone is a Go
//! program, and Go dies of `SIGPIPE` the first time it logs to a closed pipe —
//! and its last lines are what a failure to start reports.
//!
//! **The daemon cannot outlive the thread that owns it.** `ssh` exits when
//! delightfile's end of its stdin closes; `rclone rcd` serves a socket and
//! would not notice. So the child is started with a parent-death signal (see
//! `super::child`): if the worker thread that spawned it ends — including when
//! the whole process dies by `SIGKILL`, an abort or the OOM killer, where no
//! destructor runs — the kernel sends the daemon `SIGTERM`. An ordinary end
//! (the worker dropping the [`Daemon`]) sends the same `SIGTERM` itself and
//! waits for rclone to go, which is what lets rclone remove its `.partial`
//! files and its socket; `SIGKILL` is only for a daemon that has not gone
//! within [`STOP_GRACE`].
//!
//! ## One HTTP request per call
//!
//! [`Daemon::call`] is a `POST` of a JSON object to `/<method>`, answered with
//! a JSON object (see [`super::http`] and [`super::json`]). A 2xx is the
//! answer; anything else carries rclone's own `error` sentence, which becomes a
//! [`VfsError::Status`] — non-fatal, because rclone saying "directory not
//! empty" is rclone working. The not-found family maps to
//! [`StatusCode::NoSuchFile`], which is what keeps [`super::Vfs::exists`] and
//! the `name_1` ladder working here exactly as they do over SFTP. A socket that
//! stops answering, or a daemon that has exited, is
//! [`VfsError::Disconnected`]: fatal, so the worker drops the daemon and the
//! next command starts a fresh one.
//!
//! ## Transfers are jobs
//!
//! A copy can take an hour, and an HTTP request that stays open for an hour is
//! a request [`super::OP_TIMEOUT`] would rightly kill. So transfers run
//! `_async`: the call returns a job id at once, and [`Daemon::run_job`] polls
//! the job and its stats group every [`POLL`] — bytes into the task's
//! progress, `finished`/`success`/`error` into the result — and a cancel
//! becomes `job/stop`. rclone writes a download to a `.partial` name and
//! renames it into place, and removes the partial when a job is stopped, so a
//! cancelled transfer leaves the destination exactly as it was: the same
//! promise the SFTP upload's scratch file keeps, kept by rclone. A download
//! that fails some other way sweeps up any partial of its own that rclone did
//! not get to (see [`sweep_partials`]).
//!
//! **A listing is a job too.** Paging through a big cloud folder can take
//! rclone longer than [`super::OP_TIMEOUT`], and a synchronous `operations/list`
//! that ran past it would be a timeout — fatal, so the daemon would be killed
//! and the same listing would die the same way on every retry. As a job it has
//! no deadline of its own (each poll still has one), and it has the thing a
//! long listing needs instead: the pane's "is this still wanted?", checked on
//! every poll, and a `job/stop` when the answer is no.
//!
//! **Pause holds the polling, not the transfer.** rclone has no way to pause
//! one job, so a paused task stops reporting and rclone keeps moving bytes; the
//! bar catches up on resume. Cancel is immediate.
//!
//! ## What rclone cannot do
//!
//! A cloud remote has no symlinks, no permission bits and no canonical path,
//! so `realpath`, `readlink`, `symlink` and `chmod` answer
//! [`VfsError::Unsupported`]. A listing still gives every row a type and
//! permission word (`0o040755` or `0o100644`), because the panes sort and
//! classify by `st_mode` — but no owner, and no date that rclone did not send.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::child;
use super::config::Service;
use super::http::{self, HttpError};
use super::json::Json;
use super::wire::{self, Attrs, Status, StatusCode};
use super::{lock, remote_entry, Op, Outcome, VfsError, VfsPath, CONNECT_TIMEOUT, OP_TIMEOUT};
use crate::fs::Entry;
use crate::tasks::TaskCtx;

/// How many rows a listing update carries.
///
/// rclone hands a whole directory over in one reply, where SFTP's `READDIR`
/// arrives in server-sized pieces; this is the piece size put back, so a pane
/// paints the first screenful of a ten-thousand-file bucket without waiting
/// for the sort of all ten thousand. 256 is a few screenfuls: small enough to
/// paint at once, large enough that a big directory is not a thousand wakes.
pub const LIST_BATCH: usize = 256;

/// How often a running job is asked how it is doing.
///
/// 150 ms: under the ~200 ms at which a progress bar stops looking live, and
/// two small local requests per tick — nothing next to the transfer itself.
const POLL: Duration = Duration::from_millis(150);

/// How often a starting daemon is knocked on.
const READY_POLL: Duration = Duration::from_millis(20);

/// How long one readiness knock may wait for an answer. Short, so a socket
/// that accepted and then said nothing costs one knock, not the whole
/// [`CONNECT_TIMEOUT`].
const READY_ANSWER: Duration = Duration::from_secs(2);

/// How long a stopped job is given to wind down before `Cancelled` is
/// returned anyway — and how long a terminated daemon is given to exit before
/// it is killed.
///
/// Both wind-downs are where rclone removes its `.partial` files, so they are
/// waited for rather than fired and forgotten: a caller that looks at the
/// destination the moment it is told "cancelled" must find it as it was. Five
/// seconds is far past what either takes (tens of milliseconds for a job, two
/// hundred for the daemon, measured); it is a bound, not an estimate.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// How often a terminated daemon is checked for having exited.
const EXIT_POLL: Duration = Duration::from_millis(10);

/// How long a daemon that has exited is given to finish saying why.
///
/// The same race `conn`'s `STDERR_GRACE` covers: the exit and the last line
/// of stderr travel separately, and the line is the part worth reporting.
const STDERR_GRACE: Duration = Duration::from_millis(200);

/// How many of rclone's most recent stderr lines are kept, and how long each
/// may be. A daemon logs for as long as it runs, so this is a tail, not a
/// buffer — the *last* thing rclone said is the diagnosis.
const STDERR_LINES: usize = 32;
const STDERR_LINE_MAX: usize = 512;

/// The longest path a unix socket can be bound at: `sun_path` is 108 bytes
/// and the last is the NUL. A longer one fails in `bind` with "invalid
/// argument", which would be a baffling thing to show anybody.
const SUN_PATH_MAX: usize = 107;

/// What a directory row and a file row are given as `st_mode`.
const DIR_MODE: u32 = 0o040_755;
const FILE_MODE: u32 = 0o100_644;

/// The program run when the service names none.
const RCLONE: &str = "rclone";

/// The sentence for the one failure that is not rclone's to explain.
const NOT_INSTALLED: &str = "rclone is not installed";

/// A running `rclone rcd` for one service, and the client that talks to it.
///
/// Owned by the service's worker thread, exactly as an SFTP [`super::conn`]
/// connection is — and spawned on it, which is what ties the daemon's life to
/// that thread's (see the module note). Dropping it stops and reaps the daemon
/// and removes its socket.
pub(super) struct Daemon {
    service: Arc<Service>,
    /// [`Service::rclone_fs`], computed once.
    fs: String,
    child: Child,
    socket: PathBuf,
    /// The tail of rclone's stderr, filled by the reader thread.
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// The reader thread. Never joined: it ends when the daemon's stderr
    /// closes, and a drop that waited for that would hang on any grandchild
    /// that inherited the pipe.
    reader: Option<JoinHandle<()>>,
    /// The next stats group's number: one group per job, so a transfer's bytes
    /// are its own and not the sum of everything the daemon has done.
    next_group: u64,
}

/// A started async job, as the two request bodies that ask about it.
struct Job {
    /// `{"jobid": N}` — for `job/status` and `job/stop`.
    id: Json,
    /// `{"group": "df/N"}` — for `core/stats` and `core/stats-delete`.
    group: Json,
}

impl Daemon {
    /// Start `rclone rcd` for `service` and wait until it answers.
    ///
    /// `extra_args` go on the end of the command line — the tests' way in for
    /// `--bwlimit`, which is how a cancel is caught mid-transfer on a machine
    /// where a local copy would otherwise finish before the first poll.
    pub(super) fn spawn(service: Arc<Service>, extra_args: &[String]) -> Result<Daemon, VfsError> {
        let socket = socket_path(&service)?;
        // A socket file left by a crashed run with the same pid would make
        // `bind` fail with "address already in use"; nothing can be listening
        // on it, because the name has this process's pid in it.
        match std::fs::remove_file(&socket) {
            Ok(()) => log::debug!("vfs {}: removed a stale {}", service.name, socket.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("vfs {}: {}: {e}", service.name, socket.display()),
        }

        let (program, program_args): (PathBuf, &[String]) = match &service.program {
            Some((program, args)) => (program.clone(), args),
            None => (PathBuf::from(RCLONE), &[]),
        };
        let mut address = OsString::from("unix://");
        address.push(&socket);
        let mut command = Command::new(&program);
        command
            .arg("rcd")
            .arg("--rc-addr")
            .arg(address)
            .arg("--rc-no-auth")
            .args(["--rc-job-expire-duration", "10m"])
            .arg("--ask-password=false")
            .args(program_args)
            .args(extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // This is the worker thread that will own the daemon, so its end —
        // however it comes — is the daemon's too.
        child::tie_to_this_thread(&mut command);
        let mut child = command
            .spawn()
            .map_err(|source| spawn_failure(&service, &program, source))?;
        log::info!(
            "vfs {}: rclone rcd (pid {}) on {} for {}",
            service.name,
            child.id(),
            socket.display(),
            service.rclone_fs()
        );

        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let reader = match child.stderr.take() {
            Some(pipe) => match collect_stderr(pipe, &service.name, Arc::clone(&stderr)) {
                Ok(handle) => Some(handle),
                Err(source) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(VfsError::Spawn {
                        service: service.name.clone(),
                        program: "rclone's log reader".to_string(),
                        source,
                    });
                }
            },
            None => None,
        };

        let mut daemon = Daemon {
            fs: service.rclone_fs(),
            service,
            child,
            socket,
            stderr,
            reader,
            next_group: 0,
        };
        // On failure `daemon` drops here, which stops and reaps the child and
        // removes the socket — the same teardown as any other end.
        daemon.wait_ready()?;
        log::debug!(
            "vfs {}: rclone {} is answering",
            daemon.service.name,
            daemon.pid()
        );
        Ok(daemon)
    }

    /// The daemon's process id — what the log line names, and what the tests
    /// watch for after the thread that owned it has gone.
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Knock on the socket until `rc/noop` answers, the daemon exits, or
    /// [`CONNECT_TIMEOUT`] passes.
    fn wait_ready(&mut self) -> Result<(), VfsError> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(self.exited(status));
            }
            match http::post(&self.socket, "rc/noop", "{}", READY_ANSWER) {
                Ok(response) if response.status == 200 => return Ok(()),
                Ok(response) => {
                    return Err(self.disconnected(format!(
                        "rclone answered HTTP {} to its first request",
                        response.status
                    )))
                }
                // Not listening yet — the usual answer for the first few
                // knocks.
                Err(_) => {}
            }
            if Instant::now() >= deadline {
                let detail = self.last_words().unwrap_or_else(|| {
                    format!(
                        "rclone did not answer within {}s",
                        CONNECT_TIMEOUT.as_secs()
                    )
                });
                return Err(self.disconnected(detail));
            }
            std::thread::sleep(READY_POLL);
        }
    }

    // ── The client ──────────────────────────────────────────────────────────

    /// One rc call: `params` to `/<method>`, the reply's JSON back. `about` is
    /// the place the call concerns, which is what an error names.
    fn call(
        &mut self,
        method: &'static str,
        params: &Json,
        about: &VfsPath,
    ) -> Result<Json, VfsError> {
        if let Ok(Some(status)) = self.child.try_wait() {
            return Err(self.exited(status));
        }
        let body = params.to_string();
        let response = match http::post(&self.socket, method, &body, OP_TIMEOUT) {
            Ok(response) => response,
            Err(HttpError::TimedOut) => {
                return Err(VfsError::Timeout {
                    service: self.service.name.clone(),
                    op: method,
                    timeout: OP_TIMEOUT,
                })
            }
            Err(e) => return Err(self.lost(&e)),
        };
        let json = std::str::from_utf8(&response.body)
            .ok()
            .and_then(|text| Json::parse(text).ok());
        if (200..300).contains(&response.status) {
            return json.ok_or_else(|| self.garbled(method));
        }
        let message = json
            .as_ref()
            .and_then(|reply| reply.get("error"))
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("rclone answered HTTP {} to {method}", response.status));
        Err(status_error(about, message))
    }

    /// The request body for a call about one place: `{fs, remote}`.
    fn at(&self, path: &VfsPath) -> Json {
        Json::object([
            ("fs", Json::from(self.fs.as_str())),
            ("remote", Json::from(remote_of(path))),
        ])
    }

    // ── Operations ──────────────────────────────────────────────────────────

    /// Run one [`Op`] — the rclone half of `super::run_op`.
    pub(super) fn run_op(&mut self, op: Op, ctx: &TaskCtx) -> Result<Outcome, VfsError> {
        match op {
            Op::Stat { path, .. } => self.stat(&path).map(Outcome::Attrs),
            Op::Mkdir { path } => self.mkdir(&path).map(|()| Outcome::Unit),
            Op::Rmdir { path } => self.rmdir(&path).map(|()| Outcome::Unit),
            Op::Remove { path } => self.remove(&path).map(|()| Outcome::Unit),
            Op::Rename { from, to } => self.rename(&from, &to, ctx).map(|()| Outcome::Unit),
            Op::Download { remote, local } => {
                self.download(&remote, &local, ctx).map(Outcome::Bytes)
            }
            Op::Upload { local, remote } => self.upload(&local, &remote, ctx).map(Outcome::Bytes),
            Op::RealPath { .. } => Err(self.unsupported("realpath")),
            Op::ReadLink { .. } => Err(self.unsupported("reading a link")),
            Op::Symlink { .. } => Err(self.unsupported("a symlink")),
            Op::Chmod { .. } => Err(self.unsupported("chmod")),
        }
    }

    /// Every row of `dir`, in rclone's order — or `None` if `wanted` said
    /// no before rclone finished, in which case the listing job has been
    /// stopped.
    ///
    /// Run as a job with no deadline of its own (see the module note on why a
    /// slow listing must not be a timeout); `wanted` is asked on every poll,
    /// and is the pane's live-listing set, so arrowing away from a folder that
    /// is still being paged stops the paging.
    pub(super) fn list(
        &mut self,
        dir: &VfsPath,
        wanted: &dyn Fn() -> bool,
    ) -> Result<Option<Vec<Entry>>, VfsError> {
        let mut params = self.at(dir);
        // The MIME type is a per-object lookup on some providers and is
        // guessed from the name here anyway (`mime::hint_for_name`); the
        // modification time is the date column.
        params.insert(
            "opt",
            Json::object([
                ("noMimeType", Json::from(true)),
                ("noModTime", Json::from(false)),
            ]),
        );
        let job = self.start_job("operations/list", params, dir)?;
        let outcome = loop {
            if !wanted() {
                self.stop(&job.id, dir);
                break Ok(None);
            }
            let status = match self.call("job/status", &job.id, dir) {
                Ok(status) => status,
                Err(e) => break Err(self.abandon(&job, e, dir)),
            };
            if status.get("finished").and_then(Json::as_bool) == Some(true) {
                break self.job_result(&status, dir).map(Some);
            }
            std::thread::sleep(POLL);
        };
        self.end_job(&job, &outcome, dir);
        let Some(reply) = outcome? else {
            return Ok(None);
        };
        let Some(items) = reply.get("list").and_then(Json::as_array) else {
            return Err(self.garbled("operations/list"));
        };
        Ok(Some(
            items
                .iter()
                .filter_map(|item| {
                    let name = item.get("Name").and_then(Json::as_str)?;
                    if name.is_empty() {
                        return None;
                    }
                    let entry = wire::NameEntry {
                        filename: name.as_bytes().to_vec(),
                        longname: Vec::new(),
                        attrs: attrs_of(item),
                    };
                    Some(remote_entry(dir, &entry, None))
                })
                .collect(),
        ))
    }

    /// `operations/stat`. An `item` of `null` is rclone's "there is nothing
    /// there", which is a [`StatusCode::NoSuchFile`] like SFTP's.
    fn stat(&mut self, path: &VfsPath) -> Result<Attrs, VfsError> {
        let reply = self.call("operations/stat", &self.at(path), path)?;
        match reply.get("item") {
            Some(item) if !item.is_null() => Ok(attrs_of(item)),
            _ => Err(VfsError::Status {
                path: path.to_url(),
                status: Status {
                    code: StatusCode::NoSuchFile,
                    message: String::new(),
                },
            }),
        }
    }

    fn exists(&mut self, path: &VfsPath) -> Result<bool, VfsError> {
        match self.stat(path) {
            Ok(_) => Ok(true),
            Err(VfsError::Status { status, .. }) if status.code == StatusCode::NoSuchFile => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn mkdir(&mut self, path: &VfsPath) -> Result<(), VfsError> {
        self.call("operations/mkdir", &self.at(path), path)
            .map(|_| ())
    }

    /// An empty directory only — the contract SFTP's `RMDIR` has, and the one
    /// the app's delete relies on: a folder with something in it fails with
    /// rclone's own "directory not empty" rather than being emptied.
    fn rmdir(&mut self, path: &VfsPath) -> Result<(), VfsError> {
        self.call("operations/rmdir", &self.at(path), path)
            .map(|_| ())
    }

    fn remove(&mut self, path: &VfsPath) -> Result<(), VfsError> {
        self.call("operations/deletefile", &self.at(path), path)
            .map(|_| ())
    }

    /// Rename within the service, refusing to replace anything.
    ///
    /// SFTP's `RENAME` refuses an existing destination, and the app's `r` is
    /// written against that refusal; `operations/movefile` would overwrite
    /// without a word. So the destination is asked about first. The window
    /// between the question and the move is the same one [`super::Vfs::
    /// unique_name`] documents: there is no atomic "move if absent" to use.
    ///
    /// A file is one `movefile`. A directory is `sync/move`, which is a
    /// server-side directory rename wherever the provider has one and a
    /// copy-then-delete per object where it does not (S3 and its kin) — run as
    /// a job, because the second case can take as long as a transfer.
    fn rename(&mut self, from: &VfsPath, to: &VfsPath, ctx: &TaskCtx) -> Result<(), VfsError> {
        if self.exists(to)? {
            return Err(VfsError::Status {
                path: to.to_url(),
                status: Status {
                    code: StatusCode::Failure,
                    message: "already exists".to_string(),
                },
            });
        }
        let attrs = self.stat(from)?;
        if !attrs.is_dir() {
            let params = Json::object([
                ("srcFs", Json::from(self.fs.as_str())),
                ("srcRemote", Json::from(remote_of(from))),
                ("dstFs", Json::from(self.fs.as_str())),
                ("dstRemote", Json::from(remote_of(to))),
            ]);
            return self.call("operations/movefile", &params, from).map(|_| ());
        }
        let params = Json::object([
            ("srcFs", Json::from(join_fs(&self.fs, &remote_of(from)))),
            ("dstFs", Json::from(join_fs(&self.fs, &remote_of(to)))),
            ("deleteEmptySrcDirs", Json::from(true)),
            ("createEmptySrcDirs", Json::from(true)),
        ]);
        self.run_job("sync/move", params, ctx, None, from)?;
        // A move done object by object empties the source folder but, on a
        // provider with real folders, does not remove the folder itself; a
        // rename that left the old name behind would not be a rename. Where
        // the provider moved the directory whole, the source is already gone
        // and this is a not-found nobody needs to hear about.
        if let Err(e) = self.rmdir(from) {
            log::debug!("vfs {}: after moving {from}: {e}", self.service.name);
        }
        Ok(())
    }

    /// Copy `remote` to the local file `local`, as a job, with progress.
    ///
    /// A failure removes `local` if it was not there before — rclone never
    /// renames its partial file into place unless the copy finished, so this
    /// is belt and braces for a daemon killed mid-rename — and sweeps up any
    /// `.partial` of `local`'s that rclone left beside it, which a daemon that
    /// died or was killed mid-copy never removes itself. A success is flushed
    /// to disk before it is reported, as the SFTP download is.
    fn download(&mut self, remote: &VfsPath, local: &Path, ctx: &TaskCtx) -> Result<u64, VfsError> {
        let attrs = self.stat(remote)?;
        if attrs.is_dir() {
            return Err(VfsError::Status {
                path: remote.to_url(),
                status: Status {
                    code: StatusCode::Failure,
                    message: "is a folder, and only files download".to_string(),
                },
            });
        }
        let (dir, name) = local_parts(local)?;
        let existed = local.symlink_metadata().is_ok();
        let params = Json::object([
            ("srcFs", Json::from(self.fs.as_str())),
            ("srcRemote", Json::from(remote_of(remote))),
            ("dstFs", Json::from(dir)),
            ("dstRemote", Json::from(name)),
        ]);
        // Google Docs list with a size of -1: the export's size is not known
        // until it has been made, so the bar runs without a total, as SFTP's
        // does for a server that sends none.
        let total = attrs.size.unwrap_or(0);
        match self.run_job("operations/copyfile", params, ctx, Some(total), remote) {
            Ok(bytes) => {
                let flushed = std::fs::File::open(local).and_then(|file| file.sync_all());
                flushed.map_err(|source| VfsError::Io {
                    path: local.to_path_buf(),
                    source,
                })?;
                Ok(bytes)
            }
            Err(e) => {
                if !existed {
                    let _ = std::fs::remove_file(local);
                }
                sweep_partials(local);
                Err(e)
            }
        }
    }

    /// Copy the local file `local` to `remote`, as a job, with progress.
    ///
    /// **This replaces whatever is at `remote`**, like [`super::Vfs::upload`]
    /// says — and, like the SFTP upload, a cancelled or failed one leaves what
    /// was there untouched: every provider rclone speaks commits an object
    /// only when its upload completes, and on a local or SFTP-like remote rclone
    /// writes to a `.partial` name first and renames it at the end.
    fn upload(&mut self, local: &Path, remote: &VfsPath, ctx: &TaskCtx) -> Result<u64, VfsError> {
        let meta = std::fs::metadata(local).map_err(|source| VfsError::Io {
            path: local.to_path_buf(),
            source,
        })?;
        if meta.is_dir() {
            return Err(VfsError::Io {
                path: local.to_path_buf(),
                source: std::io::Error::from(std::io::ErrorKind::IsADirectory),
            });
        }
        let (dir, name) = local_parts(local)?;
        let params = Json::object([
            ("srcFs", Json::from(dir)),
            ("srcRemote", Json::from(name)),
            ("dstFs", Json::from(self.fs.as_str())),
            ("dstRemote", Json::from(remote_of(remote))),
        ]);
        self.run_job("operations/copyfile", params, ctx, Some(meta.len()), remote)
    }

    // ── Jobs ────────────────────────────────────────────────────────────────

    /// Start `method` as an async job in a stats group of its own, follow it
    /// to the end, and return the bytes it moved.
    ///
    /// `total` is the size the progress bar is measured against; `None` for a
    /// job whose bytes are not the point (a folder rename), which then reports
    /// no progress at all rather than a bar with no end.
    fn run_job(
        &mut self,
        method: &'static str,
        params: Json,
        ctx: &TaskCtx,
        total: Option<u64>,
        about: &VfsPath,
    ) -> Result<u64, VfsError> {
        if let Some(total) = total {
            ctx.set_total(total, 1);
        }
        let job = self.start_job(method, params, about)?;
        let report = total.is_some();
        let mut moved = 0u64;
        let outcome = loop {
            if let Err(crate::DfError::Cancelled) = ctx.checkpoint() {
                self.stop(&job.id, about);
                break Err(VfsError::Cancelled);
            }
            match self.call("core/stats", &job.group, about) {
                Ok(now) => moved = self.advance(&now, moved, report, ctx),
                Err(e) => break Err(self.abandon(&job, e, about)),
            }
            let status = match self.call("job/status", &job.id, about) {
                Ok(status) => status,
                Err(e) => break Err(self.abandon(&job, e, about)),
            };
            if status.get("finished").and_then(Json::as_bool) == Some(true) {
                // The bytes of the job's last moments landed between the two
                // calls above; one more look so the bar ends at the truth.
                if let Ok(last) = self.call("core/stats", &job.group, about) {
                    moved = self.advance(&last, moved, report, ctx);
                }
                break self.job_result(&status, about).map(|_| moved);
            }
            std::thread::sleep(POLL);
        };
        self.end_job(&job, &outcome, about);
        outcome
    }

    /// Send `method` with `_async`, in a stats group named for this daemon's
    /// next job, and return the handles the polling needs.
    fn start_job(
        &mut self,
        method: &'static str,
        mut params: Json,
        about: &VfsPath,
    ) -> Result<Job, VfsError> {
        let group = format!("df/{}", self.next_group);
        self.next_group += 1;
        params.insert("_async", Json::from(true));
        params.insert("_group", Json::from(group.as_str()));
        let started = self.call(method, &params, about)?;
        let Some(jobid) = started.get("jobid").and_then(Json::as_i64) else {
            return Err(self.garbled(method));
        };
        Ok(Job {
            id: Json::object([("jobid", Json::from(jobid))]),
            group: Json::object([("group", Json::from(group.as_str()))]),
        })
    }

    /// A finished job's answer: its `output` when it succeeded, rclone's
    /// sentence as a status error when it did not.
    fn job_result(&self, status: &Json, about: &VfsPath) -> Result<Json, VfsError> {
        if status.get("success").and_then(Json::as_bool) == Some(true) {
            return Ok(status.get("output").cloned().unwrap_or(Json::Null));
        }
        let message = status
            .get("error")
            .and_then(Json::as_str)
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("rclone reported a failure and gave no reason");
        Err(status_error(about, message.to_string()))
    }

    /// Asking about a running job failed. While the daemon is still up, stop
    /// the job rather than leave it running unwatched; a fatal error takes the
    /// daemon down with it, which stops everything anyway. Returns the error.
    fn abandon(&mut self, job: &Job, error: VfsError, about: &VfsPath) -> VfsError {
        if !error.is_connection_fatal() {
            self.stop(&job.id, about);
        }
        error
    }

    /// Delete a finished job's stats group — one per job would otherwise pile
    /// up in the daemon for as long as it runs — unless the daemon is on its
    /// way out, when there is nobody to ask.
    fn end_job<T>(&mut self, job: &Job, outcome: &Result<T, VfsError>, about: &VfsPath) {
        if outcome
            .as_ref()
            .err()
            .is_some_and(VfsError::is_connection_fatal)
        {
            return;
        }
        if let Err(e) = self.call("core/stats-delete", &job.group, about) {
            log::debug!("vfs {}: core/stats-delete: {e}", self.service.name);
        }
    }

    /// Report the growth in a stats group's `bytes` as progress, returning the
    /// new high-water mark.
    fn advance(&self, stats: &Json, moved: u64, report: bool, ctx: &TaskCtx) -> u64 {
        let bytes = stats
            .get("bytes")
            .and_then(Json::as_i64)
            .and_then(|b| u64::try_from(b).ok())
            .unwrap_or(0);
        if bytes <= moved {
            return moved;
        }
        if report {
            ctx.advance(bytes - moved, 0);
        }
        bytes
    }

    /// `job/stop`, then wait (up to [`STOP_GRACE`]) for the job to say it has
    /// finished — which is when its partial file is gone.
    fn stop(&mut self, job: &Json, about: &VfsPath) {
        if let Err(e) = self.call("job/stop", job, about) {
            log::debug!("vfs {}: job/stop: {e}", self.service.name);
            return;
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            match self.call("job/status", job, about) {
                Ok(status) if status.get("finished").and_then(Json::as_bool) == Some(true) => {
                    return
                }
                Ok(_) => std::thread::sleep(READY_POLL),
                Err(_) => return,
            }
        }
        log::warn!(
            "vfs {}: a stopped rclone job was still running after {}s",
            self.service.name,
            STOP_GRACE.as_secs()
        );
    }

    // ── Errors ──────────────────────────────────────────────────────────────

    fn unsupported(&self, op: &'static str) -> VfsError {
        VfsError::Unsupported {
            service: self.service.name.clone(),
            op,
        }
    }

    fn disconnected(&self, detail: String) -> VfsError {
        VfsError::Disconnected {
            service: self.service.name.clone(),
            detail,
        }
    }

    /// The daemon has exited: say what it said last, or how it ended.
    fn exited(&mut self, status: ExitStatus) -> VfsError {
        // The exit and its explanation race down different paths; give the
        // explanation a moment to arrive.
        let deadline = Instant::now() + STDERR_GRACE;
        while self.reader.as_ref().is_some_and(|r| !r.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let detail = self
            .last_words()
            .unwrap_or_else(|| format!("rclone exited ({status})"));
        self.disconnected(detail)
    }

    /// The socket stopped answering. If the daemon died, that is the story;
    /// otherwise it is the socket error.
    fn lost(&mut self, error: &HttpError) -> VfsError {
        if let Ok(Some(status)) = self.child.try_wait() {
            return self.exited(status);
        }
        self.disconnected(format!("lost the connection to rclone: {error}"))
    }

    /// A reply that is not the JSON it should be. Fatal: a daemon that answers
    /// in something other than its own protocol is not one to keep talking to.
    fn garbled(&self, method: &str) -> VfsError {
        self.disconnected(format!("rclone's answer to {method} made no sense"))
    }

    /// The last line rclone wrote to stderr, cleaned of its timestamp.
    fn last_words(&self) -> Option<String> {
        lock(&self.stderr).back().cloned()
    }
}

impl Drop for Daemon {
    /// Stop, reap, unlink — unconditionally, for `conn::Transport`'s reason:
    /// a daemon per reconnect that nobody reaped is a file manager that leaks
    /// processes for as long as it runs.
    ///
    /// Stop *gently* first. `SIGTERM` is the signal rclone cleans up on — it
    /// removes the `.partial` file of anything it was writing, and its socket,
    /// and exits in about 200 ms — where `SIGKILL` leaves both behind. So
    /// `SIGTERM`, a wait of up to [`STOP_GRACE`] for the exit, and `SIGKILL`
    /// only for a daemon that is still there after it.
    fn drop(&mut self) {
        let pid = self.pid();
        if let Err(e) = child::terminate(&mut self.child) {
            log::debug!("vfs {}: SIGTERM to rclone {pid}: {e}", self.service.name);
        }
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(EXIT_POLL),
                Ok(None) | Err(_) => {
                    log::warn!(
                        "vfs {}: rclone {pid} did not exit within {}s of SIGTERM; killing it",
                        self.service.name,
                        STOP_GRACE.as_secs()
                    );
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// The path inside the service's fs that `path` names: no leading or trailing
/// slash, because rclone joins it onto the fs root itself.
fn remote_of(path: &VfsPath) -> String {
    path.path.trim_matches('/').to_string()
}

/// An fs one level down: `r2:` + `photos` is `r2:photos`, `/tmp/x` +
/// `photos` is `/tmp/x/photos`.
fn join_fs(fs: &str, remote: &str) -> String {
    if remote.is_empty() {
        fs.to_string()
    } else if fs.ends_with(':') || fs.ends_with('/') {
        format!("{fs}{remote}")
    } else {
        format!("{fs}/{remote}")
    }
}

/// A local file as rclone addresses it: its directory as the fs, its name as
/// the remote. Both have to be UTF-8 to go into JSON at all.
fn local_parts(local: &Path) -> Result<(String, String), VfsError> {
    let refuse = |why: &str| VfsError::Io {
        path: local.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_string()),
    };
    let absolute = std::path::absolute(local).map_err(|source| VfsError::Io {
        path: local.to_path_buf(),
        source,
    })?;
    let name = absolute
        .file_name()
        .ok_or_else(|| refuse("not a file name"))?
        .to_str()
        .ok_or_else(|| refuse("rclone can only be given UTF-8 names"))?
        .to_string();
    let dir = absolute
        .parent()
        .ok_or_else(|| refuse("not a file name"))?
        .to_str()
        .ok_or_else(|| refuse("rclone can only be given UTF-8 paths"))?
        .to_string();
    Ok((dir, name))
}

/// One `operations/list` item (or `operations/stat`'s `item`) as the
/// attributes a row is built from.
fn attrs_of(item: &Json) -> Attrs {
    let is_dir = item.get("IsDir").and_then(Json::as_bool).unwrap_or(false);
    Attrs {
        // -1 is rclone's "not known" (a Google Doc before export).
        size: item
            .get("Size")
            .and_then(Json::as_i64)
            .and_then(|s| u64::try_from(s).ok()),
        // A cloud object has no owner a local `passwd` could name, and
        // pretending it is root's would be the lie `wire::Attrs` exists to
        // prevent.
        uid: None,
        gid: None,
        permissions: Some(if is_dir { DIR_MODE } else { FILE_MODE }),
        atime: None,
        // Before 1970 or after 2106 does not fit the row's date; a date that
        // does not parse is no date. Neither fails the listing.
        mtime: item
            .get("ModTime")
            .and_then(Json::as_str)
            .and_then(super::rfc3339::parse)
            .and_then(|seconds| u32::try_from(seconds).ok()),
    }
}

/// rclone's error sentence as a [`VfsError::Status`] about `about`.
fn status_error(about: &VfsPath, message: String) -> VfsError {
    VfsError::Status {
        path: about.to_url(),
        status: Status {
            code: code_for(&message),
            message,
        },
    }
}

/// Which status an rclone error sentence is.
///
/// Only the not-found family is told apart, because it is the only one
/// anything *decides* on: "is this name free?" is a stat that fails with
/// [`StatusCode::NoSuchFile`], and a permission problem must never be read as
/// that. Everything else is a [`StatusCode::Failure`] carrying rclone's own
/// sentence, which is the part a person reads anyway.
///
/// **Only rclone's own sentences count, whole** ([`NOT_FOUND`]). A looser
/// "contains *not found*" also caught a provider's `404 Not Found` and Google
/// Drive's `File not found` — which Drive says for a file you may not see as
/// well as for one that is not there — and a name reported free that is not
/// free is the one wrong answer here that destroys something: `unique_name`
/// hands it to an upload, and the upload replaces what was there. So the match
/// is case-sensitive, as rclone writes them, and a phrase only counts when it
/// is not part of a longer word on either side.
fn code_for(message: &str) -> StatusCode {
    if NOT_FOUND
        .iter()
        .any(|phrase| contains_phrase(message, phrase))
    {
        StatusCode::NoSuchFile
    } else {
        StatusCode::Failure
    }
}

/// rclone's canonical "there is nothing there": `fs.ErrorObjectNotFound`,
/// `fs.ErrorDirNotFound`, and the local backend's `ENOENT` as Go words it.
const NOT_FOUND: [&str; 3] = [
    "object not found",
    "directory not found",
    "no such file or directory",
];

/// Whether `phrase` occurs in `text` as a whole phrase: with no letter or digit
/// touching it on either side.
fn contains_phrase(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + phrase.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Remove the `.partial` files rclone left beside `local` — its own name, a
/// dot, rclone's eight lowercase hex digits, `.partial` — and nothing else.
///
/// rclone removes its partial file itself when a job is stopped or fails, and
/// when the daemon is sent `SIGTERM`; this is for the ends where it could not:
/// a daemon that crashed, or was killed, mid-copy. The pattern is exact
/// ([`is_rclone_partial`]) because the directory is the user's: a file called
/// `notes.txt.backup.partial` is somebody's, not debris.
fn sweep_partials(local: &Path) {
    let (Some(dir), Some(name)) = (local.parent(), local.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(candidate) = file_name.to_str() else {
            continue;
        };
        if !is_rclone_partial(candidate, name) {
            continue;
        }
        // A file, never a folder or a link that happens to wear the name.
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => log::debug!("vfs: removed rclone's leftover {}", entry.path().display()),
            Err(e) => log::warn!("vfs: {}: {e}", entry.path().display()),
        }
    }
}

/// `candidate` is `<name>.<8 lowercase hex digits>.partial` — the shape rclone
/// gives the file a copy to `name` writes before it is renamed into place.
fn is_rclone_partial(candidate: &str, name: &str) -> bool {
    let Some(middle) = candidate
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".partial"))
    else {
        return false;
    };
    middle.len() == 8
        && middle
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Why a daemon would not start, in words.
///
/// The one failure worth its own sentence is the program not being there:
/// rclone is not a dependency of delightfile, it is a program the user may
/// simply not have installed, and "No such file or directory (os error 2)"
/// does not say which file.
fn spawn_failure(service: &Service, program: &Path, source: std::io::Error) -> VfsError {
    if service.program.is_none() && source.kind() == std::io::ErrorKind::NotFound {
        return VfsError::Disconnected {
            service: service.name.clone(),
            detail: NOT_INSTALLED.to_string(),
        };
    }
    VfsError::Spawn {
        service: service.name.clone(),
        program: program.display().to_string(),
        source,
    }
}

/// Where the socket goes: the service's [`Service::socket_dir`] when it names
/// one (the tests' way to keep out of the user's runtime directory), else
/// `$XDG_RUNTIME_DIR/delightfile/`, else a private directory under the temp
/// dir.
///
/// The name carries the pid, the service and a per-process counter. The pid
/// keeps two delightfiles apart; the counter keeps two daemons in one process
/// apart even when their services' names sanitise to the same file name (or
/// when a test starts two for one service), since each start removes whatever
/// is at its path first.
fn socket_path(service: &Service) -> Result<PathBuf, VfsError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "rclone-{}-{}-{n}.sock",
        std::process::id(),
        file_safe(&service.name)
    );
    let uid = crate::platform::user::uid();
    let mut candidates: Vec<PathBuf> = Vec::new();
    match &service.socket_dir {
        // Only there: a service that says where its socket goes has said
        // where it must not go, too.
        Some(dir) => candidates.push(dir.clone()),
        None => {
            if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
                candidates.push(PathBuf::from(runtime).join("delightfile"));
            }
            candidates.push(std::env::temp_dir().join(format!("delightfile-{uid}")));
            // A `$TMPDIR` deep enough to push the name past `sun_path` still
            // leaves `/tmp`, which never does.
            candidates.push(PathBuf::from("/tmp").join(format!("delightfile-{uid}")));
        }
    }
    for dir in candidates {
        let path = dir.join(&name);
        if path.as_os_str().len() > SUN_PATH_MAX {
            continue;
        }
        match private_dir(&dir, uid) {
            Ok(()) => return Ok(path),
            Err(e) => log::warn!("vfs {}: {}: {e}", service.name, dir.display()),
        }
    }
    Err(VfsError::Spawn {
        service: service.name.clone(),
        program: RCLONE.to_string(),
        source: std::io::Error::other(
            "found no private directory short enough to put its socket in",
        ),
    })
}

/// Make `dir` if needed, and insist it is this user's and nobody else's.
///
/// The socket has no authentication of its own (`--rc-no-auth`); the
/// directory's mode is its authentication. A directory someone else owns is
/// refused rather than used, and one of ours that has been opened up is closed
/// again.
fn private_dir(dir: &Path, uid: u32) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "not a directory this user owns",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A service name as a piece of a file name: letters, digits, `.`, `_` and
/// `-` kept, everything else `_`, and short enough to leave room in
/// `sun_path`.
fn file_safe(name: &str) -> String {
    name.chars()
        .take(32)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Read rclone's stderr for as long as it has one, logging each line and
/// keeping the last [`STDERR_LINES`].
fn collect_stderr(
    pipe: ChildStderr,
    service: &str,
    tail: Arc<Mutex<VecDeque<String>>>,
) -> std::io::Result<JoinHandle<()>> {
    let service = service.to_string();
    std::thread::Builder::new()
        .name(format!("df-rclone-{service}"))
        .spawn(move || {
            let reader = BufReader::new(pipe);
            for line in reader.split(b'\n') {
                let Ok(line) = line else { break };
                let text = String::from_utf8_lossy(&line);
                let text = clean_log_line(text.trim_end());
                if text.is_empty() {
                    continue;
                }
                log::debug!("rclone {service}: {text}");
                let mut kept: String = text.chars().take(STDERR_LINE_MAX).collect();
                if kept.len() < text.len() {
                    kept.push('…');
                }
                let mut tail = lock(&tail);
                if tail.len() == STDERR_LINES {
                    tail.pop_front();
                }
                tail.push_back(kept);
            }
        })
}

/// `2026/09/25 10:30:56 CRITICAL: Failed to start…` → `Failed to start…`.
///
/// rclone prefixes every log line with a timestamp and a level; the first is
/// noise in a toast and the second is implied by there being a toast at all.
fn clean_log_line(line: &str) -> &str {
    let mut rest = line;
    let b = rest.as_bytes();
    let stamped = b.len() > 20
        && b[4] == b'/'
        && b[7] == b'/'
        && b[10] == b' '
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b' '
        && b[..19]
            .iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16) || c.is_ascii_digit());
    if stamped {
        rest = &rest[20..];
    }
    // `ERROR : `, `NOTICE: `, `CRITICAL: ` — an upper-case word, maybe
    // padded, then a colon.
    if let Some((level, after)) = rest.split_once(':') {
        let level = level.trim_end();
        if !level.is_empty() && level.len() <= 8 && level.bytes().all(|c| c.is_ascii_uppercase()) {
            rest = after.trim_start();
        }
    }
    rest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fs_joins_the_remote_and_the_root_the_way_rclone_writes_them() {
        let mut service = Service::rclone("r2", "r2");
        assert_eq!(service.rclone_fs(), "r2:");
        service.root = Some("/photos/".into());
        assert_eq!(service.rclone_fs(), "r2:photos");
        let mut named = Service::rclone("r2", "r2");
        named.remote = None;
        assert_eq!(
            named.rclone_fs(),
            "r2:",
            "no remote means the service's name"
        );

        let mut path = Service::rclone("t", "/tmp/fixture");
        assert_eq!(path.rclone_fs(), "/tmp/fixture");
        path.root = Some("sub".into());
        assert_eq!(path.rclone_fs(), "/tmp/fixture/sub");

        let mut bucket = Service::rclone("b", "r2:bucket");
        assert_eq!(bucket.rclone_fs(), "r2:bucket");
        bucket.root = Some("inside".into());
        assert_eq!(bucket.rclone_fs(), "r2:bucket/inside");
        let mut colon = Service::rclone("c", "r2:");
        colon.root = Some("bucket".into());
        assert_eq!(colon.rclone_fs(), "r2:bucket");

        assert_eq!(join_fs("r2:", "a/b"), "r2:a/b");
        assert_eq!(join_fs("/tmp/x", "a"), "/tmp/x/a");
        assert_eq!(join_fs("r2:bucket", ""), "r2:bucket");
    }

    #[test]
    fn remote_paths_lose_their_slashes() {
        assert_eq!(remote_of(&VfsPath::rclone("r2", "")), "");
        assert_eq!(remote_of(&VfsPath::rclone("r2", "/a/b/")), "a/b");
        let parsed = VfsPath::parse("rclone://r2/bucket/photos")
            .unwrap_or_else(|| VfsPath::rclone("never", ""));
        assert_eq!(remote_of(&parsed), "bucket/photos");
    }

    #[test]
    fn rclone_error_sentences_sort_into_not_found_and_everything_else() {
        // rclone's own sentences, whole, wherever they sit in the message.
        for message in [
            "object not found",
            "error in ListJSON: directory not found",
            "stat /tmp/x/zz: no such file or directory",
            "Failed to copy: object not found.",
            "(directory not found)",
        ] {
            assert_eq!(code_for(message), StatusCode::NoSuchFile, "{message}");
        }
        // Everything else — including the providers' own "not found"s, which
        // can mean "you may not see it" and so must never read as "free".
        for message in [
            "404 Not Found",
            "googleapi: Error 404: File not found: 1AbC., notFound",
            "Object Not Found",
            "job not found",
            "subdirectory not found",
            "object not founded",
            "remove /tmp/x/sub: directory not empty",
            "didn't find section in config file (\"r9\")",
            "permission denied",
            "",
        ] {
            assert_eq!(code_for(message), StatusCode::Failure, "{message}");
        }
        let error = status_error(&VfsPath::rclone("r2", "a"), "directory not empty".into());
        assert!(
            !error.is_connection_fatal(),
            "rclone saying no is rclone working"
        );
        assert_eq!(error.to_string(), "rclone://r2/a: directory not empty");
    }

    #[test]
    fn a_missing_rclone_is_named_as_such() {
        let service = Service::rclone("r2", "r2");
        let error = spawn_failure(
            &service,
            Path::new(RCLONE),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert_eq!(error.to_string(), "r2: rclone is not installed");

        // A configured stand-in that is missing is named by its path instead:
        // it is not rclone that is missing.
        let mut custom = Service::rclone("r2", "r2");
        custom.program = Some(("/opt/rclone-beta".into(), Vec::new()));
        let error = spawn_failure(
            &custom,
            Path::new("/opt/rclone-beta"),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert!(
            error
                .to_string()
                .starts_with("r2: could not start /opt/rclone-beta:"),
            "{error}"
        );
        // And any other spawn failure keeps its own words.
        let error = spawn_failure(
            &service,
            Path::new(RCLONE),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert!(
            error.to_string().starts_with("r2: could not start rclone:"),
            "{error}"
        );
    }

    #[test]
    fn listing_items_become_attributes_without_inventing_any() {
        let item = Json::parse(
            r#"{"Path":"a/f.txt","Name":"f.txt","Size":3,"ModTime":"2026-09-25T10:19:42.038713743-05:00","IsDir":false}"#,
        )
        .unwrap_or(Json::Null);
        let attrs = attrs_of(&item);
        assert_eq!(attrs.size, Some(3));
        assert_eq!(attrs.permissions, Some(FILE_MODE));
        assert!(attrs.is_file());
        assert_eq!(attrs.mtime, Some(1_790_349_582));
        assert_eq!((attrs.uid, attrs.gid, attrs.atime), (None, None, None));

        let doc = Json::parse(r#"{"Name":"Plan","Size":-1,"ModTime":"garbage","IsDir":false}"#)
            .unwrap_or(Json::Null);
        let attrs = attrs_of(&doc);
        assert_eq!(attrs.size, None, "a Google Doc's -1 is not a size");
        assert_eq!(attrs.mtime, None, "a date that does not parse is no date");

        let dir =
            Json::parse(r#"{"Name":"d","Size":0,"ModTime":"0001-01-01T00:00:00Z","IsDir":true}"#)
                .unwrap_or(Json::Null);
        let attrs = attrs_of(&dir);
        assert!(attrs.is_dir());
        assert_eq!(
            attrs.mtime, None,
            "Go's zero time is before the row's epoch"
        );
    }

    #[test]
    fn socket_names_are_short_safe_and_distinct() {
        assert_eq!(file_safe("r2"), "r2");
        assert_eq!(file_safe("my remote/../x"), "my_remote_.._x");
        assert_eq!(file_safe(&"x".repeat(100)).len(), 32);
        // In a scratch directory of its own, opened up so the check that
        // closes it again has something to do — never the user's runtime
        // directory.
        let scratch = super::super::tests::TempDir::new("rclone-sockets");
        let run = scratch.path.join("run");
        std::fs::create_dir(&run).unwrap_or_default();
        let _ = std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755));
        let mut service = Service::rclone("my remote", "r2");
        service.socket_dir = Some(run.clone());
        let a = socket_path(&service).unwrap_or_default();
        let b = socket_path(&service).unwrap_or_default();
        assert_eq!(
            a.parent(),
            Some(run.as_path()),
            "the service's own directory"
        );
        assert_ne!(a, b, "two daemons in one process never share a socket");
        assert!(a.as_os_str().len() <= SUN_PATH_MAX, "{}", a.display());
        let name = a.file_name().map(|n| n.to_string_lossy().into_owned());
        assert!(
            name.as_deref().is_some_and(
                |n| n.starts_with(&format!("rclone-{}-my_remote-", std::process::id()))
            ),
            "{name:?}"
        );
        let mode = std::fs::metadata(&run).map(|m| m.permissions().mode() & 0o777);
        assert_eq!(mode.ok(), Some(0o700), "closed again: {}", run.display());

        // A directory the socket would not fit in is refused, not bound.
        let mut deep = Service::rclone("r2", "r2");
        deep.socket_dir = Some(scratch.path.join("d".repeat(SUN_PATH_MAX)));
        let error = socket_path(&deep)
            .map(|_| ())
            .expect_err("a socket path past sun_path is refused");
        assert!(error.to_string().contains("short enough"), "{error}");
    }

    /// **Dropped mid-copy, the daemon is stopped gently** — `SIGTERM`, which
    /// rclone cleans up on — so the `.partial` it was writing goes with it,
    /// and the drop does not have to wait for the `SIGKILL` fallback.
    #[test]
    fn a_daemon_dropped_mid_copy_cleans_up_after_itself() {
        use super::super::rclone_tests::{
            find_rclone, partials_in, scratch_service, sockets_in, unique_name,
        };
        use super::super::tests::TempDir;
        let Some(rclone) = find_rclone() else {
            eprintln!(
                "skipping a_daemon_dropped_mid_copy_cleans_up_after_itself: no rclone on $PATH"
            );
            return;
        };
        let remote = TempDir::new("rclone-drop-remote");
        let scratch = TempDir::new("rclone-drop-scratch");
        let local = TempDir::new("rclone-drop-local");
        let big: Vec<u8> = (0..16 * 1024 * 1024u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        remote.file("big.bin", &big);
        let name = unique_name("dr");
        let service = scratch_service(
            Service::rclone(&name, remote.path.display().to_string()),
            &scratch,
            &rclone,
        );
        let Ok(mut daemon) = Daemon::spawn(Arc::new(service), &["--bwlimit".into(), "2M".into()])
        else {
            panic!("the daemon did not start");
        };
        let about = VfsPath::rclone(&name, "big.bin");
        let params = Json::object([
            ("srcFs", Json::from(daemon.fs.as_str())),
            ("srcRemote", Json::from("big.bin")),
            ("dstFs", Json::from(local.path.display().to_string())),
            ("dstRemote", Json::from("big.bin")),
        ]);
        assert!(daemon
            .start_job("operations/copyfile", params, &about)
            .is_ok());
        // Wait for the copy to be under way: its partial file exists.
        let deadline = Instant::now() + Duration::from_secs(10);
        while partials_in(&local.path).is_empty() {
            assert!(Instant::now() < deadline, "the copy never started");
            std::thread::sleep(Duration::from_millis(20));
        }

        let started = Instant::now();
        drop(daemon);
        assert!(
            started.elapsed() < STOP_GRACE,
            "SIGTERM was enough; the drop took {:?}",
            started.elapsed()
        );
        assert!(
            partials_in(&local.path).is_empty(),
            "rclone left {:?} behind",
            partials_in(&local.path)
        );
        assert!(!local.path.join("big.bin").exists());
        assert!(sockets_in(&scratch).is_empty());
    }

    /// Only `<name>.<8 lowercase hex>.partial` is rclone's debris.
    #[test]
    fn only_rclones_own_partial_names_are_debris() {
        assert!(is_rclone_partial("keep.bin.0123abcd.partial", "keep.bin"));
        assert!(is_rclone_partial("a b.txt.fb7953c6.partial", "a b.txt"));
        for (candidate, name) in [
            ("keep.bin.backup.partial", "keep.bin"),
            ("keep.bin.0123ABCD.partial", "keep.bin"),
            ("keep.bin.0123abc.partial", "keep.bin"),
            ("keep.bin.0123abcde.partial", "keep.bin"),
            ("keep.bin.partial", "keep.bin"),
            ("keep.bin.0123abcd.partial.old", "keep.bin"),
            ("other.bin.0123abcd.partial", "keep.bin"),
            ("xkeep.bin.0123abcd.partial", "keep.bin"),
            ("keep.bin", "keep.bin"),
        ] {
            assert!(!is_rclone_partial(candidate, name), "{candidate}");
        }
    }

    #[test]
    fn rclone_log_lines_lose_their_stamp_and_level() {
        assert_eq!(
            clean_log_line(
                "2026/09/25 10:30:56 CRITICAL: Failed to start remote control: bind: invalid argument"
            ),
            "Failed to start remote control: bind: invalid argument"
        );
        assert_eq!(
            clean_log_line("2026/09/25 10:30:31 ERROR : nope: error listing: directory not found"),
            "nope: error listing: directory not found"
        );
        assert_eq!(clean_log_line("NOTICE: Serving"), "Serving");
        assert_eq!(
            clean_log_line("Error: unknown flag: --nope"),
            "Error: unknown flag: --nope",
            "not an upper-case level, so left alone"
        );
        assert_eq!(clean_log_line(""), "");
    }
}
