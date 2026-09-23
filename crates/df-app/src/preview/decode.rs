//! The decode side of PLAN §6's seam: pixels, off the paint thread.
//!
//! df-core answers [`df_core::preview::Preview::NeedsDecode`] for anything it
//! cannot render from bytes it already read — images, video, audio, PDFs,
//! fonts, models — and this is what takes that over. What it decodes out of
//! the file itself is decided by [`plan`]: a picture is decoded whole, a song
//! gives up its **sleeve**, and everything else arrives here only for its
//! cached thumbnail — the poster a video's first frame, or a PDF's first page,
//! lands on top of.
//!
//! ## A song's picture is its sleeve
//!
//! A tagged MP3, FLAC or M4A carries its artwork as a one-frame `ATTACHED_PIC`
//! stream. dv-media's probe, decoder and keyframe index skip that stream on
//! purpose — a song with a sleeve is audio, not a one-frame video — so the
//! transport never shows it, and the only way it reaches the screen is here:
//! [`dv_media::cover_art`], on this worker, fitted to the pane like any other
//! still and written back to the shared cache like any other still, which is
//! how the grid's tile for the song gets it too.
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
//! ## …and then the loop
//!
//! A GIF is a picture that moves, and a preview pane that showed the first
//! frame of one was showing a still of a thing whose whole content is the
//! motion. So after the still goes out, the formats that can hold an animation
//! are asked for their frames and those are **streamed** — one message apiece,
//! in order, each carrying how long it is held — so a long loop starts playing
//! from its first frame while its last is still being decoded.
//!
//! GIF, animated WebP and APNG come from the `image` crate's own
//! [`image::AnimationDecoder`], which composites each frame's disposal and blend
//! for us. Animated AVIF and HEIF have no decoder in the `image` crate at all,
//! so they come through the same ffmpeg path the stills do, with the delays
//! worked out from the stream's timestamps.
//!
//! Two bounds, because a decode worker is not a place to find out how long a
//! GIF somebody downloaded is: [`ANIM_BUDGET_BYTES`] of decoded pixels and
//! [`ANIM_MAX_FRAMES`] frames. Past either, what has been sent *is* the loop.
//!
//! **SVG is the known gap.** df-core routes `image/svg+xml` to the image
//! previewer (a picture is what the user means by it) but neither decoder
//! rasterises vectors; delightviewer does it with `resvg`, which is a real
//! dependency and therefore a decision for its own commit. Until then an SVG
//! reports "no decoder", which is honest and is not a crash.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;
use df_core::preview::{store_thumb, PreviewKind, PreviewToken, STILL_SKIP};

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

/// How much decoded animation one preview may hold, in bytes.
///
/// Frames are decoded at the *pane's* size rather than the file's, so 64 MiB is
/// a hundred-odd frames of a pane-filling loop and several hundred of a small
/// one. It is a bound on damage, not a target: past it the loop is the frames
/// that fitted, which is a short loop of the right picture rather than a
/// gigabyte spent on a four-thousand-frame GIF somebody left in a folder.
const ANIM_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// …and a frame count past which even a tiny animation stops. A texture apiece
/// is the cost there, not bytes, and nothing reads a loop this long as a loop.
const ANIM_MAX_FRAMES: usize = 512;

/// What a delay of nothing means.
///
/// GIFs written for browsers say `0` or `10 ms` for "as fast as you can", and
/// every browser answers the same way: a tenth of a second. Matching them is
/// what makes a GIF run here at the speed it runs everywhere else, rather than
/// at whatever the repaint loop can manage.
const ANIM_ZERO_DELAY_MS: u32 = 100;

/// The delay the ffmpeg path falls back to when the container has no timing to
/// read — the same tenth of a second, for the same reason.
const ANIM_DEFAULT_DELAY_MS: u32 = 100;

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
    /// One frame of an animated image, in order from the first, with how long
    /// it is held on screen.
    ///
    /// They arrive **after** the [`Stage::Full`] still, which is frame zero
    /// decoded on its own: a GIF is a picture before it is a loop, and the
    /// picture is on screen while the rest of the frames are still coming.
    /// Frame zero is therefore sent twice — once as the still and once as the
    /// head of the loop — which is one extra upload of one frame and the price
    /// of not making the still path wait for the animation's.
    Frame { delay_ms: u32 },
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
    ///
    /// `Ok(None)` is a [`Stage::Full`] that worked and found nothing to draw:
    /// a song with no sleeve. It is not an error — most songs have no art —
    /// and it is not silence either, because the pane is waiting on it (see
    /// [`Plan::decoding`]) and the audio card is waiting on the pane.
    pub result: Result<Option<egui::ColorImage>, String>,
}

/// Where the **full** picture — the one that replaces the cached thumbnail —
/// comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Full {
    /// Somewhere other than this worker. A video's frames are
    /// [`crate::playback`]'s, and a PDF's, font's or model's pages are
    /// [`super::doc`]'s; the cached thumbnail is all this worker fetches, as
    /// the poster those land on top of.
    Elsewhere,
    /// The file *is* the picture: decode it, and its loop if it has one.
    File,
    /// The file is a song, and its picture is the sleeve riding inside it
    /// ([`dv_media::cover_art`]). Never the file's bytes: those are the
    /// music, and a 400 MB FLAC is not read into memory to find a JPEG at its
    /// front.
    CoverArt,
}

/// What the pane asks the worker for, for one file: the part of a request that
/// is a decision rather than plumbing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub full: Full,
    /// Write the full decode back to the shared cache (see [`Job::store`]).
    pub store: bool,
    /// Something is coming, so an empty pane is "not yet" rather than "no
    /// picture" — which decides whether the pane says what the file is, and
    /// whether the audio card may take it.
    pub decoding: bool,
}

/// Decide what to decode for a file of `kind`, given whether df-core found a
/// cached thumbnail for it.
///
/// Pure, so "a song's poster comes from its cover art and a video's does not"
/// is a table test rather than a thing noticed in the pane.
pub fn plan(kind: &PreviewKind, cached: bool) -> Plan {
    let full = match kind {
        PreviewKind::Image => Full::File,
        PreviewKind::Audio => Full::CoverArt,
        _ => Full::Elsewhere,
    };
    let fetches = full != Full::Elsewhere;
    Plan {
        full,
        // Only a decode that happens here can be written back, only when the
        // cache did not already have it, and only for the kinds the cache is
        // read back for — otherwise it is a write nobody reads.
        store: fetches && !cached && kind.thumbnailable(),
        decoding: fetches || cached,
    }
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
    /// What to decode after the thumbnail, if anything ([`plan`]).
    pub full: Full,
    /// Whether a successful full decode should be written back to the shared
    /// cache. Only when there was no thumbnail to begin with, and only for the
    /// kinds that cache is read back for.
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
                if !send(Stage::Thumb, Ok(Some(image))) {
                    return;
                }
            }
            // A cache entry that will not decode is a miss, not an error: the
            // real decode is still on its way and is the answer either way.
            Err(e) => log::debug!("cached thumbnail {} is unreadable: {e}", thumb.display()),
        }
    }

    if !live(state, token) {
        return;
    }

    // The file's own picture. `bytes` is kept only for a picture file: it is
    // read once and decoded twice at most — the still comes out of these
    // bytes, and so, for the handful of formats that have one, does the loop.
    let mut bytes = None;
    let still = match full {
        // The thumbnail was the whole job; the real pixels are not ours.
        Full::Elsewhere => return,
        // A sleeve that will not decode is a song with no sleeve. The file is
        // still a song that plays, and printing ffmpeg's complaint across the
        // pane would be reporting a failure nobody asked about.
        Full::CoverArt => Ok(decode_cover(&path, target).unwrap_or_else(|e| {
            log::debug!("{}: no cover art: {e}", path.display());
            None
        })),
        Full::File => read_source(&path).and_then(|read| {
            let still = decode_bytes(&path, &read, target);
            bytes = Some(read);
            still.map(Some)
        }),
    };

    match still {
        Ok(Some(image)) => {
            let store_from = store.then(|| (image.width, image.height, image.pixels.clone()));
            let color = match to_color(&image) {
                Ok(color) => color,
                Err(e) => {
                    send(Stage::Full, Err(e));
                    return;
                }
            };
            if !send(Stage::Full, Ok(Some(color))) {
                return;
            }
            // After the pane has its pixels, never before: the write-back is a
            // favour to the *next* program to look at this file, and it must
            // not delay the preview it is a by-product of.
            if let Some((w, h, pixels)) = store_from {
                write_thumb(&path, w, h, &pixels);
            }
        }
        // Nothing to draw, and nothing wrong: said out loud all the same,
        // because the pane is holding the space open until it hears.
        Ok(None) => {
            send(Stage::Full, Ok(None));
            return;
        }
        Err(e) => {
            log::debug!("{}: {e}", path.display());
            send(Stage::Full, Err(e));
            return;
        }
    }
    let Some(bytes) = bytes else {
        // A sleeve is one picture; there is no loop to look for.
        return;
    };

    // ── …and then the loop, if the file has one ─────────────────────────────
    //
    // Streamed frame by frame rather than gathered up and sent in one piece: a
    // long GIF plays from its first frame while its last is still being
    // decoded, which is what a browser does and the only version that reads as
    // the file opening rather than as the file loading.
    let Some(route) = animated_route(&bytes) else {
        return;
    };
    if !live(state, token) {
        return;
    }
    // **The budget is spent before the frame is decoded, not after.** Every
    // frame is scaled to the pane, so what the next one will cost is known
    // without decoding it: `target` pixels, four bytes each. Asking afterwards
    // meant the frame that broke the bound was decoded, scaled and thrown away
    // — the one allocation the bound exists to prevent, paid for in full at the
    // exact moment the loop was declared too big.
    let budget = Cell::new(ANIM_BUDGET_BYTES);
    let sent = Cell::new(0usize);
    let stopped = Cell::new(false);
    let projected = target.0 as usize * target.1 as usize * 4;
    // Cells rather than captured `mut`, because the two closures are live at
    // once: one is asked whether to decode, the other is handed what was
    // decoded, and they share one purse.
    let mut room = || {
        if sent.get() >= ANIM_MAX_FRAMES || projected > budget.get() {
            // Out of room. What has already been sent *is* the loop — a
            // shorter loop of the right picture, which is the honest answer
            // and needs nothing said in the pane to be one.
            stopped.set(true);
            return false;
        }
        true
    };
    let mut emit = |delay_ms: u32, image: Rgba| -> bool {
        let cost = image.pixels.len();
        // A frame bigger than the projection — a decoder that ignored the
        // scale — still cannot overdraw the purse.
        if cost > budget.get() {
            stopped.set(true);
            return false;
        }
        budget.set(budget.get() - cost);
        let Ok(color) = to_color(&image) else {
            return false;
        };
        sent.set(sent.get() + 1);
        send(Stage::Frame { delay_ms }, Ok(Some(color)))
    };
    match stream_animation(&path, &bytes, target, route, &mut room, &mut emit) {
        Ok(0) => {}
        Ok(frames) => {
            if stopped.get() {
                log::debug!(
                    "{}: {frames} frames is as much of the loop as {} holds",
                    path.display(),
                    crate::format::human_size(ANIM_BUDGET_BYTES as u64),
                );
            }
        }
        // A file that decoded a still and then would not decode its frames is
        // a still, and the still is already on screen. Nothing to report.
        Err(e) => log::debug!("{}: no animation: {e}", path.display()),
    }
}

/// Which decoder can hand back this file's frames, if any.
enum Route {
    /// The `image` crate's own [`image::AnimationDecoder`] — GIF, animated
    /// WebP, APNG.
    Frames,
    /// ffmpeg, for the ISOBMFF image sequences the `image` crate has no
    /// decoder for at all: animated AVIF and HEIF.
    Ffmpeg,
}

/// Is this file worth asking for frames from?
///
/// Deliberately narrow. Every still that reaches here would otherwise pay for
/// the question — a second full ffmpeg decode of every HEIC photograph, on the
/// path a held arrow key retries — so the ffmpeg route is taken only when the
/// container's own brands say the file is a *sequence*, and the `image` route
/// only for the three formats that can hold one.
fn animated_route(bytes: &[u8]) -> Option<Route> {
    match image::guess_format(bytes) {
        // The decoders themselves answer whether these are animated: ruling a
        // still PNG or WebP out costs a header parse, which is nothing beside
        // the decode that has already happened.
        Ok(image::ImageFormat::Gif | image::ImageFormat::WebP | image::ImageFormat::Png) => {
            Some(Route::Frames)
        }
        // AVIF is a format the `image` crate names but the workspace builds no
        // decoder for, and HEIF it does not name at all — both arrive here as
        // an ISOBMFF file, and both say in their brands whether there is more
        // than one picture inside.
        Ok(image::ImageFormat::Avif) | Err(_) => iso_sequence(bytes).then_some(Route::Ffmpeg),
        Ok(_) => None,
    }
}

/// Do an ISOBMFF file's `ftyp` brands claim an image **sequence**?
///
/// `avis` is animated AVIF's major brand (a still is `avif`); `msf1` is HEIF's
/// image-sequence brand. Either can appear in the compatible-brand list rather
/// than in the major slot, so both are looked at. Pure, and four bytes at a
/// time, so it is a unit test rather than a thing observed on somebody's phone.
///
/// **The box's shape is respected rather than swept.** `ftyp` is a length, the
/// tag, the major brand, a four-byte `minor_version` **number**, and then the
/// compatible brands — and the number is not a brand. Scanning from byte 8 in
/// four-byte steps read it as one, so a file whose minor version happened to
/// spell `avis` would have been sent down the ffmpeg path for frames it does
/// not have.
fn iso_sequence(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let Ok(length) = bytes[0..4].try_into().map(u32::from_be_bytes) else {
        return false;
    };
    let sequence = |brand: &[u8]| matches!(brand, b"avis" | b"msf1");
    if sequence(&bytes[8..12]) {
        return true;
    }
    // Byte 16, past the minor version. The box's own length bounds the scan,
    // clamped into what is actually here and to a header nobody sane writes
    // more of.
    let end = (length as usize).clamp(16, bytes.len().min(4096));
    bytes[16..end].chunks_exact(4).any(sequence)
}

/// The delay a frame is held for, in milliseconds, as every browser reads it.
///
/// A GIF asking for `0` or `10 ms` is asking for "as fast as you can", which no
/// program has honoured since the nineties; the agreed answer is a tenth of a
/// second, and a loop that ran at the repaint rate instead would be a different
/// animation from the one every other program on the machine shows.
pub fn frame_delay_ms(raw: u32) -> u32 {
    if raw <= 10 {
        ANIM_ZERO_DELAY_MS
    } else {
        raw
    }
}

/// Hand every frame of an animated `path` to `emit`, scaled to `target`.
///
/// Returns how many frames were sent. **One is not an animation** — a still
/// WebP and a single-picture AVIF both come back as one — and the pane reads it
/// that way.
fn stream_animation(
    path: &Path,
    bytes: &[u8],
    target: (u32, u32),
    route: Route,
    room: &mut impl FnMut() -> bool,
    emit: &mut impl FnMut(u32, Rgba) -> bool,
) -> Result<usize, String> {
    use image::AnimationDecoder;
    let cursor = || std::io::Cursor::new(bytes);
    match route {
        Route::Ffmpeg => ffmpeg_frames(path, target, ANIM_MAX_FRAMES, room, emit),
        Route::Frames => match image::guess_format(bytes).map_err(|e| e.to_string())? {
            image::ImageFormat::Gif => {
                let decoder =
                    image::codecs::gif::GifDecoder::new(cursor()).map_err(|e| e.to_string())?;
                Ok(pump(decoder.into_frames(), target, room, emit))
            }
            image::ImageFormat::WebP => {
                let decoder =
                    image::codecs::webp::WebPDecoder::new(cursor()).map_err(|e| e.to_string())?;
                if !decoder.has_animation() {
                    return Ok(0);
                }
                Ok(pump(decoder.into_frames(), target, room, emit))
            }
            image::ImageFormat::Png => {
                let decoder =
                    image::codecs::png::PngDecoder::new(cursor()).map_err(|e| e.to_string())?;
                if !decoder.is_apng().map_err(|e| e.to_string())? {
                    return Ok(0);
                }
                let apng = decoder.apng().map_err(|e| e.to_string())?;
                Ok(pump(apng.into_frames(), target, room, emit))
            }
            other => Err(format!("{other:?} has no frames")),
        },
    }
}

/// Drain an [`image::Frames`] into `emit`, scaling each frame to the pane.
///
/// The frames arrive already composited — the `image` crate's decoders apply a
/// GIF's disposal method and an APNG's blend op for us — so what goes out is a
/// full canvas per frame and the pane has nothing to compose.
fn pump(
    frames: image::Frames<'_>,
    target: (u32, u32),
    room: &mut impl FnMut() -> bool,
    emit: &mut impl FnMut(u32, Rgba) -> bool,
) -> usize {
    let mut sent = 0usize;
    let mut frames = frames;
    // `room()` before `next()`, because `next()` *is* the decode: an iterator
    // that has already handed a frame over has already paid for it.
    while room() {
        let Some(frame) = frames.next() else { break };
        let frame = match frame {
            Ok(frame) => frame,
            // A truncated GIF is a GIF up to the truncation: what decoded is
            // still a loop, and it is the only honest one available.
            Err(e) => {
                log::debug!("animation frame {sent}: {e}");
                break;
            }
        };
        // Read before the buffer is taken: `into_buffer` consumes the frame.
        let (numer, denom) = frame.delay().numer_denom_ms();
        let ms = numer.checked_div(denom).unwrap_or(0);
        let image = scale_dynamic(image::DynamicImage::ImageRgba8(frame.into_buffer()), target);
        sent += 1;
        if !emit(frame_delay_ms(ms), image) {
            break;
        }
    }
    sent
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
    let bytes = read_source(path)?;
    decode_bytes(path, &bytes, target)
}

/// Read a file the decoders are allowed to open.
///
/// Split out from [`decode_file`] so the worker can read once and decode twice
/// — the still, and then the animation's frames out of the same bytes.
fn read_source(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "{} is too large to preview",
            crate::format::human_size(meta.len())
        ));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

/// The still, out of bytes already read.
fn decode_bytes(path: &Path, bytes: &[u8], target: (u32, u32)) -> Result<Rgba, String> {
    match image::load_from_memory(bytes) {
        Ok(image) => Ok(scale_dynamic(image, target)),
        // Not a format the `image` crate was built with — which is the AVIF and
        // HEIC path, and the one PLAN §6 cares about most.
        Err(first) => ffmpeg_still(path, target).inspect_err(|_| {
            log::debug!("{}: image crate said: {first}", path.display());
        }),
    }
}

/// A song's sleeve, fitted to `target` like any other still. `Ok(None)` is a
/// song with no art, which is most of them.
///
/// Two steps, for the reason [`decode_file`] gives: the only full-size buffer
/// should be the one the decoder had to make. `cover_art` shrinks inside the
/// swscale pass it runs anyway, but it can only cap the *long* side, and which
/// side of the pane binds depends on an aspect ratio nobody knows until the
/// picture is open. Capping at the pane's longer side can never undershoot the
/// fit — whichever side binds, the fitted picture is no longer than that — so
/// the 3000-pixel sleeve is gone before this function sees it, and
/// [`scale_dynamic`] makes the exact fit with the same filter every other
/// still in the pane gets.
fn decode_cover(path: &Path, target: (u32, u32)) -> Result<Option<Rgba>, String> {
    let cap = target.0.max(target.1).max(1);
    let Some(art) = dv_media::cover_art(path, Some(cap)).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let (width, height) = (art.width, art.height);
    let image = image::RgbaImage::from_raw(width, height, art.rgba)
        .ok_or_else(|| format!("cover art is shorter than its own {width}×{height}"))?;
    Ok(Some(scale_dynamic(
        image::DynamicImage::ImageRgba8(image),
        target,
    )))
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
    let mut still = None;
    // A still is one frame and the budget is a loop's: `room` says yes once,
    // which is all the `limit` of one will ask it.
    ffmpeg_frames(path, target, 1, &mut || true, &mut |_, image| {
        still = Some(image);
        false
    })?;
    still.ok_or_else(|| "decoded no frames".to_string())
}

/// Every frame ffmpeg can get out of `path`, in order, up to `limit`.
///
/// The still above is this with a limit of one, so a HEIC photograph and an
/// animated AVIF go through exactly the same decoder, scaler and display
/// matrix — two paths here would be two answers to "which way up is it".
///
/// **Each frame is held back one decode**, because a frame's delay is the
/// distance to the *next* one's timestamp and nothing else in the container
/// says it. The last frame out gets the delay the one before it had, which is
/// what a sequence with a constant frame rate means by it anyway. A limit of
/// one skips the hold-back entirely: a still has no delay to work out, and
/// decoding a second frame to learn it would be a second frame decoded for
/// nothing on the path a held arrow key retries.
fn ffmpeg_frames(
    path: &Path,
    target: (u32, u32),
    limit: usize,
    room: &mut impl FnMut() -> bool,
    emit: &mut impl FnMut(u32, Rgba) -> bool,
) -> Result<usize, String> {
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
    // The stream's clock, for turning two timestamps into a delay.
    let time_base = stream.time_base();
    let seconds_per_tick = if time_base.denominator() == 0 {
        0.0
    } else {
        f64::from(time_base.numerator()) / f64::from(time_base.denominator())
    };
    // The container's display matrix, read by the same code `dv_media::probe`
    // reads it with. A poster that ignores it is a portrait clip lying on its
    // side, and — worse — a poster the *player's* frame cannot line up with.
    let (rotation, mirrored) = dv_media::display_orientation(&stream);
    let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?;
    let mut decoder = context.decoder().video().map_err(|e| e.to_string())?;

    // Built on the first frame, when its pixel format and coded size are known,
    // and then reused: a sequence's frames all have the same shape, and
    // rebuilding swscale per frame is the one avoidable cost in this loop.
    let mut scaler: Option<(Scaler, u32, u32)> = None;
    let scale =
        |frame: &VideoFrame, scaler: &mut Option<(Scaler, u32, u32)>| -> Result<Rgba, String> {
            let (w, h) = (frame.width().max(1), frame.height().max(1));
            if scaler.is_none() {
                // Fit the *upright* picture to the pane, then ask swscale for that
                // size back in coded orientation — so a portrait clip is scaled to
                // the pane's height rather than to the height of the landscape it
                // is stored as.
                let quarter = matches!(rotation % 360, 90 | 270);
                let (ow, oh) = if quarter { (h, w) } else { (w, h) };
                let (ow, oh) = fit(ow, oh, target);
                let (tw, th) = if quarter { (oh, ow) } else { (ow, oh) };
                let built = Scaler::get(
                    frame.format(),
                    w,
                    h,
                    ffmpeg::format::Pixel::RGBA,
                    tw,
                    th,
                    // Bilinear on a downscale of this ratio is indistinguishable
                    // from anything slower, and this runs on the path a held arrow
                    // key retries.
                    ScaleFlags::BILINEAR,
                )
                .map_err(|e| e.to_string())?;
                *scaler = Some((built, tw, th));
            }
            let Some((scaler, tw, th)) = scaler.as_mut() else {
                return Err("no scaler".to_string());
            };
            let (tw, th) = (*tw, *th);
            let mut rgba = VideoFrame::empty();
            scaler.run(frame, &mut rgba).map_err(|e| e.to_string())?;
            Ok(rotate_rgba(
                Rgba {
                    width: tw,
                    height: th,
                    pixels: pack(rgba.data(0), rgba.stride(0), tw, th),
                },
                rotation,
                mirrored,
            ))
        };

    // The frame waiting for the next timestamp to tell it how long it lasts,
    // and the delay the one before it was given.
    let mut pending: Option<(i64, Rgba)> = None;
    let mut last_delay = ANIM_DEFAULT_DELAY_MS;
    let mut sent = 0usize;
    let mut wanted = true;

    // Every decoded frame passes through here: it either goes straight out (a
    // still, which has nothing to wait for) or displaces the one held back.
    let mut offer = |pts: i64,
                     image: Rgba,
                     pending: &mut Option<(i64, Rgba)>,
                     sent: &mut usize,
                     last_delay: &mut u32|
     -> bool {
        if limit <= 1 {
            *sent += 1;
            return emit(ANIM_DEFAULT_DELAY_MS, image);
        }
        let Some((held_pts, held)) = pending.replace((pts, image)) else {
            return true;
        };
        let ticks = (pts - held_pts).max(0) as f64;
        let ms = (ticks * seconds_per_tick * 1000.0).round();
        let delay = if ms > 0.0 && ms < f64::from(u32::MAX) {
            frame_delay_ms(ms as u32)
        } else {
            *last_delay
        };
        *last_delay = delay;
        *sent += 1;
        emit(delay, held)
    };

    'packets: for (s, packet) in input.packets() {
        if s.index() != index {
            continue;
        }
        decoder.send_packet(&packet).map_err(|e| e.to_string())?;
        let mut frame = VideoFrame::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            // Before the scale, which is where the frame-sized allocation is.
            if !room() {
                wanted = false;
                break 'packets;
            }
            let pts = frame.timestamp().or_else(|| frame.pts()).unwrap_or(0);
            let image = scale(&frame, &mut scaler)?;
            wanted = offer(pts, image, &mut pending, &mut sent, &mut last_delay);
            if !wanted || sent >= limit {
                break 'packets;
            }
        }
    }
    if wanted && sent < limit {
        // Some single-frame formats hand nothing back until the decoder is told
        // the file is over — and a sequence's tail sits in the same queue.
        decoder.send_eof().map_err(|e| e.to_string())?;
        let mut frame = VideoFrame::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            if !room() {
                wanted = false;
                break;
            }
            let pts = frame.timestamp().or_else(|| frame.pts()).unwrap_or(0);
            let image = scale(&frame, &mut scaler)?;
            wanted = offer(pts, image, &mut pending, &mut sent, &mut last_delay);
            if !wanted || sent >= limit {
                break;
            }
        }
    }
    // The one still held back, with the delay its predecessor had.
    if wanted && sent < limit {
        if let Some((_, held)) = pending.take() {
            sent += 1;
            emit(last_delay, held);
        }
    }
    if sent == 0 {
        return Err("decoded no frames".to_string());
    }
    Ok(sent)
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

    /// **A GIF is a picture that moves**, end to end: a real three-frame file,
    /// through the route chooser, the animation decoder and the pane-sized
    /// scale, with each frame's delay coming out the other side.
    #[test]
    fn a_gif_streams_its_frames_with_their_delays() {
        let dir = std::env::temp_dir().join(format!("df-anim-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("spin.gif");

        // Three 60×40 frames, each a flat colour, at 120 ms apiece — long
        // enough not to trip the browsers' "as fast as you can" clamp.
        let frames: Vec<image::Frame> = [0u8, 128, 255]
            .into_iter()
            .map(|value| {
                let buffer = image::RgbaImage::from_pixel(60, 40, image::Rgba([value, 0, 0, 255]));
                image::Frame::from_parts(buffer, 0, 0, image::Delay::from_numer_denom_ms(120, 1))
            })
            .collect();
        {
            let file = std::fs::File::create(&path).expect("write the fixture");
            let mut encoder = image::codecs::gif::GifEncoder::new(file);
            encoder.encode_frames(frames).expect("encode the fixture");
        }
        let bytes = std::fs::read(&path).expect("read back");

        // The still comes out first and unchanged — a GIF is a picture before
        // it is a loop.
        let still = decode_bytes(&path, &bytes, (30, 30)).expect("the still");
        assert_eq!((still.width, still.height), (30, 20));

        let route = animated_route(&bytes).expect("a GIF has frames");
        assert!(matches!(route, Route::Frames));
        let mut got: Vec<(u32, u32, u32)> = Vec::new();
        let sent = stream_animation(
            &path,
            &bytes,
            (30, 30),
            route,
            &mut || true,
            &mut |delay, image| {
                got.push((delay, image.width, image.height));
                true
            },
        )
        .expect("stream");
        assert_eq!(sent, 3);
        assert_eq!(got, vec![(120, 30, 20); 3], "delays and the pane's fit");

        // **The emitter's `false` stops it**, which is how the memory cap is
        // enforced: what has been handed over is the loop, and nothing is
        // decoded past it.
        let mut count = 0;
        let route = animated_route(&bytes).expect("a GIF has frames");
        let sent = stream_animation(&path, &bytes, (30, 30), route, &mut || true, &mut |_, _| {
            count += 1;
            count < 2
        })
        .expect("stream");
        assert_eq!((sent, count), (2, 2));

        // …and **the budget stops it a frame earlier still**: a `room` that
        // says no is asked before the decoder is pulled, so the frame that
        // would not have fitted is never decoded and never scaled.
        let mut decoded = 0;
        let mut allowed = 2;
        let route = animated_route(&bytes).expect("a GIF has frames");
        let sent = stream_animation(
            &path,
            &bytes,
            (30, 30),
            route,
            &mut || {
                allowed -= 1;
                allowed > 0
            },
            &mut |_, _| {
                decoded += 1;
                true
            },
        )
        .expect("stream");
        assert_eq!(
            (sent, decoded),
            (1, 1),
            "one frame decoded, and the refusal costs no decode of its own"
        );

        // A still PNG goes down the same route and comes back with nothing:
        // "can hold an animation" is not "does".
        let png = dir.join("flat.png");
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([1, 2, 3, 255]),
        ))
        .save(&png)
        .expect("write");
        let bytes = std::fs::read(&png).expect("read back");
        let route = animated_route(&bytes).expect("PNG can be an APNG");
        assert_eq!(
            stream_animation(&png, &bytes, (30, 30), route, &mut || true, &mut |_, _| {
                true
            })
            .expect("stream"),
            0,
            "a still PNG is not an APNG"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A JPEG must not pay for the question.** The route chooser is what
    /// keeps a second full decode off every photograph in a folder, so which
    /// formats it says yes to is the test.
    #[test]
    fn only_the_formats_that_can_animate_are_asked_for_frames() {
        let jpeg = [0xff, 0xd8, 0xff, 0xdb, 0, 0, 0, 0];
        assert!(animated_route(&jpeg).is_none(), "a JPEG has no frames");
        assert!(animated_route(b"GIF89a...........").is_some());
        assert!(animated_route(b"").is_none(), "and neither has nothing");

        // ISOBMFF: a still AVIF is left alone, a sequence is not. Built the
        // way the box really is — major brand, then the four-byte minor
        // *version*, then the compatible brands.
        let iso = |major: &[u8; 4], minor: u32, compatible: &[&[u8; 4]]| {
            let mut bytes = Vec::new();
            let length = 16 + compatible.len() * 4;
            bytes.extend_from_slice(&(length as u32).to_be_bytes());
            bytes.extend_from_slice(b"ftyp");
            bytes.extend_from_slice(major);
            bytes.extend_from_slice(&minor.to_be_bytes());
            for brand in compatible {
                bytes.extend_from_slice(*brand);
            }
            // Enough tail that the 16-byte floor is met on the short cases.
            bytes.resize(bytes.len().max(32), 0);
            bytes
        };
        assert!(!iso_sequence(&iso(b"avif", 0, &[b"mif1"])), "a still AVIF");
        assert!(
            iso_sequence(&iso(b"avis", 0, &[b"avif"])),
            "the major brand"
        );
        assert!(
            iso_sequence(&iso(b"mif1", 0, &[b"msf1"])),
            "a compatible brand"
        );
        // The minor version is a number, not a brand: a still whose version
        // spells one must not be read as a sequence.
        let spelled = u32::from_be_bytes(*b"avis");
        assert!(
            !iso_sequence(&iso(b"avif", spelled, &[b"mif1"])),
            "the minor version is not a brand"
        );
        assert!(!iso_sequence(b"not an iso file at all, honestly"));
        assert!(!iso_sequence(b"short"), "a truncated header is not a panic");
    }

    /// **A song's poster comes from its cover art; a video's does not.** The
    /// regression this exists for: dv-media stopped calling a tagged song a
    /// one-frame video, which was the only road its sleeve had to the screen,
    /// and nothing here had been asked to decode it instead.
    #[test]
    fn a_songs_picture_is_its_cover_art_and_a_videos_is_not_decoded_here() {
        // A song: the sleeve, written back when the cache did not have it,
        // and the pane waits for the answer either way.
        assert_eq!(
            plan(&PreviewKind::Audio, false),
            Plan {
                full: Full::CoverArt,
                store: true,
                decoding: true,
            }
        );
        assert_eq!(
            plan(&PreviewKind::Audio, true),
            Plan {
                full: Full::CoverArt,
                store: false,
                decoding: true,
            },
            "a cached sleeve is the placeholder, and is not written twice"
        );

        // A picture decodes itself, exactly as before.
        assert_eq!(
            plan(&PreviewKind::Image, false),
            Plan {
                full: Full::File,
                store: true,
                decoding: true,
            }
        );

        // **A real video is untouched**: its frames are the player's, so the
        // cached thumbnail is all this worker fetches — and with none, nothing
        // is pending and the badge may say "video".
        assert_eq!(
            plan(&PreviewKind::Video, true),
            Plan {
                full: Full::Elsewhere,
                store: false,
                decoding: true,
            }
        );
        assert_eq!(
            plan(&PreviewKind::Video, false),
            Plan {
                full: Full::Elsewhere,
                store: false,
                decoding: false,
            }
        );

        // The documents are `doc`'s, whatever the cache holds.
        for kind in [PreviewKind::Pdf, PreviewKind::Font, PreviewKind::Model3d] {
            assert_eq!(plan(&kind, false).full, Full::Elsewhere, "{kind:?}");
            assert!(!plan(&kind, false).store, "{kind:?}");
        }
    }

    /// The generated fixtures (`build/test-assets.sh`), or `None` with a
    /// printed reason where there is no `ffmpeg` CLI to make them — the same
    /// gate dv-media's own integration tests use.
    fn media_fixtures() -> Option<PathBuf> {
        let ffmpeg = std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ffmpeg {
            eprintln!("SKIP: `ffmpeg` CLI not on PATH — cannot generate the media fixtures");
            return None;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2)?;
        let status = std::process::Command::new("bash")
            .arg(root.join("build/test-assets.sh"))
            .status()
            .ok()?;
        assert!(status.success(), "build/test-assets.sh failed");
        Some(root.join("build/assets"))
    }

    /// The sleeve end to end, through ffmpeg: the tagged song's 300×300 art
    /// comes out fitted to the pane like a photograph would; a song with no
    /// art, and a real video, come out with nothing — a video's first frame
    /// is not a sleeve.
    #[test]
    fn a_tagged_song_decodes_its_sleeve_to_the_panes_fit() {
        let Some(assets) = media_fixtures() else {
            return;
        };

        // A portrait pane: the width binds, so the square sleeve comes out
        // 200 wide and 200 tall — not capped at the long side and left 300.
        let art = decode_cover(&assets.join("cover.mp3"), (200, 400))
            .expect("decode the sleeve")
            .expect("cover.mp3 has a sleeve");
        assert_eq!((art.width, art.height), (200, 200));
        assert_eq!(art.pixels.len(), 200 * 200 * 4, "tightly packed RGBA");
        // The fixture's sleeve is flat orange, and it survived the trip.
        let centre = ((100 * 200 + 100) * 4) as usize;
        let [r, g, b, a] = [
            art.pixels[centre],
            art.pixels[centre + 1],
            art.pixels[centre + 2],
            art.pixels[centre + 3],
        ];
        assert!(
            r > 200 && (100..200).contains(&g) && b < 60 && a == 255,
            "{r} {g} {b} {a}"
        );

        // A pane bigger than the sleeve leaves it alone, like any still.
        let whole = decode_cover(&assets.join("cover.mp3"), (1000, 800))
            .expect("decode")
            .expect("sleeve");
        assert_eq!((whole.width, whole.height), (300, 300));

        // No art: an honest nothing, not an error.
        assert!(decode_cover(&assets.join("tone.m4a"), (200, 200))
            .expect("tone.m4a opens")
            .is_none());
        assert!(
            decode_cover(&assets.join("basic.mp4"), (200, 200))
                .expect("basic.mp4 opens")
                .is_none(),
            "a video's first frame is not cover art"
        );
    }

    /// Every browser's reading of a delay that says "as fast as you can".
    #[test]
    fn a_delay_of_nothing_is_a_tenth_of_a_second() {
        assert_eq!(frame_delay_ms(0), ANIM_ZERO_DELAY_MS);
        assert_eq!(frame_delay_ms(10), ANIM_ZERO_DELAY_MS);
        assert_eq!(frame_delay_ms(20), 20, "a real delay is left alone");
        assert_eq!(frame_delay_ms(1000), 1000);
    }

    #[test]
    fn fitting_never_enlarges() {
        // Smaller than the pane: left exactly as it is. The *pane* does draw a
        // 16×16 icon at 128 points now (`paint::fit_rect`, up to its
        // magnification cap); what it must never do is spend that much memory
        // on 16 points of picture, so the enlargement is a rectangle and never
        // a resample.
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
