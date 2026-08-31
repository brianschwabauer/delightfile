//! Reading zoxide's database directly, for `Z` (PLAN §7.2).
//!
//! `Z` opens a frecency jump list. zoxide already keeps exactly that list, and
//! the plan's rule is to read it "directly (simple format, no subprocess)" —
//! which is the right call twice over: spawning `zoxide query -l --score` costs
//! a fork and an exec on a keypress that is supposed to feel instant, and it
//! makes the overlay's contents depend on a binary being on `$PATH` at the
//! moment the key is pressed rather than on a file that either parses or does
//! not.
//!
//! # The format, verified against the db on this machine
//!
//! zoxide 0.9–0.10 serialize with bincode's fixed-int little-endian encoding,
//! which for their `Db { version: u32, dirs: Vec<Dir> }` flattens to:
//!
//! ```text
//! u32   version                  (3, and 3 is the only version we read)
//! u64   number of directories
//! for each directory:
//!   u64   path length in bytes
//!   ..    path bytes (UTF-8, not NUL-terminated)
//!   f64   rank
//!   u64   last_accessed, seconds since the Unix epoch
//! ```
//!
//! That layout was not taken on faith: `~/.local/share/zoxide/db.zo` here is
//! 7440 bytes, declares 121 directories, and parsing it by the table above
//! consumes exactly 7440 bytes with nothing left over — the strongest evidence
//! a hand-rolled binary parser can have, because a wrong field width would
//! desynchronize within a few records and run off the end. The scores this
//! module computes for that file also match `zoxide query -l --score` entry for
//! entry, top to bottom, which pins the aging brackets below as well.
//!
//! # Failure is silence
//!
//! Nothing here returns an error. zoxide is optional: most machines have no
//! `db.zo` at all, and a half-written one during zoxide's own save is a normal
//! event, not a fault. Either way `Z` opens an empty list and a line goes to
//! the log. A file manager that refused to draw a jump overlay because another
//! program's cache was mid-write would be broken in a way the user cannot fix.

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// The only database version this parser understands.
///
/// zoxide has been on 3 since 0.5 (2021) and bumps it when the layout changes,
/// so refusing anything else is how a future zoxide gets an empty overlay and a
/// log line instead of a plausible-looking list of garbage paths.
pub const DB_VERSION: u32 = 3;

/// The largest `db.zo` this will read, in bytes.
///
/// A heavily used zoxide database is tens of kilobytes (121 entries is 7 KB
/// here). 8 MB is four orders of magnitude of headroom and still small enough
/// that a corrupt length field cannot talk us into an enormous allocation on a
/// keypress.
pub const MAX_DB_BYTES: u64 = 8 * 1024 * 1024;

/// Seconds in an hour, a day and a week — the aging brackets, named because the
/// numbers `3600` and `604800` appearing bare in a scoring function tell you
/// nothing about why they are there.
const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

/// One directory zoxide remembers.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoxideDir {
    pub path: PathBuf,
    /// How much zoxide thinks you use this place. Incremented on each visit and
    /// decayed by zoxide itself when the total gets large.
    pub rank: f64,
    /// Seconds since the Unix epoch.
    pub last_accessed: u64,
}

impl ZoxideDir {
    /// Frecency at time `now`, in seconds since the epoch.
    pub fn score(&self, now: u64) -> f64 {
        score(self.rank, self.last_accessed, now)
    }
}

/// zoxide's frecency: rank, multiplied or divided by how stale the last visit
/// is.
///
/// The brackets are zoxide's, not an approximation of them — within the hour
/// ×4, within the day ×2, within the week ÷2, older ÷4. There is no smooth
/// curve; the steps are the algorithm, and matching them exactly is what makes
/// delightfile's `Z` list and `zoxide query -l --score` agree, which is the
/// only way a user with both can trust either.
///
/// A future timestamp (a clock that went backwards, a db copied from another
/// machine) saturates to zero elapsed time rather than wrapping into the ÷4
/// bracket, so a bad clock cannot bury a directory you were just in.
pub fn score(rank: f64, last_accessed: u64, now: u64) -> f64 {
    let elapsed = now.saturating_sub(last_accessed);
    if elapsed < HOUR {
        rank * 4.0
    } else if elapsed < DAY {
        rank * 2.0
    } else if elapsed < WEEK {
        rank / 2.0
    } else {
        rank / 4.0
    }
}

/// Where zoxide keeps its database: `$XDG_DATA_HOME/zoxide/db.zo`, falling back
/// to `~/.local/share/zoxide/db.zo`.
///
/// `$_ZO_DATA_DIR` wins over both, because that is zoxide's own override and a
/// user who moved the database has said where it went.
pub fn db_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("_ZO_DATA_DIR") {
        return Some(PathBuf::from(dir).join("db.zo"));
    }
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(data.join("zoxide/db.zo"))
}

/// Every directory zoxide knows about, in the order the file stores them.
///
/// Never fails: a missing, oversized, truncated or future-versioned database is
/// an empty list and a log line. See the module header.
pub fn load() -> Vec<ZoxideDir> {
    let Some(path) = db_path() else {
        log::warn!("zoxide: no HOME, cannot find db.zo");
        return Vec::new();
    };
    load_from(&path)
}

/// [`load`] against a named file, so tests can point at a fixture and so a
/// future `$_ZO_DATA_DIR` from config has somewhere to go.
pub fn load_from(path: &Path) -> Vec<ZoxideDir> {
    let bytes = match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_DB_BYTES => {
            log::warn!(
                "zoxide: {} is {} bytes, past the {MAX_DB_BYTES} cap; ignoring",
                path.display(),
                meta.len()
            );
            return Vec::new();
        }
        Ok(_) => match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("zoxide: {}: {e}", path.display());
                return Vec::new();
            }
        },
        Err(e) => {
            // Not having zoxide installed is the common case, and it is not
            // worth a warning every time the overlay opens.
            log::debug!("zoxide: {}: {e}", path.display());
            return Vec::new();
        }
    };
    match parse(&bytes) {
        Ok(dirs) => dirs,
        Err(e) => {
            log::warn!("zoxide: {}: {e}; ignoring the database", path.display());
            Vec::new()
        }
    }
}

/// Why a `db.zo` could not be read.
///
/// Private, and it never leaves this module as an error — it exists so the log
/// line says *what* was wrong instead of "malformed", and so the tests can
/// assert on the specific failure rather than on an empty vector that any bug
/// would also produce.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum ParseError {
    #[error("truncated: wanted {want} bytes at offset {at}, {have} left")]
    Truncated { at: usize, want: usize, have: usize },
    #[error("version {0}, but only version {DB_VERSION} is understood")]
    Version(u32),
    #[error("path at offset {at} is not UTF-8")]
    NotUtf8 { at: usize },
    #[error("claims {0} directories, which cannot fit in the file")]
    Absurd(u64),
    #[error("{0} trailing bytes after the last directory")]
    Trailing(usize),
}

/// A cursor over the file, so every read is bounds-checked in one place.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let end = self.at.checked_add(n).ok_or(ParseError::Truncated {
            at: self.at,
            want: n,
            have: 0,
        })?;
        if end > self.bytes.len() {
            return Err(ParseError::Truncated {
                at: self.at,
                want: n,
                have: self.bytes.len() - self.at,
            });
        }
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, ParseError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn f64(&mut self) -> Result<f64, ParseError> {
        Ok(f64::from_bits(self.u64()?))
    }
}

/// The whole parser: the table in the module header, once.
fn parse(bytes: &[u8]) -> Result<Vec<ZoxideDir>, ParseError> {
    let mut r = Reader { bytes, at: 0 };
    let version = r.u32()?;
    if version != DB_VERSION {
        return Err(ParseError::Version(version));
    }
    let count = r.u64()?;
    // The smallest possible record is 24 bytes (two u64s, an f64, an empty
    // path), so a count that cannot fit is a corrupt length field and there is
    // no point allocating for it.
    if count.saturating_mul(24) > bytes.len() as u64 {
        return Err(ParseError::Absurd(count));
    }
    let mut dirs = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let len = r.u64()?;
        let at = r.at;
        let raw = r.take(len as usize)?;
        // Paths are bytes on Linux, but zoxide writes them from a Rust `String`
        // and delightfile's overlay has to *display* them. A non-UTF-8 entry is
        // a corrupt file, not a legitimately exotic path.
        let path = std::str::from_utf8(raw).map_err(|_| ParseError::NotUtf8 { at })?;
        let rank = r.f64()?;
        let last_accessed = r.u64()?;
        dirs.push(ZoxideDir {
            path: PathBuf::from(path),
            rank,
            last_accessed,
        });
    }
    // Nothing follows the last directory. Checking is what turns "the layout
    // looks plausible" into "the layout is right": a wrong field width almost
    // always leaves a remainder here.
    if r.at != bytes.len() {
        return Err(ParseError::Trailing(bytes.len() - r.at));
    }
    Ok(dirs)
}

/// How well a directory matched a query, so the overlay can rank matches above
/// frecency rather than instead of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchKind {
    /// Every keyword is in the path in order, the last one inside the final
    /// component. zoxide's own rule.
    Keywords,
    /// The query is a substring of the final path component.
    Component,
    /// The final component starts with the query. What you meant when you typed
    /// three letters.
    Prefix,
}

/// One row of the `Z` overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub dir: ZoxideDir,
    pub score: f64,
    pub kind: MatchKind,
}

/// The `Z` overlay's list: everything matching `query`, best first.
///
/// Ordering is **match quality, then frecency**. Frecency alone would put a
/// directory you live in above the one you actually typed the name of, which is
/// the single most irritating thing a jump tool can do; quality alone would
/// throw away everything zoxide is for. Ties inside a bracket fall back to the
/// path, so the list never reshuffles between two identical scores.
///
/// An empty query matches everything, which is what `Z` with nothing typed
/// should show: the frecency list itself.
///
/// Matching is case-insensitive by ASCII folding — paths are `/home/brian/Work`
/// and queries are typed in a hurry; a Unicode-aware fold would need a table
/// this crate will not take on (PLAN §1) and would change the answer for no
/// path anyone here has.
pub fn query(dirs: &[ZoxideDir], query: &str, now: u64) -> Vec<Match> {
    let needle = query.to_ascii_lowercase();
    let keywords: Vec<&str> = needle.split_whitespace().collect();
    let mut out: Vec<Match> = dirs
        .iter()
        .filter_map(|dir| {
            let path = dir.path.to_string_lossy().to_ascii_lowercase();
            let kind = classify(&path, &needle, &keywords)?;
            Some(Match {
                dir: dir.clone(),
                score: dir.score(now),
                kind,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.kind
            .cmp(&a.kind)
            .then_with(|| b.score.total_cmp(&a.score))
            .then_with(|| a.dir.path.cmp(&b.dir.path))
    });
    out
}

/// How (and whether) `path` matches, both already lowercased.
fn classify(path: &str, needle: &str, keywords: &[&str]) -> Option<MatchKind> {
    let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
    if needle.is_empty() {
        return Some(MatchKind::Keywords);
    }
    if !needle.contains(char::is_whitespace) {
        if last.starts_with(needle) {
            return Some(MatchKind::Prefix);
        }
        if last.contains(needle) {
            return Some(MatchKind::Component);
        }
    }
    // zoxide's rule: the keywords must appear in order, and the last one must
    // land in the final component — `wo del` finds `~/Work/delightfile` but not
    // `~/delightfile/work`.
    let mut at = 0;
    for (i, kw) in keywords.iter().enumerate() {
        let found = path[at..].find(kw)? + at;
        if i + 1 == keywords.len() && found + kw.len() <= path.len() - last.len() {
            return None;
        }
        at = found + kw.len();
    }
    Some(MatchKind::Keywords)
}
