//! Light and dark: the `theme-*` commands, the desktop's word through the
//! portal watcher, and the Appearance radios — and that a window turned
//! light still paints every surface it has.

use super::*;
use crate::appearance::{
    fake_bus, scheme_signal, setting_changed, Connect, Desktop, FakePortal, Link,
};

/// The two sides of the default theme, as the painter gets them.
fn mocha() -> Palette {
    Palette::from_theme(&Theme::default(), Appearance::Dark)
}

fn latte() -> Palette {
    Palette::from_theme(&Theme::default(), Appearance::Light)
}

/// What the portal would say, as it comes off the wire: `2` light, `1` dark,
/// `0` no preference.
fn desktop_says(portal: &FakePortal, value: u32) {
    let scheme = setting_changed(&scheme_signal(value)).expect("a colour scheme signal");
    portal.say(scheme);
}

/// Poll until the workers have been quiet for a few passes in a row, so a
/// test's next `poll_workers` answers for what the test did and nothing a
/// scan or a preview was still finishing.
fn settle_workers(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = 0;
    while quiet < 3 && Instant::now() < deadline {
        if app.poll_workers() {
            quiet = 0;
        } else {
            quiet += 1;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `theme-light` swaps the palette in one go, and every kind of surface —
/// the panes, a clicked row's ripple, the app menu and a submenu, the help
/// sheet on its scrim, the palette card, a toast — paints on the light side.
/// `theme-dark` puts back exactly the palette the window opened with.
#[test]
fn theme_light_turns_the_window_and_every_surface_still_paints() {
    let mut app = Fixture::new("theme-light", &["a.txt", "b.txt", "c.txt"]);
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
    assert_eq!(app.palette, mocha(), "the default session opens dark");

    app.run(Command::ThemeLight, 10, Instant::now());
    assert_eq!(app.palette, latte());
    assert!(app.palette.light);
    assert_eq!(toast_text(&app), Some("Light theme"));
    // Chrome over a playing picture stays on the dark side.
    assert!(!app.media_palette.light);
    run_frame(&mut app, &ctx, Vec::new());

    // A row clicked, so a ripple is drawn in the light side's ink.
    let at = row_centre(&app, 1);
    click_at(&mut app, &ctx, at);
    run_frame(&mut app, &ctx, Vec::new());

    // The app menu, with a list flown out of it.
    app.run(Command::AppMenu, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    let view = row_labelled(&app, "View");
    let at = menu_geometry(&app, &ctx).rows[view].center();
    run_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
    assert_eq!(app.menu.as_ref().and_then(|m| m.submenu), Some(view));
    app.close_menu(Instant::now());

    // The cards that dim the window, on the light side's pale scrim.
    app.run(Command::Help, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    app.run(Command::OverlayClose, 10, Instant::now());
    app.run(Command::CommandPalette, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    app.run(Command::OverlayClose, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());

    app.run(Command::ThemeDark, 10, Instant::now());
    assert_eq!(app.palette, mocha());
    assert_eq!(toast_text(&app), Some("Dark theme"));
    run_frame(&mut app, &ctx, Vec::new());

    // Following a desktop this session has no way to reach is dark, and the
    // toast says it could not be reached rather than claim to follow it.
    app.run(Command::ThemeLight, 10, Instant::now());
    app.run(Command::ThemeAuto, 10, Instant::now());
    assert_eq!(app.palette, mocha());
    assert_eq!(
        toast_text(&app),
        Some("Could not reach the desktop's light or dark setting — staying dark")
    );
}

/// Following the desktop: a `SettingChanged` turns the window the next time
/// the workers are drained; a session that has chosen a side keeps it
/// whatever the desktop says — but still hears it, so `theme-auto` lands on
/// the desktop's side at once; and "no preference" is dark.
#[test]
fn the_desktop_is_followed_only_while_the_session_follows_it() {
    let mut app = Fixture::new("theme-desktop", &["a.txt"]);
    let ctx = egui::Context::default();
    let (desktop, portal) = Desktop::fake();
    app.desktop = Some(desktop);
    assert_eq!(app.theme_mode, ThemeMode::Auto, "the shipped mode");
    run_frame(&mut app, &ctx, Vec::new());
    assert_eq!(app.palette, mocha(), "dark until the desktop says");

    desktop_says(&portal, 2);
    assert!(app.poll_workers(), "turning is a change worth a frame");
    assert_eq!(app.palette, latte());
    run_frame(&mut app, &ctx, Vec::new());
    // Saying it again changes nothing, and asks for nothing.
    settle_workers(&mut app);
    desktop_says(&portal, 2);
    assert!(!app.poll_workers(), "the same word again is no change");
    assert_eq!(app.palette, latte());

    // A side chosen for the session holds against the desktop.
    app.run(Command::ThemeDark, 10, Instant::now());
    assert_eq!(app.palette, mocha());
    desktop_says(&portal, 1);
    desktop_says(&portal, 2);
    app.poll_workers();
    assert_eq!(app.palette, mocha(), "the override ignored the desktop");
    assert_eq!(app.desktop_side, Appearance::Light, "but heard it");

    // Back to following: straight to the desktop's latest word.
    app.run(Command::ThemeAuto, 10, Instant::now());
    assert_eq!(app.palette, latte());
    assert_eq!(toast_text(&app), Some("Following the desktop (light)"));

    // Dark, and then no preference, which is dark as well.
    desktop_says(&portal, 1);
    app.poll_workers();
    assert_eq!(app.palette, mocha());
    desktop_says(&portal, 2);
    desktop_says(&portal, 0);
    app.poll_workers();
    assert_eq!(
        app.palette,
        mocha(),
        "the newest word wins, and it was none"
    );
    run_frame(&mut app, &ctx, Vec::new());

    // The line drops: the window stays where it is, and `theme-auto` says
    // the desktop could not be reached rather than that it is following it.
    desktop_says(&portal, 2);
    app.poll_workers();
    portal.hang_up();
    app.poll_workers();
    assert_eq!(app.palette, latte(), "a dropped line says nothing");
    app.run(Command::ThemeAuto, 10, Instant::now());
    assert_eq!(
        toast_text(&app),
        Some("Could not reach the desktop's light or dark setting — staying light")
    );
}

/// A connector over the pair-socket fixture: each connection is a new make-
/// believe bus whose portal prefers the next scheme in `schemes`, and the test
/// keeps the far ends to speak through.
fn fixture_buses(schemes: &[u32]) -> (Connect, crossbeam_channel::Receiver<fake_bus::Peer>) {
    let schemes = std::sync::Mutex::new(schemes.to_vec());
    let (peers_tx, peers) = crossbeam_channel::unbounded();
    let connect: Connect = Arc::new(move || {
        let scheme = {
            let mut left = schemes.lock().map_err(|_| "poisoned".to_string())?;
            if left.is_empty() {
                return Err("the fixture is out of buses".to_string());
            }
            left.remove(0)
        };
        let (bus, peer) = fake_bus::bus(scheme);
        let _ = peers_tx.send(peer);
        Ok(bus)
    });
    (connect, peers)
}

/// Until `app`'s watcher is at `link`, polling the workers as a frame would,
/// a few seconds at most.
fn until_link(app: &mut App, link: Link) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_workers();
        if app.desktop.as_ref().map(Desktop::link) == Some(link) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the watcher never got to {link:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A watcher whose bus drops is started again: not on the next frame, but on
/// one after [`crate::appearance::RETRY`] — and at once when `theme-auto`
/// asks. The new one asks afresh, so a change made while nobody was
/// listening turns the window; and `theme-auto` says what it follows only
/// once the new line is listening.
#[test]
fn a_watcher_whose_line_drops_is_started_again_and_asks_afresh() {
    let mut app = Fixture::new("theme-restart", &["a.txt"]);
    let (connect, peers) = fixture_buses(&[2, 1, 2]);
    app.desktop_bus = Some(connect);
    app.follow_desktop(true, Instant::now());

    until_link(&mut app, Link::Listening);
    assert_eq!(app.palette, latte(), "the first bus's desktop is light");
    let first = peers.recv().expect("the first bus");

    // The bus goes. The window stays light; the watcher is gone; a frame
    // straight after starts nothing, since the last start was just now.
    first.say(fake_bus::Say::Drop);
    until_link(&mut app, Link::Gone);
    assert_eq!(app.palette, latte());
    app.revive_desktop(Instant::now());
    assert_eq!(app.desktop.as_ref().map(Desktop::link), Some(Link::Gone));
    assert!(peers.try_recv().is_err(), "no second connection yet");

    // Once it is due, the next frame starts one — which asks, and hears that
    // the desktop went dark while nobody was listening.
    app.revive_desktop(Instant::now() + crate::appearance::RETRY);
    until_link(&mut app, Link::Listening);
    assert_eq!(app.palette, mocha());
    let second = peers.recv().expect("the second bus");

    // Dropped again, and this time `theme-auto` asks: started at once, and
    // the toast waits for the new line to be listening before it says so.
    second.say(fake_bus::Say::Drop);
    until_link(&mut app, Link::Gone);
    app.run(Command::ThemeAuto, 10, Instant::now());
    until_link(&mut app, Link::Listening);
    assert_eq!(app.palette, latte());
    assert_eq!(toast_text(&app), Some("Following the desktop (light)"));
    let _third = peers.recv().expect("the third bus");
}

/// The Appearance radios tick the side the session asked for, and a click on
/// one runs its command: the window turns and the menu is gone.
#[test]
fn the_appearance_radios_follow_the_session() {
    let mut app = Fixture::new("theme-menu", &["a.txt"]);
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
    let ticked = |app: &App| -> Vec<String> {
        let menu = app.menu.as_ref().expect("up");
        menu.items
            .iter()
            .find(|i| i.label == "Appearance")
            .and_then(|i| i.submenu.as_ref())
            .expect("an Appearance list")
            .iter()
            .filter(|i| i.checked == Some(true))
            .map(|i| i.label.clone())
            .collect()
    };

    app.run(Command::AppMenu, 10, Instant::now());
    assert_eq!(ticked(&app), vec!["Follow the desktop"]);
    app.close_menu(Instant::now());
    app.run(Command::ThemeLight, 10, Instant::now());
    app.run(Command::AppMenu, 10, Instant::now());
    assert_eq!(ticked(&app), vec!["Light"]);

    // Through the pointer: fly the list out and click Dark.
    run_frame(&mut app, &ctx, Vec::new());
    let parent = row_labelled(&app, "Appearance");
    let at = menu_geometry(&app, &ctx).rows[parent].center();
    run_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
    assert_eq!(app.menu.as_ref().and_then(|m| m.submenu), Some(parent));
    let dark = app
        .menu
        .as_ref()
        .and_then(|m| m.sub_items())
        .and_then(|rows| rows.iter().position(|i| i.label == "Dark"))
        .expect("a Dark row");
    let g = menu_geometry(&app, &ctx);
    let at = g.sub.as_ref().expect("the Appearance list is out").1[dark].center();
    click_at(&mut app, &ctx, at);
    assert_eq!(live_menu(&app), None);
    assert_eq!(app.theme_mode, ThemeMode::Dark);
    assert_eq!(app.palette, mocha());
    app.run(Command::AppMenu, 10, Instant::now());
    assert_eq!(ticked(&app), vec!["Dark"]);
}

/// `[flavor] mode = "light"` opens light with no desktop to ask, and a
/// `[palette.light]` override is in the palette it opens with.
#[test]
fn a_session_opens_on_the_side_its_theme_asks_for() {
    let root = std::env::temp_dir().join(format!("df-theme-mode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _sandbox = Sandbox(root.clone());
    let files = root.join("files");
    std::fs::create_dir_all(&files).expect("make the fixture directory");
    let (theme, warnings) = Theme::parse(
        "[flavor]\nmode = \"light\"\n\n[palette.light]\nbase = \"#fafafa\"\n",
        Path::new("theme.toml"),
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut app = App::assemble(
        Waker {
            ring: Arc::new(|| {}),
            source: "test",
        },
        crate::cli::Args {
            start: Some(files),
            ..Default::default()
        },
        Config::default(),
        theme,
        Registry::defaults(),
        StateStore::load_from(root.join("state").join("state")),
    );
    assert_eq!(app.theme_mode, ThemeMode::Light);
    assert!(app.palette.light);
    assert_eq!(app.palette.base, egui::Color32::from_rgb(0xfa, 0xfa, 0xfa));
    assert!(app.desktop.is_none(), "a test's app never asks the bus");
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
}

/// A folder picked up and carried while the window turns light wears the
/// same icon as its row on either side — a shipped folder's yazi colour on
/// the dark side and latte's accent for it on the light, as the rows do —
/// because the ghost's face is drawn each frame from the row it came off.
#[test]
fn a_ghost_in_the_hand_turns_with_the_rows() {
    let mut app = Fixture::with_folders("theme-ghost", &["a.txt"], &["Work"]);
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());
    let index = (0..app.tab().cwd.dir.len())
        .find(|&i| app.tab().cwd.dir.row(i).is_some_and(|e| e.name == "Work"))
        .expect("the Work row");
    let row_icon = |app: &App| {
        let entry = app.tab().cwd.dir.row(index).expect("the row").clone();
        crate::icons::icon_for(&entry, &app.theme, &app.palette, app.nerd)
    };
    let ghost_icon = |app: &App| {
        let drag = app.drag.as_ref().expect("a drag in the hand");
        drag.face.icon(&app.theme, &app.palette, app.nerd)
    };

    // Press on the row and carry it past the threshold, the button held.
    let from = row_centre(&app, index);
    let to = from + egui::vec2(0.0, crate::mouse::DRAG_THRESHOLD * 3.0);
    run_frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    run_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
    assert!(app.drag.is_some(), "the row was picked up");
    assert_eq!(ghost_icon(&app), row_icon(&app));
    assert_eq!(
        ghost_icon(&app).color,
        egui::Color32::from_rgb(0xf7, 0x76, 0x8e),
        "yazi's colour for Work, on the dark side"
    );

    app.run(Command::ThemeLight, 10, Instant::now());
    run_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(to)]);
    assert!(app.drag.is_some(), "still in the hand");
    assert_eq!(ghost_icon(&app), row_icon(&app));
    assert_eq!(ghost_icon(&app).color, latte().red);
}
