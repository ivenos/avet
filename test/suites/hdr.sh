#!/bin/sh
# Tests for hdr.rs: HDR type detection, static and dynamic metadata, encoder arg generation.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# -- HDR10 source: detected, logged, color_transfer in output file -------------
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

# -- HLG source: detected, logged, color_transfer in output file ---------------
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

# -- svt-av1-hdr encoder binary: encode succeeds with HDR10 source -------------
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

# -- SDR source: nothing logged as HDR --------------------------------------------
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

frames_with_side_data() {
    ffprobe -v error "$1" -select_streams v:0 -show_frames \
        -show_entries frame_side_data=side_data_type -of csv=p=0 | grep -c "$2"
}

assert_frames_with_side_data() {
    local file="$1" pattern="$2" expected="$3"
    local actual
    actual=$(frames_with_side_data "$file" "$pattern")
    [ "$actual" = "$expected" ] || \
        fail "frames with '$pattern': expected $expected, got $actual ($file)"
}

assert_dovi_record() {
    local file="$1" expected="$2"
    local actual
    actual=$(ffprobe -v error "$file" -select_streams v:0 \
        -show_entries stream_side_data=dv_profile,dv_bl_signal_compatibility_id -of default=nw=1:nk=1 | paste -sd, -)
    [ "$actual" = "$expected" ] || \
        fail "Dolby Vision record (profile,compatibility): expected '$expected', got '$actual' ($file)"
}

# -- Dolby Vision 8.1 + HDR10+: both carried per frame, across chunks and a crop -
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
assert_log_contains "2 chunks"
assert_video_frames "$O/test.mkv" 48
assert_video_height "$O/test.mkv" 280
assert_dovi_record  "$O/test.mkv" "10,1"
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48
assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" 48
assert_color_transfer "$O/test.mkv" "smpte2084"
# mkvmerge copies frame 0's metadata OBUs into av1C; avet removes them again.
extradata=$(ffprobe -v error "$O/test.mkv" -select_streams v:0 -show_entries stream=extradata_size -of default=nw=1:nk=1)
[ "${extradata:-999}" -lt 100 ] || fail "av1C still carries frame metadata: $extradata bytes"

# -- Same source without dv, scaled: HDR10+ stays, the RPU does not ------------
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
assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" 48

# -- Dolby Vision 5 without dv: refused -----------------------------------------
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv5.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet_timed "$I" "$O" 60 "job failed"
assert_log_contains   "Set avet.dv = true"
assert_file_not_exists "$O/test.mkv"

# -- Dolby Vision 5 with dv: profile 10.0 --------------------------------------
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv5.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
dv  = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "DV 5: no output"
assert_dovi_record "$O/test.mkv" "10,0"
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48

# -- Dolby Vision 8.4 on HLG: profile 10.4 --------------------------------------
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
assert_color_transfer "$O/test.mkv" "arib-std-b67"
assert_dovi_record    "$O/test.mkv" "10,4"
assert_frames_with_side_data "$O/test.mkv" "Dolby Vision Metadata" 48

# -- Dolby Vision 7 FEL, base and enhancement layer in one track: profile 10.1 --
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

# -- Dolby Vision 7 without dv: the HDR10 base layer alone ----------------------
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

# -- The same Dolby Vision + HDR10+ stream from MPEG-TS and MP4 -----------------
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
    assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" 48
done

# -- video = copy: the source's Dolby Vision and HDR10+ pass through untouched --
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

# -- HDR10+ that stops: the last value carries on, also into chunks behind a seek -
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
assert_log_contains "4 chunks"
assert_video_frames "$O/test.mkv" 96
assert_frames_with_side_data "$O/test.mkv" "SMPTE2094-40" 96
# AverageRGB 123 belongs to frame 23, the last one that has its own metadata.
carried=$(ffprobe -v error "$O/test.mkv" -select_streams v:0 -show_frames \
    -show_entries frame_side_data=average_maxrgb -of default=nw=1:nk=1 | grep -cx "123/100000")
[ "$carried" = 73 ] || fail "frames carrying frame 23's HDR10+: expected 73, got $carried"

test_done
