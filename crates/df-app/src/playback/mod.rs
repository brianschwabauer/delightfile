//! Playing the hovered file (PLAN §4.3, §6, §10's playback checkbox).
//!
//! ```text
//!   cursor lands on a clip ──▶ Prober (ffmpeg, off-thread) ──▶ TemporalInfo
//!                                                                  │
//!            j k l L [ ] < > ──▶ Player ◀── mount, paused ─────────┘
//!                                  │
//!                                  ├─ frame() ──▶ FrameConverter ──▶ TextureId
//!                                  └─ state_at() ──▶ strip::paint
//! ```
//!
//! Four rules hold it together.
//!
//! **It starts paused.** A file manager's preview shows you a file; it does not
//! start playing it at you because the cursor moved. dv-playback publishes the
//! first frame on `set_source` whether or not it is running, so the poster
//! frame is free — and [`AUTOPLAY`] is where the config flag will land when
//! `[preview]` grows one in df-core.
//!
//! **The controller is built lazily and kept.** `Playback::new` opens a cpal
//! device and starts a decode thread; a session that only ever looks at
//! photographs must never pay for either. Once built it stays — it costs about
//! 25 ms and switching files is one `set_source` — but the *source* is dropped
//! after [`MOUNT_GRACE`], so arrowing off a clip really does stop it.
//!
//! **The shuttle ladder is delightviewer's, ported verbatim** —
//! [`shuttle_step`] and [`ladder_rung`] are the same functions with the same
//! tests, because "arrow onto a video and immediately shuttle it" is the whole
//! point of this program's `j`/`k`/`l` and it has to feel identical to the
//! viewer the files get opened in.
//!
//! **Nothing here polls.** dv-playback's controller thread publishes frames
//! with no callback of its own (see "df-core / vendored gaps" in the commit
//! notes), so the event loop stays awake for a frame it *asked for* — a new
//! source, a seek, a step — for at most [`AWAIT_TIMEOUT`], and otherwise only
//! while something is actually playing. A paused video costs the same number of
//! frames as a JPEG: none.

pub mod frame;
pub mod strip;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;
use dv_playback::{Playback, SourceSpec};

pub use frame::{FrameConverter, FrameTex};

/// Ladder ceiling (PLAN §4.3, delightviewer's `SHUTTLE_MAX`).
pub const SHUTTLE_MAX: f64 = 128.0;

/// Minimum time between ladder steps while the key is HELD, so a key-repeat
/// sweeps the ladder instead of exploding to 128× (PLAN §4.3).
pub const SHUTTLE_HOLD_STEP: Duration = Duration::from_millis(550);

/// `<` / `>` skip: ten seconds, delightviewer's number and yazi's habit.
pub const SKIP_US: i64 = 10_000_000;

/// `K` / `J` on a *mounted* clip: five seconds.
///
/// The same 5 the document previewer moves in *lines* (`preview::SEEK_LINES`),
/// because yazi's binding says "seek preview ±5" and the unit is whatever the
/// thing in the pane is measured in — lines for a document, seconds for a clip.
/// Five seconds is also the step every video player on the machine uses for its
/// small skip, so the two readings of the key agree with their own worlds.
pub const SEEK_US: i64 = 5_000_000;

/// How far before the end `]` stops.
///
/// One frame at 25 fps: `]` means "the last frame", and seeking to the duration
/// itself is seeking past the end, which lands on a stopped decoder and a black
/// pane. delightviewer's `seek_edge` uses exactly this.
pub const EDGE_BACKOFF_US: i64 = 40_000;

/// How far before the end a loop asks for its seek back to the start.
///
/// About two frames at 25 fps: long enough that the request is in the
/// controller's hands while the decoder is still running, short enough that
/// what is lost off the end is a frame nobody was looking at.
pub const LOOP_LEAD_US: i64 = 100_000;

/// A wrap already asked for is not asked for again inside this. The position
/// keeps reading "the end" until the seek lands, and a seek per frame while
/// that happens would be a stampede for a decision already made.
const WRAP_DEBOUNCE: Duration = Duration::from_millis(250);

/// How long the event loop stays awake for a frame it has asked for.
///
/// A seek or a step that lands on the frame already on screen republishes an
/// identical one, and a wait that can only end by *noticing a different frame*
/// never ends there — a paused pane would then wake every few milliseconds for
/// the rest of the session. Long enough to cover a cold source open (hardware
/// decoder probe plus trial decode), short enough that a frame that never
/// arrives costs a handful of wake-ups. delightviewer's `AWAIT_TIMEOUT`.
const AWAIT_TIMEOUT: Duration = Duration::from_millis(500);

/// One `↑`/`↓` (or `Shift+↑`/`Shift+↓`) step.
pub const VOLUME_STEP: f32 = 0.1;

/// How far past the source's own level the volume will go — 200%.
///
/// A player whose volume stops at "exactly what is in the file" has no answer
/// for a file that is simply quiet, which is most phone clips and every voice
/// memo. dv-playback's mixer has the soft knee that makes the top of this range
/// listenable rather than square.
pub const VOLUME_MAX: f32 = 2.0;

/// **Does a mounted clip start playing?** No.
///
/// A file manager previewing a file is a quick look, not a player: forty
/// arrow-downs through a folder of clips would be forty soundtracks starting.
/// The constant exists rather than the behaviour simply being absent because
/// PLAN §3's `[preview]` config table is where this belongs the moment df-core
/// grows one — at which point this becomes its default and nothing else here
/// changes.
pub const AUTOPLAY: bool = false;

/// How long a mounted source survives the cursor leaving it.
///
/// Two seconds, and it is a *thrash* guard rather than a feature: flicking down
/// through a directory of clips with the key held passes each one in tens of
/// milliseconds, and tearing a decoder down and building it back up at that
/// rate is how a file manager stutters. Audio stops the instant the cursor
/// moves — [`Player::leave`] pauses — so what the grace buys is only that
/// coming *back* inside two seconds is instant. Long enough to cover a
/// mis-keyed `↓ ↑`, short enough that a decoder is never left holding a file
/// for a directory you have walked away from.
pub const MOUNT_GRACE: Duration = Duration::from_secs(2);

/// Next shuttle rate for a `j`/`l` press — `None` means "keep the current
/// rate", which is how a held key is paced.
///
/// Ported **verbatim** from delightviewer's `playback::shuttle_step`, which
/// ported it from delightvideo: this is the one function in the program whose
/// feel is a contract with two other programs, and it keeps its tests with it.
/// `held` is "this press came from key repeat".
pub fn shuttle_step(
    current: f64,
    dir: f64,
    held: bool,
    last_bump: &mut Instant,
    now: Instant,
) -> Option<f64> {
    if current * dir > 0.0 {
        if held && now.duration_since(*last_bump) < SHUTTLE_HOLD_STEP {
            return None;
        }
        *last_bump = now;
        Some((current.abs() * 2.0).min(SHUTTLE_MAX))
    } else {
        *last_bump = now;
        Some(1.0)
    }
}

/// **Where a `j`/`l` press starts from** — the rung the ladder is standing on,
/// or 0 for "off the ladder entirely", which is what makes the next press 1×.
///
/// The convention is one convention: **a pause takes you off the ladder**.
/// Press `l` four times to 8×, press `k`, and `l` plays at 1× again — which is
/// what jkl has meant since the first tape machine had those three keys on it.
///
/// `playing` is the transport's own answer and covers every way of leaving the
/// ladder — a pause, running off the end, a frame step. `off_ladder` is the
/// hardening for the one window where `playing` can lie: it is reported by the
/// controller thread, and opening a file costs that thread a hardware-decoder
/// probe and a trial decode. A `k` and an `l` inside that window would both
/// read the pre-pause state; the key that asked for the pause knows, so it says
/// so.
pub fn ladder_rung(playing: bool, rate: f64, off_ladder: bool) -> f64 {
    if off_ladder || !playing {
        0.0
    } else {
        rate
    }
}

/// How far before the end the loop asks for its wrap on a clip of
/// `duration_us` — [`LOOP_LEAD_US`], or nothing at all.
///
/// The lead is an *anticipation*, and it only means anything on a clip several
/// times longer than it. On a shorter one every position in the file is "near
/// the end", so the loop would ask for a wrap once per [`WRAP_DEBOUNCE`] for as
/// long as the file is open — a seek storm on a clip that is not ending.
pub fn loop_lead(duration_us: i64) -> i64 {
    if duration_us > LOOP_LEAD_US * 4 {
        LOOP_LEAD_US
    } else {
        0
    }
}

/// **Is this the end of the file, and does the loop own it?**
///
/// `was_playing` is the previous poll's answer and `user_paused` is "a key
/// asked for this stop". Stopped on the last frame is stopped on the last frame
/// whether the decoder ran out or somebody pressed `k` two frames early, and
/// only the key knows which — so the key is asked.
pub fn wants_wrap(
    position_us: i64,
    duration_us: i64,
    playing: bool,
    rate: f64,
    was_playing: bool,
    user_paused: bool,
) -> bool {
    if duration_us <= 0 || user_paused {
        return false;
    }
    if position_us < duration_us - loop_lead(duration_us) {
        return false;
    }
    if playing {
        rate > 0.0
    } else {
        was_playing
    }
}

/// What `K` / `J` means, which depends on what is in the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSeek {
    /// A document: five lines, which is yazi's own binding and its own number.
    Lines(isize),
    /// A mounted clip: five seconds, because five *lines* of a video is not a
    /// quantity.
    Micros(i64),
}

/// yazi's "seek preview ±5", read in the unit the pane is actually measured in
/// (PLAN §4.1, §10). `mounted` is "there is a transport on this file".
///
/// A function rather than a branch inside the command handler so the split — the
/// one place two behaviours share a key — is stated once and tested.
pub fn list_seek(down: bool, mounted: bool) -> ListSeek {
    let sign = if down { 1 } else { -1 };
    if mounted {
        ListSeek::Micros(sign as i64 * SEEK_US)
    } else {
        ListSeek::Lines(sign as isize * crate::preview::SEEK_LINES as isize)
    }
}

/// What `dv-media` found in the file: enough to open it and enough to label it.
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalInfo {
    pub has_video: bool,
    pub has_audio: bool,
    pub duration_us: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Degrees **clockwise** the decoded frame has to be turned to sit upright
    /// — the container's display matrix, which every phone writes into a
    /// portrait clip and which dv-playback hands out frames *without* applying
    /// (`dv_media::ProbeInfo::rotation`). The pane inverts it into the four
    /// UVs it draws the frame with, so the frame lands on the same rectangle
    /// the poster did (`preview::oriented_uvs`).
    pub rotation: u32,
    /// The same matrix's left-to-right flip, applied to the source *before*
    /// the rotation.
    pub mirrored: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub sample_rate: Option<u32>,
}

impl TemporalInfo {
    /// Is there anything here a transport could act on? A "video" whose only
    /// stream failed to open is not a clip, and mounting it would give the pane
    /// a transport with nothing behind it.
    pub fn playable(&self) -> bool {
        self.has_video || self.has_audio
    }
}

/// One finished probe.
pub struct Probed {
    pub path: PathBuf,
    /// `None` when the file is not something dv-media can open — a `.mp3` that
    /// is actually a text file, a truncated download. The pane then keeps the
    /// still-image treatment it already had, which is honest.
    pub info: Option<TemporalInfo>,
}

/// Asking ffmpeg what a file is, off the paint thread.
///
/// Opening a container costs a demuxer probe — milliseconds on a warm local
/// file, and unbounded on a sleeping disk or a dead NFS mount — so it is a
/// worker like every other worker in this program, woken by a channel and
/// ringing the same [`crate::Wake`] bell (PLAN §1). Newest wins, by the same
/// `AtomicU64` token trick [`crate::preview::decode`] uses: a held `↓` asks for
/// forty probes and wants one.
pub struct Prober {
    jobs: Sender<(u64, PathBuf)>,
    results: Receiver<Probed>,
    live: Arc<AtomicU64>,
    next: u64,
}

impl Prober {
    pub fn start(notify: Notifier) -> Prober {
        let (jobs, rx) = unbounded::<(u64, PathBuf)>();
        let (tx, results) = unbounded::<Probed>();
        let live = Arc::new(AtomicU64::new(0));
        let worker_live = Arc::clone(&live);
        std::thread::Builder::new()
            .name("df-probe".into())
            .spawn(move || {
                // An ffmpeg probe is somebody's cursor waiting on a demuxer;
                // it must not preempt the paint thread (`df_core::thread`).
                df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                for (token, path) in rx {
                    if worker_live.load(Ordering::Relaxed) != token {
                        continue;
                    }
                    let info = dv_media::probe(&path).ok().map(|p| TemporalInfo {
                        has_video: p.video_codec.is_some(),
                        has_audio: p.has_audio || p.audio_codec.is_some(),
                        duration_us: p.duration_us.unwrap_or(0),
                        width: p.width,
                        height: p.height,
                        rotation: p.rotation,
                        mirrored: p.mirrored,
                        video_codec: p.video_codec,
                        audio_codec: p.audio_codec,
                        sample_rate: p.sample_rate,
                    });
                    if worker_live.load(Ordering::Relaxed) != token {
                        continue;
                    }
                    if tx.send(Probed { path, info }).is_err() {
                        return;
                    }
                    notify();
                }
            })
            // A failed spawn is a machine out of threads; the pane simply never
            // gets a transport, which is the same as a file ffmpeg cannot open.
            .ok();
        Prober {
            jobs,
            results,
            live,
            next: 0,
        }
    }

    /// Ask about `path`, superseding whatever was asked before.
    pub fn probe(&mut self, path: &Path) {
        self.next += 1;
        self.live.store(self.next, Ordering::Relaxed);
        let _ = self.jobs.send((self.next, path.to_path_buf()));
    }

    pub fn drain(&self) -> impl Iterator<Item = Probed> + '_ {
        self.results.try_iter()
    }
}

/// What the transport strip needs to draw itself.
#[derive(Clone, Copy)]
pub struct TransportState {
    pub position_us: i64,
    pub duration_us: i64,
    pub playing: bool,
    pub rate: f64,
    pub muted: bool,
    pub volume: f32,
    pub looping: bool,
    /// A clip with no picture: the strip draws the audio card's line instead of
    /// sitting over a frame.
    pub has_video: bool,
}

/// The file the controller currently has open.
struct Active {
    path: PathBuf,
    info: TemporalInfo,
}

/// The whole temporal-media side of the preview pane.
pub struct Player {
    pb: Playback,
    converter: FrameConverter,
    active: Option<Active>,
    volume: f32,
    muted: bool,
    /// `L`. The app's, not the controller's: it is a property of this sitting,
    /// and it survives moving between files the way a volume does.
    looping: bool,
    /// **The `k` the controller has not answered yet.** Set by every key that
    /// takes the transport off the shuttle ladder and cleared by the next
    /// `j`/`l` — see [`ladder_rung`].
    off_ladder: bool,
    last_bump: Instant,
    /// Last pointer/transport activity — the strip's auto-hide clock.
    activity_at: Instant,
    /// The frame currently converted and on screen (pts, source serial).
    shown: Option<(i64, u64)>,
    /// The identity of the *publication* that frame came from — a republished
    /// frame is a new `Arc` even when its pts and serial are already on screen,
    /// which is exactly what a seek onto the current frame produces.
    published: Option<usize>,
    /// A decode is expected to land shortly (a new source, a seek, a step) —
    /// until this instant. See [`AWAIT_TIMEOUT`].
    awaiting_until: Option<Instant>,
    playhead: Playhead,
    /// Was it playing last time the loop looked? The difference between "it ran
    /// off the end" and "somebody parked it there".
    was_playing: bool,
    /// **A pause this application asked for**, as opposed to the controller
    /// running out of file.
    user_paused: bool,
    wrapped_at: Option<Instant>,
    /// When the cursor moved off this file, if it has — the [`MOUNT_GRACE`]
    /// clock. `None` while the file is the hovered one.
    left_at: Option<Instant>,
}

/// The playhead, interpolated between the controller's updates.
///
/// A video-only clip's position advances one *frame* at a time — 40 ms at
/// 25 fps — so a bar driven straight off it visibly hops. This carries the last
/// reported position and when it arrived, and hands the strip a wall-clock
/// interpolation between the two, capped at one observed step so a stall shows
/// as a stall rather than as a bar that keeps sailing on. Every seek reads
/// [`Playback::position_us`], which is the controller's own answer, unsmoothed.
#[derive(Debug, Clone, Copy)]
struct Playhead {
    raw: i64,
    at: Instant,
    /// What was last shown, so the bar never walks backwards mid-play.
    shown: i64,
    /// The observed gap between updates — one frame, in practice.
    step_us: i64,
}

/// A jump larger than this is a seek, not playback: the smoothing restarts
/// rather than gliding across it.
const PLAYHEAD_JUMP_US: i64 = 500_000;

impl Playhead {
    fn new(now: Instant) -> Playhead {
        Playhead {
            raw: 0,
            at: now,
            shown: 0,
            step_us: 40_000,
        }
    }

    fn update(&mut self, raw: i64, now: Instant, playing: bool, rate: f64) -> i64 {
        if raw != self.raw {
            let delta = raw - self.raw;
            if delta.abs() < PLAYHEAD_JUMP_US {
                // A slow average, so one late frame does not widen the cap.
                self.step_us = ((self.step_us * 3 + delta.abs()) / 4).clamp(1_000, 200_000);
            } else {
                // A seek: land on it exactly, and start again from there.
                self.shown = raw;
            }
            self.raw = raw;
            self.at = now;
        }
        if !playing || rate == 0.0 {
            self.shown = raw;
            return raw;
        }
        let elapsed_us = now.saturating_duration_since(self.at).as_micros() as f64;
        let target = raw + (elapsed_us * rate) as i64;
        // Never more than one frame ahead of what has actually been decoded,
        // and never backwards while running forwards. Written as two
        // independent bounds rather than one `clamp`, because the two can
        // genuinely cross — the controller's position can step *back* a little
        // while `shown` is out ahead of it — and `clamp` panics on `min > max`.
        self.shown = if rate > 0.0 {
            let cap = raw + self.step_us;
            target.min(cap).max(self.shown.min(cap))
        } else {
            let floor = raw - self.step_us;
            target.max(floor).min(self.shown.max(floor))
        };
        self.shown
    }
}

impl Player {
    /// Build the controller. Called on the first temporal file, never before.
    pub fn new(
        device: &egui_wgpu::wgpu::Device,
        queue: &egui_wgpu::wgpu::Queue,
        now: Instant,
    ) -> Player {
        let t = Instant::now();
        let pb = Playback::new(device.clone(), queue.clone());
        log::info!("playback started (cpal + controller in {:?})", t.elapsed());
        Player {
            converter: FrameConverter::new(device),
            pb,
            active: None,
            volume: 1.0,
            muted: false,
            looping: false,
            off_ladder: true,
            last_bump: now,
            activity_at: now,
            shown: None,
            published: None,
            awaiting_until: None,
            playhead: Playhead::new(now),
            was_playing: false,
            user_paused: false,
            wrapped_at: None,
            left_at: None,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.active.as_ref().map(|a| a.path.as_path())
    }

    pub fn info(&self) -> Option<&TemporalInfo> {
        self.active.as_ref().map(|a| &a.info)
    }

    pub fn has_video(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.info.has_video)
    }

    /// Open `path` — **paused, on its first frame** ([`AUTOPLAY`]).
    pub fn open(&mut self, path: &Path, info: &TemporalInfo, now: Instant) {
        if self.active.as_ref().is_some_and(|a| a.path == path) {
            self.left_at = None;
            return;
        }
        self.pb.set_source(
            SourceSpec {
                path: path.to_path_buf(),
                keyframe_index: None,
                proxy: None,
                proxy_index: None,
                has_video: info.has_video,
                has_audio: info.has_audio,
                snap_fps: None,
            },
            0,
            AUTOPLAY,
        );
        // No `set_rate(1.0)` here, however tempting: dv-playback's `SetRate`
        // *starts playback* as a side effect, and `SetSource` already resets
        // the rate to 1×. Calling it would auto-play every file the cursor
        // touched.
        self.active = Some(Active {
            path: path.to_path_buf(),
            info: info.clone(),
        });
        self.activity_at = now;
        self.shown = None;
        self.published = None;
        self.await_frame();
        // A new file is a new ladder — `SetSource` puts the controller's rate
        // back to 1× and the app must not think it is still at 8×.
        self.off_ladder = true;
        self.playhead = Playhead::new(now);
        self.was_playing = false;
        self.user_paused = false;
        self.wrapped_at = None;
        self.left_at = None;
        log::info!(
            "playback source {} ({} µs, video {}, audio {})",
            path.display(),
            info.duration_us,
            info.has_video,
            info.has_audio
        );
    }

    /// The cursor is back on (or still on) the open file: cancel the grace
    /// clock without touching the transport.
    ///
    /// Separate from [`Player::open`] because coming back to a file must not
    /// resume it — what the hand left paused stays paused — and because the
    /// probe that opened it may have aged out of the app's small ring by then.
    pub fn stay(&mut self) {
        self.left_at = None;
    }

    /// The cursor moved off this file: **stop the sound now**, and start the
    /// [`MOUNT_GRACE`] clock on the source itself.
    pub fn leave(&mut self, now: Instant) {
        if self.active.is_none() || self.left_at.is_some() {
            return;
        }
        self.pb.pause();
        self.off_ladder = true;
        self.user_paused = true;
        self.left_at = Some(now);
    }

    /// Has the grace run out on a file the cursor has left?
    pub fn grace_expired(&self, now: Instant) -> bool {
        self.left_at
            .is_some_and(|at| now.saturating_duration_since(at) >= MOUNT_GRACE)
    }

    /// When the grace ends, so the event loop can schedule the one wake-up that
    /// tears the source down instead of polling for it.
    pub fn grace_deadline(&self) -> Option<Instant> {
        self.left_at.map(|at| at + MOUNT_GRACE)
    }

    /// Stop playing and forget the source — moving to a still must not leave
    /// audio running behind the picture, and a texture the size of a 4K frame
    /// must not outlive the file it came from.
    pub fn close(&mut self, renderer: &mut egui_wgpu::Renderer) {
        self.shown = None;
        self.published = None;
        self.awaiting_until = None;
        self.left_at = None;
        if self.active.take().is_some() {
            self.pb.pause();
            self.pb.clear_source();
        }
        self.converter.clear(renderer);
    }

    pub fn state(&self) -> TransportState {
        TransportState {
            position_us: self.pb.position_us(),
            // The container's header when it has one, since dv-playback reports
            // 0 for a source it has not finished opening and a bar with no
            // length is not a bar.
            duration_us: match self.pb.duration_us() {
                d if d > 0 => d,
                _ => self.info().map(|i| i.duration_us).unwrap_or(0),
            },
            playing: self.pb.is_playing(),
            rate: self.pb.rate(),
            muted: self.muted,
            volume: self.volume,
            looping: self.looping,
            has_video: self.has_video(),
        }
    }

    /// The state to *draw*: the same thing, with the playhead interpolated
    /// between decoded frames so the bar glides instead of hopping.
    pub fn state_at(&mut self, now: Instant) -> TransportState {
        let mut state = self.state();
        let shown = self
            .playhead
            .update(state.position_us, now, state.playing, state.rate);
        state.position_us = if state.duration_us > 0 {
            shown.clamp(0, state.duration_us)
        } else {
            shown.max(0)
        };
        state
    }

    pub fn is_playing(&self) -> bool {
        self.active.is_some() && self.pb.is_playing()
    }

    /// True while a requested frame has not arrived yet.
    pub fn awaiting_frame(&self) -> bool {
        self.awaiting_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// Stay awake for the frame just asked for. Audio-only has no frame coming,
    /// so it never waits for one.
    fn await_frame(&mut self) {
        self.awaiting_until = self.has_video().then(|| Instant::now() + AWAIT_TIMEOUT);
    }

    /// The decoded frame, converted to something the pane can draw.
    pub fn frame(
        &mut self,
        device: &egui_wgpu::wgpu::Device,
        queue: &egui_wgpu::wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
    ) -> Option<FrameTex> {
        if !self.has_video() {
            return None;
        }
        let frame = self.pb.current_frame()?;
        let key = (frame.pts_us, frame.source_serial);
        // The *publication*, not the picture: a seek onto the frame already on
        // screen — `[` at 0, `,` on the first frame — arrives as a new `Arc`
        // carrying the pts and serial that are already up, and a wait that only
        // ends on a changed key never ends for it.
        let published = Arc::as_ptr(&frame) as usize;
        if self.shown != Some(key) || self.published != Some(published) {
            self.shown = Some(key);
            self.published = Some(published);
            self.awaiting_until = None;
        }
        Some(self.converter.convert(device, queue, renderer, &frame))
    }

    // ── Transport commands ─────────────────────────────────────────────────

    pub fn note_activity(&mut self, now: Instant) {
        self.activity_at = now;
    }

    /// How visible the strip is at `now`, 0–1.
    ///
    /// Held while anything is happening, then eased away — PLAN §8's "linger
    /// then leave", with delightviewer's [`strip::LINGER`] and the plan's own
    /// 500 ms fade. A **paused** player keeps its strip: the fade is what a
    /// video being *watched* earns, and a still frame with no controls under it
    /// is a picture nobody can tell is a video.
    pub fn strip_alpha(&self, now: Instant) -> f32 {
        if !self.pb.is_playing() {
            return 1.0;
        }
        strip::linger_alpha(self.activity_at, now)
    }

    /// The moment the strip finishes fading, so the repaint policy can stop
    /// asking for frames once it is gone.
    pub fn strip_deadline(&self) -> Instant {
        self.activity_at + strip::LINGER + strip::FADE
    }

    pub fn play_pause(&mut self, now: Instant) {
        self.note_activity(now);
        // One command, resolved on the controller thread — *not* a local
        // `if is_playing() { pause } else { play }`. `is_playing` reports what
        // the controller last processed, and opening a file costs it a
        // hardware-decoder probe: a second press inside that window would read
        // the stale answer and send the same command twice.
        self.pb.toggle_play();
        // **Either way this key went, it is a step off the shuttle ladder.**
        self.off_ladder = true;
        // And for the loop, the same key either way is the same claim: wherever
        // the playhead is left, a *hand* left it there.
        self.user_paused = true;
    }

    pub fn shuttle(&mut self, dir: f64, held: bool, now: Instant) {
        self.note_activity(now);
        let current = ladder_rung(self.pb.is_playing(), self.pb.rate(), self.off_ladder);
        self.off_ladder = false;
        // A shuttle is an unambiguous "run it": whatever pause was parked here
        // is over, and the loop owns the end of the file again.
        self.user_paused = false;
        let mut landed = current;
        if let Some(next) = shuttle_step(current, dir, held, &mut self.last_bump, now) {
            landed = dir * next;
            self.pb.set_rate(landed);
        }
        self.pb.play();
        // Logged rather than read back, for `play_pause`'s reason: `pb.rate()`
        // is what the controller last *processed*, and this is what it has just
        // been asked for.
        log::debug!("shuttle {landed}× at {} µs", self.pb.position_us());
    }

    /// **The loop's wrap** (`L`), asked every frame while the loop is on.
    ///
    /// It is *anticipated*, not reacted to: dv-playback's answer to running out
    /// of file is to pause and clamp, so a loop built on noticing that has the
    /// stop inside it — the audio ring drains and the restart is a cold seek
    /// from a standstill. Asking [`LOOP_LEAD_US`] early means the request is in
    /// flight while the pipeline is still running. The second branch is the
    /// belt: a stall, or a shuttle rate that steps clean over the lead.
    pub fn wrap_if_ending(&mut self, now: Instant) -> bool {
        if !self.looping {
            return false;
        }
        let playing = self.pb.is_playing();
        let was = std::mem::replace(&mut self.was_playing, playing);
        let duration = self.state().duration_us;
        if self.active.is_none() || duration <= 0 {
            return false;
        }
        let position = self.pb.position_us();
        // Running, and nowhere near the end: whatever pause a key last asked
        // for has been answered and overtaken.
        if playing && position < duration - loop_lead(duration) {
            self.user_paused = false;
        }
        if self
            .wrapped_at
            .is_some_and(|at| now.saturating_duration_since(at) < WRAP_DEBOUNCE)
        {
            return false;
        }
        if !wants_wrap(
            position,
            duration,
            playing,
            self.pb.rate(),
            was,
            self.user_paused,
        ) {
            return false;
        }
        self.wrapped_at = Some(now);
        self.seek_to(0);
        if !playing {
            self.pb.play();
            self.was_playing = true;
        }
        true
    }

    pub fn toggle_loop(&mut self, now: Instant) -> bool {
        self.note_activity(now);
        self.looping = !self.looping;
        self.looping
    }

    pub fn skip(&mut self, delta_us: i64, now: Instant) {
        self.note_activity(now);
        self.seek_to(self.pb.position_us() + delta_us);
    }

    /// `[` / `]`.
    ///
    /// A single file's edges are its start and its last frame. **Chapters are
    /// deferred**: `dv_media::ProbeInfo` already carries them and PLAN §4.3
    /// promises "chapters if present", but a chapter list is a second model in
    /// the strip (marks on the bar, a name in the badge) and it belongs in its
    /// own commit. Until then `[` is the start and `]` is
    /// [`EDGE_BACKOFF_US`] before the end, which is what a single-chapter file
    /// means by both anyway.
    pub fn seek_edge(&mut self, end: bool, now: Instant) {
        self.note_activity(now);
        let to = if end {
            (self.state().duration_us - EDGE_BACKOFF_US).max(0)
        } else {
            0
        };
        self.seek_to(to);
    }

    pub fn step(&mut self, frames: i64, now: Instant) {
        self.note_activity(now);
        if self.pb.is_playing() {
            self.pb.pause();
        }
        // A frame step is a pause with a destination, so it leaves the ladder
        // for `play_pause`'s reason and by the same one line.
        self.off_ladder = true;
        // A step parks the playhead on purpose, the last frame included.
        self.user_paused = true;
        self.await_frame();
        self.pb.step(frames);
    }

    pub fn seek_to(&mut self, us: i64) {
        let dur = self.state().duration_us;
        self.await_frame();
        // **An unknown duration is not a duration of zero.** A container with no
        // duration header reports 0, and clamping into `0..=0` would pin every
        // skip and `]` to the head of the file. Ask for the target instead;
        // running out of file is the decoder's own answer.
        let to = if dur > 0 { us.clamp(0, dur) } else { us.max(0) };
        self.pb.seek(to);
    }

    pub fn toggle_mute(&mut self, now: Instant) -> bool {
        self.note_activity(now);
        self.muted = !self.muted;
        self.pb.set_muted(self.muted);
        self.muted
    }

    pub fn nudge_volume(&mut self, delta: f32, now: Instant) -> f32 {
        self.note_activity(now);
        self.volume = (self.volume + delta).clamp(0.0, VOLUME_MAX);
        self.pb.set_volume(self.volume);
        // Nudging the volume up is also how you unmute — a level nobody can
        // hear because of a mute they forgot about is a bug report.
        if self.muted && delta > 0.0 {
            self.muted = false;
            self.pb.set_muted(false);
        }
        self.volume
    }

    /// On quit: stop the sound before the window goes, and join the controller.
    pub fn shutdown(&mut self) {
        self.pb.pause();
        self.pb.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The ladder, with delightviewer's own tests ──────────────────────────

    /// **A pause takes you off the ladder.** The rung is what a `j`/`l`
    /// doubles, so a rung of zero is what makes the next press 1× — and every
    /// way of stopping reports one.
    #[test]
    fn a_pause_puts_the_ladder_back_on_its_bottom_rung() {
        // Playing along at 8×, `l` doubles: nothing here changed.
        assert_eq!(ladder_rung(true, 8.0, false), 8.0);
        assert_eq!(ladder_rung(true, -4.0, false), -4.0);
        // Stopped — paused, stepped, run off the end — is off the ladder, and
        // whatever rate the controller is still carrying is not a rung.
        assert_eq!(ladder_rung(false, 8.0, false), 0.0);
        // **And the flag is the window `playing` cannot cover**: the pause is
        // processed on the controller thread and leaves the rate where it was.
        assert_eq!(ladder_rung(true, 8.0, true), 0.0);
        // A rung of zero is a fresh ladder in either direction.
        let now = Instant::now();
        let mut last = now;
        let off = ladder_rung(true, 8.0, true);
        assert_eq!(shuttle_step(off, 1.0, false, &mut last, now), Some(1.0));
        assert_eq!(shuttle_step(off, -1.0, false, &mut last, now), Some(1.0));
        assert_eq!(
            shuttle_step(ladder_rung(true, 8.0, false), 1.0, false, &mut last, now),
            Some(16.0),
            "and an uninterrupted ladder still doubles"
        );
    }

    #[test]
    fn discrete_presses_double_up_the_ladder_to_128() {
        let now = Instant::now();
        let mut last = now;
        let mut rate = 0.0;
        let mut seen = Vec::new();
        for _ in 0..10 {
            rate = shuttle_step(rate, 1.0, false, &mut last, now).unwrap_or(rate);
            seen.push(rate);
        }
        assert_eq!(
            seen,
            vec![1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 128.0, 128.0]
        );
    }

    #[test]
    fn a_direction_change_restarts_at_one() {
        let now = Instant::now();
        let mut last = now;
        let mut rate = shuttle_step(0.0, 1.0, false, &mut last, now).expect("start");
        rate = shuttle_step(rate, 1.0, false, &mut last, now).expect("double");
        assert_eq!(rate, 2.0);
        // Reversing from +2× is 1× the other way, not 4×.
        let back = shuttle_step(rate, -1.0, false, &mut last, now).expect("reverse");
        assert_eq!(back, 1.0);
    }

    #[test]
    fn a_held_key_is_paced_to_one_doubling_per_step() {
        let t0 = Instant::now();
        let mut last = t0;
        let rate = shuttle_step(0.0, 1.0, true, &mut last, t0).expect("start");
        assert_eq!(rate, 1.0);
        // Key repeat arrives every ~30 ms; it must not climb yet.
        assert_eq!(
            shuttle_step(rate, 1.0, true, &mut last, t0 + Duration::from_millis(30)),
            None
        );
        assert_eq!(
            shuttle_step(rate, 1.0, true, &mut last, t0 + SHUTTLE_HOLD_STEP),
            Some(2.0)
        );
    }

    /// A discrete press is never paced — that is the whole difference.
    #[test]
    fn discrete_presses_are_not_paced() {
        let t0 = Instant::now();
        let mut last = t0;
        let rate = shuttle_step(0.0, 1.0, false, &mut last, t0).expect("start");
        assert_eq!(
            shuttle_step(rate, 1.0, false, &mut last, t0 + Duration::from_millis(5)),
            Some(2.0)
        );
    }

    // ── The playhead ────────────────────────────────────────────────────────

    #[test]
    fn the_playhead_interpolates_between_decoded_frames() {
        let t0 = Instant::now();
        let mut p = Playhead::new(t0);
        p.update(0, t0, true, 1.0);
        p.update(40_000, t0 + Duration::from_millis(40), true, 1.0);
        let mid = p.update(40_000, t0 + Duration::from_millis(60), true, 1.0);
        assert!(mid > 40_000 && mid < 80_000, "{mid}");
        let stalled = p.update(40_000, t0 + Duration::from_secs(2), true, 1.0);
        assert!(stalled <= 40_000 + p.step_us, "{stalled}");
    }

    #[test]
    fn the_playhead_is_exact_when_it_matters() {
        let t0 = Instant::now();
        let mut p = Playhead::new(t0);
        assert_eq!(p.update(1_234_567, t0, false, 1.0), 1_234_567);
        p.update(0, t0, true, 1.0);
        p.update(20_000, t0 + Duration::from_millis(20), true, 1.0);
        let after_seek = p.update(9_000_000, t0 + Duration::from_millis(21), true, 1.0);
        assert!(after_seek >= 9_000_000, "{after_seek}");
        assert!(after_seek < 9_100_000, "{after_seek}");
        let mut last = after_seek;
        for ms in 22..40 {
            let now = t0 + Duration::from_millis(ms);
            let shown = p.update(9_000_000, now, true, 1.0);
            assert!(shown >= last, "{shown} < {last}");
            last = shown;
        }
    }

    /// The controller's position can step **back** a little without that being
    /// a seek — a correction, or the first frames after a resume — while the
    /// interpolation is still out ahead of it. That crossed the two bounds of
    /// what used to be one `clamp`, and `clamp` panics on `min > max`.
    #[test]
    fn a_small_backwards_correction_does_not_panic() {
        let t0 = Instant::now();
        let mut p = Playhead::new(t0);
        p.update(0, t0, true, 1.0);
        p.update(40_000, t0 + Duration::from_millis(40), true, 1.0);
        let ahead = p.update(40_000, t0 + Duration::from_millis(70), true, 1.0);
        assert!(ahead > 40_000, "{ahead}");
        let back = p.update(20_000, t0 + Duration::from_millis(71), true, 1.0);
        assert!(back <= 20_000 + p.step_us, "{back}");
        let mut p = Playhead::new(t0);
        p.update(1_000_000, t0, true, -2.0);
        p.update(960_000, t0 + Duration::from_millis(40), true, -2.0);
        let behind = p.update(960_000, t0 + Duration::from_millis(70), true, -2.0);
        let forward = p.update(1_000_000, t0 + Duration::from_millis(71), true, -2.0);
        assert!(forward >= 1_000_000 - p.step_us, "{behind} then {forward}");
    }

    // ── The loop ────────────────────────────────────────────────────────────

    /// **A `k` two frames from the end is not "it ran off the end".**
    #[test]
    fn a_user_pause_at_the_end_is_not_the_end_of_the_file() {
        let dur = 10_000_000;
        let at_end = dur - 20_000;
        assert!(wants_wrap(at_end, dur, false, 1.0, true, false));
        assert!(!wants_wrap(at_end, dur, false, 1.0, true, true));
        assert!(!wants_wrap(at_end, dur, true, 1.0, true, true));
        assert!(!wants_wrap(at_end, dur, false, 1.0, false, false));
        assert!(wants_wrap(at_end, dur, true, 1.0, true, false));
        assert!(!wants_wrap(at_end, dur, true, -2.0, true, false));
        let short = dur - LOOP_LEAD_US - 1;
        assert!(!wants_wrap(short, dur, true, 1.0, true, false));
    }

    /// A clip shorter than the lead is "ending" at every position in it, which
    /// would fire a seek every debounce tick for as long as it was open.
    #[test]
    fn a_clip_shorter_than_the_lead_does_not_storm() {
        assert_eq!(loop_lead(10_000_000), LOOP_LEAD_US);
        assert_eq!(loop_lead(LOOP_LEAD_US * 4 + 1), LOOP_LEAD_US);
        assert_eq!(loop_lead(LOOP_LEAD_US * 4), 0);
        assert_eq!(loop_lead(60_000), 0);
        let dur = 60_000;
        assert!(!wants_wrap(0, dur, true, 1.0, true, false));
        assert!(!wants_wrap(dur - 1, dur, true, 1.0, true, false));
        assert!(wants_wrap(dur, dur, false, 1.0, true, false));
        assert!(!wants_wrap(0, 0, true, 1.0, true, false));
    }

    /// **One key, two units.** `K`/`J` moves a document by lines and a clip by
    /// seconds, and five of each is what yazi's binding and every video player
    /// on the machine respectively mean by "a small step".
    #[test]
    fn seek_preview_is_lines_on_a_document_and_seconds_on_a_clip() {
        assert_eq!(list_seek(true, false), ListSeek::Lines(5));
        assert_eq!(list_seek(false, false), ListSeek::Lines(-5));
        assert_eq!(list_seek(true, true), ListSeek::Micros(SEEK_US));
        assert_eq!(list_seek(false, true), ListSeek::Micros(-SEEK_US));
        // The document step is the preview pane's own constant, not a second
        // copy of the number.
        assert_eq!(
            list_seek(true, false),
            ListSeek::Lines(crate::preview::SEEK_LINES as isize)
        );
    }

    /// A probe with nothing decodable in it is not a clip, whatever the name
    /// said — the pane keeps its still-image treatment rather than growing a
    /// transport with no transport behind it.
    #[test]
    fn a_file_with_no_streams_is_not_playable() {
        let none = TemporalInfo {
            has_video: false,
            has_audio: false,
            duration_us: 0,
            width: None,
            height: None,
            rotation: 0,
            mirrored: false,
            video_codec: None,
            audio_codec: None,
            sample_rate: None,
        };
        assert!(!none.playable());
        assert!(TemporalInfo {
            has_audio: true,
            ..none.clone()
        }
        .playable());
        assert!(TemporalInfo {
            has_video: true,
            ..none
        }
        .playable());
    }
}
