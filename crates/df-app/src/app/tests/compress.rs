//! `A` through the app: the prompt it opens, the hint on it, the refusals
//! that keep it open, the Replace card, and the job — run on the real engine
//! and landed through the real scanner, the way `finish_op` lands a paste.

use df_core::archive::write::Format;
use df_core::ops::journal::OpRecord;

use super::*;
use crate::app::compress::{format_hint, stem_for};
use crate::input::Ink;

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

/// `A`, then the prompt as it opened.
fn press_a(app: &mut App) -> (String, Option<std::ops::Range<usize>>) {
    app.run(Command::ArchiveCreate, 10, Instant::now());
    let prompt = app.prompt.as_ref().expect("A opened no prompt");
    assert_eq!(prompt.kind, PromptKind::Archive);
    assert_eq!(prompt.kind.title(), "Archive as:");
    (prompt.query().to_string(), prompt.buffer.selection())
}

/// Replace what is in the field and press `Enter`.
fn enter(app: &mut App, text: &str) {
    let now = Instant::now();
    let submitted = text.to_string();
    if let Some(prompt) = &mut app.prompt {
        prompt.buffer = InputBuffer::new(text, text.chars().count());
    }
    app.submit_prompt(submitted, now);
}

/// Let every queued job finish and land: the engine's events handed to the
/// app as the frame loop hands them, then the scanner drained until `done`
/// says the listing has caught up.
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

fn cursor_name(app: &App) -> Option<String> {
    app.tab().cwd.dir.cursor_entry().map(|e| e.name.clone())
}

fn names_in(archive: &Path) -> Vec<String> {
    let tree = df_core::archive::list(archive).expect("the archive lists");
    let mut names: Vec<String> = tree
        .all()
        .iter()
        .filter(|e| !e.synthesized)
        .map(|e| e.path.clone())
        .collect();
    names.sort();
    names
}

// ── The prompt ──────────────────────────────────────────────────────────────

#[test]
fn one_folder_prefills_its_name_with_the_stem_selected() {
    let mut app = Fixture::with_folders("archive-prefill-folder", &["a.txt"], &["photos"]);
    at(&mut app, "photos", false);
    let (text, selection) = press_a(&mut app);
    assert_eq!(text, "photos.zip");
    assert_eq!(selection, Some(0..6), "the stem is selected");
    assert_eq!(app.prompt.as_ref().map(|p| p.buffer.cursor()), Some(6));
}

#[test]
fn one_file_prefills_its_stem() {
    let mut app = Fixture::new("archive-prefill-file", &["notes.txt", "b.txt"]);
    at(&mut app, "notes.txt", true);
    let (text, selection) = press_a(&mut app);
    assert_eq!(text, "notes.zip");
    assert_eq!(selection, Some(0..5));

    // The rule on its own: the extension goes, a compound one whole, and a
    // folder keeps every dot it has.
    let cwd = Path::new("/home/me/Documents");
    let t = df_core::test_support::TempTree::new("archive-stems");
    let folder = t.dir("v1.2");
    assert_eq!(stem_for(&[cwd.join("report.pdf")], cwd), "report");
    assert_eq!(stem_for(&[cwd.join("backup.tar.gz")], cwd), "backup");
    assert_eq!(stem_for(&[cwd.join(".bashrc")], cwd), ".bashrc");
    assert_eq!(stem_for(&[folder], cwd), "v1.2");
    assert_eq!(stem_for(&[cwd.join("a"), cwd.join("b")], cwd), "Documents");
    assert_eq!(
        stem_for(&[PathBuf::from("/a"), PathBuf::from("/b")], Path::new("/")),
        "archive"
    );
}

#[test]
fn several_items_prefill_the_folder_they_are_in() {
    let mut app = Fixture::new("archive-prefill-several", &["a.txt", "b.txt", "c.txt"]);
    at(&mut app, "a.txt", true);
    at(&mut app, "c.txt", true);
    let (text, selection) = press_a(&mut app);
    assert_eq!(text, "files.zip");
    assert_eq!(selection, Some(0..5));
}

/// The hint names every format and lights the one the extension picks — a
/// name with none picks zip, and one this cannot write lights nothing.
#[test]
fn the_extension_lights_its_format_in_the_hint() {
    let lit = |text: &str| format_hint(text, &[]).inked(Ink::Strong).join("");
    assert_eq!(lit("photos.zip"), "zip");
    assert_eq!(lit("photos"), "zip");
    assert_eq!(lit("photos.tar"), "tar");
    assert_eq!(lit("photos.tar.gz"), "tar.gz");
    assert_eq!(lit("photos.tgz"), "tar.gz");
    assert_eq!(lit("photos.tar.zst"), "tar.zst");
    assert_eq!(lit("photos.tar.xz"), "tar.xz");
    assert_eq!(lit("photos.7z"), "7z");
    assert_eq!(lit("photos.rar"), "");
    assert_eq!(
        format_hint("photos.zip", &[]).text(),
        "zip · tar · tar.gz · tar.zst · tar.xz · 7z"
    );
    assert_eq!(
        format_hint("photos.rar", &[]).inked(Ink::Warn),
        ["rar cannot be written"]
    );

    // A missing program dims its format, and says what it needs when picked.
    let missing = [Format::TarZst, Format::SevenZip];
    let hint = format_hint("photos.tar.zst", &missing);
    assert_eq!(hint.inked(Ink::Absent), ["7z"]);
    assert_eq!(hint.inked(Ink::Warn), ["tar.zst needs zstd"]);
    assert_eq!(hint.inked(Ink::Strong), Vec::<&str>::new());
    let hint = format_hint("photos.zip", &missing);
    assert_eq!(hint.inked(Ink::Absent), ["tar.zst", "7z"]);

    // …and the open prompt carries it, following the text.
    let mut app = Fixture::with_folders("archive-hint", &["a.txt"], &["photos"]);
    at(&mut app, "photos", false);
    press_a(&mut app);
    app.sync_prompt_hint();
    let lit_now = |app: &App| {
        app.prompt
            .as_ref()
            .and_then(|p| p.inked_message())
            .map(|h| h.inked(Ink::Strong).join(""))
    };
    assert_eq!(lit_now(&app).as_deref(), Some("zip"));
    if let Some(prompt) = &mut app.prompt {
        prompt.buffer = InputBuffer::new("photos.tar.xz", 13);
    }
    app.sync_prompt_hint();
    let expected = if Format::TarXz.is_available() {
        "tar.xz"
    } else {
        ""
    };
    assert_eq!(lit_now(&app).as_deref(), Some(expected));
}

// ── Enter ───────────────────────────────────────────────────────────────────

/// A bare name gets `.zip`; the job lands the cursor on the archive, the
/// toast counts what went in, and `u` takes the archive away again.
#[test]
fn a_bare_name_becomes_a_zip_that_lands_under_the_cursor_and_undoes() {
    let mut app = Fixture::with_folders("archive-zip", &["a.txt"], &["photos"]);
    std::fs::write(app.files.join("photos/IMG_0001.jpg"), [0xFFu8; 4000]).expect("write");
    std::fs::write(app.files.join("photos/.hidden"), b"dot").expect("write");
    at(&mut app, "photos", false);
    press_a(&mut app);
    enter(&mut app, "trip");
    assert!(app.prompt.is_none(), "the prompt stayed open");
    let rows = app.task_rows();
    assert!(
        rows.iter()
            .any(|row| row.name == "Archive 1 item → trip.zip"),
        "{:?}",
        rows.iter().map(|r| &r.name).collect::<Vec<_>>()
    );

    land(&mut app, |app| {
        cursor_name(app).as_deref() == Some("trip.zip")
    });
    let archive = app.files.join("trip.zip");
    assert_eq!(
        names_in(&archive),
        ["photos", "photos/.hidden", "photos/IMG_0001.jpg"]
    );
    let said = toast(&app).unwrap_or_default();
    assert!(said.starts_with("Archived 1 item · "), "{said}");
    assert!(said.ends_with(" KB") || said.ends_with(" B"), "{said}");

    match app.journal.peek() {
        Some(OpRecord::Create { path, is_dir, .. }) => {
            assert_eq!(path, &archive);
            assert!(!is_dir);
        }
        other => panic!("the journal holds {other:?}"),
    }
    app.run(Command::Undo, 10, Instant::now());
    assert!(!archive.exists(), "u left the archive");
    assert!(app.files.join("photos").is_dir(), "u took the folder too");
}

#[test]
fn a_rar_is_refused_and_the_prompt_stays() {
    let mut app = Fixture::with_folders("archive-rar", &["a.txt"], &["photos"]);
    at(&mut app, "photos", false);
    press_a(&mut app);
    enter(&mut app, "photos.rar");
    let prompt = app.prompt.as_ref().expect("the prompt closed");
    assert_eq!(
        prompt.query(),
        "photos.rar",
        "the field was not left as typed"
    );
    assert!(app.ops.is_empty(), "a job was queued");
    let said = toast(&app).unwrap_or_default();
    assert!(said.starts_with("rar archives cannot be written"), "{said}");
    assert!(
        said.contains("zip, tar, tar.gz, tar.zst, tar.xz or 7z"),
        "{said}"
    );

    // A name with nothing in it is said beside the field instead.
    enter(&mut app, ".zip");
    assert_eq!(
        app.prompt.as_ref().and_then(|p| p.error.as_deref()),
        Some("no name given")
    );
}

#[test]
fn a_missing_program_is_refused_in_the_prompt() {
    let mut app = Fixture::with_folders("archive-missing-tool", &["a.txt"], &["photos"]);
    at(&mut app, "photos", false);
    press_a(&mut app);
    if let Some(draft) = &mut app.archive_draft {
        draft_missing(draft, Format::TarZst);
    }
    enter(&mut app, "photos.tar.zst");
    assert!(app.prompt.is_some(), "the prompt closed");
    assert!(app.ops.is_empty());
    assert_eq!(
        toast(&app).as_deref(),
        Some("tar.zst needs zstd, which is not installed")
    );
}

/// Stands in for a machine without the program.
fn draft_missing(draft: &mut crate::app::compress::Draft, format: Format) {
    draft.set_missing(vec![format]);
}

#[test]
fn a_name_with_a_slash_is_made_in_its_folder() {
    let mut app = Fixture::with_folders("archive-slash", &["a.txt"], &["photos"]);
    std::fs::write(app.files.join("photos/x.txt"), b"x").expect("write");
    at(&mut app, "photos", false);
    press_a(&mut app);
    enter(&mut app, "out/deeper/photos.tar");
    // The row that appears here is the folder made for it.
    land(&mut app, |app| cursor_name(app).as_deref() == Some("out"));
    let archive = app.files.join("out/deeper/photos.tar");
    assert_eq!(names_in(&archive), ["photos", "photos/x.txt"]);
    // `u` takes the archive and the folders made for it.
    app.run(Command::Undo, 10, Instant::now());
    assert!(
        !app.files.join("out").exists(),
        "the folders made for it stayed"
    );
}

#[test]
fn an_archive_inside_the_folder_it_archives_is_refused() {
    let mut app = Fixture::with_folders("archive-itself", &["a.txt"], &["photos"]);
    at(&mut app, "photos", false);
    press_a(&mut app);
    enter(&mut app, "photos/photos.zip");
    assert!(app.prompt.is_some(), "the prompt closed");
    assert!(app.ops.is_empty(), "a job was queued");
    let said = toast(&app).unwrap_or_default();
    assert!(said.contains("inside photos"), "{said}");
}

/// A taken name asks first, on the Replace card; yes writes over it, and no
/// leaves it.
#[test]
fn a_taken_name_asks_before_it_is_replaced() {
    let mut app = Fixture::with_folders("archive-replace", &["a.txt"], &["photos"]);
    std::fs::write(app.files.join("photos/new.txt"), b"new").expect("write");
    // Somebody's file, under the name the archive will ask for. On the disk
    // rather than in the listing: the question is asked of the disk.
    std::fs::write(app.files.join("photos.zip"), b"x").expect("write");
    at(&mut app, "photos", false);
    press_a(&mut app);
    enter(&mut app, "photos.zip");
    assert!(app.prompt.is_none());
    assert!(
        matches!(&app.dialog, Some(Dialog::Confirm(c)) if c.kind == ConfirmKind::Replace),
        "a taken name was not asked about"
    );
    assert!(app.ops.is_empty(), "the job started before the answer");
    assert_eq!(
        std::fs::read(app.files.join("photos.zip")).expect("still there"),
        b"x"
    );

    // No: the card goes and the file stays.
    app.close_overlay(Instant::now());
    assert!(app.dialog.is_none());
    assert_eq!(
        std::fs::read(app.files.join("photos.zip")).expect("kept"),
        b"x"
    );

    // Again, and yes.
    press_a(&mut app);
    enter(&mut app, "photos.zip");
    app.submit_overlay(10, Instant::now());
    assert!(app.dialog.is_none());
    land(&mut app, |app| {
        cursor_name(app).as_deref() == Some("photos.zip")
    });
    assert_eq!(
        names_in(&app.files.join("photos.zip")),
        ["photos", "photos/new.txt"]
    );
}

/// The Replace card a save dialog raises is still the save dialog's: an
/// archive prompt that was cancelled does not take its yes.
#[test]
fn a_cancelled_archive_does_not_answer_someone_elses_card() {
    let mut app = Fixture::with_folders("archive-card", &["a.txt"], &["photos"]);
    at(&mut app, "photos", false);
    press_a(&mut app);
    app.cancel_prompt();
    let theirs = [app.files.join("a.txt")];
    assert!(!app.archive_replace(&theirs));
    assert!(app.ops.is_empty());
}

// ── Where it is refused ─────────────────────────────────────────────────────

#[test]
fn a_is_refused_in_the_trash_an_archive_and_over_the_link() {
    let now = Instant::now();
    let mut app = Fixture::new("archive-refused", &["a.txt"]);
    let origin = app.files.clone();

    app.tabs.active_mut().trash = Some(crate::trashview::View {
        items: Vec::new(),
        origin: origin.clone(),
    });
    app.run(Command::ArchiveCreate, 10, now);
    assert!(app.prompt.is_none(), "A opened a prompt in the trash");
    assert_eq!(
        toast(&app).as_deref(),
        Some("Not in the trash — Enter restores, D destroys")
    );
    // The app menu greys it where the key refuses.
    app.open_app_menu();
    let compress = app
        .menu
        .as_ref()
        .and_then(|menu| menu.items.iter().find(|i| i.label == "Compress…").cloned())
        .expect("the app menu has Compress…");
    assert!(!compress.enabled, "Compress… is live in the trash");
    app.menu = None;
    app.tabs.active_mut().trash = None;

    let tree = df_core::archive::build(
        app.files.join("x.zip"),
        df_core::archive::ArchiveFormat::Zip,
        Vec::new(),
        false,
    );
    app.tabs.active_mut().archive = Some(crate::archive::Browse {
        path: app.files.join("x.zip"),
        tree: Arc::new(tree),
    });
    app.run(Command::ArchiveCreate, 10, now);
    assert!(app.prompt.is_none(), "A opened a prompt inside an archive");
    assert_eq!(
        toast(&app).as_deref(),
        Some("Archives are read-only — press e to extract")
    );
    app.tabs.active_mut().archive = None;

    app.tabs.active_mut().remote = Some(crate::remote::Session::new(
        df_core::vfs::VfsPath::new("box", "/home"),
        origin,
    ));
    // Through the menu row's door, which is the key's.
    app.menu_action(menu::Action::Run(Command::ArchiveCreate), 10, now);
    assert!(app.prompt.is_none(), "A opened a prompt over the link");
    assert!(
        toast(&app).is_some_and(|t| t.starts_with("Not over the link")),
        "{:?}",
        toast(&app)
    );
}

/// The row menu offers it on any row, as the command, beside the extract
/// rows when there are some.
#[test]
fn the_row_menu_offers_compress() {
    let facts = |archive: bool| menu::Facts {
        has_row: true,
        is_dir: false,
        targets: 1,
        clipboard: false,
        archive,
        archives: usize::from(archive),
        trash: false,
        trashed: 0,
    };
    for archive in [false, true] {
        let items = menu::items(facts(archive), &["Viewer".to_string()]);
        let at = items
            .iter()
            .position(|i| i.label == "Compress…")
            .expect("Compress… is on the row menu");
        assert_eq!(items[at].action, menu::Action::Run(Command::ArchiveCreate));
        assert_eq!(items[at].keys, "A");
        assert!(items[at].enabled);
        let before = &items[at - 1];
        if archive {
            assert_eq!(before.label, "Extract here", "not beside the extract rows");
        } else {
            assert_eq!(before.label, "Open with");
        }
    }
}
