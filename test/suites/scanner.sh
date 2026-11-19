#!/bin/sh
# Tests for scanner.rs: profile discovery, skip logic, file extensions, env vars.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# no encode.toml: profile skipped with a hint, and so is a video outside any profile
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/loose.mkv"
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 20 "no jobs"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mkv"
assert_log_contains    "avet started version="
assert_log_contains    "no jobs"
assert_log_contains    "/input/p: it holds videos but no encode.toml"
assert_log_contains    "/input/loose.mkv: a video goes into a folder with an encode.toml"
assert_log_not_contains "ERROR"

# existing output: job skipped, source not moved
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
printf "sentinel" > "$O/test.mkv"
SENTINEL_SIZE=$(wc -c < "$O/test.mkv")
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 15 "skip: output exists"
CURRENT_SIZE=$(wc -c < "$O/test.mkv" 2>/dev/null || echo 0)
[ "$CURRENT_SIZE" = "$SENTINEL_SIZE" ] || fail "existing output was overwritten"
assert_file_exists     "$I/p/test.mkv"
assert_log_contains    "skip: output exists"

# processed/ dir is never scanned for new jobs
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"
mkdir -p "$I/processed" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/processed/test.mkv"
cat > "$I/processed/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 20 "no jobs"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "no jobs"

# POLL_INTERVAL env var is logged at startup
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I" "$O"
start_avet "$I" "$O" 42
wait_for_log "poll_s=42" 30
kill_avet
assert_log_contains "poll_s=42"

# .mp4 and .webm extensions both recognized by scanner
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/a.mp4"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/b.webm"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/b.mkv" 240 || fail "extensions: no output"
assert_file_nonempty "$O/a.mkv"
assert_file_nonempty "$O/b.mkv"
assert_file_exists   "$I/processed/a.mp4"
assert_file_exists   "$I/processed/b.webm"

# invalid POLL_INTERVAL: warning logged, default used
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I" "$O"
TEST_RUST_LOG=warn start_avet "$I" "$O" notanumber
wait_for_log "invalid value" 30
kill_avet
assert_log_contains "invalid value"

# two files sharing a stem: neither runs
# Same output, same temp dir, same name under processed/.
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/a" "$I/b" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/a/test.mkv"
cp "$FIXTURES_DIR/sdr_720p.mkv"   "$I/b/test.mkv"
for p in a b; do
cat > "$I/$p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
done
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 20 "no jobs"
assert_log_not_contains "encoding:"
assert_file_exists     "$I/a/test.mkv"
assert_file_exists     "$I/b/test.mkv"
assert_log_contains    "share this name"

# same stem, different extension, in one profile
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mp4"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 20 "no jobs"
assert_log_not_contains "encoding:"
assert_log_contains    "share this name"

# line breaks in folder and file names are encoded like any other name
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"
NL=$(printf 'Part\nTwo')
mkdir -p "$I/p/$NL" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/$NL/$NL.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/$NL/$NL.mkv" 120 || fail "line break in name: no output"
assert_video_codec       "$O/$NL/$NL.mkv" av1
assert_audio_track_count "$O/$NL/$NL.mkv" 1

# an encode.toml in a folder inside a profile is ignored with a warning
I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p/Show A" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Show A/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
printf 'encoder = "svt-av1"\n' > "$I/p/Show A/encode.toml"
run_avet "$I" "$O" "$O/Show A/test.mkv" 120 || fail "nested encode.toml: no output"
assert_log_contains "Show A/encode.toml: only the encode.toml of the profile folder applies"
assert_video_codec  "$O/Show A/test.mkv" h264

# LOG_NAME_LENGTH cuts the name in front of a job's log lines, the start line shows the full path
I="$WORKDIR/11/in"; O="$WORKDIR/11/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Dune.Part.Two.2024.2160p.UHD.BluRay.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
start_avet "$I" "$O" 2 -e LOG_NAME_LENGTH=10 "$TEST_IMAGE"
wait_for_file "$O/Dune.Part.Two.2024.2160p.UHD.BluRay.mkv" 120 || fail "LOG_NAME_LENGTH: no output"
kill_avet
assert_log_contains     "[Dune.Part.…] starting /input/p/Dune.Part.Two.2024.2160p.UHD.BluRay.mkv"
assert_log_not_contains "[Dune.Part.Two"

I="$WORKDIR/12/in"; O="$WORKDIR/12/out"; mkdir -p "$I" "$O"
start_avet "$I" "$O" 2 -e LOG_NAME_LENGTH=0 "$TEST_IMAGE"
wait_for_log 'LOG_NAME_LENGTH has invalid value "0" - logging full names' 30 || fail "LOG_NAME_LENGTH=0: no warning"
kill_avet

test_done
