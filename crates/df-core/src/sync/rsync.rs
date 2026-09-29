//! Sync with a server, through `rsync`.
//!
//! A sync with `sftp://…` at one end cannot be planned the way [`super::plan`]
//! plans one. That walk is a `stat` per path, and over SFTP a stat is a round
//! trip: a tree of twenty thousand photos would take minutes to compare
//! before a byte moved. `rsync` does exactly this comparison on both machines
//! at once and is on every machine a person syncs to, so the remote half
//! hands it the job, keeping everything around it the same:
//!
//! - the **plan** is `rsync`'s own dry run, `-n -ii --delete`, whose itemized
//!   lines ([`parse_itemized`]) become the same [`SyncPlan`] the local walk
//!   makes, so the card cannot tell the two apart;
//! - the **run** is `rsync -a --info=progress2`, its running byte count read
//!   off the `\r`-separated progress line ([`progress_bytes`]) into the task's
//!   bar, the child stopped on a pause and killed on a cancel;
//! - the **verify** hashes the files on this machine and asks the server's own
//!   `sha256sum` about the far side over `ssh`. When the server cannot answer,
//!   the report says the copy was verified locally only, rather than claiming
//!   a proof it does not have.
//!
//! `rsync --delete-after` deletes for good, wherever the extras are — there is
//! no trash on the far side of it — so a remote plan always says
//! [`Removal::Delete`], and the card says so on its button. *After*, as a
//! local mirror does it: the copies land first, and only then do the extras
//! go, so a run that fails half way has not already emptied the destination.
//!
//! Durability follows the local rule as far as `rsync` allows. On a download
//! the files land on this machine, and each one — and the folder holding it —
//! is `fsync`ed here once `rsync` is done, before the verify reads them back.
//! On an upload the server's `rsync` has to do it, which `--fsync` asks for;
//! a server too old to know the option refuses the run at once, the run is
//! made again without it, and the result says the server was not flushed.
//!
//! The connection is the vfs's: the same `ssh` options
//! ([`crate::vfs::Service::command`]) — never prompt, give up connecting after
//! [`crate::vfs::CONNECT_TIMEOUT`] — so a host that works in the remote pane
//! works here, and one that wants a password fails at once with ssh's own
//! sentence instead of waiting on a prompt nobody can see.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::platform;
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::{Class, Item, Kind, Mode, Removal, Root, SyncOptions, SyncPlan, SyncReport, Verify};

/// How often a waiting loop looks at the child and the task's flags: short
/// enough that a cancel is felt at once, long enough to cost nothing.
const POLL: Duration = Duration::from_millis(50);

/// How much of a child's stderr is kept for the error message.
const KEEP_STDERR: u64 = 64 * 1024;

/// `rsync`'s exit codes for a run that did most of its work: 23, some files
/// could not be transferred; 24, some vanished while it ran. Anything else
/// that is not 0 is a run that did not happen.
const PARTIAL: [i32; 2] = [23, 24];

/// The oldest `rsync` a sync with a server runs: 3.1.0, the first to know
/// `--info=progress2`, which the run's progress bar is read from.
///
/// macOS's own `/usr/bin/rsync` is older: Samba's 2.6.9, or openrsync, which
/// calls itself compatible with 2.6.9. Both refuse the option, so on a Mac the
/// feature needs Homebrew's ([`crate::platform::process::RSYNC_HINT`] says
/// so).
pub const MIN_VERSION: (u32, u32, u32) = (3, 1, 0);

/// Whether `rsync` is on `PATH` and new enough ([`MIN_VERSION`]), asked the way
/// the archive readers ask about their tools: by running it, so the answer
/// cannot disagree with the run. One too old is not there as far as a sync is
/// concerned, since every run would fail on its first option.
///
/// Only where rsync is a tool of the platform at all
/// ([`crate::platform::process::HAS_RSYNC`]): not on Windows, where it is not
/// installed and its `host:path` syntax reads a drive letter as a host.
pub fn available() -> bool {
    gated(crate::platform::process::HAS_RSYNC, || {
        Command::new("rsync")
            .arg("--version")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|out| {
                out.status.success() && new_enough(&String::from_utf8_lossy(&out.stdout))
            })
    })
}

/// Whether `rsync --version` printed `text` for an rsync of [`MIN_VERSION`] or
/// newer. openrsync's banner (`openrsync: protocol version 29`) names no rsync
/// version on its first line, and is not one.
pub fn new_enough(text: &str) -> bool {
    parse_version(text).is_some_and(|version| version >= MIN_VERSION)
}

/// [`available`]'s rule: `probe` runs only on a platform that has rsync.
fn gated(has_rsync: bool, probe: impl FnOnce() -> bool) -> bool {
    has_rsync && probe()
}

/// How to reach the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// `user@host`, or an alias `~/.ssh/config` knows. The user, when there is
    /// one, goes to `ssh` as `-l`, so the host can come after a `--`.
    pub destination: String,
    /// `None` leaves the port to `~/.ssh/config`.
    pub port: Option<u16>,
    /// `None` leaves the key to `~/.ssh/config` and the agent.
    pub key: Option<PathBuf>,
    /// Run this instead of `ssh`: it is handed the destination and then the
    /// command, and runs the command on the other machine.
    ///
    /// The seam [`crate::vfs::Service::program`] is for SFTP, here for a
    /// shell: a stand-in that simply runs its command on this machine makes
    /// the whole remote path — dry run, run, far-side `sha256sum` — testable
    /// with real `rsync` and no network. A real field rather than a test-only
    /// one, for the reason the vfs gives: a seam the shipping code does not
    /// have is not the code that ships.
    pub program: Option<PathBuf>,
}

impl Host {
    /// A host reached by its `~/.ssh/config` alias and nothing else.
    pub fn alias(name: impl Into<String>) -> Host {
        Host {
            destination: name.into(),
            port: None,
            key: None,
            program: None,
        }
    }

    /// The machine, without the user: what goes after `--` and before `:`.
    pub fn host(&self) -> &str {
        match self.destination.rsplit_once('@') {
            Some((_, host)) => host,
            None => &self.destination,
        }
    }

    /// The user, when the destination names one.
    fn user(&self) -> Option<&str> {
        self.destination.rsplit_once('@').map(|(user, _)| user)
    }

    /// The command that runs `script` on the server.
    fn shell(&self, script: String) -> Command {
        match &self.program {
            Some(program) => {
                let mut command = Command::new(program);
                command.arg(&self.destination).arg(script);
                command
            }
            None => {
                let mut ssh = Command::new("ssh");
                ssh.args(self.ssh_options()).arg(self.host()).arg(script);
                ssh
            }
        }
    }

    /// The options every `ssh` this module starts gets, ending in `--`: the
    /// host that follows is a host even if a hostile `vfs.toml` or alias made
    /// it start with a dash.
    fn ssh_options(&self) -> Vec<OsString> {
        let mut options: Vec<OsString> = ["-x", "-o", "BatchMode=yes", "-o"]
            .iter()
            .map(OsString::from)
            .collect();
        options.push(format!("ConnectTimeout={}", crate::vfs::CONNECT_TIMEOUT.as_secs()).into());
        if let Some(port) = self.port {
            options.push("-p".into());
            options.push(port.to_string().into());
        }
        if let Some(key) = &self.key {
            options.push("-i".into());
            options.push(key.clone().into_os_string());
        }
        if let Some(user) = self.user() {
            options.push("-l".into());
            options.push(user.into());
        }
        options.push("--".into());
        options
    }

    /// The same options as the one string `rsync -e` takes, `rsync` appending
    /// the host and its own command after them.
    ///
    /// Every word is single-quoted, with a quote inside it doubled. That is
    /// `rsync`'s own rule for `-e`: it splits the string itself rather than
    /// through a shell, it knows no backslash, and the shell's `'\''` is a
    /// syntax error to it ("Missing trailing-' in remote-shell command").
    pub fn rsh(&self) -> String {
        self.rsh_with("ssh")
    }

    fn rsh_with(&self, ssh: &str) -> String {
        if let Some(program) = &self.program {
            return rsync_quote(&program.to_string_lossy());
        }
        let mut words = vec![rsync_quote(ssh)];
        words.extend(
            self.ssh_options()
                .iter()
                .map(|option| rsync_quote(&option.to_string_lossy())),
        );
        words.join(" ")
    }
}

/// One word for `rsync -e`: single-quoted, a quote inside doubled.
fn rsync_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "''"))
}

/// Which way the bytes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From this machine to the server.
    Upload,
    /// From the server to this machine.
    Download,
}

/// One `rsync` sync: which machine, which way, and the paths as each side
/// names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub host: Host,
    pub direction: Direction,
    /// The sources as their own side names them: paths on this machine for an
    /// upload, paths on the server — relative to the login directory, or
    /// absolute — for a download.
    pub sources: Vec<PathBuf>,
    /// The destination folder, as the other side names it.
    pub dest: PathBuf,
}

impl Transfer {
    fn remote_sources(&self) -> bool {
        self.direction == Direction::Download
    }

    /// `path` as `rsync` should see it: `host:path` on the server, the path
    /// itself here. A source loses any trailing slash — `rsync` reads
    /// `photos/` as "the contents of photos", and a synced folder has to land
    /// as `dest/photos`, the way a paste lands it. The destination gains one,
    /// so it is always taken as a folder; the server's login directory is
    /// `host:` alone, since `host:/` would be its root.
    fn endpoint(&self, path: &Path, remote: bool, folder: bool) -> Result<OsString> {
        let mut bytes = platform::os::as_bytes(path.as_os_str())?.into_owned();
        while bytes.len() > 1 && bytes.last() == Some(&b'/') {
            bytes.pop();
        }
        if folder && !bytes.is_empty() && bytes.last() != Some(&b'/') {
            bytes.push(b'/');
        }
        let mut out = OsString::new();
        if remote {
            out.push(format!("{}:", self.host.host()));
        }
        out.push(platform::os::from_bytes(&bytes)?);
        Ok(out)
    }

    /// The paths at the end of every command line: `--`, then the sources,
    /// then the destination. The `--` keeps a server path that starts with a
    /// dash from being read as an option.
    fn endpoints(&self) -> Result<Vec<OsString>> {
        let mut out = vec![OsString::from("--")];
        for source in &self.sources {
            out.push(self.endpoint(source, self.remote_sources(), false)?);
        }
        out.push(self.endpoint(&self.dest, !self.remote_sources(), true)?);
        Ok(out)
    }

    fn common(&self) -> Vec<OsString> {
        vec!["-e".into(), self.host.rsh().into()]
    }

    /// The dry run the card's plan comes from: every path itemized, the
    /// unchanged ones included (`-ii`) so the card can count them, and
    /// `--delete-after` always, so the extras are known before `m` asks for
    /// them. Each line is `%i %l %n`: the change, the length, the name.
    pub fn dry_run_args(&self, content: bool) -> Result<Vec<OsString>> {
        let mut args: Vec<OsString> =
            ["-a", "-n", "-ii", "--delete-after", "--out-format=%i %l %n"]
                .iter()
                .map(OsString::from)
                .collect();
        if content {
            args.push("--checksum".into());
        }
        args.extend(self.common());
        args.extend(self.endpoints()?);
        Ok(args)
    }

    /// The run itself. `--no-inc-recursive` makes `rsync` count the whole
    /// transfer before it starts, so the progress line is about all of it
    /// rather than about what has been found so far; each path it writes or
    /// deletes is itemized as the dry run's are, which is how the report
    /// counts what really landed; `fsync` asks the receiving side to flush
    /// each file it writes ([`fsync_for`]).
    pub fn run_args(&self, mode: Mode, content: bool, fsync: bool) -> Result<Vec<OsString>> {
        let mut args: Vec<OsString> = [
            "-a",
            "--info=progress2",
            "--no-inc-recursive",
            "--out-format=%i %l %n",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        if mode == Mode::Mirror {
            args.push("--delete-after".into());
        }
        if content {
            args.push("--checksum".into());
        }
        if fsync {
            args.push("--fsync".into());
        }
        args.extend(self.common());
        args.extend(self.endpoints()?);
        Ok(args)
    }
}

/// The first `rsync` with `--fsync`.
const FSYNC_SINCE: (u32, u32, u32) = (3, 2, 0);

/// This machine's `rsync` version, from `rsync --version`.
pub fn local_version() -> Option<(u32, u32, u32)> {
    let out = Command::new("rsync")
        .arg("--version")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// The version on `rsync --version`'s first line — `rsync  version
/// 3.5.0-g471e17dc  protocol version 32` is `(3, 5, 0)`; a missing third
/// number is 0, a `v` before the numbers (a build from a git tag) is not part
/// of them, and neither is anything after them (a git suffix, `pre1`).
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let line = text.lines().next()?;
    let mut words = line.split_whitespace();
    if words.next()? != "rsync" || words.next()? != "version" {
        return None;
    }
    let word = words.next()?;
    let mut numbers = word
        .strip_prefix('v')
        .unwrap_or(word)
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u32>().ok()
        });
    let major = numbers.next()??;
    let minor = numbers.next().flatten().unwrap_or(0);
    let patch = numbers.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

/// Whether a run should ask for `--fsync`: only on an upload, where the
/// receiver is the server's `rsync` and nobody else will flush what it wrote,
/// and only when this `rsync` is new enough to send the option. A download's
/// files are flushed here instead, with the same calls a local sync uses.
pub fn fsync_for(direction: Direction, version: Option<(u32, u32, u32)>) -> bool {
    direction == Direction::Upload && version.is_some_and(|version| version >= FSYNC_SINCE)
}

// ── The dry run ─────────────────────────────────────────────────────────────

/// One line of `rsync -ii --out-format='%i %l %n'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Itemized {
    pub class: Class,
    pub kind: Kind,
    /// Relative to the destination, without the trailing slash a folder is
    /// printed with.
    pub name: PathBuf,
    /// `%l`: a file's length. Zero for a deletion, which `rsync` does not
    /// measure.
    pub len: u64,
}

/// Read a dry run's itemized lines.
///
/// The first eleven bytes are the change (`>f.st......`, `cd+++++++++`,
/// `*deleting  `): what happens, to what kind of thing, and — all `+` — that
/// it is new. A `.` in front is a path that is already right, `*deleting` an
/// extra. Anything else on stdout is not an itemized line and is passed over.
/// Names are bytes, with `rsync`'s `\#ooo` escapes for unprintable ones undone.
pub fn parse_itemized(output: &[u8]) -> Vec<Itemized> {
    output
        .split(|&byte| byte == b'\n')
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &[u8]) -> Option<Itemized> {
    if line.len() < 14 || line[11] != b' ' {
        return None;
    }
    let (code, rest) = (&line[..11], &line[12..]);
    let gap = rest.iter().position(|&byte| byte == b' ')?;
    let len = std::str::from_utf8(&rest[..gap]).ok()?.parse().ok()?;
    let mut name = unescape(&rest[gap + 1..]);
    let folder = name.last() == Some(&b'/');
    while name.len() > 1 && name.last() == Some(&b'/') {
        name.pop();
    }
    if name.is_empty() || name == b"." {
        return None;
    }
    let name = PathBuf::from(platform::os::from_bytes(&name).ok()?);
    if code.starts_with(b"*deleting") {
        return Some(Itemized {
            class: Class::Extra,
            kind: if folder { Kind::Dir } else { Kind::File },
            name,
            len,
        });
    }
    let kind = match code[1] {
        b'f' => Kind::File,
        b'd' => Kind::Dir,
        b'L' => Kind::Symlink,
        b'D' | b'S' => Kind::Special,
        _ => return None,
    };
    let class = match code[0] {
        b'.' => Class::Unchanged,
        b'<' | b'>' | b'c' | b'h' if code[2..].iter().all(|&byte| byte == b'+') => Class::New,
        b'<' | b'>' | b'c' | b'h' => Class::Changed,
        _ => return None,
    };
    Some(Itemized {
        class,
        kind,
        name,
        len,
    })
}

/// Undo `rsync`'s `\#ooo` escapes, three octal digits standing for one byte.
fn unescape(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    let mut at = 0;
    while at < name.len() {
        if name[at] == b'\\' && name.get(at + 1) == Some(&b'#') && at + 5 <= name.len() {
            let digits = &name[at + 2..at + 5];
            if digits.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                let value = digits
                    .iter()
                    .fold(0u32, |value, byte| value * 8 + u32::from(byte - b'0'));
                if let Ok(byte) = u8::try_from(value) {
                    out.push(byte);
                    at += 5;
                    continue;
                }
            }
        }
        out.push(name[at]);
        at += 1;
    }
    out
}

/// Make a [`SyncPlan`] of a dry run's lines, the way the local walk would
/// have made it: each line under the root its first component names, a
/// folder counted only when nothing is listed under it.
pub fn plan_from(
    lines: Vec<Itemized>,
    roots: Vec<Root>,
    dest_dir: PathBuf,
    options: SyncOptions,
    transfer: Transfer,
) -> SyncPlan {
    let names: HashMap<OsString, usize> = roots
        .iter()
        .enumerate()
        .filter_map(|(index, root)| Some((root.dst.file_name()?.to_os_string(), index)))
        .collect();
    let mut plan = SyncPlan::empty(roots, dest_dir, options);
    for line in lines {
        let mut components = line.name.components();
        let Some(first) = components.next() else {
            continue;
        };
        let Some(&root) = names.get(first.as_os_str()) else {
            continue;
        };
        plan.items.push(Item {
            root,
            rel: components.as_path().to_path_buf(),
            class: line.class,
            kind: line.kind,
            bytes: if line.kind == Kind::File { line.len } else { 0 },
            leaf: true,
        });
    }
    let parents: HashSet<(usize, PathBuf)> = plan
        .items
        .iter()
        .flat_map(|item| {
            item.rel
                .ancestors()
                .skip(1)
                .map(move |parent| (item.root, parent.to_path_buf()))
        })
        .collect();
    for item in plan.items.iter_mut().filter(|item| item.kind == Kind::Dir) {
        item.leaf = !parents.contains(&(item.root, item.rel.clone()));
    }
    plan.count();
    plan.removal = Removal::Delete;
    plan.remote = Some(transfer);
    plan
}

/// Plan a sync with a server: run the dry run and read it.
///
/// `stop` is asked while `rsync` runs; once it says yes the child is killed
/// and the answer is [`DfError::Cancelled`]. A run `rsync` calls partial (a
/// file it could not read, one that vanished) is still a plan, with what went
/// wrong in [`SyncPlan::skipped`]; one that could not happen at all — `ssh`
/// could not connect, a path is not there — is an error in `rsync`'s words.
pub fn plan(
    transfer: Transfer,
    roots: Vec<Root>,
    dest_dir: PathBuf,
    options: SyncOptions,
    stop: &dyn Fn() -> bool,
) -> Result<SyncPlan> {
    let mut command = Command::new("rsync");
    command.args(transfer.dry_run_args(options.content)?);
    let ran = collect(command, None, stop)?;
    let Some(status) = ran.status else {
        return Err(DfError::Cancelled);
    };
    if !status.success() && !status.code().is_some_and(|code| PARTIAL.contains(&code)) {
        return Err(DfError::Op(failure_line(&ran.stderr, status)));
    }
    let mut plan = plan_from(
        parse_itemized(&ran.stdout),
        roots,
        dest_dir,
        options,
        transfer,
    );
    plan.skipped.extend(complaints(&ran.stderr));
    Ok(plan)
}

/// `rsync`'s complaints about particular paths, as `(path, what)`: each
/// `rsync: … "path": reason` line, with the path in the quotes.
fn complaints(stderr: &str) -> Vec<(PathBuf, String)> {
    stderr
        .lines()
        .filter(|line| line.starts_with("rsync:"))
        .filter_map(|line| {
            let open = line.find('"')?;
            let close = open + 1 + line[open + 1..].find('"')?;
            let why = line[close + 1..].trim_start_matches(':').trim();
            Some((PathBuf::from(&line[open + 1..close]), why.to_string()))
        })
        .collect()
}

/// The one line worth showing from a failed `rsync` or `ssh`: the first that
/// says something, or the exit status when nothing did.
fn failure_line(stderr: &str, status: ExitStatus) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_start_matches("rsync: ").to_string())
        .unwrap_or_else(|| format!("rsync failed ({status})"))
}

// ── Running a child ─────────────────────────────────────────────────────────

/// What a child printed, and how it ended — `None` when `stop` killed it.
struct Collected {
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: String,
}

/// Run `command` to its end, feeding it `input`, keeping all of stdout and
/// the start of stderr, and killing it the moment `stop` says so.
///
/// Every pipe gets a thread of its own: waiting on a child while a pipe it is
/// writing to fills is the classic deadlock, and so is writing a long list to
/// its stdin while it waits to be read from.
fn collect(
    mut command: Command,
    input: Option<Vec<u8>>,
    stop: &dyn Fn() -> bool,
) -> Result<Collected> {
    command
        .env("LC_ALL", "C")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = PathBuf::from(command.get_program());
    let mut child = command.spawn().map_err(|e| DfError::io(&program, e))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            // A child that stops reading early — `ssh` failing to connect —
            // closes the pipe, and the write error is that, not news.
            let _ = stdin.write_all(&input);
        });
    }
    let stdout = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut all = Vec::new();
            let _ = pipe.read_to_end(&mut all);
            all
        })
    });
    let stderr = child.stderr.take().map(drain_stderr);
    let status = wait(&mut child, &program, stop, &mut |_| {})?;
    Ok(Collected {
        status,
        stdout: stdout.and_then(|h| h.join().ok()).unwrap_or_default(),
        stderr: stderr.and_then(|h| h.join().ok()).unwrap_or_default(),
    })
}

/// Read a stderr to its end, keeping the first [`KEEP_STDERR`] bytes.
fn drain_stderr<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let _ = pipe.by_ref().take(KEEP_STDERR).read_to_end(&mut kept);
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        String::from_utf8_lossy(&kept).into_owned()
    })
}

/// Wait for `child`, asking `stop` every [`POLL`] and killing it when it says
/// yes; `tick` is handed the child each time round, for a caller that has
/// more to do while it waits.
fn wait(
    child: &mut Child,
    program: &Path,
    stop: &dyn Fn() -> bool,
    tick: &mut dyn FnMut(&Child),
) -> Result<Option<ExitStatus>> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DfError::io(program, e));
            }
        }
        if stop() {
            // Woken first if a pause had stopped it, or the kill would wait
            // on a process that cannot run to die.
            signal(child, false);
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        tick(child);
        std::thread::sleep(POLL);
    }
}

/// Stop `child` (`pause`) or let it run again: the stop and continue signals
/// on Unix ([`crate::platform::process::pause`]) are how a pause reaches a
/// process that is not ours to checkpoint. A refusal is logged and nothing
/// else — the run goes on.
fn signal(child: &Child, pause: bool) {
    let sent = if pause {
        crate::platform::process::pause(child)
    } else {
        crate::platform::process::resume(child)
    };
    if let Err(e) = sent {
        log::debug!("could not signal rsync ({e})");
    }
}

// ── The run ─────────────────────────────────────────────────────────────────

/// The bytes moved so far, from one line of `--info=progress2`:
/// `     30,000,004  99%    1.40GB/s    0:00:00 (xfr#2, to-chk=7/10)`.
///
/// The first field is the count, grouped with commas (or, in some locales,
/// dots — the child runs under `LC_ALL=C`, and both are dropped anyway); the
/// second must be a percentage, which is what tells a progress line from
/// anything else `rsync` prints.
pub fn progress_bytes(line: &str) -> Option<u64> {
    let mut fields = line.split_whitespace();
    let count = fields.next()?;
    if !fields.next()?.ends_with('%') {
        return None;
    }
    if !count
        .chars()
        .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
    {
        return None;
    }
    let digits: String = count.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Carry a remote plan out with `rsync`, then verify it.
pub(crate) fn execute(
    plan: &SyncPlan,
    transfer: &Transfer,
    mode: Mode,
    verify: Verify,
    ctx: &TaskCtx,
) -> SyncReport {
    let mut report = SyncReport {
        mode,
        verify,
        removal: Removal::Delete,
        skipped: plan.skipped.clone(),
        specials: plan.specials.len() as u64,
        ..SyncReport::default()
    };
    let to_copy = plan.bytes_to_copy();
    let checks = files_to_verify(plan, verify, None);
    let copies = plan
        .items
        .iter()
        .filter(|item| matches!(item.class, Class::New | Class::Changed))
        .count() as u64;
    ctx.set_total(
        to_copy
            + checks
                .iter()
                .map(|&index| plan.items[index].bytes)
                .sum::<u64>(),
        copies + checks.len() as u64,
    );

    let mut fsync = fsync_for(transfer.direction, local_version());
    let mut ran = match run(transfer, mode, plan.options.content, fsync, ctx) {
        Ok(ran) => ran,
        Err(e) => {
            report.errors.push((plan.dest_dir.clone(), e.to_string()));
            return report;
        }
    };
    // A server whose `rsync` predates `--fsync` refuses the whole run before
    // a byte moves, naming the option. That is not a failed sync; it is a
    // sync that has to go without the server's flush, and says so.
    if fsync && refused_fsync(&ran) {
        log::info!(
            "{} refused --fsync; syncing without it",
            transfer.host.destination
        );
        fsync = false;
        ran = match run(transfer, mode, plan.options.content, fsync, ctx) {
            Ok(ran) => ran,
            Err(e) => {
                report.errors.push((plan.dest_dir.clone(), e.to_string()));
                return report;
            }
        };
    }
    report.unflushed = transfer.direction == Direction::Upload && !fsync;
    let Some(status) = ran.status else {
        report.cancelled = true;
        return report;
    };
    let partial = status.code().is_some_and(|code| PARTIAL.contains(&code));
    if !status.success() && !partial {
        report
            .errors
            .push((plan.dest_dir.clone(), failure_line(&ran.stderr, status)));
        return report;
    }
    // What landed is what `rsync` itemized as it went, not what the plan
    // said would: after an exit 23 the two differ by exactly the files it
    // could not send, which it names on stderr instead.
    let landed = landed(plan, &ran.lines);
    report.copied = landed.copied;
    report.copied_bytes = landed.copied_bytes;
    report.made = landed.made;
    report.removed = landed.removed;
    ctx.advance(to_copy.saturating_sub(ran.moved), copies);
    report.errors.extend(complaints(&ran.stderr));
    if partial && report.errors.is_empty() {
        report
            .errors
            .push((plan.dest_dir.clone(), failure_line(&ran.stderr, status)));
    }
    if transfer.direction == Direction::Download {
        flush_here(
            plan,
            landed.items.iter().map(|&index| &plan.items[index]),
            &mut report,
        );
    }
    // Only what landed is read back: a file `rsync` could not send is on the
    // result card once, as the failure it is, not again as missing.
    let checks = files_to_verify(plan, verify, Some(&landed.items));
    verify_remote(plan, transfer, &checks, ctx, &mut report);
    report
}

/// What a run really did, read off its itemized lines.
#[derive(Debug, Default, PartialEq, Eq)]
struct Landed {
    /// The plan's items that were written, by index.
    items: HashSet<usize>,
    copied: u64,
    copied_bytes: u64,
    made: u64,
    /// Removed, counted as the plan counts extras: files, and folders with
    /// nothing else removed under them.
    removed: u64,
}

fn landed(plan: &SyncPlan, lines: &[Itemized]) -> Landed {
    let roots: HashMap<&OsStr, usize> = plan
        .roots
        .iter()
        .enumerate()
        .filter_map(|(index, root)| Some((root.dst.file_name()?, index)))
        .collect();
    let items: HashMap<(usize, &Path), usize> = plan
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| ((item.root, item.rel.as_path()), index))
        .collect();
    let mut out = Landed::default();
    let mut deleted: Vec<(&Itemized, PathBuf)> = Vec::new();
    for line in lines {
        let mut components = line.name.components();
        let Some(&root) = components
            .next()
            .and_then(|first| roots.get(first.as_os_str()))
        else {
            continue;
        };
        let rel = components.as_path();
        match line.class {
            Class::New | Class::Changed => {
                if let Some(&index) = items.get(&(root, rel)) {
                    out.items.insert(index);
                }
                match line.kind {
                    Kind::Dir => out.made += 1,
                    Kind::File => {
                        out.copied += 1;
                        out.copied_bytes += line.len;
                    }
                    _ => out.copied += 1,
                }
            }
            Class::Extra => deleted.push((line, line.name.clone())),
            Class::Unchanged => {}
        }
    }
    let parents: HashSet<&Path> = deleted
        .iter()
        .flat_map(|(_, name)| name.ancestors().skip(1))
        .collect();
    out.removed = deleted
        .iter()
        .filter(|(line, name)| line.kind != Kind::Dir || !parents.contains(name.as_path()))
        .count() as u64;
    out
}

/// Whether a run was refused for `--fsync` alone: it failed, before any
/// progress, with the option named on stderr.
fn refused_fsync(ran: &Ran) -> bool {
    ran.status.is_some_and(|status| !status.success())
        && ran.moved == 0
        && ran.stderr.contains("--fsync")
}

/// Flush what a download wrote on this machine: each file, and each folder
/// that gained a name. `rsync` renames its temporary files into place without
/// an `fsync` unless told, and the verify that follows would otherwise read
/// the page cache — the same reason a local sync flushes (`ops::copy`). A
/// file that cannot be flushed is a problem like a failed copy.
fn flush_here<'a>(plan: &SyncPlan, items: impl Iterator<Item = &'a Item>, report: &mut SyncReport) {
    let mut folders = std::collections::BTreeSet::new();
    for item in items {
        let path = plan.dst_of(item);
        if item.kind == Kind::File {
            if let Err(e) = crate::ops::copy::sync_path(&path) {
                report
                    .errors
                    .push((path.clone(), format!("could not be flushed: {e}")));
            }
        }
        if let Some(parent) = path.parent() {
            folders.insert(parent.to_path_buf());
        }
    }
    for folder in folders {
        if let Err(e) = crate::ops::copy::sync_dir(&folder) {
            report
                .errors
                .push((folder, format!("could not be flushed: {e}")));
        }
    }
}

/// What the run came to: how it ended, what it said, how many bytes the
/// progress line had counted, and every path it itemized.
struct Ran {
    status: Option<ExitStatus>,
    stderr: String,
    moved: u64,
    lines: Vec<Itemized>,
}

/// Run `rsync`, turning its progress line into the task's bar: stopped while
/// the task is paused, killed when it is cancelled.
fn run(transfer: &Transfer, mode: Mode, content: bool, fsync: bool, ctx: &TaskCtx) -> Result<Ran> {
    let mut command = Command::new("rsync");
    command
        .args(transfer.run_args(mode, content, fsync)?)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = PathBuf::from("rsync");
    let mut child = command.spawn().map_err(|e| DfError::io(&program, e))?;
    let counted = Arc::new(AtomicU64::new(0));
    let stdout = child.stdout.take().map(|mut pipe| {
        let counted = Arc::clone(&counted);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut line = Vec::new();
            let mut itemized = Vec::new();
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for &byte in &buf[..n] {
                            if byte == b'\r' || byte == b'\n' {
                                // The progress line is rewritten in place with
                                // `\r`; an itemized path ends its own line.
                                if let Some(bytes) = progress_bytes(&String::from_utf8_lossy(&line))
                                {
                                    counted.fetch_max(bytes, Ordering::Relaxed);
                                } else if byte == b'\n' {
                                    itemized.extend(parse_line(&line));
                                }
                                line.clear();
                            } else {
                                line.push(byte);
                            }
                        }
                    }
                }
            }
            itemized
        })
    });
    let stderr = child.stderr.take().map(drain_stderr);
    let flags = ctx.flags();
    let mut reported = 0u64;
    let mut stopped = false;
    let status = wait(&mut child, &program, &|| ctx.is_cancelled(), &mut |child| {
        let now = counted.load(Ordering::Relaxed);
        if now > reported {
            ctx.advance(now - reported, 0);
            reported = now;
        }
        if flags.is_paused() != stopped {
            stopped = flags.is_paused();
            signal(child, stopped);
        }
    })?;
    let lines = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let last = counted.load(Ordering::Relaxed);
    if last > reported {
        ctx.advance(last - reported, 0);
        reported = last;
    }
    Ok(Ran {
        status,
        stderr: stderr.and_then(|h| h.join().ok()).unwrap_or_default(),
        moved: reported,
        lines,
    })
}

// ── The verify ──────────────────────────────────────────────────────────────

/// The files a verify of `plan` reads, by index: what was copied, or every
/// file of the source. Regular files only — `sha256sum` follows links, and a
/// link's target text is not something it can be asked about.
///
/// `landed`, once the run is over, is what it really wrote: before it, the
/// plan's own list stands in, which is what the progress total is sized by.
fn files_to_verify(plan: &SyncPlan, verify: Verify, landed: Option<&HashSet<usize>>) -> Vec<usize> {
    plan.items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.kind == Kind::File)
        .filter(|(index, item)| {
            let copied = match landed {
                Some(landed) => landed.contains(index),
                None => matches!(item.class, Class::New | Class::Changed),
            };
            match verify {
                Verify::Copied => copied,
                Verify::Everything => copied || item.class == Class::Unchanged,
            }
        })
        .map(|(index, _)| index)
        .collect()
}

/// Where `item` is on the server, as `(folder, name under it)`: the folder a
/// `sha256sum` is run in, and the name it is handed. An upload's files are all
/// under the destination; a download's are under each source's own parent.
fn far_side(plan: &SyncPlan, transfer: &Transfer, item: &Item) -> (PathBuf, PathBuf) {
    let root = &plan.roots[item.root];
    let name = root.dst.file_name().map(PathBuf::from).unwrap_or_default();
    let name = if item.rel.as_os_str().is_empty() {
        name
    } else {
        name.join(&item.rel)
    };
    let folder = match transfer.direction {
        Direction::Upload => transfer.dest.clone(),
        Direction::Download => transfer
            .sources
            .get(item.root)
            .and_then(|source| source.parent())
            .map(Path::to_path_buf)
            .unwrap_or_default(),
    };
    (folder, name)
}

/// Where `item` is on this machine.
fn near_side(plan: &SyncPlan, transfer: &Transfer, item: &Item) -> PathBuf {
    match transfer.direction {
        Direction::Upload => plan.src_of(item),
        Direction::Download => plan.dst_of(item),
    }
}

/// Hash both sides of each file in `checks` and compare: the server's with
/// its own `sha256sum`, this machine's with [`crate::sha256`] after dropping
/// the page cache.
///
/// The server is asked first, one `ssh` per folder, with the names handed to
/// `xargs -0 sha256sum` on stdin — a list of any length, and any name, never
/// passes through a shell. When that cannot happen at all (no `sha256sum`
/// there, no shell, `ssh` refused), nothing is compared and the report says
/// the copy was verified locally only.
fn verify_remote(
    plan: &SyncPlan,
    transfer: &Transfer,
    checks: &[usize],
    ctx: &TaskCtx,
    report: &mut SyncReport,
) {
    if checks.is_empty() {
        return;
    }
    let mut by_folder: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for &index in checks {
        let (folder, name) = far_side(plan, transfer, &plan.items[index]);
        by_folder.entry(folder).or_default().push(name);
    }
    let mut theirs: HashMap<(PathBuf, PathBuf), [u8; 32]> = HashMap::new();
    for (folder, names) in &by_folder {
        match remote_digests(&transfer.host, folder, names, ctx) {
            Ok(Some(digests)) => {
                for (name, digest) in digests {
                    theirs.insert((folder.clone(), name), digest);
                }
            }
            Ok(None) => {
                report.local_only = true;
                return;
            }
            Err(DfError::Cancelled) => {
                report.cancelled = true;
                return;
            }
            Err(e) => {
                log::warn!("verify on {}: {e}", transfer.host.destination);
                report.local_only = true;
                return;
            }
        }
    }
    let mut buf = vec![0u8; crate::ops::COPY_CHUNK];
    for &index in checks {
        let item = &plan.items[index];
        let here = near_side(plan, transfer, item);
        let ours = match super::execute::read_back(&here, ctx, &mut buf) {
            Ok(digest) => digest,
            Err(DfError::Cancelled) => {
                report.cancelled = true;
                return;
            }
            Err(e) => {
                report.verify_failures.push((here, e.to_string()));
                continue;
            }
        };
        ctx.advance(0, 1);
        match theirs.get(&far_side(plan, transfer, item)) {
            Some(digest) if *digest == ours => report.verified += 1,
            Some(_) => report
                .verify_failures
                .push((here, "contents differ on the server".to_string())),
            None => report
                .verify_failures
                .push((here, "missing on the server".to_string())),
        }
    }
}

/// Each file's name and SHA-256, as a `sha256sum` reported them.
pub type Digests = Vec<(PathBuf, [u8; 32])>;

/// `sha256sum` of `names` in `folder` on the server: each name's digest, or
/// `None` when the server has no way to answer.
fn remote_digests(
    host: &Host,
    folder: &Path,
    names: &[PathBuf],
    ctx: &TaskCtx,
) -> Result<Option<Digests>> {
    let folder = if folder.as_os_str().is_empty() {
        Path::new(".")
    } else {
        folder
    };
    let command = host.shell(remote_script(folder));
    let mut input = Vec::new();
    for name in names {
        input.extend_from_slice(&platform::os::as_bytes(name.as_os_str())?);
        input.push(0);
    }
    let ran = collect(command, Some(input), &|| ctx.is_cancelled())?;
    let Some(status) = ran.status else {
        return Err(DfError::Cancelled);
    };
    // 255 is `ssh` itself failing; 126 and 127 are the shell finding no
    // `xargs` or no `sha256sum`. `sha256sum` exits 1 when some file was
    // missing, and its output for the rest still stands.
    if matches!(status.code(), Some(255) | Some(126) | Some(127) | None) {
        log::info!(
            "no sha256sum on {}: {}",
            host.destination,
            failure_line(&ran.stderr, status)
        );
        return Ok(None);
    }
    let digests = parse_sha256sum(&ran.stdout);
    if digests.is_empty() && !status.success() {
        return Ok(None);
    }
    Ok(Some(digests))
}

/// The command the server's shell runs: into the folder, then `sha256sum` on
/// every name read from stdin. The folder is single-quoted for that shell —
/// the one string here that passes through one.
fn remote_script(folder: &Path) -> String {
    if folder.to_str().is_none() {
        log::warn!(
            "{} is not UTF-8; the server is asked about {} instead",
            folder.display(),
            folder.to_string_lossy()
        );
    }
    let quoted = folder.to_string_lossy().replace('\'', r"'\''");
    format!("cd -- '{quoted}' && xargs -0 sha256sum --")
}

/// Read `sha256sum` output: `<64 hex>  <name>` per line, or — for a name with
/// a newline or a backslash in it — the same line with a `\` in front and
/// those two escaped as `\n` and `\\`.
pub fn parse_sha256sum(output: &[u8]) -> Digests {
    let mut out = Vec::new();
    for line in output.split(|&byte| byte == b'\n') {
        let (escaped, line) = match line.strip_prefix(b"\\") {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        if line.len() < 66 || &line[64..66] != b"  " {
            continue;
        }
        let Some(digest) = decode_hex(&line[..64]) else {
            continue;
        };
        let mut name = line[66..].to_vec();
        if escaped {
            name = unescape_sha256sum(&name);
        }
        if let Some(rest) = name.strip_prefix(b"./") {
            name = rest.to_vec();
        }
        let Ok(name) = platform::os::from_bytes(&name) else {
            continue;
        };
        out.push((PathBuf::from(name), digest));
    }
    out
}

fn unescape_sha256sum(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    let mut bytes = name.iter();
    while let Some(&byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        match bytes.next() {
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            Some(&other) => out.push(other),
            None => out.push(b'\\'),
        }
    }
    out
}

fn decode_hex(hex: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.chunks(2)) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (*pair.get(1)? as char).to_digit(16)?;
        *slot = u8::try_from(high * 16 + low).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;

    /// A platform without rsync (Windows) answers "no" without spawning
    /// anything; one with it asks the probe, whatever the probe says.
    #[test]
    fn rsync_is_asked_for_only_where_the_platform_has_it() {
        assert!(!gated(false, || panic!("nothing is spawned without rsync")));
        assert!(gated(true, || true));
        assert!(!gated(true, || false));
    }

    fn upload() -> Transfer {
        Transfer {
            host: Host {
                destination: "brian@showandtour1".to_string(),
                port: Some(2222),
                key: Some(PathBuf::from("/home/brian/my keys/id_ed25519")),
                program: None,
            },
            direction: Direction::Upload,
            sources: vec![
                PathBuf::from("/home/brian/Photos/2024/"),
                PathBuf::from("/home/brian/notes.txt"),
            ],
            dest: PathBuf::from("backups/photos"),
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    const RSH: &str = "'ssh' '-x' '-o' 'BatchMode=yes' '-o' 'ConnectTimeout=15' '-p' '2222' '-i' '/home/brian/my keys/id_ed25519' '-l' 'brian' '--'";

    #[test]
    fn the_run_is_rsync_archive_with_the_whole_transfer_counted_up_front() {
        let t = upload();
        assert_eq!(
            strings(&t.run_args(Mode::Update, false, false).unwrap()),
            [
                "-a",
                "--info=progress2",
                "--no-inc-recursive",
                "--out-format=%i %l %n",
                "-e",
                RSH,
                "--",
                "/home/brian/Photos/2024",
                "/home/brian/notes.txt",
                "showandtour1:backups/photos/",
            ]
        );
        let mirror = strings(&t.run_args(Mode::Mirror, true, true).unwrap());
        assert_eq!(mirror[4..7], ["--delete-after", "--checksum", "--fsync"]);
        assert!(
            !mirror.contains(&"--delete".to_string()),
            "after, not during"
        );
    }

    #[test]
    fn the_dry_run_itemizes_everything_and_always_looks_for_extras() {
        let t = upload();
        assert_eq!(
            strings(&t.dry_run_args(false).unwrap())[..5],
            ["-a", "-n", "-ii", "--delete-after", "--out-format=%i %l %n"]
        );
        assert!(strings(&t.dry_run_args(true).unwrap()).contains(&"--checksum".to_string()));
    }

    #[test]
    fn a_download_names_the_server_on_the_sources_and_its_home_as_host_colon() {
        let t = Transfer {
            host: Host::alias("showandtour1"),
            direction: Direction::Download,
            sources: vec![PathBuf::from("photos/2024"), PathBuf::from("/srv/-odd")],
            dest: PathBuf::from("/home/brian/Pictures"),
        };
        assert_eq!(
            strings(&t.endpoints().unwrap()),
            [
                "--",
                "showandtour1:photos/2024",
                "showandtour1:/srv/-odd",
                "/home/brian/Pictures/",
            ]
        );
        let home = Transfer {
            direction: Direction::Upload,
            sources: vec![PathBuf::from("/tmp/a")],
            dest: PathBuf::new(),
            ..t
        };
        assert_eq!(
            strings(&home.endpoints().unwrap()).last().unwrap(),
            "showandtour1:",
            "the login directory, not the server's root"
        );
        assert_eq!(
            Host::alias("showandtour1").rsh(),
            "'ssh' '-x' '-o' 'BatchMode=yes' '-o' 'ConnectTimeout=15' '--'"
        );
    }

    /// Captured from `rsync -a -n -ii --delete --out-format='%i %l %n'` run
    /// between two local folders (rsync 3.5.0): a source `photos` with a new
    /// folder, a new and a grown file in an existing one, a new link and an
    /// unchanged file, and a destination with a nested extra folder and an
    /// extra file.
    const DRY_RUN: &str = "\
>f+++++++++ 4 solo.txt
*deleting   0 photos/old/gone.jpg
*deleting   0 photos/old/
*deleting   0 photos/extra.txt
.d          140 photos/
cL+++++++++ 10 photos/link
>f+++++++++ 10 photos/name with space.txt
.f          4 photos/same.txt
.d          80 photos/2024/
>f+++++++++ 4 photos/2024/a.jpg
>f.s....... 8 photos/2024/b.jpg
cd+++++++++ 40 photos/empty/
";

    #[test]
    fn a_dry_run_reads_as_new_changed_unchanged_and_extra() {
        let lines = parse_itemized(DRY_RUN.as_bytes());
        let got: Vec<(Class, Kind, String, u64)> = lines
            .iter()
            .map(|line| {
                (
                    line.class,
                    line.kind,
                    line.name.display().to_string(),
                    line.len,
                )
            })
            .collect();
        use Class::*;
        use Kind::*;
        assert_eq!(
            got,
            vec![
                (New, File, "solo.txt".into(), 4),
                (Extra, File, "photos/old/gone.jpg".into(), 0),
                (Extra, Dir, "photos/old".into(), 0),
                (Extra, File, "photos/extra.txt".into(), 0),
                (Unchanged, Dir, "photos".into(), 140),
                (New, Symlink, "photos/link".into(), 10),
                (New, File, "photos/name with space.txt".into(), 10),
                (Unchanged, File, "photos/same.txt".into(), 4),
                (Unchanged, Dir, "photos/2024".into(), 80),
                (New, File, "photos/2024/a.jpg".into(), 4),
                (Changed, File, "photos/2024/b.jpg".into(), 8),
                (New, Dir, "photos/empty".into(), 40),
            ]
        );
    }

    #[test]
    fn a_dry_run_becomes_the_plan_the_card_reads() {
        let roots = vec![
            Root {
                src: PathBuf::from("/src/photos"),
                dst: PathBuf::from("sftp://host/dst/photos"),
            },
            Root {
                src: PathBuf::from("/src/solo.txt"),
                dst: PathBuf::from("sftp://host/dst/solo.txt"),
            },
        ];
        let plan = plan_from(
            parse_itemized(DRY_RUN.as_bytes()),
            roots,
            PathBuf::from("sftp://host/dst"),
            SyncOptions::default(),
            upload(),
        );
        // New: solo.txt, link, the spaced name, a.jpg and the empty folder.
        assert_eq!(
            plan.new,
            super::super::Tally {
                count: 5,
                bytes: 18
            }
        );
        assert_eq!(plan.changed.count, 1);
        assert_eq!(
            plan.unchanged.count, 1,
            "same.txt; the folders are not things"
        );
        // gone.jpg and extra.txt; `old/` goes with gone.jpg.
        assert_eq!(plan.extra.count, 2);
        assert_eq!(plan.removal, Removal::Delete);
        assert!(plan.remote.is_some());
        // Read with slashes: a label is in the platform's separator, and this
        // test reads rsync's output on every platform.
        assert_eq!(
            plan.listed(Mode::Mirror)
                .map(|item| crate::path::with_slashes(&plan.label(item)).into_owned())
                .collect::<Vec<_>>(),
            [
                "solo.txt",
                "photos/link",
                "photos/name with space.txt",
                "photos/2024/a.jpg",
                "photos/2024/b.jpg",
                "photos/empty/",
                "photos/old/gone.jpg",
                "photos/old/",
                "photos/extra.txt",
            ]
        );
    }

    #[test]
    fn an_escaped_name_comes_back_as_its_bytes() {
        let lines = parse_itemized(b">f+++++++++ 3 photos/tab\\#011here.txt\n");
        assert_eq!(lines[0].name, PathBuf::from("photos/tab\there.txt"));
        assert_eq!(unescape(b"no\\#escape"), b"no\\#escape");
    }

    #[test]
    fn noise_on_stdout_is_not_a_path() {
        assert!(parse_itemized(b"sending incremental file list\n\nsent 1 bytes\n").is_empty());
    }

    /// Captured from `rsync -a --info=progress2 --no-inc-recursive` copying a
    /// 30 MB file and four small ones (rsync 3.5.0): `\r` between updates,
    /// a line ending at each file.
    const PROGRESS: &str = "\r              4   0%    0.00kB/s    0:00:00  \
\r              4   0%    0.00kB/s    0:00:00 (xfr#1, to-chk=9/10)\
\r     30,000,004  99%    1.40GB/s    0:00:00 (xfr#2, to-chk=7/10)\
\r     30,000,014  99%    1.40GB/s    0:00:00 (xfr#3, to-chk=5/10)\
\r     30,000,030 100%    1.40GB/s    0:00:00 (xfr#6, to-chk=0/10)\n";

    #[test]
    fn the_progress_line_is_read_as_bytes_so_far() {
        let counts: Vec<u64> = PROGRESS
            .split(['\r', '\n'])
            .filter_map(progress_bytes)
            .collect();
        assert_eq!(counts, [4, 4, 30_000_004, 30_000_014, 30_000_030]);
        assert_eq!(progress_bytes("sent 1,234 bytes  received 5 bytes"), None);
        assert_eq!(
            progress_bytes("     1.234.567  12%  1MB/s 0:00:01"),
            Some(1_234_567)
        );
        assert_eq!(progress_bytes(""), None);
    }

    #[test]
    fn sha256sum_output_is_read_names_and_all() {
        let good = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let output = format!(
            "{good}  photos/a.jpg\n{good}  ./photos/b c.jpg\n\\{good}  photos/new\\nline\\\\x\nsha256sum: gone.jpg: No such file or directory\n"
        );
        let parsed = parse_sha256sum(output.as_bytes());
        let names: Vec<String> = parsed
            .iter()
            .map(|(name, _)| name.display().to_string())
            .collect();
        assert_eq!(
            names,
            ["photos/a.jpg", "photos/b c.jpg", "photos/new\nline\\x"]
        );
        assert_eq!(crate::sha256::hex(&parsed[0].1), good);
    }

    #[test]
    fn rsyncs_complaints_name_their_paths() {
        let stderr = "rsync: [sender] send_files failed to open \"/src/photos/locked.jpg\": Permission denied (13)\n\
rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1338) [sender=3.5.0]\n";
        assert_eq!(
            complaints(stderr),
            [(
                PathBuf::from("/src/photos/locked.jpg"),
                "Permission denied (13)".to_string()
            )]
        );
    }

    // ── End to end, with a stand-in for ssh ─────────────────────────────────

    #[cfg(unix)]
    use crate::ops::fixture::TempTree;
    #[cfg(unix)]
    use crate::tasks::TaskCtx;

    /// A "server" that is this machine: `ssh`'s stand-in drops the destination
    /// and runs the rest. `fail_verify` makes it answer a `sha256sum` the way a
    /// server with none would.
    #[cfg(unix)]
    fn server(t: &TempTree, fail_verify: bool) -> Host {
        use std::os::unix::fs::PermissionsExt;
        let refuse = if fail_verify {
            "case \"$*\" in *sha256sum*) echo 'sha256sum: command not found' >&2; exit 127;; esac\n"
        } else {
            ""
        };
        let script = t.file(
            "bin/fake-ssh",
            format!("#!/bin/sh\nshift\n{refuse}exec sh -c \"$*\"\n").as_bytes(),
        );
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Host {
            destination: "fake".to_string(),
            port: None,
            key: None,
            program: Some(script),
        }
    }

    #[cfg(unix)]
    fn remote_plan(transfer: &Transfer, t: &TempTree) -> SyncPlan {
        let (sources, dest): (Vec<PathBuf>, PathBuf) = match transfer.direction {
            Direction::Upload => (transfer.sources.clone(), t.join("server-view")),
            Direction::Download => (transfer.sources.clone(), transfer.dest.clone()),
        };
        let roots = sources
            .iter()
            .map(|src| Root {
                src: src.clone(),
                dst: dest.join(src.file_name().unwrap()),
            })
            .collect();
        plan(
            transfer.clone(),
            roots,
            dest,
            SyncOptions::default(),
            &|| false,
        )
        .unwrap()
    }

    #[cfg(unix)]
    fn rsync_here() -> bool {
        let here = available();
        if !here {
            eprintln!("no rsync 3.1 or newer is installed; skipping");
        }
        here
    }

    #[cfg(unix)]
    #[test]
    fn an_upload_is_planned_run_and_verified_on_the_far_side() {
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-upload");
        let photos = t.dir("src/photos");
        t.file("src/photos/a.jpg", b"aaaa");
        t.file("src/photos/2024/b.jpg", b"bbbbbbbb");
        let server_home = t.dir("server");
        t.file("server/photos/extra.txt", b"only there");
        let transfer = Transfer {
            host: server(&t, false),
            direction: Direction::Upload,
            sources: vec![photos],
            dest: server_home.clone(),
        };

        let plan = remote_plan(&transfer, &t);
        assert_eq!((plan.new.count, plan.new.bytes), (2, 12));
        assert_eq!(plan.extra.count, 1);
        assert_eq!(plan.removal, Removal::Delete);

        let report =
            super::super::execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
        assert_eq!(report.problems(), 0, "{report:?}");
        assert_eq!((report.copied, report.copied_bytes), (2, 12));
        assert_eq!(report.verified, 2);
        assert!(!report.local_only);
        assert_eq!(
            std::fs::read(server_home.join("photos/2024/b.jpg")).unwrap(),
            b"bbbbbbbb"
        );
        assert!(
            server_home.join("photos/extra.txt").exists(),
            "an update removes nothing"
        );

        let again = remote_plan(&transfer, &t);
        assert!(again.in_sync(Mode::Update));
        let mirrored = super::super::execute(
            &again,
            Mode::Mirror,
            Verify::Everything,
            &TaskCtx::detached(),
        );
        assert_eq!(mirrored.problems(), 0, "{mirrored:?}");
        assert_eq!((mirrored.removed, mirrored.verified), (1, 2));
        assert!(!server_home.join("photos/extra.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_download_is_verified_against_the_folder_each_source_is_in() {
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-download");
        t.file("server/shoot/day1/a.jpg", b"aa");
        t.file("server/notes.txt", b"n");
        let local = t.dir("local");
        let transfer = Transfer {
            host: server(&t, false),
            direction: Direction::Download,
            sources: vec![t.join("server/shoot"), t.join("server/notes.txt")],
            dest: local.clone(),
        };
        let plan = remote_plan(&transfer, &t);
        assert_eq!(plan.new.count, 2);
        let before = crate::ops::copy::syncs();
        let report =
            super::super::execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
        let after = crate::ops::copy::syncs();
        // Both files flushed here, and the three folders that gained names:
        // `local`, `local/shoot` and `local/shoot/day1`.
        assert_eq!(after.0 - before.0, 2);
        assert_eq!(after.1 - before.1, 3);
        assert_eq!(report.problems(), 0, "{report:?}");
        assert_eq!(report.verified, 2);
        assert_eq!(
            std::fs::read(local.join("shoot/day1/a.jpg")).unwrap(),
            b"aa"
        );
        assert_eq!(std::fs::read(local.join("notes.txt")).unwrap(), b"n");
    }

    #[cfg(unix)]
    #[test]
    fn a_server_without_sha256sum_is_verified_locally_only() {
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-no-sha");
        let photos = t.dir("src/photos");
        t.file("src/photos/a.jpg", b"aaaa");
        let transfer = Transfer {
            host: server(&t, true),
            direction: Direction::Upload,
            sources: vec![photos],
            dest: t.dir("server"),
        };
        let plan = remote_plan(&transfer, &t);
        let report =
            super::super::execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
        assert_eq!((report.copied, report.verified), (1, 0));
        assert!(report.local_only, "{report:?}");
        assert!(
            report.verify_failures.is_empty(),
            "no pretending either way"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_server_that_cannot_be_reached_is_an_error_in_its_own_words() {
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-unreachable");
        let photos = t.dir("src/photos");
        let script = t.file("bin/no-ssh", b"#!/bin/sh\necho 'ssh: connect to host fake port 22: Connection refused' >&2\nexit 255\n");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let transfer = Transfer {
            host: Host {
                program: Some(script),
                ..Host::alias("fake")
            },
            direction: Direction::Upload,
            sources: vec![photos.clone()],
            dest: PathBuf::from("backups"),
        };
        let err = plan(
            transfer,
            vec![Root {
                src: photos,
                dst: PathBuf::from("sftp://fake/backups/photos"),
            }],
            PathBuf::from("sftp://fake/backups"),
            SyncOptions::default(),
            &|| false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Connection refused"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_stopped_dry_run_is_killed_and_cancelled() {
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-stop");
        let photos = t.dir("src/photos");
        let transfer = Transfer {
            host: server(&t, false),
            direction: Direction::Upload,
            sources: vec![photos.clone()],
            dest: t.dir("server"),
        };
        let result = plan(
            transfer,
            vec![Root {
                src: photos,
                dst: PathBuf::from("sftp://fake/server/photos"),
            }],
            PathBuf::from("sftp://fake/server"),
            SyncOptions::default(),
            &|| true,
        );
        assert!(matches!(result, Err(DfError::Cancelled)), "{result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_runs_progress_fills_the_bar_and_a_cancel_stops_it() {
        use crate::tasks::{ProgressSink, TaskFlags};
        use std::sync::Mutex;
        #[derive(Default)]
        struct Record {
            total: Mutex<(u64, u64)>,
            done: Mutex<(u64, u64)>,
        }
        impl ProgressSink for Record {
            fn set_total(&self, bytes: u64, files: u64) {
                *self.total.lock().unwrap() = (bytes, files);
            }
            fn advance(&self, bytes: u64, files: u64) {
                let mut done = self.done.lock().unwrap();
                done.0 += bytes;
                done.1 += files;
            }
        }
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-progress");
        let photos = t.dir("src/photos");
        t.file("src/photos/big.bin", &vec![7u8; 3 * 1024 * 1024]);
        t.file("src/photos/small.txt", b"small");
        let transfer = Transfer {
            host: server(&t, false),
            direction: Direction::Upload,
            sources: vec![photos],
            dest: t.dir("server"),
        };
        let plan = remote_plan(&transfer, &t);

        let flags = Arc::new(TaskFlags::new());
        flags.cancel();
        let ctx = TaskCtx::with_sink(flags, Arc::new(Record::default()));
        let cancelled = super::super::execute(&plan, Mode::Update, Verify::Copied, &ctx);
        assert!(cancelled.cancelled, "{cancelled:?}");

        let record = Arc::new(Record::default());
        let ctx = TaskCtx::with_sink(Arc::new(TaskFlags::new()), record.clone());
        let report = super::super::execute(&plan, Mode::Update, Verify::Copied, &ctx);
        assert_eq!(report.problems(), 0, "{report:?}");
        let total = *record.total.lock().unwrap();
        let bytes = 3 * 1024 * 1024 + 5;
        // Copied, then read back on this side; the far side is the server's.
        // Three paths to write (the new folder and its two files), two to read.
        assert_eq!(total, (bytes * 2, 3 + 2));
        assert_eq!(*record.done.lock().unwrap(), total);
    }

    #[test]
    fn the_version_is_read_off_the_first_line() {
        assert_eq!(
            parse_version("rsync  version 3.5.0-g471e17dc  protocol version 32\nCopyright"),
            Some((3, 5, 0))
        );
        assert_eq!(
            parse_version("rsync  version 3.1.3  protocol version 31"),
            Some((3, 1, 3))
        );
        assert_eq!(
            parse_version("rsync version 3.2.0pre1 protocol"),
            Some((3, 2, 0))
        );
        assert_eq!(
            parse_version("rsync  version 2.6  protocol"),
            Some((2, 6, 0))
        );
        assert_eq!(
            parse_version("rsync  version v3.3.0  protocol version 32"),
            Some((3, 3, 0))
        );
        assert_eq!(parse_version("openrsync: protocol version 29"), None);
        assert_eq!(parse_version(""), None);
    }

    /// The rsyncs a Mac comes with are refused, Homebrew's is run: Samba's
    /// 2.6.9 and openrsync (which says it is 2.6.9 on its second line, never
    /// its first) know nothing of `--info=progress2`.
    #[test]
    fn only_an_rsync_that_knows_progress2_is_run() {
        assert!(!new_enough(
            "rsync  version 2.6.9  protocol version 29\nCopyright"
        ));
        assert!(!new_enough(
            "openrsync: protocol version 29\nrsync version 2.6.9 compatible\n"
        ));
        assert!(!new_enough("rsync  version 3.0.9  protocol version 30"));
        assert!(new_enough("rsync  version 3.1.0  protocol version 31"));
        assert!(new_enough(
            "rsync  version 3.2.7  protocol version 31\nCopyright"
        ));
        assert!(new_enough(
            "rsync  version 3.5.0-g471e17dc  protocol version 32"
        ));
        assert!(!new_enough(""));
    }

    #[test]
    fn only_an_upload_from_a_new_enough_rsync_asks_the_server_to_flush() {
        assert!(fsync_for(Direction::Upload, Some((3, 2, 0))));
        assert!(fsync_for(Direction::Upload, Some((3, 5, 0))));
        assert!(!fsync_for(Direction::Upload, Some((3, 1, 3))));
        assert!(!fsync_for(Direction::Upload, None));
        assert!(
            !fsync_for(Direction::Download, Some((3, 5, 0))),
            "a download is flushed here"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_server_that_refuses_fsync_is_synced_without_it_and_says_so() {
        use std::os::unix::fs::PermissionsExt;
        if !rsync_here() || !fsync_for(Direction::Upload, local_version()) {
            return;
        }
        let t = TempTree::new("rsync-old-server");
        let photos = t.dir("src/photos");
        t.file("src/photos/a.jpg", b"aaaa");
        // An old server: its rsync does not know --fsync, and says so the way
        // rsync does, before anything moves.
        let script = t.file(
            "bin/old-ssh",
            b"#!/bin/sh\nshift\ncase \"$*\" in *--fsync*) echo 'rsync: on remote machine: --fsync: unknown option' >&2; exit 1;; esac\nexec sh -c \"$*\"\n",
        );
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let transfer = Transfer {
            host: Host {
                program: Some(script),
                ..Host::alias("old")
            },
            direction: Direction::Upload,
            sources: vec![photos],
            dest: t.dir("server"),
        };
        let plan = remote_plan(&transfer, &t);
        let report =
            super::super::execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
        assert_eq!(report.problems(), 0, "{report:?}");
        assert!(report.unflushed, "the server was never asked to flush");
        assert_eq!((report.copied, report.verified), (1, 1));
        assert_eq!(
            std::fs::read(t.join("server/photos/a.jpg")).unwrap(),
            b"aaaa"
        );

        // A server that knows the option is asked, and the result says so.
        let transfer = Transfer {
            host: server(&t, false),
            sources: vec![t.join("src/photos")],
            dest: t.dir("server2"),
            direction: Direction::Upload,
        };
        let plan = remote_plan(&transfer, &t);
        let report =
            super::super::execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
        assert_eq!(report.problems(), 0, "{report:?}");
        assert!(!report.unflushed);
    }

    /// What `rsync` really hands its remote shell for our `-e`: every option
    /// whole, a key with a space and a quote in its name included, the user
    /// as `-l`, then `--`, then the host.
    #[cfg(unix)]
    #[test]
    fn rsync_hands_ssh_exactly_the_options_given() {
        use std::os::unix::fs::PermissionsExt;
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-rsh");
        let src = t.file("src/a.txt", b"a");
        let show = t.file(
            "bin/show",
            b"#!/bin/sh\nfor a in \"$@\"; do printf '[%s]\\n' \"$a\" >&2; done\nexit 1\n",
        );
        std::fs::set_permissions(&show, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = Host {
            destination: "brian@showandtour1".to_string(),
            port: Some(2222),
            key: Some(PathBuf::from("/keys/brian's key")),
            program: None,
        };
        let out = Command::new("rsync")
            .arg("-e")
            .arg(host.rsh_with(&show.to_string_lossy()))
            .arg("-n")
            .arg(&src)
            .arg(format!("{}:x", host.host()))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let words: Vec<&str> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix('[')?.strip_suffix(']'))
            .take_while(|word| *word != "rsync")
            .collect();
        assert_eq!(
            words,
            [
                "-x",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=15",
                "-p",
                "2222",
                "-i",
                "/keys/brian's key",
                "-l",
                "brian",
                "--",
                "showandtour1",
            ],
            "{stderr}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_rsync_could_not_send_is_one_problem_not_two() {
        use std::os::unix::fs::PermissionsExt;
        if !rsync_here() {
            return;
        }
        let t = TempTree::new("rsync-partial");
        let photos = t.dir("src/photos");
        t.file("src/photos/a.jpg", b"aaaa");
        let locked = t.file("src/photos/locked.jpg", b"locked away");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let transfer = Transfer {
            host: server(&t, false),
            direction: Direction::Upload,
            sources: vec![photos],
            dest: t.dir("server"),
        };
        let plan = remote_plan(&transfer, &t);
        assert_eq!(plan.new.count, 2, "the dry run cannot tell");
        let report =
            super::super::execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!((report.copied, report.copied_bytes), (1, 4), "{report:?}");
        assert_eq!(report.verified, 1);
        assert!(report.verify_failures.is_empty(), "{report:?}");
        assert_eq!(report.errors.len(), 1, "{report:?}");
        assert_eq!(report.errors[0].0, locked);
    }

    #[test]
    fn what_landed_is_counted_from_the_run_itself() {
        let roots = vec![Root {
            src: PathBuf::from("/src/photos"),
            dst: PathBuf::from("sftp://h/dst/photos"),
        }];
        let plan = plan_from(
            parse_itemized(DRY_RUN.as_bytes()),
            roots,
            PathBuf::from("sftp://h/dst"),
            SyncOptions::default(),
            upload(),
        );
        // A run that sent one of the new files, made the new folder, and
        // deleted a folder with a file in it.
        let lines = parse_itemized(
            b"cd+++++++++ 40 photos/empty/\n>f+++++++++ 4 photos/2024/a.jpg\n*deleting   0 photos/old/gone.jpg\n*deleting   0 photos/old/\n",
        );
        let landed = landed(&plan, &lines);
        assert_eq!(
            (
                landed.copied,
                landed.copied_bytes,
                landed.made,
                landed.removed
            ),
            (1, 4, 1, 1)
        );
        assert_eq!(landed.items.len(), 2);
    }

    #[test]
    fn the_servers_folder_is_quoted_for_its_shell() {
        assert_eq!(
            remote_script(Path::new("it's here")),
            r"cd -- 'it'\''s here' && xargs -0 sha256sum --"
        );
    }
}
