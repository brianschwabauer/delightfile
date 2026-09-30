//! Cloud remotes through the app: `rclone://` from every door, the mount
//! card's Network rows, and the refusals that name what a remote cannot do.
//!
//! No daemon runs. Every service here points its program at a path that does
//! not exist, so a listing that does start fails on its worker and changes
//! nothing these tests look at — and nothing reads this machine's `vfs.toml`
//! or `rclone.conf`, because each test hands the app its vfs. The socket a
//! daemon would have had goes in the fixture's sandbox, so even the attempt
//! never creates a directory in the user's runtime directory.

use df_core::ops::paste::{Clipboard, PasteMode};
use df_core::vfs::{Service, Vfs, VfsConfig, VfsPath};

use super::*;
use crate::mounts::{Cloud, Item};

/// Where no rclone is.
const NO_RCLONE: &str = "/nonexistent/df-test-rclone";

/// An rclone service that can never start a daemon.
fn cloud(name: &str, provider: Option<&str>) -> Service {
    let mut service = Service::rclone(name, name);
    service.provider = provider.map(str::to_string);
    service.program = Some((PathBuf::from(NO_RCLONE), Vec::new()));
    service
}

/// An sftp service that can never start an ssh.
fn server(name: &str) -> Service {
    Service::direct(name, NO_RCLONE, Vec::new())
}

fn with_services(app: &mut Fixture, services: Vec<Service>) {
    // Beside the fixture's files, inside its sandbox, which goes when it does.
    let sockets = app.files.with_file_name("run");
    let mut config = VfsConfig::default();
    for mut service in services {
        service.socket_dir = Some(sockets.clone());
        config.insert(service);
    }
    app.vfs = Some(Arc::new(Vfs::with_config(
        config,
        Vec::new(),
        Arc::new(|| {}),
    )));
}

fn toast(app: &App) -> Option<String> {
    app.toasts.current().map(|toast| toast.message.clone())
}

fn remote_at(app: &App) -> Option<VfsPath> {
    app.tab().remote.as_ref().map(|session| session.at.clone())
}

/// `Go to:` takes a URL of either scheme to the place it names, rather than
/// joining it onto the folder on screen as a relative name.
#[test]
fn go_to_takes_a_cloud_url_and_a_server_one() {
    let mut app = Fixture::new("cloud-goto", &["a.txt"]);
    with_services(&mut app, vec![server("box"), cloud("r2", Some("s3"))]);
    let now = Instant::now();

    assert_eq!(app.go_to_path("rclone://r2/bucket/photos", now), Ok(()));
    assert_eq!(
        remote_at(&app),
        Some(VfsPath::rclone("r2", "bucket/photos"))
    );
    assert_eq!(
        app.cwd(),
        PathBuf::from("rclone://r2/bucket/photos"),
        "the pane is on the remote, by its URL"
    );

    // Pasted out of a terminal, with the whitespace that brings.
    assert_eq!(app.go_to_path("  sftp://box/srv\n", now), Ok(()));
    assert_eq!(remote_at(&app), Some(VfsPath::new("box", "/srv")));
}

/// The config decides which backend a name is: `sftp://r2` for an rclone
/// remote lands on the remote, and the pane's rows and crumbs are `rclone://`.
#[test]
fn a_server_url_for_a_cloud_remote_lands_on_the_remote() {
    let mut app = Fixture::new("cloud-scheme", &["a.txt"]);
    with_services(&mut app, vec![cloud("r2", None)]);
    app.navigate(PathBuf::from("sftp://r2/bucket"), Instant::now());
    assert_eq!(remote_at(&app), Some(VfsPath::rclone("r2", "bucket")));
    assert_eq!(app.cwd(), PathBuf::from("rclone://r2/bucket"));
}

/// A remote folder is entered and left by its URL, built by the vfs's own
/// `join` and `parent`, never by a local path's: on Windows a `PathBuf` join
/// would write `sftp://box/srv\www`, which no service parses (W4.14). The
/// rows come from the session's cache, as a step back does, so no server is
/// needed.
#[test]
fn a_remote_folder_is_entered_and_left_by_its_url() {
    use df_core::vfs::{stat_entry, Attrs};
    let mut app = Fixture::new("remote-rows", &["a.txt"]);
    with_services(&mut app, vec![server("box")]);
    let now = Instant::now();
    let srv = VfsPath::new("box", "/srv");
    app.navigate(crate::remote::display(&srv), now);
    let folder = Attrs {
        permissions: Some(0o040_755),
        ..Attrs::default()
    };
    let file = Attrs {
        permissions: Some(0o100_644),
        size: Some(3),
        ..Attrs::default()
    };
    let rows = vec![
        stat_entry(&srv.join("www"), folder),
        stat_entry(&srv.join("notes.txt"), file),
    ];
    app.tabs
        .active_mut()
        .remote
        .as_mut()
        .expect("a session")
        .store(&srv, rows);
    // The same place again: a step on one service keeps its cache.
    app.navigate(crate::remote::display(&srv), now);
    let www = app
        .tab()
        .cwd
        .dir
        .position_of("www")
        .expect("the folder's row");
    app.dir().set_cursor(www);

    app.run(Command::EnterDirectory, 10, now);
    assert_eq!(remote_at(&app), Some(srv.join("www")));
    let url = app.cwd().to_string_lossy().into_owned();
    assert_eq!(url, "sftp://box/srv/www");
    assert!(!url.contains('\\'), "{url}");

    app.run(Command::Leave, 10, now);
    assert_eq!(remote_at(&app), Some(srv.clone()));
    assert_eq!(app.cwd(), PathBuf::from("sftp://box/srv"));
    for entry in app.tab().cwd.dir.entries() {
        let path = entry.path.to_string_lossy();
        assert!(
            path.starts_with("sftp://box/srv/") && !path.contains('\\'),
            "{path}"
        );
    }
}

#[test]
fn an_unknown_service_names_both_files() {
    let mut app = Fixture::new("cloud-unknown", &["a.txt"]);
    let now = Instant::now();
    with_services(&mut app, Vec::new());
    app.navigate(PathBuf::from("rclone://nope"), now);
    assert_eq!(
        toast(&app).as_deref(),
        Some("No service called nope — vfs.toml and rclone.conf define no services")
    );
    assert!(app.tab().remote.is_none());

    with_services(&mut app, vec![server("box"), cloud("r2", None)]);
    app.navigate(PathBuf::from("rclone://nope/x"), now);
    assert_eq!(
        toast(&app).as_deref(),
        Some("No service called nope — vfs.toml and rclone.conf have box, r2")
    );
    assert!(app.tab().remote.is_none());
}

/// `M`'s Network section lists the rclone services after the shares; `m`,
/// `u` and `e` do nothing on one, and `Enter` goes there.
#[test]
fn the_mount_card_lists_cloud_remotes_and_enter_goes_there() {
    let mut app = Fixture::new("cloud-card", &["a.txt"]);
    with_services(
        &mut app,
        vec![
            server("box"),
            cloud("r2", Some("s3")),
            cloud("gdrive", None),
        ],
    );
    let now = Instant::now();

    let mut card = app.mount_card();
    card.set_clouds(app.cloud_rows());
    assert_eq!(
        card.clouds,
        [
            Cloud {
                name: "r2".into(),
                provider: "s3".into(),
            },
            Cloud {
                name: "gdrive".into(),
                provider: "rclone".into(),
            },
        ],
        "rclone services only, in the vfs's order, `rclone` where the type is unknown"
    );
    card.select(Item::Cloud(0));
    app.mounts = Some(card);

    for verb in [
        App::mount_selected,
        App::unmount_selected,
        App::eject_selected,
    ] {
        verb(&mut app, now);
        let card = app.mounts.as_ref().expect("the card stays up");
        assert_eq!(card.selected(), Some(Item::Cloud(0)));
        assert!(card.busy.is_none(), "nothing was asked of udisks2 or gvfs");
        assert!(remote_at(&app).is_none());
        assert_eq!(toast(&app), None, "and nothing was said about it");
    }

    app.mount_action(now);
    assert!(app.mounts.is_none(), "the card came down");
    assert_eq!(remote_at(&app), Some(VfsPath::rclone("r2", "")));
}

/// A sync with a server is rsync over ssh; a cloud remote is refused by name
/// before anything else is asked.
#[test]
fn a_sync_with_a_cloud_remote_is_refused_by_name() {
    let mut app = Fixture::new("cloud-sync", &["a.txt"]);
    with_services(&mut app, vec![cloud("r2", Some("s3"))]);
    app.clipboard = Clipboard {
        mode: PasteMode::Copy,
        paths: vec![PathBuf::from("rclone://r2/photos")],
    };
    app.run(Command::PasteSync, 10, Instant::now());
    assert_eq!(
        toast(&app).as_deref(),
        Some("Sync needs ssh, and r2 is an rclone remote")
    );
    assert!(app.dialog.is_none(), "no card for a sync that cannot run");

    // The same for an `sftp://` URL naming the remote: the config says what
    // `r2` is, whatever the scheme says. That refusal is past the check for
    // rsync, which a machine without it answers first.
    if df_core::sync::rsync::available() {
        app.toasts.clear();
        app.clipboard = Clipboard {
            mode: PasteMode::Copy,
            paths: vec![PathBuf::from("sftp://r2/photos")],
        };
        app.run(Command::PasteSync, 10, Instant::now());
        assert_eq!(
            toast(&app).as_deref(),
            Some("Sync needs ssh, and r2 is an rclone remote")
        );
        assert!(app.dialog.is_none());
    }
}

/// `C` on a cloud remote is refused before a card goes up, in the same
/// words wherever it is asked from, and its menu row is grey.
#[test]
fn permissions_are_refused_on_cloud_storage() {
    let mut app = Fixture::new("cloud-permissions", &["a.txt"]);
    with_services(&mut app, vec![cloud("r2", Some("s3"))]);
    let now = Instant::now();
    app.navigate(PathBuf::from("rclone://r2/bucket"), now);
    assert_eq!(
        app.refusal(Command::Permissions),
        Some("Permissions can't be set on cloud storage")
    );
    app.run(Command::Permissions, 10, now);
    assert!(app.dialog.is_none());
    assert_eq!(
        toast(&app).as_deref(),
        Some("Permissions can't be set on cloud storage")
    );
    app.run(Command::AppMenu, 10, now);
    let row = app
        .menu
        .as_ref()
        .expect("up")
        .items
        .iter()
        .flat_map(|item| item.submenu.iter().flatten())
        .find(|item| item.label == "Permissions…")
        .map(|item| item.enabled);
    assert_eq!(row, Some(false));
}

/// On a server the card goes up over the rows the listing has, one level
/// only — a folder there has no checkbox — and says there is no undo before
/// the key lands. Its Apply is a remote job named for the `w` panel, and
/// nothing is journalled.
#[test]
fn permissions_on_a_server_are_one_level_and_journal_nothing() {
    use df_core::fs::{Entry, Kind};
    let mut app = Fixture::new("cloud-permissions-sftp", &["a.txt"]);
    with_services(&mut app, vec![server("box")]);
    let now = Instant::now();
    let at = VfsPath::new("box", "/srv");
    let row = |name: &str, kind: Kind, mode: u32| {
        let mime = if kind == Kind::Dir {
            "inode/directory"
        } else {
            "text/plain"
        };
        Entry {
            name: name.to_string(),
            path: PathBuf::from(at.join(name).to_url()),
            kind,
            len: 1,
            mtime: None,
            btime: None,
            mode,
            uid: 1000,
            gid: 100,
            is_hidden: false,
            mime,
            file_kind: df_core::fs::classify(kind, name, mime, mode),
            tags: Vec::new(),
        }
    };
    let mut session = crate::remote::Session::new(at.clone(), app.files.clone());
    session.store(
        &at,
        vec![
            row("notes.txt", Kind::File, 0o100644),
            row("www", Kind::Dir, 0o40755),
        ],
    );
    let (mgr, sort) = (app.mgr.clone(), app.sort());
    let _cached = app.tabs.active_mut().show_remote(session, &mgr, sort, now);
    assert_eq!(
        app.tab().cwd.dir.len(),
        2,
        "the rows did not come from the cache"
    );
    assert_eq!(app.refusal(Command::Permissions), None);

    app.dir().select_all();
    app.run(Command::Permissions, 10, now);
    let Some(Dialog::Permissions(card)) = &app.dialog else {
        panic!("no card over the server's rows");
    };
    assert_eq!(card.host.as_deref(), Some("box"));
    assert!(!card.has_folder(), "a folder on a server has no checkbox");
    assert_eq!(
        card.owner_line(),
        "1000 · 100",
        "a server's owner is its number"
    );
    assert!(card
        .status()
        .is_some_and(|(line, _)| line.contains("no undo on a server")));

    let journal = app.journal.len();
    for digit in ['7', '0', '0'] {
        app.route_chord(Chord::plain(Key::Char(digit)), 10, now);
    }
    app.route_chord(Chord::plain(Key::Enter), 10, now);
    assert!(app.dialog.is_none());
    assert!(app
        .engine
        .snapshot()
        .iter()
        .any(|task| task.name == "Set permissions on 2 remote items"));
    assert_eq!(
        app.journal.len(),
        journal,
        "a server's change was journalled"
    );
}

/// Remote to remote is refused in words that fit a cloud at either end —
/// including the case of one of each.
#[test]
fn a_paste_between_a_server_and_a_cloud_is_refused_in_neutral_words() {
    let mut app = Fixture::new("cloud-across", &["a.txt"]);
    with_services(&mut app, vec![server("box"), cloud("r2", None)]);
    let now = Instant::now();
    app.navigate(PathBuf::from("sftp://box/srv"), now);
    app.clipboard = Clipboard {
        mode: PasteMode::Copy,
        paths: vec![PathBuf::from("rclone://r2/photo.jpg")],
    };
    app.run(Command::Paste, 10, now);
    assert_eq!(
        toast(&app).as_deref(),
        Some("Remote to remote would come through this machine — download it first")
    );
}
