#!/bin/sh
# Audio codecs and channel layouts: copied bit for bit, decoded in sync, every channel in place.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

VIDEO='encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n'
OPUS='[audio]\nmode = "encode"\ncodec = "libopus"\nbitrate = "192k"\n[audio.lossless]\ncodec = "flac"\n'

run() { # NAME FIXTURE AUDIO_LINES
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    SRC="$FIXTURES_DIR/$2"
    cp "$SRC" "$I/p/test.${2##*.}"
    printf '%b%b' "$VIDEO" "${3:-}" > "$I/p/encode.toml"
    run_avet "$I" "$O" "$O/test.mkv" 300 || fail "$1: no output"
}

# -- copy: every codec Matroska holds, the same samples at the same time ----------------------
run copy audio_codecs.mkv
assert_tracks_match "$O/test.mkv" "$SRC" a
i=0
for codec in ac3 eac3 dts truehd mp2 mp3 aac alac flac wavpack tta vorbis opus pcm_f32le; do
    assert_audio_codec "$O/test.mkv" "$i" "$codec"
    # mkvmerge drops a TrueHD stream's last frame of 40 samples.
    missing=0; [ "$codec" = truehd ] && missing=40
    assert_audio_samples_identical "$O/test.mkv" "$SRC" "$i" "$i" "$missing"
    assert_av_sync "$O/test.mkv" "$SRC" "$i"
    i=$((i + 1))
done

# -- decoded: Opus, or FLAC from a lossless source --------------------------------------------
run encode audio_codecs.mkv "$OPUS"
i=0
for codec in opus opus opus flac opus opus opus flac flac flac flac opus opus flac; do
    assert_audio_codec "$O/test.mkv" "$i" "$codec"
    [ "$codec" = flac ] && [ "$i" != 13 ] && assert_audio_samples_identical "$O/test.mkv" "$SRC" "$i"
    assert_av_sync "$O/test.mkv" "$SRC" "$i"
    i=$((i + 1))
done

# -- Blu-ray and broadcast: LPCM and SMPTE 302M as PCM, a stream announced but never sent -----
run m2ts audio_codecs.m2ts
assert_log_contains "audio track 2 (ac3) has no sample rate or channel count - skipped"
assert_audio_track_count "$O/test.mkv" 6
set -- "0 0 pcm_s24le" "1 1 truehd" "2 3 dts" "3 4 ac3" "4 5 pcm_s24le" "5 6 aac_latm"
for track; do
    set -- $track
    assert_audio_codec "$O/test.mkv" "$1" "$3"
    missing=0; [ "$3" = truehd ] && missing=40
    assert_audio_samples_identical "$O/test.mkv" "$SRC" "$1" "$2" "$missing"
    assert_av_sync "$O/test.mkv" "$SRC" "$1" "$2"
done

# -- MP4: AAC and MP3 lose their priming, which Matroska cannot mark, and stay in sync ---------
run mp4 audio_codecs.mp4
i=0
for codec in aac aac alac mp3 ac3; do
    assert_audio_codec "$O/test.mkv" "$i" "$codec"
    assert_av_sync "$O/test.mkv" "$SRC" "$i"
    i=$((i + 1))
done
# AC-3's 256 priming samples are less than half a frame, which stays in.
assert_audio_samples_identical "$O/test.mkv" "$SRC" 2

run mp4_encode audio_codecs.mp4 "$OPUS"
for i in 0 1 2 3 4; do
    assert_av_sync "$O/test.mkv" "$SRC" "$i"
done

# -- Opus: each channel on its own speaker, with silence where the layout has more -------------
run layouts tones_layouts.mov '[audio]\nmode = "encode"\ncodec = "libopus"\nbitrate = "256k"\n'
set -- "0 300 500 0 90 0 0" \
       "1 300 500 700" \
       "2 300 500 700 0 1100 0 0" \
       "3 300 500 1300 1500" \
       "4 300 500 700 1100 1300" \
       "5 300 500 700 1300 1500" \
       "6 300 500 700 0 1100 1300 1500" \
       "7 300 500 700 90 1100 1300 1500" \
       "8 300 500 700 90 1500 1100 1300" \
       "9 300 500 700 0 1100 1300 1500 1700" \
       "10 300 500 700 0 1500 1100 1300"
for track; do
    assert_channel_frequencies "$O/test.mkv" "${track%% *}" "${track#* }"
done
# No Opus layout has front center pairs or a back center next to back left and right.
assert_audio_channels "$O/test.mkv" 11 8
assert_audio_channels "$O/test.mkv" 12 8

run layouts_flac tones_layouts.mov '[audio]\nmode = "encode"\ncodec = "flac"\n'
i=0
while [ $i -lt 13 ]; do
    assert_audio_samples_identical "$O/test.mkv" "$SRC" "$i"
    assert_stream_value "$O/test.mkv" "a:$i" stream=channel_layout \
        "$(stream_value "$SRC" "a:$i" stream=channel_layout)"
    i=$((i + 1))
done

test_done
