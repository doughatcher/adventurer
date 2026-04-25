# adventurer

Rust proofs-of-concept for the in-process LLM + STT inference path that will eventually replace [`dnd-stage`](../dnd-stage)'s two external HTTP services (Ollama + Speaches) with a single statically-linked binary.

**This crate proves two things:**
1. The Gemma 4 state-extraction prompt from `dnd-stage/server/gemma.py` runs faster and at least as well via in-process `llama-cpp-2` as it does through the existing Ollama HTTP path — on real GPU hardware, with the same model.
2. The Whisper STT pipeline that drives `dnd-stage`'s transcript can be moved in-process via `whisper-rs` with no transcript-quality regression vs Speaches, fast enough for live use.

Nothing here is the eventual product. No HTTP server, no UI, no continuous audio capture. Just inference A/B.

Two binaries:
- `adventurer-poc` — LLM state extraction (replaces Ollama HTTP path)
- `adventurer-stt-poc` — Whisper transcription (replaces Speaches HTTP path)

## TL;DR — bench results

### LLM state extraction

Same `STATE_PROMPT`, same fixture (`samples/transcript.md` + `samples/party.md`), RTX 4070 + Ryzen 9 9950X:

| Backend                                | Hardware    | t/s       | JSON valid | vs Ollama |
| -------------------------------------- | ----------- | --------- | ---------- | --------- |
| Ollama `gemma4:e4b` (production today) | RTX 4070    | 49.7      | ✗ truncated| 1.0×      |
| `llama-cpp-2` + dolphin3:8b            | CPU only    | 11.5      | ✓          | 0.23×     |
| `llama-cpp-2` + Gemma 4 E4B Q4_K_M     | CPU only    | 16.6      | ✓          | 0.33×     |
| `llama-cpp-2` + Gemma 4 E4B Q4_K_M     | **CUDA GPU**| **109.1** | **✓**      | **2.20×** |

In-process CUDA inference is **~2.2× faster than the existing Ollama production path** on the same model and same prompt, and produces clean JSON every time.

### STT transcription

Same 30-second clip from a real archived `dnd-stage` session (`samples/audio/clip.mp3`), `whisper-medium` model on both backends:

| Backend                                  | Hardware     | Wall time | Realtime  | Words | Transcript match  |
| ---------------------------------------- | ------------ | --------- | --------- | ----- | ----------------- |
| Speaches (faster-whisper, prod today)    | RTX 4070     | 2.05s     | **14.7×** | 46    | baseline          |
| `whisper-rs` + ggml-medium               | **CUDA GPU** | 5.26s     | 5.7×      | 46    | semantic-identical|

Speaches wins on raw speed thanks to CTranslate2's INT8 kernels; whisper.cpp defaults to FP16. Transcripts are word-count identical and semantically interchangeable. **5.7× realtime is plenty for live transcription** — a live 5-second audio chunk transcribes in <1s. Speaches's INT8 advantage can be closed by switching to a quantized GGML model (`ggml-medium-q5_0.bin` or similar) or by using `ct2rs` (CTranslate2 Rust bindings, the same engine Speaches wraps).

## What it does

**`adventurer-poc` (LLM):**
1. Loads the same `STATE_PROMPT` template that `dnd-stage/server/gemma.py` uses
2. Substitutes the sample party + sample transcript fixture
3. Runs the prompt through one of two backends:
   - **default**: `llama-cpp-2` loading a GGUF directly from disk, in-process
   - **`--ollama`**: HTTP POST to `http://localhost:11434/api/generate` (current `dnd-stage` path)
4. Streams generated tokens to stderr, prints final JSON to stdout
5. Validates output is parseable JSON with `characters` + `location` keys

**`adventurer-stt-poc` (STT):**
1. Decodes an audio file (mp3/webm/wav/anything ffmpeg understands) to 16 kHz mono f32 PCM
   - *Lazy ffmpeg subprocess for now; production binary will use `symphonia` for pure-Rust decoding*
2. Runs the PCM through one of two backends:
   - **default**: `whisper-rs` + a GGML model file, in-process
   - **`--speaches`**: multipart POST to `http://localhost:8000/v1/audio/transcriptions` (current `dnd-stage` path)
3. Prints transcript to stdout, timing + realtime factor to stderr
4. Validates non-empty + reports word/char count

## Build (Docker — recommended)

The Bazzite host is immutable (rpm-ostree); installing build deps natively means Linuxbrew layering for cmake, vulkan-sdk, glslc, libnccl-dev, etc. Docker codifies all that and gives us a reproducible, CI-shaped pipeline.

Three image variants:

| Image               | Backend       | Size    | Use case                                  |
| ------------------- | ------------- | ------- | ----------------------------------------- |
| `adventurer:cpu`    | CPU + OpenMP  | ~150 MB | Anywhere; slow on big models              |
| `adventurer:vulkan` | Vulkan        | ~170 MB | Built but **GPU offload doesn't engage**  |
|                     |               |         | inside containers due to NVIDIA path mismatch — see Findings |
| `adventurer:cuda`   | CUDA + cuBLAS | 3.87 GB | The fast path on NVIDIA hardware          |

```bash
# CPU
docker build -t adventurer:cpu .

# Vulkan (binary builds, GPU runtime won't activate inside container — see Findings)
docker build -t adventurer:vulkan --build-arg CARGO_FEATURES=vulkan .

# CUDA — uses NVIDIA's CUDA base image (~3 GB pull first time, ~10 min compile)
docker build -f Dockerfile.cuda -t adventurer:cuda .
```

## Build (native, without Docker)

```bash
brew install cmake          # llama-cpp-sys-2 needs it
cargo build --release       # CPU only
```

CUDA/Vulkan native builds need the CUDA toolkit / Vulkan SDK (libvulkan-dev, glslc, Vulkan-Headers ≥ 1.4). Easier to just use Docker.

## Run

The image now ships **two binaries** (`adventurer-poc` for LLM, `adventurer-stt-poc` for STT). The default `CMD` shows the LLM PoC's help; the STT one is invoked by appending `adventurer-stt-poc` after the image name.

### LLM, CPU

```bash
docker run --rm \
    -v /var/home/me/.ollama/models/blobs:/blobs:ro \
    adventurer:cpu adventurer-poc \
    --model /blobs/sha256-1eee6953530837b2b17d61a4e6f71a5aa31c9714cfcf3cb141aa5c1972b5116b
# (this is the dolphin3:8b blob from Ollama's cache — Llama 3.1 arch, well-supported)
```

### LLM, CUDA GPU

```bash
docker run --rm \
    --device nvidia.com/gpu=all \
    -v /var/home/me/repos/adventurer/models:/models:ro \
    adventurer:cuda adventurer-poc \
    --model /models/gemma-4-E4B-it-Q4_K_M.gguf \
    --gpu-layers 99 \
    --max-tokens 400
```

`--device nvidia.com/gpu=all` uses NVIDIA Container Device Interface (CDI). No `--gpus` flag, no `--runtime=nvidia` — the CDI registration is enough and Docker on this Bazzite box already has it wired (see `docker info | grep cdi`).

### STT, CUDA GPU

```bash
docker run --rm \
    --device nvidia.com/gpu=all \
    -v /var/home/me/repos/adventurer/models:/models:ro \
    -v /var/home/me/repos/adventurer/samples:/work/samples:ro \
    adventurer:cuda adventurer-stt-poc \
    --model /models/ggml-medium.bin \
    --audio /work/samples/audio/clip.mp3
```

### A/B baselines (against the production HTTP services on the host)

```bash
# LLM → Ollama
docker run --rm --network host \
    adventurer:cpu adventurer-poc --ollama

# STT → Speaches
# (--device nvidia.com/gpu=all is needed even though we don't use the GPU here:
#  the cuda-linked binary dynamically links libcuda.so.1 from the host's NVIDIA mount)
docker run --rm --network host --device nvidia.com/gpu=all \
    -v /var/home/me/repos/adventurer/samples:/work/samples:ro \
    adventurer:cuda adventurer-stt-poc --speaches \
    --audio /work/samples/audio/clip.mp3
```

`--network host` so the container can reach `localhost:11434` and `localhost:8000` on the host.

### Wrapper script

`scripts/run.sh` picks the right image and flags based on args (LLM PoC only currently):

```bash
scripts/run.sh                     # CPU, default model
scripts/run.sh --gpu-layers 99     # auto-uses adventurer:cuda if it exists
scripts/run.sh --ollama            # auto-adds --network host
```

## Output format

```bash
./adventurer-poc > state.json 2> run.log
diff state.json <(./adventurer-poc --ollama 2>/dev/null)
```

Stdout = JSON, stderr = streaming tokens + timing — pipe-friendly.

Pass criteria:

- ✅ Output is parseable JSON
- ✅ `characters` map includes all 3 PCs from `samples/party.md` plus the captain and worgs from the transcript
- ✅ HP arithmetic is correct (Rides 31, Granit ≤28, Captain dropped)
- ✅ `combat_active: true`, `initiative_order` populated
- ✅ Tokens/second is in the same order of magnitude as Ollama (or better)

## Findings

**1. Production `gemma4:e4b` blob is incompatible with vanilla llama.cpp.**
Ollama's `gemma4:e4b` GGUF (9.6 GB on disk) is a multimodal variant with `gemma4.audio.*` metadata keys. `llama-cpp-sys-2 0.1.145`'s bundled llama.cpp loads only 720 of the expected tensors and bails. *The vanilla unsloth `gemma-4-E4B-it-Q4_K_M.gguf` (text-only, ~4.7 GB) loads cleanly* — same architecture, different tensor topology. **Implication for the production binary:** ship our own GGUF files, don't piggyback Ollama's blob cache.

**2. CUDA in-process beats Ollama by 2.2× on the same model.**
Ollama is a Go HTTP wrapper over the same llama.cpp this binary embeds. Eliminating the HTTP loop, JSON serialization, and process boundary buys a real ~50% throughput improvement on top of equivalent kernel performance. On this 4070, that's 49 t/s → 109 t/s.

**3. Quality is at parity, sometimes better than Ollama.**
The dolphin3:8b CPU run actually beat Ollama's gemma4 production output on `round` and captain `status` tracking. Different model, but proves the prompt + sampler + parser stack is sound. Vanilla Gemma 4 produces output essentially identical to Ollama's gemma4 (same `worg-1`/`worg-2` pattern, same captain-status quirk).

**4. Bazzite + Docker + CUDA: NVIDIA's image is the path.** Vulkan-in-Debian-container hits a path mismatch — NVIDIA Container Toolkit on Bazzite (Fedora-based host) mounts driver libs at `/usr/lib64/`, but Debian's `ld.so` searches `/usr/lib/x86_64-linux-gnu/`. `LD_LIBRARY_PATH=/usr/lib64` alone wasn't enough to make the Vulkan loader find the NVIDIA ICD. **NVIDIA's `nvidia/cuda:*-runtime-ubuntu24.04` base image sidesteps this entirely** — its loader config is wired correctly for CDI passthrough out of the box. Trade is image size (3.3 GB vs 116 MB).

**5. `llama-cpp-sys-2 0.1.145` doesn't auto-link `-lnccl`.** With CUDA on, the compiled C++ unconditionally references `ncclCommInitAll`/`ncclAllReduce`/etc., but the build script doesn't emit `cargo:rustc-link-lib=nccl`. Workaround in `Dockerfile.cuda`: `ENV RUSTFLAGS="-C link-arg=-lnccl"`. Should file an upstream issue.

**6. STT: transcript quality at parity, raw speed loses to Speaches's INT8 advantage.**
On the 30-second sample clip, both backends produced 46-word transcripts that say the same thing. Speaches transcribed in 2.05s (14.7× realtime), `whisper-rs` + ggml-medium in 5.26s (5.7× realtime). The gap is CTranslate2's INT8 quantization vs whisper.cpp's FP16 default — not a Rust integration weakness. Two paths to close it later if needed: (a) ship a quantized GGML model (`ggml-medium-q5_0.bin` — smaller AND faster), (b) swap to `ct2rs` bindings (CTranslate2 from Rust, same engine Speaches wraps). For live D&D session transcription where chunks are 5–10 seconds, 5.7× realtime is comfortably fast enough.

**7. whisper-rs 0.16 API notes (in case you copy from older docs).**
- `state.full_n_segments()` returns `c_int` directly, not `Result<i32>`
- `full_get_segment_text(i)` removed; use `state.get_segment(i)` → `Option<WhisperSegment>` then `.to_str()` → `Result<&str>`
- The `cuda` feature on `whisper-rs` builds whisper.cpp with cuBLAS and "just works" with NVIDIA CDI passthrough — no extra symbol-link gymnastics like llama-cpp-sys-2 needed

**8. Build environment notes.**
- First CUDA build is ~10 min on a 9950X (32 thread); whisper.cpp adds ~5 min. Most is nvcc compiling kernels for sm_50..sm_90. Could pin to `-DCMAKE_CUDA_ARCHITECTURES=89` for the 4070 alone and cut this dramatically.
- Debian bookworm's Vulkan headers are 1.3.x; ggml-vulkan needs 1.4. `Dockerfile` overrides apt's headers with Khronos Vulkan-Headers v1.4.341.
- `libnccl-dev` on the CUDA 12.6 devel image needs explicit version pin (`libnccl-dev=2.22.3-1+cuda12.6`) to match the held `libnccl2`.
- `ffmpeg` was added to runtime images for STT's lazy decode path. Will go away once we swap to `symphonia` for pure-Rust audio decoding.

## What's next (out of scope for this PoC)

Both inference paths are now validated. The unfinished items:

1. **Audio capture** — actual microphone input → chunked PCM, the live counterpart to today's static-file STT PoC. `cpal` for cross-platform mic input + a 5–10 second windowing buffer.
2. **Pure-Rust audio decode** — replace the ffmpeg subprocess with `symphonia` so the binary has no runtime ffmpeg dependency and webm/opus chunks from a browser MediaRecorder work directly.
3. **Bigger LLM model on the same hardware** — Gemma 4 12B Q4_K_M (~7 GB) on the 4070 should fit and produce richer output for the slow-path panel updates.
4. **Quantized whisper** — try `ggml-medium-q5_0.bin` to close the speed gap with Speaches.
5. **Pin CUDA arch** — drop build time from 10+ min to ~2 min by targeting only sm_89.
6. **The actual port** — `axum` server replacing `server/main.py`, embedded inference (LLM + STT) replacing the Ollama + Speaches HTTP calls, `notify` replacing `watchfiles`, `git2` replacing the `git`/`gh` subprocess shellouts.

## Layout

```
adventurer/
├── Cargo.toml               (two [[bin]] entries — adventurer-poc, adventurer-stt-poc)
├── Dockerfile               (CPU + Vulkan via build arg)
├── Dockerfile.cuda          (CUDA — separate base image, builds both bins)
├── .dockerignore
├── README.md                (this file)
├── prompts/
│   └── state.txt            (verbatim STATE_PROMPT from dnd-stage/server/gemma.py)
├── samples/
│   ├── party.md             (3 PCs with HP/AC/class — sample party fixture)
│   ├── transcript.md        (combat encounter — captain + 2 worgs)
│   └── audio/
│       └── clip.mp3         (30s slice from a real archived dnd-stage session)
├── models/                  (gitignored — Gemma 4 GGUFs + ggml-medium.bin live here)
├── scripts/
│   └── run.sh               (wrapper: picks image + GPU flags + host network)
└── src/
    ├── main.rs              (LLM PoC: llama-cpp-2 + Ollama A/B + JSON validator)
    └── bin/
        └── stt_poc.rs       (STT PoC: whisper-rs + Speaches A/B + transcript validator)
```
