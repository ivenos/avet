#!/bin/sh
# avet integration test suite - single entry point.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
CASES_DIR="$SCRIPT_DIR/suites"

export TEST_IMAGE="${TEST_IMAGE:-avet:test}"
export TOOLS_IMAGE="${TOOLS_IMAGE:-avet:fixture-tools}"
export VERBOSE=0

NO_BUILD=0
FILTER=""
JOBS=$(( $(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2) / 2 ))
[ "$JOBS" -ge 1 ] || JOBS=1

while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) NO_BUILD=1 ;;
        --verbose)  export VERBOSE=1 ;;
        -j)         JOBS="${2:-}"; [ $# -lt 2 ] || shift ;;
        -h|--help)
            cat << 'EOF'
Builds the Docker images, generates fixtures in a temp dir, runs all test
cases. The fixtures directory is automatically removed on exit.

Usage:
  ./test/run.sh                    full run: build + fixtures + tests
  ./test/run.sh --no-build         reuse existing images
  ./test/run.sh --verbose          print container logs on failure
  ./test/run.sh -j 4               run at most 4 suites at once (default: half the CPU cores)
  ./test/run.sh audio              filter: only tests matching "audio"

Environment:
  TEST_IMAGE       Docker image tag (default: avet:test)
  TOOLS_IMAGE      Fixture tools image tag (default: avet:fixture-tools)
EOF
            exit 0 ;;
        # A mistyped flag as the filter would match no suite and pass.
        -*)         printf "ERROR: unknown option %s\n" "$1" >&2; exit 2 ;;
        *)          [ -z "$FILTER" ] || { printf "ERROR: only one filter, got '%s' and '%s'\n" "$FILTER" "$1" >&2; exit 2; }
                    FILTER="$1" ;;
    esac
    shift
done

case "$JOBS" in
    ''|*[!0-9]*|0) printf "ERROR: -j needs a number of at least 1, got '%s'\n" "$JOBS" >&2; exit 2 ;;
esac

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

if [ "$NO_BUILD" -eq 0 ]; then
    printf "=== Building %s and %s ===\n" "$TEST_IMAGE" "$TOOLS_IMAGE"
    docker build -t "$TEST_IMAGE" "$ROOT_DIR" || exit 1
    # From stdin: with test/ as the context, docker would tar test/local/ along with it.
    docker build -t "$TOOLS_IMAGE" - < "$SCRIPT_DIR/tools.Dockerfile" || exit 1
else
    for image in "$TEST_IMAGE" "$TOOLS_IMAGE"; do
        if ! docker image inspect "$image" >/dev/null 2>&1; then
            printf "${RED}ERROR:${NC} image %s not found (drop --no-build or build manually)\n" "$image"
            exit 1
        fi
    done
fi

FIXTURES_DIR=$(mktemp -d)
RESULTS=$(mktemp -d)
AVET_TEST_RUN=$$
trap 'rm -rf "$FIXTURES_DIR" "$RESULTS"; docker rm -f $(docker ps -aq --filter "label=avet-test-tools=$AVET_TEST_RUN") >/dev/null 2>&1' EXIT
# dash skips the EXIT trap when a signal ends the shell.
trap 'exit 130' INT TERM
export FIXTURES_DIR RESULTS CASES_DIR GREEN RED NC AVET_TEST_RUN

printf "\n=== Generating fixtures ===\n"
sh "$SCRIPT_DIR/fixtures.sh" || exit 1

SKIP=0
for case_script in "$CASES_DIR"/*.sh; do
    [ -f "$case_script" ] || continue
    name=$(basename "$case_script" .sh)
    if [ -n "$FILTER" ] && ! echo "$name" | grep -qF "$FILTER"; then
        SKIP=$((SKIP + 1))
    else
        echo "$name"
    fi
done > "$RESULTS/suites"

printf "\n=== avet Integration Test Suite (%s at once) ===\n\n" "$JOBS"

xargs -P "$JOBS" -I {} sh -c '
    start=$(date +%s)
    sh "$CASES_DIR/$1.sh" > "$RESULTS/$1.out" 2>&1
    echo $? > "$RESULTS/$1.rc"
    if [ "$(cat "$RESULTS/$1.rc")" -eq 0 ]; then result="${GREEN}PASS${NC}"; else result="${RED}FAIL${NC}"; fi
    printf "  %-40s%b %4ss\n" "$1" "$result" "$(( $(date +%s) - start ))"
' _ {} < "$RESULTS/suites"

PASS=0; FAIL=0
while read -r name; do
    if [ "$(cat "$RESULTS/$name.rc" 2>/dev/null)" = 0 ]; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        printf "\n${RED}FAIL${NC} %s\n" "$name"
        while IFS= read -r line; do
            printf "    %s\n" "$line"
        done < "$RESULTS/$name.out"
    fi
done < "$RESULTS/suites"

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
