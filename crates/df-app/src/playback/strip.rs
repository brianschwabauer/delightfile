//! The position strip: the only chrome a playing file gets (PLAN §4.3, §8).
//!
//! One line across the bottom of the preview pane — play state, timecode, a
//! position bar, and the shuttle rate when it is not 1× — over an **eased**
//! scrim, because the thing behind it is an arbitrary frame of video and white
//! text on an arbitrary frame is unreadable.
//!
//! **It is delightviewer's transport strip**, ported rather than approximated:
//! the same 30-point line, the same 12-point proportional type with the elapsed
//! time at the playhead's end of the track and the duration at the other, the
//! same scrim curve to the constant, the same drawn mute and loop badges, and
//! the same Range for a position bar — two rounded track segments with a notch
//! either side of a pill handle that grows under the pointer, grows a halo while
//! it is dragged, and stretches on a rubber band past either end of the track.
//! delightviewer keeps all of that in its own binary crate (`dlv-app::ui::
//! transport`), tangled with its edit document and its chapter list, so there
//! was nothing to vendor the way the `dv-*` crates were vendored; what is here
//! is the module ported to this program's palette, its `Painting`, and its
//! preview pane's geometry.
//!
//! Four things here are load-bearing and none of them is decoration.
//!
//! **The position bar is a control.** It reported a position and nothing else
//! until this commit. Now a press on the track seeks, the drag stays captured
//! until the release wherever it wanders, the hover shows the time the click
//! would land on, and the play glyph is a button that toggles playback. The
//! *geometry* of all three is reported back from [`paint`] as [`Hits`], because
//! the strip lays itself out against the width its own timecodes came out —
//! a hit test that computed that a second time is the second answer to a
//! question with one right answer.
//!
//! **The scrim is a raised cosine, not a linear ramp** (`delightful-ui` §14,
//! PLAN §8). A straight interpolation from opaque to clear has a corner at each
//! end — the alpha falls at a constant rate and then abruptly does not — and on
//! a flat sky or a plain wall the eye finds exactly that corner as a line ruled
//! across the picture. A raised cosine has zero slope at *both* ends, and the
//! bias pushes the darkness back toward the edge so the ramp can be long
//! without greying the middle of the frame. Ported from delightviewer's
//! `ui::scrim`, bias and all.
//!
//! **It is one contiguous mesh.** The flat part under the text is the first
//! band of the same mesh rather than a `rect_filled` beneath it: epaint
//! feathers a rect's edge over about a pixel while a mesh's edge is hard, and
//! where the two met a row of pixels could be covered by neither — a bright
//! hairline ruled across the video at whatever height the strip happened to
//! sit. There is no boundary to fall between now.
//!
//! **It lingers, then leaves.** [`LINGER`] at full strength after the last
//! transport or pointer activity, then [`FADE`] of eased fade — and exactly one
//! scheduled wake-up in between (`Player::strip_deadline`), never a poll.

use std::time::{Duration, Instant};

use crate::motion::Easing;
use crate::ui::Painting;

use super::TransportState;

/// How long the strip stays up after the last transport activity.
///
/// PLAN §8's "transient chrome holds ~2.5–3 s", and delightviewer's
/// `STRIP_LINGER` to the millisecond — the two programs hand files to each
/// other and their chrome should not disagree about how long a thing stays.
pub const LINGER: Duration = Duration::from_millis(2500);

/// …and then leaves over this. PLAN §8's 500 ms: the strip is a whole line of
/// chrome over a moving picture, and anything faster reads as a flicker of the
/// video rather than as the controls getting out of the way.
pub const FADE: Duration = Duration::from_millis(500);

/// The strip's height, in logical points: one line of type with room over and
/// under it, and — now that the position bar is a control — with room for a
/// handle standing across it. delightviewer's `HEIGHT`.
const HEIGHT: f32 = 30.0;

/// The position bar's thickness at rest. The played side runs thicker and both
/// sides grow under the pointer; see the Range in [`paint`].
const BAR_HEIGHT: f32 = 3.0;

/// The gap between the strip's pieces.
const GAP: f32 = 10.0;

/// How far the strip is inset from the pane's content box at either end.
///
/// delightviewer's `MARGIN` is 16, measured against a whole window; the pane
/// here is a third of one and already inset from its own frame, so the inset
/// that reads the same is the strip's own gap.
const MARGIN: f32 = GAP;

/// The strip's type size. A shade under the row font: a timecode is a readout,
/// not content. delightviewer's `FONT_SIZE`, and — like it — **proportional**:
/// a monospace timecode in a line of proportional chrome reads as a different
/// program's widget, and the digits do not need the column anyway when the
/// number they are in is pinned to one end of the strip.
const FONT: f32 = 12.0;

/// How far past the strip the scrim keeps fading, in points.
///
/// Long on purpose: a gentle curve needs further to get to nothing, and the
/// last quarter of this is under three units of alpha. delightviewer's
/// `SCRIM_FADE` exactly — this used to be scaled down "for a pane a third of
/// the window wide", which shortened the one curve the two programs were
/// supposed to share and put back a hint of the corner it exists to remove.
const SCRIM_FADE: f32 = 132.0;

/// Bias on the falloff. 1 is a plain raised cosine; higher pulls the darkness
/// toward the edge, so the ramp can be long without washing out the picture.
const SCRIM_BIAS: f32 = 2.5;

/// How many quads the ramp is built from. Each interpolates linearly between
/// its two ends, so this is how finely the curve is sampled — high enough that
/// the per-band corners are far below one step of alpha. delightviewer's count.
const SCRIM_BANDS: usize = 40;

/// The scrim's darkest alpha, under the text itself. delightviewer's `0.8`.
const SCRIM_PEAK: f32 = 0.8;

/// The side of the box the two state badges are drawn in.
///
/// Sized to the strip's type rather than to the strip: these sit in the row the
/// timecode is in, and a glyph taller than the digits beside it reads as a
/// button rather than as a state. 13 px against a 12 px font is the optical
/// match, not the geometric one — outlines read smaller than letters do.
const BADGE: f32 = 13.0;

/// The Range's edge rubber band: `24 · tanh(overflow / 100)`, verbatim from
/// delightstack's `Range.svelte`. Past the end of the track the handle keeps
/// following the pointer, but less and less — which is what says "this is as
/// far as it goes" without the handle simply sticking.
pub const MAX_OVERSHOOT: f32 = 24.0;

/// Slack left at each end of the track so the rubber band has somewhere to
/// stretch into — [`MAX_OVERSHOOT`] would otherwise push the handle over the
/// timecode beside it.
const OVERSHOOT_ROOM: f32 = 18.0;

/// How visible a piece of linger-then-leave chrome is at `now`: 1 while it is
/// held, then eased to 0 over [`FADE`].
///
/// Eased rather than linear for the same reason every other fade in the program
/// is (PLAN §8): a linear opacity ramp has a visible start and stop, and this
/// one is over a moving picture where that reads as a glitch in the video.
pub fn linger_alpha(activity_at: Instant, now: Instant) -> f32 {
    let age = now.saturating_duration_since(activity_at);
    if age < LINGER {
        return 1.0;
    }
    let t = (age - LINGER).as_secs_f32() / FADE.as_secs_f32();
    if t >= 1.0 {
        return 0.0;
    }
    1.0 - Easing::OutQuint.apply(t.clamp(0.0, 1.0))
}

/// `h:mm:ss.d` — the tenth matters when you are frame-stepping, and the hour
/// only shows up when there is one.
pub fn timecode(us: i64) -> String {
    let us = us.max(0);
    let total_ds = us / 100_000;
    let (d, s) = (total_ds % 10, total_ds / 10);
    let (secs, mins, hours) = (s % 60, (s / 60) % 60, s / 3600);
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}.{d}")
    } else {
        format!("{mins}:{secs:02}.{d}")
    }
}

/// A shuttle rate, as the transport says it: `8×`, `◀ 4×`, and nothing at all
/// at 1× — a badge that was always up would be a badge nobody reads.
///
/// The reverse arrow rather than a minus sign: `−4×` reads as arithmetic, and
/// what the key did was turn the tape around.
pub fn rate_label(rate: f64, playing: bool) -> Option<String> {
    if !playing {
        return None;
    }
    let mag = rate.abs();
    if (mag - 1.0).abs() < 0.01 {
        return None;
    }
    let number = if (mag - mag.round()).abs() < 0.01 {
        format!("{}×", mag.round() as i64)
    } else {
        format!("{mag:.1}×")
    };
    Some(if rate < 0.0 {
        // `◂`, the small triangle: `◀` is not in the faces the strip is set in.
        format!("◂ {number}")
    } else {
        number
    })
}

/// The falloff curve: 1 at the strip, 0 at the far end of the fade, flat at
/// both ends.
fn falloff(t: f32) -> f32 {
    let raised = 0.5 + 0.5 * (std::f32::consts::PI * t.clamp(0.0, 1.0)).cos();
    raised.powf(SCRIM_BIAS)
}

/// The scrim as bands of `(y0, y1, alpha at y0, alpha at y1)`, running up from
/// the pane's bottom edge with `hold` points of flat at the start.
///
/// Separate from the painting so the property that matters can be tested
/// without a GPU: **the bands are contiguous**, each band's far edge being the
/// next one's near edge bit for bit.
fn bands(bottom: f32, hold: f32) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::with_capacity(SCRIM_BANDS + 1);
    let start = bottom - hold;
    if hold > 0.0 {
        out.push((bottom, start, 1.0, 1.0));
    }
    let step = SCRIM_FADE / SCRIM_BANDS as f32;
    let mut y = start;
    for i in 0..SCRIM_BANDS {
        let (t0, t1) = (
            i as f32 / SCRIM_BANDS as f32,
            (i + 1) as f32 / SCRIM_BANDS as f32,
        );
        // Carried forward rather than recomputed, so consecutive bands share a
        // coordinate exactly and not merely to within float error.
        let next = start - (i + 1) as f32 * step;
        out.push((y, next, falloff(t0), falloff(t1)));
        y = next;
    }
    out
}

/// Paint the eased scrim along `content`'s bottom edge.
fn scrim(painter: &egui::Painter, content: egui::Rect, hold: f32, peak: f32) {
    let peak = peak.clamp(0.0, 1.0);
    if peak <= 0.004 {
        return;
    }
    // Black on either side: this darkens a frame of video, which is its own
    // ground, not a pane — and the strip on it is drawn in the dark palette
    // for the same reason (`Palette::for_media`).
    let ink = |a: f32| {
        egui::Color32::from_rgba_unmultiplied(
            0,
            0,
            0,
            (a * peak * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    };
    let mut mesh = egui::Mesh::default();
    for (y0, y1, a0, a1) in bands(content.bottom(), hold) {
        let (c0, c1) = (ink(a0), ink(a1));
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(content.left(), y0), c0);
        mesh.colored_vertex(egui::pos2(content.right(), y0), c0);
        mesh.colored_vertex(egui::pos2(content.right(), y1), c1);
        mesh.colored_vertex(egui::pos2(content.left(), y1), c1);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The play/pause **button**, drawn rather than typed.
///
/// It wears the icon of what pressing it *does*, not of what the player is
/// currently doing — a paused clip offers a play triangle, a playing one offers
/// the pause bars. That is what every video player people already use does, and
/// the state is legible from the picture moving anyway.
///
/// `k` scales the glyph, which is how the press reads: the delightstack Video's
/// `.btn:active { scale: 0.88 }`, painted.
fn state_glyph(
    painter: &egui::Painter,
    centre: egui::Pos2,
    playing: bool,
    color: egui::Color32,
    k: f32,
) {
    let r = 5.0 * k;
    if playing {
        // Pause: two bars — "press to stop".
        for dx in [-r * 0.65, r * 0.15] {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(centre.x + dx, centre.y - r),
                    egui::vec2(r * 0.5, r * 2.0),
                ),
                0,
                color,
            );
        }
    } else {
        // Play: the triangle — "press to go". Nudged left of geometric centre
        // (`delightful-ui` §17): its mass sits on its flat edge, so a
        // bounding-box centring reads as shoved right.
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(centre.x - r * 0.7, centre.y - r),
                egui::pos2(centre.x + r * 0.8, centre.y),
                egui::pos2(centre.x - r * 0.7, centre.y + r),
            ],
            color,
            egui::Stroke::NONE,
        ));
    }
}

/// A pen for one badge glyph: its box, optically scaled, and a stroke sized to
/// it. Coordinates run 0…1 in both axes.
///
/// delightviewer's `ui::editbar::Nib`, cut down to the four calls the two
/// badges here make. Ported rather than reinvented so the mute and the loop are
/// drawn at the same weight in both programs — a glyph is a drawing, and two
/// sets of helpers is two stroke weights.
struct Nib<'a> {
    painter: &'a egui::Painter,
    rect: egui::Rect,
    stroke: egui::Stroke,
    ink: egui::Color32,
}

impl<'a> Nib<'a> {
    /// `weight` is the optical correction — 1.0 fills the box, less shrinks it
    /// about its centre.
    fn new(
        painter: &'a egui::Painter,
        box_: egui::Rect,
        weight: f32,
        ink: egui::Color32,
    ) -> Nib<'a> {
        let rect = egui::Rect::from_center_size(box_.center(), box_.size() * weight);
        Nib {
            painter,
            rect,
            // Scaled with the box, floored where a hairline would disappear.
            stroke: egui::Stroke::new((rect.width() / 11.0).max(1.1), ink),
            ink,
        }
    }

    fn at(&self, x: f32, y: f32) -> egui::Pos2 {
        egui::pos2(
            self.rect.left() + x * self.rect.width(),
            self.rect.top() + y * self.rect.height(),
        )
    }

    fn line(&self, a: (f32, f32), b: (f32, f32)) {
        self.painter
            .line_segment([self.at(a.0, a.1), self.at(b.0, b.1)], self.stroke);
    }

    /// An open path through every point.
    fn path(&self, points: &[(f32, f32)]) {
        let points: Vec<egui::Pos2> = points.iter().map(|(x, y)| self.at(*x, *y)).collect();
        self.painter.add(egui::Shape::line(points, self.stroke));
    }

    /// A solid convex shape.
    fn poly(&self, points: &[(f32, f32)]) {
        let points: Vec<egui::Pos2> = points.iter().map(|(x, y)| self.at(*x, *y)).collect();
        self.painter.add(egui::Shape::convex_polygon(
            points,
            self.ink,
            egui::Stroke::NONE,
        ));
    }

    fn rect(&self, a: (f32, f32), b: (f32, f32)) {
        let rect = egui::Rect::from_two_pos(self.at(a.0, a.1), self.at(b.0, b.1));
        self.painter
            .rect_filled(rect, egui::CornerRadius::same(1), self.ink);
    }

    /// An arc, as a polyline, `from`→`to` radians about `c`.
    fn arc(&self, c: (f32, f32), radius: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
        (0..=16)
            .map(|i| {
                let t = from + (to - from) * (i as f32 / 16.0);
                (c.0 + radius * t.cos(), c.1 + radius * t.sin())
            })
            .collect()
    }
}

/// A badge's box, right-aligned at `right` and centred on the strip's line —
/// the glyph's answer to `Align2::RIGHT_CENTER`.
fn badge_box(right: f32, mid: f32) -> egui::Rect {
    egui::Rect::from_center_size(
        egui::pos2(right - BADGE / 2.0, mid),
        egui::vec2(BADGE, BADGE),
    )
}

/// **Muted**: a speaker with a cross beside it, drawn rather than spelled.
///
/// The word `muted` was four times the width of the fact, in the one strip
/// where width is the position bar's to spend — and a crossed speaker is the
/// picture every player on the machine already uses for it.
///
/// A cross **beside** the cone rather than a slash **through** it: the cone is
/// filled, and a stroke of the same ink laid over a filled shape is a stroke
/// you cannot see. Struck-through mute glyphs draw their slash in the
/// background's colour, and the background here is an arbitrary frame of video
/// under a scrim.
fn mute_glyph(painter: &egui::Painter, box_: egui::Rect, ink: egui::Color32) {
    let n = Nib::new(painter, box_, 0.92, ink);
    // The cone, filled: a box and a trapezoid rather than one concave outline,
    // so the shape reads solid at 13 px the way the play triangle does — and so
    // both halves are convex, which is all `poly` can fill.
    n.rect((0.0, 0.34), (0.20, 0.66));
    n.poly(&[(0.18, 0.34), (0.18, 0.66), (0.46, 0.94), (0.46, 0.06)]);
    // …and the cross, which is the whole message. Set in from the cone so the
    // two read as a glyph and its mark, not as one tangle.
    n.line((0.60, 0.30), (0.98, 0.70));
    n.line((0.98, 0.30), (0.60, 0.70));
}

/// **Loop**: a circular arrow — the arc that comes back to where it started,
/// which is what the state means.
fn loop_glyph(painter: &egui::Painter, box_: egui::Rect, ink: egui::Color32) {
    let n = Nib::new(painter, box_, 1.0, ink);
    // Nearly closed: the gap is what the arrowhead sits in, and a full circle
    // with a head on it reads as a circle with a nick in it.
    const R: f32 = 0.40;
    const END: f32 = 4.5;
    let arc = n.arc((0.5, 0.5), R, -1.0, END);
    n.path(&arc);
    // The head points *along* the travel, not at the centre: the barbs come
    // back from the tip against the tangent, which at the top of the circle is
    // rightwards. A head aimed anywhere else turns the arrow into a tick.
    if let Some(&(hx, hy)) = arc.last() {
        let (tx, ty) = (-END.sin(), END.cos());
        for side in [-1.0, 1.0] {
            n.line(
                (hx, hy),
                (
                    hx - 0.17 * tx + side * 0.11 * ty,
                    hy - 0.17 * ty - side * 0.11 * tx,
                ),
            );
        }
    }
}

/// How the position bar is being handled this frame — the delightstack
/// `Range`'s three states, plus the elastic overshoot it gives you when a drag
/// runs off the end of the track.
#[derive(Debug, Clone, Copy, Default)]
pub struct Grab {
    /// A drag is in progress on the handle.
    pub scrubbing: bool,
    /// The primary button is down (anywhere) — what makes the play button read
    /// as pressed while it is held.
    pub down: bool,
    /// Signed pixels the handle is displaced by, past the end of the track.
    /// Live while dragging, springing back to zero on release.
    pub overshoot: f32,
}

/// The rubber band itself, as a function of how far past the end the pointer is.
pub fn overshoot_for(overflow_px: f32) -> f32 {
    MAX_OVERSHOOT * (overflow_px / 100.0).tanh()
}

/// The same band, measured from a track and a pointer.
pub fn overshoot_past(bar: egui::Rect, x: f32) -> f32 {
    let overflow = if x < bar.left() {
        x - bar.left()
    } else if x > bar.right() {
        x - bar.right()
    } else {
        0.0
    };
    overshoot_for(overflow)
}

/// Where the strip's controls landed, so the pointer layer can claim exactly
/// the bar as a scrubber and exactly the glyph as a button.
///
/// Read on the *next* frame's pointer pass, which is the frame after the one
/// that drew them: the strip lays itself out during the paint, and a hit test
/// that guessed the geometry instead is the second answer to a question with
/// one right answer.
#[derive(Debug, Clone, Copy)]
pub struct Hits {
    /// `Rect::NOTHING` when the pane is too narrow to lay a track out — the
    /// button is still a button then, which is the difference between a
    /// cramped strip and no strip.
    pub bar: egui::Rect,
    pub glyph: egui::Rect,
}

/// How far through the file the playhead is, 0..=1.
pub fn progress(state: &TransportState) -> f32 {
    if state.duration_us <= 0 {
        return 0.0;
    }
    (state.position_us as f64 / state.duration_us as f64).clamp(0.0, 1.0) as f32
}

/// Measure a line of text without drawing it — the strip lays itself out by
/// hand, so it needs this before it knows where the bar's ends are.
fn text_width(painter: &egui::Painter, text: &str, font: &egui::FontId) -> f32 {
    painter
        // Measured, never drawn: the colour is no part of the width.
        .layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE)
        .rect
        .width()
}

/// The strip's own rect inside `content`, so the hit test and the drawing
/// cannot disagree about where it is.
pub fn rect(content: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(content.left() + MARGIN, content.bottom() - HEIGHT),
        egui::pos2(content.right() - MARGIN, content.bottom()),
    )
}

/// Draw the strip across the bottom of `content`, and report where its controls
/// landed.
///
/// `alpha` is the linger-fade, already multiplied by whatever crossfade the
/// pane is in. `pointer` is where the hand is, in the same points — the Range
/// grows under it and the seek tooltip follows it — and `grab` is what the hand
/// is *doing*, which the pointer layer decided a frame ago.
///
/// The look is delightviewer's transport, which is the delightstack Video's
/// control bar: a scrim gradient under the controls, and the position bar is
/// its Range — two rounded track segments with a notch either side of a pill
/// handle that grows under the pointer and grows a halo while scrubbing.
pub fn paint(
    paint: &Painting<'_>,
    content: egui::Rect,
    state: &TransportState,
    alpha: f32,
    pointer: Option<egui::Pos2>,
    grab: Grab,
) -> Option<Hits> {
    if alpha <= 0.004 || content.width() <= 0.0 || content.height() <= 0.0 {
        return None;
    }
    let painter = paint.painter.with_clip_rect(content);
    // Only over a picture: an audio card is drawn on the pane's own ground and
    // a scrim there would be a dark band under nothing.
    if state.has_video {
        scrim(&painter, content, HEIGHT + GAP, SCRIM_PEAK * alpha);
    }

    let a = |color: egui::Color32, mul: f32| color.gamma_multiply(alpha.clamp(0.0, 1.0) * mul);
    let text = paint.palette.text;
    let dim = paint.palette.subtext0;
    let accent = paint.palette.blue;

    let strip = rect(content);
    let font = egui::FontId::proportional(FONT);
    let mid = strip.center().y;

    // ── The play/pause button ───────────────────────────────────────────────
    // A button-sized target with a soft backdrop under the pointer, exactly
    // like the Video's `.btn:hover`.
    let glyph_centre = egui::pos2(strip.left() + 8.0, mid);
    let glyph_rect = egui::Rect::from_center_size(glyph_centre, egui::vec2(26.0, 26.0));
    let glyph_hover = pointer.is_some_and(|p| glyph_rect.contains(p));
    // `.btn:active` — the whole button, backdrop and glyph together, shrinks
    // under the finger. Held, not toggled: it springs back on release because
    // the pointer is no longer down, which is what makes it feel physical.
    let press = if glyph_hover && grab.down { 0.88 } else { 1.0 };
    if glyph_hover {
        painter.rect_filled(
            egui::Rect::from_center_size(glyph_centre, glyph_rect.size() * press),
            egui::CornerRadius::same(6),
            a(text, if press < 1.0 { 0.22 } else { 0.14 }),
        );
    }
    // Painted, not typed: egui's default font has no ▶ or ❙❙ and draws tofu.
    state_glyph(&painter, glyph_centre, state.playing, a(text, 1.0), press);

    // ── The two timecodes, one at each end of the track ─────────────────────
    // Not `position / duration` in one lump on the left, which is what this
    // was: the elapsed time is *where the playhead is*, so it belongs at the
    // playhead's end of the bar, and the duration is where the bar stops.
    let elapsed = timecode(state.position_us);
    let total = timecode(state.duration_us);
    let left_x = strip.left() + 22.0;
    painter.text(
        egui::pos2(left_x, mid),
        egui::Align2::LEFT_CENTER,
        &elapsed,
        font.clone(),
        a(text, 1.0),
    );
    let elapsed_w = text_width(&painter, &elapsed, &font);

    // The right end carries the states that are *unusual* — a mute, a loop, a
    // volume that is not 100% — so the strip says nothing at all when there is
    // nothing unusual to say. Glyphs rather than words, because width here is
    // the position bar's to spend.
    let mut right = strip.right();
    if state.looping {
        loop_glyph(&painter, badge_box(right, mid), a(accent, 1.0));
        right -= BADGE + GAP;
    }
    let boosted = state.volume > 1.001;
    if state.muted {
        // Muted outranks the level: a level is what you would hear, and while
        // it is muted you would hear none of it.
        mute_glyph(&painter, badge_box(right, mid), a(accent, 1.0));
        right -= BADGE + GAP;
    } else if state.volume < 0.999 || boosted {
        // A *number* is still a number — a percentage has no picture. A boost
        // takes the accent because it is the app adding something that is not
        // in the file; a quiet level is merely quiet.
        let label = format!("vol {}%", (state.volume * 100.0).round() as i32);
        let width = text_width(&painter, &label, &font);
        painter.text(
            egui::pos2(right, mid),
            egui::Align2::RIGHT_CENTER,
            &label,
            font.clone(),
            a(if boosted { accent } else { dim }, 1.0),
        );
        right -= width + GAP;
    }

    // The shuttle rate **replaces** the duration rather than sitting beside it,
    // and the slot keeps the duration's width either way. It used to be an
    // extra chip, which meant every shuttle key shoved the whole position bar
    // sideways — the one part of the strip that must never move, since the hand
    // is on it.
    let total_w = text_width(&painter, &total, &font);
    let (label, ink) = match rate_label(state.rate, state.playing) {
        Some(rate) => (rate, accent),
        None => (total, dim),
    };
    let label_w = text_width(&painter, &label, &font);
    painter.text(
        egui::pos2(right, mid),
        egui::Align2::RIGHT_CENTER,
        &label,
        font.clone(),
        a(ink, 1.0),
    );
    right -= total_w.max(label_w) + GAP;

    // ── The Range, painted (delightstack `Range.svelte`) ────────────────────
    // The track fills whatever is left between the two label groups, minus the
    // room the rubber band needs to stretch into at either end without shoving
    // the handle through a label.
    let bar = egui::Rect::from_min_max(
        egui::pos2(
            left_x + elapsed_w + GAP + OVERSHOOT_ROOM,
            mid - BAR_HEIGHT / 2.0,
        ),
        egui::pos2(right - OVERSHOOT_ROOM, mid + BAR_HEIGHT / 2.0),
    );
    if bar.width() < 20.0 {
        // No room for a track — but the button is still a button. delightviewer
        // gives up on the whole strip here; a pane a third of a window wide
        // reaches this width often enough that losing play/pause with it would
        // be a control that comes and goes with the splitter.
        return Some(Hits {
            bar: egui::Rect::NOTHING,
            glyph: glyph_rect,
        });
    }
    let hits = Hits {
        bar,
        glyph: glyph_rect,
    };

    let hovered =
        grab.scrubbing || pointer.is_some_and(|p| bar.expand2(egui::vec2(4.0, 10.0)).contains(p));
    // The handle's *visual* position: the value's position plus the rubber band
    // when a drag has run off the end of the track. The track segments ride the
    // same offset — `Range.svelte`'s `lower_visual_offset`, applied to the fill
    // and the handle alike — and that is what makes the overshoot read as the
    // whole control stretching rather than as the handle coming loose from it.
    let fx = bar.left() + bar.width() * progress(state) + grab.overshoot;
    // Track heights: the played (active) segment runs thicker than the rest,
    // and both grow 2 px under the pointer.
    let grow = if hovered { 2.0 } else { 0.0 };
    let active_h = 5.0 + grow;
    let inactive_h = 3.0 + grow;
    // The notch: a gap either side of the handle, so it sits *in* the track
    // rather than on it.
    let notch = 6.0;
    if fx - notch > bar.left() {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(bar.left(), mid - active_h / 2.0),
                egui::pos2(fx - notch, mid + active_h / 2.0),
            ),
            egui::CornerRadius::same((active_h / 2.0) as u8),
            a(text, 1.0),
        );
    }
    if fx + notch < bar.right() {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(fx + notch, mid - inactive_h / 2.0),
                egui::pos2(bar.right(), mid + inactive_h / 2.0),
            ),
            egui::CornerRadius::same((inactive_h / 2.0) as u8),
            a(text, 0.26),
        );
    }

    // The handle: a pill that widens under the pointer and squashes slightly
    // while held, with a halo saying how engaged it is.
    let (hw, hh, halo, halo_a) = if grab.scrubbing {
        (7.5, 16.0, 15.0, 0.18)
    } else if hovered {
        (7.5, 18.0, 13.0, 0.12)
    } else {
        (5.0, 14.0, 0.0, 0.0)
    };
    if halo > 0.0 {
        painter.circle_filled(egui::pos2(fx, mid), halo, a(text, halo_a));
    }
    painter.rect_filled(
        egui::Rect::from_center_size(egui::pos2(fx, mid), egui::vec2(hw, hh)),
        egui::CornerRadius::same((hw / 2.0) as u8),
        a(text, 1.0),
    );

    // The hover timecode, floating above the bar like the Video's seek tooltip
    // — where the click would land, not where the playhead is.
    if hovered && state.duration_us > 0 {
        if let Some(p) = pointer {
            let hover_f = ((p.x - bar.left()) / bar.width().max(1.0)).clamp(0.0, 1.0);
            let label = timecode((hover_f as f64 * state.duration_us as f64) as i64);
            let width = text_width(&painter, &label, &font);
            let cx = p.x.clamp(
                content.left() + width / 2.0 + 8.0,
                content.right() - width / 2.0 - 8.0,
            );
            let tip = egui::Rect::from_center_size(
                egui::pos2(cx, bar.top() - 18.0),
                egui::vec2(width + 12.0, 20.0),
            );
            painter.rect_filled(
                tip,
                egui::CornerRadius::same(4),
                a(paint.palette.crust, 0.92),
            );
            painter.text(
                tip.center(),
                egui::Align2::CENTER_CENTER,
                &label,
                font,
                a(text, 1.0),
            );
        }
    }
    Some(hits)
}

/// Whether the [`audio_card`] takes the pane this frame.
///
/// Only for a clip with no video, and only when the preview pane is not
/// showing the song's sleeve instead — a card over the art is a caption on the
/// album cover. The interesting case is the pane not knowing yet: the
/// transport mounts off a probe, and the probe usually lands before the pane
/// has read the file, let alone decoded the sleeve. So the probe's
/// `has_cover_art` breaks the tie. A song it says has art waits, blank under
/// its strip, for a picture that is on its way; a song without art gets the
/// card at once, exactly as it always has; and a sleeve that turns out not to
/// decode leaves the pane [`Poster::Absent`], which hands it back to the card.
///
/// [`Poster::Absent`]: crate::preview::Poster::Absent
pub fn card_shows(info: &super::TemporalInfo, poster: crate::preview::Poster) -> bool {
    use crate::preview::Poster;
    if info.has_video {
        return false;
    }
    match poster {
        Poster::Shown => false,
        Poster::Pending => !info.has_cover_art,
        Poster::Absent => true,
    }
}

/// The audio card: what an audio file with no sleeve looks like while it
/// plays. A song with one shows the sleeve instead ([`card_shows`]).
///
/// Metadata and the strip, centred in the pane — deliberately not a waveform.
/// **The waveform is deferred**: `dv_media::waveform` can generate one, but it
/// is a decode of the whole file into a peak envelope, which means a worker, a
/// cache keyed like the thumbnail cache, and a second painter — its own commit
/// (PLAN §6 lists it, this is the note that it is owed). What is here instead
/// is honest: the file's name is in the list, so the card says the things the
/// list cannot — how long, what codec, what rate.
pub fn audio_card(
    paint: &Painting<'_>,
    content: egui::Rect,
    info: &super::TemporalInfo,
    alpha: f32,
) {
    let mut lines: Vec<String> = Vec::new();
    if info.duration_us > 0 {
        lines.push(timecode(info.duration_us));
    }
    if let Some(codec) = &info.audio_codec {
        lines.push(codec.to_uppercase());
    }
    if let Some(rate) = info.sample_rate {
        // kHz, because 48000 is a number and 48 kHz is a fact.
        lines.push(format!("{:.1} kHz", rate as f32 / 1000.0));
    }
    if lines.is_empty() {
        lines.push("audio".into());
    }
    // Optically centred (`delightful-ui` §16): biased above the true middle,
    // and above the strip's own line as well.
    let centre = egui::pos2(
        content.center().x,
        content.top() + (content.height() - HEIGHT) * crate::chrome::OPTICAL_CENTRE,
    );
    let line_height = 20.0;
    let top = centre.y - (lines.len() as f32 - 1.0) * line_height / 2.0;
    for (i, line) in lines.iter().enumerate() {
        paint.painter.text(
            egui::pos2(centre.x, top + i as f32 * line_height),
            egui::Align2::CENTER_CENTER,
            line,
            egui::FontId::proportional(if i == 0 { 18.0 } else { 13.0 }),
            if i == 0 {
                paint.palette.text.gamma_multiply(alpha)
            } else {
                paint.palette.overlay0.gamma_multiply(alpha)
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::paint as paint_state;
    use super::*;

    /// The bug that put a hairline across the picture above the controls: two
    /// shapes meeting at a boundary that could fall between two device pixels.
    /// There is one shape now, and its bands are contiguous — each band's far
    /// edge **is** the next one's near edge, bit for bit.
    #[test]
    fn the_scrim_is_one_contiguous_strip_with_no_join_in_it() {
        // A fractional bottom on purpose: the case that made the seam appear
        // and disappear with the window size.
        for bottom in [600.0f32, 601.37] {
            let b = bands(bottom, HEIGHT + GAP);
            assert_eq!(b.len(), SCRIM_BANDS + 1);
            assert_eq!(b[0].0, bottom, "the strip starts at the pane's edge");
            for pair in b.windows(2) {
                assert_eq!(pair[0].1, pair[1].0, "a gap the picture could show through");
            }
        }
    }

    /// The curve: full under the strip, nothing at the far end, never
    /// brightening on the way, and flat at both ends so neither join has a
    /// corner in it.
    #[test]
    fn the_falloff_runs_from_full_to_nothing_and_only_downwards() {
        let b = bands(600.0, HEIGHT + GAP);
        assert_eq!(b[0].2, 1.0);
        assert_eq!(b[0].3, 1.0, "the hold is flat");
        let last = b.last().expect("bands");
        assert!(last.3.abs() < 1e-6, "the tail reaches nothing: {}", last.3);
        for (i, band) in b.iter().enumerate() {
            assert!(band.3 <= band.2 + 1e-6, "band {i} brightens: {band:?}");
        }
        let steps: Vec<f32> = b[1..].iter().map(|band| band.2 - band.3).collect();
        let steepest = steps.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            steps[0] < steepest / 4.0,
            "a corner where it leaves the strip"
        );
        assert!(
            steps[steps.len() - 1] < steepest / 4.0,
            "a corner where it meets the picture"
        );
    }

    /// **Linger, then leave** (PLAN §8): held solid for 2.5 s, gone by 3, and
    /// monotonic in between — chrome that brightened again on its way out would
    /// read as a flicker.
    #[test]
    fn the_strip_holds_then_fades_and_never_comes_back() {
        let t0 = Instant::now();
        assert_eq!(linger_alpha(t0, t0), 1.0);
        assert_eq!(
            linger_alpha(t0, t0 + LINGER - Duration::from_millis(1)),
            1.0
        );
        let mid = linger_alpha(t0, t0 + LINGER + FADE / 2);
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        assert_eq!(linger_alpha(t0, t0 + LINGER + FADE), 0.0);
        assert_eq!(linger_alpha(t0, t0 + Duration::from_secs(60)), 0.0);
        let mut previous = 1.0;
        for step in 0..=20 {
            let value = linger_alpha(t0, t0 + LINGER + FADE * step / 20);
            assert!(
                value <= previous + 1e-4,
                "the fade went backwards at {step}"
            );
            previous = value;
        }
        // A `now` from before the activity — a stale instant carried into a
        // frame — is "just happened", not a panic on a negative duration.
        assert_eq!(linger_alpha(t0, t0 - Duration::from_secs(1)), 1.0);
    }

    /// Every state the strip has draws: a paused video, a reverse shuttle at
    /// 128× with a mute and a loop on, an audio card with no duration, and a
    /// pane so narrow the bar has nowhere to go.
    #[test]
    fn the_strip_paints_without_panicking() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let paint = Painting {
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now: Instant::now(),
            };
            let base = TransportState {
                position_us: 12_300_000,
                duration_us: 225_000_000,
                playing: false,
                rate: 1.0,
                muted: false,
                volume: 1.0,
                looping: false,
                has_video: true,
            };
            for content in [
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(520.0, 880.0)),
                // Narrower than the timecode and the chips together.
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(90.0, 120.0)),
                egui::Rect::NOTHING,
            ] {
                // Every pointer state the Range has, on every strip: at rest,
                // hovering the button, hovering the track, and mid-drag past
                // the end of it.
                let hands = [
                    (None, Grab::default()),
                    (
                        Some(egui::pos2(content.left() + 18.0, content.bottom() - 15.0)),
                        Grab {
                            down: true,
                            ..Grab::default()
                        },
                    ),
                    (
                        Some(content.center_bottom() - egui::vec2(0.0, 15.0)),
                        Grab::default(),
                    ),
                    (
                        Some(egui::pos2(content.right() + 400.0, content.bottom())),
                        Grab {
                            scrubbing: true,
                            down: true,
                            overshoot: MAX_OVERSHOOT,
                        },
                    ),
                ];
                for (pointer, grab) in hands {
                    paint_state(&paint, content, &base, 1.0, pointer, grab);
                    paint_state(
                        &paint,
                        content,
                        &TransportState {
                            playing: true,
                            rate: -128.0,
                            muted: true,
                            looping: true,
                            volume: 1.8,
                            ..base
                        },
                        0.4,
                        pointer,
                        grab,
                    );
                    paint_state(
                        &paint,
                        content,
                        &TransportState {
                            duration_us: 0,
                            has_video: false,
                            ..base
                        },
                        1.0,
                        pointer,
                        grab,
                    );
                }
                audio_card(
                    &paint,
                    content,
                    &super::super::TemporalInfo {
                        has_video: false,
                        has_audio: true,
                        has_cover_art: false,
                        duration_us: 225_000_000,
                        width: None,
                        height: None,
                        rotation: 0,
                        mirrored: false,
                        video_codec: None,
                        audio_codec: Some("flac".into()),
                        sample_rate: Some(48_000),
                    },
                    1.0,
                );
                audio_card(
                    &paint,
                    content,
                    &super::super::TemporalInfo {
                        has_video: false,
                        has_audio: true,
                        has_cover_art: false,
                        duration_us: 0,
                        width: None,
                        height: None,
                        rotation: 0,
                        mirrored: false,
                        video_codec: None,
                        audio_codec: None,
                        sample_rate: None,
                    },
                    1.0,
                );
            }
        });
    }

    /// The Range's rubber band: it follows the pointer past the end of the
    /// track, but less and less, and it never runs away.
    #[test]
    fn the_overshoot_is_elastic_and_bounded() {
        assert_eq!(overshoot_for(0.0), 0.0);
        // Symmetric, and signed the way the overflow is.
        assert!((overshoot_for(-40.0) + overshoot_for(40.0)).abs() < 1e-6);
        assert!(overshoot_for(-40.0) < 0.0);
        // Monotonic, with diminishing returns: 20 px more of drag past the end
        // buys less the further out you already are.
        let (a, b, c) = (
            overshoot_for(20.0),
            overshoot_for(40.0),
            overshoot_for(60.0),
        );
        assert!(a < b && b < c);
        assert!(b - a > c - b, "{a} {b} {c}");
        // …and it stops, however hard the drag pulls.
        assert!(overshoot_for(100_000.0) <= MAX_OVERSHOOT);
        assert!(overshoot_for(100_000.0) > MAX_OVERSHOOT * 0.99);
        // The track leaves room at each end for the handle to stretch into,
        // rather than pushing it over the timecode beside it.
        const { assert!(OVERSHOOT_ROOM + 7.5 / 2.0 >= MAX_OVERSHOOT * 0.8) };
    }

    /// The band, measured from a track and a pointer — inside the track it is
    /// nothing at all, which is what keeps an ordinary drag rigid.
    #[test]
    fn the_band_is_measured_from_the_track_and_the_pointer() {
        let bar = egui::Rect::from_min_max(egui::pos2(100.0, 0.0), egui::pos2(500.0, 4.0));
        assert_eq!(overshoot_past(bar, 300.0), 0.0, "inside the track");
        assert_eq!(overshoot_past(bar, 100.0), 0.0);
        assert!(overshoot_past(bar, 540.0) > 0.0);
        assert!(overshoot_past(bar, 60.0) < 0.0);
        assert_eq!(overshoot_past(bar, 540.0), overshoot_for(40.0));
        assert!(overshoot_past(bar, 100_000.0) <= MAX_OVERSHOOT);
    }

    #[test]
    fn progress_is_clamped_and_safe_on_an_unknown_duration() {
        let mut state = TransportState {
            position_us: 500,
            duration_us: 1000,
            playing: true,
            rate: 1.0,
            muted: false,
            volume: 1.0,
            looping: false,
            has_video: true,
        };
        assert!((progress(&state) - 0.5).abs() < 1e-6);
        state.duration_us = 0;
        assert_eq!(progress(&state), 0.0, "no duration is no fraction");
        state.duration_us = 100;
        assert_eq!(progress(&state), 1.0);
    }

    /// **What the pointer layer is allowed to claim.** The button and the track
    /// come back from the paint, they do not overlap, and both are inside the
    /// strip — a hit box that reached past it would take a click on the picture.
    #[test]
    fn the_strip_reports_a_button_and_a_track_that_do_not_overlap() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let paint = Painting {
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now: Instant::now(),
                tips: None,
                held: None,
            };
            let state = TransportState {
                position_us: 12_300_000,
                duration_us: 225_000_000,
                playing: true,
                rate: 1.0,
                muted: true,
                volume: 1.0,
                looping: true,
                has_video: true,
            };
            let content = egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(520.0, 880.0));
            let hits = paint_state(&paint, content, &state, 1.0, None, Grab::default())
                .expect("the strip lays out at 520 points");
            let strip = rect(content);
            assert!(strip.contains_rect(hits.bar), "{:?}", hits.bar);
            assert!(
                hits.glyph.expand(2.0).right() < hits.bar.left(),
                "the button and the track share a press"
            );
            // Even with both badges up there is a track left, and it stops
            // short of them.
            assert!(hits.bar.right() < strip.right() - BADGE);

            // A pane too narrow for a track still has a button: the play/pause
            // must not come and go with the splitter.
            let cramped = egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(90.0, 120.0));
            let hits = paint_state(&paint, cramped, &state, 1.0, None, Grab::default())
                .expect("a cramped strip still has a button");
            assert!(!hits.bar.is_positive(), "{:?}", hits.bar);
            assert!(hits.glyph.is_positive());
            // …and `Rect::NOTHING` takes no press, which is what makes that safe.
            assert!(!hits
                .bar
                .expand2(egui::vec2(4.0, 10.0))
                .contains(cramped.center()));

            // An invisible strip reports nothing at all, so a press on a faded
            // one is a press on the picture.
            assert!(paint_state(&paint, content, &state, 0.0, None, Grab::default()).is_none());
        });
    }

    #[test]
    fn a_timecode_reads_the_way_a_transport_says_it() {
        assert_eq!(timecode(0), "0:00.0");
        assert_eq!(timecode(1_500_000), "0:01.5");
        assert_eq!(timecode(61_200_000), "1:01.2");
        assert_eq!(timecode(3_723_400_000), "1:02:03.4");
        // A negative position is a clock that has not started, not a minus sign
        // in the middle of the strip.
        assert_eq!(timecode(-5), "0:00.0");
    }

    /// The badge says something only when the rate is worth saying: 1× while
    /// playing is what a transport does, and a rate on a *paused* clip is a
    /// claim about something that is not happening.
    #[test]
    fn the_rate_badge_appears_only_off_the_bottom_rung() {
        assert_eq!(rate_label(1.0, true), None);
        assert_eq!(rate_label(8.0, false), None);
        assert_eq!(rate_label(8.0, true).as_deref(), Some("8×"));
        assert_eq!(rate_label(128.0, true).as_deref(), Some("128×"));
        assert_eq!(rate_label(-4.0, true).as_deref(), Some("◂ 4×"));
        assert_eq!(rate_label(0.5, true).as_deref(), Some("0.5×"));
    }

    /// **A song with a sleeve shows the sleeve; a song without one shows the
    /// card, as it always has** — and neither flashes the other on its way in.
    #[test]
    fn the_audio_card_gives_way_to_a_sleeve_and_only_to_a_sleeve() {
        use crate::preview::Poster;
        let song = super::super::TemporalInfo {
            has_video: false,
            has_audio: true,
            has_cover_art: false,
            duration_us: 225_000_000,
            width: None,
            height: None,
            rotation: 0,
            mirrored: false,
            video_codec: None,
            audio_codec: Some("mp3".into()),
            sample_rate: Some(44_100),
        };
        let tagged = super::super::TemporalInfo {
            has_cover_art: true,
            ..song.clone()
        };

        // No art: the card, whether or not the pane has finished looking —
        // waiting on a pane that will find nothing would only delay it.
        assert!(card_shows(&song, Poster::Pending));
        assert!(card_shows(&song, Poster::Absent));

        // Art: the pane is about to draw it, so the card waits rather than
        // flashing its words underneath…
        assert!(!card_shows(&tagged, Poster::Pending));
        // …stays out of the way once it is there…
        assert!(!card_shows(&tagged, Poster::Shown));
        // …and comes back if the sleeve would not decode after all.
        assert!(card_shows(&tagged, Poster::Absent));

        // A picture on the pane is never captioned, whatever the probe said.
        assert!(!card_shows(&song, Poster::Shown));

        // **A real video never gets the card**: its picture is the player's.
        let video = super::super::TemporalInfo {
            has_video: true,
            video_codec: Some("h264".into()),
            ..song
        };
        for poster in [Poster::Shown, Poster::Pending, Poster::Absent] {
            assert!(!card_shows(&video, poster), "{poster:?}");
        }
    }
}
