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

- [SVT-AV1](https://gitlab.com/AOMediaCodec/SVT-AV1) or [SVT-AV1-HDR](https://github.com/juliobbv-p/svt-av1-hdr) per profile
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

Create `input/` and `output/` before the first start, otherwise Docker creates them owned by root. Without `user:` the container runs as root and its files on the host belong to root.

`[target_quality]` needs a GPU, nothing else does:

- Intel or AMD: add `devices: ["/dev/dri:/dev/dri"]` and `group_add` with the numeric ID of the host's `render` group (`getent group render`).
- NVIDIA: use the AppImage. The image is built on musl, where NVIDIA's Vulkan driver does not load.

The arm64 image has no hardware Vulkan driver.

### AppImage

Download the AppImage for your architecture from the [latest release](https://github.com/ivenos/avet/releases/latest) and run it. It watches `input/` and `output/` in the working directory. It needs glibc 2.39 or newer and FUSE; without FUSE, run it with `--appimage-extract-and-run`.

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
- Output files keep the source's name with `.mkv`, so queued files that would end up at the same output path, such as `film.mkv` and `film.mp4`, wait until one is renamed.
- Work in progress lives in `.avet_<name>/` next to the output file. Delete that folder to encode a file from scratch.
- A failure that can clear on its own, such as a typo in the profile, a timeout or a full disk, is retried on the next scan. Any other failure writes a `.failed` file into that folder, and the video is skipped until you delete it.
- On `SIGTERM` or `SIGINT` avet exits after the current file, on a second signal at once. A file stopped mid-encode, e.g. by Ctrl-C in a terminal or by `docker stop` after its timeout, resumes from its last finished chunk.

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `INPUT_DIR` | `./input`, `/input` in the image | Input directory |
| `OUTPUT_DIR` | `./output`, `/output` in the image | Output directory |
| `POLL_INTERVAL` | `60` | Seconds between scans |
| `RUST_LOG` | `info` | Log level, e.g. `debug` |

## Configuration

Only `encoder` is required. Unknown keys are rejected.

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

Passed to the encoder as `--key value`; booleans become `1`/`0`. avet reads two of them itself: it encodes one chunk per `lp` CPU cores at once (`6` when unset) as far as free RAM allows, and `crf` is the first probe when `[target_quality]` is set. `[target_quality]` cannot be combined with `tbr` or an `rc` other than `0`.

### `[target_quality]`

Replaces the fixed `crf`: avet probes each chunk at a few CRF values and encodes at the highest one that still holds `jod`. CVVDP scores in JOD from 0 to 10, where 10 means no visible difference from the source.

```toml
[target_quality]
jod = 9.5
```

| Key | Default | Description |
|---|---|---|
| `jod` | - | Minimum CVVDP score per chunk, above `0` and below `10` (required) |
| `min_crf` | `1` | Lowest CRF to try, at least `1` and below `max_crf` |
| `max_crf` | `70` | Highest CRF to try, at most `70` |
| `min_probes` | `2` | Probes before `tolerance` may stop the search, at least `2` |
| `max_probes` | `7` | Maximum probes per chunk, at least `min_probes` |
| `tolerance` | `0.05` | Stop once a probe is at most this far above `jod`, at least `0` |
| `probe_preset` | `13` | Encoder preset for probes, `0` to `13` |
| `max_encoded_percent` | `90` | Maximum chunk size as a percent of the source's bytes for that chunk, above `0` |
| `max_cambi` | - | Maximum CAMBI of the encode, at least `0` |
| `max_cambi_diff` | - | Maximum CAMBI the encode may add on top of its input, at least `0` |

- Needs `avet.video = "encode"` and cannot be combined with `avet.scale`.
- `max_encoded_percent` wins over `jod`: a chunk that would grow past it gets a higher CRF, and a warning is logged. The sizes are those of the probes; a finished chunk that still comes out larger is logged too. Where the source's packet sizes cannot be read, the limit is off for that file.
- CAMBI is `0` without banding; Netflix places slightly annoying banding at around `5`. `max_cambi` counts banding the source already has, `max_cambi_diff` does not. Both are compared with the 95th percentile of a chunk's frames and are measured without film grain.
- If no probe holds every limit, the chunk uses the lowest probed CRF under `max_encoded_percent`, or the smallest probe if none is under it.

### `[avet]`

| Key | Default | Description |
|---|---|---|
| `video` | `"encode"` | `"copy"` passes the video through and only processes audio and subtitles |
| `dv` | `false` | Carry Dolby Vision from an HEVC source into the output as AV1 profile 10, converting profile 7 to 8.1 first. Not with `bit_depth = 8`. Without it, Dolby Vision 7 and 8 keep only their base layer, and Dolby Vision without one, such as profile 5, is refused |
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
| `bitrate` | - | A single value such as `"192k"`, or a table keyed by `mono`, `stereo`, `3.0`, `quad`, `5.0`, `5.1`, `6.1`, `7.1` and `default`. Required for lossy codecs; a layout missing from a table without `default` gets the encoder's own bitrate |
| `options` | `{}` | Extra per-track encoder options, e.g. `{ compression_level = 12 }` |
| `language_whitelist` | `[]` | Keep only these ISO 639-2 languages. Empty keeps all |

- The whitelist matches both spellings (`deu` and `ger`). Tracks without a language or tagged `und` are always kept.
- Re-encoded tracks get the codec added to their title, e.g. `English 5.1 (Opus)`.
- Copied tracks Matroska has no codec ID for, such as Blu-ray LPCM, are stored as PCM. A track ffmpeg can neither copy into Matroska nor decode, such as AC-4, is left out with a warning.
- Opus gets every channel of a layout it has no mapping for by using the next larger one, e.g. 2.1 as 5.1. A layout none of them holds, such as 7.1(wide) or 7.1.4, is mixed to the one with its channel count, at most 7.1.

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

Chapters are always kept. MP4 text subtitles become SRT, and subtitle tracks Matroska cannot hold, such as TTML, are skipped.

### `[scene_detection]`

| Key | Default | Description |
|---|---|---|
| `min_scene_len` | `24` | Minimum distance between scene cuts in frames, at least `1` |
| `extra_split_sec` | `10` | Maximum chunk length in seconds, never below 24 frames, `0` disables |
| `extra_split` | `0` | Maximum chunk length in frames, at least `24`, `0` disables, overrides `extra_split_sec` |
| `speed` | `"standard"` | `"fast"` trades accuracy for speed |
| `downscale_height` | - | Detect scenes on a copy scaled to this height, at least `64` |

## License

Copyright © Iven Schlösser. avet is free software, licensed under the [GNU General Public License v3.0 only](https://github.com/ivenos/avet/blob/main/LICENSE). You may use, modify and redistribute it. Anyone distributing a modified version must release it under the same license and make its source code available.

The Docker image and the AppImage bundle third-party software, each under its own license:

- [SVT-AV1](https://gitlab.com/AOMediaCodec/SVT-AV1) and [SVT-AV1-HDR](https://github.com/juliobbv-p/svt-av1-hdr): BSD 3-Clause Clear License with the Alliance for Open Media Patent License 1.0
- [FFmpeg](https://ffmpeg.org): GPL-3.0-or-later in the image, GPL-2.0-or-later in the AppImage
- [FFMS2](https://github.com/FFMS/ffms2): MIT as source, GPL as a binary built against FFmpeg
- [Vship](https://codeberg.org/Line-fr/Vship): MIT NON-AI License
- [libvmaf](https://github.com/Netflix/vmaf): BSD-2-Clause-Patent
- [MKVToolNix](https://mkvtoolnix.download): GPL-2.0-only

The license texts are in `/usr/share/licenses/` in the image and in `usr/share/licenses/` and `usr/share/doc/` inside the AppImage. The image's other Alpine packages name their licenses in the apk database.

avet is not affiliated with or endorsed by any of them. "Dolby Vision" is a trademark of Dolby Laboratories Licensing Corporation, "HDR10+" is a trademark of HDR10+ Technologies, LLC.
