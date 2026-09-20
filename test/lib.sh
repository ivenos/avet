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
_DONE=0
_CLEANUP=""
RUN_LOGS=""
_ESC=$(printf '\033')

# -- Tools ---------------------------------------------------------------------

# Every probe runs the image's ffmpeg and mkvtoolnix: a host build of another version
# reads side data, timestamps and dispositions differently, or not at all.
_TMP_ROOT=$(dirname "$(mktemp -u)")
_TOOLS=$(docker run -d --rm --label avet-test-tools \
    --security-opt label=disable \
    --user "$(id -u):$(id -g)" \
    -v "${_TMP_ROOT}:${_TMP_ROOT}" \
    --entrypoint sleep "$TEST_IMAGE" infinity) || {
    printf "ERROR: could not start the tools container from %s\n" "$TEST_IMAGE" >&2
    exit 2
}

tool()       { docker exec "$_TOOLS" "$@"; }
ffmpeg()     { tool ffmpeg "$@"; }
ffprobe()    { tool ffprobe "$@"; }
mkvmerge()   { tool mkvmerge "$@"; }
mkvextract() { tool mkvextract "$@"; }

# -- Failure tracking --------------------------------------------------------

fail() {
    _FAIL=1
    _ERRORS="${_ERRORS}  > $1
"
}

# Owning the EXIT trap here keeps a suite from replacing the one that calls test_done.
test_workdir() {
    local d
    d=$(mktemp -d)
    _CLEANUP="$_CLEANUP $d"
    printf '%s' "$d"
}

# -- Docker helpers ----------------------------------------------------------

# Run avet and wait until EXPECTED_FILE appears (or TIMEOUT_S elapses).
# Sets RUN_LOGS with container stdout+stderr.
# Returns 0 if the file appeared, 1 if timed out.
run_avet() {
    local input="$1" output="$2" expected="$3" timeout="${4:-120}"
    RUN_LOGS=""

    local cid
    cid=$(docker run -d --label avet-test-tools \
        --user "$(id -u):$(id -g)" \
        -v "${input}:/input:z" \
        -v "${output}:/output:z" \
        -e POLL_INTERVAL=999999 \
        -e "RUST_LOG=${TEST_RUST_LOG:-info}" \
        "${TEST_IMAGE}")

    local elapsed=0
    while [ "$elapsed" -lt "$timeout" ]; do
        # -s, not -e: a zero-byte leftover from an earlier run is not this run's output.
        [ -s "$expected" ] && break
        local running
        running=$(docker inspect -f '{{.State.Running}}' "$cid" 2>/dev/null) || running="false"
        [ "$running" = "false" ] && break
        sleep 1
        elapsed=$((elapsed + 1))
    done

    # Output file appears at mux time but source is only moved to processed/
    # several lines later. Wait for "[stem] done" to confirm full cleanup.
    if [ -s "$expected" ]; then
        local stem done_wait=0
        stem=$(basename "$expected" .mkv)
        while [ "$done_wait" -lt 30 ]; do
            docker logs "$cid" 2>&1 | sed "s/${_ESC}\[[0-9;]*m//g" | \
                grep -qF "[$stem] done" && break
            sleep 1
            done_wait=$((done_wait + 1))
        done
        # Otherwise the container is killed mid-archive and the suite blames the archiving.
        [ "$done_wait" -lt 30 ] || fail "run_avet: $stem produced output but never logged done"
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
    cid=$(docker run -d --label avet-test-tools \
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

# -- Daemon helpers ------------------------------------------------------------

# Leaves avet running under AVET_CID, for a test that acts on a live daemon.
start_avet() { # INPUT OUTPUT [POLL_INTERVAL]
    RUN_LOGS=""
    AVET_CID=$(docker run -d --label avet-test-tools \
        --user "$(id -u):$(id -g)" \
        -v "${1}:/input:z" \
        -v "${2}:/output:z" \
        -e POLL_INTERVAL="${3:-2}" \
        -e "RUST_LOG=${TEST_RUST_LOG:-info}" \
        "${TEST_IMAGE}")
}

avet_logs() {
    RUN_LOGS=$(docker logs "$AVET_CID" 2>&1) || true
}

wait_for_log() { # PATTERN TIMEOUT_S
    local elapsed=0
    while [ "$elapsed" -lt "$2" ]; do
        docker logs "$AVET_CID" 2>&1 | sed "s/${_ESC}\[[0-9;]*m//g" | grep -qF "$1" && { avet_logs; return 0; }
        sleep 1
        elapsed=$((elapsed + 1))
    done
    avet_logs
    return 1
}

wait_for_file() { # PATH TIMEOUT_S
    local elapsed=0
    while [ "$elapsed" -lt "$2" ]; do
        [ -s "$1" ] && { avet_logs; return 0; }
        sleep 1
        elapsed=$((elapsed + 1))
    done
    avet_logs
    return 1
}

# SIGTERM, then wait; sets AVET_RC, 124 for a container still running after TIMEOUT_S.
stop_avet() { # [TIMEOUT_S]
    local elapsed=0 timeout="${1:-60}" running
    docker kill --signal=TERM "$AVET_CID" >/dev/null 2>&1
    while [ "$elapsed" -lt "$timeout" ]; do
        running=$(docker inspect -f '{{.State.Running}}' "$AVET_CID" 2>/dev/null) || running="false"
        [ "$running" = "false" ] && break
        sleep 1
        elapsed=$((elapsed + 1))
    done
    if [ "$running" = "false" ]; then
        AVET_RC=$(docker inspect -f '{{.State.ExitCode}}' "$AVET_CID" 2>/dev/null || echo 125)
    else
        AVET_RC=124
    fi
    avet_logs
    docker rm -f "$AVET_CID" >/dev/null 2>&1 || true
}

kill_avet() {
    avet_logs
    docker rm -f "$AVET_CID" >/dev/null 2>&1 || true
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
        -show_entries stream=height -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "video height: expected $expected, got $actual ($file)"
}

assert_video_height_le() {
    local file="$1" max="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=height -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
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
        -show_entries stream=codec_name -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
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
        -show_entries stream=color_transfer -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "color_transfer: expected $expected, got $actual ($file)"
}

assert_color_primaries() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=color_primaries -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "color_primaries: expected $expected, got $actual ($file)"
}

assert_video_pix_fmt() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=pix_fmt -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
    [ "$actual" = "$expected" ] || \
        fail "pix_fmt: expected $expected, got $actual ($file)"
}

assert_video_height_lt() {
    local file="$1" max="$2"
    local actual
    actual=$(ffprobe -v quiet -select_streams v:0 \
        -show_entries stream=height -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
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
        -show_entries stream=nb_read_packets -of default=nw=1:nk=1 "$file" 2>/dev/null | tr -d '\n')
    case "$actual" in
        ''|*[!0-9]*) fail "frame count: could not read one from $file"; return ;;
    esac
    [ "$actual" = "$expected" ] || \
        fail "frame count: expected $expected, got $actual ($file)"
}

# The first capture group of PATTERN in the run's log, with the color codes removed.
log_capture() {
    printf '%s\n' "$RUN_LOGS" | sed "s/${_ESC}\[[0-9;]*m//g" | sed -n "$1" | head -n 1
}

assert_log_contains() {
    printf '%s\n' "$RUN_LOGS" | sed "s/${_ESC}\[[0-9;]*m//g" | grep -qF "$1" || \
        fail "log does not contain: $1"
}

assert_log_not_contains() {
    printf '%s\n' "$RUN_LOGS" | sed "s/${_ESC}\[[0-9;]*m//g" | grep -qF "$1" && \
        fail "log should NOT contain: $1" || true
}

# -- Content ---------------------------------------------------------------------

# MPEG-TS lists the stream a second time under its program.
packet_count() {
    ffprobe -v error -select_streams v:0 -count_packets \
        -show_entries stream=nb_read_packets -of default=nw=1:nk=1 "$1" 2>/dev/null | head -n 1
}

# Frame by frame against REFERENCE after FILTER, by index rather than by time. The
# fixtures change completely from one frame to the next (about 11 dB between
# neighbours), so a dropped, doubled, swapped or shifted frame lands far below MIN_DB.
assert_frames_match() {
    local out="$1" ref="$2" filter="${3:-null}" min="${4:-26}"
    local n_out n_ref pix report
    n_out=$(packet_count "$out")
    n_ref=$(packet_count "$ref")
    if [ -z "$n_out" ] || [ "$n_out" != "$n_ref" ]; then
        fail "frames: $out holds '$n_out' frames, the reference '$n_ref'"
        return
    fi
    pix=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt \
        -of default=nw=1:nk=1 "$out" | tr -d '\n')
    report=$(ffmpeg -hide_banner -loglevel fatal -i "$out" -i "$ref" -filter_complex \
        "[0:v:0]settb=1,setpts=N,format=$pix[a];[1:v:0]$filter,settb=1,setpts=N,format=$pix[b];[a][b]psnr=stats_file=-" \
        -fps_mode passthrough -f null - | awk -v min="$min" '
            /psnr_y:/ {
                db = 99
                for (i = 1; i <= NF; i++) {
                    split($i, kv, ":")
                    if (kv[1] ~ /^psnr_[yuv]$/ && kv[2] != "inf" && kv[2] + 0 < db) db = kv[2] + 0
                }
                if (db < min && !bad) { bad = 1; at = n; worst = db }
                n++
            }
            END {
                if (bad) printf "frame %d is %.2f dB", at, worst
                else printf "%d", n
            }')
    [ "$report" = "$n_out" ] || \
        fail "frames: $out against $ref: $report (all $n_out frames must reach $min dB)"
}

# Milliseconds from the first video frame, sorted, one per line.
frame_times() {
    ffprobe -v error -select_streams v:0 -show_entries packet=pts_time \
        -of default=nw=1:nk=1 "$1" | grep -v N/A | sort -g \
        | awk 'NR == 1 { base = $1 } { printf "%.3f\n", ($1 - base) * 1000 }'
}

# Tolerance 1 ms: Matroska stores milliseconds, the source may be finer.
assert_frame_times_match() {
    local out="$1" ref="$2" diff
    diff=$(paste -d ' ' "$(_tmp_file frame_times "$out")" "$(_tmp_file frame_times "$ref")" | awk '
        NF != 2 { printf "frame %d exists on one side only", NR - 1; exit }
        { d = $1 - $2; if (d < 0) d = -d; if (d > 1.001) { printf "frame %d at %s ms, expected %s ms", NR - 1, $1, $2; exit } }
        END { if (NR == 0) printf "neither file has readable frame times" }')
    [ -z "$diff" ] || fail "frame times: $out: $diff"
}

_SCRATCH=$(mktemp -d)

_tmp_file() {
    local f
    f=$(mktemp -p "$_SCRATCH")
    "$@" > "$f"
    echo "$f"
}

# Times of the frames the fixtures flash white, and of the beeps under them, in seconds
# on the file's own timeline (-copyts), so a lost stream offset shows up. A beep counts
# once silence follows it; the end of the stream is not one.
flash_times() {
    ffmpeg -hide_banner -v error -copyts -i "$1" -map 0:v:0 \
        -vf "signalstats,metadata=mode=print:key=lavfi.signalstats.YAVG:file=-" -enc_time_base:v 1/1000 -f null - \
        | awk '/pts_time:/ { n = split($0, f, "pts_time:"); t = f[n] + 0 }
               /YAVG=/ { split($0, v, "="); time[++k] = t; y[k] = v[2] + 0; if (y[k] > max) max = y[k] }
               END { for (i = 1; i <= k; i++) if (y[i] > max * 0.9) printf "%.4f\n", time[i] }'
}

beep_times() {
    ffmpeg -hide_banner -v info -copyts -i "$1" -map "0:a:${2:-0}" \
        -af silencedetect=noise=-30dB:duration=0.02 -f null - 2>&1 \
        | awk '{ for (i = 1; i < NF; i++) {
                     if ($i == "silence_end:") onset = $(i + 1)
                     if ($i == "silence_start:" && onset != "") { printf "%.4f\n", onset; onset = "" }
                 } }'
}

# Beep minus flash for every pair, in milliseconds.
av_offsets() {
    paste -d ' ' "$(_tmp_file flash_times "$1")" "$(_tmp_file beep_times "$1" "${2:-0}")" \
        | awk 'NF == 2 { printf "%.1f\n", ($2 - $1) * 1000 } NF != 2 { print "unpaired" }'
}

# Audio track INDEX keeps the sync that track SRC_INDEX has in the source, at every flash,
# within 12 ms: half a frame at 24 fps is 21 ms, and codec frame edges move a beep by a few.
assert_av_sync() {
    local out="$1" src="$2" index="${3:-0}" o s diff
    o=$(av_offsets "$out" "$index")
    s=$(av_offsets "$src" "${4:-$index}")
    if [ -z "$s" ] || printf '%s\n' "$s" | grep -q unpaired; then
        fail "sync: the source $src has no clean flash/beep pairs: $(echo $s)"
        return
    fi
    diff=$(printf '%s\n' "$o" | paste -d ' ' - "$(_tmp_file printf '%s\n' "$s")" | awk '
        NF != 2 || $1 == "unpaired" { printf "pair %d missing", NR; exit }
        { d = $1 - $2; if (d < 0) d = -d; if (d > 12) { printf "pair %d: audio %s ms from the flash, source %s ms", NR, $1, $2; exit } }')
    [ -z "$diff" ] || fail "sync: $out audio $index: $diff"
}

# Packet payloads of one stream, in order: size and MD5, without timestamps.
packet_hashes() {
    ffmpeg -hide_banner -v error -i "$1" -map "0:$2" -c copy -f framemd5 - \
        | awk -F', *' '!/^#/ && NF >= 6 { print $5, $6 }'
}

assert_packets_identical() {
    local out="$1" out_spec="$2" src="$3" src_spec="$4" a b
    a=$(packet_hashes "$out" "$out_spec")
    b=$(packet_hashes "$src" "$src_spec")
    if [ -z "$b" ]; then
        fail "packets: the source $src has nothing in $src_spec"
    elif [ "$a" != "$b" ]; then
        fail "packets: $out $out_spec differs from $src $src_spec ($(printf '%s\n' "$a" | wc -l) against $(printf '%s\n' "$b" | wc -l) packets)"
    fi
}

# Packet times of STREAM relative to the first video frame, in milliseconds.
stream_times() {
    local first
    first=$(frame_times_abs "$1" | head -n 1)
    ffprobe -v error -select_streams "$2" -show_entries packet=pts_time \
        -of default=nw=1:nk=1 "$1" | grep -v N/A \
        | awk -v base="$first" '{ printf "%.1f\n", ($1 - base) * 1000 }'
}

frame_times_abs() {
    ffprobe -v error -select_streams v:0 -show_entries packet=pts_time \
        -of default=nw=1:nk=1 "$1" | grep -v N/A | sort -g
}

assert_stream_times_match() {
    local out="$1" out_spec="$2" src="$3" src_spec="$4" diff
    diff=$(paste -d ' ' "$(_tmp_file stream_times "$out" "$out_spec")" "$(_tmp_file stream_times "$src" "$src_spec")" | awk '
        NF != 2 { printf "packet %d exists on one side only", NR - 1; exit }
        { d = $1 - $2; if (d < 0) d = -d; if (d > 2) { printf "packet %d at %s ms from the first frame, source %s ms", NR - 1, $1, $2; exit } }
        END { if (NR == 0) printf "neither stream has readable packet times" }')
    [ -z "$diff" ] || fail "stream times: $out $out_spec: $diff"
}

# Start and end relative to the first video frame, and the title.
chapter_list() {
    local first
    first=$(frame_times_abs "$1" | head -n 1)
    ffprobe -v error -show_entries chapter=start_time,end_time:chapter_tags=title \
        -of csv=p=0 "$1" | awk -F, -v base="$first" '{ printf "%.3f %.3f %s\n", $1 - base, $2 - base, $3 }'
}

# Within 2 ms: the last chapter ends with the file, which each muxer rounds on its own.
assert_chapters_match() {
    local a b diff
    a=$(chapter_list "$1")
    b=$(chapter_list "$2")
    [ -n "$b" ] || { fail "chapters: the source $2 has none"; return; }
    diff=$(printf '%s\n' "$a" | paste -d '|' - "$(_tmp_file printf '%s\n' "$b")" | awk -F'|' '
        { split($1, x, " "); split($2, y, " ")
          if (x[3] != y[3] || (x[1] - y[1])^2 > 0.000004 || (x[2] - y[2])^2 > 0.000004) { printf "chapter %d", NR; exit } }')
    [ -z "$diff" ] && [ "$(printf '%s\n' "$a" | wc -l)" = "$(printf '%s\n' "$b" | wc -l)" ] || \
        fail "chapters: $1 has [$(echo $a)], the source [$(echo $b)]"
}

# Language, title and every disposition flag of the streams matching SPEC, once per stream.
track_list() {
    ffprobe -v error -select_streams "$2" \
        -show_entries stream=codec_type:stream_tags=language,title:stream_disposition=default,forced,comment,hearing_impaired,visual_impaired,original \
        -of compact=nk=1 "$1" | awk -F'|' 'NF > 2 && $1 != "program"'
}

# Language and every disposition flag, without the title an encode marks with its codec.
track_flags() {
    ffprobe -v error -select_streams "$2" \
        -show_entries stream=codec_type:stream_tags=language:stream_disposition=default,forced,comment,hearing_impaired,visual_impaired,original \
        -of compact=nk=1 "$1" | awk -F'|' 'NF > 2 && $1 != "program"'
}

assert_track_flags_match() {
    local out="$1" src="$2" spec="$3" a b
    a=$(track_flags "$out" "$spec")
    b=$(track_flags "$src" "$spec")
    [ -n "$b" ] && [ "$a" = "$b" ] || \
        fail "track flags $spec: $out has [$(echo $a)], the source [$(echo $b)]"
}

# Payload bytes of one stream.
track_bytes() {
    ffprobe -v error -select_streams "$2" -show_entries packet=size -of csv=p=0 "$1" \
        | awk '{ s += $1 } END { print s + 0 }'
}

assert_tracks_match() {
    local out="$1" src="$2" spec="$3" a b
    a=$(track_list "$out" "$spec")
    b=$(track_list "$src" "$spec")
    [ -n "$b" ] && [ "$a" = "$b" ] || \
        fail "tracks $spec: $out has [$(echo $a)], the source [$(echo $b)]"
}

# Name and SHA-256 of every attachment.
attachment_list() {
    local dir
    dir=$(mktemp -d -p "$_SCRATCH")
    mkvmerge -i "$1" | sed -n "s/^Attachment ID \([0-9]*\):.*file name '\(.*\)'$/\1 \2/p" \
        | while read -r id name; do
            mkvextract "$1" attachments "$id:$dir/$id" >/dev/null
            printf '%s %s\n' "$name" "$(sha256sum "$dir/$id" | cut -d ' ' -f 1)"
        done
    rm -rf "$dir"
}

assert_attachments_match() {
    local a b
    a=$(attachment_list "$1")
    b=$(attachment_list "$2")
    [ -n "$b" ] || { fail "attachments: the source $2 has none"; return; }
    [ "$a" = "$b" ] || fail "attachments: $1 has [$(echo $a)], the source [$(echo $b)]"
}

# The whole file decodes without a single error. A millisecond time base, or the null muxer
# reports frames half a frame off the grid as out of order.
assert_decodes_cleanly() {
    local err
    if ! err=$(ffmpeg -hide_banner -v error -xerror -i "$1" -map 0:v -map '0:a?' -enc_time_base:v 1/1000 -f null - 2>&1); then
        fail "decode: $1 fails: $err"
    elif [ -n "$err" ]; then
        fail "decode: $1 reports errors: $err"
    fi
}

# Decoded samples of audio track INDEX against the source's SRC_INDEX, bit for bit, the
# output at most MISSING samples short at the end.
assert_audio_samples_identical() {
    local out="$1" src="$2" index="${3:-0}" src_index="${4:-${3:-0}}" missing="${5:-0}" a b channels
    a=$(mktemp -p "$_SCRATCH"); b=$(mktemp -p "$_SCRATCH")
    ffmpeg -hide_banner -v error -i "$out" -map "0:a:$index" -c:a pcm_s32le -f s32le - > "$a"
    ffmpeg -hide_banner -v error -i "$src" -map "0:a:$src_index" -c:a pcm_s32le -f s32le - > "$b"
    channels=$(ffprobe -v error -select_streams "a:$src_index" -show_entries stream=channels -of csv=p=0 "$src" | head -n 1)
    if [ ! -s "$b" ]; then
        fail "samples: the source $src has no audio $src_index"
    elif ! cmp -s -n "$(stat -c %s "$a")" "$a" "$b" || \
        [ $(( ($(stat -c %s "$b") - $(stat -c %s "$a")) / 4 / channels )) -gt "$missing" ] || \
        [ "$(stat -c %s "$a")" -gt "$(stat -c %s "$b")" ]; then
        fail "samples: $out audio $index is not bit-exact against source audio $src_index ($(stat -c %s "$a") against $(stat -c %s "$b") bytes)"
    fi
}

# Decoded pictures, bit for bit and in order.
assert_frames_identical() {
    local a b
    a=$(ffmpeg -hide_banner -v error -i "$1" -map 0:v:0 -f framemd5 - | awk -F', *' '!/^#/ { print $6 }')
    b=$(ffmpeg -hide_banner -v error -i "$2" -map 0:v:0 -f framemd5 - | awk -F', *' '!/^#/ { print $6 }')
    [ -n "$b" ] && [ "$a" = "$b" ] || \
        fail "pictures: $1 is not bit-exact ($(printf '%s\n' "$a" | wc -l) against $(printf '%s\n' "$b" | wc -l) frames of $2)"
}

# Decoded subtitle events: start from the first decoded video frame, duration, show or clear.
subtitle_events() {
    local first
    first=$(ffprobe -v error -select_streams v:0 -read_intervals '%+#1' -show_entries frame=pts_time \
        -of default=nw=1:nk=1 "$1" | head -n 1)
    ffprobe -v error -select_streams "$2" -show_frames \
        -show_entries subtitle=pts_time,end_display_time,num_rects -of csv=p=0 "$1" \
        | awk -F, -v base="$first" '{ printf "%.3f %s %s\n", $1 - base, $2, ($3 > 0 ? "show" : "clear") }'
}

# Within 2 ms: every muxer rounds to its own time base.
assert_subtitle_events_match() {
    local out="$1" out_spec="$2" src="$3" src_spec="$4" diff
    diff=$(paste -d ' ' "$(_tmp_file subtitle_events "$out" "$out_spec")" "$(_tmp_file subtitle_events "$src" "$src_spec")" | awk '
        NF != 6 { printf "event %d exists on one side only", NR - 1; exit }
        { d = $1 - $4; if (d < 0) d = -d
          if (d > 0.002 || $2 != $5 || $3 != $6) { printf "event %d is %s %s %s, the source %s %s %s", NR - 1, $1, $2, $3, $4, $5, $6; exit } }
        END { if (NR == 0) print "no events" }')
    [ -z "$diff" ] || fail "subtitles: $out $out_spec: $diff"
}

# Frame numbers of the keyframes, one per line.
keyframe_indices() {
    ffprobe -v error -select_streams v:0 -show_entries packet=flags -of csv=p=0 "$1" \
        | awk '/K/ { print NR - 1 }'
}

# Every chunk but the last holds at least MIN frames, as min_scene_len asks.
assert_min_chunk_frames() {
    local file="$1" min="$2" problem
    problem=$(tr -d ' \n' < "$file" | grep -o '"start_frame":[0-9]*,"end_frame":[0-9]*' \
        | awk -F'[:,]' -v min="$min" '
            { s[NR] = $2; e[NR] = $4 }
            END {
                if (NR == 0) { print "no frame ranges found"; exit }
                for (i = 1; i < NR; i++) {
                    len = e[i] - s[i] + 1
                    if (len < min) { printf "chunk %d holds %d frames", i - 1, len; exit }
                }
            }')
    [ -z "$problem" ] || fail "chunk length: $problem, expected at least $min ($file)"
}

# No chunk of SCENES_JSON longer than MAX frames, the last one included.
assert_max_chunk_frames() {
    local file="$1" max="$2" problem
    problem=$(tr -d ' \n' < "$file" | grep -o '"start_frame":[0-9]*,"end_frame":[0-9]*' \
        | awk -F'[:,]' -v max="$max" '
            { s[NR] = $2; e[NR] = $4 }
            END {
                if (NR == 0) { print "no frame ranges found"; exit }
                for (i = 1; i <= NR; i++) {
                    len = e[i] - s[i] + 1
                    if (len > max) { printf "chunk %d holds %d frames", i - 1, len; exit }
                }
            }')
    [ -z "$problem" ] || fail "chunk length: $problem, expected at most $max ($file)"
}

# Every chunk of SCENES_JSON starts on a keyframe of OUT.
assert_keyframes_at_chunks() {
    local keyframes missing
    keyframes=" $(ffprobe -v error -select_streams v:0 -show_entries packet=flags -of csv=p=0 "$1" \
        | awk '/K/ { printf "%d ", NR - 1 }')"
    missing=$(tr -d ' \n' < "$2" | grep -o '"start_frame":[0-9]*' | cut -d: -f2 | while read -r start; do
        case "$keyframes" in *" $start "*) ;; *) printf '%s ' "$start" ;; esac
    done)
    [ -n "$(tr -d ' \n' < "$2")" ] && [ -z "$missing" ] || fail "keyframes: chunks of $1 start without one at frame(s) $missing"
}

# A player that seeks lands on the same picture a straight decode shows at that time.
assert_seeks_land_on_frames() {
    local full t got want
    full=$(mktemp -p "$_SCRATCH")
    ffmpeg -hide_banner -v error -copyts -i "$1" -map 0:v:0 -f framemd5 - | awk -F', *' '!/^#/ { print $3, $6 }' > "$full"
    for t in 1.3 2.05 4.9 7.7 9.5; do
        got=$(ffmpeg -hide_banner -v error -ss "$t" -i "$1" -map 0:v:0 -frames:v 1 -copyts -f framemd5 - \
            | awk -F', *' '!/^#/ { print $3, $6 }')
        want=$(awk -v pts="${got%% *}" '$1 == pts { print $2 }' "$full")
        [ -n "$got" ] && [ "${got##* }" = "$want" ] || fail "seek: $1 at $t s shows '${got##* }', a straight decode '$want'"
    done
}

# The dominant frequency of each channel of audio track INDEX, in Hz, in channel order.
channel_frequencies() {
    ffmpeg -hide_banner -v error -i "$1" -map "0:a:${2:-0}" \
        -af "aspectralstats=measure=centroid,ametadata=mode=print:file=-" -f null - \
        | awk -F= '/centroid=/ { n = split($1, k, "."); ch = k[n - 1] + 0; sum[ch] += $2; cnt[ch]++; if (ch > max) max = ch }
                   END { for (c = 1; c <= max; c++) printf "%d\n", sum[c] / cnt[c] }'
}

# EXPECTED lists one frequency per channel; each has to come out on its own channel. The
# 30 Hz floor is for the LFE, which Opus low-passes.
assert_channel_frequencies() {
    local out="$1" index="$2" expected="$3" got bad
    got=$(channel_frequencies "$out" "$index")
    bad=$(printf '%s\n' "$got" | paste -d ' ' - "$(_tmp_file printf '%s\n' $expected)" | awk '
        NF != 2 { printf "channel %d missing", NR; exit }
        { d = $1 - $2; if (d < 0) d = -d; if (d > 30 && d > $2 * 0.07) { printf "channel %d at %d Hz, expected %d Hz", NR, $1, $2; exit } }')
    [ -z "$bad" ] || fail "channels: $out audio $index: $bad (got $(echo $got))"
}

assert_same_bytes() {
    cmp -s "$1" "$2" || fail "bytes: $1 differs from $2"
}

# EXPECTED is "-" for "no such value"; an empty one is a fixture that lost the property.
assert_stream_value() {
    local file="$1" spec="$2" entry="$3" expected="$4" actual
    if [ -z "$expected" ]; then
        fail "$entry of $spec: no expected value given (pass - for none) ($file)"
        return
    fi
    [ "$expected" = "-" ] && expected=""
    actual=$(ffprobe -v error -select_streams "$spec" -show_entries "$entry" \
        -of default=nw=1:nk=1 "$file" | tr '\n' ' ' | sed 's/ *$//')
    [ "$actual" = "$expected" ] || fail "$entry of $spec: expected '$expected', got '$actual' ($file)"
}

stream_value() {
    local v
    v=$(ffprobe -v error -select_streams "$2" -show_entries "$3" \
        -of default=nw=1:nk=1 "$1" | tr '\n' ' ' | sed 's/ *$//')
    printf '%s' "${v:--}"
}

frames_with_side_data() {
    ffprobe -v error "$1" -select_streams v:0 -show_frames \
        -show_entries frame_side_data=side_data_type -of csv=p=0 | grep -c "$2"
}

assert_frames_with_side_data() {
    local file="$1" pattern="$2" expected="$3" actual
    assert_probeable "$file" || return
    actual=$(frames_with_side_data "$file" "$pattern")
    [ "$actual" = "$expected" ] || \
        fail "frames with '$pattern': expected $expected, got $actual ($file)"
}

assert_dovi_record() {
    local file="$1" expected="$2" actual
    assert_probeable "$file" || return
    actual=$(ffprobe -v error "$file" -select_streams v:0 \
        -show_entries stream_side_data=dv_profile,dv_bl_signal_compatibility_id -of default=nw=1:nk=1 | paste -sd, -)
    [ "$actual" = "$expected" ] || \
        fail "Dolby Vision record (profile,compatibility): expected '$expected', got '$actual' ($file)"
}

# Mastering display and content light level of the first frame, as plain numbers.
hdr_static() {
    ffprobe -v error -select_streams v:0 -read_intervals '%+#1' \
        -show_entries frame_side_data=red_x,red_y,green_x,green_y,blue_x,blue_y,white_point_x,white_point_y,min_luminance,max_luminance,max_content,max_average \
        -of default=nw=1 "$1" | awk -F= 'NF == 2 { split($2, r, "/"); printf "%s %.8f\n", $1, (r[2] + 0 ? r[1] / r[2] : r[1]) }' | sort
}

# Chromaticity to 1/65536 and minimum luminance to 1/16384, what AV1 can store;
# anything coarser is a loss avet caused.
assert_hdr_static_match() {
    local diff
    diff=$(paste -d ' ' "$(_tmp_file hdr_static "$1")" "$(_tmp_file hdr_static "$2")" | awk '
        NF != 4 || $1 != $3 { print "fields differ: " $0; exit }
        { d = $2 - $4; if (d < 0) d = -d
          tol = ($1 ~ /_x$|_y$/) ? 1 / 65536 : ($1 == "min_luminance") ? 1 / 16384 : 0.5
          if (d > tol) { printf "%s is %s, the source %s", $1, $2, $4; exit } }')
    [ -z "$diff" ] && [ -n "$(hdr_static "$2")" ] || fail "HDR static metadata of $1: ${diff:-source has none}"
}

# -- Test lifecycle ------------------------------------------------------------

# Call at the end of every test case.
test_done() {
    [ "$_DONE" -eq 0 ] || return 0
    _DONE=1
    docker rm -f "$_TOOLS" >/dev/null 2>&1
    rm -rf "$_SCRATCH" $_CLEANUP
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

# A suite that dies before its last line has run only part of its assertions, and
# test_done's own exit code would report that as a pass.
_on_exit() {
    [ "$_DONE" -eq 1 ] || fail "suite ended before test_done"
    test_done
}

trap _on_exit EXIT
