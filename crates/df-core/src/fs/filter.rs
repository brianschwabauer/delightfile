//! `f` filter and `/` find: smart-case name matching, with the spans the list
//! needs to underline the part that matched (PLAN §7.2).
//!
//! **Substring, not fuzzy.** `f` is filter-as-you-type on a directory you are
//! looking at, where you already know the first few letters of the name; a
//! fuzzy matcher would answer `src` with `s-o-u-r-c-e`, `screenshot-2024`, and
//! everything else containing those three letters in order, and the one file
//! you meant would be third. Fuzzy belongs to `z` and the command palette,
//! where the corpus is a thousand paths you cannot see. (`Ctrl+p`, PLAN §4.4.)
//!
//! **Smart case** is the vim/rg convention and it is the one Brian's fingers
//! have: an all-lowercase query matches any case, and a query containing a
//! capital means the capital. Typing `readme` finds `README.md`; typing
//! `README` finds only the shouting one.
//!
//! **`#` asks for a tag.** A query beginning with `#` is not a name at all:
//! `#red` keeps the rows carrying a tag that starts with `red`, in any case,
//! and `#` alone keeps every row that carries a tag (see
//! [`super::tags::matches`]). No span is drawn for one — the name is not why
//! the row is there, and the dots at the end of it already say what is.
//!
//! The spans are byte ranges into the original name, and getting them right
//! through a case fold is the reason this file is longer than a `contains`
//! call: lowercasing can change a string's length (`İ` folds to two chars), so
//! the folded haystack carries a map back to the original offsets.

use super::entry::Entry;

/// A highlighted run, as `[start, end)` byte offsets into [`Entry::name`].
/// Byte offsets rather than char indices because that is what a layout engine
/// slices a `&str` with.
pub type Span = (usize, usize);

/// One row that survived the filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    /// Index into the entry list — not into the view.
    pub index: usize,
    /// Every occurrence of the query in the name, in order, non-overlapping.
    /// Empty when the query is empty (everything matches, nothing is
    /// highlighted).
    pub spans: Vec<Span>,
}

/// Whether a query is case-sensitive: it is, iff it contains an uppercase
/// character.
pub fn is_case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Match `query` against one name, smart-case. `None` is "no match"; an empty
/// query matches everything with no spans.
pub fn match_name(name: &str, query: &str) -> Option<Vec<Span>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    if is_case_sensitive(query) {
        return collect_spans(name, query, |i| i);
    }
    let (folded, map) = fold_with_map(name);
    let needle = fold_str(query);
    collect_spans(&folded, &needle, |i| {
        map.get(i).copied().unwrap_or(name.len())
    })
}

/// Every non-overlapping occurrence of `needle` in `hay`, with each offset run
/// back through `to_original`.
fn collect_spans(
    hay: &str,
    needle: &str,
    to_original: impl Fn(usize) -> usize,
) -> Option<Vec<Span>> {
    let mut spans = Vec::new();
    let mut from = 0usize;
    while let Some(found) = hay[from..].find(needle) {
        let start = from + found;
        let end = start + needle.len();
        spans.push((to_original(start), to_original(end)));
        from = end;
        if from >= hay.len() {
            break;
        }
    }
    if spans.is_empty() {
        None
    } else {
        Some(spans)
    }
}

/// Lowercase `s`, and a map from each byte of the result back to the byte in
/// `s` that produced it (plus a final entry for the end, so a span's exclusive
/// end maps too).
fn fold_with_map(s: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len() + 1);
    for (i, c) in s.char_indices() {
        let before = folded.len();
        for lower in c.to_lowercase() {
            folded.push(lower);
        }
        for _ in before..folded.len() {
            map.push(i);
        }
    }
    map.push(s.len());
    (folded, map)
}

fn fold_str(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// The visible rows: `candidates` (already in sort order) narrowed by the
/// hidden toggle and the filter query, keeping the order it was given.
///
/// Hidden files are dropped *before* the query is applied, so `.` and `f`
/// compose the way you would expect: filtering for `git` with hidden off does
/// not surface `.gitignore`. A query beginning with `#` matches tags rather
/// than names (see the module header).
pub fn filter_indices(
    entries: &[Entry],
    candidates: &[usize],
    query: &str,
    show_hidden: bool,
) -> Vec<Matched> {
    candidates
        .iter()
        .filter_map(|&index| {
            let entry = entries.get(index)?;
            if entry.is_hidden && !show_hidden {
                return None;
            }
            let spans = match query.strip_prefix('#') {
                Some(tag) => super::tags::matches(&entry.tags, tag).then(Vec::new)?,
                None => match_name(&entry.name, query)?,
            };
            Some(Matched { index, spans })
        })
        .collect()
}

/// Which way `/` and `?` walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindDirection {
    /// `/` and `n`.
    Forward,
    /// `?` and `N`.
    Backward,
}

/// The next row matching `query`, as a position in `view`, wrapping.
///
/// Starts at the row *after* `from` (before it, going backward) and walks the
/// whole list, so:
///
/// - the row under the cursor is never the immediate answer — pressing `n`
///   twice moves twice;
/// - the search wraps, and a query that matches only the current row comes back
///   around to it rather than returning nothing, because "there is one match and
///   you are on it" and "there are no matches" are different answers and only
///   the second should say so.
pub fn find_from(
    entries: &[Entry],
    view: &[usize],
    from: usize,
    query: &str,
    direction: FindDirection,
) -> Option<usize> {
    if view.is_empty() || query.is_empty() {
        return None;
    }
    let len = view.len();
    let from = from.min(len - 1);
    for step in 1..=len {
        let position = match direction {
            FindDirection::Forward => (from + step) % len,
            FindDirection::Backward => (from + len - (step % len)) % len,
        };
        let index = view[position];
        if entries
            .get(index)
            .is_some_and(|e| match_name(&e.name, query).is_some())
        {
            return Some(position);
        }
    }
    None
}
