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

# variable frame rate: every frame keeps its own timestamp
encode vfr pattern_vfr.mkv
assert_log_contains "variable frame rate"
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"

# 23.976 fps without drift, 300 fps past SvtAv1EncApp's limit of 240
encode fps300 pattern_300fps.mkv
assert_log_contains "encoding at a 1/2 rate"
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"

# 119.88 fps through the filter stage, which rounds a rate near 120 on its own
I="$WORKDIR/fps119/in"; O="$WORKDIR/fps119/out"; mkdir -p "$I/p" "$O"
SRC="$FIXTURES_DIR/pattern_119.mkv"
cp "$SRC" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\nscale = 64\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "fps119: no output"
assert_video_frames      "$O/test.mkv" 1199
assert_frame_times_match "$O/test.mkv" "$SRC"

encode ntsc pattern_ntsc.mkv
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"
assert_av_sync           "$O/test.mkv" "$SRC"

# video that starts after the audio, and audio that starts after the video
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

# every stream five seconds into the file
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

# the same with a subtitle only mkvmerge reads, video passed through and encoded
SRC="$FIXTURES_DIR/pattern_offset_vtt.mkv"
for mode in copy encode; do
    I="$WORKDIR/offset_$mode/in"; O="$WORKDIR/offset_$mode/out"; mkdir -p "$I/p" "$O"
    cp "$SRC" "$I/p/test.mkv"
    case "$mode" in
        copy)   printf '[avet]\nvideo = "copy"\n' ;;
        encode) printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' ;;
    esac > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "offset $mode: no output"
    assert_av_sync "$O/test.mkv" "$SRC" 0
    vtt=$(mkvmerge -i "$O/test.mkv" | sed -n 's/^Track ID \([0-9]*\): subtitles (WebVTT).*/\1/p')
    [ -n "$vtt" ] && mkvextract "$O/test.mkv" tracks "$vtt:$WORKDIR/offset_$mode/eng.vtt" >/dev/null
    grep -q "^00:00:01.000 --> 00:00:02.500" "$WORKDIR/offset_$mode/eng.vtt" 2>/dev/null \
        || fail "offset $mode: the WebVTT cue is not 1 s after the first frame: $(cat "$WORKDIR/offset_$mode/eng.vtt" 2>/dev/null)"
done

# MPEG-TS: a start time far from zero, audio 321 ms ahead of the video
encode ts_offset pattern_offset.m2ts
assert_frames_match "$O/test.mkv" "$SRC"
assert_av_sync      "$O/test.mkv" "$SRC" 0
assert_audio_codec  "$O/test.mkv" 0 aac

# the same, video passed through
I="$WORKDIR/ts_copy/in"; O="$WORKDIR/ts_copy/out"; mkdir -p "$I/p" "$O"
cp "$SRC" "$I/p/test.m2ts"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "ts copy: no output"
assert_av_sync "$O/test.mkv" "$SRC" 0

test_done
