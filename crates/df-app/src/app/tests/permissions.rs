//! `C` through the app: the card over the cursor's file, over a selection
//! and over a folder, its grid and its field kept in step, the job on the
//! real engine landed the way `finish_op` lands one, `u`, the menus' rows,
//! the spot panel's door to it, and where it refuses.

use std::os::unix::fs::PermissionsExt;

use df_core::ops::mode::{Cell, Grid};

use super::*;
use crate::permissions::{Focus, PermCard};

fn set(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).expect("stat").mode() & 0o7777
}

/// Read the folder again, so the rows carry the modes just set by hand.
fn reread(app: &mut App) {
    let cwd = app.cwd();
    app.rescan(&cwd, Instant::now());
    settle(app.tabs.active_mut(), &app.scanner);
}

/// Put the cursor on `name` and, with `select`, mark it.
fn at(app: &mut App, name: &str, select: bool) {
    let position = app
        .tab()
        .cwd
        .dir
        .position_of(name)
        .unwrap_or_else(|| panic!("{name} is not in the listing"));
    let dir = &mut app.tabs.active_mut().cwd.dir;
    dir.set_cursor(position);
    if select {
        dir.toggle_selected(position);
    }
}

fn press(app: &mut App, chord: Chord) {
    app.route_chord(chord, 10, Instant::now());
}

fn key(key: Key) -> Chord {
    Chord::plain(key)
}

/// `C`, as the keyboard sends it.
fn press_c(app: &mut App) {
    press(app, Chord::shift(Key::Char('c')));
}

fn typed(app: &mut App, digits: &str) {
    for digit in digits.chars() {
        press(app, key(Key::Char(digit)));
    }
}

fn card(app: &App) -> &PermCard {
    match &app.dialog {
        Some(Dialog::Permissions(card)) => card,
        _ => panic!("the permissions card is not up"),
    }
}

/// Let the queued job finish and land, then drain the scanner until `done`
/// says the listing has caught up — `compress`'s rule, for this job.
fn land(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !app.ops.is_empty() && Instant::now() < deadline {
        let events: Vec<TaskEvent> = app.task_events.try_iter().collect();
        for event in events {
            app.task_event(event, Instant::now());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.ops.is_empty(), "the job never finished");
    while !done(app) && Instant::now() < deadline {
        for update in app.scanner.drain() {
            app.tabs.active_mut().apply(&update);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(done(app), "the listing never caught up");
}

/// The mode the listing's row for `name` carries: what the permissions
/// column draws.
fn row_mode(app: &App, name: &str) -> Option<u32> {
    app.tab()
        .cwd
        .dir
        .entries()
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.mode & 0o7777)
}

fn cursor_name(app: &App) -> Option<String> {
    app.tab().cwd.dir.cursor_entry().map(|e| e.name.clone())
}

/// The card's layout, measured as a frame would measure it.
fn geometry_of(app: &App) -> crate::permissions::Geometry {
    let ctx = egui::Context::default();
    let mut out = None;
    let _ = ctx.run_ui(Default::default(), |ui| {
        out = Some(crate::permissions::geometry(
            ui.painter(),
            screen(),
            card(app),
        ));
    });
    out.expect("measured")
}

#[test]
fn c_opens_the_card_with_the_cursor_files_bits() {
    let mut app = Fixture::new("perm-open", &["a.txt", "b.txt"]);
    set(&app.files.join("b.txt"), 0o640);
    reread(&mut app);
    at(&mut app, "b.txt", false);
    press_c(&mut app);
    let card = card(&app);
    assert_eq!(card.grid, Grid::of_mode(0o640));
    assert_eq!(card.field, "640");
    assert_eq!(card.subtitle(), "b.txt");
    assert!(!card.has_folder(), "a file has nothing inside to apply to");
    assert_eq!(card.focus, Focus::Cell(0));
    assert!(geometry_of(&app).recursive.is_none());
    // The owner line is this user's own, by name where the passwd file has
    // one and by number where it does not.
    let uid = std::fs::metadata(app.files.join("b.txt"))
        .map(|meta| std::os::unix::fs::MetadataExt::uid(&meta))
        .expect("stat");
    let owner = df_core::fs::owner::user_name(uid).map_or_else(|| uid.to_string(), str::to_string);
    assert!(
        card.owner_line().starts_with(&format!("{owner} · ")),
        "{}",
        card.owner_line()
    );

    // `Esc` puts it away, having done nothing.
    press(&mut app, key(Key::Escape));
    assert!(app.dialog.is_none());
    assert_eq!(mode_of(&app.files.join("b.txt")), 0o640);
}

#[test]
fn a_cell_moves_the_field_and_the_field_moves_the_cells() {
    let mut app = Fixture::new("perm-grid", &["a.txt"]);
    set(&app.files.join("a.txt"), 0o640);
    reread(&mut app);
    press_c(&mut app);

    // Space on the owner's read, where the card opens.
    press(&mut app, key(Key::Space));
    assert_eq!(card(&app).field, "240");
    // Right twice and down once is the group's execute.
    for arrow in [Key::ArrowRight, Key::ArrowRight, Key::ArrowDown] {
        press(&mut app, key(arrow));
    }
    assert_eq!(card(&app).focus, Focus::Cell(5));
    press(&mut app, key(Key::Space));
    assert_eq!(card(&app).field, "250");
    assert_eq!(card(&app).grid, Grid::of_mode(0o250));

    // A digit goes to the field from wherever the keyboard is, replacing
    // what it showed; the third sets the grid.
    typed(&mut app, "7");
    assert_eq!(card(&app).focus, Focus::Field);
    assert_eq!(card(&app).field, "7");
    assert!(card(&app).partial());
    assert_eq!(
        card(&app).grid,
        Grid::of_mode(0o250),
        "one digit is not a mode"
    );
    typed(&mut app, "55");
    assert_eq!(card(&app).grid, Grid::of_mode(0o755));
    assert!(!card(&app).partial());

    // Half a number holds `Enter`: the card stays and the file is untouched.
    press(&mut app, key(Key::Backspace));
    assert_eq!(card(&app).field, "75");
    assert!(card(&app).status().is_some_and(|(_, warning)| warning));
    press(&mut app, key(Key::Enter));
    assert!(app.dialog.is_some(), "Enter went ahead on half a number");
    assert_eq!(mode_of(&app.files.join("a.txt")), 0o640);

    // A box pressed again puts the field back in step with the grid.
    press(&mut app, key(Key::ArrowUp));
    assert!(matches!(card(&app).focus, Focus::Cell(6..=8)));
    press(&mut app, key(Key::Space));
    assert!(!card(&app).partial());
    assert_eq!(card(&app).field, card(&app).grid.octal());
}

#[test]
fn enter_applies_and_u_restores() {
    let mut app = Fixture::new("perm-apply", &["a.txt", "b.txt", "c.txt"]);
    let b = app.files.join("b.txt");
    set(&b, 0o640);
    reread(&mut app);
    at(&mut app, "b.txt", false);
    press_c(&mut app);
    typed(&mut app, "600");
    press(&mut app, key(Key::Enter));
    assert!(app.dialog.is_none(), "the card stayed up");
    land(&mut app, |app| row_mode(app, "b.txt") == Some(0o600));
    assert_eq!(mode_of(&b), 0o600);
    assert_eq!(toast_text(&app), Some("Permissions set on 1 item"));
    assert_eq!(
        app.toasts.current().map(|toast| toast.kind),
        Some(crate::toast::ToastKind::Undo)
    );
    assert_eq!(
        cursor_name(&app).as_deref(),
        Some("b.txt"),
        "the cursor moved"
    );

    app.run(Command::Undo, 10, Instant::now());
    assert_eq!(mode_of(&b), 0o640);
    assert_eq!(toast_text(&app), Some("Restored permissions of 1 item"));
    land(&mut app, |app| row_mode(app, "b.txt") == Some(0o640));
    assert_eq!(cursor_name(&app).as_deref(), Some("b.txt"));
}

/// `u`, `U`, `u` after `C`: the mode is put back, set again with the toast
/// an operation lands with, and put back again, the row's mode following
/// each time; the undo history names the change on either side of its line.
#[test]
fn a_change_undone_is_redone_and_the_history_names_it() {
    let mut app = Fixture::new("perm-redo", &["a.txt", "b.txt"]);
    let b = app.files.join("b.txt");
    set(&b, 0o640);
    reread(&mut app);
    at(&mut app, "b.txt", false);
    press_c(&mut app);
    typed(&mut app, "600");
    press(&mut app, key(Key::Enter));
    land(&mut app, |app| row_mode(app, "b.txt") == Some(0o600));
    let history = |app: &App| -> Vec<(String, bool)> {
        crate::history::rows(&app.journal)
            .into_iter()
            .map(|row| (row.label, row.redo))
            .collect()
    };
    assert_eq!(
        history(&app),
        [("Changed permissions of 1 item".to_string(), false)]
    );

    app.run(Command::Undo, 10, Instant::now());
    assert_eq!(mode_of(&b), 0o640);
    land(&mut app, |app| row_mode(app, "b.txt") == Some(0o640));
    assert_eq!(
        history(&app),
        [("Changed permissions of 1 item again".to_string(), true)]
    );

    app.run(Command::Redo, 10, Instant::now());
    assert_eq!(mode_of(&b), 0o600);
    assert_eq!(
        toast_text(&app),
        Some("Changed permissions of 1 item again")
    );
    assert_eq!(
        app.toasts.current().map(|toast| toast.kind),
        Some(crate::toast::ToastKind::Undo)
    );
    land(&mut app, |app| row_mode(app, "b.txt") == Some(0o600));
    assert_eq!(cursor_name(&app).as_deref(), Some("b.txt"));

    app.run(Command::Undo, 10, Instant::now());
    assert_eq!(mode_of(&b), 0o640);
    assert_eq!(toast_text(&app), Some("Restored permissions of 1 item"));
    land(&mut app, |app| row_mode(app, "b.txt") == Some(0o640));
}

#[test]
fn two_files_that_differ_show_a_dash_and_keep_it() {
    let mut app = Fixture::new("perm-mixed", &["a.txt", "b.txt"]);
    let (a, b) = (app.files.join("a.txt"), app.files.join("b.txt"));
    set(&a, 0o644);
    set(&b, 0o600);
    reread(&mut app);
    at(&mut app, "a.txt", true);
    at(&mut app, "b.txt", true);
    press_c(&mut app);
    assert_eq!(card(&app).subtitle(), "2 items");
    assert_eq!(card(&app).grid.cells[3], Cell::Mixed, "group read");
    assert_eq!(card(&app).grid.cells[6], Cell::Mixed, "everybody's read");
    assert_eq!(
        card(&app).grid.cells[0],
        Cell::On,
        "they agree on the owner"
    );
    assert_eq!(card(&app).field, "6––");

    // Nothing touched is nothing to do.
    press(&mut app, key(Key::Enter));
    assert!(app.dialog.is_none());
    assert_eq!(toast_text(&app), Some("Nothing to change"));

    // The group's read turned on for both; everybody's left as each has it.
    press_c(&mut app);
    press(&mut app, key(Key::ArrowDown));
    press(&mut app, key(Key::Space));
    assert_eq!(card(&app).field, "64–");
    press(&mut app, key(Key::Enter));
    land(&mut app, |app| row_mode(app, "b.txt") == Some(0o640));
    assert_eq!(mode_of(&a), 0o644);
    assert_eq!(mode_of(&b), 0o640);
}

#[test]
fn the_checkbox_is_there_only_for_a_folder_and_goes_inside() {
    let mut app = Fixture::with_folders("perm-folder", &["a.txt"], &["sub"]);
    let sub = app.files.join("sub");
    let inner = sub.join("inner.txt");
    std::fs::write(&inner, b"i").expect("write");
    set(&inner, 0o600);
    set(&sub, 0o700);
    reread(&mut app);

    at(&mut app, "a.txt", false);
    press_c(&mut app);
    assert!(!card(&app).has_folder());
    assert!(geometry_of(&app).recursive.is_none());
    for _ in 0..4 {
        press(&mut app, key(Key::ArrowDown));
    }
    assert_eq!(card(&app).focus, Focus::Field, "no checkbox to go down to");
    press(&mut app, key(Key::Escape));

    at(&mut app, "sub", false);
    press_c(&mut app);
    assert!(card(&app).has_folder());
    let geometry = geometry_of(&app);
    assert!(geometry.recursive.is_some() && geometry.note.is_some());
    for _ in 0..4 {
        press(&mut app, key(Key::ArrowDown));
    }
    assert_eq!(card(&app).focus, Focus::Recursive);
    press(&mut app, key(Key::Space));
    assert!(card(&app).recursive);
    // The note's line was held before the box was ticked: nothing moved.
    assert_eq!(geometry_of(&app), geometry);

    typed(&mut app, "644");
    press(&mut app, key(Key::Enter));
    land(&mut app, |app| row_mode(app, "sub") == Some(0o755));
    assert_eq!(
        mode_of(&sub),
        0o755,
        "a folder gets execute where it gets read"
    );
    assert_eq!(mode_of(&inner), 0o644);

    app.run(Command::Undo, 10, Instant::now());
    assert_eq!(toast_text(&app), Some("Restored permissions of 2 items"));
    assert_eq!(mode_of(&sub), 0o700);
    assert_eq!(mode_of(&inner), 0o600);
}

#[test]
fn a_press_on_a_box_toggles_it_and_the_buttons_answer() {
    let mut app = Fixture::new("perm-click", &["a.txt"]);
    let a = app.files.join("a.txt");
    set(&a, 0o644);
    reread(&mut app);
    press_c(&mut app);
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
    let geometry = geometry_of(&app);

    // The group's write, by pointer.
    click_at(&mut app, &ctx, geometry.cells[4].center());
    assert_eq!(card(&app).field, "664");
    assert_eq!(card(&app).focus, Focus::Cell(4));
    click_at(&mut app, &ctx, geometry.field.center());
    assert_eq!(card(&app).focus, Focus::Field);

    // Apply is the second button.
    click_at(&mut app, &ctx, geometry.actions[1].center());
    assert!(app.dialog.is_none());
    land(&mut app, |app| row_mode(app, "a.txt") == Some(0o664));

    // …and Cancel the first.
    press_c(&mut app);
    run_frame(&mut app, &ctx, Vec::new());
    let cancel = geometry_of(&app).actions[0].center();
    click_at(&mut app, &ctx, cancel);
    assert!(app.dialog.is_none());
    assert_eq!(mode_of(&a), 0o664);
}

#[test]
fn the_menu_rows_open_it_and_grey_in_the_trash() {
    let mut app = Fixture::new("perm-menus", &["a.txt", "b.txt"]);
    let now = Instant::now();
    let edit_row = |app: &App| {
        app.menu
            .as_ref()
            .expect("a menu is up")
            .items
            .iter()
            .flat_map(|item| item.submenu.iter().flatten())
            .find(|item| item.label == "Permissions…")
            .cloned()
            .expect("a Permissions… row in Edit")
    };

    // The app menu's row, after Rename, with the key `C` is.
    app.run(Command::AppMenu, 10, now);
    let row = edit_row(&app);
    assert!(row.enabled);
    assert_eq!(row.keys, "C");
    let edit = app
        .menu
        .as_ref()
        .and_then(|menu| menu.items.iter().find(|item| item.label == "Edit"))
        .and_then(|item| item.submenu.clone())
        .expect("an Edit list");
    let place = |label: &str| edit.iter().position(|item| item.label == label);
    assert_eq!(place("Permissions…"), place("Rename").map(|i| i + 1));
    app.close_menu(now);
    app.menu_action(menu::Action::Run(Command::Permissions), 10, now);
    assert!(matches!(app.dialog, Some(Dialog::Permissions(_))));
    app.close_overlay(now);

    // The row menu's: the row it opened on, not the selection it is not in.
    at(&mut app, "a.txt", true);
    at(&mut app, "b.txt", false);
    app.menu_action(menu::Action::Permissions, 10, now);
    assert_eq!(card(&app).subtitle(), "b.txt");
    app.close_overlay(now);
    // …and the selection when the row is part of it.
    at(&mut app, "b.txt", true);
    app.menu_action(menu::Action::Permissions, 10, now);
    assert_eq!(card(&app).subtitle(), "2 items");
    app.close_overlay(now);

    // In the trash the row is grey, and the key says why.
    let origin = app.files.clone();
    app.tabs.active_mut().trash = Some(crate::trashview::View {
        items: Vec::new(),
        origin,
    });
    assert!(app.refusal(Command::Permissions).is_some());
    app.run(Command::AppMenu, 10, now);
    assert!(!edit_row(&app).enabled, "live in the trash");
    app.close_menu(now);
    app.run(Command::Permissions, 10, now);
    assert!(app.dialog.is_none());
    assert_eq!(
        toast_text(&app),
        Some("Not in the trash — Enter restores, D destroys")
    );
}

#[test]
fn the_row_menu_offers_it_after_rename() {
    let mut app = Fixture::new("perm-row-menu", &["a.txt"]);
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
    let row = row_rect(&app, 0).center();
    right_click_at(&mut app, &ctx, row);
    let permissions = |app: &App| {
        app.menu
            .as_ref()
            .expect("up")
            .items
            .iter()
            .find(|item| item.action == menu::Action::Permissions)
            .map(|item| (item.keys.clone(), item.enabled))
    };
    assert_eq!(permissions(&app), Some(("C".to_string(), true)));
    let items = &app.menu.as_ref().expect("up").items;
    let rename = items
        .iter()
        .position(|item| item.action == menu::Action::Rename);
    let row_at = items
        .iter()
        .position(|item| item.action == menu::Action::Permissions);
    assert_eq!(row_at, rename.map(|i| i + 1), "straight after Rename");
}

#[test]
fn enter_on_the_spots_permission_row_opens_the_card_over_it() {
    let mut app = Fixture::new("perm-spot", &["a.txt"]);
    set(&app.files.join("a.txt"), 0o644);
    reread(&mut app);
    app.run(Command::Spot, 10, Instant::now());
    let row = app
        .spot
        .as_ref()
        .and_then(Spot::perm_row)
        .expect("a permissions row");
    if let Some(spot) = &mut app.spot {
        spot.select(row);
    }
    press(&mut app, key(Key::Enter));
    assert_eq!(card(&app).grid, Grid::of_mode(0o644));
    assert!(
        app.spot.is_none(),
        "the panel is under the card, not beside it"
    );
    // `Esc` puts the card away and the panel back, on the same row.
    press(&mut app, key(Key::Escape));
    assert!(app.dialog.is_none());
    assert_eq!(app.spot.as_ref().map(|spot| spot.cursor), Some(row));

    // `C` over the panel is the same door, and an Apply lands in the
    // panel's own readout.
    press_c(&mut app);
    assert!(app.spot.is_none());
    typed(&mut app, "600");
    press(&mut app, key(Key::Enter));
    land(&mut app, |app| row_mode(app, "a.txt") == Some(0o600));
    assert_eq!(
        app.spot.as_ref().map(|spot| spot.facts.mode & 0o777),
        Some(0o600)
    );
    // `Space` on the row still flips the chosen bit where it stands.
    press(&mut app, key(Key::Space));
    assert_eq!(mode_of(&app.files.join("a.txt")), 0o200);
}

/// `C` over the spot is on the help sheet (it is a `[spot]` row) and not on
/// the card's strip: the five hints there fill about 528 of its 540 points,
/// and a sixth would push `Tab / Esc` off the end. `Enter` on the
/// Permissions row is the panel's own door to the card. This is the guard.
#[test]
fn the_spots_strip_keeps_every_hint_it_has() {
    let mut app = Fixture::new("perm-spot-hints", &["a.txt"]);
    app.run(Command::Spot, 10, Instant::now());
    let geometry = overlay_of(&app).expect("the spot card");
    let hints = overlay_hints(&geometry, &app.dialog);
    let strip = strip_of(geometry.card(), &hints);
    assert_eq!(strip.rects.len(), hints.len(), "a hint fell off the strip");
    assert!(app
        .keymap
        .active_bindings(&ContextStack::with(&[Context::Spot]), WhenFlags::NONE)
        .iter()
        .any(|binding| binding.command == Command::Permissions));
}

/// The spot's chips are refused where `C` is: in the trash view a press on
/// one says why, changes nothing and journals nothing.
#[test]
fn a_spot_chip_in_the_trash_view_is_refused_and_journals_nothing() {
    let mut app = Fixture::new("perm-spot-trash", &["a.txt"]);
    let a = app.files.join("a.txt");
    set(&a, 0o644);
    reread(&mut app);
    app.run(Command::Spot, 10, Instant::now());
    let row = app
        .spot
        .as_ref()
        .and_then(Spot::perm_row)
        .expect("a permissions row");
    if let Some(spot) = &mut app.spot {
        spot.select(row);
    }
    let origin = app.files.clone();
    app.tabs.active_mut().trash = Some(crate::trashview::View {
        items: Vec::new(),
        origin,
    });
    let journal = app.journal.len();
    press(&mut app, key(Key::Space));
    assert_eq!(
        toast_text(&app),
        Some("Not in the trash — Enter restores, D destroys")
    );
    assert_eq!(
        app.journal.len(),
        journal,
        "the refused press was journalled"
    );
    assert_eq!(mode_of(&a), 0o644);
}

#[test]
fn a_link_has_no_permissions_of_its_own_to_set() {
    let mut app = Fixture::new("perm-link", &["a.txt"]);
    std::os::unix::fs::symlink(app.files.join("a.txt"), app.files.join("link")).expect("symlink");
    reread(&mut app);
    at(&mut app, "link", false);
    press_c(&mut app);
    assert!(app.dialog.is_none(), "a card over a link");
    assert_eq!(
        toast_text(&app),
        Some("Links have no permissions of their own — change the file they point to")
    );
}
