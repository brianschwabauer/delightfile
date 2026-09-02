//! Zoom and pan for the preview pane — **ported from delightviewer's
//! `crates/dlv-app/src/gesture.rs`**, which is itself a port of the delightstack
//! Carousel's state machine and physics.
//!
//! Every number, curve and duration here is delightviewer's to the digit. The
//! point of a port rather than a rewrite is that a photograph zoomed in this
//! program and the same photograph zoomed in that one feel like one gesture,
//! and the only way to guarantee that is to copy the constants instead of
//! re-deriving them.
//!
//! ## Coordinate model
//!
//! Everything is **screen points about the pane's centre**: [`View::pan`] is
//! the offset of the picture's centre from the pane's centre, and
//! [`View::scale`] is relative to the *fitted* size — 1.0 is always "the whole
//! thing, as large as the pane draws it". The clamp bounds in [`clamp_pan`] are
//! the Carousel's `calcBounds` written in those terms: same numbers, same
//! 100 px of overshoot, fewer terms.
//!
//! ## What this port deliberately leaves behind
//!
//! delightviewer is a viewer with a *reel* of files; this is a pane that shows
//! whatever the cursor is on. Three of its gestures have no meaning here and
//! are gone rather than disabled, so there is no path left that can fire them:
//!
//! * **Swipe left/right to the previous/next file** (`Phase::PanX`, `handoff`,
//!   `Slide`, `GestureEvent::Navigate`, the flick's multi-item skip). The list
//!   is what changes file here, and a drag that walked the cursor off the row
//!   would be a pane stealing the list's job.
//! * **Drag-down to dismiss** (`Phase::PanYDismiss`, `dismiss_scale`,
//!   `circ_in_out`, `DISMISS_*`). There is no window to close: the pane is a
//!   third of a file manager.
//! * **The page-handoff slide** (`page_handoff`, `page_slide`) — dragging a
//!   page up and having the next one slide in. Pages still turn (see
//!   [`GestureEvent::PageStep`]) and still land on the edge you are arriving
//!   from ([`Gestures::land_on_page`], kept), but by the wheel and the
//!   keyboard, which is how this pane has always turned them.
//!
//! With the handoff and the dismiss gone, `Settle` — the released-but-
//! uncommitted drag springing back to zero — has nothing left to settle, so it
//! is gone too. The sub-fit spring-back that *is* wanted was never a `Settle`:
//! it is [`SPRING_DURATION`] of [`Easing::BackOut`] on the view itself, and it
//! is ported whole.
//!
//! Three types are borrowed from this program rather than re-ported, because
//! delightfile already has delightviewer's copy of each and a second one would
//! be a number free to drift: [`Easing`] (`crate::motion`, the same
//! `cubic-bezier` solve), [`WheelUnit`] (`crate::mouse`), and `egui::Vec2` in
//! place of the hand-rolled vector — egui's needs no `Context`, so the physics
//! is still unit-testable without a window.

use std::time::{Duration, Instant};

use egui::Vec2;

use crate::motion::Easing;
use crate::mouse::{WheelUnit, DOUBLE_CLICK_WINDOW, DRAG_THRESHOLD};

use super::Zoom;

// ── Constants, verbatim from the Carousel by way of delightviewer ───────────

/// `CLAMP_PADDING` — how far past the edge a zoomed picture may be dragged.
pub const CLAMP_PADDING: f32 = 100.0;

/// Zoom ceiling, applied before any spring.
pub const MAX_SCALE: f32 = 6.0;

/// Zoom floor, applied before the rubber band springs back to 1.
pub const MIN_SCALE: f32 = 0.5;

/// Double-click target scale.
pub const DOUBLE_CLICK_SCALE: f32 = 3.0;

/// Double-click / keyboard-zoom animation length.
pub const ZOOM_DURATION: Duration = Duration::from_millis(200);

/// Wheel-zoom blend — the wheel writes a target and the view eases toward it.
pub const WHEEL_DURATION: Duration = Duration::from_millis(300);

/// The momentum fling, executed as ONE eased animation rather than a physics
/// loop (PLAN §8, and `crate::mouse`'s header).
pub const MOMENTUM_DURATION: Duration = Duration::from_millis(600);

/// Spring-back after a rubber-banded (sub-fit) scale is released.
pub const SPRING_DURATION: Duration = Duration::from_millis(400);

/// `normalizeWheel` line/page multipliers and clamp.
///
/// **Not `crate::mouse::POINTS_PER_LINE`.** That one converts a wheel notch
/// into a *distance a list scrolls*; this one converts it into the exponent of
/// a zoom, and the two have no reason to be the same number — they were tuned
/// against different things.
const DELTA_LINE_MULTIPLIER: f32 = 8.0;
const DELTA_PAGE_MULTIPLIER: f32 = 24.0;
const MAX_WHEEL_DELTA: f32 = 24.0;

/// Wheel zoom rate, as an exponent: one normalized point of wheel travel
/// multiplies the scale by `e^0.045`.
///
/// delightviewer's note on why it is exponential rather than the Carousel's
/// linear `1 − dy · 0.03`: a linear read of a multiplicative control means the
/// same notch buys a smaller fraction of what the eye is looking at at 400 %
/// than it did at 100 %, and the wheel feels like it is running out of road. An
/// exponential is the same *ratio* per notch at every zoom, which is what
/// "linear" means to the hand.
const WHEEL_ZOOM_RATE: f32 = 0.045;

/// Keyboard `+`/`-` step, anchored at the pane's centre — which is what makes
/// `+` `+` `+` feel like a telescope rather than like a drag.
const KEY_ZOOM_STEP: f32 = 1.25;

/// A press this short with (almost) no travel and no velocity is a click, not
/// a drag. delightviewer: "A press under `gesture::TAP_DURATION` is still a
/// click and still plays or pauses a clip."
pub const TAP_DURATION: Duration = Duration::from_millis(150);
const TAP_MAX_VELOCITY: f32 = 0.5;

/// How far apart the two halves of a "double click" may land and still be one
/// gesture. A double click whose halves are 200 px apart is two clicks.
const DOUBLE_CLICK_SLOP: f32 = 32.0;

/// How long the wheel may go quiet before the sub-fit spring fires when the
/// zoom arrived with no animation to hang the spring on (a touchpad pinch has
/// no release event).
const PINCH_SPRING_IDLE: Duration = Duration::from_millis(150);

/// Scale below which the view counts as "fitted" (the Carousel's 1.01 epsilon).
pub const FIT_EPSILON: f32 = 0.01;

/// One wheel tick's worth of "next item", when the pane has items to tick
/// between — see [`Gestures::set_navigates_at_fit`].
const WHEEL_NAV_THRESHOLD: f32 = 12.0;

/// What a plain wheel roll should do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAction {
    Zoom,
    /// Step to the pane's own next item. In delightviewer that was the next
    /// *file*; here the pane never leaves the file the cursor is on, so the
    /// only thing it can step to is the next **page** of a document.
    Navigate,
}

/// The wheel-behaviour decision, as a pure function so *both* answers stay
/// tested however a given body sets `navigates_at_fit`.
pub fn wheel_action(scale: f32, ctrl: bool, navigates_at_fit: bool) -> WheelAction {
    let fitted = (scale - 1.0).abs() <= FIT_EPSILON;
    if navigates_at_fit && !ctrl && fitted {
        WheelAction::Navigate
    } else {
        WheelAction::Zoom
    }
}

/// `circIn` from the Carousel's rubber band: the easing applied to how much
/// *further* below fit a shrink is allowed to go.
pub fn circ_in(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t * t).max(0.0).sqrt()
}

// ── The view transform ─────────────────────────────────────────────────────

/// Where the picture sits. `scale` is relative to the fitted (letterboxed)
/// size, so 1.0 is always "the whole thing, as large as the pane draws it".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub scale: f32,
    /// Picture-centre offset from pane centre, in screen points.
    pub pan: Vec2,
}

impl Default for View {
    fn default() -> View {
        View::FIT
    }
}

impl View {
    pub const FIT: View = View {
        scale: 1.0,
        pan: Vec2::ZERO,
    };

    /// Linear interpolation of both components — animations ease `t`, not this.
    pub fn lerp(self, to: View, t: f32) -> View {
        View {
            scale: self.scale + (to.scale - self.scale) * t,
            pan: self.pan + (to.pan - self.pan) * t,
        }
    }

    /// Apply the transform to the rect the picture would occupy at fit.
    ///
    /// The whole of the painting side of this module: a fitted rect goes in,
    /// the rect actually drawn comes out. Scaling about the rect's own centre
    /// and then translating is the same thing as scaling about the pane's
    /// centre — the fitted rect is already centred on it — and it is one
    /// multiply instead of a matrix.
    pub fn apply(self, fitted: egui::Rect) -> egui::Rect {
        egui::Rect::from_center_size(fitted.center() + self.pan, fitted.size() * self.scale)
    }
}

// ── Pure physics ───────────────────────────────────────────────────────────

/// `clampMatrix`/`calcBounds`, in centre-relative screen points.
///
/// Below fit the picture is pinned to the centre. Above it, the picture may be
/// dragged until its far edge reaches the pane's edge, plus [`CLAMP_PADDING`]
/// of overshoot — and an axis whose picture is narrower than the pane *minus*
/// both paddings is pinned, exactly like the Carousel's
/// `imageW * scale < viewportW - padding * 2` branch.
pub fn clamp_pan(pan: Vec2, fitted: Vec2, viewport: Vec2, scale: f32) -> Vec2 {
    Vec2::new(
        clamp_axis(pan.x, fitted.x * scale, viewport.x),
        clamp_axis(pan.y, fitted.y * scale, viewport.y),
    )
}

fn clamp_axis(pan: f32, content: f32, viewport: f32) -> f32 {
    if content < viewport - CLAMP_PADDING * 2.0 {
        return 0.0;
    }
    let limit = (content - viewport) / 2.0 + CLAMP_PADDING;
    pan.clamp(-limit, limit)
}

/// The Carousel's rubber band on *scale*: above fit it is a plain clamp to
/// [`MAX_SCALE`]; below fit a shrink is progressively resisted by [`circ_in`],
/// with [`MIN_SCALE`] as the hard floor before the spring takes over.
pub fn rubber_band_scale(scale: f32, factor: f32) -> f32 {
    let mut target = (scale * factor).min(MAX_SCALE);
    if factor < 1.0 && scale <= 1.0 {
        target = scale * (factor + circ_in(1.0 - scale) * (1.0 - factor));
    }
    target.max(MIN_SCALE)
}

/// Zoom to `target_scale` while keeping the picture point under `anchor`
/// (pane-centre-relative screen coords) under `anchor`.
pub fn zoom_about(view: View, target_scale: f32, anchor: Vec2) -> View {
    let k = target_scale / view.scale.max(1e-6);
    View {
        scale: target_scale,
        // The picture point under the anchor is (anchor − pan) / scale; keeping
        // it fixed after scaling by k means pan' = anchor − k·(anchor − pan).
        pan: anchor - (anchor - view.pan) * k,
    }
}

/// `normalizeWheel`: line/page units become points, and one tick is clamped so
/// a chunky mouse wheel and a smooth trackpad land in the same range.
pub fn normalize_wheel(delta: Vec2, unit: WheelUnit) -> Vec2 {
    let k = match unit {
        WheelUnit::Point => 1.0,
        WheelUnit::Line => DELTA_LINE_MULTIPLIER,
        WheelUnit::Page => DELTA_PAGE_MULTIPLIER,
    };
    let limit = |v: f32| v.signum() * (MAX_WHEEL_DELTA.min(v.abs()));
    Vec2::new(limit(delta.x * k), limit(delta.y * k))
}

/// The wheel's zoom factor for a normalized `dy`.
///
/// `e^(−dy · rate)`, so the factor is a pure ratio: two notches are exactly one
/// notch squared, whichever zoom they are pressed at, and a notch back out
/// undoes a notch in to the pixel.
pub fn wheel_zoom_factor(dy: f32) -> f32 {
    (-dy * WHEEL_ZOOM_RATE).exp()
}

/// The momentum fling: `min(3000, |v| · 300) · sign(v)`, in screen points.
///
/// The Carousel writes this as `… / scale` and then applies it through a matrix
/// that multiplies by `scale` again, so the scale cancels and the fling is the
/// same physical distance whatever the zoom. `v` is points per millisecond.
pub fn momentum_offset(velocity: Vec2) -> Vec2 {
    let axis = |v: f32| 3000.0f32.min((v * 300.0).abs()) * v.signum();
    Vec2::new(axis(velocity.x), axis(velocity.y))
}

// ── Animation ──────────────────────────────────────────────────────────────

/// One eased hop from one view to another. Sampled, never integrated — a late
/// frame costs nothing and the animation always knows when it has arrived
/// (PLAN §1, and `crate::motion`'s header).
#[derive(Debug, Clone, Copy)]
struct Anim {
    from: View,
    to: View,
    t0: Instant,
    duration: Duration,
    easing: Easing,
}

impl Anim {
    fn sample(&self, now: Instant) -> (View, bool) {
        let elapsed = now.saturating_duration_since(self.t0).as_secs_f32();
        let total = self.duration.as_secs_f32().max(1e-6);
        let t = (elapsed / total).clamp(0.0, 1.0);
        (self.from.lerp(self.to, self.easing.apply(t)), t >= 1.0)
    }
}

// ── The state machine ──────────────────────────────────────────────────────

/// The committed gesture. A gesture **never reclassifies after commit** — that
/// is the whole point of the machine, and it is what stops a drag that started
/// as a pan from turning into something else halfway through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Pressed, but under the 10 px threshold — nothing decided yet.
    Indeterminate,
    /// Panning a zoomed picture, or `Ctrl`+dragging to zoom (the pinch
    /// stand-in: winit on Wayland delivers no touchpad pinch).
    Pan,
    /// Committed to nothing. A drag at fit has no picture to move and — with
    /// the swipe and the dismiss gone — nothing else to mean, so it is inert
    /// for the rest of the press rather than being re-examined every frame.
    Inert,
}

/// What the gesture layer wants the pane to do, beyond moving the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureEvent {
    /// A press that stayed a click: under [`TAP_DURATION`], no travel, no
    /// velocity. `double` marks the second half of a double click, which has
    /// *also* just zoomed — see [`Gestures::handle_click`].
    Tap { double: bool },
    /// A wheel tick at fit on a document with pages. +1 is the next page.
    PageStep(i32),
}

/// What the pane must feed in each frame.
pub struct Input {
    /// The pane's content box, in points.
    pub viewport: Vec2,
    /// The size the picture is drawn at when `scale` is 1, in points.
    ///
    /// **Given, not derived.** delightviewer computed this itself from the
    /// source size (`fit_size`); here `preview::paint::fit_rect` and
    /// `preview::paint::doc_rect` already answer it, and they answer it with
    /// rules this module does not have — the 8× magnification cap on a tiny
    /// icon, the HiDPI division, a page fitted from the pane rather than from
    /// its raster. Two fits in one program is how they drift apart.
    pub fitted: Vec2,
    /// Pointer position, pane-centre-relative, if it is over the pane.
    pub pointer: Option<Vec2>,
    pub pointer_down: bool,
    pub pointer_pressed: bool,
    pub pointer_released: bool,
    pub pointer_delta: Vec2,
    /// Points per millisecond, from egui's pointer velocity.
    pub velocity: Vec2,
    pub ctrl: bool,
    /// Raw wheel events this frame, in their own units.
    pub wheel: Vec<(WheelUnit, Vec2)>,
    /// egui's normalized pinch factor. Only consulted when there were no wheel
    /// events, since egui synthesizes this *from* ctrl+wheel and it would
    /// otherwise double-apply.
    pub zoom_delta: f32,
    pub now: Instant,
}

impl Input {
    /// An input with nothing happening in it — the shape every caller starts
    /// from, since most frames have no pointer and no wheel.
    pub fn still(viewport: Vec2, fitted: Vec2, now: Instant) -> Input {
        Input {
            viewport,
            fitted,
            pointer: None,
            pointer_down: false,
            pointer_pressed: false,
            pointer_released: false,
            pointer_delta: Vec2::ZERO,
            velocity: Vec2::ZERO,
            ctrl: false,
            wheel: Vec::new(),
            zoom_delta: 1.0,
            now,
        }
    }
}

/// The live zoom/pan state for whatever the pane is showing.
pub struct Gestures {
    view: View,
    anim: Option<Anim>,
    phase: Phase,
    /// Pointer travel since press, for the 10 px commit threshold.
    press_travel: Vec2,
    press_at: Option<Instant>,
    last_click: Option<Instant>,
    last_click_pos: Vec2,
    /// When the sub-fit spring-back is due. The Carousel springs only after the
    /// *last* wheel event's blend has finished — springing on every tick is
    /// what made zooming out feel like a fight.
    spring_at: Option<Instant>,
    /// Wheel accumulator for the at-fit page step.
    wheel_nav: f32,
    /// May a wheel roll at fit step to the next item? Set per body by
    /// [`Gestures::set_navigates_at_fit`].
    navigates_at_fit: bool,
}

impl Default for Gestures {
    fn default() -> Gestures {
        Gestures::new()
    }
}

impl Gestures {
    pub fn new() -> Gestures {
        Gestures {
            view: View::FIT,
            anim: None,
            phase: Phase::Idle,
            press_travel: Vec2::ZERO,
            press_at: None,
            last_click: None,
            last_click_pos: Vec2::ZERO,
            spring_at: None,
            wheel_nav: 0.0,
            navigates_at_fit: false,
        }
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Is the view zoomed past fit? What the cursor icon and the paint both
    /// ask.
    pub fn is_zoomed(&self) -> bool {
        self.view.scale > 1.0 + FIT_EPSILON
    }

    /// Whether a wheel roll at fit steps to the next item instead of zooming.
    ///
    /// **The one place this port reads delightviewer's `WHEEL_AT_FIT_NAVIGATES`
    /// differently, because "the next item" means something different here.**
    /// There it was the next *file*, and it shipped **off**: spending the wheel
    /// on navigation would have left `Ctrl`+wheel as the only way to zoom a
    /// photograph, which on a platform with no touchpad pinch is no way at all.
    /// That reasoning is unchanged for a photograph, a clip or a single-page
    /// document, and all three set this false.
    ///
    /// A **multi-page document** sets it true, and gets delightviewer's other
    /// answer: at fit the wheel turns pages, which is what a wheel has always
    /// done over a PDF in this pane and what a wheel does over a PDF
    /// everywhere else. Past fit — and under `Ctrl` at any zoom — it zooms,
    /// exactly as the flag's own branch says.
    pub fn set_navigates_at_fit(&mut self, yes: bool) {
        if self.navigates_at_fit == yes {
            return;
        }
        self.navigates_at_fit = yes;
        // Anything already accumulated belongs to the body that is ending.
        self.wheel_nav = 0.0;
    }

    /// Is anything **moving**? PLAN §1's idle rule: a settled view must stop
    /// asking for frames.
    ///
    /// A scheduled spring is deliberately not moving. It is a single instant
    /// known in advance, so it is a `WaitUntil` ([`Gestures::next_deadline`])
    /// and not a hundred and fifty milliseconds of frames spent waiting for a
    /// clock.
    pub fn is_animating(&self) -> bool {
        self.anim.is_some()
    }

    /// When the *scheduled* spring is due, for a pane that is otherwise still.
    ///
    /// The sub-fit spring is the one thing here that waits rather than moves:
    /// the wheel writes a deadline and nothing happens until it arrives. A
    /// deadline, not a poll (PLAN §1).
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        self.spring_at
            .filter(|_| self.anim.is_none())
            .map(|at| at.saturating_duration_since(now))
    }

    /// Back to fit for a newly previewed file.
    ///
    /// Zoom is **not** remembered per file (unlike the reading position — see
    /// `Pane::places`): a photograph you left at 4× and come back to is a
    /// photograph you cannot see, and the pane's job on arrival is to show you
    /// what the file is.
    pub fn reset_for_new_item(&mut self) {
        self.view = View::FIT;
        self.anim = None;
        self.phase = Phase::Idle;
        self.press_at = None;
        self.press_travel = Vec2::ZERO;
        self.last_click = None;
        self.spring_at = None;
        self.wheel_nav = 0.0;
    }

    /// Run one frame of input. Returns whatever the pane has to act on.
    pub fn update(&mut self, input: &Input) -> Vec<GestureEvent> {
        let mut events = Vec::new();
        if input.fitted.x <= 0.0 || input.fitted.y <= 0.0 {
            return events;
        }

        // A running animation owns the view until a pointer or wheel takes it
        // back — the Carousel commits styles and cancels for the same reason.
        if let Some(anim) = self.anim {
            let (view, done) = anim.sample(input.now);
            self.view = view;
            if done {
                self.anim = None;
            }
        }

        // The scheduled sub-fit spring: due, nothing else animating, nothing
        // held — go home with the 400 ms back-out chaser.
        if let Some(at) = self.spring_at {
            if input.now >= at && self.anim.is_none() && !input.pointer_down {
                self.spring_at = None;
                if self.view.scale < 1.0 - 1e-3 {
                    self.animate(View::FIT, SPRING_DURATION, Easing::BackOut, input.now);
                }
            }
        }

        self.handle_wheel(input, &mut events);
        self.handle_pointer(input, &mut events);
        events
    }

    // ── Wheel ──────────────────────────────────────────────────────────────

    fn handle_wheel(&mut self, input: &Input, events: &mut Vec<GestureEvent>) {
        let mut dy = 0.0;
        for (unit, delta) in &input.wheel {
            dy += normalize_wheel(*delta, *unit).y;
        }
        if input.wheel.is_empty() {
            // No wheel this frame: a real pinch may still have produced a zoom
            // factor.
            if (input.zoom_delta - 1.0).abs() > 1e-3 {
                let anchor = input.pointer.unwrap_or(Vec2::ZERO);
                let target = rubber_band_scale(self.view.scale, input.zoom_delta);
                self.set_zoom_from(self.view, target, anchor, input, Duration::ZERO);
            }
            return;
        }
        if dy == 0.0 {
            return;
        }

        if self.anim.is_none()
            && wheel_action(self.view.scale, input.ctrl, self.navigates_at_fit)
                == WheelAction::Navigate
        {
            self.wheel_nav += dy;
            while self.wheel_nav.abs() >= WHEEL_NAV_THRESHOLD {
                let step = self.wheel_nav.signum();
                self.wheel_nav -= step * WHEEL_NAV_THRESHOLD;
                events.push(GestureEvent::PageStep(step as i32));
            }
            return;
        }
        self.wheel_nav = 0.0;

        let anchor = input.pointer.unwrap_or(Vec2::ZERO);
        // Compound against where the *last* notch was headed, not against where
        // the blend has got to so far. Rolling the wheel faster than the 300 ms
        // ease can keep up used to lose most of the travel — every event read a
        // scale that was still catching up to the one before it, so ten fast
        // notches zoomed less than ten slow ones. The animation is presentation;
        // the target is the state.
        let base = self.anim.map(|a| a.to).unwrap_or(self.view);
        let target = rubber_band_scale(base.scale, wheel_zoom_factor(dy));
        self.set_zoom_from(base, target, anchor, input, WHEEL_DURATION);
    }

    /// Animate toward `target_scale` anchored at `anchor`, then spring back if
    /// the rubber band left us under fit (the 400 ms back-out chaser).
    ///
    /// `from` is not necessarily the view on screen — the wheel compounds
    /// against its own pending target.
    fn set_zoom_from(
        &mut self,
        from: View,
        target_scale: f32,
        anchor: Vec2,
        input: &Input,
        duration: Duration,
    ) {
        let mut to = zoom_about(from, target_scale, anchor);
        to.pan = clamp_pan(to.pan, input.fitted, input.viewport, to.scale);
        if to.scale < 1.0 {
            // Under fit: ease toward the rubber-banded target like any other
            // wheel step, and *schedule* the spring for when that blend ends.
            // Every further tick pushes the deadline out, so the spring never
            // fights a wheel that is still rolling — it fires once, after the
            // last event, exactly like the Carousel's `lastWheelEvent` guard.
            if duration.is_zero() {
                self.view = to;
                self.anim = None;
                self.spring_at = Some(input.now + PINCH_SPRING_IDLE);
            } else {
                self.animate(to, duration, Easing::OutQuint, input.now);
                self.spring_at = Some(input.now + duration);
            }
            return;
        }
        self.spring_at = None;
        if duration.is_zero() {
            self.view = to;
            self.anim = None;
        } else {
            self.animate(to, duration, Easing::OutQuint, input.now);
        }
    }

    fn animate(&mut self, to: View, duration: Duration, easing: Easing, now: Instant) {
        self.anim = Some(Anim {
            from: self.view,
            to,
            t0: now,
            duration,
            easing,
        });
    }

    // ── Pointer ────────────────────────────────────────────────────────────

    fn handle_pointer(&mut self, input: &Input, events: &mut Vec<GestureEvent>) {
        if input.pointer_pressed {
            self.phase = Phase::Indeterminate;
            self.press_travel = Vec2::ZERO;
            self.press_at = Some(input.now);
            // Taking the view back from a running animation, at wherever it had
            // got to — the Carousel's `commitStyles(); cancel()`. The release
            // path owns any sub-fit spring from here.
            self.anim = None;
            self.spring_at = None;
        }

        if input.pointer_down && self.phase != Phase::Idle {
            self.press_travel += input.pointer_delta;
            if self.phase == Phase::Indeterminate {
                self.commit_phase(input);
            }
            self.drag(input);
        }

        if input.pointer_released {
            self.release(input, events);
        }
    }

    /// The commit: 10 px of travel, and then there is only one thing left for a
    /// drag to be.
    fn commit_phase(&mut self, input: &Input) {
        // Ctrl+drag is the pinch stand-in — there is no touchpad pinch on
        // Wayland, and vertical drag is the gesture people already make for
        // "zoom" in every other app that offers this.
        if input.ctrl {
            self.phase = Phase::Pan;
            return;
        }
        if self.press_travel.length() < DRAG_THRESHOLD {
            return;
        }
        // Zoomed in, any direction: pan the picture. At fit there is nothing to
        // pan and — with the swipe and the dismiss gone — nothing else a drag
        // could mean, so it commits to doing nothing for the rest of the press.
        self.phase = if self.view.scale > 1.0 + FIT_EPSILON {
            Phase::Pan
        } else {
            Phase::Inert
        };
    }

    fn drag(&mut self, input: &Input) {
        let d = input.pointer_delta;
        match self.phase {
            Phase::Indeterminate | Phase::Idle | Phase::Inert => {}
            Phase::Pan => {
                if input.ctrl {
                    // Ctrl+drag: vertical travel is a zoom about the press
                    // point.
                    let anchor = input.pointer.unwrap_or(Vec2::ZERO);
                    let factor = 1.0 - d.y * 0.01;
                    let target = rubber_band_scale(self.view.scale, factor);
                    let mut to = zoom_about(self.view, target, anchor);
                    to.pan = clamp_pan(to.pan, input.fitted, input.viewport, to.scale);
                    self.view = to;
                    return;
                }
                // A plain drag while zoomed pans. Whatever travel the clamp
                // refuses is simply refused: in delightviewer the leftover
                // became a swipe to the next file or a page handoff, and this
                // port has neither.
                self.view.pan = clamp_pan(
                    self.view.pan + d,
                    input.fitted,
                    input.viewport,
                    self.view.scale,
                );
            }
        }
    }

    fn release(&mut self, input: &Input, events: &mut Vec<GestureEvent>) {
        let phase = self.phase;
        self.phase = Phase::Idle;

        // A press that neither travelled nor lingered is a click.
        let held = self
            .press_at
            .map(|t| input.now.saturating_duration_since(t))
            .unwrap_or(Duration::MAX);
        let is_click = held < TAP_DURATION
            && input.velocity.length() < TAP_MAX_VELOCITY
            && self.press_travel.length() < DRAG_THRESHOLD;
        self.press_at = None;

        if is_click {
            let double = self.handle_click(input);
            events.push(GestureEvent::Tap { double });
            return;
        }

        if phase == Phase::Pan {
            if self.view.scale <= 1.0 + FIT_EPSILON {
                self.animate(View::FIT, SPRING_DURATION, Easing::BackOut, input.now);
            } else {
                // Momentum: ONE eased animation, not a physics loop (PLAN §8).
                let offset = momentum_offset(input.velocity);
                let to = View {
                    scale: self.view.scale,
                    pan: clamp_pan(
                        self.view.pan + offset,
                        input.fitted,
                        input.viewport,
                        self.view.scale,
                    ),
                };
                self.animate(to, MOMENTUM_DURATION, Easing::OutQuint, input.now);
            }
        }
    }

    /// The double-click zoom. Returns whether this click was the second half of
    /// one, which is how the pane knows a play/pause it has already performed
    /// needs taking back (see `Pane::gesture`).
    fn handle_click(&mut self, input: &Input) -> bool {
        let pos = input.pointer.unwrap_or(Vec2::ZERO);
        let double = self
            .last_click
            .map(|t| input.now.saturating_duration_since(t) < DOUBLE_CLICK_WINDOW)
            .unwrap_or(false)
            && (pos - self.last_click_pos).length() < DOUBLE_CLICK_SLOP;
        self.last_click = Some(input.now);
        self.last_click_pos = pos;
        if !double {
            return false;
        }
        // Three clicks in a row are a double and then a single, not two
        // doubles: the pair is spent.
        self.last_click = None;
        if (self.view.scale - 1.0).abs() <= FIT_EPSILON {
            self.zoom_to(DOUBLE_CLICK_SCALE, pos, input);
        } else {
            self.animate(View::FIT, ZOOM_DURATION, Easing::OutQuint, input.now);
        }
        true
    }

    // ── Keyboard-driven zoom and pan ───────────────────────────────────────

    /// `+` / `-` / `0`, anchored at the pane's centre — which is what makes
    /// `+` `+` `+` feel like a telescope rather than like a drag.
    ///
    /// The keyboard and the pointer drive **one** `View`, on one curve, so the
    /// two can never disagree about where the picture is.
    pub fn zoom_command(&mut self, step: Zoom, input: &Input) {
        match step {
            Zoom::In => {
                let target = (self.view.scale * KEY_ZOOM_STEP).min(MAX_SCALE);
                self.zoom_to(target, Vec2::ZERO, input);
            }
            Zoom::Out => {
                // Never below fit from the keyboard: the rubber band is a
                // *gesture*'s way of saying "that is as far out as it goes",
                // and a key press has no release to spring it back from.
                let target = (self.view.scale / KEY_ZOOM_STEP).max(1.0);
                self.zoom_to(target, Vec2::ZERO, input);
            }
            Zoom::Fit => self.animate(View::FIT, ZOOM_DURATION, Easing::OutQuint, input.now),
        }
    }

    /// `Ctrl+↑`/`Ctrl+↓` over a zoomed picture: move it by `offset` points, on
    /// the same curve a double-click zoom takes.
    ///
    /// `false` when there is nothing to move, so the caller falls back to
    /// scrolling the body — the same "did you take the key" answer
    /// `Pane::doc_scroll` has always given.
    pub fn pan_by(&mut self, offset: Vec2, input: &Input) -> bool {
        // Against the view the *last* command was headed for, not the one on
        // screen: `+` and then `Ctrl+↓` inside the 200 ms zoom is one hand
        // doing two things, and reading the half-finished scale would refuse
        // the second because the first had not landed yet.
        let from = self.anim.map(|a| a.to).unwrap_or(self.view);
        if from.scale <= 1.0 + FIT_EPSILON || input.fitted.x <= 0.0 {
            return false;
        }
        let to = View {
            scale: from.scale,
            pan: clamp_pan(from.pan + offset, input.fitted, input.viewport, from.scale),
        };
        if (to.pan - self.view.pan).length() < 0.01 {
            return true;
        }
        self.animate(to, ZOOM_DURATION, Easing::OutQuint, input.now);
        true
    }

    /// Arrive on a new page at the edge the reader is coming from: forward
    /// lands at the top, backward at the bottom.
    ///
    /// delightviewer's `land_on_page`, minus the handoff it also cleared. A
    /// page turn that dropped you mid-page would be the one place this pane
    /// made you hunt for the text — and only when there is more page than pane,
    /// because at fit "the top" would just be the clamp's 100 px of overshoot
    /// applied for no reason.
    pub fn land_on_page(&mut self, direction: i32, input: &Input) {
        self.anim = None;
        if self.view.scale <= 1.0 + FIT_EPSILON {
            return;
        }
        let extreme = Vec2::new(self.view.pan.x, direction as f32 * f32::INFINITY);
        self.view.pan = clamp_pan(extreme, input.fitted, input.viewport, self.view.scale);
    }

    fn zoom_to(&mut self, target_scale: f32, anchor: Vec2, input: &Input) {
        let mut to = zoom_about(self.view, target_scale, anchor);
        to.pan = clamp_pan(to.pan, input.fitted, input.viewport, to.scale);
        self.animate(to, ZOOM_DURATION, Easing::OutQuint, input.now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWPORT: Vec2 = Vec2 { x: 800.0, y: 600.0 };

    /// The clamp is the ported `calcBounds`: the far edge may pass the pane's
    /// edge by exactly [`CLAMP_PADDING`], and no further.
    #[test]
    fn pan_clamp_allows_exactly_one_hundred_pixels_of_overshoot() {
        // A full-bleed picture at 2×: 1600×1200 in an 800×600 pane.
        let fitted = VIEWPORT;
        let limit = (1600.0 - 800.0) / 2.0 + CLAMP_PADDING; // 500
        let p = clamp_pan(Vec2::new(9999.0, 0.0), fitted, VIEWPORT, 2.0);
        assert!((p.x - limit).abs() < 1e-3, "got {}", p.x);
        let p = clamp_pan(Vec2::new(-9999.0, 0.0), fitted, VIEWPORT, 2.0);
        assert!((p.x + limit).abs() < 1e-3);
        // Inside the bounds, the pan is untouched.
        let p = clamp_pan(Vec2::new(120.0, -30.0), fitted, VIEWPORT, 2.0);
        assert_eq!(p, Vec2::new(120.0, -30.0));
    }

    /// An axis whose picture is narrower than the pane less both paddings is
    /// pinned dead centre — the Carousel's `imageW*scale < viewportW - 2p`.
    #[test]
    fn a_narrow_axis_is_pinned_to_the_centre() {
        let fitted = Vec2::new(200.0, 600.0);
        let p = clamp_pan(Vec2::new(300.0, 0.0), fitted, VIEWPORT, 1.0);
        assert_eq!(p.x, 0.0);
    }

    #[test]
    fn zoom_keeps_the_anchored_point_under_the_cursor() {
        let anchor = Vec2::new(200.0, -100.0);
        let view = zoom_about(View::FIT, 3.0, anchor);
        assert_eq!(view.scale, 3.0);
        // The picture point under the anchor before and after must match.
        let before = (anchor - View::FIT.pan) * (1.0 / View::FIT.scale);
        let after = (anchor - view.pan) * (1.0 / view.scale);
        assert!((before - after).length() < 1e-3);
    }

    #[test]
    fn the_rubber_band_resists_below_fit_and_clamps_above_the_ceiling() {
        // Above fit: a plain multiply, clamped at the ceiling.
        assert!((rubber_band_scale(2.0, 1.5) - 3.0).abs() < 1e-4);
        assert!((rubber_band_scale(5.0, 4.0) - MAX_SCALE).abs() < 1e-4);
        // At exactly fit the band has not started giving yet — `circIn(0)` is
        // zero — so the first notch out is the plain product.
        assert!((rubber_band_scale(1.0, 0.5) - 0.5).abs() < 1e-4);
        // Below it the shrink is progressively resisted, so the result is
        // *larger* than the plain product — and never past the floor.
        assert!(rubber_band_scale(0.8, 0.5) > 0.8 * 0.5);
        assert!(rubber_band_scale(0.6, 0.1) >= MIN_SCALE);
    }

    #[test]
    fn circ_in_is_the_carousel_curve() {
        assert!((circ_in(0.0)).abs() < 1e-6);
        assert!((circ_in(1.0) - 1.0).abs() < 1e-6);
        assert!(circ_in(0.5) < 0.5, "circIn starts slow");
    }

    #[test]
    fn wheel_normalization_scales_lines_and_clamps_the_tick() {
        let points = normalize_wheel(Vec2::new(0.0, 3.0), WheelUnit::Point);
        assert!((points.y - 3.0).abs() < 1e-4);
        let lines = normalize_wheel(Vec2::new(0.0, 2.0), WheelUnit::Line);
        assert!((lines.y - 16.0).abs() < 1e-4);
        // One enormous tick is clamped, in both directions.
        let huge = normalize_wheel(Vec2::new(0.0, 500.0), WheelUnit::Page);
        assert!((huge.y - MAX_WHEEL_DELTA).abs() < 1e-4);
        let huge = normalize_wheel(Vec2::new(0.0, -500.0), WheelUnit::Page);
        assert!((huge.y + MAX_WHEEL_DELTA).abs() < 1e-4);
    }

    /// The wheel-behaviour decision is about the *fitted* case only: past fit,
    /// and under `Ctrl`, the wheel zooms whatever the flag says.
    #[test]
    fn the_wheel_behaviour_flag_decides_only_the_fitted_case() {
        assert_eq!(wheel_action(1.0, false, false), WheelAction::Zoom);
        assert_eq!(wheel_action(1.0, false, true), WheelAction::Navigate);
        assert_eq!(wheel_action(1.0, true, true), WheelAction::Zoom);
        assert_eq!(wheel_action(2.0, false, true), WheelAction::Zoom);
        assert_eq!(wheel_action(0.9, false, true), WheelAction::Zoom);
    }

    #[test]
    fn wheel_up_zooms_in_and_down_zooms_out() {
        // egui's wheel is positive-up, and the Carousel's dy is inverted from
        // it, so a *negative* dy is a zoom in.
        assert!(wheel_zoom_factor(-10.0) > 1.0);
        assert!(wheel_zoom_factor(10.0) < 1.0);
        // Exactly reciprocal: a notch out undoes a notch in.
        assert!((wheel_zoom_factor(-4.0) * wheel_zoom_factor(4.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn momentum_is_three_hundred_milliseconds_of_travel_capped_at_three_thousand() {
        assert!((momentum_offset(Vec2::new(2.0, 0.0)).x - 600.0).abs() < 1e-3);
        assert!((momentum_offset(Vec2::new(-2.0, 0.0)).x + 600.0).abs() < 1e-3);
        assert!((momentum_offset(Vec2::new(50.0, 0.0)).x - 3000.0).abs() < 1e-3);
    }

    #[test]
    fn a_momentum_target_still_respects_the_clamp() {
        let fitted = VIEWPORT;
        let from = View {
            scale: 2.0,
            pan: Vec2::new(400.0, 0.0),
        };
        let offset = momentum_offset(Vec2::new(5.0, 0.0)); // 1500 px
        let to = clamp_pan(from.pan + offset, fitted, VIEWPORT, from.scale);
        assert!((to.x - 500.0).abs() < 1e-3, "got {}", to.x);
    }

    /// A full-bleed picture: fitted exactly to the 800×600 pane.
    fn input(now: Instant) -> Input {
        Input::still(VIEWPORT, VIEWPORT, now)
    }

    /// One click: press, then release `held` later having travelled `travel`.
    fn click_with(
        g: &mut Gestures,
        at: Instant,
        held: Duration,
        travel: Vec2,
    ) -> Vec<GestureEvent> {
        let mut i = input(at);
        i.pointer = Some(Vec2::ZERO);
        i.pointer_pressed = true;
        i.pointer_down = true;
        g.update(&i);
        if travel != Vec2::ZERO {
            let mut i = input(at + held / 2);
            i.pointer = Some(travel);
            i.pointer_down = true;
            i.pointer_delta = travel;
            g.update(&i);
        }
        let mut i = input(at + held);
        i.pointer = Some(travel);
        i.pointer_released = true;
        g.update(&i)
    }

    fn click(g: &mut Gestures, at: Instant) -> Vec<GestureEvent> {
        click_with(g, at, Duration::from_millis(30), Vec2::ZERO)
    }

    /// The commit-once rule: a drag that committed to `Pan` while zoomed stays
    /// `Pan`, and one that committed to `Inert` at fit stays inert.
    #[test]
    fn a_committed_gesture_never_reclassifies() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        let mut i = input(t0);
        i.pointer_pressed = true;
        i.pointer_down = true;
        g.update(&i);
        assert_eq!(g.phase(), Phase::Indeterminate);

        let mut i = input(t0 + Duration::from_millis(20));
        i.pointer_down = true;
        i.pointer_delta = Vec2::new(20.0, 2.0);
        g.update(&i);
        assert_eq!(g.phase(), Phase::Inert, "at fit there is nothing to pan");

        // Even a huge later drag does not re-open the question.
        let mut i = input(t0 + Duration::from_millis(40));
        i.pointer_down = true;
        i.pointer_delta = Vec2::new(0.0, 200.0);
        g.update(&i);
        assert_eq!(g.phase(), Phase::Inert);
        assert_eq!(g.view().pan, Vec2::ZERO, "an inert drag moves nothing");
    }

    /// Under the 10 px threshold nothing commits at all.
    #[test]
    fn a_gesture_under_the_threshold_stays_indeterminate() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        let mut i = input(t0);
        i.pointer_pressed = true;
        i.pointer_down = true;
        g.update(&i);
        let mut i = input(t0 + Duration::from_millis(20));
        i.pointer_down = true;
        i.pointer_delta = Vec2::new(6.0, 4.0);
        g.update(&i);
        assert_eq!(g.phase(), Phase::Indeterminate);
    }

    #[test]
    fn a_double_click_zooms_to_three_times_and_back() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        click(&mut g, t0);
        click(&mut g, t0 + Duration::from_millis(150));
        // Let the 200 ms animation finish.
        g.update(&input(t0 + Duration::from_millis(600)));
        assert!(
            (g.view().scale - DOUBLE_CLICK_SCALE).abs() < 0.01,
            "got {}",
            g.view().scale
        );

        // A second double-click goes back to fit.
        click(&mut g, t0 + Duration::from_millis(700));
        click(&mut g, t0 + Duration::from_millis(850));
        g.update(&input(t0 + Duration::from_millis(1400)));
        assert!(
            (g.view().scale - 1.0).abs() < 0.01,
            "got {}",
            g.view().scale
        );
    }

    /// Two clicks a long way apart in time are two clicks, not a double.
    #[test]
    fn slow_clicks_do_not_zoom() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        click(&mut g, t0);
        click(&mut g, t0 + Duration::from_millis(900));
        g.update(&input(t0 + Duration::from_millis(1500)));
        assert!((g.view().scale - 1.0).abs() < 1e-4);
    }

    /// **Task A's disambiguation, end to end.** A quick still press is a tap
    /// and nothing else; the second of a pair is a tap *marked* double, so the
    /// pane can undo the play/pause the first one caused; and a press that
    /// travelled, or one that lingered, is not a tap at all.
    #[test]
    fn a_click_taps_a_drag_does_not_and_a_double_says_so() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        assert_eq!(
            click(&mut g, t0),
            vec![GestureEvent::Tap { double: false }],
            "a lone click plays"
        );
        assert_eq!(
            click(&mut g, t0 + Duration::from_millis(150)),
            vec![GestureEvent::Tap { double: true }],
            "the second half says so, and has zoomed"
        );

        // A press that travelled past the threshold is a drag.
        let mut g = Gestures::new();
        let events = click_with(&mut g, t0, Duration::from_millis(60), Vec2::new(40.0, 0.0));
        assert!(events.is_empty(), "a drag never taps: {events:?}");

        // …and one held past TAP_DURATION is a press, not a click.
        let mut g = Gestures::new();
        let events = click_with(
            &mut g,
            t0,
            TAP_DURATION + Duration::from_millis(10),
            Vec2::ZERO,
        );
        assert!(events.is_empty(), "a long hold never taps: {events:?}");
    }

    /// A pan only exists past fit, and it obeys the clamp.
    #[test]
    fn a_zoomed_drag_pans_and_flings() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        click(&mut g, t0);
        click(&mut g, t0 + Duration::from_millis(150));
        g.update(&input(t0 + Duration::from_millis(600)));
        assert!(g.is_zoomed());

        let at = t0 + Duration::from_millis(700);
        let mut i = input(at);
        i.pointer = Some(Vec2::ZERO);
        i.pointer_pressed = true;
        i.pointer_down = true;
        g.update(&i);
        let mut i = input(at + Duration::from_millis(40));
        i.pointer_down = true;
        i.pointer_delta = Vec2::new(-60.0, 0.0);
        g.update(&i);
        assert_eq!(g.phase(), Phase::Pan);
        assert!(g.view().pan.x < -50.0, "got {}", g.view().pan.x);

        // Released with velocity: the fling is one eased animation, and it ends.
        let mut i = input(at + Duration::from_millis(80));
        i.pointer_released = true;
        i.velocity = Vec2::new(-2.0, 0.0);
        g.update(&i);
        assert!(g.is_animating());
        g.update(&input(at + Duration::from_millis(80) + MOMENTUM_DURATION));
        assert!(!g.is_animating(), "the fling must end (PLAN §1)");
    }

    /// Wheeling *out* from fit rubber-bands — eased, like every wheel step —
    /// and springs back to exactly fit only after the blend has finished, never
    /// while the wheel is still rolling.
    #[test]
    fn wheeling_out_from_fit_springs_back() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        let mut i = input(t0);
        i.wheel = vec![(WheelUnit::Point, Vec2::new(0.0, 10.0))];
        g.update(&i);
        g.update(&input(t0 + Duration::from_millis(150)));
        assert!(
            g.view().scale < 1.0,
            "the band must give: {}",
            g.view().scale
        );

        // A second tick keeps pushing the spring out.
        let mut i = input(t0 + Duration::from_millis(200));
        i.wheel = vec![(WheelUnit::Point, Vec2::new(0.0, 10.0))];
        g.update(&i);
        g.update(&input(t0 + Duration::from_millis(450)));
        assert!(g.view().scale < 1.0, "still held down: {}", g.view().scale);

        // Quiet: the blend ends at 500 ms, the 400 ms spring follows.
        g.update(&input(t0 + Duration::from_millis(600)));
        g.update(&input(t0 + Duration::from_millis(1100)));
        assert!(
            (g.view().scale - 1.0).abs() < 1e-3,
            "got {}",
            g.view().scale
        );
        assert!(!g.is_animating(), "and then it stops asking for frames");
    }

    #[test]
    fn the_wheel_zooms_about_the_cursor() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        let mut i = input(t0);
        i.pointer = Some(Vec2::new(200.0, 0.0));
        i.wheel = vec![(WheelUnit::Point, Vec2::new(0.0, -10.0))];
        g.update(&i);
        // The blend is a 300 ms animation; sample past its end.
        g.update(&input(t0 + Duration::from_millis(400)));
        assert!(g.view().scale > 1.0, "got {}", g.view().scale);
        // Zooming about a point right of centre pushes the picture left.
        assert!(g.view().pan.x < 0.0, "got {}", g.view().pan.x);
    }

    /// A multi-page document takes the wheel as a page turn at fit, and as a
    /// zoom once it is past fit — the two halves of `wheel_action`'s branch,
    /// through the machine.
    #[test]
    fn a_paged_document_turns_pages_at_fit_and_zooms_past_it() {
        let mut g = Gestures::new();
        g.set_navigates_at_fit(true);
        let t0 = Instant::now();
        let mut i = input(t0);
        // One notch of a line-unit wheel is 8 normalized points; two clear the
        // 12-point threshold.
        i.wheel = vec![(WheelUnit::Line, Vec2::new(0.0, -2.0))];
        let events = g.update(&i);
        assert_eq!(events, vec![GestureEvent::PageStep(-1)]);
        assert!((g.view().scale - 1.0).abs() < 1e-4, "and it did not zoom");

        // Ctrl takes the same roll back to zooming.
        let mut i = input(t0 + Duration::from_millis(50));
        i.ctrl = true;
        i.wheel = vec![(WheelUnit::Line, Vec2::new(0.0, -2.0))];
        let events = g.update(&i);
        assert!(events.is_empty(), "ctrl+wheel never turns a page");
        g.update(&input(t0 + Duration::from_millis(500)));
        assert!(g.view().scale > 1.0, "got {}", g.view().scale);

        // …and now that it is zoomed, a plain roll zooms too.
        let mut i = input(t0 + Duration::from_millis(600));
        i.wheel = vec![(WheelUnit::Line, Vec2::new(0.0, -2.0))];
        assert!(g.update(&i).is_empty());
    }

    #[test]
    fn keyboard_zoom_is_anchored_at_the_centre_and_fit_goes_home() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        g.zoom_command(Zoom::In, &input(t0));
        g.update(&input(t0 + Duration::from_millis(300)));
        assert!((g.view().scale - KEY_ZOOM_STEP).abs() < 0.01);
        assert_eq!(g.view().pan, Vec2::ZERO, "centre zoom must not pan");

        g.zoom_command(Zoom::Fit, &input(t0 + Duration::from_millis(300)));
        g.update(&input(t0 + Duration::from_millis(700)));
        assert!((g.view().scale - 1.0).abs() < 1e-3);
        assert!(g.view().pan.length() < 0.5);

        // Zoom out never goes below fit from the keyboard.
        g.zoom_command(Zoom::Out, &input(t0 + Duration::from_millis(700)));
        g.update(&input(t0 + Duration::from_millis(1100)));
        assert!((g.view().scale - 1.0).abs() < 1e-3);
    }

    /// The keyboard pan takes the key only when there is something to move.
    #[test]
    fn the_keyboard_pan_declines_at_fit() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        assert!(!g.pan_by(Vec2::new(0.0, -24.0), &input(t0)));

        g.zoom_command(Zoom::In, &input(t0));
        g.zoom_command(Zoom::In, &input(t0));
        g.zoom_command(Zoom::In, &input(t0));
        g.update(&input(t0 + Duration::from_millis(300)));
        assert!(g.pan_by(
            Vec2::new(0.0, -24.0),
            &input(t0 + Duration::from_millis(300))
        ));
    }

    /// A new file arrives at fit, whatever the last one was left at.
    #[test]
    fn a_new_item_starts_fitted() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        click(&mut g, t0);
        click(&mut g, t0 + Duration::from_millis(150));
        g.update(&input(t0 + Duration::from_millis(600)));
        assert!(g.is_zoomed());
        g.reset_for_new_item();
        assert_eq!(g.view(), View::FIT);
        assert!(!g.is_animating());
    }

    /// The transform the paint applies: fit in, drawn rect out.
    #[test]
    fn the_view_scales_about_the_pane_centre() {
        let fitted = egui::Rect::from_center_size(egui::pos2(100.0, 100.0), egui::vec2(80.0, 40.0));
        assert_eq!(View::FIT.apply(fitted), fitted);
        let view = View {
            scale: 2.0,
            pan: Vec2::new(10.0, 0.0),
        };
        let drawn = view.apply(fitted);
        assert_eq!(drawn.size(), egui::vec2(160.0, 80.0));
        assert_eq!(drawn.center(), egui::pos2(110.0, 100.0));
    }

    /// A pane with nothing in it must not divide by zero or move.
    #[test]
    fn a_degenerate_picture_is_left_alone() {
        let mut g = Gestures::new();
        let t0 = Instant::now();
        let mut i = Input::still(VIEWPORT, Vec2::ZERO, t0);
        i.pointer_pressed = true;
        i.pointer_down = true;
        i.wheel = vec![(WheelUnit::Point, Vec2::new(0.0, -10.0))];
        assert!(g.update(&i).is_empty());
        assert_eq!(g.view(), View::FIT);
    }
}
