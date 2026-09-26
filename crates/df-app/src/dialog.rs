//! The two modal cards a destructive operation goes through: the **confirm**
//! (`D`, and `d` on a server, which has no trash) and the **conflict resolver**
//! (`p` onto a name that is taken). A local `d` asks nothing — the trash is
//! undoable, and its toast says how.
//!
//! Both are modal in the strong sense — while one is up, keystrokes are matched
//! against the `[confirm]` context **alone** and never fall through to the
//! browser's own map. That is deliberate and it is a safety property: with the
//! stack merely *pushed*, a `d` typed into a delete confirmation would still
//! reach `Files` underneath and queue a second trash. A dialog that is asking
//! "are you sure" must not also be a file manager.
//!
//! Everything in this file that decides anything is a pure state machine over
//! df-core's [`PastePlan`]; the painting reads it and draws. That split is what
//! lets "Enter on the last conflict finishes the paste" be a unit test rather
//! than a thing somebody clicks through once.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use df_core::fs::Entry;
use df_core::ops::paste::{Conflict, PastePlan, Resolution};
use df_core::rename::template::{Missing, Template};
use df_core::text::grouped;

use crate::bulk::{Bulk, Focus, Problem};
use crate::chrome::{self, CARD_MARGIN, CARD_PAD, FONT, PAD_X};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting, ROW_RADIUS};

/// A dialog row's height. The same 20 pt every other card list uses.
pub(crate) const ROW: f32 = 20.0;

/// The action buttons' height. Taller than a row: they are the things being
/// pressed, and `delightful-ui` §1 wants a real hit target under them.
pub(crate) const BUTTON_HEIGHT: f32 = 26.0;

/// Between two buttons: the card's padding, so every gap beside a button —
/// the card's edge, its neighbour, the content above — is the same width.
const BUTTON_GAP: f32 = CARD_PAD;

/// The narrowest a button gets. A two-letter verb on a button the width of its
/// word is a target the pointer has to aim for rather than land on.
const BUTTON_MIN_WIDTH: f32 = 64.0;

/// The widest either dialog gets. Past this a two-column comparison stops being
/// a comparison — the eye cannot hold both halves at once.
pub(crate) const MAX_WIDTH: f32 = 620.0;

/// How many body lines a confirm shows before it scrolls. Six names is enough
/// to recognise the set you selected; past that the count in the title is the
/// thing being read anyway.
const BODY_VISIBLE: usize = 6;

/// The narrowest a confirm gets. The card is sized to what it says, and a
/// one-file delete says little — but a card much narrower than this stops
/// reading as a question laid over the window and starts reading as a tooltip.
const CONFIRM_MIN_WIDTH: f32 = 300.0;

/// Between the confirm's title and its first name: close, because the names
/// are what the title's count is counting.
const TITLE_GAP: f32 = 8.0;

/// Between a dialog's content and its buttons: wider than the title's gap,
/// because the buttons are a different kind of thing — the answer, not more
/// of the question — and the card's padding, so the gap above a button is the
/// gap beside it.
pub(crate) const ANSWER_GAP: f32 = CARD_PAD;

/// How many conflicts the resolver lists at once, for the same reason.
const CONFLICT_VISIBLE: usize = 5;

/// The height of one side of the side-by-side comparison.
const FACTS_HEIGHT: f32 = 74.0;

// ── The confirm dialog (PLAN §4.1's `[confirm]` context) ────────────────────

/// Which irreversible thing is being confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    /// A local `d`. **Never shown as a card**: a trash is undoable, and the
    /// undo toast that follows it answers "are you sure" better than a card
    /// asking before anything has happened. `d` builds one of these and hands
    /// it straight to the confirm's own yes, so the trash is carried out on
    /// exactly one path whether anybody was asked or not.
    Trash,
    /// `D`. The one operation with no inverse (PLAN §5).
    Delete,
    /// `d` on a remote service (PLAN §7.6). Its own kind rather than
    /// [`ConfirmKind::Trash`] with different words, because it is a different
    /// promise: **there is no trash on the other machine**, so the title has
    /// to say so before the key lands, not in the toast afterwards.
    RemoteDelete,
    /// `D` inside the trash view (PLAN §7.4): destroy what is already deleted.
    Purge,
    /// "Empty trash", from the palette or the context menu. The scary one.
    EmptyTrash,
    /// A save dialog aimed at a file that is already there (`--chooser-save`).
    /// Nothing is overwritten *here* — the program that asked does that after
    /// the pick — but the pick is the last moment anybody can say no, so this
    /// is where it is asked. Never for the name the dialog itself suggested:
    /// the portal made that file a moment ago, and it is nobody's work.
    Replace,
}

/// A yes/no card over the panes.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub kind: ConfirmKind,
    pub paths: Vec<PathBuf>,
    /// First visible body line — `↑`/`↓` scroll the body, as the `[confirm]`
    /// keymap says, rather than moving a cursor there is nothing to move.
    pub scroll: usize,
    /// How wide the widest name in the body is, measured once.
    ///
    /// The card is as wide as its longest name, and the longest name can be in
    /// a list that is a whole trash: ten thousand names laid out on every frame
    /// is a stall paid for a number that cannot change while the card is up.
    /// Filled the first time the card is laid out, because that is the first
    /// moment there is a painter to measure with.
    name_width: std::cell::OnceCell<f32>,
    /// How many names the card showed when it was last drawn: what the keys
    /// scroll against. Fewer than [`BODY_VISIBLE`] in a window too short for
    /// six ([`Confirm::fit`]).
    shown: usize,
    /// When the names last scrolled, for their bar.
    bar: crate::scrollbar::Linger,
    /// The wheel's roll that has not come to a whole name yet
    /// ([`crate::mouse::roll`]).
    carry: f32,
    /// What emptying the trash frees, for [`ConfirmKind::EmptyTrash`]'s
    /// question: the trash chip's number, `~` and all, kept in step with it by
    /// the app while the card is up. `None` for every other kind, and for a
    /// trash whose walk has said nothing yet.
    pub size: Option<crate::folders::Size>,
}

impl Confirm {
    pub fn new(kind: ConfirmKind, paths: Vec<PathBuf>) -> Confirm {
        Confirm {
            size: None,
            kind,
            paths,
            scroll: 0,
            name_width: std::cell::OnceCell::new(),
            shown: BODY_VISIBLE,
            bar: crate::scrollbar::Linger::default(),
            carry: 0.0,
        }
    }

    /// yazi's custom-body style: the question names the verb, the count and
    /// where the files came from, so the title alone is enough to answer with.
    ///
    /// **The title is the whole message.** There is no line under it: the one
    /// sentence a subtitle used to carry was the word *permanently* (or
    /// *destroy*), and it reads better inside the question than as a second,
    /// smaller one beneath it. A remote delete says it too, because it is the
    /// fact that card exists to put in front of you — there is no trash on the
    /// other machine, so that `d` is not the local one.
    pub fn title(&self) -> String {
        let n = self.paths.len();
        let noun = if n == 1 { "file" } else { "files" };
        let count = grouped(n as u64);
        match self.kind {
            ConfirmKind::Trash => format!("Trash {count} selected {noun}?"),
            ConfirmKind::Delete => format!("Delete {count} selected {noun} permanently?"),
            ConfirmKind::RemoteDelete => format!("Delete {count} remote {noun} permanently?"),
            ConfirmKind::Purge => format!("Destroy {count} trashed {noun}?"),
            // The count *and* the weight, in the trash chip's own words: what
            // is about to go, and what going frees.
            ConfirmKind::EmptyTrash => format!(
                "Empty the trash? {} will be deleted for good.",
                crate::trashview::weight_text(n, self.size)
            ),
            // One file, by name: the question is about *that* file, and a
            // count of one would be the card not saying which.
            ConfirmKind::Replace => format!("Replace {}?", self.body().join(", ")),
        }
    }

    /// How many names the card lists under its title.
    ///
    /// None for a replace, whose title already *is* the name — the same word
    /// twice, once as the question and once underneath it, would read as two
    /// files.
    fn lines(&self) -> usize {
        match self.kind {
            ConfirmKind::Replace => 0,
            _ => self.paths.len().min(BODY_VISIBLE),
        }
    }

    /// The widest body line, in the face the body is drawn in.
    fn name_width(&self, painter: &egui::Painter) -> f32 {
        *self.name_width.get_or_init(|| {
            let font = egui::FontId::proportional(FONT);
            self.body()
                .iter()
                .map(|name| chrome::text_width(painter, name, font.clone()))
                .fold(0.0, f32::max)
        })
    }

    pub fn danger(&self) -> bool {
        !matches!(self.kind, ConfirmKind::Trash)
    }

    /// The names, which is the body the `↑`/`↓` keys scroll.
    pub fn body(&self) -> Vec<String> {
        self.paths
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string())
            })
            .collect()
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.paths.len().saturating_sub(self.shown);
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, max as isize) as usize;
    }

    /// The card was laid out showing `shown` names: the keys scroll against
    /// that from now on, and a scroll a taller window allowed is brought back
    /// so the last name still ends the list rather than a gap.
    pub fn fit(&mut self, shown: usize, now: std::time::Instant) {
        self.shown = shown;
        self.scroll_by(0);
        self.bar.saw(self.scroll as f32, now);
    }

    /// When the names last scrolled, for their bar's linger.
    pub fn scrolled_at(&self) -> Option<std::time::Instant> {
        self.bar.scrolled_at()
    }

    /// A hand let go of the bar: it lingers from now
    /// ([`crate::scrollbar::Linger::let_go`]).
    pub fn let_go(&mut self, now: std::time::Instant) {
        self.bar.let_go(now);
    }

    /// The wheel over the card, in points: whole names at a time
    /// ([`crate::mouse::roll`]). Returns whether the names moved.
    pub fn wheel(&mut self, points: f32, now: std::time::Instant) -> bool {
        let rows = crate::mouse::wheel_rows(points, ROW);
        let last = self.paths.len().saturating_sub(self.shown);
        let first = crate::mouse::roll(self.scroll, last, &mut self.carry, rows);
        self.scroll_to(first, now)
    }

    /// Start the names at `first`, kept inside the list. Returns whether they
    /// moved.
    pub fn scroll_to(&mut self, first: usize, now: std::time::Instant) -> bool {
        let first = first.min(self.paths.len().saturating_sub(self.shown));
        if first == self.scroll {
            return false;
        }
        self.scroll = first;
        self.bar.saw(first as f32, now);
        true
    }
}

// ── The conflict dialog (PLAN §5) ───────────────────────────────────────────

/// What to do about one taken name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    Overwrite,
    Skip,
    Rename,
}

impl ConflictAction {
    pub const ALL: [ConflictAction; 3] = [
        ConflictAction::Overwrite,
        ConflictAction::Skip,
        ConflictAction::Rename,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConflictAction::Overwrite => "Overwrite",
            ConflictAction::Skip => "Skip",
            ConflictAction::Rename => "Rename",
        }
    }

    /// The key that picks it directly, shown on the button. Keyboard-first
    /// (PLAN §5) means the pointer is the second way in, not the first.
    pub fn key(self) -> char {
        match self {
            ConflictAction::Overwrite => 'o',
            ConflictAction::Skip => 's',
            ConflictAction::Rename => 'r',
        }
    }

    fn resolution(self, suggested: &Path) -> Resolution {
        match self {
            ConflictAction::Overwrite => Resolution::Overwrite,
            ConflictAction::Skip => Resolution::Skip,
            ConflictAction::Rename => {
                Resolution::Rename(PathBuf::from(suggested.file_name().unwrap_or_default()))
            }
        }
    }
}

/// What the caller must do after an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Still conflicts to answer — keep the dialog up.
    Continue,
    /// Ask for a name, pre-filled with this one, then feed it back through
    /// [`ConflictDialog::rename`].
    NeedName(String),
    /// Everything is answered: run the plan.
    Settled,
}

/// The facts shown for one side of the comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub mtime: String,
    /// `None` when the path could not be read at all — a broken symlink, a race
    /// with something else deleting it. Shown as "missing" rather than as
    /// zeroes, which would read as an empty file.
    pub present: bool,
    entry: Option<Entry>,
}

impl Facts {
    fn read(path: &Path) -> Facts {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        match Entry::read(path) {
            Ok(entry) => Facts {
                name,
                size: entry.len,
                is_dir: entry.is_dir(),
                mtime: crate::format::linemode_text(&entry, df_core::config::LineMode::Mtime),
                present: true,
                entry: Some(entry),
            },
            Err(_) => Facts {
                name,
                size: 0,
                is_dir: false,
                mtime: String::new(),
                present: false,
                entry: None,
            },
        }
    }

    /// Facts about a row this program already has an [`Entry`] for.
    ///
    /// A remote destination's stat came off the wire — `Facts::read` would run
    /// `Entry::read` on an `sftp://…` string, find nothing, and draw the file
    /// that is about to be overwritten as **missing**, which is the one lie a
    /// conflict card must never tell.
    pub fn of(entry: Entry) -> Facts {
        Facts {
            name: entry.name.clone(),
            size: entry.len,
            is_dir: entry.is_dir(),
            mtime: crate::format::linemode_text(&entry, df_core::config::LineMode::Mtime),
            present: true,
            entry: Some(entry),
        }
    }

    pub fn size_text(&self) -> String {
        if !self.present {
            return "missing".to_string();
        }
        if self.is_dir {
            return "folder".to_string();
        }
        crate::format::human_size(self.size)
    }
}

/// The modal that resolves a paste's name collisions.
pub struct ConflictDialog {
    pub plan: PastePlan,
    /// Which conflict the `↑`/`↓` keys are on.
    pub cursor: usize,
    pub action: ConflictAction,
    /// "Apply to all": the answer given to this conflict answers every
    /// remaining one.
    pub apply_all: bool,
    /// A rename that was refused, shown inline under the field (PLAN §5's
    /// bare-filename rule) rather than as a toast — the error is *about* the
    /// thing being typed, so it belongs next to it.
    pub error: Option<String>,
    /// Metadata, read once per path. A dialog that stats on every frame is a
    /// dialog that hits the disk sixty times a second.
    facts: HashMap<PathBuf, Facts>,
    /// How many taken names the card listed when it was last drawn: what the
    /// list scrolls by. Fewer than [`CONFLICT_VISIBLE`] in a window too short
    /// for five ([`ConflictDialog::fit`]).
    shown: usize,
    /// When the list last scrolled, for its bar.
    bar: crate::scrollbar::Linger,
    /// The wheel's roll that has not come to a whole name yet
    /// ([`crate::mouse::roll`]).
    carry: f32,
    /// Where the wheel left the list, off the cursor, until a key or a click
    /// moves the cursor: the panes' rule ([`crate::tab::Listing::attach`]).
    /// `None` is the list following the cursor
    /// ([`ConflictDialog::first_visible`]).
    detached: Option<usize>,
}

impl ConflictDialog {
    pub fn new(plan: PastePlan) -> ConflictDialog {
        ConflictDialog::with_facts(plan, HashMap::new())
    }

    /// The same dialog, with the facts for some of its paths already known.
    ///
    /// What an **upload** needs (PLAN §7.6): the destinations are `sftp://…`
    /// display paths, whose sizes and dates arrived in the probe's stat replies
    /// and cannot be read from this machine. Seeded first, so [`Facts::read`]
    /// is only ever asked about the paths that really are local — the sources.
    pub fn with_facts(plan: PastePlan, facts: HashMap<PathBuf, Facts>) -> ConflictDialog {
        let mut dialog = ConflictDialog {
            plan,
            cursor: 0,
            action: ConflictAction::Rename,
            apply_all: false,
            error: None,
            facts,
            shown: CONFLICT_VISIBLE,
            bar: crate::scrollbar::Linger::default(),
            carry: 0.0,
            detached: None,
        };
        dialog.load_facts();
        dialog
    }

    /// Read the facts for whichever conflict is under the cursor. Lazy, and
    /// only for the pair on screen.
    fn load_facts(&mut self) {
        let Some(conflict) = self.plan.conflicts.get(self.cursor).cloned() else {
            return;
        };
        for path in [conflict.src, conflict.dst] {
            self.facts
                .entry(path.clone())
                .or_insert_with(|| Facts::read(&path));
        }
    }

    pub fn conflict(&self) -> Option<&Conflict> {
        self.plan.conflicts.get(self.cursor)
    }

    pub fn facts_for(&self, path: &Path) -> Option<&Facts> {
        self.facts.get(path)
    }

    pub fn len(&self) -> usize {
        self.plan.conflicts.len()
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.plan.conflicts.is_empty() {
            return;
        }
        let last = self.plan.conflicts.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.error = None;
        self.detached = None;
        self.load_facts();
    }

    /// `←`/`→` and Tab walk the three answers; the letters pick one outright.
    pub fn cycle_action(&mut self, delta: isize) {
        let at = ConflictAction::ALL
            .iter()
            .position(|a| *a == self.action)
            .unwrap_or(0) as isize;
        let n = ConflictAction::ALL.len() as isize;
        let next = (at + delta).rem_euclid(n) as usize;
        self.action = ConflictAction::ALL[next];
    }

    pub fn set_action(&mut self, action: ConflictAction) {
        self.action = action;
    }

    /// The letter keys, if this is one of them.
    pub fn action_for_key(key: char) -> Option<ConflictAction> {
        ConflictAction::ALL.into_iter().find(|a| a.key() == key)
    }

    pub fn toggle_apply_all(&mut self) {
        self.apply_all = !self.apply_all;
    }

    /// Enter: answer the conflict under the cursor with the selected action.
    pub fn apply(&mut self) -> Step {
        self.error = None;
        let Some(conflict) = self.conflict().cloned() else {
            return Step::Settled;
        };
        // A single typed name cannot answer many conflicts, so "rename all"
        // means "keep both, auto-named" — df-core's `resolve_all` does exactly
        // that, and asking for one name here would be a lie about what happens.
        if self.apply_all {
            self.plan
                .resolve_all(&self.action.resolution(&conflict.suggested));
            return Step::Settled;
        }
        if self.action == ConflictAction::Rename {
            let suggested = conflict
                .suggested
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            return Step::NeedName(suggested);
        }
        // Overwrite and Skip cannot fail: only a rename carries a name that
        // could be refused.
        let _ = self
            .plan
            .resolve(&conflict.src, &self.action.resolution(&conflict.suggested));
        self.settle()
    }

    /// The typed name came back. `Err` leaves the conflict unanswered and the
    /// message on screen, so the field can be corrected and submitted again.
    pub fn rename(&mut self, name: &str) -> Step {
        let Some(conflict) = self.conflict().cloned() else {
            return Step::Settled;
        };
        match self
            .plan
            .resolve(&conflict.src, &Resolution::Rename(PathBuf::from(name)))
        {
            Ok(()) => {
                self.error = None;
                self.settle()
            }
            Err(e) => {
                self.error = Some(e.to_string());
                Step::NeedName(name.to_string())
            }
        }
    }

    /// After an answer: clamp the cursor and say whether anything is left.
    fn settle(&mut self) -> Step {
        if self.plan.is_settled() {
            return Step::Settled;
        }
        if self.cursor >= self.plan.conflicts.len() {
            self.cursor = self.plan.conflicts.len() - 1;
        }
        self.load_facts();
        // The answered name is gone from the list, which can move the view
        // without anybody scrolling it ([`crate::scrollbar::Linger`]).
        self.bar = crate::scrollbar::Linger::default();
        Step::Continue
    }

    /// Where the resolver's first visible conflict row is, so a long list
    /// scrolls with the cursor instead of hiding it — or where the wheel left
    /// it, kept inside a list an answer may have made shorter.
    pub fn first_visible(&self) -> usize {
        match self.detached {
            Some(first) => first.min(self.len().saturating_sub(self.shown)),
            None => self.cursor.saturating_sub(self.shown.saturating_sub(1)),
        }
    }

    /// The wheel over the card, in points: whole names at a time
    /// ([`crate::mouse::roll`]), the list leaving the cursor where it was.
    /// Returns whether the names moved.
    pub fn wheel(&mut self, points: f32, now: std::time::Instant) -> bool {
        let rows = crate::mouse::wheel_rows(points, ROW);
        let last = self.len().saturating_sub(self.shown);
        let first = crate::mouse::roll(self.first_visible(), last, &mut self.carry, rows);
        self.scroll_to(first, now)
    }

    /// Start the list at `first`, kept inside it, off the cursor until a key
    /// or a click moves it. Returns whether the names moved.
    pub fn scroll_to(&mut self, first: usize, now: std::time::Instant) -> bool {
        let first = first.min(self.len().saturating_sub(self.shown));
        if first == self.first_visible() {
            return false;
        }
        self.detached = Some(first);
        self.bar.saw(first as f32, now);
        true
    }

    /// The card was laid out listing `shown` names: the list scrolls by that
    /// from now on, so a cursor in a short window never walks off its end.
    pub fn fit(&mut self, shown: usize, now: std::time::Instant) {
        self.shown = shown;
        self.bar.saw(self.first_visible() as f32, now);
    }

    /// When the list last scrolled, for its bar's linger.
    pub fn scrolled_at(&self) -> Option<std::time::Instant> {
        self.bar.scrolled_at()
    }

    /// A hand let go of the bar: it lingers from now
    /// ([`crate::scrollbar::Linger::let_go`]).
    pub fn let_go(&mut self, now: std::time::Instant) {
        self.bar.let_go(now);
    }
}

// ── Geometry, shared by the paint and the hit test ──────────────────────────

/// Where a dialog's pieces are. One function, two callers — a click has to land
/// on the button it looks like it landed on.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    pub card: egui::Rect,
    /// The scrollable body, whatever the body is.
    pub body: egui::Rect,
    /// One rect per listed row, in the order they are drawn.
    pub rows: Vec<egui::Rect>,
    /// The action buttons, left to right.
    pub actions: Vec<egui::Rect>,
    /// The apply-to-all toggle, when the dialog has one.
    pub apply_all: Option<egui::Rect>,
    /// The `×` in the top-right corner, on a card with no `Cancel` of its own.
    pub close: Option<egui::Rect>,
    /// The band the names' bar is pointed at by, while there are more names
    /// than rows ([`crate::scrollbar::band`]).
    pub band: Option<egui::Rect>,
}

impl Geometry {
    /// The action index a point is over, if any.
    pub fn action_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.actions.iter().position(|r| r.contains(pos))
    }

    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(pos))
    }
}

/// A card of `width` × `height`, centred horizontally and biased *above* true
/// centre — `delightful-ui` §16: content centred in a big region reads as
/// sitting low. Never wider or taller than the window can hold with its
/// margins, whatever was asked for; any narrower cap, [`MAX_WIDTH`] or the
/// rename card's own, is the caller's.
pub(crate) fn place_card(area: egui::Rect, width: f32, height: f32) -> egui::Rect {
    let width = width.min(area.width() - CARD_MARGIN * 2.0).max(0.0);
    let height = height.min((area.height() - CARD_MARGIN * 2.0).max(0.0));
    // 40 % of the free space above, 60 % below.
    let top = area.top() + (area.height() - height).max(0.0) * chrome::OPTICAL_CENTRE;
    egui::Rect::from_min_size(
        egui::pos2(area.center().x - width / 2.0, top),
        egui::vec2(width, height),
    )
}

/// How much of the window's height a card has for its rows, in points: the
/// window less a [`CARD_MARGIN`] above and below, less the card's `fixed`
/// parts — its padding, its heading, its answers, its hint strip.
pub(crate) fn room(area: egui::Rect, fixed: f32) -> f32 {
    (area.height() - CARD_MARGIN * 2.0 - fixed).max(0.0)
}

/// How many of `wanted` rows a card shows, and the card that shows them.
///
/// The card is `fixed` points of everything that is not a row and `row`
/// points a row, `width` wide (the caller's to cap), placed by [`place_card`]
/// and so never taller than the window allows. As many rows as the
/// window has [`room`] for, up to `wanted`, and never none while there is one
/// to show: a window too short for a row and the rest still gets that row,
/// with the card clipped to the window, because a list showing nothing reads
/// as a list with nothing in it.
///
/// The rename card ([`bulk_geometry`]) was the first card to fit its rows to
/// the window, and this is its rule, for every card with a list.
pub(crate) fn fit_rows(
    area: egui::Rect,
    width: f32,
    fixed: f32,
    row: f32,
    wanted: usize,
) -> (usize, egui::Rect) {
    let fits = ((room(area, fixed) / row).floor() as usize).max(1);
    let rows = wanted.min(fits);
    (rows, place_card(area, width, fixed + rows as f32 * row))
}

/// The confirm's title face: a step over the body, because the title is the
/// question and the names are only what it is about.
pub(crate) fn title_font() -> egui::FontId {
    egui::FontId::proportional(FONT + 2.0)
}

/// Lay out the confirm card: the question, the names, the two answers.
///
/// **Sized to what it says**, which needs real measurement — hence the
/// painter, and hence one call per frame shared by the hit test and the paint
/// rather than one each, so both see the same widths. As wide as the widest of
/// the title, the longest name and the button row, plus the padding either
/// side; never under [`CONFIRM_MIN_WIDTH`], never over [`MAX_WIDTH`] or the
/// window. As tall as its names need, up to [`BODY_VISIBLE`] of them and up
/// to what the window leaves room for ([`fit_rows`]).
///
/// Top to bottom: the pad, the title, [`TITLE_GAP`], one [`ROW`] per visible
/// name, [`ANSWER_GAP`], the buttons, the pad. The title and the names share
/// one left edge, the card's padding. The buttons sit exactly [`CARD_PAD`] in
/// from the card's right and bottom edges, and since the card's radius is
/// [`chrome::CARD_RADIUS`] = [`ROW_RADIUS`] + [`CARD_PAD`] and a button's is
/// [`ROW_RADIUS`], the last button's corner is concentric with the card's —
/// the gap between them is the same width all the way round the curve
/// (`delightful-ui` §15).
pub fn confirm_geometry(painter: &egui::Painter, area: egui::Rect, confirm: &Confirm) -> Geometry {
    let lines = confirm.lines();
    let title = painter
        .layout_no_wrap(confirm.title(), title_font(), egui::Color32::WHITE)
        .size();
    // Cancel first, the committing button last and rightmost: the destructive
    // one is the furthest from where the pointer rests after opening the card.
    let labels = ["Cancel", confirm_verb(confirm.kind)];
    let buttons = labels
        .iter()
        .map(|label| button_width(painter, label))
        .sum::<f32>()
        + BUTTON_GAP * (labels.len() - 1) as f32;
    let content = title.x.max(confirm.name_width(painter)).max(buttons);
    let width = (content + CARD_PAD * 2.0).max(CONFIRM_MIN_WIDTH);
    // As many names as fit between the title and the buttons, so a short
    // window scrolls them rather than drawing them down over the answers.
    let fixed = CARD_PAD + title.y + TITLE_GAP + ANSWER_GAP + BUTTON_HEIGHT + CARD_PAD;
    let (lines, card) = fit_rows(area, width.min(MAX_WIDTH), fixed, ROW, lines);
    let inner_left = card.left() + CARD_PAD;
    let inner_right = card.right() - CARD_PAD;
    let body_top = card.top() + CARD_PAD + title.y + TITLE_GAP;
    let body = egui::Rect::from_min_max(
        egui::pos2(inner_left, body_top),
        egui::pos2(inner_right, body_top + lines as f32 * ROW),
    );
    let rows = (0..lines)
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(inner_left, body_top + i as f32 * ROW),
                egui::vec2(body.width(), ROW),
            )
        })
        .collect();
    let actions = button_row(
        painter,
        inner_right,
        card.bottom() - CARD_PAD - BUTTON_HEIGHT,
        &labels,
    );
    let band = crate::scrollbar::band(card, body, lines as f32, confirm.paths.len() as f32);
    Geometry {
        card,
        body,
        rows,
        actions,
        apply_all: None,
        close: None,
        band,
    }
}

pub fn confirm_verb(kind: ConfirmKind) -> &'static str {
    match kind {
        ConfirmKind::Trash => "Trash",
        ConfirmKind::Delete => "Delete",
        ConfirmKind::RemoteDelete => "Delete",
        ConfirmKind::Purge => "Destroy",
        ConfirmKind::EmptyTrash => "Empty",
        ConfirmKind::Replace => "Replace",
    }
}

/// Lay out the conflict card.
pub fn conflict_geometry(
    painter: &egui::Painter,
    area: egui::Rect,
    dialog: &ConflictDialog,
) -> Geometry {
    // Everything but the names, which get what the window leaves above the
    // comparison and the answers: a short window scrolls the names rather
    // than sliding the comparison down over the buttons.
    let fixed = CARD_PAD * 2.0
        + ROW * 2.0                       // title and subtitle
        + 6.0
        + 10.0
        + FACTS_HEIGHT
        + ANSWER_GAP
        + BUTTON_HEIGHT
        + chrome::HINT_ROW; // the card's own hint strip
    let wanted = dialog.len().min(CONFLICT_VISIBLE);
    let (listed, card) = fit_rows(area, MAX_WIDTH, fixed, ROW, wanted);
    let inner_left = card.left() + CARD_PAD;
    let inner_right = card.right() - CARD_PAD;
    let body_top = card.top() + CARD_PAD + ROW * 2.0 + 6.0;
    let rows: Vec<egui::Rect> = (0..listed)
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(inner_left, body_top + i as f32 * ROW),
                egui::vec2(inner_right - inner_left, ROW),
            )
        })
        .collect();
    let body = egui::Rect::from_min_max(
        egui::pos2(inner_left, body_top),
        egui::pos2(inner_right, body_top + listed as f32 * ROW),
    );
    let buttons_top = card.bottom() - CARD_PAD - chrome::HINT_ROW - BUTTON_HEIGHT;
    let labels: Vec<&str> = ConflictAction::ALL.iter().map(|a| a.label()).collect();
    let actions = button_row(painter, inner_right, buttons_top, &labels);
    // The toggle sits on the left of the same line as the buttons: it modifies
    // what pressing one of them means, so it must be read before them.
    let apply_all = Some(egui::Rect::from_min_size(
        egui::pos2(inner_left, buttons_top),
        egui::vec2(110.0, BUTTON_HEIGHT),
    ));
    Geometry {
        card,
        body,
        band: crate::scrollbar::band(card, body, listed as f32, dialog.len() as f32),
        rows,
        actions,
        apply_all,
        // Overwrite, Skip and Rename all answer the question, and none of
        // them backs out of the paste, so the card carries its own way out.
        close: Some(chrome::close_button_rect(card)),
    }
}

/// How wide a button with `label` on it is: the word, measured in the face
/// [`button`] draws it in, with a bar's padding either side — and never under
/// [`BUTTON_MIN_WIDTH`].
///
/// Measured rather than guessed from the character count. A guess per
/// character is right for none of them: `Destroy` and `Empty` are the same
/// seven-points-a-letter to a guess and visibly different widths on screen, and
/// a card sized to its content has to be sized to the content it really draws.
fn button_width(painter: &egui::Painter, label: &str) -> f32 {
    (chrome::text_width(painter, label, egui::FontId::proportional(FONT)) + PAD_X * 2.0)
        .max(BUTTON_MIN_WIDTH)
}

/// Buttons laid right-to-left from `right`, returned left-to-right.
pub(crate) fn button_row(
    painter: &egui::Painter,
    right: f32,
    top: f32,
    labels: &[&str],
) -> Vec<egui::Rect> {
    let mut rects = Vec::new();
    let mut x = right;
    for label in labels.iter().rev() {
        let width = button_width(painter, label);
        rects.push(egui::Rect::from_min_size(
            egui::pos2(x - width, top),
            egui::vec2(width, BUTTON_HEIGHT),
        ));
        x -= width + BUTTON_GAP;
    }
    rects.reverse();
    rects
}

/// The confirm's bar, beside its names, while it lists fewer than it has.
pub fn names_bar(geometry: &Geometry, confirm: &Confirm) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        geometry.card,
        geometry.body,
        confirm.scroll as f32,
        geometry.rows.len() as f32,
        confirm.paths.len() as f32,
    )
}

/// The resolver's bar, beside its taken names, while it lists fewer than it
/// has.
pub fn conflicts_bar(
    geometry: &Geometry,
    dialog: &ConflictDialog,
) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        geometry.card,
        geometry.body,
        dialog.first_visible() as f32,
        geometry.rows.len() as f32,
        dialog.len() as f32,
    )
}

// ── Painting ────────────────────────────────────────────────────────────────

/// Draw the confirm card, over the scrim the app lays for it.
pub fn paint_confirm(
    paint: &Painting<'_>,
    confirm: &Confirm,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    chrome::card(paint, geometry.card, 1.0);

    // The title fills the band the geometry left above the names: from the
    // card's top padding down to the gap before the first name. The same left
    // edge as the names, so the card reads as one column.
    let title = egui::Rect::from_min_max(
        egui::pos2(
            geometry.card.left() + CARD_PAD,
            geometry.card.top() + CARD_PAD,
        ),
        egui::pos2(
            geometry.card.right() - CARD_PAD,
            geometry.body.top() - TITLE_GAP,
        ),
    );
    chrome::truncated_in(
        painter,
        egui::pos2(title.left(), title.center().y),
        &confirm.title(),
        palette.text,
        title.width().max(0.0),
        title_font(),
    );

    let body = confirm.body();
    // How many names are below the last visible row, and the marker saying so.
    // Measured before the names are drawn, because the last row's name has to
    // stop short of it rather than run underneath.
    let more = body
        .len()
        .saturating_sub(geometry.rows.len() + confirm.scroll);
    let marker = (more > 0).then(|| {
        painter.layout_no_wrap(
            format!("+{} more", grouped(more as u64)),
            egui::FontId::proportional(FONT - 1.0),
            palette.overlay0,
        )
    });
    let clipped = painter.with_clip_rect(geometry.body);
    let last = geometry.rows.len().saturating_sub(1);
    for (i, rect) in geometry.rows.iter().enumerate() {
        let Some(line) = body.get(confirm.scroll + i) else {
            break;
        };
        let room = match &marker {
            Some(marker) if i == last => rect.width() - marker.size().x - PAD_X,
            _ => rect.width(),
        };
        chrome::truncated(
            &clipped,
            egui::pos2(rect.left(), rect.center().y),
            line,
            palette.subtext0,
            room.max(0.0),
        );
    }
    if let (Some(marker), Some(row)) = (marker, geometry.rows.last()) {
        painter.galley(
            egui::pos2(
                row.right() - marker.size().x,
                row.center().y - marker.size().y / 2.0,
            ),
            marker,
            palette.overlay0,
        );
    }
    if let Some(bar) = names_bar(geometry, confirm) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Confirm,
            hovers,
            confirm.scrolled_at(),
            1.0,
        );
    }

    let labels = ["Cancel", confirm_verb(confirm.kind)];
    for (i, rect) in geometry.actions.iter().enumerate() {
        let danger = i == 1 && confirm.danger();
        button(
            paint,
            *rect,
            labels[i],
            i == 1,
            danger,
            Control::Action(i),
            hovers,
            ripples,
        );
    }
}

// ── Bulk rename (PLAN §5) ───────────────────────────────────────────────────

/// The gap between the two columns of the diff, where the arrow goes.
const BULK_ARROW: f32 = 22.0;
/// The old-name column's share of the list's width.
const BULK_SPLIT: f32 = 0.42;
/// One editable line's height. Taller than a body row: it is a *field*, and a
/// field the caret sits in needs room around the text (`delightful-ui` §1).
pub const BULK_ROW: f32 = 24.0;
/// The widest the rename card gets. Past [`MAX_WIDTH`] on purpose: the other
/// cards here are questions, and this one is a workspace. Two columns of long
/// names side by side need the room, and a name cut off with `…` is a name
/// that cannot be checked.
const BULK_MAX_WIDTH: f32 = 960.0;
/// Between the title and the template field.
const BULK_FIELD_GAP: f32 = 8.0;
/// Between the template field and the first row: a little more than above it,
/// because the field is the rule and the rows are what it made.
const BULK_LIST_GAP: f32 = 10.0;
/// The template field's label.
const TEMPLATE_LABEL: &str = "Name";
/// The `{` popover's width, before the window has a say.
const POPOVER_WIDTH: f32 = 520.0;
/// The padding around the popover's rows. Its corner radius is a row's plus
/// this, so a highlighted row's corner is concentric with the plate's
/// (`delightful-ui` §15).
const POPOVER_PAD: f32 = 4.0;
/// How far the popover hangs clear of the line it completes.
const POPOVER_OFFSET: f32 = 2.0;
/// How much of a popover row the inserted text may take before the detail
/// beside it gets the rest.
const POPOVER_INSERT_SHARE: f32 = 0.42;
/// …and the preview at the row's far end.
const POPOVER_PREVIEW_SHARE: f32 = 0.3;

/// One visible row of the rename card, measured.
#[derive(Debug, Clone, PartialEq)]
pub struct BulkRowGeom {
    /// Which row of the card this is, counting the ones scrolled off.
    pub index: usize,
    /// The whole row.
    pub rect: egui::Rect,
    /// The old name's column.
    pub old: egui::Rect,
    /// The new name's plate: what a press lands on.
    pub new: egui::Rect,
    /// Where the new name is drawn and clipped: the plate less its padding and
    /// less the refusal at its right end, when there is one.
    pub text: egui::Rect,
    /// The x of the new name's first character, scrolled.
    pub origin: f32,
    /// What is wrong with the row, measured into `text`'s width.
    pub problem: Option<crate::bulk::Problem>,
}

/// The `{` popover, measured.
#[derive(Debug, Clone, PartialEq)]
pub struct PopoverGeom {
    pub card: egui::Rect,
    /// The candidate rows drawn, top to bottom.
    pub rows: Vec<egui::Rect>,
    /// The candidate the first row shows.
    pub first: usize,
}

/// Where the rename card's pieces are: the one measurement the hit test, the
/// press and the paint all read.
#[derive(Debug, Clone, PartialEq)]
pub struct BulkGeometry {
    pub card: egui::Rect,
    /// The `×`, top right.
    pub close: egui::Rect,
    /// The template field's plate.
    pub template: egui::Rect,
    /// Where its text is drawn and clipped, after the label.
    pub template_text: egui::Rect,
    /// The x of the template's first character, scrolled.
    pub template_origin: f32,
    /// How far the template is scrolled, for the card to keep.
    pub template_scroll: f32,
    /// The list, every visible row's rect inside it.
    pub body: egui::Rect,
    pub rows: Vec<BulkRowGeom>,
    /// How far the primary caret's row is scrolled, for the card to keep.
    pub row_scroll: f32,
    /// The list's scrollbar, when it has more rows than it shows.
    pub bar: Option<crate::scrollbar::Geometry>,
    /// Cancel, Rename.
    pub actions: Vec<egui::Rect>,
    /// Where the status goes: the rest of the buttons' line, from the card's
    /// padding to a [`GAP`](crate::ui::GAP) short of `Cancel`.
    pub status: egui::Rect,
    pub popover: Option<PopoverGeom>,
    /// How many rows the list has room for.
    pub visible: usize,
}

impl BulkGeometry {
    /// What the pointer is over. The popover first, because it is drawn over
    /// everything else on the card; a press on its padding is the popover's,
    /// and lands on nothing under it.
    pub fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        if let Some(popover) = &self.popover {
            if popover.card.contains(pos) {
                return popover
                    .rows
                    .iter()
                    .position(|rect| rect.contains(pos))
                    .map(|i| Control::BulkCandidate(popover.first + i));
            }
        }
        if self.close.contains(pos) {
            return Some(Control::Close);
        }
        if let Some(i) = self.actions.iter().position(|rect| rect.contains(pos)) {
            return Some(Control::Action(i));
        }
        if self.bar.is_some_and(|bar| bar.contains(pos)) {
            return Some(Control::Bar(crate::scrollbar::Bar::Bulk));
        }
        if self.template.contains(pos) {
            return Some(Control::BulkTemplate);
        }
        self.rows
            .iter()
            .position(|row| row.new.contains(pos))
            .map(Control::BulkRow)
    }

    /// Where a control was drawn, for the ripple to start from.
    pub fn rect_of(&self, control: Control) -> Option<egui::Rect> {
        match control {
            Control::Close => Some(self.close),
            Control::Action(i) => self.actions.get(i).copied(),
            Control::BulkTemplate => Some(self.template),
            Control::BulkRow(i) => self.rows.get(i).map(|row| row.new),
            Control::Bar(crate::scrollbar::Bar::Bulk) => self.bar.map(|bar| bar.hit),
            Control::BulkCandidate(i) => self
                .popover
                .as_ref()
                .and_then(|popover| popover.rows.get(i.checked_sub(popover.first)?).copied()),
            _ => None,
        }
    }

    /// The measured row that shows row `index` of the card, if it is on screen.
    pub fn row(&self, index: usize) -> Option<&BulkRowGeom> {
        self.rows.iter().find(|row| row.index == index)
    }
}

/// The face every name on the card is set in: the template, both columns,
/// and what the `{` list inserts and makes.
///
/// Monospace, because the card is read down its columns as much as along its
/// rows. A stack of carets at column 12 is a vertical line only when column 12
/// is at the same x on every row, and in a proportional face it is not: an
/// `m` and an `i` before it put it in two places, so forty carets that are all
/// in the same place in the text looked scattered. The same goes for the diff.
/// The old name and the new one are meant to be compared letter by letter, and
/// in one face with one advance the letters that did not change sit at the
/// same offset in both columns. The words about the names (the label, the
/// title, the status, the reasons and the buttons) stay in the proportional
/// face, which is what marks them as words rather than names.
///
/// egui's own monospace family at the card's size, as every other monospace
/// run in the app is.
fn bulk_font() -> egui::FontId {
    egui::FontId::monospace(FONT)
}

/// A line laid out in [`bulk_font`], uncoloured: what every caret, selection
/// and click on the card is measured against.
fn bulk_galley(painter: &egui::Painter, text: &str) -> std::sync::Arc<egui::Galley> {
    painter.layout_no_wrap(text.to_string(), bulk_font(), egui::Color32::PLACEHOLDER)
}

/// The x of char boundary `col` along a laid-out line.
fn x_of(galley: &egui::Galley, col: usize) -> f32 {
    galley.pos_from_cursor(egui::text::CCursor::new(col)).min.x
}

/// The char boundary nearest `x` in `text` drawn from `x_origin`: where a click
/// puts the caret and how far a drag has reached. Asked at the line's own
/// height, so a pointer a little above or below the row still reads along it.
pub fn bulk_col_at(painter: &egui::Painter, text: &str, x_origin: f32, x: f32) -> usize {
    let galley = bulk_galley(painter, text);
    let y = galley.size().y / 2.0;
    let cursor = galley.cursor_from_pos(egui::vec2(x - x_origin, y));
    cursor.index.0.min(text.chars().count())
}

/// The character under `x`, for a double click: the one whose box `x` is in,
/// so a click on the right half of a letter is still on that letter.
pub fn bulk_char_at(painter: &egui::Painter, text: &str, x_origin: f32, x: f32) -> usize {
    let at = bulk_col_at(painter, text, x_origin, x);
    let galley = bulk_galley(painter, text);
    if at > 0 && x < x_origin + x_of(&galley, at) {
        at - 1
    } else {
        at
    }
}

/// Lay out the bulk-rename card.
///
/// As wide as the window allows up to [`BULK_MAX_WIDTH`], and as tall as its
/// rows need up to what the window can hold, which is also how many rows the
/// list shows at once ([`BulkGeometry::visible`]). Top to bottom: the pad, the
/// title, the template field, the rows, [`ANSWER_GAP`], the status and the
/// buttons on one line, the pad. No hint strip: the card's keys are an
/// editor's, and the status says the one thing that matters, which is what
/// `Enter` will do.
///
/// The status is on the buttons' line rather than under the title because it
/// is about them: "3 files will be renamed" is what `Rename` does, and "fix
/// the names in red to continue" is why it is veiled. Read beside the button,
/// it answers the question at the moment the hand is on its way there. The
/// line under the title it used to have goes to the list, one more name.
///
/// The buttons sit exactly [`CARD_PAD`] in from the card's right and bottom
/// edges, so the last one's corner is concentric with the card's.
pub fn bulk_geometry(painter: &egui::Painter, area: egui::Rect, bulk: &Bulk) -> BulkGeometry {
    let count = bulk.len();
    let fixed = CARD_PAD * 2.0
        + ROW // the title
        + BULK_FIELD_GAP
        + BULK_ROW // the template
        + BULK_LIST_GAP
        + ANSWER_GAP
        + BUTTON_HEIGHT;
    let room = (area.height() - CARD_MARGIN * 2.0 - fixed).max(0.0);
    let fit = ((room / BULK_ROW).floor() as usize).max(1);
    let visible = count.min(fit).max(1);
    let width = (area.width() - CARD_MARGIN * 2.0).min(BULK_MAX_WIDTH);
    let card = place_card(area, width, fixed + visible as f32 * BULK_ROW);
    let inner_left = card.left() + CARD_PAD;
    let inner_right = card.right() - CARD_PAD;
    let font = egui::FontId::proportional(FONT);

    // ── The template field ──────────────────────────────────────────────────
    let field_top = card.top() + CARD_PAD + ROW + BULK_FIELD_GAP;
    let template = egui::Rect::from_min_max(
        egui::pos2(inner_left, field_top),
        egui::pos2(inner_right, field_top + BULK_ROW),
    );
    let label = chrome::text_width(painter, TEMPLATE_LABEL, font.clone());
    let template_text = egui::Rect::from_min_max(
        egui::pos2(template.left() + PAD_X + label + PAD_X, template.top()),
        egui::pos2(
            (template.right() - PAD_X).max(template.left() + PAD_X + label + PAD_X),
            template.bottom(),
        ),
    );
    let galley = bulk_galley(painter, bulk.template.text());
    let template_scroll = if bulk.focus == Focus::Template {
        chrome::caret_scroll(
            template_text.width(),
            x_of(&galley, bulk.template.cursor()),
            galley.size().x,
            bulk.template_scroll,
        )
    } else {
        let most = (galley.size().x + chrome::CARET_WIDTH - template_text.width()).max(0.0);
        bulk.template_scroll.clamp(0.0, most)
    };
    let template_origin = template_text.left() - template_scroll;

    // ── The rows ────────────────────────────────────────────────────────────
    let body_top = template.bottom() + BULK_LIST_GAP;
    let body = egui::Rect::from_min_max(
        egui::pos2(inner_left, body_top),
        egui::pos2(inner_right, body_top + visible as f32 * BULK_ROW),
    );
    let bar = crate::scrollbar::geometry(body, bulk.first as f32, visible as f32, count as f32);
    // The rows stop short of the bar's band, so a press on the band is the
    // bar's and the end of a name is never under the thumb.
    let rows_right = match bar {
        Some(_) => body.right() - crate::scrollbar::HIT_WIDTH,
        None => body.right(),
    };
    let split = (rows_right - body.left()) * BULK_SPLIT;
    let problems = bulk.problems();
    let primary = bulk.editor.primary().head;
    let mut row_scroll = 0.0;
    let mut rows = Vec::with_capacity(visible);
    for slot in 0..visible {
        let index = bulk.first + slot;
        let Some(line) = bulk.editor.line(index) else {
            break;
        };
        let rect = egui::Rect::from_min_max(
            egui::pos2(body.left(), body_top + slot as f32 * BULK_ROW),
            egui::pos2(rows_right, body_top + (slot + 1) as f32 * BULK_ROW),
        );
        let old = egui::Rect::from_min_size(rect.min, egui::vec2(split, BULK_ROW));
        let new =
            egui::Rect::from_min_max(egui::pos2(old.right() + BULK_ARROW, rect.top()), rect.max);
        let problem = problems.get(index).copied().flatten();
        // The refusal takes its room from the right end, up to half the plate:
        // a long reason must not leave no name to read beside it.
        let mut right = new.right() - PAD_X;
        if let Some(problem) = problem {
            let reason =
                chrome::text_width(painter, problem.message(), font.clone()).min(new.width() / 2.0);
            right -= reason + PAD_X;
        }
        let text = egui::Rect::from_min_max(
            egui::pos2(new.left() + PAD_X, new.top()),
            egui::pos2(right.max(new.left() + PAD_X), new.bottom()),
        );
        // Only the primary caret's row scrolls: it is the one the keyboard is
        // in, and every other row reads from its start.
        let scroll = if bulk.focus == Focus::Rows && primary.row == index {
            let galley = bulk_galley(painter, line);
            row_scroll = chrome::caret_scroll(
                text.width(),
                x_of(&galley, primary.col),
                galley.size().x,
                bulk.row_scroll,
            );
            row_scroll
        } else {
            0.0
        };
        rows.push(BulkRowGeom {
            index,
            rect,
            old,
            new,
            text,
            origin: text.left() - scroll,
            problem,
        });
    }

    let buttons_top = card.bottom() - CARD_PAD - BUTTON_HEIGHT;
    let actions = button_row(painter, inner_right, buttons_top, &["Cancel", "Rename"]);
    let cancel_left = actions.first().map_or(inner_right, egui::Rect::left);
    let status = egui::Rect::from_min_max(
        egui::pos2(inner_left, buttons_top),
        egui::pos2(
            (cancel_left - crate::ui::GAP).max(inner_left),
            buttons_top + BUTTON_HEIGHT,
        ),
    );
    let popover = popover_geometry(painter, area, bulk, &template, template_origin, &rows);
    BulkGeometry {
        card,
        close: chrome::close_button_rect(card),
        template,
        template_text,
        template_origin,
        template_scroll,
        body,
        rows,
        row_scroll,
        bar,
        actions,
        status,
        popover,
        visible,
    }
}

/// Where the popover goes: under the line it completes, its text lined up
/// with the `{` that opened it, kept inside the window, and above the line
/// instead when there is no room below.
fn popover_geometry(
    painter: &egui::Painter,
    area: egui::Rect,
    bulk: &Bulk,
    template: &egui::Rect,
    template_origin: f32,
    rows: &[BulkRowGeom],
) -> Option<PopoverGeom> {
    let popover = bulk.live_popover()?;
    let (line, origin, text) = match popover.anchor_row {
        None => (*template, template_origin, bulk.template.text()),
        Some(index) => {
            let row = rows.iter().find(|row| row.index == index)?;
            (row.new, row.origin, bulk.editor.line(index)?)
        }
    };
    let brace = origin + x_of(&bulk_galley(painter, text), popover.open_at);
    let shown = popover.candidates.len().min(crate::bulk::POPOVER_ROWS);
    let height = shown as f32 * ROW + POPOVER_PAD * 2.0;
    let width = POPOVER_WIDTH
        .min(area.width() - crate::ui::GAP * 2.0)
        .max(0.0);
    let lowest = area.left() + crate::ui::GAP;
    let highest = (area.right() - crate::ui::GAP - width).max(lowest);
    let left = (brace - POPOVER_PAD - PAD_X).clamp(lowest, highest);
    let below = line.bottom() + POPOVER_OFFSET;
    let top = if below + height <= area.bottom() - crate::ui::GAP {
        below
    } else {
        (line.top() - POPOVER_OFFSET - height).max(area.top())
    };
    let card = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, height));
    let rows = (0..shown)
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(
                    card.left() + POPOVER_PAD,
                    card.top() + POPOVER_PAD + i as f32 * ROW,
                ),
                egui::vec2(card.width() - POPOVER_PAD * 2.0, ROW),
            )
        })
        .collect();
    Some(PopoverGeom {
        card,
        rows,
        first: popover.first,
    })
}

/// Draw the rename card: the heading, the template field, the two columns,
/// the answers, and the `{` popover over all of it. The list's bar is drawn
/// held while a hand is on its thumb ([`Painting::held`]).
pub fn paint_bulk(
    paint: &Painting<'_>,
    bulk: &Bulk,
    geometry: &BulkGeometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    chrome::card(paint, geometry.card, 1.0);

    let problems = bulk.problems();
    let settled = problems
        .iter()
        .any(|problem| problem.is_some_and(|p| p != Problem::Pending));
    let waiting = problems.contains(&Some(Problem::Pending));
    let changes = bulk.changes();
    let left = geometry.card.left() + CARD_PAD;

    // The title stops short of the `×` that shares its row.
    chrome::close_button(paint, geometry.close, hovers, ripples);
    chrome::truncated_in(
        painter,
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW / 2.0),
        "Rename files",
        palette.text,
        (geometry.close.left() - crate::ui::GAP - left).max(0.0),
        egui::FontId::proportional(FONT + 2.0),
    );
    // The status is the answer to "what will Enter do", and it is the one
    // place the card says *no* — a disabled button with no reason beside it is
    // a dead end (`delightful-ui` §9). It is drawn beside the buttons, below.
    let (status, tint) = if settled {
        ("fix the names in red to continue".to_string(), palette.red)
    } else if waiting {
        ("reading photo data…".to_string(), palette.overlay1)
    } else if changes == 0 {
        ("nothing has changed yet".to_string(), palette.overlay1)
    } else {
        (
            format!(
                "{} will be renamed",
                if changes == 1 {
                    "1 file".to_string()
                } else {
                    format!("{} files", grouped(changes as u64))
                }
            ),
            palette.green,
        )
    };
    let font = egui::FontId::proportional(FONT);

    paint_template(paint, bulk, geometry);

    // ── The rows ────────────────────────────────────────────────────────────
    let clipped = painter.with_clip_rect(geometry.body);
    let rows_focused = bulk.focus == Focus::Rows;
    for row in &geometry.rows {
        let (Some(old), Some(line)) = (bulk.olds.get(row.index), bulk.editor.line(row.index))
        else {
            break;
        };
        chrome::truncated_in(
            &clipped,
            egui::pos2(row.old.left() + PAD_X, row.old.center().y),
            old,
            // The old name is history: it is here to be compared against, not
            // read, so it is a step quieter than the name being typed.
            if old != line {
                palette.overlay1
            } else {
                palette.subtext0
            },
            (row.old.width() - PAD_X * 2.0).max(0.0),
            bulk_font(),
        );
        clipped.text(
            egui::pos2(row.old.right() + BULK_ARROW / 2.0, row.rect.center().y),
            egui::Align2::CENTER_CENTER,
            "→",
            font.clone(),
            palette.overlay0,
        );
        let carets = bulk.editor.cursors_on(row.index).next().is_some();
        paint_name(paint, bulk, row, old, line, rows_focused.then_some(carets));
    }
    if let Some(bar) = &geometry.bar {
        let lit = hovers.hover(Control::Bar(crate::scrollbar::Bar::Bulk));
        let held = paint.held == Some(crate::scrollbar::Bar::Bulk);
        crate::scrollbar::paint(paint, bar, 1.0, lit, held);
    }

    // The status, with how many carets are typing at once while they are: a
    // word typed at forty of them lands forty times, and that should never be
    // a surprise. One run cut to the room left of `Cancel`, so a long status
    // on a narrow card ends in `…` rather than under the button.
    let carets = bulk.editor.cursors().len();
    let inked = |color: egui::Color32| egui::text::TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&status, 0.0, inked(tint));
    if carets > 1 && bulk.focus == Focus::Rows {
        job.append(
            &format!(" · {} carets", grouped(carets as u64)),
            0.0,
            inked(palette.overlay0),
        );
    }
    job.wrap = egui::text::TextWrapping {
        max_width: geometry.status.width().max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let said = painter.layout_job(job);
    painter.galley(
        egui::pos2(
            geometry.status.left(),
            geometry.status.center().y - said.size().y / 2.0,
        ),
        said,
        palette.text,
    );

    let valid = problems.iter().all(Option::is_none);
    for (i, rect) in geometry.actions.iter().enumerate() {
        button(
            paint,
            *rect,
            ["Cancel", "Rename"][i],
            i == 1,
            false,
            Control::Action(i),
            hovers,
            ripples,
        );
        // A disabled commit is drawn as a veil over the button rather than as a
        // different button, so it stays in the same place and at the same size
        // (`delightful-ui` §8) and the reason is in the status beside it.
        if i == 1 && (!valid || changes == 0) {
            painter.rect_filled(*rect, ROW_RADIUS, chrome::fade(palette.crust, 0.55));
        }
    }

    if let Some(popover) = &geometry.popover {
        paint_popover(paint, bulk, popover, hovers, ripples);
    }
}

/// The template field: its label, the template with every value in the accent
/// and every stretch that did not parse underlined, and the caret and
/// selection while it has the keyboard.
fn paint_template(paint: &Painting<'_>, bulk: &Bulk, geometry: &BulkGeometry) {
    let palette = paint.palette;
    let painter = paint.painter;
    let focused = bulk.focus == Focus::Template;
    let plate = geometry.template;
    painter.rect_filled(
        plate.shrink(1.0),
        ROW_RADIUS,
        if focused {
            palette.surface0
        } else {
            mix(palette.crust, palette.surface0, 0.45)
        },
    );
    painter.text(
        egui::pos2(plate.left() + PAD_X, plate.center().y),
        egui::Align2::LEFT_CENTER,
        TEMPLATE_LABEL,
        egui::FontId::proportional(FONT),
        palette.overlay1,
    );

    let text = bulk.template.text();
    let clip = painter.with_clip_rect(geometry.template_text);
    let origin = geometry.template_origin;
    let galley = bulk_galley(painter, text);
    let caret_top = plate.top() + 4.0;
    let caret_height = (plate.height() - 8.0).max(0.0);
    if focused {
        if let Some(range) = bulk.template.selection() {
            clip.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(origin + x_of(&galley, range.start), caret_top),
                    egui::pos2(origin + x_of(&galley, range.end), caret_top + caret_height),
                ),
                2,
                chrome::fade(palette.blue, 0.3),
            );
        }
    }
    let job = template_job(text, bulk.parsed(), palette);
    let shaped = painter.layout_job(job);
    clip.galley(
        egui::pos2(origin, plate.center().y - shaped.size().y / 2.0),
        shaped,
        palette.text,
    );
    if focused {
        clip.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(origin + x_of(&galley, bulk.template.cursor()), caret_top),
                egui::vec2(chrome::CARET_WIDTH, caret_height),
            ),
            0,
            palette.blue,
        );
    }
}

/// The template as coloured runs: plain text in the body colour, each value in
/// the accent, and each stretch [`Template::parse`] could not read underlined
/// in red with its literal text left as it is.
///
/// `parsed` is `text` parsed, which the card keeps from the last edit
/// ([`Bulk::parsed`]), so a frame paints without parsing.
fn template_job(
    text: &str,
    parsed: &Template,
    palette: &crate::theme::Palette,
) -> egui::text::LayoutJob {
    #[derive(Clone, Copy, PartialEq)]
    enum Ink {
        Plain,
        Value,
        Problem,
    }
    let chars: Vec<char> = text.chars().collect();
    let mut inks = vec![Ink::Plain; chars.len()];
    for token in parsed.tokens() {
        for ink in inks.iter_mut().take(token.span.end).skip(token.span.start) {
            *ink = Ink::Value;
        }
    }
    for problem in parsed.problems() {
        for ink in inks
            .iter_mut()
            .take(problem.span.end)
            .skip(problem.span.start)
        {
            *ink = Ink::Problem;
        }
    }
    let format = |ink: Ink| egui::text::TextFormat {
        font_id: bulk_font(),
        color: match ink {
            Ink::Plain | Ink::Problem => palette.text,
            Ink::Value => palette.blue,
        },
        underline: match ink {
            Ink::Problem => egui::Stroke::new(1.0, palette.red),
            _ => egui::Stroke::NONE,
        },
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    let mut start = 0;
    while start < chars.len() {
        let ink = inks[start];
        let end = (start..chars.len())
            .find(|&i| inks[i] != ink)
            .unwrap_or(chars.len());
        let run: String = chars[start..end].iter().collect();
        job.append(&run, 0.0, format(ink));
        start = end;
    }
    job
}

/// One row's new name: its plate, the stretch that differs from the old name,
/// the selections, the text, the carets, and the refusal at its right end.
///
/// `lit` is `None` while the template has the keyboard (no row is lit and no
/// caret drawn), else whether this row has a caret on it.
fn paint_name(
    paint: &Painting<'_>,
    bulk: &Bulk,
    row: &BulkRowGeom,
    old: &str,
    line: &str,
    lit: Option<bool>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    let rect = row.new;
    if !rect.is_positive() {
        return;
    }
    let pending = row.problem == Some(Problem::Pending);
    // A row with a caret on it is lit; a refused one is washed in red. Both at
    // once is possible and correct — the row you are in is the one you are
    // fixing. A row waiting on the photo reader is not refused, only not yet
    // answered, so it keeps the quiet ground.
    let ground = match (lit, row.problem) {
        (_, Some(problem)) if problem != Problem::Pending => mix(palette.crust, palette.red, 0.14),
        (Some(true), _) => palette.surface0,
        _ => mix(palette.crust, palette.surface0, 0.45),
    };
    painter.rect_filled(rect.shrink(1.0), ROW_RADIUS, ground);

    if let Some(problem) = row.problem {
        let color = if pending {
            palette.overlay1
        } else {
            palette.red
        };
        let right = rect.right() - PAD_X;
        chrome::truncated(
            painter,
            egui::pos2(row.text.right() + PAD_X, rect.center().y),
            problem.message(),
            color,
            (right - row.text.right() - PAD_X).max(0.0),
        );
    }

    let clip = painter.with_clip_rect(row.text);
    let galley = bulk_galley(painter, line);
    let origin = row.origin;
    let top = rect.top() + 4.0;
    let height = (rect.height() - 8.0).max(0.0);
    let span = |range: Range<usize>| {
        egui::Rect::from_min_max(
            egui::pos2(origin + x_of(&galley, range.start), top),
            egui::pos2(origin + x_of(&galley, range.end), top + height),
        )
    };
    // What the new name adds over the old one, faintly: a diff read at a
    // glance down a column of forty names.
    for range in changed_spans(old, line) {
        clip.rect_filled(span(range), 2, chrome::fade(palette.green, 0.18));
    }
    let focused = lit.is_some();
    if focused {
        for cursor in bulk.editor.cursors_on(row.index) {
            if cursor.is_selection() {
                clip.rect_filled(span(cursor.range()), 2, chrome::fade(palette.blue, 0.3));
            }
        }
    }
    // The measuring galley is uncoloured, so it is drawn in the body colour.
    clip.galley(
        egui::pos2(origin, rect.center().y - galley.size().y / 2.0),
        galley.clone(),
        palette.text,
    );
    if focused {
        for cursor in bulk.editor.cursors_on(row.index) {
            clip.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(origin + x_of(&galley, cursor.head.col), top),
                    egui::vec2(chrome::CARET_WIDTH, height),
                ),
                0,
                palette.blue,
            );
        }
    }
}

/// The stretches of `new` the rename adds, in chars, left to right. Empty
/// when it only takes something away, or changes nothing.
///
/// When the old name is still there, whole, somewhere in the new one, what
/// was added is everything around it: nothing, one side or both. Only when it
/// is not does the diff fall back to what is left between the common start
/// and the common end.
///
/// The fallback alone reads the most common template wrong. A date put in
/// front of a name that starts with a date, `2026-01-01_x.mp4` becoming
/// `2026-08-05_2026-01-01_x.mp4`, shares its first six characters with the
/// old name by accident, and the stretch between the common ends is
/// `8-05_2026-0`: the middle of two dates, which is neither the part added
/// nor the part kept. Finding the old name first gives `2026-08-05_`, which
/// is what the template wrote. Where the old name occurs more than once, as
/// `{name}_{name}` makes it, the first is the one taken as kept.
fn changed_spans(old: &str, new: &str) -> Vec<Range<usize>> {
    let old: Vec<char> = old.chars().collect();
    let new: Vec<char> = new.chars().collect();
    let kept = if old.is_empty() {
        None
    } else {
        new.windows(old.len()).position(|window| window == old)
    };
    if let Some(at) = kept {
        let (before, after) = (0..at, at + old.len()..new.len());
        return [before, after]
            .into_iter()
            .filter(|range| !range.is_empty())
            .collect();
    }
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    let middle = prefix..new.len() - suffix;
    if middle.is_empty() {
        Vec::new()
    } else {
        vec![middle]
    }
}

/// The `{` popover: a small plate over everything, one row per candidate —
/// what it inserts, what it is, and what it would make of the row it is about.
fn paint_popover(
    paint: &Painting<'_>,
    bulk: &Bulk,
    geometry: &PopoverGeom,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let Some(popover) = bulk.live_popover() else {
        return;
    };
    let palette = paint.palette;
    let painter = paint.painter;
    let radius = ROW_RADIUS + POPOVER_PAD as u8;
    painter.rect_filled(geometry.card, radius, palette.surface0);
    painter.rect_stroke(
        geometry.card,
        radius,
        egui::Stroke::new(1.0, palette.surface1),
        egui::StrokeKind::Inside,
    );
    // What a candidate inserts and what it makes are names, in the names'
    // face; what it is, and why it cannot make one, are words.
    let mono = bulk_font();
    let font = egui::FontId::proportional(FONT);
    let small = egui::FontId::proportional(FONT - 1.0);
    for (slot, rect) in geometry.rows.iter().enumerate() {
        let index = geometry.first + slot;
        let Some(candidate) = popover.candidates.get(index) else {
            break;
        };
        let key = Control::BulkCandidate(index);
        let hover = hovers.hover(key);
        let selected = index == popover.selected;
        let rect_now = pressed_rect(*rect, hovers.press(key));
        if selected || hover > 0.0 {
            let fill = if selected {
                palette.surface1
            } else {
                mix(palette.surface0, palette.surface1, hover * 0.6)
            };
            painter.rect_filled(rect_now, ROW_RADIUS, fill);
        }
        let inside = painter.with_clip_rect(rect_now);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }

        let inner = rect.shrink2(egui::vec2(PAD_X, 0.0));
        let y = rect.center().y;
        let insert_room = inner.width() * POPOVER_INSERT_SHARE;
        let insert = chrome::text_width(painter, &candidate.insert, mono.clone()).min(insert_room);
        // A point of slack: a run measured to fit exactly must not be cut to
        // `…` by the rounding of the second measurement.
        chrome::truncated_in(
            painter,
            egui::pos2(inner.left(), y),
            &candidate.insert,
            palette.text,
            insert + 1.0,
            mono.clone(),
        );
        let (preview, preview_color, preview_font) = match bulk.preview(candidate) {
            Ok(name) => (name, palette.blue, mono.clone()),
            Err(Missing::Pending) => ("reading photo…".to_string(), palette.overlay0, font.clone()),
            Err(Missing::Because(why)) => (why.to_string(), palette.overlay0, font.clone()),
        };
        let preview_room = inner.width() * POPOVER_PREVIEW_SHARE;
        let preview_width =
            chrome::text_width(painter, &preview, preview_font.clone()).min(preview_room);
        chrome::truncated_in(
            painter,
            egui::pos2(inner.right() - preview_width, y),
            &preview,
            preview_color,
            preview_width + 1.0,
            preview_font,
        );
        let detail_left = inner.left() + insert + PAD_X * 1.5;
        let detail_room = inner.right() - preview_width - PAD_X * 1.5 - detail_left;
        chrome::truncated_in(
            painter,
            egui::pos2(detail_left, y),
            candidate.detail,
            palette.overlay1,
            detail_room.max(0.0),
            small.clone(),
        );
    }
}

/// Draw the conflict resolver.
pub fn paint_conflict(
    paint: &Painting<'_>,
    dialog: &ConflictDialog,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    chrome::card(paint, geometry.card, 1.0);

    let left = geometry.card.left() + CARD_PAD;
    let n = dialog.len();
    // The title stops short of the `×` that shares its row.
    let title_right = match geometry.close {
        Some(close) => {
            chrome::close_button(paint, close, hovers, ripples);
            close.left() - crate::ui::GAP
        }
        None => geometry.card.right() - CARD_PAD,
    };
    chrome::truncated_in(
        painter,
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW / 2.0),
        &if n == 1 {
            "A file with that name is already there".to_string()
        } else {
            format!("{} names are already taken", grouped(n as u64))
        },
        palette.text,
        (title_right - left).max(0.0),
        egui::FontId::proportional(FONT + 2.0),
    );
    painter.text(
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW + ROW / 2.0),
        egui::Align2::LEFT_CENTER,
        dialog
            .error
            .clone()
            .unwrap_or_else(|| "↑↓ choose · o/s/r answer · Enter apply · Esc cancel".to_string()),
        egui::FontId::proportional(FONT),
        if dialog.error.is_some() {
            palette.red
        } else {
            palette.overlay1
        },
    );

    // The list of taken names, cursor row lit.
    let first = dialog.first_visible();
    let clipped = painter.with_clip_rect(geometry.body);
    for (i, rect) in geometry.rows.iter().enumerate() {
        let Some(conflict) = dialog.plan.conflicts.get(first + i) else {
            break;
        };
        let on_cursor = first + i == dialog.cursor;
        // The row's place among the drawn ones, which is what the hit test
        // reports: a scrolled list keyed by its conflicts would light a row
        // the pointer is not on.
        let key = Control::PanelRow(i);
        let hover = hovers.hover(key);
        let fill = mix(
            if on_cursor {
                palette.surface1
            } else {
                palette.crust
            },
            palette.surface0,
            hover,
        );
        let rect = pressed_rect(*rect, hovers.press(key));
        if on_cursor || hover > 0.0 {
            clipped.rect_filled(rect, chrome::CARD_ROW_RADIUS, fill);
        }
        for splash in ripples.splashes(key, paint.now) {
            clipped.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        chrome::truncated(
            &clipped,
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            &conflict
                .dst
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            if on_cursor {
                palette.text
            } else {
                palette.subtext0
            },
            (rect.width() - PAD_X * 2.0).max(0.0),
        );
    }
    if let Some(bar) = conflicts_bar(geometry, dialog) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Conflict,
            hovers,
            dialog.scrolled_at(),
            1.0,
        );
    }

    // The comparison: what is coming in, and what is already there.
    // Thumbnails are deferred to the drag-and-drop phase (PLAN §7.1) — the
    // decode path exists but wiring a second consumer of it belongs with the
    // work that needs ghost images anyway.
    if let Some(conflict) = dialog.conflict() {
        let top = geometry.body.bottom() + 10.0;
        let width = (geometry.body.width() - BUTTON_GAP) / 2.0;
        let sides = [
            ("Pasting", conflict.src.as_path(), palette.blue),
            ("Already there", conflict.dst.as_path(), palette.peach),
        ];
        for (i, (heading, path, accent)) in sides.into_iter().enumerate() {
            let rect = egui::Rect::from_min_size(
                egui::pos2(geometry.body.left() + i as f32 * (width + BUTTON_GAP), top),
                egui::vec2(width, FACTS_HEIGHT),
            );
            paint_facts(paint, rect, heading, accent, dialog.facts_for(path));
        }
    }

    if let Some(rect) = geometry.apply_all {
        // The toggle is a control like any other, so it gets the whole
        // treatment (`delightful-ui` §3/§4, PLAN §8): hover in, press down,
        // ripple from the pointer. It was the one clickable thing on a Phase 2
        // card that had only the hover.
        let key = Control::Action(ConflictAction::ALL.len());
        let hover = hovers.hover(key);
        let rect = pressed_rect(rect, hovers.press(key));
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        let color = if dialog.apply_all {
            palette.yellow
        } else {
            mix(palette.overlay1, palette.text, hover)
        };
        let box_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 8.0, rect.center().y),
            egui::vec2(11.0, 11.0),
        );
        painter.rect_stroke(
            box_rect,
            3,
            egui::Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        if dialog.apply_all {
            painter.rect_filled(box_rect.shrink(3.0), 1, color);
        }
        painter.text(
            egui::pos2(box_rect.right() + 7.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "Apply to all",
            egui::FontId::proportional(FONT),
            color,
        );
    }

    for (i, rect) in geometry.actions.iter().enumerate() {
        let action = ConflictAction::ALL[i];
        button(
            paint,
            *rect,
            action.label(),
            action == dialog.action,
            action == ConflictAction::Overwrite,
            Control::Action(i),
            hovers,
            ripples,
        );
    }
}

/// One side of the comparison: heading, icon, name, size, modified.
fn paint_facts(
    paint: &Painting<'_>,
    rect: egui::Rect,
    heading: &str,
    accent: egui::Color32,
    facts: Option<&Facts>,
) {
    let painter = paint.painter;
    let palette = paint.palette;
    painter.rect_filled(rect, ROW_RADIUS, mix(palette.crust, palette.base, 0.6));
    painter.text(
        egui::pos2(rect.left() + PAD_X, rect.top() + 11.0),
        egui::Align2::LEFT_CENTER,
        heading,
        egui::FontId::proportional(FONT - 1.5),
        accent,
    );
    let Some(facts) = facts else { return };

    let icon_x = rect.left() + PAD_X;
    let name_y = rect.top() + 32.0;
    if let Some(entry) = &facts.entry {
        let icon = crate::icons::icon_for(entry, paint.theme, palette, paint.nerd);
        let family = if paint.nerd {
            egui::FontFamily::Name(crate::icons::ICON_FAMILY.into())
        } else {
            egui::FontFamily::Monospace
        };
        painter.text(
            egui::pos2(icon_x, name_y),
            egui::Align2::LEFT_CENTER,
            icon.glyph,
            egui::FontId::new(FONT, family),
            icon.color,
        );
    }
    chrome::truncated(
        painter,
        egui::pos2(icon_x + 18.0, name_y),
        &facts.name,
        palette.text,
        (rect.width() - PAD_X * 2.0 - 18.0).max(0.0),
    );
    painter.text(
        egui::pos2(rect.left() + PAD_X, rect.top() + 52.0),
        egui::Align2::LEFT_CENTER,
        facts.size_text(),
        egui::FontId::proportional(FONT - 1.0),
        palette.subtext0,
    );
    painter.text(
        egui::pos2(rect.right() - PAD_X, rect.top() + 52.0),
        egui::Align2::RIGHT_CENTER,
        &facts.mtime,
        egui::FontId::proportional(FONT - 1.0),
        palette.overlay1,
    );
}

/// One dialog button, with the hover/press/ripple treatment every control in
/// delightfile gets (`delightful-ui` §3, §4).
#[allow(clippy::too_many_arguments)]
pub fn button(
    paint: &Painting<'_>,
    rect: egui::Rect,
    label: &str,
    selected: bool,
    danger: bool,
    key: Control,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let accent = if danger { palette.red } else { palette.blue };
    let hover = hovers.hover(key);
    let rect = pressed_rect(rect, hovers.press(key));
    let fill = if selected {
        mix(palette.crust, accent, 0.22)
    } else {
        mix(palette.crust, palette.surface0, 0.5 + hover * 0.5)
    };
    paint.painter.rect_filled(rect, ROW_RADIUS, fill);
    paint.painter.rect_stroke(
        rect,
        ROW_RADIUS,
        egui::Stroke::new(
            1.0,
            if selected {
                accent
            } else {
                mix(palette.surface1, accent, hover)
            },
        ),
        egui::StrokeKind::Inside,
    );
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
        );
    }
    inside.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(FONT),
        if selected {
            palette.text
        } else {
            palette.subtext0
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;
    use df_core::ops::paste::{plan_paste, Clipboard};

    /// A throwaway tree under `$TMPDIR`.
    ///
    /// df-core has one of these (`ops::fixture::TempTree`) but it is
    /// `#[cfg(test)] pub(crate)`, so it does not cross the crate boundary; this
    /// is the twenty lines of it these tests need.
    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> TempTree {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("df-app-{label}-{nanos}"));
            std::fs::create_dir_all(&path).unwrap();
            TempTree { path }
        }

        fn join(&self, rel: &str) -> PathBuf {
            self.path.join(rel)
        }

        fn file(&self, rel: &str, contents: &[u8]) -> PathBuf {
            let path = self.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Run `f` with a painter that can measure text — which every layout here
    /// needs, now that the cards are sized to what they say.
    fn with_painter(mut f: impl FnMut(&egui::Painter)) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| f(ui.painter()));
    }

    fn plan(tree: &TempTree) -> PastePlan {
        let a = tree.file("src/a.txt", b"new");
        let b = tree.file("src/b.txt", b"new");
        tree.file("dst/a.txt", b"old");
        tree.file("dst/b.txt", b"old");
        plan_paste(&Clipboard::yank([a, b]), &tree.join("dst"), false).unwrap()
    }

    #[test]
    fn a_confirm_says_what_it_is_about_to_do() {
        let two = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        let trash = Confirm::new(ConfirmKind::Trash, two.clone());
        assert_eq!(trash.title(), "Trash 2 selected files?");
        assert!(!trash.danger());

        let one = Confirm::new(ConfirmKind::Delete, vec![PathBuf::from("/tmp/a")]);
        assert_eq!(one.title(), "Delete 1 selected file permanently?");
        assert!(one.danger());
        assert_eq!(one.body(), vec!["a".to_string()]);

        // With no line under the title, the title carries the warning: a
        // remote `d` is the one that looks local and is not.
        for (kind, title) in [
            (
                ConfirmKind::RemoteDelete,
                "Delete 2 remote files permanently?",
            ),
            (ConfirmKind::Purge, "Destroy 2 trashed files?"),
            (
                ConfirmKind::EmptyTrash,
                "Empty the trash? 2 items will be deleted for good.",
            ),
        ] {
            let confirm = Confirm::new(kind, two.clone());
            assert_eq!(confirm.title(), title);
            assert!(confirm.danger(), "{kind:?} is red");
        }
        // Emptying the trash says what it frees, in the trash chip's words:
        // counting, then settled.
        let mut empty = Confirm::new(ConfirmKind::EmptyTrash, two.clone());
        empty.size = Some(crate::folders::Size {
            bytes: 1_288_490_189,
            settled: false,
        });
        assert_eq!(
            empty.title(),
            "Empty the trash? 2 items · ~1.2 GB will be deleted for good."
        );
        empty.size = Some(crate::folders::Size {
            bytes: 1_288_490_189,
            settled: true,
        });
        assert_eq!(
            empty.title(),
            "Empty the trash? 2 items · 1.2 GB will be deleted for good."
        );

        // A save's replace names its one file in the question, and lists
        // nothing under it: the name once, where it is being asked about.
        let replace = Confirm::new(ConfirmKind::Replace, vec![PathBuf::from("/tmp/report.pdf")]);
        assert_eq!(replace.title(), "Replace report.pdf?");
        assert_eq!(confirm_verb(replace.kind), "Replace");
        assert!(replace.danger(), "a replace loses a file");
        assert_eq!(replace.lines(), 0);
        assert_eq!(one.lines(), 1);
    }

    /// The body scrolls and stops — `↑` at the top and `↓` at the bottom are
    /// no-ops rather than a list that walks off its own end.
    #[test]
    fn the_confirm_body_scrolls_within_its_bounds() {
        let paths: Vec<PathBuf> = (0..10)
            .map(|i| PathBuf::from(format!("/tmp/{i}")))
            .collect();
        let mut confirm = Confirm::new(ConfirmKind::Trash, paths);
        confirm.scroll_by(-1);
        assert_eq!(confirm.scroll, 0);
        confirm.scroll_by(3);
        assert_eq!(confirm.scroll, 3);
        confirm.scroll_by(100);
        assert_eq!(confirm.scroll, 10 - BODY_VISIBLE);
    }

    /// Answering the last conflict settles the plan, and the answers land in
    /// df-core's plan rather than in a parallel copy of it.
    #[test]
    fn answering_every_conflict_settles_the_plan() {
        let tree = TempTree::new("dialog-settle");
        let mut dialog = ConflictDialog::new(plan(&tree));
        assert_eq!(dialog.len(), 2);

        dialog.set_action(ConflictAction::Skip);
        assert_eq!(dialog.apply(), Step::Continue);
        assert_eq!(dialog.len(), 1, "the answered one is gone");

        dialog.set_action(ConflictAction::Overwrite);
        assert_eq!(dialog.apply(), Step::Settled);
        assert!(dialog.plan.is_settled());
        assert_eq!(dialog.plan.ready.len(), 1, "skip left nothing behind");
        assert!(dialog.plan.ready[0].overwrite);
    }

    /// "Apply to all" answers everything at once, and a rename-all uses each
    /// conflict's own suggested name — one typed name cannot serve two files.
    #[test]
    fn apply_to_all_answers_every_remaining_conflict() {
        let tree = TempTree::new("dialog-all");
        let mut dialog = ConflictDialog::new(plan(&tree));
        dialog.toggle_apply_all();
        assert!(dialog.apply_all);
        dialog.set_action(ConflictAction::Rename);
        assert_eq!(dialog.apply(), Step::Settled);
        assert_eq!(dialog.plan.ready.len(), 2);
        for item in &dialog.plan.ready {
            assert!(!item.overwrite);
            let name = item.dst.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.contains('_'), "auto-named: {name}");
        }
    }

    /// PLAN §5's bare-filename rule, surfaced inline: a path is refused, the
    /// conflict stays unanswered, and the dialog says why.
    #[test]
    fn a_rename_answer_must_be_a_bare_file_name() {
        let tree = TempTree::new("dialog-rename");
        let mut dialog = ConflictDialog::new(plan(&tree));
        dialog.set_action(ConflictAction::Rename);
        let Step::NeedName(suggested) = dialog.apply() else {
            panic!("rename asks for a name");
        };
        assert!(
            !suggested.contains('/'),
            "the suggestion is a name: {suggested}"
        );

        let before = dialog.len();
        assert!(matches!(dialog.rename("../escape.txt"), Step::NeedName(_)));
        assert_eq!(dialog.len(), before, "the conflict is still unanswered");
        let error = dialog.error.clone().expect("an inline message");
        assert!(error.contains("not a usable name"), "{error}");

        assert_eq!(dialog.rename("a-copy.txt"), Step::Continue);
        assert_eq!(dialog.len(), before - 1);
        assert!(dialog.error.is_none());
        assert_eq!(
            dialog.plan.ready[0].dst.file_name().unwrap(),
            "a-copy.txt",
            "the name lands in the destination directory"
        );
    }

    #[test]
    fn the_cursor_and_the_action_wrap_sensibly() {
        let tree = TempTree::new("dialog-cursor");
        let mut dialog = ConflictDialog::new(plan(&tree));
        dialog.move_cursor(-1);
        assert_eq!(dialog.cursor, 0, "the top is the top");
        dialog.move_cursor(5);
        assert_eq!(dialog.cursor, 1, "and the bottom is the bottom");

        dialog.set_action(ConflictAction::Overwrite);
        dialog.cycle_action(-1);
        assert_eq!(
            dialog.action,
            ConflictAction::Rename,
            "left from the first wraps"
        );
        dialog.cycle_action(1);
        assert_eq!(dialog.action, ConflictAction::Overwrite);
        assert_eq!(
            ConflictDialog::action_for_key('s'),
            Some(ConflictAction::Skip)
        );
        assert_eq!(ConflictDialog::action_for_key('q'), None);
    }

    /// A conflict about an **upload** draws the file that is really on the
    /// server (PLAN §7.6).
    ///
    /// **The bug this pins**: the destinations are `sftp://…` strings, so the
    /// card's own `Facts::read` found nothing and drew the file about to be
    /// overwritten as "missing" — a card asking "replace this?" about what
    /// looked like an empty slot. The probe's stat is seeded instead, and the
    /// local source is still read from the disk it is on.
    #[test]
    fn an_upload_conflict_shows_the_file_that_is_really_on_the_server() {
        let tree = TempTree::new("dialog-upload");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        let src = tree.join("src/index.html");
        std::fs::write(&src, b"local body").unwrap();

        let dest = df_core::vfs::VfsPath::new("showandtour1", "/srv/www");
        let there = df_core::vfs::stat_entry(
            &dest.join("index.html"),
            df_core::vfs::Attrs {
                size: Some(4096),
                permissions: Some(0o100_644),
                mtime: Some(1_700_000_000),
                ..df_core::vfs::Attrs::default()
            },
        );
        let plan = crate::remote::plan_upload(
            std::slice::from_ref(&src),
            &dest,
            std::slice::from_ref(&there),
        );
        let facts: HashMap<PathBuf, Facts> = [(there.path.clone(), Facts::of(there))]
            .into_iter()
            .collect();
        let dialog = ConflictDialog::with_facts(plan, facts);

        let conflict = dialog.conflict().expect("one conflict").clone();
        let remote = dialog.facts_for(&conflict.dst).expect("seeded");
        assert!(remote.present, "the server's file is not missing");
        assert_eq!(remote.size, 4096);
        assert_eq!(remote.size_text(), "4.0 KB");
        assert!(!remote.mtime.is_empty(), "the date came off the wire");
        // The source side is still read locally, from the file it really is.
        let local = dialog.facts_for(&conflict.src).expect("read from disk");
        assert!(local.present);
        assert_eq!(local.size, "local body".len() as u64);
    }

    /// `n` sources pasted onto `n` names already taken.
    fn plan_of(tree: &TempTree, n: usize) -> PastePlan {
        let sources: Vec<PathBuf> = (0..n)
            .map(|i| {
                tree.file(&format!("dst/{i}.txt"), b"old");
                tree.file(&format!("src/{i}.txt"), b"new")
            })
            .collect();
        plan_paste(&Clipboard::yank(sources), &tree.join("dst"), false).unwrap()
    }

    fn window(height: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, height))
    }

    /// The rule every card with a list is fitted by: as many rows as the
    /// window leaves room for, up to what is wanted, never none while there
    /// is one to show, and the card inside the window.
    #[test]
    fn a_card_fits_as_many_rows_as_the_window_has_room_for() {
        let (rows, card) = fit_rows(window(900.0), 400.0, 100.0, 20.0, 12);
        assert_eq!(rows, 12, "room for all of them");
        assert!((card.height() - (100.0 + 12.0 * 20.0)).abs() < 1e-3);
        // 300 less two margins less the fixed parts is 168: eight rows.
        let short = window(300.0);
        let (rows, card) = fit_rows(short, 400.0, 100.0, 20.0, 12);
        assert_eq!(rows, 8);
        assert!(short.contains_rect(card));
        assert_eq!(fit_rows(short, 400.0, 100.0, 20.0, 3).0, 3, "fewer wanted");
        assert_eq!(fit_rows(short, 400.0, 100.0, 20.0, 0).0, 0, "none wanted");
        let tiny = window(110.0);
        let (rows, card) = fit_rows(tiny, 400.0, 100.0, 20.0, 12);
        assert_eq!(rows, 1, "never none while there is one to show");
        assert!(tiny.contains_rect(card), "clipped to the window instead");
    }

    /// A confirm in a short window lists what fits between its title and its
    /// buttons, and its keys scroll the rest; in a tall one it lists six, as
    /// it always did. The bar is there only while names are left out.
    #[test]
    fn a_short_window_scrolls_the_confirms_names() {
        let many: Vec<PathBuf> = (0..20)
            .map(|i| PathBuf::from(format!("/tmp/{i}")))
            .collect();
        let few = many[..3].to_vec();
        with_painter(|painter| {
            for area in [window(900.0), window(300.0), window(180.0)] {
                for paths in [&many, &few] {
                    let mut confirm = Confirm::new(ConfirmKind::Purge, paths.clone());
                    let g = confirm_geometry(painter, area, &confirm);
                    assert!(area.contains_rect(g.card), "{area:?}");
                    assert!(!g.rows.is_empty());
                    assert!(g.card.contains_rect(g.body));
                    assert!(g.body.bottom() <= g.actions[0].top(), "under the answers");
                    let overflows = paths.len() > g.rows.len();
                    assert_eq!(names_bar(&g, &confirm).is_some(), overflows);
                    assert_eq!(g.band.is_some(), overflows, "a band only with a bar");
                    confirm.fit(g.rows.len(), std::time::Instant::now());
                    confirm.scroll_by(100);
                    assert_eq!(confirm.scroll, paths.len() - g.rows.len());
                }
            }
            let tall = confirm_geometry(
                painter,
                window(900.0),
                &Confirm::new(ConfirmKind::Purge, many.clone()),
            );
            assert_eq!(tall.rows.len(), BODY_VISIBLE, "the old count");
            let short = confirm_geometry(
                painter,
                window(180.0),
                &Confirm::new(ConfirmKind::Purge, many.clone()),
            );
            assert!(short.rows.len() < BODY_VISIBLE);
            // A replace lists nothing, however tall the window.
            let replace = Confirm::new(ConfirmKind::Replace, vec![PathBuf::from("/tmp/a")]);
            let g = confirm_geometry(painter, window(900.0), &replace);
            assert!(g.rows.is_empty() && names_bar(&g, &replace).is_none());
        });
    }

    /// The resolver in a short window lists what fits above the comparison
    /// and the answers, and its cursor never walks below the last row drawn;
    /// in a tall one it lists five, as it always did.
    #[test]
    fn a_short_window_scrolls_the_resolvers_names() {
        let tree = TempTree::new("dialog-fit");
        let mut dialog = ConflictDialog::new(plan_of(&tree, 12));
        let two = TempTree::new("dialog-fit-two");
        let few = ConflictDialog::new(plan(&two));
        with_painter(|painter| {
            let g = conflict_geometry(painter, window(900.0), &dialog);
            assert_eq!(g.rows.len(), CONFLICT_VISIBLE, "the old count");
            assert!(conflicts_bar(&g, &dialog).is_some(), "twelve names in five");
            let g = conflict_geometry(painter, window(900.0), &few);
            assert_eq!(conflicts_bar(&g, &few), None, "two names fit");
            assert_eq!(g.band, None);

            let short = window(300.0);
            let g = conflict_geometry(painter, short, &dialog);
            assert!(short.contains_rect(g.card), "{:?}", g.card);
            assert!(!g.rows.is_empty() && g.rows.len() < CONFLICT_VISIBLE);
            // The comparison under the names ends above the answers.
            assert!(g.body.bottom() + 10.0 + FACTS_HEIGHT <= g.actions[0].top() + 1e-3);
            assert!(conflicts_bar(&g, &dialog).is_some());
            dialog.fit(g.rows.len(), std::time::Instant::now());
            for _ in 0..8 {
                dialog.move_cursor(1);
                let first = dialog.first_visible();
                assert!((first..first + g.rows.len()).contains(&dialog.cursor));
            }
        });
    }

    /// A click lands on the button it looks like it landed on.
    #[test]
    fn the_geometry_hit_tests_where_it_draws() {
        let tree = TempTree::new("dialog-geometry");
        let dialog = ConflictDialog::new(plan(&tree));
        with_painter(|painter| {
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            let confirm = Confirm::new(ConfirmKind::Delete, vec![PathBuf::from("/tmp/a")]);
            let geometry = confirm_geometry(painter, area, &confirm);
            assert_eq!(geometry.actions.len(), 2);
            for (i, rect) in geometry.actions.iter().enumerate() {
                assert_eq!(geometry.action_at(rect.center()), Some(i));
                assert!(geometry.card.contains(rect.center()));
            }
            assert_eq!(geometry.action_at(egui::pos2(-5.0, -5.0)), None);

            let geometry = conflict_geometry(painter, area, &dialog);
            assert_eq!(geometry.actions.len(), 3);
            assert_eq!(geometry.rows.len(), 2);
            assert!(geometry.apply_all.is_some());
            for (i, rect) in geometry.rows.iter().enumerate() {
                assert_eq!(geometry.row_at(rect.center()), Some(i));
            }
            // …and it survives a window too small to hold the card.
            let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 120.0));
            let small = conflict_geometry(painter, tiny, &dialog);
            assert!(small.card.height() <= tiny.height());
        });
    }

    /// The confirm card is a question, the names and two answers, laid out to
    /// a rhythm rather than to a template: sized to its own text, one left
    /// edge for the title and the names, and the buttons tucked into the
    /// corner at exactly the card's padding — which, with the card's radius
    /// being the buttons' plus that padding, is what makes the two corners
    /// concentric.
    #[test]
    fn the_confirm_card_fits_its_words_and_tucks_its_buttons_into_the_corner() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        with_painter(|painter| {
            let short = Confirm::new(ConfirmKind::Delete, vec![PathBuf::from("/tmp/a")]);
            let g = confirm_geometry(painter, area, &short);
            let title = painter
                .layout_no_wrap(short.title(), title_font(), egui::Color32::WHITE)
                .size();

            // As wide as the widest thing on it and no wider: a one-letter
            // delete is a card around its question, not a 620 pt slab.
            let buttons =
                button_width(painter, "Cancel") + BUTTON_GAP + button_width(painter, "Delete");
            let name = chrome::text_width(painter, "a", egui::FontId::proportional(FONT));
            let fitted = (title.x.max(buttons).max(name) + CARD_PAD * 2.0).max(CONFIRM_MIN_WIDTH);
            assert!(
                (g.card.width() - fitted).abs() < 1e-3,
                "{} is not {fitted}",
                g.card.width()
            );
            assert!(g.card.width() < MAX_WIDTH);

            // The corner. Exactly the padding in from the right and the
            // bottom, the gap between the two buttons, and the radii that make
            // those insets concentric.
            let (cancel, verb) = (g.actions[0], g.actions[1]);
            assert!((g.card.right() - verb.right() - CARD_PAD).abs() < 1e-3);
            assert!((g.card.bottom() - verb.bottom() - CARD_PAD).abs() < 1e-3);
            assert!((verb.left() - cancel.right() - BUTTON_GAP).abs() < 1e-3);
            assert_eq!(cancel.top(), verb.top());
            assert_eq!(
                f32::from(chrome::CARD_RADIUS),
                f32::from(ROW_RADIUS) + CARD_PAD,
                "the card's radius is the button's plus the inset"
            );
            assert!(cancel.width() >= BUTTON_MIN_WIDTH && verb.width() >= BUTTON_MIN_WIDTH);

            // The rhythm, top to bottom: pad, title, gap, names, gap, buttons,
            // pad — and the names start at the card's padding, not indented
            // past the title.
            let row = g.rows[0];
            assert!((row.left() - (g.card.left() + CARD_PAD)).abs() < 1e-3);
            assert!((row.top() - (g.card.top() + CARD_PAD + title.y + TITLE_GAP)).abs() < 1e-3);
            assert!((verb.top() - (g.body.bottom() + ANSWER_GAP)).abs() < 1e-3);

            // A long name widens the card to hold it — up to the cap and no
            // further, and never past the window.
            let long = "a-name-long-enough-to-want-a-wider-card-than-the-minimum.txt";
            let wide = Confirm::new(ConfirmKind::Delete, vec![PathBuf::from("/tmp").join(long)]);
            let w = confirm_geometry(painter, area, &wide);
            let needed = chrome::text_width(painter, long, egui::FontId::proportional(FONT));
            assert!(w.card.width() > CONFIRM_MIN_WIDTH);
            assert!(w.body.width() + 1e-3 >= needed, "the name has its room");
            let huge = Confirm::new(
                ConfirmKind::Delete,
                vec![PathBuf::from("/tmp").join(long.repeat(8))],
            );
            assert!(confirm_geometry(painter, area, &huge).card.width() <= MAX_WIDTH + 1e-3);
            let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(280.0, 400.0));
            let n = confirm_geometry(painter, narrow, &huge);
            assert!(n.card.width() <= narrow.width() - CARD_MARGIN * 2.0 + 1e-3);

            // Six names show, the rest scroll, and the card does not grow for
            // them.
            let many: Vec<PathBuf> = (0..20)
                .map(|i| PathBuf::from(format!("/tmp/{i}")))
                .collect();
            let m = confirm_geometry(painter, area, &Confirm::new(ConfirmKind::Purge, many));
            assert_eq!(m.rows.len(), BODY_VISIBLE);
            assert!((m.body.height() - BODY_VISIBLE as f32 * ROW).abs() < 1e-3);
        });
    }

    #[test]
    fn the_dialogs_paint_without_panicking() {
        let tree = TempTree::new("dialog-paint");
        let dialog = ConflictDialog::new(plan(&tree));
        let confirm = Confirm::new(ConfirmKind::Delete, vec![tree.join("src/a.txt")]);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let paint = Painting {
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            let hovers = Hovers::new();
            let ripples = Ripples::new();
            let g = conflict_geometry(ui.painter(), area, &dialog);
            paint_conflict(&paint, &dialog, &g, &hovers, &ripples);
            let g = confirm_geometry(ui.painter(), area, &confirm);
            paint_confirm(&paint, &confirm, &g, &hovers, &ripples);

            // A confirm longer than it shows, scrolled part way, so the
            // `+N more` marker draws beside a name that has to make room.
            let many: Vec<PathBuf> = (0..20)
                .map(|i| tree.join(&format!("a-rather-long-file-name-{i}.txt")))
                .collect();
            let mut long = Confirm::new(ConfirmKind::EmptyTrash, many);
            long.scroll_by(3);
            let g = confirm_geometry(ui.painter(), area, &long);
            paint_confirm(&paint, &long, &g, &hovers, &ripples);

            // The rename card, in the states it has: clean, rewritten by the
            // template, with the `{` list open, refused with carets on two
            // rows, and longer than it can show at once.
            let many: Vec<String> = (0..60).map(|i| format!("file-{i}.txt")).collect();
            let mut bulk = Bulk::new(
                tree.path.clone(),
                many.clone(),
                &many,
                df_core::fs::no_notifier(),
            )
            .expect("a local directory builds a card");
            let g = bulk_geometry(ui.painter(), area, &bulk);
            paint_bulk(&paint, &bulk, &g, &hovers, &ripples);

            bulk.insert_text("-x");
            let g = bulk_geometry(ui.painter(), area, &bulk);
            paint_bulk(&paint, &bulk, &g, &hovers, &ripples);

            bulk.insert_text("{");
            assert!(bulk.live_popover().is_some());
            let g = bulk_geometry(ui.painter(), area, &bulk);
            assert!(g.popover.is_some(), "the list is measured");
            paint_bulk(&paint, &bulk, &g, &hovers, &ripples);

            // Two rows wanting the same name, so the refusal treatment draws.
            for script in ["esc", "tab", "ctrl+a", "ctrl+k"] {
                bulk.key(chord(script));
            }
            bulk.insert_text("same");
            for script in ["down", "ctrl+a", "ctrl+k"] {
                bulk.key(chord(script));
            }
            bulk.insert_text("same");
            bulk.key(chord("ctrl+shift+down"));
            assert!(!bulk.valid());
            let g = bulk_geometry(ui.painter(), area, &bulk);
            let held = Painting {
                held: Some(crate::scrollbar::Bar::Bulk),
                ..paint
            };
            paint_bulk(&held, &bulk, &g, &hovers, &ripples);

            // …and a window with no room for a card at all.
            let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(160.0, 60.0));
            let g = bulk_geometry(ui.painter(), tiny, &bulk);
            paint_bulk(&paint, &bulk, &g, &hovers, &ripples);
        });
    }

    fn chord(script: &str) -> df_core::keymap::Chord {
        df_core::keymap::parse_chord(script).unwrap()
    }

    fn names(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("f{i}")).collect()
    }

    fn rename_card(count: usize) -> Bulk {
        Bulk::new(
            PathBuf::from("/tmp"),
            names(count),
            &[],
            df_core::fs::no_notifier(),
        )
        .expect("local")
    }

    /// The card's geometry: as many rows as the window has room for, a
    /// scrollbar for the rest, the template field above them, and the answers
    /// in the corner, the card's padding in from both edges.
    #[test]
    fn the_rename_card_shows_what_it_can_and_scrolls_the_rest() {
        let bulk = rename_card(60);
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        with_painter(|painter| {
            let g = bulk_geometry(painter, area, &bulk);
            assert_eq!(g.rows.len(), g.visible);
            assert!(g.visible < 60, "sixty rows do not fit in 900 points");
            assert!(g.bar.is_some(), "the rest are a scroll away");
            assert!((g.card.width() - BULK_MAX_WIDTH).abs() < 1e-3);
            assert!(g.card.width() > MAX_WIDTH, "a workspace, not a question");
            assert!(g.card.bottom() <= area.bottom() - CARD_MARGIN + 1e-3);
            // Every row is inside the body, and the field is above all of them.
            for row in &g.rows {
                assert!(g.body.contains_rect(row.rect));
                assert!(g.template.bottom() <= row.rect.top());
                assert!(row.new.left() > row.old.right());
            }
            // The answers sit the card's padding in from the right and the
            // bottom: concentric with its corner.
            let rename = g.actions[1];
            assert!((g.card.right() - rename.right() - CARD_PAD).abs() < 1e-3);
            assert!((g.card.bottom() - rename.bottom() - CARD_PAD).abs() < 1e-3);
            assert_eq!(g.close, chrome::close_button_rect(g.card));

            // One line of title and then the field: the status is not under
            // the title any more.
            let field_top = g.card.top() + CARD_PAD + ROW + BULK_FIELD_GAP;
            assert!((g.template.top() - field_top).abs() < 1e-3);
            // It shares the buttons' line instead, from the card's padding to
            // a gap short of `Cancel`, centred on the buttons.
            let cancel = g.actions[0];
            assert!((g.status.left() - (g.card.left() + CARD_PAD)).abs() < 1e-3);
            assert!((g.status.right() - (cancel.left() - crate::ui::GAP)).abs() < 1e-3);
            assert!((g.status.center().y - cancel.center().y).abs() < 1e-3);
            assert!(!g.status.intersects(cancel), "the status runs under Cancel");

            // What the pointer finds where.
            assert_eq!(g.hit(g.close.center()), Some(Control::Close));
            assert_eq!(g.hit(rename.center()), Some(Control::Action(1)));
            assert_eq!(g.hit(g.template.center()), Some(Control::BulkTemplate));
            assert_eq!(g.hit(g.rows[2].new.center()), Some(Control::BulkRow(2)));
            assert_eq!(
                g.hit(g.rows[2].old.center()),
                None,
                "the old name is history"
            );
            let bar = g.bar.expect("measured above");
            assert_eq!(
                g.hit(bar.thumb.center()),
                Some(Control::Bar(crate::scrollbar::Bar::Bulk))
            );

            // A short card is only as tall as it needs to be, with no bar:
            // the pad, the title, the field, two rows, the buttons' line.
            let two = bulk_geometry(painter, area, &rename_card(2));
            assert!(two.card.height() < g.card.height());
            assert!(two.bar.is_none());
            assert_eq!(two.visible, 2);
            let needed = CARD_PAD * 2.0
                + ROW
                + BULK_FIELD_GAP
                + BULK_ROW
                + BULK_LIST_GAP
                + 2.0 * BULK_ROW
                + ANSWER_GAP
                + BUTTON_HEIGHT;
            assert!((two.card.height() - needed).abs() < 1e-3);

            // A small window still gets a row, and a card inside it.
            let small = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(500.0, 260.0));
            let s = bulk_geometry(painter, small, &bulk);
            assert!(s.visible >= 1);
            assert!(small.contains_rect(s.card));
        });
    }

    /// The popover hangs under the line it completes, its text in line with
    /// the `{`, and its rows answer the pointer by candidate.
    #[test]
    fn the_popover_hangs_under_its_brace() {
        let mut bulk = rename_card(3);
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        bulk.insert_text("{");
        with_painter(|painter| {
            let g = bulk_geometry(painter, area, &bulk);
            let popover = g.popover.clone().expect("a `{` opens the list");
            assert!(popover.card.top() >= g.template.bottom());
            let brace = g.template_origin + x_of(&bulk_galley(painter, "{name}{ext}{"), 11);
            assert!((popover.card.left() + POPOVER_PAD + PAD_X - brace).abs() < 1.0);
            assert_eq!(popover.rows.len(), crate::bulk::POPOVER_ROWS);
            assert_eq!(
                g.hit(popover.rows[3].center()),
                Some(Control::BulkCandidate(3))
            );
            assert_eq!(g.rect_of(Control::BulkCandidate(3)), Some(popover.rows[3]));
        });
    }

    /// A click lands on the boundary nearest it, and a double click on the
    /// character it is over.
    ///
    /// The boundary is read off the card's own galley rather than measured
    /// as the width of `a`: egui snaps each glyph to a whole pixel, and in the
    /// monospace face that puts the first boundary half a point short of the
    /// advance.
    #[test]
    fn a_pointer_x_is_a_column() {
        with_painter(|painter| {
            assert_eq!(bulk_col_at(painter, "abc", 100.0, 50.0), 0);
            assert_eq!(bulk_col_at(painter, "abc", 100.0, 900.0), 3);
            let after_a = 100.0 + x_of(&bulk_galley(painter, "abc"), 1);
            assert_eq!(bulk_col_at(painter, "abc", 100.0, after_a + 0.5), 1);
            assert_eq!(bulk_char_at(painter, "abc", 100.0, after_a - 0.5), 0);
            assert_eq!(bulk_char_at(painter, "abc", 100.0, after_a + 0.5), 1);
        });
    }

    /// The reason the names are monospace: a caret at the same column is at
    /// the same x on every row, whatever the letters before it, so a stack of
    /// carets is a straight line and the old and new columns line up letter
    /// for letter.
    #[test]
    fn a_column_is_the_same_x_on_every_row() {
        with_painter(|painter| {
            let narrow = bulk_galley(painter, "iiii_1.txt");
            let wide = bulk_galley(painter, "MMMM_W.JPG");
            for col in 0..=10 {
                assert_eq!(x_of(&narrow, col), x_of(&wide, col), "column {col}");
            }
            assert!(x_of(&wide, 4) > 0.0, "the columns really are apart");
        });
    }

    /// [`changed_spans`] as `(start, end)` char offsets, which is how the
    /// expectations below are counted.
    fn added(old: &str, new: &str) -> Vec<(usize, usize)> {
        changed_spans(old, new)
            .into_iter()
            .map(|range| (range.start, range.end))
            .collect()
    }

    /// The diff highlight is what the new name adds.
    #[test]
    fn the_changed_span_is_what_was_added() {
        assert_eq!(added("a.txt", "a-2.txt"), [(1, 3)]);
        assert_eq!(added("aaa", "aaaa"), [(3, 4)]);
        assert_eq!(added("IMG_1", "2024"), [(0, 4)]);
        assert!(added("abc", "abc").is_empty());
    }

    /// A date put in front of a name that starts with a date shares its first
    /// characters with the old name by accident. The highlight is the date
    /// that was added, not the stretch between the two names' common ends.
    #[test]
    fn a_prefix_insertion_highlights_only_the_prefix() {
        assert_eq!(
            added("2026-01-01_x.mp4", "2026-08-05_2026-01-01_x.mp4"),
            [(0, 11)]
        );
        assert_eq!(added("a.txt", "x-a.txt"), [(0, 2)]);
    }

    #[test]
    fn a_suffix_insertion_highlights_only_the_suffix() {
        assert_eq!(added("photo.jpg", "photo.jpg.bak"), [(9, 13)]);
        assert_eq!(
            added("x.mp4", "x.mp4_x.mp4"),
            [(5, 11)],
            "the first x.mp4 is kept"
        );
    }

    #[test]
    fn an_insertion_on_both_sides_highlights_both() {
        assert_eq!(added("a.txt", "x-a.txt-y"), [(0, 2), (7, 9)]);
        assert_eq!(
            added("IMG_0001.jpg", "2026-IMG_0001.jpg.bak"),
            [(0, 5), (17, 21)]
        );
    }

    /// With the old name no longer in the new one, the diff is what lies
    /// between their common start and common end.
    #[test]
    fn an_edit_in_the_middle_still_highlights_the_middle() {
        assert_eq!(added("abc", "abXc"), [(2, 3)]);
        assert_eq!(added("IMG_0001.jpg", "IMG-0001.jpg"), [(3, 4)]);
    }

    #[test]
    fn a_pure_deletion_highlights_nothing() {
        assert!(changed_spans("abc", "ab").is_empty());
        assert!(changed_spans("2026-08-05_x.mp4", "x.mp4").is_empty());
        assert!(changed_spans("IMG_0001.jpg", "0001.jpg").is_empty());
    }
}
