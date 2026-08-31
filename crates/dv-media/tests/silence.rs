//! Integration test for silence detection (§1) over an ffmpeg-generated
//! fixture. Like the other dv-media integration tests, this exercises the real
//! decode path and so only runs where the `ffmpeg` CLI is present; when it is
//! absent the test prints a reason and returns rather than failing.

use std::process::Command;

use dv_media::silence::{
    detect_silences, SILENCE_MIN_DUR_US, SILENCE_PAD_US, SILENCE_THRESHOLD_DB,
};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn detects_single_interior_silence() {
    if !ffmpeg_available() {
        eprintln!("SKIP: `ffmpeg` CLI not on PATH — cannot synthesize silence fixture");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let wav = dir.path().join("gap.wav");

    // 3 s: 1 s of a 440 Hz sine at full scale (≈ −3 dBFS RMS), 1 s of true
    // silence (t in [1,2) → 0), then 1 s of tone again. The one interior gap is
    // the only span the detector should find.
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='if(between(t,1,2),0,sin(440*2*PI*t))':s=48000:d=3",
            "-ac",
            "2",
        ])
        .arg(&wav)
        .status()
        .expect("run ffmpeg (fixture)");
    assert!(status.success(), "ffmpeg failed to make the fixture");

    let spans = detect_silences(
        &wav,
        SILENCE_THRESHOLD_DB,
        SILENCE_MIN_DUR_US,
        SILENCE_PAD_US,
    )
    .expect("detect_silences");

    assert_eq!(
        spans.len(),
        1,
        "exactly one interior silence, got {spans:?}"
    );
    let sp = spans[0];

    // Raw gap is [1.0 s, 2.0 s]; padding pulls each edge inward by 100 ms, so
    // the reported span is ≈ [1.1 s, 1.9 s]. Allow ±80 ms of detector slack.
    let tol = 80_000;
    let want_start = 1_000_000 + SILENCE_PAD_US;
    let want_end = 2_000_000 - SILENCE_PAD_US;
    assert!(
        (sp.start_us - want_start).abs() <= tol,
        "start {} should be within ±{}µs of {}",
        sp.start_us,
        tol,
        want_start
    );
    assert!(
        (sp.end_us - want_end).abs() <= tol,
        "end {} should be within ±{}µs of {}",
        sp.end_us,
        tol,
        want_end
    );
    assert!(sp.start_us < sp.end_us, "span must be well-formed: {sp:?}");
}
