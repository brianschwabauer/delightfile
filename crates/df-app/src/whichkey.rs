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
    /// (`delightful-ui` §15).
    pub rows: Vec<egui::Rect>,
    /// Where each row's label starts: its column's keys all take the width of
    /// the widest.
    pub labels: Vec<f32>,
}

impl Geometry {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|row| row.contains(pos))
    }
}

/// Lay the card out for `rows`, sitting above `bottom`.
///
/// Bottom-anchored and horizontally centred: the card is an answer to
/// something the hand is doing right now, so it belongs where the eyes are —
/// and near the bottom edge, which is where every other transient thing
/// appears. Needs a painter because the columns are measured from their text.
pub fn geometry(painter: &egui::Painter, area: egui::Rect, bottom: f32, rows: &[Row]) -> Geometry {
    let columns: Vec<&[Row]> = rows.chunks(COLUMN).collect();
    let measure = |group: &[Row]| {
        let key_w = group.iter().fold(0.0f32, |m, row| {
            m.max(text_width(painter, &row.keys, key_font(FONT)))
        });
        let label_w = group.iter().fold(0.0f32, |m, row| {
            m.max(text_width(
                painter,
                &row.label,
                egui::FontId::proportional(FONT),
            ))
        });
        (key_w, PAD_X + key_w + KEY_GAP + label_w + PAD_X)
    };
    let widths: Vec<(f32, f32)> = columns.iter().map(|group| measure(group)).collect();
    let tall = columns.iter().map(|group| group.len()).max().unwrap_or(0);
    let size = egui::vec2(
        widths.iter().map(|(_, width)| width).sum::<f32>()
            + ROW_SEP * (columns.len().saturating_sub(1)) as f32
            + CARD_PAD * 2.0,
        tall as f32 * CARD_ROW + CARD_PAD * 2.0,
    );
    let card = egui::Rect::from_min_size(
        egui::pos2(
            (area.center().x - size.x / 2.0).max(area.left() + CARD_MARGIN),
            (bottom - CARD_MARGIN - size.y).max(area.top() + CARD_MARGIN),
        ),
        size,
    );
    let mut rects = Vec::with_capacity(rows.len());
    let mut labels = Vec::with_capacity(rows.len());
    let mut left = card.left() + CARD_PAD;
    for (group, (key_w, width)) in columns.iter().zip(&widths) {
        for i in 0..group.len() {
            rects.push(egui::Rect::from_min_size(
                egui::pos2(left, card.top() + CARD_PAD + i as f32 * CARD_ROW),
                egui::vec2(*width, CARD_ROW),
            ));
            labels.push(left + PAD_X + key_w + KEY_GAP);
        }
        left += width + ROW_SEP;
    }
    Geometry {
        card,
        rows: rects,
        labels,
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
