//! Carrying out an [`ExtractPlan`] — the half [`mod@super::extract`] deliberately
//! left for later, written now (PLAN §7.3: "extract selection with progress").
//!
//! ## The one rule
//!
//! **An entry's name never becomes a path.** The plan already decided where
//! every entry lands, checking the name for traversal twice and the joined
//! destination once more (see [`mod@super::extract`]). What the walkers below do
//! with a name is *look it up*: `Sink::open` takes the bytes an archive spells
//! its member with, normalizes them the same way the listing did, and asks the
//! plan's map whether there is a destination for that key. A name that is not in
//! the map — because it was unsafe, or encrypted, or simply not selected — gets
//! `false` and its payload is skipped. There is no code path in this file that
//! joins an archive's bytes onto a directory, which is the property that makes
//! a malicious zip boring.
//!
//! ## Why the walkers live in the parsers
//!
//! `super::zip::extract_into` and `super::tar::extract_into` are next to the
//! listing code for their formats, because they are the same parse: a zip's
//! local file headers and a tar's 512-byte blocks. What is *here* is everything
//! that is not format-specific — the destination map, the collision suffixes,
//! the progress ticks, the partial file that a cancel has to remove — expressed
//! once, as a `Sink` the walkers can only push bytes at.
//!
//! ## Decompression
//!
//! - **Stored** zip members and every tar member are a copy: the payload in the
//!   file is the payload on disk.
//! - **Deflate** zip members go through `miniz_oxide`'s streaming inflate, a
//!   chunk at a time, so a 4 GiB member costs [`EXTRACT_BUF`] of memory and not
//!   4 GiB. The crate is already in the tree (df-app's PDF reader uses it) and
//!   rewriting an inflate is exactly the "impractical" the dependency rule is
//!   about.
//! - **Compressed tars** are streamed through the same `gzip`/`xz`/`zstd`
//!   process the listing uses, for the reason stated there: one spawn, no
//!   second thread, no pipe deadlock.
//! - Anything else — bzip2 or LZMA inside a zip, an encrypted member — is
//!   refused per entry rather than per archive, so a zip with one exotic member
//!   still extracts the other ninety-nine.
//!
//! ## The bomb guard
//!
//! Every entry is written under the length the *central directory* declared for
//! it. A member that keeps inflating past its own declared size is a lie about
//! its contents, and the honest response is to stop writing that entry and say
//! so — not to fill the disk on the strength of a number the archive supplied
//! and then contradicted.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::ops::exists;
use crate::ops::journal::{CopyManifest, OpRecord};
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::extract::{ExtractPlan, SkipReason};
use super::tree::normalize;
use super::ArchiveFormat;

/// How much of an entry is decompressed at a time.
///
/// 128 KiB: large enough that the inflate loop is not syscall-bound on a
/// directory of small files, small enough that the peak cost of extracting a
/// terabyte-sized member is still a rounding error. It is also the unit
/// progress advances in, so the `w` panel's bar moves smoothly on a big file
/// rather than jumping once per entry.
pub const EXTRACT_BUF: usize = 128 * 1024;

/// How often the cancel flag is read while streaming one entry: once per
/// [`EXTRACT_BUF`] chunk, which at any plausible disk speed is several times a
/// second. See [`crate::du::walk::CANCEL_CHECK_ENTRIES`] for the same trade in
/// the other direction — there the unit is entries, here it is bytes, because
/// one archive member can be the whole job.
pub const CANCEL_CHECK_BYTES: u64 = EXTRACT_BUF as u64;

/// What one extraction actually did.
///
/// Shaped like [`crate::ops::paste::PasteReport`]: per-entry failures are
/// collected rather than thrown, because a 900-file archive with one bad member
/// should still yield 899 files, and the journal still needs a record of them.
#[derive(Debug, Default)]
pub struct ExtractReport {
    /// Every destination that was created, in the order it was created.
    pub written: Vec<PathBuf>,
    pub files: usize,
    pub bytes: u64,
    /// `(inner path, reason)` for entries the plan refused before anything ran.
    pub skipped: Vec<(String, SkipReason)>,
    /// `(inner path, message)` for entries that were tried and failed.
    pub errors: Vec<(String, String)>,
    pub cancelled: bool,
    /// The inverse, when there is one. See [`plan_record`].
    pub record: Option<OpRecord>,
}

impl ExtractReport {
    /// The one line for the toast, and the count that has to be in it.
    ///
    /// The skip count is *named* rather than folded into a general "some
    /// entries failed": PLAN §7.3's contract is that an archive with an unsafe
    /// member produces fewer files than the listing showed **and says so**, and
    /// a summary that only said "extracted 40 files" would be the quiet
    /// wrongness the plan's essay refuses.
    pub fn message(&self) -> String {
        let files = plural(self.files, "file", "files");
        let mut out = if self.cancelled {
            format!("Cancelled — extracted {files}")
        } else {
            format!("Extracted {files}")
        };
        let unsafe_names = self
            .skipped
            .iter()
            .filter(|(_, r)| *r == SkipReason::UnsafeName)
            .count();
        let encrypted = self
            .skipped
            .iter()
            .filter(|(_, r)| *r == SkipReason::Encrypted)
            .count();
        if unsafe_names > 0 {
            out.push_str(&format!(
                ", skipped {} with unsafe paths",
                plural(unsafe_names, "entry", "entries")
            ));
        }
        if encrypted > 0 {
            out.push_str(&format!(
                ", skipped {}",
                plural(encrypted, "encrypted entry", "encrypted entries")
            ));
        }
        out
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Where every entry in a plan is going, once collisions have been resolved.
///
/// Split out from the extraction so the collision policy is testable without a
/// decompressor: given a plan and a directory, this is the whole answer to
/// "what will be on disk when this finishes".
///
/// ## The collision policy
///
/// - **Directories merge.** A `src/` that is already there is used, not
///   suffixed. Suffixing it would send half the archive to `src_1/` and leave
///   the tree the listing showed unrecognisable.
/// - **Files auto-suffix**, through [`crate::ops::paste::unique_name`] — the
///   same `name_1`, `name_2` ladder a paste uses, so a collision looks the same
///   however the file arrived. Nothing on disk is ever overwritten by an
///   extraction: an archive is untrusted content, and "replace" is a decision
///   for a person.
#[derive(Debug, Default)]
pub struct Destinations {
    /// Inner path → where it lands. Files only; directories are created from
    /// the plan directly.
    pub files: HashMap<String, PathBuf>,
    /// Directories to create, parents first.
    pub dirs: Vec<PathBuf>,
    /// Top-level paths under the destination directory that this extraction
    /// will *create*, as opposed to merge into. See [`plan_record`].
    pub fresh: Vec<PathBuf>,
    /// Whether anything landed in a directory that was already there.
    pub merged: bool,
}

/// Resolve a plan's destinations against what is on disk right now.
pub fn destinations(plan: &ExtractPlan) -> Result<Destinations> {
    let mut out = Destinations::default();
    let mut claimed: Vec<PathBuf> = Vec::new();
    let mut fresh_tops: Vec<PathBuf> = Vec::new();

    for item in &plan.items {
        // The first component under the destination directory: the thing a
        // person would drag to the trash to undo this by hand.
        if let Some(top) = top_level(&plan.dest_dir, &item.dest) {
            if exists(&top) {
                out.merged = true;
            } else if !fresh_tops.contains(&top) {
                fresh_tops.push(top);
            }
        }
        if item.is_dir {
            out.dirs.push(item.dest.clone());
            continue;
        }
        let Some(name) = item.dest.file_name() else {
            continue;
        };
        let Some(parent) = item.dest.parent() else {
            continue;
        };
        // A file inside a directory this same extraction is about to create
        // cannot collide with anything, but `unique_name` has to be asked
        // anyway — the directory may already exist with a file of that name in
        // it, which is exactly the case the ladder is for.
        let dest = crate::ops::paste::unique_name(parent, name, &claimed)?;
        claimed.push(dest.clone());
        out.files.insert(item.inner.clone(), dest);
    }
    out.fresh = fresh_tops;
    Ok(out)
}

/// The first path component of `dest` below `dest_dir`.
fn top_level(dest_dir: &Path, dest: &Path) -> Option<PathBuf> {
    let rest = dest.strip_prefix(dest_dir).ok()?;
    let first = rest.components().next()?;
    Some(dest_dir.join(first.as_os_str()))
}

/// The inverse of a finished extraction, or `None` when there is not an honest
/// one.
///
/// An extraction is undone the way a copy is: delete exactly what it created,
/// having verified every path first — so it reuses [`OpRecord::Copy`] and
/// [`CopyManifest`] rather than growing a variant that would be the same code
/// with a different name. The manifest is what makes it safe: `u` after an
/// extraction removes the paths this extraction made and nothing else.
///
/// **`None` when anything merged.** If a top-level destination was already
/// there — `Extract here` into a directory that already has a `src/` — then
/// what is under it is partly the archive's and partly the user's, and
/// `CopyManifest::of_tree` cannot tell them apart: it walks what is on disk
/// *now*. Recording it anyway would mean `u` deleting files the extraction
/// never wrote, which is the "press u, lose today's work" bug the manifest type
/// exists to prevent. So the honest answer is that this particular extraction is
/// not undoable, and the toast says so instead of offering an undo that lies.
pub fn plan_record(dests: &Destinations) -> Option<OpRecord> {
    if dests.merged || dests.fresh.is_empty() {
        return None;
    }
    let mut created = Vec::new();
    for top in &dests.fresh {
        // A top-level path that is not there is one the extraction failed to
        // make; there is nothing to take back and nothing to record.
        if !exists(top) {
            continue;
        }
        // Too big to describe honestly, per `MAX_MANIFEST_ENTRIES`: the whole
        // extraction becomes not-undoable rather than half of it.
        created.push(CopyManifest::of_tree(top).ok()?);
    }
    (!created.is_empty()).then_some(OpRecord::Copy { created })
}

/// Where a walker puts what it finds.
///
/// The walkers hold one of these and can do exactly three things with it: ask
/// whether a name is wanted, push bytes, and finish the entry. They cannot
/// build a path, which is the point.
pub(crate) struct Sink<'a> {
    ctx: &'a TaskCtx,
    dests: &'a HashMap<String, PathBuf>,
    /// The file being written right now, and how much of it is left before the
    /// declared length is exceeded.
    open: Option<Open>,
    report: ExtractReport,
    since_check: u64,
}

struct Open {
    inner: String,
    path: PathBuf,
    file: File,
    /// What the central directory said this entry weighs. Writing past it is
    /// refused — see the module essay's bomb guard.
    remaining: u64,
    wrote: u64,
}

impl<'a> Sink<'a> {
    pub(crate) fn new(ctx: &'a TaskCtx, dests: &'a HashMap<String, PathBuf>) -> Sink<'a> {
        Sink {
            ctx,
            dests,
            open: None,
            report: ExtractReport::default(),
            since_check: 0,
        }
    }

    /// Whether the entry a walker just read a header for is wanted, opening its
    /// destination if so.
    ///
    /// `name` is the archive's own spelling; it is normalized here and used only
    /// as a key. `declared` is what the archive says the entry weighs.
    ///
    /// Returns `false` for "skip the payload", which is also what a failed
    /// `create` returns — a file that cannot be opened is one entry's problem,
    /// recorded in the report, and the walk carries on.
    pub(crate) fn open(&mut self, name: &str, declared: u64) -> bool {
        let Some(key) = normalize(name) else {
            return false;
        };
        let Some(dest) = self.dests.get(&key) else {
            return false;
        };
        // The parent is normally already there — the plan lists directories
        // first — but a zip that names `a/b.txt` without an `a/` entry of its
        // own has no directory record to have created.
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                self.report
                    .errors
                    .push((key, DfError::io(parent, e).to_string()));
                return false;
            }
        }
        // `create_new`, not `create`: nothing on disk is overwritten by an
        // extraction. `destinations` has already suffixed past anything that
        // existed when the plan was resolved, so a failure here means something
        // appeared in the last few milliseconds — a race, and the safe end of
        // it is to refuse.
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
        {
            Ok(f) => f,
            Err(e) => {
                self.report
                    .errors
                    .push((key, DfError::io(dest, e).to_string()));
                return false;
            }
        };
        self.open = Some(Open {
            inner: key,
            path: dest.clone(),
            file,
            remaining: declared,
            wrote: 0,
        });
        true
    }

    /// Push decompressed bytes at the entry that is open.
    ///
    /// `Ok(false)` means "stop reading this entry" — it overran its declared
    /// length, or the write failed. `Err(Cancelled)` means stop the whole walk.
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<bool> {
        self.since_check = self.since_check.saturating_add(bytes.len() as u64);
        if self.since_check >= CANCEL_CHECK_BYTES {
            self.since_check = 0;
            self.ctx.checkpoint()?;
        }
        let Some(open) = &mut self.open else {
            return Ok(false);
        };
        if bytes.len() as u64 > open.remaining {
            let inner = open.inner.clone();
            self.fail(
                inner,
                "entry is larger than the archive said it was".to_string(),
            );
            return Ok(false);
        }
        if let Err(e) = open.file.write_all(bytes) {
            let (inner, path) = (open.inner.clone(), open.path.clone());
            self.fail(inner, DfError::io(path, e).to_string());
            return Ok(false);
        }
        open.remaining -= bytes.len() as u64;
        open.wrote += bytes.len() as u64;
        self.report.bytes += bytes.len() as u64;
        self.ctx.advance(bytes.len() as u64, 0);
        Ok(true)
    }

    /// Finish the entry that is open, if one is.
    pub(crate) fn close(&mut self) {
        let Some(open) = self.open.take() else { return };
        // A short entry is not a failure worth losing the bytes over — a tar
        // truncated mid-file still yields what it had — but it is worth
        // recording, because the file on disk is not the file in the listing.
        if open.remaining > 0 {
            self.report.errors.push((
                open.inner.clone(),
                format!("ended {} bytes early", open.remaining),
            ));
        }
        drop(open.file);
        self.report.written.push(open.path);
        self.report.files += 1;
        self.ctx.advance(0, 1);
    }

    /// Record an entry's failure and remove the partial file it left.
    fn fail(&mut self, inner: String, message: String) {
        if let Some(open) = self.open.take() {
            drop(open.file);
            let _ = std::fs::remove_file(&open.path);
        }
        self.report.errors.push((inner, message));
    }

    /// One entry the walker refused before opening anything — an unsupported
    /// compression method, say.
    pub(crate) fn refuse(&mut self, name: &str, message: impl Into<String>) {
        let key = normalize(name).unwrap_or_else(|| name.to_string());
        if self.dests.contains_key(&key) {
            self.report.errors.push((key, message.into()));
        }
    }

    /// Whether this name is one the plan wants, without opening it. Lets a
    /// walker skip the work of decompressing an entry nobody asked for.
    pub(crate) fn wants(&self, name: &str) -> bool {
        normalize(name).is_some_and(|key| self.dests.contains_key(&key))
    }

    /// The cancel/pause checkpoint, for walkers between entries.
    pub(crate) fn checkpoint(&self) -> Result<()> {
        self.ctx.checkpoint()
    }

    /// Give up whatever is half-written and hand back what happened.
    pub(crate) fn finish(mut self, cancelled: bool) -> ExtractReport {
        // PLAN §5's contract for a cancelled operation: what landed is real,
        // and the file that was mid-flight when the button was pressed is not
        // left behind as a truncated copy of something.
        if let Some(open) = self.open.take() {
            drop(open.file);
            let _ = std::fs::remove_file(&open.path);
        }
        self.report.cancelled = cancelled;
        self.report
    }
}

/// Carry out a settled plan.
///
/// Blocking, cancellable, and safe to run twice: a retry re-walks the archive
/// and every destination it already made is refused by `create_new`, so the
/// second run produces errors rather than duplicates. In practice a retry is
/// rare, because per-entry failures are collected rather than returned.
pub fn extract(plan: &ExtractPlan, ctx: &TaskCtx) -> Result<ExtractReport> {
    let format = super::detect(&plan.archive)?;
    let dests = destinations(plan)?;

    ctx.set_total(plan.total_len, plan.file_count() as u64);

    // Directories first, top down, so nothing has to create a parent on the
    // fly. `create_dir_all` rather than `create_dir`: the plan's order already
    // guarantees parents come first, and being idempotent about it is what lets
    // a merge into an existing tree work at all.
    for dir in &dests.dirs {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return Err(DfError::io(dir, e));
        }
    }

    let mut sink = Sink::new(ctx, &dests.files);
    let outcome = match format {
        ArchiveFormat::Zip => {
            let mut file = File::open(&plan.archive).map_err(|e| DfError::io(&plan.archive, e))?;
            super::zip::extract_into(&mut file, &mut sink)
        }
        ArchiveFormat::Tar => {
            let file = File::open(&plan.archive).map_err(|e| DfError::io(&plan.archive, e))?;
            super::tar::extract_into(file, &mut sink)
        }
        other => extract_compressed_tar(&plan.archive, other, &mut sink),
    };

    let cancelled = matches!(outcome, Err(DfError::Cancelled));
    if let Err(e) = outcome {
        if !cancelled {
            return Err(e);
        }
    }

    let mut report = sink.finish(cancelled);
    report.skipped = plan.skipped.clone();
    // A cancelled extraction has half a tree on disk, and half a tree is
    // exactly what a manifest describes correctly — so it is still undoable,
    // and `u` takes back what actually landed.
    report.record = plan_record(&dests);
    Ok(report)
}

/// Stream a compressed tar through its decompressor and the extracting walker.
///
/// The same spawn the listing uses ([`super::list`]), for the same reason: the
/// child reads the file directly, so there is no second thread and no way for
/// the parent to deadlock writing a pipe.
fn extract_compressed_tar(path: &Path, format: ArchiveFormat, sink: &mut Sink<'_>) -> Result<()> {
    let Some(binary) = format.decompressor() else {
        return Err(DfError::Op(format!(
            "{} archives are not supported",
            format.label()
        )));
    };
    let file = File::open(path).map_err(|e| DfError::io(path, e))?;
    let mut child = std::process::Command::new(binary)
        .arg("-dc")
        .stdin(std::process::Stdio::from(file))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DfError::Op(format!(
                    "{} archives need `{binary}`, which is not installed",
                    format.label()
                ))
            } else {
                DfError::io(path, e)
            }
        })?;

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(DfError::Op(format!("{binary} produced no output")));
    };

    let result = super::tar::extract_into(stdout, sink);

    match &result {
        Ok(()) => {
            let status = child.wait().map_err(|e| DfError::io(path, e))?;
            if !status.success() {
                return Err(DfError::Op(format!("{binary} exited with {status}")));
            }
        }
        // Stopped early — cancelled, or a parse failure. The child is writing
        // into a pipe nobody is reading and has to be killed, or a cancelled
        // extraction leaves a stuck `zstd` behind every time.
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    result
}

/// Read a text member into memory, for the preview of an entry inside an
/// archive (PLAN §7.3: "previews inside").
///
/// Capped rather than streamed: this is a preview, it runs off a cursor move,
/// and an entry that does not fit in `limit` is one the pane could not show
/// anyway. Returns `None` for an entry that is not in the archive, is a
/// directory, is encrypted, or is bigger than the cap — all four of which the
/// caller draws the same way, as an info card with no body.
pub fn read_entry(path: &Path, inner: &str, limit: usize) -> Result<Option<Vec<u8>>> {
    let Some(key) = normalize(inner) else {
        return Ok(None);
    };
    let format = super::detect(path)?;
    let mut out: Option<Vec<u8>> = None;
    let mut buffer = Buffer {
        want: key,
        limit,
        got: &mut out,
    };
    match format {
        ArchiveFormat::Zip => {
            let mut file = File::open(path).map_err(|e| DfError::io(path, e))?;
            super::zip::read_one(&mut file, &mut buffer)?;
        }
        ArchiveFormat::Tar => {
            let file = File::open(path).map_err(|e| DfError::io(path, e))?;
            super::tar::read_one(file, &mut buffer)?;
        }
        // A compressed tar would mean decompressing the whole thing to find one
        // member, which is not a cost a cursor move may pay. The card shows the
        // entry's facts without its contents, which is what it does for a
        // binary member anyway.
        _ => {}
    }
    Ok(out)
}

/// One member, collected into memory. The read-side twin of [`Sink`], with the
/// same rule: the name is a key, never a path.
pub(crate) struct Buffer<'a> {
    want: String,
    limit: usize,
    got: &'a mut Option<Vec<u8>>,
}

impl Buffer<'_> {
    pub(crate) fn wants(&self, name: &str) -> bool {
        self.got.is_none() && normalize(name).as_deref() == Some(self.want.as_str())
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    /// Take a whole member. Over the cap, nothing is kept — a half file is not
    /// a preview, it is a file with its end cut off.
    pub(crate) fn take(&mut self, bytes: Vec<u8>) {
        if bytes.len() <= self.limit {
            *self.got = Some(bytes);
        }
    }

    pub(crate) fn done(&self) -> bool {
        self.got.is_some()
    }
}

/// Read exactly `buf.len()` bytes, or as many as there are. Shared by the two
/// walkers, which both sit on a stream with no seek.
pub(crate) fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut at = 0usize;
    while at < buf.len() {
        match reader.read(&mut buf[at..]) {
            Ok(0) => break,
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(at)
}
