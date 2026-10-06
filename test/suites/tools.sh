#!/bin/sh
# What avet reads off its tools: a release that words one of these differently fails here.
. "$(dirname "$0")/../lib.sh"

WORKDIR=$(test_workdir)
W="$WORKDIR"
FF="ffmpeg -nostdin -y -hide_banner -loglevel error"

$FF -f lavfi -i "testsrc2=size=320x240:rate=24" -frames:v 12 -pix_fmt yuv420p -c:v ffv1 "$W/src.mkv"
$FF -i "$W/src.mkv" -f yuv4mpegpipe "$W/src.y4m"
tool ffmsindex -f "$W/src.mkv" "$W/src.ffindex" >/dev/null 2>&1
[ -s "$W/src.y4m" ] && [ -s "$W/src.ffindex" ] || { fail "tools: could not build the source"; test_done; }

# the encoders say "Encoding" once they took their arguments, which names the one to blame
for encoder in SvtAv1EncApp SvtAv1EncApp-hdr; do
    tool "$encoder" --preset 12 --crf 45 -i "$W/src.y4m" -b "$W/$encoder.ivf" 2>&1 | grep -q '^Encoding' || \
        fail "$encoder: no 'Encoding' line on a good run"
    tool "$encoder" --prest 12 --crf 45 -i "$W/src.y4m" -b "$W/refused.ivf" 2>&1 | grep -q '^Encoding' && \
        fail "$encoder: an 'Encoding' line although it refused its arguments"
done
[ -s "$W/SvtAv1EncApp.ivf" ] || { fail "tools: could not build the encode"; test_done; }

# FFVship: the device list and its own verdict on a device
gpus=$(tool FFVship --list-gpu 2>&1)
printf '%s\n' "$gpus" | grep -qE '^GPU [0-9]+: .+' || fail "FFVship --list-gpu: no 'GPU <n>: <name>' line in: $gpus"
printf '%s\n' "$gpus" | grep -q 'llvmpipe' || fail "FFVship --list-gpu: the image's software device is not named llvmpipe in: $gpus"
tool FFVship --gpu-info --gpu-id 0 2>&1 | grep -qx 'Passes Kernel Check: [01]' || \
    fail "FFVship --gpu-info: no 'Passes Kernel Check: <0|1>' line"

# FFVship: one cumulative CVVDP score per frame, for the display config target_quality.rs writes
MODEL='{"avet":{"name":"avet","resolution":[320,240],"colorspace":"sRGB","viewing_distance_meters":0.7472,"diagonal_size_inches":30.000,"max_luminance":200,"contrast":1000,"E_ambient":250,"k_refl":0.005}}'
cvvdp() { # MODEL_KEY GPU_ID -> stderr
    tool FFVship -s "$W/src.mkv" -e "$W/SvtAv1EncApp.ivf" --source-index "$W/src.ffindex" -m CVVDP \
        --start 0 --encoded-offset -0 --displayModel "$1" --displayConfig "$MODEL" --gpu-id "$2" \
        --json "$W/cvvdp.json" 2>&1 >/dev/null
}
rm -f "$W/cvvdp.json"
cvvdp avet 0 >/dev/null
rows=$(tr -d ' \n' < "$W/cvvdp.json" 2>/dev/null | grep -oE '\[[0-9]+(\.[0-9]+)?\]' | wc -l | tr -d ' ')
[ "$rows" = 12 ] || fail "FFVship CVVDP json: expected 12 rows of one score, got $rows in: $(cat "$W/cvvdp.json" 2>/dev/null)"
last=$(tr -d ' \n' < "$W/cvvdp.json" 2>/dev/null | grep -oE '[0-9]+(\.[0-9]+)?\]\]$' | tr -d ']')
awk -v jod="$last" 'BEGIN { exit !(jod > 0 && jod <= 10) }' || fail "FFVship CVVDP json: the last score '$last' is no JOD"

# FFVship: how it read each of the two files, which avet compares and picks the display by
$FF -f lavfi -i "testsrc2=size=320x240:rate=24" -frames:v 2 -vf "format=yuv420p10le,setparams=colorspace=bt2020nc:color_trc=smpte2084:color_primaries=bt2020" \
    -c:v ffv1 "$W/pq.mkv"
$FF -i "$W/pq.mkv" -strict -1 -f yuv4mpegpipe "$W/pq.y4m"
tool ffmsindex -f "$W/pq.mkv" "$W/pq.ffindex" >/dev/null 2>&1
tool SvtAv1EncApp --preset 12 --crf 45 --color-primaries 9 --transfer-characteristics 16 --matrix-coefficients 9 \
    -i "$W/pq.y4m" -b "$W/pq.ivf" >/dev/null 2>&1
read=$(tool FFVship -s "$W/pq.mkv" -e "$W/pq.ivf" --source-index "$W/pq.ffindex" -m CVVDP --start 0 --encoded-offset -0 \
    --displayModel avet --displayConfig "$MODEL" --gpu-id 0 --json "$W/pq.json" --verbose 2>/dev/null)
blocks=$(printf '%s\n' "$read" | awk '/Source-Colorspace/ { b = "source" } /Encoded-Colorspace/ { b = "encoded" }
    b != "" && /^(Color Family|Range|YUV Matrix|Transfer Function|Primaries): / { printf "%s %s\n", b, $0 }')
for side in source encoded; do
    for line in "Color Family: YUV" "Range: Limited" "YUV Matrix: BT2020_NCL" "Transfer Function: PQ" "Primaries: BT2020"; do
        printf '%s\n' "$blocks" | grep -qxF "$side $line" || fail "FFVship --verbose: no '$line' for the $side in: $(echo $blocks)"
    done
done

# Vship's errors: the type on the line after VshipException
type_after_exception() { awk 'prev == "VshipException" { sub(/:.*/, ""); print; exit } { prev = $0 }'; }
got=$(cvvdp nope 0 | type_after_exception)
[ "$got" = BadDisplayModel ] || fail "FFVship: an unknown display model reports '$got', expected BadDisplayModel"
got=$(cvvdp avet 99 | type_after_exception)
[ "$got" = BadDeviceArgument ] || fail "FFVship: a GPU that is not there reports '$got', expected BadDeviceArgument"
got=$(docker exec -e VK_DRIVER_FILES=/nonexistent.json -e VK_ICD_FILENAMES=/nonexistent.json "$_TOOLS" FFVship --list-gpu 2>&1 \
    | type_after_exception)
[ "$got" = InternalError ] || fail "FFVship: no Vulkan driver reports '$got', expected InternalError"
for type in OutOfVRAM OutOfRAM InternalError DeviceCountError NoDeviceDetected BadDeviceArgument BadDeviceCode; do
    tool grep -qaF "$type: " /usr/local/lib/libvship.so || fail "libvship: no error type $type"
done

# vmaf: the three CAMBI scores per frame, the encode's own under a name that takes the options
$FF -i "$W/SvtAv1EncApp.ivf" -pix_fmt yuv420p -f yuv4mpegpipe "$W/enc.y4m"
for feature in cambi=full_ref=true cambi=full_ref=true:eotf=pq; do
    rm -f "$W/cambi.json"
    tool vmaf --reference "$W/src.y4m" --distorted "$W/enc.y4m" --no_prediction --feature "$feature" \
        --json --output "$W/cambi.json" >/dev/null 2>&1
    frames=$(grep -o '"frameNum"' "$W/cambi.json" 2>/dev/null | wc -l | tr -d ' ')
    [ "$frames" = 12 ] || fail "vmaf $feature: expected 12 frames, got $frames"
    keys=$(grep -oE '"cambi[a-z_]*"' "$W/cambi.json" 2>/dev/null | tr -d '"' | sort -u | tr '\n' ' ')
    case " $keys" in *" cambi_full_reference "*) ;; *) fail "vmaf $feature: no cambi_full_reference among: $keys" ;; esac
    own=$(printf '%s\n' $keys | grep -vcE '^cambi_(source|full_reference)')
    [ "$own" = 1 ] || fail "vmaf $feature: expected one score of the encode itself among: $keys"
done

# mkvmerge: English on request, and the track properties avet joins the tools by
tool mkvmerge --ui-language en_US -o "$W/none.mkv" "$W/missing.mkv" 2>&1 | grep -q '^Error: ' || \
    fail "mkvmerge --ui-language en_US: no 'Error: ' line for a missing file"
identified=$(tool mkvmerge --ui-language en_US --identify --identification-format json "$FIXTURES_DIR/pattern.mkv")
for property in '"number":' '"language":' '"language_ietf":'; do
    printf '%s\n' "$identified" | grep -qF "$property" || fail "mkvmerge --identify: no $property property"
done

test_done
