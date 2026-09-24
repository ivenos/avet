#!/bin/sh
# Generates every fixture into $FIXTURES_DIR. Called by run.sh.

set -u
: "${FIXTURES_DIR:?}" "${TEST_IMAGE:?}" "${TOOLS_IMAGE:?}"

docker run --rm -i \
    --user "$(id -u):$(id -g)" \
    -v "${FIXTURES_DIR}:/out:z" \
    --entrypoint sh \
    "$TEST_IMAGE" << 'GEN'
set -e
cd /out
FF="ffmpeg -nostdin -y -hide_banner -loglevel error"

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

# Letterboxed, so the Dolby Vision active area has something to follow through a crop.
# The enhancement layer has to share the base layer's GOP, hence the fixed one.
X265="log-level=error:bframes=4:keyint=24:scenecut=0"
HDR10="hdr10=1:max-cll=1000,400:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)"
$FF -f lavfi -i "testsrc2=size=640x280:rate=24" \
    -frames:v 48 \
    -vf "pad=640:360:0:40:black,format=yuv420p10le,setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc" \
    -c:v libx265 -preset ultrafast -x265-params "$X265:$HDR10" \
    dv_base.hevc
$FF -f lavfi -i "testsrc2=size=640x360:rate=24" \
    -frames:v 48 \
    -vf "format=yuv420p10le,setparams=range=tv:color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc" \
    -c:v libx265 -preset ultrafast -x265-params "$X265" \
    dv_hlg.hevc
$FF -f lavfi -i "testsrc2=size=320x180:rate=24" \
    -frames:v 48 -pix_fmt yuv420p10le \
    -c:v libx265 -preset ultrafast -x265-params "$X265" \
    dv_el.hevc
for part in 24 72; do
    $FF -f lavfi -i "testsrc2=size=640x360:rate=24" \
        -frames:v "$part" \
        -vf "format=yuv420p10le,setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc" \
        -c:v libx265 -preset ultrafast -x265-params "$X265:$HDR10" \
        "hdr10plus_part$part.hevc"
done
GEN

GEN_RC=$?
if [ "$GEN_RC" -ne 0 ]; then
    printf "ERROR: fixture generation failed (rc=%d)\n" "$GEN_RC"
    exit 1
fi

# fel_orig.bin is a test asset from quietvoid/dovi_tool 2.3.4, MIT.
cp "$(dirname "$0")/assets/fel_orig.bin" "$FIXTURES_DIR/fel.bin" || exit 1

docker run --rm -i \
    -e OWNER="$(id -u):$(id -g)" \
    -v "${FIXTURES_DIR}:/out:z" \
    "$TOOLS_IMAGE" sh << 'GEN'
set -e
cd /out

cat > dovi.json << 'JSON'
{
  "length": 48,
  "level5": { "active_area_left_offset": 0, "active_area_right_offset": 0,
              "active_area_top_offset": 40, "active_area_bottom_offset": 40 },
  "level6": { "max_display_mastering_luminance": 1000, "min_display_mastering_luminance": 1,
              "max_content_light_level": 1000, "max_frame_average_light_level": 400 }
}
JSON
dovi_tool generate -j dovi.json -p 8.1 -o rpu81.bin >/dev/null
dovi_tool generate -j dovi.json -p 8.4 -o rpu84.bin >/dev/null
dovi_tool generate -j dovi.json -p 5 -o rpu5.bin >/dev/null

# dovi_tool cannot generate profile 7, so a disc RPU from its own test assets stands in.
echo '{ "duplicate": [{ "source": 0, "offset": 0, "length": 47 }] }' > duplicate.json
dovi_tool editor -i fel.bin -j duplicate.json -o rpu7.bin >/dev/null

hdr10plus_json() {
    printf '{"JSONInfo":{"HDR10plusProfile":"B","Version":"1.0"},"SceneInfo":['
    i=0
    while [ "$i" -lt "$1" ]; do
        [ "$i" -gt 0 ] && printf ','
        printf '{"BezierCurveData":{"Anchors":[143,298,447,592,731,864,891,917,938],"KneePointX":164,"KneePointY":240},'
        printf '"LuminanceParameters":{"AverageRGB":%d,"LuminanceDistributions":{"DistributionIndex":[1,5,10,25,50,75,90,95,99],' "$((100 + i))"
        printf '"DistributionValues":[0,6080,92,1,4,107,726,1784,5843]},"MaxScl":[7768,6589,6912]},'
        printf '"NumberOfWindows":1,"TargetedSystemDisplayMaximumLuminance":400,"SceneFrameIndex":%d,"SceneId":0,"SequenceFrameIndex":%d}' "$i" "$i"
        i=$((i + 1))
    done
    printf '],"SceneInfoSummary":{"SceneFirstFrameIndex":[0],"SceneFrameNumbers":[%d]},' "$1"
    printf '"ToolInfo":{"Tool":"hdr10plus_tool","Version":"1.7.2"}}'
}
hdr10plus_json 48 > hdr10plus.json

echo "  dv81_hdr10plus.mkv"
dovi_tool inject-rpu -i dv_base.hevc --rpu-in rpu81.bin -o dv81.hevc >/dev/null
hdr10plus_tool inject -i dv81.hevc -j hdr10plus.json -o dv81_hdr10plus.hevc >/dev/null
mkvmerge -q -o dv81_hdr10plus.mkv --default-duration 0:24fps dv81_hdr10plus.hevc

echo "  dv84.mkv"
dovi_tool inject-rpu -i dv_hlg.hevc --rpu-in rpu84.bin -o dv84.hevc >/dev/null
mkvmerge -q -o dv84.mkv --default-duration 0:24fps dv84.hevc

echo "  dv5.mkv"
dovi_tool inject-rpu -i dv_base.hevc --rpu-in rpu5.bin -o dv5.hevc >/dev/null
mkvmerge -q -o dv5.mkv --default-duration 0:24fps dv5.hevc

echo "  dv7_fel.mkv"
dovi_tool inject-rpu -i dv_el.hevc --rpu-in rpu7.bin -o dv_el_rpu.hevc >/dev/null
dovi_tool mux --bl dv_base.hevc --el dv_el_rpu.hevc -o dv7.hevc >/dev/null
mkvmerge -q -o dv7_fel.mkv --default-duration 0:24fps dv7.hevc

# x265's --dhdr10-opt writes HDR10+ only where it changes; here it stops after 24 frames.
echo "  hdr10plus_sparse.mkv"
hdr10plus_json 24 > hdr10plus24.json
hdr10plus_tool inject -i hdr10plus_part24.hevc -j hdr10plus24.json -o hdr10plus_first.hevc >/dev/null
cat hdr10plus_first.hevc hdr10plus_part72.hevc > hdr10plus_sparse.hevc
mkvmerge -q -o hdr10plus_sparse.mkv --default-duration 0:24fps hdr10plus_sparse.hevc

rm -f dovi.json hdr10plus.json hdr10plus24.json duplicate.json fel.bin rpu81.bin rpu84.bin rpu5.bin rpu7.bin \
    dv_base.hevc dv_hlg.hevc dv_el.hevc dv_el_rpu.hevc dv81.hevc dv81_hdr10plus.hevc dv84.hevc dv5.hevc dv7.hevc \
    hdr10plus_part24.hevc hdr10plus_part72.hevc hdr10plus_first.hevc hdr10plus_sparse.hevc
chown "$OWNER" dv81_hdr10plus.mkv dv84.mkv dv5.mkv dv7_fel.mkv hdr10plus_sparse.mkv
GEN

GEN_RC=$?
if [ "$GEN_RC" -ne 0 ]; then
    printf "ERROR: Dolby Vision fixture generation failed (rc=%d)\n" "$GEN_RC"
    exit 1
fi

docker run --rm -i \
    --user "$(id -u):$(id -g)" \
    -v "${FIXTURES_DIR}:/out:z" \
    --entrypoint sh \
    "$TEST_IMAGE" << 'GEN'
set -e
cd /out
FF="ffmpeg -nostdin -y -hide_banner -loglevel error"

echo "  dv81_hdr10plus.m2ts"
$FF -i dv81_hdr10plus.mkv -c copy -f mpegts dv81_hdr10plus.m2ts

echo "  dv81_hdr10plus.mp4"
$FF -i dv81_hdr10plus.mkv -c copy -tag:v hvc1 -strict unofficial dv81_hdr10plus.mp4

echo "  dv5_norecord.mp4"
$FF -i dv5.mkv -c copy -tag:v hvc1 dv5_norecord.mp4
GEN

GEN_RC=$?
if [ "$GEN_RC" -ne 0 ]; then
    printf "ERROR: container fixture generation failed (rc=%d)\n" "$GEN_RC"
    exit 1
fi

# Content fixtures. Every frame's luma differs completely from its neighbors, chroma is
# smooth enough for a fast preset, every 48th frame (from frame 12) is white, and a 50 ms
# beep starts under each white frame.
docker run --rm -i \
    --user "$(id -u):$(id -g)" \
    -v "${FIXTURES_DIR}:/out:z" \
    --entrypoint sh \
    "$TEST_IMAGE" << 'GEN'
set -e
cd /out
FF="ffmpeg -nostdin -y -hide_banner -loglevel error"
PATTERN="geq=lum=128+60*sin(X/9+N*1.7)+50*cos(Y/7-N*1.1):cb=128+60*sin(X/40+Y/90+N*0.05):cr=128+60*cos(Y/30-X/70-N*0.04)"
FLASH="drawbox=w=iw:h=ih:color=white:t=fill:enable=eq(mod(n\,48)\,12)"
BEEP="0.5*sin(2*PI*1000*t)*lt(mod(t-0.5\,2)\,0.05)*gte(t\,0.5)"
pattern() { # SIZE RATE PIXFMT
    echo "nullsrc=size=$1:rate=$2,$PATTERN,$FLASH,format=$3"
}

echo "  pattern.mkv"
$FF -f lavfi -i "$(pattern 320x180 24 yuv420p)" \
    -f lavfi -i "aevalsrc=exprs=$BEEP|$BEEP:c=stereo:s=48000:d=10" \
    -frames:v 240 -map 0:v -map 1:a -map 1:a \
    -c:v libx264 -qp 0 -preset ultrafast \
    -c:a:0 flac -c:a:1 aac -b:a:1 128k \
    pattern_av.mkv
printf '1\n00:00:01,000 --> 00:00:02,500\nFirst line.\n\n2\n00:00:04,000 --> 00:00:06,042\nSecond line.\n\n' > eng.srt
printf '1\n00:00:02,000 --> 00:00:03,000\nErste Zeile.\n\n' > deu.srt
cat > jpn.ass << 'ASS'
[Script Info]
ScriptType: v4.00+
PlayResX: 320
PlayResY: 180

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Avet Test Sans,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:03.00,0:00:05.50,Default,,0,0,0,,{\b1}Dritte Spur
ASS
head -c 4096 /dev/urandom > font.ttf
printf 'CHAPTER01=00:00:00.000\nCHAPTER01NAME=Opening\nCHAPTER02=00:00:03.500\nCHAPTER02NAME=Middle\nCHAPTER03=00:00:07.250\nCHAPTER03NAME=End\n' > chapters.txt
mkvmerge -q -o pattern.mkv \
    --language 1:eng --track-name 1:English --default-track-flag 1:yes \
    --language 2:ger --track-name 2:Kommentar --default-track-flag 2:no --commentary-flag 2:yes \
    pattern_av.mkv \
    --language 0:eng --default-track-flag 0:yes eng.srt \
    --language 0:ger --default-track-flag 0:no --forced-display-flag 0:yes deu.srt \
    --language 0:jpn --default-track-flag 0:no --hearing-impaired-flag 0:yes jpn.ass \
    --attachment-mime-type font/ttf --attach-file font.ttf \
    --chapters chapters.txt

echo "  pattern_video_delay.mkv, pattern_audio_delay.mkv, pattern_global_offset.mkv"
mkvmerge -q -o pattern_video_delay.mkv --sync 0:300 pattern.mkv
mkvmerge -q -o pattern_audio_delay.mkv --sync 1:250 --sync 2:250 pattern.mkv
mkvmerge -q -o pattern_global_offset.mkv --sync -1:5000 --chapter-sync 5000 pattern.mkv

echo "  pattern_offset_vtt.mkv"
printf 'WEBVTT\n\n00:00:01.000 --> 00:00:02.500\nFirst line.\n' > eng.vtt
mkvmerge -q -o pattern_offset_vtt.mkv --sync -1:5000 pattern_av.mkv --sync 0:5000 --language 0:eng eng.vtt

echo "  pattern_offset.m2ts"
$FF -itsoffset 0.321 -i pattern_av.mkv -i pattern_av.mkv -map 0:v -map 1:a:1 -c copy -f mpegts pattern_offset.m2ts

echo "  pattern_vfr.mkv"
$FF -f lavfi -i "$(pattern 320x180 48 yuv420p)" -frames:v 480 \
    -vf "select='lt(n\,240)+not(mod(n\,2))'" -fps_mode vfr \
    -c:v libx264 -qp 0 -preset ultrafast pattern_vfr.mkv

echo "  pattern_ntsc.mkv"
$FF -f lavfi -i "$(pattern 320x180 24000/1001 yuv420p)" \
    -f lavfi -i "aevalsrc=exprs=$BEEP|$BEEP:c=stereo:s=48000:d=11" \
    -frames:v 240 -map 0:v -map 1:a -shortest \
    -c:v libx264 -qp 0 -preset ultrafast -c:a flac pattern_ntsc.mkv

echo "  pattern_rot90.mp4, pattern_rot270.mp4"
$FF -i pattern_av.mkv -map 0:v -c:v copy pattern_plain.mp4
$FF -display_rotation 90 -i pattern_plain.mp4 -c copy pattern_rot90.mp4
$FF -display_rotation 270 -i pattern_plain.mp4 -c copy pattern_rot270.mp4

echo "  pattern_anamorphic.mkv"
$FF -f lavfi -i "$(pattern 360x288 25 yuv420p),setsar=64/45" -frames:v 100 \
    -c:v libx264 -qp 0 -preset ultrafast pattern_anamorphic.mkv

# Uneven bars, so a crop that is off by a line or mirrored shows.
echo "  pattern_bars.mkv, pattern_bars_rot90.mp4"
$FF -f lavfi -i "nullsrc=size=640x276:rate=24,$PATTERN,format=yuv420p,pad=640:360:0:44:black" -frames:v 96 \
    -c:v libx264 -qp 0 -preset ultrafast pattern_bars.mkv
$FF -display_rotation 90 -i pattern_bars.mkv -map 0:v -c:v copy pattern_bars_rot90.mp4

echo "  pattern_dark.mkv"
$FF -f lavfi -i "color=c=black:size=320x180:rate=24" -frames:v 48 \
    -vf "drawbox=x=40:y=30:w=100:h=50:color=white:t=fill" \
    -c:v libx264 -qp 0 -preset ultrafast pattern_dark.mkv

echo "  pattern_odd.mkv, pattern_422.mkv, pattern_444.mkv"
$FF -f lavfi -i "$(pattern 321x181 24 yuv420p)" -frames:v 48 -c:v ffv1 pattern_odd.mkv
$FF -f lavfi -i "$(pattern 320x180 24 yuv422p10le)" -frames:v 48 -c:v ffv1 pattern_422.mkv
$FF -f lavfi -i "$(pattern 320x180 24 yuv444p)" -frames:v 48 -c:v ffv1 pattern_444.mkv

echo "  pattern_rgb.mkv, pattern_gray.mkv"
$FF -f lavfi -i "$(pattern 320x180 24 gbrp),setparams=range=pc:colorspace=gbr" -frames:v 48 -c:v ffv1 pattern_rgb.mkv
$FF -f lavfi -i "$(pattern 320x180 24 gray),setparams=range=pc" -frames:v 48 -c:v ffv1 pattern_gray.mkv

# White point and minimum luminance off a 4-decimal grid: rounding them is a loss.
echo "  pattern_hdr12.mkv"
$FF -f lavfi -i "$(pattern 320x180 24 yuv420p12le),setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc" \
    -frames:v 48 -c:v libx265 \
    -x265-params "log-level=error:lossless=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15636,16451)L(10000000,5):max-cll=1017,391" \
    pattern_hdr12.mkv

echo "  pattern_fullrange.mkv"
$FF -f lavfi -i "$(pattern 320x180 24 yuv420p10le),setparams=range=pc:color_primaries=bt709:color_trc=bt709:colorspace=bt709" \
    -frames:v 48 -c:v libx264 -qp 0 -preset ultrafast -chroma_sample_location topleft pattern_fullrange.mkv

# One tone per channel: a swapped or dropped channel moves or loses its frequency.
echo "  tones_71.mkv, tones_51side.mkv"
tone() { printf '0.4*sin(2*PI*%s*t)' "$1"; }
$FF -f lavfi -i "$(pattern 320x180 24 yuv420p)" \
    -f lavfi -i "aevalsrc=exprs=$(tone 300)|$(tone 500)|$(tone 700)|$(tone 90)|$(tone 1100)|$(tone 1300)|$(tone 1700)|$(tone 1900):c=7.1:s=48000:d=2" \
    -frames:v 48 -map 0:v -map 1:a -c:v libx264 -qp 0 -preset ultrafast -c:a flac tones_71.mkv
$FF -f lavfi -i "$(pattern 320x180 24 yuv420p)" \
    -f lavfi -i "aevalsrc=exprs=$(tone 300)|$(tone 500)|$(tone 700)|$(tone 90)|$(tone 1700)|$(tone 1900):c=5.1(side):s=48000:d=2" \
    -frames:v 48 -map 0:v -map 1:a -c:v libx264 -qp 0 -preset ultrafast -c:a flac tones_51side.mkv

rm -f pattern_av.mkv pattern_plain.mp4 eng.srt eng.vtt deu.srt jpn.ass font.ttf chapters.txt
GEN

GEN_RC=$?
if [ "$GEN_RC" -ne 0 ]; then
    printf "ERROR: content fixture generation failed (rc=%d)\n" "$GEN_RC"
    exit 1
fi

# The pattern as real sources carry it: GOP structures and containers, audio codecs and
# channel layouts, bitmap and text subtitles, color descriptions.
docker run --rm -i \
    --user "$(id -u):$(id -g)" \
    -v "${FIXTURES_DIR}:/out:z" \
    --entrypoint sh \
    "$TEST_IMAGE" << 'GEN'
set -e
cd /out
FF="ffmpeg -nostdin -y -hide_banner -loglevel error"
PATTERN="geq=lum=128+60*sin(X/9+N*1.7)+50*cos(Y/7-N*1.1):cb=128+60*sin(X/40+Y/90+N*0.05):cr=128+60*cos(Y/30-X/70-N*0.04)"
FLASH="drawbox=w=iw:h=ih:color=white:t=fill:enable=eq(mod(n\,48)\,12)"
BEEP="0.5*sin(2*PI*1000*t)*lt(mod(t-0.5\,2)\,0.05)*gte(t\,0.5)"
pattern() { # SIZE RATE PIXFMT
    echo "nullsrc=size=$1:rate=$2,$PATTERN,$FLASH,format=$3"
}
H264="-c:v libx264 -qp 2 -x264-params keyint=12:min-keyint=12:bframes=3:b-pyramid=normal:open-gop=1:scenecut=0"

bytes() { for v in "$@"; do printf "\\$(printf '%03o' "$v")"; done; }
u16() { bytes $(($1 >> 8 & 255)) $(($1 & 255)); }
u32() { bytes $(($1 >> 24 & 255)) $(($1 >> 16 & 255)) $(($1 >> 8 & 255)) $(($1 & 255)); }
segment() { printf 'PG'; u32 "$1"; u32 0; bytes "$2"; u16 "$3"; }
# PGS, one START_MS:END_MS:CR per subtitle: a 120x20 box, half white, half colored.
pgs() {
    n=0
    for event in "$@"; do
        start=$((${event%%:*} * 90)); rest=${event#*:}; end=$((${rest%%:*} * 90)); cr=${rest#*:}
        segment $start 22 19; u16 320; u16 180; bytes 16; u16 $n; bytes 128 0 0 1; u16 0; bytes 0 0; u16 100; u16 150
        segment $start 23 10; bytes 1 0; u16 100; u16 150; u16 120; u16 20
        segment $start 20 12; bytes 0 0 1 235 128 128 255 2 "$cr" 200 60 255
        segment $start 21 131; u16 0; bytes 0 192 0 0 124; u16 120; u16 20
        row=0; while [ $row -lt 20 ]; do bytes 0 192 120 $((row < 10 ? 1 : 2)) 0 0; row=$((row + 1)); done
        segment $start 128 0
        segment $end 22 11; u16 320; u16 180; bytes 16; u16 $((n + 1)); bytes 0 0 0 0
        segment $end 23 10; bytes 1 0; u16 100; u16 150; u16 120; u16 20
        segment $end 128 0
        n=$((n + 2))
    done
}
printf '1\n00:00:01,000 --> 00:00:02,500\nFirst line.\n\n2\n00:00:04,000 --> 00:00:06,042\nSecond line.\n\n' > eng.srt
printf '1\n00:00:02,000 --> 00:00:03,000\nErste Zeile.\n\n' > ger.srt
pgs 1000:2500:40 4000:6042:90 > eng.sup
pgs 2000:3000:160 > ger.sup

echo "  gop_h264.{mkv,mp4,ts,flv,mov}, gop_hevc.ts, gop_mpeg2.m2ts, gop_mpeg4.avi, gop_vp9.webm, gop_cut.mp4"
$FF -i pattern.mkv -map 0:v -map 0:a:0 $H264 -c:a flac gop_h264.mkv
$FF -i pattern.mkv -i eng.srt -map 0:v -map 0:a:0 -map 1 $H264 -c:a aac -b:a 192k \
    -c:s mov_text -metadata:s:s:0 language=eng gop_h264.mp4
$FF -i pattern.mkv -map 0:v -map 0:a:0 $H264 -c:a mp2 -b:a 256k gop_h264.ts
$FF -i pattern.mkv -map 0:v -map 0:a:0 $H264 -c:a aac -b:a 192k gop_h264.flv
$FF -i pattern.mkv -map 0:v -map 0:a:0 $H264 -c:a pcm_s24le -timecode 01:00:00:00 gop_h264.mov
$FF -i pattern.mkv -map 0:v -map 0:a:0 -c:v libx265 -crf 2 \
    -x265-params log-level=error:keyint=12:min-keyint=12:bframes=4:open-gop=1:scenecut=0 \
    -c:a eac3 -b:a 384k gop_hevc.ts
$FF -i pattern.mkv -map 0:v -map 0:a:0 -c:v mpeg2video -q:v 1 -g 12 -bf 2 -c:a ac3 -b:a 384k gop_mpeg2.m2ts
$FF -i pattern.mkv -map 0:v -map 0:a:0 -c:v mpeg4 -q:v 1 -g 12 -bf 2 -c:a pcm_s16le gop_mpeg4.avi
$FF -i pattern.mkv -map 0:v -map 0:a:0 -c:v libvpx-vp9 -crf 4 -b:v 0 -g 12 -auto-alt-ref 1 \
    -lag-in-frames 16 -deadline good -cpu-used 8 -c:a libopus -b:a 192k gop_vp9.webm
# Cut 3.3 s in: the edit list hides frames 72 to 79 and 3.3 s of audio.
$FF -ss 3.3 -i gop_h264.mp4 -map 0 -c copy gop_cut.mp4
$FF -i pattern.mkv -map 0:v -map 0:a:0 -vf "trim=start_frame=80,setpts=PTS-STARTPTS" \
    -af "atrim=start_sample=160000,asetpts=PTS-STARTPTS" \
    -c:v libx264 -qp 0 -preset ultrafast -c:a flac pattern_from80.mkv

# 65 s in, its timestamps pass 2^33 and start over.
echo "  long.mkv, gop_wrap.m2ts"
$FF -f lavfi -i "$(pattern 160x90 24 yuv420p)" -f lavfi -i "aevalsrc=exprs=$BEEP|$BEEP:c=stereo:s=48000:d=70" \
    -frames:v 1680 -map 0:v -map 1:a -c:v libx264 -qp 0 -preset ultrafast -c:a flac long.mkv
pgs 10000:12000:40 68000:69500:90 > late.sup
$FF -copyts -i long.mkv -i late.sup -map 0:v -map 0:a -map 1 $H264 -c:a mp2 -b:a 256k -c:s copy \
    -mpegts_m2ts_mode 1 -output_ts_offset 95378 gop_wrap.m2ts

echo "  audio_codecs.{mkv,m2ts,mp4}"
$FF -i pattern.mkv -map 0:v -c:v copy \
    -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 \
    -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 \
    -c:a:0 ac3 -c:a:1 eac3 -c:a:2 dca -c:a:3 truehd -c:a:4 mp2 -c:a:5 libmp3lame -c:a:6 aac \
    -c:a:7 alac -c:a:8 flac -ar:a:8 96000 -sample_fmt:a:8 s32 -c:a:9 wavpack -c:a:10 tta \
    -c:a:11 libvorbis -c:a:12 libopus -c:a:13 pcm_f32le -strict -2 audio_codecs.mkv
# The TrueHD stream announces an AC-3 core it does not carry.
$FF -i pattern.mkv -map 0:v -c:v copy \
    -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 \
    -c:a:0 pcm_bluray -sample_fmt:a:0 s32 -c:a:1 truehd -c:a:2 dca -c:a:3 ac3 -c:a:4 s302m \
    -c:a:5 aac -strict -2 -mpegts_m2ts_mode 1 -mpegts_flags latm audio_codecs.m2ts
$FF -i pattern.mkv -map 0:v -c:v copy -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 -map 0:a:0 \
    -c:a:0 aac -c:a:1 aac -ar:a:1 44100 -c:a:2 alac -c:a:3 libmp3lame -c:a:4 ac3 audio_codecs.mp4

echo "  tones_layouts.mov"
i=0; inputs=""; maps=""
while read -r layout frequencies; do
    exprs=""
    for f in $frequencies; do exprs="$exprs${exprs:+|}0.4*sin(2*PI*$f*t)"; done
    $FF -f lavfi -i "aevalsrc=exprs=$exprs:c=$layout:s=48000:d=2" -c:a pcm_s16le "layout$i.mov"
    inputs="$inputs -i layout$i.mov"; maps="$maps -map $((i + 1)):a"; i=$((i + 1))
done << 'LAYOUTS'
2.1 300 500 90
3.0 300 500 700
4.0 300 500 700 1100
quad(side) 300 500 1300 1500
5.0 300 500 700 1100 1300
5.0(side) 300 500 700 1300 1500
6.0 300 500 700 1100 1300 1500
6.1 300 500 700 90 1100 1300 1500
6.1(back) 300 500 700 90 1100 1300 1500
7.0 300 500 700 1100 1300 1500 1700
hexagonal 300 500 700 1100 1300 1500
7.1(wide) 300 500 700 90 1100 1300 1500 1700
octagonal 300 500 700 1100 1300 1500 1700 1900
LAYOUTS
$FF -i pattern.mkv $inputs -map 0:v -c:v copy $maps -c:a pcm_s16le -frames:v 48 tones_layouts.mov

echo "  subs_bitmap.{mkv,m2ts}, subs_dvb.ts, subs_text.mp4, subs_ttml.mp4"
$FF -copyts -i pattern.mkv -i eng.sup -i ger.sup -map 0:v -map 0:a:0 -map 1 -map 2 -map 1 -c:v copy -c:a copy \
    -c:s:0 copy -c:s:1 copy -c:s:2 dvdsub -metadata:s:s:0 language=eng -metadata:s:s:1 language=ger \
    -metadata:s:s:2 language=eng -disposition:s:0 default -disposition:s:1 forced subs_bitmap.mkv
$FF -copyts -i pattern.mkv -i eng.sup -i ger.sup -map 0:v -map 0:a:0 -map 1 -map 2 -c:v copy -c:a ac3 \
    -c:s copy -mpegts_m2ts_mode 1 subs_bitmap.m2ts
$FF -copyts -i pattern.mkv -i ger.sup -i eng.sup -map 0:v -map 0:a:0 -map 1 -map 2 -c:v copy -c:a mp2 \
    -c:s dvbsub -metadata:s:s:0 language=ger -metadata:s:s:1 language=eng subs_dvb.ts
$FF -i pattern.mkv -i eng.srt -i ger.srt -map 0:v -map 0:a:0 -map 1 -map 2 -c:v copy -c:a aac \
    -c:s mov_text -metadata:s:s:0 language=eng -metadata:s:s:1 language=ger subs_text.mp4
# mkvmerge skips the TTML track, so its subtitle numbering is one behind ffprobe's.
$FF -i pattern.mkv -i ger.srt -i eng.srt -map 0:v -map 0:a:0 -map 1 -map 2 -c:v copy -c:a aac \
    -c:s:0 ttml -c:s:1 mov_text -metadata:s:s:0 language=ger -metadata:s:s:1 language=eng subs_ttml.mp4

echo "  color_pal.mkv, color_ntsc.mkv, color_full8.mkv, color_center.avi"
$FF -f lavfi -i "$(pattern 320x180 25 yuv420p),setparams=range=tv:color_primaries=bt470bg:color_trc=bt709:colorspace=bt470bg" \
    -frames:v 50 -c:v mpeg2video -q:v 1 color_pal.mkv
$FF -f lavfi -i "$(pattern 320x180 30000/1001 yuv420p),setparams=range=tv:color_primaries=smpte170m:color_trc=smpte170m:colorspace=smpte170m" \
    -frames:v 60 -c:v libx264 -qp 0 -preset ultrafast color_ntsc.mkv
$FF -f lavfi -i "$(pattern 320x180 24 yuv420p),setparams=range=pc:color_primaries=bt709:color_trc=bt709:colorspace=bt709" \
    -frames:v 48 -c:v libx264 -qp 0 -preset ultrafast color_full8.mkv
$FF -f lavfi -i "$(pattern 320x180 24 yuvj420p)" -frames:v 48 -c:v mjpeg -q:v 1 color_center.avi

echo "  one_frame.mkv, audio_only.mkv"
$FF -f lavfi -i "$(pattern 320x180 24 yuv420p)" -frames:v 1 -c:v libx264 -qp 0 -preset ultrafast one_picture.mkv
$FF -i one_picture.mkv -f lavfi -i "aevalsrc=exprs=$BEEP|$BEEP:c=stereo:s=48000:d=3" \
    -map 0:v -map 1:a -c:v copy -c:a flac one_frame.mkv
$FF -i pattern.mkv -map 0:a:0 -c:a copy audio_only.mkv

rm -f eng.srt ger.srt eng.sup ger.sup late.sup layout*.mov one_picture.mkv
GEN

GEN_RC=$?
if [ "$GEN_RC" -ne 0 ]; then
    printf "ERROR: real-world fixture generation failed (rc=%d)\n" "$GEN_RC"
    exit 1
fi
