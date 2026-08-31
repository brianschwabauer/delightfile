//! Waveform peaks (§5).
//!
//! Decodes audio, resamples/mixes to **mono f32 48 kHz** (swresample), then
//! records a min/max pair per 256 samples, quantized to `i8`. The strip UI
//! draws clips' waveforms from this. Peaks are precomputed to the cache on
//! import (§5); export uses the real audio, not these.
//!
//! # `waveform.pk` binary format (little-endian, versioned)
//!
//! ```text
//! offset  size  field
//! 0       4     magic              = b"DVWF"
//! 4       2     version            = 1 (u16)
//! 6       2     reserved           = 0 (u16)
//! 8       4     sample_rate        = 48000 (u32)
//! 12      4     samples_per_bucket = 256 (u32)
//! 16      8     count              (u64)   — number of min/max pairs
//! 24      …     pairs[count], each 2 bytes: min (i8), max (i8)
//! ```
//!
//! Peaks are in `[-127, 127]` (full-scale ±1.0 quantized). The last bucket may
//! summarize fewer than `samples_per_bucket` samples (the stream tail).

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use ffmpeg::format::sample::Type as SampleType;
use ffmpeg::format::Sample;
use ffmpeg::util::frame::audio::Audio as AudioFrame;
use ffmpeg::ChannelLayout;
use ffmpeg_next as ffmpeg;

use crate::{ensure_ffmpeg, MediaError, Result};

const MAGIC: &[u8; 4] = b"DVWF";
const VERSION: u16 = 1;
const OUT_RATE: u32 = 48_000;
const SAMPLES_PER_BUCKET: u32 = 256;

/// Summary of a generated waveform (also the `waveform.pk` header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaveformMeta {
    pub sample_rate: u32,
    pub samples_per_bucket: u32,
    /// Number of min/max pairs.
    pub count: usize,
}

/// One decoded waveform: its header plus the min/max pairs.
#[derive(Debug, Clone, PartialEq)]
pub struct Waveform {
    pub meta: WaveformMeta,
    /// `(min, max)` per bucket, each in `[-127, 127]`.
    pub peaks: Vec<(i8, i8)>,
}

/// Decode `path`'s audio to mono f32 48 kHz and write `out_file` (`waveform.pk`).
/// Errors with [`MediaError::NoAudio`] if the file has no audio stream.
pub fn generate_waveform(path: &Path, out_file: &Path) -> Result<WaveformMeta> {
    ensure_ffmpeg();
    let mut ictx = ffmpeg::format::input(&path)?;
    let stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or_else(|| MediaError::NoAudio(path.display().to_string()))?;
    let aindex = stream.index();

    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
    let mut decoder = ctx.decoder().audio()?;

    // The resampler is built lazily from the first *decoded* frame: some
    // decoders (e.g. PCM WAV) don't report a concrete channel layout until
    // they've produced a frame, and configuring swr from stale parameters makes
    // it error ("Input changed") once real frames arrive.
    let mut resampler: Option<ffmpeg::software::resampling::Context> = None;

    let mut peaks: Vec<(i8, i8)> = Vec::new();
    // Rolling min/max over the current bucket.
    let mut in_bucket = 0u32;
    let mut cur_min = f32::MAX;
    let mut cur_max = f32::MIN;

    let fold = |samples: &[f32],
                peaks: &mut Vec<(i8, i8)>,
                in_bucket: &mut u32,
                cur_min: &mut f32,
                cur_max: &mut f32| {
        for &s in samples {
            if s < *cur_min {
                *cur_min = s;
            }
            if s > *cur_max {
                *cur_max = s;
            }
            *in_bucket += 1;
            if *in_bucket >= SAMPLES_PER_BUCKET {
                peaks.push((quantize(*cur_min), quantize(*cur_max)));
                *in_bucket = 0;
                *cur_min = f32::MAX;
                *cur_max = f32::MIN;
            }
        }
    };

    // Reuse ONE output frame across every resample call — swr caches the
    // output frame's parameters and errors ("Output changed") if handed a
    // fresh frame each time.
    let mut out = AudioFrame::empty();
    let mut decoded = AudioFrame::empty();

    // Some decoders (notably PCM/WAV) leave the decoded frame's channel layout
    // *unspecified*; swr then rejects it with "Input changed". Stamp a concrete
    // default layout for the channel count, and build the resampler from that
    // same frame so its input config matches exactly.
    let convert = |decoded: &mut AudioFrame,
                   resampler: &mut Option<ffmpeg::software::resampling::Context>,
                   out: &mut AudioFrame,
                   peaks: &mut Vec<(i8, i8)>,
                   in_bucket: &mut u32,
                   cur_min: &mut f32,
                   cur_max: &mut f32|
     -> Result<()> {
        decoded.set_channel_layout(ChannelLayout::default(decoded.channels() as i32));
        if resampler.is_none() {
            *resampler = Some(decoded.resampler(
                Sample::F32(SampleType::Packed),
                ChannelLayout::MONO,
                OUT_RATE,
            )?);
        }
        let r = resampler.as_mut().expect("resampler set");
        r.run(decoded, out)?;
        fold(mono_samples(out), peaks, in_bucket, cur_min, cur_max);
        Ok(())
    };

    for (s, packet) in ictx.packets() {
        if s.index() != aindex {
            continue;
        }
        decoder.send_packet(&packet)?;
        while decoder.receive_frame(&mut decoded).is_ok() {
            convert(
                &mut decoded,
                &mut resampler,
                &mut out,
                &mut peaks,
                &mut in_bucket,
                &mut cur_min,
                &mut cur_max,
            )?;
        }
    }
    decoder.send_eof()?;
    while decoder.receive_frame(&mut decoded).is_ok() {
        convert(
            &mut decoded,
            &mut resampler,
            &mut out,
            &mut peaks,
            &mut in_bucket,
            &mut cur_min,
            &mut cur_max,
        )?;
    }
    // Drain the resampler's internal buffer (upsampling leaves a tail).
    if let Some(r) = resampler.as_mut() {
        while r.delay().is_some() {
            r.flush(&mut out)?;
            if out.samples() == 0 {
                break;
            }
            fold(
                mono_samples(&out),
                &mut peaks,
                &mut in_bucket,
                &mut cur_min,
                &mut cur_max,
            );
        }
    }
    // Flush a final partial bucket, if any samples remain.
    if in_bucket > 0 {
        peaks.push((quantize(cur_min), quantize(cur_max)));
    }

    let meta = WaveformMeta {
        sample_rate: OUT_RATE,
        samples_per_bucket: SAMPLES_PER_BUCKET,
        count: peaks.len(),
    };
    write_pk(out_file, meta, &peaks)?;
    Ok(meta)
}

impl Waveform {
    /// Load a `waveform.pk` written by [`generate_waveform`].
    pub fn load(path: &Path) -> Result<Waveform> {
        let mut r = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(bad("waveform.pk", "wrong magic bytes"));
        }
        let version = read_u16(&mut r)?;
        if version != VERSION {
            return Err(bad(
                "waveform.pk",
                &format!("unsupported version {version}"),
            ));
        }
        let _reserved = read_u16(&mut r)?;
        let sample_rate = read_u32(&mut r)?;
        let samples_per_bucket = read_u32(&mut r)?;
        let count = read_u64(&mut r)? as usize;
        let mut peaks = Vec::with_capacity(count);
        for _ in 0..count {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            peaks.push((b[0] as i8, b[1] as i8));
        }
        Ok(Waveform {
            meta: WaveformMeta {
                sample_rate,
                samples_per_bucket,
                count,
            },
            peaks,
        })
    }
}

/// Packed mono f32 frame → its sample slice (plane 0 holds all samples).
fn mono_samples(frame: &AudioFrame) -> &[f32] {
    if frame.samples() == 0 {
        return &[];
    }
    // Packed layout: plane(0) is the full interleaved buffer; mono => 1 ch.
    &frame.plane::<f32>(0)[..frame.samples()]
}

fn quantize(v: f32) -> i8 {
    (v.clamp(-1.0, 1.0) * 127.0).round() as i8
}

fn write_pk(path: &Path, meta: WaveformMeta, peaks: &[(i8, i8)]) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(MAGIC)?;
    w.write_all(&VERSION.to_le_bytes())?;
    w.write_all(&0u16.to_le_bytes())?;
    w.write_all(&meta.sample_rate.to_le_bytes())?;
    w.write_all(&meta.samples_per_bucket.to_le_bytes())?;
    w.write_all(&(peaks.len() as u64).to_le_bytes())?;
    for &(mn, mx) in peaks {
        w.write_all(&[mn as u8, mx as u8])?;
    }
    w.flush()?;
    Ok(())
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
fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64<R: Read>(r: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_clamps_full_scale() {
        assert_eq!(quantize(0.0), 0);
        assert_eq!(quantize(1.0), 127);
        assert_eq!(quantize(-1.0), -127);
        assert_eq!(quantize(2.0), 127); // clamped
        assert_eq!(quantize(-9.0), -127);
    }

    #[test]
    fn pk_roundtrip_is_equal() {
        let peaks = vec![(-127i8, 127i8), (0, 10), (-5, 5), (-1, 1)];
        let meta = WaveformMeta {
            sample_rate: OUT_RATE,
            samples_per_bucket: SAMPLES_PER_BUCKET,
            count: peaks.len(),
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("waveform.pk");
        write_pk(&path, meta, &peaks).expect("write");
        let wf = Waveform::load(&path).expect("load");
        assert_eq!(wf.meta, meta);
        assert_eq!(wf.peaks, peaks);
    }

    #[test]
    fn load_rejects_bad_magic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.pk");
        std::fs::write(&path, b"XXXXxxxx").expect("write");
        assert!(matches!(
            Waveform::load(&path),
            Err(MediaError::Format {
                kind: "waveform.pk",
                ..
            })
        ));
    }
}
