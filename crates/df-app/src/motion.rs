//! Easing curves and one-shot animations — the motion vocabulary of PLAN §8,
//! ported from delightviewer's `gesture.rs`.
//!
//! Two rules this module exists to keep:
//!
//! 1. **CSS semantics.** Everything in delightstack is written as a
//!    `cubic-bezier(x1, y1, x2, y2)`, so the port evaluates the same curve the
//!    same way a browser does — a Newton solve for the parameter at `x = t`
//!    with a bisection fallback. A hand-rolled "ease-out-ish" polynomial would
//!    be a second, slightly different motion language in the same program.
//! 2. **Pure functions over `(inputs, Instant)`.** An animation here is a
//!    description — where it started, where it is going, when it started, how
//!    long it takes — that can be *sampled* at any instant. There is no
//!    integration step, no accumulated state, and therefore nothing that drifts
//!    when frames are late, and every curve is unit-testable without a window
//!    (PLAN §1).
// Phase 0 ports the whole vocabulary; most of its callers arrive with the panes
// it exists for (PLAN §10). Kept whole and allowed here rather than trimmed to
// what today's placeholder happens to touch — deleting half a motion system and
// re-porting it in Phase 4 is exactly how constants drift away from the
// originals they were checked against.
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// The two curves PLAN §8 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    /// `cubic-bezier(0.22, 1, 0.36, 1)` — delightstack's momentum/slide curve.
    /// Front-loaded: most of the distance is covered in the first third, which
    /// is what makes a slide read as *decelerating* rather than as travelling.
    OutQuint,
    /// `cubic-bezier(0.34, 1.30, 0.55, 1)` — `BACK_OUT_EASING`, which
    /// overshoots and settles. The 1.30 variant is the one delightstack's
    /// carousel actually ships (the 1.56 one is commented out there); a bigger
    /// overshoot on a file row would read as a bounce, not as a spring.
    BackOut,
    /// Straight-line. Not a design curve — it exists so a value that must not
    /// be shaped (a decay already given its shape elsewhere, a test) can say so
    /// rather than borrowing one of the two above.
    Linear,
}

impl Easing {
    /// The curve at progress `t`. Out-of-range `t` is **clamped, never
    /// extrapolated** — a late frame must not walk `BackOut` off the end of its
    /// overshoot and back down.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::OutQuint => cubic_bezier(0.22, 1.0, 0.36, 1.0, t),
            Easing::BackOut => cubic_bezier(0.34, 1.30, 0.55, 1.0, t),
            Easing::Linear => t,
        }
    }
}

/// How many Newton iterations get a shot at the `x → parameter` solve before
/// the bisection fallback takes over. Browsers use the same order of magnitude;
/// past this it is converging on a difference nobody can see.
const NEWTON_STEPS: usize = 8;

/// Bisection passes after Newton gives up. 24 halvings of `[0, 1]` is a
/// parameter resolved to ~6e-8 — far below the pixel this eventually becomes.
const BISECT_STEPS: usize = 24;

/// Close enough in `x` to stop solving: a thousandth of a percent of the
/// curve's domain, well under one frame of one animation.
const SOLVE_EPSILON: f32 = 1e-5;

/// A slope this flat makes Newton's division meaningless (and can throw `u`
/// out of range); hand over to bisection instead.
const FLAT_SLOPE: f32 = 1e-6;

/// CSS `cubic-bezier(x1, y1, x2, y2)` evaluated at progress `t`.
///
/// The control points define a *parametric* curve, so getting `y` for a given
/// `x` needs the parameter solved first. Newton converges in a couple of steps
/// for well-behaved curves; the bisection fallback exists because it cannot
/// fail, and `BackOut`'s control points are exactly the kind that make Newton
/// wander.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    // A cubic Bézier with endpoints pinned at 0 and 1, which is what CSS
    // guarantees: only the two middle control points are free.
    fn bezier(a: f32, b: f32, u: f32) -> f32 {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    }
    fn bezier_slope(a: f32, b: f32, u: f32) -> f32 {
        let v = 1.0 - u;
        3.0 * v * v * a + 6.0 * v * u * (b - a) + 3.0 * u * u * (1.0 - b)
    }
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let mut u = t;
    for _ in 0..NEWTON_STEPS {
        let x = bezier(x1, x2, u) - t;
        if x.abs() < SOLVE_EPSILON {
            return bezier(y1, y2, u);
        }
        let slope = bezier_slope(x1, x2, u);
        if slope.abs() < FLAT_SLOPE {
            break;
        }
        u -= x / slope;
    }
    // Newton wandered out of range (possible with the BackOut control points);
    // bisect, which cannot.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut u = t;
    for _ in 0..BISECT_STEPS {
        let x = bezier(x1, x2, u);
        if (x - t).abs() < SOLVE_EPSILON {
            break;
        }
        if x < t {
            lo = u;
        } else {
            hi = u;
        }
        u = (lo + hi) * 0.5;
    }
    bezier(y1, y2, u)
}

/// A duration of zero would divide progress by zero. Treated as "one
/// microsecond", so a zero-length animation is simply already finished rather
/// than a NaN spreading through a layout.
const MIN_DURATION_SECS: f32 = 1e-6;

/// One eased animation from `from` to `to`, sampled at an `Instant`.
///
/// This is the shape *every* one-shot in delightfile takes (PLAN §8: "momentum
/// is one eased animation, never a physics loop"). Because it is pure, a
/// dropped frame costs nothing — the next sample lands wherever the clock says
/// it should be, not wherever an accumulator got to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub t0: Instant,
    pub duration: Duration,
    pub easing: Easing,
}

impl Tween {
    pub fn new(from: f32, to: f32, duration: Duration, easing: Easing, now: Instant) -> Tween {
        Tween {
            from,
            to,
            t0: now,
            duration,
            easing,
        }
    }

    /// Linear progress 0…1. `saturating_duration_since` rather than
    /// `duration_since`: a sample taken with an instant from *before* the start
    /// (a stale `now` carried into a frame) must read as "not begun", not
    /// panic.
    pub fn progress(&self, now: Instant) -> f32 {
        // A zero-length animation is one that was asked for and has already
        // happened; it is *done*, not stuck at zero forever.
        if self.duration.is_zero() {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.t0).as_secs_f32();
        (elapsed / self.duration.as_secs_f32().max(MIN_DURATION_SECS)).clamp(0.0, 1.0)
    }

    /// The value now. Note this is a lerp *through* the eased fraction, so
    /// `BackOut` overshoots `to` and comes back, as it must.
    pub fn value(&self, now: Instant) -> f32 {
        let e = self.easing.apply(self.progress(now));
        self.from + (self.to - self.from) * e
    }

    /// Has it arrived? The `animating()` half of PLAN §1's idle-cost rule: a
    /// finished tween must stop asking for frames.
    pub fn finished(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    /// When the next frame is owed, or `None` when there is nothing left to
    /// draw. Callers hand this straight to the repaint deadline.
    pub fn ends_at(&self) -> Instant {
        self.t0 + self.duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_curves_start_at_zero_and_end_at_one() {
        for e in [Easing::OutQuint, Easing::BackOut, Easing::Linear] {
            assert!((e.apply(0.0)).abs() < 1e-4, "{e:?} at 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-4, "{e:?} at 1");
            // Out of range is clamped, never extrapolated.
            assert_eq!(e.apply(-1.0), 0.0);
            assert_eq!(e.apply(2.0), 1.0);
        }
    }

    /// `cubic-bezier(0.22, 1, 0.36, 1)` is front-loaded: most of the distance
    /// is covered in the first third. That is what makes a slide read as
    /// deceleration rather than as travel.
    #[test]
    fn out_quint_is_front_loaded() {
        let a = Easing::OutQuint.apply(0.25);
        let b = Easing::OutQuint.apply(0.5);
        assert!(
            a > 0.6,
            "quarter of the time should cover most of it, got {a}"
        );
        assert!(b > 0.9, "half the time should be nearly done, got {b}");
        // Monotonic — no wobble on the way.
        let mut prev = 0.0;
        for i in 0..=20 {
            let v = Easing::OutQuint.apply(i as f32 / 20.0);
            assert!(v >= prev - 1e-4, "not monotonic at {i}");
            prev = v;
        }
    }

    /// back-out(1.30) must actually overshoot — that is the whole reason it is
    /// the spring curve rather than another ease-out.
    #[test]
    fn back_out_overshoots_past_one_then_settles() {
        let peak = (0..=40)
            .map(|i| Easing::BackOut.apply(i as f32 / 40.0))
            .fold(0.0f32, f32::max);
        assert!(peak > 1.0, "back-out never overshot (peak {peak})");
        assert!(peak < 1.2, "back-out overshot absurdly (peak {peak})");
        assert!((Easing::BackOut.apply(1.0) - 1.0).abs() < 1e-4);
    }

    /// The identity case, which also pins the solver's accuracy: a bezier whose
    /// control points lie on the diagonal *is* `t`.
    #[test]
    fn a_linear_bezier_is_the_identity() {
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let v = cubic_bezier(1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, t);
            assert!((v - t).abs() < 1e-3, "at {t} got {v}");
        }
    }

    #[test]
    fn a_tween_runs_from_its_start_to_its_end_and_then_stops() {
        let t0 = Instant::now();
        let tw = Tween::new(10.0, 20.0, Duration::from_millis(400), Easing::OutQuint, t0);
        assert_eq!(tw.value(t0), 10.0);
        assert!(!tw.finished(t0));
        let mid = tw.value(t0 + Duration::from_millis(100));
        assert!(mid > 10.0 && mid < 20.0, "got {mid}");
        // Front-loaded: a quarter of the way through is already past halfway.
        assert!(mid > 15.0, "OutQuint should be ahead of linear, got {mid}");
        assert!((tw.value(t0 + Duration::from_millis(400)) - 20.0).abs() < 1e-4);
        assert!(tw.finished(t0 + Duration::from_millis(400)));
        // Past the end it stays put — no extrapolation, no second life.
        assert!((tw.value(t0 + Duration::from_secs(10)) - 20.0).abs() < 1e-4);
    }

    /// A stale `now` from before the start must read as "not begun" rather than
    /// panicking on a negative duration.
    #[test]
    fn a_tween_sampled_before_it_started_has_not_started() {
        let t0 = Instant::now() + Duration::from_secs(1);
        let tw = Tween::new(0.0, 1.0, Duration::from_millis(200), Easing::Linear, t0);
        assert_eq!(tw.progress(t0 - Duration::from_millis(500)), 0.0);
    }

    /// A zero-length tween is finished, not a NaN.
    #[test]
    fn a_zero_length_tween_is_already_over() {
        let t0 = Instant::now();
        let tw = Tween::new(0.0, 1.0, Duration::ZERO, Easing::OutQuint, t0);
        assert!(tw.finished(t0));
        assert_eq!(tw.value(t0), 1.0);
    }
}
