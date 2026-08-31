//! Streaming decoders for playback (§4.1, §4.3).
//!
//! [`VideoDecoder`] turns any ffmpeg-decodable file into a stream of CPU NV12
//! frames (Y plane + interleaved UV half-res plane — the exact two-texture
//! layout the preview shader consumes, §4.2). [`AudioDecoder`] turns the audio
//! stream into interleaved **stereo f32 48 kHz** chunks (§5's internal format).
//!
//! Both are pull-based and synchronous; the playback controller (dv-playback)
//! owns the threads. Seeking is backward-keyframe via `avformat_seek_file`
//! (AV_TIME_BASE == microseconds, so timeline µs pass straight through);
//! callers decode forward from the keyframe to the exact target (§4.3).
//!
//! Hardware decode (§4.1): [`HwDevice::probe_all`] lists creatable VAAPI/CUDA
//! devices (table-driven, §19.4). [`VideoDecoder::open_with`] trial-decodes
//! each per file and silently falls back to software — some codec/profile combos won't map
//! (10-bit on old GPUs etc.). Hw frames are transferred back to CPU NV12 via
//! `av_hwframe_transfer_data` (the v1 path; DMA-BUF zero-copy is a §4.1
//! stretch goal).

use std::path::Path;

use ffmpeg::software::scaling::{Context as Scaler, Flags as ScaleFlags};
use ffmpeg::util::frame::audio::Audio as AudioFrame;
use ffmpeg::util::frame::video::Video as Frame;
use ffmpeg::ChannelLayout;
use ffmpeg_next as ffmpeg;

use crate::{ensure_ffmpeg, MediaError, Result, US_PER_SEC};

/// Which decode path produced frames (status bar, §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodePath {
    Vaapi,
    Nvdec,
    Software,
}

impl DecodePath {
    /// Status-bar label (§4.1: `vaapi`, `nvdec`, `sw`).
    pub fn label(self) -> &'static str {
        match self {
            DecodePath::Vaapi => "vaapi",
            DecodePath::Nvdec => "nvdec",
            DecodePath::Software => "sw",
        }
    }
}

/// YUV→RGB matrix the shader must use (§4.1: chosen from stream metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
}

/// One decoded frame as tightly-packed CPU NV12 planes.
#[derive(Clone)]
pub struct Nv12Frame {
    pub pts_us: i64,
    /// Even dimensions (NV12 chroma is 2x2 subsampled).
    pub width: u32,
    pub height: u32,
    /// `width * height` bytes.
    pub y: Vec<u8>,
    /// `(width/2) * (height/2)` interleaved UV pairs = `width * height / 2` bytes.
    pub uv: Vec<u8>,
    pub matrix: ColorMatrix,
    /// True = limited/studio range (16–235), the video default.
    pub limited_range: bool,
}

/// Streaming video decoder: open → (seek →) next_frame in presentation order.
pub struct VideoDecoder {
    ictx: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    stream_index: usize,
    time_base: (i32, i32),
    scaler: Option<(ffmpeg::format::Pixel, u32, u32, Scaler)>,
    matrix: ColorMatrix,
    limited_range: bool,
    frame_duration_us: i64,
    duration_us: i64,
    path: DecodePath,
    /// Reused between calls to avoid per-frame allocations of AVFrames.
    decoded: Frame,
    transfer: Frame,
    converted: Frame,
    sent_eof: bool,
    /// Frame counter fallback for VFR/missing pts (§14 snap happens upstream).
    frame_ord: i64,
    /// True once a decoded frame actually arrived in GPU memory — the proof
    /// the attached hw device is really decoding (see [`Self::open_with`]).
    hw_frames_seen: bool,
}

impl VideoDecoder {
    /// Open with software decoding.
    pub fn open(path: &Path) -> Result<VideoDecoder> {
        Self::open_with(path, &[])
    }

    /// Open, trying each hardware device in order and **verifying** it by
    /// decoding one frame — a device context can be creatable while the
    /// codec/profile still decodes in software (typical on NVIDIA, where
    /// VAAPI may exist but only CUDA yields hw frames). The first device
    /// whose trial frame is genuinely hw-decoded wins; else silent software
    /// fallback, per file (§4.1).
    pub fn open_with(path: &Path, hw: &[HwDevice]) -> Result<VideoDecoder> {
        ensure_ffmpeg();
        for dev in hw {
            match Self::open_inner(path, Some(dev)) {
                Ok(mut d) => match d.next_frame() {
                    Ok(Some(_)) if d.hw_frames_seen => {
                        // A live stream cannot rewind past the trial frame —
                        // and does not need to: the next frame is the present.
                        if !crate::is_stream_url(path) {
                            d.seek(0)?;
                        }
                        return Ok(d);
                    }
                    Ok(_) => {
                        log::debug!(
                            "{}: {} attached but decoded sw; trying next",
                            path.display(),
                            dev.path().label()
                        );
                    }
                    Err(e) => {
                        log::debug!(
                            "{}: {} trial decode failed: {e}",
                            path.display(),
                            dev.path().label()
                        );
                    }
                },
                Err(e) => {
                    log::debug!(
                        "{}: {} open failed: {e}",
                        path.display(),
                        dev.path().label()
                    );
                }
            }
        }
        Self::open_inner(path, None)
    }

    fn open_inner(path: &Path, hw: Option<&HwDevice>) -> Result<VideoDecoder> {
        let ictx = crate::open_input(path)?;
        let stream = ictx
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| MediaError::NoVideo(path.display().to_string()))?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = (tb.numerator(), tb.denominator());

        let mut ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decode_path = match hw {
            Some(hw) => {
                hw::attach(&mut ctx, hw)?;
                hw.path()
            }
            None => DecodePath::Software,
        };
        let decoder = ctx.decoder().video()?;

        // Frame cadence: prefer the stream's average frame rate; fall back to
        // the decoder's, then 30 fps. Used for stepping and clock math.
        let rate = {
            let r = stream.avg_frame_rate();
            let stream_fps = r.numerator() as f64 / r.denominator().max(1) as f64;
            if stream_fps > 0.0 {
                stream_fps
            } else {
                decoder
                    .frame_rate()
                    .map(|r| r.numerator() as f64 / r.denominator().max(1) as f64)
                    .filter(|f| *f > 0.0)
                    .unwrap_or(30.0)
            }
        };
        let frame_duration_us = (US_PER_SEC as f64 / rate).round() as i64;

        let duration_us = if stream.duration() > 0 {
            ts_to_us(stream.duration(), time_base)
        } else if ictx.duration() > 0 {
            ictx.duration() // already AV_TIME_BASE == µs
        } else {
            0
        };

        // §4.1: BT.601/BT.709 from stream metadata; unspecified guesses by
        // resolution (SD → 601, HD → 709 — the universal convention).
        use ffmpeg::color::Space;
        let matrix = match decoder.color_space() {
            Space::BT709 => ColorMatrix::Bt709,
            Space::BT470BG | Space::SMPTE170M | Space::SMPTE240M => ColorMatrix::Bt601,
            _ => {
                if decoder.height() >= 720 {
                    ColorMatrix::Bt709
                } else {
                    ColorMatrix::Bt601
                }
            }
        };
        let limited_range = decoder.color_range() != ffmpeg::color::Range::JPEG;

        Ok(VideoDecoder {
            ictx,
            decoder,
            stream_index,
            time_base,
            scaler: None,
            matrix,
            limited_range,
            frame_duration_us,
            duration_us,
            path: decode_path,
            decoded: Frame::empty(),
            transfer: Frame::empty(),
            converted: Frame::empty(),
            sent_eof: false,
            frame_ord: 0,
            hw_frames_seen: false,
        })
    }

    /// The active decode path for this file (status bar, §4.1).
    pub fn decode_path(&self) -> DecodePath {
        self.path
    }

    /// Nominal frame duration in µs (from the stream's average rate).
    pub fn frame_duration_us(&self) -> i64 {
        self.frame_duration_us
    }

    /// Stream duration in µs (0 if unknown).
    pub fn duration_us(&self) -> i64 {
        self.duration_us
    }

    /// Seek so that decoding forward reaches `target_us`: backward keyframe
    /// seek + codec flush. The next [`Self::next_frame`] calls return frames
    /// from the preceding keyframe on — the caller drops frames whose pts is
    /// below the target (§4.3).
    pub fn seek(&mut self, target_us: i64) -> Result<()> {
        let target = target_us.max(0);
        // AV_TIME_BASE is microseconds, so µs pass straight through; the
        // `..=target` range = AVSEEK_FLAG_BACKWARD (land on/before target).
        self.ictx.seek(target, ..target.saturating_add(1))?;
        self.decoder.flush();
        self.sent_eof = false;
        self.frame_ord = (target / self.frame_duration_us.max(1)).max(0);
        Ok(())
    }

    /// Decode and return the next frame in presentation order, or `None` at
    /// end of stream.
    pub fn next_frame(&mut self) -> Result<Option<Nv12Frame>> {
        loop {
            if self.try_receive()? {
                return Ok(Some(self.emit()?));
            }
            if self.sent_eof {
                return Ok(None);
            }
            // Feed the next video packet (skipping other streams).
            let mut fed = false;
            // Scope the packet iteration so `self` isn't double-borrowed.
            {
                let mut packets = self.ictx.packets();
                for (s, packet) in packets.by_ref() {
                    if s.index() != self.stream_index {
                        continue;
                    }
                    self.decoder.send_packet(&packet)?;
                    fed = true;
                    break;
                }
            }
            if !fed {
                self.decoder.send_eof()?;
                self.sent_eof = true;
            }
        }
    }

    /// Try to pull one decoded frame into `self.decoded`.
    fn try_receive(&mut self) -> Result<bool> {
        match self.decoder.receive_frame(&mut self.decoded) {
            Ok(()) => Ok(true),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => Ok(false),
            Err(ffmpeg::Error::Eof) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Convert `self.decoded` (possibly a hw frame) to a packed NV12 frame.
    fn emit(&mut self) -> Result<Nv12Frame> {
        let pts_us = self
            .decoded
            .pts()
            .map(|t| ts_to_us(t, self.time_base))
            .unwrap_or(self.frame_ord * self.frame_duration_us);
        self.frame_ord += 1;

        // Hw frames live in GPU memory: transfer to CPU first (§4.1). The
        // transfer target format is chosen by ffmpeg (NV12 for VAAPI/CUDA).
        let src: &Frame = if hw::is_hw_frame(&self.decoded) {
            self.hw_frames_seen = true;
            hw::transfer(&self.decoded, &mut self.transfer)?;
            self.transfer.set_pts(self.decoded.pts());
            &self.transfer
        } else {
            &self.decoded
        };

        let (w, h) = (src.width() & !1, src.height() & !1);
        let src_fmt = src.format();

        let nv12: &Frame = if src_fmt == ffmpeg::format::Pixel::NV12 {
            src
        } else {
            // Rebuild the scaler if the source format/size changed.
            let needs_new = !matches!(&self.scaler, Some((f, sw, sh, _))
                if *f == src_fmt && *sw == src.width() && *sh == src.height());
            if needs_new {
                self.scaler = Some((
                    src_fmt,
                    src.width(),
                    src.height(),
                    Scaler::get(
                        src_fmt,
                        src.width(),
                        src.height(),
                        ffmpeg::format::Pixel::NV12,
                        w,
                        h,
                        ScaleFlags::BILINEAR,
                    )?,
                ));
            }
            let (_, _, _, scaler) = self.scaler.as_mut().expect("scaler just set");
            scaler.run(src, &mut self.converted)?;
            &self.converted
        };

        // Copy planes dropping stride padding: Y then interleaved UV.
        let y_stride = nv12.stride(0);
        let y_data = nv12.data(0);
        let mut y = Vec::with_capacity((w * h) as usize);
        for row in 0..h as usize {
            let start = row * y_stride;
            y.extend_from_slice(&y_data[start..start + w as usize]);
        }
        let uv_h = (h / 2) as usize;
        let uv_row_bytes = w as usize; // (w/2) UV pairs * 2 bytes
        let uv_stride = nv12.stride(1);
        let uv_data = nv12.data(1);
        let mut uv = Vec::with_capacity(uv_row_bytes * uv_h);
        for row in 0..uv_h {
            let start = row * uv_stride;
            uv.extend_from_slice(&uv_data[start..start + uv_row_bytes]);
        }

        Ok(Nv12Frame {
            pts_us,
            width: w,
            height: h,
            y,
            uv,
            matrix: self.matrix,
            limited_range: self.limited_range,
        })
    }
}

/// A run of interleaved **stereo f32 48 kHz** samples (§5's mix format).
pub struct AudioChunk {
    /// Presentation time of the first sample.
    pub start_us: i64,
    /// Interleaved L R L R …; `len() / 2` sample frames.
    pub samples: Vec<f32>,
}

/// Output rate everything is mixed at (§5).
pub const AUDIO_RATE: u32 = 48_000;
/// Output channel count (stereo).
pub const AUDIO_CHANNELS: usize = 2;

/// Streaming audio decoder: any input → stereo f32 48 kHz chunks.
pub struct AudioDecoder {
    ictx: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Audio,
    stream_index: usize,
    time_base: (i32, i32),
    resampler: Option<ffmpeg::software::resampling::Context>,
    out: AudioFrame,
    decoded: AudioFrame,
    sent_eof: bool,
    flushed: bool,
    /// Output position is anchored **once** per open/seek to the first decoded
    /// frame's pts, then advances by emitted sample counts: the resampled
    /// stream is contiguous, and the swr FIFO makes per-frame input pts lag
    /// the output — re-anchoring every frame would overstate positions.
    anchored: bool,
    /// pts of the next output sample.
    next_us: i64,
    duration_us: i64,
}

impl AudioDecoder {
    pub fn open(path: &Path) -> Result<AudioDecoder> {
        ensure_ffmpeg();
        let ictx = crate::open_input(path)?;
        let stream = ictx
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .ok_or_else(|| MediaError::NoAudio(path.display().to_string()))?;
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = (tb.numerator(), tb.denominator());
        let duration_us = if stream.duration() > 0 {
            ts_to_us(stream.duration(), time_base)
        } else if ictx.duration() > 0 {
            ictx.duration()
        } else {
            0
        };

        let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = ctx.decoder().audio()?;

        Ok(AudioDecoder {
            ictx,
            decoder,
            stream_index,
            time_base,
            resampler: None,
            out: AudioFrame::empty(),
            decoded: AudioFrame::empty(),
            sent_eof: false,
            flushed: false,
            anchored: false,
            next_us: 0,
            duration_us,
        })
    }

    /// Stream duration in µs (0 if unknown).
    pub fn duration_us(&self) -> i64 {
        self.duration_us
    }

    /// Seek near `target_us` (keyframe-backward; audio "keyframes" are dense,
    /// so this lands close). Callers trim leading samples using chunk
    /// `start_us` to hit the exact target.
    pub fn seek(&mut self, target_us: i64) -> Result<()> {
        let target = target_us.max(0);
        self.ictx.seek(target, ..target.saturating_add(1))?;
        self.decoder.flush();
        // Drop the resampler: its internal FIFO holds pre-seek samples.
        self.resampler = None;
        self.sent_eof = false;
        self.flushed = false;
        self.anchored = false;
        self.next_us = target;
        Ok(())
    }

    /// Decode and return the next chunk, or `None` at end of stream.
    pub fn next_chunk(&mut self) -> Result<Option<AudioChunk>> {
        loop {
            if self.try_receive()? {
                if !self.anchored {
                    if let Some(pts) = self.decoded.pts() {
                        self.next_us = ts_to_us(pts, self.time_base);
                    }
                    self.anchored = true;
                }
                let chunk = self.convert()?;
                if chunk.samples.is_empty() {
                    continue; // resampler swallowed everything (startup)
                }
                return Ok(Some(chunk));
            }
            if self.sent_eof {
                // Drain the resampler's FIFO: `run` caps output at the reused
                // frame's capacity, so a tail accumulates — flush repeatedly.
                if !self.flushed {
                    let start_us = self.next_us;
                    let mut samples = Vec::new();
                    if let Some(r) = self.resampler.as_mut() {
                        loop {
                            r.flush(&mut self.out)?;
                            if self.out.samples() == 0 {
                                break;
                            }
                            samples.extend_from_slice(&interleaved(&self.out));
                        }
                    }
                    self.flushed = true;
                    if !samples.is_empty() {
                        self.next_us += samples_to_us(samples.len() / AUDIO_CHANNELS);
                        return Ok(Some(AudioChunk { start_us, samples }));
                    }
                }
                return Ok(None);
            }
            let mut fed = false;
            {
                let mut packets = self.ictx.packets();
                for (s, packet) in packets.by_ref() {
                    if s.index() != self.stream_index {
                        continue;
                    }
                    self.decoder.send_packet(&packet)?;
                    fed = true;
                    break;
                }
            }
            if !fed {
                self.decoder.send_eof()?;
                self.sent_eof = true;
            }
        }
    }

    fn try_receive(&mut self) -> Result<bool> {
        match self.decoder.receive_frame(&mut self.decoded) {
            Ok(()) => Ok(true),
            Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => Ok(false),
            Err(ffmpeg::Error::Eof) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Resample `self.decoded` to stereo f32 48 kHz (lazy-built resampler —
    /// same PCM/WAV layout quirks as waveform.rs).
    fn convert(&mut self) -> Result<AudioChunk> {
        self.decoded
            .set_channel_layout(ChannelLayout::default(self.decoded.channels() as i32));
        if self.resampler.is_none() {
            self.resampler = Some(self.decoded.resampler(
                ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                ChannelLayout::STEREO,
                AUDIO_RATE,
            )?);
        }
        let r = self.resampler.as_mut().expect("resampler set");
        r.run(&self.decoded, &mut self.out)?;
        let start_us = self.next_us;
        let samples = interleaved(&self.out);
        self.next_us += samples_to_us(samples.len() / AUDIO_CHANNELS);
        Ok(AudioChunk { start_us, samples })
    }
}

/// Packed stereo frame → interleaved samples. Reads the raw plane bytes:
/// `plane::<f32>()` sizes by sample count only, undercounting packed stereo.
fn interleaved(frame: &AudioFrame) -> Vec<f32> {
    let n = frame.samples() * AUDIO_CHANNELS;
    if n == 0 {
        return Vec::new();
    }
    frame.data(0)[..n * std::mem::size_of::<f32>()]
        .chunks_exact(4)
        .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn samples_to_us(sample_frames: usize) -> i64 {
    (sample_frames as i64 * US_PER_SEC) / AUDIO_RATE as i64
}

/// Stream timestamp → microseconds.
fn ts_to_us(ts: i64, (num, den): (i32, i32)) -> i64 {
    if den == 0 {
        return 0;
    }
    // i128 to avoid overflow on large timestamps × time_base numerators.
    ((ts as i128 * num as i128 * US_PER_SEC as i128) / den as i128) as i64
}

pub use hw::HwDevice;

/// Hardware decode device contexts (§4.1). This is the one corner of the
/// crate that needs raw FFI: `ffmpeg-next` 8 exposes hwaccel types but not
/// the device-context plumbing, so we go through `ffmpeg::ffi` directly.
#[allow(unsafe_code)]
mod hw {
    use super::{DecodePath, Result};
    use ffmpeg::ffi;
    use ffmpeg_next as ffmpeg;

    /// The §4.1 probe table, in preference order (§19.4: ports add rows).
    const PROBE_ORDER: &[(ffi::AVHWDeviceType, DecodePath)] = &[
        (
            ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            DecodePath::Vaapi,
        ),
        (
            ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            DecodePath::Nvdec,
        ),
    ];

    /// A process-wide hardware decode device (an `AVBufferRef` to an
    /// `AVHWDeviceContext`). Cloned into each decoder via refcount bump.
    pub struct HwDevice {
        ctx: *mut ffi::AVBufferRef,
        path: DecodePath,
    }

    // SAFETY: AVBufferRef refcounting is thread-safe; the device context is
    // only handed to ffmpeg, never dereferenced by us.
    unsafe impl Send for HwDevice {}
    unsafe impl Sync for HwDevice {}

    impl HwDevice {
        /// Probe every creatable hw decode device, in preference order
        /// (§4.1: VAAPI, then CUDA). Device creation alone doesn't prove
        /// decoding works — [`super::VideoDecoder::open_with`] trial-decodes
        /// per file. Empty = software only.
        pub fn probe_all() -> Vec<HwDevice> {
            let mut found = Vec::new();
            crate::ensure_ffmpeg();
            // §19.4: the VAAPI render node is a Linux-ism, cfg-gated.
            #[cfg(target_os = "linux")]
            let vaapi_ok = std::path::Path::new("/dev/dri/renderD128").exists();
            #[cfg(not(target_os = "linux"))]
            let vaapi_ok = false;

            for &(kind, path) in PROBE_ORDER {
                if path == DecodePath::Vaapi && !vaapi_ok {
                    continue;
                }
                let mut ctx: *mut ffi::AVBufferRef = std::ptr::null_mut();
                // SAFETY: standard hwdevice creation; NULL device string lets
                // ffmpeg pick the default node.
                let err = unsafe {
                    ffi::av_hwdevice_ctx_create(
                        &mut ctx,
                        kind,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        0,
                    )
                };
                if err >= 0 && !ctx.is_null() {
                    log::info!("hw decode device available: {}", path.label());
                    found.push(HwDevice { ctx, path });
                }
            }
            found
        }

        pub fn path(&self) -> DecodePath {
            self.path
        }
    }

    impl Drop for HwDevice {
        fn drop(&mut self) {
            // SAFETY: we own one reference; unref may free the context.
            unsafe { ffi::av_buffer_unref(&mut self.ctx) };
        }
    }

    /// Attach the hw device to a codec context before opening the decoder.
    /// ffmpeg's default `get_format` accepts the hw pixel format whenever
    /// `hw_device_ctx` is set and the codec supports the device type.
    pub fn attach(ctx: &mut ffmpeg::codec::context::Context, hw: &HwDevice) -> Result<()> {
        // SAFETY: bumping the refcount and storing it on the codec context is
        // the documented ownership pattern; ffmpeg unrefs it on codec close.
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).hw_device_ctx = ffi::av_buffer_ref(hw.ctx);
            if (*raw).hw_device_ctx.is_null() {
                return Err(ffmpeg::Error::Unknown.into());
            }
        }
        Ok(())
    }

    /// Is this decoded frame in GPU memory (hw pixel format)?
    pub fn is_hw_frame(frame: &ffmpeg::util::frame::video::Video) -> bool {
        // SAFETY: reading a plain field of a valid frame.
        unsafe { !(*frame.as_ptr()).hw_frames_ctx.is_null() }
    }

    /// `av_hwframe_transfer_data`: copy a GPU frame to CPU memory. ffmpeg
    /// picks the CPU format (NV12 for VAAPI/NVDEC 8-bit).
    pub fn transfer(
        src: &ffmpeg::util::frame::video::Video,
        dst: &mut ffmpeg::util::frame::video::Video,
    ) -> Result<()> {
        // SAFETY: dst is a valid (possibly empty) frame; transfer_data
        // allocates its buffers. Unref first so repeated transfers don't leak.
        unsafe {
            ffi::av_frame_unref(dst.as_mut_ptr());
            let err = ffi::av_hwframe_transfer_data(dst.as_mut_ptr(), src.as_ptr(), 0);
            if err < 0 {
                return Err(ffmpeg::Error::from(err).into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_to_us_converts() {
        assert_eq!(ts_to_us(30, (1, 30)), US_PER_SEC);
        assert_eq!(ts_to_us(90_000, (1, 90_000)), US_PER_SEC);
        assert_eq!(ts_to_us(1, (1001, 30_000)), 33_366);
    }

    #[test]
    fn samples_to_us_at_48k() {
        assert_eq!(samples_to_us(48_000), US_PER_SEC);
        assert_eq!(samples_to_us(24_000), US_PER_SEC / 2);
    }
}
