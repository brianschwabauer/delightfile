//! Folder sizes in the size column, on a best-effort basis (PLAN §7.3).
//!
//! [`df_core::fs::Entry::len`] is zero for a directory because nothing cheap
//! knows the answer, and the column has therefore drawn an em dash for every
//! folder since the list existed. Half the rows in a home directory say nothing,
//! and a size *sort* puts every folder in one indistinguishable block at the
//! end. That is the gap this closes.
//!
//! ## Two answers, in the order they can be had
//!
//! 1. **`12 items`** — one `read_dir` per row, which is microseconds. It lands
//!    within a frame or two of entering a directory and is a true, useful thing
//!    to say. It is not a size and does not pretend to be.
//! 2. **`~4.2 MB`, then `4.2 MB`** — the recursive walk, streaming. The `~`
//!    means *this subtree is still being counted and the number will only go
//!    up*; it comes off the moment the walk settles that row.
//!
//! Neither one blocks: both come off [`df_core::du::DuScanner`]'s worker pool,
//! which nices itself out of the UI thread's way ([`df_core::nice`]), and both
//! arrive as messages the app drains on its own frame.
//!
//! ## Where the numbers live
//!
//! The bytes are pushed into `Entry::len` itself — the same thing "what's big"
//! mode does, and for the same reason: that field is what the size *sort* reads,
//! so a folder full of video sorts as a folder full of video for free, and the
//! spot panel agrees with the column because there is one number.
//!
//! What stays here is the part `Entry::len` cannot carry: whether a row is
//! settled, and the child count for the rows the walk has not reached. A `u64`
//! has no room for "still counting".
//!
//! ## What is deliberately not measured
//!
//! - **Remote, archive and trash listings.** A `read_dir` of an sftp path is a
//!   round trip and a walk of one is a thousand; a size column is not worth a
//!   minute of somebody's network. The app gates on
//!   [`crate::tab::Tab::virtual_kind`] rather than this module knowing what a
//!   URL is.
//! - **Other filesystems.** [`df_core::du::DuOptions`]'s default, which is
//!   `du -x`: entering `/` must not walk a 4 TB backup drive under `/mnt`.
//! - **Anything, when `[mgr] folder_sizes = false`.** The escape hatch for the
//!   machine where `~` is an NFS mount.
//!
//! ## Staleness, and why it is the right trade
//!
//! A remembered size is only as fresh as the directory's own `mtime`, which
//! cannot see a file written three levels down — [`df_core::du::DuCache`] says
//! so at length. So a revisited folder shows the number from last time, plainly,
//! while the walk re-runs behind it and corrects it. Showing a slightly stale
//! size instantly beats showing a dash for two seconds and then the same number.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::du::{ChildCount, DuRecord, DuToken, DuUpdate};

/// How long a directory has to hold still before its sizes are walked again.
///
/// A build, an `rsync`, an unpack: the watcher fires dozens of times a second
/// and each event says "these numbers are stale". Re-walking on each one meant
/// a churning directory was walked from scratch forever and never showed a
/// number at all. Two seconds of quiet is the debounce, and the previous sizes
/// stay on screen wearing their `~` in the meantime — which is exactly what the
/// `~` means: still counting, and only going to change.
pub const RESTALE_QUIET: Duration = Duration::from_secs(2);

/// One directory's recursive size, and whether it is final.
///
/// Deliberately the same shape as [`crate::usage::Weight`] and deliberately not
/// the same type: the two are filled from the same stream but mean different
/// things on screen — that one is a bar's numerator, this one is a column's
/// text — and sharing the type would be an invitation to share the code that
/// draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub bytes: u64,
    /// `false` while the subtree is still being counted — the `~` rows.
    pub settled: bool,
}

/// What is known about the directories in the listing on screen.
#[derive(Debug, Default)]
pub struct Folders {
    /// The directory this is about. Empty when nothing is being measured.
    dir: PathBuf,
    /// The walk being read. Messages carrying any other token are from a walk
    /// that has been superseded — by another folder, or by the "what's big"
    /// mode taking the same root for itself.
    token: Option<DuToken>,
    /// Which tab this is about.
    ///
    /// Two tabs open on the same directory are two listings, and one of them
    /// scrolling or sorting is not the other's business — a path alone made
    /// `is_about` say yes to whichever tab asked, so switching between them
    /// left the second one reading the first one's walk.
    tab: usize,
    /// Per immediate child, by name: a name is what a listing row has and a
    /// path is what the walker sends.
    sizes: HashMap<String, Size>,
    counts: HashMap<String, ChildCount>,
    /// What each directory row's `Entry::len` said before a walk overwrote it.
    ///
    /// The walked size is pushed into `Entry::len` because that is the field
    /// the size *sort* reads — but `len` for a directory is otherwise the stat
    /// value, and everything else that asks a row how big it is gets the walk's
    /// answer whether or not the walk is still running. So the number that was
    /// there first is kept, and [`Folders::stat_len`] hands it back when the
    /// column is turned off, the linemode changes, or the walk is stopped.
    stat_len: HashMap<String, u64>,
    /// When the last watcher event said these numbers were stale. See
    /// [`RESTALE_QUIET`].
    stale_since: Option<Instant>,
}

impl Folders {
    /// Point this at a new directory, forgetting the last one. `token` is the
    /// walk that will fill it in.
    pub fn begin(&mut self, dir: PathBuf, tab: usize, token: DuToken) {
        self.dir = dir;
        self.tab = tab;
        self.token = Some(token);
        self.sizes.clear();
        self.counts.clear();
        self.stat_len.clear();
        self.stale_since = None;
    }

    /// Point this at a directory that will **not** be measured, so nothing
    /// asks again.
    ///
    /// A network mount (see [`df_core::du::fstype`]). Without this the frame
    /// loop would `statfs` it once per frame forever, because "is this the
    /// directory being measured" would keep answering no.
    pub fn decline(&mut self, dir: PathBuf, tab: usize) {
        self.begin(dir, tab, DuToken(0));
        self.token = None;
    }

    /// Stop measuring — leaving for a remote listing, turning the feature off,
    /// closing the last tab.
    pub fn clear(&mut self) {
        self.dir = PathBuf::new();
        self.token = None;
        self.sizes.clear();
        self.counts.clear();
        self.stat_len.clear();
        self.stale_since = None;
    }

    pub fn is_about(&self, dir: &Path, tab: usize) -> bool {
        !self.dir.as_os_str().is_empty() && self.dir == dir && self.tab == tab
    }

    /// Whether this is measuring `dir` at all, whichever tab asked.
    ///
    /// What the *watcher* wants to know: a directory that changed on disk
    /// changed for every tab looking at it.
    pub fn watches(&self, dir: &Path) -> bool {
        !self.dir.as_os_str().is_empty() && self.dir == dir
    }

    /// The directory changed under the walk: the numbers on screen are stale,
    /// and a fresh walk is due once it stops changing.
    ///
    /// The sizes are **kept** and put back to `~`. Dropping them would blank
    /// the column for the whole of a build, which is precisely when somebody is
    /// watching it; a slightly-behind number that says it is behind is the
    /// better half of that trade.
    pub fn mark_stale(&mut self, now: Instant) {
        self.token = None;
        self.stale_since = Some(now);
        for size in self.sizes.values_mut() {
            size.settled = false;
        }
    }

    /// Whether the directory has been quiet long enough to be walked again.
    pub fn due(&self, now: Instant) -> bool {
        self.stale_since
            .is_some_and(|since| now.saturating_duration_since(since) >= RESTALE_QUIET)
    }

    /// Point the existing numbers at a fresh walk, keeping them on screen.
    pub fn restart(&mut self, token: DuToken) {
        self.token = Some(token);
        self.stale_since = None;
    }

    /// Remember what a row's `Entry::len` was before the walk wrote to it. The
    /// first answer wins — a second walk must not record the first one's.
    pub fn remember_stat(&mut self, name: &str, len: u64) {
        if !self.stat_len.contains_key(name) {
            self.stat_len.insert(name.to_string(), len);
        }
    }

    /// What a row's `Entry::len` said before the walk, if the walk changed it.
    pub fn stat_len(&self, name: &str) -> Option<u64> {
        self.stat_len.get(name).copied()
    }

    pub fn token(&self) -> Option<DuToken> {
        self.token
    }

    /// Fill in from a walk that has already run. Everything a cache record
    /// holds is final by construction — it was written when a walk finished —
    /// so these rows start settled and lose the `~` they never had.
    pub fn seed(&mut self, record: &DuRecord) -> bool {
        let mut changed = false;
        for (name, totals) in &record.children {
            let size = Size {
                bytes: totals.total_bytes,
                settled: true,
            };
            if self.sizes.get(name) != Some(&size) {
                self.sizes.insert(name.clone(), size);
                changed = true;
            }
        }
        changed
    }

    /// Take the cheap `read_dir` pass. Returns whether anything on screen
    /// changed.
    pub fn apply_counts(&mut self, counts: &[(PathBuf, ChildCount)]) -> bool {
        let mut changed = false;
        for (path, count) in counts {
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if self.counts.insert(name, *count) != Some(*count) {
                changed = true;
            }
        }
        changed
    }

    /// Take a batch of walk updates. Only depth 1 is kept: deeper directories
    /// are counted — that is what makes the depth-1 numbers true — but nothing
    /// in this listing is about them.
    pub fn apply(&mut self, updates: &[DuUpdate]) -> bool {
        let mut changed = false;
        for update in updates {
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
            let size = Size {
                bytes: update.total_bytes,
                settled: update.done,
            };
            if self.sizes.get(&name) != Some(&size) {
                self.sizes.insert(name, size);
                changed = true;
            }
        }
        changed
    }

    /// The walk finished. Every row still wearing a `~` settles with it: a
    /// directory the walk never reported is one it found nothing in, and it
    /// weighs what it weighs.
    pub fn finish(&mut self) {
        self.token = None;
        for size in self.sizes.values_mut() {
            size.settled = true;
        }
    }

    /// What is known about one row.
    pub fn size(&self, name: &str) -> Option<Size> {
        self.sizes.get(name).copied()
    }

    /// How many entries that row's directory holds, when the cheap pass has
    /// been that far.
    pub fn count(&self, name: &str) -> Option<ChildCount> {
        self.counts.get(name).copied()
    }

    /// The size column's text for one directory row, or `None` when there is
    /// nothing yet to say and the em dash stands.
    pub fn label(&self, name: &str) -> Option<String> {
        crate::format::folder_size_text(self.size(name), self.count(name))
    }

    /// Whether anything at all is known — the cheap check the painter makes
    /// before asking per row.
    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty() && self.counts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use df_core::du::DuTotals;

    fn update(name: &str, depth: usize, bytes: u64, done: bool) -> DuUpdate {
        DuUpdate {
            dir: PathBuf::from("/home/brian/Downloads").join(name),
            depth,
            total_bytes: bytes,
            apparent_bytes: bytes,
            files: 1,
            dirs: 1,
            done,
        }
    }

    fn folders() -> Folders {
        let mut f = Folders::default();
        f.begin(PathBuf::from("/home/brian/Downloads"), 0, DuToken(1));
        f
    }

    fn count(entries: u64) -> ChildCount {
        ChildCount {
            entries,
            capped: false,
        }
    }

    /// The three states one row passes through, in order.
    #[test]
    fn a_row_goes_from_nothing_to_a_count_to_a_size() {
        let mut f = folders();
        assert_eq!(f.label("photos"), None);

        assert!(f.apply_counts(&[(PathBuf::from("/home/brian/Downloads/photos"), count(12))]));
        assert_eq!(f.label("photos").as_deref(), Some("12 items"));

        assert!(f.apply(&[update("photos", 1, 4 * 1024 * 1024, false)]));
        assert_eq!(f.label("photos").as_deref(), Some("~4.0 MB"));

        assert!(f.apply(&[update("photos", 1, 5 * 1024 * 1024, true)]));
        assert_eq!(f.label("photos").as_deref(), Some("5.0 MB"));
    }

    /// Only the children of the directory on screen are kept; the root's own
    /// total and everything deeper is somebody else's business.
    #[test]
    fn only_depth_one_reaches_a_row() {
        let mut f = folders();
        assert!(!f.apply(&[update("", 0, 99, true)]));
        assert!(!f.apply(&[update("photos/raw", 2, 99, true)]));
        assert!(f.is_empty());
    }

    /// A repeated update with the same numbers must not ask for a repaint —
    /// the walk re-emits running totals ten times a second.
    #[test]
    fn an_unchanged_update_changes_nothing() {
        let mut f = folders();
        assert!(f.apply(&[update("photos", 1, 1024, false)]));
        assert!(!f.apply(&[update("photos", 1, 1024, false)]));
        // …but the same bytes *settling* is a change: the `~` comes off.
        assert!(f.apply(&[update("photos", 1, 1024, true)]));
    }

    /// A walk that ends settles the rows it never got to.
    #[test]
    fn finishing_takes_every_tilde_off() {
        let mut f = folders();
        f.apply(&[update("photos", 1, 1024, false)]);
        f.finish();
        assert_eq!(f.label("photos").as_deref(), Some("1.0 KB"));
        assert!(f.token().is_none());
    }

    /// A cached record is a finished walk, so a revisit is instant and plain —
    /// no `~`, because nothing is counting.
    #[test]
    fn a_seeded_row_is_settled() {
        let mut f = folders();
        let record = DuRecord {
            mtime: None,
            totals: DuTotals::default(),
            children: vec![(
                "photos".to_string(),
                DuTotals {
                    total_bytes: 2048,
                    ..DuTotals::default()
                },
            )],
            children_complete: true,
        };
        assert!(f.seed(&record));
        assert_eq!(f.label("photos").as_deref(), Some("2.0 KB"));
        assert!(!f.seed(&record), "seeding the same record twice is a no-op");
    }

    /// Moving to another directory forgets the last one's numbers outright —
    /// a size keyed by name would otherwise be shown against a different
    /// folder that happens to share it.
    #[test]
    fn beginning_elsewhere_forgets_everything() {
        let mut f = folders();
        f.apply(&[update("photos", 1, 1024, true)]);
        assert!(f.is_about(Path::new("/home/brian/Downloads"), 0));

        f.begin(PathBuf::from("/home/brian/Work"), 0, DuToken(2));
        assert!(f.is_empty());
        assert_eq!(f.label("photos"), None);
        assert!(!f.is_about(Path::new("/home/brian/Downloads"), 0));

        f.clear();
        assert!(!f.is_about(Path::new("/home/brian/Work"), 0));
        // An empty path must never match an empty path.
        assert!(!f.is_about(Path::new(""), 0));
    }

    /// Two tabs on the same directory are two listings, and the walk belongs to
    /// the one that asked for it.
    #[test]
    fn the_same_path_in_another_tab_is_another_question() {
        let f = folders();
        assert!(f.is_about(Path::new("/home/brian/Downloads"), 0));
        assert!(!f.is_about(Path::new("/home/brian/Downloads"), 1));
        // …but the watcher's question is about the directory, not the tab.
        assert!(f.watches(Path::new("/home/brian/Downloads")));
    }

    /// A churning directory keeps its numbers, wearing the `~` that says they
    /// are behind, and is re-walked only once it has been quiet.
    #[test]
    fn a_churning_directory_keeps_its_numbers_and_waits_for_quiet() {
        let t0 = Instant::now();
        let mut f = folders();
        f.apply(&[update("photos", 1, 1024, true)]);
        assert_eq!(f.label("photos").as_deref(), Some("1.0 KB"));

        f.mark_stale(t0);
        assert_eq!(
            f.label("photos").as_deref(),
            Some("~1.0 KB"),
            "the last number stands, and says it is still counting"
        );
        assert!(f.token().is_none());
        assert!(!f.due(t0));
        assert!(!f.due(t0 + RESTALE_QUIET / 2));
        // A second event during the burst pushes the deadline out again.
        f.mark_stale(t0 + RESTALE_QUIET / 2);
        assert!(!f.due(t0 + RESTALE_QUIET));
        assert!(f.due(t0 + RESTALE_QUIET + RESTALE_QUIET / 2));

        f.restart(DuToken(9));
        assert_eq!(f.token(), Some(DuToken(9)));
        assert!(
            !f.due(t0 + RESTALE_QUIET * 10),
            "a restart is not still due"
        );
    }

    /// The stat size a walk overwrites is remembered once, so it can be put
    /// back when the walk stops.
    #[test]
    fn the_stat_size_is_remembered_before_the_walk_overwrites_it() {
        let mut f = folders();
        assert_eq!(f.stat_len("photos"), None);
        f.remember_stat("photos", 4096);
        f.remember_stat("photos", 4_200_000_000);
        assert_eq!(
            f.stat_len("photos"),
            Some(4096),
            "the first answer is the honest one"
        );
        f.clear();
        assert_eq!(f.stat_len("photos"), None);
    }
}
