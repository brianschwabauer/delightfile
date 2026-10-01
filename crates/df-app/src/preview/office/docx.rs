//! A Word document: `word/document.xml`, with `word/styles.xml` and
//! `word/numbering.xml` when the package has them.
//!
//! The body is a run of paragraphs (`w:p`) and tables (`w:tbl`). A paragraph
//! is runs (`w:r`) of text (`w:t`) under one set of paragraph properties
//! (`w:pPr`), and almost everything a reader sees as structure is in those
//! properties, by reference: the paragraph style (`w:pStyle`) says heading,
//! quote or code, and the numbering (`w:numPr`) says list item and how deep.
//!
//! **Styles are recognised by name.** A style's id is the writer's handle and
//! Word localises it — a German document's first-level heading is
//! `berschrift1` — but `styles.xml` gives every style a `w:name`, and the
//! built-in ones keep their English names (`heading 1`, `Title`, `Quote`)
//! whatever the language. So the id is looked up there first, and taken as
//! the name only when there is no `styles.xml` or no entry in it. Names are
//! compared lower-case with their spaces taken out, because the same style is
//! `Intense Quote` to one writer and `IntenseQuote` to another, and the table
//! of them takes in LibreOffice's and pandoc's names as well as Word's. A
//! style carries no formatting into the preview: a heading is a heading
//! because it is called one, not because it is large and bold.
//!
//! **A table's header is its first row, when the document says so.** Word
//! says so with a flag (`w:tblHeader`, "repeat as header row"), which most
//! documents never set. What both Word and LibreOffice do with a header row
//! that came from anywhere else — an HTML `<th>`, a pasted spreadsheet — is
//! make its text bold, so a first row whose every filled cell is bold
//! throughout is a header too. That bold is read the way Word works it out:
//! the run's own, else its character style's, else its paragraph style's,
//! each style deferring to the one it is based on. It decides the header and
//! nothing else; the bold drawn in a run is still only the run's own.
//!
//! **Numbering is a lookup and a counter.** A list item names a list
//! (`w:numId`) and a level (`w:ilvl`); `numbering.xml` maps the list to an
//! abstract definition, and the definition gives each level a format — a
//! bullet, decimal, letters, roman numerals. The numbers themselves are not in
//! the file: Word counts them as it lays the document out, so this counts
//! them too, one counter per list and level, a deeper level starting again
//! under each new item above it. A bullet is `•` at every level, as it is in
//! rendered markdown: the indent is what shows the depth.
//!
//! **Drawings are read through.** A picture in one is its `a:blip`, which
//! leaves the picture marker; a chart or a plain shape leaves nothing. A text
//! box in one (`w:txbxContent`) is paragraphs of the document's own — a cover
//! page's title, subtitle and author are all text boxes — so the paragraph the
//! drawing sits in is set aside, the box is read as blocks, and the paragraph
//! carries on when the box ends, written out after it. A VML picture and an
//! embedded object, the older forms, are a marker each with their insides
//! skipped, and only the first of a writer's two alternatives is read, so a
//! text box offered both ways is read once.
//!
//! **What is not the text is skipped.** Tracked deletions and moves away, a
//! field's code (`PAGE`, `HYPERLINK "…"`) as opposed to the result it shows,
//! the old formatting a tracked change records, and the fallback half of an
//! alternative a newer writer offers an older reader. Content controls, smart
//! tags, tracked insertions and simple fields are wrappers around ordinary
//! runs and are read straight through, which is what the scanner does with
//! any element it is not told about. Headers, footers, footnotes and comments
//! are parts of their own and are not read at all.
//!
//! **A page break is a rule.** An explicit one (`w:br w:type="page"`) ends the
//! paragraph there and draws a rule, so the pages an author made are still
//! visible as pages; what follows a break part-way through a paragraph
//! continues as the same kind of block. The mark Word leaves where its last
//! layout happened to turn the page (`w:lastRenderedPageBreak`) is not one:
//! it falls at every page boundary, mid-sentence as often as not, and says
//! where the pages fell for one printer's margins on one machine — layout,
//! which is the part a preview leaves out.

use std::collections::HashMap;
use std::io::{Read, Seek};

use super::super::markdown::{Block, Span, Style, MAX_DEPTH};
use super::{
    attr, is_blank, picture, push_cell_paragraph, push_text, scan, table, toggle, Event, Out,
    Package,
};

/// Word's list levels, `w:ilvl` 0 to 8.
const LEVELS: usize = 9;

/// Elements whose contents are not the document's text, skipped whole (see
/// the module essay): tracked deletions and moves away, deleted text, a
/// field's code, the formatting a tracked change replaced, and the second of
/// two alternatives — the first (`Choice`) is read.
const SKIPPED: &[&str] = &[
    "del",
    "moveFrom",
    "delText",
    "instrText",
    "rPrChange",
    "pPrChange",
    "Fallback",
];

pub(super) fn read<R: Read + Seek>(
    document: &str,
    package: &mut Package<R>,
    out: &mut Out,
) -> Result<(), String> {
    let styles = package
        .part("word/styles.xml")?
        .map(|xml| Styles::parse(&xml))
        .unwrap_or_default();
    let numbering = package
        .part("word/numbering.xml")?
        .map(|xml| Numbering::parse(&xml));
    let mut body = Body::new(&styles, numbering.as_ref());
    for event in scan(document) {
        body.event(event, out);
    }
    Ok(())
}

// ── Styles ──────────────────────────────────────────────────────────────────

/// What `styles.xml` says that a preview can use.
#[derive(Default)]
struct Styles {
    /// Style id → the style's name.
    names: HashMap<String, String>,
    /// Style id → the style it is based on (`w:basedOn`).
    based_on: HashMap<String, String>,
    /// Style id → its own bold (`w:b` in its `w:rPr`), for the styles that
    /// set one.
    bold: HashMap<String, bool>,
    /// Style id → the list (`numId`, level) a paragraph in that style is an
    /// item of, for the styles that carry their own numbering — Word's
    /// "List Bullet" and "List Number", and every document generated with
    /// them.
    lists: HashMap<String, (String, usize)>,
}

impl Styles {
    fn parse(xml: &str) -> Styles {
        let mut styles = Styles::default();
        let mut id: Option<String> = None;
        let mut list: (Option<String>, usize) = (None, 0);
        let mut in_run_properties = false;
        let mut skipping = 0usize;
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
                // A table style's formatting for its first row or its banded
                // columns, and the formatting a tracked change replaced, are
                // not the style's own.
                Event::Start {
                    name: "tblStylePr" | "rPrChange" | "pPrChange",
                    ..
                } => skipping = 1,
                Event::Start {
                    name: "basedOn",
                    attrs,
                } => {
                    if let (Some(id), Some(base)) = (&id, attr(attrs, "val")) {
                        styles.based_on.insert(id.clone(), base.into_owned());
                    }
                }
                Event::Start { name: "rPr", .. } if id.is_some() => in_run_properties = true,
                Event::End { name: "rPr" } => in_run_properties = false,
                Event::Start { name: "b", attrs } if in_run_properties => {
                    if let Some(id) = &id {
                        styles.bold.insert(id.clone(), toggle(attrs));
                    }
                }
                Event::Start {
                    name: "style",
                    attrs,
                } => {
                    id = attr(attrs, "styleId").map(|id| id.into_owned());
                    list = (None, 0);
                }
                Event::Start {
                    name: "name",
                    attrs,
                } => {
                    if let (Some(id), Some(name)) = (&id, attr(attrs, "val")) {
                        styles.names.insert(id.clone(), name.into_owned());
                    }
                }
                Event::Start {
                    name: "numId",
                    attrs,
                } if id.is_some() => list.0 = attr(attrs, "val").map(|v| v.into_owned()),
                Event::Start {
                    name: "ilvl",
                    attrs,
                } if id.is_some() => list.1 = level(attr(attrs, "val").as_deref()),
                Event::End { name: "style" } => {
                    if let (Some(id), (Some(num), level)) = (id.take(), list.clone()) {
                        styles.lists.insert(id, (num, level));
                    }
                }
                _ => {}
            }
        }
        styles
    }

    /// A style's name, or its id when `styles.xml` does not name it.
    fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.names.get(id).map_or(id, String::as_str)
    }

    /// Whether text in style `id` is bold by its style: the style's own `w:b`,
    /// or that of the nearest style it is based on that has one. `None` when
    /// no style in the chain says.
    fn bold(&self, id: &str) -> Option<bool> {
        let mut id = id;
        // Chains are a few styles long; a loop is a writer's bug, and sixteen
        // steps is where it stops being followed.
        for _ in 0..16 {
            if let Some(bold) = self.bold.get(id) {
                return Some(*bold);
            }
            id = self.based_on.get(id)?;
        }
        None
    }

    /// Whether a character style makes its runs code.
    fn is_code(&self, id: &str) -> bool {
        [self.name(id), id]
            .iter()
            .any(|label| label.contains("Code") || CODE_RUNS.contains(&squash(label).as_str()))
    }
}

/// Paragraph styles that make a quote, by squashed name ([`squash`]): Word's
/// `Quote` and `Intense Quote`, LibreOffice's `Block Quotation`, pandoc's
/// `Block Text`.
const QUOTES: &[&str] = &["quote", "intensequote", "blockquotation", "blocktext"];

/// Paragraph styles that make code, besides any with `Code` in its name:
/// LibreOffice's `Preformatted Text`, pandoc's `Source Code`, Word's `HTML
/// Preformatted`.
const CODE_BLOCKS: &[&str] = &["preformattedtext", "sourcecode", "htmlpreformatted"];

/// Character styles that make inline code, besides any with `Code` in its
/// name: pandoc's `Verbatim Char`, LibreOffice's `Source Text`, Word's `HTML
/// Code`.
const CODE_RUNS: &[&str] = &["verbatimchar", "sourcetext", "htmlcode"];

/// A style name as it is compared: lower-case, with its spaces taken out.
fn squash(label: &str) -> String {
    label
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// What a paragraph style makes of its paragraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Body,
    Heading(u8),
    Quote,
    Code,
}

/// The kind a style is, by its name and then by its id: `Title` is the first
/// heading, `heading N` (or `HeadingN`) the Nth, clamped to the six markdown
/// has; the [`QUOTES`] are quotes; a style with `Code` in its name and the
/// [`CODE_BLOCKS`] are code.
fn kind(id: &str, name: &str) -> Kind {
    for label in [name, id] {
        let squashed = squash(label);
        if squashed == "title" {
            return Kind::Heading(1);
        }
        if let Some(n) = squashed
            .strip_prefix("heading")
            .and_then(|n| n.parse::<u32>().ok())
        {
            return Kind::Heading(n.clamp(1, 6) as u8);
        }
        if QUOTES.contains(&squashed.as_str()) {
            return Kind::Quote;
        }
        if label.contains("Code") || CODE_BLOCKS.contains(&squashed.as_str()) {
            return Kind::Code;
        }
    }
    Kind::Body
}

/// A list level as written, clamped to Word's nine.
fn level(value: Option<&str>) -> usize {
    value
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(LEVELS - 1)
}

// ── Numbering ───────────────────────────────────────────────────────────────

/// What `numbering.xml` says each list's levels look like.
struct Numbering {
    /// `numId` → `abstractNumId`.
    lists: HashMap<String, String>,
    /// `abstractNumId` → level → its format.
    formats: HashMap<String, HashMap<usize, Format>>,
}

/// How one level of a list is numbered.
#[derive(Debug, Clone)]
struct Format {
    /// `w:numFmt`: `bullet`, `decimal`, `lowerLetter`, `upperRoman`…
    kind: String,
    /// `w:start`: what the first item at this level is numbered.
    start: u32,
}

impl Numbering {
    fn parse(xml: &str) -> Numbering {
        let mut lists = HashMap::new();
        let mut formats: HashMap<String, HashMap<usize, Format>> = HashMap::new();
        let mut abstract_id: Option<String> = None;
        let mut current: Option<(usize, Format)> = None;
        let mut list: Option<String> = None;
        for event in scan(xml) {
            match event {
                Event::Start {
                    name: "abstractNum",
                    attrs,
                } => abstract_id = attr(attrs, "abstractNumId").map(|v| v.into_owned()),
                // A level override inside a `w:num` is not a definition.
                Event::Start { name: "lvl", attrs } if abstract_id.is_some() => {
                    current = Some((
                        level(attr(attrs, "ilvl").as_deref()),
                        Format {
                            kind: "decimal".to_string(),
                            start: 1,
                        },
                    ));
                }
                Event::Start {
                    name: "numFmt",
                    attrs,
                } => {
                    if let (Some((_, format)), Some(kind)) = (&mut current, attr(attrs, "val")) {
                        format.kind = kind.into_owned();
                    }
                }
                Event::Start {
                    name: "start",
                    attrs,
                } => {
                    if let (Some((_, format)), Some(start)) = (
                        &mut current,
                        attr(attrs, "val").and_then(|v| v.trim().parse().ok()),
                    ) {
                        format.start = start;
                    }
                }
                Event::End { name: "lvl" } => {
                    if let (Some(id), Some((level, format))) = (&abstract_id, current.take()) {
                        formats.entry(id.clone()).or_default().insert(level, format);
                    }
                }
                Event::End {
                    name: "abstractNum",
                } => abstract_id = None,
                Event::Start { name: "num", attrs } => {
                    list = attr(attrs, "numId").map(|v| v.into_owned());
                }
                Event::Start {
                    name: "abstractNumId",
                    attrs,
                } => {
                    if let (Some(num), Some(id)) = (&list, attr(attrs, "val")) {
                        lists.insert(num.clone(), id.into_owned());
                    }
                }
                Event::End { name: "num" } => list = None,
                _ => {}
            }
        }
        Numbering { lists, formats }
    }

    fn format(&self, num: &str, level: usize) -> Option<&Format> {
        let id = self.lists.get(num)?;
        self.formats.get(id)?.get(&level)
    }
}

/// `n` as a level of the given format writes it, before its full stop.
/// Anything this does not know is decimal.
fn number(n: u32, kind: &str) -> String {
    match kind {
        "lowerLetter" => letters(n).map(|s| s.to_lowercase()),
        "upperLetter" => letters(n),
        "lowerRoman" => roman(n).map(|s| s.to_lowercase()),
        "upperRoman" => roman(n),
        _ => None,
    }
    .unwrap_or_else(|| n.to_string())
}

/// A, B, … Z, AA, BB, … — Word's letters, which repeat rather than carry.
/// Past four repeats it is a number again: a list that long is a list of
/// numbers in all but name.
fn letters(n: u32) -> Option<String> {
    let index = n.checked_sub(1)?;
    let letter = char::from(b'A' + (index % 26) as u8);
    let times = index / 26 + 1;
    (times <= 4).then(|| letter.to_string().repeat(times as usize))
}

/// Roman numerals, for 1 to 3999.
fn roman(mut n: u32) -> Option<String> {
    if n == 0 || n >= 4000 {
        return None;
    }
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, numeral) in NUMERALS {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    Some(out)
}

// ── The body ────────────────────────────────────────────────────────────────

/// The block a paragraph becomes.
#[derive(Debug, Clone)]
enum Shape {
    Heading(u8),
    Item { depth: usize, marker: String },
    Quote,
    Code,
    Body,
}

impl Shape {
    fn block(&self, spans: Vec<Span>, blank: bool) -> Block {
        match self {
            // An empty numbered paragraph is still a number on the page.
            Shape::Item { depth, marker } => Block::Item {
                depth: *depth,
                marker: marker.clone(),
                spans,
            },
            // …and an empty line of code is a line of the listing.
            Shape::Code => Block::Code {
                syntax: None,
                lines: spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
                    .split('\n')
                    .map(str::to_string)
                    .collect(),
            },
            _ if blank => Block::Gap,
            Shape::Heading(level) => Block::Heading {
                level: *level,
                spans,
            },
            Shape::Quote => Block::Quote(spans),
            Shape::Body => Block::Paragraph(spans),
        }
    }

    /// What the rest of a paragraph a page break split is: the same, except
    /// that a list item's marker was the first half's.
    fn continued(self) -> Shape {
        match self {
            Shape::Item { depth, .. } => Shape::Item {
                depth,
                marker: String::new(),
            },
            shape => shape,
        }
    }
}

/// One paragraph being read.
#[derive(Default)]
struct Paragraph {
    style: Option<String>,
    num: Option<String>,
    level: Option<usize>,
    spans: Vec<Span>,
    /// What the paragraph became once part of it was written out, by a page
    /// break part-way through it.
    shape: Option<Shape>,
    /// It has text a person would see, and some of that text is not bold —
    /// the two facts a table's header row is judged by.
    has_text: bool,
    has_plain_text: bool,
}

/// The table being read: only the outermost one has rows and cells, and a
/// table inside one of its cells is that cell's text.
#[derive(Default)]
struct Table {
    rows: Vec<Vec<Vec<Span>>>,
    row: Vec<Vec<Span>>,
    cell: Vec<Span>,
    /// The first row is the table's header.
    header: bool,
    /// The row being read repeats as a header on every page (`w:tblHeader`).
    row_is_header: bool,
    /// The row being read has text, and some of it is not bold.
    row_has_text: bool,
    row_has_plain_text: bool,
}

/// The walk over `document.xml`.
struct Body<'a> {
    styles: &'a Styles,
    numbering: Option<&'a Numbering>,
    /// Per list, the number each level last showed.
    counters: HashMap<String, [Option<u32>; LEVELS]>,
    /// How deep inside a skipped element the walk is; zero when reading.
    skipping: usize,
    paragraph: Option<Paragraph>,
    run: Style,
    /// The current run's own bold and its character style, for working out
    /// whether its text is bold when the run does not say (see the module
    /// essay on headers).
    run_bold: Option<bool>,
    run_style: Option<String>,
    in_run: bool,
    in_run_properties: bool,
    in_paragraph_properties: bool,
    in_numbering: bool,
    in_text: bool,
    in_row_properties: bool,
    /// Hyperlinks open around the current run.
    links: usize,
    /// Tables open around the current paragraph.
    tables: usize,
    table: Table,
    /// The paragraphs, innermost last, that a text box being read interrupted.
    boxes: Vec<Outer>,
}

/// What a text box interrupts: the paragraph its drawing sits in and the run
/// the drawing is part of, as they were when the box began.
struct Outer {
    paragraph: Option<Paragraph>,
    run: Style,
    run_bold: Option<bool>,
    run_style: Option<String>,
    in_run: bool,
    in_text: bool,
    links: usize,
}

impl<'a> Body<'a> {
    fn new(styles: &'a Styles, numbering: Option<&'a Numbering>) -> Body<'a> {
        Body {
            styles,
            numbering,
            counters: HashMap::new(),
            skipping: 0,
            paragraph: None,
            run: Style::default(),
            run_bold: None,
            run_style: None,
            in_run: false,
            in_run_properties: false,
            in_paragraph_properties: false,
            in_numbering: false,
            in_text: false,
            in_row_properties: false,
            links: 0,
            tables: 0,
            table: Table::default(),
            boxes: Vec::new(),
        }
    }

    fn event(&mut self, event: Event<'_>, out: &mut Out) {
        if self.skipping > 0 {
            match event {
                Event::Start { .. } => self.skipping += 1,
                Event::End { .. } => self.skipping -= 1,
                Event::Text(_) => {}
            }
            return;
        }
        match event {
            Event::Start { name, attrs } => self.start(name, attrs, out),
            Event::End { name } => self.end(name, out),
            Event::Text(text) => {
                if self.in_text {
                    self.text(&text);
                }
            }
        }
    }

    fn start(&mut self, name: &str, attrs: &str, out: &mut Out) {
        if SKIPPED.contains(&name) {
            self.skipping = 1;
            return;
        }
        match name {
            "p" => self.paragraph = Some(Paragraph::default()),
            "pPr" => self.in_paragraph_properties = self.paragraph.is_some(),
            "pStyle" if self.in_paragraph_properties => {
                if let Some(paragraph) = &mut self.paragraph {
                    paragraph.style = attr(attrs, "val").map(|v| v.into_owned());
                }
            }
            "numPr" if self.in_paragraph_properties => self.in_numbering = true,
            "numId" if self.in_numbering => {
                if let Some(paragraph) = &mut self.paragraph {
                    paragraph.num = attr(attrs, "val").map(|v| v.into_owned());
                }
            }
            "ilvl" if self.in_numbering => {
                if let Some(paragraph) = &mut self.paragraph {
                    paragraph.level = Some(level(attr(attrs, "val").as_deref()));
                }
            }
            "r" => {
                self.in_run = true;
                self.run = Style {
                    link: self.links > 0,
                    ..Style::default()
                };
                self.run_bold = None;
                self.run_style = None;
            }
            "rPr" if self.in_run => self.in_run_properties = true,
            "b" if self.in_run_properties => {
                self.run.bold = toggle(attrs);
                self.run_bold = Some(self.run.bold);
            }
            "i" if self.in_run_properties => self.run.italic = toggle(attrs),
            "rStyle" if self.in_run_properties => {
                self.run_style = attr(attrs, "val").map(|id| id.into_owned());
                self.run.code = self
                    .run_style
                    .as_deref()
                    .is_some_and(|id| self.styles.is_code(id));
            }
            "t" if self.in_run => self.in_text = true,
            "tab" if self.in_run => self.text("    "),
            "br" if self.in_run => {
                if attr(attrs, "type").as_deref() == Some("page") {
                    self.page_break(out);
                } else {
                    self.text("\n");
                }
            }
            "cr" if self.in_run => self.text("\n"),
            "noBreakHyphen" if self.in_run => self.text("-"),
            // A drawing is read through: its picture is its `a:blip`, and a
            // text box in it is paragraphs of the document's own.
            "blip" => self.picture(),
            // The older forms, VML pictures and embedded objects, are a
            // picture each, whatever text they hold.
            "pict" | "object" => {
                self.picture();
                self.skipping = 1;
            }
            "txbxContent" => self.open_box(),
            "hyperlink" => self.links += 1,
            "tbl" => {
                self.tables += 1;
                if self.tables == 1 {
                    self.table = Table::default();
                }
            }
            "tr" if self.tables == 1 => {
                self.table.row.clear();
                self.table.row_is_header = false;
                self.table.row_has_text = false;
                self.table.row_has_plain_text = false;
            }
            "trPr" if self.tables == 1 => self.in_row_properties = true,
            "tblHeader" if self.in_row_properties => self.table.row_is_header = toggle(attrs),
            "tc" if self.tables == 1 => self.table.cell.clear(),
            _ => {}
        }
    }

    fn end(&mut self, name: &str, out: &mut Out) {
        match name {
            "txbxContent" => self.close_box(),
            "p" => self.end_paragraph(out),
            "pPr" => self.in_paragraph_properties = false,
            "numPr" => self.in_numbering = false,
            "r" => {
                self.in_run = false;
                self.in_run_properties = false;
                self.in_text = false;
            }
            "rPr" => self.in_run_properties = false,
            "t" => self.in_text = false,
            "hyperlink" => self.links = self.links.saturating_sub(1),
            "trPr" => self.in_row_properties = false,
            "tc" if self.tables == 1 => {
                let cell = std::mem::take(&mut self.table.cell);
                self.table.row.push(cell);
            }
            "tr" if self.tables == 1 => {
                if self.table.rows.is_empty() {
                    // Marked as one, or bold wherever it says anything.
                    self.table.header = self.table.row_is_header
                        || (self.table.row_has_text && !self.table.row_has_plain_text);
                }
                let row = std::mem::take(&mut self.table.row);
                self.table.rows.push(row);
            }
            "tbl" => {
                if self.tables == 1 {
                    let finished = std::mem::take(&mut self.table);
                    if let Some(block) = table(finished.rows, finished.header) {
                        out.push(block);
                    }
                }
                self.tables = self.tables.saturating_sub(1);
            }
            _ => {}
        }
    }

    /// The picture marker, in the current paragraph.
    fn picture(&mut self) {
        let style = if self.in_run {
            self.run
        } else {
            Style::default()
        };
        if let Some(paragraph) = &mut self.paragraph {
            picture(&mut paragraph.spans, style);
        }
    }

    /// A text box begins. Its paragraphs are read as the document's own — a
    /// cover page's title, subtitle and author are all in text boxes — so
    /// the paragraph its drawing sits in is set aside until the box ends.
    fn open_box(&mut self) {
        self.boxes.push(Outer {
            paragraph: self.paragraph.take(),
            run: self.run,
            run_bold: self.run_bold.take(),
            run_style: self.run_style.take(),
            in_run: self.in_run,
            in_text: self.in_text,
            links: self.links,
        });
        self.run = Style::default();
        self.in_run = false;
        self.in_text = false;
        self.links = 0;
    }

    /// The text box ends, and the paragraph it interrupted carries on.
    fn close_box(&mut self) {
        let Some(outer) = self.boxes.pop() else {
            return;
        };
        self.paragraph = outer.paragraph;
        self.run = outer.run;
        self.run_bold = outer.run_bold;
        self.run_style = outer.run_style;
        self.in_run = outer.in_run;
        self.in_text = outer.in_text;
        self.links = outer.links;
    }

    /// Text in the current run.
    fn text(&mut self, text: &str) {
        let visible = !text.trim().is_empty();
        let bold = visible && self.bold();
        if let Some(paragraph) = &mut self.paragraph {
            push_text(&mut paragraph.spans, text, self.run);
            paragraph.has_text |= visible;
            paragraph.has_plain_text |= visible && !bold;
        }
    }

    /// Whether the current run's text is bold, as Word works it out: the
    /// run's own say, else its character style's, else its paragraph style's.
    fn bold(&self) -> bool {
        let styled = |id: Option<&str>| id.and_then(|id| self.styles.bold(id));
        self.run_bold
            .or_else(|| styled(self.run_style.as_deref()))
            .or_else(|| styled(self.paragraph.as_ref().and_then(|p| p.style.as_deref())))
            .unwrap_or(false)
    }

    fn end_paragraph(&mut self, out: &mut Out) {
        let Some(mut paragraph) = self.paragraph.take() else {
            return;
        };
        if self.tables > 0 {
            self.table.row_has_text |= paragraph.has_text;
            self.table.row_has_plain_text |= paragraph.has_plain_text;
            push_cell_paragraph(&mut self.table.cell, paragraph.spans);
            return;
        }
        self.write(&mut paragraph, out);
    }

    /// A page break: what the paragraph holds so far is written out, and a
    /// rule after it. Inside a table there is nowhere to put a rule, and the
    /// break is the table's business.
    fn page_break(&mut self, out: &mut Out) {
        if self.tables > 0 {
            return;
        }
        if let Some(mut paragraph) = self.paragraph.take() {
            if !is_blank(&paragraph.spans) {
                self.write(&mut paragraph, out);
            }
            self.paragraph = Some(paragraph);
        }
        out.push(Block::Rule);
    }

    /// Write out what `paragraph` holds as the block it is.
    fn write(&mut self, paragraph: &mut Paragraph, out: &mut Out) {
        let blank = is_blank(&paragraph.spans);
        let spans = std::mem::take(&mut paragraph.spans);
        let shape = match paragraph.shape.take() {
            // The rest of a split paragraph, with nothing in it.
            Some(shape) if blank => {
                paragraph.shape = Some(shape);
                return;
            }
            Some(shape) => shape,
            None => self.shape(paragraph),
        };
        out.push(shape.block(spans, blank));
        paragraph.shape = Some(shape.continued());
    }

    /// What a paragraph is, from its style and its numbering. A heading is a
    /// heading even when it is numbered — Word numbers headings through their
    /// style — and a paragraph whose numbering is list zero has had its
    /// style's numbering taken off.
    fn shape(&mut self, paragraph: &Paragraph) -> Shape {
        let style = paragraph.style.as_deref();
        let kind = style.map_or(Kind::Body, |id| kind(id, self.styles.name(id)));
        if let Kind::Heading(level) = kind {
            return Shape::Heading(level);
        }
        let listed = style.and_then(|id| self.styles.lists.get(id));
        let num = paragraph
            .num
            .clone()
            .or_else(|| listed.map(|(num, _)| num.clone()))
            .filter(|num| num != "0");
        if let Some(num) = num {
            let level = paragraph
                .level
                .or_else(|| listed.map(|(_, level)| *level))
                .unwrap_or(0);
            let marker = self.marker(&num, level);
            return Shape::Item {
                depth: level.min(MAX_DEPTH),
                marker,
            };
        }
        match kind {
            Kind::Quote => Shape::Quote,
            Kind::Code => Shape::Code,
            _ => Shape::Body,
        }
    }

    /// The marker the next item of list `num` at `level` shows, counting it.
    fn marker(&mut self, num: &str, level: usize) -> String {
        let level = level.min(LEVELS - 1);
        let counters = self
            .counters
            .entry(num.to_string())
            .or_insert([None; LEVELS]);
        // A deeper level starts again under each new item above it.
        for deeper in &mut counters[level + 1..] {
            *deeper = None;
        }
        let Some(format) = self.numbering.and_then(|n| n.format(num, level)) else {
            return "•".to_string();
        };
        if format.kind == "bullet" {
            return "•".to_string();
        }
        let n = counters[level].map_or(format.start, |n| n.saturating_add(1));
        counters[level] = Some(n);
        format!("{}.", number(n, &format.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{blocks, heading, item, paragraph, row, span, styled};
    use super::*;
    use crate::preview::markdown::Align;

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    /// A Word package whose body is `body`, with the styles and numbering
    /// parts when given.
    fn docx(body: &str, styles: Option<&str>, numbering: Option<&str>) -> Vec<Block> {
        let document = format!("<?xml version=\"1.0\"?><w:document {W}><w:body>{body}<w:sectPr/></w:body></w:document>");
        let styles = styles.map(|s| format!("<w:styles {W}>{s}</w:styles>"));
        let numbering = numbering.map(|n| format!("<w:numbering {W}>{n}</w:numbering>"));
        let mut parts = vec![
            ("[Content_Types].xml", "<Types/>".to_string()),
            ("word/document.xml", document),
        ];
        if let Some(styles) = styles {
            parts.push(("word/styles.xml", styles));
        }
        if let Some(numbering) = numbering {
            parts.push(("word/numbering.xml", numbering));
        }
        let parts: Vec<(&str, &str)> = parts.iter().map(|(n, d)| (*n, d.as_str())).collect();
        blocks(&parts)
    }

    /// `<w:p>` in `style`, of one plain run.
    fn p(style: &str, text: &str) -> String {
        let properties = if style.is_empty() {
            String::new()
        } else {
            format!(r#"<w:pPr><w:pStyle w:val="{style}"/></w:pPr>"#)
        };
        format!(r#"<w:p>{properties}<w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
    }

    /// `<w:p>`, an item of list `num` at `level`.
    fn li(num: &str, level: usize, text: &str) -> String {
        format!(
            r#"<w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{num}"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
        )
    }

    fn style(id: &str, name: &str) -> String {
        format!(
            r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="{name}"/><w:basedOn w:val="Normal"/></w:style>"#
        )
    }

    const BOLD: Style = Style {
        bold: true,
        italic: false,
        code: false,
        link: false,
    };
    const ITALIC: Style = Style {
        bold: false,
        italic: true,
        code: false,
        link: false,
    };

    #[test]
    fn headings_come_from_the_style_name_and_then_the_id() {
        let styles = [
            style("berschrift1", "heading 1"),
            style("Titel", "Title"),
            style("Quote", "Quote"),
        ]
        .concat();
        let body = [
            p("Titel", "The title"),
            p("berschrift1", "Localised"),
            // Not in styles.xml: the id is the name.
            p("Heading2", "By id"),
            p("Heading9", "Too deep"),
            p("Quote", "Said once"),
            p("Normal", "Plain"),
        ]
        .concat();
        assert_eq!(
            docx(&body, Some(&styles), None),
            vec![
                heading(1, "The title"),
                Block::Gap,
                heading(1, "Localised"),
                Block::Gap,
                heading(2, "By id"),
                Block::Gap,
                heading(6, "Too deep"),
                Block::Gap,
                Block::Quote(vec![span("Said once")]),
                Block::Gap,
                paragraph("Plain"),
            ]
        );
    }

    #[test]
    fn a_document_with_no_styles_or_numbering_still_reads() {
        let body = [p("Heading1", "Top"), li("4", 1, "an item"), p("", "text")].concat();
        assert_eq!(
            docx(&body, None, None),
            vec![
                heading(1, "Top"),
                Block::Gap,
                item(1, "•", "an item"),
                Block::Gap,
                paragraph("text"),
            ]
        );
    }

    #[test]
    fn runs_carry_bold_italic_links_and_code_and_merge_when_alike() {
        let styles = r#"<w:style w:type="character" w:styleId="HTMLCode"><w:name w:val="HTML Code"/></w:style>"#;
        let body = r#"<w:p>
            <w:r><w:t xml:space="preserve">Some </w:t></w:r>
            <w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r>
            <w:r><w:rPr><w:b w:val="1"/></w:rPr><w:t xml:space="preserve"> still</w:t></w:r>
            <w:r><w:rPr><w:b w:val="0"/><w:lang w:val="en-GB"/></w:rPr><w:t xml:space="preserve">, </w:t></w:r>
            <w:r><w:rPr><w:i/></w:rPr><w:t>italic</w:t></w:r>
            <w:r><w:t xml:space="preserve"> and </w:t></w:r>
            <w:hyperlink r:id="rId4"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>a link</w:t></w:r></w:hyperlink>
            <w:r><w:rPr><w:rStyle w:val="HTMLCode"/></w:rPr><w:t>x()</w:t></w:r>
            <w:r><w:tab/><w:t>tabbed</w:t><w:br/><w:t>broken</w:t></w:r>
            </w:p>"#;
        let link = Style {
            link: true,
            ..Style::default()
        };
        let code = Style {
            code: true,
            ..Style::default()
        };
        assert_eq!(
            docx(body, Some(styles), None),
            vec![Block::Paragraph(vec![
                span("Some "),
                styled("bold still", BOLD),
                span(", "),
                styled("italic", ITALIC),
                span(" and "),
                styled("a link", link),
                styled("x()", code),
                span("    tabbed\nbroken"),
            ])]
        );
    }

    /// Two lists: bullets (num 1) and a decimal / letter / roman outline
    /// (num 2), whose second level starts again under each new first-level
    /// item.
    const NUMBERING: &str = r#"
        <w:abstractNum w:abstractNumId="10">
          <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val=""/></w:lvl>
          <w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl>
          <w:lvl w:ilvl="2"><w:numFmt w:val="bullet"/></w:lvl>
          <w:lvl w:ilvl="7"><w:numFmt w:val="bullet"/></w:lvl>
        </w:abstractNum>
        <w:abstractNum w:abstractNumId="20">
          <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
          <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/></w:lvl>
          <w:lvl w:ilvl="2"><w:numFmt w:val="upperRoman"/></w:lvl>
        </w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="10"/></w:num>
        <w:num w:numId="2"><w:abstractNumId w:val="20"/><w:lvlOverride w:ilvl="0"><w:lvl w:ilvl="0"><w:numFmt w:val="upperLetter"/></w:lvl></w:lvlOverride></w:num>
    "#;

    /// One bullet at every depth, as in rendered markdown: the indent shows
    /// the nesting.
    #[test]
    fn a_bullet_is_the_same_bullet_at_every_depth() {
        let body = [
            li("1", 0, "a"),
            li("1", 1, "b"),
            li("1", 2, "c"),
            li("1", 7, "d"),
        ]
        .concat();
        assert_eq!(
            docx(&body, None, Some(NUMBERING)),
            vec![
                item(0, "•", "a"),
                item(1, "•", "b"),
                item(2, "•", "c"),
                item(MAX_DEPTH, "•", "d"),
            ]
        );
    }

    #[test]
    fn numbered_lists_count_and_a_deeper_level_starts_again() {
        let body = [
            li("2", 0, "One"),
            li("2", 1, "first"),
            li("2", 1, "second"),
            li("2", 2, "deep"),
            li("2", 2, "deeper"),
            li("2", 0, "Two"),
            li("2", 1, "again"),
            li("2", 2, "deep again"),
        ]
        .concat();
        assert_eq!(
            docx(&body, None, Some(NUMBERING)),
            vec![
                item(0, "1.", "One"),
                item(1, "a.", "first"),
                item(1, "b.", "second"),
                item(2, "I.", "deep"),
                item(2, "II.", "deeper"),
                item(0, "2.", "Two"),
                item(1, "a.", "again"),
                item(2, "I.", "deep again"),
            ]
        );
    }

    #[test]
    fn a_list_style_numbers_its_paragraphs_and_list_zero_takes_it_off() {
        let styles = r#"<w:style w:type="paragraph" w:styleId="ListNumber"><w:name w:val="List Number"/>
            <w:pPr><w:numPr><w:numId w:val="2"/></w:numPr></w:pPr></w:style>"#;
        let body = [
            p("ListNumber", "first"),
            p("ListNumber", "second"),
            r#"<w:p><w:pPr><w:pStyle w:val="ListNumber"/><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr><w:r><w:t>not an item</w:t></w:r></w:p>"#.to_string(),
        ]
        .concat();
        assert_eq!(
            docx(&body, Some(styles), Some(NUMBERING)),
            vec![
                item(0, "1.", "first"),
                item(0, "2.", "second"),
                Block::Gap,
                paragraph("not an item"),
            ]
        );
    }

    #[test]
    fn numbers_are_written_in_their_format() {
        assert_eq!(number(3, "decimal"), "3");
        assert_eq!(number(3, "ordinal"), "3");
        assert_eq!(number(1, "lowerLetter"), "a");
        assert_eq!(number(26, "upperLetter"), "Z");
        assert_eq!(number(27, "lowerLetter"), "aa");
        assert_eq!(number(200, "lowerLetter"), "200");
        assert_eq!(number(1994, "upperRoman"), "MCMXCIV");
        assert_eq!(number(4, "lowerRoman"), "iv");
        assert_eq!(number(0, "upperRoman"), "0");
    }

    #[test]
    fn a_table_is_its_rows_with_a_header_only_when_marked() {
        let cell =
            |text: &str| format!("<w:tc><w:tcPr/><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>");
        let marked = format!(
            r#"<w:tbl><w:tblPr/><w:tblGrid/>
                <w:tr><w:trPr><w:tblHeader/></w:trPr>{}{}</w:tr>
                <w:tr>{}{}</w:tr>
                <w:tr>{}</w:tr>
            </w:tbl>"#,
            cell("Name"),
            cell("Qty"),
            cell("Apples"),
            cell("3"),
            cell("Pears"),
        );
        let unmarked = format!(
            "<w:tbl><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
            cell("a"),
            cell("b"),
            cell("c"),
            cell("d")
        );
        let body = [marked, p("", "between"), unmarked].concat();
        assert_eq!(
            docx(&body, None, None),
            vec![
                Block::Table {
                    align: vec![Align::Left; 2],
                    header: row(&["Name", "Qty"]),
                    rows: vec![row(&["Apples", "3"]), row(&["Pears", ""])],
                },
                Block::Gap,
                paragraph("between"),
                Block::Gap,
                Block::Table {
                    align: vec![Align::Left; 2],
                    header: Vec::new(),
                    rows: vec![row(&["a", "b"]), row(&["c", "d"])],
                },
            ]
        );
    }

    #[test]
    fn a_cell_is_its_paragraphs_and_a_nested_table_flattens_into_it() {
        let body = r#"<w:tbl><w:tr>
            <w:tc><w:p><w:r><w:t>one</w:t></w:r></w:p><w:p/><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>two</w:t></w:r></w:p></w:tc>
            <w:tc><w:tbl><w:tr><w:tc><w:p><w:r><w:t>inner a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>inner b</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/></w:tc>
        </w:tr></w:tbl>"#;
        assert_eq!(
            docx(body, None, None),
            vec![Block::Table {
                align: vec![Align::Left; 2],
                header: Vec::new(),
                rows: vec![vec![
                    vec![span("one\n"), styled("two", BOLD)],
                    vec![span("inner a\ninner b")],
                ]],
            }]
        );
    }

    #[test]
    fn a_page_break_is_a_rule_and_splits_its_paragraph() {
        let body = [
            // A break at the very start divides nothing from nothing.
            r#"<w:p><w:r><w:br w:type="page"/></w:r></w:p>"#.to_string(),
            p("", "page one"),
            r#"<w:p><w:r><w:t>before</w:t><w:br w:type="page"/><w:t>after</w:t></w:r></w:p>"#
                .to_string(),
            // Two breaks in a row are one rule.
            r#"<w:p><w:r><w:br w:type="page"/></w:r></w:p>"#.to_string(),
            r#"<w:p><w:r><w:br w:type="page"/><w:t>page four</w:t></w:r></w:p>"#.to_string(),
        ]
        .concat();
        assert_eq!(
            docx(&body, None, None),
            vec![
                paragraph("page one"),
                Block::Gap,
                paragraph("before"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                paragraph("after"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                paragraph("page four"),
            ]
        );
    }

    /// Where Word's last layout turned the page is layout, not the document:
    /// the paragraph it fell in reads whole, with no rule.
    #[test]
    fn where_word_last_turned_the_page_is_not_a_break() {
        let body = [
            r#"<w:p><w:r><w:t xml:space="preserve">a sentence that ran </w:t><w:lastRenderedPageBreak/><w:t>onto the next page</w:t></w:r></w:p>"#.to_string(),
            r#"<w:p><w:r><w:lastRenderedPageBreak/><w:t>a page that began here</w:t></w:r></w:p>"#.to_string(),
        ]
        .concat();
        assert_eq!(
            docx(&body, None, None),
            vec![
                paragraph("a sentence that ran onto the next page"),
                Block::Gap,
                paragraph("a page that began here"),
            ]
        );
    }

    #[test]
    fn a_list_item_split_by_a_page_continues_without_a_second_marker() {
        let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr>
            <w:r><w:t>starts here</w:t><w:br w:type="page"/><w:t>ends here</w:t></w:r></w:p>"#;
        assert_eq!(
            docx(body, None, Some(NUMBERING)),
            vec![
                item(0, "1.", "starts here"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                item(0, "", "ends here"),
            ]
        );
    }

    #[test]
    fn tracked_deletions_and_field_codes_are_not_the_text() {
        let body = r#"<w:p>
            <w:r><w:t xml:space="preserve">kept </w:t></w:r>
            <w:del w:id="1" w:author="a"><w:r><w:delText>deleted </w:delText></w:r></w:del>
            <w:ins w:id="2" w:author="a"><w:r><w:t xml:space="preserve">inserted </w:t></w:r></w:ins>
            <w:moveFrom w:id="3"><w:r><w:t>moved away</w:t></w:r></w:moveFrom>
            <w:r><w:fldChar w:fldCharType="begin"/></w:r>
            <w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>
            <w:r><w:fldChar w:fldCharType="separate"/></w:r>
            <w:r><w:t>7</w:t></w:r>
            <w:r><w:fldChar w:fldCharType="end"/></w:r>
            <w:fldSimple w:instr=" NUMPAGES "><w:r><w:t>/9</w:t></w:r></w:fldSimple>
            <w:sdt><w:sdtPr><w:alias w:val="Field"/></w:sdtPr><w:sdtContent><w:r><w:t xml:space="preserve"> controlled</w:t></w:r></w:sdtContent></w:sdt>
            <w:r><w:rPr><w:b/><w:rPrChange w:id="4"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr><w:t xml:space="preserve"> bold</w:t></w:r>
            </w:p>"#;
        assert_eq!(
            docx(body, None, None),
            vec![Block::Paragraph(vec![
                span("kept inserted 7/9 controlled"),
                styled(" bold", BOLD),
            ])]
        );
    }

    /// A drawing's picture is its blip: one marker each, two side by side
    /// with a space between, the older VML form a marker too, and one marker
    /// for a picture offered both ways.
    #[test]
    fn a_picture_is_the_image_marker_once_and_two_are_two() {
        let blip = r#"<w:drawing><wp:inline><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="pic"><pic:pic><pic:nvPicPr/><pic:blipFill><a:blip r:embed="rId5"><a:extLst/></a:blip></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#;
        let body = format!(
            r#"<w:p>
            <w:r><w:t xml:space="preserve">See </w:t></w:r>
            <w:r>{blip}</w:r>
            <w:r><mc:AlternateContent><mc:Choice Requires="wp14">{blip}</mc:Choice><mc:Fallback><w:pict><v:shape><v:imagedata r:id="rId5"/></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r>
            <w:r><w:t xml:space="preserve"> and </w:t></w:r>
            <w:r><w:pict><v:shape><v:imagedata r:id="rId6"/></v:shape></w:pict></w:r>
            <w:r><w:drawing><wp:inline><a:graphic><a:graphicData uri="chart"><c:chart r:id="rId7"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>
            <w:r><w:t xml:space="preserve"> here.</w:t></w:r>
            </w:p>"#
        );
        let body = body.as_str();
        let marker = Style {
            code: true,
            ..Style::default()
        };
        assert_eq!(
            docx(body, None, None),
            vec![Block::Paragraph(vec![
                span("See "),
                styled("🖼 image 🖼 image", marker),
                span(" and "),
                styled("🖼 image", marker),
                span(" here."),
            ])]
        );
    }

    /// A cover page: the title, subtitle and author are text boxes in a
    /// drawing, offered as a DrawingML shape and again as VML. Their
    /// paragraphs are read once, as the document's own, and the paragraph the
    /// drawing sat in carries on after them.
    #[test]
    fn a_text_box_is_read_as_its_paragraphs() {
        let paragraphs = r#"<w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Annual Report</w:t></w:r></w:p>
            <w:p><w:r><w:rPr><w:i/></w:rPr><w:t>Prepared by Ada</w:t></w:r></w:p>"#;
        let body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Before </w:t></w:r>
            <w:r><mc:AlternateContent>
              <mc:Choice Requires="wps"><w:drawing><wp:anchor><wp:docPr id="2" name="Text Box 2"/><a:graphic><a:graphicData uri="wps"><wps:wsp><wps:spPr/><wps:txbx><w:txbxContent>{paragraphs}</w:txbxContent></wps:txbx><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice>
              <mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent>{paragraphs}</w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback>
            </mc:AlternateContent></w:r>
            <w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">after</w:t></w:r></w:p>"#
        );
        assert_eq!(
            docx(&body, None, None),
            vec![
                heading(1, "Annual Report"),
                Block::Gap,
                Block::Paragraph(vec![styled("Prepared by Ada", ITALIC)]),
                Block::Gap,
                Block::Paragraph(vec![span("Before "), styled("after", BOLD)]),
            ]
        );
    }

    #[test]
    fn empty_paragraphs_are_one_gap_and_none_at_either_end() {
        let body = [
            "<w:p/>".to_string(),
            r#"<w:p><w:r><w:t xml:space="preserve">   </w:t></w:r></w:p>"#.to_string(),
            p("", "one"),
            "<w:p/><w:p/><w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr></w:p>".to_string(),
            p("", "two"),
            "<w:p/>".to_string(),
        ]
        .concat();
        assert_eq!(
            docx(&body, None, None),
            vec![paragraph("one"), Block::Gap, paragraph("two")]
        );
    }

    #[test]
    fn code_paragraphs_are_one_block_and_keep_their_blank_lines() {
        let styles = style("SourceCode", "Source Code");
        let body = [
            p("SourceCode", "fn main() {"),
            "<w:p><w:pPr><w:pStyle w:val=\"SourceCode\"/></w:pPr></w:p>".to_string(),
            p("SourceCode", "    go();"),
            p("HTMLPreformatted", "}"),
            p("", "after"),
        ]
        .concat();
        assert_eq!(
            docx(&body, Some(&styles), None),
            vec![
                Block::Code {
                    syntax: None,
                    lines: vec![
                        "fn main() {".to_string(),
                        String::new(),
                        "    go();".to_string(),
                        "}".to_string(),
                    ],
                },
                Block::Gap,
                paragraph("after"),
            ]
        );
    }

    /// Word's, LibreOffice's and pandoc's names for a quote, a code block and
    /// inline code, by name or by id, however they are spaced or cased.
    #[test]
    fn quotes_and_code_are_known_by_every_writers_name_for_them() {
        let styles = [
            style("BlockQuotation", "Block Quotation"),
            style("BlockText", "Block Text"),
            style("a1", "Intense Quote"),
            style("PreformattedText", "Preformatted Text"),
            style("SourceCode", "Source Code"),
            style("a2", "HTML Preformatted"),
            r#"<w:style w:type="character" w:styleId="VerbatimChar"><w:name w:val="Verbatim Char"/></w:style>
               <w:style w:type="character" w:styleId="SourceText"><w:name w:val="Source Text"/></w:style>
               <w:style w:type="character" w:styleId="a3"><w:name w:val="HTML Code"/></w:style>"#
                .to_string(),
        ]
        .concat();
        let run = |style: &str, text: &str| {
            format!(r#"<w:r><w:rPr><w:rStyle w:val="{style}"/></w:rPr><w:t>{text}</w:t></w:r>"#)
        };
        let body = [
            p("BlockQuotation", "LibreOffice's"),
            p("BlockText", "pandoc's"),
            p("a1", "Word's"),
            // Not in styles.xml: the id is the name, and squashes the same.
            p("IntenseQuote", "by id"),
            p("", "between"),
            p("PreformattedText", "one"),
            p("SourceCode", "two"),
            p("a2", "three"),
            format!(
                "<w:p>{}{}{}</w:p>",
                run("VerbatimChar", "a"),
                run("SourceText", "b"),
                run("a3", "c")
            ),
        ]
        .concat();
        let code = Style {
            code: true,
            ..Style::default()
        };
        assert_eq!(
            docx(&body, Some(&styles), None),
            vec![
                Block::Quote(vec![span("LibreOffice's")]),
                Block::Quote(Vec::new()),
                Block::Quote(vec![span("pandoc's")]),
                Block::Quote(Vec::new()),
                Block::Quote(vec![span("Word's")]),
                Block::Quote(Vec::new()),
                Block::Quote(vec![span("by id")]),
                Block::Gap,
                paragraph("between"),
                Block::Gap,
                Block::Code {
                    syntax: None,
                    lines: vec!["one".to_string(), "two".to_string(), "three".to_string()],
                },
                Block::Gap,
                Block::Paragraph(vec![styled("abc", code)]),
            ]
        );
    }

    /// A table whose first row came from an HTML `<th>`: bold wherever it
    /// says anything — by the run, as Word writes it, or by the paragraph
    /// style, as LibreOffice's "Table Heading" does — is a header; a first row
    /// only partly bold is not.
    #[test]
    fn a_first_row_bold_throughout_is_a_header() {
        let styles = r#"
            <w:style w:type="paragraph" w:styleId="TableContents"><w:name w:val="Table Contents"/><w:rPr/></w:style>
            <w:style w:type="paragraph" w:styleId="TableHeading"><w:name w:val="Table Heading"/><w:basedOn w:val="TableContents"/><w:rPr><w:b/><w:bCs/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="HeadingRow"><w:name w:val="Heading Row"/><w:basedOn w:val="TableHeading"/></w:style>
            <w:style w:type="table" w:styleId="Grid"><w:name w:val="Grid"/><w:tblStylePr w:type="firstRow"><w:rPr><w:b/></w:rPr></w:tblStylePr></w:style>"#;
        let bold = |text: &str| {
            format!(r#"<w:tc><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>{text}</w:t></w:r></w:p></w:tc>"#)
        };
        let plain = |text: &str| format!("<w:tc><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>");
        let styled_cell = |style: &str, text: &str| {
            format!(
                r#"<w:tc><w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:rPr/><w:t>{text}</w:t></w:r></w:p></w:tc>"#
            )
        };
        let empty = "<w:tc><w:p/></w:tc>".to_string();
        let tables = [
            // Word's: bold runs, an empty corner cell.
            format!("<w:tbl><w:tr>{empty}{}{}</w:tr><w:tr>{}{}{}</w:tr></w:tbl>", bold("Qty"), bold("Price"), plain("Apples"), plain("3"), plain("1.5")),
            // LibreOffice's: a bold paragraph style, one level up its chain
            // and two.
            format!("<w:tbl><w:tblPr><w:tblStyle w:val=\"Grid\"/></w:tblPr><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>", styled_cell("TableHeading", "Name"), styled_cell("HeadingRow", "Qty"), styled_cell("TableContents", "Pears"), styled_cell("TableContents", "12")),
            // Partly bold: not a header.
            format!("<w:tbl><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>", bold("Total"), plain("15"), plain("a"), plain("b")),
            // A bold style with the run saying otherwise: not a header.
            format!(r#"<w:tbl><w:tr><w:tc><w:p><w:pPr><w:pStyle w:val="TableHeading"/></w:pPr><w:r><w:rPr><w:b w:val="0"/></w:rPr><w:t>x</w:t></w:r></w:p></w:tc></w:tr><w:tr>{}</w:tr></w:tbl>"#, plain("y")),
        ]
        .join(&p("", "-"));
        let headers: Vec<Vec<Vec<Span>>> = docx(&tables, Some(styles), None)
            .into_iter()
            .filter_map(|block| match block {
                Block::Table { header, .. } => Some(header),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec![
                vec![
                    Vec::new(),
                    vec![styled("Qty", BOLD)],
                    vec![styled("Price", BOLD)]
                ],
                row(&["Name", "Qty"]),
                Vec::new(),
                Vec::new(),
            ]
        );
    }

    #[test]
    fn style_kinds_are_read_from_names_whatever_their_case_or_spacing() {
        assert_eq!(kind("x", "heading 3"), Kind::Heading(3));
        assert_eq!(kind("Heading4", "Heading4"), Kind::Heading(4));
        assert_eq!(kind("x", "Intense Quote"), Kind::Quote);
        assert_eq!(kind("x", "HTML Preformatted"), Kind::Code);
        assert_eq!(kind("x", "Heading 1 Char"), Kind::Body);
        assert_eq!(kind("BodyText", "Body Text"), Kind::Body);
    }
}
