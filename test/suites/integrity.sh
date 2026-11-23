#!/bin/sh
# End-to-end integrity: every frame, timestamp, sample, subtitle, chapter and attachment.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

SRC="$FIXTURES_DIR/pattern.mkv"

assert_everything_but_video_kept() {
    local out="$1"
    assert_frame_times_match "$out" "$SRC"
    assert_av_sync "$out" "$SRC" 0
    assert_av_sync "$out" "$SRC" 1
    for spec in a:0 a:1 s:0 s:1 s:2; do
        assert_packets_identical  "$out" "$spec" "$SRC" "$spec"
        assert_stream_times_match "$out" "$spec" "$SRC" "$spec"
    done
    assert_tracks_match      "$out" "$SRC" a
    assert_tracks_match      "$out" "$SRC" s
    assert_chapters_match    "$out" "$SRC"
    assert_attachments_match "$out" "$SRC"
    assert_decodes_cleanly   "$out"
}

for encoder in svt-av1 svt-av1-hdr; do
    # ten chunks or more: nothing lost, doubled, reordered or shifted anywhere
    I="$WORKDIR/$encoder/in"; O="$WORKDIR/$encoder/out"; mkdir -p "$I/p" "$O"
    cp "$SRC" "$I/p/test.mkv"
    cat > "$I/p/encode.toml" << EOF
encoder = "$encoder"
[encoder_params]
preset = 8
crf    = 40
[avet]
keep_temp = true
[scene_detection]
extra_split = 24
EOF
    run_avet "$I" "$O" "$O/test.mkv" 180 || fail "$encoder: no output"
    chunks=$(log_capture 's/.*\] \([0-9]*\) chunks$/\1/p')
    [ "${chunks:-0}" -ge 10 ] || fail "$encoder: expected at least 10 chunks, got '$chunks'"
    assert_video_codec  "$O/test.mkv" av1
    assert_frames_match "$O/test.mkv" "$SRC"
    assert_everything_but_video_kept "$O/test.mkv"
    assert_keyframes_at_chunks  "$O/test.mkv" "$O/.avet_test/scenes.json"
    assert_log_not_contains     " WARN "
    assert_seeks_land_on_frames "$O/test.mkv"
    # The source is archived, not rewritten.
    assert_same_bytes "$I/processed/test.mkv" "$SRC"
done

[ "$(packet_hashes "$WORKDIR/svt-av1/out/test.mkv" v:0)" \
    != "$(packet_hashes "$WORKDIR/svt-av1-hdr/out/test.mkv" v:0)" ] || \
    fail "svt-av1 and svt-av1-hdr produced the same bitstream - both ran the same binary"

# video = copy: the video stream comes through bit for bit
I="$WORKDIR/copy/in"; O="$WORKDIR/copy/out"; mkdir -p "$I/p" "$O"
cp "$SRC" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
[avet]
video = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "copy: no output"
assert_packets_identical "$O/test.mkv" v:0 "$SRC" v:0
assert_everything_but_video_kept "$O/test.mkv"

# re-encoded audio keeps its sync, and a lossless target keeps every sample
I="$WORKDIR/audio/in"; O="$WORKDIR/audio/out"; mkdir -p "$I/p" "$O"
cp "$SRC" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 8
crf    = 40
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = "128k"
[audio.lossless]
codec   = "flac"
options = { compression_level = 12 }
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "audio encode: no output"
assert_audio_codec "$O/test.mkv" 0 flac
assert_audio_codec "$O/test.mkv" 1 opus
assert_track_flags_match "$O/test.mkv" "$SRC" a
assert_audio_title "$O/test.mkv" 0 "English (FLAC)"
assert_audio_title "$O/test.mkv" 1 "Kommentar (Opus)"
assert_audio_samples_identical "$O/test.mkv" "$SRC" 0
assert_av_sync "$O/test.mkv" "$SRC" 0
assert_av_sync "$O/test.mkv" "$SRC" 1
assert_frames_match "$O/test.mkv" "$SRC"

# every channel stays on its own channel through Opus
for layout in 71 51side; do
    I="$WORKDIR/tones$layout/in"; O="$WORKDIR/tones$layout/out"; mkdir -p "$I/p" "$O"
    cp "$FIXTURES_DIR/tones_$layout.mkv" "$I/p/test.mkv"
    cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 8
crf    = 40
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = "256k"
EOF
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "tones $layout: no output"
    assert_audio_codec "$O/test.mkv" 0 opus
    case "$layout" in
        71)     assert_channel_frequencies "$O/test.mkv" 0 "300 500 700 90 1100 1300 1700 1900" ;;
        # Opus has no side channels: 5.1(side) comes out as 5.1 with them as the back pair.
        51side) assert_channel_frequencies "$O/test.mkv" 0 "300 500 700 90 1700 1900" ;;
    esac
done

# a few frames FFMS2 leaves at a smaller size fail the job instead of reading past them
segment() { # COLOR SIZE SECONDS OFFSET FILE
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "color=$1:s=$2:r=24:d=$3" \
        -c:v libx264 -preset veryfast -bf 3 -refs 3 -pix_fmt yuv420p \
        -x264-params scenecut=0:keyint=100:repeat-headers=1 -f mpegts -output_ts_offset "$4" "$5"
}
I="$WORKDIR/500/in"; O="$WORKDIR/500/out"; mkdir -p "$I/p" "$O"
segment gray 1280x720 1 0 "$WORKDIR/500/a.ts"
segment red 1280x64 0.125 1 "$WORKDIR/500/b.ts"
segment gray 1280x720 1 1.125 "$WORKDIR/500/c.ts"
cat "$WORKDIR/500/a.ts" "$WORKDIR/500/b.ts" "$WORKDIR/500/c.ts" > "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 40\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 120 "job failed"
assert_log_contains    "FFMS2 did not scale"
assert_file_exists     "$O/.avet_test/.failed"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.ts"

# a lasting size change, which FFMS2 scales, keeps every frame and the whole picture
I="$WORKDIR/501/in"; O="$WORKDIR/501/out"; mkdir -p "$I/p" "$O"
segment gray 640x360 2 0 "$WORKDIR/501/a.ts"
segment red 320x180 2 2 "$WORKDIR/501/b.ts"
cat "$WORKDIR/501/a.ts" "$WORKDIR/501/b.ts" > "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 40\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "lasting size change: no output"
assert_log_not_contains "FFMS2 did not scale"
assert_video_frames "$O/test.mkv" 96
assert_video_height "$O/test.mkv" 360
spread=$(ffmpeg -hide_banner -loglevel error -i "$O/test.mkv" \
    -vf "select=eq(n\,72),crop=iw:ih/2:0:ih/2,signalstats,metadata=print:file=-" -frames:v 1 -f null - 2>&1 |
    awk -F= '/YMIN/ { low = $2 } /YMAX/ { high = $2 } END { print high - low }')
[ "${spread:-99}" -lt 40 ] || fail "lasting size change: the scaled picture is not flat (luma spread '$spread')"

# three frames in another pixel format are converted from their own chroma planes
format_segment() { # COLOR SECONDS PIX_FMT OFFSET FILE
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "color=$1:s=1280x720:r=24:d=$2" \
        -c:v libx264 -preset veryfast -bf 3 -refs 3 -pix_fmt "$3" \
        -x264-params scenecut=0:keyint=100:repeat-headers=1 -f mpegts -output_ts_offset "$4" "$5"
}
I="$WORKDIR/502/in"; O="$WORKDIR/502/out"; mkdir -p "$I/p" "$O"
format_segment gray 1 yuv444p 0 "$WORKDIR/502/a.ts"
format_segment red 0.125 yuv420p 1 "$WORKDIR/502/b.ts"
format_segment gray 1 yuv444p 1.125 "$WORKDIR/502/c.ts"
cat "$WORKDIR/502/a.ts" "$WORKDIR/502/b.ts" "$WORKDIR/502/c.ts" > "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 20\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "short format change: no output"
spread=$(ffmpeg -hide_banner -loglevel quiet -i "$O/test.mkv" -vf "format=yuv420p,signalstats,metadata=print:file=-" -f null - 2>&1 |
    awk -F= '/UMIN/ { u = $2 } /UMAX/ { d = $2 - u; if (d > m) m = d } /VMIN/ { v = $2 } /VMAX/ { d = $2 - v; if (d > m) m = d } END { print int(m) }')
[ "${spread:-99}" -lt 40 ] || fail "short format change: chroma of a flat picture spreads over $spread"

# the first frames in another pixel format are read from their own planes
I="$WORKDIR/503/in"; O="$WORKDIR/503/out"; mkdir -p "$I/p" "$O"
format_segment red 0.125 yuv420p 0 "$WORKDIR/503/a.ts"
format_segment gray 1 yuv444p 0.125 "$WORKDIR/503/b.ts"
cat "$WORKDIR/503/a.ts" "$WORKDIR/503/b.ts" > "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 20\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "format change at the start: no output"
spread=$(ffmpeg -hide_banner -loglevel quiet -i "$O/test.mkv" -vf "format=yuv420p,signalstats,metadata=print:file=-" -f null - 2>&1 |
    awk -F= '/UMIN/ { u = $2 } /UMAX/ { d = $2 - u; if (d > m) m = d } /VMIN/ { v = $2 } /VMAX/ { d = $2 - v; if (d > m) m = d } END { print int(m) }')
[ "${spread:-99}" -lt 40 ] || fail "format change at the start: chroma of a flat picture spreads over $spread"

test_done
