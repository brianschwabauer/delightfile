//! A hand-rolled reader for the slice of TOML delightfile's config files use.
//!
//! PLAN §1 forbids the `toml` crate, and §3 says what the parser actually has
//! to survive: *"invalid lines warn human-readably while valid ones still
//! apply"*. That second half is the whole reason this file exists rather than a
//! dependency. A spec-complete parser answers "is this document valid?" — one
//! bad line and the user's entire keymap is gone, which is precisely the moment
//! a file manager must not forget where the bookmarks are. This one answers
//! "what could I understand, and what should I tell you about the rest?": every
//! line is parsed on its own, a line that makes no sense becomes a
//! [`ConfigWarning`] naming the file and the 1-based line number, and the lines
//! around it are applied as if nothing happened.
//!
//! Ported and generalized from delightviewer's `parse_toml_tables`, which
//! handled only `[table]` headers over `"string" = "string"` pairs. This one
//! adds the value types the config files in PLAN §3 need:
//!
//! - `[table]` and `[[array.of.tables]]` headers, dotted names, quoted segments
//! - strings (double-quoted with escapes, single-quoted literal)
//! - integers, floats, booleans
//! - arrays of any of the above, **including arrays written across lines**
//!   (`ratio = [\n 1,\n 4,\n 3,\n]`), which is how a person actually writes a
//!   list of openers
//! - `#` comments, whole-line or trailing, quote-aware so `"#"` is a value
//!
//! Deliberately **not** supported, because nothing in PLAN §3 spells anything
//! this way and every feature is another thing to get subtly wrong: dates and
//! times, inline tables (`{ a = 1 }`), multi-line basic/literal strings
//! (`"""`), dotted keys inside a table, and integer prefixes (`0x`, `0o`,
//! `0b`). Each of those produces a warning on the line that used it rather than
//! a wrong value, so the failure mode is a message and not a mystery.

use std::path::{Path, PathBuf};

/// One thing the parser (or a config reader built on it) could not understand.
///
/// It carries the file and the line because a warning that says only
/// "unrecognized value" is a warning the user cannot act on — they have four
/// config files and no idea which one is angry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub file: PathBuf,
    /// 1-based, so it matches what an editor's gutter shows.
    pub line: usize,
    pub message: String,
}

impl ConfigWarning {
    pub fn new(file: &Path, line: usize, message: impl Into<String>) -> ConfigWarning {
        let w = ConfigWarning {
            file: file.to_path_buf(),
            line,
            message: message.into(),
        };
        // Logged here rather than at the call site so that *every* warning
        // reaches the log exactly once, whoever produced it.
        log::warn!("{w}");
        w
    }
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file.display(), self.line, self.message)
    }
}

/// A TOML scalar or array of them. No table variant: tables are the document's
/// top-level structure here, not a value, because nothing in PLAN §3 nests one
/// inside an array element.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Integers coerce to floats: a person who writes `1` for a ratio meant
    /// `1.0`, and refusing it would be pedantry with a warning attached.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(v) => Some(v),
            _ => None,
        }
    }

    /// `["a", "b"]` → `["a", "b"]`, and `None` if any element is not a string.
    /// All-or-nothing on purpose: half an opener list is worse than a warning.
    pub fn as_str_array(&self) -> Option<Vec<&str>> {
        self.as_array()?.iter().map(Value::as_str).collect()
    }

    pub fn as_int_array(&self) -> Option<Vec<i64>> {
        self.as_array()?.iter().map(Value::as_int).collect()
    }

    /// For warning text — "expected an integer, found a string".
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "a string",
            Value::Int(_) => "an integer",
            Value::Float(_) => "a float",
            Value::Bool(_) => "a boolean",
            Value::Array(_) => "an array",
        }
    }
}

/// One `key = value` line, with the line it came from so a reader that rejects
/// the *value* can still point at the right place.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    pub line: usize,
}

/// A `[header]` and everything under it, in file order.
///
/// `entries` is a `Vec` and not a map because **order is meaning** here: the
/// `[goto]` bookmarks and the opener rules are read top-to-bottom, and the
/// which-key card shows chords in declaration order (PLAN §4).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    /// Dotted and unquoted: `opener.zed`. Empty for the implicit root table
    /// that holds key/value lines written before any header.
    pub name: String,
    /// True when the header was `[[double]]` — one element of an array of
    /// tables, of which there may be several with the same name.
    pub array_element: bool,
    pub line: usize,
    pub entries: Vec<Entry>,
}

impl Table {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entry(key).map(|e| &e.value)
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        // Last wins, matching TOML's own "a later definition overrides" feel
        // for the one case a person hits it: pasting a block twice.
        self.entries.iter().rev().find(|e| e.key == key)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Everything a file said, plus everything it got wrong.
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub tables: Vec<Table>,
    pub warnings: Vec<ConfigWarning>,
}

impl Document {
    /// The first table with this exact dotted name.
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// Every table with this name — the `[[array.of.tables]]` case.
    pub fn tables_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Table> {
        self.tables.iter().filter(move |t| t.name == name)
    }

    /// Tables whose dotted name starts with `prefix.`, in file order — how
    /// `[opener.zed]`, `[opener.edit]`, … are collected. The returned `&str` is
    /// the part after the prefix.
    pub fn tables_under<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a Table)> {
        self.tables.iter().filter_map(move |t| {
            t.name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('.'))
                .map(|rest| (rest, t))
        })
    }

    /// Key/value lines written before any `[header]`.
    pub fn root(&self) -> Option<&Table> {
        self.tables.first().filter(|t| t.name.is_empty())
    }
}

/// Parse `text` as the TOML subset described in this module's header.
///
/// Never fails: the return value is what could be understood plus a warning for
/// everything that could not. `file` is only used to label those warnings.
pub fn parse(text: &str, file: &Path) -> Document {
    let lines: Vec<&str> = text.lines().collect();
    let mut doc = Document {
        // The implicit root table, so a bare `key = value` before any header
        // has somewhere to land instead of being an error.
        tables: vec![Table::default()],
        warnings: Vec::new(),
    };
    let mut current = 0usize;
    let mut i = 0usize;

    while i < lines.len() {
        let start_line = i + 1;
        let line = strip_comment(lines[i]).trim();
        i += 1;
        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') {
            match parse_header(line) {
                Ok((name, array_element)) => {
                    doc.tables.push(Table {
                        name,
                        array_element,
                        line: start_line,
                        entries: Vec::new(),
                    });
                    current = doc.tables.len() - 1;
                }
                Err(msg) => doc.warnings.push(ConfigWarning::new(file, start_line, msg)),
            }
            continue;
        }

        let Some((key_text, mut value_text)) = split_key_value(line) else {
            doc.warnings.push(ConfigWarning::new(
                file,
                start_line,
                format!("expected `key = value`, found `{line}`"),
            ));
            continue;
        };

        // An array may be written across lines. Keep pulling lines in until the
        // brackets balance — this is the shape every list a human writes has.
        let mut joined = String::new();
        if value_text.starts_with('[') && bracket_balance(value_text) > 0 {
            joined.push_str(value_text);
            let mut depth = bracket_balance(value_text);
            while depth > 0 && i < lines.len() {
                let next = strip_comment(lines[i]).trim();
                i += 1;
                depth += bracket_balance(next);
                joined.push(' ');
                joined.push_str(next);
            }
            if depth > 0 {
                doc.warnings.push(ConfigWarning::new(
                    file,
                    start_line,
                    "array is never closed — missing `]`",
                ));
                continue;
            }
            value_text = &joined;
        }

        let key = match parse_key(key_text) {
            Ok(k) => k,
            Err(msg) => {
                doc.warnings.push(ConfigWarning::new(file, start_line, msg));
                continue;
            }
        };
        match parse_value(value_text) {
            Ok(value) => doc.tables[current].entries.push(Entry {
                key,
                value,
                line: start_line,
            }),
            Err(msg) => doc.warnings.push(ConfigWarning::new(
                file,
                start_line,
                format!("{key_text}: {msg}"),
            )),
        }
    }

    // Drop the root table when nothing landed in it, so `tables` is exactly the
    // headers the file wrote.
    if doc.tables[0].name.is_empty() && doc.tables[0].is_empty() {
        doc.tables.remove(0);
    }
    doc
}

/// `[a.b]` → `("a.b", false)`, `[[a.b]]` → `("a.b", true)`.
fn parse_header(line: &str) -> Result<(String, bool), String> {
    let (inner, array_element) = if let Some(rest) = line.strip_prefix("[[") {
        (
            rest.strip_suffix("]]")
                .ok_or_else(|| format!("unterminated [[table]] header `{line}`"))?,
            true,
        )
    } else {
        let rest = line
            .strip_prefix('[')
            .ok_or_else(|| format!("unterminated [table] header `{line}`"))?;
        (
            rest.strip_suffix(']')
                .ok_or_else(|| format!("unterminated [table] header `{line}`"))?,
            false,
        )
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return Err("empty table name".to_string());
    }
    let mut segments = Vec::new();
    for segment in split_top_level(inner, '.')? {
        let segment = segment.trim();
        if segment.is_empty() {
            return Err(format!("empty segment in table name `{inner}`"));
        }
        segments.push(parse_key(segment)?);
    }
    Ok((segments.join("."), array_element))
}

/// A bare key, or a quoted one (`"g g"`, which is how a chord is spelled).
fn parse_key(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty key".to_string());
    }
    if text.starts_with('"') || text.starts_with('\'') {
        return match parse_value(text)? {
            Value::String(s) => Ok(s),
            other => Err(format!("expected a key, found {}", other.type_name())),
        };
    }
    if text.contains(char::is_whitespace) {
        return Err(format!("bare key `{text}` contains whitespace — quote it"));
    }
    Ok(text.to_string())
}

fn parse_value(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("missing value".to_string());
    }
    if text.starts_with("\"\"\"") || text.starts_with("'''") {
        return Err("multi-line strings are not supported".to_string());
    }
    if text.starts_with('{') {
        return Err("inline tables are not supported — use a [table] header".to_string());
    }
    match text.as_bytes()[0] {
        b'"' => parse_basic_string(text),
        b'\'' => parse_literal_string(text),
        b'[' => parse_array(text),
        _ => {
            if text == "true" {
                return Ok(Value::Bool(true));
            }
            if text == "false" {
                return Ok(Value::Bool(false));
            }
            parse_number(text)
        }
    }
}

fn parse_basic_string(text: &str) -> Result<Value, String> {
    let mut out = String::new();
    let mut chars = text.char_indices();
    chars.next(); // the opening quote
    let mut closed_at = None;
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                closed_at = Some(i + 1);
                break;
            }
            '\\' => {
                let (_, esc) = chars.next().ok_or("string ends in a backslash")?;
                match esc {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    '"' => out.push('"'),
                    '\'' => out.push('\''),
                    '\\' => out.push('\\'),
                    // `\u`/`\U` matter here: theme.toml's directory icons are
                    // private-use glyphs that some editors will not render, so
                    // being able to write one as a code point is the difference
                    // between an editable config and a wall of tofu.
                    'u' => out.push(parse_unicode_escape(&mut chars, 4)?),
                    'U' => out.push(parse_unicode_escape(&mut chars, 8)?),
                    other => return Err(format!("unknown escape `\\{other}`")),
                }
            }
            other => out.push(other),
        }
    }
    let closed_at = closed_at.ok_or("string is never closed — missing `\"`")?;
    let trailing = text[closed_at..].trim();
    if !trailing.is_empty() {
        return Err(format!("unexpected `{trailing}` after the closing quote"));
    }
    Ok(Value::String(out))
}

fn parse_unicode_escape(
    chars: &mut std::str::CharIndices<'_>,
    width: usize,
) -> Result<char, String> {
    let mut hex = String::new();
    for _ in 0..width {
        let (_, c) = chars.next().ok_or("truncated \\u escape")?;
        hex.push(c);
    }
    let n = u32::from_str_radix(&hex, 16).map_err(|_| format!("`{hex}` is not hexadecimal"))?;
    char::from_u32(n).ok_or_else(|| format!("U+{hex} is not a character"))
}

fn parse_literal_string(text: &str) -> Result<Value, String> {
    let rest = &text[1..];
    let end = rest
        .find('\'')
        .ok_or("string is never closed — missing `'`")?;
    let trailing = rest[end + 1..].trim();
    if !trailing.is_empty() {
        return Err(format!("unexpected `{trailing}` after the closing quote"));
    }
    Ok(Value::String(rest[..end].to_string()))
}

fn parse_array(text: &str) -> Result<Value, String> {
    let inner = text
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or("array is never closed — missing `]`")?;
    let mut out = Vec::new();
    for element in split_top_level(inner, ',')? {
        let element = element.trim();
        if element.is_empty() {
            // A trailing comma is normal and idiomatic; an empty slot between
            // two commas is a typo, but treating both as "nothing here" costs
            // nothing and never loses a value.
            continue;
        }
        out.push(parse_value(element)?);
    }
    Ok(Value::Array(out))
}

fn parse_number(text: &str) -> Result<Value, String> {
    if text.starts_with("0x") || text.starts_with("0o") || text.starts_with("0b") {
        return Err(format!("`{text}`: only decimal numbers are supported"));
    }
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    if let Ok(i) = cleaned.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    if let Ok(f) = cleaned.parse::<f64>() {
        return Ok(Value::Float(f));
    }
    Err(format!(
        "`{text}` is not a string, number, boolean or array — did you forget the quotes?"
    ))
}

/// Split on `sep`, ignoring separators inside quotes or nested brackets.
fn split_top_level(text: &str, sep: char) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if q == '"' && c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' => depth += 1,
            ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return Err("a quoted string is never closed".to_string());
    }
    out.push(&text[start..]);
    Ok(out)
}

/// Split `key = value` at the first `=` that is not inside quotes.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if q == '"' && c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '=' => return Some((line[..i].trim(), line[i + 1..].trim())),
            _ => {}
        }
    }
    None
}

/// `[` minus `]`, ignoring quoted ones — how a multi-line array knows it is
/// still open.
fn bracket_balance(text: &str) -> i32 {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in text.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if q == '"' && c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Drop a trailing `#` comment, respecting quotes so `key = "#"` survives.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if q == '"' && c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '#' => return &line[..i],
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        parse(text, Path::new("test.toml"))
    }

    #[test]
    fn tables_scalars_and_arrays() {
        let d = doc(r##"
            # a leading comment
            [mgr]
            ratio = [1, 4, 3]
            sort_by = "alphabetical"   # trailing comment
            scrolloff = 5
            show_hidden = false
            image_scale = 1.5
            hash = "#"
            "##);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
        let mgr = d.table("mgr").expect("[mgr]");
        assert_eq!(
            mgr.get("ratio").and_then(Value::as_int_array),
            Some(vec![1, 4, 3])
        );
        assert_eq!(
            mgr.get("sort_by").and_then(Value::as_str),
            Some("alphabetical")
        );
        assert_eq!(mgr.get("scrolloff").and_then(Value::as_int), Some(5));
        assert_eq!(mgr.get("show_hidden").and_then(Value::as_bool), Some(false));
        assert_eq!(mgr.get("image_scale").and_then(Value::as_float), Some(1.5));
        // The `#` inside quotes is a value, not the start of a comment.
        assert_eq!(mgr.get("hash").and_then(Value::as_str), Some("#"));
    }

    /// PLAN §3's rule, as a test: the bad line warns, the good ones still land.
    #[test]
    fn a_bad_line_warns_and_the_rest_still_applies() {
        let d = doc(
            "[mgr]\nscrolloff = 5\nthis line is nonsense\nshow_hidden = true\nlinemode = size\n",
        );
        assert_eq!(d.warnings.len(), 2, "{:?}", d.warnings);
        assert_eq!(d.warnings[0].line, 3);
        assert_eq!(d.warnings[1].line, 5);
        assert!(d.warnings[1].message.contains("quotes"), "{:?}", d.warnings);
        let mgr = d.table("mgr").expect("[mgr]");
        assert_eq!(mgr.get("scrolloff").and_then(Value::as_int), Some(5));
        assert_eq!(mgr.get("show_hidden").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn arrays_may_span_lines() {
        let d = doc("[mgr]\nratio = [\n  1,\n  4,\n  3,\n]\nscrolloff = 2\n");
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
        let mgr = d.table("mgr").expect("[mgr]");
        assert_eq!(
            mgr.get("ratio").and_then(Value::as_int_array),
            Some(vec![1, 4, 3])
        );
        // …and the parser resumes on the line after the closing bracket.
        assert_eq!(mgr.get("scrolloff").and_then(Value::as_int), Some(2));
    }

    #[test]
    fn unclosed_array_warns_once() {
        let d = doc("[mgr]\nratio = [1, 4,\n");
        assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
        assert!(d.warnings[0].message.contains("never closed"));
    }

    #[test]
    fn array_of_tables_and_dotted_names() {
        let d = doc(r#"
            [opener.zed]
            command = "zeditor"

            [[open.rules]]
            mime = "image/*"
            use = ["view", "reveal"]

            [[open.rules]]
            glob = "*.pdf"
            use = ["view"]
            "#);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
        let rules: Vec<&Table> = d.tables_named("open.rules").collect();
        assert_eq!(rules.len(), 2);
        assert!(rules[0].array_element);
        assert_eq!(
            rules[0].get("mime").and_then(Value::as_str),
            Some("image/*")
        );
        assert_eq!(rules[1].get("glob").and_then(Value::as_str), Some("*.pdf"));
        let under: Vec<&str> = d.tables_under("opener").map(|(n, _)| n).collect();
        assert_eq!(under, vec!["zed"]);
    }

    #[test]
    fn quoted_keys_hold_chord_sequences() {
        let d = doc("[files]\n\"g g\" = \"cursor-top\"\n\", m\" = \"sort-mtime\"\n");
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
        let files = d.table("files").expect("[files]");
        assert_eq!(files.entries[0].key, "g g");
        assert_eq!(files.entries[1].key, ", m");
    }

    #[test]
    fn string_escapes_and_literals() {
        let d = doc("[t]\na = \"tab\\there\"\nb = '\\no escapes\\'\nc = \"\\u00e9\"\n");
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
        let t = d.table("t").expect("[t]");
        assert_eq!(t.get("a").and_then(Value::as_str), Some("tab\there"));
        assert_eq!(t.get("b").and_then(Value::as_str), Some("\\no escapes\\"));
        assert_eq!(t.get("c").and_then(Value::as_str), Some("é"));
    }

    #[test]
    fn unsupported_shapes_warn_rather_than_lie() {
        let d = doc("[t]\na = { x = 1 }\nb = 0xff\nc = \"\"\"multi\n");
        assert_eq!(d.warnings.len(), 3, "{:?}", d.warnings);
        assert!(d.warnings[0].message.contains("inline tables"));
        assert!(d.warnings[1].message.contains("decimal"));
        assert!(d.warnings[2].message.contains("multi-line"));
    }

    #[test]
    fn unterminated_header_warns() {
        let d = doc("[mgr\nscrolloff = 5\n");
        assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
        assert!(d.warnings[0].message.contains("unterminated"));
    }

    #[test]
    fn root_table_holds_headerless_pairs() {
        let d = doc("version = 2\n[mgr]\nscrolloff = 1\n");
        assert_eq!(
            d.root()
                .and_then(|t| t.get("version"))
                .and_then(Value::as_int),
            Some(2)
        );
    }

    #[test]
    fn empty_input_is_an_empty_document() {
        let d = doc("");
        assert!(d.tables.is_empty());
        assert!(d.warnings.is_empty());
    }
}
