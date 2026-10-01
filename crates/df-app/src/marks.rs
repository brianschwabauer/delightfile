//! Row icons no loaded face can draw, drawn by the app instead
//! (`plans/other-platforms/04-windows.md` W4.42).
//!
//! ## The gap
//!
//! A row's icon is a Nerd Font picture ([`crate::icons`]), and a stock Mac or
//! Windows never has a Nerd Font. The icon column used to fall back to `ls
//! -F`'s classifiers — `/` before every folder, `@` before a link, `*` before
//! something that runs — which a Unix eye reads and everyone else reads as a
//! stray slash at the start of each folder's name. The Places card's disks,
//! phones and shares, and the archive header's mark, went blank.
//!
//! ## The answer: draw them
//!
//! As [`crate::glyphs`] draws the chrome's few missing symbols, these are
//! small drawings rather than a bundled face: a folder with a tab, as
//! Explorer and Finder draw one, filled with the folder's colour; a page with
//! its corner folded for a file, in its kind's colour; the page with an arrow
//! on it for a link, with `▶` for something that runs, with a zip for an
//! archive's header; and for the Places card a disk, a stick, a phone, a
//! camera, a server and a plus. The classifier picked which of the first
//! four a row is ([`Mark`]); the colours are the kinds' as they always were.
//!
//! Each is drawn to the metrics the Nerd Font's picture has, measured off
//! JetBrainsMono Nerd Font as egui draws it: about an em wide from where the
//! glyph starts, never more than [`HEIGHT`] tall, standing on the line's
//! baseline as a glyph would and centred on the same point an anchored line
//! of text is — so the name beside it sits where it sat, the icon column is
//! as wide, and a grid tile's picture is the size it was. Every edge is on a
//! pixel and every outline a whole number of pixels, so a 12-point icon is
//! crisp at 100 % and at 125 %; everything scales with the size asked for.
//!
//! ## Only where nothing else can
//!
//! [`crate::icons::paint`] draws the face's glyph whenever a Nerd Font was
//! installed — exactly the text it drew before this module existed, so a
//! Linux machine with one renders the icon column pixel for pixel as it did —
//! and comes here only when there is none.

use egui::emath::GuiRounding as _;
use egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, StrokeKind};

/// What a row or a place is drawn as when no icon face is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A folder: the tab, the back and the front, filled.
    Folder,
    /// A file of any kind, a socket and a pipe included: a page.
    File,
    /// A link: the page with an arrow on it.
    Link,
    /// Something that runs: the page with `▶` on it.
    Exec,
    /// An archive (the preview's header): the page with a zip down it.
    Archive,
    /// A fixed disk.
    Drive,
    /// A disk that comes out: a stick.
    Usb,
    Phone,
    Camera,
    /// A place on another machine.
    Server,
    /// "Connect to server…": a row that adds a place.
    Plus,
    /// An archive member that needs a password.
    Lock,
}

/// The box a mark is drawn in, in ems of the size asked for: as wide as the
/// Nerd Font's pictures are, and as tall. Anchored as a line of text is, the
/// box is centred on the anchor as their ink is, which stands its foot
/// [`HEIGHT`]` / 2` below the anchor's middle: on the baseline of the name
/// beside it in a row.
const WIDTH: f32 = 1.0;
const HEIGHT: f32 = 0.84;

/// Draw `mark` in `color`, `size` points tall as a glyph of that size would
/// be, anchored at `pos` as a line of text would be; the box it was drawn in.
pub fn paint(
    painter: &Painter,
    pos: Pos2,
    anchor: Align2,
    mark: Mark,
    size: f32,
    color: Color32,
) -> Rect {
    let ppp = painter.pixels_per_point();
    let box_size = egui::vec2(WIDTH * size, HEIGHT * size);
    let rect = anchor.anchor_size(pos, box_size).round_to_pixels(ppp);
    let pen = Pen::new(painter, rect, size, ppp, color);
    pen.draw(mark);
    rect
}

/// A mark's drawing, in fractions of its box.
struct Pen<'a> {
    painter: &'a Painter,
    rect: Rect,
    ppp: f32,
    color: Color32,
    /// The outline's width: a fourteenth of an em, never under a pixel,
    /// always whole pixels.
    line: f32,
}

impl<'a> Pen<'a> {
    fn new(painter: &'a Painter, rect: Rect, size: f32, ppp: f32, color: Color32) -> Pen<'a> {
        let line = ((size / 14.0) * ppp).round().max(1.0) / ppp;
        Pen {
            painter,
            rect,
            ppp,
            color,
            line,
        }
    }

    /// The point `fx`, `fy` of the way across and down the box, on a pixel
    /// edge.
    fn at(&self, fx: f32, fy: f32) -> Pos2 {
        egui::pos2(
            self.rect.left() + fx * self.rect.width(),
            self.rect.top() + fy * self.rect.height(),
        )
        .round_to_pixels(self.ppp)
    }

    /// The rectangle between two such points.
    fn span(&self, from: (f32, f32), to: (f32, f32)) -> Rect {
        Rect::from_min_max(self.at(from.0, from.1), self.at(to.0, to.1))
    }

    /// A corner's radius, in ems of the box's width, a pixel at least.
    fn radius(&self, f: f32) -> f32 {
        (f * self.rect.width()).max(1.0 / self.ppp)
    }

    fn stroke(&self) -> Stroke {
        Stroke::new(self.line, self.color)
    }

    /// The colour, a third of the way to black: the back of a folder.
    fn shade(&self) -> Color32 {
        let [r, g, b, a] = self.color.to_array();
        let k = |c: u8| (f32::from(c) * 0.68).round() as u8;
        Color32::from_rgba_premultiplied(k(r), k(g), k(b), a)
    }

    /// The colour as a faint wash: the inside of an outlined shape.
    fn wash(&self) -> Color32 {
        self.color.gamma_multiply(0.2)
    }

    fn fill(&self, points: Vec<Pos2>, color: Color32) {
        self.painter
            .add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    }

    /// An outline through `points`, closed, its stroke inside the shape's
    /// pixel edges so a one-pixel line is one pixel and not two half ones.
    fn outline(&self, points: &[Pos2], wash: bool) {
        let inset = inset(points, self.line / 2.0);
        let fill = if wash {
            self.wash()
        } else {
            Color32::TRANSPARENT
        };
        self.painter.add(egui::Shape::Path(egui::epaint::PathShape {
            points: inset,
            closed: true,
            fill,
            stroke: self.stroke().into(),
        }));
    }

    /// A line through `points`, its stroke centred on pixel centres.
    fn line(&self, points: &[Pos2]) {
        let half = self.line / 2.0;
        let centred: Vec<Pos2> = points
            .iter()
            .map(|p| egui::pos2(p.x + half, p.y + half))
            .collect();
        self.painter.add(egui::Shape::line(centred, self.stroke()));
    }

    fn draw(&self, mark: Mark) {
        match mark {
            Mark::Folder => self.folder(),
            Mark::File => {
                self.page();
            }
            Mark::Link => {
                let page = self.page();
                // An arrow up and out of the page's lower left, as a
                // shortcut's overlay has it.
                let from = self.at(page.0 + 0.10, 0.86);
                let to = self.at(page.0 + 0.38, 0.50);
                self.line(&[from, to]);
                let barb = self.line.max((to.x - from.x) * 0.55);
                self.line(&[
                    egui::pos2(to.x - barb, to.y),
                    to,
                    egui::pos2(to.x, to.y + barb),
                ]);
            }
            Mark::Exec => {
                let page = self.page();
                let mid = (page.0 + page.1) / 2.0;
                self.fill(
                    vec![
                        self.at(mid - 0.12, 0.42),
                        self.at(mid + 0.16, 0.62),
                        self.at(mid - 0.12, 0.82),
                    ],
                    self.color,
                );
            }
            Mark::Archive => {
                let page = self.page();
                let mid = (page.0 + page.1) / 2.0;
                // The zip: teeth either side of the middle, down from the top.
                for (i, fy) in [0.14f32, 0.30, 0.46, 0.62].iter().enumerate() {
                    let side = if i % 2 == 0 { -0.08 } else { 0.0 };
                    let tooth = self.span((mid + side, *fy), (mid + side + 0.08, fy + 0.10));
                    self.painter.rect_filled(tooth, 0.0, self.color);
                }
            }
            Mark::Drive => {
                let body = self.span((0.0, 0.30), (1.0, 0.92));
                self.painter.rect(
                    body,
                    self.radius(0.12),
                    self.wash(),
                    self.stroke(),
                    StrokeKind::Inside,
                );
                let led = self.at(0.78, 0.61);
                self.painter
                    .circle_filled(led, (self.line * 1.1).max(1.0 / self.ppp), self.color);
                self.line(&[self.at(0.16, 0.61), self.at(0.52, 0.61)]);
            }
            Mark::Usb => {
                let body = self.span((0.0, 0.26), (0.66, 0.90));
                self.painter
                    .rect_filled(body, self.radius(0.10), self.color);
                let plug = self.span((0.62, 0.38), (1.0, 0.78));
                self.painter
                    .rect_stroke(plug, 0.0, self.stroke(), StrokeKind::Inside);
            }
            Mark::Phone => {
                let body = self.span((0.24, 0.0), (0.76, 1.0));
                self.painter.rect(
                    body,
                    self.radius(0.12),
                    self.wash(),
                    self.stroke(),
                    StrokeKind::Inside,
                );
                self.line(&[self.at(0.42, 0.82), self.at(0.58, 0.82)]);
            }
            Mark::Camera => {
                let bump = self.span((0.30, 0.10), (0.62, 0.30));
                self.painter.rect_filled(bump, 0.0, self.color);
                let body = self.span((0.0, 0.26), (1.0, 0.96));
                self.painter.rect(
                    body,
                    self.radius(0.12),
                    self.wash(),
                    self.stroke(),
                    StrokeKind::Inside,
                );
                let lens = body.center();
                let radius = body.height() * 0.26;
                self.painter.circle_stroke(lens, radius, self.stroke());
            }
            Mark::Server => {
                for (top, bottom) in [(0.04, 0.48), (0.54, 0.98)] {
                    let unit = self.span((0.0, top), (1.0, bottom));
                    self.painter.rect(
                        unit,
                        self.radius(0.10),
                        self.wash(),
                        self.stroke(),
                        StrokeKind::Inside,
                    );
                    let dot = egui::pos2(unit.left() + unit.width() * 0.22, unit.center().y);
                    self.painter
                        .circle_filled(dot, self.line.max(1.0 / self.ppp), self.color);
                }
            }
            Mark::Plus => {
                let middle = self.at(0.5, 0.5);
                let reach = self.rect.height() * 0.40;
                self.line(&[
                    egui::pos2(middle.x - reach, middle.y),
                    egui::pos2(middle.x + reach, middle.y),
                ]);
                self.line(&[
                    egui::pos2(middle.x, middle.y - reach),
                    egui::pos2(middle.x, middle.y + reach),
                ]);
            }
            Mark::Lock => {
                let body = self.span((0.18, 0.44), (0.82, 1.0));
                self.painter
                    .rect_filled(body, self.radius(0.08), self.color);
                // The shackle: a hoop over the body, its feet in it.
                let centre = egui::pos2(body.center().x, body.top());
                let radius = body.width() * 0.30;
                let hoop: Vec<Pos2> = (0..=12)
                    .map(|i| {
                        let angle = std::f32::consts::PI * (1.0 + i as f32 / 12.0);
                        egui::pos2(
                            centre.x + radius * angle.cos(),
                            centre.y + radius * angle.sin(),
                        )
                    })
                    .collect();
                self.painter.add(egui::Shape::line(hoop, self.stroke()));
            }
        }
    }

    /// The folder: the tab and the back in the colour's shade, the front in
    /// the colour, nothing overlapping, so a folder fading out fades evenly.
    fn folder(&self) {
        let back = self.shade();
        self.fill(
            vec![
                self.at(0.0, 0.08),
                self.at(0.40, 0.08),
                self.at(0.50, 0.22),
                self.at(0.0, 0.22),
            ],
            back,
        );
        let strip = self.span((0.0, 0.22), (1.0, 0.36));
        self.painter.rect_filled(
            strip,
            egui::CornerRadius {
                ne: self.radius(0.08).round() as u8,
                ..Default::default()
            },
            back,
        );
        let front = self.span((0.0, 0.36), (1.0, 1.0));
        let r = self.radius(0.10).round() as u8;
        self.painter.rect_filled(
            front,
            egui::CornerRadius {
                sw: r,
                se: r,
                ..Default::default()
            },
            self.color,
        );
    }

    /// The page with its top right corner folded, outlined and washed in the
    /// colour; where across the box it runs, for what goes on it.
    fn page(&self) -> (f32, f32) {
        let (left, right) = (0.14, 0.82);
        let fold = 0.26;
        let points = [
            self.at(left, 0.0),
            self.at(right - fold, 0.0),
            self.at(right, fold / HEIGHT * WIDTH),
            self.at(right, 1.0),
            self.at(left, 1.0),
        ];
        self.outline(&points, true);
        // The dog-ear: the corner folded down, solid, inside the cut.
        let corner = self.at(right - fold, fold / HEIGHT * WIDTH);
        self.fill(vec![points[1], points[2], corner], self.color);
        (left, right)
    }
}

/// `points` moved `by` towards the middle of their box, so a stroke centred
/// on them stays inside the edges they were on.
fn inset(points: &[Pos2], by: f32) -> Vec<Pos2> {
    let bounds = Rect::from_points(points);
    let middle = bounds.center();
    points
        .iter()
        .map(|p| {
            let dx = if p.x < middle.x { by } else { -by };
            let dy = if p.y < middle.y { by } else { -by };
            egui::pos2(p.x + dx, p.y + dy)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Mark; 12] = [
        Mark::Folder,
        Mark::File,
        Mark::Link,
        Mark::Exec,
        Mark::Archive,
        Mark::Drive,
        Mark::Usb,
        Mark::Phone,
        Mark::Camera,
        Mark::Server,
        Mark::Plus,
        Mark::Lock,
    ];

    /// One frame at `ppp` with `mark` painted in it: its box and its shapes.
    fn painted(mark: Mark, size: f32, ppp: f32, anchor: Align2) -> (Rect, Vec<egui::Shape>) {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(ppp),
                ..Default::default()
            },
        );
        let mut rect = Rect::NOTHING;
        let shapes = ctx
            .run_ui(input, |ui| {
                rect = paint(
                    ui.painter(),
                    egui::pos2(100.3, 50.6),
                    anchor,
                    mark,
                    size,
                    Color32::from_rgb(0x89, 0xb4, 0xfa),
                );
            })
            .shapes
            .into_iter()
            .map(|clipped| clipped.shape)
            .filter(|shape| !matches!(shape, egui::Shape::Noop))
            .collect();
        (rect, shapes)
    }

    /// Every mark draws something, inside its box, at a list's size, a
    /// tile's, and every scale a screen has; and the box's edges are on
    /// pixels.
    #[test]
    fn every_mark_draws_inside_its_box_on_whole_pixels() {
        for mark in ALL {
            for (size, ppp) in [
                (12.5, 1.0),
                (12.5, 1.25),
                (12.5, 2.0),
                (44.0, 1.0),
                (14.0, 1.5),
            ] {
                let (rect, shapes) = painted(mark, size, ppp, Align2::LEFT_CENTER);
                assert!(!shapes.is_empty(), "{mark:?} drew nothing");
                for edge in [rect.left(), rect.right(), rect.top(), rect.bottom()] {
                    let px = edge * ppp;
                    assert!((px - px.round()).abs() < 1e-3, "{mark:?} at {ppp}: {edge}");
                }
                let slack = 1.0 / ppp + 0.01;
                for shape in &shapes {
                    let bounds = shape.visual_bounding_rect();
                    assert!(
                        rect.expand(slack).contains_rect(bounds),
                        "{mark:?} {size} at {ppp}: {bounds:?} outside {rect:?}"
                    );
                }
            }
        }
    }

    /// The box is the Nerd Font picture's: an em wide from the anchor at
    /// the left, about as tall, its foot on the baseline below the anchor's
    /// middle; anchored at the middle, it is centred across.
    #[test]
    fn the_box_is_where_the_glyph_would_be() {
        let (rect, _) = painted(Mark::Folder, 12.5, 1.0, Align2::LEFT_CENTER);
        assert!((rect.left() - 100.0).abs() <= 1.0, "{rect:?}");
        assert!((rect.width() - 12.5).abs() <= 1.0, "{rect:?}");
        assert!((rect.height() - 0.84 * 12.5).abs() <= 1.0, "{rect:?}");
        assert!(
            (rect.bottom() - (50.6 + HEIGHT / 2.0 * 12.5)).abs() <= 1.0,
            "{rect:?}"
        );
        let (rect, _) = painted(Mark::File, 44.0, 1.0, Align2::CENTER_CENTER);
        assert!((rect.center().x - 100.3).abs() <= 1.0, "{rect:?}");
        assert!((rect.width() - 44.0).abs() <= 1.0, "{rect:?}");
    }

    /// An outline is a whole number of pixels wide, a pixel at the least,
    /// and grows with the size.
    #[test]
    fn an_outline_is_whole_pixels() {
        let ctx = egui::Context::default();
        let painter = Painter::new(ctx, egui::LayerId::background(), Rect::EVERYTHING);
        for (size, ppp, want) in [
            (12.5, 1.0, 1.0),
            (12.5, 2.0, 1.0),
            (44.0, 1.0, 3.0),
            (6.0, 1.0, 1.0),
        ] {
            let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(size, size));
            let pen = Pen::new(&painter, rect, size, ppp, Color32::WHITE);
            assert!(
                (pen.line - want).abs() < 1e-4,
                "{size} at {ppp}: {}",
                pen.line
            );
            assert!((pen.line * ppp - (pen.line * ppp).round()).abs() < 1e-4);
        }
    }
}
