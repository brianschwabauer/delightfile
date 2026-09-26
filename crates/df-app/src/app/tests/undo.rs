//! `U`: redo through the keys, the cursor after it, and the app menu's
//! greys.

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

/// Undo and Redo grey in the app menu while their stack is empty — asked of
/// the gate, so a row is never live only to toast "nothing to".
#[test]
fn the_menu_greys_undo_and_redo_with_nothing_to_do() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("redo-menu", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    let rows = edit_rows(&mut app);
    assert!(!enabled(&rows, "Undo"));
    assert!(!enabled(&rows, "Redo"));

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
