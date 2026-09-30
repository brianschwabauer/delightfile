//! An archive under the cursor, as its list of members.
//!
//! What the preview pane shows for a zip, a tarball or a 7z: every member's
//! path inside the archive, in the order the archive stores them (the order
//! `unzip -l` prints, and often a meaningful one), under a header that says it
//! is an archive. This module is the half that runs on [`super::body`]'s
//! worker — it opens the file, and sometimes runs 7-Zip — plus the pure half:
//! the `7z -slt` parser and the counting.
//!
//! ## Three readers, tried in order
//!
//! 1. **df-core's own** ([`df_core::archive::list`]) for zip and for every tar
//!    it has a decompressor for. It needs no program, and it is the reader
//!    whose name rules everything else in the program relies on.
//! 2. **7-Zip as a decompressor**, for a compressed tar df-core cannot open: a
//!    `.tar.bz2`, or a `.tar.zst` on a machine without `zstd`. `7z l` on those
//!    lists the one compressed stream — the tar itself, nameless, which says
//!    nothing about what is in it — so instead `7z x -so` decompresses and
//!    df-core's tar parser reads the pipe, exactly as it reads `zstd -dc`.
//! 3. **7-Zip as a lister** (`7z l -ba -slt`), for everything else: 7z, rar,
//!    iso, deb, a single compressed file, and every multi-part set, whose head
//!    goes straight here because only 7-Zip reads volumes.
//!
//! When none of them can, the pane keeps its kind badge and says why under it
//! ([`INSTALL_7ZIP`], or the reader's own sentence).
//!
//! ## What is kept
//!
//! The first [`PREVIEW_ENTRIES`] members, and a count of all of them. A preview
//! is a glance: the listing a person navigates is `→` into the archive, which
//! is the tab's own tree. The count still runs to the end, because "and 41,211
//! more" is the fact that tells somebody the glance was not the whole archive.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use df_core::archive::{ArchiveError, ArchiveFormat, ArchiveTree, VolumeKind};

/// The most members a preview keeps: five hundred.
///
/// Twenty-odd screenfuls of the pane — far past where anybody stops scrolling
/// a glance — and small enough that holding them is nothing. What is past it is
/// counted, not kept, and the footer says how much there was.
pub const PREVIEW_ENTRIES: usize = 500;

/// The line under the badge when no reader on this machine can list the file.
pub const INSTALL_7ZIP: &str = "Install 7-Zip to list this archive";

/// How much of 7-Zip's stderr is kept for the one line the badge shows. The
/// sentence worth showing is in the first few lines; the rest is read and
/// dropped so a chatty failure cannot block on a full pipe.
const KEEP_STDERR: u64 = 16 * 1024;

/// One member, as the preview draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRow {
    /// The member's path inside the archive, `/`-separated with no leading
    /// slash: `src/main.rs`. A single top-level folder is kept, not stripped:
    /// it is what extracting the archive will create.
    pub name: String,
    pub is_dir: bool,
    /// Uncompressed bytes. `None` for a folder, and for a member whose size
    /// the archive does not record — a bzip2 stream says nothing about how
    /// big it will come out, and a `0 B` there would be a lie a person could
    /// act on.
    pub len: Option<u64>,
    pub encrypted: bool,
}

/// What a listing found: the rows the preview keeps, and the counts of
/// everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The format, lowercase — `zip`, `tar.gz`, `7z`. The header draws it in
    /// capitals.
    pub format: String,
    /// The first [`PREVIEW_ENTRIES`] members, in the archive's own order.
    pub rows: Vec<ArchiveRow>,
    /// Every member the listing found, folders included.
    pub total: usize,
    /// …of which files.
    pub files: usize,
    /// Uncompressed bytes across every file whose size is known.
    pub total_len: u64,
    /// Whether any member needs a password.
    pub encrypted: bool,
    /// Whether the listing reached the end of the archive. `false` when a cap
    /// or a read error stopped it, so [`Listing::total`] is a floor rather than
    /// the count.
    pub complete: bool,
}

impl Listing {
    /// A tree df-core listed, as rows.
    ///
    /// **Only the members the archive stores.** df-core synthesizes the
    /// folders a zip of bare file paths implies, so browsing into it has
    /// directories to walk; a listing of what is *in the file* leaves them
    /// out, the way `unzip -l` does. Their names are still on screen, as the
    /// front of every path under them.
    pub fn from_tree(tree: &ArchiveTree) -> Listing {
        let mut tally = Tally::new(tree.format().label().to_string());
        for entry in tree.all().iter().filter(|entry| !entry.synthesized) {
            tally.push(ArchiveRow {
                name: entry.path.clone(),
                is_dir: entry.is_dir,
                len: (!entry.is_dir).then_some(entry.len),
                encrypted: entry.encrypted,
            });
        }
        tally.finish(!tree.truncated())
    }
}

/// The counts, and the rows kept so far.
struct Tally {
    format: String,
    rows: Vec<ArchiveRow>,
    total: usize,
    files: usize,
    total_len: u64,
    encrypted: bool,
}

impl Tally {
    fn new(format: String) -> Tally {
        Tally {
            format,
            rows: Vec::new(),
            total: 0,
            files: 0,
            total_len: 0,
            encrypted: false,
        }
    }

    /// Count one member, and keep it while the preview has room. `false` once
    /// the count has reached df-core's own [`df_core::archive::MAX_ENTRIES`]:
    /// past that the caller stops reading, the same place df-core's own
    /// listing stops.
    fn push(&mut self, row: ArchiveRow) -> bool {
        self.total += 1;
        if !row.is_dir {
            self.files += 1;
            self.total_len = self.total_len.saturating_add(row.len.unwrap_or(0));
        }
        self.encrypted |= row.encrypted;
        if self.rows.len() < PREVIEW_ENTRIES {
            self.rows.push(row);
        }
        self.total < df_core::archive::MAX_ENTRIES
    }

    fn finish(self, complete: bool) -> Listing {
        Listing {
            format: self.format,
            rows: self.rows,
            total: self.total,
            files: self.files,
            total_len: self.total_len,
            encrypted: self.encrypted,
            complete,
        }
    }
}

// ── Reading one ─────────────────────────────────────────────────────────────

/// What a listing abandoned by `stop` answers. Nobody reads it: the worker
/// that asked has already dropped the job it belonged to.
const STOPPED: &str = "the listing was stopped";

/// List `path`, or say in one line why it cannot be.
///
/// Blocking — it reads the file, and may decompress all of it or run 7-Zip —
/// so it is [`super::body`]'s worker that calls it, never a frame. `stop` is
/// that worker saying the cursor has moved on: it is asked between reads of a
/// tar and between lines of 7-Zip's output, and a stopped listing kills
/// whatever program it had running and returns at once, rather than
/// decompressing the rest of an archive nobody is looking at.
pub fn list(path: &Path, stop: &dyn Fn() -> bool) -> Result<Listing, String> {
    list_with(path, seven_zip().as_deref(), stop)
}

/// [`list`], with the 7-Zip to use (or none) handed in, so both halves of every
/// fallback are testable on a machine that has it.
pub fn list_with(
    path: &Path,
    seven: Option<&Path>,
    stop: &dyn Fn() -> bool,
) -> Result<Listing, String> {
    // A multi-part set is read from its head by 7-Zip or not at all: the
    // pieces after the first are raw slices of one stream, and df-core reads
    // one file.
    if let Some(kind) = volume_head(path) {
        let Some(program) = seven else {
            return Err(INSTALL_7ZIP.to_string());
        };
        return list_with_7z(program, path, kind.label(), stop);
    }
    let error = match df_core::archive::list_until(path, stop) {
        Ok(tree) => return Ok(Listing::from_tree(&tree)),
        Err(error) => error,
    };
    // The file itself would not read. 7-Zip would not do better, and the
    // reason is the one worth showing.
    if matches!(error, ArchiveError::Io { .. } | ArchiveError::Read(_)) {
        return Err(super::readable(&error.to_string(), path));
    }
    let Some(program) = seven else {
        return Err(without_7z(&error));
    };
    if let Some(label) = tar_needing_a_decompressor(&error) {
        if let Some(tree) = tar_through_7z(program, path, stop) {
            let mut listing = Listing::from_tree(&tree);
            listing.format = name_label(path).unwrap_or(label);
            return Ok(listing);
        }
    }
    let format = name_label(path).unwrap_or_else(|| hint_label(&error));
    list_with_7z(program, path, format, stop).map_err(|reason| match error {
        // df-core recognised the format and found it broken: that sentence
        // names the format and the fault, which 7-Zip's rarely does.
        ArchiveError::Malformed { .. } | ArchiveError::Truncated { .. } => {
            sentence(&error.to_string())
        }
        _ => reason,
    })
}

/// 7-Zip on `PATH`, if it is there. `bsdtar` is not asked: it would list most
/// of the same formats, but 7-Zip is the one that also reads volumes, and one
/// fallback is one set of failure messages to get right.
fn seven_zip() -> Option<PathBuf> {
    df_core::archive::external_extractor()
        .filter(|extractor| extractor.kind == df_core::archive::ExtractorKind::SevenZip)
        .map(|extractor| extractor.program)
}

/// Whether `path` is the head of a multi-part set, and of which kind.
///
/// A numbered head (`backup.7z.001`, `movie.part1.rar`, `bundle.tar.gz.001`)
/// says so by its name alone. A `photos.zip` or `movie.rar` heads a set only
/// when its first numbered piece sits beside it — alone, it is an ordinary
/// archive — and that is two `stat`s rather than a listing of the directory,
/// because every zip the cursor lands on asks.
fn volume_head(path: &Path) -> Option<VolumeKind> {
    let name = path.file_name()?.to_str()?;
    let volume = df_core::archive::volume_of(name)?;
    if volume.index != volume.kind.head_index() {
        return None;
    }
    let beside = |ext: &str| {
        [ext.to_string(), ext.to_ascii_uppercase()]
            .iter()
            .any(|ext| {
                path.with_file_name(format!("{}.{ext}", volume.base))
                    .exists()
            })
    };
    let head = match volume.kind {
        VolumeKind::ZipSplit => beside("z01"),
        VolumeKind::RarOld => beside("r00"),
        _ => true,
    };
    head.then_some(volume.kind)
}

/// The badge's line when there is no 7-Zip to fall back on.
fn without_7z(error: &ArchiveError) -> String {
    match error {
        ArchiveError::NoDecompressor { binary, .. } => {
            format!("Install {binary} or 7-Zip to list this archive")
        }
        ArchiveError::Malformed { .. } | ArchiveError::Truncated { .. } => {
            sentence(&error.to_string())
        }
        _ => INSTALL_7ZIP.to_string(),
    }
}

/// When df-core recognised a compressed tar and could not open it, the format
/// to call it — the case [`tar_through_7z`] exists for.
fn tar_needing_a_decompressor(error: &ArchiveError) -> Option<String> {
    match error {
        ArchiveError::NoDecompressor { format, .. } => Some((*format).to_string()),
        // df-core names bzip2 by its compression alone; the file sniffed as a
        // bzip2 stream and is listed as a tar if the stream turns out to be one.
        ArchiveError::Unsupported { format: "bzip2" } => Some("tar.bz2".to_string()),
        _ => None,
    }
}

/// What df-core's refusal says the format is, for a file whose name does not.
fn hint_label(error: &ArchiveError) -> String {
    match error {
        ArchiveError::Unsupported { format } | ArchiveError::NoDecompressor { format, .. } => {
            (*format).to_string()
        }
        _ => "archive".to_string(),
    }
}

/// The format a name claims — `7z`, `tar.bz2`, `cbr`, `gz` — or `None` for a
/// name with no extension to go by. The same split
/// [`df_core::archive::format_label`] makes, without its placeholder.
fn name_label(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = df_core::archive::archive_stem(name);
    name.get(stem.len()..)
        .map(|rest| rest.trim_start_matches('.').to_ascii_lowercase())
        .filter(|label| !label.is_empty())
}

/// `7z x -so -- <path>`, piped through df-core's tar parser.
///
/// `None` when the stream is not a tar after all (a `notes.txt.bz2`), or 7-Zip
/// could not decompress it; the caller then asks 7-Zip for a plain listing.
fn tar_through_7z(program: &Path, path: &Path, stop: &dyn Fn() -> bool) -> Option<ArchiveTree> {
    if stop() {
        return None;
    }
    let mut child = Command::new(program)
        .args(["x", "-so", "--"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let result = df_core::archive::tar::list(Stoppable {
        inner: stdout,
        stop,
    });
    match &result {
        // Read to the end, so the exit status is the verdict on the stream.
        Ok((_, false)) => {
            if !child.wait().ok()?.success() {
                return None;
            }
        }
        // Stopped early: 7-Zip is still writing into a pipe nobody reads.
        _ => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    let (raws, truncated) = result.ok()?;
    Some(df_core::archive::build(
        path.to_path_buf(),
        ArchiveFormat::Tar,
        raws,
        truncated,
    ))
}

/// `7z l -ba -slt -- <path>`, read as it streams.
///
/// Streamed rather than collected, because a listing of a quarter of a
/// million members is tens of megabytes of `Key = value` and only the first
/// [`PREVIEW_ENTRIES`] rows are kept. stdin is `/dev/null`, so an archive
/// whose *names* are encrypted gets 7-Zip's password prompt answered with
/// end-of-file, and the badge says it needs a password.
fn list_with_7z(
    program: &Path,
    path: &Path,
    format: String,
    stop: &dyn Fn() -> bool,
) -> Result<Listing, String> {
    if stop() {
        return Err(STOPPED.to_string());
    }
    let mut child = Command::new(program)
        .args(["l", "-ba", "-slt", "--"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| sentence(&e.to_string()))?;
    // Drained on a thread of its own: waiting on stdout while stderr fills is
    // the classic deadlock.
    let stderr = child.stderr.take().map(|pipe| {
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut kept = Vec::new();
            let _ = pipe.by_ref().take(KEEP_STDERR).read_to_end(&mut kept);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
            String::from_utf8_lossy(&kept).into_owned()
        })
    });

    // A member with no name is the one stream of a compressed single file
    // (`notes.txt.bz2`), which is the file with its last extension off.
    let fallback = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut tally = Tally::new(format);
    let mut parser = SltParser::default();
    let mut prompt = String::new();
    let mut capped = false;
    let mut stopped = false;
    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            if stop() {
                stopped = true;
                break;
            }
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            // Lossy: a name 7-Zip could not decode still lists, with a
            // replacement character where the bad byte was.
            let text = String::from_utf8_lossy(&buf);
            let line = text.trim_end_matches(['\n', '\r']);
            if line.contains("Enter password") {
                prompt = line.to_string();
            }
            if let Some(entry) = parser.line(line) {
                if !tally.push(row_from_slt(entry, &fallback)) {
                    capped = true;
                    break;
                }
            }
        }
    }
    if capped || stopped {
        let _ = child.kill();
    } else if let Some(entry) = parser.finish() {
        tally.push(row_from_slt(entry, &fallback));
    }
    let status = child.wait().ok();
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    if stopped {
        return Err(STOPPED.to_string());
    }
    if capped {
        return Ok(tally.finish(false));
    }
    // 1 is 7-Zip's *warning*: the listing is whole and something about the
    // archive was odd. Anything else is a failure, and a failure that listed
    // members anyway is a listing that stopped early.
    let failed = status.is_none_or(|s| !s.success() && s.code() != Some(1));
    if !failed {
        return Ok(tally.finish(true));
    }
    if tally.total > 0 {
        return Ok(tally.finish(false));
    }
    Err(failure_reason(status, prompt, &stderr, path))
}

/// The one line for a 7-Zip that listed nothing.
///
/// df-core's [`df_core::archive::external::failure_line`] does the choosing —
/// it knows the password prompt and the headings — once the listing's own
/// echo is out of the way: `7z l` opens its complaint with `ERROR: <path> :
/// <path>`, which is not a sentence anybody wants to read.
fn failure_reason(
    status: Option<std::process::ExitStatus>,
    prompt: String,
    stderr: &str,
    path: &Path,
) -> String {
    let full = path.to_string_lossy();
    let stderr: String = stderr
        .lines()
        .filter(|line| !(line.starts_with("ERROR: ") && line.contains(full.as_ref())))
        .map(|line| format!("{line}\n"))
        .collect();
    let ran = df_core::archive::external::Ran {
        status,
        stdout: prompt,
        stderr,
        cancelled: false,
    };
    let line = df_core::archive::external::failure_line(&ran, path);
    sentence(line.strip_prefix("Open ERROR: ").unwrap_or(&line))
}

/// Capitalise the first letter — the badge's line is a sentence, and df-core's
/// messages start in lowercase because they are usually spliced into one.
fn sentence(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    out
}

/// A reader that fails once `stop` says so, for the tar 7-Zip decompresses.
/// df-core has the same few lines behind [`df_core::archive::list_until`];
/// they are private there, and not worth a public type for two callers.
struct Stoppable<'a, R> {
    inner: R,
    stop: &'a dyn Fn() -> bool,
}

impl<R: Read> Read for Stoppable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if (self.stop)() {
            // `Other`, never `Interrupted`, which the tar reader retries.
            return Err(std::io::Error::other(STOPPED));
        }
        self.inner.read(buf)
    }
}

// ── `7z l -slt` ─────────────────────────────────────────────────────────────

/// One member, as `7z l -slt` describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SltEntry {
    /// `None` for the one stream of a compressed single file, which has no
    /// name of its own (bzip2, zstd).
    pub path: Option<String>,
    /// `None` when 7-Zip prints the key with nothing after it.
    pub size: Option<u64>,
    pub is_dir: bool,
    pub encrypted: bool,
}

/// `7z l -ba -slt`, a line at a time.
///
/// The technical listing is one block of `Key = value` lines per member, the
/// blocks separated by a blank line (`-ba` drops the archive's own block and
/// the banner). Four keys matter:
///
/// - `Path` — the member's name. Its arrival also closes a block that is
///   still open, so a missing blank line cannot merge two members.
/// - `Size` — uncompressed bytes, or nothing when the format does not say.
/// - `Folder = +` — how zip and rar mark a directory. 7z's own format has no
///   such key and puts a `D` in `Attributes` instead, which is read too.
/// - `Encrypted = +`.
///
/// Anything that is not `Key = value` — the `Enter password:` prompt, a
/// warning — is not part of a block and is skipped.
#[derive(Debug, Default)]
pub struct SltParser {
    open: Option<SltEntry>,
}

impl SltParser {
    /// Feed one line, without its newline. Returns a member when this line
    /// finished one.
    pub fn line(&mut self, line: &str) -> Option<SltEntry> {
        if line.trim().is_empty() {
            return self.open.take();
        }
        // ` =` rather than ` = `: an empty value is printed as `CRC = ` and a
        // copy that lost its trailing space must still parse.
        let (key, value) = line.split_once(" =")?;
        let value = value.strip_prefix(' ').unwrap_or(value);
        let done = if key == "Path" && self.open.as_ref().is_some_and(|e| e.path.is_some()) {
            self.open.take()
        } else {
            None
        };
        let entry = self.open.get_or_insert_with(SltEntry::default);
        match key {
            // Kept verbatim — a name can end in a space.
            "Path" => entry.path = Some(value.to_string()),
            "Size" => entry.size = value.trim().parse().ok(),
            "Folder" => entry.is_dir |= value.trim() == "+",
            "Attributes" => entry.is_dir |= attributes_say_directory(value),
            "Encrypted" => entry.encrypted = value.trim() == "+",
            _ => {}
        }
        done
    }

    /// The member still open when the output ended, if any.
    pub fn finish(&mut self) -> Option<SltEntry> {
        self.open.take()
    }
}

/// Whether an `Attributes` value marks a directory: a `D` among the Windows
/// attribute letters (`D`, `DA`), or a Unix mode string that starts with `d`
/// (`D drwxr-xr-x`, or ` drwxr-xr-x` from a zip made on Unix).
fn attributes_say_directory(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        (token.bytes().all(|b| b.is_ascii_uppercase()) && token.contains('D'))
            || (token.len() == 10 && token.starts_with('d'))
    })
}

/// A parsed member as a row. `fallback` names a member that has no name.
fn row_from_slt(entry: SltEntry, fallback: &str) -> ArchiveRow {
    let raw = entry.path.unwrap_or_default();
    let is_dir = entry.is_dir || raw.ends_with('/') || raw.ends_with('\\');
    let name = df_core::archive::normalize(&raw).unwrap_or_else(|| fallback.to_string());
    ArchiveRow {
        name,
        is_dir,
        len: if is_dir { None } else { entry.size },
        encrypted: entry.encrypted,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// Every member in a whole `7z l -ba -slt` output, fed a line at a time the
    /// way the runner feeds it.
    fn parse_slt(text: &str) -> Vec<SltEntry> {
        let mut parser = SltParser::default();
        let mut out: Vec<SltEntry> = text.lines().filter_map(|line| parser.line(line)).collect();
        out.extend(parser.finish());
        out
    }

    /// `7z l -ba -slt` of a small 7z, captured from 7-Zip 26.03: a top-level
    /// folder, a nested one, an empty file, and two encrypted members — with
    /// the `Attributes` letters as the only directory mark, the way 7z's own
    /// format writes it.
    const SEVEN_Z: &str = "\
Path = proj
Size = 0
Packed Size = 0
Modified = 2026-09-23 15:11:26.7492681
Attributes = D drwxr-xr-x
CRC =
Encrypted = -
Method =
Block =

Path = proj/src/nested
Size = 0
Packed Size = 0
Modified = 2026-09-23 15:11:26.7492681
Attributes = D drwxr-xr-x
CRC =
Encrypted = -
Method =
Block =

Path = proj/src/nested/empty.txt
Size = 0
Packed Size = 0
Modified = 2026-09-23 15:11:26.7492681
Attributes = A -rw-r--r--
CRC =
Encrypted = -
Method =
Block =

Path = proj/README.md
Size = 12
Packed Size = 5040
Modified = 2026-09-23 15:11:26.7492681
Attributes = A -rw-r--r--
CRC = AF083B2D
Encrypted = +
Method = LZMA2:6k 7zAES:19
Block = 0

Path = proj/data.bin
Size = 5000
Packed Size =
Modified = 2026-09-23 15:11:26.7492878
Attributes = A -rw-r--r--
CRC = 82ADA145
Encrypted = +
Method = LZMA2:6k 7zAES:19
Block = 0

";

    /// The same tool on a zip: `Folder = +/-` marks directories here, and a
    /// file's attributes open with a space because the Windows half is empty.
    const ZIP: &str = "\
Path = proj
Folder = +
Size = 0
Packed Size = 0
Modified = 2026-09-23 15:11:26
Created =
Accessed =
Attributes = D drwxr-xr-x
Encrypted = -
Comment =
CRC =
Method = Store
Characteristics = UT:MA:1 ux
Host OS = Unix
Version = 10
Volume Index = 0
Offset = 0

Path = proj/data.bin
Folder = -
Size = 5000
Packed Size = 5012
Modified = 2026-09-23 15:11:26
Created =
Accessed =
Attributes =  -rw-r--r--
Encrypted = +
Comment =
CRC = 82ADA145
Method = ZipCrypto Store
Characteristics = UT:MA:1 ux : Encrypt Descriptor
Host OS = Unix
Version = 10
Volume Index = 0
Offset = 63
";

    fn entry(path: &str, size: Option<u64>, is_dir: bool, encrypted: bool) -> SltEntry {
        SltEntry {
            path: Some(path.to_string()),
            size,
            is_dir,
            encrypted,
        }
    }

    #[test]
    fn a_7z_listing_parses_member_by_member() {
        assert_eq!(
            parse_slt(SEVEN_Z),
            vec![
                entry("proj", Some(0), true, false),
                entry("proj/src/nested", Some(0), true, false),
                entry("proj/src/nested/empty.txt", Some(0), false, false),
                entry("proj/README.md", Some(12), false, true),
                entry("proj/data.bin", Some(5000), false, true),
            ]
        );
    }

    /// A zip read through 7-Zip: `Folder` says which is a directory, and the
    /// last block is closed by the end of the output rather than a blank line.
    #[test]
    fn a_zip_listing_parses_with_its_folder_key() {
        assert_eq!(
            parse_slt(ZIP),
            vec![
                entry("proj", Some(0), true, false),
                entry("proj/data.bin", Some(5000), false, true),
            ]
        );
    }

    /// The shapes that are not a tidy block per member: a compressed single
    /// file's nameless stream with no size, a password prompt in the middle
    /// of stdout, two blocks with no blank line between them, CRLF endings,
    /// and a name with ` = ` in it.
    #[test]
    fn the_untidy_shapes_still_parse() {
        let bz2 = "Size = \nPacked Size = \n\n";
        assert_eq!(
            parse_slt(bz2),
            vec![SltEntry {
                path: None,
                size: None,
                is_dir: false,
                encrypted: false,
            }]
        );

        assert!(parse_slt("\nEnter password:\n\n").is_empty());

        let run_on = "Path = a.txt\r\nSize = 3\r\nPath = b = c.txt\r\nSize = 4\r\n";
        assert_eq!(
            parse_slt(run_on),
            vec![
                entry("a.txt", Some(3), false, false),
                entry("b = c.txt", Some(4), false, false),
            ]
        );

        // Windows attribute letters only, and a copy that lost the trailing
        // space after an empty value.
        let windows = "Path = docs\nAttributes = DA\nCRC =\n\nPath = x\nAttributes = A\n";
        assert_eq!(
            parse_slt(windows),
            vec![
                SltEntry {
                    path: Some("docs".to_string()),
                    size: None,
                    is_dir: true,
                    encrypted: false,
                },
                SltEntry {
                    path: Some("x".to_string()),
                    size: None,
                    is_dir: false,
                    encrypted: false,
                },
            ]
        );
    }

    /// A parsed member becomes a row: a folder has no size, a trailing slash
    /// is a folder whatever the keys said, and the nameless stream is named
    /// for the file it came out of.
    #[test]
    fn members_become_rows() {
        let row = row_from_slt(entry("proj/src/", Some(0), false, false), "x");
        assert_eq!(
            row,
            ArchiveRow {
                name: "proj/src".to_string(),
                is_dir: true,
                len: None,
                encrypted: false,
            }
        );
        let row = row_from_slt(SltEntry::default(), "notes.txt");
        assert_eq!(row.name, "notes.txt");
        assert_eq!(row.len, None, "an unknown size is not zero");
        let row = row_from_slt(entry("a/b.bin", Some(7), false, true), "x");
        assert_eq!(row.len, Some(7));
        assert!(row.encrypted);
    }

    #[test]
    fn the_tally_keeps_five_hundred_and_counts_the_rest() {
        let mut tally = Tally::new("zip".to_string());
        tally.push(ArchiveRow {
            name: "d".to_string(),
            is_dir: true,
            len: None,
            encrypted: false,
        });
        for i in 0..700 {
            tally.push(ArchiveRow {
                name: format!("d/{i}"),
                is_dir: false,
                len: Some(10),
                encrypted: i == 650,
            });
        }
        let listing = tally.finish(true);
        assert_eq!(listing.rows.len(), PREVIEW_ENTRIES);
        assert_eq!(listing.total, 701);
        assert_eq!(listing.files, 700);
        assert_eq!(listing.total_len, 7000);
        assert!(
            listing.encrypted,
            "a member past the kept rows still counts"
        );
        assert_eq!(listing.rows[0].name, "d", "the archive's order is kept");
    }

    #[test]
    fn a_format_is_named_by_its_extension_first() {
        assert_eq!(
            name_label(Path::new("/d/proj.tar.bz2")).as_deref(),
            Some("tar.bz2")
        );
        assert_eq!(name_label(Path::new("/d/a.7z")).as_deref(), Some("7z"));
        assert_eq!(
            name_label(Path::new("/d/notes.txt.gz")).as_deref(),
            Some("gz")
        );
        assert_eq!(name_label(Path::new("/d/README")), None);
        assert_eq!(
            hint_label(&ArchiveError::Unsupported { format: "rar" }),
            "rar"
        );
        assert_eq!(
            hint_label(&ArchiveError::NotAnArchive(PathBuf::from("/x"))),
            "archive"
        );
    }

    /// With no 7-Zip, a format df-core refuses keeps its badge and says what
    /// would list it.
    #[test]
    fn with_no_7zip_the_reason_says_what_to_install() {
        let tree = df_core::test_support::TempTree::new("listing-no-7z");
        // The 7z signature and nothing after it: df-core names the format and
        // refuses, which is all this needs.
        let path = tree.file("a.7z", b"7z\xBC\xAF\x27\x1C\x00\x04junk");
        assert_eq!(
            list_with(&path, None, &|| false),
            Err(INSTALL_7ZIP.to_string())
        );

        // A split set's head goes to 7-Zip before df-core is asked at all.
        let head = tree.file("backup.7z.001", b"7z\xBC\xAF\x27\x1C\x00\x04junk");
        assert_eq!(
            list_with(&head, None, &|| false),
            Err(INSTALL_7ZIP.to_string())
        );

        // …as does a zip with its first split piece beside it, and not one
        // without.
        tree.file("photos.z01", b"PK\x07\x08");
        let split = tree.file("photos.zip", b"");
        assert_eq!(volume_head(&split), Some(VolumeKind::ZipSplit));
        let lone = tree.file("lone.zip", b"");
        assert_eq!(volume_head(&lone), None);

        let missing = tree.join("gone.zip");
        let reason = list_with(&missing, None, &|| false).unwrap_err();
        // In the system's own words, which are Linux's "No such file or
        // directory" and Windows' "The system cannot find the file
        // specified" (W4.26).
        let system = std::fs::File::open(&missing).expect_err("gone").to_string();
        let words = system.split(" (os error").next().unwrap_or(&system);
        assert!(
            reason.starts_with(words),
            "an unreadable file says why: {reason}"
        );
    }

    fn installed(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    }

    fn run(program: &Path, args: &[&str], dir: &Path) -> bool {
        Command::new(program)
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The real thing, where 7-Zip is installed: a 7z made on the spot lists
    /// through `7z l`, encrypted members and all, and one whose *names* are
    /// encrypted says it needs a password.
    #[test]
    fn a_7z_lists_through_7zip() {
        let Some(seven) = installed("7z") else {
            return;
        };
        let tree = df_core::test_support::TempTree::new("listing-7z");
        tree.file("src/proj/a.txt", b"hello");
        tree.file("src/proj/sub/b.txt", b"world!");
        let src = tree.join("src");
        let archive = tree.join("out.7z");
        let locked = tree.join("locked.7z");
        let hidden = tree.join("hidden.7z");
        let arg = |p: &Path| p.to_string_lossy().into_owned();
        assert!(run(&seven, &["a", "-bd", &arg(&archive), "proj"], &src));
        assert!(run(
            &seven,
            &["a", "-bd", "-pdf", &arg(&locked), "proj"],
            &src
        ));
        assert!(run(
            &seven,
            &["a", "-bd", "-pdf", "-mhe=on", &arg(&hidden), "proj"],
            &src
        ));

        let listing = list_with(&archive, Some(&seven), &|| false).unwrap();
        assert_eq!(listing.format, "7z");
        assert_eq!(listing.files, 2);
        assert_eq!(listing.total_len, 11);
        assert!(listing.complete);
        assert!(!listing.encrypted);
        let names: Vec<&str> = listing.rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"proj"), "{names:?}");
        assert!(names.contains(&"proj/sub/b.txt"), "{names:?}");
        assert!(listing.rows.iter().any(|r| r.name == "proj" && r.is_dir));

        let locked = list_with(&locked, Some(&seven), &|| false).unwrap();
        assert!(locked.encrypted);
        assert_eq!(locked.files, 2);

        let reason = list_with(&hidden, Some(&seven), &|| false).unwrap_err();
        assert!(reason.contains("password"), "{reason}");

        // A listing retired before it starts runs nothing at all.
        assert_eq!(
            list_with(&archive, Some(&seven), &|| true),
            Err(STOPPED.to_string())
        );
    }

    /// A `.tar.bz2` is a tar df-core has no decompressor for: 7-Zip
    /// decompresses and df-core's parser reads the tar, so what lists is the
    /// tar's members and not the one stream `7z l` would show.
    #[test]
    fn a_tar_bz2_lists_its_members_through_7zip() {
        let (Some(seven), Some(tar)) = (installed("7z"), installed("tar")) else {
            return;
        };
        if installed("bzip2").is_none() {
            return;
        }
        let tree = df_core::test_support::TempTree::new("listing-tbz");
        tree.file("src/proj/a.txt", b"hello");
        let src = tree.join("src");
        let archive = tree.join("proj.tar.bz2");
        assert!(run(
            &tar,
            &["cjf", &archive.to_string_lossy(), "proj"],
            &src
        ));
        let listing = list_with(&archive, Some(&seven), &|| false).unwrap();
        assert_eq!(listing.format, "tar.bz2");
        let names: Vec<&str> = listing.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["proj", "proj/a.txt"]);
        assert_eq!(listing.total_len, 5);
    }
}
