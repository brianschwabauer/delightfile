//! The project model (§6, §8.1). Plain Rust structs owned exclusively by the
//! main thread; workers receive immutable `Arc<ProjectSnapshot>` clones (§3).
//! SQLite is a serialization target, not the runtime data structure.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// All times are microseconds (§8.1 `*_us` columns).
pub type TimeUs = i64;

/// One second in `TimeUs`.
pub const US_PER_SEC: TimeUs = 1_000_000;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);
    };
}

id_type!(MediaId);
id_type!(ClipId);
id_type!(StripId);
id_type!(GradeId);
id_type!(GraphicId);
id_type!(MarkerId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Image => "image",
        }
    }

    pub fn parse(s: &str) -> Option<MediaKind> {
        match s {
            "video" => Some(MediaKind::Video),
            "audio" => Some(MediaKind::Audio),
            "image" => Some(MediaKind::Image),
            _ => None,
        }
    }
}

/// An imported media file (§8.1 `media`). `duration_us` is `None` for images
/// (infinite source range, §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub id: MediaId,
    pub path: PathBuf,
    /// blake3 of first+last 1 MB + size (§8.1) — relink + cache key.
    pub hash: String,
    pub kind: MediaKind,
    pub duration_us: Option<TimeUs>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    /// Unix seconds.
    pub added_at: i64,
    /// Runtime-only: file missing on disk (§9). Never persisted or snapshotted.
    #[serde(skip)]
    pub offline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ChannelMode {
    #[default]
    Stereo,
    Left,
    Right,
    Sum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum FitMode {
    #[default]
    Fill,
    Fit,
    Custom,
}

/// A clip: a non-destructive reference into a media file (§6.3), plus all
/// per-clip audio/framing values (§8.1 `clips`). Placement (V1 sequence
/// position vs. free-track start time) lives in the containing collection,
/// not here — V1 position is *derived* from order (§6.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    /// `None` = Gap pseudo-clip (V1 only, §6.3).
    pub media_id: Option<MediaId>,
    pub source_in_us: TimeUs,
    pub source_out_us: TimeUs,
    pub speed: f64,
    pub label: Option<String>,
    /// Index into the 8-color clip palette; `None` = uncolored.
    pub color: Option<u8>,

    // Per-clip mixing — never shared (§5).
    pub gain_db: f64,
    pub pan: f64,
    pub muted: bool,
    pub channel_mode: ChannelMode,
    pub fade_in_us: TimeUs,
    pub fade_out_us: TimeUs,
    /// Audio crossfade at this clip's trailing V1 cut (§5); dies with the cut.
    pub xfade_us: TimeUs,
    pub vfade_in_us: TimeUs,
    pub vfade_out_us: TimeUs,
    /// Shared audio treatment (§5); `None` = flat.
    pub strip_id: Option<StripId>,

    // Framing (§6.4): source rect = frame minus crop insets, slid by nudge.
    pub crop_l: i32,
    pub crop_r: i32,
    pub crop_t: i32,
    pub crop_b: i32,
    pub nudge_x: i32,
    pub nudge_y: i32,
    pub rotate: f64,
    pub fit_mode: FitMode,
    /// Custom fit mode only.
    pub scale: Option<f64>,
    pub tx: Option<f64>,
    pub ty: Option<f64>,
    /// Shared color grade (§15, reserved); `None` = no grade.
    pub grade_id: Option<GradeId>,
    /// The §17 graphic document this clip renders (V1 title card or a G-track
    /// overlay); `None` = a media clip or a Gap. Never shared: exactly one
    /// clip references a given [`crate::graphic::Graphic`] row.
    #[serde(default)]
    pub graphic_id: Option<GraphicId>,
}

impl Clip {
    /// A fresh clip over `media` with every §8.1 default.
    pub fn new(
        id: ClipId,
        media_id: Option<MediaId>,
        source_in_us: TimeUs,
        source_out_us: TimeUs,
    ) -> Clip {
        Clip {
            id,
            media_id,
            source_in_us,
            source_out_us,
            speed: 1.0,
            label: None,
            color: None,
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            channel_mode: ChannelMode::default(),
            fade_in_us: 0,
            fade_out_us: 0,
            xfade_us: 0,
            vfade_in_us: 0,
            vfade_out_us: 0,
            strip_id: None,
            crop_l: 0,
            crop_r: 0,
            crop_t: 0,
            crop_b: 0,
            nudge_x: 0,
            nudge_y: 0,
            rotate: 0.0,
            fit_mode: FitMode::default(),
            scale: None,
            tx: None,
            ty: None,
            grade_id: None,
            graphic_id: None,
        }
    }

    /// Timeline duration: source range scaled by speed.
    pub fn duration_us(&self) -> TimeUs {
        let src = (self.source_out_us - self.source_in_us).max(0);
        ((src as f64) / self.speed).round() as TimeUs
    }

    /// A Gap pseudo-clip (V1 only, §6.3): no media *and* no graphic. A graphic
    /// clip has no media either, but it renders — it is never a gap.
    pub fn is_gap(&self) -> bool {
        self.media_id.is_none() && self.graphic_id.is_none()
    }

    /// A §17 graphic clip (title card in V1, overlay on G).
    pub fn is_graphic(&self) -> bool {
        self.graphic_id.is_some()
    }
}

/// A clip on a free-positioned track (V2/A1/A2, §6.3): absolute start plus
/// optional anchoring to a V1 clip (`anchor_clip_id + offset`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreeClip {
    pub clip: Clip,
    pub timeline_start_us: TimeUs,
    /// `Some((v1_clip, offset))` = travels with that a-roll clip on ripple;
    /// `None` = anchored to the timeline (the per-clip opt-out, §6.3).
    pub anchor: Option<(ClipId, TimeUs)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    V1,
    V2,
    /// Graphics track (§17.2): composites over everything below it.
    G,
    A1,
    A2,
}

impl TrackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackKind::V1 => "v1",
            TrackKind::V2 => "v2",
            TrackKind::G => "g",
            TrackKind::A1 => "a1",
            TrackKind::A2 => "a2",
        }
    }

    pub fn parse(s: &str) -> Option<TrackKind> {
        match s {
            "v1" => Some(TrackKind::V1),
            "v2" => Some(TrackKind::V2),
            "g" => Some(TrackKind::G),
            "a1" => Some(TrackKind::A1),
            "a2" => Some(TrackKind::A2),
            _ => None,
        }
    }
}

/// Per-track settings (§8.1 `tracks`). The track *layout* is fixed (§6.1);
/// only these values vary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackSettings {
    pub gain_db: f64,
    pub pan: f64,
    pub muted: bool,
    pub solo: bool,
    /// Music tracks: duck under a-roll speech (§5).
    pub duck: bool,
    /// Send level to the global delay bus (§5).
    pub send: f64,
}

impl Default for TrackSettings {
    fn default() -> Self {
        TrackSettings {
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            solo: false,
            duck: false,
            send: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tracks {
    pub v1: TrackSettings,
    pub v2: TrackSettings,
    /// Graphics track (§17.2). Uniform with the others so the track code has
    /// no special case; graphics carry no audio, so the mixing fields here are
    /// simply never read.
    #[serde(default)]
    pub g: TrackSettings,
    pub a1: TrackSettings,
    pub a2: TrackSettings,
}

/// The fixed-layout timeline (§6.1, §6.3): V1 is a gapless ordered sequence
/// (position derived from cumulative durations); V2/A1/A2 are free-positioned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub v1: Vec<Clip>,
    pub v2: Vec<FreeClip>,
    /// Graphics (§17.2): mechanically an anchored free track exactly like V2 —
    /// "composites over" instead of "replaces" is a renderer concern, not an
    /// editing one.
    #[serde(default)]
    pub g: Vec<FreeClip>,
    pub a1: Vec<FreeClip>,
    pub a2: Vec<FreeClip>,
}

impl Timeline {
    /// Total V1 duration = timeline duration (§6.3: V1 is the sequence).
    pub fn duration_us(&self) -> TimeUs {
        self.v1.iter().map(Clip::duration_us).sum()
    }

    /// Timeline start of V1 clip at sequence index `i` (cumulative durations).
    pub fn v1_start_us(&self, i: usize) -> TimeUs {
        self.v1.iter().take(i).map(Clip::duration_us).sum()
    }

    /// Sequence index of the V1 clip covering time `t` (clamped to the last).
    pub fn v1_index_at(&self, t: TimeUs) -> Option<usize> {
        if self.v1.is_empty() {
            return None;
        }
        let mut acc = 0;
        for (i, c) in self.v1.iter().enumerate() {
            acc += c.duration_us();
            if t < acc {
                return Some(i);
            }
        }
        Some(self.v1.len() - 1)
    }

    /// Sequence index of the V1 clip with id `id`.
    pub fn v1_index_of(&self, id: ClipId) -> Option<usize> {
        self.v1.iter().position(|c| c.id == id)
    }

    /// The free-positioned track for `kind` (`None` for V1, §6.3).
    pub fn free_track(&self, kind: TrackKind) -> Option<&Vec<FreeClip>> {
        match kind {
            TrackKind::V1 => None,
            TrackKind::V2 => Some(&self.v2),
            TrackKind::G => Some(&self.g),
            TrackKind::A1 => Some(&self.a1),
            TrackKind::A2 => Some(&self.a2),
        }
    }

    pub fn free_track_mut(&mut self, kind: TrackKind) -> Option<&mut Vec<FreeClip>> {
        match kind {
            TrackKind::V1 => None,
            TrackKind::V2 => Some(&mut self.v2),
            TrackKind::G => Some(&mut self.g),
            TrackKind::A1 => Some(&mut self.a1),
            TrackKind::A2 => Some(&mut self.a2),
        }
    }

    /// Locate a clip anywhere on the timeline.
    pub fn find_clip(&self, id: ClipId) -> Option<(TrackKind, usize)> {
        if let Some(i) = self.v1_index_of(id) {
            return Some((TrackKind::V1, i));
        }
        for kind in [TrackKind::V2, TrackKind::G, TrackKind::A1, TrackKind::A2] {
            if let Some(i) = self
                .free_track(kind)
                .expect("free kinds only")
                .iter()
                .position(|f| f.clip.id == id)
            {
                return Some((kind, i));
            }
        }
        None
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        match self.find_clip(id)? {
            (TrackKind::V1, i) => Some(&self.v1[i]),
            (kind, i) => Some(&self.free_track(kind).expect("free kinds only")[i].clip),
        }
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        match self.find_clip(id)? {
            (TrackKind::V1, i) => Some(&mut self.v1[i]),
            (kind, i) => Some(&mut self.free_track_mut(kind).expect("free kinds only")[i].clip),
        }
    }

    /// Resolved timeline start of a free clip: anchored clips derive their
    /// position from their V1 anchor (§6.3), timeline-anchored use the stored
    /// absolute start.
    pub fn free_start_us(&self, fc: &FreeClip) -> TimeUs {
        match fc.anchor {
            Some((anchor_id, offset)) => self
                .v1
                .iter()
                .position(|c| c.id == anchor_id)
                .map(|i| self.v1_start_us(i) + offset)
                .unwrap_or(fc.timeline_start_us),
            None => fc.timeline_start_us,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub time_us: TimeUs,
    pub name: String,
    pub color: Option<u8>,
}

/// Shared audio treatment object (§5). Parameters stay opaque JSON at the DB
/// boundary; [`Strip::params`] parses into the typed [`StripParams`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    pub id: StripId,
    pub params_json: String,
}

impl Strip {
    pub fn new(id: StripId, params: &StripParams) -> Strip {
        Strip {
            id,
            params_json: params.to_json(),
        }
    }

    /// Parsed treatment parameters (unknown/missing fields default — old
    /// snapshots and hand-edited JSON both stay loadable).
    pub fn params(&self) -> StripParams {
        StripParams::from_json(&self.params_json)
    }

    pub fn set_params(&mut self, params: &StripParams) {
        self.params_json = params.to_json();
    }
}

/// The shareable audio *treatment* half of the channel strip (§5): denoise,
/// HPF, EQ, LPF, compressor, delay-bus send. Per-clip *mixing* (gain, pan,
/// fades, channel mode, mute) lives on [`Clip`] and is never shared.
///
/// Off states keep their last swept value (`hpf_on: false, hpf_hz: 90.0`) so
/// toggling an effect back on restores where the user left it. `comp_ratio`
/// 1.0 = compressor off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StripParams {
    pub denoise: bool,
    /// 12 dB/oct high-pass, sweepable 20–300 Hz (§5).
    pub hpf_on: bool,
    pub hpf_hz: f64,
    /// 3-band EQ (§5): low shelf 120 Hz, peaking (freq adjustable 250 Hz–8 kHz),
    /// high shelf 8 kHz; ±15 dB each.
    pub eq_low_db: f64,
    pub eq_mid_db: f64,
    pub eq_mid_hz: f64,
    pub eq_high_db: f64,
    /// 12 dB/oct low-pass, sweepable 4–20 kHz (§5).
    pub lpf_on: bool,
    pub lpf_hz: f64,
    /// Compressor (§5): fixed 5 ms attack / 100 ms release, soft knee.
    pub comp_threshold_db: f64,
    pub comp_ratio: f64,
    pub comp_makeup_db: f64,
    /// Send level to the global delay bus, 0–1 (§5 treatment half).
    pub send: f64,
}

impl Default for StripParams {
    fn default() -> Self {
        StripParams {
            denoise: false,
            hpf_on: false,
            hpf_hz: 90.0,
            eq_low_db: 0.0,
            eq_mid_db: 0.0,
            eq_mid_hz: 1000.0,
            eq_high_db: 0.0,
            lpf_on: false,
            lpf_hz: 12_000.0,
            comp_threshold_db: -18.0,
            comp_ratio: 1.0,
            comp_makeup_db: 0.0,
            send: 0.0,
        }
    }
}

impl StripParams {
    pub fn from_json(s: &str) -> StripParams {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// True when every parameter is at its default — a flat strip renders
    /// identically to no strip at all.
    pub fn is_flat(&self) -> bool {
        *self == StripParams::default()
    }

    /// Built-in presets (§5). `(name, params)`; "Flat" is `default()`.
    pub fn builtin_presets() -> Vec<(&'static str, StripParams)> {
        let flat = StripParams::default();
        vec![
            (
                "Interview",
                StripParams {
                    denoise: true,
                    hpf_on: true,
                    hpf_hz: 90.0,
                    eq_mid_db: 2.0,
                    eq_mid_hz: 3000.0,
                    comp_threshold_db: -24.0,
                    comp_ratio: 4.0,
                    comp_makeup_db: 3.0,
                    ..flat.clone()
                },
            ),
            (
                "Voiceover",
                StripParams {
                    hpf_on: true,
                    hpf_hz: 80.0,
                    comp_threshold_db: -20.0,
                    comp_ratio: 2.5,
                    comp_makeup_db: 2.0,
                    ..flat.clone()
                },
            ),
            ("Music", flat.clone()),
            (
                "Ambient/Foley",
                StripParams {
                    hpf_on: true,
                    hpf_hz: 60.0,
                    comp_threshold_db: -20.0,
                    comp_ratio: 2.0,
                    comp_makeup_db: 1.0,
                    ..flat.clone()
                },
            ),
            ("Flat", flat),
        ]
    }
}

/// Shared color grade object (§15). Parameters stay opaque JSON at the DB
/// boundary; [`Grade::params`] parses into the typed [`GradeParams`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grade {
    pub id: GradeId,
    pub params_json: String,
}

impl Grade {
    pub fn new(id: GradeId, params: &GradeParams) -> Grade {
        Grade {
            id,
            params_json: params.to_json(),
        }
    }

    /// Parsed grade parameters (unknown/missing fields default — old
    /// snapshots and hand-edited JSON both stay loadable).
    pub fn params(&self) -> GradeParams {
        GradeParams::from_json(&self.params_json)
    }

    pub fn set_params(&mut self, params: &GradeParams) {
        self.params_json = params.to_json();
    }
}

/// The §15 color grade parameter set — the seven sliders, nothing more.
/// Applied in linear light in the frame shader (§4.2); identity values render
/// bit-identically to no grade at all.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeParams {
    /// Stops, ±3; 0 = neutral.
    pub exposure: f64,
    /// Gain about mid-gray (0.18 linear), 0.5–1.5; 1 = neutral.
    pub contrast: f64,
    /// White balance temperature, −1 (cool) … +1 (warm); 0 = neutral.
    pub temperature: f64,
    /// White balance tint, −1 (green) … +1 (magenta); 0 = neutral.
    pub tint: f64,
    /// 0–2; 1 = neutral, 0 = grayscale.
    pub saturation: f64,
    /// ±1 saturation weighted toward less-saturated pixels; 0 = neutral.
    pub vibrance: f64,
    /// ±1 recovery/lift of the bright end; 0 = neutral.
    pub highlights: f64,
    /// ±1 recovery/lift of the dark end; 0 = neutral.
    pub shadows: f64,
}

impl Default for GradeParams {
    fn default() -> Self {
        GradeParams {
            exposure: 0.0,
            contrast: 1.0,
            temperature: 0.0,
            tint: 0.0,
            saturation: 1.0,
            vibrance: 0.0,
            highlights: 0.0,
            shadows: 0.0,
        }
    }
}

impl GradeParams {
    pub fn from_json(s: &str) -> GradeParams {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// True when every parameter is neutral — an identity grade renders
    /// identically to no grade at all.
    pub fn is_identity(&self) -> bool {
        *self == GradeParams::default()
    }

    /// The shader uniform packing (§4.2): `grade_a` = exposure, contrast,
    /// saturation, temperature; `grade_b` = tint, highlights, shadows,
    /// vibrance. Must match `FrameUniforms`' field docs in dv-playback.
    pub fn to_uniforms(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.exposure as f32,
                self.contrast as f32,
                self.saturation as f32,
                self.temperature as f32,
            ],
            [
                self.tint as f32,
                self.highlights as f32,
                self.shadows as f32,
                self.vibrance as f32,
            ],
        )
    }

    /// Stable key for caches of graded pixels (thumbnail textures, §12 blast
    /// radius): 0 for identity, else a hash of the exact parameter bits.
    pub fn fingerprint(&self) -> u64 {
        if self.is_identity() {
            return 0;
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for v in [
            self.exposure,
            self.contrast,
            self.temperature,
            self.tint,
            self.saturation,
            self.vibrance,
            self.highlights,
            self.shadows,
        ] {
            v.to_bits().hash(&mut h);
        }
        h.finish().max(1)
    }

    /// CPU mirror of the frame shader's grade math (§4.2/§15) for pixels that
    /// never reach the GPU pipeline — thumbnail JPEGs. Operates in place on
    /// sRGB RGBA8; alpha untouched. Identity is a no-op by construction.
    pub fn apply_srgb8(&self, rgba: &mut [u8]) {
        if self.is_identity() {
            return;
        }
        let (a, b) = self.to_uniforms();
        let (exposure, contrast, saturation, temp) = (a[0], a[1], a[2], a[3]);
        let (tint, highlights, shadows, vibrance) = (b[0], b[1], b[2], b[3]);
        let to_lin = |c: f32| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let to_srgb = |c: f32| {
            if c <= 0.003_130_8 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            }
        };
        let smoothstep = |e0: f32, e1: f32, x: f32| {
            let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        let gain = exposure.exp2();
        let wb = [1.0 + 0.25 * temp, 1.0 - 0.10 * tint, 1.0 - 0.25 * temp];
        for px in rgba.chunks_exact_mut(4) {
            let mut lin = [0f32; 3];
            for i in 0..3 {
                lin[i] = (to_lin(px[i] as f32 / 255.0) * gain * wb[i]).max(0.0);
            }
            let luma = |l: &[f32; 3]| 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2];
            let l0 = luma(&lin);
            let tone = (1.0 + highlights * smoothstep(0.18, 1.0, l0))
                * (1.0 + shadows * (1.0 - smoothstep(0.0, 0.35, l0)));
            for c in &mut lin {
                *c = (((*c * tone) - 0.18) * contrast + 0.18).max(0.0);
            }
            let l = luma(&lin);
            for c in &mut lin {
                *c = l + (*c - l) * saturation;
            }
            let mx = lin[0].max(lin[1]).max(lin[2]);
            let chroma = (mx - lin[0].min(lin[1]).min(lin[2])) / mx.max(1e-4);
            let vib = 1.0 + vibrance * (1.0 - chroma);
            for (i, c) in lin.iter().enumerate() {
                px[i] = (to_srgb((l + (c - l) * vib).clamp(0.0, 1.0)) * 255.0).round() as u8;
            }
        }
    }
}

/// Project format + app-level settings stored in `meta` (§8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub sample_rate: u32,
    /// Unix seconds.
    pub created_at: i64,
    pub modified_at: i64,
    // Master bus (§5, defaults per §8.1).
    pub loudness_target_lufs: f64,
    pub loudness_enabled: bool,
    pub limiter_ceiling_dbtp: f64,
    pub bus_comp_json: Option<String>,
    // Export range marks (§6.4); `None` = full timeline.
    pub range_in_us: Option<TimeUs>,
    pub range_out_us: Option<TimeUs>,
}

impl ProjectMeta {
    pub fn new(name: impl Into<String>, now_unix: i64) -> ProjectMeta {
        ProjectMeta {
            name: name.into(),
            // Defaults until the first imported clip sets the format (§8.1).
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
            sample_rate: 48_000,
            created_at: now_unix,
            modified_at: now_unix,
            loudness_target_lufs: -14.0,
            loudness_enabled: true,
            limiter_ceiling_dbtp: -1.0,
            bus_comp_json: None,
            range_in_us: None,
            range_out_us: None,
        }
    }

    pub fn fps(&self) -> f64 {
        self.fps_num as f64 / self.fps_den.max(1) as f64
    }
}

/// The whole project model. One `id_counter` allocates ids for every entity
/// kind — ids are unique project-wide, which keeps anchor/strip/grade
/// references unambiguous across snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub meta: ProjectMeta,
    pub media: Vec<Media>,
    pub tracks: Tracks,
    pub timeline: Timeline,
    pub markers: Vec<Marker>,
    pub strips: Vec<Strip>,
    pub grades: Vec<Grade>,
    /// §17 graphic documents, one per graphic clip (never shared).
    #[serde(default)]
    pub graphics: Vec<crate::graphic::Graphic>,
    id_counter: i64,
}

impl Project {
    pub fn new(name: impl Into<String>, now_unix: i64) -> Project {
        Project {
            meta: ProjectMeta::new(name, now_unix),
            media: Vec::new(),
            tracks: Tracks::default(),
            timeline: Timeline::default(),
            markers: Vec::new(),
            strips: Vec::new(),
            grades: Vec::new(),
            graphics: Vec::new(),
            id_counter: 0,
        }
    }

    /// Allocate the next project-wide unique id.
    pub fn alloc_id(&mut self) -> i64 {
        self.id_counter += 1;
        self.id_counter
    }

    /// Keep the allocator ahead of every id present (used after loading rows
    /// whose ids came from a file).
    pub fn bump_id_counter(&mut self, seen: i64) {
        self.id_counter = self.id_counter.max(seen);
    }

    /// Current allocator position without allocating (undo snapshots record
    /// it so a revert never rewinds the allocator, §6.5).
    pub fn peek_id_counter(&self) -> i64 {
        self.id_counter
    }

    pub fn media_by_id(&self, id: MediaId) -> Option<&Media> {
        self.media.iter().find(|m| m.id == id)
    }

    pub fn media_by_id_mut(&mut self, id: MediaId) -> Option<&mut Media> {
        self.media.iter_mut().find(|m| m.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(id: i64, dur_s: i64) -> Clip {
        Clip::new(ClipId(id), Some(MediaId(999)), 0, dur_s * US_PER_SEC)
    }

    #[test]
    fn v1_positions_are_cumulative() {
        let tl = Timeline {
            v1: vec![clip(1, 10), clip(2, 5), clip(3, 20)],
            ..Timeline::default()
        };
        assert_eq!(tl.duration_us(), 35 * US_PER_SEC);
        assert_eq!(tl.v1_start_us(0), 0);
        assert_eq!(tl.v1_start_us(2), 15 * US_PER_SEC);
        assert_eq!(tl.v1_index_at(0), Some(0));
        assert_eq!(tl.v1_index_at(12 * US_PER_SEC), Some(1));
        assert_eq!(tl.v1_index_at(999 * US_PER_SEC), Some(2));
    }

    #[test]
    fn speed_scales_duration() {
        let mut c = clip(1, 10);
        c.speed = 2.0;
        assert_eq!(c.duration_us(), 5 * US_PER_SEC);
    }

    #[test]
    fn anchored_free_clip_derives_position() {
        let tl = Timeline {
            v1: vec![clip(1, 10), clip(2, 5)],
            v2: vec![FreeClip {
                clip: clip(3, 3),
                timeline_start_us: 0, // stale on purpose — anchor wins
                anchor: Some((ClipId(2), 2 * US_PER_SEC)),
            }],
            ..Timeline::default()
        };
        let fc = &tl.v2[0];
        assert_eq!(tl.free_start_us(fc), 12 * US_PER_SEC);
    }

    #[test]
    fn grade_params_roundtrip_and_identity() {
        let flat = GradeParams::default();
        assert!(flat.is_identity());
        assert_eq!(GradeParams::from_json(&flat.to_json()), flat);
        // Unknown/missing fields default (old snapshots stay loadable).
        assert_eq!(GradeParams::from_json("{}"), flat);
        assert!(GradeParams::from_json("not json").is_identity());
        let warm = GradeParams {
            exposure: 0.5,
            temperature: 0.3,
            ..flat
        };
        assert!(!warm.is_identity());
        assert_eq!(GradeParams::from_json(&warm.to_json()), warm);
        // Uniform packing matches the documented grade_a/grade_b layout.
        let (a, b) = warm.to_uniforms();
        assert_eq!(a, [0.5, 1.0, 1.0, 0.3]);
        assert_eq!(b, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn grade_cpu_apply_matches_identity_and_direction() {
        // Identity is byte-for-byte a no-op (thumbnails must not shift).
        let mut px = vec![10u8, 128, 240, 255, 0, 55, 200, 7];
        let orig = px.clone();
        GradeParams::default().apply_srgb8(&mut px);
        assert_eq!(px, orig);
        assert_eq!(GradeParams::default().fingerprint(), 0);
        // +1 stop brightens every non-clipped channel; alpha untouched.
        let warm = GradeParams {
            exposure: 1.0,
            ..GradeParams::default()
        };
        warm.apply_srgb8(&mut px);
        assert!(px[0] > orig[0] && px[1] > orig[1]);
        assert_eq!(px[3], 255);
        assert_eq!(px[7], 7);
        assert_ne!(warm.fingerprint(), 0);
        assert_ne!(
            warm.fingerprint(),
            GradeParams {
                exposure: 2.0,
                ..GradeParams::default()
            }
            .fingerprint()
        );
    }

    #[test]
    fn gap_graphic_and_media_clips_are_distinct() {
        let media = clip(1, 5);
        assert!(!media.is_gap() && !media.is_graphic());
        let gap = Clip::new(ClipId(2), None, 0, 5 * US_PER_SEC);
        assert!(gap.is_gap() && !gap.is_graphic());
        let mut graphic = Clip::new(ClipId(3), None, 0, 5 * US_PER_SEC);
        graphic.graphic_id = Some(GraphicId(9));
        assert!(!graphic.is_gap(), "a graphic renders — it is not a gap");
        assert!(graphic.is_graphic());
    }

    #[test]
    fn g_track_is_a_free_track() {
        let mut tl = Timeline {
            v1: vec![clip(1, 10)],
            ..Timeline::default()
        };
        let mut g = Clip::new(ClipId(5), None, 0, 3 * US_PER_SEC);
        g.graphic_id = Some(GraphicId(9));
        tl.g.push(FreeClip {
            clip: g,
            timeline_start_us: 0,
            anchor: Some((ClipId(1), 2 * US_PER_SEC)),
        });
        assert_eq!(tl.find_clip(ClipId(5)), Some((TrackKind::G, 0)));
        assert!(tl.free_track(TrackKind::G).is_some());
        assert!(tl.free_track_mut(TrackKind::G).is_some());
        assert_eq!(tl.free_start_us(&tl.g[0]), 2 * US_PER_SEC);
        assert_eq!(TrackKind::G.as_str(), "g");
        assert_eq!(TrackKind::parse("g"), Some(TrackKind::G));
    }

    /// Old snapshots predate §17: `clips.graphic_id`, `timeline.g`, `tracks.g`
    /// and `project.graphics` must all default rather than fail the decode.
    #[test]
    fn pre_graphics_json_still_deserializes() {
        let mut p = Project::new("old", 0);
        p.timeline.v1.push(clip(1, 10));
        let mut v = serde_json::to_value(&p).expect("to_value");
        let obj = v.as_object_mut().expect("object");
        obj.remove("graphics");
        obj["timeline"]
            .as_object_mut()
            .expect("timeline")
            .remove("g");
        obj["tracks"].as_object_mut().expect("tracks").remove("g");
        obj["timeline"]["v1"][0]
            .as_object_mut()
            .expect("clip")
            .remove("graphic_id");
        let back: Project = serde_json::from_value(v).expect("old model still loads");
        assert!(back.graphics.is_empty());
        assert!(back.timeline.g.is_empty());
        assert_eq!(back.tracks.g, TrackSettings::default());
        assert_eq!(back.timeline.v1[0].graphic_id, None);
    }

    #[test]
    fn id_allocation_is_unique_and_bumpable() {
        let mut p = Project::new("t", 0);
        let a = p.alloc_id();
        let b = p.alloc_id();
        assert_ne!(a, b);
        p.bump_id_counter(100);
        assert_eq!(p.alloc_id(), 101);
    }
}
