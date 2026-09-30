//! "Open terminal here": `Ctrl+t`, the folder menu, a folder row's menu and
//! the app menu's Go list, and the places a terminal cannot start.
//!
//! Nothing here opens a terminal. Each test swaps the `terminal-here` opener
//! for a snippet that writes its `$1` and its working directory into the
//! fixture's sandbox, and reads that back — the real spawn, the real shell and
//! the real argument order, with a file where the window would have been. On
//! Windows, where an opener is an argument list and not a shell snippet
//! (`plans/other-platforms/04-windows.md` W4.3), the snippet is a `.cmd`
//! beside the file and the opener names it and its `$1`.

use df_core::config::Opener;
use df_core::keymap::Mods;

use super::*;

/// The opener this program runs for "Open terminal here", swapped for one
/// that says where it was asked to open. Returns the file it writes.
fn stub_terminal(app: &mut Fixture) -> PathBuf {
    let sandbox = app
        .files
        .parent()
        .expect("the fixture's files are in its sandbox")
        .to_path_buf();
    let out = sandbox.join("terminal.out");
    let command = if cfg!(windows) {
        let stub = sandbox.join("terminal.cmd");
        std::fs::write(
            &stub,
            format!("@(echo %~1& cd) > \"{}\"\r\n", out.display()),
        )
        .expect("write the stub");
        format!(r#""{}" "$1""#, stub.display())
    } else {
        format!(r#"printf '%s\n' "$1" "$PWD" > '{}'"#, out.display())
    };
    app.config
        .openers
        .retain(|opener| opener.name != open::TERMINAL_OPENER);
    app.config.openers.push(Opener {
        name: open::TERMINAL_OPENER.to_string(),
        command,
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
    // Written the platform's way (M2.21).
    let ctrl_t = if cfg!(target_os = "macos") {
        "⌘t"
    } else {
        "Ctrl+t"
    };
    assert_eq!(terminal.keys, ctrl_t);
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
/// folder — not in the folder on screen, which is `Ctrl+t`'s — as its `$1`
/// and as its working directory both.
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
    let (argument, cwd) = opened_in(&out);
    assert_eq!(argument, sub);
    // …and started there, for an opener that trusts `$PWD` over `$1`.
    assert_eq!(
        cwd.canonicalize().expect("real"),
        sub.canonicalize().expect("real"),
        "the row's terminal ran in the folder on screen"
    );

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

/// A search's hits are a listing, not a folder: `Ctrl+t` over them is
/// refused with the sentence the other virtual listings get, the folder
/// menu greys its row, and nothing is started. A folder *among* the hits is
/// a real folder at its own path, though: its row menu's "Open terminal
/// here" is live and opens the terminal in it.
#[test]
fn ctrl_t_is_refused_in_a_search_s_hits_and_a_folder_row_s_terminal_is_not() {
    let mut app = Fixture::with_folders("terminal-hits", &["a.txt"], &["src/deep"]);
    std::fs::write(app.files.join("src/foo.txt"), b"foo\n").expect("write the tree");
    let out = stub_terminal(&mut app);
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());

    let now = Instant::now();
    let root = app.cwd();
    app.run(Command::SearchName, 10, now);
    let search = app.search.as_mut().expect("the panel opened");
    search.seed("foo", now);
    let feed = search.feed();
    feed.hits(
        ["src/foo.txt", "src/deep"]
            .iter()
            .map(|name| search::parse(search::Mode::Names, &root, "foo", name).expect("a hit"))
            .collect(),
    );
    feed.done(false);
    app.poll_workers();
    app.overlay_key(Chord::plain(Key::Enter), 10, now);
    assert_eq!(app.tab().virtual_kind(), Some(Virtual::Hits));

    let refusal = Some("Terminals open on local folders");
    assert_eq!(app.refusal(Command::TerminalHere), refusal);
    ctrl_t(&mut app, &ctx);
    assert_eq!(toast_text(&app), refusal);

    app.open_folder_menu(egui::pos2(400.0, 400.0));
    let row = app
        .menu
        .as_ref()
        .expect("a menu is up")
        .items
        .iter()
        .find(|item| item.label == "Open terminal here")
        .cloned()
        .expect("the row");
    assert!(!row.enabled, "live over the hits");

    std::thread::sleep(Duration::from_millis(50));
    assert!(!out.exists(), "a terminal was opened over the hits");

    let deep = root.join("src/deep");
    let index = app
        .tab()
        .cwd
        .dir
        .position_of(&format!("src{}deep", std::path::MAIN_SEPARATOR))
        .expect("the folder's row");
    app.dir().set_cursor(index);
    app.open_menu(egui::pos2(300.0, 300.0));
    let items = &app.menu.as_ref().expect("up").items;
    let terminal = items
        .iter()
        .find(|item| item.action == menu::Action::TerminalRow)
        .expect("the folder row's terminal");
    assert!(terminal.enabled, "greyed on a real folder among the hits");

    app.menu_action(menu::Action::TerminalRow, 10, now);
    let (argument, cwd) = opened_in(&out);
    assert_eq!(argument, deep, "the row's folder, at its real path");
    assert_eq!(
        cwd.canonicalize().expect("real"),
        deep.canonicalize().expect("real")
    );
    assert_eq!(
        app.tab().virtual_kind(),
        Some(Virtual::Hits),
        "still the hits"
    );
}
