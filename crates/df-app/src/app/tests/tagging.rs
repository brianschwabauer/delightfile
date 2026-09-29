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

/// A link in a selection is skipped rather than failing the rest: the files
/// are tagged and journaled, and the undo toast says how many links were
/// left. A selection of nothing but links is refused before a prompt opens.
#[test]
#[cfg(unix)]
fn links_in_a_selection_are_skipped_and_counted() {
    let Some(mut app) = fixture("tags-links", &["a.txt", "b.txt"]) else {
        return;
    };
    let a = app.files.join("a.txt");
    std::os::unix::fs::symlink(&a, app.files.join("link")).expect("link");
    app.refresh_all(Instant::now());
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.tab().cwd.dir.position_of("link").is_none() && Instant::now() < deadline {
        for update in app.scanner.drain() {
            app.tabs.active_mut().apply(&update);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    at(&mut app, "a.txt", true);
    at(&mut app, "link", true);

    let (title, line) = press_t(&mut app);
    assert_eq!(title, "Tags of 2 items:");
    assert_eq!(line, "");
    enter(&mut app, "red");
    assert_eq!(file_tags::read(&a), ["red"]);
    assert_eq!(toast(&app), "Tagged a.txt red · 1 link skipped");
    assert_eq!(
        app.toasts.current().map(|t| t.kind),
        Some(crate::toast::ToastKind::Undo)
    );
    app.run(Command::Undo, 10, Instant::now());
    assert!(
        file_tags::read(&a).is_empty(),
        "the file's change was journaled"
    );

    // Only the link: nothing to tag, said at once.
    app.dir().clear_selection();
    at(&mut app, "link", false);
    app.run(Command::Tag, 10, Instant::now());
    assert!(app.prompt.is_none(), "no prompt for a link alone");
    assert_eq!(toast(&app), "Links can't hold tags");
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
    // After Permissions…, the other thing a file carries that is set here.
    assert_eq!(labels[rename + 1..rename + 3], ["Permissions…", "Tags…"]);
    assert!(edit[rename + 2].enabled);

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

/// The undo history's rows, top to bottom: each label, and whether `U`
/// would do it again.
fn history(app: &App) -> Vec<(String, bool)> {
    crate::history::rows(&app.journal)
        .into_iter()
        .map(|row| (row.label, row.redo))
        .collect()
}

/// `u`, `U`, `u` after `T`: the tag comes off, goes back on with the toast
/// an operation lands with, and comes off again, the row wearing it or not
/// each time; the undo history names the change on either side of its line.
#[test]
fn t_undone_is_redone_and_the_history_names_it() {
    let Some(mut app) = fixture("tags-redo", &["a.txt", "b.txt"]) else {
        return;
    };
    let a = app.files.join("a.txt");
    at(&mut app, "a.txt", false);
    press_t(&mut app);
    enter(&mut app, "red");
    assert_eq!(history(&app), [("Tagged a.txt".to_string(), false)]);

    app.run(Command::Undo, 10, Instant::now());
    assert!(file_tags::read(&a).is_empty());
    assert_eq!(history(&app), [("Tagged a.txt again".to_string(), true)]);

    app.run(Command::Redo, 10, Instant::now());
    assert_eq!(file_tags::read(&a), ["red"]);
    assert_eq!(toast(&app), "Tagged a.txt again");
    assert_eq!(
        app.toasts.current().map(|toast| toast.kind),
        Some(crate::toast::ToastKind::Undo),
        "a redo is an operation u takes back"
    );
    settle_here(&mut app);
    assert_eq!(entry_tags(&app, "a.txt"), ["red"]);
    assert_eq!(history(&app), [("Tagged a.txt".to_string(), false)]);

    app.run(Command::Undo, 10, Instant::now());
    assert!(file_tags::read(&a).is_empty());
    assert_eq!(toast(&app), "Restored tags of a.txt");
    settle_here(&mut app);
    assert!(entry_tags(&app, "a.txt").is_empty());
}

// ── In a search's hits ──────────────────────────────────────────────────────

/// One name in three folders: what a names search for `foo` finds in
/// [`hits_fixture`].
const FOOS: [&str; 3] = ["src/foo.txt", "src/deep/foo.txt", "docs/foo.txt"];

/// A fixture with [`FOOS`] under it, whose files can hold tags, or `None` on
/// a machine whose `$TMPDIR` cannot.
fn hits_fixture(name: &str) -> Option<Fixture> {
    let app = Fixture::with_folders(name, &["a.txt"], &["src/deep", "docs"]);
    for file in FOOS {
        std::fs::write(app.files.join(file), b"foo\n").expect("write the tree");
    }
    let probe = app.files.join("a.txt");
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

/// `names`, relative to the folder on screen, fed to the `s` panel as fd
/// prints them and committed with `Enter`: the tab's listing is their hits.
fn list_hits(app: &mut App, names: &[&str]) {
    let now = Instant::now();
    let root = app.cwd();
    app.run(Command::SearchName, 10, now);
    let search = app.search.as_mut().expect("the panel opened");
    search.seed("foo", now);
    let feed = search.feed();
    feed.hits(
        names
            .iter()
            .map(|name| search::parse(search::Mode::Names, &root, "foo", name).expect("a hit"))
            .collect(),
    );
    feed.done(false);
    app.poll_workers();
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));
}

/// A hit is a file at its own path, so its row carries the tags the file
/// does — read by the search's worker as a scan reads them — and wears their
/// dots after its path; `f #red` narrows the hits to the red ones, and a
/// mark on a hit it hides stays on it but is not acted on until the query
/// is cleared.
#[test]
fn a_hit_wears_its_tags_and_f_hash_filters_the_hits() {
    let Some(mut app) = hits_fixture("tags-hits-dots") else {
        return;
    };
    let root = app.files.clone();
    file_tags::write(&root.join("src/foo.txt"), &list(&["red", "work"])).expect("tag");
    list_hits(&mut app, &FOOS);

    assert_eq!(entry_tags(&app, "src/foo.txt"), ["red", "work"]);
    assert!(entry_tags(&app, "docs/foo.txt").is_empty());
    let dir = &app.tab().cwd.dir;
    let row = dir
        .row(dir.position_of("src/foo.txt").expect("row"))
        .expect("row");
    assert_eq!(
        app.tag_colors.dots(&row.tags, &app.palette),
        vec![app.palette.red],
        "one dot, red"
    );
    // The row paints its path and its dots without trouble.
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());

    app.run(Command::SelectAll, 10, Instant::now());
    app.dir().set_filter("#red");
    let shown: Vec<String> = app
        .tab()
        .cwd
        .dir
        .rows()
        .map(|(entry, _)| entry.name.clone())
        .collect();
    assert_eq!(shown, ["src/foo.txt"]);
    assert_eq!(app.tab().cwd.dir.selected_count(), 1);
    assert_eq!(
        app.tab().cwd.dir.selected_paths(),
        vec![root.join("src/foo.txt")],
        "only the red hit is acted on"
    );
    assert!(
        app.tab().cwd.dir.is_selected("docs/foo.txt"),
        "its mark stays"
    );
    app.dir().clear_filter();
    assert_eq!(app.tab().cwd.dir.selected_count(), 3);
}

/// `T` in a search's hits is let through — the rows are files on this disk —
/// and tags the file under the cursor at its own path, two folders down, and
/// none of the others with its name; its row wears the tag at once, and `u`
/// takes it off the file and the row again.
#[test]
fn t_in_the_hits_tags_the_file_at_its_real_path() {
    let Some(mut app) = hits_fixture("tags-hits-t") else {
        return;
    };
    let root = app.files.clone();
    list_hits(&mut app, &FOOS);
    assert_eq!(
        app.refusal(Command::Tag),
        None,
        "hits are files on this disk"
    );
    at(&mut app, "src/deep/foo.txt", false);

    let (title, line) = press_t(&mut app);
    assert_eq!(title, "Tags of foo.txt:");
    assert_eq!(line, "");
    enter(&mut app, "red");
    let deep = root.join("src/deep/foo.txt");
    assert_eq!(file_tags::read(&deep), ["red"]);
    for other in ["src/foo.txt", "docs/foo.txt"] {
        assert!(
            file_tags::read(&root.join(other)).is_empty(),
            "{other} was tagged"
        );
    }
    assert_eq!(entry_tags(&app, "src/deep/foo.txt"), ["red"]);
    assert_eq!(toast(&app), "Tagged foo.txt red");
    assert_eq!(
        app.tab().virtual_kind(),
        Some(Virtual::Hits),
        "still the hits"
    );

    app.run(Command::Undo, 10, Instant::now());
    assert_eq!(toast(&app), "Restored tags of foo.txt");
    assert!(file_tags::read(&deep).is_empty());
    assert!(
        entry_tags(&app, "src/deep/foo.txt").is_empty(),
        "the row kept the tag the undo took off"
    );
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));
}
