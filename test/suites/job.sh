#!/bin/sh
# Tests for job.rs: full encode pipeline, lifecycle, scaling, resume, failure handling.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# -- baseline: output created, source in processed/, temp dir removed ----------
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "baseline: no output"
assert_file_nonempty   "$O/test.mkv"
assert_file_exists     "$I/processed/test.mkv"
assert_file_not_exists "$I/p/test.mkv"
assert_dir_not_exists  "$O/.avet_test"

# -- keep_temp=true: temp dir preserved ---------------------------------------
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "keep_temp=true: no output"
assert_dir_exists "$O/.avet_test"

# -- scale down: 720p to 360p ---------------------------------------------------
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_720p.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 360
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "scale down: no output"
assert_video_height  "$O/test.mkv" 360
assert_log_contains  "auto-scale"
assert_log_contains  "workers:"

# -- scale noop: source smaller than target, no scaling applied ---------------
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 720
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "scale noop: no output"
assert_video_height "$O/test.mkv" 360

# -- resume: pre-created frame index is reused ---------------------------------
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
EOF
docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "${I}:/input:z" \
    -v "${O}:/output:z" \
    --entrypoint ffmsindex \
    "${TEST_IMAGE:-avet:test}" \
    /input/p/test.mkv /output/.avet_test/frame-index.ffindex
[ -f "$O/.avet_test/frame-index.ffindex" ] || fail "resume: ffmsindex produced no index"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "resume: no output"
assert_log_contains "reusing existing index"

# -- multi-file: two videos in same profile, both encoded and moved ------------
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/alpha.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/beta.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/beta.mkv" 240 || fail "multi-file: no output"
assert_file_nonempty "$O/alpha.mkv"
assert_file_nonempty "$O/beta.mkv"
assert_file_exists   "$I/processed/alpha.mkv"
assert_file_exists   "$I/processed/beta.mkv"

# -- multiple profiles: two profile dirs, independent configs ------------------
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"
mkdir -p "$I/movies" "$I/series" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv"  "$I/movies/alpha.mkv"
cp "$FIXTURES_DIR/sdr_720p.mkv"    "$I/series/beta.mkv"
cat > "$I/movies/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
cat > "$I/series/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 360
EOF
run_avet "$I" "$O" "$O/beta.mkv" 240 || fail "multi-profile: no beta output"
assert_file_nonempty "$O/alpha.mkv"
assert_file_nonempty "$O/beta.mkv"
assert_video_height  "$O/beta.mkv" 360
assert_file_exists   "$I/processed/alpha.mkv"
assert_file_exists   "$I/processed/beta.mkv"

# -- .failed workflow: write marker, block retry, recover after fix ------------
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv5.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet_timed "$I" "$O" 60 "job failed"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$O/.avet_test/.failed"
assert_log_contains    "Set avet.dv = true"

run_avet_timed "$I" "$O" 15 "permanently failed"
assert_log_contains "permanently failed"

rm -f "$O/.avet_test/.failed"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
dv = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail ".failed recovery: no output"
assert_file_nonempty "$O/test.mkv"
assert_file_exists   "$I/processed/test.mkv"

# -- a folder in a profile comes out as the same folder -----------------------
I="$WORKDIR/11/in"; O="$WORKDIR/11/out"
mkdir -p "$I/p/Show/Season 1" "$I/p/Show/Season 2" "$I/p/Show/Specials" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Show/Season 1/Episode 01.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Show/Season 2/Episode 01.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Show/Specials/Special.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/Show/Specials/Special.mkv" 360 || fail "folder: no output"
assert_file_nonempty   "$O/Show/Season 1/Episode 01.mkv"
assert_file_nonempty   "$O/Show/Season 2/Episode 01.mkv"
assert_file_exists     "$I/processed/Show/Season 1/Episode 01.mkv"
assert_file_exists     "$I/processed/Show/Season 2/Episode 01.mkv"
assert_file_exists     "$I/processed/Show/Specials/Special.mkv"
assert_dir_not_exists  "$I/p/Show"
assert_dir_exists      "$I/p"
assert_dir_not_exists  "$O/Show/Season 1/.avet_Episode 01"
assert_file_not_exists "$O/Episode 01.mkv"
assert_log_not_contains "share this name"

# -- done.json resume: second run skips already-encoded chunks ----------------
I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "done resume: first encode failed"
cp "$I/processed/test.mkv" "$I/p/test.mkv"
rm  "$O/test.mkv"
TEST_RUST_LOG=debug run_avet "$I" "$O" "$O/test.mkv" 90 || fail "done resume: second encode failed"
assert_log_contains  "already done"
assert_file_nonempty "$O/test.mkv"

# -- edge cases: a single picture, a name at the 255-byte limit, a file without video ------------
I="$WORKDIR/12/in"; O="$WORKDIR/12/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/one_frame.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "one frame: no output"
assert_video_frames "$O/test.mkv" 1
assert_frames_match "$O/test.mkv" "$FIXTURES_DIR/one_frame.mkv"
assert_audio_samples_identical "$O/test.mkv" "$FIXTURES_DIR/one_frame.mkv" 0

I="$WORKDIR/13/in"; O="$WORKDIR/13/out"; mkdir -p "$I/p" "$O"
long=$(printf '%0250d' 0 | tr 0 x)
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/$long.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/$long.mkv" 120 || fail "long name: no output"
assert_file_exists "$I/processed/$long.mkv"

I="$WORKDIR/14/in"; O="$WORKDIR/14/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/audio_only.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 60 "job failed"
assert_file_not_exists "$O/test.mkv"
grep -q "no video track" "$O/.avet_test/.failed" 2>/dev/null || fail "audio only: .failed does not name the missing video"

# -- an empty output file is a leftover, not a finished encode ----------------
I="$WORKDIR/15/in"; O="$WORKDIR/15/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
: > "$O/test.mkv"
run_avet_timed "$I" "$O" 180 "[test] done"
assert_log_contains  "ignoring empty output file"
assert_video_frames  "$O/test.mkv" 240
assert_file_exists   "$I/processed/test.mkv"

test_done
