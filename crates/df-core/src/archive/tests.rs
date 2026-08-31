//! Fixtures built byte by byte, plus whatever the machine's archivers can add.
//!
//! Every format case that matters is handcrafted here rather than produced by a
//! tool, for two reasons: the interesting cases (a comment-tail EOCD, a pax
//! header, a name with `../..` in it) are exactly the ones a well-behaved
//! archiver will not write on request, and a fixture that is a byte string is a
//! fixture that still exists on a machine with no `zip` installed. The
//! tool-built tests below are additions, and skip themselves when the tool is
//! absent.

#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::io::Cursor;
use std::path::Path;
use std::process::{Command, Stdio};

use super::tar::TAR_BLOCK;
use super::*;
use crate::ops::fixture::TempTree;

// ── builders ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ZipMember {
    /// Bytes, not a `String`: the interesting name cases are the ones that are
    /// not UTF-8.
    name: Vec<u8>,
    data: Vec<u8>,
    external: u32,
    flags: u16,
    method: u16,
}

impl ZipMember {
    fn file(name: &str, data: &[u8]) -> ZipMember {
        ZipMember {
            name: name.as_bytes().to_vec(),
            data: data.to_vec(),
            external: 0,
            flags: 0,
            method: 0,
        }
    }

    fn raw(name: &[u8]) -> ZipMember {
        ZipMember {
            name: name.to_vec(),
            data: b"x".to_vec(),
            external: 0,
            flags: 0,
            method: 0,
        }
    }

    fn dir(name: &str) -> ZipMember {
        ZipMember {
            name: format!("{}/", name.trim_end_matches('/')).into_bytes(),
            data: Vec::new(),
            external: 0x10,
            flags: 0,
            method: 0,
        }
    }

    fn encrypted(mut self) -> ZipMember {
        self.flags |= 0x0001;
        self
    }

    fn deflated(mut self) -> ZipMember {
        self.method = 8;
        self
    }
}

/// A stored-only zip, optionally with a file comment after the EOCD.
fn build_zip(members: &[ZipMember], comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();

    for m in members {
        let at = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&m.flags.to_le_bytes());
        out.extend_from_slice(&m.method.to_le_bytes());
        // 1980-01-01 00:00:00, the epoch of the DOS timestamp.
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0x0021u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // crc, unread by a listing
        out.extend_from_slice(&(m.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(m.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(m.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&m.name);
        out.extend_from_slice(&m.data);

        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&m.flags.to_le_bytes());
        central.extend_from_slice(&m.method.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x0021u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&(m.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(m.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(m.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&m.external.to_le_bytes());
        central.extend_from_slice(&at.to_le_bytes());
        central.extend_from_slice(&m.name);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment);
    out
}

fn octal(field: &mut [u8], value: u64) {
    let text = format!("{value:o}");
    let width = field.len() - 1;
    let text = format!("{text:0>width$}");
    field[..width].copy_from_slice(&text.as_bytes()[text.len() - width..]);
    field[width] = 0;
}

/// A 512-byte tar header with a valid checksum.
fn tar_header(name: &str, size: u64, type_flag: u8, ustar: bool, prefix: &str) -> Vec<u8> {
    let mut h = vec![0u8; TAR_BLOCK];
    let n = name.as_bytes();
    h[..n.len().min(100)].copy_from_slice(&n[..n.len().min(100)]);
    octal(&mut h[100..108], 0o644);
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 1_600_000_000);
    h[148..156].fill(b' ');
    h[156] = type_flag;
    if ustar {
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let p = prefix.as_bytes();
        h[345..345 + p.len().min(155)].copy_from_slice(&p[..p.len().min(155)]);
    }
    let sum: u64 = h.iter().map(|b| *b as u64).sum();
    let checksum = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(checksum.as_bytes());
    h
}

fn tar_entry(out: &mut Vec<u8>, name: &str, data: &[u8], type_flag: u8) {
    out.extend_from_slice(&tar_header(name, data.len() as u64, type_flag, true, ""));
    out.extend_from_slice(data);
    let pad = (TAR_BLOCK - data.len() % TAR_BLOCK) % TAR_BLOCK;
    out.extend(std::iter::repeat_n(0u8, pad));
}

fn tar_end(out: &mut Vec<u8>) {
    out.extend(std::iter::repeat_n(0u8, TAR_BLOCK * 2));
}

fn names(entries: &[ArchiveEntry]) -> Vec<String> {
    entries.iter().map(|e| e.name.clone()).collect()
}

fn write(t: &TempTree, rel: &str, bytes: &[u8]) -> std::path::PathBuf {
    t.file(rel, bytes)
}

// ── zip ─────────────────────────────────────────────────────────────────────

#[test]
fn a_minimal_zip_lists_its_members() {
    let bytes = build_zip(
        &[
            ZipMember::file("readme.txt", b"hello"),
            ZipMember::dir("src"),
            ZipMember::file("src/main.rs", b"fn main() {}"),
        ],
        b"",
    );
    let (raws, truncated) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    assert!(!truncated);
    assert_eq!(raws.len(), 3);

    let tree = tree::build("/x.zip".into(), ArchiveFormat::Zip, raws, false);
    assert_eq!(names(&tree.entries("")), vec!["src", "readme.txt"]);
    assert_eq!(names(&tree.entries("src")), vec!["main.rs"]);
    assert_eq!(tree.file_count(), 2);
    assert_eq!(tree.dir_count(), 1);
    assert_eq!(tree.total_len(), 5 + 12);
    assert!(!tree.has_encrypted());
    assert_eq!(tree.unsafe_count(), 0);
}

#[test]
fn the_eocd_is_found_behind_a_comment_that_contains_its_own_signature() {
    // The nastiest legal zip: a comment long enough to need the backward scan,
    // holding a decoy `PK\x05\x06`. Only the comment-length check tells them
    // apart.
    let mut comment = vec![b'.'; 300];
    comment[100..104].copy_from_slice(b"PK\x05\x06");
    let bytes = build_zip(&[ZipMember::file("a.txt", b"x")], &comment);

    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].name, "a.txt");
}

#[test]
fn a_maximum_length_comment_still_finds_the_eocd() {
    let comment = vec![b'z'; 65_535];
    let bytes = build_zip(&[ZipMember::file("deep.txt", b"y")], &comment);
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    assert_eq!(raws.len(), 1);
}

#[test]
fn zip_metadata_survives_the_trip() {
    let bytes = build_zip(
        &[
            ZipMember::file("stored.bin", b"12345").deflated(),
            ZipMember::file("secret.txt", b"nope").encrypted(),
        ],
        b"",
    );
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();

    assert_eq!(raws[0].method, Method::Deflate);
    assert_eq!(raws[0].len, 5);
    assert!(!raws[0].encrypted);
    assert!(raws[1].encrypted, "bit 0 of the flags is the password bit");
    // 1980-01-01T00:00:00Z, the DOS epoch, is 315,532,800 unix seconds.
    assert_eq!(raws[0].mtime, Some(315_532_800));

    let tree = tree::build("/x.zip".into(), ArchiveFormat::Zip, raws, false);
    assert!(tree.has_encrypted());
}

#[test]
fn a_directory_is_recognized_by_slash_or_by_attribute() {
    let mut attr_only = ZipMember::file("bare", b"");
    attr_only.external = 0x10;
    let mut unix_mode = ZipMember::file("unixdir", b"");
    unix_mode.external = 0o040755 << 16;

    let bytes = build_zip(&[ZipMember::dir("slashed"), attr_only, unix_mode], b"");
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    assert!(raws.iter().all(|r| r.is_dir), "{raws:?}");
}

#[test]
fn a_zip_that_is_too_short_is_a_clear_error() {
    let err = zip::list(&mut Cursor::new(b"PK\x05\x06".to_vec()), 4).unwrap_err();
    assert!(err.to_string().contains("zip"), "{err}");
}

#[test]
fn a_zip_with_no_eocd_is_a_clear_error() {
    let bytes = vec![b'x'; 4096];
    let err = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap_err();
    assert!(
        err.to_string().contains("end-of-central-directory"),
        "{err}"
    );
}

#[test]
fn a_truncated_zip_never_panics() {
    let full = build_zip(
        &[
            ZipMember::file("a.txt", b"aaaa"),
            ZipMember::file("b/c.txt", b"bbbb"),
        ],
        b"",
    );
    // Every prefix of a valid zip: some parse, most do not, none may panic.
    for cut in 0..full.len() {
        let bytes = &full[..cut];
        let _ = zip::list(&mut Cursor::new(bytes.to_vec()), bytes.len() as u64);
    }
}

#[test]
fn a_zip_claiming_a_giant_central_directory_is_bounded() {
    let mut bytes = build_zip(&[ZipMember::file("a.txt", b"x")], b"");
    // Overwrite the EOCD's central-directory size with something enormous but
    // not the zip64 sentinel. The read must clamp to what the file holds rather
    // than allocating four gigabytes.
    let eocd = bytes.len() - 22;
    bytes[eocd + 12..eocd + 16].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
    let len = bytes.len() as u64;
    let (raws, truncated) = zip::list(&mut Cursor::new(&bytes), len).unwrap();
    assert_eq!(raws.len(), 1);
    assert!(
        truncated,
        "a clamped central directory is a truncated listing"
    );
}

#[test]
fn a_cp437_name_is_decoded_rather_than_mangled() {
    // `M`, 0x81 (`ü` in CP437), `ller.txt`. Not valid UTF-8, so the fallback
    // table is the only thing between the user and `M?ller.txt`.
    let mut name = b"M".to_vec();
    name.push(0x81);
    name.extend_from_slice(b"ller.txt");
    let bytes = build_zip(&[ZipMember::raw(&name)], b"");
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    assert_eq!(raws[0].name, "Müller.txt");
}

#[test]
fn a_utf8_name_is_never_run_through_the_cp437_table() {
    let bytes = build_zip(&[ZipMember::raw("naïve/文書.txt".as_bytes())], b"");
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    assert_eq!(raws[0].name, "naïve/文書.txt");
}

// ── tar ─────────────────────────────────────────────────────────────────────

#[test]
fn a_plain_ustar_lists() {
    let mut bytes = Vec::new();
    tar_entry(&mut bytes, "top/", b"", b'5');
    tar_entry(&mut bytes, "top/a.txt", b"hello world", b'0');
    tar_entry(&mut bytes, "top/sub/b.bin", &[7u8; 1000], b'0');
    tar_end(&mut bytes);

    let (raws, truncated) = tar::list(Cursor::new(&bytes)).unwrap();
    assert!(!truncated);
    assert_eq!(raws.len(), 3);
    assert_eq!(raws[1].len, 11);
    assert_eq!(raws[2].len, 1000);
    assert_eq!(raws[1].mtime, Some(1_600_000_000));

    let tree = tree::build("/x.tar".into(), ArchiveFormat::Tar, raws, false);
    assert_eq!(names(&tree.entries("")), vec!["top"]);
    assert_eq!(names(&tree.entries("top")), vec!["sub", "a.txt"]);
    assert_eq!(names(&tree.entries("top/sub")), vec!["b.bin"]);
}

#[test]
fn a_ustar_prefix_is_rejoined_to_the_name() {
    let mut bytes = Vec::new();
    let long_dir = "a-very-long-directory-name-that-eats-the-hundred-byte-field";
    bytes.extend_from_slice(&tar_header("leaf.txt", 4, b'0', true, long_dir));
    bytes.extend_from_slice(b"data");
    bytes.extend(std::iter::repeat_n(0u8, TAR_BLOCK - 4));
    tar_end(&mut bytes);

    let (raws, _) = tar::list(Cursor::new(&bytes)).unwrap();
    assert_eq!(raws[0].name, format!("{long_dir}/leaf.txt"));
}

#[test]
fn a_gnu_long_name_entry_renames_the_entry_after_it() {
    let long = format!("{}/deep.txt", "n".repeat(300));
    let mut payload = long.clone().into_bytes();
    payload.push(0);

    let mut bytes = Vec::new();
    tar_entry(&mut bytes, "././@LongLink", &payload, b'L');
    tar_entry(&mut bytes, "short.txt", b"body", b'0');
    tar_entry(&mut bytes, "after.txt", b"x", b'0');
    tar_end(&mut bytes);

    let (raws, _) = tar::list(Cursor::new(&bytes)).unwrap();
    assert_eq!(raws.len(), 2, "the L pseudo-entry is not a row");
    assert_eq!(raws[0].name.trim_end_matches('\0'), long);
    assert_eq!(
        raws[1].name, "after.txt",
        "the long name applies to exactly one entry"
    );
}

#[test]
fn a_pax_header_overrides_path_and_size() {
    let path = "pax/very/deep/name.txt";
    let mut records = Vec::new();
    for (key, value) in [("path", path), ("size", "12345")] {
        let body = format!("{key}={value}\n");
        // The length field counts itself, which means solving for it.
        let mut len = body.len() + 2;
        loop {
            let candidate = format!("{len} {body}");
            if candidate.len() == len {
                records.extend_from_slice(candidate.as_bytes());
                break;
            }
            len = candidate.len();
        }
    }

    let mut bytes = Vec::new();
    tar_entry(&mut bytes, "PaxHeaders/0/x", &records, b'x');
    // The real header still says `short.txt` and a size of 0 — the pax record is
    // what a correct parser believes.
    tar_entry(&mut bytes, "short.txt", b"", b'0');
    tar_end(&mut bytes);

    let (raws, _) = tar::list(Cursor::new(&bytes)).unwrap();
    assert_eq!(raws.len(), 1);
    assert_eq!(raws[0].name, path);
    assert_eq!(raws[0].len, 12_345);
}

#[test]
fn a_base_256_size_field_is_read() {
    let mut header = tar_header("huge.bin", 0, b'0', true, "");
    // GNU's escape from the 8 GiB octal ceiling: high bit set, big-endian after.
    let size: u64 = 12 * 1024 * 1024 * 1024;
    header[124..136].fill(0);
    header[124] = 0x80;
    header[128..136].copy_from_slice(&size.to_be_bytes());
    let sum: u64 = {
        let mut h = header.clone();
        h[148..156].fill(b' ');
        h.iter().map(|b| *b as u64).sum()
    };
    let checksum = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(checksum.as_bytes());

    // Only the header is listed; the 12 GiB payload is not in the fixture, so
    // the stream ends early and the listing says so.
    let (raws, truncated) = tar::list(Cursor::new(&header)).unwrap();
    assert_eq!(raws[0].len, size);
    assert!(truncated);
}

#[test]
fn a_symlink_entry_keeps_its_target() {
    let mut header = tar_header("link", 0, b'2', true, "");
    header[157..157 + 8].copy_from_slice(b"../other");
    let sum: u64 = {
        let mut h = header.clone();
        h[148..156].fill(b' ');
        h.iter().map(|b| *b as u64).sum()
    };
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut bytes = header;
    tar_end(&mut bytes);

    let (raws, _) = tar::list(Cursor::new(&bytes)).unwrap();
    assert_eq!(raws[0].link_target.as_deref(), Some("../other"));
}

#[test]
fn a_long_name_is_capped_rather_than_allocated() {
    let long = "z".repeat(10_000);
    let mut payload = long.into_bytes();
    payload.push(0);
    let mut bytes = Vec::new();
    tar_entry(&mut bytes, "././@LongLink", &payload, b'L');
    tar_entry(&mut bytes, "short.txt", b"", b'0');
    tar_end(&mut bytes);

    let (raws, _) = tar::list(Cursor::new(&bytes)).unwrap();
    assert_eq!(raws[0].name.len(), MAX_NAME_BYTES);
}

#[test]
fn a_bad_checksum_is_a_clear_error_not_a_listing() {
    let mut bytes = tar_header("a.txt", 0, b'0', true, "");
    bytes[0] = b'Z'; // changes the name, and so the checksum
    tar_end(&mut bytes);
    let err = tar::list(Cursor::new(&bytes)).unwrap_err();
    assert!(err.to_string().contains("checksum"), "{err}");
}

#[test]
fn a_tar_cut_mid_stream_reports_truncation_and_keeps_what_it_read() {
    let mut bytes = Vec::new();
    tar_entry(&mut bytes, "first.txt", b"aaaa", b'0');
    tar_entry(&mut bytes, "second.txt", &[1u8; 2000], b'0');
    tar_end(&mut bytes);
    bytes.truncate(TAR_BLOCK * 3 + 100);

    let (raws, truncated) = tar::list(Cursor::new(&bytes)).unwrap();
    assert_eq!(raws.len(), 2);
    assert!(truncated);
}

#[test]
fn something_that_is_not_a_tar_at_all_is_an_error() {
    let bytes = vec![b'q'; TAR_BLOCK * 4];
    let err = tar::list(Cursor::new(&bytes)).unwrap_err();
    assert!(err.to_string().contains("tar"), "{err}");
}

#[test]
fn every_prefix_of_a_tar_parses_or_errors_without_panicking() {
    let mut full = Vec::new();
    tar_entry(&mut full, "d/", b"", b'5');
    tar_entry(&mut full, "d/a.txt", b"hello", b'0');
    tar_end(&mut full);
    for cut in 0..full.len() {
        let _ = tar::list(Cursor::new(&full[..cut]));
    }
}

// ── the tree: synthesis, ordering, safety ───────────────────────────────────

#[test]
fn intermediate_directories_are_synthesized_from_prefixes() {
    // A zip of a selection: only the leaves are stored, nothing says `a/` or
    // `a/b/` exists.
    let bytes = build_zip(
        &[
            ZipMember::file("a/b/c/deep.txt", b"x"),
            ZipMember::file("a/other.txt", b"y"),
        ],
        b"",
    );
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    let tree = tree::build("/x.zip".into(), ArchiveFormat::Zip, raws, false);

    assert_eq!(names(&tree.entries("")), vec!["a"]);
    assert_eq!(names(&tree.entries("a")), vec!["b", "other.txt"]);
    assert_eq!(names(&tree.entries("a/b")), vec!["c"]);
    assert_eq!(names(&tree.entries("a/b/c")), vec!["deep.txt"]);

    let b = tree.get("a/b").unwrap();
    assert!(b.is_dir && b.synthesized);
    assert!(!tree.get("a/b/c/deep.txt").unwrap().synthesized);
    assert_eq!(tree.dir_count(), 3);
    assert_eq!(tree.file_count(), 2);
}

#[test]
fn a_real_header_replaces_a_synthesized_directory() {
    // The leaf comes first, so `a/` is synthesized, and the real `a/` header
    // arrives afterwards carrying an mtime.
    let bytes = build_zip(
        &[ZipMember::file("a/x.txt", b"x"), ZipMember::dir("a")],
        b"",
    );
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    let tree = tree::build("/x.zip".into(), ArchiveFormat::Zip, raws, false);

    let a = tree.get("a").unwrap();
    assert!(a.is_dir);
    assert!(!a.synthesized);
    assert_eq!(names(&tree.entries("a")), vec!["x.txt"], "children survive");
    assert_eq!(tree.dir_count(), 1);
}

#[test]
fn listings_are_directories_first_then_by_name() {
    let bytes = build_zip(
        &[
            ZipMember::file("zeta.txt", b""),
            ZipMember::file("Alpha.txt", b""),
            ZipMember::dir("zzz"),
            ZipMember::dir("Aaa"),
        ],
        b"",
    );
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    let tree = tree::build("/x.zip".into(), ArchiveFormat::Zip, raws, false);
    assert_eq!(
        names(&tree.entries("")),
        vec!["Aaa", "zzz", "Alpha.txt", "zeta.txt"]
    );
}

#[test]
fn traversal_and_absolute_and_dot_git_names_are_flagged() {
    for name in [
        "../escape.txt",
        "a/../../escape.txt",
        "/etc/cron.d/evil",
        "\\windows\\system32\\x",
        "C:\\Users\\x",
        ".git/hooks/post-checkout",
        "repo/.GIT/config",
        "deep/path/.Git/hooks/pre-commit",
        "",
    ] {
        assert!(name_is_unsafe(name), "{name:?} should be flagged");
    }
    for name in [
        "a/b.txt",
        "normal.txt",
        "..hidden/x",
        "a/.gitignore",
        "a/git/config",
        "dots../x",
    ] {
        assert!(!name_is_unsafe(name), "{name:?} should be fine");
    }
    assert!(name_is_unsafe(&"x".repeat(MAX_NAME_BYTES + 1)));
    assert!(name_is_unsafe("has\0nul"));
}

#[test]
fn unsafe_entries_are_listed_and_flagged_but_never_escape_the_tree() {
    let bytes = build_zip(
        &[
            ZipMember::file("../../etc/passwd", b"root:x"),
            ZipMember::file("/absolute.txt", b"a"),
            ZipMember::file(".git/config", b"c"),
            ZipMember::file("fine.txt", b"f"),
        ],
        b"",
    );
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    let tree = tree::build("/x.zip".into(), ArchiveFormat::Zip, raws, false);

    assert_eq!(tree.unsafe_count(), 3);
    // The traversing entry is visible as a literal `..` directory, not as
    // something that climbed out of the listing.
    assert!(names(&tree.entries("")).contains(&"..".to_string()));
    assert_eq!(names(&tree.entries("../..")), vec!["etc"]);
    // The absolute name lost its leading slash and is flagged.
    assert!(tree.get("absolute.txt").unwrap().unsafe_name);
    assert!(tree.get(".git/config").unwrap().unsafe_name);
    assert!(!tree.get("fine.txt").unwrap().unsafe_name);
}

#[test]
fn normalize_agrees_with_itself_about_separators_and_dots() {
    assert_eq!(normalize("a/b/c").as_deref(), Some("a/b/c"));
    assert_eq!(normalize("./a//b/").as_deref(), Some("a/b"));
    assert_eq!(normalize("a\\b").as_deref(), Some("a/b"));
    assert_eq!(normalize("/a/").as_deref(), Some("a"));
    assert_eq!(normalize(""), None);
    assert_eq!(normalize("/"), None);
    assert_eq!(normalize("./."), None);
}

// ── extraction planning ─────────────────────────────────────────────────────

fn sample_tree() -> ArchiveTree {
    let bytes = build_zip(
        &[
            ZipMember::file("src/main.rs", b"fn main() {}"),
            ZipMember::file("src/lib.rs", b"pub fn x() {}"),
            ZipMember::file("docs/readme.md", b"# hi"),
            ZipMember::file("../escape.txt", b"bad"),
            ZipMember::file("locked.txt", b"secret").encrypted(),
        ],
        b"",
    );
    let (raws, _) = zip::list(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
    tree::build("/x.zip".into(), ArchiveFormat::Zip, raws, false)
}

#[test]
fn a_plan_refuses_unsafe_names_and_says_so() {
    let tree = sample_tree();
    let plan = plan_extract(&tree, &[], Path::new("/tmp/out"));

    assert!(
        plan.items.iter().all(|i| i.dest.starts_with("/tmp/out")),
        "{:?}",
        plan.items
    );
    assert!(plan
        .items
        .iter()
        .all(|i| !i.inner.split('/').any(|c| c == "..")));
    let reasons: Vec<SkipReason> = plan.skipped.iter().map(|(_, r)| *r).collect();
    assert!(reasons.contains(&SkipReason::UnsafeName));
    assert!(reasons.contains(&SkipReason::Encrypted));
}

#[test]
fn a_plan_puts_directories_before_the_files_inside_them() {
    let tree = sample_tree();
    let plan = plan_extract(&tree, &[], Path::new("/tmp/out"));
    let first_file = plan.items.iter().position(|i| !i.is_dir).unwrap();
    assert!(
        plan.items[..first_file].iter().all(|i| i.is_dir),
        "directories come first so parents exist before children"
    );
}

#[test]
fn a_selection_brings_its_subtree_and_nothing_else() {
    let tree = sample_tree();
    let plan = plan_extract(&tree, &["src"], Path::new("/tmp/out"));
    let inner: Vec<&str> = plan.items.iter().map(|i| i.inner.as_str()).collect();
    assert_eq!(
        inner,
        vec!["src", "src/main.rs", "src/lib.rs"],
        "directories first, then files in the order the archive listed them"
    );
    assert_eq!(plan.total_len, 12 + 13);

    // A trailing slash selects the same subtree.
    let same = plan_extract(&tree, &["src/"], Path::new("/tmp/out"));
    assert_eq!(same.items.len(), plan.items.len());
}

#[test]
fn a_plan_notices_what_is_already_on_disk() {
    let t = TempTree::new("archive-plan");
    let out = t.dir("out");
    t.file("out/docs/readme.md", b"already here");

    let tree = sample_tree();
    let plan = plan_extract(&tree, &["docs"], &out);
    // Two: the directory that already exists and the file inside it. A
    // directory conflict is not a problem the way a file conflict is, and the UI
    // separates them — but the plan reports both rather than deciding.
    assert_eq!(plan.conflicts, 2);
    assert!(plan.items.iter().any(|i| i.conflict && !i.is_dir));
}

#[test]
fn a_destination_is_never_offered_for_an_unsafe_entry() {
    let tree = sample_tree();
    let bad = tree.get("../escape.txt").unwrap();
    assert_eq!(destination_for(bad, Path::new("/tmp/out")), None);
    let good = tree.get("src/main.rs").unwrap();
    assert_eq!(
        destination_for(good, Path::new("/tmp/out")),
        Some(Path::new("/tmp/out/src/main.rs").to_path_buf())
    );
}

// ── detection and the whole pipeline ────────────────────────────────────────

#[test]
fn formats_are_detected_by_their_bytes() {
    let zip = build_zip(&[ZipMember::file("a", b"")], b"");
    assert_eq!(
        format_for(&zip, Path::new("mislabelled.txt")).unwrap(),
        ArchiveFormat::Zip
    );

    let mut tar = Vec::new();
    tar_entry(&mut tar, "a", b"", b'0');
    assert_eq!(
        format_for(&tar, Path::new("x.bin")).unwrap(),
        ArchiveFormat::Tar
    );

    assert_eq!(
        format_for(b"\x1f\x8b\x08\x00", Path::new("x.tgz")).unwrap(),
        ArchiveFormat::TarGz
    );
    assert_eq!(
        format_for(b"\xfd7zXZ\x00\x00", Path::new("x.txz")).unwrap(),
        ArchiveFormat::TarXz
    );
    assert_eq!(
        format_for(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0], Path::new("x.tzst")).unwrap(),
        ArchiveFormat::TarZst
    );
}

#[test]
fn unsupported_containers_name_themselves() {
    for (magic, want) in [
        (b"7z\xbc\xaf\x27\x1c".as_slice(), "7z"),
        (b"Rar!\x1a\x07\x00".as_slice(), "rar"),
        (b"BZh9".as_slice(), "bzip2"),
    ] {
        let err = format_for(magic, Path::new("x")).unwrap_err();
        assert!(err.to_string().contains(want), "{err}");
        assert!(err.to_string().contains("not supported"), "{err}");
    }
}

#[test]
fn a_plain_file_is_not_an_archive() {
    let err = format_for(b"just some text\n", Path::new("notes.txt")).unwrap_err();
    assert!(err.to_string().contains("not an archive"), "{err}");
}

#[test]
fn list_reads_a_zip_from_disk() {
    let t = TempTree::new("archive-list");
    let bytes = build_zip(
        &[
            ZipMember::dir("pkg"),
            ZipMember::file("pkg/one.txt", b"1111"),
            ZipMember::file("pkg/two.txt", b"22"),
        ],
        b"tail comment",
    );
    let path = write(&t, "x.zip", &bytes);

    let tree = list(&path).unwrap();
    assert_eq!(tree.format(), ArchiveFormat::Zip);
    assert_eq!(tree.path(), path);
    assert_eq!(names(&tree.entries("pkg")), vec!["one.txt", "two.txt"]);
    assert!(tree.is_dir(""));
    assert!(tree.is_dir("pkg"));
    assert!(!tree.is_dir("pkg/one.txt"));
    assert_eq!(tree.total_len(), 6);
}

#[test]
fn an_archive_error_becomes_a_df_error_with_the_message_intact() {
    let e = ArchiveError::Unsupported { format: "7z" };
    let text = e.to_string();
    let df: crate::DfError = e.into();
    assert_eq!(df.to_string(), text);
}

// ── compressed tars, through the system decompressors ───────────────────────

fn have_binary(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn compress(binary: &str, args: &[&str], input: &Path, output: &Path) -> bool {
    let Ok(src) = std::fs::File::open(input) else {
        return false;
    };
    let Ok(dst) = std::fs::File::create(output) else {
        return false;
    };
    Command::new(binary)
        .args(args)
        .stdin(Stdio::from(src))
        .stdout(Stdio::from(dst))
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn sample_tar_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    tar_entry(&mut bytes, "pkg/", b"", b'5');
    tar_entry(&mut bytes, "pkg/a.txt", b"alpha", b'0');
    tar_entry(&mut bytes, "pkg/nested/b.bin", &[3u8; 3000], b'0');
    tar_end(&mut bytes);
    bytes
}

fn assert_sample_tree(tree: &ArchiveTree) {
    assert_eq!(names(&tree.entries("")), vec!["pkg"]);
    assert_eq!(names(&tree.entries("pkg")), vec!["nested", "a.txt"]);
    assert_eq!(names(&tree.entries("pkg/nested")), vec!["b.bin"]);
    assert_eq!(tree.total_len(), 5 + 3000);
    assert!(!tree.truncated());
}

#[test]
fn an_uncompressed_tar_lists_from_disk() {
    let t = TempTree::new("archive-tar");
    let path = write(&t, "s.tar", &sample_tar_bytes());
    assert_sample_tree(&list(&path).unwrap());
}

#[test]
fn compressed_tars_stream_through_the_system_decompressors() {
    let t = TempTree::new("archive-compressed");
    let plain = write(&t, "s.tar", &sample_tar_bytes());

    let cases: [(&str, &[&str], &str, ArchiveFormat); 3] = [
        ("gzip", &["-c"], "s.tar.gz", ArchiveFormat::TarGz),
        ("xz", &["-c"], "s.tar.xz", ArchiveFormat::TarXz),
        ("zstd", &["-q", "-c"], "s.tar.zst", ArchiveFormat::TarZst),
    ];
    let mut ran = 0;
    for (binary, args, name, format) in cases {
        if !have_binary(binary) {
            eprintln!("skipping {name}: {binary} is not installed");
            continue;
        }
        let out = t.join(name);
        assert!(compress(binary, args, &plain, &out), "{binary} failed");

        let tree = list(&out).unwrap();
        assert_eq!(tree.format(), format);
        assert_sample_tree(&tree);
        assert!(format.is_available());
        ran += 1;
    }
    eprintln!("{ran} of 3 decompressors exercised");
}

#[test]
fn a_corrupt_compressed_tar_is_an_error_not_a_short_listing() {
    if !have_binary("gzip") {
        eprintln!("skipping: gzip is not installed");
        return;
    }
    let t = TempTree::new("archive-corrupt");
    let plain = write(&t, "s.tar", &sample_tar_bytes());
    let gz = t.join("s.tar.gz");
    assert!(compress("gzip", &["-c"], &plain, &gz));

    // Cut the stream in half: gzip exits non-zero, and that has to reach the
    // caller rather than being read as "the archive ends here".
    let mut bytes = std::fs::read(&gz).unwrap();
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&gz, &bytes).unwrap();

    match list(&gz) {
        Err(e) => assert!(
            e.to_string().contains("tar") || e.to_string().contains("gzip"),
            "{e}"
        ),
        Ok(tree) => assert!(
            tree.truncated(),
            "a half a gzip must not list as a complete archive"
        ),
    }
}

#[test]
fn a_zip_written_by_a_real_archiver_lists_the_same_way() {
    let t = TempTree::new("archive-real-zip");
    let src = t.dir("payload");
    std::fs::write(src.join("one.txt"), b"first").unwrap();
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("sub/two.txt"), b"second file").unwrap();

    let out = t.join("real.zip");
    let built = if have_binary("zip") {
        Command::new("zip")
            .arg("-qr")
            .arg(&out)
            .arg(".")
            .current_dir(&src)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else if have_binary("bsdtar") {
        Command::new("bsdtar")
            .arg("-a")
            .arg("-cf")
            .arg(&out)
            .arg(".")
            .current_dir(&src)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        false
    };
    if !built {
        eprintln!("skipping: no zip or bsdtar to build a fixture with");
        return;
    }

    let tree = list(&out).unwrap();
    assert_eq!(tree.format(), ArchiveFormat::Zip);
    assert!(names(&tree.entries("")).contains(&"one.txt".to_string()));
    assert!(names(&tree.entries("")).contains(&"sub".to_string()));
    assert_eq!(names(&tree.entries("sub")), vec!["two.txt"]);
    assert_eq!(tree.get("sub/two.txt").unwrap().len, 11);
    assert_eq!(tree.unsafe_count(), 0);
}
