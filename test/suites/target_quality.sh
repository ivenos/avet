#!/bin/sh
# Tests for target_quality. A CI runner has no GPU: without one the job has to say so, and
# with AVET_SOFTWARE_GPU the whole search runs on the image's software device, on clips
# small enough for it.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)
FF="ffmpeg -nostdin -y -hide_banner -loglevel error"

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
assert_log_contains "retrying on the next scan"
assert_file_not_exists "$O/.avet_test/.failed"

search() { # NAME SOURCE [TARGET_QUALITY_LINES]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$2"
    cp "$SRC" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\n[avet]\nkeep_temp = true\n[target_quality]\njod = 9.0\nmin_crf = 10\nmax_probes = 3\n%b' \
        "${3:-}" > "$I/p/encode.toml"
    TEST_SOFTWARE_GPU=1 run_avet "$I" "$O" "$O/test.mkv" 600 || fail "$1: no output"
}

# the search end to end: probes, a CRF per chunk, the finished chunk measured as well
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 24 -pix_fmt yuv420p -c:v ffv1 "$WORKDIR/sdr.mkv"
search sdr "$WORKDIR/sdr.mkv"
assert_log_contains "display 320x180 SDR"
assert_log_contains "chunk 00001 probe 1/3 crf"
assert_log_contains "chunk 00001 target crf"
assert_log_contains "lowest JOD"
assert_video_frames "$O/test.mkv" 24
assert_frames_match "$O/test.mkv" "$SRC" null 20
grep -q '"crf"' "$O/.avet_test/tq.json" 2>/dev/null || fail "sdr: tq.json holds no solved CRF"
grep -q '"measured"' "$O/.avet_test/tq.json" 2>/dev/null || fail "sdr: the finished chunk was not measured"
[ -z "$(ls "$O/.avet_test" | grep -E '^(probe_|cvvdp_|cambi_)')" ] || fail "sdr: probe files were left behind"

# a CAMBI limit puts vmaf into every probe that holds the floor
search cambi "$WORKDIR/sdr.mkv" 'max_cambi = 20\n'
assert_log_contains "CAMBI at most 20"
assert_log_contains ", CAMBI "

# a BT.2020 matrix without a transfer is PQ to FFVship, and the display follows what it reads
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 24 -vf "format=yuv420p10le,setparams=colorspace=bt2020nc" \
    -c:v ffv1 "$WORKDIR/bt2020.mkv"
search bt2020 "$WORKDIR/bt2020.mkv"
assert_log_contains "display 320x180 SDR"
assert_log_contains "FFVship reads the source as PQ, display 320x180 HDR"

# RGB reaches the encoder as 4:2:0 YUV, which costs it the floor, and is still compared as the same picture
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 24 -c:v libx264rgb -qp 0 -preset ultrafast "$WORKDIR/rgb.mkv"
[ "$(stream_value "$WORKDIR/rgb.mkv" v:0 stream=pix_fmt)" = gbrp ] || fail "rgb: the source is not planar RGB"
search rgb "$WORKDIR/rgb.mkv"
assert_log_contains "chunk 00001 probe 1/3 crf"
assert_log_contains "floor unreachable"
assert_log_not_contains "two different pictures"

docker run --rm --entrypoint vmaf "$TEST_IMAGE" --version >/dev/null 2>&1 || \
    fail "bundled vmaf does not start"

test_done
