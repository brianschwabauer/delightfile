//! Every bar in the hand: a card's and a menu's thumb is taken and dragged,
//! and its track paged, as a pane's is ([`crate::scrollbar::Bar`]).

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

/// One frame in `window` at `now`, with `events`.
fn frame(
    app: &mut App,
    ctx: &egui::Context,
    window: egui::Rect,
    events: Vec<egui::Event>,
    now: Instant,
) {
    let input = egui::RawInput {
        screen_rect: Some(window),
        events,
        focused: true,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |ui| app.frame_at(ui, now));
}

fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// The mount card thirty shares long, as the frame fits it to the window.
fn thirty_shares(now: Instant) -> Card {
    let mut card = Card::new();
    card.update(Vec::new(), shares(30));
    card.fit(crate::mounts::window(screen()), now);
    card
}

fn mount_card(app: &App) -> &Card {
    app.mounts.as_ref().expect("the card stays up")
}

/// The mount card's bar, from the geometry the frame lays it out with.
fn mount_bar(app: &App) -> crate::scrollbar::Geometry {
    let Some(OverlayGeom::Mounts(geometry)) = overlay_of(app) else {
        panic!("no mount card");
    };
    crate::mounts::bar(&geometry, mount_card(app)).expect("thirty shares overflow the card")
}

/// Pressed and pulled, the mount card's thumb moves the lines with the hand
/// — to the line the thumb's place on the track stands for — held bright
/// while it is in the hand, even once the hand has wandered off the card; it
/// leaves the cursor and the list behind the card where they were; and let
/// go, the bar lingers from the release.
#[test]
fn the_mount_cards_thumb_is_taken_and_dragged() {
    let ctx = egui::Context::default();
    let mut app = long_listing("bar-mounts");
    let t0 = Instant::now();
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    // Built rather than opened, as `M` would start the udisks worker.
    let mut built = Card::new();
    built.update(Vec::new(), shares(30));
    app.mounts = Some(built);
    app.sync_context();
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    let bar = mount_bar(&app);
    let list = app.tab().cwd.first();
    let cursor = mount_card(&app).cursor;

    // Taken by the middle: nothing moves yet, and the bar is in the hand.
    let grab = bar.thumb.center();
    let pressed = vec![egui::Event::PointerMoved(grab), button(grab, true)];
    frame(&mut app, &ctx, screen(), pressed, t0);
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Mounts)));
    assert_eq!(
        app.gesture(),
        Some(select::Gesture::Bar(Bar::Card(Surface::Mounts)))
    );
    assert_eq!(mount_card(&app).first, 0, "a press on the thumb scrolled");

    // Down by 60 points: the line the thumb's new place stands for.
    let dy = 60.0;
    let t1 = t0 + Duration::from_millis(16);
    let moved = grab + egui::vec2(0.0, dy);
    frame(
        &mut app,
        &ctx,
        screen(),
        vec![egui::Event::PointerMoved(moved)],
        t1,
    );
    let mut twin = thirty_shares(t0);
    twin.scroll_to(bar.first_at(bar.thumb.top() + dy), t1);
    assert!(twin.first > 0, "sixty points of travel is some lines");
    assert_eq!(mount_card(&app).first, twin.first);
    assert_eq!(
        mount_card(&app).cursor,
        cursor,
        "the thumb moved the cursor"
    );
    assert_eq!(
        app.tab().cwd.first(),
        list,
        "the list behind the card moved"
    );
    assert_eq!(app.card_scrolled_at(Surface::Mounts), Some(t1));
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Mounts)), "held");

    // Off the card to the left and further down: still the thumb's.
    let Some(OverlayGeom::Mounts(geometry)) = overlay_of(&app) else {
        panic!("the card went");
    };
    let away = grab + egui::vec2(-geometry.card.width(), dy * 2.0);
    assert!(!geometry.card.contains(away));
    let t2 = t1 + Duration::from_millis(16);
    frame(
        &mut app,
        &ctx,
        screen(),
        vec![egui::Event::PointerMoved(away)],
        t2,
    );
    let mut twin = thirty_shares(t0);
    twin.scroll_to(bar.first_at(bar.thumb.top() + dy * 2.0), t2);
    assert_eq!(
        mount_card(&app).first,
        twin.first,
        "off the card, the drag stopped"
    );
    assert!(app.mounts.is_some(), "the drag off the card closed it");

    // Held still a long while, then let go: the bar lingers from the release.
    let late = t2 + crate::scrollbar::LINGER * 3;
    frame(&mut app, &ctx, screen(), vec![button(away, false)], late);
    assert_eq!(app.held_bar(), None);
    assert_eq!(app.card_scrolled_at(Surface::Mounts), Some(late));
    assert_eq!(
        mount_card(&app).first,
        twin.first,
        "letting go moved the lines"
    );
}

/// A press on the mount card's track, below the thumb, pages a view's worth
/// down and takes nothing into the hand.
#[test]
fn a_press_on_a_cards_track_pages_towards_it() {
    let ctx = egui::Context::default();
    let mut app = long_listing("bar-mounts-page");
    let t0 = Instant::now();
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    let mut built = Card::new();
    built.update(Vec::new(), shares(30));
    app.mounts = Some(built);
    app.sync_context();
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    let bar = mount_bar(&app);

    let below = egui::pos2(bar.thumb.center().x, bar.track.bottom() - 4.0);
    assert!(bar.contains(below) && !bar.on_thumb(below));
    let pressed = vec![egui::Event::PointerMoved(below), button(below, true)];
    frame(&mut app, &ctx, screen(), pressed, t0);
    let mut twin = thirty_shares(t0);
    twin.scroll_to(bar.page(false), t0);
    assert!(twin.first > 0);
    assert_eq!(mount_card(&app).first, twin.first, "not a page down");
    assert_eq!(
        app.held_bar(),
        None,
        "the track took the thumb into the hand"
    );
    frame(&mut app, &ctx, screen(), vec![button(below, false)], t0);
    assert_eq!(mount_card(&app).first, twin.first);
}

/// A press on a card's row is the row's, as it always was: the task panel's
/// row is selected, and no bar is taken into the hand.
#[test]
fn a_press_on_a_cards_row_is_the_rows() {
    let ctx = egui::Context::default();
    let mut app = long_listing("bar-row");
    for i in 0..12 {
        app.engine
            .spawn(FnJob::new(format!("Job {i}"), Lane::Micro, |_ctx| Ok(())));
    }
    app.run(Command::TasksShow, 10, Instant::now());
    let t0 = Instant::now();
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    let Some(OverlayGeom::Panel(_, rects, _)) = overlay_of(&app) else {
        panic!("no task panel");
    };
    let row = egui::pos2(rects[3].left() + 20.0, rects[3].center().y);
    frame(
        &mut app,
        &ctx,
        screen(),
        vec![egui::Event::PointerMoved(row), button(row, true)],
        t0,
    );
    assert_eq!(app.held_bar(), None);
    let panel = app.panel.as_ref().expect("the panel stays up");
    assert_eq!(panel.cursor, 3, "the press did not select the row");
    assert_eq!(panel.first, 0);
    frame(&mut app, &ctx, screen(), vec![button(row, false)], t0);
}

/// The app menu in a window too short for it: its thumb is taken and
/// dragged, the rows following the hand point for point, the bar held while
/// the menu stays up and its cursor where it was; a press on its track pages;
/// and let go, the bar lingers from the release.
#[test]
fn a_menus_thumb_is_taken_and_dragged() {
    let names: Vec<String> = (0..80).map(|i| format!("{i:02}.txt")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut app = Fixture::new("bar-menu", &names);
    let ctx = egui::Context::default();
    let short = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 360.0));
    let t0 = Instant::now();
    frame(&mut app, &ctx, short, Vec::new(), t0);
    app.pending_keys.push(Press {
        repeat: false,
        chord: Some(Chord::plain(Key::F(10))),
        text: None,
    });
    frame(&mut app, &ctx, short, Vec::new(), t0);
    let laid_out = |app: &App| {
        let mut out = None;
        let _ = ctx.run_ui(Default::default(), |ui| {
            let menu = app.menu.as_ref().expect("a menu is up");
            out = Some(menu::geometry(short, menu, ui.painter()));
        });
        out.expect("measured")
    };
    let bar = laid_out(&app)
        .bar
        .expect("the menu scrolls in a short window");
    let cursor = app.menu.as_ref().and_then(|menu| menu.cursor);

    let grab = bar.thumb.center();
    let pressed = vec![egui::Event::PointerMoved(grab), button(grab, true)];
    frame(&mut app, &ctx, short, pressed, t0);
    assert_eq!(app.held_bar(), Some(Bar::Menu));
    let dy = 40.0;
    let t1 = t0 + Duration::from_millis(16);
    let moved = grab + egui::vec2(0.0, dy);
    frame(
        &mut app,
        &ctx,
        short,
        vec![egui::Event::PointerMoved(moved)],
        t1,
    );
    let menu = app.menu.as_ref().expect("the menu stays up");
    assert!(menu.live(), "the drag closed the menu");
    let want = bar.first_at(bar.thumb.top() + dy);
    assert!(want > 0.0);
    assert!(
        (menu.scroll - want).abs() < 1e-3,
        "{} ≠ {want}",
        menu.scroll
    );
    assert_eq!(menu.scrolled_at, Some(t1));
    assert_eq!(menu.cursor, cursor, "the drag moved the menu's cursor");
    assert_eq!(app.held_bar(), Some(Bar::Menu), "held");

    let late = t1 + crate::scrollbar::LINGER * 3;
    frame(&mut app, &ctx, short, vec![button(moved, false)], late);
    let menu = app.menu.as_ref().expect("the menu stays up");
    assert!(menu.live(), "letting go closed the menu");
    assert_eq!(app.held_bar(), None);
    assert_eq!(menu.scrolled_at, Some(late), "the bar does not linger");
    assert!(
        (menu.scroll - want).abs() < 1e-3,
        "letting go moved the rows"
    );

    // The track above the thumb: a view's worth back up, to the top.
    let bar = laid_out(&app).bar.expect("still scrolls");
    let above = egui::pos2(bar.thumb.center().x, bar.track.top() + 1.0);
    assert!(bar.contains(above) && !bar.on_thumb(above), "{bar:?}");
    frame(
        &mut app,
        &ctx,
        short,
        vec![egui::Event::PointerMoved(above), button(above, true)],
        late,
    );
    let menu = app.menu.as_ref().expect("the menu stays up");
    assert!(
        (menu.scroll - bar.page(true)).abs() < 1e-3,
        "{}",
        menu.scroll
    );
    assert_eq!(app.held_bar(), None);
    frame(&mut app, &ctx, short, vec![button(above, false)], late);
    assert!(app.menu.as_ref().is_some_and(Menu::live));
}
