//! Chrome glyphs no loaded face can draw, drawn by the app instead.
//!
//! ## The gap
//!
//! The chrome is set in egui's stock faces — Ubuntu Light for words, Hack for
//! keys — with a Nerd Font appended behind them when one is installed
//! ([`crate::icons::install`]). A handful of the symbols the chrome spells with
//! are in the Nerd Font and in neither stock face: the menu's `✓` and `▸`, the
//! arrows in hints and task names, the rate badge's `◂`, the menu button's
//! `≡`, the hint strip's `⇧` and `⟷`. On a machine without a Nerd Font —
//! every stock Mac and Windows install, and CI's container — each of those
//! was the missing-glyph box, and it *measured* as the box too, which is a
//! box's width wherever the chrome lays one out: the menu's key column ran
//! into its own chevron.
//!
//! ## The answer: draw them
//!
//! Not a bundled font. The shapes are ten, a font file is megabytes and a
//! licence, and a second face in the fallback chain would answer for far more
//! than these ten. Not a substitute character either: there is no symbol the
//! stock faces are known to have that says "tick" or "submenu", which is how
//! the chrome came to reach for these in the first place.
//!
//! So each character in [`StandIn`]'s table is a small drawing — a tick as two
//! strokes, a small triangle filled, an arrow as a shaft and an open head —
//! made to the metrics the glyph would have had: it takes an advance of its own
//! in the line, sits on the line's baseline, centres on the stock face's math
//! axis (where `−` and `=` sit), and is drawn at the face's own stroke weight in
//! the text's colour, so it scales with the font size and the display the way
//! the letters beside it do. Strokes are round at their ends and corners, and a
//! stroke that runs along a pixel row is put on it, as egui puts the glyphs.
//!
//! ## Only where nothing else can
//!
//! A stand-in is drawn only for a character **no face in its family draws**.
//! Everything else — which, on a machine with a Nerd Font, is everything —
//! goes to egui exactly as it did: [`layout`] hands the job to egui untouched
//! and paints the galley egui gives back, so the chrome there stays the same
//! glyph, the same metrics and the same pixels.
//!
//! "No face draws it" is asked of the advance, not of egui's `has_glyph`:
//! that one also answers no for every character that happens to live in the
//! same face as egui's own missing-glyph box, which in the stock chain is
//! Noto Emoji.
//!
//! ## Not the file icons
//!
//! The row icons are Nerd Font private-use pictures, not symbols in running
//! text, and they have their own answer: without the font the icon column
//! turns into `ls -F`'s classifiers ([`crate::icons`]). Nothing here draws
//! a file icon.

use std::sync::Arc;

use egui::emath::GuiRounding as _;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, FontFamily, FontId, Galley, Painter, Pos2, Rect, Stroke, Vec2};

/// A character the app can draw for itself, when no face can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandIn {
    /// `✓`, U+2713: a menu row's tick and a done task in a markdown list.
    Check,
    /// `▸`, U+25B8: a menu row that opens a submenu.
    PointRight,
    /// `◂`, U+25C2: the playback strip's reverse rate.
    PointLeft,
    /// `←`, U+2190.
    Left,
    /// `↑`, U+2191.
    Up,
    /// `→`, U+2192: a task's "from → to", a symlink's target, a hint.
    Right,
    /// `↓`, U+2193.
    Down,
    /// `⇧`, U+21E7: Shift, in a hint.
    Shift,
    /// `≡`, U+2261: the app menu's button.
    Bars,
    /// `⟷`, U+27F7: "names ⟷ contents".
    Both,
}

/// Every character with a stand-in, and its stand-in.
const TABLE: [(char, StandIn); 10] = [
    ('✓', StandIn::Check),
    ('▸', StandIn::PointRight),
    ('◂', StandIn::PointLeft),
    ('←', StandIn::Left),
    ('↑', StandIn::Up),
    ('→', StandIn::Right),
    ('↓', StandIn::Down),
    ('⇧', StandIn::Shift),
    ('≡', StandIn::Bars),
    ('⟷', StandIn::Both),
];

fn stand_in(c: char) -> Option<StandIn> {
    TABLE
        .iter()
        .find(|(glyph, _)| *glyph == c)
        .map(|(_, stand_in)| *stand_in)
}

/// The stock face a stand-in sits beside, as fractions of the font size.
///
/// Numbers rather than a measurement: the first face of each family is always
/// egui's own ([`crate::icons::install`] appends and never replaces), so these
/// are the only two faces a stand-in ever sits next to.
/// `the_face_numbers_are_the_stock_faces_own` reads them back out of the font
/// files, so an egui that changed its faces would say so.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Face {
    /// Where `−` centres, above the baseline: the height symbols are centred
    /// on, so an arrow sits where a minus sign would.
    axis: f32,
    /// The height of a capital, which the tall stand-ins reach.
    cap: f32,
    /// The thickness of `−`'s bar. A stand-in is a symbol, and a face draws
    /// its symbols at this weight.
    stroke: f32,
    /// A monospace face's cell, which every stand-in in it fills so a column
    /// of keys stays a column. `None` for a proportional face.
    cell: Option<f32>,
}

/// Ubuntu Light, egui's proportional face.
const UBUNTU_LIGHT: Face = Face {
    axis: 0.289,
    cap: 0.693,
    stroke: 0.056,
    cell: None,
};

/// Hack, egui's monospace face.
const HACK: Face = Face {
    axis: 0.313,
    cap: 0.729,
    stroke: 0.083,
    cell: Some(0.602),
};

fn face(family: &FontFamily) -> Face {
    match family {
        FontFamily::Monospace => HACK,
        _ => UBUNTU_LIGHT,
    }
}

impl StandIn {
    /// The advance in a proportional face, in ems: about what a face that
    /// drew the glyph would give it. The small triangle is `>`'s width, the
    /// bars `=`'s, a horizontal arrow a little wider than either so its head
    /// has room.
    fn em(self) -> f32 {
        match self {
            StandIn::Check => 0.62,
            StandIn::PointRight | StandIn::PointLeft => 0.56,
            StandIn::Left | StandIn::Right => 0.80,
            StandIn::Up | StandIn::Down => 0.56,
            StandIn::Shift => 0.72,
            StandIn::Bars => 0.56,
            StandIn::Both => 1.20,
        }
    }

    /// The advance in `face`, in ems: [`Self::em`], or in a monospace face the
    /// whole number of cells nearest it.
    fn advance(self, face: Face) -> f32 {
        match face.cell {
            Some(cell) => cell * (self.em() / cell).round().max(1.0),
            None => self.em(),
        }
    }

    /// Draw this with `pen`, whose origin is the middle of the advance on the
    /// baseline. Coordinates are in ems, `y` up from the baseline.
    fn draw(self, pen: &Pen<'_>) {
        let face = pen.face;
        // Half the stroke, in ems: a stroke centred this high sits on the
        // baseline rather than through it.
        let rest = pen.stroke.width / 2.0 / pen.size;
        match self {
            // The short arm shallower than the long one, or it is a root sign.
            StandIn::Check => {
                pen.stroke(&[pen.at(-0.26, 0.23), pen.at(-0.08, rest), pen.at(0.26, 0.57)])
            }
            StandIn::PointRight | StandIn::PointLeft => {
                // About `>`'s height, filled: a submenu's promise is a solid
                // thing, and an outline at seven points is a smudge.
                let axis = pen.on_row(face.axis);
                let half = 0.20 * pen.size;
                let (back, tip) = match self {
                    StandIn::PointRight => (pen.on_edge(-0.17), pen.x(0.18)),
                    _ => (pen.on_edge(0.17), pen.x(-0.18)),
                };
                let (top, bottom) = (egui::pos2(back, axis - half), egui::pos2(back, axis + half));
                let point = egui::pos2(tip, axis);
                // Clockwise on screen either way round, as egui's fill prefers.
                pen.fill(match self {
                    StandIn::PointRight => vec![top, point, bottom],
                    _ => vec![bottom, point, top],
                });
            }
            StandIn::Left | StandIn::Right => {
                let axis = pen.on_row(face.axis);
                let (tail, tip) = (pen.x(-0.30), pen.x(0.30));
                let (tail, tip) = match self {
                    StandIn::Right => (tail, tip),
                    _ => (tip, tail),
                };
                let heads = [pen.head(egui::pos2(tip, axis), egui::vec2(tip - tail, 0.0))];
                pen.arrow(egui::pos2(tail, axis), egui::pos2(tip, axis), &heads);
            }
            StandIn::Up | StandIn::Down => {
                let x = pen.on_column(0.0);
                let (low, high) = (pen.y(rest), pen.y(face.cap - rest));
                let (tail, tip) = match self {
                    StandIn::Up => (low, high),
                    _ => (high, low),
                };
                let heads = [pen.head(egui::pos2(x, tip), egui::vec2(0.0, tip - tail))];
                pen.arrow(egui::pos2(x, tail), egui::pos2(x, tip), &heads);
            }
            StandIn::Shift => {
                // The keycap's arrow: a head on a stem, outlined. The stem's
                // two sides are a whole number of pixels either side of a
                // crisp middle, so both land on the grid and it stays
                // symmetrical.
                let mid = pen.on_column(0.0);
                let half = pen.whole_pixels(0.12);
                let (stem, stem_r) = (mid - half, mid + half);
                let (head, head_r) = (mid - 0.31 * pen.size, mid + 0.31 * pen.size);
                let shoulder = pen.on_row(0.37);
                let foot = pen.on_row(rest);
                let tip = egui::pos2(mid, pen.y(face.cap - rest));
                let outline = [
                    tip,
                    egui::pos2(head_r, shoulder),
                    egui::pos2(stem_r, shoulder),
                    egui::pos2(stem_r, foot),
                    egui::pos2(stem, foot),
                    egui::pos2(stem, shoulder),
                    egui::pos2(head, shoulder),
                ];
                pen.legs(&outline);
                pen.legs(&[egui::pos2(head, shoulder), tip]);
                pen.dots(&outline);
            }
            StandIn::Bars => {
                // `=` with a third bar through the axis. The gap is a whole
                // number of pixels, so the three bars are evenly spaced once
                // each is on its pixel row.
                let axis = pen.on_row(face.axis);
                let gap = pen.whole_pixels(0.19).max(2.0 / pen.ppp);
                let (left, right) = (pen.x(-0.21), pen.x(0.21));
                for y in [axis - gap, axis, axis + gap] {
                    pen.stroke(&[egui::pos2(left, y), egui::pos2(right, y)]);
                }
            }
            StandIn::Both => {
                let axis = pen.on_row(face.axis);
                let (left, right) = (
                    egui::pos2(pen.x(-0.50), axis),
                    egui::pos2(pen.x(0.50), axis),
                );
                let heads = [
                    pen.head(right, egui::vec2(1.0, 0.0)),
                    pen.head(left, egui::vec2(-1.0, 0.0)),
                ];
                pen.arrow(left, right, &heads);
            }
        }
    }
}

/// What a stand-in is drawn with: where its advance is, how big an em is,
/// and the stroke the face would use.
struct Pen<'a> {
    painter: &'a Painter,
    /// The middle of the advance, on the baseline.
    origin: Pos2,
    /// The font size, in points.
    size: f32,
    ppp: f32,
    face: Face,
    stroke: Stroke,
}

impl Pen<'_> {
    fn x(&self, em: f32) -> f32 {
        self.origin.x + em * self.size
    }

    fn y(&self, em: f32) -> f32 {
        self.origin.y - em * self.size
    }

    fn at(&self, x: f32, y: f32) -> Pos2 {
        egui::pos2(self.x(x), self.y(y))
    }

    /// `em` as a whole number of pixels, in points.
    fn whole_pixels(&self, em: f32) -> f32 {
        (em * self.size * self.ppp).round() / self.ppp
    }

    /// The pixel row a horizontal stroke `em` above the baseline is crisp on —
    /// where egui itself would put a line of this weight.
    fn on_row(&self, em: f32) -> f32 {
        let mut y = self.y(em);
        self.stroke.round_center_to_pixel(self.ppp, &mut y);
        y
    }

    /// The same for a vertical stroke.
    fn on_column(&self, em: f32) -> f32 {
        let mut x = self.x(em);
        self.stroke.round_center_to_pixel(self.ppp, &mut x);
        x
    }

    /// The pixel edge nearest `em`, for a filled shape's straight side.
    fn on_edge(&self, em: f32) -> f32 {
        self.x(em).round_to_pixels(self.ppp)
    }

    /// A stroke through `points`, round at both ends and at every corner.
    ///
    /// egui has no round cap or join, so a stroke is its legs, each drawn on
    /// its own, and a dot the stroke's width across on each point: the union
    /// of the two is the round-capped, round-joined line. The pieces overlap
    /// where they meet. In an opaque ink, which every palette colour is, that
    /// does not show; while a surface fades it is a slightly darker joint for
    /// the length of the fade.
    fn stroke(&self, points: &[Pos2]) {
        self.legs(points);
        self.dots(points);
    }

    fn legs(&self, points: &[Pos2]) {
        for leg in points.windows(2) {
            self.painter.line_segment([leg[0], leg[1]], self.stroke);
        }
    }

    fn dots(&self, points: &[Pos2]) {
        for point in points {
            self.painter
                .circle_filled(*point, self.stroke.width / 2.0, self.stroke.color);
        }
    }

    /// A solid convex shape.
    fn fill(&self, points: Vec<Pos2>) {
        self.painter.add(egui::Shape::convex_polygon(
            points,
            self.stroke.color,
            Stroke::NONE,
        ));
    }

    /// An open head at `tip` pointing along `towards`: a barb either side,
    /// each [`BARB`] back and [`BARB`] out, which is forty-five degrees.
    fn head(&self, tip: Pos2, towards: Vec2) -> [Pos2; 3] {
        let back = towards.normalized() * BARB * self.size;
        let out = back.rot90();
        [tip - back + out, tip, tip - back - out]
    }

    /// A shaft from `tail` to `tip` with `heads` on it, as one stroke.
    fn arrow(&self, tail: Pos2, tip: Pos2, heads: &[[Pos2; 3]]) {
        self.stroke(&[tail, tip]);
        for head in heads {
            self.legs(head);
            self.dots(&[head[0], head[2]]);
        }
    }
}

/// How far an arrowhead's barbs reach, back and out, in ems.
const BARB: f32 = 0.19;

/// Whether some face in `font`'s family draws `c`.
///
/// Asked of the advance: a face without the character gives it none, and a
/// face with it gives it one — including the face egui's own missing-glyph
/// box comes from, which `has_glyph` wrongly answers no for.
pub fn drawn_by_a_face(ctx: &egui::Context, font: &FontId, c: char) -> bool {
    ctx.fonts_mut(|fonts| fonts.glyph_width(font, c) > 0.0)
}

/// Whether `c` is drawn in `font` at all: by a face, or by a stand-in here.
#[cfg(test)]
pub fn drawn(ctx: &egui::Context, font: &FontId, c: char) -> bool {
    stand_in(c).is_some() || drawn_by_a_face(ctx, font, c)
}

/// The stand-in `c` needs in `font`, if no face draws it.
fn needed(painter: &Painter, font: &FontId, c: char) -> Option<StandIn> {
    stand_in(c).filter(|_| !drawn_by_a_face(painter.ctx(), font, c))
}

fn section_text<'a>(job: &'a LayoutJob, section: &egui::text::LayoutSection) -> &'a str {
    job.text
        .get(section.byte_range.start.0..section.byte_range.end.0)
        .unwrap_or_default()
}

/// A line of chrome text, laid out and ready to paint.
///
/// When every character has a face this is egui's own galley and nothing
/// else. When one has none it is the runs of text either side of it, each
/// egui's, with the stand-in drawn in the gap between them.
pub struct Line {
    size: Vec2,
    laid: Laid,
}

enum Laid {
    Galley(Arc<Galley>),
    Pieces(Vec<Piece>),
}

/// One run of a pieced line, `x` points from its start.
struct Piece {
    x: f32,
    ink: Ink,
}

enum Ink {
    Text(Arc<Galley>),
    Drawn(Drawn),
}

/// A stand-in placed in a line: which one, in what, and where its baseline is.
struct Drawn {
    stand_in: StandIn,
    font: FontId,
    color: Color32,
    /// The advance, in points.
    advance: f32,
    /// From the top of the line to the baseline, in points.
    baseline: f32,
    /// The height of a line of text in `font`.
    height: f32,
}

impl Drawn {
    fn new(painter: &Painter, stand_in: StandIn, format: &TextFormat) -> Drawn {
        let font = format.font_id.clone();
        // Where egui puts the baseline in this font: read off a letter the
        // family's first face has, laid out the way the text around it is.
        let probe = painter.layout_job(LayoutJob::simple(
            "x".to_owned(),
            font.clone(),
            Color32::WHITE,
            f32::INFINITY,
        ));
        let height = probe.size().y;
        let baseline = probe
            .rows
            .first()
            .and_then(|row| Some(row.pos.y + row.row.glyphs.first()?.pos.y))
            .unwrap_or(height * 0.8);
        Drawn {
            stand_in,
            advance: stand_in.advance(face(&font.family)) * font.size,
            font,
            color: format.color,
            baseline,
            height,
        }
    }

    fn paint(&self, painter: &Painter, top_left: Pos2, fallback: Color32) {
        let ppp = painter.pixels_per_point();
        let face = face(&self.font.family);
        let color = if self.color == Color32::PLACEHOLDER {
            fallback
        } else {
            self.color
        };
        let left = top_left.x.round_to_pixels(ppp);
        // Never under a pixel: the face's own stems are hinted to one at the
        // sizes where they would be thinner, and a stand-in thinner than the
        // letters beside it reads as a fainter colour rather than a lighter
        // weight.
        let width = (face.stroke * self.font.size).max(1.0 / ppp);
        let pen = Pen {
            painter,
            origin: egui::pos2(left + self.advance / 2.0, top_left.y + self.baseline),
            size: self.font.size,
            ppp,
            face,
            stroke: Stroke::new(width, color),
        };
        self.stand_in.draw(&pen);
    }
}

impl Line {
    /// The line's size, as a galley's would be.
    pub fn size(&self) -> Vec2 {
        self.size
    }

    /// Paint the line with its top-left at `pos`. `fallback` is the colour of
    /// any part laid out in [`Color32::PLACEHOLDER`], as with
    /// [`Painter::galley`].
    pub fn paint(self, painter: &Painter, pos: Pos2, fallback: Color32) {
        match self.laid {
            Laid::Galley(galley) => painter.galley(pos, galley, fallback),
            Laid::Pieces(pieces) => {
                // Where the tessellator would put a galley laid at `pos`.
                let origin = pos.round_to_pixels(painter.pixels_per_point());
                for piece in pieces {
                    let at = origin + egui::vec2(piece.x, 0.0);
                    match piece.ink {
                        Ink::Text(galley) => painter.galley(at, galley, fallback),
                        Ink::Drawn(drawn) => drawn.paint(painter, at, fallback),
                    }
                }
            }
        }
    }
}

/// Lay out a line of chrome text: the one door every glyph the chrome draws
/// goes through, and the twin of [`Painter::layout_job`].
///
/// With a face for every character, this is `painter.layout_job(job)` and
/// nothing more. Otherwise the stand-ins are placed in the line, and a
/// `job.wrap` that cuts the line at a width cuts it the same way, `…` and all.
/// One line only: the chrome sets its text a line at a time, and a stand-in
/// never wraps.
pub fn layout(painter: &Painter, job: LayoutJob) -> Line {
    let stands_in = !job.text.is_ascii()
        && job.sections.iter().any(|section| {
            section_text(&job, section)
                .chars()
                .any(|c| needed(painter, &section.format.font_id, c).is_some())
        });
    if !stands_in {
        let galley = painter.layout_job(job);
        return Line {
            size: galley.size(),
            laid: Laid::Galley(galley),
        };
    }
    pieced(painter, &job)
}

/// [`layout`] for one run of text in one font and colour, never cut: the
/// twin of [`Painter::layout_no_wrap`].
pub fn line(painter: &Painter, text: String, font: FontId, color: Color32) -> Line {
    layout(painter, LayoutJob::simple(text, font, color, f32::INFINITY))
}

/// Draw `text` anchored at `pos`, and say where it went: the twin of
/// [`Painter::text`].
pub fn text(
    painter: &Painter,
    pos: Pos2,
    anchor: Align2,
    text: impl ToString,
    font: FontId,
    color: Color32,
) -> Rect {
    let line = line(painter, text.to_string(), font, color);
    let rect = anchor.anchor_size(pos, line.size());
    line.paint(painter, rect.min, color);
    rect
}

/// How wide `text` is in `font`, stand-ins and all.
pub fn width(painter: &Painter, text: &str, font: FontId) -> f32 {
    // Measured, never drawn: the colour is no part of the width.
    line(painter, text.to_owned(), font, Color32::WHITE)
        .size()
        .x
}

/// A stretch of a job between stand-ins, or a stand-in.
enum Atom<'a> {
    Text {
        text: &'a str,
        leading: f32,
        format: &'a TextFormat,
    },
    Drawn {
        stand_in: StandIn,
        leading: f32,
        format: &'a TextFormat,
    },
}

/// The job, cut at every character that needs a stand-in.
fn atoms<'a>(painter: &Painter, job: &'a LayoutJob) -> Vec<Atom<'a>> {
    let mut atoms = Vec::new();
    for section in &job.sections {
        let text = section_text(job, section);
        let format = &section.format;
        // A section's leading space goes before whatever comes first in it.
        let mut leading = section.leading_space;
        let mut start = 0;
        for (at, c) in text.char_indices() {
            let Some(stand_in) = needed(painter, &format.font_id, c) else {
                continue;
            };
            if start < at {
                atoms.push(Atom::Text {
                    text: &text[start..at],
                    leading,
                    format,
                });
                leading = 0.0;
            }
            atoms.push(Atom::Drawn {
                stand_in,
                leading,
                format,
            });
            leading = 0.0;
            start = at + c.len_utf8();
        }
        if start < text.len() {
            atoms.push(Atom::Text {
                text: &text[start..],
                leading,
                format,
            });
        }
    }
    atoms
}

/// A job of its own for a run of text atoms: `job`'s settings, its sections'
/// formats, one line at the most `max_width` wide.
fn part(job: &LayoutJob, atoms: &[Atom<'_>], max_width: f32) -> LayoutJob {
    let mut part = LayoutJob {
        text: String::new(),
        sections: Vec::new(),
        wrap: TextWrapping {
            max_width,
            max_rows: 1,
            ..job.wrap.clone()
        },
        halign: egui::Align::LEFT,
        justify: false,
        ..job.clone()
    };
    for atom in atoms {
        if let Atom::Text {
            text,
            leading,
            format,
        } = atom
        {
            part.append(text, *leading, (*format).clone());
        }
    }
    part
}

/// `galley`'s own job again, cut to `room`.
fn cut(painter: &Painter, galley: &Galley, room: f32) -> Arc<Galley> {
    let mut job = (*galley.job).clone();
    job.wrap.max_width = room.max(0.0);
    painter.layout_job(job)
}

/// The line with its stand-ins: text runs laid out by egui, stand-ins placed
/// between them, and the whole cut at `job.wrap.max_width` the way egui would
/// cut it.
fn pieced(painter: &Painter, job: &LayoutJob) -> Line {
    let atoms = atoms(painter, job);
    let room = job.wrap.max_width;
    let mut pieces: Vec<Piece> = Vec::new();
    let mut x = 0.0;
    let mut height: f32 = 0.0;
    let mut rest = atoms.as_slice();
    while let Some(first) = rest.first() {
        match first {
            Atom::Text { .. } => {
                let run = rest
                    .iter()
                    .take_while(|atom| matches!(atom, Atom::Text { .. }))
                    .count();
                let (texts, after) = rest.split_at(run);
                let mut galley = painter.layout_job(part(job, texts, f32::INFINITY));
                let over = x + galley.size().x > room;
                if over {
                    galley = cut(painter, &galley, room - x);
                }
                height = height.max(galley.size().y);
                let width = galley.size().x;
                pieces.push(Piece {
                    x,
                    ink: Ink::Text(galley),
                });
                x += width;
                if over {
                    break;
                }
                rest = after;
            }
            Atom::Drawn {
                stand_in,
                leading,
                format,
            } => {
                let drawn = Drawn::new(painter, *stand_in, format);
                height = height.max(drawn.height);
                if x + leading + drawn.advance > room {
                    x = close(painter, job, format, &mut pieces, x, room);
                    break;
                }
                let advance = drawn.advance;
                pieces.push(Piece {
                    x: x + leading,
                    ink: Ink::Drawn(drawn),
                });
                x += leading + advance;
                rest = &rest[1..];
            }
        }
    }
    Line {
        size: egui::vec2(x, height),
        laid: Laid::Pieces(pieces),
    }
}

/// End a line that ran out of room at a stand-in: the overflow character
/// where the stand-in would have gone, or, where even that does not fit,
/// pieces taken back off the end until it does — a stand-in whole, a run of
/// text cut short enough to leave the character room after it. Returns where
/// the line now ends.
fn close(
    painter: &Painter,
    job: &LayoutJob,
    format: &TextFormat,
    pieces: &mut Vec<Piece>,
    mut x: f32,
    room: f32,
) -> f32 {
    let Some(overflow) = job.wrap.overflow_character else {
        return x;
    };
    let mark = painter.layout_job(LayoutJob::simple_singleline(
        overflow.to_string(),
        format.font_id.clone(),
        format.color,
    ));
    let width = mark.size().x;
    while x + width > room {
        let Some(last) = pieces.pop() else {
            break;
        };
        x = last.x;
        if let Ink::Text(galley) = last.ink {
            let mut job = (*galley.job).clone();
            job.wrap.max_width = (room - width - last.x).max(0.0);
            job.wrap.overflow_character = None;
            let galley = painter.layout_job(job);
            x += galley.size().x;
            pieces.push(Piece {
                x: last.x,
                ink: Ink::Text(galley),
            });
            break;
        }
    }
    pieces.push(Piece {
        x,
        ink: Ink::Text(mark),
    });
    x + width
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame over egui's stock faces alone at `ppp` — no Nerd Font,
    /// whatever this machine has, which is the condition every stand-in is
    /// for — and the shapes `draw` painted in it.
    fn stock(ppp: f32, draw: impl FnMut(&Painter)) -> Vec<egui::Shape> {
        frame(&egui::Context::default(), ppp, draw)
    }

    fn frame(ctx: &egui::Context, ppp: f32, mut draw: impl FnMut(&Painter)) -> Vec<egui::Shape> {
        let mut input = egui::RawInput::default();
        input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(ppp),
                ..Default::default()
            },
        );
        ctx.run_ui(input, |ui| draw(ui.painter()))
            .shapes
            .into_iter()
            .map(|clipped| clipped.shape)
            .filter(|shape| !matches!(shape, egui::Shape::Noop))
            .collect()
    }

    /// The colour a stand-in's shape is inked in; `None` for anything that
    /// is not one of the shapes a stand-in is made of.
    fn ink(shape: &egui::Shape) -> Option<Color32> {
        match shape {
            egui::Shape::LineSegment { stroke, .. } => Some(stroke.color),
            egui::Shape::Circle(circle) => Some(circle.fill),
            egui::Shape::Path(path) => Some(path.fill),
            _ => None,
        }
    }

    fn texts(shapes: &[egui::Shape]) -> Vec<&egui::epaint::TextShape> {
        shapes
            .iter()
            .filter_map(|shape| match shape {
                egui::Shape::Text(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    /// The premise: none of the ten is in a stock proportional face, so on a
    /// machine without a Nerd Font every one of them is a stand-in. An egui
    /// that grew one of them would fail here, and that stand-in could go.
    #[test]
    fn no_stock_face_draws_the_covered_glyphs() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |_| {});
        for (c, _) in TABLE {
            assert!(
                !drawn_by_a_face(&ctx, &FontId::proportional(12.5), c),
                "a stock face draws {c:?} now"
            );
        }
    }

    /// Every stand-in is drawn — as strokes and fills, no text — in the
    /// colour of the text it stands in, inside its own advance and between
    /// the baseline and the top of a capital, in both families and at both
    /// the scales a display has.
    #[test]
    fn every_stand_in_draws_in_its_box_and_its_texts_colour() {
        let color = Color32::from_rgb(0x12, 0x34, 0x56);
        for (c, stand_in) in TABLE {
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                for ppp in [1.0, 2.0] {
                    let format = TextFormat::simple(FontId::new(12.5, family.clone()), color);
                    let at = egui::pos2(10.0, 10.0);
                    let mut placed = (0.0, 0.0);
                    let shapes = stock(ppp, |painter| {
                        let drawn = Drawn::new(painter, stand_in, &format);
                        placed = (drawn.advance, drawn.baseline);
                        drawn.paint(painter, at, Color32::RED);
                    });
                    let what = format!("{c:?} in {family:?} at {ppp}x");
                    assert!(!shapes.is_empty(), "{what} draws nothing");
                    let (advance, baseline) = placed;
                    let face = face(&family);
                    // A pixel of slack for the snapping, and the stroke's
                    // own half-width past the points it runs through.
                    let slack = 1.0 / ppp + face.stroke * 12.5;
                    let base = at.y + baseline;
                    for shape in &shapes {
                        assert_eq!(ink(shape), Some(color), "{what}: {shape:?}");
                        let bounds = shape.visual_bounding_rect();
                        assert!(
                            bounds.left() >= at.x - slack
                                && bounds.right() <= at.x + advance + slack,
                            "{what} leaves its advance: {bounds:?}"
                        );
                        assert!(
                            bounds.bottom() <= base + slack
                                && bounds.top() >= base - face.cap * 12.5 - slack,
                            "{what} leaves the line: {bounds:?} over a baseline at {base}"
                        );
                    }
                }
            }
        }
    }

    /// A stand-in takes an advance the way a glyph does: a sane share of the
    /// font size, and the same share at every size, so it scales with the
    /// text. This is the width the menu's key column is measured against.
    #[test]
    fn a_stand_in_measures_like_a_glyph() {
        for (c, stand_in) in TABLE {
            for size in [9.0, 12.5, 20.0] {
                let mut measured = 0.0;
                let _ = stock(1.0, |painter| {
                    measured = width(painter, &c.to_string(), FontId::proportional(size));
                });
                assert!(
                    measured > 0.3 * size && measured < 1.5 * size,
                    "{c:?} at {size}: {measured}"
                );
                assert!(
                    (measured / size - stand_in.em()).abs() < 1e-4,
                    "{c:?} at {size}: {measured}"
                );
            }
        }
    }

    /// In a monospace face a stand-in fills whole cells, so a column of keys
    /// stays a column.
    #[test]
    fn a_monospace_stand_in_fills_whole_cells() {
        let cell = HACK.cell.unwrap_or_default();
        assert_eq!(StandIn::Check.advance(HACK), cell);
        assert_eq!(StandIn::Right.advance(HACK), cell);
        assert_eq!(StandIn::Both.advance(HACK), cell * 2.0);
        assert_eq!(StandIn::Check.advance(UBUNTU_LIGHT), StandIn::Check.em());
    }

    /// Where a face has the glyph, nothing here happens: the job goes to egui
    /// untouched and comes back as egui's own galley, painted as one text
    /// shape. Hack has the arrows, so this holds on any machine.
    #[test]
    fn a_face_that_has_the_glyph_draws_it() {
        let font = FontId::monospace(12.5);
        let shapes = stock(1.0, |painter| {
            assert!(drawn_by_a_face(painter.ctx(), &font, '→'));
            let job = LayoutJob::simple(
                "Alt+→ ↑↓".to_owned(),
                font.clone(),
                Color32::WHITE,
                f32::INFINITY,
            );
            let line = layout(painter, job.clone());
            let Laid::Galley(galley) = &line.laid else {
                panic!("a face draws every character, and the line was pieced");
            };
            assert!(Arc::ptr_eq(galley, &painter.layout_job(job)));
            line.paint(painter, egui::pos2(0.0, 0.0), Color32::WHITE);
        });
        assert_eq!(shapes.len(), 1, "{shapes:?}");
        assert_eq!(texts(&shapes).len(), 1);
    }

    /// …and with a Nerd Font installed, the menu's tick and chevron are the
    /// font's again. Skipped on a machine without one.
    #[test]
    fn an_installed_font_takes_the_glyphs_back() {
        let ctx = egui::Context::default();
        if !crate::icons::install(&ctx) {
            return;
        }
        let font = FontId::proportional(12.5);
        let _ = frame(&ctx, 1.0, |painter| {
            for c in ['✓', '▸'] {
                let line = line(painter, c.to_string(), font.clone(), Color32::WHITE);
                assert!(matches!(line.laid, Laid::Galley(_)), "{c:?} was pieced");
            }
        });
    }

    /// A stand-in in the middle of a line leaves the words either side where
    /// they would be around a glyph of its width: `names ⟷ contents` is the
    /// two runs and the arrow's advance, laid end to end.
    #[test]
    fn the_text_either_side_of_a_stand_in_stays_put() {
        let font = FontId::proportional(12.5);
        let mut widths = (0.0, 0.0, 0.0);
        let shapes = stock(1.0, |painter| {
            widths = (
                width(painter, "names ", font.clone()),
                width(painter, " contents", font.clone()),
                width(painter, "names ⟷ contents", font.clone()),
            );
            text(
                painter,
                egui::pos2(0.0, 0.0),
                Align2::LEFT_TOP,
                "names ⟷ contents",
                font.clone(),
                Color32::WHITE,
            );
        });
        let (before, after, whole) = widths;
        let arrow = StandIn::Both.em() * 12.5;
        assert!(
            (whole - (before + arrow + after)).abs() < 1e-3,
            "{widths:?}"
        );
        let runs = texts(&shapes);
        assert_eq!(runs.len(), 2, "{shapes:?}");
        assert_eq!(runs[0].galley.text(), "names ");
        assert_eq!(runs[1].galley.text(), " contents");
        assert!((runs[1].pos.x - (before + arrow)).abs() < 1e-3);
        assert!(shapes.iter().any(|shape| ink(shape).is_some()));
    }

    /// A line cut at a width is cut as egui cuts one: never wider than the
    /// room, and ending in `…` — whether the cut falls in the text after the
    /// stand-in, on the stand-in itself, or before it.
    #[test]
    fn a_cut_line_still_ends_in_an_ellipsis() {
        let font = FontId::proportional(12.5);
        let cut = |room: f32| {
            let mut job = LayoutJob::simple(
                "Copy 3 items → /home/brian/Work/a/long/way/down".to_owned(),
                font.clone(),
                Color32::WHITE,
                room,
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            job.wrap.overflow_character = Some('…');
            job
        };
        let mut head = 0.0;
        let _ = stock(1.0, |painter| {
            head = width(painter, "Copy 3 items ", font.clone());
        });
        let arrow = StandIn::Right.em() * 12.5;
        for room in [head + arrow * 0.5, head + arrow + 40.0, head * 0.5] {
            let mut size = Vec2::ZERO;
            let shapes = stock(1.0, |painter| {
                let line = layout(painter, cut(room));
                size = line.size();
                line.paint(painter, egui::pos2(0.0, 0.0), Color32::WHITE);
            });
            assert!(size.x <= room + 1e-3, "{size:?} in {room}");
            let last = texts(&shapes)
                .into_iter()
                .max_by(|a, b| a.pos.x.total_cmp(&b.pos.x))
                .map(|text| (text.galley.elided, text.galley.text().to_owned()));
            assert!(
                matches!(&last, Some((true, _))) || matches!(&last, Some((_, t)) if t == "…"),
                "{room}: {last:?}"
            );
        }
    }

    /// A horizontal stroke is on the pixel row egui would put it on, at
    /// every scale — `≡`'s three bars crisp and evenly spaced.
    #[test]
    fn the_bars_sit_on_pixel_rows() {
        for ppp in [1.0, 1.5, 2.0] {
            for size in [12.5, 13.0, 20.0] {
                let format = TextFormat::simple(FontId::proportional(size), Color32::WHITE);
                let shapes = stock(ppp, |painter| {
                    Drawn::new(painter, StandIn::Bars, &format).paint(
                        painter,
                        egui::pos2(3.3, 7.7),
                        Color32::WHITE,
                    );
                });
                let rows: Vec<(f32, Stroke)> = shapes
                    .iter()
                    .filter_map(|shape| match shape {
                        egui::Shape::LineSegment { points, stroke } => Some((points[0].y, *stroke)),
                        _ => None,
                    })
                    .collect();
                assert_eq!(rows.len(), 3, "{shapes:?}");
                for (y, stroke) in &rows {
                    let mut snapped = *y;
                    stroke.round_center_to_pixel(ppp, &mut snapped);
                    assert_eq!(*y, snapped, "{size} at {ppp}x");
                }
                let gaps = (rows[1].0 - rows[0].0, rows[2].0 - rows[1].0);
                assert!((gaps.0 - gaps.1).abs() < 1e-4, "{gaps:?}");
            }
        }
    }

    /// The numbers [`Face`] carries, read back out of egui's own font files:
    /// a stand-in's axis, weight, height and cell are the face's.
    #[test]
    fn the_face_numbers_are_the_stock_faces_own() {
        let definitions = egui::FontDefinitions::default();
        for (family, face) in [
            (FontFamily::Proportional, UBUNTU_LIGHT),
            (FontFamily::Monospace, HACK),
        ] {
            let name = &definitions.families[&family][0];
            let data = &definitions.font_data[name];
            let parsed = ttf_parser::Face::parse(&data.font, data.index).expect("a stock face");
            let em = f32::from(parsed.units_per_em());
            let minus = parsed
                .glyph_index('−')
                .and_then(|glyph| parsed.glyph_bounding_box(glyph))
                .expect("a stock face has a minus");
            let near = |ours: f32, theirs: f32| (ours - theirs).abs() < 0.002;
            let what = format!("{name} ({family:?})");
            let axis = (f32::from(minus.y_min) + f32::from(minus.y_max)) / 2.0 / em;
            assert!(near(face.axis, axis), "{what}: axis {axis}");
            let stroke = (f32::from(minus.y_max) - f32::from(minus.y_min)) / em;
            assert!(near(face.stroke, stroke), "{what}: stroke {stroke}");
            let cap = f32::from(parsed.capital_height().unwrap_or_default()) / em;
            assert!(near(face.cap, cap), "{what}: cap {cap}");
            if let Some(cell) = face.cell {
                let x = parsed
                    .glyph_index('x')
                    .and_then(|glyph| parsed.glyph_hor_advance(glyph))
                    .map(|advance| f32::from(advance) / em);
                assert!(x.is_some_and(|x| near(cell, x)), "{what}: cell {x:?}");
            }
        }
    }
}
