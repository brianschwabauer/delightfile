//! How wide the three panes are: where the two dividers were left, and which
//! of the side panes is folded away.
//!
//! **Global, not per directory.** The panes are the window's shape, not a fact
//! about what is in them: a preview dragged wide to read a PDF stays wide when
//! the cursor walks into the next folder, the way a window keeps its size.
//! So it is one record, like the tab list, rather than a field on each
//! directory's line.
//!
//! **In the state file, not in config.** `ratio = [1, 4, 3]` in
//! `delightfile.toml` is the owner's own number and stays the panes' *home*:
//! the widths a reset goes back to and the positions the dividers snap to.
//! A drag is a keystroke's cousin — the hand made it, the program remembers
//! it — and the program never writes config (see [`super::pins`] for the
//! same argument about pins).
//!
//! # The format
//!
//! One line, after the tabs:
//!
//! ```text
//! !panes\tratio=0.125,0.5,0.375\tparent_collapsed=0\tpreview_collapsed=1\tparent_before=0.125\tpreview_before=0.3
//! ```
//!
//! `ratio` is each pane's share of the width the panes have between them —
//! the window less its margins and the two gaps, the width the config's ratio
//! has always divided — as fractions that sum to 1. A folded pane's share is
//! 0 and the list holds it, because the list is what grows into the space;
//! `*_before` is what the pane had when it was last open, so opening it again
//! gives back the width it was folded from rather than some default.
//!
//! A record that does not describe three panes filling the window — a share
//! that is not a number, a negative one, three that do not add up to one — is
//! dropped whole with a warning and the config's ratio is used. Half a record
//! is not a layout, and guessing which half was meant would be a window that
//! opens in a shape nobody made.

use super::{bool_bytes, push_field, split_field, text, StateStore};

/// The record key. `!` is not the start of an absolute path, so it cannot
/// collide with a directory's line — the rule `!tabs` and `!pin` rely on.
pub const PANES_KEY: &str = "!panes";

/// How far from 1 the three shares may add up and still be read as the whole
/// window. A file this crate wrote sums to 1 within `f32` printing; a sum
/// further off than this is a record something else wrote, or damaged.
const SUM_SLOP: f32 = 1e-3;

/// The two panes that fold away. The list does not: it is where the keyboard
/// is, and a window with no list in it is not a file manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Parent,
    Preview,
}

impl Side {
    /// Its place in [`Panes::ratio`].
    pub fn index(self) -> usize {
        match self {
            Side::Parent => 0,
            Side::Preview => 2,
        }
    }
}

/// The panes' widths, as the state file keeps them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Panes {
    /// Parent, list, preview: each pane's share of the width between them,
    /// summing to 1. A folded pane's is 0.
    pub ratio: [f32; 3],
    pub parent_collapsed: bool,
    pub preview_collapsed: bool,
    /// The parent's share when it was last open, which is what opening it
    /// again gives back. Equal to its share while it is open.
    pub parent_before: f32,
    pub preview_before: f32,
}

impl Panes {
    /// The config's ratio as shares of the window: `[1, 4, 3]` is
    /// `[0.125, 0.5, 0.375]`, both panes open.
    ///
    /// A side pane the ratio gives nothing to is folded, which is what a `0`
    /// there has always meant — a pane with no width — now that an open pane
    /// has a width it will not go below. An all-zero ratio is refused by the
    /// config parser; one reaching here anyway is the default's.
    pub fn from_ratio(ratio: [u16; 3]) -> Panes {
        let ratio = if ratio.iter().all(|r| *r == 0) {
            crate::config::DEFAULT_RATIO
        } else {
            ratio
        };
        let total: f32 = ratio.iter().map(|r| *r as f32).sum();
        let share = |r: u16| r as f32 / total;
        Panes {
            ratio: [share(ratio[0]), share(ratio[1]), share(ratio[2])],
            parent_collapsed: ratio[0] == 0,
            preview_collapsed: ratio[2] == 0,
            parent_before: share(ratio[0]),
            preview_before: share(ratio[2]),
        }
    }

    /// This record, if it describes three panes filling the window; `None`
    /// when it does not (see the module essay).
    ///
    /// What comes back is tidied as well as checked: the shares are scaled to
    /// sum to exactly 1, a folded pane's share is handed to the list, and an
    /// open pane's `before` is its share — so everything downstream can take
    /// those three things as given.
    pub fn validated(self) -> Option<Panes> {
        let shares_ok = self.ratio.iter().all(|r| r.is_finite() && *r >= 0.0);
        let before_ok = [self.parent_before, self.preview_before]
            .iter()
            .all(|b| b.is_finite() && (0.0..=1.0).contains(b));
        let total: f32 = self.ratio.iter().sum();
        if !shares_ok || !before_ok || (total - 1.0).abs() > SUM_SLOP {
            return None;
        }
        let mut panes = self;
        // Only when it is off by more than rounding: scaling a record that
        // already sums to 1 moves its last bit, and a record that changed on
        // every load would never read back as the one that was saved.
        if (total - 1.0).abs() > 1e-6 {
            for share in &mut panes.ratio {
                *share /= total;
            }
        }
        for side in [Side::Parent, Side::Preview] {
            let i = side.index();
            if panes.collapsed(side) {
                panes.ratio[1] += panes.ratio[i];
                panes.ratio[i] = 0.0;
            } else {
                *panes.before_mut(side) = panes.ratio[i];
            }
        }
        Some(panes)
    }

    pub fn collapsed(&self, side: Side) -> bool {
        match side {
            Side::Parent => self.parent_collapsed,
            Side::Preview => self.preview_collapsed,
        }
    }

    fn collapsed_mut(&mut self, side: Side) -> &mut bool {
        match side {
            Side::Parent => &mut self.parent_collapsed,
            Side::Preview => &mut self.preview_collapsed,
        }
    }

    /// The share `side` had when it was last open.
    pub fn before(&self, side: Side) -> f32 {
        match side {
            Side::Parent => self.parent_before,
            Side::Preview => self.preview_before,
        }
    }

    fn before_mut(&mut self, side: Side) -> &mut f32 {
        match side {
            Side::Parent => &mut self.parent_before,
            Side::Preview => &mut self.preview_before,
        }
    }

    /// Fold `side` away. Its share goes to the list and is remembered as
    /// what it opens back to.
    pub fn collapse(&mut self, side: Side) {
        if self.collapsed(side) {
            return;
        }
        let i = side.index();
        *self.before_mut(side) = self.ratio[i];
        self.ratio[1] += self.ratio[i];
        self.ratio[i] = 0.0;
        *self.collapsed_mut(side) = true;
    }

    /// Open `side` again at the share it was folded from — or, for a pane
    /// that was never open, at `home`'s — with the list paying for it. The
    /// other side pane keeps its width: opening one pane is not a reason to
    /// move the other.
    pub fn expand(&mut self, side: Side, home: &Panes) {
        if !self.collapsed(side) {
            return;
        }
        let i = side.index();
        let share = self.reopen_share(side, home);
        self.ratio[1] -= share;
        self.ratio[i] = share;
        *self.before_mut(side) = share;
        *self.collapsed_mut(side) = false;
    }

    /// What a folded `side` would open to: its `before`, or `home`'s share
    /// when it has none, and never more than the list has to give.
    ///
    /// Also what a pane is drawn *from* while it folds or unfolds: its width
    /// is this share scaled down by how open it is, so the fold is one number
    /// moving rather than a share and a gap moving on two clocks.
    pub fn reopen_share(&self, side: Side, home: &Panes) -> f32 {
        let before = self.before(side);
        let wanted = if before > 0.0 {
            before
        } else {
            home.ratio[side.index()].max(home.before(side))
        };
        wanted.min(self.ratio[1]).max(0.0)
    }

    /// Open `side` at `share`, trading with the list alone: what a divider
    /// that has been let go leaves behind. The two of them keep the width
    /// they had between them, so the pane on the far side of the list never
    /// moves, and the share is clamped to that width.
    pub fn open_at(&mut self, side: Side, share: f32) {
        let i = side.index();
        let pair = self.ratio[i] + self.ratio[1];
        let share = if share.is_finite() {
            share.clamp(0.0, pair)
        } else {
            self.ratio[i]
        };
        self.ratio[i] = share;
        self.ratio[1] = pair - share;
        *self.before_mut(side) = share;
        *self.collapsed_mut(side) = false;
    }
}

impl StateStore {
    /// The panes' widths as they were last left, or `None` when they never
    /// moved from the config's ratio (or the record was unreadable, which is
    /// the same answer).
    pub fn panes(&self) -> Option<Panes> {
        self.panes
    }

    /// Remember the panes' widths. A record that does not describe the whole
    /// window is refused with a warning rather than written: it would only be
    /// dropped again on the next load.
    pub fn set_panes(&mut self, panes: Panes) {
        let Some(panes) = panes.validated() else {
            log::warn!("state: refusing a pane record that does not fill the window: {panes:?}");
            return;
        };
        if self.panes != Some(panes) {
            self.panes = Some(panes);
            self.dirty = true;
        }
    }

    /// The panes' line, for [`StateStore::render`].
    pub(super) fn render_panes(&self, out: &mut Vec<u8>) {
        let Some(panes) = self.panes else { return };
        out.extend_from_slice(PANES_KEY.as_bytes());
        let [parent, list, preview] = panes.ratio;
        push_field(
            out,
            "ratio",
            format!("{parent},{list},{preview}").as_bytes(),
        );
        push_field(out, "parent_collapsed", bool_bytes(panes.parent_collapsed));
        push_field(
            out,
            "preview_collapsed",
            bool_bytes(panes.preview_collapsed),
        );
        push_field(
            out,
            "parent_before",
            panes.parent_before.to_string().as_bytes(),
        );
        push_field(
            out,
            "preview_before",
            panes.preview_before.to_string().as_bytes(),
        );
        out.push(b'\n');
    }

    /// The `!panes` line, for [`StateStore::parse`].
    ///
    /// An unknown field is a newer build's and is let go, as everywhere else
    /// in the file. A missing flag is an open pane and a missing `before` is
    /// none — opening that pane then takes the config's share — but a missing
    /// or unreadable ratio, or one that fails [`Panes::validated`], drops the
    /// record: see the module essay.
    pub(super) fn parse_panes<'a>(&mut self, fields: impl Iterator<Item = &'a [u8]>, line: usize) {
        let mut ratio: Option<[f32; 3]> = None;
        let mut panes = Panes {
            ratio: [0.0; 3],
            parent_collapsed: false,
            preview_collapsed: false,
            parent_before: 0.0,
            preview_before: 0.0,
        };
        let number = |value: &[u8]| text(value).trim().parse::<f32>().ok();
        for field in fields {
            let Some((name, value)) = split_field(field) else {
                log::warn!(
                    "state: {}:{line}: malformed pane field; skipped",
                    self.path.display()
                );
                continue;
            };
            match name.as_slice() {
                b"ratio" => {
                    let shares: Vec<Option<f32>> =
                        value.split(|b| *b == b',').map(number).collect();
                    ratio = match shares.as_slice() {
                        [Some(a), Some(b), Some(c)] => Some([*a, *b, *c]),
                        _ => None,
                    };
                }
                b"parent_collapsed" => panes.parent_collapsed = value.as_slice() == b"1",
                b"preview_collapsed" => panes.preview_collapsed = value.as_slice() == b"1",
                // Unreadable is NaN, which `validated` refuses: a number that
                // is there but is not a number is damage, not absence.
                b"parent_before" => panes.parent_before = number(&value).unwrap_or(f32::NAN),
                b"preview_before" => panes.preview_before = number(&value).unwrap_or(f32::NAN),
                _ => log::debug!(
                    "state: {}:{line}: unknown pane field {}",
                    self.path.display(),
                    text(&name)
                ),
            }
        }
        let Some(ratio) = ratio else {
            log::warn!(
                "state: {}:{line}: a pane record with no readable ratio; the config's is used",
                self.path.display()
            );
            return;
        };
        panes.ratio = ratio;
        match panes.validated() {
            Some(panes) => self.panes = Some(panes),
            None => log::warn!(
                "state: {}:{line}: the pane widths do not fill the window; the config's are used",
                self.path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;
    use crate::state::HEADER;

    fn store_at(tree: &TempTree) -> StateStore {
        StateStore::load_from(tree.join("state"))
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    /// The config's whole numbers become shares of the window, and a zero
    /// on a side is that side folded.
    #[test]
    fn a_config_ratio_becomes_shares() {
        let panes = Panes::from_ratio([1, 4, 3]);
        assert_eq!(panes.ratio, [0.125, 0.5, 0.375]);
        assert!(!panes.parent_collapsed && !panes.preview_collapsed);
        assert_eq!(panes.parent_before, 0.125);
        assert_eq!(panes.preview_before, 0.375);

        let bare = Panes::from_ratio([0, 1, 1]);
        assert_eq!(bare.ratio, [0.0, 0.5, 0.5]);
        assert!(bare.parent_collapsed);
        assert!(!bare.preview_collapsed);
        // All zeroes cannot come out of the parser; if it arrives anyway it
        // is the default, not a division by zero.
        assert_eq!(Panes::from_ratio([0, 0, 0]), Panes::from_ratio([1, 4, 3]));
    }

    /// Anything that is not three shares filling the window is refused, and
    /// what is accepted comes back tidied.
    #[test]
    fn a_record_that_does_not_fill_the_window_is_refused() {
        let good = Panes::from_ratio([1, 4, 3]);
        assert_eq!(good.validated(), Some(good));
        for ratio in [
            [f32::NAN, 0.5, 0.5],
            [f32::INFINITY, 0.0, 0.0],
            [-0.1, 0.6, 0.5],
            [0.2, 0.2, 0.2],
            [0.5, 0.5, 0.5],
        ] {
            assert_eq!(Panes { ratio, ..good }.validated(), None, "{ratio:?}");
        }
        for before in [f32::NAN, -0.1, 1.5] {
            let panes = Panes {
                preview_before: before,
                ..good
            };
            assert_eq!(panes.validated(), None, "before {before}");
        }
        // A sum a hair off 1 is rounding, and is scaled away.
        let nearly = Panes {
            ratio: [0.1251, 0.5, 0.375],
            ..good
        }
        .validated()
        .unwrap();
        assert!((nearly.ratio.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        // A folded pane's share is the list's, and an open pane's `before` is
        // its share, whatever the record said.
        let folded = Panes {
            ratio: [0.125, 0.5, 0.375],
            preview_collapsed: true,
            preview_before: 0.3,
            parent_before: 0.9,
            ..good
        }
        .validated()
        .unwrap();
        assert!(close(folded.ratio, [0.125, 0.875, 0.0]));
        assert_eq!(folded.preview_before, 0.3);
        assert_eq!(folded.parent_before, 0.125);
    }

    /// Folding hands the share to the list and remembers it; opening gives it
    /// back from the list, and the other side pane never moves.
    #[test]
    fn folding_and_opening_trade_only_with_the_list() {
        let home = Panes::from_ratio([1, 4, 3]);
        let mut panes = Panes {
            ratio: [0.2, 0.5, 0.3],
            ..home
        }
        .validated()
        .unwrap();
        panes.collapse(Side::Parent);
        assert!(close(panes.ratio, [0.0, 0.7, 0.3]));
        assert!(panes.parent_collapsed);
        assert_eq!(panes.parent_before, 0.2);
        // Folding twice is folding once.
        panes.collapse(Side::Parent);
        assert!(close(panes.ratio, [0.0, 0.7, 0.3]));
        // Folded, it is drawn from its old share.
        assert_eq!(panes.reopen_share(Side::Parent, &home), 0.2);
        panes.expand(Side::Parent, &home);
        assert!(close(panes.ratio, [0.2, 0.5, 0.3]));
        assert!(!panes.parent_collapsed);

        // A pane with no share to go back to takes home's.
        let mut bare = Panes::from_ratio([0, 1, 1]);
        bare.expand(Side::Parent, &home);
        assert!(close(bare.ratio, [0.125, 0.375, 0.5]));

        // Set by a divider: the pair keeps its width, and a share wider than
        // the pair is the pair.
        let mut dragged = Panes::from_ratio([1, 4, 3]);
        dragged.open_at(Side::Preview, 0.5);
        assert!(close(dragged.ratio, [0.125, 0.375, 0.5]));
        assert_eq!(dragged.preview_before, 0.5);
        dragged.open_at(Side::Parent, 2.0);
        assert!(close(dragged.ratio, [0.5, 0.0, 0.5]));
        // …and it opens a folded pane, which it is the only way to from a
        // drag.
        let mut folded = Panes::from_ratio([1, 4, 3]);
        folded.collapse(Side::Parent);
        folded.open_at(Side::Parent, 0.1);
        assert!(!folded.parent_collapsed);
        assert!(close(folded.ratio, [0.1, 0.525, 0.375]));
    }

    /// Written, read back, and equal — folded panes and all.
    #[test]
    fn the_panes_round_trip_through_the_file() {
        let tree = TempTree::new("state-panes-trip");
        let mut store = store_at(&tree);
        assert_eq!(store.panes(), None, "nothing until something moves");
        let mut panes = Panes {
            ratio: [0.1, 0.55, 0.35],
            ..Panes::from_ratio([1, 4, 3])
        }
        .validated()
        .unwrap();
        panes.collapse(Side::Preview);
        store.set_panes(panes);
        assert!(store.is_dirty());
        store.flush().unwrap();

        let back = store_at(&tree);
        assert_eq!(back.panes(), Some(panes));
        let text = String::from_utf8_lossy(&back.render()).into_owned();
        assert!(text.contains("\n!panes\tratio=0.1,"), "{text}");
        assert!(
            text.contains(",0\tparent_collapsed=0\tpreview_collapsed=1\t"),
            "{text}"
        );

        // Setting what is already there is not a change.
        let mut again = store_at(&tree);
        again.set_panes(panes);
        assert!(!again.is_dirty());
        // …and a record that is nonsense is not stored at all.
        again.set_panes(Panes {
            ratio: [f32::NAN, 0.0, 0.0],
            ..panes
        });
        assert!(!again.is_dirty());
        assert_eq!(again.panes(), Some(panes));
    }

    /// A file from before the panes were remembered loads with none, and one
    /// from a newer build — a field this one has never heard of, a record
    /// kind it has never heard of — loads with everything it can read.
    #[test]
    fn old_and_new_files_load() {
        let tree = TempTree::new("state-panes-versions");
        std::fs::write(
            tree.join("state"),
            format!("{HEADER}\n!tabs\t0=/tmp\tactive=0\tt=1\n"),
        )
        .unwrap();
        assert_eq!(store_at(&tree).panes(), None);

        std::fs::write(
            tree.join("state"),
            format!(
                "{HEADER}\n!panes\tratio=0.2,0.5,0.3\tsplit=vertical\tpreview_collapsed=0\n!future\tx=1\n"
            ),
        )
        .unwrap();
        let store = store_at(&tree);
        let panes = store.panes().expect("the record is read");
        assert!(close(panes.ratio, [0.2, 0.5, 0.3]));
        assert!(!panes.parent_collapsed, "a missing flag is an open pane");
    }

    /// A damaged record costs the record and nothing else.
    #[test]
    fn a_damaged_record_is_dropped_and_the_rest_loads() {
        let tree = TempTree::new("state-panes-damage");
        for line in [
            "!panes\tratio=0.2,0.5\tparent_collapsed=0",
            "!panes\tratio=0.2,x,0.3",
            "!panes\tratio=0.5,0.5,0.5",
            "!panes\tparent_collapsed=1",
            "!panes\tratio=0.2,0.5,0.3\tparent_before=wide",
        ] {
            std::fs::write(
                tree.join("state"),
                format!("{HEADER}\n{line}\n!tabs\t0=/tmp\tactive=0\tt=1\n"),
            )
            .unwrap();
            let store = store_at(&tree);
            assert_eq!(store.panes(), None, "{line}");
            assert_eq!(store.tabs().len(), 1, "{line}: the tabs still load");
        }
    }
}
