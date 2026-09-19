<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/ivenos/avet/main/.github/assets/avet-logo-on-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/ivenos/avet/main/.github/assets/avet-logo-on-light.svg">
  <img alt="avet" src="https://raw.githubusercontent.com/ivenos/avet/main/.github/assets/avet-logo-auto.svg" width="50%">
</picture>

<a href="https://hub.docker.com/r/ivenos/avet"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/docker/size/ivenos/avet.svg?variant=secondary&amp;mode=dark"><img alt="Docker Image Size" src="https://shieldcn.dev/docker/size/ivenos/avet.svg?variant=secondary&amp;mode=light"></picture></a>
<a href="https://hub.docker.com/r/ivenos/avet"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/docker/pulls/ivenos/avet.svg?variant=secondary&amp;mode=dark"><img alt="Docker Pulls" src="https://shieldcn.dev/docker/pulls/ivenos/avet.svg?variant=secondary&amp;mode=light"></picture></a>
<a href="https://github.com/ivenos/avet/releases"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/github/downloads/ivenos/avet.svg?variant=secondary&amp;mode=dark&amp;label=AppImage%20downloads"><img alt="AppImage Downloads" src="https://shieldcn.dev/github/downloads/ivenos/avet.svg?variant=secondary&amp;mode=light&amp;label=AppImage%20downloads"></picture></a>
<a href="https://github.com/ivenos/avet/blob/main/LICENSE"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/license-GPL_3.0.svg?variant=secondary&amp;mode=dark"><img alt="License" src="https://shieldcn.dev/badge/license-GPL_3.0.svg?variant=secondary&amp;mode=light"></picture></a>

avet is an AV1 encoding service that watches a folder, splits each video at its scene cuts and encodes the chunks in parallel with SVT-AV1. It writes a finished MKV with audio, subtitles and chapters carried over, and ships as a Docker image and as a Linux AppImage with every tool bundled.

</div>

---

> [!WARNING]
> This README describes avet 2.0, which is not released yet. The current release is avxs 1.0.0 with the image `ivenos/avxs:latest`, documented in its [README](https://github.com/ivenos/avet/blob/v1.0.0/README.md).

## Features

- Scene-based parallel encoding with [SVT-AV1](https://gitlab.com/AOMediaCodec/SVT-AV1) or [SVT-AV1-HDR](https://github.com/juliobbv-p/svt-av1-hdr)
- Resumes from the last finished chunk after a restart
- Target quality: a CRF per chunk from a [CVVDP](https://codeberg.org/Line-fr/Vship) score, with optional [CAMBI](https://github.com/Netflix/vmaf/blob/master/resource/doc/cambi.md) banding limits (GPU required)
- HDR10, HLG, HDR10+ and Dolby Vision profiles 5, 7 and 8
- Automatic crop, downscale and keyframe interval
- Per-track audio and subtitle rules with language filters

## Installation

### Docker

```yaml
services:
  avet:
    image: ivenos/avet:latest
    user: "1000:1000"
    volumes:
      - ./input:/input
      - ./output:/output
    restart: unless-stopped
```

Without `user:` the container runs as root and its files on the host belong to root.

`[target_quality]` needs a GPU, nothing else does:

- Intel or AMD: add `devices: ["/dev/dri:/dev/dri"]` and `group_add: ["render"]`.
- NVIDIA: install the [nvidia-container-toolkit](https://github.com/NVIDIA/nvidia-container-toolkit) and add a GPU reservation, or run with `--gpus all`.

The arm64 image has no hardware Vulkan driver.

### AppImage

Download the AppImage for your architecture from the [latest release](https://github.com/ivenos/avet/releases/latest) and run it. It watches `input/` and `output/` in the working directory.

## Usage

Every subfolder of the input directory is a profile: one `encode.toml` and the videos it applies to.

```
input/
├── movies/
│   ├── encode.toml
│   └── The Movie (2021).mkv
├── anime/
│   ├── encode.toml
│   └── The Show/
│       └── Season 1/
│           └── Episode 01.mkv
└── processed/               # sources move here once encoded

output/
├── The Movie (2021).mkv
└── The Show/
    └── Season 1/
        └── Episode 01.mkv
```

- Supported extensions: `mkv`, `mp4`, `mov`, `avi`, `ts`, `m2ts`, `flv`, `webm`, `m4v`. Only the first video track is encoded.
- Video is encoded as 4:2:0 at 8 or 10 bits. An odd frame size loses its last column or row. Aspect ratio, rotation, the color description, HDR10, HLG and HDR10+ metadata and the offsets between streams are kept.
- A file whose output already exists is skipped. A file that is still being copied in is picked up once it stops growing.
- Folders inside a profile are kept in `output/` and `processed/`. Once a folder's last video is encoded, the empty source folder is removed.
- Output files are named after the source, so two queued files with the same name in the same folder wait until one is renamed.
- Work in progress lives in `.avet_<name>/` next to the output file. Delete that folder to encode a file from scratch.
- A failure that can clear on its own, such as a typo in the profile, a timeout or a full disk, is retried on the next scan. Any other failure writes a `.failed` file into that folder, and the video is skipped until you delete it.
- On `SIGTERM` or `SIGINT` avet finishes the current file, then exits.

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `INPUT_DIR` | `./input`, `/input` in the image | Input directory |
| `OUTPUT_DIR` | `./output`, `/output` in the image | Output directory |
| `POLL_INTERVAL` | `60` | Seconds between scans |
| `RUST_LOG` | `info` | Log level, e.g. `debug` |

## Configuration

Only `encoder` is required. Unknown keys are rejected, so a typo fails the profile instead of turning a feature off.

```toml
encoder = "svt-av1"

[encoder_params]
preset = 6
crf    = 28

[avet]
crop      = true
keyint    = true
scale     = 1080
bit_depth = 10

[audio]
language_whitelist = ["eng"]
mode    = "encode"
codec   = "libopus"
bitrate = { stereo = "192k", "5.1" = "320k", "7.1" = "512k", default = "192k" }

[audio.lossless]
codec   = "flac"
options = { compression_level = 12 }

[subtitles]
language_whitelist = ["eng", "jpn"]
```

### `encoder`

`svt-av1` or `svt-av1-hdr`. Not needed with `avet.video = "copy"`.

### `[encoder_params]`

Passed to the encoder as `--key value`; booleans become `1`/`0`. avet reads two of them itself: `lp` (default `6`) together with free RAM sets how many chunks encode at once, and `crf` is the first probe when `[target_quality]` is set.

### `[target_quality]`

Replaces the fixed `crf`: avet probes each chunk at a few CRF values and encodes at the highest one that still holds `jod`. CVVDP scores in JOD from 0 to 10, where 10 means no visible difference from the source.

```toml
[target_quality]
jod = 9.5
```

| Key | Default | Description |
|---|---|---|
| `jod` | - | Minimum CVVDP score per chunk, in `(0, 10)` (required) |
| `min_crf` | `1` | Lowest CRF to try |
| `max_crf` | `70` | Highest CRF to try (max `70`) |
| `min_probes` | `2` | Probes before `tolerance` may stop the search |
| `max_probes` | `7` | Maximum probes per chunk |
| `tolerance` | `0.05` | Stop once a probe is at most this far above `jod` |
| `probe_preset` | `13` | Encoder preset for probes |
| `max_encoded_percent` | `90` | Maximum chunk size as a percent of the source's bytes for that chunk |
| `max_cambi` | - | Maximum CAMBI of the encode, `>= 0` |
| `max_cambi_diff` | - | Maximum CAMBI the encode may add on top of its input, `>= 0` |

- `max_encoded_percent` wins over `jod`: a chunk that would grow past it gets a higher CRF, and a warning is logged.
- CAMBI is `0` without banding; Netflix places slightly annoying banding at around `5`. `max_cambi` counts banding the source already has, `max_cambi_diff` does not. Both apply to the worst 5% of a chunk's frames and are measured without film grain.
- If no probe holds every limit, the chunk uses the lowest CRF under `max_encoded_percent`.

### `[avet]`

| Key | Default | Description |
|---|---|---|
| `video` | `"encode"` | `"copy"` passes the video through and only processes audio and subtitles |
| `dv` | `false` | Carry Dolby Vision into the output as AV1 profile 10, converting profile 7 to 8.1 first. Without it, Dolby Vision 7 and 8 keep only their HDR10 base layer and Dolby Vision 5 is refused |
| `crop` | `false` | Remove black bars |
| `keyint` | `false` | Keyframe every ~5 s from the frame rate, unless `keyint` is in `[encoder_params]` |
| `scale` | - | Maximum output height, at least `64`. Taller sources are scaled down with Lanczos |
| `bit_depth` | - | Encoder input bit depth, `8` or `10`. Unset keeps the source depth, capped at 10 |
| `keep_temp` | `false` | Keep `.avet_<name>/` after a finished encode |

### `[audio]`

| Key | Default | Description |
|---|---|---|
| `mode` | `"copy"` | `"copy"` or `"encode"` |
| `codec` | - | ffmpeg encoder, e.g. `"libopus"`. Required for `"encode"` |
| `bitrate` | - | A single value, or a table keyed by `mono`, `stereo`, `3.0`, `quad`, `5.0`, `5.1`, `6.1`, `7.1` and `default`. Required for lossy codecs |
| `options` | `{}` | Extra per-track encoder options, e.g. `{ compression_level = 12 }` |
| `language_whitelist` | `[]` | Keep only these ISO 639-2 languages. Empty keeps all |

- The whitelist matches both spellings (`deu` and `ger`). Tracks without a language or tagged `und` are always kept.
- Re-encoded tracks get the codec added to their title, e.g. `English 5.1 (Opus)`.
- Copied tracks Matroska has no codec ID for, such as Blu-ray LPCM, are stored as PCM.
- Opus gets every channel of a layout it has no mapping for by using the next larger one, e.g. 2.1 as 5.1.

`[audio.lossless]` applies to tracks with a lossless source (`dts` only as DTS-HD MA). `[audio.codec_rules]` applies by source codec as ffprobe names it. Both take the keys above except `language_whitelist`. Unset keys come from `[audio]`, except a non-empty `options`, which replaces it. A matching codec rule wins over `[audio.lossless]`.

```toml
[audio.codec_rules]
opus = { mode = "copy" }
eac3 = { mode = "encode", codec = "libopus", bitrate = "192k" }
```

### `[subtitles]`

| Key | Default | Description |
|---|---|---|
| `mode` | `"copy"` | `"copy"` or `"strip"` |
| `language_whitelist` | `[]` | Same rules as for audio |

Chapters are always kept. Subtitle tracks Matroska cannot hold, such as TTML, are skipped.

### `[scene_detection]`

| Key | Default | Description |
|---|---|---|
| `min_scene_len` | `24` | Minimum chunk length in frames |
| `extra_split_sec` | `10` | Maximum chunk length in seconds, `0` disables |
| `extra_split` | `0` | Maximum chunk length in frames (at least `24`), overrides `extra_split_sec` |
| `speed` | `"standard"` | `"fast"` trades accuracy for speed |
| `downscale_height` | - | Detect scenes on a copy scaled to this height (at least `64`) |

## License

Copyright © Iven Schlösser. avet is free software, licensed under the [GNU General Public License v3.0 only](https://github.com/ivenos/avet/blob/main/LICENSE). You may use, modify and redistribute it. Anyone distributing a modified version must release it under the same license and make its source code available.

The Docker image and the AppImage bundle third-party software, each under its own license:

- [SVT-AV1](https://gitlab.com/AOMediaCodec/SVT-AV1) and [SVT-AV1-HDR](https://github.com/juliobbv-p/svt-av1-hdr): BSD 3-Clause Clear License with the Alliance for Open Media Patent License 1.0
- [FFmpeg](https://ffmpeg.org): GPL-2.0-or-later and LGPL-2.1-or-later
- [FFMS2](https://github.com/FFMS/ffms2): MIT as source, GPL as a binary built against FFmpeg
- [Vship](https://codeberg.org/Line-fr/Vship): MIT NON-AI License
- [libvmaf](https://github.com/Netflix/vmaf): BSD-2-Clause-Patent
- [MKVToolNix](https://mkvtoolnix.download): GPL-2.0-only

avet is not affiliated with or endorsed by any of them. "Dolby Vision" is a trademark of Dolby Laboratories Licensing Corporation, "HDR10+" is a trademark of HDR10+ Technologies, LLC.
