#!/bin/sh
# Tests for target_quality. A CI runner has no GPU, so only the error path is checked
# here; the CVVDP search itself is covered by the Rust unit tests.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# No GPU: target_quality fails with a clear error, no output, no crash
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
[target_quality]
jod = 9.5
EOF
run_avet_timed "$I" "$O" 90 "requires a GPU"

# "requires a GPU" alone also covers FFVship failing to start, so a broken bundle would
# keep this green. This wording needs FFVship to have run and enumerated a device.
assert_log_contains "found only a software Vulkan device"
assert_log_contains "llvmpipe"
assert_file_not_exists "$O/test.mkv"

docker run --rm --entrypoint vmaf "$TEST_IMAGE" --version >/dev/null 2>&1 || \
    fail "bundled vmaf does not start"

test_done
