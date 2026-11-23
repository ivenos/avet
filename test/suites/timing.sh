#!/bin/sh
# Timestamps and sync: variable and fractional frame rates, stream offsets, start times.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

encode() { # NAME FIXTURE [SCENE_SPLIT]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    cat > "$I/p/encode.toml" << EOF
encoder = "svt-av1"
[encoder_params]
preset = 8
crf    = 40
[scene_detection]
extra_split = ${3:-60}
EOF
    run_avet "$I" "$O" "$O/test.mkv" 180 || fail "$1: no output"
}

# variable frame rate: every frame keeps its own timestamp
encode vfr pattern_vfr.mkv
assert_log_contains "variable frame rate"
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"

# 23.976 fps without drift, 300 fps past SvtAv1EncApp's limit of 240 and 500 past SvtAv1EncApp-hdr's of 480
encode ntsc pattern_ntsc.mkv
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"
assert_av_sync           "$O/test.mkv" "$SRC"

encode fps300 pattern_300fps.mkv
assert_log_contains "encoding at a 1/2 rate"
assert_frames_match      "$O/test.mkv" "$SRC"
assert_frame_times_match "$O/test.mkv" "$SRC"

I="$WORKDIR/fps500/in"; O="$WORKDIR/fps500/out"; mkdir -p "$I/p" "$O"
SRC="$FIXTURES_DIR/pattern_500fps.mkv"
cp "$SRC" "$I/p/test.mkv"
printf 'encoder = "svt-av1-hdr"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "fps500: no output"
assert_log_contains      "encoding at a 1/2 rate"
assert_video_frames      "$O/test.mkv" 250
assert_frame_times_match "$O/test.mkv" "$SRC"

# 119.88 fps through the filter stage, which rounds a rate near 120 on its own
I="$WORKDIR/fps119/in"; O="$WORKDIR/fps119/out"; mkdir -p "$I/p" "$O"
SRC="$FIXTURES_DIR/pattern_119.mkv"
cp "$SRC" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\nscale = 64\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "fps119: no output"
assert_video_frames      "$O/test.mkv" 1199
assert_frame_times_match "$O/test.mkv" "$SRC"


# video that starts after the audio, and audio that starts after the video
for case in video_delay audio_delay; do
    encode "$case" "pattern_$case.mkv"
    assert_frames_match "$O/test.mkv" "$SRC"
    assert_av_sync      "$O/test.mkv" "$SRC" 0
    assert_av_sync      "$O/test.mkv" "$SRC" 1
    for spec in s:0 s:1 s:2; do
        assert_stream_times_match "$O/test.mkv" "$spec" "$SRC" "$spec"
    done
    assert_chapters_match "$O/test.mkv" "$SRC"
done

# every stream five seconds into the file
encode global_offset pattern_global_offset.mkv
assert_frames_match "$O/test.mkv" "$SRC"
assert_av_sync      "$O/test.mkv" "$SRC" 0
assert_av_sync      "$O/test.mkv" "$SRC" 1
for spec in s:0 s:1 s:2; do
    assert_stream_times_match "$O/test.mkv" "$spec" "$SRC" "$spec"
done
assert_chapters_match "$O/test.mkv" "$SRC"
assert_stream_value "$O/test.mkv" v:0 stream=start_time "0.000000"
assert_log_not_contains " WARN "

# the same with a subtitle only mkvmerge reads, video passed through and encoded
SRC="$FIXTURES_DIR/pattern_offset_vtt.mkv"
for mode in copy encode; do
    I="$WORKDIR/offset_$mode/in"; O="$WORKDIR/offset_$mode/out"; mkdir -p "$I/p" "$O"
    cp "$SRC" "$I/p/test.mkv"
    case "$mode" in
        copy)   printf '[avet]\nvideo = "copy"\n' ;;
        encode) printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' ;;
    esac > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "offset $mode: no output"
    assert_av_sync "$O/test.mkv" "$SRC" 0
    vtt=$(mkvmerge -i "$O/test.mkv" | sed -n 's/^Track ID \([0-9]*\): subtitles (WebVTT).*/\1/p')
    [ -n "$vtt" ] && mkvextract "$O/test.mkv" tracks "$vtt:$WORKDIR/offset_$mode/eng.vtt" >/dev/null
    grep -q "^00:00:01.000 --> 00:00:02.500" "$WORKDIR/offset_$mode/eng.vtt" 2>/dev/null \
        || fail "offset $mode: the WebVTT cue is not 1 s after the first frame: $(cat "$WORKDIR/offset_$mode/eng.vtt" 2>/dev/null)"
done

# MPEG-TS: a start time far from zero, audio 321 ms ahead of the video or 500 ms behind it
for fixture in pattern_offset.m2ts pattern_late_audio.ts; do
    encode "ts_${fixture%%.*}" "$fixture"
    assert_frames_match "$O/test.mkv" "$SRC"
    assert_av_sync      "$O/test.mkv" "$SRC" 0
    assert_audio_codec  "$O/test.mkv" 0 aac

    I="$WORKDIR/ts_copy_${fixture%%.*}/in"; O="$WORKDIR/ts_copy_${fixture%%.*}/out"; mkdir -p "$I/p" "$O"
    cp "$SRC" "$I/p/test.${fixture##*.}"
    printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "ts copy $fixture: no output"
    assert_av_sync "$O/test.mkv" "$SRC" 0
done

# cut at an open-GOP key frame: the frames before the first picture shown are not encoded
I="$WORKDIR/200/in"; O="$WORKDIR/200/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error \
    -f lavfi -i "testsrc2=size=320x180:rate=24,drawbox=w=iw:h=ih:color=white:t=fill:enable=eq(mod(n\,48)\,12)" \
    -f lavfi -i "aevalsrc=0.5*sin(2*PI*1000*t)*lt(mod(t-0.5\,2)\,0.05)*gte(t\,0.5):c=stereo:s=48000:d=10" \
    -frames:v 240 -map 0:v -map 1:a -c:v libx265 -crf 8 \
    -x265-params "log-level=error:keyint=12:min-keyint=12:bframes=4:open-gop=1:scenecut=0" -c:a flac "$WORKDIR/200/base.mkv"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 2.2 -i "$WORKDIR/200/base.mkv" -t 6 \
    -map 0:v:0 -map 0:a:0 -c copy "$I/p/test.mkv"
OPEN_GOP="$WORKDIR/200/source.mkv"
cp "$I/p/test.mkv" "$OPEN_GOP"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n[avet]\nkeep_temp = true\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "open GOP cut: no output"
assert_log_contains "scene cuts move by"
shown=$(ffprobe -v error -select_streams v:0 -count_frames -show_entries stream=nb_read_frames \
    -of default=nw=1:nk=1 "$OPEN_GOP" | tr -d '\n')
[ "$shown" -lt "$(packet_count "$OPEN_GOP")" ] || fail "open GOP cut: the source has no leading frames"
assert_video_frames "$O/test.mkv" "$shown"
assert_av_sync      "$O/test.mkv" "$OPEN_GOP"

# a recording joined mid-GOP: the picture starts where its first decodable frame plays, not at the first packet
I="$WORKDIR/204/in"; O="$WORKDIR/204/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error \
    -f lavfi -i "testsrc2=size=320x180:rate=24,drawbox=w=iw:h=ih:color=white:t=fill:enable=eq(mod(n\,96)\,12)" \
    -f lavfi -i "aevalsrc=0.5*sin(2*PI*1000*t)*lt(mod(t-0.5\,4)\,0.05)*gte(t\,0.5):c=stereo:s=48000:d=12" \
    -frames:v 288 -map 0:v -map 1:a -c:v libx264 -preset veryfast -crf 12 -g 48 -bf 3 -sc_threshold 0 -c:a mp2 \
    -f mpegts "$WORKDIR/204/full.ts"
pos=$(ffprobe -v error -select_streams v:0 -show_entries packet=pos -of csv=p=0 "$WORKDIR/204/full.ts" | sed -n 60p | tr -d ,)
tail -c +$((pos / 188 * 188 + 1)) "$WORKDIR/204/full.ts" > "$I/p/test.ts"
cp "$I/p/test.ts" "$WORKDIR/204/source.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "mid-GOP recording: no output"
assert_log_contains "scene cuts move by"
assert_av_sync "$O/test.mkv" "$WORKDIR/204/source.ts"

# a scene list from before leading frames were left out still resumes with them
I="$WORKDIR/201/in"; O="$WORKDIR/201/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$OPEN_GOP" "$I/p/test.mkv"
all=$(packet_count "$OPEN_GOP")
printf '[{"index":0,"start_frame":0,"end_frame":%d}]\n' "$((all - 1))" > "$O/.avet_test/scenes.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "old scene list: no output"
assert_log_contains "reusing scenes.json"
assert_video_frames "$O/test.mkv" "$all"

# a scene list that starts after the leading frames is picked up again
I="$WORKDIR/205/in"; O="$WORKDIR/205/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$OPEN_GOP" "$I/p/test.mkv"
printf '[{"index":0,"start_frame":%d,"end_frame":%d}]\n' "$((all - shown))" "$((all - 1))" > "$O/.avet_test/scenes.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "scene list after the leading frames: no output"
assert_log_contains "reusing scenes.json"
assert_video_frames "$O/test.mkv" "$shown"
assert_av_sync      "$O/test.mkv" "$OPEN_GOP"

# a scene list that starts anywhere else is refused
I="$WORKDIR/206/in"; O="$WORKDIR/206/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$OPEN_GOP" "$I/p/test.mkv"
printf '[{"index":0,"start_frame":%d,"end_frame":%d}]\n' "$((all - shown + 1))" "$((all - 1))" > "$O/.avet_test/scenes.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 90 "job failed"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "gap or an overlap"

# one repeated timestamp in a variable frame rate keeps every other frame's time
I="$WORKDIR/202/in"; O="$WORKDIR/202/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 240 \
    -c:v libx264 -preset ultrafast -qp 18 -pix_fmt yuv420p "$WORKDIR/202/cfr.mkv"
awk 'BEGIN {
    print "# timestamp format v2"
    for (i = 0; i < 240; i++) { t[i] = c; c += (i < 100 || i >= 200) ? 1000 / 24 : 100 }
    t[151] = t[150]
    for (i = 0; i < 240; i++) printf "%.3f\n", t[i]
}' > "$WORKDIR/202/tc.txt"
mkvmerge -q -o "$I/p/test.mkv" --timestamps "0:$WORKDIR/202/tc.txt" "$WORKDIR/202/cfr.mkv"
cp "$I/p/test.mkv" "$WORKDIR/202/source.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 40\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "repeated timestamp: no output"
assert_log_contains     "variable frame rate"
assert_log_not_contains "not increasing"
assert_video_frames      "$O/test.mkv" 240
assert_frame_times_match "$O/test.mkv" "$WORKDIR/202/source.mkv"

# AVI with B-frames: the size limit reads the Matroska copy, whose packets all have times
I="$WORKDIR/203/in"; O="$WORKDIR/203/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 48 \
    -pix_fmt yuv420p -c:v mpeg4 -q:v 24 -bf 2 -g 24 "$I/p/test.avi"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 20\n[target_quality]\njod = 9.0\nmin_crf = 10\nmax_probes = 2\nmax_encoded_percent = 50\n' > "$I/p/encode.toml"
TEST_SOFTWARE_GPU=1 run_avet "$I" "$O" "$O/test.mkv" 600 || fail "AVI size limit: no output"
assert_log_contains     "working from a Matroska copy"
assert_log_not_contains "size cap disabled"

# MP4 cut between key frames: the size limit leaves out the packets the edit list hides
I="$WORKDIR/207/in"; O="$WORKDIR/207/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 120 \
    -c:v libx264 -preset veryfast -crf 42 -g 48 -bf 2 -pix_fmt yuv420p "$WORKDIR/207/base.mkv"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 1.2 -i "$WORKDIR/207/base.mkv" -t 3 -map 0:v:0 -c copy "$I/p/test.mp4"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 20\n[target_quality]\njod = 9.0\nmin_crf = 10\nmax_probes = 2\nmax_encoded_percent = 50\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 TEST_SOFTWARE_GPU=1 run_avet "$I" "$O" "$O/test.mkv" 600 || fail "MP4 size limit: no output"
assert_log_not_contains "size cap disabled"

# MPEG-2 in TS cut with -ss -c copy keeps its length and stays with the sound
I="$WORKDIR/208/in"; O="$WORKDIR/208/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -i "$FIXTURES_DIR/long.mkv" -t 12 -map 0:v -map 0:a \
    -c:v mpeg2video -q:v 3 -g 12 -bf 2 -flags -cgop -c:a mp2 -b:a 192k "$WORKDIR/208/base.mkv"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 2.2 -i "$WORKDIR/208/base.mkv" -t 7 -map 0:v:0 -map 0:a:0 -c copy "$I/p/test.ts"
cp "$I/p/test.ts" "$WORKDIR/208/source.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "cut TS: no output"
assert_log_contains "do not fit the packets"
span=$(frame_times "$O/test.mkv" | tail -n 1 | cut -d. -f1)
want=$(frame_times "$WORKDIR/208/source.ts" | tail -n 1 | cut -d. -f1)
[ "$((span - want))" -ge -50 ] && [ "$((span - want))" -le 50 ] || fail "cut TS: the video runs $span ms, the source $want"
assert_av_sync "$O/test.mkv" "$WORKDIR/208/source.ts"

# MPEG-2 in Matroska cut the same way: leading frames with the first picture's time stay out
I="$WORKDIR/209/in"; O="$WORKDIR/209/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 2.2 -i "$WORKDIR/208/base.mkv" -t 7 -map 0:v:0 -map 0:a:0 -c copy "$I/p/test.mkv"
shown=$(ffprobe -v error -select_streams v:0 -count_frames -show_entries stream=nb_read_frames -of csv=p=0 "$I/p/test.mkv" | tr -d ,)
cp "$I/p/test.mkv" "$WORKDIR/209/source.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "cut Matroska: no output"
assert_log_contains "scene cuts move by"
assert_video_frames "$O/test.mkv" "$shown"
assert_av_sync "$O/test.mkv" "$WORKDIR/209/source.mkv"

# a cut TS with a gap in its timestamps keeps its length
I="$WORKDIR/210/in"; O="$WORKDIR/210/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 2.2 -i "$WORKDIR/208/base.mkv" -t 3 -map 0:v:0 -map 0:a:0 -c copy "$WORKDIR/210/a.ts"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 6 -i "$WORKDIR/208/base.mkv" -t 3 -map 0:v:0 -map 0:a:0 -c copy -output_ts_offset 5 "$WORKDIR/210/b.ts"
cat "$WORKDIR/210/a.ts" "$WORKDIR/210/b.ts" > "$I/p/test.ts"
cp "$I/p/test.ts" "$WORKDIR/210/source.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "cut TS with a gap: no output"
assert_log_contains "do not fit the packets"
span=$(frame_times "$O/test.mkv" | tail -n 1 | cut -d. -f1)
want=$(frame_times "$WORKDIR/210/source.ts" | tail -n 1 | cut -d. -f1)
[ "$((span - want))" -ge -50 ] && [ "$((span - want))" -le 50 ] || fail "cut TS with a gap: the video runs $span ms, the source $want"

# a cut TS with B-frames in a small part of it keeps its length
I="$WORKDIR/211/in"; O="$WORKDIR/211/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 96 \
    -c:v mpeg2video -q:v 3 -g 12 -bf 2 -flags -cgop -f mpeg2video "$WORKDIR/211/a.m2v"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24,trim=start_frame=96,setpts=PTS-STARTPTS" -frames:v 500 \
    -c:v mpeg2video -q:v 3 -g 12 -bf 0 -f mpeg2video "$WORKDIR/211/b.m2v"
cat "$WORKDIR/211/a.m2v" "$WORKDIR/211/b.m2v" > "$WORKDIR/211/all.m2v"
ffmpeg -nostdin -y -hide_banner -loglevel error -fflags +genpts -framerate 24 -f mpegvideo -i "$WORKDIR/211/all.m2v" -c copy "$WORKDIR/211/base.mkv"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 2.2 -i "$WORKDIR/211/base.mkv" -t 20 -map 0:v:0 -c copy "$I/p/test.ts"
cp "$I/p/test.ts" "$WORKDIR/211/source.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "cut TS with few B-frames: no output"
assert_log_contains "do not fit the packets"
span=$(frame_times "$O/test.mkv" | tail -n 1 | cut -d. -f1)
want=$(frame_times "$WORKDIR/211/source.ts" | tail -n 1 | cut -d. -f1)
[ "$((span - want))" -ge -50 ] && [ "$((span - want))" -le 50 ] || fail "cut TS with few B-frames: the video runs $span ms, the source $want"

# a job begun on the source's frame numbers does not resume on the Matroska copy's
I="$WORKDIR/212/in"; O="$WORKDIR/212/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$WORKDIR/208/source.ts" "$I/p/test.ts"
printf '[{"index":0,"start_frame":0,"end_frame":%d}]\n' "$(($(packet_count "$WORKDIR/208/source.ts") - 1))" > "$O/.avet_test/scenes.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 35\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 180 || fail "job from before the copy: no output"
assert_log_contains     "do not fit the packets"
assert_log_not_contains "reusing scenes.json"
assert_av_sync "$O/test.mkv" "$WORKDIR/208/source.ts"

# MP4 whose streams all start late: the chapters move with the picture
mkdir -p "$WORKDIR/213"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -f lavfi -i "sine=frequency=440:r=48000" -t 6 \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac "$WORKDIR/213/plain.mkv"
printf ';FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=5000\nEND=7000\ntitle=First\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=7000\nEND=11000\ntitle=Second\n' > "$WORKDIR/213/chapters.txt"
ffmpeg -nostdin -y -hide_banner -loglevel error -itsoffset 5 -i "$WORKDIR/213/plain.mkv" -i "$WORKDIR/213/chapters.txt" \
    -map 0:v -map 0:a -map_chapters 1 -c copy "$WORKDIR/213/source.mp4"
for mode in copy encode; do
    I="$WORKDIR/213/$mode/in"; O="$WORKDIR/213/$mode/out"; mkdir -p "$I/p" "$O"
    cp "$WORKDIR/213/source.mp4" "$I/p/test.mp4"
    case "$mode" in
        copy)   printf '[avet]\nvideo = "copy"\n' ;;
        encode) printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' ;;
    esac > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "late MP4 $mode: no output"
    second=$(chapter_list "$O/test.mkv" | sed -n 2p | cut -d' ' -f1)
    awk -v s="${second:-99}" 'BEGIN { exit !(s >= 1.998 && s <= 2.002) }' || fail "late MP4 $mode: the second chapter starts $second s after the picture"
done

# two recordings joined with cat, the second restarting its timestamps
I="$WORKDIR/800/in"; O="$WORKDIR/800/out"; mkdir -p "$I/p" "$O"
X="-c:v libx264 -preset veryfast -qp 16 -pix_fmt yuv420p -g 48 -bf 2 -muxdelay 0"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 240 $X "$WORKDIR/800/a.ts"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24,hue=h=120" -frames:v 240 $X "$WORKDIR/800/b.ts"
cat "$WORKDIR/800/a.ts" "$WORKDIR/800/b.ts" > "$I/p/test.ts"
cp "$I/p/test.ts" "$WORKDIR/800/source.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 30\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 240 || fail "joined TS: no output"
assert_log_contains "do not fit the packets"
assert_frames_match "$O/test.mkv" "$WORKDIR/800/source.ts" null 20

# two TS parts whose timestamps overlap by two frames decode in file order
I="$WORKDIR/801/in"; O="$WORKDIR/801/out"; mkdir -p "$I/p" "$O"
X="-c:v libx264 -preset veryfast -qp 14 -pix_fmt yuv420p -g 48 -bf 2 -sc_threshold 0 -muxdelay 0"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 200 $X "$WORKDIR/801/a.ts"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24,hue=h=90" -frames:v 280 $X -output_ts_offset 8.3333 "$WORKDIR/801/b.ts"
cat "$WORKDIR/801/a.ts" "$WORKDIR/801/b.ts" > "$I/p/test.ts"
cp "$I/p/test.ts" "$WORKDIR/801/source.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 10\ncrf = 20\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 240 || fail "overlapping TS: no output"
assert_log_contains "decoding it from the start"
assert_frames_match "$O/test.mkv" "$WORKDIR/801/source.ts" null 20

# target quality measures such a source against a reference, as FFVship seeks on its own
I="$WORKDIR/802/in"; O="$WORKDIR/802/out"; mkdir -p "$I/p" "$O"
cp "$WORKDIR/801/source.ts" "$I/p/test.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 10\ncrf = 15\n[target_quality]\njod = 8.0\nmin_crf = 10\nmax_crf = 20\nmax_probes = 2\nprobe_preset = 12\n' > "$I/p/encode.toml"
TEST_SOFTWARE_GPU=1 run_avet "$I" "$O" "$O/test.mkv" 600 || fail "overlapping TS with target quality: no output"
assert_log_contains "decoding it from the start"
assert_frames_match "$O/test.mkv" "$WORKDIR/801/source.ts" null 20

# Xvid with packed B-frames; the image's ffmpeg has no libxvid, so the clip is an asset:
# -f lavfi -i testsrc=size=128x72:rate=24 -frames:v 72 -c:v libxvid -q:v 12 -g 24 -bf 1
I="$WORKDIR/803/in"; O="$WORKDIR/803/out"; mkdir -p "$I/p" "$O"
cp "$(dirname "$0")/../assets/xvid_packed.avi" "$I/p/test.avi"
cp "$I/p/test.avi" "$WORKDIR/803/source.avi"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 10\ncrf = 20\n[scene_detection]\nextra_split = 25\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "packed Xvid: no output"
assert_log_contains "decoding it from the start"
assert_frames_match "$O/test.mkv" "$WORKDIR/803/source.avi" null 24

# a TS that opens on leading B-frames is not decoded linearly, which would shift its pictures
I="$WORKDIR/804/in"; O="$WORKDIR/804/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 300 -c:v libx264 -preset veryfast -qp 14 -pix_fmt yuv420p -x264-params open-gop=1:keyint=48:min-keyint=48:scenecut=0:bframes=3:b-pyramid=0 -f mpegts -muxdelay 0 "$WORKDIR/804/open.ts"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 2.2 -i "$WORKDIR/804/open.ts" -c copy -muxdelay 0 -f mpegts "$WORKDIR/804/a.ts"
LAST=$(ffprobe -v error -select_streams v:0 -show_entries packet=pts_time -of default=nw=1:nk=1 "$WORKDIR/804/a.ts" | sort -n | tail -1)
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24,hue=h=90" -frames:v 120 -c:v libx264 -preset veryfast -qp 14 -pix_fmt yuv420p -g 48 -bf 2 -sc_threshold 0 -muxdelay 0 -output_ts_offset "$(awk "BEGIN{printf \"%.4f\", $LAST + 0.0005}")" "$WORKDIR/804/b.ts"
cat "$WORKDIR/804/a.ts" "$WORKDIR/804/b.ts" > "$I/p/test.ts"
cp "$I/p/test.ts" "$WORKDIR/804/source.ts"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 10\ncrf = 12\n[scene_detection]\nextra_split = 48\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 240 || fail "TS opening on leading frames: no output"
assert_log_not_contains "decoding it from the start"
ffmpeg -nostdin -y -hide_banner -loglevel error -i "$WORKDIR/804/source.ts" -map 0:v:0 -fps_mode passthrough -frames:v 200 -c:v ffv1 "$WORKDIR/804/shown.mkv"
ffmpeg -nostdin -y -hide_banner -loglevel error -i "$O/test.mkv" -map 0:v:0 -frames:v 200 -c copy "$WORKDIR/804/start.mkv"
assert_frames_match "$WORKDIR/804/start.mkv" "$WORKDIR/804/shown.mkv" null 30

test_done
