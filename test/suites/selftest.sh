#!/bin/sh
# The assertions themselves: each has to catch a deliberately broken file and pass the
# intact one, or a green run proves nothing.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

SRC="$FIXTURES_DIR/pattern.mkv"
W="$WORKDIR"
FF="ffmpeg -nostdin -y -hide_banner -loglevel error"

expect_fail() { # DESCRIPTION ASSERTION...
    local desc="$1" fails="$_FAIL" errors="$_ERRORS" caught
    shift
    _FAIL=0; _ERRORS=""
    "$@"
    caught="$_FAIL"
    _FAIL="$fails"; _ERRORS="$errors"
    [ "$caught" -eq 1 ] || fail "selftest: $desc went unnoticed"
}

expect_pass() { # DESCRIPTION ASSERTION...
    local desc="$1" fails="$_FAIL" errors="$_ERRORS" caught report
    shift
    _FAIL=0; _ERRORS=""
    "$@"
    caught="$_FAIL"; report="$_ERRORS"
    _FAIL="$fails"; _ERRORS="$errors"
    [ "$caught" -eq 0 ] || fail "selftest: $desc failed on intact input: $report"
}

x264() { printf '%s' "-c:v libx264 -qp 0 -preset ultrafast"; }

# A broken variant that was never written would make its expect_fail pass for nothing.
need() {
    for f in "$@"; do
        [ -s "$f" ] || { fail "selftest: could not build $f"; test_done; }
    done
}

# -- frames: swapped, shifted and mis-cropped pictures ------------------------------------
$FF -i "$SRC" -map 0:v -vf "shuffleframes=0 1 2 3 5 4" $(x264) "$W/swapped.mkv"
$FF -i "$SRC" -map 0:v -vf "trim=start_frame=1,setpts=PTS-STARTPTS,tpad=stop=1:stop_mode=clone" $(x264) "$W/shifted.mkv"
$FF -i "$FIXTURES_DIR/pattern_bars.mkv" -vf "crop=640:276:0:46" $(x264) "$W/crop46.mkv"
need "$W/swapped.mkv" "$W/shifted.mkv" "$W/crop46.mkv"
expect_pass "identical frames"       assert_frames_match "$SRC" "$SRC"
expect_fail "two swapped frames"     assert_frames_match "$W/swapped.mkv" "$SRC"
expect_fail "every frame one late"   assert_frames_match "$W/shifted.mkv" "$SRC"
expect_fail "a crop two lines low"   assert_frames_match "$W/crop46.mkv" "$FIXTURES_DIR/pattern_bars.mkv" "crop=640:276:0:44"
expect_pass "the right crop"         assert_frames_match "$W/crop46.mkv" "$FIXTURES_DIR/pattern_bars.mkv" "crop=640:276:0:46"
expect_fail "a missing frame"        assert_frames_match "$FIXTURES_DIR/pattern_vfr.mkv" "$SRC"

# -- frame times: one frame five milliseconds late ------------------------------------------
{ echo "# timestamp format v2"; i=0; while [ $i -lt 240 ]; do
      if [ $i -eq 100 ]; then echo "$((i * 1000 / 24 + 5))"; else echo "$((i * 1000 / 24))"; fi
      i=$((i + 1)); done; } > "$W/late.txt"
mkvmerge -q -o "$W/late.mkv" --timestamps "0:$W/late.txt" "$SRC"
need "$W/late.mkv"
expect_pass "unchanged frame times"  assert_frame_times_match "$SRC" "$SRC"
expect_fail "one late frame"         assert_frame_times_match "$W/late.mkv" "$SRC"

# -- sync, packets, stream times, tracks, chapters, attachments -----------------------------
mkvmerge -q -o "$W/audio40.mkv" --sync 1:40 "$SRC"
mkvmerge -q -o "$W/sub100.mkv" --sync 3:100 "$SRC"
mkvmerge -q -o "$W/chapters50.mkv" --chapter-sync 50 "$SRC"
mkvmerge -q -o "$W/nochapters.mkv" --no-chapters "$SRC"
mkvmerge -q -o "$W/flag.mkv" --default-track-flag 1:no "$SRC"
head -c 4096 /dev/zero > "$W/font.ttf"
mkvmerge -q -o "$W/attachment.mkv" --no-attachments "$SRC" --attachment-mime-type font/ttf --attach-file "$W/font.ttf"
$FF -i "$SRC" -map 0 -c copy -c:a:0 flac -compression_level 0 "$W/reflac.mkv"
$FF -i "$SRC" -map 0 -c copy -c:a:0 flac -filter:a:0 volume=0.99 "$W/quieter.mkv"
need "$W/audio40.mkv" "$W/sub100.mkv" "$W/chapters50.mkv" "$W/nochapters.mkv" "$W/flag.mkv" \
    "$W/attachment.mkv" "$W/reflac.mkv" "$W/quieter.mkv"
expect_pass "audio in sync"          assert_av_sync "$SRC" "$SRC" 0
expect_fail "audio 40 ms late"       assert_av_sync "$W/audio40.mkv" "$SRC" 0
expect_pass "identical packets"      assert_packets_identical "$SRC" a:0 "$SRC" a:0
expect_fail "re-encoded packets"     assert_packets_identical "$W/reflac.mkv" a:0 "$SRC" a:0
expect_pass "same samples"           assert_audio_samples_identical "$W/reflac.mkv" "$SRC" 0
expect_fail "quieter samples"        assert_audio_samples_identical "$W/quieter.mkv" "$SRC" 0
expect_pass "subtitles on time"      assert_stream_times_match "$SRC" s:0 "$SRC" s:0
expect_fail "subtitles 100 ms late"  assert_stream_times_match "$W/sub100.mkv" s:0 "$SRC" s:0
expect_pass "same tracks"            assert_tracks_match "$SRC" "$SRC" a
expect_fail "a lost default flag"    assert_tracks_match "$W/flag.mkv" "$SRC" a
expect_pass "same chapters"          assert_chapters_match "$SRC" "$SRC"
expect_fail "chapters 50 ms late"    assert_chapters_match "$W/chapters50.mkv" "$SRC"
expect_fail "no chapters"            assert_chapters_match "$W/nochapters.mkv" "$SRC"
expect_pass "same attachments"       assert_attachments_match "$SRC" "$SRC"
expect_fail "a changed attachment"   assert_attachments_match "$W/attachment.mkv" "$SRC"

# -- a corrupted stream, a swapped channel, changed bytes -------------------------------------
cp "$SRC" "$W/corrupt.mkv"
# A single block of random bytes still decodes cleanly about once in 200 runs.
corrupt_size=$(stat -c %s "$W/corrupt.mkv")
for part in 1 2 3; do
    dd if=/dev/urandom of="$W/corrupt.mkv" bs=1 seek=$((corrupt_size * part / 4)) \
        count=4096 conv=notrunc 2>/dev/null
done
$FF -i "$FIXTURES_DIR/tones_71.mkv" -map 0 -c:v copy -c:a flac -af "pan=7.1|FL=FR|FR=FL|FC=FC|LFE=LFE|BL=BL|BR=BR|SL=SL|SR=SR" "$W/swapped_lr.mkv"
need "$W/corrupt.mkv" "$W/swapped_lr.mkv"
expect_pass "a clean decode"         assert_decodes_cleanly "$SRC"
expect_fail "corrupted packets"      assert_decodes_cleanly "$W/corrupt.mkv"
expect_pass "channels in place"      assert_channel_frequencies "$FIXTURES_DIR/tones_71.mkv" 0 "300 500 700 90 1100 1300 1700 1900"
expect_fail "left and right swapped" assert_channel_frequencies "$W/swapped_lr.mkv" 0 "300 500 700 90 1100 1300 1700 1900"
expect_pass "same bytes"             assert_same_bytes "$SRC" "$SRC"
expect_fail "changed bytes"          assert_same_bytes "$W/corrupt.mkv" "$SRC"

# -- pictures bit for bit, subtitle events, chunk keyframes, samples cut short ---------------------
printf '[{"index":0,"start_frame":0,"end_frame":239}]' > "$W/one_chunk.json"
printf '[{"index":0,"start_frame":0,"end_frame":99},{"index":1,"start_frame":100,"end_frame":239}]' > "$W/two_chunks.json"
$FF -i "$SRC" -map 0:a:0 -af "atrim=end_sample=479960" -c:a flac "$W/short40.mkv"
$FF -i "$SRC" -map 0:a:0 -af "atrim=end_sample=479900" -c:a flac "$W/short100.mkv"
need "$W/short40.mkv" "$W/short100.mkv"
expect_pass "the same pictures"      assert_frames_identical "$SRC" "$SRC"
expect_fail "two swapped pictures"   assert_frames_identical "$W/swapped.mkv" "$SRC"
expect_pass "subtitles shown on time" assert_subtitle_events_match "$SRC" s:0 "$SRC" s:0
expect_fail "subtitles 100 ms late"  assert_subtitle_events_match "$W/sub100.mkv" s:0 "$SRC" s:0
expect_pass "a chunk on a keyframe"  assert_keyframes_at_chunks "$SRC" "$W/one_chunk.json"
expect_fail "a chunk without one"    assert_keyframes_at_chunks "$SRC" "$W/two_chunks.json"
expect_pass "40 samples short, allowed"  assert_audio_samples_identical "$W/short40.mkv" "$SRC" 0 0 40
expect_fail "40 samples short"       assert_audio_samples_identical "$W/short40.mkv" "$SRC" 0
expect_fail "100 samples short"      assert_audio_samples_identical "$W/short100.mkv" "$SRC" 0 0 40

test_done
