//! CRC-32, the checksum a zip member and a gzip stream both end on.
//!
//! The IEEE polynomial in its reflected form (`0xEDB88320`), which is the one
//! both formats name. Table-driven, and eight bytes at a time rather than one
//! ("slicing-by-8"): a zip of a photo library is *stored*, not deflated, so
//! for those members the checksum is the only work between the read and the
//! write, and at one byte per lookup it would be the slowest part of the
//! archive — slower than the disk. Eight tables of 256 are 8 KiB, built at
//! compile time, and there is no dependency to take for them.

/// `TABLES[0]` is the classic byte table; `TABLES[k][b]` is the CRC of byte
/// `b` followed by `k` zero bytes, which is what lets eight lookups stand in
/// for eight rounds of the byte loop.
const TABLES: [[u32; 256]; 8] = tables();

const fn tables() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        tables[0][i] = crc;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut k = 1;
        while k < 8 {
            let prev = tables[k - 1][i];
            tables[k][i] = (prev >> 8) ^ tables[0][(prev & 0xff) as usize];
            k += 1;
        }
        i += 1;
    }
    tables
}

/// A running CRC-32: [`Crc32::update`] with every chunk in order, then
/// [`Crc32::finish`].
#[derive(Debug, Clone, Copy)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Crc32 {
        Crc32::new()
    }
}

impl Crc32 {
    pub fn new() -> Crc32 {
        Crc32(0xFFFF_FFFF)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let t = &TABLES;
        let mut crc = self.0;
        let (blocks, rest) = bytes.as_chunks::<8>();
        for b in blocks {
            let lo = crc ^ u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            crc = t[7][(lo & 0xff) as usize]
                ^ t[6][((lo >> 8) & 0xff) as usize]
                ^ t[5][((lo >> 16) & 0xff) as usize]
                ^ t[4][(lo >> 24) as usize]
                ^ t[3][b[4] as usize]
                ^ t[2][b[5] as usize]
                ^ t[1][b[6] as usize]
                ^ t[0][b[7] as usize];
        }
        for byte in rest {
            crc = t[0][((crc ^ *byte as u32) & 0xff) as usize] ^ (crc >> 8);
        }
        self.0 = crc;
    }

    pub fn finish(self) -> u32 {
        !self.0
    }
}

/// The CRC-32 of one buffer.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check value every CRC catalogue lists for this polynomial.
    #[test]
    fn the_catalogue_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    /// The eight-at-a-time path and the byte path agree, whatever the split:
    /// a chunk boundary in the middle of an eight-byte block is the case a
    /// streaming caller hits on every read that is not a multiple of eight.
    #[test]
    fn chunking_does_not_change_the_answer() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 31 + i / 7) as u8).collect();
        let whole = crc32(&data);
        for split in [1usize, 3, 7, 8, 9, 64, 4097] {
            let mut crc = Crc32::new();
            for piece in data.chunks(split) {
                crc.update(piece);
            }
            assert_eq!(crc.finish(), whole, "pieces of {split}");
        }
    }
}
