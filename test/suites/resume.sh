#!/bin/sh
# Interrupted jobs: whatever a kill leaves behind, the finished file is the same.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

SRC="$FIXTURES_DIR/pattern.mkv"

setup() { # NAME CRF
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    cp "$SRC" "$I/p/test.mkv"
    write_profile "$2"
}

write_profile() { # CRF
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 4\ncrf = %s\n[scene_detection]\nextra_split = 24\n' "$1" > "$I/p/encode.toml"
}

assert_intact() {
    assert_frames_match      "$O/test.mkv" "$SRC"
    assert_frame_times_match "$O/test.mkv" "$SRC"
    assert_av_sync           "$O/test.mkv" "$SRC" 0
    assert_av_sync           "$O/test.mkv" "$SRC" 1
    assert_decodes_cleanly   "$O/test.mkv"
    assert_same_bytes        "$I/processed/test.mkv" "$SRC"
}

killed_after() { # LOG_PATTERN
    run_avet_timed "$I" "$O" 180 "$1"
    assert_log_contains "$1"
    assert_file_not_exists "$O/test.mkv"
    [ -s "$O/.avet_test/done.json" ] || fail "no chunk was finished before the kill"
}

# SVT-AV1 is deterministic, so a resumed encode has to match an uninterrupted one bit for bit.
setup clean 40
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "clean: no output"
CLEAN="$O/test.mkv"

# -- killed mid-encode: the next run keeps what was done and finishes the rest ---------
setup plain 40
killed_after "chunk 2/"
TEST_RUST_LOG=debug run_avet "$I" "$O" "$O/test.mkv" 300 || fail "plain: no output after the restart"
assert_log_contains "already done"
assert_intact
assert_packets_identical "$O/test.mkv" v:0 "$CLEAN" v:0

# -- a finished chunk cut short and half-written leftovers do not reach the output ------
setup damaged 40
killed_after "chunk 3/"
chunk=$(grep -o '"[0-9]\{5\}"' "$O/.avet_test/done.json" | head -n 1 | tr -d '"')
[ -n "$chunk" ] || fail "damaged: done.json lists no finished chunk"
truncate -s 200 "$O/.avet_test/chunks/$chunk.ivf"
printf 'not a video' > "$O/.avet_test/video.ivf"
printf 'not a video' > "$O/.avet_test/muxed.mkv"
printf 'not an index' > "$O/.avet_test/frame-index.ffindex"
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "damaged: no output after the restart"
assert_log_contains "indexing again"
assert_intact
assert_packets_identical "$O/test.mkv" v:0 "$CLEAN" v:0

# -- a profile change between the runs throws the old chunks away ----------------------
setup changed 45
killed_after "chunk 2/"
write_profile 40
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "changed: no output after the restart"
assert_log_contains "encode profile changed"
assert_intact
assert_packets_identical "$O/test.mkv" v:0 "$CLEAN" v:0

test_done
