//! The smallest photo reader that answers what the template asks.
//!
//! `{taken}`, `{width}`, `{height}` and `{camera}` need four things from a
//! photo: when it was taken, how big it is, and the make and model of what
//! took it. A general metadata library answers a thousand questions to get to
//! those four, and this program's dependency rule (PLAN §1) asks whether
//! rewriting is impractical. For four tags in two container formats it is not,
//! so this is hand-rolled.
//!
//! # What it reads
//!
//! - **JPEG.** The marker stream up to the first scan: the frame size from the
//!   first SOF, and the EXIF block from the `APP1` segment that starts with
//!   `Exif\0\0`, which is a TIFF stream in its own right.
//! - **TIFF**, and with it every raw format built on it: NEF, CR2, DNG, ARW,
//!   PEF, SRW. The file *is* the TIFF stream, so it goes straight to the same
//!   parser. Olympus ORF and Panasonic RW2 are TIFF streams too. Each has its
//!   own four-byte magic in place of TIFF's `*`, and nothing else differs.
//!
//! PNG, HEIC and WebP are left out on purpose. PNG rarely carries a taken
//! date, and HEIC hides its EXIF inside an ISO box tree that is a project of
//! its own. A file this does not recognise is `None`, and `{taken}` says "no
//! date taken" on its row, which is also true of a screenshot.
//!
//! # What it trusts
//!
//! Nothing. The input is any file a person selected, and a truncated download
//! or a corrupted card is exactly the kind of file that turns up in a bulk
//! rename. Every offset is checked before it is followed, every count is
//! bounded by the bytes actually present, and the only pointer followed out of
//! IFD0 is the one to the Exif IFD, so a hostile file cannot build a loop. The
//! worst it can do is make this return less.
//!
//! It never reads a whole file, either. A 40 MB raw keeps its metadata in its
//! first few kilobytes, and a hundred raws on an SD card at 90 MB/s is the
//! difference between a card that fills in and one that stalls. See [`read`]
//! for the window.

use std::io::Read;
use std::path::Path;

use super::facts::{Civil, PhotoFacts};

/// The first read. Enough for almost every JPEG, whose EXIF segment is capped
/// at 64 KB and whose frame header follows it, and for the IFDs at the front of
/// a raw.
const FIRST_LOOK: u64 = 256 * 1024;

/// The most this will ever read of one file. A JPEG with a large ICC profile or
/// several XMP segments ahead of its frame header, or a raw that parks a
/// value past its preview, gets this far and no further.
const LAST_LOOK: u64 = 2 * 1024 * 1024;

/// Longest string value read out of an IFD. A camera model is a few dozen bytes
/// and a date is twenty; a tag that claims a megabyte is lying or padding.
const STRING_CAP: usize = 256;

/// Read what the template wants from the photo at `path`, or `None` when the
/// file is not a JPEG or TIFF (or cannot be opened).
///
/// Reads the first 256 KB and parses them. Only if that came back without a
/// taken date or a size, and the file is longer, does it read on to 2 MB and
/// try again: the common photo costs one small read, and the odd one with a
/// fat header still gets found.
pub fn read(path: &Path) -> Option<PhotoFacts> {
    // Only regular files (through a symlink, if that is what was selected).
    // Opening a named pipe blocks until something writes to it, which would
    // park a photo worker for good on a file that was never a photo.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(FIRST_LOOK)
        .read_to_end(&mut bytes)
        .ok()?;
    let first = parse(&bytes)?;
    let answered = first.taken.is_some() && first.width.is_some();
    let whole_file = (bytes.len() as u64) < FIRST_LOOK;
    if answered || whole_file {
        return Some(first);
    }
    if file
        .by_ref()
        .take(LAST_LOOK - FIRST_LOOK)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Some(first);
    }
    parse(&bytes).or(Some(first))
}

/// Pick the parser by the file's magic number, never by its extension: a
/// `.jpg` that is really a PNG is not a JPEG, and a `.NEF` is a TIFF.
fn parse(bytes: &[u8]) -> Option<PhotoFacts> {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        parse_jpeg(bytes)
    } else {
        parse_tiff(bytes)
    }
}

/// A JPEG: the frame size from the first SOF, the rest from its EXIF.
///
/// `None` only when `bytes` does not start with the JPEG start-of-image
/// marker. A JPEG that stops short or carries no EXIF is still a JPEG, and
/// comes back with whatever was found before the damage.
///
/// The frame header's size wins over the EXIF's pixel dimensions: it is the
/// size of the image that is actually encoded, whereas the EXIF copy is
/// whatever the last editor forgot to update.
pub fn parse_jpeg(bytes: &[u8]) -> Option<PhotoFacts> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut exif: Option<PhotoFacts> = None;
    let mut frame: Option<(u32, u32)> = None;
    let mut pos = 2;
    // Every pass consumes at least the marker byte, so `pos` only grows and the
    // loop ends at the buffer's end whatever the bytes say.
    while bytes.get(pos) == Some(&0xFF) {
        // Any number of 0xFF fill bytes may precede a marker.
        while bytes.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let Some(&marker) = bytes.get(pos) else {
            break;
        };
        pos += 1;
        match marker {
            // Markers with no length: SOI again, TEM, and the restart markers.
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            // End of image, or start of scan: past here is entropy-coded data,
            // and everything this reader wants comes before it.
            0xD9 | 0xDA => break,
            _ => {}
        }
        let Some(len) = be16(bytes, pos).map(usize::from) else {
            break;
        };
        // The length counts its own two bytes; less than that is corruption.
        if len < 2 {
            break;
        }
        let Some(payload) = bytes.get(pos + 2..pos + len) else {
            break;
        };
        if marker == 0xE1 && exif.is_none() {
            if let Some(tiff) = payload.strip_prefix(b"Exif\0\0") {
                exif = parse_tiff(tiff);
            }
        }
        if is_frame_header(marker) {
            let height = be16(payload, 1).map(u32::from).filter(|&v| v > 0);
            let width = be16(payload, 3).map(u32::from).filter(|&v| v > 0);
            frame = width.zip(height);
            // EXIF is required to come before the frame, so nothing past the
            // first frame header is of interest.
            break;
        }
        pos += len;
    }
    let mut facts = exif.unwrap_or_default();
    if let Some((width, height)) = frame {
        facts.width = Some(width);
        facts.height = Some(height);
    }
    Some(facts)
}

/// A start-of-frame marker: SOF0–SOF15, minus the three codes in that range
/// that are something else (DHT `C4`, JPG `C8`, DAC `CC`).
fn is_frame_header(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF)
}

/// Whether a TIFF stream is big-endian, read off its first four bytes, or
/// `None` when they are not a TIFF header.
///
/// Plain TIFF is `II*\0` (Intel, little-endian) or `MM\0*` (Motorola,
/// big-endian). Two raw formats that are TIFF inside change the magic and
/// nothing else, so a reader that only knew TIFF's own would call their files
/// "not a photo":
///
/// - Olympus ORF: `IIRO` or `IIRS` little-endian, `MMOR` big-endian.
/// - Panasonic RW2: `IIU\0`, little-endian.
///
/// In all of them the IFD0 offset follows the magic, and the IFDs are laid
/// out as TIFF lays them out.
fn tiff_order(bytes: &[u8]) -> Option<bool> {
    let magic = bytes.get(..4)?;
    match magic {
        b"MM\0*" | b"MMOR" => Some(true),
        b"II*\0" | b"IIRO" | b"IIRS" | b"IIU\0" => Some(false),
        _ => None,
    }
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    let pair = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([pair[0], pair[1]]))
}

/// A TIFF stream: a raw file, a `.tif`, or the inside of a JPEG's EXIF
/// segment.
///
/// `None` only when the header is not a TIFF magic: `II*\0`, `MM\0*`, or the
/// ORF and RW2 variants `tiff_order` lists. Past the header, a damaged stream
/// yields whatever could be read.
///
/// Dimensions come from IFD0's `ImageWidth`/`ImageLength` when it has both
/// and is the full image, else from the Exif IFD's
/// `PixelXDimension`/`PixelYDimension`. Many raws (NEF, most DNGs) keep a
/// small preview in IFD0 and the sensor data in a sub-IFD; IFD0's
/// `NewSubfileType` says so with bit 0, "reduced resolution", and its
/// 160×120 is then the size of the thumbnail, not of the photo. The date is
/// `DateTimeOriginal` from the Exif IFD, falling back to IFD0's `DateTime`,
/// which is when the file was last written but on an untouched camera file
/// is the same moment.
pub fn parse_tiff(bytes: &[u8]) -> Option<PhotoFacts> {
    let big = tiff_order(bytes)?;
    let tiff = Tiff { bytes, big };
    let mut facts = PhotoFacts::default();
    let Some(ifd0) = tiff.u32(4) else {
        return Some(facts);
    };

    let mut written = None;
    let mut exif_ifd = None;
    let (mut width, mut height) = (None, None);
    let mut reduced = false;
    for entry in tiff.entries(ifd0) {
        match entry.tag {
            0x010F => facts.make = tiff.ascii(&entry),
            0x0110 => facts.model = tiff.ascii(&entry),
            0x0132 => written = tiff.ascii(&entry).as_deref().and_then(parse_datetime),
            0x0100 => width = tiff.uint(&entry).filter(|&v| v > 0),
            0x0101 => height = tiff.uint(&entry).filter(|&v| v > 0),
            0x00FE => reduced = tiff.uint(&entry).is_some_and(|v| v & 1 != 0),
            0x8769 => exif_ifd = tiff.uint(&entry),
            _ => {}
        }
    }

    let mut original = None;
    let (mut pixel_x, mut pixel_y) = (None, None);
    if let Some(at) = exif_ifd {
        for entry in tiff.entries(at) {
            match entry.tag {
                0x9003 => original = tiff.ascii(&entry).as_deref().and_then(parse_datetime),
                0xA002 => pixel_x = tiff.uint(&entry).filter(|&v| v > 0),
                0xA003 => pixel_y = tiff.uint(&entry).filter(|&v| v > 0),
                _ => {}
            }
        }
    }

    facts.taken = original.or(written);
    // A width from one IFD and a height from the other would describe no image
    // at all, so the pair is taken together.
    (facts.width, facts.height) = match (width, height) {
        (Some(w), Some(h)) if !reduced => (Some(w), Some(h)),
        _ => (pixel_x, pixel_y),
    };
    Some(facts)
}

/// A TIFF stream and its byte order.
struct Tiff<'a> {
    bytes: &'a [u8],
    big: bool,
}

/// One 12-byte IFD entry, by where it sits. The value (or the offset to it)
/// is the four bytes at `at + 8`.
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    at: usize,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let pair = self.bytes.get(at..at.checked_add(2)?)?;
        let pair = [pair[0], pair[1]];
        Some(if self.big {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let quad = self.bytes.get(at..at.checked_add(4)?)?;
        let quad = [quad[0], quad[1], quad[2], quad[3]];
        Some(if self.big {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        })
    }

    /// The entries of the IFD at `offset`, stopping at the first one that
    /// would run past the end. The count is a `u16`, so this is at most 65,535
    /// twelve-byte reads however the file lies.
    fn entries(&self, offset: u32) -> impl Iterator<Item = Entry> + '_ {
        let start = offset as usize;
        let count = self.u16(start).unwrap_or(0);
        (0..usize::from(count)).map_while(move |i| {
            let at = start.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
            Some(Entry {
                tag: self.u16(at)?,
                kind: self.u16(at + 2)?,
                count: self.u32(at + 4)?,
                at,
            })
        })
    }

    /// A SHORT or LONG (or IFD-pointer) value, which the dimension tags may be
    /// written as either of.
    fn uint(&self, entry: &Entry) -> Option<u32> {
        if entry.count == 0 {
            return None;
        }
        match entry.kind {
            3 => self.u16(entry.at + 8).map(u32::from),
            4 | 13 => self.u32(entry.at + 8),
            _ => None,
        }
    }

    /// An ASCII value: inline when it fits in four bytes, else at the offset
    /// those four bytes hold. Cut at the first NUL and trimmed, since cameras
    /// pad make and model with both. Empty is `None`.
    fn ascii(&self, entry: &Entry) -> Option<String> {
        if entry.kind != 2 {
            return None;
        }
        let count = (entry.count as usize).min(STRING_CAP);
        let start = if entry.count <= 4 {
            entry.at + 8
        } else {
            self.u32(entry.at + 8)? as usize
        };
        let raw = self.bytes.get(start..start.checked_add(count)?)?;
        let raw = raw.split(|&b| b == 0).next().unwrap_or_default();
        let text = String::from_utf8_lossy(raw);
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }
}

/// EXIF's `YYYY:MM:DD HH:MM:SS`.
///
/// Exactly nineteen characters, digits where the digits go and any non-digit
/// between them: the standard says colons, and the odd writer that used dashes
/// in the date still meant the same moment. `0000:00:00 00:00:00`, which is
/// what a camera writes when it has no clock, is not a date, and neither is
/// February 30th.
fn parse_datetime(text: &str) -> Option<Civil> {
    let b = text.as_bytes();
    if b.len() != 19 {
        return None;
    }
    let separators_ok = [4, 7, 10, 13, 16]
        .iter()
        .all(|&i| b.get(i).is_some_and(|c| !c.is_ascii_digit()));
    if !separators_ok {
        return None;
    }
    let number = |from: usize, len: usize| -> Option<u32> {
        b.get(from..from + len)?.iter().try_fold(0u32, |acc, &d| {
            d.is_ascii_digit().then(|| acc * 10 + u32::from(d - b'0'))
        })
    };
    let year = i32::try_from(number(0, 4)?).ok()?;
    let civil = Civil {
        year,
        month: number(5, 2)?,
        day: number(8, 2)?,
        hour: number(11, 2)?,
        minute: number(14, 2)?,
        second: number(17, 2)?,
    };
    let valid = year >= 1
        && (1..=12).contains(&civil.month)
        && (1..=days_in(year, civil.month)).contains(&civil.day)
        && civil.hour < 24
        && civil.minute < 60
        && civil.second < 60;
    valid.then_some(civil)
}

fn days_in(year: i32, month: u32) -> u32 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempTree;

    // ── Building files ──────────────────────────────────────────────────────

    #[derive(Clone, Copy)]
    enum Value<'a> {
        Ascii(&'a str),
        Short(u16),
        Long(u32),
    }

    /// Writes numbers in one byte order, so the same test can be run on an
    /// Intel-order and a Motorola-order file.
    struct Writer {
        big: bool,
        out: Vec<u8>,
    }

    impl Writer {
        fn u16(&mut self, v: u16) {
            let bytes = if self.big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            };
            self.out.extend(bytes);
        }

        fn u32(&mut self, v: u32) {
            let bytes = if self.big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            };
            self.out.extend(bytes);
        }

        /// One IFD. Values over four bytes go into `data`, which the caller
        /// appends at `data_at` once every IFD is written.
        fn ifd(&mut self, entries: &[(u16, Value)], data: &mut Vec<u8>, data_at: usize) {
            self.u16(entries.len() as u16);
            for &(tag, value) in entries {
                self.u16(tag);
                match value {
                    Value::Short(v) => {
                        self.u16(3);
                        self.u32(1);
                        self.u16(v);
                        self.u16(0);
                    }
                    Value::Long(v) => {
                        self.u16(4);
                        self.u32(1);
                        self.u32(v);
                    }
                    Value::Ascii(s) => {
                        let mut bytes = s.as_bytes().to_vec();
                        bytes.push(0);
                        self.u16(2);
                        self.u32(bytes.len() as u32);
                        if bytes.len() <= 4 {
                            bytes.resize(4, 0);
                            self.out.extend(bytes);
                        } else {
                            self.u32((data_at + data.len()) as u32);
                            data.extend(bytes);
                            if data.len() % 2 == 1 {
                                data.push(0);
                            }
                        }
                    }
                }
            }
            self.u32(0);
        }
    }

    /// A TIFF stream: header, IFD0, then an Exif IFD if `exif` has entries
    /// (IFD0 gets the pointer to it), then the out-of-line values.
    fn tiff(big: bool, ifd0: &[(u16, Value)], exif: &[(u16, Value)]) -> Vec<u8> {
        let ifd_len = |n: usize| 2 + 12 * n + 4;
        let mut first: Vec<(u16, Value)> = ifd0.to_vec();
        let exif_at = 8 + ifd_len(first.len() + usize::from(!exif.is_empty()));
        if !exif.is_empty() {
            first.push((0x8769, Value::Long(exif_at as u32)));
        }
        let data_at = exif_at
            + if exif.is_empty() {
                0
            } else {
                ifd_len(exif.len())
            };
        let mut w = Writer {
            big,
            out: Vec::new(),
        };
        w.out.extend(if big { b"MM\0*" } else { b"II*\0" });
        w.u32(8);
        let mut data = Vec::new();
        w.ifd(&first, &mut data, data_at);
        if !exif.is_empty() {
            w.ifd(exif, &mut data, data_at);
        }
        assert_eq!(w.out.len(), data_at, "the layout arithmetic is off");
        w.out.extend(data);
        w.out
    }

    fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        out.extend([0xFF, marker]);
        out.extend(((payload.len() + 2) as u16).to_be_bytes());
        out.extend(payload);
    }

    /// A JPEG shaped like a camera's: JFIF, the EXIF segment, a quantisation
    /// table, a baseline frame header, a scan and some entropy-coded bytes.
    fn jpeg(exif: Option<&[u8]>, width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        segment(&mut out, 0xE0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0");
        if let Some(tiff) = exif {
            let mut payload = b"Exif\0\0".to_vec();
            payload.extend(tiff);
            segment(&mut out, 0xE1, &payload);
        }
        segment(&mut out, 0xDB, &[0; 65]);
        let mut sof = vec![8];
        sof.extend(height.to_be_bytes());
        sof.extend(width.to_be_bytes());
        sof.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        segment(&mut out, 0xC0, &sof);
        segment(&mut out, 0xDA, &[3, 1, 0, 2, 0x11, 3, 0x11, 0, 63, 0]);
        out.extend([0x12, 0x34, 0xFF, 0x00, 0x56, 0xFF, 0xD9]);
        out
    }

    fn camera_tiff(big: bool) -> Vec<u8> {
        tiff(
            big,
            &[
                (0x010F, Value::Ascii("Canon")),
                (0x0110, Value::Ascii("Canon EOS R5")),
                (0x0132, Value::Ascii("2024:07:01 10:00:00")),
            ],
            &[
                (0x9003, Value::Ascii("2024:05:06 14:03:22")),
                (0xA002, Value::Long(999)),
                (0xA003, Value::Short(666)),
            ],
        )
    }

    fn civil(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Civil {
        Civil {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    // ── JPEG ────────────────────────────────────────────────────────────────

    #[test]
    fn a_jpeg_gives_its_exif_and_its_frame_size_in_either_byte_order() {
        for big in [false, true] {
            let bytes = jpeg(Some(&camera_tiff(big)), 6000, 4000);
            let facts = parse_jpeg(&bytes).expect("a JPEG");
            assert_eq!(facts.taken, Some(civil(2024, 5, 6, 14, 3, 22)), "big={big}");
            assert_eq!(facts.make.as_deref(), Some("Canon"));
            assert_eq!(facts.model.as_deref(), Some("Canon EOS R5"));
            // The frame header, not the EXIF's stale 999×666.
            assert_eq!((facts.width, facts.height), (Some(6000), Some(4000)));
        }
    }

    #[test]
    fn a_jpeg_without_exif_still_has_its_size() {
        let facts = parse_jpeg(&jpeg(None, 640, 480)).expect("a JPEG");
        assert_eq!((facts.width, facts.height), (Some(640), Some(480)));
        assert_eq!(facts.taken, None);
        assert_eq!(facts.make, None);
    }

    /// A progressive JPEG's frame header is SOF2, not SOF0; DHT (`C4`), which
    /// sits in the same range, is not a frame header at all.
    #[test]
    fn every_frame_header_counts_and_nothing_else_in_its_range_does() {
        let mut bytes = vec![0xFF, 0xD8];
        segment(&mut bytes, 0xC4, &[0, 0x0F, 0xA0, 0x0B, 0xB8]);
        segment(
            &mut bytes,
            0xC2,
            &[8, 0x01, 0x00, 0x02, 0x00, 1, 1, 0x11, 0],
        );
        let facts = parse_jpeg(&bytes).expect("a JPEG");
        assert_eq!((facts.width, facts.height), (Some(512), Some(256)));
    }

    #[test]
    fn fill_bytes_before_a_marker_are_skipped() {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xFF, 0xFF];
        segment(&mut bytes, 0xC0, &[8, 0, 10, 0, 20, 1, 1, 0x11, 0]);
        let facts = parse_jpeg(&bytes).expect("a JPEG");
        assert_eq!((facts.width, facts.height), (Some(20), Some(10)));
    }

    // ── TIFF and raws ───────────────────────────────────────────────────────

    #[test]
    fn a_tiff_takes_its_size_from_ifd0_first() {
        for big in [false, true] {
            let bytes = tiff(
                big,
                &[(0x0100, Value::Short(7360)), (0x0101, Value::Long(4912))],
                &[(0xA002, Value::Long(1)), (0xA003, Value::Long(1))],
            );
            let facts = parse_tiff(&bytes).expect("a TIFF");
            assert_eq!(
                (facts.width, facts.height),
                (Some(7360), Some(4912)),
                "big={big}"
            );
        }
    }

    /// A NEF-shaped file: IFD0 is a 160×120 preview marked reduced-resolution,
    /// and the photo's real size is in the Exif IFD. The flag is read wherever
    /// it sits among IFD0's entries.
    #[test]
    fn a_thumbnail_in_ifd0_gives_way_to_the_exif_size() {
        for big in [false, true] {
            let exif = [(0xA002, Value::Long(6048)), (0xA003, Value::Short(4024))];
            let first = tiff(
                big,
                &[
                    (0x00FE, Value::Long(1)),
                    (0x0100, Value::Short(160)),
                    (0x0101, Value::Short(120)),
                ],
                &exif,
            );
            let last = tiff(
                big,
                &[
                    (0x0100, Value::Short(160)),
                    (0x0101, Value::Short(120)),
                    (0x00FE, Value::Short(1)),
                ],
                &exif,
            );
            for bytes in [first, last] {
                let facts = parse_tiff(&bytes).expect("a TIFF");
                assert_eq!(
                    (facts.width, facts.height),
                    (Some(6048), Some(4024)),
                    "big={big}"
                );
            }
        }
    }

    /// Only bit 0 means "reduced": 0 is the full image, and bit 1 alone (a page
    /// of a multi-page file) is still full resolution.
    #[test]
    fn a_full_size_ifd0_keeps_its_own_size() {
        for flag in [0, 2] {
            let bytes = tiff(
                false,
                &[
                    (0x00FE, Value::Long(flag)),
                    (0x0100, Value::Long(7360)),
                    (0x0101, Value::Long(4912)),
                ],
                &[(0xA002, Value::Long(1)), (0xA003, Value::Long(1))],
            );
            let facts = parse_tiff(&bytes).expect("a TIFF");
            assert_eq!(
                (facts.width, facts.height),
                (Some(7360), Some(4912)),
                "flag={flag}"
            );
        }
    }

    #[test]
    fn a_tiff_without_ifd0_dimensions_uses_the_exif_ones() {
        for big in [false, true] {
            let facts = parse_tiff(&camera_tiff(big)).expect("a TIFF");
            assert_eq!(
                (facts.width, facts.height),
                (Some(999), Some(666)),
                "big={big}"
            );
        }
    }

    #[test]
    fn the_taken_date_falls_back_to_when_the_file_was_written() {
        let bytes = tiff(false, &[(0x0132, Value::Ascii("2019:12:31 23:59:59"))], &[]);
        let facts = parse_tiff(&bytes).expect("a TIFF");
        assert_eq!(facts.taken, Some(civil(2019, 12, 31, 23, 59, 59)));
    }

    /// Four bytes or fewer, NUL included, and the value is in the entry itself
    /// rather than at an offset.
    #[test]
    fn a_short_string_is_read_from_inside_its_entry() {
        for big in [false, true] {
            let bytes = tiff(
                big,
                &[(0x010F, Value::Ascii("LG")), (0x0110, Value::Ascii("V60"))],
                &[],
            );
            let facts = parse_tiff(&bytes).expect("a TIFF");
            assert_eq!(facts.make.as_deref(), Some("LG"), "big={big}");
            assert_eq!(facts.model.as_deref(), Some("V60"));
        }
    }

    #[test]
    fn strings_lose_their_padding() {
        let bytes = tiff(
            false,
            &[
                (0x010F, Value::Ascii("  NIKON CORPORATION  ")),
                (0x0110, Value::Ascii("   ")),
            ],
            &[],
        );
        let facts = parse_tiff(&bytes).expect("a TIFF");
        assert_eq!(facts.make.as_deref(), Some("NIKON CORPORATION"));
        assert_eq!(facts.model, None, "all padding is no model");
    }

    #[test]
    fn a_camera_with_no_clock_has_no_taken_date() {
        let bytes = tiff(false, &[], &[(0x9003, Value::Ascii("0000:00:00 00:00:00"))]);
        assert_eq!(parse_tiff(&bytes).expect("a TIFF").taken, None);
    }

    #[test]
    fn dates_are_validated_field_by_field() {
        assert_eq!(
            parse_datetime("2024:02:29 12:00:00"),
            Some(civil(2024, 2, 29, 12, 0, 0))
        );
        assert_eq!(
            parse_datetime("2024-05-06 14:03:22"),
            Some(civil(2024, 5, 6, 14, 3, 22))
        );
        for bad in [
            "0000:00:00 00:00:00",
            "2023:02:29 12:00:00",
            "2024:13:01 00:00:00",
            "2024:00:10 00:00:00",
            "2024:04:31 00:00:00",
            "2024:05:06 24:00:00",
            "2024:05:06 14:60:00",
            "2024:05:06 14:03:60",
            "    :  :     :  :  ",
            "2024:05:06",
            "2024:05:06 14:03:22 extra",
            "2024:05:0614:03:22 ",
            "２０２４:05:06 14:03",
        ] {
            assert_eq!(parse_datetime(bad), None, "{bad:?}");
        }
    }

    /// Olympus and Panasonic raws are TIFF streams behind their own magic.
    /// The same camera file, relabelled, reads the same in both byte orders
    /// the formats come in.
    #[test]
    fn orf_and_rw2_magics_are_read_as_tiff() {
        let magics: [(&[u8; 4], bool); 4] = [
            (b"IIRO", false),
            (b"IIRS", false),
            (b"MMOR", true),
            (b"IIU\0", false),
        ];
        for (magic, big) in magics {
            let mut bytes = camera_tiff(big);
            let plain = parse_tiff(&bytes).expect("the TIFF it is built from");
            bytes[..4].copy_from_slice(magic);
            let name = String::from_utf8_lossy(magic);
            let facts = parse(&bytes).unwrap_or_else(|| panic!("{name} is a raw"));
            assert_eq!(facts, plain, "{name}");
            assert_eq!(facts.taken, Some(civil(2024, 5, 6, 14, 3, 22)), "{name}");
            assert_eq!(facts.model.as_deref(), Some("Canon EOS R5"), "{name}");
            assert_eq!(
                (facts.width, facts.height),
                (Some(999), Some(666)),
                "{name}"
            );
        }

        // The magic fixes the byte order. An Olympus header on a big-endian
        // stream reads IFD0's offset backwards, and gets nothing.
        let mut crossed = camera_tiff(true);
        crossed[..4].copy_from_slice(b"IIRO");
        assert_eq!(parse(&crossed).and_then(|facts| facts.taken), None);
    }

    // ── Refusing ────────────────────────────────────────────────────────────

    #[test]
    fn anything_else_is_not_a_photo() {
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), None);
        assert_eq!(parse(b"hello, world"), None);
        assert_eq!(parse(b"II"), None);
        assert_eq!(parse(b"IIRX\x08\0\0\0"), None, "an ORF look-alike");
        assert_eq!(parse(b"MMU\0\0\0\0\x08"), None, "RW2 is only ever Intel");
        assert_eq!(parse_jpeg(&camera_tiff(false)), None);
        assert_eq!(parse_tiff(&jpeg(None, 1, 1)), None);
    }

    /// A download that stopped partway, at every possible byte: whatever it
    /// returns, it returns without panicking, and a cut after the frame
    /// header loses nothing.
    #[test]
    fn every_truncation_parses_without_panicking() {
        for big in [false, true] {
            let whole = jpeg(Some(&camera_tiff(big)), 6000, 4000);
            let complete = parse(&whole);
            for len in 0..=whole.len() {
                let _ = parse(&whole[..len]);
            }
            let scan = whole.len() - 17;
            assert_eq!(parse(&whole[..scan]), complete);

            let raw = camera_tiff(big);
            for len in 0..=raw.len() {
                let _ = parse(&raw[..len]);
            }
        }
    }

    #[test]
    fn hostile_offsets_and_counts_are_refused_quietly() {
        // IFD0 at the far end of the address space.
        let mut bytes = b"II*\0".to_vec();
        bytes.extend(u32::MAX.to_le_bytes());
        assert_eq!(parse_tiff(&bytes), Some(PhotoFacts::default()));

        // 65,535 entries promised, one present.
        let mut bytes = b"II*\0\x08\0\0\0\xFF\xFF".to_vec();
        bytes.extend([0x0F, 0x01, 2, 0, 5, 0, 0, 0, b'S', b'o', b'n', b'y']);
        assert_eq!(parse_tiff(&bytes), Some(PhotoFacts::default()));

        // A string that claims four gigabytes at an offset past the end, and
        // an Exif pointer into nowhere.
        let mut w = Writer {
            big: true,
            out: b"MM\0*".to_vec(),
        };
        w.u32(8);
        w.u16(2);
        w.u16(0x0110);
        w.u16(2);
        w.u32(u32::MAX);
        w.u32(u32::MAX - 1);
        w.u16(0x8769);
        w.u16(4);
        w.u32(1);
        w.u32(0xFFFF_FFF0);
        w.u32(0);
        assert_eq!(parse_tiff(&w.out), Some(PhotoFacts::default()));

        // An Exif IFD pointing back at IFD0 is read once, as an Exif IFD, and
        // not followed again.
        let looped = tiff(false, &[(0x8769, Value::Long(8))], &[]);
        assert_eq!(parse_tiff(&looped), Some(PhotoFacts::default()));

        // JPEG segment lengths of zero, one, and past the end.
        for len in [[0, 0], [0, 1], [0xFF, 0xFF]] {
            let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1];
            bytes.extend(len);
            bytes.extend(b"Exif\0\0II*\0");
            assert_eq!(parse_jpeg(&bytes), Some(PhotoFacts::default()), "{len:?}");
        }
    }

    /// Cheap fuzzing: a fixed-seed xorshift flips bytes in a valid JPEG and a
    /// valid TIFF, thousands of times. The assertion is the absence of a
    /// panic; the seed is fixed so a failure reproduces.
    #[test]
    fn random_corruption_never_panics() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let samples = [
            jpeg(Some(&camera_tiff(false)), 6000, 4000),
            jpeg(Some(&camera_tiff(true)), 6000, 4000),
            camera_tiff(false),
            camera_tiff(true),
        ];
        for sample in &samples {
            for _ in 0..3000 {
                let mut bytes = sample.clone();
                let flips = 1 + next() % 8;
                for _ in 0..flips {
                    let at = (next() % bytes.len() as u64) as usize;
                    bytes[at] = next() as u8;
                }
                let cut = (next() % (bytes.len() as u64 + 1)) as usize;
                let _ = parse(&bytes[..cut]);
                let _ = parse(&bytes);
            }
        }
    }

    // ── From disk ───────────────────────────────────────────────────────────

    #[test]
    fn read_opens_a_file_and_refuses_a_missing_one() {
        let tree = TempTree::new("exif-read");
        let path = tree.file("IMG_0001.JPG", &jpeg(Some(&camera_tiff(false)), 30, 20));
        let facts = read(&path).expect("a JPEG on disk");
        assert_eq!(facts.taken, Some(civil(2024, 5, 6, 14, 3, 22)));
        assert_eq!((facts.width, facts.height), (Some(30), Some(20)));
        assert_eq!(read(&tree.join("missing.jpg")), None);
        let text = tree.file("notes.jpg", b"not really a jpeg");
        assert_eq!(read(&text), None);
        assert_eq!(read(&tree.dir("DCIM.jpg")), None);
    }

    /// Five full-size APP2 segments push the frame header past the first read;
    /// the second read still finds it.
    #[test]
    fn read_looks_further_when_the_header_is_fat() {
        let tree = TempTree::new("exif-fat");
        let mut bytes = vec![0xFF, 0xD8];
        for _ in 0..5 {
            segment(&mut bytes, 0xE2, &[0; 65_000]);
        }
        assert!(bytes.len() as u64 > FIRST_LOOK);
        segment(&mut bytes, 0xC0, &[8, 0, 10, 0, 20, 1, 1, 0x11, 0]);
        bytes.resize(bytes.len() + 1_000_000, 0);
        let path = tree.file("fat.jpg", &bytes);
        let facts = read(&path).expect("a JPEG");
        assert_eq!((facts.width, facts.height), (Some(20), Some(10)));
    }
}
