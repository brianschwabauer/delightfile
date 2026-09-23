//! Extracting whole archives: "Extract to folder", "Extract here" and "Extract
//! all into one folder" on files in a real directory, whatever the files are
//! and however many of them there are.
//!
//! Each archive takes one of three roads, decided when the job reaches it:
//!
//! 1. **The reader here** — [`super::list`], [`super::plan_extract`],
//!    [`super::unpack::extract`] — for everything it can list. That is the
//!    road with the per-entry safety checks, so it is always tried first.
//! 2. **An [`Extractor`]** — 7-Zip, or `bsdtar` — for what the reader cannot
//!    list (7z, rar, a bzip2 tar) and for every multi-part set.
//! 3. **Two stages**, for the two things 7-Zip only does half of. A byte-split
//!    set (`bundle.tar.gz.001`, …) is *joined* by `7z -tsplit`, and 7-Zip
//!    reads a `.tar.bz2` as a bzip2 stream with one file in it, so it
//!    *decompresses* to `bundle.tar` and stops. Either way the intermediate
//!    file goes into a scratch directory inside the destination — same disk,
//!    so no second copy of a large archive lands in a RAM-backed `/tmp` — and
//!    is extracted from there by whichever road it needs. The scratch
//!    directory is removed on every way out, including a cancel.
//!
//! Several archives in one job run strictly one after another, which is what
//! makes "last write wins" a statement about the order they were listed in
//! rather than about which worker was faster.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::external::{self, Extractor, ExtractorKind};
use super::unpack::{self, ExtractReport};
use super::volumes::{archive_stem, VolumeKind};
use super::{ArchiveError, SkipReason};
use crate::ops::exists;
use crate::ops::journal::{CopyManifest, OpRecord};
use crate::ops::OpOutcome;
use crate::tasks::TaskCtx;

/// One thing to extract whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whole {
    /// The archive, or the head of a multi-part set.
    pub head: PathBuf,
    /// Set when `head` heads a multi-part set, which only an extractor reads.
    pub volumes: Option<VolumeKind>,
}

impl Whole {
    /// The head's file name, for messages.
    pub fn name(&self) -> String {
        file_name(&self.head)
    }

    /// What it is, for "Install 7-Zip to extract …".
    pub fn format_label(&self) -> String {
        match &self.volumes {
            Some(kind) => format!("multi-part {}", kind.label()),
            None => format_label(&self.name()),
        }
    }
}

/// The format a name claims to be: everything after its archive stem,
/// lowercased — `tar.bz2`, `7z`, `rar`, `cbr`.
pub fn format_label(name: &str) -> String {
    let stem = archive_stem(name);
    name.get(stem.len()..)
        .map(|rest| rest.trim_start_matches('.').to_ascii_lowercase())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| "this archive".to_string())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// What extracting one [`Whole`] did.
#[derive(Debug, Default)]
pub struct WholeReport {
    /// The top-level paths under the destination that this extraction wrote,
    /// in the order it wrote them: what a cursor belongs on afterwards. From
    /// the plan when the reader did the work, and from looking at the
    /// destination before and after when an extractor did.
    pub tops: Vec<PathBuf>,
    /// The reader's own report, when the reader did the extracting.
    pub internal: Option<ExtractReport>,
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

impl WholeReport {
    fn failed(head: &Path, message: impl Into<String>) -> WholeReport {
        WholeReport {
            errors: vec![(head.to_path_buf(), message.into())],
            ..Default::default()
        }
    }

    /// It finished, and nothing in it failed.
    pub fn succeeded(&self) -> bool {
        self.errors.is_empty() && !self.cancelled
    }
}

/// How many intermediate files deep one extraction may go. A split set joins
/// into a compressed tar, which decompresses into a tar: two is all a real
/// archive needs, and the bound means no name can make this recurse.
const MAX_STAGES: u32 = 2;

/// The scratch directory's name prefix. A dot name, so a listing showing the
/// destination mid-extraction does not offer it as a row.
const SCRATCH_PREFIX: &str = ".delightfile-unpack-";

/// Extract `whole` into `dest`, which must exist.
///
/// `overwrite` is [`super::ExtractPlan::overwrite`]'s policy, carried to
/// whichever road the archive takes. `extractor` is what
/// [`super::external_extractor`] found — passed in rather than looked up, so a
/// caller checks once for a batch and a test can choose.
///
/// Never an `Err`: every failure is one archive's, and is reported in
/// [`WholeReport::errors`] against the archive's path so a batch carries on.
pub fn extract_whole(
    whole: &Whole,
    dest: &Path,
    overwrite: bool,
    extractor: Option<&Extractor>,
    ctx: &TaskCtx,
) -> WholeReport {
    extract_staged(whole, dest, overwrite, extractor, ctx, 0)
}

fn extract_staged(
    whole: &Whole,
    dest: &Path,
    overwrite: bool,
    extractor: Option<&Extractor>,
    ctx: &TaskCtx,
    depth: u32,
) -> WholeReport {
    let head = whole.head.as_path();
    if whole.volumes.is_none() {
        match super::list(head) {
            Ok(tree) => return read_here(&tree, head, dest, overwrite, ctx),
            // What the reader cannot do, somebody else may.
            Err(
                ArchiveError::Unsupported { .. }
                | ArchiveError::NotAnArchive(_)
                | ArchiveError::NoDecompressor { .. },
            ) => {}
            // A zip the reader could open and found broken is broken; a second
            // opinion would only make the error message harder to trust.
            Err(e) => return WholeReport::failed(head, e.to_string()),
        }
    }
    let Some(extractor) = extractor.filter(|e| whole.volumes.is_none() || e.reads_volumes()) else {
        return WholeReport::failed(
            head,
            format!("Install 7-Zip to extract {}", whole.format_label()),
        );
    };
    let stage = match &whole.volumes {
        Some(VolumeKind::Split { .. }) => Some(Stage::Join),
        None if extractor.kind == ExtractorKind::SevenZip && compressed_tar(&whole.name()) => {
            Some(Stage::Decompress)
        }
        _ => None,
    };
    match stage {
        None => by_extractor(extractor, head, dest, overwrite, ctx),
        Some(_) if depth >= MAX_STAGES => {
            WholeReport::failed(head, "too many layers of archive inside archive")
        }
        Some(stage) => two_stage(stage, extractor, head, dest, overwrite, ctx, depth),
    }
}

/// The reader's road: plan the whole tree, and write it.
fn read_here(
    tree: &super::ArchiveTree,
    head: &Path,
    dest: &Path,
    overwrite: bool,
    ctx: &TaskCtx,
) -> WholeReport {
    let mut plan = super::plan_extract(tree, &[], dest);
    plan.overwrite = overwrite;
    let mut seen = HashSet::new();
    let tops = plan
        .items
        .iter()
        .filter_map(|item| unpack::top_level(dest, &item.dest))
        .filter(|top| seen.insert(top.clone()))
        .collect();
    match unpack::extract(&plan, ctx) {
        Ok(report) => WholeReport {
            tops,
            errors: report
                .errors
                .iter()
                .map(|(inner, message)| (dest.join(inner), message.clone()))
                .collect(),
            cancelled: report.cancelled,
            internal: Some(report),
        },
        Err(e) => WholeReport::failed(head, e.to_string()),
    }
}

/// The extractor's road: run it, and see what appeared.
fn by_extractor(
    extractor: &Extractor,
    head: &Path,
    dest: &Path,
    overwrite: bool,
    ctx: &TaskCtx,
) -> WholeReport {
    let before = names_in(dest);
    let ran = match external::run(
        &extractor.program,
        &extractor.args(head, dest, overwrite),
        ctx,
    ) {
        Ok(ran) => ran,
        Err(e) => return WholeReport::failed(head, e.to_string()),
    };
    let mut report = WholeReport {
        tops: appeared(dest, &before),
        cancelled: ran.cancelled,
        ..Default::default()
    };
    if !ran.cancelled && !ran.succeeded() {
        report
            .errors
            .push((head.to_path_buf(), external::failure_line(&ran, head)));
    }
    report
}

/// The half of an extraction 7-Zip does on its own, before the other half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// `-tsplit`: put a byte-split set back into one file.
    Join,
    /// Take the compression off a compressed tar, leaving the tar.
    Decompress,
}

fn two_stage(
    stage: Stage,
    extractor: &Extractor,
    head: &Path,
    dest: &Path,
    overwrite: bool,
    ctx: &TaskCtx,
    depth: u32,
) -> WholeReport {
    let scratch = match Scratch::new(dest) {
        Ok(scratch) => scratch,
        Err(e) => return WholeReport::failed(head, crate::DfError::io(dest, e).to_string()),
    };
    let args: Vec<OsString> = match stage {
        Stage::Join => match extractor.join_args(head, &scratch.0) {
            Some(args) => args,
            None => return WholeReport::failed(head, "Install 7-Zip to extract a split archive"),
        },
        // Into an empty directory of our own, so the policy is moot; `-aoa`
        // keeps 7-Zip from stopping to ask about anything.
        Stage::Decompress => extractor.args(head, &scratch.0, true),
    };
    let ran = match external::run(&extractor.program, &args, ctx) {
        Ok(ran) => ran,
        Err(e) => return WholeReport::failed(head, e.to_string()),
    };
    if ran.cancelled {
        return WholeReport {
            cancelled: true,
            ..Default::default()
        };
    }
    if !ran.succeeded() {
        return WholeReport::failed(head, external::failure_line(&ran, head));
    }
    let Some(inner) = only_file(&scratch.0) else {
        return WholeReport::failed(head, "the extractor produced nothing to extract");
    };
    let mut report = extract_staged(
        &Whole {
            head: inner.clone(),
            volumes: None,
        },
        dest,
        overwrite,
        Some(extractor),
        ctx,
        depth + 1,
    );
    // The intermediate is an implementation detail; a failure in it is this
    // archive's failure, and is named after the file the user chose.
    for (path, _) in &mut report.errors {
        if *path == inner {
            *path = head.to_path_buf();
        }
    }
    report
}

/// Whether a name is a tar inside a single-stream compressor — the shape
/// 7-Zip unwraps one layer of and stops.
fn compressed_tar(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".tar.bz2",
        ".tbz2",
        ".tbz",
        ".tb2",
        ".tar.gz",
        ".tgz",
        ".tar.xz",
        ".txz",
        ".tar.zst",
        ".tzst",
        ".tar.lzma",
        ".tlz",
        ".tar.z",
        ".taz",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// A directory of our own inside the destination, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(dest: &Path) -> std::io::Result<Scratch> {
        let mut last = std::io::Error::other("no free scratch name");
        for n in 0..64u32 {
            let path = dest.join(format!("{SCRATCH_PREFIX}{}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Scratch(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The one regular file in `dir`, when there is exactly one.
fn only_file(dir: &Path) -> Option<PathBuf> {
    let mut entries = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok());
    let first = entries.next()?;
    if entries.next().is_some() || !first.file_type().ok()?.is_file() {
        return None;
    }
    Some(first.path())
}

/// The names directly in `dir`, for noticing what an extractor added.
fn names_in(dir: &Path) -> HashSet<OsString> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .collect()
        })
        .unwrap_or_default()
}

/// What is in `dir` now that was not in `before`, directories first and then
/// by name — the order the listing will show them in by default, so the first
/// is the row the cursor lands on. A scratch directory is never "made".
fn appeared(dir: &Path, before: &HashSet<OsString>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut made: Vec<(bool, OsString)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| !before.contains(&e.file_name()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with(SCRATCH_PREFIX))
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (!is_dir, e.file_name())
        })
        .collect();
    made.sort();
    made.into_iter().map(|(_, name)| dir.join(name)).collect()
}

/// A job's worth of whole-archive extraction: one or more archives into one
/// destination, in order.
#[derive(Debug, Clone)]
pub struct Unpack {
    pub items: Vec<Whole>,
    /// Where they go. Must exist before [`Unpack::run`].
    pub dest: PathBuf,
    /// Last write wins. Only for several archives merged into one place; see
    /// [`super::ExtractPlan::overwrite`].
    pub overwrite: bool,
    /// `dest` was made for this extraction a moment ago, so everything in it is
    /// the extraction's: undo takes the whole folder back, and a folder that
    /// ends up empty is removed rather than left as litter.
    pub fresh: bool,
}

impl Unpack {
    /// What the `w` panel calls it.
    pub fn name(&self) -> String {
        let what = match self.items.as_slice() {
            [one] => one.name(),
            items => format!("{} archives", items.len()),
        };
        format!("Extract {what} → {}", self.dest.display())
    }

    /// Extract every item, in order, and say what happened.
    ///
    /// Stops at a cancel; an archive that fails is reported and the next one
    /// still runs, the way one bad file does not stop a paste.
    pub fn run(&self, extractor: Option<&Extractor>, ctx: &TaskCtx) -> OpOutcome {
        let before = if self.fresh {
            HashSet::new()
        } else {
            names_in(&self.dest)
        };
        let mut reports: Vec<WholeReport> = Vec::new();
        for item in &self.items {
            if ctx.is_cancelled() {
                break;
            }
            let report = extract_whole(item, &self.dest, self.overwrite, extractor, ctx);
            let stop = report.cancelled;
            reports.push(report);
            if stop {
                break;
            }
        }
        let cancelled = ctx.is_cancelled() || reports.iter().any(|r| r.cancelled);

        let mut tops: Vec<PathBuf> = Vec::new();
        for top in reports.iter().flat_map(|r| r.tops.iter()) {
            if !tops.contains(top) {
                tops.push(top.clone());
            }
        }
        let message = self.message(&reports);
        let errors = reports
            .iter()
            .flat_map(|r| r.errors.iter().cloned())
            .collect();
        let record = self.record(&mut reports, &tops, &before);
        // A fresh folder is the row to land on, and the caller already knows
        // it; into an existing directory, only this job knows what it made.
        let made = if self.fresh {
            Vec::new()
        } else {
            tops.into_iter().filter(|top| exists(top)).collect()
        };
        OpOutcome {
            record,
            message,
            errors,
            cancelled,
            made,
        }
    }

    /// The one line for the toast.
    fn message(&self, reports: &[WholeReport]) -> String {
        if let [item] = self.items.as_slice() {
            let name = item.name();
            let Some(report) = reports.first() else {
                return format!("Did not extract {name}");
            };
            if let Some(internal) = &report.internal {
                return internal.message();
            }
            if report.cancelled {
                return format!("Extracted part of {name}");
            }
            if let Some((_, line)) = report.errors.first() {
                return format!("{name}: {line}");
            }
            return format!("Extracted {name}");
        }
        let n = self.items.len();
        let done = reports.iter().filter(|r| r.succeeded()).count();
        let count = if done == n {
            format!("{n} archives")
        } else {
            format!("{done} of {n} archives")
        };
        let skipped: Vec<(String, SkipReason)> = reports
            .iter()
            .filter_map(|r| r.internal.as_ref())
            .flat_map(|r| r.skipped.iter().cloned())
            .collect();
        format!(
            "Extracted {count} into {}{}",
            file_name(&self.dest),
            unpack::skip_note(&skipped)
        )
    }

    /// The inverse, when there is an honest one.
    ///
    /// - **A fresh folder** is the extraction's entirely, whatever wrote it —
    ///   the reader or an extractor — so it is recorded whole, the way
    ///   [`unpack::plan_record`] records a fresh top-level entry.
    /// - **One archive into an existing directory** keeps the reader's own
    ///   answer; an extractor's is `None`, because it does not say what it
    ///   merged into.
    /// - **Several into an existing directory** are undoable only when the
    ///   reader did all of them and every top-level path they wrote is new —
    ///   the same rule, applied to the batch: an overwrite or a merge into
    ///   something of the user's has no inverse.
    fn record(
        &self,
        reports: &mut [WholeReport],
        tops: &[PathBuf],
        before: &HashSet<OsString>,
    ) -> Option<OpRecord> {
        if self.fresh {
            let empty = std::fs::read_dir(&self.dest).is_ok_and(|mut d| d.next().is_none());
            if empty {
                let _ = std::fs::remove_dir(&self.dest);
                return None;
            }
            let manifest = CopyManifest::of_tree(&self.dest).ok()?;
            return Some(OpRecord::Copy {
                created: vec![manifest],
            });
        }
        if let [only] = reports {
            return only.internal.take().and_then(|report| report.record);
        }
        let all_read_here = reports.iter().all(|r| r.internal.is_some());
        let untouched = tops
            .iter()
            .all(|top| top.file_name().is_some_and(|name| !before.contains(name)));
        if !all_read_here || !untouched {
            return None;
        }
        let mut created = Vec::new();
        for top in tops.iter().filter(|top| exists(top)) {
            created.push(CopyManifest::of_tree(top).ok()?);
        }
        (!created.is_empty()).then_some(OpRecord::Copy { created })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_is_named_from_the_name() {
        assert_eq!(format_label("a.tar.bz2"), "tar.bz2");
        assert_eq!(format_label("a.7z"), "7z");
        assert_eq!(format_label("Comic.CBR"), "cbr");
        assert_eq!(format_label("noext"), "this archive");
        let set = Whole {
            head: PathBuf::from("/dl/photos.zip"),
            volumes: Some(VolumeKind::ZipSplit),
        };
        assert_eq!(set.format_label(), "multi-part zip");
    }

    #[test]
    fn only_compressed_tars_take_the_decompress_stage() {
        assert!(compressed_tar("a.tar.bz2"));
        assert!(compressed_tar("A.TBZ2"));
        assert!(!compressed_tar("notes.txt.bz2"));
        assert!(!compressed_tar("a.7z"));
    }
}
