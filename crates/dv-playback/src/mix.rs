//! The timeline audio mixer (§5). The app compiles the project into a
//! [`MixSpec`] — overlapping per-clip spans across four track lanes, with
//! crossfade handle material already resolved by the compiler (which knows
//! media durations) — and [`MixRenderer`] renders it block by block:
//!
//! per clip: decode → channel mode → clip gain → strip chain
//! (denoise→HPF→EQ→LPF→comp) → fades/crossfades → pan → track buffer;
//! then: ducking sidechain (voice tracks drive duck-marked tracks) → track
//! gain/pan/sends → delay bus → master (optional bus comp → loudness gain →
//! true-peak limiter) → output. Export (M6) renders through this same path,
//! so what you hear is exactly what exports.
//!
//! All output is 48 kHz stereo interleaved f32, in fixed
//! [`dsp::BLOCK_FRAMES`]-frame blocks (10 ms — also the denoiser's frame).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use dv_core::model::{ChannelMode, StripParams};
use dv_media::{AudioDecoder, AUDIO_RATE};

use crate::dsp::{self, Compressor, Ducker, Limiter, StereoDelay, StripChain, BLOCK_FRAMES};

/// Track lanes in the mix (§6.1): V1's own audio, unmuted b-roll audio,
/// voiceover, music.
pub const MIX_TRACKS: usize = 4;
pub const TR_V1: usize = 0;
pub const TR_V2: usize = 1;
pub const TR_A1: usize = 2;
pub const TR_A2: usize = 3;

/// Always-on micro-fade at every hard clip boundary (§5, click prevention).
pub const MICRO_FADE_US: i64 = 5_000;

/// One audible clip span. Spans may overlap freely (crossfades overlap on the
/// same lane; different lanes mix); the compiler has already extended
/// crossfading spans into their handle material and clamped to what exists.
#[derive(Debug, Clone, PartialEq)]
pub struct MixClip {
    /// Model clip id — GR meter reporting key.
    pub clip_id: i64,
    /// `TR_*` lane index.
    pub track: usize,
    pub path: PathBuf,
    pub tl_start_us: i64,
    pub tl_dur_us: i64,
    /// Source µs at `tl_start_us`.
    pub source_offset_us: i64,
    pub speed: f64,
    /// Linear per-clip gain (mixing half — never shared, §5).
    pub gain: f32,
    /// −1..1.
    pub pan: f32,
    pub channel_mode: ChannelMode,
    /// Head ramp: duration + shape. `xfade` = equal-power (incoming half of a
    /// crossfade); plain = linear fade from silence.
    pub head_fade_us: i64,
    pub head_xfade: bool,
    /// Tail ramp, mirrored.
    pub tail_fade_us: i64,
    pub tail_xfade: bool,
    /// Shared treatment (§5); `None` = flat.
    pub strip: Option<StripParams>,
    /// Counts toward the ducking sidechain (§5: V1 + A1 are voice).
    pub voice: bool,
}

impl MixClip {
    pub fn tl_end_us(&self) -> i64 {
        self.tl_start_us + self.tl_dur_us
    }

    /// Gain of the head/tail ramps at timeline instant `t` (1.0 mid-clip).
    /// The micro-fade floor (§5) applies to un-faded hard edges.
    pub fn ramp_gain_at(&self, t: i64) -> f32 {
        let mut g = 1.0f32;
        let head = if self.head_fade_us > 0 || self.head_xfade {
            self.head_fade_us
        } else {
            MICRO_FADE_US.min(self.tl_dur_us / 2)
        };
        if head > 0 && t < self.tl_start_us + head {
            let p = (t - self.tl_start_us) as f32 / head as f32;
            g *= if self.head_xfade {
                dsp::equal_power(p.clamp(0.0, 1.0)).1
            } else {
                p.clamp(0.0, 1.0)
            };
        }
        let tail = if self.tail_fade_us > 0 || self.tail_xfade {
            self.tail_fade_us
        } else {
            MICRO_FADE_US.min(self.tl_dur_us / 2)
        };
        let end = self.tl_end_us();
        if tail > 0 && t >= end - tail {
            let p = (end - t) as f32 / tail as f32;
            g *= if self.tail_xfade {
                dsp::equal_power((1.0 - p).clamp(0.0, 1.0)).0
            } else {
                p.clamp(0.0, 1.0)
            };
        }
        g
    }
}

/// Resolved per-track settings (mute/solo already folded into `audible`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixTrack {
    pub gain: f32,
    pub pan: f32,
    /// Delay-bus send level 0–1 (§5 per-track send).
    pub send: f32,
    /// Music ducking under voice (§5).
    pub duck: bool,
    pub audible: bool,
}

impl Default for MixTrack {
    fn default() -> Self {
        MixTrack {
            gain: 1.0,
            pan: 0.0,
            send: 0.0,
            duck: false,
            audible: true,
        }
    }
}

/// Optional gentle bus compressor on the master (§5), default off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BusComp {
    pub threshold_db: f64,
    pub ratio: f64,
    pub makeup_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MasterParams {
    pub loudness_enabled: bool,
    pub loudness_target_lufs: f64,
    pub limiter_ceiling_dbtp: f64,
    pub comp: Option<BusComp>,
}

/// The compiled mix: everything the renderer needs, immutable per edit.
#[derive(Debug, Clone, PartialEq)]
pub struct MixSpec {
    pub clips: Vec<MixClip>,
    pub tracks: [MixTrack; MIX_TRACKS],
    pub master: MasterParams,
    pub duration_us: i64,
}

impl MixSpec {
    pub fn empty() -> MixSpec {
        MixSpec {
            clips: Vec::new(),
            tracks: [MixTrack::default(); MIX_TRACKS],
            master: MasterParams {
                loudness_enabled: true,
                loudness_target_lufs: -14.0,
                limiter_ceiling_dbtp: -1.0,
                comp: None,
            },
            duration_us: 0,
        }
    }

    /// Indices of clips whose span intersects [a, b).
    pub fn clips_overlapping(&self, a: i64, b: i64) -> Vec<usize> {
        self.clips
            .iter()
            .enumerate()
            .filter(|(_, c)| c.tl_start_us < b && c.tl_end_us() > a)
            .map(|(i, _)| i)
            .collect()
    }
}

/// Constant-power pan gains for `pan` ∈ −1..1.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let t = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (t.cos(), t.sin())
}

/// Apply a channel mode in place (§5): stereo passthrough, one-side-as-mono,
/// or sum-to-mono.
pub fn apply_channel_mode(mode: ChannelMode, interleaved: &mut [f32]) {
    match mode {
        ChannelMode::Stereo => {}
        ChannelMode::Left => {
            for f in interleaved.chunks_exact_mut(2) {
                f[1] = f[0];
            }
        }
        ChannelMode::Right => {
            for f in interleaved.chunks_exact_mut(2) {
                f[0] = f[1];
            }
        }
        ChannelMode::Sum => {
            for f in interleaved.chunks_exact_mut(2) {
                let m = (f[0] + f[1]) * 0.5;
                f[0] = m;
                f[1] = m;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Live meters (§5): the render thread publishes, the UI reads.
// ---------------------------------------------------------------------------

/// Fixed-point milli-dB atomics; `i64::MIN` = no measurement.
pub struct MixMeters {
    pub short_term_lufs_milli: AtomicI64,
    pub integrated_lufs_milli: AtomicI64,
    /// Static loudness makeup currently applied (§5), milli-dB.
    pub loudness_gain_db_milli: AtomicI64,
    /// Master output peak per channel, milli-dBFS.
    pub peak_l_milli: AtomicI64,
    pub peak_r_milli: AtomicI64,
    /// Compressor gain reduction per clip id (only clips audible right now).
    pub clip_gr_db: parking_lot::Mutex<HashMap<i64, f32>>,
}

impl MixMeters {
    pub fn new() -> MixMeters {
        MixMeters {
            short_term_lufs_milli: AtomicI64::new(i64::MIN),
            integrated_lufs_milli: AtomicI64::new(i64::MIN),
            loudness_gain_db_milli: AtomicI64::new(0),
            peak_l_milli: AtomicI64::new(i64::MIN),
            peak_r_milli: AtomicI64::new(i64::MIN),
            clip_gr_db: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    pub fn reset(&self) {
        self.short_term_lufs_milli
            .store(i64::MIN, Ordering::Relaxed);
        self.integrated_lufs_milli
            .store(i64::MIN, Ordering::Relaxed);
        self.loudness_gain_db_milli.store(0, Ordering::Relaxed);
        self.peak_l_milli.store(i64::MIN, Ordering::Relaxed);
        self.peak_r_milli.store(i64::MIN, Ordering::Relaxed);
        self.clip_gr_db.lock().clear();
    }

    fn store_db(cell: &AtomicI64, db: f64) {
        cell.store((db * 1000.0) as i64, Ordering::Relaxed);
    }

    /// Read a milli-dB cell back as dB.
    pub fn load_db(cell: &AtomicI64) -> Option<f64> {
        let v = cell.load(Ordering::Relaxed);
        (v != i64::MIN).then(|| v as f64 / 1000.0)
    }
}

impl Default for MixMeters {
    fn default() -> Self {
        MixMeters::new()
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

/// Per-clip streaming state: decoder + DSP chain, continuous across blocks.
struct ClipCtx {
    dec: Option<AudioDecoder>,
    /// Decoded source samples not yet consumed by the stepper.
    pending: Vec<f32>,
    /// Source µs of pending[0].
    pending_us: i64,
    step_phase: f64,
    chain: Option<StripChain>,
    /// Timeline µs this ctx expects the next block to start at; a mismatch
    /// (seek) forces a re-position.
    next_tl_us: i64,
    /// LRU tick of last use.
    last_used: u64,
    /// Decoder open failed for this span — render silence, don't retry.
    dead: bool,
}

/// Cap on simultaneously open audio decoders (spans overlap only briefly).
const CTX_CAP: usize = 8;

/// Renders a [`MixSpec`] block by block. Not thread-safe; owned by the audio
/// render thread (preview) or the export thread (M6).
pub struct MixRenderer {
    spec: Arc<MixSpec>,
    ctxs: HashMap<i64, ClipCtx>,
    ducker: Ducker,
    delay: StereoDelay,
    bus_comp: Option<Compressor>,
    limiter: Limiter,
    loudness: Option<ebur128::EbuR128>,
    /// Applied loudness makeup, linear; slewed toward target − integrated.
    loudness_gain: f32,
    meters: Arc<MixMeters>,
    tick: u64,
    // Scratch buffers reused across blocks.
    track_buf: [Vec<f32>; MIX_TRACKS],
    clip_buf: Vec<f32>,
    send_buf: Vec<f32>,
    wet_buf: Vec<f32>,
    duck_gains: Vec<f32>,
    master_buf: Vec<f32>,
}

impl MixRenderer {
    pub fn new(spec: Arc<MixSpec>, meters: Arc<MixMeters>) -> MixRenderer {
        let limiter = Limiter::new(spec.master.limiter_ceiling_dbtp);
        let bus_comp = spec
            .master
            .comp
            .map(|c| Compressor::new(c.threshold_db, c.ratio, c.makeup_db));
        let loudness =
            ebur128::EbuR128::new(2, AUDIO_RATE, ebur128::Mode::S | ebur128::Mode::I).ok();
        MixRenderer {
            spec,
            ctxs: HashMap::new(),
            ducker: Ducker::new(),
            delay: StereoDelay::new(),
            bus_comp,
            limiter,
            loudness,
            loudness_gain: 1.0,
            meters,
            tick: 0,
            track_buf: std::array::from_fn(|_| vec![0.0; BLOCK_FRAMES * 2]),
            clip_buf: vec![0.0; BLOCK_FRAMES * 2],
            send_buf: vec![0.0; BLOCK_FRAMES * 2],
            wet_buf: vec![0.0; BLOCK_FRAMES * 2],
            duck_gains: Vec::with_capacity(BLOCK_FRAMES),
            master_buf: vec![0.0; BLOCK_FRAMES * 2],
        }
    }

    /// Swap in a new spec (edit re-send). Keeps decoder/DSP state for clips
    /// whose span is unchanged, so edits elsewhere never click here.
    pub fn set_spec(&mut self, spec: Arc<MixSpec>) {
        let old = self.spec.clone();
        for (id, ctx) in self.ctxs.iter_mut() {
            let before = old.clips.iter().find(|c| c.clip_id == *id);
            let after = spec.clips.iter().find(|c| c.clip_id == *id);
            match (before, after) {
                (Some(b), Some(a)) if b == a => {
                    // Untouched — keep streaming state, refresh strip params
                    // (identical params are a no-op inside set_params).
                }
                (_, Some(a)) => {
                    // Changed: force a re-position; rebuild the chain params.
                    ctx.next_tl_us = i64::MIN;
                    ctx.dead = false;
                    match (&mut ctx.chain, &a.strip) {
                        (Some(ch), Some(p)) => ch.set_params(p),
                        (slot, Some(p)) => *slot = Some(StripChain::new(p)),
                        (slot, None) => *slot = None,
                    }
                }
                (_, None) => {}
            }
        }
        self.ctxs
            .retain(|id, _| spec.clips.iter().any(|c| c.clip_id == *id));
        if spec.master != self.spec.master {
            self.limiter = Limiter::new(spec.master.limiter_ceiling_dbtp);
            self.bus_comp = spec
                .master
                .comp
                .map(|c| Compressor::new(c.threshold_db, c.ratio, c.makeup_db));
        }
        self.spec = spec;
    }

    pub fn spec(&self) -> &Arc<MixSpec> {
        &self.spec
    }

    /// Reset all streaming state for a seek: decoders re-position lazily, the
    /// loudness measurement restarts (§5: the live meter measures from the
    /// current play start; export does the exact two-pass instead).
    pub fn reset_for_seek(&mut self) {
        for ctx in self.ctxs.values_mut() {
            ctx.next_tl_us = i64::MIN;
            ctx.pending.clear();
            ctx.step_phase = 0.0;
            ctx.dead = false;
            if let Some(ch) = ctx.chain.as_mut() {
                ch.reset();
            }
        }
        self.ducker.reset();
        self.delay.reset();
        self.limiter.reset();
        if let Some(c) = self.bus_comp.as_mut() {
            c.reset();
        }
        self.loudness =
            ebur128::EbuR128::new(2, AUDIO_RATE, ebur128::Mode::S | ebur128::Mode::I).ok();
        self.loudness_gain = dsp::db_to_lin(0.0);
        self.meters.reset();
    }

    /// Render one block: BLOCK_FRAMES output frames starting at timeline
    /// `tl_us`, at transport `rate` (1.0 for export). Returns the timeline µs
    /// after the block (tl_us + BLOCK_FRAMES·rate/48k). The output slice is
    /// valid until the next call.
    pub fn render_block(&mut self, tl_us: i64, rate: f64) -> (&[f32], i64) {
        self.tick += 1;
        let block_tl_us = (BLOCK_FRAMES as f64 * rate * 1e6 / AUDIO_RATE as f64) as i64;
        let tl_end = tl_us + block_tl_us;
        for b in self.track_buf.iter_mut() {
            b.iter_mut().for_each(|s| *s = 0.0);
        }
        self.send_buf.iter_mut().for_each(|s| *s = 0.0);

        let idxs = self.spec.clips_overlapping(tl_us, tl_end);
        let mut gr_updates: Vec<(i64, f32)> = Vec::new();
        for i in idxs {
            let clip = self.spec.clips[i].clone();
            let track = self.spec.tracks[clip.track];
            if !track.audible {
                continue;
            }
            self.render_clip(&clip, tl_us, rate, block_tl_us);
            // clip_buf now holds the clip's block: apply mode, gain, chain.
            apply_channel_mode(clip.channel_mode, &mut self.clip_buf);
            if (clip.gain - 1.0).abs() > 1e-6 {
                for s in self.clip_buf.iter_mut() {
                    *s *= clip.gain;
                }
            }
            let mut send = 0.0f32;
            if let Some(ctx) = self.ctxs.get_mut(&clip.clip_id) {
                if let Some(chain) = ctx.chain.as_mut() {
                    let gr = chain.process(&mut self.clip_buf);
                    gr_updates.push((clip.clip_id, gr));
                    send = chain.send();
                }
            }
            // Fades/crossfades + pan, then accumulate into the lane.
            let (pl, pr) = pan_gains(clip.pan);
            let buf = &mut self.track_buf[clip.track];
            for f in 0..BLOCK_FRAMES {
                let t = tl_us + (f as i64 * block_tl_us) / BLOCK_FRAMES as i64;
                let g = clip.ramp_gain_at(t);
                let l = self.clip_buf[f * 2] * g;
                let r = self.clip_buf[f * 2 + 1] * g;
                buf[f * 2] += l * pl;
                buf[f * 2 + 1] += r * pr;
                if send > 0.0 {
                    self.send_buf[f * 2] += l * send;
                    self.send_buf[f * 2 + 1] += r * send;
                }
            }
        }
        {
            let mut gr = self.meters.clip_gr_db.lock();
            gr.clear();
            for (id, v) in gr_updates {
                gr.insert(id, v);
            }
        }

        // Ducking sidechain (§5): voice lanes drive duck-marked lanes.
        let duck_needed = self
            .spec
            .tracks
            .iter()
            .enumerate()
            .any(|(i, t)| t.duck && t.audible && self.track_buf[i].iter().any(|&s| s != 0.0));
        if duck_needed {
            // Voice = V1 + A1 lanes (§5 "summed voice tracks").
            self.clip_buf.iter_mut().for_each(|s| *s = 0.0);
            for lane in [TR_V1, TR_A1] {
                if self.spec.tracks[lane].audible {
                    for (d, s) in self.clip_buf.iter_mut().zip(&self.track_buf[lane]) {
                        *d += *s;
                    }
                }
            }
            self.ducker.gains(&self.clip_buf, &mut self.duck_gains);
            for (i, t) in self.spec.tracks.iter().enumerate() {
                if t.duck && t.audible {
                    for f in 0..BLOCK_FRAMES {
                        let g = self.duck_gains.get(f).copied().unwrap_or(1.0);
                        self.track_buf[i][f * 2] *= g;
                        self.track_buf[i][f * 2 + 1] *= g;
                    }
                }
            }
        } else {
            // Keep the follower tracking voice so re-entry attacks correctly.
            self.ducker.reset();
        }

        // Track gain/pan/sends → master + delay input.
        self.master_buf.iter_mut().for_each(|s| *s = 0.0);
        for (i, t) in self.spec.tracks.iter().enumerate() {
            if !t.audible {
                continue;
            }
            let (pl, pr) = pan_gains(t.pan);
            let buf = &self.track_buf[i];
            for f in 0..BLOCK_FRAMES {
                let l = buf[f * 2] * t.gain * pl;
                let r = buf[f * 2 + 1] * t.gain * pr;
                self.master_buf[f * 2] += l;
                self.master_buf[f * 2 + 1] += r;
                if t.send > 0.0 {
                    self.send_buf[f * 2] += l * t.send;
                    self.send_buf[f * 2 + 1] += r * t.send;
                }
            }
        }

        // Delay bus (§5): wet added on top of the dry master.
        self.wet_buf.iter_mut().for_each(|s| *s = 0.0);
        self.delay.process(&self.send_buf, &mut self.wet_buf);
        for (d, w) in self.master_buf.iter_mut().zip(&self.wet_buf) {
            *d += *w;
        }

        // Master bus (§5): comp → loudness normalization → limiter.
        if let Some(c) = self.bus_comp.as_mut() {
            c.process(&mut self.master_buf);
        }
        self.update_loudness();
        if self.spec.master.loudness_enabled {
            for s in self.master_buf.iter_mut() {
                *s *= self.loudness_gain;
            }
        }
        self.limiter.process(&mut self.master_buf);

        // Meters: feed the (post-gain, post-limit) program to R128 + peaks.
        if let Some(lm) = self.loudness.as_mut() {
            let _ = lm.add_frames_f32(&self.master_buf);
            if let Ok(s) = lm.loudness_shortterm() {
                if s.is_finite() {
                    MixMeters::store_db(&self.meters.short_term_lufs_milli, s);
                }
            }
            if let Ok(i) = lm.loudness_global() {
                if i.is_finite() {
                    MixMeters::store_db(&self.meters.integrated_lufs_milli, i);
                }
            }
        }
        let (mut pk_l, mut pk_r) = (0.0f32, 0.0f32);
        for f in self.master_buf.chunks_exact(2) {
            pk_l = pk_l.max(f[0].abs());
            pk_r = pk_r.max(f[1].abs());
        }
        MixMeters::store_db(
            &self.meters.peak_l_milli,
            20.0 * (pk_l.max(1e-6) as f64).log10(),
        );
        MixMeters::store_db(
            &self.meters.peak_r_milli,
            20.0 * (pk_r.max(1e-6) as f64).log10(),
        );

        self.evict_ctxs();
        (&self.master_buf, tl_end)
    }

    /// §5 loudness normalization: *static* makeup toward the target from the
    /// running integrated measurement — slewed slowly (≤ ~3 dB/s) so it
    /// converges to a constant instead of pumping. Export replaces this with
    /// the exact two-pass value.
    fn update_loudness(&mut self) {
        if !self.spec.master.loudness_enabled {
            MixMeters::store_db(&self.meters.loudness_gain_db_milli, 0.0);
            return;
        }
        let measured = self
            .loudness
            .as_mut()
            .and_then(|l| l.loudness_global().ok())
            .filter(|v| v.is_finite() && *v > -60.0);
        let current_db = 20.0 * (self.loudness_gain as f64).log10();
        if let Some(integrated) = measured {
            // The meter reads post-gain program: the remaining error is
            // target − integrated, on top of what's already applied.
            let target_db = (current_db + (self.spec.master.loudness_target_lufs - integrated))
                .clamp(-12.0, 12.0);
            let step = 3.0 * BLOCK_FRAMES as f64 / AUDIO_RATE as f64; // dB per block
            let next = current_db + (target_db - current_db).clamp(-step, step);
            self.loudness_gain = dsp::db_to_lin(next);
            MixMeters::store_db(&self.meters.loudness_gain_db_milli, next);
        } else {
            MixMeters::store_db(&self.meters.loudness_gain_db_milli, current_db);
        }
    }

    /// Fill `clip_buf` with this clip's decoded, speed/rate-stepped block.
    /// Frames outside the clip's span are zero.
    fn render_clip(&mut self, clip: &MixClip, tl_us: i64, rate: f64, block_tl_us: i64) {
        self.clip_buf.iter_mut().for_each(|s| *s = 0.0);
        let ctx = self.ctxs.entry(clip.clip_id).or_insert_with(|| ClipCtx {
            dec: None,
            pending: Vec::new(),
            pending_us: 0,
            step_phase: 0.0,
            chain: clip.strip.as_ref().map(StripChain::new),
            next_tl_us: i64::MIN,
            last_used: 0,
            dead: false,
        });
        ctx.last_used = self.tick;
        if ctx.dead {
            return;
        }

        // (Re)position on discontinuity.
        if ctx.next_tl_us != tl_us {
            let start_t = tl_us.max(clip.tl_start_us);
            let src_us =
                clip.source_offset_us + ((start_t - clip.tl_start_us) as f64 * clip.speed) as i64;
            if ctx.dec.is_none() {
                match AudioDecoder::open(&clip.path) {
                    Ok(d) => ctx.dec = Some(d),
                    Err(e) => {
                        log::debug!("mix: no audio for {}: {e}", clip.path.display());
                        ctx.dead = true;
                        return;
                    }
                }
            }
            ctx.pending.clear();
            ctx.pending_us = src_us;
            ctx.step_phase = 0.0;
            if let Some(d) = ctx.dec.as_mut() {
                if d.seek(src_us).is_err() {
                    ctx.dead = true;
                    return;
                }
            }
            if let Some(ch) = ctx.chain.as_mut() {
                ch.reset();
            }
        }
        ctx.next_tl_us = tl_us + block_tl_us;

        // Step source samples into the block, frame by frame.
        let e = (rate * clip.speed).max(1e-9);
        for f in 0..BLOCK_FRAMES {
            let t = tl_us + (f as i64 * block_tl_us) / BLOCK_FRAMES as i64;
            if t < clip.tl_start_us || t >= clip.tl_end_us() {
                continue;
            }
            // Ensure pending covers step_phase.
            loop {
                let idx = ctx.step_phase as usize;
                if idx < ctx.pending.len() / 2 {
                    self.clip_buf[f * 2] = ctx.pending[idx * 2];
                    self.clip_buf[f * 2 + 1] = ctx.pending[idx * 2 + 1];
                    ctx.step_phase += e;
                    break;
                }
                // Drop consumed frames to keep pending bounded.
                let consumed = (ctx.pending.len() / 2).min(ctx.step_phase as usize);
                if consumed > 0 {
                    ctx.pending.drain(..consumed * 2);
                    ctx.step_phase -= consumed as f64;
                    ctx.pending_us += (consumed as f64 * 1e6 / AUDIO_RATE as f64) as i64;
                }
                let Some(d) = ctx.dec.as_mut() else {
                    return;
                };
                match d.next_chunk() {
                    Ok(Some(chunk)) => {
                        // Discard pre-target chunks after a seek.
                        let want = ctx.pending_us;
                        let mut samples = chunk.samples;
                        if ctx.pending.is_empty() && chunk.start_us < want {
                            let skip =
                                ((want - chunk.start_us) as f64 * AUDIO_RATE as f64 / 1e6) as usize;
                            if skip * 2 >= samples.len() {
                                continue;
                            }
                            samples.drain(..skip * 2);
                        }
                        ctx.pending.extend_from_slice(&samples);
                    }
                    Ok(None) | Err(_) => {
                        // Source ran out inside the span: rest stays silent.
                        return;
                    }
                }
            }
        }
    }

    /// Drop LRU contexts beyond the cap (their clips are far from the head).
    fn evict_ctxs(&mut self) {
        while self.ctxs.len() > CTX_CAP {
            if let Some((&id, _)) = self.ctxs.iter().min_by_key(|(_, c)| c.last_used) {
                self.ctxs.remove(&id);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn clip(start: i64, dur: i64) -> MixClip {
        MixClip {
            clip_id: 1,
            track: TR_V1,
            path: PathBuf::from("/nonexistent"),
            tl_start_us: start,
            tl_dur_us: dur,
            source_offset_us: 0,
            speed: 1.0,
            gain: 1.0,
            pan: 0.0,
            channel_mode: ChannelMode::Stereo,
            head_fade_us: 0,
            head_xfade: false,
            tail_fade_us: 0,
            tail_xfade: false,
            strip: None,
            voice: true,
        }
    }

    #[test]
    fn overlap_lookup() {
        let mut spec = MixSpec::empty();
        spec.clips.push(clip(0, 1_000_000));
        spec.clips.push(clip(500_000, 1_000_000));
        assert_eq!(spec.clips_overlapping(0, 100), vec![0]);
        assert_eq!(spec.clips_overlapping(600_000, 600_100), vec![0, 1]);
        assert_eq!(spec.clips_overlapping(1_400_000, 1_400_100), vec![1]);
        assert!(spec.clips_overlapping(2_000_000, 2_000_100).is_empty());
    }

    #[test]
    fn micro_fade_floors_hard_edges() {
        let c = clip(0, 1_000_000);
        // Hard head edge gets the 5 ms micro-fade (§5).
        assert_eq!(c.ramp_gain_at(0), 0.0);
        assert!((c.ramp_gain_at(2_500) - 0.5).abs() < 0.01);
        assert_eq!(c.ramp_gain_at(500_000), 1.0);
        // Hard tail edge mirrored.
        assert!(c.ramp_gain_at(999_999) < 0.01);
    }

    #[test]
    fn plain_fade_ramps_linear() {
        let mut c = clip(0, 1_000_000);
        c.head_fade_us = 100_000;
        assert!((c.ramp_gain_at(50_000) - 0.5).abs() < 0.01);
        assert_eq!(c.ramp_gain_at(100_000), 1.0);
    }

    #[test]
    fn crossfade_halves_sum_to_constant_power() {
        // Two clips crossfading over [0.9 s, 1.1 s].
        let mut a = clip(0, 1_100_000);
        a.tail_fade_us = 200_000;
        a.tail_xfade = true;
        let mut b = clip(900_000, 1_000_000);
        b.head_fade_us = 200_000;
        b.head_xfade = true;
        for t in [900_000i64, 950_000, 1_000_000, 1_050_000, 1_099_000] {
            let ga = a.ramp_gain_at(t);
            let gb = b.ramp_gain_at(t);
            let power = ga * ga + gb * gb;
            assert!((power - 1.0).abs() < 0.05, "t={t}: {ga} {gb} -> {power}");
        }
        // Midpoint is −3 dB each.
        assert!((a.ramp_gain_at(1_000_000) - 0.707).abs() < 0.02);
    }

    #[test]
    fn pan_law_constant_power() {
        let (l, r) = pan_gains(0.0);
        assert!((l - r).abs() < 1e-6);
        assert!((l * l + r * r - 1.0).abs() < 1e-5);
        let (l, r) = pan_gains(-1.0);
        assert!(l > 0.99 && r < 0.01);
    }

    #[test]
    fn channel_modes() {
        let mut s = vec![1.0, -1.0, 0.5, 0.25];
        apply_channel_mode(ChannelMode::Sum, &mut s);
        assert_eq!(s, vec![0.0, 0.0, 0.375, 0.375]);
        let mut s = vec![1.0, -1.0];
        apply_channel_mode(ChannelMode::Left, &mut s);
        assert_eq!(s, vec![1.0, 1.0]);
        let mut s = vec![1.0, -1.0];
        apply_channel_mode(ChannelMode::Right, &mut s);
        assert_eq!(s, vec![-1.0, -1.0]);
    }
}
