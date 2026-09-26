//! The undo history card: the journal as one timeline, and a way to walk it
//! more than a step at a time (PLAN §5).
//!
//! `u` and `U` take one step each. The card shows every step there is to
//! take: what `U` would do again above a hairline, dimmed, the next of them
//! nearest the line, and what `u` would take back below it, newest first. So
//! the list reads top to bottom in the order things happened, and the line is
//! where the files are now. `Enter` on a row walks the stacks one step at a
//! time until that row is on the other side of the line, and stops at the
//! first step that is refused — its reason is the toast, and the card is
//! drawn again from the journal as the walk left it.
//!
//! A step never moves a row: undoing the newest operation turns the row just
//! under the line into the row just over it, and redoing turns it back. The
//! cursor is an index, and stays on the operation it was on.
//!
//! Except when the list is rebuilt rather than stepped along. An undo that
//! stops part way clears every row above the line (the journal's header says
//! why), and so does anything new journalled while the card is up — a paste
//! landing. The rows below move up under the cursor then, and the cursor is
//! only kept inside the list ([`HistoryCard::tick`]), not on the operation it
//! was on. And a copy being redone is on neither stack while its job runs, so
//! its row is missing until it lands, and a walk stops at it.
//!
//! The rows are a pure function of the journal ([`rows`]), what `Enter` does
//! on one is a pure function of the rows ([`walk`]), and "how long ago" is a
//! pure function of two instants ([`ago`]), so the card's wording is a unit
//! test rather than something to catch on screen. Nothing on it moves: the
//! only frames it asks for are the wake-ups at the instants an "ago" would
//! read differently ([`next_change`]).

use std::time::{Duration, Instant};

use df_core::ops::journal::Journal;

use crate::chrome::{self, CARD_PAD, FONT, PAD_X};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting};

/// One row's height: a line of text, at `delightful-ui` §1's hit-target floor,
/// the menus' height — a row here is aimed at and clicked, as a menu's is.
const ROW: f32 = 24.0;

/// How many rows the card shows before it scrolls. The journal holds up to
/// 64 steps each way, which is a history rather than a screenful; ten is the
/// last few minutes of work, which is what a person opens this to find. At
/// most: a window too short for ten gets as many as fit ([`geometry`]).
const VISIBLE: usize = 10;

/// The card's width, in points: the task panel's, so a sentence with two
/// file names in it has room before it is reduced to an ellipsis.
const WIDTH: f32 = 460.0;

/// The gap between a row's sentence and its "ago".
const AGO_GAP: f32 = 12.0;

/// One row of the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// What the step is, as a sentence: what `U` would do again above the
    /// line, what was done below it.
    pub label: String,
    /// When the operation was first done.
    pub at: Instant,
    /// Above the line: undone, and `U` would do it again.
    pub redo: bool,
}

/// The journal's two stacks as one list: the redo stack farthest first, so
/// the next redo is the last row above the line, then the undo stack newest
/// first.
pub fn rows(journal: &Journal) -> Vec<Row> {
    let mut rows: Vec<Row> = journal
        .redoable()
        .map(|(record, at)| Row {
            label: sentence(&record.describe_redo()),
            at,
            redo: true,
        })
        .collect();
    rows.reverse();
    rows.extend(journal.undoable().map(|(record, at)| Row {
        label: sentence(&record.describe()),
        at,
        redo: false,
    }));
    rows
}

/// A record's description with its first letter raised: the journal words
/// them to be read mid-sentence, and a row is a sentence of its own.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// How many rows are above the line.
pub fn redos(rows: &[Row]) -> usize {
    rows.iter().take_while(|row| row.redo).count()
}

/// Where the cursor starts: on the newest thing `u` would take back, or,
/// with nothing to take back, on the next thing `U` would do again.
pub fn start(rows: &[Row]) -> usize {
    let redos = redos(rows);
    if redos < rows.len() {
        redos
    } else {
        redos.saturating_sub(1)
    }
}

/// What `Enter` on a row does: that many steps of `u`, or of `U`, until the
/// row is on the other side of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    Undo(usize),
    Redo(usize),
}

/// The walk that takes row `index` across the line, or `None` past the end.
pub fn walk(rows: &[Row], index: usize) -> Option<Walk> {
    if index >= rows.len() {
        return None;
    }
    let redos = redos(rows);
    Some(if index < redos {
        Walk::Redo(redos - index)
    } else {
        Walk::Undo(index - redos + 1)
    })
}

/// How long ago `at` was, as a row says it: `just now` for the first minute,
/// then whole minutes, whole hours and whole days, each rounded down — a
/// history is read to find the thing done a few minutes ago, and a
/// second-by-second count would be a clock nobody asked for.
pub fn ago(at: Instant, now: Instant) -> String {
    let secs = now.saturating_duration_since(at).as_secs();
    match secs {
        ..60 => "just now".to_string(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// How long until [`ago`] reads differently for `at`: a single instant known
/// in advance, so the card is owed a wake-up rather than a frame a second.
pub fn next_change(at: Instant, now: Instant) -> Duration {
    let elapsed = now.saturating_duration_since(at);
    let secs = elapsed.as_secs();
    let unit = match secs {
        ..3_600 => 60,
        3_600..86_400 => 3_600,
        _ => 86_400,
    };
    Duration::from_secs((secs / unit + 1) * unit).saturating_sub(elapsed)
}

/// The card's own state. The rows are not its to keep: they are the
/// journal's, read afresh every frame, as the task panel's are the engine's,
/// and the card holds on to the last frame's only to tell a rebuilt list from
/// a scroll.
#[derive(Debug, Default)]
pub struct HistoryCard {
    pub cursor: usize,
    /// First visible row.
    pub first: usize,
    /// How many rows the card showed when it was last drawn: what the page
    /// keys stride by and the cursor scrolls in.
    shown: Option<usize>,
    /// When the rows last scrolled, for their bar.
    bar: crate::scrollbar::Linger,
    /// The wheel's roll that has not come to a whole row yet
    /// ([`crate::mouse::roll`]).
    carry: f32,
    /// The wheel has scrolled the rows off the cursor, and they stay where it
    /// left them until a key or a click moves the cursor: the panes' rule
    /// ([`crate::tab::Listing::attach`]).
    detached: bool,
    /// The rows as last drawn: a list different from them is the list
    /// rebuilt under the card, not a scroll.
    seen: Vec<Row>,
}

impl HistoryCard {
    /// The card, opened on `rows`, with the cursor where [`start`] puts it.
    pub fn new(rows: &[Row]) -> HistoryCard {
        HistoryCard {
            cursor: start(rows),
            seen: rows.to_vec(),
            ..HistoryCard::default()
        }
    }

    pub fn move_cursor(&mut self, delta: isize, rows: usize) {
        let Some(last) = rows.checked_sub(1) else {
            self.cursor = 0;
            return;
        };
        self.cursor = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
        self.detached = false;
    }

    /// Put the cursor on row `index` — a click's, or a page key's — and the
    /// view back on it.
    pub fn select(&mut self, index: usize, rows: usize) {
        self.cursor = index.min(rows.saturating_sub(1));
        self.detached = false;
    }

    /// A page of rows, for the page keys: as many as the card showed.
    pub fn page(&self, rows: usize) -> usize {
        self.shown.unwrap_or(rows)
    }

    /// The journal's rows this frame. A list that is not the one last drawn
    /// was rebuilt under the card — a step walked, or a job that landed and
    /// was journalled while the card was up — so the bar starts afresh
    /// rather than reporting a scroll nobody made, and the cursor is kept
    /// inside the list.
    pub fn tick(&mut self, rows: &[Row]) {
        if self.seen != rows {
            self.seen = rows.to_vec();
            self.bar = crate::scrollbar::Linger::default();
        }
        self.cursor = self.cursor.min(rows.len().saturating_sub(1));
    }

    /// The card was laid out showing `shown` of its `rows`: the view follows
    /// the cursor in those, unless the wheel has taken it off the cursor, when
    /// it is only kept inside the list.
    pub fn fit(&mut self, shown: usize, rows: usize, now: Instant) {
        self.shown = Some(shown);
        self.first = if self.detached {
            self.first.min(rows.saturating_sub(shown))
        } else {
            crate::viewport::first_visible(self.first, self.cursor, rows, shown, 0)
        };
        self.bar.saw(self.first as f32, now);
    }

    /// The wheel over the card, in points, with `rows` listed: whole rows at
    /// a time ([`crate::mouse::roll`]), the view leaving the cursor where it
    /// was. Returns whether the rows moved.
    pub fn wheel(&mut self, points: f32, rows: usize, now: Instant) -> bool {
        let step = crate::mouse::wheel_rows(points, ROW);
        let last = rows.saturating_sub(self.page(rows));
        let first = crate::mouse::roll(self.first, last, &mut self.carry, step);
        self.scroll_to(first, rows, now)
    }

    /// Start the rows at `first`, kept inside the `rows` listed, off the
    /// cursor until a key or a click moves it. Returns whether they moved.
    pub fn scroll_to(&mut self, first: usize, rows: usize, now: Instant) -> bool {
        let first = first.min(rows.saturating_sub(self.page(rows)));
        if first == self.first {
            return false;
        }
        self.first = first;
        self.detached = true;
        self.bar.saw(first as f32, now);
        true
    }

    /// When the rows last scrolled, for their bar's linger.
    pub fn scrolled_at(&self) -> Option<Instant> {
        self.bar.scrolled_at()
    }

    /// A hand let go of the bar: it lingers from now
    /// ([`crate::scrollbar::Linger::let_go`]).
    pub fn let_go(&mut self, now: Instant) {
        self.bar.let_go(now);
    }
}

/// Where the card's pieces are this frame, and the rows they were measured
/// for. Shared by the hit test, the hints and the paint.
#[derive(Debug, Clone)]
pub struct Geometry {
    pub card: egui::Rect,
    /// One rect per row the card has room for, from its `first` row down.
    pub rects: Vec<egui::Rect>,
    pub rows: Vec<Row>,
    /// Whether the cursor is above the line, so `Enter` redoes rather than
    /// undoes — what the hint strip says it will do.
    pub on_redo: bool,
    /// The band its bar is pointed at by, while there are more rows than it
    /// shows ([`crate::scrollbar::band`]).
    pub band: Option<egui::Rect>,
}

impl Geometry {
    /// The row a point is over, among the rows drawn.
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rects.iter().position(|rect| rect.contains(pos))
    }

    /// The card's `×`.
    pub fn close(&self) -> egui::Rect {
        chrome::close_button_rect(self.card)
    }
}

/// Lay the card out: hung above `bar_top` where the task panel hangs, up to
/// [`VISIBLE`] rows and as many as the window has room for
/// ([`crate::dialog::fit_rows`]), a heading above them and the hint strip
/// under them.
pub fn geometry(area: egui::Rect, bar_top: f32, card: &HistoryCard, rows: Vec<Row>) -> Geometry {
    let fixed = CARD_PAD * 2.0 + chrome::CARD_ROW + chrome::HINT_ROW;
    let above = egui::Rect::from_min_max(area.min, egui::pos2(area.right(), bar_top));
    let (listed, fitted) =
        crate::dialog::fit_rows(above, WIDTH, fixed, ROW, rows.len().clamp(1, VISIBLE));
    let plate = egui::Rect::from_min_size(
        egui::pos2(
            fitted.left(),
            (bar_top - chrome::CARD_MARGIN - fitted.height()).max(area.top() + chrome::CARD_MARGIN),
        ),
        fitted.size(),
    );
    let top = plate.top() + CARD_PAD + chrome::CARD_ROW;
    let rects: Vec<egui::Rect> = (0..rows.len().min(listed))
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(plate.left() + CARD_PAD, top + i as f32 * ROW),
                egui::vec2(plate.width() - CARD_PAD * 2.0, ROW),
            )
        })
        .collect();
    let band = body(&rects).and_then(|body| {
        crate::scrollbar::band(plate, body, rects.len() as f32, rows.len() as f32)
    });
    let on_redo = rows.get(card.cursor).is_some_and(|row| row.redo);
    Geometry {
        card: plate,
        rects,
        rows,
        on_redo,
        band,
    }
}

/// The card's bar, beside its rows, while it shows fewer than there are.
pub fn bar(geometry: &Geometry, card: &HistoryCard) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        geometry.card,
        body(&geometry.rects)?,
        card.first as f32,
        geometry.rects.len() as f32,
        geometry.rows.len() as f32,
    )
}

/// The rows, as one rect.
fn body(rects: &[egui::Rect]) -> Option<egui::Rect> {
    let (first, last) = (rects.first()?, rects.last()?);
    Some(egui::Rect::from_min_max(first.min, last.max))
}

/// Draw the card.
pub fn paint(
    paint: &Painting<'_>,
    geometry: &Geometry,
    card: &HistoryCard,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let painter = paint.painter;
    let palette = paint.palette;
    let plate = geometry.card;
    chrome::card(paint, plate, 1.0);

    let heading_y = plate.top() + CARD_PAD + chrome::CARD_ROW / 2.0;
    painter.text(
        egui::pos2(plate.left() + CARD_PAD, heading_y),
        egui::Align2::LEFT_CENTER,
        "History",
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
    );
    // The `×` in the heading's far corner, and the two counts left of it.
    let close = geometry.close();
    chrome::close_button(paint, close, hovers, ripples);
    let rows = &geometry.rows;
    let redos = redos(rows);
    let counts = match (rows.len() - redos, redos) {
        (0, 0) => String::new(),
        (undos, 0) => format!("{undos} to undo"),
        (0, redos) => format!("{redos} to redo"),
        (undos, redos) => format!("{undos} to undo · {redos} to redo"),
    };
    painter.text(
        egui::pos2(close.left() - crate::ui::GAP, heading_y),
        egui::Align2::RIGHT_CENTER,
        counts,
        egui::FontId::proportional(FONT),
        palette.overlay0,
    );

    if rows.is_empty() {
        // An empty journal says so in the words `u` would use.
        painter.text(
            egui::pos2(plate.center().x, plate.center().y + 6.0),
            egui::Align2::CENTER_CENTER,
            "Nothing to undo",
            egui::FontId::proportional(FONT),
            palette.overlay0,
        );
        return;
    }

    for (i, rect) in geometry.rects.iter().enumerate() {
        let index = card.first + i;
        let Some(row) = rows.get(index) else {
            break;
        };
        let on_cursor = index == card.cursor;
        // The row's place among the drawn ones, which is what the hit test
        // reports.
        let key = Control::PanelRow(i);
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        if on_cursor || hover > 0.0 {
            painter.rect_filled(
                rect,
                chrome::CARD_ROW_RADIUS,
                mix(
                    if on_cursor {
                        palette.surface1
                    } else {
                        palette.crust
                    },
                    palette.surface0,
                    hover,
                ),
            );
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        // When, quietly on the right, where it can be found but is not what
        // the row is read for.
        let when = inside.layout_no_wrap(
            ago(row.at, paint.now),
            egui::FontId::proportional(FONT - 1.0),
            palette.overlay0,
        );
        let when_width = when.size().x;
        inside.galley(
            egui::pos2(
                rect.right() - PAD_X - when_width,
                rect.center().y - when.size().y / 2.0,
            ),
            when,
            palette.overlay0,
        );
        // Above the line is what is not done any more: dimmed, the way a
        // step that has been taken back reads everywhere.
        chrome::truncated(
            &inside,
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            &row.label,
            if row.redo {
                palette.subtext0
            } else {
                palette.text
            },
            (rect.width() - PAD_X * 2.0 - when_width - AGO_GAP).max(0.0),
        );
    }

    // The line between what could be done again and what could be taken
    // back: where the files are now. Drawn only where it falls between two
    // rows on screen — at the edge of the window it would read as the card's
    // own rule, and the rows' colours already say which side is which.
    let drawn = geometry.rects.len();
    if redos > card.first && redos < card.first + drawn && redos < rows.len() {
        if let Some(under) = geometry.rects.get(redos - card.first) {
            painter.hline(
                (under.left() + PAD_X)..=(under.right() - PAD_X),
                under.top(),
                egui::Stroke::new(1.0, palette.surface1),
            );
        }
    }

    if let Some(bar) = bar(geometry, card) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::History,
            hovers,
            card.scrolled_at(),
            1.0,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use df_core::ops::journal::{FileKind, Fingerprint, OpRecord};
    use std::path::PathBuf;

    fn create(name: &str) -> OpRecord {
        OpRecord::Create {
            path: PathBuf::from(format!("/w/{name}")),
            is_dir: false,
            fingerprint: Fingerprint {
                kind: FileKind::File,
                len: 0,
                mtime: None,
                entries: None,
            },
            created_parents: Vec::new(),
        }
    }

    fn row(label: &str, redo: bool, at: Instant) -> Row {
        Row {
            label: label.to_string(),
            at,
            redo,
        }
    }

    /// Newest first below the line, the next undo first — and nothing above
    /// it on a journal that has not been walked back.
    #[test]
    fn the_rows_are_the_undo_stack_newest_first() {
        let t0 = Instant::now();
        let mut journal = Journal::default();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            journal.record_at(create(name), t0 + Duration::from_secs(i as u64));
        }
        let rows = rows(&journal);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Created file c", "Created file b", "Created file a"]
        );
        assert!(rows.iter().all(|row| !row.redo));
        assert_eq!(rows[0].at, t0 + Duration::from_secs(2));
        assert_eq!(start(&rows), 0, "the newest thing to take back");
        assert_eq!(rows.len(), journal.len());
    }

    /// Two steps walked back: the rows are where they were, and the two
    /// newest have crossed the line — dimmed, named by what `U` would do,
    /// the next redo nearest the line.
    #[test]
    fn a_walked_back_journal_reads_as_one_timeline() {
        let rows = vec![
            row("Created file c again", true, Instant::now()),
            row("Created file b again", true, Instant::now()),
            row("Created file a", false, Instant::now()),
        ];
        assert_eq!(redos(&rows), 2);
        assert_eq!(start(&rows), 2, "on the first row below the line");
        assert_eq!(walk(&rows, 2), Some(Walk::Undo(1)));
        assert_eq!(walk(&rows, 1), Some(Walk::Redo(1)), "the next redo");
        assert_eq!(walk(&rows, 0), Some(Walk::Redo(2)), "and the one after");
        assert_eq!(walk(&rows, 3), None);

        // With nothing left below the line, the cursor starts on the next
        // redo; with nothing at all, at the top of nothing.
        assert_eq!(start(&rows[..2]), 1);
        assert_eq!(start(&[]), 0);
    }

    /// `Enter` on the second row below the line undoes two.
    #[test]
    fn enter_below_the_line_undoes_down_to_the_row() {
        let now = Instant::now();
        let rows = vec![
            row("Created file c", false, now),
            row("Created file b", false, now),
            row("Created file a", false, now),
        ];
        assert_eq!(walk(&rows, 0), Some(Walk::Undo(1)));
        assert_eq!(walk(&rows, 1), Some(Walk::Undo(2)));
        assert_eq!(walk(&rows, 2), Some(Walk::Undo(3)));
    }

    #[test]
    fn ago_says_it_in_whole_minutes_hours_and_days() {
        let at = Instant::now();
        let after = |secs: u64| ago(at, at + Duration::from_secs(secs));
        assert_eq!(after(0), "just now");
        assert_eq!(after(59), "just now");
        assert_eq!(after(60), "1 min ago");
        assert_eq!(after(150), "2 min ago");
        assert_eq!(after(3_599), "59 min ago");
        assert_eq!(after(3_600), "1 h ago");
        assert_eq!(after(3 * 3_600 + 1_799), "3 h ago");
        assert_eq!(after(86_400), "1 d ago");
        assert_eq!(after(10 * 86_400), "10 d ago");
        // An instant from a clock that has not caught up is not "in the
        // future", it is now.
        assert_eq!(ago(at + Duration::from_secs(5), at), "just now");
    }

    /// The wake-up is exactly the instant the words change, never sooner —
    /// a card left open on the desk asks for one frame a minute at most, and
    /// one an hour once it is talking in hours.
    #[test]
    fn the_next_change_is_the_next_boundary() {
        let at = Instant::now();
        let from = |elapsed: Duration| next_change(at, at + elapsed);
        assert_eq!(from(Duration::ZERO), Duration::from_secs(60));
        assert_eq!(
            from(Duration::from_millis(59_500)),
            Duration::from_millis(500)
        );
        assert_eq!(from(Duration::from_secs(60)), Duration::from_secs(60));
        assert_eq!(from(Duration::from_secs(150)), Duration::from_secs(30));
        assert_eq!(from(Duration::from_secs(3_600)), Duration::from_secs(3_600));
        assert_eq!(from(Duration::from_secs(5_000)), Duration::from_secs(2_200));
        assert_eq!(
            from(Duration::from_secs(86_400)),
            Duration::from_secs(86_400)
        );
        for secs in [0, 59, 60, 61, 3_599, 3_600, 90_000] {
            let elapsed = Duration::from_secs(secs);
            let now = at + elapsed;
            let wake = now + next_change(at, now);
            assert_ne!(ago(at, wake), ago(at, now), "{secs} s: woke for nothing");
            assert_eq!(
                ago(at, wake - Duration::from_millis(1)),
                ago(at, now),
                "{secs} s: slept through a change"
            );
        }
    }

    /// The cursor stays inside the list, and the view follows it in the rows
    /// a short window gives the card.
    #[test]
    fn the_cursor_stays_inside_and_the_view_follows_it() {
        let now = Instant::now();
        let rows: Vec<Row> = (0..30).map(|i| row(&format!("{i}"), false, now)).collect();
        let mut card = HistoryCard::new(&rows);
        card.fit(5, rows.len(), now);
        card.move_cursor(-1, rows.len());
        assert_eq!(card.cursor, 0);
        card.move_cursor(12, rows.len());
        card.fit(5, rows.len(), now);
        assert_eq!(card.cursor, 12);
        assert!(card.first <= 12 && 12 < card.first + 5, "{}", card.first);
        card.move_cursor(100, rows.len());
        card.fit(5, rows.len(), now);
        assert_eq!((card.cursor, card.first), (29, 25));

        // A shorter list — the journal cleared under the card — keeps the
        // cursor on a row that is there.
        card.tick(&rows[..3]);
        assert_eq!(card.cursor, 2);
    }

    /// A step walked changes the rows under the card, and that is the list
    /// rebuilt, not a scroll: the bar has nothing to linger for.
    #[test]
    fn a_list_rebuilt_under_the_card_is_not_a_scroll() {
        let now = Instant::now();
        let rows: Vec<Row> = (0..30).map(|i| row(&format!("{i}"), false, now)).collect();
        let mut card = HistoryCard::new(&rows);
        card.tick(&rows);
        card.fit(5, rows.len(), now);
        assert_eq!(card.scrolled_at(), None, "opening is not a scroll");
        // A roll towards the end of the list: egui's `y` is negative for it.
        assert!(card.wheel(-ROW * 2.0, rows.len(), now));
        card.fit(5, rows.len(), now);
        assert_eq!(card.scrolled_at(), Some(now), "the wheel scrolled it");

        let mut walked = rows.clone();
        walked[0].redo = true;
        card.tick(&walked);
        assert_eq!(card.scrolled_at(), None, "and nobody scrolled that");
    }

    /// A short window gets the rows it has room for, and the card stays in
    /// it; the bar is there only while rows are left out. An empty journal
    /// keeps the card's one line of guidance.
    #[test]
    fn a_short_window_fits_the_cards_rows() {
        let now = Instant::now();
        let window = |height: f32| {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, height))
        };
        let rows: Vec<Row> = (0..30).map(|i| row(&format!("{i}"), i < 4, now)).collect();
        let card = HistoryCard::new(&rows);

        let tall = window(900.0);
        let geometry = geometry(tall, tall.bottom() - 8.0, &card, rows.clone());
        assert_eq!(geometry.rects.len(), VISIBLE);
        assert!(geometry.band.is_some());
        assert!(bar(&geometry, &card).is_some());
        assert!(!geometry.on_redo, "the cursor starts below the line");

        let short = window(200.0);
        let geometry = super::geometry(short, short.bottom() - 8.0, &card, rows.clone());
        assert!(short.contains_rect(geometry.card), "{:?}", geometry.card);
        assert!(!geometry.rects.is_empty() && geometry.rects.len() < VISIBLE);
        assert!(geometry
            .rects
            .iter()
            .all(|rect| geometry.card.contains_rect(*rect)));

        let few = super::geometry(tall, tall.bottom() - 8.0, &card, rows[..3].to_vec());
        assert_eq!(few.band, None, "three rows fit");

        let empty = super::geometry(short, short.bottom() - 8.0, &card, Vec::new());
        assert!(empty.rects.is_empty() && short.contains_rect(empty.card));
    }

    #[test]
    fn the_card_paints_without_panicking() {
        let now = Instant::now();
        let rows = vec![
            row(
                "Renamed a.txt → a much longer name than the card is wide, to be cut short again",
                true,
                now,
            ),
            row("Created file c", false, now - Duration::from_secs(200)),
            row("Trashed 3 items", false, now - Duration::from_secs(90_000)),
        ];
        let card = HistoryCard::new(&rows);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let painting = Painting {
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now,
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            for rows in [rows.clone(), Vec::new()] {
                let geometry = geometry(area, 860.0, &card, rows);
                paint(&painting, &geometry, &card, &Hovers::new(), &Ripples::new());
            }
        });
    }
}
