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

# the same pictures, still in sync where mkvmerge sets its own time zero
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

# mkvmerge drops MJPEG from MP4 and stores FFV1 from MOV under an ID ffmpeg cannot read
uncopyable() { # NUMBER CODEC EXTENSION
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    ffmpeg -y -hide_banner -loglevel error -i "$MASTER" -map 0:v -map 0:a:0 -frames:v 24 \
        -c:v "$2" -c:a aac "$I/p/test.$3" || fail "could not build the $2 fixture"
    printf '%b' "$COPY" > "$I/p/encode.toml"
    run_avet_timed "$I" "$O" 60 "job failed"
    assert_log_contains    "mkvmerge could not copy the $2 video"
    assert_file_not_exists "$O/test.mkv"
    assert_file_exists     "$O/.avet_test/.failed"
    assert_file_exists     "$I/p/test.$3"
}
uncopyable 300 mjpeg mp4
uncopyable 301 ffv1 mov

# V_QUICKTIME that ffmpeg reads back stays a finished job
I="$WORKDIR/302/in"; O="$WORKDIR/302/out"; mkdir -p "$I/p" "$O"
ffmpeg -y -hide_banner -loglevel error -i "$MASTER" -map 0:v -map 0:a:0 -frames:v 24 \
    -c:v rawvideo -pix_fmt uyvy422 -c:a pcm_s16le "$I/p/test.mov" || fail "could not build the UYVY fixture"
cp "$I/p/test.mov" "$WORKDIR/302/source.mov"
printf '%b' "$COPY" > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "uyvy: no output"
assert_frames_identical "$O/test.mkv" "$WORKDIR/302/source.mov"

# a cover picture is not the copied video
I="$WORKDIR/303/in"; O="$WORKDIR/303/out"; mkdir -p "$I/p" "$O"
ffmpeg -y -hide_banner -loglevel error -f lavfi -i "color=red:s=64x64" -frames:v 1 "$WORKDIR/303/cover.jpg"
ffmpeg -y -hide_banner -loglevel error -i "$MASTER" -i "$WORKDIR/303/cover.jpg" -map 0:v -map 0:a:0 -map 1:v -frames:v:0 24 \
    -c:v:0 mjpeg -c:a aac -c:v:1 copy -disposition:v:1 attached_pic "$I/p/test.mp4" || fail "could not build the cover fixture"
printf '%b' "$COPY" > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 60 "job failed"
assert_log_contains    "mkvmerge could not copy the mjpeg video"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mp4"

# with no other track the failure still names the video
I="$WORKDIR/304/in"; O="$WORKDIR/304/out"; mkdir -p "$I/p" "$O"
ffmpeg -y -hide_banner -loglevel error -i "$MASTER" -map 0:v -frames:v 24 -c:v mjpeg "$I/p/test.mp4" || fail "could not build the MJPEG fixture"
printf '%b' "$COPY" > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 60 "job failed"
assert_log_contains    "mkvmerge could not copy the mjpeg video"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mp4"

test_done
