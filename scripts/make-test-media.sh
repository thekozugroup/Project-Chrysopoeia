#!/usr/bin/env sh
# Generate a small library of synthetic media for tests and demos.
# Usage: scripts/make-test-media.sh <output-dir> [duration-seconds]
# Needs ffmpeg with libx264, libx265 and libmp3lame (any distro build has them).
set -eu

OUT="${1:?usage: make-test-media.sh <output-dir> [duration]}"
DUR="${2:-6}"
FF="${FFMPEG:-ffmpeg}"
mkdir -p "$OUT/Movies/Big Test (2020)" "$OUT/TV/Show/Season 01" "$OUT/Music" "$OUT/Broken"

q() { "$FF" -hide_banner -loglevel error -y "$@"; }

# Moving test pattern with a burnt-in timer so frame alignment and artifacts are visible.
VIDEO_SRC="testsrc2=size=1280x720:rate=24:duration=$DUR"

# 1. H.264 + stereo AAC + text subtitle in MP4 (typical web download)
q -f lavfi -i "$VIDEO_SRC" -f lavfi -i "sine=frequency=440:duration=$DUR" \
  -f lavfi -i "sine=frequency=660:duration=$DUR" \
  -map 0:v -map 1:a -map 2:a -c:v libx264 -preset veryfast -crf 18 -pix_fmt yuv420p \
  -c:a aac -b:a 160k -ac 2 -metadata:s:a:0 language=eng -metadata:s:a:1 language=jpn \
  "$OUT/Movies/Big Test (2020)/Big Test (2020).mp4"

# 2. H.264 1080p with 5.1(side) AC-3 and an SRT subtitle in MKV (typical Blu-ray rip)
printf '1\n00:00:00,500 --> 00:00:02,500\nHello from Szalinski\n\n2\n00:00:03,000 --> 00:00:05,000\nSecond line\n' > "$OUT/.subs.srt"
q -f lavfi -i "testsrc2=size=1920x1080:rate=24:duration=$DUR" \
  -f lavfi -i "sine=frequency=220:duration=$DUR" -i "$OUT/.subs.srt" \
  -filter_complex "[1:a]pan=5.1(side)|FL=c0|FR=c0|FC=c0|LFE=c0|SL=c0|SR=c0[a]" \
  -map 0:v -map "[a]" -map 2:s -c:v libx264 -preset veryfast -crf 20 -pix_fmt yuv420p \
  -c:a ac3 -b:a 384k -c:s srt -metadata:s:s:0 language=eng \
  "$OUT/TV/Show/Season 01/Show - S01E01.mkv"
rm -f "$OUT/.subs.srt"

# 3. 10-bit HEVC (already efficient: should be skipped when targeting HEVC)
q -f lavfi -i "$VIDEO_SRC" -f lavfi -i "sine=frequency=330:duration=$DUR" \
  -c:v libx265 -preset ultrafast -crf 24 -pix_fmt yuv420p10le -x265-params log-level=error \
  -c:a aac -b:a 128k "$OUT/TV/Show/Season 01/Show - S01E02.mkv"

# 4. Interlaced MPEG-2 in MPEG-TS (broadcast recording)
q -f lavfi -i "testsrc2=size=720x576:rate=25:duration=$DUR" -f lavfi -i "sine=frequency=500:duration=$DUR" \
  -vf "tinterlace=mode=interleave_top,setfield=tff" -c:v mpeg2video -flags +ilme+ildct -top 1 -b:v 4M \
  -c:a mp2 -b:a 192k -f mpegts "$OUT/TV/Show/Season 01/Show - S01E03.ts"

# 5. Legacy AVI (Motion JPEG 4:4:4 + MP3, like old cameras), odd dimensions.
#    testsrc2 and 4:2:0 formats round sizes to even numbers, so crop a 4:4:4
#    picture: the file really is 639x359 and exercises the odd-size path.
q -f lavfi -i "testsrc2=size=640x360:rate=25:duration=$DUR" -f lavfi -i "sine=frequency=880:duration=$DUR" \
  -vf "format=yuv444p,crop=639:359:0:0" -c:v mjpeg -q:v 4 -pix_fmt yuvj444p \
  -c:a libmp3lame -b:a 128k "$OUT/Movies/Old Home Video.avi"

# 6. Audio only (should be skipped)
q -f lavfi -i "sine=frequency=440:duration=$DUR" -c:a flac "$OUT/Music/Tone.flac"

# 7. Truncated file (should fail probing or decoding, never crash anything)
q -f lavfi -i "$VIDEO_SRC" -c:v libx264 -preset veryfast -pix_fmt yuv420p "$OUT/Broken/.whole.mkv"
head -c 60000 "$OUT/Broken/.whole.mkv" > "$OUT/Broken/Truncated.mkv"
rm -f "$OUT/Broken/.whole.mkv"

# 8. Not media at all, with a media extension
printf 'this is not a video' > "$OUT/Broken/Fake.mp4"

echo "Test media written to $OUT"
