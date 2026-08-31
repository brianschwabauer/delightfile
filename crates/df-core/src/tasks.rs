//! The task engine: a worker pool with progress, pause/resume, cancel and a
//! reorderable queue.
//!
//! Copies, moves, deletes, previews and directory scans are all tasks, and the
//! `w` panel (PLAN §5) shows the same state machine that drives them — one
//! model, not a UI guess at what the workers are doing. Workers never touch the
//! window: they publish results on a channel and ring the event loop's `Wake`
//! bell (PLAN §1), which is what keeps an idle delightfile at zero repaints.
//! The state machine is a pure function over `(inputs, Instant)` so its
//! transitions are tested rather than watched.
//!
//! ## Shape
//!
//! - A [`Job`] is a unit of work that can be *re-run from the top*, because a
//!   transient failure is retried `bizarre_retry` times (PLAN §5).
//! - A [`TaskCtx`] is the only thing a job is handed: it reports progress and
//!   it is where the job asks "should I still be running?" — [`TaskCtx::checkpoint`]
//!   blocks while paused and returns [`DfError::Cancelled`] when cancelled.
//!   Every long loop in [`crate::ops`] calls it between chunks.
//! - A [`TaskCtx`] can be built [detached](TaskCtx::detached), with no engine
//!   behind it. That is what lets an operation be called straight from a test
//!   (or from the UI thread for a one-file rename) without spinning a pool.
//! - The UI never reads a job. It reads [`TaskEngine::snapshot`] — a plain
//!   `Vec<TaskSnapshot>` — after being woken by the notifier, or drains
//!   [`TaskEngine::events`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::config::TasksConfig;
use crate::{DfError, Result};

/// How long a paused worker sleeps before re-checking its flags.
///
/// Pause/resume goes through a condvar, so this timeout is only a backstop
/// against a missed notification; 100 ms is far below the ~250 ms at which a
/// person notices a button did nothing, and costs one wakeup per paused task
/// per tenth of a second — nothing next to the copy that is paused.
const PAUSE_POLL: Duration = Duration::from_millis(100);

/// First retry backoff; each further retry doubles it.
///
/// With `bizarre_retry = 3` that is 50 + 100 + 200 ms of waiting before a task
/// gives up. Long enough for the momentary `EAGAIN`/`EBUSY` of a busy NFS mount
/// or an autofs mount still coming up, short enough that a genuinely broken
/// copy still reports failure inside half a second.
const RETRY_BACKOFF: Duration = Duration::from_millis(50);

/// Minimum gap between two notifier calls caused by *progress*.
///
/// State changes always notify immediately — those are what the user is waiting
/// to see. Progress is a firehose (one call per chunk, i.e. per megabyte), and
/// repainting faster than the display refreshes is wasted work; 50 ms is ~20
/// repaints a second, above the rate at which a progress bar reads as smooth
/// and below the rate at which it costs anything.
const PROGRESS_NOTIFY_INTERVAL: Duration = Duration::from_millis(50);

/// Identifier for a queued or running task. Monotonic, never reused, so a stale
/// UI reference resolves to "gone" rather than to somebody else's task.
pub type TaskId = u64;

/// The wake-the-UI hook. Called on every state change and (throttled) on
/// progress; the app installs one that posts winit's zero-sized `Wake` event.
pub type Notifier = Box<dyn Fn() + Send + Sync>;

/// Which pool a job runs in.
///
/// Yazi's split, kept: *micro* is for the many small jobs whose latency the
/// user feels (a single-file rename, a preview, a directory scan) and *macro*
/// is for the few big ones that saturate a disk (a 40 GB copy, a recursive
/// delete). Two pools mean a tree copy can never starve the preview of the file
/// the cursor is sitting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    Micro,
    Macro,
}

/// How far along a task is, in both the units a user cares about.
///
/// Bytes drive the bar; files drive the "312 / 4,001" label. Totals are 0 until
/// the job has measured its work, and a job that cannot know its total (a
/// stream) simply leaves them 0 — the UI shows an indeterminate bar rather than
/// a lie.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
}

impl Progress {
    /// Fraction complete in `0.0..=1.0`, or `None` when the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        if self.bytes_total > 0 {
            Some((self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0) as f32)
        } else if self.files_total > 0 {
            Some((self.files_done as f64 / self.files_total as f64).clamp(0.0, 1.0) as f32)
        } else {
            None
        }
    }
}

/// Where a task is in its life.
///
/// `Paused` and `Failed` keep the progress/attempt count they had, because the
/// `w` panel has to show a paused copy's bar and a failing one's retry count —
/// dropping them would make the panel forget what it was showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running(Progress),
    Paused(Progress),
    Cancelled,
    Failed { error: String, retries: u32 },
    Done,
}

impl TaskState {
    /// Still occupying the queue: the `w` panel shows it, and quitting should
    /// warn about it.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            TaskState::Pending | TaskState::Running(_) | TaskState::Paused(_)
        )
    }

    /// Progress if this state carries any.
    pub fn progress(&self) -> Option<Progress> {
        match self {
            TaskState::Running(p) | TaskState::Paused(p) => Some(*p),
            _ => None,
        }
    }
}

/// One row of the `w` panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub id: TaskId,
    pub name: String,
    pub lane: Lane,
    pub state: TaskState,
}

/// A state transition, for a UI that would rather react than diff snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEvent {
    pub id: TaskId,
    pub name: String,
    pub state: TaskState,
}

/// The cancel/pause bits a running job checks between chunks.
///
/// Shared by `Arc` between the engine (which sets them) and the job's
/// [`TaskCtx`] (which reads them). The condvar is what makes resume immediate
/// instead of "within `PAUSE_POLL`".
#[derive(Debug, Default)]
pub struct TaskFlags {
    cancel: AtomicBool,
    paused: AtomicBool,
    lock: Mutex<()>,
    wake: Condvar,
}

impl TaskFlags {
    pub fn new() -> TaskFlags {
        TaskFlags::default()
    }

    /// Ask the job to stop at its next checkpoint. One-way: a cancelled task is
    /// never un-cancelled, so a job that has begun tearing down its partial
    /// output cannot be told half-way through to carry on.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        let _guard = self.lock.lock();
        self.wake.notify_all();
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        let _guard = self.lock.lock();
        self.wake.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

/// Where a job's progress reports go.
///
/// The engine's implementation writes into the task registry and rings the
/// notifier; [`NullSink`] throws them away, which is what a detached context
/// uses. Tests implement it to observe — or to cancel — a job mid-flight at an
/// exact byte count, which is how "cancel removes the partial file" is tested
/// deterministically instead of with a sleep.
pub trait ProgressSink: Send + Sync {
    /// Declare the size of the work. May be called more than once as a job
    /// discovers more (a recursive copy measures as it walks).
    fn set_total(&self, bytes: u64, files: u64);
    /// Add to the completed counts.
    fn advance(&self, bytes: u64, files: u64);
}

/// A sink that discards everything, for operations run outside the pool.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl ProgressSink for NullSink {
    fn set_total(&self, _bytes: u64, _files: u64) {}
    fn advance(&self, _bytes: u64, _files: u64) {}
}

/// Everything a running job is allowed to know about the world outside it.
#[derive(Clone)]
pub struct TaskCtx {
    flags: Arc<TaskFlags>,
    sink: Arc<dyn ProgressSink>,
}

impl std::fmt::Debug for TaskCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskCtx")
            .field("cancelled", &self.flags.is_cancelled())
            .field("paused", &self.flags.is_paused())
            .finish()
    }
}

impl TaskCtx {
    /// A context with no engine behind it: progress is discarded, and the
    /// returned flags are the only handle for cancelling it.
    pub fn detached() -> TaskCtx {
        TaskCtx {
            flags: Arc::new(TaskFlags::new()),
            sink: Arc::new(NullSink),
        }
    }

    pub fn with_sink(flags: Arc<TaskFlags>, sink: Arc<dyn ProgressSink>) -> TaskCtx {
        TaskCtx { flags, sink }
    }

    /// The cancel/pause bits, for whoever is driving this task from outside.
    pub fn flags(&self) -> Arc<TaskFlags> {
        Arc::clone(&self.flags)
    }

    pub fn set_total(&self, bytes: u64, files: u64) {
        self.sink.set_total(bytes, files);
    }

    pub fn advance(&self, bytes: u64, files: u64) {
        self.sink.advance(bytes, files);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flags.is_cancelled()
    }

    /// The one call every loop in [`crate::ops`] makes between chunks: it
    /// blocks for as long as the task is paused, and returns
    /// [`DfError::Cancelled`] the moment it is cancelled.
    pub fn checkpoint(&self) -> Result<()> {
        if self.flags.is_cancelled() {
            return Err(DfError::Cancelled);
        }
        while self.flags.is_paused() && !self.flags.is_cancelled() {
            let guard = match self.flags.lock.lock() {
                Ok(g) => g,
                // A poisoned lock means another thread panicked while holding a
                // guard that protects nothing but the condvar. Nothing is
                // corrupt; carry on rather than poisoning this task too.
                Err(poisoned) => poisoned.into_inner(),
            };
            let _unused = self.flags.wake.wait_timeout(guard, PAUSE_POLL);
        }
        if self.flags.is_cancelled() {
            return Err(DfError::Cancelled);
        }
        Ok(())
    }
}

/// A unit of work for the pool.
///
/// `run` must be safe to call again from the top: a transient failure is
/// retried, so a job that has already half-written its output has to clean that
/// up itself (which is exactly what [`crate::ops::copy`] does with a partial
/// destination file).
pub trait Job: Send + 'static {
    /// Label for the `w` panel: "Copy 4 items → ~/Work".
    fn name(&self) -> String;
    fn lane(&self) -> Lane;
    fn run(&mut self, ctx: &TaskCtx) -> Result<()>;
    /// Whether a failure is worth retrying. The default classifies io errors
    /// that a busy or slow filesystem produces and nothing else — retrying
    /// `EACCES` three times only makes the user wait longer for the same no.
    fn is_transient(&self, err: &DfError) -> bool {
        default_transient(err)
    }
}

/// The default [`Job::is_transient`] rule.
pub fn default_transient(err: &DfError) -> bool {
    let DfError::Io { source, .. } = err else {
        return false;
    };
    use std::io::ErrorKind::*;
    if matches!(source.kind(), Interrupted | WouldBlock | TimedOut) {
        return true;
    }
    matches!(
        source.raw_os_error(),
        // EAGAIN, EBUSY, ENFILE, EMFILE, ETXTBSY, ESTALE: all of them mean
        // "not now" rather than "not ever".
        Some(11) | Some(16) | Some(23) | Some(24) | Some(26) | Some(116)
    )
}

/// A [`Job`] made from a closure, for the many one-liner tasks (and for tests).
pub struct FnJob<F> {
    name: String,
    lane: Lane,
    run: F,
}

impl<F> FnJob<F>
where
    F: FnMut(&TaskCtx) -> Result<()> + Send + 'static,
{
    pub fn new(name: impl Into<String>, lane: Lane, run: F) -> FnJob<F> {
        FnJob {
            name: name.into(),
            lane,
            run,
        }
    }
}

impl<F> Job for FnJob<F>
where
    F: FnMut(&TaskCtx) -> Result<()> + Send + 'static,
{
    fn name(&self) -> String {
        self.name.clone()
    }
    fn lane(&self) -> Lane {
        self.lane
    }
    fn run(&mut self, ctx: &TaskCtx) -> Result<()> {
        (self.run)(ctx)
    }
}

struct TaskRecord {
    name: String,
    lane: Lane,
    state: TaskState,
    flags: Arc<TaskFlags>,
    /// The worker has let go of this task for good.
    ///
    /// Separate from the state because `Failed` is *not* always the end: a
    /// transient failure publishes `Failed { retries }` — which is what the `w`
    /// panel should show while it backs off — and then runs again. Anything
    /// waiting for a task to be over waits on this, not on the state.
    terminal: bool,
}

struct Shared {
    tasks: Mutex<BTreeMap<TaskId, TaskRecord>>,
    /// Signalled whenever a task leaves the active set, so `wait_idle` and
    /// `join` do not have to poll.
    idle: Condvar,
    next_id: AtomicU64,
    notifier: Mutex<Option<Notifier>>,
    events: Sender<TaskEvent>,
    retries: u32,
}

impl Shared {
    fn lock_tasks(&self) -> std::sync::MutexGuard<'_, BTreeMap<TaskId, TaskRecord>> {
        match self.tasks.lock() {
            Ok(g) => g,
            // The registry is a plain map of plain data; a panic elsewhere
            // cannot have left it half-updated in a way that matters, and
            // losing the whole task panel over one poisoned lock is worse.
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn notify(&self) {
        let guard = match self.notifier.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(n) = guard.as_ref() {
            n();
        }
    }

    /// Write a new state, publish the event, ring the bell.
    fn set_state(&self, id: TaskId, state: TaskState) {
        self.publish(id, state, false);
    }

    /// The last state this task will ever have.
    fn finish(&self, id: TaskId, state: TaskState) {
        self.publish(id, state, true);
    }

    fn publish(&self, id: TaskId, state: TaskState, terminal: bool) {
        let mut event = None;
        {
            let mut tasks = self.lock_tasks();
            if let Some(rec) = tasks.get_mut(&id) {
                if rec.state == state && rec.terminal == terminal {
                    return;
                }
                rec.state = state.clone();
                rec.terminal = terminal;
                event = Some(TaskEvent {
                    id,
                    name: rec.name.clone(),
                    state,
                });
            }
        }
        if let Some(event) = event {
            // Full channel is impossible (unbounded) and a disconnected one
            // just means nobody is listening — neither is worth failing a task.
            let _dropped = self.events.send(event);
            if terminal {
                self.idle.notify_all();
            }
            self.notify();
        }
    }
}

struct EngineSink {
    shared: Arc<Shared>,
    id: TaskId,
    last_notify: Mutex<Instant>,
}

impl EngineSink {
    /// Apply `f` to this task's progress, then wake the UI at most every
    /// [`PROGRESS_NOTIFY_INTERVAL`].
    fn update(&self, f: impl FnOnce(&mut Progress)) {
        {
            let mut tasks = self.shared.lock_tasks();
            let Some(rec) = tasks.get_mut(&self.id) else {
                return;
            };
            match &mut rec.state {
                TaskState::Running(p) | TaskState::Paused(p) => f(p),
                // Progress reported by a task that is already finishing (a
                // final `advance` racing the cancel) has nowhere to go.
                _ => return,
            }
        }
        let now = Instant::now();
        let due = {
            let mut last = match self.last_notify.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            if now.duration_since(*last) >= PROGRESS_NOTIFY_INTERVAL {
                *last = now;
                true
            } else {
                false
            }
        };
        if due {
            self.shared.notify();
        }
    }
}

impl ProgressSink for EngineSink {
    fn set_total(&self, bytes: u64, files: u64) {
        self.update(|p| {
            p.bytes_total = bytes;
            p.files_total = files;
        });
    }

    fn advance(&self, bytes: u64, files: u64) {
        self.update(|p| {
            p.bytes_done = p.bytes_done.saturating_add(bytes);
            p.files_done = p.files_done.saturating_add(files);
        });
    }
}

/// The pool. Two lanes, `micro_workers`/`macro_workers` threads each.
///
/// Dropping the engine closes the queues and joins the workers, so anything
/// already queued still runs; [`TaskEngine::shutdown_now`] cancels first if the
/// app is quitting and does not care.
pub struct TaskEngine {
    shared: Arc<Shared>,
    senders: Option<(Sender<Queued>, Sender<Queued>)>,
    workers: Vec<std::thread::JoinHandle<()>>,
    events_rx: Receiver<TaskEvent>,
}

type Queued = (TaskId, Box<dyn Job>);

impl TaskEngine {
    /// Start the pool. Worker counts come from config (10/10 by default, PLAN
    /// §5); a configured 0 is clamped to 1 so a typo cannot wedge every
    /// operation in the program forever.
    pub fn new(config: &TasksConfig) -> TaskEngine {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            tasks: Mutex::new(BTreeMap::new()),
            idle: Condvar::new(),
            next_id: AtomicU64::new(1),
            notifier: Mutex::new(None),
            events: events_tx,
            retries: config.bizarre_retry,
        });
        let (micro_tx, micro_rx) = crossbeam_channel::unbounded::<Queued>();
        let (macro_tx, macro_rx) = crossbeam_channel::unbounded::<Queued>();

        let mut workers = Vec::new();
        for (lane, rx, count) in [
            (Lane::Micro, &micro_rx, config.micro_workers.max(1)),
            (Lane::Macro, &macro_rx, config.macro_workers.max(1)),
        ] {
            for n in 0..count {
                let rx = rx.clone();
                let shared = Arc::clone(&shared);
                let name = format!("df-{lane:?}-{n}").to_lowercase();
                match std::thread::Builder::new()
                    .name(name)
                    .spawn(move || worker_loop(shared, rx))
                {
                    Ok(h) => workers.push(h),
                    Err(e) => log::warn!("task engine: could not spawn worker: {e}"),
                }
            }
        }

        TaskEngine {
            shared,
            senders: Some((micro_tx, macro_tx)),
            workers,
            events_rx,
        }
    }

    /// Install the wake-the-UI hook. Replaces any previous one.
    pub fn set_notifier(&self, notifier: Notifier) {
        let mut guard = match self.shared.notifier.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(notifier);
    }

    /// The transition stream. The receiver is cloneable and multi-consumer;
    /// nothing is lost if it is never read.
    pub fn events(&self) -> Receiver<TaskEvent> {
        self.events_rx.clone()
    }

    /// Queue a job. Returns immediately with the id the UI will refer to it by.
    pub fn spawn(&self, job: impl Job) -> TaskId {
        self.spawn_boxed(Box::new(job))
    }

    pub fn spawn_boxed(&self, job: Box<dyn Job>) -> TaskId {
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
        let lane = job.lane();
        let name = job.name();
        {
            let mut tasks = self.shared.lock_tasks();
            tasks.insert(
                id,
                TaskRecord {
                    name: name.clone(),
                    lane,
                    state: TaskState::Pending,
                    flags: Arc::new(TaskFlags::new()),
                    terminal: false,
                },
            );
        }
        let _dropped = self.shared.events.send(TaskEvent {
            id,
            name,
            state: TaskState::Pending,
        });
        self.shared.notify();
        if let Some((micro, r#macro)) = &self.senders {
            let tx = match lane {
                Lane::Micro => micro,
                Lane::Macro => r#macro,
            };
            if let Err(e) = tx.send((id, job)) {
                // Only reachable if the workers are gone, i.e. during teardown.
                log::warn!("task engine: queue closed, dropping job: {e}");
                self.shared.finish(id, TaskState::Cancelled);
            }
        }
        id
    }

    fn flags(&self, id: TaskId) -> Option<Arc<TaskFlags>> {
        let tasks = self.shared.lock_tasks();
        tasks.get(&id).map(|r| Arc::clone(&r.flags))
    }

    /// Pause a running task at its next checkpoint. A pending task is paused
    /// too — it will sit in `Paused` the moment a worker picks it up rather
    /// than running a chunk first.
    pub fn pause(&self, id: TaskId) {
        if let Some(flags) = self.flags(id) {
            flags.pause();
        }
        let mut tasks = self.shared.lock_tasks();
        if let Some(rec) = tasks.get_mut(&id) {
            if let TaskState::Running(p) = rec.state {
                rec.state = TaskState::Paused(p);
                let _dropped = self.shared.events.send(TaskEvent {
                    id,
                    name: rec.name.clone(),
                    state: TaskState::Paused(p),
                });
            }
        }
        drop(tasks);
        self.shared.notify();
    }

    pub fn resume(&self, id: TaskId) {
        if let Some(flags) = self.flags(id) {
            flags.resume();
        }
        let mut tasks = self.shared.lock_tasks();
        if let Some(rec) = tasks.get_mut(&id) {
            if let TaskState::Paused(p) = rec.state {
                rec.state = TaskState::Running(p);
                let _dropped = self.shared.events.send(TaskEvent {
                    id,
                    name: rec.name.clone(),
                    state: TaskState::Running(p),
                });
            }
        }
        drop(tasks);
        self.shared.notify();
    }

    /// Cancel a task. A pending one never starts; a running one stops at its
    /// next checkpoint and cleans up after itself.
    pub fn cancel(&self, id: TaskId) {
        if let Some(flags) = self.flags(id) {
            flags.cancel();
        }
    }

    pub fn cancel_all(&self) {
        let flags: Vec<_> = {
            let tasks = self.shared.lock_tasks();
            tasks
                .values()
                .filter(|r| !r.terminal)
                .map(|r| Arc::clone(&r.flags))
                .collect()
        };
        for f in flags {
            f.cancel();
        }
    }

    /// What the `w` panel renders, oldest task first.
    pub fn snapshot(&self) -> Vec<TaskSnapshot> {
        let tasks = self.shared.lock_tasks();
        tasks
            .iter()
            .map(|(id, rec)| TaskSnapshot {
                id: *id,
                name: rec.name.clone(),
                lane: rec.lane,
                state: rec.state.clone(),
            })
            .collect()
    }

    pub fn task(&self, id: TaskId) -> Option<TaskSnapshot> {
        let tasks = self.shared.lock_tasks();
        tasks.get(&id).map(|rec| TaskSnapshot {
            id,
            name: rec.name.clone(),
            lane: rec.lane,
            state: rec.state.clone(),
        })
    }

    /// Forget finished tasks. The `w` panel calls this when the user clears the
    /// list; nothing removes rows behind the user's back.
    pub fn clear_finished(&self) {
        let mut tasks = self.shared.lock_tasks();
        tasks.retain(|_, rec| rec.state.is_active());
    }

    pub fn active_count(&self) -> usize {
        let tasks = self.shared.lock_tasks();
        tasks.values().filter(|r| !r.terminal).count()
    }

    /// Block until one task leaves the active set, or the deadline passes.
    /// Returns the final state, or `None` on timeout.
    pub fn join(&self, id: TaskId, timeout: Duration) -> Option<TaskState> {
        let deadline = Instant::now() + timeout;
        let mut tasks = self.shared.lock_tasks();
        loop {
            match tasks.get(&id) {
                None => return None,
                Some(rec) if rec.terminal => return Some(rec.state.clone()),
                Some(_) => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _timeout) = match self.shared.idle.wait_timeout(tasks, deadline - now) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            tasks = guard;
        }
    }

    /// Block until nothing is pending, running or paused. `false` on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut tasks = self.shared.lock_tasks();
        loop {
            if !tasks.values().any(|r| !r.terminal) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, _timeout) = match self.shared.idle.wait_timeout(tasks, deadline - now) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            tasks = guard;
        }
    }

    /// Cancel everything, then close the queues and join the workers.
    pub fn shutdown_now(mut self) {
        self.cancel_all();
        self.teardown();
    }

    fn teardown(&mut self) {
        drop(self.senders.take());
        for handle in std::mem::take(&mut self.workers) {
            let _joined = handle.join();
        }
    }
}

impl Drop for TaskEngine {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn worker_loop(shared: Arc<Shared>, rx: Receiver<Queued>) {
    while let Ok((id, mut job)) = rx.recv() {
        run_one(&shared, id, job.as_mut());
    }
}

/// Run one job to a terminal state, retrying transient failures.
fn run_one(shared: &Arc<Shared>, id: TaskId, job: &mut dyn Job) {
    let Some(flags) = ({
        let tasks = shared.lock_tasks();
        tasks.get(&id).map(|r| Arc::clone(&r.flags))
    }) else {
        // The task was cleared before a worker got to it.
        return;
    };

    if flags.is_cancelled() {
        shared.finish(id, TaskState::Cancelled);
        return;
    }

    let sink = Arc::new(EngineSink {
        shared: Arc::clone(shared),
        id,
        last_notify: Mutex::new(Instant::now() - PROGRESS_NOTIFY_INTERVAL),
    });
    let ctx = TaskCtx::with_sink(Arc::clone(&flags), sink);

    let mut attempt: u32 = 0;
    loop {
        let start_state = if flags.is_paused() {
            TaskState::Paused(Progress::default())
        } else {
            TaskState::Running(Progress::default())
        };
        shared.set_state(id, start_state);

        match job.run(&ctx) {
            Ok(()) => {
                shared.finish(id, TaskState::Done);
                return;
            }
            Err(DfError::Cancelled) => {
                shared.finish(id, TaskState::Cancelled);
                return;
            }
            Err(e) => {
                if flags.is_cancelled() {
                    shared.finish(id, TaskState::Cancelled);
                    return;
                }
                let transient = job.is_transient(&e);
                let failed = TaskState::Failed {
                    error: e.to_string(),
                    retries: attempt,
                };
                if !transient || attempt >= shared.retries {
                    shared.finish(id, failed);
                    return;
                }
                // Shown while it backs off, then superseded by the next run.
                shared.set_state(id, failed);
                // Backoff, but stay cancellable while waiting.
                let wait = RETRY_BACKOFF * 2u32.saturating_pow(attempt);
                let deadline = Instant::now() + wait;
                while Instant::now() < deadline {
                    if flags.is_cancelled() {
                        shared.finish(id, TaskState::Cancelled);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5).min(deadline - Instant::now()));
                }
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn config(retries: u32) -> TasksConfig {
        TasksConfig {
            micro_workers: 2,
            macro_workers: 2,
            bizarre_retry: retries,
        }
    }

    /// Generous enough that a loaded CI box does not flake, short enough that a
    /// genuine hang fails the suite instead of hanging it.
    const T: Duration = Duration::from_secs(5);

    /// Poll until a task's state satisfies `pred`. Timing tests wait for the
    /// state they need rather than sleeping a guessed amount, which is what
    /// keeps them honest on a machine that is busy compiling something else.
    fn wait_for(engine: &TaskEngine, id: TaskId, pred: impl Fn(&TaskState) -> bool) -> TaskState {
        let deadline = Instant::now() + T;
        loop {
            if let Some(t) = engine.task(id) {
                if pred(&t.state) {
                    return t.state;
                }
            }
            assert!(Instant::now() < deadline, "state never arrived for task {id}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn runs_a_job_and_reports_done() {
        let engine = TaskEngine::new(&config(0));
        let ran = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ran);
        let id = engine.spawn(FnJob::new("t", Lane::Micro, move |ctx| {
            ctx.set_total(10, 1);
            ctx.advance(10, 1);
            r.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        assert_eq!(engine.join(id, T), Some(TaskState::Done));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(engine.wait_idle(T));
    }

    #[test]
    fn notifier_is_rung() {
        let engine = TaskEngine::new(&config(0));
        let hits = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&hits);
        engine.set_notifier(Box::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));
        let id = engine.spawn(FnJob::new("t", Lane::Micro, |_| Ok(())));
        engine.join(id, T);
        assert!(hits.load(Ordering::SeqCst) >= 2, "spawn + done at minimum");
    }

    #[test]
    fn progress_is_visible_in_the_snapshot() {
        let engine = TaskEngine::new(&config(0));
        let (tx, rx) = crossbeam_channel::bounded::<()>(0);
        let id = engine.spawn(FnJob::new("copy", Lane::Macro, move |ctx| {
            ctx.set_total(100, 2);
            ctx.advance(40, 1);
            // Hold the job open so the snapshot is taken mid-flight.
            let _ = rx.recv();
            Ok(())
        }));
        // Wait for the progress to land.
        let deadline = Instant::now() + T;
        loop {
            if let Some(TaskState::Running(p)) = engine.task(id).map(|t| t.state) {
                if p.bytes_done == 40 {
                    assert_eq!(p.bytes_total, 100);
                    assert_eq!(p.files_done, 1);
                    assert_eq!(p.fraction(), Some(0.4));
                    break;
                }
            }
            assert!(Instant::now() < deadline, "progress never appeared");
            std::thread::sleep(Duration::from_millis(5));
        }
        let snap = engine.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].name, "copy");
        assert_eq!(snap[0].lane, Lane::Macro);
        drop(tx);
        engine.join(id, T);
    }

    #[test]
    fn pause_blocks_then_resume_continues() {
        let engine = TaskEngine::new(&config(0));
        let steps = Arc::new(AtomicUsize::new(0));
        let s = Arc::clone(&steps);
        let id = engine.spawn(FnJob::new("t", Lane::Micro, move |ctx| {
            for _ in 0..20 {
                ctx.checkpoint()?;
                s.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }));
        wait_for(&engine, id, |s| matches!(s, TaskState::Running(_)));
        engine.pause(id);
        // Let the job reach its checkpoint and block there.
        std::thread::sleep(Duration::from_millis(40));
        let frozen = steps.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            steps.load(Ordering::SeqCst),
            frozen,
            "a paused job must not advance"
        );
        assert!(matches!(
            engine.task(id).map(|t| t.state),
            Some(TaskState::Paused(_))
        ));
        engine.resume(id);
        assert_eq!(engine.join(id, T), Some(TaskState::Done));
        assert_eq!(steps.load(Ordering::SeqCst), 20);
    }

    #[test]
    fn cancel_stops_at_the_next_checkpoint() {
        let engine = TaskEngine::new(&config(0));
        let steps = Arc::new(AtomicUsize::new(0));
        let s = Arc::clone(&steps);
        let id = engine.spawn(FnJob::new("t", Lane::Micro, move |ctx| {
            for _ in 0..1000 {
                ctx.checkpoint()?;
                s.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(())
        }));
        wait_for(&engine, id, |s| matches!(s, TaskState::Running(_)));
        engine.cancel(id);
        assert_eq!(engine.join(id, T), Some(TaskState::Cancelled));
        assert!(steps.load(Ordering::SeqCst) < 1000);
    }

    #[test]
    fn cancel_while_paused_wakes_the_job() {
        let engine = TaskEngine::new(&config(0));
        let id = engine.spawn(FnJob::new("t", Lane::Micro, |ctx| {
            for _ in 0..1000 {
                ctx.checkpoint()?;
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(())
        }));
        wait_for(&engine, id, |s| matches!(s, TaskState::Running(_)));
        engine.pause(id);
        wait_for(&engine, id, |s| matches!(s, TaskState::Paused(_)));
        engine.cancel(id);
        assert_eq!(engine.join(id, T), Some(TaskState::Cancelled));
    }

    #[test]
    fn a_pending_task_cancelled_before_it_starts_never_runs() {
        // One worker, occupied, so the second task is definitely still pending.
        let engine = TaskEngine::new(&TasksConfig {
            micro_workers: 1,
            macro_workers: 1,
            bizarre_retry: 0,
        });
        let (tx, rx) = crossbeam_channel::bounded::<()>(0);
        let blocker = engine.spawn(FnJob::new("block", Lane::Micro, move |_| {
            let _ = rx.recv();
            Ok(())
        }));
        let ran = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ran);
        let victim = engine.spawn(FnJob::new("victim", Lane::Micro, move |_| {
            r.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        wait_for(&engine, blocker, |s| matches!(s, TaskState::Running(_)));
        assert_eq!(engine.task(victim).map(|t| t.state), Some(TaskState::Pending));
        engine.cancel(victim);
        drop(tx);
        engine.join(blocker, T);
        assert_eq!(engine.join(victim, T), Some(TaskState::Cancelled));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn transient_failures_are_retried_then_succeed() {
        let engine = TaskEngine::new(&config(3));
        let attempts = Arc::new(AtomicUsize::new(0));
        let a = Arc::clone(&attempts);
        let id = engine.spawn(FnJob::new("flaky", Lane::Micro, move |_| {
            let n = a.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(DfError::io(
                    "/tmp/x",
                    std::io::Error::from(std::io::ErrorKind::Interrupted),
                ))
            } else {
                Ok(())
            }
        }));
        assert_eq!(engine.join(id, T), Some(TaskState::Done));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn transient_failures_give_up_after_bizarre_retry() {
        let engine = TaskEngine::new(&config(3));
        let attempts = Arc::new(AtomicUsize::new(0));
        let a = Arc::clone(&attempts);
        let id = engine.spawn(FnJob::new("doomed", Lane::Micro, move |_| {
            a.fetch_add(1, Ordering::SeqCst);
            Err(DfError::io(
                "/tmp/x",
                std::io::Error::from(std::io::ErrorKind::Interrupted),
            ))
        }));
        match engine.join(id, T) {
            Some(TaskState::Failed { retries, .. }) => assert_eq!(retries, 3),
            other => panic!("expected Failed, got {other:?}"),
        }
        // Initial attempt plus three retries.
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn permanent_failures_are_not_retried() {
        let engine = TaskEngine::new(&config(3));
        let attempts = Arc::new(AtomicUsize::new(0));
        let a = Arc::clone(&attempts);
        let id = engine.spawn(FnJob::new("nope", Lane::Micro, move |_| {
            a.fetch_add(1, Ordering::SeqCst);
            Err(DfError::io(
                "/tmp/x",
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ))
        }));
        assert!(matches!(
            engine.join(id, T),
            Some(TaskState::Failed { .. })
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn transient_classification() {
        let t = |e: std::io::Error| default_transient(&DfError::io("/x", e));
        assert!(t(std::io::Error::from(std::io::ErrorKind::Interrupted)));
        assert!(t(std::io::Error::from(std::io::ErrorKind::WouldBlock)));
        assert!(t(std::io::Error::from_raw_os_error(16))); // EBUSY
        assert!(!t(std::io::Error::from(std::io::ErrorKind::NotFound)));
        assert!(!t(std::io::Error::from_raw_os_error(28))); // ENOSPC
        assert!(!default_transient(&DfError::Op("nope".into())));
    }

    #[test]
    fn events_stream_the_transitions() {
        let engine = TaskEngine::new(&config(0));
        let rx = engine.events();
        let id = engine.spawn(FnJob::new("t", Lane::Micro, |_| Ok(())));
        engine.join(id, T);
        let mut seen = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push(ev.state);
        }
        assert!(seen.contains(&TaskState::Pending));
        assert!(seen.contains(&TaskState::Done));
    }

    #[test]
    fn both_lanes_run_independently() {
        let engine = TaskEngine::new(&config(0));
        let (tx, rx) = crossbeam_channel::bounded::<()>(0);
        // Saturate the macro lane.
        let mut big = Vec::new();
        for _ in 0..2 {
            let rx = rx.clone();
            big.push(engine.spawn(FnJob::new("big", Lane::Macro, move |_| {
                let _ = rx.recv();
                Ok(())
            })));
        }
        let small = engine.spawn(FnJob::new("small", Lane::Micro, |_| Ok(())));
        assert_eq!(
            engine.join(small, T),
            Some(TaskState::Done),
            "a micro job must not wait behind macro jobs"
        );
        drop(tx);
        for id in big {
            engine.join(id, T);
        }
    }

    #[test]
    fn clear_finished_keeps_active_rows() {
        let engine = TaskEngine::new(&config(0));
        let id = engine.spawn(FnJob::new("t", Lane::Micro, |_| Ok(())));
        engine.join(id, T);
        assert_eq!(engine.snapshot().len(), 1);
        engine.clear_finished();
        assert!(engine.snapshot().is_empty());
    }

    #[test]
    fn detached_context_is_cancellable() {
        let ctx = TaskCtx::detached();
        assert!(ctx.checkpoint().is_ok());
        let flags = ctx.flags();
        flags.cancel();
        assert!(matches!(ctx.checkpoint(), Err(DfError::Cancelled)));
        assert!(ctx.is_cancelled());
    }

    #[test]
    fn progress_fraction_prefers_bytes_then_files() {
        let p = Progress {
            bytes_done: 1,
            bytes_total: 4,
            files_done: 1,
            files_total: 2,
        };
        assert_eq!(p.fraction(), Some(0.25));
        let p = Progress {
            files_done: 1,
            files_total: 2,
            ..Default::default()
        };
        assert_eq!(p.fraction(), Some(0.5));
        assert_eq!(Progress::default().fraction(), None);
    }
}
