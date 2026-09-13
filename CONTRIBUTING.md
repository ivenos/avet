# Contributing

## Setup

Rust 1.95 or newer, plus the FFMS2 headers and `nasm`:

```bash
sudo apt-get install -y libffms2-dev nasm
cargo build
```

Running avet outside the image also needs `ffmpeg`, `ffprobe`, `mkvmerge`,
`ffmsindex`, `FFVship`, `vmaf` and `SvtAv1EncApp` on `PATH`. The Docker image
bundles all of them:

```bash
docker build -t avet:test .
```

## Tests

```bash
cargo test --locked           # unit tests
./test/run.sh                 # build the image, then run test/suites/
./test/run.sh --no-build      # reuse the existing image
./test/run.sh audio           # only suites matching "audio"
```

The integration suites test the binary inside the image, so rebuild it after a
change. `test/local/` is gitignored for trying the image on your own samples.

## Code style

- Errors are `anyhow::Result` with a `.context()` naming the failed step. A
  failed job must never stop the scan loop.
- Failures that clear on their own carry `job::Transient` and are retried.
  Everything else writes a `.failed` marker.
- Run external tools through `ext::output_with_timeout`. The encoders and the
  scene-detection ffmpeg are fed through a pipe instead; drain their stderr on
  a thread.
- ffmpeg calls on the source name their stream: `-map 0:v:0`, `-map 0:a:<n>`.
- Write output and state files under a scratch name, then rename them into
  place.
- Comments only for what the code cannot say, in one or two lines.
- Rust 2024, no formatter in CI: match the file you edit.

## External tools

`ext::external_bin` looks for a tool next to the avet binary first, then on
`PATH`. A new tool needs the call site, the runtime stage of the `Dockerfile`
and the AppDir step in `.github/workflows/appimage.yml`.

## Configuration keys

A new `encode.toml` key needs its field and default in `src/config.rs`, a check
in `Config::validate` if the value can be wrong, a test with an accepted and a
rejected value, and a row in the README. Keep `#[serde(deny_unknown_fields)]` on
every config struct.

## Commits

Conventional Commits (https://www.conventionalcommits.org/en/v1.0.0/), with a
short imperative subject.

## Releases

The git tag is the version; `Cargo.toml` stays at `0.0.0`. A push to `main`
publishes the `dev` image, a `v*` tag the release image, and a GitHub release
gets the AppImages. Release notes follow Keep a Changelog
(https://keepachangelog.com/en/1.1.0/); there is no CHANGELOG.md.

## Dependencies

`Cargo.lock` is committed and CI builds with `--locked`. The pinned sources
(SVT-AV1, SVT-AV1-HDR, FFMS2, Vship, libvmaf, Rust) are set in both the
`Dockerfile` and `.github/workflows/appimage.yml`; change them together. FFmpeg
in `appimage.yml` follows the image's Alpine version and is bumped by hand. Base
images and GitHub Actions stay on version tags.

## Pull requests

- One concern per PR. Add tests for behaviour changes.
- `cargo test` and `./test/run.sh` must pass.
- Fill in the PR template, including the test plan and the CLA checkbox.
