# syntax=docker/dockerfile:1

ARG SVT_AV1_VERSION=v4.2.0
# Rolling-release repo: pin a main commit, bump via PR.
ARG SVT_AV1_HDR_REF=18327c0ae91842a548d71303595cf6386fc2433f
ARG FFMS2_VERSION=5.0
ARG VSHIP_VERSION=v5.1.1
ARG VMAF_VERSION=v3.2.1
ARG RUST_VERSION=1.98.1

FROM alpine:3.24 AS base

ARG TARGETARCH

# clang + vulkan build FFVship; nasm is x86-only, arm64 uses NEON.
RUN apk add --no-cache \
        build-base \
        clang \
        cmake \
        git \
        curl \
        pkgconf \
        autoconf \
        automake \
        libtool \
        meson \
        ninja \
        ffmpeg-dev \
        vulkan-headers \
        vulkan-loader-dev \
        zlib-dev \
        ca-certificates && \
    if [ "$TARGETARCH" = "amd64" ]; then apk add --no-cache nasm; fi

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

FROM base AS ffms2

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

# Without the VMAF models busybox xxd suffices.
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

RUN apk add --no-cache jq && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain ${RUST_VERSION} --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY .github/scripts/crate-licenses.sh ./

ENV PKG_CONFIG_PATH=/usr/local/lib/pkgconfig
ENV RUSTFLAGS="-C target-feature=-crt-static"
# The touch is load-bearing: COPY carries host mtimes in, /src/target is a cache mount,
# and cargo would call the crate fresh and ship a stale binary.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry-${TARGETARCH} \
    --mount=type=cache,target=/root/.cargo/git,id=cargo-git-${TARGETARCH} \
    --mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH} \
    find src build.rs -type f -exec touch {} + && \
    cargo build --release --locked && \
    cp /src/target/release/avet /avet && \
    sh crate-licenses.sh /licenses/avet/crates

FROM alpine:3.24 AS runtime

ARG TARGETARCH

RUN apk add --no-cache \
        ffmpeg \
        mkvtoolnix \
        libstdc++ \
        libgcc \
        vulkan-loader \
        mesa-vulkan-swrast && \
    if [ "$TARGETARCH" = "amd64" ]; then apk add --no-cache mesa-vulkan-intel mesa-vulkan-ati; fi

# Alpine's packages carry only an SPDX identifier, so the texts come from the same release.
RUN version() { apk list -I "$1" | sed -n "s/^$1-\([0-9.]*\)-r[0-9]* .*/\1/p"; } && \
    ffmpeg=$(version ffmpeg) && mkvtoolnix=$(version mkvtoolnix) && \
    mkdir -p /usr/share/licenses/ffmpeg /usr/share/licenses/mkvtoolnix && \
    for f in LICENSE.md COPYING.GPLv2 COPYING.GPLv3 COPYING.LGPLv2.1 COPYING.LGPLv3; do \
        wget -q -O "/usr/share/licenses/ffmpeg/$f" "https://raw.githubusercontent.com/FFmpeg/FFmpeg/n$ffmpeg/$f" || exit 1; \
    done && \
    wget -q -O /usr/share/licenses/mkvtoolnix/COPYING "https://codeberg.org/mbunkus/mkvtoolnix/raw/tag/release-$mkvtoolnix/COPYING"

COPY --from=svt-av1     /usr/local/bin/SvtAv1EncApp     /usr/local/bin/SvtAv1EncApp
COPY --from=svt-av1-hdr /usr/local/hdr/bin/SvtAv1EncApp /usr/local/bin/SvtAv1EncApp-hdr
COPY --from=ffms2       /usr/local/bin/ffmsindex        /usr/local/bin/ffmsindex
COPY --from=builder     /avet                           /usr/local/bin/avet
COPY --from=ffms2       /usr/local/lib/libffms2.so*     /usr/local/lib/
COPY --from=vship       /usr/local/bin/FFVship          /usr/local/bin/FFVship
COPY --from=vship       /usr/local/lib/libvship.so      /usr/local/lib/
COPY --from=vmaf        /usr/local/bin/vmaf             /usr/local/bin/vmaf
# musl searches /usr/local/lib itself, so no /etc/ld-musl-<arch>.path is needed.

COPY LICENSE                                  /usr/share/licenses/avet/LICENSE
COPY --from=builder     /licenses/avet/crates /usr/share/licenses/avet/crates
COPY --from=svt-av1     /licenses/svt-av1     /usr/share/licenses/svt-av1
COPY --from=svt-av1-hdr /licenses/svt-av1-hdr /usr/share/licenses/svt-av1-hdr
COPY --from=ffms2       /licenses/ffms2       /usr/share/licenses/ffms2
COPY --from=vship       /licenses/vship       /usr/share/licenses/vship
COPY --from=vmaf        /licenses/libvmaf     /usr/share/licenses/libvmaf

ENV INPUT_DIR=/input
ENV OUTPUT_DIR=/output
ENV POLL_INTERVAL=60

ENTRYPOINT ["/usr/local/bin/avet"]
