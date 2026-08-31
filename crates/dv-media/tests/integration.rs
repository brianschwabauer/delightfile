//! Integration tests over tiny generated fixtures (§11). These exercise the
//! real ffmpeg paths, so they only run where the `ffmpeg` CLI is present (CI
//! gates dv-media on ffmpeg — §11). When it is absent, each test prints a clear
//! message and returns rather than failing.
//!
//! Fixtures are produced by `build/test-assets.sh` (generate-if-missing). The
//! script already emits everything these tests need: `basic.mp4` (video+audio),
//! `longgop.mp4` (g=300), `tone.m4a` (audio-only), `still.png` (image).

use std::path::{Path, PathBuf};
use std::process::Command;

use dv_core::model::MediaKind;
use dv_media::{
    generate_proxy, generate_thumbnails, generate_waveform, probe, sync_offset, KeyframeIndex,
    ProxyOpts, ThumbnailOpts, Waveform,
};

/// Repo root = two levels up from this crate's manifest dir.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure fixtures exist; returns the assets dir, or `None` (with a printed
/// reason) if we must skip — no `ffmpeg` CLI to generate them.
fn fixtures() -> Option<PathBuf> {
    let root = repo_root();
    let assets = root.join("build/assets");
    if !ffmpeg_available() {
        eprintln!("SKIP: `ffmpeg` CLI not on PATH — cannot generate/probe fixtures");
        return None;
    }
    let status = Command::new("bash")
        .arg(root.join("build/test-assets.sh"))
        .status()
        .expect("run build/test-assets.sh");
    assert!(status.success(), "test-assets.sh failed");
    Some(assets)
}

macro_rules! skip_unless_fixtures {
    () => {
        match fixtures() {
            Some(a) => a,
            None => return,
        }
    };
}

#[test]
fn probe_video_fixture_dims_fps_duration() {
    let assets = skip_unless_fixtures!();
    let info = probe(&assets.join("basic.mp4")).expect("probe basic.mp4");
    assert_eq!(info.kind, MediaKind::Video);
    assert_eq!(info.width, Some(640));
    assert_eq!(info.height, Some(360));
    assert_eq!(info.fps_num, Some(30));
    assert_eq!(info.fps_den, Some(1));
    assert!(info.has_audio, "basic.mp4 has an audio track");
    assert_eq!(info.video_codec.as_deref(), Some("h264"));
    // ~3 s, allow slack for container rounding.
    let dur = info.duration_us.expect("duration");
    assert!(
        (2_900_000..=3_200_000).contains(&dur),
        "duration_us was {dur}"
    );
}

#[test]
fn probe_reads_container_chapters() {
    let assets = skip_unless_fixtures!();
    let info = probe(&assets.join("chapters.mkv")).expect("probe chapters.mkv");
    let starts: Vec<i64> = info.chapters.iter().map(|c| c.start_us).collect();
    assert_eq!(starts, vec![0, 2_000_000, 4_000_000]);
    assert_eq!(info.chapters[2].end_us, 6_000_000);
    let titles: Vec<&str> = info.chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, vec!["Intro", "Middle Part", "Fin — ünïcode"]);
    // A file without any is an empty list, not a fabricated single chapter.
    let plain = probe(&assets.join("basic.mp4")).expect("probe basic.mp4");
    assert!(plain.chapters.is_empty());
}

#[test]
fn probe_audio_only_fixture() {
    let assets = skip_unless_fixtures!();
    let info = probe(&assets.join("tone.m4a")).expect("probe tone.m4a");
    assert_eq!(info.kind, MediaKind::Audio);
    assert!(info.has_audio);
    assert!(info.video_codec.is_none());
    assert!(info.audio_codec.is_some());
    assert!(info.width.is_none());
    assert!(info.sample_rate.unwrap_or(0) > 0);
}

#[test]
fn probe_image_fixture() {
    let assets = skip_unless_fixtures!();
    let info = probe(&assets.join("still.png")).expect("probe still.png");
    assert_eq!(info.kind, MediaKind::Image);
    assert_eq!(info.width, Some(640));
    assert_eq!(info.height, Some(360));
    assert_eq!(info.duration_us, None, "images have no duration");
    assert!(!info.has_audio);
}

#[test]
fn probe_rejects_text_file() {
    let assets = skip_unless_fixtures!();
    let dir = tempfile::tempdir().expect("tempdir");
    let txt = dir.path().join("notes.txt");
    std::fs::write(&txt, b"this is not media, just some text\n").expect("write txt");
    let err = probe(&txt).expect_err("text file must be rejected");
    // Any error is acceptable; assert it is the Unsupported classification.
    assert!(
        matches!(err, dv_media::MediaError::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
    let _ = assets; // keep the fixture-guard symmetry
}

#[test]
fn keyframe_index_build_save_load_roundtrip() {
    let assets = skip_unless_fixtures!();
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = KeyframeIndex::build(&assets.join("basic.mp4")).expect("build index");
    assert!(!idx.entries.is_empty(), "index has packets");
    assert!(idx.keyframe_count() >= 1, "at least one keyframe");

    let path = dir.path().join("index.bin");
    idx.save(&path).expect("save");
    let loaded = KeyframeIndex::load(&path).expect("load");
    assert_eq!(idx, loaded, "roundtrip must be lossless");

    // keyframe_before: the first keyframe should be at/near t=0, and a query
    // partway in should return a keyframe no later than the query.
    let first = idx.keyframe_before(i64::MAX).expect("some keyframe");
    assert!(first.keyframe);
    if let Some(kf) = idx.keyframe_before(1_500_000) {
        assert!(kf.pts_us.unwrap_or(i64::MAX) <= 1_500_000);
    }
}

#[test]
fn keyframe_index_longgop() {
    let assets = skip_unless_fixtures!();
    // g=300 over 5 s @30fps → very few keyframes but many packets.
    let idx = KeyframeIndex::build(&assets.join("longgop.mp4")).expect("build longgop");
    assert!(
        idx.entries.len() > idx.keyframe_count(),
        "long GOP: sparse keyframes"
    );
    assert!(idx.keyframe_count() >= 1);
}

#[test]
fn thumbnails_count_and_valid_jpegs() {
    let assets = skip_unless_fixtures!();
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("thumbs");
    let n = generate_thumbnails(&assets.join("basic.mp4"), &out, ThumbnailOpts::default())
        .expect("thumbnails");
    // 3 s at ~1/s → 3 thumbnails (buckets 0,1,2), allow one extra either way.
    assert!((2..=4).contains(&n), "expected ~3 thumbnails, got {n}");

    // Every produced file is a valid, correctly-sized JPEG.
    let mut files: Vec<_> = std::fs::read_dir(&out)
        .expect("read thumbs dir")
        .map(|e| e.expect("dirent").path())
        .collect();
    files.sort();
    assert_eq!(files.len(), n);
    for f in &files {
        assert_eq!(f.extension().and_then(|e| e.to_str()), Some("jpg"));
        let img = image::open(f).expect("decode thumbnail jpeg");
        assert_eq!(img.width(), 160, "thumbnail width is 160 px");
        // 640x360 source → 160x90.
        assert_eq!(img.height(), 90);
    }
    // First file is 0-padded to 4 digits.
    assert_eq!(
        files[0].file_name().expect("file name").to_str(),
        Some("0000.jpg")
    );
}

#[test]
fn waveform_roundtrip_plausible_peaks() {
    let assets = skip_unless_fixtures!();
    let dir = tempfile::tempdir().expect("tempdir");
    let pk = dir.path().join("waveform.pk");
    // gappy.wav: a 440 Hz sine for ~1 s, then ~2 s of silence (PCM, full-
    // fidelity). Early buckets ride the tone; late buckets are silent — so the
    // peaks are plausible AND ordered correctly in time.
    let meta = generate_waveform(&assets.join("gappy.wav"), &pk).expect("waveform");
    assert_eq!(meta.sample_rate, 48_000);
    assert_eq!(meta.samples_per_bucket, 256);
    assert!(
        meta.count > 100,
        "several buckets over ~3 s: {}",
        meta.count
    );

    let wf = Waveform::load(&pk).expect("load waveform.pk");
    assert_eq!(wf.meta, meta);
    assert_eq!(wf.peaks.len(), meta.count);

    // A bucket early in the tone: a real sine swings both ways.
    let early = &wf.peaks[10];
    assert!(early.0 <= -8, "tone min should be negative: {}", early.0);
    assert!(early.1 >= 8, "tone max should be positive: {}", early.1);
    // The final bucket is deep in the silent tail: essentially flat at zero.
    let last = wf.peaks.last().expect("last bucket");
    assert!(last.0.abs() <= 2 && last.1.abs() <= 2, "silence: {last:?}");
}

// ---- M4: scrub proxies (§4.3) ----

#[test]
fn proxy_of_longgop_is_all_intra_half_height() {
    let assets = skip_unless_fixtures!();
    let dir = tempfile::tempdir().expect("tempdir");
    let dst = dir.path().join("proxy.mp4");

    // longgop.mp4: 1280x720, 5 s, g=300. The proxy must be all-intra and
    // quarter-ish (720/2 = 360, under the 960 cap).
    let mut max_progress = 0.0f32;
    generate_proxy(
        &assets.join("longgop.mp4"),
        &dst,
        &ProxyOpts::default(),
        &mut |p| max_progress = max_progress.max(p),
    )
    .expect("generate proxy");

    assert!(dst.exists(), "proxy.mp4 was written into place");
    // No leftover temp sibling.
    assert!(
        !dir.path().join("proxy.mp4.part").exists(),
        ".part must be renamed away on success"
    );

    // Every frame a keyframe → longest GOP is 1.
    let idx = KeyframeIndex::build(&dst).expect("build proxy index");
    assert_eq!(idx.max_gop_frames(), 1, "proxy must be all-intra");

    // Geometry + duration match the source (within container slack).
    let src_info = probe(&assets.join("longgop.mp4")).expect("probe source");
    let proxy_info = probe(&dst).expect("probe proxy");
    assert_eq!(proxy_info.height, Some(360), "half of 720");
    let src_dur = src_info.duration_us.expect("source duration") as f64;
    let proxy_dur = proxy_info.duration_us.expect("proxy duration") as f64;
    assert!(
        (src_dur - proxy_dur).abs() < 500_000.0,
        "proxy duration within 0.5 s (src {src_dur}, proxy {proxy_dur})"
    );

    assert!(
        max_progress >= 0.9,
        "progress should approach completion, peaked at {max_progress}"
    );
}

#[test]
fn proxy_of_basic_keeps_audio() {
    let assets = skip_unless_fixtures!();
    let dir = tempfile::tempdir().expect("tempdir");
    let dst = dir.path().join("proxy.mp4");
    generate_proxy(
        &assets.join("basic.mp4"),
        &dst,
        &ProxyOpts::default(),
        &mut |_| {},
    )
    .expect("generate proxy");

    let info = probe(&dst).expect("probe proxy");
    assert!(info.has_audio, "audio stream must survive proxying");
    assert_eq!(info.audio_codec.as_deref(), Some("aac"), "AAC audio");
    // basic.mp4 is 640x360; 360/2 = 180 (under the cap).
    assert_eq!(info.height, Some(180), "half of 360");
}

// ---- M2: streaming decoders (§4.1, §4.3) ----

#[test]
fn video_decoder_produces_monotonic_nv12_frames() {
    let assets = skip_unless_fixtures!();
    let mut dec = dv_media::VideoDecoder::open(&assets.join("basic.mp4")).expect("open");
    assert_eq!(dec.decode_path(), dv_media::DecodePath::Software);
    assert!((dec.frame_duration_us() - 33_333).abs() <= 1);

    let mut last_pts = i64::MIN;
    let mut count = 0;
    while let Some(frame) = dec.next_frame().expect("decode") {
        assert_eq!(frame.width, 640);
        assert_eq!(frame.height, 360);
        assert_eq!(frame.y.len(), 640 * 360);
        assert_eq!(frame.uv.len(), 640 * 360 / 2);
        assert!(frame.pts_us > last_pts, "pts must be monotonic");
        last_pts = frame.pts_us;
        count += 1;
        if count >= 20 {
            break;
        }
    }
    assert_eq!(count, 20);
}

#[test]
fn video_decoder_seek_lands_on_or_before_target() {
    let assets = skip_unless_fixtures!();
    // longgop.mp4: 5 s @ 30 fps, g=300 → keyframe only at 0. A mid-file seek
    // must land at/before the target and reach it by decoding forward.
    let mut dec = dv_media::VideoDecoder::open(&assets.join("longgop.mp4")).expect("open");
    let target = 2_000_000; // 2 s
    dec.seek(target).expect("seek");
    let mut frame = dec.next_frame().expect("decode").expect("frame");
    assert!(frame.pts_us <= target, "seek must land before target");
    while frame.pts_us + dec.frame_duration_us() <= target {
        frame = dec.next_frame().expect("decode").expect("next");
    }
    // The frame covering 2 s is within one frame of the target.
    assert!((frame.pts_us - target).abs() <= dec.frame_duration_us());
}

#[test]
fn video_decoder_seek_back_to_zero_restarts() {
    let assets = skip_unless_fixtures!();
    let mut dec = dv_media::VideoDecoder::open(&assets.join("basic.mp4")).expect("open");
    for _ in 0..10 {
        dec.next_frame().expect("decode").expect("frame");
    }
    dec.seek(0).expect("seek");
    let first = dec.next_frame().expect("decode").expect("frame");
    assert!(first.pts_us <= 40_000, "seek(0) restarts at the head");
}

#[test]
fn audio_decoder_yields_48k_stereo_covering_duration() {
    let assets = skip_unless_fixtures!();
    let mut dec = dv_media::AudioDecoder::open(&assets.join("tone.m4a")).expect("open");
    assert!(dec.duration_us() > 2_500_000, "≈3 s fixture");
    let mut total_frames = 0usize;
    let mut nonzero = false;
    while let Some(chunk) = dec.next_chunk().expect("decode") {
        assert_eq!(chunk.samples.len() % dv_media::AUDIO_CHANNELS, 0);
        total_frames += chunk.samples.len() / dv_media::AUDIO_CHANNELS;
        nonzero |= chunk.samples.iter().any(|s| s.abs() > 0.05);
    }
    let secs = total_frames as f64 / dv_media::AUDIO_RATE as f64;
    assert!(
        (secs - 3.0).abs() < 0.2,
        "expected ≈3 s of samples, got {secs}"
    );
    assert!(nonzero, "sine fixture must not be silent");
}

#[test]
fn audio_decoder_seek_positions_chunks() {
    let assets = skip_unless_fixtures!();
    let mut dec = dv_media::AudioDecoder::open(&assets.join("basic.mp4")).expect("open");
    dec.seek(1_500_000).expect("seek");
    let chunk = dec.next_chunk().expect("decode").expect("chunk");
    // Container seek lands on the keyframe grid at/before the target
    // (basic.mp4 has g=30 ≈ 1 s); the caller trims forward from there.
    assert!(
        chunk.start_us <= 1_500_000 && 1_500_000 - chunk.start_us < 1_200_000,
        "chunk starts at/shortly before the seek target, got {}",
        chunk.start_us
    );
}

#[test]
fn hw_probe_does_not_crash_and_falls_back() {
    let assets = skip_unless_fixtures!();
    // Whatever the machine has: opening with the probed device (or None) must
    // produce identical-shaped frames — fallback is silent (§4.1).
    let hw = dv_media::HwDevice::probe_all();
    let mut dec = dv_media::VideoDecoder::open_with(&assets.join("basic.mp4"), &hw).expect("open");
    let frame = dec.next_frame().expect("decode").expect("frame");
    assert_eq!(frame.y.len(), 640 * 360);
    assert_eq!(frame.uv.len(), 640 * 360 / 2);
    eprintln!("decode path: {}", dec.decode_path().label());
}

// ---- M5: sync external audio (§5) ----

#[test]
fn sync_recovers_known_delay() {
    // Needs the ffmpeg CLI to synthesize the two fixtures; skip cleanly if
    // absent (same policy as the shared fixtures).
    if !ffmpeg_available() {
        eprintln!("SKIP: `ffmpeg` CLI not on PATH — cannot synthesize sync fixtures");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let cam = dir.path().join("cam.wav");
    let ext = dir.path().join("ext.wav");

    // Camera audio: pink noise gated into irregular bursts (a sum of three
    // incommensurate sines thresholded on/off). This mimics speech — bursts of
    // sound separated by near-silence — so the amplitude envelope has sharp,
    // non-periodic structure that yields a prominent (confident) correlation
    // peak. Flat or slowly-modulated noise gives a broad, ambiguous peak.
    let mk_cam = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "anoisesrc=d=6:c=pink:r=48000:a=0.9:seed=7",
            "-af",
            "volume='0.02+0.98*gt(sin(2*PI*0.5*t)+sin(2*PI*1.1*t)+sin(2*PI*2.3*t),1.2)':eval=frame",
            "-ac",
            "1",
        ])
        .arg(&cam)
        .status()
        .expect("run ffmpeg (cam)");
    assert!(mk_cam.success(), "ffmpeg failed to make cam.wav");

    // External file: exactly the camera audio delayed by 500 ms (adelay
    // prepends silence). Identical content, only shifted — the ideal target.
    let delay_ms = 500i64;
    let mk_ext = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .arg("-i")
        .arg(&cam)
        .args(["-af", &format!("adelay={delay_ms}"), "-ac", "1"])
        .arg(&ext)
        .status()
        .expect("run ffmpeg (ext)");
    assert!(mk_ext.success(), "ffmpeg failed to make ext.wav");

    let cam_pk = dir.path().join("cam.pk");
    let ext_pk = dir.path().join("ext.pk");
    generate_waveform(&cam, &cam_pk).expect("cam waveform");
    generate_waveform(&ext, &ext_pk).expect("ext waveform");

    let t0 = std::time::Instant::now();
    let res = sync_offset(&cam, &cam_pk, &ext, &ext_pk).expect("sync_offset");
    let elapsed = t0.elapsed();
    eprintln!(
        "sync_offset: offset_us={} confidence={:.3} in {:?}",
        res.offset_us, res.confidence, elapsed
    );

    // ext = cam delayed 500 ms → external content sits 0.5 s later on its own
    // clock, so camera_time = external_time − 0.5 s → offset_us ≈ −500_000.
    let expected = -delay_ms * 1000;
    assert!(
        (res.offset_us - expected).abs() <= 2_000,
        "recovered offset {} should be within ±2 ms of {}",
        res.offset_us,
        expected
    );
    assert!(
        res.confident(),
        "a clean identical-content match must be confident: {}",
        res.confidence
    );
}
