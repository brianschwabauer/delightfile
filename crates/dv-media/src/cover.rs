//! Cover art: the picture a song carries, decoded to RGBA.
//!
//! A tagged MP3/FLAC/M4A stores its artwork as a one-frame video stream with
//! the `ATTACHED_PIC` disposition — the same flag everything else in this crate
//! goes out of its way to *skip* (see [`crate::best_video_stream`]). This is
//! the one place that wants it, so it looks for exactly that stream and
//! nothing else: a real video's first frame is not cover art, and asking for
//! one here would quietly turn a film into an album sleeve.

use std::path::Path;

use ffmpeg_next as ffmpeg;

use crate::{ensure_ffmpeg, Result};

/// A decoded cover picture: tightly packed RGBA8, `width * height * 4` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverArt {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode `path`'s embedded cover art, if it has any.
///
/// `max_side` caps the long edge: art larger than that is scaled down inside
/// the swscale step that was going to run anyway, preserving aspect ratio, so
/// the caller never allocates (or uploads) a 3000×3000 sleeve it is going to
/// draw at 400 points. `None` keeps the source size.
///
/// `Ok(None)` is the ordinary answer for a file with no artwork — a song
/// without a sleeve is not an error.
pub fn cover_art(path: &Path, max_side: Option<u32>) -> Result<Option<CoverArt>> {
    use ffmpeg::software::scaling::{Context as Scaler, Flags as ScaleFlags};
    use ffmpeg::util::frame::video::Video as VideoFrame;

    ensure_ffmpeg();

    let mut ictx = crate::open_input(path)?;
    let Some(stream) = ictx.streams().find(|s| {
        s.parameters().medium() == ffmpeg::media::Type::Video && crate::is_attached_pic(s)
    }) else {
        return Ok(None);
    };
    let pindex = stream.index();

    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
    let mut decoder = ctx.decoder().video()?;

    // ffmpeg hands the attached picture over as a packet at the very start of
    // demuxing, so the first packet on this stream is the whole picture — no
    // seeking, no waiting for a keyframe.
    let mut decoded = VideoFrame::empty();
    let mut got = false;
    for (s, packet) in ictx.packets() {
        if s.index() != pindex {
            continue;
        }
        decoder.send_packet(&packet)?;
        if decoder.receive_frame(&mut decoded).is_ok() {
            got = true;
        }
        // One packet is the entire stream; whether or not it produced a frame
        // yet, there is nothing after it worth demuxing the rest of the song
        // for.
        break;
    }
    if !got {
        // A decoder that buffered the single packet rather than answering it
        // needs to be told the stream is over before it will give the frame up.
        decoder.send_eof()?;
        got = decoder.receive_frame(&mut decoded).is_ok();
    }
    if !got {
        return Ok(None);
    }

    let (src_w, src_h) = (decoded.width().max(1), decoded.height().max(1));
    let (w, h) = fit_within(src_w, src_h, max_side);

    let mut scaler = Scaler::get(
        decoded.format(),
        src_w,
        src_h,
        ffmpeg::format::Pixel::RGBA,
        w,
        h,
        // BILINEAR for a straight convert, BICUBIC when this is also the
        // downscale: a sleeve shrunk 4× with bilinear looks like it was
        // resized by a thumbnailer, which is exactly the tell to avoid.
        if (w, h) == (src_w, src_h) {
            ScaleFlags::BILINEAR
        } else {
            ScaleFlags::BICUBIC
        },
    )?;
    let mut rgba = VideoFrame::empty();
    scaler.run(&decoded, &mut rgba)?;

    // swscale pads rows out to its own alignment; hand back a tightly packed
    // buffer, because every consumer of this assumes `width * 4` per row.
    let stride = rgba.stride(0);
    let row = w as usize * 4;
    let plane = rgba.data(0);
    let mut out = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        let start = y * stride;
        match plane.get(start..start + row) {
            Some(s) => out.extend_from_slice(s),
            // A plane shorter than its own stride claims to be is not worth a
            // panic — the rest goes out transparent, same answer `still.rs`
            // gives to the same question.
            None => {
                out.resize(row * h as usize, 0);
                break;
            }
        }
    }

    Ok(Some(CoverArt {
        width: w,
        height: h,
        rgba: out,
    }))
}

/// `(w, h)` shrunk so neither side exceeds `max_side`, aspect ratio kept. Never
/// enlarges: a 200 px sleeve stays 200 px rather than being blown up to the cap.
fn fit_within(w: u32, h: u32, max_side: Option<u32>) -> (u32, u32) {
    let Some(max) = max_side.filter(|m| *m > 0) else {
        return (w, h);
    };
    let long = w.max(h);
    if long <= max {
        return (w, h);
    }
    let scale = max as f64 / long as f64;
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_shrinks_the_long_side_and_leaves_small_art_alone() {
        assert_eq!(fit_within(3000, 3000, Some(2048)), (2048, 2048));
        assert_eq!(fit_within(4000, 2000, Some(2048)), (2048, 1024));
        assert_eq!(fit_within(2000, 4000, Some(2048)), (1024, 2048));
        // Already inside the cap, or no cap at all: untouched.
        assert_eq!(fit_within(600, 600, Some(2048)), (600, 600));
        assert_eq!(fit_within(3000, 3000, None), (3000, 3000));
        // A degenerate cap is "no cap", and nothing ever rounds to zero.
        assert_eq!(fit_within(3000, 3000, Some(0)), (3000, 3000));
        assert_eq!(fit_within(4000, 1, Some(2)), (2, 1));
    }
}
