//! Trees with known contents, walked and counted.
//!
//! Everything here builds a real directory under `$TMPDIR` with `std::fs` and
//! then asserts on the numbers, because the whole module is about what the
//! filesystem says and a mock of the filesystem would only assert that the mock
//! agrees with itself. The two things a test genuinely cannot make are a second
//! device and a directory with a million hardlinks; the first is covered by
//! testing the boundary predicate and the flag that reaches it, and the second by
//! setting the cap to a number a fixture can reach.

#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use super::*;
use crate::fs::no_notifier;
use crate::ops::fixture::TempTree;
use crate::DfError;

/// Collect every update a walk emits, and its totals.
fn walk_collect(root: &Path, options: &DuOptions) -> (DuTotals, Vec<DuUpdate>) {
    let never = || false;
    let mut seen = Vec::new();
    let mut sink = |batch: Vec<DuUpdate>| seen.extend(batch);
    let totals = walk(root, options, &never, &mut sink).expect("walk");
    (totals, seen)
}

fn apparent_of(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).unwrap().size()
}

fn blocks_of(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).unwrap().blocks() * BLOCK_UNIT
}

/// A tree whose every byte is accounted for:
///
/// ```text
/// root/a.txt      1000 bytes
/// root/sub/b.txt  2000 bytes
/// root/sub/deep/c.txt  3000 bytes
/// ```
fn known_tree() -> TempTree {
    let tree = TempTree::new("du-known");
    tree.file("a.txt", &vec![b'a'; 1000]);
    tree.file("sub/b.txt", &vec![b'b'; 2000]);
    tree.file("sub/deep/c.txt", &vec![b'c'; 3000]);
    tree
}

// ── the arithmetic ──────────────────────────────────────────────────────────

#[test]
fn totals_add_up_over_a_known_tree() {
    let tree = known_tree();
    let root = tree.path();
    let (totals, _) = walk_collect(root, &DuOptions::default());

    assert_eq!(totals.files, 3, "three files");
    assert_eq!(totals.dirs, 3, "root, sub, deep — the root counts itself");

    // The directories' own inodes count too, so subtract them to get back to
    // the file bytes the fixture put there.
    let dir_apparent =
        apparent_of(root) + apparent_of(&tree.join("sub")) + apparent_of(&tree.join("sub/deep"));
    assert_eq!(
        totals.apparent_bytes - dir_apparent,
        1000 + 2000 + 3000,
        "apparent size is the sum of st_size"
    );

    let dir_blocks =
        blocks_of(root) + blocks_of(&tree.join("sub")) + blocks_of(&tree.join("sub/deep"));
    let file_blocks = blocks_of(&tree.join("a.txt"))
        + blocks_of(&tree.join("sub/b.txt"))
        + blocks_of(&tree.join("sub/deep/c.txt"));
    assert_eq!(
        totals.total_bytes,
        dir_blocks + file_blocks,
        "block usage is the sum of st_blocks * 512"
    );
}

#[test]
fn an_empty_directory_is_one_dir_and_no_files() {
    let tree = TempTree::new("du-empty");
    let totals = walk_blocking(tree.path(), &DuOptions::default()).unwrap();
    assert_eq!(totals.files, 0);
    assert_eq!(totals.dirs, 1);
    assert_eq!(totals.apparent_bytes, apparent_of(tree.path()));
}

#[test]
fn a_sparse_file_is_small_on_disk_and_large_on_paper() {
    let tree = TempTree::new("du-sparse");
    let path = tree.join("sparse.img");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    drop(file);

    if blocks_of(&path) != 0 {
        // Some filesystems (and every one with compression or inline data
        // heuristics) allocate anyway. The divergence is the point of the test,
        // so on such a filesystem there is nothing here to prove.
        return;
    }

    let totals = walk_blocking(tree.path(), &DuOptions::default()).unwrap();
    assert_eq!(
        totals.apparent_bytes - apparent_of(tree.path()),
        8 * 1024 * 1024,
        "the file claims eight megabytes"
    );
    assert_eq!(
        totals.total_bytes,
        blocks_of(tree.path()),
        "and occupies none of them"
    );
    assert!(
        totals.total_bytes < totals.apparent_bytes,
        "which is the whole reason both numbers exist"
    );
}

#[test]
fn hardlinked_content_is_counted_once() {
    let tree = TempTree::new("du-hardlink");
    let original = tree.file("one.bin", &vec![b'x'; 4096]);
    tree.dir("sub");
    std::fs::hard_link(&original, tree.join("sub/two.bin")).unwrap();

    let totals = walk_blocking(tree.path(), &DuOptions::default()).unwrap();
    let dirs = apparent_of(tree.path()) + apparent_of(&tree.join("sub"));
    assert_eq!(
        totals.apparent_bytes - dirs,
        4096,
        "one inode, one set of bytes"
    );
    assert_eq!(totals.files, 2, "but two names, and both are listed");
}

#[test]
fn past_the_hardlink_cap_bytes_are_counted_twice() {
    let tree = TempTree::new("du-hardlink-cap");
    let original = tree.file("one.bin", &vec![b'x'; 4096]);
    std::fs::hard_link(&original, tree.join("two.bin")).unwrap();

    // A cap of zero is "the set is already full", which is the documented
    // cliff — bytes past it double-count rather than dedup non-deterministically.
    let options = DuOptions {
        hardlink_cap: 0,
        ..DuOptions::default()
    };
    let totals = walk_blocking(tree.path(), &options).unwrap();
    assert_eq!(
        totals.apparent_bytes - apparent_of(tree.path()),
        4096 * 2,
        "the cap costs accuracy, exactly as documented"
    );
}

#[test]
fn symlinks_are_neither_followed_nor_counted() {
    let elsewhere = TempTree::new("du-symlink-target");
    elsewhere.file("enormous.bin", &vec![b'z'; 100_000]);

    let tree = TempTree::new("du-symlink");
    tree.file("real.txt", &vec![b'r'; 500]);
    tree.symlink(elsewhere.path(), "away");
    tree.symlink("real.txt", "here");
    // A link to the tree's own parent: following it would walk `$TMPDIR`.
    tree.symlink("..", "up");

    let totals = walk_blocking(tree.path(), &DuOptions::default()).unwrap();
    assert_eq!(totals.files, 1, "only the real file is a file");
    assert_eq!(totals.dirs, 1, "the link to a directory is not a directory");
    assert_eq!(totals.apparent_bytes - apparent_of(tree.path()), 500);
}

#[test]
fn the_depth_cap_stops_descending_without_stopping_counting() {
    let tree = known_tree();
    let options = DuOptions {
        max_depth: 1,
        ..DuOptions::default()
    };
    let totals = walk_blocking(tree.path(), &options).unwrap();
    assert_eq!(totals.files, 2, "a.txt and sub/b.txt; c.txt is too deep");
    assert_eq!(totals.dirs, 3, "`deep` still counts as a directory");
}

#[test]
fn an_unreadable_subdirectory_does_not_fail_the_walk() {
    use std::os::unix::fs::PermissionsExt;
    let tree = TempTree::new("du-unreadable");
    tree.file("visible.txt", &[b'v'; 100]);
    let locked = tree.dir("locked");
    tree.file("locked/hidden.txt", &[b'h'; 100]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let totals = walk_blocking(tree.path(), &DuOptions::default()).unwrap();
    // Restore before the fixture tries to delete it.
    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));

    // Running as root reads it anyway, and then there is nothing to assert.
    if totals.files == 1 {
        assert_eq!(
            totals.dirs, 2,
            "the locked directory is counted, not entered"
        );
    }
}

#[test]
fn a_missing_root_is_an_io_error_not_a_panic() {
    let tree = TempTree::new("du-missing");
    let err = walk_blocking(&tree.join("nope"), &DuOptions::default()).unwrap_err();
    assert!(matches!(err, DfError::Io { .. }), "{err}");
}

#[test]
fn a_file_root_is_refused() {
    let tree = TempTree::new("du-file-root");
    let file = tree.file("a.txt", b"x");
    let err = walk_blocking(&file, &DuOptions::default()).unwrap_err();
    assert!(matches!(err, DfError::Io { .. }), "{err}");
}

// ── the device boundary ─────────────────────────────────────────────────────

#[test]
fn the_boundary_predicate_is_the_flag() {
    // A unit test cannot mount a second filesystem, so the rule is tested where
    // it lives. Same device: always fine. Different device: only with the flag.
    assert!(!crosses_boundary(66, 66, false));
    assert!(!crosses_boundary(66, 66, true));
    assert!(crosses_boundary(66, 99, false));
    assert!(!crosses_boundary(66, 99, true));
}

#[test]
fn a_single_device_tree_walks_the_same_either_way() {
    let tree = known_tree();
    let stay = walk_blocking(tree.path(), &DuOptions::default()).unwrap();
    let cross = walk_blocking(
        tree.path(),
        &DuOptions {
            cross_filesystems: true,
            ..DuOptions::default()
        },
    )
    .unwrap();
    assert_eq!(stay, cross, "the flag only matters at a real mount point");
}

#[test]
fn the_flag_survives_the_trip_through_a_request() {
    // The plumbing half of the boundary rule: what `request_with` is handed is
    // what the walk runs with. Proven by an option a fixture *can* observe —
    // `max_depth` — travelling the same path through the same struct.
    let tree = known_tree();
    let du = DuScanner::start(no_notifier());
    let options = DuOptions {
        cross_filesystems: true,
        max_depth: 1,
        ..DuOptions::default()
    };
    let token = du.request_with(tree.path(), options);
    let totals = wait_for_done(&du, token);
    assert_eq!(
        totals.files, 2,
        "max_depth reached the worker, and so did the rest"
    );
}

// ── streaming ───────────────────────────────────────────────────────────────

#[test]
fn depth_of_interest_bounds_what_is_reported() {
    let tree = known_tree();

    let (_, shallow) = walk_collect(tree.path(), &DuOptions::at_depth(0));
    let dirs: Vec<&Path> = shallow.iter().map(|u| u.dir.as_path()).collect();
    assert!(
        dirs.iter().all(|d| *d == tree.path()),
        "depth 0 reports the root alone: {dirs:?}"
    );

    let (_, deeper) = walk_collect(tree.path(), &DuOptions::at_depth(1));
    assert!(
        deeper.iter().any(|u| u.dir == tree.join("sub") && u.done),
        "depth 1 reports the children"
    );
    assert!(
        !deeper.iter().any(|u| u.dir == tree.join("sub/deep")),
        "but not the grandchildren"
    );
}

#[test]
fn a_finished_directory_is_reported_exactly_once() {
    let tree = known_tree();
    let (_, updates) = walk_collect(tree.path(), &DuOptions::at_depth(1));
    let done: Vec<&DuUpdate> = updates.iter().filter(|u| u.done).collect();
    assert_eq!(done.len(), 2, "root and sub: {done:#?}");
    assert!(done.iter().any(|u| u.dir == tree.path() && u.depth == 0));
    assert!(done
        .iter()
        .any(|u| u.dir == tree.join("sub") && u.depth == 1));
}

#[test]
fn running_totals_are_emitted_while_the_walk_is_alive() {
    let tree = known_tree();
    // Zero interval: every pass of the loop re-reports the open directories,
    // which is the batching path the 100 ms default takes on a big tree.
    let options = DuOptions {
        update_interval: Duration::ZERO,
        ..DuOptions::at_depth(1)
    };
    let (totals, updates) = walk_collect(tree.path(), &options);

    let partials: Vec<&DuUpdate> = updates
        .iter()
        .filter(|u| !u.done && u.dir == tree.path())
        .collect();
    assert!(!partials.is_empty(), "the root reported itself climbing");
    assert!(
        partials
            .windows(2)
            .all(|w| w[0].apparent_bytes <= w[1].apparent_bytes),
        "and a running total only ever grows"
    );
    assert!(
        partials
            .iter()
            .all(|u| u.apparent_bytes <= totals.apparent_bytes),
        "never past the final answer"
    );
    assert!(
        updates.last().map(|u| u.done && u.dir == tree.path()) == Some(true),
        "and the last word is the root, finished"
    );
}

#[test]
fn batches_are_flushed_at_the_batch_size() {
    let tree = TempTree::new("du-batching");
    for i in 0..10 {
        tree.file(format!("d{i}/f.txt"), b"hello");
    }
    let options = DuOptions {
        batch: 4,
        // Long enough that the timer cannot be what flushes.
        update_interval: Duration::from_secs(3600),
        ..DuOptions::at_depth(1)
    };
    let never = || false;
    let mut sizes = Vec::new();
    let mut sink = |batch: Vec<DuUpdate>| sizes.push(batch.len());
    walk(tree.path(), &options, &never, &mut sink).unwrap();

    assert!(sizes.len() > 1, "eleven directories did not arrive at once");
    assert!(
        sizes.iter().take(sizes.len() - 1).all(|n| *n == 4),
        "every batch but the last is full: {sizes:?}"
    );
}

#[test]
fn cancellation_stops_the_walk_where_it_stands() {
    let tree = known_tree();
    let stop = || true;
    let mut sink = |_: Vec<DuUpdate>| {};
    let err = walk(tree.path(), &DuOptions::default(), &stop, &mut sink).unwrap_err();
    assert!(matches!(err, DfError::Cancelled), "{err}");
}

// ── the pool ────────────────────────────────────────────────────────────────

fn wait_for_done(du: &DuScanner, token: DuToken) -> DuTotals {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let Ok(message) = du.updates().recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        match message {
            DuMessage::Done {
                token: t, totals, ..
            } if t == token => return totals,
            DuMessage::Failed { error, .. } => panic!("walk failed: {error}"),
            _ => {}
        }
    }
    panic!("no Done inside ten seconds");
}

#[test]
fn the_pool_walks_and_says_so() {
    let tree = known_tree();
    let du = DuScanner::start(no_notifier());
    let token = du.request(tree.path(), 1);
    let totals = wait_for_done(&du, token);
    assert_eq!(
        totals,
        walk_blocking(tree.path(), &DuOptions::default()).unwrap()
    );
    assert!(!du.is_live(token), "a finished walk is no longer live");
}

#[test]
fn the_notifier_rings_once_per_message() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let tree = known_tree();
    let rings = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&rings);
    let du = DuScanner::start(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let token = du.request(tree.path(), 1);
    wait_for_done(&du, token);
    assert!(
        rings.load(Ordering::SeqCst) >= 2,
        "Started and Done at least"
    );
}

#[test]
fn a_second_request_supersedes_the_first() {
    let tree = known_tree();
    let du = DuScanner::new(1, no_notifier());
    let first = du.request(tree.path(), 1);
    let second = du.request(tree.path(), 1);
    assert!(!du.is_live(first), "the same root cancels itself");
    assert_ne!(first, second, "tokens are never reused");
}

#[test]
fn a_different_root_is_not_superseded() {
    let one = known_tree();
    let two = known_tree();
    let du = DuScanner::new(1, no_notifier());
    let first = du.request(one.path(), 1);
    let second = du.request(two.path(), 1);
    assert_ne!(first, second);
    // Neither cancelled the other; both are live or already done, never
    // cancelled-by-the-other, which `cancel_all` then proves is the only way
    // they stop.
    du.cancel_all();
    assert!(!du.is_live(first) && !du.is_live(second));
}

#[test]
fn a_missing_root_comes_back_as_failed() {
    let tree = TempTree::new("du-pool-missing");
    let du = DuScanner::start(no_notifier());
    let token = du.request(tree.join("nope"), 1);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let Ok(message) = du.updates().recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        if let DuMessage::Failed {
            token: t, error, ..
        } = message
        {
            assert_eq!(t, token);
            assert!(matches!(error, DfError::Io { .. }), "{error}");
            return;
        }
    }
    panic!("no Failed inside ten seconds");
}

// ── the cache ───────────────────────────────────────────────────────────────

#[test]
fn heavy_hitters_are_biggest_first_with_shares_that_add_up() {
    let tree = TempTree::new("du-heavy");
    tree.file("small/a.bin", &vec![b'a'; 1024]);
    tree.file("large/b.bin", &vec![b'b'; 256 * 1024]);
    tree.file("medium/c.bin", &vec![b'c'; 32 * 1024]);

    let du = DuScanner::start(no_notifier());
    let token = du.request(tree.path(), 1);
    wait_for_done(&du, token);

    let hits = du.heavy_hitters(tree.path());
    let names: Vec<&str> = hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["large", "medium", "small"], "{hits:#?}");
    assert!(
        hits.windows(2).all(|w| w[0].bytes >= w[1].bytes),
        "sorted descending"
    );
    let share: f64 = hits.iter().map(|h| h.fraction).sum();
    assert!(
        share > 0.0 && share <= 1.0,
        "the children are shares of the parent, not more: {share}"
    );
    assert_eq!(hits[0].path, tree.join("large"));
}

#[test]
fn heavy_hitters_are_empty_until_a_walk_has_run() {
    let tree = known_tree();
    let du = DuScanner::start(no_notifier());
    assert!(du.heavy_hitters(tree.path()).is_empty());
}

#[test]
fn a_walk_fills_the_directory_sizes_in_a_listing() {
    let tree = known_tree();
    let du = DuScanner::start(no_notifier());
    let token = du.request(tree.path(), 1);
    wait_for_done(&du, token);

    let mut entries = crate::fs::scan_blocking(tree.path()).unwrap();
    assert!(
        entries.iter().all(|e| !e.is_dir() || e.len == 0),
        "Entry::len starts at zero for directories, by design"
    );
    let filled = du.fill_dir_sizes(tree.path(), &mut entries);
    assert_eq!(filled, 1, "one directory in the listing");
    let sub = entries.iter().find(|e| e.name == "sub").expect("sub");
    assert!(sub.len > 0, "and it now knows what is inside it");
    let file = entries.iter().find(|e| e.name == "a.txt").expect("a.txt");
    assert_eq!(
        file.len, 1000,
        "files were already honest and stay untouched"
    );
}

#[test]
fn a_cached_record_hits_until_the_directory_changes() {
    let tree = known_tree();
    let du = DuScanner::start(no_notifier());
    let token = du.request(tree.path(), 1);
    wait_for_done(&du, token);
    assert!(du.cached(tree.path()).is_some(), "walked, therefore cached");

    // A directory's mtime has one-second granularity on some filesystems, so
    // changing the contents is not enough on its own — set it explicitly to a
    // time that cannot be the one recorded.
    let long_ago = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    let file = std::fs::File::open(tree.path()).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(long_ago))
        .unwrap();

    assert!(
        du.cached(tree.path()).is_none(),
        "a changed mtime invalidates the record"
    );
}

#[test]
fn forget_drops_one_record_and_leaves_the_rest() {
    let mut cache = DuCache::new();
    let tree = known_tree();
    let mtime = current_mtime(tree.path());
    cache.insert(tree.path(), mtime, DuTotals::default(), Vec::new(), true);
    assert!(cache.get(tree.path()).is_some());
    assert!(cache.forget(tree.path()));
    assert!(!cache.forget(tree.path()), "twice is a no-op");
    assert!(cache.get(tree.path()).is_none());
}

#[test]
fn a_record_without_an_mtime_never_hits() {
    let mut cache = DuCache::new();
    let tree = known_tree();
    cache.insert(tree.path(), None, DuTotals::default(), Vec::new(), true);
    assert!(
        cache.get(tree.path()).is_none(),
        "a record that cannot be invalidated must not be trusted"
    );
    assert!(
        cache.get_stale(tree.path()).is_some(),
        "but it is still there for a caller that asked for it anyway"
    );
}

#[test]
fn the_cache_evicts_the_least_recently_touched() {
    let mut cache = DuCache::new();
    for i in 0..DU_CACHE_DIRS + 8 {
        cache.insert(
            format!("/synthetic/{i}"),
            Some(std::time::SystemTime::UNIX_EPOCH),
            DuTotals::default(),
            Vec::new(),
            true,
        );
    }
    assert_eq!(cache.len(), DU_CACHE_DIRS, "the cap holds");
    assert!(
        cache.get_stale(Path::new("/synthetic/0")).is_none(),
        "the oldest went first"
    );
    let newest = format!("/synthetic/{}", DU_CACHE_DIRS + 7);
    assert!(
        cache.get_stale(Path::new(&newest)).is_some(),
        "the newest stayed"
    );
}

#[test]
fn the_child_cap_keeps_the_big_ones() {
    let mut cache = DuCache::new();
    let children: Vec<(String, DuTotals)> = (0..MAX_CACHED_CHILDREN + 10)
        .map(|i| {
            (
                format!("child{i}"),
                DuTotals {
                    total_bytes: i as u64,
                    ..DuTotals::default()
                },
            )
        })
        .collect();
    cache.insert(
        "/synthetic/big",
        Some(std::time::SystemTime::UNIX_EPOCH),
        DuTotals {
            total_bytes: 1_000_000,
            ..DuTotals::default()
        },
        children,
        true,
    );
    let record = cache
        .get_stale(Path::new("/synthetic/big"))
        .expect("record");
    assert_eq!(record.children.len(), MAX_CACHED_CHILDREN);
    assert!(!record.children_complete, "and it says it dropped some");
    assert_eq!(
        record.children[0].1.total_bytes,
        (MAX_CACHED_CHILDREN + 9) as u64,
        "the ones kept are the biggest"
    );
}

#[test]
fn a_zero_byte_parent_gives_zero_shares_not_nan() {
    let mut cache = DuCache::new();
    let tree = TempTree::new("du-zero");
    cache.insert(
        tree.path(),
        current_mtime(tree.path()),
        DuTotals::default(),
        vec![("child".to_string(), DuTotals::default())],
        true,
    );
    let hits = cache.heavy_hitters(tree.path());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].fraction, 0.0, "not NaN, which would panic a layout");
}

// ── the cheap child-count pass ──────────────────────────────────────────────

/// The number the size column shows before a single byte has been added up.
#[test]
fn child_counts_are_immediate_children_by_name() {
    let tree = TempTree::new("du-counts");
    tree.file("sub/b.txt", b"b");
    tree.file("sub/deep/c.txt", b"c");
    tree.file("empty/.keep", b"");
    std::fs::remove_file(tree.path().join("empty/.keep")).unwrap();
    tree.file("a.txt", b"a");

    let never = || false;
    let counts = child_counts(tree.path(), &never);
    let find = |name: &str| {
        counts
            .iter()
            .find(|(p, _)| p.file_name().unwrap() == name)
            .map(|(_, n)| *n)
    };
    // `sub` holds `b.txt` and `deep` — two names, not the three files under it:
    // the pass counts entries, and says so.
    assert_eq!(find("sub"), Some(2));
    assert_eq!(find("empty"), Some(0));
    // Files in the root are not counted at all; their own length is already in
    // the listing.
    assert!(find("a.txt").is_none(), "{counts:?}");
    assert_eq!(counts.len(), 2, "{counts:?}");
}

/// A symlink to a directory is not a directory here, for the reason the walk
/// skips them: the tree it points at is somewhere else.
#[test]
fn child_counts_skip_symlinks() {
    let tree = TempTree::new("du-counts-link");
    tree.file("real/a.txt", b"a");
    std::os::unix::fs::symlink(tree.path().join("real"), tree.path().join("link")).unwrap();
    let never = || false;
    let counts = child_counts(tree.path(), &never);
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert_eq!(counts[0].0.file_name().unwrap(), "real");
}

/// Cancellation reaches inside the pass, so leaving a directory stops it.
#[test]
fn child_counts_stop_when_cancelled() {
    let tree = TempTree::new("du-counts-cancel");
    for i in 0..8 {
        tree.file(format!("d{i}/a.txt"), b"a");
    }
    let always = || true;
    assert!(child_counts(tree.path(), &always).is_empty());
}

/// The pass streams through the scanner ahead of the walk, under the same
/// token — which is what lets the app drop both when it moves on.
#[test]
fn the_scanner_sends_counts_before_the_totals() {
    let tree = TempTree::new("du-counts-stream");
    tree.file("sub/b.txt", b"b");
    let du = DuScanner::new(1, no_notifier());
    let token = du.request_with(tree.path(), DuOptions::at_depth(1).counting_children());

    let mut counted = None;
    let mut done = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !done {
        let Ok(message) = du.updates().recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        assert_eq!(message.token(), token);
        match message {
            DuMessage::Counts { counts, .. } => {
                assert!(!done, "counts must arrive before the walk finishes");
                counted = Some(counts);
            }
            DuMessage::Done { .. } => done = true,
            _ => {}
        }
    }
    assert!(done, "the walk never finished");
    let counts = counted.expect("no Counts message");
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0].1, 1);
}

/// …and it is off unless asked for: the "what's big" mode wants totals, not a
/// pass over every subdirectory it is about to walk anyway.
#[test]
fn counting_children_is_opt_in() {
    assert!(!DuOptions::default().count_children);
    assert!(!DuOptions::at_depth(1).count_children);
    assert!(DuOptions::at_depth(1).counting_children().count_children);
}
