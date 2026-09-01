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

use crate::motion::Easing;
use crate::theme::mix;
use crate::ui::Painting;

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

    /// Draw the toast, bottom-centred above `bottom`.
    ///
    /// `hint` is the keystroke line an undo toast carries ("u — undo"), drawn
    /// as a chip on the right so the offer is a key you can see rather than a
    /// sentence you have to finish reading.
    pub fn paint(&self, paint: &Painting<'_>, area: egui::Rect, bottom: f32, now: Instant) {
        if let Some((toast, at)) = &self.leaving {
            let t = now.saturating_duration_since(*at).as_secs_f32() / REPLACE_FADE.as_secs_f32();
            // Quad ease-in on the way out (`delightful-ui` §5), and it sinks
            // back down the way it came in.
            let alpha = (1.0 - t.clamp(0.0, 1.0)).powi(2);
            self.plate(paint, area, bottom, toast, alpha, RISE_DISTANCE * t, now);
        }
        if let Some(toast) = &self.current {
            let alpha = toast.alpha(now);
            let offset = toast.rise_offset(now);
            self.plate(paint, area, bottom, toast, alpha, offset, now);
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
        _now: Instant,
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
        let hint = matches!(toast.kind, ToastKind::Undo).then_some(("u", "undo"));

        let fade = |c: egui::Color32| {
            egui::Color32::from_rgba_unmultiplied(
                c.r(),
                c.g(),
                c.b(),
                (alpha.clamp(0.0, 1.0) * c.a() as f32).round() as u8,
            )
        };

        let message = painter.layout_no_wrap(
            toast.message.clone(),
            egui::FontId::proportional(FONT),
            palette.text,
        );
        let hint_width = hint
            .map(|(key, what)| {
                text_width(painter, key, egui::FontId::monospace(FONT))
                    + 6.0
                    + text_width(painter, what, egui::FontId::proportional(FONT))
                    + PAD
            })
            .unwrap_or(0.0);
        let width = (message.size().x + hint_width + PAD * 2.0 + RULE_WIDTH)
            .min(area.width() - MARGIN * 2.0);
        let rect = egui::Rect::from_center_size(
            egui::pos2(area.center().x, bottom - MARGIN - HEIGHT / 2.0 + drop),
            egui::vec2(width, HEIGHT),
        );

        // The same plate the which-key card and the help sheet use, so every
        // floating thing in delightfile is visibly one surface.
        let plate = palette.crust;
        painter.rect_filled(rect, TOAST_RADIUS, fade(plate));
        painter.rect_stroke(
            rect,
            TOAST_RADIUS,
            egui::Stroke::new(1.0, fade(mix(palette.surface1, accent, 0.35))),
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
            let what_x = rect.right() - PAD - what_w;
            inside.text(
                egui::pos2(what_x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                what,
                egui::FontId::proportional(FONT),
                fade(palette.overlay1),
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
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now,
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            toasts.paint(&paint, area, 860.0, now + Duration::from_millis(120));
            // …and a very narrow window, where the plate has to be clamped.
            let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 200.0));
            toasts.paint(&paint, narrow, 180.0, now + Duration::from_millis(120));
        });
    }
}
