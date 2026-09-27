//! The `[pick]` cards' page keys, pressed through the frame the way the
//! window hands them over: `PageUp`/`PageDown`, `Ctrl+u`/`Ctrl+d` and
//! `Home`/`End` stride a card's list and stop at its ends, while the mount
//! card's arrows go round.

use super::*;
use crate::mounts::{Card, Item, Share};

/// `n` gvfs shares, which is the Network section `n` rows long.
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

/// One key, pressed and handed to a frame.
fn press(app: &mut App, ctx: &egui::Context, mods: Mods, key: Key) {
    app.pending_keys.push(Press {
        repeat: false,
        chord: Some(Chord::new(mods, key)),
        text: None,
    });
    run_frame(app, ctx, Vec::new());
}

/// `End` is the connect row and `Home` the top, pressed twice or once;
/// `PageDown` is the rows on screen, `Ctrl+d` half of them, and the arrows
/// go round where the page keys stop.
#[test]
fn the_mount_cards_page_keys_stop_at_the_ends_and_its_arrows_go_round() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("paging-mounts", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    // Built rather than opened, as `M` would start the udisks worker.
    let mut built = Card::new();
    built.update(Vec::new(), Vec::new(), shares(30));
    app.mounts = Some(built);
    app.sync_context();
    run_frame(&mut app, &ctx, Vec::new());
    fn card(app: &App) -> &Card {
        app.mounts.as_ref().expect("the card stays up")
    }
    let last = card(&app).items().len() - 1;
    assert_eq!(last, 30, "thirty shares and the connect row");
    assert_eq!(card(&app).cursor, 0);

    press(&mut app, &ctx, Mods::NONE, Key::End);
    assert_eq!(card(&app).selected(), Some(Item::Connect));
    press(&mut app, &ctx, Mods::NONE, Key::End);
    assert_eq!(card(&app).cursor, last, "already there");
    press(&mut app, &ctx, Mods::NONE, Key::ArrowDown);
    assert_eq!(card(&app).cursor, 0, "↓ off the connect row is the top");
    press(&mut app, &ctx, Mods::NONE, Key::ArrowUp);
    assert_eq!(card(&app).cursor, last, "↑ off the top is the connect row");

    press(&mut app, &ctx, Mods::NONE, Key::Home);
    assert_eq!(card(&app).cursor, 0);
    assert_eq!(card(&app).first, 0, "the card's top is back in view");
    press(&mut app, &ctx, Mods::NONE, Key::Home);
    assert_eq!(card(&app).cursor, 0, "already there");
    press(&mut app, &ctx, Mods::NONE, Key::PageUp);
    assert_eq!(card(&app).cursor, 0, "stopped at the top, not round");

    let page = card(&app).visible_items().len();
    assert!(page > 1, "{page}");
    press(&mut app, &ctx, Mods::NONE, Key::PageDown);
    assert_eq!(card(&app).cursor, page, "a page is the rows on screen");
    press(&mut app, &ctx, Mods::NONE, Key::PageUp);
    assert_eq!(card(&app).cursor, 0);

    let half = card(&app).visible_items().len() / 2;
    press(&mut app, &ctx, Mods::CTRL, Key::Char('d'));
    assert_eq!(card(&app).cursor, half);
    press(&mut app, &ctx, Mods::CTRL, Key::Char('u'));
    assert_eq!(card(&app).cursor, 0);
    let half = card(&app).visible_items().len() / 2;
    press(&mut app, &ctx, Mods::CTRL, Key::ArrowDown);
    assert_eq!(card(&app).cursor, half, "Ctrl+↓ is the half page too");
    press(&mut app, &ctx, Mods::CTRL, Key::ArrowUp);
    assert_eq!(card(&app).cursor, 0);

    let page = card(&app).visible_items().len();
    press(&mut app, &ctx, Mods::CTRL, Key::Char('f'));
    assert_eq!(card(&app).cursor, page, "Ctrl+f is the page too");
    press(&mut app, &ctx, Mods::CTRL, Key::Char('b'));
    assert_eq!(card(&app).cursor, 0);

    // Paged down from near the bottom, the cursor stops on the connect row.
    press(&mut app, &ctx, Mods::NONE, Key::End);
    press(&mut app, &ctx, Mods::NONE, Key::ArrowUp);
    press(&mut app, &ctx, Mods::NONE, Key::PageDown);
    assert_eq!(card(&app).cursor, last);
    // …and the view went with it.
    let on = card(&app).selected().expect("a row");
    assert!(card(&app).visible_items().contains(&on));
    assert!(app.udisks.is_none(), "a key started the udisks worker");
}

/// The opener picker shows every choice in a window tall enough for them, so
/// a page is all of them: the page keys go to its ends and its arrows stop
/// there.
#[test]
fn the_opener_pickers_page_keys_go_to_its_ends() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("paging-picker", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    let choices = (0..5)
        .map(|i| open::Choice {
            name: format!("opener {i}"),
            command: "true".to_string(),
            description: String::new(),
            block: false,
        })
        .collect();
    let anchor = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(200.0, 22.0));
    app.picker = Some(Picker::new(choices, vec![app.files.join("a.txt")], anchor));
    app.sync_context();
    let cursor = |app: &App| app.picker.as_ref().expect("the picker stays up").cursor;

    press(&mut app, &ctx, Mods::NONE, Key::End);
    assert_eq!(cursor(&app), 4);
    press(&mut app, &ctx, Mods::NONE, Key::ArrowDown);
    assert_eq!(cursor(&app), 4, "the picker's arrows stop at the ends");
    press(&mut app, &ctx, Mods::NONE, Key::Home);
    assert_eq!(cursor(&app), 0);
    press(&mut app, &ctx, Mods::NONE, Key::PageDown);
    assert_eq!(cursor(&app), 4);
    press(&mut app, &ctx, Mods::NONE, Key::PageUp);
    assert_eq!(cursor(&app), 0);
    press(&mut app, &ctx, Mods::CTRL, Key::Char('d'));
    assert_eq!(cursor(&app), 2, "half of five");
}

/// The search panel's `PageDown` is a page of the hits it shows, measured
/// off the geometry its paint reads before the key is routed — not a page of
/// the pane beside it, and not one row for want of a measurement.
#[test]
fn the_search_panels_page_is_the_hits_it_shows() {
    let ctx = egui::Context::default();
    let mut app = Fixture::new("paging-search", &["a.txt"]);
    run_frame(&mut app, &ctx, Vec::new());
    app.run(Command::SearchName, 10, Instant::now());
    {
        // No query, so nothing runs; the hits are put in by hand.
        let search = app.search.as_mut().expect("the panel is open");
        for i in 0..200 {
            search.hits.push(search::Hit {
                path: PathBuf::from(format!("/tmp/hit{i}.txt")),
                relative: format!("hit{i}.txt"),
                entry: None,
                line: None,
                text: String::new(),
                span: None,
            });
        }
    }
    run_frame(&mut app, &ctx, Vec::new());
    let Some(OverlayGeom::Search(geometry)) = overlay_of(&app) else {
        panic!("no search panel");
    };
    let page = geometry.page();
    assert!(page > 1, "{page}");
    assert_eq!(app.search_rows, page);
    let cursor = |app: &App| app.search.as_ref().expect("the panel stays up").cursor;

    press(&mut app, &ctx, Mods::NONE, Key::PageDown);
    assert_eq!(cursor(&app), page);
    press(&mut app, &ctx, Mods::NONE, Key::PageDown);
    assert_eq!(cursor(&app), page * 2);
    press(&mut app, &ctx, Mods::NONE, Key::PageUp);
    assert_eq!(cursor(&app), page);
    press(&mut app, &ctx, Mods::CTRL, Key::ArrowUp);
    assert_eq!(cursor(&app), page - page / 2);
    for _ in 0..200 / page + 1 {
        press(&mut app, &ctx, Mods::NONE, Key::PageDown);
    }
    assert_eq!(cursor(&app), 199, "stopped on the last hit");
    let search = app.search.as_ref().expect("the panel");
    assert!(
        search.first <= 199 && 199 < search.first + page,
        "the view went with it"
    );
}
