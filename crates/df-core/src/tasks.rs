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
