#!/usr/bin/env bash
# adventurer-launch.sh — start the adventurer Docker container, wait for it
# to be ready, open a fullscreen browser pointed at it, then stop the
# container when the browser closes.
#
# Designed to be added to Steam as a non-Steam game:
#   Steam → Library → "+ Add a Game" → "Add a Non-Steam Game" → Browse
#   → /home/me/bin/adventurer-launch.sh
#
# The script intentionally stays foreground for the duration of the session so
# Steam's playtime tracker / "Stop game" button reflects what's actually running.

set -euo pipefail

IMAGE="${ADVENTURER_IMAGE:-adventurer:cuda}"
NAME="${ADVENTURER_CONTAINER:-adventurer-live}"
PORT="${ADVENTURER_PORT:-3200}"
MODELS="${ADVENTURER_MODELS:-/var/home/me/repos/adventurer/models}"
SESSION="${ADVENTURER_SESSION:-${HOME}/.local/share/adventurer/session}"
LOGFILE="${ADVENTURER_LOG:-${XDG_STATE_HOME:-${HOME}/.local/state}/adventurer/launch.log}"
mkdir -p "$(dirname "$LOGFILE")" "$SESSION"

# Best-effort LAN IP for the QR code (the container can't see this from inside).
LAN_IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
if [[ -z "$LAN_IP" ]]; then
    LAN_IP="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") print $(i+1)}')"
fi
LAN_IP="${LAN_IP:-127.0.0.1}"

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOGFILE" >&2; }

cleanup() {
    log "stopping container ${NAME}"
    docker stop "${NAME}" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

# ─── 1. ensure no prior container is running ───
docker rm -f "${NAME}" >/dev/null 2>&1 || true

# ─── 2. start the container ───
log "starting ${IMAGE} (lan=${LAN_IP}, port=${PORT})"
docker run -d --name "${NAME}" \
    --device nvidia.com/gpu=all \
    -p "${PORT}:3200" \
    -e "ADVENTURER_LAN_IP=${LAN_IP}" \
    -v "${MODELS}:/models:ro" \
    -v "${SESSION}:/work/session" \
    "${IMAGE}" >> "$LOGFILE"

# ─── 3. wait for /health ───
log "waiting for server to come up…"
for i in {1..120}; do
    if curl -sf "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
        log "server ready (after ${i}s)"
        break
    fi
    sleep 1
    if [[ $i -eq 120 ]]; then
        log "ERROR: server did not become ready in 120s"
        docker logs --tail 30 "${NAME}" >> "$LOGFILE" 2>&1 || true
        exit 1
    fi
done

URL="http://127.0.0.1:${PORT}/"
log "opening browser → ${URL}"

# ─── 4. open browser, prefer kiosk/app modes for fullscreen ───
# Bazzite ships Chrome as a Flatpak (com.google.Chrome). Use --app= for an
# app-window without browser chrome — better for the "boot into the game" feel
# than a regular tab. Falls back to xdg-open for whatever the user has set.
BROWSER_PID=""
if command -v flatpak >/dev/null && flatpak info com.google.Chrome >/dev/null 2>&1; then
    flatpak run com.google.Chrome --app="${URL}" --start-fullscreen >>"$LOGFILE" 2>&1 &
    BROWSER_PID=$!
    log "launched Chrome (flatpak) PID=${BROWSER_PID}"
elif command -v google-chrome >/dev/null; then
    google-chrome --app="${URL}" --start-fullscreen >>"$LOGFILE" 2>&1 &
    BROWSER_PID=$!
elif command -v firefox >/dev/null; then
    firefox --kiosk "${URL}" >>"$LOGFILE" 2>&1 &
    BROWSER_PID=$!
else
    xdg-open "${URL}" >>"$LOGFILE" 2>&1 &
    BROWSER_PID=$!
fi

# ─── 5. wait for the browser to close, then trap cleanup runs ───
wait "${BROWSER_PID}" 2>/dev/null || true
log "browser exited; container will be stopped by trap"
