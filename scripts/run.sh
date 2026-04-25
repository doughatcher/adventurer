#!/usr/bin/env bash
# Run adventurer-poc inside the Docker container with sensible defaults.
#
# Usage:
#   scripts/run.sh                                  # default: dolphin3, CPU
#   scripts/run.sh --gpu-layers 99                  # enable GPU (need adventurer:vulkan image)
#   scripts/run.sh --ollama                         # A/B path: hit local Ollama on host
#   scripts/run.sh --model /blobs/sha256-...        # different model
#
# Picks adventurer:vulkan if it exists and any --gpu-layers arg > 0 is passed,
# otherwise adventurer:cpu. Falls back gracefully.

set -euo pipefail

OLLAMA_BLOBS="${OLLAMA_BLOBS:-/var/home/me/.ollama/models/blobs}"
HOST_NET_FLAGS=()
GPU_FLAGS=()
IMAGE="adventurer:cpu"

# Heuristic: if any arg requests GPU layers > 0, use the vulkan image.
for arg in "$@"; do
    case "$arg" in
        --gpu-layers)        wants_gpu=1 ;;
        --gpu-layers=*)
            n="${arg#*=}"
            [[ "$n" -gt 0 ]] && wants_gpu=1
            ;;
    esac
done

if [[ "${wants_gpu:-0}" == "1" ]] && docker image inspect adventurer:vulkan >/dev/null 2>&1; then
    IMAGE="adventurer:vulkan"
    GPU_FLAGS=(--device nvidia.com/gpu=all)
fi

# --ollama path needs to reach the host's localhost:11434
for arg in "$@"; do
    if [[ "$arg" == "--ollama" ]]; then
        HOST_NET_FLAGS=(--network host)
        break
    fi
done

echo "→ image: $IMAGE  ${GPU_FLAGS[*]}  ${HOST_NET_FLAGS[*]}" >&2

exec docker run --rm \
    -v "$OLLAMA_BLOBS:/blobs:ro" \
    "${GPU_FLAGS[@]}" \
    "${HOST_NET_FLAGS[@]}" \
    "$IMAGE" \
    "$@"
