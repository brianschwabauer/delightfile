//! A binary format read by hand is only as trustworthy as its round-trip, so
//! the tests build `db.zo` files byte by byte with the *writer* the parser is
//! not allowed to see inside — and then, if this machine has a real zoxide
//! database, parse that too. The synthetic tests prove the parser; the real one
//! proves the format.

use super::*;

// ── A writer, so the parser has something to disagree with ──────────────────

/// Serialize a database exactly as the module header describes it. Written
/// independently of [`parse`] on purpose: a round-trip against a writer derived
/// from the parser proves only that the code is self-consistent.
fn encode(version: u32, dirs: &[(&str, f64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(dirs.len() as u64).to_le_bytes());
    for (path, rank, last) in dirs {
        out.extend_from_slice(&(path.len() as u64).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(&rank.to_le_bytes());
        out.extend_from_slice(&last.to_le_bytes());
    }
    out
}

fn fixture() -> Vec<(&'static str, f64, u64)> {
    vec![
        ("/home/brian/Work", 72.0, 1_700_000_000),
        ("/home/brian/Work/delightstack", 20.5, 1_699_990_000),
        ("/mnt/plex", 1.0, 1_600_000_000),
        ("/home/brian/Downloads", 6.0, 1_699_000_000),
        // A path that is not ASCII, because filenames are not.
        ("/home/brian/Musik/Grüße", 3.0, 1_699_900_000),
    ]
}

#[test]
fn a_written_database_reads_back_identically() {
    let dirs = fixture();
    let bytes = encode(DB_VERSION, &dirs);
    let parsed = parse(&bytes).expect("parses");
    assert_eq!(parsed.len(), dirs.len());
    for (got, (path, rank, last)) in parsed.iter().zip(&dirs) {
        assert_eq!(got.path, PathBuf::from(path));
        assert_eq!(got.rank, *rank);
        assert_eq!(got.last_accessed, *last);
    }
}

#[test]
fn an_empty_database_is_valid_and_empty() {
    let bytes = encode(DB_VERSION, &[]);
    assert_eq!(bytes.len(), 12, "just the header");
    assert_eq!(parse(&bytes), Ok(Vec::new()));
}

#[test]
fn the_header_is_the_layout_the_module_documents() {
    let bytes = encode(DB_VERSION, &[("/ab", 1.0, 2)]);
    // version, count, len, path, rank, last_accessed.
    assert_eq!(&bytes[0..4], &[3, 0, 0, 0]);
    assert_eq!(&bytes[4..12], &[1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&bytes[12..20], &[3, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&bytes[20..23], b"/ab");
    assert_eq!(bytes.len(), 12 + 8 + 3 + 8 + 8);
}

// ── Everything a corrupt file can be ────────────────────────────────────────

#[test]
fn a_future_version_is_refused_rather_than_guessed_at() {
    let bytes = encode(4, &fixture());
    assert_eq!(parse(&bytes), Err(ParseError::Version(4)));
}

#[test]
fn truncation_anywhere_is_caught() {
    let full = encode(DB_VERSION, &fixture());
    // Every prefix of a valid file except the file itself must fail, and none
    // of them may panic — which is the actual claim, since a hand-rolled
    // parser's failure mode is a slice out of range.
    for cut in 0..full.len() {
        let err = parse(&full[..cut]);
        assert!(err.is_err(), "prefix of {cut} bytes parsed");
    }
    assert!(parse(&full).is_ok());
}

#[test]
fn trailing_bytes_mean_the_layout_is_wrong() {
    let mut bytes = encode(DB_VERSION, &fixture());
    bytes.push(0);
    assert_eq!(parse(&bytes), Err(ParseError::Trailing(1)));
}

#[test]
fn an_absurd_count_does_not_become_an_allocation() {
    let mut bytes = encode(DB_VERSION, &[]);
    bytes[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(parse(&bytes), Err(ParseError::Absurd(u64::MAX)));
}

#[test]
fn a_path_that_is_not_utf8_is_a_corrupt_file() {
    let mut bytes = encode(DB_VERSION, &[("/ab", 1.0, 2)]);
    bytes[21] = 0xff;
    assert!(matches!(parse(&bytes), Err(ParseError::NotUtf8 { .. })));
}

#[test]
fn an_absurd_path_length_is_truncation_not_a_panic() {
    let mut bytes = encode(DB_VERSION, &[("/ab", 1.0, 2)]);
    bytes[12..20].copy_from_slice(&(1u64 << 40).to_le_bytes());
    assert!(matches!(parse(&bytes), Err(ParseError::Truncated { .. })));
}

#[test]
fn garbage_is_never_an_error_at_the_public_boundary() {
    let dir = TempDir::new("zoxide-garbage");
    let path = dir.path.join("db.zo");
    std::fs::write(&path, b"not a database at all").expect("write");
    assert_eq!(load_from(&path), Vec::new());
    // …and neither is a file that is not there.
    assert_eq!(load_from(&dir.path.join("absent.zo")), Vec::new());
}

#[test]
fn a_real_looking_database_loads_through_the_public_path() {
    let dir = TempDir::new("zoxide-good");
    let path = dir.path.join("db.zo");
    std::fs::write(&path, encode(DB_VERSION, &fixture())).expect("write");
    let dirs = load_from(&path);
    assert_eq!(dirs.len(), 5);
    assert_eq!(dirs[0].path, PathBuf::from("/home/brian/Work"));
}

#[test]
fn an_oversized_file_is_refused_before_it_is_read() {
    let dir = TempDir::new("zoxide-huge");
    let path = dir.path.join("db.zo");
    std::fs::write(&path, vec![0u8; (MAX_DB_BYTES + 1) as usize]).expect("write");
    assert_eq!(load_from(&path), Vec::new());
}

// ── Scoring ─────────────────────────────────────────────────────────────────

#[test]
fn the_aging_brackets_are_zoxides() {
    const NOW: u64 = 1_000_000_000;
    // (seconds since the visit, multiplier)
    let cases: &[(u64, f64)] = &[
        (0, 4.0),
        (HOUR - 1, 4.0),
        (HOUR, 2.0),
        (DAY - 1, 2.0),
        (DAY, 0.5),
        (WEEK - 1, 0.5),
        (WEEK, 0.25),
        (WEEK * 52, 0.25),
    ];
    for (elapsed, mult) in cases {
        let got = score(10.0, NOW - elapsed, NOW);
        assert_eq!(got, 10.0 * mult, "{elapsed}s ago");
    }
}

#[test]
fn a_clock_that_went_backwards_does_not_bury_a_directory() {
    // Last accessed "in the future": elapsed saturates to zero, so it lands in
    // the freshest bracket rather than wrapping into the stalest.
    assert_eq!(score(10.0, 2_000, 1_000), 40.0);
}

#[test]
fn score_on_a_dir_agrees_with_the_free_function() {
    let d = ZoxideDir {
        path: PathBuf::from("/tmp"),
        rank: 8.0,
        last_accessed: 500,
    };
    assert_eq!(d.score(500 + DAY), score(8.0, 500, 500 + DAY));
}

// ── Querying ────────────────────────────────────────────────────────────────

fn dirs() -> Vec<ZoxideDir> {
    fixture()
        .into_iter()
        .map(|(path, rank, last_accessed)| ZoxideDir {
            path: PathBuf::from(path),
            rank,
            last_accessed,
        })
        .collect()
}

/// `now` far enough after every fixture timestamp that they all share the ÷4
/// bracket, so ordering tests are about rank and match quality, not aging.
const LATER: u64 = 1_800_000_000;

#[test]
fn an_empty_query_is_the_frecency_list() {
    let got = query(&dirs(), "", LATER);
    let paths: Vec<_> = got.iter().map(|m| m.dir.path.to_string_lossy()).collect();
    assert_eq!(
        paths,
        [
            "/home/brian/Work",
            "/home/brian/Work/delightstack",
            "/home/brian/Downloads",
            "/home/brian/Musik/Grüße",
            "/mnt/plex",
        ]
    );
}

#[test]
fn matching_is_prefix_then_substring_then_keywords() {
    let cases: &[(&str, &str, MatchKind)] = &[
        ("del", "/home/brian/Work/delightstack", MatchKind::Prefix),
        (
            "light",
            "/home/brian/Work/delightstack",
            MatchKind::Component,
        ),
        (
            "work del",
            "/home/brian/Work/delightstack",
            MatchKind::Keywords,
        ),
        // Case folds both ways.
        ("DEL", "/home/brian/Work/delightstack", MatchKind::Prefix),
        ("downloads", "/home/brian/Downloads", MatchKind::Prefix),
        // Multibyte in the query and in the path.
        ("grüße", "/home/brian/Musik/Grüße", MatchKind::Prefix),
    ];
    for (q, path, kind) in cases {
        let got = query(&dirs(), q, LATER);
        let top = got
            .first()
            .unwrap_or_else(|| panic!("{q:?} matched nothing"));
        assert_eq!(top.dir.path, PathBuf::from(path), "{q:?}");
        assert_eq!(top.kind, *kind, "{q:?}");
    }
}

#[test]
fn a_prefix_match_outranks_a_more_frecent_substring_match() {
    // `/home/brian/Work` has ten times the rank, and `stack` still wins,
    // because the user typed something that names one directory and not the
    // other. Frecency breaks ties; it does not overrule the query.
    let got = query(&dirs(), "stack", LATER);
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].dir.path,
        PathBuf::from("/home/brian/Work/delightstack")
    );
}

#[test]
fn frecency_orders_within_a_match_quality() {
    // Both are prefix matches of `w`… only one is: `Work` starts with it,
    // `delightstack` does not. Use a query both components contain instead.
    let got = query(&dirs(), "o", LATER);
    let paths: Vec<_> = got.iter().map(|m| m.dir.path.to_string_lossy()).collect();
    // `Downloads` and `Work` both contain `o`; neither starts with it, so
    // rank decides: Work (72) before Downloads (6).
    assert!(paths.contains(&std::borrow::Cow::Borrowed("/home/brian/Work")));
    let work = paths
        .iter()
        .position(|p| p.ends_with("/Work"))
        .expect("Work present");
    let dl = paths
        .iter()
        .position(|p| p.ends_with("/Downloads"))
        .expect("Downloads present");
    assert!(work < dl, "{paths:?}");
}

#[test]
fn keywords_must_appear_in_order_and_end_in_the_last_component() {
    let d = dirs();
    // In order: matches.
    assert_eq!(query(&d, "work del", LATER).len(), 1);
    // Out of order: does not.
    assert!(query(&d, "del work", LATER).is_empty());
    // The last keyword has to land in the final component, so a query whose
    // last word only appears in a parent directory is not a match.
    assert!(query(&d, "delightstack work", LATER).is_empty());
    assert!(!query(&d, "brian work", LATER).is_empty());
}

#[test]
fn a_query_that_matches_nothing_is_empty_not_everything() {
    assert!(query(&dirs(), "zzzz", LATER).is_empty());
}

#[test]
fn scores_come_back_with_the_matches() {
    let d = dirs();
    let got = query(&d, "plex", LATER);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].score, score(1.0, 1_600_000_000, LATER));
}

// ── The real database, if there is one ──────────────────────────────────────

/// Parse this machine's actual `db.zo`.
///
/// Skipped, loudly but cleanly, when zoxide is not installed — PLAN §9's rule
/// that the suite must pass on a machine with nothing on it. When the file *is*
/// there this is the test that matters most: it is the only one whose fixture
/// was written by zoxide rather than by us.
#[test]
fn the_real_database_parses_if_it_exists() {
    let Some(path) = db_path() else {
        eprintln!("skipped: no HOME");
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipped: no {}", path.display());
        return;
    };
    let dirs = parse(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    // A database zoxide has been using is not empty, every path is absolute,
    // every rank is positive, and every timestamp is after 2001 — all four are
    // properties of the *format* being read correctly, not of this machine.
    assert!(!dirs.is_empty(), "{} has no entries", path.display());
    for d in &dirs {
        assert!(d.path.is_absolute(), "{:?} is not absolute", d.path);
        assert!(d.rank > 0.0 && d.rank < 1e6, "{:?} rank {}", d.path, d.rank);
        assert!(
            d.last_accessed > 1_000_000_000,
            "{:?} last_accessed {}",
            d.path,
            d.last_accessed
        );
    }
    eprintln!("parsed {} entries from {}", dirs.len(), path.display());
}

// ── A temp directory, since `tempfile` is not a dependency ──────────────────

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let mut path = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("df-{tag}-{unique}-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `testdata/db-v3.zo` is a database in the layout `main` read at 419a776,
/// before the path model of `plans/other-platforms/03-paths.md`: the
/// [`fixture`] rows and one path with a tab, a dash and CJK in it. It loads
/// with every path exactly the bytes that were written, on every target.
#[test]
fn a_database_main_read_still_loads_byte_for_byte() {
    let dirs = parse(include_bytes!("testdata/db-v3.zo")).expect("parses");
    let mut expected = fixture();
    expected.push(("/tmp/tab\there and — 日本語 🎬", 0.25, 1_234_567_890));
    assert_eq!(dirs.len(), expected.len());
    for (got, (path, rank, last)) in dirs.iter().zip(&expected) {
        assert_eq!(got.path.to_str(), Some(*path));
        assert_eq!(got.rank, *rank);
        assert_eq!(got.last_accessed, *last);
    }
}
