//! The *card* bodies, off the paint thread.
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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;

/// Which card a job belongs to.
///
/// **The two are retired independently, and that is the whole point of this
/// type.** One `AtomicU64` for both meant an archive card cancelling its body
/// also retired the remote card's in-flight read — and the remote side's
/// "already showing this url" early return then never asked again, so the card
/// sat with an empty body until the cursor left the row and came back. Two
/// counters, one per kind, because they are two independent newest-wins races
/// that happen to share a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Archive,
    Remote,
}

impl Which {
    fn slot(self) -> usize {
        match self {
            Which::Archive => 0,
            Which::Remote => 1,
        }
    }
}

/// What to read.
pub enum Job {
    /// An entry inside an archive the tab is browsing.
    Archive {
        archive: PathBuf,
        /// The entry's path *inside* the container.
        inner: String,
        /// `df_core::archive::read_entry`'s cap.
        limit: usize,
    },
    /// A remote row whose bytes are already on the disk as a temp file.
    Remote { url: String, local: PathBuf },
}

impl Job {
    fn which(&self) -> Which {
        match self {
            Job::Archive { .. } => Which::Archive,
            Job::Remote { .. } => Which::Remote,
        }
    }
}

/// What came back. Carries its own subject, so the app can check the answer is
/// still about the row under the cursor without trusting the token alone.
pub enum Body {
    Archive {
        archive: PathBuf,
        inner: String,
        text: Option<String>,
    },
    Remote {
        url: String,
        text: Option<String>,
    },
}

/// The card-body worker.
///
/// Dropping it closes the channel; the worker finishes the job it is on and
/// exits, and the drop joins it so nothing outlives the window.
pub struct Bodies {
    jobs: Option<Sender<(Which, u64, Job)>>,
    results: Receiver<Body>,
    /// One counter per [`Which`] — see that type for why they are not one.
    live: Arc<[AtomicU64; 2]>,
    next: [u64; 2],
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Bodies {
    pub fn start(notify: Notifier) -> Bodies {
        let (job_tx, job_rx) = unbounded::<(Which, u64, Job)>();
        let (res_tx, res_rx) = unbounded::<Body>();
        let live = Arc::new([AtomicU64::new(0), AtomicU64::new(0)]);
        let worker_live = Arc::clone(&live);
        let handle = std::thread::Builder::new()
            .name("df-card-body".to_string())
            .spawn(move || {
                df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                for (which, token, job) in job_rx {
                    let slot = &worker_live[which.slot()];
                    // Checked before the read and again before the send: a
                    // held `↓` through a zip asks for forty and wants one.
                    if slot.load(Ordering::Relaxed) != token {
                        continue;
                    }
                    let body = read(job);
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
            // A thread that will not spawn costs the card bodies and nothing
            // else; the facts above them are already in hand.
            Err(e) => {
                log::warn!("the card-body worker did not start: {e}");
                None
            }
        };
        Bodies {
            jobs: Some(job_tx),
            results: res_rx,
            live,
            next: [0, 0],
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
            if jobs.send((which, self.next[slot], job)).is_err() {
                log::debug!("the card-body worker is gone");
            }
        }
    }

    /// Stop caring about whatever is in flight for one kind of card. The other
    /// kind's read is not this card's to retire.
    pub fn cancel(&self, which: Which) {
        self.live[which.slot()].store(0, Ordering::Relaxed);
    }

    /// Both, for the drop.
    fn cancel_all(&self) {
        self.cancel(Which::Archive);
        self.cancel(Which::Remote);
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

fn read(job: Job) -> Body {
    match job {
        Job::Archive {
            archive,
            inner,
            limit,
        } => {
            let text = df_core::archive::read_entry(&archive, &inner, limit)
                .ok()
                .flatten()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            Body::Archive {
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
    }
}
