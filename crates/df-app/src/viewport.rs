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

/// The cursor, dragged into the window a *view* move has settled on.
///
/// The inverse of [`first_visible`], and it exists for one caller: the mouse
/// wheel (PLAN §7.5), which moves the view rather than the cursor. Because
/// `first_visible` recomputes the view from the cursor on every frame, a wheel
/// roll that left the cursor behind would be undone before it was drawn — so
/// the cursor is pulled to the nearest row that satisfies the same margin the
/// keyboard obeys. Scrolling therefore *carries* the cursor, which is also what
/// yazi does, and means the row you stopped on is the row `Enter` opens.
pub fn cursor_in_view(
    first: usize,
    cursor: usize,
    rows: usize,
    visible: usize,
    scrolloff: usize,
) -> usize {
    if rows == 0 {
        return 0;
    }
    if visible == 0 || rows <= visible {
        // Nothing scrolls, so nothing may move the cursor.
        return cursor.min(rows - 1);
    }
    let margin = scrolloff.min((visible - 1) / 2);
    let last = rows - 1;
    let low = (first + margin).min(last);
    let high = (first + visible - 1).saturating_sub(margin).min(last);
    cursor.clamp(low.min(high), high)
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
        assert_eq!(first_visible(0, 60, rows, visible, off), 60 + off + 1 - visible);
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

    /// The wheel's half of the rule: a view move drags the cursor to the
    /// nearest row that keeps the margin, and the pair agree — feeding the
    /// result back through `first_visible` must leave the view where the wheel
    /// put it, or scrolling would fight itself every frame.
    #[test]
    fn a_view_move_drags_the_cursor_into_its_window() {
        let (rows, visible, off) = (100, 20, 5);
        // The cursor is above the new window: pulled down to the top margin.
        let cursor = cursor_in_view(40, 0, rows, visible, off);
        assert_eq!(cursor, 45);
        assert_eq!(first_visible(40, cursor, rows, visible, off), 40);
        // …and below it: pulled up to the bottom margin.
        let cursor = cursor_in_view(40, 99, rows, visible, off);
        assert_eq!(cursor, 54);
        assert_eq!(first_visible(40, cursor, rows, visible, off), 40);
        // Already inside: left exactly where it was.
        assert_eq!(cursor_in_view(40, 50, rows, visible, off), 50);

        // Scrolled to the very bottom, the cursor rests on the last row the
        // margin allows — the same place `↓` would stop it. The last *rows*
        // are on screen; reaching them is the keyboard's job, not the wheel's.
        let first = rows - visible;
        let cursor = cursor_in_view(first, 99, rows, visible, off);
        assert_eq!(cursor, 94);
        assert_eq!(first_visible(first, cursor, rows, visible, off), first);
    }

    /// A listing that fits has nothing to scroll, so the wheel must not move
    /// the cursor at all.
    #[test]
    fn a_short_listing_keeps_its_cursor() {
        assert_eq!(cursor_in_view(0, 3, 5, 20, 5), 3);
        assert_eq!(cursor_in_view(0, 0, 0, 20, 5), 0);
        assert_eq!(cursor_in_view(0, 7, 100, 0, 5), 7);
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
