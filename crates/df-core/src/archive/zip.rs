//! Zip, read from the back.
//!
//! A zip is not a stream, it is a random-access file with an index at the end.
//! The index — the *central directory* — is the only authoritative listing:
//! local headers at the front of each member are allowed to lie (they carry
//! zeroes when the writer was streaming and put the real sizes in a data
//! descriptor afterwards), and a self-extracting zip has an entire executable
//! bolted on the front, so a parser that walks forward from byte zero is a
//! parser that is wrong about half the zips in the world.
//!
//! So: find the End Of Central Directory record, follow it to the central
//! directory, read the entries. Three steps, each with a way of being nasty.
//!
//! ## Finding the EOCD
//!
//! The EOCD is 22 bytes, at the end of the file — *unless* there is a zip file
//! comment, which is up to 65,535 bytes and sits after it. There is no length
//! prefix; the only way to find the record is to scan backwards for its
//! signature over the last 64 KiB. Which means the signature can also appear
//! inside the comment, or inside a stored member's data. So the scan takes the
//! **last** candidate whose declared comment length exactly accounts for the
//! bytes after it — that check is what distinguishes the record from a
//! coincidence.
//!
//! ## Zip64
//!
//! Past 65,535 entries or 4 GiB, the 16- and 32-bit fields in the EOCD saturate
//! to all-ones and the real values move to a zip64 EOCD record, found through a
//! 20-byte locator immediately before the EOCD. Both are read when present,
//! because "your 8 GB backup has 0 files in it" is the failure mode otherwise.
//!
//! ## Names
//!
//! Bit 11 of the general purpose flags means the name is UTF-8. Without it the
//! name is officially CP437, and in practice is often UTF-8 anyway (writers lie
//! about this constantly), so: UTF-8 if it decodes as UTF-8, CP437 otherwise.
//! That order never mangles a correct UTF-8 name and still renders a 1997 zip's
//! `ä` as `ä` instead of a replacement character.

use std::io::{Read, Seek, SeekFrom};

use super::tree::{Method, RawEntry, MAX_ENTRIES, MAX_NAME_BYTES};
use super::ArchiveError;

/// How far back to look for the EOCD: 65,557 bytes.
///
/// 22 bytes of record plus the 65,535-byte maximum comment. Exactly the largest
/// distance the record can legally be from the end of the file, so a smaller
/// number would fail to open valid archives and a larger one would only read
/// bytes that cannot contain it.
pub const EOCD_SEARCH: usize = 22 + 65_535;

/// The most central-directory bytes read: 128 MiB.
///
/// The central directory is ~46 bytes plus the name per entry, so 128 MiB is
/// roughly two million entries — comfortably past [`MAX_ENTRIES`], which is the
/// cap that should actually bite. This one exists because `cd_size` is a number
/// *in the file*: a 200-byte zip may claim a 16 EiB directory, and the
/// allocation must be bounded before the claim is believed.
pub const MAX_CENTRAL_BYTES: u64 = 128 * 1024 * 1024;

const EOCD_SIG: &[u8] = b"PK\x05\x06";
const EOCD64_LOCATOR_SIG: &[u8] = b"PK\x06\x07";
const EOCD64_SIG: &[u8] = b"PK\x06\x06";
const CENTRAL_SIG: &[u8] = b"PK\x01\x02";
/// The fixed part of a central directory header, before the name.
const CENTRAL_FIXED: usize = 46;
/// Saturation sentinel: this field's real value is in a zip64 extra field.
const ZIP64_MARK32: u32 = 0xFFFF_FFFF;
const ZIP64_MARK16: u16 = 0xFFFF;

fn malformed(message: impl Into<String>) -> ArchiveError {
    ArchiveError::Malformed {
        format: "zip",
        message: message.into(),
    }
}

/// List a zip's central directory.
///
/// `len` is the file's length, which the caller already has from its metadata
/// and which this needs to know where the end is.
pub fn list<R: Read + Seek>(
    reader: &mut R,
    len: u64,
) -> Result<(Vec<RawEntry>, bool), ArchiveError> {
    if len < 22 {
        return Err(malformed("shorter than an end-of-central-directory record"));
    }

    let tail_len = len.min(EOCD_SEARCH as u64);
    let tail_at = len - tail_len;
    reader
        .seek(SeekFrom::Start(tail_at))
        .map_err(ArchiveError::io_bare)?;
    let mut tail = vec![0u8; tail_len as usize];
    reader
        .read_exact(&mut tail)
        .map_err(ArchiveError::io_bare)?;

    let eocd_at =
        find_eocd(&tail).ok_or_else(|| malformed("no end-of-central-directory record"))?;
    let eocd = &tail[eocd_at..];

    let mut entry_count = read_u16(eocd, 10)? as u64;
    let mut cd_size = read_u32(eocd, 12)? as u64;
    let mut cd_offset = read_u32(eocd, 16)? as u64;

    let saturated = read_u16(eocd, 10)? == ZIP64_MARK16
        || read_u32(eocd, 12)? == ZIP64_MARK32
        || read_u32(eocd, 16)? == ZIP64_MARK32;
    // The locator is read whenever it is there, not only when a field saturated:
    // some writers emit zip64 records for archives that would have fit, and the
    // zip64 values are authoritative either way.
    if let Some((c, s, o)) = read_zip64(reader, &tail, eocd_at, tail_at)? {
        entry_count = c;
        cd_size = s;
        cd_offset = o;
    } else if saturated {
        return Err(malformed(
            "zip64 fields are saturated but there is no zip64 record",
        ));
    }

    if cd_offset > len {
        return Err(malformed(format!(
            "central directory starts at {cd_offset}, past the end of a {len}-byte file"
        )));
    }
    let want = cd_size.min(len - cd_offset).min(MAX_CENTRAL_BYTES);
    reader
        .seek(SeekFrom::Start(cd_offset))
        .map_err(ArchiveError::io_bare)?;
    let mut central = vec![0u8; want as usize];
    reader
        .read_exact(&mut central)
        .map_err(ArchiveError::io_bare)?;

    let mut entries = Vec::with_capacity((entry_count as usize).min(4096));
    let mut truncated = want < cd_size;
    let mut at = 0usize;

    while at + CENTRAL_FIXED <= central.len() {
        if &central[at..at + 4] != CENTRAL_SIG {
            // The directory ended, or the file lied about its size. Either way
            // what has been read so far is real, and stopping beats guessing.
            break;
        }
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let h = &central[at..];
        let flags = read_u16(h, 8)?;
        let method = read_u16(h, 10)?;
        let dos_time = read_u16(h, 12)?;
        let dos_date = read_u16(h, 14)?;
        let mut compressed = read_u32(h, 20)? as u64;
        let mut uncompressed = read_u32(h, 24)? as u64;
        let name_len = read_u16(h, 28)? as usize;
        let extra_len = read_u16(h, 30)? as usize;
        let comment_len = read_u16(h, 32)? as usize;
        let external = read_u32(h, 38)?;

        let name_at = at + CENTRAL_FIXED;
        let extra_at = name_at + name_len;
        let end = extra_at + extra_len + comment_len;
        if end > central.len() {
            truncated = true;
            break;
        }
        let name_bytes = &central[name_at..extra_at];
        let extra = &central[extra_at..extra_at + extra_len];

        let utf8_flag = flags & 0x0800 != 0;
        let name = decode_name(name_bytes, utf8_flag);
        let mut mtime = dos_datetime(dos_date, dos_time);

        if let Some(unix) = extended_timestamp(extra) {
            // A real unix timestamp beats DOS's two-second, no-timezone
            // approximation whenever a writer bothered to include one.
            mtime = Some(unix);
        }
        if uncompressed == ZIP64_MARK32 as u64 || compressed == ZIP64_MARK32 as u64 {
            if let Some((u, c)) = zip64_sizes(extra, uncompressed, compressed) {
                uncompressed = u;
                compressed = c;
            }
        }

        // Directory-ness has three tells and any one is enough: the trailing
        // slash every writer produces, the MS-DOS directory attribute, and the
        // unix mode in the high half of the external attributes.
        let unix_mode = (external >> 16) as u16;
        let is_dir = name.ends_with('/')
            || name.ends_with('\\')
            || external & 0x10 != 0
            || unix_mode & 0xF000 == 0x4000;

        entries.push(RawEntry {
            name,
            len: uncompressed,
            compressed,
            mtime,
            is_dir,
            method: Method::from_zip(method),
            // Bit 0 is "this entry is encrypted". Listing still works: names and
            // sizes live in the central directory, which is never encrypted.
            encrypted: flags & 0x0001 != 0,
            link_target: None,
        });

        at = end;
    }

    Ok((entries, truncated))
}

/// The offset of the EOCD within `tail`, or `None`.
///
/// Backwards, taking the first candidate whose comment length lands exactly on
/// the end of the file — see the module essay on why that check is the whole
/// game. If nothing validates exactly, the last structurally-possible candidate
/// is accepted, because a zip with trailing garbage after the comment is still a
/// zip every other tool opens.
fn find_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < 22 {
        return None;
    }
    let mut fallback = None;
    let mut i = tail.len() - 22;
    loop {
        if &tail[i..i + 4] == EOCD_SIG {
            let comment_len = u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize;
            if i + 22 + comment_len == tail.len() {
                return Some(i);
            }
            if fallback.is_none() && i + 22 + comment_len <= tail.len() {
                fallback = Some(i);
            }
        }
        if i == 0 {
            return fallback;
        }
        i -= 1;
    }
}

/// `(entry_count, cd_size, cd_offset)` from the zip64 records, if present.
fn read_zip64<R: Read + Seek>(
    reader: &mut R,
    tail: &[u8],
    eocd_at: usize,
    tail_at: u64,
) -> Result<Option<(u64, u64, u64)>, ArchiveError> {
    if eocd_at < 20 {
        return Ok(None);
    }
    let locator = &tail[eocd_at - 20..eocd_at];
    if &locator[..4] != EOCD64_LOCATOR_SIG {
        return Ok(None);
    }
    let at = read_u64(locator, 8)?;
    // The offset is absolute in the file; a bogus one must not become a seek to
    // the middle of nowhere followed by a 56-byte read of whatever is there.
    if at + 56 > tail_at + tail.len() as u64 {
        return Err(malformed("zip64 locator points past the end of the file"));
    }
    reader
        .seek(SeekFrom::Start(at))
        .map_err(ArchiveError::io_bare)?;
    let mut rec = [0u8; 56];
    reader.read_exact(&mut rec).map_err(ArchiveError::io_bare)?;
    if &rec[..4] != EOCD64_SIG {
        return Err(malformed("zip64 locator does not point at a zip64 record"));
    }
    Ok(Some((
        read_u64(&rec, 32)?,
        read_u64(&rec, 40)?,
        read_u64(&rec, 48)?,
    )))
}

/// Walk the extra-field list looking for the zip64 sizes (header id 0x0001).
///
/// The zip64 extra is positional and *only contains the fields that saturated*,
/// in a fixed order: uncompressed, compressed, local header offset, disk. So
/// which 8 bytes to read depends on which 32-bit fields were all-ones.
fn zip64_sizes(extra: &[u8], uncompressed: u64, compressed: u64) -> Option<(u64, u64)> {
    for (id, data) in ExtraFields(extra) {
        if id != 0x0001 {
            continue;
        }
        let mut at = 0usize;
        let mut u = uncompressed;
        let mut c = compressed;
        if uncompressed == ZIP64_MARK32 as u64 {
            u = read_u64(data, at).ok()?;
            at += 8;
        }
        if compressed == ZIP64_MARK32 as u64 {
            c = read_u64(data, at).ok()?;
        }
        return Some((u, c));
    }
    None
}

/// The unix mtime from an extended timestamp extra (header id 0x5455), if it
/// carries one. Byte zero is a bitmask; bit 0 means a modification time follows.
fn extended_timestamp(extra: &[u8]) -> Option<i64> {
    for (id, data) in ExtraFields(extra) {
        if id != 0x5455 || data.len() < 5 || data[0] & 0x01 == 0 {
            continue;
        }
        let secs = i32::from_le_bytes([data[1], data[2], data[3], data[4]]);
        return Some(secs as i64);
    }
    None
}

/// `(header id, payload)` over a zip extra-field block.
struct ExtraFields<'a>(&'a [u8]);

impl<'a> Iterator for ExtraFields<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<(u16, &'a [u8])> {
        if self.0.len() < 4 {
            return None;
        }
        let id = u16::from_le_bytes([self.0[0], self.0[1]]);
        let size = u16::from_le_bytes([self.0[2], self.0[3]]) as usize;
        let end = 4usize.checked_add(size)?;
        if end > self.0.len() {
            // A field claiming more than is there: stop rather than run off.
            self.0 = &[];
            return None;
        }
        let data = &self.0[4..end];
        self.0 = &self.0[end..];
        Some((id, data))
    }
}

/// Decode an entry name.
///
/// UTF-8 first regardless of the flag — see the module essay. The name is capped
/// here rather than in the tree so a 60 KB "filename" never becomes a `String`.
fn decode_name(bytes: &[u8], _utf8_flag: bool) -> String {
    let bytes = &bytes[..bytes.len().min(MAX_NAME_BYTES)];
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|b| cp437(*b)).collect(),
    }
}

/// CP437, the character set every pre-Unicode zip's names are in.
///
/// The low half is ASCII; the high half is the IBM PC's box-drawing and accented
/// characters, which is the table below. Without it, `Müller.txt` from a 1998
/// archive lists as `M?ller.txt` and cannot be selected by typing.
fn cp437(b: u8) -> char {
    const HIGH: [char; 128] = [
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ',
        'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú',
        'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡',
        '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟',
        '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘',
        '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ',
        '∞', 'φ', 'ε', '∩', '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²',
        '■', '\u{a0}',
    ];
    if b < 0x80 {
        b as char
    } else {
        HIGH[(b - 0x80) as usize]
    }
}

/// MS-DOS date and time to unix seconds.
///
/// The format is 1980-based, two-second resolution, and has no timezone at all —
/// it means "whatever the clock said on the machine that wrote it". Treating it
/// as UTC is the only choice available and is what every other tool does; the
/// error is bounded by the writer's offset from UTC, which for a listing that
/// shows a date is invisible.
fn dos_datetime(date: u16, time: u16) -> Option<i64> {
    let day = (date & 0x1f) as i64;
    let month = ((date >> 5) & 0x0f) as i64;
    let year = 1980 + ((date >> 9) & 0x7f) as i64;
    let second = ((time & 0x1f) * 2) as i64;
    let minute = ((time >> 5) & 0x3f) as i64;
    let hour = ((time >> 11) & 0x1f) as i64;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`, the inverse of the `civil_from_days` in
/// [`crate::ops::trash`]. Shifting the year to start in March makes the leap day
/// the last day of the year, which is what removes every special case.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn read_u16(b: &[u8], at: usize) -> Result<u16, ArchiveError> {
    let end = at
        .checked_add(2)
        .ok_or_else(|| malformed("offset overflow"))?;
    if end > b.len() {
        return Err(ArchiveError::Truncated {
            format: "zip",
            at: at as u64,
            want: 2,
        });
    }
    Ok(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn read_u32(b: &[u8], at: usize) -> Result<u32, ArchiveError> {
    let end = at
        .checked_add(4)
        .ok_or_else(|| malformed("offset overflow"))?;
    if end > b.len() {
        return Err(ArchiveError::Truncated {
            format: "zip",
            at: at as u64,
            want: 4,
        });
    }
    Ok(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

fn read_u64(b: &[u8], at: usize) -> Result<u64, ArchiveError> {
    let end = at
        .checked_add(8)
        .ok_or_else(|| malformed("offset overflow"))?;
    if end > b.len() {
        return Err(ArchiveError::Truncated {
            format: "zip",
            at: at as u64,
            want: 8,
        });
    }
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..end]);
    Ok(u64::from_le_bytes(v))
}
