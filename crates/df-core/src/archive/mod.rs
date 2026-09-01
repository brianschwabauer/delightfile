//! Archives as read-only directories.
//!
//! PLAN §7.3's third bullet: `l` on a `.zip` walks into it, and what is inside
//! lists like any other directory. This module is the half that makes that
//! possible without a window — turning a file into an [`ArchiveTree`] that
//! answers "what is in `src/`" the way [`crate::fs::scan`] answers it for a real
//! directory.
//!
//! ```text
//!   path ──▶ sniff ──▶ ArchiveFormat ──┬─▶ zip  (Read + Seek, index at the end)
//!                                      │
//!                                      └─▶ tar  (Read, walk forwards)
//!                                            ▲
//!                       gzip -dc / xz -dc / zstd -dc
//! ```
//!
//! ## Why the parsers are ours and the decompressors are not
//!
//! Container formats are *framing*: fixed-width fields, offsets, a directory at
//! one end. Zip and tar together are about six hundred lines here, they are
//! stable by decades of contract, and owning them means the path-safety rules
//! ([`tree`]) are applied by the code that reads the names rather than trusted to
//! somebody else's. That clears PLAN §1's bar for writing it ourselves.
//!
//! Compression is not framing. deflate, LZMA and zstd are real algorithms with
//! real performance work in them, and this crate's dependency budget does not
//! stretch to three of them. But `gzip`, `xz` and `zstd` are already on the
//! machine, they stream on stdout, and a tar parser that reads from a pipe does
//! not care where the bytes came from. So a `.tar.zst` is `zstd -dc` piped
//! through the same parser a plain `.tar` uses, and a missing binary means that
//! one format lists as unavailable — [`ArchiveError::NoDecompressor`] naming the
//! program to install — rather than the feature being absent.
//!
//! The seam is honest about its cost: listing a compressed tar decompresses the
//! whole thing, because tar's headers are interleaved with its data. Zip does
//! not have that problem, which is one more reason its index is read from the
//! back.
//!
//! ## Caps
//!
//! The file under the cursor was chosen by the user and may have been written by
//! someone else entirely. Every allocation here is bounded before a number in
//! the file is believed: [`tree::MAX_ENTRIES`], [`tree::MAX_NAME_BYTES`],
//! [`zip::MAX_CENTRAL_BYTES`], [`tar::MAX_LONG_NAME`], [`tar::MAX_PAX_BYTES`],
//! [`tar::MAX_STREAM_BYTES`]. Hitting one sets [`ArchiveTree::truncated`], which
//! the UI shows as a listing that stops rather than an archive that ends.
//!
//! Malformed input is an [`ArchiveError`] with a sentence in it, never a panic:
//! every read is bounds-checked, every length is `checked_`, and the tests
//! include truncated files for each format.
//!
//! ## What is not here
//!
//! - **Extraction.** [`extract::plan_extract`] settles what an extraction would
//!   do — destinations, conflicts, byte totals, and the refusal of unsafe names —
//!   because that is the part with the security-relevant decisions in it and it
//!   is pure. Running the plan is not: a zip member is deflate, which this crate
//!   cannot decode, so executing means either an inflate implementation or
//!   shelling out to `unzip`/`bsdtar` and inheriting *their* path handling.
//!   That is a decision to make deliberately rather than to fall into, and it is
//!   Phase 5's UI half, so the job lives there and takes this plan.
//! - **7z, rar, bzip2.** Listed as [`ArchiveError::Unsupported`] with the format
//!   named. 7z and rar are genuinely complicated containers and neither is worth
//!   a hand-rolled parser for v1; bzip2-compressed tar works the moment a
//!   `bzip2` line is added to [`ArchiveFormat::decompressor`] and is left out
//!   only because nothing produces `.tar.bz2` any more.
//! - **Decompressing to preview a file inside.** The preview pane sees archives
//!   as directories, and a file inside one previews once extraction exists.

pub mod extract;
pub mod tar;
pub mod tree;
pub mod unpack;
pub mod zip;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub use extract::{destination_for, plan_extract, ExtractItem, ExtractPlan, SkipReason};
pub use unpack::{
    destinations, extract, plan_record, read_entry, Destinations, ExtractReport, CANCEL_CHECK_BYTES,
    EXTRACT_BUF,
};
pub use tree::{
    build, name_is_unsafe, normalize, ArchiveEntry, ArchiveTree, Method, RawEntry, MAX_ENTRIES,
    MAX_NAME_BYTES,
};

use crate::preview::sniff;

/// What kind of archive a file turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// A zip, or one of the formats that is a zip with a fixed first member —
    /// epub, docx, jar. Those list too, and browsing into a `.docx` to see its
    /// XML is a feature, not an accident.
    Zip,
    Tar,
    TarGz,
    TarXz,
    TarZst,
}

impl ArchiveFormat {
    pub fn label(self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::TarXz => "tar.xz",
            ArchiveFormat::TarZst => "tar.zst",
        }
    }

    /// The external program needed to read it, if any.
    pub fn decompressor(self) -> Option<&'static str> {
        match self {
            ArchiveFormat::Zip | ArchiveFormat::Tar => None,
            ArchiveFormat::TarGz => Some("gzip"),
            ArchiveFormat::TarXz => Some("xz"),
            ArchiveFormat::TarZst => Some("zstd"),
        }
    }

    /// Whether this format can be listed on this machine right now — false when
    /// its decompressor is not installed.
    pub fn is_available(self) -> bool {
        match self.decompressor() {
            None => true,
            Some(bin) => have(bin),
        }
    }
}

/// Everything listing an archive can fail at.
///
/// Its own enum rather than a [`DfError`](crate::DfError) variant, in the shape
/// [`crate::zoxide`] uses: the failures are format-specific and the messages want
/// the offset that went wrong, while the caller only ever turns them into one
/// line of toast. The [`From`] impl below is that conversion.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// An io failure from inside a parser, where the path is the caller's to add.
    #[error("{0}")]
    Read(#[source] std::io::Error),
    #[error("{}: not an archive", .0.display())]
    NotAnArchive(PathBuf),
    #[error("{format} archives are not supported")]
    Unsupported { format: &'static str },
    #[error("{format} archives need `{binary}`, which is not installed")]
    NoDecompressor {
        format: &'static str,
        binary: &'static str,
    },
    #[error("malformed {format}: {message}")]
    Malformed {
        format: &'static str,
        message: String,
    },
    #[error("truncated {format}: wanted {want} bytes at offset {at}")]
    Truncated {
        format: &'static str,
        at: u64,
        want: u64,
    },
}

impl ArchiveError {
    /// An io error from inside a parser, which has no path to name.
    pub(crate) fn io_bare(source: std::io::Error) -> ArchiveError {
        ArchiveError::Read(source)
    }

    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> ArchiveError {
        ArchiveError::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<ArchiveError> for crate::DfError {
    fn from(e: ArchiveError) -> crate::DfError {
        crate::DfError::Op(e.to_string())
    }
}

/// What `path` is, by its bytes.
///
/// Signature first, through [`crate::preview::sniff`], so the answer does not
/// depend on the name — an archive mailed with the extension stripped still
/// opens. The extension is consulted only for the one case bytes cannot settle:
/// a pre-POSIX tar, which has no magic anywhere in its header.
pub fn detect(path: &Path) -> Result<ArchiveFormat, ArchiveError> {
    let mut file = File::open(path).map_err(|e| ArchiveError::io(path, e))?;
    let mut head = vec![0u8; sniff::SNIFF_BYTES];
    let got = read_some(&mut file, &mut head).map_err(|e| ArchiveError::io(path, e))?;
    head.truncate(got);
    format_for(&head, path)
}

/// The format decision, split out so it is a table test.
pub fn format_for(head: &[u8], path: &Path) -> Result<ArchiveFormat, ArchiveError> {
    match sniff::sniff(head) {
        Some("application/zip") | Some("application/epub+zip") => return Ok(ArchiveFormat::Zip),
        Some("application/x-tar") => return Ok(ArchiveFormat::Tar),
        Some("application/gzip") => return Ok(ArchiveFormat::TarGz),
        Some("application/x-xz") => return Ok(ArchiveFormat::TarXz),
        Some("application/zstd") => return Ok(ArchiveFormat::TarZst),
        Some("application/x-7z-compressed") => {
            return Err(ArchiveError::Unsupported { format: "7z" })
        }
        Some("application/vnd.rar") => return Err(ArchiveError::Unsupported { format: "rar" }),
        Some("application/x-bzip2") => return Err(ArchiveError::Unsupported { format: "bzip2" }),
        _ => {}
    }
    // v7 tar: no magic, no signature, nothing but a plausible header. The
    // extension is all there is, and the parser's checksum check is what stops a
    // mis-named text file listing as an empty archive.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if ext.as_deref() == Some("tar") {
        return Ok(ArchiveFormat::Tar);
    }
    Err(ArchiveError::NotAnArchive(path.to_path_buf()))
}

/// List an archive as a browsable tree.
///
/// Blocking: it reads the file, and for a compressed tar it decompresses all of
/// it. Callers on the event loop must run it as a [`crate::tasks::Job`] — the
/// same rule every other read in this crate follows.
pub fn list(path: &Path) -> Result<ArchiveTree, ArchiveError> {
    let format = detect(path)?;
    let (raws, truncated) = match format {
        ArchiveFormat::Zip => {
            let mut file = File::open(path).map_err(|e| ArchiveError::io(path, e))?;
            let len = file
                .metadata()
                .map_err(|e| ArchiveError::io(path, e))?
                .len();
            zip::list(&mut file, len)?
        }
        ArchiveFormat::Tar => {
            let file = File::open(path).map_err(|e| ArchiveError::io(path, e))?;
            tar::list(file)?
        }
        other => list_compressed_tar(path, other)?,
    };
    Ok(tree::build(path.to_path_buf(), format, raws, truncated))
}

/// Stream a compressed tar through its decompressor and the tar parser.
///
/// `-d -c` — decompress to stdout — is spelled the same way by gzip, xz and
/// zstd, which is the only reason [`ArchiveFormat::decompressor`] can be a table
/// of program names rather than three special cases.
fn list_compressed_tar(
    path: &Path,
    format: ArchiveFormat,
) -> Result<(Vec<tree::RawEntry>, bool), ArchiveError> {
    let Some(binary) = format.decompressor() else {
        return Err(ArchiveError::Unsupported {
            format: format.label(),
        });
    };
    let file = File::open(path).map_err(|e| ArchiveError::io(path, e))?;

    // stdin is the file itself rather than a pipe we feed: the decompressor
    // reads it directly, so there is no second thread and no chance of the
    // classic deadlock where the parent blocks writing stdin while the child
    // blocks writing stdout.
    let mut child = Command::new(binary)
        .arg("-dc")
        .stdin(Stdio::from(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ArchiveError::NoDecompressor {
                    format: format.label(),
                    binary,
                }
            } else {
                ArchiveError::io(path, e)
            }
        })?;

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ArchiveError::Malformed {
            format: format.label(),
            message: format!("{binary} produced no output"),
        });
    };

    let result = tar::list(stdout);

    match &result {
        // Read to the end: the child is finished, and its exit status is the
        // only place a "corrupt archive" from the decompressor shows up.
        Ok((_, false)) => {
            let status = child.wait().map_err(|e| ArchiveError::io(path, e))?;
            if !status.success() {
                return Err(ArchiveError::Malformed {
                    format: format.label(),
                    message: format!("{binary} exited with {status}"),
                });
            }
        }
        // Stopped early, by a cap or by a parse failure. The child is still
        // writing into a pipe nobody is reading and has to be killed, or this
        // process leaves a stuck `zstd` behind every time a user glances at a
        // big archive.
        _ => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    result
}

/// Whether a program is on `PATH`.
///
/// Spawns it with `--version` rather than scanning `PATH` by hand, because that
/// is the same question the spawn will ask later and the answer cannot then
/// disagree. Used only by [`ArchiveFormat::is_available`], which exists so the UI
/// can grey a row out; the listing path finds out by spawning and reads the
/// `NotFound` directly.
fn have(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Read up to `buf.len()` bytes, tolerating short reads.
fn read_some(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut at = 0usize;
    while at < buf.len() {
        match file.read(&mut buf[at..]) {
            Ok(0) => break,
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(at)
}
