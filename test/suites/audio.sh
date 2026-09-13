#!/bin/sh
# Tests for audio.rs: track selection, codec rules, language whitelist, channel preservation.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# -- copy: codecs and track count unchanged ------------------------------------
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_multiaudio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "copy: no output"
assert_audio_track_count "$O/test.mkv" 3
assert_audio_codec       "$O/test.mkv" 0 aac
assert_audio_codec       "$O/test.mkv" 1 ac3
assert_audio_codec       "$O/test.mkv" 2 aac

# -- global encode: all tracks transcoded -------------------------------------
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_multiaudio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "aac"
bitrate = "96k"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "global encode: no output"
assert_audio_track_count "$O/test.mkv" 3
assert_audio_codec       "$O/test.mkv" 0 aac
assert_audio_codec       "$O/test.mkv" 1 aac
assert_audio_codec       "$O/test.mkv" 2 aac

# -- codec_rule: ac3 to aac, others copied -------------------------------------
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_multiaudio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode = "copy"
[audio.codec_rules]
ac3 = { mode = "encode", codec = "aac", bitrate = "128k" }
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "codec_rule: no output"
assert_audio_track_count "$O/test.mkv" 3
assert_audio_codec       "$O/test.mkv" 0 aac
assert_audio_codec       "$O/test.mkv" 1 aac
assert_audio_codec       "$O/test.mkv" 2 aac

# -- language_whitelist: only deu track kept -----------------------------------
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_multiaudio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode               = "copy"
language_whitelist = ["deu"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "whitelist: no output"
assert_audio_track_count "$O/test.mkv" 1
assert_audio_codec       "$O/test.mkv" 0 ac3

# -- whitelist + codec_rule combined ------------------------------------------
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_multiaudio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode               = "copy"
language_whitelist = ["deu", "jpn"]
[audio.codec_rules]
ac3 = { mode = "encode", codec = "aac", bitrate = "128k" }
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "whitelist+rule: no output"
assert_audio_track_count "$O/test.mkv" 2
assert_audio_codec       "$O/test.mkv" 0 aac
assert_audio_codec       "$O/test.mkv" 1 aac

# -- no audio source: must not crash, output has 0 audio tracks ---------------
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_noaudio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "no-audio: no output"
assert_audio_track_count "$O/test.mkv" 0

# -- 7.1 copy: channel layout preserved ---------------------------------------
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_71audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "7.1 copy: no output"
assert_audio_track_count "$O/test.mkv" 1
assert_audio_channels    "$O/test.mkv" 0 8
assert_audio_codec       "$O/test.mkv" 0 flac

# -- 7.1 encode to opus: aformat filter preserves 7.1 layout -------------------
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_71audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = "256k"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "7.1 encode: no output"
assert_audio_track_count "$O/test.mkv" 1
assert_audio_channels    "$O/test.mkv" 0 8
assert_audio_codec       "$O/test.mkv" 0 opus

# -- whitelist no match: all tracks filtered, audio omitted -------------------
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode               = "copy"
language_whitelist = ["fra"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "whitelist no match: no output"
assert_audio_track_count "$O/test.mkv" 0
assert_log_contains      "audio omitted"

# -- untagged audio track: no language tag, always kept by whitelist ----------
I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_71audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode               = "copy"
language_whitelist = ["fra"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "untagged track: no output"
assert_audio_track_count "$O/test.mkv" 1
assert_audio_channels    "$O/test.mkv" 0 8
assert_audio_codec       "$O/test.mkv" 0 flac

# -- untagged in MP4: ffprobe reports "und", which still means untagged -------
# Reading only Matroska's missing field as untagged leaves an MP4 rip with no audio.
I="$WORKDIR/20/in"; O="$WORKDIR/20/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_untagged.mp4" "$I/p/test.mp4"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode               = "copy"
language_whitelist = ["fra"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "und track: no output"
assert_audio_track_count "$O/test.mkv" 1

# -- lossless override: flac source to flac, no bitrate, options applied --------
I="$WORKDIR/11/in"; O="$WORKDIR/11/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_71audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = "128k"
[audio.lossless]
codec   = "flac"
options = { compression_level = 12 }
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "lossless override: no output"
assert_audio_track_count "$O/test.mkv" 1
assert_audio_channels    "$O/test.mkv" 0 8
assert_audio_codec       "$O/test.mkv" 0 flac
assert_audio_title       "$O/test.mkv" 0 "FLAC"   # untitled source to marker only

# -- lossy default + per-layout bitrate: lossy tracks to opus -------------------
I="$WORKDIR/12/in"; O="$WORKDIR/12/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_multiaudio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = { mono = "64k", stereo = "128k", default = "96k" }
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "per-layout: no output"
assert_audio_track_count "$O/test.mkv" 3
assert_audio_codec       "$O/test.mkv" 0 opus
assert_audio_codec       "$O/test.mkv" 1 opus
assert_audio_codec       "$O/test.mkv" 2 opus
assert_audio_title       "$O/test.mkv" 0 "Opus"   # untitled source to marker only

# -- codec_rules beats lossless detection; per-layout 7.1 to opus --------------
I="$WORKDIR/13/in"; O="$WORKDIR/13/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_71audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = "128k"
[audio.lossless]
codec   = "flac"
[audio.codec_rules]
flac = { mode = "encode", codec = "libopus", bitrate = { "7.1" = "512k", default = "128k" } }
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "rule beats lossless: no output"
assert_audio_track_count "$O/test.mkv" 1
assert_audio_channels    "$O/test.mkv" 0 8
assert_audio_codec       "$O/test.mkv" 0 opus

# -- track title: copy keeps the source name unchanged ------------------------
I="$WORKDIR/14/in"; O="$WORKDIR/14/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_named_audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "title copy: no output"
assert_audio_codec "$O/test.mkv" 0 ac3
assert_audio_title "$O/test.mkv" 0 "Deutsch Dolby Digital 5.1"

# -- track title: encode appends the target codec marker ----------------------
I="$WORKDIR/15/in"; O="$WORKDIR/15/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_named_audio.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = { "5.1" = "320k", default = "192k" }
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "title encode: no output"
assert_audio_codec    "$O/test.mkv" 0 opus
assert_audio_channels "$O/test.mkv" 0 6
assert_audio_title    "$O/test.mkv" 0 "Deutsch Dolby Digital 5.1 (Opus)"

test_done
