#!/bin/sh
# Tests for config.rs: TOML parsing, validation errors, encoder param serialization.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# unknown encoder value: TOML deserialization fails
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "x264"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "parse encode.toml"

# invalid TOML syntax: parse error
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'this is not valid toml !!!\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "parse encode.toml"

# audio.mode=encode without codec: validation error
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
bitrate = "96k"
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "audio: codec required when mode = encode"

# codec_rule encode without bitrate: validation error
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio.codec_rules]
ac3 = { mode = "encode", codec = "aac" }
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "audio.codec_rules.ac3: bitrate required when mode = encode"

# TOML bool param serialized as 1/0, not true/false
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset       = 12
crf          = 50
fast-decode  = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "bool param: no output"
assert_log_contains     "fast-decode=1"
assert_log_not_contains "fast-decode=true"

# all encoder param types appear in "encoder args:" log
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset           = 12
crf              = 50
film-grain       = 8
film-grain-denoise = 0
tune             = 0
fast-decode      = 1
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "param types: no output"
assert_log_contains "film-grain=8"
assert_log_contains "film-grain-denoise=0"
assert_log_contains "tune=0"
assert_log_contains "fast-decode=1"

# audio.mode=encode without bitrate: validation error
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode  = "encode"
codec = "aac"
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "audio: bitrate required when mode = encode"

# no encoder without video=copy: validation error
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "encoder is required"

# avet.bit_depth = 12: validation error
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
bit_depth = 12
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "bit_depth"

# misspelled key is rejected, not silently ignored
I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
bitdepth = 10
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "unknown field"

# misspelled section is rejected too
# Otherwise it parses as "no target quality configured" and the job runs on silently.
I="$WORKDIR/11/in"; O="$WORKDIR/11/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[target_qualtiy]
jod = 9.5
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "unknown field"

# avet.scale = 0 is rejected instead of becoming a no-op
I="$WORKDIR/12/in"; O="$WORKDIR/12/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 0
EOF
run_avet_timed "$I" "$O" 15 "ERROR"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "avet.scale must be at least 64"
# The profile is broken, not the file, so the next scan has to pick it up after the fix.
assert_dir_not_exists  "$O/.avet_test"

# a key or value only the encoder checks marks the file failed until encode.toml is edited
for encoder in svt-av1 svt-av1-hdr; do
    I="$WORKDIR/13-$encoder/in"; O="$WORKDIR/13-$encoder/out"; mkdir -p "$I/p" "$O"
    cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
    printf 'encoder = "%s"\n[encoder_params]\npreset = 12\ncrf = 50\nprest = 6\n' "$encoder" > "$I/p/encode.toml"
    run_avet_timed "$I" "$O" 60 "job failed"
    assert_log_contains    "Error in configuration"
    assert_file_exists     "$O/.avet_test/.failed"
    assert_file_exists     "$O/.avet_test/failed.profile"
    assert_file_not_exists "$O/test.mkv"
    TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 15 "no jobs"
    assert_log_contains    "permanently failed"
    assert_log_contains    "or change encode.toml to retry"
    printf 'encoder = "%s"\n[encoder_params]\npreset = 12\ncrf = 50\n' "$encoder" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "$encoder: fixing the profile did not lift the marker"
    assert_dir_not_exists  "$O/.avet_test"
done

# SvtAv1EncApp exits 0 on this one, and only its closed input shows it.
I="$WORKDIR/14/in"; O="$WORKDIR/14/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 99\ncrf = 50\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 60 "job failed"
assert_log_contains    "EncoderMode must be in the range"
assert_file_exists     "$O/.avet_test/.failed"

# a marker without the record of its profile, as written before there was one, stays
rm -f "$O/.avet_test/failed.profile"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 15 "no jobs"
assert_log_contains    "permanently failed"
assert_file_not_exists "$O/test.mkv"

I="$WORKDIR/15/in"; O="$WORKDIR/15/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[audio]
mode    = "encode"
codec   = "flac"
options = { compresion_level = 10 }
EOF
run_avet_timed "$I" "$O" 120 "job failed"
assert_file_not_exists "$O/test.mkv"
assert_log_contains    "Option not found"
assert_file_exists     "$O/.avet_test/.failed"
sed -i 's/compresion_level/compression_level/' "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "audio option: fixing the profile did not lift the marker"

I="$WORKDIR/16/in"; O="$WORKDIR/16/out"; mkdir -p "$I/p" "$O"
ffmpeg -nostdin -y -hide_banner -loglevel error -f lavfi -i "testsrc2=size=16386x64:rate=24" -frames:v 24 \
    -c:v ffv1 "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 120 "job failed"
assert_log_contains    "Source Width must be less than or equal to 16384"
assert_file_exists     "$O/.avet_test/.failed"

test_done
