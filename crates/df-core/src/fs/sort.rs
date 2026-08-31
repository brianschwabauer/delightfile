//! Putting the list in order: every sort mode PLAN §4.1's `,` chord offers,
//! as pure functions over `&[Entry]`.
//!
//! Nothing here touches the filesystem or mutates a model. [`sort_order`] takes
//! entries and options and returns the permutation, which is the shape the rest
//! of the code wants: [`super::DirState`] keeps entries in scan order forever
//! and re-derives a `Vec<usize>` view, so re-sorting a 200k directory moves
//! indices rather than strings, and PLAN §8's FLIP animation gets the old and
//! new positions of every row for free.
//!
//! Three rules hold across every mode:
//!
//! - **`dir_first` outranks everything, including reverse.** Directories are a
//!   band at the top; reversing the sort reverses the order *within* each band
//!   and does not float files above folders. Reversing a listing should feel
//!   like reading it upside down, not like the panes swapped.
//! - **The sort is stable**, so equal keys keep the order they arrived in and a
//!   redraw never shuffles rows that did not change.
//! - **Every mode has a total tie-break** (the natural name comparison), because
//!   `mtime` ties are the normal case — a `git checkout` stamps a hundred files
//!   with the same second — and `read_dir` order is whatever the filesystem
//!   felt like. A list that reorders itself between two identical scans looks
//!   broken even when it is technically stable.

use std::cmp::Ordering;

use super::entry::Entry;
use crate::config::{MgrConfig, SortBy};

/// Everything the `,` chord can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortOptions {
    pub by: SortBy,
    /// Descending. `, M`, `, B`, `, E`… — the uppercase half of every pair.
    pub reverse: bool,
    pub dir_first: bool,
    /// Case-*sensitive* name comparison. Off by default (yazi's
    /// `sort_sensitive = false`): with it on, `Zebra` sorts before `apple`
    /// because capitals are lower code points, which is nobody's alphabet.
    pub sensitive: bool,
    /// The shuffle for [`SortBy::Random`]. Held rather than drawn fresh each
    /// call so that a random order *stays put* while you scroll it — a listing
    /// that reshuffles on every repaint is unusable — and so the tests can pin
    /// it. Re-roll with [`random_seed`] on each `, r`.
    pub seed: u64,
}

impl Default for SortOptions {
    fn default() -> SortOptions {
        SortOptions::from_config(&MgrConfig::default())
    }
}

impl SortOptions {
    pub fn from_config(mgr: &MgrConfig) -> SortOptions {
        SortOptions {
            by: mgr.sort_by,
            reverse: mgr.sort_reverse,
            dir_first: mgr.sort_dir_first,
            sensitive: mgr.sort_sensitive,
            seed: 0,
        }
    }
}

/// A fresh shuffle for `, r`. Time-based because df-core has no rng and does
/// not want one: the only requirement is "different from last time", and
/// nanoseconds-since-epoch clears that without a dependency.
pub fn random_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
}

/// The order to draw `entries` in: a permutation of `0..entries.len()`.
pub fn sort_order(entries: &[Entry], opts: &SortOptions) -> Vec<usize> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    // `sort_by` is stable, which the doc comment above promises. The comparator
    // takes indices so `SortBy::None` — "however the filesystem handed them
    // over" — can still be reversed, which a key-based comparator could not do.
    order.sort_by(|&a, &b| compare(entries, a, b, opts));
    order
}

/// Sort in place. Convenience for callers that do not keep a view (tests, the
/// parent pane); [`sort_order`] is the one the list uses.
pub fn sort_entries(entries: &mut Vec<Entry>, opts: &SortOptions) {
    let order = sort_order(entries, opts);
    let mut taken: Vec<Option<Entry>> = entries.drain(..).map(Some).collect();
    entries.extend(order.into_iter().filter_map(|i| taken[i].take()));
}

fn compare(entries: &[Entry], ia: usize, ib: usize, opts: &SortOptions) -> Ordering {
    let (a, b) = (&entries[ia], &entries[ib]);
    if opts.dir_first {
        match (a.is_dir(), b.is_dir()) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
    }
    let ord = key_cmp(a, b, ia, ib, opts);
    if opts.reverse {
        ord.reverse()
    } else {
        ord
    }
}

fn key_cmp(a: &Entry, b: &Entry, ia: usize, ib: usize, opts: &SortOptions) -> Ordering {
    let name = || natural_cmp(&a.name, &b.name, opts.sensitive);
    match opts.by {
        SortBy::Alphabetical => alphabetical_cmp(&a.name, &b.name, opts.sensitive),
        SortBy::Natural => name(),
        // The extension, then the name, so `a.rs`/`b.rs` stay in alphabetical
        // order inside their group instead of in `read_dir` order.
        SortBy::Extension => {
            compare_folded(a.extension(), b.extension(), opts.sensitive).then_with(name)
        }
        // Newest first is what "sort by time" means to everyone who has ever
        // looked at a Downloads folder, so the *default* direction is
        // descending and `reverse` flips it back to oldest-first. Entries with
        // no timestamp sort last in either direction — an unknown time is not
        // "the beginning of history".
        SortBy::Mtime => newest_first(a.mtime, b.mtime).then_with(name),
        SortBy::Btime => newest_first(a.btime, b.btime).then_with(name),
        // Biggest first, same reasoning: you sort by size to find the big one.
        // Directories are all zero (see `Entry::len`) so they tie and fall
        // through to the name comparison.
        SortBy::Size => b.len.cmp(&a.len).then_with(name),
        SortBy::Random => shuffle_key(&a.name, opts.seed).cmp(&shuffle_key(&b.name, opts.seed)),
        SortBy::None => ia.cmp(&ib),
    }
}

fn newest_first(a: Option<std::time::SystemTime>, b: Option<std::time::SystemTime>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// A stable pseudo-random key per name: FNV-1a mixed with the seed and run
/// through splitmix64's finaliser. Keyed on the *name* rather than the index so
/// that a rescan of an unchanged directory produces the same shuffle — the
/// files do not jump around while a copy is landing.
fn shuffle_key(name: &str, seed: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for byte in name.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    // splitmix64 finaliser: FNV alone leaves short names clumped in the low
    // bits, which would sort every one-character name together.
    let mut z = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

// ── Name comparison ─────────────────────────────────────────────────────────

/// Lowercase a char for comparison. `char::to_lowercase` yields a sequence
/// (`İ` → two chars); taking the first is enough to *order* by and keeps this a
/// char-to-char comparison, and the case tie-break below means the two spellings
/// still get a deterministic order.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Plain lexicographic, case-insensitive unless `sensitive`.
///
/// This is the default mode and yazi's: `file10` sorts before `file9`, because
/// character three is `1` and `1 < 9`. That is the correct answer for a
/// dictionary and the wrong one for a folder of episodes, which is exactly why
/// [`natural_cmp`] exists as its own mode rather than replacing this one.
pub fn alphabetical_cmp(a: &str, b: &str, sensitive: bool) -> Ordering {
    compare_names(a, b, sensitive, false)
}

/// Numeric-aware: a run of digits compares as a number, so `file9` sorts before
/// `file10` (yazi's `natural`, PLAN §4.1's `, n`).
///
/// Leading zeros break a tie *at the end*, not in place: `1` and `01` are the
/// same number, so `img1.png` vs `img01.jpg` is decided by the `.png`/`.jpg`
/// that follows, and only a whole-name tie falls back to "fewer zeros first".
/// Doing it the other way round makes `img01.jpg` sort after `img1.png`, which
/// looks like the extension was ignored.
pub fn natural_cmp(a: &str, b: &str, sensitive: bool) -> Ordering {
    compare_names(a, b, sensitive, true)
}

fn compare_folded(a: &str, b: &str, sensitive: bool) -> Ordering {
    compare_names(a, b, sensitive, false)
}

fn compare_names(a: &str, b: &str, sensitive: bool, numeric: bool) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    // Deferred tie-breaks: case and leading zeros only decide names that are
    // otherwise identical, so they are remembered and applied at the end.
    let mut tie = Ordering::Equal;

    loop {
        let (Some(x), Some(y)) = (ai.peek().copied(), bi.peek().copied()) else {
            // Whichever ran out first is the shorter name, and shorter sorts
            // first — `file` before `file2`.
            return match (ai.peek(), bi.peek()) {
                (None, None) => tie,
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                // Unreachable: the `let else` above only fires when at least
                // one side is exhausted.
                (Some(_), Some(_)) => tie,
            };
        };

        if numeric && x.is_ascii_digit() && y.is_ascii_digit() {
            let xs = take_digits(&mut ai);
            let ys = take_digits(&mut bi);
            let xt = xs.trim_start_matches('0');
            let yt = ys.trim_start_matches('0');
            // Same digits, no leading zeros: longer is bigger. Otherwise the
            // usual lexicographic comparison is the numeric one.
            let ord = xt.len().cmp(&yt.len()).then_with(|| xt.cmp(yt));
            if ord != Ordering::Equal {
                return ord;
            }
            if tie == Ordering::Equal {
                tie = xs.len().cmp(&ys.len());
            }
            continue;
        }

        let (cx, cy) = if sensitive {
            (x, y)
        } else {
            (fold(x), fold(y))
        };
        if cx != cy {
            return cx.cmp(&cy);
        }
        if tie == Ordering::Equal && x != y {
            // Same letter, different case. Uppercase first, which is code-point
            // order, so `README` and `readme` at least never swap places.
            tie = x.cmp(&y);
        }
        ai.next();
        bi.next();
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}
