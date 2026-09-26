#!/bin/sh
# Frame geometry: crop, scale, odd sizes, sample aspect ratio and rotation, checked on content.
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

# crop, then scale what is left
encode crop_scale pattern_bars.mkv 'crop = true\nscale = 138'
# Downscaled chroma planes cost a few dB, still twice what a shifted frame reaches.
assert_frames_match "$O/test.mkv" "$SRC" "crop=640:276:0:44,scale=320:138:flags=lanczos" 22

# scale alone
encode scale pattern.mkv 'scale = 90'
assert_frames_match "$O/test.mkv" "$SRC" "scale=160:90:flags=lanczos" 22

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
