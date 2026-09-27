//! Whose wheel it is: a card's list scrolls under the wheel over the card,
//! a card that dims the window spends the wheel everywhere else on it, and a
//! card that floats without dimming it leaves the panes the wheel past its
//! edge ([`wheel_owner`]).

use super::*;
use crate::mounts::{Card, Share};
use crate::scrollbar::Surface;

/// `n` gvfs shares, which is the mount card's Network section `n` rows long.
fn shares(n: usize) -> Vec<Share> {
    (0..n)
        .map(|i| Share {
            url: format!("sftp://me@host{i}/"),
            label: format!("me@host{i}"),
            scheme: "sftp".to_string(),
            path: PathBuf::from(format!("/run/user/1000/gvfs/sftp:host=host{i},user=me")),
        })
        .collect()
}

/// One notch of a wheel away from the hand — down the list — or, negative,
/// towards it, with the pointer at `at`.
fn wheel_at(app: &mut App, ctx: &egui::Context, at: egui::Pos2, notches: f32) {
    let notch = egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -notches),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    };
    run_frame(app, ctx, vec![egui::Event::PointerMoved(at), notch]);
}

/// Where the list's scroll is headed, rather than where its glide has got to
/// this instant.
fn settled(app: &App) -> f32 {
    app.tab()
        .cwd
        .scroll_rows(Instant::now() + Duration::from_secs(5))
}

/// A long listing with the mount card up over it, thirty shares long.
fn with_mount_card(name: &str, ctx: &egui::Context) -> Fixture {
    let mut app = long_listing(name);
    run_frame(&mut app, ctx, Vec::new());
    // Built rather than opened, as `M` would start the udisks worker.
    let mut built = Card::new();
    built.update(Vec::new(), Vec::new(), shares(30));
    app.mounts = Some(built);
    app.sync_context();
    run_frame(&mut app, ctx, Vec::new());
    app
}

fn mount_card(app: &App) -> &Card {
    app.mounts.as_ref().expect("the card stays up")
}

/// The wheel over the mount card rolls its lines, leaves its cursor where it
/// was and the list behind it where it was, and brings the card's bar up;
/// the lines stay where the wheel left them until a key moves the cursor,
/// which brings its row back into view.
#[test]
fn the_wheel_over_the_mount_card_scrolls_the_card_and_not_the_list() {
    let ctx = egui::Context::default();
    let mut app = with_mount_card("wheel-mounts", &ctx);
    let Some(OverlayGeom::Mounts(geometry)) = overlay_of(&app) else {
        panic!("no mount card");
    };
    assert!(geometry.band.is_some(), "thirty shares overflow the card");
    let list = settled(&app);
    assert_eq!(mount_card(&app).first, 0);
    let cursor = mount_card(&app).cursor;

    wheel_at(&mut app, &ctx, geometry.body.center(), 3.0);
    let first = mount_card(&app).first;
    assert!(first > 0, "the wheel did not scroll the card");
    assert_eq!(
        mount_card(&app).cursor,
        cursor,
        "the wheel moved the cursor"
    );
    assert_eq!(settled(&app), list, "the list scrolled under the card");
    assert!(
        app.card_scrolled_at(Surface::Mounts).is_some(),
        "the card's bar did not come up for the scroll"
    );

    run_frame(&mut app, &ctx, Vec::new());
    assert_eq!(
        mount_card(&app).first,
        first,
        "the view went back by itself"
    );

    // Back up past the top: held there.
    for _ in 0..10 {
        wheel_at(&mut app, &ctx, geometry.body.center(), -1.0);
    }
    assert_eq!(mount_card(&app).first, 0);

    // Down again, then a key: the cursor's row comes back into view.
    wheel_at(&mut app, &ctx, geometry.body.center(), 20.0);
    let card = mount_card(&app);
    assert!(!card
        .visible_items()
        .contains(&card.selected().expect("a row")));
    app.overlay_move(1);
    run_frame(&mut app, &ctx, Vec::new());
    let card = mount_card(&app);
    assert!(
        card.visible_items()
            .contains(&card.selected().expect("a row")),
        "a key left the cursor off the card"
    );
    assert_eq!(settled(&app), list);
}

/// The palette: the wheel over it rolls its rows and not the list, and the
/// cursor stays on the row it was on.
#[test]
fn the_wheel_over_the_palette_scrolls_the_palette_and_not_the_list() {
    let ctx = egui::Context::default();
    let mut app = long_listing("wheel-palette");
    run_frame(&mut app, &ctx, Vec::new());
    app.open_palette();
    run_frame(&mut app, &ctx, Vec::new());
    let Some(OverlayGeom::Finder(geometry)) = overlay_of(&app) else {
        panic!("no palette");
    };
    assert!(geometry.band.is_some(), "the palette overflows its card");
    let list = settled(&app);

    wheel_at(&mut app, &ctx, geometry.body.center(), 1.0);
    let finder = app.finder.as_ref().expect("the palette stays up");
    // A notch is fifty points, and a palette row twenty.
    assert_eq!(finder.first, 2, "the wheel did not scroll the palette");
    assert_eq!(finder.cursor, 0, "the wheel moved the cursor");
    assert_eq!(settled(&app), list, "the list scrolled under the palette");
    assert!(app.card_scrolled_at(Surface::Palette).is_some());

    run_frame(&mut app, &ctx, Vec::new());
    let finder = app.finder.as_ref().expect("the palette stays up");
    assert_eq!(finder.first, 2, "the scrolloff rule took the view back");
}

/// The task panel, a dozen tasks long: the wheel over it rolls the tasks and
/// not the list.
#[test]
fn the_wheel_over_the_task_panel_scrolls_the_panel_and_not_the_list() {
    let ctx = egui::Context::default();
    let mut app = long_listing("wheel-tasks");
    for i in 0..12 {
        app.engine
            .spawn(FnJob::new(format!("Job {i}"), Lane::Micro, |_ctx| Ok(())));
    }
    app.run(Command::TasksShow, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    let Some(OverlayGeom::Panel(_, rects, rows)) = overlay_of(&app) else {
        panic!("no task panel");
    };
    assert!(rows.len() > rects.len(), "the tasks fit the panel");
    let list = settled(&app);

    wheel_at(&mut app, &ctx, rects[2].center(), 2.0);
    let panel = app.panel.as_ref().expect("the panel stays up");
    assert!(panel.first > 0, "the wheel did not scroll the panel");
    assert_eq!(panel.cursor, 0, "the wheel moved the cursor");
    assert_eq!(settled(&app), list, "the list scrolled under the panel");
    assert!(app.card_scrolled_at(Surface::Tasks).is_some());

    let first = panel.first;
    run_frame(&mut app, &ctx, Vec::new());
    let panel = app.panel.as_ref().expect("the panel stays up");
    assert_eq!(panel.first, first, "the view went back by itself");
}

/// The delete confirm, forty names long: the wheel over the names rolls
/// them and not the list.
#[test]
fn the_wheel_over_the_confirm_scrolls_its_names_and_not_the_list() {
    let ctx = egui::Context::default();
    let mut app = long_listing("wheel-confirm");
    run_frame(&mut app, &ctx, Vec::new());
    app.run(Command::SelectAll, 10, Instant::now());
    app.run(Command::DeletePermanently, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    let Some(OverlayGeom::Confirm(geometry)) = overlay_of(&app) else {
        panic!("no confirm");
    };
    let list = settled(&app);
    let scroll = |app: &App| match &app.dialog {
        Some(Dialog::Confirm(confirm)) => confirm.scroll,
        _ => panic!("the confirm went"),
    };

    wheel_at(&mut app, &ctx, geometry.rows[1].center(), 1.0);
    assert_eq!(scroll(&app), 2, "a notch is two twenty-point names");
    assert_eq!(settled(&app), list, "the list scrolled under the confirm");
    assert!(app.card_scrolled_at(Surface::Confirm).is_some());
}

/// With a card up that dims the window, a wheel anywhere off the card moves
/// nothing: not the card, not the list behind the scrim, not the preview.
#[test]
fn a_card_with_a_backdrop_spends_the_wheel_off_its_edge() {
    let ctx = egui::Context::default();
    let mut app = with_mount_card("wheel-backdrop", &ctx);
    let Some(OverlayGeom::Mounts(geometry)) = overlay_of(&app) else {
        panic!("no mount card");
    };
    let list = settled(&app);
    let layout = layout_of(&app);
    let over_list = egui::pos2(
        layout.list.center().x,
        (geometry.card.bottom() + layout.list.bottom()) / 2.0,
    );
    assert!(layout.list.contains(over_list) && !geometry.card.contains(over_list));

    for at in [over_list, layout.preview.center(), layout.parent.center()] {
        assert!(!geometry.card.contains(at), "{at:?} is on the card");
        wheel_at(&mut app, &ctx, at, 3.0);
        assert_eq!(settled(&app), list, "the list scrolled behind the scrim");
        assert_eq!(mount_card(&app).first, 0, "a roll off the card scrolled it");
    }
    assert!(app.mounts.is_some(), "the wheel closed the card");
}

/// A card that floats without dimming the window owns the wheel only over
/// itself: past the task panel's edge the list scrolls as it always has, and
/// the panel stays where it was.
#[test]
fn a_card_without_a_backdrop_leaves_the_wheel_to_the_pane_past_its_edge() {
    let ctx = egui::Context::default();
    let mut app = long_listing("wheel-no-backdrop");
    for i in 0..12 {
        app.engine
            .spawn(FnJob::new(format!("Job {i}"), Lane::Micro, |_ctx| Ok(())));
    }
    app.run(Command::TasksShow, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    let Some(geometry @ OverlayGeom::Panel(..)) = overlay_of(&app) else {
        panic!("no task panel");
    };
    let list = settled(&app);
    let over_list = row_centre(&app, 2);
    assert!(!geometry.covers(over_list), "row 2 is under the panel");

    wheel_at(&mut app, &ctx, over_list, 1.0);
    assert!(settled(&app) > list, "the list did not scroll");
    assert_eq!(app.panel.as_ref().map(|panel| panel.first), Some(0));
}

/// The help sheet dims the window too: the wheel over it rolls its lines,
/// and off it moves nothing.
#[test]
fn the_wheel_turns_the_help_sheet_and_nothing_behind_it() {
    let ctx = egui::Context::default();
    let mut app = long_listing("wheel-help");
    run_frame(&mut app, &ctx, Vec::new());
    app.run(Command::Help, 10, Instant::now());
    run_frame(&mut app, &ctx, Vec::new());
    let card = help_card(&app);
    let list = settled(&app);
    let first = |app: &App| app.help.map(|help| help.first).expect("the sheet stays up");

    wheel_at(&mut app, &ctx, card.center(), 1.0);
    assert_eq!(first(&app), 2, "a notch is two twenty-point lines");
    assert_eq!(settled(&app), list, "the list scrolled under the sheet");
    run_frame(&mut app, &ctx, Vec::new());
    assert_eq!(first(&app), 2, "the scrolloff rule took the sheet back");

    let off = egui::pos2(card.center().x, (card.bottom() + screen().bottom()) / 2.0);
    assert!(!card.contains(off));
    wheel_at(&mut app, &ctx, off, 1.0);
    assert_eq!(first(&app), 2, "a roll off the sheet turned it");
    assert_eq!(settled(&app), list, "the list scrolled behind the scrim");
}
