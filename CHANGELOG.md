# Changelog

All notable changes to this project are documented in this file, in the format of [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Dependencies

- signal-hook 0.4.4 -> 0.4.5 (#45)

## [2.0.0] - 2026-10-07

### Breaking changes

- avxs is now avet: new image names, `[avet]` instead of `[avxs]`, and environment variables without the `AVXS_` prefix.
- Unfinished avxs encodes start over.
- `avet.hdr` is gone, HDR metadata is always kept. A profile that still sets it is rejected.
- `[target_quality]` no longer works with `avet.scale` or with rate control and color keys in `[encoder_params]`.
- The color keys in `[encoder_params]` take their numeric code.
- An audio bitrate needs its unit, such as `192k`, and `language_whitelist` only takes ISO 639-2 codes.
- `tolerance` in `[target_quality]` defaults to `0.05` instead of `0.5`.

### Added

- `avet.dv` carries Dolby Vision from HEVC sources into the output.
- HDR10+ metadata is kept.
- Dolby Vision and HDR10+ survive crop and scale.
- Banding limits for target quality with `max_cambi` and `max_cambi_diff`.
- Target quality scores every finished chunk again and logs a miss.
- Interlaced video is deinterlaced.
- Folders inside a profile are kept in the output.
- Variable frame rate sources keep their timestamps.
- 4:2:2, 4:4:4, RGB and gray sources and odd frame sizes are encoded instead of failing.
- Language tags keep their region and script, such as `pt-BR`.
- Blu-ray LPCM and AAC from DVB-T2 can be copied.
- An audio track that can be neither copied nor decoded is left out with a warning.
- A codec or option ffmpeg refuses fails the job before the video encode.
- `avet --version`, and the version in the start log line.
- The done line shows sizes, time taken and speed, and chunk lines the time left.
- A second avet on the same output folder waits for the first one.
- A second stop signal stops avet at once.
- A `.failed` marker is lifted when `encode.toml` changes.
- A file that kills avet or its tools three times in a row is marked as failed.
- A source that could not be archived is archived on a later scan.
- Warnings for an unknown codec in `audio.codec_rules` and a subtitle whitelist that matches nothing.
- The image and the AppImage carry the license texts of everything they bundle.

### Changed

- The license is GPL-3.0-only instead of the Business Source License 1.1.
- HDR10 and HLG metadata and the color description are always kept.
- The target quality search handles the quality floor and the size cap in one pass.
- Target quality scores against a display that fits the video instead of a fixed one.
- Opus keeps every channel of an unusual layout instead of mixing it down.
- TrueHD, MLP and S302M as output need no bitrate.
- Auto-crop looks at more of the video and only accepts centered bars.
- `extra_split_sec` produces no chunk below 24 frames.
- Empty and hidden files are skipped, and a folder that cannot be read no longer ends the scan.
- A file is picked up once its size and timestamp stayed the same twice in a row.
- Failures that can clear on their own are retried at growing intervals.
- A missing tool, a lost share and a GPU error are retried instead of marking the file as failed.
- Unfinished encodes survive an update that adds a setting.
- A source replaced under the same name starts over.
- Subtitle tracks Matroska cannot hold, such as TTML, are skipped with a warning.
- Audio and subtitle tracks keep the source's order.
- 8-bit sources that need a conversion stay 8-bit.
- Warnings and failure markers are logged once per run instead of on every scan.
- Tool errors keep the last 40 lines of output, and log colors appear only on a terminal.
- Timeouts of steps over the whole file grow with its size.
- The AppImage ships the same FFmpeg and MKVToolNix as the image.

### Fixed

- Audio from MP4 and MOV played late, by 21 ms or by much more after a cut.
- Audio in an MPEG-TS that starts after the video came out early.
- A video track that starts after the audio lost its delay.
- A Matroska start time shifted subtitles and chapters.
- `video = "copy"` put audio and subtitles out of sync for FLV, MP4 and MPEG-TS, and attached MP4 cover art twice.
- Opus swapped channels of 5.0 and 6.1 tracks and dropped the LFE of 2.1.
- A track switching from stereo to 5.1 was mixed down to stereo.
- FLAC from a 32-bit source was cut to 24 bits.
- The color description was lost unless `hdr` was set.
- Mastering display values were rounded.
- The aspect ratio, the rotation of phone videos and a container crop were lost.
- Downscaling could shift the colors by a fraction of a pixel, and the scaled height could miss `avet.scale`.
- Auto-crop and scene detection failed on rotated sources.
- XviD and DivX in AVI had broken frames at every chunk start.
- Scaled sources near 120 or 240 fps lost or doubled frames.
- Scene cuts landed early on recordings that start between two keyframes.
- DVB subtitles were cleared at the wrong time.
- The subtitle whitelist kept the wrong track, or none, next to a subtitle stream mkvmerge cannot read.
- The first audio and subtitle track became default where the source marked none.
- Encoded video lost its default flag and language, and the source's global tags were dropped.
- MPEG-TS tracks tagged with several languages were dropped by a whitelist or lost their language.
- An MPEG-TS audio stream that is announced but never carried failed the job.
- A video of a single frame failed.
- A file name of 250 bytes or more failed the job.
- The encoder crashed on a host with a single CPU.
- A full disk during a chunk encode counted as a finished chunk.
- A source replaced during a job came out with the old video and the new audio.
- A frame index from another FFmpeg build, such as after an image update, failed the job for good.
- A signal during the scan started another full encode before avet stopped.
- A `.failed` marker hid one half of a name collision.
- A chunk list with a gap or an overlap was encoded as if it were complete.
- A truncated Matroska or IVF file could make avet use far too much memory.
- A panic in a job stopped avet.
- Archiving failed where `processed/` or the profile is a mount of its own.
- A profile folder with a non-UTF-8 name marked every video in it as failed.
- A source that reports no average frame rate is encoded at the container's rate.
- Full-range video without other color tags was not marked full range.
- The README said NVIDIA GPUs work in the image. They need the AppImage.

### Dependencies

- SVT-AV1-HDR cfb4e17 -> 18327c0 (#17, #37, #38)
- Vship v5.1.0 -> v5.1.2 (#31, #44)
- rust 1.97.1 -> 1.99.0 (#30, #33, #41)
- toml 1.1.4 -> 1.1.6 (#32, #34)

## [1.0.0] - 2026-08-12

### Added

- Target quality: `[target_quality]` picks the CRF of each chunk so that it holds a CVVDP score. It needs a GPU.
- `max_encoded_percent` limits the size of a chunk against its source.
- `SIGTERM` and `SIGINT` finish the current file, then avxs exits.
- A finished file is checked for missing frames before it reaches the output folder.

### Changed

- Auto-crop runs before scaling.
- A changed profile discards the unfinished work of a file.
- A full disk, a timeout or a source still being copied is retried on the next scan.
- The number of parallel chunks follows the memory limit of the container.
- Two input files with the same name are both skipped instead of overwriting each other.

### Removed

- The image no longer declares `/input` and `/output` as volumes.

### Fixed

- Every job outside the Docker image failed after its last chunk.
- Odd frame sizes, such as 853x480, broke the encode.
- Auto-crop found no bars in 10-bit sources and made one up in clean 8-bit ones.
- Dolby Vision profile 5 came out with wrong colors. It is refused now.
- Five color transfer names never reached the encoder.
- A language whitelist removed tracks without a language, which could leave a file without audio.
- An archived source could overwrite an earlier one of the same name.
- An empty state file failed every later run.
- Switching between `svt-av1` and `svt-av1-hdr` resumed onto the chunks of the other encoder.
- The Docker image could ship an old binary.
- A failure after the output was in place was reported as a failed job.

### Dependencies

- SVT-AV1 v4.1.0 -> v4.2.0 (#19)
- SVT-AV1-HDR 1bd66be -> cfb4e17 (#9)
- rust 1.96.0 -> 1.97.1 (#15, #16, #22)
- tokio 1.52.3 -> 1.53.1 (#21, #23, #27)
- serde 1.0.228 -> 1.0.229 (#25)
- serde_json 1.0.150 -> 1.0.151 (#26)
- anyhow 1.0.102 -> 1.0.104 (#14, #24)
- toml 1.1.2 -> 1.1.4 (#18, #28)
- av-scenechange 0.24.0 -> 0.24.1 (#13)

## [0.4.2] - 2026-06-22

### Changed

- `encoder` is no longer required with `video = "copy"`.

### Fixed

- A short failure while reading the subtitles no longer fails the whole job.
- A failed job always writes its `.failed` marker instead of being tried again on every scan.

### Dependencies

- av-scenechange 0.23.0 -> 0.24.0 (#12)

## [0.4.1] - 2026-06-20

### Added

- `video = "copy"` passes the video through and only processes audio and subtitles.

## [0.4.0] - 2026-06-20

### Added

- An audio bitrate per channel layout.
- `[audio.lossless]` for tracks with a lossless source.
- Encoder options per audio codec, such as the FLAC compression level.
- Re-encoded audio tracks get the codec added to their name.
- The audio plan is logged before the encode, one line per kept track.

### Changed

- A lossless target codec needs no bitrate.

### Fixed

- Opus failed on 5.1(side) and similar layouts.

### Dependencies

- alpine 3.23 -> 3.24 (#10)
- actions/checkout v6 -> v7 (#11)

## [0.3.1] - 2026-06-05

### Changed

- SVT-AV1-HDR is built from its main branch instead of the `v4.1.0` tag.

### Dependencies

- SVT-AV1-HDR v4.1.0 -> 1bd66be (#8)
- rust 1.95.0 -> 1.96.0 (#7)
- serde_json 1.0.149 -> 1.0.150 (#3)

## [0.3.0] - 2026-05-20

### Added

- `bit_depth` sets the bit depth the encoder reads, 8 or 10.

## [0.2.1] - 2026-05-18

### Fixed

- An encode could stop at 99% on sources with a damaged end.
- A file marked as failed was indexed again before it was skipped.

## [0.2.0] - 2026-05-17

### Added

- An AppImage for x86_64 and aarch64 with every tool bundled.
- Track flags such as forced, default and hearing impaired are kept.

### Changed

- More HDR and broadcast color formats are tagged correctly.
- `AVXS_POLL_INTERVAL=0` scans once a second instead of without a pause.
- `lp` can be written as a number or as text.
- More problems are logged as warnings instead of passing silently.

### Fixed

- A chunk cut short by a full disk or a kill is encoded again instead of ending up in the output.
- HLG content no longer logs a warning about incomplete HDR metadata.
- Files with a non-UTF-8 name are skipped with a warning.

## [0.1.0] - 2026-05-16

### Added

- avxs watches a folder, splits each video at its scene cuts and encodes the chunks in parallel with SVT-AV1.
- An encode resumes from its last finished chunk.
- HDR10 and HLG metadata is passed to the encoder.
- Automatic crop, downscale and keyframe interval.
- Audio is copied or re-encoded per codec, subtitles are copied or stripped, both with a language whitelist.

[Unreleased]: https://github.com/ivenos/avet/compare/v2.0.0...HEAD
[2.0.0]: https://github.com/ivenos/avet/compare/v1.0.0...v2.0.0
[1.0.0]: https://github.com/ivenos/avet/compare/v0.4.2...v1.0.0
[0.4.2]: https://github.com/ivenos/avet/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/ivenos/avet/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/ivenos/avet/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/ivenos/avet/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/ivenos/avet/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/ivenos/avet/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/ivenos/avet/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ivenos/avet/releases/tag/v0.1.0
