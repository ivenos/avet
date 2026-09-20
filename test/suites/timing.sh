#!/bin/sh
# Timestamps and sync: variable and fractional frame rates, stream offsets, start times.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

encode() { # NAME FIXTURE [SCENE_SPLIT]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    cat > "$I/p/encode.toml" << EOF
encoder = "svt-av1"
[encoder_params]
preset = 8
crf    = 40
[scene_detection]
extra_split = ${3:-60}
EOF
    run_avet "$I" "$O" "$O/test.mkv" 180 || fail "$1: no output"
}

# -- variable frame rate: every frame keeps its own timestamp --------------------------
encode vfr pattern_vfr.mkv
assert_log_contains "variable frame rate"
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"

# -- 23.976 fps: no drift from rounding the frame duration -----------------------------
encode ntsc pattern_ntsc.mkv
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"
assert_av_sync           "$O/test.mkv" "$SRC"

# -- video that starts after the audio, and audio that starts after the video ----------
for case in video_delay audio_delay; do
    encode "$case" "pattern_$case.mkv"
    assert_frames_match "$O/test.mkv" "$SRC"
    assert_av_sync      "$O/test.mkv" "$SRC" 0
    assert_av_sync      "$O/test.mkv" "$SRC" 1
    for spec in s:0 s:1 s:2; do
        assert_stream_times_match "$O/test.mkv" "$spec" "$SRC" "$spec"
    done
    assert_chapters_match "$O/test.mkv" "$SRC"
done

# -- every stream five seconds into the file ------------------------------------------
encode global_offset pattern_global_offset.mkv
assert_frames_match "$O/test.mkv" "$SRC"
assert_av_sync      "$O/test.mkv" "$SRC" 0
assert_av_sync      "$O/test.mkv" "$SRC" 1
for spec in s:0 s:1 s:2; do
    assert_stream_times_match "$O/test.mkv" "$spec" "$SRC" "$spec"
done
assert_chapters_match "$O/test.mkv" "$SRC"
assert_stream_value "$O/test.mkv" v:0 stream=start_time "0.000000"
assert_log_not_contains "Warning"

# -- MPEG-TS: a start time far from zero, audio 321 ms ahead of the video --------------
encode ts_offset pattern_offset.m2ts
assert_frames_match "$O/test.mkv" "$SRC"
assert_av_sync      "$O/test.mkv" "$SRC" 0

# -- the same, video passed through ----------------------------------------------------
I="$WORKDIR/ts_copy/in"; O="$WORKDIR/ts_copy/out"; mkdir -p "$I/p" "$O"
cp "$SRC" "$I/p/test.m2ts"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "ts copy: no output"
assert_av_sync "$O/test.mkv" "$SRC" 0

test_done
