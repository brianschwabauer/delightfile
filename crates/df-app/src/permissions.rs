//! The permissions card (`C`): the nine bits of a selection as a grid of
//! checkboxes, with the octal number under them, applied as one operation
//! that `u` takes back.
//!
//! The spot panel's chips already flip a bit on one file at a time. This card
//! is for the other jobs: a selection, a folder and everything in it, a mode
//! typed as `755`. Like [`crate::dialog`] and [`crate::sync`], everything here
//! that decides anything is a plain state machine over df-core's
//! [`Grid`](df_core::ops::mode::Grid), and the app wires it to the keyboard,
//! the pointer and the task engine (`app/permissions.rs`).
//!
//! ## Mixed
//!
//! Two files that differ in a bit show that cell as a dash, and a dash left
//! alone is left alone on disk: each file keeps its own bit there. A press on
//! a dash turns it on for all of them, as a mixed checkbox does everywhere.
//! The octal field writes a mixed triad as a dash too, since there is no
//! digit for "some of them".
//!
//! ## The field
//!
//! The field mirrors the grid, and is typed into: a digit goes to the field
//! wherever the keyboard is on the card, and the first one replaces what it
//! showed, as a field whose text is selected would. Three digits (or four,
//! the first being the special bits', which the card never changes) set the
//! grid, mixed cells included. One or two digits are not a mode yet, and
//! `Enter` waits for the third rather than guessing.

use std::collections::HashSet;
use std::path::PathBuf;

use df_core::fs::{Entry, Kind};
use df_core::keymap::{Chord, Command, Key, Mods};
use df_core::ops::mode::{Cell, Grid, MODE_BITS};
use df_core::text::grouped;

use crate::chrome::{self, Hint, CARD_PAD, FONT, PAD_X};
use crate::dialog::{self, ANSWER_GAP, BUTTON_HEIGHT, ROW};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting, ROW_RADIUS};

/// The card's width. Room for the row labels, three columns of boxes and
/// the hint strip's five hints on one line; a name longer than that loses
/// its middle rather than widening the card.
pub const WIDTH: f32 = 420.0;

/// The row labels' column: "Owner", "Group", "Others", "Octal", "Owned by".
const LABEL: f32 = 76.0;

/// One of the three columns, Read, Write and Execute. Each box's target is
/// its whole cell, far over the 24-point floor (`delightful-ui` §1).
const COLUMN: f32 = 76.0;

/// A row of boxes, the field's row and the checkbox's: a box's target is
/// this tall.
const CELL_ROW: f32 = 26.0;

/// The box drawn inside a cell.
const BOX: f32 = 16.0;

/// The octal field: four digits and room either side of them.
const FIELD_WIDTH: f32 = 64.0;
const FIELD_HEIGHT: f32 = 24.0;

/// Between the heading and the grid, and between the grid and the field.
const SECTION_GAP: f32 = 8.0;

/// The column headings, left to right.
pub const COLUMNS: [&str; 3] = ["Read", "Write", "Execute"];

/// The row labels, top to bottom.
pub const ROWS: [&str; 3] = ["Owner", "Group", "Others"];

/// The checkbox's words, and the line under it while it is ticked.
pub const RECURSIVE_LABEL: &str = "Apply to everything inside";
pub const RECURSIVE_NOTE: &str = "Folders also get execute wherever they get read";

/// The card's controls, as [`Control::Action`] indices: the two buttons
/// first, as on every other card, then the nine boxes, the field and the
/// checkbox.
pub const CANCEL: usize = 0;
pub const APPLY: usize = 1;
const FIRST_CELL: usize = 2;
pub const FIELD: usize = FIRST_CELL + 9;
pub const RECURSIVE: usize = FIELD + 1;

/// The control a box is.
pub fn cell_control(index: usize) -> Control {
    Control::Action(FIRST_CELL + index)
}

/// One item the card is about, from the row the listing already has: the
/// card reads nothing from the disk to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub path: PathBuf,
    pub name: String,
    /// The whole `st_mode`, as the listing read it.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// A folder itself, not a link to one: links are never subjects.
    pub is_dir: bool,
}

impl Subject {
    pub fn of(entry: &Entry) -> Subject {
        Subject {
            path: entry.path.clone(),
            name: entry.name.clone(),
            mode: entry.mode,
            uid: entry.uid,
            gid: entry.gid,
            is_dir: matches!(entry.kind, Kind::Dir),
        }
    }
}

/// Where the keyboard is on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// A box, in [`df_core::ops::mode::BITS`] order: row × 3 + column.
    Cell(usize),
    Field,
    Recursive,
}

/// What a keystroke asks of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Handled on the card.
    Consumed,
    /// Not the card's: the registry's, which is where `Enter` and `Esc` are.
    Ignored,
    /// Carry the change out.
    Submit,
    /// Close the card: `Enter` with nothing to change.
    Close,
}

/// The card, while it is up.
pub struct PermCard {
    pub subjects: Vec<Subject>,
    pub grid: Grid,
    /// What the field shows: the grid's octal while it mirrors the grid, or
    /// what was typed.
    pub field: String,
    /// The field is showing the grid rather than typing: the next digit
    /// replaces it.
    mirrors: bool,
    pub focus: Focus,
    /// The column the keyboard left the grid from, so `↑` out of the field
    /// comes back to it.
    column: usize,
    pub recursive: bool,
    /// The server, for a card over SFTP: one level only, and no undo.
    pub host: Option<String>,
    /// The spot panel the card was opened from, put back when it closes.
    pub spot: Option<Box<crate::spot::Spot>>,
}

impl PermCard {
    /// The card over `subjects`, which must not be empty. `host` is the
    /// server's name when they are on one.
    pub fn new(subjects: Vec<Subject>, host: Option<String>) -> PermCard {
        let grid = Grid::of(subjects.iter().map(|subject| subject.mode));
        PermCard {
            field: grid.octal(),
            grid,
            subjects,
            mirrors: true,
            focus: Focus::Cell(0),
            column: 0,
            recursive: false,
            host,
            spot: None,
        }
    }

    /// The line under the title: the file's name, or how many there are.
    pub fn subtitle(&self) -> String {
        match self.subjects.as_slice() {
            [one] => one.name.clone(),
            many => format!("{} items", grouped(many.len() as u64)),
        }
    }

    /// Who owns them, `brian · users`, read only. Names from this machine's
    /// `/etc/passwd` and `/etc/group` ([`df_core::fs::owner`]) and the number
    /// where there is none; on a server always the number, because the names
    /// here are not the server's. Several owners are counted, not listed.
    pub fn owner_line(&self) -> String {
        let local = self.host.is_none();
        let user = |uid: u32| {
            local
                .then(|| df_core::fs::owner::user_name(uid))
                .flatten()
                .map_or_else(|| uid.to_string(), str::to_string)
        };
        let group = |gid: u32| {
            local
                .then(|| df_core::fs::owner::group_name(gid))
                .flatten()
                .map_or_else(|| gid.to_string(), str::to_string)
        };
        let uids: HashSet<u32> = self.subjects.iter().map(|s| s.uid).collect();
        let gids: HashSet<u32> = self.subjects.iter().map(|s| s.gid).collect();
        let owners = match uids.iter().next() {
            Some(uid) if uids.len() == 1 => user(*uid),
            _ => format!("{} owners", grouped(uids.len() as u64)),
        };
        let groups = match gids.iter().next() {
            Some(gid) if gids.len() == 1 => group(*gid),
            _ => format!("{} groups", grouped(gids.len() as u64)),
        };
        format!("{owners} · {groups}")
    }

    /// Whether the checkbox is on the card: a folder among the subjects, on
    /// this machine.
    pub fn has_folder(&self) -> bool {
        self.host.is_none() && self.subjects.iter().any(|subject| subject.is_dir)
    }

    /// What the change is about: every subject's path.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.subjects.iter().map(|s| s.path.clone()).collect()
    }

    /// Whether the field holds one or two digits: not a mode yet.
    pub fn partial(&self) -> bool {
        !self.mirrors && Grid::parse(&self.field).is_none()
    }

    /// Whether applying would change anything. Everything inside a folder is
    /// not known to the card, so a change that goes inside always might.
    pub fn changes(&self) -> bool {
        self.recursive
            || self.subjects.iter().any(|subject| {
                self.grid.apply(subject.mode) & MODE_BITS != subject.mode & MODE_BITS
            })
    }

    /// The line beside the buttons, and whether it is a warning: why `Enter`
    /// is waiting, what a fourth digit does, or that a server keeps no undo.
    pub fn status(&self) -> Option<(String, bool)> {
        if self.partial() {
            return Some(("Three digits, like 755".to_string(), true));
        }
        if !self.mirrors && self.field.len() == 4 && !self.field.starts_with('0') {
            return Some((
                "The first digit is kept as it is on each item".to_string(),
                false,
            ));
        }
        self.host
            .as_ref()
            .map(|host| (format!("On {host} · no undo on a server"), false))
    }

    // ── Keys ────────────────────────────────────────────────────────────────

    /// One keystroke. The arrows, `Space`, `Tab`, the digits and `Backspace`
    /// are the card's; `Enter` and `Esc` are the registry's, so the hints
    /// that press them run what the keys run.
    pub fn key(&mut self, chord: Chord) -> Outcome {
        let plain = chord.mods.is_none();
        if plain {
            if let Key::Char(c @ '0'..='7') = chord.key {
                self.type_digit(c);
                return Outcome::Consumed;
            }
        }
        match chord.key {
            // Not octal: swallowed rather than handed to the registry, where
            // a stray `8` would mean nothing anyway.
            Key::Char('8' | '9') if plain => {}
            Key::ArrowLeft if plain => self.step(0, -1),
            Key::ArrowRight if plain => self.step(0, 1),
            Key::ArrowUp if plain => self.step(-1, 0),
            Key::ArrowDown if plain => self.step(1, 0),
            Key::Tab if plain => self.cycle(true),
            Key::Tab if chord.mods == Mods::SHIFT => self.cycle(false),
            Key::Space if plain => self.press_focus(),
            Key::Backspace if plain => self.backspace(),
            _ => return Outcome::Ignored,
        }
        Outcome::Consumed
    }

    /// `Enter`, and the Apply button: carry it out, unless the field is half
    /// typed (the status says so) or there is nothing to change.
    pub fn enter(&self) -> Outcome {
        if self.partial() {
            return Outcome::Consumed;
        }
        if !self.changes() {
            return Outcome::Close;
        }
        Outcome::Submit
    }

    /// Move the keyboard `rows` down and `columns` right: across the grid,
    /// then down out of it into the field and on to the checkbox.
    fn step(&mut self, rows: isize, columns: isize) {
        self.focus = match self.focus {
            Focus::Cell(index) => {
                let (row, column) = ((index / 3) as isize, (index % 3) as isize);
                let column = (column + columns).clamp(0, 2);
                let row = row + rows;
                if row > 2 {
                    self.column = column as usize;
                    Focus::Field
                } else {
                    Focus::Cell((row.max(0) * 3 + column) as usize)
                }
            }
            Focus::Field if rows < 0 => Focus::Cell(6 + self.column),
            Focus::Field if rows > 0 && self.has_folder() => Focus::Recursive,
            Focus::Recursive if rows < 0 => Focus::Field,
            other => other,
        };
    }

    /// `Tab`: the grid, the field, the checkbox, round again.
    fn cycle(&mut self, forward: bool) {
        let mut stops = vec![Focus::Cell(self.column), Focus::Field];
        if self.has_folder() {
            stops.push(Focus::Recursive);
        }
        let here = stops
            .iter()
            .position(|stop| match (stop, self.focus) {
                (Focus::Cell(_), Focus::Cell(_)) => true,
                (stop, focus) => *stop == focus,
            })
            .unwrap_or(0);
        let next = if forward {
            (here + 1) % stops.len()
        } else {
            (here + stops.len() - 1) % stops.len()
        };
        if let Focus::Cell(index) = self.focus {
            self.column = index % 3;
        }
        self.focus = stops[next];
    }

    /// `Space`: tick the box the keyboard is on.
    fn press_focus(&mut self) {
        match self.focus {
            Focus::Cell(index) => self.toggle(index),
            Focus::Recursive => self.toggle_recursive(),
            Focus::Field => {}
        }
    }

    /// Flip one box, and the field follows the grid again.
    pub fn toggle(&mut self, index: usize) {
        self.grid.toggle(index);
        self.focus = Focus::Cell(index);
        self.column = index % 3;
        self.mirror();
    }

    pub fn toggle_recursive(&mut self) {
        if self.has_folder() {
            self.recursive = !self.recursive;
            self.focus = Focus::Recursive;
        }
    }

    /// Put the keyboard in the field, its text still the grid's until a
    /// digit replaces it.
    pub fn focus_field(&mut self) {
        if let Focus::Cell(index) = self.focus {
            self.column = index % 3;
        }
        self.focus = Focus::Field;
    }

    /// A digit, into the field from wherever the keyboard is.
    fn type_digit(&mut self, digit: char) {
        self.focus_field();
        if self.mirrors {
            self.field.clear();
            self.mirrors = false;
        }
        if self.field.len() < 4 {
            self.field.push(digit);
        }
        self.read_field();
    }

    fn backspace(&mut self) {
        if self.focus != Focus::Field {
            return;
        }
        if self.mirrors {
            self.field.clear();
            self.mirrors = false;
        } else {
            self.field.pop();
        }
        self.read_field();
    }

    /// Three or four digits in the field are the grid from now on.
    fn read_field(&mut self) {
        if let Some(grid) = Grid::parse(&self.field) {
            self.grid = grid;
        }
    }

    /// The field shows the grid again.
    fn mirror(&mut self) {
        self.field = self.grid.octal();
        self.mirrors = true;
    }

    /// Whether the field is showing the grid rather than something typed.
    pub fn mirrors(&self) -> bool {
        self.mirrors
    }

    /// A press on one of the card's controls other than its two buttons.
    pub fn click(&mut self, index: usize) {
        match index {
            FIELD => self.focus_field(),
            RECURSIVE => self.toggle_recursive(),
            index if (FIRST_CELL..FIELD).contains(&index) => self.toggle(index - FIRST_CELL),
            _ => {}
        }
    }

    /// The card's hint strip.
    pub fn hints(&self) -> Vec<Hint> {
        vec![
            Hint::inert("↑↓←→", "move"),
            Hint::key("Space", "toggle", Chord::plain(Key::Space)),
            Hint::inert("0–7", "octal"),
            Hint::new("Enter", "apply", Command::OverlaySubmit),
            Hint::new("Esc", "cancel", Command::OverlayClose),
        ]
    }
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// Where the card's pieces are: one measurement for the hit test and the
/// paint.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    pub card: egui::Rect,
    /// The title's and the subtitle's lines.
    pub title: egui::Rect,
    pub subtitle: egui::Rect,
    /// The line the column headings sit on.
    pub header: egui::Rect,
    /// The three rows of boxes, full width.
    pub rows: [egui::Rect; 3],
    /// Each box's cell, which is its target, in bit order.
    pub cells: [egui::Rect; 9],
    /// The field's row, and the field in it.
    pub field_row: egui::Rect,
    pub field: egui::Rect,
    pub owner: egui::Rect,
    /// The checkbox's target, and the note's line under it, when there is a
    /// folder to go into. The note's line is held whether or not the box is
    /// ticked, so ticking it moves nothing under the pointer.
    pub recursive: Option<egui::Rect>,
    pub note: Option<egui::Rect>,
    /// The status, on the buttons' line up to a gap short of the first.
    pub status: egui::Rect,
    /// Cancel and Apply.
    pub actions: Vec<egui::Rect>,
}

impl Geometry {
    pub fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        if let Some(index) = self.actions.iter().position(|r| r.contains(pos)) {
            return Some(Control::Action(index));
        }
        if let Some(index) = self.cells.iter().position(|r| r.contains(pos)) {
            return Some(cell_control(index));
        }
        if self.field.contains(pos) {
            return Some(Control::Action(FIELD));
        }
        if self.recursive.is_some_and(|r| r.contains(pos)) {
            return Some(Control::Action(RECURSIVE));
        }
        None
    }

    pub fn rect_of(&self, control: Control) -> Option<egui::Rect> {
        match control {
            Control::Action(index) if index < FIRST_CELL => self.actions.get(index).copied(),
            Control::Action(FIELD) => Some(self.field),
            Control::Action(RECURSIVE) => self.recursive,
            Control::Action(index) => self.cells.get(index - FIRST_CELL).copied(),
            _ => None,
        }
    }
}

/// Lay the card out: the pad, the title and the subtitle, [`SECTION_GAP`],
/// the column headings and three rows of boxes, [`SECTION_GAP`], the field,
/// the owner, the checkbox and its note when there is a folder, then
/// [`ANSWER_GAP`], the status and the buttons on one line, the hint strip and
/// the pad. The buttons are the confirm card's ([`dialog::button_row`]), the
/// last [`CARD_PAD`] in from the corner, so its corner is concentric with the
/// card's.
pub fn geometry(painter: &egui::Painter, area: egui::Rect, card: &PermCard) -> Geometry {
    let folder = card.has_folder();
    let height = CARD_PAD
        + ROW * 2.0
        + SECTION_GAP
        + ROW
        + CELL_ROW * 3.0
        + SECTION_GAP
        + CELL_ROW
        + ROW
        + if folder { CELL_ROW + ROW } else { 0.0 }
        + ANSWER_GAP
        + BUTTON_HEIGHT
        + chrome::HINT_ROW
        + CARD_PAD;
    let rect = dialog::place_card(area, WIDTH, height);
    let left = rect.left() + CARD_PAD;
    let right = rect.right() - CARD_PAD;
    let line = |top: f32, height: f32| {
        egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, top + height))
    };
    let mut y = rect.top() + CARD_PAD;
    let title = line(y, ROW);
    y += ROW;
    let subtitle = line(y, ROW);
    y += ROW + SECTION_GAP;
    let header = line(y, ROW);
    y += ROW;
    let rows = [0, 1, 2].map(|row| line(y + row as f32 * CELL_ROW, CELL_ROW));
    let cells = std::array::from_fn(|index| {
        let (row, column) = (index / 3, index % 3);
        egui::Rect::from_min_size(
            egui::pos2(left + LABEL + column as f32 * COLUMN, rows[row].top()),
            egui::vec2(COLUMN, CELL_ROW),
        )
    });
    y += CELL_ROW * 3.0 + SECTION_GAP;
    let field_row = line(y, CELL_ROW);
    let field = egui::Rect::from_min_size(
        egui::pos2(
            left + LABEL + (COLUMN - FIELD_WIDTH) / 2.0,
            field_row.center().y - FIELD_HEIGHT / 2.0,
        ),
        egui::vec2(FIELD_WIDTH, FIELD_HEIGHT),
    );
    y += CELL_ROW;
    let owner = line(y, ROW);
    y += ROW;
    let (recursive, note) = if folder {
        // The target is the box and its words, not the whole line: a press
        // out in the empty end of the line ticking a box would be a press
        // landing on something it was not aimed at.
        let words = chrome::text_width(painter, RECURSIVE_LABEL, egui::FontId::proportional(FONT));
        let target = egui::Rect::from_min_size(
            egui::pos2(left, y),
            egui::vec2(
                (BOX + PAD_X + words + PAD_X * 2.0).min(right - left),
                CELL_ROW,
            ),
        );
        (Some(target), Some(line(y + CELL_ROW, ROW)))
    } else {
        (None, None)
    };
    // The buttons from the bottom up, which is where the height above put
    // them: the checkbox's two lines are in that height only with a folder.
    let buttons_top = rect.bottom() - CARD_PAD - chrome::HINT_ROW - BUTTON_HEIGHT;
    let actions = dialog::button_row(painter, right, buttons_top, &["Cancel", "Apply"]);
    let first_button = actions.first().map_or(right, egui::Rect::left);
    let status = egui::Rect::from_min_max(
        egui::pos2(left, buttons_top),
        egui::pos2(
            (first_button - crate::ui::GAP).max(left),
            buttons_top + BUTTON_HEIGHT,
        ),
    );
    Geometry {
        card: rect,
        title,
        subtitle,
        header,
        rows,
        cells,
        field_row,
        field,
        owner,
        recursive,
        note,
        status,
        actions,
    }
}

// ── Painting ────────────────────────────────────────────────────────────────

/// Draw the card, over the scrim the app lays for it.
pub fn paint(
    paint: &Painting<'_>,
    card: &PermCard,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    let font = egui::FontId::proportional(FONT);
    let small = egui::FontId::proportional(FONT - 1.0);
    chrome::card(paint, geometry.card, 1.0);

    let left = geometry.title.left();
    let width = geometry.title.width().max(0.0);
    chrome::truncated_in(
        painter,
        egui::pos2(left, geometry.title.center().y),
        "Permissions",
        palette.text,
        width,
        dialog::title_font(),
    );
    // A long name loses its middle, not its end: the end is the extension,
    // and the start is what tells it from its neighbours.
    let subtitle = chrome::elide_middle(painter, &card.subtitle(), font.clone(), width);
    painter.text(
        egui::pos2(left, geometry.subtitle.center().y),
        egui::Align2::LEFT_CENTER,
        subtitle,
        font.clone(),
        palette.subtext0,
    );

    for (column, heading) in COLUMNS.iter().enumerate() {
        painter.text(
            egui::pos2(
                geometry.cells[column].center().x,
                geometry.header.center().y,
            ),
            egui::Align2::CENTER_CENTER,
            heading,
            small.clone(),
            palette.overlay1,
        );
    }
    for (row, label) in ROWS.iter().enumerate() {
        painter.text(
            egui::pos2(left, geometry.rows[row].center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font.clone(),
            palette.subtext0,
        );
    }
    for (index, cell) in geometry.cells.iter().enumerate() {
        let key = cell_control(index);
        let focused = card.focus == Focus::Cell(index);
        let rect = pressed_rect(*cell, hovers.press(key));
        plate(paint, rect, key, hovers, ripples);
        checkbox(
            paint,
            egui::Rect::from_center_size(rect.center(), egui::vec2(BOX, BOX)),
            card.grid.cells[index],
            focused,
        );
    }

    // The field, under the Read column.
    painter.text(
        egui::pos2(left, geometry.field_row.center().y),
        egui::Align2::LEFT_CENTER,
        "Octal",
        font.clone(),
        palette.subtext0,
    );
    field(paint, card, geometry.field, hovers);

    painter.text(
        egui::pos2(left, geometry.owner.center().y),
        egui::Align2::LEFT_CENTER,
        "Owned by",
        font.clone(),
        palette.subtext0,
    );
    chrome::truncated_in(
        painter,
        egui::pos2(left + LABEL, geometry.owner.center().y),
        &card.owner_line(),
        palette.overlay1,
        (geometry.owner.right() - left - LABEL).max(0.0),
        font.clone(),
    );

    if let Some(target) = geometry.recursive {
        let key = Control::Action(RECURSIVE);
        let rect = pressed_rect(target, hovers.press(key));
        plate(paint, rect, key, hovers, ripples);
        let tick = egui::Rect::from_center_size(
            egui::pos2(rect.left() + PAD_X / 2.0 + BOX / 2.0, rect.center().y),
            egui::vec2(BOX, BOX),
        );
        checkbox(
            paint,
            tick,
            if card.recursive { Cell::On } else { Cell::Off },
            card.focus == Focus::Recursive,
        );
        painter.text(
            egui::pos2(tick.right() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            RECURSIVE_LABEL,
            font.clone(),
            mix(palette.subtext0, palette.text, hovers.hover(key)),
        );
        if let (true, Some(note)) = (card.recursive, geometry.note) {
            chrome::truncated_in(
                painter,
                egui::pos2(tick.right() + PAD_X, note.center().y),
                RECURSIVE_NOTE,
                palette.overlay1,
                (note.right() - tick.right() - PAD_X).max(0.0),
                small.clone(),
            );
        }
    }

    if let Some((status, warning)) = card.status() {
        chrome::truncated_in(
            painter,
            egui::pos2(geometry.status.left(), geometry.status.center().y),
            &status,
            if warning {
                palette.peach
            } else {
                palette.overlay1
            },
            geometry.status.width().max(0.0),
            font,
        );
    }

    for (index, (rect, label)) in geometry.actions.iter().zip(["Cancel", "Apply"]).enumerate() {
        dialog::button(
            paint,
            *rect,
            label,
            index == APPLY,
            false,
            Control::Action(index),
            hovers,
            ripples,
        );
        // Veiled rather than taken away while the field is half typed: the
        // button stays where the hand expects it, and the status says why.
        if index == APPLY && card.partial() {
            painter.rect_filled(*rect, ROW_RADIUS, chrome::fade(palette.crust, 0.55));
        }
    }
}

/// A target's plate: a row's lift under the pointer, and the ripple from
/// where it was pressed, clipped to it.
fn plate(
    paint: &Painting<'_>,
    rect: egui::Rect,
    key: Control,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let hover = hovers.hover(key);
    if hover > 0.0 {
        paint.painter.rect_filled(
            rect,
            chrome::CARD_ROW_RADIUS,
            mix(palette.crust, palette.surface0, hover),
        );
    }
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
        );
    }
}

/// One box: filled with a tick when on, an outline when off, and a dash on
/// a lighter fill when mixed. The state is a shape as well as a colour, as
/// the spot's chips are, and the keyboard's box wears the spot's blue ring.
fn checkbox(paint: &Painting<'_>, rect: egui::Rect, cell: Cell, focused: bool) {
    let palette = paint.palette;
    let painter = paint.painter;
    let radius = 4;
    match cell {
        Cell::On => {
            painter.rect_filled(rect, radius, mix(palette.crust, palette.green, 0.35));
            // A tick drawn as two strokes rather than set as a glyph: the
            // same weight in every face, and centred on the box rather than
            // on a glyph's side bearings (`delightful-ui` §17).
            let at = |x: f32, y: f32| {
                egui::pos2(
                    rect.left() + rect.width() * x,
                    rect.top() + rect.height() * y,
                )
            };
            painter.line(
                vec![at(0.25, 0.52), at(0.43, 0.7), at(0.76, 0.32)],
                egui::Stroke::new(1.6, palette.text),
            );
        }
        Cell::Off => {
            painter.rect_stroke(
                rect,
                radius,
                egui::Stroke::new(1.0, palette.surface2),
                egui::StrokeKind::Inside,
            );
        }
        Cell::Mixed => {
            painter.rect_filled(rect, radius, palette.surface0);
            painter.rect_stroke(
                rect,
                radius,
                egui::Stroke::new(1.0, palette.surface2),
                egui::StrokeKind::Inside,
            );
            let y = rect.center().y;
            painter.line_segment(
                [
                    egui::pos2(rect.left() + rect.width() * 0.28, y),
                    egui::pos2(rect.right() - rect.width() * 0.28, y),
                ],
                egui::Stroke::new(1.6, palette.subtext0),
            );
        }
    }
    if focused {
        painter.rect_stroke(
            rect.expand(1.5),
            radius + 2,
            egui::Stroke::new(1.5, palette.blue),
            egui::StrokeKind::Outside,
        );
    }
}

/// The octal field: its digits in the key face, a caret while typing, and
/// the text lit as a selection while the keyboard is in it and the next
/// digit would replace it.
fn field(paint: &Painting<'_>, card: &PermCard, rect: egui::Rect, hovers: &Hovers<Control>) {
    let palette = paint.palette;
    let painter = paint.painter;
    let focused = card.focus == Focus::Field;
    let hover = hovers.hover(Control::Action(FIELD));
    painter.rect_filled(
        rect,
        ROW_RADIUS,
        mix(palette.crust, palette.surface0, 0.5 + hover * 0.5),
    );
    painter.rect_stroke(
        rect,
        ROW_RADIUS,
        egui::Stroke::new(
            1.0,
            if focused {
                palette.blue
            } else {
                mix(palette.surface1, palette.blue, hover)
            },
        ),
        egui::StrokeKind::Inside,
    );
    let mono = chrome::key_font(FONT + 1.0);
    let galley = painter.layout_no_wrap(
        card.field.clone(),
        mono,
        if card.partial() {
            palette.peach
        } else {
            palette.text
        },
    );
    let text = egui::Rect::from_min_size(
        egui::pos2(rect.left() + PAD_X, rect.center().y - galley.size().y / 2.0),
        galley.size(),
    );
    if focused && card.mirrors() && !card.field.is_empty() {
        painter.rect_filled(
            text.expand2(egui::vec2(1.0, 0.0)),
            2,
            mix(palette.crust, palette.blue, 0.35),
        );
    }
    painter.galley(text.min, galley, palette.text);
    if focused && !card.mirrors() {
        let x = text.right() + 1.0;
        painter.line_segment(
            [
                egui::pos2(x, rect.center().y - 7.0),
                egui::pos2(x, rect.center().y + 7.0),
            ],
            egui::Stroke::new(chrome::CARET_WIDTH, palette.text),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(name: &str, mode: u32, is_dir: bool) -> Subject {
        Subject {
            path: PathBuf::from("/tmp").join(name),
            name: name.to_string(),
            mode,
            uid: 0,
            gid: 0,
            is_dir,
        }
    }

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))
    }

    /// Run `f` with a painter that can measure text.
    fn with_painter(mut f: impl FnMut(&egui::Painter)) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| f(ui.painter()));
    }

    /// A long name loses its middle on the card, not its extension; the
    /// subtitle is the whole name, and a selection is counted.
    #[test]
    fn the_subtitle_is_the_name_or_the_count() {
        let one = PermCard::new(vec![subject("notes.txt", 0o100644, false)], None);
        assert_eq!(one.subtitle(), "notes.txt");
        let many = PermCard::new(
            (0..1200)
                .map(|i| subject(&format!("f{i}"), 0o100644, false))
                .collect(),
            None,
        );
        assert_eq!(many.subtitle(), "1,200 items");
    }

    /// Several owners are counted, not listed, and a server's are numbers.
    #[test]
    fn the_owner_line_counts_what_it_cannot_name() {
        let mut a = subject("a", 0o100644, false);
        let mut b = subject("b", 0o100644, false);
        a.uid = 4_000_001;
        b.uid = 4_000_002;
        a.gid = 4_000_003;
        b.gid = 4_000_003;
        let card = PermCard::new(vec![a.clone(), b], None);
        assert_eq!(card.owner_line(), "2 owners · 4000003");
        let remote = PermCard::new(vec![a], Some("box".to_string()));
        assert_eq!(remote.owner_line(), "4000001 · 4000003");
    }

    /// `Tab` goes grid → field → checkbox and round; without a folder it
    /// skips the checkbox, and `↑` out of the field lands on the column the
    /// keyboard left.
    #[test]
    fn tab_and_the_arrows_walk_the_card() {
        let mut card = PermCard::new(vec![subject("d", 0o40755, true)], None);
        assert_eq!(card.key(Chord::plain(Key::ArrowRight)), Outcome::Consumed);
        assert_eq!(card.focus, Focus::Cell(1));
        card.key(Chord::plain(Key::Tab));
        assert_eq!(card.focus, Focus::Field);
        card.key(Chord::plain(Key::Tab));
        assert_eq!(card.focus, Focus::Recursive);
        card.key(Chord::plain(Key::Tab));
        assert_eq!(card.focus, Focus::Cell(1));
        card.key(Chord::shift(Key::Tab));
        assert_eq!(card.focus, Focus::Recursive);
        card.key(Chord::plain(Key::ArrowUp));
        card.key(Chord::plain(Key::ArrowUp));
        assert_eq!(card.focus, Focus::Cell(7), "back to the column it left");

        let mut file = PermCard::new(vec![subject("f", 0o100644, false)], None);
        file.key(Chord::plain(Key::Tab));
        file.key(Chord::plain(Key::Tab));
        assert_eq!(file.focus, Focus::Cell(0), "no checkbox to stop at");
        // `Enter` and `Esc` are the registry's, so their hints run them.
        assert_eq!(file.key(Chord::plain(Key::Enter)), Outcome::Ignored);
        assert_eq!(file.key(Chord::plain(Key::Escape)), Outcome::Ignored);
        // Not octal is swallowed, not typed.
        file.key(Chord::plain(Key::Char('9')));
        assert_eq!(file.field, "644");
    }

    /// A fourth digit is accepted and says what it will not do.
    #[test]
    fn a_fourth_digit_is_kept_as_it_was() {
        let mut card = PermCard::new(vec![subject("f", 0o100644, false)], None);
        for digit in ['4', '7', '5', '5'] {
            card.key(Chord::plain(Key::Char(digit)));
        }
        assert_eq!(card.grid, Grid::of_mode(0o755));
        let (status, warning) = card.status().expect("a word about the fourth digit");
        assert!(status.contains("first digit"), "{status}");
        assert!(!warning);
        // A fifth is not a mode at all, and is not taken.
        card.key(Chord::plain(Key::Char('1')));
        assert_eq!(card.field, "4755");
    }

    /// Every box, the field, the checkbox and both buttons are where the hit
    /// test finds them, every box's target clears the 24-point floor, and
    /// the last button's corner is concentric with the card's.
    #[test]
    fn the_hit_test_finds_what_is_drawn() {
        let card = PermCard::new(vec![subject("d", 0o40755, true)], None);
        with_painter(|painter| {
            let g = geometry(painter, area(), &card);
            for index in 0..9 {
                let rect = g.cells[index];
                assert!(rect.width() >= 24.0 && rect.height() >= 24.0, "{index}");
                assert_eq!(g.hit(rect.center()), Some(cell_control(index)));
                assert_eq!(g.rect_of(cell_control(index)), Some(rect));
            }
            assert_eq!(g.hit(g.field.center()), Some(Control::Action(FIELD)));
            let recursive = g.recursive.expect("a folder has the checkbox");
            assert!(recursive.height() >= 24.0);
            assert_eq!(g.hit(recursive.center()), Some(Control::Action(RECURSIVE)));
            for (index, rect) in g.actions.iter().enumerate() {
                assert_eq!(g.hit(rect.center()), Some(Control::Action(index)));
            }
            let apply = g.actions[APPLY];
            assert!((g.card.right() - apply.right() - CARD_PAD).abs() < 0.01);
            assert!((g.card.bottom() - chrome::HINT_ROW - apply.bottom() - CARD_PAD).abs() < 0.01);
            // Everything is on the plate.
            for rect in g.cells.iter().chain([&g.field, &recursive, &g.status]) {
                assert!(g.card.contains_rect(*rect), "{rect:?} is off the card");
            }
            // The five hints fit the strip whole.
            let strip = chrome::hint_rects(painter, chrome::hint_rect(g.card), &card.hints());
            assert_eq!(strip.len(), card.hints().len(), "a hint fell off the strip");
        });
    }

    /// The card paints in every state it has: one file, a mixed selection, a
    /// folder with the box ticked, a field half typed, and a server.
    #[test]
    fn the_card_paints_in_every_state() {
        let mut mixed = PermCard::new(
            vec![subject("a", 0o100644, false), subject("b", 0o100600, false)],
            None,
        );
        mixed.key(Chord::plain(Key::ArrowDown));
        let mut folder = PermCard::new(vec![subject("d", 0o40755, true)], None);
        folder.toggle_recursive();
        let mut half = PermCard::new(vec![subject("f", 0o100644, false)], None);
        half.key(Chord::plain(Key::Char('7')));
        let remote = PermCard::new(vec![subject("f", 0o100644, false)], Some("box".to_string()));
        let long = PermCard::new(
            vec![subject(
                &format!("{}.txt", "long ".repeat(80)),
                0o100644,
                false,
            )],
            None,
        );
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
            let hovers = Hovers::new();
            let ripples = Ripples::new();
            for card in [&mixed, &folder, &half, &remote, &long] {
                let g = geometry(ui.painter(), area(), card);
                super::paint(&paint, card, &g, &hovers, &ripples);
                // A window too small for the card still lays it out.
                let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 90.0));
                let g = geometry(ui.painter(), tiny, card);
                super::paint(&paint, card, &g, &hovers, &ripples);
            }
        });
    }
}
