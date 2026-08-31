#![cfg(feature = "transcribe")]
//! Integration tests for speech-to-text (§18).
//!
//! Both tests exercise the real ffmpeg CLI, and the transcription test also
//! needs a whisper model and a speech fixture — neither is checked in (a model
//! is ~150 MB). Like the other dv-media integration tests, a missing
//! prerequisite prints a reason and returns rather than failing, so a bare
//! checkout still runs green:
//!
//! ```text
//! model:   ~/.config/delightvideo/models/ggml-base.en.bin   (the app default)
//! speech:  espeak-ng/espeak on PATH, else build/assets/speech.wav
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use dv_media::transcribe::{extract_pcm_16k_mono, transcribe, WHISPER_SAMPLE_RATE};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The app's default model location (§18).
fn model_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home).join(".config/delightvideo/models/ggml-base.en.bin")
}

/// A wav of speech, plus whether we synthesized it (and therefore know it says
/// [`SPOKEN`]). `None` when neither a TTS nor the fixture is available.
fn speech_wav(dir: &Path) -> Option<(PathBuf, bool)> {
    for tts in ["espeak-ng", "espeak"] {
        let out = dir.join("spoken.wav");
        let ok = Command::new(tts)
            .args(["-w", &out.to_string_lossy(), SPOKEN])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok && out.exists() {
            return Some((out, true));
        }
    }
    // build/assets/ is the repo's fixture dir (build/test-assets.sh).
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/assets/speech.wav");
    fixture.exists().then_some((fixture, false))
}

const SPOKEN: &str = "hello world this is a test";

#[test]
fn extract_resamples_and_downmixes_to_16k_mono() {
    if !ffmpeg_available() {
        eprintln!("SKIP: `ffmpeg` CLI not on PATH");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("tone.wav");

    // 1 s of stereo 48 kHz — both the rate and the channel count differ from
    // what whisper wants, so the extract must resample *and* downmix.
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1:sample_rate=48000",
            "-ac",
            "2",
        ])
        .arg(&wav)
        .status()
        .expect("spawn ffmpeg");
    assert!(status.success(), "fixture generation failed");

    let pcm = extract_pcm_16k_mono(&wav).expect("extract");
    let expected = WHISPER_SAMPLE_RATE as f64;
    let ratio = pcm.len() as f64 / expected;
    assert!(
        (0.95..=1.05).contains(&ratio),
        "expected ~{expected} mono samples, got {} (ratio {ratio:.3})",
        pcm.len()
    );
    // Tone, not silence — proves we got audio rather than a buffer of zeros.
    // (lavfi's `sine` peaks well below full scale, so the bar is deliberately
    // low; it only has to clear the noise floor.)
    let peak = pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05, "extracted PCM is silent (peak {peak})");
}

#[test]
fn extract_rejects_a_file_with_no_audio() {
    if !ffmpeg_available() {
        eprintln!("SKIP: `ffmpeg` CLI not on PATH");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let mp4 = dir.path().join("silent.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=10:duration=1",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&mp4)
        .status()
        .expect("spawn ffmpeg");
    assert!(status.success(), "fixture generation failed");

    let err = extract_pcm_16k_mono(&mp4).expect_err("video-only file must not yield PCM");
    assert!(
        matches!(err, dv_media::MediaError::NoAudio(_)),
        "expected NoAudio, got {err:?}"
    );
}

#[test]
fn transcribes_speech_to_monotonic_words() {
    if !ffmpeg_available() {
        eprintln!("SKIP: `ffmpeg` CLI not on PATH");
        return;
    }
    let model = model_path();
    if !model.exists() {
        eprintln!("SKIP: no model at {} (§18)", model.display());
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let Some((wav, synthesized)) = speech_wav(dir.path()) else {
        eprintln!("SKIP: no espeak on PATH and no build/assets/speech.wav fixture");
        return;
    };

    // Re-encode to 48 kHz stereo so the run goes through the resample path the
    // real (camera) sources take.
    let src = dir.path().join("src.wav");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&wav)
        .args(["-ar", "48000", "-ac", "2"])
        .arg(&src)
        .status()
        .expect("spawn ffmpeg");
    assert!(status.success(), "fixture re-encode failed");

    let ticks = AtomicUsize::new(0);
    let t = transcribe(&src, &model, Some("en"), &|p: f32| {
        assert!((0.0..=1.0).contains(&p), "progress out of range: {p}");
        ticks.fetch_add(1, Ordering::Relaxed);
    })
    .expect("transcribe");

    let joined = t
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("transcript ({} words): {joined}", t.words.len());

    assert_eq!(t.language, "en");
    assert_eq!(t.model, "ggml-base.en.bin");
    assert!(
        ticks.load(Ordering::Relaxed) >= 2,
        "progress never reported"
    );
    assert!(!t.words.is_empty(), "no words recognized");

    // Timestamps are non-negative and non-decreasing, and no word is a marker.
    let mut prev = 0i64;
    for w in &t.words {
        assert!(w.start_us >= 0 && w.end_us >= w.start_us, "bad span: {w:?}");
        assert!(w.start_us >= prev, "non-monotonic at {w:?}");
        assert!((0.0..=1.0).contains(&w.prob), "bad prob: {w:?}");
        assert!(!w.text.starts_with('['), "marker leaked through: {w:?}");
        prev = w.start_us;
    }

    // Only a synthesized clip has known content; with the fallback fixture all
    // we can assert is that real words came back, checked above.
    if synthesized {
        let lower = joined.to_lowercase();
        assert!(lower.contains("hello"), "missing 'hello' in {lower:?}");
        assert!(lower.contains("test"), "missing 'test' in {lower:?}");
    }
}
