#!/bin/sh
# The assertions themselves: each has to catch a deliberately broken file and pass the
# intact one, or a green run proves nothing.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

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

# -- scene lists: a gap, an overlap, a late start, a short chunk -------------------------------
printf '[{"index":0,"start_frame":0,"end_frame":99},{"index":1,"start_frame":120,"end_frame":239}]' > "$W/gap.json"
printf '[{"index":0,"start_frame":0,"end_frame":99},{"index":1,"start_frame":90,"end_frame":239}]' > "$W/overlap.json"
printf '[{"index":0,"start_frame":4,"end_frame":239}]' > "$W/late.json"
printf '[{"index":0,"start_frame":0,"end_frame":9},{"index":1,"start_frame":10,"end_frame":239}]' > "$W/short.json"
expect_pass "a gapless scene list"   assert_scenes_cover "$W/one_chunk.json"
expect_pass "two gapless chunks"     assert_scenes_cover "$W/two_chunks.json"
expect_fail "a gap between chunks"   assert_scenes_cover "$W/gap.json"
expect_fail "two chunks overlapping" assert_scenes_cover "$W/overlap.json"
expect_fail "a list starting late"   assert_scenes_cover "$W/late.json"
expect_fail "an empty scene list"    assert_scenes_cover "$W/missing.json"
expect_pass "chunks of 100 frames"   assert_min_chunk_frames "$W/two_chunks.json" 100
expect_fail "a ten-frame chunk"      assert_min_chunk_frames "$W/short.json" 24
expect_pass "a short last chunk"     assert_min_chunk_frames "$W/gap.json" 100
expect_pass "chunks of 140 frames"   assert_max_chunk_frames "$W/two_chunks.json" 140
expect_fail "a chunk 40 frames over" assert_max_chunk_frames "$W/two_chunks.json" 100
expect_fail "a long last chunk"      assert_max_chunk_frames "$W/short.json" 100

# -- frame counts, stream values and track flags ------------------------------------------------
expect_pass "the frame count"        assert_video_frames "$SRC" 240
expect_fail "120 frames too many"    assert_video_frames "$FIXTURES_DIR/pattern_vfr.mkv" 240
expect_fail "counting an absent file" assert_video_frames "$W/missing.mkv" 240
expect_pass "the pixel format"       assert_stream_value "$SRC" v:0 stream=pix_fmt yuv420p
expect_fail "the wrong one"          assert_stream_value "$SRC" v:0 stream=pix_fmt yuv420p10le
expect_fail "no expected value"      assert_stream_value "$SRC" v:0 stream_side_data=rotation ""
expect_pass "no rotation, spelled out" assert_stream_value "$SRC" v:0 stream_side_data=rotation -
expect_pass "language and flags"     assert_track_flags_match "$SRC" "$SRC" a
expect_fail "a lost default flag"    assert_track_flags_match "$W/flag.mkv" "$SRC" a

# -- HDR static metadata and the dynamic side data ----------------------------------------------
MD="G(13250,34500)B(7500,3000)R(34000,16000)"
for wp in 15636,16451 15635,16450; do
    $FF -f lavfi -i "color=c=gray:size=64x64:rate=24" -frames:v 2 -pix_fmt yuv420p10le \
        -vf "setparams=range=tv:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc" \
        -c:v libx265 -x265-params \
        "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=${MD}WP($wp)L(10000000,5):max-cll=1017,391" \
        "$W/wp$wp.mkv"
done
need "$W/wp15636,16451.mkv" "$W/wp15635,16450.mkv"
expect_pass "the same mastering display" assert_hdr_static_match "$W/wp15636,16451.mkv" "$W/wp15636,16451.mkv"
# One unit of x265's 1/50000 grid: exactly what rounding to four decimals costs.
expect_fail "a white point off by one"   assert_hdr_static_match "$W/wp15635,16450.mkv" "$W/wp15636,16451.mkv"
expect_fail "a source with no metadata"  assert_hdr_static_match "$SRC" "$SRC"

DV="$FIXTURES_DIR/dv81_hdr10plus.mkv"
expect_pass "HDR10+ on every frame"  assert_frames_with_side_data "$DV" "SMPTE2094-40" 48
expect_fail "one frame too few"      assert_frames_with_side_data "$DV" "SMPTE2094-40" 47
expect_fail "frames carrying none"   assert_frames_with_side_data "$SRC" "SMPTE2094-40" 48
expect_pass "the Dolby Vision record" assert_dovi_record "$DV" "8,1"
expect_fail "the wrong profile"      assert_dovi_record "$DV" "10,1"
expect_fail "no record at all"       assert_dovi_record "$SRC" "8,1"

expect_pass "seeks landing correctly" assert_seeks_land_on_frames "$SRC"

# -- the plain ffprobe assertions the suites lean on most --------------------------------
# pattern.mkv: 320x180 h264, flac "English" (eng, stereo) and aac "Kommentar" (ger),
# three subtitle tracks eng/ger/jpn, an attachment and three chapters.
: > "$W/empty.mkv"
mkdir -p "$W/adir"
expect_pass "a file that is there"   assert_file_exists "$SRC"
expect_fail "a file that is not"     assert_file_exists "$W/missing.mkv"
expect_pass "a file with content"    assert_file_nonempty "$SRC"
expect_fail "a zero-byte file"       assert_file_nonempty "$W/empty.mkv"
expect_pass "an absent file"         assert_file_not_exists "$W/missing.mkv"
expect_fail "a file that exists"     assert_file_not_exists "$SRC"
expect_pass "a directory"            assert_dir_exists "$W/adir"
expect_fail "a missing directory"    assert_dir_exists "$W/nodir"
expect_pass "no such directory"      assert_dir_not_exists "$W/nodir"
expect_fail "a directory that is there" assert_dir_not_exists "$W/adir"

expect_pass "a readable file"        assert_probeable "$SRC"
expect_fail "a zero-byte one"        assert_probeable "$W/empty.mkv"
expect_fail "one ffprobe rejects"    assert_probeable "$W/font.ttf"

expect_pass "two audio tracks"       assert_audio_track_count "$SRC" 2
expect_fail "counting three"         assert_audio_track_count "$SRC" 3
expect_pass "the audio codec"        assert_audio_codec "$SRC" 0 flac
expect_fail "the wrong codec"        assert_audio_codec "$SRC" 0 opus
expect_pass "the channel count"      assert_audio_channels "$SRC" 0 2
expect_fail "the wrong count"        assert_audio_channels "$SRC" 0 6
expect_pass "the audio language"     assert_audio_language "$SRC" 1 ger
expect_fail "the wrong language"     assert_audio_language "$SRC" 1 jpn
expect_pass "the track title"        assert_audio_title "$SRC" 0 English
expect_fail "the wrong title"        assert_audio_title "$SRC" 0 Deutsch

expect_pass "three subtitle tracks"  assert_subtitle_track_count "$SRC" 3
expect_fail "counting two"           assert_subtitle_track_count "$SRC" 2
expect_pass "the subtitle language"  assert_subtitle_language "$SRC" 2 jpn
expect_fail "the wrong one"          assert_subtitle_language "$SRC" 2 eng

expect_pass "the video codec"        assert_video_codec "$SRC" h264
expect_fail "the wrong codec"        assert_video_codec "$SRC" av1
expect_pass "the video height"       assert_video_height "$SRC" 180
expect_fail "the wrong height"       assert_video_height "$SRC" 1080
expect_pass "at most its height"     assert_video_height_le "$SRC" 180
expect_fail "one line over"          assert_video_height_le "$SRC" 179
expect_fail "measuring nothing"      assert_video_height_le "$W/missing.mkv" 180
expect_pass "under the limit"        assert_video_height_lt "$SRC" 181
expect_fail "not under it"           assert_video_height_lt "$SRC" 180
expect_pass "the pixel format again" assert_video_pix_fmt "$SRC" yuv420p
expect_fail "a ten-bit one"          assert_video_pix_fmt "$SRC" yuv420p10le

expect_pass "the HLG transfer"       assert_color_transfer "$FIXTURES_DIR/hlg.mkv" arib-std-b67
expect_fail "PQ instead"             assert_color_transfer "$FIXTURES_DIR/hlg.mkv" smpte2084
expect_pass "the BT.2020 primaries"  assert_color_primaries "$FIXTURES_DIR/hlg.mkv" bt2020
expect_fail "BT.709 instead"         assert_color_primaries "$FIXTURES_DIR/hlg.mkv" bt709

RUN_LOGS="[test] encoding: 4 chunks
[test] done"
expect_pass "a line that is logged"  assert_log_contains "4 chunks"
expect_fail "a line that is not"     assert_log_contains "job failed"
expect_pass "a line that is absent"  assert_log_not_contains "job failed"
expect_fail "a line that is present" assert_log_not_contains "4 chunks"
RUN_LOGS=""

# A file no seek can read at all must not pass as "every seek landed".
expect_fail "seeking in nothing"     assert_seeks_land_on_frames "$W/missing.mkv"

test_done
