//! `dv-media` — ffmpeg wrappers for delightvideo (§11).
//!
//! Pure **synchronous** functions: probe, keyframe index, thumbnails, waveform
//! peaks, transcription (§18), plus the content-hash-keyed cache layout (§8.3).
//! Threading and the background job queue live in `dv-app` (§3) — nothing here
//! spawns threads.
//!
//! All `unsafe`/raw ffmpeg usage is confined to this crate behind safe APIs
//! (§11). Every ffmpeg entry point goes through `ffmpeg-next`'s safe wrappers,
//! so the workspace's `unsafe_code = "warn"` lint needs no crate-wide override;
//! the one `unsafe` block in the crate is `transcribe`'s progress-callback
//! bridge, which carries a local `#[allow]` and a safety argument.
//!
//! Transcription is behind the `transcribe` feature: it is the only thing
//! pulling `whisper-rs` (and its whisper.cpp source build) in. Upstream in
//! delightvideo the feature is on by default; in this vendored copy it is off,
//! since the viewer only wants probe/demux/decode. Build with
//! `--features transcribe` to get it back.
//!
//! `ffmpeg_next::init()` is idempotently run once (via [`std::sync::Once`])
//! from every public entry point, so callers never have to remember to.

use std::sync::Once;

use thiserror::Error;

pub mod cache;
pub mod decode;
pub mod keyframe;
pub mod probe;
pub mod proxy;
pub mod silence;
pub mod sync;
pub mod thumbnail;
#[cfg(feature = "transcribe")]
pub mod transcribe;
pub mod waveform;

pub use cache::CacheDir;
pub use decode::{
    AudioChunk, AudioDecoder, ColorMatrix, DecodePath, HwDevice, Nv12Frame, VideoDecoder,
    AUDIO_CHANNELS, AUDIO_RATE,
};
pub use keyframe::{KeyframeEntry, KeyframeIndex};
pub use probe::{display_orientation, probe, Chapter, ProbeInfo};
pub use proxy::{generate_proxy, proxy_needed, ProxyOpts};
pub use sync::{correlate_envelopes, sync_offset, SyncResult};
pub use thumbnail::{generate_thumbnails, ThumbnailOpts};
#[cfg(feature = "transcribe")]
pub use transcribe::{extract_pcm_16k_mono, transcribe};
pub use waveform::{generate_waveform, Waveform, WaveformMeta};

/// Re-export so callers can name ffmpeg types (e.g. in error handling) without
/// depending on `ffmpeg-next` directly and risking a version skew (§2).
pub use ffmpeg_next as ffmpeg;

/// Errors surfaced by `dv-media`. Every fallible entry point returns this.
#[derive(Debug, Error)]
pub enum MediaError {
    /// The file is not something we can use: no decodable video *and* no audio
    /// stream, or an unrecognized container (§9's reject rule).
    #[error("unsupported media: {0}")]
    Unsupported(String),

    /// The operation needs a video stream (thumbnails, keyframe index) but the
    /// file has none.
    #[error("no video stream: {0}")]
    NoVideo(String),

    /// The operation needs an audio stream (waveform) but the file has none.
    #[error("no audio stream: {0}")]
    NoAudio(String),

    /// A cache/binary file (`index.bin`, `waveform.pk`) failed to parse.
    #[error("malformed {kind} file: {detail}")]
    Format { kind: &'static str, detail: String },

    /// The `ffmpeg` CLI (used for asset/proxy encoding — §2) failed to spawn or
    /// exited non-zero. The string carries a human-readable reason, including
    /// the tail of ffmpeg's stderr on a failed run.
    #[error("ffmpeg CLI: {0}")]
    Cli(String),

    /// Speech-to-text (§18) failed: the model file would not load, or
    /// whisper.cpp's decode returned an error.
    #[error("transcription: {0}")]
    Transcribe(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ffmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg::Error),

    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, MediaError>;

/// Is this "path" really a live-stream URL (`rtsp://…`)? The demuxer opens
/// URLs as happily as files, so streams ride the `Path` plumbing end to end —
/// this predicate is how every layer that assumes a *file* (magic sniffing,
/// stat, seeking, the sibling scan) knows to step aside. Scheme-sniffed here,
/// once, rather than threading a `live` flag through every struct between
/// `main` and the decoder.
pub fn is_stream_url(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("rtsp://") || s.starts_with("rtsps://")
}

/// `ffmpeg::format::input`, with the demuxer options a live stream needs.
///
/// * `rtsp_transport=tcp` — UDP on Wi-Fi drops packets and smears the picture;
///   every consumer-camera vendor recommends TCP interleaved.
/// * `fflags=nobuffer` + `flags=low_delay` — the viewer is a monitor, not a
///   player; a second of demuxer buffering is a second of latency.
/// * `stimeout` — socket I/O timeout in µs. Without it a dead camera blocks
///   the calling thread in `packets()` indefinitely; with it a stall surfaces
///   as EOF and the playback layer can reconnect.
/// * `analyzeduration`/`probesize` — how much stream `find_stream_info` is
///   allowed to swallow before the open returns. The defaults sit there for
///   seconds; an h264 camera announces its parameters in the first packets,
///   and this cap is most of the difference between a 5-second and a
///   2-second open.
///
/// Plain files keep the bare open — an options dictionary on a local file is
/// noise the demuxer has to ignore.
pub(crate) fn open_input(
    path: &std::path::Path,
) -> std::result::Result<ffmpeg::format::context::Input, ffmpeg::Error> {
    if is_stream_url(path) {
        let mut opts = ffmpeg::Dictionary::new();
        opts.set("rtsp_transport", "tcp");
        opts.set("fflags", "nobuffer");
        opts.set("flags", "low_delay");
        opts.set("stimeout", "5000000");
        opts.set("analyzeduration", "1000000");
        opts.set("probesize", "1000000");
        ffmpeg::format::input_with_dictionary(&path, opts)
    } else {
        ffmpeg::format::input(&path)
    }
}

static FFMPEG_INIT: Once = Once::new();

/// Initialize ffmpeg exactly once, process-wide (§ crate rules). Safe to call
/// from every entry point; subsequent calls are no-ops. Also quiets libav's
/// stderr logging to errors-only so the app/tests aren't spammed.
pub(crate) fn ensure_ffmpeg() {
    FFMPEG_INIT.call_once(|| {
        // A failure here means the libav* libraries are unusable — the whole
        // media subsystem is dead, so panicking at first touch is correct.
        ffmpeg::init().expect("ffmpeg-next init (libav* unavailable)");
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
    });
}

/// Microseconds per second — times in this crate are microseconds (§8.1).
pub const US_PER_SEC: i64 = 1_000_000;
