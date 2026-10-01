//! Producing a preview off the event loop, newest-request-wins.
//!
//! The same shape as [`crate::fs::Scanner`], for the same reason: reading a file
//! can block for as long as the disk feels like, and the event loop is never
//! the thread it blocks on (PLAN §1). Requests go in on a channel, results
//! come back on another, and df-core rings a [`Notifier`] the app supplies
//! rather than knowing what the app waits on.
//!
//! ## Newest wins, per pane
//!
//! Holding `↓` through a directory asks for forty previews in a second and
//! wants exactly one of them. So a request is keyed by the **pane** that asked
//! (PLAN §2 has several: a preview pane per tab, and the spot panel), and a
//! new request for a pane retires the pane's previous one on the spot. A
//! retired request is dropped before it opens anything — the cancellation has
//! to bite *before* the work, or a burst of forty still reads forty files and
//! only throws thirty-nine away.
//!
//! There are two checks, deliberately:
//!
//! - before the debounce wait, so a request already superseded while queued
//!   costs nothing at all;
//! - after it, which is the one that matters. [`DEBOUNCE`] is measured from
//!   when the request was *made*, so a request that waited in the queue that
//!   long does not wait again.
//!
//! And a third, cheaper guarantee on top: every result carries the
//! [`PreviewToken`] it answers, so anything that slips through the race is
//! discarded by the model on arrival. Cancellation stops the work; the token
//! stops the paint.
//!
//! ## Caps, everywhere
//!
//! Every read here is bounded before it starts, because the file under the
//! cursor is chosen by the user and might be a 40 GB disk image, a 900 MB
//! minified bundle, or `/dev/zero`. [`TEXT_BYTES`], [`TEXT_LINES`],
//! [`LINE_CHARS`], [`HEX_BYTES`] and [`DIR_ENTRIES`] are those bounds, and
//! every one of them is a *hard* cap with a truncation marker rather than a
//! best-effort. A preview that hangs the pane is worse than a preview that
//! stops early and says so.
//!
//! ## The decode seam
//!
//! Images, video, audio, PDFs, fonts and models are not decoded here: df-core
//! must not link ffmpeg or pdfium (PLAN §1). For those the worker does the
//! cheap half — type, size, and the yazi cache lookup that might already have
//! a thumbnail ([`super::cache`]) — and answers
//! [`Preview::NeedsDecode`]. df-app takes that and calls dv-media. That is the
//! entire contract between the halves, and it is one enum variant.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::fs::{scan_blocking, sort_entries, Entry, Notifier, SortOptions};
use crate::{DfError, Result};

use super::cache;
use super::kind::{kind_for, PreviewKind};
use super::sniff;
use super::syntax::syntax_for;

/// How long a request waits before the expensive part starts.
///
/// The window a burst of cursor movement fits inside. 40 ms is under the
/// ~100 ms at which a person reads a delay as the program being slow, and
/// comfortably over the 8–16 ms between two keys of an autorepeat — so
/// arrowing through a directory reads *one* file rather than one per row, and
/// stopping on a file still feels immediate.
pub const DEBOUNCE: Duration = Duration::from_millis(40);

/// Preview workers.
///
/// Two: one for the pane under the cursor and one for the spot panel or a
/// second tab. A third would only ever be working on a request that has
/// already been superseded — the debounce guarantees at most one live request
/// per pane, and there are not three panes previewing at once.
pub const PREVIEW_WORKERS: usize = 2;

/// The most of a text file to read: 1 MiB.
///
/// Two thousand screenfuls. Nobody scrolls a preview pane that far, and the
/// number is small enough that reading it off a cold spinning disk is
/// bounded — which is the property that matters, since the alternative is a
/// `read_to_string` on a log file that grew to 4 GB overnight.
pub const TEXT_BYTES: usize = 1024 * 1024;

/// The most lines kept from a text file.
///
/// The cap that actually bites first on source: 1 MiB of Rust is roughly
/// 25k lines, and holding them is a few megabytes of `String`. Past this the
/// preview says so and the user opens an editor, which is the right tool for
/// line 20,001 anyway.
pub const TEXT_LINES: usize = 20_000;

/// The most characters kept from one line.
///
/// Minified JavaScript is one line of 900 KB, and a text layout engine asked
/// to shape that will stall for seconds. 2000 is several times the widest
/// pane; what is cut was never going to be visible.
pub const LINE_CHARS: usize = 2000;

/// The most of a binary file to hexdump: 64 KiB.
///
/// 4096 dump rows at 16 bytes each — far past what anyone scrolls, and one
/// bounded read. A hexdump exists to answer "what *is* this", and the answer
/// is always in the first page.
pub const HEX_BYTES: usize = 64 * 1024;

/// The most entries listed in a directory preview.
///
/// A preview is a glance, not a pane you navigate — `→` is how you get the
/// real listing, and that path is [`crate::fs::Scanner`]'s, batched and
/// cancellable. 1000 is well past a screenful and keeps a peek at
/// `/nix/store` from turning into a 200k-entry sort.
pub const DIR_ENTRIES: usize = 1000;

/// Which pane asked. Two requests with the same id supersede each other; two
/// with different ids run side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(pub u64);

/// Identifies one preview request. Monotonic per [`Previewer`], never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreviewToken(pub u64);

/// How big the result should be, in physical pixels.
///
/// df-core never resizes anything — it has no decoder — but the size travels
/// with the request so that the decode df-app runs is sized once, by the pane
/// that asked, instead of decoding full-resolution and throwing the pixels
/// away. Zero means "the caller does not know yet", which is what a request
/// made before the first layout looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TargetSize {
    pub width: u32,
    pub height: u32,
}

impl TargetSize {
    pub fn new(width: u32, height: u32) -> TargetSize {
        TargetSize { width, height }
    }
}

/// A request to preview one file.
#[derive(Debug, Clone)]
pub struct PreviewRequest {
    pub pane: PaneId,
    pub path: PathBuf,
    pub target: TargetSize,
}

/// What the worker produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Nothing in the file.
    Empty,
    /// Lines, already capped. `truncated` is what the pane draws its "…and N
    /// more" marker from — the count is deliberately not carried, because
    /// counting the rest means reading the rest.
    Text {
        lines: Vec<String>,
        syntax: Option<&'static str>,
        /// The file continued past [`TEXT_LINES`] or [`TEXT_BYTES`].
        truncated: bool,
    },
    /// Text that gets rendered rather than highlighted (PLAN §6, "replaces
    /// glow"). Kept as source: the markdown parser lives in df-app with the
    /// fonts it needs.
    Markdown { source: String, truncated: bool },
    /// A one-level listing, sorted the way the list pane sorts.
    Directory {
        entries: Vec<Entry>,
        /// More than [`DIR_ENTRIES`] entries; the rest were not read.
        truncated: bool,
    },
    /// The first [`HEX_BYTES`] of something with no previewer.
    Hex { bytes: Vec<u8>, truncated: bool },
    /// The seam (see the module essay). Everything df-core can cheaply learn
    /// about a file it cannot decode.
    NeedsDecode {
        kind: PreviewKind,
        path: PathBuf,
        target: TargetSize,
        /// A yazi-compatible thumbnail that already exists on disk, if one
        /// does — the placeholder PLAN §6 crossfades away from. `None` is
        /// ordinary; it means the decode has to finish before anything shows.
        thumb: Option<PathBuf>,
    },
    /// There is nothing honest to draw; the opener rules are the answer.
    Unsupported { kind: PreviewKind },
    /// A file whose picture is not in it, described instead: an Affinity
    /// document with no thumbnail ([`super::affinity`]). What it is, and the
    /// facts the list has about it.
    Card {
        kind: PreviewKind,
        /// What a person calls it: "Affinity Designer document".
        what: &'static str,
        name: String,
        len: u64,
        modified: Option<std::time::SystemTime>,
        created: Option<std::time::SystemTime>,
    },
}

/// One update from the worker pool.
#[derive(Debug)]
pub enum PreviewUpdate {
    Ready {
        token: PreviewToken,
        pane: PaneId,
        path: PathBuf,
        preview: Preview,
    },
    /// The file could not be read. Carries the path, because "permission
    /// denied" without a name is not something a person can act on.
    Failed {
        token: PreviewToken,
        pane: PaneId,
        path: PathBuf,
        error: DfError,
    },
}

impl PreviewUpdate {
    pub fn token(&self) -> PreviewToken {
        match self {
            PreviewUpdate::Ready { token, .. } | PreviewUpdate::Failed { token, .. } => *token,
        }
    }

    pub fn pane(&self) -> PaneId {
        match self {
            PreviewUpdate::Ready { pane, .. } | PreviewUpdate::Failed { pane, .. } => *pane,
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            PreviewUpdate::Ready { path, .. } | PreviewUpdate::Failed { path, .. } => path,
        }
    }
}

struct Job {
    token: PreviewToken,
    request: PreviewRequest,
    /// The sort the directory preview is listed in, copied at request time so
    /// that changing it (`,` in the list pane) affects the next preview rather
    /// than the one already in flight.
    sort: SortOptions,
    /// When the request was made, not when a worker picked it up — the
    /// debounce is a promise to the user about latency, and a job that queued
    /// for 40 ms has already kept it.
    made: Instant,
}

/// The live request per pane. One entry each, by construction: inserting is
/// how a request supersedes its predecessor.
type Live = Arc<Mutex<HashMap<PaneId, PreviewToken>>>;

/// The worker pool.
///
/// Dropping it closes the request channel; workers finish the job they are on
/// and exit, and the drop joins them so a test cannot outlive its threads.
pub struct Previewer {
    requests: Option<Sender<Job>>,
    updates: Receiver<PreviewUpdate>,
    live: Live,
    next_token: AtomicU64,
    debounce: Duration,
    sort: SortOptions,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Previewer {
    /// Start `workers` threads. `notify` is rung once per update sent.
    pub fn new(workers: usize, notify: Notifier) -> Previewer {
        Previewer::with_debounce(workers, notify, DEBOUNCE)
    }

    /// The default pool.
    pub fn start(notify: Notifier) -> Previewer {
        Previewer::new(PREVIEW_WORKERS, notify)
    }

    /// A pool with a custom debounce. Tests use zero so they do not sleep;
    /// nothing else should need it.
    pub fn with_debounce(workers: usize, notify: Notifier, debounce: Duration) -> Previewer {
        let (req_tx, req_rx) = unbounded::<Job>();
        let (up_tx, up_rx) = unbounded::<PreviewUpdate>();
        let live: Live = Arc::new(Mutex::new(HashMap::new()));
        let sort = SortOptions::default();

        let mut handles = Vec::with_capacity(workers);
        for i in 0..workers.max(1) {
            let req_rx = req_rx.clone();
            let up_tx = up_tx.clone();
            let live = Arc::clone(&live);
            let notify = Arc::clone(&notify);
            let handle = std::thread::Builder::new()
                .name(format!("df-preview-{i}"))
                .spawn(move || {
                    // Milder than the bulk pool: somebody is looking at the
                    // pane waiting for this, so it gives way to the paint
                    // thread and to nothing else (`df_core::thread`).
                    crate::thread::lower_priority(crate::thread::NICE_INTERACTIVE);
                    for job in req_rx {
                        run_job(job, &up_tx, &live, &notify, debounce);
                    }
                });
            match handle {
                Ok(h) => handles.push(h),
                // A thread that will not spawn is not fatal: the file list
                // still works, the preview pane stays empty. Better a file
                // manager without previews than no file manager.
                Err(e) => log::warn!("preview worker {i} did not start: {e}"),
            }
        }

        Previewer {
            requests: Some(req_tx),
            updates: up_rx,
            live,
            next_token: AtomicU64::new(1),
            debounce,
            sort,
            workers: handles,
        }
    }

    /// The sort a directory preview is listed in. Defaults to the config's;
    /// df-app sets it so a peek matches what entering the directory shows.
    /// Applies to requests made after it, not to one already in flight — the
    /// listing you are looking at does not reorder under you.
    pub fn set_sort(&mut self, sort: SortOptions) {
        self.sort = sort;
    }

    /// Queue a preview and return its token. Any in-flight request from the
    /// same pane is retired first, whether or not a worker has picked it up.
    pub fn request(&self, request: PreviewRequest) -> PreviewToken {
        let pane = request.pane;
        let token = PreviewToken(self.next_token.fetch_add(1, Ordering::Relaxed));
        lock(&self.live).insert(pane, token);
        if let Some(requests) = &self.requests {
            let job = Job {
                token,
                request,
                sort: self.sort,
                made: Instant::now(),
            };
            // Unbounded channel, receivers alive for the pool's lifetime: a
            // send failure means the pool is gone.
            if requests.send(job).is_err() {
                let mut live = lock(&self.live);
                // Only retire *this* token: a newer request for the same pane
                // may have landed in between, and it is still wanted.
                if live.get(&pane) == Some(&token) {
                    live.remove(&pane);
                }
            }
        }
        token
    }

    /// Shorthand for the common request.
    pub fn preview(
        &self,
        pane: PaneId,
        path: impl Into<PathBuf>,
        target: TargetSize,
    ) -> PreviewToken {
        self.request(PreviewRequest {
            pane,
            path: path.into(),
            target,
        })
    }

    /// Stop whatever this pane asked for — closing a tab, hiding the preview.
    pub fn cancel(&self, pane: PaneId) {
        lock(&self.live).remove(&pane);
    }

    pub fn cancel_all(&self) {
        lock(&self.live).clear();
    }

    /// Whether this request is still the live one for its pane.
    pub fn is_live(&self, token: PreviewToken) -> bool {
        lock(&self.live).values().any(|t| *t == token)
    }

    /// The debounce this pool was built with.
    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    /// The channel, for a caller that wants to select on it.
    pub fn updates(&self) -> &Receiver<PreviewUpdate> {
        &self.updates
    }

    /// Everything that has arrived, without blocking.
    pub fn drain(&self) -> Vec<PreviewUpdate> {
        self.updates.try_iter().collect()
    }
}

impl Drop for Previewer {
    fn drop(&mut self) {
        self.cancel_all();
        self.requests = None;
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Lock without ever panicking on a poisoned mutex — see
/// [`crate::fs::Scanner`]'s note; the same argument applies to the same map.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_live(live: &Live, pane: PaneId, token: PreviewToken) -> bool {
    lock(live).get(&pane) == Some(&token)
}

fn run_job(
    job: Job,
    updates: &Sender<PreviewUpdate>,
    live: &Live,
    notify: &Notifier,
    debounce: Duration,
) {
    let Job {
        token,
        request,
        sort,
        made,
    } = job;
    let pane = request.pane;

    // Superseded while it sat in the queue: the whole burst lands here, and
    // lands here having opened nothing.
    if !is_live(live, pane, token) {
        return;
    }

    // The debounce, measured from when the user's keypress produced the
    // request rather than from now.
    let waited = made.elapsed();
    if waited < debounce {
        std::thread::sleep(debounce - waited);
    }
    if !is_live(live, pane, token) {
        return;
    }

    let result = build(&request, &sort);
    // One last look before spending a channel slot and a repaint on it.
    if !is_live(live, pane, token) {
        return;
    }

    let update = match result {
        Ok(preview) => PreviewUpdate::Ready {
            token,
            pane,
            path: request.path,
            preview,
        },
        Err(error) => PreviewUpdate::Failed {
            token,
            pane,
            path: request.path,
            error,
        },
    };
    if updates.send(update).is_ok() {
        notify();
    }
}

/// The whole of the work, on the calling thread.
///
/// Public because it is the honest way to test the caps and the type decisions
/// without threads, and because df-app's synchronous first paint wants it.
pub fn build(request: &PreviewRequest, sort: &SortOptions) -> Result<Preview> {
    let path = &request.path;
    let entry = Entry::read(path)?;
    let hint = entry.mime;

    if entry.is_dir() {
        return directory_preview(path, sort);
    }

    // One read serves both the sniffer and, for text, the preview itself: the
    // 8 KiB the sniffer needs is the first 8 KiB the previewer would read.
    let head = read_head(path, sniff::SNIFF_BYTES)?;
    let mime = sniff::sniff_or_hint(&head, hint);
    let kind = kind_for(&entry, mime);

    match kind {
        PreviewKind::Directory => directory_preview(path, sort),
        PreviewKind::Empty => Ok(Preview::Empty),
        PreviewKind::Text { syntax } => {
            let syntax = syntax.or_else(|| syntax_for(&entry.name, &head));
            let (source, truncated) = read_text(path, &head)?;
            let (lines, more) = split_lines(&source);
            Ok(Preview::Text {
                lines,
                syntax,
                truncated: truncated || more,
            })
        }
        PreviewKind::Markdown => {
            let (source, truncated) = read_text(path, &head)?;
            Ok(Preview::Markdown { source, truncated })
        }
        PreviewKind::Binary => {
            let bytes = read_head(path, HEX_BYTES)?;
            let truncated = entry.len > bytes.len() as u64;
            Ok(Preview::Hex { bytes, truncated })
        }
        PreviewKind::Unsupported | PreviewKind::Denied => Ok(Preview::Unsupported { kind }),
        // Its thumbnail is its picture, decoded in df-app like any other;
        // with none in it, it is described, never dumped as hex.
        PreviewKind::Affinity if !matches!(super::affinity::thumbnail(path), Ok(Some(_))) => {
            Ok(Preview::Card {
                kind,
                what: super::affinity::what(&entry.name),
                name: entry.name.clone(),
                len: entry.len,
                modified: entry.mtime,
                created: entry.btime,
            })
        }
        // Images, video, audio, PDFs, fonts and models — and archives and
        // G-code, which need a reader df-core does not have either. All of
        // them leave through the same seam, and df-app answers them.
        _ => {
            let thumb = if kind.thumbnailable() {
                cache::cached_thumb(path)
            } else {
                None
            };
            Ok(Preview::NeedsDecode {
                kind,
                path: path.clone(),
                target: request.target,
                thumb,
            })
        }
    }
}

fn directory_preview(path: &Path, sort: &SortOptions) -> Result<Preview> {
    let mut entries = scan_blocking(path)?;
    let truncated = entries.len() > DIR_ENTRIES;
    sort_entries(&mut entries, sort);
    entries.truncate(DIR_ENTRIES);
    Ok(Preview::Directory { entries, truncated })
}

/// Read at most `limit` bytes from the front of a file.
///
/// The cap is applied to the *read*, not to the result: a `read_to_end`
/// followed by a truncate would still have pulled 40 GB through the page
/// cache first, which is the mistake this function exists to not make.
fn read_head(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|e| DfError::io(path, e))?;
    let mut reader = file.take(limit as u64);
    let mut buf = Vec::with_capacity(limit.min(64 * 1024));
    reader
        .read_to_end(&mut buf)
        .map_err(|e| DfError::io(path, e))?;
    Ok(buf)
}

/// The text of a file, capped at [`TEXT_BYTES`], reusing the sniffed head when
/// that is already the whole of it.
///
/// Invalid UTF-8 is replaced rather than refused: by the time this is called
/// the sniffer has said the file is text, and one bad byte in a 3000-line log
/// must not blank the pane. The tail is trimmed back to a character boundary
/// so the cap never manufactures a replacement character of its own.
fn read_text(path: &Path, head: &[u8]) -> Result<(String, bool)> {
    let bytes = if head.len() < sniff::SNIFF_BYTES {
        head.to_vec()
    } else {
        read_head(path, TEXT_BYTES + 1)?
    };
    let truncated = bytes.len() > TEXT_BYTES;
    let mut end = bytes.len().min(TEXT_BYTES);
    if truncated {
        // Back off to a UTF-8 boundary: continuation bytes are `10xxxxxx`.
        while end > 0 && (bytes[end] & 0xc0) == 0x80 {
            end -= 1;
        }
    }
    let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    // A BOM is metadata, not the first character of the document.
    let text = text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text);
    Ok((text, truncated))
}

/// Split into at most [`TEXT_LINES`] lines of at most [`LINE_CHARS`]
/// characters, reporting whether anything was left behind.
///
/// Splitting is on `\n` with a trailing `\r` trimmed, so a CRLF file does not
/// draw a stray glyph at the end of every line.
fn split_lines(text: &str) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut truncated = false;
    for line in text.split('\n') {
        if lines.len() >= TEXT_LINES {
            truncated = true;
            break;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.chars().count() > LINE_CHARS {
            let clipped: String = line.chars().take(LINE_CHARS).collect();
            lines.push(clipped);
            truncated = true;
        } else {
            lines.push(line.to_string());
        }
    }
    // A file ending in a newline splits into a trailing empty string, which is
    // not a line anybody wrote.
    if lines.last().is_some_and(|l| l.is_empty()) && text.ends_with('\n') {
        lines.pop();
    }
    (lines, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::no_notifier;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "df-preview-job-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn write(name: &str, contents: &[u8]) -> PathBuf {
        let path = dir().join(name);
        std::fs::write(&path, contents).expect("write fixture");
        path
    }

    fn request(path: &Path) -> PreviewRequest {
        PreviewRequest {
            pane: PaneId(0),
            path: path.to_path_buf(),
            target: TargetSize::new(800, 600),
        }
    }

    fn build_one(path: &Path) -> Preview {
        build(&request(path), &SortOptions::default()).expect("build")
    }

    /// Wait for updates without a sleep loop that can hang a test run.
    fn collect(p: &Previewer, count: usize) -> Vec<PreviewUpdate> {
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while out.len() < count && Instant::now() < deadline {
            match p.updates().recv_timeout(Duration::from_millis(50)) {
                Ok(u) => out.push(u),
                Err(_) => continue,
            }
        }
        out
    }

    #[test]
    fn text_comes_back_as_lines_with_its_language() {
        let path = write("hello.rs", b"fn main() {\r\n    println!(\"hi\");\n}\n");
        match build_one(&path) {
            Preview::Text {
                lines,
                syntax,
                truncated,
            } => {
                assert_eq!(lines, ["fn main() {", "    println!(\"hi\");", "}"]);
                assert_eq!(syntax, Some("Rust"));
                assert!(!truncated);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn a_script_with_no_extension_is_still_text() {
        let path = write("deploy", b"#!/usr/bin/env bash\nset -eu\n");
        match build_one(&path) {
            Preview::Text { syntax, .. } => assert_eq!(syntax, Some("Shell")),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn the_text_cap_holds_and_says_so() {
        // Two lines longer than the byte cap between them.
        let mut big = vec![b'a'; TEXT_BYTES];
        big.extend_from_slice(b"\ntail\n");
        let path = write("big.txt", &big);
        match build_one(&path) {
            Preview::Text {
                lines, truncated, ..
            } => {
                assert!(truncated, "a file past the cap must say so");
                assert!(lines.len() <= TEXT_LINES);
                for line in &lines {
                    assert!(
                        line.chars().count() <= LINE_CHARS,
                        "a line of {} chars got through",
                        line.chars().count()
                    );
                }
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn the_line_count_cap_holds() {
        let many: String = std::iter::repeat_n("line\n", TEXT_LINES + 500).collect();
        let path = write("many.txt", many.as_bytes());
        match build_one(&path) {
            Preview::Text {
                lines, truncated, ..
            } => {
                assert_eq!(lines.len(), TEXT_LINES);
                assert!(truncated);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn the_hexdump_cap_holds() {
        let mut blob = vec![0u8; HEX_BYTES * 2];
        blob[..4].copy_from_slice(b"\x7fELF");
        let path = write("a.out", &blob);
        match build_one(&path) {
            Preview::Hex { bytes, truncated } => {
                assert_eq!(bytes.len(), HEX_BYTES);
                assert!(truncated);
            }
            other => panic!("expected a hexdump, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_file_is_empty_not_denied() {
        let path = write("nothing.txt", b"");
        assert_eq!(build_one(&path), Preview::Empty);
    }

    #[test]
    fn a_directory_lists_one_level() {
        let root = dir().join("listing");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("b.txt"), b"b").expect("write");
        std::fs::write(root.join("a.txt"), b"a").expect("write");

        match build_one(&root) {
            Preview::Directory { entries, truncated } => {
                assert!(!truncated);
                let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
                // dir_first, then alphabetical: the list pane's own order.
                assert_eq!(names, ["sub", "a.txt", "b.txt"]);
            }
            other => panic!("expected a listing, got {other:?}"),
        }
    }

    #[test]
    fn an_image_becomes_a_decode_request_carrying_the_target_size() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.resize(1024, 0);
        let path = write("photo.png", &png);
        match build_one(&path) {
            Preview::NeedsDecode {
                kind,
                target,
                path: p,
                ..
            } => {
                assert_eq!(kind, PreviewKind::Image);
                assert_eq!(target, TargetSize::new(800, 600));
                assert_eq!(p, path);
            }
            other => panic!("expected a decode request, got {other:?}"),
        }
    }

    /// An Affinity document is a picture when it carries its thumbnail and
    /// a card that says what it is when it does not — never a hexdump, which
    /// is what its bytes were before it had a kind (W4.45).
    #[test]
    fn an_affinity_file_is_its_thumbnail_or_its_card() {
        use crate::preview::affinity::tests::{affinity, noise};
        let (with, _, _) = affinity(true);
        let path = write("Logo.afdesign", &with);
        match build_one(&path) {
            Preview::NeedsDecode { kind, thumb, .. } => {
                assert_eq!(kind, PreviewKind::Affinity);
                assert_eq!(thumb, None, "no yazi thumbnail is looked for");
            }
            other => panic!("expected a decode request, got {other:?}"),
        }
        // Named for nothing Affinity: the signature says what it is.
        let path = write("mystery.bin", &with);
        assert!(matches!(
            build_one(&path),
            Preview::NeedsDecode {
                kind: PreviewKind::Affinity,
                ..
            }
        ));
        let without = [crate::preview::affinity::MAGIC, &noise(20_000, 9)].concat();
        let path = write("Cover v1.af", &without);
        match build_one(&path) {
            Preview::Card {
                kind,
                what,
                name,
                len,
                modified,
                ..
            } => {
                assert_eq!(kind, PreviewKind::Affinity);
                assert_eq!(what, "Affinity document");
                assert_eq!(name, "Cover v1.af");
                assert_eq!(len, without.len() as u64);
                assert!(modified.is_some());
            }
            other => panic!("expected a card, got {other:?}"),
        }
    }

    #[test]
    fn bytes_beat_the_extension_here_too() {
        // A JPEG named `.txt` must not reach the text previewer.
        let mut jpeg = b"\xff\xd8\xff\xe0".to_vec();
        jpeg.resize(512, 0);
        let path = write("lying.txt", &jpeg);
        assert!(matches!(build_one(&path), Preview::NeedsDecode { .. }));
    }

    #[test]
    fn a_missing_file_is_an_error_that_names_it() {
        let path = dir().join("does-not-exist");
        let _ = std::fs::remove_file(&path);
        let err = build(&request(&path), &SortOptions::default()).expect_err("must fail");
        assert!(err.to_string().contains("does-not-exist"), "{err}");
    }

    #[test]
    fn the_worker_answers_and_rings_the_bell() {
        let rung = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&rung);
        let notify: Notifier = Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let p = Previewer::with_debounce(1, notify, Duration::ZERO);

        let path = write("worker.txt", b"one\ntwo\n");
        let token = p.preview(PaneId(1), &path, TargetSize::default());

        let updates = collect(&p, 1);
        assert_eq!(updates.len(), 1, "expected one update");
        assert_eq!(updates[0].token(), token);
        assert_eq!(updates[0].pane(), PaneId(1));
        // The worker rings after it sends, so the update can be in hand a
        // moment before the bell has rung.
        let rung_by = Instant::now() + Duration::from_secs(2);
        while rung.load(Ordering::Relaxed) == 0 && Instant::now() < rung_by {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            rung.load(Ordering::Relaxed) >= 1,
            "the notifier never fired"
        );
    }

    #[test]
    fn only_the_newest_request_per_pane_survives() {
        let p = Previewer::with_debounce(1, no_notifier(), Duration::from_millis(20));

        let a = write("burst-a.txt", b"a\n");
        let b = write("burst-b.txt", b"b\n");
        let c = write("burst-c.txt", b"c\n");

        // A burst, the way autorepeat produces one.
        let _t1 = p.preview(PaneId(7), &a, TargetSize::default());
        let _t2 = p.preview(PaneId(7), &b, TargetSize::default());
        let t3 = p.preview(PaneId(7), &c, TargetSize::default());

        assert!(!p.is_live(_t1), "an superseded request must be retired");
        assert!(!p.is_live(_t2));
        assert!(p.is_live(t3));

        let updates = collect(&p, 1);
        assert_eq!(updates.len(), 1, "only the newest request may answer");
        assert_eq!(updates[0].token(), t3);
        assert_eq!(updates[0].path(), c);
    }

    #[test]
    fn different_panes_do_not_supersede_each_other() {
        let p = Previewer::with_debounce(2, no_notifier(), Duration::ZERO);
        let a = write("pane-a.txt", b"a\n");
        let b = write("pane-b.txt", b"b\n");

        let ta = p.preview(PaneId(1), &a, TargetSize::default());
        let tb = p.preview(PaneId(2), &b, TargetSize::default());
        assert!(p.is_live(ta) && p.is_live(tb));

        let updates = collect(&p, 2);
        assert_eq!(updates.len(), 2, "both panes must be answered");
    }

    #[test]
    fn cancelling_a_pane_silences_it() {
        let p = Previewer::with_debounce(1, no_notifier(), Duration::from_millis(50));
        let path = write("cancelled.txt", b"x\n");
        let token = p.preview(PaneId(3), &path, TargetSize::default());
        p.cancel(PaneId(3));
        assert!(!p.is_live(token));

        // Nothing may arrive; give the worker time to prove it.
        std::thread::sleep(Duration::from_millis(200));
        assert!(p.drain().is_empty(), "a cancelled request answered anyway");
    }

    #[test]
    fn the_debounce_is_measured_from_the_request() {
        let p = Previewer::with_debounce(1, no_notifier(), Duration::from_millis(60));
        let path = write("debounced.txt", b"x\n");
        let start = Instant::now();
        p.preview(PaneId(4), &path, TargetSize::default());
        let updates = collect(&p, 1);
        assert_eq!(updates.len(), 1);
        assert!(
            start.elapsed() >= Duration::from_millis(55),
            "the answer arrived before the debounce elapsed"
        );
    }
}
