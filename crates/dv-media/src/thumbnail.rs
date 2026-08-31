//! Thumbnail strip — scrubbing layer 3 (§4.3).
//!
//! Decodes the video and emits one JPEG per ~1 s of source, scaled to 160 px
//! wide (aspect preserved) via swscale (RGB24), written as `NNNN.jpg` where
//! `NNNN` is the zero-padded second index. These feed the timeline filmstrip,
//! the vertical list's clip thumbnails, and the instant scrub placeholder.
//!
//! **Perf note (M1 correctness over speed):** this is a straight decode loop
//! that decodes every frame and keeps only one per interval. That is fine for
//! short clips and simplest to get correct. For long files M2+ should seek to
//! each keyframe (via [`crate::KeyframeIndex`]) and decode just the few frames
//! around each sample point instead of the whole stream.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use ffmpeg::software::scaling::{Context as Scaler, Flags as ScaleFlags};
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_next as ffmpeg;

use crate::{ensure_ffmpeg, MediaError, Result};

/// Thumbnail generation options (§4.3 defaults).
#[derive(Debug, Clone, Copy)]
pub struct ThumbnailOpts {
    /// Output width in pixels; height derives from source aspect. Default 160.
    pub width: u32,
    /// Seconds of source between thumbnails. Default 1.0.
    pub interval_secs: f64,
    /// JPEG quality 1–100. Default 80.
    pub jpeg_quality: u8,
}

impl Default for ThumbnailOpts {
    fn default() -> Self {
        ThumbnailOpts {
            width: 160,
            interval_secs: 1.0,
            jpeg_quality: 80,
        }
    }
}

/// Generate thumbnails for `path` into `out_dir` (created if missing). Returns
/// the number of JPEGs written. Errors with [`MediaError::NoVideo`] if the file
/// has no video stream (image media should never reach here — §4.3).
pub fn generate_thumbnails(path: &Path, out_dir: &Path, opts: ThumbnailOpts) -> Result<usize> {
    ensure_ffmpeg();
    fs::create_dir_all(out_dir)?;

    let mut ictx = ffmpeg::format::input(&path)?;
    let stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| MediaError::NoVideo(path.display().to_string()))?;
    let vindex = stream.index();
    let tb = stream.time_base();
    let tb_secs = tb.numerator() as f64 / tb.denominator().max(1) as f64;

    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
    let mut decoder = ctx.decoder().video()?;

    let interval = opts.interval_secs.max(0.001);
    let mut scaler: Option<Scaler> = None;
    let mut out_h = 0u32;
    let mut last_bucket: i64 = -1;
    let mut frame_ord: i64 = 0;
    let mut written = 0usize;

    let emit = |decoder: &ffmpeg::decoder::Video,
                frame: &VideoFrame,
                scaler: &mut Option<Scaler>,
                out_h: &mut u32,
                last_bucket: &mut i64,
                frame_ord: &mut i64,
                written: &mut usize|
     -> Result<()> {
        // Presentation time in seconds: use the frame pts, else count frames.
        let secs = match frame.pts() {
            Some(pts) => pts as f64 * tb_secs,
            None => {
                let fps = decoder
                    .frame_rate()
                    .map(|r| r.numerator() as f64 / r.denominator().max(1) as f64)
                    .filter(|f| *f > 0.0)
                    .unwrap_or(30.0);
                *frame_ord as f64 / fps
            }
        };
        *frame_ord += 1;
        let bucket = (secs / interval).floor() as i64;
        if bucket <= *last_bucket {
            return Ok(());
        }
        *last_bucket = bucket;

        // Lazily build the scaler once the first frame's real format is known.
        let scaler = match scaler {
            Some(s) => s,
            None => {
                let src_w = frame.width().max(1);
                let src_h = frame.height().max(1);
                let h = ((opts.width as u64 * src_h as u64) / src_w as u64).max(1) as u32;
                *out_h = h;
                *scaler = Some(Scaler::get(
                    frame.format(),
                    src_w,
                    src_h,
                    ffmpeg::format::Pixel::RGB24,
                    opts.width,
                    h,
                    ScaleFlags::BILINEAR,
                )?);
                scaler.as_mut().expect("scaler just set")
            }
        };

        let mut rgb = VideoFrame::empty();
        scaler.run(frame, &mut rgb)?;
        let out = out_dir.join(format!("{bucket:04}.jpg"));
        write_jpeg(&rgb, opts.width, *out_h, opts.jpeg_quality, &out)?;
        *written += 1;
        Ok(())
    };

    let mut decoded = VideoFrame::empty();
    for (s, packet) in ictx.packets() {
        if s.index() != vindex {
            continue;
        }
        decoder.send_packet(&packet)?;
        while decoder.receive_frame(&mut decoded).is_ok() {
            emit(
                &decoder,
                &decoded,
                &mut scaler,
                &mut out_h,
                &mut last_bucket,
                &mut frame_ord,
                &mut written,
            )?;
        }
    }
    decoder.send_eof()?;
    while decoder.receive_frame(&mut decoded).is_ok() {
        emit(
            &decoder,
            &decoded,
            &mut scaler,
            &mut out_h,
            &mut last_bucket,
            &mut frame_ord,
            &mut written,
        )?;
    }

    Ok(written)
}

/// Encode an RGB24 frame to JPEG, copying row-by-row to drop swscale's stride
/// padding (the `image` crate needs tightly-packed rows).
fn write_jpeg(rgb: &VideoFrame, w: u32, h: u32, quality: u8, out: &Path) -> Result<()> {
    let stride = rgb.stride(0);
    let src = rgb.data(0);
    let row_bytes = (w * 3) as usize;
    let mut packed = Vec::with_capacity(row_bytes * h as usize);
    for y in 0..h as usize {
        let start = y * stride;
        packed.extend_from_slice(&src[start..start + row_bytes]);
    }

    let mut file = BufWriter::new(File::create(out)?);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, quality);
    enc.encode(&packed, w, h, image::ExtendedColorType::Rgb8)?;
    Ok(())
}
