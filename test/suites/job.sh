#!/bin/sh
# Tests for job.rs: full encode pipeline, lifecycle, scaling, resume, failure handling.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)

# baseline: output created, source in processed/, temp dir removed
I="$WORKDIR/1/in"; O="$WORKDIR/1/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "baseline: no output"
assert_file_nonempty   "$O/test.mkv"
assert_file_exists     "$I/processed/test.mkv"
assert_file_not_exists "$I/p/test.mkv"
assert_dir_not_exists  "$O/.avet_test"
assert_log_contains    "% of the source) in "

# keep_temp=true: temp dir preserved
I="$WORKDIR/2/in"; O="$WORKDIR/2/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "keep_temp=true: no output"
assert_dir_exists "$O/.avet_test"

# scale: 720p down to 360p, and a smaller source is not scaled up
I="$WORKDIR/4/in"; O="$WORKDIR/4/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_720p.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 360
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "scale down: no output"
assert_video_height  "$O/test.mkv" 360
assert_log_contains  "auto-scale"
assert_log_contains  "workers:"

I="$WORKDIR/5/in"; O="$WORKDIR/5/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 720
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "scale noop: no output"
assert_video_height "$O/test.mkv" 360

# resume: pre-created frame index is reused
I="$WORKDIR/6/in"; O="$WORKDIR/6/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
EOF
docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "${I}:/input:z" \
    -v "${O}:/output:z" \
    --entrypoint ffmsindex \
    "${TEST_IMAGE:-avet:test}" \
    /input/p/test.mkv /output/.avet_test/frame-index.ffindex
[ -f "$O/.avet_test/frame-index.ffindex" ] || fail "resume: ffmsindex produced no index"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "resume: no output"
assert_log_contains "reusing existing index"

# multi-file: two videos in same profile, both encoded and moved
I="$WORKDIR/7/in"; O="$WORKDIR/7/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/alpha.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/beta.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/beta.mkv" 240 || fail "multi-file: no output"
assert_file_nonempty "$O/alpha.mkv"
assert_file_nonempty "$O/beta.mkv"
assert_file_exists   "$I/processed/alpha.mkv"
assert_file_exists   "$I/processed/beta.mkv"

# multiple profiles: two profile dirs, independent configs
I="$WORKDIR/8/in"; O="$WORKDIR/8/out"
mkdir -p "$I/movies" "$I/series" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv"  "$I/movies/alpha.mkv"
cp "$FIXTURES_DIR/sdr_720p.mkv"    "$I/series/beta.mkv"
cat > "$I/movies/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
cat > "$I/series/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
scale = 360
EOF
run_avet "$I" "$O" "$O/beta.mkv" 240 || fail "multi-profile: no beta output"
assert_file_nonempty "$O/alpha.mkv"
assert_file_nonempty "$O/beta.mkv"
assert_video_height  "$O/beta.mkv" 360
assert_file_exists   "$I/processed/alpha.mkv"
assert_file_exists   "$I/processed/beta.mkv"

# .failed workflow: write marker, block retry, recover after fix
I="$WORKDIR/9/in"; O="$WORKDIR/9/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/dv5.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet_timed "$I" "$O" 60 "job failed"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$O/.avet_test/.failed"
assert_log_contains    "has an IPT base layer"

TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 15 "no jobs"
assert_log_contains "permanently failed"
assert_log_not_contains "indexing"

rm -f "$O/.avet_test/.failed"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail ".failed recovery: no output"
assert_file_nonempty "$O/test.mkv"
assert_file_exists   "$I/processed/test.mkv"

# a folder in a profile comes out as the same folder
I="$WORKDIR/11/in"; O="$WORKDIR/11/out"
mkdir -p "$I/p/Show/Season 1" "$I/p/Show/Season 2" "$I/p/Show/Specials" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Show/Season 1/Episode 01.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Show/Season 2/Episode 01.mkv"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/Show/Specials/Special.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
EOF
run_avet "$I" "$O" "$O/Show/Specials/Special.mkv" 360 || fail "folder: no output"
assert_file_nonempty   "$O/Show/Season 1/Episode 01.mkv"
assert_file_nonempty   "$O/Show/Season 2/Episode 01.mkv"
assert_file_exists     "$I/processed/Show/Season 1/Episode 01.mkv"
assert_file_exists     "$I/processed/Show/Season 2/Episode 01.mkv"
assert_file_exists     "$I/processed/Show/Specials/Special.mkv"
assert_dir_not_exists  "$I/p/Show"
assert_dir_exists      "$I/p"
assert_dir_not_exists  "$O/Show/Season 1/.avet_Episode 01"
assert_file_not_exists "$O/Episode 01.mkv"
assert_log_not_contains "share this name"

# done.json resume: second run skips already-encoded chunks
I="$WORKDIR/10/in"; O="$WORKDIR/10/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
cat > "$I/p/encode.toml" << 'EOF'
encoder = "svt-av1"
[encoder_params]
preset = 12
crf    = 50
[avet]
keep_temp = true
EOF
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "done resume: first encode failed"
cp "$I/processed/test.mkv" "$I/p/test.mkv"
rm  "$O/test.mkv"
TEST_RUST_LOG=debug run_avet "$I" "$O" "$O/test.mkv" 90 || fail "done resume: second encode failed"
assert_log_contains  "already done"
assert_file_nonempty "$O/test.mkv"

# edge cases: a single picture, a name at the 255-byte limit, a file without video, an output larger than its source
I="$WORKDIR/12/in"; O="$WORKDIR/12/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/one_frame.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "one frame: no output"
assert_video_frames "$O/test.mkv" 1
assert_frames_match "$O/test.mkv" "$FIXTURES_DIR/one_frame.mkv"
assert_audio_samples_identical "$O/test.mkv" "$FIXTURES_DIR/one_frame.mkv" 0

I="$WORKDIR/13/in"; O="$WORKDIR/13/out"; mkdir -p "$I/p" "$O"
long=$(printf '%0250d' 0 | tr 0 x)
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/$long.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/$long.mkv" 120 || fail "long name: no output"
assert_file_exists "$I/processed/$long.mkv"

I="$WORKDIR/14/in"; O="$WORKDIR/14/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/audio_only.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 60 "job failed"
assert_file_not_exists "$O/test.mkv"
grep -q "no video track" "$O/.avet_test/.failed" 2>/dev/null || fail "audio only: .failed does not name the missing video"

I="$WORKDIR/16/in"; O="$WORKDIR/16/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[avet]\nvideo = "copy"\n[audio]\nmode = "encode"\ncodec = "pcm_s24le"\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "larger: no output"
assert_log_contains "the output is larger than the source"
assert_audio_codec  "$O/test.mkv" 0 pcm_s24le

# an empty output file is a leftover, not a finished encode
I="$WORKDIR/15/in"; O="$WORKDIR/15/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
: > "$O/test.mkv"
run_avet_timed "$I" "$O" 180 "[test] done"
assert_log_contains  "ignoring empty output file"
assert_video_frames  "$O/test.mkv" 240
assert_file_exists   "$I/processed/test.mkv"

I="$WORKDIR/17/in"; O="$WORKDIR/17/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
: > "$I/processed"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "failed archiving: no output"
assert_log_contains    "archiving the source failed"
assert_file_exists     "$I/p/test.mkv"
assert_file_exists     "$O/.avet_test/delivered"
assert_file_not_exists "$O/.avet_test/done.json"
rm -f "$I/processed"
run_avet_timed "$I" "$O" 30 "already encoded"
assert_log_contains    "already encoded - archiving the source"
assert_file_exists     "$I/processed/test.mkv"
assert_file_not_exists "$I/p/test.mkv"
assert_dir_not_exists  "$O/.avet_test"

I="$WORKDIR/18/in"; O="$WORKDIR/18/out"; mkdir -p "$I/p" "$I/processed" "$O" "$WORKDIR/18/processed"
long=$(printf '%0250d' 0 | tr 0 x)
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/$long.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
TEST_PROCESSED="$WORKDIR/18/processed"
chmod 555 "$I/p"
run_avet "$I" "$O" "$O/$long.mkv" 120 || fail "archive mount: no output"
chmod 755 "$I/p"
assert_log_contains    "remove the original after copying it"
assert_file_exists     "$I/p/$long.mkv"
assert_file_not_exists "$TEST_PROCESSED/$long.mkv"
TEST_RUST_LOG=debug start_avet "$I" "$O"
wait_for_log "no jobs" 30 || fail "archive mount: the source was not archived"
kill_avet
assert_same_bytes      "$TEST_PROCESSED/$long.mkv" "$FIXTURES_DIR/sdr_simple.mkv"
assert_file_not_exists "$I/p/$long.mkv"
[ -z "$(find "$O" -maxdepth 1 -name '.avet_*')" ] || fail "archive mount: the temp dir was left behind"
unset TEST_PROCESSED

# avet takes a tool next to its own binary first, so a script there stands in for mkvmerge
with_mkvmerge() { # NUMBER, the script on stdin
    I="$WORKDIR/$1/in"; O="$WORKDIR/$1/out"; mkdir -p "$I/p" "$O"
    cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
    { echo '#!/bin/sh'; cat; } > "$WORKDIR/$1/mkvmerge"
    chmod +x "$WORKDIR/$1/mkvmerge"
    start_avet "$I" "$O" 999999 -v "$WORKDIR/$1/mkvmerge:/usr/local/bin/mkvmerge:ro,z" "$TEST_IMAGE"
    wait_for_log "job failed" 180 || fail "stand-in mkvmerge $1: the job did not fail"
    kill_avet
}

# a mux that lost frames is not delivered
with_mkvmerge 700 << 'EOF'
out=""; prev=""
for arg; do [ "$prev" = "-o" ] && out=$arg; prev=$arg; done
/usr/bin/mkvmerge "$@"; rc=$?
case "$out" in
    */muxed.mkv) ffmpeg -nostdin -v error -y -i "$out" -map 0 -c copy -frames:v 200 "$out.cut.mkv" && mv "$out.cut.mkv" "$out" ;;
esac
exit $rc
EOF
assert_log_contains    "output holds 200 video frames"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mkv"
assert_file_exists     "$O/.avet_test/.failed"

# a source that changes while its output is muxed is not delivered, and is tried again
with_mkvmerge 701 << 'EOF'
case " $* " in *"/muxed.mkv "*) printf x >> /input/p/test.mkv ;; esac
exec /usr/bin/mkvmerge "$@"
EOF
assert_log_contains    "changed during the encode"
assert_log_contains    "job failed - retrying"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mkv"
assert_file_not_exists "$O/.avet_test/.failed"

# a file that appears under the output's name during the job is not overwritten
with_mkvmerge 702 << 'EOF'
[ ! -f /output/.avet_test/video.ivf ] || [ -e /output/test.mkv ] || printf 'not avet' > /output/test.mkv
exec /usr/bin/mkvmerge "$@"
EOF
printf 'not avet' > "$WORKDIR/702/expected"
assert_log_contains     "output already exists"
assert_log_not_contains "muxing to"
assert_same_bytes       "$O/test.mkv" "$WORKDIR/702/expected"
assert_file_exists      "$I/p/test.mkv"

# chunks that join to fewer frames than the list accounts for stop the job
I="$WORKDIR/703/in"; O="$WORKDIR/703/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n[avet]\nkeep_temp = true\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "short join: first encode failed"
mv "$I/processed/test.mkv" "$I/p/test.mkv"
rm "$O/test.mkv"
# done.json holds a chunk's size: the first IVF frame swallowing the second leaves it at that size.
chunk=$(ls "$O/.avet_test/chunks/"*.ivf | head -n 1)
first=$(od -An -tu4 -j 32 -N 4 "$chunk" | tr -d ' ')
second=$(od -An -tu4 -j $((44 + first)) -N 4 "$chunk" | tr -d ' ')
size=$((first + 12 + second))
printf "$(printf '\\%03o\\%03o\\%03o\\%03o' $((size & 255)) $((size >> 8 & 255)) $((size >> 16 & 255)) $((size >> 24 & 255)))" | \
    dd of="$chunk" bs=1 seek=32 conv=notrunc 2>/dev/null
run_avet_timed "$I" "$O" 120 "job failed"
assert_log_contains    "the joined chunks hold 239 frames"
assert_file_not_exists "$O/test.mkv"
assert_file_exists     "$I/p/test.mkv"

# an option the encoder refuses fails the job behind the scale filter as well
I="$WORKDIR/19/in"; O="$WORKDIR/19/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\nfilm-grain = 99\n[avet]\nscale = 90\n' > "$I/p/encode.toml"
run_avet_timed "$I" "$O" 60 "job failed"
assert_file_exists     "$O/.avet_test/.failed"
assert_file_not_exists "$O/test.mkv"

# a source whose name is taken in processed/ is archived under a number, and the log says so
I="$WORKDIR/20/in"; O="$WORKDIR/20/out"; mkdir -p "$I/p" "$I/processed" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'earlier\n' > "$I/processed/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "archive name taken: no output"
assert_same_bytes   "$I/processed/test.2.mkv" "$FIXTURES_DIR/sdr_simple.mkv"
assert_log_contains "[test] archived as /input/processed/test.2.mkv, the name was taken"

# a source named outside ASCII
I="$WORKDIR/21/in"; O="$WORKDIR/21/out"; mkdir -p "$I/p" "$O"
name="Die Brücke 進撃"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/$name.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
run_avet "$I" "$O" "$O/$name.mkv" 120 || fail "non-ASCII name: no output"
assert_file_exists "$I/processed/$name.mkv"

# the output is in place and another video of the same size takes the source's place before the archiving
I="$WORKDIR/100/in"; O="$WORKDIR/100/out"; FX="$WORKDIR/100/fx"; mkdir -p "$I/p" "$O" "$FX"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "testsrc2=s=320x180:r=24" -frames:v 24 -c:v rawvideo -pix_fmt yuv420p "$FX/a.mkv"
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "testsrc2=s=320x180:r=24,negate" -frames:v 24 -c:v rawvideo -pix_fmt yuv420p "$FX/b.mkv"
[ "$(wc -c < "$FX/a.mkv")" = "$(wc -c < "$FX/b.mkv")" ] || fail "same size: the fixtures differ in size"
cp "$FX/a.mkv" "$I/p/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
: > "$I/processed"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "same size: no output"
assert_file_exists "$O/.avet_test/delivered"
cp "$FX/b.mkv" "$I/p/test.mkv"
rm -f "$I/processed"
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 20 "no jobs"
assert_log_contains     "skip: output exists"
assert_log_not_contains "already encoded"
assert_same_bytes       "$I/p/test.mkv" "$FX/b.mkv"
assert_file_not_exists  "$I/processed/test.mkv"

# an unreadable source with a finished output stays in input until it can be read, then it is archived
I="$WORKDIR/890/in"; O="$WORKDIR/890/out"; mkdir -p "$I/p" "$O"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
: > "$I/processed"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "unreadable source: no output"
assert_file_exists "$O/.avet_test/delivered"
rm -f "$I/processed"
chmod 000 "$I/p/test.mkv"
TEST_RUST_LOG=debug run_avet_timed "$I" "$O" 20 "no jobs"
chmod 644 "$I/p/test.mkv"
assert_log_contains     "skip: output exists"
assert_log_not_contains "already encoded"
assert_same_bytes       "$I/p/test.mkv" "$FIXTURES_DIR/sdr_simple.mkv"
assert_file_not_exists  "$I/processed/test.mkv"
assert_file_exists      "$O/.avet_test/delivered"
run_avet_timed "$I" "$O" 30 "already encoded"
assert_file_exists      "$I/processed/test.mkv"
assert_file_not_exists  "$I/p/test.mkv"
assert_dir_not_exists   "$O/.avet_test"

# links at the work files are gone when the job starts, and their target stays as it was
I="$WORKDIR/891/in"; O="$WORKDIR/891/out"; mkdir -p "$I/p" "$O/.avet_test/chunks"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
printf 'only copy\n' > "$O/library.mkv"
for name in tracks.mkv video.ivf source.mkv chunks/00001.ivf; do
    ln -s /output/library.mkv "$O/.avet_test/$name"
done
run_avet "$I" "$O" "$O/test.mkv" 180 || fail "work file links: no output"
[ "$(cat "$O/library.mkv")" = "only copy" ] || fail "work file links: a target was written through"
assert_log_contains "was a symbolic link"
assert_video_frames "$O/test.mkv" 240

# an output that appears during the mux stays, and its source is not archived
I="$WORKDIR/101/in"; O="$WORKDIR/101/out"; W="$WORKDIR/101/wrap"; mkdir -p "$I/p" "$O" "$W"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
cat > "$W/mkvmerge" << 'EOF'
#!/bin/sh
case " $* " in *muxed.mkv*)
    [ -e /wrap/fired ] || { : > /wrap/fired; printf 'someone else\n' > /output/test.mkv; } ;;
esac
exec "$(PATH=/usr/local/bin:/usr/bin:/bin command -v mkvmerge)" "$@"
EOF
chmod +x "$W/mkvmerge"
start_avet "$I" "$O" 999999 -v "$W:/wrap:z" --entrypoint sh "$TEST_IMAGE" -c 'cp /usr/local/bin/avet /wrap/avet && exec /wrap/avet'
wait_for_log "[test] job failed" 45 || fail "output during mux: the job did not fail"
kill_avet
assert_file_exists     "$W/fired"
assert_log_contains    "output already exists"
[ "$(cat "$O/test.mkv")" = "someone else" ] || fail "output during mux: the file already there was replaced"
assert_same_bytes      "$I/p/test.mkv" "$FIXTURES_DIR/sdr_simple.mkv"
assert_file_not_exists "$I/processed/test.mkv"
assert_file_not_exists "$O/.avet_test/delivered"
assert_file_exists     "$O/.avet_test/.failed"

# a symlink at the archive's scratch name is replaced, not written through
I="$WORKDIR/102/in"; O="$WORKDIR/102/out"; mkdir -p "$I/p" "$I/processed" "$O" "$WORKDIR/102/processed"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
printf 'finished encode\n' > "$O/other.mkv"
TEST_PROCESSED="$WORKDIR/102/processed"
ln -s /output/other.mkv "$TEST_PROCESSED/test.mkv.part"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "part link: no output"
[ "$(cat "$O/other.mkv")" = "finished encode" ] || fail "part link: the link's target was overwritten"
assert_same_bytes "$TEST_PROCESSED/test.mkv" "$FIXTURES_DIR/sdr_simple.mkv"
[ ! -e "$TEST_PROCESSED/test.mkv.part" ] && [ ! -L "$TEST_PROCESSED/test.mkv.part" ] || fail "part link: the scratch name was left behind"
unset TEST_PROCESSED

# a temp dir that is a symlink is replaced, and the end of the job leaves its target alone
I="$WORKDIR/103/in"; O="$WORKDIR/103/out"; mkdir -p "$I/p" "$O/Finished/Season 1"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
printf 'kept\n' > "$O/Finished/movie.mkv"
printf 'kept\n' > "$O/Finished/Season 1/episode.mkv"
ln -s /output/Finished "$O/.avet_test"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "temp link: no output"
assert_log_contains "was a symbolic link"
[ "$(cat "$O/Finished/movie.mkv" "$O/Finished/Season 1/episode.mkv")" = "kept
kept" ] || fail "temp link: the link's target was emptied"
[ "$(ls -A "$O/Finished" | wc -l)" -eq 2 ] || fail "temp link: avet wrote into the link's target"
[ ! -e "$O/.avet_test" ] && [ ! -L "$O/.avet_test" ] || fail "temp link: the temp dir was left behind"

# a failure marker lands in a real temp dir, not in the target of a link
I="$WORKDIR/104/in"; O="$WORKDIR/104/out"; mkdir -p "$I/p" "$O/Finished"
cp "$FIXTURES_DIR/audio_only.mkv" "$I/p/test.mkv"
printf 'encoder = "svt-av1"\n' > "$I/p/encode.toml"
printf 'kept\n' > "$O/Finished/movie.mkv"
ln -s /output/Finished "$O/.avet_test"
run_avet_timed "$I" "$O" 60 "job failed"
[ ! -L "$O/.avet_test" ] || fail "failed link: the link is still there"
assert_file_exists     "$O/.avet_test/.failed"
assert_file_not_exists "$O/Finished/.failed"
[ "$(ls -A "$O/Finished")" = "movie.mkv" ] || fail "failed link: avet wrote into the link's target"

# a symlink at a state file's scratch name is replaced, not written through
I="$WORKDIR/105/in"; O="$WORKDIR/105/out"; mkdir -p "$I/p" "$O/.avet_test"
cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
printf '[avet]\nvideo = "copy"\n' > "$I/p/encode.toml"
printf 'only copy\n' > "$I/precious.txt"
ln -s /input/precious.txt "$O/.avet_test/source.path.tmp"
run_avet "$I" "$O" "$O/test.mkv" 120 || fail "state link: no output"
[ "$(cat "$I/precious.txt")" = "only copy" ] || fail "state link: the link's target was overwritten"

# a name left at the mux path, by a stopped publish or as a link, does not get written through
for how in hard symbolic; do
    I="$WORKDIR/704/$how/in"; O="$WORKDIR/704/$how/out"; mkdir -p "$I/p" "$O/.avet_test"
    cp "$FIXTURES_DIR/sdr_simple.mkv" "$I/p/test.mkv"
    printf 'encoder = "svt-av1"\n[encoder_params]\npreset = 12\ncrf = 50\n' > "$I/p/encode.toml"
    printf 'only copy' > "$O/library.mkv"
    if [ "$how" = hard ]; then ln "$O/library.mkv" "$O/.avet_test/muxed.mkv"
    else ln -s /output/library.mkv "$O/.avet_test/muxed.mkv"; fi
    run_avet "$I" "$O" "$O/test.mkv" 120 || fail "$how link at the mux path: no output"
    [ "$(cat "$O/library.mkv")" = "only copy" ] || fail "$how link at the mux path: its target was overwritten"
    [ ! -L "$O/test.mkv" ] || fail "$how link at the mux path: the output is a link"
done

test_done
