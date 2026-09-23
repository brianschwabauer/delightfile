//! The *card* bodies, and an archive's listing, off the paint thread.
//!
//! Two panes do not go through [`crate::preview::Pane`] at all, because what
//! they are previewing is not a file on the disk: an archive's entry (PLAN
//! §7.3) and a remote service's row (PLAN §7.6). Both were reading their body
//! synchronously inside `poll_workers`, which is inside a frame:
//!
//! - `df_core::archive::read_entry` re-scans the container's directory and
//!   inflates up to a mebibyte — per entry the cursor stops on, so a held `↓`
//!   through a zip is one of those per key repeat, in the middle of an
//!   animation frame.
//! - the remote pane reads the already-downloaded temp file with
//!   `std::fs::read`, which is a network filesystem away often enough to
//!   matter and is a syscall on the paint thread always.
//!
//! So both take the shape every other worker here has: one thread, a channel,
//! the same [`crate::Wake`] bell, and newest-wins on an `AtomicU64` token —
//! the pattern [`crate::preview::decode`] documents. The card keeps saying
//! what it already knew until the body lands, which is what it does for a
//! remote download anyway.
//!
//! The third job is the preview pane's own: **an archive under the cursor,
//! listed** ([`Job::Archive`], [`super::listing`]). It is the same shape of
//! problem — a read of the whole file for a `.tar.zst`, a child process for a
//! 7z — so it is the same shape of worker. The pane runs an instance of its
//! own, so a slow listing never queues in front of a card body.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;

use super::listing::{self, Listing};

/// Which card a job belongs to.
///
/// **They are retired independently, and that is the whole point of this
/// type.** One `AtomicU64` for both cards meant an archive card cancelling its
/// body also retired the remote card's in-flight read — and the remote side's
/// "already showing this url" early return then never asked again, so the card
/// sat with an empty body until the cursor left the row and came back. One
/// counter per kind, because they are independent newest-wins races that
/// happen to share a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// An entry inside an archive the tab is browsing.
    Entry,
    Remote,
    /// A whole archive's listing, for the preview pane.
    Archive,
}

impl Which {
    fn slot(self) -> usize {
        match self {
            Which::Entry => 0,
            Which::Remote => 1,
            Which::Archive => 2,
        }
    }

    /// How long a job of this kind waits, from the moment it was asked for,
    /// before it may start.
    ///
    /// Only the listing waits. A card body is asked for once per row the
    /// cursor *stops* on inside an archive and costs a mebibyte at most. A
    /// listing is asked for by a preview that has already waited out df-core's
    /// debounce, and can then cost a whole `.tar.zst` decompressed. So it
    /// waits out the same [`df_core::preview::DEBOUNCE`] a second time: a held
    /// `↓` whose key repeat is just slow enough to let one preview through
    /// retires the listing that preview asked for before the wait is over, and
    /// the archive the cursor only passed is never opened. (One retired later
    /// still stops between two reads — [`listing::list`] — but a decompressor
    /// never started is cheaper than one killed.)
    fn settle(self) -> Duration {
        match self {
            Which::Archive => df_core::preview::DEBOUNCE,
            Which::Entry | Which::Remote => Duration::ZERO,
        }
    }
}

/// What to read.
pub enum Job {
    /// An entry inside an archive the tab is browsing.
    Entry {
        archive: PathBuf,
        /// The entry's path *inside* the container.
        inner: String,
        /// `df_core::archive::read_entry`'s cap.
        limit: usize,
    },
    /// A remote row whose bytes are already on the disk as a temp file.
    Remote { url: String, local: PathBuf },
    /// An archive on the disk, listed for the preview pane
    /// ([`super::listing::list`]).
    Archive { path: PathBuf },
}

impl Job {
    fn which(&self) -> Which {
        match self {
            Job::Entry { .. } => Which::Entry,
            Job::Remote { .. } => Which::Remote,
            Job::Archive { .. } => Which::Archive,
        }
    }
}

/// What came back. Carries its own subject, so the app can check the answer is
/// still about the row under the cursor without trusting the token alone.
pub enum Body {
    Entry {
        archive: PathBuf,
        inner: String,
        text: Option<String>,
    },
    Remote {
        url: String,
        text: Option<String>,
    },
    /// The listing, or the one line the pane says under its badge instead.
    Archive {
        path: PathBuf,
        listing: Result<Listing, String>,
    },
}

/// One queued job: its kind, its token, and when it was asked for.
type Queued = (Which, u64, Instant, Job);

/// The card-body worker.
///
/// Dropping it closes the channel; the worker finishes the job it is on and
/// exits, and the drop joins it so nothing outlives the window. A listing in
/// the middle of a large tar is retired by the drop's cancel first, so the
/// join waits for one read of it rather than the rest of the archive.
pub struct Bodies {
    jobs: Option<Sender<Queued>>,
    results: Receiver<Body>,
    /// One counter per [`Which`] — see that type for why they are not one.
    live: Arc<[AtomicU64; 3]>,
    next: [u64; 3],
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Bodies {
    pub fn start(notify: Notifier) -> Bodies {
        Bodies::named("df-card-body", notify)
    }

    /// The same worker under another thread name, so a profiler or `top -H`
    /// says which of the two instances a busy thread is.
    pub fn named(name: &str, notify: Notifier) -> Bodies {
        let (job_tx, job_rx) = unbounded::<Queued>();
        let (res_tx, res_rx) = unbounded::<Body>();
        let live = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
        let worker_live = Arc::clone(&live);
        let handle = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                for (which, token, made, job) in job_rx {
                    let slot = &worker_live[which.slot()];
                    // Checked before the read and again before the send: a
                    // held `↓` through a zip asks for forty and wants one.
                    if slot.load(Ordering::Relaxed) != token {
                        continue;
                    }
                    // …and after the settle, for the one kind that has one.
                    // Measured from the request, so a job that queued that
                    // long does not wait again.
                    let waited = made.elapsed();
                    if waited < which.settle() {
                        std::thread::sleep(which.settle() - waited);
                        if slot.load(Ordering::Relaxed) != token {
                            continue;
                        }
                    }
                    // …and during it, for the one read that can take long
                    // enough to go stale half-way: a listing gives up between
                    // two reads of the archive (`listing::list`).
                    let body = read(job, &|| slot.load(Ordering::Relaxed) != token);
                    if slot.load(Ordering::Relaxed) != token {
                        continue;
                    }
                    if res_tx.send(body).is_err() {
                        return;
                    }
                    notify();
                }
            });
        let worker = match handle {
            Ok(h) => Some(h),
            // A thread that will not spawn costs these bodies and nothing
            // else; the facts around them are already in hand.
            Err(e) => {
                log::warn!("the {name} worker did not start: {e}");
                None
            }
        };
        Bodies {
            jobs: Some(job_tx),
            results: res_rx,
            live,
            next: [0; 3],
            worker,
        }
    }

    /// Queue a read, retiring whatever was in flight **for that kind of card**.
    pub fn request(&mut self, job: Job) {
        let which = job.which();
        let slot = which.slot();
        self.next[slot] += 1;
        self.live[slot].store(self.next[slot], Ordering::Relaxed);
        if let Some(jobs) = &self.jobs {
            if jobs
                .send((which, self.next[slot], Instant::now(), job))
                .is_err()
            {
                log::debug!("the card-body worker is gone");
            }
        }
    }

    /// Stop caring about whatever is in flight for one kind of card. The other
    /// kinds' reads are not this card's to retire.
    pub fn cancel(&self, which: Which) {
        self.live[which.slot()].store(0, Ordering::Relaxed);
    }

    /// Every kind, for the drop.
    fn cancel_all(&self) {
        self.cancel(Which::Entry);
        self.cancel(Which::Remote);
        self.cancel(Which::Archive);
    }

    pub fn drain(&self) -> Vec<Body> {
        self.results.try_iter().collect()
    }
}

impl Drop for Bodies {
    fn drop(&mut self) {
        self.cancel_all();
        self.jobs = None;
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// Do one job. `stop` says the job has been retired; only the listing asks it,
/// the other two being a mebibyte at most.
fn read(job: Job, stop: &dyn Fn() -> bool) -> Body {
    match job {
        Job::Entry {
            archive,
            inner,
            limit,
        } => {
            let text = df_core::archive::read_entry(&archive, &inner, limit)
                .ok()
                .flatten()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            Body::Entry {
                archive,
                inner,
                text,
            }
        }
        Job::Remote { url, local } => {
            // `None` when the download is not text after all. The size gate
            // happened before the download (`crate::remote::previewable`);
            // this is the second half of the same honesty, because a `.txt`
            // full of bytes is a screen of replacement characters and the
            // facts card is the better answer.
            let text = std::fs::read(&local)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            Body::Remote { url, text }
        }
        Job::Archive { path } => {
            let listing = listing::list(&path, stop);
            Body::Archive { path, listing }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// The listing waits out its settle before it opens anything, and one
    /// retired inside that wait is never read at all: the held-`↓` case, where
    /// the cursor has left the archive before the wait is over.
    #[test]
    fn a_listing_retired_during_its_settle_is_never_read() {
        let tree = df_core::test_support::TempTree::new("body-settle");
        let path = tree.file("x.zip", b"not really a zip");
        let mut bodies = Bodies::start(df_core::fs::no_notifier());

        bodies.request(Job::Archive { path: path.clone() });
        bodies.cancel(Which::Archive);
        std::thread::sleep(df_core::preview::DEBOUNCE * 4);
        assert!(bodies.drain().is_empty(), "a retired listing came back");

        // …and one left alone does come back, a settle later, about its own
        // file.
        let started = Instant::now();
        bodies.request(Job::Archive { path: path.clone() });
        let body = loop {
            if let Some(body) = bodies.drain().pop() {
                break body;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no listing came back"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(started.elapsed() >= df_core::preview::DEBOUNCE);
        let Body::Archive { path: about, .. } = body else {
            panic!("a listing job answered with another kind of body");
        };
        assert_eq!(about, path);
    }
}
