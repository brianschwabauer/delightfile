//! Which rows are on screen: the scrolloff rule, as one pure function.
//!
//! PLAN §2 pins `scrolloff = 5`, and the thing that number describes is a
//! property of the **view**, not of the cursor: the cursor goes wherever it was
//! sent, and the window slides the least it can to keep five rows of context
//! visible on the side the cursor is heading towards. Yazi feels the way it does
//! because the list mostly *doesn't* move — it holds still until the cursor
//! reaches the margin and then travels exactly one row at a time.
//!
//! Everything here is arithmetic over `(first_row, cursor, rows, visible,
//! scrolloff)` so the rule can be tested without a window (PLAN §9).
//!
//! The mouse does **not** come through here. A wheel roll scrolls the view and
//! leaves the cursor behind (PLAN §7.5), which this rule would undo on the next
//! frame — so the listing detaches instead, and the rule below resumes on the
//! next cursor move (see [`crate::tab::Listing::follow_cursor`]).

/// The first visible row after a cursor move.
///
/// `first` is where the view is now, `cursor` where the cursor has just gone,
/// `rows` how many rows the list has, `visible` how many fit. The result is
/// clamped so the list never scrolls past its own ends — which is also what
/// lets the cursor reach the first and last rows despite the margin.
pub fn first_visible(
    first: usize,
    cursor: usize,
    rows: usize,
    visible: usize,
    scrolloff: usize,
) -> usize {
    if visible == 0 || rows <= visible {
        // Everything fits: there is nothing to scroll, and a non-zero offset
        // here would leave a gap at the bottom of a short directory.
        return 0;
    }
    // A margin bigger than half the pane would demand more context than there
    // is room for, and the two bounds below would cross. Capping it at half the
    // visible height means a very short pane degrades to "keep the cursor
    // centred" rather than to nonsense.
    let margin = scrolloff.min((visible - 1) / 2);
    // The cursor must sit within `[first + margin, first + visible - 1 -
    // margin]`, which is these two bounds on `first`.
    let lowest = (cursor + margin + 1).saturating_sub(visible);
    let highest = cursor.saturating_sub(margin);
    // Clamp the *existing* position rather than recentring: the view only moves
    // when it has to, which is the whole feel this function is protecting.
    first.clamp(lowest, highest).min(rows - visible)
}

/// How many whole rows of `height` fit in `available` points.
pub fn visible_rows(available: f32, height: f32) -> usize {
    if height <= 0.0 || available <= 0.0 {
        return 0;
    }
    (available / height).floor().max(0.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The resting case: a directory shorter than the pane never scrolls.
    #[test]
    fn a_short_list_never_scrolls() {
        for cursor in 0..5 {
            assert_eq!(first_visible(0, cursor, 5, 20, 5), 0);
        }
    }

    /// The point of the whole function: moving inside the window moves nothing.
    #[test]
    fn the_view_holds_still_until_the_cursor_reaches_the_margin() {
        let (rows, visible, off) = (100, 20, 5);
        // Rows 0..=14 are reachable from the top without scrolling (row 14 is
        // the last one with five rows of context below it).
        for cursor in 0..=14 {
            assert_eq!(first_visible(0, cursor, rows, visible, off), 0, "{cursor}");
        }
        // One further and the view travels exactly one row.
        assert_eq!(first_visible(0, 15, rows, visible, off), 1);
        assert_eq!(first_visible(1, 16, rows, visible, off), 2);
    }

    /// …and symmetrically on the way back up.
    #[test]
    fn scrolling_up_keeps_the_same_margin() {
        let (rows, visible, off) = (100, 20, 5);
        // Viewing 40..59 with the cursor at 50: stepping up to 45 is the first
        // move that touches the top margin.
        assert_eq!(first_visible(40, 46, rows, visible, off), 40);
        assert_eq!(first_visible(40, 45, rows, visible, off), 40);
        assert_eq!(first_visible(40, 44, rows, visible, off), 39);
    }

    /// The margin must not make the ends unreachable: `G` has to land on the
    /// actual last row, with the view scrolled all the way down.
    #[test]
    fn the_cursor_still_reaches_both_ends() {
        let (rows, visible, off) = (100, 20, 5);
        assert_eq!(first_visible(0, 99, rows, visible, off), rows - visible);
        assert_eq!(first_visible(80, 0, rows, visible, off), 0);
    }

    /// A jump (`Ctrl+f`, a click, `g g`) lands wherever the bounds say, however
    /// far the view has to travel.
    #[test]
    fn a_long_jump_snaps_the_view_to_the_nearest_legal_position() {
        let (rows, visible, off) = (100, 20, 5);
        // From the top, straight to row 60: the view moves to put 60 five rows
        // above its bottom edge.
        assert_eq!(
            first_visible(0, 60, rows, visible, off),
            60 + off + 1 - visible
        );
        // …and back the other way.
        assert_eq!(first_visible(80, 10, rows, visible, off), 5);
    }

    /// A pane too short to hold two margins keeps the cursor centred instead of
    /// producing crossed bounds.
    #[test]
    fn a_tiny_pane_degrades_to_centring() {
        let (rows, visible, off) = (100, 3, 5);
        assert_eq!(first_visible(0, 50, rows, visible, off), 49);
        assert_eq!(first_visible(0, 0, rows, visible, off), 0);
        assert_eq!(first_visible(0, 99, rows, visible, off), 97);
        // Zero rows on screen is a pane mid-resize, not a crash.
        assert_eq!(first_visible(0, 0, 100, 0, 5), 0);
    }

    /// `scrolloff = 0` is a legal config: the cursor may sit on the edge row.
    #[test]
    fn no_scrolloff_means_the_cursor_can_touch_the_edge() {
        assert_eq!(first_visible(0, 19, 100, 20, 0), 0);
        assert_eq!(first_visible(0, 20, 100, 20, 0), 1);
    }

    #[test]
    fn rows_fit_by_whole_rows_only() {
        assert_eq!(visible_rows(100.0, 22.0), 4);
        assert_eq!(visible_rows(88.0, 22.0), 4);
        assert_eq!(visible_rows(21.0, 22.0), 0);
        assert_eq!(visible_rows(-5.0, 22.0), 0);
        assert_eq!(visible_rows(100.0, 0.0), 0);
    }
}
