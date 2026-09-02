//! Several [`Tab`]s and which one you are looking at (PLAN §2).
//!
//! A tab is a whole browsing session — cwd, parent, cursor, view position and
//! history — so this file is deliberately thin: it owns a `Vec<Tab>`, an index,
//! and the arithmetic that moves that index around. That arithmetic is the part
//! that is easy to get subtly wrong (which tab is active after you close the
//! last one? what does `}` do on the rightmost tab?), so it is four free
//! functions with tests and no `Tab` in sight.
//!
//! ## No switch animation
//!
//! Switching tabs used to slide the panes in the direction of travel. It is
//! gone: a switch is something a person does repeatedly with `Alt+]` and `1`–
//! `9`, often several times in a second to find the tab they meant, and a
//! motion on every one of those turns the strip into something that has to be
//! waited out. The strip itself already says which tab is live, on the frame
//! the key lands. Motion is for things that *move* (`delightful-ui` §5); a tab
//! switch is a cut.

use std::path::PathBuf;
use std::time::Instant;

use df_core::config::MgrConfig;
use df_core::fs::{ScanUpdate, Scanner, SortOptions};

use crate::tab::Tab;

/// How many tabs there can be.
///
/// Nine, because `1`–`9` is the switch row (PLAN §4.1) and a tenth tab would be
/// the first one you cannot reach with a single keystroke. It is also about as
/// many titles as the strip can show before they stop being readable, which is
/// the same limit arrived at from the other direction.
pub const MAX_TABS: usize = 9;

/// The tab index `delta` steps away, wrapping. `Alt+[` is −1, `Alt+]` is +1.
pub fn cycled(len: usize, active: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let len_i = len as isize;
    let next = (active as isize + delta).rem_euclid(len_i);
    next as usize
}

/// Where `{` / `}` move the active tab to. Wrapping, like [`cycled`], so `}` on
/// the last tab brings it round to the front rather than doing nothing — a key
/// that is silently inert on one tab is a key you stop trusting.
pub fn swapped(len: usize, active: usize, delta: isize) -> usize {
    cycled(len, active, delta)
}

/// Which tab is active after closing the active one.
///
/// The tab to the *right* takes over — it has slid into the closed tab's place,
/// so the cursor stays where the eye already is — except at the end of the
/// strip, where there is no right and the new last tab takes over instead.
/// Only meaningful when there is more than one tab; closing the last tab quits
/// (PLAN §4.1's `Ctrl+c`).
pub fn after_close(len: usize, closing: usize) -> usize {
    let remaining = len.saturating_sub(1);
    closing.min(remaining.saturating_sub(1))
}

/// Which tab is active after closing *any* tab — the one being looked at or
/// another one (a tab dragged out of the strip, [`crate::window`]).
///
/// `len` is the count **before** the close. Closing the active tab is
/// [`after_close`]'s rule; closing one to the left of it shifts the index down
/// so the same tab stays on screen, which is the whole point — a chip
/// disappearing from the strip must not change what the window is showing.
pub fn active_after_close(len: usize, closing: usize, active: usize) -> usize {
    if closing == active {
        after_close(len, closing)
    } else if closing < active {
        active.saturating_sub(1)
    } else {
        active
    }
}

/// Where a new tab goes: immediately after the active one.
///
/// Not at the end. `t` opens a tab on the directory you are in, and the tab you
/// spawned from is the one you will switch back to — putting them next to each
/// other means `Alt+[` is the way back.
pub fn insert_position(active: usize) -> usize {
    active + 1
}

/// Every open tab, and the one on screen.
pub struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
}

impl Tabs {
    pub fn new(tab: Tab) -> Tabs {
        Tabs {
            tabs: vec![tab],
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &Tab {
        // `active` is maintained in range by every mutator here, and the vec is
        // never empty (closing the last tab quits instead).
        &self.tabs[self.active.min(self.tabs.len() - 1)]
    }

    pub fn active_mut(&mut self) -> &mut Tab {
        let index = self.active.min(self.tabs.len() - 1);
        &mut self.tabs[index]
    }

    pub fn iter(&self) -> impl Iterator<Item = &Tab> + '_ {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tab> + '_ {
        self.tabs.iter_mut()
    }

    /// Open a tab on `path` after the active one and switch to it. Refuses past
    /// [`MAX_TABS`]; the caller says so out loud.
    pub fn create(
        &mut self,
        path: PathBuf,
        mgr: &MgrConfig,
        sort: SortOptions,
        scanner: &Scanner,
        now: Instant,
    ) -> bool {
        if self.tabs.len() >= MAX_TABS {
            return false;
        }
        let at = insert_position(self.active).min(self.tabs.len());
        self.tabs
            .insert(at, Tab::open(path, mgr, sort, scanner, now));
        self.active = at;
        true
    }

    /// `1`–`9`, and a click on the strip. Out-of-range is a no-op — pressing
    /// `7` with three tabs open should do nothing, not land on the last one.
    pub fn switch_to(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// `Alt+[` / `Alt+]`.
    pub fn cycle(&mut self, delta: isize) -> bool {
        let next = cycled(self.tabs.len(), self.active, delta);
        if next == self.active {
            return false;
        }
        self.active = next;
        true
    }

    /// `{` / `}` — move the active tab, and keep looking at it.
    pub fn swap(&mut self, delta: isize) -> bool {
        let target = swapped(self.tabs.len(), self.active, delta);
        if target == self.active {
            return false;
        }
        self.tabs.swap(self.active, target);
        self.active = target;
        true
    }

    /// `Ctrl+c`. Returns whether a tab is still open — `false` means this was
    /// the last one and the app should quit (PLAN §4.1).
    pub fn close_active(&mut self) -> bool {
        self.close(self.active)
    }

    /// Close any tab, active or not — what a tab dragged out of the strip
    /// leaves behind (PLAN §2, [`crate::window`]).
    ///
    /// Closing a tab to the *left* of the active one shifts the active index
    /// down by one so the same tab stays on screen: the window you are looking
    /// at must not change because a chip beside it went away. Closing the
    /// active one falls back to [`after_close`], which is the rule `Ctrl+c`
    /// already uses. Returns whether a tab is still open — `false` means this
    /// was the last one and nothing was closed.
    pub fn close(&mut self, index: usize) -> bool {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return false;
        }
        self.active = active_after_close(self.tabs.len(), index, self.active);
        self.tabs.remove(index);
        true
    }

    /// Route a scan update to whichever tab asked for it.
    ///
    /// Every tab, not just the active one: a tab opened a moment ago is still
    /// loading in the background, and its batches have to land or switching to
    /// it would show an empty directory that never fills.
    pub fn apply(&mut self, update: &ScanUpdate) -> bool {
        self.tabs.iter_mut().any(|tab| tab.apply(update))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycling_wraps_in_both_directions() {
        assert_eq!(cycled(3, 0, 1), 1);
        assert_eq!(cycled(3, 2, 1), 0);
        assert_eq!(cycled(3, 0, -1), 2);
        assert_eq!(cycled(3, 1, -1), 0);
        // One tab has nowhere to go, and zero tabs must not divide by zero.
        assert_eq!(cycled(1, 0, 1), 0);
        assert_eq!(cycled(0, 0, 1), 0);
    }

    /// `}` on the last tab brings it round to the front — a key that did
    /// nothing there would be one you stop reaching for.
    #[test]
    fn swapping_wraps_too() {
        assert_eq!(swapped(4, 3, 1), 0);
        assert_eq!(swapped(4, 0, -1), 3);
        assert_eq!(swapped(4, 1, 1), 2);
    }

    /// The tab to the right takes over, except at the end of the strip.
    #[test]
    fn closing_activates_the_tab_that_took_its_place() {
        assert_eq!(after_close(3, 0), 0);
        assert_eq!(after_close(3, 1), 1);
        assert_eq!(after_close(3, 2), 1, "the last tab hands over to the left");
        assert_eq!(after_close(2, 1), 0);
        assert_eq!(after_close(2, 0), 0);
    }

    /// Dragging a tab out of the strip closes a tab that is not necessarily
    /// the active one, and the window must go on showing what it was showing.
    #[test]
    fn detaching_another_tab_leaves_the_view_where_it_was() {
        // Four tabs, looking at 'c' (index 2).
        let mut order = vec!['a', 'b', 'c', 'd'];
        let mut active = 2usize;

        // Drag 'a' out: the view stays on 'c', now one place to the left.
        let before = order.len();
        active = active_after_close(before, 0, active);
        order.remove(0);
        assert_eq!(order[active], 'c');

        // Drag 'd' out: nothing to the left of the cursor moved.
        let before = order.len();
        active = active_after_close(before, 2, active);
        order.remove(2);
        assert_eq!(order[active], 'c');

        // Drag out the one being looked at: the tab to its right takes over.
        let before = order.len();
        active = active_after_close(before, active, active);
        order.remove(1);
        assert_eq!(order, vec!['b']);
        assert_eq!(order[active], 'b');
    }

    /// A new tab lands beside the one it was spawned from, so `Alt+[` is the
    /// way back to it.
    #[test]
    fn a_new_tab_lands_next_to_its_parent() {
        assert_eq!(insert_position(0), 1);
        assert_eq!(insert_position(4), 5);
    }

    /// The ordering ops, run against a real `Vec` the way `Tabs` does, to pin
    /// that the index arithmetic and the vector agree.
    #[test]
    fn the_ordering_ops_agree_with_the_vector() {
        let mut order = vec!['a', 'b', 'c', 'd'];
        let mut active = 1usize; // 'b'

        // `}` twice moves 'b' rightwards and follows it.
        let target = swapped(order.len(), active, 1);
        order.swap(active, target);
        active = target;
        assert_eq!(order, vec!['a', 'c', 'b', 'd']);
        assert_eq!(order[active], 'b');

        // Closing it leaves 'd' in its place, and active points at 'd'.
        let before = order.len();
        order.remove(active);
        active = after_close(before, active);
        assert_eq!(order, vec!['a', 'c', 'd']);
        assert_eq!(order[active], 'd');

        // …and closing the last one steps left.
        let before = order.len();
        order.remove(active);
        active = after_close(before, active);
        assert_eq!(order, vec!['a', 'c']);
        assert_eq!(order[active], 'c');
    }
}
