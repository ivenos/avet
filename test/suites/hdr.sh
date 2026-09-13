#!/bin/sh
# Tests for hdr.rs: HDR type detection, metadata extraction, encoder arg generation.
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
[avet]
hdr = true
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
[avet]
hdr = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "HLG: no output"
assert_log_contains    "HDR: HLG"
assert_color_transfer  "$O/test.mkv" "arib-std-b67"
assert_color_primaries "$O/test.mkv" "bt2020"

# -- hdr=false: no HDR encoder args injected -----------------------------------
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "hdr=false: no output"
assert_log_not_contains "HDR:"
assert_log_not_contains "color-primaries"

# -- svt-av1-hdr encoder binary: encode succeeds with HDR10 source -------------
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/hdr10.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1-hdr"
[encoder_params]
preset = 12
crf    = 50
[avet]
hdr = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "svt-av1-hdr: no output"
assert_file_nonempty    "$O/test.mkv"
assert_log_contains     "HDR: HDR10"

# -- SDR source + hdr=true: "HDR: SDR" logged, bt709 metadata may be injected -
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
hdr = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "SDR+hdr: no output"
assert_log_contains  "HDR: SDR"
assert_file_nonempty "$O/test.mkv"

test_done
