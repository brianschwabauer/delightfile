//! Light and dark: the `theme-*` commands, the desktop's word through the
//! portal watcher, and the Appearance radios — and that a window turned
//! light still paints every surface it has.

use super::*;
use crate::appearance::{scheme_signal, setting_changed, Desktop, Scheme};

/// The two sides of the default theme, as the painter gets them.
fn mocha() -> Palette {
    Palette::from_theme(&Theme::default(), Appearance::Dark)
}

fn latte() -> Palette {
    Palette::from_theme(&Theme::default(), Appearance::Light)
}

/// What the portal would say, as it comes off the wire: `2` light, `1` dark,
/// `0` no preference.
fn desktop_says(portal: &crossbeam_channel::Sender<Scheme>, value: u32) {
    let scheme = setting_changed(&scheme_signal(value)).expect("a colour scheme signal");
    portal.send(scheme).expect("the app is listening");
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
    desktop_says(&portal, 2);
    app.poll_workers();
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
