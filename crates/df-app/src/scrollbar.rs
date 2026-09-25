//! The scrollbars down the list and parent panes' right edges, and the numbers
//! the preview pane's shares with them.
//!
//! One bar, not two: the preview's reports a position and the panes' can be
//! taken hold of, but they are the same thin thumb in the same colour, held
//! for the same linger and faded over the same fade. A second set of numbers
//! would be two scrollbars in one window that disagree about what a scrollbar
//! looks like.
//!
//! The floating cards whose lists can outgrow a short window — the palette,
//! the task panel, the dialogs, the tray, the which-key card — wear it too
//! ([`card`], [`paint_card`]), for the same reason: a list cut off at a card's
//! edge has to say that it goes on, and the one bar in the window is how
//! anything here says that. Theirs is only ever read — the keys and the wheel
//! move a card's list — but it comes and goes by the panes' rule, the linger
//! after a scroll or the pointer on its band, and by nothing else.
//!
//! ## Why the hit band is wider than the thumb
//!
//! The thumb is [`WIDTH`] points: it reports a position and must not crowd the
//! rows beside it. A target that thin is one the hand has to aim at, so the
//! pointer is given [`HIT_WIDTH`] flush with the pane's edge instead
//! (`delightful-ui` §1: the target is bigger than the mark). A press anywhere
//! in that band at the thumb's height takes the thumb.

use std::time::{Duration, Instant};

use crate::ui::{Painting, GAP};

/// The thumb's width, in logical points. Thin: it says where you are, and the
/// band around it ([`HIT_WIDTH`]) is what the hand aims at.
pub const WIDTH: f32 = 3.0;

/// The thumb's corner radius, which makes it a bar with softened ends.
pub const RADIUS: u8 = (WIDTH / 2.0) as u8;

/// The shortest the thumb gets, so a 20,000-row listing still has something
/// visible to point at — and to take hold of.
pub const MIN_THUMB: f32 = 24.0;

/// The band along the pane's right edge a press lands in, in logical points.
pub const HIT_WIDTH: f32 = 12.0;

/// How far the thumb sits in from the pane's edges. The same on the right as
/// at the top and bottom, so the thumb at either end of its travel keeps an
/// even gap to the corner it is turning (`delightful-ui` §15).
const INSET: f32 = GAP / 2.0;

/// How long the bar is held after the last scroll.
///
/// PLAN §8's transient chrome holds ~2.5–3 s; a scrollbar is the quietest thing
/// in that family — it answers "where am I", which is a question asked *while*
/// scrolling — so it takes the short end of the range.
pub const LINGER: Duration = Duration::from_millis(2000);

/// …then leaves over this. Half PLAN §8's 500 ms, because the bar is 3 points
/// wide and a longer fade on something that small reads as a rendering bug.
pub const FADE: Duration = Duration::from_millis(250);

/// Where one pane's bar is drawn, and where the pointer can take it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// The line the thumb travels along: the pane's height, less the inset.
    pub track: egui::Rect,
    pub thumb: egui::Rect,
    /// The band a press lands in ([`HIT_WIDTH`]), the pane's full height.
    pub hit: egui::Rect,
    /// The last row the view can start at — what the bottom of the travel
    /// stands for.
    max_first: f32,
}

/// The bar for a pane showing rows from `first` (fractional mid-slide), with
/// `visible` rows fitting and `total` rows in all — or `None` when everything
/// fits and there is nothing to scroll.
///
/// Counted in *rows of the pane*, as [`crate::tab::Listing::wheel`] counts
/// them: a row of tiles in the grid. `visible` is the whole rows that fit,
/// the number the wheel clamps against, so the thumb reaches the bottom
/// exactly where the wheel stops.
pub fn geometry(pane: egui::Rect, first: f32, visible: f32, total: f32) -> Option<Geometry> {
    if visible <= 0.0 || total <= visible {
        return None;
    }
    let track = egui::Rect::from_min_max(
        egui::pos2(pane.right() - INSET - WIDTH, pane.top() + INSET),
        egui::pos2(
            pane.right() - INSET,
            (pane.bottom() - INSET).max(pane.top() + INSET),
        ),
    );
    let height = (track.height() * visible / total)
        .max(MIN_THUMB)
        .min(track.height());
    let max_first = total - visible;
    let travel = track.height() - height;
    let top = track.top() + travel * (first / max_first).clamp(0.0, 1.0);
    let thumb = egui::Rect::from_min_size(egui::pos2(track.left(), top), egui::vec2(WIDTH, height));
    let hit = egui::Rect::from_min_max(egui::pos2(pane.right() - HIT_WIDTH, pane.top()), pane.max);
    Some(Geometry {
        track,
        thumb,
        hit,
        max_first,
    })
}

impl Geometry {
    /// Is the pointer in the bar's band?
    pub fn contains(&self, at: egui::Pos2) -> bool {
        self.hit.contains(at)
    }

    /// Is it on the thumb — anywhere across the band, at the thumb's height?
    pub fn on_thumb(&self, at: egui::Pos2) -> bool {
        self.contains(at) && (self.thumb.top()..=self.thumb.bottom()).contains(&at.y)
    }

    /// The first row a thumb whose top is at `top` stands for, fractional.
    /// The inverse of where [`geometry`] puts the thumb.
    pub fn first_at(&self, top: f32) -> f32 {
        let travel = self.track.height() - self.thumb.height();
        if travel <= 0.0 {
            return 0.0;
        }
        ((top - self.track.top()) / travel).clamp(0.0, 1.0) * self.max_first
    }
}

/// How visible the bar is, 0–1, for a view last scrolled at `scrolled_at`:
/// held for [`LINGER`], then eased away over [`FADE`] (PLAN §8's "linger then
/// leave").
pub fn alpha(scrolled_at: Option<Instant>, now: Instant) -> f32 {
    let Some(at) = scrolled_at else {
        return 0.0;
    };
    let elapsed = now.saturating_duration_since(at);
    if elapsed < LINGER {
        return 1.0;
    }
    let over = (elapsed - LINGER).as_secs_f32() / FADE.as_secs_f32();
    (1.0 - over).clamp(0.0, 1.0)
}

/// How visible a pane's bar is, all told: the linger after the last scroll
/// ([`alpha`]), or the pointer's hover over its band (`lit`, 0–1, which fades
/// on its own when the pointer leaves), or a hand on its thumb (`held`) —
/// whichever says the most.
pub fn visibility(scrolled_at: Option<Instant>, lit: f32, held: bool, now: Instant) -> f32 {
    let held = if held { 1.0 } else { 0.0 };
    alpha(scrolled_at, now).max(lit).max(held)
}

/// Whether the bar is mid-fade and owed frames. The fade only: a bar held
/// bright is the same pixels next frame and asks for nothing (PLAN §1).
pub fn fading(scrolled_at: Option<Instant>, now: Instant) -> bool {
    matches!(alpha(scrolled_at, now), a if a > 0.0 && a < 1.0)
}

/// How long until the linger ends and the fade is owed its first frame: a
/// single instant known in advance, so a wake-up rather than a poll.
pub fn deadline(scrolled_at: Option<Instant>, now: Instant) -> Option<Duration> {
    scrolled_at
        .map(|at| (at + LINGER).saturating_duration_since(now))
        .filter(|d| !d.is_zero())
}

/// Draw one bar.
///
/// `alpha` is how visible the whole bar is. `lit` is the pointer's hover over
/// the band, 0–1, which brightens the thumb and brings its track up; `held` is
/// a hand dragging the thumb, which does both at full strength.
pub fn paint(paint: &Painting<'_>, bar: &Geometry, alpha: f32, lit: f32, held: bool) {
    if alpha <= 0.0 {
        return;
    }
    let plate = if held { 1.0 } else { lit };
    if plate > 0.0 {
        paint.painter.rect_filled(
            bar.track,
            RADIUS,
            paint.palette.surface0.gamma_multiply(0.5 * plate * alpha),
        );
    }
    let color = if held {
        paint.palette.subtext0
    } else {
        crate::theme::mix(paint.palette.overlay0, paint.palette.overlay1, lit)
    };
    paint
        .painter
        .rect_filled(bar.thumb, RADIUS, color.gamma_multiply(alpha));
}

// ── The cards' bars ─────────────────────────────────────────────────────────

/// When a card's list last moved, for its bar's linger ([`alpha`]).
///
/// A pane stamps its scroll where the scroll happens
/// ([`crate::tab::Listing`]). A card's list moves for more reasons than it has
/// places to stamp one — a key, the wheel, a click, a window made shorter
/// under its cursor — so the card is told instead, once a frame, where its
/// view starts, and a start that differs from the last frame's is a scroll.
/// The first frame it hears anything is not one: a card opening is not a card
/// scrolling.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Linger {
    first: Option<f32>,
    scrolled_at: Option<Instant>,
}

impl Linger {
    /// The view starts at `first` this frame, in whatever the card counts its
    /// list in: rows, lines, points.
    pub fn saw(&mut self, first: f32, now: Instant) {
        if self.first.is_some_and(|was| was != first) {
            self.scrolled_at = Some(now);
        }
        self.first = Some(first);
    }

    pub fn scrolled_at(&self) -> Option<Instant> {
        self.scrolled_at
    }
}

/// Which floating card a bar belongs to, so the hover on its band
/// ([`crate::ui::Control::CardBar`]) is that card's and no other's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Palette,
    Tasks,
    Mounts,
    Confirm,
    Conflict,
    Sync,
    Picker,
    Spot,
    Tray,
    WhichKey,
}

/// A card's bar: down the card's right-hand padding beside `body`, the part of
/// the card its rows are drawn in, or `None` when every row fits.
///
/// In the padding rather than over the rows: the padding is the one strip of
/// the card nothing else is drawn on, and the band a pointer lights the bar
/// from is flush with the card's edge, as a pane's is with the pane's.
pub fn card(
    card: egui::Rect,
    body: egui::Rect,
    first: f32,
    visible: f32,
    total: f32,
) -> Option<Geometry> {
    let pane = egui::Rect::from_min_max(body.min, egui::pos2(card.right(), body.bottom()));
    geometry(pane, first, visible, total)
}

/// Where a card's bar can be pointed at: its [`HIT_WIDTH`] band at the card's
/// edge, beside `body`, while the list is longer than the card shows. The one
/// part of the bar that does not move as the list scrolls, so a card measured
/// before its view settled still has it right.
pub fn band(card: egui::Rect, body: egui::Rect, visible: f32, total: f32) -> Option<egui::Rect> {
    self::card(card, body, 0.0, visible, total).map(|bar| bar.hit)
}

/// Draw a card's bar by the panes' rule: up for the [`LINGER`] after its list
/// last moved and while the pointer is on its band (`lit`, the band's hover,
/// which fades on its own when the pointer leaves), faded over [`FADE`].
/// Never held: no hand takes hold of a card's bar.
///
/// `fade` is the card's own, for a card on its way out.
pub fn paint_card(
    painting: &Painting<'_>,
    bar: &Geometry,
    lit: f32,
    scrolled_at: Option<Instant>,
    fade: f32,
) {
    let alpha = visibility(scrolled_at, lit, false, painting.now);
    self::paint(painting, bar, alpha * fade, lit, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(300.0, 400.0))
    }

    /// Nothing to scroll, no bar.
    #[test]
    fn a_listing_that_fits_has_no_bar() {
        assert_eq!(geometry(pane(), 0.0, 20.0, 20.0), None);
        assert_eq!(geometry(pane(), 0.0, 20.0, 5.0), None);
        assert_eq!(geometry(pane(), 0.0, 0.0, 5.0), None, "no rows fit");
        assert!(geometry(pane(), 0.0, 20.0, 21.0).is_some());
    }

    /// The thumb is never shorter than the floor, its top is the track's top
    /// at the first row and its bottom the track's bottom at the last, and
    /// the travel between maps back to the row it was drawn for.
    #[test]
    fn the_thumb_travels_the_track_from_first_to_last_row() {
        let total = 10_000.0;
        let visible = 20.0;
        let top = geometry(pane(), 0.0, visible, total).expect("overflows");
        assert!(top.thumb.height() >= MIN_THUMB, "{:?}", top.thumb);
        assert_eq!(top.thumb.top(), top.track.top());
        assert_eq!(top.first_at(top.thumb.top()), 0.0);

        let last = total - visible;
        let bottom = geometry(pane(), last, visible, total).expect("overflows");
        assert!((bottom.thumb.bottom() - bottom.track.bottom()).abs() < 1e-3);
        assert!((bottom.first_at(bottom.thumb.top()) - last).abs() < 1e-2);

        let half = geometry(pane(), last / 2.0, visible, total).expect("overflows");
        assert!((half.first_at(half.thumb.top()) - last / 2.0).abs() < 1e-1);
        // A drag past either end holds at that end.
        assert_eq!(half.first_at(half.track.top() - 50.0), 0.0);
        assert_eq!(half.first_at(half.track.bottom() + 50.0), last);
    }

    /// A short overflow gets a long thumb: its height is the share of the
    /// listing on screen.
    #[test]
    fn the_thumb_is_the_share_of_the_listing_on_screen() {
        let bar = geometry(pane(), 0.0, 30.0, 40.0).expect("overflows");
        let share = bar.thumb.height() / bar.track.height();
        assert!((share - 0.75).abs() < 1e-3, "{share}");
    }

    /// The thumb sits in from the pane's right edge by the inset, and the band
    /// the pointer lands in is wider than it and flush with that edge.
    #[test]
    fn the_band_is_wider_than_the_thumb_and_flush_with_the_edge() {
        let pane = pane();
        let bar = geometry(pane, 0.0, 20.0, 100.0).expect("overflows");
        assert_eq!(bar.thumb.right(), pane.right() - GAP / 2.0);
        assert_eq!(bar.thumb.width(), WIDTH);
        assert_eq!(bar.hit.right(), pane.right());
        assert_eq!(bar.hit.width(), HIT_WIDTH);
        assert_eq!(bar.hit.height(), pane.height());
        assert!(bar.hit.contains_rect(bar.thumb));
        // Across the band at the thumb's height is the thumb; below it is the
        // track.
        let beside = egui::pos2(pane.right() - 1.0, bar.thumb.center().y);
        assert!(bar.on_thumb(beside));
        let below = egui::pos2(bar.thumb.center().x, bar.thumb.bottom() + 10.0);
        assert!(bar.contains(below) && !bar.on_thumb(below));
    }

    /// Held for the linger, faded over the fade, then gone; frames are owed
    /// only for the fade, and one wake-up for the moment it starts.
    #[test]
    fn the_bar_lingers_then_fades() {
        let at = Instant::now();
        assert_eq!(alpha(None, at), 0.0);
        assert_eq!(alpha(Some(at), at), 1.0);
        assert_eq!(alpha(Some(at), at + LINGER - Duration::from_millis(1)), 1.0);
        let mid = at + LINGER + FADE / 2;
        assert!((alpha(Some(at), mid) - 0.5).abs() < 0.01);
        assert!(fading(Some(at), mid));
        assert!(!fading(Some(at), at), "held bright asks for nothing");
        assert_eq!(alpha(Some(at), at + LINGER + FADE), 0.0);
        assert!(!fading(Some(at), at + LINGER + FADE));
        assert_eq!(deadline(Some(at), at), Some(LINGER));
        assert_eq!(deadline(Some(at), at + LINGER), None);
    }

    /// Long after the last scroll the bar is gone — unless the pointer is in
    /// its band, or a hand is on its thumb.
    #[test]
    fn hover_and_a_hand_keep_the_bar_up() {
        let at = Instant::now();
        let later = at + LINGER + FADE;
        assert_eq!(visibility(Some(at), 0.0, false, at), 1.0, "just scrolled");
        assert_eq!(visibility(Some(at), 0.0, false, later), 0.0);
        assert_eq!(visibility(None, 0.0, false, later), 0.0);
        assert_eq!(visibility(Some(at), 1.0, false, later), 1.0, "hovered");
        assert_eq!(visibility(Some(at), 0.4, false, later), 0.4, "hover fading");
        assert_eq!(visibility(None, 0.0, true, later), 1.0, "held");
    }

    /// A card's list scrolls when its first row changes from one frame to the
    /// next, and only then: opening the card, or drawing it again where it
    /// was, is not a scroll.
    #[test]
    fn a_card_lingers_from_the_frame_its_view_moved() {
        let at = Instant::now();
        let mut linger = Linger::default();
        linger.saw(4.0, at);
        assert_eq!(linger.scrolled_at(), None, "opening is not a scroll");
        linger.saw(4.0, at + LINGER);
        assert_eq!(linger.scrolled_at(), None, "nor is standing still");
        let moved = at + LINGER * 2;
        linger.saw(5.0, moved);
        assert_eq!(linger.scrolled_at(), Some(moved));
        linger.saw(5.0, moved + FADE);
        assert_eq!(linger.scrolled_at(), Some(moved), "the stamp is the move's");
    }

    /// A card's bar sits in the card's right padding beside its rows, and a
    /// card whose rows fit has none.
    #[test]
    fn a_cards_bar_is_in_its_padding_and_only_when_it_overflows() {
        let card = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(300.0, 200.0));
        let body = egui::Rect::from_min_max(egui::pos2(110.0, 90.0), egui::pos2(390.0, 230.0));
        assert_eq!(super::card(card, body, 0.0, 7.0, 7.0), None);
        let bar = super::card(card, body, 0.0, 7.0, 20.0).expect("overflows");
        assert!(bar.thumb.left() >= body.right(), "clear of the rows");
        assert!(bar.thumb.right() <= card.right(), "inside the card");
        assert!(bar.track.top() >= body.top() && bar.track.bottom() <= body.bottom());
    }
}
