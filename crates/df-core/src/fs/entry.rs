//! One row: everything the list, the sorts and the linemodes need about a file,
//! read once at scan time.
//!
//! The rule this file follows is **stat once**. A row can be drawn with a size,
//! a permission string, an owner, two timestamps, an icon and a git dot; if any
//! of those went back to the filesystem at paint time, scrolling a large
//! directory would be a syscall storm. So [`Entry`] is a value — plain owned
//! data, [`Clone`], no handles, no lazy fields — and everything downstream
//! (sorting, filtering, linemodes) is a pure function over it.
//!
//! The one fact read beyond the `statx` is the file's tags — one `lgetxattr`
//! per row (see [`super::tags`]) — and it is read here for the same reason:
//! the tag dots are drawn, and the `#tag` filter asked, of every visible row.
//! Only on a local filesystem, though: the scanner asks `statfs` once per
//! directory ([`super::tags::read_here`]), and on a network or FUSE mount,
//! where each read would be a round trip, the rows carry none.
//!
//! Symlinks get read twice on purpose: `lstat` to know it *is* a link, then a
//! follow to learn what it points at. The size, times and mode reported are the
//! **target's** when the link resolves, because "how big is this file" is a
//! question about the file, not about the 12 bytes of path that name it. A
//! broken link keeps the link's own metadata and says so.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::mime;
use crate::{DfError, Result};

/// What a name refers to.
///
/// Symlinks carry their resolved kind rather than collapsing into it, because
/// the two facts are needed in different places: `→` on a link-to-directory
/// enters it (so it must sort and behave as a directory), while the row still
/// has to be drawn as a link with an arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A symlink and what it resolves to. `None` is a broken link — the target
    /// does not exist, or the chain loops (`ELOOP`).
    Symlink {
        target: Option<LinkTarget>,
    },
}

/// What a symlink resolved to. Deliberately not `Kind` — a link to a link
/// resolves all the way through, so this can never be another `Symlink`, and
/// the type says so instead of a comment saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTarget {
    File,
    Dir,
    /// A socket, fifo, or device node. Not openable, not enterable, but not
    /// broken either — and lumping these in with `File` would let `→` try to
    /// preview a block device.
    Other,
}

/// One directory entry, fully stat'd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file name alone. Owned rather than borrowed from the path because
    /// every sort, filter and paint reads it and none of them want a
    /// `file_name()` + `to_string_lossy()` per access.
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    /// Bytes, from the target for a resolving symlink.
    ///
    /// **Zero for directories**, always. PLAN §4.1's size sort wants directories
    /// ordered by how much is *in* them, which costs a recursive walk (PLAN
    /// §7.3's "what's big" mode is that walk). Until it exists, reporting the
    /// 4096-byte inode size would sort directories by nothing meaningful and
    /// look like a bug; zero is honestly "not known yet", sorts them as one
    /// block, and is the field that mode will fill in.
    pub len: u64,
    /// Last modification. `None` only if the platform refused it.
    pub mtime: Option<SystemTime>,
    /// Creation time, via `statx`. `None` on filesystems that do not store one
    /// — ext4 without `inode_size >= 256`, most network mounts — which is why
    /// the btime linemode and sort both have to tolerate a hole.
    pub btime: Option<SystemTime>,
    /// The raw `st_mode`, permission bits and file-type bits together, so the
    /// permissions linemode and the spot panel's editor read the same number.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// A leading dot. Unix's whole definition, and the `.` toggle's subject.
    pub is_hidden: bool,
    /// A guess from the name (see [`super::mime`]). Cheap enough to do for
    /// every entry in a 200k directory; replaced by real sniffing for the rows
    /// a preview actually opens.
    pub mime: &'static str,
    /// What sort of thing this is (see [`super::kind`]), settled here rather
    /// than asked per paint.
    ///
    /// Every input it is derived from — the name, the kind, the mime hint, the
    /// mode — is final by the time an entry exists, and the two painters that
    /// want it (the icon glyph and the name colour) ask for every visible row
    /// on every frame. Classifying there meant two walks of three tables and
    /// three lowercase `String`s per row per frame; a `Copy` byte on the row
    /// costs the scan one table walk it was already doing for the mime hint.
    pub file_kind: super::FileKind,
    /// The file's tags ([`super::tags`]), as it carries them: read here, with
    /// the one `lgetxattr` a row costs, because the list pane draws a dot per
    /// coloured tag on every visible row of every frame, and `f #red` filters
    /// by them. Empty for a file with none, for every row on a network or
    /// FUSE mount ([`super::tags::read_here`]), and always for an archive's,
    /// a remote service's and the trash's rows, which are not files on this
    /// disk that could carry any.
    pub tags: Vec<String>,
}

impl Entry {
    /// Read one entry, given its path. The name is the path's last component.
    /// Its tags are read when its directory is on a local filesystem.
    pub fn read(path: impl Into<PathBuf>) -> Result<Entry> {
        let path = path.into();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            // A path with no last component is `/` or `..`; `/` is the only one
            // that reaches here and it is its own name.
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let link_meta =
            std::fs::symlink_metadata(&path).map_err(|e| DfError::io(path.clone(), e))?;
        let tags = path.parent().is_none_or(super::tags::read_here);
        Ok(Entry::from_parts(name, path, link_meta, tags))
    }

    /// Build from a name, a path and the result of an `lstat`, following the
    /// link if it is one. Split out from [`Entry::read`] so the scanner can
    /// reuse the `DirEntry` it already has instead of re-`lstat`ing. `tags`
    /// says whether to read the file's tags — the scanner's one answer for
    /// the whole directory ([`super::tags::read_here`]).
    pub(crate) fn from_parts(
        name: String,
        path: PathBuf,
        link_meta: std::fs::Metadata,
        tags: bool,
    ) -> Entry {
        use std::os::unix::fs::MetadataExt;

        let is_hidden = name.starts_with('.');
        let (kind, meta) = if link_meta.file_type().is_symlink() {
            match std::fs::metadata(&path) {
                Ok(target) => {
                    let target_kind = if target.is_dir() {
                        LinkTarget::Dir
                    } else if target.is_file() {
                        LinkTarget::File
                    } else {
                        LinkTarget::Other
                    };
                    (
                        Kind::Symlink {
                            target: Some(target_kind),
                        },
                        target,
                    )
                }
                // Broken: keep the link's own metadata. Its mtime is when the
                // link was made, which is the only true thing left to show.
                Err(_) => (Kind::Symlink { target: None }, link_meta),
            }
        } else if link_meta.is_dir() {
            (Kind::Dir, link_meta)
        } else {
            (Kind::File, link_meta)
        };

        let is_dir = matches!(
            kind,
            Kind::Dir
                | Kind::Symlink {
                    target: Some(LinkTarget::Dir)
                }
        );
        let broken = matches!(kind, Kind::Symlink { target: None });
        let mime = if is_dir {
            mime::DIR_MIME
        } else if broken {
            mime::BROKEN_LINK_MIME
        } else {
            mime::hint_for_name(&name)
        };
        let file_kind = super::kind::classify(kind, &name, mime, meta.mode());
        // The link's own attributes, never its target's: a symlink carries no
        // `user.*` attribute on Linux, so a link row has no tags even when the
        // file it points at does — tagging the row tags the thing the row is.
        let tags = if tags {
            super::tags::read(&path)
        } else {
            Vec::new()
        };

        Entry {
            name,
            path,
            kind,
            len: if is_dir { 0 } else { meta.len() },
            mtime: meta.modified().ok(),
            btime: meta.created().ok(),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            is_hidden,
            mime,
            file_kind,
            tags,
        }
    }

    /// Whether `→` enters this: a directory, or a symlink to one.
    pub fn is_dir(&self) -> bool {
        matches!(
            self.kind,
            Kind::Dir
                | Kind::Symlink {
                    target: Some(LinkTarget::Dir)
                }
        )
    }

    pub fn is_symlink(&self) -> bool {
        matches!(self.kind, Kind::Symlink { .. })
    }

    /// A symlink whose target is gone. Drawn in red, never previewed, and still
    /// deletable — which is the whole reason it is listed at all.
    pub fn is_broken_symlink(&self) -> bool {
        matches!(self.kind, Kind::Symlink { target: None })
    }

    /// Where a symlink points, unresolved (the literal target path, which may
    /// be relative). Read on demand: the spot panel wants it, the list does not,
    /// and it is a `readlink` per row.
    pub fn link_target(&self) -> Option<PathBuf> {
        if !self.is_symlink() {
            return None;
        }
        std::fs::read_link(&self.path).ok()
    }

    /// The extension, lowercased, empty when there isn't one.
    ///
    /// A leading dot is the hidden marker rather than an extension separator, so
    /// `.gitignore` has no extension — otherwise the extension sort would file
    /// every dotfile under its own name.
    pub fn extension(&self) -> &str {
        let stem = self.name.strip_prefix('.').unwrap_or(&self.name);
        match stem.rsplit_once('.') {
            Some((_, ext)) => ext,
            None => "",
        }
    }

    /// The name without its extension — what `R` renames and what `r` selects.
    pub fn stem(&self) -> &str {
        let ext = self.extension();
        if ext.is_empty() {
            return &self.name;
        }
        &self.name[..self.name.len() - ext.len() - 1]
    }

    /// `drwxr-xr-x`, for the permissions linemode and the spot panel.
    pub fn permissions_string(&self) -> String {
        let mut s = String::with_capacity(10);
        s.push(match self.kind {
            Kind::Dir => 'd',
            Kind::Symlink { .. } => 'l',
            Kind::File => match self.mode & 0o170000 {
                0o140000 => 's', // socket
                0o120000 => 'l', // symlink (unreachable here, kept for honesty)
                0o060000 => 'b', // block device
                0o020000 => 'c', // character device
                0o010000 => 'p', // fifo
                _ => '-',
            },
        });
        for shift in [6, 3, 0] {
            let bits = (self.mode >> shift) & 0o7;
            s.push(if bits & 0o4 != 0 { 'r' } else { '-' });
            s.push(if bits & 0o2 != 0 { 'w' } else { '-' });
            s.push(if bits & 0o1 != 0 { 'x' } else { '-' });
        }
        // setuid/setgid/sticky overwrite the execute slot, as `ls` draws them:
        // an uppercase letter means the bit is set with no execute under it.
        let special = [
            (0o4000, 3, 's', 'S'),
            (0o2000, 6, 's', 'S'),
            (0o1000, 9, 't', 'T'),
        ];
        for (bit, index, with_x, without_x) in special {
            if self.mode & bit != 0 {
                let bytes = unsafe_free_replace(&s, index, with_x, without_x);
                s = bytes;
            }
        }
        s
    }

    /// The owner as the linemode draws it.
    pub fn owner_label(&self) -> String {
        super::owner::owner_label(self.uid, self.gid)
    }

    /// The parent directory, or `None` at the root.
    pub fn parent(&self) -> Option<&Path> {
        self.path.parent()
    }
}

/// Swap the execute character at `index` for its setuid/sticky spelling.
/// Ascii-only by construction (the string was built from ascii above), so byte
/// indexing is safe without any unsafe — hence the name.
fn unsafe_free_replace(s: &str, index: usize, with_x: char, without_x: char) -> String {
    let mut out: Vec<char> = s.chars().collect();
    if index >= out.len() {
        return s.to_string();
    }
    out[index] = if out[index] == 'x' { with_x } else { without_x };
    out.into_iter().collect()
}
