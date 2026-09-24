#!/bin/sh
# Tests for encode.rs: output codec, encoder param injection, keyint and HDR override logic.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# -- output video codec is av1 -------------------------------------------------
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
mkvmerge -q -o "$I/p/test.mkv" --default-track-flag 0:yes --language 0:jpn "$FIXTURES_DIR/sdr_simple.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "codec: no output"
assert_video_codec "$O/test.mkv" av1
assert_stream_value "$O/test.mkv" v:0 stream_disposition=default:stream_tags=language "1 jpn"

# -- manual keyint in encoder_params: auto-keyint logged but not injected ------
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
keyint = 240
[avet]
keyint = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "keyint override: no output"
assert_log_contains     "auto-keyint"
assert_log_contains     "keyint=240"

# -- auto-HDR param skipped when encoder_params has the same key ---------------
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset           = 12
crf              = 50
color-primaries  = 1
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HDR override: no output"
assert_log_contains     "color-primaries=1"
assert_color_primaries  "$O/test.mkv" "bt709"

# -- auto-keyint with no manual override: a keyframe every five seconds --------
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keyint = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "auto-keyint: no output"
assert_log_contains "auto-keyint: 120"
KEYFRAMES=$(keyframe_indices "$O/test.mkv" | tr '\n' ' ')
[ "$KEYFRAMES" = "0 120 " ] || fail "auto-keyint: keyframes at [$KEYFRAMES], expected 0 and 120"

# -- a manual keyint reaches the bitstream too ---------------------------------
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
keyint = 30
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "manual keyint: no output"
KEYFRAMES=$(keyframe_indices "$O/test.mkv" | tr '\n' ' ')
[ "$KEYFRAMES" = "0 30 60 90 120 150 180 210 " ] || \
    fail "keyint=30: keyframes at [$KEYFRAMES], expected every 30 frames"

# -- bit_depth: 8 to 10 bits and back, conversion logged; a matching depth logs none ---
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
bit_depth = 10
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "bit_depth 8 to 10: no output"
assert_video_pix_fmt "$O/test.mkv" "yuv420p10le"
assert_log_contains  "bit-depth conversion: 8-bit to 10-bit"

I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
bit_depth = 8
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "bit_depth 10 to 8: no output"
assert_video_pix_fmt "$O/test.mkv" "yuv420p"
assert_log_contains  "bit-depth conversion: 10-bit to 8-bit"

I="$WORKDIR/8/in"; O="$WORKDIR/8/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
bit_depth = 8
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "bit_depth matching: no output"
assert_video_pix_fmt    "$O/test.mkv" "yuv420p"
assert_log_not_contains "bit-depth conversion"

# -- video = copy: video kept, only audio re-encoded, no encoder needed -------
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_named_audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
[avet]
video = "copy"
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = "256k"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "video copy: no output"
assert_log_contains "copy video"
assert_video_codec  "$O/test.mkv" h264
assert_audio_codec  "$O/test.mkv" 0 opus
assert_audio_title  "$O/test.mkv" 0 "Deutsch Dolby Digital 5.1 (Opus)"

# -- variable frame rate: the output keeps the source timestamps --------------
I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_vfr.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "vfr: no output"
assert_log_contains "variable frame rate"
assert_video_frames "$O/test.mkv" 180
PTS=$(ffprobe -v error -select_streams v:0 -show_entries packet=pts_time -of csv=p=0 \
    "$O/test.mkv" 2>/dev/null | sort -n | sed -n '61p;180p' | tr '\n' ' ')
echo "$PTS" | awk '{ exit !($1 > 1.99 && $1 < 2.01 && $2 > 3.97 && $2 < 3.99) }' || \
    fail "vfr: frames 60 and 179 at ${PTS}s, expected 2.0 and 3.983"

test_done
