//! THE frame shader (§4.2): one fixed pipeline —
//! `NV12 → linear RGB → color grade (§15) →
//! fade multiply → encode sRGB` — shared verbatim by the preview
//! (`dv-app/ui/video.rs`) and export (`dv-export/video.rs`), so what you see
//! is what exports by construction. Both bind: uniforms @0, Y plane @1
//! (R8), UV plane @2 (RG8), sampler @3; the vertex stage is a fullscreen
//! triangle over whatever viewport the host sets (egui callback rect for
//! preview, the full canvas for export).
//!
//! The §6.4 framing model compiles to the `uv_mat`/`uv_off` affine (dest UV →
//! source UV, i.e. crop, nudge, rotation, fill/fit/custom scale all in one
//! 2×3 matrix) plus `bounds`, the source rect in source UV — destination
//! pixels that map outside `bounds` render black (fit letterbox, off-angle
//! rotation corners, custom-mode gaps).

/// The §6.4 default `fill` framing for an UNROTATED, UNCROPPED source: the
/// source covers the destination frame, centered, overflow cropped — as a
/// source-UV window `[off_x, off_y, scale_x, scale_y]`. Kept for the simple
/// consumers (thumbnail placeholders, media preview) that never rotate; the
/// general path goes through [`FrameUniforms::set_placement`].
pub fn cover_uv_rect(src_w: u32, src_h: u32, dst_w: f32, dst_h: f32) -> [f32; 4] {
    let src_aspect = src_w.max(1) as f32 / src_h.max(1) as f32;
    let dst_aspect = (dst_w / dst_h.max(1.0)).max(1e-6);
    if src_aspect > dst_aspect {
        // Source wider: crop left/right.
        let fx = dst_aspect / src_aspect;
        [(1.0 - fx) / 2.0, 0.0, fx, 1.0]
    } else {
        // Source taller: crop top/bottom.
        let fy = src_aspect / dst_aspect;
        [0.0, (1.0 - fy) / 2.0, 1.0, fy]
    }
}

pub const UV_IDENTITY: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// CPU mirror of the shader's uniform block — one packing routine shared by
/// preview and export so the two can never drift (§4.2). 96 bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameUniforms {
    /// x: 1 = limited (studio) range, y: fade-to-black multiply,
    /// z: 1 = BT.709 else BT.601, w: unused.
    pub params: [f32; 4],
    /// exposure (stops, 0), contrast (1), saturation (1), temperature (0) —
    /// the §15 grade, filled by [`FrameUniforms::set_grade`].
    pub grade_a: [f32; 4],
    /// tint (0), highlights (0), shadows (0), vibrance (0).
    pub grade_b: [f32; 4],
    /// Row-major 2×2 of the dest-UV → source-UV affine (§6.4).
    pub uv_mat: [f32; 4],
    /// Affine offset (only .xy used; padded to vec4 in the block).
    pub uv_off: [f32; 2],
    /// Source rect in source UV (x0, y0, x1, y1); outside renders black.
    pub bounds: [f32; 4],
}

impl FrameUniforms {
    /// Identity placement: the whole source into the whole viewport.
    pub fn new(limited_range: bool, fade: f32, bt709: bool) -> FrameUniforms {
        FrameUniforms {
            params: [
                if limited_range { 1.0 } else { 0.0 },
                fade,
                if bt709 { 1.0 } else { 0.0 },
                0.0,
            ],
            grade_a: [0.0, 1.0, 1.0, 0.0],
            grade_b: [0.0, 0.0, 0.0, 0.0],
            uv_mat: [1.0, 0.0, 0.0, 1.0],
            uv_off: [0.0, 0.0],
            bounds: [0.0, 0.0, 1.0, 1.0],
        }
    }

    /// Axis-aligned source-UV window `[off_x, off_y, scale_x, scale_y]` (the
    /// old `uv_rect` semantics — [`cover_uv_rect`]'s output goes here).
    pub fn set_window(&mut self, rect: [f32; 4]) {
        self.uv_mat = [rect[2], 0.0, 0.0, rect[3]];
        self.uv_off = [rect[0], rect[1]];
        self.bounds = [0.0, 0.0, 1.0, 1.0];
    }

    /// The §15 color grade. `GradeParams::to_uniforms` owns the packing so
    /// the CPU/WGSL layout can never drift from the model's field docs.
    pub fn set_grade(&mut self, g: &dv_core::model::GradeParams) {
        let (a, b) = g.to_uniforms();
        self.grade_a = a;
        self.grade_b = b;
    }

    /// The full §6.4 placement from `dv_core::framing`.
    pub fn set_placement(&mut self, p: &dv_core::framing::Placement) {
        self.uv_mat = [
            p.mat[0] as f32,
            p.mat[1] as f32,
            p.mat[2] as f32,
            p.mat[3] as f32,
        ];
        self.uv_off = [p.off[0] as f32, p.off[1] as f32];
        self.bounds = [
            p.bounds[0] as f32,
            p.bounds[1] as f32,
            p.bounds[2] as f32,
            p.bounds[3] as f32,
        ];
    }

    /// The uniform buffer contents, laid out exactly as the WGSL block
    /// expects (six vec4s; `uv_off` padded).
    pub fn to_bytes(&self) -> [u8; 96] {
        let mut out = [0u8; 96];
        let floats: [f32; 24] = [
            self.params[0],
            self.params[1],
            self.params[2],
            self.params[3],
            self.grade_a[0],
            self.grade_a[1],
            self.grade_a[2],
            self.grade_a[3],
            self.grade_b[0],
            self.grade_b[1],
            self.grade_b[2],
            self.grade_b[3],
            self.uv_mat[0],
            self.uv_mat[1],
            self.uv_mat[2],
            self.uv_mat[3],
            self.uv_off[0],
            self.uv_off[1],
            0.0,
            0.0,
            self.bounds[0],
            self.bounds[1],
            self.bounds[2],
            self.bounds[3],
        ];
        for (i, f) in floats.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        out
    }
}

pub const FRAME_WGSL: &str = r#"
struct Uniforms {
    params: vec4<f32>,
    grade_a: vec4<f32>,
    grade_b: vec4<f32>,
    // Dest-UV -> source-UV affine (§6.4 framing: crop/nudge/rotate/scale).
    uv_mat: vec4<f32>,   // row-major 2x2: a, b, c, d
    uv_off: vec4<f32>,   // offset in .xy
    // Source rect in source UV (x0, y0, x1, y1); outside renders black.
    bounds: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex_y: texture_2d<f32>;
@group(0) @binding(2) var tex_uv: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle over the callback viewport.
    var out: VsOut;
    let x = f32(i32(vi & 1u) * 4 - 1);
    let y = f32(i32(vi & 2u) * 2 - 1);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let suv = vec2<f32>(
        u.uv_mat.x * in.uv.x + u.uv_mat.y * in.uv.y + u.uv_off.x,
        u.uv_mat.z * in.uv.x + u.uv_mat.w * in.uv.y + u.uv_off.y,
    );
    // Outside the source rect (§6.4 fit letterbox / rotation corners): black.
    // Computed as a multiply (not an early return) so textureSample stays in
    // uniform control flow.
    var inside = 1.0;
    if (suv.x < u.bounds.x || suv.y < u.bounds.y ||
        suv.x > u.bounds.z || suv.y > u.bounds.w) {
        inside = 0.0;
    }
    var y = textureSample(tex_y, samp, clamp(suv, u.bounds.xy, u.bounds.zw)).r;
    var uv = textureSample(tex_uv, samp, clamp(suv, u.bounds.xy, u.bounds.zw)).rg - vec2<f32>(0.5, 0.5);

    // Limited (studio) range expansion.
    if (u.params.x > 0.5) {
        y = (y * 255.0 - 16.0) / 219.0;
        uv = uv * (255.0 / 224.0);
    }
    y = clamp(y, 0.0, 1.0);

    // BT.709 / BT.601 YCbCr → gamma-encoded RGB (§4.1).
    var rgb: vec3<f32>;
    if (u.params.z > 0.5) {
        rgb = vec3<f32>(
            y + 1.5748 * uv.y,
            y - 0.18732 * uv.x - 0.46812 * uv.y,
            y + 1.8556 * uv.x,
        );
    } else {
        rgb = vec3<f32>(
            y + 1.402 * uv.y,
            y - 0.344136 * uv.x - 0.714136 * uv.y,
            y + 1.772 * uv.x,
        );
    }
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));

    // Linear light: the §15 color grade, then fade multiply (§4.2).
    // grade_a = exposure, contrast, saturation, temperature;
    // grade_b = tint, highlights, shadows, vibrance — all identity-neutral.
    var lin = srgb_to_linear(rgb);
    lin = lin * exp2(u.grade_a.x);                       // exposure (stops)
    // White balance: opposing R/B gains for temperature, G gain for tint.
    lin = lin * vec3<f32>(
        1.0 + 0.25 * u.grade_a.w,
        1.0 - 0.10 * u.grade_b.x,
        1.0 - 0.25 * u.grade_a.w,
    );
    lin = max(lin, vec3<f32>(0.0));
    // Highlights / shadows recovery: luma-weighted gains, mid-gray pivot.
    let l0 = dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722));
    let wh = smoothstep(0.18, 1.0, l0);
    let ws = 1.0 - smoothstep(0.0, 0.35, l0);
    lin = lin * (1.0 + u.grade_b.y * wh) * (1.0 + u.grade_b.z * ws);
    lin = (lin - vec3<f32>(0.18)) * u.grade_a.y + vec3<f32>(0.18); // contrast @ mid-gray
    lin = max(lin, vec3<f32>(0.0));
    // Saturation, then vibrance (weighted toward less-saturated pixels).
    let luma = dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722));
    lin = mix(vec3<f32>(luma), lin, u.grade_a.z);        // saturation
    let mx = max(lin.r, max(lin.g, lin.b));
    let chroma = (mx - min(lin.r, min(lin.g, lin.b))) / max(mx, 1e-4);
    lin = mix(vec3<f32>(luma), lin, 1.0 + u.grade_b.w * (1.0 - chroma));
    lin = lin * u.params.y * inside;                     // fade to black + letterbox
    lin = clamp(lin, vec3<f32>(0.0), vec3<f32>(1.0));

    return vec4<f32>(linear_to_srgb(lin), 1.0);
}
"#;

/// Uniforms for one §17 overlay quad (a graphic element raster, or a solid
/// background). 32 bytes; shared by preview and export like [`FrameUniforms`].
///
/// The quad composites *over* the already-drawn video in gamma (sRGB) space
/// with premultiplied alpha — the CSS compositing model the §17.4 shadow look
/// is tuned against. Hosts create the pipeline with premultiplied blending
/// (`src = One, dst = OneMinusSrcAlpha`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayUniforms {
    /// Quad placement in target UV (x0, y0, x1, y1), y down. May extend
    /// outside 0..1 (element rasters can overhang the frame); the viewport
    /// clips.
    pub rect: [f32; 4],
    /// Multiplied into the sampled premultiplied texel: animation opacity
    /// goes in all four lanes; solid quads use a 1×1 white texture and put
    /// the premultiplied color (× opacity) here.
    pub tint: [f32; 4],
}

impl OverlayUniforms {
    /// A textured quad at `rect` with uniform opacity (premultiplied: one
    /// factor scales color and alpha alike).
    pub fn texture(rect: [f32; 4], opacity: f32) -> OverlayUniforms {
        let o = opacity.clamp(0.0, 1.0);
        OverlayUniforms {
            rect,
            tint: [o, o, o, o],
        }
    }

    /// A solid quad: straight sRGB `color` at `opacity`, premultiplied here.
    /// Bind the shared 1×1 white texture.
    pub fn solid(rect: [f32; 4], color: [f32; 4], opacity: f32) -> OverlayUniforms {
        let a = (color[3] * opacity).clamp(0.0, 1.0);
        OverlayUniforms {
            rect,
            tint: [color[0] * a, color[1] * a, color[2] * a, a],
        }
    }

    /// The uniform buffer contents (two vec4s).
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        let floats: [f32; 8] = [
            self.rect[0],
            self.rect[1],
            self.rect[2],
            self.rect[3],
            self.tint[0],
            self.tint[1],
            self.tint[2],
            self.tint[3],
        ];
        for (i, f) in floats.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        out
    }
}

/// One §17 overlay quad ready to draw: a premultiplied-sRGB RGBA raster (or
/// the host's shared 1×1 white texture when `w == 0` — solid quads) placed
/// via [`OverlayUniforms`]. Shared by preview and export like
/// [`crate::SegmentFx`], so the two composite identically. `tex_key` is the
/// host's GPU-upload cache key — derive it from (graphic fingerprint,
/// element index, output height); 0 is reserved for the white texture.
#[derive(Clone)]
pub struct OverlayDraw {
    pub tex_key: u64,
    pub rgba: std::sync::Arc<Vec<u8>>,
    pub w: u32,
    pub h: u32,
    pub uniforms: OverlayUniforms,
}

impl OverlayDraw {
    /// A textured quad (one graphic element raster).
    pub fn texture(
        tex_key: u64,
        rgba: std::sync::Arc<Vec<u8>>,
        w: u32,
        h: u32,
        rect: [f32; 4],
        opacity: f32,
    ) -> OverlayDraw {
        OverlayDraw {
            tex_key,
            rgba,
            w,
            h,
            uniforms: OverlayUniforms::texture(rect, opacity),
        }
    }

    /// A solid quad (graphic background / title-card fill).
    pub fn solid(rect: [f32; 4], color: [f32; 4], opacity: f32) -> OverlayDraw {
        OverlayDraw {
            tex_key: 0,
            rgba: std::sync::Arc::new(Vec::new()),
            w: 0,
            h: 0,
            uniforms: OverlayUniforms::solid(rect, color, opacity),
        }
    }
}

/// One §17 graphic element compiled for compositing: an uploaded-or-uploadable
/// raster with its resting placement in output px and its stagger slot.
#[derive(Clone)]
pub struct OverlayElement {
    /// Index into the source doc's `elements` (unrenderable ones are
    /// skipped, so this is not the vec position — the editor's selection
    /// mapping needs it).
    pub doc_index: usize,
    pub tex_key: u64,
    pub rgba: std::sync::Arc<Vec<u8>>,
    pub w: u32,
    pub h: u32,
    /// Resting top-left in output px (before animation offset).
    pub x: i32,
    pub y: i32,
    /// The element's target opacity (fades animate 0 → this, §17.5).
    pub opacity: f32,
    /// Stagger rank (`dv_core::graphic::stagger_ranks`), topmost first.
    pub slot: usize,
}

/// One graphic clip compiled against an output resolution — the §17 analogue
/// of [`crate::Segment`]'s fx, shared verbatim by preview and export so the
/// two composite identically. Built once per (doc fingerprint, out size);
/// [`GraphicSpan::quads_at`] is the per-frame animation evaluation.
#[derive(Clone)]
pub struct GraphicSpan {
    pub tl_start_us: i64,
    pub tl_dur_us: i64,
    /// Solid background (title cards); `None` = transparent overlay.
    pub background: Option<[f32; 4]>,
    /// Z-order (document order) — draw first-to-last.
    pub elements: Vec<OverlayElement>,
    pub anim: dv_core::graphic::AnimSpec,
    pub out_w: u32,
    pub out_h: u32,
}

impl GraphicSpan {
    pub fn contains(&self, tl_us: i64) -> bool {
        tl_us >= self.tl_start_us && tl_us < self.tl_start_us + self.tl_dur_us
    }

    /// The §17.5 animation evaluated at a timeline instant: background quad
    /// (fade-only, no stagger, no slide) then element quads in z-order with
    /// per-element opacity/offset. Empty when nothing is visible.
    pub fn quads_at(&self, tl_us: i64) -> Vec<OverlayDraw> {
        use dv_core::graphic::{anim_at, AnimKind, AnimSpec, DOC_UNITS_H};
        let t = tl_us - self.tl_start_us;
        let scale = self.out_h as f32 / DOC_UNITS_H;
        let (fw, fh) = (self.out_w as f32, self.out_h as f32);
        let mut quads = Vec::with_capacity(self.elements.len() + 1);

        if let Some(color) = self.background {
            // The background rides the in/out *fade* component only: slides
            // moving a full-frame fill read as cheap, and cuts stay cuts.
            let fade_only = |k: AnimKind| match k {
                AnimKind::Cut => AnimKind::Cut,
                _ => AnimKind::Fade,
            };
            let spec = AnimSpec {
                in_kind: fade_only(self.anim.in_kind),
                out_kind: fade_only(self.anim.out_kind),
                stagger_us: 0,
                ..self.anim
            };
            let s = anim_at(&spec, 0, 1, t, self.tl_dur_us);
            if s.opacity > 1e-3 {
                quads.push(OverlayDraw::solid([0.0, 0.0, 1.0, 1.0], color, s.opacity));
            }
        }

        let n = self.elements.len();
        for e in &self.elements {
            let s = anim_at(&self.anim, e.slot, n, t, self.tl_dur_us);
            let opacity = s.opacity * e.opacity;
            if opacity <= 1e-3 {
                continue;
            }
            let x = e.x as f32 + s.dx * scale;
            let y = e.y as f32 + s.dy * scale;
            let rect = [x / fw, y / fh, (x + e.w as f32) / fw, (y + e.h as f32) / fh];
            quads.push(OverlayDraw::texture(
                e.tex_key,
                e.rgba.clone(),
                e.w,
                e.h,
                rect,
                opacity,
            ));
        }
        quads
    }
}

/// The §17 overlay shader: one textured quad, premultiplied-alpha composite
/// over the video. Bindings: uniforms @0, RGBA texture @1, sampler @2.
/// Draw with `draw(0..6, 0..1)` (two triangles from `vertex_index`, no
/// vertex buffers — same convention as [`FRAME_WGSL`]'s fullscreen triangle).
pub const OVERLAY_WGSL: &str = r#"
struct Uniforms {
    // Quad rect in target UV (x0, y0, x1, y1), y down.
    rect: vec4<f32>,
    // Multiplied into the sampled premultiplied texel.
    tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Two CCW triangles: 0-1-2, 2-1-3 over the unit quad.
    let corner = vec2<f32>(
        f32(vi == 1u || vi == 3u || vi == 4u),
        f32(vi == 2u || vi == 4u || vi == 5u),
    );
    let tuv = u.rect.xy + corner * (u.rect.zw - u.rect.xy);
    var out: VsOut;
    out.pos = vec4<f32>(tuv.x * 2.0 - 1.0, 1.0 - tuv.y * 2.0, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Premultiplied texel × premultiplied tint; blending does the rest.
    return textureSample(tex, samp, in.uv) * u.tint;
}
"#;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn cover_crops_the_long_axis_only() {
        // Landscape 16:9 into portrait 9:16: full height, cropped width.
        let r = cover_uv_rect(1920, 1080, 1080.0, 1920.0);
        assert_eq!(r[1], 0.0);
        assert_eq!(r[3], 1.0);
        let fx = (9.0 / 16.0) / (16.0 / 9.0);
        assert!((r[2] - fx).abs() < 1e-5);
        assert!((r[0] - (1.0 - fx) / 2.0).abs() < 1e-5);
        // Matching aspects: identity.
        let r = cover_uv_rect(1920, 1080, 3840.0, 2160.0);
        assert_eq!(r, UV_IDENTITY);
    }

    #[test]
    fn uniform_packing_is_six_vec4s() {
        let mut u = FrameUniforms::new(true, 0.5, true);
        u.set_window([0.1, 0.2, 0.8, 0.6]);
        let b = u.to_bytes();
        assert_eq!(b.len(), 96);
        // params.x (limited) at offset 0, fade at 4.
        assert_eq!(f32::from_le_bytes(b[0..4].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(b[4..8].try_into().unwrap()), 0.5);
        // uv_mat starts at float 12: window scale on the diagonal.
        assert_eq!(f32::from_le_bytes(b[48..52].try_into().unwrap()), 0.8);
        assert_eq!(f32::from_le_bytes(b[60..64].try_into().unwrap()), 0.6);
        // uv_off at float 16, padded.
        assert_eq!(f32::from_le_bytes(b[64..68].try_into().unwrap()), 0.1);
        assert_eq!(f32::from_le_bytes(b[68..72].try_into().unwrap()), 0.2);
        // bounds at float 20.
        assert_eq!(f32::from_le_bytes(b[80..84].try_into().unwrap()), 0.0);
        assert_eq!(f32::from_le_bytes(b[92..96].try_into().unwrap()), 1.0);
    }

    #[test]
    fn window_matches_old_uv_rect_semantics() {
        // set_window([off, scale]) must reproduce suv = uv*scale + off.
        let mut u = FrameUniforms::new(false, 1.0, false);
        u.set_window([0.25, 0.0, 0.5, 1.0]);
        let (uv_x, uv_y) = (0.5f32, 0.5f32);
        let sx = u.uv_mat[0] * uv_x + u.uv_mat[1] * uv_y + u.uv_off[0];
        let sy = u.uv_mat[2] * uv_x + u.uv_mat[3] * uv_y + u.uv_off[1];
        assert!((sx - 0.5).abs() < 1e-6);
        assert!((sy - 0.5).abs() < 1e-6);
    }
}
