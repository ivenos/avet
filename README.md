# avxs

[![Docker Image Size](https://img.shields.io/docker/image-size/ivenos/avxs)](https://hub.docker.com/r/ivenos/avxs)
[![Docker Pulls](https://img.shields.io/docker/pulls/ivenos/avxs)](https://hub.docker.com/r/ivenos/avxs)
[![License](https://img.shields.io/badge/license-BSL_1.1-orange)](https://github.com/ivenos/avxs/blob/main/LICENSE)
[![svt-av1](https://img.shields.io/badge/svt--av1-v4.2.0-purple)](https://gitlab.com/AOMediaCodec/SVT-AV1)
[![svt-av1-hdr](https://img.shields.io/badge/svt--av1--hdr-0033340-purple)](https://github.com/juliobbv-p/svt-av1-hdr) <!-- renovate: juliobbv-p/svt-av1-hdr@00333404f455471aaa6ee2c927cac3c93efb76e3 -->

**avxs** is an AV1 encoding service written in Rust, shipped as a Docker image and a self-contained Linux AppImage. Drop video files and an `encode.toml` profile into a folder; avxs splits each file into scenes, encodes the chunks in parallel with SVT-AV1, and merges them back into a finished MKV.

It runs unattended: encodes resume from the last finished chunk after a restart, audio and subtitles are carried over with per-track rules, and every external tool is bundled.

## Features

- **Scene-based parallel encoding** - cuts each file into scenes with [av-scenechange](https://github.com/rust-av/av-scenechange) and encodes the chunks in parallel.
- **Resumable** - finished chunks are recorded, so a crash or restart continues where it left off.
- **Target quality (CVVDP)** - probes each chunk at a few CRF values, measures [CVVDP](https://codeberg.org/Line-fr/Vship) (ColorVideoVDP, in JOD) against the source, and encodes at the CRF that hits the target. GPU-accelerated via Vulkan (NVIDIA/AMD/Intel); a GPU is required.
- **HDR passthrough** - detects HDR10, HLG, Dolby Vision and HDR10+ and passes the color metadata to the encoder; HDR10+ and Dolby Vision profiles 7 and 8 fall back to their HDR10 base layer.
- **Auto-crop** - removes black bars (ffmpeg `cropdetect`) before scaling.
- **Auto-scale** - downscales to a target height with Lanczos, aspect ratio kept within a pixel; smaller sources are left untouched.
- **Auto-keyint** - derives `--keyint` from the source frame rate for a ~5 s keyframe interval.
- **Audio control** - copy or re-encode per source codec, with a language whitelist, per-layout bitrates, and automatic lossless handling.
- **Subtitle control** - copy or strip per language whitelist; chapters are always kept.
- **Self-contained** - bundles [ffmpeg](https://ffmpeg.org/), [mkvmerge](https://mkvtoolnix.download/), [SVT-AV1](https://gitlab.com/AOMediaCodec/SVT-AV1), [ffms2](https://github.com/FFMS/ffms2) and [Vship](https://codeberg.org/Line-fr/Vship) (FFVship, for CVVDP).

## How it works

For every video next to an `encode.toml`, avxs runs this pipeline:

1. **Index** the source with ffms2 for frame-accurate seeking.
2. **Detect scenes** and cut the file into chunks.
3. **Encode chunks in parallel**, one SVT-AV1 process per worker; the number of workers is derived from CPU cores and free RAM.
4. **Probe for target quality** (optional) - find the per-chunk CRF that hits the CVVDP (JOD) target.
5. **Merge** the chunks, **process audio** (copy or re-encode per track), and **mux** video, audio, subtitles and chapters into the final MKV.
6. **Validate** the output, move the source into `input/processed/`, and clean up. An encode has to be readable *and* hold every frame the chunk list accounts for; one that ends early never reaches the output directory. An existing file in `processed/` is never overwritten - the archived copy gets a `.2`, `.3` suffix.

Intermediate state (index, scene list, finished chunks, solved CRFs) lives in a hidden `.avxs_<name>/` directory under the output folder, which is what makes encodes resumable. A changed profile (encoder args, crop, scale, bit depth, scene detection, target quality) discards the cached scenes, chunks and CRFs. The finished file is muxed inside that directory and only moved to its final name once it validates.

The file name without its extension is the identity of a job: output, temp directory and the copy under `input/processed/` all carry it. Two queued files sharing one would overwrite each other, so neither runs until one is renamed.

## Table of contents

- [Installation](#installation)
  - [Docker](#1-docker)
  - [AppImage](#2-appimage)
- [Directory layout](#directory-layout)
- [Environment variables](#environment-variables)
- [Configuration](#configuration)
  - [`encoder`](#encoder)
  - [`[encoder_params]`](#encoder_params)
  - [`[target_quality]`](#target_quality)
  - [`[avxs]`](#avxs-1)
  - [`[audio]`](#audio)
  - [`[audio.lossless]`](#audiolossless)
  - [`[audio.codec_rules]`](#audiocodec_rules)
  - [`[subtitles]`](#subtitles)
  - [`[scene_detection]`](#scene_detection)
  - [Full example](#full-example)

## Installation

### 1. Docker

```yaml
services:
  avxs:
    image: ivenos/avxs:latest
    user: "1000:1000"
    volumes:
      - ./input:/input
      - ./output:/output
    environment:
      - AVXS_POLL_INTERVAL=60
    restart: unless-stopped
```

Without `user:` the container runs as root, and everything it writes ends up owned by root on the host.

Target quality (CVVDP) requires a GPU; the other steps never need one. Intel or AMD: add `devices: ["/dev/dri:/dev/dri"]` and, with `user:` set, `group_add: ["render"]` - the amd64 image bundles the Mesa drivers. NVIDIA: install the [nvidia-container-toolkit](https://github.com/NVIDIA/nvidia-container-toolkit) and add a GPU reservation or run with `--gpus all`. The arm64 image ships no hardware Vulkan driver.

### 2. AppImage

Grab the AppImage for your architecture from the [latest release](https://github.com/ivenos/avxs/releases/latest). By default avxs creates `input/` and `output/` inside its working directory and watches them. Override with the [environment variables](#environment-variables) below.

## Directory layout

Inside the input directory, each subfolder is a profile: it holds one `encode.toml` and the video files it applies to.

```
input/
├── movies/
│   ├── encode.toml          # profile for everything in this folder
│   ├── The Movie (2021).mkv
│   └── Another Film.mkv
├── anime/
│   ├── encode.toml          # a different profile
│   └── Episode 01.mkv
└── processed/               # sources are moved here after a successful encode

output/
├── The Movie (2021).mkv     # finished encodes land here, flat
└── Another Film.mkv
```

Supported input extensions: `mkv`, `mp4`, `mov`, `avi`, `ts`, `m2ts`, `flv`, `webm`, `m4v`. A file whose output already exists is skipped, and so is a file whose name contains a line break or is not valid UTF-8. An empty file in the output directory does not count as an existing output: avxs never produces one, so it is a leftover and gets replaced.

Only the first video track is encoded and reaches the output; `video = "copy"` passes every video track through unchanged.

On `SIGTERM` or `SIGINT` avxs finishes the job it is on and then exits, rather than leaving its encoder processes running against files the next start would write to as well.

A file that is still being copied in is waited on for up to five minutes; its size and timestamp have to hold still for three seconds. If it is still growing when the wait runs out, avxs gives up on it for this round and picks it up again on the next scan.

The same goes for every failure that clears on its own: a profile that does not parse, a tool that hits its timeout, a full output volume, a GPU that is not up yet. Those are logged on every scan and nothing is marked. Only a failure a retry cannot fix writes a marker, and the file is then skipped until you delete it (the log gives the path). The marker records which file it was written for, so a later file with the same name is not caught by it.

## Environment variables

| Variable | Default | Docker image | Description |
|---|---|---|---|
| `AVXS_INPUT_DIR` | `./input` | `/input` | Input directory to watch |
| `AVXS_OUTPUT_DIR` | `./output` | `/output` | Output directory for finished files |
| `AVXS_POLL_INTERVAL` | `60` | `60` | Directory scan interval in seconds |
| `RUST_LOG` | `info` | not set | Log verbosity. Set to `debug` for verbose output |

The second column is what the image sets itself; `RUST_LOG` it leaves alone, and the `info` default comes from avxs either way.

## Configuration

Each profile folder contains an `encode.toml`. The only required key is `encoder`, unless the video stream is copied; everything else has a default. Unknown keys are rejected rather than ignored, so a misspelled `[target_qualtiy]` fails the profile instead of quietly turning the feature off.

### `encoder`

```toml
encoder = "svt-av1"
```

| Value | Description |
|---|---|
| `svt-av1` | [SVT-AV1](https://gitlab.com/AOMediaCodec/SVT-AV1) |
| `svt-av1-hdr` | [SVT-AV1-HDR](https://github.com/juliobbv-p/svt-av1-hdr) |

Required unless `avxs.video = "copy"`, in which case the video stream is passed through and no encoder is needed.

### `[encoder_params]`

Passed straight through to SVT-AV1 as `--key value`. Keys are SVT-AV1 long flags without the leading `--`. All keys are optional.

```toml
[encoder_params]
preset = 6
crf    = 28
```

Values may be strings, integers, floats, or booleans (booleans become `1`/`0`).

Two keys are read by avxs as well as passed on. `crf` is used as the first probe seed when `[target_quality]` is active, and `lp` (SVT-AV1's logical processors per encode) is what avxs divides the CPU by to decide how many chunks to encode at once. Without `lp` it assumes `6`.

### `[target_quality]`

Targets a CVVDP score per chunk instead of a fixed `crf`. CVVDP ([ColorVideoVDP](https://codeberg.org/Line-fr/Vship)) is a colour- and motion-aware perceptual metric in JOD (Just-Objectionable-Differences) from 0 to 10, where 10 means no perceptible difference from the source. `jod` is a hard minimum: avxs probes each chunk at several CRF values and picks the highest CRF whose JOD still holds it. Requires `avxs.video = "encode"`.

```toml
[target_quality]
jod = 9.5
```

| Key | Type | Default | Description |
|---|---|---|---|
| `jod` | Float | - | Minimum CVVDP JOD to hold per chunk, in `(0, 10)` (required) |
| `min_crf` | Integer | `1` | Lower bound of the CRF search |
| `max_crf` | Integer | `70` | Upper bound of the CRF search (max `70`) |
| `min_probes` | Integer | `2` | Number of probes before the `tolerance` early stop may fire |
| `max_probes` | Integer | `7` | Maximum probe encodes per chunk |
| `tolerance` | Float | `0.5` | Stop early when a probe lands at most this far above the floor (in JOD) |
| `probe_preset` | Integer | `13` | SVT-AV1 preset for probe encodes (`13` = fastest) |
| `max_encoded_percent` | Float | `90` | Chunk size ceiling as a percent of the source's bytes for that chunk |

The search interpolates between probes to estimate where JOD crosses the floor, on the encoder's 0.25 CRF grid. It stops once a probe lands within `tolerance` above the floor, the crossing is narrowed to one step, the budget is spent, or a bound is hit; `min_probes` gates only the `tolerance` stop. Probes use `probe_preset` while the final encode uses the preset from `[encoder_params]`, so the delivered JOD tends to land above the probed value.

`max_encoded_percent` is the harder of the two constraints: if holding the floor would make a chunk larger than this percent of the source's own bytes for those frames, a higher CRF is used and that chunk's JOD drops below the floor (logged as a warning). If no probe is under the cap, the remaining budget searches toward `max_crf` until one fits, then back down between that CRF and the highest one that did not. The source size comes from one ffprobe pass over the video packets; if it fails, the cap never binds.

If the floor cannot be reached anywhere in `[min_crf, max_crf]`, avxs uses the lowest CRF (best quality) that still fits the size cap, and logs a warning. It does not fall back to `min_crf` regardless of size.

CVVDP is measured by [FFVship](https://codeberg.org/Line-fr/Vship), which compares source and probe over the same frames with the source cropped to match; a scaled-down encode is resized back up for the comparison. The display model follows `avxs.hdr`: `standard_hdr_hlg` or `standard_hdr_pq` by the signalled transfer, otherwise `standard_4k` from 1440p up and `standard_fhd` below, measured after the crop.

FFVship runs on Vulkan and picks the first hardware device it finds. Without one the job fails with a clear error - CVVDP on the CPU is too slow to be practical. See [Docker](#1-docker) for granting access.

`crf` in `[encoder_params]` is ignored while target quality is active (it is used only as the first probe seed). Solved CRFs are cached, so a resume does not re-probe.

### `[avxs]`

avxs pipeline controls. Every boolean here defaults to `false`; `video` defaults to `"encode"`.

```toml
[avxs]
hdr       = true
crop      = true
keyint    = true
scale     = 1080
bit_depth = 10
keep_temp = false
```

| Key | Type | Default | Description |
|---|---|---|---|
| `video` | `"encode"` \| `"copy"` | `"encode"` | `copy` passes the source video through untouched and only runs the audio and subtitle steps. The video-only options below and `[encoder_params]` are ignored, and `encoder` is not needed. |
| `hdr` | Boolean | `false` | Detect HDR type and pass color metadata (`--color-primaries`, `--transfer-characteristics`, `--matrix-coefficients`, `--chroma-sample-position`, `--color-range`, `--content-light`, `--mastering-display`) to the encoder automatically. Works for HDR10, HLG, HDR10+ and Dolby Vision profiles 7 and 8, which keep their HDR10 base layer. Dolby Vision profile 5 is refused: it has no HDR10 base layer, so an encode of it would have badly wrong colours. Independent of the encoder binary chosen. |
| `crop` | Boolean | `false` | Detect black bars via `ffmpeg cropdetect`: 5 samples of 10 s at 10/25/40/55/70 % of the runtime, threshold 24/255 scaled to the source's bit depth, edges rounded to even. The samples are combined into the rectangle containing all of them, and the crop is ignored unless it removes more than 1 % of the pixels. One that would cut away more than 60 % counts as a misdetection, since cropdetect boxes what is not black and an all-dark scene boxes only the lit part. Applied in the Y4M pipe **before** the encoder, and cached in the temp directory unless it failed or was rejected. |
| `keyint` | Boolean | `false` | Calculate `--keyint` from source FPS for a ~5 s keyframe distance (`round(fps x 5)`). Silently skipped if `keyint` is already set in `[encoder_params]`. SVT-AV1's own default (`--keyint -2`) also aims at roughly 5 s, so this mainly makes the value explicit and independent of the encoder. |
| `scale` | Integer | - | Maximum output height in pixels, at least `64`. The source is scaled down proportionally with Lanczos if taller than this. If the source (after crop) is already at or below this height, no scaling is applied. Example: `1080` encodes 4K content as 1080p while leaving 720p untouched. Both edges are rounded down to an even number, so the aspect ratio is held to within a pixel rather than exactly. |
| `bit_depth` | `8` \| `10` | - | Force the encoder input bit depth. Omitted passes the source depth through (8-bit stays 8, 10-bit stays 10); sources deeper than 10-bit are clamped to 10-bit, since SVT-AV1 accepts only 8/10-bit input. Set to `10` to convert 8-bit sources to 10-bit before encoding. |
| `keep_temp` | Boolean | `false` | Keep temporary chunks and index files after encoding. |

### `[audio]`

Controls how audio tracks are carried over. This is also the default profile: any track not matched by a more specific rule uses it.

```toml
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = { stereo = "192k", "5.1" = "320k", "7.1" = "512k", default = "192k" }
```

| Key | Type | Default | Description |
|---|---|---|---|
| `mode` | `"copy"` \| `"encode"` | `"copy"` | Copy or re-encode |
| `codec` | String | - | ffmpeg codec name, e.g. `"libopus"`. Required when `mode = "encode"` |
| `bitrate` | String \| table | - | Target bitrate, single value or per-layout table (see below). Required when encoding to a lossy codec |
| `options` | table | `{}` | Extra encoder options, passed per output track as `-<key>:a:<index> <value>`, e.g. `{ compression_level = 12 }`. Global ffmpeg options do not work here |
| `language_whitelist` | String array | `[]` | Keep only tracks with these language tags (ISO 639-2). Empty = keep all |

The channel count is always taken from the source. FLAC passes the source layout through unchanged; Opus normalizes the layout name to one its encoder accepts but never changes the channel count.

**Bitrate per channel layout.** `bitrate` is either a single string applied to every track, or a table keyed by layout. avxs detects each track's channel count and picks the matching entry; `default` covers anything not listed.

| Channels | Key | Channels | Key |
|---|---|---|---|
| 1 | `mono` | 5 | `5.0` |
| 2 | `stereo` | 6 | `5.1` |
| 3 | `3.0` | 7 | `6.1` |
| 4 | `quad` | 8 | `7.1` |

Lossless codecs (`flac`, `alac`, `wavpack`, `tta`, `pcm_*`) ignore bitrate, so it may be omitted for them.

**Language whitelist.** When set, only audio tracks whose language tag is in the list are kept. Tracks **without** a language tag are always kept, including those tagged `und`: Matroska omits the field where MP4 and MPEG-TS write `und`, and both mean the same thing.

```toml
[audio]
language_whitelist = ["eng"]  # English only
mode = "copy"
```

Matching is case-insensitive and covers both ISO 639-2 spellings, so `deu` also matches a track tagged `ger` - which avxs' own output carries, since Matroska stores the bibliographic form. Region and script subtags are ignored on both sides. The two code sets do not mix: `por` does not match `pt-BR`, and `de-DE` does not match `ger`.

Common ISO 639-2 codes: `eng` (English), `deu`/`ger` (German), `fra`/`fre` (French), `jpn` (Japanese). Listing `und` has no effect, since untagged tracks are kept regardless.

The same whitelist rules apply to `[subtitles]`.

**Track titles.** Re-encoded tracks keep their source name with the new codec appended: `English Dolby Digital Plus 7.1 (Opus)`. The marker does not stack when an avxs output is re-encoded. Untitled tracks get the codec name alone, copied tracks keep theirs. Dispositions (default, forced, commentary, hearing- and visual-impaired, original) come from the source. One log line per kept track:

```
[The Movie (2021)] audio track 0: eng eac3 5.1 (lossy) -> Opus 320k
[The Movie (2021)] audio track 1: eng truehd 7.1 (lossless) -> FLAC
```

### `[audio.lossless]`

Override for tracks whose **source** is lossless. Unset fields inherit from `[audio]`, except `options`: a non-empty table replaces the inherited one rather than merging into it.

Lossless comes from ffmpeg's own codec table (`ffmpeg -codecs`). `dts` is special-cased and counts as lossless only in its Master Audio profile, since ffprobe reports both under the same codec name.

```toml
[audio]
mode    = "encode"
codec   = "libopus"
bitrate = { stereo = "192k", "5.1" = "320k", "7.1" = "512k", default = "192k" }

[audio.lossless]
codec   = "flac"
options = { compression_level = 12 }
```

Result: lossless sources become FLAC at maximum compression, everything else becomes Opus.

### `[audio.codec_rules]`

Per source codec override, keyed by the codec name as reported by ffprobe (lowercase). A matching rule has the highest precedence and, like `[audio.lossless]`, inherits any unset field from `[audio]`.

```toml
[audio]
language_whitelist = ["eng"]
mode = "copy"   # default: copy all codecs not matched by a rule

[audio.codec_rules]
eac3 = { mode = "encode", codec = "libopus", bitrate = "192k" }
opus = { mode = "copy" }   # don't re-encode existing Opus
ac3  = { mode = "encode", codec = "libopus", bitrate = "128k" }
```

**Resolution order for each kept track:**

1. Filter by language whitelist (empty list = no filter).
2. Settings resolve as `codec_rules[codec]` then `[audio.lossless]` (lossless sources only) then `[audio]`. Whichever matches first wins; unset fields inherit from `[audio]`.
3. If no tracks remain after filtering, audio is omitted entirely (warning logged).

Common codec names from ffprobe: `eac3`, `ac3`, `aac`, `truehd`, `dts`, `flac`, `mp3`, `opus`, `vorbis`.

### `[subtitles]`

Controls how subtitle tracks are carried over. By default all subtitles are copied. Chapters are always preserved, regardless of this setting.

```toml
[subtitles]
mode               = "copy"
language_whitelist = ["eng", "jpn"]  # English and Japanese only
```

| Key | Type | Default | Description |
|---|---|---|---|
| `mode` | `"copy"` \| `"strip"` | `"copy"` | `copy` keeps subtitle tracks; `strip` removes them all |
| `language_whitelist` | String array | `[]` | Keep only tracks with these language tags (ISO 639-2). Empty = keep all |

When the whitelist is set, only subtitle tracks whose language tag is in the list are kept. Tracks **without** a language tag are always kept, `und` included. To remove every subtitle:

```toml
[subtitles]
mode = "strip"
```

### `[scene_detection]`

```toml
[scene_detection]
min_scene_len   = 24
extra_split_sec = 10
# extra_split      = 0       # overrides extra_split_sec when > 0
# speed            = "standard"
# downscale_height = 720
```

| Key | Type | Default | Description |
|---|---|---|---|
| `min_scene_len` | Integer | `24` | Minimum chunk length in frames (min `1`). Cuts closer than this are suppressed. |
| `extra_split_sec` | Integer | `10` | Maximum chunk length in seconds. Longer chunks are split into roughly equal parts. Set to `0` to disable. Ignored when `extra_split` > 0. |
| `extra_split` | Integer | `0` | Maximum chunk length in frames (min `24` when set). Overrides `extra_split_sec` when > 0. Set to `0` to use `extra_split_sec` instead. |
| `speed` | `"standard"` \| `"fast"` | `"standard"` | Detection algorithm. `standard` uses SATD-based motion estimation. `fast` uses raw pixel differences, which lowers detection time and accuracy. |
| `downscale_height` | Integer | - | Downscale to this height (e.g. `720`, min `64`) for scene detection only. Does not affect encoding output. Speeds up detection on high-resolution sources at some accuracy cost. |

### Full example

```toml
encoder = "svt-av1"

[encoder_params]
preset      = 6
crf         = 28
input-depth = 10
lookahead   = 120

[target_quality]
jod = 9.5          # optional: replaces the fixed crf above with a CVVDP (JOD) target

[avxs]
hdr       = true
crop      = true
keyint    = true
scale     = 1080
bit_depth = 10
keep_temp = false

[audio]
language_whitelist = ["eng"]
mode    = "encode"
codec   = "libopus"
bitrate = { stereo = "192k", "5.1" = "320k", "7.1" = "512k", default = "192k" }

[audio.lossless]
codec   = "flac"
options = { compression_level = 12 }

[audio.codec_rules]
opus = { mode = "copy" }   # don't re-encode existing Opus

[subtitles]
language_whitelist = ["eng", "jpn"]

[scene_detection]
min_scene_len   = 24
extra_split_sec = 10
```
