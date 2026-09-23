//! Audio output (§5): cpal stream + the audio render thread.
//!
//! The render thread decodes ~200 ms ahead of the audio clock into the
//! [`AudioRing`]; the cpal callback only copies out of it. The ring's consumed
//! counter drives the master clock (§4.4): while audio is audible, the
//! controller's wall-clock anchor is periodically corrected from it.
//!
//! Rate handling (M2): audible at forward rates ≤ 2× (nearest-neighbor
//! resample — 2× sounds chipmunk, per §4.4 pitch is not corrected); muted
//! above 2× and in reverse. The device is opened at the ring's 48 kHz when it
//! will take it; when it will not, the callback interpolates (see
//! [`Resampler`]) and maps the ring's stereo onto whatever channels it has.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};
use dv_media::{AudioDecoder, AUDIO_RATE};

use crate::dsp::Limiter;
use crate::ring::AudioRing;

/// How far ahead of the clock the render thread keeps the ring (§5).
const RENDER_AHEAD: Duration = Duration::from_millis(200);
/// Audio is audible at forward rates up to this (§4.4).
pub const MAX_AUDIBLE_RATE: f64 = 2.0;

/// Commands from the controller. Every transport transition sends a fresh
/// [`AudioCmd::Sync`] describing the complete desired state.
pub enum AudioCmd {
    /// Change the decoded file (single-source mode). `None` = no audio source.
    /// Either variant of a source change leaves any prior mix behind.
    SetSource(Option<std::path::PathBuf>),
    /// Switch to timeline mode: render the compiled mix (§5).
    SetMix(Arc<crate::mix::MixSpec>),
    /// Transport state changed: position/rate/playing as of `at`.
    Sync {
        media_us: i64,
        rate: f64,
        playing: bool,
    },
    Shutdown,
}

/// Shared clock feedback: the render thread publishes the media time derived
/// from consumed samples; the controller re-anchors its wall clock from it.
///
/// `media_us` and `consumed_at` are one *pair* — a media time and the consumed
/// counter it was measured against — and are meaningless apart. Read as two
/// independent atomics they could be torn across an anchor update, pairing a
/// new media time with the previous baseline, and the difference between two
/// anchors is a whole re-prime: the reported position was not merely imprecise
/// but had never been true at any instant. The controller turns that into a
/// correction of the video clock, so a torn read is a visible glitch.
///
/// So the pair is published under a sequence counter (a seqlock): odd while it
/// is being written, even when stable, and a reader that sees an odd count or a
/// count that changed under it takes the read again. There is exactly one
/// writer — the render thread — which is what makes this sound without a lock
/// the audio path would have to wait on.
pub struct AudioClock {
    /// Sequence counter guarding the `(media_us, consumed_at)` pair.
    seq: AtomicU32,
    /// Media µs corresponding to `consumed_at`; `i64::MIN` when audio is
    /// inactive (controller keeps wall clock).
    media_us: AtomicI64,
    /// Consumed counter value the anchor was computed at.
    consumed_at: AtomicU64,
    /// True while the render thread is actively feeding audible audio.
    pub active: AtomicBool,
}

/// How many times [`AudioClock::position`] re-reads a pair that changed under
/// it before giving up. The writer holds the pair for three stores, so losing
/// this many races running means the render thread was descheduled mid-update;
/// the honest answer then is "no reading", not a spin on the controller thread.
const SEQ_READ_TRIES: u32 = 4;

impl AudioClock {
    fn inactive() -> AudioClock {
        AudioClock {
            seq: AtomicU32::new(0),
            media_us: AtomicI64::new(i64::MIN),
            consumed_at: AtomicU64::new(0),
            active: AtomicBool::new(false),
        }
    }

    /// Publish a new anchor. **Render thread only** — the seqlock above allows
    /// exactly one writer.
    fn set_anchor(&self, media_us: i64, consumed: u64) {
        let seq = self.seq.load(Ordering::Relaxed);
        // Odd: a write is in progress.
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        self.media_us.store(media_us, Ordering::Relaxed);
        self.consumed_at.store(consumed, Ordering::Relaxed);
        // Even again, and one greater than before, so a reader that straddled
        // the write sees a different count and retries.
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// Read the anchor pair, or `None` if it could not be read consistently.
    fn anchor(&self) -> Option<(i64, u64)> {
        for _ in 0..SEQ_READ_TRIES {
            let before = self.seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue; // a write is in flight
            }
            let media_us = self.media_us.load(Ordering::Relaxed);
            let consumed_at = self.consumed_at.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return Some((media_us, consumed_at));
            }
        }
        None
    }

    /// Current audio-derived media position, if audio is active.
    pub fn position(&self, ring: &AudioRing, rate: f64) -> Option<i64> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let (anchor, consumed_at) = self.anchor()?;
        if anchor == i64::MIN {
            return None;
        }
        // `consumed()` is read *after* the pair, so it can only be at or ahead
        // of `consumed_at` — never behind it, which would read as the audio
        // running backwards.
        let consumed = ring.consumed().saturating_sub(consumed_at);
        Some(anchor + (consumed as f64 * rate * 1e6 / AUDIO_RATE as f64) as i64)
    }
}

/// The loudest the output gain may be set to: 4× the source, or +12 dB.
///
/// A ceiling of exactly unity is the wrong ceiling for a *player*. Plenty of
/// files are simply quiet — a phone clip, a voice memo, a badly mastered
/// export — and "turn it up past what is in the file" is something every media
/// player on the machine can do. Four is where the soft knee below stops
/// buying anything: past it, everything with any level in it is riding the
/// limiter and the result is loud rather than louder.
pub const MAX_VOLUME: f32 = 4.0;

/// Where [`soft_clip`] stops being a straight line. Below this a boosted
/// sample is scaled and nothing else; the curve exists for the peaks.
const SOFT_KNEE: f32 = 0.7;

/// Gain above unity has to go somewhere, and the one place it must not go is
/// into the rail: a sample multiplied past ±1 and hard-clipped is square-wave
/// distortion, which is the sound people mean when they say a boost "ruins"
/// the audio.
///
/// So the top of the range bends instead. Below [`SOFT_KNEE`] this is the
/// identity — a quiet track boosted 2× is genuinely 2× louder, untouched —
/// and above it the curve approaches 1.0 asymptotically, with a continuous
/// first derivative at the knee so there is no audible corner to hear.
fn soft_clip(x: f32) -> f32 {
    let mag = x.abs();
    if mag <= SOFT_KNEE {
        return x;
    }
    let head = 1.0 - SOFT_KNEE;
    let over = mag - SOFT_KNEE;
    let y = SOFT_KNEE + head * (over / (over + head));
    if x < 0.0 {
        -y
    } else {
        y
    }
}

pub struct AudioSys {
    pub ring: Arc<AudioRing>,
    /// Output mute (§12 live media peek plays muted): the cpal callback
    /// still consumes the ring (the clock keeps advancing) but emits zeros.
    pub muted: Arc<std::sync::atomic::AtomicBool>,
    /// Output gain, `0..=`[`MAX_VOLUME`], as `f32::to_bits`. Applied in the
    /// cpal callback alongside the mute, so the ring and the clock are
    /// untouched by it.
    pub volume: Arc<AtomicU32>,
    pub clock: Arc<AudioClock>,
    /// Live mix meters (§5): LUFS, peaks, per-clip GR — published by the
    /// render thread while the timeline mix plays.
    pub meters: Arc<crate::mix::MixMeters>,
    pub tx: Sender<AudioCmd>,
    /// Kept alive for the duration of the app; dropping stops the stream.
    _stream: Option<cpal::Stream>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl AudioSys {
    /// Build the output stream (best effort — a machine with no audio device
    /// still plays video against the wall clock) and spawn the render thread.
    pub fn start() -> AudioSys {
        // 1 s of stereo at 48 kHz.
        let ring = Arc::new(AudioRing::new(AUDIO_RATE as usize * 2));
        let clock = Arc::new(AudioClock::inactive());
        let meters = Arc::new(crate::mix::MixMeters::new());
        let (tx, rx) = crossbeam_channel::unbounded();

        let muted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let stream = build_stream(ring.clone(), muted.clone(), volume.clone());
        if stream.is_none() {
            log::warn!("no audio output device; playback will be silent");
        }

        let join = std::thread::Builder::new()
            .name("dv-audio-render".into())
            .spawn({
                let ring = ring.clone();
                let clock = clock.clone();
                let meters = meters.clone();
                move || render_thread(rx, ring, clock, meters)
            })
            .ok();

        AudioSys {
            ring,
            muted,
            volume,
            clock,
            meters,
            tx,
            _stream: stream,
            join,
        }
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(AudioCmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 4-point Catmull-Rom (cubic Hermite with centred-difference tangents) at
/// `t ∈ [0, 1)` between `p1` and `p2`. Passes through both, so a conversion
/// that lands exactly on a source sample reproduces it untouched.
fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (p2 - p0);
    let c2 = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c3 = 0.5 * (p3 - p0) + 1.5 * (p1 - p2);
    ((c3 * t + c2) * t + c1) * t + p1
}

/// Place one stereo frame into the device's channel layout: a mono device gets
/// the downmix, anything wider gets L/R and silence above them.
fn write_frame(out: &mut [f32], channels: usize, f: usize, l: f32, r: f32) {
    match channels {
        1 => out[f] = (l + r) * 0.5,
        _ => {
            out[f * channels] = l;
            out[f * channels + 1] = r;
            for c in 2..channels {
                out[f * channels + c] = 0.0;
            }
        }
    }
}

/// Turns the ring's fixed 48 kHz stereo into the device's rate and channel
/// count, one cpal callback at a time.
///
/// The device is opened at [`AUDIO_RATE`] when it will take it (see
/// [`output_config`]) and then this is a straight copy. When it will not —
/// plenty of machines only offer 44.1 kHz — the conversion has to genuinely
/// interpolate: picking the nearest ring frame drops every ~12th one at
/// 48→44.1, and a dropped sample is a step discontinuity, which is broadband
/// noise. It is audible as grit on anything bright.
///
/// So each output frame is a 4-point Catmull-Rom between the ring frames
/// around it. That window reaches one frame behind the output position and two
/// ahead, and a callback boundary falls wherever the device's buffer size puts
/// it — so the frames at and before the next output position are carried over
/// to the next callback along with the fractional phase. Without them the
/// window would restart from silence at the device's buffer rate, which is a
/// click train rather than a stream.
struct Resampler {
    /// Ring frames per output frame (`AUDIO_RATE` ÷ the device's rate).
    ratio: f64,
    /// Fractional source position of the next output frame, measured from
    /// `win`'s second frame.
    phase: f64,
    /// Source frames, interleaved stereo.
    ///
    /// On the interpolating path this is the window the next output frame
    /// reads: frame 0 is the point behind it and frame 1 the one `phase` is
    /// measured from, so it is never shorter than three frames. A callback
    /// pulls whatever it is short of and keeps whatever it did not consume —
    /// a device running faster than the ring can over-pull by a frame, and a
    /// pulled frame left behind is a dropped sample, which is exactly the
    /// defect this whole path exists to avoid. On the native-rate path
    /// nothing is carried and this is plain scratch.
    win: Vec<f32>,
}

/// Frames of history the window needs behind and ahead of the next output
/// position: one behind, the two the interpolation runs between. Cubic needs a
/// fourth point, but that one is always ahead of the carry.
const WINDOW_CARRY: usize = 3;

impl Resampler {
    fn new(ratio: f64) -> Resampler {
        Resampler {
            ratio,
            phase: 0.0,
            win: vec![0.0; WINDOW_CARRY * 2],
        }
    }

    /// Write one callback's worth of output — `out.len() / channels` frames —
    /// pulling ring samples through `pull`, which fills its argument and
    /// returns how many samples it actually got. Short is an underrun, and an
    /// underrun reads as silence.
    fn fill(
        &mut self,
        out: &mut [f32],
        channels: usize,
        mut pull: impl FnMut(&mut [f32]) -> usize,
    ) {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        if frames == 0 {
            return;
        }

        // Device at the ring's own rate: nothing to interpolate, and nothing
        // to carry — an interpolator asked for the sample it was handed
        // returns it, but only after paying for four multiplies.
        if self.ratio == 1.0 {
            let n = frames * 2;
            self.win.resize(n, 0.0);
            let got = pull(&mut self.win[..n]).min(n);
            self.win[got..n].fill(0.0);
            for f in 0..frames {
                write_frame(out, channels, f, self.win[f * 2], self.win[f * 2 + 1]);
            }
            return;
        }

        // Output frame `f` reads a window at source position `phase + f ×
        // ratio` that reaches one frame back and two forward, so the last
        // output frame sets how far `win` has to extend. The window then moves
        // on by `advance` whole frames and must leave `WINDOW_CARRY` behind it
        // for the next callback, which for a fast device is the binding
        // constraint instead. Pull only the difference: everything already in
        // `win` is a frame that was pulled and not yet read.
        let last = self.phase + (frames - 1) as f64 * self.ratio;
        let end = self.phase + frames as f64 * self.ratio;
        let advance = end.floor() as usize;
        let have = self.win.len() / 2;
        let total = have
            .max(last.floor() as usize + WINDOW_CARRY + 1)
            .max(advance + WINDOW_CARRY);
        self.win.resize(total * 2, 0.0);
        let got = pull(&mut self.win[have * 2..]).min((total - have) * 2);
        self.win[have * 2 + got..].fill(0.0);

        let (phase, ratio) = (self.phase, self.ratio);
        for f in 0..frames {
            let u = phase + f as f64 * ratio;
            let i = u.floor();
            let t = (u - i) as f32;
            // Window frames `i-1 ..= i+2` live at `win` frames `i ..= i+3`.
            // The arithmetic above guarantees the slice; taking it fallibly
            // anyway keeps a panic out of the audio callback, where it would
            // take the process with it.
            let base = i as usize * 2;
            let Some(w) = self.win.get(base..base + 8) else {
                write_frame(out, channels, f, 0.0, 0.0);
                continue;
            };
            let l = catmull_rom(w[0], w[2], w[4], w[6], t);
            let r = catmull_rom(w[1], w[3], w[5], w[7], t);
            write_frame(out, channels, f, l, r);
        }

        // Carry: drop what the window has moved past, keep the rest.
        self.win.drain(..advance * 2);
        self.phase = end - advance as f64;
    }
}

/// Pick the output config. The ring is fixed at [`AUDIO_RATE`], so a device
/// that will take 48 kHz is opened at 48 kHz and the callback copies instead
/// of resampling — the conversion that is not done cannot colour anything.
/// Failing that (44.1 kHz-only devices are common) fall back to whatever the
/// device calls its default and let [`Resampler`] interpolate.
fn output_config(device: &cpal::Device) -> Option<cpal::SupportedStreamConfig> {
    let wanted = cpal::SampleRate(AUDIO_RATE);
    device
        .supported_output_configs()
        .ok()
        .and_then(|configs| {
            configs
                .filter(|c| {
                    c.sample_format() == cpal::SampleFormat::F32
                        && c.channels() >= 2
                        && c.min_sample_rate() <= wanted
                        && c.max_sample_rate() >= wanted
                })
                // Plain stereo first: the callback fills everything above the
                // second channel with silence, so opening a surround config
                // would leave the centre and the surrounds dead.
                .min_by_key(|c| c.channels())
        })
        .and_then(|c| c.try_with_sample_rate(wanted))
        .or_else(|| device.default_output_config().ok())
}

/// Open the default output device at the ring's rate where possible; the
/// callback adapts rate (cubic) and channel count from the ring's 48 kHz
/// stereo where not.
fn build_stream(
    ring: Arc<AudioRing>,
    muted: Arc<std::sync::atomic::AtomicBool>,
    volume: Arc<AtomicU32>,
) -> Option<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let config = output_config(&device)?;
    let dev_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let mut resampler = Resampler::new(AUDIO_RATE as f64 / dev_rate as f64);
    let stream = device
        .build_output_stream(
            &config.config(),
            move |out: &mut [f32], _| {
                resampler.fill(out, channels, |buf| ring.pop(buf));
                if muted.load(std::sync::atomic::Ordering::Relaxed) {
                    out.fill(0.0);
                } else {
                    let gain =
                        f32::from_bits(volume.load(Ordering::Relaxed)).clamp(0.0, MAX_VOLUME);
                    if gain != 1.0 {
                        for s in out.iter_mut() {
                            *s = soft_clip(*s * gain);
                        }
                    }
                }
            },
            |e| log::warn!("audio stream error: {e}"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    if dev_rate == AUDIO_RATE {
        log::info!("audio out: {dev_rate} Hz, {channels} ch (native {AUDIO_RATE})");
    } else {
        log::info!("audio out: {dev_rate} Hz, {channels} ch (resampling from {AUDIO_RATE})");
    }
    Some(stream)
}

/// Ceiling for the single-source limiter, in dBTP. The same number as
/// `MasterParams`' default in §5, deliberately: a file has to sound the same
/// played on its own as it does dropped on a timeline.
const SOURCE_CEILING_DBTP: f64 = -1.0;

struct RenderState {
    decoder: Option<AudioDecoder>,
    source: Option<std::path::PathBuf>,
    /// Timeline mixer (§5); `Some` = timeline mode (overrides `source`).
    mix: Option<crate::mix::MixRenderer>,
    /// True-peak limiter on the decoded stream (single-source mode only —
    /// timeline mode has the same limiter on the mixer's master bus).
    ///
    /// A decoded stream is not bounded to ±1. MP3 and AAC reconstruct from
    /// coefficients, not samples, so a hot master decodes past full scale —
    /// the file this was written for peaks at 1.42 — and everything past full
    /// scale is squared off by the device. That is the sound of a loud song
    /// "distorting" in a player that is doing nothing wrong except handing the
    /// device what it was given.
    limiter: Limiter,
    /// Desired transport state. In mix mode `media_us`/`write_us` are
    /// TIMELINE µs (so the audio clock reports timeline time).
    media_us: i64,
    rate: f64,
    playing: bool,
    /// Leftover rendered samples not yet pushed to the ring.
    pending: Vec<f32>,
    /// Source sample phase for rate stepping (single-source mode).
    step_phase: f64,
    /// Media time of the next sample that will be pushed.
    write_us: i64,
    /// True once the ring/decoder are positioned for the current state.
    primed: bool,
}

fn audible(rate: f64) -> bool {
    rate > 0.0 && rate <= MAX_AUDIBLE_RATE
}

/// Timeline-mode render pass (§5): fill the ring with mixer blocks. The
/// render-ahead *is* the audio prefetch across cuts; `write_us`/the clock stay
/// in TIMELINE time, so §4.4 re-anchoring is unchanged.
fn render_mix(st: &mut RenderState, ring: &Arc<AudioRing>, clock: &Arc<AudioClock>) {
    let Some(mix) = st.mix.as_mut() else {
        if clock.active.swap(false, Ordering::AcqRel) {
            ring.flush();
        }
        return;
    };
    let duration = mix.spec().duration_us;

    // (Re)prime: restart from a clean ring at the requested timeline position.
    if !st.primed {
        clock.active.store(false, Ordering::Release);
        ring.flush();
        st.pending.clear();
        mix.reset_for_seek();
        st.write_us = st.media_us;
        clock.set_anchor(st.media_us, ring.consumed());
        clock.active.store(true, Ordering::Release);
        st.primed = true;
    }

    let ahead_target = (RENDER_AHEAD.as_secs_f64() * AUDIO_RATE as f64 * 2.0) as usize;
    while ring.len() < ahead_target {
        // Push leftovers first.
        if !st.pending.is_empty() {
            let n = ring.push(&st.pending);
            st.pending.drain(..n);
            if !st.pending.is_empty() {
                break; // ring full
            }
            continue;
        }
        // End of the timeline: let the ring drain (controller pauses at end).
        if st.write_us >= duration {
            break;
        }
        let (block, next_us) = mix.render_block(st.write_us, st.rate);
        st.pending = block.to_vec();
        st.write_us = next_us;
    }
}

/// The audio render thread (§3): keeps the ring RENDER_AHEAD full while
/// playing at an audible rate, and publishes the audio clock.
fn render_thread(
    rx: Receiver<AudioCmd>,
    ring: Arc<AudioRing>,
    clock: Arc<AudioClock>,
    meters: Arc<crate::mix::MixMeters>,
) {
    let mut st = RenderState {
        decoder: None,
        source: None,
        mix: None,
        limiter: Limiter::new(SOURCE_CEILING_DBTP),
        media_us: 0,
        rate: 1.0,
        playing: false,
        pending: Vec::new(),
        step_phase: 0.0,
        write_us: 0,
        primed: false,
    };

    loop {
        // Service all pending commands (last state wins).
        let has_work = st.mix.is_some() || st.source.is_some();
        let cmd = if st.playing && audible(st.rate) && has_work {
            match rx.recv_timeout(Duration::from_millis(20)) {
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
        let mut newest = cmd;
        while let Some(cmd) = newest.take() {
            match cmd {
                AudioCmd::Shutdown => return,
                AudioCmd::SetSource(path) => {
                    // Single-source mode: leave timeline mode.
                    st.source = path;
                    st.mix = None;
                    st.decoder = None;
                    st.primed = false;
                    meters.reset();
                }
                AudioCmd::SetMix(spec) => {
                    match st.mix.as_mut() {
                        // Edit re-send: keep streaming state where spans are
                        // unchanged so edits elsewhere never click here.
                        Some(m) => m.set_spec(spec),
                        None => {
                            st.mix = Some(crate::mix::MixRenderer::new(spec, meters.clone()));
                            st.primed = false;
                        }
                    }
                    st.source = None;
                    st.decoder = None;
                }
                AudioCmd::Sync {
                    media_us,
                    rate,
                    playing,
                } => {
                    st.media_us = media_us;
                    st.rate = rate;
                    st.playing = playing;
                    st.primed = false;
                }
            }
            newest = rx.try_recv().ok();
        }

        let has_work = st.mix.is_some() || st.source.is_some();
        if !(st.playing && audible(st.rate)) || !has_work {
            if clock.active.swap(false, Ordering::AcqRel) {
                ring.flush();
            }
            continue;
        }

        // Timeline mode has its own prime + fill path.
        if st.mix.is_some() {
            render_mix(&mut st, &ring, &clock);
            continue;
        }

        // (Re)prime: open/seek the decoder to the requested position and
        // restart the clock anchor from a clean ring.
        if !st.primed {
            clock.active.store(false, Ordering::Release);
            ring.flush();
            st.pending.clear();
            st.step_phase = 0.0;
            // The limiter's gain and lookahead line describe audio that is
            // about to be thrown away; carried across a seek they would duck
            // the first milliseconds of the new position for a peak that is no
            // longer coming.
            st.limiter.reset();
            // "The stream has no audio" is a permanent answer even on a live
            // source (`TemporalInfo.has_audio` is assumed true for streams,
            // so a camera with no mic lands here); a failed *connection* is
            // not — that camera is mid-reboot and worth another try.
            let mut no_audio = false;
            if st.decoder.is_none() {
                st.decoder = st
                    .source
                    .as_ref()
                    .and_then(|p| match AudioDecoder::open(p) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            no_audio = matches!(e, dv_media::MediaError::NoAudio(_));
                            log::debug!("no audio for {}: {e}", p.display());
                            None
                        }
                    });
            }
            let live = st.source.as_deref().is_some_and(dv_media::is_stream_url);
            let Some(dec) = st.decoder.as_mut() else {
                // A live source that would not open is a camera mid-reboot,
                // not a video-only file: keep trying, at a walk. The sleep
                // throttles the (blocking) reopen; the pause it puts on
                // command handling is a second of latency on a mute nobody
                // can hear anyway.
                if live && !no_audio {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                // Video-only source: stay inactive (wall clock drives video).
                st.playing = false;
                continue;
            };
            // A live stream has nowhere to seek — the socket is already at
            // the present, and an RTSP seek would error the prime away.
            if !live && dec.seek(st.media_us).is_err() {
                st.playing = false;
                continue;
            }
            st.write_us = st.media_us;
            clock.set_anchor(st.media_us, ring.consumed());
            clock.active.store(true, Ordering::Release);
            st.primed = true;
            // Trim up to the exact target sample below (chunks may start early).
        }

        // Keep the ring RENDER_AHEAD ahead of what's been consumed.
        let ahead_target = (RENDER_AHEAD.as_secs_f64() * AUDIO_RATE as f64 * 2.0) as usize;
        while ring.len() < ahead_target {
            // A live source delivers in real time, so the ring can never get
            // RENDER_AHEAD ahead of a real-time drain — without this check the
            // fill loop is where the thread lives and commands starve.
            if !rx.is_empty() {
                break;
            }
            if !st.pending.is_empty() {
                let n = ring.push(&st.pending);
                st.pending.drain(..n);
                if !st.pending.is_empty() {
                    break; // ring full
                }
                continue;
            }
            let Some(dec) = st.decoder.as_mut() else {
                break;
            };
            match dec.next_chunk() {
                Ok(Some(chunk)) => {
                    // Trim any part of the chunk before the current write pos.
                    let mut samples = chunk.samples;
                    let chunk_start = chunk.start_us;
                    if chunk_start < st.write_us {
                        let skip_frames =
                            ((st.write_us - chunk_start) as f64 * AUDIO_RATE as f64 / 1e6) as usize;
                        if skip_frames * 2 >= samples.len() {
                            continue;
                        }
                        samples.drain(..skip_frames * 2);
                    }
                    // Rate-step (nearest neighbor) for 1 < rate ≤ 2.
                    if (st.rate - 1.0).abs() > 1e-6 {
                        let mut stepped =
                            Vec::with_capacity((samples.len() as f64 / st.rate) as usize + 2);
                        let frames = samples.len() / 2;
                        while (st.step_phase as usize) < frames {
                            let i = st.step_phase as usize;
                            stepped.push(samples[i * 2]);
                            stepped.push(samples[i * 2 + 1]);
                            st.step_phase += st.rate;
                        }
                        st.step_phase -= frames as f64;
                        st.write_us = chunk_start.max(st.write_us)
                            + (frames as f64 * 1e6 / AUDIO_RATE as f64) as i64;
                        st.pending = stepped;
                    } else {
                        st.write_us = chunk_start.max(st.write_us)
                            + (samples.len() / 2) as i64 * 1_000_000 / AUDIO_RATE as i64;
                        st.pending = samples;
                    }
                    // Nothing above full scale reaches the ring. The limiter
                    // is transparent below its ceiling, so this runs
                    // unconditionally rather than on a level test — a test
                    // would only add a place for the gain to jump.
                    st.limiter.process(&mut st.pending);
                }
                Ok(None) | Err(_) => {
                    // EOF: let the ring drain; the controller stops playback
                    // when the clock reaches the duration. On a live source
                    // EOF is a *stall* — drop the dead session and re-prime,
                    // which reopens it (mirrors the video side's reconnect).
                    if st.source.as_deref().is_some_and(dv_media::is_stream_url) {
                        st.decoder = None;
                        st.primed = false;
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ: f64 = 1_000.0;
    const AMP: f64 = 0.9;
    /// The common case the resampler exists for: a 44.1 kHz device fed from
    /// the 48 kHz ring.
    const DEV_RATE: f64 = 44_100.0;

    /// `frames` stereo frames of a 1 kHz sine at the ring's rate.
    fn source(frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = ((2.0 * std::f64::consts::PI * HZ * n as f64 / AUDIO_RATE as f64).sin() * AMP)
                as f32;
            v.push(s);
            v.push(s);
        }
        v
    }

    /// What output frame `f` *should* be: the same sine read at source
    /// position `f × ratio`, less the two frames the window sits behind by
    /// (the frame the phase is measured from starts two before the first
    /// sample ever pulled).
    fn ideal(f: usize, ratio: f64) -> f64 {
        let pos = f as f64 * ratio - 2.0;
        (2.0 * std::f64::consts::PI * HZ * pos / AUDIO_RATE as f64).sin() * AMP
    }

    /// One cpal callback: `frames` stereo output frames, pulled from `src`
    /// where the last callback left off.
    fn callback(res: &mut Resampler, src: &[f32], cursor: &mut usize, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        res.fill(&mut out, 2, |buf| {
            let n = buf.len().min(src.len() - *cursor);
            buf[..n].copy_from_slice(&src[*cursor..*cursor + n]);
            *cursor += n;
            n
        });
        out
    }

    /// Push a 1 kHz sine through the resampler for `dev_rate` in a run of
    /// deliberately uneven callbacks, and return how many dB the output is
    /// above its own error against the ideal resampled sine.
    fn resample_snr(dev_rate: f64) -> f64 {
        let ratio = AUDIO_RATE as f64 / dev_rate;
        let src = source(48_000); // 1 s, far more than the callbacks below use
        let mut res = Resampler::new(ratio);
        let mut cursor = 0usize;
        let mut out: Vec<f32> = Vec::new();
        // Uneven sizes on purpose: the phase and the carried window have to
        // survive a callback boundary landing anywhere within a source frame.
        for frames in [512usize, 480, 501, 499, 512, 333] {
            out.extend_from_slice(&callback(&mut res, &src, &mut cursor, frames));
        }
        assert!(
            cursor < src.len(),
            "test ran the source dry at {dev_rate} Hz"
        );

        // Skip the warm-up: the window starts from silence, which is the
        // stream starting rather than an error. How many output frames that
        // covers depends on the rate — a device four times the ring's takes
        // four times as many output frames to clear the same source frames.
        let warm = (3.0 / ratio).ceil() as usize + 1;
        let mut err2 = 0.0f64;
        let mut sig2 = 0.0f64;
        for f in warm..out.len() / 2 {
            let want = ideal(f, ratio);
            let got = out[f * 2] as f64;
            assert_eq!(out[f * 2], out[f * 2 + 1], "channels diverged at {f}");
            err2 += (got - want) * (got - want);
            sig2 += want * want;
        }
        10.0 * (sig2 / err2).log10()
    }

    /// A 1 kHz sine converted to the device's rate must still be a 1 kHz sine.
    ///
    /// The nearest-neighbour pick this replaced dropped every ~12th ring frame
    /// at 48 → 44.1, and a dropped sample is a step discontinuity — broadband
    /// noise, spread across the spectrum, audible as grit on anything bright.
    /// Measured against the ideal resampled sine it scored about 22 dB; cubic
    /// has to do very much better than that.
    ///
    /// The rates run both ways past the ring's own. A device *faster* than
    /// 48 kHz is the case where a callback can pull a frame it has no room to
    /// read yet, and dropping that frame instead of carrying it is the same
    /// defect wearing a different hat.
    #[test]
    fn a_sine_resampled_to_the_devices_rate_is_still_a_sine() {
        for dev_rate in [DEV_RATE, 88_200.0, 96_000.0, 192_000.0, 32_000.0, 8_000.0] {
            let snr = resample_snr(dev_rate);
            assert!(snr > 40.0, "{dev_rate} Hz output is only {snr:.1} dB clean");
        }
    }

    /// Two callbacks in a row have to join without a seam.
    ///
    /// The interpolation window reaches behind the output position, so a
    /// callback that starts it from nothing restarts from silence — at the
    /// device's buffer rate, which is a click train. The three carried frames
    /// are what prevent that, and the way to see them working is that the
    /// joined output never steps further between samples than the sine itself
    /// does.
    #[test]
    fn consecutive_callbacks_join_without_a_seam() {
        let ratio = AUDIO_RATE as f64 / DEV_RATE;
        let src = source(8_000);
        let mut res = Resampler::new(ratio);
        let mut cursor = 0usize;
        let seam = 301usize;
        let mut out = callback(&mut res, &src, &mut cursor, seam);
        out.extend_from_slice(&callback(&mut res, &src, &mut cursor, 289));
        let frames = out.len() / 2;

        // The largest step the ideal sine takes between two output samples.
        let mut want_step = 0.0f64;
        for f in 5..frames {
            want_step = want_step.max((ideal(f, ratio) - ideal(f - 1, ratio)).abs());
        }
        // The largest the real output takes, warm-up excluded. A window
        // restarting at the seam would put most of the amplitude into one
        // step, which is nowhere near this bound.
        for f in 5..frames {
            let step = (out[f * 2] as f64 - out[(f - 1) * 2] as f64).abs();
            assert!(
                step <= want_step * 1.05,
                "step {step:.4} at frame {f} (seam {seam}) exceeds the sine's own {want_step:.4}"
            );
        }
        // And the seam frames are not merely continuous but correct.
        for f in seam - 1..=seam + 1 {
            let e = (out[f * 2] as f64 - ideal(f, ratio)).abs();
            assert!(e < 1e-3, "frame {f} is {e:.5} off the ideal");
        }
    }

    /// A device already at the ring's rate gets the ring, sample for sample.
    #[test]
    fn a_native_rate_device_gets_a_straight_copy() {
        let src = source(600);
        let mut res = Resampler::new(1.0);
        let mut cursor = 0usize;
        let out = callback(&mut res, &src, &mut cursor, 300);
        assert_eq!(&out[..600], &src[..600]);
        assert_eq!(cursor, 600);
    }

    /// A ring that cannot keep up reads as silence rather than as the last
    /// frame repeated, and the callback still fills its whole buffer.
    #[test]
    fn an_underrun_reads_as_silence() {
        let ratio = AUDIO_RATE as f64 / DEV_RATE;
        let src = source(64);
        let mut res = Resampler::new(ratio);
        let mut cursor = 0usize;
        let out = callback(&mut res, &src, &mut cursor, 256);
        assert_eq!(out.len(), 512);
        assert_eq!(cursor, src.len(), "should have drained what there was");
        // Well past the 64 frames the source had, everything is silence.
        assert!(out[200 * 2..].iter().all(|s| *s == 0.0));
    }

    /// The boost curve: transparent under the knee, never past the rail, and
    /// symmetric about zero.
    #[test]
    fn the_soft_knee_is_transparent_below_it_and_never_reaches_the_rail() {
        for x in [0.0f32, 0.1, 0.35, SOFT_KNEE] {
            assert_eq!(soft_clip(x), x, "{x} should pass through untouched");
            assert_eq!(soft_clip(-x), -x);
        }
        // Above the knee it bends, and it bends towards 1.0 without arriving.
        for x in [0.8f32, 1.0, 2.0, 4.0, 40.0] {
            let y = soft_clip(x);
            assert!(y > SOFT_KNEE && y < 1.0, "soft_clip({x}) = {y}");
            assert_eq!(soft_clip(-x), -y);
        }
        // …monotonically, so louder in is always louder out.
        let mut prev = soft_clip(SOFT_KNEE);
        for i in 1..=400 {
            let y = soft_clip(SOFT_KNEE + i as f32 * 0.01);
            assert!(y > prev, "not monotonic at step {i}");
            prev = y;
        }
        // And no corner at the knee: the slope on either side matches.
        let d = 1e-4;
        let below = (soft_clip(SOFT_KNEE) - soft_clip(SOFT_KNEE - d)) / d;
        let above = (soft_clip(SOFT_KNEE + d) - soft_clip(SOFT_KNEE)) / d;
        assert!((below - above).abs() < 0.01, "{below} vs {above}");
    }

    /// The anchor pair must never be *observed* torn — a reader either sees a
    /// `(media_us, consumed_at)` exactly as some `set_anchor` published it, or
    /// declines to read at all.
    ///
    /// Before the seqlock the two were independent atomics, so a read that
    /// straddled an update paired a new media time with the old baseline. The
    /// controller fed that difference straight into a correction of the video
    /// clock, which is a visible jump. The writer here republishes as fast as
    /// it can, which is far harder than the real render thread's ~10 Hz.
    #[test]
    fn the_anchor_pair_is_never_torn() {
        // The invariant the reader checks: consumed is always media × 7.
        fn consumed_for(media: i64) -> u64 {
            (media as u64).wrapping_mul(7)
        }

        let clock = Arc::new(AudioClock::inactive());
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let clock = Arc::clone(&clock);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut media: i64 = 0;
                while !stop.load(Ordering::Relaxed) {
                    media = media.wrapping_add(1);
                    clock.set_anchor(media, consumed_for(media));
                }
            })
        };

        let mut consistent = 0u64;
        let mut declined = 0u64;
        // Run until enough real anchors have been seen, not for a fixed number
        // of reads: 200_000 reads take less time than spawning the writer, so a
        // fixed count can finish before a single anchor has been published and
        // pass without having tested anything.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while consistent < 20_000 && std::time::Instant::now() < deadline {
            match clock.anchor() {
                // The pre-`set_anchor` initial state, which has no invariant.
                Some((media, _)) if media == i64::MIN => {}
                Some((media, consumed)) => {
                    assert_eq!(
                        consumed,
                        consumed_for(media),
                        "torn read: media {media} paired with consumed {consumed}"
                    );
                    consistent += 1;
                }
                None => declined += 1,
            }
        }

        stop.store(true, Ordering::Relaxed);
        writer.join().expect("writer thread panicked");
        // A reader that always declined would pass the tear check vacuously.
        assert_eq!(
            consistent, 20_000,
            "timed out before reading 20000 consistent pairs ({declined} declined)"
        );
    }
}
