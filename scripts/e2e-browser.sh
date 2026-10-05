#!/usr/bin/env bash
# Browser end-to-end test for a Szalinski Docker image.
#
#   scripts/e2e-browser.sh <image>        e.g. scripts/e2e-browser.sh szalinski:dev
#
# Starts the image with a fresh /config and a small synthetic library at
# /media (made with the image's own ffmpeg), then runs web/e2e/smoke.mjs
# against it: a real Chromium goes through the first-run screens (welcome,
# folder, goal), waits for a file to be converted and verified, opens its
# verification report and the Settings > Hardware page, and fails on any
# console error or server error.
#
# Needs Playwright with Chromium: installed in the project (web/) or globally
# (`npm install -g playwright && playwright install chromium`).
#
# Environment (all optional):
#   E2E_GOAL          goal to pick: save_space, balanced, compatible or archive
#                     (default compatible, the quickest on a CPU)
#   E2E_TIMEOUT       seconds the conversions may take (default 300)
#   E2E_PORT          host port on 127.0.0.1 for the web UI (default: a free
#                     port chosen by Docker)
#   E2E_SCREENSHOTS   folder to save a screenshot of each step in
#   E2E_KEEP=1        keep the container and work folder for inspection
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
IMAGE=${1:-}
GOAL=${E2E_GOAL:-compatible}
TIMEOUT=${E2E_TIMEOUT:-300}

log() { printf '\033[1m[e2e-browser]\033[0m %s\n' "$*"; }
fail() {
    printf '\033[31m[e2e-browser] FAIL:\033[0m %s\n' "$*" >&2
    exit 1
}

if [ -z "$IMAGE" ]; then
    echo "Usage: $0 <image>" >&2
    exit 2
fi
for tool in docker node; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required."
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/szalinski-e2e-browser.XXXXXX")
MEDIA="$WORK/media"
CONFIG="$WORK/config"
CONTAINER="szalinski-e2e-browser-$$"
STARTED=0
mkdir -p "$MEDIA" "$CONFIG"

cleanup() {
    local status=$?
    if [ "$STARTED" = 1 ]; then
        if [ "$status" -ne 0 ]; then
            echo "----- container log (last 100 lines) -----" >&2
            docker logs --tail 100 "$CONTAINER" >&2 2>&1 || true
            echo "------------------------------------------" >&2
        fi
        if [ "${E2E_KEEP:-0}" != 1 ]; then
            docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
        fi
    fi
    if [ "${E2E_KEEP:-0}" = 1 ]; then
        log "Keeping $WORK (container $CONTAINER)"
    elif ! rm -rf "$WORK" 2>/dev/null; then
        # Files written by the container may belong to another uid.
        docker run --rm --entrypoint sh -v "$WORK:/work" "$IMAGE" -c 'rm -rf /work/* /work/.[!.]*' >/dev/null 2>&1 || true
        rm -rf "$WORK" 2>/dev/null || log "Could not remove $WORK"
    fi
    exit "$status"
}
trap cleanup EXIT

# Run as the host user so the work folder stays removable; as root, use 1000
# so the privilege drop is exercised too.
RUN_UID=$(id -u)
RUN_GID=$(id -g)
if [ "$RUN_UID" = 0 ]; then
    RUN_UID=1000
    RUN_GID=1000
fi

log "Creating test media in $MEDIA"
docker run --rm --user "$(id -u):$(id -g)" --entrypoint sh \
    -v "$SCRIPT_DIR:/scripts:ro" -v "$MEDIA:/out" \
    "$IMAGE" /scripts/make-test-media.sh /out 8 >"$WORK/make-media.log" 2>&1 ||
    fail "Could not create test media with the image's ffmpeg: $(tail -n 20 "$WORK/make-media.log")"
# Whatever uid the app runs as must be able to replace these files.
chmod -R a+rwX "$MEDIA"

log "Starting $IMAGE as uid $RUN_UID, gid $RUN_GID"
docker run -d --name "$CONTAINER" \
    -p "127.0.0.1:${E2E_PORT:-}:8080" \
    -v "$MEDIA:/media" -v "$CONFIG:/config" \
    -e "PUID=$RUN_UID" -e "PGID=$RUN_GID" -e TZ=UTC \
    "$IMAGE" >/dev/null
STARTED=1
port=$(docker port "$CONTAINER" 8080/tcp 2>/dev/null | head -n 1 | sed 's/.*://')
[ -n "$port" ] || fail "Docker did not publish port 8080."
URL="http://127.0.0.1:$port"

args=("$URL" --folder /media --goal "$GOAL" --timeout "$TIMEOUT")
if [ -n "${E2E_SCREENSHOTS:-}" ]; then
    args+=(--screenshots "$E2E_SCREENSHOTS")
fi
log "Running web/e2e/smoke.mjs against $URL"
node "$ROOT_DIR/web/e2e/smoke.mjs" "${args[@]}"
log "PASS"
