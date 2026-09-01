//! The two modal cards a destructive operation goes through: the **confirm**
//! (`d`, `D`) and the **conflict resolver** (`p` onto a name that is taken).
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
use std::path::{Path, PathBuf};

use df_core::fs::Entry;
use df_core::ops::paste::{Conflict, PastePlan, Resolution};

use crate::chrome::{self, CARD_MARGIN, CARD_PAD, FONT, PAD_X};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting, ROW_RADIUS};

/// A dialog row's height. The same 20 pt every other card list uses.
const ROW: f32 = 20.0;

/// The action buttons' height. Taller than a row: they are the things being
/// pressed, and `delightful-ui` §1 wants a real hit target under them.
const BUTTON_HEIGHT: f32 = 26.0;

/// Between two buttons.
const BUTTON_GAP: f32 = 8.0;

/// The widest either dialog gets. Past this a two-column comparison stops being
/// a comparison — the eye cannot hold both halves at once.
const MAX_WIDTH: f32 = 620.0;

/// How many body lines a confirm shows before it scrolls. Six names is enough
/// to recognise the set you selected; past that the count in the title is the
/// thing being read anyway.
const BODY_VISIBLE: usize = 6;

/// How many conflicts the resolver lists at once, for the same reason.
const CONFLICT_VISIBLE: usize = 5;

/// The height of one side of the side-by-side comparison.
const FACTS_HEIGHT: f32 = 74.0;

// ── The confirm dialog (PLAN §4.1's `[confirm]` context) ────────────────────

/// Which irreversible thing is being confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    /// `d`. Undoable — the toast that follows says so.
    Trash,
    /// `D`. The one operation with no inverse (PLAN §5).
    Delete,
}

/// A yes/no card over the panes.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub kind: ConfirmKind,
    pub paths: Vec<PathBuf>,
    /// First visible body line — `↑`/`↓` scroll the body, as the `[confirm]`
    /// keymap says, rather than moving a cursor there is nothing to move.
    pub scroll: usize,
}

impl Confirm {
    pub fn new(kind: ConfirmKind, paths: Vec<PathBuf>) -> Confirm {
        Confirm {
            kind,
            paths,
            scroll: 0,
        }
    }

    /// yazi's custom-body style: the question names the verb, the count and
    /// where the files came from, so the title alone is enough to answer with.
    pub fn title(&self) -> String {
        let n = self.paths.len();
        let noun = if n == 1 { "file" } else { "files" };
        match self.kind {
            ConfirmKind::Trash => format!("Trash {n} selected {noun}?"),
            ConfirmKind::Delete => format!("Delete {n} selected {noun}?"),
        }
    }

    /// The line under the title: what this will actually do.
    pub fn subtitle(&self) -> &'static str {
        match self.kind {
            ConfirmKind::Trash => "They go to the trash — u puts them back.",
            ConfirmKind::Delete => "Permanently delete — cannot be undone.",
        }
    }

    pub fn danger(&self) -> bool {
        self.kind == ConfirmKind::Delete
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
        let max = self.paths.len().saturating_sub(BODY_VISIBLE);
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, max as isize) as usize;
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
            ConflictAction::Rename => Resolution::Rename(PathBuf::from(
                suggested.file_name().unwrap_or_default(),
            )),
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
}

impl ConflictDialog {
    pub fn new(plan: PastePlan) -> ConflictDialog {
        let mut dialog = ConflictDialog {
            plan,
            cursor: 0,
            action: ConflictAction::Rename,
            apply_all: false,
            error: None,
            facts: HashMap::new(),
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
            self.facts.entry(path.clone()).or_insert_with(|| Facts::read(&path));
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
        Step::Continue
    }

    /// Where the resolver's first visible conflict row is, so a long list
    /// scrolls with the cursor instead of hiding it.
    fn first_visible(&self) -> usize {
        self.cursor.saturating_sub(CONFLICT_VISIBLE.saturating_sub(1))
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

/// A card of `height`, centred horizontally and biased *above* true centre —
/// `delightful-ui` §16: content centred in a big region reads as sitting low.
fn card_rect(area: egui::Rect, height: f32) -> egui::Rect {
    let width = (area.width() - CARD_MARGIN * 2.0).min(MAX_WIDTH);
    let height = height.min((area.height() - CARD_MARGIN * 2.0).max(0.0));
    // 40 % of the free space above, 60 % below.
    let top = area.top() + (area.height() - height).max(0.0) * 0.4;
    egui::Rect::from_min_size(
        egui::pos2(area.center().x - width / 2.0, top),
        egui::vec2(width.max(0.0), height),
    )
}

/// Lay out the confirm card.
pub fn confirm_geometry(area: egui::Rect, confirm: &Confirm) -> Geometry {
    let lines = confirm.paths.len().min(BODY_VISIBLE);
    let height = CARD_PAD * 2.0
        + ROW * 2.0                       // title and subtitle
        + 6.0
        + lines as f32 * ROW
        + 10.0
        + BUTTON_HEIGHT;
    let card = card_rect(area, height);
    let inner_left = card.left() + CARD_PAD;
    let inner_right = card.right() - CARD_PAD;
    let body_top = card.top() + CARD_PAD + ROW * 2.0 + 6.0;
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
    // Cancel first, the committing button last and rightmost: the destructive
    // one is the furthest from where the pointer rests after opening the card.
    let actions = button_row(
        inner_right,
        card.bottom() - CARD_PAD - BUTTON_HEIGHT,
        &["Cancel", confirm_verb(confirm.kind)],
    );
    Geometry {
        card,
        body,
        rows,
        actions,
        apply_all: None,
    }
}

pub fn confirm_verb(kind: ConfirmKind) -> &'static str {
    match kind {
        ConfirmKind::Trash => "Trash",
        ConfirmKind::Delete => "Delete",
    }
}

/// Lay out the conflict card.
pub fn conflict_geometry(area: egui::Rect, dialog: &ConflictDialog) -> Geometry {
    let listed = dialog.len().min(CONFLICT_VISIBLE);
    let height = CARD_PAD * 2.0
        + ROW * 2.0                       // title and subtitle
        + 6.0
        + listed as f32 * ROW
        + 10.0
        + FACTS_HEIGHT
        + 10.0
        + BUTTON_HEIGHT;
    let card = card_rect(area, height);
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
    let buttons_top = card.bottom() - CARD_PAD - BUTTON_HEIGHT;
    let labels: Vec<&str> = ConflictAction::ALL.iter().map(|a| a.label()).collect();
    let actions = button_row(inner_right, buttons_top, &labels);
    // The toggle sits on the left of the same line as the buttons: it modifies
    // what pressing one of them means, so it must be read before them.
    let apply_all = Some(egui::Rect::from_min_size(
        egui::pos2(inner_left, buttons_top),
        egui::vec2(110.0, BUTTON_HEIGHT),
    ));
    Geometry {
        card,
        body,
        rows,
        actions,
        apply_all,
    }
}

/// Buttons laid right-to-left from `right`, returned left-to-right.
fn button_row(right: f32, top: f32, labels: &[&str]) -> Vec<egui::Rect> {
    let mut rects = Vec::new();
    let mut x = right;
    for label in labels.iter().rev() {
        // Measured from the label length without a painter: every dialog button
        // is short, and a fixed 7 pt per character plus padding is stable
        // across frames, which a re-measured width is not.
        let width = (label.chars().count() as f32 * 7.0 + PAD_X * 2.0).max(64.0);
        rects.push(egui::Rect::from_min_size(
            egui::pos2(x - width, top),
            egui::vec2(width, BUTTON_HEIGHT),
        ));
        x -= width + BUTTON_GAP;
    }
    rects.reverse();
    rects
}

// ── Painting ────────────────────────────────────────────────────────────────

/// Draw the confirm card over a scrim.
pub fn paint_confirm(
    paint: &Painting<'_>,
    area: egui::Rect,
    confirm: &Confirm,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    painter.rect_filled(area, 0, egui::Color32::from_black_alpha(chrome::HELP_SCRIM));
    chrome::card(paint, geometry.card, 1.0);

    let accent = if confirm.danger() {
        palette.red
    } else {
        palette.yellow
    };
    let left = geometry.card.left() + CARD_PAD;
    painter.text(
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW / 2.0),
        egui::Align2::LEFT_CENTER,
        confirm.title(),
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
    );
    painter.text(
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW + ROW / 2.0),
        egui::Align2::LEFT_CENTER,
        confirm.subtitle(),
        egui::FontId::proportional(FONT),
        accent,
    );

    let body = confirm.body();
    let clipped = painter.with_clip_rect(geometry.body);
    for (i, rect) in geometry.rows.iter().enumerate() {
        let Some(line) = body.get(confirm.scroll + i) else {
            break;
        };
        chrome::truncated(
            &clipped,
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            line,
            palette.subtext0,
            (rect.width() - PAD_X * 2.0).max(0.0),
        );
    }
    if body.len() > geometry.rows.len() {
        let more = body.len() - geometry.rows.len() - confirm.scroll;
        if more > 0 {
            painter.text(
                egui::pos2(geometry.body.right(), geometry.body.bottom() - ROW / 2.0),
                egui::Align2::RIGHT_CENTER,
                format!("+{more} more"),
                egui::FontId::proportional(FONT - 1.0),
                palette.overlay0,
            );
        }
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

/// Draw the conflict resolver.
pub fn paint_conflict(
    paint: &Painting<'_>,
    area: egui::Rect,
    dialog: &ConflictDialog,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    painter.rect_filled(area, 0, egui::Color32::from_black_alpha(chrome::HELP_SCRIM));
    chrome::card(paint, geometry.card, 1.0);

    let left = geometry.card.left() + CARD_PAD;
    let n = dialog.len();
    painter.text(
        egui::pos2(left, geometry.card.top() + CARD_PAD + ROW / 2.0),
        egui::Align2::LEFT_CENTER,
        if n == 1 {
            "A file with that name is already there".to_string()
        } else {
            format!("{n} names are already taken")
        },
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
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
        let key = Control::PanelRow(first + i);
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
        if selected { palette.text } else { palette.subtext0 },
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

    fn plan(tree: &TempTree) -> PastePlan {
        let a = tree.file("src/a.txt", b"new");
        let b = tree.file("src/b.txt", b"new");
        tree.file("dst/a.txt", b"old");
        tree.file("dst/b.txt", b"old");
        plan_paste(&Clipboard::yank([a, b]), &tree.join("dst"), false).unwrap()
    }

    #[test]
    fn a_confirm_says_what_it_is_about_to_do() {
        let trash = Confirm::new(
            ConfirmKind::Trash,
            vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")],
        );
        assert_eq!(trash.title(), "Trash 2 selected files?");
        assert!(!trash.danger());
        assert!(trash.subtitle().contains('u'), "it says how to take it back");

        let one = Confirm::new(ConfirmKind::Delete, vec![PathBuf::from("/tmp/a")]);
        assert_eq!(one.title(), "Delete 1 selected file?");
        assert!(one.danger());
        assert_eq!(one.subtitle(), "Permanently delete — cannot be undone.");
        assert_eq!(one.body(), vec!["a".to_string()]);
    }

    /// The body scrolls and stops — `↑` at the top and `↓` at the bottom are
    /// no-ops rather than a list that walks off its own end.
    #[test]
    fn the_confirm_body_scrolls_within_its_bounds() {
        let paths: Vec<PathBuf> = (0..10).map(|i| PathBuf::from(format!("/tmp/{i}"))).collect();
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
        assert!(!suggested.contains('/'), "the suggestion is a name: {suggested}");

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
        assert_eq!(dialog.action, ConflictAction::Rename, "left from the first wraps");
        dialog.cycle_action(1);
        assert_eq!(dialog.action, ConflictAction::Overwrite);
        assert_eq!(
            ConflictDialog::action_for_key('s'),
            Some(ConflictAction::Skip)
        );
        assert_eq!(ConflictDialog::action_for_key('q'), None);
    }

    /// A click lands on the button it looks like it landed on.
    #[test]
    fn the_geometry_hit_tests_where_it_draws() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let confirm = Confirm::new(ConfirmKind::Delete, vec![PathBuf::from("/tmp/a")]);
        let geometry = confirm_geometry(area, &confirm);
        assert_eq!(geometry.actions.len(), 2);
        for (i, rect) in geometry.actions.iter().enumerate() {
            assert_eq!(geometry.action_at(rect.center()), Some(i));
            assert!(geometry.card.contains(rect.center()));
        }
        assert_eq!(geometry.action_at(egui::pos2(-5.0, -5.0)), None);

        let tree = TempTree::new("dialog-geometry");
        let dialog = ConflictDialog::new(plan(&tree));
        let geometry = conflict_geometry(area, &dialog);
        assert_eq!(geometry.actions.len(), 3);
        assert_eq!(geometry.rows.len(), 2);
        assert!(geometry.apply_all.is_some());
        for (i, rect) in geometry.rows.iter().enumerate() {
            assert_eq!(geometry.row_at(rect.center()), Some(i));
        }
        // …and it survives a window too small to hold the card.
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 120.0));
        let small = conflict_geometry(tiny, &dialog);
        assert!(small.card.height() <= tiny.height());
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
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            let hovers = Hovers::new();
            let ripples = Ripples::new();
            let g = conflict_geometry(area, &dialog);
            paint_conflict(&paint, area, &dialog, &g, &hovers, &ripples);
            let g = confirm_geometry(area, &confirm);
            paint_confirm(&paint, area, &confirm, &g, &hovers, &ripples);
        });
    }
}
