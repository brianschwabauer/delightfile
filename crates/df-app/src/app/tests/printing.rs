//! Printing through the app: the gates the `print` command and opener share,
//! the folders a selection leaves out, the palette's and the app menu's rows,
//! and how a run goes on or stops.
//!
//! **Nothing here puts a print dialog up.** Every test stops at a gate, or
//! stands a run in place first ([`stand_in_run`]) so the files go into its
//! queue rather than into a job: a job would call
//! `platform::print::prepare`, which on a desktop with a print portal opens
//! the dialog on the screen of whoever runs the tests.

use std::collections::VecDeque;

use super::*;

/// A run that is already going, with no job behind it: whatever is printed
/// next queues behind it instead of starting.
fn stand_in_run(app: &mut App) -> TaskId {
    let id = TaskId::MAX;
    app.printing = Some(PrintRun {
        id,
        rest: VecDeque::new(),
    });
    id
}

/// Put the cursor on `name`, and with `select` mark it.
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

/// The app menu's Edit ▸ Print… as it would open now.
fn print_row(app: &App) -> menu::Item {
    app.app_menu_items()
        .iter()
        .find(|item| item.label == "Edit")
        .and_then(|edit| edit.submenu.as_ref())
        .and_then(|rows| rows.iter().find(|row| row.label == "Print…").cloned())
        .expect("the app menu's Edit list has Print…")
}

/// Inside an archive the command, the opener and the menu row all say the
/// rows are to be extracted first — or, where the platform has no print
/// dialog, that printing is not available, everywhere.
#[test]
fn print_is_refused_inside_an_archive_and_where_there_is_no_dialog() {
    let mut app = Fixture::new("print-archive", &["a.txt"]);
    let now = Instant::now();
    let expected = if crate::platform::print::SUPPORTED {
        assert_eq!(app.refusal(Command::Print), None, "a local folder");
        assert!(print_row(&app).enabled, "live on a local file");
        PRINT_IN_ARCHIVE
    } else {
        assert_eq!(app.refusal(Command::Print), Some(PRINT_UNSUPPORTED));
        assert!(!print_row(&app).enabled, "grey with no dialog to put up");
        PRINT_UNSUPPORTED
    };
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
    assert_eq!(app.refusal(Command::Print), Some(expected));
    assert!(!print_row(&app).enabled, "Print… is live in an archive");
    app.toasts.clear();
    app.run(Command::Print, 10, now);
    assert_eq!(toast_text(&app), Some(expected));
    // …and the opener, the way `o` or the picker would reach it.
    let choice = open::Choice {
        name: "print".to_string(),
        command: format!("builtin:{}", open::PRINT_BUILTIN),
        description: "Print…".to_string(),
        block: false,
    };
    let inside = app.files.join("x.zip/inside.txt");
    app.toasts.clear();
    app.launch(&choice, vec![inside], now);
    assert_eq!(toast_text(&app), Some(expected));
    app.tabs.active_mut().archive = None;
    assert!(app.printing.is_none(), "nothing was started");
    assert!(app.remote_ops.is_empty(), "no job was spawned");
}

/// A folder has nothing to print: alone, or with only other folders, the
/// command says so; with nothing under the cursor at all, it says that.
#[test]
fn folders_alone_are_not_printed() {
    if !crate::platform::print::SUPPORTED {
        return;
    }
    let mut app = Fixture::with_folders("print-folders", &["a.txt"], &["sub", "other"]);
    let now = Instant::now();
    at(&mut app, "sub", false);
    app.toasts.clear();
    app.run(Command::Print, 10, now);
    assert_eq!(toast_text(&app), Some("sub is a folder — only files print"));
    at(&mut app, "sub", true);
    at(&mut app, "other", true);
    app.toasts.clear();
    app.run(Command::Print, 10, now);
    assert_eq!(toast_text(&app), Some("2 folders — only files print"));
    assert!(app.printing.is_none(), "nothing was started");
    assert!(app.remote_ops.is_empty(), "no job was spawned");

    let mut empty = Fixture::new("print-nothing", &[]);
    empty.toasts.clear();
    empty.run(Command::Print, 10, now);
    assert_eq!(toast_text(&empty), Some("Nothing to print"));
    assert!(empty.printing.is_none());
}

/// Folders in a selection with files are left out, with one notice saying
/// how many, and the files join a run that is going — one dialog at a time.
#[test]
fn folders_among_files_are_skipped_and_files_queue_behind_a_run() {
    if !crate::platform::print::SUPPORTED {
        return;
    }
    let mut app = Fixture::with_folders("print-mixed", &["a.txt", "b.txt"], &["sub"]);
    let now = Instant::now();
    let id = stand_in_run(&mut app);
    at(&mut app, "sub", true);
    at(&mut app, "a.txt", true);
    at(&mut app, "b.txt", true);
    app.toasts.clear();
    app.run(Command::Print, 10, now);
    assert_eq!(
        toast_text(&app),
        Some("Skipped 1 folder — only files print")
    );
    let run = app.printing.as_ref().expect("the run is still going");
    assert_eq!(run.id, id, "the file printing now is still the one");
    assert_eq!(
        run.rest,
        VecDeque::from(vec![app.files.join("a.txt"), app.files.join("b.txt")])
    );
    assert!(app.remote_ops.is_empty(), "no second job beside the first");
    app.printing = None;
}

/// A file's job ending cancelled — its dialog was, or its task — ends the
/// run with what was waiting, said once; one ending for another task is
/// none of the run's business; and the last file's end is the run's.
#[test]
fn a_cancelled_turn_stops_the_run() {
    let mut app = Fixture::new("print-stopped", &["a.txt"]);
    let now = Instant::now();
    let id = stand_in_run(&mut app);
    let waiting = app.files.join("a.txt");
    if let Some(run) = &mut app.printing {
        run.rest.push_back(waiting);
    }
    app.toasts.clear();
    app.print_turn_over(id - 1, true, now);
    assert!(
        app.printing.is_some(),
        "another task's end is not this run's"
    );
    assert_eq!(toast_text(&app), None);
    app.print_turn_over(id, true, now);
    assert!(app.printing.is_none(), "the files after it are not printed");
    assert_eq!(toast_text(&app), Some("Printing stopped"));
    assert!(
        app.remote_ops.is_empty(),
        "and nothing was started for them"
    );

    // The last file done: the run is over, quietly.
    let id = stand_in_run(&mut app);
    app.toasts.clear();
    app.print_turn_over(id, false, now);
    assert!(app.printing.is_none());
    assert_eq!(toast_text(&app), None);
}

/// The palette offers printing, which has no key to be found under, where
/// the platform can print — and the app menu's row teaches no key.
#[test]
fn the_palette_offers_print_where_printing_is() {
    let app = Fixture::new("print-palette", &["a.txt"]);
    let rows = app.palette_rows();
    let print = rows
        .iter()
        .find(|row| matches!(row.choice, Choice::Run(Command::Print)));
    if crate::platform::print::SUPPORTED {
        let print = print.expect("Print… is in the palette");
        assert_eq!(print.label, "Print…");
        assert_eq!(print.detail, "", "unbound");
    } else {
        assert!(print.is_none(), "no row for a dialog that is not there");
    }
    assert_eq!(print_row(&app).keys, "", "unbound");
}
