//! The directory-like view, and the name rules that make extraction safe.
//!
//! An archive is a flat list of names with slashes in them. A file manager
//! needs a tree: what is in the root, what is in `src/`, which of those are
//! directories. Two things stand between the two, and both are here.
//!
//! ## Directories that are not entries
//!
//! `tar czf` an ordinary tree and every directory gets its own header. `zip` a
//! selection of files and the directories may not be stored at all — `a/b/c.txt`
//! can be the only entry in the file, with nothing saying `a/` or `a/b/` exists.
//! So intermediate directories are **synthesized** from the prefixes, and marked
//! [`ArchiveEntry::synthesized`] so that extraction knows there is no header to
//! restore a mode or an mtime from.
//!
//! ## Names are hostile until proven otherwise
//!
//! This tree is what an extraction will later walk, so the checking happens
//! here, once, at listing time — not in the extractor, where forgetting it is a
//! CVE. Three rules, each flagged as [`ArchiveEntry::unsafe_name`]:
//!
//! - **Absolute** — `/etc/cron.d/x`, or a Windows drive or UNC path. Joining one
//!   onto a destination in Rust *discards the destination*: `Path::join` with an
//!   absolute argument returns the argument. That is the whole bug.
//! - **Traversal** — any `..` component. `../../.ssh/authorized_keys` escapes
//!   upward however deep the destination is.
//! - **A `.git` collision, case-insensitively** — `.git`, `.GIT`, `.Git`, and
//!   also `.git` anywhere in the path, not just at the root. Writing into a
//!   repository's `.git` is arbitrary code execution the next time any git
//!   command runs there (a hooks directory is executables), and the
//!   case-insensitive check is because macOS and Windows filesystems will happily
//!   resolve `.GIT` to it.
//!
//! Plus the mechanical ones: an empty name, a name with a NUL in it, a name
//! longer than [`MAX_NAME_BYTES`], and a component this platform will not make
//! ([`crate::path::name_is_valid`]) — on Windows `con.txt`, `x:y`, `a?` and a
//! trailing dot, which would fail, or worse, make a file other than the one
//! named.
//!
//! A `.` component is none of these. `./one.txt`, which `bsdtar` writes for
//! every member of an archive made with `bsdtar -cf x.zip .`, is `one.txt`
//! spelled with the directory it was made in, lists as `one.txt`, and extracts
//! to the same place; `./` alone is the root of the extraction and lists as
//! nothing. Only what [`normalize`] keeps can climb: a `..` stays a `..`.
//!
//! **The contract for extraction**: an entry with `unsafe_name` set must be
//! skipped, or renamed to something inside the destination — never joined onto
//! the destination path and never trusted. [`super::extract::plan_extract`]
//! implements the skip, and reports what it skipped so the UI can say so rather
//! than silently dropping files.
//!
//! Unsafe names are still *listed*, and still land in the tree: their components
//! are kept verbatim, so a traversing entry appears as a literal `..` directory
//! inside the archive view. Showing the user what is in the file they opened is
//! the job; refusing to show it teaches them nothing and hides the attack.

use std::collections::HashMap;
use std::path::PathBuf;

use super::ArchiveFormat;

/// The most entries a listing will hold: half a million.
///
/// A Linux kernel source tarball is about 90,000 files, and a repository of
/// build artefacts a few hundred thousand — so this is past every archive
/// anybody opens on purpose, and it is the number that stops a 40 KB zip bomb
/// declaring four billion entries from turning into a hundred gigabytes of
/// `String`. Past it the listing stops and [`ArchiveTree::truncated`] says so.
pub const MAX_ENTRIES: usize = 500_000;

/// The longest entry name kept: 4 KiB.
///
/// `PATH_MAX` on Linux is 4096, so a longer name cannot be extracted anyway.
/// Archives are the one place a 16 MB "filename" can arrive, because the length
/// is a field in the file rather than a thing the kernel enforced.
pub const MAX_NAME_BYTES: usize = 4096;

/// How an entry's bytes are stored.
///
/// Listing does not decompress anything, so this is metadata — but it is the
/// metadata that decides whether an extraction is possible at all, and it is
/// worth showing: a zip whose entries are `Method::Zstd` will not open in a
/// Windows 10 Explorer, and that is the kind of thing a user wants to know
/// before mailing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Not compressed. Also what every tar entry is — the compression in a
    /// `.tar.gz` wraps the whole stream, not the entry.
    Store,
    Deflate,
    Deflate64,
    Bzip2,
    Lzma,
    Xz,
    Zstd,
    Ppmd,
    /// A zip method code this build has no name for.
    Other(u16),
}

impl Method {
    /// The zip method codes, from APPNOTE.TXT §4.4.5.
    pub fn from_zip(code: u16) -> Method {
        match code {
            0 => Method::Store,
            8 => Method::Deflate,
            9 => Method::Deflate64,
            12 => Method::Bzip2,
            14 => Method::Lzma,
            93 => Method::Zstd,
            95 => Method::Xz,
            98 => Method::Ppmd,
            other => Method::Other(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Method::Store => "store",
            Method::Deflate => "deflate",
            Method::Deflate64 => "deflate64",
            Method::Bzip2 => "bzip2",
            Method::Lzma => "lzma",
            Method::Xz => "xz",
            Method::Zstd => "zstd",
            Method::Ppmd => "ppmd",
            Method::Other(_) => "unknown",
        }
    }
}

/// What a parser produces, before the tree exists.
///
/// The name is exactly as the archive spelled it — leading slashes, backslashes,
/// `..` and all. Sanitizing happens in [`build`], so that every format gets the
/// same rules and there is one place to read them.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub name: String,
    pub len: u64,
    pub compressed: u64,
    /// Unix seconds, when the format carried one that could be believed.
    pub mtime: Option<i64>,
    pub is_dir: bool,
    pub method: Method,
    pub encrypted: bool,
    /// Where a symlink or hardlink entry points. Tar only; zip stores link
    /// targets as file contents, which listing does not read.
    pub link_target: Option<String>,
}

impl RawEntry {
    /// A plain stored file, for the parsers to fill in from.
    pub fn new(name: String) -> RawEntry {
        RawEntry {
            name,
            len: 0,
            compressed: 0,
            mtime: None,
            is_dir: false,
            method: Method::Store,
            encrypted: false,
            link_target: None,
        }
    }
}

/// One row in the archive view.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// The last component — what the row shows.
    pub name: String,
    /// The full path inside the archive, normalized: `/`-separated, no leading
    /// or trailing slash, no `.` components. This is the key
    /// [`ArchiveTree::entries`] takes and the relative path an extraction joins.
    pub path: String,
    pub len: u64,
    pub compressed: u64,
    pub mtime: Option<i64>,
    pub is_dir: bool,
    pub method: Method,
    /// The zip general-purpose bit 0. Listing works; reading the bytes would
    /// need a password.
    pub encrypted: bool,
    /// See the module essay. Extraction must skip or rename this entry.
    pub unsafe_name: bool,
    /// This directory had no header of its own; it exists because something
    /// inside it did.
    pub synthesized: bool,
    pub link_target: Option<String>,
}

impl ArchiveEntry {
    fn dir(path: String, name: String) -> ArchiveEntry {
        // A synthesized directory inherits the safety of the prefix it stands
        // for. `../../etc/passwd` synthesizes a `..` and a `../..`, and those are
        // every bit as unsafe to join onto a destination as the leaf is — more
        // so, since `dest.join("..")` is lexically still "under" the destination
        // and would slip past a naive prefix check.
        let unsafe_name = name_is_unsafe(&path);
        ArchiveEntry {
            name,
            path,
            len: 0,
            compressed: 0,
            mtime: None,
            is_dir: true,
            method: Method::Store,
            encrypted: false,
            unsafe_name,
            synthesized: true,
            link_target: None,
        }
    }

    pub fn is_link(&self) -> bool {
        self.link_target.is_some()
    }
}

/// A listed archive, viewable as directories.
#[derive(Debug, Clone)]
pub struct ArchiveTree {
    path: PathBuf,
    format: ArchiveFormat,
    nodes: Vec<ArchiveEntry>,
    index: HashMap<String, usize>,
    /// Parent path (`""` for the root) to child node indices, pre-sorted.
    children: HashMap<String, Vec<usize>>,
    file_count: usize,
    dir_count: usize,
    total_len: u64,
    truncated: bool,
    encrypted: bool,
    unsafe_count: usize,
}

impl ArchiveTree {
    /// The archive file itself.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn format(&self) -> ArchiveFormat {
        self.format
    }

    /// The rows for one directory inside the archive. `""` is the root.
    ///
    /// Sorted directories-first then by name, so a caller that has no opinion
    /// gets a sensible listing; df-app re-sorts with the pane's own
    /// [`SortOptions`](crate::fs::SortOptions) when the user has one.
    pub fn entries(&self, inner: &str) -> Vec<ArchiveEntry> {
        let key = normalize(inner).unwrap_or_default();
        match self.children.get(&key) {
            Some(ids) => ids.iter().map(|i| self.nodes[*i].clone()).collect(),
            None => Vec::new(),
        }
    }

    /// One entry by its full inner path.
    pub fn get(&self, inner: &str) -> Option<&ArchiveEntry> {
        let key = normalize(inner)?;
        self.index.get(&key).map(|i| &self.nodes[*i])
    }

    /// Whether this inner path names a directory that can be entered. The root
    /// always can, even for an empty archive.
    pub fn is_dir(&self, inner: &str) -> bool {
        match normalize(inner) {
            None => true,
            Some(key) => self.index.get(&key).is_some_and(|i| self.nodes[*i].is_dir),
        }
    }

    /// Every node, synthesized directories included. The order is the order the
    /// archive listed them, with synthesized directories interleaved where they
    /// were first needed.
    pub fn all(&self) -> &[ArchiveEntry] {
        &self.nodes
    }

    /// Files, not counting directories.
    pub fn file_count(&self) -> usize {
        self.file_count
    }

    /// Directories, synthesized ones included.
    pub fn dir_count(&self) -> usize {
        self.dir_count
    }

    /// Uncompressed bytes across every file. What an extraction will need.
    pub fn total_len(&self) -> u64 {
        self.total_len
    }

    /// Whether a cap cut the listing short. The tree is still valid — it is just
    /// not the whole archive, and the UI should say so rather than implying the
    /// archive ends there.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Whether any entry needs a password to read.
    pub fn has_encrypted(&self) -> bool {
        self.encrypted
    }

    /// How many entries carry [`ArchiveEntry::unsafe_name`]. Non-zero is worth
    /// a warning in the UI: a well-formed archive has none.
    pub fn unsafe_count(&self) -> usize {
        self.unsafe_count
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Turn a parser's flat list into a tree.
pub fn build(
    path: PathBuf,
    format: ArchiveFormat,
    raws: Vec<RawEntry>,
    truncated: bool,
) -> ArchiveTree {
    let mut tree = ArchiveTree {
        path,
        format,
        nodes: Vec::with_capacity(raws.len()),
        index: HashMap::with_capacity(raws.len()),
        children: HashMap::new(),
        file_count: 0,
        dir_count: 0,
        total_len: 0,
        truncated,
        encrypted: false,
        unsafe_count: 0,
    };

    for raw in raws {
        let flagged = name_is_unsafe(&raw.name);
        if flagged {
            tree.unsafe_count += 1;
        }
        if raw.encrypted {
            tree.encrypted = true;
        }
        let Some(key) = normalize(&raw.name) else {
            // A name that normalizes to nothing — `/`, `.`, `./` — describes the
            // archive root, which already exists, and is dropped. `/` was
            // counted as unsafe above, being absolute; `./` and `.` are the
            // root spelled relatively, which is not.
            continue;
        };
        ensure_parents(&mut tree, &key);

        let name = leaf(&key).to_string();
        let entry = ArchiveEntry {
            name,
            path: key.clone(),
            len: if raw.is_dir { 0 } else { raw.len },
            compressed: raw.compressed,
            mtime: raw.mtime,
            is_dir: raw.is_dir,
            method: raw.method,
            encrypted: raw.encrypted,
            unsafe_name: flagged,
            synthesized: false,
            link_target: raw.link_target,
        };

        match tree.index.get(&key).copied() {
            Some(at) => {
                // A real header for a directory this tree already synthesized —
                // or a duplicate name, which zip permits and which is resolved
                // the way every unzip resolves it: the last one wins. The
                // directory bit is sticky, because children are already hanging
                // off this node and turning it into a file would orphan them.
                let was_dir = tree.nodes[at].is_dir;
                tree.nodes[at] = entry;
                if was_dir {
                    tree.nodes[at].is_dir = true;
                    tree.nodes[at].len = 0;
                }
            }
            None => {
                tree.index.insert(key.clone(), tree.nodes.len());
                tree.nodes.push(entry);
            }
        }
    }

    // Counts and the child map, from the settled node list — computed after the
    // fact rather than incrementally, because a synthesized directory can be
    // replaced by a real file header partway through and the counts would drift.
    for (i, node) in tree.nodes.iter().enumerate() {
        if node.is_dir {
            tree.dir_count += 1;
        } else {
            tree.file_count += 1;
            tree.total_len = tree.total_len.saturating_add(node.len);
        }
        tree.children.entry(parent(&node.path)).or_default().push(i);
    }

    let order: Vec<(String, bool, String)> = tree
        .nodes
        .iter()
        .map(|n| (n.path.clone(), n.is_dir, n.name.to_lowercase()))
        .collect();
    for ids in tree.children.values_mut() {
        ids.sort_by(|a, b| {
            let (pa, da, la) = &order[*a];
            let (pb, db, lb) = &order[*b];
            db.cmp(da).then_with(|| la.cmp(lb)).then_with(|| pa.cmp(pb))
        });
    }

    tree
}

/// Create the synthesized directories `key` hangs from.
fn ensure_parents(tree: &mut ArchiveTree, key: &str) {
    for (i, b) in key.as_bytes().iter().enumerate() {
        if *b != b'/' {
            continue;
        }
        let prefix = &key[..i];
        if !tree.index.contains_key(prefix) {
            let entry = ArchiveEntry::dir(prefix.to_string(), leaf(prefix).to_string());
            tree.index.insert(prefix.to_string(), tree.nodes.len());
            tree.nodes.push(entry);
        }
    }
}

fn leaf(key: &str) -> &str {
    match key.rfind('/') {
        Some(i) => &key[i + 1..],
        None => key,
    }
}

fn parent(key: &str) -> String {
    match key.rfind('/') {
        Some(i) => key[..i].to_string(),
        None => String::new(),
    }
}

/// The normalized tree key for an archive name, or `None` if there is nothing
/// left of it.
///
/// Splits on both separators — zips written on Windows use `\` and the spec has
/// said not to since 1990 — drops empty and `.` components, and keeps everything
/// else verbatim, `..` included. Keeping `..` is deliberate: dropping it would
/// silently turn `../etc/passwd` into `etc/passwd` and hand a *plausible*
/// extraction path to a caller who forgot to check [`ArchiveEntry::unsafe_name`].
/// A literal `..` directory in the listing cannot be mistaken for anything.
pub fn normalize(name: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in name.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Whether extraction must refuse to join this name onto a destination.
///
/// See the module essay for each rule and why it is there.
pub fn name_is_unsafe(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.contains('\0') {
        return true;
    }
    // Absolute, in any dialect: POSIX, a Windows drive, and a UNC share (which
    // starts with the backslash form and is therefore already caught, but is
    // worth being explicit about).
    if name.starts_with('/') || name.starts_with('\\') {
        return true;
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return true;
    }
    for part in name.split(['/', '\\']) {
        if part == ".." {
            return true;
        }
        if part.eq_ignore_ascii_case(".git") {
            return true;
        }
        // A name the platform will not make: on Windows `con.txt`, `x:y`,
        // `a?`, a trailing dot. Unix refuses only NUL and `/`, which the
        // checks above have already caught.
        if !part.is_empty()
            && part != "."
            && crate::path::name_is_valid(std::ffi::OsStr::new(part)).is_err()
        {
            return true;
        }
    }
    // What is left is relative and climbs nowhere. A `.` component, leading
    // or not, is a harmless spelling of the same path — `./a` is `a` — and
    // a name that is nothing else (`./`, `.`, which `bsdtar -cf x.zip .`
    // writes for the folder it was run in) is the root of the extraction,
    // which already exists: [`build`] lists nothing for it and
    // [`super::unpack`] never opens it.
    false
}
