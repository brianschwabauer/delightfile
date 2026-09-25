//! Archives written: the selection packed into one file (`A`).
//!
//! The other half of [`crate::archive`], and the same line through it. Zip and
//! tar are framing, so they are written here — [`zip`] and [`tar`] — and so is
//! gzip's ten-byte header and eight-byte trailer ([`deflate`]); the deflate
//! inside is miniz_oxide's, the crate the reader already inflates with. zstd
//! and xz are real compressors this crate has no budget for, so a `.tar.zst`
//! is the tar writer piping into `zstd`, exactly as listing one is `zstd -dc`
//! piping into the tar reader. 7z is 7-Zip's from end to end.
//!
//! ```text
//!   walk ──▶ members ──┬─▶ zip ─────────────────────────▶ temp file
//!                      ├─▶ tar ─────────────────────────▶ temp file
//!                      ├─▶ tar ─▶ gzip ─────────────────▶ temp file
//!                      └─▶ tar ─▶ stdin │ zstd / xz │ stdout ▶ temp file
//!   sources ───────────────▶ 7z a (in their folder) ────▶ temp file
//!                                                          │
//!                                             rename ◀─────┘
//! ```
//!
//! ## Never half an archive
//!
//! Everything is written to a `.df-tmp-…` name beside the destination and
//! renamed over it at the end, the way [`crate::ops::copy`] replaces a file.
//! A cancel, a read error, a full disk or a compressor that dies each remove
//! the temporary file and leave the directory as it was: an archive that
//! stops half-way *looks* complete in a listing, and is the worst thing this
//! could leave behind. The rename also makes a replace atomic — there is no
//! instant at which the old archive is gone and the new one is not there.
//!
//! ## What goes in, and how it is laid out
//!
//! Each selected item at the archive's root under its own name: one folder is
//! the archive's single top-level entry (`photos/IMG_0001.jpg`), a file is at
//! the root, several items sit side by side. Inside a folder everything goes,
//! hidden files included, because an archive is a copy and a copy that quietly
//! dropped the dotfiles would be a different folder. Symlinks go in as links,
//! never followed — a link to `/` must not archive the machine. Hardlinks are
//! just their contents. Sockets, fifos and device nodes cannot be archived
//! meaningfully and are named in [`Packed::skipped`] rather than dropped
//! without a word.
//!
//! The walk happens first and in full, inside the job, so the progress bar has
//! its total before the first byte is written.

pub mod crc32;
mod deflate;
mod tar;
mod zip;

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

/// A format this can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarZst,
    TarXz,
    SevenZip,
}

impl Format {
    /// Every one, in the order the prompt lists them: the in-house three
    /// first, then the ones that need a program.
    pub const ALL: [Format; 6] = [
        Format::Zip,
        Format::Tar,
        Format::TarGz,
        Format::TarZst,
        Format::TarXz,
        Format::SevenZip,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::TarZst => "tar.zst",
            Format::TarXz => "tar.xz",
            Format::SevenZip => "7z",
        }
    }

    /// The program it needs, for the formats not written here alone.
    pub fn tool(self) -> Option<&'static str> {
        match self {
            Format::Zip | Format::Tar | Format::TarGz => None,
            Format::TarZst => Some("zstd"),
            Format::TarXz => Some("xz"),
            Format::SevenZip => Some("7z"),
        }
    }

    /// Whether this machine can write it now.
    ///
    /// zstd and xz are asked the question the reader asks its decompressors
    /// ([`super::have`]), so the two can never disagree about whether `zstd`
    /// is installed. 7-Zip has no `--version` — it exits 7 for it — so it is
    /// looked for on `PATH`, the way [`super::external`] finds it.
    ///
    /// **Spawns processes.** Ask once, off the frame or when a prompt opens,
    /// and keep the answer.
    pub fn is_available(self) -> bool {
        match self.tool() {
            None => true,
            Some("7z") => on_path("7z").is_some(),
            Some(tool) => super::have(tool),
        }
    }

    /// The sentence for a missing tool.
    fn missing(self) -> String {
        format!(
            "{} needs {}, which is not installed",
            self.label(),
            self.tool().unwrap_or("a program")
        )
    }
}

/// Extensions that name a format this writes, longest first so `.tar.gz` is
/// found before anything shorter could claim it. Matched without regard to
/// case, and the name keeps the case it was typed in.
const WRITABLE: &[(&str, Format)] = &[
    (".tar.gz", Format::TarGz),
    (".tar.zst", Format::TarZst),
    (".tar.xz", Format::TarXz),
    (".tgz", Format::TarGz),
    (".tzst", Format::TarZst),
    (".txz", Format::TarXz),
    (".zip", Format::Zip),
    (".tar", Format::Tar),
    (".7z", Format::SevenZip),
];

/// Extensions that name an archive or a compressor this does *not* write.
/// Refused rather than given a `.zip`: `photos.rar` is somebody asking for a
/// rar, and `photos.rar.zip` would be answering a different question.
const UNWRITABLE: &[&str] = &[
    "tar.bz2", "tar.lz", "tar.lzma", "tar.lz4", "tar.z", "tbz", "tbz2", "tb2", "tlz", "bz2", "gz",
    "xz", "zst", "lz", "lz4", "lzma", "lzo", "z", "rar", "cbr", "cab", "arj", "ace", "cpio", "iso",
    "dmg", "wim",
];

/// What a typed archive name comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// Write `name` in `format`. `name` is the text as typed — a path relative
    /// to the directory it was typed in, when it has a `/` — with `.zip` on the
    /// end when it named no archive format at all.
    Archive { name: String, format: Format },
    /// The extension names an archive this cannot write: `rar`, `tar.bz2`.
    Unwritable(String),
    /// No name: nothing typed, a bare extension, or a trailing `/`.
    Empty,
}

/// What a name's extension says, whether or not there is a name in front of
/// it yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extension {
    /// A format this writes.
    Writes(Format),
    /// No archive extension: the name will get `.zip`.
    Bare,
    /// An archive this cannot write, spelled as its extension: `rar`.
    Unwritable(String),
}

/// The extension of the last component of `text`, and how many bytes of
/// that component it takes.
fn extension_of(text: &str) -> (Extension, usize) {
    let text = text.trim();
    let leaf = text.rsplit('/').next().unwrap_or(text);
    let lower = leaf.to_ascii_lowercase();
    if let Some((suffix, format)) = WRITABLE.iter().find(|(s, _)| lower.ends_with(s)) {
        return (Extension::Writes(*format), suffix.len());
    }
    UNWRITABLE
        .iter()
        .find(|ext| {
            lower
                .strip_suffix(**ext)
                .is_some_and(|rest| rest.ends_with('.'))
        })
        .map(|ext| (Extension::Unwritable((*ext).to_string()), ext.len() + 1))
        .unwrap_or((Extension::Bare, 0))
}

/// The format a typed name's extension picks — what the prompt's hint lights
/// up while the name is still being typed.
pub fn extension(text: &str) -> Extension {
    extension_of(text).0
}

/// Read a typed name: `photos` is `photos.zip`, `photos.tar.zst` is a
/// `.tar.zst`, `photos.rar` is a refusal.
pub fn named(text: &str) -> Named {
    let text = text.trim();
    let leaf = text.rsplit('/').next().unwrap_or(text);
    let (extension, suffix) = extension_of(text);
    // An extension with nothing in front of it is not a name: `.zip` would be
    // a hidden file that is also an archive, which nobody means.
    if leaf.len() == suffix {
        return Named::Empty;
    }
    match extension {
        Extension::Writes(format) => Named::Archive {
            name: text.to_string(),
            format,
        },
        Extension::Unwritable(ext) => Named::Unwritable(ext),
        Extension::Bare => Named::Archive {
            name: format!("{text}.zip"),
            format: Format::Zip,
        },
    }
}

/// One archive to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    /// What goes in, each at the archive's root under its own name.
    pub sources: Vec<PathBuf>,
    /// The archive. Missing parent directories are made.
    pub dest: PathBuf,
    pub format: Format,
    /// Whether `dest` may be replaced. Asked by the caller, never assumed:
    /// without it an archive that finds its name taken when it is done is an
    /// error, and the file that took it is left alone.
    pub overwrite: bool,
}

/// What an archive came to, for the toast and the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packed {
    pub path: PathBuf,
    /// The selected items, which is the count a person has in mind.
    pub items: usize,
    /// Bytes of file contents actually read into it — not what the walk
    /// expected, if a file changed size in between. Zero for 7z, whose
    /// reading is 7-Zip's.
    pub bytes: u64,
    /// The archive's own size on the disk.
    pub size: u64,
    /// What the in-house writers left out: sockets, fifos, device nodes.
    /// Always empty for 7z, which is handed the items whole and archives
    /// what it finds by its own rules.
    pub skipped: Vec<PathBuf>,
    /// Directories made for the destination, shallowest first — what
    /// [`crate::ops::journal::OpRecord::Create`] peels off on undo.
    pub created_parents: Vec<PathBuf>,
}

impl Pack {
    /// What the `w` panel calls it: `Archive 12 items → photos.zip`.
    pub fn name(&self) -> String {
        let n = self.sources.len();
        let count = if n == 1 {
            "1 item".to_string()
        } else {
            format!("{} items", crate::text::grouped(n as u64))
        };
        format!("Archive {count} → {}", leaf(&self.dest))
    }

    /// Why this archive cannot be written, asked before it is queued so the
    /// answer can go back to the prompt it was typed in.
    ///
    /// Asks nothing that spawns a process: whether the format's program is
    /// installed is [`Format::is_available`]'s question, and a caller keeps
    /// that answer rather than asking it on every keystroke.
    pub fn check(&self) -> std::result::Result<(), String> {
        self.absolute().check_absolute()
    }

    /// The same archive with every path made absolute against the process's
    /// directory. Every writer works from this, because one of them — 7z —
    /// runs in the sources' folder, where a relative temporary name would
    /// land somewhere else entirely.
    ///
    /// `std::path::absolute`, not [`crate::ops::normalize`]: it prepends the
    /// directory and leaves `..` where it is, so a path through a symlink
    /// still goes where the person meant rather than where the text says.
    fn absolute(&self) -> Pack {
        let absolute =
            |path: &Path| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        Pack {
            sources: self.sources.iter().map(|source| absolute(source)).collect(),
            dest: absolute(&self.dest),
            format: self.format,
            overwrite: self.overwrite,
        }
    }

    fn check_absolute(&self) -> std::result::Result<(), String> {
        if self.sources.is_empty() {
            return Err("Nothing to archive".to_string());
        }
        // Each item goes in at the root under its own name, so two with the
        // same name — `a/box` and `b/box` — would be two members with one
        // name, and which of them an extraction keeps is anybody's guess.
        let mut seen = std::collections::HashSet::new();
        if let Some(twice) = self
            .sources
            .iter()
            .filter_map(|source| source.file_name())
            .find(|name| !seen.insert(*name))
        {
            return Err(format!(
                "Two of the items are called {} — an archive holds one of each name at its root",
                twice.to_string_lossy()
            ));
        }
        // The archive cannot be inside something that is going into it: the
        // walk would find the half-written file and try to archive it, and
        // then the archive would be in the archive. `copy.rs`'s "into itself"
        // rail, for the same reason and resolved the same way, so a symlinked
        // parent does not hide it.
        if let Some(source) = self.sources.iter().find(|source| {
            crate::ops::is_real_dir(source)
                && crate::ops::is_strict_ancestor_resolved(source, &self.dest)
        }) {
            return Err(format!(
                "{} would be inside {}, which is going into it",
                leaf(&self.dest),
                leaf(source)
            ));
        }
        if self.format == Format::SevenZip {
            // 7-Zip names the members after the paths it is given, relative
            // to where it runs. One folder is one place to run it from; items
            // from two could not all land at the archive's root.
            let first = self.sources[0].parent();
            if self.sources.iter().any(|source| source.parent() != first) {
                return Err("7z takes items from one folder at a time".to_string());
            }
        }
        if self.dest.is_dir() {
            return Err(format!("{} is a folder", leaf(&self.dest)));
        }
        Ok(())
    }

    /// Write it: walk, write to a temporary name, rename into place.
    ///
    /// Blocking and long — a job's body. Cancelled at every chunk; on a cancel
    /// or a failure nothing is left behind, not the temporary file and not the
    /// directories made for the destination.
    pub fn run(&self, ctx: &TaskCtx) -> Result<Packed> {
        let pack = self.absolute();
        pack.check_absolute().map_err(DfError::Op)?;
        let created_parents = crate::ops::create::make_parents(&pack.dest)?;
        match pack.write(ctx) {
            Ok(mut packed) => {
                packed.created_parents = created_parents;
                Ok(packed)
            }
            Err(e) => {
                crate::ops::create::remove_parents(&created_parents);
                Err(e)
            }
        }
    }

    fn write(&self, ctx: &TaskCtx) -> Result<Packed> {
        let (temp, bytes, skipped) = if self.format == Format::SevenZip {
            // Not created first: 7-Zip opens an existing file as an archive to
            // add to, and an empty one is not an archive.
            let temp = temp_name(&self.dest, ".7z")?;
            if let Err(e) = seven_zip(&self.sources, &temp, ctx) {
                let _ignored = std::fs::remove_file(&temp);
                return Err(e);
            }
            (temp, 0, Vec::new())
        } else {
            let walked = walk(&self.sources, ctx)?;
            ctx.set_total(walked.bytes, walked.members.len() as u64);
            let (temp, file) = claim_temp(&self.dest)?;
            match self.write_members(&walked.members, file, ctx) {
                Ok(read) => (temp, read, walked.skipped),
                Err(e) => {
                    let _ignored = std::fs::remove_file(&temp);
                    return Err(e);
                }
            }
        };

        // The one moment a name can have been taken since the prompt asked:
        // without the caller's say-so, a file that appeared there is not
        // replaced.
        if !self.overwrite && crate::ops::exists(&self.dest) {
            let _ignored = std::fs::remove_file(&temp);
            return Err(DfError::Op(format!(
                "{} appeared while the archive was being written, and was left alone",
                leaf(&self.dest)
            )));
        }
        if let Err(e) = std::fs::rename(&temp, &self.dest) {
            let _ignored = std::fs::remove_file(&temp);
            return Err(DfError::io(&self.dest, e));
        }
        let size = std::fs::metadata(&self.dest).map(|m| m.len()).unwrap_or(0);
        Ok(Packed {
            path: self.dest.clone(),
            items: self.sources.len(),
            bytes,
            size,
            skipped,
            created_parents: Vec::new(),
        })
    }

    /// The in-house formats, into the temporary file. Returns how many bytes
    /// of file contents went in.
    fn write_members(&self, members: &[Member], file: File, ctx: &TaskCtx) -> Result<u64> {
        let archive = self.dest.as_path();
        let buffered = BufWriter::with_capacity(crate::ops::COPY_CHUNK, file);
        // The buffer's last flush is a write like any other, and a full disk
        // is most likely to say so here — so it is asked, not dropped.
        let flushed = |out: BufWriter<File>, read: u64| -> Result<u64> {
            out.into_inner()
                .map(|_file| read)
                .map_err(|e| DfError::io(archive, e.into_error()))
        };
        match self.format {
            Format::Zip => {
                let mut writer = zip::ZipWriter::new(buffered, archive, zip::LIMITS);
                for member in members {
                    writer.add(member, ctx)?;
                }
                let (out, read) = writer.finish()?;
                flushed(out, read)
            }
            Format::Tar => {
                let mut out = buffered;
                let read = tar::write(members, &mut out, archive, ctx)?;
                flushed(out, read)
            }
            Format::TarGz => {
                let mut out = deflate::Gzip::new(buffered).map_err(|e| DfError::io(archive, e))?;
                let read = tar::write(members, &mut out, archive, ctx)?;
                flushed(out.finish().map_err(|e| DfError::io(archive, e))?, read)
            }
            Format::TarZst | Format::TarXz => {
                // The file goes to the compressor as its stdout; nothing here
                // writes to it directly.
                let file = buffered
                    .into_inner()
                    .map_err(|e| DfError::io(archive, e.into_error()))?;
                let tool = self.format.tool().unwrap_or("zstd");
                piped(tool, self.format, members, file, archive, ctx)
            }
            Format::SevenZip => Err(DfError::Op("7z is written by 7-Zip".to_string())),
        }
    }
}

/// The last component of a path, for a sentence.
fn leaf(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ── The walk ────────────────────────────────────────────────────────────────

/// One entry going into an archive: what it is, what to call it inside, and
/// the metadata that goes with it.
#[derive(Debug, Clone)]
struct Member {
    source: PathBuf,
    /// The name inside the archive, `/`-separated, as bytes — a name that is
    /// not UTF-8 goes in as it is on the disk. A directory's ends in `/`.
    name: Vec<u8>,
    what: What,
    /// `st_mode`; the writers keep the permission bits.
    mode: u32,
    /// Unix seconds.
    mtime: i64,
    uid: u32,
    gid: u32,
    /// Zip only: the contents are already compressed, so they are stored.
    stored: bool,
}

#[derive(Debug, Clone)]
enum What {
    Dir,
    /// A regular file of this many bytes, as the walk found it.
    File(u64),
    /// A symlink, and the link text.
    Link(Vec<u8>),
}

struct Walked {
    members: Vec<Member>,
    bytes: u64,
    skipped: Vec<PathBuf>,
}

/// How many entries the walk visits between two cancellation checks. Small
/// enough that a cancel during the walk of a big tree lands at once, large
/// enough that the check is invisible next to the `lstat`s.
const WALK_CHECK: usize = 256;

/// Every entry under `sources`, parents before children, each directory's
/// children in byte order so the same tree always makes the same archive.
fn walk(sources: &[PathBuf], ctx: &TaskCtx) -> Result<Walked> {
    let mut walked = Walked {
        members: Vec::new(),
        bytes: 0,
        skipped: Vec::new(),
    };
    for source in sources {
        use std::os::unix::ffi::OsStrExt;
        let Some(name) = source.file_name() else {
            return Err(DfError::Op(format!(
                "{} has no name to put in an archive",
                source.display()
            )));
        };
        visit(source, name.as_bytes().to_vec(), &mut walked, ctx)?;
    }
    Ok(walked)
}

fn visit(path: &Path, name: Vec<u8>, walked: &mut Walked, ctx: &TaskCtx) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    if walked.members.len().is_multiple_of(WALK_CHECK) {
        ctx.checkpoint()?;
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| DfError::io(path, e))?;
    let member = |name: Vec<u8>, what: What, stored: bool| Member {
        source: path.to_path_buf(),
        name,
        what,
        mode: meta.mode(),
        mtime: meta.mtime(),
        uid: meta.uid(),
        gid: meta.gid(),
        stored,
    };
    let kind = meta.file_type();
    if kind.is_symlink() {
        let target = std::fs::read_link(path).map_err(|e| DfError::io(path, e))?;
        let target = target.as_os_str().as_bytes().to_vec();
        walked.members.push(member(name, What::Link(target), true));
    } else if kind.is_dir() {
        let mut dir_name = name.clone();
        dir_name.push(b'/');
        walked.members.push(member(dir_name, What::Dir, true));
        let mut children: Vec<(OsString, PathBuf)> = std::fs::read_dir(path)
            .map_err(|e| DfError::io(path, e))?
            .map(|entry| {
                entry
                    .map(|entry| (entry.file_name(), entry.path()))
                    .map_err(|e| DfError::io(path, e))
            })
            .collect::<Result<_>>()?;
        children.sort();
        for (child, child_path) in children {
            let mut child_name = name.clone();
            child_name.push(b'/');
            child_name.extend_from_slice(child.as_bytes());
            visit(&child_path, child_name, walked, ctx)?;
        }
    } else if kind.is_file() {
        // Its own name, not the path inside the archive: a folder called
        // `trip.2024` must not make every file in it look like a `.2024`.
        let leaf = path.file_name().map(|leaf| leaf.to_string_lossy());
        let stored = leaf.is_some_and(|leaf| zip::stores(&leaf));
        walked.bytes += meta.len();
        walked
            .members
            .push(member(name, What::File(meta.len()), stored));
    } else {
        walked.skipped.push(path.to_path_buf());
    }
    Ok(())
}

/// Fill `buf` unless the file ends first.
fn read_chunk<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    super::unpack::read_full(reader, buf)
}

// ── Temporary names ─────────────────────────────────────────────────────────

/// How many `.df-tmp-…` names to try. See `copy.rs`'s `MAX_TEMP_ATTEMPTS`.
const MAX_TEMP_ATTEMPTS: u32 = 16;

/// A free `.df-tmp-…` name beside `dest`, in its own directory so the final
/// rename cannot cross a filesystem. `archive` in the name keeps it apart
/// from the ones a copy makes in the same directory at the same moment.
fn temp_name(dest: &Path, suffix: &str) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = dest.parent().unwrap_or(Path::new("."));
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let candidate = dir.join(format!(
            ".df-tmp-{}-archive-{n}{suffix}",
            std::process::id()
        ));
        if !crate::ops::exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(DfError::Op(format!(
        "no free temporary name in {}",
        dir.display()
    )))
}

/// A temporary name, made — `create_new`, so two jobs cannot both think they
/// have the same one.
fn claim_temp(dest: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let temp = temp_name(dest, "")?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(DfError::io(&temp, e)),
        }
    }
    Err(DfError::Op(format!(
        "no free temporary name beside {}",
        dest.display()
    )))
}

// ── Somebody else's compressor ──────────────────────────────────────────────

/// A program on `PATH`, found without running it.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| super::external::is_executable(candidate))
}

/// The tar, written into a compressor's stdin, whose stdout is the file.
///
/// The compressor runs beside this thread rather than after it, so the tar
/// never exists uncompressed. On a cancel or a failure the compressor is
/// killed — it would otherwise sit on a pipe nobody is writing to — and a
/// write that failed *because* it died defers to what it said on stderr,
/// which is the sentence that names the real problem (a full disk, usually).
fn piped(
    tool: &str,
    format: Format,
    members: &[Member],
    file: File,
    archive: &Path,
    ctx: &TaskCtx,
) -> Result<u64> {
    // `-T0`: every core. Both write a standard stream either way, and an
    // archive of a big folder is exactly the job threads are for.
    let args: &[&str] = match format {
        Format::TarXz => &["-z", "-c", "-q", "-T0"],
        _ => &["-q", "-c", "-T0"],
    };
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DfError::Op(format.missing())
            } else {
                DfError::io(tool, e)
            }
        })?;
    let stderr = child.stderr.take().map(super::external::drain);
    let said = |stderr: Option<std::thread::JoinHandle<Vec<u8>>>| {
        stderr
            .and_then(|handle| handle.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    let Some(stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(DfError::Op(format!("{tool} took no input")));
    };

    let mut sink = BufWriter::with_capacity(crate::ops::COPY_CHUNK, stdin);
    let wrote = tar::write(members, &mut sink, archive, ctx).and_then(|read| {
        sink.flush()
            .map(|()| read)
            .map_err(|e| DfError::io(archive, e))
    });
    // Closing stdin is the end of the input; the compressor finishes the
    // stream and exits.
    drop(sink);

    // The compressor's own account of a failure: the first thing it said, or
    // how it exited when it said nothing.
    let why = |text: &str, status: Option<std::process::ExitStatus>| {
        let line = text.lines().map(str::trim).find(|line| !line.is_empty());
        match (line, status.and_then(|s| s.code())) {
            (Some(line), _) => format!("{tool}: {line}"),
            (None, Some(code)) => format!("{tool} exited with status {code}"),
            (None, None) => format!("{tool} was stopped"),
        }
    };
    match wrote {
        Ok(read) => {
            let status = child.wait().map_err(|e| DfError::io(tool, e))?;
            let text = said(stderr);
            if status.success() {
                return Ok(read);
            }
            Err(DfError::Op(why(&text, Some(status))))
        }
        // The pipe closed under the write: the compressor died first, and
        // what it said is the error, not the pipe.
        Err(DfError::Io { source, .. }) if source.kind() == std::io::ErrorKind::BrokenPipe => {
            let status = child.wait().ok();
            Err(DfError::Op(why(&said(stderr), status)))
        }
        // Cancelled, or a source that could not be read: the compressor is
        // stopped, and the error is this side's.
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            drop(said(stderr));
            Err(e)
        }
    }
}

/// `7z a` in the sources' folder, with their names, into `temp`.
///
/// `-snl` stores a symlink as a link, as the in-house writers do; `-bd` turns
/// the progress display off, since nothing reads it; `-t7z` because the
/// temporary name is not what 7-Zip would guess from. Its progress is not
/// parsed, so the task shows a bar with no total.
fn seven_zip(sources: &[PathBuf], temp: &Path, ctx: &TaskCtx) -> Result<()> {
    let program = on_path("7z").ok_or_else(|| DfError::Op(Format::SevenZip.missing()))?;
    let dir = sources
        .first()
        .and_then(|source| source.parent())
        .ok_or_else(|| DfError::Op("Nothing to archive".to_string()))?;
    let mut args: Vec<OsString> = ["a", "-t7z", "-bd", "-y", "-snl", "--"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(temp.as_os_str().to_owned());
    for source in sources {
        let Some(name) = source.file_name() else {
            return Err(DfError::Op(format!(
                "{} has no name to put in an archive",
                source.display()
            )));
        };
        args.push(name.to_owned());
    }
    let ran = super::external::run_in(&program, &args, Some(dir), ctx)?;
    if ran.cancelled {
        return Err(DfError::Cancelled);
    }
    if !ran.succeeded() {
        return Err(DfError::Op(format!(
            "7-Zip: {}",
            super::external::failure_line(&ran, temp)
        )));
    }
    Ok(())
}
