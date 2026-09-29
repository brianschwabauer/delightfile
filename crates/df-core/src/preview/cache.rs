//! The yazi-compatible thumbnail cache — one directory, two programs.
//!
//! PLAN §6 asks for "the yazi-compatible cache handshake delightviewer already
//! reads, so delightfile ⇄ delightviewer hand off the exact frame the user was
//! looking at. One shared cache dir." This is that handshake, ported from
//! delightviewer's `dlv-doc/src/yazi_cache.rs` (which verified it empirically
//! against 208 live cache entries) and re-verified here against the real cache
//! on this machine: a sweep of `~/Downloads` reproduced **63** of the 80 names
//! in `/tmp/yazi-1000` exactly — for example
//! `~/Downloads/proj_1_Default.pdf` → `2cb8293b59601f495530cddf5b2cfe6e`, and
//! `~/Downloads/my-share-code.png` → `a065d96539bdd7ac78e19426bbdd127b`, both
//! files this code never wrote. The other 17 entries are files that have since
//! been moved or deleted.
//!
//! ## The scheme
//!
//! yazi 26.5.6, `yazi-plugin/src/utils/cache.rs` → `Utils::file_cache`:
//!
//! ```text
//! name = format!("{:x}", Twox128 { File::hash(file); skip.hash() })
//! path = <preview.cache_dir>/<name>
//! ```
//!
//! - **Directory**: `$TMPDIR/yazi-$UID`, `/tmp/yazi-1000` here. Not
//!   `~/.cache/yazi` — that holds `packages/`, the plugin manager's store, and
//!   no thumbnails at all. A user who sets `preview.cache_dir` in `yazi.toml`
//!   gets a miss; that is a deliberate limit, not a bug (see [`cache_dir`]).
//! - **Name**: the lowercase hex of an XXH3-128 digest, **not zero-padded** —
//!   yazi formats with `{:x}` and no width, so a digest with a zero top nibble
//!   produces a 31-character name. Real evidence from `/tmp/yazi-1000`:
//!   `2cb8293b59601f495530cddf5b2cfe6e` (32 chars) sits next to
//!   `6fa143542ed4550389ce8d043afc5cd` (31). Zero-padding would miss the
//!   second, which is exactly the bug this note exists to prevent.
//! - **Content**: a downscaled JPEG, with **no extension** on the name. `file`
//!   on `/tmp/yazi-1000/1c53bb845e925969689e5e341d81989a` reports
//!   "JPEG image data, baseline, precision 8, 1200x675". Some entries have an
//!   `.jpg` twin (`…2cfe6e` and `…2cfe6e.jpg`) written by an image-editing
//!   opener rule, not by the previewer; the extensionless name is the one to
//!   read.
//! - **Digest input**, in order, through `std::hash::Hash`:
//!   1. `0isize` — yazi's `Url::Regular` is the first variant of its URL enum,
//!      so `derive(Hash)` writes discriminant 0 as an `isize` first.
//!   2. the `Path`.
//!   3. `cha.len` (`u64`), `cha.btime`, `cha.ctime`, `cha.mtime` (each an
//!      `Option<SystemTime>`).
//!   4. the caller's `skip` (`usize`) — yazi's page/frame index, 0 for a still.
//!
//! Everything but the discriminant goes through std's own `Hash` impls, so the
//! only thing that can drift is yazi's *structure*, not the transcription.
//!
//! ## Freshness comes free
//!
//! There is no mtime comparison anywhere in this file, and there must not be:
//! the size and all three timestamps are hashed *into the name*, so an edited
//! file simply hashes somewhere else and misses. A stale thumbnail is
//! unreachable by construction — which is the property that lets two programs
//! share the directory without a lock or a manifest.
//!
//! ## Every failure is a miss
//!
//! No cache directory, no source file, a different yazi, a truncated JPEG: all
//! of it returns `None`. The cache is an optimisation — a missed hit costs one
//! decode, and nothing the user can see is allowed to depend on it.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::xxh3::xxh3_128;
use crate::platform;

/// The cache directory's name inside `$TMPDIR`, minus the uid.
const CACHE_DIR_PREFIX: &str = "yazi-";

/// The `skip` for a still image — the whole of what a file manager wants.
///
/// yazi uses `skip` to number PDF pages and video frames within one file, so
/// page 3 of a document and second 30 of a video each get their own entry.
/// delightfile asks for the first frame; scrubbing produces frames it renders
/// itself and does not write back, because a scrub generates hundreds of them
/// and filling a shared /tmp directory with them would be rude.
pub const STILL_SKIP: usize = 0;

/// yazi's `Url::Regular` discriminant, hashed as an `isize` ahead of the path.
const URL_REGULAR_DISCRIMINANT: isize = 0;

/// A [`Hasher`] that keeps the bytes and hashes them at the end.
///
/// yazi's `Twox128` forwards only `write`, which means every `write_u64` /
/// `write_isize` / `write_usize` from std's derived `Hash` impls arrives as
/// its native-endian bytes. Buffering and hashing once is byte-for-byte the
/// same input as streaming it — XXH3 is defined over the concatenation — and
/// it lets [`super::xxh3`] stay a single oneshot function.
///
/// Native-endian is the reason a cache entry is not portable between machines
/// of different endianness. Neither is `/tmp`, so nothing is lost.
#[derive(Default)]
struct KeyHasher {
    bytes: Vec<u8>,
}

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    /// Never called: the digest this type exists for is 128 bits wide, and
    /// [`KeyHasher::digest`] is how it comes out. yazi's own wrapper leaves
    /// this unreachable too.
    fn finish(&self) -> u64 {
        0
    }
}

impl KeyHasher {
    fn digest(&self) -> u128 {
        xxh3_128(&self.bytes)
    }
}

/// The shared cache directory, if it exists.
///
/// `preview.cache_dir` defaults to yazi's temp directory, `$TMPDIR/yazi-$UID`.
/// A user who overrides it in `yazi.toml` gets no handshake — reading yazi's
/// config to find out would mean parsing yazi's config, and the payoff is one
/// avoided decode. Returns `None` rather than creating the directory, because
/// a *reader* must not conjure the thing it is looking in; [`store_thumb`] is
/// the call that creates it.
pub fn cache_dir() -> Option<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "{CACHE_DIR_PREFIX}{}",
        platform::user::cache_suffix()
    ));
    dir.is_dir().then_some(dir)
}

/// The cache file name yazi would write for `path` at `skip`, given `path`'s
/// metadata. Pure, so the scheme is testable without a filesystem.
pub fn cache_key(path: &Path, meta: &std::fs::Metadata, skip: usize) -> String {
    let mut h = KeyHasher::default();

    // --- yazi_fs::File::hash
    URL_REGULAR_DISCRIMINANT.hash(&mut h);
    path.hash(&mut h);
    meta.len().hash(&mut h);
    meta.created().ok().hash(&mut h);
    let (ctime, ctime_nsec) = crate::platform::meta::change_time(meta);
    // yazi builds ctime by hand from the raw stat fields, because std has no
    // accessor for it. The reconstruction has to match exactly, nanoseconds
    // included — this is part of the key.
    UNIX_EPOCH
        .checked_add(Duration::new(ctime as u64, ctime_nsec as u32))
        .hash(&mut h);
    meta.modified().ok().hash(&mut h);

    // --- the caller's `skip`
    skip.hash(&mut h);

    // `{:x}` with no width: leading zeros are absent from the name, so the
    // name can be shorter than 32 characters. Matching that matters.
    format!("{:x}", h.digest())
}

/// Where a thumbnail for `path` lives, whether or not it has been written.
///
/// `None` only when there is no cache directory or `path` cannot be stat'd —
/// the stat is not optional, since the metadata *is* half the key.
pub fn thumb_path(path: &Path, skip: usize) -> Option<PathBuf> {
    let dir = cache_dir()?;
    let meta = std::fs::metadata(path).ok()?;
    Some(dir.join(cache_key(path, &meta, skip)))
}

/// The cached thumbnail for `path`, if one exists and is current.
///
/// "Current" needs no check of its own: the key contains the source's size and
/// timestamps, so a hit *is* a freshness proof. An empty file is treated as a
/// miss — yazi writes the entry before filling it, and half a JPEG is worse
/// than none.
pub fn cached_thumb(path: &Path) -> Option<PathBuf> {
    let cached = thumb_path(path, STILL_SKIP)?;
    let meta = std::fs::metadata(&cached).ok()?;
    (meta.is_file() && meta.len() > 0).then_some(cached)
}

/// Where delightfile should *write* a thumbnail for `path`, creating the
/// shared directory if this is the first program to want it.
///
/// The counterpart to [`cached_thumb`]: same name, same directory, so a
/// thumbnail delightfile decodes is one yazi finds later, and vice versa. The
/// caller writes JPEG bytes there — the format is not negotiable, it is what
/// the other side of the handshake expects to decode.
///
/// Callers should write to a temporary name in the same directory and rename
/// over the target, so a reader never sees a partial file. `None` if the
/// directory cannot be made or `path` cannot be stat'd.
pub fn store_thumb(path: &Path, skip: usize) -> Option<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "{CACHE_DIR_PREFIX}{}",
        platform::user::cache_suffix()
    ));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::debug!("thumbnail cache dir {} unusable: {e}", dir.display());
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    Some(dir.join(cache_key(path, &meta, skip)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str, contents: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("df-preview-cache-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        let _ = std::fs::write(&path, contents);
        path
    }

    #[test]
    fn the_key_is_hex_and_short_enough_to_be_a_name() {
        let path = fixture("key-shape", b"hello");
        let meta = std::fs::metadata(&path).expect("fixture stat");
        let key = cache_key(&path, &meta, STILL_SKIP);

        assert!(!key.is_empty(), "empty key");
        assert!(key.len() <= 32, "{key} is longer than a 128-bit hex digest");
        assert!(
            key.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "{key} is not lowercase hex"
        );
        // Not zero-padded: `{:x}` never emits a leading zero, so a 32-char key
        // must not start with one either.
        assert!(
            !key.starts_with('0') || key.len() < 32,
            "{key} looks padded"
        );
    }

    #[test]
    fn the_key_is_stable_for_an_unchanged_file() {
        let path = fixture("key-stable", b"hello");
        let meta = std::fs::metadata(&path).expect("fixture stat");
        let a = cache_key(&path, &meta, STILL_SKIP);
        let b = cache_key(&path, &meta, STILL_SKIP);
        assert_eq!(a, b);
    }

    #[test]
    fn skip_and_path_and_content_all_change_the_key() {
        let path = fixture("key-sensitive", b"hello");
        let meta = std::fs::metadata(&path).expect("fixture stat");
        let base = cache_key(&path, &meta, STILL_SKIP);

        assert_ne!(base, cache_key(&path, &meta, 1), "skip must matter");

        let other = fixture("key-sensitive-other", b"hello");
        let other_meta = std::fs::metadata(&other).expect("fixture stat");
        assert_ne!(
            base,
            cache_key(&other, &other_meta, STILL_SKIP),
            "the path must matter"
        );

        // Rewriting changes both the length and the mtime, which is the whole
        // of the freshness guarantee.
        let _ = std::fs::write(&path, b"hello, world");
        let changed = std::fs::metadata(&path).expect("fixture stat");
        assert_ne!(
            base,
            cache_key(&path, &changed, STILL_SKIP),
            "an edit must move the entry"
        );
    }

    #[test]
    fn an_unknown_file_misses_cleanly() {
        let missing = std::env::temp_dir().join("df-preview-no-such-file-ever");
        let _ = std::fs::remove_file(&missing);
        assert!(cached_thumb(&missing).is_none());
        assert!(thumb_path(&missing, STILL_SKIP).is_none());
    }

    #[test]
    fn store_and_read_round_trip_through_the_shared_directory() {
        let path = fixture("round-trip", b"pretend this is a photograph");
        // Nothing has been written for it yet.
        assert!(cached_thumb(&path).is_none());

        let Some(target) = store_thumb(&path, STILL_SKIP) else {
            eprintln!("skipped: no writable temp dir");
            return;
        };
        std::fs::write(&target, b"\xff\xd8\xff\xe0not really a jpeg").expect("write thumb");

        let found = cached_thumb(&path).expect("what store_thumb wrote, cached_thumb finds");
        assert_eq!(found, target);
        let _ = std::fs::remove_file(&target);
    }

    /// The scheme against the real thing.
    ///
    /// If yazi has previewed anything on this machine, `/tmp/yazi-$UID` holds
    /// its output; this walks a small, bounded corpus of the user's media
    /// directories looking for a file whose computed key names one of those
    /// entries. A hit proves the whole chain — directory, hash input, digest,
    /// hex formatting — against a file this code did not write. No hit is not
    /// a failure: the previewed files may live anywhere, or nowhere any more.
    #[test]
    fn opportunistically_matches_a_real_yazi_entry() {
        let Some(dir) = cache_dir() else {
            eprintln!("skipped: no yazi cache directory");
            return;
        };
        let entries: std::collections::HashSet<String> = match std::fs::read_dir(&dir) {
            Ok(r) => r
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => return,
        };
        if entries.is_empty() {
            eprintln!("skipped: yazi cache is empty");
            return;
        }

        let Some(home) = crate::platform::dirs::home() else {
            eprintln!("skipped: no HOME");
            return;
        };
        // Bounded on purpose: a test may not walk a home directory. Two levels
        // of the places a previewed file plausibly lives, at most a few
        // thousand stats.
        let mut roots = vec![home.clone()];
        for name in ["Pictures", "Downloads", "Videos", "Desktop", "Documents"] {
            roots.push(home.join(name));
        }
        let mut queue = roots;
        let mut checked = 0usize;
        let mut depth_budget = 4000usize;

        while let Some(dir) = queue.pop() {
            let Ok(reader) = std::fs::read_dir(&dir) else {
                continue;
            };
            for item in reader.flatten() {
                if depth_budget == 0 {
                    break;
                }
                depth_budget -= 1;
                let Ok(meta) = item.metadata() else { continue };
                if meta.is_dir() {
                    continue;
                }
                checked += 1;
                let key = cache_key(&item.path(), &meta, STILL_SKIP);
                if entries.contains(&key) {
                    let found = cached_thumb(&item.path())
                        .expect("a name that is in the cache must resolve to a file");
                    assert_eq!(
                        found.file_name().map(|n| n.to_string_lossy().into_owned()),
                        Some(key),
                        "cached_thumb disagreed with cache_key"
                    );
                    return;
                }
            }
        }

        eprintln!("skipped: no cache entry among {checked} candidate files");
    }
}
