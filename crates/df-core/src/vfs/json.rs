//! JSON, both ways, for talking to `rclone rcd` — RFC 8259 and nothing more.
//!
//! The rclone backend speaks rclone's remote-control API, which is JSON over
//! HTTP. A JSON crate would be the first dependency this crate took for a
//! wire format, and the house answer to a wire format is to write the codec:
//! SFTP's lives in [`super::wire`] and D-Bus's in df-app's `dbus`, for the
//! same reason this one lives here — the format is small, fixed, and fully
//! specified, and a codec that is ours is one whose every bound is a line we
//! can point at.
//!
//! ## What it accepts
//!
//! Exactly RFC 8259's grammar: no comments, no trailing commas, no single
//! quotes, no `NaN`, no leading `+` or `0`s, no unescaped control characters
//! inside a string. Leniency in a parser is a promise to accept whatever the
//! other end drifts into, and the other end here is a Go program whose encoder
//! never produces any of those — so a reply that has one is a reply that is not
//! what it claims to be, and saying so is more useful than guessing.
//!
//! Three choices the RFC leaves open, made here:
//!
//! - **Numbers are `f64`.** Every number rclone sends is a size, a count, a
//!   job id or a duration; an `f64` holds every integer up to 2^53 exactly,
//!   which is nine petabytes, and [`Json::as_i64`] refuses anything that is
//!   not a whole number rather than rounding it.
//! - **A lone surrogate is an error.** `\ud800` with no low half cannot be put
//!   in a Rust `String`, and replacing it with U+FFFD would produce a file name
//!   that does not address the file it came from — every later operation on
//!   that row would fail with a confusing "not found". Go's encoder never emits
//!   one (it writes invalid UTF-8 as U+FFFD itself), so this costs nothing.
//! - **Duplicate object keys are kept, and the last one wins** in
//!   [`Json::get`], which is what Go's own decoder does with a duplicate.
//!
//! ## Bounds
//!
//! Nesting is capped at [`MAX_DEPTH`], because the parser is recursive and a
//! reply of ten thousand `[` would otherwise be a stack overflow rather than
//! an error. Every error carries the byte offset it was found at.

use std::fmt::Write as _;

/// How deep arrays and objects may nest.
///
/// 64: rclone's deepest reply is three levels (`core/stats` → `transferring`
/// → one transfer), and 64 is the depth past which a document is an attack
/// rather than an answer. The parser recurses once per level, so this is also
/// the bound on its stack use.
pub const MAX_DEPTH: usize = 64;

/// One JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    /// Pairs in document order, duplicates and all — see the module note.
    Object(Vec<(String, Json)>),
}

/// Why a document did not parse, and where.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid JSON at byte {offset}: {message}")]
pub struct JsonError {
    /// Byte offset into the input where the problem was noticed.
    pub offset: usize,
    pub message: &'static str,
}

/// Parse one complete document. Whitespace around the value is allowed;
/// anything else after it is an error.
pub fn parse(text: &str) -> Result<Json, JsonError> {
    Json::parse(text)
}

impl Json {
    /// See [`parse`].
    pub fn parse(text: &str) -> Result<Json, JsonError> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            text,
            at: 0,
        };
        parser.skip_whitespace();
        let value = parser.value(0)?;
        parser.skip_whitespace();
        if parser.at != parser.bytes.len() {
            return Err(parser.error("unexpected text after the value"));
        }
        Ok(value)
    }

    /// An object from `(key, value)` pairs — how a request body is built.
    pub fn object<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Json)>) -> Json {
        Json::Object(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Add a key to an object, replacing an earlier one of the same name so
    /// the serialised form never carries a duplicate. A no-op on anything that
    /// is not an object.
    pub fn insert(&mut self, key: impl Into<String>, value: Json) {
        if let Json::Object(pairs) = self {
            let key = key.into();
            pairs.retain(|(k, _)| *k != key);
            pairs.push((key, value));
        }
    }

    /// The value of `key` in an object — the last one, when the document
    /// repeats it. `None` for a missing key or a value that is not an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(pairs) => pairs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// A whole number that fits an `i64`. `2.5`, `1e300` and `-0.5` are all
    /// `None`: a size or a job id that is not an integer is not one, and
    /// truncating it would invent a number nobody sent.
    pub fn as_i64(&self) -> Option<i64> {
        let n = self.as_f64()?;
        // 2^63 is exactly representable; anything at or past it is not an i64.
        const LIMIT: f64 = 9_223_372_036_854_775_808.0;
        if !n.is_finite() || n.fract() != 0.0 || !(-LIMIT..LIMIT).contains(&n) {
            return None;
        }
        Some(n as i64)
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(n) => write_number(*n, out),
            Json::String(s) => write_string(s, out),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(pairs) => {
                out.push('{');
                for (i, (key, value)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Compact serialisation: no whitespace, keys in insertion order.
impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::String(s.to_string())
    }
}

impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::String(s)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}

impl From<i64> for Json {
    fn from(n: i64) -> Json {
        Json::Number(n as f64)
    }
}

impl From<u64> for Json {
    fn from(n: u64) -> Json {
        Json::Number(n as f64)
    }
}

impl From<f64> for Json {
    fn from(n: f64) -> Json {
        Json::Number(n)
    }
}

/// A number as JSON writes it.
///
/// Whole numbers without a `.0`, because `{"jobid":3.0}` is a request Go's
/// decoder refuses into an integer field. Everything else through Rust's
/// shortest-round-trip `Display`, which never uses exponent notation and so
/// is always valid JSON. `NaN` and the infinities have no JSON spelling at
/// all; they are written as `null`, which is what `JSON.stringify` does too.
fn write_number(n: f64, out: &mut String) {
    if !n.is_finite() {
        out.push_str("null");
    } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        let _ = write!(out, "{}", n as i64);
    } else {
        let _ = write!(out, "{n}");
    }
}

/// A string with every character JSON requires escaped, and nothing else:
/// non-ASCII goes out as UTF-8, which RFC 8259 §8.1 makes the default.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    /// The same input as a `&str`, so runs of plain string content can be
    /// copied as slices rather than byte by byte. Slicing it is safe at every
    /// offset the parser stops at, because it only ever stops on ASCII.
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn error(&self, message: &'static str) -> JsonError {
        JsonError {
            offset: self.at,
            message,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_whitespace(&mut self) {
        // RFC 8259 §2: exactly these four, and not the rest of Unicode's.
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn expect_literal(&mut self, literal: &'static [u8], value: Json) -> Result<Json, JsonError> {
        if self.bytes[self.at..].starts_with(literal) {
            self.at += literal.len();
            Ok(value)
        } else {
            Err(self.error("unknown literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.expect_literal(b"true", Json::Bool(true)),
            Some(b'f') => self.expect_literal(b"false", Json::Bool(false)),
            Some(b'n') => self.expect_literal(b"null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.at += 1; // `{`
        let mut pairs = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(pairs));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a string key"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(self.error("expected `:` after a key"));
            }
            self.at += 1;
            self.skip_whitespace();
            let value = self.value(depth)?;
            pairs.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(pairs));
                }
                None => return Err(self.error("unterminated object")),
                Some(_) => return Err(self.error("expected `,` or `}` in an object")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.at += 1; // `[`
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                None => return Err(self.error("unterminated array")),
                Some(_) => return Err(self.error("expected `,` or `]` in an array")),
            }
        }
    }

    /// RFC 8259 §6, to the letter: `-? (0 | [1-9][0-9]*) (. [0-9]+)?
    /// ([eE] [+-]? [0-9]+)?`. The grammar is checked here and the conversion
    /// handed to std, whose `f64` parse is correctly rounded.
    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.at += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("a number may not have a leading zero"));
                }
            }
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.error("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit after `.`"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("expected a digit in the exponent"));
            }
            self.digits();
        }
        let literal = &self.text[start..self.at];
        let value: f64 = literal.parse().map_err(|_| JsonError {
            offset: start,
            message: "number out of range",
        })?;
        // `1e400` parses to infinity, which no JSON value is.
        if !value.is_finite() {
            return Err(JsonError {
                offset: start,
                message: "number out of range",
            });
        }
        Ok(Json::Number(value))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }

    /// A string, opening quote at `self.at`.
    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1; // `"`
        let mut out = String::new();
        loop {
            // Copy the plain run up to the next quote, backslash or control
            // character in one slice. Those three are all ASCII, so the run
            // always ends on a character boundary.
            let run_start = self.at;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.at += 1;
            }
            out.push_str(&self.text[run_start..self.at]);
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                Some(_) => return Err(self.error("unescaped control character in a string")),
            }
        }
    }

    /// One escape, the backslash already consumed.
    fn escape(&mut self, out: &mut String) -> Result<(), JsonError> {
        let Some(b) = self.peek() else {
            return Err(self.error("unterminated escape"));
        };
        self.at += 1;
        match b {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let escape_at = self.at - 2;
                let unit = self.hex4()?;
                let code = match unit {
                    // A high surrogate: the low half must follow as its own
                    // `\u` escape, and the pair is one character.
                    0xD800..=0xDBFF => {
                        if !self.bytes[self.at..].starts_with(b"\\u") {
                            return Err(JsonError {
                                offset: escape_at,
                                message: "a high surrogate without its low half",
                            });
                        }
                        self.at += 2;
                        let low = self.hex4()?;
                        if !(0xDC00..=0xDFFF).contains(&low) {
                            return Err(JsonError {
                                offset: escape_at,
                                message: "a high surrogate without its low half",
                            });
                        }
                        0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
                    }
                    0xDC00..=0xDFFF => {
                        return Err(JsonError {
                            offset: escape_at,
                            message: "a low surrogate with no high half",
                        })
                    }
                    other => other,
                };
                match char::from_u32(code) {
                    Some(c) => out.push(c),
                    None => {
                        return Err(JsonError {
                            offset: escape_at,
                            message: "not a Unicode scalar value",
                        })
                    }
                }
            }
            _ => {
                self.at -= 1;
                return Err(self.error("unknown escape"));
            }
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let Some(digits) = self.bytes.get(self.at..self.at + 4) else {
            return Err(self.error("a \\u escape needs four hex digits"));
        };
        let mut value = 0u32;
        for &d in digits {
            let nibble = match d {
                b'0'..=b'9' => d - b'0',
                b'a'..=b'f' => d - b'a' + 10,
                b'A'..=b'F' => d - b'A' + 10,
                _ => return Err(self.error("a \\u escape needs four hex digits")),
            };
            value = value * 16 + u32::from(nibble);
        }
        self.at += 4;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on a bad fixture is the point

    use super::*;

    #[test]
    fn scalars_parse() {
        assert_eq!(parse("null").unwrap(), Json::Null);
        assert_eq!(parse("true").unwrap(), Json::Bool(true));
        assert_eq!(parse(" false ").unwrap(), Json::Bool(false));
        assert_eq!(parse("0").unwrap(), Json::Number(0.0));
        assert_eq!(parse("-12").unwrap(), Json::Number(-12.0));
        assert_eq!(parse("3.25").unwrap(), Json::Number(3.25));
        assert_eq!(parse("1e3").unwrap(), Json::Number(1000.0));
        assert_eq!(parse("2.5E-1").unwrap(), Json::Number(0.25));
        assert_eq!(parse("\"hi\"").unwrap(), Json::String("hi".into()));
    }

    /// The shape of a real `operations/list` reply — tabs, newlines, nested
    /// objects in an array — which is what the backend actually reads.
    #[test]
    fn an_rclone_listing_parses() {
        let text = "{\n\t\"list\": [\n\t\t{\n\t\t\t\"Path\": \"a.txt\",\n\t\t\t\"Name\": \"a.txt\",\n\t\t\t\"Size\": 3,\n\t\t\t\"ModTime\": \"2026-09-25T10:30:30.687697819-05:00\",\n\t\t\t\"IsDir\": false\n\t\t}\n\t]\n}";
        let json = parse(text).unwrap();
        let list = json.get("list").and_then(Json::as_array).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].get("Name").and_then(Json::as_str), Some("a.txt"));
        assert_eq!(list[0].get("Size").and_then(Json::as_i64), Some(3));
        assert_eq!(list[0].get("IsDir").and_then(Json::as_bool), Some(false));
        assert_eq!(list[0].get("Missing"), None);
    }

    #[test]
    fn every_escape_decodes() {
        let json = parse(r#""q\" b\\ s\/ \b\f\n\r\t u\u00e9 \u0041""#).unwrap();
        assert_eq!(
            json.as_str().unwrap(),
            "q\" b\\ s/ \u{8}\u{c}\n\r\t u\u{e9} A"
        );
    }

    #[test]
    fn surrogate_pairs_are_one_character_and_lone_halves_are_errors() {
        // U+1F600, as a pair.
        let json = parse(r#""\ud83d\ude00""#).unwrap();
        assert_eq!(json.as_str().unwrap(), "\u{1F600}");
        // Upper-case hex too.
        assert_eq!(
            parse(r#""\uD83D\uDE00""#).unwrap().as_str().unwrap(),
            "\u{1F600}"
        );
        let lone_high = parse(r#""a\ud83d""#).unwrap_err();
        assert_eq!(lone_high.offset, 2, "{lone_high}");
        assert!(lone_high.message.contains("low half"));
        assert!(parse(r#""\ud83dx""#).is_err());
        assert!(parse(r#""\ud83d\u0041""#).is_err(), "not a low surrogate");
        let lone_low = parse(r#""\ude00""#).unwrap_err();
        assert!(lone_low.message.contains("high half"), "{lone_low}");
    }

    #[test]
    fn utf8_passes_through_both_ways() {
        let json = parse("\"Übersicht — 日本\"").unwrap();
        assert_eq!(json.as_str().unwrap(), "Übersicht — 日本");
        assert_eq!(json.to_string(), "\"Übersicht — 日本\"");
    }

    #[test]
    fn values_round_trip_through_the_serialiser() {
        let value = Json::object([
            ("fs", Json::from("r2:bucket")),
            ("remote", Json::from("photos/2024 \"best\"\n")),
            ("_async", Json::from(true)),
            ("jobid", Json::from(42_i64)),
            ("ratio", Json::from(0.1)),
            ("big", Json::from(9_007_199_254_740_991_u64)),
            ("neg", Json::from(-3_i64)),
            ("nothing", Json::Null),
            (
                "nested",
                Json::Array(vec![
                    Json::Array(vec![]),
                    Json::object(Vec::<(String, Json)>::new()),
                    Json::from("\u{1}\u{1f}"),
                ]),
            ),
        ]);
        let text = value.to_string();
        assert_eq!(parse(&text).unwrap(), value, "{text}");
        assert!(
            text.contains("\"jobid\":42,"),
            "no `.0` on an integer: {text}"
        );
        assert!(text.contains("\\u0001\\u001f"), "{text}");
        assert!(
            !text.contains(", ") && !text.contains(": "),
            "compact: {text}"
        );
    }

    #[test]
    fn non_finite_numbers_serialise_as_null() {
        assert_eq!(Json::Number(f64::NAN).to_string(), "null");
        assert_eq!(Json::Number(f64::INFINITY).to_string(), "null");
    }

    #[test]
    fn as_i64_refuses_what_is_not_a_whole_number() {
        assert_eq!(Json::Number(3.0).as_i64(), Some(3));
        assert_eq!(Json::Number(-1.0).as_i64(), Some(-1));
        assert_eq!(Json::Number(2.5).as_i64(), None);
        assert_eq!(Json::Number(1e300).as_i64(), None);
        assert_eq!(Json::Number(f64::NAN).as_i64(), None);
        assert_eq!(Json::from("3").as_i64(), None, "a string is not a number");
    }

    #[test]
    fn duplicate_keys_keep_the_last_and_insert_replaces() {
        let json = parse(r#"{"a":1,"a":2}"#).unwrap();
        assert_eq!(json.get("a").and_then(Json::as_i64), Some(2));
        let mut built = Json::object([("a", Json::from(1_i64))]);
        built.insert("a", Json::from(2_i64));
        built.insert("b", Json::from(true));
        assert_eq!(built.to_string(), r#"{"a":2,"b":true}"#);
    }

    #[test]
    fn malformed_input_is_refused_with_an_offset() {
        let cases: &[(&str, usize)] = &[
            ("", 0),
            ("   ", 3),
            ("{", 1),
            ("[1,", 3),
            ("[1 2]", 3),
            ("{\"a\" 1}", 5),
            ("{\"a\":1,}", 7),
            ("[1,]", 3),
            ("{a:1}", 1),
            ("'x'", 0),
            ("tru", 0),
            ("nul", 0),
            ("01", 1),
            ("-", 1),
            ("1.", 2),
            ("1e", 2),
            ("+1", 0),
            (".5", 0),
            ("NaN", 0),
            ("1e400", 0),
            ("\"abc", 4),
            ("\"a\u{1}b\"", 2),
            ("\"\\x\"", 2),
            ("\"\\u12\"", 3),
            ("\"\\u12zz\"", 3),
            ("{} x", 3),
            ("[] []", 3),
            ("// comment\n1", 0),
        ];
        for (text, offset) in cases {
            let error = parse(text).expect_err(text);
            assert_eq!(error.offset, *offset, "{text:?}: {error}");
            assert!(error.to_string().contains("invalid JSON at byte"));
        }
    }

    #[test]
    fn nesting_is_capped_rather_than_overflowing_the_stack() {
        let deep_ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(parse(&deep_ok).is_ok());
        let too_deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        let error = parse(&too_deep).unwrap_err();
        assert_eq!(error.message, "nested too deeply");
        assert_eq!(error.offset, MAX_DEPTH);
        // A hostile document far past the cap is still just an error.
        assert!(parse(&"{\"a\":".repeat(100_000)).is_err());
    }
}
