#!/bin/sh
# Tests for crop.rs: cropdetect, cache, crop+scale interaction.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# -- letterboxed source: height reduced after crop, crop.cache written --------
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_blackbars.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
crop      = true
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "crop: no output"
assert_video_height_lt "$O/test.mkv" 480
assert_log_contains    "auto-crop"
assert_file_exists     "$O/.avet_test/crop.cache"
[ -s "$O/.avet_test/crop.cache" ] || fail "crop.cache is empty after detection"

# -- clean source: cropdetect finds no bars, height unchanged -----------------
# 360 is not a multiple of 16: rounding the box to 16 reports 640x352 for a frame with no
# bars, and that passes the "did this change anything" check as a real crop.
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
crop = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "no-crop: no output"
assert_video_height  "$O/test.mkv" 360
assert_log_contains  "no black bars"

# -- crop cache hit: second run uses cached result -----------------------------
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_blackbars.mkv" "$I/p/test.mkv"
printf 'crop=640:360:0:60' > "$O/.avet_test/crop.cache"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
crop      = true
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "crop cache: no output"
assert_log_contains "(cached)"
# The cached value has to reach the encode, not just the log line.
assert_video_height "$O/test.mkv" 360

# -- crop + scale: crop runs first, the scale target applies to what is left ---
# <=, not ==: what has to hold is "no taller than asked for", whatever cropdetect reports.
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_blackbars.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
crop  = true
scale = 240
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "crop+scale: no output"
assert_video_height_le "$O/test.mkv" 240

# -- empty cache to "no black bars (cached)" -----------------------------------
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '' > "$O/.avet_test/crop.cache"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
crop = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "empty cache: no output"
assert_log_contains "no black bars (cached)"

test_done
