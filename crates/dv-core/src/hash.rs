//! Content hashing (§8.1): blake3 of first + last 1 MB + file size. Fast,
//! good enough for relink-by-hash and cache keying. The canonical
//! implementation — dv-media and relink both call this.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const CHUNK: u64 = 1024 * 1024;

/// Hex blake3 content hash of a file. Files ≤ 2 MB hash their entire
/// contents; larger files hash first 1 MB + last 1 MB. Size is always mixed
/// in, so a truncated copy never collides with its original.
pub fn content_hash(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    let mut buf = vec![0u8; CHUNK as usize];
    if size <= 2 * CHUNK {
        io::copy(&mut f, &mut HashWriter(&mut hasher))?;
    } else {
        f.read_exact(&mut buf)?;
        hasher.update(&buf);
        f.seek(SeekFrom::End(-(CHUNK as i64)))?;
        f.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

struct HashWriter<'a>(&'a mut blake3::Hasher);

impl io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn tmp_with(bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("dv-core-hash-tests");
        std::fs::create_dir_all(&dir).expect("create tmp dir");
        let p = dir.join(format!("f{}.bin", blake3::hash(bytes).to_hex()));
        let mut f = File::create(&p).expect("create tmp file");
        f.write_all(bytes).expect("write tmp file");
        p
    }

    #[test]
    fn stable_and_size_sensitive() {
        let a = tmp_with(b"hello world");
        let b = tmp_with(b"hello worl");
        let ha = content_hash(&a).expect("hash a");
        let ha2 = content_hash(&a).expect("hash a again");
        let hb = content_hash(&b).expect("hash b");
        assert_eq!(ha, ha2);
        assert_ne!(ha, hb);
    }

    #[test]
    fn large_file_middle_ignored() {
        // > 2 MB: only first/last MB + size count.
        let mut big1 = vec![7u8; 3 * CHUNK as usize];
        let mut big2 = big1.clone();
        big2[(CHUNK + 1000) as usize] = 42; // middle byte differs
        big1[0] = 1;
        big2[0] = 1;
        let h1 = content_hash(&tmp_with(&big1)).expect("hash big1");
        let h2 = content_hash(&tmp_with(&big2)).expect("hash big2");
        assert_eq!(h1, h2, "middle bytes must not affect the hash");
        let mut big3 = big1.clone();
        big3[5] = 99; // first-MB byte differs
        let h3 = content_hash(&tmp_with(&big3)).expect("hash big3");
        assert_ne!(h1, h3);
    }
}
