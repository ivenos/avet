#!/bin/sh
# video = copy from every container, checked against the lossless pattern each fixture was made from.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

MASTER="$FIXTURES_DIR/pattern.mkv"
COPY='[avet]\nvideo = "copy"\n'

run() { # NAME FIXTURE PROFILE
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf '%b' "$3" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 300 || fail "$1: no output"
}

# -- the same pictures, still in sync where mkvmerge sets its own time zero -----------------
for fixture in gop_h264.mp4 gop_h264.flv gop_h264.ts gop_mpeg4.avi; do
    run "copy_$fixture" "$fixture" "$COPY"
    assert_frames_identical "$O/test.mkv" "$SRC"
    assert_av_sync          "$O/test.mkv" "$MASTER" 0
done

run copy_wrap gop_wrap.m2ts "$COPY"
assert_frames_identical "$O/test.mkv" "$SRC"
assert_av_sync          "$O/test.mkv" "$FIXTURES_DIR/long.mkv" 0
assert_subtitle_events_match "$O/test.mkv" s:0 "$SRC" s:0

# The frames the edit list hides play in front, with silence under them.
run copy_cut gop_cut.mp4 "$COPY"
assert_av_sync "$O/test.mkv" "$SRC" 0

test_done
