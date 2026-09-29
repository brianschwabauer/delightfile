//! The format, the semantics, and the two things a persistence layer is
//! actually judged on: what it does with a path nobody expected, and what it
//! does with a file somebody else corrupted.

#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::path::{Path, PathBuf};

use super::*;
use crate::ops::fixture::TempTree;

fn store_at(tree: &TempTree) -> StateStore {
    StateStore::load_from(tree.join("state"))
}

fn text_of(store: &StateStore) -> String {
    String::from_utf8_lossy(&store.render()).into_owned()
}

// ── escaping ────────────────────────────────────────────────────────────────

#[test]
fn escaping_round_trips_every_byte() {
    for byte in 0u8..=255 {
        let raw = vec![b'a', byte, b'z'];
        let escaped = escape(&raw);
        assert!(
            !escaped.contains(&b'\t') && !escaped.contains(&b'\n') && !escaped.contains(&b'\r'),
            "byte {byte:#04x} left structure in the line"
        );
        assert_eq!(unescape(&escaped).as_deref(), Some(raw.as_slice()));
    }
}

#[test]
fn a_broken_escape_is_refused_rather_than_guessed() {
    assert_eq!(unescape(b"trailing\\"), None);
    assert_eq!(unescape(b"\\q"), None, "unknown escape");
    assert_eq!(unescape(b"\\x"), None, "truncated hex");
    assert_eq!(unescape(b"\\xg0"), None, "not hex");
    assert_eq!(unescape(b"plain").as_deref(), Some(&b"plain"[..]));
}

#[test]
fn unicode_stays_readable_in_the_file() {
    let tree = TempTree::new("state-unicode");
    let mut store = store_at(&tree);
    store.set_hidden("/tmp/ünïcödé — 日本語 🎬", Some(true));
    let rendered = text_of(&store);
    assert!(
        rendered.contains("/tmp/ünïcödé — 日本語 🎬\thidden=1"),
        "no reason to hex-escape UTF-8: {rendered}"
    );
}

// ── round trips ─────────────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn gnarly_paths_survive_a_save_and_a_load() {
    let tree = TempTree::new("state-gnarly");
    let paths: Vec<PathBuf> = vec![
        PathBuf::from("/tmp/plain"),
        PathBuf::from("/tmp/with spaces"),
        PathBuf::from("/tmp/ünïcödé — 日本語 🎬"),
        PathBuf::from("/tmp/new\nline"),
        PathBuf::from("/tmp/tab\there"),
        PathBuf::from("/tmp/back\\slash"),
        PathBuf::from("/tmp/equals=sign"),
        PathBuf::from("/tmp/carriage\rreturn"),
        PathBuf::from(format!("/tmp/{}", "x".repeat(255))),
        // Not UTF-8 at all: a Latin-1 name, which is a legal Unix filename and
        // the case a String-keyed store would silently drop.
        PathBuf::from(crate::platform::os::from_bytes(b"/tmp/latin\xff\xfe1").unwrap()),
    ];

    let mut store = store_at(&tree);
    for (i, path) in paths.iter().enumerate() {
        store.set_hidden(path, Some(i % 2 == 0));
    }
    store.flush().unwrap();

    let reloaded = store_at(&tree);
    assert_eq!(reloaded.len(), paths.len());
    for (i, path) in paths.iter().enumerate() {
        assert_eq!(
            reloaded.show_hidden(path),
            Some(i % 2 == 0),
            "{} came back wrong",
            path.display()
        );
    }
}

/// A file written while the view scale was still per directory — Brian has
/// one — loads without it: the `view=` and `scale=` keys are dropped, a line
/// that held nothing else is not a record at all, and the next save writes the
/// rest of each line back without them.
#[test]
fn a_file_from_before_the_view_scale_moved_to_the_tab_loads_without_it() {
    let tree = TempTree::new("state-old-view");
    let body = concat!(
        "# delightfile state v1\n",
        "/tmp/pictures\tview=grid\tscale=roomy\tlinemode=none\tt=100\n",
        "/tmp/only-grid\tview=grid\tt=200\n",
        "/tmp/only-scale\tscale=comfortable\tt=300\n",
        "/tmp/listed\tview=list\tsort=size\tsort_reverse=1\tt=400\n",
    );
    std::fs::write(tree.join("state"), body).unwrap();

    let mut store = store_at(&tree);
    let pictures = Path::new("/tmp/pictures");
    assert_eq!(
        store.get(pictures),
        Some(ViewState {
            linemode: Some(LineMode::None),
            ..ViewState::default()
        }),
        "the rest of the line still loads"
    );
    assert_eq!(
        store.sort(Path::new("/tmp/listed")),
        Some(SortOverride {
            by: SortBy::Size,
            reverse: true
        })
    );
    assert_eq!(store.get(Path::new("/tmp/only-grid")), None);
    assert_eq!(store.get(Path::new("/tmp/only-scale")), None);
    assert_eq!(store.len(), 2, "pictures and listed");
    assert!(!store.is_dirty(), "reading an old file is not a change");

    let rendered = text_of(&store);
    assert!(!rendered.contains("view="), "{rendered}");
    assert!(!rendered.contains("scale="), "{rendered}");

    // Something else changes, the file is rewritten, and the keys are gone
    // from the disk as well as from memory.
    store.set_hidden("/tmp/elsewhere", Some(true));
    store.flush().unwrap();
    let written = std::fs::read_to_string(tree.join("state")).unwrap();
    assert!(!written.contains("view="), "{written}");
    assert!(!written.contains("scale="), "{written}");
    assert!(
        written.contains("/tmp/pictures\tlinemode=none\tt=100\n"),
        "{written}"
    );

    let reloaded = store_at(&tree);
    assert_eq!(reloaded.get(pictures), store.get(pictures));
    assert_eq!(reloaded.len(), 3);
    assert_eq!(text_of(&reloaded), text_of(&store), "and it round-trips");
}

#[test]
fn every_override_round_trips() {
    let tree = TempTree::new("state-overrides");
    let dir = Path::new("/tmp/everything");
    let mut store = store_at(&tree);
    store.set_sort(
        dir,
        Some(SortOverride {
            by: SortBy::Mtime,
            reverse: true,
        }),
    );
    store.set_linemode(dir, Some(LineMode::Owner));
    store.set_hidden(dir, Some(true));
    store.flush().unwrap();

    let reloaded = store_at(&tree);
    assert_eq!(
        reloaded.sort(dir),
        Some(SortOverride {
            by: SortBy::Mtime,
            reverse: true
        })
    );
    assert_eq!(reloaded.linemode(dir), Some(LineMode::Owner));
    assert_eq!(reloaded.show_hidden(dir), Some(true));
    assert_eq!(reloaded.get(dir), store.get(dir));
}

#[test]
fn hidden_false_is_not_the_same_as_unset() {
    let tree = TempTree::new("state-hidden");
    let dir = Path::new("/tmp/hidden");
    let mut store = store_at(&tree);
    store.set_hidden(dir, Some(false));
    store.flush().unwrap();

    let reloaded = store_at(&tree);
    assert_eq!(
        reloaded.show_hidden(dir),
        Some(false),
        "\"I turned them off here\" has to outlive a config change"
    );
}

#[test]
fn every_sort_and_linemode_name_survives_the_trip() {
    let sorts = [
        SortBy::Alphabetical,
        SortBy::Natural,
        SortBy::Extension,
        SortBy::Size,
        SortBy::Mtime,
        SortBy::Btime,
        SortBy::Random,
        SortBy::None,
    ];
    for sort in sorts {
        assert_eq!(SortBy::from_name(sort_name(sort)), Some(sort));
    }
    let modes = [
        LineMode::Size,
        LineMode::Permissions,
        LineMode::Btime,
        LineMode::Mtime,
        LineMode::Owner,
        LineMode::None,
    ];
    for mode in modes {
        assert_eq!(LineMode::from_name(linemode_name(mode)), Some(mode));
    }
    assert_eq!(View::from_name(View::Grid.name()), Some(View::Grid));
    assert_eq!(View::from_name(View::List.name()), Some(View::List));
    assert_eq!(View::List.toggled(), View::Grid);
}

#[test]
fn tabs_round_trip_with_their_focus() {
    let tree = TempTree::new("state-tabs");
    let mut store = store_at(&tree);
    store.set_tabs(
        vec![
            PathBuf::from("/home/brian"),
            PathBuf::from("/tmp/tab\there"),
            PathBuf::from("/var/log"),
        ],
        2,
    );
    store.flush().unwrap();

    let reloaded = store_at(&tree);
    assert_eq!(
        reloaded.tabs(),
        [
            PathBuf::from("/home/brian"),
            PathBuf::from("/tmp/tab\there"),
            PathBuf::from("/var/log"),
        ]
    );
    assert_eq!(reloaded.active_tab(), 2);
}

#[test]
fn an_out_of_range_active_tab_is_clamped() {
    let tree = TempTree::new("state-tabs-clamp");
    let mut store = store_at(&tree);
    store.set_tabs(vec![PathBuf::from("/tmp")], 7);
    assert_eq!(store.active_tab(), 0, "clamped on the way in");

    std::fs::write(
        tree.join("state"),
        format!("{HEADER}\n{TABS_KEY}\t0=/tmp\tactive=9\tt=1\n"),
    )
    .unwrap();
    let reloaded = store_at(&tree);
    assert_eq!(reloaded.active_tab(), 0, "and on the way out");
}

#[test]
fn the_rendered_file_is_stable_between_saves() {
    let tree = TempTree::new("state-stable");
    let mut store = store_at(&tree);
    for i in 0..20 {
        store.set_hidden(format!("/tmp/dir{i}"), Some(true));
    }
    let once = store.render();
    let twice = store.render();
    assert_eq!(once, twice, "sorted keys, so no gratuitous diff");
    assert!(
        String::from_utf8_lossy(&once).starts_with(HEADER),
        "the header identifies the format"
    );
}

// ── override semantics ──────────────────────────────────────────────────────

#[test]
fn clearing_the_last_override_removes_the_record() {
    let tree = TempTree::new("state-clear");
    let dir = Path::new("/tmp/toggled");
    let mut store = store_at(&tree);

    store.set_hidden(dir, Some(true));
    store.set_linemode(dir, Some(LineMode::None));
    assert_eq!(store.len(), 1);

    store.set_hidden(dir, None);
    assert_eq!(store.show_hidden(dir), None, "the override is gone");
    assert_eq!(store.len(), 1, "but the record still holds the linemode");

    store.set_linemode(dir, None);
    assert_eq!(store.len(), 0, "an empty record does not hold an LRU slot");
    assert_eq!(store.get(dir), None);
}

#[test]
fn clear_forgets_a_directory_outright() {
    let tree = TempTree::new("state-forget");
    let dir = Path::new("/tmp/gone");
    let mut store = store_at(&tree);
    store.set_hidden(dir, Some(true));
    assert!(store.clear(dir));
    assert!(!store.clear(dir), "twice is a no-op");
    assert_eq!(store.show_hidden(dir), None);
}

#[test]
fn set_replaces_the_whole_record() {
    let tree = TempTree::new("state-set");
    let dir = Path::new("/tmp/whole");
    let mut store = store_at(&tree);
    store.set_sort(
        dir,
        Some(SortOverride {
            by: SortBy::Size,
            reverse: false,
        }),
    );
    store.set_hidden(dir, Some(true));
    store.set(
        dir,
        ViewState {
            linemode: Some(LineMode::Btime),
            ..ViewState::default()
        },
    );
    assert_eq!(store.sort(dir), None);
    assert_eq!(store.show_hidden(dir), None);
    assert_eq!(store.linemode(dir), Some(LineMode::Btime));
}

#[test]
fn touching_a_directory_nobody_customised_records_nothing() {
    let tree = TempTree::new("state-touch");
    let mut store = store_at(&tree);
    store.touch(Path::new("/tmp/just-visiting"));
    assert_eq!(store.len(), 0, "visiting is not choosing");
    assert!(!store.is_dirty());
}

#[test]
fn the_dirty_flag_tracks_real_changes() {
    let tree = TempTree::new("state-dirty");
    let mut store = store_at(&tree);
    assert!(!store.is_dirty(), "a fresh load owes nothing");
    store.set_hidden("/tmp/a", Some(true));
    assert!(store.is_dirty());
    store.flush().unwrap();
    assert!(!store.is_dirty(), "a flush settles the debt");
    store.flush().unwrap();
    assert!(!store.is_dirty());
}

// ── the LRU ─────────────────────────────────────────────────────────────────

#[test]
fn the_least_recently_touched_directory_is_the_one_evicted() {
    let tree = TempTree::new("state-lru");
    let mut store = store_at(&tree);
    // Written straight into the map with explicit timestamps: a test cannot
    // wait out a second per record, and the ordering is what is under test.
    for i in 0..MAX_STATE_ENTRIES {
        store.set_hidden(format!("/tmp/d{i:05}"), Some(true));
    }
    assert_eq!(store.len(), MAX_STATE_ENTRIES);

    store.set_hidden("/tmp/newcomer", Some(true));
    assert_eq!(store.len(), MAX_STATE_ENTRIES, "the cap holds");
    assert_eq!(
        store.show_hidden(Path::new("/tmp/newcomer")),
        Some(true),
        "the newest survives"
    );
    // Same-second timestamps break the tie by path, so the lowest-sorting of
    // the originals is the one that went.
    assert_eq!(store.show_hidden(Path::new("/tmp/d00000")), None);
}

#[test]
fn an_oversized_file_is_trimmed_on_load() {
    let tree = TempTree::new("state-trim");
    let mut body = String::from(HEADER);
    body.push('\n');
    for i in 0..MAX_STATE_ENTRIES + 20 {
        body.push_str(&format!("/tmp/d{i:05}\thidden=1\tt={}\n", 1000 + i));
    }
    std::fs::write(tree.join("state"), body).unwrap();

    let store = store_at(&tree);
    assert_eq!(store.len(), MAX_STATE_ENTRIES);
    assert_eq!(
        store.show_hidden(Path::new("/tmp/d00000")),
        None,
        "the oldest timestamps lost"
    );
    let newest = format!("/tmp/d{:05}", MAX_STATE_ENTRIES + 19);
    assert_eq!(store.show_hidden(Path::new(&newest)), Some(true));
}

#[test]
fn touch_moves_a_record_up_the_queue() {
    let tree = TempTree::new("state-touch-lru");
    let mut body = String::from(HEADER);
    body.push('\n');
    body.push_str("/tmp/old\thidden=1\tt=1\n");
    body.push_str("/tmp/older\thidden=1\tt=0\n");
    std::fs::write(tree.join("state"), body).unwrap();

    let mut store = store_at(&tree);
    store.touch(Path::new("/tmp/older"));
    assert!(store.is_dirty(), "a touch is worth saving");
    let rendered = text_of(&store);
    let older_line = rendered
        .lines()
        .find(|l| l.starts_with("/tmp/older"))
        .expect("older");
    assert!(
        !older_line.contains("\tt=0"),
        "the timestamp moved to now: {older_line}"
    );
}

// ── corruption ──────────────────────────────────────────────────────────────

#[test]
fn corrupt_lines_are_skipped_and_the_good_ones_load() {
    let tree = TempTree::new("state-corrupt");
    let body = concat!(
        "# delightfile state v1\n",
        "\n",
        "# a comment in the middle\n",
        "/tmp/good\thidden=1\tt=100\n",
        "relative/path\thidden=1\n",
        "/tmp/bad-escape\\q\thidden=1\n",
        "/tmp/no-equals\thidden1\n",
        "/tmp/unknown-key\tlinemode=owner\tfuture_setting=7\n",
        "/tmp/unknown-value\tlinemode=hologram\n",
        "\u{0}garbage\u{1}\u{2}\n",
        "/tmp/also-good\tsort=size\tsort_reverse=1\tt=200\n",
    );
    std::fs::write(tree.join("state"), body).unwrap();

    let store = store_at(&tree);
    assert_eq!(store.show_hidden(Path::new("/tmp/good")), Some(true));
    assert_eq!(
        store.sort(Path::new("/tmp/also-good")),
        Some(SortOverride {
            by: SortBy::Size,
            reverse: true
        }),
        "a good line after the bad ones still loads"
    );
    assert_eq!(
        store.linemode(Path::new("/tmp/unknown-key")),
        Some(LineMode::Owner),
        "an unknown key does not cost the line"
    );
    assert_eq!(
        store.get(Path::new("relative/path")),
        None,
        "keys must be absolute"
    );
    assert_eq!(store.get(Path::new("/tmp/no-equals")), None);
    assert_eq!(
        store.get(Path::new("/tmp/unknown-value")),
        None,
        "an unreadable value leaves nothing to remember"
    );
    assert_eq!(store.len(), 3, "good, also-good, unknown-key");
}

#[test]
fn a_corrupt_file_is_repaired_by_the_next_flush() {
    let tree = TempTree::new("state-repair");
    std::fs::write(
        tree.join("state"),
        "garbage\n/tmp/good\thidden=1\tt=1\nmore garbage\\q\n",
    )
    .unwrap();

    let mut store = store_at(&tree);
    store.set_hidden("/tmp/second", Some(false));
    store.flush().unwrap();

    let written = std::fs::read_to_string(tree.join("state")).unwrap();
    assert!(!written.contains("garbage"), "rewritten whole: {written}");
    assert!(written.starts_with(HEADER));

    let reloaded = store_at(&tree);
    assert_eq!(reloaded.show_hidden(Path::new("/tmp/good")), Some(true));
    assert_eq!(reloaded.show_hidden(Path::new("/tmp/second")), Some(false));
}

#[test]
fn an_enormous_file_is_ignored_rather_than_parsed() {
    let tree = TempTree::new("state-huge");
    let path = tree.join("state");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_STATE_BYTES + 1).unwrap();
    drop(file);

    let store = StateStore::load_from(&path);
    assert_eq!(store.len(), 0, "not this program's file");
    assert!(!store.is_dirty());
}

#[test]
fn a_missing_file_is_an_empty_store_not_an_error() {
    let tree = TempTree::new("state-missing");
    let store = StateStore::load_from(tree.join("never-written"));
    assert_eq!(store.len(), 0);
    assert!(store.tabs().is_empty());
}

// ── saving ──────────────────────────────────────────────────────────────────

#[test]
fn the_save_is_atomic_and_leaves_no_temp_file_behind() {
    let tree = TempTree::new("state-atomic");
    let dir = tree.dir("nested/deeper");
    let mut store = StateStore::load_from(dir.join("state"));
    store.set_hidden("/tmp/a", Some(true));
    store.flush().unwrap();

    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "state")
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files were cleaned up: {leftovers:?}"
    );
    assert!(dir.join("state").is_file());
}

#[test]
fn flush_creates_the_directory_it_needs() {
    let tree = TempTree::new("state-mkdir");
    let path = tree.join("a/b/c/state");
    let mut store = StateStore::load_from(&path);
    store.set_hidden("/tmp/a", Some(true));
    store.flush().unwrap();
    assert!(path.is_file(), "the whole chain was created");
}

#[test]
fn a_pathless_store_works_for_the_session_and_saves_nothing() {
    let mut store = StateStore::load_from(PathBuf::new());
    store.set_hidden("/tmp/a", Some(true));
    assert_eq!(store.show_hidden(Path::new("/tmp/a")), Some(true));
    store.flush().unwrap();
    assert!(!store.is_dirty(), "nowhere to write is not a failure");
    assert_eq!(store.path(), Path::new(""));
}

#[test]
fn flushing_nothing_writes_nothing() {
    let tree = TempTree::new("state-clean");
    let mut store = store_at(&tree);
    store.flush().unwrap();
    assert!(
        !tree.join("state").exists(),
        "a clean store does not create a file just to say it has nothing"
    );
}

#[test]
fn the_state_path_follows_xdg() {
    use std::ffi::OsString;

    assert_eq!(
        state_path_from(Some(OsString::from("/xdg/state")), None),
        Some(PathBuf::from("/xdg/state/delightfile/state")),
        "$XDG_STATE_HOME wins"
    );
    assert_eq!(
        state_path_from(None, Some(OsString::from("/home/brian"))),
        Some(PathBuf::from("/home/brian/.local/state/delightfile/state")),
        "the XDG-specified default, not a guess"
    );
    assert_eq!(
        state_path_from(Some(OsString::new()), Some(OsString::from("/home/brian"))),
        Some(PathBuf::from("/home/brian/.local/state/delightfile/state")),
        "an empty variable is an unset one"
    );
    assert_eq!(
        state_path_from(None, None),
        None,
        "nowhere to write is a session-only store, not a panic"
    );
    assert_eq!(
        state_path_from(Some(OsString::new()), Some(OsString::new())),
        None
    );
}

// ── the file main wrote ─────────────────────────────────────────────────────

/// `testdata/state-419a776` is the file `main` at 419a776 rendered — before
/// the path model of `plans/other-platforms/03-paths.md` touched how a key is
/// read or written — from a store holding one of everything: directory
/// records whose keys need every escape, a key that is not UTF-8, the root,
/// tabs, the panes, pins (`~`, a URL, an escape) and tags. Linux must go on
/// reading it and writing it back unchanged, byte for byte: the file is
/// Brian's, on disk now, and a load that dropped or rewrote a line of it would
/// be the port changing Linux.
///
/// Unix only, as the file is: its keys are Unix absolute paths and one is not
/// UTF-8, neither of which a Windows store reads.
#[cfg(unix)]
#[test]
fn the_state_file_main_wrote_loads_and_saves_byte_for_byte() {
    use std::os::unix::ffi::OsStrExt;

    let written: &[u8] = include_bytes!("testdata/state-419a776");
    let tree = TempTree::new("state-main");
    std::fs::write(tree.join("state"), written).unwrap();
    let mut store = store_at(&tree);

    assert_eq!(store.len(), 10, "every directory record");
    assert_eq!(
        store.linemode(Path::new(std::ffi::OsStr::from_bytes(
            b"/tmp/caf\xe9 latin-1"
        ))),
        Some(LineMode::Size),
        "the key that is not UTF-8, as the bytes it was"
    );
    assert_eq!(store.linemode(Path::new("/")), Some(LineMode::Owner));
    assert_eq!(store.show_hidden(Path::new("/tmp/tab\there")), Some(false));
    assert_eq!(
        store.show_hidden(Path::new("/tmp/new\nline and\rreturn")),
        Some(true)
    );
    assert_eq!(
        store.sort(Path::new("/home/brian/src")),
        Some(SortOverride {
            by: SortBy::Mtime,
            reverse: true
        })
    );
    assert_eq!(store.tabs().len(), 3);
    assert_eq!(store.active_tab(), 1);
    assert_eq!(
        store
            .pins()
            .iter()
            .map(|pin| pin.path.as_str())
            .collect::<Vec<_>>(),
        [
            "~/Work",
            "/mnt/plex",
            "sftp://showandtour1/srv",
            "/tmp/tab\there=eq"
        ]
    );
    assert_eq!(store.known_tags(), ["work", "Grüße"]);
    assert!(store.panes().is_some());
    assert!(!store.is_dirty(), "reading it is not a change");

    assert_eq!(store.render(), written, "rendered back byte for byte");
    // And through a real save: a change made and unmade marks the store
    // dirty, and the file it writes is the file it read.
    store.set_hidden("/tmp/tab\there", Some(true));
    store.set_hidden("/tmp/tab\there", Some(false));
    store.flush().unwrap();
    let saved = std::fs::read(tree.join("state")).unwrap();
    let saved_text = String::from_utf8_lossy(&saved).into_owned();
    let written_text = String::from_utf8_lossy(written).into_owned();
    // The one line the edit touched carries a new time; every other line is
    // the one main wrote.
    let untouched = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| !line.starts_with("/tmp/tab\\there\t"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(untouched(&saved_text), untouched(&written_text));
}
