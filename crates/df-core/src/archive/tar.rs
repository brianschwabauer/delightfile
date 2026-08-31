//! Tar, read forwards.
//!
//! A tar is 512-byte header blocks, each followed by the file's bytes padded to
//! the next 512. There is no index — the only way to list one is to walk it — and
//! that is why this parser is written over a plain [`Read`] rather than a
//! `Seek`: the same code lists a `.tar` from a file and a `.tar.zst` from the
//! stdout of `zstd -dc`, and the compressed case is the common one.
//!
//! ## Three formats wearing one header
//!
//! The 512-byte header has 100 bytes for a name, which stopped being enough in
//! about 1988. Three answers exist and all three are in the wild:
//!
//! - **ustar** (POSIX.1-1988): a 155-byte `prefix` field. The real name is
//!   `prefix/name`, and the split has to happen on a directory boundary, so a
//!   single 200-character *filename* still does not fit.
//! - **GNU** (`L` and `K` type flags): a pseudo-entry whose *contents* are the
//!   long name, applying to the entry that follows it. `K` does the same for a
//!   link target.
//! - **pax** (POSIX.1-2001, `x` type flag): a pseudo-entry whose contents are
//!   `length key=value\n` records. `path` overrides the name, `size` overrides
//!   the size (which is how tar stores files over 8 GiB), `mtime` gives
//!   sub-second precision, `linkpath` overrides the link target.
//!
//! All three are read, and pax wins over GNU wins over ustar when an archive
//! somehow contains more than one — which `bsdtar` will do, since it writes a
//! pax header and a compatible ustar header for the same entry.
//!
//! ## Numbers
//!
//! Sizes are ASCII octal in a 12-byte field, so 8 GiB is the ceiling. GNU's
//! escape hatch is base-256: high bit set on the first byte, big-endian binary
//! after. Both are read.
//!
//! ## Knowing where the end is
//!
//! Two consecutive all-zero blocks. A tar cut short by a failed transfer simply
//! ends without them, which this reports as a truncation rather than silently
//! returning a short listing that looks complete.

use std::io::Read;

use super::tree::{RawEntry, MAX_ENTRIES, MAX_NAME_BYTES};
use super::ArchiveError;

/// The tar block size. Not a choice — it is in the format.
pub const TAR_BLOCK: usize = 512;

/// The longest GNU long name accepted: 64 KiB.
///
/// A `L` entry's size field is a full 8 GiB-capable octal number, so without a
/// cap a four-block file can ask for an eight-gigabyte allocation. 64 KiB is
/// sixteen times `PATH_MAX`, so nothing extractable is refused.
pub const MAX_LONG_NAME: usize = 64 * 1024;

/// The most pax extended-header bytes read for one entry: 1 MiB.
///
/// A pax header is a handful of short records — a few hundred bytes in practice.
/// A megabyte allows for the vendor extensions that carry ACLs and xattrs
/// without letting the field length become an allocation primitive.
pub const MAX_PAX_BYTES: usize = 1024 * 1024;

/// The most bytes a listing will read through: 64 GiB.
///
/// Listing a compressed tar means decompressing all of it, because the entry
/// headers are interleaved with the data. That is bounded by the archive's real
/// size — except when the archive is a decompression bomb, where 40 KB of input
/// is petabytes of output. This is the ceiling on how long that can go on. A
/// legitimate archive larger than this exists but is not a thing anyone browses
/// in a file manager, and it lists as truncated rather than failing.
pub const MAX_STREAM_BYTES: u64 = 64 * 1024 * 1024 * 1024;

fn malformed(message: impl Into<String>) -> ArchiveError {
    ArchiveError::Malformed {
        format: "tar",
        message: message.into(),
    }
}

/// Walk a tar stream and list what is in it.
///
/// Returns the entries and whether a cap stopped the walk early.
pub fn list<R: Read>(mut reader: R) -> Result<(Vec<RawEntry>, bool), ArchiveError> {
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut truncated = false;
    let mut consumed: u64 = 0;

    // Carried from a preceding pseudo-header to the entry it describes.
    let mut long_name: Option<String> = None;
    let mut long_link: Option<String> = None;
    let mut pax: Pax = Pax::default();

    let mut block = [0u8; TAR_BLOCK];
    let mut seen_any = false;

    loop {
        let got = read_full(&mut reader, &mut block).map_err(ArchiveError::io_bare)?;
        if got == 0 {
            // Clean EOF with no end-of-archive marker. Common enough from
            // `tar | head` and from interrupted downloads that it is worth
            // reporting as truncation rather than as a parse failure — the
            // entries already read are all good.
            if seen_any {
                truncated = true;
            }
            break;
        }
        if got < TAR_BLOCK {
            truncated = true;
            break;
        }
        consumed += TAR_BLOCK as u64;

        if block.iter().all(|b| *b == 0) {
            // One zero block starts the end-of-archive marker; the second
            // confirms it. Either way there is nothing more to list.
            break;
        }

        verify_checksum(&block)?;
        seen_any = true;

        let type_flag = block[156];
        let declared_size = number(&block[124..136]).ok_or_else(|| malformed("unreadable size"))?;

        match type_flag {
            // GNU long name / long link: the payload *is* the name.
            b'L' | b'K' => {
                let want = declared_size.min(MAX_LONG_NAME as u64) as usize;
                let payload = read_payload(&mut reader, declared_size, want)?;
                consumed += padded(declared_size);
                let text = decode(&payload);
                if type_flag == b'L' {
                    long_name = Some(text);
                } else {
                    long_link = Some(text);
                }
            }
            // pax extended header, per-entry (`x`) or global (`g`). The global
            // one is read and dropped: nothing it can say changes a listing, and
            // pretending otherwise means carrying state across entries for no
            // visible benefit.
            b'x' | b'X' | b'g' => {
                let want = declared_size.min(MAX_PAX_BYTES as u64) as usize;
                let payload = read_payload(&mut reader, declared_size, want)?;
                consumed += padded(declared_size);
                if type_flag != b'g' {
                    pax = parse_pax(&payload);
                }
            }
            _ => {
                if entries.len() >= MAX_ENTRIES {
                    truncated = true;
                    break;
                }
                let name = pax
                    .path
                    .take()
                    .or_else(|| long_name.take())
                    .unwrap_or_else(|| ustar_name(&block));
                let name = if name.len() > MAX_NAME_BYTES {
                    name[..MAX_NAME_BYTES].to_string()
                } else {
                    name
                };

                let size = pax.size.unwrap_or(declared_size);
                let is_dir = type_flag == b'5' || name.ends_with('/');
                let link_target = pax
                    .linkpath
                    .take()
                    .or_else(|| long_link.take())
                    .or_else(|| {
                        // Types 1 and 2 are hardlink and symlink; for everything else
                        // the linkname field is padding.
                        if type_flag == b'1' || type_flag == b'2' {
                            let text = cstr(&block[157..257]);
                            (!text.is_empty()).then_some(text)
                        } else {
                            None
                        }
                    });
                let mtime = pax
                    .mtime
                    .or_else(|| number(&block[136..148]).map(|n| n as i64));

                entries.push(RawEntry {
                    name,
                    // A directory's size field is meaningless; a link's is zero
                    // and its target is the payload-free `linkname`.
                    len: if is_dir { 0 } else { size },
                    compressed: if is_dir { 0 } else { size },
                    mtime,
                    is_dir,
                    method: super::tree::Method::Store,
                    encrypted: false,
                    link_target,
                });

                // The data is skipped by reading it: this is a stream, and there
                // is no seek on the far end of a decompressor's pipe.
                skip_exact(&mut reader, padded(declared_size))?;
                consumed += padded(declared_size);
                pax = Pax::default();
            }
        }

        if consumed >= MAX_STREAM_BYTES {
            truncated = true;
            break;
        }
    }

    if entries.is_empty() && !seen_any {
        return Err(malformed("no entries, and no header that looked like one"));
    }
    Ok((entries, truncated))
}

/// Whether these bytes could be the start of a tar.
///
/// Used to tell a bare `.gz` from a `.tar.gz` after decompressing the first
/// block: a gzipped log file is not an archive, and saying so beats listing it
/// as an archive containing garbage.
pub fn looks_like_tar(block: &[u8]) -> bool {
    if block.len() < TAR_BLOCK {
        return false;
    }
    if block[257..262] == *b"ustar" {
        return true;
    }
    // Pre-ustar v7 tars have no magic at all, so the checksum is the only test.
    verify_checksum(block).is_ok()
}

#[derive(Default)]
struct Pax {
    path: Option<String>,
    linkpath: Option<String>,
    size: Option<u64>,
    mtime: Option<i64>,
}

/// `length key=value\n` records, where `length` counts itself.
///
/// The self-inclusive length is the format's one trick and the reason a value
/// may contain a newline: the parser never scans for a terminator, it counts.
fn parse_pax(payload: &[u8]) -> Pax {
    let mut out = Pax::default();
    let mut at = 0usize;
    while at < payload.len() {
        let Some(space) = payload[at..].iter().position(|b| *b == b' ') else {
            break;
        };
        let Ok(digits) = std::str::from_utf8(&payload[at..at + space]) else {
            break;
        };
        let Ok(len) = digits.parse::<usize>() else {
            break;
        };
        if len <= space + 1 || at + len > payload.len() {
            break;
        }
        let record = &payload[at + space + 1..at + len];
        // Trailing newline; a record missing it is malformed but harmless.
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(eq) = record.iter().position(|b| *b == b'=') {
            let key = &record[..eq];
            let value = &record[eq + 1..];
            match key {
                b"path" => out.path = Some(decode(value)),
                b"linkpath" => out.linkpath = Some(decode(value)),
                b"size" => out.size = decode(value).parse().ok(),
                // pax mtime is a decimal with an optional fraction. A listing
                // shows a date, so the fraction is dropped rather than carried.
                b"mtime" => {
                    let text = decode(value);
                    let whole = text.split('.').next().unwrap_or("");
                    out.mtime = whole.parse().ok();
                }
                _ => {}
            }
        }
        at += len;
    }
    out
}

/// `prefix/name` if this is a ustar header with a prefix, otherwise `name`.
fn ustar_name(block: &[u8]) -> String {
    let name = cstr(&block[0..100]);
    if block[257..262] != *b"ustar" {
        return name;
    }
    let prefix = cstr(&block[345..500]);
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

/// The header checksum: every byte summed with the checksum field read as
/// spaces.
///
/// Checked because it is the only thing separating a tar header from 512 bytes
/// of anything else, and this parser is handed the output of a decompressor that
/// may have been pointed at the wrong file. Both the unsigned and the signed sum
/// are accepted: early tars on machines with signed `char` wrote the latter, and
/// GNU tar has accepted both ever since.
fn verify_checksum(block: &[u8]) -> Result<(), ArchiveError> {
    let claimed =
        number(&block[148..156]).ok_or_else(|| malformed("unreadable header checksum"))?;
    let mut unsigned: u64 = 0;
    let mut signed: i64 = 0;
    for (i, b) in block.iter().enumerate() {
        let byte = if (148..156).contains(&i) { b' ' } else { *b };
        unsigned += byte as u64;
        signed += byte as i8 as i64;
    }
    if claimed == unsigned || claimed as i64 == signed {
        Ok(())
    } else {
        Err(malformed(format!(
            "header checksum {claimed} does not match {unsigned}"
        )))
    }
}

/// An octal-or-base-256 numeric field.
///
/// Octal is the format; base-256 (high bit of the first byte set, big-endian
/// after) is GNU's extension for values that do not fit, and is how a tar stores
/// a file over 8 GiB or a timestamp after 2242.
fn number(field: &[u8]) -> Option<u64> {
    if field.is_empty() {
        return None;
    }
    if field[0] & 0x80 != 0 {
        let mut value: u64 = 0;
        // Only the flag bit of the first byte is not part of the number. The
        // low eight bytes are the magnitude; anything above them must be zero,
        // because a wider value cannot fit a `u64` and silently wrapping it
        // would produce a plausible wrong size.
        let low = field.len().saturating_sub(8);
        for (i, b) in field.iter().enumerate() {
            let byte = if i == 0 { *b & 0x7f } else { *b };
            if i < low {
                if byte != 0 {
                    return None;
                }
            } else {
                value = (value << 8) | byte as u64;
            }
        }
        return Some(value);
    }
    let text = field
        .iter()
        .take_while(|b| **b != 0 && **b != b' ')
        .copied()
        .collect::<Vec<u8>>();
    let text = std::str::from_utf8(&text).ok()?.trim();
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

/// A NUL-terminated fixed field as a string.
fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    decode(&field[..end])
}

/// Bytes to a name.
///
/// Lossy on purpose: a tar written on a machine with a Latin-1 locale has names
/// that are not UTF-8, and a replacement character in a listing is better than
/// refusing to list the archive. Extraction of such an entry keeps the *declared*
/// bytes only insofar as they round-trip, which is why a name that did not decode
/// is one more reason for a caller to treat archives as read-only until asked.
fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn padded(size: u64) -> u64 {
    size.div_ceil(TAR_BLOCK as u64) * TAR_BLOCK as u64
}

/// Read `want` bytes of a `size`-byte payload and discard the rest, including
/// the block padding.
fn read_payload<R: Read>(reader: &mut R, size: u64, want: usize) -> Result<Vec<u8>, ArchiveError> {
    let mut buf = vec![0u8; want];
    let got = read_full(reader, &mut buf).map_err(ArchiveError::io_bare)?;
    buf.truncate(got);
    // What is left of the payload *plus* its block padding — `got` bytes of the
    // padded whole have already been consumed.
    skip_exact(reader, padded(size).saturating_sub(got as u64))?;
    Ok(buf)
}

/// Read and throw away exactly `total` bytes.
fn skip_exact<R: Read>(reader: &mut R, total: u64) -> Result<(), ArchiveError> {
    if total == 0 {
        return Ok(());
    }
    // `io::copy` into a sink, in bounded chunks, because the far end may be a
    // pipe: there is nothing to seek.
    let mut left = total;
    let mut scratch = [0u8; 8 * TAR_BLOCK];
    while left > 0 {
        let want = (left as usize).min(scratch.len());
        let got = read_full(reader, &mut scratch[..want]).map_err(ArchiveError::io_bare)?;
        if got == 0 {
            // Truncated payload. The entry itself was already listed, which is
            // the useful part, so this is not an error.
            return Ok(());
        }
        left -= got as u64;
    }
    Ok(())
}

/// Fill `buf` unless the stream ends first. Returns how many bytes landed.
///
/// `Read::read` on a pipe returns whatever has arrived, which for a decompressor
/// is routinely less than 512 bytes; a parser that treats a short read as EOF
/// mis-lists every compressed tar it sees.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut at = 0usize;
    while at < buf.len() {
        match reader.read(&mut buf[at..]) {
            Ok(0) => break,
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(at)
}
