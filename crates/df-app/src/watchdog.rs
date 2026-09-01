//! A second opinion on whether the UI thread is alive.
//!
//! The event loop is repaint-on-event, which has one blind spot: when the
//! thread that paints is the thread that is stuck, nothing it owns can say so.
//! The freeze that motivated this was exactly that — a swapchain acquire
//! blocking for a second at a time, with the terminal showing nothing but an
//! unrelated warning. So one thread, off to the side, watches two clocks:
//!
//! - how long the current frame has been in flight, which catches a paint
//!   that is blocked inside the driver;
//! - how long a redraw has been requested without a frame landing, which
//!   catches a loop that stopped delivering `RedrawRequested` at all.
//!
//! Either past its limit is logged at `warn`, with the elapsed time, and again
//! at intervals while it persists — so a user who sees the window go dead
//! also sees, in the terminal, which of the two it was. Idle costs nothing
//! but one sleeping thread: it wakes once a second and touches two atomics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a frame may be in flight before it is a stall.
const FRAME_LIMIT: Duration = Duration::from_secs(2);
/// How long a requested redraw may go unanswered before it is a stall.
const REDRAW_LIMIT: Duration = Duration::from_secs(3);
/// How often the watcher looks, and therefore how often a persisting stall is
/// logged again.
const TICK: Duration = Duration::from_secs(1);

/// The UI thread's side of the watchdog. Cheap to call: one atomic store.
#[derive(Clone)]
pub struct Watchdog {
    inner: Arc<Inner>,
}

struct Inner {
    /// When the process started; timestamps below are milliseconds since.
    epoch: Instant,
    /// Millis at which the current frame began, or 0 when no frame is in
    /// flight.
    frame_started: AtomicU64,
    /// Millis at which the oldest unanswered redraw was requested, or 0 when
    /// none is outstanding.
    redraw_requested: AtomicU64,
}

impl Watchdog {
    /// Start the watcher thread. Returns the handle the UI thread reports to.
    pub fn start() -> Watchdog {
        let inner = Arc::new(Inner {
            epoch: Instant::now(),
            frame_started: AtomicU64::new(0),
            redraw_requested: AtomicU64::new(0),
        });
        let watched = Arc::clone(&inner);
        // A failed spawn leaves the app exactly as it was without a watchdog;
        // there is nothing to do about it but say so.
        if let Err(e) = std::thread::Builder::new()
            .name("df-watchdog".into())
            .spawn(move || watch(&watched))
        {
            log::warn!("watchdog thread did not start: {e}");
        }
        Watchdog { inner }
    }

    fn now(&self) -> u64 {
        // `.max(1)`: zero means "none", so the first millisecond is nudged.
        (self.inner.epoch.elapsed().as_millis() as u64).max(1)
    }

    /// A redraw was asked for. Only the oldest outstanding request is kept,
    /// so a burst of requests measures from the first.
    pub fn redraw_requested(&self) {
        let now = self.now();
        let _ = self.inner.redraw_requested.compare_exchange(
            0,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// A frame is starting. Clears the outstanding-redraw clock: the request
    /// has been answered, whatever the frame goes on to do.
    pub fn frame_started(&self) {
        let now = self.now();
        self.inner.frame_started.store(now, Ordering::Relaxed);
        self.inner.redraw_requested.store(0, Ordering::Relaxed);
    }

    /// The frame is over, presented or not.
    pub fn frame_finished(&self) {
        self.inner.frame_started.store(0, Ordering::Relaxed);
    }
}

fn watch(inner: &Inner) {
    loop {
        std::thread::sleep(TICK);
        let now = inner.epoch.elapsed().as_millis() as u64;
        let started = inner.frame_started.load(Ordering::Relaxed);
        if started != 0 {
            let stuck = Duration::from_millis(now.saturating_sub(started));
            if stuck >= FRAME_LIMIT {
                log::warn!(
                    "watchdog: the UI thread has been inside one frame for {:.1} s \
                     (a blocked swapchain acquire or present, or a blocking call in the frame)",
                    stuck.as_secs_f32()
                );
            }
            continue;
        }
        let requested = inner.redraw_requested.load(Ordering::Relaxed);
        if requested != 0 {
            let waiting = Duration::from_millis(now.saturating_sub(requested));
            if waiting >= REDRAW_LIMIT {
                log::warn!(
                    "watchdog: a redraw was requested {:.1} s ago and no frame has started \
                     (the event loop is not delivering RedrawRequested)",
                    waiting.as_secs_f32()
                );
            }
        }
    }
}
