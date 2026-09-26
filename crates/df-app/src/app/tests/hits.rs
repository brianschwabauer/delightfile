//! PLAN §7.2's hits as a listing, through the app: the panel's `Enter`, the
//! rows it makes, the ways out, the refusals, the rows following an operation,
//! and a walk still streaming into them. The search's process is a test
//! [`search::Feed`] rather than fd or rg, so what arrives, and when, is the
//! test's to say.

use super::*;

/// A fixture with a small tree under it: `a.txt` and `b.txt` at the top,
/// `src/foo.txt`, `src/deep/foo.txt` and `docs/foo.txt` below, and `src/deep`
/// itself a folder a names search can find.
fn tree(name: &str) -> Fixture {
    let app = Fixture::with_folders(name, &["a.txt", "b.txt"], &["src/deep", "docs"]);
    for file in ["src/foo.txt", "src/deep/foo.txt", "docs/foo.txt"] {
        std::fs::write(app.files.join(file), b"let x = 1;\nfoo here\n").expect("write the tree");
    }
    app
}

/// A names hit for `relative`, as fd's line for it parses.
fn named(app: &App, relative: &str) -> search::Hit {
    search::parse(search::Mode::Names, &app.cwd(), "foo", relative).expect("a names hit")
}

/// A content hit on `line` of `relative`, as rg's line for it parses.
fn matched(app: &App, relative: &str, line: usize, text: &str) -> search::Hit {
    let raw = format!("{relative}\u{0}{line}:1:{text}");
    search::parse(search::Mode::Content, &app.cwd(), "foo", &raw).expect("a content hit")
}

/// The panel open in `mode` on `query`, with a [`search::Feed`] standing in
/// for its process.
fn panel(app: &mut App, mode: search::Mode, query: &str) -> search::Feed {
    let now = Instant::now();
    app.run(
        match mode {
            search::Mode::Names => Command::SearchName,
            search::Mode::Content => Command::SearchContent,
        },
        10,
        now,
    );
    let search = app.search.as_mut().expect("the panel opened");
    search.seed(query, now);
    search.feed()
}

/// Everything the feed has sent, into the panel or the listing, the way a
/// frame's `poll_workers` takes it.
fn poll(app: &mut App) {
    app.poll_workers();
}

/// The listing's row names, in the pane's order.
fn rows(app: &App) -> Vec<String> {
    app.tab()
        .cwd
        .dir
        .rows()
        .map(|(entry, _)| entry.name.clone())
        .collect()
}

fn cursor(app: &App) -> Option<String> {
    app.tab().cwd.dir.cursor_entry().map(|e| e.name.clone())
}

/// Three names hits in, the second highlighted, `Enter`: the listing, finished.
fn commit_three(app: &mut App) {
    let feed = panel(app, search::Mode::Names, "foo");
    feed.hits(vec![
        named(app, "src/foo.txt"),
        named(app, "src/deep/foo.txt"),
        named(app, "docs/foo.txt"),
    ]);
    feed.done(false);
    poll(app);
    app.search.as_mut().expect("the panel").set_cursor(1);
    app.overlay_key(Chord::plain(Key::Enter), 10, Instant::now());
}

/// `Enter` in the panel: every hit is a row named by its path from the
/// root, the real file under it, and the cursor on the hit that was
/// highlighted. The panel is gone and the tab is still, as far as the history
/// knows, in the root.
#[test]
fn enter_lists_the_hits_by_their_path_from_the_root() {
    let mut app = tree("hits-commit");
    let root = app.files.clone();
    commit_three(&mut app);

    assert!(app.search.is_none(), "the panel stayed up");
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));
    assert_eq!(app.tab().cwd.path(), root, "the listing's path is the root");
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["docs/foo.txt", "src/deep/foo.txt", "src/foo.txt"]);
    assert_eq!(cursor(&app).as_deref(), Some("src/deep/foo.txt"));
    let entry = app.tab().cwd.dir.cursor_entry().expect("a row");
    assert_eq!(entry.path, root.join("src/deep/foo.txt"), "the real file");
    // …so every "this listing's path plus a name" is the file.
    app.run(Command::ToggleSelect, 10, Instant::now());
    assert_eq!(
        app.tab().cwd.dir.selected_paths(),
        vec![root.join("src/deep/foo.txt")]
    );
    assert_eq!(app.tab().history.current(), root.as_path());
    assert_eq!(app.tab().title(), "s foo");
}

/// `←` goes back to the root folder with the cursor where it was when the
/// search was committed, and pushes nothing on the history.
#[test]
fn left_returns_to_the_root_with_the_cursor_where_it_was() {
    let mut app = tree("hits-leave");
    let root = app.files.clone();
    app.dir().cursor_to_name("b.txt");
    commit_three(&mut app);
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));

    app.run(Command::Leave, 10, Instant::now());
    settle_here(&mut app);
    assert_eq!(app.tab().virtual_kind(), None);
    assert_eq!(app.tab().cwd.path(), root);
    assert_eq!(cursor(&app).as_deref(), Some("b.txt"));
    assert!(
        !app.tab().history.can_go_back(),
        "leaving the hits is not a step in the history"
    );
}

/// `Esc` takes back the filter, then the selection, and only then — its last
/// rung — leaves the hits.
#[test]
fn escape_leaves_the_hits_last() {
    let mut app = tree("hits-escape");
    commit_three(&mut app);
    let now = Instant::now();
    app.run(Command::SelectAll, 10, now);
    app.dir().set_filter("src");

    app.run(Command::Escape, 10, now);
    assert!(
        app.tab().cwd.dir.filter().is_empty(),
        "the filter went first"
    );
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));
    app.run(Command::Escape, 10, now);
    assert_eq!(app.tab().cwd.dir.selected_count(), 0, "then the selection");
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));
    app.run(Command::Escape, 10, now);
    assert_eq!(app.tab().virtual_kind(), None, "then the hits");
}

/// `Alt+Enter` goes to the hit's own folder with the cursor on it — a hit at
/// the top of the tree included, whose folder is the root — and anywhere
/// else says the file is in its folder already.
#[test]
fn alt_enter_goes_to_the_hits_own_folder() {
    let mut app = tree("hits-reveal");
    let root = app.files.clone();
    commit_three(&mut app);
    let now = Instant::now();

    app.run(Command::Reveal, 10, now);
    settle_here(&mut app);
    assert_eq!(app.tab().virtual_kind(), None);
    assert_eq!(app.tab().cwd.path(), root.join("src/deep"));
    assert_eq!(cursor(&app).as_deref(), Some("foo.txt"));

    app.run(Command::Reveal, 10, now);
    assert_eq!(toast_text(&app), Some("Already in its folder"));

    // A hit whose folder is the root: the same path as the listing, and still
    // leaving it.
    app.navigate(root.clone(), now);
    settle_here(&mut app);
    let feed = panel(&mut app, search::Mode::Names, "a");
    feed.hits(vec![named(&app, "a.txt")]);
    feed.done(false);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));
    app.run(Command::Reveal, 10, now);
    settle_here(&mut app);
    assert_eq!(app.tab().virtual_kind(), None);
    assert_eq!(app.tab().cwd.path(), root);
    assert_eq!(cursor(&app).as_deref(), Some("a.txt"));
}

/// `Alt+Enter` in the panel is what `Enter` used to be: the highlighted hit's
/// folder, the cursor on it, and no listing.
#[test]
fn alt_enter_in_the_panel_goes_to_one_hit() {
    let mut app = tree("hits-panel-reveal");
    let root = app.files.clone();
    let feed = panel(&mut app, search::Mode::Names, "foo");
    feed.hits(vec![named(&app, "docs/foo.txt")]);
    poll(&mut app);
    app.overlay_key(Chord::alt(Key::Enter), 10, Instant::now());
    settle_here(&mut app);
    assert!(app.search.is_none());
    assert_eq!(app.tab().virtual_kind(), None);
    assert_eq!(app.tab().cwd.path(), root.join("docs"));
    assert_eq!(cursor(&app).as_deref(), Some("foo.txt"));
}

/// `→` on a directory hit enters it as a folder, and the history holds the
/// root rather than the hits: `Alt+←` from there is the root folder.
#[test]
fn right_on_a_directory_hit_enters_it_and_back_is_the_root() {
    let mut app = tree("hits-enter");
    let root = app.files.clone();
    let now = Instant::now();
    let feed = panel(&mut app, search::Mode::Names, "deep");
    feed.hits(vec![named(&app, "src/deep")]);
    feed.done(false);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    assert_eq!(cursor(&app).as_deref(), Some("src/deep"));

    app.run(Command::EnterDirectory, 10, now);
    settle_here(&mut app);
    assert_eq!(app.tab().virtual_kind(), None);
    assert_eq!(app.tab().cwd.path(), root.join("src/deep"));

    app.run(Command::HistoryBack, 10, now);
    settle_here(&mut app);
    assert_eq!(app.tab().cwd.path(), root);
    assert_eq!(
        app.tab().virtual_kind(),
        None,
        "the hits are not in the history"
    );
}

/// `f` in the hits filters on the whole relative path, so a folder's name
/// narrows to what is under it.
#[test]
fn the_filter_matches_the_path_from_the_root() {
    let mut app = tree("hits-filter");
    commit_three(&mut app);
    app.dir().set_filter("src/");
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["src/deep/foo.txt", "src/foo.txt"]);
}

/// `Enter` on a filter that has hidden every hit hands the letters to the
/// panel, as it does in a folder — with the same root.
#[test]
fn a_filter_that_hides_every_hit_hands_over_to_the_panel() {
    let mut app = tree("hits-ran-out");
    let root = app.files.clone();
    commit_three(&mut app);
    app.run(Command::Filter, 10, Instant::now());
    for c in "zzz".chars() {
        app.prompt_key(Chord::from_char(c).expect("a letter"), Instant::now());
    }
    assert!(app.tab().cwd.dir.is_empty());
    app.prompt_key(Chord::plain(Key::Enter), Instant::now());
    let search = app.search.as_ref().expect("the panel took the letters");
    assert_eq!(search.query(), "zzz");
    assert_eq!(search.root, root);
}

/// `r` on a hit renames the file where it is — in its own folder — and the
/// row follows, the cursor with it. The prompt holds the file's own name.
#[test]
fn a_renamed_hit_is_renamed_in_its_folder_and_its_row_follows() {
    let mut app = tree("hits-rename");
    let root = app.files.clone();
    commit_three(&mut app);
    let now = Instant::now();
    app.run(Command::Rename, 10, now);
    let prompt = app.prompt.as_ref().expect("the rename prompt");
    assert_eq!(prompt.query(), "foo.txt", "the file's name, not its path");
    app.submit_prompt("bar.txt".to_string(), now);

    assert!(
        root.join("src/deep/bar.txt").exists(),
        "renamed in its folder"
    );
    assert!(!root.join("bar.txt").exists(), "not moved into the root");
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["docs/foo.txt", "src/deep/bar.txt", "src/foo.txt"]);
    assert_eq!(cursor(&app).as_deref(), Some("src/deep/bar.txt"));
    assert_eq!(
        app.tab().cwd.dir.cursor_entry().map(|e| e.path.clone()),
        Some(root.join("src/deep/bar.txt"))
    );
}

/// `u` after a rename in the hits takes the file back to its old name, and
/// the row goes back with it rather than vanishing under a name it no longer
/// has.
#[test]
fn undoing_a_rename_renames_the_row_back() {
    let mut app = tree("hits-undo");
    let root = app.files.clone();
    commit_three(&mut app);
    let now = Instant::now();
    app.run(Command::Rename, 10, now);
    app.submit_prompt("bar.txt".to_string(), now);
    assert!(rows(&app).contains(&"src/deep/bar.txt".to_string()));

    app.run(Command::Undo, 10, now);
    assert!(root.join("src/deep/foo.txt").exists());
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["docs/foo.txt", "src/deep/foo.txt", "src/foo.txt"]);
}

/// `Ctrl+s` in the listing stops a walk still filling it: the rows it found
/// stay, the listing is finished, and the chip loses its `…`.
#[test]
fn ctrl_s_stops_a_walk_and_keeps_what_it_found() {
    let mut app = tree("hits-stop");
    let feed = panel(&mut app, search::Mode::Names, "foo");
    feed.hits(vec![named(&app, "src/foo.txt")]);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, Instant::now());
    assert!(app.tab().hits.as_ref().is_some_and(|v| v.running()));

    app.run(Command::CancelSearch, 10, Instant::now());
    assert!(app.tab().hits.as_ref().is_some_and(|v| !v.running()));
    assert_eq!(app.tab().cwd.dir.state(), df_core::fs::LoadState::Loaded);
    assert_eq!(rows(&app), ["src/foo.txt"]);
    // What the stopped process had still queued does not land.
    feed.hits(vec![named(&app, "docs/foo.txt")]);
    poll(&mut app);
    assert_eq!(rows(&app), ["src/foo.txt"]);
    app.sync_path_bar();
    assert_eq!(
        app.path_bar.1.last().map(|c| c.label.as_str()),
        Some("s foo · 1")
    );
}

/// `Enter` with nothing to list keeps the panel: a finished search that found
/// nothing, and a contents search with no pattern, which has asked nothing.
#[test]
fn enter_with_nothing_to_list_keeps_the_panel() {
    let mut app = tree("hits-nothing");
    let now = Instant::now();
    let feed = panel(&mut app, search::Mode::Names, "zzz");
    feed.done(false);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    assert!(app.search.is_some(), "the panel went");
    assert_eq!(app.tab().virtual_kind(), None);
    assert_eq!(toast_text(&app), Some("Nothing found to list"));

    app.close_overlay(now);
    app.run(Command::SearchContent, 10, now);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    assert!(app.search.is_some(), "the panel went");
    assert_eq!(app.tab().virtual_kind(), None);
}

/// A row whose file has gone is taken out when anything this program does
/// touches the tree — the trash view's re-read, for a listing with no
/// directory to rescan.
#[test]
fn a_hit_whose_file_went_leaves_the_listing() {
    let mut app = tree("hits-gone");
    let root = app.files.clone();
    commit_three(&mut app);
    std::fs::remove_file(root.join("docs/foo.txt")).expect("remove");
    app.rescan(&root.join("docs"), Instant::now());
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["src/deep/foo.txt", "src/foo.txt"]);
}

/// The bulk card over hits from three folders: each row labelled by its path,
/// each file renamed in its own folder, two `foo.txt` becoming two `baz.txt`
/// without colliding — and the rows follow.
#[test]
fn the_bulk_card_renames_hits_across_folders() {
    let mut app = tree("hits-bulk");
    let root = app.files.clone();
    commit_three(&mut app);
    let now = Instant::now();
    app.run(Command::SelectAll, 10, now);
    app.run(Command::Rename, 10, now);
    let Some(Dialog::Bulk(bulk)) = &mut app.dialog else {
        panic!("no card");
    };
    let mut labels: Vec<String> = (0..bulk.len()).map(|row| bulk.label(row)).collect();
    labels.sort();
    assert_eq!(labels, ["docs/foo.txt", "src/deep/foo.txt", "src/foo.txt"]);
    // Every row to `baz.txt`: three of one name, in three folders, so none
    // of them collides.
    bulk.key(Chord::ctrl(Key::Char('u')));
    for c in "baz.txt".chars() {
        bulk.key(Chord::from_char(c).expect("a letter"));
    }
    assert!(bulk.valid(), "{:?}", bulk.problems());
    app.submit_bulk(now);

    for file in ["src/baz.txt", "src/deep/baz.txt", "docs/baz.txt"] {
        assert!(root.join(file).exists(), "{file}");
    }
    assert!(!root.join("baz.txt").exists(), "a rename moved a file");
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["docs/baz.txt", "src/baz.txt", "src/deep/baz.txt"]);
}

/// The verbs that would make something *here* are refused out loud, and so
/// is `.`; every one of them is greyed where a menu offers it.
#[test]
fn making_something_in_the_hits_is_refused() {
    let mut app = tree("hits-refused");
    commit_three(&mut app);
    let now = Instant::now();
    let before = app.tab().cwd.dir.total();

    app.run(Command::Create, 10, now);
    assert!(app.prompt.is_none(), "`a` opened its prompt");
    assert!(
        toast_text(&app).is_some_and(|t| t.starts_with("Search results are not a folder")),
        "{:?}",
        toast_text(&app)
    );

    app.clipboard = Clipboard::yank([app.files.join("a.txt")]);
    app.run(Command::Paste, 10, now);
    assert!(app.dialog.is_none());
    assert!(toast_text(&app).is_some_and(|t| t.contains("paste")));
    assert_eq!(app.tab().cwd.dir.total(), before, "nothing landed");

    app.run(Command::ToggleHidden, 10, now);
    assert_eq!(
        toast_text(&app),
        Some("Search again to include hidden files")
    );
    app.run(Command::PinToggle, 10, now);
    assert!(toast_text(&app).is_some_and(|t| t.contains("not a folder to pin")));

    for command in [
        Command::Create,
        Command::Paste,
        Command::PasteSync,
        Command::PinToggle,
    ] {
        assert!(app.refusal(command).is_some(), "{} is live", command.id());
    }
    for command in [Command::Rename, Command::Trash, Command::Yank] {
        assert_eq!(app.refusal(command), None, "{} is refused", command.id());
    }
}

/// A walk still running when `Enter` came keeps landing in the listing, sorted
/// as it comes, and the cursor stays on the row it was on.
#[test]
fn a_streaming_search_adds_rows_without_moving_the_cursor() {
    let mut app = tree("hits-stream");
    let now = Instant::now();
    let feed = panel(&mut app, search::Mode::Names, "foo");
    feed.hits(vec![named(&app, "src/foo.txt")]);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    assert_eq!(rows(&app), ["src/foo.txt"]);
    assert_eq!(
        app.tab().cwd.dir.state(),
        df_core::fs::LoadState::Loading,
        "still arriving"
    );

    // Two more land, one sorting above the cursor's row and one below.
    feed.hits(vec![
        named(&app, "docs/foo.txt"),
        named(&app, "src/deep/foo.txt"),
    ]);
    poll(&mut app);
    assert_eq!(rows(&app).len(), 3);
    assert_eq!(
        cursor(&app).as_deref(),
        Some("src/foo.txt"),
        "the cursor moved"
    );

    feed.done(false);
    poll(&mut app);
    assert_eq!(app.tab().cwd.dir.state(), df_core::fs::LoadState::Loaded);
}

/// The chip at the end of the breadcrumb: the query and the count, `…` while
/// the walk runs, `· truncated` when the cap cut it off — and a click on it
/// puts the query back in the panel, in the same mode and the same root.
#[test]
fn the_chip_says_how_the_search_is_going_and_reopens_it() {
    let mut app = tree("hits-chip");
    let root = app.files.clone();
    let now = Instant::now();
    let feed = panel(&mut app, search::Mode::Names, "foo");
    feed.hits(vec![
        named(&app, "src/foo.txt"),
        named(&app, "docs/foo.txt"),
    ]);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);

    let chip = |app: &mut App| {
        app.sync_path_bar();
        let crumb = app.path_bar.1.last().expect("crumbs").clone();
        assert!(crate::hits::is_chip(&crumb), "{crumb:?} is not the chip");
        assert!(crumb.accent);
        crumb.label
    };
    assert_eq!(chip(&mut app), "s foo · 2…");
    feed.done(false);
    poll(&mut app);
    assert_eq!(chip(&mut app), "s foo · 2");
    if let Some(view) = &mut app.tabs.active_mut().hits {
        view.search.capped = true;
    }
    assert_eq!(chip(&mut app), "s foo · 2 · truncated");
    // The root's own crumbs come before it.
    assert_eq!(
        app.path_bar.1[app.path_bar.1.len() - 2].path,
        root,
        "the root is the last folder crumb"
    );

    // `s` in the hits: the panel, seeded.
    app.run(Command::SearchName, 10, now);
    let search = app.search.as_ref().expect("the panel came back");
    assert_eq!(search.query(), "foo");
    assert_eq!(search.mode, search::Mode::Names);
    assert_eq!(search.root, root);
    assert_eq!(search.hits.len(), 2, "the hits so far, until it runs again");
    app.close_overlay(now);
    assert_eq!(
        app.tab().virtual_kind(),
        Some(Virtual::Hits),
        "Esc kept them"
    );

    // …and `S`, the other tool with the same query.
    app.run(Command::SearchContent, 10, now);
    let search = app.search.as_ref().expect("the panel came back");
    assert_eq!(
        (search.mode, search.query()),
        (search::Mode::Content, "foo")
    );
    app.close_overlay(now);

    // Leaving takes the chip with it.
    app.run(Command::Leave, 10, now);
    app.sync_path_bar();
    assert!(!app.path_bar.1.iter().any(crate::hits::is_chip));
}

/// Contents mode: a file is one row however many lines matched, and the
/// column is its first matching line and the count. The preview opens the
/// file at that line.
#[test]
fn a_content_hit_shows_its_line_and_opens_there() {
    let mut app = tree("hits-content");
    let root = app.files.clone();
    let feed = panel(&mut app, search::Mode::Content, "foo");
    feed.hits(vec![
        matched(&app, "src/foo.txt", 2, "foo here"),
        matched(&app, "docs/foo.txt", 2, "  foo here"),
        matched(&app, "docs/foo.txt", 9, "foo again"),
    ]);
    feed.done(false);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, Instant::now());
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["docs/foo.txt", "src/foo.txt"]);
    let view = app.tab().hits.as_ref().expect("hits");
    assert_eq!(
        view.notes.get("docs/foo.txt").map(String::as_str),
        Some("2 · foo here")
    );
    assert_eq!(
        view.notes.get("src/foo.txt").map(String::as_str),
        Some("foo here")
    );

    // The frame seeds the preview's place for the hovered hit's file.
    app.dir().cursor_to_name("docs/foo.txt");
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
    assert_eq!(
        app.preview.wanted(),
        Some(root.join("docs/foo.txt").as_path())
    );
    assert_eq!(app.preview.scroll(), 1, "opened at line 2");
}

/// `src/deep` and `src/deep/foo.txt` as hits, finished.
fn commit_folder_and_child(app: &mut App) {
    let feed = panel(app, search::Mode::Names, "deep");
    feed.hits(vec![named(app, "src/deep"), named(app, "src/deep/foo.txt")]);
    feed.done(false);
    poll(app);
    app.overlay_key(Chord::plain(Key::Enter), 10, Instant::now());
}

/// **Review 1.** Renaming a folder's row takes every hit inside it along —
/// they used to be re-read at the old path and dropped.
#[test]
fn renaming_a_folder_hit_carries_the_hits_inside_it() {
    let mut app = tree("hits-rename-folder");
    let root = app.files.clone();
    commit_folder_and_child(&mut app);
    let now = Instant::now();
    app.dir().cursor_to_name("src/deep");
    app.run(Command::Rename, 10, now);
    app.submit_prompt("deeper".to_string(), now);

    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["src/deeper", "src/deeper/foo.txt"]);
    let child = app.tab().cwd.dir.row(
        app.tab()
            .cwd
            .dir
            .position_of("src/deeper/foo.txt")
            .expect("the child's row"),
    );
    assert_eq!(
        child.map(|e| e.path.clone()),
        Some(root.join("src/deeper/foo.txt"))
    );
    assert_eq!(cursor(&app).as_deref(), Some("src/deeper"));
}

/// **Review 1, the card.** A bulk rename of a folder and a file inside it
/// renames the file first — or the folder's rename would leave it at a path
/// that is gone — and both rows follow.
#[test]
fn the_bulk_card_renames_a_file_before_its_folder() {
    let mut app = tree("hits-bulk-nested");
    let root = app.files.clone();
    commit_folder_and_child(&mut app);
    let now = Instant::now();
    app.run(Command::SelectAll, 10, now);
    app.run(Command::Rename, 10, now);
    let Some(Dialog::Bulk(bulk)) = &mut app.dialog else {
        panic!("no card");
    };
    for chord in [
        Chord::plain(Key::Home),
        Chord::plain(Key::Char('x')),
        Chord::plain(Key::Char('-')),
    ] {
        bulk.key(chord);
    }
    assert!(bulk.valid(), "{:?}", bulk.problems());
    app.submit_bulk(now);

    assert!(
        root.join("src/x-deep/x-foo.txt").exists(),
        "the file was left behind"
    );
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["src/x-deep", "src/x-deep/x-foo.txt"]);
}

/// **Review 2.** A watcher event does not re-read anything on the spot: the
/// folder is marked, and its rows — only its — are read again once the tree
/// has been quiet for `RESTALE_QUIET`. An operation's folder is read at
/// once, and only that folder.
#[test]
fn the_watcher_is_coalesced_and_only_the_touched_folder_is_read() {
    let mut app = tree("hits-coalesce");
    let root = app.files.clone();
    commit_three(&mut app);
    std::fs::remove_file(root.join("src/foo.txt")).expect("remove");
    std::fs::remove_file(root.join("docs/foo.txt")).expect("remove");

    // An operation in `docs`: that folder's row goes now, `src`'s stays.
    let now = Instant::now();
    app.rescan(&root.join("docs"), now);
    let mut names = rows(&app);
    names.sort();
    assert_eq!(names, ["src/deep/foo.txt", "src/foo.txt"]);

    // The watcher on `src`: nothing yet, and a deadline rather than a poll.
    app.rescan_from(&root.join("src"), RescanBy::Watcher, now);
    assert!(rows(&app).contains(&"src/foo.txt".to_string()));
    let due = app.tab().hits_due_at().expect("a re-read is owed");
    assert_eq!(due, now + crate::folders::RESTALE_QUIET);
    // A second event pushes it out rather than reading.
    let later = now + crate::folders::RESTALE_QUIET / 2;
    app.rescan_from(&root.join("src"), RescanBy::Watcher, later);
    assert!(
        !app.tabs.active_mut().poll_hits(due).changed,
        "not quiet yet"
    );
    assert!(rows(&app).contains(&"src/foo.txt".to_string()));

    let quiet = later + crate::folders::RESTALE_QUIET;
    assert!(app.tabs.active_mut().poll_hits(quiet).changed);
    assert_eq!(rows(&app), ["src/deep/foo.txt"]);
    assert_eq!(app.tab().hits_due_at(), None, "nothing more is owed");
}

/// **Review 3.** `d` then `u`: the row comes back, under its name and without
/// its mark. The trash is the sandbox's own, so nothing reaches the real one:
/// the file is trashed there and journalled as `d`'s job journals it, and the
/// folder is re-read as the job's end re-reads it.
#[test]
fn undoing_a_trash_brings_the_row_back_unmarked() {
    let mut app = tree("hits-untrash");
    let root = app.files.clone();
    commit_three(&mut app);
    let now = Instant::now();
    app.dir().cursor_to_name("docs/foo.txt");
    app.run(Command::ToggleSelect, 10, now);
    assert!(app.tab().cwd.dir.is_selected("docs/foo.txt"));

    let trash = df_core::ops::Trash::at(root.join("..").join("Trash"));
    let item = trash
        .trash(&root.join("docs/foo.txt"), &TaskCtx::detached())
        .expect("trashed");
    app.journal.record(OpRecord::Trash { items: vec![item] });
    app.rescan(&root.join("docs"), now);
    assert!(!rows(&app).contains(&"docs/foo.txt".to_string()));

    app.run(Command::Undo, 10, now);
    assert!(root.join("docs/foo.txt").exists());
    assert!(
        rows(&app).contains(&"docs/foo.txt".to_string()),
        "the row stayed gone"
    );
    assert!(
        !app.tab().cwd.dir.is_selected("docs/foo.txt"),
        "the mark came back"
    );
    assert_eq!(cursor(&app).as_deref(), Some("docs/foo.txt"));
}

/// **Review 4.** `A` and every extract are refused with `a`'s sentence: what
/// they make would land in a folder that is not on screen. From every door —
/// the keys, the menu's extract rows ("all into one folder" included), and
/// the `builtin:extract…` openers `O` offers — and greyed in the menu.
#[test]
fn archive_and_the_extracts_are_refused_in_the_hits() {
    let mut app = tree("hits-archive");
    let root = app.files.clone();
    commit_three(&mut app);
    let now = Instant::now();
    let sentence = app.refusal(Command::Create).map(str::to_string);
    assert!(sentence.is_some());
    app.run(Command::ArchiveCreate, 10, now);
    assert!(app.prompt.is_none(), "`A` opened its prompt");
    assert_eq!(toast_text(&app), sentence.as_deref());
    for command in [
        Command::ArchiveExtractHere,
        Command::ArchiveExtractSubfolder,
    ] {
        app.toasts.clear();
        app.run(command, 10, now);
        assert_eq!(toast_text(&app), sentence.as_deref(), "{}", command.id());
    }
    for action in [
        menu::Action::ExtractHere,
        menu::Action::ExtractSubfolder,
        menu::Action::ExtractMerged,
    ] {
        let command = menu_command(action).expect("an extract row is a verb");
        assert_eq!(app.refusal(command), sentence.as_deref(), "{action:?}");
    }
    for builtin in ["extract", "extract-here", open::MERGED_BUILTIN] {
        app.toasts.clear();
        let choice = open::Choice {
            name: builtin.to_string(),
            command: format!("builtin:{builtin}"),
            description: String::new(),
            block: false,
        };
        app.launch(&choice, vec![root.join("src/foo.txt")], now);
        assert_eq!(toast_text(&app), sentence.as_deref(), "builtin:{builtin}");
    }
    assert!(
        app.ops.is_empty() && app.archive_job.is_none(),
        "something ran"
    );
}

/// **Review 5.** A folder dialog's button with nothing selected picks the
/// folder the cursor's row is in, as a pick of that row does — not the root
/// the hits are named from.
#[test]
fn a_folder_dialog_over_hits_picks_the_cursor_rows_folder() {
    let mut app = tree("hits-pick-folder");
    let root = app.files.clone();
    app.chooser = Some(crate::cli::Chooser {
        directory: true,
        ..crate::cli::Chooser::new(root.join("..").join("out"))
    });
    commit_three(&mut app);
    assert_eq!(cursor(&app).as_deref(), Some("src/deep/foo.txt"));
    app.press_pick(Instant::now());
    assert_eq!(app.chosen, [root.join("src/deep")]);
}

/// **Review 6.** A batch the walk read before a row was renamed, landing
/// after it, does not put the old name back beside the new one.
#[test]
fn a_late_hit_for_a_renamed_file_is_dropped() {
    let mut app = tree("hits-late");
    let now = Instant::now();
    let feed = panel(&mut app, search::Mode::Names, "foo");
    // Read by the "worker" while the file was still called that.
    let late = named(&app, "docs/foo.txt");
    feed.hits(vec![named(&app, "src/foo.txt")]);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    std::fs::rename(
        app.files.join("docs/foo.txt"),
        app.files.join("docs/bar.txt"),
    )
    .expect("rename");

    feed.hits(vec![late]);
    poll(&mut app);
    assert_eq!(
        rows(&app),
        ["src/foo.txt"],
        "a row for a file that is not there"
    );
}

/// **Review 7.** A batch landing in the hits re-sorts them, so a visual run
/// anchored to a position ends rather than stretching over other rows.
#[test]
fn a_batch_landing_ends_a_visual_run() {
    let mut app = tree("hits-visual");
    let now = Instant::now();
    let feed = panel(&mut app, search::Mode::Names, "foo");
    feed.hits(vec![named(&app, "src/foo.txt")]);
    poll(&mut app);
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    app.run(Command::VisualMode, 10, now);
    assert!(app.visual.is_some());

    poll(&mut app);
    assert!(app.visual.is_some(), "nothing landed, and the run ended");
    feed.hits(vec![named(&app, "docs/foo.txt")]);
    poll(&mut app);
    assert!(app.visual.is_none());
}
