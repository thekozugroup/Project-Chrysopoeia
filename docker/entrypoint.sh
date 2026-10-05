#!/bin/sh
# Szalinski container entrypoint (POSIX sh; runs under tini).
#
# Started as root (the default), it:
#   1. moves the "szalinski" user and group to PUID/PGID (default 1000/1000;
#      Unraid uses 99/100) and applies UMASK (default 002),
#   2. adds the user to the groups that own the GPU device nodes it can see
#      (/dev/dri, /dev/nvidia*, and ARM video/codec nodes), except root,
#   3. hands that user Szalinski's own files in /config (the database and its
#      lock), the top folder of /config when it is new, empty or already holds
#      the database, and the top folder of /temp, without ever re-owning other
#      files or folders (the folder may be shared),
#   4. prints a short banner and drops privileges with setpriv.
# Started as any other user (docker run --user ...), it only applies UMASK
# and the path defaults, then runs the command directly.
#
# Arguments: none or flags run the server ("docker run image --help" reaches
# the binary); anything else is run as a command as the app user
# ("docker run image id").
set -eu

APP_USER=szalinski

log() { printf '[szalinski] %s\n' "$*"; }
warn() { printf '[szalinski] Warning: %s\n' "$*" >&2; }
die() {
    printf '[szalinski] Error: %s\n' "$*" >&2
    exit 1
}

if [ $# -eq 0 ] || [ "${1#-}" != "$1" ]; then
    set -- szalinski "$@"
fi

# Show the banner only when starting the server for real.
show_banner=0
if [ "$1" = szalinski ]; then
    show_banner=1
    for arg in "$@"; do
        case $arg in -h | --help | -V | --version) show_banner=0 ;; esac
    done
fi

# --- Environment ------------------------------------------------------------

# Templates (Unraid in particular) pass optional settings as empty strings.
# An empty value means "not set", never "set to nothing".
for var in TEMP_DIR MAX_JOBS HW_ACCEL LIBRARIES BROWSE_ROOTS ALLOWED_HOSTS LOG_LEVEL RUST_LOG BIND DEV_CORS; do
    eval "value=\${$var-}"
    if [ -z "$value" ]; then
        unset "$var"
    fi
done
: "${DATA_DIR:=/config}"
: "${WEB_DIR:=/app/web}"
: "${PORT:=8080}"
: "${FFMPEG_PATH:=/usr/local/bin/ffmpeg}"
: "${FFPROBE_PATH:=/usr/local/bin/ffprobe}"
export DATA_DIR WEB_DIR PORT FFMPEG_PATH FFPROBE_PATH

# /temp is not part of the image, so if it exists it was mounted on purpose.
if [ -z "${TEMP_DIR:-}" ] && [ -d /temp ]; then
    export TEMP_DIR=/temp
fi
# Open the folder picker at /media when it is mounted; the rest of the
# container stays reachable through "/". (Debian ships an empty /media, so
# only a mount point counts.)
media_mounted=0
if mountpoint -q /media 2>/dev/null; then
    media_mounted=1
fi
if [ -z "${BROWSE_ROOTS:-}" ] && [ "$media_mounted" = 1 ]; then
    export BROWSE_ROOTS=/media,/
fi
# Some GPU runtimes cache compiled kernels under $HOME.
export HOME="$DATA_DIR"

# --- Mounts -------------------------------------------------------------------

# The last /proc/self/mountinfo line for mount point $1 (empty when nothing is
# mounted there). Fields: 4 = folder of the source filesystem that is
# mounted, 5 = mount point, 6 = mount options, then after "-" the filesystem
# type, source and superblock options.
mount_info() {
    awk -v target="$1" '$5 == target { line = $0 } END { if (line != "") print line }' \
        /proc/self/mountinfo 2>/dev/null || true
}

# Whether $1 is mounted read-only (docker -v ...:ro, Unraid Access Mode
# "Read Only", or a read-only filesystem).
mounted_read_only() {
    mount_info "$1" | awk '{
        n = split($6, opts, ",")
        for (i = 1; i <= n; i++) if (opts[i] == "ro") ro = 1
        for (i = 7; i < NF; i++) if ($i == "-") {
            n = split($(i + 3), opts, ",")
            for (j = 1; j <= n; j++) if (opts[j] == "ro") ro = 1
            break
        }
    } END { exit ro ? 0 : 1 }'
}

# Whether $1 is an anonymous Docker (or Podman) volume: Docker creates one for
# the image's VOLUME when no folder is mounted there, named by 64 hex digits,
# and it is not reused when the container is recreated.
anonymous_volume() {
    mount_info "$1" | grep -Eq '^[^ ]+ [^ ]+ [^ ]+ [^ ]*/volumes/[0-9a-f]{64}/_data '
}

# Names of the files Szalinski creates in the Config folder: the database (in
# write-ahead mode, hence the extra files) and the lock that keeps a second
# copy out.
DB_FILE=szalinski.db
LOCK_FILE=szalinski.lock

# Whether the Config folder holds nothing yet (the "lost+found" of a freshly
# formatted disk does not count).
config_dir_is_empty() {
    [ -z "$(find "$DATA_DIR" -mindepth 1 -maxdepth 1 ! -name lost+found -print -quit 2>/dev/null)" ]
}

# Warnings about missing or unusable mounts, printed when the server starts.
# $1: a command prefix that runs a test as the app user (empty: as is).
check_mounts() {
    [ "$show_banner" = 1 ] || return 0
    if ! mountpoint -q "$DATA_DIR" 2>/dev/null || anonymous_volume "$DATA_DIR"; then
        warn "No host folder is mounted at $DATA_DIR, so libraries, settings and history are lost when the container is recreated (for example by an update). Mount a folder there (Unraid: the Config path; docker: -v /path/on/host:$DATA_DIR)."
    fi
    if [ -d "$DATA_DIR" ] && [ ! -e "$DATA_DIR/$DB_FILE" ] && ! config_dir_is_empty; then
        warn "$DATA_DIR already holds other files but no Szalinski database. Szalinski adds its own files there and leaves the rest alone, but if this folder is shared with other apps, give Szalinski a folder of its own (Unraid: the Config path, for example /mnt/user/appdata/szalinski)."
    fi
    if [ "$media_mounted" = 0 ]; then
        if [ -z "${BROWSE_ROOTS:-}" ] && [ -z "${LIBRARIES:-}" ]; then
            warn "No media folder is mounted at /media. Mount the folder with your videos there (Unraid: the Media path; docker: -v /path/to/media:/media), or the folder picker will only show the container's own files."
        fi
    elif mounted_read_only /media; then
        warn "/media is mounted read-only, so Szalinski cannot replace files there. That only works with an Output folder elsewhere (Settings > Output). To replace originals, make it writable: on Unraid set the Media path's Access Mode to Read/Write; with docker, remove :ro."
    elif ! "$@" test -w /media; then
        if [ "$(id -u)" -eq 0 ]; then
            warn "Szalinski (uid $PUID) cannot write to /media, so it cannot replace files there. Set PUID/PGID to the owner of your media (Unraid: 99/100), or choose Output folder in Settings > Output."
        else
            warn "Szalinski (uid $(id -u)) cannot write to /media, so it cannot replace files there. Run the container as the owner of your media, or choose Output folder in Settings > Output."
        fi
    fi
}

UMASK=${UMASK:-002}
case $UMASK in
    '' | *[!0-7]*) die "UMASK must be an octal value such as 002 or 022 (got \"$UMASK\")." ;;
esac
[ "${#UMASK}" -le 4 ] || die "UMASK must be an octal value such as 002 or 022 (got \"$UMASK\")."
umask "$UMASK"

banner() {
    # $1: one line describing who the server runs as.
    [ "$show_banner" = 1 ] || return 0
    if [ -n "${TEMP_DIR:-}" ]; then
        temp_line="$TEMP_DIR"
    else
        temp_line="next to each file (mount /temp to use an SSD instead)"
    fi
    # The server reports its own version (Cargo.toml) in the log and API; the
    # build label (a release tag, or <branch>-<commit>) names the image.
    image_version=${SZALINSKI_VERSION:-${CHRYSOPOEIA_VERSION:-dev}}
    app_version=$(szalinski --version 2>/dev/null | awk 'NR == 1 { print $NF }') || app_version=""
    if [ -z "$app_version" ] || [ "$app_version" = "$image_version" ]; then
        title="Szalinski $image_version"
    else
        title="Szalinski $app_version (build $image_version)"
    fi
    log "------------------------------------------------------------"
    log "$title"
    log "Runs as      $1"
    log "Config       $DATA_DIR"
    log "Temp files   $temp_line"
    if [ -n "$gpu_lines" ]; then
        first=1
        printf '%s\n' "$gpu_lines" | while IFS= read -r line; do
            [ -n "$line" ] || continue
            if [ "$first" = 1 ]; then
                log "GPU devices  $line"
                first=0
            else
                log "             $line"
            fi
        done
    else
        log "GPU devices  none visible (CPU encoding; see docs/HARDWARE.md to add a GPU)"
    fi
    log "Web UI       port $PORT"
    log "------------------------------------------------------------"
}

# Warn when the NVIDIA variables are set but the NVIDIA runtime did not
# inject any devices: the most common NVIDIA setup mistake.
check_nvidia_runtime() {
    case ${NVIDIA_VISIBLE_DEVICES:-} in
        '' | none | void) return 0 ;;
    esac
    if [ ! -e /dev/nvidiactl ]; then
        warn "NVIDIA_VISIBLE_DEVICES is set but no NVIDIA GPU is visible. Add --runtime=nvidia to the container (Unraid: Extra Parameters) and make sure the NVIDIA driver is installed on the host."
    fi
}

gpu_lines=""
other_nodes=0

# Record a device node for the banner: render nodes and NVIDIA GPUs by name,
# companion and ARM codec nodes as a count.
note_device() {
    # $1: device path, $2: owning group name
    case $1 in
        /dev/dri/renderD*) gpu_lines="$gpu_lines$1 (group $2)
" ;;
        /dev/nvidia[0-9]*) gpu_lines="$gpu_lines$1 (NVIDIA)
" ;;
        /dev/dri/card* | /dev/nvidiactl | /dev/nvidia-*) ;;
        *) other_nodes=$((other_nodes + 1)) ;;
    esac
}

finish_device_notes() {
    if [ "$other_nodes" -gt 0 ]; then
        gpu_lines="$gpu_lines$other_nodes other video or codec device node(s)
"
    fi
}

DEVICE_NODES="/dev/dri/* /dev/nvidia* /dev/video* /dev/dma_heap/* /dev/mpp_service /dev/rga /dev/kfd"

# --- Not root: nothing to adjust ----------------------------------------------

if [ "$(id -u)" -ne 0 ]; then
    for dev in $DEVICE_NODES; do
        [ -c "$dev" ] || continue
        note_device "$dev" "$(stat -c %G "$dev")"
    done
    finish_device_notes
    check_nvidia_runtime
    check_mounts
    banner "uid $(id -u), gid $(id -g) (from --user; PUID/PGID are ignored), umask $UMASK"
    exec "$@"
fi

# --- Root: adopt PUID/PGID ------------------------------------------------------

PUID=${PUID:-1000}
PGID=${PGID:-1000}
case $PUID in '' | *[!0-9]*) die "PUID must be a number such as 99 or 1000 (got \"$PUID\")." ;; esac
case $PGID in '' | *[!0-9]*) die "PGID must be a number such as 100 or 1000 (got \"$PGID\")." ;; esac

readonly_hint="Could not update the container's user list. If the container runs with a read-only root filesystem, start it with --user $PUID:$PGID instead of PUID/PGID."

if [ "$PUID" -eq 0 ]; then
    warn "PUID is 0, so Szalinski runs as root. Files it writes will be owned by root."
else
    # Use an existing group with this gid (e.g. 100 "users" on Unraid);
    # otherwise move the image's "szalinski" group to it.
    if [ -z "$(getent group "$PGID")" ]; then
        groupmod -g "$PGID" "$APP_USER" || die "$readonly_hint"
    fi
    # -o: the uid may already belong to another account in the image.
    if [ "$(id -u "$APP_USER")" != "$PUID" ]; then
        usermod -o -u "$PUID" "$APP_USER" || die "$readonly_hint"
    fi
    if [ "$(id -g "$APP_USER")" != "$PGID" ]; then
        usermod -g "$PGID" "$APP_USER" || die "$readonly_hint"
    fi
fi

# --- GPU device access -----------------------------------------------------------

# Give the app user every group that owns a GPU or codec device node it could
# not otherwise open. Groups missing from the image are created by gid. The
# root group (gid 0) is never joined: it would also grant write access to
# every root-group-writable file in the container and in mounted folders.
for dev in $DEVICE_NODES; do
    [ -c "$dev" ] || continue
    dev_gid=$(stat -c %g "$dev")
    dev_mode=$(stat -c %a "$dev")
    group_bits=$(((dev_mode / 10) % 10))
    other_bits=$((dev_mode % 10))
    dev_group=$(getent group "$dev_gid" | cut -d: -f1)
    if [ "$PUID" -ne 0 ] && [ $((other_bits & 6)) -ne 6 ]; then
        if [ "$dev_gid" -eq 0 ] && [ "$PGID" -ne 0 ]; then
            warn "$dev belongs to the root group, which Szalinski does not join, so it may not be able to use this device. On the host, give it a group of its own, for example: chgrp video $dev && chmod g+rw $dev (a udev rule makes this permanent)."
        else
            if [ $((group_bits & 6)) -ne 6 ]; then
                warn "$dev is not readable and writable by its group, so Szalinski may not be able to use it. On the host, run: chmod g+rw $dev"
            fi
            if [ "$dev_gid" != "$PGID" ]; then
                if [ -z "$dev_group" ]; then
                    dev_group="gpu$dev_gid"
                    groupadd -g "$dev_gid" "$dev_group" || die "$readonly_hint"
                fi
                if ! id -nG "$APP_USER" | tr ' ' '\n' | grep -qx "$dev_group"; then
                    usermod -a -G "$dev_group" "$APP_USER" || die "$readonly_hint"
                fi
            fi
        fi
    fi
    note_device "$dev" "${dev_group:-gid $dev_gid}"
done
finish_device_notes
check_nvidia_runtime

# --- Ownership of writable folders ---------------------------------------------------

# Never change the owner of a whole folder tree: the Config folder may be shared
# (for example an Unraid appdata folder that also holds other apps' data), and
# re-owning that would damage those apps. Only what Szalinski itself creates
# is fixed: its database and lock files, and the top folder when it is new or
# empty (or already holds Szalinski's database).

# Give $1, a file Szalinski creates, to the app user. A symbolic link is
# changed itself, never followed.
own_file() {
    { [ -e "$1" ] || [ -L "$1" ]; } || return 0
    if [ "$(stat -c %u:%g "$1")" != "$PUID:$PGID" ]; then
        chown -h "$PUID:$PGID" "$1" || warn "Could not change the owner of $1."
    fi
}

if [ "$PUID" -ne 0 ]; then
    if [ ! -d "$DATA_DIR" ]; then
        mkdir -p "$DATA_DIR" || die "Could not create $DATA_DIR. Mount a folder there (Unraid: the Config path; docker: -v /path/on/host:$DATA_DIR)."
    fi
    if [ "$(stat -c %u:%g "$DATA_DIR")" != "$PUID:$PGID" ] && { config_dir_is_empty || [ -e "$DATA_DIR/$DB_FILE" ]; }; then
        log "Giving $DATA_DIR to uid $PUID, gid $PGID"
        chown "$PUID:$PGID" "$DATA_DIR" || warn "Could not change the owner of $DATA_DIR."
    fi
    for name in "$DB_FILE" "$DB_FILE-wal" "$DB_FILE-shm" "$DB_FILE-journal" "$LOCK_FILE"; do
        own_file "$DATA_DIR/$name"
    done
    # The scratch folder may hold large files: fix only the folder itself.
    if [ -n "${TEMP_DIR:-}" ] && [ -d "$TEMP_DIR" ]; then
        if [ "$(stat -c %u:%g "$TEMP_DIR")" != "$PUID:$PGID" ]; then
            chown "$PUID:$PGID" "$TEMP_DIR" || warn "Could not change the owner of $TEMP_DIR."
        fi
    fi
fi

# --- Drop privileges and start ----------------------------------------------------

if [ "$PUID" -eq 0 ]; then
    check_mounts
    banner "root (PUID=0), umask $UMASK"
    exec "$@"
fi

as_app() {
    setpriv --reuid="$APP_USER" --regid="$PGID" --init-groups --no-new-privs "$@"
}

if ! as_app test -w "$DATA_DIR"; then
    die "Szalinski (uid $PUID) cannot write to $DATA_DIR. Make the folder writable for PUID/PGID $PUID/$PGID, or set PUID/PGID to the folder's owner. (Szalinski only changes the owner of its own files there, never of anything else in the folder.)"
fi
check_mounts as_app

banner "uid $PUID ($APP_USER), gid $PGID ($(getent group "$PGID" | cut -d: -f1)), umask $UMASK"
exec setpriv --reuid="$APP_USER" --regid="$PGID" --init-groups --no-new-privs "$@"
