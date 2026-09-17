#!/bin/sh
# Containers and GOP structures, checked against the lossless pattern each fixture was made from.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

MASTER="$FIXTURES_DIR/pattern.mkv"
ENCODE='encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 40\n[scene_detection]\nextra_split = 24\n'

run() { # NAME FIXTURE PROFILE
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf '%b' "$3" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 300 || fail "$1: no output"
}

# -- chunks start inside open GOPs, B-frame pyramids and CRA pictures, in every container --
for fixture in gop_h264.mkv gop_h264.mp4 gop_h264.ts gop_h264.flv gop_h264.mov gop_hevc.ts \
               gop_mpeg2.m2ts gop_mpeg4.avi gop_vp9.webm; do
    run "$fixture" "$fixture" "$ENCODE"
    assert_frames_match      "$O/test.mkv" "$MASTER"
    assert_frame_times_match "$O/test.mkv" "$MASTER"
    assert_av_sync           "$O/test.mkv" "$MASTER" 0
    assert_decodes_cleanly   "$O/test.mkv"
    [ "$fixture" != gop_mpeg4.avi ] || assert_log_contains "AVI with B-frames: working from a Matroska copy of the video"
done

# -- an MP4 cut behind a keyframe: what the edit list hides stays hidden --------------------
# The cut starts the sound at 3.3 s and the picture at frame 80, 33 ms apart, as in the source.
run cut gop_cut.mp4 "$ENCODE"
assert_frames_match      "$O/test.mkv" "$FIXTURES_DIR/pattern_from80.mkv"
assert_frame_times_match "$O/test.mkv" "$FIXTURES_DIR/pattern_from80.mkv"
assert_av_sync           "$O/test.mkv" "$SRC" 0
assert_subtitle_events_match "$O/test.mkv" s:0 "$SRC" s:0

# -- an MPEG-TS whose timestamps wrap a minute in ----------------------------------------------
run wrap gop_wrap.m2ts "$ENCODE"
assert_frames_match      "$O/test.mkv" "$FIXTURES_DIR/long.mkv"
assert_frame_times_match "$O/test.mkv" "$FIXTURES_DIR/long.mkv"
assert_av_sync           "$O/test.mkv" "$FIXTURES_DIR/long.mkv" 0
assert_subtitle_events_match "$O/test.mkv" s:0 "$SRC" s:0

test_done
