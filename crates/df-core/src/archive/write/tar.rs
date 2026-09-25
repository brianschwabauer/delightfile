//! Tar, written forwards into anything that takes bytes.
//!
//! No seeking: the same writer fills a `.tar` on the disk, the gzip stream of
//! a `.tar.gz`, and the stdin of a `zstd` or `xz` whose stdout is the archive.
//! That is why a member's size goes in its header *before* its data, taken
//! from the walk, and why a file that shrinks while it is being read is an
//! error rather than a header that lies — tar has nothing after the data to
//! correct it with, and a short member desynchronises every block after it.
//! (A file that grows is archived to the length the header already declared,
//! which is what GNU tar does too.)
//!
//! ## What the reader reads, written
//!
//! ustar headers, and a pax extended header (`x`) in front of any entry whose
//! name or link target does not fit the header's hundred bytes — pax rather
//! than GNU's `L`/`K` because it is the POSIX one, and the reader here,
//! GNU tar and bsdtar all take it. Numbers too big for their octal field —
//! a size past 8 GiB, a large uid — go in GNU's base-256 form, which the
//! reader's `number` decodes: the header's own size field is what every
//! reader does its block arithmetic with, so it has to be *in* the header.
//!
//! Two zero blocks end the archive. No padding to a 10 KiB record: that was
//! for tape drives, and nothing that reads a file needs it.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use super::{read_chunk, Member, What};
use crate::archive::tar::TAR_BLOCK;
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

/// The longest name the header's `name` field holds.
const NAME_FIELD: usize = 100;

/// Write every member, then the end-of-archive marker. Returns the bytes of
/// file contents written, which is what the headers declared: a file is
/// either read to that length or the archive fails.
pub(super) fn write<W: Write>(
    members: &[Member],
    out: &mut W,
    archive: &Path,
    ctx: &TaskCtx,
) -> Result<u64> {
    let mut buf = vec![0u8; crate::ops::COPY_CHUNK];
    let mut read = 0u64;
    let put = |out: &mut W, bytes: &[u8]| out.write_all(bytes).map_err(|e| DfError::io(archive, e));
    for member in members {
        ctx.checkpoint()?;
        let (type_flag, size, link): (u8, u64, &[u8]) = match &member.what {
            What::Dir => (b'5', 0, &[]),
            What::File(len) => (b'0', *len, &[]),
            What::Link(target) => (b'2', 0, target.as_slice()),
        };

        let mut records = Vec::new();
        if member.name.len() > NAME_FIELD {
            pax_record(&mut records, "path", &member.name);
        }
        if link.len() > NAME_FIELD {
            pax_record(&mut records, "linkpath", link);
        }
        if !records.is_empty() {
            if std::str::from_utf8(&member.name).is_err() || std::str::from_utf8(link).is_err() {
                // pax values are UTF-8 unless the archive says otherwise, and
                // a name from a Latin-1 machine is not. First, because it
                // governs the records after it.
                let mut marked = Vec::new();
                pax_record(&mut marked, "hdrcharset", b"BINARY");
                marked.extend_from_slice(&records);
                records = marked;
            }
            let mut pax_name = b"PaxHeader/".to_vec();
            pax_name.extend_from_slice(&member.name);
            let header = header(&pax_name, b'x', records.len() as u64, 0o644, member, &[]);
            put(out, &header)?;
            put(out, &records)?;
            put(out, &vec![0u8; padding(records.len() as u64)])?;
        }

        put(
            out,
            &header(&member.name, type_flag, size, member.mode, member, link),
        )?;
        if let What::File(len) = member.what {
            copy(member, len, out, &mut buf, archive, ctx)?;
            read += len;
            put(out, &vec![0u8; padding(len)])?;
        }
        ctx.advance(0, 1);
    }
    put(out, &[0u8; 2 * TAR_BLOCK])?;
    Ok(read)
}

/// Exactly `len` bytes of the member's file, a chunk at a time.
fn copy<W: Write>(
    member: &Member,
    len: u64,
    out: &mut W,
    buf: &mut [u8],
    archive: &Path,
    ctx: &TaskCtx,
) -> Result<()> {
    let source = &member.source;
    let file = File::open(source).map_err(|e| DfError::io(source, e))?;
    let mut file = file.take(len);
    let mut left = len;
    while left > 0 {
        ctx.checkpoint()?;
        let want = (buf.len() as u64).min(left) as usize;
        let n = read_chunk(&mut file, &mut buf[..want]).map_err(|e| DfError::io(source, e))?;
        if n == 0 {
            return Err(DfError::Op(format!(
                "{} shrank while it was being archived",
                source.display()
            )));
        }
        out.write_all(&buf[..n])
            .map_err(|e| DfError::io(archive, e))?;
        left -= n as u64;
        ctx.advance(n as u64, 0);
    }
    Ok(())
}

/// Zero bytes to the next block boundary.
fn padding(len: u64) -> usize {
    let rem = (len % TAR_BLOCK as u64) as usize;
    if rem == 0 {
        0
    } else {
        TAR_BLOCK - rem
    }
}

/// One `length key=value\n` record, whose length counts itself — so the
/// digits are found by trying: a record that is 99 bytes without its length
/// is 102 with it, which has three digits, not two.
fn pax_record(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    let body = key.len() + 1 + value.len() + 1;
    let mut total = body + 1;
    loop {
        let digits = total.to_string().len();
        if body + 1 + digits == total {
            break;
        }
        total = body + 1 + digits;
    }
    out.extend_from_slice(total.to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(key.as_bytes());
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
}

/// A ustar header. A name or link target too long for its field is cut to fit
/// — the pax record in front of it says the whole thing.
fn header(
    name: &[u8],
    type_flag: u8,
    size: u64,
    mode: u32,
    member: &Member,
    link: &[u8],
) -> [u8; TAR_BLOCK] {
    let mut h = [0u8; TAR_BLOCK];
    let cut = |field: &mut [u8], value: &[u8]| {
        let n = value.len().min(field.len());
        field[..n].copy_from_slice(&value[..n]);
    };
    cut(&mut h[0..100], name);
    number(&mut h[100..108], u64::from(mode & 0o7777));
    number(&mut h[108..116], u64::from(member.uid));
    number(&mut h[116..124], u64::from(member.gid));
    number(&mut h[124..136], size);
    // Before 1970 has no octal spelling, and the reader's numbers are
    // unsigned; the epoch is the nearest honest answer.
    number(&mut h[136..148], member.mtime.max(0) as u64);
    h[156] = type_flag;
    cut(&mut h[157..257], link);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    number(&mut h[329..337], 0);
    number(&mut h[337..345], 0);

    // The checksum is summed with its own field read as eight spaces, and
    // written as six octal digits, a NUL and a space.
    h[148..156].copy_from_slice(b"        ");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    let digits = format!("{sum:06o}");
    h[148..154].copy_from_slice(&digits.as_bytes()[digits.len() - 6..]);
    h[154] = 0;
    h[155] = b' ';
    h
}

/// A numeric field: zero-padded octal and a NUL when it fits, GNU base-256
/// (the high bit of the first byte set, big-endian after it) when it does not.
fn number(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    if digits < 22 && value < 1u64 << (3 * digits) {
        let text = format!("{value:0digits$o}");
        field[..digits].copy_from_slice(text.as_bytes());
        field[digits] = 0;
        return;
    }
    field.fill(0);
    let bytes = value.to_be_bytes();
    let n = field.len();
    let take = bytes.len().min(n - 1);
    field[n - take..].copy_from_slice(&bytes[bytes.len() - take..]);
    field[0] |= 0x80;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pax_record_counts_its_own_length() {
        let mut out = Vec::new();
        pax_record(&mut out, "path", b"a");
        assert_eq!(out, b"9 path=a\n");
        // 9 + 90 = 99 without the digits, and three digits make it 102.
        let mut out = Vec::new();
        let long = vec![b'x'; 92];
        pax_record(&mut out, "path", &long);
        let text = String::from_utf8(out.clone()).unwrap_or_default();
        let declared: usize = text
            .split(' ')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        assert_eq!(declared, out.len());
    }

    #[test]
    fn numbers_that_do_not_fit_octal_go_base_256() {
        let mut field = [0u8; 12];
        number(&mut field, 0o777);
        assert_eq!(&field, b"00000000777\0");
        let big = 10u64 << 30;
        number(&mut field, big);
        assert_eq!(field[0] & 0x80, 0x80);
        let mut back = 0u64;
        for (i, b) in field.iter().enumerate() {
            let b = if i == 0 { b & 0x7f } else { *b };
            back = (back << 8) | u64::from(b);
        }
        assert_eq!(back, big);
    }
}
