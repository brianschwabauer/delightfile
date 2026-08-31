//! Sync external audio to a camera clip by waveform cross-correlation (§5).
//!
//! §5's "Sync external audio" feature: a lav mic or field recorder captures the
//! same performance as the camera, started at a different moment. This module
//! recovers the time offset between the two files with **no ML** — a two-stage
//! cross-correlation:
//!
//! 1. **Coarse pass** on the cached DVWF peak envelopes (§ crate::waveform).
//!    The per-bin amplitude envelope is decimated so the full-overlap search is
//!    cheap regardless of file length, then normalized cross-correlation finds
//!    the best bin lag and a confidence score.
//! 2. **Fine pass** on decoded 48 kHz audio around the coarse alignment: the
//!    loudest ~10 s of the camera is decoded together with the matching stretch
//!    of the external file and correlated at sample resolution (±~11 ms) for a
//!    sample-accurate offset.
//!
//! If the coarse peak does not stand out from the field (§5: "if the
//! correlation peak is weak … say so instead of guessing"), the result is
//! reported as low confidence and the caller inserts nothing.

use std::path::Path;

use crate::decode::{AudioDecoder, AUDIO_CHANNELS, AUDIO_RATE};
use crate::waveform::{generate_waveform, Waveform};
use crate::US_PER_SEC;

/// Confidence at/above which [`SyncResult::confident`] returns true (§5). Chosen
/// so a clean match (a distinct correlation peak) passes while ambiguous or
/// noise-only correlations fall below it.
const CONFIDENCE_THRESHOLD: f32 = 0.5;

/// Coarse-pass target length: each envelope is decimated (max-pooled) so the
/// longer of the two is at most this many bins. A full ±overlap search is then
/// O(TARGET²) ≈ 34 M ops — well under ~1 s even for hour-long files (§5).
const COARSE_TARGET_BINS: usize = 4096;

/// Fine pass decodes the loudest window of this length around the coarse
/// alignment (§5 suggests ~10 s; 3 s is ample for a sample-accurate broadband
/// correlation and keeps the O(field × window) scan well under a second, even
/// in an unoptimized test build).
const FINE_WINDOW_US: i64 = 3 * US_PER_SEC;

/// Fine pass decodes this much extra external audio on each side of the
/// expected window, so the ±sample search stays inside decoded material.
const FINE_MARGIN_US: i64 = 60_000;

/// Fine pass searches this many samples either side of the coarse alignment
/// (±~17 ms at 48 kHz). The coarse pass localizes to within ~one 5.3 ms bin
/// (±256 samples), so the true peak lies well inside this span; the rest of
/// the span is the floor against which the peak's prominence — the reported
/// confidence — is measured.
const FINE_FIELD_SAMPLES: i64 = 600;

/// When scoring fine-pass peak prominence, sample lags within this radius
/// (~3 ms) of the best are treated as the same peak (skip the main lobe).
const FINE_EXCLUDE_SAMPLES: i64 = 150;

/// Below this much envelope overlap (~2 s) the coarse correlation is not
/// trustworthy: report low confidence rather than error (§5).
const MIN_OVERLAP_BINS: usize = 375;

/// Result of a sync correlation. `offset_us` is where the EXTERNAL file's
/// t=0 falls on the CAMERA file's clock: camera_time = external_time + offset_us
/// (negative = the external recorder started after the camera).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncResult {
    pub offset_us: i64,
    /// 0–1; how much the best correlation peak stands out from the field.
    pub confidence: f32,
}

impl SyncResult {
    /// §5: refuse to guess — the caller inserts nothing below this. Threshold
    /// is [`CONFIDENCE_THRESHOLD`] (0.5).
    pub fn confident(&self) -> bool {
        self.confidence >= CONFIDENCE_THRESHOLD
    }
}

/// Pure coarse pass, exposed for tests: normalized cross-correlation of two
/// mono envelopes (arbitrary equal sample rate). Returns `(lag_in_bins,
/// confidence)` where `lag` is the shift of `b` relative to `a` maximizing
/// correlation, i.e. the lag L for which `a[i] ≈ b[i - L]`. The full ±overlap
/// range is searched.
///
/// Correlation is **normalized** (mean-subtracted, energy-normalized per lag),
/// so a level difference or DC offset between the two mics does not matter.
/// Confidence is the prominence of the best peak over the best *distinct*
/// peak elsewhere: `1 − second_best / best` (0 when the best correlation is
/// non-positive). Identical-but-shifted envelopes score near 1; unrelated
/// noise scores near 0.
pub fn correlate_envelopes(a: &[f32], b: &[f32]) -> (i64, f32) {
    if a.is_empty() || b.is_empty() {
        return (0, 0.0);
    }
    let min_overlap = min_overlap_for(a.len().min(b.len()));
    // Radius (in bins) around the best lag treated as the same peak when
    // measuring prominence — skips the correlation main lobe's shoulder.
    let exclude = (a.len().min(b.len()) / 64).max(3) as i64;

    let lo = -(b.len() as i64 - 1);
    let hi = a.len() as i64 - 1;

    let mut best_lag = 0i64;
    let mut best = f32::NEG_INFINITY;
    // Track every scored lag so the second-best distinct peak can be found.
    let mut scored: Vec<(i64, f32)> = Vec::new();
    for l in lo..=hi {
        if let Some(ncc) = ncc_at(a, b, l, min_overlap) {
            scored.push((l, ncc));
            if ncc > best {
                best = ncc;
                best_lag = l;
            }
        }
    }
    if scored.is_empty() {
        return (0, 0.0);
    }

    let second = scored
        .iter()
        .filter(|(l, _)| (*l - best_lag).abs() > exclude)
        .map(|(_, v)| *v)
        .fold(f32::NEG_INFINITY, f32::max);

    let confidence = if best <= 0.0 {
        0.0
    } else if !second.is_finite() {
        // No distinct competing peak at all — a clean, isolated match.
        best.clamp(0.0, 1.0)
    } else {
        (1.0 - second.max(0.0) / best).clamp(0.0, 1.0)
    };
    (best_lag, confidence)
}

/// Full pipeline: coarse on cached waveform envelopes (generating them if the
/// cache files are missing — reuses [`generate_waveform`]), then a fine pass at
/// 48 kHz for a sample-accurate offset.
///
/// Errors if the camera file has no audio stream (§5). A too-short or weak
/// correlation is **not** an error — it returns a low-confidence result.
pub fn sync_offset(
    camera: &Path,
    camera_waveform_pk: &Path,
    external: &Path,
    external_waveform_pk: &Path,
) -> anyhow::Result<SyncResult> {
    let cam_wf = ensure_waveform(camera, camera_waveform_pk)?;
    let ext_wf = ensure_waveform(external, external_waveform_pk)?;

    let cam_env = envelope(&cam_wf);
    let ext_env = envelope(&ext_wf);
    let bin_dt_us = bin_dt_us(&cam_wf);

    // --- Coarse pass: decimate, correlate, then refine the lag at full rate.
    let factor = decimation_factor(cam_env.len().max(ext_env.len()));
    let dec_cam = decimate(&cam_env, factor);
    let dec_ext = decimate(&ext_env, factor);
    let (coarse_lag_dec, confidence) = correlate_envelopes(&dec_cam, &dec_ext);
    let coarse_lag = refine_lag_full(&cam_env, &ext_env, coarse_lag_dec * factor as i64, factor);

    let coarse_offset_us = (coarse_lag as f64 * bin_dt_us).round() as i64;

    // Guard: enough overlapping envelope to believe the coarse result at all?
    // Too short → report low confidence rather than error (§5).
    let overlap = overlap_len(cam_env.len(), ext_env.len(), coarse_lag);
    if overlap < MIN_OVERLAP_BINS {
        return Ok(SyncResult {
            offset_us: coarse_offset_us,
            confidence: confidence.min(0.2),
        });
    }

    // --- Fine pass at 48 kHz. The coarse pass reliably *localizes* the offset
    // (the argmax bin is right even when the envelope's peak is not prominent);
    // the fine pass both refines it to sample accuracy and — since the raw
    // audio decorrelates within a few samples — scores how sharp/confident the
    // match really is. That full-resolution prominence is the reported
    // confidence (§5: "if the correlation peak is weak … say so"). If too
    // little audio decodes to correlate, fall back to the coarse figures.
    let result = match fine_offset(
        camera,
        external,
        &cam_env,
        &ext_env,
        coarse_lag,
        coarse_offset_us,
        bin_dt_us,
    )? {
        Some((offset_us, fine_conf)) => SyncResult {
            offset_us,
            confidence: fine_conf,
        },
        None => SyncResult {
            offset_us: coarse_offset_us,
            confidence,
        },
    };
    Ok(result)
}

/// Load the waveform, generating it to `pk` first if the cache file is missing.
fn ensure_waveform(media: &Path, pk: &Path) -> anyhow::Result<Waveform> {
    if !pk.exists() {
        if let Some(parent) = pk.parent() {
            std::fs::create_dir_all(parent)?;
        }
        generate_waveform(media, pk)?;
    }
    Ok(Waveform::load(pk)?)
}

/// DVWF peaks → per-bin amplitude envelope in `[0, 1]`: `max(|min|, |max|)`
/// scaled off the i8 full-scale (±127).
fn envelope(wf: &Waveform) -> Vec<f32> {
    wf.peaks
        .iter()
        .map(|(mn, mx)| {
            let amp = (*mn as i32).abs().max((*mx as i32).abs());
            amp as f32 / 127.0
        })
        .collect()
}

/// Microseconds per envelope bin, from the waveform header.
fn bin_dt_us(wf: &Waveform) -> f64 {
    let rate = wf.meta.sample_rate.max(1) as f64;
    wf.meta.samples_per_bucket as f64 * US_PER_SEC as f64 / rate
}

/// Minimum overlap length required for a lag to be scored (guards against
/// spurious perfect correlations over a handful of samples).
fn min_overlap_for(min_len: usize) -> usize {
    (min_len / 4).max(8)
}

/// Overlap (in bins) between `a` and `b` at lag `l` (pairing `a[i]` with
/// `b[i - l]`).
fn overlap_len(a_len: usize, b_len: usize, l: i64) -> usize {
    let i0 = l.max(0);
    let i1 = (a_len as i64).min(b_len as i64 + l);
    (i1 - i0).max(0) as usize
}

/// Normalized cross-correlation of `a` against `b` at lag `l`, pairing `a[i]`
/// with `b[i - l]`. `None` if the overlap is below `min_overlap` or either
/// window is flat (zero variance).
fn ncc_at(a: &[f32], b: &[f32], l: i64, min_overlap: usize) -> Option<f32> {
    let i0 = l.max(0);
    let i1 = (a.len() as i64).min(b.len() as i64 + l);
    if i1 - i0 < min_overlap as i64 {
        return None;
    }
    let (i0, i1) = (i0 as usize, i1 as usize);
    let av = &a[i0..i1];
    let bv = &b[(i0 as i64 - l) as usize..(i1 as i64 - l) as usize];
    let n = av.len() as f32;

    let mean_a = av.iter().sum::<f32>() / n;
    let mean_b = bv.iter().sum::<f32>() / n;

    let mut cov = 0.0f32;
    let mut var_a = 0.0f32;
    let mut var_b = 0.0f32;
    for (&x, &y) in av.iter().zip(bv.iter()) {
        let dx = x - mean_a;
        let dy = y - mean_b;
        cov += dx * dy;
        var_a += dx * dx;
        var_b += dy * dy;
    }
    let denom = (var_a * var_b).sqrt();
    if denom <= f32::EPSILON {
        return None;
    }
    Some(cov / denom)
}

/// Decimation factor so `len / factor <= COARSE_TARGET_BINS`.
fn decimation_factor(len: usize) -> usize {
    if len <= COARSE_TARGET_BINS {
        1
    } else {
        len.div_ceil(COARSE_TARGET_BINS)
    }
}

/// Max-pool `env` into groups of `factor` bins (peak-preserving downsample).
fn decimate(env: &[f32], factor: usize) -> Vec<f32> {
    if factor <= 1 {
        return env.to_vec();
    }
    env.chunks(factor)
        .map(|c| c.iter().copied().fold(0.0f32, f32::max))
        .collect()
}

/// Refine a decimated coarse lag back to full-bin resolution by searching the
/// full-rate NCC in a ±`factor`-bin neighborhood of `approx_full`.
fn refine_lag_full(a: &[f32], b: &[f32], approx_full: i64, factor: usize) -> i64 {
    if factor <= 1 {
        return approx_full;
    }
    let min_overlap = min_overlap_for(a.len().min(b.len()));
    let span = factor as i64;
    let mut best_lag = approx_full;
    let mut best = f32::NEG_INFINITY;
    for l in (approx_full - span)..=(approx_full + span) {
        if let Some(ncc) = ncc_at(a, b, l, min_overlap) {
            if ncc > best {
                best = ncc;
                best_lag = l;
            }
        }
    }
    best_lag
}

/// Fine pass: decode the loudest ~10 s camera window and the matching external
/// stretch, correlate at 48 kHz, and return the sample-accurate offset (µs)
/// together with the peak's prominence as a confidence in `[0, 1]`. `None` if
/// too little audio decoded to correlate — the caller keeps the coarse figures.
fn fine_offset(
    camera: &Path,
    external: &Path,
    cam_env: &[f32],
    ext_env: &[f32],
    coarse_lag: i64,
    coarse_offset_us: i64,
    bin_dt_us: f64,
) -> anyhow::Result<Option<(i64, f32)>> {
    // Loudest window of the camera envelope that also overlaps the external
    // file at the coarse lag (external bin = camera bin − coarse_lag).
    let win_bins = ((FINE_WINDOW_US as f64) / bin_dt_us).round() as usize;
    let ov_start = coarse_lag.max(0) as usize;
    let ov_end = (cam_env.len() as i64)
        .min(ext_env.len() as i64 + coarse_lag)
        .max(0) as usize;
    if ov_end <= ov_start {
        return Ok(None);
    }
    let start_bin = loudest_window(cam_env, ov_start, ov_end, win_bins);
    let cam_start_us = (start_bin as f64 * bin_dt_us).round() as i64;

    // Decode the camera window and a slightly larger external window centered
    // on where the coarse offset predicts the same content sits.
    let (cam0, cam) = decode_mono_window(camera, cam_start_us, FINE_WINDOW_US)?;
    let ext_expected_start = cam_start_us - coarse_offset_us;
    let (ext0, ext) = decode_mono_window(
        external,
        ext_expected_start - FINE_MARGIN_US,
        FINE_WINDOW_US + 2 * FINE_MARGIN_US,
    )?;
    if cam.len() < AUDIO_RATE as usize || ext.len() < cam.len() {
        return Ok(None);
    }

    let dt_us = US_PER_SEC as f64 / AUDIO_RATE as f64;
    // Sample shift k pairs cam[i] with ext[i + k]; k0 is the coarse prediction.
    let k0 = (((cam0 - ext0) as f64 - coarse_offset_us as f64) / dt_us).round() as i64;
    let min_overlap = (cam.len() / 2).max(AUDIO_RATE as usize / 4);

    // Scan a wide field of sample lags so the best peak can be scored against
    // the surrounding correlation floor.
    let mut best_k = k0;
    let mut best = f32::NEG_INFINITY;
    let mut scored: Vec<(i64, f32)> = Vec::new();
    for k in (k0 - FINE_FIELD_SAMPLES)..=(k0 + FINE_FIELD_SAMPLES) {
        // ncc_at pairs a[i] with b[i - l]; we want cam[i] vs ext[i + k] = l=-k.
        if let Some(ncc) = ncc_at(&cam, &ext, -k, min_overlap) {
            scored.push((k, ncc));
            if ncc > best {
                best = ncc;
                best_k = k;
            }
        }
    }
    if scored.is_empty() || !best.is_finite() || best <= 0.0 {
        return Ok(None);
    }

    // Confidence = prominence of the peak over the best distinct competitor
    // outside the main lobe. Raw broadband audio decorrelates within a few
    // samples, so a true match towers over the field (≈1) while an accidental
    // alignment does not.
    let second = scored
        .iter()
        .filter(|(k, _)| (*k - best_k).abs() > FINE_EXCLUDE_SAMPLES)
        .map(|(_, v)| *v)
        .fold(f32::NEG_INFINITY, f32::max);
    let confidence = if second.is_finite() {
        (1.0 - second.max(0.0) / best).clamp(0.0, 1.0)
    } else {
        best.clamp(0.0, 1.0)
    };

    // offset = camera_time − external_time = cam0 − ext0 − k·dt.
    let offset = ((cam0 - ext0) as f64 - best_k as f64 * dt_us).round() as i64;
    Ok(Some((offset, confidence)))
}

/// Index of the `win`-bin window in `env[start..end]` with the greatest summed
/// energy (Σ amplitude²). Clamped to the available span.
fn loudest_window(env: &[f32], start: usize, end: usize, win: usize) -> usize {
    let span = end - start;
    if win >= span {
        return start;
    }
    // Sliding sum of squared amplitude.
    let mut acc: f32 = env[start..start + win].iter().map(|v| v * v).sum();
    let mut best_sum = acc;
    let mut best_start = start;
    for s in (start + 1)..=(end - win) {
        acc -= env[s - 1] * env[s - 1];
        acc += env[s + win - 1] * env[s + win - 1];
        if acc > best_sum {
            best_sum = acc;
            best_start = s;
        }
    }
    best_start
}

/// Decode `[start_us, start_us + dur_us)` of `path` as mono `(L + R) / 2`
/// 48 kHz samples. Returns the actual start time of the first sample (the
/// container may seek slightly before the target) and the samples.
fn decode_mono_window(path: &Path, start_us: i64, dur_us: i64) -> anyhow::Result<(i64, Vec<f32>)> {
    let start_us = start_us.max(0);
    let end_us = start_us + dur_us;
    let mut dec = AudioDecoder::open(path)?;
    dec.seek(start_us)?;

    let mut out: Vec<f32> = Vec::new();
    let mut out_start: Option<i64> = None;
    let step = US_PER_SEC / AUDIO_RATE as i64;
    while let Some(chunk) = dec.next_chunk()? {
        let frames = chunk.samples.len() / AUDIO_CHANNELS;
        for n in 0..frames {
            let t = chunk.start_us + n as i64 * step;
            if t < start_us {
                continue;
            }
            if t >= end_us {
                return Ok((out_start.unwrap_or(start_us), out));
            }
            if out_start.is_none() {
                out_start = Some(t);
            }
            let l = chunk.samples[2 * n];
            let r = chunk.samples[2 * n + 1];
            out.push(0.5 * (l + r));
        }
    }
    Ok((out_start.unwrap_or(start_us), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random envelope (no dependency): a small LCG.
    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (s >> 33) as f32 / u32::MAX as f32 // 0..1
            })
            .collect()
    }

    /// Build `b` as `a` shifted right by `shift` (positive) — `b[i] = a[i-shift]`
    /// with leading zeros — for which the recovered lag must be `-shift`.
    fn shift_right(a: &[f32], shift: usize) -> Vec<f32> {
        let mut b = vec![0.0f32; a.len()];
        b[shift..].copy_from_slice(&a[..a.len() - shift]);
        b
    }

    #[test]
    fn recovers_positive_shift() {
        let a = noise(256, 1);
        // b delayed by 20 → a[i] = b[i + 20], i.e. lag L = -20.
        let b = shift_right(&a, 20);
        let (lag, conf) = correlate_envelopes(&a, &b);
        assert_eq!(lag, -20, "delayed b recovers lag -shift");
        assert!(conf > 0.5, "clean match is confident: {conf}");
    }

    #[test]
    fn recovers_negative_shift() {
        let a = noise(256, 2);
        // Put the shift on a instead: a delayed by 15 → recovered lag +15.
        let b = a.clone();
        let a_delayed = shift_right(&a, 15);
        let (lag, conf) = correlate_envelopes(&a_delayed, &b);
        assert_eq!(lag, 15, "a delayed recovers positive lag");
        assert!(conf > 0.5, "clean match is confident: {conf}");
    }

    #[test]
    fn level_scaled_copy_still_matches() {
        let a = noise(256, 3);
        // Scale (0.2×) and add a DC offset — normalization must absorb both.
        let scaled: Vec<f32> = a.iter().map(|v| 0.2 * v + 0.5).collect();
        let b = shift_right(&scaled, 12);
        let (lag, conf) = correlate_envelopes(&a, &b);
        assert_eq!(lag, -12, "normalized correlation is level/DC invariant");
        assert!(conf > 0.5, "scaled copy still confident: {conf}");
    }

    #[test]
    fn unrelated_noise_is_low_confidence() {
        let a = noise(256, 10);
        let b = noise(256, 999);
        let (_lag, conf) = correlate_envelopes(&a, &b);
        assert!(conf < 0.3, "uncorrelated noise scores low: {conf}");
    }

    #[test]
    fn empty_inputs_are_safe() {
        assert_eq!(correlate_envelopes(&[], &[1.0, 2.0]), (0, 0.0));
        assert_eq!(correlate_envelopes(&[1.0, 2.0], &[]), (0, 0.0));
    }

    #[test]
    fn confident_uses_threshold() {
        let hi = SyncResult {
            offset_us: 0,
            confidence: 0.8,
        };
        let lo = SyncResult {
            offset_us: 0,
            confidence: 0.3,
        };
        assert!(hi.confident());
        assert!(!lo.confident());
    }

    #[test]
    fn decimate_preserves_peaks() {
        let env = vec![0.1, 0.9, 0.2, 0.3, 0.8, 0.1];
        let d = decimate(&env, 2);
        assert_eq!(d, vec![0.9, 0.3, 0.8]);
        assert_eq!(decimate(&env, 1), env);
    }
}
