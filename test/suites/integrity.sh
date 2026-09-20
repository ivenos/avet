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
    # -- ten chunks or more: nothing lost, doubled, reordered or shifted anywhere ----
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
    assert_log_not_contains     "Warning"
    assert_seeks_land_on_frames "$O/test.mkv"
    # The source is archived, not rewritten.
    assert_same_bytes "$I/processed/test.mkv" "$SRC"
done

[ "$(packet_hashes "$WORKDIR/svt-av1/out/test.mkv" v:0)" \
    != "$(packet_hashes "$WORKDIR/svt-av1-hdr/out/test.mkv" v:0)" ] || \
    fail "svt-av1 and svt-av1-hdr produced the same bitstream - both ran the same binary"

# -- video = copy: the video stream comes through bit for bit ------------------------
I="$WORKDIR/copy/in"; O="$WORKDIR/copy/out"; mkdir -p "$I/p" "$O"
cp "$SRC" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
[avet]
video = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "copy: no output"
assert_packets_identical "$O/test.mkv" v:0 "$SRC" v:0
assert_everything_but_video_kept "$O/test.mkv"

# -- re-encoded audio keeps its sync, and a lossless target keeps every sample -------
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

# -- every channel stays on its own channel through Opus ------------------------------
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

test_done
