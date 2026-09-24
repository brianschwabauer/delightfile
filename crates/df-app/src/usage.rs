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
//!
//! ## Numbers that are there before the walk is
//!
//! The walk is the accurate answer, but it does not have to be the first one.
//! The size column has usually been through here already, and the cache it
//! filled remembers what each child weighed. That number may be stale, or
//! partly folded in from other walks, and it is still far closer than the empty
//! column the mode used to open on. So the mode opens seeded ([`Usage::seed`]).
//! Every remembered child starts as an unsettled weight wearing its `≈`, the
//! rows sort on those estimates in the same movement that opens the mode, and
//! the walk corrects them in place.
//!
//! Two rules keep the correction from reading as a reset. A running total only
//! ever raises an estimate: the walk climbs from zero, and a bar that collapsed
//! to nothing and regrew while it did would be the column forgetting something
//! it knew a moment ago. A `done` total replaces the estimate outright, larger
//! or smaller, because that is the walk's answer and not its progress. The
//! directory's own total follows the same two rules, and the walk's final total
//! beats any seed.
//!
//! The walk leans on the cache as well
//! ([`df_core::du::DuOptions::reusing_cache`]). A subtree counted recently and
//! unchanged since is folded in whole instead of being counted again, which is
//! what the size column already does, and what makes opening the mode on a
//! parent you have just come up from nearly free. Such a subtree arrives as one
//! `done` update, so its row settles at once on the remembered number. Only
//! counted records are reused, never approximate ones, so an estimate is never
//! built on another estimate (see `DuCache::reusable_under`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::du::{ChildTotal, DuToken, DuUpdate};
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
    /// `false` while the subtree is still being counted, or while the number
    /// is still the cache's from before the walk reached it — the `≈` rows.
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

    /// Fill in from what the cache remembers, before the walk has said
    /// anything. Returns whether anything on screen changed.
    ///
    /// `children` is [`df_core::du::DuScanner::remembered_children`], which
    /// already prefers a child's own newer record over what its parent's last
    /// walk believed. `total` is the directory's own remembered total, if it
    /// has one, however old. When it has none, the denominator is put together
    /// from the parts: the remembered children plus `files_bytes`, the listing's
    /// own non-directory rows, which the caller has and this does not. A rough
    /// denominator draws bars of roughly the right length. A zero draws none at
    /// all, and a column of empty tracks is the thing this exists to replace.
    ///
    /// **A seed is never settled**, even when the record it came from is fresh.
    /// Only the walk settles a row, by reporting it `done` or by finishing, so a
    /// row without its `≈` always means the walk has spoken for it. For the
    /// same reason a seed never lands on top of the walk. A row the walk has
    /// already settled keeps its number, a row it is still counting keeps the
    /// larger of the two, and a mode whose walk has finished takes no seed.
    pub fn seed(&mut self, children: &[ChildTotal], total: Option<u64>, files_bytes: u64) -> bool {
        if self.done {
            return false;
        }
        let mut changed = false;
        let mut summed = files_bytes;
        for child in children {
            let bytes = child.totals.total_bytes;
            summed = summed.saturating_add(bytes);
            let weight = match self.weights.get(&child.name) {
                Some(existing) if existing.settled => continue,
                Some(existing) => Weight {
                    bytes: existing.bytes.max(bytes),
                    settled: false,
                },
                None => Weight {
                    bytes,
                    settled: false,
                },
            };
            if self.weights.insert(child.name.clone(), weight) != Some(weight) {
                changed = true;
            }
        }
        let total = total.unwrap_or(summed);
        if total > self.total {
            self.total = total;
            changed = true;
        }
        changed
    }

    /// Take a batch of updates. Returns whether anything on screen changed.
    ///
    /// Only depth 0 and depth 1 are kept. Deeper directories are counted — that
    /// is what makes the depth-1 numbers true — but nothing on screen is about
    /// them, and storing a hundred thousand of them would be a megabyte of map
    /// nobody reads.
    ///
    /// A running total never lowers a number that is still an estimate. The
    /// walk counts every subtree up from zero, so without this a seeded bar
    /// (see [`Usage::seed`]) would drop to nothing the moment the walk first
    /// mentioned its directory and then climb back to where it started. The
    /// walk's `done` number is its answer, not its progress, so it replaces
    /// whatever is there, smaller or larger, and settles the row. A walk's own
    /// running totals only ever grow, so for a row that was never seeded the
    /// larger of the two is simply the newer one.
    pub fn apply(&mut self, updates: &[DuUpdate]) -> bool {
        let mut changed = false;
        for update in updates {
            if update.depth == 0 {
                // The same rule for the denominator: a seeded total holds until
                // the walk has counted past it or has finished counting.
                let total = if update.done {
                    update.total_bytes
                } else {
                    self.total.max(update.total_bytes)
                };
                if self.total != total {
                    self.total = total;
                    changed = true;
                }
                continue;
            }
            if update.depth != 1 {
                continue;
            }
            let Some(name) = update
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            let weight = if update.done {
                Weight {
                    bytes: update.total_bytes,
                    settled: true,
                }
            } else {
                let estimate = self
                    .weights
                    .get(&name)
                    .filter(|weight| !weight.settled)
                    .map_or(0, |weight| weight.bytes);
                Weight {
                    bytes: update.total_bytes.max(estimate),
                    settled: false,
                }
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
    ///
    /// The total is the walk's, exactly. It used to be the larger of the two,
    /// which was harmless while every number here came from this walk, and is
    /// wrong now that a seed can come from a record older than a big delete.
    /// The finished walk is the one number in the mode that is not an estimate,
    /// so it wins even when it is the smaller.
    ///
    /// A seeded row the walk never mentioned settles too, on the number the
    /// cache had for it. The walk reports every child it descends into, so
    /// that is a child it would not enter: one on another filesystem, or one
    /// deleted since the cache saw it, which has no row left to draw on.
    pub fn finish(&mut self, total: u64) {
        self.done = true;
        self.total = total;
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
            entries: 0,
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

    /// One child as the cache remembers it.
    fn child(name: &str, bytes: u64, fresh: bool) -> ChildTotal {
        ChildTotal {
            name: name.to_string(),
            totals: df_core::du::DuTotals {
                total_bytes: bytes,
                apparent_bytes: bytes,
                files: 1,
                dirs: 1,
            },
            fresh,
        }
    }

    fn estimate(bytes: u64) -> Option<Weight> {
        Some(Weight {
            bytes,
            settled: false,
        })
    }

    fn settled(bytes: u64) -> Option<Weight> {
        Some(Weight {
            bytes,
            settled: true,
        })
    }

    /// The mode opens with the cache's numbers already on the rows, every one
    /// of them an estimate, and a denominator for the bars to be a share of.
    #[test]
    fn seeding_puts_the_remembered_numbers_up_as_estimates() {
        let mut u = usage();
        assert!(u.seed(
            &[child("Work", 300, true), child("Music", 100, false)],
            Some(1000),
            0,
        ));
        // Fresh or stale, a seed wears its `≈`: only the walk settles a row.
        assert_eq!(u.weight("Work"), estimate(300));
        assert_eq!(u.weight("Music"), estimate(100));
        assert_eq!(u.total(), 1000, "the directory's own remembered total");
        assert!(
            (u.fraction(300) - 0.3).abs() < 1e-6,
            "the bars have a share"
        );
        assert!(
            !u.seed(&[child("Work", 300, true)], Some(1000), 0),
            "the same seed again changes nothing, so nothing repaints"
        );
    }

    /// With no record for the directory itself, the denominator is the parts
    /// the caller can name: the remembered children and the listing's files.
    #[test]
    fn a_seed_with_no_remembered_total_adds_up_the_children_and_the_files() {
        let mut u = usage();
        u.seed(&[child("a", 300, true), child("b", 200, false)], None, 50);
        assert_eq!(u.total(), 550);
        // Nothing remembered at all is still no denominator, not a bogus one.
        let mut empty = usage();
        assert!(!empty.seed(&[], None, 0));
        assert_eq!(empty.total(), 0);
    }

    /// The walk climbs from zero. Until it has counted past a seed, or has
    /// finished counting, the seed stands, so the bar does not collapse and
    /// regrow as the walk works its way up to it.
    #[test]
    fn a_running_total_below_a_seed_leaves_the_seed_standing() {
        let mut u = usage();
        u.seed(&[child("Work", 300, false)], Some(1000), 0);
        assert!(
            !u.apply(&[
                update("/home/b", 0, 400, false),
                update("/home/b/Work", 1, 120, false),
            ]),
            "nothing on screen moved"
        );
        assert_eq!(u.weight("Work"), estimate(300));
        assert_eq!(u.total(), 1000);

        // Past the seed, the running number is the better estimate.
        assert!(u.apply(&[
            update("/home/b", 0, 1200, false),
            update("/home/b/Work", 1, 350, false),
        ]));
        assert_eq!(u.weight("Work"), estimate(350));
        assert_eq!(u.total(), 1200);
    }

    /// A subtree the walk has finished is its answer, not its progress: it
    /// replaces the seed even when smaller, and the `≈` comes off.
    #[test]
    fn a_finished_subtree_replaces_its_seed_and_settles() {
        let mut u = usage();
        u.seed(&[child("Work", 300, true)], Some(1000), 0);
        assert!(u.apply(&[update("/home/b/Work", 1, 120, true)]));
        assert_eq!(u.weight("Work"), settled(120));
        // And the walk's word stays put: a late seed does not unsettle it.
        assert!(!u.seed(&[child("Work", 300, true)], None, 0));
        assert_eq!(u.weight("Work"), settled(120));
    }

    /// A seeded total from before a big delete is larger than the directory
    /// is now. The finished walk's total wins, and so does the root's own
    /// finished update ahead of it.
    #[test]
    fn the_walks_final_total_beats_a_seed_that_was_too_large() {
        let mut u = usage();
        u.seed(&[child("gone", 4000, false)], Some(5000), 0);
        u.apply(&[update("/home/b", 0, 800, false)]);
        assert_eq!(u.total(), 5000, "still counting: the seed stands");
        u.apply(&[update("/home/b", 0, 900, true)]);
        assert_eq!(u.total(), 900, "the root is done counting");
        u.finish(900);
        assert_eq!(u.total(), 900);
        assert_eq!(
            u.weight("gone"),
            settled(4000),
            "a row the walk never mentioned settles with it"
        );

        // `finish` on its own, with no root update ahead of it, wins as well.
        let mut v = usage();
        v.seed(&[], Some(5000), 0);
        v.finish(900);
        assert_eq!(v.total(), 900);
        assert!(!v.seed(&[child("late", 10, true)], Some(9000), 0));
        assert_eq!(v.weight("late"), None, "a finished mode takes no seed");
        assert_eq!(v.total(), 900);
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
