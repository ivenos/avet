<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset=".github/assets/avet-logo-on-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset=".github/assets/avet-logo-on-light.svg">
  <img alt="avet" src=".github/assets/avet-logo-auto.svg" width="50%">
</picture>

<a href="https://hub.docker.com/r/ivenos/avet"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/docker/size/ivenos/avet.svg?variant=secondary&amp;mode=dark"><img alt="Docker Image Size" src="https://shieldcn.dev/docker/size/ivenos/avet.svg?variant=secondary&amp;mode=light"></picture></a>
<a href="https://hub.docker.com/r/ivenos/avet"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/docker/pulls/ivenos/avet.svg?variant=secondary&amp;mode=dark"><img alt="Docker Pulls" src="https://shieldcn.dev/docker/pulls/ivenos/avet.svg?variant=secondary&amp;mode=light"></picture></a>
<a href="https://github.com/ivenos/avet/releases"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/github/downloads/ivenos/avet.svg?variant=secondary&amp;mode=dark&amp;label=AppImage%20downloads"><img alt="AppImage Downloads" src="https://shieldcn.dev/github/downloads/ivenos/avet.svg?variant=secondary&amp;mode=light&amp;label=AppImage%20downloads"></picture></a>
<a href="LICENSE"><picture><source media="(prefers-color-scheme: dark)" srcset="https://shieldcn.dev/badge/license-GPL_3.0.svg?variant=secondary&amp;mode=dark"><img alt="License" src="https://shieldcn.dev/badge/license-GPL_3.0.svg?variant=secondary&amp;mode=light"></picture></a>

avet is an AV1 encoding service that watches a folder, splits each video at its scene cuts and encodes the chunks in parallel with SVT-AV1. It writes a finished MKV with audio, subtitles and chapters carried over, and ships as a Docker image and as a Linux AppImage with every tool bundled.

</div>

---

## Features

- [SVT-AV1](https://github.com/AOMediaCodec/SVT-AV1) or [SVT-AV1-HDR](https://github.com/juliobbv-p/svt-av1-hdr) per profile
- Resumes from the last finished chunk after a restart
- Target quality: a CRF per chunk from a [CVVDP](https://codeberg.org/Line-fr/Vship) score, with optional [CAMBI](https://github.com/Netflix/vmaf/blob/master/resource/doc/cambi.md) banding limits (GPU required)
- HDR10, HLG, HDR10+ and Dolby Vision profiles 7 and 8
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

- Create `input/` and `output/` before the first start, owned by `user`. Docker creates missing ones owned by root.
- On a host with SELinux, such as Fedora or openSUSE, add `security_opt: [label=disable]`.
- `[target_quality]` needs a GPU. For Intel or AMD, add `devices: ["/dev/dri:/dev/dri"]` and `group_add` with the ID of the host's `render` group (`getent group render`).
- For an NVIDIA GPU, install the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) and add `gpus: all`.
- `docker compose stop` ends avet after 10 s, or after `stop_grace_period`. The file resumes from its last finished chunk.

### AppImage

Download it from the [latest release](https://github.com/ivenos/avet/releases/latest), make it executable and start it in a folder with `input/` and `output/`. It needs glibc 2.39 or newer and FUSE, or the option `--appimage-extract-and-run`.

## Usage

1. Create a folder in `input/`, such as `input/movies/`, with an `encode.toml` as described under [Configuration](#configuration).
2. Copy videos into it.
3. avet writes each video to `output/` as MKV and moves the source to `input/processed/`.

Every folder in `input/` with an `encode.toml` is a profile. Folders inside a profile are kept in `output/` and `processed/`.

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
└── processed/

output/
├── The Movie (2021).mkv
└── The Show/
    └── Season 1/
        └── Episode 01.mkv
```

- avet reads `mkv`, `mp4`, `mov`, `avi`, `ts`, `m2ts`, `flv`, `webm` and `m4v`.
- A stopped encode resumes from its last finished chunk.
- A video that fails for good gets `.avet_<name>/.failed` next to its output file and waits until you change `encode.toml` or delete that file.

<details>
<summary>Details</summary>

- Only the first video track is encoded, as 4:2:0 at 8 or 10 bits. An odd frame size loses its last column or row.
- Interlaced video is deinterlaced with bwdif at its frame rate, when the stream is flagged interlaced at its start or at 20, 50 or 80 % of its length and ffmpeg's idet finds its frames combed.
- Aspect ratio, rotation and mirroring, the color description, HDR10, HLG and HDR10+ metadata and the offsets between streams are kept.
- A source whose color matrix, color range or transfer changes inside the video fails with a `.failed` marker.
- From a transport stream with several programs, only the sound and subtitles of the first video's program are kept.
- A text subtitle that starts before the first picture and runs into it starts at the first picture. One that ends before it is dropped.
- A folder without `encode.toml` is skipped, with a warning if it holds videos. A video directly in `input/` is skipped with a warning.
- An `encode.toml` in a folder inside a profile is ignored with a warning.
- A file whose output already exists is skipped. A file still being copied in is picked up once it stops growing.
- Output files keep the source's name with `.mkv`. Two sources with the same name, such as `film.mkv` and `film.mp4`, wait until one is renamed.
- A name already taken in `processed/` gets a number, such as `film.2.mkv`. A source that could not be archived is archived on a later scan.
- Once a folder's last video is encoded, the empty source folder is removed.
- Work in progress lives in `.avet_<name>/` next to the output file. Delete it to encode a file from scratch.
- An `encode.toml` avet cannot read or rejects, a missing GPU, a tool that does not start, a timeout or a full disk is retried: on the next scan, then at doubling intervals of up to 60 scans, and at once after a change of `encode.toml`.
- Any other failure, such as an option the encoder refuses, writes `.failed`. So does a file during which avet or one of its tools is killed three times in a row. A stop by signal does not count.
- On `SIGTERM`, `SIGINT` or `SIGHUP` avet exits after the current file, on a second signal at once. Started with `nohup`, it ignores `SIGHUP`.
- A second avet on the same output folder waits until the first one stops.
- `avet --version` prints the version, and the `avet started` log line names it.

</details>

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `INPUT_DIR` | `./input`, `/input` in the image | Input directory |
| `OUTPUT_DIR` | `./output`, `/output` in the image | Output directory, not inside `INPUT_DIR` |
| `POLL_INTERVAL` | `60` | Seconds between scans |
| `RUST_LOG` | `info` | Log level, e.g. `avet=debug` |
| `LOG_NAME_LENGTH` | - | Cuts the file name in brackets in front of each log line to this many characters, e.g. `30`. The line that starts a job also shows the full path |

## Configuration

Only `encoder` is required. Unknown keys are rejected. With the `#:schema` line at the top, editors with [Even Better TOML](https://taplo.tamasfe.dev) or [Tombi](https://tombi-toml.github.io/tombi/) suggest and check the keys.

```toml
#:schema https://github.com/ivenos/avet/releases/latest/download/encode.schema.json
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
mode               = "encode"
codec              = "libopus"
bitrate            = { stereo = "192k", "5.1" = "320k", "7.1" = "512k", default = "192k" }
language_whitelist = ["eng"]

[audio.lossless]
codec   = "flac"
options = { compression_level = 12 }

[subtitles]
language_whitelist = ["eng", "jpn"]
```

### `encoder`

| Key | Default | Values | Description |
|---|---|---|---|
| `encoder` | - | `"svt-av1"`, `"svt-av1-hdr"` | The video encoder. Not needed with `avet.video = "copy"` |

### `[encoder_params]`

Every key goes to the encoder as `--key value`, booleans as `1` or `0`.

| Key | Default | Values | Description |
|---|---|---|---|
| `crf` | - | a number | Also the first probe of `[target_quality]` |
| color keys | - | numeric code, e.g. `9` for BT.2020 | Replace the color description avet reads from the source |

- The color keys are `color-primaries`, `transfer-characteristics`, `matrix-coefficients`, `color-range` and `chroma-sample-position`.
- avet runs one chunk per `lp` CPU cores at once, `6` when unset, as far as free RAM allows.
- `[target_quality]` does not work with `tbr`, `cqp`, an `rc` other than `0`, or the color keys except `chroma-sample-position`.

### `[avet]`

| Key | Default | Values | Description |
|---|---|---|---|
| `video` | `"encode"` | `"encode"`, `"copy"` | `"copy"` keeps the video and processes only audio and subtitles |
| `crop` | `false` | `true`, `false` | Remove black bars |
| `scale` | - | `64` and up | Maximum output height |
| `bit_depth` | - | `8`, `10` | Encoder input bit depth. Unset keeps the source depth, at most 10 |
| `keyint` | `false` | `true`, `false` | Keyframe about every 5 s |
| `dv` | `false` | `true`, `false` | Carry Dolby Vision as AV1 profile 10 |
| `keep_temp` | `false` | `true`, `false` | Keep `.avet_<name>/` after a finished encode |

- `scale` counts the height before rotation. Taller sources are scaled down with Lanczos.
- `keyint` in `[encoder_params]` wins over `keyint`.
- `dv` needs an HEVC source and does not work with `bit_depth = 8`. It converts profile 7 to 8.1.
- Without `dv`, and for other profiles, only the Dolby Vision base layer is kept.
- Dolby Vision whose base layer is no picture on its own, such as profile 5, is refused.
- Dolby Vision in a separate video stream, as on a UHD Blu-ray, is not carried: only the base layer is encoded, and with `dv` the log warns.
- With `video = "copy"`, a file fails if mkvmerge cannot carry its video over, such as MJPEG in MP4 or FFV1 in MOV.

### `[audio]`

| Key | Default | Values | Description |
|---|---|---|---|
| `mode` | `"copy"` | `"copy"`, `"encode"` | Copy or re-encode each track |
| `codec` | - | ffmpeg encoder, e.g. `"libopus"` | Required for `"encode"` |
| `bitrate` | - | `"192k"`, or a table per layout | Required for lossy codecs |
| `options` | `{}` | table, e.g. `{ compression_level = 12 }` | Extra per-track encoder options |
| `language_whitelist` | `[]` | ISO 639-2 codes, e.g. `["eng"]` | Keep only these languages. Empty keeps all |

- A `bitrate` table takes `mono`, `stereo`, `3.0`, `quad`, `5.0`, `5.1`, `6.1`, `7.1` and `default`. A layout missing without `default` gets the encoder's own bitrate.
- The whitelist matches both spellings (`deu` and `ger`). Tracks without a language or tagged `und` are always kept. The log names each track it leaves out.
- Language tags keep their region and script, such as `pt-BR` or `zh-Hant`.
- Re-encoded tracks get the codec added to their title, e.g. `English 5.1 (Opus)`.
- Copied Blu-ray LPCM is stored as PCM, copied LATM AAC from DVB-T2 as plain AAC.
- A track ffmpeg can neither copy into Matroska nor decode, such as AC-4, is left out with a warning.
- A track that changes its channel layout midway is encoded in its widest layout.
- A TS track that starts within the first 5 minutes and the first 1 GB is kept. A stream avet finds no type for by then is skipped with a warning.
- Opus takes a layout it has no mapping for, such as 2.1, as the next larger one, here 5.1, with that bitrate. Wider layouts such as 7.1(wide) or 7.1.4 are mixed down, at most to 7.1.

### `[audio.lossless]` and `[audio.codec_rules]`

| Key | Default | Values | Description |
|---|---|---|---|
| `mode`, `codec`, `bitrate`, `options` | from `[audio]` | as in `[audio]` | Override `[audio]` for these tracks |

- `[audio.lossless]` applies to tracks with a lossless source, `dts` only as DTS-HD MA.
- `[audio.codec_rules]` applies by source codec as ffprobe names it, and wins over `[audio.lossless]`.
- A non-empty `options` replaces the one from `[audio]`.
- FLAC stores floating-point PCM as 24-bit integers.

```toml
[audio.codec_rules]
opus = { mode = "copy" }
eac3 = { mode = "encode", codec = "libopus", bitrate = "192k" }
```

### `[subtitles]`

| Key | Default | Values | Description |
|---|---|---|---|
| `mode` | `"copy"` | `"copy"`, `"strip"` | Keep or drop subtitle tracks |
| `language_whitelist` | `[]` | as in `[audio]` | Keep only these languages. Empty keeps all |

- Chapters are always kept.
- MP4 text subtitles become SRT. Tracks Matroska cannot hold, such as TTML, are skipped.

### `[scene_detection]`

| Key | Default | Values | Description |
|---|---|---|---|
| `min_scene_len` | `24` | `1` and up | Minimum distance between scene cuts in frames |
| `extra_split_sec` | `10` | `0` and up | Maximum chunk length in seconds, `0` disables |
| `extra_split` | `0` | `0`, or `24` and up | Maximum chunk length in frames, `0` disables |
| `speed` | `"standard"` | `"standard"`, `"fast"` | `"fast"` trades accuracy for speed |
| `downscale_height` | - | `64` and up | Detect scenes on a copy scaled to this height |

- `extra_split_sec` counts as at least 24 frames, and is off when it comes to less than half a frame.
- `extra_split` overrides `extra_split_sec`.

### `[target_quality]`

Replaces the fixed `crf`: avet probes each chunk at a few CRF values and encodes at the highest one that still holds `jod`. CVVDP scores in JOD from 0 to 10, where 10 means no visible difference from the source.

```toml
[target_quality]
jod = 9.5
```

| Key | Default | Values | Description |
|---|---|---|---|
| `jod` | - | above `0`, below `10` | Minimum CVVDP score per chunk (required) |
| `min_crf` | `1` | `1` and up, below `max_crf` | Lowest CRF to try |
| `max_crf` | `70` | above `min_crf`, up to `70` | Highest CRF to try |
| `min_probes` | `2` | `2` and up | Probes before `tolerance` may stop the search |
| `max_probes` | `7` | `min_probes` and up | Maximum probes per chunk |
| `tolerance` | `0.05` | `0` and up | Stop once a probe is at most this far above `jod` |
| `probe_preset` | `13` | `0` to `13` | Encoder preset for probes |
| `max_encoded_percent` | `90` | above `0` | Maximum chunk size, in percent of the source's bytes for that chunk |
| `max_cambi` | - | `0` and up | Maximum CAMBI of the encode |
| `max_cambi_diff` | - | `0` and up | Maximum CAMBI the encode may add to its input |

- Needs a GPU and `avet.video = "encode"`. Scored against the picture after `avet.scale`.
- `max_encoded_percent` wins over `jod`: a chunk that would grow past it gets a higher CRF and a warning.
- The size limit uses the probes' sizes. A finished chunk that still comes out larger is logged too. Without readable packet sizes in the source, the limit is off.
- CAMBI is `0` without banding. Netflix places slightly annoying banding at around `5`.
- `max_cambi` counts banding the source already has, `max_cambi_diff` does not. Both use the 95th percentile of a chunk's frames, measured without film grain.
- A finished chunk is scored again, unless it is deinterlaced and neither scaled nor mirrored. The `done` line gives the lowest score, and a chunk under `jod` although its probe held it gets a warning.
- If no probe holds every limit, the chunk uses the probe with the highest JOD under `max_encoded_percent`, or the smallest probe if none is under it.

## License

Copyright © Iven Schlösser. avet is free software, licensed under the [GNU General Public License v3.0 only](LICENSE). You may use, modify and redistribute it. Anyone distributing a modified version must release it under the same license and make its source code available.

The Docker image and the AppImage bundle third-party software, each under its own license:

- [SVT-AV1](https://github.com/AOMediaCodec/SVT-AV1) and [SVT-AV1-HDR](https://github.com/juliobbv-p/svt-av1-hdr): BSD 3-Clause Clear License with the Alliance for Open Media Patent License 1.0
- [FFmpeg](https://ffmpeg.org): GPL-2.0-or-later
- [FFMS2](https://github.com/FFMS/ffms2): MIT as source, GPL as a binary built against FFmpeg
- [Vship](https://codeberg.org/Line-fr/Vship): MIT NON-AI License
- [libvmaf](https://github.com/Netflix/vmaf): BSD-2-Clause-Patent
- [MKVToolNix](https://mkvtoolnix.download): GPL-2.0-only

The license texts are in `usr/share/licenses/` and `usr/share/doc/`, under `/` in the image and inside the AppImage.

avet is not affiliated with or endorsed by any of them, Dolby Laboratories or HDR10+ Technologies. "Dolby Vision" is a trademark of Dolby Laboratories Licensing Corporation, "HDR10+" is a trademark of HDR10+ Technologies, LLC.
