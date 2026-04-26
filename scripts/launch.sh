#!/usr/bin/env bash
# adventurer-launch.sh — start the adventurer Docker container, wait for it
# to be ready, open a fullscreen browser pointed at it, then stop the
# container when the browser closes.
#
# Designed to be added to Steam as a non-Steam game:
#   Steam → Library → "+ Add a Game" → "Add a Non-Steam Game" → Browse
#   → /home/me/bin/adventurer-launch.sh
#
# Steam launches scripts from a clean environment that doesn't inherit the
# user's shell rc files, and the Steam Runtime narrows PATH. Defensive
# patterns below:
#
#   - `set -uo pipefail` (no -e: combined with `[[ ]] && cmd` patterns -e
#     silently exits when a test is false; that's the bug that made the very
#     first launch attempt look like an instant crash)
#   - Hard-set PATH so docker/curl/hostname/awk/flatpak resolve
#   - `exec >> $LOGFILE 2>&1` from the top so even the early failures are
#     captured (~/.local/state/adventurer/launch.log)
#   - Source ~/.env so GITHUB_TOKEN etc. are present
#   - Explicit `command -v docker` check with an actionable error

set -uo pipefail

PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:${PATH:-}"
export PATH

LOGFILE="${ADVENTURER_LOG:-${XDG_STATE_HOME:-${HOME}/.local/state}/adventurer/launch.log}"
mkdir -p "$(dirname "$LOGFILE")"
# Redirect ALL output to the log from this line on. Steam-launched processes
# usually have no terminal — without this, errors vanish.
exec >>"$LOGFILE" 2>&1
echo
echo "========================================================================"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] adventurer-launch starting (pid $$)"
echo "  USER=${USER:-?}  HOME=${HOME:-?}  PWD=$(pwd)"
echo "  PATH=$PATH"

# Source credential / config env files. Repo .env wins over ~/.env so a
# project-specific override beats global defaults.
for envfile in "${HOME}/.env" "${HOME}/repos/adventurer/.env"; do
    if [[ -f "$envfile" ]]; then
        set -a
        # shellcheck disable=SC1091
        source "$envfile" || echo "WARN: $envfile source failed"
        set +a
        echo "  sourced $envfile"
    fi
done

# Sanity-check critical commands. Bail loudly with the docker install
# command if missing, so the log is self-explanatory.
if ! command -v docker >/dev/null 2>&1; then
    echo "FATAL: docker not found in PATH ($PATH)."
    echo "  Install: https://docs.docker.com/engine/install/  (or rpm-ostree install moby-engine on Bazzite)"
    exit 2
fi

IMAGE="${ADVENTURER_IMAGE:-adventurer:cuda}"
NAME="${ADVENTURER_CONTAINER:-adventurer-live}"
# 3210 not 3200 — leaves the legacy dnd-stage uvicorn on 3200 alone while
# we co-exist. Override with ADVENTURER_PORT=… for any other value.
PORT="${ADVENTURER_PORT:-3210}"
MODELS="${ADVENTURER_MODELS:-/var/home/me/repos/adventurer/models}"
SESSION="${ADVENTURER_SESSION:-${HOME}/.local/share/adventurer/session}"
# Unique Chrome profile dir, defined early so the cleanup trap can pkill by it.
CHROME_PROFILE="${HOME}/.cache/adventurer/chrome-profile"
mkdir -p "$SESSION" "$CHROME_PROFILE"

# Confirm the image exists locally — Steam-launched processes can't pull from
# a registry without auth or network setup, so a missing image is fatal.
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "FATAL: docker image '$IMAGE' not found locally."
    echo "  Build with: cd ~/repos/adventurer && DOCKER_BUILDKIT=1 docker build -f Dockerfile.cuda -t $IMAGE ."
    exit 3
fi

# Best-effort LAN IP for the QR (Docker can't see the host LAN from inside).
LAN_IP=""
if command -v hostname >/dev/null; then
    LAN_IP="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
fi
if [[ -z "$LAN_IP" ]] && command -v ip >/dev/null; then
    LAN_IP="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") print $(i+1)}' || true)"
fi
LAN_IP="${LAN_IP:-127.0.0.1}"
echo "  LAN_IP=$LAN_IP  PORT=$PORT  IMAGE=$IMAGE  SESSION=$SESSION"

# Public URL the QR code encodes. iPad Safari refuses getUserMedia() over plain
# HTTP (LAN), so we point the QR at the Cloudflare-Tunnel-fronted HTTPS hostname
# when one's available. Default matches the tunnel rule we provisioned —
# override via ADVENTURER_PUBLIC_URL=… or unset to fall back to the LAN URL.
PUBLIC_URL="${ADVENTURER_PUBLIC_URL:-https://adventurer.superterran.net}"
echo "  PUBLIC_URL=$PUBLIC_URL"

cleanup() {
    echo "[$(date '+%H:%M:%S')] cleanup: SIGKILL container + kill Chrome window"
    # 1. Kill the Chrome window we opened, matched by our unique --user-data-dir
    #    (Steam's "Exiting Game..." overlay waits for the entire process tree
    #    so we have to take Chrome down too — otherwise the launcher exits but
    #    Chrome lingers and Steam keeps showing "Exiting Game...".)
    pkill -f -- "--user-data-dir=${CHROME_PROFILE:-/dev/null/no-match}" 2>/dev/null || true
    # 2. SIGKILL the container — we don't need graceful flush; session state
    #    is broadcast live and (if configured) already pushed to GitHub.
    #    docker kill is instant; docker stop -t 2 was a 2-second hold.
    docker kill "${NAME}" 2>/dev/null || true
    docker rm -f  "${NAME}" >/dev/null 2>&1 || true
    echo "[$(date '+%H:%M:%S')] cleanup done"
}
trap cleanup EXIT INT TERM

# ─── 1. ensure no prior container is running ───
docker rm -f "${NAME}" >/dev/null 2>&1 || true

# ─── 2. assemble env flags WITHOUT the [[ ]] && pattern (set -e safe) ───
GH_ENV_FLAGS=()
if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    GH_ENV_FLAGS+=(-e "ADVENTURER_GITHUB_PAT=${GITHUB_TOKEN}")
fi
if [[ -n "${ADVENTURER_GITHUB_PAT:-}" ]]; then
    GH_ENV_FLAGS+=(-e "ADVENTURER_GITHUB_PAT=${ADVENTURER_GITHUB_PAT}")
fi
if [[ -n "${ADVENTURER_GITHUB_REPO:-}" ]]; then
    GH_ENV_FLAGS+=(-e "ADVENTURER_GITHUB_REPO=${ADVENTURER_GITHUB_REPO}")
else
    GH_ENV_FLAGS+=(-e "ADVENTURER_GITHUB_REPO=doughatcher/adventure-log")
fi
if [[ -n "${ADVENTURER_GITHUB_BRANCH:-}" ]]; then
    GH_ENV_FLAGS+=(-e "ADVENTURER_GITHUB_BRANCH=${ADVENTURER_GITHUB_BRANCH}")
fi
echo "  github env flags: ${#GH_ENV_FLAGS[@]} entries"

# ─── 3. start the container ───
# Run server inside the container on the SAME port we expose, so the QR-encoded
# URL (which uses the server's --port) matches what's reachable from the LAN.
# DEV MODE: ADVENTURER_DEV=1 mounts the host's assets dir into the container
# at /work/dev-assets and points the server at it. UI changes (HTML/CSS/JS)
# show up on a browser reload — no docker rebuild needed. Only Rust changes
# require a rebuild.
DEV_FLAGS=()
if [[ "${ADVENTURER_DEV:-0}" == "1" ]]; then
    DEV_ASSETS_HOST="${ADVENTURER_DEV_ASSETS_HOST:-/var/home/me/repos/adventurer/crates/server/assets}"
    if [[ -d "$DEV_ASSETS_HOST" ]]; then
        DEV_FLAGS+=(-v "${DEV_ASSETS_HOST}:/work/dev-assets:ro"
                    -e "ADVENTURER_DEV_ASSETS=/work/dev-assets")
        echo "  DEV mode: live-reloading assets from ${DEV_ASSETS_HOST}"
    else
        echo "  DEV mode requested but ${DEV_ASSETS_HOST} not found — skipping"
    fi
fi

echo "[$(date '+%H:%M:%S')] docker run ${IMAGE} (port ${PORT})"
CONTAINER_ID=$(docker run -d --name "${NAME}" \
    --device nvidia.com/gpu=all \
    -p "${PORT}:${PORT}" \
    -e "PORT=${PORT}" \
    -e "ADVENTURER_LAN_IP=${LAN_IP}" \
    -e "ADVENTURER_PUBLIC_URL=${PUBLIC_URL}" \
    "${GH_ENV_FLAGS[@]}" \
    "${DEV_FLAGS[@]}" \
    -v "${MODELS}:/models:ro" \
    -v "${SESSION}:/work/session" \
    "${IMAGE}" 2>&1)
DOCKER_RC=$?
if [[ $DOCKER_RC -ne 0 ]]; then
    echo "FATAL: docker run failed (rc=$DOCKER_RC): $CONTAINER_ID"
    exit 4
fi
echo "  container: ${CONTAINER_ID:0:12}"

# ─── 4. wait for /health ───
echo "[$(date '+%H:%M:%S')] waiting for server to come up…"
for i in $(seq 1 120); do
    if curl -sf "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
        echo "  server ready after ${i}s"
        break
    fi
    sleep 1
    if [[ $i -eq 120 ]]; then
        echo "ERROR: server did not become ready in 120s"
        docker logs --tail 50 "${NAME}" 2>&1 || true
        exit 5
    fi
done

URL="http://127.0.0.1:${PORT}/"

# Headless mode: skip the browser launch and just block on the container.
# Useful for remote/SSH starts ("just bring the server up, I'll point at it
# from a real browser somewhere else") or for CI smoke tests. Set
# ADVENTURER_HEADLESS=1 to enable.
if [[ "${ADVENTURER_HEADLESS:-0}" == "1" ]]; then
    echo "[$(date '+%H:%M:%S')] HEADLESS mode — no browser, server is at ${URL}"
    echo "[$(date '+%H:%M:%S')] (or via tunnel: ${PUBLIC_URL})"
    echo "[$(date '+%H:%M:%S')] waiting on container; SIGTERM/Ctrl-C to stop"
    docker wait "${NAME}" >/dev/null 2>&1 || true
    echo "[$(date '+%H:%M:%S')] container exited; launcher done"
    exit 0
fi

echo "[$(date '+%H:%M:%S')] opening browser → ${URL}"

# ─── 5. open browser fullscreen ───
# We use a dedicated user-data-dir so Chrome always opens a NEW process tree
# instead of attaching to an existing Chrome and detaching us instantly. This
# was the original "exits immediately" bug — flatpak Chrome with no
# --user-data-dir reuses the running session and our `wait` returned at once.
# DPI scaling. The vendored dnd-stage UI was sized for 1080p-ish desktop;
# at 4K 100% scale every button is tiny. Force-scale Chrome's rendering.
# 2.0 = "200%". Override via env (e.g. ADVENTURER_DPI_SCALE=1.5 = "150%",
# =1 to disable).
DPI_SCALE="${ADVENTURER_DPI_SCALE:-2.0}"

# Wipe the Chrome profile each launch so the device-scale-factor and zoom
# settings start fresh. Chrome remembers per-domain zoom in the profile, and
# a previous launch with --force-device-scale-factor=1 silently overrides
# subsequent launches that change the value. Also stops Chrome from prompting
# about restoring tabs / closed windows.
rm -rf "$CHROME_PROFILE"
mkdir -p "$CHROME_PROFILE"

CHROME_FLAGS=(
    --app="${URL}"
    --user-data-dir="${CHROME_PROFILE}"
    --start-fullscreen
    --new-window
    --no-first-run
    --no-default-browser-check
    --disable-features=TranslateUI
    --force-device-scale-factor="${DPI_SCALE}"
    --high-dpi-support=1
)
echo "  chrome flags: ${CHROME_FLAGS[*]}"

if command -v flatpak >/dev/null && flatpak info com.google.Chrome >/dev/null 2>&1; then
    echo "  launching Chrome (flatpak)"
    flatpak run com.google.Chrome "${CHROME_FLAGS[@]}" &
elif command -v google-chrome >/dev/null; then
    echo "  launching Chrome (native)"
    google-chrome "${CHROME_FLAGS[@]}" &
elif command -v firefox >/dev/null; then
    echo "  launching Firefox kiosk"
    firefox --kiosk "${URL}" &
elif command -v xdg-open >/dev/null; then
    echo "  launching default browser via xdg-open"
    xdg-open "${URL}" &
else
    echo "WARN: no browser found — UI is at $URL"
fi

# ─── 6. wait for the CONTAINER, not the browser ───
# The browser process may be unreliable to wait on (flatpak detach, single-
# instance reuse, gamescope quirks). The container is what defines the
# session lifetime: it runs until our trap or Steam's "Stop game" stops it.
echo "[$(date '+%H:%M:%S')] waiting on container (Steam → Stop game to exit)"
docker wait "${NAME}" >/dev/null 2>&1 || true
echo "[$(date '+%H:%M:%S')] container exited; launcher done"
