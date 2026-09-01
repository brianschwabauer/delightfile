//! Font specimens — a typeface as a page of its own type.
//!
//! A font file has no picture in it. What it has is a *promise* about how text
//! will look, and the only honest way to show one in a preview pane is to set
//! some text in it. So this module composes a **specimen sheet** — the family
//! name, the alphabet, the digits, the punctuation, and a pangram waterfall —
//! and every line of it is set in the face being previewed, which is the whole
//! claim a specimen makes.
//!
//! ## Why this rasterises rather than going through SVG
//!
//! delightviewer builds the same sheet as an in-memory SVG document and hands
//! it to resvg, which then owns the text layout, the font database and the
//! rasteriser. delightfile has no resvg and is not going to gain one: the
//! preview pane's contract (see [`crate::preview::decode`]) is that a worker
//! thread hands back pixels, and paying for a full vector stack — usvg, a
//! fontdb walk of the system's faces, a tiny-skia — to draw four hundred
//! letterforms would be the largest dependency in the tree in service of the
//! smallest preview kind in the app.
//!
//! What is actually needed is much narrower. `ttf-parser` is already in the
//! build (epaint pulls 0.25 to read the fonts egui draws with), and it hands
//! back glyph outlines as move/line/quad/cubic callbacks. Turning those into
//! pixels is a curve flattener and a scanline fill with a non-zero winding
//! rule, which is what the second half of this file is. The result is the
//! *same* honest specimen — the text is genuinely set in the face on disk —
//! reached without a new dependency and without leaving the worker thread.
//!
//! There is no shaping here and no pretence of it: glyphs come straight off
//! `cmap` with `hmtx` advances, with **no kerning and no ligatures**. That is
//! right for a Latin alphabet row and would be wrong for Arabic, and it is
//! recorded as a limit rather than hidden.
//!
//! ## What a face that cannot read is allowed to do
//!
//! A symbol or icon font cannot set its own name, and a title that came out as
//! a row of empty boxes would be a specimen that failed at its one job. A
//! character with no `cmap` entry therefore draws **nothing at all** — no
//! notdef box, no fallback face — and the sheet carries on to the alphabet
//! rows, which are the part that is worth looking at anyway. A blank title on
//! a dingbat font is not a bug in the sheet; it is a fact about the file.

use ttf_parser::{Face, GlyphId, OutlineBuilder};

use super::{Ink, Rgba};

// ─── The sheet's measurements ───────────────────────────────────────────────
//
// Every one of these is in pixels of the canvas the pane asked for, because
// the pane is where the sheet is going and there is no page size in between.

/// The gap from the canvas edge to anything drawn, all four sides.
///
/// 18 px is roughly the pane's own inner padding at 1× and reads as a margin
/// rather than as a mistake; smaller and the alphabet touches the pane border,
/// larger and a narrow pane loses more of the pangram than it can afford.
const MARGIN: f32 = 18.0;

/// The family name's size, and the floor it may shrink to.
///
/// 32 px is display size — the name is the first thing on the sheet and should
/// look set rather than labelled. A long family in a narrow pane shrinks to
/// fit rather than being clipped, but never below 13, at which point it has
/// stopped being a title and clipping is the more honest failure.
const TITLE_SIZE: f32 = 32.0;
const TITLE_MIN: f32 = 13.0;

/// The title's line height. Tighter than the body's [`LINE_LEADING`] because a
/// single display line has no neighbour to be crowded by, and the space is
/// better spent on the waterfall.
const TITLE_LEADING: f32 = 1.28;

/// The quiet line under the title, and its floor.
///
/// 11 px is caption size: legible, clearly subordinate to the name, and short
/// enough that "Regular · TrueType" fits any pane wide enough to hold the
/// alphabet at all.
const LABEL_SIZE: f32 = 11.0;
const LABEL_MIN: f32 = 8.0;

/// The alphabet, digit and punctuation rows, and the floor they shrink to.
///
/// 17 px is the size at which a reader can see the shape of a letter without
/// the row wrapping in a pane narrower than the preview pane ever gets. The
/// floor is deliberately low: a 26-letter row is what the sheet is *for*, so
/// it shrinks a long way before it is allowed to be cut off.
const SET_SIZE: f32 = 17.0;
const SET_MIN: f32 = 7.0;

/// Body line height. 1.4 leaves room under the baseline for a descender at the
/// [`ASCENT_MAX`] end without rows touching, which is what a specimen's rows
/// must never do — two alphabets that collide read as one broken one.
const LINE_LEADING: f32 = 1.4;

/// The hairline rule's thickness and the air above and below it.
///
/// One pixel, because a rule that is visible as a *bar* competes with the type
/// it is separating. 9 px of air on each side is half a body line, which is
/// enough to read as a division rather than as an underline.
const RULE_H: f32 = 1.0;
const RULE_GAP: f32 = 9.0;

/// The waterfall, largest first.
///
/// Decreasing rather than increasing, which is the one place this sheet
/// departs from the classic printed specimen: a preview pane is a variable
/// height that is usually short, and running down from display size means the
/// row a reader most wants — the big one, where the face's character lives —
/// is the row guaranteed to be drawn. Each step is roughly 1.4× the next, far
/// enough apart that no two rows look like the same size twice.
const WATERFALL: &[f32] = &[38.0, 27.0, 19.0, 14.0, 10.0];

/// The pangram.
///
/// "Sphinx of black quartz, judge my vow" over the fox, for one reason: it is
/// 35 characters against the fox's 44, and in a preview pane a fifth of the
/// window's width is the difference between a waterfall row that reads as a
/// sentence and one that stops at "The quick brown". Both are pangrams; only
/// one of them survives a narrow pane.
const PANGRAM: &str = "Sphinx of black quartz, judge my vow";

/// The character sets, one row each.
///
/// Caps, lowercase, then figures and punctuation together on the last two
/// rows — the mixed rows are where a face's least considered glyphs live, and
/// a specimen that showed only letters would flatter it.
const SETS: &[&str] = &[
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "abcdefghijklmnopqrstuvwxyz",
    "0123456789 & @ # $ % ‰",
    ".,:;!?'\"“”()[]{}/\\-–—*+=<>",
];

/// How far above the baseline a row's ink is assumed to reach, as a fraction
/// of the type size, when the face's own `hhea` ascender is unusable.
const ASCENT_FALLBACK: f32 = 0.8;
/// The band a face's declared ascender is trusted within. Below 0.6 the rows
/// would crowd the row above; above 1.05 the baseline sits so low in its line
/// box that descenders leave the row. Fonts declare both, and neither is a
/// number this layout can honour.
const ASCENT_MIN: f32 = 0.6;
const ASCENT_MAX: f32 = 1.05;

// ─── The rasteriser's numbers ───────────────────────────────────────────────

/// How far a flattened curve may stray from the real one, in pixels.
///
/// A fifth of a pixel is under the [`SUBSAMPLES`] grid's own resolution, so
/// the flattening is never the thing a reader can see; the anti-aliasing is.
/// Halving it would roughly double the segment count for no visible gain.
const FLATTEN_TOLERANCE: f32 = 0.2;

/// The most line segments one curve may be cut into.
///
/// 48 is far more than [`FLATTEN_TOLERANCE`] asks for at any size this sheet
/// sets — a 38 px glyph's longest curve wants about eight — so this is not a
/// quality knob, it is a bound on what a font with absurd control points can
/// cost.
const MAX_CURVE_STEPS: usize = 48;

/// Sub-scanlines per pixel row.
///
/// Vertical supersampling; horizontal coverage is computed exactly from each
/// span's fractional ends, so four rows is enough to put a smooth edge on a
/// diagonal. Anti-aliasing matters more here than almost anywhere else in the
/// app: a *type* specimen rendered to hard pixels is a specimen of the
/// rasteriser, not of the typeface.
const SUBSAMPLES: usize = 4;

/// The most edges one filled shape may hold before it is abandoned.
///
/// A whole 26-letter row at 38 px is a few thousand; 80,000 is a font whose
/// outlines are broken or hostile, and the sheet drops that row rather than
/// spending a second of a worker thread on it.
const MAX_EDGES: usize = 80_000;

/// Coordinates further than this from the origin are discarded as nonsense.
/// A glyph cannot legitimately reach a million pixels from its own origin, and
/// letting one through would turn the scanline loop into a hang.
const MAX_COORD: f32 = 1.0e6;

/// The largest canvas the sheet will allocate, in pixels: 64 megapixels, or
/// 256 MiB of RGBA. A preview pane never asks for a tenth of that, so this is
/// purely a bound on a caller's arithmetic mistake.
const MAX_PIXELS: u64 = 64_000_000;

// ─── The magics ─────────────────────────────────────────────────────────────

/// TrueType outlines: version 1.0 written as a fixed-point number.
const MAGIC_TRUETYPE: &[u8] = &[0x00, 0x01, 0x00, 0x00];
/// Apple's older spelling of the same thing.
const MAGIC_APPLE: &[u8] = b"true";
/// OpenType with CFF outlines.
const MAGIC_OPENTYPE: &[u8] = b"OTTO";
/// A TrueType *collection* — several faces in one file.
const MAGIC_COLLECTION: &[u8] = b"ttcf";

/// What a font file says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub family: String,
    pub style: String,
    pub glyphs: u16,
    pub units_per_em: u16,
    /// How many faces the file holds — a `.ttc` collection is several.
    pub faces: u32,
    /// How many SFNT tables the face declares.
    pub tables: usize,
    pub variable: bool,
    /// "TrueType", "OpenType (CFF)", "collection" — how the file is spelled.
    pub flavour: &'static str,
}

/// Whether these bytes look like a font.
///
/// Decided by the magic rather than by the extension, because a `.otf` holding
/// TrueType outlines is a thing that exists and a file's own first four bytes
/// are the one part of it nobody renamed. WOFF and WOFF2 are deliberately not
/// recognised: they are compressed containers, and unwrapping one is a
/// decompressor this module does not have.
pub fn is_font(head: &[u8]) -> bool {
    head.starts_with(MAGIC_TRUETYPE)
        || head.starts_with(MAGIC_APPLE)
        || head.starts_with(MAGIC_OPENTYPE)
        || head.starts_with(MAGIC_COLLECTION)
}

/// Read the facts out of a font file's bytes. A collection reports its first
/// face.
///
/// A `.ttc` is a container of unrelated weights, and a preview that opened six
/// sheets would not be a preview; the face count is carried in [`Facts::faces`]
/// so the pane can say there is more in the file than it is showing.
pub fn facts(data: &[u8]) -> Result<Facts, String> {
    let faces = ttf_parser::fonts_in_collection(data).unwrap_or(1).max(1);
    let face = Face::parse(data, 0).map_err(|e| format!("font: {e}"))?;

    // Typographic family (16) before family (1). Name 16 is what a modern file
    // means by "the family"; name 1 is the legacy four-style grouping, which
    // on a large family says "Inter" where 16 says "Inter Display".
    let name = |ids: [u16; 2]| -> Option<String> {
        ids.iter().find_map(|id| {
            face.names()
                .into_iter()
                .filter(|n| n.name_id == *id)
                .find_map(|n| n.to_string())
                .filter(|s| !s.trim().is_empty())
        })
    };

    let flavour = if data.starts_with(MAGIC_COLLECTION) {
        "collection"
    } else if data.starts_with(MAGIC_OPENTYPE) || face.tables().cff.is_some() {
        "OpenType (CFF)"
    } else {
        "TrueType"
    };

    Ok(Facts {
        family: name([
            ttf_parser::name_id::TYPOGRAPHIC_FAMILY,
            ttf_parser::name_id::FAMILY,
        ])
        .unwrap_or_else(|| "Unnamed".to_string()),
        style: name([
            ttf_parser::name_id::TYPOGRAPHIC_SUBFAMILY,
            ttf_parser::name_id::SUBFAMILY,
        ])
        .unwrap_or_default(),
        glyphs: face.number_of_glyphs(),
        units_per_em: face.units_per_em(),
        faces,
        tables: face.raw_face().table_records.len() as usize,
        variable: face.is_variable(),
        flavour,
    })
}

/// One line for the pane's chip, e.g. "Inter · Regular · 2,548 glyphs ·
/// 2048 upem".
///
/// The glyph count is grouped and the units-per-em is not, which is not an
/// inconsistency: one is a quantity a reader compares ("this face has three
/// times as many glyphs as that one") and the other is an identifier from a
/// small set a typographer already knows by sight — 1000, 2048, 2000 — that
/// reads as a stranger with a comma in it.
pub fn summary(f: &Facts) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(6);
    parts.push(if f.family.trim().is_empty() {
        "Unnamed".to_string()
    } else {
        f.family.clone()
    });
    if !f.style.trim().is_empty() {
        parts.push(f.style.clone());
    }
    parts.push(format!("{} glyphs", grouped(u32::from(f.glyphs))));
    parts.push(format!("{} upem", f.units_per_em));
    if f.faces > 1 {
        parts.push(format!("{} faces", f.faces));
    }
    if f.variable {
        parts.push("variable".to_string());
    }
    parts.join(" · ")
}

/// Compose and rasterise the specimen sheet.
///
/// Everything is set in the previewed face. Rows that do not fit the canvas
/// are simply not drawn, so the sheet fills whatever pane it is handed and
/// never overflows one — which is why the waterfall runs downwards in size:
/// the steps that fall off the bottom of a short pane are the ones a reader
/// would have missed least.
pub fn specimen(
    data: &[u8],
    facts: &Facts,
    width: u32,
    height: u32,
    ink: &Ink,
) -> Result<Rgba, String> {
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("font: {width}×{height} is not a preview pane"));
    }
    let face = Face::parse(data, 0).map_err(|e| format!("font: {e}"))?;
    if face.units_per_em() == 0 {
        return Err("font: the face declares no units per em".to_string());
    }

    let mut canvas = Canvas::new(width, height, ink.bg);
    let left = MARGIN;
    let right = width as f32 - MARGIN;
    let bottom = height as f32 - MARGIN;
    let room = right - left;
    // A canvas with no room between its margins is not a failure, it is a pane
    // that is one pixel wide. It gets its background and nothing else.
    if room <= 0.0 || bottom <= MARGIN {
        return Ok(canvas.into_rgba());
    }

    let setter = Setter::new(face);
    let mut y = MARGIN;

    // ── The name, in the face it names ──────────────────────────────────────
    // The first thing a type specimen has done for five hundred years. If the
    // face cannot draw its own name, the line comes out blank and that is the
    // sheet's first honest statement about the file.
    let family = if facts.family.trim().is_empty() {
        "Unnamed"
    } else {
        facts.family.as_str()
    };
    let size = setter.fit(family, TITLE_SIZE, room, TITLE_MIN);
    if y + size * TITLE_LEADING <= bottom {
        setter.draw(&mut canvas, family, size, (left, y), right, ink.fg);
        y += size * TITLE_LEADING;
    }

    // ── What it is, quietly ─────────────────────────────────────────────────
    let label = label_line(facts);
    let size = setter.fit(&label, LABEL_SIZE, room, LABEL_MIN);
    if y + size * LINE_LEADING <= bottom {
        setter.draw(&mut canvas, &label, size, (left, y), right, ink.dim);
        y += size * LINE_LEADING;
    }

    // ── The one accent on the sheet ─────────────────────────────────────────
    //
    // The rule, not the family name. The name is set in the previewed face,
    // and a symbol font's name draws nothing at all — so colouring it accent
    // would mean the sheets that most need a point of colour are exactly the
    // sheets that have none. The rule is drawn by this module, from geometry
    // this module owns, and therefore always appears.
    if y + RULE_GAP * 2.0 + RULE_H <= bottom {
        y += RULE_GAP;
        canvas.rule(left, y, right, RULE_H, ink.accent);
        y += RULE_H + RULE_GAP;
    }

    // ── The character sets ──────────────────────────────────────────────────
    for set in SETS {
        let size = setter.fit(set, SET_SIZE, room, SET_MIN);
        if y + size * LINE_LEADING > bottom {
            break;
        }
        setter.draw(&mut canvas, set, size, (left, y), right, ink.fg);
        y += size * LINE_LEADING;
    }

    // A second division before the waterfall, in the label colour rather than
    // the accent: two accents on one sheet is two focal points, which is none.
    if y + RULE_GAP * 2.0 + RULE_H <= bottom {
        y += RULE_GAP;
        canvas.rule(left, y, right, RULE_H, ink.dim);
        y += RULE_H + RULE_GAP;
    }

    // ── The waterfall ───────────────────────────────────────────────────────
    //
    // `continue` rather than `break`: the sizes descend, so a step that will
    // not fit says nothing about the smaller ones after it, and a pane with
    // room for two more 10 px lines should get them.
    for size in WATERFALL {
        if y + size * LINE_LEADING > bottom {
            continue;
        }
        setter.draw(&mut canvas, PANGRAM, *size, (left, y), right, ink.fg);
        y += size * LINE_LEADING;
    }

    Ok(canvas.into_rgba())
}

/// The line under the name: what the face is, in the fewest words that are
/// still facts. Short on purpose — it is set in the previewed face at caption
/// size, and a long one would be the first thing a narrow pane had to shrink.
fn label_line(f: &Facts) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(4);
    if !f.style.trim().is_empty() {
        parts.push(f.style.clone());
    }
    parts.push(f.flavour.to_string());
    if f.faces > 1 {
        parts.push(format!("{} faces", f.faces));
    }
    if f.variable {
        parts.push("variable".to_string());
    }
    parts.join(" · ")
}

/// A number with thousands separators, written out by hand because a
/// formatting crate for one call site would be a dependency for a for-loop.
fn grouped(n: u32) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ─── Setting type ───────────────────────────────────────────────────────────

/// A face, and the one number the layout needs from it.
///
/// Everything the sheet draws goes through here, so a face is parsed once per
/// specimen rather than once per row.
struct Setter<'a> {
    face: Face<'a>,
    /// Where a row's ink starts above its baseline, as a fraction of the size.
    ascent: f32,
    /// Font units to pixels, per pixel of type size.
    per_em: f32,
}

impl<'a> Setter<'a> {
    fn new(face: Face<'a>) -> Setter<'a> {
        let upem = f32::from(face.units_per_em().max(1));
        let declared = f32::from(face.ascender()) / upem;
        let ascent = if declared.is_finite() && (ASCENT_MIN..=ASCENT_MAX).contains(&declared) {
            declared
        } else {
            ASCENT_FALLBACK
        };
        Setter {
            face,
            ascent,
            per_em: 1.0 / upem,
        }
    }

    /// A character's advance in pixels at `size`, and zero for a character the
    /// face has no glyph for — a missing character occupies no space at all,
    /// which is what makes a symbol font's title collapse to nothing instead of
    /// to a row of boxes.
    fn advance(&self, c: char, size: f32) -> f32 {
        let Some(id) = self.face.glyph_index(c) else {
            return 0.0;
        };
        let units = self.face.glyph_hor_advance(id).unwrap_or(0);
        f32::from(units) * self.per_em * size
    }

    /// How wide `text` would be at `size`.
    fn measure(&self, text: &str, size: f32) -> f32 {
        text.chars().map(|c| self.advance(c, size)).sum()
    }

    /// The largest size at or below `size` that fits `text` into `room`,
    /// never going below `floor`.
    ///
    /// Shrinking beats clipping for the sheet's fixed rows: an alphabet is
    /// about the shapes of twenty-six letters, and twenty of them at a
    /// comfortable size is a worse specimen than all of them at a small one.
    fn fit(&self, text: &str, size: f32, room: f32, floor: f32) -> f32 {
        let natural = self.measure(text, size);
        if !natural.is_finite() || natural <= room || natural <= 0.0 {
            return size;
        }
        (size * room / natural).max(floor.min(size))
    }

    /// Set `text` at `size` with the row's top-left at `at`, clipped to
    /// `max_x`, in `colour`.
    ///
    /// Clipping is whole glyphs: a row that stops between letters is a sample,
    /// and one cut down the middle of a letterform is a rendering bug the
    /// reader will blame on the font.
    fn draw(
        &self,
        canvas: &mut Canvas,
        text: &str,
        size: f32,
        at: (f32, f32),
        max_x: f32,
        colour: [u8; 4],
    ) {
        let baseline = at.1 + self.ascent * size;
        let mut pen = at.0;
        let mut shape = Shape::default();
        for c in text.chars() {
            let Some(id) = self.face.glyph_index(c) else {
                continue;
            };
            let advance = self.advance(c, size);
            if pen + advance > max_x && pen > at.0 {
                break;
            }
            self.outline(id, size, (pen, baseline), &mut shape);
            pen += advance;
        }
        shape.fill(canvas, colour);
    }

    /// Pull one glyph's outline into `shape`, in canvas space.
    fn outline(&self, id: GlyphId, size: f32, origin: (f32, f32), shape: &mut Shape) {
        let mut pen = Pen {
            shape,
            origin,
            scale: self.per_em * size,
            contour: Vec::new(),
            cursor: (0.0, 0.0),
        };
        // The bounding box it hands back is of no use here — the fill finds its
        // own bounds — and `None` is simply a glyph with no outline: a space, or
        // one whose content is all colour layers. Nothing to draw, nothing
        // wrong, and either way the pen has already been given whatever there
        // was.
        let _ = self.face.outline_glyph(id, &mut pen);
        pen.flush();
    }
}

/// `ttf-parser`'s outline callbacks, flattened into polygons in canvas space.
///
/// Two conversions and nothing else: font units to pixels, and **y up to y
/// down**, because a font measures from the baseline upwards and a canvas
/// measures from its top edge down.
struct Pen<'a> {
    shape: &'a mut Shape,
    origin: (f32, f32),
    scale: f32,
    contour: Vec<(f32, f32)>,
    cursor: (f32, f32),
}

impl Pen<'_> {
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin.0 + x * self.scale,
            self.origin.1 - y * self.scale,
        )
    }

    /// Hand the contour in progress to the shape. Called on `close`, and again
    /// at the end of the glyph, because a font is not obliged to close its
    /// last contour and a fill treats every contour as closed regardless.
    fn flush(&mut self) {
        if self.contour.len() >= 3 {
            self.shape.contours.push(std::mem::take(&mut self.contour));
        } else {
            self.contour.clear();
        }
    }
}

impl OutlineBuilder for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.cursor = self.at(x, y);
        self.contour.push(self.cursor);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.cursor = self.at(x, y);
        self.contour.push(self.cursor);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let c = self.at(x1, y1);
        let e = self.at(x, y);
        flatten_quad(&mut self.contour, self.cursor, c, e);
        self.cursor = e;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let c1 = self.at(x1, y1);
        let c2 = self.at(x2, y2);
        let e = self.at(x, y);
        flatten_cubic(&mut self.contour, self.cursor, c1, c2, e);
        self.cursor = e;
    }

    fn close(&mut self) {
        self.flush();
    }
}

/// How many chords a curve needs, from the size of its second difference.
///
/// Cutting a curve into `n` equal pieces leaves an error of about `d / (8n²)`
/// where `d` is the largest second difference of its control points, so `n` is
/// the square root of `d / (8 · tolerance)`. Clamped at both ends: at least one
/// segment so a degenerate curve is still a line, at most [`MAX_CURVE_STEPS`]
/// so a broken one is still bounded.
fn curve_steps(d: f32) -> usize {
    if !d.is_finite() || d <= 0.0 {
        return 1;
    }
    let n = (d / (8.0 * FLATTEN_TOLERANCE)).sqrt().ceil();
    if !n.is_finite() || n < 1.0 {
        1
    } else {
        (n as usize).min(MAX_CURVE_STEPS)
    }
}

/// Flatten a quadratic Bézier onto `out`, excluding its start point (which the
/// contour already holds) and including its end.
fn flatten_quad(out: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
    let d = (p0.0 - 2.0 * p1.0 + p2.0).hypot(p0.1 - 2.0 * p1.1 + p2.1);
    let n = curve_steps(d);
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        out.push((
            u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0,
            u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1,
        ));
    }
}

/// Flatten a cubic Bézier onto `out`, on the same terms as [`flatten_quad`].
/// CFF outlines are cubic where TrueType's are quadratic, so a sheet set in an
/// `.otf` goes entirely through this one.
fn flatten_cubic(
    out: &mut Vec<(f32, f32)>,
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
) {
    let a = (p0.0 - 2.0 * p1.0 + p2.0).hypot(p0.1 - 2.0 * p1.1 + p2.1);
    let b = (p1.0 - 2.0 * p2.0 + p3.0).hypot(p1.1 - 2.0 * p2.1 + p3.1);
    let n = curve_steps(3.0 * a.max(b));
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let (uu, tt) = (u * u, t * t);
        out.push((
            uu * u * p0.0 + 3.0 * uu * t * p1.0 + 3.0 * u * tt * p2.0 + tt * t * p3.0,
            uu * u * p0.1 + 3.0 * uu * t * p1.1 + 3.0 * u * tt * p2.1 + tt * t * p3.1,
        ));
    }
}

// ─── Filling ────────────────────────────────────────────────────────────────

/// A set of closed polygons in canvas space, filled as one.
///
/// A whole row of type is one shape rather than one per glyph, which is both
/// faster — the scanline walk pays for its edge list once — and more correct:
/// two glyphs whose ink overlaps in the same pixel blend once, so a tight pair
/// does not get a dark seam down the join.
#[derive(Default)]
struct Shape {
    contours: Vec<Vec<(f32, f32)>>,
}

impl Shape {
    /// Scanline-fill with the non-zero winding rule.
    ///
    /// Non-zero rather than even-odd because that is what TrueType and CFF both
    /// specify: a counter (the hole in an "o") is a contour wound the other way,
    /// and under even-odd a font whose contours are all wound the same way — of
    /// which there are plenty — would come out with holes where it meant solid.
    fn fill(&self, canvas: &mut Canvas, colour: [u8; 4]) {
        if canvas.width == 0 || canvas.height == 0 {
            return;
        }
        // (x0, y0, x1, y1), horizontals dropped: a horizontal edge crosses no
        // scanline and would only ever contribute a spurious winding.
        let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
        let (mut top, mut bot) = (f32::MAX, f32::MIN);
        for contour in &self.contours {
            for (i, &a) in contour.iter().enumerate() {
                // Wrapping, because every contour is treated as closed whether
                // or not the font bothered to say `close`.
                let b = contour[(i + 1) % contour.len()];
                if !a.0.is_finite() || !a.1.is_finite() || !b.0.is_finite() || !b.1.is_finite() {
                    continue;
                }
                if a.0.abs() > MAX_COORD
                    || a.1.abs() > MAX_COORD
                    || b.0.abs() > MAX_COORD
                    || b.1.abs() > MAX_COORD
                {
                    continue;
                }
                if a.1 == b.1 {
                    continue;
                }
                top = top.min(a.1.min(b.1));
                bot = bot.max(a.1.max(b.1));
                edges.push((a.0, a.1, b.0, b.1));
                if edges.len() > MAX_EDGES {
                    return;
                }
            }
        }
        if edges.is_empty() {
            return;
        }

        let first_row = top.floor().max(0.0) as u32;
        let last_row = (bot.ceil().max(0.0) as u32).min(canvas.height);
        let width = canvas.width as usize;
        let mut coverage = vec![0.0f32; width];
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        let weight = 1.0 / SUBSAMPLES as f32;

        for row in first_row..last_row {
            coverage.iter_mut().for_each(|c| *c = 0.0);
            for s in 0..SUBSAMPLES {
                let sy = row as f32 + (s as f32 + 0.5) * weight;
                crossings.clear();
                for &(x0, y0, x1, y1) in &edges {
                    // Half-open in y, so a vertex shared by two edges is
                    // counted exactly once and a shape has no pinholes at its
                    // corners.
                    let (lo, hi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
                    if sy < lo || sy >= hi {
                        continue;
                    }
                    let t = (sy - y0) / (y1 - y0);
                    crossings.push((x0 + t * (x1 - x0), if y1 > y0 { 1 } else { -1 }));
                }
                crossings
                    .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut winding = 0;
                let mut span_start = 0.0f32;
                for &(x, dir) in &crossings {
                    let was = winding;
                    winding += dir;
                    if was == 0 && winding != 0 {
                        span_start = x;
                    } else if was != 0 && winding == 0 {
                        add_span(&mut coverage, span_start, x, weight);
                    }
                }
            }
            for (x, cov) in coverage.iter().enumerate() {
                if *cov > 0.0 {
                    canvas.blend(x as u32, row, colour, cov.min(1.0));
                }
            }
        }
    }
}

/// Add a horizontal span's coverage to one pixel row, exactly.
///
/// The end pixels get the fraction of themselves the span actually covers,
/// which is where the horizontal half of the anti-aliasing comes from — the
/// vertical half is [`SUBSAMPLES`].
fn add_span(coverage: &mut [f32], x0: f32, x1: f32, weight: f32) {
    let limit = coverage.len() as f32;
    if !x0.is_finite() || !x1.is_finite() {
        return;
    }
    let x0 = x0.clamp(0.0, limit);
    let x1 = x1.clamp(0.0, limit);
    if x1 <= x0 {
        return;
    }
    let first = x0.floor() as usize;
    let last = (x1.ceil() as usize).min(coverage.len());
    for (offset, cell) in coverage[first.min(last)..last].iter_mut().enumerate() {
        let px = first + offset;
        let l = (px as f32).max(x0);
        let r = ((px + 1) as f32).min(x1);
        if r > l {
            *cell += weight * (r - l);
        }
    }
}

// ─── The canvas ─────────────────────────────────────────────────────────────

/// Tightly packed non-premultiplied RGBA8, which is what the pane's contract
/// asks for and what [`Rgba`] carries.
struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32, bg: [u8; 4]) -> Canvas {
        let count = width as usize * height as usize;
        let mut pixels = Vec::with_capacity(count * 4);
        for _ in 0..count {
            pixels.extend_from_slice(&bg);
        }
        Canvas {
            width,
            height,
            pixels,
        }
    }

    /// Source-over, in straight (non-premultiplied) alpha.
    ///
    /// The general form rather than the opaque-background shortcut, because
    /// the pane may hand down a translucent `bg` for a sheet that sits over
    /// something, and ink laid on a transparent ground has to come out with
    /// the right colour rather than the right colour multiplied by nothing.
    fn blend(&mut self, x: u32, y: u32, colour: [u8; 4], coverage: f32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let sa = f32::from(colour[3]) / 255.0 * coverage.clamp(0.0, 1.0);
        if sa <= 0.0 {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let Some(dst) = self.pixels.get_mut(i..i + 4) else {
            return;
        };
        let da = f32::from(dst[3]) / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.0 {
            dst.copy_from_slice(&[0, 0, 0, 0]);
            return;
        }
        for (d, s) in dst.iter_mut().zip(colour.iter()).take(3) {
            let src = f32::from(*s);
            let old = f32::from(*d);
            *d = ((src * sa + old * da * (1.0 - sa)) / out_a).clamp(0.0, 255.0) as u8;
        }
        dst[3] = (out_a * 255.0).clamp(0.0, 255.0) as u8;
    }

    /// A horizontal hairline from `x0` to `x1` with its top edge at `y`.
    ///
    /// Anti-aliased in both directions like everything else, so a sub-pixel
    /// thickness reads as a lighter line rather than disappearing on one
    /// canvas size and doubling on the next.
    fn rule(&mut self, x0: f32, y: f32, x1: f32, thickness: f32, colour: [u8; 4]) {
        if !x0.is_finite() || !x1.is_finite() || !y.is_finite() || !thickness.is_finite() {
            return;
        }
        if x1 <= x0 || thickness <= 0.0 {
            return;
        }
        let top = y.max(0.0);
        let bottom = (y + thickness).min(self.height as f32);
        let x_lo = x0.max(0.0);
        let x_hi = x1.min(self.width as f32);
        if bottom <= top || x_hi <= x_lo {
            return;
        }
        for row in top.floor() as u32..(bottom.ceil() as u32).min(self.height) {
            let v = ((row + 1) as f32).min(bottom) - (row as f32).max(top);
            if v <= 0.0 {
                continue;
            }
            for col in x_lo.floor() as u32..(x_hi.ceil() as u32).min(self.width) {
                let h = ((col + 1) as f32).min(x_hi) - (col as f32).max(x_lo);
                if h > 0.0 {
                    self.blend(col, row, colour, v * h);
                }
            }
        }
    }

    fn into_rgba(self) -> Rgba {
        Rgba {
            width: self.width,
            height: self.height,
            pixels: self.pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// The first `.ttf` or `.otf` this machine has, read into memory, or
    /// `None` on a machine with no fonts installed at all — which is a real
    /// configuration (a minimal container) and must not fail the suite.
    fn a_system_font() -> Option<Vec<u8>> {
        const ROOTS: &[&str] = &[
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/Library/Fonts",
            "/System/Library/Fonts",
        ];
        ROOTS
            .iter()
            .find_map(|r| find_font(Path::new(r), 0))
            .and_then(|p| std::fs::read(p).ok())
    }

    /// A bounded, sorted walk: sorted so the same machine picks the same face
    /// every run and a failure is reproducible, bounded so a symlink loop
    /// under `/usr/share/fonts` is a missing fixture rather than a hang.
    fn find_font(dir: &Path, depth: u32) -> Option<PathBuf> {
        if depth > 4 {
            return None;
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        entries.sort();
        let wanted = |p: &Path| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
                .unwrap_or(false)
        };
        if let Some(p) = entries.iter().find(|p| p.is_file() && wanted(p.as_path())) {
            return Some(p.clone());
        }
        entries
            .iter()
            .filter(|p| p.is_dir())
            .find_map(|p| find_font(p.as_path(), depth + 1))
    }

    /// What one fully covered pixel of `colour` over `ink.bg` comes out as.
    ///
    /// Computed rather than assumed to be `colour` itself, so the pixel tests
    /// keep holding if [`Ink::test`]'s palette is ever given a translucent
    /// entry — the assertion is about coverage, not about alpha.
    fn over_ground(ink: &Ink, colour: [u8; 4]) -> [u8; 4] {
        let mut one = Canvas::new(1, 1, ink.bg);
        one.blend(0, 0, colour, 1.0);
        [one.pixels[0], one.pixels[1], one.pixels[2], one.pixels[3]]
    }

    /// A hand-built `Facts`, so the wording tests do not depend on which fonts
    /// the machine happens to have.
    fn fixture() -> Facts {
        Facts {
            family: "Inter".into(),
            style: "Regular".into(),
            glyphs: 2548,
            units_per_em: 2048,
            faces: 1,
            tables: 17,
            variable: false,
            flavour: "TrueType",
        }
    }

    #[test]
    fn a_font_header_is_recognized_and_prose_and_a_png_are_not() {
        assert!(is_font(MAGIC_TRUETYPE));
        assert!(is_font(MAGIC_APPLE));
        assert!(is_font(MAGIC_OPENTYPE));
        assert!(is_font(MAGIC_COLLECTION));
        assert!(!is_font(b"Once upon a time there was a typeface."));
        assert!(!is_font(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]));
        assert!(!is_font(b"%PDF-1.7"));
        // Compressed web containers are deliberately not claimed.
        assert!(!is_font(b"wOFF"));
        assert!(!is_font(b"wOF2"));
        assert!(!is_font(&[]));
        assert!(!is_font(&[0x00, 0x01]));
    }

    #[test]
    fn a_real_font_says_a_family_a_glyph_count_and_a_units_per_em() {
        let Some(data) = a_system_font() else {
            eprintln!("no system font on this machine — skipping");
            return;
        };
        assert!(is_font(&data), "the file this walk found is not a font");
        let f = facts(&data).expect("a system font parses");
        assert!(!f.family.trim().is_empty(), "no family name");
        assert!(f.glyphs > 4, "{} glyphs is not a face", f.glyphs);
        // **Not a power of two.** TrueType outlines are conventionally 1024 or
        // 2048, but a CFF face is 1000 — the PostScript unit — and refusing
        // that would fail on half the fonts on a normal machine.
        let upem = f.units_per_em;
        assert!((16..=16384).contains(&upem), "{upem} units per em");
        assert!(f.tables > 4, "{} tables is not an SFNT", f.tables);
        assert!(f.faces >= 1);
        assert!(
            ["TrueType", "OpenType (CFF)", "collection"].contains(&f.flavour),
            "{}",
            f.flavour
        );
    }

    #[test]
    fn garbage_is_an_error_and_never_a_panic() {
        assert!(facts(&[]).is_err());
        assert!(facts(b"Once upon a time there was a typeface.").is_err());
        // The right magic and nothing behind it — the shape of a truncated
        // download, which is the failure a preview pane actually meets.
        assert!(facts(b"\x00\x01\x00\x00 and then nothing at all").is_err());
        assert!(facts(&vec![0u8; 4096]).is_err());
        let ink = Ink::test();
        assert!(specimen(b"not a font", &fixture(), 200, 200, &ink).is_err());
    }

    #[test]
    fn the_summary_names_the_family_the_style_and_the_counts() {
        assert_eq!(
            summary(&fixture()),
            "Inter · Regular · 2,548 glyphs · 2048 upem"
        );
        // A styleless face does not get an empty slot in the middle.
        let plain = Facts {
            style: String::new(),
            ..fixture()
        };
        assert_eq!(summary(&plain), "Inter · 2,548 glyphs · 2048 upem");
        // A collection and a variable font each say so, at the end.
        let ttc = Facts {
            faces: 4,
            variable: true,
            flavour: "collection",
            glyphs: 812,
            ..fixture()
        };
        assert_eq!(
            summary(&ttc),
            "Inter · Regular · 812 glyphs · 2048 upem · 4 faces · variable"
        );
        // An unnamed file is still named something.
        let nameless = Facts {
            family: "   ".into(),
            ..fixture()
        };
        assert!(summary(&nameless).starts_with("Unnamed · Regular"));
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(65535), "65,535");
    }

    #[test]
    fn a_specimen_fills_the_canvas_it_was_given_and_puts_ink_on_it() {
        let Some(data) = a_system_font() else {
            eprintln!("no system font on this machine — skipping");
            return;
        };
        let f = facts(&data).expect("parse");
        let ink = Ink::test();
        let sheet = specimen(&data, &f, 420, 560, &ink).expect("specimen");
        assert_eq!((sheet.width, sheet.height), (420, 560));
        assert_eq!(sheet.pixels.len(), 420 * 560 * 4);
        let inked = sheet
            .pixels
            .chunks_exact(4)
            .filter(|p| p[0..4] != ink.bg[0..4])
            .count();
        assert!(inked > 500, "only {inked} pixels differ from the ground");
        // Anti-aliasing is the point of the whole rasteriser: a specimen whose
        // every pixel is either ground or full ink is a specimen of a bitmap.
        // Every colour the sheet uses at *full* coverage, so what is left over
        // can only be a partly covered pixel.
        let full: Vec<[u8; 4]> = [ink.bg, ink.fg, ink.dim, ink.accent]
            .iter()
            .map(|c| over_ground(&ink, *c))
            .chain(std::iter::once(ink.bg))
            .collect();
        let partial = sheet
            .pixels
            .chunks_exact(4)
            .filter(|p| !full.iter().any(|f| f[0..4] == p[0..4]))
            .count();
        assert!(partial > 100, "only {partial} anti-aliased pixels");
    }

    #[test]
    fn the_same_font_and_the_same_canvas_draw_the_same_sheet_twice() {
        let Some(data) = a_system_font() else {
            eprintln!("no system font on this machine — skipping");
            return;
        };
        let f = facts(&data).expect("parse");
        let ink = Ink::test();
        let one = specimen(&data, &f, 300, 400, &ink).expect("first");
        let two = specimen(&data, &f, 300, 400, &ink).expect("second");
        assert_eq!(one.pixels, two.pixels, "the sheet is not deterministic");
    }

    #[test]
    fn a_specimen_survives_a_canvas_with_no_room_in_it() {
        let Some(data) = a_system_font() else {
            eprintln!("no system font on this machine — skipping");
            return;
        };
        let f = facts(&data).expect("parse");
        let ink = Ink::test();
        for (w, h) in [(0u32, 0u32), (1, 1), (0, 400), (400, 0), (12, 900), (2, 2)] {
            let sheet = specimen(&data, &f, w, h, &ink).expect("a tiny pane is not an error");
            assert_eq!((sheet.width, sheet.height), (w, h));
            assert_eq!(sheet.pixels.len(), w as usize * h as usize * 4);
        }
        // Tall and narrow: the margins leave a usable column, so rows draw and
        // the ones that do not fit are simply absent.
        let tall = specimen(&data, &f, 90, 1200, &ink).expect("tall");
        assert_eq!(tall.pixels.len(), 90 * 1200 * 4);
        // A canvas nobody could mean is refused rather than allocated.
        assert!(specimen(&data, &f, 100_000, 100_000, &ink).is_err());
    }

    #[test]
    fn a_flattened_curve_stays_on_the_curve_it_came_from() {
        // A quarter-circle-ish quadratic, 100 px across: every flattened point
        // has to lie on the real curve, and there have to be enough of them
        // that the chords are inside the tolerance.
        let (p0, p1, p2) = ((0.0, 0.0), (100.0, 0.0), (100.0, 100.0));
        let mut out = vec![p0];
        flatten_quad(&mut out, p0, p1, p2);
        assert!(out.len() > 4, "only {} points", out.len());
        assert_eq!(out.last().copied(), Some(p2), "the end point is exact");
        for w in out.windows(2) {
            let chord = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
            assert!(chord < 40.0, "a chord of {chord} px is not flat");
        }
        // A degenerate curve — all three points the same — is one segment and
        // not a division by zero.
        let mut flat = vec![(5.0, 5.0)];
        flatten_quad(&mut flat, (5.0, 5.0), (5.0, 5.0), (5.0, 5.0));
        assert_eq!(flat.len(), 2);

        // The cubic half, which is the path every CFF outline takes.
        let mut cubic = vec![(0.0, 0.0)];
        flatten_cubic(
            &mut cubic,
            (0.0, 0.0),
            (0.0, 60.0),
            (90.0, 60.0),
            (90.0, 0.0),
        );
        assert!(cubic.len() > 4, "only {} points", cubic.len());
        assert_eq!(cubic.last().copied(), Some((90.0, 0.0)));
        // The middle of a symmetric cubic bulges, so the flattening is
        // genuinely following the curve rather than drawing the chord.
        assert!(
            cubic.iter().any(|p| p.1 > 30.0),
            "the flattener drew a straight line"
        );
        // A curve the arithmetic cannot describe is still one segment, and one
        // that would ask for a million is capped instead of granted.
        assert_eq!(curve_steps(f32::NAN), 1);
        assert_eq!(curve_steps(-1.0), 1);
        assert_eq!(curve_steps(0.0), 1);
        assert_eq!(curve_steps(1.0e12), MAX_CURVE_STEPS);
    }

    #[test]
    fn a_filled_square_is_solid_inside_and_clean_outside() {
        let ink = Ink::test();
        let mut canvas = Canvas::new(20, 20, ink.bg);
        let shape = Shape {
            contours: vec![vec![(5.0, 5.0), (15.0, 5.0), (15.0, 15.0), (5.0, 15.0)]],
        };
        shape.fill(&mut canvas, ink.fg);
        let px = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * 20 + x) * 4;
            [
                canvas.pixels[i],
                canvas.pixels[i + 1],
                canvas.pixels[i + 2],
                canvas.pixels[i + 3],
            ]
        };
        let solid = over_ground(&ink, ink.fg);
        assert_eq!(px(10, 10), solid, "the middle is not solid");
        assert_eq!(px(5, 5), solid, "the first covered pixel is not solid");
        assert_eq!(px(14, 14), solid, "the last covered pixel is not solid");
        assert_eq!(px(4, 10), ink.bg, "ink leaked left");
        assert_eq!(px(15, 10), ink.bg, "ink leaked right");
        assert_eq!(px(10, 4), ink.bg, "ink leaked up");
        assert_eq!(px(10, 15), ink.bg, "ink leaked down");

        // Half a pixel over: the edge column comes out as partial coverage,
        // which is the anti-aliasing working.
        let mut soft = Canvas::new(20, 20, ink.bg);
        Shape {
            contours: vec![vec![(5.5, 5.0), (15.0, 5.0), (15.0, 15.0), (5.5, 15.0)]],
        }
        .fill(&mut soft, ink.fg);
        let i = (10 * 20 + 5) * 4;
        let edge = [soft.pixels[i], soft.pixels[i + 1], soft.pixels[i + 2]];
        assert_ne!(
            edge,
            [ink.bg[0], ink.bg[1], ink.bg[2]],
            "no coverage at all"
        );
        assert_ne!(edge, [solid[0], solid[1], solid[2]], "full coverage");
    }

    #[test]
    fn the_nonzero_rule_leaves_a_counter_hollow() {
        let ink = Ink::test();
        let mut canvas = Canvas::new(24, 24, ink.bg);
        // An outer ring wound one way and an inner one wound the other — which
        // is exactly how a font draws the bowl of an "o".
        Shape {
            contours: vec![
                vec![(2.0, 2.0), (22.0, 2.0), (22.0, 22.0), (2.0, 22.0)],
                vec![(8.0, 8.0), (8.0, 16.0), (16.0, 16.0), (16.0, 8.0)],
            ],
        }
        .fill(&mut canvas, ink.fg);
        let px = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * 24 + x) * 4;
            [
                canvas.pixels[i],
                canvas.pixels[i + 1],
                canvas.pixels[i + 2],
                canvas.pixels[i + 3],
            ]
        };
        let solid = over_ground(&ink, ink.fg);
        assert_eq!(px(4, 12), solid, "the ring is not filled");
        assert_eq!(px(12, 12), ink.bg, "the counter filled in");
        // Wind the inner contour the *same* way and non-zero fills it solid,
        // which is the behaviour that separates this rule from even-odd.
        let mut solid_canvas = Canvas::new(24, 24, ink.bg);
        Shape {
            contours: vec![
                vec![(2.0, 2.0), (22.0, 2.0), (22.0, 22.0), (2.0, 22.0)],
                vec![(8.0, 8.0), (16.0, 8.0), (16.0, 16.0), (8.0, 16.0)],
            ],
        }
        .fill(&mut solid_canvas, ink.fg);
        let i = (12 * 24 + 12) * 4;
        assert_eq!(
            solid_canvas.pixels[i..i + 4],
            solid[0..4],
            "the same winding twice should fill solid, not cancel"
        );
    }

    #[test]
    fn a_shape_that_is_nonsense_draws_nothing_rather_than_hanging() {
        let ink = Ink::test();
        let mut canvas = Canvas::new(16, 16, ink.bg);
        let before = canvas.pixels.clone();
        // Non-finite, absurd, degenerate and empty, in that order.
        for contours in [
            vec![vec![(f32::NAN, 0.0), (10.0, 0.0), (10.0, 10.0)]],
            vec![vec![(0.0, 0.0), (1.0e9, 0.0), (1.0e9, 1.0e9)]],
            vec![vec![(1.0, 1.0), (5.0, 1.0), (9.0, 1.0)]],
            vec![],
        ] {
            Shape { contours }.fill(&mut canvas, ink.fg);
        }
        assert_eq!(canvas.pixels, before, "nonsense put ink on the page");
        // …and a zero-sized canvas takes a fill without complaint.
        let mut nothing = Canvas::new(0, 0, ink.bg);
        Shape {
            contours: vec![vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0)]],
        }
        .fill(&mut nothing, ink.fg);
        assert!(nothing.pixels.is_empty());
    }

    #[test]
    fn a_row_is_shrunk_to_fit_before_it_is_ever_cut_off() {
        let Some(data) = a_system_font() else {
            eprintln!("no system font on this machine — skipping");
            return;
        };
        let face = Face::parse(&data, 0).expect("parse");
        let setter = Setter::new(face);
        let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let natural = setter.measure(alphabet, SET_SIZE);
        if natural <= 0.0 {
            eprintln!("the found face sets no Latin — skipping");
            return;
        }
        // Plenty of room: the size is untouched.
        assert_eq!(
            setter.fit(alphabet, SET_SIZE, natural * 2.0, SET_MIN),
            SET_SIZE
        );
        // Half the room: the size roughly halves, and the row then fits.
        let squeezed = setter.fit(alphabet, SET_SIZE, natural / 2.0, SET_MIN);
        assert!((SET_MIN..SET_SIZE).contains(&squeezed), "{squeezed}");
        assert!(setter.measure(alphabet, squeezed) <= natural / 2.0 + 0.5);
        // No room at all: the floor holds, and nothing divides by zero.
        assert!(setter.fit(alphabet, SET_SIZE, 0.0, SET_MIN) >= SET_MIN);
        assert_eq!(setter.fit("", SET_SIZE, 10.0, SET_MIN), SET_SIZE);
        // A character the face has no glyph for occupies no space.
        assert_eq!(setter.advance('\u{10FFFD}', SET_SIZE), 0.0);
        // The ascent is always a number the layout can use.
        assert!((ASCENT_MIN..=ASCENT_MAX).contains(&setter.ascent));
    }
}
