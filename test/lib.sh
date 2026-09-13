#!/bin/sh
# Shared test library. Source this file from each test case.
#
# Provides:
#   run_avet        INPUT OUTPUT EXPECTED_FILE [TIMEOUT_S]
#   run_avet_timed  INPUT OUTPUT WAIT_S [LOG_PATTERN]
#   assert_*        various assertion helpers
#   test_done       call at end of each test case to exit with correct code
#
# Environment:
#   TEST_IMAGE      Docker image to use (default: avet:test)
#   FIXTURES_DIR    Path to test fixtures (default: sibling fixtures/ dir)
#   VERBOSE=1       Print Docker logs on failure

TEST_IMAGE="${TEST_IMAGE:-avet:test}"
# run.sh generates the fixtures and exports this; a suite run on its own has no source
# for them, and failing here beats a cascade of "cp: no such file".
if [ -z "${FIXTURES_DIR:-}" ] || [ ! -d "${FIXTURES_DIR:-}" ]; then
    printf "ERROR: FIXTURES_DIR is not set or does not exist - run the suite via ./test/run.sh\n" >&2
    exit 2
fi

_FAIL=0
_ERRORS=""
RUN_LOGS=""
_ESC=$(printf '\033')

# -- Failure tracking --------------------------------------------------------

fail() {
    _FAIL=1
    _ERRORS="${_ERRORS}  > $1
"
}

# -- Docker helpers ----------------------------------------------------------

# Run avet and wait until EXPECTED_FILE appears (or TIMEOUT_S elapses).
# Sets RUN_LOGS with container stdout+stderr.
# Returns 0 if the file appeared, 1 if timed out.
run_avet() {
    local input="$1" output="$2" expected="$3" timeout="${4:-120}"
    RUN_LOGS=""

    local cid
    cid=$(docker run -d \
        --user "$(id -u):$(id -g)" \
        -v "${input}:/input:z" \
        -v "${output}:/output:z" \
        -e POLL_INTERVAL=999999 \
        -e "RUST_LOG=${TEST_RUST_LOG:-info}" \
        "${TEST_IMAGE}")

    local elapsed=0
    while [ "$elapsed" -lt "$timeout" ]; do
        [ -e "$expected" ] && break
        local running
        running=$(docker inspect -f '{{.State.Running}}' "$cid" 2>/dev/null) || running="false"
        [ "$running" = "false" ] && break
        sleep 1
        elapsed=$((elapsed + 1))
    done

    # Output file appears at mux time but source is only moved to processed/
    # several lines later. Wait for "[stem] done" to confirm full cleanup.
    if [ -e "$expected" ]; then
        local stem done_wait=0
        stem=$(basename "$expected" .mkv)
        while [ "$done_wait" -lt 10 ]; do
            docker logs "$cid" 2>&1 | sed "s/${_ESC}\[[0-9;]*m//g" | \
                grep -qF "[$stem] done" && break
            sleep 1
            done_wait=$((done_wait + 1))
        done
    fi

    RUN_LOGS=$(docker logs "$cid" 2>&1) || true
    docker rm -f "$cid" >/dev/null 2>&1 || true

    # -s, not -e: every "expected 0 tracks" assertion reads a zero-byte file as a pass.
    [ -s "$expected" ] && return 0 || return 1
}

# Run avet for up to WAIT seconds, then stop. With an optional LOG_PATTERN it
# returns as soon as that pattern appears in the logs (capped at WAIT); without
# one it waits the full WAIT. Useful for negative tests where no output is expected.
# Always returns 0; sets RUN_LOGS.
run_avet_timed() {
    local input="$1" output="$2" wait="${3:-15}" pattern="${4:-}"
    RUN_LOGS=""

    local cid
    cid=$(docker run -d \
        --user "$(id -u):$(id -g)" \
        -v "${input}:/input:z" \
        -v "${output}:/output:z" \
        -e POLL_INTERVAL=999999 \
        -e "RUST_LOG=${TEST_RUST_LOG:-info}" \
        "${TEST_IMAGE}")

    if [ -n "$pattern" ]; then
        local elapsed=0
        while [ "$elapsed" -lt "$wait" ]; do
            docker logs "$cid" 2>&1 | sed "s/${_ESC}\[[0-9;]*m//g" | \
                grep -qF "$pattern" && break
            sleep 1
            elapsed=$((elapsed + 1))
        done
    else
        sleep "$wait"
    fi

    RUN_LOGS=$(docker logs "$cid" 2>&1) || true
    docker rm -f "$cid" >/dev/null 2>&1 || true
    return 0
}

# -- Assertions ---------------------------------------------------------------

assert_file_exists() {
    [ -f "$1" ] || fail "expected file to exist: $1"
}

assert_file_nonempty() {
    [ -s "$1" ] || fail "expected non-empty file: $1"
}

assert_file_not_exists() {
    [ ! -f "$1" ] || fail "expected file NOT to exist: $1"
}

assert_dir_exists() {
    [ -d "$1" ] || fail "expected directory to exist: $1"
}

assert_dir_not_exists() {
    [ ! -d "$1" ] || fail "expected directory NOT to exist: $1"
}

# An unreadable file counts 0 tracks just as convincingly as a correct one, so anything
# asserting "no tracks of this kind" proves the file is readable first.
assert_probeable() {
    local file="$1"
    if [ ! -s "$file" ]; then
        fail "expected a non-empty file to probe: $file"
        return 1
    fi
    if ! ffprobe -v error -i "$file" >/dev/null 2>&1; then
        fail "ffprobe cannot read $file"
        return 1
    fi
    return 0
}

assert_audio_track_count() {
    local file="$1" expected="$2"
    local actual
    assert_probeable "$file" || return
    actual=$(ffprobe -v quiet -select_streams a \
        -show_entries stream=codec_type -of csv=p=0 "$file" 2>/dev/null | \
        grep "audio" | wc -l | tr -d ' ')
    [ "$actual" = "$expected" ] || \
        fail "audio track count: expected $expected, got $actual ($file)"
}

assert_audio_codec() {
    local file="$1" idx="$2" expected="$3"
    local actual
    actual=$(ffprobe -v quiet -select_streams "a:${idx}" \
        -show_entries stream=codec_name -of csv=p=0 "$file" 2>/dev/null | \
        tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "audio track $idx codec: expected $expected, got $actual ($file)"
}

assert_subtitle_track_count() {
    local file="$1" expected="$2"
    local actual
    assert_probeable "$file" || return
    actual=$(ffprobe -v quiet -select_streams s \
        -show_entries stream=codec_type -of csv=p=0 "$file" 2>/dev/null | \
        grep "subtitle" | wc -l | tr -d ' ')
    [ "$actual" = "$expected" ] || \
        fail "subtitle track count: expected $expected, got $actual ($file)"
}

assert_subtitle_language() {
    local file="$1" idx="$2" expected="$3"
    local actual
    actual=$(ffprobe -v quiet -select_streams "s:${idx}" \
        -show_entries stream_tags=language -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "subtitle track $idx language: expected $expected, got '$actual' ($file)"
}

assert_audio_language() {
    local file="$1" idx="$2" expected="$3"
    local actual
    actual=$(ffprobe -v quiet -select_streams "a:${idx}" \
        -show_entries stream_tags=language -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "audio track $idx language: expected $expected, got '$actual' ($file)"
}

assert_video_height() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=height -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "video height: expected $expected, got $actual ($file)"
}

assert_video_height_le() {
    local file="$1" max="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=height -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    # Defaulting to 0 would turn "there is no output file" into a passing assertion.
    case "$actual" in
        ''|*[!0-9]*) fail "video height: could not read a height from $file"; return ;;
    esac
    [ "$actual" -le "$max" ] || \
        fail "video height: expected <= $max, got $actual ($file)"
}

assert_video_codec() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=codec_name -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "video codec: expected $expected, got $actual ($file)"
}

assert_audio_channels() {
    local file="$1" idx="$2" expected="$3"
    local actual
    actual=$(ffprobe -v quiet -select_streams "a:${idx}" \
        -show_entries stream=channels -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "audio track $idx channels: expected $expected, got $actual ($file)"
}

assert_audio_title() {
    local file="$1" idx="$2" expected="$3"
    local actual
    actual=$(ffprobe -v quiet -select_streams "a:${idx}" \
        -show_entries stream_tags=title -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "audio track $idx title: expected '$expected', got '$actual' ($file)"
}

assert_color_transfer() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=color_transfer -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "color_transfer: expected $expected, got $actual ($file)"
}

assert_color_primaries() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=color_primaries -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "color_primaries: expected $expected, got $actual ($file)"
}

assert_video_pix_fmt() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=pix_fmt -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "pix_fmt: expected $expected, got $actual ($file)"
}

assert_video_height_lt() {
    local file="$1" max="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=height -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    # Defaulting to 0 would turn "there is no output file" into a passing assertion.
    case "$actual" in
        ''|*[!0-9]*) fail "video height: could not read a height from $file"; return ;;
    esac
    [ "$actual" -lt "$max" ] || \
        fail "video height: expected < $max, got $actual ($file)"
}

# Starts at frame 0 and is gapless; a gap is source no chunk ever encodes. Not the tail -
# job.rs clamps and extends that in memory. assert_video_frames covers a short encode.
assert_scenes_cover() {
    local file="$1"
    if [ ! -f "$file" ]; then
        fail "scenes.json missing: $file"
        return
    fi

    local problem
    problem=$(tr -d ' \n' < "$file" \
        | grep -o '"start_frame":[0-9]*,"end_frame":[0-9]*' \
        | awk -F'[:,]' '
            {
                s = $2; e = $4
                if (NR == 1 && s != 0) {
                    print "first chunk starts at " s ", expected 0"; exit
                }
                if (NR > 1 && s != prev + 1) {
                    print "chunk starts at " s " but the previous one ended at " prev; exit
                }
                if (e < s) { print "chunk " s ".." e " ends before it starts"; exit }
                prev = e
            }
            END { if (NR == 0) print "no frame ranges found" }')

    [ -z "$problem" ] || fail "scene list: $problem ($file)"
}

# The one assertion that catches a short encode: a truncated file still muxes and probes.
assert_video_frames() {
    local file="$1" expected="$2"
    local actual
    assert_probeable "$file" || return
    actual=$(ffprobe -v quiet -select_streams v:0 -count_packets \
        -show_entries stream=nb_read_packets -of csv=p=0 "$file" 2>/dev/null | tr -d '\n')
    case "$actual" in
        ''|*[!0-9]*) fail "frame count: could not read one from $file"; return ;;
    esac
    [ "$actual" = "$expected" ] || \
        fail "frame count: expected $expected, got $actual ($file)"
}

assert_log_contains() {
    printf '%s\n' "$RUN_LOGS" | sed "s/${_ESC}\[[0-9;]*m//g" | grep -qF "$1" || \
        fail "log does not contain: $1"
}

assert_log_not_contains() {
    printf '%s\n' "$RUN_LOGS" | sed "s/${_ESC}\[[0-9;]*m//g" | grep -qF "$1" && \
        fail "log should NOT contain: $1" || true
}

# -- Test lifecycle ------------------------------------------------------------

# Call at the end of every test case. Prints errors and exits with correct code.
test_done() {
    if [ "$_FAIL" -eq 0 ]; then
        exit 0
    fi
    printf "%s" "$_ERRORS"
    if [ "${VERBOSE:-0}" = "1" ] && [ -n "$RUN_LOGS" ]; then
        printf "  [Docker logs]\n"
        echo "$RUN_LOGS" | while IFS= read -r line; do
            printf "  | %s\n" "$line"
        done
    fi
    exit 1
}
