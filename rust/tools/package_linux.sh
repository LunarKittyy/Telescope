#!/usr/bin/env bash
# Builds the Linux package folder, the shape Telescope-linux.tar.gz has: what the Python app's updater and start.sh expect.
# Usage: package_linux.sh --ffmpeg DIR [--out DIR] [--channel stable|nightly|dev] [--build N] [--commit SHA]
# DIR comes from build_ffmpeg.sh. What it's built on sets the oldest Linux it runs on, so a release builds on Ubuntu 22.04 (rust-package.yml).
# release.yml adds Telescope.apk afterwards and packs this folder into the tarball.
set -euo pipefail

rust=$(cd "$(dirname "$0")/.." && pwd)
out=$rust/target/dist-linux
channel=dev
build=0
commit=
ffmpeg=
while [ $# -gt 0 ]; do
    case $1 in
        --ffmpeg) ffmpeg=$(realpath "$2"); shift 2 ;;
        --out) out=$(realpath -m "$2"); shift 2 ;;
        --channel) channel=$2; shift 2 ;;
        --build) build=$2; shift 2 ;;
        --commit) commit=$2; shift 2 ;;
        *) echo "Unknown option $1" >&2; exit 2 ;;
    esac
done
[ -n "$ffmpeg" ] || { echo "Pass --ffmpeg, a build from build_ffmpeg.sh" >&2; exit 2; }
case $channel in stable|nightly|dev) ;; *) echo "The channel is stable, nightly or dev" >&2; exit 2 ;; esac

cd "$rust"
FFMPEG_DIR=$ffmpeg TELESCOPE_BUILD=$build TELESCOPE_CHANNEL=$channel TELESCOPE_COMMIT=$commit cargo build --release --locked -p telescope-app
target=${CARGO_TARGET_DIR:-target}

rm -rf "$out"
mkdir -p "$out/lib/fallback" "$out/telescope"
cp "$target/release/telescope-rs" "$out/"
cp tools/start.sh "$out/start.sh"
cp tools/main_stub.py "$out/main.py"
chmod +x "$out/start.sh" "$out/telescope-rs"
# The window's .slint files and icons are read at run time (telescope_ui_kit::library_path)
cp -r crates/ui-kit/ui "$out/ui-kit"
# The Python updater wants a telescope folder, and check_release.py reads the stamp here
version=$(tr -d '[:space:]' < ../VERSION)
printf "VERSION = '%s'\nBUILD = %s\nCHANNEL = '%s'\nCOMMIT = '%s'\n" "$version" "$build" "$channel" "$commit" > "$out/telescope/_build.py"
# Real files under each library's soname: build artifacts don't keep symlinks
for lib in libavcodec libavutil libswresample libswscale; do
    soname=$(basename "$(find "$ffmpeg/lib" -name "$lib.so.*" -regex '.*\.so\.[0-9]+')")
    cp -L "$ffmpeg/lib/$soname" "$out/lib/$soname"
done
# Only reached on a system without its own (start.sh)
for lib in libva.so.2 libva-drm.so.2 libdrm.so.2; do
    cp -L "$(ldconfig -p | awk -v l="$lib" '$1 == l && /x86-64/ { print $NF; exit }')" "$out/lib/fallback/$lib"
done
python3 tools/fetch_rnnoise.py linux "$out"
python3 tools/write_notices.py linux "$ffmpeg" "$out/THIRD_PARTY_NOTICES.txt"
echo "Telescope $version $channel $build is in $out"
