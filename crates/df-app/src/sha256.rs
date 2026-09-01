//! SHA-256, written out by hand.
//!
//! The spot panel shows a checksum row, and a checksum needs a hash. This
//! project defaults to zero dependencies, and SHA-256 is the case where that
//! default is easy to keep: the whole algorithm is a page and a half of
//! shifts and additions, fully specified by FIPS 180-4 and pinned forever by
//! published test vectors, while a crate would be a supply chain to watch for
//! the rest of the program's life. So it lives here.
//!
//! The other decision that matters is that hashing streams. Somebody will
//! point this at a 40 GB disk image, and that hash has to be interruptible —
//! if they click away, or close the panel, or select a different file, the
//! work stops on the next chunk boundary instead of holding a worker for
//! several minutes. It also means the UI thread never touches a file: the
//! caller runs [`hash_file`] on a worker, watches the byte count come back
//! through the progress callback, and flips an [`AtomicBool`] to stop it.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// The first thirty-two bits of the fractional parts of the cube roots of the
/// first sixty-four primes. These are the round constants from FIPS 180-4
/// §4.2.2; the values are not ours to choose, and a compression round pulls
/// one per iteration.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The initial hash value: the first thirty-two bits of the fractional parts
/// of the square roots of the first eight primes, per FIPS 180-4 §5.3.3. Also
/// not ours to choose — a different starting state is a different hash.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// SHA-256 processes the message in 512-bit blocks. Sixty-four bytes is that
/// block, and it is the size of the buffer that holds a partial one between
/// calls to [`Sha256::update`].
const BLOCK: usize = 64;

/// How much of a file to read at a time in [`hash_file`]. 256 KiB is large
/// enough that the read syscall and the progress callback disappear against
/// the cost of hashing the bytes, and small enough that a cancel is felt at
/// once: at any plausible disk speed a chunk is a couple of milliseconds, so
/// the worker never keeps running for a perceptible moment after the user has
/// moved on.
const CHUNK: usize = 256 * 1024;

/// Streaming SHA-256 state.
///
/// Feed it bytes with [`update`](Sha256::update) as many times as you like,
/// in whatever sizes arrive, then take the digest with
/// [`finish`](Sha256::finish). Splitting the input differently never changes
/// the result.
pub struct Sha256 {
    /// The eight working words of the hash.
    state: [u32; 8],
    /// Bytes that have arrived but do not yet fill a block.
    buffer: [u8; BLOCK],
    /// How much of `buffer` is live.
    buffered: usize,
    /// Total bytes fed in, which the padding encodes as a bit count.
    total: u64,
}

impl Sha256 {
    /// A fresh hasher, primed with the standard initial hash value.
    pub fn new() -> Sha256 {
        Sha256 {
            state: H0,
            buffer: [0u8; BLOCK],
            buffered: 0,
            total: 0,
        }
    }

    /// Add bytes to the message.
    pub fn update(&mut self, bytes: &[u8]) {
        self.total = self.total.wrapping_add(bytes.len() as u64);
        let mut rest = bytes;

        // Top up a partial block first, and only compress once it is full.
        if self.buffered > 0 {
            let want = BLOCK - self.buffered;
            let take = want.min(rest.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&rest[..take]);
            self.buffered += take;
            rest = &rest[take..];
            if self.buffered < BLOCK {
                return;
            }
            let block = self.buffer;
            compress(&mut self.state, &block);
            self.buffered = 0;
        }

        // Then take whole blocks straight out of the caller's slice.
        while rest.len() >= BLOCK {
            let (block, tail) = rest.split_at(BLOCK);
            let mut fixed = [0u8; BLOCK];
            fixed.copy_from_slice(block);
            compress(&mut self.state, &fixed);
            rest = tail;
        }

        // Whatever is left over waits for the next call.
        if !rest.is_empty() {
            self.buffer[..rest.len()].copy_from_slice(rest);
            self.buffered = rest.len();
        }
    }

    /// Pad the message, compress the final block or two, and return the
    /// thirty-two byte digest.
    pub fn finish(mut self) -> [u8; 32] {
        // A single 1 bit, then zeroes, then the length in bits as a big-endian
        // u64. If the length will not fit in this block, it goes in another.
        let bits = self.total.wrapping_mul(8);
        self.buffer[self.buffered] = 0x80;
        self.buffered += 1;

        if self.buffered > BLOCK - 8 {
            for slot in self.buffer[self.buffered..].iter_mut() {
                *slot = 0;
            }
            let block = self.buffer;
            compress(&mut self.state, &block);
            self.buffered = 0;
        }

        for slot in self.buffer[self.buffered..BLOCK - 8].iter_mut() {
            *slot = 0;
        }
        self.buffer[BLOCK - 8..].copy_from_slice(&bits.to_be_bytes());
        let block = self.buffer;
        compress(&mut self.state, &block);

        let mut out = [0u8; 32];
        for (word, chunk) in self.state.iter().zip(out.chunks_exact_mut(4)) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Default for Sha256 {
    fn default() -> Sha256 {
        Sha256::new()
    }
}

/// The SHA-256 compression function: expand one 512-bit block into a
/// sixty-four word schedule and stir it into the state.
fn compress(state: &mut [u32; 8], block: &[u8; BLOCK]) {
    let mut w = [0u32; 64];
    for (word, chunk) in w[..16].iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];

    for (k, word) in K.iter().zip(w.iter()) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*k)
            .wrapping_add(*word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Hash a slice in one go.
///
/// Only the tests call it — the spot panel hashes *files*, and a file it can
/// read into memory whole is a file it may as well stream — but it is the form
/// every published test vector is written against, so it is what the vectors
/// below are checked with.
#[cfg(test)]
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finish()
}

/// Render a digest as sixty-four lowercase hex characters, the form everyone
/// expects to paste into a comparison.
pub fn hex(digest: &[u8; 32]) -> String {
    /// The sixteen hex digits, indexed by nibble. Lowercase because that is
    /// what `sha256sum` and every checksum file on the internet print.
    const DIGITS: [u8; 16] = *b"0123456789abcdef";

    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// How a streamed hash of a file ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    /// The whole file was read, and here is its digest.
    Done([u8; 32]),
    /// `cancel` went true partway through, so the digest was never finished.
    Cancelled,
}

/// Hash a file in chunks, reporting bytes read so far and checking `cancel`
/// between chunks.
///
/// `chunk` is the progress callback. It is called at most once per chunk read
/// with the running byte count — it is what drives the spot panel's progress
/// bar, and it rings the app's wake bell, so it must not be called per byte.
///
/// `cancel` is read before every chunk, including the first, so a hash that
/// was already called off never touches the disk. A cancelled scan returns
/// [`Scan::Cancelled`] rather than an error; an error means the file itself
/// could not be read, and carries the io message.
pub fn hash_file(
    path: &Path,
    chunk: &mut dyn FnMut(u64),
    cancel: &AtomicBool,
) -> Result<Scan, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut read_so_far: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Scan::Cancelled);
        }
        let filled = match file.read(&mut buffer) {
            Ok(filled) => filled,
            // A signal landed mid-read; nothing was consumed, so go again.
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        if filled == 0 {
            break;
        }
        hasher.update(&buffer[..filled]);
        read_so_far += filled as u64;
        chunk(read_so_far);
    }

    Ok(Scan::Done(hasher.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// Every published vector is written in hex, so compare in hex.
    fn hex_of(bytes: &[u8]) -> String {
        hex(&digest(bytes))
    }

    #[test]
    fn the_empty_message_hashes_to_the_well_known_constant() {
        assert_eq!(
            hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn abc_matches_the_nist_single_block_vector() {
        assert_eq!(
            hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_nist_two_block_vector_spans_the_padding_into_a_second_block() {
        assert_eq!(
            hex_of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn a_million_letter_as_fed_in_ragged_pieces_still_matches() {
        let million = vec![b'a'; 1_000_000];
        let mut hasher = Sha256::new();
        let mut at = 0usize;
        // Deliberately awkward sizes so the partial-block buffer is exercised
        // on nearly every call rather than being sidestepped by round numbers.
        let sizes = [1usize, 7, 63, 64, 65, 127, 1000, 4097];
        let mut which = 0usize;
        while at < million.len() {
            let take = sizes[which % sizes.len()].min(million.len() - at);
            hasher.update(&million[at..at + take]);
            at += take;
            which += 1;
        }
        assert_eq!(
            hex(&hasher.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn splitting_the_input_never_changes_the_digest() {
        let mut split = Sha256::new();
        split.update(b"a");
        split.update(b"bc");
        assert_eq!(split.finish(), digest(b"abc"));

        let message: Vec<u8> = (0u8..=255).cycle().take(5000).collect();
        for cut in [0usize, 1, 63, 64, 65, 4096, 4999, 5000] {
            let mut hasher = Sha256::new();
            hasher.update(&message[..cut]);
            hasher.update(&message[cut..]);
            assert_eq!(hasher.finish(), digest(&message), "split at {cut}");
        }
    }

    #[test]
    fn the_lengths_that_straddle_the_padding_boundary_hash_the_same_either_way() {
        // 55 is the last length whose padding and length word fit in one
        // block; 56 forces a second. 119 and 120 are the same boundary one
        // block further along, and 63/64 sit on the block edge itself.
        for len in [55usize, 56, 63, 64, 119, 120] {
            let message: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut hasher = Sha256::new();
            for byte in message.iter() {
                hasher.update(&[*byte]);
            }
            assert_eq!(hasher.finish(), digest(&message), "length {len}");
        }
    }

    #[test]
    fn hex_is_sixty_four_lowercase_hex_characters() {
        let rendered = hex(&digest(b"delightfile"));
        assert_eq!(rendered.len(), 64);
        assert!(rendered
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    }

    #[test]
    fn hashing_a_real_file_agrees_with_hashing_its_bytes() {
        // Comfortably more than one CHUNK, so the chunk loop runs several
        // times and progress has something to report.
        let bytes: Vec<u8> = (0..CHUNK * 2 + 1234).map(|i| (i % 253) as u8).collect();
        let path = std::env::temp_dir().join(format!("df-sha256-{}.bin", std::process::id()));
        let mut file = fs::File::create(&path).expect("the fixture");
        file.write_all(&bytes).expect("the fixture");
        drop(file);

        let mut reports: Vec<u64> = Vec::new();
        let cancel = AtomicBool::new(false);
        let scan = hash_file(&path, &mut |so_far| reports.push(so_far), &cancel);
        let _ = fs::remove_file(&path);

        assert_eq!(scan.expect("the scan"), Scan::Done(digest(&bytes)));
        assert!(reports.windows(2).all(|pair| pair[1] > pair[0]));
        assert_eq!(reports.last().copied(), Some(bytes.len() as u64));
    }

    #[test]
    fn a_hash_that_was_already_cancelled_never_reads_a_byte() {
        let path =
            std::env::temp_dir().join(format!("df-sha256-cancel-{}.bin", std::process::id()));
        fs::write(&path, b"whatever").expect("the fixture");

        let cancel = AtomicBool::new(true);
        let mut reports = 0usize;
        let scan = hash_file(&path, &mut |_| reports += 1, &cancel);
        let _ = fs::remove_file(&path);

        assert_eq!(scan.expect("the scan"), Scan::Cancelled);
        assert_eq!(reports, 0);
    }
}
