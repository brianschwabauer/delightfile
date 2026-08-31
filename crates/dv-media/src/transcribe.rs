//! Local speech-to-text (§18) producing the `transcript.json` cache asset
//! (§8.3), via whisper.cpp through the `whisper-rs` bindings.
//!
//! [`extract_pcm_16k_mono`] decodes a file's audio to the mono 16 kHz f32 PCM
//! whisper.cpp requires (ffmpeg CLI, matching `crate::proxy`'s conventions);
//! [`transcribe`] feeds that to a model and maps whisper's segments to
//! [`dv_core::transcript::Word`]s. Word-level stamps come from asking whisper
//! for one-token segments (`max_len(1)` + `split_on_word(true)`), which is the
//! documented way to get per-word boundaries without DTW.
//!
//! Everything here is synchronous and spawns no threads (crate rule); the job
//! queue that runs it — and the opt-in that gates it, §18 — lives in `dv-app`.
//! whisper.cpp's own decode threads are internal to the C++ library.
//!
//! No model is bundled: the caller passes a `ggml-*.bin` path (the app defaults
//! to `~/.config/delightvideo/models/ggml-base.en.bin`). No cloud, ever (§1).

use std::cell::Cell;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Once;

use dv_core::transcript::{Transcript, Word};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{MediaError, Result};

/// The only sample rate whisper.cpp accepts. Audio is resampled to it on
/// extraction; every timestamp below is derived from whisper's own clock, not
/// this constant.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// whisper's segment timestamps are centiseconds (10 ms units).
const US_PER_CENTISECOND: i64 = 10_000;

/// whisper.cpp's mel front-end needs a non-trivial window; shorter clips are
/// zero-padded to this many samples rather than rejected.
const MIN_SAMPLES: usize = WHISPER_SAMPLE_RATE as usize;

static LOG_HOOKS: Once = Once::new();

/// Decode `path`'s first audio stream to mono f32 PCM at [`WHISPER_SAMPLE_RATE`].
///
/// Shells out to the `ffmpeg` CLI (§2) and reads raw `f32le` off its stdout, so
/// resampling and downmixing are ffmpeg's problem and no intermediate file is
/// written. The whole stream is buffered: whisper.cpp wants one contiguous
/// slice anyway.
///
/// Errors with [`MediaError::NoAudio`] when the file has no audio stream (or
/// decodes to nothing), and [`MediaError::Cli`] when ffmpeg is missing or fails
/// for any other reason (its stderr tail is carried in the message).
pub fn extract_pcm_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .arg("-nostdin")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(path)
        // Audio only, first audio stream, mono 16 kHz raw floats on stdout.
        .arg("-vn")
        .arg("-sn")
        .arg("-dn")
        .arg("-map")
        .arg("0:a:0")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg(WHISPER_SAMPLE_RATE.to_string())
        .arg("-f")
        .arg("f32le")
        .arg("pipe:1")
        .stdin(Stdio::null())
        // `output()` drains stdout and stderr concurrently, so neither pipe can
        // fill and deadlock the other however long the source is.
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MediaError::Cli(
                    "`ffmpeg` binary not found on PATH — delightvideo requires ffmpeg ≥ 6.0 on \
                     PATH (see README)"
                        .to_string(),
                )
            } else {
                MediaError::Io(e)
            }
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // "Stream map '0:a:0' matches no streams." is ffmpeg's way of saying
        // the file has no audio — a distinct condition callers skip, not fail on.
        if stderr.contains("matches no streams") {
            return Err(MediaError::NoAudio(path.display().to_string()));
        }
        return Err(MediaError::Cli(format!(
            "audio extract failed ({}): {}",
            out.status,
            stderr_tail(&stderr)
        )));
    }

    // A successful run with no bytes means a stream that decoded to nothing —
    // indistinguishable from "no audio" for our purposes.
    if out.stdout.len() < 4 {
        return Err(MediaError::NoAudio(path.display().to_string()));
    }

    Ok(out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Transcribe `path` with the whisper model at `model_path` (§18).
///
/// `language` forces a language (`Some("en")`) or auto-detects (`None`).
/// `progress` is called with 0..=1 as decoding advances, on this thread — it
/// must not re-enter this module.
///
/// Segments that are pure markers (`[BLANK_AUDIO]`, `(music)`) or whitespace
/// are dropped, so a silent file yields an empty word list rather than noise.
///
/// Errors with [`MediaError::NoAudio`] (no audio stream) or
/// [`MediaError::Transcribe`] (model load or decode failure).
pub fn transcribe(
    path: &Path,
    model_path: &Path,
    language: Option<&str>,
    progress: &(dyn Fn(f32) + Send + Sync),
) -> Result<Transcript> {
    // whisper.cpp/GGML log a banner and per-run stats to stderr; route them to
    // the `log` crate (via whisper-rs's `log_backend`) so the app owns them.
    LOG_HOOKS.call_once(whisper_rs::install_logging_hooks);

    let mut pcm = extract_pcm_16k_mono(path)?;
    if pcm.len() < MIN_SAMPLES {
        pcm.resize(MIN_SAMPLES, 0.0);
    }
    progress(0.0);

    let mut ctx_params = WhisperContextParameters::default();
    // CPU only (§2): no GPU backend is compiled in, and asking for one anyway
    // would only produce a warning at load.
    ctx_params.use_gpu(false);
    let ctx = WhisperContext::new_with_params(model_path, ctx_params)
        .map_err(|e| MediaError::Transcribe(format!("load {}: {e}", model_path.display())))?;
    let mut state = ctx
        .create_state()
        .map_err(|e| MediaError::Transcribe(format!("whisper state: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    // `None` leaves whisper's language pointer null, which is its auto-detect.
    params.set_language(language);
    // One token per segment, split at word boundaries → segments are words.
    params.set_token_timestamps(true);
    params.set_max_len(1);
    params.set_split_on_word(true);
    // Nothing may print: this runs under a TUI/GUI (§3).
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_progress_callback_safe(|percent: i32| {
        report_progress((percent as f32 / 100.0).clamp(0.0, 1.0));
    });

    // The sink is live only for the `full` call below; the guard clears it even
    // if `full` unwinds.
    let _guard = ProgressSink::install(progress);
    state
        .full(params, &pcm)
        .map_err(|e| MediaError::Transcribe(format!("whisper decode: {e}")))?;
    drop(_guard);
    progress(1.0);

    let mut words = Vec::new();
    for seg in state.as_iter() {
        let Ok(raw) = seg.to_str_lossy() else {
            continue;
        };
        let text = raw.trim();
        if !is_speech(text) {
            continue;
        }
        words.push(Word {
            start_us: seg.start_timestamp() * US_PER_CENTISECOND,
            end_us: seg.end_timestamp() * US_PER_CENTISECOND,
            text: text.to_string(),
            prob: mean_token_prob(&seg),
        });
    }

    let language = match language {
        Some(l) if !l.eq_ignore_ascii_case("auto") => l.to_string(),
        _ => whisper_rs::get_lang_str(state.full_lang_id_from_state())
            .unwrap_or("auto")
            .to_string(),
    };
    let model = model_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| model_path.display().to_string());

    Ok(Transcript {
        language,
        model,
        words,
    })
}

/// Mean probability of a segment's tokens, the confidence the UI dims words by.
/// A segment with no tokens is trusted (1.0) rather than dropped.
fn mean_token_prob(seg: &whisper_rs::WhisperSegment<'_>) -> f32 {
    let n = seg.n_tokens();
    if n <= 0 {
        return 1.0;
    }
    let sum: f32 = (0..n)
        .filter_map(|i| seg.get_token(i))
        .map(|t| t.token_probability())
        .sum();
    (sum / n as f32).clamp(0.0, 1.0)
}

/// Is `text` (already trimmed) a real word rather than one of whisper's
/// non-speech markers? Markers are always fully bracketed or parenthesized
/// (`[BLANK_AUDIO]`, `(music)`, `♪...♪` reduces to no alphanumerics), and a
/// segment with no alphanumeric content carries nothing to show or match.
fn is_speech(text: &str) -> bool {
    if text.starts_with('[') && text.ends_with(']') {
        return false;
    }
    if text.starts_with('(') && text.ends_with(')') {
        return false;
    }
    text.chars().any(char::is_alphanumeric)
}

/// Last few lines of ffmpeg's stderr, for an error message (matching
/// `crate::proxy`'s failure reporting).
fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(4);
    lines[start..].join("; ")
}

thread_local! {
    /// The in-flight [`transcribe`] call's progress sink, as a raw pointer.
    ///
    /// whisper-rs's `set_progress_callback_safe` demands a `'static` closure,
    /// but [`transcribe`] takes its sink by reference, so the reference cannot
    /// be captured directly. whisper.cpp invokes the progress callback on the
    /// thread that called `full`, so a thread-local pointer is exactly scoped:
    /// [`ProgressSink`] sets it before `full` and clears it on drop (including
    /// on unwind), and it is only ever read from inside that window.
    static PROGRESS_SINK: Cell<Option<*const (dyn Fn(f32) + Send + Sync)>> =
        const { Cell::new(None) };
}

/// RAII installer for [`PROGRESS_SINK`], borrowing the sink for its lifetime.
struct ProgressSink<'a> {
    _sink: &'a (dyn Fn(f32) + Send + Sync),
}

impl<'a> ProgressSink<'a> {
    #[allow(unsafe_code)]
    fn install(sink: &'a (dyn Fn(f32) + Send + Sync)) -> ProgressSink<'a> {
        let ptr: *const (dyn Fn(f32) + Send + Sync + 'a) = sink;
        // SAFETY: the transmute only erases `'a` to `'static` so the pointer can
        // live in a thread-local; nothing dereferences it after `Drop` clears
        // the cell, which happens before `'a` can end (the guard borrows the
        // sink for `'a`, and the cell is only read via `report_progress`).
        let erased: *const (dyn Fn(f32) + Send + Sync + 'static) =
            unsafe { std::mem::transmute(ptr) };
        PROGRESS_SINK.with(|c| c.set(Some(erased)));
        ProgressSink { _sink: sink }
    }
}

impl Drop for ProgressSink<'_> {
    fn drop(&mut self) {
        PROGRESS_SINK.with(|c| c.set(None));
    }
}

/// Forward `fraction` to the installed sink, if any.
#[allow(unsafe_code)]
fn report_progress(fraction: f32) {
    PROGRESS_SINK.with(|c| {
        if let Some(ptr) = c.get() {
            // SAFETY: the pointer is only ever non-null while the `ProgressSink`
            // that installed it is alive on this thread, and that guard holds a
            // borrow of the referent for at least that long. The sink is `Sync`,
            // and this is the same thread that installed it.
            let sink: &(dyn Fn(f32) + Send + Sync) = unsafe { &*ptr };
            sink(fraction);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_and_blanks_are_not_speech() {
        assert!(!is_speech("[BLANK_AUDIO]"));
        assert!(!is_speech("[ Music ]"));
        assert!(!is_speech("(coughing)"));
        assert!(!is_speech(""));
        assert!(!is_speech("♪"));
        assert!(!is_speech("--"));
        assert!(is_speech("Hello,"));
        assert!(is_speech("don't"));
        assert!(is_speech("1985."));
    }

    #[test]
    fn stderr_tail_keeps_last_lines() {
        let s = "a\nb\n\nc\nd\ne\n";
        assert_eq!(stderr_tail(s), "b; c; d; e");
        assert_eq!(stderr_tail(""), "");
    }

    #[test]
    fn missing_file_is_a_cli_error() {
        let err = extract_pcm_16k_mono(Path::new("/nonexistent/nope.mp4"))
            .expect_err("no such file must fail");
        assert!(matches!(err, MediaError::Cli(_)), "got {err:?}");
    }
}
