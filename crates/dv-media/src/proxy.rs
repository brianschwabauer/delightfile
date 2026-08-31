//! Scrub proxies — scrubbing layer 2 (§4.3).
//!
//! Some footage is slow to scrub: long-GOP codecs, 4K, or formats the hardware
//! can't decode. For those we generate an **all-intra H.264** proxy in the
//! background; preview/scrub then decodes the proxy (every frame a keyframe →
//! one-frame seeks anywhere), while **export always uses the original** (§4.3).
//!
//! Two pieces live here:
//! - [`proxy_needed`] — the §4.3 decision rule (make a proxy *unless* the source
//!   is already cheap to scrub). Missing information forces a proxy.
//! - [`generate_proxy`] — spawns the `ffmpeg` CLI to encode the proxy. Per §2,
//!   asset/export encoding shells out to the CLI rather than driving libav
//!   in-process; the proxy is CFR, which is what normalizes VFR sources (§14).
//!
//! Only the *decision* and *generation* live here. The cache paths
//! (`proxy.mp4`, `proxy_index.bin`) belong to [`crate::cache`]; scheduling the
//! background job belongs to `dv-app` (§9). This function is blocking and must
//! be called from a worker thread.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{ensure_ffmpeg, MediaError, Result};

/// Proxy encode parameters (§4.3 defaults). All-intra H.264 at quarter-ish
/// resolution — see [`generate_proxy`] for how these map to ffmpeg flags.
#[derive(Debug, Clone, Copy)]
pub struct ProxyOpts {
    /// Cap on the proxy's output height in pixels. The proxy targets
    /// `min(max_height, source_height / 2)` and never upscales. Default 960.
    pub max_height: u32,
    /// libx264 constant-rate-factor (lower = higher quality/bigger). Default 23.
    pub crf: u32,
    /// libx264 speed/efficiency preset. Default "veryfast" — proxies are
    /// throwaway, so favor encode speed.
    pub preset: &'static str,
}

impl Default for ProxyOpts {
    fn default() -> Self {
        ProxyOpts {
            max_height: 960,
            crf: 23,
            preset: "veryfast",
        }
    }
}

/// Codecs a modern GPU can typically decode fast enough to scrub without a
/// proxy (§4.3). Anything outside this set always gets a proxy.
const HW_FRIENDLY_CODECS: [&str; 4] = ["h264", "hevc", "vp9", "av1"];

/// The §4.3 decision rule. Make a proxy **unless** the source is already cheap
/// to scrub, i.e. *all* of:
/// - its codec is one hardware commonly decodes (`h264`/`hevc`/`vp9`/`av1`),
/// - hardware decode was actually *verified* for this file (`hw_verified`),
/// - the longest GOP is ≤ 60 frames (short seeks), and
/// - the height is ≤ 1080.
///
/// Missing information (`None`) counts **against** skipping: an unknown codec,
/// height, or GOP length means we can't prove scrubbing is cheap, so we proxy.
pub fn proxy_needed(
    video_codec: Option<&str>,
    height: Option<u32>,
    hw_verified: bool,
    max_gop_frames: Option<u32>,
) -> bool {
    let codec_ok = video_codec.is_some_and(|c| HW_FRIENDLY_CODECS.contains(&c));
    let height_ok = height.is_some_and(|h| h <= 1080);
    let gop_ok = max_gop_frames.is_some_and(|g| g <= 60);
    let can_skip = codec_ok && hw_verified && gop_ok && height_ok;
    !can_skip
}

/// Generate an all-intra H.264 proxy of `src` at `dst` (an `.mp4`) by spawning
/// the `ffmpeg` CLI (§4.3, §2). **Blocking** — call from a worker thread.
///
/// Encode recipe (§4.3): `-g 1` (every frame a keyframe), libx264 CRF/preset
/// from `opts`, `yuv420p`, height `min(opts.max_height, src_h/2)` (rounded to
/// even, never upscaling) with width auto (`scale=-2`) to preserve aspect, CFR
/// at the source's average fps (this normalizes VFR sources — §14), AAC audio
/// at the *original* sample rate when the source has audio, and `+faststart`.
///
/// Atomicity: encodes to a temp sibling `<dst>.part` and renames into place on
/// success, so a crashed/partial encode can never be mistaken for a finished
/// proxy; the `.part` (and a stderr log sibling) are removed on failure.
///
/// `progress` is called with a fraction in `0..=1` as the encode advances. If
/// the source duration is unknown, progress is indeterminate and `progress` is
/// simply never called.
pub fn generate_proxy(
    src: &Path,
    dst: &Path,
    opts: &ProxyOpts,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    ensure_ffmpeg();

    // Probe in-process for the geometry/audio the CLI args need (§4.3). Reuses
    // the crate's probe so codec/height/fps/rate handling stays in one place.
    let info = crate::probe(src)?;
    let src_h = info
        .height
        .ok_or_else(|| MediaError::NoVideo(src.display().to_string()))?;

    // "Quarter-ish resolution capped at 960px height": halve the source height,
    // cap at opts.max_height, round down to even (libx264/yuv420p need even
    // dimensions), and never upscale. src_h/2 already guarantees no upscale.
    let mut target_h = opts.max_height.min(src_h / 2) & !1;
    if target_h < 2 {
        target_h = 2;
    }

    // CFR at the source's average fps. None → omit -r (still request CFR).
    let fps = match (info.fps_num, info.fps_den) {
        (Some(n), Some(d)) if n > 0 && d > 0 => Some((n, d)),
        _ => None,
    };

    // Temp sibling to rename from, plus a stderr log sibling (a file, not a
    // pipe, so a large stderr can never deadlock our stdout progress read).
    let part = with_suffix(dst, ".part");
    let log = with_suffix(dst, ".part.log");
    let log_file = std::fs::File::create(&log)?;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-nostdin")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(src)
        // Only the best video stream, and the first audio stream if present.
        .arg("-map")
        .arg("0:v:0");
    if info.has_audio {
        cmd.arg("-map").arg("0:a:0?");
    }
    cmd.arg("-c:v")
        .arg("libx264")
        .arg("-crf")
        .arg(opts.crf.to_string())
        .arg("-preset")
        .arg(opts.preset)
        .arg("-g")
        .arg("1")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-vf")
        .arg(format!("scale=-2:{target_h}"))
        .arg("-fps_mode")
        .arg("cfr");
    if let Some((num, den)) = fps {
        cmd.arg("-r").arg(format!("{num}/{den}"));
    }
    if info.has_audio {
        // AAC at the original sample rate ("passthrough of the original sample
        // rate" — same rate, re-encoded to AAC).
        cmd.arg("-c:a").arg("aac");
        if let Some(rate) = info.sample_rate {
            cmd.arg("-ar").arg(rate.to_string());
        }
    } else {
        cmd.arg("-an");
    }
    cmd.arg("-movflags")
        .arg("+faststart")
        // The output filename is `<dst>.part`, whose extension ffmpeg can't map
        // to a muxer — name the format explicitly so the temp sibling works.
        .arg("-f")
        .arg("mp4")
        .arg("-progress")
        .arg("pipe:1")
        .arg(&part);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log_file));

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&log);
            if e.kind() == std::io::ErrorKind::NotFound {
                return Err(MediaError::Cli(
                    "`ffmpeg` binary not found on PATH — delightvideo requires ffmpeg ≥ 6.0 on \
                     PATH (see README)"
                        .to_string(),
                ));
            }
            return Err(MediaError::Io(e));
        }
    };

    // Stream `-progress pipe:1` from stdout, converting out_time against the
    // probed duration. ffmpeg emits `out_time_us=` (and legacy `out_time_ms=`,
    // which is likewise microseconds) once per progress block.
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = line?;
            let us = line
                .strip_prefix("out_time_us=")
                .or_else(|| line.strip_prefix("out_time_ms="))
                .and_then(|v| v.trim().parse::<i64>().ok());
            if let (Some(us), Some(dur)) = (us, info.duration_us) {
                if dur > 0 {
                    let frac = (us as f64 / dur as f64).clamp(0.0, 1.0) as f32;
                    progress(frac);
                }
            }
        }
    }

    let status = child.wait()?;
    if !status.success() {
        let tail = stderr_tail(&log);
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&part);
        return Err(MediaError::Cli(format!(
            "ffmpeg exited with {status} while encoding proxy of {}: {tail}",
            src.display()
        )));
    }

    // Success: publish atomically, then ensure a final 1.0 tick (the last
    // progress line usually lands just shy of the full duration).
    std::fs::rename(&part, dst)?;
    let _ = std::fs::remove_file(&log);
    if info.duration_us.is_some() {
        progress(1.0);
    }
    Ok(())
}

/// `<path><suffix>` as a sibling path (e.g. `proxy.mp4` + `.part`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.to_path_buf().into_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// The last ~800 bytes of the stderr log, lossily decoded, for error messages.
/// Best-effort: an unreadable log yields an empty string rather than masking
/// the original failure.
fn stderr_tail(log: &Path) -> String {
    const TAIL: u64 = 800;
    let Ok(mut f) = std::fs::File::open(log) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > TAIL {
        let _ = f.seek(SeekFrom::Start(len - TAIL));
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_needed_skips_the_easy_case() {
        // h264, verified hw, short GOP, ≤1080 → cheap to scrub, no proxy.
        assert!(!proxy_needed(Some("h264"), Some(1080), true, Some(60)));
        assert!(!proxy_needed(Some("hevc"), Some(720), true, Some(1)));
        assert!(!proxy_needed(Some("vp9"), Some(360), true, Some(30)));
        assert!(!proxy_needed(Some("av1"), Some(1080), true, Some(12)));
    }

    #[test]
    fn proxy_needed_each_condition_forces_a_proxy() {
        // Baseline is the happy skip case; break one condition at a time.
        // Unfriendly codec.
        assert!(proxy_needed(Some("prores"), Some(1080), true, Some(60)));
        // Hardware not verified.
        assert!(proxy_needed(Some("h264"), Some(1080), false, Some(60)));
        // GOP too long.
        assert!(proxy_needed(Some("h264"), Some(1080), true, Some(61)));
        // Height too tall (4K).
        assert!(proxy_needed(Some("h264"), Some(2160), true, Some(60)));
    }

    #[test]
    fn proxy_needed_missing_info_forces_a_proxy() {
        // Unknown codec / height / GOP each count against skipping.
        assert!(proxy_needed(None, Some(1080), true, Some(60)));
        assert!(proxy_needed(Some("h264"), None, true, Some(60)));
        assert!(proxy_needed(Some("h264"), Some(1080), true, None));
        // Everything unknown.
        assert!(proxy_needed(None, None, false, None));
    }

    #[test]
    fn with_suffix_appends() {
        assert_eq!(
            with_suffix(Path::new("/c/h/proxy.mp4"), ".part"),
            PathBuf::from("/c/h/proxy.mp4.part")
        );
    }
}
