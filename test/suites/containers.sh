#!/bin/sh
# Containers and GOP structures, checked against the lossless pattern each fixture was made from.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

MASTER="$FIXTURES_DIR/pattern.mkv"
ENCODE='encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 40\n[scene_detection]\nextra_split = 24\n'

run() { # NAME FIXTURE PROFILE
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf '%b' "$3" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 300 || fail "$1: no output"
}

# chunks start inside open GOPs, B-frame pyramids and CRA pictures, in every container
for fixture in gop_h264.mkv gop_h264.mp4 gop_h264.ts gop_h264.flv gop_h264.mov gop_hevc.ts \
               gop_mpeg2.m2ts gop_mpeg4.avi gop_vp9.webm; do
    run "$fixture" "$fixture" "$ENCODE"
    assert_frames_match      "$O/test.mkv" "$MASTER"
    assert_frame_times_match "$O/test.mkv" "$MASTER"
    assert_av_sync           "$O/test.mkv" "$MASTER" 0
    assert_decodes_cleanly   "$O/test.mkv"
    [ "$fixture" != gop_mpeg4.avi ] || assert_log_contains "AVI with B-frames: working from a Matroska copy of the video"
done

# an MP4 cut behind a keyframe: what the edit list hides stays hidden
# The cut starts the sound at 3.3 s and the picture at frame 80, 33 ms apart, as in the source.
run cut gop_cut.mp4 "$ENCODE"
assert_frames_match      "$O/test.mkv" "$FIXTURES_DIR/pattern_from80.mkv"
assert_frame_times_match "$O/test.mkv" "$FIXTURES_DIR/pattern_from80.mkv"
assert_av_sync           "$O/test.mkv" "$SRC" 0
assert_subtitle_events_match "$O/test.mkv" s:0 "$SRC" s:0

# an MPEG-TS whose timestamps wrap a minute in
run wrap gop_wrap.m2ts "$ENCODE"
assert_frames_match      "$O/test.mkv" "$FIXTURES_DIR/long.mkv"
assert_frame_times_match "$O/test.mkv" "$FIXTURES_DIR/long.mkv"
assert_av_sync           "$O/test.mkv" "$FIXTURES_DIR/long.mkv" 0
assert_subtitle_events_match "$O/test.mkv" s:0 "$SRC" s:0

# a subtitle that starts before picture and sound leaves all three in step
E="$WORKDIR/310"; mkdir -p "$E"
printf '1\n00:00:03,000 --> 00:00:03,500\nEarly.\n\n2\n00:00:07,000 --> 00:00:07,500\nInside.\n\n' > "$E/early.srt"
mkvmerge -q -o "$E/srt.mkv" --sync 0:5000 --sync 1:5000 "$FIXTURES_DIR/gop_h264.mkv" "$E/early.srt"
mkvmerge -q -o "$E/pgs.mkv" --sync 0:5000 --sync 1:5000 "$FIXTURES_DIR/gop_h264.mkv" \
    --no-video --no-audio --no-chapters --no-attachments --no-global-tags \
    --subtitle-tracks 2 --sync 2:2000 "$FIXTURES_DIR/subs_bitmap.mkv"
for ext in mp4 mov; do
    ffmpeg -y -hide_banner -loglevel error -itsoffset 5 -i "$FIXTURES_DIR/gop_h264.mp4" -i "$E/early.srt" \
        -map 0:v -map 0:a -map 1 -c:v copy -c:a copy -c:s mov_text "$E/text.$ext" || fail "could not build the early $ext"
done
for source in srt.mkv pgs.mkv text.mp4 text.mov; do
    for mode in copy encode; do
        I="$E/$source.$mode/in"; O="$E/$source.$mode/out"; mkdir -p "$I/p" "$O"
        cp "$E/$source" "$I/p/test.${source##*.}"
        if [ "$mode" = copy ]; then profile='[avet]\nvideo = "copy"\n'; else profile=$ENCODE; fi
        printf '%b' "$profile" > "$I/p/encode.toml"
        run_avet "$I" "$O" "$O/test.mkv" 300 || fail "early $source $mode: no output"
        assert_av_sync "$O/test.mkv" "$E/$source" 0
        assert_subtitle_events_match "$O/test.mkv" s:0 "$E/$source" s:0 from_picture
    done
done

assert_reaching_cues() { # LABEL
    events=$(subtitle_events "$O/test.mkv" s:0)
    printf '%s\n' "$events" | awk '
        function near(a, b, within) { return a - b <= within && b - a <= within }
        NR == 1 { ok = near($1, 0, 0.002) && near($2, 1000, 2) }
        NR == 2 { ok = ok && near($1, 2, 0.002) && near($2, 500, 2) }
        END { exit !(ok && NR == 2) }' || fail "reaching $1: the cues are $(echo $events)"
    text=$(subtitle_text "$O/test.mkv" s:0)
    [ "$(echo $text)" = "Reaches. Inside." ] || fail "reaching $1: the text is $(echo $text)"
}

# a text cue from before the picture that reaches into it starts at the first picture, one that ends earlier stays out
E="$WORKDIR/860"; mkdir -p "$E"
printf '1\n00:00:03,000 --> 00:00:03,500\nBefore.\n\n2\n00:00:04,500 --> 00:00:06,000\nReaches.\n\n3\n00:00:07,000 --> 00:00:07,500\nInside.\n\n' > "$E/reach.srt"
mkvmerge -q -o "$E/reach.mkv" --sync 0:5000 --sync 1:5000 "$FIXTURES_DIR/gop_h264.mkv" "$E/reach.srt"
for ext in mp4 mov; do
    ffmpeg -y -hide_banner -loglevel error -itsoffset 5 -i "$FIXTURES_DIR/gop_h264.mp4" -i "$E/reach.srt" \
        -map 0:v -map 0:a -map 1 -c:v copy -c:a copy -c:s mov_text "$E/reach.$ext" || fail "could not build the reaching $ext"
done
for source in reach.mkv reach.mp4 reach.mov; do
    for mode in copy encode; do
        I="$E/$source.$mode/in"; O="$E/$source.$mode/out"; mkdir -p "$I/p" "$O"
        cp "$E/$source" "$I/p/test.${source##*.}"
        if [ "$mode" = copy ]; then profile='[avet]\nvideo = "copy"\n'; else profile=$ENCODE; fi
        printf '%b' "$profile" > "$I/p/encode.toml"
        run_avet "$I" "$O" "$O/test.mkv" 300 || fail "reaching $source $mode: no output"
        assert_av_sync "$O/test.mkv" "$E/$source" 0
        assert_reaching_cues "$source $mode"
    done
done

# with sound before the picture, the cues before the picture go and the one that reaches in starts at it
I="$E/order/in"; O="$E/order/out"; mkdir -p "$I/p" "$O"
printf '1\n00:00:02,000 --> 00:00:06,000\nReaches.\n\n2\n00:00:02,500 --> 00:00:02,800\nGone.\n\n3\n00:00:03,500 --> 00:00:03,900\nBetween.\n\n4\n00:00:07,000 --> 00:00:07,500\nInside.\n\n' > "$E/order.srt"
mkvmerge -q -o "$I/p/test.mkv" --sync 0:5000 --sync 1:3000 "$FIXTURES_DIR/gop_h264.mkv" "$E/order.srt"
cp "$I/p/test.mkv" "$E/order.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 300 || fail "reaching order: no output"
assert_av_sync "$O/test.mkv" "$E/order.mkv" 0
assert_reaching_cues order

# a transport stream with two programs brings the sound and subtitles of the first video's program
P="$WORKDIR/861"; mkdir -p "$P"
ffmpeg -y -hide_banner -loglevel error -i "$FIXTURES_DIR/subs_dvb.ts" -f lavfi -i "sine=f=300:r=48000:d=8" \
    -map 0:v -map 0:a -map 0:s:0 -map 0:v -map 1:a -map 0:s:1 -c:v copy -c:s copy -c:a mp2 -t 8 \
    -metadata:s:a:0 language=eng -metadata:s:a:1 language=fra -metadata:s:s:0 language=ger -metadata:s:s:1 language=spa \
    -program title=One:st=0:st=1:st=2 -program title=Two:st=3:st=4:st=5 -f mpegts "$P/two.ts" || fail "could not build the two-program TS"
for mode in copy encode; do
    I="$P/$mode/in"; O="$P/$mode/out"; mkdir -p "$I/p" "$O"
    cp "$P/two.ts" "$I/p/test.ts"
    if [ "$mode" = copy ]; then profile='[avet]\nvideo = "copy"\n'; else profile=$ENCODE; fi
    printf '%b' "$profile" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 300 || fail "two programs $mode: no output"
    assert_log_contains "2 audio or subtitle track(s) of another program of the transport stream left out"
    assert_audio_track_count "$O/test.mkv" 1
    assert_audio_language "$O/test.mkv" 0 eng
    assert_subtitle_track_count "$O/test.mkv" 1
    assert_subtitle_language "$O/test.mkv" 0 ger
done

test_done
