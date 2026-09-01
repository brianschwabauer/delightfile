//! The *other* half of PLAN §6's seam: turning text into something the paint
//! thread can draw without thinking.
//!
//! df-core reads the file off the paint thread and hands back lines. What it
//! cannot hand back is the two things df-app has to do to those lines before
//! anything can be drawn, because both live in df-app with the fonts and the
//! palette they need:
//!
//! - [`crate::preview::highlight::block_states`] — one pass over up to 20 000
//!   lines working out what comment or string each one *starts inside*, so
//!   that scrolling later costs a screenful of tokenising instead of a file's
//!   worth. Cheap per line, and 20 000 of them is not cheap.
//! - [`crate::preview::markdown::parse`] — up to a mebibyte of source into
//!   blocks.
//!
//! Both used to run inside `Pane::body_for`, which runs inside `poll`, which
//! runs inside a frame. A held `↓` down a directory of source files therefore
//! paid for a full parse *in the middle of an animation frame*, once per file,
//! which is exactly the jank a debounced off-thread preview pipeline was built
//! to avoid — moved one seam later.
//!
//! So it is a worker, in the shape every other worker in this program has: one
//! thread, a channel, the same [`crate::Wake`] bell, and newest-wins by the
//! `AtomicU64` token [`crate::preview::decode`] uses. The pane keeps showing
//! the file it was showing until the finished body arrives — the same thing it
//! already does for the several hundred milliseconds df-core spends reading —
//! so nothing about the pane's behaviour changes except where the work happens.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;
use df_core::preview::PreviewToken;

use super::{highlight, markdown};

/// What the pane hands over: exactly what df-core sent, unchanged.
pub enum Source {
    Text {
        lines: Vec<String>,
        syntax: Option<&'static str>,
        truncated: bool,
    },
    Markdown {
        source: String,
        truncated: bool,
    },
}

/// What comes back: exactly what the pane's `Body` needs, finished.
pub enum Ready {
    Text {
        lines: Vec<String>,
        syntax: Option<&'static str>,
        truncated: bool,
        states: Vec<highlight::Block>,
    },
    Markdown {
        blocks: Vec<markdown::Block>,
        truncated: bool,
    },
}

pub struct Job {
    pub token: PreviewToken,
    pub source: Source,
}

pub struct Prepared {
    pub token: PreviewToken,
    pub ready: Ready,
}

/// The prepare worker.
///
/// Dropping it closes the channel; the worker finishes the job it is on and
/// exits, and the drop joins it so nothing outlives the window.
pub struct Preparer {
    jobs: Option<Sender<Job>>,
    results: Receiver<Prepared>,
    live: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Preparer {
    /// Start the worker. `notify` is rung once per result.
    pub fn start(notify: Notifier) -> Preparer {
        let (job_tx, job_rx) = unbounded::<Job>();
        let (res_tx, res_rx) = unbounded::<Prepared>();
        let live = Arc::new(AtomicU64::new(0));
        let worker_live = Arc::clone(&live);
        let handle = std::thread::Builder::new()
            .name("df-prepare".to_string())
            .spawn(move || {
                // Somebody is watching the pane for this; it gives way to the
                // paint thread and to nothing else (`df_core::thread`).
                df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                for job in job_rx {
                    let Job { token, source } = job;
                    // Checked before the work and again before the send: a
                    // held `↓` asks for forty of these and wants one.
                    if worker_live.load(Ordering::Relaxed) != token.0 {
                        continue;
                    }
                    let ready = prepare(source);
                    if worker_live.load(Ordering::Relaxed) != token.0 {
                        continue;
                    }
                    if res_tx.send(Prepared { token, ready }).is_err() {
                        return;
                    }
                    notify();
                }
            });
        let worker = match handle {
            Ok(h) => Some(h),
            // A thread that will not spawn costs the text previews and nothing
            // else, exactly as the decode worker's failure costs the pictures.
            Err(e) => {
                log::warn!("the prepare worker did not start: {e}");
                None
            }
        };
        Preparer {
            jobs: Some(job_tx),
            results: res_rx,
            live,
            worker,
        }
    }

    /// Queue a preparation, retiring whatever was in flight.
    pub fn request(&self, job: Job) {
        self.live.store(job.token.0, Ordering::Relaxed);
        if let Some(jobs) = &self.jobs {
            if jobs.send(job).is_err() {
                log::debug!("the prepare worker is gone");
            }
        }
    }

    /// Stop caring about whatever is in flight.
    pub fn cancel(&self) {
        self.live.store(0, Ordering::Relaxed);
    }

    pub fn drain(&self) -> Vec<Prepared> {
        self.results.try_iter().collect()
    }
}

impl Drop for Preparer {
    fn drop(&mut self) {
        self.cancel();
        self.jobs = None;
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// The work itself, pure and off on its own so a test can time it without a
/// thread (see this module's tests).
pub fn prepare(source: Source) -> Ready {
    match source {
        Source::Text {
            lines,
            syntax,
            truncated,
        } => {
            let profile = highlight::profile_for(syntax);
            let states = highlight::block_states(&lines, profile);
            Ready::Text {
                lines,
                syntax,
                truncated,
                states,
            }
        }
        Source::Markdown { source, truncated } => Ready::Markdown {
            blocks: markdown::parse(&source),
            truncated,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measurement this module exists for: how long the UI thread used to
    /// spend on one arrival of a df-core-sized text preview (20 000 lines is
    /// `df_core::preview::TEXT_LINES`).
    ///
    /// Asserted only against an absurd ceiling — a timing assertion tight
    /// enough to be interesting is an assertion that fails on a loaded CI box
    /// — but the number is printed, which is what makes it useful with
    /// `cargo test -- --nocapture`.
    #[test]
    fn a_full_text_preview_is_measurably_expensive() {
        let lines: Vec<String> = (0..df_core::preview::TEXT_LINES)
            .map(|i| match i % 5 {
                0 => format!("// line {i}, a comment with \"a string\" in it"),
                1 => format!("fn thing_{i}(x: u32) -> u32 {{ x + {i} }}"),
                2 => "    /* an opened block comment".to_string(),
                3 => "       still inside it */ let s = \"text\";".to_string(),
                _ => format!("    let value_{i} = 0x{i:x}; // trailing"),
            })
            .collect();
        let start = std::time::Instant::now();
        let ready = prepare(Source::Text {
            lines,
            syntax: Some("Rust"),
            truncated: true,
        });
        let took = start.elapsed();
        let Ready::Text { states, .. } = ready else {
            panic!("a text source came back as something else");
        };
        assert_eq!(states.len(), df_core::preview::TEXT_LINES);
        println!("block_states over {} lines: {took:?}", states.len());
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }

    /// The same for markdown, at df-core's byte cap.
    #[test]
    fn a_full_markdown_preview_is_measurably_expensive() {
        let mut source = String::new();
        while source.len() < df_core::preview::TEXT_BYTES {
            source.push_str("# A heading\n\nSome *emphasised* text with `code` in it.\n\n- a\n- b\n\n```rust\nfn main() {}\n```\n\n");
        }
        let start = std::time::Instant::now();
        let ready = prepare(Source::Markdown {
            source,
            truncated: true,
        });
        let took = start.elapsed();
        let Ready::Markdown { blocks, .. } = ready else {
            panic!("a markdown source came back as something else");
        };
        println!("markdown::parse into {} blocks: {took:?}", blocks.len());
        assert!(!blocks.is_empty());
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }

    /// Cancellation is the token, and the token is checked twice. This is the
    /// end-to-end version: a superseded request produces nothing the pane will
    /// accept.
    #[test]
    fn the_newest_request_is_the_only_one_that_lands() {
        let preparer = Preparer::start(Arc::new(|| {}));
        for n in 1..=8u64 {
            preparer.request(Job {
                token: PreviewToken(n),
                source: Source::Text {
                    lines: vec![format!("line {n}")],
                    syntax: Some("Rust"),
                    truncated: false,
                },
            });
        }
        // Drain until the last one has been seen, then assert nothing that
        // came out claims to be anything else.
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !seen.contains(&8) {
            for prepared in preparer.drain() {
                seen.push(prepared.token.0);
            }
            std::thread::yield_now();
        }
        assert!(
            seen.contains(&8),
            "the live request never arrived: {seen:?}"
        );
        // Everything that did come through was live when it was sent; the
        // pane's own token check drops the rest, and this is the *worker's*
        // half — it never sends a token past the live one.
        assert!(seen.iter().all(|t| *t <= 8), "{seen:?}");
    }
}
