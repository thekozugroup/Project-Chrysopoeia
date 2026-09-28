#!/bin/sh
# Chrysopoeia container entrypoint (POSIX sh; runs under tini).
#
# Started as root (the default), it:
#   1. moves the "chrysopoeia" user and group to PUID/PGID (default 1000/1000;
#      Unraid uses 99/100) and applies UMASK (default 002),
#   2. adds the user to the groups that own the GPU device nodes it can see
#      (/dev/dri, /dev/nvidia*, and ARM video/codec nodes),
#   3. makes /config (and the top of /temp) owned by that user,
#   4. prints a short banner and drops privileges with setpriv.
# Started as any other user (docker run --user ...), it only applies UMASK
# and the path defaults, then runs the command directly.
#
# Arguments: none or flags run the server ("docker run image --help" reaches
# the binary); anything else is run as a command as the app user
# ("docker run image id").
set -eu

APP_USER=chrysopoeia

log() { printf '[chrysopoeia] %s\n' "$*"; }
warn() { printf '[chrysopoeia] Warning: %s\n' "$*" >&2; }
die() {
    printf '[chrysopoeia] Error: %s\n' "$*" >&2
    exit 1
}

if [ $# -eq 0 ] || [ "${1#-}" != "$1" ]; then
    set -- chrysopoeia "$@"
fi

# Show the banner only when starting the server for real.
show_banner=0
if [ "$1" = chrysopoeia ]; then
    show_banner=1
    for arg in "$@"; do
        case $arg in -h | --help | -V | --version) show_banner=0 ;; esac
    done
fi

# --- Environment ------------------------------------------------------------

# Templates (Unraid in particular) pass optional settings as empty strings.
# An empty value means "not set", never "set to nothing".
for var in TEMP_DIR MAX_JOBS HW_ACCEL LIBRARIES BROWSE_ROOTS LOG_LEVEL RUST_LOG BIND DEV_CORS; do
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
    log "------------------------------------------------------------"
    log "Chrysopoeia ${CHRYSOPOEIA_VERSION:-dev}"
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
    warn "PUID is 0, so Chrysopoeia runs as root. Files it writes will be owned by root."
else
    # Use an existing group with this gid (e.g. 100 "users" on Unraid);
    # otherwise move the image's "chrysopoeia" group to it.
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
# not otherwise open. Groups missing from the image are created by gid.
for dev in $DEVICE_NODES; do
    [ -c "$dev" ] || continue
    dev_gid=$(stat -c %g "$dev")
    dev_mode=$(stat -c %a "$dev")
    group_bits=$(((dev_mode / 10) % 10))
    other_bits=$((dev_mode % 10))
    dev_group=$(getent group "$dev_gid" | cut -d: -f1)
    if [ "$PUID" -ne 0 ] && [ $((other_bits & 6)) -ne 6 ]; then
        if [ $((group_bits & 6)) -ne 6 ]; then
            warn "$dev is not readable and writable by its group, so Chrysopoeia may not be able to use it. On the host, run: chmod g+rw $dev"
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
    note_device "$dev" "${dev_group:-gid $dev_gid}"
done
finish_device_notes
check_nvidia_runtime

# --- Ownership of writable folders ---------------------------------------------------

if [ "$PUID" -ne 0 ]; then
    mkdir -p "$DATA_DIR"
    # Recursive for the (small) config folder, but only when something in it
    # belongs to someone else.
    if [ -n "$(find "$DATA_DIR" -xdev \( ! -user "$PUID" -o ! -group "$PGID" \) -print 2>/dev/null | head -n 1)" ]; then
        log "Giving $DATA_DIR to uid $PUID, gid $PGID"
        chown -R "$PUID:$PGID" "$DATA_DIR" || warn "Could not change the owner of $DATA_DIR."
    fi
    # The scratch folder may hold large files: fix only the folder itself.
    if [ -n "${TEMP_DIR:-}" ] && [ -d "$TEMP_DIR" ]; then
        if [ "$(stat -c %u:%g "$TEMP_DIR")" != "$PUID:$PGID" ]; then
            chown "$PUID:$PGID" "$TEMP_DIR" || warn "Could not change the owner of $TEMP_DIR."
        fi
    fi
fi

# --- Drop privileges and start ----------------------------------------------------

if [ "$PUID" -eq 0 ]; then
    banner "root (PUID=0), umask $UMASK"
    exec "$@"
fi

as_app() {
    setpriv --reuid="$APP_USER" --regid="$PGID" --init-groups --no-new-privs "$@"
}

if ! as_app test -w "$DATA_DIR"; then
    die "Chrysopoeia (uid $PUID) cannot write to $DATA_DIR. Make the folder writable for PUID/PGID $PUID/$PGID, or set PUID/PGID to the folder's owner."
fi
if [ "$media_mounted" = 1 ] && ! as_app test -w /media; then
    warn "Chrysopoeia (uid $PUID) cannot write to /media, so it cannot replace files there. Set PUID/PGID to the owner of your media (Unraid: 99/100) or use an output folder."
fi

banner "uid $PUID ($APP_USER), gid $PGID ($(getent group "$PGID" | cut -d: -f1)), umask $UMASK"
exec setpriv --reuid="$APP_USER" --regid="$PGID" --init-groups --no-new-privs "$@"
