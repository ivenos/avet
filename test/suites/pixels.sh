#!/bin/sh
# Pixel formats and colour: bit depth, chroma subsampling, range, siting, HDR static metadata.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

encode() { # NAME FIXTURE [AVET_LINES]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 40\n[avet]\n%b\n' "${3:-}" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 180 || fail "$1: no output"
}

# Mastering display and content light level of the first frame, as plain numbers.
hdr_static() {
    ffprobe -v error -select_streams v:0 -read_intervals '%+#1' \
        -show_entries frame_side_data=red_x,red_y,green_x,green_y,blue_x,blue_y,white_point_x,white_point_y,min_luminance,max_luminance,max_content,max_average \
        -of default=nw=1 "$1" | awk -F= 'NF == 2 { split($2, r, "/"); printf "%s %.8f\n", $1, (r[2] + 0 ? r[1] / r[2] : r[1]) }' | sort
}

# Chromaticity to 1/65536 and minimum luminance to 1/16384, what AV1 can store;
# anything coarser is a loss avet caused.
assert_hdr_static_match() {
    local diff
    diff=$(paste -d ' ' "$(_tmp_file hdr_static "$1")" "$(_tmp_file hdr_static "$2")" | awk '
        NF != 4 || $1 != $3 { print "fields differ: " $0; exit }
        { d = $2 - $4; if (d < 0) d = -d
          tol = ($1 ~ /_x$|_y$/) ? 1 / 65536 : ($1 == "min_luminance") ? 1 / 16384 : 0.5
          if (d > tol) { printf "%s is %s, the source %s", $1, $2, $4; exit } }')
    [ -z "$diff" ] && [ -n "$(hdr_static "$2")" ] || fail "HDR static metadata of $1: ${diff:-source has none}"
}

# -- 12-bit HDR10: 10-bit output, every static metadata value kept --------------------
encode hdr12 pattern_hdr12.mkv
assert_log_contains "bit-depth conversion: 12-bit to 10-bit"
assert_video_pix_fmt "$O/test.mkv" yuv420p10le
assert_frames_match "$O/test.mkv" "$SRC"
assert_hdr_static_match "$O/test.mkv" "$SRC"
assert_stream_value "$O/test.mkv" v:0 stream=color_range,color_space,color_transfer,color_primaries "tv bt2020nc smpte2084 bt2020"

# -- full range and top-left chroma siting survive -------------------------------------
encode fullrange pattern_fullrange.mkv
assert_frames_match "$O/test.mkv" "$SRC"
assert_stream_value "$O/test.mkv" v:0 stream=color_range,color_space,color_transfer,color_primaries,chroma_location "pc bt709 bt709 bt709 topleft"

# -- 4:2:2 and 4:4:4 are brought to 4:2:0 instead of failing ----------------------------
for sub in 422 444; do
    encode "chroma$sub" "pattern_$sub.mkv"
    assert_log_contains "chroma conversion"
    assert_frames_match "$O/test.mkv" "$SRC"
done
assert_video_pix_fmt "$WORKDIR/chroma422/out/test.mkv" yuv420p10le
assert_video_pix_fmt "$WORKDIR/chroma444/out/test.mkv" yuv420p

# -- bit_depth converts without touching the picture ---------------------------------------
encode up8to10 pattern.mkv 'bit_depth = 10'
assert_video_pix_fmt "$O/test.mkv" yuv420p10le
assert_frames_match "$O/test.mkv" "$SRC"

# 8-bit chroma, dithered differently by FFMS2 and by the reference conversion.
encode down12to8 pattern_hdr12.mkv 'bit_depth = 8'
assert_video_pix_fmt "$O/test.mkv" yuv420p
assert_frames_match "$O/test.mkv" "$SRC" null 22

# -- the colour description stays: SD primaries, full range, HLG, JPEG siting -----------------------
for fixture in colour_pal.mkv colour_ntsc.mkv colour_full8.mkv colour_center.avi hlg.mkv; do
    encode "colour_$fixture" "$fixture"
    assert_frames_match "$O/test.mkv" "$SRC"
    assert_stream_value "$O/test.mkv" v:0 stream=color_range,color_space,color_transfer,color_primaries,chroma_location \
        "$(ffprobe -v error -select_streams v:0 -show_entries stream=color_range,color_space,color_transfer,color_primaries,chroma_location \
            -of default=nw=1:nk=1 "$SRC" | tr '\n' ' ' | sed 's/ *$//')"
done

test_done
