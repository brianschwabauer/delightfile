//! The catalogue behind the template field's `{` popover.
//!
//! Nobody should have to read documentation to find out that `{taken}` exists.
//! Typing `{` in the template field opens a list of every value, and each
//! keystroke after it narrows the list. This module is that list and its
//! filter, with no idea where the popover is drawn or how a pick is inserted.
//!
//! # Two stages
//!
//! Until a `|` is typed, the query is a value's name, and the list is the
//! whole catalogue: every kind once, plus the handful of variants that are
//! worth knowing about without asking (a padded counter, three date formats,
//! the transforms on `{name}`). A candidate whose kind starts with the query
//! comes first; after it, anything the query is a subsequence of, so `slug`
//! still finds `{name|slug}`.
//!
//! After a `|`, the kind is settled and the query is its argument. A date
//! offers its formats, a text value offers the next transform in its chain,
//! and the counter explains its one argument. A position where the next thing
//! is free text — the two arguments of `replace`, say — offers nothing, because
//! there is nothing to offer.
//!
//! Every candidate is a whole value, braces included, so what the popover
//! inserts is always something [`super::template::Template::parse`] accepts.

/// One line in the popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The full token to insert, e.g. "{date|YYYY-MM}".
    pub insert: String,
    /// One short line of what it is, e.g. "year and month".
    pub detail: &'static str,
}

/// The catalogue before the padded counter is slotted in after `{n}`, in the
/// order the popover shows it.
const CATALOGUE: [(&str, &str); 20] = [
    ("name", "name without its extension"),
    ("ext", "extension, with its dot"),
    ("n", "counter: 1, 2, 3…"),
    ("date", "taken date, else the earliest file date"),
    ("date|YYYY-MM-DD HH.mm", "date and time"),
    ("date|YYYY-MM", "year and month"),
    ("date|YYYY", "year"),
    ("taken", "when the photo was taken"),
    ("created", "when the file was created"),
    ("modified", "when the file was last changed"),
    ("parent", "name of the folder it is in"),
    ("width", "photo width in pixels"),
    ("height", "photo height in pixels"),
    ("camera", "camera make and model"),
    ("name|lower", "name in lowercase"),
    ("name|upper", "name in uppercase"),
    ("name|title", "name in Title Case"),
    ("name|slug", "name as lowercase-with-dashes"),
    ("name|replace|from|to", "name with text replaced"),
    ("name|re|pattern|replacement", "name with a regex replaced"),
];

/// The formats a date value offers once its `|` is typed.
const DATE_FORMATS: [(&str, &str); 7] = [
    ("YYYY-MM-DD", "year-month-day"),
    ("YYYY-MM-DD HH.mm", "date and time"),
    ("YYYY-MM-DD HH.mm.ss", "date and time to the second"),
    ("YYYY-MM", "year and month"),
    ("YYYY", "year"),
    ("YY-MM-DD", "two-digit year"),
    ("YYYYMMDD", "date without separators"),
];

/// The transforms a text value offers after a `|`, with their arguments
/// spelled as placeholders. The name is the first segment.
const TRANSFORMS: [(&str, &str); 7] = [
    ("lower", "lowercase"),
    ("upper", "UPPERCASE"),
    ("title", "Title Case"),
    ("slug", "lowercase-with-dashes"),
    ("trim", "without surrounding spaces"),
    ("replace|from|to", "replace text"),
    ("re|pattern|replacement", "replace a regex match"),
];

const DATE_KINDS: [&str; 4] = ["date", "taken", "created", "modified"];
const TEXT_KINDS: [&str; 4] = ["name", "ext", "parent", "camera"];

/// What the popover should list for `typed`, the text between the `{` and the
/// caret. `rows` is how many files the card holds, so the padded counter can
/// be offered at the width that fits all of them (120 rows is `{nnn}`).
pub fn candidates(typed: &str, rows: usize) -> Vec<Candidate> {
    match typed.split_once('|') {
        None => catalogue(typed, rows),
        Some((kind, after)) => arguments(&kind.to_lowercase(), after),
    }
}

/// Stage 1: the query is a kind.
fn catalogue(typed: &str, rows: usize) -> Vec<Candidate> {
    let mut all: Vec<(String, &'static str)> = Vec::with_capacity(CATALOGUE.len() + 1);
    for (inner, detail) in CATALOGUE {
        all.push((inner.to_string(), detail));
        if inner == "n" && rows >= 10 {
            let width = rows
                .checked_ilog10()
                .map_or(1, |digits| digits as usize + 1);
            all.push(("n".repeat(width), "counter, zero-padded to fit every row"));
        }
    }
    let entries = all
        .into_iter()
        .map(|(inner, detail)| Entry {
            name: inner.split('|').next().unwrap_or_default().to_lowercase(),
            text: inner.to_lowercase(),
            inner,
            detail,
        })
        .collect();
    ranked(entries, &typed.to_lowercase())
}

/// Stage 2: the kind is settled; `after` is everything past its first `|`.
fn arguments(kind: &str, after: &str) -> Vec<Candidate> {
    if DATE_KINDS.contains(&kind) {
        // A date takes one argument, so a second `|` has nothing to offer.
        if after.contains('|') {
            return Vec::new();
        }
        let entries = DATE_FORMATS
            .iter()
            .map(|&(format, detail)| Entry {
                inner: format!("{kind}|{format}"),
                detail,
                name: format.to_lowercase(),
                text: format.to_lowercase(),
            })
            .collect();
        return ranked(entries, &after.to_lowercase());
    }
    if TEXT_KINDS.contains(&kind) {
        return transforms(kind, after);
    }
    if !kind.is_empty() && kind.chars().all(|c| c == 'n') {
        return counter(kind, after);
    }
    Vec::new()
}

/// The next transform in a text value's chain, if the caret is where a
/// transform's name goes rather than inside one's arguments.
fn transforms(kind: &str, after: &str) -> Vec<Candidate> {
    let mut segments: Vec<&str> = after.split('|').collect();
    let query = segments.pop().unwrap_or_default().to_lowercase();
    let mut i = 0;
    while let Some(name) = segments.get(i) {
        let Some(arity) = arity(name) else {
            return Vec::new();
        };
        i += 1 + arity;
    }
    if i != segments.len() {
        return Vec::new();
    }
    let chain: String = segments.iter().map(|s| format!("|{s}")).collect();
    let entries = TRANSFORMS
        .iter()
        .map(|&(transform, detail)| {
            let name = transform.split('|').next().unwrap_or_default().to_string();
            Entry {
                inner: format!("{kind}{chain}|{transform}"),
                detail,
                text: name.clone(),
                name,
            }
        })
        .collect();
    ranked(entries, &query)
}

/// How many arguments a transform takes, or `None` if it is not one.
fn arity(name: &str) -> Option<usize> {
    match name {
        "lower" | "upper" | "title" | "slug" | "trim" => Some(0),
        "replace" | "re" => Some(2),
        _ => None,
    }
}

/// The counter's one argument, shown by example: the start typed so far if it
/// is a number, else 1.
fn counter(kind: &str, after: &str) -> Vec<Candidate> {
    let start = after.trim();
    let start = if start.is_empty() {
        "1"
    } else if start.parse::<i64>().is_ok() {
        start
    } else {
        return Vec::new();
    };
    vec![Candidate {
        insert: format!("{{{kind}|{start}}}"),
        detail: "counter, starting from this number",
    }]
}

/// One offer before filtering.
struct Entry {
    /// The candidate without its braces.
    inner: String,
    detail: &'static str,
    /// Lowercased; a query this starts with is a first-rank match. In the
    /// catalogue it is the kind, after a `|` the format or transform name.
    name: String,
    /// Lowercased; a query that is a subsequence of this is a second-rank
    /// match. In the catalogue it is the whole inner text, so `slug` finds
    /// `{name|slug}`. After a `|` it is only the part being offered: the kind
    /// and chain already typed would otherwise match every entry.
    text: String,
}

/// Filter and order: prefix matches first, then subsequence matches, each in
/// catalogue order, and nothing else. `query` is lowercased.
fn ranked(entries: Vec<Entry>, query: &str) -> Vec<Candidate> {
    let (mut first, mut then) = (Vec::new(), Vec::new());
    for entry in entries {
        let candidate = Candidate {
            insert: format!("{{{}}}", entry.inner),
            detail: entry.detail,
        };
        if entry.name.starts_with(query) {
            first.push(candidate);
        } else if subsequence(query, &entry.text) {
            then.push(candidate);
        }
    }
    first.extend(then);
    first
}

/// Whether every char of `needle` appears in `haystack` in order.
fn subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|c| rest.any(|h| h == c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rename::template::Template;

    fn inserts(typed: &str, rows: usize) -> Vec<String> {
        candidates(typed, rows)
            .into_iter()
            .map(|c| c.insert)
            .collect()
    }

    #[test]
    fn an_empty_query_lists_the_catalogue_in_order() {
        let all = inserts("", 5);
        assert_eq!(all.len(), CATALOGUE.len());
        assert_eq!(&all[..4], ["{name}", "{ext}", "{n}", "{date}"]);
        assert_eq!(
            all.last().map(String::as_str),
            Some("{name|re|pattern|replacement}")
        );
    }

    #[test]
    fn the_padded_counter_is_as_wide_as_the_row_count() {
        let padded = |rows| {
            let all = inserts("", rows);
            assert_eq!(all[2], "{n}");
            all.get(3).filter(|s| s.starts_with("{nn")).cloned()
        };
        assert_eq!(padded(1), None);
        assert_eq!(padded(9), None);
        assert_eq!(padded(10), Some("{nn}".to_string()));
        assert_eq!(padded(99), Some("{nn}".to_string()));
        assert_eq!(padded(100), Some("{nnn}".to_string()));
        assert_eq!(padded(120), Some("{nnn}".to_string()));
        assert_eq!(padded(1000), Some("{nnnn}".to_string()));
    }

    #[test]
    fn typing_part_of_a_kind_narrows_to_it() {
        assert_eq!(
            inserts("dat", 5),
            [
                "{date}",
                "{date|YYYY-MM-DD HH.mm}",
                "{date|YYYY-MM}",
                "{date|YYYY}"
            ]
        );
        assert_eq!(inserts("cam", 5), ["{camera}"]);
        // `nn` is also a subsequence of `name|re|pattern|replacement`; the
        // prefix match leads.
        assert_eq!(inserts("nn", 120)[0], "{nnn}");
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(inserts("DAT", 5), inserts("dat", 5));
        assert_eq!(inserts("Slug", 5), ["{name|slug}"]);
    }

    /// `c` starts `created` and `camera`, and is merely somewhere inside
    /// `replace` and `replacement`: the two prefix matches lead, each group in
    /// catalogue order.
    #[test]
    fn prefix_matches_come_before_subsequence_matches() {
        assert_eq!(
            inserts("c", 5),
            [
                "{created}",
                "{camera}",
                "{name|replace|from|to}",
                "{name|re|pattern|replacement}"
            ]
        );
    }

    #[test]
    fn a_subsequence_finds_a_transform_by_its_name() {
        assert_eq!(inserts("lower", 5), ["{name|lower}"]);
        assert_eq!(inserts("nmup", 5), ["{name|upper}"]);
    }

    #[test]
    fn a_query_that_matches_nothing_lists_nothing() {
        assert_eq!(inserts("zzz", 5), Vec::<String>::new());
    }

    #[test]
    fn a_date_offers_its_formats_after_the_pipe() {
        let taken = inserts("taken|", 5);
        assert_eq!(taken.len(), DATE_FORMATS.len());
        assert_eq!(taken[0], "{taken|YYYY-MM-DD}");
        assert!(taken.iter().all(|s| s.starts_with("{taken|")));
        assert_eq!(
            inserts("date|YYYY-MM-DD H", 5),
            ["{date|YYYY-MM-DD HH.mm}", "{date|YYYY-MM-DD HH.mm.ss}"]
        );
        // `yy-` starts only the two-digit year, and is inside every
        // `YYYY-` format, which follow it.
        let short = inserts("created|yy-", 5);
        assert_eq!(short[0], "{created|YY-MM-DD}");
        assert_eq!(short.len(), 5);
        assert_eq!(inserts("date|YYYY|", 5), Vec::<String>::new());
    }

    #[test]
    fn a_text_value_offers_the_next_transform() {
        let name = inserts("name|", 5);
        assert_eq!(name.len(), TRANSFORMS.len());
        assert_eq!(name[0], "{name|lower}");
        assert_eq!(name[6], "{name|re|pattern|replacement}");
        assert_eq!(inserts("ext|up", 5), ["{ext|upper}"]);
        assert_eq!(inserts("name|lower|sl", 5), ["{name|lower|slug}"]);
        assert_eq!(
            inserts("parent|replace|a|b|t", 5),
            ["{parent|replace|a|b|title}", "{parent|replace|a|b|trim}"]
        );
        // The subsequence is looked for in the transform's name, not in the
        // chain already typed: `am` is inside `name`, but no transform has it.
        assert_eq!(inserts("name|am", 5), Vec::<String>::new());
        assert_eq!(inserts("camera|ug", 5), ["{camera|slug}"]);
    }

    /// Inside `replace`'s arguments the next thing is free text, and there is
    /// nothing to offer; an unknown transform leaves the chain unreadable.
    #[test]
    fn an_argument_position_offers_nothing() {
        assert_eq!(inserts("name|replace|", 5), Vec::<String>::new());
        assert_eq!(inserts("name|replace|a|", 5), Vec::<String>::new());
        assert_eq!(inserts("name|shout|", 5), Vec::<String>::new());
    }

    #[test]
    fn the_counter_explains_its_start() {
        assert_eq!(inserts("nn|", 5), ["{nn|1}"]);
        assert_eq!(inserts("n|10", 5), ["{n|10}"]);
        assert_eq!(inserts("n|x", 5), Vec::<String>::new());
        assert_eq!(inserts("n|1|", 5), Vec::<String>::new());
    }

    #[test]
    fn a_value_without_arguments_offers_nothing_after_a_pipe() {
        assert_eq!(inserts("width|", 5), Vec::<String>::new());
        assert_eq!(inserts("bogus|", 5), Vec::<String>::new());
        assert_eq!(inserts("|", 5), Vec::<String>::new());
    }

    /// Whatever the popover inserts is a template that parses cleanly into one
    /// value, placeholders and all.
    #[test]
    fn every_candidate_is_a_clean_template() {
        let queries = [
            "",
            "date|",
            "taken|",
            "name|",
            "name|lower|",
            "camera|",
            "nnn|",
            "n|42",
        ];
        for query in queries {
            for candidate in candidates(query, 120) {
                let template = Template::parse(&candidate.insert);
                assert_eq!(template.problems(), &[], "{}", candidate.insert);
                assert_eq!(template.tokens().count(), 1, "{}", candidate.insert);
                assert!(!candidate.detail.is_empty());
            }
        }
    }
}
