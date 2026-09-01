//! The decode side of PLAN §6's seam: pixels, off the paint thread.
//!
//! df-core answers [`df_core::preview::Preview::NeedsDecode`] for anything it
//! cannot render from bytes it already read — images, video, audio, PDFs,
//! fonts, models — and this is what takes that over. Phase 3 implements the
//! **image** half; the other kinds arrive here only for their cached
//! thumbnail, which is what lets a video row show its frame before dv-playback
//! exists (PLAN §10's next checkbox).
//!
//! ## Why a thread of its own
//!
//! A 6000×4000 JPEG is 40 ms of work on a good machine and an AVIF can be
//! several times that. Doing it in `frame()` would drop frames on every cursor
//! move; doing it in df-core would put ffmpeg in the crate that is supposed to
//! be testable without it (PLAN §1). So it is one worker thread, woken by a
//! channel, ringing the same [`crate::Wake`] bell every other worker rings.
//!
//! ## Newest wins, again
//!
//! The same rule as [`df_core::preview::Previewer`], for the same reason: a
//! held `↓` asks for forty decodes and wants one. A single `AtomicU64` holds
//! the live token; the worker checks it before opening the file, after the
//! placeholder, and again before sending the full frame.
//!
//! ## Two decoders, in order
//!
//! 1. The `image` crate, for the formats the workspace builds it with — PNG,
//!    JPEG, WebP, GIF. No process-wide state, no system libraries.
//! 2. **ffmpeg**, through `dv_media::ffmpeg` (the re-export exists so nothing
//!    outside dv-media depends on `ffmpeg-next` directly). This is what makes
//!    **AVIF and HEIF/HEIC** work — PLAN §6's "replaces the hand-written avif
//!    plugin" — via dav1d and the HEVC decoder, with no libavif or libheif of
//!    our own. It also picks up BMP, TIFF, ICO, JPEG-XL and the raw formats
//!    for free, wherever the system ffmpeg has a decoder.
//!
//! Verified by hand on this machine (ffmpeg 9, `libdav1d` and `hevc` both
//! present): a 320×200 AVIF written by `ffmpeg -c:v libaom-av1` comes back
//! through [`decode_file`] as 200×125 RGBA for a 200×200 pane — the ffmpeg
//! branch, the swscale downscale and the row packing, all three. There is no
//! hermetic fixture for it because the fixture would be a system ffmpeg; the
//! `image`-crate branch has one, in this module's tests.
//!
//! **SVG is the known gap.** df-core routes `image/svg+xml` to the image
//! previewer (a picture is what the user means by it) but neither decoder
//! rasterises vectors; delightviewer does it with `resvg`, which is a real
//! dependency and therefore a decision for its own commit. Until then an SVG
//! reports "no decoder", which is honest and is not a crash.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;
use df_core::preview::{store_thumb, PreviewToken, STILL_SKIP};

/// The largest file the decoders are allowed to open, in bytes: 512 MiB.
///
/// Not a quality limit — a decoded 512 MiB source is already gigabytes of RGBA
/// — but a bound on the damage a cursor parked on a disk image can do. Past
/// this the pane says the file is too large, which is a truthful preview.
const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;

/// The longest side of the thumbnail written back to the shared cache.
///
/// yazi's own `preview.max_width`/`max_height` default to 600×900, and the
/// entries observed in `/tmp/yazi-1000` sit around that. Matching it means the
/// thumbnail delightfile writes is the size delightviewer and yazi expect to
/// find, and it is plenty for the placeholder it exists to be.
const THUMB_MAX_SIDE: u32 = 900;

/// JPEG quality for that write-back, 0–100.
///
/// 80 is dv-media's own thumbnail setting (`ThumbnailOpts::default`), and the
/// number where a downscaled photo stops having visible ringing. One shared
/// cache should not have two programs writing it at two qualities.
const THUMB_QUALITY: u8 = 80;

/// A decoded picture, ready to become a texture.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    /// Tightly packed, `width * height * 4` bytes, non-premultiplied.
    pub pixels: Vec<u8>,
}

/// Which half of a decode this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The yazi cache's thumbnail — the crossfade's placeholder.
    Thumb,
    /// The real pixels.
    Full,
}

/// One finished decode.
pub struct Decoded {
    pub token: PreviewToken,
    pub stage: Stage,
    /// **Already an [`egui::ColorImage`]**, built on this worker.
    ///
    /// `ColorImage::from_rgba_unmultiplied` is a full second copy of the
    /// picture — twenty-odd milliseconds on a pane-sized photo — and doing it
    /// on the UI thread is doing it in the middle of an animation frame, which
    /// is precisely the jank this seam exists to prevent. The UI thread's
    /// share is now the `load_texture` upload and nothing else.
    ///
    /// `Err` carries something the pane can print. A failed *thumb* is not
    /// worth showing (the full decode is still coming), a failed *full* is.
    pub result: Result<egui::ColorImage, String>,
}

/// What the pane asks for.
pub struct Job {
    pub token: PreviewToken,
    pub path: PathBuf,
    /// The pane's size in physical pixels. The decode is scaled to it, so a
    /// 50-megapixel photo becomes a 400×600 texture and never a 200 MB one.
    pub target: (u32, u32),
    /// The cached thumbnail df-core found, if any.
    pub thumb: Option<PathBuf>,
    /// Whether to decode the file itself. False for the kinds Phase 3 does not
    /// decode yet — video, audio, PDF, fonts, models — which get their cached
    /// thumbnail and a badge (PLAN §10's next checkbox picks them up).
    pub full: bool,
    /// Whether a successful full decode should be written back to the shared
    /// cache. Only when there was no thumbnail to begin with, and only for the
    /// kinds yazi itself thumbnails.
    pub store: bool,
}

/// The decode worker.
///
/// Dropping it closes the channel; the worker finishes the job it is on and
/// exits, and the drop joins it so nothing outlives the window.
pub struct Decoder {
    jobs: Option<Sender<Job>>,
    results: Receiver<Decoded>,
    live: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Decoder {
    /// Start the worker. `notify` is rung once per result.
    pub fn start(notify: Notifier) -> Decoder {
        let (job_tx, job_rx) = unbounded::<Job>();
        let (res_tx, res_rx) = unbounded::<Decoded>();
        let live = Arc::new(AtomicU64::new(0));
        let worker_live = Arc::clone(&live);
        let handle = std::thread::Builder::new()
            .name("df-decode".to_string())
            .spawn(move || {
                // A held `↓` puts this thread on every core it can reach; the
                // paint thread has to win that race (`df_core::thread`).
                df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                for job in job_rx {
                    run(job, &res_tx, &worker_live, &notify);
                }
            });
        let worker = match handle {
            Ok(h) => Some(h),
            // A thread that will not spawn costs the image previews and
            // nothing else, exactly as a preview worker that will not spawn
            // costs the text ones.
            Err(e) => {
                log::warn!("the decode worker did not start: {e}");
                None
            }
        };
        Decoder {
            jobs: Some(job_tx),
            results: res_rx,
            live,
            worker,
        }
    }

    /// Queue a decode, retiring whatever was in flight.
    pub fn request(&self, job: Job) {
        self.live.store(job.token.0, Ordering::Relaxed);
        if let Some(jobs) = &self.jobs {
            if jobs.send(job).is_err() {
                log::debug!("the decode worker is gone");
            }
        }
    }

    /// Stop caring about whatever is in flight.
    pub fn cancel(&self) {
        self.live.store(0, Ordering::Relaxed);
    }

    pub fn drain(&self) -> Vec<Decoded> {
        self.results.try_iter().collect()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.cancel();
        self.jobs = None;
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

fn live(state: &AtomicU64, token: PreviewToken) -> bool {
    state.load(Ordering::Relaxed) == token.0
}

fn run(job: Job, out: &Sender<Decoded>, state: &AtomicU64, notify: &Notifier) {
    let Job {
        token,
        path,
        target,
        thumb,
        full,
        store,
    } = job;
    if !live(state, token) {
        return;
    }

    let send = |stage, result| {
        if live(state, token)
            && out
                .send(Decoded {
                    token,
                    stage,
                    result,
                })
                .is_ok()
        {
            notify();
            true
        } else {
            false
        }
    };

    // The placeholder first, always: it is a 600-pixel JPEG and it is on
    // screen before the real decode has finished opening its file.
    if let Some(thumb) = &thumb {
        match decode_file(thumb, target).and_then(|image| to_color(&image)) {
            Ok(image) => {
                if !send(Stage::Thumb, Ok(image)) {
                    return;
                }
            }
            // A cache entry that will not decode is a miss, not an error: the
            // real decode is still on its way and is the answer either way.
            Err(e) => log::debug!("cached thumbnail {} is unreadable: {e}", thumb.display()),
        }
    }

    if !full || !live(state, token) {
        return;
    }

    match decode_file(&path, target) {
        Ok(image) => {
            let store_from = store.then(|| (image.width, image.height, image.pixels.clone()));
            let color = match to_color(&image) {
                Ok(color) => color,
                Err(e) => {
                    send(Stage::Full, Err(e));
                    return;
                }
            };
            if !send(Stage::Full, Ok(color)) {
                return;
            }
            // After the pane has its pixels, never before: the write-back is a
            // favour to the *next* program to look at this file, and it must
            // not delay the preview it is a by-product of.
            if let Some((w, h, pixels)) = store_from {
                write_thumb(&path, w, h, &pixels);
            }
        }
        Err(e) => {
            log::debug!("{}: {e}", path.display());
            send(Stage::Full, Err(e));
        }
    }
}

/// Pack decoded pixels into the shape egui uploads from, **on this thread**.
///
/// The size sanity-check that used to live next to `load_texture` comes with
/// it: a buffer shorter than its own dimensions claim is a decoder bug, and
/// the worker is where a decoder bug should be turned into a message.
pub fn to_color(image: &Rgba) -> Result<egui::ColorImage, String> {
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 || image.pixels.len() < w * h * 4 {
        return Err("decoded nothing".to_string());
    }
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [w, h],
        &image.pixels[..w * h * 4],
    ))
}

/// Decode `path`, scaled to fit `target` physical pixels.
///
/// The scale is applied *during* the decode where it can be (ffmpeg's swscale
/// takes an output size) and immediately after where it cannot, so the only
/// full-resolution buffer that ever exists is the one the decoder had to
/// produce anyway.
pub(crate) fn decode_file(path: &Path, target: (u32, u32)) -> Result<Rgba, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "{} is too large to preview",
            crate::format::human_size(meta.len())
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    match image::load_from_memory(&bytes) {
        Ok(image) => Ok(scale_dynamic(image, target)),
        // Not a format the `image` crate was built with — which is the AVIF and
        // HEIC path, and the one PLAN §6 cares about most.
        Err(first) => ffmpeg_still(path, target).inspect_err(|_| {
            log::debug!("{}: image crate said: {first}", path.display());
        }),
    }
}

/// Fit `(w, h)` inside `target`, never enlarging.
///
/// Pure, so the arithmetic that decides how much memory a decode costs is a
/// unit test rather than a thing observed in a profiler.
pub fn fit(w: u32, h: u32, target: (u32, u32)) -> (u32, u32) {
    let (tw, th) = (target.0.max(1), target.1.max(1));
    if w == 0 || h == 0 {
        return (1, 1);
    }
    if w <= tw && h <= th {
        return (w, h);
    }
    let k = (tw as f64 / w as f64).min(th as f64 / h as f64);
    (
        ((w as f64 * k).round() as u32).max(1),
        ((h as f64 * k).round() as u32).max(1),
    )
}

fn scale_dynamic(image: image::DynamicImage, target: (u32, u32)) -> Rgba {
    let (w, h) = (image.width(), image.height());
    let (tw, th) = fit(w, h, target);
    let rgba = if (tw, th) == (w, h) {
        image.to_rgba8()
    } else {
        // Catmull-Rom: the filter delightviewer downscales with, so a
        // thumbnail handed between the two programs looks the same in both.
        image
            .resize_exact(tw, th, image::imageops::FilterType::CatmullRom)
            .to_rgba8()
    };
    Rgba {
        width: rgba.width(),
        height: rgba.height(),
        pixels: rgba.into_raw(),
    }
}

/// One frame out of anything ffmpeg can open, scaled by swscale.
///
/// Ported from delightviewer's `dlv-doc/src/still.rs`, minus its float/HDR tone
/// mapping — an EXR in a file manager's preview pane is rare enough that the
/// clipped answer swscale gives is the right amount of effort for now.
fn ffmpeg_still(path: &Path, target: (u32, u32)) -> Result<Rgba, String> {
    use dv_media::ffmpeg;
    use ffmpeg::software::scaling::{Context as Scaler, Flags as ScaleFlags};
    use ffmpeg::util::frame::video::Video as VideoFrame;

    // Idempotent; dv-media does the same from each of its entry points.
    ffmpeg::init().map_err(|e| format!("ffmpeg init: {e}"))?;

    let mut input = ffmpeg::format::input(&path).map_err(|e| e.to_string())?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| "no decoder for this format".to_string())?;
    let index = stream.index();
    // The container's display matrix, read by the same code `dv_media::probe`
    // reads it with. A poster that ignores it is a portrait clip lying on its
    // side, and — worse — a poster the *player's* frame cannot line up with.
    let (rotation, mirrored) = dv_media::display_orientation(&stream);
    let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?;
    let mut decoder = context.decoder().video().map_err(|e| e.to_string())?;

    let mut frame = VideoFrame::empty();
    let mut got = false;
    for (s, packet) in input.packets() {
        if s.index() != index {
            continue;
        }
        decoder.send_packet(&packet).map_err(|e| e.to_string())?;
        if decoder.receive_frame(&mut frame).is_ok() {
            got = true;
            break;
        }
    }
    if !got {
        // Some single-frame formats hand nothing back until the decoder is
        // told the file is over.
        decoder.send_eof().map_err(|e| e.to_string())?;
        got = decoder.receive_frame(&mut frame).is_ok();
    }
    if !got {
        return Err("decoded no frames".to_string());
    }

    let (w, h) = (frame.width().max(1), frame.height().max(1));
    // Fit the *upright* picture to the pane, then ask swscale for that size
    // back in coded orientation — so a portrait clip is scaled to the pane's
    // height rather than to the height of the landscape it is stored as.
    let quarter = matches!(rotation % 360, 90 | 270);
    let (ow, oh) = if quarter { (h, w) } else { (w, h) };
    let (ow, oh) = fit(ow, oh, target);
    let (tw, th) = if quarter { (oh, ow) } else { (ow, oh) };
    let mut scaler = Scaler::get(
        frame.format(),
        w,
        h,
        ffmpeg::format::Pixel::RGBA,
        tw,
        th,
        // Bilinear on a downscale of this ratio is indistinguishable from
        // anything slower, and this runs on the path a held arrow key retries.
        ScaleFlags::BILINEAR,
    )
    .map_err(|e| e.to_string())?;
    let mut rgba = VideoFrame::empty();
    scaler.run(&frame, &mut rgba).map_err(|e| e.to_string())?;

    let pixels = pack(rgba.data(0), rgba.stride(0), tw, th);
    Ok(rotate_rgba(
        Rgba {
            width: tw,
            height: th,
            pixels,
        },
        rotation,
        mirrored,
    ))
}

/// Apply a container's display matrix to decoded pixels: `rotation` degrees
/// **clockwise**, after a left-to-right flip when `mirrored`.
///
/// The video *frames* invert this into four UVs instead (`preview::paint`),
/// which costs nothing per frame; a poster is decoded once and then drawn for
/// as long as the cursor sits on the file, so it is cheaper to turn the pixels
/// here and hand the rest of the program an upright picture. Pure, so the
/// eight cases are a table test rather than a thing squinted at in the pane.
pub fn rotate_rgba(image: Rgba, rotation: u32, mirrored: bool) -> Rgba {
    let rotation = rotation % 360;
    if rotation == 0 && !mirrored {
        return image;
    }
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 || image.pixels.len() < w * h * 4 {
        return image;
    }
    let quarter = matches!(rotation, 90 | 270);
    let (ow, oh) = if quarter { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            // Where this destination pixel comes from — the inverse of
            // "mirror, then turn clockwise".
            let (u, v) = match rotation {
                90 => (y, h - 1 - x),
                180 => (w - 1 - x, h - 1 - y),
                270 => (w - 1 - y, x),
                _ => (x, y),
            };
            let u = if mirrored { w - 1 - u } else { u };
            let src = (v * w + u) * 4;
            let dst = (y * ow + x) * 4;
            out[dst..dst + 4].copy_from_slice(&image.pixels[src..src + 4]);
        }
    }
    Rgba {
        width: ow as u32,
        height: oh as u32,
        pixels: out,
    }
}

/// swscale pads rows to its own alignment; hand back a tightly packed buffer.
///
/// A plane shorter than its stride claims it is is not worth a panic — the
/// rest of the picture goes out transparent, which is what the same question
/// gets answered with in delightviewer.
fn pack(plane: &[u8], stride: usize, width: u32, height: u32) -> Vec<u8> {
    let row = width as usize * 4;
    let mut out = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        match plane.get(y * stride..y * stride + row) {
            Some(slice) => out.extend_from_slice(slice),
            None => {
                out.resize(row * height as usize, 0);
                break;
            }
        }
    }
    out
}

/// Write a thumbnail for `path` into the shared yazi cache.
///
/// Temp name in the same directory, then rename over the target, so a reader —
/// yazi, delightviewer, another delightfile — never sees half a JPEG. Every
/// failure is silent past a debug line: this is a favour, and a favour that
/// fails must not become an error the user has to read.
fn write_thumb(path: &Path, width: u32, height: u32, pixels: &[u8]) {
    let Some(target) = store_thumb(path, STILL_SKIP) else {
        return;
    };
    if target.exists() {
        return;
    }
    let (tw, th) = fit(width, height, (THUMB_MAX_SIDE, THUMB_MAX_SIDE));
    let Some(source) = image::RgbaImage::from_raw(width, height, pixels.to_vec()) else {
        return;
    };
    let scaled = if (tw, th) == (width, height) {
        image::DynamicImage::ImageRgba8(source)
    } else {
        image::DynamicImage::ImageRgba8(source).resize_exact(
            tw,
            th,
            image::imageops::FilterType::CatmullRom,
        )
    };
    // JPEG has no alpha, and the cache's readers all expect RGB.
    let rgb = scaled.to_rgb8();

    let mut encoded = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, THUMB_QUALITY);
    if let Err(e) = encoder.encode(&rgb, tw, th, image::ExtendedColorType::Rgb8) {
        log::debug!("thumbnail encode failed for {}: {e}", path.display());
        return;
    }

    // The temporary name carries the pid, so two delightfiles racing on the
    // same file write two temporaries and rename one after the other rather
    // than corrupting each other's.
    let temp = target.with_extension(format!("df{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&temp, &encoded) {
        log::debug!("thumbnail write failed for {}: {e}", path.display());
        return;
    }
    if let Err(e) = std::fs::rename(&temp, &target) {
        log::debug!("thumbnail rename failed for {}: {e}", path.display());
        let _ = std::fs::remove_file(&temp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole `image`-crate path, end to end: a real file on disk, decoded
    /// and scaled to a pane. Not a pure-function test, but it is the one thing
    /// a mocked test cannot prove — that the bytes, the decoder and the fit
    /// agree about which way round the picture is.
    ///
    /// The **ffmpeg** path (AVIF, HEIC) has no hermetic fixture — it needs a
    /// system ffmpeg with dav1d — so it is verified by hand against a generated
    /// `.avif` rather than here; see the module header.
    #[test]
    fn a_png_decodes_and_is_scaled_to_the_pane() {
        let dir = std::env::temp_dir().join(format!("df-decode-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("wide.png");
        // 400x100: wider than it is tall, so a square target proves the aspect
        // ratio survives the round trip rather than being squashed to fit.
        let source = image::RgbaImage::from_fn(400, 100, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 0, 255])
        });
        image::DynamicImage::ImageRgba8(source)
            .save(&path)
            .expect("write the fixture");

        let decoded = decode_file(&path, (200, 200)).expect("decode");
        assert_eq!((decoded.width, decoded.height), (200, 50));
        assert_eq!(
            decoded.pixels.len(),
            200 * 50 * 4,
            "the buffer is not tightly packed RGBA"
        );

        // A pane larger than the picture leaves it alone.
        let decoded = decode_file(&path, (2000, 2000)).expect("decode");
        assert_eq!((decoded.width, decoded.height), (400, 100));

        // Something that is not an image at all fails with a message rather
        // than a panic — the path an SVG and a corrupt file both take.
        let bad = dir.join("not-an-image.png");
        std::fs::write(&bad, b"this is not a png").expect("write");
        assert!(decode_file(&bad, (200, 200)).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fitting_never_enlarges() {
        // Smaller than the pane: left exactly as it is. The *pane* does draw a
        // 16×16 icon at 400 points now (`paint::fit_rect`); what it must never
        // do is spend 400 points of memory on 16 points of picture, so the
        // enlargement is a rectangle and never a resample.
        assert_eq!(fit(16, 16, (400, 600)), (16, 16));
        assert_eq!(fit(400, 600, (400, 600)), (400, 600));
    }

    /// The eight cases of a display matrix, on a picture whose every pixel
    /// says where it came from.
    #[test]
    fn a_display_matrix_turns_the_pixels() {
        // 2×3 (w×h), each pixel tagged with its own (x, y).
        let source = Rgba {
            width: 2,
            height: 3,
            pixels: (0..3)
                .flat_map(|y: u8| (0..2).flat_map(move |x: u8| [x, y, 0, 255]))
                .collect(),
        };
        let at = |image: &Rgba, x: u32, y: u32| {
            let i = ((y * image.width + x) * 4) as usize;
            (image.pixels[i], image.pixels[i + 1])
        };
        let copy = |image: &Rgba| Rgba {
            width: image.width,
            height: image.height,
            pixels: image.pixels.clone(),
        };

        // No matrix: the same buffer, untouched.
        let same = rotate_rgba(copy(&source), 0, false);
        assert_eq!((same.width, same.height), (2, 3));
        assert_eq!(at(&same, 1, 2), (1, 2));

        // A quarter turn clockwise: the source's top-left ends up top-right.
        let turned = rotate_rgba(copy(&source), 90, false);
        assert_eq!((turned.width, turned.height), (3, 2));
        assert_eq!(at(&turned, 2, 0), (0, 0));
        assert_eq!(at(&turned, 0, 0), (0, 2));
        assert_eq!(at(&turned, 2, 1), (1, 0));

        // A half turn: opposite corners swap and the footprint does not.
        let flipped = rotate_rgba(copy(&source), 180, false);
        assert_eq!((flipped.width, flipped.height), (2, 3));
        assert_eq!(at(&flipped, 0, 0), (1, 2));
        assert_eq!(at(&flipped, 1, 2), (0, 0));

        // Three quarters: the source's top-left ends up bottom-left.
        let back = rotate_rgba(copy(&source), 270, false);
        assert_eq!((back.width, back.height), (3, 2));
        assert_eq!(at(&back, 0, 1), (0, 0));
        assert_eq!(at(&back, 0, 0), (1, 0));

        // Mirrored, unturned: columns reverse, rows do not.
        let mirrored = rotate_rgba(copy(&source), 0, true);
        assert_eq!((mirrored.width, mirrored.height), (2, 3));
        assert_eq!(at(&mirrored, 0, 0), (1, 0));
        assert_eq!(at(&mirrored, 1, 2), (0, 2));

        // Mirrored *then* turned, which is the order the display matrix means.
        let both = rotate_rgba(copy(&source), 90, true);
        assert_eq!((both.width, both.height), (3, 2));
        assert_eq!(at(&both, 2, 0), (1, 0));

        // Four quarter turns is where it started.
        let round = (0..4).fold(copy(&source), |image, _| rotate_rgba(image, 90, false));
        assert_eq!((round.width, round.height), (2, 3));
        assert_eq!(round.pixels, source.pixels);

        // A truncated buffer is a decoder bug, not a panic.
        let short = rotate_rgba(
            Rgba {
                width: 4,
                height: 4,
                pixels: vec![0; 8],
            },
            90,
            false,
        );
        assert_eq!((short.width, short.height), (4, 4));
    }

    #[test]
    fn fitting_preserves_the_aspect_ratio() {
        // Wide: the width binds.
        assert_eq!(fit(4000, 2000, (400, 600)), (400, 200));
        // Tall: the height binds.
        assert_eq!(fit(2000, 4000, (400, 600)), (300, 600));
        // Square in a portrait pane.
        assert_eq!(fit(1000, 1000, (400, 600)), (400, 400));
    }

    #[test]
    fn fitting_survives_degenerate_sizes() {
        assert_eq!(fit(0, 0, (400, 600)), (1, 1));
        assert_eq!(fit(100, 100, (0, 0)), (1, 1));
        // A 10000×1 panorama must not round its height to zero.
        let (w, h) = fit(10_000, 1, (400, 600));
        assert!(w >= 1 && h >= 1, "got {w}×{h}");
    }

    #[test]
    fn packing_drops_the_stride_padding() {
        // Two rows of two pixels, with four bytes of padding per row.
        let stride = 2 * 4 + 4;
        let mut plane = vec![0u8; stride * 2];
        plane[0] = 1;
        plane[stride] = 2;
        let packed = pack(&plane, stride, 2, 2);
        assert_eq!(packed.len(), 2 * 2 * 4);
        assert_eq!(packed[0], 1);
        assert_eq!(packed[8], 2, "the second row started at the padding");
    }

    /// A truncated plane must produce a whole picture, not a panic.
    #[test]
    fn packing_a_short_plane_is_transparent_not_fatal() {
        let packed = pack(&[0u8; 4], 16, 4, 4);
        assert_eq!(packed.len(), 4 * 4 * 4);
    }
}
