#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::*;
use crate::ops::copy::{syncs, without_reflink};
use crate::ops::fixture::TempTree;
use crate::ops::{Trash, COPY_CHUNK};
use crate::tasks::{ProgressSink, TaskCtx, TaskFlags};

fn quick(sources: &[PathBuf], dest: &Path) -> SyncPlan {
    plan(sources, dest, SyncOptions::default(), &|| false, &|_| {}).unwrap()
}

fn thorough(sources: &[PathBuf], dest: &Path) -> SyncPlan {
    plan(
        sources,
        dest,
        SyncOptions { content: true },
        &|| false,
        &|_| {},
    )
    .unwrap()
}

/// Every classified path as `(label, class)`, in plan order.
fn classes(plan: &SyncPlan) -> Vec<(String, Class)> {
    plan.items
        .iter()
        .map(|item| (plan.label(item), item.class))
        .collect()
}

fn class_of(plan: &SyncPlan, label: &str) -> Class {
    plan.items
        .iter()
        .find(|item| plan.label(item) == label)
        .map(|item| item.class)
        .unwrap_or_else(|| panic!("{label} is not in the plan: {:?}", classes(plan)))
}

fn set_mtime(path: &Path, when: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::symlink_metadata(path).unwrap().modified().unwrap()
}

/// A source file and its destination copy with the same bytes and date.
fn twin(t: &TempTree, rel: &str, body: &[u8]) -> (PathBuf, PathBuf) {
    let src = t.file(Path::new("src").join(rel), body);
    let dst = t.file(Path::new("dst").join(rel), body);
    set_mtime(&dst, mtime(&src));
    (src, dst)
}

fn tmp_names(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in walk(dir) {
        let name = entry.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with(".df-tmp-") {
            found.push(entry.display().to_string());
        }
    }
    found
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if std::fs::symlink_metadata(&path).unwrap().is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

/// A sink that remembers the declared total and adds up what was reported.
#[derive(Default)]
struct Record {
    total: Mutex<(u64, u64)>,
    done: Mutex<(u64, u64)>,
}

impl ProgressSink for Record {
    fn set_total(&self, bytes: u64, files: u64) {
        *self.total.lock().unwrap() = (bytes, files);
    }
    fn advance(&self, bytes: u64, files: u64) {
        let mut done = self.done.lock().unwrap();
        done.0 += bytes;
        done.1 += files;
    }
}

/// A sink that cancels the task once `after` bytes have gone by, so "cancel
/// mid-copy" is a deterministic assertion rather than a sleep.
struct CancelAfter {
    flags: Arc<TaskFlags>,
    after: u64,
    seen: AtomicU64,
}

impl ProgressSink for CancelAfter {
    fn set_total(&self, _bytes: u64, _files: u64) {}
    fn advance(&self, bytes: u64, _files: u64) {
        if self.seen.fetch_add(bytes, Ordering::SeqCst) + bytes >= self.after {
            self.flags.cancel();
        }
    }
}

// ── The planner ─────────────────────────────────────────────────────────────

#[test]
fn the_planner_sorts_every_path_into_new_changed_unchanged_and_extra() {
    let t = TempTree::new("sync-classes");
    let photos = t.dir("src/photos");
    t.file("src/photos/new.jpg", b"brand new");
    t.file("src/photos/2024/a.jpg", b"aaaa");
    twin(&t, "photos/same.jpg", b"same bytes");
    t.file("src/photos/grew.jpg", b"longer now");
    t.file("dst/photos/grew.jpg", b"short");
    t.file("dst/photos/only-here.jpg", b"extra");
    let dest = t.join("dst");

    let plan = quick(&[photos], &dest);
    assert_eq!(
        classes(&plan),
        vec![
            ("photos/".to_string(), Class::Unchanged),
            ("photos/2024/".to_string(), Class::New),
            ("photos/2024/a.jpg".to_string(), Class::New),
            ("photos/grew.jpg".to_string(), Class::Changed),
            ("photos/new.jpg".to_string(), Class::New),
            ("photos/same.jpg".to_string(), Class::Unchanged),
            ("photos/only-here.jpg".to_string(), Class::Extra),
        ]
    );
    // The counts are of things, not folders: `2024/` is structure for `a.jpg`.
    assert_eq!(
        plan.new,
        Tally {
            count: 2,
            bytes: 13
        }
    );
    assert_eq!(
        plan.changed,
        Tally {
            count: 1,
            bytes: 10
        }
    );
    assert_eq!(
        plan.unchanged,
        Tally {
            count: 1,
            bytes: 10
        }
    );
    assert_eq!(plan.extra, Tally { count: 1, bytes: 5 });
    assert_eq!(plan.bytes_to_copy(), 23);
    assert_eq!(
        plan.listed(Mode::Update)
            .map(|item| plan.label(item))
            .collect::<Vec<_>>(),
        [
            "photos/2024/",
            "photos/2024/a.jpg",
            "photos/grew.jpg",
            "photos/new.jpg"
        ]
    );
}

#[test]
fn a_file_lands_under_its_own_name_and_a_folder_under_its_own() {
    let t = TempTree::new("sync-roots");
    let file = t.file("src/notes.txt", b"n");
    let folder = t.dir("src/photos");
    let dest = t.dir("dst");
    let plan = quick(&[file, folder], &dest);
    assert_eq!(plan.roots[0].dst, dest.join("notes.txt"));
    assert_eq!(plan.roots[1].dst, dest.join("photos"));
    assert_eq!(
        classes(&plan),
        vec![
            ("notes.txt".to_string(), Class::New),
            ("photos/".to_string(), Class::New),
        ]
    );
    // An empty new folder is one new thing; nothing else stands for it.
    assert_eq!(plan.new.count, 2);
}

#[test]
fn modification_times_two_seconds_apart_are_the_same_time() {
    let t = TempTree::new("sync-slack");
    let (src, dst) = twin(&t, "a.jpg", b"body");
    let base = mtime(&src);
    let dest = t.join("dst");

    // FAT's two-second step, from either side.
    for apart in [Duration::from_millis(1500), Duration::from_secs(2)] {
        set_mtime(&dst, base + apart);
        assert_eq!(
            class_of(&quick(std::slice::from_ref(&src), &dest), "a.jpg"),
            Class::Unchanged
        );
        set_mtime(&dst, base - apart);
        assert_eq!(
            class_of(&quick(std::slice::from_ref(&src), &dest), "a.jpg"),
            Class::Unchanged
        );
    }
    set_mtime(&dst, base + Duration::from_secs(3));
    assert_eq!(
        class_of(&quick(std::slice::from_ref(&src), &dest), "a.jpg"),
        Class::Changed
    );
}

#[test]
fn a_content_comparison_catches_what_size_and_date_cannot() {
    let t = TempTree::new("sync-content");
    let src = t.file("src/a.bin", b"the right bytes");
    let dst = t.file("dst/a.bin", b"the wrong bytes");
    set_mtime(&dst, mtime(&src));
    let dest = t.join("dst");

    assert_eq!(
        class_of(&quick(std::slice::from_ref(&src), &dest), "a.bin"),
        Class::Unchanged
    );
    assert_eq!(
        class_of(&thorough(std::slice::from_ref(&src), &dest), "a.bin"),
        Class::Changed
    );
    // …and agrees when the bytes really are the same.
    std::fs::write(&dst, b"the right bytes").unwrap();
    set_mtime(&dst, mtime(&src));
    assert_eq!(
        class_of(&thorough(&[src], &dest), "a.bin"),
        Class::Unchanged
    );
}

#[test]
fn a_link_is_compared_by_where_it_points() {
    let t = TempTree::new("sync-links");
    let dir = t.dir("src/d");
    t.symlink("a", "src/d/same");
    t.symlink("a", "dst/d/same");
    t.symlink("a", "src/d/moved");
    t.symlink("b", "dst/d/moved");
    let plan = quick(&[dir], &t.join("dst"));
    assert_eq!(class_of(&plan, "d/same"), Class::Unchanged);
    assert_eq!(class_of(&plan, "d/moved"), Class::Changed);
}

#[test]
fn another_kind_of_thing_under_the_same_name_is_a_change() {
    let t = TempTree::new("sync-kinds");
    let dir = t.dir("src/d");
    t.file("src/d/was-a-folder", b"now a file");
    t.file("dst/d/was-a-folder/inside", b"x");
    t.file("src/d/was-a-file/inside", b"now a folder");
    t.file("dst/d/was-a-file", b"x");
    let plan = quick(&[dir], &t.join("dst"));
    assert_eq!(class_of(&plan, "d/was-a-folder"), Class::Changed);
    assert_eq!(class_of(&plan, "d/was-a-file/"), Class::Changed);
    // Below a folder that replaces a file, everything is new.
    assert_eq!(class_of(&plan, "d/was-a-file/inside"), Class::New);
    // What is inside the folder being replaced is not an extra of anything.
    assert!(!classes(&plan)
        .iter()
        .any(|(label, _)| label.contains("was-a-folder/")));
}

#[test]
fn extras_are_found_at_every_depth_with_everything_under_them() {
    let t = TempTree::new("sync-extras");
    let dir = t.dir("src/d");
    t.dir("src/d/a/b");
    t.dir("dst/d/a/b");
    t.file("dst/d/a/b/deep-extra.txt", b"12");
    t.file("dst/d/gone/one.txt", b"123");
    t.file("dst/d/gone/two/three.txt", b"1234");
    let plan = quick(&[dir], &t.join("dst"));
    for label in [
        "d/a/b/deep-extra.txt",
        "d/gone/",
        "d/gone/one.txt",
        "d/gone/two/",
        "d/gone/two/three.txt",
    ] {
        assert_eq!(class_of(&plan, label), Class::Extra, "{label}");
    }
    // Three things, not five: the folders are how they are arranged.
    assert_eq!(plan.extra, Tally { count: 3, bytes: 9 });
    assert!(
        plan.in_sync(Mode::Update),
        "extras alone are nothing to copy"
    );
}

#[test]
fn a_socket_is_skipped_by_name_rather_than_silently() {
    let t = TempTree::new("sync-special");
    let dir = t.dir("src/d");
    let socket = dir.join("sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    t.file("src/d/real.txt", b"x");
    let plan = quick(&[dir], &t.dir("dst"));
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].0, socket);
    assert!(!classes(&plan)
        .iter()
        .any(|(label, _)| label.ends_with("sock")));
    // …and the run says so too, rather than reporting a clean copy.
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
    assert_eq!(report.skipped, plan.skipped);
}

#[test]
fn the_rails_refuse_what_no_sync_can_do_safely() {
    let t = TempTree::new("sync-rails");
    let project = t.dir("project");
    let inner = t.dir("project/inner");
    let err = roots(std::slice::from_ref(&project), &inner).unwrap_err();
    assert_eq!(err.to_string(), "cannot sync a folder into itself");
    let err = roots(std::slice::from_ref(&project), &project).unwrap_err();
    assert_eq!(err.to_string(), "cannot sync a folder into itself");

    // Through a symlink, it is still itself.
    let link = t.symlink(&project, "link");
    let err = roots(std::slice::from_ref(&project), &link).unwrap_err();
    assert_eq!(err.to_string(), "cannot sync a folder into itself");

    // Onto itself: the folder's own parent.
    let err = roots(std::slice::from_ref(&project), t.path()).unwrap_err();
    assert!(err.to_string().contains("onto itself"), "{err}");

    // A destination that holds the source.
    let nested = t.dir("x/photos/photos");
    let err = roots(&[nested], &t.join("x")).unwrap_err();
    assert!(err.to_string().contains("holds it"), "{err}");

    // Two sources with one name.
    let a = t.file("a/notes.txt", b"a");
    let b = t.file("b/notes.txt", b"b");
    let err = roots(&[a, b], &t.dir("out")).unwrap_err();
    assert!(err.to_string().contains("called notes.txt"), "{err}");
}

#[test]
fn a_stopped_walk_is_cancelled_not_a_short_plan() {
    let t = TempTree::new("sync-stop");
    let dir = t.dir("src/d");
    t.file("src/d/a", b"a");
    let seen = AtomicU64::new(0);
    let result = plan(
        &[dir],
        &t.dir("dst"),
        SyncOptions::default(),
        &|| seen.load(Ordering::SeqCst) >= 1,
        &|n| {
            seen.fetch_add(n, Ordering::SeqCst);
        },
    );
    assert!(
        matches!(result, Err(crate::DfError::Cancelled)),
        "{result:?}"
    );
}

// ── The executor ────────────────────────────────────────────────────────────

#[test]
fn a_sync_copies_the_new_and_the_changed_and_touches_nothing_else() {
    let t = TempTree::new("sync-exec");
    let photos = t.dir("src/photos");
    t.file("src/photos/new.jpg", b"brand new");
    t.file("src/photos/2024/a.jpg", b"aaaa");
    let (_, same) = twin(&t, "photos/same.jpg", b"same bytes");
    t.file("src/photos/grew.jpg", b"longer now");
    t.file("dst/photos/grew.jpg", b"short");
    let extra = t.file("dst/photos/only-here.jpg", b"extra");
    let dest = t.join("dst");
    let same_before = std::fs::metadata(&same).unwrap();

    let plan = quick(&[photos], &dest);
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());

    assert_eq!(report.problems(), 0, "{report:?}");
    assert_eq!(report.copied, 3);
    assert_eq!(report.copied_bytes, 9 + 4 + 10);
    assert_eq!(report.made, 1);
    assert_eq!(report.verified, 3);
    assert_eq!(
        std::fs::read(dest.join("photos/2024/a.jpg")).unwrap(),
        b"aaaa"
    );
    assert_eq!(
        std::fs::read(dest.join("photos/grew.jpg")).unwrap(),
        b"longer now"
    );
    // The unchanged file is the same file: same inode, same date, not rewritten.
    let same_after = std::fs::metadata(&same).unwrap();
    assert_eq!(same_after.ino(), same_before.ino());
    assert_eq!(
        same_after.modified().unwrap(),
        same_before.modified().unwrap()
    );
    // And an extra is left exactly where it was.
    assert_eq!(std::fs::read(&extra).unwrap(), b"extra");

    // A second plan finds nothing left to do.
    let again = quick(&[t.join("src/photos")], &dest);
    assert!(again.in_sync(Mode::Update), "{:?}", classes(&again));
    assert_eq!(again.unchanged.count, 4);
}

#[test]
fn a_new_folder_gets_its_mode_and_date_after_its_children() {
    use std::os::unix::fs::PermissionsExt;
    let t = TempTree::new("sync-dir-meta");
    let dir = t.dir("src/locked");
    t.file("src/locked/inside.txt", b"x");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let dest = t.dir("dst");

    let plan = quick(std::slice::from_ref(&dir), &dest);
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let copy = dest.join("locked");
    let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        report.problems(),
        0,
        "a read-only folder still takes its children: {report:?}"
    );
    assert_eq!(mode & 0o777, 0o555);
    assert_eq!(std::fs::read(copy.join("inside.txt")).unwrap(), b"x");
    assert_eq!(
        mtime(&copy),
        mtime(&dir),
        "the date is the source's, not the copy's"
    );
}

#[test]
fn a_sync_flushes_every_file_and_every_name_it_writes() {
    let t = TempTree::new("sync-fsync");
    let dir = t.dir("src/d");
    t.file("src/d/a", b"a");
    t.file("src/d/sub/b", b"b");
    t.symlink("a", "src/d/link");
    let plan = quick(&[dir], &t.dir("dst"));

    let before = syncs();
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
    let after = syncs();
    assert_eq!(report.problems(), 0, "{report:?}");
    assert_eq!(
        after.0 - before.0,
        2,
        "each file flushed before it is named"
    );
    // `d/`, `d/sub/`, `a`, `sub/b` and `link`: five names, each flushed into
    // the directory that holds it.
    assert_eq!(after.1 - before.1, 5);
}

#[test]
fn the_verify_pass_catches_a_copy_damaged_after_it_was_written() {
    let t = TempTree::new("sync-corrupt");
    let dir = t.dir("src/d");
    t.file("src/d/good.bin", &vec![1u8; 3000]);
    t.file("src/d/bad.bin", &vec![2u8; 3000]);
    t.file("src/d/also-bad.bin", &[3u8; 10]);
    let dest = t.dir("dst");
    let plan = quick(&[dir], &dest);

    let bad = dest.join("d/bad.bin");
    let also = dest.join("d/also-bad.bin");
    let (b, a) = (bad.clone(), also.clone());
    execute::before_verify(move || {
        // One byte flipped in place, as a failing card would; and one file
        // gone, as a card pulled too early would.
        let mut bytes = std::fs::read(&b).unwrap();
        bytes[1234] ^= 0xff;
        std::fs::write(&b, bytes).unwrap();
        std::fs::remove_file(&a).unwrap();
    });
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());

    assert_eq!(report.copied, 3);
    assert_eq!(report.verified, 1);
    assert!(report.errors.is_empty(), "{report:?}");
    let mut failures = report.verify_failures.clone();
    failures.sort();
    assert_eq!(
        failures,
        vec![
            (also, "missing at the destination".to_string()),
            (bad, "contents differ from the source".to_string()),
        ],
        "every bad file is named, not only the first"
    );
    assert_eq!(report.problems(), 2);
}

#[test]
fn verifying_everything_reads_back_what_was_not_copied_too() {
    let t = TempTree::new("sync-verify-all");
    let dir = t.dir("src/d");
    t.file("src/d/fine.txt", b"fine");
    // Rotted in place: same size, same date, other bytes. A quick plan calls
    // it unchanged, and only reading it back can say otherwise.
    let (_, rotted) = twin(&t, "d/rotted.txt", b"rotten");
    std::fs::write(&rotted, b"R0tten").unwrap();
    set_mtime(&rotted, mtime(&t.join("src/d/rotted.txt")));
    let plan = quick(&[dir], &t.join("dst"));

    let copied = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
    assert_eq!(
        (copied.copied, copied.verified, copied.problems()),
        (1, 1, 0)
    );

    let again = quick(&[t.join("src/d")], &t.join("dst"));
    assert!(again.in_sync(Mode::Update));
    let everything = execute(
        &again,
        Mode::Update,
        Verify::Everything,
        &TaskCtx::detached(),
    );
    assert_eq!(everything.verified, 1, "{everything:?}");
    assert_eq!(
        everything.verify_failures,
        vec![(rotted, "contents differ from the source".to_string())]
    );
}

#[test]
fn an_in_sync_plan_still_verifies_everything_it_is_asked_to() {
    let t = TempTree::new("sync-verify-only");
    let dir = t.dir("src/d");
    twin(&t, "d/a", b"aa");
    twin(&t, "d/b", b"bbb");
    let plan = quick(&[dir], &t.join("dst"));
    assert!(plan.in_sync(Mode::Update));
    let report = execute(
        &plan,
        Mode::Update,
        Verify::Everything,
        &TaskCtx::detached(),
    );
    assert_eq!(
        (report.copied, report.verified, report.problems()),
        (0, 2, 0)
    );
}

#[test]
fn the_progress_total_is_every_byte_copied_plus_both_sides_read_back() {
    let t = TempTree::new("sync-progress");
    let dir = t.dir("src/d");
    t.file("src/d/a", &vec![0u8; 3 * COPY_CHUNK + 7]);
    t.file("src/d/b", b"12345");
    twin(&t, "d/c", b"unchanged");
    let plan = quick(&[dir], &t.join("dst"));
    let copy_bytes = 3 * COPY_CHUNK as u64 + 7 + 5;

    for (verify, read, files) in [
        (Verify::Copied, copy_bytes, 2 + 2),
        (Verify::Everything, copy_bytes + 9, 2 + 3),
    ] {
        // The plan was made before either run: put the destination back the
        // way it saw it.
        let _ = std::fs::remove_file(t.join("dst/d/a"));
        let _ = std::fs::remove_file(t.join("dst/d/b"));
        let record = Arc::new(Record::default());
        let ctx = TaskCtx::with_sink(Arc::new(TaskFlags::new()), record.clone());
        let report = execute(&plan, Mode::Update, verify, &ctx);
        assert_eq!(report.problems(), 0, "{report:?}");
        let total = *record.total.lock().unwrap();
        assert_eq!(total, (copy_bytes + 2 * read, files), "{verify:?}");
        assert_eq!(
            *record.done.lock().unwrap(),
            total,
            "{verify:?}: the bar ends full"
        );
    }
}

#[test]
fn a_cancelled_sync_leaves_no_temporary_files_and_no_half_written_one() {
    let t = TempTree::new("sync-cancel");
    let dir = t.dir("src/d");
    t.file("src/d/big.bin", &vec![5u8; 6 * COPY_CHUNK]);
    t.file("dst/d/big.bin", b"the old one, whole");
    t.file("src/d/later.bin", b"never reached");
    let plan = quick(&[dir], &t.join("dst"));

    let flags = Arc::new(TaskFlags::new());
    let sink = Arc::new(CancelAfter {
        flags: Arc::clone(&flags),
        after: COPY_CHUNK as u64,
        seen: AtomicU64::new(0),
    });
    let ctx = TaskCtx::with_sink(flags, sink);
    let report = without_reflink(|| execute(&plan, Mode::Update, Verify::Copied, &ctx));

    assert!(report.cancelled);
    assert!(
        report.verify_failures.is_empty(),
        "no verify after a cancel"
    );
    assert_eq!(tmp_names(&t.join("dst")), Vec::<String>::new());
    assert_eq!(
        std::fs::read(t.join("dst/d/big.bin")).unwrap(),
        b"the old one, whole",
        "an interrupted overwrite leaves the old file as it was"
    );
    assert!(!t.join("dst/d/later.bin").exists());
}

#[test]
fn a_folder_in_the_way_of_a_file_is_refused_and_a_file_in_the_way_of_a_folder_is_replaced() {
    let t = TempTree::new("sync-in-the-way");
    let dir = t.dir("src/d");
    t.file("src/d/was-a-folder", b"now a file");
    let kept = t.file("dst/d/was-a-folder/precious", b"keep me");
    t.file("src/d/was-a-file/inside", b"now a folder");
    t.file("dst/d/was-a-file", b"old file");
    t.file("src/d/after", b"still copied");
    let plan = quick(&[dir], &t.join("dst"));
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());

    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(report.errors[0].0, t.join("dst/d/was-a-folder"));
    assert_eq!(std::fs::read(&kept).unwrap(), b"keep me");
    assert_eq!(
        std::fs::read(t.join("dst/d/was-a-file/inside")).unwrap(),
        b"now a folder"
    );
    assert_eq!(
        std::fs::read(t.join("dst/d/after")).unwrap(),
        b"still copied"
    );
}

// ── Mirror ──────────────────────────────────────────────────────────────────

/// The extras fixture: a source `d` beside a destination that has an extra
/// file three levels down, and an extra folder with more under it.
fn with_extras(t: &TempTree) -> SyncPlan {
    let dir = t.dir("src/d");
    t.dir("src/d/a/b");
    t.file("src/d/keep.txt", b"keep");
    t.dir("dst/d/a/b");
    t.file("dst/d/a/b/deep-extra.txt", b"12");
    t.file("dst/d/gone/one.txt", b"123");
    t.file("dst/d/gone/two/three.txt", b"1234");
    // The trash this thread's mirrors use, inside the fixture.
    trash_at(t.join("Trash"));
    quick(&[dir], &t.join("dst"))
}

#[test]
fn a_mirror_has_something_to_do_where_an_update_has_nothing() {
    let t = TempTree::new("sync-mirror-in-sync");
    let dir = t.dir("src/d");
    twin(&t, "d/a", b"a");
    t.file("dst/d/extra", b"x");
    let plan = quick(&[dir], &t.join("dst"));
    assert!(plan.in_sync(Mode::Update));
    assert!(!plan.in_sync(Mode::Mirror));
    assert_eq!(plan.listed(Mode::Update).count(), 0);
    assert_eq!(
        plan.listed(Mode::Mirror)
            .map(|item| plan.label(item))
            .collect::<Vec<_>>(),
        ["d/extra"]
    );
}

#[test]
fn removals_are_the_topmost_extras_deepest_first_with_what_each_takes() {
    let t = TempTree::new("sync-removals");
    let plan = with_extras(&t);
    let removals: Vec<(String, u64)> = plan
        .removals(Mode::Mirror)
        .into_iter()
        .map(|(index, leaves)| (plan.label(&plan.items[index]), leaves))
        .collect();
    assert_eq!(
        removals,
        vec![
            ("d/a/b/deep-extra.txt".to_string(), 1),
            // `gone/` takes `one.txt` and `two/three.txt` with it, and is
            // removed once — not after its children, one by one.
            ("d/gone/".to_string(), 2),
        ]
    );
}

#[test]
fn a_mirror_trashes_the_extras_after_copying_and_before_verifying() {
    let t = TempTree::new("sync-mirror-trash");
    let mut plan = with_extras(&t);
    plan.removal = Removal::Trash;
    let dest = t.join("dst/d");
    let gone = dest.join("gone");
    execute::before_verify(move || {
        assert!(
            !gone.exists(),
            "the extras are gone by the time the verify runs"
        );
    });
    let report = execute(&plan, Mode::Mirror, Verify::Copied, &TaskCtx::detached());

    assert_eq!(report.problems(), 0, "{report:?}");
    assert_eq!((report.copied, report.removed), (1, 3));
    assert_eq!(report.removal, Removal::Trash);
    assert!(!dest.join("a/b/deep-extra.txt").exists());
    assert!(!dest.join("gone").exists());
    assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"keep");
    // In the trash, whole: the folder is one item that can go back as one.
    let files = t.join("Trash/files");
    assert_eq!(
        std::fs::read(files.join("gone/two/three.txt")).unwrap(),
        b"1234"
    );
    assert!(files.join("deep-extra.txt").is_file());
    let trashed = Trash::at(t.join("Trash")).list().unwrap();
    assert_eq!(trashed.len(), 2);

    // And the next plan finds the two sides the same.
    assert!(quick(&[t.join("src/d")], &t.join("dst")).in_sync(Mode::Mirror));
}

#[test]
fn a_mirror_deletes_for_good_where_there_is_no_trash() {
    let t = TempTree::new("sync-mirror-delete");
    let mut plan = with_extras(&t);
    plan.removal = Removal::Delete;
    let report = execute(&plan, Mode::Mirror, Verify::Copied, &TaskCtx::detached());
    assert_eq!(report.problems(), 0, "{report:?}");
    assert_eq!((report.removed, report.removal), (3, Removal::Delete));
    assert!(!t.join("dst/d/gone").exists());
    assert!(!t.join("Trash").exists(), "nothing went to a trash");
}

#[test]
fn an_update_removes_nothing_whatever_the_plan_found() {
    let t = TempTree::new("sync-update-keeps");
    let plan = with_extras(&t);
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
    assert_eq!((report.copied, report.removed), (1, 0));
    assert!(t.join("dst/d/gone/two/three.txt").is_file());
    assert!(t.join("dst/d/a/b/deep-extra.txt").is_file());
}

#[test]
fn a_mirror_moves_a_folder_in_the_way_to_the_trash_and_copies_the_file() {
    let t = TempTree::new("sync-mirror-in-the-way");
    let dir = t.dir("src/d");
    t.file("src/d/was-a-folder", b"now a file");
    t.file("dst/d/was-a-folder/precious", b"kept, in the trash");
    trash_at(t.join("Trash"));
    let mut plan = quick(&[dir], &t.join("dst"));
    plan.removal = Removal::Trash;
    assert_eq!(plan.folders_in_the_way, 1);
    let record = Arc::new(Record::default());
    let ctx = TaskCtx::with_sink(Arc::new(TaskFlags::new()), record.clone());
    let report = execute(&plan, Mode::Mirror, Verify::Copied, &ctx);
    assert_eq!(report.problems(), 0, "{report:?}");
    assert_eq!(report.removed, 1, "the folder that made room is counted");
    // One copy, one folder out of its way, one verify — and all of them done.
    assert_eq!(record.total.lock().unwrap().1, 3);
    assert_eq!(*record.done.lock().unwrap(), *record.total.lock().unwrap());
    assert_eq!(
        std::fs::read(t.join("dst/d/was-a-folder")).unwrap(),
        b"now a file"
    );
    assert_eq!(
        std::fs::read(t.join("Trash/files/was-a-folder/precious")).unwrap(),
        b"kept, in the trash"
    );
}

#[test]
fn a_removal_that_fails_is_recorded_and_the_rest_carry_on() {
    use std::os::unix::fs::PermissionsExt;
    let t = TempTree::new("sync-mirror-fail");
    let mut plan = with_extras(&t);
    plan.removal = Removal::Delete;
    // Nothing may be unlinked from `a/b`, so its extra cannot go.
    let locked = t.join("dst/d/a/b");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let report = execute(&plan, Mode::Mirror, Verify::Copied, &TaskCtx::detached());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(report.errors[0].0, locked.join("deep-extra.txt"));
    assert_eq!(report.removed, 2, "the folder still went");
    assert!(!t.join("dst/d/gone").exists());
}

#[test]
fn a_mirrors_progress_counts_each_removal() {
    let t = TempTree::new("sync-mirror-progress");
    let mut plan = with_extras(&t);
    plan.removal = Removal::Delete;
    let record = Arc::new(Record::default());
    let ctx = TaskCtx::with_sink(Arc::new(TaskFlags::new()), record.clone());
    let report = execute(&plan, Mode::Mirror, Verify::Copied, &ctx);
    assert_eq!(report.problems(), 0, "{report:?}");
    // One copy, two removals, one verify; four bytes copied, eight read back.
    let total = *record.total.lock().unwrap();
    assert_eq!(total, (4 + 8, 1 + 2 + 1));
    assert_eq!(*record.done.lock().unwrap(), total);
}

// ── Debris and case ─────────────────────────────────────────────────────────

#[test]
fn a_killed_copys_leftover_is_cleared_even_by_an_update() {
    let t = TempTree::new("sync-debris");
    let dir = t.dir("src/d");
    twin(&t, "d/a.jpg", b"aa");
    let debris = t.file("dst/d/.df-tmp-4242-7", b"half a photo");
    let kept = t.file("dst/d/theirs.txt", b"only there");
    let plan = quick(&[dir], &t.join("dst"));

    let item = plan
        .items
        .iter()
        .find(|item| plan.label(item) == "d/.df-tmp-4242-7")
        .unwrap();
    assert_eq!(item.class, Class::Extra);
    assert!(plan.is_debris(item));
    assert!(!plan.in_sync(Mode::Update), "there is something to clear");
    assert_eq!(
        plan.listed(Mode::Update)
            .map(|item| plan.label(item))
            .collect::<Vec<_>>(),
        ["d/.df-tmp-4242-7"],
        "an update lists the debris and no other extra"
    );
    assert_eq!(plan.removals(Mode::Update).len(), 1);

    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
    assert_eq!(report.problems(), 0, "{report:?}");
    assert_eq!(report.removed, 1);
    assert!(!debris.exists(), "our own leftover is gone");
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        b"only there",
        "theirs is not"
    );
}

#[test]
fn simple_case_folding_is_one_character_for_one() {
    for (a, b) in [
        ("Photo.JPG", "photo.jpg"),
        ("ΣΊΣΥΦΟΣ", "σίσυφος"),
        ("ẞ", "ß"),
        ("\u{212A}elvin", "kelvin"),
        ("µs", "μs"),
        ("ſtraight", "straight"),
        ("ÅNGSTRÖM", "ångström"),
    ] {
        assert_eq!(plan::fold(a), plan::fold(b), "{a} and {b}");
    }
    for (a, b) in [
        ("STRASSE", "straße"),
        ("İstanbul", "istanbul"),
        ("photo.jpg", "photo.jpeg"),
    ] {
        assert_ne!(plan::fold(a), plan::fold(b), "{a} and {b}");
    }
}

#[test]
fn a_twin_is_a_source_name_listed_only_in_the_destinations_case() {
    let names = |list: &[&str]| -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    };
    let ours = names(&["photo.jpg", "notes.txt", "A.txt", "a.txt"]);
    let theirs = names(&["Photo.JPG", "notes.txt", "NOTES.TXT", "Other.JPG", "A.TXT"]);
    let twins = plan::case_twins(&ours, &theirs);
    assert_eq!(twins.len(), 1, "{twins:?}");
    assert_eq!(twins[std::ffi::OsStr::new("Photo.JPG")], "photo.jpg");
    // `NOTES.TXT` beside an exact `notes.txt` is a second file; `Other.JPG`
    // is nobody's; `A.TXT` folds to two source names and twins neither.
}

#[test]
fn a_file_a_case_folding_card_lists_in_its_own_case_is_rewritten_not_trashed() {
    let t = TempTree::new("sync-case");
    let dir = t.dir("src/d");
    twin(&t, "d/photo.jpg", b"the only copy");
    t.file("dst/d/stray.txt", b"a real extra");
    let listed_dir = t.join("dst/d");
    // A FAT card lists what it stores, `Photo.JPG`, while a `stat` of
    // `photo.jpg` finds the same file — which the real file here gives.
    let list = |dir: &Path| -> crate::Result<Vec<std::ffi::OsString>> {
        if dir == listed_dir {
            Ok(vec!["Photo.JPG".into(), "stray.txt".into()])
        } else {
            let mut names: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            Ok(names)
        }
    };
    let mut plan = plan::walk(
        &[dir],
        &t.join("dst"),
        SyncOptions::default(),
        &|| false,
        &|_| {},
        &list,
    )
    .unwrap();
    assert_eq!(class_of(&plan, "d/photo.jpg"), Class::Changed, "rewritten");
    let extras: Vec<String> = plan
        .items
        .iter()
        .filter(|item| item.class == Class::Extra)
        .map(|item| plan.label(item))
        .collect();
    assert_eq!(extras, ["d/stray.txt"], "the twin is no extra");

    plan.removal = Removal::Delete;
    let report = execute(&plan, Mode::Mirror, Verify::Copied, &TaskCtx::detached());
    assert_eq!(report.problems(), 0, "{report:?}");
    assert_eq!((report.copied, report.removed), (1, 1));
    assert_eq!(
        std::fs::read(t.join("dst/d/photo.jpg")).unwrap(),
        b"the only copy"
    );
}

#[test]
fn a_source_that_cannot_be_read_back_is_named_as_itself() {
    use std::os::unix::fs::PermissionsExt;
    let t = TempTree::new("sync-verify-source");
    let dir = t.dir("src/d");
    let src = t.file("src/d/a.bin", b"aaaa");
    let plan = quick(&[dir], &t.dir("dst"));
    let locked = src.clone();
    execute::before_verify(move || {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    });
    let report = execute(&plan, Mode::Update, Verify::Copied, &TaskCtx::detached());
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(report.verify_failures.len(), 1, "{report:?}");
    assert_eq!(report.verify_failures[0].0, src, "the source, not its copy");
    assert!(
        report.verify_failures[0].1.contains("ermission"),
        "{report:?}"
    );
}

#[test]
fn a_folder_that_cannot_be_made_fails_once_not_once_per_file_in_it() {
    let t = TempTree::new("sync-failed-dir");
    let dir = t.dir("src/d");
    t.file("src/d/new/a", b"a");
    t.file("src/d/new/b", b"b");
    t.file("src/d/new/deeper/c", b"c");
    let plan = quick(&[dir], &t.dir("dst"));
    // Something takes the name between the plan and the run.
    t.file("dst/d/new", b"a file now");
    let report = execute(
        &plan,
        Mode::Update,
        Verify::Everything,
        &TaskCtx::detached(),
    );
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(report.errors[0].0, t.join("dst/d/new"));
    assert!(report.verify_failures.is_empty(), "{report:?}");
}
