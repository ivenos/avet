#!/bin/sh
# avet integration test suite - single entry point.
#
# Builds the Docker image, generates fixtures in a temp dir, runs all test
# cases. The fixtures directory is automatically removed on exit.
#
# Usage:
#   ./test/run.sh                    full run: build + fixtures + tests
#   ./test/run.sh --no-build         reuse existing image
#   ./test/run.sh --verbose          print container logs on failure
#   ./test/run.sh audio              filter: only tests matching "audio"
#
# Environment:
#   TEST_IMAGE       Docker image tag (default: avet:test)

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
CASES_DIR="$SCRIPT_DIR/suites"

export TEST_IMAGE="${TEST_IMAGE:-avet:test}"
export VERBOSE=0

NO_BUILD=0
FILTER=""

for arg in "$@"; do
    case "$arg" in
        --no-build) NO_BUILD=1 ;;
        --verbose)  export VERBOSE=1 ;;
        -h|--help)
            sed -n '2,15p' "$0" | sed 's/^# //; s/^#//'
            exit 0 ;;
        # Anything else is a filter, but a mistyped flag must not become one: it would
        # match no suite and the run would report success without testing anything.
        -*)         printf "ERROR: unknown option %s\n" "$arg" >&2; exit 2 ;;
        *)          FILTER="$arg" ;;
    esac
done

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

# -- 1. Build image ------------------------------------------------------------

if [ "$NO_BUILD" -eq 0 ]; then
    printf "=== Building %s ===\n" "$TEST_IMAGE"
    docker build -t "$TEST_IMAGE" "$ROOT_DIR" || exit 1
elif ! docker image inspect "$TEST_IMAGE" >/dev/null 2>&1; then
    printf "${RED}ERROR:${NC} image %s not found (drop --no-build or build manually)\n" "$TEST_IMAGE"
    exit 1
fi

# -- 2. Generate fixtures into a temp dir --------------------------------------

FIXTURES_DIR=$(mktemp -d)
trap 'rm -rf "$FIXTURES_DIR"' EXIT
export FIXTURES_DIR

printf "\n=== Generating fixtures ===\n"

docker run --rm -i \
    --user "$(id -u):$(id -g)" \
    -v "${FIXTURES_DIR}:/out:z" \
    --entrypoint sh \
    "$TEST_IMAGE" << 'GEN'
set -e
cd /out
FF="ffmpeg -y -hide_banner -loglevel error"

echo "  sdr_noaudio.mkv"
$FF -f lavfi -i "color=c=darkorange:size=640x360:rate=24" \
    -t 10 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    sdr_noaudio.mkv

echo "  sdr_simple.mkv"
$FF -f lavfi -i "color=c=darkblue:size=640x360:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -t 10 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 96k -metadata:s:a:0 language=eng \
    sdr_simple.mkv

echo "  sdr_blackbars.mkv"
$FF -f lavfi -i "color=c=white:size=640x360:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -t 10 -vf "pad=640:480:0:60:black" \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 96k \
    sdr_blackbars.mkv

echo "  sdr_720p.mkv"
$FF -f lavfi -i "color=c=darkgreen:size=1280x720:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -t 10 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 96k \
    sdr_720p.mkv

echo "  sdr_multiaudio.mkv"
$FF -f lavfi -i "color=c=purple:size=640x360:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -f lavfi -i "sine=frequency=880:sample_rate=48000" \
    -f lavfi -i "sine=frequency=220:sample_rate=48000" \
    -t 10 -map 0:v -map 1:a -map 2:a -map 3:a \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a:0 aac -b:a:0 96k  -metadata:s:a:0 language=eng \
    -c:a:1 ac3 -b:a:1 192k -metadata:s:a:1 language=deu \
    -c:a:2 aac -b:a:2 96k  -metadata:s:a:2 language=jpn \
    sdr_multiaudio.mkv

echo "  sdr_untagged.mp4"
# MP4, so the untagged audio track comes back from ffprobe as language "und" rather
# than as no tag at all. A whitelist must not strip it: that would leave a finished
# file with no audio, and the container is the only difference from the mkv case.
$FF -f lavfi -i "color=c=teal:size=640x360:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -t 10 -map 0:v -map 1:a \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 96k \
    sdr_untagged.mp4

echo "  sdr_71audio.mkv"
$FF -f lavfi -i "color=c=navy:size=640x360:rate=24" \
    -f lavfi -i "anullsrc=channel_layout=7.1:sample_rate=48000" \
    -t 10 -map 0:v -map 1:a \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a flac \
    sdr_71audio.mkv

echo "  sdr_named_audio.mkv"
$FF -f lavfi -i "color=c=teal:size=640x360:rate=24" \
    -f lavfi -i "anullsrc=channel_layout=5.1:sample_rate=48000" \
    -t 10 -map 0:v -map 1:a \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a ac3 -metadata:s:a:0 title="Deutsch Dolby Digital 5.1" -metadata:s:a:0 language=deu \
    sdr_named_audio.mkv

echo "  sdr_subtitles.mkv"
printf "1\n00:00:01,000 --> 00:00:04,000\nEnglish subtitle text.\n\n" > /tmp/eng.srt
printf "1\n00:00:01,000 --> 00:00:04,000\nDeutscher Untertiteltext.\n\n" > /tmp/deu.srt
$FF -f lavfi -i "color=c=darkcyan:size=640x360:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -i /tmp/eng.srt -i /tmp/deu.srt \
    -t 10 -map 0:v -map 1:a -map 2:s -map 3:s \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 96k \
    -c:s:0 srt -metadata:s:s:0 language=eng \
    -c:s:1 srt -metadata:s:s:1 language=deu \
    sdr_subtitles.mkv
rm -f /tmp/eng.srt /tmp/deu.srt

echo "  sdr_long.mkv"
$FF -f lavfi -i "testsrc2=size=640x360:rate=24" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -t 60 -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 96k \
    sdr_long.mkv

echo "  sdr_vfr.mkv"
$FF -f lavfi -i "testsrc2=size=320x240:rate=60" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000" \
    -t 4 -vf "select='gte(t\,2)+not(mod(n\,2))'" -fps_mode vfr \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -b:a 96k \
    sdr_vfr.mkv

echo "  hdr10.mkv"
$FF -f lavfi -i "color=c=gray:size=1280x720:rate=24" \
    -t 10 \
    -vf "format=yuv420p10le,setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc" \
    -c:v ffv1 \
    hdr10.mkv

echo "  hlg.mkv"
$FF -f lavfi -i "color=c=gray:size=1280x720:rate=24" \
    -t 10 \
    -vf "format=yuv420p10le,setparams=range=tv:color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc" \
    -c:v ffv1 \
    hlg.mkv
GEN

GEN_RC=$?
if [ "$GEN_RC" -ne 0 ]; then
    printf "${RED}ERROR:${NC} fixture generation failed (rc=%d)\n" "$GEN_RC"
    exit 1
fi

# -- 3. Run test cases ---------------------------------------------------------

PASS=0; FAIL=0; SKIP=0

printf "\n=== avet Integration Test Suite ===\n\n"

for case_script in "$CASES_DIR"/*.sh; do
    [ -f "$case_script" ] || continue
    name=$(basename "$case_script" .sh)

    if [ -n "$FILTER" ] && ! echo "$name" | grep -qF "$FILTER"; then
        SKIP=$((SKIP + 1))
        continue
    fi

    printf "  %-44s" "$name"
    output=$(sh "$case_script" 2>&1)
    rc=$?

    if [ "$rc" -eq 0 ]; then
        printf "${GREEN}PASS${NC}\n"
        PASS=$((PASS + 1))
    else
        printf "${RED}FAIL${NC}\n"
        FAIL=$((FAIL + 1))
        echo "$output" | while IFS= read -r line; do
            printf "    %s\n" "$line"
        done
    fi
done

# -- 4. Summary ----------------------------------------------------------------

printf "\n"
if [ "$SKIP" -gt 0 ]; then
    printf "=== Summary: ${GREEN}%d passed${NC}, ${RED}%d failed${NC}, ${YELLOW}%d skipped${NC} ===\n" \
        "$PASS" "$FAIL" "$SKIP"
else
    printf "=== Summary: ${GREEN}%d passed${NC}, ${RED}%d failed${NC} ===\n" "$PASS" "$FAIL"
fi
printf "\n"

# A run that executed nothing is not a passing run.
if [ "$((PASS + FAIL))" -eq 0 ]; then
    printf "${RED}ERROR:${NC} no test case ran"
    [ -n "$FILTER" ] && printf " (filter '%s' matched nothing)" "$FILTER"
    printf "\n"
    exit 2
fi

[ "$FAIL" -eq 0 ] && exit 0 || exit 1
