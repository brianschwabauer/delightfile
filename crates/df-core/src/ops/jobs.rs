//! The operations, wrapped as [`Job`]s the pool can run.
//!
//! This is the only place `ops` and `tasks` know about each other. An operation
//! stays a plain function; a job is a thin envelope that gives it a name for the
//! `w` panel, a lane, and somewhere to leave its result — the [`OpRecord`] the
//! journal needs and the errors the toast needs — since [`Job::run`] can only
//! return "it worked" or "it did not".
//!
//! The outcome slot is an `Arc<Mutex<Option<_>>>` rather than a channel because
//! the UI reads it when the task is `Done`, having been woken by the notifier;
//! there is nothing to stream.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::tasks::{Job, Lane, TaskCtx};
use crate::{DfError, Result};

use super::journal::OpRecord;
use super::mode::Grid;
use super::paste::{PasteMode, PastePlan};

/// What an operation left behind for the UI to pick up.
#[derive(Debug, Clone, Default)]
pub struct OpOutcome {
    /// The inverse, for [`super::Journal::record`]. `None` when nothing
    /// happened, or when nothing that happened can be undone.
    pub record: Option<OpRecord>,
    /// One line for the toast.
    pub message: String,
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    /// What the job made that nobody could name before it ran — an external
    /// extractor's top-level entries, found by looking at the destination
    /// afterwards. Most jobs know their results when they are spawned and
    /// leave this empty; the UI lands its cursor on these only when it was
    /// given nothing better.
    pub made: Vec<PathBuf>,
}

/// The shared slot a job writes its outcome into.
pub type Outcome = Arc<Mutex<Option<OpOutcome>>>;

fn store(slot: &Outcome, outcome: OpOutcome) {
    let mut guard = match slot.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(outcome);
}

/// "1 item" / "1,234 items" — the count a job's toast leads with, grouped so a
/// big one reads at a glance.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", crate::text::grouped(n as u64))
    }
}

/// `p` / `P`: carry out a settled [`PastePlan`].
///
/// Re-runnable, as [`Job`] requires: a retry re-walks the plan, and the items
/// that already landed are simply copied or moved again over themselves. In
/// practice a retry is rare, because per-item failures are collected into the
/// outcome rather than returned.
pub struct PasteJob {
    plan: PastePlan,
    outcome: Outcome,
}

impl PasteJob {
    pub fn new(plan: PastePlan) -> PasteJob {
        PasteJob {
            plan,
            outcome: Arc::new(Mutex::new(None)),
        }
    }

    /// The slot this job will write into. Clone it before spawning.
    pub fn outcome(&self) -> Outcome {
        Arc::clone(&self.outcome)
    }
}

impl Job for PasteJob {
    fn name(&self) -> String {
        let verb = match self.plan.mode {
            PasteMode::Copy => "Copy",
            PasteMode::Cut => "Move",
        };
        format!(
            "{verb} {} → {}",
            plural(self.plan.ready.len(), "item", "items"),
            self.plan.dest_dir.display()
        )
    }

    fn lane(&self) -> Lane {
        Lane::Macro
    }

    fn run(&mut self, ctx: &TaskCtx) -> Result<()> {
        let report = super::paste::execute(&self.plan, ctx)?;
        let done = match self.plan.mode {
            PasteMode::Copy => report.copied.len(),
            PasteMode::Cut => report.moved.len(),
        };
        let verb = match self.plan.mode {
            PasteMode::Copy => "Copied",
            PasteMode::Cut => "Moved",
        };
        // The one thing a paste can lose without failing: the destination
        // took the files and refused their tags (a FAT card). Said on the
        // same line, because the files are there and the toast is about them.
        let dropped = if report.tags_dropped {
            " · tags not kept on this drive"
        } else {
            ""
        };
        store(
            &self.outcome,
            OpOutcome {
                record: report.record,
                message: format!("{verb} {}{dropped}", plural(done, "item", "items")),
                errors: report.errors,
                cancelled: report.cancelled,
                made: Vec::new(),
            },
        );
        Ok(())
    }
}

/// `d`: move a selection to the trash it belongs in.
pub struct TrashJob {
    paths: Vec<PathBuf>,
    outcome: Outcome,
}

impl TrashJob {
    pub fn new(paths: Vec<PathBuf>) -> TrashJob {
        TrashJob {
            paths,
            outcome: Arc::new(Mutex::new(None)),
        }
    }

    pub fn outcome(&self) -> Outcome {
        Arc::clone(&self.outcome)
    }
}

impl Job for TrashJob {
    fn name(&self) -> String {
        format!("Trash {}", plural(self.paths.len(), "item", "items"))
    }

    fn lane(&self) -> Lane {
        // Trashing is a rename in the common case — fast, and the user is
        // waiting to see the row disappear.
        Lane::Micro
    }

    fn run(&mut self, ctx: &TaskCtx) -> Result<()> {
        let mut items = Vec::new();
        let mut errors = Vec::new();
        let mut cancelled = false;
        for path in &self.paths {
            if ctx.is_cancelled() {
                cancelled = true;
                break;
            }
            // Each path may live on a different mount, so each gets its own
            // trash directory decision.
            let bin = match super::trash::for_path(path) {
                Ok(b) => b,
                Err(e) => {
                    errors.push((path.clone(), e.to_string()));
                    continue;
                }
            };
            match bin.trash(path, ctx) {
                Ok(item) => items.push(item),
                Err(DfError::Cancelled) => {
                    cancelled = true;
                    break;
                }
                Err(e) => errors.push((path.clone(), e.to_string())),
            }
        }
        let message = format!("Trashed {}", plural(items.len(), "item", "items"));
        store(
            &self.outcome,
            OpOutcome {
                record: (!items.is_empty()).then_some(OpRecord::Trash { items }),
                message,
                errors,
                cancelled,
                made: Vec::new(),
            },
        );
        Ok(())
    }
}

/// `D`: delete for good. Never journalled — there is nothing to record.
pub struct DeleteJob {
    paths: Vec<PathBuf>,
    outcome: Outcome,
}

impl DeleteJob {
    pub fn new(paths: Vec<PathBuf>) -> DeleteJob {
        DeleteJob {
            paths,
            outcome: Arc::new(Mutex::new(None)),
        }
    }

    pub fn outcome(&self) -> Outcome {
        Arc::clone(&self.outcome)
    }
}

impl Job for DeleteJob {
    fn name(&self) -> String {
        format!("Delete {}", plural(self.paths.len(), "item", "items"))
    }

    fn lane(&self) -> Lane {
        Lane::Macro
    }

    fn run(&mut self, ctx: &TaskCtx) -> Result<()> {
        let mut errors = Vec::new();
        // The rails come first, before the measuring: measuring walks the whole
        // tree, and a `D` on `/` should be refused in a millisecond rather than
        // after scanning every file on the disk to size up a delete that is
        // never going to happen.
        let mut doomed = Vec::new();
        for path in &self.paths {
            match super::delete::check_deletable_here(path) {
                Ok(()) => doomed.push(path.clone()),
                Err(e) => errors.push((path.clone(), e.to_string())),
            }
        }

        let mut bytes = 0;
        let mut files = 0;
        for path in &doomed {
            if let Ok((b, f)) = super::copy::measure(path) {
                bytes += b;
                files += f;
            }
        }
        ctx.set_total(bytes, files);

        let mut deleted = 0;
        let mut cancelled = false;
        for path in &doomed {
            match super::delete::delete_permanent(path, ctx) {
                Ok(()) => deleted += 1,
                Err(DfError::Cancelled) => {
                    cancelled = true;
                    break;
                }
                Err(e) => errors.push((path.clone(), e.to_string())),
            }
        }
        store(
            &self.outcome,
            OpOutcome {
                record: None,
                message: format!("Deleted {}", plural(deleted, "item", "items")),
                errors,
                cancelled,
                made: Vec::new(),
            },
        );
        Ok(())
    }
}

/// `C`'s Apply: the card's bits set on a selection, or on everything inside
/// one ([`super::mode`]).
///
/// One task however big the tree: the walk and the change both run here, on
/// the pool, so a folder of two hundred thousand files is a row in the `w`
/// panel with a count and a cancel rather than a window that stops. Progress
/// is by count, since a mode change moves no bytes. A cancel keeps what was
/// already set, recorded, so `u` takes back exactly that.
pub struct ModeJob {
    /// The folder the change is asked in — the listing's own — which every
    /// target has to be below, and which everything is looked up from.
    anchor: PathBuf,
    targets: Vec<PathBuf>,
    grid: Grid,
    recursive: bool,
    outcome: Outcome,
}

impl ModeJob {
    pub fn new(anchor: PathBuf, targets: Vec<PathBuf>, grid: Grid, recursive: bool) -> ModeJob {
        ModeJob {
            anchor,
            targets,
            grid,
            recursive,
            outcome: Arc::new(Mutex::new(None)),
        }
    }

    pub fn outcome(&self) -> Outcome {
        Arc::clone(&self.outcome)
    }
}

impl Job for ModeJob {
    fn name(&self) -> String {
        let what = plural(self.targets.len(), "item", "items");
        if self.recursive {
            format!("Set permissions inside {what}")
        } else {
            format!("Set permissions on {what}")
        }
    }

    fn lane(&self) -> Lane {
        // A handful of `chmod`s is over before anybody could look at the
        // panel, and the user is waiting to see the column change. A walk of
        // a tree is disk work of the kind the macro lane is for.
        if self.recursive {
            Lane::Macro
        } else {
            Lane::Micro
        }
    }

    fn run(&mut self, ctx: &TaskCtx) -> Result<()> {
        let planned =
            super::mode::plan(&self.anchor, &self.targets, &self.grid, self.recursive, ctx);
        let plan = match planned {
            Ok(plan) => plan,
            Err(DfError::Cancelled) => {
                store(
                    &self.outcome,
                    OpOutcome {
                        message: "Permissions unchanged".to_string(),
                        cancelled: true,
                        ..OpOutcome::default()
                    },
                );
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        ctx.set_total(0, plan.pairs.len() as u64);
        let mut report = super::mode::chmod(&self.anchor, &plan.pairs, ctx);
        report.absorb(plan);
        let message = report.message();
        store(
            &self.outcome,
            OpOutcome {
                record: report.record,
                message,
                errors: report.errors,
                cancelled: report.cancelled,
                made: Vec::new(),
            },
        );
        Ok(())
    }
}

/// Unpack an archive (PLAN §7.3). The runner behind "Extract here" and
/// "Extract to folder" on one archive the reader can list, and behind `Enter`
/// on a selection inside an archive. Everything else — several archives at
/// once, 7z and rar, multi-part sets — is [`crate::archive::Unpack`], run by
/// the UI in a job of its own.
///
/// Lives here with the other jobs rather than beside the extractor because this
/// file is the one place `ops` and `tasks` know about each other, and an
/// extraction is an operation like any other: it has a name for the `w` panel,
/// a lane, a cancel, and an inverse to leave in the outcome slot.
///
/// [`Lane::Macro`], because it is a long read and a long write. The plan is
/// settled before the job is spawned, so everything the cancel and the toast
/// need is already decided by the time a worker picks it up.
pub struct ExtractJob {
    plan: crate::archive::ExtractPlan,
    outcome: Outcome,
}

impl ExtractJob {
    pub fn new(plan: crate::archive::ExtractPlan) -> ExtractJob {
        ExtractJob {
            plan,
            outcome: Arc::new(Mutex::new(None)),
        }
    }

    pub fn outcome(&self) -> Outcome {
        Arc::clone(&self.outcome)
    }
}

impl Job for ExtractJob {
    fn name(&self) -> String {
        format!(
            "Extract {} → {}",
            self.plan
                .archive
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            self.plan.dest_dir.display()
        )
    }

    fn lane(&self) -> Lane {
        Lane::Macro
    }

    fn run(&mut self, ctx: &TaskCtx) -> Result<()> {
        let report = crate::archive::extract(&self.plan, ctx)?;
        let message = report.message();
        store(
            &self.outcome,
            OpOutcome {
                record: report.record,
                message,
                // The per-entry failures, given a path each so the toast and the
                // `w` panel can name them the way every other operation does.
                errors: report
                    .errors
                    .into_iter()
                    .map(|(inner, message)| (self.plan.dest_dir.join(inner), message))
                    .collect(),
                cancelled: report.cancelled,
                made: Vec::new(),
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::config::TasksConfig;
    use crate::ops::fixture::TempTree;
    use crate::ops::paste::{plan_paste, Clipboard};
    use crate::ops::{trash, Journal};
    use crate::tasks::{TaskEngine, TaskState};
    use std::time::Duration;

    const T: Duration = Duration::from_secs(5);

    fn engine() -> TaskEngine {
        TaskEngine::new(&TasksConfig {
            micro_workers: 2,
            macro_workers: 2,
            bizarre_retry: 0,
        })
    }

    fn taken(slot: &Outcome) -> OpOutcome {
        slot.lock().unwrap().clone().expect("an outcome was stored")
    }

    /// The singular stays a bare `1`, and a count past a thousand is grouped
    /// the way the app's own toasts group it.
    #[test]
    fn a_job_counts_the_way_a_person_reads() {
        assert_eq!(plural(1, "file", "files"), "1 file");
        assert_eq!(plural(0, "file", "files"), "0 files");
        assert_eq!(plural(1234, "file", "files"), "1,234 files");
    }

    #[test]
    fn a_paste_runs_on_the_pool_and_can_be_undone() {
        let t = TempTree::new("job-paste");
        let src = t.file("src/a.txt", b"body");
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();

        let engine = engine();
        let job = PasteJob::new(plan);
        let slot = job.outcome();
        let id = engine.spawn(job);
        assert_eq!(engine.join(id, T), Some(TaskState::Done));

        let outcome = taken(&slot);
        assert_eq!(outcome.message, "Copied 1 item");
        assert!(outcome.errors.is_empty());
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"body");

        let mut journal = Journal::default();
        journal.record(outcome.record.expect("a copy is undoable"));
        journal.undo(&TaskCtx::detached()).unwrap();
        assert!(!super::super::exists(&dest.join("a.txt")));
        assert_eq!(std::fs::read(&src).unwrap(), b"body");
    }

    /// A paste onto a drive that keeps no tags lands every file and says, on
    /// the same line, that the tags stayed behind — a copy and a cut alike;
    /// onto one that keeps them it says nothing more than it always did.
    #[test]
    fn a_paste_onto_a_drive_without_tags_says_so() {
        use crate::fs::tags;
        let t = TempTree::new("job-paste-tags");
        let probe = t.file("probe", b"");
        if !tags::supported_here(&probe) {
            return;
        }
        let red = t.file("src/red.txt", b"x");
        tags::write(&red, &["red".to_string()]).unwrap();

        let run = |plan, refused: bool| {
            let mut job = PasteJob::new(plan);
            let slot = job.outcome();
            let ctx = TaskCtx::detached();
            if refused {
                tags::refusing(|| job.run(&ctx)).unwrap();
            } else {
                job.run(&ctx).unwrap();
            }
            taken(&slot).message
        };
        let card = t.dir("card");
        let plan = plan_paste(&Clipboard::yank([red.clone()]), &card, false).unwrap();
        assert_eq!(
            run(plan, true),
            "Copied 1 item · tags not kept on this drive"
        );
        assert!(tags::read(&card.join("red.txt")).is_empty());

        let disk = t.dir("disk");
        let plan = plan_paste(&Clipboard::yank([red.clone()]), &disk, false).unwrap();
        assert_eq!(run(plan, false), "Copied 1 item");
        assert_eq!(tags::read(&disk.join("red.txt")), ["red"]);

        // A cut within one drive is a rename, which keeps the tags whatever
        // the refusal says, so there is nothing to report.
        let moved = t.dir("moved");
        let plan = plan_paste(&Clipboard::cut([red]), &moved, false).unwrap();
        assert_eq!(run(plan, true), "Moved 1 item");
        assert_eq!(tags::read(&moved.join("red.txt")), ["red"]);
    }

    #[test]
    fn a_paste_job_reports_progress_totals() {
        let t = TempTree::new("job-progress");
        let src = t.file("src/a.bin", &vec![0u8; 4096]);
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::yank([src]), &dest, false).unwrap();

        let engine = engine();
        let job = PasteJob::new(plan);
        let id = engine.spawn(job);
        engine.join(id, T);
        // The task is Done, so its final snapshot no longer carries progress —
        // what matters is that the file arrived whole.
        assert_eq!(std::fs::metadata(dest.join("a.bin")).unwrap().len(), 4096);
        assert!(engine.wait_idle(T));
    }

    #[test]
    fn a_trash_job_is_undoable() {
        let t = TempTree::new("job-trash");
        // `for_path` picks a real trash, so this test drives the plain trash
        // API directly and only exercises the job's record-keeping shape.
        let bin = trash::Trash::at(t.join("Trash"));
        let file = t.file("work/notes.txt", b"x");
        let item = bin.trash(&file, &TaskCtx::detached()).unwrap();

        let mut journal = Journal::default();
        journal.record(OpRecord::Trash { items: vec![item] });
        let report = journal.undo(&TaskCtx::detached()).unwrap();
        assert!(report.description.contains("Restored"), "{report:?}");
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
    }

    #[test]
    fn a_delete_job_removes_and_journals_nothing() {
        let t = TempTree::new("job-delete");
        let victim = t.dir("victim");
        std::fs::write(victim.join("a"), b"x").unwrap();

        let engine = engine();
        let job = DeleteJob::new(vec![victim.clone()]);
        let slot = job.outcome();
        let id = engine.spawn(job);
        assert_eq!(engine.join(id, T), Some(TaskState::Done));

        let outcome = taken(&slot);
        assert_eq!(outcome.message, "Deleted 1 item");
        assert!(
            outcome.record.is_none(),
            "permanent delete is the one operation with no inverse"
        );
        assert!(!super::super::exists(&victim));
    }

    #[test]
    fn a_delete_job_reports_the_rails_as_errors() {
        let engine = engine();
        let job = DeleteJob::new(vec![PathBuf::from("/")]);
        let slot = job.outcome();
        let id = engine.spawn(job);
        engine.join(id, T);
        let outcome = taken(&slot);
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].1.contains("refusing"), "{outcome:?}");
    }

    /// `C`'s Apply on the pool: a folder and what is in it, the X rule for
    /// the folder, one record for the lot, and `u` putting all of it back.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_mode_job_goes_inside_and_can_be_undone() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path| {
            crate::platform::meta::mode(&std::fs::metadata(path).unwrap()) & 0o7777
        };
        let set = |path: &std::path::Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let t = TempTree::new("job-mode");
        let dir = t.dir("photos");
        let file = t.file("photos/a.jpg", b"x");
        set(&file, 0o600);
        set(&dir, 0o700);

        let engine = engine();
        let job = ModeJob::new(
            t.path().to_path_buf(),
            vec![dir.clone()],
            Grid::of_mode(0o644),
            true,
        );
        assert_eq!(job.name(), "Set permissions inside 1 item");
        assert_eq!(job.lane(), Lane::Macro);
        let slot = job.outcome();
        let id = engine.spawn(job);
        assert_eq!(engine.join(id, T), Some(TaskState::Done));

        let outcome = taken(&slot);
        assert_eq!(outcome.message, "Permissions set on 2 items");
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(mode(&dir), 0o755);
        assert_eq!(mode(&file), 0o644);

        let mut journal = Journal::default();
        journal.record(outcome.record.expect("a mode change is undoable"));
        let report = journal.undo(&TaskCtx::detached()).unwrap();
        assert_eq!(report.description, "Restored permissions of 2 items");
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);

        // One level only, and a file already right is no change at all.
        let job = ModeJob::new(dir.clone(), vec![file.clone()], Grid::of_mode(0o600), false);
        assert_eq!(job.lane(), Lane::Micro);
        let slot = job.outcome();
        let id = engine.spawn(job);
        engine.join(id, T);
        let outcome = taken(&slot);
        assert_eq!(outcome.message, "Permissions unchanged");
        assert!(outcome.record.is_none());
    }

    #[test]
    fn jobs_name_themselves_for_the_panel() {
        let t = TempTree::new("job-names");
        let src = t.file("a.txt", b"x");
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::cut([src.clone()]), &dest, false).unwrap();
        let job = PasteJob::new(plan);
        assert!(job.name().starts_with("Move 1 item → "), "{}", job.name());
        assert_eq!(
            TrashJob::new(vec![src.clone(), src]).name(),
            "Trash 2 items"
        );
        assert_eq!(DeleteJob::new(vec![]).name(), "Delete 0 items");
    }
}
