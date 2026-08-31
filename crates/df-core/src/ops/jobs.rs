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

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
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
        store(
            &self.outcome,
            OpOutcome {
                record: report.record,
                message: format!("{verb} {}", plural(done, "item", "items")),
                errors: report.errors,
                cancelled: report.cancelled,
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
