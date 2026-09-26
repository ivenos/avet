#!/bin/sh
# Tests for crop.rs: cropdetect, cache, crop+scale interaction.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# letterboxed source: height reduced after crop, crop.cache written
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
assert_video_height    "$O/test.mkv" 360
assert_log_contains    "auto-crop"
assert_file_exists     "$O/.avet_test/crop.cache"
[ -s "$O/.avet_test/crop.cache" ] || fail "crop.cache is empty after detection"

# clean source: cropdetect finds no bars, height unchanged
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

# crop cache hit: second run uses cached result
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_blackbars.mkv" "$I/p/test.mkv"
# Not the box cropdetect finds: with that one the assertion below passes either way.
printf 'crop=640:240:0:120' > "$O/.avet_test/crop.cache"
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
assert_video_height "$O/test.mkv" 240

# crop + scale: crop runs first, the scale target applies to what is left
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
# Uncropped, 640x480 would scale to 320x240 and pass a height check just as well.
assert_stream_value "$O/test.mkv" v:0 stream=width,height "426 240"

# empty cache to "no black bars (cached)"
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

# rotated source: the bars sit in storage orientation, not in the displayed one
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/pattern_bars_rot90.mp4" "$I/p/test.mp4"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
crop      = true
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "rotated crop: no output"
assert_video_height "$O/test.mkv" 276
grep -qx "crop=640:276:0:44" "$O/.avet_test/crop.cache" ||
    fail "rotated crop: cropdetect box is '$(cat "$O/.avet_test/crop.cache")'"

# an all-dark scene boxes the only lit part, which is no crop
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/pattern_dark.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
crop      = true
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "implausible crop: no output"
assert_log_contains    "ignoring implausible"
assert_video_height    "$O/test.mkv" 180
assert_stream_value    "$O/test.mkv" v:0 stream=width,height "320 180"
assert_file_not_exists "$O/.avet_test/crop.cache"

test_done
