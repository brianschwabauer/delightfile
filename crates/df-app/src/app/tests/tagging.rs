//! `T` through the app: the prompt it opens and what it is filled with, what
//! `Enter` writes to one file and to a selection, `Tab`, `u`, the row's dots,
//! the `m t` column, and the places the prompt is refused.

use df_core::fs::tags as file_tags;

use super::*;

/// The tags `names` as a list.
fn list(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// A fixture whose files can hold tags, or `None` — said on stderr — on a
/// machine whose `$TMPDIR` cannot.
fn fixture(name: &str, names: &[&str]) -> Option<Fixture> {
    let app = Fixture::new(name, names);
    let probe = app.files.join(names[0]);
    match file_tags::write(&probe, &list(&["probe"])) {
        Ok(()) => {
            file_tags::write(&probe, &[]).expect("untag the probe");
            Some(app)
        }
        Err(e) => {
            eprintln!("skipping: {} holds no tags ({e})", app.files.display());
            None
        }
    }
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

/// `T`, then what the prompt opened with: its title and its line.
fn press_t(app: &mut App) -> (String, String) {
    app.run(Command::Tag, 10, Instant::now());
    let prompt = app.prompt.as_ref().expect("T opened no prompt");
    assert_eq!(prompt.kind, PromptKind::Tags);
    (prompt.title().to_string(), prompt.query().to_string())
}

/// Replace the line and press `Enter`, through the keys.
fn enter(app: &mut App, text: &str) {
    let now = Instant::now();
    if let Some(prompt) = &mut app.prompt {
        prompt.buffer = InputBuffer::new(text, text.chars().count());
    }
    app.prompt_key(Chord::plain(Key::Enter), now);
    assert!(app.prompt.is_none(), "Enter closes the prompt");
}

fn entry_tags(app: &App, name: &str) -> Vec<String> {
    let dir = &app.tab().cwd.dir;
    let at = dir.position_of(name).expect("a row by that name");
    dir.row(at).expect("the row").tags.clone()
}

fn toast(app: &App) -> String {
    app.toasts
        .current()
        .map(|toast| toast.message.clone())
        .unwrap_or_default()
}

/// `T` on one file opens `Tags of name:` on the tags it has, the caret after
/// a separator; `Enter` writes exactly the line, the row carries the tags at
/// once and wears a dot for the coloured one, and the toast names them.
#[test]
fn t_opens_on_the_file_s_tags_and_enter_writes_the_line() {
    let Some(mut app) = fixture("tags-one", &["a.txt", "b.txt"]) else {
        return;
    };
    let a = app.files.join("a.txt");
    file_tags::write(&a, &list(&["work"])).expect("tag the file");
    at(&mut app, "a.txt", false);

    let (title, line) = press_t(&mut app);
    assert_eq!(title, "Tags of a.txt:");
    assert_eq!(line, "work, ");
    let prompt = app.prompt.as_ref().expect("open");
    assert_eq!(
        prompt.buffer.cursor(),
        line.chars().count(),
        "caret at the end"
    );

    enter(&mut app, "work, red ,  invoice 2026,");
    assert_eq!(file_tags::read(&a), ["work", "red", "invoice 2026"]);
    assert_eq!(entry_tags(&app, "a.txt"), ["work", "red", "invoice 2026"]);
    let dir = &app.tab().cwd.dir;
    let row = dir
        .row(dir.position_of("a.txt").expect("row"))
        .expect("row");
    assert_eq!(
        app.tag_colors.dots(&row.tags, &app.palette),
        vec![app.palette.red],
        "one dot, red: work and invoice 2026 have no colour"
    );
    assert_eq!(toast(&app), "Tagged a.txt work, red, invoice 2026");
    assert_eq!(
        app.state.known_tags(),
        ["work", "red", "invoice 2026"],
        "the prompt remembers what it applied"
    );

    // The row paints with its dots without trouble.
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());

    // Taking them all off is an untag, and says which went.
    press_t(&mut app);
    enter(&mut app, "");
    assert!(file_tags::read(&a).is_empty());
    assert_eq!(toast(&app), "Untagged a.txt work, red, invoice 2026");
}

/// A selection opens on the tags every item shares, and `Enter` applies the
/// difference: a shared tag taken out goes from each, a new one goes on
/// each, and the tags only one item had stay where they were.
#[test]
fn a_selection_is_tagged_by_the_difference() {
    let Some(mut app) = fixture("tags-many", &["a.txt", "b.txt", "c.txt"]) else {
        return;
    };
    let (a, b) = (app.files.join("a.txt"), app.files.join("b.txt"));
    file_tags::write(&a, &list(&["red", "work", "draft"])).expect("tag the file");
    file_tags::write(&b, &list(&["Work", "blue", "red"])).expect("tag the file");
    at(&mut app, "a.txt", true);
    at(&mut app, "b.txt", true);

    let (title, line) = press_t(&mut app);
    assert_eq!(title, "Tags of 2 items:");
    assert_eq!(
        line, "red, work, ",
        "what both share, in the first one's order"
    );

    enter(&mut app, "red, urgent");
    assert_eq!(file_tags::read(&a), ["red", "draft", "urgent"]);
    assert_eq!(file_tags::read(&b), ["blue", "red", "urgent"]);
    assert_eq!(entry_tags(&app, "b.txt"), ["blue", "red", "urgent"]);
    assert!(file_tags::read(&app.files.join("c.txt")).is_empty());
    assert_eq!(toast(&app), "Tagged 2 items");

    // `u` puts both back exactly, order and spelling included.
    app.run(Command::Undo, 10, Instant::now());
    assert_eq!(file_tags::read(&a), ["red", "work", "draft"]);
    assert_eq!(file_tags::read(&b), ["Work", "blue", "red"]);
    assert_eq!(toast(&app), "Restored tags of 2 items");

    // Taking a shared tag off both is an untag of both.
    press_t(&mut app);
    enter(&mut app, "red");
    assert_eq!(file_tags::read(&a), ["red", "draft"]);
    assert_eq!(file_tags::read(&b), ["blue", "red"]);
    assert_eq!(toast(&app), "Untagged 2 items");
}

/// `u` after `T` on one file restores what it had, and refuses — with the
/// entry kept — once somebody else has changed its tags since.
#[test]
fn u_restores_the_tags_and_refuses_a_changed_set() {
    let Some(mut app) = fixture("tags-undo", &["a.txt"]) else {
        return;
    };
    let a = app.files.join("a.txt");
    file_tags::write(&a, &list(&["blue"])).expect("tag the file");
    at(&mut app, "a.txt", false);
    press_t(&mut app);
    enter(&mut app, "green");
    assert_eq!(file_tags::read(&a), ["green"]);

    file_tags::write(&a, &list(&["green", "yellow"])).expect("tag the file");
    app.run(Command::Undo, 10, Instant::now());
    assert!(toast(&app).contains("changed since"), "{}", toast(&app));
    assert_eq!(file_tags::read(&a), ["green", "yellow"]);

    file_tags::write(&a, &list(&["green"])).expect("tag the file");
    app.run(Command::Undo, 10, Instant::now());
    assert_eq!(toast(&app), "Restored tags of a.txt");
    assert_eq!(file_tags::read(&a), ["blue"]);
}

/// `Tab` finishes the word under the caret from the colours, the tags the
/// prompt has applied before and the tags in this listing, and pressed again
/// offers the next one for the same letters. A tag already in the line is
/// not offered twice.
#[test]
fn tab_completes_from_the_known_tags() {
    let Some(mut app) = fixture("tags-tab", &["a.txt", "b.txt"]) else {
        return;
    };
    file_tags::write(&app.files.join("b.txt"), &list(&["report"])).expect("tag the file");
    app.refresh_all(Instant::now());
    settle_here(&mut app);
    app.state.remember_tags(&list(&["invoice 2026"]));
    at(&mut app, "a.txt", false);
    press_t(&mut app);

    let tab = |app: &mut App| app.prompt_key(Chord::plain(Key::Tab), Instant::now());
    let typed = |app: &mut App, text: &str| {
        for c in text.chars() {
            app.prompt_key(Chord::from_char(c).expect("printable"), Instant::now());
        }
    };
    let line = |app: &App| app.prompt.as_ref().expect("open").query().to_string();

    typed(&mut app, "inv");
    tab(&mut app);
    assert_eq!(line(&app), "invoice 2026");

    typed(&mut app, ", re");
    tab(&mut app);
    assert_eq!(
        line(&app),
        "invoice 2026, red",
        "the colour first, alphabetically"
    );
    tab(&mut app);
    assert_eq!(line(&app), "invoice 2026, report", "then the listing's");
    tab(&mut app);
    assert_eq!(line(&app), "invoice 2026, red", "and round again");

    typed(&mut app, ", i");
    tab(&mut app);
    assert_eq!(
        line(&app),
        "invoice 2026, red, i",
        "invoice 2026 is in the line already"
    );

    // `Esc` writes nothing.
    app.prompt_key(Chord::plain(Key::Escape), Instant::now());
    assert!(app.prompt.is_none());
    assert!(file_tags::read(&app.files.join("a.txt")).is_empty());
}

/// `m t` puts every tag in the column by name, the colourless ones as well.
#[test]
fn m_t_shows_the_tags_by_name() {
    let Some(mut app) = fixture("tags-linemode", &["a.txt", "b.txt"]) else {
        return;
    };
    file_tags::write(&app.files.join("a.txt"), &list(&["red", "work"])).expect("tag the file");
    app.refresh_all(Instant::now());
    settle_here(&mut app);
    app.run(Command::LinemodeTags, 10, Instant::now());
    assert_eq!(app.mgr.linemode, LineMode::Tags);
    let dir = &app.tab().cwd.dir;
    let row = |name: &str| dir.row(dir.position_of(name).expect("row")).expect("row");
    assert_eq!(
        crate::format::linemode_text(row("a.txt"), LineMode::Tags),
        "red, work"
    );
    assert_eq!(
        crate::format::linemode_text(row("b.txt"), LineMode::Tags),
        ""
    );
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
}

/// Where the rows are not files on this disk the prompt is refused out
/// loud, and the menus grey its row rather than offer it.
#[test]
fn tags_are_refused_where_the_rows_are_not_local_files() {
    let mut app = Fixture::new("tags-trash", &["a.txt"]);
    let origin = app.files.clone();
    app.tabs.active_mut().trash = Some(crate::trashview::View {
        items: Vec::new(),
        origin,
    });
    assert_eq!(app.refusal(Command::Tag), Some("Tags live on local files"));
    app.run(Command::Tag, 10, Instant::now());
    assert!(app.prompt.is_none(), "no prompt in the trash");
    assert_eq!(toast(&app), "Tags live on local files");

    app.open_app_menu();
    let menu = app.menu.as_ref().expect("the app menu is up");
    let edit = menu
        .items
        .iter()
        .find(|item| item.label == "Edit")
        .and_then(|item| item.submenu.as_ref())
        .expect("an Edit list");
    let tags = edit
        .iter()
        .find(|item| item.label == "Tags…")
        .expect("a Tags… row after Rename");
    assert!(!tags.enabled, "greyed in the trash view");
    assert_eq!(tags.keys, "T");
}

/// Out of the trash the same rows are live, the row menu's included.
#[test]
fn the_menus_offer_tags_on_local_files() {
    let mut app = Fixture::new("tags-menu", &["a.txt"]);
    app.open_app_menu();
    let menu = app.menu.as_ref().expect("the app menu is up");
    let edit = menu
        .items
        .iter()
        .find(|item| item.label == "Edit")
        .and_then(|item| item.submenu.as_ref())
        .expect("an Edit list");
    let labels: Vec<&str> = edit.iter().map(|item| item.label.as_str()).collect();
    let rename = labels.iter().position(|l| *l == "Rename").expect("Rename");
    assert_eq!(labels[rename + 1], "Tags…");
    assert!(edit[rename + 1].enabled);

    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
    let row = row_rect(&app, 0).center();
    app.open_menu(row);
    let menu = app.menu.as_ref().expect("the row menu is up");
    let tags = menu
        .items
        .iter()
        .find(|item| item.label == "Tags…")
        .expect("a Tags… row");
    assert!(tags.enabled);
    assert_eq!(tags.action, menu::Action::Run(Command::Tag));
}
