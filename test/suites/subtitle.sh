#!/bin/sh
# Tests for subtitle.rs: track selection, strip mode, language whitelist.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# -- copy: all subtitle tracks preserved --------------------------------------
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
mode = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "copy: no output"
assert_subtitle_track_count "$O/test.mkv" 2

# -- strip: no subtitle tracks in output ---------------------------------------
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
mode = "strip"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "strip: no output"
assert_subtitle_track_count "$O/test.mkv" 0

# -- language_whitelist: only matching track kept ------------------------------
I="$WORKDIR/3/in"; O="$WORKDIR/3/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
language_whitelist = ["deu"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "whitelist: no output"
assert_subtitle_track_count "$O/test.mkv" 1
# Counting alone would pass with the English track kept. mkvmerge writes the
# bibliographic form, so "deu" comes back out as "ger".
assert_subtitle_language    "$O/test.mkv" 0 ger

# -- whitelist written in the other ISO 639-2 spelling still matches ----------
# The everyday case: feeding an avet output back into the profile that produced it.
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
ffmpeg -y -hide_banner -loglevel error -i "$FIXTURES_DIR/sdr_subtitles.mkv" \
    -map 0 -c copy -metadata:s:s:1 language=ger "$I/p/test.mkv" \
    || fail "could not build ger-tagged fixture"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
language_whitelist = ["deu"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "iso alias: no output"
assert_subtitle_track_count "$O/test.mkv" 1

# -- source without subtitles + copy: 0 tracks, no error ----------------------
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
mode = "copy"
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "no-sub source: no output"
assert_subtitle_track_count "$O/test.mkv" 0

# -- whitelist with no matching language: 0 subtitle tracks -------------------
I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[subtitles]
language_whitelist = ["fra"]
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "sub whitelist no match: no output"
assert_subtitle_track_count "$O/test.mkv" 0

# -- default (no [subtitles] section): all subtitle tracks preserved -----------
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_subtitles.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "sub default: no output"
assert_subtitle_track_count "$O/test.mkv" 2

run_subs() { # NAME FIXTURE [SUBTITLE_LINES]
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n%b' "${3:-}" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "$1: no output"
}

# -- bitmap and timed text from every container: the same tracks, shown at the same time -------
run_subs bitmap subs_bitmap.mkv
assert_subtitle_track_count "$O/test.mkv" 3
assert_tracks_match "$O/test.mkv" "$SRC" s
for i in 0 1 2; do
    assert_subtitle_events_match "$O/test.mkv" "s:$i" "$SRC" "s:$i"
done

for fixture in subs_bitmap.m2ts subs_dvb.ts subs_text.mp4; do
    run_subs "$fixture" "$fixture"
    assert_subtitle_track_count "$O/test.mkv" 2
    [ "$fixture" = subs_text.mp4 ] || assert_tracks_match "$O/test.mkv" "$SRC" s
    for i in 0 1; do
        assert_subtitle_events_match "$O/test.mkv" "s:$i" "$SRC" "s:$i"
    done
done
assert_subtitle_language "$O/test.mkv" 0 eng
assert_subtitle_language "$O/test.mkv" 1 ger

run_subs whitelist_dvb subs_dvb.ts '[subtitles]\nlanguage_whitelist = ["deu"]\n'
assert_subtitle_track_count "$O/test.mkv" 1
assert_subtitle_language "$O/test.mkv" 0 ger
assert_subtitle_events_match "$O/test.mkv" s:0 "$SRC" s:0

# -- a TTML track Matroska cannot hold goes, and the whitelist still picks the right track -----
run_subs ttml subs_ttml.mp4 '[subtitles]\nlanguage_whitelist = ["eng"]\n'
assert_log_contains "(ttml) cannot be stored in Matroska - skipped"
assert_subtitle_track_count "$O/test.mkv" 1
assert_subtitle_language "$O/test.mkv" 0 eng
assert_subtitle_events_match "$O/test.mkv" s:0 "$SRC" s:1

run_subs ttml_ger subs_ttml.mp4 '[subtitles]\nlanguage_whitelist = ["deu"]\n'
assert_subtitle_track_count "$O/test.mkv" 0

test_done
