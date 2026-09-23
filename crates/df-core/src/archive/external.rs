//! Extraction by somebody else's program, for the formats this crate cannot
//! read.
//!
//! [`super`]'s essay draws the line: zip and tar are framing, and parsing them
//! here is what lets the path-safety rules be applied by the code that reads
//! the names. 7z and rar are not framing — they are whole compression suites
//! with solid blocks, filters and a volume layer — and a multi-part set of any
//! format is a second container wrapped round the first. Nothing here is going
//! to grow a reader for those. So for them the extraction is handed to 7-Zip,
//! or to `bsdtar` when 7-Zip is not installed, and this module's job is to run
//! that program the way every other job runs: on the pool, cancellable, with
//! its failure turned into one sentence.
//!
//! ## What is inherited
//!
//! The price is the one the essay names: *their* path handling rather than
//! ours. Both are good at it. `bsdtar` refuses `..` and absolute names unless
//! told otherwise, and will not write through a symlink it just extracted;
//! 7-Zip strips both and has refused "dangerous" links since 21.x. The plan's
//! per-entry refusal list is not available — neither program says which
//! entries it dropped in a form worth parsing — so an external extraction
//! reports what it made by looking at the destination afterwards.
//!
//! ## Found by `PATH`, not by running it
//!
//! [`super::ArchiveFormat::is_available`] asks a decompressor for its
//! `--version`. 7-Zip has no such flag and exits 7 for it, so here the question
//! is the one the spawn will ask anyway — is there an executable of that name
//! on `PATH` — answered with a few `stat`s and no process.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

/// Which program an [`Extractor`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractorKind {
    /// `7z`: reads everything, volumes included.
    SevenZip,
    /// `bsdtar` (libarchive): reads 7z, rar, cab, iso and every compressed
    /// tar, but not a set split across files.
    Bsdtar,
}

/// A program on this machine that can extract what this crate cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extractor {
    pub kind: ExtractorKind,
    /// Where it was found on `PATH`.
    pub program: PathBuf,
}

/// The extractor to use, in order of preference: 7-Zip, then `bsdtar`.
///
/// 7-Zip first because it is the one that also reads multi-part sets, so a
/// machine with both never has to be asked which one a set needs.
pub fn external_extractor() -> Option<Extractor> {
    extractor_on(&std::env::var_os("PATH")?)
}

/// [`external_extractor`] over a given `PATH`, so the preference order is
/// testable without touching the process environment.
pub fn extractor_on(path: &OsStr) -> Option<Extractor> {
    [
        (ExtractorKind::SevenZip, "7z"),
        (ExtractorKind::Bsdtar, "bsdtar"),
    ]
    .into_iter()
    .find_map(|(kind, name)| {
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
            .map(|program| Extractor { kind, program })
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

impl Extractor {
    /// What to call it in a message.
    pub fn label(&self) -> &'static str {
        match self.kind {
            ExtractorKind::SevenZip => "7-Zip",
            ExtractorKind::Bsdtar => "bsdtar",
        }
    }

    /// Whether it can read an archive split across several files. Only 7-Zip:
    /// libarchive reads one stream.
    pub fn reads_volumes(&self) -> bool {
        self.kind == ExtractorKind::SevenZip
    }

    /// The arguments that extract `archive` into `dest`, which must exist.
    ///
    /// `overwrite` is [`super::ExtractPlan::overwrite`]'s policy spelled in each
    /// program's flags. 7-Zip: `-aoa` replaces, `-aos` skips what is already
    /// there — `-y` alone would overwrite, which is not what an extraction of a
    /// single archive may do. `bsdtar` overwrites by default and `-k` keeps.
    ///
    /// `--` before the archive for 7-Zip, whose switches are recognised
    /// anywhere on the line; the paths handed here are absolute in practice,
    /// but a name beginning with `-` should never be able to become a switch.
    pub fn args(&self, archive: &Path, dest: &Path, overwrite: bool) -> Vec<OsString> {
        match self.kind {
            ExtractorKind::SevenZip => {
                let mut out = OsString::from("-o");
                out.push(dest.as_os_str());
                vec![
                    "x".into(),
                    "-y".into(),
                    if overwrite { "-aoa" } else { "-aos" }.into(),
                    out,
                    "--".into(),
                    archive.as_os_str().to_owned(),
                ]
            }
            ExtractorKind::Bsdtar => {
                let mut args: Vec<OsString> = vec![
                    "-x".into(),
                    "-f".into(),
                    archive.as_os_str().to_owned(),
                    "-C".into(),
                    dest.as_os_str().to_owned(),
                ];
                if !overwrite {
                    args.push("-k".into());
                }
                args
            }
        }
    }

    /// The arguments that join a byte-split set (`name.tar.gz.001`, …) back
    /// into one file inside `into`. 7-Zip only — `-tsplit` is its reader for
    /// exactly this — and `None` for anything that cannot.
    pub fn join_args(&self, head: &Path, into: &Path) -> Option<Vec<OsString>> {
        if self.kind != ExtractorKind::SevenZip {
            return None;
        }
        let mut out = OsString::from("-o");
        out.push(into.as_os_str());
        Some(vec![
            "x".into(),
            "-tsplit".into(),
            "-y".into(),
            out,
            "--".into(),
            head.as_os_str().to_owned(),
        ])
    }
}

/// How often a running extractor is checked for having finished, or for
/// having been cancelled. Short enough that a cancel feels immediate, long
/// enough that a ten-minute extraction wakes this thread a few thousand times
/// rather than a few million.
const POLL: Duration = Duration::from_millis(50);

/// How much of each output stream is kept for the error message. The rest is
/// read and dropped — it has to be read, or a chatty extractor blocks on a
/// full pipe and never finishes.
const KEEP_OUTPUT: u64 = 64 * 1024;

/// What running an extractor came to.
#[derive(Debug)]
pub struct Ran {
    /// `None` when it was killed for a cancel.
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    pub cancelled: bool,
}

impl Ran {
    pub fn succeeded(&self) -> bool {
        self.status.is_some_and(|s| s.success())
    }
}

/// Run `program` with `args` to completion, or until the task is cancelled —
/// in which case it is killed, which is the whole of "cancel" for somebody
/// else's process.
///
/// stdin is `/dev/null`, so a program that would stop to ask something (7-Zip's
/// password prompt) reads end-of-file and gives up instead of waiting for ever
/// on a terminal nobody is looking at. stdout and stderr are drained on a thread
/// each: waiting on the child while a pipe fills is the classic deadlock.
pub fn run(program: &Path, args: &[OsString], ctx: &TaskCtx) -> Result<Ran> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DfError::io(program, e))?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DfError::io(program, e));
            }
        }
        if ctx.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            cancelled = true;
            break None;
        }
        std::thread::sleep(POLL);
    };
    let collect = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| {
        handle
            .and_then(|h| h.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    Ok(Ran {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
        cancelled,
    })
}

/// Read a pipe to its end on a thread of its own, keeping the first
/// [`KEEP_OUTPUT`] bytes.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let _ = pipe.by_ref().take(KEEP_OUTPUT).read_to_end(&mut kept);
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        kept
    })
}

/// The one line worth showing from a failed extractor.
///
/// "The first line of stderr", with the noise both programs put before it
/// skipped: 7-Zip opens with a blank line, a bare `ERRORS:` heading, and
/// `ERROR: <the archive's own path>` before the sentence that says what went
/// wrong. `bsdtar` prefixes its name. A password prompt the child could not
/// answer is named as what it is.
pub fn failure_line(ran: &Ran, archive: &Path) -> String {
    if ran.stdout.contains("Enter password") || ran.stderr.contains("Enter password") {
        return "encrypted — it needs a password".to_string();
    }
    let full = archive.to_string_lossy();
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let echo = |line: &str| line == full || line == name;
    let meaningful = ran.stderr.lines().map(str::trim).find(|line| {
        !line.is_empty()
            && !matches!(*line, "ERRORS:" | "WARNINGS:" | "ERROR:" | "Break signaled")
            && !echo(line)
            && !line.strip_prefix("ERROR: ").is_some_and(echo)
    });
    if let Some(line) = meaningful {
        return line.strip_prefix("bsdtar: ").unwrap_or(line).to_string();
    }
    if let Some(line) = ran.stderr.lines().map(str::trim).find(|l| !l.is_empty()) {
        return line.to_string();
    }
    match ran.status.and_then(|s| s.code()) {
        Some(code) => format!("the extractor exited with status {code}"),
        None => "the extractor was stopped".to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;

    fn fake(t: &TempTree, rel: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = t.file(rel, b"#!/bin/sh\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn seven_zip_is_preferred_and_bsdtar_is_the_fallback() {
        let t = TempTree::new("extractor-path");
        let a = t.dir("a");
        let b = t.dir("b");
        let empty = t.dir("empty");
        fake(&t, "a/bsdtar");
        fake(&t, "b/7z");
        // Not executable: not a program, whatever it is called.
        t.file("empty/7z", b"text");

        let path = std::env::join_paths([&a, &b]).unwrap();
        let found = extractor_on(&path).unwrap();
        assert_eq!(
            found.kind,
            ExtractorKind::SevenZip,
            "7-Zip wins wherever it is"
        );
        assert_eq!(found.program, b.join("7z"));

        let path = std::env::join_paths([&empty, &a]).unwrap();
        assert_eq!(extractor_on(&path).unwrap().kind, ExtractorKind::Bsdtar);

        let path = std::env::join_paths([&empty]).unwrap();
        assert_eq!(extractor_on(&path), None);
    }

    #[test]
    fn each_program_is_told_the_overwrite_policy_in_its_own_flags() {
        let seven = Extractor {
            kind: ExtractorKind::SevenZip,
            program: PathBuf::from("/usr/bin/7z"),
        };
        let args = |e: &Extractor, overwrite| -> Vec<String> {
            e.args(Path::new("/a/-x.7z"), Path::new("/out dir"), overwrite)
                .into_iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(&seven, false),
            vec!["x", "-y", "-aos", "-o/out dir", "--", "/a/-x.7z"]
        );
        assert_eq!(args(&seven, true)[2], "-aoa");

        let tar = Extractor {
            kind: ExtractorKind::Bsdtar,
            program: PathBuf::from("/usr/bin/bsdtar"),
        };
        assert_eq!(
            args(&tar, false),
            vec!["-x", "-f", "/a/-x.7z", "-C", "/out dir", "-k"]
        );
        assert!(!args(&tar, true).contains(&"-k".to_string()));

        assert!(seven.reads_volumes());
        assert!(!tar.reads_volumes());
        assert!(tar
            .join_args(Path::new("/a.001"), Path::new("/o"))
            .is_none());
        let join: Vec<String> = seven
            .join_args(Path::new("/a.tar.gz.001"), Path::new("/o"))
            .unwrap()
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            join,
            vec!["x", "-tsplit", "-y", "-o/o", "--", "/a.tar.gz.001"]
        );
    }

    fn ran(stderr: &str) -> Ran {
        Ran {
            status: None,
            stdout: String::new(),
            stderr: stderr.to_string(),
            cancelled: false,
        }
    }

    /// The real stderr of 7-Zip 26 and bsdtar 3.8, for the failures a person
    /// actually meets.
    #[test]
    fn the_failure_line_skips_the_headings_to_the_sentence() {
        let archive = Path::new("/dl/junk.7z");
        assert_eq!(
            failure_line(
                &ran("ERROR: /dl/junk.7z\n/dl/junk.7z\nOpen ERROR: Cannot open the file as [7z] archive\n\n\nERRORS:\nIs not archive\n"),
                archive
            ),
            "Open ERROR: Cannot open the file as [7z] archive"
        );
        assert_eq!(
            failure_line(
                &ran(
                    "\nERRORS:\nMissing volume : photos.z01\n\nERROR: Unavailable data : photos\n"
                ),
                Path::new("/dl/photos.zip")
            ),
            "Missing volume : photos.z01"
        );
        assert_eq!(
            failure_line(
                &ran("bsdtar: Error opening archive: Unrecognized archive format\n"),
                archive
            ),
            "Error opening archive: Unrecognized archive format"
        );
        let locked = Ran {
            stdout: "\nEnter password (will not be echoed):".to_string(),
            ..ran("\n\nBreak signaled\n")
        };
        assert_eq!(
            failure_line(&locked, archive),
            "encrypted — it needs a password"
        );
        assert_eq!(failure_line(&ran(""), archive), "the extractor was stopped");
    }

    #[test]
    fn a_running_extractor_is_killed_by_a_cancel() {
        let Some(sleep) = extractor_free_program("sleep") else {
            return;
        };
        let ctx = TaskCtx::detached();
        let flags = ctx.flags();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flags.cancel();
        });
        let started = std::time::Instant::now();
        let ran = run(&sleep, &["30".into()], &ctx).unwrap();
        canceller.join().unwrap();
        assert!(ran.cancelled);
        assert!(ran.status.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it was killed, not waited for"
        );
    }

    #[test]
    fn output_is_captured_and_the_status_reported() {
        let Some(sh) = extractor_free_program("sh") else {
            return;
        };
        let ran = run(
            &sh,
            &["-c".into(), "echo out; echo err >&2; exit 3".into()],
            &TaskCtx::detached(),
        )
        .unwrap();
        assert!(!ran.succeeded());
        assert_eq!(ran.status.and_then(|s| s.code()), Some(3));
        assert_eq!(ran.stdout.trim(), "out");
        assert_eq!(ran.stderr.trim(), "err");
    }

    /// A plain program from `PATH`, for exercising [`run`] without an archiver.
    fn extractor_free_program(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|c| is_executable(c))
    }
}
