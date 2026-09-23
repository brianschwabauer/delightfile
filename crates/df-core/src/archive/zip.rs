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

/// General purpose bit 0: the member is encrypted.
const FLAG_ENCRYPTED: u16 = 1 << 0;
/// General purpose bit 11: the name is UTF-8.
const FLAG_UTF8: u16 = 1 << 11;

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
    let (records, truncated) = central(reader, len)?;
    Ok((records.into_iter().map(|r| r.entry).collect(), truncated))
}

/// One central-directory record: the row the listing shows, and where the
/// member's bytes are.
struct Record {
    entry: RawEntry,
    /// The absolute offset of the member's local header — the one thing a
    /// local header is still read for (see the extraction essay below).
    local_at: u64,
}

/// Read the central directory: every member, in the directory's order.
///
/// The one parse of the index, shared by listing, extraction and previews, so
/// a member's name, sizes and flags are the same numbers whichever of the
/// three is asking.
fn central<R: Read + Seek>(reader: &mut R, len: u64) -> Result<(Vec<Record>, bool), ArchiveError> {
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
        let mut local_at = read_u32(h, 42)? as u64;

        let name_at = at + CENTRAL_FIXED;
        let extra_at = name_at + name_len;
        let end = extra_at + extra_len + comment_len;
        if end > central.len() {
            truncated = true;
            break;
        }
        let name_bytes = &central[name_at..extra_at];
        let extra = &central[extra_at..extra_at + extra_len];

        let utf8_flag = flags & FLAG_UTF8 != 0;
        let name = decode_name(name_bytes, utf8_flag);
        let mut mtime = dos_datetime(dos_date, dos_time);

        if let Some(unix) = extended_timestamp(extra) {
            // A real unix timestamp beats DOS's two-second, no-timezone
            // approximation whenever a writer bothered to include one.
            mtime = Some(unix);
        }
        if [uncompressed, compressed, local_at].contains(&(ZIP64_MARK32 as u64)) {
            if let Some((u, c, l)) = zip64_extra(extra, uncompressed, compressed, local_at) {
                uncompressed = u;
                compressed = c;
                local_at = l;
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

        entries.push(Record {
            entry: RawEntry {
                name,
                len: uncompressed,
                compressed,
                mtime,
                is_dir,
                method: Method::from_zip(method),
                // Bit 0 is "this entry is encrypted". Listing still works:
                // names and sizes live in the central directory, which is
                // never encrypted.
                encrypted: flags & FLAG_ENCRYPTED != 0,
                link_target: None,
            },
            local_at,
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
    // Checked rather than added: the offset is a `u64` straight off the wire, so
    // the *bounds check itself* is where an archive gets to overflow.
    let end = at
        .checked_add(56)
        .ok_or_else(|| malformed("zip64 locator points past the end of the file"))?;
    if end > tail_at + tail.len() as u64 {
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

/// Walk the extra-field list for the zip64 values (header id 0x0001):
/// `(uncompressed, compressed, local header offset)`.
///
/// The zip64 extra is positional and *only contains the fields that saturated*,
/// in a fixed order: uncompressed, compressed, local header offset, disk. So
/// which 8 bytes to read depends on which 32-bit fields were all-ones.
fn zip64_extra(
    extra: &[u8],
    uncompressed: u64,
    compressed: u64,
    local_at: u64,
) -> Option<(u64, u64, u64)> {
    for (id, data) in ExtraFields(extra) {
        if id != 0x0001 {
            continue;
        }
        let mut at = 0usize;
        // Each field in turn: its own value when it did not saturate, the next
        // eight bytes of the extra when it did.
        let mut next = |value: u64| -> Option<u64> {
            if value != ZIP64_MARK32 as u64 {
                return Some(value);
            }
            let wide = read_u64(data, at).ok()?;
            at += 8;
            Some(wide)
        };
        let u = next(uncompressed)?;
        let c = next(compressed)?;
        // A short extra still has good sizes in it for the listing; the
        // offset it failed to carry stays saturated, and the reader finds no
        // header there and refuses that one member.
        let l = next(local_at).unwrap_or(local_at);
        return Some((u, c, l));
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

// ── Extraction ──────────────────────────────────────────────────────────────
//
// Extraction reads the same index the listing does. The central directory
// carries, for every member, the real compressed and uncompressed sizes and
// the offset of its local header — so a member is found by seeking to that
// header, stepping over it, and reading exactly the compressed length the
// index declared. A member's size is never taken from its local header.
//
// This used to be a walk over the local headers from byte zero, which meant
// believing them. A writer that streams — a web app zipping files on the fly
// into a download, macOS's Archive Utility — does not know a member's size when
// it writes the header, so it writes zeroes, sets general-purpose bit 3 and
// puts the real sizes in a data descriptor *after* the payload. A deflated
// member survives that, because the deflate stream says where it ends; a
// *stored* one does not, because nothing in its bytes does, and the walk had
// to refuse the whole archive. Every such zip still ends in a central
// directory with the right numbers in it — written last, when the sizes were
// known — and that directory is what every other zip tool believes. So it is
// what this believes.
//
// The local header is read for two things only:
//
// - Its own name and extra-field lengths, which are what say where the payload
//   starts. They need not match the central copy's (writers routinely put a
//   different extra field in each), and only the local ones describe the bytes
//   that are actually there.
// - Its encryption bit, as a second opinion: a member the index calls plain but
//   whose own header says encrypted is refused, not written out as ciphertext.
//
// And, once more, because it is the property the whole feature rests on: a
// member's name is used **only** as a key into the destinations the plan
// already settled. Nothing here joins it onto a path.

/// The local file header signature, `PK\x03\x04`.
const LOCAL_SIG: &[u8] = b"PK\x03\x04";
/// The fixed part of a local file header, before the name.
const LOCAL_FIXED: usize = 30;

/// How much compressed data is read at a time while inflating.
const IN_BUF: usize = 64 * 1024;

/// Write the members the plan asked for.
pub(crate) fn extract_into<R: Read + Seek>(
    reader: &mut R,
    sink: &mut super::unpack::Sink<'_>,
) -> crate::Result<()> {
    let (members, len) = members(reader)?;
    for member in &members {
        sink.checkpoint()?;
        let entry = &member.entry;
        if entry.is_dir {
            // Directories were created from the plan before this started.
            continue;
        }
        if !sink.wants(&entry.name) {
            continue;
        }
        if entry.encrypted {
            // The plan already refused these; this is the belt to its braces.
            sink.refuse(&entry.name, "encrypted");
            continue;
        }
        let Some(method) = supported(entry.method) else {
            sink.refuse(
                &entry.name,
                format!("{} compression is not supported", entry.method.label()),
            );
            continue;
        };
        let Some(local) = local_header(reader, member.local_at, len)? else {
            sink.refuse(
                &entry.name,
                "the archive's index points at a member that is not there",
            );
            continue;
        };
        if local.encrypted {
            sink.refuse(&entry.name, "encrypted");
            continue;
        }
        // The central directory's length is the one the bomb guard holds the
        // entry to — see the essay in `unpack`.
        if !sink.open(&entry.name, entry.len) {
            continue;
        }
        let result = payload(
            reader,
            method,
            local.data_at,
            entry.compressed,
            &mut |chunk| sink.write(chunk),
        );
        // Only a payload that ran to its end is a finished file. An error here
        // is a cancel or a corrupt stream, and closing on it would count the
        // half-written entry as extracted *and* leave it on disk — `finish` has
        // to be the one that finds it still open, so it can remove it.
        if result.is_ok() {
            sink.close();
        }
        result?;
    }
    Ok(())
}

/// Find one member and hand its bytes back whole (the preview path).
///
/// What a preview costs is the central directory plus the one member: the
/// index says where the member is, so nothing before it is read.
pub(crate) fn read_one<R: Read + Seek>(
    reader: &mut R,
    buffer: &mut super::unpack::Buffer<'_>,
) -> crate::Result<()> {
    let (members, len) = members(reader)?;
    for member in &members {
        if buffer.done() {
            break;
        }
        let entry = &member.entry;
        if entry.is_dir || entry.encrypted || !buffer.wants(&entry.name) {
            continue;
        }
        let Some(method) = supported(entry.method) else {
            continue;
        };
        if entry.len > buffer.limit() as u64 {
            continue;
        }
        let Some(local) = local_header(reader, member.local_at, len)? else {
            continue;
        };
        if local.encrypted {
            continue;
        }
        // Held to the declared length, the same rule the extractor's bomb
        // guard applies: a member that inflates past what the index said it
        // weighs is not the member the listing described, and the preview of
        // it is no preview at all.
        let declared = entry.len as usize;
        let mut out = Vec::with_capacity(declared);
        let mut overran = false;
        payload(
            reader,
            method,
            local.data_at,
            entry.compressed,
            &mut |chunk| {
                if out.len() + chunk.len() > declared {
                    overran = true;
                    return Ok(false);
                }
                out.extend_from_slice(chunk);
                Ok(true)
            },
        )?;
        if !overran {
            buffer.take(out);
        }
    }
    Ok(())
}

/// Every member in the central directory, in the order their bytes sit in the
/// file, and the file's length.
///
/// The index's order and the file's are almost always the same; sorting makes
/// it always, so a big extraction reads the archive front to back, which is
/// the order a spinning disk wants.
fn members<R: Read + Seek>(reader: &mut R) -> crate::Result<(Vec<Record>, u64)> {
    let len = reader
        .seek(SeekFrom::End(0))
        .map_err(|e| crate::DfError::Op(format!("zip: {e}")))?;
    // `truncated` is not consulted: the plan was made from a listing that hit
    // the same cap, so nothing past it was asked for.
    let (mut records, _truncated) = central(reader, len)?;
    records.sort_by_key(|record| record.local_at);
    Ok((records, len))
}

/// The deflate methods this build can actually decompress.
fn supported(method: Method) -> Option<Method> {
    matches!(method, Method::Store | Method::Deflate).then_some(method)
}

/// What a member's own local header adds to the central directory's record.
struct Local {
    /// Where the payload starts: after the header and the header's *own* name
    /// and extra field.
    data_at: u64,
    encrypted: bool,
}

/// Read the local header the central directory points at, or `None` when
/// there is not one there.
fn local_header<R: Read + Seek>(reader: &mut R, at: u64, len: u64) -> crate::Result<Option<Local>> {
    // The offset is a number in the file — up to sixteen exabytes through a
    // zip64 extra — so it is checked against the file before it is sought to,
    // and checked with a checked add, because the bounds check is exactly where
    // an archive gets to overflow.
    if at
        .checked_add(LOCAL_FIXED as u64)
        .is_none_or(|end| end > len)
    {
        return Ok(None);
    }
    reader
        .seek(SeekFrom::Start(at))
        .map_err(|e| crate::DfError::Op(format!("zip: seek to {at}: {e}")))?;
    let mut fixed = [0u8; LOCAL_FIXED];
    let got = super::unpack::read_full(reader, &mut fixed)
        .map_err(|e| crate::DfError::Op(format!("zip: {e}")))?;
    if got < LOCAL_FIXED || fixed[..4] != *LOCAL_SIG {
        return Ok(None);
    }
    let flags = u16::from_le_bytes([fixed[6], fixed[7]]);
    let name_len = u16::from_le_bytes([fixed[26], fixed[27]]) as u64;
    let extra_len = u16::from_le_bytes([fixed[28], fixed[29]]) as u64;
    // Cannot overflow: `at + 30` is at most the file's length, and a file's
    // length leaves far more than two `u16`s of headroom in a `u64`.
    let data_at = at + LOCAL_FIXED as u64 + name_len + extra_len;
    Ok(Some(Local {
        data_at,
        encrypted: flags & FLAG_ENCRYPTED != 0,
    }))
}

/// Read one member's payload from `at`, decompressing it, handing whole chunks
/// to `emit`.
///
/// Reads at most `compressed` bytes — the central directory's number. Stops
/// early when `emit` answers `false` (the entry overran its declared length,
/// or its file could not be written: going on would only inflate bytes nobody
/// keeps), when a deflate stream ends, and when the file does; a member cut
/// short keeps what it had, and the caller's length check says it is short.
fn payload<R: Read + Seek>(
    reader: &mut R,
    method: Method,
    at: u64,
    compressed: u64,
    emit: &mut dyn FnMut(&[u8]) -> crate::Result<bool>,
) -> crate::Result<()> {
    reader
        .seek(SeekFrom::Start(at))
        .map_err(|e| crate::DfError::Op(format!("zip: seek to {at}: {e}")))?;
    let mut left = compressed;

    if matches!(method, Method::Store) {
        // Stored: the payload *is* the contents.
        let mut buf = vec![0u8; IN_BUF];
        while left > 0 {
            let want = (buf.len() as u64).min(left) as usize;
            let got = super::unpack::read_full(reader, &mut buf[..want])
                .map_err(|e| crate::DfError::Op(format!("zip: {e}")))?;
            if got == 0 {
                break;
            }
            left -= got as u64;
            if !emit(&buf[..got])? {
                break;
            }
        }
        return Ok(());
    }

    use miniz_oxide::inflate::stream::{inflate, InflateState};
    use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

    // `Raw`: a zip member is a bare deflate stream with no zlib header and no
    // adler32 after it. Boxed because the state is a 32 KiB window plus the
    // huffman tables, and a stack frame is not where that belongs.
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut input = vec![0u8; IN_BUF];
    let mut output = vec![0u8; super::unpack::EXTRACT_BUF];
    let mut filled = 0usize;
    let mut used = 0usize;

    loop {
        if used == filled && left > 0 {
            let want = (input.len() as u64).min(left) as usize;
            filled = super::unpack::read_full(reader, &mut input[..want])
                .map_err(|e| crate::DfError::Op(format!("zip: {e}")))?;
            used = 0;
            // A file that ends before the index said it would ends the input.
            left = if filled == 0 { 0 } else { left - filled as u64 };
        }
        // Called even with nothing left to give it: the inflater keeps up to a
        // window's worth of output it had no room for, and only another call
        // hands that over. Stopping when the input ran out would cut the tail
        // off any member whose last chunk decompressed into a full buffer.
        let exhausted = used == filled;
        let result = inflate(&mut state, &input[used..filled], &mut output, MZFlush::None);
        used += result.bytes_consumed;
        if result.bytes_written > 0 && !emit(&output[..result.bytes_written])? {
            return Ok(());
        }
        match result.status {
            Ok(MZStatus::StreamEnd) => return Ok(()),
            Ok(_) => {}
            // It wants more and there is no more: a truncated member. What was
            // decompressed stands; the caller records the shortfall.
            Err(MZError::Buf) if exhausted => return Ok(()),
            Err(e) => {
                return Err(crate::DfError::Op(format!(
                    "zip: the compressed data is corrupt ({e:?})"
                )))
            }
        }
        if result.bytes_consumed == 0 && result.bytes_written == 0 {
            // No progress and no error: nothing more will come of it.
            return Ok(());
        }
    }
}
