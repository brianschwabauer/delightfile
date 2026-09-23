//! Audio DSP building blocks for the M5 pipeline (PLAN.md §5).
//!
//! Everything runs at 48 kHz stereo, interleaved `f32` (`dv_media::AUDIO_RATE`).
//! These are hand-written, textbook effects — RBJ Audio EQ Cookbook biquads, a
//! feed-forward peak compressor, a feedback delay bus, an envelope-follower
//! ducker, and a lookahead true-peak limiter — plus `nnnoiseless` for voice
//! denoise. The shareable per-clip *treatment* chain ([`StripChain`]) wires the
//! filters, EQ and compressor in §5's fixed order; per-clip *mixing* (gain,
//! pan, fades, channel mode) is applied elsewhere and is not part of this file.

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use dv_core::model::StripParams;
use dv_media::AUDIO_RATE;
use nnnoiseless::DenoiseState;

/// Sample rate of the internal mix (§5): 48 kHz.
const FS: f64 = AUDIO_RATE as f64;

/// Mixer block size in sample-frames: 10 ms @ 48 kHz, also nnnoiseless's frame size.
pub const BLOCK_FRAMES: usize = 480;

/// Convert decibels to a linear amplitude factor.
pub fn db_to_lin(db: f64) -> f32 {
    db_to_lin_f64(db) as f32
}

fn db_to_lin_f64(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Linear amplitude to dBFS, floored so silence maps to a finite, very low dB.
fn lin_to_db(x: f64) -> f64 {
    20.0 * x.max(1e-9).log10()
}

/// One-pole smoothing coefficient for a time constant of `secs` at [`FS`].
/// `env = target + (env - target) * coef` decays ~63 % over one `secs`.
fn smoothing_coef(secs: f64) -> f64 {
    (-1.0 / (secs * FS)).exp()
}

/// Equal-power crossfade gains at progress `t ∈ [0,1]`: `(outgoing, incoming)`.
/// `t=0 → (1,0)`; `t=1 → (0,1)`; at `t=0.5` both are ≈ 0.707 (−3 dB, constant
/// power). Used for butted-cut crossfades (§5).
pub fn equal_power(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let a = t * std::f32::consts::FRAC_PI_2;
    (a.cos(), a.sin())
}

/// RBJ biquad, stereo (independent state per channel), 2nd order — 12 dB/oct
/// for the HPF/LPF (§5). Coefficients follow the RBJ Audio EQ Cookbook; state
/// is a transposed direct-form II per channel.
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    s1: [f64; 2],
    s2: [f64; 2],
}

impl Biquad {
    fn from_unnormalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Biquad {
        Biquad {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            s1: [0.0; 2],
            s2: [0.0; 2],
        }
    }

    /// 2nd-order high-pass, `Q = 1/√2` (Butterworth, 12 dB/oct).
    pub fn hpf(hz: f64) -> Biquad {
        let (_w0, cos_w0, sin_w0) = omega(hz);
        let alpha = sin_w0 / (2.0 * FRAC_1_SQRT_2);
        Biquad::from_unnormalized(
            (1.0 + cos_w0) / 2.0,
            -(1.0 + cos_w0),
            (1.0 + cos_w0) / 2.0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// 2nd-order low-pass, `Q = 1/√2` (Butterworth, 12 dB/oct).
    pub fn lpf(hz: f64) -> Biquad {
        let (_w0, cos_w0, sin_w0) = omega(hz);
        let alpha = sin_w0 / (2.0 * FRAC_1_SQRT_2);
        Biquad::from_unnormalized(
            (1.0 - cos_w0) / 2.0,
            1.0 - cos_w0,
            (1.0 - cos_w0) / 2.0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// Low-shelf, slope `S = 1`.
    pub fn low_shelf(hz: f64, gain_db: f64) -> Biquad {
        let (_w0, cos_w0, sin_w0) = omega(hz);
        let a = db_to_lin_f64(gain_db / 2.0); // amplitude = 10^(dB/40)
        let alpha = shelf_alpha(sin_w0);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Biquad::from_unnormalized(
            a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
            (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// High-shelf, slope `S = 1`.
    pub fn high_shelf(hz: f64, gain_db: f64) -> Biquad {
        let (_w0, cos_w0, sin_w0) = omega(hz);
        let a = db_to_lin_f64(gain_db / 2.0);
        let alpha = shelf_alpha(sin_w0);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Biquad::from_unnormalized(
            a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
            (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// Peaking (bell) filter with adjustable `q`.
    pub fn peaking(hz: f64, gain_db: f64, q: f64) -> Biquad {
        let (_w0, cos_w0, sin_w0) = omega(hz);
        let a = db_to_lin_f64(gain_db / 2.0);
        let alpha = sin_w0 / (2.0 * q);
        Biquad::from_unnormalized(
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        )
    }

    /// Filter a stereo interleaved buffer in place.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        for frame in interleaved.chunks_exact_mut(2) {
            frame[0] = self.tick(0, frame[0]);
            frame[1] = self.tick(1, frame[1]);
        }
    }

    fn tick(&mut self, ch: usize, x: f32) -> f32 {
        let x = x as f64;
        let y = self.b0 * x + self.s1[ch];
        self.s1[ch] = self.b1 * x - self.a1 * y + self.s2[ch];
        self.s2[ch] = self.b2 * x - self.a2 * y;
        y as f32
    }

    /// Clear filter state (leaves coefficients intact).
    pub fn reset(&mut self) {
        self.s1 = [0.0; 2];
        self.s2 = [0.0; 2];
    }

    /// Magnitude of the transfer function at `hz` (linear).
    pub fn magnitude(&self, hz: f64) -> f64 {
        let w = 2.0 * PI * hz / FS;
        // z^-1 = e^{-jw}
        let (c1, s1) = ((-w).cos(), (-w).sin());
        let (c2, s2) = ((-2.0 * w).cos(), (-2.0 * w).sin());
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = self.b1 * s1 + self.b2 * s2;
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = self.a1 * s1 + self.a2 * s2;
        ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt()
    }
}

fn omega(hz: f64) -> (f64, f64, f64) {
    let w0 = 2.0 * PI * hz / FS;
    (w0, w0.cos(), w0.sin())
}

/// Shelf `alpha` for slope `S = 1`: the `(1/S − 1)` term vanishes, leaving
/// `alpha = sin(w0)/2 · √2`.
fn shelf_alpha(sin_w0: f64) -> f64 {
    sin_w0 / 2.0 * 2f64.sqrt()
}

/// §5 compressor: threshold/ratio/makeup are adjustable; attack (5 ms),
/// release (100 ms), soft knee (~6 dB) are fixed. Feed-forward detection on the
/// stereo-linked peak of `|L|, |R|`; gain reduction is smoothed in the dB domain.
pub struct Compressor {
    threshold_db: f64,
    ratio: f64,
    makeup_lin: f64,
    att: f64,
    rel: f64,
    /// Current gain reduction in dB (≥ 0), the smoothed detector state.
    env: f64,
    last_gr: f32,
}

/// Fixed soft-knee width in dB (§5).
const COMP_KNEE_DB: f64 = 6.0;

/// Static compressor transfer: output level (dB) for input `x_db` at the
/// given threshold/ratio, with the same [`COMP_KNEE_DB`] soft knee the
/// [`Compressor`] runs (makeup not included). Shared with the inspector's
/// compression graphic so the picture matches the sound.
pub fn comp_transfer_db(threshold_db: f64, ratio: f64, x_db: f64) -> f64 {
    let w = COMP_KNEE_DB;
    let over = x_db - threshold_db;
    if over <= -w / 2.0 {
        x_db
    } else if over >= w / 2.0 {
        threshold_db + over / ratio
    } else {
        let k = over + w / 2.0;
        x_db + (1.0 / ratio - 1.0) * k * k / (2.0 * w)
    }
}

impl Compressor {
    pub fn new(threshold_db: f64, ratio: f64, makeup_db: f64) -> Compressor {
        Compressor {
            threshold_db,
            ratio,
            makeup_lin: db_to_lin_f64(makeup_db),
            att: smoothing_coef(0.005),
            rel: smoothing_coef(0.100),
            env: 0.0,
            last_gr: 0.0,
        }
    }

    /// Compress a stereo interleaved buffer in place.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        let mut max_gr = 0.0f64;
        for frame in interleaved.chunks_exact_mut(2) {
            let l = frame[0] as f64;
            let r = frame[1] as f64;
            let peak = l.abs().max(r.abs());
            let target = self.static_gr(lin_to_db(peak));
            let coef = if target > self.env {
                self.att
            } else {
                self.rel
            };
            self.env = target + (self.env - target) * coef;
            if self.env > max_gr {
                max_gr = self.env;
            }
            let g = db_to_lin_f64(-self.env) * self.makeup_lin;
            frame[0] = (l * g) as f32;
            frame[1] = (r * g) as f32;
        }
        self.last_gr = max_gr as f32;
    }

    /// Static gain reduction (dB, ≥ 0) for an input level, with a soft knee of
    /// width [`COMP_KNEE_DB`] centered on the threshold.
    fn static_gr(&self, x_db: f64) -> f64 {
        (x_db - comp_transfer_db(self.threshold_db, self.ratio, x_db)).max(0.0)
    }

    /// Max gain reduction (dB, ≥ 0) seen during the most recent `process()` call.
    pub fn gain_reduction_db(&self) -> f32 {
        self.last_gr
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.last_gr = 0.0;
    }
}

/// nnnoiseless voice denoise (§5), stereo via two [`DenoiseState`]. nnnoiseless
/// works on 480-sample frames in i16 range, so channels are scaled ×32767 in
/// and ÷32767 out; the first frame is warm-up and can be ignored.
pub struct Denoiser {
    states: [Box<DenoiseState<'static>>; 2],
    scratch_in: Vec<f32>,
    scratch_out: Vec<f32>,
}

const I16_SCALE: f32 = 32767.0;

impl Denoiser {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Denoiser {
        Denoiser {
            states: [DenoiseState::new(), DenoiseState::new()],
            scratch_in: vec![0.0; BLOCK_FRAMES],
            scratch_out: vec![0.0; BLOCK_FRAMES],
        }
    }

    /// Denoise one block. `interleaved.len()` MUST be `BLOCK_FRAMES * 2`.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        debug_assert_eq!(interleaved.len(), BLOCK_FRAMES * 2);
        for (ch, state) in self.states.iter_mut().enumerate() {
            for (i, frame) in interleaved.chunks_exact(2).enumerate() {
                self.scratch_in[i] = frame[ch] * I16_SCALE;
            }
            state.process_frame(&mut self.scratch_out, &self.scratch_in);
            for (i, frame) in interleaved.chunks_exact_mut(2).enumerate() {
                frame[ch] = self.scratch_out[i] / I16_SCALE;
            }
        }
    }

    pub fn reset(&mut self) {
        self.states = [DenoiseState::new(), DenoiseState::new()];
    }
}

/// The shareable treatment chain in §5's fixed order:
/// denoise → HPF → EQ (low shelf 120 Hz, peaking, high shelf 8 kHz) → LPF →
/// compressor. Per-clip mixing (channel mode / gain / pan / fades) is not here.
/// `comp_ratio == 1.0` bypasses the compressor; `hpf_on`/`lpf_on` gate those
/// filters.
pub struct StripChain {
    denoise: Option<Denoiser>,
    hpf: Option<Biquad>,
    low_shelf: Biquad,
    peaking: Biquad,
    high_shelf: Biquad,
    lpf: Option<Biquad>,
    comp: Option<Compressor>,
    send: f32,
}

/// Fixed EQ band frequencies (§5).
const EQ_LOW_HZ: f64 = 120.0;
const EQ_HIGH_HZ: f64 = 8_000.0;
/// Musical default Q for the peaking band (freq is user-adjustable).
const EQ_MID_Q: f64 = 1.0;

impl StripChain {
    pub fn new(params: &StripParams) -> StripChain {
        StripChain {
            denoise: params.denoise.then(Denoiser::new),
            hpf: params.hpf_on.then(|| Biquad::hpf(params.hpf_hz)),
            low_shelf: Biquad::low_shelf(EQ_LOW_HZ, params.eq_low_db),
            peaking: Biquad::peaking(params.eq_mid_hz, params.eq_mid_db, EQ_MID_Q),
            high_shelf: Biquad::high_shelf(EQ_HIGH_HZ, params.eq_high_db),
            lpf: params.lpf_on.then(|| Biquad::lpf(params.lpf_hz)),
            comp: (params.comp_ratio > 1.0).then(|| {
                Compressor::new(
                    params.comp_threshold_db,
                    params.comp_ratio,
                    params.comp_makeup_db,
                )
            }),
            send: params.send as f32,
        }
    }

    /// Reconfigure in place, rebuilding all coefficients and clearing state.
    pub fn set_params(&mut self, params: &StripParams) {
        *self = StripChain::new(params);
    }

    /// Process one block. `len` must be `BLOCK_FRAMES*2` when denoise is on; any
    /// even `len` otherwise. Returns compressor gain reduction in dB (0.0 off).
    pub fn process(&mut self, interleaved: &mut [f32]) -> f32 {
        if let Some(denoise) = self.denoise.as_mut() {
            denoise.process(interleaved);
        }
        if let Some(hpf) = self.hpf.as_mut() {
            hpf.process(interleaved);
        }
        self.low_shelf.process(interleaved);
        self.peaking.process(interleaved);
        self.high_shelf.process(interleaved);
        if let Some(lpf) = self.lpf.as_mut() {
            lpf.process(interleaved);
        }
        match self.comp.as_mut() {
            Some(comp) => {
                comp.process(interleaved);
                comp.gain_reduction_db()
            }
            None => 0.0,
        }
    }

    /// Delay-bus send level from the params (0–1).
    pub fn send(&self) -> f32 {
        self.send
    }

    pub fn reset(&mut self) {
        if let Some(denoise) = self.denoise.as_mut() {
            denoise.reset();
        }
        if let Some(hpf) = self.hpf.as_mut() {
            hpf.reset();
        }
        self.low_shelf.reset();
        self.peaking.reset();
        self.high_shelf.reset();
        if let Some(lpf) = self.lpf.as_mut() {
            lpf.reset();
        }
        if let Some(comp) = self.comp.as_mut() {
            comp.reset();
        }
    }
}

/// Composite magnitude of a strip's LTI tone stages (HPF → EQ → LPF) at `hz`,
/// in dB — the inspector's response-curve graphic. Built from the exact same
/// coefficient math the audio path runs, so the picture can't drift from the
/// sound. Denoise and the compressor aren't LTI and are excluded.
pub fn strip_response_db(params: &StripParams, hz: f64) -> f64 {
    let mut mag = Biquad::low_shelf(EQ_LOW_HZ, params.eq_low_db).magnitude(hz)
        * Biquad::peaking(params.eq_mid_hz, params.eq_mid_db, EQ_MID_Q).magnitude(hz)
        * Biquad::high_shelf(EQ_HIGH_HZ, params.eq_high_db).magnitude(hz);
    if params.hpf_on {
        mag *= Biquad::hpf(params.hpf_hz).magnitude(hz);
    }
    if params.lpf_on {
        mag *= Biquad::lpf(params.lpf_hz).magnitude(hz);
    }
    lin_to_db(mag)
}

/// One channel of the delay bus: a delay line with a one-pole damping LPF in
/// the feedback path.
struct DelayLine {
    buf: Vec<f32>,
    idx: usize,
    lp: f32,
}

impl DelayLine {
    fn new(delay_samples: usize) -> DelayLine {
        DelayLine {
            buf: vec![0.0; delay_samples.max(1)],
            idx: 0,
            lp: 0.0,
        }
    }

    fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let delayed = self.buf[self.idx];
        self.lp += damp * (delayed - self.lp);
        self.buf[self.idx] = input + feedback * self.lp;
        self.idx = (self.idx + 1) % self.buf.len();
        delayed
    }

    fn reset(&mut self) {
        self.buf.iter_mut().for_each(|s| *s = 0.0);
        self.idx = 0;
        self.lp = 0.0;
    }
}

/// Global delay bus (§5): a simple stereo feedback delay with damping. Fixed
/// musical defaults — ~320 ms delay, feedback ~0.35, a one-pole ~4 kHz damping
/// LPF in the feedback path, and a small L/R time offset for width.
pub struct StereoDelay {
    left: DelayLine,
    right: DelayLine,
    feedback: f32,
    damp: f32,
}

impl StereoDelay {
    #[allow(clippy::new_without_default)]
    pub fn new() -> StereoDelay {
        let left_ms = 0.320;
        let right_ms = 0.335; // +15 ms offset for stereo width
        StereoDelay {
            left: DelayLine::new((left_ms * FS) as usize),
            right: DelayLine::new((right_ms * FS) as usize),
            feedback: 0.35,
            // one-pole LPF at ~4 kHz: coef = 1 - e^{-2π fc/Fs}
            damp: 1.0 - (-2.0 * PI * 4_000.0 / FS).exp() as f32,
        }
    }

    /// Feed `input` (the block's summed bus sends) and ADD the wet signal into
    /// `out`. `input.len() == out.len()`, any even length.
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) {
        for (inp, o) in input.chunks_exact(2).zip(out.chunks_exact_mut(2)) {
            o[0] += self.left.tick(inp[0], self.feedback, self.damp);
            o[1] += self.right.tick(inp[1], self.feedback, self.damp);
        }
    }

    pub fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
    }
}

/// Ducking gain computer (§5): an envelope follower on the summed voice signal
/// drives up to −12 dB of gain reduction, 50 ms attack / 500 ms release. Below
/// −40 dBFS the gain is 1.0; it ramps linearly-in-dB to −12 dB as the voice
/// envelope approaches −20 dBFS.
pub struct Ducker {
    att: f64,
    rel: f64,
    /// Current gain reduction in dB (≥ 0).
    red: f64,
}

/// Voice envelope level (dBFS) below which no ducking is applied.
const DUCK_LOW_DB: f64 = -40.0;
/// Voice envelope level (dBFS) at/above which ducking is at its maximum.
const DUCK_HIGH_DB: f64 = -20.0;
/// Maximum ducking gain reduction in dB.
const DUCK_MAX_DB: f64 = 12.0;

impl Ducker {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Ducker {
        Ducker {
            att: smoothing_coef(0.050),
            rel: smoothing_coef(0.500),
            red: 0.0,
        }
    }

    /// Compute per-frame linear gains (`out.len() == voice.len()/2`, cleared and
    /// filled) from this block's voice mix.
    pub fn gains(&mut self, voice_interleaved: &[f32], out: &mut Vec<f32>) {
        out.clear();
        for frame in voice_interleaved.chunks_exact(2) {
            let level = 0.5 * (frame[0].abs() + frame[1].abs()) as f64;
            let level_db = lin_to_db(level);
            let t = ((level_db - DUCK_LOW_DB) / (DUCK_HIGH_DB - DUCK_LOW_DB)).clamp(0.0, 1.0);
            let target = t * DUCK_MAX_DB;
            let coef = if target > self.red {
                self.att
            } else {
                self.rel
            };
            self.red = target + (self.red - target) * coef;
            out.push(db_to_lin(-self.red));
        }
    }

    pub fn reset(&mut self) {
        self.red = 0.0;
    }
}

/// Master true-peak limiter (§5): ceiling in dBTP (default use is −1.0).
/// 4× oversampled (linear-interp) peak detection with a small fixed lookahead;
/// the internal delay is not compensated. Fast attack, ~50 ms release, and gain
/// smoothing so it never clicks.
pub struct Limiter {
    ceiling: f32,
    /// Lookahead delay line (interleaved stereo), and its write cursor.
    delay: Vec<f32>,
    idx: usize,
    /// Per-frame required gains for the samples currently in the lookahead
    /// window (parallel to `delay`); the applied gain tracks their minimum.
    req_ring: Vec<f32>,
    /// Previous input sample per channel, for inter-sample interpolation.
    prev: [f32; 2],
    gain: f32,
    rel: f32,
}

/// Lookahead in sample-frames (~1.3 ms @ 48 kHz).
const LIMIT_LOOKAHEAD: usize = 64;
/// Oversampling factor for true-peak estimation.
const LIMIT_OVERSAMPLE: usize = 4;

impl Limiter {
    pub fn new(ceiling_dbtp: f64) -> Limiter {
        Limiter {
            ceiling: db_to_lin(ceiling_dbtp),
            delay: vec![0.0; LIMIT_LOOKAHEAD * 2],
            req_ring: vec![1.0; LIMIT_LOOKAHEAD],
            idx: 0,
            prev: [0.0; 2],
            gain: 1.0,
            rel: 1.0 - smoothing_coef(0.050) as f32, // ~50 ms release
        }
    }

    /// Limit a stereo interleaved buffer in place. The applied gain snaps down
    /// to the minimum gain required across the lookahead window (so the peak is
    /// already attenuated before it exits the delay) and recovers with a slow
    /// release; the min-hold keeps the gain from rippling per audio cycle.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        for frame in interleaved.chunks_exact_mut(2) {
            // True-peak estimate of the incoming (future) frame via 4× linear
            // interpolation from the previous sample on each channel.
            let mut tp = 0.0f32;
            for (ch, &cur) in frame.iter().enumerate() {
                let prev = self.prev[ch];
                for k in 0..LIMIT_OVERSAMPLE {
                    let f = k as f32 / LIMIT_OVERSAMPLE as f32;
                    tp = tp.max((prev + (cur - prev) * f).abs());
                }
                tp = tp.max(cur.abs());
            }
            self.prev = [frame[0], frame[1]];

            let req = if tp > self.ceiling {
                self.ceiling / tp
            } else {
                1.0
            };
            self.req_ring[self.idx] = req;
            let target = self.req_ring.iter().copied().fold(1.0f32, f32::min);
            if target < self.gain {
                self.gain = target; // instant attack to the window minimum
            } else {
                self.gain += self.rel * (target - self.gain); // slow release
            }

            // Emit the delayed frame at the current (already-reduced) gain, then
            // push the incoming frame into the lookahead line.
            let out_l = self.delay[self.idx * 2] * self.gain;
            let out_r = self.delay[self.idx * 2 + 1] * self.gain;
            self.delay[self.idx * 2] = frame[0];
            self.delay[self.idx * 2 + 1] = frame[1];
            self.idx = (self.idx + 1) % LIMIT_LOOKAHEAD;
            frame[0] = out_l;
            frame[1] = out_r;
        }
    }

    pub fn reset(&mut self) {
        self.delay.iter_mut().for_each(|s| *s = 0.0);
        self.req_ring.iter_mut().for_each(|r| *r = 1.0);
        self.idx = 0;
        self.prev = [0.0; 2];
        self.gain = 1.0;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point
    use super::*;

    const RATE: f64 = FS;

    /// Interleave a mono sine at `hz` with peak amplitude `amp` into `frames`
    /// stereo frames.
    fn sine(hz: f64, amp: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = (2.0 * PI * hz * n as f64 / RATE).sin() as f32 * amp;
            v.push(s);
            v.push(s);
        }
        v
    }

    /// Peak absolute value of one channel (channel 0) of an interleaved buffer.
    fn peak(interleaved: &[f32]) -> f32 {
        interleaved
            .chunks_exact(2)
            .map(|f| f[0].abs())
            .fold(0.0, f32::max)
    }

    fn db(x: f32) -> f64 {
        20.0 * (x.max(1e-9) as f64).log10()
    }

    #[test]
    fn strip_response_curve_matches_params() {
        // Flat params → 0 dB everywhere the ear cares about.
        let flat = StripParams::default();
        for hz in [50.0, 1_000.0, 10_000.0] {
            assert!(strip_response_db(&flat, hz).abs() < 0.05, "flat at {hz}");
        }
        // 120 Hz HPF: strong cut an octave below, transparent well above.
        let p = StripParams {
            hpf_on: true,
            hpf_hz: 120.0,
            ..Default::default()
        };
        assert!(strip_response_db(&p, 30.0) < -18.0);
        assert!(strip_response_db(&p, 5_000.0).abs() < 0.5);
    }

    #[test]
    fn db_lin_roundtrip() {
        assert!((db_to_lin(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_lin(-6.0206) - 0.5).abs() < 1e-3);
        assert!((db_to_lin(6.0206) - 2.0).abs() < 1e-3);
    }

    #[test]
    fn equal_power_curve() {
        let (o0, i0) = equal_power(0.0);
        assert!((o0 - 1.0).abs() < 1e-6 && i0.abs() < 1e-6);
        let (o1, i1) = equal_power(1.0);
        assert!(o1.abs() < 1e-6 && (i1 - 1.0).abs() < 1e-6);
        let (om, im) = equal_power(0.5);
        assert!((om - FRAC_1_SQRT_2 as f32).abs() < 1e-4);
        assert!((im - FRAC_1_SQRT_2 as f32).abs() < 1e-4);
        // Symmetry and constant power across t.
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let (o, inc) = equal_power(t);
            assert!((o * o + inc * inc - 1.0).abs() < 1e-5);
            let (o2, i2) = equal_power(1.0 - t);
            assert!((o - i2).abs() < 1e-5 && (inc - o2).abs() < 1e-5);
        }
    }

    #[test]
    fn hpf_response() {
        let fc = 200.0;
        let f = Biquad::hpf(fc);
        assert!((db(f.magnitude(fc) as f32) - (-3.0)).abs() < 0.3); // −3 dB at fc
        assert!(f.magnitude(8_000.0) > 0.98); // ~unity in passband
        assert!(db(f.magnitude(fc / 2.0) as f32) < -9.0); // an octave below → strong cut
    }

    #[test]
    fn lpf_response() {
        let fc = 4_000.0;
        let f = Biquad::lpf(fc);
        assert!((db(f.magnitude(fc) as f32) - (-3.0)).abs() < 0.3);
        assert!(f.magnitude(200.0) > 0.98);
        assert!(db(f.magnitude(fc * 2.0) as f32) < -9.0); // an octave above → strong cut
    }

    #[test]
    fn shelf_and_peak_gains() {
        let ls = Biquad::low_shelf(120.0, 6.0);
        assert!((db(ls.magnitude(20.0) as f32) - 6.0).abs() < 0.4); // boost at DC end
        assert!(ls.magnitude(20_000.0) < 1.02); // ~unity far above

        let hs = Biquad::high_shelf(8_000.0, -6.0);
        assert!((db(hs.magnitude(20_000.0) as f32) - (-6.0)).abs() < 0.4);
        assert!(hs.magnitude(50.0) > 0.98);

        let pk = Biquad::peaking(1_000.0, 6.0, 1.0);
        assert!((db(pk.magnitude(1_000.0) as f32) - 6.0).abs() < 0.2); // gain at center
        assert!(pk.magnitude(20.0) > 0.98 && pk.magnitude(18_000.0) > 0.98);
    }

    #[test]
    fn zero_gain_eq_is_identity() {
        for f in [
            Biquad::low_shelf(120.0, 0.0),
            Biquad::high_shelf(8_000.0, 0.0),
            Biquad::peaking(1_000.0, 0.0, 1.0),
        ] {
            for hz in [50.0, 500.0, 5_000.0, 15_000.0] {
                assert!((f.magnitude(hz) - 1.0).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn compressor_below_threshold_is_unity_plus_makeup() {
        let mut c = Compressor::new(-20.0, 4.0, 6.0);
        let mut buf = sine(1_000.0, db_to_lin(-30.0), RATE as usize); // 1 s, −30 dBFS
        c.process(&mut buf);
        let out_db = db(peak(&buf));
        assert!((out_db - (-24.0)).abs() < 0.6); // −30 + 6 dB makeup
        assert!(c.gain_reduction_db() < 0.5); // not compressing
    }

    #[test]
    fn compressor_reduces_loud_signal() {
        let mut c = Compressor::new(-20.0, 4.0, 0.0);
        let mut buf = sine(1_000.0, db_to_lin(-8.0), RATE as usize); // −8 dBFS peak
        c.process(&mut buf);
        // Measure the settled tail.
        let tail = &buf[buf.len() / 2..];
        let out_db = db(peak(tail));
        // 12 dB over threshold, 4:1 → ~9 dB reduction → ~−17 dBFS out.
        assert!((out_db - (-17.0)).abs() < 1.5);
        assert!(c.gain_reduction_db() > 5.0);
    }

    #[test]
    fn strip_flat_is_identity() {
        let mut chain = StripChain::new(&StripParams::default());
        let mut buf = sine(1_000.0, 0.5, BLOCK_FRAMES * 4);
        let orig = buf.clone();
        let gr = chain.process(&mut buf);
        assert_eq!(gr, 0.0); // comp bypassed at ratio 1.0
        for (a, b) in buf.iter().zip(orig.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn strip_eq_boost_changes_output_and_reports_gr() {
        let params = StripParams {
            eq_mid_db: 12.0,
            eq_mid_hz: 1_000.0,
            comp_threshold_db: -30.0,
            comp_ratio: 4.0,
            comp_makeup_db: 0.0,
            ..StripParams::default()
        };
        let mut chain = StripChain::new(&params);
        let mut buf = sine(1_000.0, 0.3, BLOCK_FRAMES * 4);
        let orig = buf.clone();
        let gr = chain.process(&mut buf);
        let changed = buf
            .iter()
            .zip(orig.iter())
            .any(|(a, b)| (a - b).abs() > 1e-3);
        assert!(changed);
        assert!(gr > 0.0); // loud into a low threshold → compressing

        // set_params back to flat resets to identity behaviour.
        chain.set_params(&StripParams::default());
        let mut flat = sine(1_000.0, 0.3, BLOCK_FRAMES);
        let ref_flat = flat.clone();
        chain.process(&mut flat);
        for (a, b) in flat.iter().zip(ref_flat.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn denoiser_smoke() {
        let mut d = Denoiser::new();
        // Pseudo-random noise blocks.
        let mut seed = 0x1234_5678u32;
        for _ in 0..8 {
            let mut buf: Vec<f32> = (0..BLOCK_FRAMES * 2)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
                })
                .collect();
            d.process(&mut buf);
            assert!(buf.iter().all(|s| s.is_finite()));
        }
        d.reset();
    }

    #[test]
    fn ducker_silence_is_unity() {
        let mut duck = Ducker::new();
        let silence = vec![0.0f32; BLOCK_FRAMES * 2];
        let mut gains = Vec::new();
        duck.gains(&silence, &mut gains);
        assert_eq!(gains.len(), BLOCK_FRAMES);
        assert!(gains.iter().all(|&g| (g - 1.0).abs() < 1e-4));
    }

    #[test]
    fn ducker_attacks_and_releases() {
        let mut duck = Ducker::new();
        let mut gains = Vec::new();

        // Loud voice: constant 0.5 amplitude (−6 dBFS) → full ducking target.
        let loud = vec![0.5f32; BLOCK_FRAMES * 2];

        // After ~50 ms (one attack time-constant) the gain has dropped well
        // below unity.
        let frames_50ms = (0.050 * RATE) as usize;
        let mut g_after_attack = 1.0f32;
        let mut done = 0;
        while done < frames_50ms {
            duck.gains(&loud, &mut gains);
            g_after_attack = *gains.last().unwrap();
            done += BLOCK_FRAMES;
        }
        assert!(g_after_attack < 0.6);

        // Hold loud for ~1 s → settle near −12 dB reduction.
        for _ in 0..100 {
            duck.gains(&loud, &mut gains);
        }
        let settled = *gains.last().unwrap();
        assert!((settled - db_to_lin(-12.0)).abs() < 0.02);

        // Release: feed ~500 ms of silence → gain recovers substantially.
        let silence = vec![0.0f32; BLOCK_FRAMES * 2];
        let frames_500ms = (0.500 * RATE) as usize;
        let mut done = 0;
        let mut g_after_release = settled;
        while done < frames_500ms {
            duck.gains(&silence, &mut gains);
            g_after_release = *gains.last().unwrap();
            done += BLOCK_FRAMES;
        }
        assert!(g_after_release > settled + 0.2);
    }

    #[test]
    fn limiter_caps_true_peak() {
        let mut lim = Limiter::new(-1.0);
        let mut buf = sine(1_000.0, db_to_lin(6.0), RATE as usize); // +6 dBFS
        lim.process(&mut buf);
        // Estimate the true peak of the output with the same 4× oversampling.
        let mut tp = 0.0f32;
        let ch: Vec<f32> = buf.chunks_exact(2).map(|f| f[0]).collect();
        for w in ch.windows(2) {
            for k in 0..4 {
                let f = k as f32 / 4.0;
                tp = tp.max((w[0] + (w[1] - w[0]) * f).abs());
            }
        }
        assert!(db(tp) <= -1.0 + 0.1, "true peak {} dB", db(tp));
    }

    /// The case the single-source path puts through it: a decoded MP3 that
    /// peaks at 1.42. Nothing above the ceiling may reach the device, because
    /// what the device does with it is square it off.
    #[test]
    fn limiter_holds_a_decoded_mp3_overshoot_to_the_ceiling() {
        let mut lim = Limiter::new(-1.0);
        let mut buf = sine(1_000.0, 1.4, RATE as usize);
        assert!((peak(&buf) - 1.4).abs() < 1e-3, "test signal is not 1.4");
        lim.process(&mut buf);

        // True peak of the output, measured the way the limiter measures it.
        let ch: Vec<f32> = buf.chunks_exact(2).map(|f| f[0]).collect();
        let mut tp = 0.0f32;
        for w in ch.windows(2) {
            for k in 0..LIMIT_OVERSAMPLE {
                let f = k as f32 / LIMIT_OVERSAMPLE as f32;
                tp = tp.max((w[0] + (w[1] - w[0]) * f).abs());
            }
        }
        assert!(db(tp) <= -1.0 + 0.1, "true peak {:.2} dBTP", db(tp));
        // And the sample peak too, which is what actually hits the rail.
        assert!(
            db(peak(&buf)) <= -1.0 + 0.1,
            "peak {:.2} dB",
            db(peak(&buf))
        );
    }

    /// …and a signal that never asks for gain reduction comes back out
    /// untouched, sample for sample, once the lookahead delay is accounted
    /// for. This is what makes running it unconditionally defensible.
    #[test]
    fn limiter_is_bit_transparent_below_the_ceiling() {
        let mut lim = Limiter::new(-1.0);
        let input = sine(1_000.0, 0.5, RATE as usize);
        assert!((peak(&input) - 0.5).abs() < 1e-3);
        let mut buf = input.clone();
        lim.process(&mut buf);

        let mut worst = 0.0f32;
        for f in LIMIT_LOOKAHEAD..input.len() / 2 {
            for ch in 0..2 {
                let d = (buf[f * 2 + ch] - input[(f - LIMIT_LOOKAHEAD) * 2 + ch]).abs();
                worst = worst.max(d);
            }
        }
        assert!(worst < 1e-4, "quiet signal moved by {worst}");
    }

    #[test]
    fn limiter_passes_quiet_signal() {
        let mut lim = Limiter::new(-1.0);
        let mut buf = sine(1_000.0, db_to_lin(-12.0), RATE as usize);
        let in_peak = peak(&buf);
        lim.process(&mut buf);
        // Skip the lookahead warm-up region when measuring.
        let out_peak = peak(&buf[LIMIT_LOOKAHEAD * 2..]);
        assert!((db(out_peak) - db(in_peak)).abs() < 0.1);
    }

    #[test]
    fn stereo_delay_produces_wet_tail() {
        let mut delay = StereoDelay::new();
        // One block of input, then silence — the wet output should ring on.
        let mut input = vec![0.0f32; BLOCK_FRAMES * 2];
        input[0] = 1.0;
        input[1] = 1.0;
        let mut out = vec![0.0f32; BLOCK_FRAMES * 2];
        delay.process(&input, &mut out);

        // ~320 ms later the first echo appears.
        let mut energy = 0.0f32;
        let silence = vec![0.0f32; BLOCK_FRAMES * 2];
        for _ in 0..80 {
            let mut o = vec![0.0f32; BLOCK_FRAMES * 2];
            delay.process(&silence, &mut o);
            energy += o.iter().map(|s| s.abs()).sum::<f32>();
        }
        assert!(energy > 0.0);
        assert!(out.iter().all(|s| s.is_finite()));
    }
}
