//! Every bar in the hand: a card's and a menu's thumb is taken and dragged,
//! and its track paged, as a pane's is ([`crate::scrollbar::Bar`]).

use super::*;
use crate::mounts::{Card, Share};
use crate::scrollbar::{Surface, FADE, LINGER};

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
    let late = t2 + LINGER * 3;
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

    let late = t1 + LINGER * 3;
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

/// The mount card up over a long listing, thirty shares long, at `now`.
fn with_mount_card(name: &str, ctx: &egui::Context, now: Instant) -> Fixture {
    let mut app = long_listing(name);
    frame(&mut app, ctx, screen(), Vec::new(), now);
    // Built rather than opened, as `M` would start the udisks worker.
    let mut built = Card::new();
    built.update(Vec::new(), shares(30));
    app.mounts = Some(built);
    app.sync_context();
    frame(&mut app, ctx, screen(), Vec::new(), now);
    app
}

/// A thumb held when the window stops hearing the button — the release
/// went to another window, or the compositor swallowed it — is let go on the
/// first frame that finds the button up, and lingers from that frame, as it
/// would from a release it heard.
#[test]
fn a_release_the_window_never_hears_still_lets_go_of_the_bar() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_mount_card("bar-lost-release", &ctx, t0);
    let grab = mount_bar(&app).thumb.center();
    let pressed = vec![egui::Event::PointerMoved(grab), button(grab, true)];
    frame(&mut app, &ctx, screen(), pressed, t0);
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Mounts)));

    // A window that has no button down and was told of no release: a
    // context of its own, which has never seen the press.
    let late = t0 + LINGER * 3;
    let unaware = egui::Context::default();
    frame(&mut app, &unaware, screen(), Vec::new(), late);
    assert_eq!(
        app.held_bar(),
        None,
        "the bar is still in a hand that let go"
    );
    assert_eq!(app.gesture(), None);
    assert_eq!(app.card_scrolled_at(Surface::Mounts), Some(late));
}

/// The cursor a frame with `events` asks for.
fn cursor_in(
    app: &mut App,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    now: Instant,
) -> egui::CursorIcon {
    let input = egui::RawInput {
        screen_rect: Some(screen()),
        events,
        focused: true,
        ..Default::default()
    };
    ctx.run_ui(input, |ui| app.frame_at(ui, now))
        .platform_output
        .cursor_icon
}

/// Every bar wears the plain arrow — a pane's as much as a card's — over
/// its thumb and its track, and while its thumb is dragged off the band.
#[test]
fn every_bar_wears_the_arrow() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = long_listing("bar-cursor");
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    let (pane, _) = list_bar(&app);
    let track = egui::pos2(pane.thumb.center().x, pane.track.bottom() - 4.0);
    for at in [pane.thumb.center(), track] {
        let cursor = cursor_in(&mut app, &ctx, vec![egui::Event::PointerMoved(at)], t0);
        assert_eq!(
            cursor,
            egui::CursorIcon::Default,
            "the list's bar at {at:?}"
        );
    }

    let mut app = with_mount_card("bar-cursor-card", &ctx, t0);
    let grab = mount_bar(&app).thumb.center();
    let cursor = cursor_in(&mut app, &ctx, vec![egui::Event::PointerMoved(grab)], t0);
    assert_eq!(cursor, egui::CursorIcon::Default, "the mount card's bar");
    let pressed = vec![button(grab, true)];
    assert_eq!(
        cursor_in(&mut app, &ctx, pressed, t0),
        egui::CursorIcon::Default
    );
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Mounts)));
    let away = grab + egui::vec2(-200.0, 30.0);
    let dragged = vec![egui::Event::PointerMoved(away)];
    assert_eq!(
        cursor_in(&mut app, &ctx, dragged, t0),
        egui::CursorIcon::Default,
        "the thumb dragged off the band"
    );
    frame(&mut app, &ctx, screen(), vec![button(away, false)], t0);
}

// ── The help sheet's bar and the search panel's ─────────────────────────────

/// `notches` of the wheel away from the hand — down the list — with the
/// pointer at `at`, at `now`.
fn wheel_at(app: &mut App, ctx: &egui::Context, at: egui::Pos2, notches: f32, now: Instant) {
    let notch = egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -notches),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    };
    let events = vec![egui::Event::PointerMoved(at), notch];
    frame(app, ctx, screen(), events, now);
}

/// The pointer at `at`, and nothing else, at `now`.
fn point(app: &mut App, ctx: &egui::Context, at: egui::Pos2, now: Instant) {
    frame(app, ctx, screen(), vec![egui::Event::PointerMoved(at)], now);
}

/// How lit `surface`'s bar is by the pointer on its band.
fn lit(app: &App, surface: Surface) -> f32 {
    app.hovers.hover(Control::Bar(Bar::Card(surface)))
}

/// The help sheet up over a long listing, the whole keymap in it.
fn with_help(name: &str, ctx: &egui::Context, now: Instant) -> Fixture {
    let mut app = long_listing(name);
    frame(&mut app, ctx, screen(), Vec::new(), now);
    app.run(Command::Help, 10, now);
    frame(&mut app, ctx, screen(), Vec::new(), now);
    app
}

fn sheet(app: &App) -> Help {
    app.help.expect("the sheet stays up")
}

/// The help sheet's bar, from the numbers the frame lays it out with.
fn help_bar(app: &App) -> Option<crate::scrollbar::Geometry> {
    chrome::help_bar(help_card(app), &sheet(app), app.help_lines().len())
}

/// The help sheet wears a bar while its lines run past a page, in the
/// card's padding beside them, and none while they fit: narrowed to a
/// handful there is no thumb, and nothing at the sheet's edge for the
/// pointer to light.
#[test]
fn the_help_sheet_wears_a_bar_only_while_its_lines_overflow() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let app = with_help("bar-help-overflows", &ctx, t0);
    let card = help_card(&app);
    let bar = help_bar(&app).expect("the whole keymap overflows the sheet");
    assert!(card.contains_rect(bar.thumb), "the thumb is off the sheet");
    assert!(
        bar.thumb.left() >= chrome::help_body(card).right(),
        "the thumb is over the lines"
    );
    assert_eq!(
        app.card_scrolled_at(Surface::Help),
        None,
        "opening is not a scroll"
    );

    let mut app = long_listing("bar-help-fits");
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    app.run(Command::Help, 10, t0);
    app.help_query = "quit".to_string();
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    let lines = app.help_lines().len();
    assert!(
        lines > 0 && lines <= chrome::help_page(card),
        "{lines} lines"
    );
    assert_eq!(help_bar(&app), None, "a sheet that fits has a bar");
    point(&mut app, &ctx, bar.hit.center(), t0);
    assert_eq!(lit(&app, Surface::Help), 0.0, "an edge with no bar lit one");
}

/// The wheel over the sheet turns its lines and brings the bar up for the
/// linger from that frame, owed one wake-up to start fading, and gone after
/// the linger and the fade; drawn again where the wheel left it, it is not
/// stamped again. A page key turns the page and stamps it anew.
#[test]
fn the_wheel_and_the_page_keys_bring_the_help_sheets_bar_up() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_help("bar-help-wheel", &ctx, t0);
    let card = help_card(&app);
    let cursor = sheet(&app).cursor;

    let t1 = t0 + Duration::from_millis(16);
    wheel_at(&mut app, &ctx, card.center(), 1.0, t1);
    assert_eq!(sheet(&app).first, 2, "a notch is two twenty-point lines");
    assert_eq!(sheet(&app).cursor, cursor, "the wheel moved the cursor");
    let at = app.card_scrolled_at(Surface::Help);
    assert_eq!(at, Some(t1));
    assert_eq!(lit(&app, Surface::Help), 0.0, "on the lines, not the band");
    let alpha = |now| crate::scrollbar::visibility(at, 0.0, false, now);
    assert_eq!(alpha(t1), 1.0);
    assert!(app.next_deadline(t1).is_some_and(|due| due <= LINGER));
    assert!(crate::scrollbar::fading(at, t1 + LINGER + FADE / 2));
    assert_eq!(alpha(t1 + LINGER + FADE), 0.0);
    assert!(!crate::scrollbar::fading(at, t1 + LINGER + FADE));

    frame(&mut app, &ctx, screen(), Vec::new(), t1 + LINGER);
    assert_eq!(
        sheet(&app).first,
        2,
        "the scrolloff rule took the view back"
    );
    assert_eq!(app.card_scrolled_at(Surface::Help), at, "standing still");

    let t2 = t1 + LINGER * 2;
    app.help_command(Command::HelpPageDown);
    frame(&mut app, &ctx, screen(), Vec::new(), t2);
    assert!(sheet(&app).first > 2, "a page down turned no page");
    assert_eq!(app.card_scrolled_at(Surface::Help), Some(t2));
}

/// The help sheet's thumb, pressed and pulled, turns the lines with the
/// hand to the line the thumb's place stands for, held, the cursor left
/// where it was; let go, the bar lingers from the release. A press on its
/// track pages a sheet's worth and takes nothing into the hand.
#[test]
fn the_help_sheets_thumb_is_taken_and_its_track_pages() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_help("bar-help-drag", &ctx, t0);
    let bar = help_bar(&app).expect("the keymap overflows the sheet");
    let cursor = sheet(&app).cursor;

    let grab = bar.thumb.center();
    let pressed = vec![egui::Event::PointerMoved(grab), button(grab, true)];
    frame(&mut app, &ctx, screen(), pressed, t0);
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Help)));
    assert_eq!(
        app.gesture(),
        Some(select::Gesture::Bar(Bar::Card(Surface::Help)))
    );
    assert_eq!(sheet(&app).first, 0, "a press on the thumb scrolled");

    let dy = 60.0;
    let t1 = t0 + Duration::from_millis(16);
    let moved = grab + egui::vec2(0.0, dy);
    let events = vec![egui::Event::PointerMoved(moved)];
    frame(&mut app, &ctx, screen(), events, t1);
    let want = bar.first_at(bar.thumb.top() + dy).round() as usize;
    assert!(want > 0, "sixty points of travel is some lines");
    assert_eq!(sheet(&app).first, want);
    assert_eq!(sheet(&app).cursor, cursor, "the thumb moved the cursor");
    assert_eq!(app.card_scrolled_at(Surface::Help), Some(t1));
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Help)), "held");

    let late = t1 + LINGER * 3;
    frame(&mut app, &ctx, screen(), vec![button(moved, false)], late);
    assert_eq!(app.held_bar(), None);
    assert!(app.help.is_some(), "the drag closed the sheet");
    assert_eq!(sheet(&app).first, want, "letting go moved the lines");
    assert_eq!(app.card_scrolled_at(Surface::Help), Some(late));

    let bar = help_bar(&app).expect("still overflows");
    let below = egui::pos2(bar.thumb.center().x, bar.track.bottom() - 4.0);
    assert!(bar.contains(below) && !bar.on_thumb(below), "{bar:?}");
    let pressed = vec![egui::Event::PointerMoved(below), button(below, true)];
    frame(&mut app, &ctx, screen(), pressed, late);
    assert_eq!(sheet(&app).first, bar.page(false).round() as usize);
    let page = chrome::help_page(help_card(&app));
    assert_eq!(sheet(&app).first, want + page, "not a sheet's worth");
    assert_eq!(app.held_bar(), None, "the track took the thumb");
    frame(&mut app, &ctx, screen(), vec![button(below, false)], late);
    assert!(
        app.help.is_some(),
        "the press on the track closed the sheet"
    );
}

/// Long after the last scroll the sheet's bar is up only while the pointer
/// is on its band: a pointer on a line lights nothing, and the `×` in the
/// heading's corner is the `×`'s.
#[test]
fn the_help_sheets_band_lights_its_bar_and_its_lines_do_not() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_help("bar-help-hover", &ctx, t0);
    let card = help_card(&app);
    let bar = help_bar(&app).expect("the keymap overflows the sheet");

    let line = egui::pos2(chrome::help_body(card).left() + 40.0, bar.track.center().y);
    point(&mut app, &ctx, line, t0);
    assert_eq!(lit(&app, Surface::Help), 0.0, "a line lit the bar");
    let at = app.card_scrolled_at(Surface::Help);
    assert_eq!(crate::scrollbar::visibility(at, 0.0, false, t0), 0.0);

    point(&mut app, &ctx, bar.hit.center(), t0);
    let lit_now = lit(&app, Surface::Help);
    assert_eq!(lit_now, 1.0, "the band lit nothing");
    assert_eq!(crate::scrollbar::visibility(at, lit_now, false, t0), 1.0);

    let close = chrome::close_button_rect(card);
    point(
        &mut app,
        &ctx,
        close.center(),
        t0 + Duration::from_millis(16),
    );
    assert_eq!(app.hovers.hover(Control::Close), 1.0, "the × is not the ×");
}

/// The search panel open over a long listing, with `hits` names found.
fn with_hits(name: &str, ctx: &egui::Context, hits: usize, now: Instant) -> Fixture {
    let mut app = long_listing(name);
    frame(&mut app, ctx, screen(), Vec::new(), now);
    app.run(Command::SearchName, 10, now);
    app.search.as_mut().expect("the panel is open").hits = needles("needle", hits);
    frame(&mut app, ctx, screen(), Vec::new(), now);
    app
}

/// `n` name hits, `stem0.txt` on.
fn needles(stem: &str, n: usize) -> Vec<search::Hit> {
    (0..n)
        .map(|i| search::Hit {
            path: PathBuf::from(format!("/tmp/{stem}{i}.txt")),
            relative: format!("{stem}{i}.txt"),
            entry: None,
            line: None,
            text: String::new(),
            span: None,
        })
        .collect()
}

fn panel(app: &App) -> &Search {
    app.search.as_ref().expect("the panel stays up")
}

/// The search panel as the frame lays it out.
fn panel_geometry(app: &App) -> overlay::SearchGeom {
    match overlay_of(app) {
        Some(OverlayGeom::Search(geometry)) => *geometry,
        _ => panic!("no search panel"),
    }
}

/// The panel's bar, from the geometry the frame draws it from.
fn search_bar(app: &App) -> Option<crate::scrollbar::Geometry> {
    overlay::search_bar(&panel_geometry(app), panel(app))
}

/// The search panel wears a bar while its hits run past its rows and none
/// while they fit, beside the hits and clear of the switch and the field.
#[test]
fn the_search_panel_wears_a_bar_only_while_its_hits_overflow() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let app = with_hits("bar-search-fits", &ctx, 5, t0);
    let geometry = panel_geometry(&app);
    assert!(geometry.page() > 5, "the panel has no room for five");
    assert_eq!(geometry.band, None);
    assert_eq!(search_bar(&app), None, "five hits wear a bar");

    let app = with_hits("bar-search-overflows", &ctx, 200, t0);
    let geometry = panel_geometry(&app);
    let bar = search_bar(&app).expect("two hundred hits overflow the panel");
    let band = geometry.band.expect("a band beside the hits");
    assert_eq!(band, bar.hit);
    assert!(bar.thumb.left() >= geometry.body.right(), "over the hits");
    assert!(geometry.card.contains_rect(bar.thumb));
    for rect in geometry.switch.into_iter().chain([geometry.field]) {
        assert!(!band.intersects(rect), "the band is over {rect:?}");
    }
    assert_eq!(
        app.card_scrolled_at(Surface::Search),
        None,
        "opening is not a scroll"
    );
}

/// The wheel over the hits rolls them and brings the bar up for the linger
/// from that frame, and it fades to nothing after the linger and the fade.
#[test]
fn the_wheel_brings_the_search_panels_bar_up_and_it_fades() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_hits("bar-search-wheel", &ctx, 200, t0);
    let geometry = panel_geometry(&app);
    let t1 = t0 + Duration::from_millis(16);
    wheel_at(&mut app, &ctx, geometry.rows[2].center(), 1.0, t1);
    assert_eq!(panel(&app).first, 2, "a notch is two 24-point hits");
    assert_eq!(panel(&app).cursor, 0, "the wheel moved the cursor");
    let at = app.card_scrolled_at(Surface::Search);
    assert_eq!(at, Some(t1));
    assert_eq!(lit(&app, Surface::Search), 0.0, "on a hit, not the band");
    let alpha = |now| crate::scrollbar::visibility(at, 0.0, false, now);
    assert_eq!(alpha(t1), 1.0);
    assert!(app.next_deadline(t1).is_some_and(|due| due <= LINGER));
    assert!(crate::scrollbar::fading(at, t1 + LINGER + FADE / 2));
    assert_eq!(alpha(t1 + LINGER + FADE), 0.0);

    frame(&mut app, &ctx, screen(), Vec::new(), t1 + LINGER);
    assert_eq!(panel(&app).first, 2, "the view went back by itself");
    assert_eq!(app.card_scrolled_at(Surface::Search), at, "standing still");
}

/// The panel's thumb, pressed and pulled, moves the hits with the hand,
/// held, the cursor left where it was; let go, the bar lingers from the
/// release. A press on its track pages a panel's worth.
#[test]
fn the_search_panels_thumb_is_taken_and_its_track_pages() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_hits("bar-search-drag", &ctx, 200, t0);
    let bar = search_bar(&app).expect("two hundred hits overflow");

    let grab = bar.thumb.center();
    let pressed = vec![egui::Event::PointerMoved(grab), button(grab, true)];
    frame(&mut app, &ctx, screen(), pressed, t0);
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Search)));
    assert_eq!(
        app.gesture(),
        Some(select::Gesture::Bar(Bar::Card(Surface::Search)))
    );
    assert_eq!(panel(&app).first, 0, "a press on the thumb scrolled");

    let dy = 60.0;
    let t1 = t0 + Duration::from_millis(16);
    let moved = grab + egui::vec2(0.0, dy);
    let events = vec![egui::Event::PointerMoved(moved)];
    frame(&mut app, &ctx, screen(), events, t1);
    let want = bar.first_at(bar.thumb.top() + dy).round() as usize;
    assert!(want > 0, "sixty points of travel is some hits");
    assert_eq!(panel(&app).first, want);
    assert_eq!(panel(&app).cursor, 0, "the thumb moved the cursor");
    assert_eq!(app.card_scrolled_at(Surface::Search), Some(t1));
    assert_eq!(app.held_bar(), Some(Bar::Card(Surface::Search)), "held");

    let late = t1 + LINGER * 3;
    frame(&mut app, &ctx, screen(), vec![button(moved, false)], late);
    assert_eq!(app.held_bar(), None);
    assert!(app.search.is_some(), "the drag closed the panel");
    assert_eq!(panel(&app).first, want, "letting go moved the hits");
    assert_eq!(app.card_scrolled_at(Surface::Search), Some(late));

    let bar = search_bar(&app).expect("still overflows");
    let below = egui::pos2(bar.thumb.center().x, bar.track.bottom() - 4.0);
    assert!(bar.contains(below) && !bar.on_thumb(below), "{bar:?}");
    let pressed = vec![egui::Event::PointerMoved(below), button(below, true)];
    frame(&mut app, &ctx, screen(), pressed, late);
    let page = panel_geometry(&app).page();
    assert_eq!(panel(&app).first, bar.page(false).round() as usize);
    assert_eq!(panel(&app).first, want + page, "not a panel's worth");
    assert_eq!(app.held_bar(), None, "the track took the thumb");
    frame(&mut app, &ctx, screen(), vec![button(below, false)], late);
    assert!(
        app.search.is_some(),
        "the press on the track closed the panel"
    );
}

/// Long after the last scroll the panel's bar is up only while the pointer
/// is on its band: a pointer on a hit lights the hit and not the bar.
#[test]
fn the_search_panels_band_lights_its_bar_and_its_hits_do_not() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_hits("bar-search-hover", &ctx, 200, t0);
    let geometry = panel_geometry(&app);
    let band = geometry.band.expect("two hundred hits overflow");

    let hit = egui::pos2(geometry.rows[3].left() + 40.0, geometry.rows[3].center().y);
    point(&mut app, &ctx, hit, t0);
    assert_eq!(lit(&app, Surface::Search), 0.0, "a hit lit the bar");
    assert_eq!(
        app.hovers.hover(Control::PanelRow(3)),
        1.0,
        "the hit is dark"
    );
    let at = app.card_scrolled_at(Surface::Search);
    assert_eq!(crate::scrollbar::visibility(at, 0.0, false, t0), 0.0);

    point(
        &mut app,
        &ctx,
        band.center(),
        t0 + Duration::from_millis(16),
    );
    let lit_now = lit(&app, Surface::Search);
    assert_eq!(lit_now, 1.0, "the band lit nothing");
    assert_eq!(crate::scrollbar::visibility(at, lit_now, false, t0), 1.0);
}

/// A new query's hits are a new list, not the old one scrolled: while the
/// debounce holds it the old hits stay where the wheel left them, and when
/// it runs, the view goes back to the top without the bar lingering for it
/// — and the hits that land after scroll nothing either.
#[test]
fn a_new_search_query_is_not_a_scroll() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = with_hits("bar-search-query", &ctx, 200, t0);
    let geometry = panel_geometry(&app);
    let t1 = t0 + Duration::from_millis(16);
    wheel_at(&mut app, &ctx, geometry.rows[2].center(), 3.0, t1);
    assert!(panel(&app).first > 0, "the wheel did not scroll the hits");
    assert_eq!(app.card_scrolled_at(Surface::Search), Some(t1));

    app.overlay_key(Chord::from_char('n').expect("a letter"), 10, t1);
    assert_eq!(panel(&app).query(), "n", "the letter went elsewhere");
    // The field arms its debounce off the clock, not off the frame's time.
    let typed = Instant::now();
    let typing = t1 + Duration::from_millis(16);
    frame(&mut app, &ctx, screen(), Vec::new(), typing);
    assert!(panel(&app).first > 0, "the old hits were not kept up");
    assert_eq!(app.card_scrolled_at(Surface::Search), Some(t1));

    let landed = typed + search::DEBOUNCE * 2;
    frame(&mut app, &ctx, screen(), Vec::new(), landed);
    assert_eq!(panel(&app).first, 0, "the query ran no search");
    assert_eq!(
        app.card_scrolled_at(Surface::Search),
        None,
        "the new list is a scroll"
    );

    app.search.as_mut().expect("up").hits = needles("n", 200);
    frame(&mut app, &ctx, screen(), Vec::new(), landed + LINGER);
    assert_eq!(panel(&app).first, 0);
    assert_eq!(app.card_scrolled_at(Surface::Search), None);
}

/// A pin made while the sheet is up rebuilds the keymap its lines are read
/// out of: the sheet is laid out again, not scrolled, and a bar that was
/// lingering for the wheel does not go on lingering for the rebuild.
#[test]
fn a_keymap_rebuilt_under_the_help_sheet_is_not_a_scroll() {
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    let mut app = Fixture::with_folders("bar-help-pins", &["a.txt"], &["kept"]);
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    app.run(Command::Help, 10, t0);
    frame(&mut app, &ctx, screen(), Vec::new(), t0);
    let t1 = t0 + Duration::from_millis(16);
    let card = help_card(&app);
    wheel_at(&mut app, &ctx, card.center(), 1.0, t1);
    assert_eq!(app.card_scrolled_at(Surface::Help), Some(t1));
    let on_folder = app.tab().cwd.dir.cursor_entry().is_some_and(|e| e.is_dir());
    assert!(on_folder, "the cursor is not on the folder");

    app.pin_row(t1);
    assert_eq!(
        app.card_scrolled_at(Surface::Help),
        None,
        "the pin scrolled"
    );
    frame(&mut app, &ctx, screen(), Vec::new(), t1 + LINGER);
    assert_eq!(sheet(&app).first, 2, "the pin moved the sheet");
    assert_eq!(app.card_scrolled_at(Surface::Help), None);
}
