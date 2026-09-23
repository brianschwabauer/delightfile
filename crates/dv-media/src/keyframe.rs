//! Keyframe index — scrubbing layer 1 (§4.3).
//!
//! A **demux-only** pass (no decoding) that records every video packet's
//! presentation/decode timestamps, byte position, and keyframe flag. Seeking in
//! M2 binary-searches this for the keyframe preceding a target time, issues one
//! `avformat_seek`, and decodes forward — deterministic, fast seeks even on
//! long-GOP footage.
//!
//! # `index.bin` binary format (little-endian, versioned)
//!
//! ```text
//! offset  size  field
//! 0       4     magic            = b"DVKF"
//! 4       2     version          = 1 (u16)
//! 6       2     reserved         = 0 (u16, flags/padding)
//! 8       4     time_base_num    (i32)   — video stream time_base …
//! 12      4     time_base_den    (i32)   — … kept so M2 can map µs → stream ts
//! 16      8     count            (u64)   — number of entries
//! 24      …     entries[count], each 25 bytes:
//!                 pts_us   (i64)  — i64::MIN = unknown (packet had no pts)
//!                 dts_us   (i64)  — i64::MIN = unknown
//!                 pos      (i64)  — byte offset in the file, -1 = unknown
//!                 keyframe (u8)   — 1 = keyframe packet, 0 = not
//! ```
//!
//! Times are microseconds (converted from the stream time_base at build time,
//! §4.3). The keyframe lookup index (`keyframes`, positions of keyframe
//! entries) is *derived* from `entries` on both build and load — it is not part
//! of the file, so a build→save→load roundtrip is byte-for-byte reproducible
//! and the two `KeyframeIndex` values compare equal.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use ffmpeg_next as ffmpeg;

use crate::{ensure_ffmpeg, MediaError, Result};

const MAGIC: &[u8; 4] = b"DVKF";
const VERSION: u16 = 1;
/// Sentinel for a packet with no pts/dts (µs value that can't occur naturally).
const UNKNOWN_TS: i64 = i64::MIN;

/// One demuxed video packet (§4.3). Times are microseconds; `None` when the
/// packet carried no timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyframeEntry {
    pub pts_us: Option<i64>,
    pub dts_us: Option<i64>,
    /// Byte offset of the packet in the file, or `-1` if unknown.
    pub pos: i64,
    pub keyframe: bool,
}

/// The full packet index for one source file's video stream.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframeIndex {
    /// The video stream's time_base (num, den) — retained so M2 can convert a
    /// target microsecond back into a stream timestamp for `avformat_seek`.
    pub time_base: (i32, i32),
    /// Every video packet, in demux order.
    pub entries: Vec<KeyframeEntry>,
    /// Derived: indices into `entries` that are keyframes, in pts order. Not
    /// serialized (rebuilt on build/load), so roundtrips stay reproducible.
    keyframes: Vec<u32>,
}

impl KeyframeIndex {
    /// Demux `path`'s best video stream and record every packet. Errors with
    /// [`MediaError::NoVideo`] if there is no video stream.
    pub fn build(path: &Path) -> Result<KeyframeIndex> {
        ensure_ffmpeg();
        let mut ictx = ffmpeg::format::input(&path)?;
        // Cover art is skipped, not indexed: a one-packet `ATTACHED_PIC`
        // stream would otherwise hand back a one-entry "index" for a song.
        let stream = crate::best_video_stream(&ictx)
            .ok_or_else(|| MediaError::NoVideo(path.display().to_string()))?;
        let vindex = stream.index();
        let tb = stream.time_base();
        let time_base = (tb.numerator(), tb.denominator());

        let mut entries = Vec::new();
        for (s, packet) in ictx.packets() {
            if s.index() != vindex {
                continue;
            }
            entries.push(KeyframeEntry {
                pts_us: packet.pts().map(|t| ts_to_us(t, time_base)),
                dts_us: packet.dts().map(|t| ts_to_us(t, time_base)),
                pos: packet.position() as i64,
                keyframe: packet.is_key(),
            });
        }
        Ok(Self::from_parts(time_base, entries))
    }

    /// Build the derived keyframe lookup and assemble the struct.
    fn from_parts(time_base: (i32, i32), entries: Vec<KeyframeEntry>) -> KeyframeIndex {
        let keyframes = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.keyframe && e.pts_us.is_some())
            .map(|(i, _)| i as u32)
            .collect();
        KeyframeIndex {
            time_base,
            entries,
            keyframes,
        }
    }

    /// The keyframe entry with the greatest `pts_us` ≤ `pts_us` — where a seek
    /// to `pts_us` must start decoding (§4.3). `None` if no keyframe precedes
    /// it (e.g. target before the first keyframe, or no keyframes at all).
    ///
    /// Keyframe presentation timestamps are monotonically non-decreasing, so
    /// this is an O(log n) binary search over the derived keyframe list.
    pub fn keyframe_before(&self, pts_us: i64) -> Option<KeyframeEntry> {
        // partition_point: count of keyframes whose pts <= target.
        let kf_pts = |k: &u32| self.entries[*k as usize].pts_us.unwrap_or(UNKNOWN_TS);
        let idx = self.keyframes.partition_point(|k| kf_pts(k) <= pts_us);
        if idx == 0 {
            None
        } else {
            let entry_idx = self.keyframes[idx - 1] as usize;
            Some(self.entries[entry_idx])
        }
    }

    /// Number of keyframes in the index.
    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    /// Longest GOP in the stream, in frames: the maximum number of packets from
    /// one keyframe up to (not including) the next keyframe (the final GOP runs
    /// to the end of the stream). 0 when the index has no keyframes.
    ///
    /// This drives the §4.3 proxy decision rule (a long GOP means expensive
    /// seeks and forces a proxy). It reads the raw `keyframe` packet flag rather
    /// than the derived (pts-bearing) keyframe list, so a keyframe packet that
    /// happened to carry no pts still bounds a GOP.
    pub fn max_gop_frames(&self) -> u32 {
        // Indices of every keyframe packet, in demux order.
        let kf_positions: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.keyframe)
            .map(|(i, _)| i)
            .collect();
        if kf_positions.is_empty() {
            return 0;
        }
        let mut max = 0usize;
        for w in kf_positions.windows(2) {
            max = max.max(w[1] - w[0]);
        }
        // Final GOP: from the last keyframe to the end of the stream.
        let last = *kf_positions.last().expect("non-empty checked above");
        max = max.max(self.entries.len() - last);
        max as u32
    }

    /// Serialize to `index.bin` (see the module docs for the format).
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut w = BufWriter::new(File::create(path)?);
        w.write_all(MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&0u16.to_le_bytes())?; // reserved
        w.write_all(&self.time_base.0.to_le_bytes())?;
        w.write_all(&self.time_base.1.to_le_bytes())?;
        w.write_all(&(self.entries.len() as u64).to_le_bytes())?;
        for e in &self.entries {
            w.write_all(&e.pts_us.unwrap_or(UNKNOWN_TS).to_le_bytes())?;
            w.write_all(&e.dts_us.unwrap_or(UNKNOWN_TS).to_le_bytes())?;
            w.write_all(&e.pos.to_le_bytes())?;
            w.write_all(&[u8::from(e.keyframe)])?;
        }
        w.flush()?;
        Ok(())
    }

    /// Load an `index.bin` written by [`save`](Self::save).
    pub fn load(path: &Path) -> Result<KeyframeIndex> {
        let mut r = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(bad("index.bin", "wrong magic bytes"));
        }
        let version = read_u16(&mut r)?;
        if version != VERSION {
            return Err(bad("index.bin", &format!("unsupported version {version}")));
        }
        let _reserved = read_u16(&mut r)?;
        let tb_num = read_i32(&mut r)?;
        let tb_den = read_i32(&mut r)?;
        let count = read_u64(&mut r)?;

        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let pts = read_i64(&mut r)?;
            let dts = read_i64(&mut r)?;
            let pos = read_i64(&mut r)?;
            let mut kf = [0u8; 1];
            r.read_exact(&mut kf)?;
            entries.push(KeyframeEntry {
                pts_us: (pts != UNKNOWN_TS).then_some(pts),
                dts_us: (dts != UNKNOWN_TS).then_some(dts),
                pos,
                keyframe: kf[0] != 0,
            });
        }
        Ok(Self::from_parts((tb_num, tb_den), entries))
    }
}

/// Convert a stream timestamp to microseconds via its time_base. Uses `i128`
/// intermediates so long files never overflow.
fn ts_to_us(ts: i64, tb: (i32, i32)) -> i64 {
    let (num, den) = (tb.0 as i128, tb.1 as i128);
    if den == 0 {
        return 0;
    }
    ((ts as i128) * num * 1_000_000 / den) as i64
}

fn bad(kind: &'static str, detail: &str) -> MediaError {
    MediaError::Format {
        kind,
        detail: detail.to_string(),
    }
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}
fn read_i32<R: Read>(r: &mut R) -> Result<i32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_le_bytes(b))
}
fn read_i64<R: Read>(r: &mut R) -> Result<i64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(i64::from_le_bytes(b))
}
fn read_u64<R: Read>(r: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic index (ffmpeg-free): keyframes at pts 0, 2 s, 4 s with
    /// non-key packets between them.
    fn sample() -> KeyframeIndex {
        let e = |pts: i64, kf: bool| KeyframeEntry {
            pts_us: Some(pts),
            dts_us: Some(pts),
            pos: pts, // arbitrary but distinct
            keyframe: kf,
        };
        let entries = vec![
            e(0, true),
            e(500_000, false),
            e(1_000_000, false),
            e(2_000_000, true),
            e(2_500_000, false),
            e(4_000_000, true),
            e(4_500_000, false),
        ];
        KeyframeIndex::from_parts((1, 30_000), entries)
    }

    #[test]
    fn keyframe_before_finds_preceding_keyframe() {
        let idx = sample();
        assert_eq!(idx.keyframe_count(), 3);
        // Before the first keyframe: none.
        assert_eq!(idx.keyframe_before(-1), None);
        // Exactly on a keyframe returns it.
        assert_eq!(idx.keyframe_before(0).map(|e| e.pts_us), Some(Some(0)));
        assert_eq!(
            idx.keyframe_before(2_000_000).map(|e| e.pts_us),
            Some(Some(2_000_000))
        );
        // Between keyframes returns the earlier one.
        assert_eq!(
            idx.keyframe_before(1_999_999).map(|e| e.pts_us),
            Some(Some(0))
        );
        assert_eq!(
            idx.keyframe_before(3_500_000).map(|e| e.pts_us),
            Some(Some(2_000_000))
        );
        // Past the last keyframe returns the last.
        assert_eq!(
            idx.keyframe_before(9_000_000).map(|e| e.pts_us),
            Some(Some(4_000_000))
        );
    }

    #[test]
    fn max_gop_frames_over_synthetic_index() {
        // sample(): keyframes at entry indices 0, 3, 5 over 7 packets.
        // GOPs: 3-0=3, 5-3=2, final 7-5=2 → longest is 3.
        let idx = sample();
        assert_eq!(idx.max_gop_frames(), 3);

        // No keyframes at all → 0 (and no panic).
        let none = KeyframeIndex::from_parts(
            (1, 1000),
            vec![
                KeyframeEntry {
                    pts_us: Some(0),
                    dts_us: Some(0),
                    pos: 0,
                    keyframe: false,
                },
                KeyframeEntry {
                    pts_us: Some(1),
                    dts_us: Some(1),
                    pos: 1,
                    keyframe: false,
                },
            ],
        );
        assert_eq!(none.max_gop_frames(), 0);

        // All-intra: every packet a keyframe → longest GOP is 1.
        let intra = KeyframeIndex::from_parts(
            (1, 1000),
            (0..5)
                .map(|i| KeyframeEntry {
                    pts_us: Some(i as i64),
                    dts_us: Some(i as i64),
                    pos: i as i64,
                    keyframe: true,
                })
                .collect(),
        );
        assert_eq!(intra.max_gop_frames(), 1);

        // A single leading keyframe with a long tail → the tail is one GOP.
        let one_kf = KeyframeIndex::from_parts(
            (1, 1000),
            (0..10)
                .map(|i| KeyframeEntry {
                    pts_us: Some(i as i64),
                    dts_us: Some(i as i64),
                    pos: i as i64,
                    keyframe: i == 0,
                })
                .collect(),
        );
        assert_eq!(one_kf.max_gop_frames(), 10);

        // Empty index → 0.
        let empty = KeyframeIndex::from_parts((1, 1000), vec![]);
        assert_eq!(empty.max_gop_frames(), 0);
    }

    #[test]
    fn save_load_roundtrip_is_equal() {
        let idx = sample();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("index.bin");
        idx.save(&path).expect("save");
        let loaded = KeyframeIndex::load(&path).expect("load");
        assert_eq!(idx, loaded);
    }

    #[test]
    fn load_rejects_bad_magic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.bin");
        std::fs::write(&path, b"NOPEnope").expect("write");
        assert!(matches!(
            KeyframeIndex::load(&path),
            Err(MediaError::Format {
                kind: "index.bin",
                ..
            })
        ));
    }

    #[test]
    fn unknown_timestamps_survive_roundtrip() {
        let entries = vec![KeyframeEntry {
            pts_us: None,
            dts_us: None,
            pos: -1,
            keyframe: true,
        }];
        let idx = KeyframeIndex::from_parts((1, 1000), entries);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("i.bin");
        idx.save(&path).expect("save");
        let loaded = KeyframeIndex::load(&path).expect("load");
        assert_eq!(loaded.entries[0].pts_us, None);
        assert_eq!(loaded.entries[0].dts_us, None);
        assert_eq!(loaded.entries[0].pos, -1);
        // A keyframe with no pts is excluded from the lookup list.
        assert_eq!(loaded.keyframe_count(), 0);
    }
}
