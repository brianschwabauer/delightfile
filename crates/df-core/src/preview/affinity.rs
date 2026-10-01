//! Affinity documents — Designer, Photo, Publisher, and the one `.af` the
//! single Affinity app saves — and the picture each carries of itself
//! (`plans/other-platforms/04-windows.md` W4.45; Linux too, Brian asked).
//!
//! An Affinity file is Serif's own container, which nothing outside Affinity
//! reads; what the preview pane can honestly show is the thumbnail Affinity
//! writes into every save, a PNG of the whole document no more than 512
//! pixels on a side. Without it the file is a hexdump, which says nothing.
//!
//! ## Where the thumbnail is
//!
//! Read off Brian's own files, `.afdesign` from Designer 1 and `.af` from
//! Affinity 3 alike (2026-10-01): the file begins `00 FF 4B 41` ([`MAGIC`]),
//! and the little-endian `u64` at offset 24 ([`THUMB_POINTER`]) is where a
//! record begins that reads `FF FF FF FF` `Thmb`, a `u32` version, a `u32`
//! size, a `u64` header length (29) and a `u32` payload length, then a flag
//! byte, then the PNG — the payload — whole. So the thumbnail is found
//! without reading the document, which for a 2 GB photo matters.
//!
//! **The thumbnail is not the largest PNG in the file.** A document keeps
//! the raster pictures placed in it as PNGs too, and those can be bigger: a
//! book cover's background painting at 1023×1537 beside a 320×512 thumbnail
//! of the cover with its title, one logo of a sheet of seven at 2733×570
//! beside the sheet. So the record is asked first; only where there is no
//! record to follow is the first [`SCAN_BYTES`] of the file scanned, for the
//! record and failing that for the largest well-formed PNG, which is the
//! best a file of an unknown layout offers.
//!
//! ## Its kind
//!
//! The signature is the sniff's ([`super::sniff`]): an Affinity file is
//! `application/x-affinity` whatever it is called, and the extensions say the
//! same before it is opened ([`crate::fs::mime`]). [`super::kind`] makes it
//! [`super::PreviewKind::Affinity`], whose picture the thumbnail is; a file
//! with no thumbnail is described rather than dumped
//! ([`super::Preview::Card`]).

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

/// An Affinity file's first four bytes.
pub const MAGIC: &[u8] = b"\x00\xffKA";

/// The type the sniff and the extensions give one.
pub const MIME: &str = "application/x-affinity";

/// The extensions Affinity saves under: Designer, Photo and Publisher, the
/// unified app's `.af`, and the three apps' templates.
pub const EXTENSIONS: &[&str] = &["afdesign", "afphoto", "afpub", "af", "aftemplate"];

/// Where the header keeps the thumbnail record's offset.
pub const THUMB_POINTER: usize = 24;

/// The record's own first eight bytes.
const RECORD: &[u8] = b"\xff\xff\xff\xffThmb";

/// The record's fixed part, up to its payload.
const RECORD_HEAD: usize = 29;

/// How much of a file is scanned when the header does not lead to a record.
pub const SCAN_BYTES: usize = 16 * 1024 * 1024;

/// The most a thumbnail may be: a PNG of 512 pixels a side is a few hundred
/// kilobytes, and a payload claiming more than this is not one.
const MOST: u64 = 32 * 1024 * 1024;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// What a person calls a file of this name: "Affinity Designer document" for
/// `.afdesign`, and so on; "Affinity document" for the unified app's `.af`
/// and anything else.
pub fn what(name: &str) -> &'static str {
    let ext = crate::fs::kind::extension_of(name)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "afdesign" => "Affinity Designer document",
        "afphoto" => "Affinity Photo document",
        "afpub" => "Affinity Publisher document",
        "aftemplate" => "Affinity template",
        _ => "Affinity document",
    }
}

/// The thumbnail's PNG, read from `path`: the record the header points at,
/// else what a scan of the file's first [`SCAN_BYTES`] finds. `Ok(None)` for
/// a file with no thumbnail in it.
pub fn thumbnail(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if let Some(png) = pointed(&mut file, len)? {
        return Ok(Some(png));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut head = Vec::new();
    file.take(SCAN_BYTES as u64).read_to_end(&mut head)?;
    Ok(scan(&head).map(|range| head[range].to_vec()))
}

/// The PNG the header's pointer leads to, when it leads to a record with a
/// well-formed PNG in it.
fn pointed(file: &mut std::fs::File, len: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; THUMB_POINTER + 8];
    if len < header.len() as u64 {
        return Ok(None);
    }
    file.read_exact(&mut header)?;
    if !header.starts_with(MAGIC) {
        return Ok(None);
    }
    let at = u64::from_le_bytes(header[THUMB_POINTER..].try_into().unwrap_or_default());
    if at.saturating_add(RECORD_HEAD as u64) > len {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(at))?;
    let mut head = [0u8; RECORD_HEAD];
    file.read_exact(&mut head)?;
    let Some((start, size)) = record(&head) else {
        return Ok(None);
    };
    let size = size as u64;
    if size > MOST || at + start as u64 + size > len {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(at + start as u64))?;
    let mut png = vec![0u8; size as usize];
    file.read_exact(&mut png)?;
    Ok(well_formed(&png)
        .filter(|end| *end == png.len())
        .map(|_| png))
}

/// A record's payload: where it starts from the record's first byte, and
/// how long it is. `None` for bytes that are not a record.
fn record(head: &[u8]) -> Option<(usize, usize)> {
    if head.len() < RECORD_HEAD || !head.starts_with(RECORD) {
        return None;
    }
    let start = u64::from_le_bytes(head[16..24].try_into().ok()?);
    let size = u32::from_le_bytes(head[24..28].try_into().ok()?);
    let start = usize::try_from(start).ok().filter(|s| *s >= RECORD_HEAD)?;
    Some((start, size as usize))
}

/// The thumbnail in `bytes`, a file's first bytes or all of them: the
/// record's PNG where there is a well-formed one (the last, should there be
/// more), else the largest well-formed PNG, by its pixels; `None` when there
/// is neither.
pub fn scan(bytes: &[u8]) -> Option<Range<usize>> {
    find(bytes, RECORD)
        .into_iter()
        .rev()
        .find_map(|at| {
            let (start, size) = record(&bytes[at..])?;
            let png = at.checked_add(start)?;
            let end = png.checked_add(size)?;
            let found = bytes.get(png..end)?;
            (well_formed(found)? == size).then_some(png..end)
        })
        .or_else(|| {
            find(bytes, PNG)
                .into_iter()
                .filter_map(|at| {
                    let len = well_formed(&bytes[at..])?;
                    Some((pixels(&bytes[at..])?, at..at + len))
                })
                .max_by_key(|(pixels, range)| (*pixels, range.start))
                .map(|(_, range)| range)
        })
}

/// Every offset `needle` starts at in `bytes`.
fn find(bytes: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        found.push(from + at);
        from += at + 1;
    }
    found
}

/// The length of the PNG at the start of `bytes` — signature, then chunks
/// each whole, the first `IHDR`, through `IEND` — or `None` when it is not
/// one.
fn well_formed(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(PNG) {
        return None;
    }
    let mut at = PNG.len();
    let mut first = true;
    loop {
        let length = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(at + 4..at + 8)?;
        if !kind.iter().all(u8::is_ascii_alphabetic) || (first && kind != b"IHDR") {
            return None;
        }
        let next = at.checked_add(12)?.checked_add(length)?;
        if next > bytes.len() {
            return None;
        }
        if kind == b"IEND" {
            return Some(next);
        }
        first = false;
        at = next;
    }
}

/// How many pixels the PNG at the start of `bytes` says it has, from its
/// `IHDR`.
fn pixels(bytes: &[u8]) -> Option<u64> {
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some(u64::from(width) * u64::from(height))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG of `width` × `height` that is well formed as chunks go: the
    /// signature, an `IHDR` with the size, an `IDAT` of filler, `IEND`. The
    /// scan reads the chunk structure, not the pixels.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = PNG.to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0, 0, 0, 0]);
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &vec![0x55; (width * height) as usize % 97 + 3]);
        chunk(b"IEND", &[]);
        out
    }

    /// Bytes that are none of the above, the same every run.
    pub(crate) fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 7) as u8
            })
            .collect()
    }

    /// A thumbnail record around `png`, as Affinity writes one.
    fn thumb_record(png: &[u8]) -> Vec<u8> {
        let mut out = RECORD.to_vec();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32 + 13).to_le_bytes());
        out.extend_from_slice(&(RECORD_HEAD as u64).to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.push(1);
        out.extend_from_slice(png);
        out
    }

    /// An Affinity file: the header, noise, a big placed picture, noise, the
    /// thumbnail's record, a little more; the header pointing at the record
    /// unless `point` is false.
    pub(crate) fn affinity(point: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let placed = png(2733, 570);
        let thumb = png(512, 300);
        let mut file = MAGIC.to_vec();
        file.extend_from_slice(b"\x0c\x00\x04\x03nsrP#Inf");
        file.extend_from_slice(&[0; 16]);
        file.extend(noise(5000, 1));
        file.extend_from_slice(&placed);
        file.extend(noise(3000, 2));
        let at = file.len() as u64;
        file.extend(thumb_record(&thumb));
        file.extend(noise(180, 3));
        if point {
            file[THUMB_POINTER..THUMB_POINTER + 8].copy_from_slice(&at.to_le_bytes());
        }
        (file, thumb, placed)
    }

    fn write(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("df-affinity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("written");
        path
    }

    /// The header's pointer leads to the thumbnail, not to the larger
    /// picture placed in the document.
    #[test]
    fn the_header_leads_to_the_thumbnail() {
        let (file, thumb, _) = affinity(true);
        let path = write("pointed.afdesign", &file);
        let mut opened = std::fs::File::open(&path).expect("open");
        assert_eq!(
            pointed(&mut opened, file.len() as u64).expect("read"),
            Some(thumb.clone()),
            "found by the pointer, not the scan"
        );
        assert_eq!(thumbnail(&path).expect("read"), Some(thumb));
    }

    /// With no pointer to follow, the scan finds the record all the same.
    #[test]
    fn a_scan_finds_the_record() {
        let (file, thumb, _) = affinity(false);
        let range = scan(&file).expect("a thumbnail");
        assert_eq!(&file[range], &thumb[..]);
        let path = write("scanned.af", &file);
        assert_eq!(thumbnail(&path).expect("read"), Some(thumb));
    }

    /// With no record at all, the largest well-formed PNG is the best
    /// guess; a broken one, however large it claims to be, is never taken.
    #[test]
    fn without_a_record_the_largest_png_is_taken() {
        let small = png(64, 64);
        let large = png(640, 480);
        let mut broken = png(4000, 4000);
        broken.truncate(broken.len() - 6);
        let mut file = noise(1000, 4);
        file.extend_from_slice(&small);
        file.extend(noise(200, 5));
        file.extend_from_slice(&large);
        file.extend(noise(200, 6));
        file.extend_from_slice(&broken);
        let range = scan(&file).expect("a picture");
        assert_eq!(&file[range], &large[..]);
    }

    /// Noise, or a PNG cut short, has no thumbnail; neither does a file too
    /// short for a header.
    #[test]
    fn nothing_is_found_where_there_is_nothing() {
        assert_eq!(scan(&noise(100_000, 7)), None);
        let mut cut = png(10, 10);
        cut.truncate(cut.len() - 1);
        assert_eq!(scan(&cut), None);
        let path = write("noise.afphoto", &[MAGIC, &noise(4000, 8)[..]].concat());
        assert_eq!(thumbnail(&path).expect("read"), None);
        let path = write("short.afpub", MAGIC);
        assert_eq!(thumbnail(&path).expect("read"), None);
    }

    /// A pointer past the end, or at bytes that are not a record, is
    /// ignored for the scan, which still finds the thumbnail.
    #[test]
    fn a_bad_pointer_falls_back_to_the_scan() {
        let (mut file, thumb, _) = affinity(false);
        file[THUMB_POINTER..THUMB_POINTER + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        let path = write("past.afdesign", &file);
        assert_eq!(thumbnail(&path).expect("read"), Some(thumb.clone()));
        file[THUMB_POINTER..THUMB_POINTER + 8].copy_from_slice(&40u64.to_le_bytes());
        let path = write("wrong.afdesign", &file);
        assert_eq!(thumbnail(&path).expect("read"), Some(thumb));
    }

    #[test]
    fn each_app_is_named() {
        assert_eq!(what("Logo.afdesign"), "Affinity Designer document");
        assert_eq!(what("photo.AFPHOTO"), "Affinity Photo document");
        assert_eq!(what("book.afpub"), "Affinity Publisher document");
        assert_eq!(what("Cover v1.af"), "Affinity document");
        assert_eq!(what("noext"), "Affinity document");
    }
}
