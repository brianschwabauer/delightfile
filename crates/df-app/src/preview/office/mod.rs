//! Word, PowerPoint and Excel files, read as the text they carry.
//!
//! PLAN §6 deferred office documents to the opener, and for the formats that
//! are a page layout engine's input that is still the answer. But a `.docx`,
//! a `.pptx` and an `.xlsx` are not opaque: each is a zip of XML parts, and
//! what a person glancing at one wants — what it says, how it is organised —
//! is in a few of those parts as headings, list items, runs of bold and
//! italic, tables, slides and sheets. That is the vocabulary the markdown
//! reader already has ([`super::markdown::Block`]), and its painter already
//! draws it in this pane. So each reader here turns its parts into those
//! blocks, the pane gets [`super::Body::Markdown`] like any README, and a
//! Word file scrolls, remembers its place and reads exactly like one.
//!
//! What is left out is everything that is layout rather than content: pages,
//! fonts, sizes, colours, positions on a slide, column widths, number formats.
//! A picture is the marker the markdown reader leaves for an image, never the
//! picture — a second decode pipeline for a file that is being glanced at is
//! the same bargain [`super::markdown`] declines for a README's images.
//!
//! ## Which of the three
//!
//! By the part that is there, not by the name or the type df-core sniffed:
//! `word/document.xml` is a Word document, `ppt/presentation.xml` a
//! presentation, `xl/workbook.xml` a workbook. df-core sent the file here on
//! its name alone — the bytes of all three say only "zip" — so the name is a
//! claim this checks rather than a fact it relies on: a `.pptx` that is a
//! Word file reads as the Word file it is, and a plain zip renamed `.docx` is
//! said to be not one of the three, in the one line the pane shows for a file
//! it could not read.
//!
//! ## A scanner, not a parser
//!
//! The XML is read the way [`super::doc::model`] reads a 3MF's: one pass over
//! the text yielding start tags, end tags and text, and small state machines
//! in each reader that care about a dozen element names. No tree is built and
//! no namespace is resolved. Elements are matched on their *local* names — the
//! part after the colon — because the prefixes are a writer's choice (`w:` and
//! `a:` by convention, nothing by rule) and the local names are what the
//! specification fixes. The three formats this reads are machine-written,
//! well-formed and free of DTDs, so what a general parser would add is
//! validation nobody here would act on and a dependency the rule forbids.
//!
//! ## Caps
//!
//! The file was chosen by a cursor and written by anyone. Every part is held
//! to [`MAX_PART_BYTES`] before it is inflated, and the document as a whole to
//! [`MAX_BLOCKS`]; the slide and sheet readers have caps of their own. Hitting
//! any of them sets [`Reading::truncated`], which the pane draws as a document
//! that stops rather than one that ends.

use std::borrow::Cow;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use df_core::archive::zip;

use super::markdown::{Align, Block, Span, Style};

mod docx;
mod pptx;
mod xlsx;

/// The most one part may inflate to: 32 MiB.
///
/// A part is XML, and XML is mostly markup: a 300-page novel's
/// `word/document.xml` is a few megabytes, a thousand-slide deck's slides are
/// each a few dozen kilobytes, and a sheet that reaches this is hundreds of
/// thousands of cells — far past what [`xlsx`] would keep of it anyway. The cap
/// is held against the size the zip's index declares, before a byte is
/// allocated, because that number is the file's to choose.
pub const MAX_PART_BYTES: u64 = 32 * 1024 * 1024;

/// The most blocks a document becomes: 5 000.
///
/// A block is a paragraph, a heading, a list item or a whole table, so this is
/// a long report's worth — far more than a glance reads — and it keeps the
/// painter's walk over a document to the cost of a long README rather than of
/// a manuscript.
pub const MAX_BLOCKS: usize = 5_000;

/// A document, read.
pub struct Reading {
    pub blocks: Vec<Block>,
    /// A cap stopped the reading before the document ended.
    pub truncated: bool,
}

/// Read the Office file at `path`.
///
/// The error is the one line the pane shows instead, already in its words.
pub fn read(path: &Path) -> Result<Reading, String> {
    let file = File::open(path).map_err(quiet)?;
    let len = file.metadata().map_err(quiet)?.len();
    read_package(Package { reader: file, len })
}

/// [`read`], from any zip: the tests hand it one built in memory.
fn read_package<R: Read + Seek>(mut package: Package<R>) -> Result<Reading, String> {
    let mut out = Out::default();
    if let Some(document) = package.part("word/document.xml")? {
        docx::read(&document, &mut package, &mut out)?;
    } else if let Some(presentation) = package.part("ppt/presentation.xml")? {
        pptx::read(&presentation, &mut package, &mut out)?;
    } else if let Some(workbook) = package.part("xl/workbook.xml")? {
        xlsx::read(&workbook, &mut package, &mut out)?;
    } else {
        return Err("not a Word, PowerPoint or Excel file".to_string());
    }
    Ok(out.finish())
}

/// The zip an Office file is, asked for its parts by name.
struct Package<R> {
    reader: R,
    len: u64,
}

impl<R: Read + Seek> Package<R> {
    /// One part as text, or `None` when the package has no such part.
    fn part(&mut self, name: &str) -> Result<Option<String>, String> {
        let bytes =
            zip::read_member(&mut self.reader, self.len, name, MAX_PART_BYTES).map_err(quiet)?;
        Ok(bytes.map(|bytes| text(&bytes)))
    }
}

/// A part's bytes as text.
///
/// The specification allows UTF-8 and UTF-16 and every writer uses UTF-8; the
/// byte-order mark is what says which, and a stray invalid byte is replaced
/// rather than refused, as df-core's text reader does.
fn text(bytes: &[u8]) -> String {
    let utf16 = |unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = (2..bytes.len().saturating_sub(1))
            .step_by(2)
            .map(|at| unit([bytes[at], bytes[at + 1]]))
            .collect();
        String::from_utf16_lossy(&units)
    };
    if bytes.starts_with(&[0xff, 0xfe]) {
        return utf16(u16::from_le_bytes);
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return utf16(u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// A failure as the line the pane shows for it: lower-case, as everything the
/// pane says about a file is ("empty", "not a font file"), and without the
/// "(os error 2)" nobody can act on.
fn quiet(error: impl std::fmt::Display) -> String {
    let mut text = error.to_string();
    if let Some(at) = text.rfind(" (os error ") {
        if text.ends_with(')') {
            text.truncate(at);
        }
    }
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        // "Permission denied" → "permission denied"; "IO error" stays.
        (Some(first), Some(second)) if first.is_uppercase() && !second.is_uppercase() => first
            .to_lowercase()
            .chain(text[first.len_utf8()..].chars())
            .collect(),
        _ => text,
    }
}

// ── The document as it is read ──────────────────────────────────────────────

/// The blocks a reader has produced, spaced and capped as they arrive.
///
/// Spacing is the markdown reader's: a README separates its paragraphs,
/// headings, tables and rules with a blank line, which [`Block::Gap`] carries,
/// and runs its list items together. A document here gets the same — a gap
/// between any two blocks, except between two list items; two quoted
/// paragraphs are joined by an empty quoted line, as `> a`, `>`, `> b` is
/// written; and two code paragraphs in a row are one code block. Readers push
/// what they found and never a gap of their own, except for an empty
/// paragraph, whose gap is the author's.
#[derive(Default)]
struct Out {
    blocks: Vec<Block>,
    truncated: bool,
}

impl Out {
    fn push(&mut self, block: Block) {
        match block {
            Block::Gap => {
                if !matches!(self.blocks.last(), None | Some(Block::Gap)) && !self.full() {
                    self.blocks.push(Block::Gap);
                }
                return;
            }
            // A rule with nothing above it, or with only another rule above
            // it, divides nothing: a page break straight after a page break,
            // or at the very start.
            Block::Rule if matches!(self.last_content(), None | Some(Block::Rule)) => return,
            _ => {}
        }
        if let (Some(Block::Code { lines, .. }), Block::Code { lines: more, .. }) =
            (self.blocks.last_mut(), &block)
        {
            lines.extend(more.iter().cloned());
            return;
        }
        let between = match (self.blocks.last(), &block) {
            (None | Some(Block::Gap), _) => None,
            (Some(Block::Item { .. }), Block::Item { .. }) => None,
            (Some(Block::Quote(_)), Block::Quote(_)) => Some(Block::Quote(Vec::new())),
            _ => Some(Block::Gap),
        };
        if self.blocks.len() + usize::from(between.is_some()) >= MAX_BLOCKS {
            self.truncated = true;
            return;
        }
        self.blocks.extend(between);
        self.blocks.push(block);
    }

    /// The last block that is not a gap.
    fn last_content(&self) -> Option<&Block> {
        self.blocks.iter().rev().find(|b| !matches!(b, Block::Gap))
    }

    /// Whether another block would go past [`MAX_BLOCKS`]. A reader with more
    /// to read stops on it, and says so with [`Out::truncate`].
    fn full(&self) -> bool {
        self.blocks.len() >= MAX_BLOCKS
    }

    /// There was more, and it was left out.
    fn truncate(&mut self) {
        self.truncated = true;
    }

    fn finish(mut self) -> Reading {
        // A trailing gap is an empty last paragraph, not a space anybody can
        // see the end of.
        while matches!(self.blocks.last(), Some(Block::Gap)) {
            self.blocks.pop();
        }
        Reading {
            blocks: self.blocks,
            truncated: self.truncated,
        }
    }
}

/// Add `text` to a paragraph's spans, joining the last span when it is drawn
/// the same way — a Word paragraph is often a dozen runs that differ only in
/// properties a preview does not draw.
fn push_text(spans: &mut Vec<Span>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => spans.push(Span {
            text: text.to_string(),
            style,
        }),
    }
}

/// The span a picture leaves in the text: the marker [`super::markdown`]'s
/// `inline` draws for an image, with "image" for the alt text a document's
/// pictures seldom carry. Two pictures side by side are two markers with a
/// space between, not one word.
fn picture(spans: &mut Vec<Span>, style: Style) {
    const MARKER: &str = "🖼 image";
    let style = Style {
        code: true,
        ..style
    };
    if spans.last().is_some_and(|last| last.text.ends_with(MARKER)) {
        push_text(spans, " ", style);
    }
    push_text(spans, MARKER, style);
}

/// Whether a paragraph's spans have anything a person would see.
fn is_blank(spans: &[Span]) -> bool {
    spans.iter().all(|span| span.text.trim().is_empty())
}

/// One plain span, for the text the readers make up: a slide's number, a
/// sheet's name, a cell.
fn plain(text: impl Into<String>) -> Vec<Span> {
    let text = text.into();
    if text.is_empty() {
        return Vec::new();
    }
    vec![Span {
        text,
        style: Style::default(),
    }]
}

/// A table from rows of cells as the document had them: as wide as its widest
/// row, every row padded to that, and the first row as the header when the
/// document marks it as one. `None` for a table with no cells at all.
fn table(mut rows: Vec<Vec<Vec<Span>>>, header: bool) -> Option<Block> {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return None;
    }
    for row in &mut rows {
        row.resize(width, Vec::new());
    }
    let header = if header { rows.remove(0) } else { Vec::new() };
    Some(Block::Table {
        align: vec![Align::Left; width],
        header,
        rows,
    })
}

/// Add a cell paragraph's spans to the cell: a cell is its paragraphs, one to
/// a line, and an empty one is not a line.
fn push_cell_paragraph(cell: &mut Vec<Span>, spans: Vec<Span>) {
    if is_blank(&spans) {
        return;
    }
    if !cell.is_empty() {
        push_text(cell, "\n", Style::default());
    }
    for span in spans {
        push_text(cell, &span.text, span.style);
    }
}

// ── Relationships ───────────────────────────────────────────────────────────

/// A `_rels/*.rels` part: relationship id → the part it points at, resolved
/// against `base` (the folder of the part the relationships belong to, with
/// its trailing slash: `ppt/`). External targets — a hyperlink's URL — are not
/// parts and are left out.
fn relationships(xml: &str, base: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for event in scan(xml) {
        let Event::Start {
            name: "Relationship",
            attrs,
        } = event
        else {
            continue;
        };
        if attr(attrs, "TargetMode").as_deref() == Some("External") {
            continue;
        }
        if let (Some(id), Some(target)) = (attr(attrs, "Id"), attr(attrs, "Target")) {
            map.insert(id.into_owned(), resolve(base, &target));
        }
    }
    map
}

/// A relationship's target as a part name: relative to `base` unless it starts
/// with `/`, with `..` and `.` folded away.
fn resolve(base: &str, target: &str) -> String {
    let joined = match target.strip_prefix('/') {
        Some(absolute) => absolute.to_string(),
        None => format!("{base}{target}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// The relationship id on an element that has one (`r:id`): the attribute
/// whose local name is `id` *and* has a prefix, because a slide's `<p:sldId>`
/// also carries a plain numeric `id` that is not one.
fn rel_id(attrs: &str) -> Option<Cow<'_, str>> {
    attributes(attrs)
        .find(|(qname, _)| qname.contains(':') && !is_xmlns(qname) && local(qname) == "id")
        .map(|(_, value)| value)
}

// ── The scanner ─────────────────────────────────────────────────────────────

/// One thing in a part, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event<'a> {
    /// `<w:p …>`, by its local name (`p`); `attrs` is everything after the
    /// name, for [`attr`]. A self-closing `<w:b/>` is a start and then an end.
    Start {
        name: &'a str,
        attrs: &'a str,
    },
    End {
        name: &'a str,
    },
    /// The text between two tags, entities decoded. Whitespace between tags
    /// comes through too; the readers only take text inside the elements
    /// that hold it.
    Text(Cow<'a, str>),
}

/// The events of `xml`. Comments, processing instructions, the XML
/// declaration and `<!DOCTYPE>` are skipped; CDATA is text.
fn scan(xml: &str) -> Scanner<'_> {
    Scanner {
        rest: xml,
        closing: None,
    }
}

struct Scanner<'a> {
    rest: &'a str,
    /// The end a self-closing tag owes, returned on the next call.
    closing: Option<&'a str>,
}

impl<'a> Iterator for Scanner<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        if let Some(name) = self.closing.take() {
            return Some(Event::End { name });
        }
        loop {
            if self.rest.is_empty() {
                return None;
            }
            let Some(rest) = self.rest.strip_prefix('<') else {
                let end = self.rest.find('<').unwrap_or(self.rest.len());
                let (text, rest) = self.rest.split_at(end);
                self.rest = rest;
                return Some(Event::Text(unescape(text)));
            };
            if let Some(after) = rest.strip_prefix("!--") {
                self.rest = after.find("-->").map_or("", |at| &after[at + 3..]);
                continue;
            }
            if let Some(after) = rest.strip_prefix("![CDATA[") {
                let (text, rest) = after.split_once("]]>").unwrap_or((after, ""));
                self.rest = rest;
                if text.is_empty() {
                    continue;
                }
                return Some(Event::Text(Cow::Borrowed(text)));
            }
            if let Some(after) = rest.strip_prefix('?') {
                self.rest = after.find("?>").map_or("", |at| &after[at + 2..]);
                continue;
            }
            if let Some(after) = rest.strip_prefix('!') {
                self.rest = after.find('>').map_or("", |at| &after[at + 1..]);
                continue;
            }
            // A tag that never closes is the end of a truncated part: what
            // came before it stands.
            let Some(end) = tag_end(rest) else {
                self.rest = "";
                return None;
            };
            let body = &rest[..end];
            self.rest = &rest[end + 1..];
            if let Some(name) = body.strip_prefix('/') {
                return Some(Event::End {
                    name: local(name.trim_end()),
                });
            }
            let (body, empty) = match body.strip_suffix('/') {
                Some(body) => (body, true),
                None => (body, false),
            };
            let split = body
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(body.len());
            let name = local(&body[..split]);
            if name.is_empty() {
                continue;
            }
            if empty {
                self.closing = Some(name);
            }
            return Some(Event::Start {
                name,
                attrs: &body[split..],
            });
        }
    }
}

/// Where the tag `rest` starts with ends: the first `>` that is not inside a
/// quoted attribute value, where XML allows one.
fn tag_end(rest: &str) -> Option<usize> {
    let mut quote = None;
    for (at, byte) in rest.bytes().enumerate() {
        match (quote, byte) {
            (None, b'"' | b'\'') => quote = Some(byte),
            (Some(open), _) if byte == open => quote = None,
            (None, b'>') => return Some(at),
            _ => {}
        }
    }
    None
}

/// A name without its prefix: `w:p` → `p`.
fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

fn is_xmlns(qname: &str) -> bool {
    qname == "xmlns" || qname.starts_with("xmlns:")
}

/// Every `name="value"` (or `'value'`) in a tag's attribute text, in order,
/// with the qualified name as written and the value decoded.
fn attributes(attrs: &str) -> impl Iterator<Item = (&str, Cow<'_, str>)> {
    let mut rest = attrs;
    std::iter::from_fn(move || {
        let trimmed = rest.trim_start();
        let eq = trimmed.find('=')?;
        let qname = trimmed[..eq].trim();
        let after = trimmed[eq + 1..].trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let close = after[1..].find(quote)?;
        rest = &after[close + 2..];
        Some((qname, unescape(&after[1..1 + close])))
    })
}

/// The attribute whose local name is `name` — `w:val`, `val` and `x:val` alike
/// — never a namespace declaration.
fn attr<'a>(attrs: &'a str, name: &str) -> Option<Cow<'a, str>> {
    attributes(attrs)
        .find(|(qname, _)| !is_xmlns(qname) && local(qname) == name)
        .map(|(_, value)| value)
}

/// A WordprocessingML on/off property (`<w:b/>`, `<w:b w:val="0"/>`): on
/// unless its value says off.
fn toggle(attrs: &str) -> bool {
    !matches!(attr(attrs, "val").as_deref(), Some("0" | "false" | "off"))
}

/// `text` with the five predefined entities and numeric character references
/// decoded. An `&` that starts none of them is kept as written.
fn unescape(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        // The longest reference is `&#x10FFFF;`, ten bytes.
        let decoded = after[..after.len().min(12)]
            .find(';')
            .and_then(|end| entity(&after[1..end]).map(|c| (c, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code).unwrap_or('\u{fffd}')
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A stored zip of `parts`, in order, in memory: enough of the format for
    /// `zip::read_member` to find each by name. The 3MF tests in
    /// `doc::model` build theirs the same way.
    pub fn zip_of(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut directory: Vec<u8> = Vec::new();
        for (name, data) in parts {
            let at = out.len() as u32;
            let data = data.as_bytes();
            let fields: [&[u8]; 11] = [
                b"PK\x03\x04",
                &20u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0x0021u16.to_le_bytes(),
                &0u32.to_le_bytes(),
                &(data.len() as u32).to_le_bytes(),
                &(data.len() as u32).to_le_bytes(),
                &(name.len() as u16).to_le_bytes(),
                &0u16.to_le_bytes(),
            ];
            for field in fields {
                out.extend_from_slice(field);
            }
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            let fields: [&[u8]; 17] = [
                b"PK\x01\x02",
                &20u16.to_le_bytes(),
                &20u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0x0021u16.to_le_bytes(),
                &0u32.to_le_bytes(),
                &(data.len() as u32).to_le_bytes(),
                &(data.len() as u32).to_le_bytes(),
                &(name.len() as u16).to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u32.to_le_bytes(),
                &at.to_le_bytes(),
            ];
            for field in fields {
                directory.extend_from_slice(field);
            }
            directory.extend_from_slice(name.as_bytes());
        }
        let directory_at = out.len() as u32;
        out.extend_from_slice(&directory);
        let fields: [&[u8]; 8] = [
            b"PK\x05\x06",
            &0u16.to_le_bytes(),
            &0u16.to_le_bytes(),
            &(parts.len() as u16).to_le_bytes(),
            &(parts.len() as u16).to_le_bytes(),
            &(directory.len() as u32).to_le_bytes(),
            &directory_at.to_le_bytes(),
            &0u16.to_le_bytes(),
        ];
        for field in fields {
            out.extend_from_slice(field);
        }
        out
    }

    /// Read a package made of `parts`.
    pub fn read_parts(parts: &[(&str, &str)]) -> Result<Reading, String> {
        let bytes = zip_of(parts);
        let len = bytes.len() as u64;
        read_package(Package {
            reader: Cursor::new(bytes),
            len,
        })
    }

    /// The blocks of a package that must read.
    pub fn blocks(parts: &[(&str, &str)]) -> Vec<Block> {
        match read_parts(parts) {
            Ok(reading) => reading.blocks,
            Err(e) => panic!("the package did not read: {e}"),
        }
    }

    /// A span drawn plainly.
    pub fn span(text: &str) -> Span {
        Span {
            text: text.to_string(),
            style: Style::default(),
        }
    }

    pub fn styled(text: &str, style: Style) -> Span {
        Span {
            text: text.to_string(),
            style,
        }
    }

    pub fn heading(level: u8, text: &str) -> Block {
        Block::Heading {
            level,
            spans: vec![span(text)],
        }
    }

    pub fn paragraph(text: &str) -> Block {
        Block::Paragraph(vec![span(text)])
    }

    pub fn item(depth: usize, marker: &str, text: &str) -> Block {
        Block::Item {
            depth,
            marker: marker.to_string(),
            spans: vec![span(text)],
        }
    }

    /// A row of plain cells; `""` is an empty cell.
    pub fn row(cells: &[&str]) -> Vec<Vec<Span>> {
        cells.iter().map(|cell| plain(*cell)).collect()
    }

    // ── the scanner ─────────────────────────────────────────────────────────

    fn events(xml: &str) -> Vec<Event<'_>> {
        scan(xml).collect()
    }

    fn start<'a>(name: &'a str, attrs: &'a str) -> Event<'a> {
        Event::Start { name, attrs }
    }

    fn end(name: &str) -> Event<'_> {
        Event::End { name }
    }

    fn text_event(text: &str) -> Event<'_> {
        Event::Text(Cow::Borrowed(text))
    }

    #[test]
    fn the_scanner_yields_starts_ends_and_text_by_local_name() {
        assert_eq!(
            events("<w:p><w:r><w:t>hi</w:t></w:r></w:p>"),
            vec![
                start("p", ""),
                start("r", ""),
                start("t", ""),
                text_event("hi"),
                end("t"),
                end("r"),
                end("p"),
            ]
        );
        // The prefix is the writer's choice; the local name is the format's.
        assert_eq!(
            events("<x:t>a</x:t><t>b</t>"),
            vec![
                start("t", ""),
                text_event("a"),
                end("t"),
                start("t", ""),
                text_event("b"),
                end("t"),
            ]
        );
    }

    #[test]
    fn a_self_closing_tag_is_a_start_then_an_end() {
        assert_eq!(
            events(r#"<w:b/><w:i w:val="0" /><a:br/>"#),
            vec![
                start("b", ""),
                end("b"),
                start("i", r#" w:val="0" "#),
                end("i"),
                start("br", ""),
                end("br"),
            ]
        );
    }

    #[test]
    fn nesting_comes_through_in_document_order() {
        let names: Vec<String> = scan("<a><b><c/></b><d>x</d></a>")
            .map(|event| match event {
                Event::Start { name, .. } => format!("<{name}"),
                Event::End { name } => format!("{name}>"),
                Event::Text(text) => text.into_owned(),
            })
            .collect();
        assert_eq!(names, ["<a", "<b", "<c", "c>", "b>", "<d", "x", "d>", "a>"]);
    }

    #[test]
    fn entities_and_character_references_are_decoded() {
        assert_eq!(
            events("<t>a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos; &#233; &#x263A; &#X41;</t>")
                [1],
            Event::Text(Cow::Owned("a & b <c> \"d\" 'e' é ☺ A".to_string()))
        );
        // An ampersand that starts no reference is kept, as is one whose
        // number is no character.
        assert_eq!(
            unescape("fish & chips &bogus; &#xD800;"),
            "fish & chips &bogus; \u{fffd}"
        );
        assert!(matches!(unescape("plain"), Cow::Borrowed("plain")));
    }

    #[test]
    fn comments_declarations_and_instructions_are_skipped_and_cdata_is_text() {
        assert_eq!(
            events(
                "<?xml version=\"1.0\"?><!DOCTYPE x><!-- a <b> comment --><t><![CDATA[1 < 2 & 3]]></t><?pi x?>"
            ),
            vec![start("t", ""), text_event("1 < 2 & 3"), end("t")]
        );
    }

    #[test]
    fn attributes_are_read_by_local_name_in_either_quote() {
        let attrs = r#" w:val='Heading1' r:id="rId7" id="256" xmlns:w="urn:w" title="a &gt; b""#;
        assert_eq!(attr(attrs, "val").as_deref(), Some("Heading1"));
        assert_eq!(attr(attrs, "title").as_deref(), Some("a > b"));
        // The plain `id` comes first; the relationship id is the prefixed one.
        assert_eq!(attr(attrs, "id").as_deref(), Some("rId7"));
        assert_eq!(rel_id(r#" id="256" r:id="rId7""#).as_deref(), Some("rId7"));
        // A namespace declaration is not an attribute of the element.
        assert_eq!(attr(attrs, "w"), None);
        assert_eq!(attr(attrs, "missing"), None);
    }

    #[test]
    fn a_greater_than_inside_a_quoted_value_does_not_end_the_tag() {
        assert_eq!(
            events(r#"<a:t note="x > y">z</a:t>"#),
            vec![start("t", r#" note="x > y""#), text_event("z"), end("t")]
        );
    }

    #[test]
    fn a_part_cut_off_mid_tag_ends_where_it_was_cut() {
        assert_eq!(
            events("<w:t>kept</w:t><w:t attr=\"never"),
            vec![start("t", ""), text_event("kept"), end("t")]
        );
    }

    #[test]
    fn on_off_properties_are_on_unless_they_say_off() {
        assert!(toggle(""));
        assert!(toggle(r#" w:val="1""#));
        assert!(toggle(r#" w:val="true""#));
        assert!(!toggle(r#" w:val="0""#));
        assert!(!toggle(r#" w:val="false""#));
        assert!(!toggle(r#" w:val="off""#));
    }

    #[test]
    fn relationship_targets_resolve_against_their_folder() {
        assert_eq!(
            resolve("ppt/", "slides/slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(
            resolve("xl/", "/xl/worksheets/sheet2.xml"),
            "xl/worksheets/sheet2.xml"
        );
        assert_eq!(resolve("ppt/slides/", "../media/a.png"), "ppt/media/a.png");
        assert_eq!(
            resolve("xl/", "./worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        let rels = relationships(
            r#"<Relationships>
                 <Relationship Id="rId1" Type="t" Target="slides/slide1.xml"/>
                 <Relationship Id="rId9" Type="h" Target="https://example.com" TargetMode="External"/>
               </Relationships>"#,
            "ppt/",
        );
        assert_eq!(
            rels.get("rId1").map(String::as_str),
            Some("ppt/slides/slide1.xml")
        );
        assert_eq!(rels.get("rId9"), None);
    }

    // ── the package ─────────────────────────────────────────────────────────

    #[test]
    fn a_package_with_none_of_the_three_parts_says_so() {
        let err = read_parts(&[
            ("[Content_Types].xml", "<Types/>"),
            ("3D/3dmodel.model", "<model/>"),
        ])
        .err();
        assert_eq!(err.as_deref(), Some("not a Word, PowerPoint or Excel file"));
    }

    #[test]
    fn a_file_that_is_not_a_zip_says_why_quietly() {
        let bytes = b"This is just some text, and certainly not a zip.".to_vec();
        let len = bytes.len() as u64;
        let err = read_package(Package {
            reader: Cursor::new(bytes),
            len,
        })
        .err()
        .unwrap_or_default();
        assert!(err.starts_with("malformed zip"), "{err}");
    }

    #[test]
    fn errors_are_said_quietly() {
        assert_eq!(
            quiet("Permission denied (os error 13)"),
            "permission denied"
        );
        assert_eq!(quiet("IO error"), "IO error");
        assert_eq!(quiet("malformed zip: x"), "malformed zip: x");
    }

    #[test]
    fn parts_are_read_as_utf8_or_by_their_byte_order_mark_as_utf16() {
        assert_eq!(text(b"\xef\xbb\xbf<a/>"), "<a/>");
        assert_eq!(text(b"<a>\xc3\xa9</a>"), "<a>é</a>");
        let le: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain("<a/>".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(text(&le), "<a/>");
        let be: Vec<u8> = [0xfe, 0xff]
            .into_iter()
            .chain("<é/>".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(text(&be), "<é/>");
    }

    #[test]
    fn the_document_is_spaced_like_a_readme() {
        let mut out = Out::default();
        out.push(Block::Gap);
        out.push(heading(1, "Title"));
        out.push(paragraph("one"));
        out.push(Block::Gap);
        out.push(Block::Gap);
        out.push(paragraph("two"));
        out.push(item(0, "•", "a"));
        out.push(item(1, "•", "b"));
        out.push(Block::Quote(vec![span("q1")]));
        out.push(Block::Quote(vec![span("q2")]));
        out.push(Block::Code {
            syntax: None,
            lines: vec!["fn a()".into()],
        });
        out.push(Block::Code {
            syntax: None,
            lines: vec!["fn b()".into()],
        });
        out.push(Block::Gap);
        let reading = out.finish();
        assert_eq!(
            reading.blocks,
            vec![
                heading(1, "Title"),
                Block::Gap,
                paragraph("one"),
                Block::Gap,
                paragraph("two"),
                Block::Gap,
                item(0, "•", "a"),
                item(1, "•", "b"),
                Block::Gap,
                Block::Quote(vec![span("q1")]),
                Block::Quote(Vec::new()),
                Block::Quote(vec![span("q2")]),
                Block::Gap,
                Block::Code {
                    syntax: None,
                    lines: vec!["fn a()".into(), "fn b()".into()],
                },
            ]
        );
        assert!(!reading.truncated);
    }

    #[test]
    fn a_rule_divides_something_from_something() {
        let mut out = Out::default();
        out.push(Block::Rule);
        out.push(paragraph("one"));
        out.push(Block::Rule);
        out.push(Block::Gap);
        out.push(Block::Rule);
        out.push(paragraph("two"));
        assert_eq!(
            out.finish().blocks,
            vec![
                paragraph("one"),
                Block::Gap,
                Block::Rule,
                Block::Gap,
                paragraph("two"),
            ]
        );
    }

    #[test]
    fn the_block_cap_stops_the_document_and_says_so() {
        let mut out = Out::default();
        for n in 0..MAX_BLOCKS {
            out.push(item(0, "•", &n.to_string()));
        }
        assert_eq!(out.blocks.len(), MAX_BLOCKS);
        assert!(!out.truncated, "exactly full is not truncated");
        out.push(item(0, "•", "one too many"));
        out.push(Block::Gap);
        let reading = out.finish();
        assert_eq!(reading.blocks.len(), MAX_BLOCKS);
        assert!(reading.truncated);
    }

    /// A real file, by hand, to see what it reads as:
    /// `DF_OFFICE_SAMPLE=report.docx cargo test -p df-app office_sample --
    /// --ignored --nocapture`. Ignored because the file is not in the tree.
    #[test]
    #[ignore]
    fn office_sample() {
        let Ok(path) = std::env::var("DF_OFFICE_SAMPLE") else {
            return;
        };
        match read(Path::new(&path)) {
            Ok(reading) => {
                for block in &reading.blocks {
                    println!("{block:?}");
                }
                println!("truncated: {}", reading.truncated);
            }
            Err(message) => println!("error: {message}"),
        }
    }

    #[test]
    fn a_table_is_as_wide_as_its_widest_row() {
        let Some(Block::Table {
            align,
            header,
            rows,
        }) = table(
            vec![row(&["a", "b"]), row(&["c"]), row(&["d", "e", "f"])],
            true,
        )
        else {
            panic!("no table");
        };
        assert_eq!(align, vec![Align::Left; 3]);
        assert_eq!(header, row(&["a", "b", ""]));
        assert_eq!(rows, vec![row(&["c", "", ""]), row(&["d", "e", "f"])]);
        assert_eq!(table(vec![Vec::new()], false), None);
    }
}
