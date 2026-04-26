# adventurer

Live D&D session companion — single Rust binary that replaces [`dnd-stage`](../dnd-stage)'s
Ollama + Speaches + ffmpeg + git stack with in-process inference (gemma4 LLM via
llama.cpp, whisper.cpp STT) and a clean axum + WebSocket server that serves the
existing dnd-stage UI from `rust-embed`.

**Status (as of 2026-04-26):** working end-to-end on `bazzite-desktop`. Launches
from Steam Big Picture as a non-Steam game, exposes the UI fullscreen via Chrome,
serves a player view (mobile-friendly) over Cloudflare Tunnel for iPad-as-mic,
and pushes session state to GitHub via the Trees API on demand.

## Vision (target end state)

Today this is a docker-orchestrated dev rig that one person (me) launches from
Steam on one machine. The goal is to collapse it into a **single statically-
linked binary** — `adventurer.exe` on Windows, `adventurer` on Linux — that
ships through **Steamworks as an early-access experience** anyone can install
and try.

What "shipped" looks like:

- **Single executable per OS.** No docker, no python, no Ollama install, no
  Speaches container. The binary embeds the UI (rust-embed already does this),
  spawns its own LLM and STT worker subprocesses, and bundles the model
  weights via the Steam content depot (or downloads them on first run with a
  HuggingFace-style content addressable cache).
- **CUDA on Windows + Linux, Vulkan fallback for AMD/Intel.** llama.cpp and
  whisper.cpp both compile to all three; the workspace already has feature
  flags to pick a backend at build time (`--features cuda` / `--features
  vulkan`). The release build will probably ship as N variants — `_cuda12`,
  `_vulkan`, `_cpu` — and a tiny launcher picks at runtime.
- **Steamworks integration.** Real `steam_appid.txt`, real depot upload,
  Steamworks SDK for: cloud save (replaces the `~/.local/share/adventurer/
  session/` dir), Workshop publishing for adventure modules / saved
  campaigns, achievements ("survived a session", "100 hours of D&D"), Big
  Picture-friendly controller config that ships with the game (so users
  don't have to manually pick "Keyboard and Mouse" template).
- **No mandatory cloud.** All inference is local. Only opt-in features
  reach out to the network: GitHub session push (DM-only knowledge mgmt),
  Workshop browse, Steam friends-list invite to drop a player into your
  campaign. The killer feature is "your D&D table works on a flight."
- **Early Access positioning.** First Steam release is for **DMs running
  in-person home games who want a second screen** — same audience the
  vendored `dnd-stage` UI was built for. Roadmap from there: voice-only
  "DM in your earbuds" mode, mobile player apps as native Steam Remote Play
  sessions, model-quality tiers (4B / 8B / 27B based on user's GPU).
- **Repo layout already aligned to ship.** `assets/steam/` is where final
  store-page art lives (capsule, hero, logo, screenshots — same dimensions
  Steamworks accepts, see `assets/steam/README.md`). `scripts/add-to-steam.py`
  is the local-sideload analog of `steamcmd` depot upload — both produce the
  same artifact (a Steam appid that points at our exe). The `crates/server`
  → `crates/inference-{llm,stt}` → 3-process IPC architecture is exactly
  what the binary will look like; the only Steam-specific work left is
  swapping docker-launch for direct subprocess and wiring the Steamworks
  SDK calls.

## Quick start

```bash
# Build the image (CUDA, RTX 4070 / 12 GB VRAM target)
cd ~/repos/adventurer
DOCKER_BUILDKIT=1 docker build -f Dockerfile.cuda -t adventurer:cuda .

# Headless — server only, no browser. Reachable on http://127.0.0.1:3210/
ADVENTURER_HEADLESS=1 ~/bin/adventurer-launch.sh

# OR launch from Steam Big Picture: hit Play on the "Adventure Log" non-Steam
# shortcut. Chrome opens fullscreen at 2× DPI scale, tracks "playtime", and
# Steam's "Stop game" cleanly tears down the container + Chrome window.
```

Logs land in `~/.local/state/adventurer/launch.log`; live container logs via
`docker logs -f adventurer-live`.

## What ships

Three binaries, one server + two long-running worker child processes:

| Binary                   | Purpose                                   | Linked engine             |
| ------------------------ | ----------------------------------------- | ------------------------- |
| `adventurer`             | axum HTTP/WS server, owns session state   | (none — calls workers)    |
| `adventurer-llm-bench`   | LLM state-extraction worker (`--worker`)  | llama-cpp-2 / llama.cpp   |
| `adventurer-stt-bench`   | Whisper STT worker (`--worker`)           | whisper-rs / whisper.cpp  |

The two engines have to live in **separate processes** because llama.cpp and
whisper.cpp each vendor their own ggml — linking both into one binary
silently produces a runtime-crashing executable. The server spawns both
workers serially on startup (CUDA init from two processes onto the same GPU
simultaneously deadlocks) and talks to them via line-delimited JSON on
stdin/stdout.

Audio handoff to the STT worker uses a tempfile path (not base64-in-JSON, not
ffmpeg-stdin pipe) — both alternatives hit subtle pipe-buffer / fd-dup bugs
documented in code comments.

## Architecture at runtime

```
┌─────────────────────────────────────────────────────────────────────────┐
│  bazzite-desktop                                                        │
│                                                                         │
│  Steam Big Picture ──launch──► adventurer-launch.sh                     │
│                                  │                                      │
│                                  ├─► docker run adventurer:cuda         │
│                                  │     │                                │
│                                  │     ├─► adventurer (axum:3210)       │
│                                  │     │     │                          │
│                                  │     │     ├─► adventurer-llm-bench   │
│                                  │     │     │     (--worker, gemma4    │
│                                  │     │     │      on RTX 4070)        │
│                                  │     │     │                          │
│                                  │     │     └─► adventurer-stt-bench   │
│                                  │     │           (--worker, whisper-  │
│                                  │     │            medium on 4070)     │
│                                  │     │                                │
│                                  │     └─► /work/session/ (mirrored)    │
│                                  │                                      │
│                                  └─► Chrome --app=… --start-fullscreen  │
│                                       (2× DPI for 4K)                   │
│                                                                         │
│  cloudflared.service  ──tunnel──► localhost:3210                        │
│           │                                                             │
│           └─► adventurer.superterran.net (HTTPS, public)                │
│                                                                         │
│  iPad / phone (any LAN or WAN) ──scan QR──► /join (player view)         │
│           ├── reads /api/state, broadcasts WS                           │
│           └── 🎙 Tap to talk → 5s mp4/webm chunks → POST /api/voice     │
└─────────────────────────────────────────────────────────────────────────┘
```

## Repository layout

```
adventurer/
├── Cargo.toml                         (workspace root + workspace.dependencies)
├── Dockerfile                         (CPU + Vulkan via build arg)
├── Dockerfile.cuda                    (CUDA — what ships, what runs)
├── README.md                          (this file)
├── crates/
│   ├── inference-llm/                 lib — llama.cpp engine, no whisper
│   ├── inference-stt/                 lib — whisper.cpp engine, no llama
│   ├── server/                        bin "adventurer" — axum + workers + state
│   │   ├── src/
│   │   │   ├── main.rs                routes table, worker spawn order, signals
│   │   │   ├── api.rs                 REST handlers
│   │   │   ├── ws.rs                  WebSocket fanout
│   │   │   ├── state.rs               SessionData + broadcast::Sender
│   │   │   ├── workers.rs             Worker IPC manager (line-JSON over stdin/out)
│   │   │   ├── gemma.rs               two debounced LLM update loops
│   │   │   ├── lan.rs                 LAN-IP detection + QR SVG
│   │   │   ├── players.rs             token → character mapping
│   │   │   ├── config.rs              GitHub PAT/repo persistence (chmod 600)
│   │   │   ├── sync.rs                GitHub Trees API push
│   │   │   └── embed.rs               rust-embed of dnd-stage client + player UI
│   │   └── assets/
│   │       ├── client/                vendored from dnd-stage + augmentation
│   │       │   ├── index.html         (vendored, untouched)
│   │       │   ├── stage.js           (vendored, untouched)
│   │       │   ├── style.css          (vendored, untouched)
│   │       │   ├── qr-modal.js        Players + QR + Test Mode + Save-to-GH +
│   │       │   │                       Continue-from-GitHub modal
│   │       │   ├── gamepad.js         Steam Input KB+M nav + focus trap + debug
│   │       │   ├── transcript-style.js MutationObserver → ambient-line styling
│   │       │   └── dev-reload.js      WS subscriber → location.reload() on event
│   │       └── player/                mobile-friendly /join page
│   │           ├── index.html
│   │           ├── player.css
│   │           └── player.js          announce, ws subscribe, mic recording
│   ├── llm-bench/                     bin — Ollama A/B + LLM worker mode
│   └── stt-bench/                     bin — Speaches A/B + STT worker mode
├── assets/
│   └── steam/                         store-page artwork (sideload now,
│       ├── README.md                   Steamworks depot upload later)
│       └── screenshots/
├── prompts/state.txt                  verbatim STATE_PROMPT from gemma.py
├── samples/                           bench fixtures (transcript, audio clip)
├── models/                            gitignored — gemma + whisper GGUFs
├── .env.example                       launcher-sourced config template
├── .env                               gitignored, chmod 600 (real secrets)
└── scripts/
    ├── launch.sh                      Steam launcher (also deployed to ~/bin)
    ├── add-to-steam.py                shortcuts.vdf editor
    ├── adventurer.desktop             so Steam's "Add Non-Steam Game" finds it
    └── run.sh                         legacy bench wrapper
```

## Configuration (env vars / runtime)

All configurable via env, persisted (where applicable) to
`~/.local/share/adventurer/config.json` (chmod 600).

| Var                          | Default                              | What                                     |
| ---------------------------- | ------------------------------------ | ---------------------------------------- |
| `PORT`                       | `3210`                               | server listens here (was 3200, moved to coexist with legacy dnd-stage) |
| `ADVENTURER_PUBLIC_URL`      | `https://adventurer.superterran.net` | URL the QR encodes (HTTPS so iPad mic works) |
| `ADVENTURER_LAN_IP`          | auto-detect                          | host LAN IP (Docker can't see it)        |
| `ADVENTURER_HEADLESS`        | `0`                                  | `1` = no browser, just container + wait  |
| `ADVENTURER_DPI_SCALE`       | `2.0`                                | Chrome `--force-device-scale-factor`     |
| `ADVENTURER_GITHUB_PAT`      | from `~/.env` `$GITHUB_TOKEN`        | PAT for /api/session/save                |
| `ADVENTURER_GITHUB_REPO`     | `doughatcher/adventure-log`          | content repo to push to                  |
| `ADVENTURER_GITHUB_BRANCH`   | `main`                               |                                          |
| `LLM_MODEL`                  | `/models/gemma-4-E4B-it-Q4_K_M.gguf` | path inside container                    |
| `STT_MODEL`                  | `/models/ggml-medium.bin`            | path inside container                    |
| `LLM_GPU_LAYERS`             | `99`                                 | offload everything (CUDA build)          |
| `LLM_N_CTX`                  | `4096`                               | LLM context window                       |
| `STT_THREADS`                | `8`                                  | whisper CPU threads                      |
| `SESSION_DIR`                | `/work/session`                      | inside-container session dir             |

## Cloudflare Tunnel route (already provisioned)

`adventurer.superterran.net` → `http://localhost:3210` via the `desktop`
tunnel (id `6d22ac69-0fba-4cf6-9e8a-764f0e4f212a`). Three pieces, all
configured via the Cloudflare API using `CF_AUTH_EMAIL` + `CF_AUTH_KEY`
from `~/.env`:

1. Tunnel ingress rule (PUT `/accounts/{acct}/cfd_tunnel/{id}/configurations`)
2. CNAME `adventurer.superterran.net` → `<tunnel>.cfargotunnel.com` (proxied)
3. http_request_dynamic_redirect ruleset — added `adventurer.superterran.net`
   to the allowlist so the catch-all "redirect everything else to
   doughatcher.com" rule doesn't eat it

If we ever need to reprovision: see commit history for the python
one-shot that hit all three endpoints.

## Steam non-Steam shortcut

Currently in `~/.local/share/Steam/userdata/35894255/config/shortcuts.vdf`
under whatever name it was last added with (originally "Adventure Log";
to rename to **Adventurer**, fully exit Steam — System tray → Steam → Exit
— and run:

```bash
python3 ~/repos/adventurer/scripts/add-to-steam.py --name "Adventurer"
```

The script aborts if Steam is running so you can't stomp the in-memory
shortcuts.vdf (Steam rewrites it on quit and would overwrite your edit).
It also dedupes by both name AND exe path, so renaming via `--name`
cleanly replaces the old entry instead of duplicating it.

The same script will eventually grow `--install-art` to copy the files in
`assets/steam/` into the per-shortcut grid art location — see
`assets/steam/README.md` for the dimensions and the future Steamworks
upload story.

The script computes the same CRC32-based appid Steam itself uses, dedupes
by name OR exe path (so renames cleanly replace), and writes a backup at
`shortcuts.vdf.bak`.

For Steam Big Picture / Game Mode controller setup: this needs **per-game
controller layout** (Steam button → Controller Settings → Edit Layout →
"Keyboard and Mouse" template) — the global desktop config doesn't apply
to non-Steam games. With KB+M template Steam sends:
- D-pad → WASD
- A button → Space
- B button → Escape
- Right trackpad / stick → mouse cursor

`gamepad.js` handles all of these (arrow keys + WASD + Space + Enter all map
to focus nav / click).

## Diagnostics

The DM stage UI has a **discreet 🔍 chip bottom-right** that toggles a live
input-debug overlay (key / mouse / pad / device count). Same data POSTs
to `/api/debug/input` so it's also visible in `docker logs adventurer-live`
under the `adventurer::input_debug` target — useful for diagnosing controller
issues without reading TV-text.

Visit `?debug=1` to show the overlay on load. Backtick (`` ` ``) toggles it.

`tower_http::trace::TraceLayer` is on the router so every HTTP request logs
method + path + status + latency.

## Test Mode

Start screen has a **🧪 Start in Test Mode** button. It flips the session
into ephemeral mode:
- A green pill at bottom-center reminds: "TEST MODE — nothing is being saved"
- `POST /api/session/save` refuses with `error: "session is in test mode"`
- Auto-opens the QR modal (the main reason to use test mode is iPad mic
  testing without polluting the GitHub content repo)

Flip via API: `POST /api/session/mode {mode: "live" | "test"}`.

## Continuing an existing adventure-log session

The DM stage start screen has a **▶ Continue from GitHub** button. It hits
`GET /api/adventure-log/sessions` (which calls the GitHub Trees API to list
session directories under `ADVENTURER_GITHUB_REPO`), shows a dropdown of
session ids newest-first, and on pick POSTs to `/api/session/load` to pull
the chosen session's `transcript.md` + `state.json` + `panels/*.md` and
swap them into the live session.

Continued sessions inherit the loaded session's `session_id` so subsequent
saves overwrite the same archive folder — meaning you can `Save now` after
a continue and the GitHub copy gets the new lines appended.

REST surface:
```bash
# list available sessions (newest first)
curl http://127.0.0.1:3210/api/adventure-log/sessions

# load one
curl -X POST http://127.0.0.1:3210/api/session/load \
    -H 'content-type: application/json' \
    -d '{"session_id":"2026-04-20-2138"}'

# what session am I currently in?
curl http://127.0.0.1:3210/api/session
```

## Never-delete archive policy

Game tomorrow / live session today is the explicit driver here: **we do not
delete or skip any audio capture, ever.** The voice handler:

1. Atomically writes the incoming audio blob to
   `${SESSION_DIR}/audio/chunk-{seq:06}.{ext}` **before** doing anything else.
   If STT crashes or the LLM worker hangs, the audio file is still on disk.
2. Asks the STT worker to transcribe the path (no copy, no delete).
3. Appends one line to `${SESSION_DIR}/raw-events.jsonl` *regardless of STT
   outcome* with `{ts, seq, audio_path, status, transcript?, error?}`.
   `status` is one of `ok`, `skipped` (whisper hallucination), `stt_error`.

Result: the on-disk archive is the source of truth and is independently
re-transcribable later. Even crash-recovery (`SessionData::read_existing`
on startup) reads the highest existing chunk number off disk so a restart
in the middle of a session resumes the chunk sequence and doesn't risk
overwriting prior audio.

The only thing the transcript filter actively drops is whisper's three
sentinel hallucinations (`[BLANK_AUDIO]` / `[silence]` / `[inaudible]`).
Real ambient captures like `[paper rustling]`, `[music playing]`,
`[chair creaks]` are kept and rendered with the 🔊 ambient style in the
DM panel.

## Live reload (dev mode)

Set `ADVENTURER_DEV=1` and the launcher bind-mounts `crates/server/assets/`
into the container at `/work/dev-assets`, sets `ADVENTURER_DEV_ASSETS` so
the embed shim reads from disk first (with rust-embed as fallback), and
spawns a `notify::PollWatcher` (NOT inotify — inotify events don't cross
docker bind mounts) that broadcasts a `dev_reload` WS event on every
filesystem change.

`dev-reload.js` (injected into both the DM index and the player index)
subscribes to `/ws?role=dev-reload`, displays a tiny "↻ asset changed —
reloading" pill on event, and `location.reload()`s — so iPad / phone /
laptop / DM screen all reload together when you save a file in your
editor on the host.

```bash
# Edit assets/client/style.css → save → DM stage reloads in <1s
ADVENTURER_DEV=1 ~/bin/adventurer-launch.sh
```

Only HTML / CSS / JS changes are live. Rust changes still need a
`docker build -f Dockerfile.cuda -t adventurer:cuda .`.

## Configuration via .env

The launcher sources two files in order so a project-specific override
beats global defaults:

1. `~/.env`             (machine-wide secrets, including `GITHUB_TOKEN`)
2. `~/repos/adventurer/.env`  (project — wins on collision)

Use `.env.example` (committed) as the template. Real secrets land in
`.env` (gitignored, chmod 600). Currently honored vars:

```bash
ADVENTURER_GITHUB_PAT=ghp_…                         # falls back to $GITHUB_TOKEN
ADVENTURER_GITHUB_REPO=doughatcher/adventure-log
ADVENTURER_GITHUB_BRANCH=main
ADVENTURER_PUBLIC_URL=https://adventurer.superterran.net
ADVENTURER_PORT=3210
ADVENTURER_DPI_SCALE=2.0
ADVENTURER_HEADLESS=1
ADVENTURER_DEV=1
ADVENTURER_IMAGE=adventurer:cuda
ADVENTURER_SESSION=$HOME/.local/share/adventurer/session
```

## Known quirks

- **iPad Safari requires HTTPS** for `getUserMedia()`. The QR encodes the
  Cloudflare-tunnel HTTPS URL specifically for this. Plain LAN HTTP will
  show a clear error in the player view.
- **Steam Input "Gamepad" template sends NOTHING to the focused window** —
  pick "Keyboard and Mouse" template instead.
- **`docker stop` defaults to 10s graceful timeout** — feels slow for "Stop
  game". Launcher uses `docker kill` + `pkill` Chrome by `--user-data-dir`
  for instant exit.
- **Chrome profile is wiped each launch** so DPI scale always applies (Chrome
  caches per-domain zoom in profile data).
- **CUDA workers must spawn serially** — concurrent CUDA init on the same GPU
  deadlocks.
- **Don't link both inference engines into one binary** — vendored ggml
  duplicate-symbol issue produces a runtime crash. They MUST be separate
  processes. `--allow-multiple-definition` "works" at link time but the LLM
  silently calls into whisper's ggml ABI and SEGV's.
- **inotify doesn't cross docker bind-mounts**, so the dev-reload watcher
  must use `notify::PollWatcher` (poll interval 750 ms). `RecommendedWatcher`
  silently installs an inotify hook that never fires for host-side edits.
- **Steam shortcuts.vdf is cached in memory while Steam runs** — the
  add-to-steam script refuses to write while Steam is up to avoid the
  shutdown-overwrite footgun.

---

## Original PoC bench results (kept for history)

The repo started as a pair of A/B benches before becoming a server. The
original validation runs:

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

## Launch as a (non-)Steam game

`scripts/launch.sh` boots the Docker container, waits for `/health`, and opens
a fullscreen Chrome (`--app=` mode, no chrome) pointed at `localhost:3200`.
When the browser closes, the container is stopped via `trap`. Logs go to
`~/.local/state/adventurer/launch.log`.

### One-time install

```bash
# 1. Install the launcher to ~/bin/
cp scripts/launch.sh ~/bin/adventurer-launch.sh
chmod +x ~/bin/adventurer-launch.sh

# 2. Drop the .desktop file (lets Steam's "Add a Non-Steam Game" find it)
cp scripts/adventurer.desktop ~/.local/share/applications/
```

### Add to Steam

Two paths — the second is fully automated but needs Steam to be exited.

**A) Through the Steam UI (no Steam restart needed):**

1. Switch to Steam Desktop mode (Steam button → Power → Switch to Desktop)
2. In the Steam library: **+ Add a Game** → **Add a Non-Steam Game**
3. Pick **Adventure Log** from the list (it appears because of the `.desktop` file)
4. Done. Launches via the script when you hit Play.

**B) Programmatically writing `shortcuts.vdf`:**

```bash
# Quit Steam fully first (System tray → Steam → Exit), then:
python3 scripts/add-to-steam.py
# Restart Steam — Adventure Log is in your library.
```

The script writes a backup to `shortcuts.vdf.bak` before editing. Idempotent —
safe to re-run; it replaces any existing entry with the same name.

### What the launcher does

1. Detects host LAN IP (`hostname -I`) and passes it via `ADVENTURER_LAN_IP` so
   the QR code embeds the right URL (Docker can't see the host's LAN IP from
   inside).
2. Mounts `~/.local/share/adventurer/session/` for persistent session state.
3. Starts the container detached, waits up to 120s for `/health`.
4. Launches Chrome (Flatpak preferred, then native Chrome, then Firefox kiosk,
   then `xdg-open`) in app/fullscreen mode on the URL.
5. Foreground-waits on the browser PID so Steam tracks playtime correctly.

### Backup to GitHub (the same `adventure-log` content repo)

`adventurer` writes session state to a configured GitHub repo via the REST
Trees API — atomic single-commit per save, no `git` binary required at
runtime. Same on-disk layout dnd-stage produces, so the existing GitHub
Action that generates the Hugo journal sees the new sessions and processes
them automatically.

**Configure** once via env vars (the launcher passes these through):

```bash
export ADVENTURER_GITHUB_PAT=<PAT_with_contents:write>
export ADVENTURER_GITHUB_REPO=doughatcher/adventure-log
export ADVENTURER_GITHUB_BRANCH=main      # default: main
```

…or write them through the **Players** modal's "Backup to GitHub" panel
(persists to `~/.local/share/adventurer/config.json`, chmod 600).

**Save the current session:**

```bash
curl -X POST http://localhost:3200/api/session/save \
    -H 'Content-Type: application/json' -d '{}'
# {"ok":true,"commit_sha":"…","commit_url":"https://github.com/…/commit/…","files":7}
```

…or click **⤴ Save session now** in the modal.

What gets written:

```
data/sessions/<id>/
├── transcript.md        (the running transcript)
├── state.json           (gemma's structured party/combat state)
├── scene.md
├── story-log.md
├── party.md
├── next-steps.md
└── map.md
```

`<id>` defaults to `YYYY-MM-DD-HHMM`; pass `session_id` in the JSON to override.
The PAT is never echoed back from `/api/config` (returns `has_pat: bool`).

### Players join via QR code

Click **♣ Players** in the DM stage header — the modal shows a QR encoding
`http://<lan-ip>:3200/join`. Phones scan, get a stripped mobile UI showing:
- Their assigned character (HP bar, conditions, notes)
- The current scene + party + transcript tail
- The "Decision" modal when the AI surfaces an active choice

Player → character assignment lives in the same modal: each connected device
shows up as a row with a dropdown of party characters. Pick one and that
device's view fills in.

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

## Layout (Cargo workspace)

```
adventurer/
├── Cargo.toml                       (workspace root + workspace.dependencies)
├── Dockerfile                       (CPU + Vulkan via build arg)
├── Dockerfile.cuda                  (CUDA — separate base image, builds 3 bins)
├── README.md                        (this file)
├── crates/
│   ├── inference-llm/               (lib: llama.cpp engine — links llama-cpp-sys-2 only)
│   ├── inference-stt/               (lib: whisper.cpp engine — links whisper-rs-sys only)
│   ├── server/                      (bin: adventurer — axum, no inference deps)
│   ├── llm-bench/                   (bin: adventurer-llm-bench — Ollama A/B)
│   └── stt-bench/                   (bin: adventurer-stt-bench — Speaches A/B)
├── prompts/
│   └── state.txt                    (verbatim STATE_PROMPT from dnd-stage/server/gemma.py)
├── samples/
│   ├── party.md                     (3 PCs with HP/AC/class)
│   ├── transcript.md                (combat encounter fixture)
│   └── audio/clip.mp3               (30s slice from a real archived dnd-stage session)
├── models/                          (gitignored — Gemma 4 GGUFs + ggml-medium.bin)
└── scripts/
    └── run.sh                       (Docker wrapper: picks image + GPU + host network)
```

**Why two inference crates and not one:** llama.cpp and whisper.cpp each vendor
their own static copy of `ggml`. Linking both into the same binary produces
~hundreds of duplicate `ggml_backend_*` symbols and `--allow-multiple-definition`
silently picks one and crashes the other at runtime. The architecture instead
splits inference across separate worker processes; the server spawns them and
talks via stdin/stdout (Day 2).
