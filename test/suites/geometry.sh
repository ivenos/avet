#!/bin/sh
# Frame geometry: crop, scale, odd sizes, sample aspect ratio, rotation and mirror, checked on content.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

encode() { # NAME FIXTURE [AVET_LINES]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 40\n[avet]\n%b\n' "${3:-}" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 180 || fail "$1: no output"
}

# crop: exactly the picture, no line off, bars uneven so a mirrored crop shows
encode crop pattern_bars.mkv 'crop = true'
assert_log_contains "auto-crop: detected crop=640:276:0:44"
assert_frames_match "$O/test.mkv" "$SRC" "crop=640:276:0:44"

# a container's crop is the picture, and auto-crop looks inside it
encode pixcrop pattern_pixcrop.mkv
assert_log_contains "the container crops the picture to 640x276 at 0,8"
assert_frames_match "$O/test.mkv" "$SRC"
encode pixcrop_crop pattern_pixcrop.mkv 'crop = true'
assert_log_contains "auto-crop: detected crop=480:276:72:8"
assert_frames_match "$O/test.mkv" "$SRC" "crop=480:276:72:0"
encode pixcrop_odd_crop pattern_pixcrop_odd.mkv 'crop = true'
assert_log_contains "auto-crop: detected crop=480:274:72:8"
assert_stream_value "$O/test.mkv" v:0 stream=width,height "480 274"

# crop, then scale what is left
encode crop_scale pattern_bars.mkv 'crop = true\nscale = 138'
# Downscaled chroma planes cost a few dB, still twice what a shifted frame reaches.
assert_frames_match "$O/test.mkv" "$SRC" "crop=640:276:0:44,scale=320:138:flags=lanczos" 22

# scale alone
encode scale pattern.mkv 'scale = 90'
assert_frames_match "$O/test.mkv" "$SRC" "scale=160:90:flags=lanczos" 22

# an untagged HD source scaled to SD is tagged with the BT.709 its height had
I="$WORKDIR/untagged_hd/in"; O="$WORKDIR/untagged_hd/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=1280x720:rate=24" -frames:v 24 \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p "$I/p/test.mkv"
[ "$(stream_value "$I/p/test.mkv" v:0 stream=color_space)" = unknown ] || fail "untagged_hd: the source carries a matrix"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\nscale = 360\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "untagged_hd: no output"
assert_stream_value "$O/test.mkv" v:0 stream=color_space,color_transfer,color_primaries "bt709 bt709 bt709"

# an odd frame size loses its last column and row, and nothing else
encode odd pattern_odd.mkv
assert_log_contains "odd frame size 321x181"
assert_frames_match "$O/test.mkv" "$SRC" "crop=320:180:0:0"

# anamorphic: the picture stays 16:9 on screen, scaled or not
encode anamorphic pattern_anamorphic.mkv
assert_frames_match "$O/test.mkv" "$SRC"
assert_stream_value "$O/test.mkv" v:0 stream=sample_aspect_ratio,display_aspect_ratio "64:45 16:9"

encode anamorphic_scaled pattern_anamorphic.mkv 'scale = 144'
assert_frames_match "$O/test.mkv" "$SRC" "scale=180:144:flags=lanczos" 22
assert_stream_value "$O/test.mkv" v:0 stream=width,height,sample_aspect_ratio,display_aspect_ratio "180 144 64:45 16:9"

# rotation: a phone video still plays upright
for degrees in 90 270; do
    encode "rot$degrees" "pattern_rot$degrees.mp4"
    assert_frames_match "$O/test.mkv" "$SRC"
    ROT=$(stream_value "$SRC" v:0 stream_side_data=rotation)
    [ "$ROT" = "-" ] && fail "rot$degrees: the fixture carries no rotation"
    assert_stream_value "$O/test.mkv" v:0 stream_side_data=rotation "$ROT"
done

# a mirror goes into the pixels, and only the rotation left after it into the roll
for case in "hflip -" "vflip -" "rot90_hflip -90"; do
    set -- $case
    encode "$1" "pattern_$1.mp4"
    assert_log_contains "the display matrix mirrors the picture"
    assert_frames_match "$O/test.mkv" "$SRC"
    assert_stream_value "$O/test.mkv" v:0 stream_side_data=rotation "$2"
done

# a flip keeps left-sited chroma beside its luma
I="$WORKDIR/hflip_chroma/in"; O="$WORKDIR/hflip_chroma/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=640x360:rate=24" -frames:v 12 -pix_fmt yuv420p \
    -chroma_sample_location left -c:v libx264 -qp 0 -preset ultrafast "$WORKDIR/left.mp4"
ffmpeg -nostdin -y -hide_banner -loglevel error -display_hflip -i "$WORKDIR/left.mp4" -c copy "$I/p/test.mp4"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 1\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "hflip_chroma: no output"
UP="scale=iw:ih:in_chroma_loc=left:out_chroma_loc=left:flags=bicubic,format=yuv444p"
CHROMA=$(ffmpeg -nostdin -hide_banner -noautorotate -i "$O/test.mkv" -noautorotate -i "$WORKDIR/left.mp4" -filter_complex \
    "[0:v:0]$UP,settb=1,setpts=N[a];[1:v:0]$UP,hflip,settb=1,setpts=N[b];[a][b]psnr" -fps_mode passthrough -f null - 2>&1 |
    grep -o 'u:[0-9.]* v:[0-9.]*' | tail -n 1)
echo "$CHROMA" | awk '{ split($1, u, ":"); split($2, v, ":"); exit !(u[2] >= 33 && v[2] >= 33) }' ||
    fail "hflip_chroma: chroma sits off its luma ($CHROMA dB)"

# video = copy keeps both too
# Each fixture with the property it is here for: expected and actual come from the same
# probe, so an entry the probe stops reporting would compare "-" against "-".
for pair in "pattern_rot90.mp4 stream_side_data=rotation" \
            "pattern_anamorphic.mkv stream=sample_aspect_ratio,display_aspect_ratio"; do
    set -- $pair
    fixture="$1"; entry="$2"
    name="copy_${fixture%%.*}"
    I="$WORKDIR/$name/in"; O="$WORKDIR/$name/out"; mkdir -p "$I/p" "$O"
    cp "$FIXTURES_DIR/$fixture" "$I/p/test.${fixture##*.}"
    printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "$name: no output"
    EXPECTED=$(stream_value "$FIXTURES_DIR/$fixture" v:0 "$entry")
    [ "$EXPECTED" = "-" ] && fail "$name: the fixture carries no $entry"
    assert_stream_value "$O/test.mkv" v:0 "$entry" "$EXPECTED"
done

test_done
