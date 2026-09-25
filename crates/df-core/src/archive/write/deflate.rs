//! Deflate, streamed, and the gzip framing around it.
//!
//! The compressor is miniz_oxide's — the same crate the zip reader inflates
//! with — driven through its streaming entry point a chunk at a time, never
//! `compress_to_vec` on a whole file: a member is as big as the file it came
//! from, and a 40 GB video has to go through in a megabyte of memory.
//!
//! Level 6, the level `zip` and `gzip` default to. It is where the curve
//! flattens: level 9 costs roughly twice the time for a percent or two less,
//! and a file manager's archive is made while somebody waits for it.
//!
//! Gzip is ten bytes of header, a raw deflate stream, and eight bytes of
//! trailer — framing, in the sense [`crate::archive`]'s essay draws the line
//! at, so it is written here rather than piped through `gzip`.

use std::io::{self, Write};

use miniz_oxide::deflate::core::{create_comp_flags_from_zip_params, CompressorOxide};
use miniz_oxide::deflate::stream::deflate;
use miniz_oxide::{MZError, MZFlush, MZStatus};

use super::crc32::Crc32;

/// The compression level: zip's and gzip's own default.
const LEVEL: i32 = 6;

/// How much compressed output is handed on at a time.
const OUT_BUF: usize = 256 * 1024;

/// One raw deflate stream at a time, reusable across members.
pub(super) struct Deflater {
    /// Boxed: the compressor carries its 64 KiB LZ buffer inline, and a
    /// worker's stack is not where that belongs.
    compressor: Box<CompressorOxide>,
    out: Vec<u8>,
}

impl Deflater {
    pub(super) fn new() -> Deflater {
        // Negative window bits: a raw stream, with no zlib header and no
        // adler32 after it — what a zip member and a gzip body both are.
        let flags = create_comp_flags_from_zip_params(LEVEL, -15, 0);
        Deflater {
            compressor: Box::new(CompressorOxide::new(flags)),
            out: vec![0u8; OUT_BUF],
        }
    }

    /// Start a new stream, keeping the allocations.
    pub(super) fn reset(&mut self) {
        self.compressor.reset();
    }

    /// Compress `input`, handing every piece of output to `emit` as it is
    /// made. `finish` ends the stream: it flushes what the compressor is
    /// holding and writes the final block, and nothing more may be fed after
    /// it until [`Deflater::reset`].
    pub(super) fn feed(
        &mut self,
        mut input: &[u8],
        finish: bool,
        emit: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let flush = if finish {
            MZFlush::Finish
        } else {
            MZFlush::None
        };
        loop {
            let result = deflate(&mut self.compressor, input, &mut self.out, flush);
            input = &input[result.bytes_consumed..];
            if result.bytes_written > 0 {
                emit(&self.out[..result.bytes_written])?;
            }
            match result.status {
                Ok(MZStatus::StreamEnd) => return Ok(()),
                Ok(_) => {}
                // "No progress without more input", which is only an answer
                // when more input is what comes next.
                Err(MZError::Buf) if !finish => return Ok(()),
                Err(e) => return Err(io::Error::other(format!("deflate failed ({e:?})"))),
            }
            // Everything taken and the output not full: the compressor is
            // holding the rest until it has a block's worth. A full output
            // buffer means there may be more waiting, so go round again.
            if !finish && input.is_empty() && result.bytes_written < self.out.len() {
                return Ok(());
            }
            if result.bytes_consumed == 0 && result.bytes_written == 0 {
                return Err(io::Error::other("deflate made no progress"));
            }
        }
    }
}

/// A gzip stream around a [`Write`]: header on the way in, deflate in the
/// middle, CRC-32 and length on [`Gzip::finish`].
///
/// A `.tar.gz` is exactly this over the tar writer — the tar is written once,
/// into here, and never exists uncompressed on the disk.
pub(super) struct Gzip<W: Write> {
    inner: W,
    deflater: Deflater,
    crc: Crc32,
    /// ISIZE is the length modulo 2³², which is what a `u32` that wraps is.
    len: u32,
}

impl<W: Write> Gzip<W> {
    /// Write the header. mtime 0 (RFC 1952: "no time stamp is available"),
    /// because the stream is a tar and every time worth keeping is inside it;
    /// OS 3, Unix.
    pub(super) fn new(mut inner: W) -> io::Result<Gzip<W>> {
        inner.write_all(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3])?;
        Ok(Gzip {
            inner,
            deflater: Deflater::new(),
            crc: Crc32::new(),
            len: 0,
        })
    }

    /// End the deflate stream and write the trailer. The writer comes back
    /// so its owner can flush it and check that the flush worked.
    pub(super) fn finish(mut self) -> io::Result<W> {
        let inner = &mut self.inner;
        self.deflater
            .feed(&[], true, &mut |chunk| inner.write_all(chunk))?;
        self.inner.write_all(&self.crc.finish().to_le_bytes())?;
        self.inner.write_all(&self.len.to_le_bytes())?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for Gzip<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.crc.update(buf);
        self.len = self.len.wrapping_add(buf.len() as u32);
        let inner = &mut self.inner;
        self.deflater
            .feed(buf, false, &mut |chunk| inner.write_all(chunk))?;
        Ok(buf.len())
    }

    /// Flushes what has been compressed so far, not the compressor: a sync
    /// flush would put an empty block into the stream for nothing, and the
    /// only caller that flushes is about to [`Gzip::finish`].
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
