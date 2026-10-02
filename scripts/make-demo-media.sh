#!/usr/bin/env sh
# Generate a small, believable library for screenshots and demos: films and a
# TV show with real-looking names and sizes, grainy enough that converting
# them saves a realistic share of space (not 95%, as clean test patterns do).
#
#   scripts/make-demo-media.sh <output-dir> [scale]
#
# scale (default 1) multiplies the clip lengths; 1 gives about 10 files,
# 300 MB and ten minutes of conversion on four CPU cores.
# Needs ffmpeg with libx264 and libx265. The titles are made up.
set -eu

OUT="${1:?usage: make-demo-media.sh <output-dir> [scale]}"
SCALE="${2:-1}"
FF="${FFMPEG:-ffmpeg}"

q() { "$FF" -hide_banner -loglevel error -y "$@"; }

# secs <base>: clip length in whole seconds after scaling.
secs() { awk -v b="$1" -v s="$SCALE" 'BEGIN { printf "%d", b * s }'; }

# film <path> <size> <seconds> <crf> <audio>: grainy H.264 video, one audio track.
#   audio: stereo | surround
film() {
    path=$1 size=$2 dur=$(secs "$3") crf=$4 audio=$5
    mkdir -p "$(dirname "$path")"
    case $audio in
        surround) afilter="pan=5.1(side)|FL=c0|FR=c0|FC=c0|LFE=c0|SL=c0|SR=c0"; acodec="-c:a ac3 -b:a 448k" ;;
        *) afilter="anull"; acodec="-c:a aac -b:a 192k -ac 2" ;;
    esac
    # shellcheck disable=SC2086  # $acodec is a list of options
    q -f lavfi -i "testsrc2=size=$size:rate=24:duration=$dur,noise=alls=5:allf=t,format=yuv420p" \
        -f lavfi -i "sine=frequency=330:duration=$dur" \
        -filter_complex "[1:a]${afilter}[a]" -map 0:v -map "[a]" \
        -c:v libx264 -preset veryfast -crf "$crf" $acodec \
        -metadata:s:a:0 language=eng "$path"
}

mkdir -p "$OUT"

# Films
film "$OUT/Movies/Harbor Lights (2018)/Harbor Lights (2018).mkv" 1920x1080 45 21 surround
film "$OUT/Movies/The Long Way Home (2021)/The Long Way Home (2021).mp4" 1920x1080 40 22 stereo
film "$OUT/Movies/Paper Moons (2015)/Paper Moons (2015).mkv" 1280x720 36 22 stereo
film "$OUT/Movies/Winter Quarter (2012)/Winter Quarter (2012).mp4" 1280x720 30 23 stereo

# A show: two seasons of 720p episodes, each a little different in length
film "$OUT/TV/Lantern Street/Season 01/Lantern Street - S01E01.mkv" 1280x720 22 23 stereo
film "$OUT/TV/Lantern Street/Season 01/Lantern Street - S01E02.mkv" 1280x720 25 23 stereo
film "$OUT/TV/Lantern Street/Season 01/Lantern Street - S01E03.mkv" 1280x720 21 23 stereo
film "$OUT/TV/Lantern Street/Season 01/Lantern Street - S01E04.mkv" 1280x720 24 23 stereo
film "$OUT/TV/Lantern Street/Season 02/Lantern Street - S02E01.mkv" 1280x720 23 23 stereo
film "$OUT/TV/Lantern Street/Season 02/Lantern Street - S02E02.mkv" 1280x720 26 23 stereo

# Already efficient: 10-bit HEVC, which Balanced and Save space leave alone
dur=$(secs 24)
mkdir -p "$OUT/Movies/Kite Season (2019)"
q -f lavfi -i "testsrc2=size=1920x1080:rate=24:duration=$dur,noise=alls=4:allf=t,format=yuv420p10le" \
    -f lavfi -i "sine=frequency=262:duration=$dur" \
    -c:v libx265 -preset ultrafast -crf 26 -x265-params log-level=error \
    -c:a aac -b:a 160k "$OUT/Movies/Kite Season (2019)/Kite Season (2019).mkv"

echo "Demo media written to $OUT"
