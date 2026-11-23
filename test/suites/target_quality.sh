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
assert_log_contains "gpus: all for NVIDIA"
assert_file_not_exists "$O/test.mkv"
assert_log_contains "retrying on the next scan"
assert_file_not_exists "$O/.avet_test/.failed"

search() { # NAME SOURCE [TARGET_QUALITY_LINES] [AVET_LINES]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$2"
    cp "$SRC" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\n[avet]\nkeep_temp = true\n%b[target_quality]\njod = 9.0\nmin_crf = 10\nmax_probes = 3\n%b' \
        "${4:-}" "${3:-}" > "$I/p/encode.toml"
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

# a scaled encode is held against the scaled picture, which carries the tags of the encode
search scaled "$WORKDIR/bt2020.mkv" '' 'scale = 90\n'
assert_log_contains "FFVship reads the source as PQ, display 160x90 HDR"
assert_log_contains "chunk 00001 target crf"
assert_frames_match "$O/test.mkv" "$SRC" "scale=160:90:flags=lanczos" 20
grep -q '"measured"' "$O/.avet_test/tq.json" 2>/dev/null || fail "scaled: the finished chunk was not measured"
[ -z "$(ls "$O/.avet_test" | grep -E '^(probe_|cvvdp_|cambi_|reference_)')" ] || fail "scaled: probe files were left behind"

# and CAMBI reads its reference from the same file; libvmaf returns no CAMBI below some 216 columns
$FF -f lavfi -i "testsrc2=size=640x360:rate=24" -frames:v 24 -pix_fmt yuv420p -c:v ffv1 "$WORKDIR/sdr360.mkv"
search scaled_cambi "$WORKDIR/sdr360.mkv" 'max_cambi = 20\n' 'scale = 180\n'
assert_log_contains "display 320x180 SDR"
assert_log_contains ", CAMBI "

# a mirrored encode is held against the mirrored picture
$FF -display_hflip -i "$WORKDIR/sdr.mkv" -c copy "$WORKDIR/hflip.mkv"
search mirrored "$WORKDIR/hflip.mkv"
assert_log_contains "the display matrix mirrors the picture"
assert_log_not_contains "floor unreachable"
assert_frames_match "$O/test.mkv" "$SRC" null 20
JOD=$(printf '%s\n' "$RUN_LOGS" | grep -o 'lowest JOD [0-9.]*' | tail -n 1 | cut -d ' ' -f 3)
awk -v jod="${JOD:-0}" 'BEGIN { exit !(jod >= 8.5) }' || fail "mirrored: the finished chunk scored JOD '$JOD'"
[ -z "$(ls "$O/.avet_test" | grep -E '^(probe_|cvvdp_|cambi_|reference_)')" ] || fail "mirrored: probe files were left behind"

# an untagged source over 650 rows cropped to fewer: the finished chunk is measured
$FF -f lavfi -i "testsrc2=size=192x536:rate=24" -frames:v 24 -vf "pad=192:720:0:92:black,format=yuv420p" -c:v ffv1 "$WORKDIR/hd_bars.mkv"
search crop_hd "$WORKDIR/hd_bars.mkv" '' 'crop = true\n'
assert_log_contains "display 192x536 SDR"
assert_log_not_contains "not measured"
grep -q '"measured"' "$O/.avet_test/tq.json" 2>/dev/null || fail "crop_hd: the finished chunk was not measured"
[ -z "$(ls "$O/.avet_test" | grep -E '^(probe_|cvvdp_|cambi_)')" ] || fail "crop_hd: probe files were left behind"

# only the chunk that starts near the end of the file is held against a reference
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 160 -pix_fmt yuv420p -c:v libx264 -preset veryfast -crf 14 -bf 3 \
    -x264-params scenecut=0:keyint=48 "$WORKDIR/gop.mkv"
search 830 "$WORKDIR/gop.mkv" '' '[scene_detection]\nextra_split = 72\n'
assert_log_contains "chunk 00003 starts near the end of the file"
assert_log_not_contains "chunk 00001 starts near the end"
assert_log_not_contains "floor unreachable"
assert_video_frames "$O/test.mkv" 160
assert_frames_match "$O/test.mkv" "$SRC" null 20
[ -z "$(ls "$O/.avet_test" | grep -E '^(probe_|cvvdp_|cambi_|reference_)')" ] || fail "near the end: probe files were left behind"

# a chunk whose frames grow past the source's first size is held against a reference, which FFVship can read
X="-c:v libx264 -preset veryfast -crf 14 -bf 3 -pix_fmt yuv420p -x264-params scenecut=0:keyint=100:repeat-headers=1 -f mpegts"
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 48 $X -output_ts_offset 0 "$WORKDIR/grow_a.ts"
$FF -f lavfi -i "testsrc2=size=640x360:rate=24" -frames:v 122 $X -output_ts_offset 2.5 "$WORKDIR/grow_b.ts"
I="$WORKDIR/831/in"; O="$WORKDIR/831/out"; mkdir -p "$I/p" "$O"
cat "$WORKDIR/grow_a.ts" "$WORKDIR/grow_b.ts" > "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\n[avet]\nkeep_temp = true\n[scene_detection]\nextra_split = 48\n[target_quality]\njod = 9.0\nmin_crf = 10\nmax_probes = 2\n' > "$I/p/encode.toml"
TEST_SOFTWARE_GPU=1 run_avet "$I" "$O" "$O/test.mkv" 900 || fail "size change: no output"
assert_log_contains "chunk 00002 has frames of another size"
assert_video_frames "$O/test.mkv" 170
assert_file_not_exists "$O/.avet_test/.failed"

measured_1() { sed -n '/"00001"/,/}/p' "$O/.avet_test/tq.json" | grep -q '"measured"'; }
resume() { # NAME SOURCE [AVET_LINES]: avet stops once chunk 1 is done and starts again
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    cp "$2" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\n[avet]\nkeep_temp = true\n%b[scene_detection]\nextra_split = 24\n[target_quality]\njod = 9.0\nmin_crf = 10\nmax_probes = 3\n' \
        "${3:-}" > "$I/p/encode.toml"
    TEST_SOFTWARE_GPU=1 start_avet "$I" "$O" 999999
    n=0
    until grep -q '"00001"' "$O/.avet_test/done.json" 2>/dev/null || [ $n -ge 3000 ]; do sleep 0.1; n=$((n + 1)); done
    kill_avet
    grep -q '"00001"' "$O/.avet_test/done.json" || fail "$1: chunk 1 never finished"
    measured_1 && fail "$1: chunk 1 was measured before avet was stopped"
    TEST_SOFTWARE_GPU=1 run_avet "$I" "$O" "$O/test.mkv" 600 || fail "$1: no output"
    assert_log_not_contains "chunk 00001 using cached target crf"
}

# a scaled chunk finished but not yet measured when avet stops is measured against its reference on the next start
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 48 -pix_fmt yuv420p -c:v ffv1 "$WORKDIR/sdr48.mkv"
resume resume "$WORKDIR/sdr48.mkv" 'scale = 90\n'
assert_log_contains "chunk 00001 already done, measuring it"
assert_log_not_contains "under the floor"
measured_1 || fail "resume: chunk 1 is still not measured"

# a deinterlaced chunk, which has no reference to be held against, is not measured on the next start either
$FF -f lavfi -i "testsrc2=size=320x180:rate=50" -frames:v 48 -vf "tinterlace=mode=interleave_top,setfield=tff,format=yuv420p" \
    -c:v libx264 -qp 0 -preset ultrafast -flags +ildct+ilme -x264opts tff=1 "$WORKDIR/int48.mkv"
resume resume_deinterlaced "$WORKDIR/int48.mkv"
[ "$(cat "$O/.avet_test/interlace.cache" 2>/dev/null)" = tff ] || fail "resume_deinterlaced: not detected as top field first"
assert_log_not_contains "already done, measuring it"

# RGB reaches the encoder as 4:2:0 YUV, which costs it the floor, and is still compared as the same picture
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 24 -c:v libx264rgb -qp 0 -preset ultrafast "$WORKDIR/rgb.mkv"
[ "$(stream_value "$WORKDIR/rgb.mkv" v:0 stream=pix_fmt)" = gbrp ] || fail "rgb: the source is not planar RGB"
search rgb "$WORKDIR/rgb.mkv"
assert_log_contains "chunk 00001 probe 1/3 crf"
assert_log_contains "floor unreachable"
assert_log_not_contains "two different pictures"
assert_log_not_contains "not measured"
grep -q '"measured"' "$O/.avet_test/tq.json" 2>/dev/null || fail "rgb: the finished chunk was not measured"

# an option the encoder refuses fails the job when the probes encode from the scaled reference
I="$WORKDIR/refused/in"; O="$WORKDIR/refused/out"; mkdir -p "$I/p" "$O"
cp "$WORKDIR/sdr.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\nfilm-grain = 99\n[avet]\nscale = 90\n[target_quality]\njod = 9.0\n' > "$I/p/encode.toml"
TEST_SOFTWARE_GPU=1 run_avet_timed "$I" "$O" 120 "job failed"
assert_file_exists     "$O/.avet_test/.failed"
assert_file_not_exists "$O/test.mkv"

# a disk out of inodes when a score file is due is waited out, for FFVship and for vmaf
score_disk_full() { # NUMBER TOOL_BEFORE_IT [TARGET_QUALITY_LINES]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; W="$WORKDIR/$1/wrap"; mkdir -p "$I/p" "$O" "$W"
    cp "$WORKDIR/sdr.mkv" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\n[target_quality]\njod = 5.0\nmin_crf = 10\nmax_probes = 2\n%b' \
        "${3:-}" > "$I/p/encode.toml"
    touch "$W/fill"
    cat > "$W/$2" << 'EOF'
#!/bin/sh
"/usr/local/bin/$(basename "$0")" "$@"
rc=$?
case " $* " in *" --json "*|*" -b "*)
    if [ -e /wrap/fill ]; then
        rm /wrap/fill
        # tmpfs gives an unlinked file's inode back at its last close.
        for a; do
            if [ "$prev" = --json ]; then sleep 600 < "$a" > /dev/null 2>&1 & fi
            prev=$a
        done
        n=0
        while touch "/full/.filler$n" 2>/dev/null; do n=$((n + 1)); done
    fi ;;
esac
exit $rc
EOF
    chmod +x "$W/$2"
    TEST_SOFTWARE_GPU=1 start_avet "$I" "$O" 1 --tmpfs /full:size=64m,nr_inodes=512,mode=1777 -e OUTPUT_DIR=/full \
        -v "$W:/wrap:z" --entrypoint sh "$TEST_IMAGE" -c 'cp /usr/local/bin/avet /wrap/avet && exec /wrap/avet'
    wait_for_log "job failed" 300 || fail "$1: the score file never ran out of space"
    if docker exec "$AVET_CID" test -e /full/.avet_test/.failed; then
        kill_avet
        fail "$1: a full disk wrote a .failed marker"
        return
    fi
    assert_log_contains "job failed - retrying"
    docker exec "$AVET_CID" sh -c 'rm -f /full/.filler*'
    wait_for_log "[test] done" 300 || fail "$1: the job did not finish once there was space"
    kill_avet
    assert_log_not_contains "source kept"
}
score_disk_full 500 SvtAv1EncApp
score_disk_full 501 FFVship 'max_cambi = 20\n'

docker run --rm --entrypoint vmaf "$TEST_IMAGE" --version >/dev/null 2>&1 || \
    fail "bundled vmaf does not start"

# a cut whose first packets have no time keeps the size cap
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 96 -c:v libx265 -crf 22 \
    -x265-params "log-level=error:keyint=24:min-keyint=24:bframes=4:open-gop=1:scenecut=0" "$WORKDIR/open_gop.mkv" 2>/dev/null
$FF -ss 1.5 -i "$WORKDIR/open_gop.mkv" -c copy "$WORKDIR/cut.mkv"
ffprobe -v error -select_streams v:0 -show_entries packet=pts -of csv=p=0 "$WORKDIR/cut.mkv" | grep -q N/A \
    || fail "cut: every packet has a time"
TEST_SCENE_REPAIR=1
search 892 "$WORKDIR/cut.mkv" 'max_encoded_percent = 1\n'
unset TEST_SCENE_REPAIR
assert_log_not_contains "size cap disabled"
assert_log_contains     "over max_encoded_percent"

test_done
