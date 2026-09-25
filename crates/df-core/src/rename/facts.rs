//! What is known about one file, gathered once and resolved against many times.
//!
//! The template is re-resolved on every keystroke in the template field, for
//! every row on the card. None of that can touch the disk: a stat per row per
//! keystroke is the kind of work that turns typing into waiting on a network
//! mount. So each row's facts are read once, when the card opens, and the
//! template resolves against this plain value from then on.
//!
//! The photo half is the exception, and the reason [`Photo`] has a `Pending`
//! arm. Opening a JPEG to read its EXIF is a real read, a hundred of them is a
//! noticeable one, and none of it may hold the card up. So a worker fills the
//! photo facts in behind the card, and until it has, a template that asks for
//! them answers "not yet" rather than something it would later have to take
//! back.
//!
//! # Dates are civil
//!
//! Every date here is a local wall-clock reading, [`Civil`], not an instant.
//! EXIF stamps carry no zone at all — `2024:05:06 14:03:22` is whatever the
//! camera's clock said — so there is nothing to convert them *from*. The file
//! dates are converted *into* local time once, at stat, so that a photo's taken
//! date and its file's modified date compare like with like, and so that
//! `{date}` names a file by the day its owner would say it was made.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// A local civil date-time. EXIF stamps carry no zone, so everything here is
/// civil local time and compares lexicographically as a tuple.
///
/// The field order is the comparison order: the derived `Ord` walks year,
/// month, day, hour, minute, second, which is "earlier" without a calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Civil {
    /// A `SystemTime` on the machine's local wall clock.
    ///
    /// `localtime_r` rather than a hand-rolled calendar, for the reason
    /// df-app's `format::civil_local` gives: the calendar is the easy half, and
    /// the hard half — which offset applied on that date under that year's DST
    /// rules — is the zoneinfo database this program has no business carrying.
    ///
    /// `None` only if the C library refuses, which in practice means a time so
    /// far out that the year no longer fits its `int`.
    #[allow(unsafe_code)]
    pub fn local(time: SystemTime) -> Option<Civil> {
        // Times before 1970 are legal on a filesystem (a bogus archive stamp, a
        // clock that was wrong), and `duration_since` refuses them, so the sign
        // is recovered rather than dropped. The fraction rounds *down* the
        // timeline: half a second before the epoch is 23:59:59, not 00:00:00.
        let secs = match time.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).ok()?,
            Err(e) => {
                let d = e.duration();
                let whole = i64::try_from(d.as_secs()).ok()?;
                -whole.checked_add(i64::from(d.subsec_nanos() > 0))?
            }
        };
        let t = libc::time_t::try_from(secs).ok()?;
        // SAFETY: `libc::tm` is a plain C struct of integers (and, on glibc, a
        // zone-name pointer), for which all-zero bits are a valid value.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `t` and `tm` are owned locals of the right types, and
        // `localtime_r` writes only into `tm`. The `_r` form is the reentrant
        // one, so there is no shared static to race the photo workers over.
        let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
        if !ok {
            return None;
        }
        Some(Civil {
            year: tm.tm_year.checked_add(1900)?,
            month: u32::try_from(tm.tm_mon).ok()?.checked_add(1)?,
            day: u32::try_from(tm.tm_mday).ok()?,
            hour: u32::try_from(tm.tm_hour).ok()?,
            minute: u32::try_from(tm.tm_min).ok()?,
            // A leap second reads as :60 from some C libraries. A filename has
            // no use for it, and every formatter downstream assumes 0–59.
            second: u32::try_from(tm.tm_sec).ok()?.min(59),
        })
    }

    /// The wall clock right now, for `{date}`'s "is this taken date believable"
    /// window.
    pub fn now() -> Option<Civil> {
        Civil::local(SystemTime::now())
    }
}

/// What a photo reader found. `Pending` is "a worker is still reading it".
///
/// `None` and `Some` with every field empty are different answers on purpose:
/// the first is "not a format the reader understands", the second is "a JPEG,
/// but it says nothing". A template resolves both the same way; the card may
/// not want to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Photo {
    Pending,
    None,
    Some(PhotoFacts),
}

/// The four questions the template asks of a photo, answered as far as the file
/// allows. Each one is independently optional: plenty of JPEGs have
/// dimensions and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhotoFacts {
    pub taken: Option<Civil>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub make: Option<String>,
    pub model: Option<String>,
}

/// One row's worth of knowledge: the name, the stat, and (eventually) the
/// photo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The full current file name.
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<Civil>,
    /// btime, where the filesystem has one (std's `Metadata::created`).
    pub created: Option<Civil>,
    /// The name of the directory the file is in (its last path component).
    pub parent: String,
    pub photo: Photo,
}

impl Facts {
    /// Stat `dir/name`, without following a symlink: a link is renamed as a
    /// link, so it is the link's own dates and size that describe what is
    /// being renamed.
    ///
    /// Never fails. A file that has vanished or cannot be statted still gets a
    /// row — its name and parent are known without the disk — with size 0 and
    /// no dates, and any template that needs a date says so on that row rather
    /// than the whole card refusing to open.
    ///
    /// `photo` starts as [`Photo::Pending`]; filling it in is the caller's
    /// (and a worker's) job.
    pub fn stat(dir: &Path, name: &str) -> Facts {
        let meta = std::fs::symlink_metadata(dir.join(name)).ok();
        let local = |t: std::io::Result<SystemTime>| t.ok().and_then(Civil::local);
        Facts {
            name: name.to_string(),
            is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
            size: meta.as_ref().map_or(0, |m| m.len()),
            modified: meta.as_ref().and_then(|m| local(m.modified())),
            created: meta.as_ref().and_then(|m| local(m.created())),
            // `/` has no last component; a file at the root has an empty
            // parent rather than a made-up one.
            parent: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            photo: Photo::Pending,
        }
    }

    /// The name without its extension.
    ///
    /// The same split the single-file rename prompt makes
    /// (`InputBuffer::for_rename_stem`), so `{name}` means exactly the part
    /// that prompt selects: the extension is from the last `.`, unless that
    /// dot is the first character (`.bashrc` is all stem). A directory has no
    /// extension at all — a folder called `v1.2` is not a `.2` file.
    pub fn stem(&self) -> &str {
        &self.name[..self.ext_start()]
    }

    /// The extension **with** its dot (`.jpg`), or empty. Carrying the dot is
    /// what makes `{name}{ext}` the identity for `Makefile` as well as for
    /// `photo.jpg`; a template that wrote the dot itself would leave a
    /// trailing one on every name without an extension.
    pub fn ext(&self) -> &str {
        &self.name[self.ext_start()..]
    }

    /// The byte offset where the extension begins, or the name's length when
    /// there is none. `rfind` gives a byte offset, and "not at 0" means the
    /// same thing in bytes as in chars.
    fn ext_start(&self) -> usize {
        if self.is_dir {
            return self.name.len();
        }
        match self.name.rfind('.') {
            Some(i) if i > 0 => i,
            _ => self.name.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempTree;

    fn named(name: &str, is_dir: bool) -> Facts {
        Facts {
            name: name.to_string(),
            is_dir,
            size: 0,
            modified: None,
            created: None,
            parent: String::new(),
            photo: Photo::Pending,
        }
    }

    fn split(name: &str, is_dir: bool) -> (String, String) {
        let f = named(name, is_dir);
        (f.stem().to_string(), f.ext().to_string())
    }

    #[test]
    fn the_extension_is_from_the_last_dot_and_keeps_it() {
        assert_eq!(split("photo.jpg", false), ("photo".into(), ".jpg".into()));
        assert_eq!(split("a.tar.gz", false), ("a.tar".into(), ".gz".into()));
        assert_eq!(split("trailing.", false), ("trailing".into(), ".".into()));
    }

    #[test]
    fn a_name_with_no_dot_after_the_first_character_has_no_extension() {
        assert_eq!(split("Makefile", false), ("Makefile".into(), String::new()));
        assert_eq!(split(".bashrc", false), (".bashrc".into(), String::new()));
        assert_eq!(split("", false), (String::new(), String::new()));
    }

    /// A dotfile with a real extension still splits at its *last* dot; only a
    /// dot at index 0 is disqualified.
    #[test]
    fn a_dotfile_can_still_have_an_extension() {
        assert_eq!(
            split(".config.toml", false),
            (".config".into(), ".toml".into())
        );
    }

    #[test]
    fn a_directory_never_has_an_extension() {
        assert_eq!(split("v1.2", true), ("v1.2".into(), String::new()));
        assert_eq!(
            split("photos.old", true),
            ("photos.old".into(), String::new())
        );
    }

    #[test]
    fn the_split_is_on_char_boundaries_in_a_multibyte_name() {
        assert_eq!(split("café.jpg", false), ("café".into(), ".jpg".into()));
        assert_eq!(split("日本.語", false), ("日本".into(), ".語".into()));
    }

    /// The same rule as the single-file prompt, checked against it rather than
    /// against a second copy of the expectation.
    #[test]
    fn the_split_matches_the_rename_prompts() {
        for name in crate::test_support::gnarly_names() {
            let prompt = crate::input::InputBuffer::for_rename_stem(&name);
            let facts = named(&name, false);
            assert_eq!(
                facts.stem(),
                &prompt.text()[..prompt.cursor_byte()],
                "{name:?}"
            );
        }
    }

    #[test]
    fn stat_reads_size_dates_and_parent() {
        let tree = TempTree::new("facts-stat");
        let dir = tree.dir("Holiday 2024");
        std::fs::write(dir.join("a.jpg"), b"12345").expect("write");
        let facts = Facts::stat(&dir, "a.jpg");
        assert_eq!(facts.name, "a.jpg");
        assert!(!facts.is_dir);
        assert_eq!(facts.size, 5);
        assert!(facts.modified.is_some());
        assert_eq!(facts.parent, "Holiday 2024");
        assert_eq!(facts.photo, Photo::Pending);
    }

    #[test]
    fn stat_knows_a_directory() {
        let tree = TempTree::new("facts-dir");
        tree.dir("v1.2");
        let facts = Facts::stat(tree.path(), "v1.2");
        assert!(facts.is_dir);
        assert_eq!(facts.ext(), "");
    }

    /// A symlink to a directory is renamed as a link, so it is described as
    /// one: not a directory, and its extension splits like any file's.
    #[test]
    fn stat_does_not_follow_a_symlink() {
        let tree = TempTree::new("facts-link");
        let target = tree.dir("real.d");
        tree.symlink(&target, "link.d");
        let facts = Facts::stat(tree.path(), "link.d");
        assert!(!facts.is_dir);
        assert_eq!(facts.ext(), ".d");
    }

    #[test]
    fn stat_of_a_missing_file_is_a_row_with_nothing_known() {
        let tree = TempTree::new("facts-missing");
        let facts = Facts::stat(tree.path(), "gone.txt");
        assert_eq!(facts.name, "gone.txt");
        assert_eq!(facts.size, 0);
        assert_eq!(facts.modified, None);
        assert_eq!(facts.created, None);
        assert!(!facts.parent.is_empty());
    }

    #[test]
    fn a_file_at_the_root_has_an_empty_parent() {
        assert_eq!(
            Facts::stat(Path::new("/"), "nonexistent-df-test").parent,
            ""
        );
    }

    /// The only thing that can be asserted about the timezone call without
    /// pinning the machine's zone: the epoch is somewhere in 1969–1970, and
    /// every field is in its calendar range.
    #[test]
    fn the_epoch_lands_in_the_right_year() {
        let civil = Civil::local(UNIX_EPOCH).expect("localtime_r");
        assert!((1969..=1970).contains(&civil.year), "{civil:?}");
        assert!((1..=12).contains(&civil.month));
        assert!((1..=31).contains(&civil.day));
        assert!(civil.hour < 24 && civil.minute < 60 && civil.second < 60);
    }

    #[test]
    fn a_time_before_the_epoch_is_still_a_date() {
        let before = UNIX_EPOCH - std::time::Duration::from_secs(400 * 86_400);
        let civil = Civil::local(before).expect("localtime_r");
        assert!((1968..=1969).contains(&civil.year), "{civil:?}");
    }

    #[test]
    fn now_is_a_plausible_year() {
        let now = Civil::now().expect("localtime_r");
        assert!(now.year >= 2024, "{now:?}");
    }

    #[test]
    fn civil_dates_compare_as_their_fields_read() {
        let at = |year, month, day, hour| Civil {
            year,
            month,
            day,
            hour,
            minute: 0,
            second: 0,
        };
        assert!(at(2023, 12, 31, 23) < at(2024, 1, 1, 0));
        assert!(at(2024, 2, 1, 0) < at(2024, 10, 1, 0));
        assert!(at(2024, 5, 6, 9) < at(2024, 5, 6, 10));
    }
}
