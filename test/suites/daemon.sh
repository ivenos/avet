#!/bin/sh
# The scan loop itself: files that arrive later, a job that fails beside a good one, a
# copy still running, and a signalled stop. Every other suite runs avet exactly once.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# "no jobs" and the chunk lines are the only sign the loop came round again.
TEST_RUST_LOG=debug

PROFILE='encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n'

setup() { # NAME [PROFILE]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    printf '%b' "${2:-$PROFILE}" > "$I/p/encode.toml"
}

# -- a file dropped in after the first scan is encoded on a later one ---------------------
setup rescan
start_avet "$I" "$O" 2
wait_for_log "no jobs" 30 || fail "rescan: avet never reported an idle scan"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/first.mkv"
wait_for_file "$O/first.mkv" 180 || fail "rescan: the file added after the first scan was never encoded"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/second.mkv"
wait_for_file "$O/second.mkv" 180 || fail "rescan: the second file was never encoded"
wait_for_log "[second] done" 60 || fail "rescan: second never finished"
kill_avet
assert_video_codec "$O/first.mkv"  av1
assert_video_codec "$O/second.mkv" av1
assert_file_exists "$I/processed/first.mkv"
assert_file_exists "$I/processed/second.mkv"

# -- a job that fails does not take the rest of the scan with it -------------------------
setup failing
cp "$FIXTURES_DIR/audio_only.mkv" "$I/p/alpha.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/beta.mkv"
start_avet "$I" "$O" 2
wait_for_file "$O/beta.mkv" 240 || fail "failing job: the file queued behind it was never encoded"
wait_for_log "[beta] done" 60 || fail "failing job: beta never finished"
kill_avet
assert_log_contains     "job failed"
assert_file_not_exists  "$O/alpha.mkv"
assert_file_exists      "$O/.avet_alpha/.failed"
assert_file_exists      "$I/processed/beta.mkv"
assert_file_exists      "$I/p/alpha.mkv"

# -- a copy still running is waited out, and the whole file is encoded -------------------
setup growing 'encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 40\n'
start_avet "$I" "$O" 2
wait_for_log "no jobs" 30 || fail "growing: avet never reported an idle scan"
# Keeps growing until avet has said it noticed, so a descheduled container cannot miss
# the whole window and encode a file that was never growing for it.
(
    n=0
    while [ ! -f "$WORKDIR/growing.done" ] && [ "$n" -lt 90 ]; do
        head -c 30000 /dev/urandom >> "$I/p/test.mkv"
        n=$((n + 1))
        sleep 1
    done
    cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
) &
wait_for_log "still being written" 90 || fail "growing: avet did not wait for the copy"
touch "$WORKDIR/growing.done"
wait
wait_for_file "$O/test.mkv" 180 || fail "growing: never encoded after the copy finished"
kill_avet
# A fragment would encode and mux just as happily, only shorter.
assert_video_frames "$O/test.mkv" 240
assert_frames_match "$O/test.mkv" "$FIXTURES_DIR/pattern.mkv"

# -- SIGTERM mid-encode: the job is finished, then the loop ends -------------------------
setup sigterm 'encoder = "svt-av1"\n[encoder_params]\npreset = 4\ncrf = 40\n[scene_detection]\nextra_split = 24\n'
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
start_avet "$I" "$O" 2
wait_for_log "chunk 1/" 240 || fail "sigterm: no chunk was encoded before the signal"
stop_avet 300
[ "$AVET_RC" = 0 ] || fail "sigterm: avet exited with $AVET_RC, expected 0"
assert_log_contains     "signal received"
assert_log_contains     "stopping after test"
# A marker here would lock out a file nothing is wrong with.
assert_log_not_contains "job failed"
assert_dir_not_exists   "$O/.avet_test"
assert_frames_match     "$O/test.mkv" "$FIXTURES_DIR/pattern.mkv"
assert_decodes_cleanly  "$O/test.mkv"

# -- a second signal stops it there and then ---------------------------------------------
setup twice 'encoder = "svt-av1"\n[encoder_params]\npreset = 4\ncrf = 40\n[scene_detection]\nextra_split = 24\n'
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
start_avet "$I" "$O" 2
wait_for_log "chunk 1/" 240 || fail "twice: no chunk was encoded before the signal"
docker kill --signal=TERM "$AVET_CID" >/dev/null 2>&1
wait_for_log "signal received" 30 || fail "twice: the first signal was not logged"
stop_avet 60
[ "$AVET_RC" = 130 ] || fail "twice: expected exit 130 after the second signal, got $AVET_RC"
assert_log_contains    "second signal"
assert_file_not_exists "$O/test.mkv"

# -- SIGTERM while idle: the poll sleep ends with it, it is not slept out -----------------
setup idle
start_avet "$I" "$O" 120
wait_for_log "no jobs" 30 || fail "idle: avet never reported an idle scan"
stop_avet 20
[ "$AVET_RC" = 0 ] || fail "idle: avet did not stop within 20s of SIGTERM (rc $AVET_RC)"
assert_log_contains "signal received"

test_done
