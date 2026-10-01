//! A PowerPoint presentation: `ppt/presentation.xml` for the order of the
//! slides, its relationships for which part each one is, and the slides.
//!
//! The order is the presentation's list (`p:sldIdLst`), never the part names:
//! `slide7.xml` is wherever the deck's author dragged it, and a slide that was
//! deleted and re-added keeps its number while moving to the end.
//!
//! A slide is shapes on a canvas, and the canvas is the part a preview cannot
//! keep. What it keeps is each shape's text, in the order the shapes are
//! listed — which is their stacking order, and for nearly every deck the order
//! they were added in, title first. The title placeholder becomes the slide's
//! heading wherever it sits in that list, so every slide opens with one; a
//! slide without a title is called by its number. A body or content
//! placeholder is a bulleted list, because that is what PowerPoint draws it
//! as unless a paragraph says otherwise (`a:buNone`); a free text box is
//! paragraphs, unless a paragraph asks for a bullet (`a:buChar`,
//! `a:buAutoNum`). Tables are tables, and a picture, a chart or a diagram is
//! the marker a picture leaves in rendered markdown.
//!
//! Not read: speaker notes, which are not the slide; the date, footer and
//! slide-number placeholders, which are the same on every slide and would
//! interleave each one with "‹#›"; and the layouts and masters, whose text is
//! "Click to add title" rather than anything the author wrote.

use std::io::{Read, Seek};

use super::super::markdown::{Block, Span, Style, MAX_DEPTH};
use super::{
    attr, is_blank, picture, plain, push_cell_paragraph, push_text, rel_id, relationships, scan,
    table, Event, Out, Package,
};

/// The most slides read: 200. A long talk is sixty; past two hundred the deck
/// is an archive of slides, and the preview has long since made its point.
pub const MAX_SLIDES: usize = 200;

pub(super) fn read<R: Read + Seek>(
    presentation: &str,
    package: &mut Package<R>,
    out: &mut Out,
) -> Result<(), String> {
    let rels = package
        .part("ppt/_rels/presentation.xml.rels")?
        .map(|xml| relationships(&xml, "ppt/"))
        .unwrap_or_default();
    let slides: Vec<Option<String>> = scan(presentation)
        .filter_map(|event| match event {
            Event::Start {
                name: "sldId",
                attrs,
            } => Some(rel_id(attrs).and_then(|id| rels.get(id.as_ref()).cloned())),
            _ => None,
        })
        .collect();
    for (index, part) in slides.iter().enumerate() {
        if index >= MAX_SLIDES || out.full() {
            out.truncate();
            break;
        }
        // The deck ends at the slide the package's budget has no room for.
        if part.as_deref().is_some_and(|part| !package.fits(part)) {
            package.cut_short();
            break;
        }
        // A slide the list names and the package does not have is an empty
        // slide, not an unreadable deck.
        let slide = match part {
            Some(part) => package
                .part(part)
                .map(|xml| xml.map(|xml| Slide::read(&xml)).unwrap_or_default()),
            None => Ok(Slide::default()),
        };
        if index > 0 {
            out.push(Block::Rule);
        }
        let number = || plain(format!("Slide {}", index + 1));
        match slide {
            Ok(slide) => {
                out.push(Block::Heading {
                    level: 2,
                    spans: slide.title.unwrap_or_else(number),
                });
                for block in slide.blocks {
                    out.push(block);
                }
            }
            Err(message) => {
                out.push(Block::Heading {
                    level: 2,
                    spans: number(),
                });
                out.push(Block::Paragraph(plain(message)));
            }
        }
    }
    Ok(())
}

/// One slide's text.
#[derive(Default)]
struct Slide {
    /// The title placeholder's text, when it has any.
    title: Option<Vec<Span>>,
    blocks: Vec<Block>,
}

/// One paragraph of a shape: its bullet, if it says, its level, its text.
#[derive(Default)]
struct Paragraph {
    /// `Some(true)` for `a:buChar` / `a:buAutoNum`, `Some(false)` for
    /// `a:buNone`, `None` when the paragraph leaves it to the shape.
    bullet: Option<bool>,
    depth: usize,
    spans: Vec<Span>,
}

/// A shape being read: its placeholder type, if it is one, and its
/// paragraphs.
#[derive(Default)]
struct Shape {
    placeholder: Option<String>,
    paragraphs: Vec<Paragraph>,
}

/// The table in a graphic frame being read.
#[derive(Default)]
struct Grid {
    rows: Vec<Vec<Vec<Span>>>,
    row: Vec<Vec<Span>>,
    cell: Vec<Span>,
}

/// A run's `b="1"` and `i="1"`.
fn flag(attrs: &str, name: &str) -> bool {
    matches!(attr(attrs, name).as_deref(), Some("1" | "true"))
}

impl Slide {
    fn read(xml: &str) -> Slide {
        let mut slide = Slide::default();
        let mut skipping = 0usize;
        let mut shape: Option<Shape> = None;
        let mut paragraph: Option<Paragraph> = None;
        let mut run = Style::default();
        let (mut in_run, mut in_text, mut in_properties) = (false, false, false);
        // A graphic frame is open, and whether it turned out to hold a table.
        let mut frame: Option<bool> = None;
        let mut grid: Option<Grid> = None;

        for event in scan(xml) {
            if skipping > 0 {
                match event {
                    Event::Start { .. } => skipping += 1,
                    Event::End { .. } => skipping -= 1,
                    Event::Text(_) => {}
                }
                continue;
            }
            match event {
                Event::Start { name, attrs } => match name {
                    // The first of two alternatives is read (see `docx`).
                    "Fallback" => skipping = 1,
                    "sp" => shape = Some(Shape::default()),
                    "ph" => {
                        if let Some(shape) = &mut shape {
                            // A placeholder that names no type is a content
                            // placeholder — the body of "Title and Content".
                            shape.placeholder = Some(
                                attr(attrs, "type").map_or("obj".to_string(), |t| t.into_owned()),
                            );
                        }
                    }
                    "p" => paragraph = Some(Paragraph::default()),
                    "pPr" if paragraph.is_some() => {
                        in_properties = true;
                        if let Some(paragraph) = &mut paragraph {
                            paragraph.depth = attr(attrs, "lvl")
                                .and_then(|lvl| lvl.trim().parse::<usize>().ok())
                                .unwrap_or(0)
                                .min(MAX_DEPTH);
                        }
                    }
                    "buChar" | "buAutoNum" if in_properties => {
                        if let Some(paragraph) = &mut paragraph {
                            paragraph.bullet = Some(true);
                        }
                    }
                    "buNone" if in_properties => {
                        if let Some(paragraph) = &mut paragraph {
                            paragraph.bullet = Some(false);
                        }
                    }
                    // A field (`a:fld`) — a date, a slide number — is a run
                    // whose text was filled in when the deck was saved.
                    "r" | "fld" if paragraph.is_some() => {
                        in_run = true;
                        run = Style::default();
                    }
                    "rPr" if in_run => {
                        run.bold = flag(attrs, "b");
                        run.italic = flag(attrs, "i");
                    }
                    "t" if in_run => in_text = true,
                    "br" => {
                        if let Some(paragraph) = &mut paragraph {
                            push_text(&mut paragraph.spans, "\n", Style::default());
                        }
                    }
                    "graphicFrame" => frame = Some(false),
                    "tbl" => {
                        grid = Some(Grid::default());
                        frame = frame.map(|_| true);
                    }
                    "tr" => {
                        if let Some(grid) = &mut grid {
                            grid.row.clear();
                        }
                    }
                    "tc" => {
                        if let Some(grid) = &mut grid {
                            grid.cell.clear();
                        }
                    }
                    "pic" => {
                        // Inside a frame, a picture is that frame's stand-in
                        // for an older reader, and the frame speaks for it.
                        if frame.is_none() {
                            slide.blocks.push(picture_block());
                        }
                        skipping = 1;
                    }
                    _ => {}
                },
                Event::End { name } => match name {
                    "p" => {
                        let Some(finished) = paragraph.take() else {
                            continue;
                        };
                        if let Some(grid) = &mut grid {
                            push_cell_paragraph(&mut grid.cell, finished.spans);
                        } else if let Some(shape) = &mut shape {
                            shape.paragraphs.push(finished);
                        }
                    }
                    "pPr" => in_properties = false,
                    "r" | "fld" => {
                        in_run = false;
                        in_text = false;
                    }
                    "t" => in_text = false,
                    "tc" => {
                        if let Some(grid) = &mut grid {
                            let cell = std::mem::take(&mut grid.cell);
                            grid.row.push(cell);
                        }
                    }
                    "tr" => {
                        if let Some(grid) = &mut grid {
                            let row = std::mem::take(&mut grid.row);
                            grid.rows.push(row);
                        }
                    }
                    "tbl" => {
                        // A slide's table has no header flag of its own; its
                        // first row is drawn as one by every table style
                        // PowerPoint ships, so it is read as one.
                        if let Some(block) = grid.take().and_then(|grid| table(grid.rows, true)) {
                            slide.blocks.push(block);
                        }
                    }
                    "graphicFrame" => {
                        // A chart, a diagram, an embedded object: a picture
                        // to anybody looking at the slide.
                        if frame.take() == Some(false) {
                            slide.blocks.push(picture_block());
                        }
                    }
                    "sp" => {
                        if let Some(shape) = shape.take() {
                            slide.shape(shape);
                        }
                    }
                    _ => {}
                },
                Event::Text(text) => {
                    if in_text {
                        if let Some(paragraph) = &mut paragraph {
                            push_text(&mut paragraph.spans, &text, run);
                        }
                    }
                }
            }
        }
        slide
    }

    /// A finished shape's text, onto the slide.
    fn shape(&mut self, shape: Shape) {
        let placeholder = shape.placeholder.as_deref();
        match placeholder {
            Some("title" | "ctrTitle") if self.title.is_none() => {
                // Its paragraphs are one heading, a line break in it being
                // where the title wrapped on the slide rather than a second
                // line of meaning.
                let mut title = Vec::new();
                for paragraph in shape.paragraphs {
                    if is_blank(&paragraph.spans) {
                        continue;
                    }
                    if !title.is_empty() {
                        push_text(&mut title, " ", Style::default());
                    }
                    for span in paragraph.spans {
                        push_text(&mut title, &span.text.replace('\n', " "), span.style);
                    }
                }
                if !title.is_empty() {
                    self.title = Some(title);
                }
            }
            Some("dt" | "ftr" | "sldNum" | "hdr") => {}
            _ => {
                let bulleted = matches!(placeholder, Some("body" | "subTitle" | "obj"));
                for paragraph in shape.paragraphs {
                    if is_blank(&paragraph.spans) {
                        continue;
                    }
                    self.blocks.push(if paragraph.bullet.unwrap_or(bulleted) {
                        Block::Item {
                            depth: paragraph.depth,
                            marker: "•".to_string(),
                            spans: paragraph.spans,
                        }
                    } else {
                        Block::Paragraph(paragraph.spans)
                    });
                }
            }
        }
    }
}

/// A paragraph that is only the picture marker.
fn picture_block() -> Block {
    let mut spans = Vec::new();
    picture(&mut spans, Style::default());
    Block::Paragraph(spans)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{blocks, heading, item, paragraph, read_parts, row, span, styled};
    use super::*;
    use crate::preview::markdown::Align;

    /// A presentation whose slides are `slides`, listed in the order of
    /// `order` (indices into `slides`), each slide part named by its index.
    fn deck(slides: &[String], order: &[usize]) -> Vec<(String, String)> {
        let ids: String = order
            .iter()
            .enumerate()
            .map(|(n, slide)| format!(r#"<p:sldId id="{}" r:id="rId{}"/>"#, 256 + n, slide + 10))
            .collect();
        let rels: String = (0..slides.len())
            .map(|slide| {
                format!(
                    r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{}.xml"/>"#,
                    slide + 10,
                    slide + 1
                )
            })
            .collect();
        let mut parts = vec![
            ("[Content_Types].xml".to_string(), "<Types/>".to_string()),
            (
                "ppt/presentation.xml".to_string(),
                format!(
                    r#"<p:presentation><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#
                ),
            ),
            (
                "ppt/_rels/presentation.xml.rels".to_string(),
                format!(
                    r#"<Relationships><Relationship Id="rId1" Type="m" Target="slideMasters/slideMaster1.xml"/>{rels}</Relationships>"#
                ),
            ),
        ];
        for (n, slide) in slides.iter().enumerate() {
            parts.push((format!("ppt/slides/slide{}.xml", n + 1), slide.clone()));
        }
        parts
    }

    fn read(parts: &[(String, String)]) -> Vec<Block> {
        let parts: Vec<(&str, &str)> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_str()))
            .collect();
        blocks(&parts)
    }

    fn slide(shapes: &str) -> String {
        format!(
            r#"<p:sld><p:cSld><p:spTree><p:nvGrpSpPr/><p:grpSpPr/>{shapes}</p:spTree></p:cSld></p:sld>"#
        )
    }

    /// A shape with placeholder `ph` (`""` for none, `"-"` for a placeholder
    /// with no type) holding `paragraphs`.
    fn sp(ph: &str, paragraphs: &str) -> String {
        let ph = match ph {
            "" => String::new(),
            "-" => r#"<p:ph idx="1"/>"#.to_string(),
            ph => format!(r#"<p:ph type="{ph}"/>"#),
        };
        format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="x"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paragraphs}</p:txBody></p:sp>"#
        )
    }

    fn ap(properties: &str, text: &str) -> String {
        format!(
            r#"<a:p>{properties}<a:r><a:rPr lang="en-US" dirty="0"/><a:t>{text}</a:t></a:r></a:p>"#
        )
    }

    #[test]
    fn slides_follow_the_list_not_the_part_names() {
        let first = slide(&sp("title", &ap("", "Opening")));
        let second = slide(&sp("ctrTitle", &ap("", "Closing")));
        // slide1.xml is listed second.
        let blocks = read(&deck(&[second, first], &[1, 0]));
        assert_eq!(
            blocks,
            vec![
                heading(2, "Opening"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                heading(2, "Closing"),
            ]
        );
    }

    #[test]
    fn a_slide_without_a_title_is_called_by_its_number() {
        let titled = slide(&sp("title", &ap("", "Agenda")));
        let untitled = slide(&sp("", &ap("", "Just a text box")));
        let blank_title = slide(&sp("title", "<a:p><a:endParaRPr/></a:p>"));
        assert_eq!(
            read(&deck(&[titled, untitled, blank_title], &[0, 1, 2])),
            vec![
                heading(2, "Agenda"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                heading(2, "Slide 2"),
                Block::Gap,
                paragraph("Just a text box"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                heading(2, "Slide 3"),
            ]
        );
    }

    #[test]
    fn a_body_is_bullets_by_level_and_a_text_box_is_paragraphs() {
        let shapes = [
            // The body comes before the title in the shape list; the title is
            // still the heading.
            sp(
                "-",
                &[
                    ap("", "Point"),
                    ap(r#"<a:pPr lvl="1"/>"#, "Sub-point"),
                    ap(r#"<a:pPr lvl="2"><a:buNone/></a:pPr>"#, "Not a bullet"),
                    "<a:p/>".to_string(),
                    ap(r#"<a:pPr lvl="9"/>"#, "Very deep"),
                ]
                .concat(),
            ),
            sp("title", &[ap("", "Two"), ap("", "lines")].concat()),
            sp(
                "",
                &[
                    ap("", "A caption"),
                    ap(
                        r#"<a:pPr><a:buFont typeface="Arial"/><a:buChar char="•"/></a:pPr>"#,
                        "Asked for",
                    ),
                ]
                .concat(),
            ),
            sp("subTitle", &ap("", "Subtitle")),
            sp(
                "sldNum",
                r#"<a:p><a:fld id="{1}" type="slidenum"><a:t>1</a:t></a:fld></a:p>"#,
            ),
            sp("ftr", &ap("", "Company confidential")),
        ]
        .concat();
        assert_eq!(
            read(&deck(&[slide(&shapes)], &[0])),
            vec![
                heading(2, "Two lines"),
                Block::Gap,
                item(0, "•", "Point"),
                item(1, "•", "Sub-point"),
                Block::Gap,
                paragraph("Not a bullet"),
                Block::Gap,
                item(MAX_DEPTH, "•", "Very deep"),
                Block::Gap,
                paragraph("A caption"),
                Block::Gap,
                item(0, "•", "Asked for"),
                item(0, "•", "Subtitle"),
            ]
        );
    }

    #[test]
    fn runs_carry_bold_and_italic_and_breaks_and_fields() {
        let paragraph = r#"<a:p>
            <a:r><a:rPr b="1"/><a:t>Bold</a:t></a:r>
            <a:r><a:rPr i="1" b="0"/><a:t xml:space="preserve"> then italic</a:t></a:r>
            <a:br><a:rPr/></a:br>
            <a:r><a:t>after a break, on </a:t></a:r>
            <a:fld id="{x}" type="datetime1"><a:rPr/><a:t>10/1/2026</a:t></a:fld>
            </a:p>"#;
        let blocks = read(&deck(&[slide(&sp("", paragraph))], &[0]));
        assert_eq!(
            blocks[2],
            Block::Paragraph(vec![
                styled(
                    "Bold",
                    Style {
                        bold: true,
                        ..Style::default()
                    }
                ),
                styled(
                    " then italic",
                    Style {
                        italic: true,
                        ..Style::default()
                    }
                ),
                span("\nafter a break, on 10/1/2026"),
            ])
        );
    }

    #[test]
    fn a_table_frame_is_a_table_and_a_chart_frame_or_picture_is_the_marker() {
        let tc = |text: &str| {
            format!(
                r#"<a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>{text}</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc>"#
            )
        };
        let shapes = format!(
            r#"<p:graphicFrame><p:nvGraphicFramePr/><p:xfrm/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="1"/><a:tblGrid/>
                 <a:tr h="1">{}{}</a:tr><a:tr h="1">{}{}</a:tr><a:tr h="1">{}</a:tr>
               </a:tbl></a:graphicData></a:graphic></p:graphicFrame>
               <p:graphicFrame><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame>
               <p:pic><p:nvPicPr><p:cNvPr id="4" name="Picture 3" descr="a cat"/></p:nvPicPr><p:blipFill/></p:pic>"#,
            tc("Region"),
            tc("Sales"),
            tc("North"),
            tc("12"),
            tc("South"),
        );
        let marker = Block::Paragraph(vec![styled(
            "🖼 image",
            Style {
                code: true,
                ..Style::default()
            },
        )]);
        assert_eq!(
            read(&deck(&[slide(&shapes)], &[0])),
            vec![
                heading(2, "Slide 1"),
                Block::Gap,
                Block::Table {
                    align: vec![Align::Left; 2],
                    header: row(&["Region", "Sales"]),
                    rows: vec![row(&["North", "12"]), row(&["South", ""])],
                },
                Block::Gap,
                marker.clone(),
                Block::Gap,
                marker,
            ]
        );
    }

    #[test]
    fn a_deck_past_the_slide_cap_stops_and_says_so() {
        let slides: Vec<String> = (0..=MAX_SLIDES)
            .map(|n| slide(&sp("title", &ap("", &format!("S{n}")))))
            .collect();
        let order: Vec<usize> = (0..slides.len()).collect();
        let parts = deck(&slides, &order);
        let parts: Vec<(&str, &str)> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_str()))
            .collect();
        let reading = read_parts(&parts).unwrap_or_else(|e| panic!("{e}"));
        assert!(reading.truncated);
        let headings = reading
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Heading { .. }))
            .count();
        assert_eq!(headings, MAX_SLIDES);
    }

    #[test]
    fn a_slide_the_list_names_and_the_package_lacks_is_an_empty_slide() {
        let mut parts = deck(&[slide(&sp("title", &ap("", "Here")))], &[0]);
        // A second listed slide whose relationship points nowhere.
        parts[1].1 = parts[1].1.replace(
            "</p:sldIdLst>",
            r#"<p:sldId id="999" r:id="rId404"/></p:sldIdLst>"#,
        );
        assert_eq!(
            read(&parts),
            vec![
                heading(2, "Here"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                heading(2, "Slide 2")
            ]
        );
    }
}
