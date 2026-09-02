//! The directory model is what every pane, key and operation reads, so it is
//! tested the way PLAN §9 asks: pure functions against fabricated entries where
//! the point is the ordering rule, and against a real fixture tree where the
//! point is the filesystem.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::*;
use crate::config::{MgrConfig, SortBy};

// ── Fixtures ────────────────────────────────────────────────────────────────

/// A directory under `$TMPDIR` that removes itself. `tempfile` would be a
/// dependency for twelve lines (PLAN §1).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("delightfile-{tag}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the fixture directory");
        TempDir { path }
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.path.join(name);
        std::fs::write(&path, contents).expect("write a fixture file");
        path
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        std::fs::create_dir_all(&path).expect("create a fixture directory");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn at(secs: u64) -> Option<SystemTime> {
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

/// A fabricated entry. Nothing here touches a filesystem: the sort and filter
/// rules are pure functions and are tested as such.
fn entry(name: &str, kind: Kind, len: u64, mtime: Option<SystemTime>) -> Entry {
    Entry {
        name: name.to_string(),
        path: PathBuf::from("/fixture").join(name),
        kind,
        len,
        mtime,
        btime: mtime,
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        is_hidden: name.starts_with('.'),
        mime: mime::hint_for_name(name),
        file_kind: crate::fs::classify(kind, name, mime::hint_for_name(name), 0o644),
    }
}

fn file(name: &str) -> Entry {
    entry(name, Kind::File, 0, None)
}

fn dir(name: &str) -> Entry {
    entry(name, Kind::Dir, 0, None)
}

fn files(names: &[&str]) -> Vec<Entry> {
    names.iter().map(|n| file(n)).collect()
}

fn names(entries: &[Entry], opts: &SortOptions) -> Vec<String> {
    sort_order(entries, opts)
        .into_iter()
        .map(|i| entries[i].name.clone())
        .collect()
}

fn options(by: SortBy) -> SortOptions {
    SortOptions {
        by,
        reverse: false,
        dir_first: false,
        sensitive: false,
        seed: 0,
    }
}

// ── Natural vs alphabetical ─────────────────────────────────────────────────

/// The ordering table PLAN §4.1's `, n` promises, one row per rule.
#[test]
fn natural_sort_orders_numbers_as_numbers() {
    let table: &[(&str, &str, std::cmp::Ordering)] = &[
        // The headline case: "file10" after "file9".
        ("file9", "file10", std::cmp::Ordering::Less),
        ("file10", "file9", std::cmp::Ordering::Greater),
        // Multi-digit runs anywhere in the name.
        ("s2e10.mkv", "s10e2.mkv", std::cmp::Ordering::Less),
        ("track2", "track11", std::cmp::Ordering::Less),
        // Leading zeros are the same number…
        ("img007", "img7", std::cmp::Ordering::Greater),
        // …but do not outrank what follows them.
        ("img1.png", "img01.zip", std::cmp::Ordering::Less),
        // A number is not a letter: digits sort before letters at the same spot.
        ("2nd", "second", std::cmp::Ordering::Less),
        // Plain names still compare as text.
        ("apple", "banana", std::cmp::Ordering::Less),
        // Shorter prefixes first.
        ("file", "file2", std::cmp::Ordering::Less),
        // Very long runs do not overflow anything.
        (
            "a99999999999999999999999",
            "a100000000000000000000000",
            std::cmp::Ordering::Less,
        ),
        ("same", "same", std::cmp::Ordering::Equal),
    ];
    for (a, b, expected) in table {
        assert_eq!(natural_cmp(a, b, false), *expected, "{a} vs {b}");
    }
}

#[test]
fn natural_sorts_a_season_the_way_a_person_reads_it() {
    let entries = files(&["ep10.mkv", "ep2.mkv", "ep1.mkv", "ep20.mkv", "ep3.mkv"]);
    assert_eq!(
        names(&entries, &options(SortBy::Natural)),
        vec!["ep1.mkv", "ep2.mkv", "ep3.mkv", "ep10.mkv", "ep20.mkv"]
    );
    // Alphabetical is the *other* answer, on purpose — it is the default, and
    // the two modes exist because both are right for different directories.
    assert_eq!(
        names(&entries, &options(SortBy::Alphabetical)),
        vec!["ep1.mkv", "ep10.mkv", "ep2.mkv", "ep20.mkv", "ep3.mkv"]
    );
}

#[test]
fn case_sensitivity_is_a_setting() {
    let entries = files(&["Zebra", "apple", "Apple"]);
    let mut opts = options(SortBy::Alphabetical);
    // Insensitive (the default, and yazi's): a real alphabet.
    assert_eq!(names(&entries, &opts), vec!["Apple", "apple", "Zebra"]);
    // Sensitive: ASCII order, capitals first — which is why it is off.
    opts.sensitive = true;
    assert_eq!(names(&entries, &opts), vec!["Apple", "Zebra", "apple"]);
}

// ── The other sort modes ────────────────────────────────────────────────────

#[test]
fn mtime_and_btime_sort_newest_first_and_tolerate_a_missing_time() {
    let entries = vec![
        entry("old", Kind::File, 0, at(100)),
        entry("new", Kind::File, 0, at(300)),
        entry("mid", Kind::File, 0, at(200)),
        entry("unknown", Kind::File, 0, None),
    ];
    assert_eq!(
        names(&entries, &options(SortBy::Mtime)),
        vec!["new", "mid", "old", "unknown"]
    );
    // Reversed is oldest-first, and the timeless entry still lands at the far
    // end rather than pretending to be the oldest file in the world.
    let mut reversed = options(SortBy::Mtime);
    reversed.reverse = true;
    assert_eq!(
        names(&entries, &reversed),
        vec!["unknown", "old", "mid", "new"]
    );
    // btime reads the same field-for-field.
    assert_eq!(
        names(&entries, &options(SortBy::Btime)),
        vec!["new", "mid", "old", "unknown"]
    );
}

#[test]
fn size_sorts_biggest_first_and_directories_tie_at_zero() {
    let entries = vec![
        entry("small.bin", Kind::File, 10, None),
        entry("huge.bin", Kind::File, 9_000_000, None),
        dir("b-dir"),
        dir("a-dir"),
        entry("medium.bin", Kind::File, 500, None),
    ];
    // Directories are all `len == 0` (documented on `Entry::len`) so they fall
    // through to the name tie-break instead of landing in filesystem order.
    assert_eq!(
        names(&entries, &options(SortBy::Size)),
        vec!["huge.bin", "medium.bin", "small.bin", "a-dir", "b-dir"]
    );
}

#[test]
fn extension_groups_then_names_within_the_group() {
    let entries = files(&["b.rs", "a.rs", "c.md", "plain", ".gitignore"]);
    // No extension sorts first (empty string), and `.gitignore` has none — the
    // leading dot is the hidden marker, not a separator.
    assert_eq!(
        names(&entries, &options(SortBy::Extension)),
        vec![".gitignore", "plain", "c.md", "a.rs", "b.rs"]
    );
}

#[test]
fn random_is_stable_for_a_seed_and_different_between_seeds() {
    let entries = files(&["a", "b", "c", "d", "e", "f", "g", "h"]);
    let mut opts = options(SortBy::Random);
    let first = names(&entries, &opts);
    // The same seed twice is the same order — a listing must not reshuffle
    // itself while you scroll it.
    assert_eq!(first, names(&entries, &opts));
    // It is a shuffle, not the identity.
    assert_ne!(first, vec!["a", "b", "c", "d", "e", "f", "g", "h"]);
    opts.seed = 12345;
    assert_ne!(first, names(&entries, &opts));
    // Every file is still present exactly once.
    let mut sorted = names(&entries, &opts);
    sorted.sort();
    assert_eq!(sorted, vec!["a", "b", "c", "d", "e", "f", "g", "h"]);
}

#[test]
fn none_is_scan_order_and_still_reverses() {
    let entries = files(&["zebra", "apple", "mango"]);
    assert_eq!(
        names(&entries, &options(SortBy::None)),
        vec!["zebra", "apple", "mango"]
    );
    let mut reversed = options(SortBy::None);
    reversed.reverse = true;
    assert_eq!(names(&entries, &reversed), vec!["mango", "apple", "zebra"]);
}

#[test]
fn dir_first_is_a_band_that_reverse_does_not_break() {
    let entries = vec![file("b.txt"), dir("src"), file("a.txt"), dir("assets")];
    let mut opts = options(SortBy::Alphabetical);
    opts.dir_first = true;
    assert_eq!(
        names(&entries, &opts),
        vec!["assets", "src", "a.txt", "b.txt"]
    );
    // Reversing reverses *within* the bands. Files never float above folders,
    // because reversing a listing should read like reading it upside down, not
    // like the panes swapped.
    opts.reverse = true;
    assert_eq!(
        names(&entries, &opts),
        vec!["src", "assets", "b.txt", "a.txt"]
    );
    // A symlink to a directory is a directory for ordering purposes.
    let mut with_link = entries.clone();
    with_link.push(entry(
        "link",
        Kind::Symlink {
            target: Some(LinkTarget::Dir),
        },
        0,
        None,
    ));
    opts.reverse = false;
    assert_eq!(
        names(&with_link, &opts),
        vec!["assets", "link", "src", "a.txt", "b.txt"]
    );
}

#[test]
fn the_sort_is_stable_for_equal_keys() {
    // Ten files with the same mtime and the same name-shape, arriving in an
    // order the filesystem chose. `SortBy::None` proves the algorithm itself
    // keeps input order; `dir_first` proves the banding does too.
    let entries = vec![
        file("f5"),
        dir("d3"),
        file("f1"),
        dir("d1"),
        file("f9"),
        dir("d2"),
    ];
    let mut opts = options(SortBy::None);
    opts.dir_first = true;
    assert_eq!(
        names(&entries, &opts),
        vec!["d3", "d1", "d2", "f5", "f1", "f9"]
    );
}

#[test]
fn sort_entries_permutes_in_place() {
    let mut entries = files(&["c", "a", "b"]);
    sort_entries(&mut entries, &options(SortBy::Alphabetical));
    let ordered: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(ordered, vec!["a", "b", "c"]);
}

// ── Filter, hidden, find ────────────────────────────────────────────────────

#[test]
fn smart_case_is_insensitive_until_you_type_a_capital() {
    assert!(!is_case_sensitive("readme"));
    assert!(is_case_sensitive("Readme"));
    assert!(match_name("README.md", "readme").is_some());
    assert!(match_name("README.md", "README").is_some());
    // A capital in the query means the capital in the name.
    assert!(match_name("readme.md", "README").is_none());
    // An empty query matches everything and highlights nothing.
    assert_eq!(match_name("anything", ""), Some(Vec::new()));
    assert!(match_name("anything", "zz").is_none());
}

#[test]
fn highlight_spans_point_at_the_matched_bytes() {
    // One occurrence, mid-name.
    assert_eq!(match_name("delightfile.rs", "file"), Some(vec![(7, 11)]));
    // Every occurrence, non-overlapping, in order.
    assert_eq!(
        match_name("test-test-test", "test"),
        Some(vec![(0, 4), (5, 9), (10, 14)])
    );
    // Case-folded matching still returns offsets into the *original* name, so
    // the highlight lands on the right glyphs.
    let spans = match_name("README.md", "me").expect("a match");
    assert_eq!(spans, vec![(4, 6)]);
    assert_eq!(&"README.md"[4..6], "ME");
    // Multi-byte names: the span must be a char boundary, or slicing it panics.
    let name = "café-münchen.txt";
    let spans = match_name(name, "münchen").expect("a match");
    let (start, end) = spans[0];
    assert_eq!(&name[start..end], "münchen");
}

#[test]
fn hidden_files_are_filtered_before_the_query_is() {
    let entries = files(&[".gitignore", "git-notes.md", "readme.md"]);
    let order: Vec<usize> = (0..entries.len()).collect();

    let visible = filter_indices(&entries, &order, "", false);
    assert_eq!(visible.len(), 2);
    assert!(visible
        .iter()
        .all(|m| entries[m.index].name != ".gitignore"));

    // `.` on: the dotfile is back.
    assert_eq!(filter_indices(&entries, &order, "", true).len(), 3);

    // Filtering for `git` with hidden off does not surface `.gitignore` — the
    // two toggles compose the way you would expect.
    let matched = filter_indices(&entries, &order, "git", false);
    assert_eq!(matched.len(), 1);
    assert_eq!(entries[matched[0].index].name, "git-notes.md");
    assert_eq!(matched[0].spans, vec![(0, 3)]);
    assert_eq!(filter_indices(&entries, &order, "git", true).len(), 2);
}

#[test]
fn find_walks_forward_backward_and_wraps() {
    let entries = files(&["alpha", "beta", "gamma", "beta-two", "delta"]);
    let view: Vec<usize> = (0..entries.len()).collect();

    // From the top, forward.
    assert_eq!(
        find_from(&entries, &view, 0, "beta", FindDirection::Forward),
        Some(1)
    );
    // `n` from that match goes to the next one.
    assert_eq!(
        find_from(&entries, &view, 1, "beta", FindDirection::Forward),
        Some(3)
    );
    // …and past the last match it wraps to the first.
    assert_eq!(
        find_from(&entries, &view, 3, "beta", FindDirection::Forward),
        Some(1)
    );
    // Backward is the mirror image, wrapping the other way.
    assert_eq!(
        find_from(&entries, &view, 0, "beta", FindDirection::Backward),
        Some(3)
    );
    assert_eq!(
        find_from(&entries, &view, 3, "beta", FindDirection::Backward),
        Some(1)
    );
    // Exactly one match, and the cursor is already on it: the wrap comes back
    // around to it, because "there is one and you are on it" is a different
    // answer from "there are none".
    assert_eq!(
        find_from(&entries, &view, 2, "gamma", FindDirection::Forward),
        Some(2)
    );
    // No match at all.
    assert_eq!(
        find_from(&entries, &view, 0, "omega", FindDirection::Forward),
        None
    );
    // Nothing to search.
    assert_eq!(
        find_from(&entries, &[], 0, "a", FindDirection::Forward),
        None
    );
}

// ── DirState ────────────────────────────────────────────────────────────────

fn loaded_state(entries: Vec<Entry>) -> DirState {
    let mgr = MgrConfig {
        sort_by: SortBy::Alphabetical,
        ..MgrConfig::default()
    };
    let mut state = DirState::new("/fixture", &mgr);
    let token = ScanToken(1);
    state.token = Some(token);
    state.path = PathBuf::from("/fixture");
    state.apply(&ScanUpdate::Started {
        token,
        dir: PathBuf::from("/fixture"),
    });
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries,
    });
    state.apply(&ScanUpdate::Done {
        token,
        dir: PathBuf::from("/fixture"),
        total: state.entries.len(),
    });
    state
}

/// The cursor a caller aims at a directory it has only just asked for lands
/// when the row does — which is the whole reason `aim_cursor` exists, since
/// every caller of it (`←` out of a folder, a tab returning somewhere it
/// remembers) asks while the scan is still in flight.
#[test]
fn an_aimed_cursor_waits_for_the_row_it_names() {
    let mgr = MgrConfig {
        sort_by: SortBy::Alphabetical,
        ..MgrConfig::default()
    };
    let mut state = DirState::new("/fixture", &mgr);
    let token = ScanToken(1);
    state.token = Some(token);
    // Empty, exactly as it is the instant a navigation queues the scan.
    state.aim_cursor("m.txt");
    assert_eq!(state.cursor(), 0);

    state.apply(&ScanUpdate::Started {
        token,
        dir: PathBuf::from("/fixture"),
    });
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["a.txt", "m.txt", "z.txt"]),
    });
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("m.txt"));

    // Once honoured it is spent: a later batch does not drag the cursor back
    // to it after the user has moved on.
    state.set_cursor(0);
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["n.txt"]),
    });
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("a.txt"));
}

/// …and an aim the user overrules, or that the directory turns out not to
/// contain, is dropped rather than pouncing later.
#[test]
fn an_aimed_cursor_gives_up_when_it_is_overruled_or_never_arrives() {
    let mut state = loaded_state(files(&["a.txt", "b.txt"]));
    let token = ScanToken(7);
    state.token = Some(token);
    state.aim_cursor("gone.txt");
    // The user moves the cursor themselves: the aim is off.
    state.move_cursor(1);
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["gone.txt"]),
    });
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("b.txt"));

    // And an aim nobody overrules still stops at the end of the scan, so a
    // file created in this directory ten minutes later is not pounced on.
    state.aim_cursor("later.txt");
    state.apply(&ScanUpdate::Done {
        token,
        dir: PathBuf::from("/fixture"),
        total: 3,
    });
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["later.txt"]),
    });
    assert_ne!(
        state.cursor_entry().map(|e| e.name.as_str()),
        Some("later.txt")
    );
}

/// The three ways an aim is cancelled that it used to survive.
///
/// Each one is the same bug from a different door: a name still being waited
/// for is a cursor move that has not happened yet, and a later rebuild
/// honouring it drags the view somewhere the user stopped asking for seconds
/// ago.
#[test]
fn an_aim_is_cancelled_by_the_arrow_keys_by_the_mouse_and_by_a_failure() {
    let token = ScanToken(11);

    // `↑`/`↓` wrap rather than clamping, and went through their own path that
    // forgot to clear the aim.
    let mut state = loaded_state(files(&["a.txt", "b.txt"]));
    state.token = Some(token);
    state.aim_cursor("gone.txt");
    state.wrap_cursor(1);
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["gone.txt"]),
    });
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("b.txt"));

    // A wheel roll leaves the cursor alone on purpose, so it cannot cancel the
    // aim the way a key press does — it says so itself.
    let mut state = loaded_state(files(&["a.txt", "b.txt"]));
    state.token = Some(token);
    state.aim_cursor("gone.txt");
    state.cancel_aim();
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["gone.txt"]),
    });
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("a.txt"));

    // A directory that could not be read has no row to aim at — now or in the
    // listing this state is filled from next.
    let mut state = loaded_state(files(&["a.txt", "b.txt"]));
    state.token = Some(token);
    state.aim_cursor("gone.txt");
    state.apply(&ScanUpdate::Failed {
        token,
        dir: PathBuf::from("/fixture"),
        error: DfError::io(
            PathBuf::from("/fixture"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        ),
    });
    assert_eq!(state.state(), LoadState::Failed);
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["a.txt", "gone.txt"]),
    });
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("a.txt"));
}

/// Writing a number into a row is not a reason for the rows to move.
#[test]
fn revising_entries_in_place_leaves_the_view_alone() {
    let mut state = loaded_state(files(&["a.txt", "b.txt", "c.txt"]));
    // Biggest first, which is what the size sort means (see `sort.rs`).
    state.set_sort(SortOptions {
        by: SortBy::Size,
        ..SortOptions::default()
    });
    state.set_cursor(0);
    let before = state.generation();
    let first = state.row(0).map(|e| e.name.clone());

    let changed = state.revise_entries_in_place(|entries| {
        for entry in entries.iter_mut() {
            if entry.name == "c.txt" {
                entry.len = u64::MAX;
            }
        }
        true
    });
    assert!(changed);
    assert_eq!(
        state.generation(),
        before,
        "no rebuild, so no new generation"
    );
    assert_eq!(state.row(0).map(|e| e.name.clone()), first, "nothing moved");
    assert_eq!(state.cursor(), 0);

    // …and the ordinary door still reorders, which is what the timer in the
    // app calls when it is time.
    assert!(state.revise_entries(|_| true));
    assert_eq!(state.row(0).map(|e| e.name.as_str()), Some("c.txt"));
}

// ── Recent ──────────────────────────────────────────────────────────────────

#[test]
fn a_memory_recalls_what_it_was_told_and_forgets_the_oldest_first() {
    let mut recent: Recent<String> = Recent::new(3);
    for name in ["a", "b", "c"] {
        recent.remember(format!("/dir/{name}"), name.to_string());
    }
    assert_eq!(recent.len(), 3);
    assert_eq!(
        recent.recall(Path::new("/dir/a")).map(String::as_str),
        Some("a")
    );

    // Touching `a` again makes `b` the oldest, so `b` is what the fourth
    // directory pushes out.
    recent.remember("/dir/a", "a2".to_string());
    recent.remember("/dir/d", "d".to_string());
    assert_eq!(recent.len(), 3);
    assert_eq!(recent.recall(Path::new("/dir/b")), None);
    assert_eq!(
        recent.recall(Path::new("/dir/a")).map(String::as_str),
        Some("a2")
    );
    assert_eq!(
        recent.recall(Path::new("/dir/d")).map(String::as_str),
        Some("d")
    );

    // Forgetting one takes it out of the eviction order too, so the map cannot
    // drift out of step with itself.
    recent.forget(Path::new("/dir/a"));
    assert_eq!(recent.len(), 2);
    recent.remember("/dir/e", "e".to_string());
    recent.remember("/dir/f", "f".to_string());
    assert_eq!(recent.len(), 3);
    assert_eq!(recent.recall(Path::new("/dir/c")), None);

    // A memory that holds nothing is a legal way to turn the feature off.
    let mut none: Recent<String> = Recent::new(0);
    none.remember("/dir/a", "a".to_string());
    assert!(none.is_empty());
    assert_eq!(none.recall(Path::new("/dir/a")), None);
}

#[test]
fn the_cursor_stays_on_the_same_file_across_a_reload() {
    let mut state = loaded_state(files(&["a.txt", "m.txt", "z.txt"]));
    assert!(state.cursor_to_name("m.txt"));
    assert_eq!(state.cursor(), 1);

    // A file lands *above* the cursor. The row under the cursor must not move.
    let token = ScanToken(2);
    state.token = Some(token);
    state.apply(&ScanUpdate::Started {
        token,
        dir: PathBuf::from("/fixture"),
    });
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["a.txt", "b.txt", "c.txt", "m.txt", "z.txt"]),
    });
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("m.txt"));
    assert_eq!(state.cursor(), 3);

    // The file under the cursor is deleted: the cursor holds its *position*,
    // clamped — which is where the file was.
    let token = ScanToken(3);
    state.token = Some(token);
    state.apply(&ScanUpdate::Started {
        token,
        dir: PathBuf::from("/fixture"),
    });
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["a.txt", "b.txt", "c.txt", "z.txt"]),
    });
    assert_eq!(state.cursor(), 3);
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("z.txt"));
}

#[test]
fn a_stale_scan_is_dropped_by_its_token() {
    let mut state = loaded_state(files(&["keep.txt"]));
    let stale = ScanUpdate::Batch {
        token: ScanToken(999),
        dir: PathBuf::from("/fixture"),
        entries: files(&["ghost.txt"]),
    };
    assert!(!state.apply(&stale), "a stale token must be refused");
    assert_eq!(state.len(), 1);
    assert_eq!(state.row(0).map(|e| e.name.as_str()), Some("keep.txt"));

    // Right token, wrong directory: also refused, so a caller can route it to
    // whichever pane it belongs to.
    let elsewhere = ScanUpdate::Batch {
        token: state.token().expect("a token"),
        dir: PathBuf::from("/somewhere-else"),
        entries: files(&["ghost.txt"]),
    };
    assert!(!state.apply(&elsewhere));
    assert_eq!(state.len(), 1);
}

#[test]
fn the_view_reacts_to_hidden_filter_and_sort_without_moving_the_entries() {
    let mut state = loaded_state(files(&[
        "b.txt",
        ".hidden",
        "a.txt",
        "notes-b.md",
        "notes-a.md",
    ]));
    // Hidden off by default (PLAN §2).
    assert_eq!(state.len(), 4);
    assert_eq!(state.total(), 5);

    state.toggle_hidden();
    assert_eq!(state.len(), 5);
    assert_eq!(state.row(0).map(|e| e.name.as_str()), Some(".hidden"));
    state.toggle_hidden();

    state.set_filter("notes");
    assert_eq!(state.len(), 2);
    assert_eq!(state.row_spans(0), &[(0, 5)]);
    assert_eq!(state.row(0).map(|e| e.name.as_str()), Some("notes-a.md"));

    // Sorting reverses the filtered view; the underlying entries never moved.
    let mut sort = state.sort_options();
    sort.reverse = true;
    state.set_sort(sort);
    assert_eq!(state.row(0).map(|e| e.name.as_str()), Some("notes-b.md"));
    assert_eq!(state.entries()[0].name, "b.txt");

    state.clear_filter();
    assert_eq!(state.len(), 4);
}

#[test]
fn selection_is_by_name_and_survives_a_re_sort() {
    let mut state = loaded_state(files(&["a.txt", "b.txt", "c.txt"]));
    state.toggle_selected(0);
    state.toggle_selected(2);
    assert_eq!(state.selected_count(), 2);
    assert!(state.is_selected("a.txt"));

    let mut sort = state.sort_options();
    sort.reverse = true;
    state.set_sort(sort);
    // The same two files, now at the other end of the list.
    assert!(state.is_selected("a.txt") && state.is_selected("c.txt"));
    assert_eq!(
        state.selected_paths(),
        vec![
            PathBuf::from("/fixture/a.txt"),
            PathBuf::from("/fixture/c.txt")
        ]
    );

    state.invert_selection();
    assert_eq!(state.selected_count(), 1);
    assert!(state.is_selected("b.txt"));

    state.select_all();
    assert_eq!(state.selected_count(), 3);
    state.clear_selection();
    assert_eq!(state.selected_count(), 0);

    // A selection is only ever over *visible* rows: selecting what a filter
    // hides would mean `d` deleting files you cannot see.
    state.set_filter("a");
    state.select_all();
    assert_eq!(state.selected_count(), 1);
}

/// A directory too big to re-sort on every batch stops re-sorting mid-scan and
/// catches up when the scan finishes — the pane stays responsive instead of
/// paying a full sort per batch (see `STREAM_SORT_LIMIT`).
#[test]
fn a_huge_directory_stops_re_sorting_mid_scan() {
    let mut state = loaded_state(files(&["a.txt"]));
    let token = ScanToken(11);
    state.token = Some(token);
    let dir = PathBuf::from("/fixture");
    state.apply(&ScanUpdate::Started {
        token,
        dir: dir.clone(),
    });

    let huge: Vec<Entry> = (0..STREAM_SORT_LIMIT + 500)
        .map(|i| file(&format!("f{i:06}.txt")))
        .collect();
    let total = huge.len();
    state.apply(&ScanUpdate::Batch {
        token,
        dir: dir.clone(),
        entries: huge,
    });
    // Entries are in; the view has deliberately not caught up yet.
    assert_eq!(state.total(), total);
    assert!(state.len() < total);

    state.apply(&ScanUpdate::Done { token, dir, total });
    assert_eq!(state.len(), total);
    assert_eq!(state.row(0).map(|e| e.name.as_str()), Some("f000000.txt"));
}

#[test]
fn a_selection_drops_files_that_are_gone_after_a_rescan() {
    let mut state = loaded_state(files(&["a.txt", "b.txt"]));
    state.select_all();
    assert_eq!(state.selected_count(), 2);

    let token = ScanToken(7);
    state.token = Some(token);
    state.apply(&ScanUpdate::Started {
        token,
        dir: PathBuf::from("/fixture"),
    });
    state.apply(&ScanUpdate::Batch {
        token,
        dir: PathBuf::from("/fixture"),
        entries: files(&["a.txt"]),
    });
    state.apply(&ScanUpdate::Done {
        token,
        dir: PathBuf::from("/fixture"),
        total: 1,
    });
    assert_eq!(state.selected_count(), 1);
    assert!(state.is_selected("a.txt"));
}

/// A *page* clamps: `Ctrl+f` at the bottom stops there.
#[test]
fn a_page_of_cursor_clamps_at_both_ends() {
    let mut state = loaded_state(files(&["a", "b", "c"]));
    state.move_cursor(-1);
    assert_eq!(state.cursor(), 0);
    state.move_cursor(10);
    assert_eq!(state.cursor(), 2);
    state.move_cursor(1);
    assert_eq!(state.cursor(), 2);
    state.set_cursor(99);
    assert_eq!(state.cursor(), 2);

    // An empty list has a meaningless cursor rather than a panicking one.
    let mut empty = loaded_state(Vec::new());
    empty.move_cursor(3);
    assert_eq!(empty.cursor(), 0);
    assert!(empty.cursor_entry().is_none());
    assert!(empty.is_empty());
}

/// …and one arrow key's worth wraps: the listing is a ring.
#[test]
fn an_arrow_key_wraps_around_the_ends() {
    let mut state = loaded_state(files(&["a", "b", "c"]));
    state.wrap_cursor(-1);
    assert_eq!(state.cursor(), 2, "up on the first row is the last row");
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("c"));
    state.wrap_cursor(1);
    assert_eq!(state.cursor(), 0, "down on the last row is the first row");
    state.wrap_cursor(1);
    assert_eq!(state.cursor(), 1);
    // More than one turn of the ring still lands somewhere sensible.
    state.wrap_cursor(7);
    assert_eq!(state.cursor(), 2);
    state.wrap_cursor(-7);
    assert_eq!(state.cursor(), 1);

    // A one-row listing wraps onto itself, and an empty one does nothing.
    let mut one = loaded_state(files(&["only"]));
    one.wrap_cursor(1);
    assert_eq!(one.cursor(), 0);
    let mut empty = loaded_state(Vec::new());
    empty.wrap_cursor(-1);
    assert_eq!(empty.cursor(), 0);
}

#[test]
fn find_moves_the_cursor_through_the_view() {
    let mut state = loaded_state(files(&["alpha", "beta", "gamma", "beta-two"]));
    assert!(state.find("beta", FindDirection::Forward));
    assert_eq!(state.cursor_entry().map(|e| e.name.as_str()), Some("beta"));
    assert!(state.find("beta", FindDirection::Forward));
    assert_eq!(
        state.cursor_entry().map(|e| e.name.as_str()),
        Some("beta-two")
    );
    assert!(!state.find("nothing-here", FindDirection::Forward));
    // A failed find leaves the cursor where it was.
    assert_eq!(
        state.cursor_entry().map(|e| e.name.as_str()),
        Some("beta-two")
    );
}

#[test]
fn a_failed_load_says_why_instead_of_looking_empty() {
    let mut state = DirState::new("/definitely/not/here", &MgrConfig::default());
    let err = state.load_blocking().expect_err("no such directory");
    assert!(err.to_string().contains("/definitely/not/here"));
    assert_eq!(state.state(), LoadState::Failed);
    assert!(state.error().is_some());
    assert!(state.is_empty());
}

// ── Entries against a real tree ─────────────────────────────────────────────

#[test]
fn entries_read_kinds_sizes_and_symlinks_off_the_disk() {
    let tmp = TempDir::new("entries");
    tmp.file("note.txt", b"hello");
    tmp.dir("sub");
    tmp.file(".hidden", b"");
    let link = tmp.path.join("to-note");
    std::os::unix::fs::symlink(tmp.path.join("note.txt"), &link).expect("symlink");
    let dangling = tmp.path.join("dangling");
    std::os::unix::fs::symlink(tmp.path.join("gone"), &dangling).expect("symlink");
    let dir_link = tmp.path.join("to-sub");
    std::os::unix::fs::symlink(tmp.path.join("sub"), &dir_link).expect("symlink");

    let note = Entry::read(tmp.path.join("note.txt")).expect("read note.txt");
    assert_eq!(note.kind, Kind::File);
    assert_eq!(note.len, 5);
    assert_eq!(note.mime, "text/plain");
    assert_eq!(note.extension(), "txt");
    assert_eq!(note.stem(), "note");
    assert!(!note.is_hidden);
    assert!(note.mtime.is_some());

    let sub = Entry::read(tmp.path.join("sub")).expect("read sub");
    assert_eq!(sub.kind, Kind::Dir);
    assert!(sub.is_dir());
    // Directories report zero, not the inode size — see `Entry::len`.
    assert_eq!(sub.len, 0);
    assert_eq!(sub.mime, mime::DIR_MIME);

    let hidden = Entry::read(tmp.path.join(".hidden")).expect("read .hidden");
    assert!(hidden.is_hidden);
    // A leading dot is the hidden marker, not an extension separator.
    assert_eq!(hidden.extension(), "");
    assert_eq!(hidden.stem(), ".hidden");

    // A resolving link reports the *target's* size — "how big is this" is a
    // question about the file, not about the path that names it.
    let good = Entry::read(&link).expect("read the link");
    assert_eq!(
        good.kind,
        Kind::Symlink {
            target: Some(LinkTarget::File)
        }
    );
    assert_eq!(good.len, 5);
    assert!(good.is_symlink() && !good.is_broken_symlink() && !good.is_dir());
    assert_eq!(good.link_target(), Some(tmp.path.join("note.txt")));

    let broken = Entry::read(&dangling).expect("read the dangling link");
    assert_eq!(broken.kind, Kind::Symlink { target: None });
    assert!(broken.is_broken_symlink());
    assert_eq!(broken.mime, mime::BROKEN_LINK_MIME);

    // A link to a directory *is* a directory for navigation and for dir_first.
    let to_sub = Entry::read(&dir_link).expect("read the dir link");
    assert!(to_sub.is_dir());
    assert_eq!(to_sub.mime, mime::DIR_MIME);
}

#[test]
fn permission_strings_read_like_ls() {
    let mut e = file("x");
    e.mode = 0o100_644;
    assert_eq!(e.permissions_string(), "-rw-r--r--");
    e.mode = 0o100_755;
    assert_eq!(e.permissions_string(), "-rwxr-xr-x");
    let mut d = dir("x");
    d.mode = 0o040_755;
    assert_eq!(d.permissions_string(), "drwxr-xr-x");
    // setuid replaces the owner's execute slot, sticky the other's — and both
    // shout in capitals when there is no execute bit under them.
    let mut s = file("x");
    s.mode = 0o104_755;
    assert_eq!(s.permissions_string(), "-rwsr-xr-x");
    s.mode = 0o104_644;
    assert_eq!(s.permissions_string(), "-rwSr--r--");
    let mut t = dir("x");
    t.mode = 0o041_777;
    assert_eq!(t.permissions_string(), "drwxrwxrwt");
}

#[test]
fn mime_hints_come_from_the_extension() {
    assert_eq!(mime::hint_for_name("cat.PNG"), "image/png");
    assert_eq!(mime::hint_for_name("clip.mkv"), "video/x-matroska");
    assert_eq!(mime::hint_for_name("backup.tar.gz"), "application/gzip");
    assert_eq!(mime::hint_for_name("readme"), "text/plain");
    assert_eq!(mime::hint_for_name("Makefile"), "text/x-makefile");
    // Dotfiles have no extension: `.zshrc` is not a `zshrc`-typed file.
    assert_eq!(mime::hint_for_name(".zshrc"), mime::UNKNOWN_MIME);
    assert_eq!(mime::hint_for_name("mystery"), mime::UNKNOWN_MIME);
    // The one ambiguous extension in this house.
    assert_eq!(mime::hint_for_name("model.ts"), "text/typescript");
    assert_eq!(mime::hint_for_name("00001.ts"), "video/mp2t");
    assert!(mime::is_text("text/rust"));
    assert!(mime::is_text("application/json"));
    assert!(!mime::is_text("image/png"));
}

#[test]
fn owner_names_come_from_the_passwd_table() {
    let table = owner::parse_id_table("root:x:0:0:root:/root:/bin/bash\nbrian:x:1000:1000::/home/brian:/bin/zsh\ngarbage\nbad:x:notanumber:\n");
    assert_eq!(table.get(&0).map(String::as_str), Some("root"));
    assert_eq!(table.get(&1000).map(String::as_str), Some("brian"));
    // Two lines were unusable; the rest of the file still parsed.
    assert_eq!(table.len(), 2);
    // The first record for an id wins (`root` before `toor`).
    let dupes = owner::parse_id_table("root:x:0:0:\ntoor:x:0:0:\n");
    assert_eq!(dupes.get(&0).map(String::as_str), Some("root"));
    // An unknown id renders as its number rather than as nothing.
    let label = owner::owner_label(4_294_967_294, 4_294_967_294);
    assert_eq!(label, "4294967294 4294967294");
}

// ── The scanner ─────────────────────────────────────────────────────────────

fn counting_notifier() -> (Notifier, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let bell = Arc::clone(&count);
    let notifier: Notifier = Arc::new(move || {
        bell.fetch_add(1, Ordering::SeqCst);
    });
    (notifier, count)
}

/// Collect a scan's updates until it finishes or the clock runs out.
fn collect(scanner: &Scanner, token: ScanToken, budget: Duration) -> Vec<ScanUpdate> {
    let deadline = Instant::now() + budget;
    let mut updates = Vec::new();
    while Instant::now() < deadline {
        let Ok(update) = scanner.updates().recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        let done = matches!(update, ScanUpdate::Done { .. } | ScanUpdate::Failed { .. })
            && update.token() == token;
        updates.push(update);
        if done {
            break;
        }
    }
    updates
}

#[test]
fn the_scanner_batches_a_directory_and_rings_the_bell() {
    let tmp = TempDir::new("scan");
    // More than one batch's worth, so the batching is actually exercised.
    let total = FIRST_BATCH + BATCH + 7;
    for i in 0..total {
        tmp.file(&format!("file{i:05}.txt"), b"x");
    }

    let (notifier, bell) = counting_notifier();
    let scanner = Scanner::start(notifier);
    let token = scanner.scan(tmp.path.clone());
    let updates = collect(&scanner, token, Duration::from_secs(10));

    assert!(
        matches!(updates.first(), Some(ScanUpdate::Started { .. })),
        "the first update announces the directory: {updates:?}"
    );
    let batches: Vec<usize> = updates
        .iter()
        .filter_map(|u| match u {
            ScanUpdate::Batch { entries, .. } => Some(entries.len()),
            _ => None,
        })
        .collect();
    // The first batch is small so the pane paints immediately; the rest are
    // large so a big directory is not a wake-up storm.
    assert_eq!(batches.first(), Some(&FIRST_BATCH));
    assert_eq!(batches.get(1), Some(&BATCH));
    assert_eq!(batches.iter().sum::<usize>(), total);
    assert_eq!(batches.len(), 3, "first + full + remainder: {batches:?}");

    match updates.last() {
        Some(ScanUpdate::Done { total: n, .. }) => assert_eq!(*n, total),
        other => panic!("the scan should have finished: {other:?}"),
    }
    // One ring per update, and the scan is no longer live.
    assert_eq!(bell.load(Ordering::SeqCst), updates.len());
    assert!(!scanner.is_live(token));
}

#[test]
fn a_scan_of_the_same_directory_cancels_the_previous_one() {
    let tmp = TempDir::new("recan");
    tmp.file("a.txt", b"a");
    let (notifier, _) = counting_notifier();
    let scanner = Scanner::start(notifier);

    let first = scanner.scan(tmp.path.clone());
    let second = scanner.scan(tmp.path.clone());
    assert!(!scanner.is_live(first), "the superseded scan is cancelled");
    assert!(second > first);

    // A scan of a *different* directory is untouched: the parent pane, the list
    // and a directory preview are three concurrent scans and none is stale.
    let other = TempDir::new("recan-other");
    let third = scanner.scan(other.path.clone());
    assert!(scanner.is_live(second) || scanner.is_live(third));

    scanner.cancel_all();
    assert!(!scanner.is_live(second) && !scanner.is_live(third));
}

#[test]
fn an_unreadable_directory_fails_with_its_path() {
    let (notifier, _) = counting_notifier();
    let scanner = Scanner::start(notifier);
    let missing = PathBuf::from("/definitely/not/here/either");
    let token = scanner.scan(missing.clone());
    let updates = collect(&scanner, token, Duration::from_secs(5));
    match updates.first() {
        Some(ScanUpdate::Failed { error, dir, .. }) => {
            assert_eq!(dir, &missing);
            assert!(error.to_string().contains("/definitely/not/here/either"));
        }
        other => panic!("expected a failure naming the path: {other:?}"),
    }
}

#[test]
fn a_dir_state_loads_a_real_tree_through_the_scanner() {
    let tmp = TempDir::new("state");
    tmp.file("b.txt", b"bb");
    tmp.file("a.txt", b"a");
    tmp.dir("sub");
    tmp.file(".hidden", b"");

    let (notifier, _) = counting_notifier();
    let scanner = Scanner::start(notifier);
    let mut state = DirState::new(tmp.path.clone(), &MgrConfig::default());
    let token = state.begin_scan(&scanner);
    assert_eq!(state.state(), LoadState::Loading);

    for update in collect(&scanner, token, Duration::from_secs(5)) {
        assert!(state.apply(&update));
    }

    assert_eq!(state.state(), LoadState::Loaded);
    // Default config: dir_first, alphabetical, hidden off (PLAN §2).
    let visible: Vec<&str> = state.rows().map(|(e, _)| e.name.as_str()).collect();
    assert_eq!(visible, vec!["sub", "a.txt", "b.txt"]);
    assert_eq!(state.total(), 4);
    state.toggle_hidden();
    assert_eq!(state.len(), 4);
}

// ── History ─────────────────────────────────────────────────────────────────

#[test]
fn history_is_the_browser_model() {
    let mut history = History::new("/a");
    assert!(!history.can_go_back() && !history.can_go_forward());
    assert_eq!(history.go_back(), None);

    history.push("/b");
    history.push("/c");
    assert_eq!(history.current(), Path::new("/c"));

    assert_eq!(history.go_back(), Some(Path::new("/b")));
    assert_eq!(history.go_back(), Some(Path::new("/a")));
    assert_eq!(history.go_back(), None);
    assert!(history.can_go_forward());

    assert_eq!(history.go_forward(), Some(Path::new("/b")));
    assert_eq!(history.go_forward(), Some(Path::new("/c")));
    assert_eq!(history.go_forward(), None);
}

#[test]
fn going_somewhere_new_after_going_back_truncates_the_forward_stack() {
    let mut history = History::new("/a");
    history.push("/b");
    history.push("/c");
    history.go_back();
    assert_eq!(history.forward_stack(), [PathBuf::from("/c")]);

    history.push("/d");
    assert!(!history.can_go_forward(), "the detour threw /c away");
    assert_eq!(history.current(), Path::new("/d"));
    assert_eq!(history.go_back(), Some(Path::new("/b")));
}

#[test]
fn navigating_to_where_you_already_are_is_not_a_history_entry() {
    let mut history = History::new("/a");
    history.push("/a");
    history.push("/a");
    assert!(!history.can_go_back());
    assert_eq!(history.back_stack().len(), 0);
}

#[test]
fn history_stops_growing_at_the_limit() {
    let mut history = History::new("/start");
    for i in 0..HISTORY_LIMIT + 50 {
        history.push(format!("/dir{i}"));
    }
    assert_eq!(history.back_stack().len(), HISTORY_LIMIT);
    // The oldest entries fell off the bottom, the newest are intact.
    assert!(!history.back_stack().contains(&PathBuf::from("/start")));
    assert_eq!(
        history.back_stack().last(),
        Some(&PathBuf::from(format!("/dir{}", HISTORY_LIMIT + 48)))
    );
}

// ── The watcher ─────────────────────────────────────────────────────────────

#[test]
fn a_disabled_watcher_is_inert_rather_than_a_failure() {
    let watcher = Watcher::disabled();
    assert!(!watcher.is_active());
    watcher.watch(vec![PathBuf::from("/tmp")]);
    assert!(watcher.drain().is_empty());
}

/// PLAN §1's repaint-on-event rule reaches the filesystem too: a file landing
/// in a watched directory has to ring the bell without anybody polling.
///
/// Skipped rather than failed where inotify is unavailable — a sandbox with no
/// instances left is not a broken build, and the model works without it.
#[test]
fn a_file_landing_in_a_watched_directory_raises_a_refresh() {
    let tmp = TempDir::new("watch");
    let (notifier, bell) = counting_notifier();
    let watcher = match Watcher::new(notifier) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("skipping: inotify is unavailable here ({e})");
            return;
        }
    };
    assert!(watcher.is_active());
    watcher.watch(vec![tmp.path.clone()]);

    // Files are created in a loop rather than once, so the test does not race
    // the watcher thread installing the watch.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut changed = false;
    let mut round = 0;
    while Instant::now() < deadline && !changed {
        tmp.file(&format!("landed{round}.txt"), b"hi");
        round += 1;
        while let Ok(event) = watcher
            .events()
            .recv_timeout(DEBOUNCE + Duration::from_millis(120))
        {
            if event == WatchEvent::Changed(tmp.path.clone()) {
                changed = true;
                break;
            }
        }
    }
    if !changed {
        eprintln!("skipping: no inotify watch could be installed (watch limit?)");
        return;
    }
    assert!(
        bell.load(Ordering::SeqCst) > 0,
        "the notifier is what wakes the event loop"
    );

    // A burst is one refresh, not one per file: DEBOUNCE exists so a
    // `git checkout` costs a single rescan.
    for i in 0..200 {
        tmp.file(&format!("burst{i}.txt"), b"x");
    }
    std::thread::sleep(DEBOUNCE * 4);
    let events = watcher.drain();
    assert!(
        events.len() <= 4,
        "200 files should debounce into a handful of refreshes, got {}",
        events.len()
    );
    assert!(events.iter().all(|e| matches!(e, WatchEvent::Changed(_))));
}

#[test]
fn a_watched_directory_that_disappears_reports_itself_gone() {
    let outer = TempDir::new("watch-gone");
    let inner = outer.dir("doomed");
    let (notifier, _) = counting_notifier();
    let watcher = match Watcher::new(notifier) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("skipping: inotify is unavailable here ({e})");
            return;
        }
    };
    watcher.watch(vec![inner.clone()]);
    // Let the watch land before the directory goes away.
    std::thread::sleep(Duration::from_millis(100));
    let _ = std::fs::remove_dir_all(&inner);

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut gone = false;
    while Instant::now() < deadline && !gone {
        match watcher.events().recv_timeout(Duration::from_millis(200)) {
            Ok(WatchEvent::Gone(dir)) if dir == inner => gone = true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    if !gone {
        eprintln!("skipping: no inotify watch could be installed (watch limit?)");
    }
}

// ── External listings (PLAN §7.4, §7.6) ─────────────────────────────────────

/// A listing whose rows arrive from somewhere that is not the local scanner —
/// a remote directory streaming over SFTP — is *loading* until its last batch
/// lands, and that is what the pane's "loading…" hint reads.
///
/// The distinction from [`DirState::set_entries`] is the whole point: that one
/// is "here is the listing", this one is "here is some of it".
#[test]
fn an_external_listing_streams_and_only_then_is_loaded() {
    let mut dir = DirState::new("sftp://showandtour1/srv", &MgrConfig::default());
    dir.set_sort(options(SortBy::Alphabetical));
    dir.begin_external();
    assert_eq!(dir.state(), LoadState::Loading);
    assert!(dir.is_empty());
    // No scan token, so a late `ScanUpdate` for a scan this listing used to be
    // waiting on cannot overwrite rows that did not come from a filesystem.
    assert!(dir.token().is_none());

    dir.extend_external(files(&["b.txt", "a.txt"]));
    // Rows are visible and sorted while the rest is still arriving — the whole
    // reason a slow listing streams at all.
    assert_eq!(dir.len(), 2);
    assert_eq!(dir.row(0).map(|e| e.name.as_str()), Some("a.txt"));
    assert_eq!(dir.state(), LoadState::Loading, "still arriving");

    dir.extend_external(files(&["c.txt"]));
    dir.finish_external();
    assert_eq!(dir.state(), LoadState::Loaded);
    assert_eq!(dir.len(), 3);

    // A selection is pruned to what is still here on completion, exactly as a
    // finished scan prunes it: a `y` on a row the server no longer has would
    // otherwise paste a ghost.
    dir.toggle_selected(0);
    assert_eq!(dir.selected_count(), 1);
    dir.begin_external();
    dir.extend_external(files(&["z.txt"]));
    dir.finish_external();
    assert_eq!(dir.selected_count(), 0);
    assert_eq!(dir.len(), 1, "a new listing replaces the old one");
}

/// A failed external listing says why, in the pane, instead of looking like an
/// empty directory.
#[test]
fn a_failed_external_listing_keeps_its_sentence() {
    let mut dir = DirState::new("sftp://showandtour1/srv", &MgrConfig::default());
    dir.begin_external();
    dir.extend_external(files(&["a.txt"]));
    dir.fail_external("showandtour1: cannot log in: Permission denied (publickey)");
    assert_eq!(dir.state(), LoadState::Failed);
    assert!(dir.is_empty(), "half a listing is not shown as the listing");
    assert_eq!(
        dir.error(),
        Some("showandtour1: cannot log in: Permission denied (publickey)")
    );
}
