//! An Excel workbook: `xl/workbook.xml` for the sheets and their order, its
//! relationships for which part each sheet is, `xl/sharedStrings.xml` for the
//! text, and the sheets.
//!
//! Each sheet is its name as a heading and its cells as one table. A cell
//! names its own place (`B7`), so the table is built from those references
//! rather than from the order cells happen to be written in — a sheet writes
//! only the cells that have something in them, and the gaps are real.
//!
//! **The values are as the file stores them.** Text is shared across the
//! workbook and stored once (`sharedStrings.xml`), a cell holding only its
//! index; everything else is in the cell. What is *not* applied is the number
//! format, which lives in the styles part and is a small language of its own:
//! a date is the serial number Excel counts days in (`45566`), a percentage is
//! the fraction (`0.25`), a currency a bare number, and a formula shows the
//! result Excel cached the last time it calculated. A column of numbers is
//! still a column of numbers, and is right-aligned as one.
//!
//! **The table starts at the first row with anything in it**, and that row is
//! its header — which is what the first filled row of nearly every sheet is.
//! Trailing empty rows and columns are trimmed; empty rows between filled ones
//! are kept, because there they are part of the layout somebody chose. Hidden
//! sheets are read like any other: hidden is a view setting, not a secret.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use super::super::markdown::{Align, Block, Span};
use super::{attr, plain, rel_id, relationships, scan, Event, Out, Package};

/// The most rows of a sheet read under its header: 500. A preview of a sheet
/// is its top; the rest is what opening it is for.
pub const MAX_ROWS: u32 = 500;

/// The most columns read: 30, `A` to `AD`. Past that a table in a preview
/// pane is a row of slivers, and nobody reads a sheet that wide without
/// scrolling sideways anyway.
pub const MAX_COLUMNS: usize = 30;

pub(super) fn read<R: Read + Seek>(
    workbook: &str,
    package: &mut Package<R>,
    out: &mut Out,
) -> Result<(), String> {
    let rels = package
        .part("xl/_rels/workbook.xml.rels")?
        .map(|xml| relationships(&xml, "xl/"))
        .unwrap_or_default();
    let strings = package
        .part("xl/sharedStrings.xml")?
        .map(|xml| shared_strings(&xml))
        .unwrap_or_default();
    let sheets: Vec<(String, Option<String>)> = scan(workbook)
        .filter_map(|event| match event {
            Event::Start {
                name: "sheet",
                attrs,
            } => Some((
                attr(attrs, "name").unwrap_or_default().into_owned(),
                rel_id(attrs).and_then(|id| rels.get(id.as_ref()).cloned()),
            )),
            _ => None,
        })
        .collect();
    for (name, part) in sheets {
        if out.full() {
            out.truncate();
            break;
        }
        // The workbook ends at the sheet the package's budget has no room
        // for, rather than naming it over nothing.
        if part.as_deref().is_some_and(|part| !package.fits(part)) {
            package.cut_short();
            break;
        }
        out.push(Block::Heading {
            level: 2,
            spans: plain(name),
        });
        // One sheet that will not read is a line under its name; the other
        // sheets are still worth showing.
        let xml = match part {
            Some(part) => package.part(&part),
            None => Ok(None),
        };
        match xml {
            Ok(Some(xml)) => {
                let sheet = sheet(&xml, &strings);
                if sheet.truncated {
                    out.truncate();
                }
                if let Some(table) = sheet.table {
                    out.push(table);
                }
            }
            Ok(None) => {}
            Err(message) => out.push(Block::Paragraph(plain(message))),
        }
    }
    Ok(())
}

/// Every shared string, in index order: each `<si>` is its text, its rich-text
/// runs run together. The phonetic guide some East Asian text carries
/// (`<rPh>`) is a reading aid printed above the text, not more of it.
fn shared_strings(xml: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current: Option<String> = None;
    let mut in_text = false;
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
            Event::Start { name: "si", .. } => current = Some(String::new()),
            Event::Start { name: "rPh", .. } => skipping = 1,
            Event::Start { name: "t", .. } => in_text = current.is_some(),
            Event::End { name: "t" } => in_text = false,
            Event::Text(text) if in_text => {
                if let Some(current) = &mut current {
                    current.push_str(&text);
                }
            }
            Event::End { name: "si" } => strings.push(current.take().unwrap_or_default()),
            _ => {}
        }
    }
    strings
}

/// A sheet read: its table, if it has any cells, and whether a cap cut it.
struct Sheet {
    table: Option<Block>,
    truncated: bool,
}

/// One cell being read.
#[derive(Default)]
struct Cell {
    column: usize,
    /// `t`: `s` shared, `inlineStr`, `str` formula text, `b`, `e`, `n`.
    kind: Option<String>,
    value: String,
    inline: String,
}

/// The cells of a sheet part, as a table.
fn sheet(xml: &str, strings: &[String]) -> Sheet {
    // row → column → text, for the rows kept.
    let mut grid: BTreeMap<u32, BTreeMap<usize, String>> = BTreeMap::new();
    let mut header: Option<u32> = None;
    let mut truncated = false;
    let mut row = 0u32;
    let mut next_column = 0usize;
    let mut cell: Option<Cell> = None;
    let (mut in_value, mut in_inline, mut in_text) = (false, false, false);
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
            Event::Start { name: "row", attrs } => {
                // A row that does not say where it is follows the last one.
                row = attr(attrs, "r")
                    .and_then(|r| r.trim().parse().ok())
                    .unwrap_or(row + 1);
                next_column = 0;
            }
            Event::Start { name: "c", attrs } => {
                let column = attr(attrs, "r")
                    .and_then(|r| column_of(&r))
                    .unwrap_or(next_column);
                cell = Some(Cell {
                    column,
                    kind: attr(attrs, "t").map(|t| t.into_owned()),
                    ..Cell::default()
                });
            }
            Event::Start { name: "v", .. } => in_value = cell.is_some(),
            Event::Start { name: "is", .. } => in_inline = cell.is_some(),
            Event::Start { name: "t", .. } => in_text = in_inline,
            Event::Start { name: "rPh", .. } => skipping = 1,
            Event::Text(text) => {
                if let Some(cell) = &mut cell {
                    if in_value {
                        cell.value.push_str(&text);
                    } else if in_text {
                        cell.inline.push_str(&text);
                    }
                }
            }
            Event::End { name: "v" } => in_value = false,
            Event::End { name: "t" } => in_text = false,
            Event::End { name: "is" } => in_inline = false,
            Event::End { name: "c" } => {
                let Some(finished) = cell.take() else {
                    continue;
                };
                next_column = finished.column + 1;
                let column = finished.column;
                let text = value(finished, strings);
                if text.is_empty() {
                    continue;
                }
                if column >= MAX_COLUMNS {
                    truncated = true;
                    continue;
                }
                let top = *header.get_or_insert(row);
                // Rows come in order; one above the header is a writer that
                // did not keep them so, and is left out rather than allowed
                // to stretch the table.
                if row < top {
                    continue;
                }
                if row > top.saturating_add(MAX_ROWS) {
                    truncated = true;
                    break;
                }
                grid.entry(row).or_default().insert(column, text);
            }
            _ => {}
        }
    }

    Sheet {
        table: build(&grid),
        truncated,
    }
}

/// What a finished cell shows.
fn value(cell: Cell, strings: &[String]) -> String {
    match cell.kind.as_deref() {
        Some("s") => cell
            .value
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|index| strings.get(index))
            .cloned()
            .unwrap_or_default(),
        Some("inlineStr") => cell.inline,
        Some("b") => match cell.value.trim() {
            "1" => "TRUE".to_string(),
            "0" => "FALSE".to_string(),
            other => other.to_string(),
        },
        // A number, a formula's text result, an error (`#DIV/0!`), an ISO
        // date: as written.
        _ => cell.value,
    }
}

/// The column an A1 reference is in, from zero: `A7` → 0, `AA1` → 26. Three
/// letters at most, which is as wide as a sheet goes (`XFD`).
fn column_of(reference: &str) -> Option<usize> {
    let mut column = 0usize;
    let mut letters = 0;
    for byte in reference.bytes().take_while(u8::is_ascii_alphabetic) {
        letters += 1;
        if letters > 3 {
            return None;
        }
        column = column * 26 + usize::from(byte.to_ascii_uppercase() - b'A' + 1);
    }
    column.checked_sub(1)
}

/// The kept cells as a table: from the first filled row to the last, as wide
/// as the widest, the first row the header, and a column right-aligned when
/// every cell under the header that has anything in it is a number.
fn build(grid: &BTreeMap<u32, BTreeMap<usize, String>>) -> Option<Block> {
    let (&top, _) = grid.first_key_value()?;
    let (&bottom, _) = grid.last_key_value()?;
    let width = grid
        .values()
        .filter_map(|columns| columns.keys().next_back())
        .max()
        .map_or(0, |last| last + 1);
    let text = |row: u32, column: usize| {
        grid.get(&row)
            .and_then(|columns| columns.get(&column))
            .map_or("", String::as_str)
    };
    let cells = |row: u32| -> Vec<Vec<Span>> { (0..width).map(|c| plain(text(row, c))).collect() };
    let align = (0..width)
        .map(|column| {
            let filled: Vec<&str> = (top + 1..=bottom)
                .map(|row| text(row, column))
                .filter(|text| !text.is_empty())
                .collect();
            if !filled.is_empty() && filled.iter().all(|text| text.trim().parse::<f64>().is_ok()) {
                Align::Right
            } else {
                Align::Left
            }
        })
        .collect();
    Some(Block::Table {
        align,
        header: cells(top),
        rows: (top + 1..=bottom).map(cells).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{blocks, heading, read_parts, row};
    use super::*;

    /// A workbook of `sheets` (name, sheet XML), with `strings` shared.
    fn workbook(sheets: &[(&str, String)], strings: &[&str]) -> Vec<(String, String)> {
        let listed: String = sheets
            .iter()
            .enumerate()
            .map(|(n, (name, _))| {
                format!(
                    r#"<sheet name="{name}" sheetId="{}" r:id="rId{}"/>"#,
                    n + 1,
                    n + 1
                )
            })
            .collect();
        let rels: String = (0..sheets.len())
            .map(|n| {
                format!(
                    r#"<Relationship Id="rId{}" Type="worksheet" Target="worksheets/sheet{}.xml"/>"#,
                    n + 1,
                    n + 1
                )
            })
            .collect();
        let shared: String = strings
            .iter()
            .map(|s| format!("<si><t>{s}</t></si>"))
            .collect();
        let mut parts = vec![
            ("[Content_Types].xml".to_string(), "<Types/>".to_string()),
            (
                "xl/workbook.xml".to_string(),
                format!(
                    r#"<workbook><bookViews><workbookView/></bookViews><sheets>{listed}</sheets></workbook>"#
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels".to_string(),
                format!(
                    r#"<Relationships>{rels}<Relationship Id="rIdS" Type="sharedStrings" Target="sharedStrings.xml"/></Relationships>"#
                ),
            ),
            (
                "xl/sharedStrings.xml".to_string(),
                format!(
                    r#"<sst count="{0}" uniqueCount="{0}">{shared}</sst>"#,
                    strings.len()
                ),
            ),
        ];
        for (n, (_, xml)) in sheets.iter().enumerate() {
            parts.push((
                format!("xl/worksheets/sheet{}.xml", n + 1),
                format!(
                    "<worksheet><dimension ref=\"A1\"/><sheetData>{xml}</sheetData></worksheet>"
                ),
            ));
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

    #[test]
    fn cells_are_shared_and_inline_strings_numbers_and_booleans() {
        let data = r#"
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><v>2</v></c><c r="D1" t="s"><v>3</v></c></row>
            <row r="2"><c r="A2" t="inlineStr"><is><t>Apples</t></is></c><c r="B2"><v>3</v></c><c r="C2" t="b"><v>1</v></c><c r="D2" t="str"><f>A2&amp;"!"</f><v>Apples!</v></c></row>
            <row r="3"><c r="A3" t="inlineStr"><is><r><t>Pe</t></r><r><rPr><b/></rPr><t>ars</t></r></is></c><c r="B3" s="2"><f>B2*4</f><v>12.5</v></c><c r="C3" t="b"><v>0</v></c><c r="D3" t="e"><v>#DIV/0!</v></c></row>
        "#;
        assert_eq!(
            read(&workbook(
                &[("Fruit", data.to_string())],
                &["Name", "Qty", "Fresh", "Note"]
            )),
            vec![
                heading(2, "Fruit"),
                Block::Gap,
                Block::Table {
                    align: vec![Align::Left, Align::Right, Align::Left, Align::Left],
                    header: row(&["Name", "Qty", "Fresh", "Note"]),
                    rows: vec![
                        row(&["Apples", "3", "TRUE", "Apples!"]),
                        row(&["Pears", "12.5", "FALSE", "#DIV/0!"]),
                    ],
                },
            ]
        );
    }

    #[test]
    fn references_place_cells_and_trailing_emptiness_is_trimmed() {
        // Data starting at row 3, with a gap at row 5, a cell at AA, and a
        // row past the data that holds only an empty string.
        let data = r#"
            <row r="3"><c r="A3" t="inlineStr"><is><t>a</t></is></c><c r="AA3" t="inlineStr"><is><t>far</t></is></c></row>
            <row r="4"><c r="B4"><v>1</v></c></row>
            <row r="6"><c r="A6" t="inlineStr"><is><t>after the gap</t></is></c><c r="C6" t="s"><v>0</v></c></row>
            <row r="9"><c r="A9" t="inlineStr"><is><t></t></is></c><c r="B9" s="3"/></row>
        "#;
        let Block::Table {
            align,
            header,
            rows,
        } = read(&workbook(&[("S", data.to_string())], &[""]))[2].clone()
        else {
            panic!("no table");
        };
        assert_eq!(header.len(), 27, "A through AA");
        assert_eq!(header[0], plain("a"));
        assert_eq!(header[26], plain("far"));
        assert_eq!(rows.len(), 3, "rows 4, 5 and 6; nothing after");
        assert_eq!(rows[0][1], plain("1"));
        assert!(rows[1].iter().all(Vec::is_empty), "the gap is kept");
        assert_eq!(rows[2][0], plain("after the gap"));
        assert_eq!(align[1], Align::Right);
        assert_eq!(align[0], Align::Left);
        assert_eq!(align[26], Align::Left, "an empty column is not a number");
    }

    #[test]
    fn a1_references_name_columns() {
        assert_eq!(column_of("A1"), Some(0));
        assert_eq!(column_of("Z9"), Some(25));
        assert_eq!(column_of("AA10"), Some(26));
        assert_eq!(column_of("AD1"), Some(29));
        assert_eq!(column_of("XFD1048576"), Some(16_383));
        assert_eq!(column_of("b2"), Some(1));
        assert_eq!(column_of("12"), None);
        assert_eq!(column_of("ABCD1"), None);
    }

    #[test]
    fn cells_without_references_follow_the_one_before() {
        let data = r#"<row><c t="inlineStr"><is><t>x</t></is></c><c><v>2</v></c></row><row><c r="B2"><v>3</v></c></row>"#;
        let Block::Table { header, rows, .. } =
            read(&workbook(&[("S", data.to_string())], &[]))[2].clone()
        else {
            panic!("no table");
        };
        assert_eq!(header, row(&["x", "2"]));
        assert_eq!(rows, vec![row(&["", "3"])]);
    }

    #[test]
    fn two_sheets_in_workbook_order() {
        let first = r#"<row r="1"><c r="A1"><v>1</v></c></row>"#.to_string();
        let second = r#"<row r="1"><c r="A1"><v>2</v></c></row>"#.to_string();
        let empty = String::new();
        assert_eq!(
            read(&workbook(
                &[("One", first), ("Two", second), ("Blank", empty)],
                &[]
            )),
            vec![
                heading(2, "One"),
                Block::Gap,
                Block::Table {
                    align: vec![Align::Left],
                    header: row(&["1"]),
                    rows: Vec::new(),
                },
                Block::Gap,
                heading(2, "Two"),
                Block::Gap,
                Block::Table {
                    align: vec![Align::Left],
                    header: row(&["2"]),
                    rows: Vec::new(),
                },
                Block::Gap,
                heading(2, "Blank"),
            ]
        );
    }

    #[test]
    fn a_thirty_first_column_is_cut_and_said_to_be() {
        let header: String = (0..31)
            .map(|c| format!(r#"<c r="{}1"><v>{c}</v></c>"#, column_name(c)))
            .collect();
        let data = format!(r#"<row r="1">{header}</row>"#);
        let parts = workbook(&[("Wide", data)], &[]);
        let parts: Vec<(&str, &str)> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_str()))
            .collect();
        let reading = read_parts(&parts).unwrap_or_else(|e| panic!("{e}"));
        assert!(reading.truncated);
        let Block::Table { header, align, .. } = &reading.blocks[2] else {
            panic!("no table");
        };
        assert_eq!(header.len(), MAX_COLUMNS);
        assert_eq!(align.len(), MAX_COLUMNS);
        assert_eq!(header[29], plain("29"));
    }

    #[test]
    fn rows_past_the_cap_are_cut_and_said_to_be() {
        let data: String = (1..=MAX_ROWS + 5)
            .map(|r| format!(r#"<row r="{}"><c r="A{0}"><v>{0}</v></c></row>"#, r + 1))
            .collect();
        let parts = workbook(&[("Long", data)], &[]);
        let parts: Vec<(&str, &str)> = parts
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_str()))
            .collect();
        let reading = read_parts(&parts).unwrap_or_else(|e| panic!("{e}"));
        assert!(reading.truncated);
        let Block::Table { header, rows, .. } = &reading.blocks[2] else {
            panic!("no table");
        };
        assert_eq!(header, &row(&["2"]));
        assert_eq!(rows.len(), MAX_ROWS as usize);
    }

    #[test]
    fn shared_strings_run_rich_text_together_without_the_phonetic_guide() {
        let xml = r#"<sst><si><t>plain</t></si><si><r><t>ri</t></r><r><rPr><b/></rPr><t>ch</t></r><rPh sb="0" eb="1"><t>ふりがな</t></rPh></si><si><t/></si></sst>"#;
        assert_eq!(shared_strings(xml), vec!["plain", "rich", ""]);
    }

    /// `0` → `A`, `26` → `AA`.
    fn column_name(mut column: usize) -> String {
        let mut name = Vec::new();
        loop {
            name.push(b'A' + (column % 26) as u8);
            if column < 26 {
                break;
            }
            column = column / 26 - 1;
        }
        name.reverse();
        String::from_utf8(name).unwrap_or_default()
    }
}
