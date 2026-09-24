//! The pointer's own arithmetic: what a click *is*, what a drag encloses, and
//! how far a wheel roll travels (PLAN §7.5).
//!
//! Everything here is a pure function or a small `Copy` state machine over
//! `(positions, Instant)`, for the reason [`crate::motion`] gives: a gesture
//! decided inside the paint loop is a gesture that can only be tested by
//! waving a mouse at it. The event routing itself lives in [`crate::app`];
//! this module answers the questions that routing asks.
//!
//! ## Momentum is one eased animation
//!
//! PLAN §8 is explicit: "momentum is one eased animation, never a physics
//! loop", and delightviewer's `gesture.rs` is the reference — a wheel roll
//! writes a *target* and the view eases toward it on [`Easing::OutQuint`],
//! retargeting from wherever it has got to when the next notch arrives. There
//! is no velocity, no friction and no per-frame integration, which is what
//! makes a dropped frame cost nothing and lets the animation say exactly when
//! it has arrived (PLAN §1's idle rule).

use std::time::{Duration, Instant};

use crate::hover::Key;
use crate::motion::{Easing, Tween};

/// Two clicks inside this window on the same control are a double click.
///
/// 300 ms is delightviewer's `DOUBLE_CLICK_WINDOW`, which is in turn the
/// Carousel's, which is in turn the platform default nearly everywhere. It is
/// the one timing in the program that must match what the user's other
/// applications do, so it is *borrowed*, not chosen.
pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(300);

/// How far the pointer may move between the two clicks and still be double
/// clicking, in logical points.
///
/// Six: about the travel of a hand that is holding still on a high-DPI
/// display, and comfortably under a row's height — so a "double click" that
/// actually landed on two different rows is never read as one.
pub const DOUBLE_CLICK_SLOP: f32 = 6.0;

/// How far the pointer must travel with the button down before a press becomes
/// a drag, in logical points.
///
/// delightstack's threshold is 5 px for a mouse drag; ten here because both
/// gestures this arms are *destructive-adjacent* — a band select throws away
/// the current selection, and (next phase) a row drag moves files. Twice the
/// web number is the difference between "you moved" and "you meant to move".
pub const DRAG_THRESHOLD: f32 = 10.0;

/// How long the view takes to coast to where a wheel roll sent it.
///
/// delightviewer's `WHEEL_DURATION`. Long enough that the rows visibly *travel*
/// rather than teleport — which is the whole reason momentum exists, since a
/// list that jumps gives the eye nothing to track — and short enough that a
/// second notch lands while the first is still most of the way through, so a
/// continuous roll reads as one glide instead of a series of hops. Because the
/// curve is front-loaded ([`Easing::OutQuint`]) the rows are visually in place
/// after about 110 ms and the rest is the settle.
pub const WHEEL_GLIDE: Duration = Duration::from_millis(300);

/// The most one frame's worth of wheel may travel, in rows.
///
/// delightviewer clamps normalized wheel delta for the same reason: a chunky
/// mouse wheel, a smooth trackpad and a compositor that reports *page* units
/// all arrive on the same field, and one page-unit event unclamped is a jump
/// of a whole screenful that no hand asked for. Six rows is about two notches
/// of a normal wheel — fast, and still a distance the eye can follow.
pub const WHEEL_MAX_ROWS: f32 = 6.0;

/// The last press, so the next one can be told whether it is the second half of
/// a double click — or, for a text field, the third of a triple.
///
/// Keyed by control rather than by position alone: two clicks 4 px apart that
/// happen to straddle a row boundary are two clicks on two rows, and opening
/// the second one because the first was nearby would be the pointer acting on
/// something the user never pressed.
///
/// What is kept is the length of the **run** the last press ended: how many
/// presses in a row have landed on the same control, each inside
/// [`DOUBLE_CLICK_WINDOW`] and [`DOUBLE_CLICK_SLOP`] of the one before. The two
/// questions asked of it are both read off that one number, so a control that
/// asks "double?" ([`Clicks::press`]) and the prompt that asks "how many?"
/// ([`Clicks::count`]) cannot disagree about what the hand did.
pub struct Clicks<K: Key> {
    last: Option<(K, egui::Pos2, Instant, u32)>,
}

impl<K: Key> Default for Clicks<K> {
    fn default() -> Clicks<K> {
        Clicks { last: None }
    }
}

impl<K: Key> Clicks<K> {
    pub fn new() -> Clicks<K> {
        Clicks::default()
    }

    /// Record a press and answer whether it completes a double click.
    ///
    /// Presses **pair off**: a double click is the second of a pair, so a
    /// third quick click is a single again and a fourth another double, rather
    /// than "open" firing on every click of a drum roll. That is the even
    /// presses of a run.
    pub fn press(&mut self, key: K, at: egui::Pos2, now: Instant) -> bool {
        self.record(key, at, now).is_multiple_of(2)
    }

    /// Record a press and answer how many clicks it makes: 1, 2 or 3.
    ///
    /// For a text field, where the three are three different verbs — place
    /// the caret, take the word, take the line. A run longer than three
    /// **stays** at three rather than wrapping round to one: the fourth click
    /// of a quick run leaves the whole line selected instead of dropping it
    /// for a caret the hand did not aim.
    pub fn count(&mut self, key: K, at: egui::Pos2, now: Instant) -> u8 {
        self.record(key, at, now).min(3) as u8
    }

    /// Record a press and answer how long the run it belongs to is: 1 for a
    /// press on its own, one more than the last for a press that continues it.
    fn record(&mut self, key: K, at: egui::Pos2, now: Instant) -> u32 {
        let run = match self.last {
            Some((last_key, last_at, when, run))
                if last_key == key
                    && (at - last_at).length() <= DOUBLE_CLICK_SLOP
                    && now.saturating_duration_since(when) <= DOUBLE_CLICK_WINDOW =>
            {
                run.saturating_add(1)
            }
            _ => 1,
        };
        self.last = Some((key, at, now, run));
        run
    }

    /// Forget the history — a click somewhere else, a menu opening, anything
    /// that means the next click is not a continuation of this one.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// The rectangle two pointer positions enclose, in either order.
pub fn band(from: egui::Pos2, to: egui::Pos2) -> egui::Rect {
    egui::Rect::from_two_pos(from, to)
}

/// One corner of a band select: where it is, in window points, and how far the
/// list was scrolled (in rows of the pane) when it was put there.
///
/// The scroll rides along because a band's two corners are placed at different
/// times. The pointer's is placed this frame; the origin's was placed at the
/// press, and a band hanging over the pane's edge scrolls the list in between.
/// The origin belongs to the rows it was put down beside, not to a spot on the
/// glass (see [`crate::select::Band`]), so where it is *now* depends on how far
/// those rows have moved since.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corner {
    pub at: egui::Pos2,
    pub scroll: f32,
}

/// What a band covers of the list pane, as a rectangle in window points with
/// the list drawn at `scroll_rows` — or `None` when it covers nothing.
///
/// Each corner is clipped to the content box **as the list was when that
/// corner was placed**, and then carried along by however far the list has
/// scrolled since (`step` points to a row of the pane). For the pointer's
/// corner those are the same moment, so a pointer far outside the pane holds
/// the band to the pane's edge: a drag that leaves the window at speed selects
/// to the edge of the listing and stops, rather than resolving to row four
/// thousand. For the origin they are not: an origin put down beside row 5
/// stays beside row 5 when the band has scrolled the list to row 100, and
/// every row in between is in the band — which is the only way a band can hold
/// more than a screenful. With no scroll in between, all this is simply the
/// band clipped to the content box.
///
/// Horizontally the band is clipped to the content box too, and that is what
/// lets it start in another pane: one drawn from the parent column or the
/// preview pane covers nothing until its rectangle reaches the list, because a
/// row is a strip across the list's content box and nothing either side of it.
///
/// The result is zero pixels tall only for a band dragged straight across the
/// rows inside the pane, which still covers the row it is on. Two corners
/// clipped to the same edge — a band drawn entirely below the last row on
/// screen — are `None`, not a sliver lying on that edge.
pub fn band_span(
    content: egui::Rect,
    step: f32,
    scroll_rows: f32,
    from: Corner,
    to: Corner,
) -> Option<egui::Rect> {
    if content.height() <= 0.0 {
        return None;
    }
    let left = from.at.x.min(to.at.x).max(content.left());
    let right = from.at.x.max(to.at.x).min(content.right());
    if right < left {
        return None;
    }
    let place = |corner: Corner| {
        let y = corner.at.y.clamp(content.top(), content.bottom());
        (y + (corner.scroll - scroll_rows) * step, y != corner.at.y)
    };
    let (a, a_clipped) = place(from);
    let (b, b_clipped) = place(to);
    if a == b && (a_clipped || b_clipped) {
        return None;
    }
    Some(egui::Rect::from_min_max(
        egui::pos2(left, a.min(b)),
        egui::pos2(right, a.max(b)),
    ))
}

/// Which rows a band encloses, as an inclusive `(first, last)` — or `None` when
/// it encloses none.
///
/// A row counts as enclosed when the band overlaps its strip at all, which is
/// what makes the selection follow the pointer *as it moves* rather than only
/// once a whole row is covered. What the band is, after clipping and scrolling,
/// is [`band_span`]'s answer.
///
/// The band's bottom edge is **exclusive**: a row that starts exactly where
/// the band ends is not in it. A band whose far corner is outside the pane is
/// clipped to exactly the pane's edge, and at a whole-row scroll that edge is
/// also the top of the first row *below* the pane — counting the row the clip
/// only touches would select a file nobody saw the band reach.
pub fn band_rows(
    content: egui::Rect,
    scroll_rows: f32,
    rows: usize,
    row_height: f32,
    from: Corner,
    to: Corner,
) -> Option<(usize, usize)> {
    if rows == 0 || row_height <= 0.0 {
        return None;
    }
    let span = band_span(content, row_height, scroll_rows, from, to)?;
    // The band's edges in *row* coordinates: how many rows down the listing
    // each one falls, counting from the row the view is scrolled to.
    let row = |y: f32| (y - content.top()) / row_height + scroll_rows;
    let first = row(span.top()).floor();
    let last = if span.height() > 0.0 {
        row(span.bottom()).ceil() - 1.0
    } else {
        first
    };
    if last < 0.0 || first > (rows - 1) as f32 {
        return None;
    }
    let first = first.max(0.0) as usize;
    let last = (last.max(0.0) as usize).min(rows - 1);
    Some((first, last.max(first)))
}

/// How far past the list pane's edge a band has to hang for the listing to
/// scroll one row per frame, in logical points.
///
/// The rate is proportional to the overshoot, so the hand sets the speed: just
/// over the edge creeps a row at a time, which is how a band is *aimed* at the
/// row it wants to stop on, and a long pull races. Twenty points buys a row a
/// frame — a bit less than a row's own height, so "one row further" is about
/// one row's travel of the hand.
pub const BAND_SCROLL_OVERSHOOT: f32 = 20.0;

/// The fastest a band scrolls the listing, in rows per frame.
///
/// Six, which is [`WHEEL_MAX_ROWS`] and for the same reason: past it the names
/// going by stop being something the eye can follow, and a band that has
/// overshot its target by a screenful has to be dragged back through it.
pub const BAND_SCROLL_MAX: f32 = 6.0;

/// The frame those rates are counted in, in seconds: sixty to the second.
///
/// The app runs its animation frames at the monitor's refresh, and a rate
/// counted in *actual* frames would scroll a 144 Hz display more than twice as
/// fast as a 60 Hz one. So the rate is per reference frame and each real frame
/// takes its share by how long it lasted ([`band_scroll_rows`]).
pub const BAND_SCROLL_FRAME: f32 = 1.0 / 60.0;

/// The longest one frame's worth of band scroll may be, in seconds.
///
/// Three reference frames. The first frame of an autoscroll can arrive long
/// after the frame before it — the pointer rested inside the pane, the loop
/// went idle, and then the hand moved past the edge — and without a cap that
/// whole rest would be paid out as one jump.
const BAND_SCROLL_MAX_DT: f32 = BAND_SCROLL_FRAME * 3.0;

/// How fast a band hanging over the list pane's edge scrolls it, in rows per
/// reference frame ([`BAND_SCROLL_FRAME`]) — negative for up, zero while the
/// pointer is level with the pane.
///
/// Only the pointer's *height* is read. The list, parent and preview panes
/// share a top and a bottom, so "above the list pane" is "above the panes",
/// wherever along them the band was started — a band drawn from the preview
/// pane that is pulled off the bottom of the window scrolls the list exactly
/// as one drawn inside it does.
pub fn band_scroll(pane: egui::Rect, y: f32) -> f32 {
    let overshoot = if y < pane.top() {
        y - pane.top()
    } else if y > pane.bottom() {
        y - pane.bottom()
    } else {
        0.0
    };
    (overshoot / BAND_SCROLL_OVERSHOOT).clamp(-BAND_SCROLL_MAX, BAND_SCROLL_MAX)
}

/// This frame's share of a band scroll `rate` ([`band_scroll`]), given how long
/// the frame lasted: rows, fractional, for [`crate::tab::Listing::wheel`] to
/// carry.
pub fn band_scroll_rows(rate: f32, dt: Duration) -> f32 {
    rate * dt.as_secs_f32().min(BAND_SCROLL_MAX_DT) / BAND_SCROLL_FRAME
}

/// How far past the prompt field's edge a text selection has to be dragged for
/// the text to scroll one point per reference frame ([`BAND_SCROLL_FRAME`]),
/// in logical points.
///
/// Ten: a hand just over the edge creeps the text along at about eight
/// characters a second, slow enough to stop on the one it wants, and pulling
/// further out goes faster, as the band does over the list.
pub const FIELD_SCROLL_OVERSHOOT: f32 = 10.0;

/// The fastest a text selection scrolls the field, in points per reference
/// frame: about fifty characters a second, reached sixty points past the edge.
/// A field is one line, and a path read at that speed is still a path being
/// read.
pub const FIELD_SCROLL_MAX: f32 = 6.0;

/// How far a text selection dragged past the prompt field's edge scrolls the
/// field this frame, in points — negative towards the start of the line, zero
/// while the pointer is level with the field.
///
/// The band's arithmetic turned on its side: only the pointer's *x* is read,
/// the rate is proportional to the overshoot up to a ceiling, and a frame
/// takes its share of it by how long it lasted ([`band_scroll_rows`]), so a
/// fast display scrolls no faster than a slow one and the first frame after
/// a rest is not paid the rest.
pub fn field_scroll(field: egui::Rect, x: f32, dt: Duration) -> f32 {
    let overshoot = if x < field.left() {
        x - field.left()
    } else if x > field.right() {
        x - field.right()
    } else {
        0.0
    };
    let rate = (overshoot / FIELD_SCROLL_OVERSHOOT).clamp(-FIELD_SCROLL_MAX, FIELD_SCROLL_MAX);
    band_scroll_rows(rate, dt)
}

/// What a wheel event's delta is measured in — egui's `MouseWheelUnit`,
/// mirrored so the arithmetic here stays testable without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelUnit {
    /// A trackpad, reporting real travel.
    Point,
    /// A notched wheel, reporting lines.
    Line,
    /// A device (or a compositor) reporting screenfuls.
    Page,
}

/// What one *line* of wheel is worth, in logical points.
///
/// Fifty, which is egui's own `points_per_scroll_line` and the number every
/// toolkit has converged on. At [`crate::ui::ROW_HEIGHT`] that is a shade over
/// two rows per notch — the familiar three-line scroll, in a list whose rows are
/// taller than a line of text.
pub const POINTS_PER_LINE: f32 = 50.0;

/// …and one *page*. A screenful is a property of the window, which this
/// function does not have, so it is the largest travel a single event may claim
/// — and [`WHEEL_MAX_ROWS`] clamps it to something a hand can follow anyway.
const POINTS_PER_PAGE: f32 = 400.0;

/// One wheel event in logical points.
pub fn wheel_points(unit: WheelUnit, delta_y: f32) -> f32 {
    delta_y
        * match unit {
            WheelUnit::Point => 1.0,
            WheelUnit::Line => POINTS_PER_LINE,
            WheelUnit::Page => POINTS_PER_PAGE,
        }
}

/// A wheel delta in logical points, as a signed number of rows.
///
/// egui reports positive `y` for "content moves down", which is the view moving
/// *up* the listing — hence the sign flip. The clamp is [`WHEEL_MAX_ROWS`].
pub fn wheel_rows(points: f32, row_height: f32) -> f32 {
    if row_height <= 0.0 {
        return 0.0;
    }
    (-points / row_height).clamp(-WHEEL_MAX_ROWS, WHEEL_MAX_ROWS)
}

/// One eased coast toward a target, retargeted in flight.
///
/// The preview pane's momentum. The listing panes do not need one — their
/// scroll position is *already* a [`Tween`] (see [`crate::tab::Listing`]) and a
/// wheel roll simply retargets it over [`WHEEL_GLIDE`] instead of the keyboard's
/// shorter step — but a document's scroll is a plain line number, so the
/// animation has to be held somewhere, and this is it.
#[derive(Debug, Clone, Copy)]
pub struct Fling {
    tween: Tween,
    /// Where it is heading, kept separately so a retarget can be measured from
    /// the target rather than from the sampled position — otherwise a fast roll
    /// would lose ground on every notch.
    target: f32,
    /// Sub-line travel that has not earned a whole line yet. Without it a
    /// trackpad's small deltas would each round to zero and the pane would not
    /// move at all.
    carry: f32,
}

impl Fling {
    /// A coast that is already over, parked at `at`.
    pub fn at(at: f32, now: Instant) -> Fling {
        Fling {
            tween: Tween::new(at, at, Duration::ZERO, Easing::Linear, now),
            target: at,
            carry: 0.0,
        }
    }

    /// Add `delta` to the target and re-launch the coast from wherever the
    /// value has got to. Returns whether the target actually moved.
    pub fn kick(&mut self, delta: f32, min: f32, max: f32, now: Instant) -> bool {
        self.carry += delta;
        let whole = self.carry.trunc();
        self.carry -= whole;
        if whole == 0.0 {
            return false;
        }
        let target = (self.target + whole).clamp(min, max);
        if (target - self.target).abs() < f32::EPSILON {
            return false;
        }
        let from = self.tween.value(now);
        self.target = target;
        self.tween = Tween::new(from, target, WHEEL_GLIDE, Easing::OutQuint, now);
        true
    }

    /// Where the view is now.
    pub fn value(&self, now: Instant) -> f32 {
        self.tween.value(now)
    }

    /// Where it is heading. Read by the tests that pin the retarget contract —
    /// "a second notch adds to the target and starts from where the view is" is
    /// the whole of the momentum rule, and it cannot be checked from `value`
    /// alone.
    #[allow(dead_code)]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Has it arrived? A finished coast must stop asking for frames.
    pub fn finished(&self, now: Instant) -> bool {
        self.tween.finished(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Ctl {
        Row(usize),
        /// A text field, which counts to three.
        Field,
    }

    const H: f32 = 22.0;

    fn content() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(400.0, 220.0))
    }

    /// A band drawn with the list held still: both corners placed at `scroll`.
    fn still(
        c: egui::Rect,
        scroll: f32,
        rows: usize,
        from: egui::Pos2,
        to: egui::Pos2,
    ) -> Option<(usize, usize)> {
        let corner = |at| Corner { at, scroll };
        band_rows(c, scroll, rows, H, corner(from), corner(to))
    }

    /// The timing window, both sides of it.
    #[test]
    fn two_quick_clicks_on_one_row_are_a_double_click() {
        let mut clicks = Clicks::new();
        let t0 = Instant::now();
        let at = egui::pos2(10.0, 10.0);
        assert!(!clicks.press(Ctl::Row(3), at, t0));
        assert!(clicks.press(Ctl::Row(3), at, t0 + Duration::from_millis(120)));

        // …and one that is a whisker too slow is two single clicks.
        let mut clicks = Clicks::new();
        assert!(!clicks.press(Ctl::Row(3), at, t0));
        assert!(!clicks.press(
            Ctl::Row(3),
            at,
            t0 + DOUBLE_CLICK_WINDOW + Duration::from_millis(1)
        ));
    }

    /// Presses pair off: three clicks are one open, not two.
    #[test]
    fn a_third_click_starts_a_new_pair() {
        let mut clicks = Clicks::new();
        let t0 = Instant::now();
        let at = egui::pos2(10.0, 10.0);
        assert!(!clicks.press(Ctl::Row(1), at, t0));
        assert!(clicks.press(Ctl::Row(1), at, t0 + Duration::from_millis(80)));
        assert!(!clicks.press(Ctl::Row(1), at, t0 + Duration::from_millis(160)));
        assert!(clicks.press(Ctl::Row(1), at, t0 + Duration::from_millis(240)));
        // …and a pair is still timed from its own first click: a fifth press
        // that comes too late after the fourth is a single, and the one after
        // it the double.
        let late = t0 + Duration::from_millis(240) + DOUBLE_CLICK_WINDOW;
        assert!(!clicks.press(Ctl::Row(1), at, late + Duration::from_millis(1)));
        assert!(clicks.press(Ctl::Row(1), at, late + Duration::from_millis(80)));
    }

    /// The text field's count: one, two, three, and then three again — a run
    /// that goes on keeps the line rather than wrapping back to a caret.
    #[test]
    fn a_text_field_counts_up_to_a_triple_click() {
        let mut clicks = Clicks::new();
        let t0 = Instant::now();
        let at = egui::pos2(40.0, 10.0);
        let step = |n: u64| t0 + Duration::from_millis(n * 100);
        assert_eq!(clicks.count(Ctl::Field, at, step(0)), 1);
        assert_eq!(clicks.count(Ctl::Field, at, step(1)), 2);
        assert_eq!(clicks.count(Ctl::Field, at, step(2)), 3);
        assert_eq!(clicks.count(Ctl::Field, at, step(3)), 3);

        // A pause, a drift past the slop or another control each start over.
        let mut clicks = Clicks::new();
        assert_eq!(clicks.count(Ctl::Field, at, t0), 1);
        assert_eq!(clicks.count(Ctl::Field, at, step(1)), 2);
        let slow = step(1) + DOUBLE_CLICK_WINDOW + Duration::from_millis(1);
        assert_eq!(clicks.count(Ctl::Field, at, slow), 1);
        let away = at + egui::vec2(DOUBLE_CLICK_SLOP + 1.0, 0.0);
        assert_eq!(
            clicks.count(Ctl::Field, away, slow + Duration::from_millis(50)),
            1
        );
        assert_eq!(
            clicks.count(Ctl::Row(0), away, slow + Duration::from_millis(100)),
            1
        );

        // …and the two readings share one history: a double click on a row
        // followed by a count on it is the third of the run.
        let mut clicks = Clicks::new();
        assert!(!clicks.press(Ctl::Row(2), at, t0));
        assert!(clicks.press(Ctl::Row(2), at, step(1)));
        assert_eq!(clicks.count(Ctl::Row(2), at, step(2)), 3);
    }

    /// Two clicks on two different rows are two clicks, however fast.
    #[test]
    fn a_double_click_has_to_land_on_the_same_thing_twice() {
        let mut clicks = Clicks::new();
        let t0 = Instant::now();
        assert!(!clicks.press(Ctl::Row(1), egui::pos2(10.0, 10.0), t0));
        assert!(!clicks.press(
            Ctl::Row(2),
            egui::pos2(10.0, 12.0),
            t0 + Duration::from_millis(50)
        ));
        // …and neither is a pair that drifted across the slop.
        let mut clicks = Clicks::new();
        assert!(!clicks.press(Ctl::Row(1), egui::pos2(10.0, 10.0), t0));
        assert!(!clicks.press(
            Ctl::Row(1),
            egui::pos2(10.0 + DOUBLE_CLICK_SLOP + 1.0, 10.0),
            t0 + Duration::from_millis(50)
        ));
    }

    /// The band takes every row it touches, in either drag direction.
    #[test]
    fn a_band_encloses_every_row_it_overlaps() {
        let c = content();
        // Rows 0..9 are at 100, 122, 144 … A band over the middle of rows 2–4.
        let from = egui::pos2(20.0, c.top() + 2.0 * H + 5.0);
        let to = egui::pos2(200.0, c.top() + 4.0 * H + 5.0);
        assert_eq!(still(c, 0.0, 40, from, to), Some((2, 4)));
        // Dragging upwards is the same band.
        assert_eq!(still(c, 0.0, 40, to, from), Some((2, 4)));
        // A band that has not left its row yet is a run of one…
        let tiny = from + egui::vec2(1.0, 1.0);
        assert_eq!(still(c, 0.0, 40, from, tiny), Some((2, 2)));
        // …and so is one dragged straight across it, with no height at all.
        let across = from + egui::vec2(120.0, 0.0);
        assert_eq!(still(c, 0.0, 40, from, across), Some((2, 2)));
        // The bottom edge is exclusive: a band ending exactly on the top of
        // row 5 has not reached it.
        let edge = egui::pos2(200.0, c.top() + 5.0 * H);
        assert_eq!(still(c, 0.0, 40, from, edge), Some((2, 4)));
    }

    /// Scrolling moves which rows a band at a fixed place on screen encloses.
    #[test]
    fn the_band_reads_through_the_scroll_position() {
        let c = content();
        let from = egui::pos2(20.0, c.top() + 1.0);
        let to = egui::pos2(20.0, c.top() + 2.0 * H + 1.0);
        assert_eq!(still(c, 0.0, 40, from, to), Some((0, 2)));
        assert_eq!(still(c, 7.0, 40, from, to), Some((7, 9)));
    }

    /// A drag that leaves the pane selects to the end of the listing and stops
    /// — not to row four thousand, and not to nothing.
    #[test]
    fn a_band_is_clipped_to_the_pane_and_to_the_listing() {
        let c = content();
        let miles = (
            egui::pos2(20.0, c.top() - 5_000.0),
            egui::pos2(20.0, c.bottom() + 5_000.0),
        );
        assert_eq!(still(c, 0.0, 4, miles.0, miles.1), Some((0, 3)));
        // A short listing under a band that is entirely below it: nothing.
        let below = (
            egui::pos2(20.0, c.top() + 8.0 * H),
            egui::pos2(20.0, c.top() + 9.0 * H),
        );
        assert_eq!(still(c, 0.0, 3, below.0, below.1), None);
        // …and one entirely above the pane.
        let above = (
            egui::pos2(20.0, c.top() - 90.0),
            egui::pos2(20.0, c.top() - 50.0),
        );
        assert_eq!(still(c, 0.0, 30, above.0, above.1), None);
        assert_eq!(still(c, 0.0, 0, miles.0, miles.1), None);
    }

    /// **The origin rides with the rows.** A band started beside row 2 and
    /// pulled off the bottom of the pane scrolls the list; thirty rows later
    /// the origin is far above the pane, and every row from 2 down to the last
    /// one on screen is in the band — not just the screenful that is visible.
    #[test]
    fn a_band_that_scrolled_keeps_the_rows_it_scrolled_past() {
        let c = content();
        let origin = Corner {
            at: egui::pos2(20.0, c.top() + 2.0 * H + 5.0),
            scroll: 0.0,
        };
        let pointer = |scroll| Corner {
            at: egui::pos2(20.0, c.bottom() + 40.0),
            scroll,
        };
        assert_eq!(
            band_rows(c, 0.0, 100, H, origin, pointer(0.0)),
            Some((2, 9))
        );
        // Thirty rows on: rows 30–39 on screen, and 2–39 in the band.
        assert_eq!(
            band_rows(c, 30.0, 100, H, origin, pointer(30.0)),
            Some((2, 39))
        );
        // …and back up past the origin, the band is the other side of it:
        // from the first row on screen down to the origin's row.
        let up = Corner {
            at: egui::pos2(20.0, c.top() - 40.0),
            scroll: 0.0,
        };
        let from_below = Corner {
            at: origin.at,
            scroll: 6.0,
        };
        assert_eq!(band_rows(c, 0.0, 100, H, from_below, up), Some((0, 8)));
    }

    /// An origin clipped to the pane's edge at the press stays on that row
    /// boundary as the list scrolls — a band from the space under a short
    /// parent column, pulled off the bottom, takes exactly the rows that
    /// scroll up into view, and nothing before the scroll starts.
    #[test]
    fn an_origin_below_the_list_takes_the_rows_that_scroll_into_view() {
        let c = content();
        let origin = Corner {
            at: egui::pos2(c.left() - 60.0, c.bottom() + 4.0),
            scroll: 0.0,
        };
        let pointer = |scroll| Corner {
            at: egui::pos2(c.left() + 100.0, c.bottom() + 30.0),
            scroll,
        };
        assert_eq!(band_rows(c, 0.0, 100, H, origin, pointer(0.0)), None);
        // Three rows scrolled: rows 10–12 came up from under the pane.
        assert_eq!(
            band_rows(c, 3.0, 100, H, origin, pointer(3.0)),
            Some((10, 12))
        );
    }

    /// A band started in the parent column, to the left of the list, takes
    /// nothing until its rectangle reaches the list — and then the rows it
    /// spans, exactly as if it had started at the list's edge.
    #[test]
    fn a_band_from_left_of_the_list_takes_rows_once_it_reaches_them() {
        let c = content();
        let from = egui::pos2(c.left() - 150.0, c.top() + H + 5.0);
        // Still inside the parent column: no row, however many it spans.
        let short = egui::pos2(c.left() - 20.0, c.top() + 3.0 * H + 5.0);
        assert_eq!(still(c, 0.0, 40, from, short), None);
        // Into the list: rows 1–3.
        let into = egui::pos2(c.left() + 50.0, c.top() + 3.0 * H + 5.0);
        assert_eq!(still(c, 0.0, 40, from, into), Some((1, 3)));
        // …and it reads through the scroll like any other band.
        assert_eq!(still(c, 5.0, 40, from, into), Some((6, 8)));
    }

    /// A band started below the list's content — the empty space under a short
    /// parent column is level with it — is clipped to the pane's bottom edge,
    /// and takes the rows from the pointer down to the last one on screen. Not
    /// the row after it, whose top the clip only touches.
    #[test]
    fn a_band_from_below_the_list_stops_at_the_last_row_on_screen() {
        let c = content();
        // 220 points of 22-point rows: rows 0–9 on screen, row 10 just under.
        let from = egui::pos2(c.left() - 60.0, c.bottom() + 30.0);
        let to = egui::pos2(c.left() + 200.0, c.top() + 7.0 * H + 5.0);
        assert_eq!(still(c, 0.0, 40, from, to), Some((7, 9)));
        // Half a row scrolled, row 10 *is* partly on screen, and is taken.
        let to = egui::pos2(c.left() + 200.0, c.top() + 6.5 * H + 5.0);
        assert_eq!(still(c, 0.5, 40, from, to), Some((7, 10)));
        // A band that stays below the list takes nothing.
        let under = egui::pos2(c.left() + 200.0, c.bottom() + 5.0);
        assert_eq!(still(c, 0.0, 40, from, under), None);
        // …and the same clip at the top: a band from above the pane stops at
        // the first row on screen, not the one scrolled off above it.
        let above = egui::pos2(c.left() + 20.0, c.top() - 40.0);
        let to = egui::pos2(c.left() + 20.0, c.top() + 2.0 * H + 5.0);
        assert_eq!(still(c, 3.0, 40, above, to), Some((3, 5)));
    }

    /// A band started in the preview pane, to the right of the list, is the
    /// mirror of one from the parent column.
    #[test]
    fn a_band_from_the_preview_pane_takes_rows_once_it_reaches_them() {
        let c = content();
        let from = egui::pos2(c.right() + 200.0, c.top() + 3.0 * H + 5.0);
        let still_right = egui::pos2(c.right() + 10.0, c.top() + 6.0 * H + 5.0);
        assert_eq!(still(c, 0.0, 40, from, still_right), None);
        let into = egui::pos2(c.right() - 20.0, c.top() + 6.0 * H + 5.0);
        assert_eq!(still(c, 0.0, 40, from, into), Some((3, 6)));
        // Dragged up past the pane's top and back into the list horizontally:
        // everything from the first row on screen down to the origin's row.
        let up = egui::pos2(c.right() - 20.0, c.top() - 90.0);
        assert_eq!(still(c, 0.0, 40, from, up), Some((0, 3)));
    }

    /// Level with the pane, no scroll; past its edges, a rate proportional to
    /// the overshoot and signed towards the pointer; and a ceiling.
    #[test]
    fn a_band_over_the_edge_scrolls_in_proportion_to_the_overshoot() {
        let pane = content();
        for y in [pane.top(), pane.center().y, pane.bottom()] {
            assert_eq!(band_scroll(pane, y), 0.0, "at {y}");
        }
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        // One row a frame at twenty points out, either way.
        assert!(near(band_scroll(pane, pane.bottom() + 20.0), 1.0));
        assert!(near(band_scroll(pane, pane.top() - 20.0), -1.0));
        // Proportional, not stepped: half the overshoot, half the rate.
        assert!(near(band_scroll(pane, pane.bottom() + 10.0), 0.5));
        assert!(near(band_scroll(pane, pane.bottom() + 60.0), 3.0));
        // …up to the ceiling, however far the hand goes.
        assert_eq!(band_scroll(pane, pane.bottom() + 5_000.0), BAND_SCROLL_MAX);
        assert_eq!(band_scroll(pane, pane.top() - 5_000.0), -BAND_SCROLL_MAX);
    }

    /// A text selection dragged past the field's edge scrolls it that way, in
    /// proportion to how far out the hand is, up to a ceiling, paced by the
    /// frame; level with the field it does not scroll at all.
    #[test]
    fn a_text_drag_past_the_field_scrolls_it_towards_the_pointer() {
        let field = egui::Rect::from_min_max(egui::pos2(100.0, 10.0), egui::pos2(300.0, 30.0));
        let frame = Duration::from_secs_f32(BAND_SCROLL_FRAME);
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        for x in [field.left(), field.center().x, field.right()] {
            assert_eq!(field_scroll(field, x, frame), 0.0, "at {x}");
        }
        assert!(near(field_scroll(field, field.right() + 10.0, frame), 1.0));
        assert!(near(field_scroll(field, field.left() - 10.0, frame), -1.0));
        assert!(near(field_scroll(field, field.right() + 30.0, frame), 3.0));
        assert!(near(
            field_scroll(field, field.right() + 5_000.0, frame),
            FIELD_SCROLL_MAX
        ));
        // Two frames' time is two frames' travel; a long rest is capped.
        assert!(near(
            field_scroll(field, field.right() + 10.0, frame * 2),
            2.0
        ));
        assert!(near(
            field_scroll(field, field.right() + 10.0, Duration::from_secs(3)),
            3.0
        ));
    }

    /// The rate is per *reference* frame: a real frame takes its share by how
    /// long it lasted, and a frame after a long rest is not paid the rest.
    #[test]
    fn a_band_scroll_is_paced_by_the_frame_not_the_refresh_rate() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let frame = Duration::from_secs_f32(BAND_SCROLL_FRAME);
        assert!(near(band_scroll_rows(1.0, frame), 1.0));
        assert!(near(band_scroll_rows(-2.0, frame), -2.0));
        // A 144 Hz frame is a smaller step, so a second of it travels the same.
        let fast = Duration::from_secs_f32(1.0 / 144.0);
        assert!(near(band_scroll_rows(1.0, fast) * 144.0, 60.0));
        // The first frame after the loop went idle is capped at three frames.
        assert!(near(band_scroll_rows(1.0, Duration::from_secs(2)), 3.0));
        assert_eq!(band_scroll_rows(0.0, frame), 0.0);
    }

    /// The wheel's sign and its clamp.
    #[test]
    fn a_wheel_roll_becomes_a_clamped_number_of_rows() {
        // egui's positive y is "content down", which is the view going up.
        assert!(wheel_rows(50.0, H) < 0.0);
        assert!(wheel_rows(-50.0, H) > 0.0);
        assert!((wheel_rows(-H * 3.0, H) - 3.0).abs() < 1e-4);
        // A page-unit event does not throw the list a screenful.
        assert_eq!(wheel_rows(-10_000.0, H), WHEEL_MAX_ROWS);
        assert_eq!(wheel_rows(10_000.0, H), -WHEEL_MAX_ROWS);
        assert_eq!(wheel_rows(100.0, 0.0), 0.0);
    }

    /// The momentum contract: one tween, retargeted from where it has got to,
    /// clamped to the content, and *finished* when it arrives.
    #[test]
    fn a_fling_coasts_to_its_target_and_stops() {
        let t0 = Instant::now();
        let mut fling = Fling::at(0.0, t0);
        assert!(fling.finished(t0));
        assert!(fling.kick(10.0, 0.0, 100.0, t0));
        assert_eq!(fling.target(), 10.0);
        assert_eq!(fling.value(t0), 0.0);
        assert!(!fling.finished(t0));
        let mid = fling.value(t0 + Duration::from_millis(60));
        assert!(mid > 0.0 && mid < 10.0, "got {mid}");
        assert!((fling.value(t0 + WHEEL_GLIDE) - 10.0).abs() < 1e-3);
        assert!(fling.finished(t0 + WHEEL_GLIDE));

        // A second notch mid-coast adds to the *target* and starts from where
        // the rows are — no jump backwards, and no ground lost.
        let at = t0 + Duration::from_millis(60);
        let mut fling = Fling::at(0.0, t0);
        fling.kick(10.0, 0.0, 100.0, t0);
        let seen = fling.value(at);
        fling.kick(10.0, 0.0, 100.0, at);
        assert_eq!(fling.value(at), seen, "the view must not jump on retarget");
        assert_eq!(fling.target(), 20.0);
    }

    /// The clamp is on the target, so a roll past the end coasts to the end
    /// rather than storing travel it will have to give back.
    #[test]
    fn a_fling_never_targets_past_the_content() {
        let t0 = Instant::now();
        let mut fling = Fling::at(0.0, t0);
        for _ in 0..20 {
            fling.kick(5.0, 0.0, 12.0, t0);
        }
        assert_eq!(fling.target(), 12.0);
        for _ in 0..40 {
            fling.kick(-5.0, 0.0, 12.0, t0);
        }
        assert_eq!(fling.target(), 0.0);
    }

    /// Sub-line deltas accumulate rather than rounding to nothing — a trackpad
    /// has to move the pane.
    #[test]
    fn small_deltas_add_up_instead_of_vanishing() {
        let t0 = Instant::now();
        let mut fling = Fling::at(0.0, t0);
        assert!(!fling.kick(0.3, 0.0, 100.0, t0));
        assert!(!fling.kick(0.3, 0.0, 100.0, t0));
        assert!(fling.kick(0.5, 0.0, 100.0, t0));
        assert_eq!(fling.target(), 1.0);
    }
}
