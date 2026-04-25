# syntax=docker/dockerfile:1.7
#
# adventurer-poc — reproducible build environment.
#
# Why Docker: Bazzite (immutable rpm-ostree) makes layering toolchain bits painful.
# Each new dep (cmake, vulkan-sdk, glslc, …) means more Linuxbrew or a reboot.
# A Debian-based build container codifies all that, runs identically everywhere,
# and is the same shape CI will use.
#
# Build:
#   docker build -t adventurer:cpu .
#   docker build -t adventurer:vulkan --build-arg CARGO_FEATURES=vulkan .
#
# Run (CPU, mounting Ollama blob as /models/dolphin3.gguf):
#   docker run --rm \
#     -v /var/home/me/.ollama/models/blobs:/blobs:ro \
#     adventurer:cpu \
#     --model /blobs/sha256-1eee6953530837b2b17d61a4e6f71a5aa31c9714cfcf3cb141aa5c1972b5116b
#
# Run (Vulkan, GPU passthrough via NVIDIA CDI):
#   docker run --rm --device nvidia.com/gpu=all \
#     -v /var/home/me/.ollama/models/blobs:/blobs:ro \
#     adventurer:vulkan --gpu-layers 99 \
#     --model /blobs/sha256-...
#
# Extract the binary to the host (skip runtime image):
#   docker build --target builder -t adventurer:build .
#   docker create --name extract adventurer:build
#   docker cp extract:/work/target/release/adventurer-poc ./adventurer-poc
#   docker rm extract

# ────────────── builder stage ──────────────
FROM rust:1.95-bookworm AS builder

# cmake     — llama-cpp-sys-2 invokes cmake to build llama.cpp
# build-essential, pkg-config — C/C++ toolchain
# clang, libclang-dev — bindgen needs libclang to generate Rust FFI from C++ headers
# libomp-dev — OpenMP for CPU inference
# libvulkan-dev — apt's loader stub (Debian bookworm ships old 1.3 headers; we override below)
# glslc      — shader compiler (shaderc), only needed for vulkan feature
# git, curl  — llama.cpp's CMakeLists pokes git; curl pulls Vulkan-Headers tarball
RUN apt-get update && apt-get install -y --no-install-recommends \
        cmake \
        build-essential \
        pkg-config \
        clang \
        libclang-dev \
        libomp-dev \
        libvulkan-dev \
        glslc \
        git \
        curl \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Debian bookworm's Vulkan headers are 1.3.x; ggml-vulkan needs vk::LayerSettingEXT
# from Vulkan 1.4. Override apt's headers with Khronos Vulkan-Headers v1.4.341
# (matches the host driver version on Bazzite, and what brew installs).
ARG VULKAN_HEADERS_VERSION=v1.4.341
RUN curl -fL "https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/${VULKAN_HEADERS_VERSION}.tar.gz" \
      | tar -xz -C /tmp \
    && cmake -S "/tmp/Vulkan-Headers-${VULKAN_HEADERS_VERSION#v}" -B /tmp/vk-build \
         -DCMAKE_INSTALL_PREFIX=/usr/local \
    && cmake --install /tmp/vk-build \
    && rm -rf "/tmp/Vulkan-Headers-${VULKAN_HEADERS_VERSION#v}" /tmp/vk-build

WORKDIR /work
COPY Cargo.toml ./
COPY src ./src
COPY prompts ./prompts
COPY samples ./samples

ARG CARGO_FEATURES=""

# BuildKit cache mounts keep cargo registry + target/ warm across rebuilds.
# Final binary is `cp`'d out so it survives the cache mount unmount.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/work/target,sharing=locked \
    if [ -n "$CARGO_FEATURES" ]; then \
        cargo build --release --features "$CARGO_FEATURES"; \
    else \
        cargo build --release; \
    fi && \
    cp target/release/adventurer-poc /adventurer-poc

# ────────────── runtime stage ──────────────
FROM debian:bookworm-slim AS runtime

# libgomp1   — OpenMP runtime for CPU inference
# libvulkan1 — Vulkan loader (the GPU ICD comes from the host via NVIDIA CDI)
# ca-certificates — for the --ollama HTTP path
RUN apt-get update && apt-get install -y --no-install-recommends \
        libgomp1 \
        libvulkan1 \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /adventurer-poc /usr/local/bin/adventurer-poc
COPY prompts /work/prompts
COPY samples /work/samples
WORKDIR /work

ENTRYPOINT ["adventurer-poc"]
CMD ["--help"]
