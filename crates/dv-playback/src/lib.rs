//! `dv-playback` — playback controller, clocks, frame delivery, audio output
//! (PLAN.md §3, §4.3, §4.4, §5).
//!
//! The app owns one [`Playback`]: a controller thread that decodes the active
//! source and publishes frames as wgpu textures, plus an audio render thread
//! feeding a lock-free ring consumed by the cpal callback. While audio is
//! audible it is the master clock; otherwise a rate-scaled wall clock drives
//! video. DSP (EQ/comp/etc.) lands here in M5.

pub mod audio;
pub mod controller;
pub mod dsp;
pub mod frame_shader;
pub mod mix;
pub mod ring;

pub use controller::{Playback, Segment, SegmentFx, SegmentSource, SourceSpec, VideoFrameTex};
pub use dv_media::{ColorMatrix, DecodePath};
