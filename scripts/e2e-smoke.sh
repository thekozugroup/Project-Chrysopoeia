#!/usr/bin/env bash
# End-to-end smoke test for a Szalinski Docker image.
#
#   scripts/e2e-smoke.sh <image>          e.g. scripts/e2e-smoke.sh szalinski:dev
#
# Generates a small synthetic library (scripts/make-test-media.sh, run with the
# image's own ffmpeg), starts the image with it mounted at /media and an empty
# /config, adds /media as a library with the "Plays everywhere" goal (H.264 +
# AAC in MP4: the quickest goal on a CPU), waits until every file is in the
# library (freshly written files are first left alone for the server's settle
# time, as if still being copied) and nothing is queued or running, then
# checks through the API and on disk that:
#   - the H.264 MP4, the AVI and every other real video ended done or skipped,
#     never failed (only the deliberately broken files may fail),
#   - the audio-only FLAC was skipped,
#   - every done job carries a passing verification report,
#   - done files exist on disk and really contain the target codec,
#   - no original was lost and no temporary/backup file was left behind,
#   - the container is still healthy.
#
# Environment (all optional):
#   E2E_TIMEOUT        seconds to wait for processing (default 900)
#   E2E_SETTLE         seconds the queue must stay idle before checking, so
#                      folder-watch events for new outputs settle (default 20)
#   E2E_MEDIA_SECONDS  length of each test clip in seconds (default 4)
#   E2E_GOAL           goal for the library (default compatible)
#   E2E_HW_ACCEL       HW_ACCEL for the container (default auto)
#   E2E_MAX_JOBS       MAX_JOBS for the container (default: automatic)
#   E2E_PORT           host port (on 127.0.0.1) for the container's web UI
#                      (default: a free port chosen by Docker)
#   E2E_KEEP=1         keep the container and work folder for inspection
#   E2E_URL            test a server that is already running at this base URL
#                      (e.g. http://127.0.0.1:8080) instead of starting the
#                      image. Media is then generated with the host's ffmpeg
#                      and added by its host path; container checks are skipped.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
IMAGE=${1:-}
TIMEOUT=${E2E_TIMEOUT:-900}
SETTLE=${E2E_SETTLE:-20}
CLIP_SECONDS=${E2E_MEDIA_SECONDS:-4}
GOAL=${E2E_GOAL:-compatible}
URL_MODE=0
[ -n "${E2E_URL:-}" ] && URL_MODE=1

log() { printf '\033[1m[e2e]\033[0m %s\n' "$*"; }
fail() {
    printf '\033[31m[e2e] FAIL:\033[0m %s\n' "$*" >&2
    exit 1
}

if [ "$URL_MODE" = 0 ] && [ -z "$IMAGE" ]; then
    echo "Usage: $0 <image>   (or set E2E_URL to test a running server)" >&2
    exit 2
fi
for tool in curl python3; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is required."
done
if [ "$URL_MODE" = 0 ]; then
    command -v docker >/dev/null 2>&1 || fail "docker is required."
else
    command -v ffmpeg >/dev/null 2>&1 || fail "E2E_URL mode needs ffmpeg on this host to create test media."
fi

case $GOAL in
    save_space | archive) EXPECT_CODEC=av1 ;;
    balanced) EXPECT_CODEC=hevc ;;
    compatible) EXPECT_CODEC=h264 ;;
    *) fail "Unknown E2E_GOAL \"$GOAL\" (use save_space, balanced, compatible or archive)." ;;
esac

WORK=$(mktemp -d "${TMPDIR:-/tmp}/szalinski-e2e.XXXXXX")
MEDIA="$WORK/media"
CONFIG="$WORK/config"
CONTAINER=""
mkdir -p "$MEDIA" "$CONFIG"

cleanup() {
    local status=$?
    if [ -n "$CONTAINER" ]; then
        if [ "$status" -ne 0 ]; then
            echo "----- container log (last 150 lines) -----" >&2
            docker logs --tail 150 "$CONTAINER" >&2 2>&1 || true
            echo "-------------------------------------------" >&2
        fi
        if [ "${E2E_KEEP:-0}" = 1 ]; then
            log "Keeping container $CONTAINER"
        else
            docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
        fi
    fi
    if [ "${E2E_KEEP:-0}" = 1 ]; then
        log "Keeping work folder $WORK"
    elif ! rm -rf "$WORK" 2>/dev/null && [ "$URL_MODE" = 0 ]; then
        # Files written by the container may belong to another uid.
        docker run --rm --entrypoint sh -v "$WORK:/work" "$IMAGE" -c 'rm -rf /work/*' >/dev/null 2>&1 || true
        rm -rf "$WORK" 2>/dev/null || log "Could not remove $WORK"
    fi
    exit "$status"
}
trap cleanup EXIT

# Pretty-print or query JSON from stdin: pyjson '<expression using d>'.
pyjson() {
    python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"
}

# api METHOD PATH [JSON]: prints the body; fails on HTTP errors with the body.
api() {
    local method=$1 path=$2 body=${3:-}
    local args=(-sS --fail-with-body --max-time 30 -X "$method")
    if [ -n "$body" ]; then
        args+=(-H 'Content-Type: application/json' --data "$body")
    fi
    curl "${args[@]}" "$BASE$path"
}

container_running() {
    [ "$(docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null)" = true ]
}

# --- 1. Test media --------------------------------------------------------------

log "Creating test media (${CLIP_SECONDS}s clips) in $MEDIA"
# ffmpeg's output only matters if generation fails.
if [ "$URL_MODE" = 1 ]; then
    "$SCRIPT_DIR/make-test-media.sh" "$MEDIA" "$CLIP_SECONDS" >"$WORK/make-media.log" 2>&1 ||
        fail "Could not create test media: $(tail -n 20 "$WORK/make-media.log")"
else
    docker run --rm --user "$(id -u):$(id -g)" --entrypoint sh \
        -v "$SCRIPT_DIR:/scripts:ro" -v "$MEDIA:/out" \
        "$IMAGE" /scripts/make-test-media.sh /out "$CLIP_SECONDS" >"$WORK/make-media.log" 2>&1 ||
        fail "Could not create test media with the image's ffmpeg: $(tail -n 20 "$WORK/make-media.log")"
fi
# Whatever uid the app runs as must be able to replace these files.
chmod -R a+rwX "$MEDIA"
(cd "$MEDIA" && find . -type f | sort) >"$WORK/inputs.txt"
INPUT_COUNT=$(wc -l <"$WORK/inputs.txt" | tr -d ' ')
log "$INPUT_COUNT input files"

# --- 2. Start the server --------------------------------------------------------------

if [ "$URL_MODE" = 1 ]; then
    BASE="${E2E_URL%/}/api"
    LIB_PATH="$MEDIA"
    log "Using the server at $E2E_URL"
else
    # Run as the host user so the work folder stays removable; as root, use
    # 1000 so the privilege drop is exercised too.
    RUN_UID=$(id -u)
    RUN_GID=$(id -g)
    if [ "$RUN_UID" = 0 ]; then
        RUN_UID=1000
        RUN_GID=1000
    fi
    CONTAINER="szalinski-e2e-$$"
    env_args=(-e "PUID=$RUN_UID" -e "PGID=$RUN_GID" -e TZ=UTC -e "HW_ACCEL=${E2E_HW_ACCEL:-auto}")
    if [ -n "${E2E_MAX_JOBS:-}" ]; then
        env_args+=(-e "MAX_JOBS=$E2E_MAX_JOBS")
    fi
    log "Starting $IMAGE as uid $RUN_UID, gid $RUN_GID"
    docker run -d --name "$CONTAINER" \
        -p "127.0.0.1:${E2E_PORT:-}:8080" \
        -v "$MEDIA:/media" -v "$CONFIG:/config" \
        "${env_args[@]}" \
        --health-interval 5s \
        "$IMAGE" >/dev/null
    sleep 1
    container_running || fail "The container stopped right after starting."
    port=$(docker port "$CONTAINER" 8080/tcp 2>/dev/null | head -n 1 | sed 's/.*://')
    [ -n "$port" ] || fail "Docker did not publish port 8080."
    BASE="http://127.0.0.1:$port/api"
    LIB_PATH=/media
fi

log "Waiting for $BASE/health"
deadline=$((SECONDS + 120))
until health=$(curl -fsS --max-time 5 "$BASE/health" 2>/dev/null); do
    if [ "$URL_MODE" = 0 ] && ! container_running; then
        fail "The container stopped during startup."
    fi
    [ "$SECONDS" -lt "$deadline" ] || fail "The server did not answer /api/health within 120 s."
    sleep 1
done
[ "$(pyjson 'd.get("ok") is True' <<<"$health")" = True ] || fail "/api/health did not report ok: $health"
log "Server is up: $health"

# --- 3. Add the library and wait -------------------------------------------------------

body=$(printf '{"path": "%s", "name": "E2E library", "goal": "%s"}' "$LIB_PATH" "$GOAL")
library=$(api POST /libraries "$body") || fail "POST /api/libraries failed: $library"
LIB_ID=$(pyjson 'd["id"]' <<<"$library")
log "Library $LIB_ID created with goal \"$GOAL\"; waiting up to ${TIMEOUT}s for it to finish"

deadline=$((SECONDS + TIMEOUT))
idle_polls=0
last_report=""
while :; do
    if [ "$URL_MODE" = 0 ] && ! container_running; then
        fail "The container stopped while processing."
    fi
    lib=$(api GET "/libraries/$LIB_ID") || fail "GET /api/libraries/$LIB_ID failed: $lib"
    queue=$(api GET /queue) || fail "GET /api/queue failed: $queue"
    state=$(
        python3 -c '
import json, sys
lib, q, expected = json.loads(sys.argv[1]), json.loads(sys.argv[2]), int(sys.argv[3])
s = lib.get("stats") or {}
# New files join the library only once they have stopped changing (the
# server treats them as still being copied until then).
listed = s.get("file_count", 0)
idle = (not lib["scanning"]) and lib.get("last_scan_at") and listed >= expected \
    and q["running"] == 0 and q["queued"] == 0
print("idle" if idle else "busy",
      "scanning" if lib["scanning"] else "scanned",
      "files=%d/%d running=%d queued=%d done=%d skipped=%d failed=%d" % (
          listed, expected, q["running"], q["queued"], s.get("done", 0), s.get("skipped", 0),
          s.get("failed", 0)))
' "$lib" "$queue" "$INPUT_COUNT"
    )
    report=${state#* }
    if [ "$report" != "$last_report" ]; then
        log "$report"
        last_report=$report
    fi
    if [ "${state%% *}" = idle ]; then
        idle_polls=$((idle_polls + 1))
        [ $((idle_polls * 2)) -ge "$SETTLE" ] && break
    else
        idle_polls=0
    fi
    [ "$SECONDS" -lt "$deadline" ] || fail "Timed out after ${TIMEOUT}s with files missing from the library or work still queued or running ($report)."
    sleep 2
done

# --- 4. Check the results -----------------------------------------------------------------

api GET "/files?library=$LIB_ID&limit=500" >"$WORK/files.json" || fail "GET /api/files failed: $(cat "$WORK/files.json")"
api GET "/jobs?state=history&limit=500" >"$WORK/jobs.json" || fail "GET /api/jobs failed: $(cat "$WORK/jobs.json")"
(cd "$MEDIA" && find . -type f | sort) >"$WORK/outputs.txt"

if [ "$URL_MODE" = 1 ]; then
    probe_cmd=ffprobe
    command -v ffprobe >/dev/null 2>&1 || probe_cmd=""
else
    probe_cmd="docker exec $CONTAINER ffprobe"
fi

python3 - "$WORK" "$MEDIA" "$LIB_PATH" "$EXPECT_CODEC" "$probe_cmd" <<'PY'
import json, os, shlex, subprocess, sys

work, media_host, lib_path, expect_codec, probe_cmd = sys.argv[1:6]
files = json.load(open(os.path.join(work, "files.json")))["items"]
jobs = json.load(open(os.path.join(work, "jobs.json")))["items"]
outputs = [l.strip() for l in open(os.path.join(work, "outputs.txt")) if l.strip()]
inputs = [l.strip() for l in open(os.path.join(work, "inputs.txt")) if l.strip()]
problems = []

def stem(name):
    return os.path.splitext(os.path.basename(name))[0]

def host_path(server_path):
    rel = os.path.relpath(server_path, lib_path)
    return os.path.join(media_host, rel)

by_stem = {}
for f in files:
    by_stem.setdefault(stem(f["file_name"]), []).append(f)

def expect(stem_name, allowed, why):
    found = by_stem.get(stem_name, [])
    if not found:
        problems.append(f"{stem_name}: not in the library ({why}).")
        return
    for f in found:
        if f["status"] not in allowed:
            detail = f.get("error") or f.get("skip_reason") or ""
            problems.append(f"{f['relative_path']}: {f['status']}, expected {' or '.join(allowed)} ({why}). {detail}".strip())

expect("Big Test (2020)", ("done", "skipped"), "H.264 MP4")
expect("Old Home Video", ("done", "skipped"), "MPEG-4 AVI with odd dimensions")
expect("Tone", ("skipped",), "audio-only files are left alone")

for f in files:
    broken = f["relative_path"].startswith("Broken/")
    if f["status"] in ("queued", "processing", "pending"):
        problems.append(f"{f['relative_path']}: still {f['status']} after the queue went idle.")
    elif f["status"] == "failed" and not broken:
        problems.append(f"{f['relative_path']}: failed: {f.get('error')}")

done_jobs = [j for j in jobs if j["state"] == "done"]
if not done_jobs:
    problems.append("No job finished as done; the library had files that need converting.")
for j in done_jobs:
    report = j.get("validation")
    if not report:
        problems.append(f"Job for {j['file_name']} is done but has no verification report.")
    elif not report.get("passed"):
        problems.append(f"Job for {j['file_name']} is done but its verification report did not pass.")

# Done files must exist and really be in the target codec.
for f in files:
    if f["status"] != "done":
        continue
    path = host_path(f["path"])
    if not os.path.isfile(path):
        problems.append(f"{f['relative_path']}: marked done but {path} does not exist.")
        continue
    if probe_cmd:
        cmd = shlex.split(probe_cmd) + ["-v", "error", "-select_streams", "v:0",
               "-show_entries", "stream=codec_name", "-of", "csv=p=0", f["path"]]
        out = subprocess.run(cmd, capture_output=True, text=True)
        codec = out.stdout.strip()
        if codec != expect_codec:
            problems.append(f"{f['relative_path']}: video codec is '{codec or out.stderr.strip()}', expected {expect_codec}.")

# Nothing lost, nothing left behind.
output_stems = {stem(p) for p in outputs}
for p in inputs:
    if not p.startswith("./Broken/") and stem(p) not in output_stems:
        problems.append(f"Original {p} is gone and no converted file replaced it.")
leftovers = [p for p in outputs if ".szalinski-" in os.path.basename(p)]
for p in leftovers:
    problems.append(f"Temporary or backup file left behind: {p}")

# Summary
def size(n):
    return "-" if n is None else f"{n / 1e6:.2f} MB"
print()
print(f"{'STATUS':9} {'FILE':46} {'BEFORE':>10} {'AFTER':>10}  DETAIL")
for f in sorted(files, key=lambda f: f["relative_path"]):
    before = f.get("original_size_bytes") or f.get("size_bytes")
    after = f.get("size_bytes") if f["status"] == "done" else None
    detail = f.get("skip_reason") or f.get("error") or ""
    print(f"{f['status']:9} {f['relative_path'][:46]:46} {size(before):>10} {size(after):>10}  {detail[:60]}")
print()
for j in sorted(jobs, key=lambda j: j["file_name"]):
    report = j.get("validation") or {}
    ssim = report.get("ssim_min")
    print(f"job {j['state']:9} {j['file_name'][:40]:40} encoder={j.get('encoder') or '-':12} "
          f"verified={'yes' if report.get('passed') else 'no':3} ssim_min={'-' if ssim is None else f'{ssim:.3f}'}")
print()
if problems:
    for p in problems:
        print(f"PROBLEM: {p}", file=sys.stderr)
    sys.exit(1)
print(f"{len(files)} files checked, {len(done_jobs)} converted and verified.")
PY

# --- 5. Still healthy? ------------------------------------------------------------------------

if [ "$URL_MODE" = 0 ]; then
    container_running || fail "The container is no longer running."
    status=""
    for _ in $(seq 1 24); do
        status=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$CONTAINER")
        [ "$status" = healthy ] && break
        sleep 5
    done
    [ "$status" = healthy ] || fail "Container health is \"$status\", expected healthy."
    log "Container is healthy"
fi
curl -fsS --max-time 5 "$BASE/health" >/dev/null || fail "/api/health stopped answering."

log "PASS"
