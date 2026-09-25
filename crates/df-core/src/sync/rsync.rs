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
//! `rsync --delete` deletes for good, wherever the extras are — there is no
//! trash on the far side of it — so a remote plan always says
//! [`Removal::Delete`], and the card says so on its button.
//!
//! The connection is the vfs's: the same `ssh` options
//! ([`crate::vfs::Service::command`]) — never prompt, give up connecting after
//! [`crate::vfs::CONNECT_TIMEOUT`] — so a host that works in the remote pane
//! works here, and one that wants a password fails at once with ssh's own
//! sentence instead of waiting on a prompt nobody can see.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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

/// Whether `rsync` is on `PATH`, asked the way the archive readers ask about
/// their tools: by running it, so the answer cannot disagree with the run.
pub fn available() -> bool {
    Command::new("rsync")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// How to reach the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// `user@host`, or an alias `~/.ssh/config` knows — what `ssh` is handed.
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

    /// The command that runs `script` on the server.
    fn shell(&self, script: String) -> Command {
        let mut command = match &self.program {
            Some(program) => Command::new(program),
            None => {
                let mut ssh = Command::new("ssh");
                ssh.args(self.ssh_options());
                ssh
            }
        };
        command.arg(&self.destination).arg(script);
        command
    }

    /// The options every `ssh` this module starts gets, before the
    /// destination.
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
        options
    }

    /// The same options as the one string `rsync -e` takes. The key is single
    /// quoted, which `rsync` honours inside `-e`, so a key under a folder with
    /// a space in its name still reaches `ssh` whole.
    pub fn rsh(&self) -> String {
        if let Some(program) = &self.program {
            return format!("'{}'", program.display());
        }
        let mut rsh = "ssh".to_string();
        for option in self.ssh_options() {
            let option = option.to_string_lossy().into_owned();
            rsh.push(' ');
            if option.contains(' ') {
                rsh.push_str(&format!("'{option}'"));
            } else {
                rsh.push_str(&option);
            }
        }
        rsh
    }
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
    fn endpoint(&self, path: &Path, remote: bool, folder: bool) -> OsString {
        let mut bytes = path.as_os_str().as_bytes().to_vec();
        while bytes.len() > 1 && bytes.last() == Some(&b'/') {
            bytes.pop();
        }
        if folder && !bytes.is_empty() && bytes.last() != Some(&b'/') {
            bytes.push(b'/');
        }
        let mut out = OsString::new();
        if remote {
            out.push(format!("{}:", self.host.destination));
        }
        out.push(OsStr::from_bytes(&bytes));
        out
    }

    /// The paths at the end of every command line: `--`, then the sources,
    /// then the destination. The `--` keeps a server path that starts with a
    /// dash from being read as an option.
    fn endpoints(&self) -> Vec<OsString> {
        let mut out = vec![OsString::from("--")];
        for source in &self.sources {
            out.push(self.endpoint(source, self.remote_sources(), false));
        }
        out.push(self.endpoint(&self.dest, !self.remote_sources(), true));
        out
    }

    fn common(&self) -> Vec<OsString> {
        vec!["-e".into(), self.host.rsh().into()]
    }

    /// The dry run the card's plan comes from: every path itemized, the
    /// unchanged ones included (`-ii`) so the card can count them, and
    /// `--delete` always, so the extras are known before `m` asks for them.
    /// Each line is `%i %l %n`: the change, the length, the name.
    pub fn dry_run_args(&self, content: bool) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["-a", "-n", "-ii", "--delete", "--out-format=%i %l %n"]
            .iter()
            .map(OsString::from)
            .collect();
        if content {
            args.push("--checksum".into());
        }
        args.extend(self.common());
        args.extend(self.endpoints());
        args
    }

    /// The run itself. `--no-inc-recursive` makes `rsync` count the whole
    /// transfer before it starts, so the progress line is about all of it
    /// rather than about what has been found so far.
    pub fn run_args(&self, mode: Mode, content: bool) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["-a", "--info=progress2", "--no-inc-recursive"]
            .iter()
            .map(OsString::from)
            .collect();
        if mode == Mode::Mirror {
            args.push("--delete".into());
        }
        if content {
            args.push("--checksum".into());
        }
        args.extend(self.common());
        args.extend(self.endpoints());
        args
    }
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
    let name = PathBuf::from(OsString::from_vec(name));
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
    command.args(transfer.dry_run_args(options.content));
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
            signal(child, libc::SIGCONT);
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        tick(child);
        std::thread::sleep(POLL);
    }
}

/// Send `child` a signal: `SIGSTOP` and `SIGCONT` are how a pause reaches a
/// process that is not ours to checkpoint.
fn signal(child: &Child, signal: libc::c_int) {
    let Ok(pid) = libc::pid_t::try_from(child.id()) else {
        return;
    };
    // One syscall on a pid this process spawned and has not yet reaped.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::kill(pid, signal) };
    if rc != 0 {
        log::debug!(
            "could not signal rsync ({})",
            std::io::Error::last_os_error()
        );
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
        ..SyncReport::default()
    };
    let to_copy = plan.bytes_to_copy();
    let checks = files_to_verify(plan, verify);
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

    let ran = match run(transfer, mode, plan.options.content, ctx) {
        Ok(ran) => ran,
        Err(e) => {
            report.errors.push((plan.dest_dir.clone(), e.to_string()));
            return report;
        }
    };
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
    // What the plan said would be written, was — all of it, or all but what
    // `rsync` names on stderr, which is listed.
    for item in plan
        .items
        .iter()
        .filter(|item| matches!(item.class, Class::New | Class::Changed))
    {
        if item.kind == Kind::Dir {
            report.made += 1;
        } else {
            report.copied += 1;
        }
    }
    report.copied_bytes = to_copy;
    ctx.advance(to_copy.saturating_sub(ran.moved), copies);
    if mode == Mode::Mirror {
        report.removed = plan.extra.count;
    }
    report.errors.extend(complaints(&ran.stderr));
    if partial && report.errors.is_empty() {
        report
            .errors
            .push((plan.dest_dir.clone(), failure_line(&ran.stderr, status)));
    }
    verify_remote(plan, transfer, &checks, ctx, &mut report);
    report
}

/// What the run came to, and how many bytes the progress line had counted.
struct Ran {
    status: Option<ExitStatus>,
    stderr: String,
    moved: u64,
}

/// Run `rsync`, turning its progress line into the task's bar: stopped while
/// the task is paused, killed when it is cancelled.
fn run(transfer: &Transfer, mode: Mode, content: bool, ctx: &TaskCtx) -> Result<Ran> {
    let mut command = Command::new("rsync");
    command
        .args(transfer.run_args(mode, content))
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
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for &byte in &buf[..n] {
                            if byte == b'\r' || byte == b'\n' {
                                if let Some(bytes) = progress_bytes(&String::from_utf8_lossy(&line))
                                {
                                    counted.fetch_max(bytes, Ordering::Relaxed);
                                }
                                line.clear();
                            } else {
                                line.push(byte);
                            }
                        }
                    }
                }
            }
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
            signal(
                child,
                if stopped {
                    libc::SIGSTOP
                } else {
                    libc::SIGCONT
                },
            );
        }
    })?;
    if let Some(handle) = stdout {
        let _ = handle.join();
    }
    let last = counted.load(Ordering::Relaxed);
    if last > reported {
        ctx.advance(last - reported, 0);
        reported = last;
    }
    Ok(Ran {
        status,
        stderr: stderr.and_then(|h| h.join().ok()).unwrap_or_default(),
        moved: reported,
    })
}

// ── The verify ──────────────────────────────────────────────────────────────

/// The files a verify of `plan` reads, by index: what was copied, or every
/// file of the source. Regular files only — `sha256sum` follows links, and a
/// link's target text is not something it can be asked about.
fn files_to_verify(plan: &SyncPlan, verify: Verify) -> Vec<usize> {
    plan.items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.kind == Kind::File)
        .filter(|(_, item)| match verify {
            Verify::Copied => matches!(item.class, Class::New | Class::Changed),
            Verify::Everything => item.class != Class::Extra,
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
        input.extend_from_slice(name.as_os_str().as_bytes());
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
        out.push((PathBuf::from(OsString::from_vec(name)), digest));
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

    const RSH: &str =
        "ssh -x -o BatchMode=yes -o ConnectTimeout=15 -p 2222 -i '/home/brian/my keys/id_ed25519'";

    #[test]
    fn the_run_is_rsync_archive_with_the_whole_transfer_counted_up_front() {
        let t = upload();
        assert_eq!(
            strings(&t.run_args(Mode::Update, false)),
            [
                "-a",
                "--info=progress2",
                "--no-inc-recursive",
                "-e",
                RSH,
                "--",
                "/home/brian/Photos/2024",
                "/home/brian/notes.txt",
                "brian@showandtour1:backups/photos/",
            ]
        );
        let mirror = strings(&t.run_args(Mode::Mirror, true));
        assert_eq!(mirror[3..5], ["--delete", "--checksum"]);
    }

    #[test]
    fn the_dry_run_itemizes_everything_and_always_looks_for_extras() {
        let t = upload();
        assert_eq!(
            strings(&t.dry_run_args(false))[..5],
            ["-a", "-n", "-ii", "--delete", "--out-format=%i %l %n"]
        );
        assert!(strings(&t.dry_run_args(true)).contains(&"--checksum".to_string()));
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
            strings(&t.endpoints()),
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
            strings(&home.endpoints()).last().unwrap(),
            "showandtour1:",
            "the login directory, not the server's root"
        );
        assert_eq!(
            Host::alias("showandtour1").rsh(),
            "ssh -x -o BatchMode=yes -o ConnectTimeout=15"
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
        assert_eq!(
            plan.listed(Mode::Mirror)
                .map(|item| plan.label(item))
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

    use crate::ops::fixture::TempTree;
    use crate::tasks::TaskCtx;

    /// A "server" that is this machine: `ssh`'s stand-in drops the destination
    /// and runs the rest. `fail_verify` makes it answer a `sha256sum` the way a
    /// server with none would.
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

    fn rsync_here() -> bool {
        let here = available();
        if !here {
            eprintln!("rsync is not installed; skipping");
        }
        here
    }

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
        let report =
            super::super::execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
        assert_eq!(report.problems(), 0, "{report:?}");
        assert_eq!(report.verified, 2);
        assert_eq!(
            std::fs::read(local.join("shoot/day1/a.jpg")).unwrap(),
            b"aa"
        );
        assert_eq!(std::fs::read(local.join("notes.txt")).unwrap(), b"n");
    }

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
    fn the_servers_folder_is_quoted_for_its_shell() {
        assert_eq!(
            remote_script(Path::new("it's here")),
            r"cd -- 'it'\''s here' && xargs -0 sha256sum --"
        );
    }
}
