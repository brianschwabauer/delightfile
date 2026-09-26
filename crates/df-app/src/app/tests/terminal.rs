//! "Open terminal here": `Ctrl+t`, the folder menu, a folder row's menu and
//! the app menu's Go list, and the places a terminal cannot start.
//!
//! Nothing here opens a terminal. Each test swaps the `terminal-here` opener
//! for a snippet that writes its `$1` and its working directory into the
//! fixture's sandbox, and reads that back — the real spawn, the real shell and
//! the real argument order, with a file where the window would have been.

use df_core::config::Opener;
use df_core::keymap::Mods;

use super::*;

/// The opener this program runs for "Open terminal here", swapped for one
/// that says where it was asked to open. Returns the file it writes.
fn stub_terminal(app: &mut Fixture) -> PathBuf {
    let out = app
        .files
        .parent()
        .expect("the fixture's files are in its sandbox")
        .join("terminal.out");
    app.config
        .openers
        .retain(|opener| opener.name != open::TERMINAL_OPENER);
    app.config.openers.push(Opener {
        name: open::TERMINAL_OPENER.to_string(),
        command: format!(r#"printf '%s\n' "$1" "$PWD" > '{}'"#, out.display()),
        block: false,
        description: "Open a terminal here".to_string(),
    });
    out
}

/// The two lines the stub wrote — the folder it was handed, and the one it
/// ran in — once it has written them. The snippet is detached, so it lands
/// a moment after the key.
fn opened_in(out: &Path) -> (PathBuf, PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(out) {
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() == 2 {
                return (PathBuf::from(lines[0]), PathBuf::from(lines[1]));
            }
        }
        assert!(Instant::now() < deadline, "the opener never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn ctrl_t(app: &mut App, ctx: &egui::Context) {
    app.pending_keys.push(Press {
        repeat: false,
        chord: Some(Chord::new(Mods::CTRL, Key::Char('t'))),
        text: None,
    });
    run_frame(app, ctx, Vec::new());
}

/// `Ctrl+t` hands the folder on screen to the `terminal-here` opener, as its
/// `$1` and as its working directory, and says nothing about it.
#[test]
fn ctrl_t_opens_the_terminal_opener_on_the_folder_on_screen() {
    let mut app = Fixture::new("terminal-key", &["a.txt"]);
    let ctx = egui::Context::default();
    let out = stub_terminal(&mut app);
    run_frame(&mut app, &ctx, Vec::new());

    ctrl_t(&mut app, &ctx);
    let (argument, cwd) = opened_in(&out);
    assert_eq!(argument, app.files, "the opener's $1 is the folder");
    assert_eq!(
        cwd.canonicalize().expect("real"),
        app.files.canonicalize().expect("real"),
        "…and it runs there"
    );
    assert_eq!(toast_text(&app), None, "a launch that worked says nothing");
}

/// With no `terminal-here` opener in the config, the key says so, naming
/// the opener and the file, rather than doing nothing.
#[test]
fn without_the_opener_the_key_says_which_one_is_missing() {
    let mut app = Fixture::new("terminal-missing", &["a.txt"]);
    let ctx = egui::Context::default();
    app.config
        .openers
        .retain(|opener| opener.name != open::TERMINAL_OPENER);
    run_frame(&mut app, &ctx, Vec::new());
    ctrl_t(&mut app, &ctx);
    assert_eq!(
        toast_text(&app),
        Some("No terminal-here opener in delightfile.toml")
    );
}

/// An archive's folder, a server's and the trash are not on this disk: the
/// key is refused with one sentence in all three, and nothing is started.
#[test]
fn a_terminal_opens_only_on_a_local_folder() {
    let mut app = Fixture::new("terminal-refused", &["a.txt"]);
    let out = stub_terminal(&mut app);
    let now = Instant::now();
    let refusal = Some("Terminals open on local folders");
    assert_eq!(app.refusal(Command::TerminalHere), None, "a local folder");

    let origin = app.files.clone();
    app.tabs.active_mut().trash = Some(crate::trashview::View {
        items: Vec::new(),
        origin: origin.clone(),
    });
    assert_eq!(app.refusal(Command::TerminalHere), refusal, "the trash");
    app.run(Command::TerminalHere, 10, now);
    assert_eq!(toast_text(&app), refusal);
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
    assert_eq!(app.refusal(Command::TerminalHere), refusal, "an archive");
    app.toasts.clear();
    app.run(Command::TerminalHere, 10, now);
    assert_eq!(toast_text(&app), refusal);
    app.tabs.active_mut().archive = None;

    for at in [
        df_core::vfs::VfsPath::new("box", "/srv"),
        df_core::vfs::VfsPath::rclone("r2", "bucket"),
    ] {
        app.tabs.active_mut().remote = Some(crate::remote::Session::new(at, origin.clone()));
        assert_eq!(app.refusal(Command::TerminalHere), refusal, "a remote");
        app.toasts.clear();
        app.run(Command::TerminalHere, 10, now);
        assert_eq!(toast_text(&app), refusal);
    }
    app.tabs.active_mut().remote = None;

    // Nothing was started anywhere along the way.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !out.exists(),
        "a terminal was opened somewhere it cannot be"
    );
}

/// The folder menu has the row in a group of its own under New file… and
/// New folder…, teaching `Ctrl+t`; in the trash it is there and grey — as
/// is the app menu's Go ▸ row — where the gate would refuse it.
#[test]
fn the_folder_menu_opens_a_terminal_and_greys_it_in_the_trash() {
    let mut app = Fixture::new("terminal-folder-menu", &["a.txt"]);
    let at = egui::pos2(400.0, 400.0);
    let row = |app: &App, label: &str| -> menu::Item {
        app.menu
            .as_ref()
            .expect("a menu is up")
            .items
            .iter()
            .find(|item| item.label == label)
            .cloned()
            .unwrap_or_else(|| panic!("no {label} row"))
    };

    app.open_folder_menu(at);
    let labels: Vec<String> = app
        .menu
        .as_ref()
        .expect("up")
        .items
        .iter()
        .take(4)
        .map(|item| item.label.clone())
        .collect();
    assert_eq!(
        labels,
        ["New file…", "New folder…", "Open terminal here", "Paste"]
    );
    let terminal = row(&app, "Open terminal here");
    assert!(terminal.enabled && terminal.gap_before);
    assert_eq!(terminal.keys, "Ctrl+t");
    assert_eq!(terminal.action, menu::Action::Run(Command::TerminalHere));

    let origin = app.files.clone();
    app.tabs.active_mut().trash = Some(crate::trashview::View {
        items: Vec::new(),
        origin,
    });
    app.open_folder_menu(at);
    assert!(
        !row(&app, "Open terminal here").enabled,
        "live in the trash"
    );

    app.open_app_menu();
    let go = row(&app, "Go").submenu.expect("a list");
    let terminal = go
        .iter()
        .find(|item| item.label == "Open terminal here")
        .expect("Go ▸ Open terminal here");
    assert!(
        !terminal.enabled,
        "Go ▸ Open terminal here is live in the trash"
    );
    assert_eq!(
        go[2].label, "Open terminal here",
        "the end of Go's first group"
    );
    assert!(go.get(3).is_none_or(|next| next.gap_before));
}

/// A folder row's menu opens the terminal in *that* folder, from under Pin
/// folder — not in the folder on screen, which is `Ctrl+t`'s.
#[test]
fn a_folder_rows_menu_opens_a_terminal_in_that_folder() {
    let mut app = Fixture::with_folders("terminal-row", &["a.txt"], &["sub"]);
    let now = Instant::now();
    let out = stub_terminal(&mut app);
    let sub = app.files.join("sub");
    let index = (0..app.tab().cwd.dir.len())
        .find(|&i| app.tab().cwd.dir.row(i).is_some_and(|e| e.path == sub))
        .expect("the folder's row");
    app.dir().set_cursor(index);
    app.open_menu(egui::pos2(300.0, 300.0));
    let items = &app.menu.as_ref().expect("up").items;
    let pin = items
        .iter()
        .position(|item| item.action == menu::Action::PinRow)
        .expect("Pin folder");
    let terminal = &items[pin + 1];
    assert_eq!(terminal.label, "Open terminal here");
    assert_eq!((terminal.keys.as_str(), terminal.enabled), ("", true));

    app.menu_action(menu::Action::TerminalRow, 10, now);
    let (argument, _) = opened_in(&out);
    assert_eq!(argument, sub);

    // A file's row has no such row: its terminal is `terminal-at`, under
    // Open with.
    let file = (0..app.tab().cwd.dir.len())
        .find(|&i| app.tab().cwd.dir.row(i).is_some_and(|e| !e.is_dir()))
        .expect("the file's row");
    app.dir().set_cursor(file);
    app.open_menu(egui::pos2(300.0, 300.0));
    assert!(app
        .menu
        .as_ref()
        .expect("up")
        .items
        .iter()
        .all(|item| item.action != menu::Action::TerminalRow));
}
