//! The fixture tree the tests build on: gnarly names, made and torn down per
//! test with nothing but `std::fs` (PLAN §9).
//!
//! This lives in df-core, but it is not only df-core's. The `test-support`
//! feature makes it a normal `pub` module so df-app can add
//! `df-core = { …, features = ["test-support"] }` as a **dev**-dependency and
//! build its own operation tests on the same `TempTree` — one set of gnarly
//! names, exercised on both sides of the split, rather than two that drift.
//! Without the feature the module is `#[cfg(test)]` and ships in nothing.

use std::path::{Path, PathBuf};

/// A directory under `$TMPDIR` that deletes itself on drop.
pub struct TempTree {
    path: PathBuf,
}

impl TempTree {
    pub fn new(label: &str) -> TempTree {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let unique = format!(
            "delightfile-test-{}-{}-{}-{n}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempTree { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }

    /// Write a file, creating parents.
    pub fn file(&self, rel: impl AsRef<Path>, contents: &[u8]) -> PathBuf {
        let p = self.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("parents");
        }
        std::fs::write(&p, contents).expect("write");
        p
    }

    pub fn dir(&self, rel: impl AsRef<Path>) -> PathBuf {
        let p = self.join(rel);
        std::fs::create_dir_all(&p).expect("mkdir");
        p
    }

    pub fn symlink(&self, target: impl AsRef<Path>, rel: impl AsRef<Path>) -> PathBuf {
        let p = self.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("parents");
        }
        crate::platform::fs::symlink(target.as_ref(), &p).expect("symlink");
        p
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        // Best effort: a leaked temp dir is a nuisance, a panic in a
        // destructor is a lost test failure.
        let _ignored = std::fs::remove_dir_all(&self.path);
    }
}

/// The absolute path a Unix-shaped literal names, spelled for this platform:
/// `abs("/tmp/x")` is `/tmp/x` on Unix and `C:\tmp\x` on Windows, where
/// `/tmp/x` is not absolute. For tests about paths that never reach a disk —
/// a store's keys, a rail's arguments — so they say the same thing on every
/// target.
pub fn abs(unix: &str) -> PathBuf {
    let root = if std::path::MAIN_SEPARATOR == '\\' {
        "C:\\"
    } else {
        "/"
    };
    unix.split('/')
        .filter(|name| !name.is_empty())
        .fold(PathBuf::from(root), |path, name| path.join(name))
}

/// Whether the file system `dir` is on folds case: a file made as
/// `CASE-PROBE` opens as `case-probe`. Windows' and the default macOS volume
/// do; Linux's do not. For the tests that only mean something on such a
/// volume, which skip elsewhere.
pub fn folds_case(dir: &Path) -> bool {
    let upper = dir.join(format!("CASE-PROBE-{}", std::process::id()));
    let lower = dir.join(format!("case-probe-{}", std::process::id()));
    if std::fs::write(&upper, b"").is_err() {
        return false;
    }
    let folds = std::fs::symlink_metadata(&lower).is_ok();
    let _ignored = std::fs::remove_file(&upper);
    folds
}

/// The gnarly names of PLAN §9, in one place so every operation test
/// exercises the same set — the nastiest names the platform will make.
///
/// On Unix, where only NUL and `/` are refused, that includes a newline, a
/// tab, a backslash and a double quote. Windows refuses all four (and `<>:|?*`,
/// and a trailing dot or space), so its set is the nastiest it allows instead:
/// curly quotes, a leading space, a leading dot run, a combining accent, every
/// punctuation mark a shell cares about, a name that only starts like a
/// device, and a long name (200 units, so the longest path a test builds stays
/// short of what the Recycle Bin's calls take).
pub fn gnarly_names() -> Vec<String> {
    gnarly_names_for(crate::platform::os::STRICT_NAMES)
}

/// [`gnarly_names`] for strict (Windows) or permissive (Unix) names, both
/// sets on every target so each is checked against its rule.
fn gnarly_names_for(strict: bool) -> Vec<String> {
    if strict {
        return vec![
            "plain.txt".to_string(),
            "with spaces.txt".to_string(),
            "ünïcödé — 日本語 🎬.txt".to_string(),
            "'quoted' and ‘curly’ “double”.txt".to_string(),
            " leading space.txt".to_string(),
            "-leading-dash.txt".to_string(),
            "...leading dots.txt".to_string(),
            "e\u{301} combining.txt".to_string(),
            "semi;colon, comma & ampersand = $dollar @at !bang ~tilde.txt".to_string(),
            "[brackets] {braces} (parens) #hash +plus^caret`tick.txt".to_string(),
            "CON-only-looks-like-a-device.txt".to_string(),
            "x".repeat(200),
            "%20already-encoded.txt".to_string(),
        ];
    }
    vec![
        "plain.txt".to_string(),
        "with spaces.txt".to_string(),
        "ünïcödé — 日本語 🎬.txt".to_string(),
        "new\nline.txt".to_string(),
        "'quoted' and \"double\".txt".to_string(),
        "tab\there.txt".to_string(),
        "back\\slash.txt".to_string(),
        "-leading-dash.txt".to_string(),
        // 255 bytes is the ext4/btrfs limit for one name component; the
        // point is that nothing in here appends to a name without room.
        "x".repeat(255),
        "%20already-encoded.txt".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every gnarly name is one the platform takes: the set is the nastiest
    /// allowed, not a list of names the tests would fail to make.
    #[test]
    fn every_gnarly_name_is_one_its_platform_takes() {
        use crate::path::{name_is_valid_permissive, name_is_valid_strict};
        use std::ffi::OsStr;
        let windows = gnarly_names_for(true);
        let unix = gnarly_names_for(false);
        assert!(windows.len() >= 10 && unix.len() >= 10);
        for name in &windows {
            assert!(name_is_valid_strict(OsStr::new(name)).is_ok(), "{name:?}");
        }
        for name in &unix {
            assert!(
                name_is_valid_permissive(OsStr::new(name)).is_ok(),
                "{name:?}"
            );
        }
        assert!(
            unix.iter().any(|n| n.contains('\n')),
            "the Unix set is whole"
        );
        assert_eq!(
            gnarly_names(),
            gnarly_names_for(crate::platform::os::STRICT_NAMES)
        );
    }
}
