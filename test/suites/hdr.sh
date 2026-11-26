#!/bin/sh
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# HDR10 source: detected, logged, color_transfer in output file
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HDR10: no output"
assert_log_contains    "HDR: HDR10"
assert_color_transfer  "$O/test.mkv" "smpte2084"
assert_color_primaries "$O/test.mkv" "bt2020"
assert_log_contains    "HDR metadata incomplete"

# HLG source: detected, logged, color_transfer in output file
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hlg.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HLG: no output"
assert_log_contains    "HDR: HLG"
assert_color_transfer  "$O/test.mkv" "arib-std-b67"
assert_color_primaries "$O/test.mkv" "bt2020"

# svt-av1-hdr encoder binary: encode succeeds with HDR10 source
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1-hdr"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "svt-av1-hdr: no output"
assert_file_nonempty    "$O/test.mkv"
assert_log_contains     "HDR: HDR10"

# SDR source: nothing logged as HDR
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "SDR: no output"
assert_log_not_contains "HDR:"
assert_file_nonempty "$O/test.mkv"

# Dolby Vision 8.1 + HDR10+: both carried per frame, across chunks and a crop
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv81_hdr10plus.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
dv   = true
crop = true
[scene_detection]
extra_split = 24
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV 8.1 + HDR10+: no output"
assert_log_contains "HDR: Dolby Vision profile 8 (carried as AV1 profile 10)"
assert_log_contains "encoding: 2 chunks,"
assert_video_frames "$O/test.mkv" 48
assert_video_height "$O/test.mkv" 280
assert_dovi_record  "$O/test.mkv" "10,1"
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48
assert_hdr10plus_matches "$O/test.mkv" "$FIXTURES_DIR/dv81_hdr10plus.mkv"
assert_color_transfer "$O/test.mkv" "smpte2084"
# mkvmerge copies frame 0's metadata OBUs into av1C; avet removes them again.
extradata=$(ffprobe -v error "$O/test.mkv" -select_streams v:0 -show_entries stream=extradata_size -of default=nw=1:nk=1)
[ "${extradata:-999}" -lt 100 ] || fail "av1C still carries frame metadata: $extradata bytes"

# Same source without dv, scaled: HDR10+ stays, the RPU does not
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv81_hdr10plus.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1-hdr"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 180
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HDR10+ only: no output"
assert_log_contains "RPU dropped"
assert_video_height "$O/test.mkv" 180
assert_dovi_record  "$O/test.mkv" ""
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision" 0
assert_hdr10plus_matches "$O/test.mkv" "$FIXTURES_DIR/dv81_hdr10plus.mkv"

# Dolby Vision 5, by its record or by its RPU alone: refused, with dv too
for dv in false true; do
    for src in dv5.mkv dv5_norecord.mp4; do
        I="$WORKDIR/8-$dv-$src/in"; O="$WORKDIR/8-$dv-$src/out"; mkdir -p "$I/p" "$O"
        cp "$FIXTURES_DIR/$src" "$I/p/test.${src##*.}"
        printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\ndv = %s\n' "$dv" > "$I/p/encode.toml"
        run_avet_timed "$I" "$O" 60 "job failed"
        assert_log_contains    "has an IPT base layer"
        assert_file_not_exists "$O/test.mkv"
        assert_file_exists     "$O/.avet_test/.failed"
    done
done

# Dolby Vision 8.4 on HLG: profile 10.4
I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv84.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
dv  = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV 8.4: no output"
assert_log_not_contains "HDR metadata incomplete"
assert_color_transfer "$O/test.mkv" "arib-std-b67"
assert_dovi_record    "$O/test.mkv" "10,4"
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48

# Dolby Vision 7 FEL, base and enhancement layer in one track: profile 10.1
I="$WORKDIR/11/in"; O="$WORKDIR/11/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv7_fel.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
dv  = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV 7: no output"
assert_log_contains "HDR: Dolby Vision profile 7 (carried as AV1 profile 10)"
assert_video_height "$O/test.mkv" 360
assert_video_frames "$O/test.mkv" 48
assert_dovi_record  "$O/test.mkv" "10,1"
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48

# Dolby Vision 7 without dv: the HDR10 base layer alone
I="$WORKDIR/12/in"; O="$WORKDIR/12/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv7_fel.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV 7 without dv: no output"
assert_log_contains   "RPU dropped"
assert_color_transfer "$O/test.mkv" "smpte2084"
assert_dovi_record    "$O/test.mkv" ""
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision" 0

# The same Dolby Vision + HDR10+ stream from MPEG-TS and MP4
for ext in m2ts mp4; do
    I="$WORKDIR/13-$ext/in"; O="$WORKDIR/13-$ext/out"; mkdir -p "$I/p" "$O"
    cp "$FIXTURES_DIR/dv81_hdr10plus.$ext" "$I/p/test.$ext"
    cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
dv  = true
EOF
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV from $ext: no output"
    assert_video_frames "$O/test.mkv" 48
    assert_dovi_record  "$O/test.mkv" "10,1"
    assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48
    assert_hdr10plus_matches "$O/test.mkv" "$FIXTURES_DIR/dv81_hdr10plus.$ext"
done

# video = copy: the source's Dolby Vision and HDR10+ pass through untouched
I="$WORKDIR/14/in"; O="$WORKDIR/14/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv81_hdr10plus.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
[avet]
video = "copy"
dv    = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "copy: no output"
assert_log_contains "ignoring dv"
assert_video_codec  "$O/test.mkv" hevc
assert_dovi_record  "$O/test.mkv" "8,1"
assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" 48

# HDR10+ that stops: the last value carries on behind a seek and past an MP4 cut
I="$WORKDIR/15/in"; O="$WORKDIR/15/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10plus_sparse.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[scene_detection]
extra_split = 24
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "sparse HDR10+: no output"
assert_log_contains "HDR10+: reading it from the bitstream"
assert_log_contains "encoding: 4 chunks,"
assert_video_frames "$O/test.mkv" 96
assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" 96
assert_hdr10plus_matches "$O/test.mkv" "$FIXTURES_DIR/hdr10plus_sparse.mkv"

I="$WORKDIR/16/in"; O="$WORKDIR/16/out"; mkdir -p "$I/p" "$O"
CUT="$WORKDIR/16/cut.mp4"
ffmpeg -nostdin -y -hide_banner -loglevel error -ss 0.5 -i "$FIXTURES_DIR/hdr10plus_sparse.mkv" \
    -map 0:v:0 -c copy "$CUT"
discarded=$(ffprobe -v error -select_streams v:0 -show_entries packet=flags -of csv=p=0 "$CUT" | grep -c D)
[ "$discarded" -gt 0 ] || fail "the cut MP4 has no discarded pictures to test with"
cp "$CUT" "$I/p/test.mp4"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[scene_detection]
extra_split = 24
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HDR10+ from a cut MP4: no output"
assert_log_contains "HDR10+: reading it from the bitstream"
assert_log_not_contains "using the decoder's values"
assert_hdr10plus_matches "$O/test.mkv" "$CUT"

# No RPU on the very first picture: the output still carries a Dolby Vision record
I="$WORKDIR/17/in"; O="$WORKDIR/17/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv81_late_rpu.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
dv = true
[scene_detection]
extra_split = 24
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV without a first RPU: no output"
assert_log_contains "the first frame has no readable Dolby Vision RPU"
assert_video_frames "$O/test.mkv" 49
assert_dovi_record  "$O/test.mkv" "10,1"
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 49

# a damaged HDR10+ cache is read from the bitstream again, not swapped for the decoder's values
I="$WORKDIR/18/in"; O="$WORKDIR/18/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/hdr10plus_sparse.mkv" "$I/p/test.mkv"
printf '{"frames": [' > "$O/.avet_test/hdr10plus.json"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[scene_detection]\nextra_split = 24\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "damaged HDR10+ cache: no output"
assert_log_contains "reading HDR10+ from the bitstream again"
assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" 96

# HLG as DVB sends it, BT.2020 in the VUI and HLG in an alternative transfer SEI
I="$WORKDIR/19/in"; O="$WORKDIR/19/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 24 -pix_fmt yuv420p10le \
    -c:v libx265 -preset ultrafast -x265-params "log-level=error:colorprim=bt2020:transfer=bt2020-10:colormatrix=bt2020nc:atc-sei=18" \
    "$I/p/test.mkv"
[ "$(stream_value "$I/p/test.mkv" v:0 stream=color_transfer)" = bt2020-10 ] || fail "HLG SEI: the source's VUI is not BT.2020"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HLG SEI: no output"
assert_log_contains   "HDR: HLG"
assert_color_transfer "$O/test.mkv" "arib-std-b67"

# Dolby Vision RPUs that stop mid-chunk: the frames after them carry none
I="$WORKDIR/20/in"; O="$WORKDIR/20/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv81_rpu_stops.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\ndv = true\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV whose RPUs stop: no output"
assert_log_contains "encoding: 1 chunk,"
assert_log_contains "24 of 72 frames have no Dolby Vision metadata"
assert_video_frames "$O/test.mkv" 72
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48

# HDR10 and HDR10+ recorded from mid-GOP, the first IRAP past the 48 packets detection reads
I="$WORKDIR/21/in"; O="$WORKDIR/21/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10plus_recording.ts" "$I/p/test.ts"
decoded=$(ffprobe -v quiet -select_streams v:0 -read_intervals '%+#48' -show_frames -show_entries frame=pts -of csv=p=0 "$I/p/test.ts" | wc -l)
[ "$decoded" = 0 ] || fail "HDR10+ recording: the first 48 packets decode $decoded frames"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
TEST_SCENE_REPAIR=1 run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HDR10+ recording: no output"
assert_log_contains     "HDR: HDR10+"
assert_log_not_contains "HDR metadata incomplete"
assert_hdr_static_match "$O/test.mkv" "$FIXTURES_DIR/hdr10plus_gop.mkv"
assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" "$(packet_count "$O/test.mkv")"
assert_hdr10plus_matches "$O/test.mkv" "$FIXTURES_DIR/hdr10plus_recording.ts"

# HDR10 whose Matroska Colour element holds only the matrix: the primaries come from the VUI
I="$WORKDIR/22/in"; O="$WORKDIR/22/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=320x180:rate=24" -frames:v 24 -pix_fmt yuv420p10le \
    -c:v libx265 -preset ultrafast \
    -x265-params "log-level=error:hdr10=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:max-cll=1000,400:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)" \
    -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc "$I/p/test.mkv"
# Since FFmpeg 9.1 (7c2853dfc1), ffprobe fills each colour field the container leaves unset from the bitstream.
[ "$(ffprobe -v error -nofind_stream_info -select_streams v:0 -show_entries stream=color_primaries -of default=nw=1:nk=1 "$I/p/test.mkv")" = unknown ] \
    || fail "HDR10 colour: the source stream has its primaries"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HDR10 colour: no output"
assert_log_contains    "HDR: HDR10"
assert_color_primaries "$O/test.mkv" "bt2020"
assert_color_transfer  "$O/test.mkv" "smpte2084"

# HDR10 at bit_depth = 8 is encoded with a warning
I="$WORKDIR/23/in"; O="$WORKDIR/23/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\nbit_depth = 8\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HDR10 at 8 bits: no output"
assert_log_contains  "[test] HDR: HDR10 is encoded at bit_depth = 8, which shows banding"
assert_video_pix_fmt "$O/test.mkv" "yuv420p"

# Dolby Vision in a second video stream, as on a UHD Blu-ray: the base layer alone, and the log says so
LAYERS="$WORKDIR/400/layers.mkv"; mkdir -p "$WORKDIR/400"
ffmpeg -nostdin -y -hide_banner -loglevel error -i "$FIXTURES_DIR/hdr10.mkv" -i "$FIXTURES_DIR/dv81_hdr10plus.mkv" \
    -map 0:v:0 -map 1:v:0 -c copy "$LAYERS"
[ "$(stream_value "$LAYERS" v:1 stream_side_data=dv_profile)" = 8 ] || fail "two streams: the second has no Dolby Vision record"
for dv in true false; do
    I="$WORKDIR/400/$dv/in"; O="$WORKDIR/400/$dv/out"; mkdir -p "$I/p" "$O"
    cp "$LAYERS" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\ndv = %s\n' "$dv" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "two streams, dv = $dv: no output"
    assert_dovi_record "$O/test.mkv" ""
    if [ "$dv" = true ]; then
        assert_log_contains "HDR: Dolby Vision is in a separate video stream: only the base layer is encoded"
    else
        assert_log_not_contains "separate video stream"
    fi
done

# 10-bit HEVC with a matrix swscale cannot convert: scene detection works and the tag stays
for m in ictcp bt2020c ycgco smpte2085 chroma-derived-nc chroma-derived-c; do
    I="$WORKDIR/401/$m/in"; O="$WORKDIR/401/$m/out"; mkdir -p "$I/p" "$O"
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=256x144:rate=24" -frames:v 24 -pix_fmt yuv420p10le \
        -c:v libx265 -preset ultrafast -x265-params "log-level=error:keyint=24:scenecut=0:colorprim=bt2020:transfer=smpte2084:colormatrix=$m" \
        "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 55\n' > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "matrix $m: no output"
    assert_video_frames "$O/test.mkv" 24
    assert_color_value  "$O/test.mkv" color_space "$m"
done

# a matrix AV1 has no code for fails the job instead of losing its tag
for code in 15 16 17; do
    I="$WORKDIR/402/$code/in"; O="$WORKDIR/402/$code/out"; mkdir -p "$I/p" "$O"
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=256x144:rate=24" -frames:v 24 -pix_fmt yuv420p10le \
        -c:v libx265 -preset ultrafast -x265-params "log-level=error:keyint=24:scenecut=0:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc" \
        -bsf:v "hevc_metadata=matrix_coefficients=$code" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 55\n' > "$I/p/encode.toml"
    run_avet_timed "$I" "$O" 60 "job failed"
    assert_file_not_exists "$O/test.mkv"
    assert_file_exists     "$O/.avet_test/.failed"
    assert_file_exists     "$I/p/test.mkv"
done

# a matrix swscale cannot convert that changes between frames fails instead of labeling them all as one
I="$WORKDIR/403/in"; O="$WORKDIR/403/out"; mkdir -p "$I/p" "$O"
for part in "a 60 bt2020nc" "b 24 ictcp"; do
    set -- $part
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=256x144:rate=24" -frames:v "$2" -pix_fmt yuv420p10le -c:v libx265 -preset ultrafast \
        -x265-params "log-level=error:keyint=24:scenecut=0:bframes=0:colorprim=bt2020:transfer=smpte2084:colormatrix=$3" "$WORKDIR/403/$1.hevc"
done
cat "$WORKDIR/403/a.hevc" "$WORKDIR/403/b.hevc" > "$WORKDIR/403/all.hevc"
mkvmerge -q -o "$I/p/test.mkv" --default-duration 0:24fps "$WORKDIR/403/all.hevc"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 55\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 60 "job failed"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$O/.avet_test/.failed"
assert_file_exists     "$I/p/test.mkv"

refused_for() { # NUMBER MESSAGE
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 55\n' > "$WORKDIR/$1/in/p/encode.toml"
    run_avet_timed "$WORKDIR/$1/in" "$WORKDIR/$1/out" 60 "job failed"
    assert_log_contains    "$2"
    assert_file_not_exists "$WORKDIR/$1/out/test.mkv"
    assert_file_exists     "$WORKDIR/$1/out/.avet_test/.failed"
}

hevc_parts() { # NUMBER PARAMS_A FRAMES_A PARAMS_B FRAMES_B, into $WORKDIR/NUMBER/in/p/test.mkv
    mkdir -p "$WORKDIR/$1/in/p" "$WORKDIR/$1/out"
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=256x144:rate=24" -frames:v "$3" -pix_fmt yuv420p10le -c:v libx265 -preset ultrafast \
        -x265-params "log-level=error:keyint=24:scenecut=0:bframes=0:$2" "$WORKDIR/$1/a.hevc"
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=256x144:rate=24" -frames:v "$5" -pix_fmt yuv420p10le -c:v libx265 -preset ultrafast \
        -x265-params "log-level=error:keyint=24:scenecut=0:bframes=0:$4" "$WORKDIR/$1/b.hevc"
    cat "$WORKDIR/$1/a.hevc" "$WORKDIR/$1/b.hevc" > "$WORKDIR/$1/all.hevc"
    mkvmerge -q -o "$WORKDIR/$1/in/p/test.mkv" --default-duration 0:24fps "$WORKDIR/$1/all.hevc"
}

# SDR frames followed by PQ, the last parameter set in the stream header
hevc_parts 830 "colorprim=bt709:transfer=bt709:colormatrix=bt709" 60 "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc" 24
refused_for 830 "color matrix changes inside the video, from bt709 to bt2020nc"

# the transfer alone goes from SDR to PQ
hevc_parts 831 "colorprim=bt2020:transfer=bt2020-10:colormatrix=bt2020nc" 60 "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc" 24
refused_for 831 "transfer changes inside the video, from bt2020-10 to smpte2084"

# limited range followed by full range, nothing else different
I="$WORKDIR/832/in"; mkdir -p "$I/p" "$WORKDIR/832/out"
for part in "a tv off 0" "b pc on 2"; do
    set -- $part
    ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "color=white:size=320x180:rate=24:d=2" -vf "scale=out_range=$2,format=yuv420p" -color_range "$2" \
        -c:v libx264 -preset ultrafast -bf 2 -x264-params "scenecut=0:keyint=24:repeat-headers=1:colorprim=bt709:transfer=bt709:colormatrix=bt709:fullrange=$3" -f mpegts -output_ts_offset "$4" "$WORKDIR/832/$1.ts"
done
cat "$WORKDIR/832/a.ts" "$WORKDIR/832/b.ts" > "$I/p/test.ts"
refused_for 832 "color range changes inside the video, from tv to pc"

# HLG named by an alternative transfer SEI on the first key frames only: frames decoded after a seek show the VUI
TAGS="colorprim=bt2020:transfer=bt2020-10:colormatrix=bt2020nc"
hevc_parts 833 "$TAGS:atc-sei=18:pic-struct=0" 48 "$TAGS" 48
I="$WORKDIR/833/in"; O="$WORKDIR/833/out"
[ "$(ffprobe -v error -select_streams v:0 -read_intervals "%+#1" -show_entries frame=color_transfer -of csv=p=0 "$I/p/test.mkv" | tr -d ',\n')" = arib-std-b67 ] \
    || fail "SEI fixture: the first frame is not HLG"
ffprobe -v error -select_streams v:0 -read_intervals "3%+#6" -show_entries frame=color_transfer -of csv=p=0 "$I/p/test.mkv" | grep -q bt2020-10 \
    || fail "SEI fixture: the frames after a seek carry the SEI too"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 55\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "SEI on some key frames: no output"
assert_log_contains   "HDR: HLG"
assert_color_transfer "$O/test.mkv" "arib-std-b67"
assert_video_frames   "$O/test.mkv" 96

# a container tag that disagrees with every frame is not a change inside the video
I="$WORKDIR/834/in"; O="$WORKDIR/834/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=256x144:rate=24" -frames:v 48 -pix_fmt yuv420p -c:v libx264 -preset ultrafast \
    -x264-params "keyint=24:scenecut=0:colorprim=bt709:transfer=bt709:colormatrix=bt709" "$WORKDIR/834/v.h264"
mkvmerge -q -o "$I/p/test.mkv" --default-duration 0:24fps --color-matrix-coefficients 0:6 --color-transfer-characteristics 0:6 "$WORKDIR/834/v.h264"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 55\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "container tag against the frames: no output"
assert_log_not_contains "changes inside the video"

test_done
