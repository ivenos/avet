#!/bin/sh
# The scan loop itself: files that arrive later, a job that fails beside a good one, a
# copy still running, a signaled stop, a crash and a killed encoder.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# "no jobs" and the chunk lines are the only sign the loop came round again.
TEST_RUST_LOG=debug

PROFILE='encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n'

setup() { # NAME [PROFILE]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    printf '%b' "${2:-$PROFILE}" > "$I/p/encode.toml"
}

# a file dropped in after the first scan is encoded on a later one
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

# a job that fails does not take the rest of the scan with it
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

# a copy still running is waited out, and the whole file is encoded
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

# a signal while a copy is still running starts no encode
setup waiting
start_avet "$I" "$O" 2
wait_for_log "no jobs" 30 || fail "waiting: avet never reported an idle scan"
(
    n=0
    while [ ! -f "$WORKDIR/waiting.done" ] && [ "$n" -lt 90 ]; do
        head -c 30000 /dev/urandom >> "$I/p/test.mkv"
        n=$((n + 1))
        sleep 1
    done
) &
wait_for_log "still being written" 90 || fail "waiting: avet did not wait for the copy"
stop_avet 20
touch "$WORKDIR/waiting.done"
wait
[ "$AVET_RC" = 0 ] || fail "waiting: avet exited with $AVET_RC while the copy still grew, expected 0"
assert_log_contains   "not starting, avet is stopping"
assert_dir_not_exists "$O/.avet_test"

# SIGTERM mid-encode: the job is finished, then the loop ends
setup sigterm 'encoder = "svt-av1"\n[encoder_params]\npreset = 4\ncrf = 40\n[scene_detection]\nextra_split = 24\n'
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
TEST_CPUS=0.5 start_avet "$I" "$O" 2
wait_for_log "chunk 1/" 240 || fail "sigterm: no chunk was encoded before the signal"
stop_avet 300
[ "$AVET_RC" = 0 ] || fail "sigterm: avet exited with $AVET_RC, expected 0"
signal_line=$(printf '%s\n' "$RUN_LOGS" | grep -n "signal received" | head -n 1 | cut -d: -f1)
merge_line=$(printf '%s\n' "$RUN_LOGS" | grep -n "merging chunks" | head -n 1 | cut -d: -f1)
[ "${signal_line:-999999}" -lt "${merge_line:-0}" ] || fail "sigterm: the signal came after the last chunk, not mid-encode"
assert_log_contains     "stopping after test"
# A marker here would lock out a file nothing is wrong with.
assert_log_not_contains "job failed"
assert_dir_not_exists   "$O/.avet_test"
assert_frames_match     "$O/test.mkv" "$FIXTURES_DIR/pattern.mkv"
assert_decodes_cleanly  "$O/test.mkv"
[ -n "$(log_capture 's/.*\] chunk 1\/[0-9]* - .* MB - \([0-9hms]*\) left$/\1/p')" ] || \
    fail "sigterm: the first chunk line tells no time left"

# avet dying mid-job three times in a row locks the file out; a hangup it was told about is no such death
setup crashes 'encoder = "svt-av1"\n[encoder_params]\npreset = 4\ncrf = 40\n[scene_detection]\nextra_split = 24\n'
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
crash() { # what the OOM killer or an abort in FFMS2 leaves: no signal avet could act on
    TEST_CPUS=0.5 start_avet "$I" "$O" 2
    wait_for_log "] encoding:" 240 || fail "crashes: the job did not get to the encode"
    kill_avet
}
crash
crash
TEST_CPUS=0.5 start_avet "$I" "$O" 2
wait_for_log "] encoding:" 240 || fail "crashes: the job did not get to the encode before the stop"
docker kill --signal=HUP "$AVET_CID" >/dev/null 2>&1
wait_for_log "signal received" 30 || fail "crashes: the hangup was not logged"
kill_avet
# Had the stop counted, this start would be refused before the encode.
crash
assert_file_not_exists "$O/.avet_test/.failed"
run_avet_timed "$I" "$O" 60 "job failed"
assert_log_contains    "stopped 3 times in a row"
assert_file_exists     "$O/.avet_test/.failed"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mkv"

# an encoder killed three times in a row, as the out-of-memory killer does, locks the file out as well
setup killed 'encoder = "svt-av1"\n[encoder_params]\npreset = 4\ncrf = 40\n[scene_detection]\nextra_split = 24\n'
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
TEST_CPUS=0.5 start_avet "$I" "$O" 1
kills=0; waited=0
while [ ! -f "$O/.avet_test/.failed" ] && [ "$waited" -lt 240 ]; do
    docker exec "$AVET_CID" pkill -KILL -f SvtAv1EncApp 2>/dev/null && kills=$((kills + 1))
    sleep 1
    waited=$((waited + 1))
done
kill_avet
[ "$kills" -ge 3 ] || fail "killed: only $kills kills reached an encoder"
assert_log_contains    "stopped 3 times in a row"
assert_file_exists     "$O/.avet_test/.failed"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mkv"

# a second signal stops it there and then
setup twice 'encoder = "svt-av1"\n[encoder_params]\npreset = 4\ncrf = 40\n[scene_detection]\nextra_split = 24\n'
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
TEST_CPUS=0.5 start_avet "$I" "$O" 2
wait_for_log "chunk 1/" 240 || fail "twice: no chunk was encoded before the signal"
docker kill --signal=TERM "$AVET_CID" >/dev/null 2>&1
wait_for_log "signal received" 30 || fail "twice: the first signal was not logged"
stop_avet 60
[ "$AVET_RC" = 130 ] || fail "twice: expected exit 130 after the second signal, got $AVET_RC"
assert_log_contains    "second signal"
assert_file_not_exists "$O/test.mkv"

# a second avet on the same folders waits, takes over once the first stops, and stops on a signal
setup twin
start_avet "$I" "$O" 2
FIRST=$AVET_CID
wait_for_log "no jobs" 30 || fail "twin: the first avet never reported an idle scan"
start_avet "$I" "$O" 2
wait_for_log "another avet works on /output" 30 || fail "twin: the second avet did not wait"
assert_log_not_contains "no jobs"
SECOND=$AVET_CID
AVET_CID=$FIRST
kill_avet
AVET_CID=$SECOND
wait_for_log "no jobs" 30 || fail "twin: the second avet did not take over"
stop_avet 20
[ "$AVET_RC" = 0 ] || fail "twin: the second avet exited with $AVET_RC"

# a failure that clears on its own waits twice as long each time, not just for the next scan
setup backoff 'encoder = "nope"\n'
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
started=$(date +%s)
start_avet "$I" "$O" 1
wait_for_log "retrying in 16s" 60 || fail "backoff: no fifth attempt within 60 s"
elapsed=$(( $(date +%s) - started ))
kill_avet
[ "$elapsed" -ge 12 ] || fail "backoff: five attempts within ${elapsed}s at a 1 s poll"
assert_log_not_contains "clears on its own"

# an encoder that cannot be started is retried like any failure that clears on its own, never locked out
setup unstartable
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'not a program\n' > "$WORKDIR/SvtAv1EncApp"
chmod 644 "$WORKDIR/SvtAv1EncApp"
start_avet "$I" "$O" 1 -v "$WORKDIR/SvtAv1EncApp:/usr/local/bin/SvtAv1EncApp:ro,z" "$TEST_IMAGE"
wait_for_log "retrying in 16s" 120 || fail "unstartable: no fifth attempt within 120 s"
kill_avet
assert_log_contains     "start encoder"
assert_log_not_contains "stopped 3 times in a row"
assert_file_not_exists  "$O/.avet_test/.failed"

# a full output disk is waited out
setup full 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 5\n'
start_avet "$I" "$O" 5 --tmpfs /full:size=8m,mode=1777 -e OUTPUT_DIR=/full "$TEST_IMAGE"
wait_for_log "no jobs" 30 || fail "full: avet never reported an idle scan"
docker exec "$AVET_CID" sh -c 'dd if=/dev/zero of=/full/filler bs=1024 count=$(( $(df -k /full | awk "NR == 2 { print \$4 }") - 64 )) 2>/dev/null'
cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
wait_for_log "is the disk full?" 120 || fail "full: no chunk ran out of space"
docker exec "$AVET_CID" rm /full/filler
wait_for_log "[test] done" 120 || fail "full: the job did not finish once there was space"
docker exec "$AVET_CID" cat /full/test.mkv > "$O/test.mkv"
kill_avet
assert_log_not_contains "could not write failure marker"
assert_log_not_contains "source kept"
assert_video_frames     "$O/test.mkv" 240
assert_file_exists      "$I/processed/test.mkv"

# started with nohup, a hangup leaves avet running
setup nohup
start_avet "$I" "$O" 2 --entrypoint nohup "$TEST_IMAGE" avet
wait_for_log "no jobs" 30 || fail "nohup: avet never reported an idle scan"
docker kill --signal=HUP "$AVET_CID" >/dev/null 2>&1
sleep 5
[ "$(docker inspect -f '{{.State.Running}}' "$AVET_CID" 2>/dev/null)" = true ] || fail "nohup: a hangup stopped avet"
stop_avet 20
[ "$AVET_RC" = 0 ] || fail "nohup: avet did not stop on SIGTERM (rc $AVET_RC)"

# avet refuses at the start what would run it on the wrong folders
start_once() { # DOCKER_ARG... IMAGE [ARG...]: until avet exits on its own, at most 10 s
    start_avet "$I" "$O" 2 "$@"
    waited=0
    while [ "$(docker inspect -f '{{.State.Running}}' "$AVET_CID" 2>/dev/null)" = true ] && [ "$waited" -lt 10 ]; do
        sleep 1
        waited=$((waited + 1))
    done
    stop_avet 20
}
setup nested
start_once -e OUTPUT_DIR=/input/p/out "$TEST_IMAGE"
[ "$AVET_RC" != 0 ] && [ "$AVET_RC" != 124 ] || fail "nested: avet started anyway (rc $AVET_RC)"
assert_log_contains "is inside INPUT_DIR"
[ -e "$I/p/out" ] && fail "nested: the refused output folder was created"
start_once "$TEST_IMAGE" --help-me
[ "$AVET_RC" != 0 ] && [ "$AVET_RC" != 124 ] || fail "argument: avet started anyway (rc $AVET_RC)"
assert_log_contains "takes no argument other than --version or --help, got: --help-me"
start_once "$TEST_IMAGE" --version --version
[ "$AVET_RC" != 0 ] && [ "$AVET_RC" != 124 ] || fail "two arguments: avet took them (rc $AVET_RC)"
start_once "$TEST_IMAGE" --help
[ "$AVET_RC" = 0 ] || fail "--help: exited with $AVET_RC"
assert_log_contains "usage: avet [--version | --help]"

# SIGTERM while idle: the poll sleep ends with it, it is not slept out
setup idle
start_avet "$I" "$O" 120
wait_for_log "no jobs" 30 || fail "idle: avet never reported an idle scan"
stop_avet 20
[ "$AVET_RC" = 0 ] || fail "idle: avet did not stop within 20s of SIGTERM (rc $AVET_RC)"
assert_log_contains "signal received"

# a broken file named like a full disk is failed, not waited out
setup 500 '[avet]\nvideo = "copy"\n'
head -c 4096 /dev/urandom > "$I/p/No space left on device.mkv"
start_avet "$I" "$O" 1
wait_for_file "$O/.avet_No space left on device/.failed" 60 || fail "disk full name: the broken file got no .failed"
kill_avet
assert_log_not_contains "retrying"

# ffmpeg answers SIGTERM with exit code 255: retried, and three in a row lock the file out
stopped_ffmpeg() { # NAME TIMES
    setup "$1" '[avet]\nvideo = "copy"\n[audio]\nmode = "encode"\ncodec = "flac"\n'
    cp "$FIXTURES_DIR/pattern.mkv" "$I/p/test.mkv"
    mkdir -p "$WORKDIR/$1/wrap"
    echo "$2" > "$WORKDIR/$1/wrap/left"
    cat > "$WORKDIR/$1/wrap/ffmpeg" << 'EOF'
#!/bin/sh
left=$(cat /wrap/left)
case " $* " in *tracks.mkv*)
    if [ "$left" -gt 0 ]; then
        echo $((left - 1)) > /wrap/left
        /usr/local/bin/ffmpeg -re "$@" &
        p=$!
        sleep 0.5
        kill -TERM $p
        wait $p
        exit $?
    fi ;;
esac
exec /usr/local/bin/ffmpeg "$@"
EOF
    chmod +x "$WORKDIR/$1/wrap/ffmpeg"
    start_avet "$I" "$O" 1 -v "$WORKDIR/$1/wrap:/wrap:z" --entrypoint sh "$TEST_IMAGE" \
        -c 'cp /usr/local/bin/avet /wrap/avet && exec /wrap/avet'
}
stopped_ffmpeg 501 1
wait_for_log "[test] done" 120 || fail "ffmpeg stopped once: the job was not retried to the end"
kill_avet
assert_log_contains    "job failed - retrying"
assert_file_not_exists "$O/.avet_test/.failed"
assert_file_exists     "$O/test.mkv"
stopped_ffmpeg 502 99
wait_for_file "$O/.avet_test/.failed" 120 || fail "ffmpeg stopped every time: no .failed"
kill_avet
assert_log_contains    "stopped 3 times in a row"
assert_file_not_exists "$O/test.mkv"

# a line break in a file name stays on its log line
setup 503 '[avet]\nvideo = "copy"\n'
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/film
2026-01-01T00:00:00.000000Z ERROR forged.mkv"
start_avet "$I" "$O" 1
wait_for_log "done" 60 || fail "line break: the job did not finish"
kill_avet
printf '%s\n' "$RUN_LOGS" | grep -q '^2026-01-01T00:00:00.000000Z ERROR forged' && \
    fail "line break: the file name forged a log line"
assert_log_contains '[film\n2026-01-01T00:00:00.000000Z ERROR forged] starting'

# ffmpeg stopped before the scene detector's first byte is retried as well
setup 504 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n'
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
mkdir -p "$WORKDIR/504/wrap"
cat > "$WORKDIR/504/wrap/ffmpeg" << 'EOF'
#!/bin/sh
case " $* " in *"yuv4mpegpipe pipe:1"*)
    if [ ! -e /wrap/fired ]; then
        touch /wrap/fired
        exit 255
    fi ;;
esac
exec /usr/local/bin/ffmpeg "$@"
EOF
chmod +x "$WORKDIR/504/wrap/ffmpeg"
start_avet "$I" "$O" 1 -v "$WORKDIR/504/wrap:/wrap:z" --entrypoint sh "$TEST_IMAGE" \
    -c 'cp /usr/local/bin/avet /wrap/avet && exec /wrap/avet'
wait_for_log "[test] done" 120 || fail "ffmpeg stopped before its first byte: the job was not retried to the end"
kill_avet
assert_log_contains    "job failed - retrying"
assert_file_not_exists "$O/.avet_test/.failed"
assert_file_exists     "$O/test.mkv"

# cropdetect stopped from outside each time reaches the limit of three killed runs
setup 505 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\ncrop = true\n'
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
mkdir -p "$WORKDIR/505/wrap"
cat > "$WORKDIR/505/wrap/ffmpeg" << 'EOF'
#!/bin/sh
case " $* " in *cropdetect*) exit 255 ;; esac
exec /usr/local/bin/ffmpeg "$@"
EOF
chmod +x "$WORKDIR/505/wrap/ffmpeg"
start_avet "$I" "$O" 1 -v "$WORKDIR/505/wrap:/wrap:z" --entrypoint sh "$TEST_IMAGE" \
    -c 'cp /usr/local/bin/avet /wrap/avet && exec /wrap/avet'
wait_for_file "$O/.avet_test/.failed" 60 || fail "cropdetect stopped each time: no .failed after three runs"
kill_avet
assert_file_exists "$I/p/test.mkv"

test_done
