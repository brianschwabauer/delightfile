//! Every in-house format, written and then read back by this crate's own
//! readers — and, where the machine has them, checked by the real tools.
//!
//! The readers do not carry a mode, and the zip reader does not read a link's
//! target while listing, so those two facts are read out of the bytes here by
//! a few lines of header parsing each. Everything else goes through
//! [`crate::archive::list`] and [`crate::archive::extract`], the same doors a
//! browsed or extracted archive goes through.

#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::*;
use crate::archive::tree::Method;
use crate::archive::{list, plan_extract, read_entry, ArchiveTree};
use crate::ops::fixture::TempTree;
use crate::tasks::{ProgressSink, TaskFlags};

/// 2023-11-14 22:13:21 UTC: odd seconds, which DOS time cannot hold.
const STAMP: i64 = 1_700_000_001;

/// Through the platform's own `set_times`: a handle opened only for reading
/// may not set a time on Windows, and a directory is not a `File` there.
fn set_mtime(path: &Path, secs: i64) {
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64);
    crate::platform::fs::set_times(path, None, Some(when)).unwrap();
}

/// The `st_mode` the platform reports for a fixture file, which is what the
/// archive has to carry: the mode `photos` set on Unix, and on Windows the
/// one made up from the read-only flag.
fn mode_on_disk(path: &Path) -> u32 {
    crate::platform::meta::mode(&std::fs::symlink_metadata(path).unwrap())
}

/// `photos/` with everything the writers have to get right in it: a text
/// file with a mode of its own, a hidden file, an empty file, an empty
/// folder, a symlink, a nested picture.
fn photos(t: &TempTree) -> PathBuf {
    let root = t.dir("src/photos");
    let text = t.file("src/photos/notes.txt", &b"the quick brown fox ".repeat(400));
    crate::platform::fs::apply_mode(&text, 0o640).unwrap();
    set_mtime(&text, STAMP);
    t.file("src/photos/.hidden", b"dotfile");
    let empty = t.file("src/photos/empty.txt", b"");
    set_mtime(&empty, STAMP + 2);
    t.file("src/photos/trip/IMG_0001.jpg", &[0xFFu8; 5000]);
    let run = t.file("src/photos/trip/run.sh", b"#!/bin/sh\necho hi\n");
    crate::platform::fs::apply_mode(&run, 0o755).unwrap();
    t.dir("src/photos/nothing");
    t.symlink("notes.txt", "src/photos/link");
    set_mtime(&t.join("src/photos/nothing"), STAMP + 4);
    root
}

fn pack(sources: Vec<PathBuf>, dest: PathBuf, format: Format) -> Packed {
    Pack {
        sources,
        dest,
        format,
        overwrite: false,
    }
    .run(&TaskCtx::detached())
    .unwrap()
}

fn paths(tree: &ArchiveTree) -> Vec<String> {
    let mut out: Vec<String> = tree
        .all()
        .iter()
        .filter(|e| !e.synthesized)
        .map(|e| {
            if e.is_dir {
                format!("{}/", e.path)
            } else {
                e.path.clone()
            }
        })
        .collect();
    out.sort();
    out
}

const PHOTOS: &[&str] = &[
    "photos/",
    "photos/.hidden",
    "photos/empty.txt",
    "photos/link",
    "photos/notes.txt",
    "photos/nothing/",
    "photos/trip/",
    "photos/trip/IMG_0001.jpg",
    "photos/trip/run.sh",
];

/// What the listing says about the parts of the fixture a listing can say.
fn assert_photos_listing(tree: &ArchiveTree) {
    assert_eq!(paths(tree), PHOTOS);
    let notes = tree.get("photos/notes.txt").unwrap();
    assert_eq!(notes.len, 8000);
    assert_eq!(notes.mtime, Some(STAMP), "the mtime kept its odd second");
    let empty = tree.get("photos/empty.txt").unwrap();
    assert_eq!((empty.len, empty.is_dir), (0, false));
    assert_eq!(empty.mtime, Some(STAMP + 2));
    let nothing = tree.get("photos/nothing").unwrap();
    assert!(nothing.is_dir && !nothing.synthesized);
    assert_eq!(nothing.mtime, Some(STAMP + 4));
    assert_eq!(tree.get("photos/trip/IMG_0001.jpg").unwrap().len, 5000);
}

/// Extract everything and compare it with the source.
fn assert_photos_extract(t: &TempTree, archive: &Path, into: &str) {
    let dest = t.dir(into);
    let tree = list(archive).unwrap();
    let plan = plan_extract(&tree, &[], &dest);
    crate::archive::extract(&plan, &TaskCtx::detached()).unwrap();
    for rel in [
        "notes.txt",
        ".hidden",
        "empty.txt",
        "trip/IMG_0001.jpg",
        "trip/run.sh",
    ] {
        assert_eq!(
            std::fs::read(dest.join("photos").join(rel)).unwrap(),
            std::fs::read(t.join("src/photos").join(rel)).unwrap(),
            "{rel}"
        );
    }
    assert!(dest.join("photos/nothing").is_dir());
}

// ── Reading what the listing does not carry ─────────────────────────────────

/// `(name, st_mode, method)` for every central-directory record, read
/// straight from the bytes. The archive has no comment, so the end record is
/// the last 22 bytes; a zip64 archive is not what these tests hand it.
fn zip_modes(bytes: &[u8]) -> Vec<(String, u32, u16)> {
    let eocd = &bytes[bytes.len() - 22..];
    assert_eq!(&eocd[..4], b"PK\x05\x06");
    let count = u16::from_le_bytes([eocd[10], eocd[11]]) as usize;
    let mut at = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        let h = &bytes[at..];
        assert_eq!(&h[..4], b"PK\x01\x02");
        let made_by = u16::from_le_bytes([h[4], h[5]]);
        assert_eq!(made_by >> 8, 3, "made on unix");
        let flags = u16::from_le_bytes([h[8], h[9]]);
        assert_eq!(flags & (1 << 3), 0, "no data descriptors");
        let method = u16::from_le_bytes([h[10], h[11]]);
        let name_len = u16::from_le_bytes([h[28], h[29]]) as usize;
        let extra_len = u16::from_le_bytes([h[30], h[31]]) as usize;
        let comment_len = u16::from_le_bytes([h[32], h[33]]) as usize;
        let external = u32::from_le_bytes([h[38], h[39], h[40], h[41]]);
        let name = String::from_utf8(h[46..46 + name_len].to_vec()).unwrap();
        assert_eq!(flags & (1 << 11), 1 << 11, "{name}: UTF-8 flag");
        out.push((name, external >> 16, method));
        at += 46 + name_len + extra_len + comment_len;
    }
    out
}

/// `(name, mode, type flag, link name)` for every tar entry, pax paths
/// applied.
fn tar_modes(bytes: &[u8]) -> Vec<(String, u32, u8, String)> {
    let octal = |field: &[u8]| {
        let text: String = field
            .iter()
            .take_while(|b| **b != 0 && **b != b' ')
            .map(|b| *b as char)
            .collect();
        u64::from_str_radix(text.trim(), 8).unwrap_or(0)
    };
    let cstr = |field: &[u8]| {
        let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
        String::from_utf8_lossy(&field[..end]).into_owned()
    };
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut long: Option<String> = None;
    while at + 512 <= bytes.len() {
        let h = &bytes[at..at + 512];
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let size = octal(&h[124..136]) as usize;
        let data = &bytes[at + 512..at + 512 + size];
        if h[156] == b'x' {
            let text = String::from_utf8_lossy(data);
            long = text
                .lines()
                .find_map(|l| l.split_once(" path=").map(|(_, p)| p.to_string()));
        } else {
            let name = long.take().unwrap_or_else(|| cstr(&h[0..100]));
            out.push((name, octal(&h[100..108]) as u32, h[156], cstr(&h[157..257])));
        }
        at += 512 + size.div_ceil(512) * 512;
    }
    out
}

/// The local header whose name is `name`, found by walking the signatures.
fn local_header<'a>(bytes: &'a [u8], name: &str) -> &'a [u8] {
    header_named(bytes, b"PK\x03\x04", 26, 30, name)
}

/// The central-directory header whose name is `name`.
fn central_header<'a>(bytes: &'a [u8], name: &str) -> &'a [u8] {
    header_named(bytes, b"PK\x01\x02", 28, 46, name)
}

fn header_named<'a>(
    bytes: &'a [u8],
    signature: &[u8],
    name_len_at: usize,
    fixed: usize,
    name: &str,
) -> &'a [u8] {
    (0..bytes.len().saturating_sub(fixed))
        .filter(|&at| &bytes[at..at + 4] == signature)
        .map(|at| &bytes[at..])
        .find(|h| {
            let n = u16::from_le_bytes([h[name_len_at], h[name_len_at + 1]]) as usize;
            h.len() >= fixed + n && &h[fixed..fixed + n] == name.as_bytes()
        })
        .unwrap_or_else(|| panic!("no header named {name}"))
}

fn mode_of<'a>(rows: impl IntoIterator<Item = (&'a str, u32)>, name: &str) -> u32 {
    rows.into_iter()
        .find(|row| row.0 == name)
        .map(|row| row.1)
        .unwrap_or_else(|| panic!("{name} is not in the archive"))
}

fn zip_mode(rows: &[(String, u32, u16)], name: &str) -> u32 {
    mode_of(rows.iter().map(|r| (r.0.as_str(), r.1)), name)
}

fn tar_mode(rows: &[(String, u32, u8, String)], name: &str) -> u32 {
    mode_of(rows.iter().map(|r| (r.0.as_str(), r.1)), name)
}

// ── The formats ─────────────────────────────────────────────────────────────

#[test]
fn a_zip_round_trips_through_the_reader() {
    let t = TempTree::new("write-zip");
    let photos = photos(&t);
    let dest = t.join("photos.zip");
    let packed = pack(vec![photos], dest.clone(), Format::Zip);
    assert_eq!(packed.items, 1);
    assert_eq!(packed.size, std::fs::metadata(&dest).unwrap().len());
    assert_eq!(packed.bytes, 8000 + 7 + 5000 + 18);

    let tree = list(&dest).unwrap();
    assert_photos_listing(&tree);
    assert_photos_extract(&t, &dest, "out");
    // The link went in as a link: its data is the target, not the file.
    assert_eq!(
        read_entry(&dest, "photos/link", 1024).unwrap(),
        Some(b"notes.txt".to_vec())
    );

    let modes = zip_modes(&std::fs::read(&dest).unwrap());
    if cfg!(unix) {
        assert_eq!(zip_mode(&modes, "photos/notes.txt"), 0o100_640);
        assert_eq!(zip_mode(&modes, "photos/trip/run.sh"), 0o100_755);
    }
    for rel in ["notes.txt", "trip/run.sh"] {
        assert_eq!(
            zip_mode(&modes, &format!("photos/{rel}")),
            mode_on_disk(&t.join("src/photos").join(rel)),
            "{rel}"
        );
    }
    assert_eq!(zip_mode(&modes, "photos/link") & 0o170_000, 0o120_000);
    assert_eq!(zip_mode(&modes, "photos/nothing/") & 0o170_000, 0o040_000);
}

#[test]
fn a_tar_round_trips_through_the_reader() {
    let t = TempTree::new("write-tar");
    let photos = photos(&t);
    let dest = t.join("photos.tar");
    pack(vec![photos], dest.clone(), Format::Tar);

    let tree = list(&dest).unwrap();
    assert_eq!(tree.format(), crate::archive::ArchiveFormat::Tar);
    assert_photos_listing(&tree);
    assert_eq!(
        tree.get("photos/link").unwrap().link_target.as_deref(),
        Some("notes.txt")
    );
    assert_photos_extract(&t, &dest, "out");

    let rows = tar_modes(&std::fs::read(&dest).unwrap());
    if cfg!(unix) {
        assert_eq!(tar_mode(&rows, "photos/notes.txt"), 0o640);
        assert_eq!(tar_mode(&rows, "photos/trip/run.sh"), 0o755);
    }
    for rel in ["notes.txt", "trip/run.sh"] {
        assert_eq!(
            tar_mode(&rows, &format!("photos/{rel}")),
            mode_on_disk(&t.join("src/photos").join(rel)) & 0o7777,
            "{rel}"
        );
    }
    let link = rows.iter().find(|r| r.0 == "photos/link").unwrap();
    assert_eq!((link.2, link.3.as_str()), (b'2', "notes.txt"));
    let dir = rows.iter().find(|r| r.0 == "photos/nothing/").unwrap();
    assert_eq!(dir.2, b'5');
    // Ends on the two zero blocks and nothing else.
    let bytes = std::fs::read(&dest).unwrap();
    assert_eq!(bytes.len() % 512, 0);
    assert!(bytes[bytes.len() - 1024..].iter().all(|b| *b == 0));
}

/// Inflate a gzip stream here, checking the header and the trailer, so the
/// round trip does not depend on a `gzip` binary.
fn gunzip(bytes: &[u8]) -> Vec<u8> {
    assert_eq!(&bytes[..4], &[0x1f, 0x8b, 8, 0], "magic, deflate, no flags");
    assert_eq!(bytes[9], 3, "OS: unix");
    let body = &bytes[10..bytes.len() - 8];
    let out = miniz_oxide::inflate::decompress_to_vec(body).unwrap();
    let trailer = &bytes[bytes.len() - 8..];
    let crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let len = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    assert_eq!(crc, crc32::crc32(&out), "CRC trailer");
    assert_eq!(len as usize, out.len(), "ISIZE trailer");
    out
}

#[test]
fn a_tar_gz_is_a_gzip_stream_around_the_same_tar() {
    let t = TempTree::new("write-tgz");
    let photos = photos(&t);
    let dest = t.join("photos.tar.gz");
    pack(vec![photos.clone()], dest.clone(), Format::TarGz);
    let plain = t.join("photos.tar");
    pack(vec![photos], plain.clone(), Format::Tar);

    let tar = gunzip(&std::fs::read(&dest).unwrap());
    assert_eq!(tar, std::fs::read(&plain).unwrap(), "the same tar inside");
    let (raws, truncated) = crate::archive::tar::list(Cursor::new(&tar)).unwrap();
    assert!(!truncated);
    assert_eq!(raws.len(), PHOTOS.len());

    if crate::archive::have("gzip") {
        let tree = list(&dest).unwrap();
        assert_eq!(tree.format(), crate::archive::ArchiveFormat::TarGz);
        assert_photos_listing(&tree);
        assert_photos_extract(&t, &dest, "out");
    }
}

#[test]
fn tar_zst_and_tar_xz_go_through_their_compressors() {
    for (format, tool, name) in [
        (Format::TarZst, "zstd", "photos.tar.zst"),
        (Format::TarXz, "xz", "photos.tar.xz"),
    ] {
        if !crate::archive::have(tool) {
            continue;
        }
        let t = TempTree::new("write-piped");
        let photos = photos(&t);
        let dest = t.join(name);
        pack(vec![photos], dest.clone(), format);
        let tree = list(&dest).unwrap();
        assert_photos_listing(&tree);
        assert_photos_extract(&t, &dest, "out");
    }
}

#[test]
fn a_missing_compressor_is_named_and_leaves_nothing_behind() {
    let t = TempTree::new("write-no-tool");
    let photos = photos(&t);
    let members = walk(&[photos], &TaskCtx::detached()).unwrap().members;
    let dest = t.join("photos.tar.zst");
    let (temp, file) = claim_temp(&dest).unwrap();
    // A program nobody has, standing in for a machine without zstd.
    let err = piped(
        "delightfile-no-such-zstd",
        Format::TarZst,
        &members,
        file,
        &dest,
        &TaskCtx::detached(),
    )
    .unwrap_err();
    let _ = std::fs::remove_file(&temp);
    assert_eq!(
        err.to_string(),
        "tar.zst needs zstd, which is not installed"
    );
}

/// A compressor that fails part-way is reported in its own words, and the
/// write that found the pipe closed does not hide them.
#[test]
fn a_compressor_that_fails_is_quoted() {
    let t = TempTree::new("write-tool-fails");
    let photos = photos(&t);
    let members = walk(&[photos], &TaskCtx::detached()).unwrap().members;
    let dest = t.join("photos.tar.zst");
    let (temp, file) = claim_temp(&dest).unwrap();
    // `false` reads nothing, says nothing and exits 1.
    let Some(fail) = on_path("false") else {
        return;
    };
    let err = piped(
        &fail.to_string_lossy(),
        Format::TarZst,
        &members,
        file,
        &dest,
        &TaskCtx::detached(),
    )
    .unwrap_err();
    let _ = std::fs::remove_file(&temp);
    assert!(err.to_string().ends_with("exited with status 1"), "{err}");
}

#[test]
fn a_7z_is_written_by_7_zip_from_the_items_own_folder() {
    let Some(seven) = on_path("7z") else {
        return;
    };
    let t = TempTree::new("write-7z");
    let photos = photos(&t);
    let solo = t.file("src/solo.txt", b"alone");
    let dest = t.join("both.7z");
    pack(vec![photos, solo], dest.clone(), Format::SevenZip);
    let out = Command::new(&seven)
        .args(["l", "-slt", "-ba"])
        .arg(&dest)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    // 7-Zip lists a member with the platform's separator: `\` on Windows.
    let names: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("Path = "))
        .map(|name| crate::path::with_slashes(name).into_owned())
        .collect();
    let has = |name: &str| names.iter().any(|n| n == name);
    assert!(has("solo.txt"), "{names:?}");
    assert!(has("photos/notes.txt"), "{names:?}");
    assert!(has("photos/.hidden"), "{names:?}");
    let tested = Command::new(&seven)
        .args(["t", "-bd"])
        .arg(&dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(tested.success(), "7z t");
    // No temporary name left beside it.
    assert!(leftovers(t.path()).is_empty(), "{:?}", leftovers(t.path()));
}

// ── Layout ──────────────────────────────────────────────────────────────────

#[test]
fn a_file_goes_at_the_root_and_several_items_side_by_side() {
    let t = TempTree::new("write-layout");
    let report = t.file("src/report.pdf", b"%PDF-1.7 pretend");
    let dest = t.join("report.zip");
    pack(vec![report.clone()], dest.clone(), Format::Zip);
    assert_eq!(paths(&list(&dest).unwrap()), ["report.pdf"]);

    let photos = photos(&t);
    let both = t.join("both.tar");
    pack(vec![photos, report], both.clone(), Format::Tar);
    let listed = paths(&list(&both).unwrap());
    assert!(listed.contains(&"report.pdf".to_string()), "{listed:?}");
    assert!(
        listed.contains(&"photos/notes.txt".to_string()),
        "{listed:?}"
    );
    assert!(listed.iter().all(|p| !p.starts_with("src")), "{listed:?}");
}

#[test]
fn long_names_go_through_pax_and_come_back_whole() {
    let t = TempTree::new("write-long");
    let deep = format!("{}/{}", "d".repeat(80), "n".repeat(120));
    t.file(format!("src/top/{deep}.txt"), b"deep");
    let dest = t.join("top.tar");
    pack(vec![t.join("src/top")], dest.clone(), Format::Tar);
    let tree = list(&dest).unwrap();
    let want = format!("top/{deep}.txt");
    assert!(tree.get(&want).is_some(), "{:?}", paths(&tree));
    assert_eq!(tree.get(&want).unwrap().len, 4);
}

/// A name that is not ASCII goes into a zip and a tar as its UTF-8, on every
/// target — on Windows by way of the UTF-16 name — with the zip's UTF-8 flag
/// set, and comes back the same through the reader and through an extract.
#[test]
fn a_unicode_member_name_goes_in_and_comes_back_as_its_utf8() {
    let name = "ünïcödé — 日本語 🎬";
    for format in [Format::Zip, Format::Tar] {
        let t = TempTree::new("write-unicode");
        t.file(format!("src/{name}/{name}.txt"), b"hello");
        let dest = t.join(format!("unicode.{}", format.label()));
        pack(vec![t.join(format!("src/{name}"))], dest.clone(), format);
        let want = format!("{name}/{name}.txt");
        let tree = list(&dest).unwrap();
        assert_eq!(tree.get(&want).map(|e| e.len), Some(5), "{format:?}");
        let bytes = std::fs::read(&dest).unwrap();
        assert!(
            bytes.windows(want.len()).any(|w| w == want.as_bytes()),
            "{format:?}: the name is written as its UTF-8"
        );
        if format == Format::Zip {
            // `zip_modes` asserts the UTF-8 flag on every record.
            assert_eq!(zip_modes(&bytes).len(), 2);
        }
        let out = t.dir("out");
        let plan = plan_extract(&tree, &[], &out);
        crate::archive::extract(&plan, &TaskCtx::detached()).unwrap();
        assert_eq!(std::fs::read(out.join(&want)).unwrap(), b"hello");
    }
}

/// The format readers' door into a zip, over what this writer wrote: a stored
/// member and a deflated one come back whole by name.
#[test]
fn a_written_member_is_read_back_by_name() {
    let t = TempTree::new("write-member");
    let jpg = (0..20_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let txt = b"the quick brown fox ".repeat(1_000);
    t.file("src/pics/photo.jpg", &jpg);
    t.file("src/pics/notes.txt", &txt);
    let dest = t.join("pics.zip");
    pack(vec![t.join("src/pics")], dest.clone(), Format::Zip);

    let bytes = std::fs::read(&dest).unwrap();
    let len = bytes.len() as u64;
    let read = |name: &str| {
        crate::archive::zip::read_member(&mut Cursor::new(&bytes), len, name, 1 << 20).unwrap()
    };
    assert_eq!(read("pics/photo.jpg"), Some(jpg), "the stored one");
    assert_eq!(read("pics/notes.txt"), Some(txt), "the deflated one");
    assert_eq!(read("pics/missing.txt"), None);
}

// ── Stored or deflated ──────────────────────────────────────────────────────

#[test]
fn a_jpg_is_stored_and_a_txt_is_deflated() {
    let t = TempTree::new("write-methods");
    t.file("src/pics/photo.jpg", &[7u8; 20_000]);
    t.file("src/pics/notes.txt", &[7u8; 20_000]);
    // A dot in a folder's name is not the extension of what is inside it.
    t.file("src/pics/trip.mp4/notes", &[7u8; 20_000]);
    let dest = t.join("pics.zip");
    pack(vec![t.join("src/pics")], dest.clone(), Format::Zip);
    let tree = list(&dest).unwrap();
    assert_eq!(
        tree.get("pics/trip.mp4/notes").unwrap().method,
        Method::Deflate
    );
    let jpg = tree.get("pics/photo.jpg").unwrap();
    let txt = tree.get("pics/notes.txt").unwrap();
    assert_eq!(jpg.method, Method::Store);
    assert_eq!(jpg.compressed, 20_000);
    assert_eq!(txt.method, Method::Deflate);
    assert!(txt.compressed < 1_000, "{} bytes", txt.compressed);
    assert_photos_like_contents(&t, &dest);
}

fn assert_photos_like_contents(t: &TempTree, archive: &Path) {
    let dest = t.dir("methods-out");
    let tree = list(archive).unwrap();
    let plan = plan_extract(&tree, &[], &dest);
    crate::archive::extract(&plan, &TaskCtx::detached()).unwrap();
    assert_eq!(
        std::fs::read(dest.join("pics/notes.txt")).unwrap(),
        [7u8; 20_000]
    );
    assert_eq!(
        std::fs::read(dest.join("pics/photo.jpg")).unwrap(),
        [7u8; 20_000]
    );
}

/// Several chunks' worth of data that does not repeat, so the deflate loop
/// goes round with a full output buffer and the CRC sees chunk boundaries.
#[test]
fn a_member_bigger_than_a_chunk_survives_deflate() {
    let t = TempTree::new("write-big");
    let mut state = 0x1234_5678u32;
    let data: Vec<u8> = (0..(crate::ops::COPY_CHUNK * 2 + 777))
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            // Half noise, half text, so deflate has something to do and
            // something it cannot do.
            if (i / 4096) % 2 == 0 {
                state as u8
            } else {
                b"abcdefgh"[i % 8]
            }
        })
        .collect();
    t.file("src/big/blob.bin", &data);
    for format in [Format::Zip, Format::TarGz] {
        let dest = t.join(format!("big.{}", format.label()));
        pack(vec![t.join("src/big")], dest.clone(), format);
        if format == Format::Zip {
            assert_eq!(
                read_entry(&dest, "big/blob.bin", data.len()).unwrap(),
                Some(data.clone())
            );
        } else {
            // `big/`'s header, `big/blob.bin`'s, then the data.
            let tar = gunzip(&std::fs::read(&dest).unwrap());
            assert!(tar[1024..1024 + data.len()] == data[..]);
        }
    }
}

// ── Zip64 ───────────────────────────────────────────────────────────────────

/// With the limits lowered to a thousand bytes and three entries, a small
/// archive takes every zip64 road — the per-member extra for sizes and
/// offsets, the reserved local extra, and the zip64 end records — and the
/// reader still lists it right.
#[test]
fn zip64_is_written_when_the_limits_are_passed() {
    let t = TempTree::new("write-zip64");
    t.file("src/z/big.txt", &b"zip64 ".repeat(1000));
    t.file("src/z/big.jpg", &[9u8; 3000]);
    // Past half the limit and short of it: the 2–4 GiB case, where the local
    // header reserves a zip64 extra the sizes turn out not to need.
    t.file("src/z/mid.jpg", &[8u8; 600]);
    t.file("src/z/small.txt", b"small");
    t.dir("src/z/dir");
    let members = walk(&[t.join("src/z")], &TaskCtx::detached())
        .unwrap()
        .members;
    let dest = t.join("z.zip");
    let file = std::fs::File::create(&dest).unwrap();
    let limits = zip::Limits {
        wide: 1000,
        count: 3,
    };
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file), &dest, limits);
    for member in &members {
        writer.add(member, &TaskCtx::detached()).unwrap();
    }
    let (out, read) = writer.finish().unwrap();
    out.into_inner().unwrap();
    assert_eq!(read, 6000 + 3000 + 600 + 5);

    let bytes = std::fs::read(&dest).unwrap();
    // Both headers of one member agree: the one whose local header reserved
    // a zip64 extra says 4.5 in its central entry too and carries the extra
    // there, with the sizes behind the mark.
    let local = local_header(&bytes, "z/mid.jpg");
    assert_eq!(u16::from_le_bytes([local[4], local[5]]), 45, "local");
    let central = central_header(&bytes, "z/mid.jpg");
    assert_eq!(u16::from_le_bytes([central[6], central[7]]), 45, "central");
    assert_eq!(&central[20..28], &[0xff; 8], "sizes behind the zip64 mark");
    // A directory needs 2.0 (APPNOTE 4.4.3.2) — the first one, whose offset
    // is short of the lowered limit; `z/dir/` is past it, and rightly 4.5.
    let dir = central_header(&bytes, "z/");
    assert_eq!(u16::from_le_bytes([dir[6], dir[7]]), 20, "a directory");
    let far = central_header(&bytes, "z/dir/");
    assert_eq!(
        u16::from_le_bytes([far[6], far[7]]),
        45,
        "an offset past it"
    );
    let small = local_header(&bytes, "z/small.txt");
    assert_eq!(u16::from_le_bytes([small[4], small[5]]), 20, "deflated");
    assert!(
        bytes.windows(4).any(|w| w == b"PK\x06\x06"),
        "a zip64 end record"
    );
    assert!(
        bytes.windows(4).any(|w| w == b"PK\x06\x07"),
        "a zip64 locator"
    );
    let eocd = &bytes[bytes.len() - 22..];
    assert_eq!(&eocd[8..10], &[0xff, 0xff], "the count is saturated");

    let tree = list(&dest).unwrap();
    assert_eq!(
        paths(&tree),
        [
            "z/",
            "z/big.jpg",
            "z/big.txt",
            "z/dir/",
            "z/mid.jpg",
            "z/small.txt"
        ]
    );
    assert_eq!(tree.get("z/mid.jpg").unwrap().len, 600);
    assert_eq!(
        read_entry(&dest, "z/mid.jpg", 1000).unwrap(),
        Some(vec![8u8; 600])
    );
    assert_eq!(tree.get("z/big.txt").unwrap().len, 6000);
    assert_eq!(tree.get("z/big.jpg").unwrap().len, 3000);
    assert_eq!(tree.get("z/big.jpg").unwrap().compressed, 3000);
    assert_eq!(
        read_entry(&dest, "z/big.txt", 10_000).unwrap(),
        Some(b"zip64 ".repeat(1000))
    );
    assert_eq!(
        read_entry(&dest, "z/small.txt", 100).unwrap(),
        Some(b"small".to_vec())
    );
    if on_path("unzip").is_some() {
        assert!(
            tool_accepts("unzip", &["-tqq"], &dest),
            "unzip -t rejects the zip64 archive"
        );
    }
}

// ── Never half an archive ───────────────────────────────────────────────────

/// Cancels the task the moment `after` bytes have gone by.
struct CancelAfter {
    flags: Arc<TaskFlags>,
    after: u64,
    seen: AtomicU64,
}

impl ProgressSink for CancelAfter {
    fn set_total(&self, _bytes: u64, _files: u64) {}
    fn advance(&self, bytes: u64, _files: u64) {
        let total = self.seen.fetch_add(bytes, Ordering::SeqCst) + bytes;
        if total >= self.after {
            self.flags.cancel();
        }
    }
}

/// `.df-tmp-…` names under `dir`.
fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".df-tmp-"))
        .collect()
}

#[test]
fn a_cancel_leaves_no_archive_no_temp_file_and_no_new_folders() {
    let t = TempTree::new("write-cancel");
    t.file("src/big/a.bin", &vec![1u8; crate::ops::COPY_CHUNK * 3]);
    t.file("src/big/b.bin", &vec![2u8; crate::ops::COPY_CHUNK * 3]);
    for format in [Format::Zip, Format::Tar, Format::TarGz, Format::TarZst] {
        if format == Format::TarZst && !crate::archive::have("zstd") {
            continue;
        }
        let flags = Arc::new(TaskFlags::new());
        let sink = Arc::new(CancelAfter {
            flags: Arc::clone(&flags),
            after: crate::ops::COPY_CHUNK as u64 * 2,
            seen: AtomicU64::new(0),
        });
        let ctx = TaskCtx::with_sink(flags, sink);
        let dest = t.join("made/for/it/big.archive");
        let err = Pack {
            sources: vec![t.join("src/big")],
            dest: dest.clone(),
            format,
            overwrite: false,
        }
        .run(&ctx)
        .unwrap_err();
        assert!(matches!(err, DfError::Cancelled), "{format:?}: {err}");
        assert!(!crate::ops::exists(&dest), "{format:?}");
        assert!(
            !crate::ops::exists(&t.join("made")),
            "{format:?}: the folders stayed"
        );
        assert!(leftovers(t.path()).is_empty(), "{format:?}");
    }
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_fails_the_whole_archive() {
    use std::os::unix::fs::PermissionsExt;
    let t = TempTree::new("write-unreadable");
    t.file("src/box/fine.txt", b"fine");
    let locked = t.file("src/box/locked.txt", b"secret");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&locked).is_ok() {
        // Running as root: nothing is unreadable, and there is nothing to test.
        return;
    }
    let dest = t.join("box.zip");
    let err = Pack {
        sources: vec![t.join("src/box")],
        dest: dest.clone(),
        format: Format::Zip,
        overwrite: false,
    }
    .run(&TaskCtx::detached())
    .unwrap_err();
    assert!(err.to_string().contains("locked.txt"), "{err}");
    assert!(!crate::ops::exists(&dest));
    assert!(leftovers(t.path()).is_empty());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_name_taken_meanwhile_is_only_replaced_when_asked() {
    let t = TempTree::new("write-replace");
    let src = t.file("src/a.txt", b"new");
    let dest = t.file("a.zip", b"somebody else's");
    let mut job = Pack {
        sources: vec![src],
        dest: dest.clone(),
        format: Format::Zip,
        overwrite: false,
    };
    assert!(job.run(&TaskCtx::detached()).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"somebody else's");
    assert!(leftovers(t.path()).is_empty());
    job.overwrite = true;
    job.run(&TaskCtx::detached()).unwrap();
    assert_eq!(paths(&list(&dest).unwrap()), ["a.txt"]);
}

/// A file that shrank between the walk and the write is counted as what was
/// read, and declared as that too.
#[test]
fn the_bytes_counted_are_the_bytes_read() {
    let t = TempTree::new("write-shrank");
    let log = t.file("src/s/log.txt", &[b'x'; 5000]);
    let members = walk(&[t.join("src/s")], &TaskCtx::detached())
        .unwrap()
        .members;
    std::fs::write(&log, [b'y'; 1200]).unwrap();
    let dest = t.join("s.zip");
    let file = std::fs::File::create(&dest).unwrap();
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file), &dest, zip::LIMITS);
    for member in &members {
        writer.add(member, &TaskCtx::detached()).unwrap();
    }
    let (out, read) = writer.finish().unwrap();
    out.into_inner().unwrap();
    assert_eq!(read, 1200);
    assert_eq!(list(&dest).unwrap().get("s/log.txt").unwrap().len, 1200);
}

/// A name the 16-bit length field cannot hold is an error, not a length
/// that wrapped round.
#[test]
fn a_name_too_long_for_a_zip_is_refused() {
    let dest = PathBuf::from("/nowhere/long.zip");
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()), &dest, zip::LIMITS);
    let member = Member {
        source: PathBuf::from("/nowhere/long"),
        name: vec![b'n'; 70_000],
        what: What::Dir,
        mode: 0o040_755,
        mtime: STAMP,
        uid: 0,
        gid: 0,
        stored: true,
    };
    let err = writer.add(&member, &TaskCtx::detached()).unwrap_err();
    assert!(err.to_string().contains("too long"), "{err}");
}

/// Past 2038 the extended timestamp is still written — its low 32 bits,
/// which is how the readers that know 2038 read it — and this crate's own
/// reader, told by the DOS date which half it is in, gets the year right.
#[test]
fn a_time_past_2038_keeps_its_extended_timestamp() {
    let t = TempTree::new("write-2038");
    // 2039-09-18.
    let later = 2_200_000_000i64;
    let file = t.file("src/future.txt", b"soon");
    set_mtime(&file, later);
    let dest = t.join("future.zip");
    pack(vec![file], dest.clone(), Format::Zip);
    let bytes = std::fs::read(&dest).unwrap();
    let central = central_header(&bytes, "future.txt");
    let extra = &central[46 + "future.txt".len()..];
    assert_eq!(&extra[..5], &[0x55, 0x54, 5, 0, 1]);
    assert_eq!(&extra[5..9], &(later as u32).to_le_bytes());
    assert_eq!(
        list(&dest).unwrap().get("future.txt").unwrap().mtime,
        Some(later)
    );
}

// ── The checks before anything runs ─────────────────────────────────────────

/// `path`, spelled relative to the process's own directory.
fn relative_to_cwd(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let common = cwd
        .components()
        .zip(path.components())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = PathBuf::new();
    for _ in cwd.components().skip(common) {
        out.push("..");
    }
    for part in path.components().skip(common) {
        out.push(part);
    }
    out
}

/// A relative destination is where it says relative to the process — not
/// relative to the sources' folder, which is where 7-Zip runs — and no
/// temporary file is left in either place.
#[test]
fn a_relative_destination_lands_where_it_says() {
    let t = TempTree::new("write-relative");
    let cwd = std::env::current_dir().unwrap();
    if crate::path::root_of(&cwd) != crate::path::root_of(t.path()) {
        // A Windows runner builds on one drive and keeps its temp directory
        // on another: no relative path reaches from the one to the other.
        eprintln!("skipping: the temp directory is on another drive");
        return;
    }
    let photos = photos(&t);
    let mut formats = vec![Format::Zip, Format::Tar];
    if on_path("7z").is_some() {
        formats.push(Format::SevenZip);
    }
    for format in formats {
        let dest = t.join(format!("rel.{}", format.label()));
        let relative = relative_to_cwd(&dest);
        assert!(relative.is_relative());
        let packed = pack(vec![relative_to_cwd(&photos)], relative, format);
        assert!(packed.path.is_absolute(), "{format:?}");
        assert_eq!(
            std::fs::canonicalize(&packed.path).unwrap(),
            std::fs::canonicalize(&dest).unwrap(),
            "{format:?}"
        );
        assert!(
            leftovers(&t.join("src")).is_empty(),
            "{format:?}: a temporary file in the sources' folder"
        );
        assert!(leftovers(t.path()).is_empty(), "{format:?}");
        assert!(
            names_of(&dest, format).contains(&"photos/notes.txt".to_string()),
            "{format:?}"
        );
    }
}

/// The names in an archive, through the reader — or through 7-Zip, for the
/// one format the reader does not list.
fn names_of(archive: &Path, format: Format) -> Vec<String> {
    if format != Format::SevenZip {
        return paths(&list(archive).unwrap());
    }
    let out = Command::new("7z")
        .args(["l", "-slt", "-ba"])
        .arg(archive)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("Path = "))
        .map(|name| crate::path::with_slashes(name).into_owned())
        .collect()
}

#[test]
fn two_items_with_one_name_are_refused() {
    let t = TempTree::new("write-twins");
    let a = t.file("a/box", b"a");
    let b = t.dir("b/box");
    for format in Format::ALL {
        let job = Pack {
            sources: vec![a.clone(), b.clone()],
            dest: t.join("twins.archive"),
            format,
            overwrite: false,
        };
        let err = job.check().unwrap_err();
        assert!(err.contains("called box"), "{format:?}: {err}");
        let err = job.run(&TaskCtx::detached()).unwrap_err();
        assert!(err.to_string().contains("called box"), "{format:?}: {err}");
        assert!(!crate::ops::exists(&t.join("twins.archive")));
    }
}

#[test]
fn an_archive_inside_what_it_archives_is_refused() {
    let t = TempTree::new("write-itself");
    let photos = photos(&t);
    let job = |dest: PathBuf, format| Pack {
        sources: vec![photos.clone()],
        dest,
        format,
        overwrite: false,
    };
    let err = job(photos.join("photos.zip"), Format::Zip)
        .check()
        .unwrap_err();
    assert!(err.contains("inside photos"), "{err}");
    assert!(job(photos.join("trip/x.tar"), Format::Tar).check().is_err());
    // Beside it is fine.
    assert!(job(t.join("src/photos.zip"), Format::Zip).check().is_ok());
    // …and a folder's name is not an archive's.
    assert!(job(t.join("src"), Format::Zip)
        .check()
        .unwrap_err()
        .contains("is a folder"));
}

#[test]
fn seven_zip_wants_one_folder() {
    let t = TempTree::new("write-7z-folders");
    let a = t.file("one/a.txt", b"a");
    let b = t.file("two/b.txt", b"b");
    let job = Pack {
        sources: vec![a.clone(), b.clone()],
        dest: t.join("x.7z"),
        format: Format::SevenZip,
        overwrite: false,
    };
    assert_eq!(
        job.check().unwrap_err(),
        "7z takes items from one folder at a time"
    );
    // The in-house formats do not care where the items came from.
    let zip = Pack {
        format: Format::Zip,
        dest: t.join("x.zip"),
        ..job
    };
    assert!(zip.check().is_ok());
}

#[test]
fn a_typed_name_chooses_the_format() {
    let archive = |name: &str, format| Named::Archive {
        name: name.to_string(),
        format,
    };
    assert_eq!(named("photos"), archive("photos.zip", Format::Zip));
    assert_eq!(named("photos.zip"), archive("photos.zip", Format::Zip));
    assert_eq!(named("  Photos.ZIP "), archive("Photos.ZIP", Format::Zip));
    assert_eq!(named("photos.tar"), archive("photos.tar", Format::Tar));
    assert_eq!(
        named("photos.tar.gz"),
        archive("photos.tar.gz", Format::TarGz)
    );
    assert_eq!(named("photos.tgz"), archive("photos.tgz", Format::TarGz));
    assert_eq!(
        named("photos.tar.zst"),
        archive("photos.tar.zst", Format::TarZst)
    );
    assert_eq!(
        named("photos.tar.xz"),
        archive("photos.tar.xz", Format::TarXz)
    );
    assert_eq!(named("photos.7z"), archive("photos.7z", Format::SevenZip));
    assert_eq!(named("report.pdf"), archive("report.pdf.zip", Format::Zip));
    assert_eq!(named("v1.2"), archive("v1.2.zip", Format::Zip));
    assert_eq!(named("out/photos"), archive("out/photos.zip", Format::Zip));
    assert_eq!(
        named("out/photos.tar"),
        archive("out/photos.tar", Format::Tar)
    );
    assert_eq!(named("photos.rar"), Named::Unwritable("rar".to_string()));
    assert_eq!(
        named("photos.tar.bz2"),
        Named::Unwritable("tar.bz2".to_string())
    );
    assert_eq!(named("notes.gz"), Named::Unwritable("gz".to_string()));
    for empty in ["", "   ", ".zip", "out/", "out/.tar.gz", ".rar"] {
        assert_eq!(named(empty), Named::Empty, "{empty:?}");
    }
    // The platform's own separator marks the leaf too: `\` on Windows. On
    // Unix it is part of a name, so `.zip` there goes after the whole text.
    if cfg!(windows) {
        assert_eq!(
            named(r"out\photos"),
            archive(r"out\photos.zip", Format::Zip)
        );
        assert_eq!(named(r"out\.tar.gz"), Named::Empty);
        assert_eq!(named(r"out\.zip"), Named::Empty);
    } else {
        assert_eq!(named(r"out\.zip"), archive(r"out\.zip", Format::Zip));
    }
    // The extension alone, for a hint that lights up before there is a name.
    assert_eq!(extension(".tar.gz"), Extension::Writes(Format::TarGz));
    assert_eq!(extension("x.7Z"), Extension::Writes(Format::SevenZip));
    assert_eq!(extension("photos"), Extension::Bare);
    assert_eq!(
        extension("photos.RAR"),
        Extension::Unwritable("rar".to_string())
    );
}

#[test]
fn the_task_is_named_for_the_panel() {
    let job = |n: usize| Pack {
        sources: (0..n).map(|i| PathBuf::from(format!("/x/{i}"))).collect(),
        dest: PathBuf::from("/x/photos.zip"),
        format: Format::Zip,
        overwrite: false,
    };
    assert_eq!(job(1).name(), "Archive 1 item → photos.zip");
    assert_eq!(job(12).name(), "Archive 12 items → photos.zip");
    assert_eq!(job(1200).name(), "Archive 1,200 items → photos.zip");
}

// ── The real tools, where the machine has them ──────────────────────────────

fn tool_accepts(tool: impl AsRef<std::ffi::OsStr>, args: &[&str], archive: &Path) -> bool {
    Command::new(tool)
        .args(args)
        .arg(archive)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn the_real_tools_accept_what_is_written() {
    let t = TempTree::new("write-tools");
    let photos = photos(&t);
    // A name that is not ASCII, to see the UTF-8 flag believed.
    t.file("src/photos/ünïcödé — 日本語.txt", b"names");

    let zip = t.join("photos.zip");
    pack(vec![photos.clone()], zip.clone(), Format::Zip);
    let unicode = "photos/ünïcödé — 日本語.txt";
    if on_path("unzip").is_some() {
        assert!(tool_accepts("unzip", &["-tqq"], &zip), "unzip -t");
        let out = Command::new("unzip").arg("-Z1").arg(&zip).output().unwrap();
        let names = String::from_utf8_lossy(&out.stdout);
        if !names.contains(unicode) {
            // Apple's unzip prints every byte from 0x80 to 0x9F as `?` — a
            // control character in Latin-1, and in UTF-8 the middle of most
            // characters past U+07FF — so its listing cannot show this name
            // whatever the archive holds. Its listing is checked for exactly
            // that, and the name itself is asked of libarchive, which reads
            // the UTF-8 flag.
            let filtered: Vec<u8> = unicode
                .bytes()
                .map(|b| if (0x80..=0x9F).contains(&b) { b'?' } else { b })
                .collect();
            assert!(
                names.contains(&*String::from_utf8_lossy(&filtered)),
                "{names}"
            );
            assert!(
                on_path("bsdtar").is_some(),
                "an unzip that hides names needs bsdtar beside it to check them"
            );
            let out = Command::new("bsdtar")
                .env("LC_ALL", "en_US.UTF-8")
                .arg("-tf")
                .arg(&zip)
                .output()
                .unwrap();
            let names = String::from_utf8_lossy(&out.stdout);
            // macOS's libarchive hands names out decomposed (NFD), the form
            // HFS+ stored them in: the same name, other bytes.
            let decomposed = "photos/u\u{308}ni\u{308}co\u{308}de\u{301} — 日本語.txt";
            assert!(
                names.contains(unicode) || names.contains(decomposed),
                "{names}"
            );
        }
    }
    let tgz = t.join("photos.tar.gz");
    pack(vec![photos.clone()], tgz.clone(), Format::TarGz);
    let tar = t.join("photos.tar");
    pack(vec![photos.clone()], tar.clone(), Format::Tar);
    if on_path("tar").is_some() {
        assert!(tool_accepts("tar", &["-tzf"], &tgz), "tar -tzf");
        assert!(tool_accepts("tar", &["-tf"], &tar), "tar -tf");
        let out = Command::new("tar").arg("-tvf").arg(&tar).output().unwrap();
        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(listing.contains("photos/link -> notes.txt"), "{listing}");
        // The mode `photos` gave notes.txt, which only Unix can give.
        if cfg!(unix) {
            assert!(listing.contains("-rw-r-----"), "{listing}");
        }
    }
    if on_path("gzip").is_some() {
        assert!(tool_accepts("gzip", &["-t"], &tgz), "gzip -t");
    }
    if on_path("7z").is_some() {
        assert!(tool_accepts("7z", &["t"], &zip), "7z t on the zip");
        assert!(tool_accepts("7z", &["t"], &tgz), "7z t on the tar.gz");
    }
    // The program `on_path` found, by its own name: on Windows bsdtar is
    // `tar.exe` — which lists a zip's UTF-8 names in the ANSI code page and
    // fails on one it has no characters for (日本語), a fact about the
    // console rather than the archive; 7-Zip above has read the same zip.
    if let Some(bsdtar) = on_path("bsdtar").filter(|_| !cfg!(windows)) {
        assert!(tool_accepts(&bsdtar, &["-tf"], &zip), "bsdtar on the zip");
    }
}
