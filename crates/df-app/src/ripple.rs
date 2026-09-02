//! Click ripples for the painter (PLAN §8: "ripples on click for rows,
//! buttons, tabs — radial expand from the pointer, eased alpha,
//! painter-drawn").
//!
//! New code, but written to the same rules as [`crate::hover`]: state is a
//! plain list of *descriptions* — where the click landed, how far the circle
//! has to travel, and when it started — and everything visible is a pure
//! function of that and an [`Instant`]. Nothing integrates, so a dropped frame
//! costs nothing; and a ripple that has finished is dropped rather than kept at
//! zero alpha, so an idle window asks for no frames at all (PLAN §1).
//!
//! What a ripple is *for* is worth stating, because it decides every constant
//! below: it acknowledges a click on a surface that has no other way to say it
//! heard one. It is not decoration and it is not a highlight — the hover and
//! press amounts already carry the state. So it is faint, it is quick, and it
//! is gone before the thing it acknowledged has finished happening.
// Phase 0 ports the whole vocabulary; most of its callers arrive with the panes
// it exists for (PLAN §10). Kept whole and allowed here rather than trimmed to
// what today's placeholder happens to touch — deleting half a motion system and
// re-porting it in Phase 4 is exactly how constants drift away from the
// originals they were checked against.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use crate::hover::Key;
use crate::motion::Easing;

/// How long one ripple lives, in seconds.
///
/// 0.45 s is the length delightstack's ripple runs and it is chosen against the
/// *action*, not against the eye: a click on a file row opens a directory or
/// starts a preview, and the ripple has to still be visibly travelling while
/// that lands or it reads as a separate event. Much shorter and it is a blink
/// nobody registers; much longer and a double-click stacks two ripples that are
/// both still bright.
const LIFE: f32 = 0.45;

/// The radius a ripple starts at, in logical pixels. A circle of radius zero is
/// nothing, and its first frame would be a sub-pixel dot at whatever refresh
/// rate the compositor felt like; starting at a few pixels means the first
/// frame the user sees is already a mark under the pointer.
const SEED_RADIUS: f32 = 6.0;

/// Peak alpha of the ripple over its surface colour.
///
/// PLAN §8 asks for subtle — 0.06–0.10 — and 0.08 sits in the middle of that
/// on purpose: on catppuccin-mocha's `surface0` (#313244) a white overlay at
/// 0.08 is roughly the same lift as the hover state itself, so the ripple reads
/// as the surface briefly brightening under the finger rather than as a
/// separate white shape sliding across it. Above ~0.12 it stops being a
/// material and starts being a splash.
const PEAK_ALPHA: f32 = 0.08;

/// Fraction of the life the ripple holds full brightness before it starts to
/// go. At [`LIFE`] that is about 110 ms — long enough for the eye to catch it
/// at its brightest while it is still small and near the pointer, which is
/// where the acknowledgement means something. A ripple that starts fading from
/// frame one is brightest at its least informative moment.
const ALPHA_HOLD: f32 = 0.25;

/// How many ripples one control may have in flight.
///
/// Concurrency is the point — a fast double-click should show two circles, not
/// one restarted one — but a held-down mouse on a repeating control could
/// otherwise queue an unbounded number of them, all drawn every frame. Eight is
/// more than any human input produces inside 0.45 s; the oldest is dropped
/// past that, and being the oldest it is also the faintest.
const MAX_LIVE: usize = 8;

/// A ripple's radius at progress `t`, in logical pixels.
///
/// [`Easing::OutQuint`] because the ripple is *momentum*: it leaves the pointer
/// fast and coasts to the edge of the control, the same curve every slide in
/// the program uses. A linear expansion reads mechanical, like a loading
/// indicator rather than a reaction.
pub fn ripple_radius(cover: f32, t: f32) -> f32 {
    let e = Easing::OutQuint.apply(t);
    SEED_RADIUS + (cover - SEED_RADIUS).max(0.0) * e
}

/// A ripple's alpha at progress `t`, as a 0…1 fraction.
///
/// Full for the first [`ALPHA_HOLD`] of the life, then a quadratic *ease-in*
/// falloff: it lingers, then leaves quickly. That is the opposite shape to the
/// radius on purpose — the circle is already near the edge of the control by
/// the time the alpha starts moving, so what the eye sees is a mark that
/// spreads and then dissolves, not one that spreads *while* dissolving and is
/// half gone before it arrives.
pub fn ripple_alpha(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t <= ALPHA_HOLD {
        return PEAK_ALPHA;
    }
    let fade = (t - ALPHA_HOLD) / (1.0 - ALPHA_HOLD);
    PEAK_ALPHA * (1.0 - fade * fade)
}

/// One ripple, ready to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Splash {
    pub center: egui::Pos2,
    pub radius: f32,
    /// 0…1. The caller multiplies this into whatever colour the control's
    /// surface calls for — the ripple has no colour of its own, because what
    /// looks right on a row does not look right on an accent button.
    pub alpha: f32,
}

#[derive(Debug, Clone, Copy)]
struct Live<K> {
    key: K,
    center: egui::Pos2,
    /// Distance from the click to the furthest corner of the control, so the
    /// finished circle covers the whole surface however far off-centre the
    /// click was. Computed once at spawn: the rect can move (a scrolling list)
    /// and a ripple that re-measured would change size mid-flight.
    cover: f32,
    t0: Instant,
}

/// Every ripple currently in flight, across every control.
pub struct Ripples<K: Key> {
    items: Vec<Live<K>>,
}

// Derived `Default` would demand `K: Default`, which a key never needs to be.
impl<K: Key> Default for Ripples<K> {
    fn default() -> Ripples<K> {
        Ripples { items: Vec::new() }
    }
}

impl<K: Key> Ripples<K> {
    pub fn new() -> Ripples<K> {
        Ripples::default()
    }

    /// Start a ripple at `at` inside `rect`. Call this on mouse-*down*, not on
    /// click: the acknowledgement belongs to the press, and waiting for the
    /// release puts it after the thing it is acknowledging.
    pub fn spawn(&mut self, key: K, at: egui::Pos2, rect: egui::Rect, now: Instant) {
        let cover = [
            rect.left_top(),
            rect.right_top(),
            rect.left_bottom(),
            rect.right_bottom(),
        ]
        .iter()
        .map(|c| c.distance(at))
        .fold(0.0f32, f32::max);
        if self.items.iter().filter(|r| r.key == key).count() >= MAX_LIVE {
            if let Some(i) = self.items.iter().position(|r| r.key == key) {
                self.items.remove(i);
            }
        }
        self.items.push(Live {
            key,
            center: at,
            cover,
            t0: now,
        });
    }

    /// Rewrite every key through `f`, for the reason
    /// [`crate::hover::Hovers::remap`] gives: a key is a position, and a list
    /// that has reordered would otherwise leave its splashes behind on
    /// whatever inherited their index.
    pub fn remap(&mut self, f: impl Fn(K) -> K) {
        for live in &mut self.items {
            live.key = f(live.key);
        }
    }

    /// Drop the ripples that have finished. Separate from drawing so the prune
    /// happens exactly once a frame whether or not the control that owns a
    /// ripple is still on screen — a row scrolled out of view must not leave
    /// its ripple behind forever, asking for frames nobody draws.
    pub fn tick(&mut self, now: Instant) {
        self.items.retain(|r| progress(r.t0, now) < 1.0);
    }

    /// Is anything still travelling? Every live ripple is, by construction —
    /// unlike a hover there is no "held" state for one to rest in, so this is
    /// simply "is there one left". [`tick`](Ripples::tick) is what makes that
    /// true, and it is why the prune is not optional.
    pub fn animating(&self, now: Instant) -> bool {
        self.items.iter().any(|r| progress(r.t0, now) < 1.0)
    }

    /// When the last live ripple ends — the repaint deadline a caller can hand
    /// to the event loop when it would rather schedule one wake-up than ask for
    /// a frame every frame.
    pub fn ends_at(&self) -> Option<Instant> {
        self.items
            .iter()
            .map(|r| r.t0 + Duration::from_secs_f32(LIFE))
            .max()
    }

    /// What to draw for one control, oldest first so a newer, brighter ripple
    /// lands on top of an older one.
    pub fn splashes(&self, key: K, now: Instant) -> impl Iterator<Item = Splash> + '_ {
        self.items
            .iter()
            .filter(move |r| r.key == key)
            .map(move |r| {
                let t = progress(r.t0, now);
                Splash {
                    center: r.center,
                    radius: ripple_radius(r.cover, t),
                    alpha: ripple_alpha(t),
                }
            })
    }
}

/// Linear 0…1 progress through a ripple's life. `saturating_duration_since`
/// because a stale `now` must read as "not begun", not panic.
fn progress(t0: Instant, now: Instant) -> f32 {
    (now.saturating_duration_since(t0).as_secs_f32() / LIFE).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: egui::Rect = egui::Rect {
        min: egui::Pos2 { x: 0.0, y: 0.0 },
        max: egui::Pos2 { x: 200.0, y: 40.0 },
    };

    fn at(x: f32, y: f32) -> egui::Pos2 {
        egui::Pos2 { x, y }
    }

    /// The shape of the whole effect, in one test: a mark at the pointer that
    /// grows to cover the control and is gone by the end.
    #[test]
    fn a_ripple_starts_small_and_bright_and_ends_wide_and_invisible() {
        let corner = RECT.right_bottom().distance(at(0.0, 0.0));
        assert_eq!(ripple_radius(corner, 0.0), SEED_RADIUS);
        assert_eq!(ripple_alpha(0.0), PEAK_ALPHA);

        let mid_r = ripple_radius(corner, 0.5);
        assert!(mid_r > SEED_RADIUS && mid_r < corner, "got {mid_r}");
        // OutQuint is front-loaded: halfway through the time is most of the way
        // across the control.
        assert!(mid_r > corner * 0.85, "got {mid_r} of {corner}");
        let mid_a = ripple_alpha(0.5);
        assert!(mid_a > 0.0 && mid_a < PEAK_ALPHA, "got {mid_a}");

        assert!((ripple_radius(corner, 1.0) - corner).abs() < 1e-3);
        assert_eq!(ripple_alpha(1.0), 0.0);
    }

    /// The alpha lingers before it goes — the ripple must be at its brightest
    /// while it is still near the pointer.
    #[test]
    fn the_alpha_holds_then_falls_away() {
        assert_eq!(ripple_alpha(ALPHA_HOLD), PEAK_ALPHA);
        // Just past the hold it has barely moved (quadratic ease-in).
        assert!(ripple_alpha(ALPHA_HOLD + 0.05) > PEAK_ALPHA * 0.95);
        // Monotonic down from there, and never negative.
        let mut prev = PEAK_ALPHA;
        for i in 0..=20 {
            let a = ripple_alpha(i as f32 / 20.0);
            assert!(a <= prev + 1e-6, "alpha rose at {i}");
            assert!(a >= 0.0);
            prev = a;
        }
        // Subtle, per PLAN §8.
        assert!((0.06..=0.10).contains(&PEAK_ALPHA));
    }

    /// A click in a corner still covers the whole control — the radius is
    /// measured to the *furthest* corner, not to the nearest edge.
    #[test]
    fn an_off_centre_click_still_covers_the_control() {
        let mut r = Ripples::new();
        let t0 = Instant::now();
        r.spawn((), RECT.left_top(), RECT, t0);
        let end = t0 + Duration::from_secs_f32(LIFE);
        let last = r
            .splashes((), end - Duration::from_millis(1))
            .last()
            .expect("one ripple in flight");
        let furthest = RECT.right_bottom().distance(RECT.left_top());
        assert!(last.radius > furthest - 1.0, "got {}", last.radius);
    }

    /// Ripples are per-control and concurrent: a double-click shows two.
    #[test]
    fn ripples_stack_per_control_and_do_not_leak_across_controls() {
        let mut r = Ripples::new();
        let t0 = Instant::now();
        r.spawn(1u8, at(10.0, 10.0), RECT, t0);
        r.spawn(1u8, at(30.0, 10.0), RECT, t0 + Duration::from_millis(90));
        r.spawn(2u8, at(50.0, 10.0), RECT, t0 + Duration::from_millis(90));
        let now = t0 + Duration::from_millis(100);
        assert_eq!(r.splashes(1u8, now).count(), 2);
        assert_eq!(r.splashes(2u8, now).count(), 1);
        // The younger of the two is the brighter, and it is drawn last.
        let s: Vec<_> = r.splashes(1u8, now).collect();
        assert!(s[1].alpha >= s[0].alpha);
    }

    /// A finished ripple is dropped, and a window with no ripples asks for no
    /// frames — PLAN §1's idle rule.
    #[test]
    fn a_spent_ripple_is_pruned_and_stops_the_repaint_clock() {
        let mut r = Ripples::new();
        let t0 = Instant::now();
        r.spawn((), at(10.0, 10.0), RECT, t0);
        let mid = t0 + Duration::from_millis(200);
        r.tick(mid);
        assert!(r.animating(mid));
        assert_eq!(r.splashes((), mid).count(), 1);

        let after = t0 + Duration::from_secs_f32(LIFE) + Duration::from_millis(1);
        assert!(!r.animating(after), "a finished ripple is not animating");
        r.tick(after);
        assert_eq!(r.splashes((), after).count(), 0);
        assert!(r.ends_at().is_none());
    }

    /// A mashed control does not accumulate an unbounded pile of circles.
    #[test]
    fn a_control_holds_only_so_many_ripples() {
        let mut r = Ripples::new();
        let t0 = Instant::now();
        for i in 0..(MAX_LIVE as u64 + 5) {
            r.spawn((), at(10.0, 10.0), RECT, t0 + Duration::from_millis(i * 5));
        }
        let now = t0 + Duration::from_millis(100);
        assert_eq!(r.splashes((), now).count(), MAX_LIVE);
    }
}
