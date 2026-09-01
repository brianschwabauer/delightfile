//! The `Tab` spot panel (PLAN §6): everything about one file, on a card.
//!
//! yazi's spot is the model — a floating panel over the browser, `Tab` to open
//! and `Tab` to close, `←`/`→` to swipe along the directory without shutting it
//! — and delightfile's takes three liberties, each of which is the reason the
//! panel exists here rather than being a wider status bar.
//!
//! **Permissions are an editor, not a readout.** Nine chips and an octal
//! number; clicking one flips that bit on disk immediately. A file manager that
//! can *show* you `-rw-r--r--` and cannot change it is asking you to open a
//! terminal, and the whole premise of this program is that you should not have
//! to.
//!
//! **The checksum is on demand.** Hashing is the one thing on this card that
//! costs real time, so it is a chip you press rather than a row that appears —
//! and once pressed it runs on a worker with a progress bar and a cancel, so a
//! 40 GB disk image is a thing you can start and change your mind about.
//!
//! **Everything expensive is asynchronous.** The sniff, the probe and the git
//! status all arrive after the card is already on screen, and each one lands by
//! ringing the same [`crate::Wake`] bell every other worker rings (PLAN §1). A
//! row that has nothing to say yet is simply absent; nothing on this card ever
//! shows a spinner where a fact will be.
//!
//! ## What is a decision and what is a fact
//!
//! [`rows`] is a pure function from [`Facts`] to what is drawn, so the wording,
//! the ordering and the "this row is not worth showing" rules are a unit test
//! rather than something you check by opening the panel on nine kinds of file.
//! The panel's *state* — which row has the keyboard, which permission bit is
//! selected, how far the hash has got — is the small mutable part, and it is
//! separate on purpose.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use df_core::fs::{Entry, Kind};

use crate::chrome::{self, CARD_PAD, FONT, PAD_X};
use crate::format::{human_size, long_stamp};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting};

/// The card's width, in points.
///
/// 560 is what a full 64-character SHA-256 needs on two monospace lines beside
/// a label column, which is the widest thing on the card and therefore the
/// thing that sets its width. Everything else fits inside that with room.
const WIDTH: f32 = 560.0;

/// The narrowest the card is ever drawn, in points.
///
/// The two paddings, the label column, the nine permission chips and the octal
/// readout beside them come to a shade under 480. Below that the editor — the
/// row this panel exists for — would spill out of its own card, and a card that
/// overhangs a 120-point window nobody can use at that size is the better of
/// two bad answers.
const MIN_WIDTH: f32 = 480.0;

/// The label column, in points. Wide enough for "Permissions" plus its gap, so
/// every value on the card starts at the same x — a column of facts is read by
/// running an eye down the values, and a ragged left edge defeats that.
const LABEL_COLUMN: f32 = 104.0;

/// A plain row's height. Slightly over the list's 22, because these rows are
/// read one at a time rather than scanned as a column.
const ROW: f32 = 24.0;

/// The permission editor's row height: the chips plus air above and below.
const PERM_ROW: f32 = 30.0;

/// One permission chip, as it is **drawn**. Small on purpose: nine of them in a
/// row is a `755` you can read at a glance, and a chip big enough to be a
/// button would turn that row into a toolbar.
const CHIP: f32 = 20.0;

/// One permission chip, as it is **clicked**. `delightful-ui` §1's floor: the
/// visual may be small, the target may not. The extra two points on every side
/// come out of the gap, which is why [`CHIP_PITCH`] is what it is.
const CHIP_HIT: f32 = 24.0;

/// Centre to centre between two chips. One point over [`CHIP_HIT`], so two
/// neighbouring targets touch and never overlap — an overlap would mean a click
/// on the boundary flipping whichever chip the loop happened to test first.
const CHIP_PITCH: f32 = CHIP_HIT + 1.0;

/// The extra gap between the owner, group and other triads, so nine chips read
/// as three groups of three — which is how `755` is read.
const TRIAD_GAP: f32 = 9.0;

/// The checksum row's height before it has a digest to show.
const HASH_ROW: f32 = 30.0;

/// …and after, when 64 hex characters are wrapped onto two lines.
const HASH_ROW_DONE: f32 = 44.0;

/// How many characters of the digest go on each line. Thirty-two: half of 64,
/// so the two lines are the same length and the block reads as a block.
const HASH_WRAP: usize = 32;

/// How often the hashing worker reports progress.
///
/// Sixty milliseconds is about four frames: fast enough that the bar looks
/// continuous, slow enough that hashing a gigabyte rings the wake bell a few
/// hundred times rather than four thousand. The chunk size is
/// [`crate::sha256`]'s and is far smaller, so this is the throttle that
/// actually decides the wake rate.
const PROGRESS_TICK: Duration = Duration::from_millis(60);

/// The nine permission bits, in the order they are written.
///
/// The tuple is `(bit, letter, what it means)` — the tooltip text is here
/// rather than in the painter because it is part of the *table*, and a table
/// with the explanations somewhere else is a table nobody keeps in step.
pub const BITS: [(u32, char, &str); 9] = [
    (0o400, 'r', "owner may read"),
    (0o200, 'w', "owner may write"),
    (0o100, 'x', "owner may execute"),
    (0o040, 'r', "group may read"),
    (0o020, 'w', "group may write"),
    (0o010, 'x', "group may execute"),
    (0o004, 'r', "everyone may read"),
    (0o002, 'w', "everyone may write"),
    (0o001, 'x', "everyone may execute"),
];

/// What a probe found out about a media file, as much of it as is worth a row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaFacts {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_us: i64,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub sample_rate: Option<u32>,
}

/// Everything the card knows about one file.
///
/// Built in two goes: the cheap half from the [`Entry`] the list already has,
/// and the rest as the sniffer, the prober and git answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub path: PathBuf,
    pub name: String,
    /// "file", "directory", "symlink" — what it *is*, before what is in it.
    pub kind: &'static str,
    /// The sniffed mime, which is the honest answer rather than the extension's.
    pub mime: String,
    /// `None` for a directory, whose size is a recursive walk this does not do.
    pub len: Option<u64>,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: Option<std::time::SystemTime>,
    pub btime: Option<std::time::SystemTime>,
    /// Where a symlink points, whether or not it is there.
    pub link_target: Option<PathBuf>,
    /// True when the link's target does not resolve — worth saying out loud.
    pub broken_link: bool,
    pub media: Option<MediaFacts>,
    /// git's word for this path, once the status has landed.
    pub git: Option<String>,
    /// A leading dot: the file is out of the listing unless `.` is on.
    pub hidden: bool,
    /// git is ignoring this path — the reason its row in the list is dim.
    ///
    /// Separate from [`Facts::git`], which is a sentence about the repository,
    /// because this is a fact about *why the row looks like that* and belongs
    /// next to `hidden` rather than after the branch name.
    pub ignored: bool,
}

impl Facts {
    /// The cheap half, from what the list pane already has in hand.
    pub fn from_entry(entry: &Entry) -> Facts {
        Facts {
            path: entry.path.clone(),
            name: entry.name.clone(),
            kind: match entry.kind {
                Kind::Dir => "directory",
                Kind::Symlink { .. } => "symlink",
                Kind::File => "file",
            },
            mime: entry.mime.to_string(),
            len: (!entry.is_dir()).then_some(entry.len),
            mode: entry.mode,
            uid: entry.uid,
            gid: entry.gid,
            mtime: entry.mtime,
            btime: entry.btime,
            link_target: entry.link_target(),
            broken_link: entry.is_broken_symlink(),
            media: None,
            git: None,
            hidden: entry.is_hidden,
            ignored: false,
        }
    }
}

/// What goes on the right of a row.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    /// The nine chips and the octal readout.
    Permissions,
    /// The chip, the progress bar, or the digest.
    Checksum,
}

/// One row of the card.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub value: Value,
}

impl Row {
    fn text(label: &'static str, value: impl Into<String>) -> Row {
        Row {
            label,
            value: Value::Text(value.into()),
        }
    }
}

/// What is worth saying about this file, in the order it is worth saying it.
///
/// **A row with nothing in it is not drawn.** A file with no creation time, no
/// symlink target and no git repository above it gets a shorter card rather
/// than a card of dashes — the dashes would be four lines of "we do not know"
/// between the two facts you opened the panel for.
pub fn rows(facts: &Facts) -> Vec<Row> {
    let mut rows = vec![
        Row::text("Name", facts.name.clone()),
        Row::text(
            "Where",
            facts
                .path
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string()),
        ),
        Row::text("Kind", format!("{} · {}", facts.kind, facts.mime)),
    ];
    if let Some(len) = facts.len {
        // Both spellings, always. The human one is what a person compares
        // against other files; the exact one is what they paste into a bug
        // report or check against a manifest — and a panel called "everything
        // about this file" that rounded 1,048,577 to 1.0 MB would be hiding
        // the one byte that is the whole question.
        rows.push(Row::text(
            "Size",
            format!("{} · {} bytes", human_size(len), grouped(len)),
        ));
    }
    rows.push(Row::text("Modified", long_stamp(facts.mtime)));
    if facts.btime.is_some() {
        rows.push(Row::text("Created", long_stamp(facts.btime)));
    }
    rows.push(Row::text(
        "Owner",
        format!(
            "{} · {}:{}",
            df_core::fs::owner::owner_label(facts.uid, facts.gid),
            facts.uid,
            facts.gid
        ),
    ));
    rows.push(Row {
        label: "Permissions",
        value: Value::Permissions,
    });
    if let Some(target) = &facts.link_target {
        rows.push(Row::text(
            "Links to",
            format!(
                "{}{}",
                target.to_string_lossy(),
                if facts.broken_link { "  (missing)" } else { "" }
            ),
        ));
    }
    if let Some(media) = &facts.media {
        if let Some(text) = media_text(media) {
            rows.push(Row::text("Media", text));
        }
    }
    // Why this row is not quite like the others. One line for both facts, and
    // only when there is one to make: a card that said "Visibility: normal" on
    // every file would be a row of noise explaining nothing.
    //
    // This is the panel's half of PLAN §7.3's ignored vocabulary — the list
    // says it with a dim row and a small `ignored` tag, and the card is where
    // you go when a mark is not enough of an answer.
    if let Some(text) = visibility_text(facts) {
        rows.push(Row::text("Visibility", text));
    }
    if let Some(git) = &facts.git {
        rows.push(Row::text("Git", git.clone()));
    }
    rows.push(Row {
        label: "SHA-256",
        value: Value::Checksum,
    });
    rows
}

/// "hidden · git-ignored", or `None` when the file is neither.
///
/// The two words are the two the rest of the window uses: `hidden` is what the
/// `.` toggle calls it, and `git-ignored` is what the list's tag and the help
/// sheet's legend call it. One vocabulary, three surfaces.
fn visibility_text(facts: &Facts) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if facts.hidden {
        parts.push("hidden");
    }
    if facts.ignored {
        parts.push("git-ignored");
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// "1920 × 1080 · 3:12 · h264 + aac 48 kHz", as much of it as is known.
fn media_text(media: &MediaFacts) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let (Some(w), Some(h)) = (media.width, media.height) {
        parts.push(format!("{w} × {h}"));
    }
    if media.duration_us > 0 {
        parts.push(duration(media.duration_us));
    }
    let mut codecs: Vec<String> = Vec::new();
    if let Some(video) = &media.video_codec {
        codecs.push(video.clone());
    }
    if let Some(audio) = &media.audio_codec {
        codecs.push(match media.sample_rate {
            // kHz rather than Hz: 48000 is a number to parse and 48 kHz is a
            // number to recognise.
            Some(rate) if rate > 0 => format!("{audio} {:.0} kHz", rate as f32 / 1000.0),
            _ => audio.clone(),
        });
    }
    if !codecs.is_empty() {
        parts.push(codecs.join(" + "));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `h:mm:ss`, dropping the hours when there are none.
fn duration(micros: i64) -> String {
    let total = (micros.max(0) / 1_000_000) as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A count with thousands separators. One loop over the digits, which is the
/// whole of what a formatting crate would do here.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The octal a mode is written as: `0644`.
pub fn octal(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

/// Flip one of [`BITS`] in `mode`.
///
/// Pure, and the reason is that this is the one thing on the card that changes
/// a file: "which bit does chip six flip" is a question with one answer and it
/// should not be discovered by trying it on a real file.
pub fn toggle(mode: u32, index: usize) -> u32 {
    match BITS.get(index) {
        Some((bit, _, _)) => mode ^ bit,
        None => mode,
    }
}

// ── The checksum ────────────────────────────────────────────────────────────

/// Where the SHA-256 has got to.
#[derive(Debug, Clone, PartialEq)]
pub enum Checksum {
    /// Nothing asked for. The chip is what is drawn.
    Idle,
    Running {
        done: u64,
        total: u64,
    },
    /// The digest, as 64 lowercase hex characters.
    Done(String),
    Failed(String),
}

impl Checksum {
    /// 0–1, or `None` when there is no bar to draw.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            Checksum::Running { done, total } if *total > 0 => {
                Some((*done as f32 / *total as f32).clamp(0.0, 1.0))
            }
            // A zero-length file, or one whose size is not known: a bar at 0 %
            // would be a claim about progress there is not one to make.
            Checksum::Running { .. } => None,
            _ => None,
        }
    }

    fn running(&self) -> bool {
        matches!(self, Checksum::Running { .. })
    }
}

/// The hashing worker: one thread, one file, one cancel flag.
struct Hasher {
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<Checksum>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Hasher {
    fn drop(&mut self) {
        // Closing the panel stops the hash. The flag is checked between chunks,
        // so the thread is gone within a chunk's worth of reading rather than
        // hashing a disk image nobody is looking at any more.
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// ── The panel ───────────────────────────────────────────────────────────────

/// What a key press on the card asks the app to do.
///
/// The panel does not touch the filesystem itself: it says what it wants and
/// [`crate::app`] does it, because the toast, the rescan and the journal all
/// live there and a panel that reached around them would be a second place
/// where files change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Write this mode to the file.
    SetMode(u32),
    StartChecksum,
    CancelChecksum,
}

/// The `Tab` panel's state.
pub struct Spot {
    pub facts: Facts,
    pub rows: Vec<Row>,
    /// Which row has the keyboard.
    pub cursor: usize,
    /// Which of the nine permission chips is selected, for `Space` to flip.
    pub bit: usize,
    pub checksum: Checksum,
    hasher: Option<Hasher>,
}

impl Spot {
    pub fn new(facts: Facts) -> Spot {
        let rows = rows(&facts);
        Spot {
            facts,
            rows,
            // The name is row 0 and is the least interesting thing on the card
            // to *act* on, but starting anywhere else would be starting the
            // cursor somewhere the eye did not put it.
            cursor: 0,
            bit: 0,
            checksum: Checksum::Idle,
            hasher: None,
        }
    }

    /// Swap in a different file, keeping the panel open — `←`/`→`.
    ///
    /// The row cursor is kept where it was when the new card is at least that
    /// tall, because swiping is for comparing the *same* fact across files and
    /// resetting to row 0 would undo the comparison on every press.
    pub fn swipe(&mut self, facts: Facts) {
        let was = self.cursor;
        // A hash of the file you have left is a hash nobody asked for.
        self.hasher = None;
        self.checksum = Checksum::Idle;
        self.facts = facts;
        self.rows = rows(&self.facts);
        self.cursor = was.min(self.rows.len().saturating_sub(1));
    }

    /// Rebuild the rows after a late fact landed — a sniff, a probe, git.
    pub fn refresh(&mut self) {
        self.rows = rows(&self.facts);
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn select(&mut self, index: usize) {
        self.cursor = index.min(self.rows.len().saturating_sub(1));
    }

    /// Move the permission selection — `Shift+←`/`Shift+→`.
    ///
    /// Shifted, because the plain arrows swipe between files and that is the
    /// binding the keymap documents. It wraps, because nine chips in a row is a
    /// ring and stopping at the end of it would be a dead key press.
    pub fn move_bit(&mut self, delta: isize) {
        let n = BITS.len() as isize;
        self.bit = (self.bit as isize + delta).rem_euclid(n) as usize;
    }

    /// `Space` or `Enter` on the focused row.
    pub fn activate(&self) -> Action {
        match self.rows.get(self.cursor).map(|r| &r.value) {
            Some(Value::Permissions) => Action::SetMode(toggle(self.facts.mode, self.bit)),
            Some(Value::Checksum) => {
                if self.checksum.running() {
                    Action::CancelChecksum
                } else {
                    Action::StartChecksum
                }
            }
            _ => Action::None,
        }
    }

    /// Which permission chip a pointer index refers to, if the card has one.
    /// What `c c` copies: the focused row's *value*, not its label.
    ///
    /// The two rows whose value is not already a string answer with the string
    /// a person would have read off the card — the octal for the permission
    /// chips, the digest for the checksum — so "copy the cell" means the same
    /// thing on every row. A checksum that has not been asked for has no value
    /// to copy, and says so by returning nothing.
    pub fn cell_text(&self) -> Option<(&'static str, String)> {
        let row = self.rows.get(self.cursor)?;
        let text = match &row.value {
            Value::Text(text) => text.clone(),
            Value::Permissions => octal(self.facts.mode),
            Value::Checksum => match &self.checksum {
                Checksum::Done(digest) => digest.clone(),
                _ => return None,
            },
        };
        Some((row.label, text))
    }

    pub fn perm_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r.value, Value::Permissions))
    }

    pub fn hash_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r.value, Value::Checksum))
    }

    /// Start hashing, on a thread. `wake` is rung on every progress report and
    /// once at the end — the same bell every other worker rings (PLAN §1).
    pub fn start_checksum(&mut self, wake: impl Fn() + Send + 'static) {
        let path = self.facts.path.clone();
        let total = self.facts.len.unwrap_or(0);
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Checksum::Running { done: 0, total }));
        self.checksum = Checksum::Running { done: 0, total };
        let worker_cancel = Arc::clone(&cancel);
        let worker_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("df-sha256".to_string())
            .spawn(move || {
                let mut last = Instant::now();
                let mut chunk = |done: u64| {
                    let now = Instant::now();
                    if now.duration_since(last) < PROGRESS_TICK {
                        return;
                    }
                    last = now;
                    set(&worker_shared, Checksum::Running { done, total });
                    wake();
                };
                let result = crate::sha256::hash_file(&path, &mut chunk, &worker_cancel);
                let end = match result {
                    Ok(crate::sha256::Scan::Done(digest)) => {
                        Checksum::Done(crate::sha256::hex(&digest))
                    }
                    // A cancel is not a failure and not a result: the row goes
                    // back to offering the chip, which is where it started.
                    Ok(crate::sha256::Scan::Cancelled) => Checksum::Idle,
                    Err(e) => Checksum::Failed(e),
                };
                set(&worker_shared, end);
                wake();
            });
        match handle {
            Ok(handle) => {
                self.hasher = Some(Hasher {
                    cancel,
                    shared,
                    handle: Some(handle),
                })
            }
            Err(e) => {
                self.checksum = Checksum::Failed(format!("could not start the hasher: {e}"));
            }
        }
    }

    pub fn cancel_checksum(&mut self) {
        self.hasher = None;
        self.checksum = Checksum::Idle;
    }

    /// Take whatever the hasher has published. Returns whether anything moved.
    pub fn poll(&mut self) -> bool {
        let Some(hasher) = &self.hasher else {
            return false;
        };
        let latest = match hasher.shared.lock() {
            Ok(state) => state.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if latest == self.checksum {
            return false;
        }
        self.checksum = latest;
        // A finished hash has no thread left to keep; dropping the handle here
        // joins it while it is already on its way out.
        if !self.checksum.running() {
            self.hasher = None;
        }
        true
    }
}

fn set(slot: &Arc<Mutex<Checksum>>, value: Checksum) {
    match slot.lock() {
        Ok(mut state) => *state = value,
        Err(poisoned) => *poisoned.into_inner() = value,
    }
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// Where the card's pieces are. Shared by the paint and the hit test, so the
/// two cannot disagree about where a chip is.
pub struct Geometry {
    pub card: egui::Rect,
    pub rows: Vec<egui::Rect>,
    /// The nine permission chips, when that row is on the card.
    pub bits: Vec<egui::Rect>,
    /// The checksum chip, while there is one to press.
    pub action: Option<egui::Rect>,
}

impl Geometry {
    /// What the pointer is over. `None` inside the card but on nothing is still
    /// inside the card as far as the caller is concerned — the modal swallows
    /// the pointer either way.
    pub fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        if let Some(index) = self.bits.iter().position(|r| r.contains(pos)) {
            return Some(Control::Action(index));
        }
        if self.action.is_some_and(|r| r.contains(pos)) {
            return Some(Control::Action(BITS.len()));
        }
        self.rows
            .iter()
            .position(|r| r.contains(pos))
            .map(Control::PanelRow)
    }

    pub fn rect_of(&self, control: Control) -> Option<egui::Rect> {
        match control {
            Control::Action(i) if i < BITS.len() => self.bits.get(i).copied(),
            Control::Action(_) => self.action,
            Control::PanelRow(i) => self.rows.get(i).copied(),
            _ => None,
        }
    }
}

fn row_height(row: &Row, checksum: &Checksum) -> f32 {
    match row.value {
        Value::Permissions => PERM_ROW,
        Value::Checksum => match checksum {
            Checksum::Done(_) => HASH_ROW_DONE,
            _ => HASH_ROW,
        },
        Value::Text(_) => ROW,
    }
}

/// Lay the card out above the panes' bottom edge, centred.
pub fn geometry(area: egui::Rect, bar_top: f32, spot: &Spot) -> Geometry {
    let heights: Vec<f32> = spot
        .rows
        .iter()
        .map(|row| row_height(row, &spot.checksum))
        .collect();
    let body: f32 = heights.iter().sum();
    let height = CARD_PAD * 2.0 + chrome::CARD_ROW + body + chrome::HINT_ROW;
    let width = WIDTH
        .min(area.width() - chrome::CARD_MARGIN * 2.0)
        .max(MIN_WIDTH);
    let card = egui::Rect::from_min_size(
        egui::pos2(
            area.center().x - width / 2.0,
            (bar_top - chrome::CARD_MARGIN - height).max(area.top() + chrome::CARD_MARGIN),
        ),
        // **Not clamped to the window's height.** A card shorter than its own
        // rows would put the permission chips outside the plate they are drawn
        // on, which is a card lying about where its controls are; overhanging a
        // window too short to hold it is the honest failure, and egui clips it.
        egui::vec2(width.max(0.0), height),
    );

    let mut y = card.top() + CARD_PAD + chrome::CARD_ROW;
    let mut rows = Vec::with_capacity(heights.len());
    for h in &heights {
        rows.push(egui::Rect::from_min_size(
            egui::pos2(card.left() + CARD_PAD, y),
            egui::vec2((card.width() - CARD_PAD * 2.0).max(0.0), *h),
        ));
        y += h;
    }

    let bits = spot
        .perm_row()
        .and_then(|index| rows.get(index))
        .map(|rect| bit_rects(*rect))
        .unwrap_or_default();
    let action = spot
        .hash_row()
        .and_then(|index| rows.get(index))
        .filter(|_| !matches!(spot.checksum, Checksum::Done(_)))
        .map(|rect| chip_rect(*rect, &spot.checksum));

    Geometry {
        card,
        rows,
        bits,
        action,
    }
}

/// The nine chips inside the permissions row, in three triads.
///
/// These are the **hit** rects (see [`CHIP_HIT`]); the visible chip is drawn
/// inset inside each one.
fn bit_rects(row: egui::Rect) -> Vec<egui::Rect> {
    let mut out = Vec::with_capacity(BITS.len());
    // Nine targets, eight gaps and two triad gaps. Measured rather than
    // guessed, because it is what keeps the row inside a card that has been
    // squeezed (see [`MIN_WIDTH`]).
    let total = BITS.len() as f32 * CHIP_HIT
        + (BITS.len() - 1) as f32 * (CHIP_PITCH - CHIP_HIT)
        + TRIAD_GAP * 2.0;
    let mut x = (row.left() + PAD_X + LABEL_COLUMN)
        .min(row.right() - total)
        .max(row.left());
    let y = row.center().y - CHIP_HIT / 2.0;
    for index in 0..BITS.len() {
        if index > 0 {
            x += CHIP_PITCH - CHIP_HIT;
            if index.is_multiple_of(3) {
                x += TRIAD_GAP;
            }
        }
        out.push(egui::Rect::from_min_size(
            egui::pos2(x, y),
            egui::vec2(CHIP_HIT, CHIP_HIT),
        ));
        x += CHIP_HIT;
    }
    out
}

/// The visible chip inside its hit target.
fn chip_face(hit: egui::Rect) -> egui::Rect {
    hit.shrink((CHIP_HIT - CHIP) / 2.0)
}

/// The checksum row's chip: "compute SHA-256", or "cancel" while it runs.
fn chip_rect(row: egui::Rect, checksum: &Checksum) -> egui::Rect {
    let width = if checksum.running() { 72.0 } else { 132.0 };
    egui::Rect::from_min_size(
        egui::pos2(row.left() + PAD_X + LABEL_COLUMN, row.center().y - 11.0),
        egui::vec2(width, 22.0),
    )
}

// ── Painting ────────────────────────────────────────────────────────────────

/// Draw the card.
pub fn paint(
    paint: &Painting<'_>,
    spot: &Spot,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    now: Instant,
) {
    let painter = paint.painter;
    let palette = paint.palette;
    chrome::card(paint, geometry.card, 1.0);

    painter.text(
        egui::pos2(
            geometry.card.left() + CARD_PAD,
            geometry.card.top() + CARD_PAD + chrome::CARD_ROW / 2.0,
        ),
        egui::Align2::LEFT_CENTER,
        "File info",
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
    );
    // The octal, up in the title row as well as beside the chips: it is the
    // thing a person came to read off, and it should be findable without
    // walking the card.
    painter.text(
        egui::pos2(
            geometry.card.right() - CARD_PAD,
            geometry.card.top() + CARD_PAD + chrome::CARD_ROW / 2.0,
        ),
        egui::Align2::RIGHT_CENTER,
        octal(spot.facts.mode),
        egui::FontId::monospace(FONT),
        palette.overlay1,
    );

    for (index, rect) in geometry.rows.iter().enumerate() {
        let Some(row) = spot.rows.get(index) else {
            break;
        };
        let on_cursor = index == spot.cursor;
        let key = Control::PanelRow(index);
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        if on_cursor || hover > 0.0 {
            let fill = mix(
                if on_cursor {
                    palette.surface1
                } else {
                    palette.crust
                },
                palette.surface0,
                hover,
            );
            painter.rect_filled(rect, chrome::CARD_ROW_RADIUS, fill);
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }

        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            row.label,
            egui::FontId::proportional(FONT),
            palette.overlay1,
        );
        let value_left = rect.left() + PAD_X + LABEL_COLUMN;
        let room = (rect.right() - PAD_X - value_left).max(0.0);
        match &row.value {
            Value::Text(text) => {
                chrome::truncated(
                    &inside,
                    egui::pos2(value_left, rect.center().y),
                    text,
                    if on_cursor {
                        palette.text
                    } else {
                        palette.subtext0
                    },
                    room,
                );
            }
            Value::Permissions => {
                permissions(
                    paint, &inside, spot, geometry, on_cursor, hovers, ripples, now,
                );
            }
            Value::Checksum => {
                checksum(paint, &inside, spot, geometry, rect, hovers, ripples, now);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
fn permissions(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    spot: &Spot,
    geometry: &Geometry,
    focused: bool,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    now: Instant,
) {
    let palette = paint.palette;
    for (index, chip) in geometry.bits.iter().enumerate() {
        let Some((bit, letter, _)) = BITS.get(index) else {
            break;
        };
        let on = spot.facts.mode & bit != 0;
        let key = Control::Action(index);
        let hover = hovers.hover(key);
        let rect = pressed_rect(chip_face(*chip), hovers.press(key));
        // An *on* bit is a filled chip and an off one is an outline: the state
        // has to be legible as a shape and not only as a colour
        // (`delightful-ui`, the same redundant-channel rule the selection bar
        // follows).
        let fill = if on {
            mix(palette.crust, palette.green, 0.35)
        } else {
            palette.crust
        };
        painter.rect_filled(rect, 4, mix(fill, palette.surface0, hover));
        if !on {
            painter.rect_stroke(
                rect,
                4,
                egui::Stroke::new(1.0, palette.surface2),
                egui::StrokeKind::Inside,
            );
        }
        // The selected chip wears a ring, and only while the row has the
        // keyboard — a ring on an unfocused row would be a second cursor.
        if focused && index == spot.bit {
            painter.rect_stroke(
                rect.expand(1.5),
                6,
                egui::Stroke::new(1.5, palette.blue),
                egui::StrokeKind::Outside,
            );
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if on { *letter } else { '·' },
            egui::FontId::monospace(FONT),
            if on { palette.text } else { palette.overlay0 },
        );
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
    }

    if let Some(last) = geometry.bits.last() {
        painter.text(
            egui::pos2(last.right() + TRIAD_GAP, last.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}  {}", octal(spot.facts.mode), rwx(spot.facts.mode)),
            egui::FontId::monospace(FONT),
            palette.subtext0,
        );
    }
}

/// The nine characters, as `ls` writes them.
pub fn rwx(mode: u32) -> String {
    BITS.iter()
        .map(|(bit, letter, _)| if mode & bit != 0 { *letter } else { '-' })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn checksum(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    spot: &Spot,
    geometry: &Geometry,
    row: egui::Rect,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    now: Instant,
) {
    let palette = paint.palette;
    let left = row.left() + PAD_X + LABEL_COLUMN;
    match &spot.checksum {
        Checksum::Done(digest) => {
            // The whole digest, wrapped, in monospace: a truncated hash is
            // worse than no hash, because it looks like one.
            for (line, chunk) in digest
                .as_bytes()
                .chunks(HASH_WRAP)
                .map(String::from_utf8_lossy)
                .enumerate()
            {
                painter.text(
                    egui::pos2(left, row.top() + 10.0 + line as f32 * 15.0),
                    egui::Align2::LEFT_CENTER,
                    chunk,
                    egui::FontId::monospace(FONT),
                    palette.teal,
                );
            }
        }
        Checksum::Failed(message) => {
            chrome::truncated(
                painter,
                egui::pos2(left, row.center().y),
                message,
                palette.red,
                (row.right() - PAD_X - left).max(0.0),
            );
        }
        state => {
            let Some(chip) = geometry.action else { return };
            let key = Control::Action(BITS.len());
            let hover = hovers.hover(key);
            let rect = pressed_rect(chip, hovers.press(key));
            painter.rect_filled(
                rect,
                chrome::CARD_ROW_RADIUS,
                mix(
                    mix(palette.crust, palette.blue, 0.22),
                    palette.surface0,
                    hover,
                ),
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                if state.running() {
                    "cancel"
                } else {
                    "compute SHA-256"
                },
                egui::FontId::proportional(FONT),
                palette.text,
            );
            let inside = painter.with_clip_rect(rect);
            for splash in ripples.splashes(key, now) {
                inside.circle_filled(
                    splash.center,
                    splash.radius,
                    egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
                );
            }

            if let Checksum::Running { done, total } = state {
                // The bar, beside the cancel: an unmeasured file gets a quiet
                // track and no fill, which is the honest picture of "running,
                // size unknown".
                let track = egui::Rect::from_min_size(
                    egui::pos2(rect.right() + PAD_X, rect.center().y - 2.0),
                    egui::vec2((row.right() - PAD_X - rect.right() - PAD_X).max(0.0), 4.0),
                );
                painter.rect_filled(track, 2, mix(palette.crust, palette.surface1, 0.9));
                if let Some(fraction) = state.fraction() {
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            track.min,
                            egui::vec2(track.width() * fraction, 4.0),
                        ),
                        2,
                        palette.blue,
                    );
                }
                painter.text(
                    egui::pos2(track.right(), track.center().y - 12.0),
                    egui::Align2::RIGHT_CENTER,
                    format!("{} / {}", human_size(*done), human_size(*total)),
                    egui::FontId::proportional(FONT - 1.0),
                    palette.overlay1,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spot card's rows against its plate (`delightful-ui` §15).
    #[test]
    fn the_card_radii_are_concentric() {
        assert_eq!(
            crate::chrome::CARD_ROW_RADIUS as f32 + crate::chrome::CARD_PAD,
            crate::chrome::CARD_RADIUS as f32
        );
    }
    use std::time::{Duration as Dur, SystemTime, UNIX_EPOCH};

    fn facts() -> Facts {
        Facts {
            path: PathBuf::from("/home/brian/Work/notes.md"),
            name: "notes.md".to_string(),
            kind: "file",
            mime: "text/markdown".to_string(),
            len: Some(1_048_577),
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            mtime: Some(UNIX_EPOCH + Dur::from_secs(1_700_000_000)),
            btime: None,
            link_target: None,
            broken_link: false,
            media: None,
            git: None,
            hidden: false,
            ignored: false,
        }
    }

    /// The card is the facts, in order, with nothing invented and nothing
    /// padded out — a row nobody can answer is a row nobody draws.
    #[test]
    fn the_card_lists_what_is_known_and_leaves_out_what_is_not() {
        let rows = rows(&facts());
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            vec![
                "Name",
                "Where",
                "Kind",
                "Size",
                "Modified",
                "Owner",
                "Permissions",
                "SHA-256"
            ]
        );
        // Both spellings of the size, because the exact byte is the reason
        // somebody opened this panel.
        let size = rows.iter().find(|r| r.label == "Size").expect("a size row");
        assert_eq!(
            size.value,
            Value::Text("1.0 MB · 1,048,577 bytes".to_string())
        );
    }

    #[test]
    fn a_directory_has_no_size_row_and_a_symlink_has_a_target() {
        let mut facts = facts();
        facts.kind = "directory";
        facts.len = None;
        assert!(
            !rows(&facts).iter().any(|r| r.label == "Size"),
            "a directory's size is a walk this panel does not do"
        );

        let mut facts = self::facts();
        facts.kind = "symlink";
        facts.link_target = Some(PathBuf::from("../elsewhere/notes.md"));
        facts.broken_link = true;
        let row = rows(&facts)
            .into_iter()
            .find(|r| r.label == "Links to")
            .expect("a target row");
        assert_eq!(
            row.value,
            Value::Text("../elsewhere/notes.md  (missing)".to_string())
        );
    }

    /// Why a row is dim, in the one place a mark is not enough of an answer.
    #[test]
    fn the_card_says_why_a_row_is_quiet() {
        let mut facts = facts();
        assert!(
            !rows(&facts).iter().any(|r| r.label == "Visibility"),
            "an ordinary file gets no row about being ordinary"
        );

        facts.hidden = true;
        let row = |f: &Facts| {
            rows(f)
                .into_iter()
                .find(|r| r.label == "Visibility")
                .map(|r| match r.value {
                    Value::Text(t) => t,
                    _ => unreachable!("Visibility is text"),
                })
        };
        assert_eq!(row(&facts).as_deref(), Some("hidden"));

        facts.hidden = false;
        facts.ignored = true;
        assert_eq!(row(&facts).as_deref(), Some("git-ignored"));

        // Both at once is one row, not two: they are the same sentence.
        facts.hidden = true;
        assert_eq!(row(&facts).as_deref(), Some("hidden · git-ignored"));
    }

    #[test]
    fn the_late_facts_add_their_own_rows() {
        let mut facts = facts();
        facts.btime = Some(UNIX_EPOCH + Dur::from_secs(1_600_000_000));
        facts.git = Some("M — modified".to_string());
        facts.media = Some(MediaFacts {
            width: Some(1920),
            height: Some(1080),
            duration_us: 3_732_000_000,
            video_codec: Some("h264".to_string()),
            audio_codec: Some("aac".to_string()),
            sample_rate: Some(48_000),
        });
        let rows = rows(&facts);
        let by = |label: &str| {
            rows.iter()
                .find(|r| r.label == label)
                .map(|r| r.value.clone())
        };
        assert!(by("Created").is_some());
        assert_eq!(by("Git"), Some(Value::Text("M — modified".to_string())));
        assert_eq!(
            by("Media"),
            Some(Value::Text(
                "1920 × 1080 · 1:02:12 · h264 + aac 48 kHz".to_string()
            ))
        );
        // A probe that came back knowing nothing adds no row rather than an
        // empty one.
        facts.media = Some(MediaFacts::default());
        assert!(!rows_have(&facts, "Media"));
    }

    fn rows_have(facts: &Facts, label: &str) -> bool {
        rows(facts).iter().any(|r| r.label == label)
    }

    #[test]
    fn durations_drop_the_hours_when_there_are_none() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(-5), "0:00");
        assert_eq!(duration(59_000_000), "0:59");
        assert_eq!(duration(3_600_000_000), "1:00:00");
    }

    /// The nine bits are the nine bits, and the octal is what a person types
    /// into `chmod` — this is the table the editor writes files with.
    #[test]
    fn toggling_a_bit_moves_exactly_that_bit() {
        assert_eq!(octal(0o644), "0644");
        assert_eq!(rwx(0o644), "rw-r--r--");
        assert_eq!(rwx(0o755), "rwxr-xr-x");
        assert_eq!(rwx(0), "---------");
        assert_eq!(rwx(0o777), "rwxrwxrwx");

        // Chip 2 is the owner's execute bit.
        assert_eq!(toggle(0o644, 2), 0o744);
        assert_eq!(toggle(0o744, 2), 0o644);
        // Chip 8 is everyone's execute bit, and chip 0 the owner's read.
        assert_eq!(toggle(0o644, 8), 0o645);
        assert_eq!(toggle(0o644, 0), 0o244);
        // Nothing outside the nine is touched — the setuid and sticky bits in
        // the high nibble survive a toggle, because losing them silently is how
        // a permissions editor breaks a system.
        assert_eq!(toggle(0o4755, 2), 0o4655);
        assert_eq!(octal(0o4755), "4755");
        // An index off the end is a no-op rather than a panic.
        assert_eq!(toggle(0o644, 99), 0o644);
    }

    #[test]
    fn the_permission_selection_wraps_both_ways() {
        let mut spot = Spot::new(facts());
        assert_eq!(spot.bit, 0);
        spot.move_bit(-1);
        assert_eq!(spot.bit, BITS.len() - 1, "it wrapped backwards");
        spot.move_bit(1);
        assert_eq!(spot.bit, 0);
        spot.move_bit(BITS.len() as isize + 2);
        assert_eq!(spot.bit, 2);
    }

    #[test]
    fn the_row_cursor_stays_inside_the_card() {
        let mut spot = Spot::new(facts());
        spot.move_cursor(-5);
        assert_eq!(spot.cursor, 0);
        spot.move_cursor(500);
        assert_eq!(spot.cursor, spot.rows.len() - 1);
        // …and the last row is the checksum, so `Space` there offers the hash.
        assert_eq!(spot.activate(), Action::StartChecksum);
        // On the permissions row it flips the selected bit instead.
        let perm = spot.perm_row().expect("a permissions row");
        spot.select(perm);
        spot.move_bit(2);
        assert_eq!(spot.activate(), Action::SetMode(0o744));
        // Anywhere else it does nothing at all, rather than something.
        spot.select(0);
        assert_eq!(spot.activate(), Action::None);
    }

    /// Swiping keeps the row you were reading and throws away the hash of the
    /// file you left — a digest is about one file and must never be shown
    /// beside another.
    #[test]
    fn swiping_keeps_the_row_and_drops_the_digest() {
        let mut spot = Spot::new(facts());
        let perm = spot.perm_row().expect("a permissions row");
        spot.select(perm);
        spot.checksum = Checksum::Done("a".repeat(64));

        let mut next = facts();
        next.name = "other.md".to_string();
        next.path = PathBuf::from("/home/brian/Work/other.md");
        spot.swipe(next);
        assert_eq!(spot.facts.name, "other.md");
        assert_eq!(spot.cursor, perm, "the row being compared was lost");
        assert_eq!(spot.checksum, Checksum::Idle);

        // A shorter card clamps rather than pointing past its own last row.
        let mut short = facts();
        short.kind = "directory";
        short.len = None;
        spot.select(spot.rows.len() - 1);
        spot.swipe(short);
        assert!(spot.cursor < spot.rows.len());
    }

    #[test]
    fn a_progress_bar_only_appears_when_there_is_progress_to_report() {
        assert_eq!(Checksum::Idle.fraction(), None);
        assert_eq!(
            Checksum::Running { done: 1, total: 4 }.fraction(),
            Some(0.25)
        );
        // A file whose length is not known gets a track and no fill rather
        // than a bar sitting at zero, which would be a claim about progress.
        assert_eq!(Checksum::Running { done: 9, total: 0 }.fraction(), None);
        assert_eq!(Checksum::Done("x".to_string()).fraction(), None);
    }

    /// The hash, end to end on a real file: the worker, the progress reports,
    /// the digest, and the panel picking all three up.
    #[test]
    fn hashing_a_file_reports_progress_and_lands_a_digest() {
        let dir = std::env::temp_dir().join(format!("df-spot-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hash-me.txt");
        std::fs::write(&path, b"abc").expect("write the fixture");

        let mut facts = facts();
        facts.path = path.clone();
        facts.len = Some(3);
        let mut spot = Spot::new(facts);
        spot.start_checksum(|| {});
        let deadline = Instant::now() + Dur::from_secs(5);
        while spot.checksum.running() && Instant::now() < deadline {
            spot.poll();
            std::thread::sleep(Dur::from_millis(2));
        }
        spot.poll();
        assert_eq!(
            spot.checksum,
            Checksum::Done(
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string()
            )
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hash of a file that is not there fails on the card rather than
    /// anywhere the user has to go looking for it.
    #[test]
    fn a_hash_that_cannot_be_read_says_so_on_the_row() {
        let mut facts = facts();
        facts.path = PathBuf::from("/nonexistent/delightfile-spot-test");
        let mut spot = Spot::new(facts);
        spot.start_checksum(|| {});
        let deadline = Instant::now() + Dur::from_secs(5);
        while spot.checksum.running() && Instant::now() < deadline {
            spot.poll();
            std::thread::sleep(Dur::from_millis(2));
        }
        spot.poll();
        assert!(
            matches!(spot.checksum, Checksum::Failed(_)),
            "{:?}",
            spot.checksum
        );
    }

    #[test]
    fn the_card_lays_out_and_paints_without_panicking() {
        let now = Instant::now();
        let mut facts = facts();
        facts.btime = Some(SystemTime::now());
        facts.git = Some("M — modified".to_string());
        facts.link_target = Some(PathBuf::from("elsewhere"));
        let mut spot = Spot::new(facts);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let painting = Painting {
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now,
            };
            for area in [
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0)),
                // A window too small for the card at all.
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 90.0)),
            ] {
                for state in [
                    Checksum::Idle,
                    Checksum::Running {
                        done: 5,
                        total: 100,
                    },
                    Checksum::Running { done: 5, total: 0 },
                    Checksum::Done("f".repeat(64)),
                    Checksum::Failed("Permission denied".to_string()),
                ] {
                    spot.checksum = state;
                    let geometry = geometry(area, area.bottom() - 30.0, &spot);
                    paint(
                        &painting,
                        &spot,
                        &geometry,
                        &Hovers::new(),
                        &Ripples::new(),
                        now,
                    );
                    // Every chip is inside the card it belongs to.
                    for rect in &geometry.bits {
                        assert!(geometry.card.expand(1.0).contains_rect(*rect), "{rect:?}");
                    }
                    // …and the hit test finds each of them where it was drawn.
                    for (index, rect) in geometry.bits.iter().enumerate() {
                        assert_eq!(geometry.hit(rect.center()), Some(Control::Action(index)));
                    }
                    if let Some(action) = geometry.action {
                        assert_eq!(
                            geometry.hit(action.center()),
                            Some(Control::Action(BITS.len()))
                        );
                    }
                }
            }
        });
    }

    /// The chips are at least `delightful-ui` §1's 24 points apart centre to
    /// centre, so the hit target of each one clears the minimum even though the
    /// chip itself is drawn smaller.
    #[test]
    fn every_permission_chip_clears_the_minimum_hit_target() {
        let row = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(560.0, PERM_ROW));
        let rects = bit_rects(row);
        // …and on a card squeezed to its floor, the nine still fit inside it.
        let narrow = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(MIN_WIDTH - CARD_PAD * 2.0, PERM_ROW),
        );
        for rect in bit_rects(narrow) {
            assert!(narrow.contains_rect(rect), "{rect:?} is outside {narrow:?}");
        }
        assert_eq!(rects.len(), BITS.len());
        for pair in rects.windows(2) {
            let gap = pair[1].left() - pair[0].right();
            assert!(gap >= 0.0, "two targets overlap by {}", -gap);
        }
        for rect in &rects {
            assert!(
                rect.width() >= 24.0 && rect.height() >= 24.0,
                "a target is under `delightful-ui` §1's 24 pt: {rect:?}"
            );
        }
        // …and the drawn chip is inside the target it belongs to, centred.
        for rect in &rects {
            let face = chip_face(*rect);
            assert!(rect.contains_rect(face));
            assert!((face.center() - rect.center()).length() < 1e-3);
        }
    }
}
