//! The §17 graphic document: background + ordered elements, plus the closed
//! in/out animation set (§17.5). Pure data + math — rasterization lives in
//! dv-graphic, compositing in dv-playback/dv-export.
//!
//! **Units.** All lengths (font size, offsets, padding, outline width, shadow
//! size) live in *doc units*: a virtual frame exactly [`DOC_UNITS_H`] tall,
//! width = `DOC_UNITS_H × aspect`. Rasterizers scale by
//! `output_height / DOC_UNITS_H`, so switching 1080p ⇄ 4K ⇄ portrait keeps
//! every proportion — including outline thickness (§17.3 explicit
//! requirement).
//!
//! **Z-order is document order** (painter-style, §17.3): `elements[0]` draws
//! first (bottom). Stagger order is Y position (reading order), independent
//! of z by design — see [`stagger_ranks`].

use serde::{Deserialize, Serialize};

use crate::model::{GraphicId, TimeUs};

/// Virtual frame height all doc-unit lengths are relative to.
pub const DOC_UNITS_H: f32 = 1080.0;

/// Max slide travel (§17.5): 5% of frame height, in doc units.
pub const SLIDE_TRAVEL: f32 = DOC_UNITS_H * 0.05;

/// Straight (non-premultiplied) sRGB color, 0–1 per channel.
pub type Color = [f32; 4];

pub const WHITE: Color = [1.0, 1.0, 1.0, 1.0];

/// A graphic row (§8.1 `graphics`). The document stays opaque JSON at the DB
/// boundary; [`Graphic::doc`] parses into the typed [`GraphicDoc`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graphic {
    pub id: GraphicId,
    pub doc_json: String,
}

impl Graphic {
    pub fn new(id: GraphicId, doc: &GraphicDoc) -> Graphic {
        Graphic {
            id,
            doc_json: doc.to_json(),
        }
    }

    /// Parsed document (unknown/missing fields default — old snapshots and
    /// hand-edited JSON both stay loadable).
    pub fn doc(&self) -> GraphicDoc {
        GraphicDoc::from_json(&self.doc_json)
    }

    pub fn set_doc(&mut self, doc: &GraphicDoc) {
        self.doc_json = doc.to_json();
    }
}

/// A graphic = background + ordered elements + one in/out animation pair
/// (§17.3, §17.5). List order is z-order, bottom to top.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GraphicDoc {
    /// `None` = transparent (overlays); title cards set a color (§17.3).
    pub background: Option<Color>,
    pub elements: Vec<GraphicElement>,
    pub anim: AnimSpec,
}

impl GraphicDoc {
    pub fn from_json(s: &str) -> GraphicDoc {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// Stable cache key for rasters (per output resolution the caller adds
    /// the height): a hash of the exact serialized document. Never 0.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.to_json().hash(&mut h);
        h.finish().max(1)
    }

    /// Display name for lists: first non-empty text content (truncated), else
    /// "Graphic".
    pub fn title(&self) -> String {
        for e in &self.elements {
            if let ElementKind::Text(t) = &e.kind {
                let s = t.content.trim();
                if !s.is_empty() {
                    let mut out: String = s.chars().take(24).collect();
                    if s.chars().count() > 24 {
                        out.push('…');
                    }
                    return out;
                }
            }
        }
        "Graphic".into()
    }
}

/// One element: common placement/effects + the text-or-image payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicElement {
    /// 9-point frame anchor, numpad layout (§17.6): 7 top-left, 5 center,
    /// 3 bottom-right. The element's own matching point aligns to it.
    pub anchor: u8,
    /// Offset from the anchor point, doc units.
    pub dx: f32,
    pub dy: f32,
    /// The element's own target opacity (§17.3 per-element; fades animate
    /// 0 → this, never through 1).
    pub opacity: f32,
    /// Drop shadow — text and images with alpha (§17.4).
    pub shadow: Shadow,
    pub kind: ElementKind,
}

impl Default for GraphicElement {
    fn default() -> Self {
        GraphicElement {
            anchor: 5,
            dx: 0.0,
            dy: 0.0,
            opacity: 1.0,
            shadow: Shadow::default(),
            kind: ElementKind::Text(TextBlock::default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementKind {
    Text(TextBlock),
    Image(ImageRef),
}

/// A text block (§17.3): shaped by cosmic-text in the rasterizer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextBlock {
    pub content: String,
    /// System font family name; empty = the rasterizer's default chain.
    pub font: String,
    /// Doc units (§17.4 default: big text over video).
    pub size: f32,
    pub weight: Weight,
    pub color: Color,
    pub align: Align,
    /// Wrap width in doc units; `None` = no wrap.
    pub wrap: Option<f32>,
    /// Line height multiplier.
    pub line_spacing: f32,
    pub outline: Outline,
    pub bbox: BgBox,
}

impl Default for TextBlock {
    fn default() -> Self {
        TextBlock {
            content: String::new(),
            font: String::new(),
            size: 96.0,
            weight: Weight::Bold,
            color: WHITE,
            align: Align::Center,
            wrap: None,
            line_spacing: 1.15,
            outline: Outline::default(),
            bbox: BgBox::default(),
        }
    }
}

/// An image element (§17.3): PNG/JPEG/WebP/SVG referenced by path, hash for
/// relink/cross-project verification. 1 source px = 1 doc unit at scale 1
/// (SVG: its viewBox px). SVG rasterizes at output resolution — crisp at any
/// size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageRef {
    pub path: String,
    /// blake3 content hash (`hash::content_hash`).
    pub hash: String,
    /// Natural size in source px (SVG: viewBox), captured at insert.
    pub natural_w: f32,
    pub natural_h: f32,
    pub scale: f32,
}

impl Default for ImageRef {
    fn default() -> Self {
        ImageRef {
            path: String::new(),
            hash: String::new(),
            natural_w: 0.0,
            natural_h: 0.0,
            scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Weight {
    Light,
    Regular,
    Medium,
    #[default]
    Bold,
    Black,
}

impl Weight {
    /// `w` cycles light → regular → medium → bold → black → light (§17.6).
    pub fn cycled(self) -> Weight {
        match self {
            Weight::Light => Weight::Regular,
            Weight::Regular => Weight::Medium,
            Weight::Medium => Weight::Bold,
            Weight::Bold => Weight::Black,
            Weight::Black => Weight::Light,
        }
    }

    /// CSS-style numeric weight for font selection.
    pub fn css(self) -> u16 {
        match self {
            Weight::Light => 300,
            Weight::Regular => 400,
            Weight::Medium => 500,
            Weight::Bold => 700,
            Weight::Black => 900,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Weight::Light => "light",
            Weight::Regular => "regular",
            Weight::Medium => "medium",
            Weight::Bold => "bold",
            Weight::Black => "black",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

impl Align {
    /// `a` cycles left → center → right (§17.6).
    pub fn cycled(self) -> Align {
        match self {
            Align::Left => Align::Center,
            Align::Center => Align::Right,
            Align::Right => Align::Left,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }
}

/// Drop shadow (§17.4): three controls, direction fixed straight down,
/// offset proportional to size. Off states keep their values (strip
/// convention) so toggling restores where the user left it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shadow {
    pub on: bool,
    /// Opacity 0–1.
    pub strength: f32,
    /// Extent, doc units.
    pub size: f32,
    /// Falloff exponent: 1 = linear, higher concentrates density near the
    /// glyph; default ~2 (the "stacked CSS shadows" look).
    pub falloff: f32,
}

impl Default for Shadow {
    fn default() -> Self {
        Shadow {
            on: true,
            strength: 0.6,
            size: 12.0,
            falloff: 2.0,
        }
    }
}

/// Text outline (§17.4): renders first, shadow is cast by the *outlined*
/// shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outline {
    pub on: bool,
    pub color: Color,
    /// Doc units.
    pub width: f32,
}

impl Default for Outline {
    fn default() -> Self {
        Outline {
            on: false,
            color: WHITE,
            width: 3.0,
        }
    }
}

/// Per-text-block background box (§17.4): toggling on lands with sensible
/// padding and radius already applied, not zeros.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BgBox {
    pub on: bool,
    pub color: Color,
    /// Doc units.
    pub pad: f32,
    pub radius: f32,
}

impl Default for BgBox {
    fn default() -> Self {
        BgBox {
            on: false,
            color: [0.0, 0.0, 0.0, 0.55],
            pad: 24.0,
            radius: 8.0,
        }
    }
}

/// The closed §17.5 animation set: two slots (in/out), one duration each,
/// plus stagger. Easing is not a control: in = quartic ease-out, out =
/// quartic ease-in; linear does not exist here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimSpec {
    pub in_kind: AnimKind,
    pub in_dur_us: TimeUs,
    pub out_kind: AnimKind,
    pub out_dur_us: TimeUs,
    /// Per-element start offset (Y-order, topmost first), µs.
    pub stagger_us: TimeUs,
}

impl Default for AnimSpec {
    fn default() -> Self {
        AnimSpec {
            in_kind: AnimKind::Fade,
            in_dur_us: 400_000,
            out_kind: AnimKind::Fade,
            out_dur_us: 250_000,
            stagger_us: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnimKind {
    Cut,
    #[default]
    Fade,
    Slideup,
    Slidedown,
    Slideleft,
    Slideright,
}

impl AnimKind {
    pub fn cycled(self) -> AnimKind {
        match self {
            AnimKind::Cut => AnimKind::Fade,
            AnimKind::Fade => AnimKind::Slideup,
            AnimKind::Slideup => AnimKind::Slidedown,
            AnimKind::Slidedown => AnimKind::Slideleft,
            AnimKind::Slideleft => AnimKind::Slideright,
            AnimKind::Slideright => AnimKind::Cut,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AnimKind::Cut => "cut",
            AnimKind::Fade => "fade",
            AnimKind::Slideup => "slide up",
            AnimKind::Slidedown => "slide down",
            AnimKind::Slideleft => "slide left",
            AnimKind::Slideright => "slide right",
        }
    }

    /// Exit/entry travel direction as a unit vector (dx, dy); zero for
    /// cut/fade. "Up" = element moves up = −y.
    fn dir(self) -> (f32, f32) {
        match self {
            AnimKind::Cut | AnimKind::Fade => (0.0, 0.0),
            AnimKind::Slideup => (0.0, -1.0),
            AnimKind::Slidedown => (0.0, 1.0),
            AnimKind::Slideleft => (-1.0, 0.0),
            AnimKind::Slideright => (1.0, 0.0),
        }
    }
}

/// One element's animation state at an instant: multiply `opacity` into the
/// element's target opacity; offset by (`dx`, `dy`) doc units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimSample {
    pub opacity: f32,
    pub dx: f32,
    pub dy: f32,
}

impl AnimSample {
    pub const VISIBLE: AnimSample = AnimSample {
        opacity: 1.0,
        dx: 0.0,
        dy: 0.0,
    };
}

/// Stagger ranks (§17.5): `ranks[i]` = the stagger slot of element `i`,
/// ordered by Y position topmost first (reading order), ties by document
/// order. `ys[i]` = element `i`'s resolved top edge in any consistent space.
pub fn stagger_ranks(ys: &[f32]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..ys.len()).collect();
    order.sort_by(|&a, &b| ys[a].total_cmp(&ys[b]).then(a.cmp(&b)));
    let mut ranks = vec![0usize; ys.len()];
    for (slot, &idx) in order.iter().enumerate() {
        ranks[idx] = slot;
    }
    ranks
}

fn ease_out_quart(p: f32) -> f32 {
    let q = 1.0 - p.clamp(0.0, 1.0);
    1.0 - q * q * q * q
}

fn ease_in_quart(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    p * p * p * p
}

/// Evaluate the §17.5 animation for one element.
///
/// `slot` = the element's stagger rank (see [`stagger_ranks`]), `n` = element
/// count, `t_us` = time since clip start, `dur_us` = clip duration. Slides
/// are capped at [`SLIDE_TRAVEL`] and always combine with fade. `Cut` has no
/// duration: in = visible from clip start, out = visible to clip end (stagger
/// doesn't apply to cuts).
pub fn anim_at(spec: &AnimSpec, slot: usize, n: usize, t_us: TimeUs, dur_us: TimeUs) -> AnimSample {
    let mut opacity = 1.0f32;
    let (mut dx, mut dy) = (0.0f32, 0.0f32);

    // In.
    if spec.in_kind != AnimKind::Cut && spec.in_dur_us > 0 {
        let start = spec.stagger_us.max(0) * slot as TimeUs;
        let p = (t_us - start) as f32 / spec.in_dur_us as f32;
        let e = ease_out_quart(p);
        opacity *= e;
        let (ux, uy) = spec.in_kind.dir();
        // Enters *moving toward* the target: starts displaced opposite the
        // travel direction, eases to zero.
        dx -= ux * SLIDE_TRAVEL * (1.0 - e);
        dy -= uy * SLIDE_TRAVEL * (1.0 - e);
    }

    // Out (same stagger order: topmost leaves first).
    if spec.out_kind != AnimKind::Cut && spec.out_dur_us > 0 {
        let tail = spec.stagger_us.max(0) * (n.saturating_sub(1 + slot)) as TimeUs;
        let start = dur_us - spec.out_dur_us - tail;
        let q = (t_us - start) as f32 / spec.out_dur_us as f32;
        let e = ease_in_quart(q);
        opacity *= 1.0 - e;
        let (ux, uy) = spec.out_kind.dir();
        dx += ux * SLIDE_TRAVEL * e;
        dy += uy * SLIDE_TRAVEL * e;
    }

    AnimSample { opacity, dx, dy }
}

/// The 9-point anchor as frame fractions, numpad layout (§17.6): returns
/// (fx, fy) with (0,0) = top-left, (1,1) = bottom-right. Out-of-range
/// anchors clamp to center.
pub fn anchor_frac(anchor: u8) -> (f32, f32) {
    if !(1..=9).contains(&anchor) {
        return (0.5, 0.5);
    }
    let a = anchor - 1;
    let fx = (a % 3) as f32 * 0.5;
    let fy = match a / 3 {
        0 => 1.0, // 1–3: bottom row
        1 => 0.5, // 4–6: middle
        _ => 0.0, // 7–9: top
    };
    (fx, fy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_json_roundtrip_and_unknown_fields() {
        let doc = GraphicDoc {
            background: Some([0.1, 0.2, 0.3, 1.0]),
            elements: vec![GraphicElement {
                kind: ElementKind::Text(TextBlock {
                    content: "Hello".into(),
                    ..TextBlock::default()
                }),
                ..GraphicElement::default()
            }],
            anim: AnimSpec::default(),
        };
        let json = doc.to_json();
        assert_eq!(GraphicDoc::from_json(&json), doc);
        // Unknown fields and missing fields both tolerated.
        assert_eq!(
            GraphicDoc::from_json("{\"mystery\":1}"),
            GraphicDoc::default()
        );
        assert_eq!(GraphicDoc::from_json("not json"), GraphicDoc::default());
    }

    #[test]
    fn fingerprint_tracks_content() {
        let a = GraphicDoc::default();
        let mut b = GraphicDoc::default();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.elements.push(GraphicElement::default());
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), 0);
    }

    #[test]
    fn anchor_grid_is_numpad() {
        assert_eq!(anchor_frac(7), (0.0, 0.0)); // top-left
        assert_eq!(anchor_frac(5), (0.5, 0.5)); // center
        assert_eq!(anchor_frac(3), (1.0, 1.0)); // bottom-right
        assert_eq!(anchor_frac(1), (0.0, 1.0)); // bottom-left
        assert_eq!(anchor_frac(9), (1.0, 0.0)); // top-right
        assert_eq!(anchor_frac(0), (0.5, 0.5)); // clamp
    }

    #[test]
    fn fade_endpoints() {
        let spec = AnimSpec::default(); // fade 400ms in / 250ms out
        let dur = 5_000_000;
        let s = anim_at(&spec, 0, 1, 0, dur);
        assert!(s.opacity < 0.01);
        let s = anim_at(&spec, 0, 1, 400_000, dur);
        assert!((s.opacity - 1.0).abs() < 1e-6);
        let s = anim_at(&spec, 0, 1, 2_500_000, dur);
        assert_eq!(s.opacity, 1.0);
        let s = anim_at(&spec, 0, 1, dur, dur);
        assert!(s.opacity < 0.01);
    }

    #[test]
    fn cut_has_no_ramp() {
        let spec = AnimSpec {
            in_kind: AnimKind::Cut,
            out_kind: AnimKind::Cut,
            ..AnimSpec::default()
        };
        let s = anim_at(&spec, 0, 3, 0, 1_000_000);
        assert_eq!(s.opacity, 1.0);
        let s = anim_at(&spec, 2, 3, 999_999, 1_000_000);
        assert_eq!(s.opacity, 1.0);
        assert_eq!((s.dx, s.dy), (0.0, 0.0));
    }

    #[test]
    fn slide_combines_fade_and_caps_travel() {
        let spec = AnimSpec {
            in_kind: AnimKind::Slideup,
            ..AnimSpec::default()
        };
        let s = anim_at(&spec, 0, 1, 0, 5_000_000);
        assert!(s.opacity < 0.01, "slides always combine with fade");
        // Starts displaced downward (enters moving up), capped at 5%.
        assert!((s.dy - SLIDE_TRAVEL).abs() < 0.5);
        assert_eq!(s.dx, 0.0);
        let s = anim_at(&spec, 0, 1, 400_000, 5_000_000);
        assert!(s.dy.abs() < 1e-3);
    }

    #[test]
    fn stagger_orders_in_and_out_topmost_first() {
        let spec = AnimSpec {
            stagger_us: 100_000,
            ..AnimSpec::default()
        };
        let dur = 5_000_000;
        // In: slot 0 leads.
        let a = anim_at(&spec, 0, 2, 150_000, dur);
        let b = anim_at(&spec, 1, 2, 150_000, dur);
        assert!(a.opacity > b.opacity);
        // Out: slot 0 also leaves first.
        let a = anim_at(&spec, 0, 2, dur - 200_000, dur);
        let b = anim_at(&spec, 1, 2, dur - 200_000, dur);
        assert!(a.opacity < b.opacity);
    }

    #[test]
    fn stagger_ranks_are_reading_order() {
        assert_eq!(stagger_ranks(&[300.0, 100.0, 200.0]), vec![2, 0, 1]);
        // Ties break by document order.
        assert_eq!(stagger_ranks(&[100.0, 100.0]), vec![0, 1]);
        assert_eq!(stagger_ranks(&[]), Vec::<usize>::new());
    }

    #[test]
    fn weight_and_align_cycles_close() {
        let mut w = Weight::Light;
        for _ in 0..5 {
            w = w.cycled();
        }
        assert_eq!(w, Weight::Light);
        let mut a = Align::Left;
        for _ in 0..3 {
            a = a.cycled();
        }
        assert_eq!(a, Align::Left);
    }
}
