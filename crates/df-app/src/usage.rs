//! "What's big" mode — PLAN §7.3's ncdu replacement, as a state of the list
//! rather than as a screen of its own.
//!
//! ## Why it is a mode and not a view
//!
//! ncdu is a separate program with its own cursor, its own keys and its own
//! idea of what a directory is. Everything it shows, though, is one extra
//! *number* per row: how much is under this directory. So the mode here adds
//! that number and nothing else — the rows stay the list's rows, `↑`/`↓` stay
//! the list's keys, `→` still walks in, `d` still trashes what the cursor is on,
//! and `Esc` leaves. What changes is three things:
//!
//! 1. Directory rows get a real recursive size, so the size column and the size
//!    sort stop treating every folder as weighing nothing.
//! 2. The right-hand column becomes a bar plus that size.
//! 3. The list sorts by size, descending — which *is* the drill-down, because
//!    the thing eating the disk is now the first row.
//!
//! Leaving restores the sort that was there before, and re-reads the directory
//! so the sizes go back to the honest zero they started as.
//!
//! ## Numbers that are still counting
//!
//! A walk of `~` takes seconds, and the whole value of the mode is watching the
//! big things surface while it runs. So a row whose subtree is not yet counted
//! is marked with a `≈` and its number keeps climbing; the mark comes off the
//! moment that subtree is done. Both facts come straight from
//! [`DuUpdate::done`], which is exactly the distinction the walker already
//! makes.
//!
//! The bar is the one animated part. It eases from nothing to its share when
//! the mode opens (`delightful-ui` §5: intro motion on the palette's own out-quint) and then
//! follows the number without a second animation — a bar that re-animated on
//! every 100 ms update would be a column of twitching, not a measurement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::du::{DuToken, DuUpdate};
use df_core::fs::SortOptions;

/// How long the bars take to grow in when the mode opens.
///
/// 420 ms — slower than the list's own 160 ms scroll, because this is content
/// *arriving* rather than a view moving, and an intro should be the slower of
/// the two (`delightful-ui` §5). Long enough to read as a sweep down the
/// column, short enough that the answer is on screen before the walk has got
/// far.
pub const GROW: Duration = Duration::from_millis(420);

/// How much of [`GROW`] each row further down the pane waits before starting.
///
/// The stagger is in *rows*, so the sweep reads top to bottom in reading order
/// and the whole column is still settled well inside [`GROW`] plus a few of
/// these. Deliberately tiny: this is a measurement, not a title sequence.
pub const STAGGER: Duration = Duration::from_millis(14);

/// The largest number of rows the stagger applies to, so a 10 000-row directory
/// does not have a bar that starts growing two minutes in.
pub const STAGGER_ROWS: usize = 24;

/// One child's recursive size, and whether it is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weight {
    pub bytes: u64,
    /// `false` while the subtree is still being counted — the `≈` rows.
    pub settled: bool,
}

/// The mode, while it is on.
pub struct Usage {
    /// The directory it is about. Navigating anywhere else ends it: a walk of
    /// somewhere you have left is work nobody is looking at (PLAN §1).
    pub dir: PathBuf,
    /// The walk this mode is reading. Messages carrying any other token are
    /// from a walk that has been superseded and are dropped.
    pub token: DuToken,
    /// Per immediate child, by name — the key the rows are matched on, because
    /// a name is what a listing has and a path is what the walker sends.
    weights: HashMap<String, Weight>,
    /// The directory's own total: the bars' denominator.
    total: u64,
    /// Whether the whole walk has finished.
    pub done: bool,
    /// When the mode opened, for the grow-in.
    pub since: Instant,
    /// The sort the list had before, put back on the way out.
    pub previous_sort: SortOptions,
}

impl Usage {
    pub fn new(dir: PathBuf, token: DuToken, previous_sort: SortOptions, now: Instant) -> Usage {
        Usage {
            dir,
            token,
            weights: HashMap::new(),
            total: 0,
            done: false,
            since: now,
            previous_sort,
        }
    }

    /// Take a batch of updates. Returns whether anything on screen changed.
    ///
    /// Only depth 0 and depth 1 are kept. Deeper directories are counted — that
    /// is what makes the depth-1 numbers true — but nothing on screen is about
    /// them, and storing a hundred thousand of them would be a megabyte of map
    /// nobody reads.
    pub fn apply(&mut self, updates: &[DuUpdate]) -> bool {
        let mut changed = false;
        for update in updates {
            if update.depth == 0 {
                if self.total != update.total_bytes {
                    self.total = update.total_bytes;
                    changed = true;
                }
                continue;
            }
            if update.depth != 1 {
                continue;
            }
            let Some(name) = update.dir.file_name().map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            let weight = Weight {
                bytes: update.total_bytes,
                settled: update.done,
            };
            if self.weights.get(&name) != Some(&weight) {
                self.weights.insert(name, weight);
                changed = true;
            }
        }
        changed
    }

    /// Mark the walk finished. Every row that is still `≈` settles with it —
    /// a directory the walk never reported is one it found nothing in, and it
    /// weighs what it weighs.
    pub fn finish(&mut self, total: u64) {
        self.done = true;
        self.total = self.total.max(total);
        for weight in self.weights.values_mut() {
            weight.settled = true;
        }
    }

    /// What is known about one row, by name.
    ///
    /// Files are not in the map — the walker reports directories — so a file
    /// row's weight is its own length, and it is settled from the start.
    pub fn weight(&self, name: &str) -> Option<Weight> {
        self.weights.get(name).copied()
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// A row's share of the directory, `0.0..=1.0`.
    ///
    /// Of the **total**, not of the largest child, and the difference matters:
    /// against the largest, a directory with one dominant folder draws every
    /// other bar as a sliver of a sliver and the column stops being readable as
    /// proportions. Against the total, a full bar means "this is the directory"
    /// — which is the sentence the mode exists to say.
    pub fn fraction(&self, bytes: u64) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (bytes as f64 / self.total as f64).clamp(0.0, 1.0) as f32
    }

    /// How far through its grow-in the bar on row `index` is, `0.0..=1.0`.
    pub fn growth(&self, index: usize, now: Instant) -> f32 {
        let delay = STAGGER * index.min(STAGGER_ROWS) as u32;
        let elapsed = now.saturating_duration_since(self.since);
        let Some(after) = elapsed.checked_sub(delay) else {
            return 0.0;
        };
        let t = (after.as_secs_f32() / GROW.as_secs_f32()).clamp(0.0, 1.0);
        crate::motion::Easing::OutQuint.apply(t)
    }

    /// Whether any bar is still growing — the one thing here that asks for
    /// frames, and it stops asking as soon as the sweep has landed (PLAN §1).
    pub fn animating(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) < GROW + STAGGER * STAGGER_ROWS as u32
    }

    /// Whether this mode is still about the directory on screen.
    pub fn is_about(&self, dir: &Path) -> bool {
        self.dir == dir
    }
}

/// What one row's usage column draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowUsage {
    /// Share of the directory, before the grow-in is applied.
    pub fraction: f32,
    /// The number beside the bar.
    pub bytes: u64,
    /// Still counting: the number gets a `≈`, because a size that is going to
    /// change must not be readable as one that is not.
    pub estimate: bool,
    /// How far the bar has grown, `0.0..=1.0`.
    pub growth: f32,
}

impl RowUsage {
    /// The width of the filled part, as a fraction of the track.
    pub fn filled(&self) -> f32 {
        (self.fraction * self.growth).clamp(0.0, 1.0)
    }

    /// The text beside the bar.
    pub fn label(&self) -> String {
        let size = crate::format::human_size(self.bytes);
        if self.estimate {
            format!("≈{size}")
        } else {
            size
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(dir: &str, depth: usize, bytes: u64, done: bool) -> DuUpdate {
        DuUpdate {
            dir: PathBuf::from(dir),
            depth,
            total_bytes: bytes,
            apparent_bytes: bytes,
            files: 1,
            dirs: 0,
            done,
        }
    }

    fn usage() -> Usage {
        Usage::new(
            PathBuf::from("/home/b"),
            DuToken(1),
            SortOptions::default(),
            Instant::now(),
        )
    }

    /// The stream overwrites by name and only the two depths that are on screen
    /// are kept.
    #[test]
    fn running_totals_overwrite_and_only_the_visible_depths_are_kept() {
        let mut u = usage();
        assert!(u.apply(&[
            update("/home/b", 0, 500, false),
            update("/home/b/Work", 1, 300, false),
            update("/home/b/Work/deep", 2, 300, true),
        ]));
        assert_eq!(u.total(), 500);
        assert_eq!(
            u.weight("Work"),
            Some(Weight {
                bytes: 300,
                settled: false
            })
        );
        // Depth 2 is counted but not stored: nothing on screen is about it.
        assert_eq!(u.weight("deep"), None);

        // A second, larger running total for the same child replaces it.
        assert!(u.apply(&[update("/home/b/Work", 1, 420, true)]));
        assert_eq!(
            u.weight("Work"),
            Some(Weight {
                bytes: 420,
                settled: true
            })
        );
        // The same numbers again change nothing, so nothing repaints.
        assert!(!u.apply(&[update("/home/b/Work", 1, 420, true)]));
    }

    /// Every `≈` comes off when the walk finishes, including on rows the walk
    /// never had anything more to say about.
    #[test]
    fn finishing_settles_every_row() {
        let mut u = usage();
        u.apply(&[
            update("/home/b/a", 1, 10, false),
            update("/home/b/c", 1, 20, false),
        ]);
        assert!(u.weight("a").is_some_and(|w| !w.settled));
        u.finish(30);
        assert!(u.done);
        assert!(u.weight("a").is_some_and(|w| w.settled));
        assert!(u.weight("c").is_some_and(|w| w.settled));
        assert_eq!(u.total(), 30);
    }

    /// Shares are of the total, and an empty directory is zero rather than a
    /// `NaN` that would reach a layout function as a bar of width nothing.
    #[test]
    fn fractions_are_of_the_total_and_never_nan() {
        let mut u = usage();
        assert_eq!(u.fraction(100), 0.0, "no total yet");
        u.apply(&[update("/home/b", 0, 1000, true)]);
        assert!((u.fraction(250) - 0.25).abs() < 1e-6);
        assert!((u.fraction(1000) - 1.0).abs() < 1e-6);
        // A child that somehow outweighs the parent still draws a full bar, not
        // one that overflows the track.
        assert!((u.fraction(4000) - 1.0).abs() < 1e-6);
    }

    /// The sweep runs top to bottom, is bounded, and finishes.
    #[test]
    fn the_bars_grow_in_reading_order_and_then_stop() {
        let start = Instant::now();
        let u = Usage::new(
            PathBuf::from("/x"),
            DuToken(1),
            SortOptions::default(),
            start,
        );
        assert_eq!(u.growth(0, start), 0.0);
        // A row further down starts later, which is what makes it a sweep.
        let mid = start + Duration::from_millis(60);
        assert!(u.growth(0, mid) > u.growth(4, mid));
        // …and everything lands.
        let after = start + GROW + STAGGER * STAGGER_ROWS as u32 + Duration::from_millis(1);
        assert_eq!(u.growth(0, after), 1.0);
        assert_eq!(u.growth(1000, after), 1.0);
        assert!(!u.animating(after), "a settled column asks for no frames");
        assert!(u.animating(mid));
    }

    /// The column's two halves: the bar's width, and the number beside it.
    #[test]
    fn a_row_draws_a_bar_and_a_number() {
        let row = RowUsage {
            fraction: 0.5,
            bytes: 2048,
            estimate: true,
            growth: 0.5,
        };
        assert!((row.filled() - 0.25).abs() < 1e-6);
        assert_eq!(row.label(), "≈2.0 KB");
        let settled = RowUsage {
            estimate: false,
            growth: 1.0,
            ..row
        };
        assert_eq!(settled.label(), "2.0 KB");
        assert!((settled.filled() - 0.5).abs() < 1e-6);
    }
}
