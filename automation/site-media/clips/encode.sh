#!/bin/bash
# encode.sh NAME CRF WEBPQ POSTERFRAME: work/NAME/c%05d.png (30 fps) to work/NAME/out/ as the site and README use it:
# mp4, animated webp, poster webp and animated avif. Needs ffmpeg with libx264, libwebp and libaom-av1.
set -e
work=${SITE_MEDIA_WORK:-$(cd "$(dirname "$0")/.." && pwd)/work}
n=$1; crf=$2; q=$3; pf=$4
out=$work/$n/out; mkdir -p "$out"
ffmpeg -v error -y -framerate 30 -i "$work/$n/c%05d.png" -vf "scale=1280:720:flags=lanczos" -c:v libx264 -preset slow -crf "$crf" -pix_fmt yuv420p -movflags +faststart -an "$out/$n.mp4"
ffmpeg -v error -y -framerate 30 -i "$work/$n/c%05d.png" -vf "fps=20,scale=640:360:flags=lanczos" -c:v libwebp -q:v "$q" -compression_level 6 -loop 0 -an "$out/$n.webp"
ffmpeg -v error -y -i "$work/$n/c$(printf %05d "$pf").png" -vf "scale=1280:720:flags=lanczos" -c:v libwebp -q:v 82 "$out/$n-poster.webp"
ffmpeg -v error -y -framerate 30 -i "$work/$n/c%05d.png" -vf "scale=960:540:flags=lanczos,format=yuv420p" -c:v libaom-av1 -crf 24 -b:v 0 -cpu-used 3 -row-mt 1 "$out/$n.avif"
ls -la "$out" | awk '{print $5, $9}'
