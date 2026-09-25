//! Zip, written front to back and patched once per member.
//!
//! The reader's essay ([`crate::archive::zip`]) says the central directory is
//! the only authoritative listing, and a writer that believes that writes it
//! last and gets it right. So each member is a local header, its data, and a
//! note of what to say about it at the end; the notes become the central
//! directory once every member is in.
//!
//! ## Sizes without data descriptors
//!
//! A local header carries the CRC and both sizes, which are not known until
//! the data has been through the compressor. The streaming writers' answer is
//! general-purpose bit 3 — zeroes in the header, the real numbers in a data
//! descriptor after the payload — and that answer is exactly what the reader's
//! essay spends a paragraph on: a *stored* member written that way cannot be
//! walked, because nothing in its bytes says where it ends. This writer has a
//! file on the disk, not a pipe, so it writes the header with the numbers left
//! blank, streams the data, and seeks back to fill them in. Bit 3 stays clear,
//! every local header tells the truth, and memory stays at one chunk however
//! big the member is.
//!
//! ## Stored or deflated
//!
//! Deflate buys nothing on a JPEG, an MP4 or a zip — they are compressed
//! already — and costs a CPU core running flat out to find that out. Those
//! members are stored ([`stores`]), which on a folder of photos is the
//! difference between an archive that is as fast as the disk and one that is
//! as fast as miniz.
//!
//! ## Zip64
//!
//! The 16- and 32-bit fields saturate at 65,535 entries and 4 GiB. Past them a
//! field is written as all-ones and its real value goes in a zip64 extra
//! (per member) or the zip64 end records (for the archive), which is what the
//! reader looks for. The limits are a [`Limits`] rather than two constants so
//! a test can lower them and exercise every zip64 path without writing four
//! gigabytes.
//!
//! A member's *local* header has to decide before its data is written whether
//! it will need a zip64 extra, because the header cannot grow afterwards. It
//! reserves one for anything at least half the limit: deflate can expand
//! incompressible data slightly, and a file can grow while it is being read,
//! and a 2 GiB member carrying an extra it did not need costs twenty bytes.
//!
//! ## What else goes in
//!
//! - General-purpose bit 11 on every name that is UTF-8, which is every name
//!   a person typed. A name that is not (a file from a Latin-1 machine) goes
//!   in as its bytes with the bit clear — what Info-ZIP does.
//! - "Version made by" Unix, and the full `st_mode` in the high half of the
//!   external attributes, so modes come back on extraction and a symlink is a
//!   symlink: `S_IFLNK` in the mode, the target as the member's data.
//! - The extended-timestamp extra (0x5455), so a modification time keeps its
//!   seconds. The DOS time every header also carries has a two-second grain
//!   and no idea what year 1979 was.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::crc32::{crc32, Crc32};
use super::deflate::Deflater;
use super::{read_chunk, Member, What};
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

/// The two field widths that saturate.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    /// A 32-bit size or offset at or past this is written through zip64.
    pub(super) wide: u64,
    /// An entry count at or past this is written through zip64.
    pub(super) count: u64,
}

/// The real limits: `0xFFFF_FFFF` and `0xFFFF` are themselves the "look in
/// zip64" marks, so a value *equal* to either already needs the wide form.
pub(super) const LIMITS: Limits = Limits {
    wide: u32::MAX as u64,
    count: u16::MAX as u64,
};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;

/// General purpose bit 11: the name is UTF-8.
const FLAG_UTF8: u16 = 1 << 11;
const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
/// "Version made by": Unix (3) in the high byte — the one host whose external
/// attributes carry an `st_mode` — and spec 6.3 in the low.
const MADE_BY: u16 = (3 << 8) | 63;

const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;
/// The MS-DOS directory attribute, for the readers that look at nothing else.
const DOS_DIR: u32 = 0x10;

/// Whether a file's bytes are already compressed, by what its name says it
/// is — [`crate::fs::FileKind`], the same classification the icon column
/// draws from.
///
/// Pictures, video, audio and archives, less the handful of each that are
/// *not* compressed inside: an SVG is text, a BMP or a TIFF is raw pixels, a
/// WAV raw samples, a tar a concatenation of whatever went into it. Deflate
/// halves those, so they are deflated like anything else.
pub(super) fn stores(name: &str) -> bool {
    use crate::fs::FileKind;
    const RAW: &[&str] = &[
        "svg", "svgz", "bmp", "tif", "tiff", "pnm", "ppm", "pgm", "pbm", "tga", "wav", "aif",
        "aiff", "tar",
    ];
    let extension = crate::fs::extension_of(name).map(|e| e.to_ascii_lowercase());
    if extension.as_deref().is_some_and(|e| RAW.contains(&e)) {
        return false;
    }
    let mime = crate::fs::mime::hint_for_name(name);
    matches!(
        crate::fs::kind_for_name(name, mime, 0),
        FileKind::Image | FileKind::Video | FileKind::Audio | FileKind::Archive
    )
}

/// Where a local header's unknowns are: the CRC (and the 32-bit sizes after
/// it), and the zip64 extra's two sizes when the header has one.
struct Blanks {
    crc_at: u64,
    sizes64_at: Option<u64>,
}

/// What the central directory will say about one member.
struct Central {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    compressed: u64,
    len: u64,
    offset: u64,
    external: u32,
    /// The extended timestamp's 32 bits, when the time has a spelling in
    /// them ([`timestamp_bits`]).
    mtime: Option<u32>,
    /// The local header carries a zip64 extra, so this entry does too: the
    /// two headers agree on the version they need and on where the sizes are.
    wide: bool,
}

/// A zip being written. [`ZipWriter::add`] each member, then
/// [`ZipWriter::finish`].
pub(super) struct ZipWriter<'a, W: Write + Seek> {
    out: W,
    /// Where the next byte goes. Counted rather than asked for, so writing a
    /// header is not a seek.
    at: u64,
    central: Vec<Central>,
    limits: Limits,
    deflater: Option<Deflater>,
    buf: Vec<u8>,
    /// Bytes of file contents read so far — what went in, whatever the walk
    /// expected.
    read: u64,
    /// The archive, for the error messages.
    archive: &'a Path,
}

impl<'a, W: Write + Seek> ZipWriter<'a, W> {
    pub(super) fn new(out: W, archive: &'a Path, limits: Limits) -> ZipWriter<'a, W> {
        ZipWriter {
            out,
            at: 0,
            central: Vec::new(),
            limits,
            deflater: None,
            buf: Vec::new(),
            read: 0,
            archive,
        }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        self.out
            .write_all(bytes)
            .map_err(|e| DfError::io(self.archive, e))?;
        self.at += bytes.len() as u64;
        Ok(())
    }

    /// Write one member: a directory, a link or a file's whole contents.
    pub(super) fn add(&mut self, member: &Member, ctx: &TaskCtx) -> Result<()> {
        ctx.checkpoint()?;
        let flags = if std::str::from_utf8(&member.name).is_ok() {
            FLAG_UTF8
        } else {
            0
        };
        let (dos_time, dos_date) = dos_datetime(member.mtime);
        let mut entry = Central {
            name: member.name.clone(),
            flags,
            method: METHOD_STORE,
            dos_time,
            dos_date,
            crc: 0,
            compressed: 0,
            len: 0,
            offset: self.at,
            external: 0,
            mtime: timestamp_bits(member.mtime),
            wide: false,
        };
        let perms = member.mode & 0o7777;

        match &member.what {
            What::Dir => {
                entry.external = ((S_IFDIR | perms) << 16) | DOS_DIR;
                self.local(&entry, false)?;
            }
            What::Link(target) => {
                // Info-ZIP's shape: stored, and the data is the link text.
                entry.external = (S_IFLNK | perms) << 16;
                entry.crc = crc32(target);
                entry.len = target.len() as u64;
                entry.compressed = entry.len;
                self.local(&entry, false)?;
                self.put(target)?;
            }
            What::File(expected) => {
                entry.external = (S_IFREG | perms) << 16;
                if !member.stored {
                    entry.method = METHOD_DEFLATE;
                }
                let wide = *expected >= self.limits.wide / 2;
                entry.wide = wide;
                let blanks = self.local(&entry, wide)?;
                let data_at = self.at;
                let (crc, len) = self.data(member, entry.method, ctx)?;
                entry.crc = crc;
                entry.len = len;
                entry.compressed = self.at - data_at;
                if !wide && (entry.len >= self.limits.wide || entry.compressed >= self.limits.wide)
                {
                    return Err(DfError::Op(format!(
                        "{} grew past 4 GB while it was being archived",
                        member.source.display()
                    )));
                }
                self.patch(blanks, &entry)?;
            }
        }
        self.central.push(entry);
        ctx.advance(0, 1);
        Ok(())
    }

    /// Write a local header for `entry` as it stands, returning where its
    /// blanks are so the numbers can be filled in once they are known. With
    /// `wide`, the 32-bit sizes are the zip64 mark and the real ones go in a
    /// zip64 extra — both of them, as the format requires of a local header.
    fn local(&mut self, entry: &Central, wide: bool) -> Result<Blanks> {
        let start = self.at;
        let mut extra = Vec::new();
        if let Some(mtime) = entry.mtime {
            timestamp_extra(&mut extra, mtime);
        }
        let mut sizes64_at = None;
        if wide {
            // Thirty fixed bytes, the name, the extras so far, and this
            // extra's own four-byte id and length.
            sizes64_at = Some(start + 30 + entry.name.len() as u64 + extra.len() as u64 + 4);
            extra.extend_from_slice(&1u16.to_le_bytes());
            extra.extend_from_slice(&16u16.to_le_bytes());
            extra.extend_from_slice(&entry.len.to_le_bytes());
            extra.extend_from_slice(&entry.compressed.to_le_bytes());
        }
        let (compressed, len) = if wide {
            (u32::MAX, u32::MAX)
        } else {
            (entry.compressed as u32, entry.len as u32)
        };
        let mut h = Vec::with_capacity(30 + entry.name.len() + extra.len());
        h.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        h.extend_from_slice(&needed(entry, wide).to_le_bytes());
        h.extend_from_slice(&entry.flags.to_le_bytes());
        h.extend_from_slice(&entry.method.to_le_bytes());
        h.extend_from_slice(&entry.dos_time.to_le_bytes());
        h.extend_from_slice(&entry.dos_date.to_le_bytes());
        h.extend_from_slice(&entry.crc.to_le_bytes());
        h.extend_from_slice(&compressed.to_le_bytes());
        h.extend_from_slice(&len.to_le_bytes());
        h.extend_from_slice(&len16(entry.name.len(), &entry.name)?.to_le_bytes());
        h.extend_from_slice(&len16(extra.len(), &entry.name)?.to_le_bytes());
        h.extend_from_slice(&entry.name);
        h.extend_from_slice(&extra);
        self.put(&h)?;
        Ok(Blanks {
            crc_at: start + 14,
            sizes64_at,
        })
    }

    /// Fill in a local header's CRC and sizes, and come back to the end.
    fn patch(&mut self, blanks: Blanks, entry: &Central) -> Result<()> {
        let end = self.at;
        let mut fixed = entry.crc.to_le_bytes().to_vec();
        if blanks.sizes64_at.is_none() {
            fixed.extend_from_slice(&(entry.compressed as u32).to_le_bytes());
            fixed.extend_from_slice(&(entry.len as u32).to_le_bytes());
        }
        self.overwrite(blanks.crc_at, &fixed)?;
        if let Some(at) = blanks.sizes64_at {
            let mut sizes = entry.len.to_le_bytes().to_vec();
            sizes.extend_from_slice(&entry.compressed.to_le_bytes());
            self.overwrite(at, &sizes)?;
        }
        self.out
            .seek(SeekFrom::Start(end))
            .map_err(|e| DfError::io(self.archive, e))?;
        Ok(())
    }

    fn overwrite(&mut self, at: u64, bytes: &[u8]) -> Result<()> {
        self.out
            .seek(SeekFrom::Start(at))
            .and_then(|_| self.out.write_all(bytes))
            .map_err(|e| DfError::io(self.archive, e))
    }

    /// Stream a file's contents: stored or deflated, the CRC over what was
    /// read. Returns the CRC and the length actually read, which is the length
    /// the headers will declare — a file that changed size since the walk is
    /// archived as it is now, not as it was.
    fn data(&mut self, member: &Member, method: u16, ctx: &TaskCtx) -> Result<(u32, u64)> {
        let source = &member.source;
        let mut file = File::open(source).map_err(|e| DfError::io(source, e))?;
        if self.buf.is_empty() {
            self.buf = vec![0u8; crate::ops::COPY_CHUNK];
        }
        let mut crc = Crc32::new();
        let mut len = 0u64;
        let deflating = method == METHOD_DEFLATE;
        if deflating {
            self.deflater.get_or_insert_with(Deflater::new).reset();
        }
        loop {
            ctx.checkpoint()?;
            let n = read_chunk(&mut file, &mut self.buf).map_err(|e| DfError::io(source, e))?;
            let last = n < self.buf.len();
            crc.update(&self.buf[..n]);
            len += n as u64;
            self.read += n as u64;
            if deflating {
                let ZipWriter {
                    out,
                    at,
                    deflater,
                    buf,
                    archive,
                    ..
                } = self;
                let Some(deflater) = deflater.as_mut() else {
                    return Err(DfError::Op("the compressor went missing".to_string()));
                };
                deflater
                    .feed(&buf[..n], last, &mut |chunk| {
                        out.write_all(chunk)?;
                        *at += chunk.len() as u64;
                        Ok(())
                    })
                    .map_err(|e| DfError::io(*archive, e))?;
            } else if n > 0 {
                let ZipWriter {
                    out,
                    at,
                    buf,
                    archive,
                    ..
                } = self;
                out.write_all(&buf[..n])
                    .map_err(|e| DfError::io(*archive, e))?;
                *at += n as u64;
            }
            ctx.advance(n as u64, 0);
            if last {
                break;
            }
        }
        Ok((crc.finish(), len))
    }

    /// The central directory and the end records. Hands the writer back so
    /// its owner can flush it and see the flush fail, with the bytes of file
    /// contents that went in.
    pub(super) fn finish(mut self) -> Result<(W, u64)> {
        let cd_start = self.at;
        let central = std::mem::take(&mut self.central);
        for entry in &central {
            let h = self.central_header(entry)?;
            self.put(&h)?;
        }
        let cd_size = self.at - cd_start;
        let count = central.len() as u64;
        let limits = self.limits;
        let wide_end = count >= limits.count || cd_size >= limits.wide || cd_start >= limits.wide;

        if wide_end {
            let record_at = self.at;
            let mut r = Vec::with_capacity(56 + 20);
            r.extend_from_slice(&EOCD64_SIG.to_le_bytes());
            // The size of what follows this field: 56 - 12.
            r.extend_from_slice(&44u64.to_le_bytes());
            r.extend_from_slice(&MADE_BY.to_le_bytes());
            r.extend_from_slice(&45u16.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&cd_size.to_le_bytes());
            r.extend_from_slice(&cd_start.to_le_bytes());
            // The locator: where the record above is.
            r.extend_from_slice(&EOCD64_LOCATOR_SIG.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&record_at.to_le_bytes());
            r.extend_from_slice(&1u32.to_le_bytes());
            self.put(&r)?;
        }

        let count16 = if count >= limits.count {
            u16::MAX
        } else {
            count as u16
        };
        let narrow = |value: u64| {
            if value >= limits.wide {
                u32::MAX
            } else {
                value as u32
            }
        };
        let mut e = Vec::with_capacity(22);
        e.extend_from_slice(&EOCD_SIG.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        e.extend_from_slice(&count16.to_le_bytes());
        e.extend_from_slice(&count16.to_le_bytes());
        e.extend_from_slice(&narrow(cd_size).to_le_bytes());
        e.extend_from_slice(&narrow(cd_start).to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        self.put(&e)?;
        Ok((self.out, self.read))
    }

    fn central_header(&self, entry: &Central) -> Result<Vec<u8>> {
        let limits = self.limits;
        // The zip64 extra holds only the fields that saturated, in this fixed
        // order — the positional rule the reader's `zip64_extra` follows. The
        // sizes also go through it whenever the local header's did, so both
        // headers of one member say 4.5 and carry the extra, or neither does.
        let mut wide_values = Vec::new();
        let mut narrow = |value: u64, force: bool| -> u32 {
            if force || value >= limits.wide {
                wide_values.extend_from_slice(&value.to_le_bytes());
                u32::MAX
            } else {
                value as u32
            }
        };
        let len = narrow(entry.len, entry.wide);
        let compressed = narrow(entry.compressed, entry.wide);
        let offset = narrow(entry.offset, false);
        let wide = !wide_values.is_empty();

        let mut extra = Vec::new();
        if let Some(mtime) = entry.mtime {
            timestamp_extra(&mut extra, mtime);
        }
        if wide {
            extra.extend_from_slice(&1u16.to_le_bytes());
            extra.extend_from_slice(&(wide_values.len() as u16).to_le_bytes());
            extra.extend_from_slice(&wide_values);
        }

        let mut h = Vec::with_capacity(46 + entry.name.len() + extra.len());
        h.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        h.extend_from_slice(&MADE_BY.to_le_bytes());
        h.extend_from_slice(&needed(entry, wide).to_le_bytes());
        h.extend_from_slice(&entry.flags.to_le_bytes());
        h.extend_from_slice(&entry.method.to_le_bytes());
        h.extend_from_slice(&entry.dos_time.to_le_bytes());
        h.extend_from_slice(&entry.dos_date.to_le_bytes());
        h.extend_from_slice(&entry.crc.to_le_bytes());
        h.extend_from_slice(&compressed.to_le_bytes());
        h.extend_from_slice(&len.to_le_bytes());
        h.extend_from_slice(&len16(entry.name.len(), &entry.name)?.to_le_bytes());
        h.extend_from_slice(&len16(extra.len(), &entry.name)?.to_le_bytes());
        // Comment length, disk number start, internal attributes.
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&entry.external.to_le_bytes());
        h.extend_from_slice(&offset.to_le_bytes());
        h.extend_from_slice(&entry.name);
        h.extend_from_slice(&extra);
        Ok(h)
    }
}

/// A name's or an extra field's length, which the headers hold in 16 bits.
/// Past that the entry cannot be written, and saying so beats a length that
/// wrapped and a header that points into the middle of its own name.
fn len16(len: usize, name: &[u8]) -> Result<u16> {
    u16::try_from(len).map_err(|_| {
        DfError::Op(format!(
            "{}…: {len} bytes is too long for a name in a zip",
            String::from_utf8_lossy(&name[..name.len().min(64)])
        ))
    })
}

/// The "version needed to extract" (APPNOTE 4.4.3.2): 4.5 for anything with
/// a zip64 field, 2.0 for deflate and for a directory, 1.0 for a stored file
/// or link.
fn needed(entry: &Central, wide: bool) -> u16 {
    if wide {
        45
    } else if entry.method == METHOD_DEFLATE || entry.name.ends_with(b"/") {
        20
    } else {
        10
    }
}

/// The extended timestamp's 32 bits for `mtime`.
///
/// The field is 32 bits and the spec says signed, which ends in 2038; the
/// readers that have thought about it (libarchive, 7-Zip, Info-ZIP with a
/// 64-bit `time_t`) read it unsigned, which runs to 2106. So a time from 1970
/// to 2106 goes in as its low 32 bits, one before 1970 as the signed value
/// (back to 1901), and only a time outside both is left with the DOS time
/// alone — which readers take as local, and which is why the extra is kept
/// wherever it can be.
fn timestamp_bits(mtime: i64) -> Option<u32> {
    match i32::try_from(mtime) {
        Ok(signed) => Some(signed as u32),
        Err(_) => u32::try_from(mtime).ok(),
    }
}

/// The extended-timestamp extra with the modification time only: flags bit 0,
/// then the seconds. The central and local forms are the same when mtime is
/// all they carry.
fn timestamp_extra(out: &mut Vec<u8>, mtime: u32) {
    out.extend_from_slice(&0x5455u16.to_le_bytes());
    out.extend_from_slice(&5u16.to_le_bytes());
    out.push(1);
    out.extend_from_slice(&mtime.to_le_bytes());
}

/// Unix seconds to MS-DOS `(time, date)`, as UTC — the reader's own
/// assumption, so a zip read back here says the same thing it was told.
///
/// DOS dates run from 1980 to 2107; a time outside that is pinned to the
/// nearest end rather than wrapped into a wrong year. The extended timestamp
/// carries the real one.
fn dos_datetime(mtime: i64) -> (u16, u16) {
    let days = mtime.div_euclid(86_400);
    let secs = mtime.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    if year < 1980 {
        return (0, (1 << 5) | 1);
    }
    if year > 2107 {
        return ((23 << 11) | (59 << 5) | 29, (127 << 9) | (12 << 5) | 31);
    }
    let (hour, minute, second) = (secs / 3600, (secs / 60) % 60, secs % 60);
    let time = ((hour as u16) << 11) | ((minute as u16) << 5) | (second as u16 / 2);
    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    (time, date)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a proleptic
/// Gregorian `(year, month, day)`. The inverse of the reader's
/// `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_time_is_the_utc_wall_clock_at_two_second_grain() {
        // 2024-02-29 13:45:17 UTC.
        let (time, date) = dos_datetime(1_709_214_317);
        assert_eq!(date >> 9, 2024 - 1980);
        assert_eq!((date >> 5) & 0x0f, 2);
        assert_eq!(date & 0x1f, 29);
        assert_eq!(time >> 11, 13);
        assert_eq!((time >> 5) & 0x3f, 45);
        assert_eq!((time & 0x1f) * 2, 16);
        // Before the DOS epoch: pinned, not wrapped.
        assert_eq!(dos_datetime(0), (0, (1 << 5) | 1));
    }

    #[test]
    fn already_compressed_kinds_are_stored() {
        for name in [
            "IMG_0001.jpg",
            "clip.MP4",
            "song.flac",
            "nested.zip",
            "a.tar.gz",
        ] {
            assert!(stores(name), "{name}");
        }
        for name in [
            "notes.txt",
            "main.rs",
            "drawing.svg",
            "scan.tiff",
            "raw.wav",
            "b.tar",
            "Makefile",
        ] {
            assert!(!stores(name), "{name}");
        }
    }
}
