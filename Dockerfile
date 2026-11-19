# syntax=docker/dockerfile:1

ARG SVT_AV1_VERSION=v4.3.0
# Rolling-release repo.
ARG SVT_AV1_HDR_REF=0ce7e13af2396bf3bf06408a09f1138f82b01b28
ARG FFMS2_VERSION=5.0
ARG FFMPEG_VERSION=n8.1.3
ARG MKVTOOLNIX_VERSION=99.0
ARG VSHIP_VERSION=v5.1.2
ARG VMAF_VERSION=v3.2.1
ARG RUST_VERSION=1.100.0

FROM ubuntu:24.04 AS base

ARG TARGETARCH

# nasm is x86-only, arm64 uses NEON.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        build-essential \
        clang \
        cmake \
        git \
        curl \
        ca-certificates \
        pkg-config \
        autoconf \
        automake \
        libtool \
        meson \
        ninja-build \
        jq \
        zlib1g-dev \
        libbz2-dev \
        liblzma-dev \
        libopus-dev \
        libmp3lame-dev \
        libvorbis-dev \
        libdav1d-dev \
        libx264-dev \
        libx265-dev \
        libvpx-dev \
        libsvtav1enc-dev \
        libvulkan-dev && \
    if [ "$TARGETARCH" = "amd64" ]; then apt-get install -y --no-install-recommends nasm; fi && \
    rm -rf /var/lib/apt/lists/*

FROM base AS svt-av1

ARG SVT_AV1_VERSION
ARG TARGETARCH

RUN git clone --depth 1 --branch ${SVT_AV1_VERSION} \
        https://gitlab.com/AOMediaCodec/SVT-AV1.git /svt-av1 && \
    cmake -B /svt-av1/build /svt-av1 \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX=/usr/local \
        -DBUILD_SHARED_LIBS=OFF \
        -DENABLE_AVX512=$([ "$TARGETARCH" = "amd64" ] && echo ON || echo OFF) \
        -DNATIVE=OFF && \
    cmake --build /svt-av1/build --parallel $(nproc) && \
    cmake --install /svt-av1/build && \
    install -Dm644 -t /licenses/svt-av1 /svt-av1/LICENSE.md /svt-av1/LICENSE-BSD2.md /svt-av1/PATENTS.md && \
    rm -rf /svt-av1

FROM base AS svt-av1-hdr

ARG SVT_AV1_HDR_REF
ARG TARGETARCH

RUN git clone --filter=blob:none --no-checkout \
        https://github.com/juliobbv-p/svt-av1-hdr.git /svt-av1-hdr && \
    git -C /svt-av1-hdr checkout ${SVT_AV1_HDR_REF} && \
    cmake -B /svt-av1-hdr/build /svt-av1-hdr \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX=/usr/local/hdr \
        -DBUILD_SHARED_LIBS=OFF \
        -DENABLE_AVX512=$([ "$TARGETARCH" = "amd64" ] && echo ON || echo OFF) \
        -DNATIVE=OFF && \
    cmake --build /svt-av1-hdr/build --parallel $(nproc) && \
    cmake --install /svt-av1-hdr/build && \
    install -Dm644 -t /licenses/svt-av1-hdr /svt-av1-hdr/LICENSE.md /svt-av1-hdr/LICENSE-BSD2.md /svt-av1-hdr/PATENTS.md && \
    rm -rf /svt-av1-hdr

# Ubuntu 24.04 ships 6.1, too old for Vship (AVCOL_SPC_YCGCO_RE).
FROM base AS ffmpeg

ARG FFMPEG_VERSION

# The test suites encode their sources with x264, x265, libvpx and SVT-AV1.
RUN git clone --depth 1 --branch ${FFMPEG_VERSION} \
        https://github.com/FFmpeg/FFmpeg.git /ffmpeg && \
    cd /ffmpeg && \
    ./configure \
        --prefix=/usr/local \
        --enable-shared --disable-static \
        --disable-doc --disable-ffplay \
        --enable-gpl \
        --enable-libopus --enable-libmp3lame --enable-libvorbis --enable-libdav1d \
        --enable-libx264 --enable-libx265 --enable-libvpx --enable-libsvtav1 && \
    make -j$(nproc) && \
    make install && \
    ldconfig && \
    install -Dm644 -t /licenses/ffmpeg LICENSE.md COPYING.GPLv2 COPYING.LGPLv2.1 && \
    rm -rf /ffmpeg

FROM ffmpeg AS ffms2

ARG FFMS2_VERSION

COPY packaging/ffms2-frame-hdr-metadata.patch /tmp/
# Shared, not static: C++ static-init crashes when embedded in a Rust binary.
RUN git clone --depth 1 --branch ${FFMS2_VERSION} \
        https://github.com/FFMS/ffms2.git /ffms2 && \
    cd /ffms2 && \
    git apply /tmp/ffms2-frame-hdr-metadata.patch && \
    mkdir -p src/config && \
    autoreconf -fiv && \
    ./configure --prefix=/usr/local --enable-shared=yes --enable-static=no && \
    make -j$(nproc) && \
    make install && \
    ldconfig && \
    install -Dm644 -t /licenses/ffms2 COPYING && \
    rm -rf /ffms2

FROM ffms2 AS vship

ARG VSHIP_VERSION

RUN git clone --depth 1 --branch ${VSHIP_VERSION} \
        https://codeberg.org/Line-fr/Vship.git /vship && \
    cd /vship && \
    make buildVulkan && \
    PKG_CONFIG_PATH=/usr/local/lib/pkgconfig make buildFFVSHIP && \
    install -m755 FFVship /usr/local/bin/FFVship && \
    install -m755 libvship.so /usr/local/lib/libvship.so && \
    install -Dm644 -t /licenses/vship LICENSE && \
    rm -rf /vship

FROM base AS vmaf

ARG VMAF_VERSION

# CAMBI uses no VMAF model.
RUN git clone --depth 1 --branch ${VMAF_VERSION} \
        https://github.com/Netflix/vmaf.git /vmaf && \
    meson setup /vmaf/libvmaf/build /vmaf/libvmaf \
        --buildtype release \
        -Dbuilt_in_models=false \
        -Denable_float=false \
        -Denable_tests=false \
        -Denable_docs=false && \
    ninja -C /vmaf/libvmaf/build && \
    install -m755 /vmaf/libvmaf/build/tools/vmaf /usr/local/bin/vmaf && \
    install -Dm644 -t /licenses/libvmaf /vmaf/LICENSE && \
    rm -rf /vmaf

FROM ffms2 AS builder

ARG RUST_VERSION
ARG TARGETARCH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain ${RUST_VERSION} --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY .github/scripts/crate-licenses.sh ./

# rust-lld, the default linker on x86_64, does not search /usr/local/lib.
ENV RUSTFLAGS="-L native=/usr/local/lib"
ARG AVET_VERSION
# cargo compares the host's mtimes with the cached /src/target and can skip the rebuild.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry-${TARGETARCH} \
    --mount=type=cache,target=/root/.cargo/git,id=cargo-git-${TARGETARCH} \
    --mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH},sharing=locked \
    find src -type f -exec touch {} + && \
    cargo build --release --locked && \
    cp /src/target/release/avet /avet && \
    sh crate-licenses.sh /licenses/avet/crates

FROM ubuntu:24.04 AS runtime

ARG MKVTOOLNIX_VERSION
# A new version invalidates the cached apt layer below.
ARG AVET_VERSION

# NVIDIA's Vulkan driver loads libEGL, libX11 and libXext from the image.
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    mkdir -p /etc/apt/keyrings && \
    curl -fsSL -o /etc/apt/keyrings/gpg-pub-moritzbunkus.gpg https://mkvtoolnix.download/gpg-pub-moritzbunkus.gpg && \
    echo "deb [signed-by=/etc/apt/keyrings/gpg-pub-moritzbunkus.gpg] https://mkvtoolnix.download/ubuntu/ noble main" \
        > /etc/apt/sources.list.d/mkvtoolnix.download.list && \
    apt-get update && \
    mkvtoolnix=$(apt-cache madison mkvtoolnix | awk -v v="$MKVTOOLNIX_VERSION" 'index($3, v "-") == 1 { print $3; exit }') && \
    apt-get install -y --no-install-recommends \
        "mkvtoolnix=$mkvtoolnix" \
        libopus0 \
        libmp3lame0 \
        libvorbis0a \
        libvorbisenc2 \
        libdav1d7 \
        libx264-164 \
        libx265-199 \
        libvpx9 \
        libsvtav1enc1d1 \
        libvulkan1 \
        mesa-vulkan-drivers \
        libegl1 \
        libx11-6 \
        libxext6 && \
    apt-get purge -y --autoremove curl && \
    rm -rf /var/lib/apt/lists/*

COPY --from=ffmpeg      /usr/local/bin/ffmpeg           /usr/local/bin/ffmpeg
COPY --from=ffmpeg      /usr/local/bin/ffprobe          /usr/local/bin/ffprobe
COPY --from=ffms2       /usr/local/lib/                 /usr/local/lib/
COPY --from=svt-av1     /usr/local/bin/SvtAv1EncApp     /usr/local/bin/SvtAv1EncApp
COPY --from=svt-av1-hdr /usr/local/hdr/bin/SvtAv1EncApp /usr/local/bin/SvtAv1EncApp-hdr
COPY --from=ffms2       /usr/local/bin/ffmsindex        /usr/local/bin/ffmsindex
COPY --from=builder     /avet                           /usr/local/bin/avet
COPY --from=vship       /usr/local/bin/FFVship          /usr/local/bin/FFVship
COPY --from=vship       /usr/local/lib/libvship.so      /usr/local/lib/
COPY --from=vmaf        /usr/local/bin/vmaf             /usr/local/bin/vmaf
RUN ldconfig

COPY LICENSE                                  /usr/share/licenses/avet/LICENSE
COPY --from=builder     /licenses/avet/crates /usr/share/licenses/avet/crates
COPY --from=svt-av1     /licenses/svt-av1     /usr/share/licenses/svt-av1
COPY --from=svt-av1-hdr /licenses/svt-av1-hdr /usr/share/licenses/svt-av1-hdr
COPY --from=ffmpeg      /licenses/ffmpeg      /usr/share/licenses/ffmpeg
COPY --from=ffms2       /licenses/ffms2       /usr/share/licenses/ffms2
COPY --from=vship       /licenses/vship       /usr/share/licenses/vship
COPY --from=vmaf        /licenses/libvmaf     /usr/share/licenses/libvmaf

# NVIDIA documents the graphics capability as required for Vulkan.
ENV NVIDIA_DRIVER_CAPABILITIES=graphics,compute,utility
ENV INPUT_DIR=/input
ENV OUTPUT_DIR=/output
ENV POLL_INTERVAL=60

ENTRYPOINT ["/usr/local/bin/avet"]
