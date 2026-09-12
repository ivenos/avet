# syntax=docker/dockerfile:1

ARG SVT_AV1_VERSION=v4.2.0
# Rolling-release repo: pin a main commit, bump via PR.
ARG SVT_AV1_HDR_REF=00333404f455471aaa6ee2c927cac3c93efb76e3
ARG FFMS2_VERSION=5.0
ARG VSHIP_VERSION=v5.1.1
ARG VMAF_VERSION=v3.2.0
ARG RUST_VERSION=1.98.1

FROM alpine:3.24 AS builder

ARG SVT_AV1_VERSION
ARG SVT_AV1_HDR_REF
ARG FFMS2_VERSION
ARG VSHIP_VERSION
ARG VMAF_VERSION
ARG RUST_VERSION
ARG TARGETARCH

# clang + vulkan build FFVship; nasm/yasm are x86-only, arm64 uses NEON.
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
    [ "$TARGETARCH" != "amd64" ] || apk add --no-cache nasm yasm

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain ${RUST_VERSION} --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

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
    rm -rf /svt-av1

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
    rm -rf /svt-av1-hdr

# Shared, not static: C++ static-init crashes when embedded in a Rust binary.
RUN git clone --depth 1 --branch ${FFMS2_VERSION} \
        https://github.com/FFMS/ffms2.git /ffms2 && \
    cd /ffms2 && \
    mkdir -p src/config && \
    autoreconf -fiv && \
    ./configure --prefix=/usr/local --enable-shared=yes --enable-static=no && \
    make -j$(nproc) && \
    make install && \
    rm -rf /ffms2

RUN git clone --depth 1 --branch ${VSHIP_VERSION} \
        https://codeberg.org/Line-fr/Vship.git /vship && \
    cd /vship && \
    make buildVulkan && \
    PKG_CONFIG_PATH=/usr/local/lib/pkgconfig make buildFFVSHIP && \
    install -m755 FFVship /usr/local/bin/FFVship && \
    install -m755 libvship.so /usr/local/lib/libvship.so && \
    rm -rf /vship

# Without the VMAF models busybox xxd suffices; v3.2.0's tests break on vcs_version.h.
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
    rm -rf /vmaf

WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src

ENV PKG_CONFIG_PATH=/usr/local/lib/pkgconfig
ENV RUSTFLAGS="-C target-feature=-crt-static"
# The touch is load-bearing: COPY carries host mtimes in, /src/target is a cache mount,
# and cargo would call the crate fresh and ship a stale binary.
RUN --mount=type=cache,target=/root/.cargo/registry,id=cargo-registry-${TARGETARCH} \
    --mount=type=cache,target=/root/.cargo/git,id=cargo-git-${TARGETARCH} \
    --mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH} \
    find src build.rs -type f -exec touch {} + && \
    cargo build --release --locked && \
    cp /src/target/release/avxs /avxs

FROM alpine:3.24 AS runtime

ARG TARGETARCH

RUN apk add --no-cache \
        ffmpeg \
        mkvtoolnix \
        libstdc++ \
        libgcc \
        vulkan-loader \
        mesa-vulkan-swrast && \
    [ "$TARGETARCH" != "amd64" ] || apk add --no-cache mesa-vulkan-intel mesa-vulkan-ati

COPY --from=builder /usr/local/bin/SvtAv1EncApp     /usr/local/bin/SvtAv1EncApp
COPY --from=builder /usr/local/hdr/bin/SvtAv1EncApp /usr/local/bin/SvtAv1EncApp-hdr
COPY --from=builder /usr/local/bin/ffmsindex         /usr/local/bin/ffmsindex
COPY --from=builder /avxs                             /usr/local/bin/avxs
# Not in Alpine's package manager.
COPY --from=builder /usr/local/lib/libffms2.so*      /usr/local/lib/
# FFVship + libvship, for target_quality.
COPY --from=builder /usr/local/bin/FFVship           /usr/local/bin/FFVship
COPY --from=builder /usr/local/lib/libvship.so       /usr/local/lib/
COPY --from=builder /usr/local/bin/vmaf              /usr/local/bin/vmaf
# musl searches /usr/local/lib itself, so no /etc/ld-musl-<arch>.path is needed.

ENV AVXS_INPUT_DIR=/input
ENV AVXS_OUTPUT_DIR=/output
ENV AVXS_POLL_INTERVAL=60

ENTRYPOINT ["/usr/local/bin/avxs"]
