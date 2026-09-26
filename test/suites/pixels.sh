#!/bin/sh
# Pixel formats and color: bit depth, chroma subsampling, range, siting, HDR static metadata.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

encode() { # NAME FIXTURE [AVET_LINES]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 8\ncrf = 40\n[avet]\n%b\n' "${3:-}" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 180 || fail "$1: no output"
}

# 12-bit HDR10: 10-bit output, every static metadata value kept
encode hdr12 pattern_hdr12.mkv
assert_log_contains "bit-depth conversion: 12-bit to 10-bit"
assert_video_pix_fmt "$O/test.mkv" yuv420p10le
assert_frames_match "$O/test.mkv" "$SRC"
assert_hdr_static_match "$O/test.mkv" "$SRC"
assert_stream_value "$O/test.mkv" v:0 stream=color_range,color_space,color_transfer,color_primaries "tv bt2020nc smpte2084 bt2020"

# full range and top-left chroma siting survive
encode fullrange pattern_fullrange.mkv
assert_frames_match "$O/test.mkv" "$SRC"
assert_stream_value "$O/test.mkv" v:0 stream=color_range,color_space,color_transfer,color_primaries,chroma_location "pc bt709 bt709 bt709 topleft"

# 4:2:2, 4:4:4, RGB and gray are brought to 4:2:0 instead of failing
for sub in 422 444; do
    encode "chroma$sub" "pattern_$sub.mkv"
    assert_log_contains "chroma conversion"
    assert_frames_match "$O/test.mkv" "$SRC"
done
assert_video_pix_fmt "$WORKDIR/chroma422/out/test.mkv" yuv420p10le
assert_video_pix_fmt "$WORKDIR/chroma444/out/test.mkv" yuv420p
for fmt in rgb gray; do
    encode "$fmt" "pattern_$fmt.mkv"
    assert_frames_match "$O/test.mkv" "$SRC"
    assert_stream_value "$O/test.mkv" v:0 stream=color_range,color_space,chroma_location "tv bt470bg left"
    assert_video_pix_fmt "$O/test.mkv" yuv420p
done

# interlaced: deinterlaced to one frame per frame; flagged alone, left as it is
encode interlaced pattern_interlaced.mkv 'keep_temp = true'
assert_log_contains "interlaced, top field first"
assert_frames_match "$O/test.mkv" "$SRC" "bwdif=mode=send_frame:parity=tff:deint=all"
assert_frame_times_match "$O/test.mkv" "$SRC"
[ "$(cat "$O/.avet_test/interlace.cache" 2>/dev/null)" = tff ] || fail "interlaced: not detected as top field first"
encode interlaced_bff pattern_interlaced_bff.mkv
assert_log_contains "interlaced, bottom field first"
assert_frames_match "$O/test.mkv" "$SRC" "bwdif=mode=send_frame:parity=bff:deint=all"
assert_frame_times_match "$O/test.mkv" "$SRC"
encode flagged pattern_flagged.mkv 'keep_temp = true'
assert_log_contains "flagged interlaced, but the frames are not"
assert_frames_match "$O/test.mkv" "$SRC"
[ "$(cat "$O/.avet_test/interlace.cache" 2>/dev/null)" = progressive ] || fail "flagged: deinterlaced although the frames are progressive"

# bit_depth converts without touching the picture
encode up8to10 pattern.mkv 'bit_depth = 10'
assert_video_pix_fmt "$O/test.mkv" yuv420p10le
assert_frames_match "$O/test.mkv" "$SRC"

# 8-bit chroma, dithered differently by FFMS2 and by the reference conversion.
encode down12to8 pattern_hdr12.mkv 'bit_depth = 8'
assert_video_pix_fmt "$O/test.mkv" yuv420p
assert_frames_match "$O/test.mkv" "$SRC" null 22

# the color description stays: SD primaries, full range, HLG, JPEG siting
for fixture in color_pal.mkv color_ntsc.mkv color_full8.mkv color_center.avi hlg.mkv; do
    encode "color_$fixture" "$fixture"
    assert_frames_match "$O/test.mkv" "$SRC"
    COLOR=$(stream_value "$SRC" v:0 stream=color_range,color_space,color_transfer,color_primaries,chroma_location)
    case "$COLOR" in
        -|*unknown*unknown*unknown*) fail "$fixture carries no color description to keep" ;;
    esac
    assert_stream_value "$O/test.mkv" v:0 stream=color_range,color_space,color_transfer,color_primaries,chroma_location \
        "$COLOR"
done

test_done
