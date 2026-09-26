#!/bin/sh
# Tests for scene.rs: chunk splitting, speed, downscale, scenes.json reuse.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# extra_split_sec=5 on 60s source: produces more chunks than default
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_long.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
[scene_detection]
extra_split_sec = 5
EOF
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "extra_split: no output"
CHUNK_COUNT=$(grep -c '"index"' "$O/.avet_test/scenes.json" 2>/dev/null) || CHUNK_COUNT=0
[ "$CHUNK_COUNT" -gt 6 ] || fail "extra_split: expected >6 chunks, got $CHUNK_COUNT"

# speed=fast: still detects and covers the whole source
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
[scene_detection]
speed = "fast"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "fast speed: no output"
assert_file_nonempty "$O/test.mkv"
assert_video_height  "$O/test.mkv" 360
assert_scenes_cover  "$O/.avet_test/scenes.json"
assert_video_frames  "$O/test.mkv" 240

# downscale_height: affects detection only, output resolution unchanged
# Detection runs on a 180p copy; the chunk boundaries still have to be frame numbers
# in the source, so the scene list must cover the source's own frame count.
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_720p.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
[scene_detection]
downscale_height = 180
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "downscale: no output"
assert_video_height "$O/test.mkv" 720
assert_scenes_cover "$O/.avet_test/scenes.json"
assert_video_frames "$O/test.mkv" 240

# pre-created scenes.json is reused, detection skipped
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[{"index":0,"start_frame":0,"end_frame":239}]\n' > "$O/.avet_test/scenes.json"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "scenes reuse: no output"
assert_log_contains "reusing scenes.json"

# a scene list short of the source is extended, a longer one clamped
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"; mkdir -p "$I/p" "$O/.avet_test"
# pattern.mkv, not a flat color: the 40 frames the extension adds have to be the real
# tail, and every frame of a constant fixture matches every other.
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
printf '[{"index":0,"start_frame":0,"end_frame":199}]\n' > "$O/.avet_test/scenes.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 40\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "short scene list: no output"
assert_log_contains "extending the last chunk"
assert_video_frames "$O/test.mkv" 240
assert_frames_match "$O/test.mkv" "$FIXTURES_DIR/pattern.mkv"

I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[{"index":0,"start_frame":0,"end_frame":299}]\n' > "$O/.avet_test/scenes.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "long scene list: no output"
assert_log_contains "clamping scene"
assert_video_frames "$O/test.mkv" 240

# a gap in the scene list is refused, not encoded around
# The frame count the output is checked against is summed from this same list.
I="$WORKDIR/11/in"; O="$WORKDIR/11/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[{"index":0,"start_frame":0,"end_frame":99},{"index":1,"start_frame":120,"end_frame":239}]\n' \
    > "$O/.avet_test/scenes.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 90 "job failed"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "gap or an overlap"
assert_file_exists     "$O/.avet_test/.failed"

# an empty scenes.json is a leftover, not an empty scene list
# Parsing it fails on every retry, and the fingerprint still matches, so nothing clears it.
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
: > "$O/.avet_test/scenes.json"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "empty scenes.json: no output"
assert_log_contains "scene detection"
assert_video_frames "$O/test.mkv" 240

# min_scene_len reaches the detector: no chunk shorter than it
# pattern.mkv changes completely from frame to frame: every frame is a cut candidate.
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
[scene_detection]
min_scene_len   = 60
extra_split     = 0
extra_split_sec = 0
EOF
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "min_scene_len: no output"
assert_scenes_cover       "$O/.avet_test/scenes.json"
assert_min_chunk_frames   "$O/.avet_test/scenes.json" 60
assert_video_frames       "$O/test.mkv" 240
assert_keyframes_at_chunks "$O/test.mkv" "$O/.avet_test/scenes.json"

# a recording cut mid-GOP: every chunk still starts on its scene change
I="$WORKDIR/12/in"; O="$WORKDIR/12/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/cut_gop.ts" "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\nkeep_temp = true\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "cut recording: no output"
assert_log_contains "scene cuts move by"
assert_scenes_cover "$O/.avet_test/scenes.json"
changes=$(ffmpeg -hide_banner -i "$O/test.mkv" -vf "setpts=N/TB,select=gt(scene\,0.3),showinfo" -f null - 2>&1 \
    | sed -n 's/.*pts_time:\([0-9]*\).*/\1/p' | tr '\n' ' ')
starts=$(sed -n 's/.*"start_frame": *\([0-9]*\).*/\1/p' "$O/.avet_test/scenes.json" | tr '\n' ' ')
[ -n "$changes" ] && [ "0 $changes" = "$starts" ] \
    || fail "cut recording: scene changes at frames '$changes', chunks start at '$starts'"

# extra_split_sec = 0 disables the extra splitting
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_long.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
[scene_detection]
extra_split_sec = 0
EOF
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "extra_split_sec=0: no output"
CHUNK_COUNT=$(grep -c '"index"' "$O/.avet_test/scenes.json" 2>/dev/null) || CHUNK_COUNT=0
[ "$CHUNK_COUNT" -ge 1 ] && [ "$CHUNK_COUNT" -le 3 ] || \
    fail "extra_split_sec=0: expected 1 to 3 chunks, got $CHUNK_COUNT"
assert_scenes_cover "$O/.avet_test/scenes.json"
assert_video_frames "$O/test.mkv" 1440

# extra_split in frames overrides extra_split_sec when set
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_long.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
[scene_detection]
extra_split     = 120
extra_split_sec = 999
EOF
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "extra_split frames: no output"
CHUNK_COUNT=$(grep -c '"index"' "$O/.avet_test/scenes.json" 2>/dev/null) || CHUNK_COUNT=0
[ "$CHUNK_COUNT" -gt 6 ] || fail "extra_split frames: expected >6 chunks, got $CHUNK_COUNT"
# The remainder of a split scene used to land in the last part, over the limit.
assert_max_chunk_frames "$O/.avet_test/scenes.json" 120

test_done
