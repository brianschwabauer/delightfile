//! The subsequence matcher the command palette and the jump overlays rank with
//! (PLAN §4.4, §7.2).
//!
//! Two matchers already exist in delightfile and neither one is this. df-core's
//! [`match_name`](df_core::fs::match_name) is a *substring* search — it is what
//! `f` and `/` mean, and it is deliberately literal so that filtering a
//! directory never surprises anybody. `Ctrl+p` is the opposite kind of question:
//! you type `cpa` meaning "command palette" and the answer has to be there
//! before the word is. So this is a **subsequence** matcher with a rank, which
//! is what every palette worth using does and what DelightMail's does.
//!
//! Three properties it is written to have, all of them testable without a
//! window (PLAN §1):
//!
//! 1. **Pure.** `(haystack, needle) → Option<Match>` and nothing else. No
//!    allocation the caller cannot see, no state between calls, no clock.
//! 2. **Smart-case**, the same rule the file filter uses: a lowercase needle
//!    ignores case, a needle with any capital in it does not. One case rule in
//!    the program, not two (df-core's [`is_case_sensitive`]).
//! 3. **Spans out**, as `(start, end)` byte ranges into the haystack, which is
//!    exactly the shape [`df_core::fs::Span`] already is — so the palette
//!    highlights its matches with the same painter code the listing does.
//!
//! ## How it ranks
//!
//! Greedily taking the first place each character occurs is cheap and wrong:
//! greedy matches `cp` against `copy-path` as `c`-`o`-`p`, and the `p` you meant
//! is the one that starts `path`. Trying every *start* position does not fix it
//! either, because the bad choice is in the middle. So this is a small dynamic
//! program: `best[i][j]` is the best score for matching the first `i + 1`
//! characters of the needle with the `i`-th landing on haystack position `j`,
//! and the winner is read back through parent pointers.
//!
//! The inner loop is bounded rather than quadratic. A gap's penalty is capped
//! ([`GAP_CAP`]), so every predecessor further back than [`GAP_CAP`] / [`GAP`]
//! characters is worth exactly the same thing — one running prefix maximum
//! covers all of them, and only the short window nearer than that is examined
//! one by one. That makes the whole match linear in the haystack for each
//! needle character, which is what keeps a palette responsive while it is being
//! typed into.
//!
//! The score itself is a sum of the bonuses a person actually reads by:
//! characters that start a word count for more than characters buried in one,
//! runs count for more than scattered hits, and distance between hits costs.
//! One borrowed subtlety, and it is the one that makes the ranking feel right:
//! **a character matched immediately after another inherits its bonus** if that
//! is the larger of the two. Typing `pal` into `palette` should beat typing it
//! into `p-a-l`, and it only does if the `a` and the `l` of a run count as part
//! of the word their `p` started rather than as two letters in the middle of
//! one.

use df_core::fs::{is_case_sensitive, Span};

/// What one matched character is worth before any bonus.
///
/// The unit the rest of the table is expressed in. It is positive and large
/// enough that a longer match can never lose to a shorter one on gap penalties
/// alone — typing more of a name must never make it rank worse.
const MATCH: i32 = 16;

/// A character that begins a word — after `/`, `-`, `_`, `.`, a space or a
/// comma — is worth this much extra.
///
/// Three quarters of a match, which is the weight that makes `cp` prefer
/// `command-palette` (two word starts) over `copy` (one word start and one
/// letter in the middle) without letting a pile of boundaries beat an outright
/// longer match.
const BOUNDARY: i32 = 12;

/// The very first character of the haystack, on top of [`BOUNDARY`].
///
/// Small, because "starts with what you typed" is already covered by the
/// boundary bonus; this only breaks the tie between the first word and a later
/// one, which is the order a person expects to read.
const HEAD: i32 = 4;

/// A capital after a lowercase letter or a digit — `commandPalette`,
/// `ToggleView`. Just under [`BOUNDARY`] because a camel hump is a weaker word
/// start than a real separator, but it is unmistakably one.
const CAMEL: i32 = 10;

/// A match immediately after the previous match.
///
/// Half a match. Typing consecutive characters is the strongest signal there
/// is that you are typing a *word* rather than initials, so a contiguous run
/// has to outrank the same characters scattered across the string — and this is
/// the number that makes `ab` prefer `xxab` to `xaxb`.
const RUN: i32 = 8;

/// What each skipped character costs inside the matched span.
///
/// Deliberately much smaller than [`MATCH`]: distance is a tiebreak, not a
/// veto. Two is enough to separate two otherwise identical matches by how
/// tightly they sit, and small enough that six characters of gap never sink a
/// match that has one more boundary hit than its rival.
const GAP: i32 = 2;

/// The most one gap can cost, however long it is.
///
/// Without a cap, a match at the end of a long path is dominated by the length
/// of the path rather than by anything about the match, and every deep
/// directory sorts to the bottom no matter how well it matched. Twelve is
/// three quarters of a match: a real cost, never a disqualification.
const GAP_CAP: i32 = 12;

/// What each character *before* the first match costs.
///
/// One, capped at [`LEAD_CAP`]. It is smaller than [`GAP`] because leading
/// distance is much less informative than internal distance: everything in
/// `/home/brian/…` shares a prefix nobody typed and nobody is choosing by.
const LEAD: i32 = 1;

/// The most the leading distance can cost. A third of a match — enough to break
/// a tie towards the earlier hit, never enough to reorder two different
/// matches.
const LEAD_CAP: i32 = 6;

/// One ranked match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Higher is better. Only ever compared against other scores from this
    /// function — the absolute number means nothing on its own.
    pub score: i32,
    /// Where the matched characters are, as byte ranges into the haystack,
    /// with adjacent characters merged into one run. The same `(start, end)`
    /// shape the file filter produces, so one painter highlights both.
    pub spans: Vec<Span>,
}

/// Does `needle` occur in `haystack` as a subsequence, and how well?
///
/// An empty needle matches everything at score zero with nothing highlighted —
/// which is what an unfiltered palette is, and it keeps the caller from having
/// to special-case the moment before the first keystroke.
pub fn score(haystack: &str, needle: &str) -> Option<Match> {
    if needle.is_empty() {
        return Some(Match {
            score: 0,
            spans: Vec::new(),
        });
    }
    let sensitive = is_case_sensitive(needle);
    let fold = |c: char| {
        if sensitive {
            c
        } else {
            // ASCII-only folding, the same compromise df-core's filter makes:
            // the alternative is a Unicode case table this program will not
            // take on (PLAN §1), and it changes the answer for no command name
            // and no path on this machine.
            c.to_ascii_lowercase()
        }
    };

    let hay: Vec<(usize, char)> = haystack.char_indices().map(|(i, c)| (i, fold(c))).collect();
    let raw: Vec<char> = haystack.chars().collect();
    let want: Vec<char> = needle.chars().map(fold).collect();
    if want.len() > hay.len() {
        return None;
    }

    // Each haystack position's own bonus, before any run inheritance.
    let base: Vec<i32> = (0..hay.len())
        .map(|j| {
            if j == 0 {
                BOUNDARY + HEAD
            } else if is_separator(hay[j - 1].1) {
                BOUNDARY
            } else if is_camel(raw[j - 1], raw[j]) {
                CAMEL
            } else {
                0
            }
        })
        .collect();

    // `best[j]`: the score of the best match of the needle so far that ends
    // with its last character on haystack position `j`. `NEVER` is "no match
    // lands here", kept far enough below zero that adding a bonus to it can
    // never make it look like a real score.
    let mut best = vec![NEVER; hay.len()];
    let mut effective = vec![0i32; hay.len()];
    // Parent pointers, one row per needle character, for reading the answer
    // back out as positions.
    let mut parents: Vec<Vec<usize>> = Vec::with_capacity(want.len());

    for (i, c) in want.iter().enumerate() {
        let mut row = vec![NEVER; hay.len()];
        let mut row_effective = vec![0i32; hay.len()];
        let mut parent = vec![usize::MAX; hay.len()];
        // A running maximum over every predecessor far enough back that its gap
        // has already hit [`GAP_CAP`] — one number standing in for all of them,
        // which is what keeps this loop linear rather than quadratic.
        let mut far = NEVER;
        let mut far_at = usize::MAX;
        for j in i..hay.len() {
            // The predecessor that has just fallen out of the near window.
            if j > WINDOW && best[j - WINDOW - 1] > far {
                far = best[j - WINDOW - 1];
                far_at = j - WINDOW - 1;
            }
            if hay[j].1 != *c {
                continue;
            }
            if i == 0 {
                row[j] = MATCH + base[j] - (j as i32 * LEAD).min(LEAD_CAP);
                row_effective[j] = base[j];
                continue;
            }
            // The best predecessor, as (total score, its bonus, its index).
            let mut top: Option<(i32, i32, usize)> = None;
            let mut offer = |total: i32, bonus: i32, from: usize| {
                if top.is_none_or(|(best, _, _)| total > best) {
                    top = Some((total, bonus, from));
                }
            };
            // The near window, one predecessor at a time — and `j - 1` is the
            // special one: a match immediately after another is a *run*, and it
            // inherits the bonus of the character that started the run whenever
            // that is the larger of the two.
            for k in (j.saturating_sub(WINDOW)..j).rev() {
                if best[k] == NEVER {
                    continue;
                }
                let (carried, bonus) = if k + 1 == j {
                    (best[k] + RUN, base[j].max(effective[k]))
                } else {
                    (
                        best[k] - (((j - k - 1) as i32) * GAP).min(GAP_CAP),
                        base[j],
                    )
                };
                offer(MATCH + bonus + carried, bonus, k);
            }
            // Everything further back than the window, collapsed into one.
            if far != NEVER {
                offer(MATCH + base[j] + far - GAP_CAP, base[j], far_at);
            }
            let Some((total, bonus, from)) = top else {
                continue;
            };
            row[j] = total;
            row_effective[j] = bonus;
            parent[j] = from;
        }
        best = row;
        effective = row_effective;
        parents.push(parent);
    }

    // The best place for the needle's *last* character is the answer.
    let (end, score) = best
        .iter()
        .enumerate()
        .filter(|(_, s)| **s != NEVER)
        .max_by_key(|(_, s)| **s)
        .map(|(j, s)| (j, *s))?;

    let mut hits = vec![end];
    let mut at = end;
    // Row 0 has no parents; every other row's pointer names the position its
    // predecessor landed on.
    for row in parents.iter().skip(1).rev() {
        at = row[at];
        hits.push(at);
    }
    hits.reverse();

    Some(Match {
        score,
        spans: spans(&hay, haystack.len(), &hits),
    })
}

/// The "no match ends here" sentinel. Far below any real score, and far enough
/// from `i32::MIN` that subtracting a gap penalty from it cannot overflow.
const NEVER: i32 = i32::MIN / 4;

/// How many predecessors are examined one at a time before the rest collapse
/// into a single running maximum.
///
/// Derived, not chosen: past this distance every gap costs exactly [`GAP_CAP`],
/// so every predecessor beyond it is worth the same and one number covers them
/// all. Changing [`GAP`] or [`GAP_CAP`] moves this with them.
const WINDOW: usize = (GAP_CAP / GAP) as usize;

/// The characters a word can start after. `/` and `.` are here because these
/// haystacks are paths as often as they are prose.
fn is_separator(c: char) -> bool {
    matches!(c, '/' | '\\' | '-' | '_' | '.' | ' ' | ',' | ':' | ';' | '(')
}

/// A camel hump: a capital that follows something that is not one. Checked
/// against the *unfolded* text, because a case-insensitive search folds the
/// evidence away.
fn is_camel(before: char, at: char) -> bool {
    at.is_uppercase() && !before.is_uppercase()
}

/// Hit positions, as merged byte ranges into the original string.
fn spans(hay: &[(usize, char)], len: usize, hits: &[usize]) -> Vec<Span> {
    // Where the character at `index` ends, in bytes.
    let end_of = |index: usize| hay.get(index + 1).map(|(b, _)| *b).unwrap_or(len);
    let mut out: Vec<Span> = Vec::new();
    for &at in hits {
        let (start, end) = (hay[at].0, end_of(at));
        match out.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => out.push((start, end)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(haystack: &str, needle: &str) -> i32 {
        score(haystack, needle)
            .unwrap_or_else(|| panic!("`{needle}` should match `{haystack}`"))
            .score
    }

    /// The baseline: a subsequence matches and anything else does not.
    #[test]
    fn a_subsequence_matches_and_a_non_subsequence_does_not() {
        assert!(score("command-palette", "cp").is_some());
        assert!(score("command-palette", "cmdplt").is_some());
        assert!(score("command-palette", "pc").is_none(), "order matters");
        assert!(score("cp", "cpx").is_none(), "needle longer than haystack");
        assert!(score("", "c").is_none());
    }

    /// An empty needle is the moment before the first keystroke: everything is
    /// a match, nothing is highlighted, and no row outranks another on it.
    #[test]
    fn an_empty_needle_matches_everything_flat() {
        let m = score("anything at all", "").expect("empty matches");
        assert_eq!(m.score, 0);
        assert!(m.spans.is_empty());
    }

    /// Smart-case, the same rule `f` and `/` use: lowercase is a wildcard for
    /// case, a capital is a demand.
    #[test]
    fn smart_case_means_a_capital_is_a_demand() {
        assert!(score("~/Work", "work").is_some(), "lowercase folds");
        assert!(score("~/Work", "Work").is_some());
        assert!(score("~/work", "Work").is_none(), "a capital is literal");
        assert!(score("Toggle Grid View", "tgv").is_some());
    }

    /// Word starts beat letters buried mid-word. `cp` means "command palette",
    /// not "copy".
    #[test]
    fn word_boundaries_outrank_the_middles_of_words() {
        assert!(
            value("command-palette", "cp") > value("copy", "cp"),
            "boundary {} vs middle {}",
            value("command-palette", "cp"),
            value("copy", "cp")
        );
        assert!(value("copy-path", "cp") > value("copy", "cp"));
        // A camel hump is a word start too, which is what makes an identifier
        // searchable by its initials.
        assert!(value("toggleGridView", "tgv") > value("together-giving-vast", "tgv"));
    }

    /// A run of consecutive characters is the strongest evidence there is.
    #[test]
    fn a_consecutive_run_outranks_the_same_letters_scattered() {
        assert!(value("xxab", "ab") > value("xaxb", "ab"));
        assert!(value("palette", "pal") > value("p-a-l", "pal"));
    }

    /// Every start is tried, not just the first: the greedy match from the
    /// leftmost `s` is not the best one here, and the ranker has to find that.
    #[test]
    fn the_best_start_wins_not_the_leftmost_one() {
        let m = score("asdf-ss", "ss").expect("matches");
        // The run at the end, after a separator — not `s` at 1 and `s` at 5.
        assert_eq!(m.spans, vec![(5, 7)]);
    }

    /// Adjacent hits merge into one span; separated ones do not. This is what
    /// the painter draws, so its shape is part of the contract.
    #[test]
    fn spans_are_merged_byte_ranges_into_the_original_string() {
        let m = score("copy-path", "cp").expect("matches");
        assert_eq!(m.spans, vec![(0, 1), (5, 6)]);
        let m = score("copy-path", "copy").expect("matches");
        assert_eq!(m.spans, vec![(0, 4)]);
        // Multi-byte characters keep their real byte widths.
        let m = score("… palette", "pal").expect("matches");
        let (start, end) = m.spans[0];
        assert_eq!(&"… palette"[start..end], "pal");
    }

    /// Typing more of a name must never make it rank worse — otherwise the row
    /// you are aiming at slides away as you close in on it.
    #[test]
    fn a_longer_needle_never_scores_less_on_the_same_haystack() {
        let mut previous = i32::MIN;
        for n in 1..="palette".len() {
            let now = value("command-palette", &"palette"[..n]);
            assert!(now > previous, "`{}` went backwards", &"palette"[..n]);
            previous = now;
        }
    }

    /// A deep path must not lose to a shallow one purely for being deep: the
    /// gap penalties are capped precisely so that the *match* decides, and a
    /// tight hit at the end of a long path beats a scattered one at the start
    /// of a short one.
    #[test]
    fn a_long_prefix_does_not_bury_a_good_match() {
        let deep = value("/home/brian/Work/delightfile", "delightfile");
        let shallow = value("/d/e/l/i/g/h/t/f/i/l/e", "delightfile");
        assert!(deep > shallow, "deep {deep} vs shallow {shallow}");
    }

    /// The whole reason this is a dynamic program and not a greedy walk: the
    /// wrong choice is in the *middle*, so no amount of trying different start
    /// positions finds it. Greedy matches `cp` here as `c`-`o`-`p`; the answer
    /// is the `p` that starts `path`.
    #[test]
    fn the_best_match_can_need_a_later_character_than_the_first_one_available() {
        let m = score("copy-path", "cp").expect("matches");
        assert_eq!(m.spans, vec![(0, 1), (5, 6)]);
    }
}
