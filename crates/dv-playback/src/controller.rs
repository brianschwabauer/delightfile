//! The playback controller thread (§3, §4.3, §4.4).
//!
//! Owns the playback clock and the active file's video decoder; delivers
//! frames to the UI as wgpu textures uploaded **on this thread** (§3 — only
//! texture handles cross to the main thread). Seek requests coalesce
//! latest-wins while scrubbing; playback presents the frame covering the
//! clock and drops late frames. The audio render thread (audio.rs) is kept in
//! sync by command; while audio is audible it is the master clock (§4.4).

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use dv_media::{ColorMatrix, DecodePath, HwDevice, KeyframeIndex, Nv12Frame, VideoDecoder};
use parking_lot::Mutex;

use crate::audio::{AudioCmd, AudioSys};

/// A decoded frame, uploaded and ready to draw: Y (R8) + UV (Rg8) textures
/// (§4.1's two-plane path). Only handles cross threads.
pub struct VideoFrameTex {
    pub pts_us: i64,
    pub width: u32,
    pub height: u32,
    pub y: wgpu::TextureView,
    pub uv: wgpu::TextureView,
    pub matrix: ColorMatrix,
    pub limited_range: bool,
    /// Bumps on every source change — stale frames are never drawn over a
    /// new source (§4.3 thumbnail-first placeholder decides with this).
    pub source_serial: u64,
}

/// What to play: one media file (M2; M3 feeds timeline segments through the
/// same controller).
#[derive(Clone)]
pub struct SourceSpec {
    pub path: PathBuf,
    /// Cached `index.bin` for deterministic keyframe seeks (§4.3), if built.
    pub keyframe_index: Option<PathBuf>,
    /// All-intra scrub proxy (§4.3). When present, VIDEO decode uses it —
    /// every frame is a keyframe, so any seek costs one frame decode. Audio
    /// always decodes the original `path` (the proxy's AAC is a fallback,
    /// not the preferred source).
    pub proxy: Option<PathBuf>,
    /// Cached `proxy_index.bin` for the proxy file.
    pub proxy_index: Option<PathBuf>,
    pub has_video: bool,
    pub has_audio: bool,
    /// Project fps for VFR pts snapping (§14); `None` = use the stream's rate.
    pub snap_fps: Option<f64>,
}

/// One clip's media reference within an edited timeline (§6.3). Plain data —
/// `dv-playback` never depends on `dv-core`; the app maps its project model
/// onto these before sending them (§3: workers receive immutable data).
#[derive(Clone, Debug)]
pub struct SegmentSource {
    pub path: std::path::PathBuf,
    /// Cached `index.bin` for deterministic seeks (§4.3), if built.
    pub keyframe_index: Option<std::path::PathBuf>,
    /// All-intra scrub proxy (§4.3): video decode prefers it when present.
    /// Proxy timestamps match the original's (same content, CFR re-encode),
    /// so `source_offset_us` applies to both files unchanged.
    pub proxy: Option<std::path::PathBuf>,
    /// Cached `proxy_index.bin` for the proxy file.
    pub proxy_index: Option<std::path::PathBuf>,
    /// Source time at the segment's timeline start.
    pub source_offset_us: i64,
    /// Clip speed: source advances `speed`× per timeline second (1.0 in M3
    /// practice, §6.4).
    pub speed: f64,
}

impl SegmentSource {
    /// The file video decode should read + its keyframe index: the proxy when
    /// one exists (§4.3 "preview/scrub always uses the proxy"), else the
    /// original. The bool is "this is the proxy" (status bar, §4.1).
    fn video_file(&self) -> (&std::path::Path, Option<&std::path::Path>, bool) {
        match &self.proxy {
            Some(p) => (p.as_path(), self.proxy_index.as_deref(), true),
            None => (self.path.as_path(), self.keyframe_index.as_deref(), false),
        }
    }
}

/// One entry in an edited timeline playlist (§6.2/§6.3). Segments are sorted,
/// non-overlapping and gapless from 0 to `duration_us` — the app guarantees
/// this, so the controller never has to reconcile overlaps.
#[derive(Clone, Debug)]
pub struct Segment {
    pub tl_start_us: i64,
    pub tl_dur_us: i64,
    /// `None` = black (a V1 Gap, §6.3) — a synthetic black frame is published.
    pub video: Option<SegmentSource>,
    /// The visible clip's framing + video fades (§6.4), compiled alongside
    /// the segment. Export consumes it; the preview computes the same values
    /// live from the project via the identical `dv_core::framing` calls.
    pub fx: Option<SegmentFx>,
}

impl Segment {
    fn tl_end_us(&self) -> i64 {
        self.tl_start_us + self.tl_dur_us
    }
}

/// Per-segment video framing + fade ramps (§6.4). Fade ramps are described
/// against the *clip's* timeline span (a clip can be split across several
/// segments by V2 edges), so any segment can evaluate the multiply exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentFx {
    pub placement: dv_core::framing::Placement,
    pub clip_tl_start_us: i64,
    pub clip_tl_end_us: i64,
    /// Video fade from black / to black durations (§6.4); 0 = none.
    pub vfade_in_us: i64,
    pub vfade_out_us: i64,
    /// The clip's §15 color grade, identity when ungraded — compiled here so
    /// preview and export feed the frame shader the same uniforms.
    pub grade: dv_core::model::GradeParams,
}

impl SegmentFx {
    /// The §4.2 fade-to-black multiply at timeline time `t` (1.0 = no fade).
    pub fn fade_at(&self, t: i64) -> f32 {
        let mut f = 1.0f64;
        if self.vfade_in_us > 0 {
            let into = (t - self.clip_tl_start_us).max(0);
            if into < self.vfade_in_us {
                f = f.min(into as f64 / self.vfade_in_us as f64);
            }
        }
        if self.vfade_out_us > 0 {
            let left = (self.clip_tl_end_us - t).max(0);
            if left < self.vfade_out_us {
                f = f.min(left as f64 / self.vfade_out_us as f64);
            }
        }
        f.clamp(0.0, 1.0) as f32
    }
}

enum Cmd {
    SetSource {
        spec: Box<SourceSpec>,
        start_us: i64,
        play: bool,
        serial: u64,
    },
    SetTimeline {
        segments: Vec<Segment>,
        mix: Arc<crate::mix::MixSpec>,
        duration: i64,
        snap_fps: Option<f64>,
        start_us: i64,
        play: bool,
        serial: u64,
    },
    ClearSource,
    Seek {
        us: i64,
    },
    Play,
    Pause,
    /// Play if paused, pause if playing — resolved *here*, on the thread that
    /// owns `playing`. See [`Playback::toggle_play`].
    TogglePlay,
    /// Signed rate for JKL shuttle; 0 pauses.
    SetRate(f64),
    Step {
        frames: i64,
    },
    Shutdown,
}

// ---------------------------------------------------------------------------
// Pure timeline math (unit-tested below). No I/O, no threads — the correctness
// of the threaded parts leans on these being right (§ verification).
// ---------------------------------------------------------------------------

/// Prefetch lead: prepare an upcoming cut once the playhead is within this of
/// it (§4.4: "<1 s from a cut").
const PREFETCH_LEAD_US: i64 = 1_000_000;
/// Two segments sharing a path are "continuous" (no prefetch, the running
/// decoder just keeps going) when the source position lines up within this.
const CONTINUITY_US: i64 = 100_000;

/// Source time for a timeline instant inside a segment:
/// `src = source_offset + (tl - tl_start) * speed` (§ required mapping).
fn src_from_tl(source_offset_us: i64, tl_start_us: i64, speed: f64, tl_us: i64) -> i64 {
    source_offset_us + ((tl_us - tl_start_us) as f64 * speed) as i64
}

/// Inverse of [`src_from_tl`]: `tl = tl_start + (src - source_offset) / speed`.
fn tl_from_src(source_offset_us: i64, tl_start_us: i64, speed: f64, src_us: i64) -> i64 {
    tl_start_us + ((src_us - source_offset_us) as f64 / speed) as i64
}

/// Index of the segment covering `tl_us` (`tl_start ≤ tl < tl_start+dur`).
/// Binary search — the playlist is sorted and gapless. `None` past the end.
fn segment_at(segments: &[Segment], tl_us: i64) -> Option<usize> {
    if segments.is_empty() {
        return None;
    }
    // Partition point on tl_start: last segment whose start ≤ tl_us.
    let i = segments.partition_point(|s| s.tl_start_us <= tl_us);
    if i == 0 {
        return None; // before the first segment's start
    }
    let idx = i - 1;
    if tl_us < segments[idx].tl_end_us() {
        Some(idx)
    } else {
        None
    }
}

/// The next segment worth **prefetching** while playing forward (§4.4): within
/// [`PREFETCH_LEAD_US`] of the cut and the successor's video is a *different*
/// file — or the same file at a discontinuous position. `None` otherwise
/// (paused, reverse, no successor, gap successor, or a continuous same-file
/// cut the running decoder handles by itself).
fn prefetch_target(
    segments: &[Segment],
    cur_seg: usize,
    pos_tl: i64,
    playing: bool,
    rate: f64,
) -> Option<usize> {
    if !playing || rate <= 0.0 {
        return None;
    }
    let cur = segments.get(cur_seg)?;
    let next_idx = cur_seg + 1;
    let next = segments.get(next_idx)?;
    let cut_at = cur.tl_end_us();
    let lead = cut_at - pos_tl;
    if !(0..PREFETCH_LEAD_US).contains(&lead) {
        return None;
    }
    let nvid = next.video.as_ref()?;
    // A same-file, positionally-continuous cut needs no prefetch: the decoder
    // already under the playhead simply keeps decoding across it.
    if let Some(cvid) = cur.video.as_ref() {
        if cvid.path == nvid.path {
            let src_at_cut =
                src_from_tl(cvid.source_offset_us, cur.tl_start_us, cvid.speed, cut_at);
            if (src_at_cut - nvid.source_offset_us).abs() < CONTINUITY_US {
                return None;
            }
        }
    }
    Some(next_idx)
}

/// How far the playhead may sit *behind* the frame on screen without that
/// counting as a backward jump, in the units `cur_pts` is kept in.
///
/// The clock corrects itself by a millisecond or two as it runs — it is the
/// audio clock while audio is audible, and it is read on a different thread
/// from the one that advances it — and answering one of those corrections with
/// a seek throws the decoder back to the preceding keyframe to re-decode the
/// whole GOP. Anything within a frame is showing the right frame anyway.
///
/// Only forward playback gets the tolerance. Reverse playback (§4.4) and an
/// explicit seek mean the backward move is to be *obeyed*, however small, so
/// they get zero.
fn jitter_tolerance(playing: bool, rate: f64, want: Option<i64>, frame_dur: i64) -> i64 {
    if playing && rate > 0.0 && want.is_none() {
        frame_dur
    } else {
        0
    }
}

/// Does the frame published at `cur_pts` still cover `target`?
///
/// This is what makes playback frame-paced rather than tick-paced. The
/// controller wakes every 3 ms while playing — about five times per frame
/// interval at 60 fps — and without this every one of those wakes decoded and
/// published a *new* frame, running `cur_pts` ahead of the clock until a
/// backward correction was read as a seek and sent the decoder back to the
/// keyframe. Measured on a 1080p60 clip before the guard existed: 1950 frames
/// decoded and 64 backward seeks to put 299 frames on screen.
///
/// `cur_pts == i64::MIN` means "nothing decoded yet" and never covers.
fn frame_covers(cur_pts: i64, target: i64, frame_dur: i64, jitter: i64) -> bool {
    cur_pts != i64::MIN && target >= cur_pts.saturating_sub(jitter) && target < cur_pts + frame_dur
}

/// Clamp a seek target to the media.
///
/// `duration <= 0` means *unknown*, not a zero-length source: some containers
/// report no duration in the header and no stream carries one either, and the
/// decoder is opened all the same (see the `duration == 0` fallbacks on load).
/// Clamping to it would pin every seek to 0 and leave the transport dead on
/// files that otherwise play fine, so the requested target passes through and
/// the decoder stops at EOF on its own.
fn clamp_seek(us: i64, duration: i64) -> i64 {
    if duration > 0 {
        us.clamp(0, duration)
    } else {
        us.max(0)
    }
}

/// How much decoded video a reverse shuttle may hold, in bytes.
///
/// The whole trick of playing backwards is that a codec only decodes forwards:
/// you buy a run of real frames with one forward pass and then hand them out in
/// reverse. This is the size of that purchase. 48 MB is 15 frames of 1080p NV12
/// (3.1 MB each) or 34 of 720p — half a second and better than a second of
/// footage at 30 fps.
///
/// **Being short of a whole GOP costs much less than it looks like it should**,
/// which is why the number does not need to be large. Eviction is from the
/// *front* (the oldest frames), and the refill that goes back for them stops at
/// the playhead — so a 30-frame GOP with room for 20 costs 30 + 10 frames of
/// decode for 30 shown, not 60.
const REVERSE_BUDGET_BYTES: usize = 48 << 20;

/// Fewer cached frames than this and the source is too big for the run to be
/// worth buying: the refills overlap so heavily that the decode never keeps up,
/// and a smooth 4 fps is worse than an honest keyframe step. 4K60 lands here.
const REVERSE_MIN_FRAMES: usize = 6;

/// The fastest reverse rate that still decodes every frame.
///
/// The cost of the smooth path is one GOP decode per GOP crossed, so it scales
/// with the rate: at −4× on a 1 s GOP that is four GOPs a second, about 120
/// frames of decode. Past that it is a *shuttle*, where chunky is the expected
/// and wanted behaviour — nobody watching at −16× is looking for motion, they
/// are looking for a place to stop.
const SMOOTH_REVERSE_MAX: f64 = 4.0;

/// **A run of real frames, held so they can be handed out backwards** (§4.4,
/// revised 2026-08-09).
///
/// Frames are in increasing pts. Reverse playback consumes the **back** — the
/// end nearest the playhead — and the budget evicts the **front**, which is the
/// end the next refill can re-decode most cheaply.
#[derive(Default)]
struct ReverseCache {
    frames: std::collections::VecDeque<Nv12Frame>,
    bytes: usize,
    /// Set once when a source turns out to be too big to buy a useful run of
    /// (see [`REVERSE_MIN_FRAMES`]), so the wasted pass is paid for once rather
    /// than on every tick. Cleared with the cache.
    give_up: bool,
    /// Did the budget throw anything away while this run was being bought? A
    /// short run is only evidence of a *big frame* if it was cut short — a run
    /// that ends two frames after its keyframe is simply where the playhead is.
    evicted: bool,
}

impl ReverseCache {
    fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
        self.give_up = false;
        self.evicted = false;
    }

    /// Start a fresh run, keeping [`give_up`](Self::give_up) — which is a fact
    /// about the *source* and survives every refill of the run.
    fn restart(&mut self) {
        self.frames.clear();
        self.bytes = 0;
        self.evicted = false;
    }

    fn push(&mut self, frame: Nv12Frame) {
        self.bytes += frame_bytes(&frame);
        self.frames.push_back(frame);
        // Never down to nothing: one frame is still an answer, and a budget
        // smaller than a single frame would otherwise loop forever.
        while self.bytes > REVERSE_BUDGET_BYTES && self.frames.len() > 1 {
            if let Some(f) = self.frames.pop_front() {
                self.bytes -= frame_bytes(&f);
                self.evicted = true;
            }
        }
    }

    /// The newest cached frame at or before `target`, dropping everything after
    /// it — those are in the playhead's future and reverse will never want them
    /// again.
    ///
    /// `None` when the cache cannot answer: it is empty, everything in it is
    /// ahead of the playhead, or the nearest frame is further behind than a
    /// couple of frame intervals — which is how a *seek* mid-reverse throws the
    /// run away instead of showing a frame from somewhere else entirely.
    fn frame_at(&mut self, target: i64, frame_dur: i64) -> Option<&Nv12Frame> {
        while self.frames.back().is_some_and(|f| f.pts_us > target) {
            if let Some(f) = self.frames.pop_back() {
                self.bytes -= frame_bytes(&f);
            }
        }
        let stale = self
            .frames
            .back()
            .is_some_and(|f| target - f.pts_us >= frame_dur.max(1) * 2);
        if stale {
            self.frames.clear();
            self.bytes = 0;
        }
        self.frames.back()
    }
}

fn frame_bytes(f: &Nv12Frame) -> usize {
    f.y.len() + f.uv.len()
}

/// **One tick of real reverse playback** (§4.4, revised 2026-08-09).
///
/// Puts the newest decoded frame at or before `target` on screen, buying
/// another run backwards ([`ReverseCache`]) when the last one is used up. The
/// run is one forward pass from the keyframe before the playhead, stopping *at*
/// the playhead — which is also what makes the re-decode of an evicted front
/// cheap, since that pass stops earlier every time.
///
/// **Backwards always means backwards.** The frame handed out is always the one
/// covering `target`, and `target` only ever falls while reverse is running, so
/// the picture can repeat a frame (the clock has not reached the next one yet)
/// but can never advance. The keyframe stepping this replaces was the opposite
/// bargain: correct positions, and a picture that ran forwards through a GOP
/// between them.
///
/// Returns false when it cannot answer — no decoder, the head of the file, a
/// decode failure, or a source whose frames are too big for a run to be worth
/// buying — and the caller falls through to the keyframe step, which is still
/// monotone (see [`reverse_holds`]).
fn service_reverse(
    a: &mut Active,
    shared: &Shared,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: i64,
) -> bool {
    if a.rev.frame_at(target, a.frame_dur).is_none() {
        // Deterministic landing through the index where there is one, exactly
        // as the forward seek does; without one, the demuxer's own backward
        // seek lands on the keyframe at or before the target, which is the
        // same place.
        let kf = a
            .index
            .as_ref()
            .and_then(|ix| ix.keyframe_before(target))
            .and_then(|e| e.pts_us);
        let seek_to = kf.unwrap_or(target).min(target);
        let snap_fps = a.spec.snap_fps;
        a.rev.restart();
        let Some(dec) = a.dec.as_mut() else {
            return false;
        };
        if dec.seek(seek_to).is_err() {
            return false;
        }
        loop {
            match dec.next_frame() {
                Ok(Some(mut frame)) => {
                    // §14: VFR sources snap to the project frame grid, here as
                    // everywhere else — the cache is keyed by the same pts the
                    // clock is compared against.
                    frame.pts_us = snap_pts(frame.pts_us, snap_fps);
                    if frame.pts_us > target {
                        break;
                    }
                    a.rev.push(frame);
                }
                Ok(None) => break,
                Err(e) => {
                    log::warn!("reverse decode error: {e}");
                    break;
                }
            }
        }
        // The decoder is parked past the run; `cur_pts` is the published frame
        // rather than the decoder's position, and forward playback seeks before
        // it decodes, so nothing downstream is left holding a stale idea of
        // where the file is.
        a.last_kf = kf;
        a.dec_lost = true;
        // Too big to be worth it — but only when the budget is what cut the run
        // short. Decided once per source (see `give_up`).
        if a.rev.evicted && a.rev.frames.len() < REVERSE_MIN_FRAMES {
            log::info!(
                "reverse: {} bytes a frame is too big to buy a run of — stepping keyframes",
                a.rev.bytes / a.rev.frames.len().max(1)
            );
            a.rev.give_up = true;
            return false;
        }
    }
    let serial = a.serial;
    let frame_dur = a.frame_dur;
    let Some(frame) = a.rev.frame_at(target, frame_dur) else {
        return false;
    };
    let pts = frame.pts_us;
    if pts != a.cur_pts {
        publish(shared, device, queue, frame, pts, serial);
        a.cur_pts = pts;
    }
    true
}

/// **Does reverse playback already have the right frame on screen?**
///
/// Reverse shows keyframes only (§4.4), and a backward seek lands on the head
/// of a GOP — up to a second *before* the playhead that asked for it. That
/// frame is the answer for everything from its own pts up to that playhead, so
/// there is nothing to do until the playhead moves *before* it.
///
/// Without this, a reverse shuttle ran **forwards** between its jumps and the
/// whole gesture read as a stutter rather than as playback: `frame_covers`
/// fails (the playhead is a whole GOP past the keyframe), `behind` is false
/// (the playhead is *ahead* of it), and the decode loop walks forward frame by
/// frame to catch up. Every GOP was a jump back followed by a run forward
/// through the same second of footage, at exactly the rate that makes it look
/// deliberate. Traced on a 12 s clip with 1 s GOPs: 550 frames published for
/// four seconds of reverse, against six with the guard in.
///
/// `last_kf` was meant to be this and is not: it only fires with a keyframe
/// index built, and only inside the seek branch, which is the one path the
/// forward run never takes.
fn reverse_holds(keyframe_only: bool, cur_pts: i64, target: i64) -> bool {
    keyframe_only && cur_pts != i64::MIN && target >= cur_pts
}

/// A/V error big enough to be a desync rather than drift: the clock lands on
/// the audio position outright instead of slewing to it.
const RESYNC_SNAP_US: i64 = 250_000;
/// The most one 100 ms drift correction may move the clock — kept well under a
/// frame, so a correction is never visible as a skipped or repeated frame.
const RESYNC_SLEW_US: i64 = 2_000;

/// Wall clock with rate; audio re-anchors it while audible (§4.4).
struct Clock {
    anchor_us: i64,
    anchor_at: Instant,
    rate: f64,
    playing: bool,
    /// The playhead has been moved discontinuously — a seek, a step, a
    /// play/pause, a rate change, a new source — and the audio thread has not
    /// necessarily processed the `Sync` that went with it. Until it has, the
    /// audio clock still reports where the playhead *used to be*, and is not
    /// evidence about anything. Set by [`Clock::set`], which every such move
    /// goes through; cleared by the first drift check that sees it.
    moved: bool,
}

impl Clock {
    fn position(&self) -> i64 {
        if self.playing {
            self.anchor_us + (self.anchor_at.elapsed().as_secs_f64() * self.rate * 1e6) as i64
        } else {
            self.anchor_us
        }
    }

    /// Move the playhead. Everything that relocates it comes through here,
    /// which is what makes `moved` impossible to forget to set.
    fn set(&mut self, us: i64) {
        self.anchor_us = us;
        self.anchor_at = Instant::now();
        self.moved = true;
    }

    /// Re-anchor *without* calling it a move — the drift correction's own
    /// path, which tracks the playhead rather than relocating it.
    fn drift(&mut self, us: i64) {
        self.anchor_us = us;
        self.anchor_at = Instant::now();
    }
}

struct Shared {
    clock: Mutex<Clock>,
    frame: Mutex<Option<Arc<VideoFrameTex>>>,
    duration_us: AtomicI64,
    /// Active decode path label index: 0 none, 1 vaapi, 2 nvdec, 3 sw.
    decode_path: AtomicI64,
    /// 1 while the current video decoder reads a proxy file (§4.3) — the
    /// status bar shows it next to the decode path.
    proxy_active: AtomicI64,
    /// µs from the last explicit seek/step request to its frame being
    /// published — the controller's share of §7.4's input-to-photon budget.
    /// −1 before any measured seek.
    seek_latency_us: AtomicI64,
}

/// Handle owned by the app. All methods are non-blocking.
pub struct Playback {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    audio: AudioSys,
    join: Option<std::thread::JoinHandle<()>>,
    source_serial: u64,
    /// True while the controller is in timeline mode — a fresh `source_serial`
    /// is minted only on *entering* it, so wholesale playlist re-sends after
    /// every edit keep the same serial (the published frame stays valid).
    in_timeline: bool,
}

impl Playback {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Playback {
        let shared = Arc::new(Shared {
            clock: Mutex::new(Clock {
                anchor_us: 0,
                anchor_at: Instant::now(),
                rate: 1.0,
                playing: false,
                moved: false,
            }),
            frame: Mutex::new(None),
            duration_us: AtomicI64::new(0),
            decode_path: AtomicI64::new(0),
            proxy_active: AtomicI64::new(0),
            seek_latency_us: AtomicI64::new(-1),
        });
        let audio = AudioSys::start();
        let (tx, rx) = crossbeam_channel::unbounded();
        let join = std::thread::Builder::new()
            .name("dv-playback".into())
            .spawn({
                let shared = shared.clone();
                let ring = audio.ring.clone();
                let aclock = audio.clock.clone();
                let atx = audio.tx.clone();
                move || controller_thread(rx, shared, device, queue, ring, aclock, atx)
            })
            .ok();
        Playback {
            tx,
            shared,
            audio,
            join,
            source_serial: 0,
            in_timeline: false,
        }
    }

    /// Switch to a media file, positioned at `start_us` (§7.1 media preview).
    pub fn set_source(&mut self, spec: SourceSpec, start_us: i64, play: bool) {
        self.source_serial += 1;
        self.in_timeline = false;
        let _ = self.tx.send(Cmd::SetSource {
            spec: Box::new(spec),
            start_us,
            play,
            serial: self.source_serial,
        });
    }

    /// Switch to timeline mode: play `segments` (sorted, non-overlapping,
    /// gapless from 0 to `duration_us` — the app guarantees this) with the
    /// playhead at `start_us`. Re-sent WHOLESALE after every edit; must not
    /// visibly hiccup when the underlying files/positions didn't change
    /// (the decoder cache + no-flash logic in the controller handle that).
    pub fn set_timeline(
        &mut self,
        segments: Vec<Segment>,
        mix: Arc<crate::mix::MixSpec>,
        duration_us: i64,
        snap_fps: Option<f64>,
        start_us: i64,
        play: bool,
    ) {
        if !self.in_timeline {
            self.source_serial += 1;
            self.in_timeline = true;
        }
        let _ = self.tx.send(Cmd::SetTimeline {
            segments,
            mix,
            duration: duration_us,
            snap_fps,
            start_us,
            play,
            serial: self.source_serial,
        });
    }

    /// Live audio meters (§5): LUFS, peaks, per-clip GR while the mix plays.
    pub fn mix_meters(&self) -> Arc<crate::mix::MixMeters> {
        self.audio.meters.clone()
    }

    pub fn clear_source(&mut self) {
        self.source_serial += 1;
        self.in_timeline = false;
        let _ = self.tx.send(Cmd::ClearSource);
    }

    /// Monotonic per set_source; matches [`VideoFrameTex::source_serial`].
    pub fn source_serial(&self) -> u64 {
        self.source_serial
    }

    /// Seek (scrub): coalesced latest-wins in the controller (§4.3).
    pub fn seek(&self, us: i64) {
        let _ = self.tx.send(Cmd::Seek { us: us.max(0) });
    }

    pub fn play(&self) {
        let _ = self.tx.send(Cmd::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }

    /// Toggle play/pause.
    ///
    /// Use this rather than `if is_playing() { pause() } else { play() }`. A
    /// toggle is a read-modify-write of `playing`, which this thread does not
    /// own: `is_playing` reports what the controller last *processed*, not what
    /// it has been asked to do. While the controller is busy — opening a file
    /// costs a hardware-decoder probe and a trial decode, hundreds of
    /// milliseconds — a caller deciding for itself reads the pre-command state
    /// and sends the same command twice. Pressing play then pause during that
    /// window sent two Plays and never paused.
    pub fn toggle_play(&self) {
        let _ = self.tx.send(Cmd::TogglePlay);
    }

    pub fn set_rate(&self, rate: f64) {
        let _ = self.tx.send(Cmd::SetRate(rate));
    }

    pub fn step(&self, frames: i64) {
        let _ = self.tx.send(Cmd::Step { frames });
    }

    /// Current playback position (µs into the source).
    pub fn position_us(&self) -> i64 {
        self.shared.clock.lock().position().max(0)
    }

    /// Mute/unmute the audio output (§12 live media peek). Playback and the
    /// audio clock keep running; only the emitted samples go silent.
    pub fn set_muted(&self, muted: bool) {
        self.audio
            .muted
            .store(muted, std::sync::atomic::Ordering::Relaxed);
    }

    /// Output gain (§7.3's Shift+↑/↓ volume). Independent of the mute:
    /// unmuting restores whatever level was set.
    ///
    /// Unity is 1.0 and the ceiling is [`MAX_VOLUME`], not 1.0: a quiet source
    /// played at a sensible system level used to have nowhere to go, and "turn
    /// it up past what the file has" is a thing every media player can do.
    /// Anything above unity goes through the mixer's soft knee (`audio.rs`),
    /// so a boost bends rather than squares off against the rail.
    pub fn set_volume(&self, volume: f32) {
        self.audio.volume.store(
            volume.clamp(0.0, crate::audio::MAX_VOLUME).to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.audio.volume.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.clock.lock().playing
    }

    pub fn rate(&self) -> f64 {
        self.shared.clock.lock().rate
    }

    pub fn duration_us(&self) -> i64 {
        self.shared.duration_us.load(Ordering::Relaxed)
    }

    /// Latest uploaded frame (cheap Arc clone).
    pub fn current_frame(&self) -> Option<Arc<VideoFrameTex>> {
        self.shared.frame.lock().clone()
    }

    /// Status-bar decode path label (§4.1), `None` before any source opened.
    pub fn decode_path(&self) -> Option<&'static str> {
        match self.shared.decode_path.load(Ordering::Relaxed) {
            1 => Some("vaapi"),
            2 => Some("nvdec"),
            3 => Some("sw"),
            _ => None,
        }
    }

    /// True while video frames are being decoded from a proxy file (§4.3).
    pub fn decoding_proxy(&self) -> bool {
        self.shared.proxy_active.load(Ordering::Relaxed) == 1
    }

    /// µs from the most recent seek/step to its frame publish (§7.4 latency
    /// readout); `None` before any measured seek. The UI adds one present on
    /// top of this to get input-to-photon.
    pub fn last_seek_latency_us(&self) -> Option<i64> {
        let v = self.shared.seek_latency_us.load(Ordering::Relaxed);
        (v >= 0).then_some(v)
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.audio.shutdown();
    }
}

struct Active {
    spec: SourceSpec,
    dec: Option<VideoDecoder>,
    /// The file `dec` actually reads — the proxy when one opened, else
    /// `spec.path`. It is the [`VidCache`] key this decoder is parked under when
    /// the source closes, and keying it off `spec.path` would hand a later
    /// proxy-less `SetSource` a decoder for the wrong file.
    dec_path: Option<PathBuf>,
    index: Option<KeyframeIndex>,
    /// pts of the most recently published frame.
    cur_pts: i64,
    frame_dur: i64,
    duration: i64,
    serial: u64,
    /// Pending exact-seek target (scrub coalescing, §4.3).
    want: Option<i64>,
    /// Keyframe currently shown during reverse playback (avoid re-seeking).
    last_kf: Option<i64>,
    eof: bool,
    /// Real frames bought for a reverse shuttle, handed out backwards
    /// ([`ReverseCache`]). Empty whenever playback is not running in reverse.
    rev: ReverseCache,
    /// A live stream (`dv_media::is_stream_url`): no timeline, no seeking —
    /// [`service_live`] publishes frames as the camera sends them, and a stall
    /// is a reconnect rather than an EOF.
    live: bool,
    /// Earliest moment [`service_live`] may try to reopen a dead stream, so a
    /// down camera costs one blocked open every couple of seconds, not one per
    /// service pass.
    reconnect_at: Option<Instant>,
    /// Last pass's `playing`, for live sources only: the pause→play edge
    /// forces a reconnect, because "resume" on a monitor means "rejoin the
    /// present", not "replay what the socket buffered while paused".
    live_was_playing: bool,
    /// Live pacing: the mapping from stream pts to wall clock — `(pts, wall)`
    /// of the anchor frame, cushioned by [`LIVE_CUSHION`]. RTSP delivers in
    /// bursts even when throughput is perfect, and publishing on *arrival*
    /// puts every burst on screen as judder; frames are held until their pts
    /// comes due against this anchor instead, which is how every player paces
    /// a stream.
    live_anchor: Option<(i64, Instant)>,
    /// A live frame decoded ahead of its display time, waiting for it.
    live_hold: Option<Nv12Frame>,
    /// The pacing cushion in force. Starts at [`LIVE_CUSHION`] and grows by a
    /// step on every stale re-anchor, because a re-anchor means the camera's
    /// delivery bursts are wider than the cushion — the stutter the user just
    /// saw is the measurement. Capped so a doorbell never lags by more than
    /// [`LIVE_CUSHION_MAX`].
    live_cushion: Duration,
    /// Live delivery telemetry, logged every few seconds at debug level:
    /// (window start, published, late, re-anchors, worst lateness).
    live_stats: (Instant, u32, u32, u32, Duration),
    /// Where the time goes: (next_frame calls, total decode time, total
    /// publish time) in the same window.
    live_times: (u32, Duration, Duration),
    /// **Is the decoder somewhere other than just after `cur_pts`?**
    ///
    /// The forward path decides whether to seek from `cur_pts` alone, which
    /// works because the frame it published is the last one it decoded. A
    /// reverse run breaks that: it leaves the decoder parked past the end of
    /// the run while `cur_pts` is a frame from the middle of it. Without this
    /// flag the settle after reverse stops — `want = Some(0)` at the head of
    /// the file — decoded forward from wherever the run left off and published
    /// a frame a second *later* than the one on screen.
    dec_lost: bool,
}

/// A ready video decoder for one source path — the unit of both the LRU cache
/// and the current/prefetch slots. All positions are **source** time.
struct VidSlot {
    path: PathBuf,
    dec: VideoDecoder,
    index: Option<KeyframeIndex>,
    frame_dur: i64,
    /// Source pts of the most recently decoded frame (`i64::MIN` = unknown).
    cur_pts: i64,
    /// Keyframe currently shown during reverse playback (avoid re-seeking).
    last_kf: Option<i64>,
    /// A frame decoded during prefetch but not yet published — published on
    /// adoption so the exact first frame of the cut appears (§4.4 seamless).
    warm: Option<Nv12Frame>,
}

/// Small LRU cache of open decoders keyed by path. It SURVIVES `SetTimeline`
/// so re-sending the playlist after every edit never reopens files (§ req. 3).
struct VidCache {
    slots: Vec<VidSlot>, // front = most-recently-used
    cap: usize,
}

impl VidCache {
    fn new(cap: usize) -> VidCache {
        VidCache {
            slots: Vec::new(),
            cap,
        }
    }

    /// Remove and return the decoder for `path`, if cached (checks it out).
    fn take(&mut self, path: &std::path::Path) -> Option<VidSlot> {
        let i = self.slots.iter().position(|s| s.path == path)?;
        log::debug!("decoder cache hit {}", path.display());
        Some(self.slots.remove(i))
    }

    /// Return a decoder to the cache as most-recently-used, evicting the LRU
    /// tail beyond the cap. De-dups by path (keeps the returned one).
    fn put(&mut self, slot: VidSlot) {
        self.slots.retain(|s| s.path != slot.path);
        self.slots.insert(0, slot);
        self.slots.truncate(self.cap);
    }

    /// Park a **single source's** decoder here rather than dropping it.
    ///
    /// Closing a source used to drop it outright, so arrowing off a clip in the
    /// viewer and back re-paid the ffmpeg open, the hardware trial decode and
    /// the probe — for a file that had been open a keystroke earlier. Timeline
    /// mode has always returned its decoders to this cache; a single source is
    /// parked in the same one, on the same key, under the same cap.
    ///
    /// **The position is not retained.** A parked slot is adopted with a pending
    /// exact seek (`Active.want`, set by every `SetSource`), which is precisely
    /// what a fresh open is adopted with — so `cur_pts` starts unknown and the
    /// service pass seeks, exactly as it would have.
    fn park(
        &mut self,
        path: PathBuf,
        dec: VideoDecoder,
        index: Option<KeyframeIndex>,
        frame_dur: i64,
    ) {
        self.put(VidSlot {
            path,
            dec,
            index,
            frame_dur,
            cur_pts: i64::MIN,
            last_kf: None,
            warm: None,
        });
    }
}

/// Retire the active source, parking its video decoder for the next
/// [`Cmd::SetSource`] that names the same file ([`VidCache::park`]).
///
/// The audio decoder is not parked with it: it lives on the render thread behind
/// `AudioCmd::SetSource(None)` and has no slot here to go in, and its open is
/// the cheap half of the pair.
fn retire_active(active: &mut Option<Active>, cache: &mut VidCache) {
    let Some(mut a) = active.take() else { return };
    let (Some(dec), Some(path)) = (a.dec.take(), a.dec_path.take()) else {
        return;
    };
    // A live decoder is a network session: parking it would keep the camera
    // serving a stream nobody is watching until the LRU got around to it.
    // Dropping closes the connection now, and a re-open is what "back to the
    // live edge" means anyway.
    if a.live {
        return;
    }
    cache.park(path, dec, a.index.take(), a.frame_dur);
}

/// Open (or check out of the cache) the decoder for a source path, loading its
/// keyframe index on a fresh open. `None` on open failure (treated as black).
fn acquire_slot(
    cache: &mut VidCache,
    hw: &[HwDevice],
    path: &std::path::Path,
    index_path: Option<&std::path::Path>,
) -> Option<VidSlot> {
    if let Some(slot) = cache.take(path) {
        return Some(slot);
    }
    match VideoDecoder::open_with(path, hw) {
        Ok(dec) => {
            let frame_dur = dec.frame_duration_us();
            let index = index_path.and_then(|p| KeyframeIndex::load(p).ok());
            Some(VidSlot {
                path: path.to_path_buf(),
                dec,
                index,
                frame_dur,
                cur_pts: i64::MIN,
                last_kf: None,
                warm: None,
            })
        }
        Err(e) => {
            log::warn!("open {}: {e}", path.display());
            None
        }
    }
}

/// The file a segment's video should decode from, honoring the bad-proxy set:
/// `(file, keyframe index, is_proxy)`. Proxies that already failed to open are
/// skipped without another attempt.
fn choose_video_file(
    vsrc: &SegmentSource,
    bad_proxies: &std::collections::HashSet<PathBuf>,
) -> (PathBuf, Option<PathBuf>, bool) {
    let (file, index, is_proxy) = vsrc.video_file();
    if is_proxy && !bad_proxies.contains(file) {
        (file.to_path_buf(), index.map(|p| p.to_path_buf()), true)
    } else {
        (vsrc.path.clone(), vsrc.keyframe_index.clone(), false)
    }
}

/// Timeline playback state (§6.3). Replaced wholesale on each `SetTimeline`,
/// but the current/prefetch decoders and the published-frame identity migrate
/// across so unchanged positions never reopen or flash.
struct TimelineState {
    segments: Vec<Segment>,
    duration: i64,
    snap_fps: Option<f64>,
    serial: u64,
    /// Segment the current decoder is serving.
    cur_seg: usize,
    /// Decoder for `cur_seg`'s video (`None` over a gap or open failure).
    cur: Option<VidSlot>,
    /// Warmed decoder for an upcoming cut: `(segment index, slot)`.
    prefetch: Option<(usize, VidSlot)>,
    /// Pending exact-seek target in TIMELINE µs (scrub coalescing, §4.3).
    want: Option<i64>,
    /// Current segment's source hit EOF: hold the last frame (§ req. 5).
    seg_eof: bool,
    /// Path + source pts of the currently published frame (no-flash compare).
    pub_path: Option<PathBuf>,
    pub_src: i64,
    /// A synthetic black frame is currently published (gap, §6.3).
    is_black: bool,
}

impl TimelineState {
    /// Frame duration used for `Step`: the current decoder's cadence, else the
    /// project fps, else 30 fps.
    fn step_frame_dur(&self) -> i64 {
        self.cur
            .as_ref()
            .map(|s| s.frame_dur)
            .or_else(|| self.snap_fps.map(|f| (1e6 / f) as i64))
            .unwrap_or(33_333)
            .max(1)
    }
}

#[allow(clippy::too_many_arguments)]
fn controller_thread(
    rx: Receiver<Cmd>,
    shared: Arc<Shared>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    ring: Arc<crate::ring::AudioRing>,
    aclock: Arc<crate::audio::AudioClock>,
    atx: Sender<AudioCmd>,
) {
    // §4.1: probe once; every decoder tries it, falls back per file.
    let hw = HwDevice::probe_all();
    let mut active: Option<Active> = None;
    let mut timeline: Option<TimelineState> = None;
    // Decoder cache shared across timelines — survives SetTimeline (§ req. 3).
    let mut dec_cache = VidCache::new(3);
    // Proxies that failed to open this session (§4.3 fallback): retried only
    // after ClearSource, never per-frame — a bad proxy must not cost decode
    // churn or log spam during playback.
    let mut bad_proxies: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut last_anchor_check = Instant::now();
    // Consecutive drift checks that have asked for an outright snap. A single
    // outlier reading of the audio clock never moves the playhead.
    let mut resync_snap_votes: u8 = 0;
    // Non-seek commands plucked out mid-catch-up, replayed next iteration.
    let mut deferred: Vec<Cmd> = Vec::new();
    // Birth time of the newest un-serviced seek/step (§7.4 latency readout).
    // Latest-wins like the seeks themselves: a scrub burst measures the final
    // request, which is the one whose responsiveness the user feels.
    let mut seek_born: Option<Instant> = None;

    let sync_audio = |shared: &Shared, atx: &Sender<AudioCmd>| {
        let c = shared.clock.lock();
        let _ = atx.send(AudioCmd::Sync {
            media_us: c.position().max(0),
            rate: c.rate,
            playing: c.playing,
        });
    };

    loop {
        // Wake cadence: busy while playing or a seek is pending; idle otherwise.
        let busy = {
            let want_pending = active.as_ref().map(|a| a.want.is_some()).unwrap_or(false)
                || timeline.as_ref().map(|t| t.want.is_some()).unwrap_or(false);
            let has_src = active.is_some() || timeline.is_some();
            has_src && (want_pending || shared.clock.lock().playing)
        };
        let cmd = if !deferred.is_empty() {
            Some(deferred.remove(0))
        } else if busy {
            match rx.recv_timeout(Duration::from_millis(3)) {
                Ok(c) => Some(c),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(_) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };

        // Drain all pending commands; seeks coalesce latest-wins (§4.3).
        let mut next = cmd;
        while let Some(cmd) = next.take() {
            // Resolve a toggle against the authoritative `playing` before
            // dispatch, so it becomes whichever of Play/Pause is actually
            // opposite to the current state — and gets that state's full
            // handling below rather than a second copy of it.
            let cmd = match cmd {
                Cmd::TogglePlay => {
                    if shared.clock.lock().playing {
                        Cmd::Pause
                    } else {
                        Cmd::Play
                    }
                }
                other => other,
            };
            match cmd {
                // Unreachable: resolved to Play or Pause immediately above.
                // The arm exists to keep the match total, and the assertion to
                // catch it if that resolution is ever moved or dropped.
                Cmd::TogglePlay => debug_assert!(false, "TogglePlay reached dispatch"),
                Cmd::Shutdown => return,
                Cmd::SetSource {
                    spec,
                    start_us,
                    play,
                    serial,
                } => {
                    // Leaving timeline mode: return its decoders to the cache.
                    if let Some(mut ts) = timeline.take() {
                        if let Some(s) = ts.cur.take() {
                            dec_cache.put(s);
                        }
                        if let Some((_, s)) = ts.prefetch.take() {
                            dec_cache.put(s);
                        }
                    }
                    // And the source being replaced, for the same reason: a
                    // `SetSource` straight onto another clip must not be the one
                    // path that throws a decoder away.
                    retire_active(&mut active, &mut dec_cache);
                    // Video decode prefers the proxy (§4.3); a proxy that
                    // fails to open (corrupt, evicted mid-read) falls back to
                    // the original silently — never a black preview.
                    //
                    // `acquire_slot` rather than a bare open, so a decoder
                    // parked by the last close is checked back out instead of
                    // reopened. The index is loaded below from the spec, not
                    // taken from the slot: an index built while the file was on
                    // screen is one a parked slot would not have.
                    let mut via_proxy = false;
                    let mut slot = None;
                    let opening = Instant::now();
                    if spec.has_video {
                        if let Some(pp) = spec.proxy.as_deref() {
                            slot = acquire_slot(&mut dec_cache, &hw, pp, None);
                            via_proxy = slot.is_some();
                            if slot.is_none() {
                                log::warn!("proxy {}; using the original", pp.display());
                            }
                        }
                        if slot.is_none() {
                            slot = acquire_slot(&mut dec_cache, &hw, &spec.path, None);
                        }
                    }
                    let (dec_path, dec) = match slot {
                        Some(s) => (Some(s.path), Some(s.dec)),
                        None => (None, None),
                    };
                    if let Some(p) = &dec_path {
                        // The number retention exists to move: a fresh open is
                        // the probe plus the hardware trial decode, a parked one
                        // is a `Vec::remove`.
                        log::debug!(
                            "video decoder for {} in {:.1}ms",
                            p.display(),
                            opening.elapsed().as_secs_f32() * 1e3
                        );
                    }
                    let index = if via_proxy {
                        spec.proxy_index.as_ref()
                    } else {
                        spec.keyframe_index.as_ref()
                    }
                    .and_then(|p| KeyframeIndex::load(p).ok());
                    shared
                        .proxy_active
                        .store(i64::from(via_proxy), Ordering::Relaxed);
                    let (frame_dur, mut duration, path_code) = match &dec {
                        Some(d) => (
                            d.frame_duration_us(),
                            d.duration_us(),
                            match d.decode_path() {
                                DecodePath::Vaapi => 1,
                                DecodePath::Nvdec => 2,
                                DecodePath::Software => 3,
                            },
                        ),
                        None => (33_333, 0, 0),
                    };
                    shared.decode_path.store(path_code, Ordering::Relaxed);
                    let live = dv_media::is_stream_url(&spec.path);
                    let _ = atx.send(AudioCmd::SetSource(
                        spec.has_audio.then(|| spec.path.clone()),
                    ));
                    if duration == 0 && spec.has_audio && !live {
                        // Audio-only: get duration from the audio stream. A
                        // live source has no duration to find, and this open
                        // would be one more connection to the camera.
                        duration = dv_media::AudioDecoder::open(&spec.path)
                            .map(|d| d.duration_us())
                            .unwrap_or(0);
                    }
                    shared.duration_us.store(duration, Ordering::Relaxed);
                    {
                        let mut c = shared.clock.lock();
                        c.set(clamp_seek(start_us, duration));
                        c.rate = 1.0;
                        c.playing = play;
                    }
                    *shared.frame.lock() = None;
                    active = Some(Active {
                        spec: *spec,
                        dec,
                        dec_path,
                        index,
                        cur_pts: i64::MIN,
                        frame_dur,
                        duration,
                        serial,
                        want: Some(start_us),
                        last_kf: None,
                        eof: false,
                        rev: ReverseCache::default(),
                        live,
                        reconnect_at: None,
                        live_was_playing: false,
                        live_anchor: None,
                        live_hold: None,
                        live_cushion: LIVE_CUSHION,
                        live_stats: (Instant::now(), 0, 0, 0, Duration::ZERO),
                        live_times: (0, Duration::ZERO, Duration::ZERO),
                        dec_lost: false,
                    });
                    sync_audio(&shared, &atx);
                }
                Cmd::SetTimeline {
                    segments,
                    mix,
                    duration,
                    snap_fps,
                    start_us,
                    play,
                    serial,
                } => {
                    retire_active(&mut active, &mut dec_cache);
                    // Migrate the old timeline's decoders back to the cache and
                    // remember what frame is on screen (no-flash comparison).
                    let (old_pub_path, old_pub_src, old_black) =
                        if let Some(mut ts) = timeline.take() {
                            if let Some(s) = ts.cur.take() {
                                dec_cache.put(s);
                            }
                            if let Some((_, s)) = ts.prefetch.take() {
                                dec_cache.put(s);
                            }
                            (ts.pub_path, ts.pub_src, ts.is_black)
                        } else {
                            (None, i64::MIN, false)
                        };

                    let start = start_us.clamp(0, duration.max(0));
                    shared.duration_us.store(duration, Ordering::Relaxed);
                    {
                        let mut c = shared.clock.lock();
                        c.set(start);
                        c.rate = 1.0;
                        c.playing = play;
                    }

                    // Audio: hand the render thread the compiled mix (§5).
                    let _ = atx.send(AudioCmd::SetMix(mix));

                    // No-flash: if the frame the new playlist wants at `start`
                    // is the one already published, don't reseek and don't
                    // clear the frame — keep showing it (§ req. 3).
                    let look = start.min((duration - 1).max(0));
                    let want = match segment_at(&segments, look) {
                        Some(i) => match &segments[i].video {
                            Some(v) => {
                                let src = src_from_tl(
                                    v.source_offset_us,
                                    segments[i].tl_start_us,
                                    v.speed,
                                    look,
                                );
                                // Either representation of the source counts as
                                // "same frame": when a proxy finishes mid-view
                                // the service pass upgrades to it without a
                                // want-seek (and without a flash).
                                let (vfile, _, _) = v.video_file();
                                let same = (old_pub_path.as_deref() == Some(vfile)
                                    || old_pub_path.as_deref() == Some(v.path.as_path()))
                                    && (src - old_pub_src).abs() < 1_000;
                                if same {
                                    None
                                } else {
                                    Some(start)
                                }
                            }
                            None => {
                                if old_black {
                                    None
                                } else {
                                    Some(start)
                                }
                            }
                        },
                        None => Some(start),
                    };
                    // NB: never clear `shared.frame` here — that is the flash.

                    timeline = Some(TimelineState {
                        segments,
                        duration,
                        snap_fps,
                        serial,
                        cur_seg: usize::MAX,
                        cur: None,
                        prefetch: None,
                        want,
                        seg_eof: false,
                        pub_path: old_pub_path,
                        pub_src: old_pub_src,
                        is_black: old_black,
                    });
                    sync_audio(&shared, &atx);
                }
                Cmd::ClearSource => {
                    // **Parked, not dropped** ([`VidCache::park`]). Closing a
                    // source is what the viewer does on every arrow key, and
                    // the decoder it was throwing away is the one the arrow key
                    // back would have had to build again.
                    retire_active(&mut active, &mut dec_cache);
                    if let Some(mut ts) = timeline.take() {
                        if let Some(s) = ts.cur.take() {
                            dec_cache.put(s);
                        }
                        if let Some((_, s)) = ts.prefetch.take() {
                            dec_cache.put(s);
                        }
                    }
                    *shared.frame.lock() = None;
                    shared.decode_path.store(0, Ordering::Relaxed);
                    shared.proxy_active.store(0, Ordering::Relaxed);
                    shared.duration_us.store(0, Ordering::Relaxed);
                    bad_proxies.clear();
                    let mut c = shared.clock.lock();
                    c.playing = false;
                    c.set(0);
                    drop(c);
                    let _ = atx.send(AudioCmd::SetSource(None));
                    sync_audio(&shared, &atx);
                }
                Cmd::Seek { us } => {
                    if let Some(a) = active.as_mut() {
                        let us = clamp_seek(us, a.duration);
                        shared.clock.lock().set(us);
                        a.want = Some(us);
                        a.eof = false;
                        seek_born = Some(Instant::now());
                        sync_audio(&shared, &atx);
                    } else if let Some(ts) = timeline.as_mut() {
                        let us = us.min(ts.duration.max(0));
                        shared.clock.lock().set(us);
                        ts.want = Some(us);
                        ts.seg_eof = false;
                        seek_born = Some(Instant::now());
                        sync_audio(&shared, &atx);
                    }
                }
                Cmd::Play => {
                    if let Some(a) = active.as_mut() {
                        let mut c = shared.clock.lock();
                        // Play from EOF restarts (universal player behavior).
                        if !c.playing {
                            if a.duration > 0 && c.position() >= a.duration {
                                c.set(0);
                                a.want = Some(0);
                            }
                            c.rate = 1.0;
                            c.playing = true;
                            let at = c.anchor_us;
                            c.set(at);
                            a.eof = false;
                        }
                        drop(c);
                        sync_audio(&shared, &atx);
                    } else if let Some(ts) = timeline.as_mut() {
                        let mut c = shared.clock.lock();
                        if !c.playing {
                            if ts.duration > 0 && c.position() >= ts.duration {
                                c.set(0);
                                ts.want = Some(0);
                            }
                            c.rate = 1.0;
                            c.playing = true;
                            let at = c.anchor_us;
                            c.set(at);
                            ts.seg_eof = false;
                        }
                        drop(c);
                        sync_audio(&shared, &atx);
                    }
                }
                Cmd::Pause => {
                    let mut c = shared.clock.lock();
                    let pos = c.position();
                    c.playing = false;
                    c.set(pos);
                    drop(c);
                    if let Some(a) = active.as_mut() {
                        a.want = Some(pos); // settle on the exact frame (§4.3)
                        a.eof = false;
                    } else if let Some(ts) = timeline.as_mut() {
                        ts.want = Some(pos);
                        ts.seg_eof = false;
                    }
                    sync_audio(&shared, &atx);
                }
                Cmd::SetRate(rate) => {
                    let mut c = shared.clock.lock();
                    let pos = c.position();
                    c.set(pos);
                    if rate == 0.0 {
                        c.playing = false;
                    } else {
                        c.rate = rate;
                        c.playing = true;
                    }
                    drop(c);
                    if let Some(a) = active.as_mut() {
                        a.eof = false;
                    } else if let Some(ts) = timeline.as_mut() {
                        ts.seg_eof = false;
                    }
                    sync_audio(&shared, &atx);
                }
                Cmd::Step { frames } => {
                    if let Some(a) = active.as_mut() {
                        let mut c = shared.clock.lock();
                        let pos = c.position();
                        c.playing = false;
                        let target = clamp_seek(pos + frames * a.frame_dur, a.duration);
                        c.set(target);
                        drop(c);
                        a.want = Some(target);
                        a.eof = false;
                        seek_born = Some(Instant::now());
                        sync_audio(&shared, &atx);
                    } else if let Some(ts) = timeline.as_mut() {
                        let frame_dur = ts.step_frame_dur();
                        let mut c = shared.clock.lock();
                        let pos = c.position();
                        c.playing = false;
                        let target = (pos + frames * frame_dur).clamp(0, ts.duration.max(0));
                        c.set(target);
                        drop(c);
                        ts.want = Some(target);
                        ts.seg_eof = false;
                        seek_born = Some(Instant::now());
                        sync_audio(&shared, &atx);
                    }
                }
            }
            next = rx.try_recv().ok();
        }

        // Audio is master (§4.4): while audible, re-anchor the wall clock
        // from consumed samples (~10 Hz — position() stays continuous).
        if last_anchor_check.elapsed() > Duration::from_millis(100) {
            last_anchor_check = Instant::now();
            let rate = shared.clock.lock().rate;
            if let Some(audio_pos) = aclock.position(&ring, rate) {
                let mut c = shared.clock.lock();
                // Always consume `moved`, playing or not, so a pause or a seek
                // cannot leave it armed for a check long afterwards.
                let moved = std::mem::take(&mut c.moved);
                if !c.playing || moved {
                    // Nothing to correct against: either the clock is not
                    // running, or it has just been relocated and the audio
                    // clock is still reporting the old position. Correcting
                    // here would drag the playhead back to where it was. The
                    // next check is 100 ms away, by which time the audio thread
                    // has long since processed its `Sync`.
                    resync_snap_votes = 0;
                } else {
                    let err = audio_pos - c.position();
                    if err.abs() > RESYNC_SNAP_US {
                        // A real desync — a stall, a device change. Land on it;
                        // there is nothing worth preserving.
                        //
                        // Two consecutive checks have to agree first. The audio
                        // clock is read as two independent atomics (`media_us`
                        // and `consumed_at`, audio.rs), so a read that straddles
                        // a `Sync` pairs a new anchor with a stale baseline and
                        // reports a position that was never true. One such
                        // reading must not yank the playhead; a genuine desync
                        // is still there 100 ms later.
                        resync_snap_votes += 1;
                        if resync_snap_votes >= 2 {
                            resync_snap_votes = 0;
                            c.drift(audio_pos);
                        }
                    } else if err != 0 {
                        resync_snap_votes = 0;
                        // Otherwise *slew*. The audio clock is quantised to the
                        // device's callback buffer, so it reads a few
                        // milliseconds either side of the truth the whole time
                        // it runs. Snapping to it stepped the playhead by up to
                        // a frame ten times a second, and the picture showed
                        // every one of those steps as a skipped or repeated
                        // frame — the judder that survived after the decoder
                        // stopped re-seeking.
                        //
                        // Two milliseconds per check is 20 ms/s of correction,
                        // far more authority than the drift between a system
                        // clock and an audio device could ever need (well under
                        // 1 ms/s), and no single step is anywhere near a frame.
                        let pos = c.position();
                        c.drift(pos + err.clamp(-RESYNC_SLEW_US, RESYNC_SLEW_US));
                    }
                }
            }
        }

        // Reverse playback stops at the head of the file.
        if let Some(a) = active.as_mut() {
            let mut c = shared.clock.lock();
            if c.playing && c.rate < 0.0 && c.position() <= 0 {
                c.playing = false;
                c.set(0);
                drop(c);
                a.want = Some(0);
                a.eof = false;
                sync_audio(&shared, &atx);
            }
        }

        // Timeline edge conditions: reverse stops at 0, forward pauses exactly
        // at the timeline end (§ req. 5). Segment-internal EOF never pauses —
        // it is handled inside service_timeline_video by holding the frame.
        if let Some(ts) = timeline.as_mut() {
            let mut c = shared.clock.lock();
            if c.playing && c.rate < 0.0 && c.position() <= 0 {
                c.playing = false;
                c.set(0);
                drop(c);
                ts.want = Some(0);
                ts.seg_eof = false;
                sync_audio(&shared, &atx);
            } else if c.playing && c.rate > 0.0 && ts.duration > 0 && c.position() >= ts.duration {
                c.playing = false;
                c.set(ts.duration);
                drop(c);
                ts.want = Some(ts.duration);
                ts.seg_eof = false;
                sync_audio(&shared, &atx);
            }
        }

        // Service video.
        if let Some(a) = active.as_mut() {
            service_video(
                a,
                &shared,
                &device,
                &queue,
                &rx,
                &atx,
                &mut deferred,
                &mut seek_born,
                &hw,
            );
        } else if let Some(ts) = timeline.as_mut() {
            service_timeline_video(
                ts,
                &mut dec_cache,
                &hw,
                &shared,
                &device,
                &queue,
                &rx,
                &atx,
                &mut deferred,
                &mut bad_proxies,
                &mut seek_born,
            );
            maybe_prefetch(ts, &mut dec_cache, &hw, &shared, &mut bad_proxies);
        }
    }
}

/// Decode/publish work for one wake-up. Returns quickly; long catch-ups
/// check for newer seeks between frames (latest-wins, §4.3).
#[allow(clippy::too_many_arguments)]
fn service_video(
    a: &mut Active,
    shared: &Shared,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rx: &Receiver<Cmd>,
    atx: &Sender<AudioCmd>,
    deferred: &mut Vec<Cmd>,
    seek_born: &mut Option<Instant>,
    hw: &[HwDevice],
) {
    // ── Live sources ────────────────────────────────────────────────────────
    // A live stream has no timeline to reconcile a clock against: the camera
    // sets the pace, every frame is the newest there is, and none of the
    // seek/cover/EOF machinery below can mean anything. It gets its own
    // service pass.
    if a.live {
        service_live(a, shared, device, queue, hw);
        return;
    }

    let (pos, playing, rate) = {
        let c = shared.clock.lock();
        (c.position().max(0), c.playing, c.rate)
    };
    let had_want = a.want.is_some();
    let target = a.want.unwrap_or(pos);

    // ── Reverse playback ────────────────────────────────────────────────────
    //
    // A codec decodes forwards, so playing backwards means buying a run of real
    // frames with one forward pass and handing them out in reverse
    // ([`service_reverse`]). Below the shuttle rates that is what happens, and
    // it is what makes `j` read as playback rather than as a slideshow. Above
    // them — and on sources whose frames are too big to buy a run of — it falls
    // back to stepping keyframes, which is chunky on purpose (§4.4) and, since
    // [`reverse_holds`], at least always chunky *backwards*.
    if !(playing && rate < 0.0) {
        // Nothing is running backwards: the run is dead weight, and it is the
        // largest thing this thread holds.
        a.rev.clear();
    } else if a.want.is_none()
        && rate >= -SMOOTH_REVERSE_MAX
        && !a.rev.give_up
        && service_reverse(a, shared, device, queue, target)
    {
        return;
    }

    let Some(dec) = a.dec.as_mut() else { return };
    // Reverse *playback* shows keyframes only (§4.4: proxy/keyframe stepping,
    // chunky is accepted) — decoding a whole GOP per tick would peg a core.
    // Explicit seeks/steps (a.want) still land exactly.
    let keyframe_only = playing && rate < 0.0 && a.want.is_none();

    let jitter = jitter_tolerance(playing, rate, a.want, a.frame_dur);
    // Reverse playback / backward jump: seek back when the target precedes
    // the current frame (§4.4).
    let behind = target < a.cur_pts.saturating_sub(jitter);
    // Forward gap worth a seek instead of decoding through (> 1 s or a
    // pending explicit seek far ahead).
    let far_ahead = target > a.cur_pts + 1_000_000.max(4 * a.frame_dur);

    // The frame already on screen still covers the playhead: nothing to do.
    //
    // A frame that was *dropped* rather than published can never satisfy this:
    // the loop below only drops frames for which `pts + frame_dur <= target`,
    // the negation of `frame_covers`'s upper bound. So `cur_pts` here is always
    // the frame that was published, and no extra bookkeeping is needed.
    if a.want.is_none() && frame_covers(a.cur_pts, target, a.frame_dur, jitter) {
        return;
    }

    if reverse_holds(keyframe_only, a.cur_pts, target) {
        return;
    }

    if a.cur_pts == i64::MIN || behind || far_ahead || a.dec_lost {
        // Deterministic landing via the cached keyframe index when we have
        // one (§4.3): seek to the preceding keyframe's exact pts.
        let kf = a
            .index
            .as_ref()
            .and_then(|ix| ix.keyframe_before(target))
            .and_then(|e| e.pts_us);
        if keyframe_only && kf.is_some() && kf == a.last_kf {
            return; // still inside the keyframe we're already showing
        }
        let seek_to = kf.unwrap_or(target);
        if dec.seek(seek_to.min(target)).is_err() {
            return;
        }
        a.last_kf = kf;
        a.cur_pts = i64::MIN;
        a.dec_lost = false;
    }

    // Decode forward until we hold the frame covering the target.
    let mut published = false;
    loop {
        // Latest-wins: a newer seek aborts this catch-up.
        if let Ok(cmd) = rx.try_recv() {
            // Push back for the main loop by handling only Seek here.
            match cmd {
                Cmd::Seek { us } => {
                    let us = clamp_seek(us, a.duration);
                    shared.clock.lock().set(us);
                    a.want = Some(us);
                    *seek_born = Some(Instant::now());
                    let _ = atx.send(AudioCmd::Sync {
                        media_us: us.max(0),
                        rate,
                        playing,
                    });
                    return; // re-enter next wake with the new target
                }
                other => {
                    // Non-seek command mid-catch-up: replay it from the main
                    // loop right after this pass — nothing is lost.
                    deferred.push(other);
                    return;
                }
            }
        }

        match dec.next_frame() {
            Ok(Some(frame)) => {
                let mut pts = frame.pts_us;
                // §14: VFR sources snap to the project frame grid.
                if let Some(fps) = a.spec.snap_fps {
                    let dur = 1e6 / fps;
                    pts = ((pts as f64 / dur).round() * dur) as i64;
                }
                a.cur_pts = pts;
                let covers = keyframe_only || pts + a.frame_dur > target;
                if covers {
                    publish(shared, device, queue, &frame, pts, a.serial);
                    published = true;
                    break;
                }
                // Late frame during playback: keep decoding (drop it).
            }
            Ok(None) => {
                // EOF: clamp and pause.
                a.eof = true;
                let mut c = shared.clock.lock();
                if c.playing && c.rate > 0.0 {
                    c.playing = false;
                    let end = if a.duration > 0 {
                        a.duration
                    } else {
                        a.cur_pts
                    };
                    c.set(end);
                    drop(c);
                    let _ = atx.send(AudioCmd::Sync {
                        media_us: end,
                        rate,
                        playing: false,
                    });
                }
                break;
            }
            Err(e) => {
                log::warn!("decode error: {e}");
                break;
            }
        }
    }

    if published && had_want {
        if let Some(t0) = seek_born.take() {
            shared
                .seek_latency_us
                .store(t0.elapsed().as_micros() as i64, Ordering::Relaxed);
        }
    }
    if published || a.eof {
        a.want = None;
    }
}

/// The *starting* pacing cushion for a live source: how far behind "just
/// arrived" a frame is shown, which is the jitter the display absorbs without
/// a stutter. RTSP cameras deliver in bursts — a Reolink batches most of a
/// GOP's worth of packets — so arrival times are lumpy even at a rock-steady
/// 1.0× throughput. A quarter second is invisible on a doorbell; cameras
/// whose bursts are wider grow it ([`Active::live_cushion`]) instead of
/// everyone paying the worst camera's latency up front.
const LIVE_CUSHION: Duration = Duration::from_millis(250);
/// Each stale re-anchor widens the cushion by this much…
const LIVE_CUSHION_STEP: Duration = Duration::from_millis(250);
/// …up to here. Past a second and a half of lag a doorbell stops being live.
const LIVE_CUSHION_MAX: Duration = Duration::from_millis(1500);
/// A frame later than this against the anchor means the mapping itself is
/// stale (a stall, a camera clock hiccup) — re-anchor on it rather than
/// fast-forwarding through a backlog forever.
const LIVE_SLIP: Duration = Duration::from_millis(700);

/// One service pass for a **live** source: decode at the camera's pace, show
/// each frame when its *timestamp* comes due.
///
/// The rules are a monitor's, not a player's:
/// * No target, no seeks: frames are published in arrival order, paced by a
///   private pts→wall anchor ([`LIVE_CUSHION`]). Publishing on arrival was the
///   first version, and it turned RTSP's bursty delivery straight into judder.
/// * A stall or a decode error retires the session and reconnects after a
///   beat. `stimeout` (dv-media's `open_input`) is what turns a dead socket
///   into that EOF instead of a thread blocked forever.
/// * The pause→play edge reconnects too: while paused nothing drains the
///   socket, so whatever a resume would decode is stale footage — and a
///   monitor resumes to *now*. The fresh session is the seek-to-live-edge.
///
/// The wall clock is left alone. Nothing in live mode reads a position — the
/// strip shows a LIVE badge instead of timecodes — and slaving the clock to
/// stream pts would only give the audio master-correction something to fight.
fn service_live(
    a: &mut Active,
    shared: &Shared,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    hw: &[HwDevice],
) {
    // Live never has a pending exact seek; a Cmd::Seek that slipped through
    // must not park the pass in a catch-up loop that cannot end.
    a.want = None;

    let playing = shared.clock.lock().playing;
    let resumed = playing && !a.live_was_playing && a.cur_pts != i64::MIN;
    a.live_was_playing = playing;
    if !playing {
        return;
    }
    if resumed {
        a.dec = None;
        a.reconnect_at = None;
    }

    if a.dec.is_none() {
        a.live_anchor = None;
        a.live_hold = None;
        if a.reconnect_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        match VideoDecoder::open_with(&a.spec.path, hw) {
            Ok(dec) => {
                a.frame_dur = dec.frame_duration_us();
                shared.decode_path.store(
                    match dec.decode_path() {
                        DecodePath::Vaapi => 1,
                        DecodePath::Nvdec => 2,
                        DecodePath::Software => 3,
                    },
                    Ordering::Relaxed,
                );
                a.dec_path = Some(a.spec.path.clone());
                a.dec = Some(dec);
                a.reconnect_at = None;
            }
            Err(e) => {
                log::warn!("live reopen {}: {e}", a.spec.path.display());
                a.reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                return;
            }
        }
    }

    // A frame already decoded and waiting: publish it when it comes due, and
    // do not touch the socket before then — the wait IS the cushion, and TCP
    // holds the next burst meanwhile. The 3 ms service cadence bounds how
    // late "due" can land.
    let now = Instant::now();
    if let Some(frame) = a.live_hold.take() {
        if live_due(a, frame.pts_us).is_some_and(|due| now < due) {
            a.live_hold = Some(frame);
            return;
        }
        a.cur_pts = frame.pts_us;
        publish(shared, device, queue, &frame, frame.pts_us, a.serial);
        a.live_stats.1 += 1;
        return;
    }

    let Some(dec) = a.dec.as_mut() else { return };
    let decode_t0 = Instant::now();
    let decoded = dec.next_frame();
    let decode_dt = decode_t0.elapsed();
    a.live_times.0 += 1;
    a.live_times.1 += decode_dt;
    match decoded {
        Ok(Some(frame)) => {
            let had_anchor = a.live_anchor.is_some();
            match live_due(a, frame.pts_us) {
                // A frame that would wait far past the cushion is not early,
                // it is a camera clock running fast — holding ever longer
                // would back the socket up without bound. Re-anchor, show it.
                Some(due) if due.duration_since(now) > a.live_cushion + Duration::from_secs(1) => {
                    a.live_anchor = Some((frame.pts_us, now + a.live_cushion));
                }
                // Early: hold it for its moment.
                Some(due) if now < due => {
                    a.live_hold = Some(frame);
                    return;
                }
                // Late, by less than a stall: show it now, and widen the
                // cushion by exactly the shortfall — a late frame IS the
                // measurement of how much cushion this camera's bursts need,
                // and the widening costs nothing visible (this frame appears
                // immediately; the ones behind it become early, i.e. smooth).
                // Within one burst cycle this converges on the camera's real
                // batching period. Past the cap, a couple of milliseconds of
                // slew still absorbs clock drift without unbounded latency.
                Some(due) if now.duration_since(due) < LIVE_SLIP => {
                    let late = now.duration_since(due);
                    a.live_stats.2 += 1;
                    a.live_stats.4 = a.live_stats.4.max(late);
                    // A few milliseconds is pts quantisation and clock skew,
                    // not a burst — invisible under a frame period, and
                    // widening on it ratchets the cushion to the cap over a
                    // minute. Skew gets the slew; only a *visible* shortfall
                    // buys cushion.
                    if late <= Duration::from_millis(30) {
                        if let Some((_, awall)) = a.live_anchor.as_mut() {
                            *awall += late.min(Duration::from_millis(2));
                        }
                    } else {
                        let widen = late.min(LIVE_CUSHION_MAX.saturating_sub(a.live_cushion));
                        a.live_cushion += widen;
                        if let Some((_, awall)) = a.live_anchor.as_mut() {
                            *awall += widen + (late - widen).min(Duration::from_millis(2));
                        }
                    }
                }
                // Very late, no anchor yet, or pts jumped backwards: the
                // mapping is stale. Re-anchor on this frame, cushion and all —
                // and if there WAS an anchor, the cushion just proved too
                // narrow for this camera's bursts: widen it. The stutter that
                // triggered this is the measurement.
                _ => {
                    if had_anchor {
                        a.live_cushion = (a.live_cushion + LIVE_CUSHION_STEP).min(LIVE_CUSHION_MAX);
                        a.live_stats.3 += 1;
                    }
                    a.live_anchor = Some((frame.pts_us, now + a.live_cushion));
                }
            }
            a.cur_pts = frame.pts_us;
            let publish_t0 = Instant::now();
            publish(shared, device, queue, &frame, frame.pts_us, a.serial);
            a.live_times.2 += publish_t0.elapsed();
            a.live_stats.1 += 1;

            // The delivery picture, every five seconds: how many frames, how
            // many arrived after their moment, how often the mapping had to be
            // rebuilt, and the worst lateness — the numbers cushion tuning
            // needs, without a packet capture.
            let (since, published, late, reanchors, worst) = a.live_stats;
            if since.elapsed() > Duration::from_secs(5) {
                let (calls, decode, upload) = a.live_times;
                log::debug!(
                    "live: {published} frames, {late} late (worst {worst:?}), {reanchors} re-anchors, cushion {:?}; {calls} decodes {decode:?}, uploads {upload:?}",
                    a.live_cushion
                );
                a.live_stats = (now, 0, 0, 0, Duration::ZERO);
                a.live_times = (0, Duration::ZERO, Duration::ZERO);
            }
        }
        Ok(None) => {
            // On a live source EOF *is* the stall — retire the session and
            // rejoin. The gap between attempts is what keeps a dead camera
            // from costing a blocked open per pass.
            log::warn!("live stream stalled: {}", a.spec.path.display());
            a.dec = None;
            a.dec_path = None;
            a.reconnect_at = Some(Instant::now() + Duration::from_secs(1));
        }
        Err(e) => {
            log::warn!("live decode error: {e}");
            a.dec = None;
            a.dec_path = None;
            a.reconnect_at = Some(Instant::now() + Duration::from_secs(1));
        }
    }
}

/// When `pts` should appear on screen, against the live anchor. `None` when
/// there is no anchor yet, or when `pts` runs backwards past it (a new RTSP
/// session restarts pts near zero) — both mean "re-anchor on this frame".
fn live_due(a: &Active, pts: i64) -> Option<Instant> {
    let (apts, awall) = a.live_anchor?;
    let delta = pts.checked_sub(apts).filter(|d| *d >= 0)?;
    awall.checked_add(Duration::from_micros(delta as u64))
}

/// Decode/publish work for one wake-up in **timeline** mode (§6.3). Mirrors
/// [`service_video`] within the segment under the playhead — same keyframe-
/// index seek, latest-wins coalescing, keyframe-only reverse and VFR snapping
/// (§14) — but in source time, publishing TIMELINE pts. Handles cuts (decoder
/// switch/adopt), gaps (synthetic black), and segment-internal EOF (hold).
#[allow(clippy::too_many_arguments)]
fn service_timeline_video(
    ts: &mut TimelineState,
    cache: &mut VidCache,
    hw: &[HwDevice],
    shared: &Shared,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rx: &Receiver<Cmd>,
    atx: &Sender<AudioCmd>,
    deferred: &mut Vec<Cmd>,
    bad_proxies: &mut std::collections::HashSet<PathBuf>,
    seek_born: &mut Option<Instant>,
) {
    let (pos, playing, rate) = {
        let c = shared.clock.lock();
        (c.position().max(0), c.playing, c.rate)
    };
    let had_want = ts.want.is_some();
    let record_latency = |shared: &Shared, seek_born: &mut Option<Instant>| {
        if had_want {
            if let Some(t0) = seek_born.take() {
                shared
                    .seek_latency_us
                    .store(t0.elapsed().as_micros() as i64, Ordering::Relaxed);
            }
        }
    };
    let target_tl = ts.want.unwrap_or(pos).clamp(0, ts.duration.max(0));
    let look = target_tl.min((ts.duration - 1).max(0));
    let Some(seg_idx) = segment_at(&ts.segments, look) else {
        ts.want = None;
        return;
    };

    // Gap segment (§6.3): publish one synthetic black frame on entry.
    if ts.segments[seg_idx].video.is_none() {
        if let Some(s) = ts.cur.take() {
            cache.put(s);
        }
        ts.cur_seg = seg_idx;
        ts.seg_eof = false;
        if !ts.is_black {
            publish_black(shared, device, queue, target_tl, ts.serial);
            ts.is_black = true;
            ts.pub_path = None;
            ts.pub_src = i64::MIN;
        }
        record_latency(shared, seek_born);
        ts.want = None;
        return;
    }

    // Snapshot the segment's scalars so we can mutate other fields below.
    let seg = &ts.segments[seg_idx];
    let seg_tl_start = seg.tl_start_us;
    let seg_end = seg.tl_end_us();
    let Some(vsrc) = seg.video.as_ref() else {
        ts.want = None;
        return; // unreachable: the gap case returned above
    };
    let src_offset = vsrc.source_offset_us;
    let speed = vsrc.speed;
    let (mut vpath, mut index_path, mut via_proxy) = choose_video_file(vsrc, bad_proxies);
    let opath = vsrc.path.clone();
    let oindex = vsrc.keyframe_index.clone();

    // (Re)select the current decoder. Adopt a matching prefetch (warmed at the
    // cut, incl. a discontinuous same-file jump); else keep a running same-file
    // decoder; else check out / open the right one.
    let adopt_ready = matches!(&ts.prefetch, Some((pi, ps)) if *pi == seg_idx && ps.path == vpath);
    let need_switch = adopt_ready || ts.cur.as_ref().map(|s| s.path != vpath).unwrap_or(true);
    if need_switch {
        if let Some(old) = ts.cur.take() {
            cache.put(old);
        }
        let mut slot = if adopt_ready {
            ts.prefetch.take().map(|(_, s)| s)
        } else {
            if let Some((_, ps)) = ts.prefetch.take() {
                cache.put(ps);
            }
            acquire_slot(cache, hw, &vpath, index_path.as_deref())
        };
        // A proxy that won't open falls back to the original (§4.3) and is
        // remembered as bad so this can't recur per-frame.
        if slot.is_none() && via_proxy {
            bad_proxies.insert(vpath.clone());
            via_proxy = false;
            vpath = opath.clone();
            index_path = oindex.clone();
            slot = acquire_slot(cache, hw, &vpath, index_path.as_deref());
        }
        match slot {
            Some(s) => {
                let code = match s.dec.decode_path() {
                    DecodePath::Vaapi => 1,
                    DecodePath::Nvdec => 2,
                    DecodePath::Software => 3,
                };
                shared.decode_path.store(code, Ordering::Relaxed);
                shared
                    .proxy_active
                    .store(i64::from(via_proxy), Ordering::Relaxed);
                ts.cur = Some(s);
            }
            None => {
                ts.cur = None;
                ts.cur_seg = seg_idx;
                if !ts.is_black {
                    publish_black(shared, device, queue, target_tl, ts.serial);
                    ts.is_black = true;
                    ts.pub_path = None;
                    ts.pub_src = i64::MIN;
                }
                ts.want = None;
                return;
            }
        }
        ts.seg_eof = false;
    }
    ts.cur_seg = seg_idx;

    let src_target = src_from_tl(
        src_offset,
        seg_tl_start,
        speed,
        look.clamp(seg_tl_start, seg_end - 1),
    );
    let want = ts.want;
    let snap_fps = ts.snap_fps;
    let Some(slot) = ts.cur.as_mut() else {
        return;
    };
    let frame_dur = slot.frame_dur;

    // Seamless cut: publish the warmed prefetch frame if it covers the target.
    if let Some(f) = slot.warm.take() {
        let pts = snap_pts(f.pts_us, snap_fps);
        if pts + frame_dur > src_target && src_target + 2_000_000 > pts {
            slot.cur_pts = pts;
            let tl_pts = tl_from_src(src_offset, seg_tl_start, speed, pts);
            publish(shared, device, queue, &f, tl_pts, ts.serial);
            ts.pub_path = Some(vpath.clone());
            ts.pub_src = pts;
            ts.is_black = false;
            ts.seg_eof = false;
            record_latency(shared, seek_born);
            ts.want = None;
            return;
        }
        slot.cur_pts = pts;
    }

    // Everything below is in *source* time, so the tolerance is the source
    // frame duration — a frame is a frame whatever `speed` maps it onto.
    let jitter = jitter_tolerance(playing, rate, want, frame_dur);

    // No-op fast path: the covering frame from this file is already on screen.
    //
    // Paused, this is what stops a wholesale playlist re-send after an edit
    // from re-decoding or flashing (§ req. 3). *Playing*, it is what keeps
    // playback frame-paced — see [`frame_covers`]. It used to be gated on
    // `!playing`, which left the playing case decoding a fresh frame every
    // 3 ms wake and then re-seeking to the keyframe when the clock corrected
    // itself backwards: the same defect, and the same fix, as `service_video`.
    //
    // The warmed prefetch frame above can leave `cur_pts` on a frame that was
    // decoded but *not* published — but only after failing to cover
    // `src_target`, either short of it (`pts + frame_dur <= src_target`) or
    // more than 2 s past it. Both fall outside `frame_covers`'s window, so
    // neither can trip this.
    if want.is_none()
        && !ts.is_black
        && ts.pub_path.as_deref() == Some(vpath.as_path())
        && frame_covers(slot.cur_pts, src_target, frame_dur, jitter)
    {
        return;
    }

    // Hold the last frame after segment-internal EOF; don't spin re-seeking.
    if ts.seg_eof
        && want.is_none()
        && slot.cur_pts != i64::MIN
        && src_target >= slot.cur_pts.saturating_sub(jitter)
    {
        return;
    }

    let keyframe_only = playing && rate < 0.0 && want.is_none();
    let behind = src_target < slot.cur_pts.saturating_sub(jitter);
    let far_ahead = src_target > slot.cur_pts + 1_000_000.max(4 * frame_dur);

    // Reverse holds its keyframe until the playhead falls behind it —
    // `service_video`'s guard, for the same reason and against the same
    // forward-run-between-jumps stutter.
    if reverse_holds(keyframe_only, slot.cur_pts, src_target) {
        return;
    }

    if slot.cur_pts == i64::MIN || behind || far_ahead {
        let kf = slot
            .index
            .as_ref()
            .and_then(|ix| ix.keyframe_before(src_target))
            .and_then(|e| e.pts_us);
        if keyframe_only && kf.is_some() && kf == slot.last_kf {
            return; // still inside the keyframe we're already showing
        }
        let seek_to = kf.unwrap_or(src_target);
        if slot.dec.seek(seek_to.min(src_target)).is_err() {
            return;
        }
        slot.last_kf = kf;
        slot.cur_pts = i64::MIN;
        ts.seg_eof = false;
    }

    let mut published = false;
    loop {
        // Latest-wins: a newer seek aborts the catch-up (§4.3). Timeline seeks
        // carry TIMELINE µs, and the audio clock is timeline time too.
        if let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Seek { us } => {
                    let us = us.clamp(0, ts.duration.max(0));
                    shared.clock.lock().set(us);
                    ts.want = Some(us);
                    ts.seg_eof = false;
                    *seek_born = Some(Instant::now());
                    let _ = atx.send(AudioCmd::Sync {
                        media_us: us,
                        rate,
                        playing,
                    });
                    return;
                }
                other => {
                    deferred.push(other);
                    return;
                }
            }
        }

        match slot.dec.next_frame() {
            Ok(Some(frame)) => {
                let pts = snap_pts(frame.pts_us, snap_fps);
                slot.cur_pts = pts;
                let covers = keyframe_only || pts + frame_dur > src_target;
                if covers {
                    let tl_pts = tl_from_src(src_offset, seg_tl_start, speed, pts);
                    publish(shared, device, queue, &frame, tl_pts, ts.serial);
                    ts.pub_path = Some(vpath.clone());
                    ts.pub_src = pts;
                    ts.is_black = false;
                    published = true;
                    break;
                }
                // Late frame during playback: keep decoding (drop it).
            }
            Ok(None) => {
                // Segment-internal EOF (source shorter than the clip claims):
                // hold the last frame, never pause mid-timeline (§ req. 5).
                ts.seg_eof = true;
                break;
            }
            Err(e) => {
                log::warn!("timeline decode error: {e}");
                break;
            }
        }
    }

    if published {
        record_latency(shared, seek_born);
    }
    if published || ts.seg_eof {
        ts.want = None;
    }
}

/// §14: snap a source pts to the project frame grid when a project fps is set.
fn snap_pts(pts_us: i64, snap_fps: Option<f64>) -> i64 {
    match snap_fps {
        Some(fps) if fps > 0.0 => {
            let d = 1e6 / fps;
            ((pts_us as f64 / d).round() * d) as i64
        }
        _ => pts_us,
    }
}

/// Prepare the decoder for an upcoming cut (§4.4): open + pre-seek + pre-decode
/// the first frame (unpublished) so the cut plays seamlessly. One prefetch
/// decoder at a time. Runs on the controller thread like the rest of decoding.
fn maybe_prefetch(
    ts: &mut TimelineState,
    cache: &mut VidCache,
    hw: &[HwDevice],
    shared: &Shared,
    bad_proxies: &mut std::collections::HashSet<PathBuf>,
) {
    let (pos, playing, rate) = {
        let c = shared.clock.lock();
        (c.position().max(0), c.playing, c.rate)
    };
    let Some(next) = prefetch_target(&ts.segments, ts.cur_seg, pos, playing, rate) else {
        return;
    };
    if ts.prefetch.as_ref().map(|(i, _)| *i) == Some(next) {
        return; // already prepared for this cut
    }
    if let Some((_, ps)) = ts.prefetch.take() {
        cache.put(ps); // stale prefetch (playhead moved) — recycle it
    }
    let Some(vsrc) = ts.segments[next].video.as_ref() else {
        return;
    };
    let (path, index_path, via_proxy) = choose_video_file(vsrc, bad_proxies);
    let src0 = vsrc.source_offset_us;
    let snap_fps = ts.snap_fps;

    let Some(mut slot) = acquire_slot(cache, hw, &path, index_path.as_deref()) else {
        if via_proxy {
            // Don't retry the broken proxy at the cut; service falls back.
            bad_proxies.insert(path);
        }
        return;
    };
    let kf = slot
        .index
        .as_ref()
        .and_then(|ix| ix.keyframe_before(src0))
        .and_then(|e| e.pts_us)
        .unwrap_or(src0);
    if slot.dec.seek(kf.min(src0)).is_err() {
        cache.put(slot);
        return;
    }
    slot.cur_pts = i64::MIN;
    slot.last_kf = None;
    slot.warm = None;
    let fd = slot.frame_dur;
    // Warm to the frame covering the segment's first source instant.
    while let Ok(Some(frame)) = slot.dec.next_frame() {
        let pts = snap_pts(frame.pts_us, snap_fps);
        slot.cur_pts = pts;
        if pts + fd > src0 {
            slot.warm = Some(frame);
            break;
        }
    }
    ts.prefetch = Some((next, slot));
}

/// Publish a tiny synthetic black NV12 frame for a Gap segment (§6.3). Black in
/// limited-range NV12 is Y=16, U=V=128; BT.709 matrix defaults.
fn publish_black(
    shared: &Shared,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tl_pts: i64,
    serial: u64,
) {
    let f = Nv12Frame {
        pts_us: tl_pts,
        width: 16,
        height: 16,
        y: vec![16u8; 16 * 16],
        uv: vec![128u8; 16 * 16 / 2],
        matrix: ColorMatrix::Bt709,
        limited_range: true,
    };
    publish(shared, device, queue, &f, tl_pts, serial);
}

/// Upload NV12 planes and publish the frame slot (§3: upload on this thread).
fn publish(
    shared: &Shared,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &Nv12Frame,
    pts_us: i64,
    serial: u64,
) {
    let make = |label: &str, w: u32, h: u32, format: wgpu::TextureFormat| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    };
    let y_tex = make(
        "video-y",
        frame.width,
        frame.height,
        wgpu::TextureFormat::R8Unorm,
    );
    let uv_tex = make(
        "video-uv",
        frame.width / 2,
        frame.height / 2,
        wgpu::TextureFormat::Rg8Unorm,
    );
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &y_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &frame.y,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(frame.width),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: frame.width,
            height: frame.height,
            depth_or_array_layers: 1,
        },
    );
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &uv_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &frame.uv,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(frame.width),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: frame.width / 2,
            height: frame.height / 2,
            depth_or_array_layers: 1,
        },
    );
    let tex = VideoFrameTex {
        pts_us,
        width: frame.width,
        height: frame.height,
        y: y_tex.create_view(&wgpu::TextureViewDescriptor::default()),
        uv: uv_tex.create_view(&wgpu::TextureViewDescriptor::default()),
        matrix: frame.matrix,
        limited_range: frame.limited_range,
        source_serial: serial,
    };
    *shared.frame.lock() = Some(Arc::new(tex));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame at 60 fps, the unit both guards are expressed in.
    const FD: i64 = 16_666;

    /// **A decoder parked on close is the decoder found on reopen** — the whole
    /// of the retention change. Arrowing off a clip in the viewer used to drop
    /// the decoder, so arrowing back re-paid the ffmpeg open, the hardware trial
    /// decode and the probe for a file that had been open a keystroke earlier.
    ///
    /// It is the same LRU, the same key and the same cap timeline mode has always
    /// used, so this also pins the thing that would break first: a parked slot is
    /// **checked out**, not shared, and its position starts unknown so the
    /// adopting `SetSource`'s pending seek lands exactly where a fresh open would
    /// have.
    #[test]
    fn a_parked_decoder_is_checked_back_out_by_path() {
        let clip =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/k-clip-3s.mp4");
        let mut cache = VidCache::new(3);
        // Software only: the trial decode this test is about not repeating is
        // also the slowest thing a hardware probe would do here.
        let Some(mut slot) = acquire_slot(&mut cache, &[], &clip, None) else {
            eprintln!("skipped: no decodable fixture");
            return;
        };
        // Somewhere in the middle of the file, as a closing source would be.
        assert!(slot.dec.seek(1_000_000).is_ok());
        slot.cur_pts = 1_000_000;
        let frame_dur = slot.frame_dur;
        cache.park(slot.path.clone(), slot.dec, slot.index, frame_dur);

        let parked = acquire_slot(&mut cache, &[], &clip, None).expect("parked");
        assert_eq!(parked.path, clip);
        assert_eq!(parked.frame_dur, frame_dur, "its cadence came back with it");
        assert_eq!(
            parked.cur_pts,
            i64::MIN,
            "position is not retained: the adopter seeks"
        );
        assert!(parked.warm.is_none() && parked.last_kf.is_none());
        // Checked out, not shared — nothing may hand the same decoder out twice.
        assert!(cache.take(&clip).is_none());
    }

    /// The cap is the cap: parking past it retires the least-recently-used file,
    /// which is what keeps retention from being an unbounded pile of open
    /// decoders in a folder of clips.
    #[test]
    fn parking_past_the_cap_evicts_the_oldest_file() {
        let clip =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/k-clip-3s.mp4");
        let mut cache = VidCache::new(1);
        let Some(slot) = acquire_slot(&mut cache, &[], &clip, None) else {
            eprintln!("skipped: no decodable fixture");
            return;
        };
        let first = std::path::PathBuf::from("/first.mp4");
        cache.park(first.clone(), slot.dec, None, FD);
        assert!(cache.take(&first).is_some());

        let Some(slot) = acquire_slot(&mut cache, &[], &clip, None) else {
            return;
        };
        let Some(other) = acquire_slot(&mut cache, &[], &clip, None) else {
            return;
        };
        cache.park(first.clone(), slot.dec, None, FD);
        cache.park(std::path::PathBuf::from("/second.mp4"), other.dec, None, FD);
        assert!(cache.take(&first).is_none(), "the oldest is out at the cap");
    }

    /// The window is half-open: the frame covers from its own pts up to (not
    /// including) the next one. Anything else double-publishes on the boundary
    /// or leaves a one-microsecond hole between consecutive frames.
    #[test]
    fn a_frame_covers_its_own_interval_and_not_the_next() {
        assert!(frame_covers(0, 0, FD, 0));
        assert!(frame_covers(0, FD - 1, FD, 0));
        assert!(
            !frame_covers(0, FD, FD, 0),
            "the next frame's pts is not ours"
        );
        assert!(
            !frame_covers(FD, 0, FD, 0),
            "a frame does not cover the past"
        );
    }

    /// **Reverse never walks forward.** The keyframe a backward seek lands on
    /// sits a whole GOP before the playhead, and the frame that covers that
    /// playhead is one the reverse shuttle must never go and fetch: fetching it
    /// is a run *forwards* through the GOP, which is what made `j` look like a
    /// stutter rather than like playback.
    #[test]
    fn reverse_holds_its_keyframe_until_the_playhead_passes_it() {
        // The playhead is most of a GOP ahead of the keyframe on screen: hold.
        assert!(reverse_holds(true, 9_000_000, 9_800_000));
        // Right on it, which is where a backward seek lands: still hold.
        assert!(reverse_holds(true, 9_000_000, 9_000_000));
        // Past it: this is the seek back to the previous keyframe, and the one
        // case the guard must let through or reverse never moves at all.
        assert!(!reverse_holds(true, 9_000_000, 8_999_999));
        // Nothing decoded yet is never a frame to hold.
        assert!(!reverse_holds(true, i64::MIN, 0));
        // Forward playback and explicit seeks are not keyframe-only, and land
        // exactly — the guard is not theirs.
        assert!(!reverse_holds(false, 9_000_000, 9_800_000));
    }

    /// **An unknown duration is not a zero-length source.** A container whose
    /// header and streams both report nothing leaves `duration == 0`, and
    /// clamping to that pinned every seek and step to 0 — the file played but
    /// the transport was dead. Unknown means "no upper bound known": the target
    /// goes through and EOF stops the decoder.
    #[test]
    fn seeks_are_only_clamped_when_the_duration_is_known() {
        // Known duration: clamped at both ends.
        assert_eq!(clamp_seek(5_000_000, 10_000_000), 5_000_000);
        assert_eq!(clamp_seek(20_000_000, 10_000_000), 10_000_000);
        assert_eq!(clamp_seek(-1, 10_000_000), 0);
        // Unknown duration: the target survives, and 0 is still the floor.
        assert_eq!(clamp_seek(20_000_000, 0), 20_000_000);
        assert_eq!(clamp_seek(20_000_000, -1), 20_000_000);
        assert_eq!(clamp_seek(-5, 0), 0);
    }

    /// A synthetic frame of `bytes` payload at `pts`.
    fn rev_frame(pts: i64, bytes: usize) -> Nv12Frame {
        Nv12Frame {
            pts_us: pts,
            width: 2,
            height: 2,
            y: vec![0; bytes * 2 / 3],
            uv: vec![0; bytes / 3],
            matrix: ColorMatrix::Bt709,
            limited_range: true,
        }
    }

    /// **The run is handed out from the back, one frame at a time, and never
    /// forwards.** This is the whole contract of reverse playback: the frame on
    /// screen is the newest one at or before the playhead, and a playhead that
    /// only falls can only ever be answered with earlier frames.
    #[test]
    fn a_reverse_run_is_handed_out_backwards() {
        let mut rev = ReverseCache::default();
        for i in 0..10 {
            rev.push(rev_frame(i * FD, 300));
        }
        // Walk the playhead back through the run and collect what it shows.
        let mut shown = Vec::new();
        for step in (0..10).rev() {
            // Mid-frame, which is where a clock actually sits.
            let target = step * FD + FD / 2;
            shown.push(rev.frame_at(target, FD).map(|f| f.pts_us));
        }
        assert_eq!(
            shown,
            (0..10).rev().map(|i| Some(i * FD)).collect::<Vec<_>>()
        );
        // Used up: there is nothing left before the head of the run, which is
        // what sends the caller back for another one.
        assert_eq!(rev.frame_at(-1, FD).map(|f| f.pts_us), None);
    }

    /// A frame is held while the clock crosses it, and the run is not consumed
    /// faster than the playhead moves — otherwise reverse would play at the
    /// tick rate rather than at the frame rate.
    #[test]
    fn a_reverse_frame_is_held_until_the_playhead_leaves_it() {
        let mut rev = ReverseCache::default();
        rev.push(rev_frame(0, 300));
        rev.push(rev_frame(FD, 300));
        for t in [FD + FD - 1, FD + FD / 2, FD] {
            assert_eq!(rev.frame_at(t, FD).map(|f| f.pts_us), Some(FD), "t = {t}");
        }
        assert_eq!(rev.frame_at(FD - 1, FD).map(|f| f.pts_us), Some(0));
    }

    /// **A seek mid-reverse throws the run away.** Without this the cache would
    /// answer a playhead that jumped somewhere else entirely with whatever its
    /// back happened to be — a frame from the wrong part of the file, which is
    /// worse than the refill it is avoiding.
    #[test]
    fn a_run_that_no_longer_reaches_the_playhead_is_dropped() {
        let mut rev = ReverseCache::default();
        rev.push(rev_frame(0, 300));
        rev.push(rev_frame(FD, 300));
        assert_eq!(rev.frame_at(9 * FD, FD).map(|f| f.pts_us), None);
        assert!(rev.frames.is_empty(), "and it does not hold the memory");
        assert_eq!(rev.bytes, 0);
    }

    /// The budget evicts the **front** — the oldest frames, which are the ones
    /// the next refill can re-decode most cheaply because that pass stops at
    /// the playhead. It never evicts the last frame: one frame is still an
    /// answer, and a budget under one frame would otherwise spin.
    #[test]
    fn the_budget_drops_the_oldest_end_of_the_run() {
        let mut rev = ReverseCache::default();
        let big = REVERSE_BUDGET_BYTES / 4 + 1;
        for i in 0..6 {
            rev.push(rev_frame(i * FD, big));
        }
        assert!(rev.bytes <= REVERSE_BUDGET_BYTES);
        assert!(rev.evicted, "the budget bit, and the caller has to know");
        let held: Vec<i64> = rev.frames.iter().map(|f| f.pts_us).collect();
        assert_eq!(held.last(), Some(&(5 * FD)), "the newest end is kept");
        assert!(held.len() < 6 && !held.is_empty());

        // One frame bigger than the whole budget still leaves that frame.
        let mut one = ReverseCache::default();
        one.push(rev_frame(0, REVERSE_BUDGET_BYTES * 2));
        assert_eq!(one.frames.len(), 1);
    }

    /// Nothing decoded yet never covers. `cur_pts` is `i64::MIN` between a
    /// seek and its first frame, and playback can be running with no pending
    /// `want` at that moment — so `cur_pts - jitter` is reachable and has to be
    /// saturating, or a debug build panics on the overflow and a release build
    /// wraps to a huge positive bound.
    #[test]
    fn nothing_decoded_never_covers() {
        assert!(!frame_covers(i64::MIN, 0, FD, 0));
        assert!(!frame_covers(i64::MIN, i64::MIN, FD, FD));
        assert!(!frame_covers(i64::MIN, i64::MAX, FD, FD));
        // The bound itself must not overflow, independent of the short-circuit.
        assert_eq!(i64::MIN.saturating_sub(FD), i64::MIN);
    }

    /// The tolerance is what stops a sub-frame backward correction from being
    /// read as a seek. Measured jitter on a real clip ran to ~12 ms — inside a
    /// frame, and each one used to cost a full GOP re-decode.
    #[test]
    fn the_jitter_tolerance_absorbs_a_backward_correction() {
        // 12 ms behind the current frame: without tolerance that is a seek.
        assert!(!frame_covers(100_000, 88_000, FD, 0));
        assert!(frame_covers(100_000, 88_000, FD, FD));
        // A full frame back is still absorbed; more than that is a real jump.
        assert!(frame_covers(100_000, 100_000 - FD, FD, FD));
        assert!(!frame_covers(100_000, 100_000 - FD - 1, FD, FD));
    }

    /// Forward playback is the only case that gets the tolerance: a reverse
    /// shuttle and an explicit seek mean the backward move is to be obeyed.
    #[test]
    fn only_forward_playback_tolerates_jitter() {
        assert_eq!(jitter_tolerance(true, 1.0, None, FD), FD);
        assert_eq!(jitter_tolerance(true, 8.0, None, FD), FD);
        // Reverse playback: obey every backward move.
        assert_eq!(jitter_tolerance(true, -1.0, None, FD), 0);
        // Paused: obey.
        assert_eq!(jitter_tolerance(false, 1.0, None, FD), 0);
        // A pending seek/step: land on it exactly, however small the move.
        assert_eq!(jitter_tolerance(true, 1.0, Some(42), FD), 0);
    }

    /// The two ways the timeline's warmed prefetch frame can leave `cur_pts` on
    /// an unpublished frame — short of the target, or more than 2 s past it —
    /// both fall outside the window, so neither can trip the fast path.
    #[test]
    fn a_warm_frame_that_missed_cannot_trip_the_fast_path() {
        let target = 5_000_000;
        // Short of the target (this is also the "dropped late frame" case).
        let short = target - FD;
        assert!(short + FD <= target);
        assert!(!frame_covers(short, target, FD, FD));
        // More than 2 s past it.
        let ahead = target + 2_000_000;
        assert!(!frame_covers(ahead, target, FD, FD));
    }

    fn vsrc(path: &str, offset: i64, speed: f64) -> SegmentSource {
        SegmentSource {
            path: PathBuf::from(path),
            keyframe_index: None,
            proxy: None,
            proxy_index: None,
            source_offset_us: offset,
            speed,
        }
    }

    #[test]
    fn video_decode_prefers_proxy_unless_marked_bad() {
        let mut s = vsrc("orig.mp4", 0, 1.0);
        let mut bad = std::collections::HashSet::new();
        // No proxy: the original, not flagged as proxy.
        let (f, ix, p) = choose_video_file(&s, &bad);
        assert_eq!((f, ix, p), (PathBuf::from("orig.mp4"), None, false));
        // Proxy present: preferred, with ITS index (§4.3).
        s.proxy = Some(PathBuf::from("proxy.mp4"));
        s.proxy_index = Some(PathBuf::from("proxy_index.bin"));
        let (f, ix, p) = choose_video_file(&s, &bad);
        assert_eq!(f, PathBuf::from("proxy.mp4"));
        assert_eq!(ix, Some(PathBuf::from("proxy_index.bin")));
        assert!(p);
        // A proxy that failed to open is skipped without retry.
        bad.insert(PathBuf::from("proxy.mp4"));
        let (f, ix, p) = choose_video_file(&s, &bad);
        assert_eq!((f, ix, p), (PathBuf::from("orig.mp4"), None, false));
    }

    /// `video: Some(path@offset,speed)` or a gap when `path` is `None`.
    fn seg(start: i64, dur: i64, video: Option<(&str, i64, f64)>) -> Segment {
        Segment {
            tl_start_us: start,
            tl_dur_us: dur,
            video: video.map(|(p, o, s)| vsrc(p, o, s)),
            fx: None,
        }
    }

    #[test]
    fn tl_src_mapping_roundtrip_speed1() {
        // offset 5s, tl_start 2s, speed 1: tl 3s → src 6s → back to 3s.
        let src = src_from_tl(5_000_000, 2_000_000, 1.0, 3_000_000);
        assert_eq!(src, 6_000_000);
        assert_eq!(tl_from_src(5_000_000, 2_000_000, 1.0, src), 3_000_000);
    }

    #[test]
    fn tl_src_mapping_with_speed() {
        // speed 2: one timeline second advances two source seconds.
        assert_eq!(src_from_tl(0, 0, 2.0, 1_000_000), 2_000_000);
        assert_eq!(tl_from_src(0, 0, 2.0, 2_000_000), 1_000_000);
        // speed 0.5: source advances half as fast.
        assert_eq!(src_from_tl(1_000_000, 0, 0.5, 1_000_000), 1_500_000);
    }

    #[test]
    fn segment_at_binary_search() {
        let segs = vec![
            seg(0, 1_000_000, Some(("a", 0, 1.0))),
            seg(1_000_000, 2_000_000, None), // gap
            seg(3_000_000, 1_000_000, Some(("b", 0, 1.0))),
        ];
        assert_eq!(segment_at(&segs, 0), Some(0));
        assert_eq!(segment_at(&segs, 999_999), Some(0));
        assert_eq!(segment_at(&segs, 1_000_000), Some(1)); // boundary → next
        assert_eq!(segment_at(&segs, 2_999_999), Some(1));
        assert_eq!(segment_at(&segs, 3_000_000), Some(2));
        assert_eq!(segment_at(&segs, 3_999_999), Some(2));
        assert_eq!(segment_at(&segs, 4_000_000), None); // past the end
        assert_eq!(segment_at(&segs, -1), None);
        assert_eq!(segment_at(&[], 0), None);
    }

    #[test]
    fn prefetch_only_when_playing_forward_near_a_different_file_cut() {
        // cut at 1s from "a" to a different file "b".
        let segs = vec![
            seg(0, 1_000_000, Some(("a", 0, 1.0))),
            seg(1_000_000, 1_000_000, Some(("b", 0, 1.0))),
        ];
        // Paused / reverse → never.
        assert_eq!(prefetch_target(&segs, 0, 500_000, false, 1.0), None);
        assert_eq!(prefetch_target(&segs, 0, 500_000, true, -1.0), None);
        // Just inside the 1 s window → prefetch; exactly 1 s lead is excluded.
        assert_eq!(prefetch_target(&segs, 0, 1, true, 1.0), Some(1));
        assert_eq!(prefetch_target(&segs, 0, 0, true, 1.0), None);
        // Well inside the window.
        assert_eq!(prefetch_target(&segs, 0, 500_000, true, 1.0), Some(1));
    }

    #[test]
    fn prefetch_window_and_continuity() {
        let segs = vec![
            seg(0, 2_000_000, Some(("a", 0, 1.0))),
            seg(2_000_000, 1_000_000, Some(("b", 0, 1.0))),
        ];
        // 1.5s in: cut at 2s, lead 0.5s < 1s → prefetch b.
        assert_eq!(prefetch_target(&segs, 0, 1_500_000, true, 1.0), Some(1));
        // 0.5s in: lead 1.5s ≥ 1s → not yet.
        assert_eq!(prefetch_target(&segs, 0, 500_000, true, 1.0), None);
        // No successor.
        assert_eq!(prefetch_target(&segs, 1, 2_500_000, true, 1.0), None);
    }

    #[test]
    fn prefetch_skips_continuous_same_file_cut() {
        // Same file, positions line up across the cut (a[0..2s] then a[2s..]).
        let cont = vec![
            seg(0, 2_000_000, Some(("a", 0, 1.0))),
            seg(2_000_000, 1_000_000, Some(("a", 2_000_000, 1.0))),
        ];
        assert_eq!(prefetch_target(&cont, 0, 1_500_000, true, 1.0), None);
        // Same file but a discontinuous jump → prefetch (fresh decoder).
        let jump = vec![
            seg(0, 2_000_000, Some(("a", 0, 1.0))),
            seg(2_000_000, 1_000_000, Some(("a", 30_000_000, 1.0))),
        ];
        assert_eq!(prefetch_target(&jump, 0, 1_500_000, true, 1.0), Some(1));
    }

    #[test]
    fn prefetch_skips_gap_successor() {
        let segs = vec![
            seg(0, 2_000_000, Some(("a", 0, 1.0))),
            seg(2_000_000, 1_000_000, None), // gap — nothing to prefetch
        ];
        assert_eq!(prefetch_target(&segs, 0, 1_500_000, true, 1.0), None);
    }

    #[test]
    fn snap_pts_grid() {
        // 25 fps → 40_000 µs grid.
        assert_eq!(snap_pts(41_000, Some(25.0)), 40_000);
        assert_eq!(snap_pts(59_000, Some(25.0)), 40_000);
        assert_eq!(snap_pts(61_000, Some(25.0)), 80_000);
        assert_eq!(snap_pts(41_000, None), 41_000); // no snapping
    }
}
