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

# A tagged song: audio plus a still riding along as an ATTACHED_PIC stream,
# which is what every music player's "album art" actually is. The point of the
# fixture is that probe must call this Audio even though the container reports
# a video stream — see `best_video_stream`.
#
# Three passes rather than one `gen`, for the same reason chapters.mkv needs
# its own block: a single command with `-frames:v 1` on a lavfi source ends the
# *whole* encode at one frame's worth of time, and the tone comes out 26 ms
# long. The sleeve is baked first and then muxed in, which is also the order a
# real tagger does it. id3v2 v3 because that is what taggers in the wild write.
#
# The scratch files are `$$`-tagged and the result is moved into place, because
# `cargo test` runs one copy of this script per test thread: shared scratch
# names would have one run deleting an intermediate out from under another, and
# a half-written `cover.mp3` would be worse — the `-f` skip above would then
# hand every later run a corrupt fixture.
if [[ -f cover.mp3 ]]; then
    echo "skip  cover.mp3"
else
    echo "make  cover.mp3"
    art="cover-art-$$.png"
    tone="cover-tone-$$.mp3"
    out="cover-$$.mp3"
    ffmpeg -v error -y -f lavfi -i "color=c=orange:size=300x300:duration=1" \
        -frames:v 1 "$art"
    ffmpeg -v error -y -f lavfi -i "sine=frequency=330:duration=2" \
        -c:a libmp3lame -b:a 64k "$tone"
    ffmpeg -v error -y -i "$tone" -i "$art" \
        -map 0:a -map 1:v -c:a copy -c:v mjpeg \
        -disposition:v attached_pic -id3v2_version 3 "$out"
    mv -f "$out" cover.mp3
    rm -f "$art" "$tone"
fi

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
