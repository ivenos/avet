# Contributing

## Build

Rust 1.95 or newer, plus the FFMS2 headers and `nasm`:

```bash
sudo apt-get install -y libffms2-dev nasm
cargo build
```

- A distribution's libffms2 lacks `packaging/ffms2-frame-hdr-metadata.patch`. Test Dolby Vision and HDR10+ in the image.
- Running avet outside the image also needs `ffmpeg`, `ffprobe`, `mkvmerge`, `ffmsindex`, `FFVship`, `vmaf`, `SvtAv1EncApp` and `SvtAv1EncApp-hdr` on `PATH`. The Docker image bundles all of them:

```bash
docker build -t avet:test .
```

## Tests

```bash
cargo test --locked           # unit tests
./test/run.sh                 # build the images, then run test/suites/
./test/run.sh --no-build      # reuse the existing images
./test/run.sh -j 4            # at most 4 suites at once, half the CPU cores by default
./test/run.sh audio           # only suites matching "audio"
./test/run.sh --verbose       # print the container logs of a failed suite
```

- The integration suites test the binary inside the image. Rebuild it after a change.
- `run.sh` builds the avet image and `test/tools.Dockerfile`, which adds `dovi-tool`, `hdr10plus-tool` and `mkvtoolnix` for the fixtures in `test/fixtures.sh`.
- Assertions use the image's own ffmpeg and mkvtoolnix. `test/suites/selftest.sh` checks that every assertion fails on a broken file.
- A suite takes its scratch directory from `test_workdir`, ends with `test_done` and sets no EXIT trap of its own.
- `selftest.sh` fails for an assertion of `test/lib.sh` it never shows failing.
- Output avet reads from a tool gets a check in `test/suites/tools.sh`.
- A case whose source makes avet repair its scene list sets `TEST_SCENE_REPAIR=1`.
- `TEST_SOFTWARE_GPU=1` passes `AVET_SOFTWARE_GPU=1`, with which avet accepts the image's software Vulkan device. `target_quality.sh` runs the search that way.
- `test/local/` is gitignored for trying the image on your own samples.

## Code style

- Errors are `anyhow::Result` with a `.context()` naming the failed step. A failed job must never stop the scan loop.
- Failures that clear on their own carry `job::Transient` and are retried. Everything else writes a `.failed` marker, which an edit of `encode.toml` lifts.
- Run external tools through `ext::output_with_timeout` and report a non-zero exit with `ext::tool_error`, which classifies a tool stopped from outside and a full disk as transient.
- mkvmerge starts from `ext::mkvmerge` and runs through `ext::mkvmerge_output`.
- Tools fed through a pipe (the encoders, the ffmpeg filter stage, scene detection, the HDR10+ scan and CAMBI) start with `ext::spawn` and drain their stderr with `ext::drain_text`.
- ffmpeg calls on the source name their stream: `-map 0:v:0`, `-map 0:a:<n>`.
- Write output and state files under a scratch name, then rename them into place.
- A comment only earns its place when it records something the code cannot. One or two lines.
- Rust 2024, no formatter in CI: match the file you edit.
- `rust-version` is 1.95, and CI checks that the code builds with it.

## External tools

- `ext::external_bin` looks for a tool next to the avet binary first, then on `PATH`.
- A new tool needs the call site, the runtime stage of the `Dockerfile`, the AppDir step in `.github/workflows/appimage.yml`, the "every bundled tool starts" step in both workflows, its license texts in both artifacts, and its entry in the tool list under Build and in the README's license section.

## Configuration keys

- A new `encode.toml` key needs its field and default in `src/config.rs`, a check in `Config::validate` if the value can be wrong, a test with an accepted and a rejected value, and a row in the README.
- A key that is not in the README does not exist.
- Keep `#[serde(deny_unknown_fields)]` on every config struct.

## Settled decisions

Reopen one of these with a case it gets wrong, not with a preference.

- Workers, `--lp` and every encoder parameter stay as SVT-AV1 and SVT-AV1-HDR define them. avet adds measurements, not encoder tuning.
- A `.failed` marker belongs to the `encode.toml` it was written under, so a refused option writes one like any other failure. Tool output is not searched for the reason.
- A transient failure is retried at doubling intervals of up to 60 scans. A file whose tools are killed three times in a row fails.
- The target quality search is the one bracket in `next_probe`. A change holds `the_measured_curves_are_solved_no_worse_than_before`.
- A finished chunk is scored and reported, not encoded again.
- avet writes FFVship's display model itself and takes SDR or HDR from the colors FFVship reads. `--resizeToDisplay` stays unused.
- `[target_quality]` needs a hardware GPU. `AVET_SOFTWARE_GPU` is for the suites.
- Pixel formats are classified from libavutil's descriptor, not by name.
- FFMS2 stays on its release tag plus the patch, not on a commit of its master.
- MKVToolNix in the AppImage comes from mkvtoolnix.download, not from the runner's Ubuntu.
- Renovate merges on its own only the actions listed in `.github/renovate.json`.

## Commits

Conventional Commits (https://www.conventionalcommits.org/en/v1.0.0/) with a short imperative subject.

## Releases

- The git tag is the version; `Cargo.toml` stays at `0.0.0`.
- A push to `main` publishes the `dev` image, a `v*` tag the release image, and a GitHub release gets the AppImages.
- The changelog lives in the GitHub release notes, in Keep a Changelog style (https://keepachangelog.com/en/1.1.0/). There is no CHANGELOG.md.
- An entry that goes back to an issue or a pull request ends with its number: `(#12)`.

## Dependencies

- `Cargo.lock` is committed and CI builds with `--locked`.
- The pinned sources (SVT-AV1, SVT-AV1-HDR, FFMS2, Vship, libvmaf, Rust) are set in both the `Dockerfile` and `.github/workflows/appimage.yml`; change them together. `RUST_VERSION` is in `.github/workflows/build-publish.yml` as well.
- Both apply `packaging/ffms2-frame-hdr-metadata.patch` to FFMS2.
- FFmpeg and MKVToolNix in `appimage.yml` follow the image's Alpine versions and are bumped by hand.
- CI fails while one of these versions differs between the image and the AppImage.
- GitHub Actions and base images stay on version tags, never commit SHAs or digests.
- Renovate opens the bumps. Other PRs leave dependencies alone.

## Pull requests

- One concern per PR, with tests for behavior changes.
- Only the PR author and the maintainer commit to it.
- `cargo test` and `./test/run.sh` must pass.
- Fill in the PR template, including the test plan and the CLA checkbox.
