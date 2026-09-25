//! Proving the rclone backend without a network, and the config that finds it.
//!
//! Two tiers, like `super::tests`:
//!
//! 1. **Config and addressing** — `rclone.conf` discovery, `type = "rclone"`
//!    in `vfs.toml`, and the two URL schemes through [`VfsPath`]. Pure; always
//!    run; never read the user's own files.
//! 2. **The hermetic integration tests** — a real `rclone rcd`, spawned
//!    exactly as the app spawns it, pointed at a *local directory* as its
//!    remote. rclone accepts a path anywhere it accepts a remote, so every
//!    call the backend makes — list, stat, mkdir, move, the async copy jobs
//!    and their stats, `job/stop` — goes over the real socket to the real
//!    daemon, and only the provider at the far end is the local disk rather
//!    than somebody's cloud. Each daemon is given a scratch `--config`, so the
//!    user's `rclone.conf` is never opened. Skipped with a printed reason where
//!    `rclone` is not on `$PATH`.

#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::rclone::Daemon;
use super::tests::{collect_listing, CountingSink, TempDir};
use super::{
    Op, Outcome, Service, ServiceKind, StatusCode, Vfs, VfsConfig, VfsError, VfsPath, VfsUpdate,
};
use crate::fs::{no_notifier, Kind};
use crate::tasks::{ProgressSink, TaskCtx, TaskFlags};

/// Generous enough for a loaded machine, short enough that a hang fails.
const T: Duration = Duration::from_secs(10);

// ── rclone.conf discovery ───────────────────────────────────────────────────

const VFS_TOML: &str = r#"
[services.showandtour1]
type = "sftp"
host = "showandtour1"

[services.dropbox]
host = "a-machine-called-dropbox"
"#;

/// The shape of a real `rclone.conf`: sections, a `type` each, and keys this
/// parser must never keep. The secrets are fake; the point is that they are
/// not read into anything.
const RCLONE_CONF: &str = r#"
# rclone.conf
[r2]
type = s3
provider = Cloudflare
access_key_id = not-a-real-key
secret_access_key = not-a-real-secret

; a comment the other way
[gdrive]
type    =   drive
token = {"access_token":"x","expiry":"2026-01-01T00:00:00Z"}

this line is not INI at all

[dropbox]
type = dropbox

[no-type]
"#;

#[test]
fn rclone_remotes_follow_the_vfs_toml_services_and_never_displace_them() {
    let dir = TempDir::new("rclone-conf");
    let toml = dir.file("vfs.toml", VFS_TOML.as_bytes());
    let conf = dir.file("rclone.conf", RCLONE_CONF.as_bytes());
    let (config, warnings) = VfsConfig::load_with_rclone(&[toml], Some(&conf));
    assert!(warnings.is_empty(), "{warnings:?}");

    let names: Vec<&str> = config.services.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["showandtour1", "dropbox", "r2", "gdrive", "no-type"],
        "vfs.toml's services first, in file order, so `g 1`/`g 2` never shift"
    );

    let r2 = config.service("r2").unwrap();
    assert_eq!(r2.kind, ServiceKind::Rclone);
    assert_eq!(r2.remote.as_deref(), Some("r2"));
    assert_eq!(r2.provider.as_deref(), Some("s3"));
    assert_eq!(r2.host, "", "a cloud remote has no host");
    assert_eq!(r2.rclone_fs(), "r2:");
    assert_eq!(
        config.service("gdrive").unwrap().provider.as_deref(),
        Some("drive")
    );
    assert_eq!(config.service("no-type").unwrap().provider, None);

    // The name vfs.toml already has is vfs.toml's, whole.
    let dropbox = config.service("dropbox").unwrap();
    assert_eq!(dropbox.kind, ServiceKind::Sftp);
    assert_eq!(dropbox.host, "a-machine-called-dropbox");
    assert_eq!(dropbox.provider, None);
}

#[test]
fn an_encrypted_rclone_conf_warns_once_and_discovers_nothing() {
    for text in [
        "# Encrypted rclone configuration File\n\nRCLONE_ENCRYPT_V0:\nZm9vYmFy\n",
        "RCLONE_ENCRYPT_V0:\nZm9vYmFy\n",
    ] {
        let (services, warnings) = super::parse_rclone_conf(text, Path::new("rclone.conf"));
        assert!(services.is_empty(), "{services:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let said = warnings[0].to_string();
        assert!(said.contains("encrypted"), "{said}");
        assert!(said.contains("vfs.toml"), "the way out is named: {said}");
    }
}

/// rclone's own search: `$RCLONE_CONFIG`, then the XDG file if it exists,
/// then the legacy `~/.rclone.conf` if *it* exists, then the XDG path.
#[test]
fn the_legacy_rclone_conf_is_the_last_place_looked() {
    use super::config::rclone_config_path_from as path_from;
    let home = TempDir::new("rclone-home");
    let config_home = home.path.join(".config");
    let xdg = config_home.join("rclone").join("rclone.conf");
    let legacy = home.path.join(".rclone.conf");
    let find = || path_from(None, Some(config_home.clone()), Some(home.path.clone()));

    // Neither: the XDG path, where finding nothing is silence.
    assert_eq!(find(), Some(xdg.clone()));
    // Only the legacy file: that.
    std::fs::write(&legacy, "[old]\ntype = s3\n").unwrap();
    assert_eq!(find(), Some(legacy.clone()));
    // Both: the XDG file wins.
    std::fs::create_dir_all(xdg.parent().unwrap()).unwrap();
    std::fs::write(&xdg, "[new]\ntype = drive\n").unwrap();
    assert_eq!(find(), Some(xdg.clone()));
    // `$RCLONE_CONFIG` beats both, and an empty one is no setting at all.
    let explicit = home.path.join("elsewhere.conf");
    assert_eq!(
        path_from(
            Some(explicit.clone().into_os_string()),
            Some(config_home.clone()),
            Some(home.path.clone())
        ),
        Some(explicit)
    );
    assert_eq!(
        path_from(
            Some(std::ffi::OsString::new()),
            Some(config_home.clone()),
            Some(home.path.clone())
        ),
        Some(xdg)
    );
    // No home at all: nowhere to look.
    assert_eq!(path_from(None, None, None), None);
}

#[test]
fn a_missing_rclone_conf_is_silence() {
    let (config, warnings) =
        VfsConfig::load_with_rclone(&[], Some(Path::new("/nonexistent/rclone.conf")));
    assert!(config.services.is_empty());
    assert!(warnings.is_empty());
    let (config, warnings) = VfsConfig::load_with_rclone(&[], None);
    assert!(config.services.is_empty() && warnings.is_empty());
}

#[test]
fn type_rclone_in_vfs_toml_needs_no_host() {
    let text = r#"
[services.photos]
type = "rclone"
remote = "r2"
root = "photos-bucket"

[services.plain]
type = "rclone"

[services.moved]
type = "rclone"
path = "/inside/"
user = "brian"
port = 2222
"#;
    let (config, warnings) = VfsConfig::parse(text, Path::new("vfs.toml"));
    let photos = config.service("photos").unwrap();
    assert_eq!(photos.kind, ServiceKind::Rclone);
    assert_eq!(photos.host, "");
    assert_eq!(photos.remote.as_deref(), Some("r2"));
    assert_eq!(photos.rclone_fs(), "r2:photos-bucket");

    let plain = config.service("plain").unwrap();
    assert_eq!(
        plain.rclone_fs(),
        "plain:",
        "the remote defaults to the name"
    );

    // The ssh keys are named, not obeyed, and the service still loads.
    let moved = config.service("moved").unwrap();
    assert_eq!(moved.rclone_fs(), "moved:inside", "yazi's `path` is `root`");
    assert_eq!(moved.user, None);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let said = warnings[0].to_string();
    assert!(
        said.contains("`user`, `port`") && said.contains("ignored"),
        "{said}"
    );
}

// ── Addressing ──────────────────────────────────────────────────────────────

#[test]
fn both_url_schemes_round_trip_and_keep_their_kind() {
    let cases = [
        ("sftp://s/Work", ServiceKind::Sftp, "s", "/Work"),
        ("sftp://s", ServiceKind::Sftp, "s", ""),
        ("rclone://r2", ServiceKind::Rclone, "r2", ""),
        ("rclone://r2/", ServiceKind::Rclone, "r2", ""),
        (
            "rclone://r2/bucket/photos",
            ServiceKind::Rclone,
            "r2",
            "bucket/photos",
        ),
        (
            "rclone://r2/bucket/photos/",
            ServiceKind::Rclone,
            "r2",
            "bucket/photos",
        ),
        (
            "rclone://my remote/a b",
            ServiceKind::Rclone,
            "my remote",
            "a b",
        ),
    ];
    for (url, kind, service, path) in cases {
        let parsed = VfsPath::parse(url).unwrap();
        assert_eq!(parsed.kind, kind, "{url}");
        assert_eq!(parsed.service, service, "{url}");
        assert_eq!(parsed.path, path, "{url}");
        assert_eq!(
            VfsPath::parse(&parsed.to_url()),
            Some(parsed.clone()),
            "{url}"
        );
        assert!(crate::ops::is_url(Path::new(url)), "{url}");
    }
    assert_eq!(VfsPath::parse("rclone://"), None);
    assert_eq!(VfsPath::parse("rclone:/r2"), None);

    // A row joined down from the root and the URL typed for it are one value,
    // which is what lets the pane's cache and a typed `Go to:` agree.
    let root = VfsPath::rclone("r2", "");
    let deep = root.join("bucket").join("photos");
    assert_eq!(deep, VfsPath::parse("rclone://r2/bucket/photos").unwrap());
    assert_eq!(deep.to_url(), "rclone://r2/bucket/photos");
    assert_eq!(deep.parent().unwrap(), root.join("bucket"));
    assert_eq!(root.join("bucket").parent(), Some(root.clone()));
    assert_eq!(root.parent(), None);
    assert_eq!(deep.service_root(), root);
    assert_eq!(root.name(), "r2");

    // Same name, different scheme: different places.
    assert_ne!(VfsPath::new("s", "x"), VfsPath::rclone("s", "x"));
    let service = Service::rclone("r2", "r2");
    assert_eq!(
        VfsPath::for_service(&service, "/a/"),
        VfsPath::rclone("r2", "a")
    );
    let sftp = Service::new("s", "host");
    assert_eq!(VfsPath::for_service(&sftp, "/a"), VfsPath::new("s", "/a"));
}

#[test]
fn unsupported_is_a_sentence_and_not_a_hang_up() {
    let error = VfsError::Unsupported {
        service: "r2".into(),
        op: "chmod",
    };
    assert_eq!(
        error.to_string(),
        "r2: chmod is not something rclone can do"
    );
    assert!(!error.is_connection_fatal());
    let spawn = VfsError::Spawn {
        service: "r2".into(),
        program: "rclone".into(),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };
    assert!(
        spawn.to_string().starts_with("r2: could not start rclone:"),
        "{spawn}"
    );
}

// ── The hermetic integration tests ──────────────────────────────────────────

/// `rclone` on `$PATH`, the way the app will find it.
pub(super) fn find_rclone() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("rclone"))
        .find(|candidate| candidate.is_file())
}

/// A service name no other test in this process is using, because the socket
/// name carries it and the teardown check looks for it.
pub(super) fn unique_name(tag: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    format!("{tag}{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// An rclone service whose remote is `remote`'s directory, whose daemon reads
/// an empty config in `scratch` rather than the user's, and whose socket goes
/// in `scratch` too — never in the user's runtime directory.
fn local_service(name: &str, remote: &TempDir, scratch: &TempDir, rclone: &Path) -> Service {
    scratch_service(
        Service::rclone(name, remote.path.display().to_string()),
        scratch,
        rclone,
    )
}

/// `service`, made hermetic: `rclone` with a scratch `--config`, and its socket
/// in the scratch directory.
pub(super) fn scratch_service(mut service: Service, scratch: &TempDir, rclone: &Path) -> Service {
    let conf = scratch.file("rclone.conf", b"");
    service.program = Some((
        rclone.to_path_buf(),
        vec!["--config".into(), conf.display().to_string()],
    ));
    service.socket_dir = Some(scratch.path.join("run"));
    service
}

fn vfs_over(service: Service) -> Vfs {
    let mut config = VfsConfig::default();
    config.insert(service);
    Vfs::with_config(config, Vec::new(), no_notifier())
}

/// The sockets in `scratch`'s socket directory (see [`scratch_service`]).
pub(super) fn sockets_in(scratch: &TempDir) -> Vec<PathBuf> {
    std::fs::read_dir(scratch.path.join("run"))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "sock"))
        .collect()
}

/// Whether process `pid` has exited: gone from `/proc`, or a zombie (`Z`)
/// waiting for a parent that will never reap it.
fn has_exited(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    // `pid (comm) S …` — the state follows the last `)`, since `comm` may
    // itself contain one.
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next());
    matches!(state, Some("Z" | "X" | "x"))
}

/// Anything rclone left half-written in `dir`.
pub(super) fn partials_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".partial"))
        .collect()
}

/// Every verb the app uses, against a real daemon over a local directory.
#[test]
fn rclone_round_trip_everything() {
    let Some(rclone) = find_rclone() else {
        eprintln!("skipping rclone_round_trip_everything: no rclone on $PATH");
        return;
    };
    let remote = TempDir::new("rclone-remote");
    let scratch = TempDir::new("rclone-scratch");
    let local = TempDir::new("rclone-local");

    let big: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
    remote.file("big.bin", &big);
    let hello = remote.file("hello.txt", b"hello over rclone\n");
    // A known date, so the listing's date is checked rather than trusted.
    let stamp = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::File::options()
        .write(true)
        .open(&hello)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    std::fs::create_dir(remote.path.join("sub")).unwrap();
    std::fs::write(remote.path.join("sub/inner.txt"), b"inner").unwrap();
    std::fs::create_dir(remote.path.join("empty")).unwrap();

    let name = unique_name("rt");
    let vfs = vfs_over(local_service(&name, &remote, &scratch, &rclone));
    let ctx = TaskCtx::detached();
    let root = VfsPath::rclone(&name, "");
    let url = |path: &str| PathBuf::from(format!("rclone://{name}/{path}"));

    // ── Listing ─────────────────────────────────────────────────────────
    let token = vfs.scan(root.clone());
    let updates = collect_listing(&vfs, token);
    assert!(
        matches!(updates.first(), Some(VfsUpdate::Started { .. })),
        "Started comes before any entries: {updates:?}"
    );
    let mut entries = Vec::new();
    let mut total = 0;
    for update in &updates {
        match update {
            VfsUpdate::Batch { entries: batch, .. } => entries.extend(batch.iter().cloned()),
            VfsUpdate::Done { total: t, .. } => total = *t,
            VfsUpdate::Failed { error, .. } => panic!("listing failed: {error}"),
            VfsUpdate::Started { .. } => {}
        }
    }
    assert_eq!(total, entries.len());
    let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["big.bin", "empty", "hello.txt", "sub"]);
    let by_name = |wanted: &str| entries.iter().find(|e| e.name == wanted).unwrap();
    assert_eq!(by_name("sub").kind, Kind::Dir);
    assert_eq!(
        by_name("sub").len,
        0,
        "a folder's size is the du walk's job"
    );
    assert_eq!(by_name("empty").kind, Kind::Dir);
    assert_eq!(by_name("hello.txt").kind, Kind::File);
    assert_eq!(by_name("hello.txt").len, 18);
    assert_eq!(by_name("big.bin").len, big.len() as u64);
    assert_eq!(
        by_name("hello.txt").mtime,
        Some(stamp),
        "the date came off the wire"
    );
    assert_eq!(by_name("hello.txt").mode, 0o100_644);
    assert_eq!(by_name("sub").mode, 0o040_755);
    assert_eq!(by_name("hello.txt").mime, "text/plain");
    assert_eq!(
        by_name("hello.txt").path,
        url("hello.txt"),
        "rows carry their rclone:// URL, which is what the panes navigate by"
    );

    let token = vfs.scan(root.join("sub"));
    let inner: Vec<_> = collect_listing(&vfs, token)
        .into_iter()
        .filter_map(|u| match u {
            VfsUpdate::Batch { entries, .. } => Some(entries),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(inner.len(), 1);
    assert_eq!(inner[0].path, url("sub/inner.txt"));

    // An empty folder lists as Started then Done, with nothing between.
    let token = vfs.scan(root.join("empty"));
    let updates = collect_listing(&vfs, token);
    assert!(matches!(updates.first(), Some(VfsUpdate::Started { .. })));
    assert!(matches!(
        updates.last(),
        Some(VfsUpdate::Done { total: 0, .. })
    ));

    // A folder that is not there fails the listing with rclone's words, and
    // the daemon survives it.
    let token = vfs.scan(root.join("nope"));
    match collect_listing(&vfs, token).pop() {
        Some(VfsUpdate::Failed { error, .. }) => {
            match &error {
                VfsError::Status { status, path } => {
                    assert_eq!(status.code, StatusCode::NoSuchFile, "{error}");
                    assert_eq!(path, &format!("rclone://{name}/nope"));
                }
                other => panic!("expected a status, got {other}"),
            }
            assert!(!error.is_connection_fatal());
        }
        other => panic!("expected Failed, got {other:?}"),
    }

    // ── Stat ────────────────────────────────────────────────────────────
    let attrs = vfs.stat(&root.join("big.bin"), true, &ctx).unwrap();
    assert_eq!(attrs.size, Some(big.len() as u64));
    assert!(vfs.stat(&root.join("sub"), false, &ctx).unwrap().is_dir());
    assert!(
        vfs.stat(&root, false, &ctx).unwrap().is_dir(),
        "the root is a folder"
    );
    let error = vfs
        .stat(&root.join("does-not-exist"), true, &ctx)
        .expect_err("nothing is there");
    match &error {
        VfsError::Status { path, status } => {
            assert_eq!(status.code, StatusCode::NoSuchFile);
            assert_eq!(path, &format!("rclone://{name}/does-not-exist"));
        }
        other => panic!("expected a status, got {other}"),
    }
    assert!(vfs.exists(&root.join("big.bin"), &ctx).unwrap());
    assert!(!vfs.exists(&root.join("does-not-exist"), &ctx).unwrap());

    // ── Download, as a job, with progress ───────────────────────────────
    let sink = Arc::new(CountingSink::default());
    let progress = TaskCtx::with_sink(
        Arc::new(TaskFlags::new()),
        Arc::clone(&sink) as Arc<dyn ProgressSink>,
    );
    let destination = local.path.join("big.bin");
    let n = vfs
        .download(&root.join("big.bin"), &destination, &progress)
        .unwrap();
    assert_eq!(n, big.len() as u64);
    assert_eq!(std::fs::read(&destination).unwrap(), big, "byte-identical");
    assert_eq!(sink.total.load(Ordering::SeqCst), big.len() as u64);
    assert_eq!(
        sink.advanced.load(Ordering::SeqCst),
        big.len() as u64,
        "every byte reported, none twice"
    );
    assert!(partials_in(&local.path).is_empty());

    let opened = vfs.download_to_temp(&root.join("hello.txt"), &ctx).unwrap();
    assert_eq!(std::fs::read(&opened).unwrap(), b"hello over rclone\n");
    assert!(
        opened.to_string_lossy().ends_with("hello.txt"),
        "openers sniff extensions, so the name survives: {opened:?}"
    );
    let _ = std::fs::remove_file(&opened);

    let error = vfs
        .download(&root.join("sub"), &local.path.join("sub"), &ctx)
        .expect_err("a folder is not a file");
    assert!(error.to_string().contains("folder"), "{error}");
    assert!(!local.path.join("sub").exists());

    // ── Upload, and upload over ─────────────────────────────────────────
    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 199) as u8).collect();
    let source = local.file("upload-me.bin", &payload);
    let sent = vfs
        .upload(&source, &root.join("uploaded.bin"), &ctx)
        .unwrap();
    assert_eq!(sent, payload.len() as u64);
    assert_eq!(
        std::fs::read(remote.path.join("uploaded.bin")).unwrap(),
        payload
    );
    let smaller = local.file("smaller.bin", b"tiny");
    vfs.upload(&smaller, &root.join("uploaded.bin"), &ctx)
        .unwrap();
    assert_eq!(
        std::fs::read(remote.path.join("uploaded.bin")).unwrap(),
        b"tiny",
        "an upload over a file replaces it"
    );

    // ── The upload that refuses to destroy ──────────────────────────────
    assert_eq!(
        vfs.unique_name(&root.join("nothing-here.bin"), &ctx)
            .unwrap(),
        root.join("nothing-here.bin")
    );
    let (sent, landed) = vfs
        .upload_new(&source, &root.join("uploaded.bin"), &ctx)
        .unwrap();
    assert_eq!(sent, payload.len() as u64);
    assert_eq!(landed, root.join("uploaded_1.bin"));
    assert_eq!(
        std::fs::read(remote.path.join("uploaded.bin")).unwrap(),
        b"tiny"
    );
    assert_eq!(
        std::fs::read(remote.path.join("uploaded_1.bin")).unwrap(),
        payload
    );
    let (_, again) = vfs
        .upload_new(&source, &root.join("uploaded.bin"), &ctx)
        .unwrap();
    assert_eq!(again, root.join("uploaded_2.bin"));
    vfs.mkdir(&root.join("blocked.bin"), &ctx).unwrap();
    let (_, beside) = vfs
        .upload_new(&source, &root.join("blocked.bin"), &ctx)
        .unwrap();
    assert_eq!(
        beside,
        root.join("blocked_1.bin"),
        "a folder in the way is taken too"
    );
    assert!(remote.path.join("blocked.bin").is_dir());
    vfs.rmdir(&root.join("blocked.bin"), &ctx).unwrap();

    // A stat as the row a conflict card draws.
    let entry = super::stat_entry(
        &root.join("uploaded.bin"),
        vfs.stat(&root.join("uploaded.bin"), false, &ctx).unwrap(),
    );
    assert_eq!(entry.name, "uploaded.bin");
    assert_eq!(entry.len, 4);
    assert_eq!(entry.kind, Kind::File);
    assert_eq!(entry.path, url("uploaded.bin"));
    assert!(entry.mtime.is_some());

    // ── mkdir / rename / remove / rmdir ─────────────────────────────────
    vfs.mkdir(&root.join("newdir"), &ctx).unwrap();
    assert!(remote.path.join("newdir").is_dir());
    vfs.rename(&root.join("newdir"), &root.join("renamed"), &ctx)
        .unwrap();
    assert!(!remote.path.join("newdir").exists());
    assert!(
        remote.path.join("renamed").is_dir(),
        "an empty folder renames"
    );

    // A folder with things in it moves whole.
    std::fs::create_dir_all(remote.path.join("tree/deeper")).unwrap();
    std::fs::write(remote.path.join("tree/a.txt"), b"a").unwrap();
    std::fs::write(remote.path.join("tree/deeper/b.txt"), b"b").unwrap();
    vfs.rename(&root.join("tree"), &root.join("moved-tree"), &ctx)
        .unwrap();
    assert!(!remote.path.join("tree").exists(), "the old name is gone");
    assert_eq!(
        std::fs::read(remote.path.join("moved-tree/a.txt")).unwrap(),
        b"a"
    );
    assert_eq!(
        std::fs::read(remote.path.join("moved-tree/deeper/b.txt")).unwrap(),
        b"b"
    );

    vfs.rename(&root.join("hello.txt"), &root.join("greeting.txt"), &ctx)
        .unwrap();
    assert!(!remote.path.join("hello.txt").exists());
    assert_eq!(
        std::fs::read(remote.path.join("greeting.txt")).unwrap(),
        b"hello over rclone\n"
    );

    // Onto a name that is taken: refused, as SFTP's RENAME refuses, and
    // nothing moves — `movefile` would have replaced it without a word.
    let error = vfs
        .rename(&root.join("greeting.txt"), &root.join("big.bin"), &ctx)
        .expect_err("big.bin is taken");
    match &error {
        VfsError::Status { path, status } => {
            assert_eq!(status.code, StatusCode::Failure);
            assert!(status.message.contains("already exists"), "{error}");
            assert_eq!(path, &format!("rclone://{name}/big.bin"));
        }
        other => panic!("expected a status, got {other}"),
    }
    assert!(!error.is_connection_fatal());
    assert_eq!(std::fs::read(remote.path.join("big.bin")).unwrap(), big);
    assert!(remote.path.join("greeting.txt").exists());
    let error = vfs
        .rename(&root.join("not-there"), &root.join("anything"), &ctx)
        .expect_err("nothing to rename");
    assert!(
        matches!(&error, VfsError::Status { status, .. } if status.code == StatusCode::NoSuchFile),
        "{error}"
    );

    vfs.remove(&root.join("greeting.txt"), &ctx).unwrap();
    assert!(!remote.path.join("greeting.txt").exists());

    vfs.rmdir(&root.join("renamed"), &ctx).unwrap();
    assert!(!remote.path.join("renamed").exists());
    // Not empty: refused with rclone's words, and not emptied.
    let error = vfs
        .rmdir(&root.join("sub"), &ctx)
        .expect_err("sub has a file in it");
    assert!(error.to_string().contains("not empty"), "{error}");
    assert!(!error.is_connection_fatal());
    assert!(remote.path.join("sub/inner.txt").exists());

    // ── What rclone cannot do ───────────────────────────────────────────
    for error in [
        vfs.chmod(&root.join("big.bin"), 0o600, &ctx).unwrap_err(),
        vfs.symlink("big.bin", &root.join("link"), &ctx)
            .unwrap_err(),
        vfs.readlink(&root.join("big.bin"), &ctx).unwrap_err(),
        vfs.realpath(&root, &ctx).unwrap_err(),
    ] {
        assert!(matches!(error, VfsError::Unsupported { .. }), "{error}");
        assert!(error.to_string().contains("is not something rclone can do"));
    }
    // ...and the daemon is still the same healthy daemon afterwards.
    assert!(vfs.stat(&root.join("big.bin"), true, &ctx).is_ok());
    assert_eq!(
        sockets_in(&scratch).len(),
        1,
        "one daemon for the whole session"
    );

    // ── Clean teardown ──────────────────────────────────────────────────
    drop(vfs);
    assert!(
        sockets_in(&scratch).is_empty(),
        "dropping the vfs killed the daemon and removed its socket"
    );
}

/// Cancels that land *while a job is running* — the case that matters,
/// because the worker refuses a ctx that was cancelled before it started.
/// `--bwlimit` slows the copy enough that the cancel lands mid-transfer
/// rather than after a local copy has already finished.
#[test]
fn rclone_cancel_mid_transfer_stops_the_job_and_keeps_the_original() {
    let Some(rclone) = find_rclone() else {
        eprintln!(
            "skipping rclone_cancel_mid_transfer_stops_the_job_and_keeps_the_original: \
             no rclone on $PATH"
        );
        return;
    };
    let remote = TempDir::new("rclone-cancel-remote");
    let scratch = TempDir::new("rclone-cancel-scratch");
    let local = TempDir::new("rclone-cancel-local");
    let big: Vec<u8> = (0..16 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    remote.file("big.bin", &big);
    remote.file("tiny.bin", b"tiny");
    local.file("keep.bin", b"old");
    let big_local = local.file("big-local.bin", &big);

    let name = unique_name("cx");
    let service = local_service(&name, &remote, &scratch, &rclone);
    let mut daemon = Daemon::spawn(Arc::new(service), &["--bwlimit".into(), "2M".into()]).unwrap();

    /// Cancels the task the first time any bytes are reported.
    struct CancelOnFirstTick(Arc<TaskFlags>);
    impl ProgressSink for CancelOnFirstTick {
        fn set_total(&self, _bytes: u64, _files: u64) {}
        fn advance(&self, bytes: u64, _files: u64) {
            if bytes > 0 {
                self.0.cancel();
            }
        }
    }
    let cancelling = || {
        let flags = Arc::new(TaskFlags::new());
        TaskCtx::with_sink(
            Arc::clone(&flags),
            Arc::new(CancelOnFirstTick(Arc::clone(&flags))),
        )
    };
    let root = VfsPath::rclone(&name, "");

    // ── A download over a file that was already there ───────────────────
    let started = Instant::now();
    let error = daemon
        .run_op(
            Op::Download {
                remote: root.join("big.bin"),
                local: local.path.join("keep.bin"),
            },
            &cancelling(),
        )
        .map(|_| ())
        .expect_err("cancelled mid-transfer");
    assert!(matches!(error, VfsError::Cancelled), "{error}");
    // At the limit the copy takes seconds; a cancel that waited for it would
    // show here.
    assert!(
        started.elapsed() < T,
        "the cancel took {:?}",
        started.elapsed()
    );
    assert_eq!(std::fs::read(local.path.join("keep.bin")).unwrap(), b"old");
    assert!(
        partials_in(&local.path).is_empty(),
        "{:?}",
        partials_in(&local.path)
    );
    // The job is *stopped*, not merely unwatched: a running copy would have
    // its `.partial` growing here, and the file it replaces would change when
    // it finished.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        partials_in(&local.path).is_empty(),
        "{:?}",
        partials_in(&local.path)
    );
    assert_eq!(std::fs::read(local.path.join("keep.bin")).unwrap(), b"old");

    // ── An upload over a file that was already there ────────────────────
    let error = daemon
        .run_op(
            Op::Upload {
                local: big_local,
                remote: root.join("tiny.bin"),
            },
            &cancelling(),
        )
        .map(|_| ())
        .expect_err("cancelled mid-transfer");
    assert!(matches!(error, VfsError::Cancelled), "{error}");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        std::fs::read(remote.path.join("tiny.bin")).unwrap(),
        b"tiny",
        "a cancelled upload destroyed the file it was replacing"
    );
    assert!(
        partials_in(&remote.path).is_empty(),
        "{:?}",
        partials_in(&remote.path)
    );

    // ── And the daemon is still fine ────────────────────────────────────
    match daemon.run_op(
        Op::Stat {
            path: root.join("big.bin"),
            follow: true,
        },
        &TaskCtx::detached(),
    ) {
        Ok(Outcome::Attrs(attrs)) => assert_eq!(attrs.size, Some(big.len() as u64)),
        Ok(_) => panic!("a stat answered with something else"),
        Err(e) => panic!("the daemon did not survive the cancels: {e}"),
    }
    drop(daemon);
    assert!(
        sockets_in(&scratch).is_empty(),
        "the socket went with the daemon"
    );
}

/// A daemon that will not start reports rclone's own sentence, fast, and
/// leaves nothing behind.
#[test]
fn a_daemon_that_cannot_start_says_why() {
    let Some(rclone) = find_rclone() else {
        eprintln!("skipping a_daemon_that_cannot_start_says_why: no rclone on $PATH");
        return;
    };
    let scratch = TempDir::new("rclone-bad-scratch");
    let mut service = Service::rclone(unique_name("bad"), "/nonexistent");
    service.program = Some((rclone, vec!["--no-such-flag-anywhere".into()]));
    service.socket_dir = Some(scratch.path.join("run"));
    let started = Instant::now();
    let error = Daemon::spawn(Arc::new(service), &[])
        .map(|_| ())
        .expect_err("rclone refuses the flag");
    assert!(matches!(error, VfsError::Disconnected { .. }), "{error}");
    assert!(
        error.to_string().contains("no-such-flag-anywhere"),
        "rclone's own words: {error}"
    );
    assert!(
        started.elapsed() < T,
        "it exited; nobody waited out the deadline"
    );
    assert!(sockets_in(&scratch).is_empty());
}

/// Kill the daemon out from under a live vfs: one operation fails with a
/// connection error, and the next starts a fresh daemon and succeeds.
#[test]
fn a_killed_daemon_is_replaced_on_the_next_request() {
    let Some(rclone) = find_rclone() else {
        eprintln!("skipping a_killed_daemon_is_replaced_on_the_next_request: no rclone on $PATH");
        return;
    };
    let remote = TempDir::new("rclone-respawn");
    let scratch = TempDir::new("rclone-respawn-scratch");
    remote.file("still-here.txt", b"yes");
    // A stand-in for rclone that writes its pid down and then *is* rclone.
    let pid_file = scratch.path.join("pid");
    let wrapper = scratch.file(
        "rclone-wrapper",
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec '{}' \"$@\"\n",
            pid_file.display(),
            rclone.display()
        )
        .as_bytes(),
    );
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let name = unique_name("rs");
    let service = local_service(&name, &remote, &scratch, &wrapper);
    let vfs = vfs_over(service);
    let ctx = TaskCtx::detached();
    let at = VfsPath::rclone(&name, "still-here.txt");

    assert!(vfs.stat(&at, true, &ctx).is_ok());
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();
    // `kill(1)` rather than `libc::kill`, so the tests stay out of the crate's
    // unsafe census.
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid])
        .status()
        .unwrap();
    assert!(killed.success());

    let deadline = Instant::now() + T;
    loop {
        match vfs.stat(&at, true, &ctx) {
            Err(e) if e.is_connection_fatal() => break,
            Ok(_) => {}
            Err(e) => panic!("expected a connection-fatal error, got {e}"),
        }
        assert!(Instant::now() < deadline, "the death was never noticed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let attrs = vfs
        .stat(&at, true, &ctx)
        .expect("a fresh daemon answers the next request");
    assert_eq!(attrs.size, Some(3));
    drop(vfs);
    assert!(sockets_in(&scratch).is_empty());
}

/// What the stand-in server saw.
#[derive(Debug)]
enum Seen {
    /// A request arrived; its first line.
    Asked(String),
    /// The client closed the connection without an answer.
    Abandoned,
    /// Nothing happened within the server's own deadline.
    Nothing,
}

/// Accept on a non-blocking `listener` until `deadline`, or `None`.
fn accept_by(listener: &std::net::TcpListener, deadline: Instant) -> Option<std::net::TcpStream> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).ok()?;
                return Some(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// **A listing is a job, and one nobody wants is stopped.** rclone's `:http`
/// backend is pointed at a server on the loopback that takes the request and
/// never answers — the shape of a cloud folder that takes minutes to page —
/// so the listing's job runs until something ends it. Cancelling the listing
/// must end it: rclone drops the request (the server sees the close), and the
/// worker is free for the next command rather than stuck behind the hang.
#[test]
fn a_listing_nobody_wants_is_stopped_rather_than_waited_out() {
    use std::io::{Read, Write};
    let Some(rclone) = find_rclone() else {
        eprintln!(
            "skipping a_listing_nobody_wants_is_stopped_rather_than_waited_out: \
             no rclone on $PATH"
        );
        return;
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen, events) = std::sync::mpsc::channel::<Seen>();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_done = Arc::clone(&done);
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + T;
        // The first request is the listing: take it and say nothing.
        let Some(mut held) = accept_by(&listener, deadline) else {
            let _ = seen.send(Seen::Nothing);
            return;
        };
        held.set_read_timeout(Some(T)).unwrap();
        let mut buffer = [0u8; 4096];
        let n = held.read(&mut buffer).unwrap_or(0);
        let first = String::from_utf8_lossy(&buffer[..n])
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let _ = seen.send(Seen::Asked(first));
        loop {
            match held.read(&mut buffer) {
                Ok(0) => {
                    let _ = seen.send(Seen::Abandoned);
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = seen.send(Seen::Nothing);
                    return;
                }
            }
        }
        // Everything after it is answered at once: nothing here.
        while !server_done.load(Ordering::SeqCst) {
            let Some(mut next) = accept_by(&listener, Instant::now() + Duration::from_millis(50))
            else {
                continue;
            };
            let _ = next.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = next.read(&mut buffer);
            let _ = next.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });

    let scratch = TempDir::new("rclone-slow-scratch");
    let name = unique_name("slow");
    let service = scratch_service(
        Service::rclone(&name, format!(":http,url='http://127.0.0.1:{port}/':")),
        &scratch,
        &rclone,
    );
    let vfs = vfs_over(service);
    let root = VfsPath::rclone(&name, "");

    let token = vfs.scan(root.clone());
    match events.recv_timeout(T) {
        Ok(Seen::Asked(line)) => assert!(line.starts_with("GET /"), "{line}"),
        other => panic!("the listing never reached the server: {other:?}"),
    }
    // Several polls' worth of waiting: the job is running and has not ended.
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        vfs.drain().is_empty(),
        "nothing can have arrived: the server has not answered"
    );

    vfs.cancel(token);
    match events.recv_timeout(T) {
        Ok(Seen::Abandoned) => {}
        other => panic!("rclone kept the request open after the listing was cancelled: {other:?}"),
    }

    // The worker is free, and the daemon is the same healthy daemon: the next
    // listing is answered — here, that there is nothing there.
    let token = vfs.scan(root.clone());
    let updates = collect_listing(&vfs, token);
    assert!(
        matches!(
            updates.last(),
            Some(VfsUpdate::Failed { .. } | VfsUpdate::Done { .. })
        ),
        "{updates:?}"
    );
    assert_eq!(sockets_in(&scratch).len(), 1, "no respawn was needed");

    done.store(true, Ordering::SeqCst);
    drop(vfs);
    server.join().unwrap();
}

/// **The daemon cannot outlive the thread that owns it.** The thread that
/// spawned it ends without the `Daemon` ever being dropped — which is what a
/// `SIGKILL`, an abort or the OOM killer does to the whole process — and the
/// kernel's parent-death signal ends rclone anyway.
///
/// The exited daemon is left a zombie: its `Child` was forgotten, so nothing
/// in this process will reap it, and it goes when the test binary does.
#[test]
fn a_daemon_dies_with_the_thread_that_spawned_it() {
    let Some(rclone) = find_rclone() else {
        eprintln!("skipping a_daemon_dies_with_the_thread_that_spawned_it: no rclone on $PATH");
        return;
    };
    let remote = TempDir::new("rclone-orphan-remote");
    let scratch = TempDir::new("rclone-orphan-scratch");
    let service = local_service(&unique_name("or"), &remote, &scratch, &rclone);
    let pid = std::thread::spawn(move || {
        let daemon = Daemon::spawn(Arc::new(service), &[]).unwrap();
        let pid = daemon.pid();
        assert!(!has_exited(pid), "the daemon is up while its thread is");
        // No destructor: no SIGTERM from us, no reap, no unlink.
        std::mem::forget(daemon);
        pid
    })
    .join()
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    while !has_exited(pid) {
        assert!(
            Instant::now() < deadline,
            "rclone {pid} outlived the thread that owned it"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A download that fails removes the `.partial` rclone left beside its
/// destination — and only that: the user's own files that merely look alike
/// stay.
#[test]
fn a_failed_download_sweeps_up_its_own_partials_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;
    let Some(rclone) = find_rclone() else {
        eprintln!(
            "skipping a_failed_download_sweeps_up_its_own_partials_and_nothing_else: \
             no rclone on $PATH"
        );
        return;
    };
    let remote = TempDir::new("rclone-sweep-remote");
    let scratch = TempDir::new("rclone-sweep-scratch");
    let local = TempDir::new("rclone-sweep-local");
    // A file rclone can stat but not read: the copy fails after it starts.
    let locked = remote.file("locked.bin", b"secret");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&locked).is_ok() {
        eprintln!(
            "skipping a_failed_download_sweeps_up_its_own_partials_and_nothing_else: \
             running as a user permissions do not stop"
        );
        return;
    }
    // What a daemon killed mid-copy leaves, and three neighbours that are
    // somebody's own files.
    let debris = local.file("target.bin.0123abcd.partial", b"half");
    let theirs = [
        local.file("target.bin.backup.partial", b"mine"),
        local.file("other.bin.0123abcd.partial", b"mine"),
        local.file("target.bin.partial", b"mine"),
    ];

    let name = unique_name("sw");
    let vfs = vfs_over(local_service(&name, &remote, &scratch, &rclone));
    let root = VfsPath::rclone(&name, "");
    let error = vfs
        .download(
            &root.join("locked.bin"),
            &local.path.join("target.bin"),
            &TaskCtx::detached(),
        )
        .expect_err("the source cannot be read");
    assert!(
        matches!(&error, VfsError::Status { status, .. } if status.code == StatusCode::Failure),
        "{error}"
    );
    assert!(!debris.exists(), "rclone's leftover partial was swept up");
    for file in &theirs {
        assert!(file.exists(), "{} is not rclone's to lose", file.display());
    }
    assert!(!local.path.join("target.bin").exists());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
}
