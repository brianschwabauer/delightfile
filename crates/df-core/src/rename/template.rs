//! The bulk-rename template: `{name}-{date}{ext}` in, one new name per file
//! out.
//!
//! The card's top field holds one template, and every row below it shows what
//! that template makes of its file. So the language has two jobs: be obvious
//! to someone who has never read this, and never get in the way of the rows
//! while it is being typed.
//!
//! # The language
//!
//! Text is itself. A value is `{kind}`, and a value with arguments is
//! `{kind|arg|arg}`: one separator for everything, so there is only one thing
//! to learn. `{{` is a literal `{`; a `}` on its own is just a `}`. Inside a
//! value, `\|`, `\{`, `\}` and `\\` are a literal pipe, brace, brace and
//! backslash, and any other backslash is kept as typed, so a regex's `\d` needs
//! no doubling. A regex's counted repetition is therefore spelled `0\{2\}`.
//!
//! - `{name}`, `{ext}`, `{parent}`, `{camera}` are **text**, and take a chain
//!   of transforms: `{name|lower|replace|img_|photo-}`. Each transform has a
//!   fixed number of arguments, which is what lets a single `|` separate both
//!   the transforms and their arguments without brackets.
//! - `{n}`, `{nn}`, `{nnn}` … are the counter, 1-based, padded to as many
//!   digits as there are `n`s. Its one argument is where it starts.
//! - `{date}`, `{taken}`, `{created}`, `{modified}` are dates, with an optional
//!   format (`YYYY-MM-DD` if none).
//! - `{width}` and `{height}` are a photo's pixel size.
//!
//! `{ext}` carries its dot. That is what makes [`DEFAULT`], `{name}{ext}`, the
//! identity for *every* file: `Makefile` and `.bashrc` have no extension, and a
//! template that spelled the dot out would put one on the end of each.
//!
//! # Lenient on purpose
//!
//! [`Template::parse`] cannot fail. While someone is typing `{da` on the way to
//! `{date}`, the rows must go on rendering, and a parser that rejected the
//! whole template for one half-typed value would blank the card on every
//! keystroke. So anything that is not a well-formed value (an unclosed `{`, an
//! unknown kind, a transform with too few arguments) stays in the output as the
//! literal text it is, and is also reported as a [`Problem`] with the span the
//! field should underline.
//!
//! An unescaped `{` anywhere inside a value means the value before it was
//! never closed, and starts a new one. That is what keeps a template working
//! while it is edited in the middle: typing `{date|YYYY` in front of an
//! existing `{ext}` leaves `{date|YYYY` as underlined literal text and the
//! `{ext}` still resolving, where reading on to the next `}` would have
//! swallowed the `{ext}` into a date format. The price is that a literal `{`
//! in an argument has to be written `\{`.
//!
//! # Missing values
//!
//! A value can be unanswerable for one file: a PNG has no taken date, a folder
//! has no camera. [`Template::resolve`] says so with [`Missing`] rather than
//! inventing a stand-in, and the row shows why. `Missing::Pending` is the other
//! case: the photo reader has not got to this file yet. A value that depends on
//! the photo waits rather than guessing, because a name that changes from the
//! modified date to the taken date half a second after it appeared is a name
//! nobody can trust.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use super::facts::{Civil, Facts, Photo, PhotoFacts};

/// What the template field holds when the card opens: every name as it is.
pub const DEFAULT: &str = "{name}{ext}";

/// The format a date value uses when it is not given one. Sorts correctly as
/// text, which is what a date in a filename is usually for.
const DEFAULT_DATE: &str = "YYYY-MM-DD";

/// The ceiling on a compiled `re` pattern. The regex crate's own default is
/// ten times this; a filename pattern needs a sliver of either, and a pattern
/// typed by accident (`\w\{1000\}`) should fail fast rather than stall the
/// keystroke that compiled it.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// A parsed template: the literal text and values in order, plus whatever did
/// not parse.
#[derive(Debug, Clone)]
pub struct Template {
    parts: Vec<Part>,
    problems: Vec<Problem>,
}

/// One run of a template: literal text, or a value to fill in.
#[derive(Debug, Clone)]
pub enum Part {
    Text(String),
    Token(Token),
}

/// One well-formed value, compiled: its arguments are already checked, and a
/// regex is already built, so resolving it for a thousand rows does no parsing.
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Kind,
    /// Char range in the source, from the `{` through the `}`.
    pub span: Range<usize>,
    value: Value,
}

/// What a value is. `N` covers the counter at every width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Name,
    Ext,
    Parent,
    N,
    Date,
    Taken,
    Created,
    Modified,
    Width,
    Height,
    Camera,
}

/// A stretch of the source that did not parse, and why, for the field to
/// underline. The stretch is still in the output, as literal text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Char range in the source.
    pub span: Range<usize>,
    pub message: String,
}

/// Why a template could not be resolved for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// The photo reader has not finished with this file; ask again later.
    Pending,
    /// The file does not have what a value asks for: "no date taken", "no
    /// dimensions", "no camera", "no date".
    Because(&'static str),
}

/// A token's arguments, compiled.
#[derive(Debug, Clone)]
enum Value {
    Text(Vec<Transform>),
    Counter { width: usize, start: i64 },
    Date(Vec<Piece>),
    Dimension,
}

#[derive(Debug, Clone)]
enum Transform {
    Lower,
    Upper,
    Title,
    Slug,
    Trim,
    Replace { from: String, to: String },
    Re { regex: Regex, with: String },
}

/// One piece of a date format: a field, or text to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Field(Field),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Year4,
    Year2,
    MonthName,
    Month2,
    Month1,
    Day2,
    Day1,
    Hour2,
    Hour1,
    Minute2,
    Second2,
}

/// The format letters, longest first within each letter so `YYYY` is never
/// read as `YY` twice. Case matters: `MM` is the month, `mm` the minute.
const FIELDS: [(&str, Field); 11] = [
    ("YYYY", Field::Year4),
    ("YY", Field::Year2),
    ("MMM", Field::MonthName),
    ("MM", Field::Month2),
    ("M", Field::Month1),
    ("DD", Field::Day2),
    ("D", Field::Day1),
    ("HH", Field::Hour2),
    ("H", Field::Hour1),
    ("mm", Field::Minute2),
    ("ss", Field::Second2),
];

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

impl Template {
    /// Parse a template. Never fails: see the module notes on leniency.
    pub fn parse(source: &str) -> Template {
        let chars: Vec<char> = source.chars().collect();
        let mut parts = Vec::new();
        let mut problems = Vec::new();
        let mut text = String::new();
        let mut i = 0;
        while let Some(&c) = chars.get(i) {
            if c != '{' {
                text.push(c);
                i += 1;
                continue;
            }
            if chars.get(i + 1) == Some(&'{') {
                text.push('{');
                i += 2;
                continue;
            }
            match scan(&chars, i) {
                Scan::Closed { end, segments } => {
                    match compile(&segments, i..end) {
                        Ok(token) => {
                            if !text.is_empty() {
                                parts.push(Part::Text(std::mem::take(&mut text)));
                            }
                            parts.push(Part::Token(token));
                        }
                        Err(message) => {
                            text.extend(&chars[i..end]);
                            problems.push(Problem {
                                span: i..end,
                                message,
                            });
                        }
                    }
                    i = end;
                }
                Scan::Unclosed { end } => {
                    text.extend(&chars[i..end]);
                    problems.push(Problem {
                        span: i..end,
                        message: "`{` never closed".to_string(),
                    });
                    i = end;
                }
            }
        }
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        Template { parts, problems }
    }

    /// Everything that did not parse, in source order.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// The template in order, literal text and values alike.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The values, in source order.
    pub fn tokens(&self) -> impl Iterator<Item = &Token> {
        self.parts.iter().filter_map(|part| match part {
            Part::Token(token) => Some(token),
            Part::Text(_) => None,
        })
    }

    /// Whether anything in the template depends on the file. A template with
    /// none gives every row the same name, which the card will want to say.
    pub fn has_tokens(&self) -> bool {
        self.tokens().next().is_some()
    }

    /// The new name for one file.
    ///
    /// `index` is the file's 0-based position among the rows, which drives the
    /// counter. `now` bounds `{date}`'s trust in a taken date; `None` skips the
    /// upper bound.
    ///
    /// When more than one value is missing, a definite answer beats a pending
    /// one: if `{created}` can never resolve, the row has failed whatever the
    /// photo reader says, and saying "pending" would only delay that news.
    pub fn resolve(
        &self,
        facts: &Facts,
        index: usize,
        now: Option<Civil>,
    ) -> Result<String, Missing> {
        let mut out = String::new();
        let mut pending = false;
        for part in &self.parts {
            match part {
                Part::Text(text) => out.push_str(text),
                Part::Token(token) => match token.resolve(facts, index, now) {
                    Ok(value) => out.push_str(&value),
                    Err(Missing::Pending) => pending = true,
                    Err(missing) => return Err(missing),
                },
            }
        }
        if pending {
            Err(Missing::Pending)
        } else {
            Ok(out)
        }
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// How far a `{` reached.
enum Scan {
    /// Closed by the `}` at `end - 1`. The body is split on unescaped `|`, with
    /// the escapes already applied, so the first segment is the kind.
    Closed { end: usize, segments: Vec<String> },
    /// Never closed: the source ran out, or another unescaped `{` began at
    /// `end`. The literal stretch runs from the `{` up to `end`.
    Unclosed { end: usize },
}

fn scan(chars: &[char], open: usize) -> Scan {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut i = open + 1;
    while let Some(&c) = chars.get(i) {
        match c {
            '}' => {
                segments.push(current);
                return Scan::Closed {
                    end: i + 1,
                    segments,
                };
            }
            '|' => segments.push(std::mem::take(&mut current)),
            '{' => return Scan::Unclosed { end: i },
            '\\' => match chars.get(i + 1) {
                Some(&next @ ('|' | '{' | '}' | '\\')) => {
                    current.push(next);
                    i += 1;
                }
                _ => current.push('\\'),
            },
            _ => current.push(c),
        }
        i += 1;
    }
    Scan::Unclosed { end: chars.len() }
}

/// Turn a closed value's segments into a token, or say what is wrong with it.
fn compile(segments: &[String], span: Range<usize>) -> Result<Token, String> {
    let Some((name, args)) = segments.split_first() else {
        return Err("`{}` is empty".to_string());
    };
    let text = |kind: Kind| -> Result<(Kind, Value), String> {
        Ok((kind, Value::Text(transforms(args)?)))
    };
    let date = |kind: Kind| -> Result<(Kind, Value), String> {
        Ok((kind, Value::Date(date_format(name, args)?)))
    };
    let dimension = |kind: Kind| -> Result<(Kind, Value), String> {
        if args.is_empty() {
            Ok((kind, Value::Dimension))
        } else {
            Err(format!("`{name}` takes no arguments"))
        }
    };
    let (kind, value) = match name.as_str() {
        "name" => text(Kind::Name),
        "ext" => text(Kind::Ext),
        "parent" => text(Kind::Parent),
        "camera" => text(Kind::Camera),
        "date" => date(Kind::Date),
        "taken" => date(Kind::Taken),
        "created" => date(Kind::Created),
        "modified" => date(Kind::Modified),
        "width" => dimension(Kind::Width),
        "height" => dimension(Kind::Height),
        "" => Err("`{}` is empty".to_string()),
        n if n.chars().all(|c| c == 'n') => counter(n, args).map(|value| (Kind::N, value)),
        other => Err(format!("unknown value `{other}`")),
    }?;
    Ok(Token { kind, span, value })
}

/// A text value's transform chain. Each name consumes a fixed number of the
/// segments after it, which is how `replace|a|b|lower` reads unambiguously.
fn transforms(args: &[String]) -> Result<Vec<Transform>, String> {
    let mut chain = Vec::new();
    let mut rest = args;
    while let Some((name, tail)) = rest.split_first() {
        let (transform, used) = match name.as_str() {
            "lower" => (Transform::Lower, 0),
            "upper" => (Transform::Upper, 0),
            "title" => (Transform::Title, 0),
            "slug" => (Transform::Slug, 0),
            "trim" => (Transform::Trim, 0),
            "replace" => match tail {
                [from, to, ..] => (
                    Transform::Replace {
                        from: from.clone(),
                        to: to.clone(),
                    },
                    2,
                ),
                _ => return Err("`replace` needs two arguments".to_string()),
            },
            "re" => match tail {
                [pattern, with, ..] => (
                    Transform::Re {
                        regex: compile_regex(pattern)?,
                        with: with.clone(),
                    },
                    2,
                ),
                _ => return Err("`re` needs two arguments".to_string()),
            },
            "" => return Err("a `|` with no transform after it".to_string()),
            other => return Err(format!("unknown transform `{other}`")),
        };
        chain.push(transform);
        rest = tail.get(used..).unwrap_or_default();
    }
    Ok(chain)
}

/// Build a regex, or the last line of the regex crate's message: its full
/// report is several lines with a caret diagram, and the field has room for
/// one.
fn compile_regex(pattern: &str) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|error| {
            let report = error.to_string();
            let last = report
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("invalid pattern");
            format!("`re`: {}", last.strip_prefix("error: ").unwrap_or(last))
        })
}

fn counter(name: &str, args: &[String]) -> Result<Value, String> {
    // `name` is all `n`s, so its byte length is its width.
    let width = name.len();
    let start = match args {
        [] => 1,
        [start] => start
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("`{name}` starts at a whole number, not `{start}`"))?,
        _ => return Err(format!("`{name}` takes one argument, where it starts")),
    };
    Ok(Value::Counter { width, start })
}

fn date_format(name: &str, args: &[String]) -> Result<Vec<Piece>, String> {
    match args {
        [] => Ok(pieces(DEFAULT_DATE)),
        [format] => Ok(pieces(format)),
        _ => Err(format!(
            "`{name}` takes one argument, a format like `YYYY-MM-DD`"
        )),
    }
}

/// Split a date format into fields and literal text. Anything that is not a
/// format letter run is copied through, so `YYYY-MM-DD HH.mm` needs no quoting.
fn pieces(format: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut rest = format;
    while let Some(c) = rest.chars().next() {
        if let Some(&(letters, field)) =
            FIELDS.iter().find(|(letters, _)| rest.starts_with(letters))
        {
            out.push(Piece::Field(field));
            rest = &rest[letters.len()..];
            continue;
        }
        match out.last_mut() {
            Some(Piece::Text(text)) => text.push(c),
            _ => out.push(Piece::Text(c.to_string())),
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

// ── Resolving ───────────────────────────────────────────────────────────────

impl Token {
    fn resolve(&self, facts: &Facts, index: usize, now: Option<Civil>) -> Result<String, Missing> {
        match &self.value {
            Value::Text(chain) => {
                let base = match self.kind {
                    Kind::Ext => facts.ext().to_string(),
                    Kind::Parent => facts.parent.clone(),
                    Kind::Camera => camera(&facts.photo)?,
                    _ => facts.stem().to_string(),
                };
                Ok(chain
                    .iter()
                    .fold(base, |text, transform| transform.apply(text)))
            }
            Value::Counter { width, start } => {
                let n = i64::try_from(index)
                    .unwrap_or(i64::MAX)
                    .saturating_add(*start);
                Ok(format!("{n:0width$}", width = *width))
            }
            Value::Date(pieces) => Ok(render_date(pieces, date_of(self.kind, facts, now)?)),
            Value::Dimension => {
                let photo = photo(&facts.photo, "no dimensions")?;
                let size = match self.kind {
                    Kind::Height => photo.height,
                    _ => photo.width,
                };
                size.map(|v| v.to_string())
                    .ok_or(Missing::Because("no dimensions"))
            }
        }
    }
}

/// The photo's facts, or why there are none: still being read, or not a photo.
fn photo<'a>(photo: &'a Photo, none: &'static str) -> Result<&'a PhotoFacts, Missing> {
    match photo {
        Photo::Pending => Err(Missing::Pending),
        Photo::None => Err(Missing::Because(none)),
        Photo::Some(facts) => Ok(facts),
    }
}

/// Make and model, without saying the make twice. Canon writes `Canon` and
/// `Canon EOS R5`; Sony writes `SONY` and `ILCE-7M3`. The first reads best as
/// the model alone, the second needs both.
///
/// Only the make's first word is compared, because the make is often the
/// company's legal name: Nikon writes `NIKON CORPORATION` and `NIKON Z 6`,
/// and Ricoh `RICOH IMAGING COMPANY, LTD.` and `RICOH GR III`. The model
/// already names the brand, so the model alone is the camera.
fn camera(photo_facts: &Photo) -> Result<String, Missing> {
    let facts = photo(photo_facts, "no camera")?;
    let make = facts
        .make
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let model = facts
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let brand = make
        .and_then(|make| make.split_whitespace().next())
        .map(str::to_lowercase);
    match (make, model) {
        (Some(_), Some(model))
            if brand.is_some_and(|brand| model.to_lowercase().starts_with(&brand)) =>
        {
            Ok(model.to_string())
        }
        (Some(make), Some(model)) => Ok(format!("{make} {model}")),
        (Some(one), None) | (None, Some(one)) => Ok(one.to_string()),
        (None, None) => Err(Missing::Because("no camera")),
    }
}

/// The date a date value names.
fn date_of(kind: Kind, facts: &Facts, now: Option<Civil>) -> Result<Civil, Missing> {
    match kind {
        Kind::Taken => photo(&facts.photo, "no date taken")?
            .taken
            .ok_or(Missing::Because("no date taken")),
        Kind::Created => facts.created.ok_or(Missing::Because("no date created")),
        Kind::Modified => facts.modified.ok_or(Missing::Because("no date modified")),
        _ => best_date(facts, now),
    }
}

/// `{date}`: the date a person would say the file is *from*.
///
/// A believable taken date first, because it is the one fact about a photo
/// that survives copying. Believable means 1980 or later, which rules out a
/// zeroed clock read as the Unix epoch, and no later than next year, which
/// rules out a clock set wildly forward. A camera that reset to its own
/// factory date (often 2000 or so) still gets through; nothing in the file can
/// tell that apart from a real photo taken that year. Then the earlier
/// of created and modified: a copy gets a new created date and keeps its
/// modified one, and an edit does the reverse, so whichever is earlier is the
/// closer to when the thing was made.
///
/// While the photo reader is still on this file, this waits instead of
/// answering with a file date it might have to replace a moment later.
fn best_date(facts: &Facts, now: Option<Civil>) -> Result<Civil, Missing> {
    let taken = match &facts.photo {
        Photo::Pending => return Err(Missing::Pending),
        Photo::None => None,
        Photo::Some(photo) => photo.taken.filter(|t| believable(t, now)),
    };
    let file = match (facts.created, facts.modified) {
        (Some(created), Some(modified)) => Some(created.min(modified)),
        (created, modified) => created.or(modified),
    };
    taken.or(file).ok_or(Missing::Because("no date"))
}

fn believable(taken: &Civil, now: Option<Civil>) -> bool {
    taken.year >= 1980 && now.is_none_or(|now| taken.year <= now.year.saturating_add(1))
}

fn render_date(pieces: &[Piece], at: Civil) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Field(field) => {
                let text = match field {
                    Field::Year4 => format!("{:04}", at.year),
                    Field::Year2 => format!("{:02}", at.year.rem_euclid(100)),
                    Field::MonthName => at
                        .month
                        .checked_sub(1)
                        .and_then(|m| MONTHS.get(m as usize))
                        .map_or_else(|| at.month.to_string(), |name| (*name).to_string()),
                    Field::Month2 => format!("{:02}", at.month),
                    Field::Month1 => at.month.to_string(),
                    Field::Day2 => format!("{:02}", at.day),
                    Field::Day1 => at.day.to_string(),
                    Field::Hour2 => format!("{:02}", at.hour),
                    Field::Hour1 => at.hour.to_string(),
                    Field::Minute2 => format!("{:02}", at.minute),
                    Field::Second2 => format!("{:02}", at.second),
                };
                out.push_str(&text);
            }
        }
    }
    out
}

impl Transform {
    fn apply(&self, text: String) -> String {
        match self {
            Transform::Lower => text.to_lowercase(),
            Transform::Upper => text.to_uppercase(),
            Transform::Title => title_case(&text),
            Transform::Slug => slug(&text),
            Transform::Trim => text.trim().to_string(),
            // Replacing "nothing" would mean inserting `to` between every
            // character, which no one asking for a plain replace means.
            Transform::Replace { from, .. } if from.is_empty() => text,
            Transform::Replace { from, to } => text.replace(from.as_str(), to),
            Transform::Re { regex, with } => regex.replace_all(&text, with.as_str()).into_owned(),
        }
    }
}

/// First letter of each word up, the rest down. Words are split on the three
/// characters filenames use for spaces: space, `_` and `-`, which are kept.
fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word_start = true;
    for c in text.chars() {
        if matches!(c, ' ' | '_' | '-') {
            out.push(c);
            word_start = true;
        } else if word_start {
            out.extend(c.to_uppercase());
            word_start = false;
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Lowercase, every run of anything that is not a letter or digit collapsed to
/// one `-`, and no `-` at either end. Letters are Unicode letters, so `Café`
/// stays `café` rather than losing its accent.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fixtures ────────────────────────────────────────────────────────────

    fn civil(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Civil {
        Civil {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    /// Late September 2026, for `{date}`'s "no later than next year".
    const NOW: Option<Civil> = Some(Civil {
        year: 2026,
        month: 9,
        day: 24,
        hour: 12,
        minute: 0,
        second: 0,
    });

    /// A camera file that has been copied once: taken in May, copied (created)
    /// on 2 June, last modified on 1 June.
    fn photo() -> Facts {
        Facts {
            name: "IMG_0042.JPG".to_string(),
            is_dir: false,
            size: 3_000_000,
            modified: Some(civil(2024, 6, 1, 9, 30, 0)),
            created: Some(civil(2024, 6, 2, 8, 0, 0)),
            parent: "Holiday 2024".to_string(),
            photo: Photo::Some(PhotoFacts {
                taken: Some(civil(2024, 5, 6, 14, 3, 22)),
                width: Some(6000),
                height: Some(4000),
                make: Some("Canon".to_string()),
                model: Some("Canon EOS R5".to_string()),
            }),
        }
    }

    fn file(name: &str) -> Facts {
        Facts {
            name: name.to_string(),
            is_dir: false,
            size: 0,
            modified: None,
            created: None,
            parent: "docs".to_string(),
            photo: Photo::None,
        }
    }

    fn with_photo(facts: PhotoFacts) -> Facts {
        Facts {
            photo: Photo::Some(facts),
            ..photo()
        }
    }

    /// Resolve a template that must parse cleanly.
    fn render_at(source: &str, facts: &Facts, index: usize) -> String {
        let template = Template::parse(source);
        assert_eq!(template.problems(), &[], "{source:?}");
        template
            .resolve(facts, index, NOW)
            .unwrap_or_else(|missing| panic!("{source:?}: {missing:?}"))
    }

    fn render(source: &str, facts: &Facts) -> String {
        render_at(source, facts, 0)
    }

    fn missing(source: &str, facts: &Facts) -> Missing {
        match Template::parse(source).resolve(facts, 0, NOW) {
            Ok(name) => panic!("{source:?} resolved to {name:?}"),
            Err(missing) => missing,
        }
    }

    // ── The default ─────────────────────────────────────────────────────────

    #[test]
    fn the_default_template_is_the_identity_for_files_dotfiles_and_folders() {
        let mut names: Vec<String> = [
            "photo.jpg",
            "a.tar.gz",
            "Makefile",
            ".bashrc",
            ".config.toml",
            "trailing.",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        names.extend(crate::test_support::gnarly_names());
        for name in &names {
            assert_eq!(&render(DEFAULT, &file(name)), name);
        }
        for name in ["v1.2", ".git", "plain", "photos.old"] {
            let dir = Facts {
                is_dir: true,
                ..file(name)
            };
            assert_eq!(render(DEFAULT, &dir), name);
        }
    }

    #[test]
    fn ext_carries_its_dot_and_is_empty_without_one() {
        assert_eq!(render("{ext}", &photo()), ".JPG");
        assert_eq!(render("{ext}", &file("Makefile")), "");
        assert_eq!(render("{ext}", &file(".bashrc")), "");
        assert_eq!(render("{name}", &file("a.tar.gz")), "a.tar");
    }

    // ── Every kind ──────────────────────────────────────────────────────────

    #[test]
    fn every_kind_resolves() {
        let cases = [
            ("{name}", Kind::Name, "IMG_0042"),
            ("{ext}", Kind::Ext, ".JPG"),
            ("{parent}", Kind::Parent, "Holiday 2024"),
            ("{n}", Kind::N, "1"),
            ("{date}", Kind::Date, "2024-05-06"),
            ("{taken}", Kind::Taken, "2024-05-06"),
            ("{created}", Kind::Created, "2024-06-02"),
            ("{modified}", Kind::Modified, "2024-06-01"),
            ("{width}", Kind::Width, "6000"),
            ("{height}", Kind::Height, "4000"),
            ("{camera}", Kind::Camera, "Canon EOS R5"),
        ];
        for (source, kind, expected) in cases {
            let template = Template::parse(source);
            let kinds: Vec<Kind> = template.tokens().map(|t| t.kind).collect();
            assert_eq!(kinds, [kind], "{source}");
            assert_eq!(render(source, &photo()), expected, "{source}");
        }
    }

    #[test]
    fn a_whole_template_mixes_text_and_values() {
        assert_eq!(
            render("{date}_{camera}_{nnn}{ext}", &photo()),
            "2024-05-06_Canon EOS R5_001.JPG"
        );
    }

    // ── The counter ─────────────────────────────────────────────────────────

    #[test]
    fn the_counter_is_one_based_and_padded_to_its_width() {
        assert_eq!(render_at("{n}", &photo(), 0), "1");
        assert_eq!(render_at("{n}", &photo(), 9), "10");
        assert_eq!(render_at("{nnn}", &photo(), 0), "001");
        assert_eq!(render_at("{nnn}", &photo(), 41), "042");
        // Past its width it grows rather than wrapping or truncating.
        assert_eq!(render_at("{nnn}", &photo(), 999), "1000");
    }

    #[test]
    fn the_counter_starts_where_it_is_told() {
        assert_eq!(render_at("{nn|10}", &photo(), 0), "10");
        assert_eq!(render_at("{nn|10}", &photo(), 5), "15");
        assert_eq!(render_at("{n|0}", &photo(), 0), "0");
        assert_eq!(render_at("{nnnn|007}", &photo(), 1), "0008");
    }

    #[test]
    fn a_counter_start_that_is_not_a_whole_number_is_a_problem() {
        for source in ["{n|one}", "{n|1.5}", "{nn|}", "{n|1|2}"] {
            let template = Template::parse(source);
            assert_eq!(template.problems().len(), 1, "{source}");
            assert!(!template.has_tokens(), "{source}");
        }
    }

    // ── Dates ───────────────────────────────────────────────────────────────

    #[test]
    fn date_formats_mix_fields_and_literal_text() {
        let cases = [
            ("{taken|YYYY-MM-DD HH.mm}", "2024-05-06 14.03"),
            ("{taken|YY}", "24"),
            ("{taken|YYMMDD}", "240506"),
            ("{taken|MMM D, YYYY}", "May 6, 2024"),
            ("{taken|D/M H:mm:ss}", "6/5 14:03:22"),
            ("{taken|YYYYMMDD_HHmmss}", "20240506_140322"),
            ("{taken|[YYYY] at HH}", "[2024] at 14"),
            ("{taken|MMMM}", "May5"),
            ("{created|MMM}", "Jun"),
            ("{modified|DD.MM.YYYY}", "01.06.2024"),
            ("{date|}", ""),
        ];
        for (source, expected) in cases {
            assert_eq!(render(source, &photo()), expected, "{source}");
        }
    }

    #[test]
    fn short_fields_do_not_pad_and_short_years_do() {
        let early = with_photo(PhotoFacts {
            taken: Some(civil(2005, 1, 2, 3, 4, 5)),
            ..PhotoFacts::default()
        });
        assert_eq!(render("{taken|YY M D H}", &early), "05 1 2 3");
        assert_eq!(render("{taken|MM DD HH mm ss}", &early), "01 02 03 04 05");
    }

    #[test]
    fn date_prefers_a_believable_taken_date() {
        assert_eq!(render("{date}", &photo()), "2024-05-06");
    }

    #[test]
    fn date_distrusts_a_taken_date_before_1980_or_after_next_year() {
        let at = |year| {
            with_photo(PhotoFacts {
                taken: Some(civil(year, 1, 1, 0, 0, 0)),
                ..PhotoFacts::default()
            })
        };
        // 1 June (modified) is earlier than 2 June (created).
        assert_eq!(render("{date}", &at(1970)), "2024-06-01");
        assert_eq!(render("{date}", &at(1979)), "2024-06-01");
        assert_eq!(render("{date}", &at(1980)), "1980-01-01");
        assert_eq!(render("{date}", &at(2027)), "2027-01-01");
        assert_eq!(render("{date}", &at(2028)), "2024-06-01");
        // `{taken}` asked for exactly that and gets it, believable or not.
        assert_eq!(render("{taken}", &at(1970)), "1970-01-01");
        // Without a clock there is no upper bound.
        let far = Template::parse("{date}").resolve(&at(2090), 0, None);
        assert_eq!(far, Ok("2090-01-01".to_string()));
    }

    #[test]
    fn date_without_a_taken_date_is_the_earlier_file_date() {
        let mut facts = file("notes.txt");
        facts.created = Some(civil(2023, 3, 1, 0, 0, 0));
        facts.modified = Some(civil(2024, 1, 1, 0, 0, 0));
        assert_eq!(render("{date}", &facts), "2023-03-01");
        facts.created = Some(civil(2025, 3, 1, 0, 0, 0));
        assert_eq!(render("{date}", &facts), "2024-01-01");
        facts.created = None;
        assert_eq!(render("{date}", &facts), "2024-01-01");
        facts.created = Some(civil(2022, 2, 2, 0, 0, 0));
        facts.modified = None;
        assert_eq!(render("{date}", &facts), "2022-02-02");
        facts.created = None;
        assert_eq!(missing("{date}", &facts), Missing::Because("no date"));
    }

    #[test]
    fn date_compares_file_dates_to_the_second() {
        let mut facts = file("a.txt");
        facts.created = Some(civil(2024, 1, 1, 10, 0, 1));
        facts.modified = Some(civil(2024, 1, 1, 10, 0, 0));
        assert_eq!(render("{date|HH:mm:ss}", &facts), "10:00:00");
    }

    // ── Waiting and missing ─────────────────────────────────────────────────

    #[test]
    fn photo_values_wait_while_the_photo_is_being_read() {
        let pending = Facts {
            photo: Photo::Pending,
            ..photo()
        };
        for source in [
            "{date}",
            "{taken}",
            "{width}",
            "{height}",
            "{camera}",
            "{name}-{date}",
        ] {
            assert_eq!(missing(source, &pending), Missing::Pending, "{source}");
        }
        // Values that do not need the photo do not wait for it.
        assert_eq!(render("{name}-{created}", &pending), "IMG_0042-2024-06-02");
    }

    #[test]
    fn missing_values_say_what_is_missing() {
        let not_a_photo = file("notes.txt");
        let says_nothing = Facts {
            photo: Photo::Some(PhotoFacts::default()),
            ..file("blank.jpg")
        };
        for facts in [&not_a_photo, &says_nothing] {
            assert_eq!(missing("{taken}", facts), Missing::Because("no date taken"));
            assert_eq!(missing("{width}", facts), Missing::Because("no dimensions"));
            assert_eq!(
                missing("{height}", facts),
                Missing::Because("no dimensions")
            );
            assert_eq!(missing("{camera}", facts), Missing::Because("no camera"));
            assert_eq!(
                missing("{created}", facts),
                Missing::Because("no date created")
            );
            assert_eq!(
                missing("{modified}", facts),
                Missing::Because("no date modified")
            );
            assert_eq!(missing("{date}", facts), Missing::Because("no date"));
        }
    }

    #[test]
    fn a_settled_failure_outranks_a_pending_one() {
        let facts = Facts {
            photo: Photo::Pending,
            ..file("IMG_1.JPG")
        };
        assert_eq!(
            missing("{taken}-{created}", &facts),
            Missing::Because("no date created")
        );
        assert_eq!(
            missing("{created}-{taken}", &facts),
            Missing::Because("no date created")
        );
    }

    #[test]
    fn camera_says_the_make_once() {
        let camera = |make: Option<&str>, model: Option<&str>| {
            let facts = with_photo(PhotoFacts {
                make: make.map(String::from),
                model: model.map(String::from),
                ..PhotoFacts::default()
            });
            Template::parse("{camera}").resolve(&facts, 0, NOW)
        };
        let ok = |s: &str| Ok(s.to_string());
        assert_eq!(
            camera(Some("Canon"), Some("Canon EOS R5")),
            ok("Canon EOS R5")
        );
        assert_eq!(
            camera(Some("canon"), Some("Canon EOS R5")),
            ok("Canon EOS R5")
        );
        assert_eq!(
            camera(Some("NIKON CORPORATION"), Some("NIKON Z 6")),
            ok("NIKON Z 6")
        );
        assert_eq!(
            camera(Some("RICOH IMAGING COMPANY, LTD."), Some("ricoh GR III")),
            ok("ricoh GR III")
        );
        assert_eq!(camera(Some("SONY"), Some("ILCE-7M3")), ok("SONY ILCE-7M3"));
        assert_eq!(
            camera(Some("OLYMPUS IMAGING CORP."), Some("E-M5")),
            ok("OLYMPUS IMAGING CORP. E-M5")
        );
        assert_eq!(camera(Some("Apple"), None), ok("Apple"));
        assert_eq!(camera(None, Some("X100V")), ok("X100V"));
        assert_eq!(camera(None, None), Err(Missing::Because("no camera")));
    }

    // ── Transforms ──────────────────────────────────────────────────────────

    #[test]
    fn every_transform_does_what_it_says() {
        let mut facts = photo();
        facts.parent = "  hOLIDAY in-the_sun, 2024!  ".to_string();
        let cases = [
            ("{name|lower}", "img_0042"),
            ("{name|upper}", "IMG_0042"),
            ("{ext|lower}", ".jpg"),
            ("{parent|trim}", "hOLIDAY in-the_sun, 2024!"),
            ("{parent|trim|title}", "Holiday In-The_Sun, 2024!"),
            ("{parent|slug}", "holiday-in-the-sun-2024"),
            ("{parent|upper|trim}", "HOLIDAY IN-THE_SUN, 2024!"),
            ("{name|replace|_|-}", "IMG-0042"),
            ("{name|replace|0|o}", "IMG_oo42"),
            ("{camera|replace|Canon |}", "EOS R5"),
            ("{camera|slug}", "canon-eos-r5"),
        ];
        for (source, expected) in cases {
            assert_eq!(render(source, &facts), expected, "{source}");
        }
    }

    #[test]
    fn transforms_chain_left_to_right() {
        assert_eq!(
            render("{name|lower|replace|img_|photo-}", &photo()),
            "photo-0042"
        );
        assert_eq!(
            render("{name|replace|IMG_|photo-|upper}", &photo()),
            "PHOTO-0042"
        );
    }

    #[test]
    fn slug_and_title_handle_unicode() {
        let facts = file("Crème Brûlée — Été.txt");
        assert_eq!(render("{name|slug}", &facts), "crème-brûlée-été");
        assert_eq!(render("{name|lower|title}", &facts), "Crème Brûlée — Été");
    }

    #[test]
    fn replace_with_nothing_to_find_changes_nothing() {
        assert_eq!(render("{name|replace||x}", &photo()), "IMG_0042");
    }

    #[test]
    fn re_replaces_every_match_and_fills_in_groups() {
        assert_eq!(render(r"{name|re|\d|#}", &photo()), "IMG_####");
        assert_eq!(
            render(r"{name|re|IMG_(\d+)|photo $1}", &photo()),
            "photo 0042"
        );
        assert_eq!(
            render(r"{name|re|(?<num>\d+)|#$\{num\}}", &photo()),
            "IMG_#0042"
        );
        assert_eq!(render(r"{name|re|(?i)img|pic}", &photo()), "pic_0042");
    }

    /// A brace in an argument is escaped at both ends, so a regex's counted
    /// repetition reaches the regex intact.
    #[test]
    fn an_escaped_brace_reaches_the_argument() {
        assert_eq!(render(r"{name|re|0\{2\}|x}", &photo()), "IMG_x42");
        assert_eq!(render(r"{name|replace|_|\{}", &photo()), "IMG{0042");
        // A regex that wants a literal brace escapes the backslash and the
        // brace: `\\` gives the regex its `\`, `\{` its `{`.
        assert_eq!(
            render(r"{name|replace|_|\{|re|\\\{|(}", &photo()),
            "IMG(0042"
        );
    }

    // ── Escapes ─────────────────────────────────────────────────────────────

    #[test]
    fn escapes_make_the_syntax_characters_literal() {
        let facts = photo();
        assert_eq!(render("{{name}", &facts), "{name}");
        assert!(!Template::parse("{{name}").has_tokens());
        assert_eq!(render("a}b", &facts), "a}b");
        assert_eq!(render("{{{name}}", &facts), "{IMG_0042}");
        assert_eq!(render(r"{name|replace|_|\|}", &facts), "IMG|0042");
        assert_eq!(render(r"{name|replace|0042|\}}", &facts), "IMG_}");
        assert_eq!(render(r"{name|replace|0042|\{}", &facts), "IMG_{");
        assert_eq!(render(r"{name|replace|_|\\}", &facts), r"IMG\0042");
        // Outside a value a backslash is only a backslash.
        assert_eq!(render(r"a\|b", &facts), r"a\|b");
    }

    // ── Leniency ────────────────────────────────────────────────────────────

    #[test]
    fn a_half_typed_value_stays_literal_and_is_reported() {
        let template = Template::parse("{name}-{da");
        assert_eq!(
            template.problems(),
            &[Problem {
                span: 7..10,
                message: "`{` never closed".to_string()
            }]
        );
        assert_eq!(
            template.resolve(&photo(), 0, NOW),
            Ok("IMG_0042-{da".to_string())
        );
    }

    /// Typing a new value in front of an existing one must not swallow it.
    #[test]
    fn a_half_typed_value_leaves_the_next_one_working() {
        let template = Template::parse("{name}-{da{ext}");
        assert_eq!(template.problems().len(), 1);
        assert_eq!(template.problems()[0].span, 7..10);
        assert_eq!(template.tokens().count(), 2);
        assert_eq!(
            template.resolve(&photo(), 0, NOW),
            Ok("IMG_0042-{da.JPG".to_string())
        );
    }

    /// The popover's common case: an argument typed in the middle of a
    /// template. The `{` of the value after it ends the half-typed one, and
    /// that value keeps working.
    #[test]
    fn a_half_typed_argument_leaves_the_next_value_working() {
        let template = Template::parse("{name}-{date|YYYY{ext}");
        assert_eq!(
            template.problems(),
            &[Problem {
                span: 7..17,
                message: "`{` never closed".to_string()
            }]
        );
        let kinds: Vec<Kind> = template.tokens().map(|t| t.kind).collect();
        assert_eq!(kinds, [Kind::Name, Kind::Ext]);
        assert_eq!(
            template.resolve(&photo(), 0, NOW),
            Ok("IMG_0042-{date|YYYY.JPG".to_string())
        );
        // Mid-chain, and mid-way through a transform's arguments, alike.
        for source in ["{name|lower{ext}", "{name|replace|a{ext}"] {
            let template = Template::parse(source);
            assert_eq!(template.problems().len(), 1, "{source}");
            assert_eq!(template.tokens().count(), 1, "{source}");
        }
        // An unescaped counted repetition is two problems, not a regex: the
        // `re` is never closed, and `{2}` is a value nobody has heard of.
        let messages: Vec<String> = Template::parse("{name|re|0{2}|x}")
            .problems()
            .iter()
            .map(|p| p.message.clone())
            .collect();
        assert_eq!(messages, ["`{` never closed", "unknown value `2`"]);
    }

    #[test]
    fn an_unclosed_value_at_the_end_is_reported_to_the_end() {
        let template = Template::parse("x{name|lower");
        assert_eq!(template.problems()[0].span, 1..12);
        assert_eq!(
            template.resolve(&photo(), 0, NOW),
            Ok("x{name|lower".to_string())
        );
        assert_eq!(Template::parse("{").problems()[0].span, 0..1);
    }

    #[test]
    fn unknown_values_and_wrong_arguments_are_reported_and_kept_literal() {
        let cases = [
            ("{dat}", "unknown value `dat`"),
            ("{Name}", "unknown value `Name`"),
            ("{}", "`{}` is empty"),
            ("{name|shout}", "unknown transform `shout`"),
            ("{name|}", "a `|` with no transform after it"),
            ("{name|replace|x}", "`replace` needs two arguments"),
            ("{name|lower|re|x}", "`re` needs two arguments"),
            ("{width|x}", "`width` takes no arguments"),
            (
                "{date|YYYY|x}",
                "`date` takes one argument, a format like `YYYY-MM-DD`",
            ),
            ("{n|x}", "`n` starts at a whole number, not `x`"),
            ("{nn|1|2}", "`nn` takes one argument, where it starts"),
        ];
        for (source, message) in cases {
            let template = Template::parse(source);
            let chars = source.chars().count();
            assert_eq!(
                template.problems(),
                &[Problem {
                    span: 0..chars,
                    message: message.to_string()
                }],
                "{source}"
            );
            assert_eq!(template.resolve(&photo(), 0, NOW), Ok(source.to_string()));
        }
    }

    #[test]
    fn a_regex_that_does_not_compile_is_reported_in_one_line() {
        let template = Template::parse("{name|re|(|x}");
        let problem = &template.problems()[0];
        assert!(problem.message.starts_with("`re`: "), "{problem:?}");
        assert!(!problem.message.contains('\n'), "{problem:?}");
        assert_eq!(problem.span, 0..13);
    }

    #[test]
    fn a_problem_does_not_stop_the_rest_of_the_template() {
        let template = Template::parse("{nam}_{n}{ext}");
        assert_eq!(template.problems().len(), 1);
        assert_eq!(
            template.resolve(&photo(), 2, NOW),
            Ok("{nam}_3.JPG".to_string())
        );
    }

    // ── Spans ───────────────────────────────────────────────────────────────

    #[test]
    fn spans_are_char_indices() {
        let template = Template::parse("日本-{nam}{ext}");
        assert_eq!(template.problems()[0].span, 3..8);
        let spans: Vec<Range<usize>> = template.tokens().map(|t| t.span.clone()).collect();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], 8..13);

        let template = Template::parse("é{name}ü{date|D. MMM}");
        let spans: Vec<Range<usize>> = template.tokens().map(|t| t.span.clone()).collect();
        assert_eq!(spans, [1..7, 8..21]);
    }

    #[test]
    fn has_tokens_is_whether_anything_depends_on_the_file() {
        assert!(!Template::parse("").has_tokens());
        assert!(!Template::parse("plain text").has_tokens());
        assert!(!Template::parse("{dat}").has_tokens());
        assert!(Template::parse("{n}").has_tokens());
        assert_eq!(render("same", &photo()), "same");
    }

    #[test]
    fn parts_keep_text_and_values_in_order() {
        let template = Template::parse("a{name}b{{c");
        let shape: Vec<String> = template
            .parts()
            .iter()
            .map(|part| match part {
                Part::Text(text) => format!("text {text}"),
                Part::Token(token) => format!("token {:?}", token.kind),
            })
            .collect();
        assert_eq!(shape, ["text a", "token Name", "text b{c"]);
    }
}
