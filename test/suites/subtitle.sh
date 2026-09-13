#!/bin/sh
# Tests for subtitle.rs: track selection, strip mode, language whitelist.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# -- copy: all subtitle tracks preserved --------------------------------------
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
mode = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "copy: no output"
assert_subtitle_track_count "$O/test.mkv" 2

# -- strip: no subtitle tracks in output ---------------------------------------
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
mode = "strip"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "strip: no output"
assert_subtitle_track_count "$O/test.mkv" 0

# -- language_whitelist: only matching track kept ------------------------------
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
language_whitelist = ["deu"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "whitelist: no output"
assert_subtitle_track_count "$O/test.mkv" 1
# Counting alone would pass with the English track kept. mkvmerge writes the
# bibliographic form, so "deu" comes back out as "ger".
assert_subtitle_language    "$O/test.mkv" 0 ger

# -- whitelist written in the other ISO 639-2 spelling still matches ----------
# The everyday case: feeding an avet output back into the profile that produced it.
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
ffmpeg -y -hide_banner -loglevel error -i "$FIXTURES_DIR/sdr_subtitles.mkv" \
    -map 0 -c copy -metadata:s:s:1 language=ger "$I/p/test.mkv" \
    || fail "could not build ger-tagged fixture"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
language_whitelist = ["deu"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "iso alias: no output"
assert_subtitle_track_count "$O/test.mkv" 1

# -- source without subtitles + copy: 0 tracks, no error ----------------------
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
mode = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "no-sub source: no output"
assert_subtitle_track_count "$O/test.mkv" 0

# -- whitelist with no matching language: 0 subtitle tracks -------------------
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
language_whitelist = ["fra"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "sub whitelist no match: no output"
assert_subtitle_track_count "$O/test.mkv" 0

# -- default (no [subtitles] section): all subtitle tracks preserved -----------
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "sub default: no output"
assert_subtitle_track_count "$O/test.mkv" 2

test_done
