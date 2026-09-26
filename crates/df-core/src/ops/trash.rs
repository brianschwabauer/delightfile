//! The freedesktop.org trash, hand-rolled.
//!
//! `d` must always be reversible (PLAN §5), and the way to get that for free on
//! Linux is the trash spec every other desktop already implements: move the
//! file into `$XDG_DATA_HOME/Trash/files/`, and write a sibling
//! `Trash/info/<name>.trashinfo` recording where it came from and when it left.
//! Restore is that record, played backwards — which is exactly what `u` needs.
//!
//! Implemented against the spec rather than pulled in as a dependency, in the
//! same spirit as the hand-rolled TOML and D-Bus (PLAN §1). The whole format is
//! three lines of ini and a percent-encoded path.
//!
//! ## Trash directories (spec §Trash directories)
//!
//! A file can only be *moved* into a trash on its own filesystem; anything else
//! is a copy, and a copy is not what "delete" should cost. So:
//!
//! 1. If the file lives on the same device as the home trash, the home trash is
//!    used. This is the overwhelmingly common case.
//! 2. Otherwise the file's mount point (its "top directory") is found, and the
//!    spec's two candidates are tried in order: `$topdir/.Trash/$uid` — but only
//!    if `$topdir/.Trash` exists, is a real directory rather than a symlink, and
//!    has the sticky bit, since without those it is a trap somebody else can
//!    set — then `$topdir/.Trash-$uid`, created if missing.
//! 3. If neither can be used (a read-only mount, a filesystem with no room, a
//!    permissions problem), delightfile **falls back to the home trash** and
//!    accepts the cross-device copy. The alternative is refusing to delete the
//!    file at all, and a `d` that sometimes does nothing is worse than a `d`
//!    that is sometimes slow. The copy is progress-reported and cancellable
//!    like any other, and the original is removed only after it has landed.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::{exists, file_name, normalize};

/// The suffix on every info file, per the spec.
pub const TRASHINFO_EXT: &str = "trashinfo";

/// How many `name_1`, `name_2`… variants to try before giving up on a name
/// collision inside the trash.
///
/// 1000 is far past any real case (it means a thousand files of the same name
/// deleted without ever emptying the trash) and stops a filesystem that lies
/// about `EEXIST` from spinning forever.
pub const MAX_TRASH_COLLISIONS: u32 = 1000;

/// The longest a single path component may be on ext4/btrfs/xfs. Names are
/// shortened to leave room for a `_12` suffix rather than failing with
/// `ENAMETOOLONG` on a name the user cannot see the length of.
pub const MAX_NAME_BYTES: usize = 255;

/// One trashed thing, and everything needed to put it back.
///
/// Carries its own trash root, so undo restores from the trash the file
/// actually went into — which may not be the home one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    /// The `…/Trash` directory, not `…/Trash/files`.
    pub trash_root: PathBuf,
    /// The name inside `files/` — the original name, or a `_1`-suffixed
    /// variant if that was taken.
    pub name: OsString,
    /// Where it came from, absolute.
    pub original: PathBuf,
    /// `YYYY-MM-DDThh:mm:ss`, as written into the info file.
    pub deleted_at: String,
}

impl TrashedItem {
    pub fn files_path(&self) -> PathBuf {
        self.trash_root.join("files").join(&self.name)
    }

    /// Whether this is a file in `files/` that no `.trashinfo` describes.
    ///
    /// An orphan has nowhere to be restored *to* — that is the whole content of
    /// the record it is missing — so every caller that would move it back asks
    /// this first. See [`Trash::orphans`] for where they come from.
    pub fn is_orphan(&self) -> bool {
        self.original.as_os_str().is_empty()
    }

    pub fn info_path(&self) -> PathBuf {
        let mut name = self.name.clone();
        name.push(".");
        name.push(TRASHINFO_EXT);
        self.trash_root.join("info").join(name)
    }
}

/// A trash directory: `…/Trash`, with `files/` and `info/` inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trash {
    root: PathBuf,
}

impl Trash {
    /// A trash rooted at an explicit directory. The whole spec works the same
    /// wherever the root is, which is what makes it testable.
    pub fn at(root: impl Into<PathBuf>) -> Trash {
        Trash { root: root.into() }
    }

    /// The user's home trash, from `$XDG_DATA_HOME` or `$HOME/.local/share`.
    pub fn home() -> Result<Trash> {
        let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| DfError::Op("$HOME is not set: no home trash".to_string()))?;
        Ok(Trash::at(home_trash_path(xdg.as_deref(), &home)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    pub fn info_dir(&self) -> PathBuf {
        self.root.join("info")
    }

    /// Make `files/` and `info/` if they are missing. Mode 0700: a trash is
    /// private, and the spec's `$topdir/.Trash-$uid` requires it.
    pub fn ensure(&self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        for dir in [self.files_dir(), self.info_dir()] {
            if !exists(&dir) {
                std::fs::create_dir_all(&dir).map_err(|e| DfError::io(&dir, e))?;
                if let Err(e) =
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                {
                    log::warn!("could not tighten {}: {e}", dir.display());
                }
            }
        }
        Ok(())
    }

    /// Move `path` into this trash.
    ///
    /// The info file is written *first* and with `create_new`, which is what
    /// claims the name: two delightfiles trashing `notes.txt` at the same
    /// instant cannot both win, and a crash between the two steps leaves an
    /// orphan info file rather than a file in the trash nobody can restore.
    pub fn trash(&self, path: &Path, ctx: &TaskCtx) -> Result<TrashedItem> {
        if !exists(path) {
            return Err(DfError::io(
                path,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            ));
        }
        // The same rails as a permanent delete: trashing `$HOME` is no more
        // sensible than deleting it, and it would be undoable only in theory.
        super::delete::check_deletable_here(path)?;

        let original = normalize(path);
        self.ensure()?;

        let base = file_name(&original)?.to_os_string();
        let (name, info_path) = self.claim_name(&base)?;
        let deleted_at = iso8601_utc(SystemTime::now());

        let info = trashinfo_text(&original, &deleted_at);
        if let Err(e) = std::fs::write(&info_path, info) {
            let _ignored = std::fs::remove_file(&info_path);
            return Err(DfError::io(&info_path, e));
        }

        let dst = self.files_dir().join(&name);
        let moved = match std::fs::rename(path, &dst) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                super::copy::move_cross_device(path, &dst, ctx).map(|_stats| ())
            }
            Err(e) => Err(DfError::io(path, e)),
        };
        if let Err(e) = moved {
            // The file never arrived; the reservation must not outlive it.
            let _ignored = std::fs::remove_file(&info_path);
            let _ignored = super::delete::remove_tree_unchecked(&dst);
            return Err(e);
        }

        Ok(TrashedItem {
            trash_root: self.root.clone(),
            name,
            original,
            deleted_at,
        })
    }

    /// Reserve a free name in this trash by creating its info file.
    fn claim_name(&self, base: &OsStr) -> Result<(OsString, PathBuf)> {
        // `<name>.trashinfo` must itself fit in a 255-byte component, so the
        // name in `files/` is clipped to leave room for it. Only the trash-side
        // name is shortened; the recorded original path is untouched.
        let room = MAX_NAME_BYTES - TRASHINFO_EXT.len() - 1;
        for n in 0..MAX_TRASH_COLLISIONS {
            let candidate = if n == 0 {
                fit(base, None, room)
            } else {
                fit(base, Some(n), room)
            };
            let mut info_name = candidate.clone();
            info_name.push(".");
            info_name.push(TRASHINFO_EXT);
            let info_path = self.info_dir().join(&info_name);

            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&info_path)
            {
                Ok(_file) => {
                    if exists(&self.files_dir().join(&candidate)) {
                        // An info-less file already sits under that name (a
                        // crashed trash, or another tool's). Give the name back
                        // and try the next one.
                        let _ignored = std::fs::remove_file(&info_path);
                        continue;
                    }
                    return Ok((candidate, info_path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(DfError::io(&info_path, e)),
            }
        }
        Err(DfError::Op(format!(
            "{}: {} names of this shape are already in the trash",
            base.to_string_lossy(),
            crate::text::grouped(u64::from(MAX_TRASH_COLLISIONS))
        )))
    }

    /// Every item currently in this trash, for the `trash://` view and for
    /// tests. Info files that cannot be parsed are logged and skipped rather
    /// than failing the whole listing.
    pub fn list(&self) -> Result<Vec<TrashedItem>> {
        let dir = self.info_dir();
        if !exists(&dir) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| DfError::io(&dir, e))? {
            let entry = entry.map_err(|e| DfError::io(&dir, e))?;
            let info_path = entry.path();
            // Bytes, not `to_str`: `claim_name` builds the trash name from the
            // original's bytes, so a file whose name is not UTF-8 — off a FAT
            // stick, out of an archive, from another machine's rsync — gets a
            // `.trashinfo` that is not UTF-8 either. Skipping those made them
            // invisible in `trash://`, impossible to restore, and *survivors of
            // "empty trash"*, which reported them destroyed.
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            let suffix = format!(".{TRASHINFO_EXT}");
            let Some(file) = info_path.file_name() else {
                continue;
            };
            let bytes = file.as_bytes();
            if !bytes.ends_with(suffix.as_bytes()) {
                continue;
            }
            let name = OsString::from_vec(bytes[..bytes.len() - suffix.len()].to_vec());
            let text = match std::fs::read_to_string(&info_path) {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("unreadable {}: {e}", info_path.display());
                    continue;
                }
            };
            match parse_trashinfo(&text) {
                Ok((original, deleted_at)) => out.push(TrashedItem {
                    trash_root: self.root.clone(),
                    name,
                    original,
                    deleted_at,
                }),
                Err(e) => log::warn!("unparseable {}: {e}", info_path.display()),
            }
        }
        out.extend(self.orphans(&out)?);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// The things in `files/` that no record points at.
    ///
    /// A trash is two directories that have to agree, and they can stop
    /// agreeing: a crash between [`Trash::trash`]'s two steps, another tool's
    /// half-done delete, and — the reason this exists — a [`purge`] that was
    /// **cancelled after it had removed the record**. Every one of those leaves
    /// bytes in `files/` that used to be invisible: not listed, not restorable,
    /// not destroyed by "empty trash", and reported as gone.
    ///
    /// They are listed instead, with an empty `original`, which is what
    /// [`TrashedItem::is_orphan`] reads. A restore of one is refused by name
    /// (there is nowhere to put it back), and `D` destroys it — so the trash
    /// view is a complete account of what is in the trash directory, which is
    /// the only version of that view worth trusting.
    fn orphans(&self, known: &[TrashedItem]) -> Result<Vec<TrashedItem>> {
        let dir = self.files_dir();
        if !exists(&dir) {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| DfError::io(&dir, e))? {
            let entry = entry.map_err(|e| DfError::io(&dir, e))?;
            let name = entry.file_name();
            if known.iter().any(|item| item.name == name) {
                continue;
            }
            // Cross-checked against `info/` rather than against `known` alone:
            // a record that failed to *parse* was logged and skipped above, and
            // its file is not an orphan — it is a record this program could not
            // read, and listing it twice would be worse than listing it once.
            let mut info_name = name.clone();
            info_name.push(".");
            info_name.push(TRASHINFO_EXT);
            if exists(&self.info_dir().join(&info_name)) {
                continue;
            }
            out.push(TrashedItem {
                trash_root: self.root.clone(),
                name,
                original: PathBuf::new(),
                deleted_at: String::new(),
            });
        }
        Ok(out)
    }

    /// Put an item back where it came from.
    pub fn restore(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
        restore(item, ctx)
    }

    /// Destroy one item for good — the trash view's `D` (PLAN §7.4).
    pub fn purge(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<()> {
        purge(item, ctx)
    }
}

/// Destroy a trashed item for good: its record *and* its file.
///
/// The one operation in the program with no inverse, and it is spelled here
/// rather than as "delete the path inside `files/`" at the call site because
/// deleting only the file would leave an info record pointing at nothing — a
/// row in the trash view that can never be restored and never goes away.
///
/// ## The record goes first, and that order is the safety property
///
/// **The bug this fixes**: the file went first. A purge of a directory is a
/// recursive [`delete_permanent`](super::delete::delete_permanent) that honours
/// cancel — so `D`, then `x` in the task panel, left half a tree in `files/`
/// **behind an intact `.trashinfo`**. The row was still in the trash view, the
/// record still said "this is your project directory", and `Enter` on it moved
/// a silently truncated copy back over the name it came from. That is data loss
/// wearing the mask of a restore, and it is the worst outcome this file can
/// produce.
///
/// Removing the record first cannot produce it. What a cancelled or failed
/// purge leaves is an **orphan**: bytes in `files/` that no record describes,
/// which [`Trash::orphans`] lists as a row with no origin — visible, refused by
/// [`restore`] with a sentence, and destroyed by the next `D` or "Empty trash".
/// A user who cancels sees a broken thing that says it is broken instead of a
/// whole-looking thing that is not.
///
/// The two alternatives were weighed and rejected. *Resumable* means recording
/// progress into the trash directory — a third kind of state to get out of step
/// with the other two. *Forbidding cancel* means a `D` on a hundred-gigabyte
/// directory cannot be stopped, which is a worse promise than "you may be left
/// with a broken row you can see".
pub fn purge(item: &TrashedItem, ctx: &TaskCtx) -> Result<()> {
    let info = item.info_path();
    match std::fs::remove_file(&info) {
        Ok(()) => {}
        // Already gone is the outcome that was asked for — an orphan being
        // purged has no record to remove, and this is that path too.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(DfError::io(&info, e)),
    }
    let file = item.files_path();
    if exists(&file) {
        super::delete::delete_permanent(&file, ctx)?;
    }
    Ok(())
}

/// Whether a trashinfo's `Path=` names somewhere a restore may write.
///
/// A `.trashinfo` is a text file in a directory, and the spec constrains
/// nothing about what `Path=` says. Most of them this program wrote — but a
/// `$topdir/.Trash-$uid` on a stick someone handed you, an archive extracted
/// over `~/.local/share/Trash`, and a trash directory that came back from a
/// backup are all records this code did not author. So the destination is
/// input, and two rules make it safe to rename into:
///
/// - **Absolute.** A relative `Path=notes.txt` resolves against whatever
///   directory the app was launched from, which turns "restore" into "write a
///   file of my choosing into the user's cwd".
/// - **No `..`.** `rename(2)` resolves those against the real tree, so a
///   lexical normalisation would not even describe where the file lands.
fn restorable_destination(original: &Path) -> bool {
    original.is_absolute()
        && !original
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Put a trashed item back where it came from — the inverse `u` runs.
///
/// Refuses if something has taken the original name in the meantime: undo may
/// never overwrite newer work (PLAN §5), and a restore that silently clobbers
/// would be exactly that.
pub fn restore(item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
    // An orphan has no `Path=` to go back to, and saying that is more use than
    // the generic refusal an empty path would otherwise fall into.
    if item.is_orphan() {
        return Err(DfError::Op(format!(
            "{} has no record in the trash, so there is nowhere to put it back — D destroys it",
            item.name.to_string_lossy()
        )));
    }
    if !restorable_destination(&item.original) {
        return Err(DfError::Op(format!(
            "{} is not a path a restore may write to",
            item.original.display()
        )));
    }
    let src = item.files_path();
    if !exists(&src) {
        return Err(DfError::Op(format!(
            "{} is no longer in the trash",
            item.original.display()
        )));
    }
    if exists(&item.original) {
        return Err(DfError::Op(format!(
            "{} exists again: refusing to overwrite it with the trashed copy",
            item.original.display()
        )));
    }
    let parent = item
        .original
        .parent()
        .ok_or_else(|| DfError::Op(format!("{} has no parent", item.original.display())))?;
    if !exists(parent) {
        return Err(DfError::Op(format!(
            "{} is gone, so {} cannot go back into it",
            parent.display(),
            item.original.display()
        )));
    }

    match std::fs::rename(&src, &item.original) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            super::copy::move_cross_device(&src, &item.original, ctx)?;
        }
        Err(e) => return Err(DfError::io(&src, e)),
    }

    if let Err(e) = std::fs::remove_file(item.info_path()) {
        // The file is back, which is what was asked for; a stale info file is
        // untidy, not a failure.
        log::warn!(
            "restored {} but could not remove {}: {e}",
            item.original.display(),
            item.info_path().display()
        );
    }
    Ok(item.original.clone())
}

/// Where the home trash lives, given the two environment variables. Pure, so
/// the `$XDG_DATA_HOME` rule is testable without mutating the environment.
pub fn home_trash_path(xdg_data_home: Option<&Path>, home: &Path) -> PathBuf {
    match xdg_data_home {
        // A relative `$XDG_DATA_HOME` is invalid per the basedir spec and is
        // ignored, exactly as the config loader does.
        Some(dir) if dir.is_absolute() => dir.join("Trash"),
        _ => home.join(".local/share/Trash"),
    }
}

/// Pick the trash `path` should go into (spec §Trash directories).
pub fn for_path(path: &Path) -> Result<Trash> {
    let home = Trash::home()?;
    let target = normalize(path);

    let path_dev = device_of(&target);
    // The home trash may not exist yet, so ask about the nearest ancestor that
    // does — same filesystem, same answer.
    let home_dev = device_of(home.root());
    match (path_dev, home_dev) {
        (Some(a), Some(b)) if a == b => return Ok(home),
        (None, _) | (_, None) => return Ok(home),
        _ => {}
    }

    let Some(topdir) = mount_point_of(&target) else {
        return Ok(home);
    };
    match topdir_trash(&topdir, uid()) {
        Ok(root) => Ok(Trash::at(root)),
        Err(e) => {
            log::warn!(
                "no usable trash on {} ({e}); using the home trash, which means a copy",
                topdir.display()
            );
            Ok(home)
        }
    }
}

/// The spec's two candidates under a mount point, in order.
pub fn topdir_trash(topdir: &Path, uid: u32) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let shared = topdir.join(".Trash");
    if let Ok(meta) = std::fs::symlink_metadata(&shared) {
        let sticky = meta.permissions().mode() & 0o1000 != 0;
        if meta.is_dir() && !meta.is_symlink() && sticky {
            let mine = shared.join(uid.to_string());
            if !exists(&mine) {
                std::fs::create_dir(&mine).map_err(|e| DfError::io(&mine, e))?;
            }
            return Ok(mine);
        }
        log::debug!(
            "{} exists but is not a sticky real directory; ignoring it",
            shared.display()
        );
    }

    let own = topdir.join(format!(".Trash-{uid}"));
    if !exists(&own) {
        std::fs::create_dir(&own).map_err(|e| DfError::io(&own, e))?;
        if let Err(e) = std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o700)) {
            log::warn!("could not tighten {}: {e}", own.display());
        }
    }
    Ok(own)
}

/// The device number of a path, or of the nearest ancestor that exists.
pub(crate) fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let mut cursor = Some(path);
    while let Some(p) = cursor {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            return Some(meta.dev());
        }
        cursor = p.parent();
    }
    None
}

/// Walk up until the device number changes: the last path with the original
/// device is the mount point.
pub fn mount_point_of(path: &Path) -> Option<PathBuf> {
    let dev = device_of(path)?;
    let mut best = None;
    let mut cursor = Some(normalize(path));
    while let Some(p) = cursor {
        match device_of(&p) {
            Some(d) if d == dev => {
                best = Some(p.clone());
                cursor = p.parent().map(|q| q.to_path_buf());
            }
            _ => break,
        }
    }
    best
}

/// The process's user id. Public because it is also what names the
/// directories that belong to this user under `/run/user` — the app's mount
/// manager finds gvfs's shares there — and one `getuid` wrapper is better than
/// a second `unsafe` block beside it.
pub fn uid() -> u32 {
    // `getuid` cannot fail and touches nothing; std simply does not expose it.
    #[allow(unsafe_code)]
    unsafe {
        libc::getuid()
    }
}

/// `name_1`, `name_2`… — the yazi collision suffix, applied before the
/// extension and clipped to fit a 255-byte name.
pub fn suffixed(name: &OsStr, n: u32) -> OsString {
    fit(name, Some(n), MAX_NAME_BYTES)
}

/// Build a name that fits in `max` bytes: `stem` + optional `_n` + `.ext`,
/// with the *stem* giving up bytes if something has to.
///
/// The extension is kept whole because it is what decides the icon, the opener
/// and the preview; a truncated stem is merely ugly, a truncated extension
/// changes what the file is. If the extension alone does not fit, the whole
/// name is clipped and the extension goes with it — nothing else is possible.
fn fit(name: &OsStr, suffix: Option<u32>, max: usize) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let as_path = Path::new(name);
    let stem = as_path.file_stem().unwrap_or(name).as_bytes().to_vec();
    let ext = as_path.extension().map(|e| e.as_bytes().to_vec());

    let suffix = suffix
        .map(|n| format!("_{n}").into_bytes())
        .unwrap_or_default();
    let ext_len = ext.as_ref().map(|e| e.len() + 1).unwrap_or(0);
    if suffix.len() + ext_len > max {
        // Pathological: an extension longer than a whole name may be.
        return OsString::from_vec(clip(name.as_bytes(), max));
    }
    let room = max - suffix.len() - ext_len;

    let mut out = clip(&stem, room);
    out.extend_from_slice(&suffix);
    if let Some(ext) = ext {
        out.push(b'.');
        out.extend_from_slice(&ext);
    }
    OsString::from_vec(out)
}

/// Cut a byte string to at most `room` bytes without splitting a UTF-8
/// character — a half-written `é` in a filename is legal and horrible.
fn clip(bytes: &[u8], room: usize) -> Vec<u8> {
    if bytes.len() <= room {
        return bytes.to_vec();
    }
    let mut end = room;
    while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    bytes[..end].to_vec()
}

/// The contents of a `.trashinfo` file.
pub fn trashinfo_text(original: &Path, deleted_at: &str) -> String {
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode_path(original),
        deleted_at
    )
}

/// Read back `Path` and `DeletionDate` from a `.trashinfo` file.
pub fn parse_trashinfo(text: &str) -> Result<(PathBuf, String)> {
    let mut path = None;
    let mut date = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Path=") {
            path = Some(decode_path(v));
        } else if let Some(v) = line.strip_prefix("DeletionDate=") {
            date = v.to_string();
        }
    }
    let path = path.ok_or_else(|| DfError::Op("trashinfo has no Path=".to_string()))?;
    Ok((path, date))
}

/// Percent-encode a path for `Path=`, per the spec's reference to RFC 2396.
///
/// `/` stays a separator — the value is a path, not an opaque string, and every
/// other implementation writes it that way. Everything outside the unreserved
/// set is escaped, which is what makes a name containing a newline, a `%` or a
/// `#` round-trip through an ini file at all.
pub fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/');
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The inverse of [`encode_path`]. Invalid escapes are kept verbatim: a
/// hand-edited info file should still restore something sensible.
pub fn decode_path(text: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |c: u8| -> Option<u8> {
                match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(OsString::from_vec(out))
}

/// `YYYY-MM-DDThh:mm:ss` in UTC.
///
/// The spec allows local time and most implementations write it; delightfile
/// writes UTC and says so, because the alternative is a timezone database — the
/// system's `localtime` is not reachable without `libc::localtime_r` and a
/// mutable static — for a field nothing in the program reads back except to
/// show it. A restore does not care what the string says.
pub fn iso8601_utc(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since the epoch to a calendar date,
/// with no lookup tables and no leap-year special cases.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── Keeping the trash from growing for ever (`[mgr] trash_keep_days`) ───────

/// One day, in the unit a `SystemTime` is compared in.
const DAY_SECS: u64 = 86_400;

/// How far a `DeletionDate` may be from UTC, which is how it is read.
///
/// The spec writes the date in *local* time and delightfile writes UTC (see
/// [`iso8601_utc`]); the record does not say which, so a date written by a
/// desktop west of Greenwich reads up to twelve hours older than it is. The
/// widest offset any zone has is fourteen hours, and an item has to be that
/// much past its keep before a purge will touch it — so "removed for good
/// after 30 days" can mean thirty days and a few hours, and never twenty-nine
/// and a half.
pub const DATE_SLACK_SECS: u64 = 14 * 3600;

/// `YYYY-MM-DDThh:mm:ss` — a `.trashinfo`'s `DeletionDate` — as a
/// [`SystemTime`], read as UTC.
///
/// Hand-rolled for the same reason the writer is: parsing a fixed-width ini
/// field is arithmetic, and the alternative is a date crate for one line.
/// Anything that does not parse is `None`, and every reader takes that as "no
/// date" rather than as an error: the trash view still lists the item, and the
/// automatic purge never touches it ([`expired`]).
pub fn parse_deletion_date(text: &str) -> Option<SystemTime> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    if secs >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(std::time::Duration::from_secs(secs.unsigned_abs()))
    }
}

/// Howard Hinnant's `days_from_civil` — the inverse of [`civil_from_days`],
/// for the same reason: no lookup tables, no leap-year special cases.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The items a trash kept for `keep_days` days may lose at `now`: every one
/// with a record whose `DeletionDate` reads, and reads at least that many days
/// (and [`DATE_SLACK_SECS`]) ago.
///
/// Pure, because it is the one decision in the program that destroys files
/// nobody pointed at, and the rules have to be a table rather than a hope.
/// **Never** chosen:
///
/// - anything at all when `keep_days` is `0`, which is "never purge";
/// - an orphan ([`TrashedItem::is_orphan`]) — bytes with no record, whose age
///   nobody wrote down;
/// - a record whose date is missing or does not parse — its age is unknown,
///   and unknown is not old;
/// - a date in the future, which is a clock that was wrong rather than an item
///   that is old;
/// - and anything newer than the keep.
///
/// An info file that cannot be read or parsed at all never gets this far:
/// [`Trash::list`] skips it, and [`Trash::orphans`] will not claim its file.
pub fn expired(items: &[TrashedItem], now: SystemTime, keep_days: u64) -> Vec<TrashedItem> {
    if keep_days == 0 {
        return Vec::new();
    }
    let keep = keep_days
        .saturating_mul(DAY_SECS)
        .saturating_add(DATE_SLACK_SECS);
    let Some(cutoff) = now.checked_sub(std::time::Duration::from_secs(keep)) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| !item.is_orphan())
        .filter(|item| parse_deletion_date(&item.deleted_at).is_some_and(|at| at <= cutoff))
        .cloned()
        .collect()
}

/// What a purge of old items came to: how many went, and how many would not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Purged {
    pub removed: usize,
    pub failed: usize,
    /// The first refusal, word for word, for the task panel and the toast.
    pub first_error: Option<String>,
}

/// Destroy every item in `trash` that [`expired`] chooses.
///
/// An item that will not go is counted and stepped over — one unreadable
/// directory must not keep a year of other deletions on the disk — and the
/// first reason is kept for whoever reports it. A cancel stops between items
/// (or inside one, through [`purge`]'s own tree walk) and is the only error
/// that ends the run early; a trash that cannot be listed at all is the only
/// other one.
///
/// Age is `now` less the recorded date, and nothing else: a machine whose
/// clock has jumped years ahead will purge everything as old, and there is no
/// defence here against a wrong clock.
pub fn purge_expired(
    trash: &Trash,
    keep_days: u64,
    now: SystemTime,
    ctx: &TaskCtx,
) -> Result<Purged> {
    let old = expired(&trash.list()?, now, keep_days);
    let mut report = Purged::default();
    for item in &old {
        ctx.checkpoint()?;
        // Asked again at the last moment, because the listing is a moment
        // old: a record restored, and its name taken by a newer deletion in
        // between, would otherwise be the newer file destroyed under the old
        // one's date.
        if !still_recorded(item) {
            continue;
        }
        match purge(item, ctx) {
            Ok(()) => report.removed += 1,
            Err(DfError::Cancelled) => return Err(DfError::Cancelled),
            Err(e) => {
                report.failed += 1;
                report.first_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    Ok(report)
}

// ── Once a day per user, not per window ─────────────────────────────────────

/// The file in a trash's root that says when delightfile last purged it: its
/// mtime is the purge, and its being non-empty is what says one has happened
/// (a purge that has only ever *locked* it leaves it empty).
///
/// **Per user, not per window.** Every window is its own process (see
/// df-app's `window`), and each keeps its own daily check — so without a
/// shared record every launch purged at once, a window opened beside one
/// that had the trash on screen purged its rows out from under it, and two
/// windows launched together purged side by side, the second counting what
/// the first had just taken as failures. The stamp is the one record they
/// share, and an `flock` on it is the one purge at a time.
///
/// A dotfile beside `files/` and `info/`: the spec gives the root no other
/// entries a program should trip over, and every other implementation lists
/// only those two directories.
pub const PURGE_STAMP: &str = ".delightfile-purge";

/// How long until a trash stamped at `stamped` is owed a purge, `every` apart
/// — `None` when it is owed one now.
///
/// Pure, over the stamp's reading. Never stamped is owed now; a stamp in the
/// future (a clock that went backwards) is not owed anything until the clock
/// has passed it, which errs, as everything here does, towards keeping.
pub fn purge_wait(
    stamped: Option<SystemTime>,
    every: Duration,
    now: SystemTime,
) -> Option<Duration> {
    let stamped = stamped?;
    match now.duration_since(stamped) {
        Ok(since) if since >= every => None,
        Ok(since) => Some(every - since),
        Err(_) => Some(every),
    }
}

/// When `trash` was last purged, by its stamp — `None` for never.
fn read_stamp(meta: &std::fs::Metadata) -> Option<SystemTime> {
    (meta.len() > 0).then(|| meta.modified().ok()).flatten()
}

/// How long until `trash` is owed a purge, asked without taking the lock —
/// the cheap question a window asks before it queues one. `None` is now.
///
/// A trash that does not exist yet has nothing to purge and is asked again in
/// `every`.
pub fn purge_due_in(trash: &Trash, every: Duration, now: SystemTime) -> Option<Duration> {
    if !exists(trash.root()) {
        return Some(every);
    }
    let stamp = trash.root().join(PURGE_STAMP);
    let stamped = std::fs::metadata(stamp)
        .ok()
        .and_then(|meta| read_stamp(&meta));
    purge_wait(stamped, every, now)
}

/// [`purge_expired`], at most once per `every` for everybody who purges this
/// trash: `None` when it stood down — another process holds the purge, or
/// the stamp says the last one was less than `every` ago.
///
/// The stamp is locked (`flock`, exclusive, never waiting) for the whole run
/// and read *under* the lock, so a process that loses the race to one that
/// has just finished sees the fresh stamp and stands down too. It is written
/// only when the run completes — a cancelled purge leaves it as it was, and
/// the next check runs it again.
pub fn purge_expired_if_due(
    trash: &Trash,
    keep_days: u64,
    every: Duration,
    now: SystemTime,
    ctx: &TaskCtx,
) -> Result<Option<Purged>> {
    use std::io::Write;
    if !exists(trash.root()) {
        return Ok(None);
    }
    let path = trash.root().join(PURGE_STAMP);
    let mut stamp = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| DfError::io(&path, e))?;
    match stamp.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
        Err(std::fs::TryLockError::Error(e)) => return Err(DfError::io(&path, e)),
    }
    let meta = stamp.metadata().map_err(|e| DfError::io(&path, e))?;
    if purge_wait(read_stamp(&meta), every, now).is_some() {
        return Ok(None);
    }
    let report = purge_expired(trash, keep_days, now, ctx)?;
    // Written, not only touched: the content is what tells a stamp from a
    // lock file somebody else created and never finished with.
    stamp
        .set_len(0)
        .and_then(|()| stamp.write_all(b"delightfile purged this trash; the mtime says when\n"))
        .and_then(|()| stamp.set_modified(now))
        .map_err(|e| DfError::io(&path, e))?;
    Ok(Some(report))
}

/// Whether `item`'s record still says what it said when it was listed.
fn still_recorded(item: &TrashedItem) -> bool {
    std::fs::read_to_string(item.info_path())
        .ok()
        .and_then(|text| parse_trashinfo(&text).ok())
        .is_some_and(|(original, deleted_at)| {
            original == item.original && deleted_at == item.deleted_at
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::{gnarly_names, TempTree};
    use std::time::Duration;

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    fn trash_in(t: &TempTree) -> Trash {
        Trash::at(t.join("Trash"))
    }

    #[test]
    fn trash_then_restore_round_trips() {
        let t = TempTree::new("trash-roundtrip");
        let trash = trash_in(&t);
        let file = t.file("work/notes.txt", b"the contents");

        let item = trash.trash(&file, &ctx()).unwrap();
        assert!(!exists(&file), "moved out of the way");
        assert_eq!(item.original, normalize(&file));
        assert_eq!(std::fs::read(item.files_path()).unwrap(), b"the contents");
        assert!(item.info_path().is_file());

        let back = restore(&item, &ctx()).unwrap();
        assert_eq!(back, normalize(&file));
        assert_eq!(std::fs::read(&file).unwrap(), b"the contents");
        assert!(!exists(&item.info_path()), "the record goes with it");
        assert!(!exists(&item.files_path()));
    }

    /// A purge destroys the file *and* its record — the second half is the
    /// point, because a record with no file is a row in the trash view that can
    /// never be restored and never goes away.
    #[test]
    fn purging_takes_the_record_with_the_file() {
        let t = TempTree::new("trash-purge");
        let trash = trash_in(&t);
        let file = t.file("work/notes.txt", b"gone");
        let dir = t.dir("work/project");
        std::fs::write(dir.join("a"), b"a").unwrap();

        let item = trash.trash(&file, &ctx()).unwrap();
        let folder = trash.trash(&dir, &ctx()).unwrap();
        assert_eq!(trash.list().unwrap().len(), 2);

        trash.purge(&item, &ctx()).unwrap();
        assert!(!exists(&item.files_path()));
        assert!(!exists(&item.info_path()));
        // A whole tree goes too — a trashed directory is a directory.
        purge(&folder, &ctx()).unwrap();
        assert!(!exists(&folder.files_path()));
        assert!(trash.list().unwrap().is_empty());
    }

    /// **The bug this fixes**: a cancelled purge used to leave half a tree in
    /// `files/` *behind an intact record*, and the record is what a restore
    /// believes. `D` on a project directory, `x` in the task panel, `Enter` on
    /// the row that is still there — and a directory missing most of its files
    /// moved back over the name it came from, reported as "Restored 1 item".
    ///
    /// The record now goes first, so what a cancel leaves is an orphan: listed,
    /// refused by name, and destroyed by the next `D`.
    #[test]
    fn a_cancelled_purge_leaves_a_broken_row_rather_than_a_lying_record() {
        use crate::tasks::{ProgressSink, TaskFlags};
        use std::sync::Arc;

        /// Cancels the task the moment the walk removes anything — a hand on
        /// `x` in the task panel, one file in.
        struct CancelAfterFirst(Arc<TaskFlags>);
        impl ProgressSink for CancelAfterFirst {
            fn set_total(&self, _bytes: u64, _files: u64) {}
            fn advance(&self, _bytes: u64, _files: u64) {
                self.0.cancel();
            }
        }

        let t = TempTree::new("trash-purge-cancel");
        let trash = trash_in(&t);
        let dir = t.dir("work/project");
        for name in ["a", "b", "c", "d"] {
            std::fs::write(dir.join(name), b"payload").unwrap();
        }
        let item = trash.trash(&dir, &ctx()).unwrap();

        let flags = Arc::new(TaskFlags::new());
        let cancelling = TaskCtx::with_sink(
            Arc::clone(&flags),
            Arc::new(CancelAfterFirst(Arc::clone(&flags))),
        );
        let err = purge(&item, &cancelling).unwrap_err();
        assert!(matches!(err, DfError::Cancelled), "{err}");

        // The half-deleted tree is still there — and the record that described
        // it is not, which is the whole point.
        assert!(exists(&item.files_path()), "the walk stopped part way");
        assert!(
            !exists(&item.info_path()),
            "the record went before the first byte did"
        );

        // The view shows it, as a row with no origin.
        let listed = trash.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_orphan());
        assert_eq!(listed[0].name, item.name);

        // A restore is refused *by name* rather than quietly handing back a
        // directory with most of its files missing.
        let refusal = restore(&listed[0], &ctx()).unwrap_err().to_string();
        assert!(refusal.contains("no record"), "{refusal}");

        // And the next `D` finishes what the first one started.
        purge(&listed[0], &ctx()).unwrap();
        assert!(!exists(&item.files_path()));
        assert!(trash.list().unwrap().is_empty());
    }

    /// Purging an item whose file has already gone still tidies the record —
    /// an interrupted purge, or somebody emptying the trash from another
    /// program, must not leave a row nothing can clear.
    #[test]
    fn purging_an_orphaned_record_still_removes_it() {
        let t = TempTree::new("trash-purge-orphan");
        let trash = trash_in(&t);
        let item = trash.trash(&t.file("a.txt", b"x"), &ctx()).unwrap();
        std::fs::remove_file(item.files_path()).unwrap();
        purge(&item, &ctx()).unwrap();
        assert!(!exists(&item.info_path()));
        assert!(trash.list().unwrap().is_empty());
        // …and doing it twice is not an error: "already gone" is the outcome
        // that was asked for.
        purge(&item, &ctx()).unwrap();
    }

    #[test]
    fn trashes_a_whole_directory() {
        let t = TempTree::new("trash-dir");
        let trash = trash_in(&t);
        let dir = t.dir("work/project");
        std::fs::write(dir.join("a"), b"a").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b"), b"b").unwrap();

        let item = trash.trash(&dir, &ctx()).unwrap();
        assert!(!exists(&dir));
        restore(&item, &ctx()).unwrap();
        assert_eq!(std::fs::read(dir.join("sub/b")).unwrap(), b"b");
    }

    #[test]
    fn trashes_a_broken_symlink_without_following_it() {
        let t = TempTree::new("trash-link");
        let trash = trash_in(&t);
        let link = t.symlink("/nowhere/at/all", "work/link");
        let item = trash.trash(&link, &ctx()).unwrap();
        assert!(std::fs::symlink_metadata(item.files_path())
            .unwrap()
            .is_symlink());
        restore(&item, &ctx()).unwrap();
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("/nowhere/at/all")
        );
    }

    /// A name is bytes, not text. A file off a FAT stick or out of an archive
    /// can carry one that is not UTF-8, and dropping it from the listing meant
    /// the user could not see it, could not restore it, and — worst — was told
    /// "emptied the trash" while it stayed on disk forever.
    #[test]
    fn a_trashed_name_that_is_not_utf8_is_still_listed_and_restorable() {
        use std::os::unix::ffi::OsStrExt;
        let t = TempTree::new("trash-not-utf8");
        let trash = trash_in(&t);
        let name = OsStr::from_bytes(b"caf\xE9.txt");
        let file = t.file(Path::new("work").join(name), b"bytes");
        let item = trash.trash(&file, &ctx()).unwrap();

        let listed = trash.list().unwrap();
        assert_eq!(listed.len(), 1, "a non-UTF-8 name must still list");
        assert_eq!(listed[0].name, item.name);
        assert_eq!(listed[0].original, normalize(&file));

        restore(&listed[0], &ctx()).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"bytes");
        assert!(trash.list().unwrap().is_empty());
    }

    /// A `.trashinfo` is a text file, and not every one of them was written by
    /// this program: a removable stick's `.Trash-$uid`, an extracted archive, a
    /// trash directory restored from a backup. A `Path=` that is relative or
    /// climbs is a rename into somewhere the user never trashed anything from,
    /// so the restore refuses rather than performing it.
    #[test]
    fn a_hand_written_trashinfo_cannot_aim_a_restore_anywhere_it_likes() {
        let t = TempTree::new("trash-forged-path");
        let trash = trash_in(&t);
        let file = t.file("work/notes.txt", b"mine");
        let item = trash.trash(&file, &ctx()).unwrap();

        for forged in ["notes.txt", "../../../../tmp/notes.txt", "%2E%2E/notes.txt"] {
            std::fs::write(
                item.info_path(),
                format!("[Trash Info]\nPath={forged}\nDeletionDate=2026-01-01T00:00:00\n"),
            )
            .unwrap();
            let listed = trash.list().unwrap();
            let forged_item = listed
                .iter()
                .find(|i| i.name == item.name)
                .expect("the record is still listed");
            let err = restore(forged_item, &ctx()).unwrap_err();
            assert!(
                err.to_string()
                    .contains("not a path a restore may write to"),
                "{forged}: {err}"
            );
            // And nothing moved: the trashed copy is still in the trash.
            assert!(exists(&item.files_path()), "{forged}");
        }
    }

    #[test]
    fn every_gnarly_name_round_trips() {
        let t = TempTree::new("trash-gnarly");
        let trash = trash_in(&t);
        for name in gnarly_names() {
            let file = t.file(Path::new("work").join(&name), name.as_bytes());
            let item = trash.trash(&file, &ctx()).unwrap();
            assert!(
                item.info_path().file_name().map(|n| n.len()).unwrap_or(0) <= MAX_NAME_BYTES,
                "{name:?}: the info file name must fit too"
            );
            let text = std::fs::read_to_string(item.info_path()).unwrap();
            let (original, _date) = parse_trashinfo(&text).unwrap();
            assert_eq!(original, normalize(&file), "{name:?}");
            restore(&item, &ctx()).unwrap();
            assert_eq!(std::fs::read(&file).unwrap(), name.as_bytes(), "{name:?}");
        }
    }

    #[test]
    fn collisions_get_suffixed_names() {
        let t = TempTree::new("trash-collide");
        let trash = trash_in(&t);
        let mut items = Vec::new();
        for i in 0..3 {
            let file = t.file("work/notes.txt", format!("round {i}").as_bytes());
            items.push(trash.trash(&file, &ctx()).unwrap());
        }
        let names: Vec<_> = items.iter().map(|i| i.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                OsString::from("notes.txt"),
                OsString::from("notes_1.txt"),
                OsString::from("notes_2.txt")
            ]
        );
        // Each keeps its own contents and its own record.
        for (i, item) in items.iter().enumerate() {
            assert_eq!(
                std::fs::read(item.files_path()).unwrap(),
                format!("round {i}").into_bytes()
            );
            assert_eq!(item.original, normalize(&t.join("work/notes.txt")));
        }
    }

    #[test]
    fn an_orphan_file_in_the_trash_does_not_get_clobbered() {
        let t = TempTree::new("trash-orphan");
        let trash = trash_in(&t);
        trash.ensure().unwrap();
        // A file with no info record, as a crashed trashing would leave.
        std::fs::write(trash.files_dir().join("notes.txt"), b"orphan").unwrap();

        let file = t.file("work/notes.txt", b"mine");
        let item = trash.trash(&file, &ctx()).unwrap();
        assert_eq!(item.name, OsString::from("notes_1.txt"));
        assert_eq!(
            std::fs::read(trash.files_dir().join("notes.txt")).unwrap(),
            b"orphan"
        );
    }

    #[test]
    fn restore_refuses_when_the_name_is_taken_again() {
        let t = TempTree::new("trash-retaken");
        let trash = trash_in(&t);
        let file = t.file("work/notes.txt", b"old");
        let item = trash.trash(&file, &ctx()).unwrap();
        std::fs::write(&file, b"new work").unwrap();

        let err = restore(&item, &ctx()).unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"), "{err}");
        assert_eq!(std::fs::read(&file).unwrap(), b"new work");
        assert!(exists(&item.files_path()), "still safely in the trash");
    }

    #[test]
    fn restore_refuses_when_the_parent_is_gone() {
        let t = TempTree::new("trash-noparent");
        let trash = trash_in(&t);
        let file = t.file("work/notes.txt", b"x");
        let item = trash.trash(&file, &ctx()).unwrap();
        std::fs::remove_dir_all(t.join("work")).unwrap();
        let err = restore(&item, &ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot go back"), "{err}");
    }

    #[test]
    fn trashing_a_missing_file_is_an_error() {
        let t = TempTree::new("trash-missing");
        let trash = trash_in(&t);
        assert!(trash.trash(&t.join("nope"), &ctx()).is_err());
    }

    #[test]
    fn trash_applies_the_delete_rails() {
        let trash = Trash::at("/tmp/delightfile-never-used");
        let err = trash.trash(Path::new("/"), &ctx()).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    #[test]
    fn list_reads_back_what_was_trashed() {
        let t = TempTree::new("trash-list");
        let trash = trash_in(&t);
        let a = t.file("work/a.txt", b"a");
        let b = t.file("work/b — ünï.txt", b"b");
        trash.trash(&a, &ctx()).unwrap();
        trash.trash(&b, &ctx()).unwrap();

        let listed = trash.list().unwrap();
        assert_eq!(listed.len(), 2);
        let originals: Vec<_> = listed.iter().map(|i| i.original.clone()).collect();
        assert!(originals.contains(&normalize(&a)));
        assert!(originals.contains(&normalize(&b)));
    }

    #[test]
    fn encoding_escapes_everything_but_the_unreserved_set() {
        assert_eq!(encode_path(Path::new("/a/b.txt")), "/a/b.txt");
        assert_eq!(encode_path(Path::new("/a/with space")), "/a/with%20space");
        assert_eq!(encode_path(Path::new("/a/new\nline")), "/a/new%0Aline");
        assert_eq!(encode_path(Path::new("/a/100%")), "/a/100%25");
        assert_eq!(encode_path(Path::new("/a/#hash?q")), "/a/%23hash%3Fq");
        assert_eq!(encode_path(Path::new("/a/-_.~")), "/a/-_.~");
        // Non-ASCII is escaped byte by byte, UTF-8 first.
        assert_eq!(encode_path(Path::new("/é")), "/%C3%A9");
    }

    #[test]
    fn encoding_round_trips_every_gnarly_name() {
        for name in gnarly_names() {
            let p = PathBuf::from("/tmp").join(&name);
            assert_eq!(decode_path(&encode_path(&p)), p, "{name:?}");
        }
    }

    #[test]
    fn decoding_keeps_a_broken_escape_verbatim() {
        assert_eq!(decode_path("/a/%zz"), PathBuf::from("/a/%zz"));
        assert_eq!(decode_path("/a/%2"), PathBuf::from("/a/%2"));
    }

    #[test]
    fn trashinfo_is_the_spec_format() {
        let text = trashinfo_text(Path::new("/home/x/a b.txt"), "2026-08-31T12:00:00");
        assert_eq!(
            text,
            "[Trash Info]\nPath=/home/x/a%20b.txt\nDeletionDate=2026-08-31T12:00:00\n"
        );
        let (p, d) = parse_trashinfo(&text).unwrap();
        assert_eq!(p, Path::new("/home/x/a b.txt"));
        assert_eq!(d, "2026-08-31T12:00:00");
    }

    #[test]
    fn parse_rejects_an_info_file_with_no_path() {
        assert!(parse_trashinfo("[Trash Info]\nDeletionDate=x\n").is_err());
    }

    #[test]
    fn iso8601_is_utc_and_correct() {
        assert_eq!(iso8601_utc(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00");
        // 2026-08-31T13:45:07Z = 1_788_183_907.
        assert_eq!(
            iso8601_utc(SystemTime::UNIX_EPOCH + Duration::from_secs(1_788_183_907)),
            "2026-08-31T13:45:07"
        );
        // A leap day, to prove the civil-from-days arithmetic.
        assert_eq!(
            iso8601_utc(SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
            "2024-02-29T00:00:00"
        );
    }

    #[test]
    fn home_trash_honours_xdg_data_home() {
        assert_eq!(
            home_trash_path(Some(Path::new("/data/xdg")), Path::new("/home/x")),
            Path::new("/data/xdg/Trash")
        );
        assert_eq!(
            home_trash_path(None, Path::new("/home/x")),
            Path::new("/home/x/.local/share/Trash")
        );
        assert_eq!(
            home_trash_path(Some(Path::new("relative")), Path::new("/home/x")),
            Path::new("/home/x/.local/share/Trash"),
            "a relative XDG_DATA_HOME is invalid and ignored"
        );
    }

    #[test]
    fn topdir_trash_prefers_a_sticky_shared_trash() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new("topdir-sticky");
        let top = t.dir("mount");
        let shared = t.dir("mount/.Trash");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();

        let chosen = topdir_trash(&top, 1000).unwrap();
        assert_eq!(chosen, shared.join("1000"));
        assert!(chosen.is_dir());
    }

    #[test]
    fn topdir_trash_ignores_a_non_sticky_shared_trash() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new("topdir-nonsticky");
        let top = t.dir("mount");
        let shared = t.dir("mount/.Trash");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();

        let chosen = topdir_trash(&top, 1000).unwrap();
        assert_eq!(chosen, top.join(".Trash-1000"));
    }

    #[test]
    fn topdir_trash_ignores_a_symlinked_shared_trash() {
        let t = TempTree::new("topdir-symlink");
        let top = t.dir("mount");
        let elsewhere = t.dir("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, top.join(".Trash")).unwrap();

        let chosen = topdir_trash(&top, 4242).unwrap();
        assert_eq!(chosen, top.join(".Trash-4242"));
        assert!(chosen.is_dir());
        assert!(
            std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "the symlink target is left alone"
        );
    }

    #[test]
    fn topdir_trash_reuses_an_existing_own_trash() {
        let t = TempTree::new("topdir-reuse");
        let top = t.dir("mount");
        let first = topdir_trash(&top, 7).unwrap();
        std::fs::write(first.join("marker"), b"x").unwrap();
        let second = topdir_trash(&top, 7).unwrap();
        assert_eq!(first, second);
        assert!(second.join("marker").is_file());
    }

    #[test]
    fn for_path_uses_the_home_trash_for_a_path_on_the_home_filesystem() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return; // no home, nothing to assert
        };
        let chosen = for_path(&home).unwrap();
        assert_eq!(chosen, Trash::home().unwrap());
        assert!(
            chosen.root().ends_with("Trash"),
            "{}",
            chosen.root().display()
        );
    }

    #[test]
    fn mount_point_of_reaches_a_real_mount() {
        let mp = mount_point_of(Path::new("/tmp")).unwrap();
        assert!(mp.is_absolute());
        assert!(Path::new("/tmp").starts_with(&mp) || mp == Path::new("/tmp"));
    }

    #[test]
    fn suffixing_keeps_the_extension_and_the_length_limit() {
        assert_eq!(
            suffixed(OsStr::new("notes.txt"), 1),
            OsString::from("notes_1.txt")
        );
        assert_eq!(suffixed(OsStr::new("notes"), 2), OsString::from("notes_2"));
        assert_eq!(
            suffixed(OsStr::new("a.tar.gz"), 3),
            OsString::from("a.tar_3.gz")
        );
        assert_eq!(
            suffixed(OsStr::new(".bashrc"), 1),
            OsString::from(".bashrc_1"),
            "a dotfile has no extension to preserve"
        );

        let long = "x".repeat(MAX_NAME_BYTES);
        let out = suffixed(OsStr::new(&long), 12);
        assert!(out.len() <= MAX_NAME_BYTES, "{}", out.len());
        assert!(out.to_string_lossy().ends_with("_12"));
    }

    #[test]
    fn suffixing_never_splits_a_character() {
        let long = "é".repeat(200); // 400 bytes
        let out = suffixed(OsStr::new(&long), 1);
        assert!(out.len() <= MAX_NAME_BYTES);
        assert!(
            out.to_str().is_some(),
            "the clipped name is still valid UTF-8"
        );
    }

    // ── `[mgr] trash_keep_days` ─────────────────────────────────────────────

    /// A record with the given date, in a trash that is never touched.
    fn dated(name: &str, deleted_at: &str) -> TrashedItem {
        TrashedItem {
            trash_root: PathBuf::from("/nonexistent/Trash"),
            name: OsString::from(name),
            original: PathBuf::from(format!("/home/someone/{name}")),
            deleted_at: deleted_at.to_string(),
        }
    }

    /// `days` whole days before `now`, as a `.trashinfo` writes it.
    fn days_before(now: SystemTime, days: u64) -> String {
        iso8601_utc(now - Duration::from_secs(days * DAY_SECS))
    }

    fn names(items: &[TrashedItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| i.name.to_string_lossy().into_owned())
            .collect()
    }

    /// The whole table: old enough goes, and nothing else ever does — not a
    /// newer item, not one a few hours short of its keep, not an orphan, not a
    /// date that does not read, not one from the future, and nothing at all
    /// when the keep is zero.
    #[test]
    fn only_what_is_older_than_the_keep_expires() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let orphan = TrashedItem {
            original: PathBuf::new(),
            deleted_at: String::new(),
            ..dated("orphan", "")
        };
        let items = vec![
            dated("ancient", &days_before(now, 400)),
            dated("old", &days_before(now, 31)),
            // Thirty days to the second is inside the zone slack: a desktop
            // west of Greenwich wrote that date in local time, and it may
            // really be twenty-nine and a half days old.
            dated("exactly", &days_before(now, 30)),
            dated("new", &days_before(now, 2)),
            dated("today", &iso8601_utc(now)),
            dated(
                "tomorrow",
                &iso8601_utc(now + Duration::from_secs(DAY_SECS)),
            ),
            dated("unreadable", "last tuesday"),
            dated("undated", ""),
            orphan,
        ];
        assert_eq!(names(&expired(&items, now, 30)), vec!["ancient", "old"]);
        // A day past the keep and the slack, "exactly" goes too.
        let later = now + Duration::from_secs(DAY_SECS);
        assert_eq!(
            names(&expired(&items, later, 30)),
            vec!["ancient", "old", "exactly"]
        );
        // A shorter keep reaches further, and still never the unknowable.
        assert_eq!(
            names(&expired(&items, now, 1)),
            vec!["ancient", "old", "exactly", "new"]
        );
        // Zero is "never purge", however old the trash is.
        assert!(expired(&items, now, 0).is_empty());
        // A keep longer than the clock has been running chooses nothing
        // rather than wrapping round.
        assert!(expired(&items, SystemTime::UNIX_EPOCH, 30).is_empty());
        assert!(expired(&items, now, u64::MAX).is_empty());
    }

    /// The date parser reads back exactly what the writer wrote, and turns
    /// everything else into "no date" rather than a panic.
    #[test]
    fn a_deletion_date_round_trips_and_nonsense_is_none() {
        for stamp in [0u64, 1, 951_827_696, 1_756_598_400, 4_102_444_800] {
            let t = SystemTime::UNIX_EPOCH + Duration::from_secs(stamp);
            assert_eq!(parse_deletion_date(&iso8601_utc(t)), Some(t));
        }
        for text in [
            "",
            "yesterday",
            "2026-13-01T00:00:00",
            "2026-08-31T25:00:00",
            "2026/08/31T00:00:00",
        ] {
            assert_eq!(parse_deletion_date(text), None, "{text}");
        }
    }

    /// Write an item straight into a trash, with the date its record says —
    /// the way another program's trash, or last month's, looks on disk.
    fn plant(trash: &Trash, name: &str, deleted_at: &str, dir: bool) {
        trash.ensure().unwrap();
        let file = trash.files_dir().join(name);
        if dir {
            std::fs::create_dir_all(file.join("inner")).unwrap();
            std::fs::write(file.join("inner/deep.txt"), b"deep").unwrap();
        } else {
            std::fs::write(&file, b"body").unwrap();
        }
        let info = trash.info_dir().join(format!("{name}.{TRASHINFO_EXT}"));
        let original = PathBuf::from(format!("/home/someone/{name}"));
        std::fs::write(info, trashinfo_text(&original, deleted_at)).unwrap();
    }

    /// Against a real trash: the old items go, file and record both, and
    /// every item the table protects is still there afterwards — including an
    /// orphan and a record that cannot be read at all.
    #[test]
    fn purging_the_old_leaves_everything_else_where_it_was() {
        let t = TempTree::new("trash-keep");
        let trash = trash_in(&t);
        let now = SystemTime::now();
        plant(&trash, "old.txt", &days_before(now, 45), false);
        plant(&trash, "old-folder", &days_before(now, 90), true);
        plant(&trash, "recent.txt", &days_before(now, 3), false);
        plant(&trash, "undated.txt", "", false);
        // A record that is not a record: no `Path=`, so it never lists.
        plant(&trash, "broken.txt", &days_before(now, 400), false);
        std::fs::write(
            trash.info_dir().join(format!("broken.txt.{TRASHINFO_EXT}")),
            "[Trash Info]\nDeletionDate=2001-01-01T00:00:00\n",
        )
        .unwrap();
        // Bytes with no record at all.
        std::fs::write(trash.files_dir().join("orphan.bin"), b"?").unwrap();

        let report = purge_expired(&trash, 30, now, &ctx()).unwrap();
        assert_eq!(
            report,
            Purged {
                removed: 2,
                failed: 0,
                first_error: None
            }
        );
        for gone in ["old.txt", "old-folder"] {
            assert!(!exists(&trash.files_dir().join(gone)), "{gone} survived");
            assert!(
                !exists(&trash.info_dir().join(format!("{gone}.{TRASHINFO_EXT}"))),
                "{gone}'s record survived"
            );
        }
        for kept in ["recent.txt", "undated.txt", "broken.txt", "orphan.bin"] {
            assert!(exists(&trash.files_dir().join(kept)), "{kept} was purged");
        }
        let left: Vec<String> = names(&trash.list().unwrap());
        assert_eq!(left, vec!["orphan.bin", "recent.txt", "undated.txt"]);

        // Run again and there is nothing old left to take.
        assert_eq!(
            purge_expired(&trash, 30, now, &ctx()).unwrap(),
            Purged::default()
        );
        // …and a keep of zero never takes anything.
        assert_eq!(
            purge_expired(
                &trash,
                0,
                now + Duration::from_secs(9_000 * DAY_SECS),
                &ctx()
            )
            .unwrap(),
            Purged::default()
        );
        assert_eq!(trash.list().unwrap().len(), 3);
    }

    /// A record that changed between the listing and the purge is not the
    /// item the listing chose, and is left alone.
    #[test]
    fn a_record_that_changed_since_the_listing_is_not_purged() {
        let t = TempTree::new("trash-keep-race");
        let trash = trash_in(&t);
        let now = SystemTime::now();
        plant(&trash, "notes.txt", &days_before(now, 45), false);
        let listed = expired(&trash.list().unwrap(), now, 30);
        assert_eq!(listed.len(), 1);
        // Restored and trashed again under the same name, a moment ago.
        plant(&trash, "notes.txt", &iso8601_utc(now), false);
        assert!(!still_recorded(&listed[0]));
        assert!(still_recorded(&trash.list().unwrap()[0]));
    }

    const DAY: Duration = Duration::from_secs(DAY_SECS);

    /// Never stamped is owed now; a stamp a day old is owed now; a younger one
    /// waits out the rest of its day; one from the future waits a whole day.
    #[test]
    fn a_stamp_says_how_long_until_the_next_purge() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let hour = Duration::from_secs(3600);
        assert_eq!(purge_wait(None, DAY, now), None);
        assert_eq!(purge_wait(Some(now - DAY), DAY, now), None);
        assert_eq!(purge_wait(Some(now - DAY * 3), DAY, now), None);
        assert_eq!(purge_wait(Some(now - hour), DAY, now), Some(DAY - hour));
        assert_eq!(purge_wait(Some(now), DAY, now), Some(DAY));
        assert_eq!(purge_wait(Some(now + hour), DAY, now), Some(DAY));
    }

    /// Once a day for everybody: the first run purges and stamps, a second
    /// the same day stands down whatever has aged since, and a day on it runs
    /// again — and the quick question a window asks agrees throughout.
    #[test]
    fn a_purge_runs_once_a_day_whoever_asks() {
        let t = TempTree::new("trash-keep-stamp");
        let trash = trash_in(&t);
        let now = SystemTime::now();
        // Nothing to purge in a trash that is not there, and nothing made.
        assert_eq!(
            purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap(),
            None
        );
        assert_eq!(purge_due_in(&trash, DAY, now), Some(DAY));
        assert!(!exists(trash.root()));

        plant(&trash, "old.txt", &days_before(now, 45), false);
        assert_eq!(purge_due_in(&trash, DAY, now), None, "never purged");
        let first = purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap();
        assert_eq!(first.map(|r| r.removed), Some(1));
        // Stamped with the purge's own `now` — to the filesystem's grain.
        let stamp = trash.root().join(PURGE_STAMP);
        let stamped = std::fs::metadata(&stamp).unwrap().modified().unwrap();
        let off = now.duration_since(stamped).unwrap_or_else(|e| e.duration());
        assert!(off < Duration::from_secs(1), "stamped {off:?} away");
        let wait = purge_due_in(&trash, DAY, now).expect("not owed again today");
        assert!(wait > DAY - Duration::from_secs(1));

        // Another process, later the same day: stands down, and the item that
        // has become old since is left for tomorrow's.
        plant(&trash, "older.txt", &days_before(now, 60), false);
        let later = now + Duration::from_secs(3600);
        assert_eq!(
            purge_expired_if_due(&trash, 30, DAY, later, &ctx()).unwrap(),
            None
        );
        assert!(exists(&trash.files_dir().join("older.txt")));

        let tomorrow = now + DAY + Duration::from_secs(1);
        assert_eq!(purge_due_in(&trash, DAY, tomorrow), None);
        let next = purge_expired_if_due(&trash, 30, DAY, tomorrow, &ctx()).unwrap();
        assert_eq!(next.map(|r| r.removed), Some(1));
        assert!(!exists(&trash.files_dir().join("older.txt")));
    }

    /// A purge another process is running is not run twice: the second
    /// stands down silently while the lock is held — and a stamp file that
    /// has only ever been locked, never written, is not a purge.
    #[test]
    fn a_second_purger_stands_down_while_the_first_holds_the_stamp() {
        let t = TempTree::new("trash-keep-lock");
        let trash = trash_in(&t);
        let now = SystemTime::now();
        plant(&trash, "old.txt", &days_before(now, 45), false);
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(trash.root().join(PURGE_STAMP))
            .unwrap();
        held.lock().unwrap();
        assert_eq!(
            purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap(),
            None
        );
        assert!(
            exists(&trash.files_dir().join("old.txt")),
            "purged under a lock"
        );
        // Empty, so still owed: the other process never finished.
        assert_eq!(purge_due_in(&trash, DAY, now), None);
        held.unlock().unwrap();
        let report = purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap();
        assert_eq!(report.map(|r| r.removed), Some(1));
    }

    /// A cancelled purge does not stamp: it has not happened, and the next
    /// check runs it.
    #[test]
    fn a_cancelled_purge_is_still_owed() {
        use crate::tasks::{NullSink, TaskFlags};
        use std::sync::Arc;
        let t = TempTree::new("trash-keep-cancel");
        let trash = trash_in(&t);
        let now = SystemTime::now();
        plant(&trash, "old.txt", &days_before(now, 45), false);
        let flags = Arc::new(TaskFlags::new());
        flags.cancel();
        let cancelled = TaskCtx::with_sink(flags, Arc::new(NullSink));
        assert!(matches!(
            purge_expired_if_due(&trash, 30, DAY, now, &cancelled),
            Err(DfError::Cancelled)
        ));
        assert!(exists(&trash.files_dir().join("old.txt")));
        assert_eq!(purge_due_in(&trash, DAY, now), None);
    }
}
