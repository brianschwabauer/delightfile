//! Media classification (§9). Decides `MediaKind`, pulls dimensions / codecs /
//! duration / fps / sample rate. Still images (PNG/JPG/WebP) are validated with
//! the `image` crate (dimensions only, no full decode); everything else is
//! opened with ffmpeg and must have a decodable video *or* audio stream.

use std::path::Path;

use dv_core::model::MediaKind;
use ffmpeg_next as ffmpeg;

use crate::{ensure_ffmpeg, MediaError, Result};

/// Everything `dv-app` needs to insert a `media` row (§8.1) after import.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeInfo {
    pub kind: MediaKind,
    /// `None` for images (infinite source range, §1).
    pub duration_us: Option<i64>,
    /// e.g. `"h264"`; `None` when there is no (decodable) video stream.
    pub video_codec: Option<String>,
    /// e.g. `"aac"`; `None` when there is no (decodable) audio stream.
    pub audio_codec: Option<String>,
    /// Coded frame size — the pixels the decoder hands out, *before* any
    /// [`rotation`](Self::rotation) the container asks the player to apply.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The turn the container asks for, from the video stream's display
    /// matrix (the `rotate` a phone writes into a portrait clip): degrees
    /// **clockwise** the decoded frame must be turned to sit upright — one of
    /// 0, 90, 180, 270. Exactly what ffmpeg's own autorotate would do.
    pub rotation: u32,
    /// The display matrix also mirrors, on the odd file. `true` when it does:
    /// the frame is flipped left-to-right *before* the rotation.
    pub mirrored: bool,
    /// `avg_frame_rate` numerator/denominator (§8.1). `None` for audio/images.
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    /// Audio sample rate in Hz, when an audio stream exists.
    pub sample_rate: Option<u32>,
    pub has_audio: bool,
    /// Container chapters in source order, empty when the file has none.
    pub chapters: Vec<Chapter>,
}

/// One container chapter, in microseconds off the start of the file. The title
/// is whatever the container's `title` metadata said, or empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub start_us: i64,
    pub end_us: i64,
    pub title: String,
}

/// Classify `path`. Returns [`MediaError::Unsupported`] when the file is
/// neither a still image nor a container with a decodable video/audio stream.
pub fn probe(path: &Path) -> Result<ProbeInfo> {
    // A stream URL is not a file the image crate can open; go straight to the
    // demuxer, which speaks RTSP itself.
    if !crate::is_stream_url(path) {
        if let Some(info) = probe_image(path)? {
            return Ok(info);
        }
    }
    ensure_ffmpeg();
    probe_av(path)
}

/// Try to read `path` as a still image. `Ok(None)` means "not one of our image
/// formats" (fall through to ffmpeg); `Ok(Some)` is a valid image.
fn probe_image(path: &Path) -> Result<Option<ProbeInfo>> {
    use image::ImageFormat;

    // Guess from content, not just the extension — a mislabeled `.png` that is
    // really JPEG still classifies correctly.
    let reader = match image::ImageReader::open(path) {
        Ok(r) => r,
        Err(_) => return Ok(None), // unreadable as a plain file: let ffmpeg try
    };
    let reader = match reader.with_guessed_format() {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    match reader.format() {
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP) => {}
        _ => return Ok(None),
    }
    // Dimensions only — avoids decoding the full pixel buffer (§9).
    let (w, h) = image::image_dimensions(path)?;
    Ok(Some(ProbeInfo {
        kind: MediaKind::Image,
        duration_us: None,
        video_codec: None,
        audio_codec: None,
        width: Some(w),
        height: Some(h),
        rotation: 0,
        mirrored: false,
        fps_num: None,
        fps_den: None,
        sample_rate: None,
        has_audio: false,
        chapters: Vec::new(),
    }))
}

fn probe_av(path: &Path) -> Result<ProbeInfo> {
    // A container we can't even demux is a clean reject (§9), not a raw ffmpeg
    // error — e.g. a text file or an unknown format.
    let ictx = crate::open_input(path).map_err(|e| {
        MediaError::Unsupported(format!(
            "{}: not a recognized media file ({e})",
            path.display()
        ))
    })?;

    // Best streams of each medium, if any.
    let video = ictx.streams().best(ffmpeg::media::Type::Video);
    let audio = ictx.streams().best(ffmpeg::media::Type::Audio);

    // Resolve a decodable video stream → (codec name, w, h, fps).
    let mut video_codec = None;
    let mut width = None;
    let mut height = None;
    let mut fps_num = None;
    let mut fps_den = None;
    let mut rotation = 0;
    let mut mirrored = false;
    if let Some(stream) = video {
        (rotation, mirrored) = display_orientation(&stream);
        let params = stream.parameters();
        if let Ok(ctx) = ffmpeg::codec::context::Context::from_parameters(params) {
            if let Ok(decoder) = ctx.decoder().video() {
                video_codec =
                    ffmpeg::codec::decoder::find(decoder.id()).map(|c| c.name().to_string());
                width = Some(decoder.width());
                height = Some(decoder.height());
                let r = stream.avg_frame_rate();
                if r.numerator() > 0 && r.denominator() > 0 {
                    fps_num = Some(r.numerator() as u32);
                    fps_den = Some(r.denominator() as u32);
                }
            }
        }
    }

    // Resolve a decodable audio stream → (codec name, sample rate).
    let mut audio_codec = None;
    let mut sample_rate = None;
    if let Some(stream) = audio {
        let params = stream.parameters();
        if let Ok(ctx) = ffmpeg::codec::context::Context::from_parameters(params) {
            if let Ok(decoder) = ctx.decoder().audio() {
                audio_codec =
                    ffmpeg::codec::decoder::find(decoder.id()).map(|c| c.name().to_string());
                sample_rate = Some(decoder.rate());
            }
        }
    }

    let has_video = video_codec.is_some();
    let has_audio = audio_codec.is_some();
    if !has_video && !has_audio {
        return Err(MediaError::Unsupported(format!(
            "{}: no decodable video or audio stream",
            path.display()
        )));
    }

    let duration_us = duration_us(&ictx);
    let chapters = chapters(&ictx, duration_us);

    Ok(ProbeInfo {
        kind: if has_video {
            MediaKind::Video
        } else {
            MediaKind::Audio
        },
        duration_us,
        video_codec,
        audio_codec,
        width,
        height,
        rotation,
        mirrored,
        fps_num,
        fps_den,
        sample_rate,
        has_audio,
        chapters,
    })
}

/// The turn a stream's display matrix asks for: `(degrees clockwise, mirrored)`.
/// `(0, false)` when the stream carries no matrix, or a degenerate one.
fn display_orientation(stream: &ffmpeg::format::stream::Stream) -> (u32, bool) {
    stream
        .side_data()
        .find(|sd| sd.kind() == ffmpeg::codec::packet::side_data::Type::DisplayMatrix)
        .map(|sd| display_matrix_orientation(sd.data()))
        .unwrap_or((0, false))
}

/// Read a 3×3 display matrix (nine little-endian `i32`s, 16.16 fixed point in
/// the rotation/scale quadrant) the way ffmpeg's autorotate does: the angle
/// `av_display_rotation_get` measures is counter-clockwise, the player's turn
/// is its negation, and the two minus signs cancel into the plain `atan2`
/// below. Snapped to the nearest quarter turn, which is all a container ever
/// writes. A negative determinant is a mirror.
fn display_matrix_orientation(data: &[u8]) -> (u32, bool) {
    if data.len() < 36 {
        return (0, false);
    }
    let m = |i: usize| {
        let mut b = [0u8; 4];
        b.copy_from_slice(&data[i * 4..i * 4 + 4]);
        i32::from_le_bytes(b) as f64
    };
    let (a, b, c, d) = (m(0), m(1), m(3), m(4));
    let scale_x = a.hypot(c);
    let scale_y = b.hypot(d);
    if scale_x == 0.0 || scale_y == 0.0 {
        return (0, false);
    }
    let mirrored = a * d - b * c < 0.0;
    // Undo the mirror before measuring the angle, so a flipped-and-turned
    // matrix reads as the turn it is rather than its reflection.
    let (a, b) = if mirrored { (-a, -b) } else { (a, b) };
    let theta = (b / scale_y).atan2(a / scale_x).to_degrees();
    let quarter = ((theta / 90.0).round() as i64).rem_euclid(4) as u32;
    (quarter * 90, mirrored)
}

/// Chapter start/end in a chapter's own time base, converted to microseconds.
/// Saturates rather than wrapping — a bogus `start` on a broken file must not
/// come back as a negative time.
fn chapter_us(ts: i64, time_base: (i32, i32)) -> i64 {
    let (num, den) = time_base;
    if num <= 0 || den <= 0 {
        return 0;
    }
    ((ts as i128 * num as i128 * 1_000_000) / den as i128).clamp(i64::MIN as i128, i64::MAX as i128)
        as i64
}

/// The container's chapters, sorted by start. Conservative: only a chapter
/// that starts before the file does, or past its end, is dropped — a zero- or
/// negative-length one is kept, because the only thing this list is used for
/// is "where do the chapters begin".
fn chapters(ictx: &ffmpeg::format::context::Input, duration_us: Option<i64>) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = ictx
        .chapters()
        .filter_map(|c| {
            let tb = c.time_base();
            let tb = (tb.numerator(), tb.denominator());
            let start_us = chapter_us(c.start(), tb);
            let end_us = chapter_us(c.end(), tb);
            if start_us < 0 {
                return None;
            }
            if let Some(d) = duration_us {
                if start_us > d {
                    return None;
                }
            }
            Some(Chapter {
                start_us,
                end_us,
                title: c.metadata().get("title").unwrap_or_default().to_string(),
            })
        })
        .collect();
    out.sort_by_key(|c| c.start_us);
    out
}

/// Container duration in microseconds. `AVFormatContext::duration` is already
/// in `AV_TIME_BASE` (= 1 µs) units; fall back to `None` if unknown (< 0).
fn duration_us(ictx: &ffmpeg::format::context::Input) -> Option<i64> {
    let d = ictx.duration();
    if d > 0 {
        // AV_TIME_BASE is 1_000_000, so the value is already microseconds.
        Some(d)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A display matrix as `av_display_rotation_set` writes it, for a turn of
    /// `degrees` counter-clockwise (ffmpeg's sign convention).
    fn matrix(degrees: f64, mirrored: bool) -> Vec<u8> {
        let r = degrees.to_radians();
        let (s, c) = (r.sin(), r.cos());
        let fixed = |v: f64| ((v * 65536.0).round() as i32).to_le_bytes();
        let flip = if mirrored { -1.0 } else { 1.0 };
        [c * flip, -s * flip, 0.0, s, c, 0.0, 0.0, 0.0, 1.0]
            .into_iter()
            .flat_map(fixed)
            .collect()
    }

    /// The phone's portrait clip: `rotate=-90` in ffprobe's words is a
    /// quarter turn clockwise on the way to the screen.
    #[test]
    fn the_display_matrix_reads_as_a_clockwise_turn() {
        assert_eq!(display_matrix_orientation(&matrix(0.0, false)), (0, false));
        assert_eq!(display_matrix_orientation(&matrix(-90.0, false)), (90, false));
        assert_eq!(display_matrix_orientation(&matrix(90.0, false)), (270, false));
        assert_eq!(display_matrix_orientation(&matrix(180.0, false)), (180, false));
        assert_eq!(display_matrix_orientation(&matrix(-270.0, false)), (270, false));
        assert_eq!(display_matrix_orientation(&matrix(-90.0, true)), (90, true));
        // Junk is upright, not a panic.
        assert_eq!(display_matrix_orientation(&[0u8; 36]), (0, false));
        assert_eq!(display_matrix_orientation(&[1u8; 7]), (0, false));
    }

    #[test]
    fn chapter_times_convert_through_their_own_time_base() {
        // The common matroska chapter base, 1/1_000_000_000.
        assert_eq!(chapter_us(0, (1, 1_000_000_000)), 0);
        assert_eq!(chapter_us(2_500_000_000, (1, 1_000_000_000)), 2_500_000);
        // A millisecond base, and a non-unit numerator.
        assert_eq!(chapter_us(1_500, (1, 1_000)), 1_500_000);
        assert_eq!(chapter_us(3, (1001, 30_000)), 100_100);
        // A degenerate base is a zero, not a panic.
        assert_eq!(chapter_us(42, (0, 1)), 0);
        assert_eq!(chapter_us(42, (1, 0)), 0);
        // Long files stay exact rather than overflowing an i64 multiply.
        assert_eq!(
            chapter_us(10_000_000_000_000, (1, 1_000_000_000)),
            10_000_000_000
        );
    }
}
