//! Fixtures for the parts that are ours, and one live repository for the part
//! that is not.
//!
//! The porcelain parser is exercised against hand-built byte strings rather than
//! against `git status`, because the point of the fixtures is the cases a real
//! repository is *hard* to produce on demand: a filename with a newline in it, a
//! rename whose source is in another directory, a truncated stream. The
//! integration test at the bottom then proves the fixtures describe the real
//! format, and skips itself where git is not installed.

#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::ops::fixture::TempTree;

fn root() -> PathBuf {
    PathBuf::from("/repo")
}

/// Build a NUL-framed porcelain stream from records.
fn framed(records: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in records {
        out.extend_from_slice(r);
        out.push(0);
    }
    out
}

// ── the porcelain parser ────────────────────────────────────────────────────

#[test]
fn ordinary_entries_reduce_to_one_dot_each() {
    let bytes = framed(&[
        b"1 .M N... 100644 100644 100644 aaa bbb src/main.rs",
        b"1 M. N... 100644 100644 100644 aaa bbb staged.txt",
        b"1 A. N... 000000 100644 100644 aaa bbb new.txt",
        b"1 .D N... 100644 100644 000000 aaa bbb gone.txt",
        b"1 .T N... 100644 120000 120000 aaa bbb link.txt",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());

    assert_eq!(
        data.status_for(Path::new("/repo/src/main.rs")),
        Some(FileStatus::Modified)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/staged.txt")),
        Some(FileStatus::Modified)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/new.txt")),
        Some(FileStatus::Added)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/gone.txt")),
        Some(FileStatus::Deleted)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/link.txt")),
        Some(FileStatus::Typechange)
    );
    assert_eq!(data.counts.staged, 2);
    assert_eq!(data.counts.unstaged, 3);
    assert!(!data.truncated);
}

#[test]
fn the_work_tree_wins_when_both_sides_changed() {
    // Staged as added, then modified again in the work tree. The dot the user
    // needs is the newer one.
    let bytes = framed(&[b"1 AM N... 000000 100644 100644 aaa bbb both.txt"]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/both.txt")),
        Some(FileStatus::Modified)
    );
    assert_eq!(data.counts.staged, 1);
    assert_eq!(data.counts.unstaged, 1);
}

#[test]
fn a_rename_is_two_fields_and_marks_both_ends() {
    let bytes = framed(&[
        b"2 R. N... 100644 100644 100644 aaa bbb R100 new/place.rs",
        b"old/place.rs",
        b"1 .M N... 100644 100644 100644 aaa bbb after.txt",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());

    assert_eq!(
        data.status_for(Path::new("/repo/new/place.rs")),
        Some(FileStatus::Renamed)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/old/place.rs")),
        Some(FileStatus::Deleted),
        "the source row still exists in the old directory until a repaint"
    );
    // The record *after* the rename must still parse: the second field has to be
    // consumed or the framing slips.
    assert_eq!(
        data.status_for(Path::new("/repo/after.txt")),
        Some(FileStatus::Modified)
    );
    assert_eq!(data.counts.staged, 1);
}

#[test]
fn a_copy_reads_as_a_rename() {
    let bytes = framed(&[
        b"2 C. N... 100644 100644 100644 aaa bbb C75 copy.rs",
        b"orig.rs",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/copy.rs")),
        Some(FileStatus::Renamed)
    );
}

#[test]
fn unmerged_entries_are_conflicts() {
    let bytes = framed(&[
        b"u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.txt",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/conflict.txt")),
        Some(FileStatus::Conflict)
    );
    assert_eq!(data.counts.conflicted, 1);
    assert!(!data.counts.is_clean());
}

#[test]
fn untracked_and_ignored_are_their_own_entry_types() {
    let bytes = framed(&[b"? scratch.txt", b"! target/debug/df", b"! .env"]);
    let data = parse_porcelain_v2(&bytes, &root());

    assert_eq!(
        data.status_for(Path::new("/repo/scratch.txt")),
        Some(FileStatus::Untracked)
    );
    assert!(data.is_ignored(Path::new("/repo/target/debug/df")));
    assert!(data.is_ignored(Path::new("/repo/.env")));
    assert_eq!(data.counts.untracked, 1);
    assert_eq!(
        data.counts.staged + data.counts.unstaged,
        0,
        "ignored files are not dirt"
    );
}

#[test]
fn ignored_does_not_roll_up_but_everything_else_does() {
    let bytes = framed(&[
        b"! build/artifacts/a.o",
        b"1 .M N... 100644 100644 100644 aaa bbb deep/nested/dir/file.rs",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());

    assert_eq!(
        data.status_for(Path::new("/repo/build")),
        None,
        "a directory holding an ignored file is not itself ignored"
    );
    for dir in [
        "/repo/deep/nested/dir",
        "/repo/deep/nested",
        "/repo/deep",
        "/repo",
    ] {
        assert_eq!(
            data.status_for(Path::new(dir)),
            Some(FileStatus::Modified),
            "{dir} should carry the rollup"
        );
    }
    assert_eq!(
        data.status_for(Path::new("/")),
        None,
        "the rollup stops at the repository root"
    );
}

#[test]
fn the_worst_status_wins_a_rollup() {
    let bytes = framed(&[
        b"? src/scratch.txt",
        b"1 .M N... 100644 100644 100644 aaa bbb src/a.rs",
        b"u UU N... 100644 100644 100644 100644 a b c src/deep/c.rs",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/src")),
        Some(FileStatus::Conflict)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/src/deep")),
        Some(FileStatus::Conflict)
    );
}

#[test]
fn a_directory_entry_keeps_its_own_status_and_loses_the_slash() {
    // `-unormal` collapses an untracked directory to one entry with a trailing
    // slash. It has to land in `dirs`, not `files`, and without the slash.
    let bytes = framed(&[b"? newdir/"]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/newdir")),
        Some(FileStatus::Untracked)
    );
    assert!(data.files.is_empty());
}

#[test]
fn a_collapsed_directory_is_inherited_by_everything_inside_it() {
    // What `-unormal` actually produces: one entry for the directory, none for
    // the files. Every row inside still needs a dot.
    let bytes = framed(&[b"? newdir/", b"! target/"]);
    let data = parse_porcelain_v2(&bytes, &root());

    assert_eq!(
        data.status_for(Path::new("/repo/newdir/a/b/c.txt")),
        Some(FileStatus::Untracked)
    );
    assert!(data.is_ignored(Path::new("/repo/target/debug/df")));
    assert_eq!(
        data.status_for(Path::new("/repo/tracked.rs")),
        None,
        "inheritance only reaches inside the collapsed directory"
    );
}

#[test]
fn a_rollup_does_not_leak_downwards() {
    let bytes = framed(&[b"1 .M N... 100644 100644 100644 aaa bbb src/a.rs"]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/src")),
        Some(FileStatus::Modified)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/src/clean.rs")),
        None,
        "a modified directory does not make its clean files modified"
    );
}

#[test]
fn a_directory_can_be_ignored_and_a_rollup_at_once() {
    let bytes = framed(&[
        b"! target/",
        b"1 .M N... 100644 100644 100644 aaa bbb target/keep.rs",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());
    // The file map answers first, so the file keeps its own dot...
    assert_eq!(
        data.status_for(Path::new("/repo/target/keep.rs")),
        Some(FileStatus::Modified)
    );
    // ...and the directory carries the worse of "ignored" and the rollup.
    assert_eq!(
        data.status_for(Path::new("/repo/target")),
        Some(FileStatus::Modified)
    );
}

#[test]
fn weird_filenames_survive_z_framing() {
    // A newline, a space, a tab, a quote, and a `->` that would break v1
    // porcelain outright. None of them need escaping under `-z`.
    let mut record = b"1 .M N... 100644 100644 100644 aaa bbb ".to_vec();
    record.extend_from_slice(b"a\nb c\td \"e\" -> f.txt");
    let bytes = framed(&[&record, b"? plain.txt"]);
    let data = parse_porcelain_v2(&bytes, &root());

    assert_eq!(
        data.status_for(&root().join("a\nb c\td \"e\" -> f.txt")),
        Some(FileStatus::Modified)
    );
    assert_eq!(
        data.status_for(Path::new("/repo/plain.txt")),
        Some(FileStatus::Untracked),
        "the record after a newline-bearing path still parses"
    );
}

#[test]
fn non_utf8_filenames_still_get_a_dot() {
    let mut record = b"1 .M N... 100644 100644 100644 aaa bbb ".to_vec();
    record.extend_from_slice(&[0xff, 0xfe, b'.', b't', b'x', b't']);
    let bytes = framed(&[&record]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(data.files.len(), 1);
    assert!(data.files.values().all(|s| *s == FileStatus::Modified));
}

#[test]
fn headers_are_parsed_and_are_not_paths() {
    let bytes = framed(&[
        b"# branch.oid 1111111111111111111111111111111111111111",
        b"# branch.head main",
        b"# branch.upstream origin/main",
        b"# branch.ab +3 -1",
        b"1 .M N... 100644 100644 100644 aaa bbb f.txt",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(data.branch.as_deref(), Some("main"));
    assert_eq!(data.ahead_behind, Some((3, 1)), "both are counts, not signs");
    assert_eq!(data.files.len(), 1);
}

#[test]
fn a_detached_head_header_is_not_a_branch_name() {
    let bytes = framed(&[b"# branch.head (detached)"]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(data.branch, None);
}

#[test]
fn a_truncated_stream_keeps_what_parsed() {
    let mut bytes = framed(&[b"1 .M N... 100644 100644 100644 aaa bbb a.txt"]);
    // A second record cut off mid-path, with no terminator.
    bytes.extend_from_slice(b"1 .M N... 100644 100644 100644 aaa bbb b");
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/a.txt")),
        Some(FileStatus::Modified)
    );
    // The partial record parsed as far as it went, which is fine — it names a
    // path that may not exist, and a dot on a nonexistent row is invisible.
    assert!(data.files.len() <= 2);
}

#[test]
fn malformed_records_are_skipped_not_fatal() {
    let bytes = framed(&[
        b"1 short",
        b"2 R. N... 100644 100644 100644 aaa bbb R100",
        b"",
        b"x who knows",
        b"?",
        b"1 .M N... 100644 100644 100644 aaa bbb good.txt",
    ]);
    let data = parse_porcelain_v2(&bytes, &root());
    assert_eq!(
        data.status_for(Path::new("/repo/good.txt")),
        Some(FileStatus::Modified)
    );
}

#[test]
fn the_entry_cap_truncates_rather_than_growing() {
    let mut records: Vec<Vec<u8>> = Vec::new();
    for i in 0..(MAX_STATUS_ENTRIES / 4 + 10) {
        records.push(format!("? f{i}").into_bytes());
    }
    let refs: Vec<&[u8]> = records.iter().map(|r| r.as_slice()).collect();
    let bytes = framed(&refs);
    let data = parse_porcelain_v2(&bytes, &root());
    assert!(data.files.len() + data.dirs.len() <= MAX_STATUS_ENTRIES + 1);
}

#[test]
fn status_ranking_is_total_and_conflict_is_loudest() {
    let all = [
        FileStatus::Ignored,
        FileStatus::Untracked,
        FileStatus::Added,
        FileStatus::Deleted,
        FileStatus::Renamed,
        FileStatus::Typechange,
        FileStatus::Modified,
        FileStatus::Conflict,
    ];
    let mut ranks: Vec<u8> = all.iter().map(|s| s.rank()).collect();
    ranks.sort_unstable();
    ranks.dedup();
    assert_eq!(ranks.len(), all.len(), "ranks must be distinct");
    for s in all {
        assert_eq!(s.worse(FileStatus::Conflict), FileStatus::Conflict);
        assert_eq!(FileStatus::Ignored.worse(s), s);
    }
    assert!(!FileStatus::Ignored.rolls_up());
    assert!(all.iter().filter(|s| s.rolls_up()).count() == all.len() - 1);
}

// ── HEAD and the gitfile redirect ───────────────────────────────────────────

#[test]
fn head_parses_a_branch() {
    assert_eq!(
        parse_head(b"ref: refs/heads/main\n"),
        Some(Head::Branch("main".into()))
    );
    assert_eq!(
        parse_head(b"ref: refs/heads/feature/nested/name\n"),
        Some(Head::Branch("feature/nested/name".into()))
    );
}

#[test]
fn head_parses_a_detached_hash_short() {
    let head = parse_head(b"9c4a2b1f0e5d6c7b8a90112233445566778899aa\n").unwrap();
    assert_eq!(head, Head::Detached("9c4a2b1".into()));
    assert!(head.is_detached());
    assert_eq!(head.label().len(), SHORT_HASH);
}

#[test]
fn head_keeps_a_symbolic_ref_that_is_not_a_branch() {
    assert_eq!(
        parse_head(b"ref: refs/remotes/origin/main"),
        Some(Head::Branch("refs/remotes/origin/main".into()))
    );
}

#[test]
fn head_rejects_junk() {
    assert_eq!(parse_head(b""), None);
    assert_eq!(parse_head(b"   \n"), None);
    assert_eq!(parse_head(b"ref:"), None);
    assert_eq!(parse_head(b"not a hash at all"), None);
    assert_eq!(parse_head(&[0xff, 0xfe]), None);
}

#[test]
fn gitfile_redirects_are_parsed() {
    assert_eq!(
        parse_gitfile(b"gitdir: /home/u/proj/.git/worktrees/wt\n").as_deref(),
        Some("/home/u/proj/.git/worktrees/wt")
    );
    assert_eq!(
        parse_gitfile(b"gitdir: ../.git/modules/sub").as_deref(),
        Some("../.git/modules/sub")
    );
    assert_eq!(parse_gitfile(b"gitdir:\n"), None);
    assert_eq!(parse_gitfile(b"something else\n"), None);
}

#[test]
fn a_gitfile_worktree_is_followed_to_its_head() {
    let t = TempTree::new("git-worktree");
    // The real repository...
    let real = t.dir("proj/.git/worktrees/wt");
    std::fs::write(real.join("HEAD"), b"ref: refs/heads/side\n").unwrap();
    // ...and a linked worktree whose `.git` is a file pointing at it.
    let wt = t.dir("wt");
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();

    assert_eq!(git_dir(&wt).as_deref(), Some(real.as_path()));
    assert_eq!(branch(&wt).as_deref(), Some("side"));
    assert_eq!(repo_root(&wt).as_deref(), Some(wt.as_path()));
}

#[test]
fn a_relative_gitfile_resolves_against_the_worktree() {
    let t = TempTree::new("git-relative");
    let real = t.dir("store/gitdir");
    std::fs::write(real.join("HEAD"), b"ref: refs/heads/rel\n").unwrap();
    let wt = t.dir("work");
    std::fs::write(wt.join(".git"), b"gitdir: ../store/gitdir\n").unwrap();

    assert_eq!(branch(&wt).as_deref(), Some("rel"));
}

#[test]
fn repo_root_walks_up_from_a_file() {
    let t = TempTree::new("git-root");
    let repo = t.dir("r");
    t.dir("r/.git");
    let deep = t.file("r/a/b/c/file.txt", b"x");

    assert_eq!(repo_root(&deep).as_deref(), Some(repo.as_path()));
    assert_eq!(repo_root(&t.join("r/a/b")).as_deref(), Some(repo.as_path()));
    assert_eq!(repo_root(repo.as_path()).as_deref(), Some(repo.as_path()));
}

#[test]
fn a_directory_outside_any_repository_finds_nothing() {
    // `/proc` has no `.git` anywhere above it, and unlike a temp dir it cannot
    // accidentally be inside this checkout.
    assert_eq!(repo_root(Path::new("/proc/self")), None);
}

#[test]
fn git_dir_of_a_plain_repo_is_dot_git() {
    let t = TempTree::new("git-dir");
    let repo = t.dir("r");
    let dot = t.dir("r/.git");
    assert_eq!(git_dir(&repo).as_deref(), Some(dot.as_path()));
    assert_eq!(git_dir(&t.join("nope")), None);
}

// ── the cache ───────────────────────────────────────────────────────────────

#[test]
fn a_disabled_cache_answers_nothing_and_never_spawns() {
    let git = Git::disabled();
    assert!(!git.available());
    let t = TempTree::new("git-off");
    let repo = t.dir("r");
    t.dir("r/.git");
    git.refresh(&repo);
    assert!(!git.is_pending(&repo));
    assert!(git.status(&repo).is_none());
    assert_eq!(git.generation(), 0);
    // The `.git`-reading half still works with the subprocess half switched off.
    assert_eq!(git.repo_root(&repo).as_deref(), Some(repo.as_path()));
}

#[test]
fn repo_root_lookups_are_memoized_including_the_misses() {
    let git = Git::disabled();
    let t = TempTree::new("git-memo");
    let plain = t.dir("plain");
    assert_eq!(git.repo_root(&plain), None);
    // Creating the repo now must not change the memoized answer — which is the
    // observable proof that the second call did not walk the filesystem.
    t.dir("plain/.git");
    assert_eq!(git.repo_root(&plain), None);
    git.clear();
    assert_eq!(git.repo_root(&plain).as_deref(), Some(plain.as_path()));
}

// ── against a real repository ───────────────────────────────────────────────

fn git_installed() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_in(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        // A test must not inherit the developer's identity, hooks or templates.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "df")
        .env("GIT_AUTHOR_EMAIL", "df@example.invalid")
        .env("GIT_COMMITTER_NAME", "df")
        .env("GIT_COMMITTER_EMAIL", "df@example.invalid")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

#[test]
fn a_real_repository_reports_what_the_fixtures_describe() {
    if !git_installed() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let t = TempTree::new("git-live");
    let repo = t.dir("live");
    git_in(&repo, &["init", "-q", "-b", "trunk"]);

    std::fs::write(repo.join(".gitignore"), b"ignored/\n*.log\n").unwrap();
    std::fs::create_dir_all(repo.join("src/deep")).unwrap();
    std::fs::write(repo.join("src/tracked.rs"), b"fn main() {}\n").unwrap();
    std::fs::write(repo.join("src/deep/moved.rs"), b"// content\n").unwrap();
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "one"]);

    // Now make one of everything.
    std::fs::write(repo.join("src/tracked.rs"), b"fn main() { }\n").unwrap();
    std::fs::write(repo.join("src/deep/fresh.rs"), b"new\n").unwrap();
    std::fs::create_dir_all(repo.join("ignored")).unwrap();
    std::fs::write(repo.join("ignored/junk.o"), b"\0\0").unwrap();
    std::fs::write(repo.join("build.log"), b"noise\n").unwrap();
    std::fs::write(repo.join("staged.rs"), b"staged\n").unwrap();
    git_in(&repo, &["add", "staged.rs"]);
    std::fs::rename(
        repo.join("src/deep/moved.rs"),
        repo.join("src/deep/elsewhere.rs"),
    )
    .unwrap();
    git_in(&repo, &["add", "-A", "src/deep"]);

    let data = status_blocking(&repo).expect("git status");

    assert_eq!(data.branch.as_deref(), Some("trunk"));
    assert_eq!(
        data.status_for(&repo.join("src/tracked.rs")),
        Some(FileStatus::Modified)
    );
    assert_eq!(
        data.status_for(&repo.join("staged.rs")),
        Some(FileStatus::Added)
    );
    assert_eq!(
        data.status_for(&repo.join("src/deep/elsewhere.rs")),
        Some(FileStatus::Renamed)
    );
    assert!(
        data.is_ignored(&repo.join("build.log")),
        "an ignored file is reported individually"
    );
    assert!(
        data.is_ignored(&repo.join("ignored/junk.o")),
        "--ignored=matching lists inside an ignored directory: {:?}",
        data.dirs.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        data.status_for(&repo.join("src")),
        Some(FileStatus::Modified),
        "the rollup reaches src/"
    );
    assert!(!data.counts.is_clean());
    assert!(data.counts.staged >= 2, "{:?}", data.counts);
    assert!(!data.truncated);

    // And the `.git`-reading half agrees with the porcelain about the branch.
    assert_eq!(branch(&repo).as_deref(), Some("trunk"));
    assert_eq!(repo_root(&repo.join("src/deep")).as_deref(), Some(&*repo));
}

#[test]
fn the_cache_fills_in_asynchronously_and_bumps_the_generation() {
    if !git_installed() {
        eprintln!("skipping: git is not installed");
        return;
    }
    let t = TempTree::new("git-async");
    let repo = t.dir("async");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), b"x\n").unwrap();

    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&hits);
    let git = Git::start(std::sync::Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));

    // The very first ask never blocks and never has an answer.
    assert!(git.ensure(&repo.join("a.txt")).is_none());
    assert_eq!(git.generation(), 0);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while git.status(&repo).is_none() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }

    let status = git.status(&repo).expect("a status landed");
    assert_eq!(status.root, repo);
    assert_eq!(status.branch(), Some("main"));
    assert_eq!(
        status.status_for(&repo.join("a.txt")),
        Some(FileStatus::Untracked)
    );
    assert_eq!(status.counts().untracked, 1);
    assert_eq!(git.generation(), 1);
    assert!(hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    assert!(!git.is_pending(&repo));

    // And a second ask is served from the cache.
    assert!(git.ensure(&repo.join("a.txt")).is_some());
    assert_eq!(git.generation(), 1);
}
