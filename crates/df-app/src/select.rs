//! Visual mode: `v` and `V` (PLAN §4.1).
//!
//! `v` drops an anchor on the row under the cursor and, as the cursor moves,
//! keeps the whole run between anchor and cursor selected. `V` is the same
//! motion with the sign flipped — it *unselects* the run. Both leave everything
//! outside the run exactly as it was, and that last clause is the entire reason
//! this module exists rather than a call to
//! [`DirState::select_range`](df_core::fs::DirState::select_range).
//!
//! ## Why the prior state is remembered
//!
//! The run is not committed on exit; it is applied *live*, because a visual mode
//! you cannot see is not one you can aim. So when the cursor comes back towards
//! the anchor and the run shrinks, the rows that just left it have to go back to
//! what they were before visual mode touched them — which for a row that was
//! already selected is *selected*, not clear. Anything simpler (re-selecting the
//! new range over a cleared list, say) quietly eats a selection built up before
//! `v` was pressed, which is exactly the selection somebody was extending.
//!
//! So a [`Visual`] remembers, per row it has touched, what that row was before
//! it touched it — and the two pure functions below are the whole of the range
//! arithmetic, tested without a directory.

/// The inclusive run between the anchor and the cursor, low end first.
pub fn range(anchor: usize, cursor: usize) -> (usize, usize) {
    if anchor <= cursor {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    }
}

/// What changed between the run that is applied and the run that should be:
/// `(leaving, entering)`.
///
/// `leaving` rows are restored to what they were before visual mode; `entering`
/// rows take the mode's value. Rows in both are left alone — repainting them
/// would be harmless but recording their "prior" state a second time would
/// overwrite the real one with visual mode's own answer.
pub fn range_delta(
    applied: Option<(usize, usize)>,
    wanted: (usize, usize),
) -> (Vec<usize>, Vec<usize>) {
    let inside = |run: (usize, usize), i: usize| i >= run.0 && i <= run.1;
    let leaving = match applied {
        Some(old) => (old.0..=old.1).filter(|i| !inside(wanted, *i)).collect(),
        None => Vec::new(),
    };
    let entering = (wanted.0..=wanted.1)
        .filter(|i| !applied.is_some_and(|old| inside(old, *i)))
        .collect();
    (leaving, entering)
}

/// One live visual-mode session.
#[derive(Debug, Clone)]
pub struct Visual {
    /// `true` for `v` (select the run), `false` for `V` (unselect it).
    pub selecting: bool,
    /// The row `v` was pressed on, as a view position.
    ///
    /// A position rather than a name: a sort or a filter change mid-visual would
    /// move the anchor's *file* anywhere in the list, and a run that jumped to
    /// wherever that file went is not the run anybody was drawing. Holding the
    /// position means the anchor stays where it looks like it is.
    pub anchor: usize,
    /// The run currently applied to the selection, if any.
    pub applied: Option<(usize, usize)>,
    /// Per row this mode has touched: its name, and whether it was selected
    /// before. Names, because that is what the selection set holds.
    pub prior: Vec<(String, bool)>,
}

impl Visual {
    pub fn new(selecting: bool, anchor: usize) -> Visual {
        Visual {
            selecting,
            anchor,
            applied: None,
            prior: Vec::new(),
        }
    }

    /// Remember a row's state before visual mode changes it — the first answer
    /// wins, so re-entering a row it has already touched does not record
    /// visual mode's own doing as the row's history.
    pub fn remember(&mut self, name: &str, selected: bool) {
        if self.prior.iter().any(|(n, _)| n == name) {
            return;
        }
        self.prior.push((name.to_string(), selected));
    }

    /// What a row was before visual mode touched it. `None` for a row it never
    /// did, which cannot happen for a row that is leaving the run.
    pub fn was_selected(&self, name: &str) -> Option<bool> {
        self.prior
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, was)| *was)
    }
}

/// A band-select drag (PLAN §7.5), which is visual mode drawn with a pointer.
///
/// It is deliberately the *same* arithmetic: [`range_delta`] decides what is
/// entering and leaving the run, and the prior state of every row it has
/// touched is remembered for exactly the reason [`Visual`] remembers it — a
/// band that shrinks has to hand back a selection built up before the drag
/// started, not clear it.
///
/// What is different is only where the run comes from: a rectangle over the
/// pane rather than an anchor row and a cursor.
#[derive(Debug, Clone)]
pub struct Band {
    /// Where the drag began, in window points. The rectangle is this and
    /// wherever the pointer is now.
    pub origin: egui::Pos2,
    /// The run currently applied, if any.
    pub applied: Option<(usize, usize)>,
    /// Per row the band has touched: its name, and whether it was selected
    /// before the drag reached it.
    pub prior: Vec<(String, bool)>,
}

impl Band {
    pub fn new(origin: egui::Pos2) -> Band {
        Band {
            origin,
            applied: None,
            prior: Vec::new(),
        }
    }

    /// The first answer wins — see [`Visual::remember`].
    pub fn remember(&mut self, name: &str, selected: bool) {
        if self.prior.iter().any(|(n, _)| n == name) {
            return;
        }
        self.prior.push((name.to_string(), selected));
    }

    pub fn was_selected(&self, name: &str) -> Option<bool> {
        self.prior
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, was)| *was)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_reads_the_same_in_both_directions() {
        assert_eq!(range(3, 7), (3, 7));
        assert_eq!(range(7, 3), (3, 7));
        assert_eq!(range(4, 4), (4, 4), "the anchor row alone is a run of one");
    }

    /// Opening a run selects all of it and restores nothing.
    #[test]
    fn the_first_application_is_all_entering() {
        let (leaving, entering) = range_delta(None, (2, 5));
        assert!(leaving.is_empty());
        assert_eq!(entering, vec![2, 3, 4, 5]);
    }

    /// Growing the run only touches the rows it grew over — the rows already in
    /// it must not have their remembered state rewritten.
    #[test]
    fn growing_a_run_only_adds_the_new_rows() {
        let (leaving, entering) = range_delta(Some((2, 5)), (2, 8));
        assert!(leaving.is_empty());
        assert_eq!(entering, vec![6, 7, 8]);
        // …and growing the other way, past the anchor.
        let (leaving, entering) = range_delta(Some((2, 5)), (0, 5));
        assert!(leaving.is_empty());
        assert_eq!(entering, vec![0, 1]);
    }

    /// Shrinking hands back exactly the rows that left.
    #[test]
    fn shrinking_a_run_gives_back_the_rows_it_lost() {
        let (leaving, entering) = range_delta(Some((2, 8)), (2, 4));
        assert_eq!(leaving, vec![5, 6, 7, 8]);
        assert!(entering.is_empty());
    }

    /// Crossing the anchor is both at once: the old side is handed back and the
    /// new side is taken.
    #[test]
    fn crossing_the_anchor_swaps_the_run() {
        let (leaving, entering) = range_delta(Some((5, 8)), (2, 5));
        assert_eq!(leaving, vec![6, 7, 8]);
        assert_eq!(entering, vec![2, 3, 4]);
    }

    /// The prior state is a row's state *before* visual mode, and it is written
    /// once — that is what makes a shrink restore a pre-existing selection
    /// rather than clearing it.
    #[test]
    fn the_first_remembered_answer_is_the_one_that_survives() {
        let mut v = Visual::new(true, 0);
        v.remember("kept.txt", true);
        v.remember("kept.txt", false);
        assert_eq!(v.was_selected("kept.txt"), Some(true));
        assert_eq!(v.was_selected("never.txt"), None);
    }
}
