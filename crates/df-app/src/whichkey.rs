//! The which-key card's timing (PLAN §4, §8) — ported from delightviewer's
//! `ui/chords.rs`, whose delay this shares — and its layout, which the paint
//! and the pointer both read: a click on a row presses that row's key.
//!
//! The card lists what could finish a half-typed chord. df-core already decides
//! *when* it is due — [`KeymapState::which_key_due`] is
//! `pending_started + WHICH_KEY_DELAY` — so all that is left here is the small
//! state machine around that instant, and it exists for one reason: **the card
//! must not vanish the moment the chord resolves.** `g` `g` typed at speed never
//! shows a card at all, which is the point of the delay; but a card that *did*
//! appear and then disappeared between two frames would read as a flicker
//! rather than as an answer, so it fades.
//!
//! Two rules the tests pin:
//!
//! - **Instant in, animated out** (`delightful-ui` §3), which is the same rule
//!   the hover system runs on. Waiting 175 ms and then *also* fading in would be
//!   two delays stacked, and the card would arrive after the hesitation it was
//!   supposed to answer.
//! - **A settled card asks for no frames.** [`WhichKey::deadline`] hands the
//!   event loop exactly one instant — the moment the card is due, or the moment
//!   its fade ends — and nothing polls in between (PLAN §1).
//!
//! [`KeymapState::which_key_due`]: df_core::keymap::KeymapState::which_key_due

use std::time::{Duration, Instant};

use df_core::keymap::{Chord, Continuation};

use crate::chrome::{key_font, text_width, CARD_MARGIN, CARD_PAD, CARD_ROW, FONT, PAD_X};

/// How long the card takes to leave once the chord has resolved or been
/// abandoned.
///
/// 120 ms is PLAN §8's state-fade duration, and an outro should be quicker than
/// its intro — the card's "intro" is a 175 ms wait and then nothing, so this is
/// already the slower half of the pair. Long enough not to be a cut, short
/// enough that the card is gone before the command it explained has finished
/// happening.
pub const FADE_OUT: Duration = Duration::from_millis(120);

/// Whether the card is up, and how far through leaving it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhichKey {
    /// The card is fully on screen.
    shown: bool,
    /// When the fade-out started, while one is running.
    fading_since: Option<Instant>,
    /// The first row of the card's columns drawn, in a window too small for
    /// all of them ([`geometry`]).
    first: usize,
    /// The part of a row the wheel has rolled over the card and not yet
    /// spent.
    carry: f32,
    /// When the columns last scrolled, for their bar.
    bar: crate::scrollbar::Linger,
}

impl WhichKey {
    pub fn new() -> WhichKey {
        WhichKey::default()
    }

    /// Advance the state one frame.
    ///
    /// `due` is [`KeymapState::which_key_due`]: `Some` while a chord is pending,
    /// `None` the instant it resolves, matches nothing, or is cancelled.
    ///
    /// [`KeymapState::which_key_due`]: df_core::keymap::KeymapState::which_key_due
    pub fn update(&mut self, due: Option<Instant>, now: Instant) {
        match due {
            Some(at) => {
                // A new chord while the old card is still fading takes the
                // screen back immediately rather than finishing the fade — the
                // card is about the chord in the hand, not the one that ended.
                self.fading_since = None;
                self.shown = now >= at;
            }
            None if self.shown => {
                self.shown = false;
                self.fading_since = Some(now);
            }
            None => {
                if self
                    .fading_since
                    .is_some_and(|from| now.saturating_duration_since(from) >= FADE_OUT)
                {
                    self.fading_since = None;
                }
            }
        }
    }

    /// 0…1. Full while shown; a quadratic ease-*in* falloff on the way out — it
    /// holds and then goes, the same shape [`crate::ripple`]'s alpha uses, so
    /// the two pieces of transient chrome leave the same way.
    pub fn alpha(&self, now: Instant) -> f32 {
        if self.shown {
            return 1.0;
        }
        let Some(from) = self.fading_since else {
            return 0.0;
        };
        let t = (now.saturating_duration_since(from).as_secs_f32()
            / FADE_OUT.as_secs_f32().max(f32::EPSILON))
        .clamp(0.0, 1.0);
        1.0 - t * t
    }

    pub fn visible(&self, now: Instant) -> bool {
        self.alpha(now) > 0.0
    }

    /// Whether the card is fully up, over a chord still pending — the only
    /// time it takes the pointer. A card on its way out is pixels about a
    /// chord that has already resolved, as a fading menu is.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Whether the card is mid-fade and therefore needs frames back to back. A
    /// card that is *up* is not animating — it is a static rectangle, and asking
    /// for 60 frames a second to redraw it is how a hint costs a battery.
    pub fn fading(&self) -> bool {
        self.fading_since.is_some()
    }

    /// The first row drawn, for [`geometry`].
    pub fn first(&self) -> usize {
        self.first
    }

    /// The card has new rows — a chord begun, or one gone a key further — so
    /// it starts from their top, with nothing lingering from the last ones.
    pub fn rows_changed(&mut self) {
        self.first = 0;
        self.carry = 0.0;
        self.bar = crate::scrollbar::Linger::default();
    }

    /// A wheel roll over the card, in points: whole rows at a time, the
    /// fraction kept for the next roll and dropped at either end, the tray's
    /// rule ([`crate::tray::scroll`]), in the card laid out as `geometry`.
    pub fn wheel(&mut self, points: f32, geometry: &Geometry) {
        let rows = crate::mouse::wheel_rows(points, CARD_ROW);
        let last = geometry.total.saturating_sub(geometry.per);
        self.first = crate::mouse::roll(self.first, last, &mut self.carry, rows);
    }

    /// The card was laid out as `geometry`: its scroll is kept inside the
    /// columns, and the bar's linger is stamped from the frame they moved.
    pub fn fit(&mut self, geometry: &Geometry, now: Instant) {
        self.first = geometry.first;
        self.bar.saw(self.first as f32, now);
    }

    /// When the columns last scrolled, for their bar's linger.
    pub fn scrolled_at(&self) -> Option<Instant> {
        self.bar.scrolled_at()
    }

    /// A hand let go of the bar: it lingers from now
    /// ([`crate::scrollbar::Linger::let_go`]).
    pub fn let_go(&mut self, now: Instant) {
        self.bar.let_go(now);
    }

    /// Start the columns at row `first`, for a hand on the bar, in the card
    /// laid out as `geometry`, kept inside them.
    pub fn scroll_to(&mut self, first: usize, geometry: &Geometry) {
        self.first = first.min(geometry.total.saturating_sub(geometry.per));
    }

    /// When the next frame is owed, or `None` when the card is settled — either
    /// fully up (a static card costs nothing) or fully gone.
    pub fn deadline(&self, due: Option<Instant>) -> Option<Instant> {
        if let Some(from) = self.fading_since {
            return Some(from + FADE_OUT);
        }
        // Waiting for the card to become due: exactly one wake-up, at the
        // instant it does.
        match due {
            Some(at) if !self.shown => Some(at),
            _ => None,
        }
    }
}

/// One row of the card: the keys that finish the chord from here, what they
/// do, and the keystroke a click on the row presses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub keys: String,
    pub label: String,
    /// The next key alone. A click is one keystroke, as a key press is, so a
    /// row whose binding goes on (`x y`) presses `x` and the card follows the
    /// chord to its next rows.
    pub next: Chord,
}

impl Row {
    pub fn of(continuation: &Continuation) -> Row {
        Row {
            keys: continuation.label(),
            label: continuation.description.clone(),
            next: continuation.next,
        }
    }
}

// ── Where the card goes (PLAN §4, §8) ───────────────────────────────────────

/// The most continuations one column shows before the card grows a second one.
///
/// The `g` chord has a dozen bookmarks and the `,` chord thirteen sorts; a
/// single column of those is a tower up the middle of the window that the eye
/// has to scan end to end. Nine is about the length a list is still taken in at
/// a glance rather than read.
const COLUMN: usize = 9;

/// Between a key and what it does.
const KEY_GAP: f32 = 14.0;

/// Between the words of two columns of the card. Wider than the key/label gap
/// by enough that the columns are unambiguously separate groups.
const COLUMN_SEP: f32 = 26.0;

/// Between two columns' rows: the words keep [`COLUMN_SEP`], less the padding
/// each row carries inside it.
const ROW_SEP: f32 = COLUMN_SEP - PAD_X * 2.0;

/// The card and its rows, as the paint draws them and the hit test reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    pub card: egui::Rect,
    /// One per row, in order: a card row tall and as wide as its column, the
    /// words inset by a chip's padding, so a hovered row lifts as a menu row
    /// does and its plate keeps the card's padding off the card's edge
    /// (`delightful-ui` §15). Every row, where it is: one scrolled out of
    /// [`Geometry::clip`] is neither drawn nor pressed.
    pub rows: Vec<egui::Rect>,
    /// Where each row's label starts: its column's keys all take the width of
    /// the widest.
    pub labels: Vec<f32>,
    /// The inside of the card, where rows are drawn and pressed.
    pub clip: egui::Rect,
    /// How many rows a column shows at once.
    pub per: usize,
    /// How many rows the tallest column has: more than [`Geometry::per`] only
    /// when the window was too narrow for the columns and the rows were
    /// shared out among fewer, which is when the card scrolls.
    pub total: usize,
    /// The first row drawn, kept inside the columns.
    pub first: usize,
    /// The band the card's bar is pointed at by, while it scrolls
    /// ([`crate::scrollbar::band`]).
    pub band: Option<egui::Rect>,
}

impl Geometry {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        if !self.clip.contains(pos) {
            return None;
        }
        self.rows.iter().position(|row| row.contains(pos))
    }
}

/// The card's bar, beside its columns, while they are taller than it.
pub fn bar(geometry: &Geometry) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        geometry.card,
        geometry.clip,
        geometry.first as f32,
        geometry.per as f32,
        geometry.total as f32,
    )
}

/// Lay the card out for `rows`, sitting above `bottom`, drawn from row
/// `first` of its columns.
///
/// Bottom-anchored and horizontally centred: the card is an answer to
/// something the hand is doing right now, so it belongs where the eyes are —
/// and near the bottom edge, which is where every other transient thing
/// appears. Needs a painter because the columns are measured from their text.
///
/// Always inside the window. A column holds [`COLUMN`] rows, or as many as
/// the window's height has room for, which makes more columns; the card has
/// as many columns as its width has room for, and past that the rows are
/// shared out evenly among those columns, which run on below the card's foot,
/// and the card scrolls them together as one block — never sideways. Its keys
/// work the same whatever is on screen.
pub fn geometry(
    painter: &egui::Painter,
    area: egui::Rect,
    bottom: f32,
    rows: &[Row],
    first: usize,
) -> Geometry {
    let room = egui::vec2(
        (area.width() - CARD_MARGIN * 2.0).max(0.0),
        (bottom - CARD_MARGIN) - (area.top() + CARD_MARGIN),
    );
    let fits = ((room.y - CARD_PAD * 2.0) / CARD_ROW).floor().max(1.0) as usize;
    let per = fits.min(COLUMN);
    let keys: Vec<f32> = rows
        .iter()
        .map(|row| text_width(painter, &row.keys, key_font(FONT)))
        .collect();
    let words: Vec<f32> = rows
        .iter()
        .map(|row| text_width(painter, &row.label, egui::FontId::proportional(FONT)))
        .collect();
    let measure = |span: &std::ops::Range<usize>| {
        let widest = |widths: &[f32]| widths[span.clone()].iter().fold(0.0f32, |m, w| m.max(*w));
        let key_w = widest(&keys);
        (key_w, PAD_X + key_w + KEY_GAP + widest(&words) + PAD_X)
    };
    // `count` columns of `per` rows, the last one taking whatever is left.
    // `count` columns: of `per` rows while that many fit the width, the last
    // one shorter. Fewer than that, and the rows are shared out evenly — no
    // two columns more than a row apart, the longer ones first — so the
    // columns scroll as one block whose every column keeps rows on the card
    // however far it goes (in any window with room for two rows a column),
    // where columns of `per` with the rest piled into the last would scroll
    // the others empty.
    let spans = |count: usize| -> Vec<std::ops::Range<usize>> {
        if count == 0 || count >= rows.len().div_ceil(per) {
            return (0..rows.len().div_ceil(per))
                .map(|c| c * per..((c + 1) * per).min(rows.len()))
                .collect();
        }
        let (each, longer) = (rows.len() / count, rows.len() % count);
        let mut start = 0;
        (0..count)
            .map(|c| {
                let span = start..start + each + usize::from(c < longer);
                start = span.end;
                span
            })
            .collect()
    };
    let span_width = |widths: &[(f32, f32)]| {
        widths.iter().map(|(_, width)| width).sum::<f32>()
            + ROW_SEP * widths.len().saturating_sub(1) as f32
            + CARD_PAD * 2.0
    };
    let mut count = rows.len().div_ceil(per);
    let (columns, widths) = loop {
        let columns = spans(count);
        let widths: Vec<(f32, f32)> = columns.iter().map(measure).collect();
        if count <= 1 || span_width(&widths) <= room.x {
            break (columns, widths);
        }
        count -= 1;
    };
    let total = columns.iter().map(|span| span.len()).max().unwrap_or(0);
    let shown = total.min(per);
    let first = first.min(total - shown);
    let size = egui::vec2(
        span_width(&widths).min(room.x.max(CARD_PAD * 2.0)),
        shown as f32 * CARD_ROW + CARD_PAD * 2.0,
    );
    let card = egui::Rect::from_min_size(
        egui::pos2(
            (area.center().x - size.x / 2.0).max(area.left() + CARD_MARGIN),
            (bottom - CARD_MARGIN - size.y).max(area.top() + CARD_MARGIN),
        ),
        size,
    );
    let clip = card.shrink(CARD_PAD);
    let mut rects = Vec::with_capacity(rows.len());
    let mut labels = Vec::with_capacity(rows.len());
    let mut left = clip.left();
    for (span, (key_w, width)) in columns.iter().zip(&widths) {
        for i in 0..span.len() {
            let top = clip.top() + (i as f32 - first as f32) * CARD_ROW;
            rects.push(egui::Rect::from_min_size(
                egui::pos2(left, top),
                // A column wider than the card can be, alone in a window
                // narrower than it, stops at the card's padding.
                egui::vec2(width.min(clip.right() - left).max(0.0), CARD_ROW),
            ));
            labels.push(left + PAD_X + key_w + KEY_GAP);
        }
        left += width + ROW_SEP;
    }
    Geometry {
        card,
        rows: rects,
        labels,
        clip,
        per: shown,
        total,
        first,
        band: crate::scrollbar::band(card, clip, shown as f32, total as f32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use df_core::keymap::WHICH_KEY_DELAY;

    /// The whole point of the delay: a chord finished before it elapses never
    /// puts a card on screen, and never asks for a frame to take one off.
    #[test]
    fn a_fast_typist_never_sees_the_card() {
        let t0 = Instant::now();
        let mut card = WhichKey::new();
        let quick = t0 + Duration::from_millis(90);
        card.update(Some(t0 + WHICH_KEY_DELAY), quick);
        assert!(!card.visible(quick));
        // …and the second key of the chord resolves it.
        card.update(None, quick);
        assert!(!card.visible(quick));
        assert_eq!(card.deadline(None), None);
    }

    /// A hand that stopped gets the card, at once, at the moment it is due.
    #[test]
    fn a_hesitation_shows_the_card_instantly_when_it_is_due() {
        let t0 = Instant::now();
        let due = t0 + WHICH_KEY_DELAY;
        let mut card = WhichKey::new();

        card.update(Some(due), t0);
        assert!(!card.visible(t0));
        assert_eq!(card.deadline(Some(due)), Some(due), "one scheduled wake-up");

        card.update(Some(due), due);
        assert_eq!(card.alpha(due), 1.0, "instant in, no fade-in");
        assert_eq!(card.deadline(Some(due)), None, "a held card costs nothing");
    }

    /// Resolving a chord the card was up for fades it rather than cutting it.
    #[test]
    fn resolving_a_shown_chord_fades_the_card_out() {
        let t0 = Instant::now();
        let due = t0 + WHICH_KEY_DELAY;
        let mut card = WhichKey::new();
        card.update(Some(due), due);

        card.update(None, due);
        assert_eq!(card.alpha(due), 1.0, "the fade starts from full");
        assert_eq!(card.deadline(None), Some(due + FADE_OUT));
        let mid = due + FADE_OUT / 2;
        let a = card.alpha(mid);
        assert!(a > 0.0 && a < 1.0, "got {a}");

        let end = due + FADE_OUT;
        assert_eq!(card.alpha(end), 0.0);
        card.update(None, end);
        assert!(!card.visible(end));
        assert_eq!(card.deadline(None), None, "and then it is asleep");
    }

    fn rows(n: usize) -> Vec<Row> {
        (0..n)
            .map(|i| Row {
                keys: format!("g {i}"),
                label: format!("Go to the place called number {i}"),
                next: Chord::plain(df_core::keymap::Key::Char('a')),
            })
            .collect()
    }

    fn with_painter(mut f: impl FnMut(&egui::Painter)) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| f(ui.painter()));
    }

    fn screen(width: f32, height: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, height))
    }

    /// A window big enough lays the card out as it always was: columns of
    /// nine, side by side, nothing to scroll and no bar.
    #[test]
    fn a_big_window_lays_the_card_out_in_columns_of_nine() {
        with_painter(|painter| {
            let area = screen(1400.0, 900.0);
            let g = geometry(painter, area, area.bottom(), &rows(25), 0);
            assert_eq!((g.per, g.total), (COLUMN, COLUMN));
            assert!(area.contains_rect(g.card));
            assert_eq!(g.band, None);
            assert_eq!(bar(&g), None);
            let columns: std::collections::BTreeSet<i64> =
                g.rows.iter().map(|row| row.left() as i64).collect();
            assert_eq!(columns.len(), 3, "nine, nine and seven");
            assert!(g.rows.iter().all(|row| g.clip.contains(row.center())));
        });
    }

    /// A window too short for nine rows makes the columns shorter and more
    /// of them; one too narrow for those as well shares the rows out among
    /// the columns it has room for and scrolls all of them as one block, down
    /// and never sideways. The card is inside the window every time, and only
    /// the rows inside it take the pointer.
    #[test]
    fn a_small_window_keeps_the_card_inside_it_and_scrolls_the_rest() {
        with_painter(|painter| {
            let short = screen(1400.0, 200.0);
            let g = geometry(painter, short, short.bottom(), &rows(25), 0);
            assert!(short.contains_rect(g.card), "{:?}", g.card);
            assert!(g.per < COLUMN, "shorter columns");
            assert_eq!(g.total, g.per, "and enough of them: nothing to scroll");
            assert_eq!(g.band, None);
            assert!(g.rows.iter().all(|row| g.clip.contains(row.center())));

            let small = screen(400.0, 200.0);
            let g = geometry(painter, small, small.bottom(), &rows(25), 0);
            assert!(small.contains_rect(g.card), "{:?}", g.card);
            assert!(g.total > g.per, "the columns run on");
            assert!(g.band.is_some() && bar(&g).is_some());
            let mut drawn = g.rows.iter().filter(|row| g.clip.contains(row.center()));
            assert!(drawn.all(|row| g.card.contains_rect(*row)));
            let hidden = g.rows.last().expect("rows");
            assert!(!g.clip.contains(hidden.center()));
            assert_eq!(g.row_at(hidden.center()), None, "a row off the card");

            // The wheel scrolls every column together, and stops at the end.
            let mut which = WhichKey::new();
            which.wheel(-3.0 * CARD_ROW, &g);
            assert_eq!(which.first(), 3);
            let scrolled = geometry(painter, small, small.bottom(), &rows(25), which.first());
            assert_eq!(scrolled.rows[3].top(), scrolled.clip.top());
            assert_eq!(scrolled.row_at(scrolled.rows[3].center()), Some(3));
            for _ in 0..20 {
                which.wheel(-3.0 * CARD_ROW, &scrolled);
            }
            assert_eq!(which.first(), g.total - g.per);
            let end = geometry(painter, small, small.bottom(), &rows(25), 1_000);
            assert_eq!(end.first, g.total - g.per, "kept inside the columns");
            // New rows start from their top.
            which.rows_changed();
            assert_eq!(which.first(), 0);
        });
    }

    /// Columns that do not all fit the width share the rows out evenly — no
    /// two more than a row apart — so the block scrolls as one rectangle and,
    /// wherever it has scrolled to, no column is left with nothing on the
    /// card. Swept over window sizes, and over every row the view can start
    /// at in each.
    #[test]
    fn a_scrolled_card_never_empties_a_column() {
        let short: Vec<Row> = (0..25)
            .map(|i| Row {
                keys: format!("g {i}"),
                label: format!("Row {i}"),
                next: Chord::plain(df_core::keymap::Key::Char('a')),
            })
            .collect();
        with_painter(|painter| {
            let mut shared = 0;
            for width in (150..=700).step_by(10) {
                for height in [150.0, 175.0, 200.0, 250.0, 300.0] {
                    let area = screen(width as f32, height);
                    let g = geometry(painter, area, area.bottom(), &short, 0);
                    assert!(area.contains_rect(g.card), "{area:?}: {:?}", g.card);
                    // The columns, by where their rows start across the card.
                    let mut lefts: Vec<f32> = g.rows.iter().map(|row| row.left()).collect();
                    lefts.dedup();
                    let heights: Vec<usize> = lefts
                        .iter()
                        .map(|left| g.rows.iter().filter(|row| row.left() == *left).count())
                        .collect();
                    if g.total == g.per {
                        continue;
                    }
                    if heights.len() > 1 {
                        shared += 1;
                        let (low, high) = (heights.iter().min(), heights.iter().max());
                        assert!(
                            high.zip(low).is_some_and(|(high, low)| high - low <= 1),
                            "{area:?}: uneven columns {heights:?}"
                        );
                    }
                    for first in 0..=g.total - g.per {
                        let g = geometry(painter, area, area.bottom(), &short, first);
                        for left in &lefts {
                            let on_card = g
                                .rows
                                .iter()
                                .filter(|row| row.left() == *left)
                                .any(|row| g.clip.contains(row.center()));
                            assert!(on_card, "{area:?} from row {first}: a column is empty");
                        }
                    }
                }
            }
            assert!(
                shared > 0,
                "no window in the sweep shared rows among columns"
            );

            // The case the rule is for: twenty-five rows, seven to a column,
            // two columns' width — thirteen and twelve, not seven and eighteen.
            let two = (150..=700)
                .step_by(5)
                .map(|width| screen(width as f32, 200.0))
                .map(|area| geometry(painter, area, area.bottom(), &short, 0))
                .find(|g| {
                    let mut lefts: Vec<f32> = g.rows.iter().map(|row| row.left()).collect();
                    lefts.dedup();
                    g.per == 7 && lefts.len() == 2
                })
                .expect("a width for two columns");
            assert_eq!((two.total, two.per), (13, 7), "a range of six rows");
        });
    }

    /// A second chord started mid-fade takes the card back rather than waiting
    /// for the first one's exit to finish.
    #[test]
    fn a_new_chord_interrupts_the_fade() {
        let t0 = Instant::now();
        let mut card = WhichKey::new();
        card.update(Some(t0), t0);
        card.update(None, t0);
        let later = t0 + FADE_OUT / 2;
        let next_due = later + WHICH_KEY_DELAY;
        card.update(Some(next_due), later);
        assert_eq!(card.alpha(later), 0.0, "the old card is gone at once");
        assert_eq!(card.deadline(Some(next_due)), Some(next_due));
    }
}
