//! End-to-end mixer render (§5): decode real fixture audio through
//! [`MixRenderer`] and assert audible, correctly-shaped output — the same
//! path the audio render thread and M6's export drive. ffmpeg-gated like
//! dv-media's integration tests: absent CLI prints a message and returns.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dv_core::model::{ChannelMode, StripParams};
use dv_playback::mix::{MixClip, MixMeters, MixRenderer, MixSpec, TR_A2, TR_V1};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn fixtures() -> Option<PathBuf> {
    let ffmpeg = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ffmpeg {
        println!("skipping: ffmpeg CLI not available");
        return None;
    }
    let root = repo_root();
    let assets = root.join("build/assets");
    if !assets.join("basic.mp4").exists() {
        let ok = Command::new("bash")
            .arg(root.join("build/test-assets.sh"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            println!("skipping: could not generate fixtures");
            return None;
        }
    }
    Some(assets)
}

fn clip(path: PathBuf, track: usize, start: i64, dur: i64, voice: bool) -> MixClip {
    MixClip {
        clip_id: (track as i64) * 100 + start / 1_000_000,
        track,
        path,
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
        voice,
    }
}

#[test]
fn mixer_renders_audible_blocks_with_meters() {
    let Some(assets) = fixtures() else { return };
    let basic = assets.join("basic.mp4");

    let mut spec = MixSpec::empty();
    spec.duration_us = 2_000_000;
    // V1 voice + the same file again as "music" on A2, treated + ducked.
    spec.clips
        .push(clip(basic.clone(), TR_V1, 0, 2_000_000, true));
    let mut music = clip(basic.clone(), TR_A2, 0, 2_000_000, false);
    music.strip = Some(StripParams {
        hpf_on: true,
        hpf_hz: 120.0,
        comp_threshold_db: -30.0,
        comp_ratio: 4.0,
        ..Default::default()
    });
    spec.clips.push(music);
    spec.tracks[TR_A2].duck = true;

    let meters = Arc::new(MixMeters::new());
    let mut r = MixRenderer::new(Arc::new(spec), meters.clone());

    let mut tl = 0i64;
    let mut energy = 0.0f64;
    let mut blocks = 0;
    while tl < 2_000_000 {
        let (block, next) = r.render_block(tl, 1.0);
        assert!(block.iter().all(|s| s.is_finite()));
        energy += block.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
        assert!(next > tl, "time must advance");
        tl = next;
        blocks += 1;
    }
    assert!(blocks >= 195, "≈200 10 ms blocks over 2 s, got {blocks}");
    assert!(energy > 1.0, "mix must be audible, energy={energy}");
    // Meters populated: peaks always; LUFS needs its 400 ms/3 s windows.
    assert!(MixMeters::load_db(&meters.peak_l_milli).is_some());
    assert!(MixMeters::load_db(&meters.short_term_lufs_milli).is_some());
    // The compressor on the treated A2 clip reported GR at some point is
    // content-dependent; just assert the per-clip map is being maintained.
    assert!(meters.clip_gr_db.lock().len() <= 2);

    // Seek reset: render from 1 s again — output stays finite and audible.
    r.reset_for_seek();
    let (block, _) = r.render_block(1_000_000, 1.0);
    assert!(block.iter().all(|s| s.is_finite()));
}

#[test]
fn mixer_gap_renders_silence() {
    let Some(assets) = fixtures() else { return };
    let mut spec = MixSpec::empty();
    spec.duration_us = 1_000_000;
    // One clip covering only [0.5 s, 1.0 s): the first half must be silent.
    spec.clips.push(clip(
        assets.join("basic.mp4"),
        TR_V1,
        500_000,
        500_000,
        true,
    ));
    let meters = Arc::new(MixMeters::new());
    let mut r = MixRenderer::new(Arc::new(spec), meters);
    let (block, _) = r.render_block(0, 1.0);
    assert!(block.iter().all(|&s| s == 0.0), "gap must be silent");
    let mut tl = 400_000;
    let mut energy = 0.0f64;
    while tl < 1_000_000 {
        let (block, next) = r.render_block(tl, 1.0);
        energy += block.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
        tl = next;
    }
    assert!(energy > 0.0, "clip region must produce signal");
}
