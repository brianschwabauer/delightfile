//! `U`, and the undo history card: redo through the keys, the cursor after
//! it, the card's rows and its walk, and the app menu's greys.

use super::*;

/// One key, pressed and handed to a frame.
fn press(app: &mut App, ctx: &egui::Context, chord: Chord) {
    app.pending_keys.push(Press {
        repeat: false,
        chord: Some(chord),
        text: None,
    });
    run_frame(app, ctx, Vec::new());
}

/// `U`.
fn shift_u() -> Chord {
    Chord::shift(Key::Char('u'))
}

/// The row labels the card lists, top to bottom.
fn labels(app: &App) -> Vec<String> {
    history::rows(&app.journal)
        .into_iter()
        .map(|row| row.label)
        .collect()
}

/// The Edit list of the app menu as it opens now: each row's label and
/// whether it can be pressed.
fn edit_rows(app: &mut App) -> Vec<(String, bool)> {
    app.open_app_menu();
    let menu = app.menu.as_ref().expect("the app menu is up");
    let edit = menu
        .items
        .iter()
        .find(|item| item.label == "Edit")
        .and_then(|item| item.submenu.as_ref())
        .expect("an Edit list");
    edit.iter()
        .map(|item| (item.label.clone(), item.enabled))
        .collect()
}

fn enabled(rows: &[(String, bool)], label: &str) -> bool {
    rows.iter()
        .find(|(name, _)| name == label)
        .unwrap_or_else(|| panic!("no {label} row in {rows:?}"))
        .1
}

/// `U` with nothing undone says so, through the key the default keymap puts
/// it on.
#[test]
fn u_with_nothing_undone_says_so() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("redo-empty", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    press(&mut app, &ctx, shift_u());
    assert_eq!(toast_text(&app), Some("Nothing to redo"));
    assert!(app.files.join("a.txt").is_file());
}

/// A rename, `u`, `U`: the file has the new name again, the cursor is on it
/// — not on the row the old name sorted to — and the toast says what was
/// done again. `u` takes it back once more.
#[test]
fn a_rename_undone_and_redone_lands_the_cursor_on_the_new_name() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("redo-rename", &["a.txt", "m.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    let now = Instant::now();
    assert_eq!(
        app.tab().cwd.dir.cursor_entry().map(|e| e.name.as_str()),
        Some("a.txt")
    );
    app.rename("z.txt", now).expect("the rename");
    settle_here(&mut app);

    press(&mut app, &ctx, Chord::plain(Key::Char('u')));
    settle_here(&mut app);
    assert!(app.files.join("a.txt").is_file() && !app.files.join("z.txt").exists());
    assert_eq!(
        app.tab().cwd.dir.cursor_entry().map(|e| e.name.as_str()),
        Some("a.txt")
    );

    press(&mut app, &ctx, shift_u());
    assert_eq!(toast_text(&app), Some("Renamed a.txt → z.txt again"));
    settle_here(&mut app);
    assert!(app.files.join("z.txt").is_file() && !app.files.join("a.txt").exists());
    assert_eq!(
        app.tab().cwd.dir.cursor_entry().map(|e| e.name.as_str()),
        Some("z.txt"),
        "the cursor followed the name the redo gave back"
    );

    press(&mut app, &ctx, Chord::plain(Key::Char('u')));
    assert!(
        app.files.join("a.txt").is_file(),
        "and `u` takes it back again"
    );
}

/// The card lists the journal newest first, the cursor on the newest thing
/// to take back; `Enter` on the second row undoes two, the card stays up,
/// and the two rows it walked past are over the line, where `U` would redo
/// them. `Enter` there redoes up to that row.
#[test]
fn the_history_walks_the_journal_to_the_row_under_the_cursor() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("history-walk", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    let now = Instant::now();
    for name in ["one.txt", "two.txt", "three.txt"] {
        app.create(name, now).expect("made");
    }
    app.run(Command::UndoHistory, 10, now);
    run_frame(&mut app, &ctx, Vec::new());
    assert!(app.undo_history.is_some(), "the card is up");
    assert_eq!(
        labels(&app),
        [
            "Created file three.txt",
            "Created file two.txt",
            "Created file one.txt"
        ]
    );
    assert_eq!(app.undo_history.as_ref().map(|card| card.cursor), Some(0));

    press(&mut app, &ctx, Chord::plain(Key::ArrowDown));
    press(&mut app, &ctx, Chord::plain(Key::Enter));
    assert!(app.files.join("one.txt").is_file());
    assert!(!app.files.join("two.txt").exists() && !app.files.join("three.txt").exists());
    assert_eq!((app.journal.len(), app.journal.redo_len()), (1, 2));
    assert_eq!(toast_text(&app), Some("Undid 2 operations"));
    assert!(app.undo_history.is_some(), "the card stays up");
    assert_eq!(
        labels(&app),
        [
            "Created file three.txt again",
            "Created file two.txt again",
            "Created file one.txt"
        ],
        "the same rows, the line moved"
    );
    let card = app.undo_history.as_ref().expect("up");
    assert_eq!(card.cursor, 1, "still on the row it was on");

    // The row under the cursor is over the line now, and `Enter` says so.
    let rows = history::rows(&app.journal);
    let geometry = history::geometry(screen(), screen().bottom() - ui::GAP, card, rows);
    assert!(geometry.on_redo);
    press(&mut app, &ctx, Chord::plain(Key::Enter));
    assert!(app.files.join("two.txt").is_file(), "redone to that row");
    assert!(!app.files.join("three.txt").exists(), "and no further");
    assert_eq!((app.journal.len(), app.journal.redo_len()), (2, 1));

    press(&mut app, &ctx, Chord::plain(Key::Escape));
    assert!(app.undo_history.is_none());
}

/// A walk stops at the first step refused: the steps before it are taken,
/// its reason is the toast, and the rest are left where they were.
#[test]
fn a_walk_stops_at_the_first_refusal() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("history-refused", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    let now = Instant::now();
    for name in ["one.txt", "two.txt", "three.txt"] {
        app.create(name, now).expect("made");
    }
    // Somebody wrote into two.txt: taking its create back would lose that.
    std::fs::write(app.files.join("two.txt"), b"work").expect("written");
    app.run(Command::UndoHistory, 10, now);
    run_frame(&mut app, &ctx, Vec::new());
    press(&mut app, &ctx, Chord::plain(Key::End));
    press(&mut app, &ctx, Chord::plain(Key::Enter));
    assert!(
        !app.files.join("three.txt").exists(),
        "the first step was taken"
    );
    assert!(
        app.files.join("two.txt").is_file(),
        "the second was refused"
    );
    assert!(
        app.files.join("one.txt").is_file(),
        "and the third never tried"
    );
    assert!(
        toast_text(&app).is_some_and(|t| t.contains("cannot undo")),
        "{:?}",
        toast_text(&app)
    );
    assert_eq!((app.journal.len(), app.journal.redo_len()), (2, 1));
}

/// A click on a row puts the cursor there and does nothing else; a second
/// click on it is `Enter`.
#[test]
fn a_click_is_the_cursor_and_a_double_click_is_enter() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("history-click", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    let now = Instant::now();
    for name in ["one.txt", "two.txt"] {
        app.create(name, now).expect("made");
    }
    app.run(Command::UndoHistory, 10, now);
    run_frame(&mut app, &ctx, Vec::new());
    let rows = history::rows(&app.journal);
    let card = app.undo_history.as_ref().expect("up");
    let geometry = history::geometry(screen(), screen().bottom() - ui::GAP, card, rows);
    let second = geometry.rects[1].center();

    click_at(&mut app, &ctx, second);
    assert_eq!(app.undo_history.as_ref().map(|card| card.cursor), Some(1));
    assert_eq!(app.journal.len(), 2, "one click undid nothing");
    click_at(&mut app, &ctx, second);
    assert_eq!(app.journal.len(), 0, "the second click was Enter on it");
    assert!(app.undo_history.is_some());
}

/// An empty journal is a card that says so, with nothing for `Enter` to do.
#[test]
fn an_empty_history_says_nothing_to_undo() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("history-empty", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    app.run(Command::UndoHistory, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    assert!(labels(&app).is_empty());
    press(&mut app, &ctx, Chord::plain(Key::Enter));
    assert!(app.undo_history.is_some());
    assert!(app.files.join("a.txt").is_file());
}

/// Undo and Redo grey in the app menu while their stack is empty — asked of
/// the gate, so a row is never live only to toast "nothing to" — and the
/// history is always there to open.
#[test]
fn the_menu_greys_undo_and_redo_with_nothing_to_do() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("history-menu", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    let rows = edit_rows(&mut app);
    assert!(!enabled(&rows, "Undo"));
    assert!(!enabled(&rows, "Redo"));
    assert!(enabled(&rows, "Undo history…"));

    let now = Instant::now();
    app.create("one.txt", now).expect("made");
    let rows = edit_rows(&mut app);
    assert!(enabled(&rows, "Undo"));
    assert!(!enabled(&rows, "Redo"), "nothing undone yet");

    app.run(Command::Undo, 10, now);
    let rows = edit_rows(&mut app);
    assert!(!enabled(&rows, "Undo"));
    assert!(enabled(&rows, "Redo"));
}

/// The history has no key by default, and the palette lists it all the
/// same — once, even when a `keymap.toml` has given it one.
#[test]
fn the_palette_lists_the_history() {
    let mut app = Fixture::new("history-palette", &["a.txt"]);
    let listed = |app: &App| {
        app.palette_rows()
            .iter()
            .filter(|row| row.choice == Choice::Run(Command::UndoHistory))
            .count()
    };
    assert_eq!(listed(&app), 1);
    app.keymap
        .register(
            Context::Files,
            df_core::keymap::parse_sequence("alt+u").expect("parses"),
            Command::UndoHistory,
            "Undo history…",
            df_core::keymap::When::Always,
        )
        .expect("free");
    assert_eq!(listed(&app), 1, "not twice");
}
