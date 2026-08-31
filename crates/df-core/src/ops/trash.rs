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
use std::time::SystemTime;

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
                super::copy::move_cross_device(path, &dst, ctx)
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
            "{}: {MAX_TRASH_COLLISIONS} names of this shape are already in the trash",
            base.to_string_lossy()
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
            let name = match info_path.file_name().and_then(|n| n.to_str()) {
                Some(n) if n.ends_with(&format!(".{TRASHINFO_EXT}")) => {
                    OsString::from(&n[..n.len() - TRASHINFO_EXT.len() - 1])
                }
                _ => continue,
            };
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
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Put an item back where it came from.
    pub fn restore(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
        restore(item, ctx)
    }
}

/// Put a trashed item back where it came from — the inverse `u` runs.
///
/// Refuses if something has taken the original name in the meantime: undo may
/// never overwrite newer work (PLAN §5), and a restore that silently clobbers
/// would be exactly that.
pub fn restore(item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
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
fn device_of(path: &Path) -> Option<u64> {
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

fn uid() -> u32 {
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
}
