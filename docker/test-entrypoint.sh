#!/usr/bin/env bash
# Regression tests for docker/entrypoint.sh, run against a built image.
#
#   docker/test-entrypoint.sh <image>        e.g. docker/test-entrypoint.sh chrysopoeia:dev
#
# Covers PUID/PGID (including a gid that already exists, such as Unraid's 100
# "users"), GPU device groups (never the root group), UMASK, the /temp and
# empty-variable rules, the startup banner, warnings about missing, read-only
# or unwritable mounts, /config ownership (only Chrysopoeia's own files are
# re-owned, never a shared folder's other contents), the privilege drop and
# argument pass-through. Fake GPU nodes are created inside the container with mknod
# (Docker's default capabilities allow it), so no GPU and no sudo are needed.
# Takes about 30 seconds; nothing is left behind.
#
# Commands in single quotes expand their variables inside the container.
# shellcheck disable=SC2016
set -euo pipefail

IMAGE=${1:-}
if [ -z "$IMAGE" ]; then
    echo "Usage: $0 <image>" >&2
    exit 2
fi
command -v docker >/dev/null 2>&1 || {
    echo "docker is required." >&2
    exit 2
}

VOLUME="chrysopoeia-entrypoint-test-$$"
MEDIA_VOLUME="chrysopoeia-entrypoint-test-media-$$"
SHARED_VOLUME="chrysopoeia-entrypoint-test-shared-$$"
CONTAINER="chrysopoeia-entrypoint-test-$$"
FAILURES=0
OUT=""
STATUS=0

cleanup() {
    docker rm -f -v "$CONTAINER" >/dev/null 2>&1 || true
    docker volume rm -f "$VOLUME" "$MEDIA_VOLUME" "$SHARED_VOLUME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

pass() { printf '\033[32mPASS\033[0m %s\n' "$1"; }
fail() {
    printf '\033[31mFAIL\033[0m %s\n' "$1"
    printf '%s\n' "$OUT" | sed 's/^/     | /'
    FAILURES=$((FAILURES + 1))
}

# run <docker run options...> -- <command...>
# Runs the image's normal entrypoint; stdout and stderr go to $OUT, the exit
# status to $STATUS.
run() {
    local opts=()
    while [ "$1" != "--" ]; do
        opts+=("$1")
        shift
    done
    shift
    STATUS=0
    OUT=$(docker run --rm "${opts[@]}" "$IMAGE" "$@" 2>&1) || STATUS=$?
}

# run_with_devices <docker run options...> -- <command...>
# Like run, but first creates fake GPU nodes (all backed by /dev/null's
# driver) inside the container:
#   /dev/dri/renderD128  gid 993 (no group in the image)  0660
#   /dev/dri/card0       gid 44  ("video" in the image)   0660
#   /dev/dri/renderD129  gid 0   (root)                   0660
#   /dev/nvidia0         gid 0   (root)                   0666  world rw
#   /dev/video11         gid 100 ("users")                0660
run_with_devices() {
    local opts=()
    while [ "$1" != "--" ]; do
        opts+=("$1")
        shift
    done
    shift
    local setup='set -e
mkdir -p /dev/dri
mknod /dev/dri/renderD128 c 1 3 && chgrp 993 /dev/dri/renderD128 && chmod 660 /dev/dri/renderD128
mknod /dev/dri/card0 c 1 3 && chgrp 44 /dev/dri/card0 && chmod 660 /dev/dri/card0
mknod /dev/dri/renderD129 c 1 3 && chgrp 0 /dev/dri/renderD129 && chmod 660 /dev/dri/renderD129
mknod /dev/nvidia0 c 1 3 && chgrp 0 /dev/nvidia0 && chmod 666 /dev/nvidia0
mknod /dev/video11 c 1 3 && chgrp 100 /dev/video11 && chmod 660 /dev/video11
exec /usr/local/bin/entrypoint.sh "$@"'
    STATUS=0
    OUT=$(docker run --rm --entrypoint /bin/sh "${opts[@]}" "$IMAGE" -c "$setup" sh "$@" 2>&1) || STATUS=$?
}

# server_log <docker run options...>
# Starts the server detached through the normal entrypoint and puts its log,
# up to the end of the startup banner, in $OUT. Warnings about mounts are
# printed only when the server itself starts, before the banner.
server_log() {
    docker rm -f -v "$CONTAINER" >/dev/null 2>&1 || true
    STATUS=0
    OUT=""
    docker run -d --name "$CONTAINER" "$@" "$IMAGE" >/dev/null || STATUS=$?
    for _ in $(seq 1 30); do
        OUT=$(docker logs "$CONTAINER" 2>&1 || true)
        case $OUT in *"Web UI"*) break ;; esac
        sleep 0.5
    done
    docker rm -f -v "$CONTAINER" >/dev/null 2>&1 || true
}

# expect <description> <extended regex that $OUT must match>
expect() {
    if [ "$STATUS" -eq 0 ] && printf '%s\n' "$OUT" | grep -Eq -- "$2"; then
        pass "$1"
    else
        fail "$1 (exit $STATUS, expected output matching: $2)"
    fi
}

# expect_not <description> <extended regex that $OUT must not match>
expect_not() {
    if [ "$STATUS" -eq 0 ] && ! printf '%s\n' "$OUT" | grep -Eq -- "$2"; then
        pass "$1"
    else
        fail "$1 (exit $STATUS, output must not match: $2)"
    fi
}

# expect_error <description> <extended regex for the error message>
expect_error() {
    if [ "$STATUS" -ne 0 ] && printf '%s\n' "$OUT" | grep -Eq -- "$2"; then
        pass "$1"
    else
        fail "$1 (exit $STATUS, expected a failure matching: $2)"
    fi
}

echo "Testing the entrypoint of $IMAGE"

# --- Users and groups ---------------------------------------------------------

run -- id
expect "defaults to uid 1000 and gid 1000" '^uid=1000\(chrysopoeia\) gid=1000\(chrysopoeia\)'

run -e PUID=99 -e PGID=100 -- id
expect "Unraid ids 99/100 reuse the existing users group" '^uid=99\(chrysopoeia\) gid=100\(users\)'

run -e PUID=4242 -e PGID=4343 -- id
expect "new ids move the image's user and group" '^uid=4242\(chrysopoeia\) gid=4343\(chrysopoeia\)'

# 33 is www-data in Debian: `id` may print either account's name.
run -e PUID=33 -e PGID=33 -- id
expect "a uid and gid that belong to another account still work" '^uid=33\([a-z-]+\) gid=33\('

run -e PUID=abc -- id
expect_error "rejects a PUID that is not a number" 'PUID must be a number'

run -e PUID=0 -e PGID=0 -- id -u
expect "PUID=0 runs as root" '^0$'

# --- GPU device groups ----------------------------------------------------------

run_with_devices -e PUID=99 -e PGID=100 -- id
expect "joins the group of a render node without a named group" 'groups=.*993\(gpu993\)'
expect "joins an existing video group" 'groups=.*44\(video\)'
expect_not "never joins the root group" 'groups=.*[=,]0\(root\)'
expect "warns about a root-group render node" 'renderD129 belongs to the root group'
expect_not "does not warn about world-readable nodes" '/dev/nvidia0 belongs to the root group'

run_with_devices -e PUID=99 -e PGID=0 -- id
expect_not "no root-group warning when PGID is 0" 'belongs to the root group'

# The startup banner lists the GPU devices; it is printed only when the server
# itself starts, so start it detached and read the log.
docker run -d --name "$CONTAINER" --entrypoint /bin/sh -e PUID=99 -e PGID=100 "$IMAGE" -c \
    'mkdir -p /dev/dri && mknod /dev/dri/renderD128 c 1 3 && chgrp 993 /dev/dri/renderD128 && chmod 660 /dev/dri/renderD128 && exec /usr/local/bin/entrypoint.sh' >/dev/null
OUT=""
for _ in $(seq 1 30); do
    OUT=$(docker logs "$CONTAINER" 2>&1 || true)
    case $OUT in *"Web UI"*) break ;; esac
    sleep 0.5
done
docker rm -f -v "$CONTAINER" >/dev/null 2>&1 || true
STATUS=0
expect "the startup banner lists GPU devices" 'GPU devices +/dev/dri/renderD128 \(group gpu993\)'
expect "the startup banner names the user" 'Runs as +uid 99 \(chrysopoeia\), gid 100 \(users\)'
# The server's own version (Cargo.toml), plus the build label when it differs.
expect "the startup banner shows the server version" 'Chrysopoeia [0-9]+\.[0-9]+\.[0-9]+( \(build [^)]+\))?$'

# --- Mount warnings ---------------------------------------------------------------

docker volume create "$VOLUME" >/dev/null
docker volume create "$MEDIA_VOLUME" >/dev/null

server_log -e PUID=99 -e PGID=100
expect "warns when no host folder is mounted at /config" 'No host folder is mounted at /config'
expect "warns when no media folder is mounted" 'No media folder is mounted at /media'

server_log -e PUID=99 -e PGID=100 -v "$VOLUME:/config" --tmpfs /media:mode=1777
expect_not "no mount warnings with /config and a writable /media" 'No host folder|No media folder|cannot write to /media|read-only'

server_log -e PUID=99 -e PGID=100 -e BROWSE_ROOTS=/data -v "$VOLUME:/config"
expect_not "no /media warning when other folders are configured" 'No media folder'

# A fresh volume holds the image's empty /media: owned by root, mode 755.
server_log -e PUID=99 -e PGID=100 -v "$VOLUME:/config" -v "$MEDIA_VOLUME:/media:ro"
expect "explains a read-only /media" '/media is mounted read-only'
expect_not "does not blame PUID/PGID for a read-only /media" 'cannot write to /media'

server_log -e PUID=99 -e PGID=100 -v "$VOLUME:/config" -v "$MEDIA_VOLUME:/media"
expect "warns when PUID/PGID cannot write to /media" 'Chrysopoeia \(uid 99\) cannot write to /media.*PUID/PGID'

server_log --user 1234:1234 -v "$VOLUME:/config" -v "$MEDIA_VOLUME:/media"
expect "with --user, warns that /media is not writable" 'Chrysopoeia \(uid 1234\) cannot write to /media'

# --- Environment rules ----------------------------------------------------------

run -e UMASK=022 -- sh -c umask
expect "applies UMASK" '^0022$'

run -e UMASK=abc -- sh -c umask
expect_error "rejects a UMASK that is not octal" 'UMASK must be an octal value'

run --tmpfs /temp -- sh -c 'echo "temp=${TEMP_DIR-unset}"'
expect "uses /temp when it is mounted" '^temp=/temp$'

run -- sh -c 'echo "temp=${TEMP_DIR-unset}"'
expect "leaves TEMP_DIR unset without a /temp mount" '^temp=unset$'

run -e MAX_JOBS= -e HW_ACCEL= -e LIBRARIES= -e ALLOWED_HOSTS= -- sh -c 'echo "jobs=${MAX_JOBS-unset} hw=${HW_ACCEL-unset} libs=${LIBRARIES-unset} hosts=${ALLOWED_HOSTS-unset}"'
expect "treats empty template values as unset" '^jobs=unset hw=unset libs=unset hosts=unset$'

# --- Ownership and privileges -----------------------------------------------------

docker volume create "$VOLUME" >/dev/null
run -v "$VOLUME:/config" -e PUID=99 -e PGID=100 -- stat -c '%u:%g' /config
expect "gives /config to PUID/PGID" '^99:100$'

# /config may be a folder shared with other apps (a template mistake such as
# pointing Config at /mnt/user/appdata). Only Chrysopoeia's own files may change
# owner; everything else in it must be left exactly as it is.

# seed_shared <shell commands run as root in /config, which starts empty>
seed_shared() {
    docker run --rm --entrypoint /bin/sh -v "$SHARED_VOLUME:/config" "$IMAGE" -c \
        "set -e; find /config -mindepth 1 -delete; $1"
}
# shared_owners <paths...>: "<path> <uid>:<gid>" lines in $OUT, read without the entrypoint.
shared_owners() {
    STATUS=0
    OUT=$(docker run --rm --entrypoint /bin/stat -v "$SHARED_VOLUME:/config" "$IMAGE" -c '%n %u:%g' "$@" 2>&1) || STATUS=$?
}
docker volume create "$SHARED_VOLUME" >/dev/null

# Chrysopoeia's database and lock files left behind by another owner (for
# example by an earlier PUID) are fixed; a neighbour's files are not.
seed_shared 'mkdir -p /config/otherapp/data
echo x > /config/otherapp/data/file
echo y > /config/notes.txt
touch /config/chrysopoeia.db /config/chrysopoeia.db-wal /config/chrysopoeia.db-shm /config/chrysopoeia.lock
chown -R 1234:1234 /config/otherapp /config/notes.txt
chown 555:555 /config/chrysopoeia.db /config/chrysopoeia.db-wal /config/chrysopoeia.db-shm /config/chrysopoeia.lock
chown 555:555 /config'
run -v "$SHARED_VOLUME:/config" -e PUID=99 -e PGID=100 -- id -u
expect "starts with a Config folder that has Chrysopoeia's files and other data" '^99$'
shared_owners /config /config/chrysopoeia.db /config/chrysopoeia.db-wal /config/chrysopoeia.db-shm /config/chrysopoeia.lock \
    /config/notes.txt /config/otherapp /config/otherapp/data /config/otherapp/data/file
expect "gives the database to PUID/PGID" '^/config/chrysopoeia\.db 99:100$'
expect "gives the database's write-ahead files to PUID/PGID" '^/config/chrysopoeia\.db-wal 99:100$'
expect "gives the database's shared-memory file to PUID/PGID" '^/config/chrysopoeia\.db-shm 99:100$'
expect "gives the lock file to PUID/PGID" '^/config/chrysopoeia\.lock 99:100$'
expect "gives a Config folder that holds the database to PUID/PGID" '^/config 99:100$'
expect "leaves another app's folder alone" '^/config/otherapp 1234:1234$'
expect "leaves files inside another app's folder alone" '^/config/otherapp/data/file 1234:1234$'
expect "leaves other files in the Config folder alone" '^/config/notes\.txt 1234:1234$'

# A folder that only holds other data is not Chrysopoeia's to take over: it
# is left as it is, and the container stops with a message if it cannot write.
seed_shared 'mkdir -p /config/otherapp
echo x > /config/otherapp/file
chown -R 1234:1234 /config/otherapp
chown 0:0 /config
chmod 755 /config'
run -v "$SHARED_VOLUME:/config" -e PUID=99 -e PGID=100 -- id -u
expect_error "stops with a message instead of taking over a folder that holds other data" 'cannot write to /config.*never of anything else'
shared_owners /config /config/otherapp /config/otherapp/file
expect "did not change the shared folder's owner" '^/config 0:0$'
expect "did not change the owner of anything in the shared folder" '^/config/otherapp 1234:1234$'
expect "did not change the owner of files in the shared folder" '^/config/otherapp/file 1234:1234$'

# Already writable for the app user: starts, leaves everything alone and says
# that the folder looks shared.
seed_shared 'echo y > /config/other.conf
chown 1234:1234 /config/other.conf
chown 99:100 /config
chmod 775 /config'
server_log -e PUID=99 -e PGID=100 -v "$SHARED_VOLUME:/config"
expect "warns when the Config folder holds other files and no Chrysopoeia database" 'already holds other files but no Chrysopoeia database'
shared_owners /config/other.conf
expect "still leaves the other files alone" '^/config/other\.conf 1234:1234$'

run -e PUID=99 -e PGID=100 -- sh -c 'grep -E "^(NoNewPrivs|CapEff)" /proc/self/status'
expect "drops all capabilities" 'CapEff:[[:space:]]+0+$'
expect "sets no_new_privs" 'NoNewPrivs:[[:space:]]+1$'

run --user 1234:1234 -- id -u
expect "with --user, runs the command directly as that user" '^1234$'

# --- Argument pass-through ---------------------------------------------------------

run -- --help
expect "flags reach the server binary" 'Usage'

echo
if [ "$FAILURES" -gt 0 ]; then
    echo "$FAILURES entrypoint test(s) failed."
    exit 1
fi
echo "All entrypoint tests passed."
