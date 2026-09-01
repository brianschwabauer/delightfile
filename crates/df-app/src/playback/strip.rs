//! The position strip: the only chrome a playing file gets (PLAN §4.3, §8).
//!
//! One line across the bottom of the preview pane — play state, timecode, a
//! position bar, and the shuttle rate when it is not 1× — over an **eased**
//! scrim, because the thing behind it is an arbitrary frame of video and white
//! text on an arbitrary frame is unreadable.
//!
//! Three things here are load-bearing and none of them is decoration.
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
/// under it.
const HEIGHT: f32 = 26.0;

/// The position bar's thickness. Thin — it reports a position and (for now) is
/// not a control; pointer scrubbing lands with the rest of the mouse work in
/// Phase 4.
const BAR_HEIGHT: f32 = 3.0;

/// The gap between the strip's pieces.
const GAP: f32 = 10.0;

/// The strip's type size. A shade under the row font: a timecode is a readout,
/// not content.
const FONT: f32 = 12.0;

/// How far past the strip the scrim keeps fading, in points.
///
/// Long on purpose: a gentle curve needs further to get to nothing, and the
/// last quarter of this is under three units of alpha. delightviewer's
/// `SCRIM_FADE`, scaled down for a pane a third of the window wide.
const SCRIM_FADE: f32 = 96.0;

/// Bias on the falloff. 1 is a plain raised cosine; higher pulls the darkness
/// toward the edge, so the ramp can be long without washing out the picture.
const SCRIM_BIAS: f32 = 2.5;

/// How many quads the ramp is built from. Each interpolates linearly between
/// its two ends, so this is how finely the curve is sampled — high enough that
/// the per-band corners are far below one step of alpha.
const SCRIM_BANDS: usize = 32;

/// The scrim's darkest alpha, under the text itself.
const SCRIM_PEAK: f32 = 0.72;

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
        format!("◀ {number}")
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

/// The play/pause glyph, drawn rather than typed.
///
/// It wears the icon of **what the state is**, not of what pressing `k` would
/// do: this is a readout on a preview, not a button you aim at, and a paused
/// clip showing a play triangle would be claiming to be playing.
///
/// The triangle is nudged left of geometric centre (`delightful-ui` §17): its
/// mass sits on its flat edge, so a bounding-box centring reads as shoved
/// right.
fn state_glyph(painter: &egui::Painter, centre: egui::Pos2, playing: bool, color: egui::Color32) {
    let r = 5.0;
    if playing {
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
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(centre.x - r * 0.75, centre.y - r),
                egui::pos2(centre.x + r * 0.75, centre.y),
                egui::pos2(centre.x - r * 0.75, centre.y + r),
            ],
            color,
            egui::Stroke::NONE,
        ));
    }
}

/// Draw the strip across the bottom of `content`.
///
/// `alpha` is the linger-fade, already multiplied by whatever crossfade the
/// pane is in.
pub fn paint(paint: &Painting<'_>, content: egui::Rect, state: &TransportState, alpha: f32) {
    if alpha <= 0.004 || content.width() <= 0.0 || content.height() <= 0.0 {
        return;
    }
    let painter = paint.painter.with_clip_rect(content);
    // Only over a picture: an audio card is drawn on the pane's own ground and
    // a scrim there would be a dark band under nothing.
    if state.has_video {
        scrim(&painter, content, HEIGHT + GAP, SCRIM_PEAK * alpha);
    }

    let ink = paint.palette.text.gamma_multiply(alpha);
    let dim = paint.palette.subtext0.gamma_multiply(alpha);
    let row = egui::Rect::from_min_max(
        egui::pos2(content.left(), content.bottom() - HEIGHT),
        egui::pos2(content.right(), content.bottom()),
    );
    let mid = row.center().y;

    state_glyph(
        &painter,
        egui::pos2(row.left() + 6.0, mid),
        state.playing,
        ink,
    );

    // `position / duration`, left of the bar: one readout, not two, because the
    // question is always "how far through".
    let label = if state.duration_us > 0 {
        format!(
            "{} / {}",
            timecode(state.position_us),
            timecode(state.duration_us)
        )
    } else {
        timecode(state.position_us)
    };
    let galley = painter.layout_no_wrap(label, egui::FontId::monospace(FONT), dim);
    let text_left = row.left() + 6.0 + 8.0 + GAP;
    painter.galley(
        egui::pos2(text_left, mid - galley.size().y / 2.0),
        galley.clone(),
        dim,
    );

    // The right end carries the states that are *unusual* — a rate that is not
    // 1×, a mute, a loop — so the strip says nothing at all when there is
    // nothing unusual to say.
    let mut right = row.right() - 6.0;
    let mut chip = |text: String, color: egui::Color32, painter: &egui::Painter| {
        let g = painter.layout_no_wrap(text, egui::FontId::monospace(FONT), color);
        let width = g.size().x;
        right -= width;
        painter.galley(egui::pos2(right, mid - g.size().y / 2.0), g, color);
        right -= GAP;
    };
    if state.muted {
        chip(
            "muted".into(),
            paint.palette.peach.gamma_multiply(alpha),
            &painter,
        );
    } else if (state.volume - 1.0).abs() > 0.01 {
        chip(
            format!("{}%", (state.volume * 100.0).round() as i32),
            dim,
            &painter,
        );
    }
    if state.looping {
        chip(
            "loop".into(),
            paint.palette.teal.gamma_multiply(alpha),
            &painter,
        );
    }
    if let Some(rate) = rate_label(state.rate, state.playing) {
        // The rate badge is the loudest thing on the strip because it is the
        // one piece of state a hand is actively driving.
        chip(rate, paint.palette.blue.gamma_multiply(alpha), &painter);
    }

    // The bar fills what is left between the timecode and the chips, so it
    // never runs under either.
    let bar_left = text_left + galley.size().x + GAP;
    let bar_right = right;
    if bar_right - bar_left < 24.0 {
        return;
    }
    let track = egui::Rect::from_min_max(
        egui::pos2(bar_left, mid - BAR_HEIGHT / 2.0),
        egui::pos2(bar_right, mid + BAR_HEIGHT / 2.0),
    );
    painter.rect_filled(
        track,
        BAR_HEIGHT / 2.0,
        paint.palette.surface1.gamma_multiply(alpha * 0.9),
    );
    let fraction = if state.duration_us > 0 {
        (state.position_us as f64 / state.duration_us as f64).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    if fraction > 0.0 {
        let filled = egui::Rect::from_min_max(
            track.min,
            egui::pos2(track.left() + track.width() * fraction, track.bottom()),
        );
        painter.rect_filled(
            filled,
            BAR_HEIGHT / 2.0,
            paint.palette.blue.gamma_multiply(alpha),
        );
    }
}

/// The audio card: what an audio file looks like while it plays.
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
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
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
                paint_state(&paint, content, &base, 1.0);
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
                );
                audio_card(
                    &paint,
                    content,
                    &super::super::TemporalInfo {
                        has_video: false,
                        has_audio: true,
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
        assert_eq!(rate_label(-4.0, true).as_deref(), Some("◀ 4×"));
        assert_eq!(rate_label(0.5, true).as_deref(), Some("0.5×"));
    }
}
