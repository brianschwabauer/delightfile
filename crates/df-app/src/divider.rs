//! The two dividers between the panes, taken hold of and dragged (PLAN §2).
//!
//! The feel is delightstack's `SplitPane`, ported for its numbers rather than
//! its code, with one deliberate difference. What it keeps:
//!
//! - **A minimum is a wall with give in it.** Past a pane's minimum the
//!   divider stops following the pointer and stretches instead, by
//!   [`rubber_band`]: `24 · tanh(overflow / 80)`, which gives at first and
//!   then refuses, and can never be pulled more than 24 points. Let go and it
//!   springs back onto the minimum over [`SPRING`] on `back-out`, the curve
//!   every other spring in the program settles on (`crate::motion`).
//! - **Home is magnetic.** The position the config's ratio puts a divider at
//!   captures it from [`SNAP_CAPTURE`] points away and does not let go until
//!   it is pulled [`SNAP_ESCAPE`] away — hysteresis, so a hand resting near
//!   the edge of the zone does not make the divider chatter between the two.
//!   While it is held there it lags the pointer on [`gravity`]'s curve and
//!   reaches it exactly at the escape boundary, so leaving is a release, not
//!   a jump. Measured in points, not percentages: a magnet that grew with the
//!   window would be a different magnet on every screen.
//! - The list never bounces and never folds. It is the pane the keyboard is
//!   in, so a divider simply stops at its minimum.
//!
//! What it changes: **folding does not end the drag.** `SplitPane` lets go of
//! the divider the moment a pane collapses, so the hand has to come back and
//! take hold again to undo a fold it overshot. Here a side pane folds
//! ([`FOLD`], `out-quint`) when the pointer goes [`FOLD_PAST`] points beyond
//! the place its minimum would put the divider, and opens again when the
//! pointer comes [`UNFOLD_BACK`] points back inside that place — the same
//! hysteresis, for the same reason — all while the button is still down.
//! Nothing is written until the button comes up; `Esc` before then puts
//! everything back where the press found it.
//!
//! Everything here is a pure function of pointer positions and `Instant`s, so
//! the whole feel is tested without a window, the way [`crate::scrollbar`]'s
//! is. The app asks for [`Dividers::split`] once a frame and hands it to
//! [`crate::ui::layout`].

use std::time::{Duration, Instant};

use df_core::state::{Panes, Side};

use crate::motion::{Easing, Tween};
use crate::ui::{Split, LIST_MIN, PARENT_MIN, PREVIEW_MIN};

/// How far from home a divider is captured, in points.
pub const SNAP_CAPTURE: f32 = 14.0;

/// How far it has to be pulled to get away again: `SplitPane`'s 2.2 × the
/// capture, which is what makes home feel held rather than merely marked.
pub const SNAP_ESCAPE: f32 = SNAP_CAPTURE * 2.2;

/// How much of the pull a snapped divider shows at first — the linear half of
/// [`gravity`]'s curve. Small, so a divider at home visibly *sticks*, and not
/// zero, so it never looks dead under a hand that is moving.
pub const GRAVITY: f32 = 0.16;

/// The furthest a rubber band stretches past a minimum, in points.
pub const BAND_REACH: f32 = 24.0;

/// How soon it stiffens: the overflow, in points, at which the band has given
/// three quarters of its reach.
pub const BAND_SOFTNESS: f32 = 80.0;

/// How far past the minimum's position the pointer goes before a side pane
/// folds, in points.
pub const FOLD_PAST: f32 = 40.0;

/// How far back inside the fold position it comes before the pane opens
/// again, in points.
pub const UNFOLD_BACK: f32 = 24.0;

/// A stretched or lagging divider settling onto where it stopped.
pub const SPRING: Duration = Duration::from_millis(300);

/// A pane folding, opening, or every pane going back home.
pub const FOLD: Duration = Duration::from_millis(200);

/// Below this the spring back is not worth a frame: half a point is a
/// difference nobody sees settle.
const SPRING_FLOOR: f32 = 0.5;

/// Which divider: between the parent and the list, or between the list and
/// the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Divider {
    Left,
    Right,
}

impl Divider {
    /// The side pane it borders, which is the one that folds.
    pub fn side(self) -> Side {
        match self {
            Divider::Left => Side::Parent,
            Divider::Right => Side::Preview,
        }
    }

    /// Its place in [`Split`]'s pairs and [`crate::ui::Layout::dividers`].
    pub fn index(self) -> usize {
        self.side().slot()
    }

    /// Which way the side pane grows as the pointer moves right: the parent
    /// does, the preview shrinks.
    fn direction(self) -> f32 {
        match self {
            Divider::Left => 1.0,
            Divider::Right => -1.0,
        }
    }

    fn minimum(self) -> f32 {
        match self {
            Divider::Left => PARENT_MIN,
            Divider::Right => PREVIEW_MIN,
        }
    }
}

/// The rubber band past a minimum: how far the divider is drawn beyond it for
/// a pointer `overflow` points beyond it. Bounded by [`BAND_REACH`] and signed
/// like the overflow.
pub fn rubber_band(overflow: f32) -> f32 {
    BAND_REACH * (overflow / BAND_SOFTNESS).tanh()
}

/// How far a snapped divider is drawn from home for a pointer `pull` points
/// from it: `GRAVITY · t + (1 − GRAVITY) · t²` of the escape radius, with `t`
/// the pull as a fraction of it. At the boundary that is exactly the pull,
/// which is the whole point: the divider catches up with the pointer at the
/// moment it lets go. Being *captured* is the one place it jumps — up to 6.4
/// points towards home at the 14-point edge — and that is meant: it is the
/// click of the magnet, and `SplitPane` does the same.
pub fn gravity(pull: f32) -> f32 {
    let t = (pull.abs() / SNAP_ESCAPE).min(1.0);
    pull.signum() * (GRAVITY * t + (1.0 - GRAVITY) * t * t) * SNAP_ESCAPE
}

/// What one move of a divider is measured against, in points of the side
/// pane's width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    /// The side pane's minimum.
    pub minimum: f32,
    /// Its widest: everything it and the list have between them, less the
    /// list's minimum.
    pub maximum: f32,
    /// Where home puts it, which is where it snaps.
    pub home: f32,
}

/// The two hysteresis latches, remembered from one move to the next: whether
/// the divider is held at home, and whether the side pane is folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Latch {
    pub snapped: bool,
    pub folded: bool,
}

/// Where a pointer asking for some width puts the divider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// The side pane's width where the rules leave it: the pointer's own
    /// within the bounds, the minimum or the maximum at them, home while
    /// snapped. What is kept when the button comes up.
    pub width: f32,
    /// How far the drawing is from `width`: the rubber band, or the snap's
    /// lag. What springs back to nothing when the button comes up.
    pub overshoot: f32,
    pub snapped: bool,
    pub folded: bool,
}

/// The divider for a pointer asking for a side pane `raw` points wide.
///
/// In this order: the fold, which trumps everything and is decided on the raw
/// pointer; the list's minimum, a plain stop; the side's minimum, the rubber
/// band; and home, the magnet. A window too narrow for both minimums keeps
/// the list's and gives the side what is left.
pub fn follow(track: &Track, raw: f32, latch: &mut Latch) -> Reading {
    let maximum = track.maximum.max(0.0);
    let minimum = track.minimum.min(maximum);
    let fold_at = minimum - FOLD_PAST;
    latch.folded = if latch.folded {
        raw <= fold_at + UNFOLD_BACK
    } else {
        raw < fold_at
    };
    if latch.folded {
        latch.snapped = false;
        return Reading {
            width: 0.0,
            overshoot: 0.0,
            snapped: false,
            folded: true,
        };
    }
    let plain = |width: f32, overshoot: f32| Reading {
        width,
        overshoot,
        snapped: false,
        folded: false,
    };
    if raw >= maximum {
        latch.snapped = false;
        return plain(maximum, 0.0);
    }
    if raw < minimum {
        latch.snapped = false;
        return plain(minimum, rubber_band(raw - minimum));
    }
    let reach = if latch.snapped {
        SNAP_ESCAPE
    } else {
        SNAP_CAPTURE
    };
    let home_reachable = (minimum..=maximum).contains(&track.home);
    latch.snapped = home_reachable && (raw - track.home).abs() <= reach;
    if latch.snapped {
        Reading {
            width: track.home,
            overshoot: gravity(raw - track.home),
            snapped: true,
            folded: false,
        }
    } else {
        plain(raw, 0.0)
    }
}

/// A divider in the hand.
#[derive(Debug, Clone, Copy)]
struct Drag {
    which: Divider,
    /// Where the button went down, and how wide the side pane was drawn
    /// then: the pointer asks for that width plus however far it has come.
    from_x: f32,
    width_at_press: f32,
    /// The widths as the press found them, which `Esc` puts back.
    at_press: Panes,
    /// …and as the drag has them now, which letting go keeps.
    live: Panes,
    latch: Latch,
    reading: Reading,
    /// The last reading with the pane open, which is what a pane folding
    /// mid-drag folds down *from* — its width and its stretch — so the fold
    /// starts where the hand left it rather than a jump away.
    shown: Reading,
    /// The width the drag was last measured against, for turning `shown`
    /// into a share.
    usable: f32,
}

/// Both dividers: the widths as the state file keeps them, and everything
/// moving between those and what is on screen.
#[derive(Debug, Clone)]
pub struct Dividers {
    /// The widths, committed — what the state file has, or is about to.
    panes: Panes,
    /// The config's ratio: where the dividers snap and a reset goes.
    home: Panes,
    /// How open the parent and the preview are, on their way between 0 and 1.
    open: [Tween; 2],
    /// The shares on screen when every pane was sent somewhere at once — a
    /// reset, or an `Esc` — and the clock of the slide from them to where
    /// they were sent.
    slide: Option<([f32; 3], Tween)>,
    /// Each divider's stretch or lag let go, springing back to nothing on
    /// `back-out`.
    springs: [Option<Tween>; 2],
    /// The share a side pane a drag folded is being folded *from*: the width
    /// the hand left it at, which is not the share it will open back to.
    /// Kept here rather than on the drag, because the fold outlives the drag
    /// — a divider let go mid-fold must go on folding from where it was, not
    /// from the share it had before the press. Cleared when the fold is done
    /// or the pane opens again.
    fold_from: [Option<f32>; 2],
    drag: Option<Drag>,
}

impl Dividers {
    pub fn new(panes: Panes, home: Panes, now: Instant) -> Dividers {
        let settled = |collapsed: bool| {
            let at = if collapsed { 0.0 } else { 1.0 };
            Tween::new(at, at, Duration::ZERO, Easing::Linear, now)
        };
        Dividers {
            panes,
            home,
            open: [
                settled(panes.parent_collapsed),
                settled(panes.preview_collapsed),
            ],
            slide: None,
            springs: [None; 2],
            fold_from: [None; 2],
            drag: None,
        }
    }

    /// How open the parent and the preview are at `now`.
    fn openness(&self, now: Instant) -> [f32; 2] {
        self.open.map(|tween| tween.value(now))
    }

    /// The widths as they are now, a drag included.
    fn live(&self) -> Panes {
        self.drag.map_or(self.panes, |drag| drag.live)
    }

    /// Whether `side` is folded — including by the drag in the hand, from the
    /// moment it folds rather than from the moment it is let go.
    pub fn collapsed(&self, side: Side) -> bool {
        self.live().collapsed(side)
    }

    /// The divider in the hand, if one is.
    pub fn dragging(&self) -> Option<Divider> {
        self.drag.map(|drag| drag.which)
    }

    /// The panes as they are to be drawn at `now` ([`Split`]).
    pub fn split(&self, now: Instant) -> Split {
        let open = self.openness(now);
        let mut fractions = self.fractions(open);
        if let Some((from, tween)) = &self.slide {
            let e = tween.value(now);
            for (to, from) in fractions.iter_mut().zip(from) {
                *to = from + (*to - from) * e;
            }
        }
        let mut overshoot = [self.spring_at(0, now), self.spring_at(1, now)];
        if let Some(drag) = &self.drag {
            overshoot[drag.which.index()] += drag.shown.overshoot;
        }
        Split {
            fractions,
            open,
            overshoot,
            collapsed: [self.collapsed(Side::Parent), self.collapsed(Side::Preview)],
        }
    }

    /// The shares to draw from: the live widths, with each folded pane that
    /// is still any amount open at the share it is folding from (or opening
    /// to), the list paying for it. A pane folded all the way is left at
    /// nothing, its share the list's, which is what the layout wants of a
    /// pane that is not there.
    fn fractions(&self, open: [f32; 2]) -> [f32; 3] {
        let live = self.live();
        let mut fractions = live.ratio;
        for side in [Side::Parent, Side::Preview] {
            if !live.collapsed(side) || open[side.slot()] <= 0.0 {
                continue;
            }
            let share = match self.fold_from[side.slot()] {
                Some(share) => share.min(fractions[1]),
                None => live.reopen_share(side, &self.home),
            };
            fractions[side.index()] = share;
            fractions[1] -= share;
        }
        fractions
    }

    /// How far divider `k`'s spring still has it from where it settles.
    fn spring_at(&self, k: usize, now: Instant) -> f32 {
        self.springs[k].map_or(0.0, |spring| spring.value(now))
    }

    /// Let divider `which` go `from` points away from where it settles. A
    /// spring it is still on is carried into the new one rather than cut off,
    /// so a divider let go twice in quick succession never jumps — and a
    /// let-go that adds nothing to it (a click, a press that never moved)
    /// leaves it running on its own clock rather than starting it again.
    /// "Nothing" is anything under [`SPRING_FLOOR`], the same half point a
    /// spring is not started for.
    fn spring_from(&mut self, which: Divider, from: f32, now: Instant) {
        let k = which.index();
        let running = self.springs[k].is_some_and(|spring| !spring.finished(now));
        if running && from.abs() <= SPRING_FLOOR {
            return;
        }
        let from = from + self.spring_at(k, now);
        self.springs[k] = (from.abs() > SPRING_FLOOR)
            .then(|| Tween::new(from, 0.0, SPRING, Easing::BackOut, now));
    }

    /// Whether anything is moving on its own: a fold, a slide, a spring. A
    /// drag is not — a divider held still is the same pixels next frame, and
    /// one that moves brings its own frame with it.
    pub fn animating(&self, now: Instant) -> bool {
        self.open.iter().any(|tween| !tween.finished(now))
            || self.slide.is_some_and(|(_, tween)| !tween.finished(now))
            || self
                .springs
                .iter()
                .flatten()
                .any(|spring| !spring.finished(now))
    }

    /// Drop whatever has finished: an `Option` that is never `None` again is
    /// a window that never stops asking for frames (PLAN §1).
    pub fn settle(&mut self, now: Instant) {
        if self.slide.is_some_and(|(_, tween)| tween.finished(now)) {
            self.slide = None;
        }
        for (fold_from, open) in self.fold_from.iter_mut().zip(&self.open) {
            if open.finished(now) {
                *fold_from = None;
            }
        }
        for spring in &mut self.springs {
            if spring.is_some_and(|spring| spring.finished(now)) {
                *spring = None;
            }
        }
    }

    /// The button went down on `which` at `x`, with the side pane drawn
    /// `drawn` points wide across `usable` points of panes.
    ///
    /// The hand takes hold of the pane as it *is*, which is not always as it
    /// is drawn. A folded pane is nothing, even while it is still on its way
    /// down — a press on it is a press on a closed pane, and it starts from
    /// nothing. An open one on its way open is the width it is opening to.
    /// And a stretch still springing back is left to finish on its own clock
    /// under the hand, so taking hold of a divider mid-spring never jumps it.
    pub fn press(&mut self, which: Divider, x: f32, drawn: f32, usable: f32, now: Instant) {
        self.settle(now);
        let side = which.side();
        let folded = self.panes.collapsed(side);
        let open = self.open[side.slot()].value(now);
        let width = if folded {
            0.0
        } else if open > 0.0 {
            drawn / open - self.spring_at(which.index(), now)
        } else {
            self.panes.ratio[side.index()] * usable
        };
        let at_rest = Reading {
            width,
            overshoot: 0.0,
            snapped: false,
            folded,
        };
        // What a pane still folding is drawn from, so taking hold of it does
        // not snap what is left of it away.
        let shown = Reading {
            width: match self.fold_from[side.slot()] {
                Some(share) if folded => share * usable,
                _ if folded => self.panes.reopen_share(side, &self.home) * usable,
                _ => width,
            },
            ..at_rest
        };
        // A divider taken hold of at home is held there from the start, or
        // the magnet would depend on how fast the hand moved: one quick pull
        // past the capture distance on the first frame would skip it.
        let at_home =
            !folded && (width - self.home.ratio[side.index()] * usable).abs() <= SNAP_CAPTURE;
        self.drag = Some(Drag {
            which,
            from_x: x,
            width_at_press: width,
            at_press: self.panes,
            live: self.panes,
            latch: Latch {
                snapped: at_home,
                folded,
            },
            reading: at_rest,
            shown,
            usable,
        });
    }

    /// The pointer is at `x` with the divider in the hand.
    pub fn drag_to(&mut self, x: f32, usable: f32, now: Instant) {
        let home = self.home;
        let Some(drag) = &mut self.drag else { return };
        let side = drag.which.side();
        let i = side.index();
        let pair = (drag.at_press.ratio[i] + drag.at_press.ratio[1]) * usable;
        let track = Track {
            minimum: drag.which.minimum(),
            maximum: pair - LIST_MIN,
            home: home.ratio[i] * usable,
        };
        let raw = drag.width_at_press + drag.which.direction() * (x - drag.from_x);
        let was_folded = drag.latch.folded;
        let reading = follow(&track, raw, &mut drag.latch);
        drag.reading = reading;
        drag.usable = usable;
        let mut live = drag.at_press;
        if reading.folded {
            live.collapse(side);
        } else {
            drag.shown = reading;
            // Home is kept as home's own share, not as a width divided back
            // out of it: a divider let go at home is *at* home, to the bit.
            let share = if reading.snapped {
                home.ratio[i]
            } else if usable > 0.0 {
                reading.width / usable
            } else {
                drag.at_press.ratio[i]
            };
            live.open_at(side, share);
        }
        drag.live = live;
        let fold_from = (usable > 0.0).then(|| drag.shown.width / usable);
        if reading.folded != was_folded {
            self.fold(side, reading.folded, now);
            // Folding, it goes down from where the hand left it.
            if reading.folded {
                self.fold_from[side.slot()] = fold_from;
            }
        }
    }

    /// The button came up. The widths it leaves are committed and handed
    /// back for the state file — `None` when nothing moved, so a click on a
    /// divider writes nothing — and a stretch or a lag springs back.
    pub fn release(&mut self, now: Instant) -> Option<Panes> {
        let drag = self.drag.take()?;
        self.let_go(&drag, now);
        let panes = drag.live.validated().unwrap_or(drag.at_press);
        self.panes = panes;
        (panes != drag.at_press).then_some(panes)
    }

    /// `Esc` with a divider in the hand: everything goes back to where the
    /// press found it, over the same slide a reset takes, and nothing is
    /// written.
    pub fn cancel(&mut self, now: Instant) {
        let Some(drag) = self.drag else { return };
        let open = self.openness(now);
        let from = self.fractions(open);
        self.drag = None;
        self.slide = Some((from, Tween::new(0.0, 1.0, FOLD, Easing::OutQuint, now)));
        self.let_go(&drag, now);
        let side = drag.which.side();
        if drag.at_press.collapsed(side) != drag.live.collapsed(side) {
            self.fold(side, drag.at_press.collapsed(side), now);
        }
    }

    /// Fold `side` away, or open it again — a double click on its divider, or
    /// the command. Refused while a divider is in the hand, which owns the
    /// widths until it is let go. Hands back the widths for the state file.
    pub fn toggle(&mut self, side: Side, now: Instant) -> Option<Panes> {
        if self.drag.is_some() {
            return None;
        }
        let folding = !self.panes.collapsed(side);
        if folding {
            self.panes.collapse(side);
        } else {
            self.panes.expand(side, &self.home);
        }
        self.fold(side, folding, now);
        Some(self.panes)
    }

    /// Every pane back to home, sliding there: the config's ratio, with both
    /// side panes open unless the ratio gives one nothing. A drag in the hand
    /// is let go of without committing anything — the reset is the more
    /// deliberate act — and a stretch it had springs back rather than
    /// vanishing.
    pub fn reset(&mut self, now: Instant) -> Panes {
        let open = self.openness(now);
        let from = self.fractions(open);
        if let Some(drag) = self.drag.take() {
            self.let_go(&drag, now);
        }
        self.panes = self.home;
        self.slide = Some((from, Tween::new(0.0, 1.0, FOLD, Easing::OutQuint, now)));
        for side in [Side::Parent, Side::Preview] {
            self.fold(side, self.home.collapsed(side), now);
        }
        self.panes
    }

    /// The stretch or lag a drag was drawn with, handed to the spring as the
    /// hand lets go — the drawing is the same on the frame after as on the
    /// frame before. Including for a pane the drag folded, which is still
    /// drawn with it while it folds; not for one folded all the way, where
    /// there is nothing left to see settle and the spring would be frames
    /// asked for nothing.
    fn let_go(&mut self, drag: &Drag, now: Instant) {
        let slot = drag.which.side().slot();
        let hidden = drag.reading.folded && self.open[slot].value(now) <= 0.0;
        if !hidden {
            self.spring_from(drag.which, drag.shown.overshoot, now);
        }
    }

    /// Start `side` folding (or opening) from wherever it is now, from the
    /// share it would open to — a drag that folds it says otherwise after.
    fn fold(&mut self, side: Side, folded: bool, now: Instant) {
        let k = side.slot();
        self.fold_from[k] = None;
        let to = if folded { 0.0 } else { 1.0 };
        let at = self.open[k].value(now);
        if at == to && self.open[k].to == to {
            return;
        }
        self.open[k] = Tween::new(at, to, FOLD, Easing::OutQuint, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        Track {
            minimum: 96.0,
            maximum: 800.0,
            home: 200.0,
        }
    }

    /// Walk a pointer through `raws`, keeping the latch between moves.
    fn walk(raws: &[f32]) -> Vec<Reading> {
        let mut latch = Latch::default();
        raws.iter()
            .map(|raw| follow(&track(), *raw, &mut latch))
            .collect()
    }

    /// Home captures at 14 points and lets go at 2.2 × that: the same
    /// pointer position is held or free depending on where it came from.
    #[test]
    fn home_captures_close_and_lets_go_far() {
        let r = walk(&[
            240.0, 220.0, 213.0, 225.0, 230.0, 231.0, 225.0, 215.0, 214.0,
        ]);
        assert!(!r[0].snapped && !r[1].snapped, "20 points is not close");
        assert!(r[2].snapped, "13 points is");
        assert_eq!(r[2].width, 200.0, "held at home");
        assert!(r[3].snapped && r[4].snapped, "held until 30.8");
        assert!(!r[5].snapped, "31 points out, free");
        assert_eq!(r[5].width, 231.0);
        assert!(
            !r[6].snapped && !r[7].snapped,
            "coming back, 25 and 15 are free"
        );
        assert!(r[8].snapped, "14 is close again");
    }

    /// While held, the divider lags on the gravity curve and reaches the
    /// pointer exactly at the escape boundary — so letting go is seamless.
    #[test]
    fn a_held_divider_catches_the_pointer_at_the_boundary() {
        assert!((gravity(SNAP_ESCAPE) - SNAP_ESCAPE).abs() < 1e-4);
        assert!((gravity(-SNAP_ESCAPE) + SNAP_ESCAPE).abs() < 1e-4);
        assert_eq!(gravity(0.0), 0.0);
        // Everywhere inside it trails the pointer, and never overtakes it.
        for tenth in 1..10 {
            let pull = SNAP_ESCAPE * tenth as f32 / 10.0;
            let shown = gravity(pull);
            assert!(shown > 0.0 && shown < pull, "{pull}: {shown}");
        }
        // Held at the very edge, the drawing is where a free divider would be.
        let mut latch = Latch {
            snapped: true,
            folded: false,
        };
        let edge = 200.0 + SNAP_ESCAPE - 0.01;
        let held = follow(&track(), edge, &mut latch);
        assert!(held.snapped);
        assert!((held.width + held.overshoot - edge).abs() < 0.02);
    }

    /// Past the minimum the divider stretches, never more than 24 points,
    /// and the stretch is `24 · tanh(overflow / 80)`.
    #[test]
    fn a_minimum_stretches_and_is_bounded() {
        let r = walk(&[80.0, 60.0, 57.0]);
        for reading in &r {
            assert_eq!(reading.width, 96.0, "the width stays at the minimum");
            assert!(reading.overshoot < 0.0 && reading.overshoot > -BAND_REACH);
        }
        // Further out is further stretched, and never past the reach.
        assert!(r[0].overshoot > r[1].overshoot && r[1].overshoot > r[2].overshoot);
        let mut last = 0.0;
        for overflow in [-1.0, -50.0, -500.0, -1e9] {
            let band = rubber_band(overflow);
            assert!(band <= last && band.abs() <= BAND_REACH, "{overflow}");
            last = band;
        }
        assert!(rubber_band(10.0) > 0.0, "signed like the overflow");
    }

    /// The side folds 40 points past its minimum and opens again only 24
    /// points back inside that — never at the same place both ways.
    #[test]
    fn a_side_pane_folds_past_its_minimum_with_hysteresis() {
        let r = walk(&[57.0, 55.0, 60.0, 79.0, 80.0, 81.0, 70.0, 55.9]);
        assert!(!r[0].folded, "57 is inside the fold at 56");
        assert!(r[1].folded, "55 is past it");
        assert_eq!(r[1].width, 0.0);
        assert!(r[2].folded && r[3].folded && r[4].folded, "held to 80");
        assert!(!r[5].folded, "81 opens it");
        assert_eq!(r[5].width, 96.0, "open, it is at its minimum…");
        assert!(r[5].overshoot < 0.0, "…stretched towards the pointer");
        assert!(!r[6].folded, "70 does not fold it again");
        assert!(r[7].folded, "past 56 does");
    }

    /// The list's minimum is a stop: no stretch, no fold, no snap.
    #[test]
    fn the_list_stops_the_divider_dead() {
        for raw in [800.0, 850.0, 5000.0] {
            let r = walk(&[raw]);
            assert_eq!(r[0].width, 800.0);
            assert_eq!(r[0].overshoot, 0.0);
            assert!(!r[0].folded && !r[0].snapped);
        }
    }

    const USABLE: f32 = 1376.0;

    fn dividers(now: Instant) -> Dividers {
        let home = Panes::from_ratio([1, 4, 3]);
        Dividers::new(home, home, now)
    }

    fn sums_to_one(fractions: [f32; 3]) -> bool {
        (fractions.iter().sum::<f32>() - 1.0).abs() < 1e-5
    }

    /// A drag trades width between the two panes either side of it and
    /// nothing else: on screen, and in what it leaves behind.
    #[test]
    fn a_drag_trades_only_between_its_neighbours() {
        let now = Instant::now();
        for which in [Divider::Left, Divider::Right] {
            let mut d = dividers(now);
            let far = match which {
                Divider::Left => 2,
                Divider::Right => 0,
            };
            let before = d.split(now).fractions;
            let width = before[which.side().index()] * USABLE;
            d.press(which, 500.0, width, USABLE, now);
            for x in [520.0, 600.0, 700.0, 450.0, 380.0] {
                d.drag_to(x, USABLE, now);
                let split = d.split(now);
                assert!(sums_to_one(split.fractions), "{which:?} {x}");
                assert_eq!(split.fractions[far], before[far], "{which:?} {x}");
            }
            let kept = d.release(now).expect("it moved");
            assert!(sums_to_one(kept.ratio));
            assert_eq!(kept.ratio[far], before[far]);
            assert_eq!(d.panes, kept);
        }
    }

    /// Let go at home, the share is home's to the bit, and the lag springs
    /// back over 300 ms and then stops asking for frames.
    #[test]
    fn let_go_at_home_it_is_home_exactly() {
        let t0 = Instant::now();
        let mut d = dividers(t0);
        let home = Panes::from_ratio([1, 4, 3]);
        // Pulled off home and brought back to 10 points short of it.
        let width = home.ratio[0] * USABLE;
        d.press(Divider::Left, 300.0, width, USABLE, t0);
        d.drag_to(400.0, USABLE, t0);
        d.drag_to(290.0, USABLE, t0);
        let split = d.split(t0);
        assert!(split.overshoot[0] < 0.0, "lagging behind the pointer");
        assert!(split.overshoot[0] > -10.0, "…but not all the way");
        let kept = d.release(t0);
        // Home is where it started, so nothing is written…
        assert_eq!(kept, None);
        assert_eq!(d.panes.ratio[0], home.ratio[0]);
        // …and the lag springs back, on its own clock.
        assert!(d.animating(t0));
        assert!(d.split(t0 + SPRING / 2).overshoot[0] != 0.0);
        assert!(!d.animating(t0 + SPRING));
        assert_eq!(d.split(t0 + SPRING).overshoot[0], 0.0);
        d.settle(t0 + SPRING);
        assert!(
            d.springs.iter().all(Option::is_none),
            "a spent spring is dropped"
        );

        // Snapped from somewhere else, it is kept as home's own share.
        let mut d = Dividers::new(
            Panes {
                ratio: [0.2, 0.425, 0.375],
                ..home
            }
            .validated()
            .expect("valid"),
            home,
            t0,
        );
        d.press(Divider::Left, 300.0, 0.2 * USABLE, USABLE, t0);
        d.drag_to(300.0 - (0.2 - 0.125) * USABLE + 5.0, USABLE, t0);
        let kept = d.release(t0).expect("it moved");
        assert_eq!(kept.ratio[0], home.ratio[0]);
    }

    /// Folding mid-drag keeps the drag: the pane folds, the hand comes back,
    /// it opens again, and only letting go writes anything.
    #[test]
    fn a_fold_keeps_the_drag_alive() {
        let t0 = Instant::now();
        let mut d = dividers(t0);
        let width = 0.375 * USABLE;
        d.press(Divider::Right, 900.0, width, USABLE, t0);
        // The preview's minimum is 180; the fold is 40 past it.
        let fold_x = 900.0 + (width - (PREVIEW_MIN - FOLD_PAST)) + 1.0;
        d.drag_to(fold_x, USABLE, t0);
        assert!(d.collapsed(Side::Preview), "folded as it goes");
        assert_eq!(d.dragging(), Some(Divider::Right), "and still held");
        assert!(!d.panes.preview_collapsed, "nothing committed yet");
        // Folding is animated, and starts from the width it was drawn at.
        assert!(d.animating(t0));
        assert!(d.split(t0).open[1] > 0.99);
        assert_eq!(d.split(t0 + FOLD).open[1], 0.0);
        // Back 30 points: open again, under the same hand.
        d.drag_to(fold_x - 30.0, USABLE, t0 + FOLD);
        assert!(!d.collapsed(Side::Preview));
        // …and out again, and let go folded.
        d.drag_to(fold_x + 10.0, USABLE, t0 + FOLD * 2);
        let kept = d.release(t0 + FOLD * 2).expect("it folded");
        assert!(kept.preview_collapsed);
        assert_eq!(kept.preview_before, 0.375, "it opens back to where it was");
        assert!(sums_to_one(kept.ratio));
        assert_eq!(kept.ratio[0], 0.125);
    }

    /// Taken hold of mid-animation, a pane is what it is becoming: a folding
    /// one is nothing to the hand (a press and a nudge do not open it) and
    /// is not snapped away, and a stretch still springing back goes on
    /// springing under the hand, from where it was drawn.
    #[test]
    fn a_press_takes_a_moving_pane_as_it_is() {
        let t0 = Instant::now();
        let mut d = dividers(t0);
        d.toggle(Side::Preview, t0);
        let mid = t0 + FOLD / 10;
        let drawn = d.split(mid).open[1] * 0.375 * USABLE;
        assert!(drawn > 100.0, "still well open: {drawn}");
        d.press(Divider::Right, 1300.0, drawn, USABLE, mid);
        assert!(
            (d.split(mid).open[1] * d.split(mid).fractions[2] * USABLE - drawn).abs() < 1e-2,
            "taking hold does not snap the folding pane away"
        );
        d.drag_to(1290.0, USABLE, mid);
        assert!(d.collapsed(Side::Preview), "ten points is not an unfold");
        assert_eq!(d.release(mid), None);

        // A stretch springing back, taken hold of on its way home.
        let mut d = dividers(t0);
        d.press(Divider::Left, 100.0, 0.125 * USABLE, USABLE, t0);
        d.drag_to(100.0 - (0.125 * USABLE - PARENT_MIN) - 30.0, USABLE, t0);
        d.release(t0);
        let half = t0 + SPRING / 8;
        let split = d.split(half);
        let drawn = PARENT_MIN + split.overshoot[0];
        assert!(drawn < PARENT_MIN, "still stretched: {drawn}");
        d.press(Divider::Left, 50.0, drawn, USABLE, half);
        d.drag_to(50.0, USABLE, half);
        let held = d.split(half);
        assert!(
            (held.fractions[0] * USABLE + held.overshoot[0] - drawn).abs() < 1e-2,
            "held where it was drawn"
        );
    }

    /// The window the divider tests' panes are laid out in: `USABLE` points
    /// of panes between the margins and the two gaps.
    fn drawn(split: &Split) -> crate::ui::Layout {
        let area = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(USABLE + 4.0 * crate::ui::GAP, 908.0),
        );
        crate::ui::layout(area, split, false, 1)
    }

    /// Let go while the pane it folded is still folding, the pane goes on
    /// folding from where it was drawn — its width and its stretch — rather
    /// than jumping to the share it will open back to.
    #[test]
    fn let_go_mid_fold_it_folds_on_from_where_it_was() {
        let t0 = Instant::now();
        for escape in [false, true] {
            let mut d = dividers(t0);
            let width = 0.375 * USABLE;
            d.press(Divider::Right, 900.0, width, USABLE, t0);
            // Into the stretch past the minimum first, so the pane folds from
            // its minimum less the band — nowhere near the share it had.
            let x_for = |raw: f32| 900.0 + (width - raw);
            d.drag_to(x_for(PREVIEW_MIN - 30.0), USABLE, t0);
            assert!(d.split(t0).overshoot[1] < -5.0, "stretched");
            d.drag_to(x_for(PREVIEW_MIN - FOLD_PAST - 1.0), USABLE, t0);
            assert!(d.collapsed(Side::Preview));
            let at = t0 + FOLD / 4;
            let before = drawn(&d.split(at)).preview.width();
            assert!(before > 1.0, "still folding: {before}");
            if escape {
                d.cancel(at);
            } else {
                d.release(at);
            }
            let after = drawn(&d.split(at)).preview.width();
            assert!(
                (after - before).abs() < 1.0,
                "escape {escape}: {before} → {after} on the frame it was let go"
            );
            let next = drawn(&d.split(at + Duration::from_millis(16)))
                .preview
                .width();
            if escape {
                assert!(next > after, "Esc opens it back up: {after} → {next}");
            } else {
                assert!(next < after, "it goes on folding: {after} → {next}");
                assert_eq!(d.panes.preview_before, 0.375, "and opens to its old share");
            }
        }
    }

    /// A divider taken hold of at home is held there from the first frame,
    /// however fast the hand goes: one 20-point pull is held with lag, one
    /// 31-point pull escapes.
    #[test]
    fn home_holds_a_quick_first_pull() {
        let t0 = Instant::now();
        let home = 0.125 * USABLE;
        let mut d = dividers(t0);
        d.press(Divider::Left, 300.0, home, USABLE, t0);
        d.drag_to(320.0, USABLE, t0);
        let held = d.split(t0);
        assert_eq!(held.fractions[0], 0.125, "held at home");
        assert!(
            held.overshoot[0] > 0.0 && held.overshoot[0] < 20.0,
            "lagging"
        );
        d.release(t0);

        let mut d = dividers(t0);
        d.press(Divider::Left, 300.0, home, USABLE, t0);
        d.drag_to(331.0, USABLE, t0);
        let free = d.split(t0);
        assert!((free.fractions[0] * USABLE - (home + 31.0)).abs() < 1e-2);
        assert_eq!(free.overshoot[0], 0.0);
    }

    /// A click on a divider that is springing back leaves the spring on its
    /// own clock: the drawing is what it would have been untouched, and the
    /// frames stop when the first spring would have stopped.
    #[test]
    fn a_click_mid_spring_does_not_restart_it() {
        let t0 = Instant::now();
        let mut d = dividers(t0);
        d.press(Divider::Left, 100.0, 0.125 * USABLE, USABLE, t0);
        d.drag_to(100.0 - (0.125 * USABLE - PARENT_MIN) - 30.0, USABLE, t0);
        d.release(t0);
        let untouched = d.clone();
        let mid = t0 + SPRING / 4;
        let drawn_at = |d: &Dividers, now: Instant| drawn(&d.split(now)).parent.width();
        let x = 100.0;
        d.press(Divider::Left, x, drawn_at(&d, mid), USABLE, mid);
        d.drag_to(x, USABLE, mid);
        assert_eq!(d.release(mid), None, "nothing moved");
        for later in [mid, t0 + SPRING / 2, t0 + SPRING * 3 / 4] {
            assert!(
                (drawn_at(&d, later) - drawn_at(&untouched, later)).abs() < 1e-3,
                "the spring was restarted"
            );
        }
        assert!(!d.animating(t0 + SPRING));
    }

    /// `Esc` puts back what the press found and commits nothing.
    #[test]
    fn escape_puts_everything_back() {
        let t0 = Instant::now();
        let mut d = dividers(t0);
        let before = d.panes;
        d.press(Divider::Left, 100.0, 0.125 * USABLE, USABLE, t0);
        d.drag_to(-20.0, USABLE, t0);
        assert!(d.collapsed(Side::Parent));
        d.cancel(t0);
        assert_eq!(d.dragging(), None);
        assert_eq!(d.panes, before);
        assert!(!d.collapsed(Side::Parent));
        assert!(d.animating(t0), "it slides back");
        let settled = d.split(t0 + FOLD);
        assert_eq!(settled.open, [1.0, 1.0]);
        assert!(sums_to_one(settled.fractions));
        assert!((settled.fractions[0] - 0.125).abs() < 1e-6);
    }

    /// A toggle folds and opens a side pane in the list's width, animated —
    /// but not out from under a divider in the hand — and a reset sends
    /// everything home and open, the hand's divider included.
    #[test]
    fn toggles_and_reset() {
        let t0 = Instant::now();
        let mut d = dividers(t0);
        let folded = d.toggle(Side::Preview, t0).expect("no drag");
        assert!(folded.preview_collapsed);
        assert!(d.animating(t0));
        assert!(!d.animating(t0 + FOLD));
        assert_eq!(d.split(t0 + FOLD).open[1], 0.0);
        let opened = d.toggle(Side::Preview, t0 + FOLD).expect("no drag");
        assert!(!opened.preview_collapsed);
        assert_eq!(opened.ratio, Panes::from_ratio([1, 4, 3]).ratio);

        d.toggle(Side::Parent, t0);
        d.press(Divider::Right, 900.0, 0.375 * USABLE, USABLE, t0);
        assert_eq!(d.toggle(Side::Preview, t0), None, "the hand owns it");
        d.drag_to(700.0, USABLE, t0);
        let home = d.reset(t0);
        assert_eq!(home, Panes::from_ratio([1, 4, 3]));
        assert_eq!(d.dragging(), None);
        let settled = d.split(t0 + FOLD);
        assert_eq!(settled.open, [1.0, 1.0]);
        assert_eq!(settled.fractions, home.ratio);
    }
}
