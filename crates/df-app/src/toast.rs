//! The one-at-a-time toast (PLAN §5), ported from delightviewer's `ui::toast`.
//!
//! Two rules carry the whole design. **One toast at a time, never stacked**: a
//! second message replaces the first, because a column of notices is a log, and
//! a log is something you read later rather than something you glance at.
//! **Lifetime by kind**: a plain notice is gone in a couple of seconds, an undo
//! offer stays as long as the offer is worth making, and a toast that describes
//! a *state* does not expire at all — a message that goes away while its subject
//! is still true has lied by leaving.
//!
//! The third rule is the one PLAN §1 cares about: **a resting toast schedules
//! exactly one deadline**. It slides in, then it is a still picture for seconds,
//! then it fades. The still stretch asks for nothing; [`Toasts::deadline`]
//! returns the single instant the fade begins, and only inside the two moving
//! stretches does [`Toasts::animating`] hold the frame rate up.

use std::time::{Duration, Instant};

use crate::hover::Hovers;
use crate::motion::Easing;
use crate::theme::mix;
use crate::ui::{Control, Painting};

/// A plain "that happened" — the yank, the shell that exited 0. There is
/// nothing to decide, so it leaves quickly (delightviewer's number, kept).
pub const NOTICE_LIFETIME: Duration = Duration::from_millis(2200);

/// An offer to take something back. PLAN §5: "each op lands with an undo toast
/// (8 s)". Long enough to notice the file count is wrong and reach for `u`.
pub const UNDO_LIFETIME: Duration = Duration::from_secs(8);

/// A question standing in the toast. Long enough to read and decide, short
/// enough that walking away is an answer — and the answer walking away gives is
/// always the one that changes nothing.
pub const CONFIRM_LIFETIME: Duration = Duration::from_secs(5);

/// Effectively forever: a sticky toast is taken down by whatever ends the state
/// it reports (a paused task resuming), never by the clock. A day is past any
/// session, and keeping it finite means nothing has to special-case "no end".
pub const STICKY_LIFETIME: Duration = Duration::from_secs(60 * 60 * 24);

/// The slide-and-fade in (`delightful-ui` §5: intros are slower than outros,
/// and almost never linear — this one is OutQuint, the plan's slide curve).
const RISE: Duration = Duration::from_millis(220);

/// How far the toast travels on its way in, in points. A nudge, not a flight:
/// `delightful-ui` §5's "animate the element, not the page".
const RISE_DISTANCE: f32 = 12.0;

/// The fade at the end of a toast's own life.
const FADE: Duration = Duration::from_millis(250);

/// How fast a *replaced* toast gets out of the way. Much quicker than its own
/// fade would have been: the new message is the one being read, and the old one
/// lingering over it would be two messages on screen, which is the thing this
/// module exists to prevent.
const REPLACE_FADE: Duration = Duration::from_millis(110);

/// The plate's height, and the space it keeps off the chrome below it.
const HEIGHT: f32 = 32.0;
const MARGIN: f32 = 12.0;

/// Padding inside the plate. The plate's radius is derived from it further down
/// (`delightful-ui` §15).
const PAD: f32 = 12.0;

/// Body text size, matching the rest of the chrome.
const FONT: f32 = 12.5;

/// The accent stripe down the toast's leading edge: the redundant channel that
/// says "error" without relying on the colour of the text alone.
const RULE_WIDTH: f32 = 2.5;

/// What kind of message this is, which is the same question as how long it
/// stays and what colour it wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Something happened and there is nothing to do about it.
    Notice,
    /// Something failed. Same clock as a notice; a different colour, and the
    /// message says what to do next.
    Error,
    /// Something happened that `u` can take back.
    Undo,
    /// A question being asked in the toast itself. Constructed by nothing yet:
    /// PLAN §5 names the four lifetimes as one set, and the surfaces that ask a
    /// question *in* the toast rather than in a dialog arrive with the drag and
    /// the mount manager. Kept so the set is complete and tested rather than
    /// re-derived later from a half-remembered number.
    #[allow(dead_code)]
    Confirm,
    /// A state that is true right now and will stay true until something ends
    /// it.
    Sticky,
}

impl ToastKind {
    pub fn lifetime(self) -> Duration {
        match self {
            ToastKind::Notice | ToastKind::Error => NOTICE_LIFETIME,
            ToastKind::Undo => UNDO_LIFETIME,
            ToastKind::Confirm => CONFIRM_LIFETIME,
            ToastKind::Sticky => STICKY_LIFETIME,
        }
    }

    fn sticky(self) -> bool {
        matches!(self, ToastKind::Sticky)
    }
}

/// One message on screen.
#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    born: Instant,
}

impl Toast {
    /// Alpha for the toast in its own life: fading up as it arrives, down as it
    /// expires, 1 through the long middle.
    fn alpha(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.born);
        let lifetime = self.kind.lifetime();
        if age >= lifetime {
            return 0.0;
        }
        let rising = (age.as_secs_f32() / RISE.as_secs_f32()).min(1.0);
        let remaining = lifetime.saturating_sub(age);
        let leaving = if self.kind.sticky() || remaining > FADE {
            1.0
        } else {
            remaining.as_secs_f32() / FADE.as_secs_f32()
        };
        rising.min(leaving)
    }

    /// How far below its resting place the toast still is, in points.
    fn rise_offset(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.born);
        if age >= RISE {
            return 0.0;
        }
        let t = age.as_secs_f32() / RISE.as_secs_f32();
        RISE_DISTANCE * (1.0 - Easing::OutQuint.apply(t))
    }

    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.born) >= self.kind.lifetime()
    }

    /// True only while something about this toast is actually changing.
    fn animating(&self, now: Instant) -> bool {
        let age = now.saturating_duration_since(self.born);
        if age < RISE {
            return true;
        }
        if self.kind.sticky() {
            return false;
        }
        let lifetime = self.kind.lifetime();
        age < lifetime && lifetime.saturating_sub(age) <= FADE
    }

    /// The one instant a still toast needs the loop woken for: the frame its
    /// fade begins. `None` for a sticky toast, which has no clock, and for one
    /// already fading, which [`Toast::animating`] is carrying.
    fn fade_starts_at(&self, now: Instant) -> Option<Instant> {
        if self.kind.sticky() {
            return None;
        }
        let at = self.born + self.kind.lifetime().saturating_sub(FADE);
        (at > now).then_some(at)
    }
}

/// The toast surface: what is up, and what is on its way out.
#[derive(Debug, Default)]
pub struct Toasts {
    current: Option<Toast>,
    /// The toast a replacement pushed off screen, and when it was pushed. Kept
    /// only for [`REPLACE_FADE`], so the swap is a hand-off rather than a cut.
    leaving: Option<(Toast, Instant)>,
}

impl Toasts {
    pub fn new() -> Toasts {
        Toasts::default()
    }

    /// Put `message` up, replacing whatever was there.
    pub fn show(&mut self, message: impl Into<String>, kind: ToastKind, now: Instant) {
        if let Some(previous) = self.current.take() {
            // Only a toast that had actually arrived is worth animating out; one
            // replaced mid-rise never got read, and sliding it away would draw
            // the eye to the message that is *not* the current one.
            if !previous.expired(now) {
                self.leaving = Some((previous, now));
            }
        }
        self.current = Some(Toast {
            message: message.into(),
            kind,
            born: now,
        });
    }

    pub fn notice(&mut self, message: impl Into<String>, now: Instant) {
        self.show(message, ToastKind::Notice, now);
    }

    pub fn error(&mut self, message: impl Into<String>, now: Instant) {
        self.show(message, ToastKind::Error, now);
    }

    /// An operation landed and `u` can take it back (PLAN §5).
    pub fn undo(&mut self, message: impl Into<String>, now: Instant) {
        self.show(message, ToastKind::Undo, now);
    }

    /// A question asked in the toast rather than in a dialog. See
    /// [`ToastKind::Confirm`] on why it is here before it has a caller.
    #[allow(dead_code)]
    pub fn confirm(&mut self, message: impl Into<String>, now: Instant) {
        self.show(message, ToastKind::Confirm, now);
    }

    pub fn sticky(&mut self, message: impl Into<String>, now: Instant) {
        self.show(message, ToastKind::Sticky, now);
    }

    /// Take down a sticky toast, if that is what is up. For the caller whose
    /// state ended without a message of its own.
    pub fn clear_sticky(&mut self, now: Instant) -> bool {
        if self.current.as_ref().is_some_and(|t| t.kind.sticky()) {
            if let Some(previous) = self.current.take() {
                self.leaving = Some((previous, now));
            }
            return true;
        }
        false
    }

    /// Take everything down at once. For the caller that is leaving the screen
    /// the toast is about — a tab switch, a quit.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.current = None;
        self.leaving = None;
    }

    /// What is up, for the tests and for anything that has to know whether a
    /// message is already saying what it was about to say.
    #[allow(dead_code)]
    pub fn current(&self) -> Option<&Toast> {
        self.current.as_ref()
    }

    #[allow(dead_code)]
    pub fn is_showing(&self) -> bool {
        self.current.is_some()
    }

    /// Retire whatever has run out. Called once a frame, before the paint, so
    /// the expiry decision is made in one place rather than inside the painter.
    pub fn tick(&mut self, now: Instant) {
        if self.current.as_ref().is_some_and(|t| t.expired(now)) {
            self.current = None;
        }
        if self
            .leaving
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= REPLACE_FADE)
        {
            self.leaving = None;
        }
    }

    /// Is anything moving? (PLAN §1's idle-cost rule: a toast standing still is
    /// not.)
    pub fn animating(&self, now: Instant) -> bool {
        self.leaving.is_some()
            || self
                .current
                .as_ref()
                .is_some_and(|toast| toast.animating(now))
    }

    /// The single wake-up a resting toast asks for.
    pub fn deadline(&self, now: Instant) -> Option<Duration> {
        let toast = self.current.as_ref()?;
        toast
            .fade_starts_at(now)
            .map(|at| at.saturating_duration_since(now))
    }

    /// Where the live toast is, for the hit test.
    ///
    /// The one that is *leaving* is not offered. It is pixels on their way out
    /// of the picture, and a click landing on a message that has already been
    /// replaced would act on something nobody is reading any more — the same
    /// rule the yank chip's fade follows in the top row's hit test.
    pub fn geometry(
        &self,
        painter: &egui::Painter,
        area: egui::Rect,
        bottom: f32,
        now: Instant,
    ) -> Option<ToastGeom> {
        let toast = self.current.as_ref()?;
        // Nothing invisible takes a click. The hit test runs before
        // [`Toasts::tick`] retires what has run out, so without this an expired
        // toast would be clickable for the one frame between the two — and on
        // an undo toast that frame would take an offer that had just closed.
        if toast.alpha(now) <= 0.0 {
            return None;
        }
        Some(measure(
            painter,
            area,
            bottom,
            toast,
            toast.rise_offset(now),
        ))
    }

    /// Take the current toast down, whatever it was. The pointer's way of
    /// waiting for the clock.
    ///
    /// **Through the leaving slot, not straight to `None`.** A click on the
    /// plate used to delete it between one frame and the next, so the one
    /// acknowledgement the press had — the ripple the click router spawns for
    /// it — was drawn on a surface that no longer existed, and the message
    /// vanished with no sign that the click was what did it. It sinks and fades
    /// over [`REPLACE_FADE`] instead, which is the same going-away a replaced
    /// message gets and is over inside an eighth of a second: long enough to
    /// read as "that press did this", too short to be in the way of whatever
    /// the hand is doing next.
    pub fn dismiss(&mut self, now: Instant) {
        if let Some(toast) = self.current.take() {
            self.leaving = Some((toast, now));
        }
    }

    /// Draw the toast, bottom-centred above `bottom`.
    ///
    /// `hint` is the keystroke line an undo toast carries ("u — undo"), drawn
    /// as a chip on the right so the offer is a key you can see rather than a
    /// sentence you have to finish reading.
    ///
    /// `hovers` is the window's, so the plate can say it takes a click — it
    /// does: one dismisses it, and on an undo toast one on the chip takes the
    /// offer (PLAN §5, §7.5). A toast that is *leaving* is drawn cold, because
    /// it is not hit-tested and a lit plate under a pointer that cannot press
    /// it would be a lie.
    pub fn paint(
        &self,
        paint: &Painting<'_>,
        area: egui::Rect,
        bottom: f32,
        hovers: &Hovers<Control>,
        now: Instant,
    ) {
        if let Some((toast, at)) = &self.leaving {
            let t = now.saturating_duration_since(*at).as_secs_f32() / REPLACE_FADE.as_secs_f32();
            // Quad ease-in on the way out (`delightful-ui` §5), and it sinks
            // back down the way it came in.
            let alpha = (1.0 - t.clamp(0.0, 1.0)).powi(2);
            self.plate(
                paint,
                area,
                bottom,
                toast,
                alpha,
                RISE_DISTANCE * t,
                (0.0, 0.0),
            );
        }
        if let Some(toast) = &self.current {
            let alpha = toast.alpha(now);
            let offset = toast.rise_offset(now);
            let warm = (
                hovers
                    .hover(Control::Toast)
                    .max(hovers.hover(Control::ToastAction)),
                hovers.hover(Control::ToastAction),
            );
            self.plate(paint, area, bottom, toast, alpha, offset, warm);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn plate(
        &self,
        paint: &Painting<'_>,
        area: egui::Rect,
        bottom: f32,
        toast: &Toast,
        alpha: f32,
        drop: f32,
        // How lit the plate is, and how lit its offer chip is.
        (warm, action_warm): (f32, f32),
    ) {
        if alpha <= 0.0 {
            return;
        }
        let painter = paint.painter;
        let palette = paint.palette;
        let accent = match toast.kind {
            ToastKind::Error => palette.red,
            ToastKind::Undo => palette.yellow,
            ToastKind::Confirm => palette.peach,
            ToastKind::Sticky => palette.sky,
            ToastKind::Notice => palette.blue,
        };
        let hint = hint_of(toast);

        let fade = |c: egui::Color32| {
            egui::Color32::from_rgba_unmultiplied(
                c.r(),
                c.g(),
                c.b(),
                (alpha.clamp(0.0, 1.0) * c.a() as f32).round() as u8,
            )
        };

        let rect = measure(painter, area, bottom, toast, drop).rect;
        let message = painter.layout_no_wrap(
            fitted_message(painter, toast, rect.width()),
            egui::FontId::proportional(FONT),
            palette.text,
        );

        // The same plate the which-key card and the help sheet use, so every
        // floating thing in delightfile is visibly one surface. Under the
        // pointer it lifts towards its own accent — the plate is a button, and
        // the one thing it must not look like is a label.
        let plate = mix(palette.crust, palette.surface0, warm);
        painter.rect_filled(rect, TOAST_RADIUS, fade(plate));
        painter.rect_stroke(
            rect,
            TOAST_RADIUS,
            egui::Stroke::new(1.0, fade(mix(palette.surface1, accent, 0.35 + warm * 0.4))),
            egui::StrokeKind::Inside,
        );
        // The leading rule, inset to the corner radius so it stops where the
        // corner starts turning rather than being clipped square.
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + 1.0, rect.top() + TOAST_RADIUS as f32),
                egui::pos2(
                    rect.left() + 1.0 + RULE_WIDTH,
                    rect.bottom() - TOAST_RADIUS as f32,
                ),
            ),
            1,
            fade(accent),
        );

        let text_color = if toast.kind == ToastKind::Error {
            palette.red
        } else {
            palette.text
        };
        let inside = painter.with_clip_rect(rect);
        inside.galley(
            egui::pos2(
                rect.left() + RULE_WIDTH + PAD,
                rect.center().y - message.size().y / 2.0,
            ),
            message,
            fade(text_color),
        );
        if let Some((key, what)) = hint {
            let what_w = text_width(&inside, what, egui::FontId::proportional(FONT));
            // The offer, lit on its own when the pointer is on it rather than
            // merely on the plate: two targets, said apart.
            let what_x = rect.right() - PAD - what_w;
            inside.text(
                egui::pos2(what_x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                what,
                egui::FontId::proportional(FONT),
                fade(mix(palette.overlay1, palette.text, action_warm)),
            );
            inside.text(
                egui::pos2(what_x - 6.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                key,
                egui::FontId::monospace(FONT),
                fade(palette.yellow),
            );
        }
    }
}

/// Where a toast is on screen, and where the offer inside it is.
///
/// Measured by one function and read by two — the painter and the hit test —
/// for the reason the breadcrumb's rects are: a click has to land on the thing
/// it looks like it landed on, and two copies of this arithmetic is how that
/// stops being true.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToastGeom {
    pub rect: egui::Rect,
    /// The `u undo` chip at the trailing edge, on the kind of toast that has
    /// one. Clicking it takes the offer.
    pub action: Option<egui::Rect>,
}

/// The offer an undo toast carries, as a key and the word for what it does.
fn hint_of(toast: &Toast) -> Option<(&'static str, &'static str)> {
    matches!(toast.kind, ToastKind::Undo).then_some(("u", "undo"))
}

fn measure(
    painter: &egui::Painter,
    area: egui::Rect,
    bottom: f32,
    toast: &Toast,
    drop: f32,
) -> ToastGeom {
    let hint = hint_of(toast);
    let message = text_width(painter, &toast.message, egui::FontId::proportional(FONT));
    let hint_width = hint_width(painter, toast);
    // Never wider than the window: a message longer than that loses its
    // middle to `…` where it is drawn ([`fitted_message`]), and the offer at
    // the trailing edge stays on the plate.
    let width = (message + hint_width + PAD * 2.0 + RULE_WIDTH).min(area.width() - MARGIN * 2.0);
    let rect = egui::Rect::from_center_size(
        egui::pos2(area.center().x, bottom - MARGIN - HEIGHT / 2.0 + drop),
        egui::vec2(width, HEIGHT),
    );
    // The chip is the two words at the trailing edge and the padding around
    // them — a target the size of what is drawn, not a hairline around the
    // glyphs.
    let action = hint.map(|_| {
        egui::Rect::from_min_max(
            egui::pos2(rect.right() - hint_width - PAD / 2.0, rect.top()),
            egui::pos2(rect.right(), rect.bottom()),
        )
    });
    ToastGeom { rect, action }
}

/// How wide the offer at a toast's trailing edge is, with the padding before
/// it: nothing, on a toast that makes none.
fn hint_width(painter: &egui::Painter, toast: &Toast) -> f32 {
    hint_of(toast)
        .map(|(key, what)| {
            text_width(painter, key, egui::FontId::monospace(FONT))
                + 6.0
                + text_width(painter, what, egui::FontId::proportional(FONT))
                + PAD
        })
        .unwrap_or(0.0)
}

/// The message as it fits a plate `width` wide: whole when it does, and
/// otherwise with its middle taken out and `…` in its place
/// ([`crate::chrome::elide_middle`]). The end is where a message names what
/// it was about — the file, the folder, the count — and a message cut at its
/// end would lose exactly that; cut in the middle, it keeps both its ends.
fn fitted_message(painter: &egui::Painter, toast: &Toast, width: f32) -> String {
    let room = width - RULE_WIDTH - PAD * 2.0 - hint_width(painter, toast);
    crate::chrome::elide_middle(
        painter,
        &toast.message,
        egui::FontId::proportional(FONT),
        room.max(0.0),
    )
}

/// The plate's radius — [`crate::chrome::CARD_RADIUS`], the same as every
/// other floating plate in the window.
///
/// It used to be `ROW_RADIUS + PAD` (18), a concentric derivation
/// (`delightful-ui` §15) with nothing to be concentric *with*: no rounded child
/// is ever drawn inside a toast, and `PAD` is the horizontal text inset rather
/// than a gap around a nested row. So the number was two points off the card
/// radius for no reason, while the code beside it claimed the toast was the
/// same surface as the which-key card and the help sheet. Now it is.
const TOAST_RADIUS: u8 = crate::chrome::CARD_RADIUS;

fn text_width(painter: &egui::Painter, text: &str, font: egui::FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_string(), font, egui::Color32::WHITE)
        .size()
        .x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plate the pointer is offered is the plate that was drawn, and the
    /// offer chip is inside it — only on the kind of toast that makes one.
    #[test]
    fn the_toast_offers_the_pointer_the_plate_it_draws() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
            let t0 = Instant::now();
            let mut toasts = Toasts::new();
            assert!(
                toasts
                    .geometry(ui.painter(), area, area.bottom(), t0)
                    .is_none(),
                "nothing up, nothing to point at"
            );

            toasts.notice("Yanked 3 items", t0);
            assert!(
                toasts
                    .geometry(ui.painter(), area, area.bottom(), t0)
                    .is_none(),
                "the first instant is fully transparent, so it takes no click"
            );
            // Past the rise, so the plate is at rest and the geometry is the
            // resting one rather than a frame of the slide.
            let settled = t0 + RISE;
            let plain = toasts
                .geometry(ui.painter(), area, area.bottom(), settled)
                .expect("a toast is up");
            assert!(plain.action.is_none(), "a notice has nothing to offer");
            assert!(area.contains(plain.rect.center()));
            assert!(plain.rect.bottom() <= area.bottom());

            toasts.undo("Copied 3 items", t0);
            let offered = toasts
                .geometry(ui.painter(), area, area.bottom(), settled)
                .expect("a toast is up");
            let action = offered.action.expect("an undo toast offers `u`");
            assert!(offered.rect.contains(action.center()));
            assert!(action.right() <= offered.rect.right() + 1e-3);

            // …and a dismissed toast is gone for the pointer at once, even
            // though the plate is still fading out under it: what is leaving is
            // pixels on their way out and takes no clicks (see
            // [`Toasts::geometry`]).
            toasts.dismiss(settled);
            assert!(toasts.current().is_none());
            assert!(toasts
                .geometry(ui.painter(), area, area.bottom(), settled)
                .is_none());
            assert!(
                toasts.animating(settled),
                "the plate sinks away rather than cutting, so the click is acknowledged"
            );
            toasts.tick(settled + REPLACE_FADE);
            assert!(
                !toasts.animating(settled + REPLACE_FADE),
                "and then it rests"
            );
        });
    }

    /// A message longer than a narrow window loses its middle, not its end:
    /// what is drawn fits the plate beside the offer, keeps its last word,
    /// and the plate and its `u undo` stay inside the window.
    #[test]
    fn a_long_message_loses_its_middle_and_keeps_its_last_word() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let narrow = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 600.0));
            let t0 = Instant::now();
            let mut toasts = Toasts::new();
            let message = "Moved 1,204 files from the camera card into the folder called Holiday";
            toasts.undo(message, t0);
            let geometry = toasts
                .geometry(painter, narrow, narrow.bottom(), t0 + RISE)
                .expect("a toast is up");
            assert!(narrow.contains_rect(geometry.rect), "{:?}", geometry.rect);
            let action = geometry.action.expect("an undo toast offers `u`");
            assert!(
                geometry.rect.contains_rect(action),
                "the offer is on the plate"
            );

            let toast = toasts.current().expect("the toast");
            let shown = fitted_message(painter, toast, geometry.rect.width());
            assert_ne!(shown, message, "the window is too narrow for all of it");
            assert!(shown.contains('…'), "{shown}");
            assert!(shown.ends_with("Holiday"), "the last word went: {shown}");
            assert!(shown.starts_with("Moved"), "{shown}");
            let room = geometry.rect.width() - RULE_WIDTH - PAD * 2.0 - hint_width(painter, toast);
            assert!(text_width(painter, &shown, egui::FontId::proportional(FONT)) <= room);

            // …and a message that fits is drawn whole.
            let wide = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 600.0));
            let geometry = toasts
                .geometry(painter, wide, wide.bottom(), t0 + RISE)
                .expect("a toast is up");
            assert_eq!(
                fitted_message(painter, toast, geometry.rect.width()),
                message
            );
        });
    }

    #[test]
    fn one_toast_at_a_time_and_the_newest_wins() {
        let t0 = Instant::now();
        let mut toasts = Toasts::new();
        toasts.notice("Yanked 3 items", t0);
        assert_eq!(
            toasts.current().map(|t| t.message.as_str()),
            Some("Yanked 3 items")
        );

        // The replacement takes over immediately; the old one is only on screen
        // long enough to get out of the way.
        let t1 = t0 + Duration::from_millis(500);
        toasts.undo("Copied 3 items — u to undo", t1);
        assert_eq!(
            toasts.current().map(|t| t.kind),
            Some(ToastKind::Undo),
            "the newest message is the current one"
        );
        assert!(toasts.leaving.is_some(), "the replaced toast animates out");
        assert!(toasts.animating(t1));

        toasts.tick(t1 + REPLACE_FADE);
        assert!(toasts.leaving.is_none(), "and then it is gone");
        assert!(toasts.is_showing());
    }

    /// PLAN §1: the seconds an undo toast is up cost one wake-up, not a stream
    /// of frames.
    #[test]
    fn a_resting_toast_schedules_exactly_one_deadline() {
        let t0 = Instant::now();
        let mut toasts = Toasts::new();
        toasts.undo("Trashed 3 items", t0);

        // Rising: moving, so the frame rate carries it and no deadline is owed
        // beyond the next frame.
        assert!(toasts.animating(t0 + Duration::from_millis(50)));

        // Resting: still, and asking for exactly the instant the fade begins.
        let resting = t0 + Duration::from_secs(2);
        assert!(!toasts.animating(resting));
        let due = toasts.deadline(resting).expect("one deadline");
        let expected = UNDO_LIFETIME - FADE - Duration::from_secs(2);
        assert!(
            due.abs_diff(expected) < Duration::from_millis(5),
            "woken at {due:?}, wanted {expected:?}"
        );

        // Fading: moving again, and nothing further to schedule.
        let fading = t0 + UNDO_LIFETIME - FADE / 2;
        assert!(toasts.animating(fading));
        assert!(toasts.deadline(fading).is_none());

        // Expired: gone, and idle.
        toasts.tick(t0 + UNDO_LIFETIME);
        assert!(!toasts.is_showing());
        assert!(!toasts.animating(t0 + UNDO_LIFETIME));
        assert_eq!(toasts.deadline(t0 + UNDO_LIFETIME), None);
    }

    /// Lifetimes are the plan's, by kind, and the sticky one does not keep a
    /// clock at all.
    #[test]
    fn each_kind_keeps_its_own_clock() {
        assert_eq!(ToastKind::Notice.lifetime(), NOTICE_LIFETIME);
        assert_eq!(ToastKind::Undo.lifetime(), UNDO_LIFETIME);
        assert_eq!(ToastKind::Confirm.lifetime(), CONFIRM_LIFETIME);

        let t0 = Instant::now();
        let mut toasts = Toasts::new();
        toasts.sticky("Copy paused — p to resume", t0);
        let much_later = t0 + NOTICE_LIFETIME * 10;
        toasts.tick(much_later);
        assert!(toasts.is_showing(), "a state does not expire");
        assert!(!toasts.animating(much_later), "and does not spin the loop");
        assert_eq!(toasts.deadline(much_later), None);
        assert!(toasts.clear_sticky(much_later));
        assert!(!toasts.is_showing());

        // …and clear_sticky is for sticky toasts alone.
        toasts.notice("Copied 1 item", much_later);
        assert!(!toasts.clear_sticky(much_later));
        assert!(toasts.is_showing());
    }

    /// The rise is a real intro: it starts below and lands, on the plan's
    /// slide curve, and it is over inside RISE.
    #[test]
    fn the_toast_slides_in_from_below_and_settles() {
        let t0 = Instant::now();
        let mut toasts = Toasts::new();
        toasts.notice("Yanked 1 item", t0);
        let toast = toasts.current().expect("a toast");
        assert!((toast.rise_offset(t0) - RISE_DISTANCE).abs() < 1e-3);
        assert!(toast.alpha(t0) < 0.01, "it fades in as it rises");
        let mid = toast.rise_offset(t0 + RISE / 2);
        assert!(
            mid < RISE_DISTANCE / 2.0,
            "OutQuint covers most of the distance early: {mid}"
        );
        assert_eq!(toast.rise_offset(t0 + RISE), 0.0);
        assert!((toast.alpha(t0 + RISE) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn toasts_paint_without_panicking() {
        let ctx = egui::Context::default();
        let now = Instant::now();
        let mut toasts = Toasts::new();
        toasts.notice("Yanked 3 items", now);
        toasts.undo(
            "Trashed 3 items — u to undo",
            now + Duration::from_millis(80),
        );
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let paint = Painting {
                tips: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now,
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            toasts.paint(
                &paint,
                area,
                860.0,
                &Hovers::new(),
                now + Duration::from_millis(120),
            );
            // …and a very narrow window, where the plate has to be clamped.
            let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 200.0));
            toasts.paint(
                &paint,
                narrow,
                180.0,
                &Hovers::new(),
                now + Duration::from_millis(120),
            );
        });
    }
}
