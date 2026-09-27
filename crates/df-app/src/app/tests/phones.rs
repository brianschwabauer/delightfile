//! Phones and cameras on the Places card, through the app: `Enter` mounting
//! one and going into it, `u` putting it away, what a locked phone is told,
//! gvfs's events heard, and the trees the size column and the grid's
//! thumbnails leave alone.
//!
//! No phone, no gvfs and no udisks2 take part. `gio` is a stand-in that notes
//! what it was asked and answers as gio would ([`crate::mounts::Gio`]), the
//! udisks worker is one with no thread behind it
//! ([`crate::mounts::Mounts::detached`]), gvfs's events are handed to the app
//! as the watcher's thread would hand them, and gvfs-fuse's directory is a
//! folder in the fixture's sandbox.

use std::os::unix::process::ExitStatusExt;
use std::sync::Mutex;

use super::*;
use crate::mounts::{Change, Event, Gio, Item, Phone, Protocol, Request};

const ROOT: &str = "mtp://Google_Pixel_10a_4B021FDAQ00123/";
const DIR: &str = "mtp:host=Google_Pixel_10a_4B021FDAQ00123";

/// What the stand-in gio has been asked, one argument list per run.
type Asked = Arc<Mutex<Vec<Vec<String>>>>;

/// A gio that answers every run with `code` and `stderr`.
fn fake_gio(app: &mut Fixture, code: i32, stderr: &'static str) -> Asked {
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let gio: Gio = Arc::new(move |args: &[&str]| {
        log.lock()
            .expect("the log")
            .push(args.iter().map(|arg| arg.to_string()).collect());
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    });
    app.gio = gio;
    asked
}

/// gvfs-fuse's directory, in the sandbox.
fn gvfs(app: &mut Fixture) -> PathBuf {
    let dir = app.files.with_file_name("gvfs");
    std::fs::create_dir_all(&dir).expect("make the gvfs directory");
    app.gvfs = dir.clone();
    dir
}

/// The Places card up with the phone on it, the worker detached, the cursor
/// on the phone. Returns what the worker is asked.
fn card_with(app: &mut Fixture, phone: Phone) -> crossbeam_channel::Receiver<Request> {
    let (worker, asked) = crate::mounts::Mounts::detached();
    app.udisks = Some(worker);
    let mut card = app.mount_card();
    card.update(Vec::new(), vec![phone], Vec::new());
    card.select(Item::Phone(0));
    app.mounts = Some(card);
    app.sync_context();
    asked
}

fn pixel(mount: Option<PathBuf>) -> Phone {
    Phone {
        root: ROOT.to_string(),
        name: "Pixel 10a".to_string(),
        protocol: Protocol::Mtp,
        mount,
    }
}

fn toast(app: &App) -> Option<String> {
    app.toasts.current().map(|toast| toast.message.clone())
}

/// Go to `dir` and let its listing land.
fn go(app: &mut App, dir: &Path) {
    app.navigate(dir.to_path_buf(), Instant::now());
    settle(app.tabs.active_mut(), &app.scanner);
}

/// Until every mount the card started has come back and been read.
fn land(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.connects.is_empty() && Instant::now() < deadline {
        let events: Vec<TaskEvent> = app.task_events.try_iter().collect();
        for event in events {
            app.task_event(event, Instant::now());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.connects.is_empty(), "the mount never landed");
}

/// `Enter` on a phone that is not mounted runs `gio mount <root>` — on the
/// pool, its row saying so meanwhile — and the card stays up, listing again,
/// for the next `Enter` to go in.
#[test]
fn enter_on_a_phone_mounts_it_by_its_root() {
    let mut app = Fixture::new("phone-mount", &["a.txt"]);
    let asked = fake_gio(&mut app, 0, "");
    let worker = card_with(&mut app, pixel(None));
    let now = Instant::now();

    app.mount_action(now);
    let card = app.mounts.as_ref().expect("the card stays up");
    assert_eq!(
        card.busy,
        Some((ROOT.to_string(), "mounting…")),
        "the row says what is happening to it"
    );
    // One at a time: a second `Enter` while it is out asks nothing more.
    app.mount_action(now);
    land(&mut app);

    assert_eq!(
        *asked.lock().expect("the log"),
        vec![vec!["mount".to_string(), ROOT.to_string()]]
    );
    let card = app.mounts.as_ref().expect("the card stays up");
    assert!(card.busy.is_none() && card.failed.is_none());
    assert_eq!(toast(&app).as_deref(), Some("Mounted Pixel 10a"));
    assert!(
        matches!(worker.try_recv(), Ok(Request::List)),
        "the card lists again to show it mounted"
    );
    assert_eq!(app.cwd(), app.files, "a mount goes nowhere by itself");
}

/// A phone that is locked, or charging rather than in File transfer, is
/// told to be unlocked — in a toast and on its row — rather than shown
/// libmtp's words; a failure of any other kind is shown in gio's.
#[test]
fn a_locked_phone_is_asked_to_be_unlocked() {
    let mut app = Fixture::new("phone-locked", &["a.txt"]);
    fake_gio(
        &mut app,
        2,
        "gio: mtp://Google_Pixel_10a_4B021FDAQ00123/: Unable to open MTP device “003,012”\n",
    );
    let worker = card_with(&mut app, pixel(None));
    app.mount_action(Instant::now());
    land(&mut app);

    let unlock = "Unlock the phone and choose File transfer, then try again";
    assert_eq!(toast(&app).as_deref(), Some(unlock));
    let card = app.mounts.as_ref().expect("the card stays up");
    assert!(card.busy.is_none());
    assert_eq!(
        card.failed,
        Some((ROOT.to_string(), unlock.to_string())),
        "the row says it in place of its detail"
    );
    assert!(worker.try_recv().is_err(), "nothing to list again");

    // gio's own words for anything else.
    fake_gio(&mut app, 2, "gio: mtp://x/: Operation not supported\n");
    app.mount_action(Instant::now());
    land(&mut app);
    assert_eq!(
        toast(&app).as_deref(),
        Some("gio: mtp://x/: Operation not supported")
    );
}

/// `Enter` on a mounted phone goes into the directory gvfs-fuse shows it as,
/// and the card comes down; `m` on one says it is mounted already.
#[test]
fn enter_on_a_mounted_phone_goes_into_it() {
    let mut app = Fixture::new("phone-enter", &["a.txt"]);
    let asked = fake_gio(&mut app, 0, "");
    let phone = gvfs(&mut app).join(DIR);
    std::fs::create_dir_all(phone.join("DCIM")).expect("make the phone");
    card_with(&mut app, pixel(Some(phone.clone())));
    let now = Instant::now();

    app.mount_selected(now);
    assert_eq!(toast(&app).as_deref(), Some("Pixel 10a is already mounted"));
    app.mount_action(now);
    assert!(app.mounts.is_none(), "the card came down");
    assert_eq!(app.cwd(), phone);
    assert!(
        asked.lock().expect("the log").is_empty(),
        "gio was not asked"
    );
}

/// `u` on a mounted phone is `gio mount -u <root>` on the worker, the row
/// busy until it answers; `e` is the same, a phone having no drive to eject;
/// on one not mounted, both say so.
#[test]
fn u_and_e_on_a_phone_unmount_it() {
    let mut app = Fixture::new("phone-unmount", &["a.txt"]);
    let mounted = Some(PathBuf::from("/run/user/1000/gvfs").join(DIR));
    for eject in [false, true] {
        let worker = card_with(&mut app, pixel(mounted.clone()));
        if eject {
            app.eject_selected(Instant::now());
        } else {
            app.unmount_selected(Instant::now());
        }
        assert!(
            matches!(worker.try_recv(), Ok(Request::GioUnmount(root)) if root == ROOT),
            "eject: {eject}"
        );
        assert_eq!(
            app.mounts.as_ref().and_then(|card| card.busy.clone()),
            Some((ROOT.to_string(), "unmounting…"))
        );
    }
    let worker = card_with(&mut app, pixel(None));
    app.unmount_selected(Instant::now());
    assert_eq!(toast(&app).as_deref(), Some("Pixel 10a is not mounted"));
    assert!(worker.try_recv().is_err());
}

fn event(change: Change, name: &str, protocol: Option<Protocol>, root: Option<&str>) -> Event {
    Event {
        change,
        name: name.to_string(),
        protocol,
        root: root.map(str::to_string),
    }
}

/// A phone plugged in is said in a toast that names the card's key; a disk's
/// volume is not a phone and says nothing here (the card lists it). Any
/// event lists the card again when it is up.
#[test]
fn a_phone_plugged_in_is_announced_with_the_key_that_opens_it() {
    let mut app = Fixture::new("phone-plugged", &["a.txt"]);
    let now = Instant::now();
    app.gio_heard(
        vec![event(Change::VolumeAdded, "Card Reader", None, None)],
        now,
    );
    assert_eq!(toast(&app), None);
    assert!(app.udisks.is_none(), "no card, so nothing listed");

    app.gio_heard(
        vec![event(
            Change::VolumeAdded,
            "Pixel 10a",
            Some(Protocol::Mtp),
            Some(ROOT),
        )],
        now,
    );
    assert_eq!(
        toast(&app).as_deref(),
        Some("Pixel 10a plugged in · M to open it")
    );

    let worker = card_with(&mut app, pixel(None));
    app.gio_heard(vec![event(Change::Other, "Pixel 10a", None, None)], now);
    assert!(matches!(worker.try_recv(), Ok(Request::List)));
}

/// A phone pulled out from under a tab takes the tab up to the nearest
/// folder still there — every tab inside it, the one on screen and the one
/// behind it — as a folder deleted under a tab does; a tab elsewhere stays.
#[test]
fn a_phone_pulled_out_takes_its_tabs_out_with_it() {
    let mut app = Fixture::new("phone-pulled", &["a.txt"]);
    let gvfs = gvfs(&mut app);
    let camera = gvfs.join(DIR).join("DCIM").join("Camera");
    std::fs::create_dir_all(&camera).expect("make the phone");
    let now = Instant::now();
    let files = app.files.clone();

    // Tab 1 on the phone, tab 2 at home in the fixture, tab 3 on the phone
    // and the one on screen.
    app.navigate(camera.clone(), now);
    app.run(Command::TabCreate, 10, now);
    app.navigate(files.clone(), now);
    app.run(Command::TabCreate, 10, now);
    app.navigate(camera.clone(), now);
    assert_eq!(app.cwd(), camera);

    std::fs::remove_dir_all(gvfs.join(DIR)).expect("pull the phone");
    app.gio_heard(
        vec![event(
            Change::VolumeRemoved,
            "Pixel 10a",
            Some(Protocol::Mtp),
            Some(ROOT),
        )],
        now,
    );
    let cwds: Vec<PathBuf> = app
        .tabs
        .iter()
        .map(|tab| tab.cwd.path().to_path_buf())
        .collect();
    assert_eq!(cwds, vec![gvfs.clone(), files, gvfs]);
}

/// Inside a phone the size column walks nothing — declined by the path,
/// before anything asks the device — and the grid asks for no thumbnail;
/// beside it, both go on as ever.
#[test]
fn a_phone_is_left_to_its_icons_and_its_sizes_unwalked() {
    let mut app = Fixture::with_folders("phone-gate", &["a.txt"], &["sub"]);
    let phone = gvfs(&mut app).join(DIR);
    std::fs::create_dir_all(phone.join("DCIM")).expect("make the phone");
    std::fs::write(phone.join("IMG_0001.txt"), b"x").expect("a photo");
    let now = Instant::now();
    let tab = app.tabs.active_index();
    let content = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let metrics = crate::grid::metrics(content.width());

    go(&mut app, &phone);
    assert!(app.begin_folder_sizes(phone.clone(), now));
    assert!(
        app.folders.is_about(&phone, tab),
        "declined, so not asked again"
    );
    assert!(app.du.is_none(), "no walk was started");
    assert!(
        app.tile_wants(content, &metrics, 0.0).is_empty(),
        "no thumbnail is asked of a phone"
    );

    let files = app.files.clone();
    go(&mut app, &files);
    assert!(!app.tile_wants(content, &metrics, 0.0).is_empty());
    app.begin_folder_sizes(files, now);
    assert!(app.du.is_some(), "a folder on this disk is walked");
}
