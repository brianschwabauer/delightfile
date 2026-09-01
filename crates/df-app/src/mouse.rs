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
/// a double click.
///
/// Keyed by control rather than by position alone: two clicks 4 px apart that
/// happen to straddle a row boundary are two clicks on two rows, and opening
/// the second one because the first was nearby would be the pointer acting on
/// something the user never pressed.
pub struct Clicks<K: Key> {
    last: Option<(K, egui::Pos2, Instant)>,
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
    /// A double click **consumes** its history, so a third click starts a fresh
    /// pair rather than firing "open" again on every click of a drum roll.
    pub fn press(&mut self, key: K, at: egui::Pos2, now: Instant) -> bool {
        let double = self.last.is_some_and(|(last_key, last_at, when)| {
            last_key == key
                && (at - last_at).length() <= DOUBLE_CLICK_SLOP
                && now.saturating_duration_since(when) <= DOUBLE_CLICK_WINDOW
        });
        self.last = if double { None } else { Some((key, at, now)) };
        double
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

/// Which rows a band encloses, as an inclusive `(first, last)` — or `None` when
/// it encloses none.
///
/// A row counts as enclosed when the band overlaps its strip at all, which is
/// what makes the selection follow the pointer *as it moves* rather than only
/// once a whole row is covered. The band is clipped to the pane's content box
/// first, so a drag that leaves the window at speed selects to the edge of the
/// listing and stops, rather than resolving to row four thousand.
pub fn band_rows(
    content: egui::Rect,
    scroll_rows: f32,
    rows: usize,
    row_height: f32,
    band: egui::Rect,
) -> Option<(usize, usize)> {
    if rows == 0 || row_height <= 0.0 {
        return None;
    }
    let top = band.top().max(content.top());
    let bottom = band.bottom().min(content.bottom());
    if bottom < top {
        return None;
    }
    // The band's edges in *row* coordinates: how many rows down the listing
    // each one falls, counting from the row the view is scrolled to.
    let first = ((top - content.top()) / row_height + scroll_rows).floor();
    let last = ((bottom - content.top()) / row_height + scroll_rows).floor();
    if last < 0.0 || first > (rows - 1) as f32 {
        return None;
    }
    let first = first.max(0.0) as usize;
    let last = (last.max(0.0) as usize).min(rows - 1);
    Some((first, last.max(first)))
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
const POINTS_PER_LINE: f32 = 50.0;

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
    }

    const H: f32 = 22.0;

    fn content() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(400.0, 220.0))
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

    /// A double click consumes its history: three clicks are one open, not two.
    #[test]
    fn a_third_click_starts_a_new_pair() {
        let mut clicks = Clicks::new();
        let t0 = Instant::now();
        let at = egui::pos2(10.0, 10.0);
        assert!(!clicks.press(Ctl::Row(1), at, t0));
        assert!(clicks.press(Ctl::Row(1), at, t0 + Duration::from_millis(80)));
        assert!(!clicks.press(Ctl::Row(1), at, t0 + Duration::from_millis(160)));
        assert!(clicks.press(Ctl::Row(1), at, t0 + Duration::from_millis(240)));
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
        assert_eq!(band_rows(c, 0.0, 40, H, band(from, to)), Some((2, 4)));
        // Dragging upwards is the same band.
        assert_eq!(band_rows(c, 0.0, 40, H, band(to, from)), Some((2, 4)));
        // A band that has not left its row yet is a run of one.
        let tiny = band(from, from + egui::vec2(1.0, 1.0));
        assert_eq!(band_rows(c, 0.0, 40, H, tiny), Some((2, 2)));
    }

    /// Scrolling moves which rows a band at a fixed place on screen encloses.
    #[test]
    fn the_band_reads_through_the_scroll_position() {
        let c = content();
        let from = egui::pos2(20.0, c.top() + 1.0);
        let to = egui::pos2(20.0, c.top() + 2.0 * H + 1.0);
        assert_eq!(band_rows(c, 0.0, 40, H, band(from, to)), Some((0, 2)));
        assert_eq!(band_rows(c, 7.0, 40, H, band(from, to)), Some((7, 9)));
    }

    /// A drag that leaves the pane selects to the end of the listing and stops
    /// — not to row four thousand, and not to nothing.
    #[test]
    fn a_band_is_clipped_to_the_pane_and_to_the_listing() {
        let c = content();
        let miles = band(
            egui::pos2(20.0, c.top() - 5_000.0),
            egui::pos2(20.0, c.bottom() + 5_000.0),
        );
        assert_eq!(band_rows(c, 0.0, 4, H, miles), Some((0, 3)));
        // A short listing under a band that is entirely below it: nothing.
        let below = band(
            egui::pos2(20.0, c.top() + 8.0 * H),
            egui::pos2(20.0, c.top() + 9.0 * H),
        );
        assert_eq!(band_rows(c, 0.0, 3, H, below), None);
        // …and one entirely above the pane.
        let above = band(
            egui::pos2(20.0, c.top() - 90.0),
            egui::pos2(20.0, c.top() - 50.0),
        );
        assert_eq!(band_rows(c, 0.0, 30, H, above), None);
        assert_eq!(band_rows(c, 0.0, 0, H, miles), None);
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
