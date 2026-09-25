//! The sync card (`alt+p`): what a sync would do, and — when it went wrong —
//! what it did.
//!
//! One card, three stages. **Comparing** while df-core's planner walks the
//! trees on the pool: the card is up at once and counts, because a walk of a
//! big tree takes seconds and a key that shows nothing for five seconds is a
//! key pressed twice. **Ready** once the plan lands: the counts, the list of
//! what will be copied — and, for a mirror, removed — and the switches that
//! change what `Enter` does. And
//! **Result**, only for a run that ended with problems: the same card, naming
//! every file that failed and why. A toast has room for one name, and "2
//! problems" with no names is not something anybody can act on.
//!
//! Like [`crate::dialog`], everything here that decides anything is a plain
//! state machine over df-core's [`SyncPlan`] and [`SyncReport`]; the app wires
//! it to the keyboard, the pointer and the task engine (`app/syncing.rs`),
//! and the paint only reads it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use df_core::keymap::{Chord, Command, Key};
use df_core::sync::rsync::Transfer;
use df_core::sync::{Class, Kind, Mode, Removal, Root, SyncOptions, SyncPlan, SyncReport, Verify};
use df_core::tasks::{FnJob, Job, Lane, TaskCtx, TaskId};
use df_core::text::grouped;
use df_core::DfError;

use crate::chrome::{self, Hint, CARD_PAD, FONT, PAD_X};
use crate::dialog::{self, ANSWER_GAP, BUTTON_HEIGHT, MAX_WIDTH, ROW};
use crate::hover::Hovers;
use crate::ripple::Ripples;
use crate::ui::{Control, Painting};

/// How many paths the card lists before it says how many more there are.
///
/// Two hundred is more than anybody reads one by one and few enough that
/// building the rows is nothing. The list is there to recognise — "yes, those
/// are the new photos" — and the counts above it are what is read past that.
pub const LIST_CAP: usize = 200;

/// How many rows the list shows at once before the wheel takes over.
pub const VISIBLE: usize = 10;

/// Between the summary and the first row of the list.
const LIST_GAP: f32 = 8.0;

/// The width of the mark column: one monospace glyph and air after it.
const MARK: f32 = 16.0;

/// Where a comparison's answer lands: the plan, or why there is none.
pub type PlanSlot = Arc<Mutex<Option<Result<SyncPlan, String>>>>;

/// Where a sync's report lands.
pub type ReportSlot = Arc<Mutex<Option<SyncReport>>>;

/// Where the card is in its life.
pub enum Stage {
    /// The planner is walking, as task `id`; `seen` is how many paths it has
    /// looked at so far.
    Comparing {
        id: TaskId,
        slot: PlanSlot,
        seen: u64,
    },
    /// The plan is in, and `Enter` runs it.
    Ready(Arc<SyncPlan>),
    /// A run ended with problems, and these are they.
    Result(Box<SyncReport>),
}

/// How a row is marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    New,
    Changed,
    /// Only at the destination, and a mirror will remove it.
    Extra,
    /// A source path the sync will not copy — a socket, a folder it could not
    /// read — or, on the result card, a path that failed.
    Problem,
    /// The "… and N more" line at the foot of a capped list.
    More,
}

impl Mark {
    fn glyph(self) -> &'static str {
        match self {
            Mark::New => "+",
            Mark::Changed => "~",
            Mark::Extra => "−",
            Mark::Problem => "!",
            Mark::More => "",
        }
    }
}

/// One line of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub mark: Mark,
    pub text: String,
    /// Right-aligned: a file's size, or what went wrong.
    pub detail: String,
}

/// A sync with a server: the `rsync` run it is, and where each source lands
/// as the card names it (`sftp://…` at one end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub transfer: Transfer,
    pub roots: Vec<Root>,
}

/// The card.
pub struct SyncCard {
    /// What is being synced: the clipboard's paths when the card opened.
    pub sources: Vec<PathBuf>,
    /// Where to.
    pub dest: PathBuf,
    /// `Sync 3 items → ~/Photos` — the card's heading, and the task's name in
    /// the `w` panel, so the two read as one thing.
    pub title: String,
    pub mode: Mode,
    pub verify: Verify,
    pub content: bool,
    pub stage: Stage,
    /// The first row the list shows.
    pub first: usize,
    /// The part of a row the wheel has rolled and not yet spent.
    carry: f32,
    rows: Vec<Row>,
    /// How many rows the list had before a re-comparison emptied it, so the
    /// card holds its size while `c` compares again instead of shrinking and
    /// growing back a moment later.
    reserved: usize,
    /// When one end is a server: what `c` compares again with.
    pub remote: Option<Remote>,
}

impl SyncCard {
    /// A card for syncing `sources` into `dest`, comparing as task `id`.
    pub fn new(
        sources: Vec<PathBuf>,
        dest: PathBuf,
        title: String,
        id: TaskId,
        slot: PlanSlot,
    ) -> SyncCard {
        SyncCard {
            sources,
            dest,
            title,
            mode: Mode::Update,
            verify: Verify::Copied,
            content: false,
            stage: Stage::Comparing { id, slot, seen: 0 },
            first: 0,
            carry: 0.0,
            rows: Vec::new(),
            reserved: 0,
            remote: None,
        }
    }

    /// The same card, showing how a run went.
    pub fn result(dest: PathBuf, title: String, report: SyncReport) -> SyncCard {
        let rows = problem_rows(&dest, &report);
        SyncCard {
            sources: Vec::new(),
            dest,
            title,
            mode: report.mode,
            verify: report.verify,
            content: false,
            stage: Stage::Result(Box::new(report)),
            first: 0,
            carry: 0.0,
            rows,
            reserved: 0,
            remote: None,
        }
    }

    /// The options the planner is asked to compare with.
    pub fn options(&self) -> SyncOptions {
        SyncOptions {
            content: self.content,
        }
    }

    /// Back to comparing, as task `id` — after `c` has changed how.
    pub fn compare_again(&mut self, id: TaskId, slot: PlanSlot) {
        self.reserved = self.visible();
        self.rows.clear();
        self.first = 0;
        self.stage = Stage::Comparing { id, slot, seen: 0 };
    }

    /// The planner's answer is in.
    pub fn land(&mut self, plan: SyncPlan) {
        self.rows = plan_rows(&plan, self.mode);
        self.first = 0;
        self.reserved = 0;
        self.stage = Stage::Ready(Arc::new(plan));
    }

    /// The comparing task, while there is one.
    pub fn comparing(&self) -> Option<(TaskId, &PlanSlot)> {
        match &self.stage {
            Stage::Comparing { id, slot, .. } => Some((*id, slot)),
            _ => None,
        }
    }

    pub fn plan(&self) -> Option<&Arc<SyncPlan>> {
        match &self.stage {
            Stage::Ready(plan) => Some(plan),
            _ => None,
        }
    }

    pub fn is_result(&self) -> bool {
        matches!(self.stage, Stage::Result(_))
    }

    /// Record how far the comparison has got. Whether it moved, so the frame
    /// knows to draw.
    pub fn set_seen(&mut self, now: u64) -> bool {
        match &mut self.stage {
            Stage::Comparing { seen, .. } if *seen != now => {
                *seen = now;
                true
            }
            _ => false,
        }
    }

    /// `m`: update ⟷ mirror. The plan already knows the extras, so the
    /// summary and the list change at once.
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::Update => Mode::Mirror,
            Mode::Mirror => Mode::Update,
        };
        if let Stage::Ready(plan) = &self.stage {
            self.rows = plan_rows(plan, self.mode);
            let last = self.rows.len().saturating_sub(self.visible());
            self.first = self.first.min(last);
        }
    }

    /// `v`: verify what is copied ⟷ everything.
    pub fn toggle_verify(&mut self) {
        self.verify = match self.verify {
            Verify::Copied => Verify::Everything,
            Verify::Everything => Verify::Copied,
        };
    }

    /// `c`: compare by size and date ⟷ by contents. The plan is stale after
    /// it; the caller compares again.
    pub fn toggle_content(&mut self) {
        self.content = !self.content;
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// How many rows the list shows.
    pub fn visible(&self) -> usize {
        if matches!(self.stage, Stage::Comparing { .. }) {
            return self.reserved;
        }
        self.rows.len().min(VISIBLE)
    }

    /// `↑`/`↓`: a row at a time.
    pub fn scroll_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(self.visible()) as isize;
        self.first = (self.first as isize + delta).clamp(0, last) as usize;
    }

    /// The wheel over the card, in points. Whole rows at a time, the fraction
    /// kept for the next roll, and dropped at either end so the first notch
    /// back moves at once — the tray's rule ([`crate::tray::scroll`]).
    pub fn wheel(&mut self, points: f32) -> bool {
        let before = self.first;
        self.carry += crate::mouse::wheel_rows(points, ROW);
        let whole = self.carry.trunc();
        self.carry -= whole;
        let last = self.rows.len().saturating_sub(self.visible()) as i64;
        let moved = (self.first as i64 + whole as i64).clamp(0, last);
        if (moved == 0 && self.carry < 0.0) || (moved == last && self.carry > 0.0) {
            self.carry = 0.0;
        }
        self.first = moved as usize;
        self.first != before
    }

    /// The card's heading.
    pub fn heading(&self) -> String {
        match &self.stage {
            Stage::Result(report) => {
                let n = report.problems();
                format!(
                    "Sync finished with {}",
                    plural(n as u64, "problem", "problems")
                )
            }
            _ => self.title.clone(),
        }
    }

    /// The line under the heading: what the sync will do, in numbers.
    pub fn summary(&self) -> String {
        match &self.stage {
            Stage::Comparing { seen, .. } => format!("Comparing… {} so far", grouped(*seen)),
            Stage::Ready(plan) => {
                let mut parts = if plan.in_sync(self.mode) {
                    vec![
                        "Already in sync".to_string(),
                        plural(plan.unchanged.count, "file", "files"),
                    ]
                } else {
                    vec![
                        format!("{} new", grouped(plan.new.count)),
                        format!("{} changed", grouped(plan.changed.count)),
                        format!("{} unchanged", grouped(plan.unchanged.count)),
                        format!(
                            "{} to copy",
                            crate::format::human_size(plan.bytes_to_copy())
                        ),
                    ]
                };
                if !plan.skipped.is_empty() {
                    parts.push(format!("{} skipped", grouped(plan.skipped.len() as u64)));
                }
                parts.join(" · ")
            }
            Stage::Result(report) => outcome(report),
        }
    }

    /// A mirror's second line: what it will take away, and where to. Red, as
    /// the delete confirm's answer is, because it is the part of the card
    /// that removes things.
    pub fn removal_line(&self) -> Option<String> {
        let plan = self.plan()?;
        if self.mode != Mode::Mirror || !plan.has_extras() {
            return None;
        }
        Some(format!(
            "{} to {}",
            grouped(plan.extra.count),
            removal_verb(plan.removal)
        ))
    }

    /// Whether the answer destroys something for good: a mirror with nowhere
    /// to trash its extras. Its button is drawn as the delete confirm's is.
    pub fn danger(&self) -> bool {
        self.plan().is_some_and(|plan| {
            self.mode == Mode::Mirror && plan.has_extras() && plan.removal == Removal::Delete
        })
    }

    /// The settings, beside the buttons: what `Enter` will do, read at the
    /// moment the hand is on its way there.
    pub fn status(&self) -> String {
        if self.is_result() {
            return String::new();
        }
        let mode = match self.mode {
            Mode::Update => "Update",
            Mode::Mirror => "Mirror",
        };
        let verify = match self.verify {
            Verify::Copied => "verify copied",
            Verify::Everything => "verify everything",
        };
        let compare = if self.content {
            "by contents"
        } else {
            "by size and date"
        };
        format!("{mode} · {verify} · {compare}")
    }

    /// The buttons, left to right: the way out, then the answer. The result
    /// card is only read, so it has the one. A mirror that removes things says
    /// so on its own button — `Sync and trash 3` — so the removal is in the
    /// label the hand is pressing, not only in a line above it.
    pub fn labels(&self) -> Vec<String> {
        let answer = match &self.stage {
            Stage::Result(_) => return vec!["Close".to_string()],
            Stage::Ready(plan) if plan.in_sync(self.mode) => "Verify".to_string(),
            Stage::Ready(plan) if self.mode == Mode::Mirror && plan.has_extras() => format!(
                "Sync and {} {}",
                removal_verb(plan.removal),
                grouped(plan.extra.count)
            ),
            _ => "Sync".to_string(),
        };
        vec!["Cancel".to_string(), answer]
    }

    /// Whether the answer button does anything yet. Not while comparing: there
    /// is no plan to run.
    pub fn can_commit(&self) -> bool {
        matches!(self.stage, Stage::Ready(_))
    }

    /// What `Enter` runs: the card's verify, or — with nothing to copy — a
    /// verify of everything, which is the only thing left worth doing.
    pub fn run_verify(&self) -> Verify {
        match &self.stage {
            Stage::Ready(plan) if plan.in_sync(self.mode) => Verify::Everything,
            _ => self.verify,
        }
    }

    /// The card's hint strip. Each switch says what pressing it will do, the
    /// status line says what is set now.
    pub fn hints(&self) -> Vec<Hint> {
        if self.is_result() {
            return vec![
                Hint::inert("↑↓", "scroll"),
                Hint::new("Esc", "close", Command::OverlayClose),
            ];
        }
        let verify = match self.verify {
            Verify::Copied => "verify everything",
            Verify::Everything => "verify copied",
        };
        let compare = if self.content {
            "compare size, date"
        } else {
            "compare contents"
        };
        let enter = match &self.stage {
            Stage::Ready(plan) if plan.in_sync(self.mode) => "verify",
            _ => "sync",
        };
        let mode = match self.mode {
            Mode::Update => "mirror",
            Mode::Mirror => "update only",
        };
        vec![
            Hint::key("m", mode, Chord::plain(Key::Char('m'))),
            Hint::key("v", verify, Chord::plain(Key::Char('v'))),
            Hint::key("c", compare, Chord::plain(Key::Char('c'))),
            Hint::new("Enter", enter, Command::OverlaySubmit),
            Hint::new("Esc", "cancel", Command::OverlayClose),
        ]
    }
}

/// "trash" or "delete": what a mirror does with an extra.
fn removal_verb(removal: Removal) -> &'static str {
    match removal {
        Removal::Trash => "trash",
        Removal::Delete => "delete",
    }
}

/// The list for a plan in `mode`: the new and the changed, then a mirror's
/// extras, then what the sync will not touch and why, capped at
/// [`LIST_CAP`].
fn plan_rows(plan: &SyncPlan, mode: Mode) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut total = 0usize;
    for item in plan.listed(mode) {
        total += 1;
        if rows.len() < LIST_CAP {
            rows.push(Row {
                mark: match item.class {
                    Class::New => Mark::New,
                    Class::Extra => Mark::Extra,
                    _ => Mark::Changed,
                },
                text: plan.label(item),
                detail: if item.kind == Kind::File {
                    crate::format::human_size(item.bytes)
                } else {
                    String::new()
                },
            });
        }
    }
    for (path, why) in &plan.skipped {
        total += 1;
        if rows.len() < LIST_CAP {
            rows.push(Row {
                mark: Mark::Problem,
                text: path.display().to_string(),
                detail: why.clone(),
            });
        }
    }
    if total > rows.len() {
        rows.push(Row {
            mark: Mark::More,
            text: format!("… and {} more", grouped((total - rows.len()) as u64)),
            detail: String::new(),
        });
    }
    rows
}

/// Every problem a run had, each once: the copies that failed, the files the
/// verify found wrong, then what could not be read to copy at all. Not
/// capped — this is the list the card exists to show.
fn problem_rows(dest: &Path, report: &SyncReport) -> Vec<Row> {
    report
        .errors
        .iter()
        .chain(&report.verify_failures)
        .chain(&report.skipped)
        .map(|(path, why)| Row {
            mark: Mark::Problem,
            text: path
                .strip_prefix(dest)
                .map(|rel| rel.display().to_string())
                .unwrap_or_else(|_| path.display().to_string()),
            detail: why.clone(),
        })
        .collect()
}

/// A finished run in one line, for the toast and the result card's summary.
pub fn outcome(report: &SyncReport) -> String {
    let mut parts = Vec::new();
    if report.copied > 0 {
        parts.push(format!("Synced {}", plural(report.copied, "file", "files")));
        parts.push(crate::format::human_size(report.copied_bytes));
    } else if report.made > 0 {
        parts.push(format!(
            "Synced {}",
            plural(report.made, "folder", "folders")
        ));
    } else if report.removed > 0 {
        parts.push("Mirrored".to_string());
    } else {
        parts.push("Already in sync".to_string());
    }
    if report.removed > 0 {
        let verb = match report.removal {
            Removal::Trash => "trashed",
            Removal::Delete => "deleted",
        };
        parts.push(format!("{} {verb}", grouped(report.removed)));
    }
    if report.cancelled {
        parts.push("cancelled".to_string());
    } else if report.local_only {
        // The server could not hash its side, so nothing was compared, and
        // the toast must not say otherwise.
        parts.push("verified locally only".to_string());
    } else if report.verify_failures.is_empty() && report.verified > 0 {
        // A bare "verified" says the whole of it arrived; with files left
        // unread it did not, so the count of what was verified stands instead.
        let everything =
            report.verify == Verify::Everything || report.copied == 0 || !report.skipped.is_empty();
        parts.push(if everything {
            format!("{} verified", plural(report.verified, "file", "files"))
        } else {
            "verified".to_string()
        });
    }
    if report.unflushed && !report.cancelled {
        parts.push("not flushed on the server".to_string());
    }
    if !report.skipped.is_empty() {
        parts.push(format!(
            "{} could not be read",
            grouped(report.skipped.len() as u64)
        ));
    }
    parts.join(" · ")
}

/// "1 file" / "1,204 files".
fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", grouped(n))
    }
}

// ── The two jobs ────────────────────────────────────────────────────────────

/// The comparison, as a job for the pool, and where it will leave its answer.
///
/// [`Lane::Micro`], as an archive listing is: it reads rather than writes, and
/// the card is waiting on it. A cancel — `Esc` on the card, `c` asking for a
/// different comparison, `x` in the `w` panel — stops the walk at its next
/// entry and leaves the slot empty.
pub fn plan_job(
    name: String,
    sources: Vec<PathBuf>,
    dest: PathBuf,
    options: SyncOptions,
) -> (impl Job, PlanSlot) {
    let slot: PlanSlot = Arc::new(Mutex::new(None));
    let landing = Arc::clone(&slot);
    let job = FnJob::new(name, Lane::Micro, move |ctx: &TaskCtx| {
        let result = df_core::sync::plan(&sources, &dest, options, &|| ctx.is_cancelled(), &|n| {
            ctx.advance(0, n)
        });
        let result = match result {
            Err(DfError::Cancelled) => return Err(DfError::Cancelled),
            other => other.map_err(|e| e.to_string()),
        };
        store(&landing, result);
        Ok(())
    });
    (job, slot)
}

/// The comparison with a server — `rsync`'s dry run — as a job for the pool.
/// The same lane, slot and cancel as [`plan_job`]; a cancel kills the `rsync`.
pub fn remote_plan_job(
    name: String,
    remote: Remote,
    dest: PathBuf,
    options: SyncOptions,
) -> (impl Job, PlanSlot) {
    let slot: PlanSlot = Arc::new(Mutex::new(None));
    let landing = Arc::clone(&slot);
    let job = FnJob::new(name, Lane::Micro, move |ctx: &TaskCtx| {
        let result = df_core::sync::rsync::plan(
            remote.transfer.clone(),
            remote.roots.clone(),
            dest.clone(),
            options,
            &|| ctx.is_cancelled(),
        );
        let result = match result {
            Err(DfError::Cancelled) => return Err(DfError::Cancelled),
            other => other.map_err(|e| e.to_string()),
        };
        store(&landing, result);
        Ok(())
    });
    (job, slot)
}

/// The sync itself, as a job, and where its report will land.
///
/// The report is left in the slot *before* the job says how it ended, and the
/// ending is what the `w` panel shows: cancelled when it was, failed when
/// anything could not be copied or did not verify — so a run with problems is
/// a red row in the panel as well as a card — and done otherwise.
pub fn sync_job(
    name: String,
    plan: Arc<SyncPlan>,
    mode: Mode,
    verify: Verify,
) -> (impl Job, ReportSlot) {
    let slot: ReportSlot = Arc::new(Mutex::new(None));
    let landing = Arc::clone(&slot);
    let job = FnJob::new(name, Lane::Macro, move |ctx: &TaskCtx| {
        let report = df_core::sync::execute(&plan, mode, verify, ctx);
        let (cancelled, problems) = (report.cancelled, report.problems());
        store(&landing, report);
        if cancelled {
            return Err(DfError::Cancelled);
        }
        if problems > 0 {
            return Err(DfError::Op(plural(problems as u64, "problem", "problems")));
        }
        Ok(())
    });
    (job, slot)
}

fn store<T>(slot: &Arc<Mutex<Option<T>>>, value: T) {
    match slot.lock() {
        Ok(mut guard) => *guard = Some(value),
        Err(poisoned) => *poisoned.into_inner() = Some(value),
    }
}

/// Take whatever has landed in a slot.
pub fn take<T>(slot: &Arc<Mutex<Option<T>>>) -> Option<T> {
    match slot.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

/// A sync running on the pool, and what its landing owes the UI.
pub struct Running {
    pub id: TaskId,
    pub slot: ReportSlot,
    /// The card's heading, for a result card if one is needed.
    pub title: String,
    /// The directory the sync wrote into: rescanned when it lands.
    pub dest: PathBuf,
    /// The top-level entries it wrote, in plan order: where the cursor goes.
    pub focus: Vec<PathBuf>,
}

/// The top-level entries a plan will write into: every root with something
/// new or changed at or under it.
pub fn focus(plan: &SyncPlan) -> Vec<PathBuf> {
    let mut touched = vec![false; plan.roots.len()];
    for item in plan.listed(Mode::Update) {
        touched[item.root] = true;
    }
    plan.roots
        .iter()
        .zip(touched)
        .filter(|(_, touched)| *touched)
        .map(|(root, _)| root.dst.clone())
        .collect()
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// Where the card's pieces are: one measurement for the hit test and the
/// paint.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    pub card: egui::Rect,
    pub close: egui::Rect,
    /// The list, every visible row's rect inside it.
    pub body: egui::Rect,
    pub rows: Vec<egui::Rect>,
    /// The buttons, left to right.
    pub actions: Vec<egui::Rect>,
    /// The status, on the buttons' line from the card's padding to a gap
    /// short of the first button.
    pub status: egui::Rect,
}

impl Geometry {
    /// What the pointer is over: the `×` and the buttons. The rows are what
    /// the answer is about, not controls — the confirm card's rule.
    pub fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        if self.close.contains(pos) {
            return Some(Control::Close);
        }
        self.actions
            .iter()
            .position(|rect| rect.contains(pos))
            .map(Control::Action)
    }

    pub fn rect_of(&self, control: Control) -> Option<egui::Rect> {
        match control {
            Control::Close => Some(self.close),
            Control::Action(i) => self.actions.get(i).copied(),
            _ => None,
        }
    }
}

/// Lay the card out.
///
/// As wide as the conflict card — a path is read left to right and the room
/// is what keeps it whole — and as tall as its list needs, up to [`VISIBLE`]
/// rows. Top to bottom: the pad, the heading (with the `×` on its line), the
/// summary, a mirror's removal line, the list, [`ANSWER_GAP`], the status and the buttons on one line,
/// the hint strip, the pad. The buttons are measured and placed by the confirm
/// card's own rule ([`dialog::button_row`]), the last one [`CARD_PAD`] in from
/// the card's right edge, so its corner is concentric with the card's.
pub fn geometry(painter: &egui::Painter, area: egui::Rect, card: &SyncCard) -> Geometry {
    let visible = card.visible();
    let list = if visible > 0 {
        LIST_GAP + visible as f32 * ROW
    } else {
        0.0
    };
    // The heading, the summary, and a mirror's line of what it removes.
    let lines = if card.removal_line().is_some() {
        3.0
    } else {
        2.0
    };
    let height =
        CARD_PAD + ROW * lines + list + ANSWER_GAP + BUTTON_HEIGHT + chrome::HINT_ROW + CARD_PAD;
    let rect = dialog::card_rect(area, MAX_WIDTH, height);
    let inner_left = rect.left() + CARD_PAD;
    let inner_right = rect.right() - CARD_PAD;
    let body_top = rect.top() + CARD_PAD + ROW * lines + if visible > 0 { LIST_GAP } else { 0.0 };
    let body = egui::Rect::from_min_max(
        egui::pos2(inner_left, body_top),
        egui::pos2(inner_right, body_top + visible as f32 * ROW),
    );
    let rows = (0..visible.min(card.rows().len()))
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(inner_left, body_top + i as f32 * ROW),
                egui::vec2(body.width(), ROW),
            )
        })
        .collect();
    let buttons_top = rect.bottom() - CARD_PAD - chrome::HINT_ROW - BUTTON_HEIGHT;
    let labels = card.labels();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let actions = dialog::button_row(painter, inner_right, buttons_top, &labels);
    let first_button = actions.first().map_or(inner_right, egui::Rect::left);
    let status = egui::Rect::from_min_max(
        egui::pos2(inner_left, buttons_top),
        egui::pos2(
            (first_button - crate::ui::GAP).max(inner_left),
            buttons_top + BUTTON_HEIGHT,
        ),
    );
    Geometry {
        card: rect,
        close: chrome::close_button_rect(rect),
        body,
        rows,
        actions,
        status,
    }
}

// ── Painting ────────────────────────────────────────────────────────────────

/// Draw the card over a scrim.
pub fn paint(
    paint: &Painting<'_>,
    area: egui::Rect,
    card: &SyncCard,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    painter.rect_filled(area, 0, egui::Color32::from_black_alpha(chrome::HELP_SCRIM));
    chrome::card(paint, geometry.card, 1.0);

    let left = geometry.card.left() + CARD_PAD;
    let font = egui::FontId::proportional(FONT);
    chrome::close_button(paint, geometry.close, hovers, ripples);
    chrome::truncated_in(
        painter,
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW / 2.0),
        &card.heading(),
        palette.text,
        (geometry.close.left() - crate::ui::GAP - left).max(0.0),
        dialog::title_font(),
    );
    let summary_tint = match &card.stage {
        Stage::Comparing { .. } => palette.overlay1,
        _ => palette.subtext0,
    };
    chrome::truncated_in(
        painter,
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW + ROW / 2.0),
        &card.summary(),
        summary_tint,
        (geometry.card.right() - CARD_PAD - left).max(0.0),
        font.clone(),
    );
    if let Some(line) = card.removal_line() {
        chrome::truncated_in(
            painter,
            egui::pos2(left, geometry.card.top() + CARD_PAD + ROW * 2.0 + ROW / 2.0),
            &line,
            palette.red,
            (geometry.card.right() - CARD_PAD - left).max(0.0),
            font.clone(),
        );
    }

    let clipped = painter.with_clip_rect(geometry.body);
    let mono = egui::FontId::monospace(FONT);
    // How many rows are below the last one drawn: said on that row, where the
    // confirm card says it, since a list that simply stops reads as the end.
    let below = card
        .rows()
        .len()
        .saturating_sub(card.first + geometry.rows.len());
    let last = geometry.rows.len().saturating_sub(1);
    for (i, rect) in geometry.rows.iter().enumerate() {
        let Some(row) = card.rows().get(card.first + i) else {
            break;
        };
        let (detail_text, detail_tint) = if i == last && below > 0 {
            (format!("+{} more", grouped(below as u64)), palette.overlay0)
        } else if row.mark == Mark::Problem {
            (row.detail.clone(), palette.red)
        } else {
            (row.detail.clone(), palette.overlay1)
        };
        let (mark_tint, text_tint) = match row.mark {
            Mark::New => (palette.green, palette.subtext0),
            Mark::Changed => (palette.peach, palette.subtext0),
            Mark::Extra => (palette.red, palette.subtext0),
            Mark::Problem => (palette.red, palette.subtext0),
            Mark::More => (palette.overlay0, palette.overlay0),
        };
        let y = rect.center().y;
        clipped.text(
            egui::pos2(rect.left(), y),
            egui::Align2::LEFT_CENTER,
            row.mark.glyph(),
            mono.clone(),
            mark_tint,
        );
        // The detail keeps its room first — a size or a reason is short and
        // is the half of the row that answers something — up to half the row.
        let detail =
            chrome::text_width(painter, &detail_text, font.clone()).min(rect.width() / 2.0);
        if !detail_text.is_empty() {
            chrome::truncated_in(
                &clipped,
                egui::pos2(rect.right() - detail, y),
                &detail_text,
                detail_tint,
                detail + 1.0,
                font.clone(),
            );
        }
        let text_left = rect.left() + MARK;
        let room = rect.right() - detail - PAD_X - text_left;
        chrome::truncated_in(
            &clipped,
            egui::pos2(text_left, y),
            &row.text,
            text_tint,
            room.max(0.0),
            font.clone(),
        );
    }

    let status = card.status();
    if !status.is_empty() {
        chrome::truncated_in(
            painter,
            egui::pos2(geometry.status.left(), geometry.status.center().y),
            &status,
            palette.overlay1,
            geometry.status.width().max(0.0),
            font,
        );
    }

    let labels = card.labels();
    let last = labels.len().saturating_sub(1);
    for (i, rect) in geometry.actions.iter().enumerate() {
        let Some(label) = labels.get(i) else { break };
        dialog::button(
            paint,
            *rect,
            label,
            i == last,
            i == last && card.danger(),
            Control::Action(i),
            hovers,
            ripples,
        );
        // Veiled rather than swapped while there is no plan to run: the button
        // stays where the hand expects it, at the size it will be.
        if i == last && !card.is_result() && !card.can_commit() {
            painter.rect_filled(
                *rect,
                crate::ui::ROW_RADIUS,
                chrome::fade(palette.crust, 0.55),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;
    use df_core::sync::{Item, Root, Tally};

    fn plan_with(items: Vec<Item>, new: u64, changed: u64, unchanged: u64, bytes: u64) -> SyncPlan {
        df_core::sync::SyncPlan {
            roots: vec![Root {
                src: PathBuf::from("/src/photos"),
                dst: PathBuf::from("/dst/photos"),
            }],
            dest_dir: PathBuf::from("/dst"),
            options: SyncOptions::default(),
            items,
            new: Tally { count: new, bytes },
            changed: Tally {
                count: changed,
                bytes: 0,
            },
            unchanged: Tally {
                count: unchanged,
                bytes: 0,
            },
            extra: Tally::default(),
            removal: Removal::Trash,
            skipped: Vec::new(),
            remote: None,
        }
    }

    fn item(rel: &str, class: Class, kind: Kind, bytes: u64) -> Item {
        Item {
            root: 0,
            rel: PathBuf::from(rel),
            class,
            kind,
            bytes,
            leaf: kind != Kind::Dir,
        }
    }

    fn card() -> SyncCard {
        SyncCard::new(
            vec![PathBuf::from("/src/photos")],
            PathBuf::from("/dst"),
            "Sync 1 item → /dst".to_string(),
            7,
            Arc::new(Mutex::new(None)),
        )
    }

    #[test]
    fn a_comparing_card_counts_and_cannot_be_run() {
        let mut card = card();
        assert_eq!(card.summary(), "Comparing… 0 so far");
        assert!(card.set_seen(1234));
        assert!(!card.set_seen(1234), "no change, no frame");
        assert_eq!(card.summary(), "Comparing… 1,234 so far");
        assert!(!card.can_commit());
        assert_eq!(card.labels(), ["Cancel", "Sync"]);
    }

    #[test]
    fn a_ready_card_says_what_it_will_do_in_numbers_and_rows() {
        let mut card = card();
        card.land(plan_with(
            vec![
                item("", Class::Unchanged, Kind::Dir, 0),
                item("a.jpg", Class::New, Kind::File, 2048),
                item("b.jpg", Class::Changed, Kind::File, 10),
                item("c.jpg", Class::Unchanged, Kind::File, 5),
            ],
            12,
            3,
            1204,
            8_804_682_137,
        ));
        assert_eq!(
            card.summary(),
            "12 new · 3 changed · 1,204 unchanged · 8.2 GB to copy"
        );
        assert_eq!(
            card.rows(),
            [
                Row {
                    mark: Mark::New,
                    text: "photos/a.jpg".to_string(),
                    detail: "2.0 KB".to_string(),
                },
                Row {
                    mark: Mark::Changed,
                    text: "photos/b.jpg".to_string(),
                    detail: "10 B".to_string(),
                },
            ]
        );
        assert!(card.can_commit());
        assert_eq!(card.labels(), ["Cancel", "Sync"]);
        assert_eq!(card.status(), "Update · verify copied · by size and date");
        card.toggle_verify();
        assert_eq!(
            card.status(),
            "Update · verify everything · by size and date"
        );
        assert_eq!(card.run_verify(), Verify::Everything);
    }

    #[test]
    fn a_plan_with_nothing_to_copy_offers_to_verify() {
        let mut card = card();
        card.land(plan_with(
            vec![
                item("", Class::Unchanged, Kind::Dir, 0),
                item("a.jpg", Class::Unchanged, Kind::File, 5),
            ],
            0,
            0,
            1204,
            0,
        ));
        assert_eq!(card.summary(), "Already in sync · 1,204 files");
        assert_eq!(card.labels(), ["Cancel", "Verify"]);
        assert_eq!(card.run_verify(), Verify::Everything, "whatever `v` says");
        assert!(card.rows().is_empty());
    }

    #[test]
    fn a_long_list_is_capped_and_says_how_many_more() {
        let items = (0..LIST_CAP + 37)
            .map(|i| item(&format!("{i}.jpg"), Class::New, Kind::File, 1))
            .collect();
        let mut card = card();
        card.land(plan_with(items, (LIST_CAP + 37) as u64, 0, 0, 0));
        assert_eq!(card.rows().len(), LIST_CAP + 1);
        assert_eq!(card.rows()[LIST_CAP].text, "… and 37 more");
        assert_eq!(card.rows()[LIST_CAP].mark, Mark::More);

        // The wheel and the arrows scroll it, and stop at either end.
        while card.wheel(-400.0) {}
        assert_eq!(card.first, card.rows().len() - VISIBLE);
        card.scroll_by(-3);
        assert_eq!(card.first, card.rows().len() - VISIBLE - 3);
        card.scroll_by(-10_000);
        assert_eq!(card.first, 0);
    }

    #[test]
    fn comparing_again_keeps_the_card_the_size_it_was() {
        let items = (0..30)
            .map(|i| item(&format!("{i}.jpg"), Class::New, Kind::File, 1))
            .collect();
        let mut card = card();
        card.land(plan_with(items, 30, 0, 0, 30));
        assert_eq!(card.visible(), VISIBLE);
        card.toggle_content();
        card.compare_again(8, Arc::new(Mutex::new(None)));
        assert_eq!(card.visible(), VISIBLE);
        assert!(card.rows().is_empty());
        assert_eq!(card.options(), SyncOptions { content: true });
    }

    #[test]
    fn the_result_card_names_every_problem() {
        let report = SyncReport {
            verify: Verify::Copied,
            copied: 12,
            copied_bytes: 3 * 1024 * 1024,
            made: 1,
            verified: 10,
            verify_failures: vec![(
                PathBuf::from("/dst/photos/b.jpg"),
                "contents differ from the source".to_string(),
            )],
            errors: vec![(
                PathBuf::from("/dst/photos/a.jpg"),
                "Permission denied".to_string(),
            )],
            ..SyncReport::default()
        };
        let card = SyncCard::result(PathBuf::from("/dst"), "Sync 1 item → /dst".into(), report);
        assert_eq!(card.heading(), "Sync finished with 2 problems");
        assert_eq!(card.summary(), "Synced 12 files · 3.0 MB");
        assert_eq!(
            card.rows()
                .iter()
                .map(|row| format!("{} — {}", row.text, row.detail))
                .collect::<Vec<_>>(),
            [
                "photos/a.jpg — Permission denied",
                "photos/b.jpg — contents differ from the source"
            ]
        );
        assert_eq!(card.labels(), ["Close"]);
        assert!(card.hints().iter().all(|hint| hint.keys != "Enter"));
    }

    #[test]
    fn a_finished_run_reads_as_one_line() {
        let mut report = SyncReport {
            verify: Verify::Copied,
            copied: 15,
            copied_bytes: 8_804_682_137,
            verified: 15,
            ..SyncReport::default()
        };
        assert_eq!(outcome(&report), "Synced 15 files · 8.2 GB · verified");
        report.verify = Verify::Everything;
        report.verified = 1219;
        assert_eq!(
            outcome(&report),
            "Synced 15 files · 8.2 GB · 1,219 files verified"
        );
        let verify_only = SyncReport {
            verify: Verify::Everything,
            verified: 1204,
            ..SyncReport::default()
        };
        assert_eq!(
            outcome(&verify_only),
            "Already in sync · 1,204 files verified"
        );
        let cancelled = SyncReport {
            copied: 1,
            copied_bytes: 10,
            cancelled: true,
            ..SyncReport::default()
        };
        assert_eq!(outcome(&cancelled), "Synced 1 file · 10 B · cancelled");
    }

    #[test]
    fn the_hints_say_what_each_switch_will_do() {
        let mut card = card();
        let labels = |card: &SyncCard| {
            card.hints()
                .iter()
                .map(|hint| format!("{} {}", hint.keys, hint.label))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        assert_eq!(
            labels(&card),
            "m mirror · v verify everything · c compare contents · Enter sync · Esc cancel"
        );
        card.toggle_verify();
        card.toggle_content();
        assert_eq!(
            labels(&card),
            "m mirror · v verify copied · c compare size, date · Enter sync · Esc cancel"
        );
    }

    #[test]
    fn focus_is_the_roots_something_was_written_into() {
        let mut plan = plan_with(
            vec![
                item("", Class::Unchanged, Kind::Dir, 0),
                item("a.jpg", Class::New, Kind::File, 1),
            ],
            1,
            0,
            0,
            1,
        );
        plan.roots.push(Root {
            src: PathBuf::from("/src/notes.txt"),
            dst: PathBuf::from("/dst/notes.txt"),
        });
        plan.items.push(Item {
            root: 1,
            rel: PathBuf::new(),
            class: Class::Unchanged,
            kind: Kind::File,
            bytes: 1,
            leaf: true,
        });
        assert_eq!(focus(&plan), [PathBuf::from("/dst/photos")]);
    }

    /// A plan with one new file and three extras, two of them in a folder.
    fn with_extras(removal: Removal) -> SyncPlan {
        let mut plan = plan_with(
            vec![
                item("", Class::Unchanged, Kind::Dir, 0),
                item("a.jpg", Class::New, Kind::File, 5),
                item("old", Class::Extra, Kind::Dir, 0),
                item("old/x.jpg", Class::Extra, Kind::File, 7),
                item("old/y.jpg", Class::Extra, Kind::File, 7),
                item("stray.txt", Class::Extra, Kind::File, 1),
            ],
            1,
            0,
            0,
            5,
        );
        plan.extra = Tally {
            count: 3,
            bytes: 15,
        };
        plan.removal = removal;
        plan
    }

    #[test]
    fn m_turns_an_update_into_a_mirror_that_says_what_it_removes() {
        let mut card = card();
        card.land(with_extras(Removal::Trash));
        assert_eq!(card.mode, Mode::Update);
        assert_eq!(card.removal_line(), None, "an update removes nothing");
        assert_eq!(card.labels(), ["Cancel", "Sync"]);
        assert_eq!(card.rows().len(), 1);

        card.toggle_mode();
        assert_eq!(card.mode, Mode::Mirror);
        assert_eq!(card.removal_line().as_deref(), Some("3 to trash"));
        assert_eq!(card.labels(), ["Cancel", "Sync and trash 3"]);
        assert!(!card.danger(), "the trash is undoable");
        assert!(card.status().starts_with("Mirror · "));
        assert_eq!(
            card.rows()
                .iter()
                .map(|row| (row.mark, row.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (Mark::New, "photos/a.jpg"),
                (Mark::Extra, "photos/old/"),
                (Mark::Extra, "photos/old/x.jpg"),
                (Mark::Extra, "photos/old/y.jpg"),
                (Mark::Extra, "photos/stray.txt"),
            ]
        );
        assert!(card.hints()[0].label == "update only");

        card.toggle_mode();
        assert_eq!(card.rows().len(), 1, "and back, at once");
    }

    #[test]
    fn a_mirror_with_nowhere_to_trash_says_delete_and_its_button_is_red() {
        let mut card = card();
        card.land(with_extras(Removal::Delete));
        card.toggle_mode();
        assert_eq!(card.removal_line().as_deref(), Some("3 to delete"));
        assert_eq!(card.labels(), ["Cancel", "Sync and delete 3"]);
        assert!(card.danger());
    }

    #[test]
    fn a_mirror_with_extras_is_not_in_sync_even_with_nothing_to_copy() {
        let mut plan = with_extras(Removal::Trash);
        plan.items.retain(|item| item.class != Class::New);
        plan.new = Tally::default();
        let mut card = card();
        card.land(plan);
        assert_eq!(
            card.labels(),
            ["Cancel", "Verify"],
            "an update has nothing to do"
        );
        card.toggle_mode();
        assert_eq!(card.labels(), ["Cancel", "Sync and trash 3"]);
        assert_eq!(card.run_verify(), Verify::Copied);
    }

    #[test]
    fn a_server_that_could_not_hash_its_side_is_not_called_verified() {
        let report = SyncReport {
            copied: 3,
            copied_bytes: 30,
            local_only: true,
            ..SyncReport::default()
        };
        assert_eq!(
            outcome(&report),
            "Synced 3 files · 30 B · verified locally only"
        );
    }

    #[test]
    fn a_server_that_was_not_flushed_says_so_after_verified() {
        let report = SyncReport {
            copied: 1,
            copied_bytes: 2,
            verified: 1,
            unflushed: true,
            ..SyncReport::default()
        };
        assert_eq!(
            outcome(&report),
            "Synced 1 file · 2 B · verified · not flushed on the server"
        );
    }

    #[test]
    fn files_that_could_not_be_read_keep_a_run_from_reading_as_clean() {
        let report = SyncReport {
            copied: 3,
            copied_bytes: 30,
            verified: 3,
            skipped: vec![
                (
                    PathBuf::from("/src/photos/locked"),
                    "Permission denied".into(),
                ),
                (
                    PathBuf::from("/src/photos/sock"),
                    "not a file, folder or link".into(),
                ),
            ],
            ..SyncReport::default()
        };
        assert_eq!(
            outcome(&report),
            "Synced 3 files · 30 B · 3 files verified · 2 could not be read"
        );
        // …and the result card, when one is up, names them.
        let card = SyncCard::result(
            PathBuf::from("/dst"),
            "Sync".into(),
            SyncReport {
                errors: vec![(PathBuf::from("/dst/photos/a"), "No space left".into())],
                ..report
            },
        );
        assert_eq!(
            card.rows()
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>(),
            ["photos/a", "/src/photos/locked", "/src/photos/sock"]
        );
    }

    #[test]
    fn a_mirrors_removals_are_in_the_toast() {
        let report = SyncReport {
            mode: Mode::Mirror,
            copied: 2,
            copied_bytes: 10,
            removed: 3,
            removal: Removal::Trash,
            verified: 2,
            ..SyncReport::default()
        };
        assert_eq!(
            outcome(&report),
            "Synced 2 files · 10 B · 3 trashed · verified"
        );
        let only_removed = SyncReport {
            mode: Mode::Mirror,
            removed: 1,
            removal: Removal::Delete,
            ..SyncReport::default()
        };
        assert_eq!(outcome(&only_removed), "Mirrored · 1 deleted");
    }

    /// The confirm card's rules, on this card: the last button a card's
    /// padding in from the right edge and sitting on the hint strip, so its
    /// corner is concentric with the card's; the status stopping short of the
    /// first button; every row inside the list; every hint on the strip.
    #[test]
    fn the_card_is_laid_out_by_the_confirm_cards_rules() {
        let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let items = (0..30)
            .map(|i| item(&format!("{i}.jpg"), Class::New, Kind::File, 1))
            .collect();
        let comparing = card();
        let mut ready = card();
        ready.land(plan_with(items, 30, 0, 0, 30));
        let mut mirror = card();
        mirror.land(with_extras(Removal::Delete));
        mirror.toggle_mode();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            for (card, rows) in [(&comparing, 0), (&ready, VISIBLE), (&mirror, 5)] {
                let g = geometry(ui.painter(), area, card);
                assert_eq!(g.rows.len(), rows);
                // The list starts below every summary line.
                let lines = if card.removal_line().is_some() {
                    3.0
                } else {
                    2.0
                };
                assert!(g.body.top() >= g.card.top() + CARD_PAD + ROW * lines);
                assert!(g.rows.iter().all(|row| g.body.contains_rect(*row)));
                assert!(g.card.contains_rect(g.body));
                let last = *g.actions.last().unwrap();
                assert!((g.card.right() - CARD_PAD - last.right()).abs() < 1e-3);
                let strip = chrome::hint_rect(g.card);
                assert!((strip.top() - last.bottom()).abs() < 1e-3);
                assert!(g.status.right() < g.actions[0].left());
                assert!(g.card.contains_rect(g.close));
                let hints = card.hints();
                let fitted = chrome::hint_rects(ui.painter(), strip, &hints);
                assert_eq!(fitted.len(), hints.len(), "a hint fell off the strip");
            }
        });
    }
}
