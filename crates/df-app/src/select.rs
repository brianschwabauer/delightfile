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

use std::time::Instant;

use crate::ui::{Column, Control};

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
    /// The file that position held when the mode opened.
    ///
    /// The position is what the run is drawn from, and it is right for the
    /// changes a *person* makes — a sort, a filter. It is wrong for the ones
    /// that arrive on their own: a scan batch, or the folder-size walk pushing
    /// a new number into a listing sorted by size, reorders the rows under a
    /// run nobody was touching, and the anchor then points at a different file
    /// than the one `v` was pressed on. So the name rides along and
    /// [`Visual::reanchored`] notices when the two have come apart.
    pub anchor_name: Option<String>,
    /// The run currently applied to the selection, if any.
    pub applied: Option<(usize, usize)>,
    /// Per row this mode has touched: its name, and whether it was selected
    /// before. Names, because that is what the selection set holds.
    pub prior: Vec<(String, bool)>,
}

impl Visual {
    pub fn new(selecting: bool, anchor: usize, anchor_name: Option<String>) -> Visual {
        Visual {
            selecting,
            anchor,
            anchor_name,
            applied: None,
            prior: Vec::new(),
        }
    }

    /// Where the anchor's file has got to, when the listing has moved it.
    ///
    /// `None` when nothing moved — which is every frame of an ordinary run, so
    /// the caller's fast path is "no". `Some` means the rows were reordered
    /// under the run and the whole thing has to be laid down again from the
    /// file's new position; see the caller for why that is an undo-and-redo
    /// rather than a nudge.
    pub fn reanchored(&self, position_of: impl Fn(&str) -> Option<usize>) -> Option<usize> {
        let name = self.anchor_name.as_deref()?;
        let at = position_of(name)?;
        (at != self.anchor).then_some(at)
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
/// panes rather than an anchor row and a cursor.
///
/// ## The origin is pinned to the listing, not to the glass
///
/// A band hanging over the list pane's edge scrolls the list (PLAN §7.5), and
/// the rows it has already taken have to *stay* taken while it does. If the
/// origin stayed put on screen, every row the scroll carried past it would
/// leave the rectangle and be handed back — the selection would be a window a
/// screenful tall sliding down the listing, and a band could never hold more
/// than fits on screen, which is the one thing the scroll is there for. So the
/// origin rides with the rows: it is remembered together with the list's
/// scroll position at the press, and moved by however far the list has
/// scrolled since — [`crate::mouse::band_span`] for which rows are in the band,
/// [`Band::origin_at`] for where the rectangle is painted. Only its height
/// moves — the list scrolls vertically — so a band started in the preview pane
/// keeps its corner in the preview pane, level with the row it started beside.
#[derive(Debug, Clone)]
pub struct Band {
    /// Where the drag began, in window points, *as the list was scrolled then*.
    /// The rectangle is [`Band::origin_at`] and wherever the pointer is now.
    pub origin: egui::Pos2,
    /// The list's scroll position when the press landed, in rows of the pane.
    pub scroll: f32,
    /// When the band last scrolled the list (or would have), so the next frame
    /// scrolls by how long it has been ([`crate::mouse::band_scroll_rows`]).
    pub ticked: Instant,
    /// Whether the band is hanging over the list's edge with somewhere left to
    /// scroll to — the one thing a band asks animation frames for. A band held
    /// still inside the pane is the same pixels next frame, and so is one
    /// pressed against the end of the listing.
    pub scrolling: bool,
    /// The run currently applied, if any.
    pub applied: Option<(usize, usize)>,
    /// Per row the band has touched: its name, and whether it was selected
    /// before the drag reached it.
    pub prior: Vec<(String, bool)>,
}

impl Band {
    pub fn new(origin: egui::Pos2, scroll: f32, now: Instant) -> Band {
        Band {
            origin,
            scroll,
            ticked: now,
            scrolling: false,
            applied: None,
            prior: Vec::new(),
        }
    }

    /// Where the origin is on screen with the list scrolled to `scroll_rows`,
    /// `step` points to a row of the pane (a row of tiles, in the grid).
    pub fn origin_at(&self, scroll_rows: f32, step: f32) -> egui::Pos2 {
        self.origin + egui::vec2(0.0, (self.scroll - scroll_rows) * step)
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

/// A drag that owns the pointer until the button comes up, for
/// [`gesture_filter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// A band select, drawing a rectangle over the listing.
    Band,
    /// A selection being dragged out of the top-bar prompt's text.
    Text,
}

/// What the pointer is over, as far as anything is allowed to answer it while
/// `gesture` has it: a list row during a band, the prompt's text during a text
/// selection, or nothing.
///
/// A drag is the hand doing one thing, not pointing at the things it crosses
/// on the way. A parent row, a crumb, a chip or a tab that lit up — or sank
/// under a button that is down for another reason — as a band's corner or a
/// text selection swept over it would be the window answering a question
/// nobody asked it. A band keeps the list's own rows, because their highlight
/// is the feedback that the band has reached them; a text selection keeps only
/// the field it is selecting in, so a drag that runs off the field and down
/// over the listing lights no row and sinks none. Everything else gets `None`,
/// including the space outside every pane.
///
/// Applied where the hit test hands its answer out, so every reader of it —
/// the hover, the press, the cursor shape — is filtered together rather than
/// each remembering to ask; and once more after the frame's drag has run,
/// because that is where a band begins. With no gesture it is the identity,
/// which is how the ordinary rules come back the frame after the release. The
/// media transport keeps a pointer of its own outside the hit test, and is
/// handed none while a gesture is live for the same reason.
pub fn gesture_filter(
    gesture: Option<Gesture>,
    over: Option<(Control, egui::Pos2)>,
) -> Option<(Control, egui::Pos2)> {
    over.filter(|(control, _)| match gesture {
        None => true,
        Some(Gesture::Band) => matches!(control, Control::Row(Column::List, _)),
        Some(Gesture::Text) => matches!(control, Control::PromptField),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mid-band only a list row answers the pointer, mid-selection only the
    /// prompt's text; with no gesture, everything does exactly as it did.
    #[test]
    fn a_live_gesture_leaves_only_its_own_target_hoverable() {
        let at = egui::pos2(10.0, 10.0);
        let row = Some((Control::Row(Column::List, 4), at));
        let field = Some((Control::PromptField, at));
        assert_eq!(
            gesture_filter(Some(Gesture::Band), row),
            row,
            "the band's own feedback"
        );
        assert_eq!(gesture_filter(Some(Gesture::Band), field), None);
        assert_eq!(gesture_filter(Some(Gesture::Text), field), field);
        assert_eq!(
            gesture_filter(Some(Gesture::Text), row),
            None,
            "a text drag that runs over the listing lights no row"
        );
        for control in [
            Control::Row(Column::Parent, 2),
            Control::Crumb(1),
            Control::Tab(0),
            Control::GitChip,
            Control::FilterChip,
            Control::Counter,
            Control::BasketChip,
            Control::Toast,
        ] {
            for gesture in [Gesture::Band, Gesture::Text] {
                assert_eq!(
                    gesture_filter(Some(gesture), Some((control, at))),
                    None,
                    "{control:?} during {gesture:?}"
                );
            }
            assert_eq!(
                gesture_filter(None, Some((control, at))),
                Some((control, at)),
                "{control:?} answers again once the gesture is let go"
            );
        }
        // Outside every pane there was nothing to begin with.
        assert_eq!(gesture_filter(Some(Gesture::Band), None), None);
        assert_eq!(gesture_filter(Some(Gesture::Text), None), None);
    }

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

    /// The band's origin travels with the rows: scroll the list down three
    /// rows and the corner it was drawn from is three rows higher on screen,
    /// still level with the row it started beside. Only its height moves.
    #[test]
    fn a_band_origin_rides_with_the_scroll() {
        let at = egui::pos2(900.0, 300.0);
        let band = Band::new(at, 2.0, Instant::now());
        assert_eq!(band.origin_at(2.0, 22.0), at);
        assert_eq!(band.origin_at(5.0, 22.0), egui::pos2(900.0, 300.0 - 66.0));
        assert_eq!(band.origin_at(0.0, 22.0), egui::pos2(900.0, 300.0 + 44.0));
        // A grid scrolls in rows of tiles, and the origin moves by the same.
        assert_eq!(band.origin_at(3.0, 200.0), egui::pos2(900.0, 100.0));
    }

    /// The prior state is a row's state *before* visual mode, and it is written
    /// once — that is what makes a shrink restore a pre-existing selection
    /// rather than clearing it.
    #[test]
    fn the_first_remembered_answer_is_the_one_that_survives() {
        let mut v = Visual::new(true, 0, None);
        v.remember("kept.txt", true);
        v.remember("kept.txt", false);
        assert_eq!(v.was_selected("kept.txt"), Some(true));
        assert_eq!(v.was_selected("never.txt"), None);
    }
}
