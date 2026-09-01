//! `s` and `S`: fd and rg, streamed into a native overlay (PLAN §7.2).
//!
//! The two tools are excellent and rewriting either would be absurd, so
//! delightfile shells out to them — but it does **not** hand the terminal over
//! to fzf. The results arrive on a channel, land in a list you arrow through,
//! and the preview pane follows the highlighted hit as you go. That is the
//! whole difference between a file manager that has search and a file manager
//! that launches one.
//!
//! ## The shape of a running search
//!
//! One process at a time, killed and respawned when the query changes. Three
//! defences, because a search box is a machine for asking a process to do
//! something enormous:
//!
//! 1. **A debounce** ([`DEBOUNCE`]). Typing `delight` is seven queries, six of
//!    which are wrong and one of which is a recursive walk of `$HOME`.
//! 2. **A cap** ([`MAX_HITS`]). Past a couple of thousand rows nobody is
//!    reading the list, and the process is charged for the privilege of
//!    producing them — so it is killed and the list says it was truncated.
//! 3. **A kill on respawn.** The old process is not merely ignored; it is
//!    killed, because an ignored `rg` over a home directory keeps a core busy
//!    for the length of the walk.
//!
//! ## Wiring
//!
//! The reader is a thread feeding a crossbeam channel and ringing the
//! [`Notifier`], which is the same worker→`Wake` pattern the scanner, the
//! previewer and the decoder use (PLAN §1). Nothing polls.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::{Entry, Notifier, Span};
use df_core::input::InputBuffer;

/// How long the query has to stand still before a process is spawned for it.
///
/// 150 ms. Under about 120 ms a fast typist still spawns one process per
/// keystroke, which is the thing this exists to stop; over about 200 ms the
/// list visibly lags the field and the overlay feels like it is thinking rather
/// than searching. It is also comfortably longer than the 40 ms the preview
/// pane debounces by, which is right: a preview is one file and a search is a
/// filesystem walk.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// The most hits kept before the process is killed and the list says so.
///
/// Two thousand. It is far past what anyone scrolls — the useful answer is
/// always in the first screen or the query was wrong — and it bounds the memory
/// a runaway search can take to a few hundred kilobytes of paths. The list
/// tells you it was truncated rather than quietly lying about the count.
pub const MAX_HITS: usize = 2000;

/// The longest line rg is asked to print, in columns.
///
/// PLAN §7.2's number. A minified bundle or a base64 blob has lines megabytes
/// long, and one of them in the results would be a row the painter spends a
/// millisecond laying out and nobody can read. rg replaces anything longer with
/// a note saying it did.
const MAX_COLUMNS: usize = 200;

/// How many hits accumulate before the reader thread sends a batch.
///
/// A batch per line would be a channel send and a window wakeup per result,
/// which on a fast search is thousands of frames nobody asked for. 64 is about
/// five rows more than a screenful, so the first batch fills the visible list in
/// one go and the rest arrive as fast as the eye could use them anyway.
const BATCH: usize = 64;

/// How long a partial batch waits before being sent anyway.
///
/// Without it, a search that finds three things would hold them until EOF. 30 ms
/// is under two frames — the results are on screen before anybody could notice
/// they were not.
const BATCH_LINGER: Duration = Duration::from_millis(30);

/// Which of the two searches this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `s` — filenames, via `fd`.
    Names,
    /// `S` — file contents, via `rg`.
    Content,
}

impl Mode {
    pub fn title(self) -> &'static str {
        match self {
            Mode::Names => "Search by name",
            Mode::Content => "Search in files",
        }
    }

    /// The binary this mode needs, for the message shown when it is missing.
    pub fn binary(self) -> &'static str {
        match self {
            Mode::Names => "fd",
            Mode::Content => "rg",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            Mode::Names => "Type a name…",
            Mode::Content => "Type a pattern…",
        }
    }
}

/// One result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    /// The path as the overlay prints it: relative to where the search started,
    /// which is the form that fits the column and the form a person recognises.
    pub relative: String,
    /// The file as the icon table sees it, read once in the worker thread so
    /// the painter never touches the disk. `None` when the file went away
    /// between the search and the stat.
    pub entry: Option<Entry>,
    /// `rg` only: the 1-based line number the match is on.
    pub line: Option<usize>,
    /// `rg` only: the text of that line, already lossy-decoded.
    pub text: String,
    /// Where in `text` to highlight, as a byte range.
    pub span: Option<Span>,
}

/// What the reader thread sends back.
enum Message {
    Hits(Vec<Hit>),
    /// The process ended — normally, or because the cap was reached.
    Done { capped: bool },
    /// The process could not be started, or died saying something.
    Failed(String),
}

/// A live process and the channel its output arrives on.
struct Running {
    /// Which query this process is for. A message tagged with anything else is
    /// from a search the user has already moved on from.
    generation: u64,
    /// Held so the search can be killed rather than merely ignored.
    child: Arc<Mutex<Option<Child>>>,
    /// Set when the reader should stop; checked between lines so a killed
    /// process's reader exits promptly rather than draining a full pipe.
    stop: Arc<AtomicBool>,
    results: Receiver<(u64, Message)>,
}

impl Drop for Running {
    /// Dropping a running search kills it. The whole point of holding the
    /// child.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut held) = self.child.lock() {
            if let Some(child) = held.as_mut() {
                let _ = child.kill();
                // Reaped so the process does not sit as a zombie for the life
                // of the session.
                let _ = child.wait();
            }
        }
    }
}

/// The `s` / `S` overlay.
pub struct Search {
    pub mode: Mode,
    /// Where the search runs, and what `relative` is relative to.
    pub root: PathBuf,
    pub buffer: InputBuffer,
    pub hits: Vec<Hit>,
    pub cursor: usize,
    pub first: usize,
    /// The search was cut off at [`MAX_HITS`].
    pub capped: bool,
    /// The process has ended, so the list is final.
    pub done: bool,
    /// Something to say instead of results: a missing binary, or a pattern rg
    /// refused.
    pub error: Option<String>,
    /// Whether [`Search::error`] is new enough to deserve a toast. The panel
    /// keeps showing the message; the toast is raised once, because a toast
    /// re-raised every frame is not a toast.
    notice: bool,
    /// Whether hidden files are searched, taken from the manager's own `.`
    /// toggle so the two never disagree about what is in a directory.
    hidden: bool,
    running: Option<Running>,
    /// The query the debounce is waiting to run, and when it is due.
    pending: Option<(String, Instant)>,
    generation: u64,
    notify: Notifier,
}

impl Search {
    pub fn new(mode: Mode, root: impl Into<PathBuf>, hidden: bool, notify: Notifier) -> Search {
        Search {
            mode,
            root: root.into(),
            buffer: InputBuffer::new(String::new(), 0),
            hits: Vec::new(),
            cursor: 0,
            first: 0,
            capped: false,
            done: false,
            error: None,
            notice: false,
            hidden,
            running: None,
            pending: None,
            generation: 0,
            notify,
        }
    }

    pub fn query(&self) -> &str {
        self.buffer.text()
    }

    /// The failure that has not been reported yet, if there is one. Taking it
    /// is reporting it.
    pub fn take_notice(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.notice) {
            return None;
        }
        self.error.clone()
    }

    /// The query changed. Arms the debounce; nothing spawns yet.
    ///
    /// The results already on screen are **kept** until the new ones arrive,
    /// rather than being cleared here. Clearing would make the list flicker
    /// empty on every keystroke, and the stale list is a better answer than no
    /// list for the 150 ms it survives (`delightful-ui` §8).
    pub fn changed(&mut self, now: Instant) {
        let query = self.buffer.text().to_string();
        if query.is_empty() {
            // Nothing to search for: stop whatever is running and empty the
            // list, because an empty query with the old results still up looks
            // like a search that ignored you.
            self.pending = None;
            self.running = None;
            self.hits.clear();
            self.cursor = 0;
            self.first = 0;
            self.done = true;
            self.capped = false;
            self.error = None;
            return;
        }
        self.pending = Some((query, now + DEBOUNCE));
    }

    /// When the next frame is owed by the debounce, if one is. This is the
    /// overlay's whole contribution to [`crate::app::App::next_deadline`] — a
    /// single instant known in advance, never a poll (PLAN §1).
    pub fn deadline(&self, now: Instant) -> Option<Duration> {
        let (_, due) = self.pending.as_ref()?;
        Some(due.saturating_duration_since(now))
    }

    /// Spawn the pending query if its debounce has expired. Returns whether
    /// anything changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some((query, due)) = &self.pending else {
            return false;
        };
        if now < *due {
            return false;
        }
        let query = query.clone();
        self.pending = None;
        self.spawn(&query);
        true
    }

    /// `Ctrl+s`: stop the process, keep what it found.
    pub fn cancel(&mut self) {
        self.pending = None;
        self.running = None;
        self.done = true;
    }

    /// Whether a process is running right now — what the card's spinner-free
    /// "searching…" label reads.
    pub fn searching(&self) -> bool {
        self.running.is_some() || self.pending.is_some()
    }

    /// Take whatever the reader thread has produced. Returns whether the list
    /// changed, which is what tells the frame to repaint.
    pub fn poll(&mut self) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let generation = running.generation;
        let messages: Vec<(u64, Message)> = running.results.try_iter().collect();
        let mut changed = false;
        let mut ended = false;
        for (tagged, message) in messages {
            // A message from a superseded search. The channel dies with its
            // `Running`, so this is belt and braces rather than the usual case.
            if tagged != generation {
                continue;
            }
            changed = true;
            match message {
                Message::Hits(mut hits) => {
                    let room = MAX_HITS.saturating_sub(self.hits.len());
                    hits.truncate(room);
                    self.hits.append(&mut hits);
                }
                Message::Done { capped } => {
                    self.capped = capped;
                    self.done = true;
                    ended = true;
                }
                Message::Failed(message) => {
                    self.error = Some(message);
                    self.done = true;
                    ended = true;
                }
            }
        }
        if ended {
            self.running = None;
        }
        changed
    }

    /// `↑` / `↓`, clamped.
    ///
    /// Clamped rather than wrapped, unlike the palette ([`crate::finder`]): a
    /// search result list is a place with a top and a bottom that you are
    /// reading *down*, and wrapping from the last hit to the first while
    /// results are still arriving would teleport you away from the one you were
    /// looking at.
    pub fn move_cursor(&mut self, delta: isize) {
        if self.hits.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.hits.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn chosen(&self) -> Option<&Hit> {
        self.hits.get(self.cursor)
    }

    /// Keep the cursor on screen, by the same scrolloff rule the panes use.
    pub fn scroll_into_view(&mut self, rows: usize, scrolloff: usize) {
        self.first =
            crate::viewport::first_visible(self.first, self.cursor, self.hits.len(), rows, scrolloff);
    }

    /// Kill whatever is running and start a process for `query`.
    fn spawn(&mut self, query: &str) {
        // Dropping the old `Running` kills its process — see its `Drop`.
        self.running = None;
        self.hits.clear();
        self.cursor = 0;
        self.first = 0;
        self.capped = false;
        self.done = false;
        self.error = None;
        self.notice = false;
        self.generation += 1;
        let generation = self.generation;

        let mut process = build(self.mode, query, self.hidden);
        process
            .current_dir(&self.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let (tx, rx) = unbounded::<(u64, Message)>();
        let child = match process.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.error = Some(start_failure(self.mode.binary(), &e));
                self.notice = true;
                self.done = true;
                return;
            }
        };
        let child = Arc::new(Mutex::new(Some(child)));
        let stop = Arc::new(AtomicBool::new(false));
        let running = Running {
            generation,
            child: Arc::clone(&child),
            stop: Arc::clone(&stop),
            results: rx,
        };
        let mode = self.mode;
        let root = self.root.clone();
        let query = query.to_string();
        let notify = Arc::clone(&self.notify);
        let spawned = std::thread::Builder::new()
            .name("df-search".to_string())
            .spawn(move || read(mode, root, query, generation, child, stop, tx, notify));
        if spawned.is_err() {
            // A reader that will not start costs this search and nothing else,
            // exactly as a decode worker that will not start costs the image
            // previews.
            self.error = Some("could not start the search reader".to_string());
            self.notice = true;
            self.done = true;
            return;
        }
        self.running = Some(running);
    }
}

/// The command line for one search.
///
/// Split out and `pub(crate)` so the argument list is a thing a test can read
/// rather than a thing buried in a spawn.
fn build(mode: Mode, query: &str, hidden: bool) -> Process {
    let mut process = Process::new(mode.binary());
    match mode {
        Mode::Names => {
            // `--color=never` because we parse this, and `--` so a query
            // beginning with a dash is a query and not a flag.
            process.arg("--color=never");
            if hidden {
                process.arg("--hidden");
            }
            process.arg("--").arg(query);
        }
        Mode::Content => {
            process
                .arg("--color=never")
                .arg("--smart-case")
                .arg("--line-number")
                // The column is what lets the match be highlighted inside the
                // line; without it a content hit is a row you have to re-read
                // to find the thing you searched for.
                .arg("--column")
                .arg("--no-heading")
                // Paths followed by a NUL rather than a colon: a path can
                // contain `:1:` and a colon-delimited parse would split it in
                // the wrong place. This makes the format unambiguous.
                .arg("--null")
                .arg("--max-columns")
                .arg(MAX_COLUMNS.to_string());
            if hidden {
                process.arg("--hidden");
            }
            process.arg("--").arg(query);
        }
    }
    process
}

/// What to say when the process would not start.
///
/// The one failure worth a sentence of its own is the tool not being installed
/// — PLAN §7.2's "missing fd/rg binary" — because that one has an action
/// attached to it and the reader can take it. Everything else is reported as it
/// came, because guessing at an unknown `io::Error` helps nobody.
fn start_failure(binary: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!("{binary} is not installed")
    } else {
        format!("{binary} would not start: {error}")
    }
}

/// The reader thread: lines in, batches of [`Hit`] out, one bell per batch.
#[allow(clippy::too_many_arguments)]
fn read(
    mode: Mode,
    root: PathBuf,
    query: String,
    generation: u64,
    child: Arc<Mutex<Option<Child>>>,
    stop: Arc<AtomicBool>,
    out: Sender<(u64, Message)>,
    notify: Notifier,
) {
    let stdout = match child.lock() {
        Ok(mut held) => held.as_mut().and_then(|c| c.stdout.take()),
        Err(_) => None,
    };
    let Some(stdout) = stdout else {
        let _ = out.send((generation, Message::Failed("no output from the search".into())));
        notify();
        return;
    };

    let mut reader = BufReader::new(stdout);
    let mut batch: Vec<Hit> = Vec::with_capacity(BATCH);
    let mut sent = 0usize;
    let mut capped = false;
    let mut last_flush = Instant::now();
    let mut line: Vec<u8> = Vec::new();

    // Sends the batch and rings the bell. Returns false when the receiver is
    // gone, which means the overlay was closed and there is nothing to read
    // for any more.
    let flush = |batch: &mut Vec<Hit>, last: &mut Instant| -> bool {
        if batch.is_empty() {
            return true;
        }
        let payload = std::mem::take(batch);
        if out.send((generation, Message::Hits(payload))).is_err() {
            return false;
        }
        notify();
        *last = Instant::now();
        true
    };

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                let _ = out.send((generation, Message::Failed(e.to_string())));
                notify();
                return;
            }
        }
        // Lossy on purpose: a filename or a matched line can be any bytes at
        // all, and a windows-1252 source file must show up as a row with a
        // replacement character in it rather than not show up.
        let text = String::from_utf8_lossy(trim_newline(&line));
        if let Some(hit) = parse(mode, &root, &query, &text) {
            batch.push(hit);
            sent += 1;
        }
        if sent >= MAX_HITS {
            capped = true;
            // Kill rather than keep reading: past the cap the process is
            // working for a list nobody will see.
            if let Ok(mut held) = child.lock() {
                if let Some(child) = held.as_mut() {
                    let _ = child.kill();
                }
            }
            break;
        }
        let due = batch.len() >= BATCH || last_flush.elapsed() >= BATCH_LINGER;
        if due && !flush(&mut batch, &mut last_flush) {
            return;
        }
    }

    if !flush(&mut batch, &mut last_flush) {
        return;
    }
    if let Ok(mut held) = child.lock() {
        if let Some(child) = held.as_mut() {
            let _ = child.wait();
        }
    }
    let _ = out.send((generation, Message::Done { capped }));
    notify();
}

/// `b"line\n"` → `b"line"`, `b"line\r\n"` → `b"line"`.
fn trim_newline(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// One output line as a [`Hit`], or `None` for a line that is not one.
///
/// `pub(crate)` rather than private so the two formats are pinned by tests
/// against the exact bytes the tools emit — this is the part that breaks
/// silently when a tool changes its output, and a silent break here is a search
/// that finds nothing and says nothing.
pub(crate) fn parse(mode: Mode, root: &Path, query: &str, line: &str) -> Option<Hit> {
    if line.is_empty() {
        return None;
    }
    match mode {
        Mode::Names => {
            // fd prints paths relative to where it was run, which is `root`.
            let relative = line.trim_end_matches('/').to_string();
            if relative.is_empty() {
                return None;
            }
            let path = root.join(&relative);
            Some(Hit {
                entry: Entry::read(&path).ok(),
                path,
                relative,
                line: None,
                text: String::new(),
                span: None,
            })
        }
        Mode::Content => {
            // `path\0line:column:text`.
            let (relative, rest) = line.split_once('\0')?;
            let (number, rest) = rest.split_once(':')?;
            let (column, text) = rest.split_once(':')?;
            let number: usize = number.parse().ok()?;
            let column: usize = column.parse().ok()?;
            let path = root.join(relative);
            Some(Hit {
                entry: Entry::read(&path).ok(),
                path,
                relative: relative.to_string(),
                line: Some(number),
                span: highlight(text, column, query),
                text: text.to_string(),
            })
        }
    }
}

/// Where to draw the match inside a result line.
///
/// rg reports a 1-based **byte** column but not the length of what matched, and
/// the pattern is a regex, so in general the length is not knowable from here
/// without asking rg for JSON. What is knowable is the overwhelmingly common
/// case: the pattern is a literal, and the literal is sitting at that column.
/// When it is, the span is exact. When it is not — a real regex, a
/// case-transformed match — nothing is highlighted, which is honest. A guessed
/// span drawn over the wrong characters would be worse than none.
fn highlight(text: &str, column: usize, query: &str) -> Option<Span> {
    let start = column.checked_sub(1)?;
    if !text.is_char_boundary(start) {
        return None;
    }
    let end = start + query.len();
    if end > text.len() || !text.is_char_boundary(end) {
        return None;
    }
    // Smart-case, matching the `--smart-case` the process was given.
    let matched = if df_core::fs::is_case_sensitive(query) {
        &text[start..end] == query
    } else {
        text[start..end].eq_ignore_ascii_case(query)
    };
    matched.then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/nonexistent-search-root")
    }

    /// fd's output is one relative path per line, and that is all it is.
    #[test]
    fn an_fd_line_is_a_relative_path() {
        let hit = parse(Mode::Names, &root(), "mouse", "src/mouse.rs").expect("parses");
        assert_eq!(hit.relative, "src/mouse.rs");
        assert_eq!(hit.path, root().join("src/mouse.rs"));
        assert_eq!(hit.line, None);
        // The file does not exist, so there is no entry — and that is a row,
        // not a crash.
        assert!(hit.entry.is_none());
        // fd marks directories with a trailing slash under some flags; the
        // slash is not part of the name.
        let hit = parse(Mode::Names, &root(), "src", "src/").expect("parses");
        assert_eq!(hit.relative, "src");
        assert!(parse(Mode::Names, &root(), "x", "").is_none());
    }

    /// rg's `--null --column` format, exactly as it comes off the pipe.
    #[test]
    fn an_rg_line_splits_into_path_line_column_and_text() {
        let hit = parse(
            Mode::Content,
            &root(),
            "ROW_HEIGHT",
            "src/ui.rs\u{0}66:11:pub const ROW_HEIGHT: f32 = 22.0;",
        )
        .expect("parses");
        assert_eq!(hit.relative, "src/ui.rs");
        assert_eq!(hit.line, Some(66));
        assert_eq!(hit.text, "pub const ROW_HEIGHT: f32 = 22.0;");
        let (start, end) = hit.span.expect("the literal is at the reported column");
        assert_eq!(&hit.text[start..end], "ROW_HEIGHT");
    }

    /// A path with a `:12:` in it is exactly why the format is NUL-delimited:
    /// a colon parse would split it in the wrong place and invent a line
    /// number.
    #[test]
    fn a_colon_in_a_path_does_not_confuse_the_parse() {
        let hit = parse(
            Mode::Content,
            &root(),
            "hello",
            "weird:12:name.txt\u{0}3:1:hello",
        )
        .expect("parses");
        assert_eq!(hit.relative, "weird:12:name.txt");
        assert_eq!(hit.line, Some(3));
        assert_eq!(hit.text, "hello");
    }

    /// A source file that is not UTF-8 — a windows-1252 curly quote, here —
    /// must produce a row with a replacement character in it rather than
    /// vanishing. The decode is lossy at the read, so by the time the parser
    /// sees it the damage is already a `U+FFFD`.
    #[test]
    fn a_windows_1252_line_survives_as_lossy_text() {
        let raw = b"src/legacy.c\x00\x34\x32:5:let it \x92 be\n";
        let text = String::from_utf8_lossy(trim_newline(raw));
        let hit = parse(Mode::Content, &root(), "it", &text).expect("parses");
        assert_eq!(hit.line, Some(42));
        assert!(hit.text.contains('\u{fffd}'), "got {:?}", hit.text);
        assert!(hit.text.starts_with("let it "));
    }

    /// Garbage in the middle of a stream is skipped, not fatal: a tool that
    /// prints a warning to stdout must not take the search down with it.
    #[test]
    fn a_line_that_is_not_a_result_is_skipped() {
        assert!(parse(Mode::Content, &root(), "x", "not a result at all").is_none());
        assert!(parse(Mode::Content, &root(), "x", "path\u{0}notanumber:1:text").is_none());
        assert!(parse(Mode::Content, &root(), "x", "path\u{0}12").is_none());
    }

    /// The highlight is exact when the literal really is at the column, and
    /// absent rather than wrong when it is not.
    #[test]
    fn the_highlight_is_exact_or_absent_never_approximate() {
        assert_eq!(highlight("hello world", 7, "world"), Some((6, 11)));
        // Case-insensitive, because the process was given `--smart-case`.
        assert_eq!(highlight("Hello World", 7, "world"), Some((6, 11)));
        // A capital in the query makes it literal, and this one does not match.
        assert_eq!(highlight("hello world", 7, "World"), None);
        // A regex the column cannot explain: no guess is made.
        assert_eq!(highlight("hello world", 1, r"w\w+"), None);
        // Off the end of the line.
        assert_eq!(highlight("hi", 2, "iiiii"), None);
        // Columns are 1-based *bytes*, so a multi-byte character is at the
        // column its first byte is at, and the span covers both of its bytes.
        assert_eq!(highlight("café au lait", 4, "é"), Some((3, 5)));
        // A column landing inside a multi-byte character cannot be a match, and
        // must not be a panicking slice either.
        assert_eq!(highlight("café au lait", 5, "au"), None);
        // On a boundary but not the text the query says: no guess.
        assert_eq!(highlight("café au lait", 6, "au"), None);
        // Column 0 does not exist; rg counts from one.
        assert_eq!(highlight("café au lait", 0, "c"), None);
    }

    /// The two command lines, pinned. `--` is there so a query starting with a
    /// dash is a query.
    #[test]
    fn the_command_lines_are_what_the_plan_asks_for() {
        let args = |mode, hidden| -> Vec<String> {
            build(mode, "-foo", hidden)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let names = args(Mode::Names, false);
        assert_eq!(names, vec!["--color=never", "--", "-foo"]);
        assert!(args(Mode::Names, true).contains(&"--hidden".to_string()));
        let content = args(Mode::Content, false);
        assert!(content.contains(&"--smart-case".to_string()));
        assert!(content.contains(&"--line-number".to_string()));
        assert!(content.contains(&"--null".to_string()));
        assert_eq!(content.last().map(String::as_str), Some("-foo"));
        let max = content
            .iter()
            .position(|a| a == "--max-columns")
            .expect("--max-columns is passed");
        assert_eq!(content[max + 1], MAX_COLUMNS.to_string());
    }

    fn silent() -> Notifier {
        Arc::new(|| {})
    }

    /// A keystroke arms the debounce and nothing else; the process is only
    /// spawned once the query has stood still.
    #[test]
    fn the_debounce_holds_the_process_back() {
        let now = Instant::now();
        let mut search = Search::new(Mode::Names, "/tmp", false, silent());
        let _ = search.buffer.insert_text("de");
        search.changed(now);
        assert!(search.searching(), "the debounce counts as searching");
        assert!(!search.tick(now), "not due yet");
        assert_eq!(
            search.deadline(now),
            Some(DEBOUNCE),
            "one scheduled wake-up, not a poll"
        );
        // Half way through, still nothing.
        assert!(!search.tick(now + DEBOUNCE / 2));
        // A second keystroke pushes the deadline out rather than queueing a
        // second search.
        search.changed(now + DEBOUNCE / 2);
        assert!(!search.tick(now + DEBOUNCE));
        assert!(search.tick(now + DEBOUNCE + DEBOUNCE));
    }

    /// Emptying the field stops the search and clears the list — the one case
    /// where the old results are *not* kept, because leaving them up under an
    /// empty query looks like a search that ignored you.
    #[test]
    fn an_empty_query_stops_and_clears() {
        let now = Instant::now();
        let mut search = Search::new(Mode::Names, "/tmp", false, silent());
        search.hits.push(Hit {
            path: PathBuf::from("/tmp/x"),
            relative: "x".into(),
            entry: None,
            line: None,
            text: String::new(),
            span: None,
        });
        search.changed(now);
        assert!(search.hits.is_empty());
        assert!(!search.searching());
        assert_eq!(search.deadline(now), None, "nothing is owed a frame");
    }

    /// A machine with no `fd` gets a sentence saying so, raised once —
    /// PLAN §7.2's "missing binary → notice". Anything else is reported as it
    /// came rather than guessed at.
    #[test]
    fn a_missing_tool_says_so_and_is_reported_exactly_once() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            start_failure("fd", &Error::from(ErrorKind::NotFound)),
            "fd is not installed"
        );
        let denied = start_failure("rg", &Error::from(ErrorKind::PermissionDenied));
        assert!(denied.starts_with("rg would not start: "), "got {denied}");

        let mut search = Search::new(Mode::Names, "/tmp", false, silent());
        assert_eq!(search.take_notice(), None, "nothing has gone wrong yet");
        search.error = Some("fd is not installed".to_string());
        search.notice = true;
        assert_eq!(search.take_notice().as_deref(), Some("fd is not installed"));
        // Once. A toast raised on every frame is not a toast.
        assert_eq!(search.take_notice(), None);
        assert!(search.error.is_some(), "the panel still says it");
    }

    /// The cursor clamps at both ends — unlike the palette's, and the reason is
    /// on `move_cursor`.
    #[test]
    fn the_cursor_clamps_rather_than_wrapping() {
        let mut search = Search::new(Mode::Names, "/tmp", false, silent());
        for n in 0..3 {
            search.hits.push(Hit {
                path: PathBuf::from(format!("/tmp/{n}")),
                relative: n.to_string(),
                entry: None,
                line: None,
                text: String::new(),
                span: None,
            });
        }
        search.move_cursor(-1);
        assert_eq!(search.cursor, 0);
        search.move_cursor(9);
        assert_eq!(search.cursor, 2);
        search.hits.clear();
        search.move_cursor(1);
        assert_eq!(search.cursor, 0);
    }
}
