//! The trash's weight and its clock: the chip beside the counter, the Empty
//! trash card's question, the du channel's one door, and the purge that
//! keeps away from an open trash.
//!
//! Every trash here is the fixture's own, under its sandbox. The app's clock
//! is only ever wound onto one of those: a test `App` has no clock at all
//! ([`App::for_test`]), so nothing here can reach the trash of the machine
//! running it.

use std::time::SystemTime;

use df_core::du::{DuMessage, DuToken, DuTotals, DuUpdate};
use df_core::ops::trash::{iso8601_utc, trashinfo_text, TRASHINFO_EXT};
use df_core::ops::{Trash, TrashedItem};

use super::*;
use crate::app::trash::{Clock, PURGE_EVERY};
use crate::folders::Size;

/// 1.2 GB, in the size column's 1024s.
const GB_1_2: u64 = 1_288_490_189;

/// A trash in the fixture's sandbox.
fn sandbox_trash(app: &Fixture) -> Trash {
    let trash = Trash::at(
        app.files
            .parent()
            .expect("the fixture's files are in its sandbox")
            .join("Trash"),
    );
    trash.ensure().expect("make the trash");
    trash
}

/// Put an item in `trash` with the deletion date its record says.
fn plant(trash: &Trash, name: &str, deleted: SystemTime) -> TrashedItem {
    let file = trash.files_dir().join(name);
    std::fs::write(&file, vec![b'x'; 10_000]).expect("write");
    let original = PathBuf::from(format!("/nonexistent/{name}"));
    let deleted_at = iso8601_utc(deleted);
    std::fs::write(
        trash.info_dir().join(format!("{name}.{TRASHINFO_EXT}")),
        trashinfo_text(&original, &deleted_at),
    )
    .expect("record");
    TrashedItem {
        trash_root: trash.root().to_path_buf(),
        name: std::ffi::OsString::from(name),
        original,
        deleted_at,
    }
}

/// A moment, `days` ago.
fn days_ago(days: u64) -> SystemTime {
    SystemTime::now() - Duration::from_secs(days * 86_400)
}

/// Show `items` in the active tab, the way `g t` does.
fn show(app: &mut Fixture, items: Vec<TrashedItem>) {
    let view = crate::trashview::View {
        items,
        origin: app.files.clone(),
    };
    let (mgr, sort) = (app.mgr.clone(), app.sort());
    app.tabs
        .active_mut()
        .show_trash(view, &mgr, sort, Instant::now());
}

fn done(token: DuToken, bytes: u64) -> DuMessage {
    DuMessage::Done {
        token,
        root: PathBuf::from("/t/Trash/files"),
        totals: DuTotals {
            total_bytes: bytes,
            apparent_bytes: bytes,
            files: 2,
            dirs: 1,
        },
    }
}

fn running(token: DuToken, bytes: u64) -> DuMessage {
    let root = PathBuf::from("/t/Trash/files");
    DuMessage::Progress {
        token,
        root: root.clone(),
        updates: vec![DuUpdate {
            dir: root,
            depth: 0,
            total_bytes: bytes,
            apparent_bytes: bytes,
            files: 1,
            dirs: 1,
            done: false,
            entries: 2,
        }],
    }
}

fn chip(app: &App) -> Option<String> {
    app.cluster(Instant::now()).trash.map(|chip| chip.label)
}

/// The chip is up in the trash view and nowhere else, and reads the count,
/// then the bytes under the size column's `~`, then the bytes settled.
#[test]
fn the_trash_chip_weighs_the_trash_as_it_is_counted() {
    let mut app = Fixture::new("trash-chip", &["a.txt"]);
    assert_eq!(chip(&app), None, "a folder has no trash chip");
    let trash = sandbox_trash(&app);
    let items = vec![
        plant(&trash, "one.txt", days_ago(1)),
        plant(&trash, "two.txt", days_ago(2)),
    ];
    show(&mut app, items);
    assert_eq!(
        chip(&app).as_deref(),
        Some("2 items"),
        "nothing counted yet"
    );

    let token = DuToken(7_001);
    app.trash_weight.begin(token);
    app.du_backlog = vec![running(token, 700 * 1024 * 1024)];
    let _ = app.drain_du();
    assert_eq!(chip(&app).as_deref(), Some("2 items · ~700.0 MB"));
    app.du_backlog = vec![done(token, GB_1_2)];
    let _ = app.drain_du();
    assert_eq!(chip(&app).as_deref(), Some("2 items · 1.2 GB"));
    // …and the frame draws it, beside the counter.
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx, Vec::new());

    // The tooltip is the trash's clock — and there is none at zero.
    let tip = app.cluster(Instant::now()).trash.and_then(|chip| chip.tip);
    assert_eq!(
        tip.as_deref(),
        Some("Items are removed for good after 30 days")
    );
    app.config.mgr.trash_keep_days = 0;
    let tip = app.cluster(Instant::now()).trash.and_then(|chip| chip.tip);
    assert_eq!(tip, None);

    // An empty trash has no chip: the counter's `0 / 0` and the pane's
    // "empty" already say it — with the clock under it, drawn by the frame.
    show(&mut app, Vec::new());
    assert_eq!(chip(&app), None);
    app.config.mgr.trash_keep_days = 30;
    run_frame(&mut app, &ctx, Vec::new());
}

/// Against a real `files/` directory and the real scanner: the walk runs in
/// the background, and the chip settles on what `du` says the trash weighs.
#[test]
fn the_trash_is_weighed_by_the_du_scanner() {
    let mut app = Fixture::new("trash-weigh", &["a.txt"]);
    let trash = sandbox_trash(&app);
    let items = vec![plant(&trash, "one.bin", days_ago(1))];
    show(&mut app, items);
    app.weigh_trash(trash.files_dir());
    assert!(app.trash_weight.token().is_some(), "a walk was asked for");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.trash_weight.size().is_some_and(|size| size.settled) {
        assert!(Instant::now() < deadline, "the walk never settled");
        app.poll_workers();
        std::thread::sleep(Duration::from_millis(5));
    }
    let expected =
        df_core::du::walk_blocking(&trash.files_dir(), &df_core::du::DuOptions::at_depth(0))
            .expect("walk")
            .total_bytes;
    assert_eq!(
        chip(&app),
        Some(format!("1 item · {}", crate::format::human_size(expected)))
    );

    // Leaving the trash stops a walk nobody will read.
    app.weigh_trash(trash.files_dir());
    app.tabs.active_mut().trash = None;
    app.poll_workers();
    assert_eq!(app.trash_weight.token(), None);
}

/// The scanner's one channel has three readers. Whichever drains it, the
/// trash's messages reach the trash and everybody else's are handed back —
/// not dropped, which is what the other two readers do with what is not
/// theirs.
#[test]
fn the_du_channel_is_drained_through_one_door() {
    let mut app = Fixture::new("trash-drain", &["a.txt"]);
    let ours = DuToken(9_001);
    let theirs = DuToken(9_002);
    app.trash_weight.begin(ours);
    app.du_backlog = vec![running(theirs, 5), done(ours, 4096), done(theirs, 6)];
    let rest = app.drain_du();
    let tokens: Vec<DuToken> = rest.iter().map(DuMessage::token).collect();
    assert_eq!(
        tokens,
        vec![theirs, theirs],
        "the others' messages, in order"
    );
    assert_eq!(
        app.trash_weight.size(),
        Some(Size {
            bytes: 4096,
            settled: true
        })
    );
    // With the trash's walk over, a drain is only a drain.
    app.du_backlog = vec![done(ours, 1)];
    assert_eq!(app.drain_du().len(), 1);
}

/// The Empty trash card asks with the chip's numbers — and keeps asking with
/// them as the walk settles under it.
#[test]
fn the_empty_trash_card_says_what_it_frees() {
    let mut app = Fixture::new("trash-confirm", &["a.txt"]);
    let trash = sandbox_trash(&app);
    let items = vec![
        plant(&trash, "one.txt", days_ago(1)),
        plant(&trash, "two.txt", days_ago(2)),
    ];
    show(&mut app, items);
    let token = DuToken(8_001);
    app.trash_weight.begin(token);
    app.du_backlog = vec![running(token, GB_1_2)];
    let _ = app.drain_du();

    let now = Instant::now();
    app.run(Command::EmptyTrash, 10, now);
    let title = |app: &App| match &app.dialog {
        Some(Dialog::Confirm(confirm)) => confirm.title(),
        _ => panic!("no confirm is up"),
    };
    assert_eq!(
        title(&app),
        "Empty the trash? 2 items · ~1.2 GB will be deleted for good."
    );
    app.du_backlog = vec![done(token, GB_1_2)];
    let _ = app.drain_du();
    assert_eq!(
        title(&app),
        "Empty the trash? 2 items · 1.2 GB will be deleted for good."
    );
}

/// The clock never runs in a file dialog, with no home trash, or with
/// `trash_keep_days = 0`; otherwise it is owed a purge at once.
#[test]
fn the_clock_runs_only_in_a_file_manager_that_keeps_a_limit() {
    let now = Instant::now();
    let home = Some(PathBuf::from("/nonexistent/Trash"));
    let clock = Clock::starting(false, home.clone(), 30, now);
    assert_eq!(clock.deadline(now), Some(Duration::ZERO));
    for (picker, root, days) in [
        (true, home.clone(), 30),
        (false, None, 30),
        (false, home, 0),
    ] {
        let clock = Clock::starting(picker, root, days, now);
        assert_eq!(clock.deadline(now), None, "{picker} {days}");
    }
}

/// While any tab has the trash open, the owed purge does not run — it is
/// put off to the next day's — and when the trash is closed it runs as a
/// task, takes only what is older than the keep, and says so.
#[test]
fn the_purge_waits_for_the_trash_to_be_closed() {
    let mut app = Fixture::new("trash-purge-clock", &["a.txt"]);
    let trash = sandbox_trash(&app);
    let old = plant(&trash, "old.txt", days_ago(45));
    let recent = plant(&trash, "recent.txt", days_ago(3));
    let now = Instant::now();
    app.trash_clock = Clock::starting(false, Some(trash.root().to_path_buf()), 30, now);

    // The trash open in a tab that is not even the one on screen.
    app.run(Command::TabCreate, 10, now);
    assert_eq!(app.tabs.active_index(), 1);
    let origin = app.files.clone();
    fn first(app: &mut App) -> &mut Tab {
        app.tabs.iter_mut().next().expect("the first tab")
    }
    first(&mut app).trash = Some(crate::trashview::View {
        items: vec![old.clone(), recent.clone()],
        origin,
    });
    assert!(app.tab().trash.is_none());
    app.tick_trash_clock(now);
    assert_eq!(
        app.trash_clock.running(),
        None,
        "a purge ran under an open trash"
    );
    assert_eq!(
        app.trash_clock.deadline(now),
        Some(PURGE_EVERY),
        "put off to the next day's"
    );
    assert!(old.files_path().exists());

    // The trash is shut, and nothing is owed before the next day's.
    first(&mut app).trash = None;
    app.tick_trash_clock(now + PURGE_EVERY / 2);
    assert_eq!(app.trash_clock.running(), None);

    // A day on, with the trash shut, it runs — in `w`, like any task.
    let later = now + PURGE_EVERY;
    app.tick_trash_clock(later);
    let id = app.trash_clock.running().expect("the purge is queued");
    assert!(app.engine.snapshot().iter().any(|task| task.id == id));
    assert_eq!(
        app.engine.join(id, Duration::from_secs(10)),
        Some(TaskState::Done)
    );
    app.poll_workers();
    assert_eq!(app.trash_clock.running(), None);
    assert_eq!(
        toast_text(&app),
        Some("Emptied 1 item older than 30 days from the trash")
    );
    assert!(!old.files_path().exists(), "the old item survived");
    assert!(!old.info_path().exists(), "…or its record did");
    assert!(recent.files_path().exists(), "the recent item was purged");
    assert_eq!(app.trash_clock.deadline(later), Some(PURGE_EVERY));
}
