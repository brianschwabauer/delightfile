#!/usr/bin/env bash
# Generate tiny media fixtures for dv-media integration tests (PLAN.md §11).
# Output lands in build/assets/ (gitignored); safe to re-run, skips existing.
set -euo pipefail

cd "$(dirname "$0")"
mkdir -p assets
cd assets

gen() {
    local name="$1"
    shift
    if [[ -f $name ]]; then
        echo "skip  $name"
    else
        echo "make  $name"
        ffmpeg -v error -y "$@" "$name"
    fi
}

# 3 s of 640x360 30 fps H.264 + 440 Hz tone — the everyday fixture.
gen basic.mp4 \
    -f lavfi -i "testsrc2=size=640x360:rate=30:duration=3" \
    -f lavfi -i "sine=frequency=440:duration=3" \
    -c:v libx264 -pix_fmt yuv420p -g 30 -c:a aac -shortest

# Long-GOP (g=300) to exercise the keyframe index and proxy decision rule.
gen longgop.mp4 \
    -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=5" \
    -c:v libx264 -pix_fmt yuv420p -g 300

# Audio-only file (valid import, lands on A1/A2 — §9).
gen tone.m4a \
    -f lavfi -i "sine=frequency=220:duration=3" -c:a aac

# Still image (media kind 'image' — §9).
gen still.png \
    -f lavfi -i "testsrc2=size=640x360:rate=1:duration=1" -frames:v 1

# A file with silence gaps for the silence-detection tests (§1).
gen gappy.wav \
    -f lavfi -i "sine=frequency=440:duration=1" \
    -af "apad=pad_dur=1,asetpts=PTS-STARTPTS" -t 3


# Chaptered mkv (§9) — three named chapters over 6 s, the last title non-ASCII
# so the viewer's strip sanitizer has something real to chew on.
if [[ -f chapters.mkv ]]; then
    echo "skip  chapters.mkv"
else
    echo "make  chapters.mkv"
    cat >chapters.ffmeta <<'META'
;FFMETADATA1
[CHAPTER]
TIMEBASE=1/1000
START=0
END=2000
title=Intro
[CHAPTER]
TIMEBASE=1/1000
START=2000
END=4000
title=Middle Part
[CHAPTER]
TIMEBASE=1/1000
START=4000
END=6000
title=Fin — ünïcode
META
    ffmpeg -v error -y \
        -f lavfi -i "testsrc2=size=320x180:rate=15:duration=6" \
        -i chapters.ffmeta -map_metadata 1 \
        -c:v libx264 -pix_fmt yuv420p -g 15 chapters.mkv
    rm -f chapters.ffmeta
fi

echo "done: $(ls | wc -l) fixtures in build/assets/"
