//! Parsing `git status --porcelain=v2 -z`, exactly.
//!
//! ## Why v2, and why `-z`
//!
//! v1 porcelain is two status characters and a path, with renames written as
//! `old -> new` and paths *quoted* whenever they contain anything interesting.
//! That means a file called `a -> b` is ambiguous with a rename, and a file with
//! a newline in it is C-escaped and has to be unescaped to be used. v2 fixes
//! both: every field is positional, the rename source is a separate field, and
//! `-z` replaces the line terminator with a NUL — the one byte a POSIX filename
//! cannot contain. Between them, there is no filename that can be misread.
//!
//! So the framing is: records terminated by NUL, and a rename record is *two*
//! NUL-terminated fields (the new path, then the original), because that is what
//! git writes — the tab separator documented for the non-`-z` form becomes a NUL
//! and the record simply carries an extra field.
//!
//! ## `--ignored=matching`
//!
//! `traditional` collapses an ignored directory to one entry, which is what
//! `git status` prints for a human and exactly wrong for a file manager: entering
//! `target/` would then show a hundred rows git said nothing about, none of them
//! dimmed. `matching` lists every ignored path, so the dimming is per-row and
//! correct at any depth. It costs a bigger listing, which the [`MAX_STATUS_ENTRIES`]
//! cap bounds.
//!
//! ## Rollups
//!
//! A collapsed directory still has to say something: a `src/` whose contents are
//! modified shows a modified dot. So every non-ignored status is walked up to the
//! repository root, merging into a per-directory map by [`FileStatus::rank`] —
//! the worst thing inside wins, because a conflict two levels down is the thing
//! you need to see.
//!
//! Ignored is the one status that does *not* roll up. A directory holding
//! `.DS_Store` is not an ignored directory, and dimming it would hide a real
//! directory behind a rule about one file in it. Ignored directories are still
//! dimmed — git reports those as their own entry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::io::Read;

/// The most status entries kept from one repository.
///
/// 200k paths is a `chromium` checkout with its build directory ignored and
/// listed — well past any repository a person navigates by hand, and about
/// 20 MB of `PathBuf` once mapped, which is a cache and not a leak. Past it the
/// listing is truncated and [`StatusData::truncated`] says so, because a status
/// map that grows without bound is a file manager that gets killed by the OOM
/// reaper an hour into a session.
pub const MAX_STATUS_ENTRIES: usize = 200_000;

/// The most bytes read from `git status`'s stdout: 64 MiB.
///
/// The byte-side twin of [`MAX_STATUS_ENTRIES`], because the entry cap is only
/// checked once a record is parsed and a single record can be a 4 KiB path.
/// 64 MiB is roughly 200k long paths, so the two caps bite at about the same
/// place, and neither one can be walked past.
pub const MAX_STATUS_BYTES: u64 = 64 * 1024 * 1024;

/// What one path's git state is, reduced to the one thing a row can show.
///
/// git's real answer is two characters — staged and unstaged, independently. A
/// row has one dot, so this is the reduction, and the reduction favours the
/// working tree: a file that is staged-modified *and* modified again shows as
/// modified, because the un-staged change is the newer news.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileStatus {
    /// Matched a `.gitignore`. Dimmed, never rolled up.
    Ignored,
    /// git has never heard of it.
    Untracked,
    /// New in the index (`A`), or copied from another path (`C`).
    Added,
    /// Gone from the work tree or the index.
    Deleted,
    /// Moved. Copies land here too — both are v2's "entry with an original
    /// path", and both read to a person as "this came from somewhere else".
    Renamed,
    /// File became a symlink, or vice versa. Rare and worth its own dot,
    /// because it is almost always a mistake.
    Typechange,
    /// Contents differ.
    Modified,
    /// An unmerged path. The loudest thing a row can say.
    Conflict,
}

impl FileStatus {
    /// Which status wins when a directory contains several.
    ///
    /// Ordered by how much the user needs to know: a conflict outranks
    /// everything, a modification outranks a new file (the new file is expected,
    /// the modification may not be), and ignored is bottom because it never
    /// competes — it does not roll up at all.
    pub fn rank(self) -> u8 {
        match self {
            FileStatus::Ignored => 0,
            FileStatus::Untracked => 1,
            FileStatus::Added => 2,
            FileStatus::Deleted => 3,
            FileStatus::Renamed => 4,
            FileStatus::Typechange => 5,
            FileStatus::Modified => 6,
            FileStatus::Conflict => 7,
        }
    }

    /// Whether a directory inherits this from its contents. See the module
    /// essay: everything but [`FileStatus::Ignored`] does.
    pub fn rolls_up(self) -> bool {
        !matches!(self, FileStatus::Ignored)
    }

    /// The single character for a dot-plus-letter badge, matching what git's own
    /// porcelain prints so that a user who knows `git status` needs no key.
    pub fn letter(self) -> char {
        match self {
            FileStatus::Ignored => '!',
            FileStatus::Untracked => '?',
            FileStatus::Added => 'A',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Typechange => 'T',
            FileStatus::Modified => 'M',
            FileStatus::Conflict => 'U',
        }
    }

    /// The worse of two statuses, for the rollup merge.
    pub fn worse(self, other: FileStatus) -> FileStatus {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// The three numbers the breadcrumb shows beside the branch.
///
/// Counted from the *records*, not from the reduced [`FileStatus`], because
/// staged and unstaged are independent: one file that is both adds one to each.
/// That is what `git status -sb` reports and what the user expects to see.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirtyCounts {
    /// Paths with an index change (v2's `X` field is not `.`).
    pub staged: usize,
    /// Paths with a work-tree change (v2's `Y` field is not `.`).
    pub unstaged: usize,
    /// Paths git has never heard of.
    pub untracked: usize,
    /// Unmerged paths.
    pub conflicted: usize,
}

impl DirtyCounts {
    /// Whether there is anything at all to report. Ignored files do not count —
    /// a repository whose only "change" is a build directory is clean.
    pub fn is_clean(&self) -> bool {
        self.staged == 0 && self.unstaged == 0 && self.untracked == 0 && self.conflicted == 0
    }

    pub fn total(&self) -> usize {
        self.staged + self.unstaged + self.untracked + self.conflicted
    }
}

/// One repository's status, keyed by absolute path — by [`crate::path::key`]
/// of it, so a row spelled in another case than git's output (Windows) finds
/// its dot. Look paths up through [`StatusData::status_for`], which keys them.
///
/// Two maps rather than one, because a directory can carry *both*: `target/` may
/// be ignored in its own right while also being the rollup of something. The
/// lookup order in [`StatusData::status_for`] resolves that — an exact file
/// answer wins, then the directory answer.
#[derive(Debug, Clone, Default)]
pub struct StatusData {
    /// Non-directory paths git reported, absolute.
    pub files: HashMap<PathBuf, FileStatus>,
    /// Directories: the ones git reported as directories, plus every rollup.
    pub dirs: HashMap<PathBuf, FileStatus>,
    /// The directories git reported *collapsed* — an entry with a trailing
    /// slash, meaning "and everything inside". Under the default `-unormal` an
    /// untracked directory is reported this way, and so is an ignored directory
    /// whose own name matched a pattern, so the paths inside get no entry of
    /// their own and have to inherit. Kept apart from `dirs` because inheritance
    /// runs *down* and rollups run *up*: a modified `src/` must not make every
    /// file in it show modified.
    pub collapsed: HashMap<PathBuf, FileStatus>,
    pub counts: DirtyCounts,
    /// From the `# branch.head` header, when the command asked for it. Nothing
    /// depends on it — [`super::repo::branch`] answers faster and without a
    /// subprocess — but it is free here and it is the authoritative spelling.
    pub branch: Option<String>,
    /// From `# branch.ab`, as `(ahead, behind)` — both positive counts. git
    /// writes them as `+3 -1`; the sign is a label for which side, not a
    /// negative number, and storing it as one would make `behind > 0` read
    /// backwards at every call site. `None` when there is no upstream.
    pub ahead_behind: Option<(i64, i64)>,
    /// Whether [`MAX_STATUS_ENTRIES`] or [`MAX_STATUS_BYTES`] cut the listing
    /// short. A `true` here means "some rows have no dot", not "no rows do".
    pub truncated: bool,
}

impl StatusData {
    /// What to draw for one row.
    ///
    /// Its own entry wins — file, then directory. Failing that, the nearest
    /// collapsed ancestor: a file inside an untracked directory is untracked,
    /// and a file inside an ignored directory is dimmed, even though git printed
    /// one entry for the directory and none for the file. The walk is a handful
    /// of hash lookups and it is what makes `-unormal` affordable; the
    /// alternative, `-uall`, is a listing of every file in `node_modules`.
    pub fn status_for(&self, path: &Path) -> Option<FileStatus> {
        let path = crate::path::key(path);
        if let Some(s) = self.files.get(path.as_ref()) {
            return Some(*s);
        }
        if let Some(s) = self.dirs.get(path.as_ref()) {
            return Some(*s);
        }
        if self.collapsed.is_empty() {
            return None;
        }
        for ancestor in path.ancestors().skip(1) {
            if let Some(s) = self.collapsed.get(ancestor) {
                return Some(*s);
            }
        }
        None
    }

    /// Whether this path is dimmed, directly or by inheritance.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.status_for(path) == Some(FileStatus::Ignored)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.dirs.is_empty()
    }
}

/// Why a status could not be taken.
///
/// [`StatusError::NoGit`] is not really an error — it is the "feature silently
/// absent" case, and [`super::cache::Git`] latches it and stops asking.
#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("git is not installed")]
    NoGit,
    #[error("git status failed: {0}")]
    Failed(String),
    #[error("git status: {0}")]
    Io(#[from] std::io::Error),
}

/// Run `git status` in `root` and parse it. Blocks; only [`super::cache::Git`]'s
/// worker thread and tests should call it.
///
/// The flags, each load-bearing:
///
/// - `--porcelain=v2` — the positional, stable-by-contract format.
/// - `-z` — NUL framing, so no filename needs unescaping.
/// - `--ignored=matching` — one entry per ignored path, so dimming works at any
///   depth.
/// - `--branch` — the `# branch.*` headers, which cost nothing and carry
///   ahead/behind for the breadcrumb.
/// - `--no-optional-locks` — a file manager polling status must never take the
///   index lock out from under a `git commit` the user is running in a terminal.
///   This is the flag that makes background polling safe, and it is the one that
///   would be easiest to forget.
pub fn status_blocking(root: &Path) -> Result<StatusData, StatusError> {
    let mut child = crate::platform::process::quiet(&mut Command::new("git"))
        // The repository is untrusted content. `.git/config` is read before
        // anything else git does, and several of its keys are *command lines*:
        // a directory that arrived in an archive, on a stick, or out of a
        // download can carry `core.fsmonitor = sh -c ...` and git runs it on a
        // plain `status`. In this program that means walking the cursor into a
        // directory is enough — no click, no opener. `safe.directory` does not
        // help, because a file the user just extracted is owned by the user.
        // `-c` is applied after the repository's own config, so these win.
        .arg("-c")
        .arg("core.fsmonitor=")
        .arg("-c")
        .arg(format!(
            "core.hooksPath={}",
            crate::platform::process::NULL_DEVICE
        ))
        .arg("-c")
        .arg("core.pager=cat")
        .arg("-c")
        .arg("core.sshCommand=")
        .arg("--no-optional-locks")
        .arg("status")
        .arg("--porcelain=v2")
        .arg("-z")
        .arg("--ignored=matching")
        .arg("--branch")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StatusError::NoGit
            } else {
                StatusError::Io(e)
            }
        })?;

    let mut out = Vec::new();
    let mut hit_cap = false;
    if let Some(stdout) = child.stdout.take() {
        // `take` rather than `read_to_end`: a repository with a runaway ignore
        // listing must cost a bounded amount of memory, not whatever git felt
        // like producing.
        stdout.take(MAX_STATUS_BYTES).read_to_end(&mut out)?;
        hit_cap = out.len() as u64 >= MAX_STATUS_BYTES;
    }
    let mut err = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        // Capped for the same reason, and much smaller: this only ever becomes
        // a one-line log message.
        stderr.take(8192).read_to_end(&mut err)?;
    }
    let done = child.wait()?;

    if !done.success() && out.is_empty() {
        let message = String::from_utf8_lossy(&err).trim().to_string();
        return Err(StatusError::Failed(if message.is_empty() {
            format!("exit {done}")
        } else {
            message
        }));
    }

    let mut data = parse_porcelain_v2(&out, root);
    data.truncated |= hit_cap;
    Ok(data)
}

/// Parse porcelain v2 `-z` bytes into a status map rooted at `root`.
///
/// Pure, total, and never panicking: a malformed record is skipped, not fatal.
/// git's output is well-formed by construction, but this function is also handed
/// truncated output whenever a cap bites, and half a record must not lose the
/// half that parsed.
///
/// The record grammar, all NUL-terminated:
///
/// ```text
/// # <header> <value>
/// 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
/// 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path> NUL <origPath>
/// u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
/// ? <path>
/// ! <path>
/// ```
pub fn parse_porcelain_v2(bytes: &[u8], root: &Path) -> StatusData {
    let mut data = StatusData::default();
    let mut records = Records::new(bytes);

    while let Some(record) = records.next_record() {
        if record.is_empty() {
            continue;
        }
        if data.files.len() + data.dirs.len() >= MAX_STATUS_ENTRIES {
            data.truncated = true;
            break;
        }
        match record[0] {
            b'#' => header(&mut data, record),
            b'1' => {
                if let Some((fields, path)) = split_fields(record, 8) {
                    changed(&mut data, root, fields[1], path);
                }
            }
            b'2' => {
                // The rename source is its own NUL-terminated field. It is read
                // whether or not the record parsed, because the framing has to
                // stay in step: skipping it would make the *next* record start
                // mid-path.
                let source = records.next_record();
                if let Some((fields, path)) = split_fields(record, 9) {
                    changed(&mut data, root, fields[1], path);
                    // The original path is where the file *was*. It is gone from
                    // the work tree, so its row — if the user is looking at the
                    // old directory — is a deletion. Recording it keeps a rename
                    // across directories showing something on both sides.
                    if let Some(source) = source {
                        if !source.is_empty() {
                            insert(&mut data, root, source, FileStatus::Deleted, false);
                        }
                    }
                }
            }
            b'u' => {
                if let Some((_, path)) = split_fields(record, 10) {
                    data.counts.conflicted += 1;
                    insert(&mut data, root, path, FileStatus::Conflict, true);
                }
            }
            b'?' => {
                if let Some((_, path)) = split_fields(record, 1) {
                    data.counts.untracked += 1;
                    insert(&mut data, root, path, FileStatus::Untracked, true);
                }
            }
            b'!' => {
                if let Some((_, path)) = split_fields(record, 1) {
                    // Deliberately no rollup: see the module essay.
                    insert(&mut data, root, path, FileStatus::Ignored, false);
                }
            }
            _ => {}
        }
    }

    data
}

/// A cursor over NUL-terminated records.
///
/// A type rather than a `split(0)` because entry type 2 needs to pull a second
/// record out of the middle of the loop, and an iterator that has already been
/// `collect`ed into a `Vec` would work too but would copy every path.
struct Records<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Records<'a> {
    fn new(bytes: &'a [u8]) -> Records<'a> {
        Records { bytes, at: 0 }
    }

    fn next_record(&mut self) -> Option<&'a [u8]> {
        if self.at >= self.bytes.len() {
            return None;
        }
        let rest = &self.bytes[self.at..];
        match rest.iter().position(|b| *b == 0) {
            Some(end) => {
                self.at += end + 1;
                Some(&rest[..end])
            }
            None => {
                // No terminator: the output was cut mid-record by a cap. Hand
                // back what there is — the leading fields are still valid, and
                // the path is the only part that might be short.
                self.at = self.bytes.len();
                Some(rest)
            }
        }
    }
}

/// Split `record` into its first `n` space-separated fields plus the rest.
///
/// The rest is the path, taken verbatim: paths may contain spaces, and in v2
/// they are never quoted, so the *only* correct way to find one is to count
/// separators from the left and stop.
fn split_fields(record: &[u8], n: usize) -> Option<(Vec<&[u8]>, &[u8])> {
    let mut fields = Vec::with_capacity(n);
    let mut rest = record;
    for _ in 0..n {
        let space = rest.iter().position(|b| *b == b' ')?;
        fields.push(&rest[..space]);
        rest = &rest[space + 1..];
    }
    if rest.is_empty() {
        return None;
    }
    Some((fields, rest))
}

fn header(data: &mut StatusData, record: &[u8]) {
    let Ok(text) = std::str::from_utf8(record) else {
        return;
    };
    let text = text.trim_start_matches('#').trim();
    if let Some(name) = text.strip_prefix("branch.head ") {
        let name = name.trim();
        // git spells a detached HEAD `(detached)` here. That is a sentinel, not
        // a branch name, so it is dropped and the breadcrumb falls back to
        // `repo::head`, which has the short hash.
        if !name.is_empty() && name != "(detached)" {
            data.branch = Some(name.to_string());
        }
    } else if let Some(ab) = text.strip_prefix("branch.ab ") {
        let mut ahead = None;
        let mut behind = None;
        for part in ab.split_whitespace() {
            if let Some(n) = part.strip_prefix('+') {
                ahead = n.parse::<i64>().ok();
            } else if let Some(n) = part.strip_prefix('-') {
                behind = n.parse::<i64>().ok();
            }
        }
        if let (Some(a), Some(b)) = (ahead, behind) {
            data.ahead_behind = Some((a, b));
        }
    }
}

/// A type 1 or type 2 record: count it, reduce its `XY`, record it.
fn changed(data: &mut StatusData, root: &Path, xy: &[u8], path: &[u8]) {
    if xy.len() < 2 {
        return;
    }
    let (x, y) = (xy[0], xy[1]);
    if x != b'.' {
        data.counts.staged += 1;
    }
    if y != b'.' {
        data.counts.unstaged += 1;
    }
    if x == b'U' || y == b'U' {
        data.counts.conflicted += 1;
    }
    insert(data, root, path, reduce(x, y), true);
}

/// Two status characters to one dot.
///
/// The work tree wins when both sides changed, because the work tree is what the
/// user is looking at. `U` on either side is a conflict regardless — v2 reports
/// most unmerged paths as type `u`, but `.U`/`U.` combinations exist and mean the
/// same thing.
fn reduce(x: u8, y: u8) -> FileStatus {
    if x == b'U' || y == b'U' {
        return FileStatus::Conflict;
    }
    let code = if y != b'.' { y } else { x };
    match code {
        b'A' => FileStatus::Added,
        b'D' => FileStatus::Deleted,
        b'R' | b'C' => FileStatus::Renamed,
        b'T' => FileStatus::Typechange,
        // `M` and anything unrecognized: a record git bothered to print is a
        // change, and "modified" is the honest default for a change with no
        // better name.
        _ => FileStatus::Modified,
    }
}

/// Record one path and, if it rolls up, its ancestors.
///
/// A trailing `/` is git's way of naming a directory — untracked directories are
/// reported collapsed under the default `-unormal`, and ignored directories can
/// be too. Those go in `dirs`; everything else goes in `files`.
fn insert(data: &mut StatusData, root: &Path, raw: &[u8], status: FileStatus, roll_up: bool) {
    let is_dir = raw.last() == Some(&b'/');
    let trimmed = if is_dir { &raw[..raw.len() - 1] } else { raw };
    if trimmed.is_empty() {
        return;
    }
    // Bytes, not `str`: a path is bytes on Unix, and a filename that is not
    // UTF-8 still gets a dot. On Windows one that is not UTF-8 names no file
    // there, and is left out.
    let Ok(rel) = crate::platform::os::from_bytes(trimmed) else {
        return;
    };
    let abs = crate::path::into_key(root.join(rel));
    let root = crate::path::key(root);
    let root = root.as_ref();

    if is_dir {
        merge(&mut data.dirs, abs.clone(), status);
        merge(&mut data.collapsed, abs.clone(), status);
    } else {
        merge(&mut data.files, abs.clone(), status);
    }

    if !roll_up || !status.rolls_up() {
        return;
    }
    for ancestor in abs.ancestors().skip(1) {
        // A path that somehow is not under the root (git will not do this, but a
        // hand-written fixture can) must not walk to `/`.
        if !ancestor.starts_with(root) {
            break;
        }
        merge(&mut data.dirs, ancestor.to_path_buf(), status);
        if ancestor == root {
            break;
        }
    }
}

fn merge(map: &mut HashMap<PathBuf, FileStatus>, path: PathBuf, status: FileStatus) {
    map.entry(path)
        .and_modify(|s| *s = s.worse(status))
        .or_insert(status);
}
