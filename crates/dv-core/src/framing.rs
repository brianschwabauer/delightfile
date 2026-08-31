//! Framing math (§6.4): source rect + fill/fit/custom placement, derived at
//! render time — never baked numbers. One primitive covers fit/fill, crop,
//! nudge, and rotation, and it all compiles to the single affine + scissor the
//! frame shader consumes (§4.2), preview and export identically.
//!
//! ## Conventions
//! - **Angle** is `clip.rotate` in degrees. Positive = **clockwise on screen**.
//!   The forward model applies the standard rotation matrix
//!   `R(a) = [[cos a, -sin a], [sin a, cos a]]` in *pixel* coordinates, and
//!   because screen Y grows downward that rotation reads as clockwise visually.
//!   At exact multiples of 90° we use exact cos/sin (0, ±1) — no float dust.
//! - **Translate** `(tx, ty)` is in destination pixels; `(0, 0)` = centered.
//!   Positive `tx` shifts the visible content **right** on screen (and thus the
//!   sampled source window moves in the negative-U direction); positive `ty`
//!   shifts content **down**.
//!
//! ## The model (§6.4)
//! Every clip has a *source rect* (the full source frame minus per-edge crop
//! insets, slid within the source by a nudge) and a rotation angle. `fit_mode`
//! decides how that rect meets the project frame:
//! - **fill** — the rotated rect is scaled to *cover* the frame ([`cover_scale`]),
//!   centered on its own center.
//! - **fit** — same rect, letterboxed/pillarboxed ([`fit_scale`]).
//! - **custom** — stored uniform scale + translate, seeded from the current
//!   derived values when the user first touches a manual field.
//!
//! Forward model (destination px from source px):
//! `dest_px = dst_center + (tx, ty) + s * R(angle) * (src_px - rect_center)`.
//! The shader needs the inverse in UV space, which [`placement`] returns.

use crate::model::{Clip, FitMode};

/// Affine mapping destination-frame UV -> source UV, plus the source-rect
/// bounds (in source UV) outside which the shader renders black.
/// `src_uv = mat * dst_uv + off`, row-major 2x2: `[a, b, c, d]` meaning
/// `src_u = a*u + b*v + off[0]`; `src_v = c*u + d*v + off[1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub mat: [f64; 4],
    pub off: [f64; 2],
    /// x0, y0, x1, y1 in source UV.
    pub bounds: [f64; 4],
}

/// Seed values for switching a clip to custom mode from its current derived
/// fill/fit placement (§6.4 "custom ... seeded from the current derived values").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Derived {
    /// Uniform scale, destination px per source px.
    pub scale: f64,
    /// Translate in destination px (0 = centered).
    pub tx: f64,
    pub ty: f64,
}

/// The smallest source-rect edge length the clamp preserves (§6.4 "rect stays
/// >= 16 px each axis when possible").
const MIN_RECT_PX: f64 = 16.0;

/// Exact cos/sin, snapping the four cardinal angles to (0, ±1) so 90°/180°/270°
/// carry no float dust (§6.4 "exact behavior at 0/90/180/270").
fn cos_sin(angle_deg: f64) -> (f64, f64) {
    let a = angle_deg.rem_euclid(360.0);
    if a == 0.0 {
        (1.0, 0.0)
    } else if a == 90.0 {
        (0.0, 1.0)
    } else if a == 180.0 {
        (-1.0, 0.0)
    } else if a == 270.0 {
        (0.0, -1.0)
    } else {
        let r = a.to_radians();
        (r.cos(), r.sin())
    }
}

/// The clip's source rect in source pixels: full frame minus per-edge crop
/// insets, slid by the nudge offset, everything clamped (rect stays
/// `>= MIN_RECT_PX` each axis when possible, rect stays inside the source,
/// nudge clamped to available overflow: `nudge_x in [-crop_l, crop_r]`,
/// `nudge_y in [-crop_t, crop_b]`). Returns `(x, y, w, h)` in source px.
pub fn source_rect(clip: &Clip, src_w: u32, src_h: u32) -> (f64, f64, f64, f64) {
    let sw = src_w as f64;
    let sh = src_h as f64;
    // Degenerate source: hand back a safe unit-ish rect, never NaN.
    if sw < 1.0 || sh < 1.0 {
        return (0.0, 0.0, sw.max(1.0), sh.max(1.0));
    }

    let (x, w) = axis_rect(clip.crop_l, clip.crop_r, clip.nudge_x, sw);
    let (y, h) = axis_rect(clip.crop_t, clip.crop_b, clip.nudge_y, sh);
    (x, y, w, h)
}

/// One axis of [`source_rect`]: clamp the two insets so the remaining span stays
/// `>= MIN_RECT_PX` (or the whole source when it is itself smaller), then slide
/// by the nudge clamped to the available overflow `[-lo, hi]`.
fn axis_rect(inset_lo: i32, inset_hi: i32, nudge: i32, size: f64) -> (f64, f64) {
    let mut lo = (inset_lo.max(0)) as f64;
    let mut hi = (inset_hi.max(0)) as f64;
    let min_span = MIN_RECT_PX.min(size);

    let total = lo + hi;
    let max_total = size - min_span; // total crop that still leaves min_span
    if total > max_total {
        // Give crop back proportionally so the span lands exactly at min_span.
        if total > 0.0 {
            let factor = max_total.max(0.0) / total;
            lo *= factor;
            hi *= factor;
        }
    }

    let span = size - lo - hi;
    let n = (nudge as f64).clamp(-lo, hi);
    let start = lo + n; // in [0, lo + hi] = [0, size - span]
    (start, span)
}

/// Closed-form minimum scale for the rotated `rect_w × rect_h` source rect to
/// COVER the `dst_w × dst_h` frame (fill, §6.4):
/// `max((W*|cos|+H*|sin|)/w, (W*|sin|+H*|cos|)/h)`.
pub fn cover_scale(rect_w: f64, rect_h: f64, dst_w: f64, dst_h: f64, angle_deg: f64) -> f64 {
    if rect_w <= 0.0 || rect_h <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
        return 1.0;
    }
    let (ca, sa) = cos_sin(angle_deg);
    let (ac, as_) = (ca.abs(), sa.abs());
    let sx = (dst_w * ac + dst_h * as_) / rect_w;
    let sy = (dst_w * as_ + dst_h * ac) / rect_h;
    sx.max(sy)
}

/// Scale for the rotated rect's bounding box to FIT inside the frame (§6.4):
/// `min(W/(w*|cos|+h*|sin|), H/(w*|sin|+h*|cos|))`.
pub fn fit_scale(rect_w: f64, rect_h: f64, dst_w: f64, dst_h: f64, angle_deg: f64) -> f64 {
    if rect_w <= 0.0 || rect_h <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
        return 1.0;
    }
    let (ca, sa) = cos_sin(angle_deg);
    let (ac, as_) = (ca.abs(), sa.abs());
    let sx = dst_w / (rect_w * ac + rect_h * as_);
    let sy = dst_h / (rect_w * as_ + rect_h * ac);
    sx.min(sy)
}

/// Current derived scale/translate for the clip (§6.4). fill/fit use their
/// formula and `(0, 0)` translate; custom returns the stored values, scale
/// defaulting to the fill cover scale and translate to 0 when unset.
pub fn derived(clip: &Clip, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Derived {
    let (_, _, w, h) = source_rect(clip, src_w, src_h);
    let dw = dst_w as f64;
    let dh = dst_h as f64;
    let cover = cover_scale(w, h, dw, dh, clip.rotate);
    match clip.fit_mode {
        FitMode::Fill => Derived {
            scale: cover,
            tx: 0.0,
            ty: 0.0,
        },
        FitMode::Fit => Derived {
            scale: fit_scale(w, h, dw, dh, clip.rotate),
            tx: 0.0,
            ty: 0.0,
        },
        FitMode::Custom => Derived {
            scale: clip.scale.unwrap_or(cover),
            tx: clip.tx.unwrap_or(0.0),
            ty: clip.ty.unwrap_or(0.0),
        },
    }
}

/// The full placement (§6.4). The forward model is
/// `dest_px = dst_center + (tx,ty) + s * R(angle) * (src_px - rect_center)`,
/// where `s` and `(tx,ty)` come from `fit_mode` via [`derived`]. This returns
/// the INVERSE mapping in UV space:
/// `src_px = rect_center + R(-angle) * (dest_px - dst_center - (tx,ty)) / s`,
/// converted so the shader can do `src_uv = mat*dst_uv + off` with `dst_uv` in
/// `[0,1]^2` over the project frame and `src_uv` in `[0,1]^2` over the full
/// source texture. `bounds` is the source rect as UV.
pub fn placement(clip: &Clip, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Placement {
    let (rx, ry, rw, rh) = source_rect(clip, src_w, src_h);
    let sw = src_w as f64;
    let sh = src_h as f64;
    let dw = dst_w as f64;
    let dh = dst_h as f64;

    let rect_uv = [rx / sw, ry / sh, (rx + rw) / sw, (ry + rh) / sh];
    let identity = |bounds: [f64; 4]| Placement {
        mat: [1.0, 0.0, 0.0, 1.0],
        off: [0.0, 0.0],
        bounds,
    };

    // Degenerate dimensions: fall back to a safe identity mapping.
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return identity([0.0, 0.0, 1.0, 1.0]);
    }

    let d = derived(clip, src_w, src_h, dst_w, dst_h);
    let s = d.scale;
    if !s.is_finite() || s <= 1e-12 {
        // Bad scale: keep the scissor rect meaningful, mapping identity.
        return identity(rect_uv);
    }

    // R(-angle) = [[cos, sin], [-sin, cos]]; M = (1/s) * R(-angle).
    let (ca, sa) = cos_sin(clip.rotate);
    let inv_s = 1.0 / s;
    let m11 = inv_s * ca;
    let m12 = inv_s * sa;
    let m21 = -inv_s * sa;
    let m22 = inv_s * ca;

    let dcx = dw * 0.5;
    let dcy = dh * 0.5;
    let rcx = rx + rw * 0.5;
    let rcy = ry + rh * 0.5;
    let (tx, ty) = (d.tx, d.ty);

    // dst_px = dst_uv * (dw, dh); src_uv = src_px / (sw, sh). Fold both
    // conversions into the affine so it acts on dst_uv -> src_uv directly.
    let a = m11 * dw / sw;
    let b = m12 * dh / sw;
    let c = m21 * dw / sh;
    let dd = m22 * dh / sh;

    let off0 = (rcx + m11 * (-dcx - tx) + m12 * (-dcy - ty)) / sw;
    let off1 = (rcy + m21 * (-dcx - tx) + m22 * (-dcy - ty)) / sh;

    Placement {
        mat: [a, b, c, dd],
        off: [off0, off1],
        bounds: rect_uv,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point
    use super::*;
    use crate::model::{ClipId, MediaId};

    fn clip() -> Clip {
        Clip::new(ClipId(1), Some(MediaId(1)), 0, 1_000_000)
    }

    /// Map a dst-UV point through a placement to source UV.
    fn map(p: &Placement, u: f64, v: f64) -> (f64, f64) {
        (
            p.mat[0] * u + p.mat[1] * v + p.off[0],
            p.mat[2] * u + p.mat[3] * v + p.off[1],
        )
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // 1. Default clip, matching src/dst dims -> identity.
    #[test]
    fn identity_when_dims_match() {
        let p = placement(&clip(), 1920, 1080, 1920, 1080);
        assert_eq!(p.mat, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(p.off, [0.0, 0.0]);
        assert_eq!(p.bounds, [0.0, 0.0, 1.0, 1.0]);
    }

    // 2. Default fill, landscape 1920x1080 into portrait 1080x1920 -> cover crop.
    //    Cross-checked against dv-playback frame_shader::cover_uv_rect math
    //    (no dependency on that crate).
    #[test]
    fn fill_landscape_into_portrait_covers() {
        let p = placement(&clip(), 1920, 1080, 1080, 1920);
        let fx = (1080.0 / 1920.0) / (1920.0 / 1080.0);
        // Full source height visible, horizontal window = fx, centered.
        assert!(close(p.mat[0], fx));
        assert!(close(p.mat[1], 0.0));
        assert!(close(p.mat[2], 0.0));
        assert!(close(p.mat[3], 1.0));
        assert!(close(p.off[0], (1.0 - fx) / 2.0));
        assert!(close(p.off[1], 0.0));
        assert_eq!(p.bounds, [0.0, 0.0, 1.0, 1.0]);
        // Same shape cover_uv_rect would produce: [(1-fx)/2, 0, fx, 1].
        let (cu, cv) = map(&p, 0.0, 0.0);
        assert!(close(cu, (1.0 - fx) / 2.0));
        assert!(close(cv, 0.0));
    }

    // 3. §6.4 narrative: crop the bottom / crop the right (fill).
    #[test]
    fn crop_shifts_the_visible_center() {
        // Crop 100 px off the bottom: dst center -> rect center, sides centered,
        // vertical center pulled up (bottom gone).
        let mut c = clip();
        c.crop_b = 100;
        let p = placement(&c, 1920, 1080, 1920, 1080);
        let (u, v) = map(&p, 0.5, 0.5);
        assert!(close(u, 0.5)); // sides centered
        assert!(close(v, ((1080.0 - 100.0) / 2.0) / 1080.0)); // 490/1080
        assert!(v < 0.5);

        // Crop the right edge: visible source center moves left.
        let mut c = clip();
        c.crop_r = 200;
        let p = placement(&c, 1920, 1080, 1920, 1080);
        let (u, v) = map(&p, 0.5, 0.5);
        assert!(close(u, ((1920.0 - 200.0) / 2.0) / 1920.0)); // 860/1920
        assert!(u < 0.5);
        assert!(close(v, 0.5));
    }

    // 4. Nudge clamps to overflow; crop clamps to a >= 16 px rect.
    #[test]
    fn nudge_and_crop_clamp() {
        // Nudge clamped to [-crop_l, crop_r] = [-0, 50].
        let mut c = clip();
        c.crop_r = 50;
        c.nudge_x = 10_000; // absurd, clamps to +50
        let (x, _, w, _) = source_rect(&c, 1920, 1080);
        assert!(close(w, 1870.0)); // 1920 - 50
        assert!(close(x, 50.0)); // slid fully right: crop_l(0) + 50
        assert!(close(x + w, 1920.0)); // touches the right source edge

        // Negative nudge clamps to -crop_l = 0 (no left overflow).
        let mut c = clip();
        c.crop_r = 50;
        c.nudge_x = -10_000;
        let (x, _, _, _) = source_rect(&c, 1920, 1080);
        assert!(close(x, 0.0));

        // Crop everything -> rect clamps to exactly MIN_RECT_PX wide.
        let mut c = clip();
        c.crop_l = 10_000;
        c.crop_r = 10_000;
        let (x, _, w, _) = source_rect(&c, 1920, 1080);
        assert!(close(w, MIN_RECT_PX));
        // Symmetric huge crops keep it centered.
        assert!(close(x, (1920.0 - MIN_RECT_PX) / 2.0));
    }

    // 5. Fit mode letterboxes: corners map OUTSIDE bounds, center -> rect center.
    #[test]
    fn fit_letterboxes() {
        let mut c = clip();
        c.fit_mode = FitMode::Fit;
        let p = placement(&c, 1920, 1080, 1080, 1920);
        // Center still lands on the rect center.
        let (u, v) = map(&p, 0.5, 0.5);
        assert!(close(u, 0.5));
        assert!(close(v, 0.5));
        // A dst corner falls outside the source-rect bounds (the letterbox).
        let (cu, cv) = map(&p, 0.0, 0.0);
        let outside = cu < p.bounds[0] - 1e-9
            || cv < p.bounds[1] - 1e-9
            || cu > p.bounds[2] + 1e-9
            || cv > p.bounds[3] + 1e-9;
        assert!(
            outside,
            "fit corner {cu},{cv} should be outside {:?}",
            p.bounds
        );
    }

    // 6. Rotation 90° (fill, same aspect): center -> rect center; cover_scale
    //    swaps the axis formula to max(H/w, W/h).
    #[test]
    fn rotation_90_fill() {
        let mut c = clip();
        c.rotate = 90.0;
        let p = placement(&c, 1920, 1080, 1920, 1080);
        let (u, v) = map(&p, 0.5, 0.5);
        assert!(close(u, 0.5));
        assert!(close(v, 0.5));

        // cover_scale(w,h,W,H,90) == max(H/w, W/h).
        let s = cover_scale(1920.0, 1080.0, 1920.0, 1080.0, 90.0);
        assert!(close(s, (1080.0f64 / 1920.0).max(1920.0 / 1080.0)));

        // Exactness at cardinal angles: 180° cover == 0° cover (aspect-symmetric).
        assert!(close(
            cover_scale(1920.0, 1080.0, 1080.0, 1920.0, 0.0),
            cover_scale(1920.0, 1080.0, 1080.0, 1920.0, 180.0),
        ));
    }

    // 7. Small off-angle rotation in fill: all four dst corners map INSIDE the
    //    source bounds — the auto cover-scale that straightens a crooked shot.
    #[test]
    fn small_rotation_keeps_corners_inside() {
        let mut c = clip();
        c.rotate = 5.0;
        let p = placement(&c, 1920, 1080, 1920, 1080);
        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let (su, sv) = map(&p, u, v);
            assert!(
                su >= p.bounds[0] - 1e-9 && su <= p.bounds[2] + 1e-9,
                "corner {u},{v} -> su {su} outside {:?}",
                p.bounds
            );
            assert!(
                sv >= p.bounds[1] - 1e-9 && sv <= p.bounds[3] + 1e-9,
                "corner {u},{v} -> sv {sv} outside {:?}",
                p.bounds
            );
        }
    }

    // 8. Custom: identity at scale 1 / no translate / equal dims; translate
    //    moves the sampled source window opposite the on-screen shift.
    #[test]
    fn custom_identity_and_translate() {
        let mut c = clip();
        c.fit_mode = FitMode::Custom;
        c.scale = Some(1.0);
        c.tx = Some(0.0);
        c.ty = Some(0.0);
        let p = placement(&c, 1920, 1080, 1920, 1080);
        assert_eq!(p.mat, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(p.off, [0.0, 0.0]);

        // Positive tx shifts content right on screen -> the source window the
        // dst center samples moves LEFT (negative U) by tx source px.
        let mut c2 = c.clone();
        c2.tx = Some(100.0);
        let p2 = placement(&c2, 1920, 1080, 1920, 1080);
        let (u, _) = map(&p2, 0.5, 0.5);
        assert!(close(u, (960.0 - 100.0) / 1920.0));
        assert!(u < 0.5);
    }

    // 9. derived(): fill returns the cover scale; seeding custom from those
    //    derived values reproduces the fill placement exactly.
    #[test]
    fn derived_seeds_custom_to_match_fill() {
        let mut c = clip();
        c.crop_t = 60;
        c.crop_r = 40;
        c.rotate = 3.0;
        let fill = placement(&c, 1920, 1080, 1080, 1920);

        let d = derived(&c, 1920, 1080, 1080, 1920);
        assert!(close(
            d.scale,
            cover_scale(
                source_rect(&c, 1920, 1080).2,
                source_rect(&c, 1920, 1080).3,
                1080.0,
                1920.0,
                3.0
            )
        ));
        assert_eq!((d.tx, d.ty), (0.0, 0.0));

        // Switch to custom seeded from the derived values.
        let mut cc = c.clone();
        cc.fit_mode = FitMode::Custom;
        cc.scale = Some(d.scale);
        cc.tx = Some(d.tx);
        cc.ty = Some(d.ty);
        let custom = placement(&cc, 1920, 1080, 1080, 1920);

        for i in 0..4 {
            assert!(close(custom.mat[i], fill.mat[i]));
        }
        assert!(close(custom.off[0], fill.off[0]));
        assert!(close(custom.off[1], fill.off[1]));
        assert_eq!(custom.bounds, fill.bounds);
    }

    // Degenerate inputs never panic or produce NaN.
    #[test]
    fn degenerate_inputs_are_safe() {
        let p = placement(&clip(), 0, 0, 1920, 1080);
        assert!(p.mat.iter().all(|x| x.is_finite()));
        assert!(p.off.iter().all(|x| x.is_finite()));

        let p = placement(&clip(), 1920, 1080, 0, 0);
        assert!(p.mat.iter().all(|x| x.is_finite()));

        let mut c = clip();
        c.fit_mode = FitMode::Custom;
        c.scale = Some(-3.0); // bad scale
        let p = placement(&c, 1920, 1080, 1920, 1080);
        assert!(p.mat.iter().all(|x| x.is_finite()));
        assert!(p.off.iter().all(|x| x.is_finite()));

        // fit/cover scale on a zero rect stays finite.
        assert!(cover_scale(0.0, 0.0, 1920.0, 1080.0, 0.0).is_finite());
        assert!(fit_scale(0.0, 0.0, 1920.0, 1080.0, 0.0).is_finite());
    }
}
