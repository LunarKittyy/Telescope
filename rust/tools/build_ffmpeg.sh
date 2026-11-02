#!/usr/bin/env bash
# Builds the small LGPL FFmpeg the release packages ship: only the decoders and hardware paths the app calls, no programs, no encoders.
# Usage: build_ffmpeg.sh linux|windows OUT [DOWNLOADS]
# Runs on Ubuntu 22.04 (CI's rust-package.yml, or `docker run ubuntu:22.04` here), which sets how old a Linux the package still runs on.
# Linux needs: build-essential nasm patchelf pkg-config libva-dev libdrm-dev zlib1g-dev
# Windows also needs: gcc-mingw-w64-x86-64-win32 libz-mingw-w64-dev (the win32 thread model, so no libwinpthread DLL comes along)
# The GPL build with libx264 stays for tests only: nothing here may turn on --enable-gpl.
set -euo pipefail

FFMPEG_VERSION=8.1.3
FFMPEG_SHA256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
NVCODEC_VERSION=12.2.72.0
NVCODEC_SHA256=dbeaec433d93b850714760282f1d0992b1254fc3b5a6cb7d76fc1340a1e47563
# Every decoder the app opens (media/src/ffmpeg.rs and opus.rs), and HEVC, which costs under a megabyte, for H.265 some day
DECODERS=h264,hevc,mjpeg,vp8,opus,png,webp,bmp
JOBS=${FFMPEG_JOBS:-3}

target=${1:?linux or windows}
out=$(realpath -m "${2:?output folder}")
downloads=$(realpath -m "${3:-$out.downloads}")
work=$out.work

fetch() {
    local url=$1 file=$downloads/$2 sum=$3
    [ -f "$file" ] || curl -fsSL --connect-timeout 30 --retry 5 --retry-all-errors --retry-delay 10 -o "$file" "$url"
    echo "$sum  $file" | sha256sum -c --quiet -
}

mkdir -p "$downloads"
rm -rf "$work" "$out"
mkdir -p "$work" "$out"
fetch "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" "ffmpeg-$FFMPEG_VERSION.tar.xz" "$FFMPEG_SHA256"
fetch "https://github.com/FFmpeg/nv-codec-headers/archive/refs/tags/n$NVCODEC_VERSION.tar.gz" "nv-codec-headers-$NVCODEC_VERSION.tar.gz" "$NVCODEC_SHA256"
tar -xf "$downloads/ffmpeg-$FFMPEG_VERSION.tar.xz" -C "$work"
tar -xf "$downloads/nv-codec-headers-$NVCODEC_VERSION.tar.gz" -C "$work"

# NVDEC's headers only: the driver's library is opened at run time, so nothing links against it
make -C "$work/nv-codec-headers-n$NVCODEC_VERSION" PREFIX="$work/nv" install >/dev/null
export PKG_CONFIG_PATH="$work/nv/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

flags=(
    --prefix="$out" --enable-shared --disable-static --enable-pic
    --disable-autodetect --disable-everything --disable-programs --disable-doc --disable-debug --disable-network
    --disable-avdevice --disable-avformat --disable-avfilter
    --enable-decoder="$DECODERS" --enable-parser=h264,hevc
    --enable-zlib --enable-ffnvcodec --enable-nvdec --enable-hwaccel=h264_nvdec,hevc_nvdec
)
case $target in
    linux)
        flags+=(--enable-vaapi --enable-hwaccel=h264_vaapi,hevc_vaapi)
        ;;
    windows)
        flags+=(--target-os=mingw32 --arch=x86_64 --enable-cross-compile --cross-prefix=x86_64-w64-mingw32- --pkg-config=pkg-config
                --enable-w32threads --enable-d3d11va --enable-dxva2 --disable-stripping
                --enable-hwaccel=h264_d3d11va,h264_d3d11va2,h264_dxva2,hevc_d3d11va,hevc_d3d11va2,hevc_dxva2
                --extra-ldflags=-static-libgcc --extra-ldflags=-Wl,--no-insert-timestamp --extra-ldflags="-L$work/zlib")
        # A folder with only the static zlib, so no zlib1.dll has to ship
        mkdir -p "$work/zlib"
        cp /usr/x86_64-w64-mingw32/lib/libz.a "$work/zlib/"
        ;;
    *) echo "linux or windows, not $target" >&2; exit 2 ;;
esac

cd "$work/ffmpeg-$FFMPEG_VERSION"
./configure "${flags[@]}"
if grep -q 'CONFIG_GPL 1\|CONFIG_NONFREE 1' config.h; then
    echo "This build must stay LGPL" >&2
    exit 1
fi
make -j"$JOBS"
make install
if [ "$target" = windows ]; then
    x86_64-w64-mingw32-strip "$out"/bin/*.dll
else
    # The Opus decoder pulls in swresample, which only finds its way through libavcodec's own search path
    find "$out/lib" -maxdepth 1 -type f -name 'lib*.so.*' -exec patchelf --set-rpath '$ORIGIN' {} +
fi
cp COPYING.LGPLv2.1 "$out/"
{
    echo "FFmpeg $FFMPEG_VERSION, https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz (sha256 $FFMPEG_SHA256)"
    echo "Configured with: ${flags[*]}"
} > "$out/BUILD.txt"
rm -rf "$work"
echo "FFmpeg $FFMPEG_VERSION for $target is in $out"
