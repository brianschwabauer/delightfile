//! Cache layout (§8.3). Content-hash-keyed on-disk cache, shared across
//! projects and safe to delete entirely (everything regenerates).
//!
//! Layout under `<root>/<media-hash>/`:
//! ```text
//! index.bin      keyframe index (§4.3, crate::keyframe)
//! thumbs/NNNN.jpg thumbnail strip (§4.3, crate::thumbnail)
//! waveform.pk    waveform peaks (§5, crate::waveform)
//! proxy.mp4      all-intra proxy media (§4.3, crate::proxy)
//! proxy_index.bin keyframe index of proxy.mp4 (same DVKF format)
//! silence.json   detected silence spans (§1, crate::silence)
//! transcript.json word-level transcript (§18, crate::transcribe)
//! ```
//!
//! Root resolution (`dirs::cache_dir()/delightvideo`) belongs to the caller so
//! tests can point at a temp dir (§8.3); [`CacheDir::new`] just takes the root.
//! LRU eviction reads directory mtime (bumped by [`CacheDir::touch`]); eviction
//! itself lives in `dv-app`, not here.

use std::path::{Path, PathBuf};

/// A handle to the cache root. Cheap to clone; holds only the root path.
#[derive(Debug, Clone)]
pub struct CacheDir {
    root: PathBuf,
}

impl CacheDir {
    /// Wrap a cache root (e.g. `~/.cache/delightvideo`). Does not create it.
    pub fn new(root: PathBuf) -> CacheDir {
        CacheDir { root }
    }

    /// The cache root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<hash>/` — a media file's cache directory.
    pub fn media_dir(&self, hash: &str) -> PathBuf {
        self.root.join(hash)
    }

    /// `<root>/<hash>/index.bin` (keyframe index, §4.3).
    pub fn index_path(&self, hash: &str) -> PathBuf {
        self.media_dir(hash).join("index.bin")
    }

    /// `<root>/<hash>/thumbs/` (thumbnail strip, §4.3).
    pub fn thumbs_dir(&self, hash: &str) -> PathBuf {
        self.media_dir(hash).join("thumbs")
    }

    /// `<root>/<hash>/waveform.pk` (waveform peaks, §5).
    pub fn waveform_path(&self, hash: &str) -> PathBuf {
        self.media_dir(hash).join("waveform.pk")
    }

    /// `<root>/<hash>/proxy.mp4` (all-intra scrub proxy, §4.3).
    pub fn proxy_path(&self, hash: &str) -> PathBuf {
        self.media_dir(hash).join("proxy.mp4")
    }

    /// `<root>/<hash>/proxy_index.bin` — keyframe index of the proxy itself
    /// (all frames are keyframes; the index makes reverse stepping and seeks
    /// deterministic without probing, §4.4).
    pub fn proxy_index_path(&self, hash: &str) -> PathBuf {
        self.media_dir(hash).join("proxy_index.bin")
    }

    /// `<root>/<hash>/silence.json` (detected silence spans, §8.3).
    pub fn silence_path(&self, hash: &str) -> PathBuf {
        self.media_dir(hash).join("silence.json")
    }

    /// `<root>/<hash>/transcript.json` (word-level transcript, §18).
    pub fn transcript_path(&self, hash: &str) -> PathBuf {
        self.media_dir(hash).join("transcript.json")
    }

    /// Create `<root>/<hash>/` if missing; returns it. Callers make the dir
    /// before writing any asset into it.
    pub fn ensure_media_dir(&self, hash: &str) -> std::io::Result<PathBuf> {
        let dir = self.media_dir(hash);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Bump the media dir's mtime so LRU eviction (§8.3, in `dv-app`) treats it
    /// as recently used. Called on every asset read. No-op if the dir is
    /// missing (nothing to keep warm yet).
    pub fn touch(&self, hash: &str) -> std::io::Result<()> {
        let dir = self.media_dir(hash);
        if !dir.exists() {
            return Ok(());
        }
        // Portable mtime bump: set the directory's file times to now.
        let now = std::time::SystemTime::now();
        let times = std::fs::FileTimes::new()
            .set_modified(now)
            .set_accessed(now);
        let f = std::fs::File::open(&dir)?;
        f.set_times(times)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_hash_scoped() {
        let c = CacheDir::new(PathBuf::from("/cache/root"));
        let h = "abc123";
        assert_eq!(c.media_dir(h), PathBuf::from("/cache/root/abc123"));
        assert_eq!(
            c.index_path(h),
            PathBuf::from("/cache/root/abc123/index.bin")
        );
        assert_eq!(c.thumbs_dir(h), PathBuf::from("/cache/root/abc123/thumbs"));
        assert_eq!(
            c.waveform_path(h),
            PathBuf::from("/cache/root/abc123/waveform.pk")
        );
    }

    #[test]
    fn ensure_and_touch_bumps_mtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = CacheDir::new(dir.path().to_path_buf());
        let h = "deadbeef";
        // touch on a missing dir is a no-op, not an error.
        c.touch(h).expect("touch missing");
        let md = c.ensure_media_dir(h).expect("ensure");
        assert!(md.is_dir());
        // Set an old mtime, then touch, then confirm it moved forward.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let f = std::fs::File::open(&md).expect("open dir");
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .expect("set old");
        let before = std::fs::metadata(&md)
            .expect("meta")
            .modified()
            .expect("mtime");
        c.touch(h).expect("touch");
        let after = std::fs::metadata(&md)
            .expect("meta")
            .modified()
            .expect("mtime");
        assert!(after > before, "touch should bump mtime forward");
    }
}
