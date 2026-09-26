#!/bin/sh
# Tests for audio.rs: track selection, codec rules, language whitelist, channel preservation.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# copy: codecs and track count unchanged
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

# global encode: all tracks transcoded
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
[ "$(packet_hashes "$O/test.mkv" a:0)" != "$(packet_hashes "$FIXTURES_DIR/sdr_multiaudio.mkv" a:0)" ] || \
    fail "global encode: audio 0 came through as a copy"
assert_log_contains "track 0: eng aac mono (lossy) -> AAC 96k"
assert_log_contains "track 1: deu ac3 mono (lossy) -> AAC 96k"

# codec_rule: ac3 to aac, others copied
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
assert_packets_identical "$O/test.mkv" a:0 "$FIXTURES_DIR/sdr_multiaudio.mkv" a:0
assert_packets_identical "$O/test.mkv" a:2 "$FIXTURES_DIR/sdr_multiaudio.mkv" a:2
assert_log_contains "track 0: eng aac mono (lossy) -> copy"
assert_log_contains "track 1: deu ac3 mono (lossy) -> AAC 128k"
assert_log_contains "track 2: jpn aac mono (lossy) -> copy"

# language_whitelist: only deu track kept
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
assert_channel_frequencies "$O/test.mkv" 0 880

# whitelist + codec_rule combined
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
assert_channel_frequencies "$O/test.mkv" 0 880
assert_channel_frequencies "$O/test.mkv" 1 220

# no audio source: must not crash, output has 0 audio tracks
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

# 7.1 copy: channel layout preserved
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

# 7.1 encode to opus: aformat filter preserves 7.1 layout
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

# whitelist no match: all tracks filtered, audio omitted
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

# untagged audio track: no language tag, always kept by whitelist
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

# untagged in MP4: ffprobe reports "und", which still means untagged
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

# lossless override: flac source to flac, no bitrate, options applied
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

# lossy default + per-layout bitrate: lossy tracks to opus
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
assert_log_contains "track 0: eng aac mono (lossy) -> Opus 64k"
assert_log_contains "track 2: jpn aac mono (lossy) -> Opus 64k"

# codec_rules beats lossless detection; per-layout 7.1 to opus
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
assert_log_contains      "Opus 512k"

# track title: copy keeps the source name unchanged
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

# track title: encode appends the target codec marker
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
assert_log_contains   "Opus 320k"
assert_audio_title    "$O/test.mkv" 0 "Deutsch Dolby Digital 5.1 (Opus)"

# audio.options reach ffmpeg: FLAC compression 0 against 12
for level in 0 12; do
    I="$WORKDIR/16-$level/in"; O="$WORKDIR/16-$level/out"; mkdir -p "$I/p" "$O"
    cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
    cat > "$I/p/encode.toml" << EOF
[avet]
video = "copy"
[audio]
mode    = "encode"
codec   = "flac"
options = { compression_level = $level }
EOF
    run_avet "$I" "$O" "$O/test.mkv" 180 || fail "flac level $level: no output"
    assert_audio_codec "$O/test.mkv" 0 flac
    assert_audio_samples_identical "$O/test.mkv" "$FIXTURES_DIR/pattern.mkv" 0
done
LOOSE=$(track_bytes "$WORKDIR/16-0/out/test.mkv"  a:0)
TIGHT=$(track_bytes "$WORKDIR/16-12/out/test.mkv" a:0)
[ "$TIGHT" -lt "$LOOSE" ] || \
    fail "audio.options: compression_level 12 wrote $TIGHT bytes, level 0 wrote $LOOSE"

# BCP 47 tags survive the tracks ffmpeg writes, which keep ISO 639-2 only
I="$WORKDIR/18/in"; O="$WORKDIR/18/out"; mkdir -p "$I/p" "$O"
mkvmerge -q -o "$I/p/test.mkv" --language 0:en-US --language 1:en-GB --language 2:de-CH \
    --language 3:en-US --language 4:de-AT --language 5:ja-JP "$FIXTURES_DIR/pattern.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = "128k"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "BCP 47: no output"
assert_bcp47_languages "$O/test.mkv" "en-US en-GB de-CH en-US de-AT ja-JP"

# a TS descriptor naming two languages: kept for either, written as the first
I="$WORKDIR/21/in"; O="$WORKDIR/21/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/lang_multi.ts" "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[audio]\nlanguage_whitelist = ["deu"]\n[subtitles]\nlanguage_whitelist = ["deu"]\n' \
    > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "TS languages: no output"
assert_audio_track_count    "$O/test.mkv" 1
assert_audio_language       "$O/test.mkv" 0 ger
assert_subtitle_track_count "$O/test.mkv" 1
assert_subtitle_language    "$O/test.mkv" 0 ger

# -- a video language mkvmerge does not know leaves the video untagged, not the job failed -
I="$WORKDIR/22/in"; O="$WORKDIR/22/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/lang_video.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "video language: no output"
assert_stream_value "$O/test.mkv" v:0 stream_tags=language -

# a codec ffmpeg cannot encode is transient: no marker, the file stays
I="$WORKDIR/17/in"; O="$WORKDIR/17/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "nonexistent"
bitrate = "128k"
EOF
run_avet_timed "$I" "$O" 60 "has no encoder"
assert_log_contains    "ffmpeg has no encoder 'nonexistent'"
assert_file_not_exists "$O/test.mkv"
assert_file_not_exists "$O/.avet_test/.failed"
assert_file_exists     "$I/p/test.mkv"

# an encoder ffmpeg refuses to open stops the job before the video is encoded
I="$WORKDIR/19/in"; O="$WORKDIR/19/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "opus"
bitrate = "128k"
EOF
run_avet_timed "$I" "$O" 60 "job failed"
assert_log_contains     "experimental codecs are not enabled"
assert_log_contains     "retrying on the next scan"
assert_log_not_contains "scene detection"
assert_file_not_exists  "$O/.avet_test/.failed"

I="$WORKDIR/23/in"; O="$WORKDIR/23/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "flac"
options = { compression_level = 13 }
EOF
run_avet_timed "$I" "$O" 60 "job failed"
assert_log_contains     "try the audio encoders on the first second"
assert_log_not_contains "scene detection"

test_done
