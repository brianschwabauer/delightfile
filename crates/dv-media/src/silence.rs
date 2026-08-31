//! Silence detection (§1) and its `silence.json` cache asset (§8.3).
//!
//! [`detect_silences`] decodes a file's audio (via [`AudioDecoder`], stereo f32
//! 48 kHz), measures RMS over short windows, coalesces below-threshold runs into
//! spans, and pads each span inward so ripple-deleting it never clips a speech
//! onset. [`write_silences`] / [`read_silences`] persist the result as a
//! versioned JSON cache asset.
//!
//! # `silence.json` format
//!
//! ```json
//! {"version":1,"spans":[{"start_us":1100000,"end_us":1900000}]}
//! ```
//!
//! serde is not a dependency of this crate, so the (trivial) JSON is written and
//! parsed by hand rather than derived.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::decode::{AudioDecoder, AUDIO_CHANNELS, AUDIO_RATE};
use crate::{Result, US_PER_SEC};

/// A span counts as silence below this RMS level (§1 default).
pub const SILENCE_THRESHOLD_DB: f32 = -40.0;
/// Runs shorter than this are not silences (§1 default).
pub const SILENCE_MIN_DUR_US: i64 = 500_000;
/// Each detected span is shrunk by this much per side (§1 default), so
/// ripple-deleting it leaves the speech onset/offset intact.
pub const SILENCE_PAD_US: i64 = 100_000;

/// RMS window length. ~10 ms is fine-grained enough to place a span edge well
/// inside the ±80 ms tolerance the UI cares about, while smoothing over the
/// instantaneous zero-crossings of a tone.
const WINDOW_US: i64 = 10_000;

/// The persisted cache format version.
const FORMAT_VERSION: i64 = 1;

/// One detected silence span, in source time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SilenceSpan {
    pub start_us: i64,
    pub end_us: i64,
}

/// Scan `path`'s audio for silences (§1).
///
/// Decodes via [`AudioDecoder`] (stereo f32 48 kHz), computes RMS over ~10 ms
/// windows (mono energy = mean of both channels), marks windows below
/// `threshold_db`, coalesces runs of `>= min_dur_us` into spans, then shrinks
/// each span by `pad_us` per side (spans that vanish are dropped). Returns spans
/// in source time, sorted and non-overlapping.
///
/// Errors with [`crate::MediaError::NoAudio`] if the file has no audio stream
/// (surfaced by [`AudioDecoder::open`]).
pub fn detect_silences(
    path: &Path,
    threshold_db: f32,
    min_dur_us: i64,
    pad_us: i64,
) -> Result<Vec<SilenceSpan>> {
    let mut dec = AudioDecoder::open(path)?;

    // Buffer the whole stream: silence scan runs on short clips and needs a
    // contiguous timeline anyway. `t0` anchors window k to source time.
    let mut samples: Vec<f32> = Vec::new();
    let mut t0: Option<i64> = None;
    while let Some(chunk) = dec.next_chunk()? {
        if t0.is_none() {
            t0 = Some(chunk.start_us);
        }
        samples.extend_from_slice(&chunk.samples);
    }
    let t0 = t0.unwrap_or(0);

    let total_frames = samples.len() / AUDIO_CHANNELS;
    let win_frames = (AUDIO_RATE as i64 * WINDOW_US / US_PER_SEC) as usize;
    let win_frames = win_frames.max(1);

    let mut spans: Vec<SilenceSpan> = Vec::new();
    // Frame index where the current silent run began, if we are in one.
    let mut run_start: Option<usize> = None;

    let mut frame = 0usize;
    while frame < total_frames {
        let end = (frame + win_frames).min(total_frames);

        // Mono energy = mean of both channels' squared amplitude over the window.
        let mut sum = 0f64;
        for f in frame..end {
            let l = samples[f * AUDIO_CHANNELS] as f64;
            let r = samples[f * AUDIO_CHANNELS + 1] as f64;
            sum += l * l + r * r;
        }
        let n = (end - frame) as f64;
        let mean = sum / (AUDIO_CHANNELS as f64 * n);
        let rms = mean.sqrt();
        let db = if rms > 0.0 {
            20.0 * rms.log10()
        } else {
            f64::NEG_INFINITY
        };
        let silent = db < threshold_db as f64;

        if silent {
            if run_start.is_none() {
                run_start = Some(frame);
            }
        } else if let Some(rs) = run_start.take() {
            push_span(&mut spans, t0, rs, frame, min_dur_us, pad_us);
        }

        frame = end;
    }
    // A silent run that reaches EOF still counts — flush it.
    if let Some(rs) = run_start.take() {
        push_span(&mut spans, t0, rs, total_frames, min_dur_us, pad_us);
    }

    Ok(spans)
}

/// Turn a silent frame-run `[start_frame, end_frame)` into a padded span and
/// push it, if it clears `min_dur_us` and survives padding.
fn push_span(
    spans: &mut Vec<SilenceSpan>,
    t0: i64,
    start_frame: usize,
    end_frame: usize,
    min_dur_us: i64,
    pad_us: i64,
) {
    let start_us = t0 + frames_to_us(start_frame);
    let end_us = t0 + frames_to_us(end_frame);
    // Duration test is on the *raw* run, before padding (§1).
    if end_us - start_us < min_dur_us {
        return;
    }
    let s = start_us + pad_us;
    let e = end_us - pad_us;
    if s < e {
        spans.push(SilenceSpan {
            start_us: s,
            end_us: e,
        });
    }
}

fn frames_to_us(frames: usize) -> i64 {
    frames as i64 * US_PER_SEC / AUDIO_RATE as i64
}

/// Write the §8.3 cache asset atomically (temp sibling + rename), matching the
/// other cache writers in this crate.
pub fn write_silences(json_path: &Path, spans: &[SilenceSpan]) -> Result<()> {
    let mut json = String::from("{\"version\":");
    let _ = write!(json, "{FORMAT_VERSION}");
    json.push_str(",\"spans\":[");
    for (i, sp) in spans.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let _ = write!(
            json,
            "{{\"start_us\":{},\"end_us\":{}}}",
            sp.start_us, sp.end_us
        );
    }
    json.push_str("]}");

    let mut tmp = json_path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, json.as_bytes())?;
    fs::rename(&tmp, json_path)?;
    Ok(())
}

/// Read the §8.3 cache asset. Returns `None` on a missing, unparseable, or
/// wrong-version file — the caller regenerates in that case.
pub fn read_silences(json_path: &Path) -> Option<Vec<SilenceSpan>> {
    let text = fs::read_to_string(json_path).ok()?;
    // Version must be present and current.
    if *ints_after(&text, "\"version\"").first()? != FORMAT_VERSION {
        return None;
    }
    let starts = ints_after(&text, "\"start_us\"");
    let ends = ints_after(&text, "\"end_us\"");
    if starts.len() != ends.len() {
        return None;
    }
    Some(
        starts
            .into_iter()
            .zip(ends)
            .map(|(start_us, end_us)| SilenceSpan { start_us, end_us })
            .collect(),
    )
}

/// Every integer that immediately follows an occurrence of `key` (a JSON key,
/// including its quotes), in document order. Enough to parse our own trivial,
/// fixed-shape output without a JSON dependency.
fn ints_after(text: &str, key: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(key) {
        let after = &rest[pos + key.len()..];
        if let Some(n) = parse_leading_int(after) {
            out.push(n);
        }
        rest = after;
    }
    out
}

/// Parse the first integer in `s`, skipping a leading `:` and whitespace. `None`
/// if no digits are found before the first non-numeric byte.
fn parse_leading_int(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b':' || bytes[i].is_ascii_whitespace()) {
        i += 1;
    }
    let start = i;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let digits_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    s[start..i].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_preserves_spans() {
        let spans = vec![
            SilenceSpan {
                start_us: 1_100_000,
                end_us: 1_900_000,
            },
            SilenceSpan {
                start_us: 4_250_000,
                end_us: 6_000_000,
            },
        ];
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("silence.json");
        write_silences(&path, &spans).expect("write");
        // No leftover temp sibling after the atomic rename.
        assert!(!path.with_extension("json.tmp").exists());
        let loaded = read_silences(&path).expect("read");
        assert_eq!(loaded, spans);
    }

    #[test]
    fn empty_spans_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("silence.json");
        write_silences(&path, &[]).expect("write");
        assert_eq!(read_silences(&path), Some(vec![]));
    }

    #[test]
    fn read_rejects_wrong_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("silence.json");
        fs::write(
            &path,
            b"{\"version\":2,\"spans\":[{\"start_us\":0,\"end_us\":1}]}",
        )
        .expect("write");
        assert_eq!(read_silences(&path), None);
    }

    #[test]
    fn read_missing_or_garbage_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_silences(&dir.path().join("nope.json")), None);
        let garbage = dir.path().join("garbage.json");
        fs::write(&garbage, b"not json at all").expect("write");
        assert_eq!(read_silences(&garbage), None);
    }

    #[test]
    fn parse_leading_int_handles_sign_and_colon() {
        assert_eq!(parse_leading_int(": 42}"), Some(42));
        assert_eq!(parse_leading_int(":-7,"), Some(-7));
        assert_eq!(parse_leading_int(":\"x\""), None);
    }
}
