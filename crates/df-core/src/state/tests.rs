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
    store.set_view("/tmp/ünïcödé — 日本語 🎬", Some(View::Grid));
    let rendered = text_of(&store);
    assert!(
        rendered.contains("/tmp/ünïcödé — 日本語 🎬\tview=grid"),
        "no reason to hex-escape UTF-8: {rendered}"
    );
}

// ── round trips ─────────────────────────────────────────────────────────────

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
        {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(
                b"/tmp/latin\xff\xfe1".to_vec(),
            ))
        },
    ];

    let mut store = store_at(&tree);
    for (i, path) in paths.iter().enumerate() {
        store.set_view(path, Some(if i % 2 == 0 { View::Grid } else { View::List }));
    }
    store.flush().unwrap();

    let reloaded = store_at(&tree);
    assert_eq!(reloaded.len(), paths.len());
    for (i, path) in paths.iter().enumerate() {
        let expected = if i % 2 == 0 { View::Grid } else { View::List };
        assert_eq!(
            reloaded.view(path),
            Some(expected),
            "{} came back wrong",
            path.display()
        );
    }
}

#[test]
fn every_override_round_trips() {
    let tree = TempTree::new("state-overrides");
    let dir = Path::new("/tmp/everything");
    let mut store = store_at(&tree);
    store.set_view(dir, Some(View::Grid));
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
    assert_eq!(reloaded.view(dir), Some(View::Grid));
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
        store.set_view(format!("/tmp/dir{i}"), Some(View::Grid));
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

    store.set_view(dir, Some(View::Grid));
    store.set_linemode(dir, Some(LineMode::None));
    assert_eq!(store.len(), 1);

    store.set_view(dir, None);
    assert_eq!(store.view(dir), None, "the override is gone");
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
    store.set_view(dir, Some(View::Grid));
    assert!(store.clear(dir));
    assert!(!store.clear(dir), "twice is a no-op");
    assert_eq!(store.view(dir), None);
}

#[test]
fn set_replaces_the_whole_record() {
    let tree = TempTree::new("state-set");
    let dir = Path::new("/tmp/whole");
    let mut store = store_at(&tree);
    store.set_view(dir, Some(View::Grid));
    store.set_hidden(dir, Some(true));
    store.set(
        dir,
        ViewState {
            linemode: Some(LineMode::Btime),
            ..ViewState::default()
        },
    );
    assert_eq!(store.view(dir), None);
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
    store.set_view("/tmp/a", Some(View::Grid));
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
        store.set_view(format!("/tmp/d{i:05}"), Some(View::Grid));
    }
    assert_eq!(store.len(), MAX_STATE_ENTRIES);

    store.set_view("/tmp/newcomer", Some(View::Grid));
    assert_eq!(store.len(), MAX_STATE_ENTRIES, "the cap holds");
    assert_eq!(
        store.view(Path::new("/tmp/newcomer")),
        Some(View::Grid),
        "the newest survives"
    );
    // Same-second timestamps break the tie by path, so the lowest-sorting of
    // the originals is the one that went.
    assert_eq!(store.view(Path::new("/tmp/d00000")), None);
}

#[test]
fn an_oversized_file_is_trimmed_on_load() {
    let tree = TempTree::new("state-trim");
    let mut body = String::from(HEADER);
    body.push('\n');
    for i in 0..MAX_STATE_ENTRIES + 20 {
        body.push_str(&format!("/tmp/d{i:05}\tview=grid\tt={}\n", 1000 + i));
    }
    std::fs::write(tree.join("state"), body).unwrap();

    let store = store_at(&tree);
    assert_eq!(store.len(), MAX_STATE_ENTRIES);
    assert_eq!(
        store.view(Path::new("/tmp/d00000")),
        None,
        "the oldest timestamps lost"
    );
    let newest = format!("/tmp/d{:05}", MAX_STATE_ENTRIES + 19);
    assert_eq!(store.view(Path::new(&newest)), Some(View::Grid));
}

#[test]
fn touch_moves_a_record_up_the_queue() {
    let tree = TempTree::new("state-touch-lru");
    let mut body = String::from(HEADER);
    body.push('\n');
    body.push_str("/tmp/old\tview=grid\tt=1\n");
    body.push_str("/tmp/older\tview=grid\tt=0\n");
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
        "/tmp/good\tview=grid\tt=100\n",
        "relative/path\tview=grid\n",
        "/tmp/bad-escape\\q\tview=grid\n",
        "/tmp/no-equals\tviewgrid\n",
        "/tmp/unknown-key\tview=list\tfuture_setting=7\n",
        "/tmp/unknown-value\tview=hologram\n",
        "\u{0}garbage\u{1}\u{2}\n",
        "/tmp/also-good\tsort=size\tsort_reverse=1\tt=200\n",
    );
    std::fs::write(tree.join("state"), body).unwrap();

    let store = store_at(&tree);
    assert_eq!(store.view(Path::new("/tmp/good")), Some(View::Grid));
    assert_eq!(
        store.sort(Path::new("/tmp/also-good")),
        Some(SortOverride {
            by: SortBy::Size,
            reverse: true
        }),
        "a good line after the bad ones still loads"
    );
    assert_eq!(
        store.view(Path::new("/tmp/unknown-key")),
        Some(View::List),
        "an unknown key does not cost the line"
    );
    assert_eq!(
        store.view(Path::new("relative/path")),
        None,
        "keys must be absolute"
    );
    assert_eq!(store.view(Path::new("/tmp/no-equals")), None);
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
        "garbage\n/tmp/good\tview=grid\tt=1\nmore garbage\\q\n",
    )
    .unwrap();

    let mut store = store_at(&tree);
    store.set_view("/tmp/second", Some(View::List));
    store.flush().unwrap();

    let written = std::fs::read_to_string(tree.join("state")).unwrap();
    assert!(!written.contains("garbage"), "rewritten whole: {written}");
    assert!(written.starts_with(HEADER));

    let reloaded = store_at(&tree);
    assert_eq!(reloaded.view(Path::new("/tmp/good")), Some(View::Grid));
    assert_eq!(reloaded.view(Path::new("/tmp/second")), Some(View::List));
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
    store.set_view("/tmp/a", Some(View::Grid));
    store.flush().unwrap();

    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "state")
        .collect();
    assert!(leftovers.is_empty(), "temp files were cleaned up: {leftovers:?}");
    assert!(dir.join("state").is_file());
}

#[test]
fn flush_creates_the_directory_it_needs() {
    let tree = TempTree::new("state-mkdir");
    let path = tree.join("a/b/c/state");
    let mut store = StateStore::load_from(&path);
    store.set_view("/tmp/a", Some(View::Grid));
    store.flush().unwrap();
    assert!(path.is_file(), "the whole chain was created");
}

#[test]
fn a_pathless_store_works_for_the_session_and_saves_nothing() {
    let mut store = StateStore::load_from(PathBuf::new());
    store.set_view("/tmp/a", Some(View::Grid));
    assert_eq!(store.view(Path::new("/tmp/a")), Some(View::Grid));
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
    assert_eq!(state_path_from(Some(OsString::new()), Some(OsString::new())), None);
}
