//! Hover and press states for the hand-painted chrome — "instant in, animated
//! out" (PLAN §8, `delightful-ui` §3/§4), implemented for a painter rather than
//! for CSS. Ported from delightviewer's `ui/hover.rs`.
//!
//! On the web those two rules are a `transition` on the base rule and an
//! override inside `:hover`. There is no cascade here: every control in
//! delightfile is a rectangle somebody drew, so the fade has to be a number
//! that lives between frames. That is all this is — one 0…1 amount per control,
//! snapped to 1 the moment the pointer is over it and eased back to 0 over
//! [`FADE`] once it is not, so a quick sweep down a file list leaves the trail
//! behind it that makes the whole column feel responsive.
//!
//! The press amount works the same way and for the same reason: a click is over
//! in 40 ms, and a depress that vanished with it would never be seen. It snaps
//! in on mouse-down and eases out over [`PRESS_FADE`] — the "release" half of
//! delightstack's `:active` transition.
// Phase 0 ports the whole vocabulary; most of its callers arrive with the panes
// it exists for (PLAN §10). Kept whole and allowed here rather than trimmed to
// what today's placeholder happens to touch — deleting half a motion system and
// re-porting it in Phase 4 is exactly how constants drift away from the
// originals they were checked against.
#![allow(dead_code)]

use std::time::Instant;

/// What a control has to be for [`Hovers`] to track it: nothing but comparable
/// and cheap to copy. A toolbar's key is its button id; a file list's is the row
/// index, because a row has no identity beyond where it is in the list.
pub trait Key: Copy + PartialEq {}
impl<T: Copy + PartialEq> Key for T {}

/// Hover fade-out, in seconds. delightstack's 300 ms colour transition rounded
/// to the length that still reads as a trail rather than as lag (PLAN §8).
pub const FADE: f32 = 0.24;

/// Press release, in seconds. Quicker than [`FADE`]: the control is coming
/// *back*, and a slow return reads as a button that is stuck rather than one
/// that is springing.
pub const PRESS_FADE: f32 = 0.16;

/// The longest step a single frame may take, in seconds. A frame that arrives
/// after a long sleep — the window was occluded, the compositor throttled us —
/// must not be one enormous step: a fade that has been off screen for a second
/// is simply over, and interpolating it in one jump is the same answer with a
/// worse worst case.
const MAX_DT: f32 = 0.25;

/// Below this an amount is zero. It stops the map growing without bound and,
/// more importantly, stops the app repainting forever for a fade nobody can
/// see (PLAN §1: idle cost is a design constraint).
const EPSILON: f32 = 0.004;

/// Per-control hover and press amounts.
pub struct Hovers<K: Key> {
    /// `(control, hover amount, press amount)`. A `Vec` rather than a map: a
    /// pane has a couple of dozen live controls and at most a handful of them
    /// are ever warm at once, so a linear scan over a warm cache beats hashing.
    items: Vec<(K, f32, f32)>,
    last: Option<Instant>,
    /// What the last [`tick`](Hovers::tick) was told is under the pointer and
    /// under the button. Kept only so [`animating`](Hovers::animating) can tell
    /// a fade that is still running from an amount that has arrived and is
    /// being *held* there — a pointer resting on a row is not an animation, and
    /// asking for 60 frames a second to redraw the identical highlight is how
    /// an idle file manager ends up costing a battery.
    hot: Option<K>,
    pressed: Option<K>,
}

// Derived `Default` would demand `K: Default`, which a key never needs to be.
impl<K: Key> Default for Hovers<K> {
    fn default() -> Hovers<K> {
        Hovers {
            items: Vec::new(),
            last: None,
            hot: None,
            pressed: None,
        }
    }
}

impl<K: Key> Hovers<K> {
    pub fn new() -> Hovers<K> {
        Hovers::default()
    }

    /// Advance every amount one frame: `hot` is snapped to 1, `pressed` is
    /// snapped to 1, and everything else decays.
    pub fn tick(&mut self, hot: Option<K>, pressed: Option<K>, now: Instant) {
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(MAX_DT);
        self.last = Some(now);
        self.hot = hot;
        self.pressed = pressed;
        for (hit, hover, press) in &mut self.items {
            if Some(*hit) != hot {
                *hover = (*hover - dt / FADE).max(0.0);
            }
            if Some(*hit) != pressed {
                *press = (*press - dt / PRESS_FADE).max(0.0);
            }
        }
        for (hit, which) in [(hot, true), (pressed, false)] {
            let Some(hit) = hit else { continue };
            match self.items.iter_mut().find(|(h, _, _)| *h == hit) {
                Some((_, hover, press)) => {
                    // Instant in — the whole point of the rule.
                    if which {
                        *hover = 1.0;
                    } else {
                        *press = 1.0;
                    }
                }
                None => self.items.push((
                    hit,
                    if which { 1.0 } else { 0.0 },
                    if which { 0.0 } else { 1.0 },
                )),
            }
        }
        self.items
            .retain(|(_, hover, press)| *hover > EPSILON || *press > EPSILON);
    }

    pub fn hover(&self, hit: K) -> f32 {
        self.amount(hit).0
    }

    pub fn press(&self, hit: K) -> f32 {
        self.amount(hit).1
    }

    fn amount(&self, hit: K) -> (f32, f32) {
        self.items
            .iter()
            .find(|(h, _, _)| *h == hit)
            .map(|(_, hover, press)| (*hover, *press))
            .unwrap_or((0.0, 0.0))
    }

    /// The warmest control and how warm it is — what a tooltip hangs off.
    ///
    /// Reading it back out of the fade rather than taking "what is hovered
    /// *now*" is what makes the tooltip leave the way it arrived: it belongs to
    /// the control whose hover is still on its way down, and it dims with it.
    pub fn hottest(&self) -> Option<(K, f32)> {
        self.items
            .iter()
            .filter(|(_, hover, _)| *hover > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(hit, hover, _)| (*hit, *hover))
    }

    /// Is anything still *moving*? The app repaints on events (PLAN §1), so a
    /// fade nobody asks to be drawn would freeze halfway.
    ///
    /// "Moving" is the whole point of the question. The in-half of the rule is
    /// a snap, so the only amount that ever changes between frames is one on
    /// its way *down*: an amount whose control is no longer hot, or no longer
    /// held, and has not yet reached zero (a finished fade is dropped by
    /// `tick`'s retain). A pointer parked on a row holds a 1.0 that will be
    /// exactly 1.0 next frame too — nothing to draw, so nothing to ask for.
    pub fn animating(&self) -> bool {
        self.items.iter().any(|(hit, hover, press)| {
            (*hover > 0.0 && Some(*hit) != self.hot) || (*press > 0.0 && Some(*hit) != self.pressed)
        })
    }
}

/// How far a fully pressed control's edges move, in logical pixels. Small
/// enough that a 30 px row does not visibly change size, large enough to read
/// as travel at arm's length on a 27" display.
const PRESS_SQUEEZE: f32 = 1.5;

/// The vertical share of that squeeze. Controls are wider than they are tall,
/// so an equal squeeze on both axes eats proportionally more of the height and
/// the control reads as flattening; four-fifths keeps it square-ish.
const PRESS_SQUEEZE_Y: f32 = 0.8;

/// How far a fully pressed control sinks, in logical pixels — the painter's
/// half of delightstack's `translate: 0 2px`, minus a pixel because the squeeze
/// is already carrying part of the depth cue.
const PRESS_SINK: f32 = 1.0;

/// A pressed control's rectangle: shrunk and nudged down, which is the
/// painter's version of delightstack's `scale: 0.98; translate: 0 2px`.
///
/// Shrunk by a fixed number of *pixels* rather than by a ratio, for the reason
/// delightstack's `ListItem.svelte` gives: a uniform scale on a wide control
/// moves its edges much further than it moves a narrow one's, and the wide one
/// then reads as collapsing rather than as being pressed.
pub fn pressed_rect(r: egui::Rect, press: f32) -> egui::Rect {
    if press <= 0.0 {
        return r;
    }
    let squeeze = PRESS_SQUEEZE * press;
    egui::Rect::from_min_max(
        egui::pos2(r.left() + squeeze, r.top() + squeeze * PRESS_SQUEEZE_Y),
        egui::pos2(r.right() - squeeze, r.bottom() - squeeze * PRESS_SQUEEZE_Y),
    )
    .translate(egui::vec2(0.0, press * PRESS_SINK))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Stand-in for the real controls Phase 1 brings (rows, tabs, breadcrumb
    /// segments). Any `Copy + PartialEq` will do — that is the point of [`Key`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Ctl {
        Row(usize),
        Save,
    }

    /// The rule, as a test: hover arrives in one frame and leaves over a
    /// quarter of a second.
    #[test]
    fn hover_snaps_in_and_eases_out() {
        let mut h = Hovers::new();
        let t0 = Instant::now();
        let hit = Ctl::Row(3);
        h.tick(Some(hit), None, t0);
        assert_eq!(h.hover(hit), 1.0);
        // Still fully on a frame later — a pointer that has not moved has not
        // left.
        h.tick(Some(hit), None, t0 + Duration::from_millis(16));
        assert_eq!(h.hover(hit), 1.0);
        // …and a pointer that has not moved is not an animation: a highlight
        // held at 1.0 must not keep the repaint clock running.
        assert!(
            !h.animating(),
            "a hover parked under the pointer must not ask for frames"
        );

        h.tick(None, None, t0 + Duration::from_millis(16 + 120));
        let half = h.hover(hit);
        assert!(half > 0.3 && half < 0.7, "got {half}");
        assert!(h.animating());
        h.tick(None, None, t0 + Duration::from_millis(16 + 400));
        assert_eq!(h.hover(hit), 0.0);
        assert!(
            !h.animating(),
            "a finished fade must stop the repaint clock"
        );
    }

    /// A press outlives the click that made it, or it would never be seen.
    #[test]
    fn a_press_releases_over_its_own_fade() {
        let mut h = Hovers::new();
        let t0 = Instant::now();
        let hit = Ctl::Save;
        h.tick(Some(hit), Some(hit), t0);
        assert_eq!(h.press(hit), 1.0);
        // A button held down is as static as one merely hovered.
        assert!(!h.animating());
        h.tick(Some(hit), None, t0 + Duration::from_millis(40));
        assert!(h.press(hit) > 0.0 && h.press(hit) < 1.0);
        // …and the hover it is under is untouched by the release.
        assert_eq!(h.hover(hit), 1.0);
        // The release *is* moving, even though the pointer has not: the
        // depress is on its way back up under a hover that is staying put.
        assert!(h.animating());
        h.tick(Some(hit), None, t0 + Duration::from_millis(400));
        assert_eq!(h.press(hit), 0.0);
        assert!(!h.animating(), "a spent release stops the clock too");
    }

    /// A sweep across several rows leaves every one of them fading, and the
    /// warmest is the one the pointer left last — that is what a tooltip or a
    /// trailing highlight hangs off.
    #[test]
    fn a_sweep_leaves_a_trail_and_the_warmest_is_the_most_recent() {
        let mut h = Hovers::new();
        let t0 = Instant::now();
        for i in 0..4 {
            h.tick(
                Some(Ctl::Row(i)),
                None,
                t0 + Duration::from_millis(i as u64 * 30),
            );
        }
        assert_eq!(h.hottest(), Some((Ctl::Row(3), 1.0)));
        assert!(
            h.hover(Ctl::Row(0)) < h.hover(Ctl::Row(1)),
            "trail is ordered"
        );
        assert!(h.hover(Ctl::Row(2)) > 0.0);
        // Everything but the one under the pointer is on its way out.
        assert!(h.animating());
    }

    /// Epsilon pruning is what keeps a long session's map from being a log of
    /// every row ever hovered.
    #[test]
    fn spent_fades_are_dropped_not_kept_at_a_whisker_above_zero() {
        let mut h = Hovers::new();
        let t0 = Instant::now();
        for i in 0..50 {
            h.tick(
                Some(Ctl::Row(i)),
                None,
                t0 + Duration::from_millis(i as u64 * 40),
            );
        }
        h.tick(None, None, t0 + Duration::from_secs(5));
        assert!(h.items.is_empty(), "{} left behind", h.items.len());
        assert!(!h.animating());
    }

    /// A frame that arrives after a long sleep is one step, not one enormous
    /// step — and either way the fade is over, never overshot into a negative.
    #[test]
    fn a_late_frame_finishes_the_fade_rather_than_overshooting_it() {
        let mut h = Hovers::new();
        let t0 = Instant::now();
        h.tick(Some(Ctl::Save), None, t0);
        h.tick(None, None, t0 + Duration::from_secs(30));
        assert_eq!(h.hover(Ctl::Save), 0.0);
        assert!(!h.animating());
    }

    /// The depress is a fixed pixel squeeze, not a ratio — a wide control and a
    /// narrow one move their edges by the same amount.
    #[test]
    fn the_press_rect_shrinks_by_pixels_and_sinks() {
        let wide = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 30.0));
        let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(30.0, 30.0));
        let dw = wide.width() - pressed_rect(wide, 1.0).width();
        let dn = narrow.width() - pressed_rect(narrow, 1.0).width();
        assert!((dw - dn).abs() < 1e-4);
        assert!(pressed_rect(wide, 1.0).center().y > wide.center().y);
        assert_eq!(pressed_rect(wide, 0.0), wide);
    }
}
